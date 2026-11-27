use enumerated::Enumerated;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant as WallClock;

use tokio::sync::Notify;

use blitz_traits::net::NetHandler;
use net::cors::CrossOrigin;
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
use self::state::{CspState, RefreshState, ResponseStage, UiEventQueue};

mod csp;
pub(crate) mod dial;
mod dials;
pub(crate) mod dom;
mod drain;
mod state;

mod intern;
mod load;

pub(crate) use crate::js::ScriptValue;

// https://html.spec.whatwg.org/multipage/document-sequences.html#creating-a-new-auxiliary-browsing-context
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

const MAX_PENDING_JS_FETCHES: usize = 256;
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

#[derive(Clone, Copy)]
struct DialTiming {
    start: f64,
    end: f64,
}

impl DialTiming {
    fn new(started: std::time::Instant, end: f64) -> Self {
        Self {
            start: end - started.elapsed().as_secs_f64() * 1000.0,
            end,
        }
    }
}

enum Task {
    Timer(TimerId),
    DialFinished(DialId, Box<CompletedDial>),
    DialFailed(DialId, DialContext),
    WindowMessage(WindowMessage),
    StorageEvent(crate::storage::PendingStorageEvent),
    RemoteMessage(crate::protocol::PostedMessage),
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
    WorkerMessage {
        worker: crate::worker::WorkerId,
        payload: String,
        ports: Vec<u64>,
    },
    WorkerError(crate::worker::WorkerError),
    // https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model
    WorkerLoadFailed(crate::worker::WorkerId),
    // https://html.spec.whatwg.org/multipage/images.html#updating-the-image-data
    // https://html.spec.whatwg.org/multipage/semantics.html#the-link-element
    NodeLoad(crate::dom::tree::NodeId),
    NodeError(crate::dom::tree::NodeId),
    // https://w3c.github.io/webappsec-csp/#report-violation
    CspViolation(Vec<crate::csp::Violation>),
    // https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#set-the-selection-range
    SelectionChanged(crate::dom::tree::NodeId),
    // https://html.spec.whatwg.org/multipage/interactive-elements.html#queue-a-details-toggle-event-task
    // https://html.spec.whatwg.org/multipage/interactive-elements.html#queue-a-dialog-toggle-event-task
    ToggleEvent(crate::dom::tree::NodeId),
    // https://html.spec.whatwg.org/multipage/interactive-elements.html#the-dialog-element:close-event
    DialogClose(crate::dom::tree::NodeId),
    // https://drafts.csswg.org/cssom-view/#run-the-scroll-steps
    ScrollEvents,
    // https://drafts.csswg.org/css-animations-1/#events
    AnimationEvents,
    // https://html.spec.whatwg.org/multipage/webappapis.html#notify-about-rejected-promises
    RejectionTasks(crate::task_queue::RealmId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RealmReset {
    Navigation,
    Reopen,
}

// https://html.spec.whatwg.org/multipage/webappapis.html#queue-a-global-task
// https://html.spec.whatwg.org/multipage/webappapis.html#concept-task-runnable
pub(crate) struct WindowMessage {
    pub(crate) source: FrameId,
    pub(crate) origin: String,
    pub(crate) target_origin: TargetOrigin,
    pub(crate) payload: String,
    pub(crate) ports: Vec<u64>,
}

// https://html.spec.whatwg.org/multipage/browsing-the-web.html#url-and-history-update-steps
pub(crate) struct HistoryUpdate {
    pub(crate) url: String,
    pub(crate) state: String,
    pub(crate) handling: crate::protocol::HistoryHandling,
}

#[derive(Clone, Copy)]
pub(crate) enum DialContext {
    JsFetch {
        id: JsFetchId,
        // https://chromedevtools.github.io/devtools-protocol/tot/Page/#method-createIsolatedWorld
        realm: crate::task_queue::RealmId,
    },
    ClassicScript {
        element: crate::dom::tree::NodeId,
    },
    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#navigate
    FrameLoad {
        embedder: Option<FrameId>,
        // https://html.spec.whatwg.org/multipage/browsing-the-web.html#determine-navigation-params
        sandbox: crate::protocol::SandboxFlags,
    },
    BlitzResource {
        id: u64,
        destination: Option<crate::csp::Destination>,
    },
    // https://w3c.github.io/webappsec-csp/#report-violation
    CspReport,
}

pub(crate) struct QueuedDial {
    pub(crate) context: DialContext,
    pub(crate) req: DialRequest,
    pub(crate) started: std::time::Instant,
    // https://w3c.github.io/webappsec-csp/#frame-ancestors-navigation-response
    pub(crate) ancestors: Vec<Url>,
    pub(crate) integrity: Option<String>,
}

impl QueuedDial {
    pub(crate) fn new(
        context: DialContext,
        policy: crate::protocol::RequestPolicy,
        url: &Url,
        initiator: &url::Origin,
    ) -> Self {
        Self {
            req: crate::protocol::DialRequest::new(policy, url, initiator),
            context,
            started: std::time::Instant::now(),
            ancestors: Vec::new(),
            integrity: None,
        }
    }
}

pub(crate) struct CompletedDial {
    pub(crate) context: DialContext,
    pub(crate) outcome: DialOutcome,
    pub(crate) req: DialRequest,
    pub(crate) started: std::time::Instant,
    pub(crate) ancestors: Vec<Url>,
    pub(crate) integrity: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ParserOwner {
    Carrier,
    Script,
}

struct ActiveParse {
    buffer: String,
    delay: LoadDelayGuard,
}

// https://html.spec.whatwg.org/multipage/browsing-the-web.html#session-history-entry
#[derive(Clone)]
pub(crate) struct LocalDocumentState {
    pub(crate) url: Url,
    pub(crate) base_url: Option<Url>,
    pub(crate) cookie_url: Option<Url>,
    pub(crate) referrer: Option<Url>,
    pub(crate) referrer_url: Url,
    pub(crate) origin: url::Origin,
    pub(crate) policy: PolicyContainer,
    pub(crate) sandbox: crate::protocol::SandboxFlags,
}

// https://html.spec.whatwg.org/multipage/iframe-embed-object.html#the-iframe-element:html-element-insertion-steps
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

// https://html.spec.whatwg.org/multipage/iframe-embed-object.html#the-iframe-element:html-element-insertion-steps
fn materialize_iframe_document(
    world_rc: &Rc<RefCell<World>>,
    frame: FrameId,
    container: crate::dom::tree::NodeId,
    new_frames: &Rc<RefCell<Vec<CreatedFrame>>>,
) {
    // https://html.spec.whatwg.org/multipage/document-sequences.html#creating-browsing-contexts
    let base_url = world_rc.borrow().main_base_url();
    let (runtime, referrer_url, init_scripts, bindings, viewport) = {
        let world = world_rc.borrow();
        let doc = world.doc();
        let win = world.win();
        (
            world.runtime.clone(),
            doc.referrer_url.clone(),
            Rc::clone(&win.init_scripts),
            Rc::clone(&win.bindings),
            win.viewport_size.get(),
        )
    };
    // https://html.spec.whatwg.org/multipage/browsers.html#determining-the-creation-sandboxing-flags
    let sandbox = sandboxing_flags(world_rc, container);
    let origin = if sandbox.origin_is_opaque() {
        url::Origin::new_opaque()
    } else {
        world_rc.borrow().doc().origin.clone()
    };
    let mut document = Document::with_shared(frame, &runtime, &init_scripts, &bindings);
    document.set_viewport_size(viewport);
    {
        let world_handle = document.world();
        let mut world = world_handle.borrow_mut();
        {
            let mut doc = world.doc_mut();
            doc.policy = world_rc.borrow().doc().policy.clone();
            doc.sandbox = sandbox;
            doc.top_level = Some(world_rc.borrow().top_level_origin());
        }
        world.set_parent_context(&world_rc.borrow());
    }
    document.load_about_blank_tree(
        Some(&base_url),
        &world_rc.borrow().cookie_url(),
        Some(InheritedOrigin {
            origin,
            domain: Rc::clone(&world_rc.borrow().doc().domain),
        }),
    );
    // https://html.spec.whatwg.org/multipage/browsers.html#creating-a-new-browsing-context
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

// https://html.spec.whatwg.org/multipage/document-sequences.html#creating-a-new-auxiliary-browsing-context
fn materialize_auxiliary_document(
    world_rc: &Rc<RefCell<World>>,
    frame: FrameId,
    opener: crate::protocol::OpenerPolicy,
    new_frames: &Rc<RefCell<Vec<CreatedFrame>>>,
) {
    let (runtime, url, origin, policy, sandbox, init_scripts, bindings, viewport) = {
        let world = world_rc.borrow();
        let doc = world.doc();
        let win = world.win();
        (
            world.runtime.clone(),
            doc.document_url.clone(),
            doc.origin.clone(),
            doc.policy.clone(),
            doc.sandbox.auxiliary_flags(),
            Rc::clone(&win.init_scripts),
            Rc::clone(&win.bindings),
            win.viewport_size.get(),
        )
    };
    let mut document = Document::with_shared(frame, &runtime, &init_scripts, &bindings);
    document.set_viewport_size(viewport);
    // https://html.spec.whatwg.org/multipage/document-sequences.html#creating-a-new-auxiliary-browsing-context
    {
        let world_handle = document.world();
        let mut world = world_handle.borrow_mut();
        world.doc_mut().policy = policy;
        world.install_sandbox(sandbox);
    }
    let origin = if sandbox.origin_is_opaque() {
        url::Origin::new_opaque()
    } else {
        origin
    };
    document.load_about_blank_tree(
        Some(&url),
        &world_rc.borrow().cookie_url(),
        Some(InheritedOrigin {
            origin,
            domain: Rc::clone(&world_rc.borrow().doc().domain),
        }),
    );
    if opener == crate::protocol::OpenerPolicy::NoReferrer {
        document.set_document_referrer(None);
    }
    new_frames
        .borrow_mut()
        .push(CreatedFrame::Auxiliary { frame, document });
}

// https://html.spec.whatwg.org/multipage/browsers.html#determining-the-creation-sandboxing-flags
pub(crate) fn sandboxing_flags(
    world_rc: &Rc<RefCell<World>>,
    container: crate::dom::tree::NodeId,
) -> crate::protocol::SandboxFlags {
    let world = world_rc.borrow();
    let sandbox = world.doc().sandbox;
    let Some(parsed) = world.document(container) else {
        return sandbox;
    };
    let base = &parsed.document.base;
    if !crate::dom::tree::is_html_element(base, container.node, "iframe") {
        return sandbox;
    }
    let attribute = crate::dom::tree::attr(base, container.node, "sandbox");
    crate::protocol::SandboxFlags::parse(attribute).union(sandbox)
}

enum ParseStep {
    Base(crate::dom::tree::NodeId),
    Meta(crate::dom::tree::NodeId),
    Script(crate::dom::tree::NodeId),
}

type SubresourceKey = (Url, Option<blitz_traits::net::CorsSetting>);

// https://fetch.spec.whatwg.org/#concept-request-client
struct BlitzRequest {
    handler: crate::render::CountingHandler,
    key: blitz_dom::ImageKey,
    referrer_policy: Option<blitz_traits::net::ReferrerPolicy>,
    node: Option<crate::dom::tree::NodeId>,
    destination: Option<crate::csp::Destination>,
}

pub(crate) use net::about_blank;

#[expect(
    clippy::struct_excessive_bools,
    reason = "each bool mirrors an HTML document or parser flag"
)]
pub(crate) struct Document {
    services: Arc<dyn BrowserServices>,
    world: Rc<RefCell<World>>,
    js_runtime: JsRuntimeHandle,
    wake: Arc<Notify>,
    net_provider: std::sync::Arc<crate::render::TinyNetProvider>,
    blitz_shell: std::sync::Arc<crate::render::TinyShell>,
    #[cfg(test)]
    blitz_resolve_passes: u64,
    image_settle_rx: std::sync::mpsc::Receiver<blitz_dom::ImageSettlement>,
    blitz_fetch_rx:
        std::sync::mpsc::Receiver<(crate::render::BlitzFetch, crate::render::CountingHandler)>,
    blitz_requests: HashMap<u64, BlitzRequest>,
    // https://drafts.csswg.org/css-fonts-4/#font-face-loading
    // https://html.spec.whatwg.org/multipage/semantics.html#the-link-element
    blitz_response_pending: bool,
    // https://html.spec.whatwg.org/multipage/links.html#link-type-stylesheet
    // https://drafts.csswg.org/css-fonts-4/#font-face-rule
    subresource_bytes: HashMap<SubresourceKey, Vec<u8>>,
    // https://w3c.github.io/webappsec-referrer-policy/#integration-with-css
    style_policies: HashMap<String, net::ReferrerPolicy>,
    frame: FrameId,
    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName
    committed: bool,
    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName
    name_resolved: bool,
    // https://chromedevtools.github.io/devtools-protocol/tot/Emulation/#method-setDeviceMetricsOverride
    pub(crate) viewport_size: (u32, u32),
    shared: SharedHandle,
    url: Url,
    tasks: crate::task_queue::TaskQueue<Task>,
    toggles: crate::task_queue::ToggleCoalescer,
    ui_events: UiEventQueue,
    timers: crate::timers::TimerQueue<Option<(crate::task_queue::RealmId, PageTimerId)>>,
    dials: DialLedger,
    // https://html.spec.whatwg.org/multipage/images.html#update-the-image-data
    deferred_image_events: VecDeque<(crate::task_queue::TaskSource, Task)>,
    // https://html.spec.whatwg.org/multipage/images.html#update-the-image-data
    // https://html.spec.whatwg.org/multipage/parsing.html#delay-the-load-event
    image_load_delays: HashMap<crate::dom::tree::NodeId, LoadDelayGuard>,
    // https://html.spec.whatwg.org/multipage/images.html#updating-the-image-data
    connected_images: HashSet<crate::dom::tree::NodeId>,
    // https://html.spec.whatwg.org/multipage/semantics.html#the-link-element
    linked_sheets: HashMap<crate::dom::tree::NodeId, String>,
    stylesheet_generation: u64,
    // https://html.spec.whatwg.org/multipage/iframe-embed-object.html#process-the-iframe-attributes
    initial_blank: bool,
    // https://html.spec.whatwg.org/multipage/parsing.html#delay-the-load-event
    pending_frame_loads: HashMap<crate::dom::tree::NodeId, LoadDelayGuard>,
    frame_traversals: Vec<i32>,
    history_updates: Vec<HistoryUpdate>,
    // https://html.spec.whatwg.org/multipage/document-sequences.html#document-tree-child-navigable
    new_frames: Rc<RefCell<Vec<CreatedFrame>>>,
    load_fired: bool,
    refresh: RefreshState,
    events: Vec<RendererEvent>,
    events_overflowed: bool,
    js: Option<crate::js::JsRealm>,
    // https://chromedevtools.github.io/devtools-protocol/tot/Page/#method-createIsolatedWorld
    isolated_js: HashMap<crate::task_queue::RealmId, (Rc<RefCell<World>>, crate::js::JsRealm)>,
    isolated_names: HashMap<String, crate::task_queue::RealmId>,
    js_timers: HashMap<(crate::task_queue::RealmId, PageTimerId), TimerId>,
    load_delay: LoadDelay,
    init_scripts_deferred: bool,
    defer_js: bool,
    active_parse: Option<ActiveParse>,
    stream_publish: Option<bool>,
    executed_scripts: HashSet<crate::dom::tree::NodeId>,
    parser_eof: bool,
    parser_owner: ParserOwner,
    decoder: Option<dial::ResponseDecoder>,
    response: ResponseStage,
    csp: CspState,
    // https://html.spec.whatwg.org/multipage/scripting.html#prepare-the-script-element
    parser_blocked_script: Option<crate::dom::tree::NodeId>,
    deferred_modules: Vec<DeferredModule>,
    stop: Arc<crate::js::Stop>,
}

enum DeferredModule {
    Ready {
        element: crate::dom::tree::NodeId,
        source: crate::js::ScriptElementSource,
    },
    Blocked(crate::dom::tree::NodeId),
    Failed,
}

impl Drop for Document {
    fn drop(&mut self) {
        self.release();
    }
}

// https://html.spec.whatwg.org/multipage/semantics.html#attr-meta-http-equiv-refresh
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

// https://html.spec.whatwg.org/multipage/browsers.html#relaxing-the-same-origin-restriction
pub(crate) struct InheritedOrigin {
    pub(crate) origin: url::Origin,
    pub(crate) domain: Rc<RefCell<Option<String>>>,
}

fn document_task_sink(
    tasks: crate::task_queue::TaskQueue<Task>,
    toggles: crate::task_queue::ToggleCoalescer,
) -> crate::task_queue::TaskSink {
    crate::task_queue::TaskSink::new(move |source, task| match task {
        crate::task_queue::RealmTask::SelectionChanged(id) => {
            tasks.push(source, Task::SelectionChanged(id));
        }
        crate::task_queue::RealmTask::ToggleEvent {
            id,
            old_state,
            new_state,
        } => {
            if toggles.push(id, old_state, new_state) {
                tasks.push(source, Task::ToggleEvent(id));
            }
        }
        crate::task_queue::RealmTask::DialogClose(id) => {
            tasks.push(source, Task::DialogClose(id));
        }
        crate::task_queue::RealmTask::RejectionTasks(realm) => {
            tasks.push(source, Task::RejectionTasks(realm));
        }
    })
}

impl Document {
    pub(crate) fn with_shared(
        frame: FrameId,
        runtime: &FrameRuntime,
        init_scripts: &Rc<RefCell<Vec<(u64, String)>>>,
        bindings: &Rc<RefCell<Vec<String>>>,
    ) -> Self {
        let document_url = about_blank();
        runtime.shared.borrow_mut().begin_document(frame);
        let world = Rc::new(RefCell::new(World::new(
            document_url.clone(),
            frame,
            runtime,
            Rc::clone(init_scripts),
            Rc::clone(bindings),
        )));
        runtime.registry.borrow_mut().insert_frame(frame, &world);
        let tasks = crate::task_queue::TaskQueue::default();
        let toggles = crate::task_queue::ToggleCoalescer::default();
        world
            .borrow_mut()
            .set_task_sink(document_task_sink(tasks.clone(), toggles.clone()));
        let new_frames: Rc<RefCell<Vec<CreatedFrame>>> = Rc::new(RefCell::new(Vec::new()));
        world.borrow_mut().set_frame_host(frame_host(&new_frames));
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
            blitz_response_pending: false,
            subresource_bytes: HashMap::new(),
            style_policies: HashMap::new(),
            frame,
            committed: false,
            name_resolved: false,
            viewport_size: crate::engine::DEFAULT_VIEWPORT,
            shared: runtime.shared.clone(),
            url: document_url,
            tasks,
            toggles,
            ui_events: UiEventQueue::default(),
            timers: crate::timers::TimerQueue::default(),
            dials: DialLedger::new(Arc::clone(&runtime.stop), Arc::clone(&runtime.wake)),
            deferred_image_events: VecDeque::new(),
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
            refresh: RefreshState::default(),
            events: Vec::new(),
            events_overflowed: false,
            js: None,
            isolated_js: HashMap::new(),
            isolated_names: HashMap::new(),
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
            response: ResponseStage::default(),
            csp: CspState::default(),
            parser_blocked_script: None,
            deferred_modules: Vec::new(),
            stop: Arc::clone(&runtime.stop),
        }
    }

    pub(crate) fn world(&self) -> Rc<RefCell<World>> {
        Rc::clone(&self.world)
    }

    // https://html.spec.whatwg.org/multipage/webstorage.html#the-localstorage-attribute
    pub(crate) fn storage_key(&self) -> Option<crate::protocol::StorageKey> {
        self.world.borrow().storage_origin()
    }

    pub(crate) fn document_root(&self) -> Option<crate::dom::tree::NodeId> {
        self.world
            .borrow()
            .with_main_document(|parsed| crate::dom::tree::NodeId {
                document: parsed.id,
                node: parsed.document.base.root_node().id,
            })
    }

    // https://html.spec.whatwg.org/multipage/iframe-embed-object.html#attr-iframe-referrerpolicy
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

    // https://html.spec.whatwg.org/multipage/iframe-embed-object.html#attr-iframe-src
    #[must_use]
    pub(crate) fn frame_src(&self, container: crate::dom::tree::NodeId) -> Option<String> {
        let world = self.world.borrow();
        let value = world.document(container).and_then(|parsed| {
            crate::dom::tree::attr(&parsed.document.base, container.node, "src").map(str::to_owned)
        })?;
        (!value.is_empty()).then_some(value)
    }

    // https://html.spec.whatwg.org/multipage/iframe-embed-object.html#attr-iframe-srcdoc
    #[must_use]
    pub(crate) fn frame_srcdoc(&self, container: crate::dom::tree::NodeId) -> Option<String> {
        let world = self.world.borrow();
        world.document(container).and_then(|parsed| {
            crate::dom::tree::attr(&parsed.document.base, container.node, "srcdoc")
                .map(str::to_owned)
        })
    }

    // https://html.spec.whatwg.org/multipage/urls-and-fetching.html#parse-a-url
    #[must_use]
    pub(crate) fn resolve_frame_url(&self, spec: &str) -> Option<Url> {
        Url::parse(spec)
            .or_else(|_| self.base_url().join(spec))
            .ok()
    }

    // https://w3c.github.io/FileAPI/#blob-url-store
    #[must_use]
    pub(crate) fn object_url_contents(&self, url: &str) -> Option<Arc<str>> {
        self.world.borrow().object_url_contents(url)
    }

    #[must_use]
    pub(crate) fn object_url_type(&self, url: &str) -> Option<Arc<str>> {
        self.world.borrow().object_url_type(url)
    }

    #[must_use]
    pub(crate) fn object_url_origin(&self, url: &str) -> Option<url::Origin> {
        self.world.borrow().object_url_origin(url)
    }

    // https://html.spec.whatwg.org/multipage/iframe-embed-object.html#process-the-iframe-attributes
    pub(crate) fn adopt_pending_frames(&mut self) {
        self.adopt_pending_frames_before_node(None);
    }

    // https://html.spec.whatwg.org/multipage/iframe-embed-object.html#the-iframe-element:html-element-insertion-steps
    fn adopt_pending_frames_before_node(&mut self, before: Option<crate::dom::tree::NodeId>) {
        let created = self.world.borrow_mut().register_pending_frames(before);
        self.materialize_pending_frames();
        for container in created {
            // https://html.spec.whatwg.org/multipage/parsing.html#delay-the-load-event
            self.mark_frame_load_pending(container);
        }
    }

    fn materialize_pending_frames(&mut self) {
        let pending = self.world.borrow_mut().take_pending_frames();
        if pending.is_empty() {
            return;
        }
        let base_url = self.base_url();
        let (runtime, referrer_url, origin, init_scripts, bindings, viewport) = {
            let world = self.world.borrow();
            let doc = world.doc();
            let win = world.win();
            (
                world.runtime.clone(),
                doc.referrer_url.clone(),
                doc.origin.clone(),
                Rc::clone(&win.init_scripts),
                Rc::clone(&win.bindings),
                win.viewport_size.get(),
            )
        };
        let mut loaded = Vec::with_capacity(pending.len());
        let inherited = self.world.borrow().doc().policy.clone();
        let top_level = self.world.borrow().top_level_origin();
        for (frame, container) in pending {
            // https://html.spec.whatwg.org/multipage/browsers.html#determining-the-creation-sandboxing-flags
            let sandbox = sandboxing_flags(&self.world, container);
            let origin = if sandbox.origin_is_opaque() {
                url::Origin::new_opaque()
            } else {
                origin.clone()
            };
            let mut document = Document::with_shared(frame, &runtime, &init_scripts, &bindings);
            // https://html.spec.whatwg.org/multipage/iframe-embed-object.html#the-iframe-element:html-element-insertion-steps
            document.set_viewport_size(viewport);
            {
                let world_handle = document.world();
                let mut world = world_handle.borrow_mut();
                {
                    let mut doc = world.doc_mut();
                    doc.policy = inherited.clone();
                    doc.sandbox = sandbox;
                    doc.top_level = Some(top_level.clone());
                }
                world.set_parent_context(&self.world.borrow());
            }
            // https://html.spec.whatwg.org/multipage/iframe-embed-object.html#the-iframe-element:html-element-insertion-steps
            let cookie_url = self.world.borrow().cookie_url();
            document.load_about_blank_tree(
                Some(&base_url),
                &cookie_url,
                Some(InheritedOrigin {
                    origin,
                    domain: Rc::clone(&self.world.borrow().doc().domain),
                }),
            );
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

    pub(crate) fn flush_frame_proxy_sets(&mut self, child: FrameId) {
        self.fire_js(|js| js.flush_frame_sets(child.get()));
        self.adopt_js_work();
    }

    #[must_use]
    pub(crate) const fn has_realm(&self) -> bool {
        self.js.is_some()
    }

    #[must_use]
    pub(crate) fn needs_proxy_sets_replay(&self) -> bool {
        self.world.borrow().needs_proxy_sets_replay()
    }

    // https://html.spec.whatwg.org/multipage/window-object.html#windowproxy-set
    #[must_use]
    pub(crate) fn awaits_proxy_replay(&self) -> bool {
        self.frame.get() != FrameId::MAIN.get() && self.needs_proxy_sets_replay()
    }

    #[must_use]
    pub(crate) fn has_pending_frame_load(&self) -> bool {
        self.dials.has_pending_frame_load()
    }

    pub(crate) fn mark_proxy_sets_replayed(&mut self) {
        self.world.borrow_mut().mark_proxy_sets_replayed();
    }

    pub(crate) fn take_new_frames(&mut self) -> Vec<CreatedFrame> {
        std::mem::take(&mut *self.new_frames.borrow_mut())
    }

    // https://html.spec.whatwg.org/multipage/browsers.html#determining-the-origin
    #[must_use]
    pub(crate) fn inherited_url(&self) -> Url {
        self.url.clone()
    }

    // https://html.spec.whatwg.org/multipage/browsers.html#creating-a-new-browsing-context
    pub(crate) fn set_document_referrer(&mut self, referrer: Option<Url>) {
        self.world.borrow().doc_mut().document_referrer = referrer;
    }

    pub(crate) fn local_document_state(&self) -> LocalDocumentState {
        let world = self.world.borrow();
        let doc = world.doc();
        LocalDocumentState {
            url: self.url.clone(),
            base_url: doc.base_url.clone(),
            cookie_url: doc.inherited_cookie_url.clone(),
            referrer: doc.document_referrer.clone(),
            referrer_url: doc.referrer_url.clone(),
            origin: doc.origin.clone(),
            policy: doc.policy.clone(),
            sandbox: doc.sandbox,
        }
    }

    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#apply-the-history-step
    pub(crate) fn restore_local_document(
        &mut self,
        state: &LocalDocumentState,
        srcdoc: Option<&str>,
    ) {
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
                InheritedOrigin {
                    origin: state.origin.clone(),
                    domain: Rc::new(RefCell::new(None)),
                },
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
            Some(InheritedOrigin {
                origin: state.origin.clone(),
                domain: Rc::new(RefCell::new(None)),
            }),
        );
        self.set_document_referrer(state.referrer.clone());
        self.world.borrow().doc_mut().policy = state.policy.clone();
    }

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
        for element in disconnected {
            self.disconnect_image(element);
        }
        removed
    }

    pub(crate) fn take_frame_navigations(&mut self) -> Vec<FrameNavigation> {
        self.world.borrow_mut().take_frame_navigations()
    }

    pub(crate) fn take_frame_traversals(&mut self) -> Vec<i32> {
        std::mem::take(&mut self.frame_traversals)
    }

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

    // https://html.spec.whatwg.org/multipage/images.html#update-the-image-data
    fn disconnect_image(&mut self, element: crate::dom::tree::NodeId) {
        self.world
            .borrow()
            .doc_mut()
            .state
            .creator_stacks
            .remove(&element);
    }

    // https://w3c.github.io/webappsec-upgrade-insecure-requests/#upgrade-request
    fn queue_dial(&mut self, mut dial: QueuedDial) {
        // https://github.com/privacycg/CHIPS#computing-the-cookie-partition-key
        dial.req.partition_key = self.world.borrow().partition_key();
        if self
            .world
            .borrow()
            .doc()
            .policy
            .csp
            .upgrades_insecure_requests()
        {
            let nested = matches!(
                dial.context,
                DialContext::FrameLoad {
                    embedder: Some(_),
                    ..
                }
            );
            let upgrades =
                if dial.req.policy.kind() == crate::protocol::DialKind::FrameLoad && !nested {
                    // https://w3c.github.io/webappsec-upgrade-insecure-requests/#upgrade-request
                    Self::same_host_port(&dial.req.url, &self.url)
                } else {
                    true
                };
            if upgrades {
                Self::upgrade_scheme(&mut dial.req.url);
            }
        }
        // https://w3c.github.io/webappsec-upgrade-insecure-requests/#upgrade-insecure-requests-http-request-header-field
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

    // https://url.spec.whatwg.org/#concept-url-host
    fn same_host_port(left: &Url, right: &Url) -> bool {
        left.host_str() == right.host_str()
            && left.port_or_known_default() == right.port_or_known_default()
    }

    fn upgrade_scheme(url: &mut Url) {
        if url.scheme() == "http" && url.set_scheme("https").is_ok() {}
    }

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
            started: std::time::Instant::now(),
            ancestors,
            integrity: None,
        });
    }

    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#aborting-navigation
    pub(crate) fn cancel_frame_navigation(&mut self) {
        self.dials.cancel_frame_loads();
    }

    // https://html.spec.whatwg.org/multipage/webappapis.html#secure-contexts
    pub(crate) fn set_creator_secure_context(&mut self, creator: Option<bool>) {
        self.response.creator_secure_context = creator;
    }

    // https://html.spec.whatwg.org/multipage/webappapis.html#concept-settings-object-cross-origin-isolated-capability
    pub(crate) fn set_creator_cross_origin_isolated(&mut self, creator: Option<bool>) {
        self.response.creator_cross_origin_isolated = creator;
    }

    fn apply_blank_url(
        &mut self,
        inherited_url: Option<&Url>,
        cookie_url: &Url,
        inherited: Option<InheritedOrigin>,
    ) {
        let Some(base) = inherited_url else {
            return;
        };
        let blank = about_blank();
        // https://html.spec.whatwg.org/multipage/browsers.html#relaxing-the-same-origin-restriction
        let shares_domain =
            inherited.is_some() && !self.world.borrow().doc().sandbox.origin_is_opaque();
        let (origin, domain) = match inherited {
            Some(inherited) => (
                self.sandboxed_origin(Some(inherited.origin)),
                inherited.domain,
            ),
            None => (None, Rc::new(RefCell::new(None))),
        };
        self.commit_document_url(blank, origin);
        if shares_domain {
            self.world.borrow().doc_mut().domain = domain;
        }
        let world = self.world.borrow();
        let mut doc = world.doc_mut();
        doc.base_url = Some(base.clone());
        // https://html.spec.whatwg.org/multipage/dom.html#dom-document-cookie
        doc.inherited_cookie_url = Some(cookie_url.clone());
    }

    // https://html.spec.whatwg.org/multipage/iframe-embed-object.html#process-the-iframe-attributes
    // https://html.spec.whatwg.org/multipage/browsers.html#determining-the-origin
    // https://html.spec.whatwg.org/multipage/urls-and-fetching.html#fallback-base-url
    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#session-history-entry
    pub(crate) fn load_about_blank(
        &mut self,
        inherited_url: Option<&Url>,
        cookie_url: &Url,
        inherited: Option<InheritedOrigin>,
    ) {
        self.apply_blank_url(inherited_url, cookie_url, inherited);
        // https://html.spec.whatwg.org/multipage/browsers.html#creating-a-new-browsing-context
        self.world.borrow().doc_mut().document_referrer = inherited_url.cloned();
        self.csp.http_status = 0;
        self.load_html("");
        self.ensure_js_ok();
    }

    // https://html.spec.whatwg.org/multipage/iframe-embed-object.html#attr-iframe-srcdoc
    // https://html.spec.whatwg.org/multipage/urls-and-fetching.html#fallback-base-url
    pub(crate) fn load_frame_srcdoc(
        &mut self,
        base: &Url,
        cookie_url: &Url,
        referrer_url: &Url,
        inherited: InheritedOrigin,
        policy: &PolicyContainer,
        markup: &str,
    ) {
        let url = Url::parse("about:srcdoc").expect("about:srcdoc parses");
        self.csp.http_status = 0;
        // https://html.spec.whatwg.org/multipage/browsers.html#relaxing-the-same-origin-restriction
        let shares_domain = !self.world.borrow().doc().sandbox.origin_is_opaque();
        let domain = inherited.domain;
        let origin = self.sandboxed_origin(Some(inherited.origin));
        self.commit_document_url(url, origin);
        if shares_domain {
            self.world.borrow().doc_mut().domain = domain;
        }
        {
            let world = self.world.borrow();
            let mut doc = world.doc_mut();
            doc.base_url = Some(base.clone());
            // https://html.spec.whatwg.org/multipage/dom.html#dom-document-cookie
            doc.inherited_cookie_url = Some(cookie_url.clone());
            doc.referrer_url = referrer_url.clone();
            // https://html.spec.whatwg.org/multipage/browsers.html#creating-a-new-browsing-context
            doc.document_referrer = Some(referrer_url.clone());
        }
        self.response.inherited_policy = Some(policy.clone());
        self.initial_blank = false;
        self.load_html(markup);
    }

    // https://html.spec.whatwg.org/multipage/iframe-embed-object.html#process-the-iframe-attributes
    pub(crate) fn load_about_blank_tree(
        &mut self,
        inherited_url: Option<&Url>,
        cookie_url: &Url,
        inherited: Option<InheritedOrigin>,
    ) {
        self.apply_blank_url(inherited_url, cookie_url, inherited);
        // https://html.spec.whatwg.org/multipage/browsers.html#creating-a-new-browsing-context
        self.world.borrow().doc_mut().document_referrer = inherited_url.cloned();
        self.defer_js = true;
        self.load_html("");
        self.defer_js = false;
        self.initial_blank = true;
    }

    #[must_use]
    pub(crate) fn is_initial_blank(&self) -> bool {
        self.initial_blank
    }

    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#javascript-protocol
    pub(crate) fn load_javascript_frame(&mut self, script: &str, blocked: bool) {
        if self.js.is_none() {
            self.ensure_js_ok();
        }
        self.eval_frame_script(script, blocked);
    }

    // https://w3c.github.io/webappsec-csp/#should-block-navigation-request
    // https://w3c.github.io/webappsec-csp/#report-violation
    #[must_use]
    pub(crate) fn javascript_url_violations(&self, source: &str) -> Vec<crate::csp::Violation> {
        let world = self.world.borrow();
        Self::navigation_script_violations(&world.doc().policy.csp, &self.url, source)
    }

    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#javascript-protocol
    pub(crate) fn eval_frame_script(&mut self, script: &str, blocked: bool) {
        // https://html.spec.whatwg.org/multipage/browsing-the-web.html#javascript-protocol
        if script.is_empty() || self.world.borrow().doc().sandbox.scripts_disabled() || blocked {
            return;
        }
        match self.eval(script) {
            Ok(result) if !result.is_empty() => self.load_html(&result),
            Ok(_) => {}
            Err(_) => self.record_event(RendererEvent::ScriptFailed),
        }
    }

    // https://html.spec.whatwg.org/multipage/browsers.html#sandboxed-origin-browsing-context-flag
    fn sandboxed_origin(&self, origin: Option<url::Origin>) -> Option<url::Origin> {
        if self.world.borrow().doc().sandbox.origin_is_opaque() {
            Some(url::Origin::new_opaque())
        } else {
            origin
        }
    }

    fn set_document_url(&mut self, url: Url, origin: Option<url::Origin>) {
        let mut world = self.world.borrow_mut();
        match origin {
            Some(origin) => world.set_url_with_origin(url.clone(), origin),
            None => world.set_url(url.clone()),
        }
        drop(world);
        self.url = url;
    }

    // https://html.spec.whatwg.org/multipage/webappapis.html#secure-contexts
    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName
    fn commit_document_url(&mut self, url: Url, origin: Option<url::Origin>) {
        let new_origin = origin.clone().unwrap_or_else(|| url.origin());
        let frame = self.frame;
        let previous = self.world.borrow().doc().origin.clone();
        let first_document = !self.committed;
        let resolved_by_browser = std::mem::take(&mut self.name_resolved);
        self.committed = true;
        let creation_url = url.clone();
        self.set_document_url(url, origin);
        // https://html.spec.whatwg.org/multipage/document-lifecycle.html#concept-document-creation-url
        self.world.borrow().doc_mut().creation_url = creation_url;
        if first_document || resolved_by_browser {
            return;
        }
        // https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName
        let cleared = self
            .world
            .borrow()
            .shared()
            .borrow_mut()
            .tree
            .reset_name_on_cross_origin_navigation(frame, previous == new_origin);
        if cleared && frame == FrameId::MAIN {
            let services = self.world.borrow().runtime.services.clone();
            services.set_window_name(crate::protocol::NameChange::Reset);
        }
    }

    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#navigate-to-a-fragment
    pub(crate) fn queue_fragment_navigation(&self, url: Url) {
        let world = self.world.borrow();
        let (old_url, handling) = {
            let mut doc = world.doc_mut();
            let old_url = doc.document_url.as_str().to_owned();
            // https://html.spec.whatwg.org/multipage/browsing-the-web.html#navigate-to-a-fragment
            let handling = if doc.document_url == url {
                crate::protocol::HistoryHandling::Replace
            } else {
                crate::protocol::HistoryHandling::Push
            };
            doc.document_url = url.clone();
            doc.history.apply("null".to_owned(), handling);
            (old_url, handling)
        };
        world
            .outbox
            .borrow_mut()
            .push_back(crate::js::world::RealmRequest::FragmentNavigation {
                old_url,
                url,
                handling,
            });
    }

    // https://drafts.csswg.org/selectors-4/#the-target-pseudo
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
            let (origin, domain) = {
                let world = self.world.borrow();
                let doc = world.doc();
                (doc.origin.clone(), Rc::clone(&doc.domain))
            };
            self.set_document_url(url, Some(origin));
            // https://html.spec.whatwg.org/multipage/browsing-the-web.html#navigate-to-a-fragment
            self.world.borrow().doc_mut().domain = domain;
        }
    }

    pub(crate) fn set_history(&mut self, history: &crate::protocol::HistorySnapshot) {
        self.world.borrow().doc_mut().history = history.clone();
    }

    // https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-history-length
    pub(crate) fn update_history_mirror(&mut self, history: &crate::protocol::HistorySnapshot) {
        self.set_history(history);
        if let Some(js) = self.js.as_ref() {
            let _ = js.restore_history(history.state.as_deref(), history.length);
        }
    }

    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#update-document-for-history-step-application
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
        let previous_history = self.world.borrow().doc().history.clone();
        self.apply_document_url_same_origin(url.as_str());
        self.world.borrow().doc_mut().history = history.clone();
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
                self.world.borrow().doc_mut().history = previous_history;
                return Err(TabError::from(error));
            }
        }
        Ok(())
    }

    // https://html.spec.whatwg.org/multipage/web-messaging.html#posted-message-task-source
    pub(crate) fn push_window_message(&mut self, message: WindowMessage) {
        self.tasks.push(
            crate::task_queue::TaskSource::PostedMessage,
            Task::WindowMessage(message),
        );
    }

    pub(crate) fn matches_document(&self, id: DocumentId) -> bool {
        self.shared.borrow().document_id(self.frame) == Some(id)
    }

    // https://html.spec.whatwg.org/multipage/webstorage.html#concept-storage-broadcast
    pub(crate) fn push_storage_event(&mut self, event: crate::storage::PendingStorageEvent) {
        self.tasks.push(
            crate::task_queue::TaskSource::DomManipulation,
            Task::StorageEvent(event),
        );
    }

    // https://html.spec.whatwg.org/multipage/web-messaging.html#posted-message-task-source
    pub(crate) fn push_remote_message(&mut self, message: crate::protocol::PostedMessage) {
        self.tasks.push(
            crate::task_queue::TaskSource::PostedMessage,
            Task::RemoteMessage(message),
        );
    }

    // https://html.spec.whatwg.org/multipage/web-messaging.html#broadcasting-to-other-browsing-contexts
    pub(crate) fn push_broadcast_message(
        &mut self,
        origin: String,
        name: String,
        payload: String,
        source: Option<u64>,
    ) {
        self.tasks.push(
            crate::task_queue::TaskSource::PostedMessage,
            Task::BroadcastMessage {
                origin,
                name,
                payload,
                source,
            },
        );
    }

    pub(crate) fn push_port_message(&mut self, endpoint: u64, payload: String, ports: Vec<u64>) {
        self.tasks.push(
            crate::task_queue::TaskSource::PostedMessage,
            Task::PortMessage {
                endpoint,
                payload,
                ports,
            },
        );
    }

    pub(crate) fn push_port_closed(&mut self, endpoint: u64) {
        self.tasks.push(
            crate::task_queue::TaskSource::PostedMessage,
            Task::PortClosed { endpoint },
        );
    }

    pub(crate) fn push_worker_message(
        &mut self,
        worker: crate::worker::WorkerId,
        payload: String,
        ports: Vec<u64>,
    ) {
        self.tasks.push(
            crate::task_queue::TaskSource::PostedMessage,
            Task::WorkerMessage {
                worker,
                payload,
                ports,
            },
        );
    }

    pub(crate) fn push_worker_error(&mut self, error: crate::worker::WorkerError) {
        self.tasks.push(
            crate::task_queue::TaskSource::DomManipulation,
            Task::WorkerError(error),
        );
    }

    pub(crate) fn push_worker_load_failed(&mut self, worker: crate::worker::WorkerId) {
        self.tasks.push(
            crate::task_queue::TaskSource::DomManipulation,
            Task::WorkerLoadFailed(worker),
        );
    }

    // https://w3c.github.io/webappsec-csp/#report-violation
    pub(crate) fn push_csp_violations(&mut self, violations: Vec<crate::csp::Violation>) {
        self.tasks.push(
            crate::task_queue::TaskSource::DomManipulation,
            Task::CspViolation(violations),
        );
    }

    fn deliver_remote_message(&mut self, message: &crate::protocol::PostedMessage) {
        // https://html.spec.whatwg.org/multipage/web-messaging.html#window-post-message-steps
        if !message
            .target_origin
            .admits(&self.world.borrow().doc().origin)
        {
            return;
        }
        if !self.ensure_js_ok() {
            return;
        }
        let origin = message.origin.to_string();
        self.fire_js(|js| js.deliver_remote_message(&origin, &message.payload));
        self.adopt_js_work();
    }

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

    // https://html.spec.whatwg.org/multipage/webstorage.html#concept-storage-broadcast
    fn deliver_storage_event(&mut self, event: &crate::storage::PendingStorageEvent) {
        if !self.ensure_js_ok() {
            return;
        }
        self.fire_js(|js| js.fire_storage_event(event));
        self.adopt_js_work();
    }

    fn deliver_window_message(&mut self, message: &WindowMessage) {
        // https://html.spec.whatwg.org/multipage/web-messaging.html#window-post-message-steps
        if !message
            .target_origin
            .admits(&self.world.borrow().doc().origin)
        {
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
        // https://html.spec.whatwg.org/multipage/web-messaging.html#message-port-post-message-steps
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

    // https://html.spec.whatwg.org/multipage/web-messaging.html#transfer-receiving-steps
    fn endpoint_moved(&self, endpoint: u64) -> bool {
        let shared = self.shared.borrow();
        shared.ports.owner(endpoint) != Some(self.frame)
    }

    #[must_use]
    pub(crate) fn url(&self) -> &Url {
        &self.url
    }

    pub(crate) fn eval(&mut self, source: &str) -> Result<String, TabError> {
        // https://html.spec.whatwg.org/multipage/iframe-embed-object.html#the-iframe-element:html-element-insertion-steps
        if !self.world.borrow().doc().parser_active {
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

    // https://chromedevtools.github.io/devtools-protocol/tot/Page/#method-createIsolatedWorld
    pub(in crate::document) fn isolated_realm(&self, name: &str) -> Option<&crate::js::JsRealm> {
        let realm_id = self.isolated_names.get(name)?;
        self.isolated_js.get(realm_id).map(|(_, realm)| realm)
    }

    // https://chromedevtools.github.io/devtools-protocol/tot/Page/#method-createIsolatedWorld
    fn ensure_isolated(&mut self, name: &str) -> Result<(), TabError> {
        if self.isolated_names.contains_key(name) {
            return Ok(());
        }
        let isolated_world = Rc::new(RefCell::new(World::new_isolated(
            &self.world.borrow(),
            name.to_owned(),
        )));
        let realm_id = isolated_world.borrow().realm_id();
        let realm = crate::js::JsRealm::new(
            &self.js_runtime,
            Rc::clone(&isolated_world),
            Arc::clone(&self.stop),
        )
        .map_err(TabError::from)?;
        self.isolated_names.insert(name.to_owned(), realm_id);
        self.isolated_js.insert(realm_id, (isolated_world, realm));
        Ok(())
    }

    pub(crate) fn execute_script_deadline(
        &mut self,
        source: crate::ScriptSource<&str>,
        deadline: Option<WallClock>,
    ) -> Result<ScriptValue, TabError> {
        self.ensure_js()?;
        if let crate::ScriptSource::Isolated {
            world: name,
            source,
        } = source
        {
            self.ensure_isolated(name)?;
            let Some(isolated_realm) = self.isolated_realm(name) else {
                return Err(TabError::Script(ScriptFailure::HostMissing));
            };
            let out = isolated_realm
                .eval_value_deadline(crate::ScriptSource::Browser(source), deadline)
                .map_err(TabError::from);
            self.adopt_js_work();
            return out;
        }
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

    pub(crate) fn mount(&mut self, mount: &Mount) {
        self.set_history(&mount.head.history);
        self.set_document_referrer(None);
        self.install_tab_state(&mount.tab);
        self.install_response(&mount.head, &mount.body, None);
    }

    // https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-name
    // https://html.spec.whatwg.org/multipage/browsers.html#popup-sandboxing-flag-set
    pub(crate) fn install_tab_state(&mut self, tab: &crate::protocol::TabState) {
        debug_assert_eq!(
            self.frame,
            FrameId::MAIN,
            "a tab's browsing-context state belongs to its top-level document"
        );
        // https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName
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
        self.world.borrow().win_mut().own_window = tab.id;
        self.set_creator_secure_context(tab.secure_context);
    }

    fn start_parser(
        &mut self,
        input: &str,
        eof: bool,
        owner: ParserOwner,
        decoder: Option<dial::ResponseDecoder>,
    ) {
        self.world.borrow().doc_mut().parser_active = true;
        self.parser_eof = eof;
        self.decoder = decoder;
        self.parser_owner = owner;
        self.response.reset_for_parse();
        self.active_parse = Some(ActiveParse {
            buffer: input.to_owned(),
            delay: self.load_delay.guard(),
        });
    }

    pub(crate) fn install_head(&mut self, head: &ResponseHead, origin: Option<url::Origin>) {
        self.reset_js_realm(RealmReset::Navigation);
        // https://w3c.github.io/webappsec-csp/#create-violation-for-global
        self.csp.http_status = head.source.status();
        self.set_reporting_endpoints(head.source.reporting_endpoints());
        if let Some((width, height)) = head.viewport {
            self.viewport_size = (width, height);
        }
        let policy = head.source.policy();
        // https://w3c.github.io/webappsec-csp/#sandbox
        if !policy.sandbox.is_empty() {
            let world = self.world.borrow();
            let mut doc = world.doc_mut();
            doc.sandbox = doc.sandbox.union(policy.sandbox);
        }
        self.install_response_url(head, origin);
        {
            let world = self.world.borrow();
            let mut doc = world.doc_mut();
            doc.base_url = None;
            doc.inherited_cookie_url = None;
        }
        self.start_parser(
            "",
            false,
            ParserOwner::Carrier,
            // https://fetch.spec.whatwg.org/#concept-header-extract-mime-type
            Some(dial::ResponseDecoder::new(
                head.content_type
                    .as_deref()
                    .and_then(crate::sniff::extract_mime_type),
            )),
        );
        let top_level = {
            let shared = self.shared.borrow();
            shared.tree.top_level(self.frame) == self.frame
        };
        self.response.stage_head(head, top_level);
        // https://html.spec.whatwg.org/multipage/document-lifecycle.html#creating-and-initializing-a-document-object
        if let Some(refresh) = head.source.refresh() {
            self.start_declarative_refresh(refresh);
        }
    }

    // https://w3c.github.io/FileAPI/#blob-url-entry
    // https://html.spec.whatwg.org/multipage/browsers.html#determining-the-origin
    fn install_response_url(&mut self, head: &ResponseHead, origin: Option<url::Origin>) {
        self.initial_blank = false;
        let origin = self.sandboxed_origin(origin);
        self.commit_document_url(head.url.clone(), origin);
    }

    fn install_response(&mut self, head: &ResponseHead, body: &[u8], origin: Option<url::Origin>) {
        self.install_head(head, origin);
        self.write_body(body);
        self.end_body();
    }

    pub(crate) fn install_frame_response(
        &mut self,
        head: &ResponseHead,
        body: &[u8],
        origin: Option<url::Origin>,
    ) {
        self.install_response(head, body, origin);
        // https://html.spec.whatwg.org/multipage/window-object.html#the-window-object
        if self.js.is_none() && self.shared.borrow().tree.parent(self.frame).is_none() {
            self.ensure_js_ok();
        }
    }

    // https://w3c.github.io/reporting/#header
    fn set_reporting_endpoints(&mut self, endpoints: Option<&str>) {
        self.csp.set_reporting_endpoints(endpoints);
    }

    pub(crate) fn write_body(&mut self, bytes: &[u8]) {
        self.response.push_sniff_bytes(bytes);
        let Some(decoder) = self.decoder.as_mut() else {
            return;
        };
        let text = decoder.push(bytes);
        self.write_text(&text);
    }

    pub(crate) fn write_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if let Some(parse) = self.active_parse.as_mut() {
            parse.buffer.push_str(text);
        }
    }

    pub(crate) fn end_body(&mut self) {
        if let Some(decoder) = self.decoder.take() {
            let (text, encoding) = decoder.finish();
            self.response.character_set = encoding;
            self.write_text(&text);
        }
        self.parser_eof = true;
        self.finish_parse();
    }

    pub(crate) fn abort_body(&mut self) {
        self.decoder = None;
        self.parser_eof = true;
        self.finish_parse();
    }

    // https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#document-open-steps
    pub(crate) fn reopen_document(&mut self) {
        self.reset_js_realm(RealmReset::Reopen);
        self.start_parser("", false, ParserOwner::Script, None);
    }

    pub(crate) fn load_html(&mut self, input: &str) {
        self.reset_js_realm(RealmReset::Navigation);
        self.start_parser(input, true, ParserOwner::Carrier, None);
        self.finish_parse();
    }

    fn register_document(&self, document: u32) {
        let registry = self.world.borrow().registry();
        registry.borrow_mut().insert_document(document, &self.world);
    }

    fn fire_js(
        &mut self,
        operation: impl FnOnce(&crate::js::JsRealm) -> Result<(), crate::js::JsError>,
    ) {
        let failed = self.js.as_ref().is_some_and(|js| operation(js).is_err());
        if failed {
            self.record_event(RendererEvent::ScriptFailed);
        }
    }

    // https://chromedevtools.github.io/devtools-protocol/tot/Page/#method-createIsolatedWorld
    fn fire_realm(
        &mut self,
        realm: crate::task_queue::RealmId,
        operation: impl FnOnce(&crate::js::JsRealm) -> Result<(), crate::js::JsError>,
    ) {
        let main_realm = self.world.borrow().realm_id();
        let failed = if main_realm == realm {
            self.js.as_ref().is_some_and(|js| operation(js).is_err())
        } else {
            self.isolated_js
                .get(&realm)
                .is_some_and(|(_, js)| operation(js).is_err())
        };
        if failed {
            self.record_event(RendererEvent::ScriptFailed);
        }
    }

    pub(crate) fn ensure_js_ok(&mut self) -> bool {
        if self.ensure_js().is_err() {
            self.record_event(RendererEvent::ScriptFailed);
            return false;
        }
        true
    }

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
                self.init_scripts_deferred = true;
            } else {
                self.run_init_scripts();
            }
        }
        Ok(())
    }

    fn run_init_scripts(&mut self) {
        let _turn = self.js.as_ref().map(crate::js::JsRealm::enter_turn);
        let scripts: Vec<String> = self
            .world
            .borrow()
            .win()
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

    // https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#method-addBinding
    pub(crate) fn run_binding(&mut self, name: &str) {
        if !self.ensure_js_ok() {
            return;
        }
        let failed = match self.js.as_ref() {
            Some(js) => js.add_binding(name).is_err(),
            None => true,
        };
        if failed {
            self.record_event(RendererEvent::ScriptFailed);
        }
    }

    fn reset_js_realm(&mut self, reason: RealmReset) {
        // https://html.spec.whatwg.org/multipage/webappapis.html#concept-task-runnable
        for task in self.tasks.take_all() {
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
            // https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#method-evaluate
            self.world
                .borrow()
                .win()
                .evaluation_replies
                .borrow_mut()
                .clear();
        }
        let old = std::mem::replace(
            &mut self.dials,
            DialLedger::new(Arc::clone(&self.stop), Arc::clone(&self.wake)),
        );
        old.terminate();
        self.load_delay = LoadDelay::default();
        if reason == RealmReset::Reopen {
            self.record_event(RendererEvent::Browser(BrowserEvent::RealmReplaced));
        }
        self.events.retain(|event| {
            !matches!(
                event,
                RendererEvent::Browser(BrowserEvent::Load | BrowserEvent::DomContentLoaded)
            )
        });
        // https://html.spec.whatwg.org/multipage/web-messaging.html#disentangle
        {
            let mut shared = self.shared.borrow_mut();
            shared.close_frame_ports(self.frame);
            shared.detach_child_frames(self.frame);
        }
        self.load_fired = false;
        if let Some(id) = self.refresh.reset() {
            self.timers.remove(id);
        }
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
        self.csp.clear_pending_events();
        self.image_load_delays.clear();
        self.connected_images.clear();
        self.timers.retain(|_, handle| handle.is_none());
        self.js_timers.clear();
        self.toggles.clear();
        self.js = None;
        self.isolated_js.clear();
        self.isolated_names.clear();
        // https://html.spec.whatwg.org/multipage/window-object.html#windowproxy-set
        self.world.borrow_mut().await_proxy_sets_replay();
        self.world.borrow_mut().realm_context = None;
        self.init_scripts_deferred = false;
        self.active_parse = None;
        self.stream_publish = None;
        self.executed_scripts.clear();
        self.parser_eof = true;
        {
            let world = self.world.borrow();
            let mut doc = world.doc_mut();
            doc.parser_active = false;
            doc.state.current_script = None;
            doc.import_map.clear();
        }
        if reason == RealmReset::Reopen {
            self.world.borrow().clear_outbox_for_open();
        } else {
            self.world.borrow().clear_outbox();
        }
        self.parser_blocked_script = None;
        self.deferred_modules.clear();
    }

    pub(crate) fn apply_document_stream(&mut self, command: DocumentStreamCommand) {
        match command {
            DocumentStreamCommand::Open => self.reopen_document(),
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
        let previous = self.world.borrow().doc().state.current_script;
        self.world.borrow().doc_mut().state.current_script = element;
        if !self.ensure_js_ok() {
            return;
        }
        self.fire_js(|js| js.eval_classic_script(source, base_line, filename));
        self.world.borrow().doc_mut().state.current_script = previous;
        self.adopt_js_work();
    }

    // https://html.spec.whatwg.org/multipage/webappapis.html#perform-a-microtask-checkpoint
    fn deliver_mutations(&mut self) {
        self.fire_js(crate::js::JsRealm::deliver_mutations);
    }

    fn blitz_config(&self) -> blitz_dom::DocumentConfig {
        // https://html.spec.whatwg.org/multipage/urls-and-fetching.html#document-base-url
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

    fn shared_font_ctx(&self) -> parley::FontContext {
        self.world.borrow().runtime.font_ctx.clone()
    }

    // https://html.spec.whatwg.org/multipage/window-object.html#the-window-object
    fn skip_nested_html_realm(&self, parsed: &crate::Parsed) -> bool {
        if parsed.xml_document || html_needs_realm(parsed) {
            return false;
        }
        if !self.world.borrow().win().init_scripts.borrow().is_empty() {
            return false;
        }
        self.shared.borrow().tree.parent(self.frame).is_some()
    }

    fn install_parsed(&mut self, parsed: Parsed) -> bool {
        if self.js.is_none() {
            let skip_js = self.defer_js || self.skip_nested_html_realm(&parsed);
            let document = self.world.borrow_mut().replace_document(parsed);
            self.register_document(document);
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

    // https://html.spec.whatwg.org/multipage/scripting.html#parser-inserted
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

    fn install_policy(&mut self) {
        if let Some(policy) = self.response.policy.clone() {
            self.world.borrow().doc_mut().policy = policy;
            return;
        }
        if let Some(policy) = self.response.inherited_policy.take() {
            self.world.borrow().doc_mut().policy = policy;
        }
    }

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

    fn apply_viewport(&self) {
        let (width, height) = self.viewport_size;
        self.world.borrow().win().viewport_size.set((width, height));
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

    pub(crate) fn set_viewport(&mut self, width: u32, height: u32) {
        self.viewport_size = (width, height);
        self.apply_viewport();
        {
            let world = self.world.borrow();
            if let Some(mut parsed) = world.main_document_mut() {
                crate::render::resolve_until_settled(
                    &mut parsed.document.base,
                    world.runtime.animation_seconds(),
                );
            }
        }
        // https://drafts.csswg.org/cssom-view/#evaluate-media-queries-and-report-changes
        // https://drafts.csswg.org/cssom-view/#run-the-resize-steps
        self.fire_js(super::js::JsRealm::report_media_changes);
        self.fire_js(super::js::JsRealm::report_visual_viewport_changes);
    }

    pub(crate) fn set_viewport_size(&mut self, size: (u32, u32)) {
        self.viewport_size = size;
        self.world.borrow().win().viewport_size.set(size);
    }

    // https://chromedevtools.github.io/devtools-protocol/tot/Page/#method-getLayoutMetrics
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
        crate::render::resolve_until_settled(base, world.runtime.animation_seconds());
        // https://drafts.csswg.org/cssom-view/#scrolling-element
        let root = base.root_element();
        content.0 = content.0.max(root.scroll_width());
        content.1 = content.1.max(root.scroll_height());
        content
    }

    // https://html.spec.whatwg.org/multipage/parsing.html#the-end
    fn finish_parse(&mut self) {
        if !self.parser_eof {
            return;
        }
        self.adopt_js_work();
        let Some(ActiveParse { buffer, delay }) = self.active_parse.take() else {
            return;
        };
        drop(delay);
        self.response.sniff_content_type();
        // https://html.spec.whatwg.org/multipage/parsing.html#xml-parser
        // https://mimesniff.spec.whatwg.org/#xml-mime-type
        let parsed = if let Some(content_type) = self
            .response
            .content_type
            .as_deref()
            .and_then(crate::xml::navigated_content_type)
        {
            let mut parsed =
                crate::xml::parse_navigated(&buffer, content_type, self.blitz_config());
            parsed.character_set = self.response.character_set;
            if let Some(header) = self.response.content_type.as_deref() {
                parsed.content_type = crate::xml::document_content_type(header);
            }
            parsed
        } else {
            // https://mimesniff.spec.whatwg.org/#sniffing-a-mime-type
            // https://html.spec.whatwg.org/multipage/rendering.html#the-pre-element
            let plain = self
                .response
                .content_type
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
            parsed.character_set = self.response.character_set;
            // https://html.spec.whatwg.org/multipage/nav-history-apis.html#concept-document-content-type
            if let Some(header) = self.response.content_type.as_deref() {
                parsed.content_type = crate::xml::document_content_type(header);
            }
            parsed
        };
        if !self.install_parsed(parsed) {
            self.world.borrow().doc_mut().parser_active = false;
            self.record_event(RendererEvent::ScriptFailed);
            return;
        }
        self.deliver_mutations();
        self.resume_scripts();
    }

    // https://w3c.github.io/webappsec-csp/#should-block-inline
    // https://html.spec.whatwg.org/multipage/webappapis.html#event-handler-content-attributes
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
                .borrow()
                .doc_mut()
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

    fn resume_scripts(&mut self) {
        let _turn = self.js.as_ref().map(crate::js::JsRealm::enter_turn);
        if self.run_scripts() {
            return;
        }
        self.world.borrow().doc_mut().parser_active = false;
        // https://html.spec.whatwg.org/multipage/browsing-the-web.html#scroll-to-fragid
        self.update_url_target();
        self.check_parser_handler_attributes();
        self.deliver_mutations();
        self.fire_document_end();
        let _ = self.check_load();
    }

    // https://w3c.github.io/webappsec-csp/#meta-element
    fn apply_meta_csp(&mut self, id: crate::dom::tree::NodeId) {
        self.world.borrow_mut().apply_meta_policy(id);
    }

    // https://html.spec.whatwg.org/multipage/semantics.html#meta-referrer
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
            self.world.borrow().doc_mut().policy.referrer_policy = Some(policy);
        }
    }

    pub(in crate::document) fn inline_script_violations(
        &self,
        source: &str,
        id: crate::dom::tree::NodeId,
    ) -> Vec<crate::csp::Violation> {
        let world = self.world.borrow();
        let doc = world.doc();
        let slot = doc.state.nonce_slots.get(&id).map(String::as_str);
        let parser_inserted = doc.state.parser_inserted.contains(&id);
        let (nonce, nonceable) = world.document(id).map_or_else(
            || (String::new(), false),
            |parsed| crate::csp::element_nonce(&parsed.document.base, id.node, slot),
        );
        let mut violations = doc.policy.csp.check_inline(
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
            // https://w3c.github.io/webappsec-csp/#report-violation
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
        let doc = world.doc();
        let slot = doc.state.nonce_slots.get(&id).map(String::as_str);
        let parser_inserted = doc.state.parser_inserted.contains(&id);
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
        doc.policy.csp.check_request(
            &doc.origin,
            &doc.document_url,
            url,
            crate::csp::Destination::Script,
            Some(crate::csp::ScriptRequest {
                nonce: &nonce,
                parser_inserted,
                integrity: &integrity,
            }),
        )
    }

    pub(crate) fn document_node(&self) -> Option<crate::dom::tree::NodeId> {
        self.world
            .borrow()
            .with_main_document(|parsed| crate::dom::tree::NodeId {
                document: parsed.id,
                node: parsed.document.base.root_node().id,
            })
    }

    // https://html.spec.whatwg.org/multipage/scripting.html#prepare-the-script-element
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
            // https://html.spec.whatwg.org/multipage/semantics.html#meta-referrer
            let referrer_meta = crate::dom::tree::is_html_element(base, id, "meta")
                && crate::dom::tree::attr(base, id, "name")
                    .is_some_and(|name| name.eq_ignore_ascii_case("referrer"));
            if referrer_meta || crate::csp::meta_policy(base, id).is_some() {
                steps.push(ParseStep::Meta(node_id));
            }
            // https://html.spec.whatwg.org/multipage/urls-and-fetching.html#document-base-url
            if crate::dom::tree::is_html_element(base, id, "base")
                && crate::dom::tree::attr(base, id, "href").is_some()
            {
                steps.push(ParseStep::Base(node_id));
            }
            // https://html.spec.whatwg.org/multipage/scripting.html#parser-inserted
            if crate::dom::tree::is_html_element(base, id, "script")
                && !world.doc().sandbox.scripts_disabled()
                && world.is_parser_inserted(node_id)
            {
                steps.push(ParseStep::Script(node_id));
            }
            stack.extend(node.children.iter().rev().copied());
        }
        steps
    }

    // https://html.spec.whatwg.org/multipage/scripting.html#pending-parsing-blocking-script
    // https://html.spec.whatwg.org/multipage/parsing.html#parsing-main-intext
    // https://html.spec.whatwg.org/multipage/scripting.html#prepare-the-script-element
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
                        // https://w3c.github.io/webappsec-csp/#is-base-allowed-for-document
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
            // https://html.spec.whatwg.org/multipage/scripting.html#prepare-the-script-element
            if let Some(mut parsed) = self.world.borrow().document_mut(id) {
                parsed.document.mark_script_started(id.node);
            }
            // https://html.spec.whatwg.org/multipage/iframe-embed-object.html#the-iframe-element:html-element-insertion-steps
            self.adopt_pending_frames_before_node(Some(id));
            self.deliver_mutations();
            let script = crate::js::script_at(&self.world.borrow(), id);
            match script {
                Some(crate::js::Script::Classic(crate::js::ScriptElementSource::Inline {
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
                Some(crate::js::Script::Classic(crate::js::ScriptElementSource::Src(src))) => {
                    if self.start_src_script(id, &src, launched, &parse_base) {
                        return true;
                    }
                }
                Some(crate::js::Script::Module(source)) => {
                    // https://html.spec.whatwg.org/multipage/scripting.html#prepare-the-script-element
                    let prepared = self.prepare_deferred_module(id, source);
                    self.deferred_modules.push(prepared);
                }
                Some(crate::js::Script::ImportMap(crate::js::ScriptElementSource::Inline {
                    source,
                    ..
                })) => {
                    // https://html.spec.whatwg.org/multipage/scripting.html#prepare-the-script-element
                    let violations = self.inline_script_violations(&source, id);
                    if !violations.is_empty() {
                        self.fire_csp_violations(id, &violations);
                    }
                    if crate::csp::blocks(&violations) {
                        continue;
                    }
                    if let Ok(map) = crate::js::modules::ImportMap::parse(&source, &parse_base) {
                        self.world.borrow().doc_mut().import_map.merge(map);
                    }
                }
                Some(crate::js::Script::ImportMap(crate::js::ScriptElementSource::Src(_))) => {
                    // https://html.spec.whatwg.org/multipage/scripting.html#prepare-the-script-element
                    self.tasks.push(
                        crate::task_queue::TaskSource::DomManipulation,
                        Task::NodeError(id),
                    );
                }
                None => {}
            }
        }
        // https://html.spec.whatwg.org/multipage/iframe-embed-object.html#the-iframe-element:html-element-insertion-steps
        self.adopt_pending_frames();
        false
    }

    // https://html.spec.whatwg.org/multipage/scripting.html#prepare-the-script-element
    fn dynamic_script_launched(&self, id: crate::dom::tree::NodeId) -> bool {
        self.world
            .borrow()
            .document(id)
            .is_some_and(|parsed| parsed.document.script_already_started(id.node))
    }

    // https://html.spec.whatwg.org/multipage/scripting.html#prepare-the-script-element
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
            .or(self.world.borrow().doc().policy.referrer_policy)
            .unwrap_or_default();
        dial.req.csp = Some(Box::new(
            self.redirect_gate(crate::csp::Destination::Script),
        ));
        dial.integrity = self.script_integrity(id);
        self.queue_dial(dial);
        true
    }

    // https://www.w3.org/TR/SRI/#parse-metadata
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
        // https://fetch.spec.whatwg.org/#scheme-fetch
        if url.scheme() != "http" && url.scheme() != "https" && url.scheme() != "data" {
            return Err(TabError::InvalidUrl { spec: spec.into() });
        }
        Ok(url)
    }

    // https://html.spec.whatwg.org/multipage/browsers.html#concept-origin
    // https://fetch.spec.whatwg.org/#concept-request-origin
    pub(crate) fn dial_initiator(&self) -> url::Origin {
        self.world.borrow().doc().origin.clone()
    }

    // https://w3c.github.io/hr-time/#dom-performance-now
    fn performance_now(&self) -> f64 {
        let world = self.world.borrow();
        let (sampled, origin) = world.win().event_clock;
        origin + sampled.elapsed().as_secs_f64() * 1000.0
    }

    // https://fetch.spec.whatwg.org/#concept-tao-check
    fn timing_allow_origin_passed(&self, outcome: &crate::protocol::DialOutcome) -> bool {
        if !outcome.crossed_origin {
            return true;
        }
        let origin = self.world.borrow().doc().origin.ascii_serialization();
        outcome.headers.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("timing-allow-origin")
                && value.split(',').any(|part| {
                    let token = part.trim();
                    token == "*" || (origin != "null" && token == origin)
                })
        })
    }

    // https://w3c.github.io/resource-timing/#dfn-add-a-performanceresourcetiming-entry
    fn queue_resource_timing(
        &mut self,
        outcome: &crate::protocol::DialOutcome,
        initiator_type: &str,
        timing: DialTiming,
        encoded_body_size: u64,
    ) {
        let name = outcome.final_url.to_string();
        let response_status = outcome.status;
        let tao_passed = self.timing_allow_origin_passed(outcome);
        self.fire_js(|js| {
            js.queue_resource_timing(crate::js::ResourceTiming {
                name: &name,
                initiator_type,
                start_time: timing.start,
                end_time: timing.end,
                response_status,
                encoded_body_size,
                decoded_body_size: encoded_body_size,
                tao_passed,
            })
        });
    }

    // https://w3c.github.io/resource-timing/#dom-performanceresourcetiming-initiatortype
    fn queue_classic_script_timing(
        &mut self,
        outcome: &crate::protocol::DialOutcome,
        timing: DialTiming,
    ) {
        let encoded = match &outcome.body {
            crate::protocol::ResponseBody::Bytes(body) => body.len() as u64,
            crate::protocol::ResponseBody::Undecodable
            | crate::protocol::ResponseBody::Streamed(_) => 0,
        };
        self.queue_resource_timing(outcome, "script", timing, encoded);
    }

    // https://w3c.github.io/resource-timing/#dom-performanceresourcetiming-initiatortype
    fn subresource_initiator(destination: Option<crate::csp::Destination>) -> &'static str {
        match destination {
            Some(crate::csp::Destination::Img) => "img",
            Some(crate::csp::Destination::StyleSheet) => "link",
            Some(crate::csp::Destination::Font) => "css",
            Some(
                crate::csp::Destination::Connect
                | crate::csp::Destination::Script
                | crate::csp::Destination::Frame
                | crate::csp::Destination::Worker,
            )
            | None => "other",
        }
    }

    // https://w3c.github.io/webappsec-referrer-policy/#determine-requests-referrer
    pub(crate) fn referrer_url(&self) -> Url {
        self.world.borrow().doc().referrer_url.clone()
    }

    // https://html.spec.whatwg.org/multipage/urls-and-fetching.html#document-base-url
    pub(crate) fn base_url(&self) -> Url {
        // https://html.spec.whatwg.org/multipage/urls-and-fetching.html#fallback-base-url
        self.world.borrow().main_base_url()
    }

    // https://html.spec.whatwg.org/multipage/urls-and-fetching.html#fallback-base-url
    fn fallback_base_url(&self) -> Url {
        let world = self.world.borrow();
        let base_url = world.doc().base_url.clone();
        base_url.unwrap_or_else(|| self.url.clone())
    }

    // https://html.spec.whatwg.org/multipage/scripting.html#prepare-the-script-element
    fn resolve_script_url(base: &Url, spec: &str) -> Result<Url, TabError> {
        let url = Url::parse(spec)
            .or_else(|_| base.join(spec))
            .map_err(|_| TabError::InvalidUrl { spec: spec.into() })?;
        if !matches!(url.scheme(), "http" | "https" | "data") {
            return Err(TabError::InvalidUrl { spec: spec.into() });
        }
        Ok(url)
    }

    fn finish_js_fetch_dial(
        &mut self,
        id: crate::js::JsFetchId,
        realm: crate::task_queue::RealmId,
        redirected: bool,
        request_url: &Url,
        outcome: DialOutcome,
    ) {
        self.record_event(RendererEvent::Fetch {
            status: outcome.status,
        });
        // https://w3c.github.io/webappsec-csp/#should-block-response
        let blocked = redirected
            && self.response_url_blocked(
                &outcome.final_url,
                request_url,
                crate::csp::Destination::Connect,
                None,
            );
        if blocked {
            // https://fetch.spec.whatwg.org/#concept-response-body
            if let crate::protocol::ResponseBody::Streamed(stream) = outcome.body {
                self.services.cancel_stream(stream);
            }
            self.settle_js_fetch(id, realm, None);
        } else {
            self.settle_js_fetch(id, realm, Some(outcome));
        }
    }

    pub(in crate::document) fn finish_dial(&mut self, done: CompletedDial) {
        let CompletedDial {
            context,
            outcome,
            req,
            started,
            ancestors,
            integrity,
        } = done;
        // https://w3c.github.io/resource-timing/#sec-resource-timing
        let timing = DialTiming::new(started, self.performance_now());
        let redirected = outcome.final_url != req.url;
        // https://cookiestore.spec.whatwg.org/#process-cookie-changes
        let cookies_stored = outcome.cookies_stored;
        match context {
            DialContext::JsFetch { id, realm } => {
                self.finish_js_fetch_dial(id, realm, redirected, &req.url, outcome);
            }
            DialContext::ClassicScript { element } => {
                self.record_event(RendererEvent::Fetch {
                    status: outcome.status,
                });
                let unblocks_parser = self
                    .parser_blocked_script
                    .is_none_or(|waiting| waiting == element);
                if unblocks_parser {
                    self.parser_blocked_script = None;
                }
                // https://w3c.github.io/webappsec-csp/#should-block-response
                let blocked = redirected
                    && self.response_url_blocked(
                        &outcome.final_url,
                        &req.url,
                        crate::csp::Destination::Script,
                        Some(element),
                    );
                // https://www.w3.org/TR/SRI/#verify-integrity
                let body = match &outcome.body {
                    crate::protocol::ResponseBody::Bytes(body) => Some(body.as_slice()),
                    crate::protocol::ResponseBody::Undecodable
                    | crate::protocol::ResponseBody::Streamed(_) => None,
                };
                let integrity_ok = integrity.as_deref().is_none_or(|metadata| {
                    body.is_some_and(|body| crate::csp::integrity_verifies(metadata, body))
                });
                // https://html.spec.whatwg.org/multipage/scripting.html#prepare-the-script-element
                let mime_ok = crate::js::classic_script_mime(outcome.content_type.as_deref());
                // https://w3c.github.io/resource-timing/#resources-included-in-the-performanceresourcetiming-interface
                if !blocked {
                    self.queue_classic_script_timing(&outcome, timing);
                }
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
                // https://w3c.github.io/webappsec-csp/#should-block-response
                if redirected
                    && let Some(embedder) = embedder
                    && let Some(gate) = req.csp.as_deref()
                    && self.note_frame_redirect(embedder, gate, &outcome.final_url, &req.url)
                {
                } else {
                    let url = outcome.final_url.clone();
                    self.world.borrow_mut().install_sandbox(sandbox);
                    self.commit_frame_response(&outcome, &url, embedder, &ancestors);
                }
            }
            DialContext::BlitzResource { id, .. } => {
                let cors_clean = req.policy.cors().mode == net::cors::RequestMode::Cors;
                if let Some(request) = self.blitz_requests.remove(&id) {
                    self.deliver_blitz_resource(&outcome, request, redirected, cors_clean, timing);
                }
            }
            DialContext::CspReport => {}
        }
        if cookies_stored {
            self.world.borrow().notify_cookie_changes();
            self.fire_js(crate::js::JsRealm::process_cookie_changes);
        }
        self.adopt_js_work();
    }

    // https://w3c.github.io/webappsec-csp/#frame-ancestors-navigation-response
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
        self.set_reporting_endpoints(head.source.reporting_endpoints());
        if embedder.is_some()
            && !ancestors.is_empty()
            && self.note_frame_ancestors_block(embedder, url, policy, ancestors)
        {
            return;
        }
        // https://chromedevtools.github.io/devtools-protocol/tot/Page/#event-frameNavigated
        // https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName
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
        // https://html.spec.whatwg.org/multipage/document-lifecycle.html#creating-and-initializing-a-document-object
        self.set_document_referrer(outcome.referrer.clone());
        self.install_frame_response(&head, body, None);
    }

    fn deliver_blitz_resource(
        &mut self,
        outcome: &DialOutcome,
        request: BlitzRequest,
        redirected: bool,
        cors_clean: bool,
        timing: DialTiming,
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
        // https://w3c.github.io/resource-timing/#resources-included-in-the-performanceresourcetiming-interface
        if !blocked {
            self.queue_resource_timing(
                outcome,
                Self::subresource_initiator(destination),
                timing,
                body.map_or(0, |body| body.len() as u64),
            );
        }
        let succeeded = !blocked && (200..300).contains(&outcome.status) && body.is_some();
        // https://fetch.spec.whatwg.org/#concept-request-response-tainting
        // https://html.spec.whatwg.org/multipage/canvas.html#the-image-argument
        let origin_clean = cors_clean || !outcome.crossed_origin;
        if succeeded
            && let Some(body) = body
            && !destination.is_some_and(crate::csp::Destination::is_image)
        {
            self.subresource_bytes.insert(
                (request.key.url.clone(), request.key.crossorigin),
                body.to_vec(),
            );
        }
        // https://w3c.github.io/webappsec-referrer-policy/#integration-with-css
        if destination == Some(crate::csp::Destination::StyleSheet)
            && !blocked
            && (200..300).contains(&outcome.status)
        {
            let inherited = self
                .world
                .borrow()
                .doc()
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
        self.blitz_response_pending = true;
        Box::new(request.handler).response(
            outcome.final_url.to_string(),
            blitz_traits::net::Bytes::from(body.unwrap_or_default().to_vec()),
            blitz_traits::net::ResponseInfo { origin_clean },
        );
    }

    pub(in crate::document) fn fail_dial(&mut self, fail: DialContext) {
        self.record_event(RendererEvent::FetchFailed);
        match fail {
            DialContext::JsFetch { id, realm } => {
                self.settle_js_fetch(id, realm, None);
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
            DialContext::FrameLoad { .. } | DialContext::CspReport => {}
            DialContext::BlitzResource { id, .. } => {
                if let Some(request) = self.blitz_requests.remove(&id) {
                    self.fail_blitz_request(request);
                }
            }
        }
        // https://cookiestore.spec.whatwg.org/#process-cookie-changes
        self.world.borrow().notify_cookie_changes();
        self.fire_js(crate::js::JsRealm::process_cookie_changes);
        self.adopt_js_work();
    }

    fn scroll_animation_active(&self) -> bool {
        let world = self.world.borrow();
        world
            .main_document()
            .is_some_and(|parsed| parsed.document.base.has_scroll_animation())
    }

    // https://drafts.csswg.org/css-animations-1/#events
    fn css_animation_active(&self) -> bool {
        let world = self.world.borrow();
        world
            .main_document()
            .is_some_and(|parsed| parsed.document.base.has_finishing_css_animations())
    }

    // https://drafts.csswg.org/cssom-view/#run-the-scroll-steps
    fn adopt_scroll_events(&mut self) {
        let events = {
            let world = self.world.borrow();
            match world.main_document_mut() {
                Some(mut parsed) => parsed.document.base.take_pending_scroll_events(),
                None => Vec::new(),
            }
        };
        if self.ui_events.push_scroll(events) {
            self.tasks.push(
                crate::task_queue::TaskSource::UserInteraction,
                Task::ScrollEvents,
            );
        }
    }

    // https://drafts.csswg.org/css-animations-1/#events
    fn adopt_animation_events(&mut self) {
        let events = {
            let world = self.world.borrow();
            match world.main_document_mut() {
                Some(mut parsed) => parsed.document.base.take_pending_animation_events(),
                None => Vec::new(),
            }
        };
        if self.ui_events.push_animation(events) {
            self.tasks.push(
                crate::task_queue::TaskSource::DomManipulation,
                Task::AnimationEvents,
            );
        }
    }

    fn settle_blitz_layout(&mut self) {
        let passes = {
            let world = self.world.borrow();
            let Some(mut parsed) = world.main_document_mut() else {
                return;
            };
            crate::render::resolve_until_settled(
                &mut parsed.document.base,
                world.runtime.animation_seconds(),
            )
        };
        #[cfg(test)]
        {
            self.blitz_resolve_passes += u64::from(passes);
        }
        #[cfg(not(test))]
        let _ = passes;
        self.note_linked_stylesheets();
        self.drain_image_settlements();
        let settled: Vec<(u64, Vec<usize>, bool)> = {
            let world = self.world.borrow();
            let Some(mut parsed) = world.main_document_mut() else {
                return;
            };
            parsed
                .document
                .base
                .take_pending_font_face_events()
                .into_iter()
                .map(|event| {
                    let loaded = matches!(
                        event.status,
                        blitz_dom::font_face_events::FontFaceLoadStatus::Loaded
                    );
                    (event.sheet, event.path, loaded)
                })
                .collect()
        };
        if !settled.is_empty() {
            let apply = |js: &crate::js::JsRealm| {
                for (sheet, path, loaded) in &settled {
                    js.font_face_settled(*sheet, path.clone(), *loaded)?;
                }
                Ok(())
            };
            self.fire_js(apply);
            let failed = self
                .isolated_js
                .values()
                .map(|(_, realm)| apply(realm).is_err())
                .fold(false, |any, failed| any | failed);
            if failed {
                self.record_event(RendererEvent::ScriptFailed);
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn blitz_resolve_passes(&self) -> u64 {
        self.blitz_resolve_passes
    }

    #[cfg(test)]
    pub(crate) fn reset_blitz_resolve_passes(&mut self) {
        self.blitz_resolve_passes = 0;
    }

    // https://html.spec.whatwg.org/multipage/images.html#updating-the-image-data
    fn pump_blitz_messages(&mut self) {
        {
            let world = self.world.borrow();
            if let Some(mut parsed) = world.main_document_mut() {
                parsed.document.base.handle_messages();
            }
        }
        self.blitz_response_pending = false;
    }

    // https://html.spec.whatwg.org/multipage/semantics.html#the-link-element
    // https://html.spec.whatwg.org/multipage/links.html#link-type-stylesheet
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
            self.tasks.push(
                crate::task_queue::TaskSource::Networking,
                Task::NodeLoad(element),
            );
        }
    }

    pub(in crate::document) fn settle_js_fetch(
        &mut self,
        id: JsFetchId,
        realm: crate::task_queue::RealmId,
        outcome: Option<DialOutcome>,
    ) {
        self.fire_realm(realm, |js| js.finish_js_fetch(id, outcome));
    }

    // https://html.spec.whatwg.org/multipage/parsing.html#the-end
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
        // https://html.spec.whatwg.org/multipage/parsing.html#delay-the-load-event
        self.adopt_pending_frames();
    }

    // https://html.spec.whatwg.org/multipage/webappapis.html#fetch-the-descendants-of-and-link-a-module-script
    // https://html.spec.whatwg.org/multipage/dom.html#dom-document-currentscript
    pub(in crate::document) fn with_module_root<R>(
        &mut self,
        element: Option<crate::dom::tree::NodeId>,
        body: impl FnOnce(&mut Self) -> R,
    ) -> R {
        let previous = self.world.borrow().doc().state.module_fetch_root;
        self.world.borrow().doc_mut().state.module_fetch_root = element;
        let result = body(self);
        self.world.borrow().doc_mut().state.module_fetch_root = previous;
        result
    }

    // https://html.spec.whatwg.org/multipage/scripting.html#prepare-the-script-element
    // https://html.spec.whatwg.org/multipage/webappapis.html#fetch-an-inline-module-script-graph
    fn prepare_deferred_module(
        &mut self,
        element: crate::dom::tree::NodeId,
        source: crate::js::ScriptElementSource,
    ) -> DeferredModule {
        let declared = match &source {
            crate::js::ScriptElementSource::Inline { source: text, .. } => {
                let violations = self.inline_script_violations(text, element);
                if !violations.is_empty() {
                    self.fire_csp_violations(element, &violations);
                }
                if crate::csp::blocks(&violations) {
                    return DeferredModule::Blocked(element);
                }
                let name = format!("{}#inline-module-{element:?}", self.url);
                self.with_module_root(Some(element), |this| {
                    this.js
                        .as_ref()
                        .map_or(Ok(()), |js| js.declare_inline_module(&name, text))
                })
            }
            crate::js::ScriptElementSource::Src(src) => match self.resolve_dial_url(src) {
                Ok(url) => {
                    let violations = self.script_url_violations(&url, element);
                    if !violations.is_empty() {
                        self.fire_csp_violations(element, &violations);
                    }
                    if crate::csp::blocks(&violations) {
                        return DeferredModule::Blocked(element);
                    }
                    self.with_module_root(Some(element), |this| {
                        this.js
                            .as_ref()
                            .map_or(Ok(()), |js| js.declare_external_module(url.as_str()))
                    })
                }
                Err(_) => return DeferredModule::Failed,
            },
        };
        if declared.is_err() {
            DeferredModule::Failed
        } else {
            DeferredModule::Ready { element, source }
        }
    }

    fn run_deferred_modules(&mut self) {
        let modules = std::mem::take(&mut self.deferred_modules);
        for module in modules {
            match module {
                DeferredModule::Blocked(element) => self.tasks.push(
                    crate::task_queue::TaskSource::DomManipulation,
                    crate::document::Task::NodeError(element),
                ),
                DeferredModule::Failed => {
                    self.record_event(RendererEvent::ScriptFailed);
                    self.adopt_js_work();
                }
                DeferredModule::Ready { element, source } => {
                    match self.run_module_body(element, source) {
                        None => self.tasks.push(
                            crate::task_queue::TaskSource::DomManipulation,
                            crate::document::Task::NodeError(element),
                        ),
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
        }
    }

    // https://html.spec.whatwg.org/multipage/semantics.html#attr-meta-http-equiv-refresh
    fn start_declarative_refresh(&mut self, value: &str) {
        if self.refresh.is_scheduled()
            || self
                .world
                .borrow()
                .doc()
                .sandbox
                .contains(crate::protocol::SandboxFlag::AutomaticFeatures)
        {
            return;
        }
        let document_url = self.world.borrow().doc().document_url.clone();
        let Some((target, delay)) = parse_declarative_refresh(value, &document_url) else {
            return;
        };
        self.refresh.schedule(target, delay);
        self.arm_refresh();
    }

    // https://html.spec.whatwg.org/multipage/semantics.html#attr-meta-http-equiv-refresh
    fn arm_refresh(&mut self) {
        if !self.load_fired {
            return;
        }
        let mut refresh = std::mem::take(&mut self.refresh);
        refresh.arm(|delay| self.schedule_timer(delay));
        self.refresh = refresh;
    }

    fn fire_refresh(&mut self, target: &Url) {
        let navigation = FrameNavigation::get(
            crate::js::NavigationTarget::Target(crate::protocol::TargetName::Current),
            target.to_string(),
        );
        self.world.borrow_mut().queue_frame_navigation(navigation);
    }

    // https://html.spec.whatwg.org/multipage/parsing.html#the-end
    pub(crate) fn check_load(&mut self) -> bool {
        self.pump_blitz_messages();
        // https://html.spec.whatwg.org/multipage/images.html#update-the-image-data
        self.settle_blitz_layout();
        self.blitz_shell.take_redraw();
        self.drain_image_settlements();
        self.drain_blitz_fetches();
        if !self.world.borrow().doc().parser_active {
            self.tasks.append(&mut self.deferred_image_events);
            self.tasks.append(&mut self.csp.pending_events);
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
        // https://html.spec.whatwg.org/multipage/window-object.html#windowproxy-set
        if self.awaits_proxy_replay() {
            return false;
        }
        self.complete_ready_state();
        self.load_fired = true;
        self.arm_refresh();
        self.record_event(RendererEvent::Browser(BrowserEvent::Load));
        self.fire_js(crate::js::JsRealm::fire_load);
        // https://w3c.github.io/navigation-timing/#marking-navigation-timing
        self.fire_js(crate::js::JsRealm::fire_navigation_timing_entry);
        self.adopt_js_work();
        true
    }

    fn complete_ready_state(&mut self) {
        if self.world.borrow().main_ready_state() == ReadyState::Complete {
            return;
        }
        self.world
            .borrow_mut()
            .set_main_ready_state(ReadyState::Complete);
        self.fire_js(crate::js::JsRealm::fire_ready_state_change);
    }

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
                self.blitz_response_pending = true;
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
            // https://w3c.github.io/webappsec-csp/#should-block-request
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
            // https://fetch.spec.whatwg.org/#concept-request-initiator
            let (referrer, referrer_policy) = if let Some(sheet) = incoming.style_sheet.clone() {
                // https://w3c.github.io/webappsec-referrer-policy/#integration-with-css
                let policy = self
                    .style_policies
                    .get(sheet.as_str())
                    .copied()
                    .or(self.world.borrow().doc().policy.referrer_policy)
                    .unwrap_or_default();
                (crate::protocol::ReferrerRequest::Url(sheet), policy)
            } else {
                // https://html.spec.whatwg.org/multipage/urls-and-fetching.html#referrer-policy-attribute
                let policy = request
                    .referrer_policy
                    .map(crate::protocol::referrer_policy)
                    .or(self.world.borrow().doc().policy.referrer_policy)
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

    fn fail_blitz_request(&mut self, request: BlitzRequest) {
        let BlitzRequest {
            handler,
            node,
            destination,
            ..
        } = request;
        self.blitz_response_pending = true;
        Box::new(handler).bytes(String::new(), blitz_traits::net::Bytes::new());
        if !destination.is_some_and(crate::csp::Destination::is_image)
            && let Some(node) = node
        {
            self.tasks.push(
                crate::task_queue::TaskSource::Networking,
                Task::NodeError(node),
            );
        }
    }

    pub(crate) fn redirect_gate(
        &self,
        destination: crate::csp::Destination,
    ) -> crate::RedirectGate {
        let world = self.world.borrow();
        let doc = world.doc();
        crate::RedirectGate::new(doc.policy.csp.clone(), &doc.origin, destination)
    }

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
            self.csp.embedder_reports.push((embedder, violations));
        }
        blocked
    }

    // https://w3c.github.io/webappsec-csp/#should-navigation-response-to-navigation-request-of-type-in-target-be-blocked-by-content-security-policy
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
                Some(InheritedOrigin {
                    origin: url::Origin::new_opaque(),
                    domain: Rc::new(RefCell::new(None)),
                }),
            );
        }
        blocked
    }

    pub(crate) fn take_embedder_csp_reports(
        &mut self,
    ) -> Vec<(FrameId, Vec<crate::csp::Violation>)> {
        std::mem::take(&mut self.csp.embedder_reports)
    }

    // https://w3c.github.io/webappsec-csp/#should-block-response
    pub(in crate::document) fn response_url_blocked(
        &mut self,
        final_url: &Url,
        request_url: &Url,
        destination: crate::csp::Destination,
        target: Option<crate::dom::tree::NodeId>,
    ) -> bool {
        let violations = {
            let world = self.world.borrow();
            let doc = world.doc();
            doc.policy.csp.check_redirected_request(
                &doc.origin,
                &doc.document_url,
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

    pub(in crate::document) fn subresource_violations(
        &self,
        url: &Url,
        destination: crate::csp::Destination,
    ) -> Vec<crate::csp::Violation> {
        let world = self.world.borrow();
        let doc = world.doc();
        doc.policy
            .csp
            .check_request(&doc.origin, &doc.document_url, url, destination, None)
    }

    fn subresource_element(
        &self,
        node: Option<blitz_traits::node_id::NodeId>,
    ) -> Option<crate::dom::tree::NodeId> {
        let document = self.world.borrow().main_document().map(|parsed| parsed.id);
        document
            .zip(node)
            .map(|(document, node)| crate::dom::tree::NodeId { document, node })
    }

    fn queue_image_event(&mut self, event: Task) {
        if self.world.borrow().doc().parser_active {
            self.deferred_image_events
                .push_back((crate::task_queue::TaskSource::DomManipulation, event));
        } else {
            self.tasks
                .push(crate::task_queue::TaskSource::DomManipulation, event);
        }
    }

    // https://html.spec.whatwg.org/multipage/images.html#update-the-image-data
    fn drain_image_settlements(&mut self) {
        let document = self.world.borrow().main_document().map(|parsed| parsed.id);
        while let Ok(settlement) = self.image_settle_rx.try_recv() {
            let Some(document) = document else {
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
                    self.image_load_delays.remove(&element);
                }
            }
        }
    }

    pub(crate) fn mark_frame_load_pending(&mut self, container: crate::dom::tree::NodeId) {
        self.pending_frame_loads
            .insert(container, self.load_delay.guard());
    }

    pub(crate) fn pending_frame_loads(&self) -> Vec<crate::dom::tree::NodeId> {
        self.pending_frame_loads.keys().copied().collect()
    }

    pub(crate) fn finish_frame_load(&mut self, container: crate::dom::tree::NodeId) -> bool {
        if self.pending_frame_loads.remove(&container).is_none() {
            return false;
        }
        // https://html.spec.whatwg.org/multipage/iframe-embed-object.html#the-iframe-element:html-element-insertion-steps
        let already_fired = self
            .world
            .borrow()
            .doc_mut()
            .insertion_load_fired
            .remove(&container);
        if !already_fired {
            self.fire_node_load(container);
        }
        true
    }

    pub(crate) fn cancel_frame_load(&mut self, container: crate::dom::tree::NodeId) {
        let _ = self
            .world
            .borrow()
            .doc_mut()
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
