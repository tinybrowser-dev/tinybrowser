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
use std::time::Duration;

use tokio::time::Instant;
use url::Url;

use crate::js::world::World;
use crate::js::{FrameRuntime, JsError, JsRealm, PageTimerId, RealmRequest, SharedJsRuntime};
use crate::protocol::{
    DialRequest, FrameId, PolicyContainer, ReferrerRequest, RequestPolicy, SandboxFlags, TimerId,
};

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
    pub(crate) origin: url::Origin,
    pub(crate) top_level: url::Origin,
    pub(crate) policy: PolicyContainer,
    pub(crate) sandbox: SandboxFlags,
    pub(crate) referrer_url: Url,
}

impl CreatedWorker {
    /// Captures the creator's origin, policy container, sandbox set, and
    /// referrer for a worker created from `world`.
    pub(crate) fn from_creator(
        world: &World,
        id: WorkerId,
        url: Url,
        name: String,
        module: bool,
    ) -> Self {
        Self {
            id,
            parent: world.frame(),
            url,
            name,
            module,
            origin: world.origin.clone(),
            top_level: world
                .top_level
                .clone()
                .unwrap_or_else(|| world.origin.clone()),
            policy: world.policy.clone(),
            sandbox: world.sandbox,
            referrer_url: world.document_url.clone(),
        }
    }
}

/// One lifecycle or message request a worker host function queued.
pub(crate) enum WorkerRequest {
    Create(Box<CreatedWorker>),
    Post {
        id: WorkerId,
        payload: String,
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
    fn new(created: &CreatedWorker, message: impl Into<String>) -> Self {
        Self {
            worker: created.id,
            parent: created.parent,
            message: message.into(),
            filename: created.url.to_string(),
            lineno: 0,
            colno: 0,
        }
    }

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
    Message(String),
    Timer(TimerId),
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
    url: Url,
    world: Rc<RefCell<World>>,
    js: JsRealm,
    script: Option<WorkerScript>,
    tasks: VecDeque<WorkerTask>,
    timers: HashMap<TimerId, WorkerTimer>,
    js_timers: HashMap<PageTimerId, TimerId>,
    next_timer: TimerId,
    closed: bool,
}

impl Worker {
    /// Builds the worker realm and installs its global.
    ///
    /// A script fetch or realm failure is returned as a [`WorkerError`]; the
    /// caller reports it to the creating document's `Worker` object. The
    /// script itself runs at the worker's first turn, because `new Worker()`
    /// must return before the worker starts
    /// (<https://html.spec.whatwg.org/multipage/workers.html#dom-worker>).
    pub(crate) fn create(
        created: CreatedWorker,
        runtime: &FrameRuntime,
        js_runtime: &SharedJsRuntime,
    ) -> Result<Self, WorkerError> {
        let script = if created.module {
            WorkerScript::Module
        } else {
            let source = fetch_worker_source(&created, runtime)?;
            WorkerScript::Classic(
                String::from_utf8(source)
                    .map_err(|_| WorkerError::new(&created, "worker script is not UTF-8"))?,
            )
        };
        let world = World::new_worker(
            created.url.clone(),
            FrameId::worker(created.id.get()),
            created.parent,
            runtime,
            created.id,
            created.name.clone(),
        );
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
        }
        let js = JsRealm::new_worker(
            &js_runtime.handle(),
            Rc::clone(&world),
            Arc::clone(&runtime.stop),
        )
        .map_err(|error| WorkerError::from_js(created.id, created.parent, &created.url, &error))?;
        Ok(Self {
            id: created.id,
            parent: created.parent,
            url: created.url,
            world,
            js,
            script: Some(script),
            tasks: VecDeque::new(),
            timers: HashMap::new(),
            js_timers: HashMap::new(),
            next_timer: TimerId::default(),
            closed: false,
        })
    }

    pub(crate) const fn id(&self) -> WorkerId {
        self.id
    }

    pub(crate) fn queue_message(&mut self, payload: String) {
        self.tasks.push_back(WorkerTask::Message(payload));
    }

    /// Runs one task and reports whether the engine must step the worker
    /// again. A closed worker drains nothing.
    pub(crate) fn drain_step(&mut self) -> bool {
        if self.closed {
            return false;
        }
        if let Some(script) = self.script.take() {
            let _turn = self.js.enter_turn();
            let (evaluated, reports) = match script {
                // The classic path reports through the realm's `error` event,
                // which propagates to the owner unless the worker global
                // cancels it
                // (<https://html.spec.whatwg.org/multipage/webappapis.html#report-the-error>).
                WorkerScript::Classic(source) => (
                    self.js.eval_classic_script(&source, 0, self.url.as_str()),
                    false,
                ),
                WorkerScript::Module => (self.js.eval_external_module(self.url.as_str()), true),
            };
            if let Err(error) = evaluated {
                if reports {
                    self.report_error(&error);
                }
                self.closed = true;
                return false;
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
            WorkerTask::Message(payload) => {
                if let Err(error) = self.js.deliver_worker_message(&payload) {
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
        self.tasks.clear();
        self.timers.clear();
        self.js_timers.clear();
    }

    /// `close()` stops the worker before its next task; the engine drops the
    /// realm after the current drain so it is never destroyed from inside its
    /// own script.
    pub(crate) fn close(&mut self) {
        self.closed = true;
    }

    fn report_error(&mut self, error: &JsError) {
        let error = WorkerError::from_js(self.id, self.parent, &self.url, error);
        self.world
            .borrow()
            .runtime
            .worker_requests
            .borrow_mut()
            .push(WorkerRequest::Error(Box::new(error)));
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
            RealmRequest::Fetch(fetch) => {
                if let Err(error) = self.js.finish_js_fetch(fetch.js_id, None) {
                    self.report_error(&error);
                }
            }
            RealmRequest::AbortFetch(_)
            | RealmRequest::Script(_)
            | RealmRequest::HistoryPush { .. }
            | RealmRequest::HistoryTraverse { .. }
            | RealmRequest::CspViolation(_, _)
            | RealmRequest::DeclarativeRefresh(_)
            | RealmRequest::HtmlWrite(_)
            | RealmRequest::DocumentStream(_) => {}
        }
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

/// Fetches a classic worker's script.
///
/// The dial uses the same-origin request mode, so every redirect hop must stay
/// on the creator's origin. `data:` is decoded in this process; a response
/// whose MIME type is not JavaScript is a load failure
/// (<https://html.spec.whatwg.org/multipage/workers.html#fetch-a-classic-worker-script>).
fn fetch_worker_source(
    created: &CreatedWorker,
    runtime: &FrameRuntime,
) -> Result<Vec<u8>, WorkerError> {
    if created.url.scheme() == "data" {
        // A `data:` worker script is not MIME-checked; every MIME type is
        // allowed, including an empty one (WPT `workers/data-url.html`).
        let (_, body) = crate::engine::decode_data_url_bounded(created.url.as_str())
            .ok_or_else(|| WorkerError::new(created, "worker script could not be decoded"))?;
        return Ok(body);
    }
    if created.url.scheme() != "http" && created.url.scheme() != "https" {
        return Err(WorkerError::new(
            created,
            "worker script URL is not supported",
        ));
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
        Ok(Err(failure)) => {
            return Err(WorkerError::new(
                created,
                format!("worker script dial failed: {failure:?}"),
            ));
        }
        Err(_) => {
            cancel();
            return Err(WorkerError::new(created, "worker script dial timed out"));
        }
    };
    if !(200..300).contains(&outcome.status) {
        return Err(WorkerError::new(
            created,
            format!("worker script returned HTTP {}", outcome.status),
        ));
    }
    // A worker script request uses the same-origin mode, so every redirect
    // hop must stay on the creator's origin
    // (<https://html.spec.whatwg.org/multipage/workers.html#fetch-a-classic-worker-script>).
    if outcome.final_url.origin() != created.origin {
        return Err(WorkerError::new(
            created,
            "worker script redirected to a different origin",
        ));
    }
    if let Some(content_type) = &outcome.content_type
        && !crate::js::javascript_mime_essence(content_type_essence(content_type))
    {
        return Err(WorkerError::new(
            created,
            "worker script has a non-JavaScript MIME type",
        ));
    }
    match outcome.body {
        crate::protocol::ResponseBody::Bytes(body) => Ok(body),
        crate::protocol::ResponseBody::Streamed(_) => Err(WorkerError::new(
            created,
            "worker script response was streamed",
        )),
        crate::protocol::ResponseBody::Undecodable => Err(WorkerError::new(
            created,
            "worker script body could not be decoded",
        )),
    }
}

/// The essence of a `Content-Type` value, without parameters or whitespace.
fn content_type_essence(value: &str) -> &str {
    value.split(';').next().unwrap_or_default().trim()
}
