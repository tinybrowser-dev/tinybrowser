//! One document: HTML tasks we own, browser services for dials and cookies. The
//! renderer loop owns every wait; the browser process owns the tab and drives
//! navigation.

use enumerated::Enumerated;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant as WallClock;

use tokio::sync::Notify;

use blitz_traits::net::NetHandler;
use net::cors::CrossOrigin;
use tokio::time::Instant;
use url::Url;

use crate::Parsed;
use crate::ReadyState;
use crate::js::{
    DocumentStreamCommand, FrameNavigation, FrameRuntime, JsFetchId, JsRuntimeHandle, PageTimerId,
    PolicyContainer, World,
};
use crate::messaging::{DocumentId, SharedHandle};
use crate::protocol::TargetOrigin;
use crate::protocol::{
    BrowserEvent, BrowserServices, DialOutcome, DialRequest, FrameId, Mount, RendererEvent,
    ResponseHead, ScriptFailure, TabError, TimerId,
};

use self::dials::DialLedger;
use self::load::{LoadDelay, LoadDelayGuard};

pub(crate) mod dial;
mod dials;
pub(crate) mod dom;
mod drain;

mod intern;
mod load;

pub(crate) use crate::js::ScriptValue;

/// A frame document this document created for the engine to adopt.
///
/// A child navigable is owned by the `iframe` that asked for it; an auxiliary
/// context has no container and keeps the frame that opened it
/// (<https://html.spec.whatwg.org/multipage/document-sequences.html#creating-a-new-auxiliary-browsing-context>).
pub(crate) enum CreatedFrame {
    Child {
        frame: FrameId,
        container: crate::dom::tree::NodeId,
        document: Document,
    },
    Auxiliary {
        frame: FrameId,
        document: Document,
    },
}

/// Concurrent `fetch` jobs one document may keep in flight.
const MAX_PENDING_JS_FETCHES: usize = 256;
/// Retained events before a document reports overflow.
const MAX_PENDING_EVENTS: usize = 2048;

#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
pub(crate) struct DialId(u64);

impl DialId {
    fn take_next(&mut self) -> Self {
        let id = *self;
        self.0 += 1;
        id
    }
}

enum Task {
    Timer(TimerId),
    /// Boxed: a completed dial carries the request and response with it, and
    /// the variant otherwise dominates every other task's size.
    DialFinished(DialId, Box<CompletedDial>),
    DialFailed(DialId, DialContext),
    WindowMessage(WindowMessage),
    StorageEvent(crate::storage::PendingStorageEvent),
    /// One cross-tab `message` from another renderer. The sender's origin and
    /// intended target origin travel with it, so the task re-checks both
    /// against whichever document runs it.
    RemoteMessage(crate::protocol::PostedMessage),
    /// One `BroadcastChannel` message for this frame's channels.
    BroadcastMessage {
        origin: String,
        name: String,
        payload: String,
        source: Option<u64>,
    },
    PortMessage {
        endpoint: u64,
        payload: String,
        ports: Vec<u64>,
    },
    PortClosed {
        endpoint: u64,
    },
    /// One message a dedicated worker posted to this document.
    WorkerMessage {
        worker: crate::worker::WorkerId,
        payload: String,
        ports: Vec<u64>,
    },
    /// One error a dedicated worker reported to this document.
    WorkerError(crate::worker::WorkerError),
    /// A worker that never started, or whose script did not parse: a plain
    /// `error` event at its `Worker` object
    /// (<https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model>).
    WorkerLoadFailed(crate::worker::WorkerId),
    /// A subresource event for one element, queued so a listener attached
    /// later in the same script still sees it: `update the image data` and the
    /// stylesheet fetch steps queue a task rather than firing inside the
    /// mutation that started the request
    /// (<https://html.spec.whatwg.org/multipage/images.html#updating-the-image-data>,
    /// <https://html.spec.whatwg.org/multipage/semantics.html#the-link-element>).
    NodeLoad(crate::dom::tree::NodeId),
    NodeError(crate::dom::tree::NodeId),
    /// One queued `securitypolicyviolation` dispatch. The event fires in a
    /// task, never at report time, so a listener installed after the
    /// violation was reported still sees it
    /// (<https://w3c.github.io/webappsec-csp/#report-violation>).
    CspViolation(Vec<crate::csp::Violation>),
    /// One queued `select` dispatch for a text control whose selection
    /// changed. The spec queues an element task on the user interaction task
    /// source, so the event never fires inside the assignment
    /// (<https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#set-the-selection-range>).
    SelectionChanged(crate::dom::tree::NodeId),
}

/// Why a realm is being replaced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RealmReset {
    /// A navigation creates a new document instance: the `DocumentId` changes
    /// so tasks bound to the old one stop being runnable.
    Navigation,
    /// `document.open()` keeps the document and window: the identity stays.
    Reopen,
}

/// One posted window message, ready for the target realm's task queue. The
/// task is bound to the document that was the target window's associated
/// document at post time, and it is not runnable once that document is
/// replaced
/// (<https://html.spec.whatwg.org/multipage/webappapis.html#queue-a-global-task>,
/// <https://html.spec.whatwg.org/multipage/webappapis.html#concept-task-runnable>).
pub(crate) struct WindowMessage {
    pub(crate) source: FrameId,
    pub(crate) origin: String,
    /// The intended target origin the sender passed to `postMessage`, checked
    /// inside the task against the document that runs it.
    pub(crate) target_origin: TargetOrigin,
    pub(crate) payload: String,
    pub(crate) ports: Vec<u64>,
}

/// One same-document history update this document applied.
///
/// A browser-hosted engine learns it through `BrowserEvent::HistoryUpdated`.
/// A standalone engine also needs it to add the joint step the update creates;
/// the event queue is not drained there, so the document keeps this copy
/// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#url-and-history-update-steps>).
pub(crate) struct HistoryUpdate {
    pub(crate) url: String,
    pub(crate) state: String,
    pub(crate) handling: crate::protocol::HistoryHandling,
}

/// The identity of one outstanding dial, shared by its request and its
/// completion so the frame knows which operation the response belongs to.
#[derive(Clone, Copy)]
pub(crate) enum DialContext {
    JsFetch {
        id: JsFetchId,
    },
    ClassicScript {
        element: crate::dom::tree::NodeId,
    },
    /// A child frame's own navigation
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#navigate>);
    /// starting another navigation cancels this one through the ledger.
    FrameLoad {
        /// The embedding document, when this navigation is a nested browsing
        /// context. `None` for the top-level document.
        embedder: Option<FrameId>,
        /// The flags the container's `sandbox` attribute and the embedder's
        /// active set impose. They take effect when the response commits, not
        /// when the navigation starts
        /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#determine-navigation-params>).
        sandbox: crate::protocol::SandboxFlags,
    },
    /// A Blitz subresource (stylesheet, image, font) fetch. Its record waits
    /// in [`Document::blitz_requests`] under this id; delivery hands Blitz its
    /// bytes through the handler, which releases Blitz's own critical hold.
    BlitzResource {
        id: u64,
        /// `style-src-elem`, `img-src`, or `font-src` for this fetch.
        destination: Option<crate::csp::Destination>,
    },
    /// A `report-uri` violation report. The response is dropped; reports
    /// never gate load and are never CSP-checked themselves
    /// (<https://w3c.github.io/webappsec-csp/#report-violation>).
    CspReport,
}

/// One dial waiting to start: the wire request plus the dial-local context
/// the backend keeps for the response.
pub(crate) struct QueuedDial {
    pub(crate) context: DialContext,
    /// The request the dial starts as, built where the dial is queued: kind,
    /// URLs, method, body, referrer, CSP redirect gate, and CORS parameters
    /// are final.
    pub(crate) req: DialRequest,
    /// Ancestor document URLs nearest-first, snapshotted when a nested frame
    /// navigation starts. The child's `frame-ancestors` policy is checked
    /// against these at commit
    /// (<https://w3c.github.io/webappsec-csp/#frame-ancestors-navigation-response>).
    pub(crate) ancestors: Vec<Url>,
    /// Raw `integrity` attribute for a classic script dial, verified against
    /// the response bytes. `None` enforces nothing.
    pub(crate) integrity: Option<String>,
}

impl QueuedDial {
    /// A fresh GET dial: the shape of every dial but a form submission and a
    /// page `fetch`. The call sites that need more set the request's fields
    /// directly (`dial.req.…`), and each names the request policy its fetch
    /// specifies.
    pub(crate) fn new(
        context: DialContext,
        policy: crate::protocol::RequestPolicy,
        url: &Url,
        initiator: &url::Origin,
    ) -> Self {
        Self {
            req: crate::protocol::DialRequest::new(policy, url, initiator),
            context,
            ancestors: Vec::new(),
            integrity: None,
        }
    }
}

/// One finished dial with the response it produced.
pub(crate) struct CompletedDial {
    pub(crate) context: DialContext,
    pub(crate) outcome: DialOutcome,
    /// The wire request the dial started as: the request URL tells a redirect
    /// from the original hop, and the CSP gate reports refused redirects.
    pub(crate) req: DialRequest,
    /// Ancestor document URLs for a nested frame navigation, checked against
    /// the response's `frame-ancestors` at commit.
    pub(crate) ancestors: Vec<Url>,
    /// Raw `integrity` attribute for a classic script response, verified
    /// against the response bytes before execution.
    pub(crate) integrity: Option<String>,
}

/// Who feeds the active parser, and therefore who may end it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ParserOwner {
    /// A response body, or a document handed over whole: only the carrier
    /// decides when the input ends.
    Carrier,
    /// A script's `document.open()`: it writes text, and `document.close()`
    /// ends the parser.
    Script,
}

/// The markup an active parser accumulates and the delay token that keeps
/// `load` pending for that parse. Taking the value ends the parse.
struct ActiveParse {
    buffer: String,
    delay: LoadDelayGuard,
}

/// The state a local-scheme document needs to be installed again: a
/// session-history entry for an `about:blank` or `about:srcdoc` document
/// restores the document it held instead of re-fetching it
/// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#session-history-entry>).
#[derive(Clone)]
pub(crate) struct LocalDocumentState {
    /// The document URL, `about:blank` or `about:srcdoc`.
    pub(crate) url: Url,
    /// The fallback base URL the creator supplied.
    pub(crate) base_url: Option<Url>,
    /// The cookie URL an inherited document uses, captured at creation.
    pub(crate) cookie_url: Option<Url>,
    /// The document's own referrer, unredacted.
    pub(crate) referrer: Option<Url>,
    /// The URL `"client"` referrers resolve against.
    pub(crate) referrer_url: Url,
    pub(crate) origin: url::Origin,
    pub(crate) policy: PolicyContainer,
    pub(crate) sandbox: crate::protocol::SandboxFlags,
}

struct Timer {
    /// `None` when the delay overflows the clock: the timer never fires
    /// (<https://html.spec.whatwg.org/multipage/timers-and-user-prompts.html#dom-settimeout>).
    when: Option<Instant>,
    fired: bool,
    /// The script-visible id, when this timer belongs to a realm.
    js_id: Option<PageTimerId>,
}

/// Builds and queues the initial `about:blank` document for one connected
/// `iframe` so `contentDocument` is available in the inserting script
/// (<https://html.spec.whatwg.org/multipage/iframe-embed-object.html#the-iframe-element:html-element-insertion-steps>).
///
/// The nested realm is deferred: `QuickJS` cannot enter a new context while
/// the parent script still runs.
/// Maps a Blitz request kind onto the CSP destination it is checked as.
/// The dial one Blitz resource request becomes: its request policy, and the
/// CSP destination that gates it.
///
/// Blitz classifies what it fetches. `Other` is a request it did not classify;
/// the engine treats it as an image, because the only unclassified resource a
/// document hands Blitz today is an image a style sheet named. The guess lives
/// here, in one exhaustive match, instead of in a default arm somewhere else.
/// Blitz classifying what it fetches would delete it
/// (<https://github.com/tinybrowser-dev/tinybrowser/blob/main/docs/upstream.md>).
fn subresource_request(
    kind: blitz_traits::net::RequestKind,
    crossorigin: Option<CrossOrigin>,
) -> (
    crate::protocol::RequestPolicy,
    Option<crate::csp::Destination>,
) {
    use blitz_traits::net::RequestKind;
    match kind {
        RequestKind::Stylesheet => (
            crate::protocol::RequestPolicy::Stylesheet { crossorigin },
            Some(crate::csp::Destination::StyleSheet),
        ),
        RequestKind::Image => (
            crate::protocol::RequestPolicy::Image { crossorigin },
            Some(crate::csp::Destination::Img),
        ),
        RequestKind::Font => (
            crate::protocol::RequestPolicy::Font,
            Some(crate::csp::Destination::Font),
        ),
        RequestKind::Other => (crate::protocol::RequestPolicy::Image { crossorigin }, None),
    }
}

/// The frame factory a realm calls when a page creates a child or auxiliary
/// document.
///
/// The closure only queues the document: `QuickJS` cannot enter a new context
/// while the script that created the frame still runs, so the carrier
/// materializes it later from the queue.
fn frame_host(new_frames: &Rc<RefCell<Vec<CreatedFrame>>>) -> crate::js::world::FrameHost {
    crate::js::world::FrameHost {
        child: Rc::new({
            let new_frames = Rc::clone(new_frames);
            move |world_rc: &Rc<RefCell<World>>, frame, container| {
                materialize_iframe_document(world_rc, frame, container, &new_frames);
            }
        }),
        auxiliary: Rc::new({
            let new_frames = Rc::clone(new_frames);
            move |world_rc: &Rc<RefCell<World>>, frame, opener: crate::protocol::OpenerPolicy| {
                materialize_auxiliary_document(world_rc, frame, opener, &new_frames);
            }
        }),
    }
}

/// Builds and queues the initial `about:blank` document for one connected
/// `iframe` so `contentDocument` is available in the inserting script
/// (<https://html.spec.whatwg.org/multipage/iframe-embed-object.html#the-iframe-element:html-element-insertion-steps>).
///
/// The nested realm is deferred: `QuickJS` cannot enter a new context while
/// the parent script still runs.
fn materialize_iframe_document(
    world_rc: &Rc<RefCell<World>>,
    frame: FrameId,
    container: crate::dom::tree::NodeId,
    new_frames: &Rc<RefCell<Vec<CreatedFrame>>>,
) {
    // The about base URL is the creator's document base URL, so a parent
    // `<base href>` reaches the child
    // (<https://html.spec.whatwg.org/multipage/document-sequences.html#creating-browsing-contexts>).
    let base_url = world_rc.borrow().main_base_url();
    let (runtime, referrer_url, init_scripts, viewport) = {
        let world = world_rc.borrow();
        (
            world.runtime.clone(),
            world.referrer_url.clone(),
            Rc::clone(&world.init_scripts),
            world.viewport_size.get(),
        )
    };
    // The container's `sandbox` attribute, unioned with the embedder's active
    // flags, decides the flag set for every document this frame loads
    // (<https://html.spec.whatwg.org/multipage/browsers.html#determining-the-creation-sandboxing-flags>).
    let sandbox = sandboxing_flags(world_rc, container);
    let origin = if sandbox.origin_is_opaque() {
        url::Origin::new_opaque()
    } else {
        world_rc.borrow().origin.clone()
    };
    let mut document = Document::with_shared(frame, &runtime, &init_scripts);
    document.set_viewport_size(viewport);
    {
        let world_handle = document.world();
        let mut world = world_handle.borrow_mut();
        world.policy = world_rc.borrow().policy.clone();
        world.sandbox = sandbox;
        world.top_level = Some(world_rc.borrow().top_level_origin());
        world.set_parent_context(&world_rc.borrow());
    }
    document.load_about_blank_tree(
        Some(&base_url),
        &world_rc.borrow().cookie_url(),
        Some(origin),
    );
    // The initial blank reports its creator's URL, walked out of a `srcdoc`
    // chain the same way the referrer source is
    // (<https://html.spec.whatwg.org/multipage/browsers.html#creating-a-new-browsing-context>).
    document.set_document_referrer(Some(referrer_url));
    if let Some(root) = document.document_root() {
        world_rc
            .borrow()
            .registry()
            .borrow_mut()
            .set_frame_document(container, root);
    }
    new_frames.borrow_mut().push(CreatedFrame::Child {
        frame,
        container,
        document,
    });
}

/// Builds the initial `about:blank` document of an auxiliary top-level
/// context opened by `world_rc`'s realm
/// (<https://html.spec.whatwg.org/multipage/document-sequences.html#creating-a-new-auxiliary-browsing-context>).
///
/// The document inherits its creator's origin, policy container, sandboxing
/// flags, base URL, and viewport, and reports the creator's full URL as its
/// referrer. Its URL stays `about:blank`. The realm is deferred, because
/// `window.open` runs inside the opener's realm and a nested `QuickJS` realm
/// cannot be entered there; the engine creates it when it adopts the frame.
fn materialize_auxiliary_document(
    world_rc: &Rc<RefCell<World>>,
    frame: FrameId,
    opener: crate::protocol::OpenerPolicy,
    new_frames: &Rc<RefCell<Vec<CreatedFrame>>>,
) {
    let (runtime, url, origin, policy, sandbox, init_scripts, viewport) = {
        let world = world_rc.borrow();
        (
            world.runtime.clone(),
            world.document_url.clone(),
            world.origin.clone(),
            world.policy.clone(),
            world.sandbox.auxiliary_flags(),
            Rc::clone(&world.init_scripts),
            world.viewport_size.get(),
        )
    };
    let mut document = Document::with_shared(frame, &runtime, &init_scripts);
    document.set_viewport_size(viewport);
    // The sandbox flags are installed before the blank document is built: an
    // inherited sandboxed-origin flag decides the origin `load_about_blank_tree`
    // captures
    // (<https://html.spec.whatwg.org/multipage/document-sequences.html#creating-a-new-auxiliary-browsing-context>).
    {
        let world_handle = document.world();
        let mut world = world_handle.borrow_mut();
        world.policy = policy;
        world.install_sandbox(sandbox);
    }
    let origin = if sandbox.origin_is_opaque() {
        url::Origin::new_opaque()
    } else {
        origin
    };
    document.load_about_blank_tree(Some(&url), &world_rc.borrow().cookie_url(), Some(origin));
    if opener == crate::protocol::OpenerPolicy::NoReferrer {
        document.set_document_referrer(None);
    }
    new_frames
        .borrow_mut()
        .push(CreatedFrame::Auxiliary { frame, document });
}

/// The sandboxing flags the `iframe` at `container` imposes: its `sandbox`
/// content attribute unioned with the active flags of the document that owns
/// it
/// (<https://html.spec.whatwg.org/multipage/browsers.html#determining-the-creation-sandboxing-flags>).
pub(crate) fn sandboxing_flags(
    world_rc: &Rc<RefCell<World>>,
    container: crate::dom::tree::NodeId,
) -> crate::protocol::SandboxFlags {
    let world = world_rc.borrow();
    let Some(parsed) = world.document(container) else {
        return world.sandbox;
    };
    let base = &parsed.document.base;
    if !crate::dom::tree::is_html_element(base, container.node, "iframe") {
        return world.sandbox;
    }
    let attribute = crate::dom::tree::attr(base, container.node, "sandbox");
    crate::protocol::SandboxFlags::parse(attribute).union(world.sandbox)
}

/// One parse-time step over a document's element tree, in tree order.
enum ParseStep {
    Base(crate::dom::tree::NodeId),
    Meta(crate::dom::tree::NodeId),
    Script(crate::dom::tree::NodeId),
}

/// The cache key a repeated Blitz subresource request is filed under: the URL
/// the element asked for and the CORS settings state it asked with. Image
/// bodies are not cached here: Blitz keys its own image cache by request and
/// owns the decoded pixels.
type SubresourceKey = (Url, Option<blitz_traits::net::CorsSetting>);

/// One filed Blitz subresource request: its handler, the request key it went
/// out under, and the element that initiated it. The element is carried, not
/// recovered from the URL, so two elements on one URL keep their own
/// `crossorigin`, `referrerpolicy`, and `error` behavior
/// (<https://fetch.spec.whatwg.org/#concept-request-client>).
struct BlitzRequest {
    handler: crate::render::CountingHandler,
    /// The URL and CORS settings state the request went out under, as Blitz
    /// keys its cache and settlements.
    key: blitz_dom::ImageKey,
    /// The `referrerpolicy` state sampled when Blitz created the request.
    referrer_policy: Option<blitz_traits::net::ReferrerPolicy>,
    node: Option<crate::dom::tree::NodeId>,
    destination: Option<crate::csp::Destination>,
}

pub(crate) use net::about_blank;

/// One document: tree, task list, `QuickJS` realm, and browser services.
#[expect(
    clippy::struct_excessive_bools,
    reason = "each bool mirrors an HTML document or parser flag"
)]
pub(crate) struct Document {
    services: Arc<dyn BrowserServices>,
    world: Rc<RefCell<World>>,
    js_runtime: JsRuntimeHandle,
    wake: Arc<Notify>,
    /// Blitz subresource fetches run through our agent on the carrier
    /// runtime; Blitz owns stylesheets and images once they land.
    net_provider: std::sync::Arc<crate::render::TinyNetProvider>,
    /// Blitz link/form navigations are not routed: tinybrowser never calls
    /// Blitz's event driver, and the renderer's own JS bindings perform the
    /// navigations. A Blitz navigation target is therefore inert.
    blitz_shell: std::sync::Arc<crate::render::TinyShell>,
    /// Resolve passes run by [`Self::settle_blitz_layout`]. A test resets and
    /// reads it to prove that several mutations between two paints settle once.
    #[cfg(test)]
    blitz_resolve_passes: u64,
    /// Image settlements from Blitz, drained on this document's thread.
    image_settle_rx: std::sync::mpsc::Receiver<blitz_dom::ImageSettlement>,
    /// Blitz subresource fetches awaiting carrier dials.
    blitz_fetch_rx:
        std::sync::mpsc::Receiver<(crate::render::BlitzFetch, crate::render::CountingHandler)>,
    /// Filed Blitz subresource requests by fetch id, delivered on completion.
    blitz_requests: HashMap<u64, BlitzRequest>,
    /// Stylesheet and font bodies by request, so a repeat Blitz fetch reuses
    /// the response instead of dialing again. Images are absent: Blitz's own
    /// cache answers a repeat request with no dial
    /// (<https://html.spec.whatwg.org/multipage/links.html#link-type-stylesheet>,
    /// <https://drafts.csswg.org/css-fonts-4/#font-face-rule>).
    subresource_bytes: HashMap<SubresourceKey, Vec<u8>>,
    /// Referrer policy each fetched CSS style sheet's response carried, keyed
    /// by the URL the sheet was located at. A request the sheet initiates uses
    /// it and the sheet's URL, not the document's own
    /// (<https://w3c.github.io/webappsec-referrer-policy/#integration-with-css>).
    style_policies: HashMap<String, net::ReferrerPolicy>,
    /// The browsing context this document belongs to.
    frame: FrameId,
    /// Whether this frame has committed a document since its engine started.
    ///
    /// The target-name reset compares the new document's origin with the
    /// *active* document's, and a frame's first document has no predecessor to
    /// compare against: a tab's first mount carries the name the browser
    /// already resolved
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName>).
    committed: bool,
    /// Whether the next commit arrives from the browser with the target name
    /// already resolved.
    ///
    /// A mount carries [`crate::protocol::TabState`], whose name the browser
    /// computed against the document it is replacing
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName>).
    /// A commit this renderer drives, such as a child frame's response, has no
    /// browser step and resolves the name here, so the reset still runs. The
    /// flag is consumed by the commit it describes.
    name_resolved: bool,
    /// The frame's persistent viewport in device pixels (the scale is fixed
    /// at 1, so device and CSS pixels coincide). CDP emulation and every
    /// document mount use it, so Blitz layout, the painted size, and
    /// `window` metrics all agree
    /// (<https://chromedevtools.github.io/devtools-protocol/tot/Emulation/#method-setDeviceMetricsOverride>).
    pub(crate) viewport_size: (u32, u32),
    /// The renderer-process state every frame shares.
    shared: SharedHandle,
    url: Url,
    tasks: VecDeque<Task>,
    timers: HashMap<TimerId, Timer>,
    next_timer_id: TimerId,
    /// Every queued, in-flight, and cancelled dial for this document.
    dials: DialLedger,
    /// Image `load`/`error` events queued while the parser is running. They
    /// fire after the parser stops, so a handler the document's own markup
    /// installs later still sees them
    /// (<https://html.spec.whatwg.org/multipage/images.html#update-the-image-data>).
    deferred_image_events: VecDeque<Task>,
    /// `securitypolicyviolation` events reported while the parser was still
    /// active. They wait for the parser so the listeners the document's own
    /// scripts install are in place
    /// (<https://w3c.github.io/webappsec-csp/#report-violation>).
    pending_csp_events: VecDeque<Task>,
    /// The token that delays this document's `load` event for each connected
    /// `<img>` whose current request has not settled
    /// (<https://html.spec.whatwg.org/multipage/images.html#update-the-image-data>,
    /// <https://html.spec.whatwg.org/multipage/parsing.html#delay-the-load-event>).
    image_load_delays: HashMap<crate::dom::tree::NodeId, LoadDelayGuard>,
    /// Connected `<img>` elements the last lifecycle scan saw, so removal
    /// drops their state
    /// (<https://html.spec.whatwg.org/multipage/images.html#updating-the-image-data>).
    connected_images: HashSet<crate::dom::tree::NodeId>,
    /// Connected `<link rel=stylesheet>` elements already reported as loaded,
    /// with the resolved URL that arrival was for, so a re-fetch after an
    /// `href` or `rel` change reports again
    /// (<https://html.spec.whatwg.org/multipage/semantics.html#the-link-element>).
    linked_sheets: HashMap<crate::dom::tree::NodeId, String>,
    /// Stylesheet generation observed at the last linked-stylesheet scan.
    stylesheet_generation: u64,
    /// Whether the frame is still on the `about:blank` document a browser
    /// creates at insertion. A URL comparison cannot answer this: a frame
    /// whose `src` resolves to the parent's own URL is still on that document
    /// (<https://html.spec.whatwg.org/multipage/iframe-embed-object.html#process-the-iframe-attributes>).
    initial_blank: bool,
    /// The `iframe` containers awaiting their child's load event; this
    /// document's own `load` event is delayed until they all have
    /// (<https://html.spec.whatwg.org/multipage/parsing.html#delay-the-load-event>).
    /// Each value is that container's delay token.
    pending_frame_loads: HashMap<crate::dom::tree::NodeId, LoadDelayGuard>,
    /// `history.back()`/`go()` calls from a child frame. A browser-hosted
    /// engine forwards them to the tab actor, which owns the joint steps; an
    /// embedded carrier applies them against the frame's replay store.
    frame_traversals: Vec<i32>,
    /// Same-document history updates this document applied, for a standalone
    /// engine's joint list. A browser-hosted engine reads the matching
    /// `HistoryUpdated` events instead.
    history_updates: Vec<HistoryUpdate>,
    /// Child frame documents this document's realm created for the engine to
    /// adopt
    /// (<https://html.spec.whatwg.org/multipage/document-sequences.html#document-tree-child-navigable>).
    new_frames: Rc<RefCell<Vec<CreatedFrame>>>,
    /// Whether this document's `load` event has fired.
    load_fired: bool,
    /// Whether the shared declarative refresh steps already ran for this
    /// document; a later `Refresh` value is ignored
    /// (<https://html.spec.whatwg.org/multipage/semantics.html#attr-meta-http-equiv-refresh>).
    will_declaratively_refresh: bool,
    /// The target of a refresh waiting for the document to finish loading,
    /// with its delay. The clock starts at the completely-loaded point, not
    /// when the value was parsed.
    pending_refresh: Option<(Url, std::time::Duration)>,
    /// The timer a refresh is waiting on, so a new navigation can cancel it.
    refresh_timer: Option<TimerId>,
    /// The URL an armed refresh navigates to when its timer fires.
    refresh_target: Option<Url>,
    events: Vec<RendererEvent>,
    events_overflowed: bool,
    js: Option<crate::js::JsRealm>,
    /// Live JS timers by the id script holds. The timer owns the page id.
    js_timers: HashMap<PageTimerId, TimerId>,
    /// One count for every operation delaying this document's `load` event.
    load_delay: LoadDelay,
    /// Whether init scripts were deferred because the realm was created while
    /// a navigation response was still streaming.
    init_scripts_deferred: bool,
    /// Skip realm creation while an auxiliary or child document is installed
    /// from inside the opener's live realm. Creating the realm there loses
    /// the opener's load event; the engine creates it when it adopts the
    /// frame instead.
    defer_js: bool,
    /// The current parse's markup and delay, or `None` when no parse is active.
    active_parse: Option<ActiveParse>,
    /// Whether `document.open` or `document.close` ran and the engine still
    /// needs to publish the frame's document. `Some(true)` also restarts the
    /// parent's load wait for a closed stream.
    stream_publish: Option<bool>,
    /// Scripts already executed for the current parse, so a walk resumed
    /// after a `src` fetch does not run them twice.
    executed_scripts: HashSet<crate::dom::tree::NodeId>,
    parser_eof: bool,
    /// Who owns the active parser, which decides whether `document.close()` may
    /// end it.
    parser_owner: ParserOwner,
    /// Decoder for a body that is still arriving; `None` when the markup is
    /// already in hand.
    decoder: Option<dial::ResponseDecoder>,
    /// Encoding sniffed for the current response, reported as
    /// `document.characterSet`.
    character_set: &'static str,
    /// The response `Content-Type` for the active parser. XML MIME types
    /// select the XML parser; anything else, including a script's
    /// `document.open()`, stays HTML.
    response_content_type: Option<String>,
    /// `Content-Security-Policy` and `Content-Security-Policy-Report-Only`
    /// from the response being parsed, parsed once
    /// (<https://w3c.github.io/webappsec-csp/#parse-response-csp>).
    response_policy: crate::csp::List,
    /// The sandbox flags the response's enforced CSP `sandbox` directives
    /// impose, kept beside the policy so a local-scheme document inherits the
    /// same container
    /// (<https://w3c.github.io/webappsec-csp/#sandbox>).
    response_sandbox: crate::protocol::SandboxFlags,
    /// `Referrer-Policy` from the response being parsed, applied after the
    /// parser installs the new document (the state reset clears the realm).
    response_referrer_policy: Option<net::ReferrerPolicy>,
    /// Whether the response being parsed is a secure context, computed at
    /// the head from its URL and its creator's bit
    /// (<https://html.spec.whatwg.org/multipage/webappapis.html#secure-contexts>).
    response_secure_context: bool,
    /// The response's `Cross-Origin-Opener-Policy` and
    /// `Cross-Origin-Embedder-Policy` values
    /// (<https://html.spec.whatwg.org/multipage/browsers.html#cross-origin-opener-policies>).
    response_cross_origin_opener_policy: crate::protocol::CrossOriginOpenerPolicy,
    response_cross_origin_embedder_policy: crate::protocol::CrossOriginEmbedderPolicy,
    /// Whether the response being parsed is cross-origin isolated, computed at
    /// the head from its own headers, its secure context, and its creator
    /// (<https://html.spec.whatwg.org/multipage/webappapis.html#concept-settings-object-cross-origin-isolated-capability>).
    response_cross_origin_isolated: bool,
    /// The creator's secure-context bit for the next navigation this document
    /// commits. `None` when the browser started the navigation, which has no
    /// creator (`is_secure_environment` then reads the URL alone).
    creator_secure_context: Option<bool>,
    /// The creator's cross-origin isolation for the next navigation this
    /// document commits. A nested document needs its ancestor's capability as
    /// well as its own embedder policy.
    creator_cross_origin_isolated: Option<bool>,
    /// The policy container a local-scheme document inherits, installed
    /// atomically after the parser replaces the document's state
    /// (<https://html.spec.whatwg.org/multipage/browsers.html#policy-containers>).
    inherited_policy: Option<PolicyContainer>,
    /// When set, [`Document::install_parsed`] replaces the world's policy
    /// with the response headers. A script's `document.open()` leaves it
    /// unset so the existing policy survives.
    csp_from_response: bool,
    /// Endpoint names and URLs from the navigation response's
    /// `Reporting-Endpoints` headers. A violated policy's `report-to`
    /// directive resolves its first known name here
    /// (<https://w3c.github.io/reporting/#header>).
    ///
    /// Only [`Document::install_head`] and `set_reporting_endpoints` write
    /// it; no caller reaches around them.
    reporting_endpoints: Vec<(String, String)>,
    /// HTTP status of the committed navigation response, reported as the
    /// violation `status-code`. Zero when unknown
    /// (<https://w3c.github.io/webappsec-csp/#create-violation-for-global>).
    http_status: u16,
    /// First 1445 raw response bytes, the MIME sniff resource header.
    sniff_header: Vec<u8>,
    /// Whether [`Document::finish_parse`] runs the MIME sniffing algorithm.
    apply_mime_sniff: bool,
    /// `nosniff` and whether this body came from HTTP.
    mime_guard: crate::sniff::MimeGuard,
    /// The parser-inserted script the parser waits for, if any. A dynamic
    /// script that finishes first must not resume the walk: the synchronous
    /// scripts after a parser-inserted `src` run only once it executes
    /// (<https://html.spec.whatwg.org/multipage/scripting.html#prepare-the-script-element>).
    parser_blocked_script: Option<crate::dom::tree::NodeId>,
    deferred_modules: Vec<(crate::dom::tree::NodeId, crate::js::ScriptSource)>,
    stop: Arc<crate::js::Stop>,
    /// `frame-src` redirect violations to fire in the embedding document.
    /// The navigating frame cannot dispatch into its parent's realm.
    embedder_csp_reports: Vec<(FrameId, Vec<crate::csp::Violation>)>,
}

impl Drop for Document {
    fn drop(&mut self) {
        self.release();
    }
}

/// Parses one declarative refresh value: `"<seconds>"` or
/// `"<seconds>; url=<url>"`, with or without the `url=` prefix, optional
/// quotes, and any ASCII whitespace. `None` is the algorithm's early return,
/// which means the value names nothing to refresh to. The target defaults to
/// the document's own URL.
/// (<https://html.spec.whatwg.org/multipage/semantics.html#attr-meta-http-equiv-refresh>).
fn parse_declarative_refresh(
    input: &str,
    document_url: &Url,
) -> Option<(Url, std::time::Duration)> {
    let mut position = 0;
    skip_ascii_whitespace(input, &mut position);
    let time_start = position;
    while next_char(input, position).is_some_and(|c| c.is_ascii_digit()) {
        position += 1;
    }
    let time = if position == time_start {
        if next_char(input, position) != Some('.') {
            return None;
        }
        0
    } else {
        input[time_start..position]
            .parse::<u64>()
            .unwrap_or(u64::MAX)
    };
    while next_char(input, position).is_some_and(|c| c.is_ascii_digit() || c == '.') {
        position += 1;
    }
    let mut target = document_url.clone();
    if position < input.len() {
        let separator = next_char(input, position)?;
        if separator != ';' && separator != ',' && !separator.is_ascii_whitespace() {
            return None;
        }
        skip_ascii_whitespace(input, &mut position);
        if matches!(next_char(input, position), Some(';' | ',')) {
            position += 1;
        }
        skip_ascii_whitespace(input, &mut position);
    }
    if position < input.len() {
        let mut url_start = position;
        // `url=` is optional and case-insensitive. A prefix without `=` falls
        // through to the URL text itself, so `0; urlish` is a relative URL.
        if starts_with_ascii_case(&input[position..], "url") {
            let mut probe = position + 3;
            skip_ascii_whitespace(input, &mut probe);
            if next_char(input, probe) == Some('=') {
                probe += 1;
                skip_ascii_whitespace(input, &mut probe);
                url_start = probe;
            }
        }
        let mut text = input.get(url_start..)?;
        let quote = match text.chars().next() {
            Some(quote @ ('\'' | '"')) => {
                text = &text[1..];
                Some(quote)
            }
            _ => None,
        };
        let text = match quote {
            Some(quote) => text.split_once(quote).map_or(text, |(before, _)| before),
            None => text,
        };
        let text = text.trim();
        // The document's own URL stands when the value names nothing.
        target = if text.is_empty() {
            document_url.clone()
        } else {
            document_url.join(text).ok()?
        };
        if target.scheme() == "javascript" {
            return None;
        }
    }
    Some((target, std::time::Duration::from_secs(time)))
}

fn skip_ascii_whitespace(input: &str, position: &mut usize) {
    while let Some(rest) = input.get(*position..) {
        match rest.chars().next() {
            Some(character) if character.is_ascii_whitespace() => {
                *position += character.len_utf8();
            }
            _ => break,
        }
    }
}

fn next_char(input: &str, position: usize) -> Option<char> {
    input.get(position..)?.chars().next()
}

fn starts_with_ascii_case(input: &str, prefix: &str) -> bool {
    input.len() >= prefix.len() && input[..prefix.len()].eq_ignore_ascii_case(prefix)
}

impl Document {
    /// A document sharing its renderer process's `QuickJS` heap, wake handle,
    /// document store, realm registry, and frame tree.
    pub(crate) fn with_shared(
        frame: FrameId,
        runtime: &FrameRuntime,
        init_scripts: &Rc<RefCell<Vec<(u64, String)>>>,
    ) -> Self {
        let document_url = about_blank();
        runtime.shared.borrow_mut().begin_document(frame);
        let world = Rc::new(RefCell::new(World::new(
            document_url.clone(),
            frame,
            runtime,
            Rc::clone(init_scripts),
        )));
        runtime.registry.borrow_mut().insert_frame(frame, &world);
        // Document creation belongs to this layer: the realm asks the frame
        // host for a child or auxiliary document and never names a document
        // type.
        let new_frames: Rc<RefCell<Vec<CreatedFrame>>> = Rc::new(RefCell::new(Vec::new()));
        world.borrow_mut().set_frame_host(frame_host(&new_frames));
        // Blitz subresources enqueue here and ride carrier dials: the
        // renderer owns no network runtime of its own.
        let (blitz_tx, blitz_rx) = std::sync::mpsc::channel();
        let (settle_tx, settle_rx) = std::sync::mpsc::channel();
        let net_provider =
            std::sync::Arc::new(crate::render::TinyNetProvider::new(blitz_tx, settle_tx));
        Self {
            world,
            services: Arc::clone(&runtime.services),
            js_runtime: runtime.js_runtime.clone(),
            wake: Arc::clone(&runtime.wake),
            net_provider,
            blitz_shell: std::sync::Arc::new(crate::render::TinyShell::new()),
            #[cfg(test)]
            blitz_resolve_passes: 0,
            blitz_fetch_rx: blitz_rx,
            image_settle_rx: settle_rx,
            blitz_requests: HashMap::new(),
            subresource_bytes: HashMap::new(),
            style_policies: HashMap::new(),
            frame,
            committed: false,
            name_resolved: false,
            viewport_size: crate::engine::DEFAULT_VIEWPORT,
            shared: runtime.shared.clone(),
            url: document_url,
            tasks: VecDeque::new(),
            timers: HashMap::new(),
            next_timer_id: TimerId::default(),
            dials: DialLedger::new(Arc::clone(&runtime.stop), Arc::clone(&runtime.wake)),
            deferred_image_events: VecDeque::new(),
            pending_csp_events: VecDeque::new(),
            image_load_delays: HashMap::new(),
            connected_images: HashSet::new(),
            linked_sheets: HashMap::new(),
            stylesheet_generation: 0,
            initial_blank: true,
            pending_frame_loads: HashMap::new(),
            frame_traversals: Vec::new(),
            history_updates: Vec::new(),
            new_frames,
            load_fired: false,
            will_declaratively_refresh: false,
            pending_refresh: None,
            refresh_timer: None,
            refresh_target: None,
            events: Vec::new(),
            events_overflowed: false,
            js: None,
            js_timers: HashMap::new(),
            load_delay: LoadDelay::default(),
            init_scripts_deferred: false,
            defer_js: false,
            active_parse: None,
            stream_publish: None,
            executed_scripts: HashSet::new(),
            parser_eof: true,
            parser_owner: ParserOwner::Carrier,
            decoder: None,
            character_set: "UTF-8",
            response_content_type: None,
            response_policy: crate::csp::List::default(),
            response_sandbox: crate::protocol::SandboxFlags::default(),
            response_referrer_policy: None,
            response_secure_context: false,
            response_cross_origin_opener_policy:
                crate::protocol::CrossOriginOpenerPolicy::UnsafeNone,
            response_cross_origin_embedder_policy:
                crate::protocol::CrossOriginEmbedderPolicy::UnsafeNone,
            response_cross_origin_isolated: false,
            creator_secure_context: None,
            creator_cross_origin_isolated: None,
            inherited_policy: None,
            embedder_csp_reports: Vec::new(),
            csp_from_response: false,
            reporting_endpoints: Vec::new(),
            http_status: 0,
            sniff_header: Vec::new(),
            apply_mime_sniff: false,
            mime_guard: crate::sniff::MimeGuard::default(),
            parser_blocked_script: None,
            deferred_modules: Vec::new(),
            stop: Arc::clone(&runtime.stop),
        }
    }

    /// The world this document's realm belongs to.
    pub(crate) fn world(&self) -> Rc<RefCell<World>> {
        Rc::clone(&self.world)
    }

    /// The storage key of this document's origin, or `None` for an opaque
    /// origin, where the storage getters throw `SecurityError`
    /// (<https://html.spec.whatwg.org/multipage/webstorage.html#the-localstorage-attribute>).
    pub(crate) fn storage_key(&self) -> Option<crate::protocol::StorageKey> {
        self.world.borrow().storage_origin()
    }

    /// The root node of the frame's active document.
    pub(crate) fn document_root(&self) -> Option<crate::dom::tree::NodeId> {
        self.world
            .borrow()
            .with_main_document(|parsed| crate::dom::tree::NodeId {
                document: parsed.id,
                node: parsed.document.base.root_node().id,
            })
    }

    /// The `iframe`'s `referrerpolicy` attribute parsed to a policy, or
    /// `None` when absent or invalid, which leaves the document's policy in
    /// force
    /// (<https://html.spec.whatwg.org/multipage/iframe-embed-object.html#attr-iframe-referrerpolicy>).
    #[must_use]
    pub(crate) fn frame_referrer_policy(
        &self,
        container: crate::dom::tree::NodeId,
    ) -> Option<net::ReferrerPolicy> {
        let world = self.world.borrow();
        world.document(container).and_then(|parsed| {
            crate::dom::tree::attr(&parsed.document.base, container.node, "referrerpolicy")
                .and_then(net::ReferrerPolicy::parse)
        })
    }

    /// The `iframe`'s `src` attribute value, when the element has a
    /// non-empty one
    /// (<https://html.spec.whatwg.org/multipage/iframe-embed-object.html#attr-iframe-src>).
    #[must_use]
    pub(crate) fn frame_src(&self, container: crate::dom::tree::NodeId) -> Option<String> {
        let world = self.world.borrow();
        let value = world.document(container).and_then(|parsed| {
            crate::dom::tree::attr(&parsed.document.base, container.node, "src").map(str::to_owned)
        })?;
        (!value.is_empty()).then_some(value)
    }

    /// The `iframe`'s `srcdoc` attribute value, when the element has one —
    /// even an empty one, which loads the empty `about:srcdoc` document
    /// (<https://html.spec.whatwg.org/multipage/iframe-embed-object.html#attr-iframe-srcdoc>).
    #[must_use]
    pub(crate) fn frame_srcdoc(&self, container: crate::dom::tree::NodeId) -> Option<String> {
        let world = self.world.borrow();
        world.document(container).and_then(|parsed| {
            crate::dom::tree::attr(&parsed.document.base, container.node, "srcdoc")
                .map(str::to_owned)
        })
    }

    /// Resolves one of the frame's URLs the way an attribute would: absolute
    /// against the document, or joined with its base URL
    /// (<https://html.spec.whatwg.org/multipage/urls-and-fetching.html#parse-a-url>).
    #[must_use]
    pub(crate) fn resolve_frame_url(&self, spec: &str) -> Option<Url> {
        Url::parse(spec)
            .or_else(|_| self.base_url().join(spec))
            .ok()
    }

    /// An object URL's contents, when this document's origin created it
    /// (<https://w3c.github.io/FileAPI/#blob-url-store>).
    #[must_use]
    pub(crate) fn object_url_contents(&self, url: &str) -> Option<Arc<str>> {
        self.world.borrow().object_url_contents(url)
    }

    /// The `Content-Type` an object URL was created from.
    #[must_use]
    pub(crate) fn object_url_type(&self, url: &str) -> Option<Arc<str>> {
        self.world.borrow().object_url_type(url)
    }

    /// The creator origin stored for an object URL this document may use.
    #[must_use]
    pub(crate) fn object_url_origin(&self, url: &str) -> Option<url::Origin> {
        self.world.borrow().object_url_origin(url)
    }

    /// Creates the browsing context of every connected `iframe` that lacks
    /// one, so a script sees it in the same task
    /// (<https://html.spec.whatwg.org/multipage/iframe-embed-object.html#process-the-iframe-attributes>).
    pub(crate) fn adopt_pending_frames(&mut self) {
        self.adopt_pending_frames_before_node(None);
    }

    /// Creates browsing contexts for connected `iframe`s that precede `before`
    /// in tree order, or every remaining iframe when `before` is `None`.
    ///
    /// Whole-document parse inserts every iframe before any script runs. A
    /// real incremental parser would only have inserted iframes that precede
    /// the current script; src loads that start earlier fire `load` before
    /// later classic scripts define their onload functions
    /// (<https://html.spec.whatwg.org/multipage/iframe-embed-object.html#the-iframe-element:html-element-insertion-steps>).
    fn adopt_pending_frames_before_node(&mut self, before: Option<crate::dom::tree::NodeId>) {
        let created = self.world.borrow_mut().register_pending_frames(before);
        self.materialize_pending_frames();
        for container in created {
            // The child is loading from this moment on, so this document's
            // `load` event waits for it
            // (<https://html.spec.whatwg.org/multipage/parsing.html#delay-the-load-event>).
            self.mark_frame_load_pending(container);
        }
    }

    /// Loads every registered frame document without holding the world
    /// borrow: pending frames and frame data are taken first, documents load
    /// unborrowed, and only then are the results published. Loading runs the
    /// parser, which delivers mutations, which fans out across live worlds;
    /// holding the world's borrow across that would alias the fan-out
    /// borrows and panic the renderer.
    fn materialize_pending_frames(&mut self) {
        let pending = self.world.borrow_mut().take_pending_frames();
        if pending.is_empty() {
            return;
        }
        let base_url = self.base_url();
        let (runtime, referrer_url, origin, init_scripts, viewport) = {
            let world = self.world.borrow();
            (
                world.runtime.clone(),
                world.referrer_url.clone(),
                world.origin.clone(),
                Rc::clone(&world.init_scripts),
                world.viewport_size.get(),
            )
        };
        let mut loaded = Vec::with_capacity(pending.len());
        let inherited = self.world.borrow().policy.clone();
        let top_level = self.world.borrow().top_level_origin();
        for (frame, container) in pending {
            // Parser-created and script-created frames share this funnel's
            // rule: the container's `sandbox` attribute unioned with this
            // document's active flags decides the set, and an opaque origin
            // when the origin flag is in it
            // (<https://html.spec.whatwg.org/multipage/browsers.html#determining-the-creation-sandboxing-flags>).
            let sandbox = sandboxing_flags(&self.world, container);
            let origin = if sandbox.origin_is_opaque() {
                url::Origin::new_opaque()
            } else {
                origin.clone()
            };
            let mut document = Document::with_shared(frame, &runtime, &init_scripts);
            // Frames created from parsed markup share the tab's viewport too.
            // This path runs before page script, so the realm can exist for
            // `contentWindow` in the following classic script
            // (<https://html.spec.whatwg.org/multipage/iframe-embed-object.html#the-iframe-element:html-element-insertion-steps>).
            document.set_viewport_size(viewport);
            {
                let world_handle = document.world();
                let mut world = world_handle.borrow_mut();
                world.policy = inherited.clone();
                world.sandbox = sandbox;
                world.top_level = Some(top_level.clone());
                world.set_parent_context(&self.world.borrow());
            }
            // The document a following classic script reads through
            // `contentDocument`: the blank tree exists before that script
            // runs, exactly as it does for a script-created iframe
            // (<https://html.spec.whatwg.org/multipage/iframe-embed-object.html#the-iframe-element:html-element-insertion-steps>).
            let cookie_url = self.world.borrow().cookie_url();
            document.load_about_blank_tree(Some(&base_url), &cookie_url, Some(origin));
            document.set_document_referrer(Some(referrer_url.clone()));
            if let Some(root) = document.document_root() {
                document
                    .world()
                    .borrow()
                    .registry()
                    .borrow_mut()
                    .set_frame_document(container, root);
            }
            loaded.push(CreatedFrame::Child {
                frame,
                container,
                document,
            });
        }
        self.new_frames.borrow_mut().extend(loaded);
    }

    /// Applies writes the realm stored on a child frame's `contentWindow`
    /// before that frame's realm existed.
    pub(crate) fn flush_frame_proxy_sets(&mut self, child: FrameId) {
        self.fire_js(|js| js.flush_frame_sets(child.get()));
        self.adopt_js_work();
    }

    /// Whether this document has a JS realm, so a `WindowProxy` write can be
    /// replayed into it.
    #[must_use]
    pub(crate) const fn has_realm(&self) -> bool {
        self.js.is_some()
    }

    /// Whether this frame still owes the pre-realm `WindowProxy` writes a replay.
    #[must_use]
    pub(crate) fn needs_proxy_sets_replay(&self) -> bool {
        self.world.borrow().needs_proxy_sets_replay()
    }

    /// Whether this frame's `load` waits for the engine to replay the writes
    /// made through its `WindowProxy` before its realm existed.
    ///
    /// A frame's realm starts inside the response install, which also calls
    /// [`Self::check_load`], so without this the `load` event would fire before
    /// an `onload` assigned at `window.open` or `iframe.contentWindow` reached
    /// the frame
    /// (<https://html.spec.whatwg.org/multipage/window-object.html#windowproxy-set>).
    #[must_use]
    pub(crate) fn awaits_proxy_replay(&self) -> bool {
        self.frame.get() != FrameId::MAIN.get() && self.needs_proxy_sets_replay()
    }

    /// Whether a load of a new document for this frame is queued or in
    /// flight, so this document is being replaced.
    #[must_use]
    pub(crate) fn has_pending_frame_load(&self) -> bool {
        self.dials.has_pending_frame_load()
    }

    /// Records that the pre-realm writes reached this frame's realm.
    pub(crate) fn mark_proxy_sets_replayed(&mut self) {
        self.world.borrow_mut().mark_proxy_sets_replayed();
    }

    /// Takes the child frames this document's realm created for the engine to
    /// adopt.
    pub(crate) fn take_new_frames(&mut self) -> Vec<CreatedFrame> {
        std::mem::take(&mut *self.new_frames.borrow_mut())
    }

    /// The document URL used by `about:blank` frames, which inherit the
    /// parent's origin and, in this engine, its URL
    /// (<https://html.spec.whatwg.org/multipage/browsers.html#determining-the-origin>).
    #[must_use]
    pub(crate) fn inherited_url(&self) -> Url {
        self.url.clone()
    }

    /// Sets the referrer the active document reports. Local-scheme installs
    /// have no navigation request to read one from; their document reports
    /// the creator's URL instead
    /// (<https://html.spec.whatwg.org/multipage/browsers.html#creating-a-new-browsing-context>).
    pub(crate) fn set_document_referrer(&mut self, referrer: Option<Url>) {
        self.world.borrow_mut().document_referrer = referrer;
    }

    /// The state this document needs to be installed again from a
    /// session-history entry.
    pub(crate) fn local_document_state(&self) -> LocalDocumentState {
        let world = self.world.borrow();
        LocalDocumentState {
            url: self.url.clone(),
            base_url: world.base_url.clone(),
            cookie_url: world.inherited_cookie_url.clone(),
            referrer: world.document_referrer.clone(),
            referrer_url: world.referrer_url.clone(),
            origin: world.origin.clone(),
            policy: world.policy.clone(),
            sandbox: world.sandbox,
        }
    }

    /// Installs this frame's saved local-scheme document again, without a
    /// navigation request: a history traversal reuses the entry's document
    /// state (origin, policy container, referrer) exactly as it was created
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#apply-the-history-step>).
    pub(crate) fn restore_local_document(
        &mut self,
        state: &LocalDocumentState,
        srcdoc: Option<&str>,
    ) {
        // A traversed-to entry is always a document the frame navigated to,
        // never the blank created at insertion.
        self.initial_blank = false;
        self.world.borrow_mut().install_sandbox(state.sandbox);
        if let Some(markup) = srcdoc {
            let base = state.base_url.as_ref().unwrap_or(&state.url);
            let cookie_url = state
                .cookie_url
                .clone()
                .unwrap_or_else(|| state.url.clone());
            self.load_frame_srcdoc(
                base,
                &cookie_url,
                &state.referrer_url,
                state.origin.clone(),
                &state.policy,
                markup,
            );
            return;
        }
        let cookie_url = state
            .cookie_url
            .clone()
            .unwrap_or_else(|| state.url.clone());
        self.load_about_blank(
            state.base_url.as_ref(),
            &cookie_url,
            Some(state.origin.clone()),
        );
        self.set_document_referrer(state.referrer.clone());
        self.world.borrow_mut().policy = state.policy.clone();
    }

    /// Takes the `iframe` containers that left this document. Insertion needs
    /// no event: the frame host materializes a new container when it meets it
    /// in tree order with scripts. This also drops the document's own state for
    /// every `<img>` that left, because only this document's realm owns it.
    pub(crate) fn take_removed_containers(&mut self) -> Vec<crate::dom::tree::NodeId> {
        let (connected, connected_images) = self
            .world
            .borrow()
            .with_main_document(|parsed| {
                let base = &parsed.document.base;
                let document = parsed.id;
                let mut containers = Vec::new();
                let mut images = Vec::new();
                let mut stack = vec![base.root_node().id];
                while let Some(id) = stack.pop() {
                    if crate::dom::tree::is_connected(base, id) {
                        if crate::dom::tree::is_child_navigable_container(base, id) {
                            containers.push(crate::dom::tree::NodeId { document, node: id });
                        }
                        if crate::dom::tree::is_html_element(base, id, "img") {
                            images.push(crate::dom::tree::NodeId { document, node: id });
                        }
                    }
                    if let Some(node) = base.get_node(id) {
                        stack.extend(node.children.iter().rev().copied());
                    }
                }
                (containers, images)
            })
            .unwrap_or_default();
        let shared = self.shared.borrow();
        let known: std::collections::HashSet<crate::dom::tree::NodeId> = shared
            .tree
            .children(self.frame)
            .iter()
            .filter_map(|child| shared.tree.container(*child))
            .collect();
        drop(shared);
        let connected_set: std::collections::HashSet<crate::dom::tree::NodeId> =
            connected.iter().copied().collect();
        // Insertion is the frame-host materialization (`Document`'s pending
        // frames) / `materialize_iframe` in tree order with scripts. Scanning
        // connected iframes here would start parser `src` loads before
        // preceding classic scripts run.
        let removed = known
            .iter()
            .filter(|container| !connected_set.contains(container))
            .copied()
            .collect();
        let connected_image_set: std::collections::HashSet<crate::dom::tree::NodeId> =
            connected_images.iter().copied().collect();
        let previous = std::mem::replace(&mut self.connected_images, connected_image_set);
        let disconnected: Vec<crate::dom::tree::NodeId> = previous
            .into_iter()
            .filter(|element| !self.connected_images.contains(element))
            .collect();
        // The connected set above already replaced the old one; what is left is
        // this document's per-element state, which no engine-side reader shares.
        for element in disconnected {
            self.disconnect_image(element);
        }
        removed
    }

    pub(crate) fn take_frame_navigations(&mut self) -> Vec<FrameNavigation> {
        self.world.borrow_mut().take_frame_navigations()
    }

    /// Takes the traversals this frame's script requested.
    pub(crate) fn take_frame_traversals(&mut self) -> Vec<i32> {
        std::mem::take(&mut self.frame_traversals)
    }

    /// Takes the same-document history updates this document applied, in the
    /// order its scripts made them.
    pub(crate) fn take_history_updates(&mut self) -> Vec<HistoryUpdate> {
        std::mem::take(&mut self.history_updates)
    }

    pub(crate) fn take_stream_publish(&mut self) -> Option<bool> {
        self.stream_publish.take()
    }

    pub(crate) fn fire_node_load(&mut self, id: crate::dom::tree::NodeId) {
        self.fire_js(|js| js.fire_node_load(id));
        self.adopt_js_work();
    }

    /// Drops a disconnected `<img>`'s creator stack entry. Its request still
    /// settles through Blitz, which fires `load` or `error` on the element and
    /// owns every piece of request state
    /// (<https://html.spec.whatwg.org/multipage/images.html#update-the-image-data>).
    fn disconnect_image(&mut self, element: crate::dom::tree::NodeId) {
        // A re-insertion starts a new load: drop the old trigger so the move
        // attributes to itself rather than to a stale `src` set.
        self.world
            .borrow_mut()
            .state
            .creator_stacks
            .remove(&element);
    }

    /// Applies CSP `upgrade-insecure-requests` to the request URL of one queued
    /// dial.
    ///
    /// The rewrite happens at the top of main fetch, before the request leaves
    /// the renderer: an `http` request URL becomes `https`. A nested navigation
    /// always upgrades because the embedder's security state depends on it; a
    /// top-level navigation upgrades only a host in the document's upgrade set,
    /// which is the protected resource's own host and port. Every other request
    /// upgrades unconditionally
    /// (<https://w3c.github.io/webappsec-upgrade-insecure-requests/#upgrade-request>).
    fn queue_dial(&mut self, mut dial: QueuedDial) {
        // Every request a document starts belongs to its top-level site's
        // cookie partition, which is what a `Partitioned` cookie is keyed on
        // (<https://github.com/privacycg/CHIPS#computing-the-cookie-partition-key>).
        dial.req.partition_key = self.world.borrow().partition_key();
        if self.world.borrow().policy.csp.upgrades_insecure_requests() {
            let nested = matches!(
                dial.context,
                DialContext::FrameLoad {
                    embedder: Some(_),
                    ..
                }
            );
            let upgrades =
                if dial.req.policy.kind() == crate::protocol::DialKind::FrameLoad && !nested {
                    // A top-level navigation must target the protected resource's
                    // own host and port, so a policy cannot upgrade a link to a
                    // third party
                    // (<https://w3c.github.io/webappsec-upgrade-insecure-requests/#upgrade-request>).
                    Self::same_host_port(&dial.req.url, &self.url)
                } else {
                    true
                };
            if upgrades {
                Self::upgrade_scheme(&mut dial.req.url);
            }
        }
        // A navigation request advertises the client's upgrade capability so a
        // server can feature-detect it
        // (<https://w3c.github.io/webappsec-upgrade-insecure-requests/#upgrade-insecure-requests-http-request-header-field>).
        if dial.req.policy.kind() == crate::protocol::DialKind::FrameLoad
            && !dial
                .req
                .headers
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case("upgrade-insecure-requests"))
        {
            dial.req
                .headers
                .push(("Upgrade-Insecure-Requests".to_owned(), "1".to_owned()));
        }
        self.dials.queue(dial, &self.load_delay);
    }

    /// Whether `left` and `right` name the same host and port
    /// (<https://url.spec.whatwg.org/#concept-url-host>).
    fn same_host_port(left: &Url, right: &Url) -> bool {
        left.host_str() == right.host_str()
            && left.port_or_known_default() == right.port_or_known_default()
    }

    /// Rewrites one URL's scheme from `http` to `https`, leaving every other
    /// scheme alone.
    fn upgrade_scheme(url: &mut Url) {
        if url.scheme() == "http" && url.set_scheme("https").is_ok() {}
    }

    /// Starts one frame-load dial. `req` is the wire request, with a
    /// [`crate::protocol::RequestPolicy::FrameLoad`] policy, the method, body,
    /// `Content-Type`, referrer, and the embedder's `frame-src` redirect gate
    /// already on it; `ancestors` are the ancestor URLs the child response's
    /// `frame-ancestors` is checked against at commit. It forces the response
    /// body to be read and cancels an earlier navigation: the newest load owns
    /// the frame.
    pub(crate) fn navigate(
        &mut self,
        embedder: Option<FrameId>,
        mut req: crate::protocol::DialRequest,
        ancestors: Vec<Url>,
        sandbox: crate::protocol::SandboxFlags,
    ) {
        req.delivery = crate::protocol::BodyDelivery::Buffer;
        self.dials.cancel_frame_loads();
        self.initial_blank = false;
        self.queue_dial(QueuedDial {
            context: DialContext::FrameLoad { embedder, sandbox },
            req,
            ancestors,
            integrity: None,
        });
    }

    /// Cancels an in-flight frame load because a history traversal now owns
    /// the frame.
    ///
    /// The cancelled dial's completion is dropped by the ledger, so it cannot
    /// commit a document the browser no longer names
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#aborting-navigation>).
    pub(crate) fn cancel_frame_navigation(&mut self) {
        self.dials.cancel_frame_loads();
    }

    /// Records the creator's secure-context bit for the navigation this
    /// document is about to commit. `None` when the browser started the
    /// navigation, which has no creator
    /// (<https://html.spec.whatwg.org/multipage/webappapis.html#secure-contexts>).
    pub(crate) fn set_creator_secure_context(&mut self, creator: Option<bool>) {
        self.creator_secure_context = creator;
    }

    /// Records the creator's cross-origin isolation for the navigation this
    /// document is about to commit. A nested document needs its embedder's
    /// capability in addition to its own `Cross-Origin-Embedder-Policy`
    /// (<https://html.spec.whatwg.org/multipage/webappapis.html#concept-settings-object-cross-origin-isolated-capability>).
    pub(crate) fn set_creator_cross_origin_isolated(&mut self, creator: Option<bool>) {
        self.creator_cross_origin_isolated = creator;
    }

    /// Gives a blank document the URL `about:blank` and the creator's base
    /// URL for relative resolution.
    fn apply_blank_url(
        &mut self,
        inherited_url: Option<&Url>,
        cookie_url: &Url,
        inherited_origin: Option<url::Origin>,
    ) {
        let Some(base) = inherited_url else {
            return;
        };
        let blank = about_blank();
        let origin = self.sandboxed_origin(inherited_origin);
        self.commit_document_url(blank, origin);
        let mut world = self.world.borrow_mut();
        world.base_url = Some(base.clone());
        // The blank has no URL of its own that cookies can apply to, so it
        // borrows the creator's cookie URL for as long as it lives
        // (<https://html.spec.whatwg.org/multipage/dom.html#dom-document-cookie>).
        world.inherited_cookie_url = Some(cookie_url.clone());
    }

    /// Installs an `about:blank` document, with `inherited_url` as its
    /// fallback base URL and `inherited_origin` as the captured origin. The
    /// document URL stays `about:blank`: it is what `location.href` reports,
    /// while relative URLs still resolve against the creator's base URL
    /// (<https://html.spec.whatwg.org/multipage/iframe-embed-object.html#process-the-iframe-attributes>,
    /// <https://html.spec.whatwg.org/multipage/browsers.html#determining-the-origin>,
    /// <https://html.spec.whatwg.org/multipage/urls-and-fetching.html#fallback-base-url>).
    ///
    /// This installs a blank for either a frame's first document or a
    /// navigation to `about:blank`, so it leaves [`Self::initial_blank`]
    /// alone: [`Self::load_about_blank_tree`] marks the first document, and a
    /// navigation to `about:blank` is a session history entry like any other
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#session-history-entry>).
    pub(crate) fn load_about_blank(
        &mut self,
        inherited_url: Option<&Url>,
        cookie_url: &Url,
        inherited_origin: Option<url::Origin>,
    ) {
        self.apply_blank_url(inherited_url, cookie_url, inherited_origin);
        // A blank document's referrer is its creator's URL, unredacted by any
        // policy: the initial `about:blank` was never fetched
        // (<https://html.spec.whatwg.org/multipage/browsers.html#creating-a-new-browsing-context>).
        self.world.borrow_mut().document_referrer = inherited_url.cloned();
        // No HTTP response stands behind a blank document.
        self.http_status = 0;
        self.load_html("");
        self.ensure_js_ok();
    }

    /// Loads the frame's `about:srcdoc` document: `markup` parsed as HTML,
    /// the document URL `about:srcdoc`, and the creator's base URL, origin,
    /// referrer policy, and CSP list inherited. Relative URLs inside an
    /// `iframe srcdoc document` resolve against the creator's base URL,
    /// which the document's own URL cannot supply
    /// (<https://html.spec.whatwg.org/multipage/iframe-embed-object.html#attr-iframe-srcdoc>,
    /// <https://html.spec.whatwg.org/multipage/urls-and-fetching.html#fallback-base-url>).
    pub(crate) fn load_frame_srcdoc(
        &mut self,
        base: &Url,
        cookie_url: &Url,
        referrer_url: &Url,
        origin: url::Origin,
        policy: &PolicyContainer,
        markup: &str,
    ) {
        let url = Url::parse("about:srcdoc").expect("about:srcdoc parses");
        self.http_status = 0;
        let origin = self.sandboxed_origin(Some(origin));
        self.commit_document_url(url, origin);
        {
            let mut world = self.world.borrow_mut();
            world.base_url = Some(base.clone());
            // A `srcdoc` document's own `about:srcdoc` URL cannot carry
            // cookies; its cookies belong to its creator
            // (<https://html.spec.whatwg.org/multipage/dom.html#dom-document-cookie>).
            world.inherited_cookie_url = Some(cookie_url.clone());
            world.referrer_url = referrer_url.clone();
            // A `srcdoc` document's referrer is its creator's URL, like the
            // initial `about:blank` it replaces
            // (<https://html.spec.whatwg.org/multipage/browsers.html#creating-a-new-browsing-context>).
            world.document_referrer = Some(referrer_url.clone());
        }
        self.inherited_policy = Some(policy.clone());
        self.initial_blank = false;
        self.load_html(markup);
    }

    /// Installs the initial `about:blank` tree for a newly inserted `iframe`
    /// or `window.open()` document
    /// (<https://html.spec.whatwg.org/multipage/iframe-embed-object.html#process-the-iframe-attributes>).
    ///
    /// The tree exists before the caller returns so `contentDocument` is
    /// available in the same task. The realm is deferred: creating a second
    /// `QuickJS` realm under the opener's live `Ctx` loses the opener's load
    /// event, so the engine creates the auxiliary realm when it adopts the
    /// frame (`Engine::adopt_pending_frames`).
    pub(crate) fn load_about_blank_tree(
        &mut self,
        inherited_url: Option<&Url>,
        cookie_url: &Url,
        inherited_origin: Option<url::Origin>,
    ) {
        self.apply_blank_url(inherited_url, cookie_url, inherited_origin);
        // The initial blank reports its creator's URL as its referrer
        // (<https://html.spec.whatwg.org/multipage/browsers.html#creating-a-new-browsing-context>).
        self.world.borrow_mut().document_referrer = inherited_url.cloned();
        self.defer_js = true;
        self.load_html("");
        self.defer_js = false;
        self.initial_blank = true;
    }

    /// Whether the frame is still on the document created at insertion.
    #[must_use]
    pub(crate) fn is_initial_blank(&self) -> bool {
        self.initial_blank
    }

    /// Runs one `javascript:` frame URL's script in the frame's current realm,
    /// replacing the document only when the result is a string. A void
    /// result leaves the document (and its realm) untouched; the script
    /// observes the page's own state, not a fresh blank realm. The result
    /// document keeps the target's URL
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#javascript-protocol>).
    pub(crate) fn load_javascript_frame(&mut self, script: &str, blocked: bool) {
        if self.js.is_none() {
            self.ensure_js_ok();
        }
        self.eval_frame_script(script, blocked);
    }

    /// The violations a `javascript:` navigation URL produces under this
    /// document's policy. The initiator, never the target frame, owns the
    /// check and the report
    /// (<https://w3c.github.io/webappsec-csp/#should-block-navigation-request>).
    ///
    /// The caller fires them, because the violation names the navigable's
    /// container element in this document
    /// (<https://w3c.github.io/webappsec-csp/#report-violation>).
    #[must_use]
    pub(crate) fn javascript_url_violations(&self, source: &str) -> Vec<crate::csp::Violation> {
        let world = self.world.borrow();
        Self::navigation_script_violations(&world.policy.csp, &self.url, source)
    }

    /// Runs one `javascript:` URL script in this frame's realm once the
    /// initiator's policy allowed it. A string result replaces the document
    /// with that string parsed as HTML
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#javascript-protocol>).
    pub(crate) fn eval_frame_script(&mut self, script: &str, blocked: bool) {
        // A `javascript:` navigation does nothing while scripting is disabled
        // (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#javascript-protocol>).
        if script.is_empty() || self.world.borrow().sandbox.scripts_disabled() || blocked {
            return;
        }
        match self.eval(script) {
            Ok(result) if !result.is_empty() => self.load_html(&result),
            Ok(_) => {}
            Err(_) => self.record_event(RendererEvent::ScriptFailed),
        }
    }

    /// The origin a document this frame creates gets: a new opaque origin when
    /// the frame is sandboxed without `allow-same-origin`. Same-document URL
    /// updates keep the origin they already have, so they bypass this
    /// (<https://html.spec.whatwg.org/multipage/browsers.html#sandboxed-origin-browsing-context-flag>).
    fn sandboxed_origin(&self, origin: Option<url::Origin>) -> Option<url::Origin> {
        if self.world.borrow().sandbox.origin_is_opaque() {
            Some(url::Origin::new_opaque())
        } else {
            origin
        }
    }

    /// Writes the document URL and its captured origin together.
    ///
    /// [`Self::url`] and [`World::document_url`] are mirrors, and origin
    /// checks read the captured origin rather than the URL's serialization.
    /// A navigation that writes one and not the other leaves the document
    /// reporting a stale URL or origin to the next frame.
    fn set_document_url(&mut self, url: Url, origin: Option<url::Origin>) {
        let mut world = self.world.borrow_mut();
        match origin {
            Some(origin) => world.set_url_with_origin(url.clone(), origin),
            None => world.set_url(url.clone()),
        }
        drop(world);
        self.url = url;
    }

    /// Commits a new document at `url`: writes the URL and captured origin,
    /// and applies the target-name reset a navigation commit performs. A
    /// same-document history update keeps the secure-context bit its document
    /// already has
    /// (<https://html.spec.whatwg.org/multipage/webappapis.html#secure-contexts>).
    ///
    /// The reset has two owners by construction. A commit the browser drives
    /// arrives as a mount, and the browser resolves the name against the
    /// document it is replacing, which this renderer may never have hosted; a
    /// commit this renderer drives has no browser step at all, so this is the
    /// only place that can clear the name before the new document's scripts run
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName>).
    /// A frame's first document in this renderer is always the former, which
    /// is what [`Self::committed`] skips; a later mount reports its resolved
    /// name through [`Self::install_tab_state`], which is what
    /// [`Self::name_resolved`] skips.
    fn commit_document_url(&mut self, url: Url, origin: Option<url::Origin>) {
        let new_origin = origin.clone().unwrap_or_else(|| url.origin());
        let frame = self.frame;
        let previous = self.world.borrow().origin.clone();
        let first_document = !self.committed;
        let resolved_by_browser = std::mem::take(&mut self.name_resolved);
        self.committed = true;
        let creation_url = url.clone();
        self.set_document_url(url, origin);
        // A new document commits a new creation URL; a same-document history
        // update keeps the one it had
        // (<https://html.spec.whatwg.org/multipage/document-lifecycle.html#concept-document-creation-url>).
        self.world.borrow_mut().creation_url = creation_url;
        if first_document || resolved_by_browser {
            return;
        }
        // The reset compares the new document's origin with the *active*
        // document's, so the old origin is captured before this write replaces
        // it
        // (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName>).
        let cleared = self
            .world
            .borrow()
            .shared()
            .borrow_mut()
            .tree
            .reset_name_on_cross_origin_navigation(frame, previous == new_origin);
        if cleared && frame == FrameId::MAIN {
            // The tab's name is what `window.open` searches across tabs and
            // what a later mount carries, so the browser has to hear about a
            // reset it did not perform itself. The call is synchronous, so no
            // world borrow may be live while it runs.
            let services = self.world.borrow().runtime.services.clone();
            services.set_window_name(crate::protocol::NameChange::Reset);
        }
    }

    /// Queues the same-document fragment navigation a link, a form, or a
    /// `Location` assignment asked for. The URL and the realm's history mirror
    /// move now, and the document delivers `hashchange` in its next turn
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#navigate-to-a-fragment>).
    pub(crate) fn queue_fragment_navigation(&self, url: Url) {
        let mut world = self.world.borrow_mut();
        let old_url = world.document_url.as_str().to_owned();
        // "auto" history handling replaces the active entry when the
        // navigation names the URL it started from
        // (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#navigate-to-a-fragment>).
        let handling = if world.document_url == url {
            crate::protocol::HistoryHandling::Replace
        } else {
            crate::protocol::HistoryHandling::Push
        };
        world.document_url = url.clone();
        // A fragment navigation never carries classic history state.
        world.history.apply("null".to_owned(), handling);
        world
            .outbox
            .borrow_mut()
            .push_back(crate::js::world::RealmRequest::FragmentNavigation {
                old_url,
                url,
                handling,
            });
    }

    /// Points `:target` at the element the document URL's fragment names, or
    /// clears it when the URL has none
    /// (<https://drafts.csswg.org/selectors-4/#the-target-pseudo>).
    fn update_url_target(&mut self) {
        let fragment = self.url.fragment().map(|fragment| {
            percent_encoding::percent_decode_str(fragment)
                .decode_utf8_lossy()
                .into_owned()
        });
        let fragment = fragment.as_deref();
        let world = self.world.borrow();
        let Some(id) = self.document_node() else {
            return;
        };
        let Some(mut parsed) = world.document_mut(id) else {
            return;
        };
        parsed.document.base.set_url_target(fragment);
    }

    fn apply_document_url_same_origin(&mut self, url: &str) {
        if let Ok(url) = Url::parse(url) {
            let origin = self.world.borrow().origin.clone();
            self.set_document_url(url, Some(origin));
        }
    }

    /// Sets the realm-visible session history of a new document, before its
    /// realm runs so the `history` bindings read the fresh entry.
    pub(crate) fn set_history(&mut self, history: &crate::protocol::HistorySnapshot) {
        self.world.borrow_mut().history = history.clone();
    }

    /// Replaces the mirror and, when the document already has a realm, the
    /// live `history` bindings it was initialized from.
    ///
    /// The browser owns the joint entry count, so a change made by another
    /// frame has to reach this realm without replacing its document
    /// (<https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-history-length>).
    pub(crate) fn update_history_mirror(&mut self, history: &crate::protocol::HistorySnapshot) {
        self.set_history(history);
        if let Some(js) = self.js.as_ref() {
            let _ = js.restore_history(history.state.as_deref(), history.length);
        }
    }

    /// Restores a same-document session history entry while preserving the realm.
    /// <https://html.spec.whatwg.org/multipage/browsing-the-web.html#update-document-for-history-step-application>
    pub(crate) fn traverse_history(
        &mut self,
        url: &str,
        history: &crate::protocol::HistorySnapshot,
    ) -> Result<(), TabError> {
        let url = Url::parse(url).map_err(|_| TabError::InvalidUrl {
            spec: url.to_owned(),
        })?;
        if !self.world.borrow().url_matches_origin(&url) {
            return Err(TabError::InvalidUrl { spec: url.into() });
        }
        let previous_url = self.url.clone();
        let previous_history = self.world.borrow().history.clone();
        self.apply_document_url_same_origin(url.as_str());
        self.world.borrow_mut().history = history.clone();
        let restored_state = self
            .js
            .as_ref()
            .map(|js| js.restore_history(history.state.as_deref(), history.length))
            .transpose();
        match restored_state {
            Ok(Some(state)) => self.fire_js(|js| js.fire_popstate(&state)),
            Ok(None) => {}
            Err(error) => {
                self.apply_document_url_same_origin(previous_url.as_str());
                self.world.borrow_mut().history = previous_history;
                return Err(TabError::from(error));
            }
        }
        Ok(())
    }

    /// Queues one posted window message as a task on this frame's task source
    /// (<https://html.spec.whatwg.org/multipage/web-messaging.html#posted-message-task-source>).
    pub(crate) fn push_window_message(&mut self, message: WindowMessage) {
        self.tasks.push_back(Task::WindowMessage(message));
    }

    /// Whether `id` still names this frame's current document instance.
    pub(crate) fn matches_document(&self, id: DocumentId) -> bool {
        self.shared.borrow().document_id(self.frame) == Some(id)
    }

    /// Queues one `storage` event as a task on this frame's task source
    /// (<https://html.spec.whatwg.org/multipage/webstorage.html#concept-storage-broadcast>).
    pub(crate) fn push_storage_event(&mut self, event: crate::storage::PendingStorageEvent) {
        self.tasks.push_back(Task::StorageEvent(event));
    }

    /// Queues one cross-tab `message` as a task on this frame's task source
    /// (<https://html.spec.whatwg.org/multipage/web-messaging.html#posted-message-task-source>).
    pub(crate) fn push_remote_message(&mut self, message: crate::protocol::PostedMessage) {
        self.tasks.push_back(Task::RemoteMessage(message));
    }

    /// Queues one `BroadcastChannel` message on this frame's task source
    /// (<https://html.spec.whatwg.org/multipage/web-messaging.html#broadcasting-to-other-browsing-contexts>).
    pub(crate) fn push_broadcast_message(
        &mut self,
        origin: String,
        name: String,
        payload: String,
        source: Option<u64>,
    ) {
        self.tasks.push_back(Task::BroadcastMessage {
            origin,
            name,
            payload,
            source,
        });
    }

    /// Queues one channel message as a task on this frame's task source.
    pub(crate) fn push_port_message(&mut self, endpoint: u64, payload: String, ports: Vec<u64>) {
        self.tasks.push_back(Task::PortMessage {
            endpoint,
            payload,
            ports,
        });
    }

    /// Queues a `close` event for one channel endpoint.
    pub(crate) fn push_port_closed(&mut self, endpoint: u64) {
        self.tasks.push_back(Task::PortClosed { endpoint });
    }

    /// Queues one message a dedicated worker posted to this document's
    /// `Worker` object.
    pub(crate) fn push_worker_message(
        &mut self,
        worker: crate::worker::WorkerId,
        payload: String,
        ports: Vec<u64>,
    ) {
        self.tasks.push_back(Task::WorkerMessage {
            worker,
            payload,
            ports,
        });
    }

    /// Queues one worker error for the `Worker` object's `error` handler.
    pub(crate) fn push_worker_error(&mut self, error: crate::worker::WorkerError) {
        self.tasks.push_back(Task::WorkerError(error));
    }

    /// Queues the plain `error` event a worker that never started fires at its
    /// `Worker` object.
    pub(crate) fn push_worker_load_failed(&mut self, worker: crate::worker::WorkerId) {
        self.tasks.push_back(Task::WorkerLoadFailed(worker));
    }

    /// Queues the `select` task of a changed text selection.
    pub(crate) fn push_selection_changed(&mut self, id: crate::dom::tree::NodeId) {
        self.tasks.push_back(Task::SelectionChanged(id));
    }

    /// Queues `securitypolicyviolation` events a worker's policy raised,
    /// reported at this document because a worker global has no page-side
    /// event target (<https://w3c.github.io/webappsec-csp/#report-violation>).
    pub(crate) fn push_csp_violations(&mut self, violations: Vec<crate::csp::Violation>) {
        self.tasks.push_back(Task::CspViolation(violations));
    }

    /// Dispatches one cross-tab `message` event at this frame's window.
    fn deliver_remote_message(&mut self, message: &crate::protocol::PostedMessage) {
        // The intended target origin is checked when the task runs, against the
        // document active then
        // (<https://html.spec.whatwg.org/multipage/web-messaging.html#window-post-message-steps>).
        if !message.target_origin.admits(&self.world.borrow().origin) {
            return;
        }
        if !self.ensure_js_ok() {
            return;
        }
        let origin = message.origin.to_string();
        self.fire_js(|js| js.deliver_remote_message(&origin, &message.payload));
        self.adopt_js_work();
    }

    /// Dispatches one `BroadcastChannel` message to this frame's channels.
    fn deliver_broadcast_message(
        &mut self,
        origin: &str,
        name: &str,
        payload: &str,
        source: Option<u64>,
    ) {
        if !self.ensure_js_ok() {
            return;
        }
        self.fire_js(|js| js.deliver_broadcast_message(origin, name, payload, source));
        self.adopt_js_work();
    }

    /// Fires one `storage` event at this frame's window
    /// (<https://html.spec.whatwg.org/multipage/webstorage.html#concept-storage-broadcast>).
    fn deliver_storage_event(&mut self, event: &crate::storage::PendingStorageEvent) {
        if !self.ensure_js_ok() {
            return;
        }
        self.fire_js(|js| js.fire_storage_event(event));
        self.adopt_js_work();
    }

    fn deliver_window_message(&mut self, message: &WindowMessage) {
        // The intended target origin is checked when the task runs, not when
        // it was queued: a navigation that committed between the two must not
        // receive the sender's message. Origins compare with identity, so two
        // opaque origins never match
        // (<https://html.spec.whatwg.org/multipage/web-messaging.html#window-post-message-steps>,
        // Blink: LocalDOMWindow::DispatchMessageEventWithOriginCheck).
        if !message.target_origin.admits(&self.world.borrow().origin) {
            // The transferred endpoints arrive with no message to carry them;
            // forgetting them fires `close` at their peers instead of leaking
            // the endpoints until the next reset.
            self.shared.borrow_mut().forget_endpoints(&message.ports);
            return;
        }
        if !self.ensure_js_ok() {
            return;
        }
        let delivered = match &self.js {
            Some(js) => js.deliver_window_message(
                message.source.get(),
                &message.origin,
                &message.payload,
                &message.ports,
            ),
            None => return,
        };
        match delivered {
            Ok(true) => {}
            Ok(false) => self.fire_js(|js| {
                js.deliver_window_message_error(message.source.get(), &message.origin)
            }),
            Err(_) => self.record_event(RendererEvent::ScriptFailed),
        }
        self.adopt_js_work();
    }

    fn deliver_port_message(&mut self, endpoint: u64, payload: &str, ports: &[u64]) {
        // The port may have moved to another realm between the message being
        // queued and this task; it goes back on the port's queue, which the
        // new realm flushes when the port is enabled there
        // (<https://html.spec.whatwg.org/multipage/web-messaging.html#message-port-post-message-steps>).
        if self.endpoint_moved(endpoint) {
            let shared = self.shared.clone();
            let mut shared = shared.borrow_mut();
            shared.requeue_port_message(endpoint, payload.to_owned(), ports.to_vec());
            return;
        }
        if !self.ensure_js_ok() {
            return;
        }
        let delivered = match &self.js {
            Some(js) => js.deliver_port_message(endpoint, payload, ports),
            None => return,
        };
        match delivered {
            Ok(true) => {}
            Ok(false) => self.fire_js(|js| js.deliver_port_message_error(endpoint)),
            Err(_) => self.record_event(RendererEvent::ScriptFailed),
        }
        self.adopt_js_work();
    }

    fn deliver_port_close(&mut self, endpoint: u64) {
        if self.endpoint_moved(endpoint) {
            let shared = self.shared.clone();
            shared.borrow_mut().requeue_port_close(endpoint);
            return;
        }
        if !self.ensure_js_ok() {
            return;
        }
        self.fire_js(|js| js.deliver_port_close(endpoint));
        self.adopt_js_work();
    }

    /// Whether `endpoint` no longer belongs to this frame, either because it
    /// moved to another realm or because it is in transit.
    ///
    /// A port can move between the moment a delivery is queued and the moment
    /// its task runs; the delivery then belongs to the new owner's realm
    /// (<https://html.spec.whatwg.org/multipage/web-messaging.html#transfer-receiving-steps>).
    fn endpoint_moved(&self, endpoint: u64) -> bool {
        let shared = self.shared.borrow();
        shared.ports.owner(endpoint) != Some(self.frame)
    }

    /// The document URL: the relative-URL base and the same-document
    /// comparison every loader makes.
    #[must_use]
    pub(crate) fn url(&self) -> &Url {
        &self.url
    }

    /// Evaluates `source` as classic script on this document's JS context.
    ///
    /// # Errors
    ///
    /// [`TabError::Script`] when the engine cannot start or the script throws.
    pub(crate) fn eval(&mut self, source: &str) -> Result<String, TabError> {
        // A frame this evaluation creates still has to be adopted, but an
        // evaluation that runs *during* parsing must not adopt the frames the
        // parser has not reached yet: their `src` loads would start before the
        // parser-blocking scripts ahead of them run
        // (<https://html.spec.whatwg.org/multipage/iframe-embed-object.html#the-iframe-element:html-element-insertion-steps>).
        // The walk adopts those in tree order, and adopts the rest when it
        // ends.
        if !self.world.borrow().parser_active {
            self.adopt_pending_frames();
        }
        self.ensure_js()?;
        let Some(js) = self.js.as_ref() else {
            return Err(TabError::Script(ScriptFailure::HostMissing));
        };
        let out = js.eval(source).map_err(TabError::from);
        self.adopt_js_work();
        out
    }

    pub(crate) fn execute_script_deadline(
        &mut self,
        source: crate::ScriptSource<&str>,
        deadline: Option<WallClock>,
    ) -> Result<ScriptValue, TabError> {
        self.ensure_js()?;
        let Some(js) = self.js.as_ref() else {
            return Err(TabError::Script(ScriptFailure::HostMissing));
        };
        let out = js
            .eval_value_deadline(source, deadline)
            .map_err(TabError::from);
        self.adopt_js_work();
        out
    }

    pub(crate) fn take_events(&mut self) -> Result<Vec<RendererEvent>, ()> {
        if std::mem::take(&mut self.events_overflowed) {
            self.events.clear();
            return Err(());
        }
        Ok(std::mem::take(&mut self.events))
    }

    /// Replaces the document from a host mount: new realm, decoded bytes,
    /// parsed to load. The host has already dialed and chosen this renderer.
    pub(crate) fn mount(&mut self, mount: &Mount) {
        self.set_history(&mount.head.history);
        self.set_document_referrer(None);
        self.install_tab_state(&mount.tab);
        self.install_response(&mount.head, &mount.body, None);
    }

    /// Seeds the browsing-context state the browser owns for this tab: its
    /// target name, whether it keeps that name across origins, and the popup
    /// sandboxing flag set every document it installs inherits.
    ///
    /// The flag set is stored in the frame tree, not only on this document:
    /// every commit of the tab's top-level context unions it again, so a
    /// navigation this renderer performs itself cannot install an unsandboxed
    /// document
    /// (<https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-name>,
    /// <https://html.spec.whatwg.org/multipage/browsers.html#popup-sandboxing-flag-set>).
    pub(crate) fn install_tab_state(&mut self, tab: &crate::protocol::TabState) {
        debug_assert_eq!(
            self.frame,
            FrameId::MAIN,
            "a tab's browsing-context state belongs to its top-level document"
        );
        // The mount carries the name the browser resolved against the document
        // it is replacing, so the commit that follows must not reset it again
        // (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName>).
        self.name_resolved = true;
        let frame = self.frame;
        {
            let world = self.world.borrow();
            let shared = world.shared();
            let mut shared = shared.borrow_mut();
            shared.tree.set_name(
                frame,
                tab.name
                    .as_ref()
                    .map_or_else(String::new, |name| name.as_str().to_owned()),
            );
            shared
                .tree
                .set_main_auxiliary_with_opener(tab.auxiliary_with_opener);
            shared.tree.set_popup_sandbox(frame, tab.popup_sandbox);
        }
        self.world.borrow_mut().install_sandbox(tab.popup_sandbox);
        self.world.borrow_mut().own_window = tab.id;
        // A tab's initial blank inherits its creator's secure-context bit.
        // Other mounts pass `None`, so the document's own URL decides.
        self.set_creator_secure_context(tab.secure_context);
    }

    /// Opens a parse on `input`, taking ownership of the input stream.
    ///
    /// `decoder` is set exactly when bytes still arrive from the carrier; a
    /// script's `document.write` feeds text directly instead. The markup only
    /// accumulates here; `finish_parse` parses it whole.
    fn start_parser(
        &mut self,
        input: &str,
        eof: bool,
        owner: ParserOwner,
        decoder: Option<dial::ResponseDecoder>,
    ) {
        self.world.borrow_mut().parser_active = true;
        self.parser_eof = eof;
        self.decoder = decoder;
        self.parser_owner = owner;
        self.response_content_type = None;
        self.response_policy = crate::csp::List::default();
        self.response_sandbox = crate::protocol::SandboxFlags::default();
        self.response_referrer_policy = None;
        self.response_secure_context = false;
        self.response_cross_origin_opener_policy =
            crate::protocol::CrossOriginOpenerPolicy::UnsafeNone;
        self.response_cross_origin_embedder_policy =
            crate::protocol::CrossOriginEmbedderPolicy::UnsafeNone;
        self.response_cross_origin_isolated = false;
        self.csp_from_response = false;
        self.sniff_header.clear();
        self.apply_mime_sniff = false;
        self.mime_guard = crate::sniff::MimeGuard::default();
        self.active_parse = Some(ActiveParse {
            buffer: input.to_owned(),
            delay: self.load_delay.guard(),
        });
        // A new parse starts undecided: without this an `about:blank` or
        // `document.open()` document inherits the previous response's
        // charset, and `document.characterSet` lies. The response decoder,
        // when one exists, reports the real charset at EOF.
        self.character_set = "UTF-8";
    }

    /// One funnel: opens the realm from a response head — new realm, URL
    /// (with an optional captured origin), status, CSP policy, MIME guard,
    /// and reporting endpoints — then opens a carrier-owned parser that
    /// consumes bytes until [`Self::end_body`].
    ///
    /// Every response-install path runs the same derivation in one order
    /// here; callers only choose when the body arrives. A URL that does not
    /// parse leaves the document on its previous URL, as a streamed response
    /// with an unusable final URL does.
    pub(crate) fn install_head(&mut self, head: &ResponseHead, origin: Option<url::Origin>) {
        self.reset_js_realm(RealmReset::Navigation);
        // A new response brings its own HTTP status: documents without one
        // (blank, data) report zero rather than a previous page's status.
        // Violation `status-code` reports it
        // (<https://w3c.github.io/webappsec-csp/#create-violation-for-global>).
        self.http_status = head.source.status();
        self.set_reporting_endpoints(head.source.reporting_endpoints());
        if let Some((width, height)) = head.viewport {
            self.viewport_size = (width, height);
        }
        // The origin variant decides both: an HTTP response carries the
        // policy its headers establish, and a local-scheme response carries
        // only what it inherited, so no field can override the other.
        let policy = head.source.policy();
        // A CSP `sandbox` directive is part of the flags the document runs
        // under, and its origin flag decides the origin this commit captures:
        // union it before the URL is installed
        // (<https://w3c.github.io/webappsec-csp/#sandbox>).
        if !policy.sandbox.is_empty() {
            let mut world = self.world.borrow_mut();
            world.sandbox = world.sandbox.union(policy.sandbox);
        }
        self.install_response_url(head, origin);
        let mut world = self.world.borrow_mut();
        // A response document resolves against its own URL; any inherited
        // `about:srcdoc` base and cookie URL stop applying.
        world.base_url = None;
        world.inherited_cookie_url = None;
        drop(world);
        // The realm is already new when this runs; this opens the parser and
        // the decoder for the response that will feed it.
        self.start_parser(
            "",
            false,
            ParserOwner::Carrier,
            // The decoder resolves the charset from the extracted MIME type,
            // not from the header value as served: a `charset` in
            // `a;charset=x, b` does not decode `b`
            // (<https://fetch.spec.whatwg.org/#concept-header-extract-mime-type>).
            Some(dial::ResponseDecoder::new(
                head.content_type
                    .as_deref()
                    .and_then(crate::sniff::extract_mime_type),
            )),
        );
        // Kept raw until `finish_parse`: the Apache `text/plain` check needs
        // the exact bytes the server sent before the extraction runs
        // (<https://mimesniff.spec.whatwg.org/#supplied-mime-type-detection-algorithm>).
        self.response_content_type.clone_from(&head.content_type);
        self.response_policy = policy.csp.clone();
        self.response_sandbox = policy.sandbox;
        self.response_referrer_policy = policy.referrer_policy;
        // A local-scheme document clones its creator's whole container, and its
        // creator's secure-context bit is the one it inherits. A document with no
        // creator has no bit to inherit, so its own URL decides
        // (<https://html.spec.whatwg.org/multipage/browsers.html#determining-navigation-params-policy-container>).
        let creator = match &head.source {
            crate::protocol::ResponseSource::Http(_) => self.creator_secure_context,
            crate::protocol::ResponseSource::Local { inherited } => inherited
                .as_ref()
                .map(|policy| policy.secure_context)
                .or(self.creator_secure_context),
        };
        self.response_secure_context = secure_context::is_secure_environment(&head.url, creator);
        // A top-level document is isolated by its own headers. A nested one
        // also needs its embedder's capability, and only the embedder policy
        // applies to it; a local-scheme document inherits the whole decision
        // with the rest of the policy container
        // (<https://html.spec.whatwg.org/multipage/webappapis.html#concept-settings-object-cross-origin-isolated-capability>).
        let creator_isolated = match &head.source {
            crate::protocol::ResponseSource::Http(_) => self.creator_cross_origin_isolated,
            crate::protocol::ResponseSource::Local { inherited } => inherited
                .as_ref()
                .map(|policy| policy.cross_origin_isolated)
                .or(self.creator_cross_origin_isolated),
        };
        let top_level = {
            let shared = self.shared.borrow();
            shared.tree.top_level(self.frame) == self.frame
        };
        self.response_cross_origin_opener_policy = policy.cross_origin_opener_policy;
        self.response_cross_origin_embedder_policy = policy.cross_origin_embedder_policy;
        self.response_cross_origin_isolated = match &head.source {
            crate::protocol::ResponseSource::Local { .. } => policy.cross_origin_isolated,
            crate::protocol::ResponseSource::Http(_) => {
                self.response_secure_context
                    && policy.cross_origin_embedder_policy.compatible()
                    && (if top_level {
                        policy.cross_origin_opener_policy
                            == crate::protocol::CrossOriginOpenerPolicy::SameOrigin
                    } else {
                        creator_isolated.unwrap_or(false)
                    })
            }
        };
        self.csp_from_response = true;
        self.apply_mime_sniff = true;
        self.mime_guard = crate::sniff::MimeGuard {
            no_sniff: head.source.no_sniff(),
            http: head.source.is_http(),
        };
        // A response `Refresh` starts the document's one declarative refresh
        // (<https://html.spec.whatwg.org/multipage/document-lifecycle.html#creating-and-initializing-a-document-object>).
        if let Some(refresh) = head.source.refresh() {
            self.start_declarative_refresh(refresh);
        }
    }

    /// Points the document at the URL the response head carries.
    ///
    /// The URL is parsed at the seam, so the document always takes this
    /// response's URL. `about:blank`, `data:`, and `blob:` URLs cannot
    /// re-derive an origin, so their captured one comes from the caller
    /// (<https://w3c.github.io/FileAPI/#blob-url-entry>,
    /// <https://html.spec.whatwg.org/multipage/browsers.html#determining-the-origin>).
    fn install_response_url(&mut self, head: &ResponseHead, origin: Option<url::Origin>) {
        self.initial_blank = false;
        let origin = self.sandboxed_origin(origin);
        self.commit_document_url(head.url.clone(), origin);
    }

    /// Installs a complete response head and body: [`Self::install_head`]
    /// plus the whole body, ending the parse at once.
    fn install_response(&mut self, head: &ResponseHead, body: &[u8], origin: Option<url::Origin>) {
        self.install_head(head, origin);
        self.write_body(body);
        self.end_body();
    }

    /// Installs a complete response into this frame's document, as the
    /// frame's own `src` load or a committed child-frame response delivers.
    pub(crate) fn install_frame_response(
        &mut self,
        head: &ResponseHead,
        body: &[u8],
        origin: Option<url::Origin>,
    ) {
        self.install_response(head, body, origin);
        // Nested HTML documents without scripts skip a realm at parse;
        // `fire_ready_frame_loads` creates one when the parent is not in the
        // encoding-label burst
        // (<https://html.spec.whatwg.org/multipage/window-object.html#the-window-object>).
        if self.js.is_none() && self.shared.borrow().tree.parent(self.frame).is_none() {
            self.ensure_js_ok();
        }
    }

    /// Installs the response's `Reporting-Endpoints` endpoints: a violated
    /// policy's `report-to` directive resolves its endpoint names against
    /// them (<https://w3c.github.io/reporting/#header>). A refused
    /// `frame-ancestors` response dispatches its violations on the fallback
    /// document against the new response's endpoints, so this is also set
    /// before the check.
    fn set_reporting_endpoints(&mut self, endpoints: Option<&str>) {
        self.reporting_endpoints =
            endpoints.map_or_default(crate::csp::parse_reporting_endpoints_value);
    }

    /// Feeds response bytes. The decoder holds them until the encoding is
    /// known, then the parser sees the markup.
    pub(crate) fn write_body(&mut self, bytes: &[u8]) {
        if self.sniff_header.len() < crate::sniff::RESOURCE_HEADER_LEN {
            let room = crate::sniff::RESOURCE_HEADER_LEN - self.sniff_header.len();
            self.sniff_header.extend(bytes.iter().copied().take(room));
        }
        let Some(decoder) = self.decoder.as_mut() else {
            return;
        };
        let text = decoder.push(bytes);
        self.write_text(&text);
    }

    /// Feeds markup that is already decoded, as a script's `document.write`
    /// provides.
    pub(crate) fn write_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if let Some(parse) = self.active_parse.as_mut() {
            parse.buffer.push_str(text);
        }
        // Known gap: without an incremental parser a write after the parse
        // finished has nowhere to go, so it is ignored instead of re-parsing.
    }

    /// Ends a body: flushes the decoder and lets the parser finish.
    pub(crate) fn end_body(&mut self) {
        if let Some(decoder) = self.decoder.take() {
            let (text, encoding) = decoder.finish();
            self.character_set = encoding;
            self.write_text(&text);
        }
        self.parser_eof = true;
        self.finish_parse();
    }

    /// Aborts a body the carrier will not finish.
    ///
    /// Without this the frame waits for bytes that never come, so its load
    /// never fires and the tab stays loading forever.
    pub(crate) fn abort_body(&mut self) {
        self.decoder = None;
        self.parser_eof = true;
        self.finish_parse();
    }

    /// A script's `document.open()`: a new realm, a parser the script owns, and
    /// any live response aborted, because the script now holds the input
    /// stream.
    ///
    /// <https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#document-open-steps>
    pub(crate) fn reopen_document(&mut self) {
        self.reset_js_realm(RealmReset::Reopen);
        // A script writes text, not bytes, and it takes the input stream away
        // from any live response: dropping the decoder ignores the rest of
        // that body instead of decoding it with the wrong charset.
        self.start_parser("", false, ParserOwner::Script, None);
    }

    /// Parses `input` into this document and starts a new JS realm.
    ///
    /// The caller already holds the whole document, so the parser starts at
    /// EOF with the markup in hand and no decoder, but the carrier owns this
    /// parser: `document.close()` must not end it.
    pub(crate) fn load_html(&mut self, input: &str) {
        self.reset_js_realm(RealmReset::Navigation);
        self.start_parser(input, true, ParserOwner::Carrier, None);
        self.finish_parse();
    }

    /// Records `document` as this realm's so wrappers resolve its owner.
    fn register_document(&self, document: u32) {
        let registry = self.world.borrow().registry();
        registry.borrow_mut().insert_document(document, &self.world);
    }

    /// Runs one realm operation and records a failed script when it throws.
    fn fire_js(
        &mut self,
        operation: impl FnOnce(&crate::js::JsRealm) -> Result<(), crate::js::JsError>,
    ) {
        let failed = self.js.as_ref().is_some_and(|js| operation(js).is_err());
        if failed {
            self.record_event(RendererEvent::ScriptFailed);
        }
    }

    /// Ensures the frame's realm exists, recording a failed script when it
    /// cannot be created.
    pub(crate) fn ensure_js_ok(&mut self) -> bool {
        if self.ensure_js().is_err() {
            self.record_event(RendererEvent::ScriptFailed);
            return false;
        }
        true
    }

    /// Runs inline scripts that were inserted from another realm while this
    /// document had none, now that its realm can exist. Returns whether any
    /// script was taken, so the engine reconciles what it did.
    pub(crate) fn run_deferred_scripts(&mut self) -> bool {
        let scripts = self.world.borrow_mut().take_deferred_scripts();
        if scripts.is_empty() {
            return false;
        }
        if !self.ensure_js_ok() {
            return true;
        }
        for id in scripts {
            let failed = self
                .js
                .as_ref()
                .is_none_or(|js| js.run_deferred_inline_script(id).is_err());
            if failed {
                self.record_event(RendererEvent::ScriptFailed);
            }
        }
        self.adopt_js_work();
        true
    }

    fn ensure_js(&mut self) -> Result<(), TabError> {
        if self.js.is_none() {
            self.js = Some(
                crate::js::JsRealm::new(
                    &self.js_runtime,
                    self.world.clone(),
                    Arc::clone(&self.stop),
                )
                .map_err(TabError::from)?,
            );
            if self.decoder.is_some() && self.active_parse.is_some() {
                // The realm exists before the new document installs; defer the
                // init scripts until `bind_realm_document`.
                self.init_scripts_deferred = true;
            } else {
                self.run_init_scripts();
            }
        }
        Ok(())
    }

    /// Runs every registered init script in the fresh realm, before any page
    /// script. A failing script is reported, not fatal.
    fn run_init_scripts(&mut self) {
        let _turn = self.js.as_ref().map(crate::js::JsRealm::enter_turn);
        let scripts: Vec<String> = self
            .world
            .borrow()
            .init_scripts
            .borrow()
            .iter()
            .map(|(_, source)| source.clone())
            .collect();
        for source in scripts {
            let failed = match self.js.as_ref() {
                Some(js) => js.eval(&source).is_err(),
                None => return,
            };
            if failed {
                self.record_event(RendererEvent::ScriptFailed);
            }
        }
    }

    /// Runs one init script in this frame's realm immediately, creating the
    /// realm when needed (`Page.addScriptToEvaluateOnNewDocument` with
    /// `runImmediately`).
    pub(crate) fn run_init_script(&mut self, source: &str) {
        if !self.ensure_js_ok() {
            return;
        }
        let failed = match self.js.as_ref() {
            Some(js) => js.eval(source).is_err(),
            None => true,
        };
        if failed {
            self.record_event(RendererEvent::ScriptFailed);
        }
    }

    fn reset_js_realm(&mut self, reason: RealmReset) {
        // A task is not runnable once its document is not fully active
        // (<https://html.spec.whatwg.org/multipage/webappapis.html#concept-task-runnable>).
        for task in std::mem::take(&mut self.tasks) {
            match task {
                Task::WindowMessage(message) => {
                    self.shared.borrow_mut().forget_endpoints(&message.ports);
                }
                Task::PortMessage { ports, .. } => {
                    self.shared.borrow_mut().forget_endpoints(&ports);
                }
                _ => {}
            }
        }
        if reason == RealmReset::Navigation {
            self.shared.borrow_mut().begin_document(self.frame);
        }
        let old = std::mem::replace(
            &mut self.dials,
            DialLedger::new(Arc::clone(&self.stop), Arc::clone(&self.wake)),
        );
        old.terminate();
        self.load_delay = LoadDelay::default();
        // A pending `load` from the replaced document must not be reported as
        // the new document's readiness.
        self.events.retain(|event| {
            !matches!(
                event,
                RendererEvent::Browser(BrowserEvent::Load | BrowserEvent::DomContentLoaded)
            )
        });
        // The old realm's ports are gone with it; the peers fire `close`
        // (<https://html.spec.whatwg.org/multipage/web-messaging.html#disentangle>).
        // A navigation also destroys this frame's own children.
        {
            let mut shared = self.shared.borrow_mut();
            shared.close_frame_ports(self.frame);
            shared.detach_child_frames(self.frame);
        }
        self.load_fired = false;
        self.will_declaratively_refresh = false;
        self.pending_refresh = None;
        if let Some(id) = self.refresh_timer.take() {
            self.timers.remove(&id);
        }
        self.refresh_target = None;
        self.pending_frame_loads.clear();
        for (_, request) in self.blitz_requests.drain() {
            Box::new(request.handler)
                .bytes(String::new(), blitz_traits::net::Bytes::from(Vec::new()));
        }
        while self.blitz_fetch_rx.try_recv().is_ok() {}
        while self.image_settle_rx.try_recv().is_ok() {}
        self.subresource_bytes.clear();
        self.style_policies.clear();
        self.deferred_image_events.clear();
        self.pending_csp_events.clear();
        self.image_load_delays.clear();
        self.connected_images.clear();
        self.timers.retain(|_, timer| timer.js_id.is_none());
        self.js_timers.clear();
        self.js = None;
        // A new realm needs the opener's or parent's pre-realm `WindowProxy`
        // writes again: the old realm is gone with the document
        // (<https://html.spec.whatwg.org/multipage/window-object.html#windowproxy-set>).
        self.world.borrow_mut().await_proxy_sets_replay();
        // The realm is gone; do not keep its context alive through the world.
        self.world.borrow_mut().realm_context = None;
        self.init_scripts_deferred = false;
        self.active_parse = None;
        self.stream_publish = None;
        self.executed_scripts.clear();
        self.parser_eof = true;
        self.world.borrow_mut().parser_active = false;
        self.world.borrow_mut().state.current_script = None;
        if reason == RealmReset::Reopen {
            self.world.borrow().clear_outbox_for_open();
        } else {
            self.world.borrow().clear_outbox();
        }
        self.parser_blocked_script = None;
        self.deferred_modules.clear();
    }

    /// `document.open()`, `document.write()`, and `document.close()` all land
    /// on the same parser feed the network path uses, so a pending classic
    /// script blocks them the same way.
    pub(crate) fn apply_document_stream(&mut self, command: DocumentStreamCommand) {
        match command {
            DocumentStreamCommand::Open => self.reopen_document(),
            // Only a parser a script opened is a script's to close; a response
            // body ends when its exchange does.
            // https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#closing-the-input-stream
            DocumentStreamCommand::Close => {
                if self.parser_owner == ParserOwner::Script {
                    self.end_body();
                }
            }
        }
    }

    fn eval_classic(
        &mut self,
        source: &str,
        element: Option<crate::dom::tree::NodeId>,
        base_line: u32,
        filename: &str,
    ) {
        // https://html.spec.whatwg.org/multipage/webappapis.html#run-a-classic-script
        let previous = self.world.borrow().state.current_script;
        self.world.borrow_mut().state.current_script = element;
        if !self.ensure_js_ok() {
            return;
        }
        self.fire_js(|js| js.eval_classic_script(source, base_line, filename));
        self.world.borrow_mut().state.current_script = previous;
        self.adopt_js_work();
    }

    /// Drains parser-driven mutation records at a microtask checkpoint
    /// (<https://html.spec.whatwg.org/multipage/webappapis.html#perform-a-microtask-checkpoint>).
    /// Parser insertions bypass the JS bindings that schedule delivery, so
    /// the document's `MutationObserver`s would otherwise not fire until the
    /// next scripted mutation.
    fn deliver_mutations(&mut self) {
        self.fire_js(crate::js::JsRealm::deliver_mutations);
    }

    /// The Blitz configuration every document of this frame shares: our net
    /// provider for subresources, a no-op shell (screenshots re-resolve
    /// explicitly), the frame's base URL, and the process-shared font
    /// context (fresh scans cost ~25ms per document).
    fn blitz_config(&self) -> blitz_dom::DocumentConfig {
        // Subresource URLs in the markup resolve against the document's base
        // URL, which for `about:srcdoc` is the creator's
        // (<https://html.spec.whatwg.org/multipage/urls-and-fetching.html#document-base-url>).
        let base_url = self.base_url();
        blitz_dom::DocumentConfig {
            net_provider: Some(std::sync::Arc::clone(&self.net_provider)
                as std::sync::Arc<dyn blitz_traits::net::NetProvider>),
            navigation_provider: Some(std::sync::Arc::new(
                blitz_traits::navigation::DummyNavigationProvider,
            )
                as std::sync::Arc<dyn blitz_traits::navigation::NavigationProvider>),
            shell_provider: Some(std::sync::Arc::clone(&self.blitz_shell)
                as std::sync::Arc<dyn blitz_traits::shell::ShellProvider>),
            base_url: Some(crate::render::blitz_base_url(&base_url)),
            font_ctx: Some(self.shared_font_ctx()),
            image_settle: Some(std::sync::Arc::clone(&self.net_provider)
                as std::sync::Arc<dyn blitz_dom::ImageSettleObserver>),
            ..blitz_dom::DocumentConfig::default()
        }
    }

    /// The process-shared font context, cloned cheaply per document.
    fn shared_font_ctx(&self) -> parley::FontContext {
        self.world.borrow().runtime.font_ctx.clone()
    }

    /// Nested HTML without scripts defers a realm: encoding-label WPT files
    /// create one iframe per label, and installing `QuickJS` in each stalls
    /// those tests. Inline event handler content attributes need a realm too
    /// (`<body onload=...>`, `<img onerror=...>` compile into handler
    /// functions), as do registered init scripts, which evaluate in every
    /// new realm. XML documents always get a realm
    /// (<https://html.spec.whatwg.org/multipage/window-object.html#the-window-object>).
    fn skip_nested_html_realm(&self, parsed: &crate::Parsed) -> bool {
        if parsed.xml_document || html_needs_realm(parsed) {
            return false;
        }
        if !self.world.borrow().init_scripts.borrow().is_empty() {
            return false;
        }
        self.shared.borrow().tree.parent(self.frame).is_some()
    }

    /// Installs `parsed` as the active document and registers it, reporting
    /// whether a realm owns it afterwards.
    fn install_parsed(&mut self, parsed: Parsed) -> bool {
        if self.js.is_none() {
            let skip_js = self.defer_js || self.skip_nested_html_realm(&parsed);
            let document = self.world.borrow_mut().replace_document(parsed);
            self.register_document(document);
            // Style resolution and scripts both read the viewport; apply the
            // frame's size before the realm evaluates anything.
            self.apply_viewport();
            self.install_policy();
            self.mark_parser_scripts();
            if skip_js {
                return true;
            }
            self.ensure_js().is_ok()
        } else {
            let document = self.world.borrow_mut().set_document(parsed);
            self.register_document(document);
            self.apply_viewport();
            self.bind_realm_document();
            self.install_policy();
            self.mark_parser_scripts();
            true
        }
    }

    /// Marks every script element of the freshly parsed tree as
    /// parser-inserted
    /// (<https://html.spec.whatwg.org/multipage/scripting.html#parser-inserted>).
    fn mark_parser_scripts(&mut self) {
        let scripts: Vec<crate::dom::tree::NodeId> = {
            let world = self.world.borrow();
            let Some(parsed) = world.main_document() else {
                return;
            };
            let base = &parsed.document.base;
            let document = parsed.id;
            let mut scripts = Vec::new();
            let mut stack = vec![base.root_node().id];
            while let Some(node) = stack.pop() {
                let Some(tree) = base.get_node(node) else {
                    continue;
                };
                stack.extend(tree.children.iter().copied());
                if crate::dom::tree::is_html_element(base, node, "script") {
                    scripts.push(crate::dom::tree::NodeId { document, node });
                }
            }
            scripts
        };
        let mut world = self.world.borrow_mut();
        for id in scripts {
            world.mark_parser_inserted(id);
        }
    }

    /// Replaces the world's policy with the response headers when this parse
    /// came from a navigation. `document.open()` leaves the flag unset.
    fn install_policy(&mut self) {
        if self.csp_from_response {
            self.world.borrow_mut().policy = PolicyContainer {
                csp: self.response_policy.clone(),
                sandbox: self.response_sandbox,
                referrer_policy: self.response_referrer_policy,
                secure_context: self.response_secure_context,
                cross_origin_opener_policy: self.response_cross_origin_opener_policy,
                cross_origin_embedder_policy: self.response_cross_origin_embedder_policy,
                cross_origin_isolated: self.response_cross_origin_isolated,
            };
            return;
        }
        if let Some(policy) = self.inherited_policy.take() {
            self.world.borrow_mut().policy = policy;
        }
    }

    /// Rebinds the realm's `document` global to the installed document and
    /// runs init scripts deferred while the response streamed, so page code
    /// always sees the new tree under the same realm.
    fn bind_realm_document(&mut self) {
        if let Some(js) = self.js.as_ref()
            && js.refresh_document().is_err()
        {
            self.record_event(RendererEvent::ScriptFailed);
        }
        if std::mem::take(&mut self.init_scripts_deferred) {
            self.run_init_scripts();
        }
    }

    /// Applies this frame's viewport size to the active Blitz document.
    fn apply_viewport(&self) {
        let (width, height) = self.viewport_size;
        self.world.borrow().viewport_size.set((width, height));
        let world = self.world.borrow();
        let Some(mut parsed) = world.main_document_mut() else {
            return;
        };
        parsed
            .document
            .base
            .set_viewport(blitz_traits::shell::Viewport::new(
                width,
                height,
                1.0,
                blitz_traits::shell::ColorScheme::Light,
            ));
    }

    /// Persists a new viewport size for this frame and re-lays out the active
    /// document at it.
    pub(crate) fn set_viewport(&mut self, width: u32, height: u32) {
        self.viewport_size = (width, height);
        self.apply_viewport();
        {
            let world = self.world.borrow();
            if let Some(mut parsed) = world.main_document_mut() {
                crate::render::resolve_until_settled(&mut parsed.document.base);
            }
        }
        // Viewport changes can flip `matchMedia` results
        // (<https://drafts.csswg.org/cssom-view/#evaluate-media-queries-and-report-changes>).
        self.fire_js(super::js::JsRealm::report_media_changes);
    }

    /// Sets the frame's persisted viewport size without touching its current
    /// document, for callers that apply it during construction.
    pub(crate) fn set_viewport_size(&mut self, size: (u32, u32)) {
        self.viewport_size = size;
        self.world.borrow().viewport_size.set(size);
    }

    /// The active document's scrollable content size in CSS pixels, at least
    /// the frame's viewport
    /// (<https://chromedevtools.github.io/devtools-protocol/tot/Page/#method-getLayoutMetrics>).
    pub(crate) fn content_size(&mut self) -> (f32, f32) {
        let (viewport_width, viewport_height) = self.viewport_size;
        #[expect(
            clippy::cast_precision_loss,
            reason = "viewport sides are at most 4096, exactly representable"
        )]
        let mut content = (viewport_width as f32, viewport_height as f32);
        let world = self.world.borrow();
        let Some(mut parsed) = world.main_document_mut() else {
            return content;
        };
        let base = &mut parsed.document.base;
        crate::render::resolve_until_settled(base);
        // The document element is the scrolling element
        // (<https://drafts.csswg.org/cssom-view/#scrolling-element>); the
        // root's first child can be a doctype, which has no layout.
        let root = base.root_element();
        content.0 = content.0.max(root.scroll_width());
        content.1 = content.1.max(root.scroll_height());
        content
    }

    /// Parses the accumulated markup as one whole document, installs it, and
    /// runs its scripts in tree order
    /// (<https://html.spec.whatwg.org/multipage/parsing.html#the-end>).
    fn finish_parse(&mut self) {
        if !self.parser_eof {
            return;
        }
        self.adopt_js_work();
        let Some(ActiveParse { buffer, delay }) = self.active_parse.take() else {
            return;
        };
        drop(delay);
        // Writes queued while the parser was active join this parse; later
        // writes have no incremental parser to feed (see `write_text`).
        if self.apply_mime_sniff {
            // The stored value is the raw header list. The Apache check needs
            // it exactly as sent; the sniff needs the extracted MIME type.
            // mimesniff names "the value of the last Content-Type header" as
            // the supplied type, but browsers extract — last value that
            // parses, `*/*` skipped — and the WPT `fetch/content-type`
            // expectations follow the browsers
            // (<https://fetch.spec.whatwg.org/#concept-header-extract-mime-type>,
            // <https://mimesniff.spec.whatwg.org/#supplied-mime-type-detection-algorithm>).
            let raw = self.response_content_type.as_deref();
            let supplied = raw.and_then(crate::sniff::extract_mime_type);
            let apache =
                self.mime_guard.http && raw.is_some_and(crate::sniff::apache_bug_content_type);
            self.response_content_type = Some(crate::sniff::sniff_mime(
                &self.sniff_header,
                supplied.as_deref(),
                self.mime_guard.no_sniff,
                apache,
            ));
        }
        // XML MIME types use the XML parser. HTML, and a script's
        // `document.open()`, do not
        // (<https://html.spec.whatwg.org/multipage/parsing.html#xml-parser>,
        // <https://mimesniff.spec.whatwg.org/#xml-mime-type>).
        let parsed = if let Some(content_type) = self
            .response_content_type
            .as_deref()
            .and_then(crate::xml::navigated_content_type)
        {
            let mut parsed =
                crate::xml::parse_navigated(&buffer, content_type, self.blitz_config());
            parsed.character_set = self.character_set;
            // The parser needs the XML kind; `contentType` reports the
            // actual essence (`application/atom+xml`, not `application/xml`).
            if let Some(header) = self.response_content_type.as_deref() {
                parsed.content_type = crate::xml::document_content_type(header);
            }
            parsed
        } else {
            // `text/plain` is not parsed as markup: the response bytes become
            // the document's text verbatim, so character references such as
            // `&#601;` stay literal. Wrapping the escaped text in `pre` keeps
            // whitespace and gives the frame a body with that text
            // (<https://mimesniff.spec.whatwg.org/#sniffing-a-mime-type>,
            // <https://html.spec.whatwg.org/multipage/rendering.html#the-pre-element>).
            let plain = self
                .response_content_type
                .as_deref()
                .and_then(crate::sniff::extract_mime_type)
                .is_some_and(|mime| {
                    mime.split(';').next().is_some_and(|essence| {
                        essence.trim_ascii().eq_ignore_ascii_case("text/plain")
                    })
                });
            let markup = if plain {
                let escaped = buffer
                    .replace('&', "&amp;")
                    .replace('<', "&lt;")
                    .replace('>', "&gt;");
                format!("<pre>\n{escaped}</pre>")
            } else {
                buffer
            };
            let mut parsed = crate::parse_html(&markup, self.blitz_config());
            parsed.character_set = self.character_set;
            // A non-XML response still records its MIME type on the
            // document. Images, style sheets, and plain text in a frame
            // parse as HTML wrapping, but `contentType` is the type
            // (<https://html.spec.whatwg.org/multipage/nav-history-apis.html#concept-document-content-type>).
            if let Some(header) = self.response_content_type.as_deref() {
                parsed.content_type = crate::xml::document_content_type(header);
            }
            parsed
        };
        if !self.install_parsed(parsed) {
            // Nothing will resume the walk, so the parser is no longer
            // active: a later evaluation must adopt the frames it creates.
            self.world.borrow_mut().parser_active = false;
            self.record_event(RendererEvent::ScriptFailed);
            return;
        }
        // Microtask checkpoint before scripts run; parser mutations queued
        // since the last script deliver now. Parser iframes are adopted in
        // tree order with scripts, not all at once before the first fetch.
        self.deliver_mutations();
        self.resume_scripts();
    }

    /// Runs the inline check for every handler content attribute the parser
    /// inserted. HTML checks the attribute when it is compiled, and the
    /// one-shot parser never compiles them, so the walk runs once the parse
    /// ends: by then the document's own scripts installed their listeners,
    /// and no handler has been invoked yet
    /// (<https://w3c.github.io/webappsec-csp/#should-block-inline>,
    /// <https://html.spec.whatwg.org/multipage/webappapis.html#event-handler-content-attributes>).
    fn check_parser_handler_attributes(&mut self) {
        let handlers: Vec<(crate::dom::tree::NodeId, String)> = {
            let world = self.world.borrow();
            let Some(parsed) = world.main_document() else {
                return;
            };
            let base = &parsed.document.base;
            let document = parsed.id;
            let mut handlers = Vec::new();
            let mut stack = vec![base.root_node().id];
            while let Some(node) = stack.pop() {
                let Some(tree) = base.get_node(node) else {
                    continue;
                };
                stack.extend(tree.children.iter().copied());
                let Some(element) = tree.data.downcast_element() else {
                    continue;
                };
                if !crate::dom::tree::is_connected(base, node) {
                    continue;
                }
                let id = crate::dom::tree::NodeId { document, node };
                for attribute in element.attrs.iter() {
                    if !attribute.name.ns.is_empty() {
                        continue;
                    }
                    let name = attribute.name.local.as_str();
                    let Some(typ) = name.strip_prefix("on") else {
                        continue;
                    };
                    if typ.is_empty()
                        || !typ.chars().all(|character| {
                            character.is_ascii_lowercase() || character.is_ascii_digit()
                        })
                        || attribute.value.trim().is_empty()
                    {
                        continue;
                    }
                    handlers.push((id, attribute.value.clone()));
                }
            }
            handlers
        };
        for (id, source) in handlers {
            let violations =
                crate::js::handler_attribute_violations(&self.world.borrow(), id, &source);
            if violations.is_empty() {
                continue;
            }
            let repeated = self
                .world
                .borrow_mut()
                .state
                .reported_handlers
                .insert((id, source.clone()), source.clone())
                .is_some_and(|previous| previous == source);
            if repeated {
                continue;
            }
            self.fire_csp_violations(id, &violations);
        }
    }

    /// Continues the document after a parse settled or a parser-blocking
    /// fetch completed: runs the remaining scripts, then the end-of-parse
    /// steps unless another fetch blocks the walk.
    fn resume_scripts(&mut self) {
        let _turn = self.js.as_ref().map(crate::js::JsRealm::enter_turn);
        if self.run_scripts() {
            return;
        }
        self.world.borrow_mut().parser_active = false;
        // A document loaded with a fragment has its target element only now
        // that the tree is complete
        // (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#scroll-to-fragid>).
        self.update_url_target();
        self.check_parser_handler_attributes();
        // Deliver parser mutations before the document's events.
        self.deliver_mutations();
        self.fire_document_end();
        let _ = self.check_load();
    }

    /// Appends one in-head `meta` policy the first time it is seen
    /// (<https://w3c.github.io/webappsec-csp/#meta-element>).
    fn apply_meta_csp(&mut self, id: crate::dom::tree::NodeId) {
        self.world.borrow_mut().apply_meta_policy(id);
    }

    /// A parser-inserted `<meta name=referrer>` sets the document's policy,
    /// mirroring the JS insertion path
    /// (<https://html.spec.whatwg.org/multipage/semantics.html#meta-referrer>).
    fn apply_meta_referrer(&mut self, id: crate::dom::tree::NodeId) {
        let policy = self.world.borrow().document(id).and_then(|parsed| {
            let base = &parsed.document.base;
            let name = crate::dom::tree::attr(base, id.node, "name").unwrap_or_default();
            if !name.eq_ignore_ascii_case("referrer") {
                return None;
            }
            let content = crate::dom::tree::attr(base, id.node, "content").unwrap_or_default();
            net::ReferrerPolicy::from_meta(content)
        });
        if let Some(policy) = policy {
            self.world.borrow_mut().policy.referrer_policy = Some(policy);
        }
    }

    pub(in crate::document) fn inline_script_violations(
        &self,
        source: &str,
        id: crate::dom::tree::NodeId,
    ) -> Vec<crate::csp::Violation> {
        let world = self.world.borrow();
        let slot = world.state.nonce_slots.get(&id).map(String::as_str);
        let parser_inserted = world.is_parser_inserted(id);
        let (nonce, nonceable) = world.document(id).map_or_else(
            || (String::new(), false),
            |parsed| crate::csp::element_nonce(&parsed.document.base, id.node, slot),
        );
        let mut violations = world.policy.csp.check_inline(
            &self.url,
            &crate::csp::InlineScript {
                source,
                nonce: &nonce,
                nonceable,
                parser_inserted,
                kind: crate::csp::InlineKind::Script,
            },
        );
        for violation in &mut violations {
            // An inline check names its element, so the event fires there
            // while that element is connected to this document
            // (<https://w3c.github.io/webappsec-csp/#report-violation>).
            violation.element = Some(id);
        }
        violations
    }

    fn navigation_script_violations(
        policy: &crate::csp::List,
        document_url: &Url,
        source: &str,
    ) -> Vec<crate::csp::Violation> {
        policy.check_inline(
            document_url,
            &crate::csp::InlineScript {
                source,
                nonce: "",
                nonceable: false,
                parser_inserted: false,
                kind: crate::csp::InlineKind::Navigation,
            },
        )
    }

    pub(in crate::document) fn script_url_violations(
        &self,
        url: &Url,
        id: crate::dom::tree::NodeId,
    ) -> Vec<crate::csp::Violation> {
        let world = self.world.borrow();
        let slot = world.state.nonce_slots.get(&id).map(String::as_str);
        let parser_inserted = world.is_parser_inserted(id);
        let (nonce, integrity) = world.document(id).map_or_else(
            || (String::new(), String::new()),
            |parsed| {
                let (nonce, _) = crate::csp::element_nonce(&parsed.document.base, id.node, slot);
                let integrity = crate::dom::tree::attr(&parsed.document.base, id.node, "integrity")
                    .unwrap_or_default()
                    .to_owned();
                (nonce, integrity)
            },
        );
        world.policy.csp.check_request(
            &world.origin,
            &world.document_url,
            url,
            crate::csp::Destination::Script,
            Some(crate::csp::ScriptRequest {
                nonce: &nonce,
                parser_inserted,
                integrity: &integrity,
            }),
        )
    }

    /// The main document's root node, the violation target for checks that
    /// are not attributable to an element.
    pub(crate) fn document_node(&self) -> Option<crate::dom::tree::NodeId> {
        self.world
            .borrow()
            .with_main_document(|parsed| crate::dom::tree::NodeId {
                document: parsed.id,
                node: parsed.document.base.root_node().id,
            })
    }

    /// A JSON string literal: `"` and `\` plus the control escapes a report
    /// body needs. The report fields are URLs and policy text, so full
    /// Unicode passes through unescaped.
    fn json_string(value: &str) -> String {
        use std::fmt::Write as _;
        let mut out = String::with_capacity(value.len() + 2);
        out.push('"');
        for c in value.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if (c as u32) < 0x20 => {
                    let _ = write!(out, "\\u{:04x}", c as u32);
                }
                c => out.push(c),
            }
        }
        out.push('"');
        out
    }

    /// Reports the violations a check produced: queues the legacy
    /// `report-uri` POSTs and queues the `securitypolicyviolation` events.
    /// Reports are fire-and-forget dials: the response is dropped, they never
    /// gate load, and they carry no CSP gate of their own, so a restrictive
    /// policy cannot silence its own reports
    /// (<https://w3c.github.io/webappsec-csp/#report-violation>).
    ///
    /// `target` attributes a request violation's report to the element whose
    /// trigger (insertion or connected `src` set) the executing script ran.
    /// The event itself fires at the document, because a request violation
    /// names no element. While the parser is still active the events wait:
    /// the violation can be reported before the parser reaches the script
    /// that would install its listener, and a task queued now would run
    /// before that script's fetch settles.
    pub(crate) fn fire_csp_violations(
        &mut self,
        target: crate::dom::tree::NodeId,
        violations: &[crate::csp::Violation],
    ) {
        if violations.is_empty() {
            return;
        }
        let mut enriched: Vec<crate::csp::Violation> = violations.to_vec();
        {
            let world = self.world.borrow();
            for violation in &mut enriched {
                // A fetch violation attributes to the element whose trigger
                // (insertion or connected `src` set) the executing script ran.
                // Inline and `eval` violations carry their own source (sample
                // or the eval hook's capture), so the creator map stays out.
                if violation.source_file.is_none()
                    && violation.blocked_uri != "inline"
                    && violation.blocked_uri != "eval"
                    && let Some(creator) = world.state.creator_stacks.get(&target)
                {
                    violation.source_file = Some(creator.url.clone());
                    violation.line_number = creator.line;
                    violation.column_number = creator.column;
                }
                if violation.status_code == 0 {
                    violation.status_code = self.http_status;
                }
            }
        }
        self.queue_csp_reports(&enriched);
        self.queue_csp_events(enriched);
    }

    /// Queues the event dispatch for one batch of violations, or holds it
    /// until the parser finishes. A parser-inserted blocked subresource can
    /// be reported while the parser is still waiting on a blocking script, so
    /// dispatching now would run before a listener that appears later in the
    /// document
    /// (<https://w3c.github.io/webappsec-csp/#report-violation>).
    fn queue_csp_events(&mut self, violations: Vec<crate::csp::Violation>) {
        if self.world.borrow().parser_active {
            self.pending_csp_events
                .push_back(Task::CspViolation(violations));
        } else {
            self.tasks.push_back(Task::CspViolation(violations));
        }
    }

    /// Dispatches one queued `securitypolicyviolation` batch. An inline
    /// violation fires at its element while that element is connected to the
    /// document the policy belongs to; anything else fires at the document
    /// (<https://w3c.github.io/webappsec-csp/#report-violation>).
    fn deliver_csp_violations(&mut self, violations: &[crate::csp::Violation]) {
        let Some(document) = self.document_node() else {
            return;
        };
        for violation in violations {
            let target = violation
                .element
                .filter(|element| self.csp_element_target(*element, document))
                .unwrap_or(document);
            self.fire_js(|js| js.fire_csp_violations(target, std::slice::from_ref(violation)));
        }
        self.adopt_js_work();
    }

    /// Whether an inline violation's element is still connected to `document`,
    /// which is what lets the event fire there instead of at the document
    /// (<https://w3c.github.io/webappsec-csp/#report-violation>).
    fn csp_element_target(
        &self,
        element: crate::dom::tree::NodeId,
        document: crate::dom::tree::NodeId,
    ) -> bool {
        if element.document != document.document {
            return false;
        }
        self.world.borrow().document(element).is_some_and(|parsed| {
            crate::dom::tree::is_connected(&parsed.document.base, element.node)
        })
    }

    /// Queues the `report-uri` / `report-to` POSTs for violations without
    /// firing DOM events. `frame-ancestors` response violations report this
    /// way: they are created with a null global, so no event target exists
    /// (<https://w3c.github.io/webappsec-csp/#should-navigation-response-to-navigation-request-of-type-in-target-be-blocked-by-content-security-policy>,
    /// <https://w3c.github.io/webappsec-csp/#report-violation>).
    pub(crate) fn queue_csp_reports(&mut self, violations: &[crate::csp::Violation]) {
        for violation in violations {
            // A `report-to` name the response's `Reporting-Endpoints`
            // defined takes the report as a Reporting API `csp-violation`
            // POST and suppresses `report-uri`
            // (<https://w3c.github.io/webappsec-csp/#directive-report-to>).
            if let Some(url) = self.reporting_url(&violation.report_to) {
                let user_agent = self
                    .world
                    .borrow()
                    .runtime
                    .identity
                    .user_agent
                    .as_str()
                    .to_owned();
                let body = Self::reporting_api_body(violation, &user_agent);
                self.queue_report(
                    &url,
                    body,
                    "application/reports+json",
                    crate::protocol::RequestPolicy::CspReportTo,
                );
                continue;
            }
            if violation.report_uris.is_empty() {
                continue;
            }
            // The legacy `report-uri` serialization, built without a JSON
            // dependency: every field is a string except `status-code`,
            // `line-number`, and `column-number`. `source-file` and its line
            // and column only appear when a source file exists
            // (<https://w3c.github.io/webappsec-csp/#deprecated-serialize-violation>).
            let source_fields =
                violation
                    .source_file
                    .as_deref()
                    .map_or_else(String::new, |source| {
                        format!(
                            ",\"source-file\":{},\"line-number\":{},\"column-number\":{}",
                            Self::json_string(&crate::csp::strip_url_for_reports(source)),
                            violation.line_number,
                            violation.column_number,
                        )
                    });
            let body = format!(
                "{{\"csp-report\":{{\"document-uri\":{},\"referrer\":\"\",\"blocked-uri\":{},\"effective-directive\":{},\"violated-directive\":{},\"original-policy\":{},\"disposition\":{},\"status-code\":{},\"script-sample\":{}{}}}}}",
                Self::json_string(&violation.document_uri),
                Self::json_string(&violation.blocked_uri),
                Self::json_string(&violation.effective_directive),
                Self::json_string(&violation.effective_directive),
                Self::json_string(&violation.original_policy),
                Self::json_string(violation.disposition.as_str()),
                violation.status_code,
                Self::json_string(violation.sample.as_deref().unwrap_or_default()),
                source_fields,
            )
            .into_bytes();
            for endpoint in &violation.report_uris {
                let Ok(url) = self.url.join(endpoint) else {
                    continue;
                };
                if !matches!(url.scheme(), "http" | "https") {
                    continue;
                }
                self.queue_report(
                    &url,
                    body.clone(),
                    "application/csp-report",
                    crate::protocol::RequestPolicy::CspReportUri,
                );
            }
        }
    }

    /// First `Reporting-Endpoints` URL named by `report-to`, resolved
    /// against the document URL. `None` falls back to `report-uri`.
    fn reporting_url(&self, names: &[String]) -> Option<Url> {
        for name in names {
            let url = self
                .reporting_endpoints
                .iter()
                .find(|(endpoint, _)| endpoint == name)
                .map(|(_, url)| url)?;
            let Ok(url) = self.url.join(url) else {
                continue;
            };
            if matches!(url.scheme(), "http" | "https") {
                return Some(url);
            }
        }
        None
    }

    /// Queues one violation report POST. The response is dropped and the
    /// dial never gates load. The policy is the delivery's own: a `report-to`
    /// endpoint gets the Reporting API's `cors` request, while a deprecated
    /// `report-uri` endpoint gets `no-cors` and no preflight.
    fn queue_report(
        &mut self,
        url: &Url,
        body: Vec<u8>,
        content_type: &str,
        policy: crate::protocol::RequestPolicy,
    ) {
        let mut dial = QueuedDial::new(DialContext::CspReport, policy, url, &self.dial_initiator());
        dial.req.method = net::Method::POST;
        dial.req.body = body;
        dial.req.content_type = Some(content_type.to_owned());
        self.queue_dial(dial);
    }

    /// The Reporting API envelope for a violation, with the
    /// `CSPViolationReportBody` camelCase fields. The endpoint receives a
    /// list of reports, even when only one fired
    /// (<https://w3c.github.io/webappsec-csp/#cspviolationreportbody>,
    /// <https://w3c.github.io/reporting/#send-reports>).
    fn reporting_api_body(violation: &crate::csp::Violation, user_agent: &str) -> Vec<u8> {
        let sample = violation
            .sample
            .as_deref()
            .map_or_else(|| "null".to_owned(), Self::json_string);
        let source_file = violation.source_file.as_deref().map_or_else(
            || "null".to_owned(),
            |source| Self::json_string(&crate::csp::strip_url_for_reports(source)),
        );
        let (line_number, column_number) = violation.source_file.as_deref().map_or_else(
            || ("null".to_owned(), "null".to_owned()),
            |_| {
                (
                    violation.line_number.to_string(),
                    violation.column_number.to_string(),
                )
            },
        );
        format!(
            "[{{\"type\":\"csp-violation\",\"age\":0,\"url\":{},\"user_agent\":{},\"body\":{{\"documentURL\":{},\"referrer\":\"\",\"blockedURL\":{},\"effectiveDirective\":{},\"originalPolicy\":{},\"sourceFile\":{},\"lineNumber\":{},\"columnNumber\":{},\"sample\":{},\"disposition\":{},\"statusCode\":{}}}}}]",
            Self::json_string(&violation.document_uri),
            Self::json_string(user_agent),
            Self::json_string(&violation.document_uri),
            Self::json_string(&violation.blocked_uri),
            Self::json_string(&violation.effective_directive),
            Self::json_string(&violation.original_policy),
            source_file,
            line_number,
            column_number,
            sample,
            Self::json_string(violation.disposition.as_str()),
            violation.status_code,
        )
        .into_bytes()
    }

    /// The document's `<base>`, referrer-policy `<meta>`, CSP `<meta>`, and
    /// script elements in tree order: the steps a parse owes once its tree
    /// exists. A script element only joins the walk while scripting is
    /// enabled, so a sandboxed frame still applies its meta referrer policy
    /// (<https://html.spec.whatwg.org/multipage/scripting.html#prepare-the-script-element>).
    fn parse_steps(&self) -> Vec<ParseStep> {
        let world = self.world.borrow();
        let Some(parsed) = world.main_document() else {
            return Vec::new();
        };
        let base = &parsed.document.base;
        let document = parsed.id;
        let mut steps = Vec::new();
        let mut stack = vec![base.root_node().id];
        while let Some(id) = stack.pop() {
            let Some(node) = base.get_node(id) else {
                continue;
            };
            let node_id = crate::dom::tree::NodeId { document, node: id };
            // A `<meta name=referrer>` applies its policy in the same
            // insertion steps as a CSP meta
            // (<https://html.spec.whatwg.org/multipage/semantics.html#meta-referrer>).
            let referrer_meta = crate::dom::tree::is_html_element(base, id, "meta")
                && crate::dom::tree::attr(base, id, "name")
                    .is_some_and(|name| name.eq_ignore_ascii_case("referrer"));
            if referrer_meta || crate::csp::meta_policy(base, id).is_some() {
                steps.push(ParseStep::Meta(node_id));
            }
            // A `<base href>` changes the base URL for the elements parsed
            // after it, so the walk carries the first one it meets. A script's
            // request URL is fixed when the parser reaches its end tag, which
            // tree order reproduces: a `<base>` later in the document must not
            // rewrite an earlier script's URL
            // (<https://html.spec.whatwg.org/multipage/urls-and-fetching.html#document-base-url>).
            if crate::dom::tree::is_html_element(base, id, "base")
                && crate::dom::tree::attr(base, id, "href").is_some()
            {
                steps.push(ParseStep::Base(node_id));
            }
            // Only a script the parser inserted belongs to this walk. A
            // script a running script connected is "script-inserted": it
            // prepares itself on insertion, and walking it here would mark it
            // started before that preparation could run it
            // (<https://html.spec.whatwg.org/multipage/scripting.html#parser-inserted>).
            if crate::dom::tree::is_html_element(base, id, "script")
                && !world.sandbox.scripts_disabled()
                && world.is_parser_inserted(node_id)
            {
                steps.push(ParseStep::Script(node_id));
            }
            stack.extend(node.children.iter().rev().copied());
        }
        steps
    }

    /// Runs every not-yet-executed script in tree order, stopping (returning
    /// true) when a `src` fetch starts: the fetch completion resumes the
    /// walk, so later scripts still run in order
    /// (<https://html.spec.whatwg.org/multipage/scripting.html#pending-parsing-blocking-script>).
    ///
    /// <https://html.spec.whatwg.org/multipage/parsing.html#parsing-main-intext>
    /// <https://html.spec.whatwg.org/multipage/scripting.html#prepare-the-script-element>
    fn run_scripts(&mut self) -> bool {
        let steps = self.parse_steps();
        let has_script = steps
            .iter()
            .any(|step| matches!(step, ParseStep::Script(_)));
        if has_script && !self.ensure_js_ok() {
            return false;
        }
        let mut parse_base = self.fallback_base_url();
        let mut base_seen = false;
        let _turn = self.js.as_ref().map(crate::js::JsRealm::enter_turn);
        for step in steps {
            let id = match step {
                ParseStep::Base(id) => {
                    if !base_seen {
                        base_seen = true;
                        // `base-uri` refuses a base the document may not
                        // use; the violation is queued as the parser reaches
                        // the element
                        // (<https://w3c.github.io/webappsec-csp/#is-base-allowed-for-document>).
                        // The element keeps its first answer across a resumed
                        // walk, so one base reports once.
                        if let Some(resolved) = self.world.borrow_mut().apply_base_element(id) {
                            parse_base = resolved;
                        }
                    }
                    continue;
                }
                ParseStep::Meta(id) => {
                    self.apply_meta_referrer(id);
                    self.apply_meta_csp(id);
                    continue;
                }
                ParseStep::Script(id) => id,
            };
            if !self.executed_scripts.insert(id) {
                continue;
            }
            let launched = self.dynamic_script_launched(id);
            // Mirror the run into the tree's already-started flag: the
            // binding layer's prepare step checks that flag, so without
            // this every move, clone, or adopt of a parser-run script
            // would execute it again
            // (<https://html.spec.whatwg.org/multipage/scripting.html#prepare-the-script-element>).
            if let Some(mut parsed) = self.world.borrow().document_mut(id) {
                parsed.document.mark_script_started(id.node);
            }
            // Iframes that precede this script in tree order already have
            // browsing contexts; later ones wait, matching incremental
            // insertion
            // (<https://html.spec.whatwg.org/multipage/iframe-embed-object.html#the-iframe-element:html-element-insertion-steps>).
            self.adopt_pending_frames_before_node(Some(id));
            // Microtask checkpoint before the script runs; parser
            // mutations queued since the last script deliver now.
            self.deliver_mutations();
            let script = crate::js::script_at(&self.world.borrow(), id);
            match script {
                Some(crate::js::Script::Classic(crate::js::ScriptSource::Inline {
                    source,
                    line,
                })) => {
                    let violations = self.inline_script_violations(&source, id);
                    if !violations.is_empty() {
                        self.fire_csp_violations(id, &violations);
                    }
                    if crate::csp::blocks(&violations) {
                        continue;
                    }
                    let filename = self.url.as_str().to_owned();
                    self.eval_classic(&source, Some(id), line, &filename);
                }
                Some(crate::js::Script::Classic(crate::js::ScriptSource::Src(src))) => {
                    if self.start_src_script(id, &src, launched, &parse_base) {
                        return true;
                    }
                }
                Some(crate::js::Script::Module(source)) => {
                    // Module scripts are deferred by default: parsing
                    // continues, then the queue runs in document order
                    // before `DOMContentLoaded`
                    // (<https://html.spec.whatwg.org/multipage/scripting.html#attr-script-defer>).
                    self.deferred_modules.push((id, source));
                }
                None => {}
            }
        }
        // The walk finished without starting a blocking fetch: every script
        // has run, so the iframes after the last one (or all of them, when
        // there are no scripts) get browsing contexts now. Deferred modules
        // and `DOMContentLoaded` handlers must already see them; leaving
        // adoption to `fire_document_end` puts it after `DOMContentLoaded`
        // (<https://html.spec.whatwg.org/multipage/iframe-embed-object.html#the-iframe-element:html-element-insertion-steps>).
        self.adopt_pending_frames();
        false
    }

    /// Whether a script-inserted fetch `adopt_js_work` already launched
    /// for `id`. A later tree walk must not start it again. Callers read
    /// this before marking the step started; after the mark every script
    /// looks launched
    /// (<https://html.spec.whatwg.org/multipage/scripting.html#prepare-the-script-element>).
    fn dynamic_script_launched(&self, id: crate::dom::tree::NodeId) -> bool {
        self.world
            .borrow()
            .document(id)
            .is_some_and(|parsed| parsed.document.script_already_started(id.node))
    }

    /// Starts the fetch for a tree-walked `src` script, unless a
    /// script-inserted fetch already launched for `id`. Returns whether the
    /// walk blocks on the fetch
    /// (<https://html.spec.whatwg.org/multipage/scripting.html#prepare-the-script-element>).
    fn start_src_script(
        &mut self,
        id: crate::dom::tree::NodeId,
        src: &str,
        launched: bool,
        base: &Url,
    ) -> bool {
        if launched {
            return false;
        }
        let Ok(url) = Self::resolve_script_url(base, src) else {
            return false;
        };
        let violations = self.script_url_violations(&url, id);
        if !violations.is_empty() {
            self.fire_csp_violations(id, &violations);
        }
        if crate::csp::blocks(&violations) {
            return false;
        }
        self.parser_blocked_script = Some(id);
        let options = self.world.borrow().subresource_options(id);
        let mut dial = QueuedDial::new(
            DialContext::ClassicScript { element: id },
            crate::protocol::RequestPolicy::ClassicScript {
                crossorigin: crate::protocol::cross_origin(options.crossorigin),
            },
            &url,
            &self.dial_initiator(),
        );
        dial.req.referrer = crate::protocol::ReferrerRequest::Client {
            url: self.referrer_url(),
        };
        dial.req.referrer_policy = options
            .referrer_policy
            .map(crate::protocol::referrer_policy)
            .or(self.world.borrow().policy.referrer_policy)
            .unwrap_or_default();
        dial.req.csp = Some(Box::new(
            self.redirect_gate(crate::csp::Destination::Script),
        ));
        dial.integrity = self.script_integrity(id);
        self.queue_dial(dial);
        true
    }

    /// Raw `integrity` attribute of a classic script element, for SRI
    /// verification and the CSP integrity-metadata allowance
    /// (<https://www.w3.org/TR/SRI/#parse-metadata>).
    fn script_integrity(&self, id: crate::dom::tree::NodeId) -> Option<String> {
        let world = self.world.borrow();
        world.document(id).and_then(|parsed| {
            crate::dom::tree::attr(&parsed.document.base, id.node, "integrity").map(str::to_owned)
        })
    }

    pub(in crate::document) fn resolve_dial_url(&self, spec: &str) -> Result<Url, TabError> {
        let url = self
            .resolve_frame_url(spec)
            .ok_or_else(|| TabError::InvalidUrl { spec: spec.into() })?;
        // `data:` is fetched by decoding the URL, not by a carrier dial
        // (<https://fetch.spec.whatwg.org/#scheme-fetch>).
        if url.scheme() != "http" && url.scheme() != "https" && url.scheme() != "data" {
            return Err(TabError::InvalidUrl { spec: spec.into() });
        }
        Ok(url)
    }

    /// The origin a dial from this document claims as its initiator. The
    /// inherited origin, never the URL: `<base href>` changes the base URL
    /// but not the document origin, and the browser derives CORS and the
    /// site lock from this value
    /// (<https://html.spec.whatwg.org/multipage/browsers.html#concept-origin>,
    /// <https://fetch.spec.whatwg.org/#concept-request-origin>).
    pub(crate) fn dial_initiator(&self) -> url::Origin {
        self.world.borrow().origin.clone()
    }

    /// The document URL the `"client"` referrer branch reads. For an
    /// `iframe srcdoc` document this walks to the container document's URL,
    /// which the world already captured
    /// (<https://w3c.github.io/webappsec-referrer-policy/#determine-requests-referrer>).
    pub(crate) fn referrer_url(&self) -> Url {
        self.world.borrow().referrer_url.clone()
    }

    /// This frame's base URL: the first `base` element's `href` resolved
    /// against the fallback base URL, or the fallback itself
    /// (<https://html.spec.whatwg.org/multipage/urls-and-fetching.html#document-base-url>).
    pub(crate) fn base_url(&self) -> Url {
        // An `about:srcdoc` document's own URL cannot be a base; its
        // fallback base URL is the creator document's base URL
        // (<https://html.spec.whatwg.org/multipage/urls-and-fetching.html#fallback-base-url>).
        self.world.borrow().main_base_url()
    }

    /// The URL a document's `<base href>` elements resolve against: the
    /// document URL, or the creator's base URL for an `about:srcdoc`
    /// document
    /// (<https://html.spec.whatwg.org/multipage/urls-and-fetching.html#fallback-base-url>).
    fn fallback_base_url(&self) -> Url {
        let world = self.world.borrow();
        world.base_url.clone().unwrap_or_else(|| self.url.clone())
    }

    /// Resolves a parser-inserted script's `src` against the base URL in
    /// effect where the parser reached it. Only schemes a dial can fetch are
    /// script resources
    /// (<https://html.spec.whatwg.org/multipage/scripting.html#prepare-the-script-element>).
    fn resolve_script_url(base: &Url, spec: &str) -> Result<Url, TabError> {
        let url = Url::parse(spec)
            .or_else(|_| base.join(spec))
            .map_err(|_| TabError::InvalidUrl { spec: spec.into() })?;
        if !matches!(url.scheme(), "http" | "https" | "data") {
            return Err(TabError::InvalidUrl { spec: spec.into() });
        }
        Ok(url)
    }

    /// Settles one page `fetch` dial: a post-request CSP check, then the
    /// response or a network error.
    fn finish_js_fetch_dial(
        &mut self,
        id: crate::js::JsFetchId,
        redirected: bool,
        request_url: &Url,
        outcome: DialOutcome,
    ) {
        self.record_event(RendererEvent::Fetch {
            status: outcome.status,
        });
        // A redirect may have landed on a URL the source list does not allow
        // (<https://w3c.github.io/webappsec-csp/#should-block-response>).
        let blocked = redirected
            && self.response_url_blocked(
                &outcome.final_url,
                request_url,
                crate::csp::Destination::Connect,
                None,
            );
        if blocked {
            // A streamed body's task holds the dial permits until the renderer
            // reads or cancels it, so cancel before dropping the outcome
            // (<https://fetch.spec.whatwg.org/#concept-response-body>).
            if let crate::protocol::ResponseBody::Streamed(stream) = outcome.body {
                self.services.cancel_stream(stream);
            }
            self.settle_js_fetch(id, None);
        } else {
            self.settle_js_fetch(id, Some(outcome));
        }
    }

    pub(in crate::document) fn finish_dial(&mut self, done: CompletedDial) {
        let CompletedDial {
            context,
            outcome,
            req,
            ancestors,
            integrity,
        } = done;
        let redirected = outcome.final_url != req.url;
        // A response `Set-Cookie` is a cookie store change like a script
        // write. The other realms process it in their own turn; this one
        // below, before the response reaches its caller
        // (<https://cookiestore.spec.whatwg.org/#process-cookie-changes>).
        let cookies_stored = outcome.cookies_stored;
        match context {
            DialContext::JsFetch { id } => {
                self.finish_js_fetch_dial(id, redirected, &req.url, outcome);
            }
            DialContext::ClassicScript { element } => {
                self.record_event(RendererEvent::Fetch {
                    status: outcome.status,
                });
                // Only the script the parser waits for resumes the walk.
                // Any other completion still runs its load steps, but the
                // walk stays paused until the blocking fetch settles.
                let unblocks_parser = self
                    .parser_blocked_script
                    .is_none_or(|waiting| waiting == element);
                if unblocks_parser {
                    self.parser_blocked_script = None;
                }
                // A redirect may have landed outside `script-src`. The
                // network refuses that hop; this is the report, and the
                // check for a host that already followed.
                // <https://w3c.github.io/webappsec-csp/#should-block-response>
                let blocked = redirected
                    && self.response_url_blocked(
                        &outcome.final_url,
                        &req.url,
                        crate::csp::Destination::Script,
                        Some(element),
                    );
                // SRI verification fails the load like a network error:
                // no execution, no `load` event, and no CSP violation
                // (<https://www.w3.org/TR/SRI/#verify-integrity>).
                let body = match &outcome.body {
                    crate::protocol::ResponseBody::Bytes(body) => Some(body.as_slice()),
                    crate::protocol::ResponseBody::Undecodable
                    | crate::protocol::ResponseBody::Streamed(_) => None,
                };
                let integrity_ok = integrity.as_deref().is_none_or(|metadata| {
                    body.is_some_and(|body| crate::csp::integrity_verifies(metadata, body))
                });
                // A classic script with a non-JavaScript MIME type does not
                // execute (<https://html.spec.whatwg.org/multipage/scripting.html#prepare-the-script-element>).
                let mime_ok = crate::js::classic_script_mime(outcome.content_type.as_deref());
                if !blocked
                    && integrity_ok
                    && mime_ok
                    && let Some(body) = body
                    && (200..300).contains(&outcome.status)
                {
                    let source = String::from_utf8_lossy(body);
                    let filename = outcome.final_url.to_string();
                    self.eval_classic(&source, Some(element), 1, &filename);
                    self.fire_js(|js| js.fire_node_load(element));
                } else {
                    self.fire_js(|js| js.fire_node_error(element));
                }
                if unblocks_parser {
                    self.resume_scripts();
                }
            }
            DialContext::FrameLoad { embedder, sandbox } => {
                // A redirect the embedder's `frame-src` refuses is not loaded.
                // Report-only still reports and then loads.
                // <https://w3c.github.io/webappsec-csp/#should-block-response>
                if redirected
                    && let Some(embedder) = embedder
                    && let Some(gate) = req.csp.as_deref()
                    && self.note_frame_redirect(embedder, gate, &outcome.final_url, &req.url)
                {
                    // Leave the current document in place.
                } else {
                    let url = outcome.final_url.clone();
                    // The flags enter the frame with the document they
                    // govern, so a failed or cancelled navigation leaves the
                    // current document under its own flags.
                    self.world.borrow_mut().install_sandbox(sandbox);
                    self.commit_frame_response(&outcome, &url, embedder, &ancestors);
                }
            }
            DialContext::BlitzResource { id, .. } => {
                let cors_clean = req.policy.cors().mode == net::cors::RequestMode::Cors;
                if let Some(request) = self.blitz_requests.remove(&id) {
                    self.deliver_blitz_resource(&outcome, request, redirected, cors_clean);
                }
            }
            // Fire-and-forget: the endpoint already stored the report, if it
            // cares. A failure is invisible by design.
            DialContext::CspReport => {}
        }
        if cookies_stored {
            self.world.borrow().notify_cookie_changes();
            self.fire_js(crate::js::JsRealm::process_cookie_changes);
        }
        self.adopt_js_work();
    }

    /// Commits a child frame's HTTP response, unless its `frame-ancestors`
    /// refuses the ancestors. A refused response falls back to an opaque
    /// `about:blank` the embedder can no longer read; the embedder dispatches
    /// the violations
    /// (<https://w3c.github.io/webappsec-csp/#frame-ancestors-navigation-response>).
    fn commit_frame_response(
        &mut self,
        outcome: &DialOutcome,
        url: &Url,
        embedder: Option<FrameId>,
        ancestors: &[Url],
    ) {
        let head = ResponseHead::http(
            outcome.status,
            url.clone(),
            outcome.content_type.clone(),
            &outcome.headers,
        );
        let policy = &head.source.policy().csp;
        // Set before the `frame-ancestors` decision: a refused response
        // dispatches its violations on the fallback document against the new
        // response's endpoints.
        self.set_reporting_endpoints(head.source.reporting_endpoints());
        if embedder.is_some()
            && !ancestors.is_empty()
            && self.note_frame_ancestors_block(embedder, url, policy, ancestors)
        {
            // The blank completes and fires the container's load.
            return;
        }
        // Announce the commit before the new document's load, so the
        // browser can re-create the top-level execution contexts
        // (<https://chromedevtools.github.io/devtools-protocol/tot/Page/#event-frameNavigated>).
        // The name is read before the install below: the reset that install
        // performs clears the live name, and the new document's scripts may
        // rename the tab, while the entry the old document owns keeps the
        // value it had
        // (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName>).
        let outgoing_name = {
            let shared = self.shared.borrow();
            crate::protocol::ContextName::parse(shared.tree.name(self.frame))
        };
        self.record_event(RendererEvent::Browser(BrowserEvent::Navigated {
            url: url.to_string(),
            outgoing_name,
        }));
        let crate::protocol::ResponseBody::Bytes(body) = &outcome.body else {
            return;
        };
        // Set before the parse runs: the new document's own scripts read
        // `document.referrer` while their inline scripts execute. The value is
        // the referrer the browser actually sent on the final hop, so redirect
        // policy and downgrade rules cannot disagree with the header
        // (<https://html.spec.whatwg.org/multipage/document-lifecycle.html#creating-and-initializing-a-document-object>).
        self.set_document_referrer(outcome.referrer.clone());
        self.install_frame_response(&head, body, None);
    }

    /// Hands one Blitz subresource response to its handler. A redirect the
    /// policy refuses is a failed load: empty bytes, and the element learns
    /// through its settlement (`<img>`) or an `error` event. `cors_clean`
    /// records that the request went out in CORS mode and survived the check,
    /// which makes the loaded image origin-clean for canvas.
    fn deliver_blitz_resource(
        &mut self,
        outcome: &DialOutcome,
        request: BlitzRequest,
        redirected: bool,
        cors_clean: bool,
    ) {
        let destination = request.destination;
        let blocked = redirected
            && destination.is_some_and(|destination| {
                self.response_url_blocked(&outcome.final_url, &request.key.url, destination, None)
            });
        let body = match &outcome.body {
            crate::protocol::ResponseBody::Bytes(body) => Some(body.as_slice()),
            crate::protocol::ResponseBody::Undecodable
            | crate::protocol::ResponseBody::Streamed(_) => None,
        };
        let succeeded = !blocked && (200..300).contains(&outcome.status) && body.is_some();
        // A `no-cors` image stays clean only while the request stays on this
        // origin; one cross-origin hop makes the response opaque for the rest
        // of the chain, even if it redirects back. A `crossorigin` image that
        // arrived passed its CORS check
        // (<https://fetch.spec.whatwg.org/#concept-request-response-tainting>,
        // <https://html.spec.whatwg.org/multipage/canvas.html#the-image-argument>).
        let origin_clean = cors_clean || !outcome.crossed_origin;
        // An image request's response taint travels with the delivery: Blitz
        // keeps it on the element's request record, which canvas reads. A
        // stylesheet or font body is cached to answer a repeat fetch without
        // another dial.
        if succeeded
            && let Some(body) = body
            && !destination.is_some_and(crate::csp::Destination::is_image)
        {
            self.subresource_bytes.insert(
                (request.key.url.clone(), request.key.crossorigin),
                body.to_vec(),
            );
        }
        // A style sheet's response is the policy source for every request the
        // sheet initiates: its own `Referrer-Policy` when it sent one, else the
        // document policy the sheet inherited at fetch time. Record it under
        // both the requested and the final URL, because Blitz files the sheet
        // under the URL it asked for
        // (<https://w3c.github.io/webappsec-referrer-policy/#integration-with-css>).
        if destination == Some(crate::csp::Destination::StyleSheet)
            && !blocked
            && (200..300).contains(&outcome.status)
        {
            let inherited = self
                .world
                .borrow()
                .policy
                .referrer_policy
                .unwrap_or_default();
            let policy = net::ReferrerPolicy::from_header(
                outcome
                    .headers
                    .iter()
                    .filter(|(name, _)| name.eq_ignore_ascii_case("referrer-policy"))
                    .map(|(_, value)| value.as_str()),
            )
            .unwrap_or(inherited);
            let final_url = &outcome.final_url;
            self.style_policies.insert(final_url.to_string(), policy);
            if &request.key.url != final_url {
                self.style_policies
                    .insert(request.key.url.to_string(), policy);
            }
        }
        if !succeeded {
            self.fail_blitz_request(request);
            return;
        }
        Box::new(request.handler).response(
            outcome.final_url.to_string(),
            blitz_traits::net::Bytes::from(body.unwrap_or_default().to_vec()),
            blitz_traits::net::ResponseInfo { origin_clean },
        );
        // The response is queued on Blitz's message channel; the batch's one
        // settle ingests it, so several arrivals in one drain pass cost one
        // resolve rather than one each.
    }

    pub(in crate::document) fn fail_dial(&mut self, fail: DialContext) {
        self.record_event(RendererEvent::FetchFailed);
        match fail {
            DialContext::JsFetch { id } => {
                self.settle_js_fetch(id, None);
            }
            DialContext::ClassicScript { element } => {
                let unblocks_parser = self
                    .parser_blocked_script
                    .is_none_or(|waiting| waiting == element);
                if unblocks_parser {
                    self.parser_blocked_script = None;
                }
                self.fire_js(|js| js.fire_node_error(element));
                if unblocks_parser {
                    self.resume_scripts();
                }
            }
            // Keep the frame's current (about:blank) document; the
            // container's load event still fires because the frame is no
            // longer waiting. A lost report is invisible by design.
            DialContext::FrameLoad { .. } | DialContext::CspReport => {}
            DialContext::BlitzResource { id, .. } => {
                if let Some(request) = self.blitz_requests.remove(&id) {
                    self.fail_blitz_request(request);
                }
            }
        }
        // A failed dial may still have stored a cookie: a hop applies its
        // `Set-Cookie` before the check that refuses the exchange, so a
        // redirect chain can end in a CORS, gate, or transport error with the
        // jar already changed. The jar is the record, so every realm diffs it
        // (<https://cookiestore.spec.whatwg.org/#process-cookie-changes>).
        self.world.borrow().notify_cookie_changes();
        self.fire_js(crate::js::JsRealm::process_cookie_changes);
        self.adopt_js_work();
    }

    /// Applies Blitz's queued resource messages and gives the active document
    /// its one style/layout pass for this rendering opportunity. Resolving is a
    /// no-op when nothing invalidated the tree. The arrival may also be the
    /// last one holding the load event.
    fn settle_blitz_layout(&mut self) {
        let passes = {
            let world = self.world.borrow();
            let Some(mut parsed) = world.main_document_mut() else {
                return;
            };
            crate::render::resolve_until_settled(&mut parsed.document.base)
        };
        #[cfg(test)]
        {
            self.blitz_resolve_passes += u64::from(passes);
        }
        #[cfg(not(test))]
        let _ = passes;
        self.note_linked_stylesheets();
        self.drain_image_settlements();
    }

    /// Resolve passes run by [`Self::settle_blitz_layout`] since the last
    /// reset. Test-only: it is the measurement behind "N mutations settle
    /// once".
    #[cfg(test)]
    pub(crate) fn blitz_resolve_passes(&self) -> u64 {
        self.blitz_resolve_passes
    }

    /// Clears the resolve-pass measurement before a test drives one batch.
    #[cfg(test)]
    pub(crate) fn reset_blitz_resolve_passes(&mut self) {
        self.blitz_resolve_passes = 0;
    }

    /// Delivers Blitz's queued document messages without resolving layout, so
    /// a settlement that does not change the tree (a source-less image
    /// failure) still reaches the drain that releases its load token
    /// (<https://html.spec.whatwg.org/multipage/images.html#updating-the-image-data>).
    fn pump_blitz_messages(&mut self) {
        let world = self.world.borrow();
        let Some(mut parsed) = world.main_document_mut() else {
            return;
        };
        parsed.document.base.handle_messages();
    }

    /// Fires `load` on connected `<link rel=stylesheet>` elements whose sheet
    /// has arrived. Blitz attaches a sheet to the link node only after the
    /// response parses, so the sheet's presence is the fetch-complete signal;
    /// the scan runs when the document's stylesheet generation moved, so image
    /// and font deliveries pay nothing
    /// (<https://html.spec.whatwg.org/multipage/semantics.html#the-link-element>,
    /// <https://html.spec.whatwg.org/multipage/links.html#link-type-stylesheet>).
    fn note_linked_stylesheets(&mut self) {
        let (generation, arrivals, stale) = {
            let world = self.world.borrow();
            let Some(parsed) = world.main_document() else {
                return;
            };
            let base = &parsed.document.base;
            let generation = base.stylesheet_generation();
            if generation == self.stylesheet_generation {
                return;
            }
            let document = parsed.id;
            let linked = |id: blitz_dom::NodeId| {
                crate::dom::tree::is_stylesheet_link(base, id)
                    && base.node_has_stylesheet(id)
                    && crate::dom::tree::is_connected(base, id)
            };
            let arrivals: Vec<(crate::dom::tree::NodeId, String)> = base
                .stylesheet_owner_nodes()
                .filter(|id| linked(*id))
                .filter_map(|id| {
                    crate::dom::tree::attr(base, id, "href")
                        .and_then(|href| base.resolve_url(href))
                        .map(|url| {
                            (
                                crate::dom::tree::NodeId { document, node: id },
                                url.as_str().to_owned(),
                            )
                        })
                })
                .collect();
            let stale: Vec<crate::dom::tree::NodeId> = self
                .linked_sheets
                .keys()
                .copied()
                .filter(|node| node.document != document || !linked(node.node))
                .collect();
            (generation, arrivals, stale)
        };
        self.stylesheet_generation = generation;
        for node in stale {
            self.linked_sheets.remove(&node);
        }
        for (element, url) in arrivals {
            if self.linked_sheets.get(&element) == Some(&url) {
                continue;
            }
            self.linked_sheets.insert(element, url);
            self.tasks.push_back(Task::NodeLoad(element));
        }
    }

    pub(in crate::document) fn settle_js_fetch(
        &mut self,
        id: JsFetchId,
        outcome: Option<DialOutcome>,
    ) {
        self.fire_js(|js| js.finish_js_fetch(id, outcome));
    }

    /// Runs the post-parsing steps of "the end": set readiness to
    /// `interactive`, fire `DOMContentLoaded`, then fire `load`
    /// (<https://html.spec.whatwg.org/multipage/parsing.html#the-end>).
    fn fire_document_end(&mut self) {
        if self.world.borrow().main_ready_state() != ReadyState::Loading {
            return;
        }
        self.world
            .borrow_mut()
            .set_main_ready_state(ReadyState::Interactive);
        self.fire_js(crate::js::JsRealm::fire_ready_state_change);
        self.run_deferred_modules();
        self.fire_js(crate::js::JsRealm::fire_dom_content_loaded);
        self.adopt_js_work();
        self.record_event(RendererEvent::Browser(BrowserEvent::DomContentLoaded));
        // A script may have appended an iframe after the parser finished; its
        // load must delay this document's own load event
        // (<https://html.spec.whatwg.org/multipage/parsing.html#delay-the-load-event>).
        self.adopt_pending_frames();
    }

    /// Runs `body` with `element` recorded as the root of the module graph
    /// being fetched, restoring the previous value afterwards.
    ///
    /// Every fetch in the graph reads the root's script fetch options: its
    /// `crossorigin` credentials mode and `referrerpolicy`
    /// (<https://html.spec.whatwg.org/multipage/webappapis.html#fetch-the-descendants-of-and-link-a-module-script>).
    /// `current_script` stays unset, so `document.currentScript` is null while
    /// a module evaluates
    /// (<https://html.spec.whatwg.org/multipage/dom.html#dom-document-currentscript>).
    pub(in crate::document) fn with_module_root<R>(
        &mut self,
        element: Option<crate::dom::tree::NodeId>,
        body: impl FnOnce(&mut Self) -> R,
    ) -> R {
        let previous = self.world.borrow().state.module_fetch_root;
        self.world.borrow_mut().state.module_fetch_root = element;
        let result = body(self);
        self.world.borrow_mut().state.module_fetch_root = previous;
        result
    }

    fn run_deferred_modules(&mut self) {
        let modules = std::mem::take(&mut self.deferred_modules);
        for (element, source) in modules {
            match self.run_module_body(element, source) {
                None => self
                    .tasks
                    .push_back(crate::document::Task::NodeError(element)),
                Some(result) => {
                    if result.is_err() {
                        self.record_event(RendererEvent::ScriptFailed);
                    } else {
                        self.fire_js(|js| js.fire_node_load(element));
                    }
                    self.adopt_js_work();
                }
            }
        }
    }

    /// Runs the shared declarative refresh steps for one value, from a
    /// `Refresh` response head or a `meta http-equiv=refresh`. The first value
    /// wins; a later one is ignored
    /// (<https://html.spec.whatwg.org/multipage/semantics.html#attr-meta-http-equiv-refresh>).
    fn start_declarative_refresh(&mut self, value: &str) {
        if self.will_declaratively_refresh
            || self
                .world
                .borrow()
                .sandbox
                .contains(crate::protocol::SandboxFlag::AutomaticFeatures)
        {
            return;
        }
        let document_url = self.world.borrow().document_url.clone();
        let Some((target, delay)) = parse_declarative_refresh(value, &document_url) else {
            return;
        };
        self.will_declaratively_refresh = true;
        self.pending_refresh = Some((target, delay));
        self.arm_refresh();
    }

    /// Starts a pending refresh's clock. The delay is measured from the
    /// document's completely-loaded point, or from the `meta` insertion when
    /// the document already loaded
    /// (<https://html.spec.whatwg.org/multipage/semantics.html#attr-meta-http-equiv-refresh>).
    fn arm_refresh(&mut self) {
        if !self.load_fired || self.refresh_timer.is_some() {
            return;
        }
        let Some((target, delay)) = self.pending_refresh.take() else {
            return;
        };
        self.refresh_target = Some(target);
        let id = self.schedule_timer(delay);
        self.refresh_timer = Some(id);
    }

    /// Navigates this document's browsing context to its armed refresh target,
    /// using this document as the navigation's source.
    fn fire_refresh(&mut self) {
        let Some(target) = self.refresh_target.take() else {
            return;
        };
        let navigation = FrameNavigation::get(
            crate::js::NavigationTarget::Target(crate::protocol::TargetName::Current),
            target.to_string(),
        );
        self.world.borrow_mut().queue_frame_navigation(navigation);
    }

    /// The single `load` check. Readiness must have reached `interactive`, the
    /// delay count must be zero, and any Blitz request that a completion just
    /// produced is made a dial first, so its guard exists before the count is
    /// consulted
    /// (<https://html.spec.whatwg.org/multipage/parsing.html#the-end>).
    pub(crate) fn check_load(&mut self) -> bool {
        // A queued document message (a failed image source, a landed
        // stylesheet) must reach the settlement drain even when no redraw is
        // pending.
        self.pump_blitz_messages();
        // A stale tree must settle before load is decided: starting the image
        // requests a newly inserted `src` implies is what makes `load` wait for
        // them (<https://html.spec.whatwg.org/multipage/images.html#update-the-image-data>).
        // `resolve_until_settled` is a no-op when nothing invalidated the
        // tree, so a paint-only frame pays nothing here.
        self.settle_blitz_layout();
        // Answer the shell's request so a later settle is not opened by (or
        // blocked on) a redraw that this pass already consumed.
        self.blitz_shell.take_redraw();
        self.drain_image_settlements();
        // The settle can start image fetches (a new `src`, a style-only
        // background). Move them onto dials before any early return, or the
        // loop sees no queued work and sleeps with the fetch parked.
        self.drain_blitz_fetches();
        if !self.world.borrow().parser_active {
            self.tasks.append(&mut self.deferred_image_events);
            self.tasks.append(&mut self.pending_csp_events);
        }
        if self.load_fired {
            return false;
        }
        if self.world.borrow().main_ready_state() == ReadyState::Loading {
            return false;
        }
        if self.load_delay.is_pending() {
            return false;
        }
        // The opener's or parent's pre-realm `WindowProxy` writes (an `onload`
        // assigned at `window.open` or `iframe.contentWindow`) must land before
        // `load` fires, or the handler is not there to hear it
        // (<https://html.spec.whatwg.org/multipage/window-object.html#windowproxy-set>).
        if self.awaits_proxy_replay() {
            return false;
        }
        self.complete_ready_state();
        self.load_fired = true;
        self.arm_refresh();
        self.record_event(RendererEvent::Browser(BrowserEvent::Load));
        self.fire_js(crate::js::JsRealm::fire_load);
        self.adopt_js_work();
        true
    }

    /// Moves readiness to `complete` and dispatches its change event once. A
    /// child frame can be the last thing delaying `load` after the parser
    /// already reached `interactive`.
    fn complete_ready_state(&mut self) {
        if self.world.borrow().main_ready_state() == ReadyState::Complete {
            return;
        }
        self.world
            .borrow_mut()
            .set_main_ready_state(ReadyState::Complete);
        self.fire_js(crate::js::JsRealm::fire_ready_state_change);
    }

    /// Moves every queued Blitz subresource fetch onto a carrier dial,
    /// filing its response handler under the fetch id.
    fn drain_blitz_fetches(&mut self) {
        while let Ok((fetch, handler)) = self.blitz_fetch_rx.try_recv() {
            let incoming = fetch.request;
            let key = blitz_dom::ImageKey {
                url: incoming.url.clone(),
                crossorigin: incoming.crossorigin,
            };
            let element = self.subresource_element(incoming.node);
            let (policy, destination) = subresource_request(
                incoming.kind,
                crate::protocol::cross_origin(incoming.crossorigin),
            );
            if !destination.is_some_and(crate::csp::Destination::is_image)
                && let Some(cached) = self
                    .subresource_bytes
                    .get(&(incoming.url.clone(), incoming.crossorigin))
                    .cloned()
            {
                Box::new(handler).bytes(
                    incoming.url.to_string(),
                    blitz_traits::net::Bytes::from(cached),
                );
                continue;
            }
            let request = BlitzRequest {
                handler,
                key,
                referrer_policy: incoming.referrer_policy,
                node: element,
                destination,
            };
            // Per-destination policy: a blocked stylesheet, image, or font is
            // never dialed; the handler receives empty bytes so Blitz
            // releases the resource as failed
            // (<https://w3c.github.io/webappsec-csp/#should-block-request>).
            if let Some(destination) = destination {
                let violations = self.subresource_violations(&incoming.url, destination);
                let target = element.or_else(|| self.document_node());
                if !violations.is_empty()
                    && let Some(target) = target
                {
                    self.fire_csp_violations(target, &violations);
                }
                if crate::csp::blocks(&violations) {
                    self.fail_blitz_request(request);
                    continue;
                }
            }
            // The Fetch initiator is the document, not the fetch target: the
            // provider never sees the document URL, so it stamps the target
            // as initiator and the document corrects it here
            // (<https://fetch.spec.whatwg.org/#concept-request-initiator>).
            let (referrer, referrer_policy) = if let Some(sheet) = incoming.style_sheet.clone() {
                // A request a style sheet initiates uses the sheet's URL
                // as its referrer and the policy that sheet's response
                // carried, not the document's
                // (<https://w3c.github.io/webappsec-referrer-policy/#integration-with-css>).
                let policy = self
                    .style_policies
                    .get(sheet.as_str())
                    .copied()
                    .or(self.world.borrow().policy.referrer_policy)
                    .unwrap_or_default();
                (crate::protocol::ReferrerRequest::Url(sheet), policy)
            } else {
                // HTML samples the element's `referrerpolicy` when the
                // request is created; Blitz carried that value, so a write
                // between creation and dial cannot change it
                // (<https://html.spec.whatwg.org/multipage/urls-and-fetching.html#referrer-policy-attribute>).
                let policy = request
                    .referrer_policy
                    .map(crate::protocol::referrer_policy)
                    .or(self.world.borrow().policy.referrer_policy)
                    .unwrap_or_default();
                (
                    crate::protocol::ReferrerRequest::Client {
                        url: self.referrer_url(),
                    },
                    policy,
                )
            };
            let mut dial = QueuedDial::new(
                DialContext::BlitzResource {
                    id: fetch.id,
                    destination,
                },
                policy,
                &incoming.url,
                &self.dial_initiator(),
            );
            self.blitz_requests.insert(fetch.id, request);
            dial.req.referrer = referrer;
            dial.req.referrer_policy = referrer_policy;
            dial.req.csp = destination.map(|destination| Box::new(self.redirect_gate(destination)));
            self.queue_dial(dial);
        }
    }

    /// Fails one Blitz request: empty bytes release Blitz's hold, and the
    /// element that started it learns through its settlement (`<img>`) or an
    /// `error` event.
    fn fail_blitz_request(&mut self, request: BlitzRequest) {
        let BlitzRequest {
            handler,
            node,
            destination,
            ..
        } = request;
        Box::new(handler).bytes(String::new(), blitz_traits::net::Bytes::new());
        if !destination.is_some_and(crate::csp::Destination::is_image)
            && let Some(node) = node
        {
            self.tasks.push_back(Task::NodeError(node));
        }
    }

    pub(crate) fn redirect_gate(
        &self,
        destination: crate::csp::Destination,
    ) -> crate::RedirectGate {
        let world = self.world.borrow();
        crate::RedirectGate::new(world.policy.csp.clone(), &world.origin, destination)
    }

    /// Records `frame-src` violations for a redirect and reports whether the
    /// enforced policy refuses the response. The embedding document dispatches
    /// the events; this frame only queues them.
    fn note_frame_redirect(
        &mut self,
        embedder: FrameId,
        gate: &crate::RedirectGate,
        final_url: &Url,
        request_url: &Url,
    ) -> bool {
        let violations = gate.redirected_violations(&self.url, final_url, request_url);
        let blocked = crate::csp::blocks(&violations);
        if !violations.is_empty() {
            self.embedder_csp_reports.push((embedder, violations));
        }
        blocked
    }

    /// Records `frame-ancestors` violations for a child response and reports
    /// whether the enforced policy refuses the embed. A refused response is
    /// replaced with an opaque `about:blank` the embedder can no longer read.
    /// No DOM event fires: the response-policy violation is created with a
    /// null global, so only the network reports are sent
    /// (<https://w3c.github.io/webappsec-csp/#should-navigation-response-to-navigation-request-of-type-in-target-be-blocked-by-content-security-policy>).
    fn note_frame_ancestors_block(
        &mut self,
        embedder: Option<FrameId>,
        child_url: &Url,
        policy: &crate::csp::List,
        ancestors: &[Url],
    ) -> bool {
        if embedder.is_none() {
            return false;
        }
        let violations = policy.check_frame_ancestors(child_url, ancestors);
        let blocked = crate::csp::blocks(&violations);
        if !violations.is_empty() {
            self.queue_csp_reports(&violations);
        }
        if blocked {
            self.load_about_blank(
                Some(&about_blank()),
                &about_blank(),
                Some(url::Origin::new_opaque()),
            );
        }
        blocked
    }

    /// `frame-src` redirect violations queued for the embedding document.
    pub(crate) fn take_embedder_csp_reports(
        &mut self,
    ) -> Vec<(FrameId, Vec<crate::csp::Violation>)> {
        std::mem::take(&mut self.embedder_csp_reports)
    }

    /// Whether the response URL of a completed request violates the policy
    /// after redirects, firing the violations. Paths are ignored, as the
    /// post-request check requires
    /// (<https://w3c.github.io/webappsec-csp/#should-block-response>).
    pub(in crate::document) fn response_url_blocked(
        &mut self,
        final_url: &Url,
        request_url: &Url,
        destination: crate::csp::Destination,
        target: Option<crate::dom::tree::NodeId>,
    ) -> bool {
        let violations = {
            let world = self.world.borrow();
            world.policy.csp.check_redirected_request(
                &world.origin,
                &world.document_url,
                final_url,
                request_url,
                destination,
            )
        };
        if !violations.is_empty()
            && let Some(target) = target.or_else(|| self.document_node())
        {
            self.fire_csp_violations(target, &violations);
        }
        crate::csp::blocks(&violations)
    }

    /// The violations a Blitz subresource fetch produces.
    pub(in crate::document) fn subresource_violations(
        &self,
        url: &Url,
        destination: crate::csp::Destination,
    ) -> Vec<crate::csp::Violation> {
        let world = self.world.borrow();
        world
            .policy
            .csp
            .check_request(&world.origin, &world.document_url, url, destination, None)
    }

    /// The element a Blitz request names, paired with this document. The
    /// provider is per document and Blitz's own document id is a process-wide
    /// counter unrelated to this store's id, so the active document is the one
    /// that issued the fetch.
    fn subresource_element(
        &self,
        node: Option<blitz_traits::node_id::NodeId>,
    ) -> Option<crate::dom::tree::NodeId> {
        let document = self.world.borrow().main_document().map(|parsed| parsed.id);
        document
            .zip(node)
            .map(|(document, node)| crate::dom::tree::NodeId { document, node })
    }

    /// Queues one image event, behind the parser while it runs.
    fn queue_image_event(&mut self, event: Task) {
        if self.world.borrow().parser_active {
            self.deferred_image_events.push_back(event);
        } else {
            self.tasks.push_back(event);
        }
    }

    /// Applies Blitz's image settlements. Blitz owns request identity, so the
    /// event says which node settled how; this attaches or releases the
    /// node's load-delay token and queues the element's event. Every getter
    /// reads Blitz state directly, so nothing has to be copied or validated
    /// here (<https://html.spec.whatwg.org/multipage/images.html#update-the-image-data>).
    fn drain_image_settlements(&mut self) {
        let document = self.world.borrow().main_document().map(|parsed| parsed.id);
        while let Ok(settlement) = self.image_settle_rx.try_recv() {
            let Some(document) = document else {
                // The document this request belonged to is gone, so nothing
                // observes the settlement.
                continue;
            };
            let element = crate::dom::tree::NodeId {
                document,
                node: settlement.node,
            };
            match settlement.outcome {
                blitz_dom::ImageOutcome::Started => {
                    let delay = self.load_delay.guard();
                    self.image_load_delays.insert(element, delay);
                }
                blitz_dom::ImageOutcome::Loaded => {
                    self.image_load_delays.remove(&element);
                    self.queue_image_event(Task::NodeLoad(element));
                }
                blitz_dom::ImageOutcome::Failed => {
                    self.image_load_delays.remove(&element);
                    self.queue_image_event(Task::NodeError(element));
                }
                blitz_dom::ImageOutcome::Aborted => {
                    // The element lost its source; it is complete and no
                    // event is due, but the request no longer holds `load`.
                    self.image_load_delays.remove(&element);
                }
            }
        }
    }

    /// A child browsing context started loading; this document's `load` event
    /// waits for it, and the container is remembered until it finishes.
    pub(crate) fn mark_frame_load_pending(&mut self, container: crate::dom::tree::NodeId) {
        self.pending_frame_loads
            .insert(container, self.load_delay.guard());
    }

    /// The containers whose child frames have not finished loading.
    pub(crate) fn pending_frame_loads(&self) -> Vec<crate::dom::tree::NodeId> {
        self.pending_frame_loads.keys().copied().collect()
    }

    /// Fires one container's `load` event and lets this document's own `load`
    /// event proceed once no child is left loading.
    pub(crate) fn finish_frame_load(&mut self, container: crate::dom::tree::NodeId) -> bool {
        if self.pending_frame_loads.remove(&container).is_none() {
            return false;
        }
        // A src-less iframe already fired `load` from its insertion steps;
        // that firing is recorded per container, not re-read from the current
        // attributes, so a later navigation reusing the container still fires
        // (<https://html.spec.whatwg.org/multipage/iframe-embed-object.html#the-iframe-element:html-element-insertion-steps>).
        let already_fired = self
            .world
            .borrow_mut()
            .insertion_load_fired
            .remove(&container);
        if !already_fired {
            self.fire_node_load(container);
        }
        true
    }

    /// Drops a container that went away before its child finished loading.
    pub(crate) fn cancel_frame_load(&mut self, container: crate::dom::tree::NodeId) {
        let _ = self
            .world
            .borrow_mut()
            .insertion_load_fired
            .remove(&container);
        self.pending_frame_loads.remove(&container);
    }

    pub(in crate::document) fn record_event(&mut self, event: RendererEvent) {
        if self.events.len() == MAX_PENDING_EVENTS {
            self.events_overflowed = true;
            return;
        }
        self.events.push(event);
    }
}

/// Whether parsed HTML needs a realm: a `script` element or any event
/// handler content attribute (`on*`, same predicate as attribute-set
/// compilation in `bindings/attributes.rs`). The walk is iterative so deep
/// documents cannot overflow the stack.
fn html_needs_realm(parsed: &crate::Parsed) -> bool {
    let base = &parsed.document.base;
    let mut stack = vec![base.root_node().id];
    while let Some(id) = stack.pop() {
        let Some(node) = base.get_node(id) else {
            continue;
        };
        if crate::dom::tree::is_html_element(base, id, "script") {
            return true;
        }
        if let Some(element) = node.data.downcast_element()
            && element.attrs.iter().any(|attribute| {
                attribute.name.prefix.as_ref().is_none()
                    && attribute.name.ns.as_str().is_empty()
                    && crate::dom::tree::is_handler_attribute_name(attribute.name.local.as_str())
            })
        {
            return true;
        }
        stack.extend(node.children.iter().rev().copied());
    }
    false
}

impl From<crate::js::JsError> for TabError {
    fn from(err: crate::js::JsError) -> Self {
        match err {
            crate::js::JsError::Engine(message) => Self::Script(ScriptFailure::Engine { message }),
            crate::js::JsError::Interrupted => Self::Script(ScriptFailure::Interrupted),
            crate::js::JsError::BadTimerId => Self::Script(ScriptFailure::BadTimerId),
        }
    }
}
