//! Dedicated workers: one documentless realm per `new Worker()`.
//!
//! A worker runs in the page engine that created it, so it shares the
//! engine's services, `QuickJS` runtime, and turn deadline. It owns its own
//! global (`DedicatedWorkerGlobalScope`), task queue, and timers, and it has
//! no document, browsing context, or history
//! (<https://html.spec.whatwg.org/multipage/workers.html#dedicated-workers-and-the-dedicatedworkerglobalscope-interface>).
//!
//! `QuickJS` cannot create a context while another context's script runs, so
//! the constructor only queues a [`WorkerRequest::Create`]. The engine
//! materializes the realm after the script returns, the same deferred shape
//! an `iframe` insertion uses.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

use tokio::time::Instant;
use url::Url;

use crate::js::world::World;
use crate::js::{
    FrameRuntime, JsError, JsFetchId, JsFetchRequest, JsRealm, PageTimerId, RealmRequest,
    SharedJsRuntime, WorkerScriptError,
};
use crate::protocol::{
    DialCancellation, DialOutcome, DialRequest, FrameId, PolicyContainer, ReferrerRequest,
    RequestPolicy, SandboxFlags, TimerId,
};

/// A worker's share of the engine's in-flight `fetch`/XHR conncurrency cap.
const MAX_PENDING_FETCHES: usize = 256;

/// One dedicated worker's identity inside its engine.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub(crate) struct WorkerId(u64);

impl WorkerId {
    pub(crate) const fn new(raw: u64) -> Self {
        Self(raw)
    }

    pub(crate) const fn get(self) -> u64 {
        self.0
    }
}

/// A worker the constructor queued but the engine has not built yet.
///
/// The realm that constructs a worker captures everything the worker needs
/// from its creator now: the creator can navigate before the engine adopts
/// the request, and the worker must keep the policy and origin it was created
/// with.
pub(crate) struct CreatedWorker {
    pub(crate) id: WorkerId,
    pub(crate) parent: FrameId,
    pub(crate) url: Url,
    pub(crate) name: String,
    pub(crate) module: bool,
    /// Whether this is a `SharedWorker`: the engine keeps one realm per
    /// `(url, name)` and attaches a port for every constructor call
    /// (<https://html.spec.whatwg.org/multipage/workers.html#dom-sharedworker>).
    pub(crate) shared: bool,
    /// The `WorkerOptions.credentials` a shared worker's connections must
    /// agree on; the two scripts fetch under the same mode
    /// (<https://html.spec.whatwg.org/multipage/workers.html#dom-sharedworker>).
    pub(crate) credentials: net::cors::CredentialsMode,
    /// The `WorkerOptions.extendedLifetime` a shared worker's connections must
    /// agree on.
    pub(crate) extended_lifetime: bool,
    pub(crate) origin: url::Origin,
    pub(crate) top_level: url::Origin,
    pub(crate) policy: PolicyContainer,
    pub(crate) sandbox: SandboxFlags,
    pub(crate) referrer_url: Url,
    /// A `blob:` script's source, read from the Blob URL Store while the
    /// constructor runs.
    ///
    /// The constructor starts the fetch in parallel, so revoking the object URL
    /// right after `new Worker()` does not stop it (WPT
    /// `workers/dedicated-worker-from-blob-url.window.html`). The engine adopts
    /// the create after the constructor returns, which would miss a store entry
    /// revoked in between.
    pub(crate) local_script: Option<String>,
}

/// What the constructor asked a worker to be: classic or module, dedicated or
/// shared, and the options two shared connections must agree on.
#[derive(Clone, Copy)]
pub(crate) struct CreatedWorkerOptions {
    pub(crate) module: bool,
    /// Whether this is a `SharedWorker`: the engine keeps one realm per
    /// `(origin, url, name)` and attaches a port for every constructor call
    /// (<https://html.spec.whatwg.org/multipage/workers.html#dom-sharedworker>).
    pub(crate) shared: bool,
    /// The `WorkerOptions.credentials` a shared worker's connections must
    /// agree on; their scripts fetch under the same mode.
    pub(crate) credentials: net::cors::CredentialsMode,
    /// The `WorkerOptions.extendedLifetime` a shared worker's connections must
    /// agree on.
    pub(crate) extended_lifetime: bool,
}

impl CreatedWorkerOptions {
    /// The options of a dedicated worker, which has no shared-realm key.
    pub(crate) const fn dedicated(module: bool) -> Self {
        Self {
            module,
            shared: false,
            credentials: net::cors::CredentialsMode::SameOrigin,
            extended_lifetime: false,
        }
    }
}

impl CreatedWorker {
    /// Captures the creator's origin, policy container, sandbox set, and
    /// referrer for a worker created from `world`.
    pub(crate) fn from_creator(
        world: &World,
        id: WorkerId,
        url: Url,
        name: String,
        options: CreatedWorkerOptions,
    ) -> Self {
        let local_script = (url.scheme() == "blob")
            .then(|| world.object_url_contents(url.as_str()))
            .flatten()
            .map(|contents| contents.to_string());
        Self {
            id,
            parent: world.frame(),
            url,
            name,
            module: options.module,
            shared: options.shared,
            credentials: options.credentials,
            extended_lifetime: options.extended_lifetime,
            origin: world.origin.clone(),
            top_level: world
                .top_level
                .clone()
                .unwrap_or_else(|| world.origin.clone()),
            policy: world.policy.clone(),
            sandbox: world.sandbox,
            referrer_url: world.document_url.clone(),
            local_script,
        }
    }
}

/// A shared worker's identity in one engine.
///
/// The script URL and name select the realm; `origin` keeps data-URL and
/// cross-origin creators apart, because a shared worker is shared only within
/// one origin
/// (<https://html.spec.whatwg.org/multipage/workers.html#shared-worker-creation-steps>).
#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct SharedWorkerKey {
    pub(crate) origin: url::Origin,
    pub(crate) url: Url,
    pub(crate) name: String,
}

impl SharedWorkerKey {
    pub(crate) fn new(origin: url::Origin, url: Url, name: String) -> Self {
        Self { origin, url, name }
    }
}

/// The options two connections to the same shared worker must agree on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SharedWorkerOptions {
    pub(crate) module: bool,
    pub(crate) credentials: net::cors::CredentialsMode,
    pub(crate) extended_lifetime: bool,
}

/// One lifecycle or message request a worker host function queued.
pub(crate) enum WorkerRequest {
    Create(Box<CreatedWorker>),
    /// A new `SharedWorker` connection: the realm keyed by `created`'s URL and
    /// name, plus the worker-side port endpoint that receives the `connect`
    /// event
    /// (<https://html.spec.whatwg.org/multipage/workers.html#dom-sharedworker>).
    CreateShared {
        created: Box<CreatedWorker>,
        endpoint: u64,
    },
    /// One `BroadcastChannel` post from an opaque origin. The engine routes it
    /// to every realm that shares the origin, because the browser keys
    /// broadcasts by storage key and an opaque origin has none
    /// (<https://html.spec.whatwg.org/multipage/web-messaging.html#broadcasting-to-other-browsing-contexts>).
    BroadcastPost {
        origin: url::Origin,
        name: String,
        payload: String,
        channel: u64,
    },
    Post {
        id: WorkerId,
        payload: String,
        ports: Vec<u64>,
    },
    Terminate {
        id: WorkerId,
    },
    Close {
        id: WorkerId,
    },
    PostToParent {
        parent: FrameId,
        worker: WorkerId,
        payload: String,
        ports: Vec<u64>,
    },
    /// A worker that never started, or whose script did not parse.
    ///
    /// Every load, MIME, realm, and parse failure reaches script the same way:
    /// a plain `error` event at the `Worker` object, with no message, and the
    /// environment is discarded
    /// (<https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model>,
    /// step 14).
    LoadFailed {
        worker: WorkerId,
        parent: FrameId,
    },
    /// A shared worker's script failed to load or parse. Every associated
    /// `SharedWorker` object hears a plain `error` event
    /// (<https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model>,
    /// step 14).
    SharedLoadFailed {
        worker: WorkerId,
    },
    Error(Box<WorkerError>),
}

/// A worker failure reported to the `Worker` object in its creating document.
pub(crate) struct WorkerError {
    pub(crate) worker: WorkerId,
    pub(crate) parent: FrameId,
    pub(crate) message: String,
    pub(crate) filename: String,
    pub(crate) lineno: u32,
    pub(crate) colno: u32,
}

impl WorkerError {
    fn from_js(worker: WorkerId, parent: FrameId, filename: &Url, error: &JsError) -> Self {
        Self {
            worker,
            parent,
            message: error.to_string(),
            filename: filename.to_string(),
            lineno: 0,
            colno: 0,
        }
    }
}

/// One queued task in a worker's event loop.
enum WorkerTask {
    Message {
        payload: String,
        ports: Vec<u64>,
    },
    /// One message arriving on a port this worker's realm owns
    /// (<https://html.spec.whatwg.org/multipage/web-messaging.html#message-port-post-message-steps>).
    PortMessage {
        endpoint: u64,
        payload: String,
        ports: Vec<u64>,
    },
    PortClosed {
        endpoint: u64,
    },
    /// One `SharedWorker` connection: deliver a `connect` event carrying the
    /// worker-side port
    /// (<https://html.spec.whatwg.org/multipage/workers.html#shared-workers-and-the-sharedworkerglobalscope-interface>).
    Connect {
        endpoint: u64,
    },
    /// One `BroadcastChannel` message this worker's realm must dispatch.
    Broadcast {
        origin: String,
        name: String,
        payload: String,
        source: Option<u64>,
    },
    Timer(TimerId),
}

/// Why a worker never reached its first turn.
///
/// The spec maps a failed script fetch, a non-JavaScript MIME type, a realm
/// that cannot be created, and a script that does not parse onto one
/// observable: a plain `error` event at the `Worker` object, then the
/// environment is discarded
/// (<https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model>,
/// step 14).
#[derive(Clone, Copy)]
pub(crate) struct WorkerLoadFailure {
    pub(crate) worker: WorkerId,
    pub(crate) parent: FrameId,
}

struct WorkerTimer {
    when: Option<Instant>,
    fired: bool,
    js_id: PageTimerId,
}

/// A worker's top-level script, evaluated at its first turn rather than
/// during `new Worker()`: the constructor returns before the worker runs
/// (<https://html.spec.whatwg.org/multipage/workers.html#dom-worker>).
enum WorkerScript {
    Classic(String),
    Module,
}

/// One live dedicated worker.
pub(crate) struct Worker {
    id: WorkerId,
    parent: FrameId,
    /// Whether this realm is a `SharedWorker`'s global scope. A shared worker
    /// has several owners and receives connections instead of window messages
    /// (<https://html.spec.whatwg.org/multipage/workers.html#shared-workers-and-the-sharedworkerglobalscope-interface>).
    shared: bool,
    /// `worker global scope`'s url: the response URL, which differs from the
    /// request URL after a redirect
    /// (<https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model>,
    /// step 14).
    url: Url,
    world: Rc<RefCell<World>>,
    js: JsRealm,
    script: Option<WorkerScript>,
    tasks: VecDeque<WorkerTask>,
    timers: HashMap<TimerId, WorkerTimer>,
    js_timers: HashMap<PageTimerId, TimerId>,
    next_timer: TimerId,
    /// `fetch`/XHR completions from the dial threads, adopted at the top of a
    /// drain step so the response runs as a worker task
    /// (<https://fetch.spec.whatwg.org/#fetch-processresponse>).
    fetch_tx: Sender<(JsFetchId, Option<DialOutcome>)>,
    fetch_rx: Receiver<(JsFetchId, Option<DialOutcome>)>,
    /// Completions started but not yet adopted. A host that waits on this
    /// never calls the worker idle while a response is in flight.
    pending_fetches: Arc<AtomicUsize>,
    fetches: HashMap<JsFetchId, DialCancellation>,
    closed: bool,
}

impl Worker {
    /// Builds the worker realm and installs its global.
    ///
    /// A script fetch or realm failure is returned as a [`WorkerLoadFailure`];
    /// the caller reports it to the creating document's `Worker` object. The
    /// script itself runs at the worker's first turn, because `new Worker()`
    /// must return before the worker starts
    /// (<https://html.spec.whatwg.org/multipage/workers.html#dom-worker>).
    pub(crate) fn create(
        created: &CreatedWorker,
        id: WorkerId,
        runtime: &FrameRuntime,
        js_runtime: &SharedJsRuntime,
    ) -> Result<Self, WorkerLoadFailure> {
        let failure = WorkerLoadFailure {
            worker: id,
            parent: created.parent,
        };
        let (script, worker_url) = if created.module {
            // A module worker's url is its response URL: relative imports,
            // `fetch`, and XHR resolve against the redirect target, not the
            // request URL
            // (<https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model>,
            // step 14).
            (
                WorkerScript::Module,
                fetch_module_url(created, runtime).map_err(|()| failure)?,
            )
        } else {
            let (source, response_url) =
                fetch_worker_source(created, runtime).map_err(|()| failure)?;
            // A script body is decoded with UTF-8 decode, whose replacement
            // mode substitutes U+FFFD rather than failing the load
            // (<https://infra.spec.whatwg.org/#utf-8-decode>).
            (
                WorkerScript::Classic(String::from_utf8_lossy(&source).into_owned()),
                response_url,
            )
        };
        let world = if created.shared {
            World::new_shared_worker(
                worker_url.clone(),
                FrameId::worker(id.get()),
                runtime,
                id,
                created.name.clone(),
                created.module,
            )
        } else {
            World::new_worker(
                worker_url.clone(),
                FrameId::worker(id.get()),
                created.parent,
                runtime,
                id,
                created.name.clone(),
                created.module,
            )
        };
        let world = Rc::new(RefCell::new(world));
        {
            let mut state = world.borrow_mut();
            // A `data:` worker gets an opaque origin; every other scheme
            // inherits the creator's
            // (<https://html.spec.whatwg.org/multipage/workers.html#dom-worker>).
            if created.url.scheme() == "data" {
                state.origin = url::Origin::new_opaque();
                state.top_level = Some(url::Origin::new_opaque());
            } else {
                state.origin = created.origin.clone();
                state.top_level = Some(created.top_level.clone());
            }
            state.policy = created.policy.clone();
            state.sandbox = created.sandbox;
            state.referrer_url = created.referrer_url.clone();
            state.document_referrer = Some(created.referrer_url.clone());
            // A module worker's top-level graph is fetched with the outside
            // settings object's origin, which keeps `data:` worker static
            // imports same-origin to the creator; the worker keeps its own
            // origin for dynamic imports after the graph runs
            // (<https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model>,
            // step 8).
            if created.module {
                state.state.module_client_origin = Some(created.origin.clone());
            }
        }
        let js = JsRealm::new_worker(
            &js_runtime.handle(),
            Rc::clone(&world),
            Arc::clone(&runtime.stop),
        )
        .map_err(|_| failure)?;
        let (fetch_tx, fetch_rx) = std::sync::mpsc::channel();
        let pending_fetches = Arc::new(AtomicUsize::new(0));
        Ok(Self {
            id,
            parent: created.parent,
            shared: created.shared,
            url: worker_url,
            world,
            js,
            script: Some(script),
            tasks: VecDeque::new(),
            timers: HashMap::new(),
            js_timers: HashMap::new(),
            next_timer: TimerId::default(),
            fetch_tx,
            fetch_rx,
            pending_fetches,
            fetches: HashMap::new(),
            closed: false,
        })
    }

    pub(crate) const fn id(&self) -> WorkerId {
        self.id
    }

    /// Fires one message a nested worker posted at this worker's `Worker`
    /// object
    /// (<https://html.spec.whatwg.org/multipage/workers.html#dom-worker-postmessage>).
    pub(crate) fn deliver_message(
        &self,
        worker: WorkerId,
        payload: &str,
        ports: &[u64],
    ) -> Result<(), JsError> {
        self.js.deliver_worker_event(worker.get(), payload, ports)
    }

    /// Fires one error a nested worker reported at this worker's `Worker`
    /// object
    /// (<https://html.spec.whatwg.org/multipage/workers.html#runtime-script-errors-in-workers>).
    pub(crate) fn deliver_error(&self, error: &WorkerError) -> Result<(), JsError> {
        self.js.deliver_worker_error(
            error.worker.get(),
            &error.message,
            &error.filename,
            error.lineno,
            error.colno,
        )
    }

    /// Fires the plain `error` event a nested worker that never started
    /// reports at its `Worker` object
    /// (<https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model>).
    pub(crate) fn deliver_load_failure(&self, worker: WorkerId) -> Result<(), JsError> {
        self.js.deliver_worker_load_error(worker.get())
    }

    pub(crate) fn queue_message(&mut self, payload: String, ports: Vec<u64>) {
        // `terminate()` empties the implicit port's queue and `close()` sets
        // the closing flag, after which no further tasks may be queued; a
        // message the owner posts later is dropped, not held
        // (<https://html.spec.whatwg.org/multipage/workers.html#terminate-a-worker>).
        if !self.closed {
            self.tasks.push_back(WorkerTask::Message { payload, ports });
        }
    }

    /// Queues a message that arrived on a port this worker's realm owns.
    ///
    /// The endpoint may have moved since the delivery was queued, so the
    /// running task re-checks its owner
    /// (<https://html.spec.whatwg.org/multipage/web-messaging.html#message-port-post-message-steps>).
    pub(crate) fn queue_port_message(&mut self, endpoint: u64, payload: String, ports: Vec<u64>) {
        if self.closed {
            self.forget_endpoints(&ports);
            return;
        }
        self.tasks.push_back(WorkerTask::PortMessage {
            endpoint,
            payload,
            ports,
        });
    }

    pub(crate) fn queue_port_closed(&mut self, endpoint: u64) {
        if !self.closed {
            self.tasks.push_back(WorkerTask::PortClosed { endpoint });
        }
    }

    /// Queues one `connect` event carrying a worker-side port endpoint.
    ///
    /// A shared worker queues connects behind its script, so the script has
    /// installed `onconnect` before the first event fires
    /// (<https://html.spec.whatwg.org/multipage/workers.html#dom-sharedworkerglobalscope-onconnect>).
    pub(crate) fn queue_connect(&mut self, endpoint: u64) {
        if self.closed {
            self.forget_endpoints(&[endpoint]);
            return;
        }
        self.tasks.push_back(WorkerTask::Connect { endpoint });
    }

    /// Queues one `BroadcastChannel` message for this realm.
    pub(crate) fn queue_broadcast(
        &mut self,
        origin: String,
        name: String,
        payload: String,
        source: Option<u64>,
    ) {
        if !self.closed {
            self.tasks.push_back(WorkerTask::Broadcast {
                origin,
                name,
                payload,
                source,
            });
        }
    }

    /// Whether this worker's origin is `origin`.
    pub(crate) fn has_origin(&self, origin: &url::Origin) -> bool {
        self.world.borrow().origin == *origin
    }

    /// Releases transferred endpoints the worker will never adopt.
    fn forget_endpoints(&self, endpoints: &[u64]) {
        if endpoints.is_empty() {
            return;
        }
        self.world
            .borrow()
            .shared()
            .borrow_mut()
            .forget_endpoints(endpoints);
    }

    /// Runs one task and reports whether the engine must step the worker
    /// again. A closed worker drains nothing.
    pub(crate) fn drain_step(&mut self) -> bool {
        if self.closed {
            return false;
        }
        self.adopt_fetches();
        if let Some(script) = self.script.take() {
            let _turn = self.js.enter_turn();
            match script {
                // A runtime exception was reported at the worker global; the
                // worker keeps its event loop either way. Only a script that
                // never compiled discards the environment
                // (<https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model>).
                WorkerScript::Classic(source) => {
                    match self.js.eval_worker_script(&source, self.url.as_str()) {
                        Ok(()) | Err(WorkerScriptError::Runtime) => {}
                        Err(WorkerScriptError::Parse) => {
                            self.report_load_failure();
                            self.closed = true;
                            return false;
                        }
                        // The renderer's turn budget cut a script that never
                        // yielded; stop the worker without a script-visible
                        // event
                        // (<https://html.spec.whatwg.org/multipage/workers.html#terminate-a-worker>).
                        Err(WorkerScriptError::Interrupted) => {
                            self.closed = true;
                            return false;
                        }
                    }
                }
                // A module graph that fails to load or parse is the same
                // environment-discarding failure; a module runtime error is
                // reported at the worker global
                // (<https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model>).
                WorkerScript::Module => match self.js.eval_worker_module(self.url.as_str()) {
                    Ok(()) | Err(WorkerScriptError::Runtime) => {}
                    Err(WorkerScriptError::Parse) => {
                        self.report_load_failure();
                        self.closed = true;
                        return false;
                    }
                    Err(WorkerScriptError::Interrupted) => {
                        self.closed = true;
                        return false;
                    }
                },
            }
            return !self.closed;
        }
        self.adopt_js_work();
        while let Some(id) = self.due_timer() {
            self.tasks.push_back(WorkerTask::Timer(id));
        }
        let Some(task) = self.tasks.pop_front() else {
            return false;
        };
        let _turn = self.js.enter_turn();
        match task {
            WorkerTask::Message { payload, ports } => {
                if let Err(error) = self.js.deliver_worker_message(&payload, &ports) {
                    // The realm could not run the delivery; the ports were
                    // detached from their sender, so they are lost here
                    // (<https://html.spec.whatwg.org/multipage/web-messaging.html#transfer-receiving-steps>).
                    self.forget_endpoints(&ports);
                    self.report_error(&error);
                }
            }
            WorkerTask::PortMessage {
                endpoint,
                payload,
                ports,
            } => self.deliver_port_message(endpoint, payload, ports),
            WorkerTask::PortClosed { endpoint } => self.deliver_port_close(endpoint),
            WorkerTask::Connect { endpoint } => {
                if let Err(error) = self.js.deliver_shared_connect(endpoint) {
                    self.forget_endpoints(&[endpoint]);
                    self.report_error(&error);
                }
            }
            WorkerTask::Broadcast {
                origin,
                name,
                payload,
                source,
            } => {
                if let Err(error) = self
                    .js
                    .deliver_broadcast_message(&origin, &name, &payload, source)
                {
                    self.report_error(&error);
                }
            }
            WorkerTask::Timer(id) => {
                if let Some(timer) = self.timers.remove(&id) {
                    self.js_timers.remove(&timer.js_id);
                    if let Err(error) = self.js.fire_timer(timer.js_id) {
                        self.report_error(&error);
                    }
                }
            }
        }
        !self.closed
    }

    pub(crate) fn has_ready_work(&self) -> bool {
        if self.closed {
            return false;
        }
        if self.script.is_some() || !self.tasks.is_empty() || self.world.borrow().has_outbox_work()
        {
            return true;
        }
        let now = Instant::now();
        self.timers
            .values()
            .any(|timer| !timer.fired && timer.when.is_some_and(|when| when <= now))
    }

    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.timers
            .values()
            .filter(|timer| !timer.fired)
            .filter_map(|timer| timer.when)
            .min()
    }

    pub(crate) const fn is_closed(&self) -> bool {
        self.closed
    }

    pub(crate) fn terminate(&mut self) {
        self.closed = true;
        self.discard_queued_tasks();
        self.timers.clear();
        self.js_timers.clear();
    }

    /// `close()` stops the worker before its next task; the engine drops the
    /// realm after the current drain so it is never destroyed from inside its
    /// own script. Both close and terminate discard the tasks still queued
    /// (<https://html.spec.whatwg.org/multipage/workers.html#close-a-worker>).
    pub(crate) fn close(&mut self) {
        self.closed = true;
        self.discard_queued_tasks();
    }

    /// Drops every queued task, releasing the endpoints its messages carried.
    /// A transferred port is already detached from its sender, so dropping
    /// the message without adopting it would strand the channel.
    fn discard_queued_tasks(&mut self) {
        while let Some(task) = self.tasks.pop_front() {
            match task {
                WorkerTask::Message { ports, .. } | WorkerTask::PortMessage { ports, .. } => {
                    self.forget_endpoints(&ports);
                }
                WorkerTask::Connect { endpoint } => self.forget_endpoints(&[endpoint]),
                WorkerTask::PortClosed { .. }
                | WorkerTask::Broadcast { .. }
                | WorkerTask::Timer(_) => {}
            }
        }
    }

    /// Runs one message that arrived on a port this worker owns.
    ///
    /// The port may have moved to another realm between the message being
    /// queued and this task; it goes back on the port's queue, which the new
    /// realm flushes when the port is enabled there
    /// (<https://html.spec.whatwg.org/multipage/web-messaging.html#message-port-post-message-steps>).
    fn deliver_port_message(&mut self, endpoint: u64, payload: String, ports: Vec<u64>) {
        let frame = self.world.borrow().frame();
        let shared = self.world.borrow().shared();
        if shared.borrow().ports.owner(endpoint) != Some(frame) {
            shared
                .borrow_mut()
                .requeue_port_message(endpoint, payload, ports);
            return;
        }
        match self.js.deliver_port_message(endpoint, &payload, &ports) {
            Ok(true) => {}
            // The payload could not be decoded; the shim already dropped the
            // ports that travelled with it and the port reports `messageerror`
            // (<https://html.spec.whatwg.org/multipage/web-messaging.html#message-port-post-message-steps>).
            Ok(false) => {
                if let Err(error) = self.js.deliver_port_message_error(endpoint) {
                    self.report_error(&error);
                }
            }
            Err(error) => {
                self.forget_endpoints(&ports);
                self.report_error(&error);
            }
        }
    }

    fn deliver_port_close(&mut self, endpoint: u64) {
        let frame = self.world.borrow().frame();
        let shared = self.world.borrow().shared();
        if shared.borrow().ports.owner(endpoint) != Some(frame) {
            shared.borrow_mut().requeue_port_close(endpoint);
            return;
        }
        if let Err(error) = self.js.deliver_port_close(endpoint) {
            self.report_error(&error);
        }
    }

    fn report_error(&mut self, error: &JsError) {
        // Runtime errors in a shared worker fire at its global scope only; no
        // `SharedWorker` object hears them
        // (<https://html.spec.whatwg.org/multipage/workers.html#runtime-script-errors-2>).
        if self.shared {
            return;
        }
        let error = WorkerError::from_js(self.id, self.parent, &self.url, error);
        self.world
            .borrow()
            .runtime
            .worker_requests
            .borrow_mut()
            .push(WorkerRequest::Error(Box::new(error)));
    }

    fn report_load_failure(&self) {
        let request = if self.shared {
            WorkerRequest::SharedLoadFailed { worker: self.id }
        } else {
            WorkerRequest::LoadFailed {
                worker: self.id,
                parent: self.parent,
            }
        };
        self.world
            .borrow()
            .runtime
            .worker_requests
            .borrow_mut()
            .push(request);
    }

    fn adopt_js_work(&mut self) {
        loop {
            let Some(request) = self.world.borrow().pop_outbox() else {
                break;
            };
            self.apply_realm_request(request);
        }
    }

    fn apply_realm_request(&mut self, request: RealmRequest) {
        match request {
            RealmRequest::Timer(timeout) => {
                let id = self.schedule_timer(timeout.delay, timeout.js_id);
                self.js_timers.insert(timeout.js_id, id);
            }
            RealmRequest::ClearTimer(js_id) => {
                if let Some(id) = self.js_timers.remove(&js_id) {
                    self.timers.remove(&id);
                }
            }
            RealmRequest::Fetch(fetch) => self.start_fetch(*fetch),
            RealmRequest::AbortFetch(js_id) => {
                if let Some(cancel) = self.fetches.remove(&js_id) {
                    cancel();
                }
            }
            RealmRequest::Script(_)
            | RealmRequest::HistoryPush { .. }
            | RealmRequest::HistoryTraverse { .. }
            | RealmRequest::CspViolation(_, _)
            | RealmRequest::DeclarativeRefresh(_)
            | RealmRequest::HtmlWrite(_)
            | RealmRequest::DocumentStream(_) => {}
        }
    }

    /// Starts one `fetch`/XHR dial for this worker.
    ///
    /// The completion goes through a channel and is adopted in the worker's
    /// next turn, so the response callback runs as worker script rather than
    /// on the dial thread
    /// (<https://fetch.spec.whatwg.org/#fetch-processresponse>).
    fn start_fetch(&mut self, fetch: JsFetchRequest) {
        let js_id = fetch.js_id;
        if self.pending_fetches.load(Ordering::Acquire) >= MAX_PENDING_FETCHES {
            self.fail_fetch(js_id);
            return;
        }
        let Ok(url) = Url::parse(&fetch.url) else {
            self.fail_fetch(js_id);
            return;
        };
        let (origin, referrer_url, referrer_policy, gate) = {
            let world = self.world.borrow();
            (
                world.origin.clone(),
                world.referrer_url.clone(),
                world.policy.referrer_policy,
                crate::RedirectGate::new(
                    world.policy.csp.clone(),
                    &world.origin,
                    crate::csp::Destination::Connect,
                ),
            )
        };
        let mut dial = DialRequest::new(RequestPolicy::Fetch(fetch.options.cors), &url, &origin);
        dial.method = fetch.method;
        dial.body = fetch.body;
        dial.content_type = fetch.content_type;
        dial.headers = fetch.headers;
        dial.referrer = match fetch.options.referrer {
            ReferrerRequest::Client { .. } => ReferrerRequest::Client { url: referrer_url },
            referrer => referrer,
        };
        dial.referrer_policy = fetch
            .options
            .referrer_policy
            .or(referrer_policy)
            .unwrap_or_default();
        dial.keepalive = fetch.options.keepalive;
        dial.body_len = fetch.options.body_len;
        dial.integrity = fetch.options.integrity;
        dial.cache = fetch.options.cache;
        dial.delivery = fetch.options.delivery;
        dial.csp = Some(Box::new(gate));
        let tx = self.fetch_tx.clone();
        self.pending_fetches.fetch_add(1, Ordering::Release);
        let cancel = self.world.borrow().runtime.services.start_dial(
            dial,
            Box::new(move |outcome| {
                // A failure maps to the same empty outcome a rejected fetch
                // reports; the shim turns it into a `TypeError`
                // (<https://fetch.spec.whatwg.org/#concept-network-error>).
                let _ = tx.send((js_id, outcome.ok()));
            }),
        );
        self.fetches.insert(js_id, cancel);
    }

    /// Adopts every settled fetch and runs its callback.
    fn adopt_fetches(&mut self) {
        let _turn = self.js.enter_turn();
        while let Ok((js_id, outcome)) = self.fetch_rx.try_recv() {
            self.pending_fetches.fetch_sub(1, Ordering::Release);
            self.fetches.remove(&js_id);
            if let Err(error) = self.js.finish_js_fetch(js_id, outcome) {
                self.report_error(&error);
            }
            self.adopt_js_work();
        }
    }

    fn fail_fetch(&mut self, js_id: JsFetchId) {
        if let Err(error) = self.js.finish_js_fetch(js_id, None) {
            self.report_error(&error);
        }
    }

    /// Whether this worker still owes a fetch completion.
    pub(crate) fn has_pending_dials(&self) -> bool {
        self.pending_fetches.load(Ordering::Acquire) > 0
    }

    fn schedule_timer(&mut self, delay: Duration, js_id: PageTimerId) -> TimerId {
        let id = self.next_timer.take_next();
        self.timers.insert(
            id,
            WorkerTimer {
                when: Instant::now().checked_add(delay),
                fired: false,
                js_id,
            },
        );
        id
    }

    fn due_timer(&mut self) -> Option<TimerId> {
        let now = Instant::now();
        let id = self
            .timers
            .iter()
            .find(|(_, timer)| !timer.fired && timer.when.is_some_and(|when| when <= now))
            .map(|(&id, _)| id)?;
        if let Some(timer) = self.timers.get_mut(&id) {
            timer.fired = true;
        }
        Some(id)
    }
}

/// Resolves a module worker's script URL before its realm exists.
///
/// The realm is built with the response URL so the worker's base URL is the
/// redirect target. The module graph itself is fetched again at the worker's
/// first turn, where the loader applies the MIME check and CSP
/// (<https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model>).
fn fetch_module_url(created: &CreatedWorker, runtime: &FrameRuntime) -> Result<Url, ()> {
    if created.url.scheme() == "data" || created.url.scheme() == "blob" {
        return Ok(created.url.clone());
    }
    if created.url.scheme() != "http" && created.url.scheme() != "https" {
        return Err(());
    }
    let mut dial = DialRequest::new(
        RequestPolicy::ModuleWorkerScript,
        &created.url,
        &created.origin,
    );
    dial.referrer = ReferrerRequest::Client {
        url: created.referrer_url.clone(),
    };
    dial.referrer_policy = created.policy.referrer_policy.unwrap_or_default();
    dial.timeout_ms = 30_000;
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    let cancel = runtime.services.start_dial(
        dial,
        Box::new(move |outcome| {
            let _ = send.send(outcome);
        }),
    );
    let outcome = match receive.recv_timeout(Duration::from_secs(30)) {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(_)) => return Err(()),
        Err(_) => {
            cancel();
            return Err(());
        }
    };
    if !(200..300).contains(&outcome.status) {
        return Err(());
    }
    Ok(outcome.final_url)
}

/// Fetches a classic worker's script and reports the URL its response came
/// from.
///
/// The dial uses the same-origin request mode, so every redirect hop must stay
/// on the creator's origin. `data:` is decoded in this process; any failed
/// fetch, status, or MIME check is a load failure, which script sees as a plain
/// `error` event at the `Worker` object
/// (<https://html.spec.whatwg.org/multipage/workers.html#fetch-a-classic-worker-script>).
fn fetch_worker_source(
    created: &CreatedWorker,
    runtime: &FrameRuntime,
) -> Result<(Vec<u8>, Url), ()> {
    if created.url.scheme() == "data" {
        // A `data:` worker script is not MIME-checked; every MIME type is
        // allowed, including an empty one (WPT `workers/data-url.html`).
        let (_, body) = crate::engine::decode_data_url_bounded(created.url.as_str()).ok_or(())?;
        return Ok((body, created.url.clone()));
    }
    if created.url.scheme() == "blob" {
        // A `blob:` worker script is the store entry read when the constructor
        // ran. Only a network-scheme response is MIME-checked
        // (<https://html.spec.whatwg.org/multipage/webappapis.html#fetch-a-classic-worker-script>,
        // step 5), so the backing Blob's type is irrelevant
        // (WPT `workers/Worker_script_mimetype.htm`).
        return match &created.local_script {
            Some(source) => Ok((source.clone().into_bytes(), created.url.clone())),
            None => Err(()),
        };
    }
    if created.url.scheme() != "http" && created.url.scheme() != "https" {
        return Err(());
    }
    let mut dial = DialRequest::new(RequestPolicy::WorkerScript, &created.url, &created.origin);
    dial.referrer = ReferrerRequest::Client {
        url: created.referrer_url.clone(),
    };
    dial.referrer_policy = created.policy.referrer_policy.unwrap_or_default();
    dial.timeout_ms = 30_000;
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    let cancel = runtime.services.start_dial(
        dial,
        Box::new(move |outcome| {
            let _ = send.send(outcome);
        }),
    );
    let outcome = match receive.recv_timeout(Duration::from_secs(30)) {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(_)) => return Err(()),
        Err(_) => {
            cancel();
            return Err(());
        }
    };
    if !(200..300).contains(&outcome.status) {
        return Err(());
    }
    // A worker script request uses the same-origin mode, so every redirect
    // hop must stay on the creator's origin
    // (<https://html.spec.whatwg.org/multipage/workers.html#fetch-a-classic-worker-script>).
    if outcome.final_url.origin() != created.origin {
        return Err(());
    }
    if let Some(content_type) = &outcome.content_type
        && !crate::js::javascript_mime_essence(content_type_essence(content_type))
    {
        return Err(());
    }
    match outcome.body {
        crate::protocol::ResponseBody::Bytes(body) => Ok((body, outcome.final_url)),
        crate::protocol::ResponseBody::Streamed(_) | crate::protocol::ResponseBody::Undecodable => {
            Err(())
        }
    }
}

/// The essence of a `Content-Type` value, without parameters or whitespace.
fn content_type_essence(value: &str) -> &str {
    value.split(';').next().unwrap_or_default().trim()
}
