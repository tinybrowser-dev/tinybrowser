//! The renderer wire: what the browser process and a `renderer` child send
//! each other, and the frame codec that carries it.
//!
//! Only value-only messages cross; DOM handles, `QuickJS` values, callbacks,
//! and `net` types never do.
//!
//! Both ends of this conversation live in this crate — the host end in
//! [`crate::link`], the child end in [`crate::child`] — so the message
//! vocabulary and the framing sit next to both. Payloads that describe a page
//! ([`FrameId`], [`Mount`](renderer::Mount), [`BrowserEvent`],
//! [`DialRequest`])
//! come from the engine crate and stay there; this module only carries them.

pub mod channel;

use serde::{Deserialize, Serialize};
use url::Url;

use crate::exchange::{Frame, RequestId};
use renderer::{
    BrowserEvent, DialFailure, DialOutcome, DialRequest, FrameId, NavigationSource, OpenerPolicy,
    PostedMessage, RemoteValue, SerializedOrigin, StorageChange, StorageError, StorageKey,
    StorageKind, StreamId, TabError, TargetName, WindowId,
};

/// Browser-minted identity of one top-level document hosted by a renderer.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RendererAssignmentId(u64);

impl RendererAssignmentId {
    /// Constructs an assignment id from a browser-process integer.
    #[must_use]
    pub fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// The raw browser-process integer, used for release high-water marks.
    #[must_use]
    pub(crate) const fn get(self) -> u64 {
        self.0
    }
}

/// Host command to a renderer.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Command {
    /// Evaluate `source` in one frame and return a value-only result.
    ExecuteScript {
        /// Frame to evaluate in.
        frame: FrameId,
        /// Script source.
        source: renderer::ScriptSource,
        /// Optional execution budget in milliseconds.
        timeout_ms: Option<u64>,
    },
    /// Render one frame to a PNG and stream it back in body frames.
    Screenshot {
        /// Frame to render.
        frame: FrameId,
        /// Viewport and crop window.
        request: renderer::ScreenshotRequest,
    },
    /// Registers a JS-truth predicate the renderer evaluates after each drain
    /// and reports through [`RendererNotice::WaiterSettled`].
    WaitForJs {
        /// Waiter identity, unique within this renderer.
        id: u64,
        /// Frame whose realm runs the predicate.
        frame: FrameId,
        /// Predicate source.
        source: renderer::ScriptSource,
    },
    /// Drops a waiter before it settles.
    CancelWait {
        /// Waiter identity from [`Command::WaitForJs`].
        id: u64,
    },
    /// Delivers one remote `message` event, encoded by the sender's realm.
    WindowMessage(renderer::PostedMessage),
    /// Activate an existing entry in the current document without replacing it.
    HistoryTraverse {
        url: String,
        history: renderer::HistorySnapshot,
    },
    /// Activates one child frame's entry. The renderer restores the local
    /// document or replays the navigation its replay store holds at `index`;
    /// the browser supplies the joint length and the frame's active position.
    HistoryTraverseFrame {
        /// Frame whose entry is activated.
        frame: FrameId,
        /// The entry's position in the frame's own list.
        index: usize,
        /// The entry's URL.
        url: String,
        /// The joint length and this frame's active position.
        history: renderer::HistorySnapshot,
        /// Whether the entry presents the document already active in the
        /// frame, which makes this a same-document traversal.
        same_document: bool,
        /// Whether the step reloads the active entry, as `history.go(0)` does.
        reload: bool,
    },
    /// Replaces one frame's mirrored joint history without moving its
    /// document, so a frame's `history.length` follows another frame's push.
    SetHistory {
        /// Frame whose mirror changes.
        frame: FrameId,
        /// The joint length, this frame's active position, and its state.
        history: renderer::HistorySnapshot,
    },
    /// Registers a page-scoped init script, identified by `id` so a replaced
    /// renderer can replay it.
    AddInitScript {
        /// Caller-assigned identifier.
        id: u64,
        /// Script source.
        source: String,
        /// Whether to evaluate the script in existing realms immediately
        /// (<https://chromedevtools.github.io/devtools-protocol/tot/Page/#method-addScriptToEvaluateOnNewDocument>).
        run_immediately: bool,
    },
    /// Removes a previously registered init script.
    RemoveInitScript {
        /// Identifier returned by the matching `AddInitScript`.
        id: u64,
    },
    /// Sets a frame's persistent viewport size.
    SetViewport {
        /// Frame to resize.
        frame: FrameId,
        /// Viewport width in device pixels.
        width: u32,
        /// Viewport height in device pixels.
        height: u32,
    },
    /// Reads a frame's scrollable content size.
    ContentSize {
        /// Frame to measure.
        frame: FrameId,
    },
    /// Answers one typed devtools DOM request from a frame's live tree.
    Dom {
        /// Frame whose tree answers.
        frame: FrameId,
        /// The typed request.
        request: renderer::DomRequest,
    },
}

/// Renderer reply to one [`Command`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Reply {
    /// Unit result.
    Unit(Result<(), TabError>),
    /// Value-only script result.
    Value(Result<RemoteValue, TabError>),
    /// A PNG follows in body frames for this request id; `len` is its exact
    /// byte length. The control payload never carries the bytes.
    Screenshot {
        /// PNG byte length, or the failure that replaced it.
        result: Result<u32, TabError>,
    },
    /// A frame's scrollable content size in CSS pixels.
    ContentSize(Result<(f32, f32), TabError>),
    /// A typed devtools DOM answer. Boxed: the document reply carries a whole
    /// node tree, and the other replies are a header of that size.
    Dom(Result<Box<renderer::DomReply>, TabError>),
}

/// One browser-originated renderer call.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum RendererCall {
    /// One operation on an assigned page engine.
    Command {
        /// Top-level document that owns the command.
        assignment: RendererAssignmentId,
        /// The command.
        command: Command,
    },
    /// Starts a streamed top-level response. Request chunks follow before the
    /// exchange's request-stream terminator.
    Response {
        /// Response metadata needed to mount the completed body. Boxed: the
        /// mount carries the whole document state, and it must not set the size
        /// of every command frame.
        response: Box<ResponseStart>,
    },
}

/// Browser-originated notifications that do not have replies.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum HostNotice {
    /// First frame from the browser process: protocol handshake, which also
    /// carries the identity every page and CDP answer in this process reports
    /// (<https://wicg.github.io/ua-client-hints/#http-ua-hints>).
    Hello {
        /// The browser's identity. Boxed because the handshake is one frame
        /// per process and the identity must not set the size of every other
        /// notice.
        identity: Box<net::ValidIdentity>,
    },
    /// Stops the renderer process.
    Shutdown,
    /// Creates an isolated page engine inside this renderer process.
    Assign {
        /// Browser-minted assignment identity.
        assignment: RendererAssignmentId,
    },
    /// Removes one page engine from this renderer process.
    Release {
        /// Browser-minted assignment identity.
        assignment: RendererAssignmentId,
    },
    /// One `localStorage` change another renderer made; every frame of
    /// `origin` except the source window fires a `storage` event
    /// (<https://html.spec.whatwg.org/multipage/webstorage.html#concept-storage-broadcast>).
    StorageEvent {
        /// Storage key whose area changed.
        origin: StorageKey,
        /// Which area changed.
        kind: StorageKind,
        /// The changed key, or `None` for `clear()`.
        key: Option<String>,
        /// The value before the change.
        old_value: Option<String>,
        /// The value after the change.
        new_value: Option<String>,
        /// URL of the document whose script made the change.
        url: String,
        /// The assignment and frame whose script made the change. Only the
        /// matching assignment excludes the frame; other renderers and
        /// assignments see a change with no local source.
        source: Option<(RendererAssignmentId, FrameId)>,
    },
    /// One same-origin `BroadcastChannel` message.
    BroadcastMessage {
        /// Storage key whose channels receive it.
        origin: StorageKey,
        /// Channel name.
        name: String,
        /// `__tbEncode` payload from the posting realm.
        payload: String,
        /// The posting assignment and channel; the matching assignment skips
        /// that channel.
        source: Option<(RendererAssignmentId, u64)>,
    },
}

/// Metadata sent before the raw bytes of a top-level response: the document
/// that receives it plus the response head it installs. The body arrives in
/// bounded raw frames after the postcard control message.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResponseStart {
    /// Top-level document receiving this response.
    pub assignment: RendererAssignmentId,
    /// Frame that will receive the document.
    pub frame: FrameId,
    /// The response head the document installs.
    pub head: renderer::ResponseHead,
    /// The browsing-context state the tab gives the document it installs.
    pub tab: renderer::TabState,
}

/// One renderer-originated browser-service call.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BrowserCall {
    /// Top-level document requesting the browser service.
    pub assignment: RendererAssignmentId,
    /// The call.
    pub call: ServiceCall,
}

/// Answer from a renderer, including the assignment used for authorization.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RendererReply {
    /// Top-level document that produced the reply.
    pub assignment: RendererAssignmentId,
    /// The answer.
    pub reply: Reply,
}

/// Renderer-originated notifications that do not have replies.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum RendererNotice {
    /// First message from a `renderer` child: protocol handshake.
    Ready,
    /// Unsolicited document event.
    Event {
        /// Top-level document that produced the reply.
        assignment: RendererAssignmentId,
        /// Frame that emitted the event.
        frame: FrameId,
        /// The event.
        event: BrowserEvent,
    },
    /// A registered JS-truth predicate evaluated to `true`.
    WaiterSettled {
        /// Top-level document that owns the waiter.
        assignment: RendererAssignmentId,
        /// Waiter identity from the matching command.
        id: u64,
    },
}

/// Decoded browser-to-renderer exchange messages.
pub(crate) type ToRenderer = Frame<RendererCall, ServiceReply, HostNotice>;

/// Decoded renderer-to-browser exchange messages.
pub(crate) type FromRenderer = Frame<BrowserCall, RendererReply, RendererNotice>;

/// What the renderer needs the browser process to do.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ServiceCall {
    /// Network and cookie operation.
    Network(NetworkCall),
    /// Web Storage operation.
    Storage(StorageCall),
    /// Browsing-context operation.
    BrowsingContext(BrowsingContextCall),
    /// Cross-context messaging operation.
    Messaging(MessagingCall),
}

/// Network and cookie operation requested by a renderer.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum NetworkCall {
    /// HTTP request submitted to the browser-owned network executor.
    Dial(Box<DialRequest>),
    /// Cancels a dial submitted by this assignment.
    CancelDial { id: RequestId },
    /// Reads the next decoded body chunk for a streamed dial.
    DialRead { id: StreamId },
    /// Cancels a streamed response body.
    CancelStream { id: StreamId },
    /// `document.cookie` getter.
    CookieGet {
        /// Document URL.
        url: String,
        /// The site of the top-level document, which selects the
        /// `Partitioned` cookies the caller may see.
        partition_key: Option<String>,
    },
    /// `document.cookie` setter.
    CookieSet {
        /// Cookie string.
        value: String,
        /// Document URL.
        url: String,
        /// The site of the top-level document, which a `Partitioned`
        /// cookie is stored under.
        partition_key: Option<String>,
    },
}

/// Web Storage operation requested by a renderer.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum StorageCall {
    /// `localStorage.getItem(key)`.
    Get {
        /// Storage area kind.
        kind: StorageKind,
        /// Serialized origin of the calling document.
        origin: SerializedOrigin,
        /// Item key.
        key: String,
    },
    /// Every key of one origin's local storage area.
    Keys {
        /// Storage area kind.
        kind: StorageKind,
        /// Serialized origin of the calling document.
        origin: SerializedOrigin,
    },
    /// `localStorage.setItem(key, value)`.
    Set {
        /// Storage area kind.
        kind: StorageKind,
        /// Serialized origin of the calling document.
        origin: SerializedOrigin,
        /// Calling document URL; the `storage` event reports it.
        url: String,
        /// Item key.
        key: String,
        /// Item value.
        value: String,
        /// Frame whose script made the change.
        source: FrameId,
    },
    /// `localStorage.removeItem(key)`.
    Remove {
        /// Storage area kind.
        kind: StorageKind,
        /// Serialized origin of the calling document.
        origin: SerializedOrigin,
        /// Calling document URL.
        url: String,
        /// Item key.
        key: String,
        /// Frame whose script made the change.
        source: FrameId,
    },
    /// `localStorage.clear()`.
    Clear {
        /// Storage area kind.
        kind: StorageKind,
        /// Serialized origin of the calling document.
        origin: SerializedOrigin,
        /// Calling document URL.
        url: String,
        /// Frame whose script made the change.
        source: FrameId,
    },
}

/// Browsing-context operation requested by a renderer.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum BrowsingContextCall {
    /// `window.open(url, target, features)`.
    WindowOpen {
        /// Absolute URL to load, or `None` when the call named none.
        url: Option<Url>,
        /// Which context the target names, so the browser records the name a
        /// newly created tab answers to
        /// (<https://html.spec.whatwg.org/multipage/document-sequences.html#the-rules-for-choosing-a-navigable>).
        target: TargetName,
        /// What the `noopener` and `noreferrer` features asked for, already
        /// parsed by the renderer that read the feature string.
        opener: OpenerPolicy,
        /// The calling document's origin, referrer source, policy, and
        /// sandbox set. The browser unions the set into its own copy of the
        /// tab's, so the caller can only make the new tab stricter
        /// (<https://w3c.github.io/webappsec-fetch-metadata/#abstract-opdef-set-site>,
        /// <https://w3c.github.io/webappsec-referrer-policy/#determine-requests-referrer>,
        /// <https://html.spec.whatwg.org/multipage/browsers.html#sandboxing>).
        source: NavigationSource,
    },
    /// The calling tab's target name changed, or the browsing context's name
    /// was cleared
    /// (<https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-name>).
    WindowName {
        /// An assignment from `window.name`, which writes the live name and
        /// the active document state's name, or the cross-origin reset, which
        /// clears only the live one.
        change: renderer::NameChange,
    },
    /// Selects the tab this tab's browsing context group holds under `name`
    /// and navigates it. The browser answers the selected tab, or `None` when
    /// no tab in the group has the name.
    NavigateNamed {
        /// The name to look up and then navigate.
        name: renderer::ContextName,
        /// Absolute URL to load, or `None` to leave the tab where it is.
        url: Option<Url>,
        /// What the call's `noopener` and `noreferrer` features asked for.
        opener: OpenerPolicy,
        /// The calling document's origin, referrer source, and policy. The
        /// browser derives the sandbox state from its own per-frame copy, so
        /// this does not carry it.
        source: NavigationSource,
    },
    /// A frame the renderer created, reported once before its document can
    /// run script. The browser stores the frame's active sandbox flag set,
    /// unions a child's reported `sandbox` attribute with its parent's set,
    /// and derives an auxiliary context's popup set itself. A later call from
    /// that frame is checked against this record, never against a fresh
    /// renderer claim
    /// (<https://html.spec.whatwg.org/multipage/browsers.html#determining-the-creation-sandboxing-flags>).
    FrameCreated {
        /// The new frame.
        frame: FrameId,
        /// The frame that created it: its embedder, or the opener of an
        /// auxiliary context.
        parent: FrameId,
        /// Which creation rule applies.
        kind: renderer::FrameKind,
        /// The `iframe sandbox` attribute flags for a child, or the popup set
        /// the renderer applied for an auxiliary context.
        sandbox: renderer::SandboxFlags,
    },
    /// A frame the renderer tore down. The browser drops its sandbox record,
    /// so a reused frame id starts from the tab's own set.
    FrameDestroyed {
        /// The frame that is gone, with every descendant the renderer also
        /// destroyed.
        frame: FrameId,
    },
    /// `window.close()` on a window this renderer opened.
    WindowClose {
        /// Browser-minted window identity.
        window: WindowId,
    },
    /// `window.opener` for this assignment's tab.
    Opener,
    /// `postMessage` to a window this renderer opened.
    WindowMessage {
        /// Target window identity.
        window: WindowId,
        /// The encoded message, with the sender's origin and resolved target.
        message: PostedMessage,
    },
    /// Reads one key from another tab's session area.
    RemoteSessionGet {
        /// Target window identity.
        window: WindowId,
        /// Serialized origin of the calling document.
        origin: SerializedOrigin,
        /// Encoded item key.
        key: String,
    },
}

/// Cross-context messaging operation requested by a renderer.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum MessagingCall {
    /// One `BroadcastChannel.postMessage` for every same-origin channel.
    BroadcastPost {
        /// Serialized origin of the posting document.
        origin: SerializedOrigin,
        /// Channel name.
        name: String,
        /// `__tbEncode` payload from the posting realm.
        payload: String,
        /// Posting realm's channel id, so the sender is skipped.
        channel: u64,
    },
}

/// Answer to a [`ServiceCall`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ServiceReply {
    /// Dial result, with a typed failure. Boxed: the outcome carries every
    /// response field, and the other replies are a header of that size.
    Dial(Box<Result<DialOutcome, DialFailure>>),
    /// Next streamed body chunk, or `None` at the end.
    StreamChunk(Result<Option<Vec<u8>>, DialFailure>),
    /// Cookie getter result.
    Cookie(String),
    /// Local storage getter result.
    StorageValue(Option<String>),
    /// Local storage key list, in iteration order.
    StorageKeys(Vec<String>),
    /// Local storage mutation result; `Ok(None)` means nothing changed.
    StorageChanged(Result<Option<StorageChange>, StorageError>),
    /// `window.open` result: the new window, or `None` when it was refused.
    Window(Option<WindowId>),
    /// No payload (`CookieSet`).
    Unit,
}

impl ServiceReply {
    /// The benign answer to `call`: the transport must always answer a
    /// service call so the renderer's synchronous path returns, even when the
    /// call was released, cancels itself, or is fire-and-forget and no one
    /// reads the reply. Each answer is that call type's empty value.
    #[must_use]
    pub fn benign(call: &ServiceCall) -> Self {
        match call {
            ServiceCall::Network(NetworkCall::Dial(_)) => {
                ServiceReply::Dial(Box::new(Err(DialFailure::Connect)))
            }
            ServiceCall::Network(NetworkCall::DialRead { .. }) => {
                ServiceReply::StreamChunk(Err(DialFailure::Connect))
            }
            ServiceCall::Network(NetworkCall::CookieGet { .. }) => {
                ServiceReply::Cookie(String::new())
            }
            ServiceCall::Network(
                NetworkCall::CancelDial { .. }
                | NetworkCall::CancelStream { .. }
                | NetworkCall::CookieSet { .. },
            )
            | ServiceCall::Messaging(MessagingCall::BroadcastPost { .. })
            | ServiceCall::BrowsingContext(
                BrowsingContextCall::WindowClose { .. }
                | BrowsingContextCall::WindowMessage { .. }
                | BrowsingContextCall::WindowName { .. }
                | BrowsingContextCall::FrameCreated { .. }
                | BrowsingContextCall::FrameDestroyed { .. },
            ) => ServiceReply::Unit,
            ServiceCall::Storage(StorageCall::Get { .. })
            | ServiceCall::BrowsingContext(BrowsingContextCall::RemoteSessionGet { .. }) => {
                ServiceReply::StorageValue(None)
            }
            ServiceCall::Storage(StorageCall::Keys { .. }) => ServiceReply::StorageKeys(Vec::new()),
            ServiceCall::Storage(
                StorageCall::Set { .. } | StorageCall::Remove { .. } | StorageCall::Clear { .. },
            ) => ServiceReply::StorageChanged(Ok(None)),
            ServiceCall::BrowsingContext(
                BrowsingContextCall::WindowOpen { .. }
                | BrowsingContextCall::NavigateNamed { .. }
                | BrowsingContextCall::Opener,
            ) => ServiceReply::Window(None),
        }
    }
}
