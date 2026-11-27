pub mod channel;

use serde::{Deserialize, Serialize};
use url::Url;

use crate::exchange::{Frame, RequestId};
use renderer::{
    BrowserEvent, DialFailure, DialOutcome, DialRequest, EntryId, FrameId, NavigationSource,
    OpenerPolicy, PostedMessage, RemoteValue, SerializedOrigin, StorageChange, StorageError,
    StorageKey, StorageKind, StreamId, TabError, TargetName, WindowId,
};

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RendererAssignmentId(u64);

impl RendererAssignmentId {
    #[must_use]
    pub fn new(raw: u64) -> Self {
        Self(raw)
    }

    #[must_use]
    pub(crate) const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Command {
    ExecuteScript {
        frame: FrameId,
        source: renderer::ScriptSource,
        timeout_ms: Option<u64>,
    },
    Screenshot {
        frame: FrameId,
        request: renderer::ScreenshotRequest,
    },
    WaitForJs {
        id: u64,
        frame: FrameId,
        source: renderer::ScriptSource,
    },
    CancelWait {
        id: u64,
    },
    WindowMessage(renderer::PostedMessage),
    HistoryTraverse {
        url: String,
        history: renderer::HistorySnapshot,
    },
    HistoryTraverseFrame {
        frame: FrameId,
        entry: EntryId,
        url: String,
        history: renderer::HistorySnapshot,
        same_document: bool,
        reload: bool,
    },
    HistoryEvicted {
        entries: Vec<EntryId>,
    },
    SetHistory {
        frame: FrameId,
        history: renderer::HistorySnapshot,
    },
    AddInitScript {
        id: u64,
        source: String,
        // https://chromedevtools.github.io/devtools-protocol/tot/Page/#method-addScriptToEvaluateOnNewDocument
        run_immediately: bool,
    },
    // https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#method-addBinding
    AddBinding {
        name: String,
        run_immediately: bool,
    },
    RemoveInitScript {
        id: u64,
    },
    SetViewport {
        frame: FrameId,
        width: u32,
        height: u32,
    },
    // https://chromedevtools.github.io/devtools-protocol/tot/Emulation/#method-setUserAgentOverride
    SetIdentity {
        identity: Box<net::ValidIdentity>,
    },
    ContentSize {
        frame: FrameId,
    },
    DocumentState {
        frame: FrameId,
    },
    Dom {
        frame: FrameId,
        request: renderer::DomRequest,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Reply {
    Unit(Result<(), TabError>),
    Value(Result<RemoteValue, TabError>),
    Screenshot { result: Result<u32, TabError> },
    ContentSize(Result<(f32, f32), TabError>),
    DocumentState(Result<renderer::DocumentLoadState, TabError>),
    Dom(Result<Box<renderer::DomReply>, TabError>),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum RendererCall {
    Command {
        assignment: RendererAssignmentId,
        command: Command,
    },
    Response {
        response: Box<ResponseStart>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum HostNotice {
    // https://wicg.github.io/ua-client-hints/#http-ua-hints
    Hello {
        identity: Box<net::ValidIdentity>,
    },
    Shutdown,
    Assign {
        assignment: RendererAssignmentId,
    },
    Release {
        assignment: RendererAssignmentId,
    },
    // https://html.spec.whatwg.org/multipage/webstorage.html#concept-storage-broadcast
    StorageEvent {
        origin: StorageKey,
        kind: StorageKind,
        key: Option<String>,
        old_value: Option<String>,
        new_value: Option<String>,
        url: String,
        source: Option<(RendererAssignmentId, FrameId)>,
    },
    BroadcastMessage {
        origin: StorageKey,
        name: String,
        payload: String,
        source: Option<(RendererAssignmentId, u64)>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResponseStart {
    pub assignment: RendererAssignmentId,
    pub frame: FrameId,
    pub generation: u64,
    pub head: renderer::ResponseHead,
    pub tab: renderer::TabState,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BrowserCall {
    pub assignment: RendererAssignmentId,
    pub call: ServiceCall,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RendererReply {
    pub assignment: RendererAssignmentId,
    pub reply: Reply,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum RendererNotice {
    Ready,
    StartupFailed {
        message: String,
    },
    Event {
        assignment: RendererAssignmentId,
        generation: u64,
        frame: FrameId,
        event: BrowserEvent,
    },
    WaiterSettled {
        assignment: RendererAssignmentId,
        id: u64,
    },
}

pub(crate) type ToRenderer = Frame<RendererCall, ServiceReply, HostNotice>;

pub(crate) type FromRenderer = Frame<BrowserCall, RendererReply, RendererNotice>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ServiceCall {
    Network(NetworkCall),
    Storage(StorageCall),
    BrowsingContext(BrowsingContextCall),
    Messaging(MessagingCall),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum NetworkCall {
    Dial(Box<DialRequest>),
    CancelDial { id: RequestId },
    DialRead { id: StreamId },
    CancelStream { id: StreamId },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum StorageCall {
    Get {
        kind: StorageKind,
        origin: SerializedOrigin,
        key: String,
    },
    Keys {
        kind: StorageKind,
        origin: SerializedOrigin,
    },
    Set {
        kind: StorageKind,
        origin: SerializedOrigin,
        url: String,
        key: String,
        value: String,
        source: FrameId,
    },
    Remove {
        kind: StorageKind,
        origin: SerializedOrigin,
        url: String,
        key: String,
        source: FrameId,
    },
    Clear {
        kind: StorageKind,
        origin: SerializedOrigin,
        url: String,
        source: FrameId,
    },
    RemoteSessionGet {
        window: WindowId,
        origin: SerializedOrigin,
        key: String,
    },
    CookieGet {
        url: String,
        partition_key: Option<String>,
    },
    CookieSet {
        value: String,
        url: String,
        partition_key: Option<String>,
    },
    CookieStoreGet {
        url: String,
        partition_key: Option<String>,
    },
    CookieStoreSet {
        write: net::ScriptCookieWrite,
        url: String,
        partition_key: Option<String>,
    },
    BlobCreate {
        scope: u64,
        origin: SerializedOrigin,
        contents: String,
        content_type: String,
    },
    BlobResolve {
        scope: u64,
        origin: SerializedOrigin,
        url: String,
    },
    BlobRevoke {
        scope: u64,
        origin: SerializedOrigin,
        url: String,
    },
    BlobUnload {
        scope: u64,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum BrowsingContextCall {
    WindowOpen {
        url: Option<Url>,
        // https://html.spec.whatwg.org/multipage/document-sequences.html#the-rules-for-choosing-a-navigable
        target: TargetName,
        opener: OpenerPolicy,
        // https://w3c.github.io/webappsec-fetch-metadata/#abstract-opdef-set-site
        // https://w3c.github.io/webappsec-referrer-policy/#determine-requests-referrer
        // https://html.spec.whatwg.org/multipage/browsers.html#sandboxing
        source: NavigationSource,
    },
    // https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-name
    WindowName {
        change: renderer::NameChange,
    },
    NavigateNamed {
        name: renderer::ContextName,
        url: Option<Url>,
        opener: OpenerPolicy,
        source: NavigationSource,
    },
    NavigateTopLevel {
        url: Url,
        source: NavigationSource,
    },
    // https://html.spec.whatwg.org/multipage/browsers.html#determining-the-creation-sandboxing-flags
    FrameCreated {
        frame: FrameId,
        parent: FrameId,
        kind: renderer::FrameKind,
        sandbox: renderer::SandboxFlags,
    },
    FrameDestroyed {
        frame: FrameId,
    },
    WindowClose {
        window: WindowId,
    },
    Opener,
    WindowMessage {
        window: WindowId,
        message: PostedMessage,
    },
    // https://html.spec.whatwg.org/multipage/timers-and-user-prompts.html#simple-dialogs
    UserPrompt {
        frame: FrameId,
        kind: renderer::PromptKind,
        message: String,
        default_value: Option<String>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum MessagingCall {
    BroadcastPost {
        origin: SerializedOrigin,
        name: String,
        payload: String,
        channel: u64,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ServiceReply {
    Dial(Box<Result<DialOutcome, DialFailure>>),
    StreamChunk(Result<Option<Vec<u8>>, DialFailure>),
    Cookie(String),
    CookieStore(Vec<renderer::ScriptCookie>),
    StorageValue(Option<String>),
    StorageKeys(Vec<String>),
    StorageChanged(Result<Option<StorageChange>, StorageError>),
    BlobUrl(Option<String>),
    Blob(Option<renderer::BlobRecord>),
    Window(Option<WindowId>),
    Prompt(renderer::PromptOutcome),
    Unit,
}

impl ServiceReply {
    #[must_use]
    pub fn benign(call: &ServiceCall) -> Self {
        match call {
            ServiceCall::Network(NetworkCall::Dial(_)) => {
                ServiceReply::Dial(Box::new(Err(DialFailure::Connect)))
            }
            ServiceCall::Network(NetworkCall::DialRead { .. }) => {
                ServiceReply::StreamChunk(Err(DialFailure::Connect))
            }
            ServiceCall::Storage(StorageCall::CookieGet { .. }) => {
                ServiceReply::Cookie(String::new())
            }
            ServiceCall::Storage(StorageCall::CookieStoreGet { .. }) => {
                ServiceReply::CookieStore(Vec::new())
            }
            ServiceCall::Network(
                NetworkCall::CancelDial { .. } | NetworkCall::CancelStream { .. },
            )
            | ServiceCall::Storage(
                StorageCall::CookieSet { .. }
                | StorageCall::CookieStoreSet { .. }
                | StorageCall::BlobRevoke { .. }
                | StorageCall::BlobUnload { .. },
            )
            | ServiceCall::Messaging(MessagingCall::BroadcastPost { .. })
            | ServiceCall::BrowsingContext(
                BrowsingContextCall::WindowClose { .. }
                | BrowsingContextCall::WindowMessage { .. }
                | BrowsingContextCall::WindowName { .. }
                | BrowsingContextCall::NavigateTopLevel { .. }
                | BrowsingContextCall::FrameCreated { .. }
                | BrowsingContextCall::FrameDestroyed { .. },
            ) => ServiceReply::Unit,
            ServiceCall::Storage(
                StorageCall::Get { .. } | StorageCall::RemoteSessionGet { .. },
            ) => ServiceReply::StorageValue(None),
            ServiceCall::Storage(StorageCall::Keys { .. }) => ServiceReply::StorageKeys(Vec::new()),
            ServiceCall::Storage(
                StorageCall::Set { .. } | StorageCall::Remove { .. } | StorageCall::Clear { .. },
            ) => ServiceReply::StorageChanged(Ok(None)),
            ServiceCall::Storage(StorageCall::BlobCreate { .. }) => ServiceReply::BlobUrl(None),
            ServiceCall::Storage(StorageCall::BlobResolve { .. }) => ServiceReply::Blob(None),
            ServiceCall::BrowsingContext(
                BrowsingContextCall::WindowOpen { .. }
                | BrowsingContextCall::NavigateNamed { .. }
                | BrowsingContextCall::Opener,
            ) => ServiceReply::Window(None),
            ServiceCall::BrowsingContext(BrowsingContextCall::UserPrompt { .. }) => {
                ServiceReply::Prompt(renderer::PromptOutcome::Dismissed)
            }
        }
    }
}
