// https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model
// https://html.spec.whatwg.org/multipage/workers.html#dedicated-workers-and-the-dedicatedworkerglobalscope-interface
// https://html.spec.whatwg.org/multipage/workers.html#dom-worker

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::time::Instant;
use url::Url;

use crate::js::world::World;
use crate::js::{
    FrameRuntime, JsError, JsFetchId, JsFetchRequest, JsRealm, JsRuntimeHandle, PageTimerId,
    RealmRequest, SharedJsRuntime, WorkerScriptError,
};
use crate::protocol::{
    DialCancellation, DialOutcome, DialRequest, FrameId, PolicyContainer, ReferrerRequest,
    RequestPolicy, SandboxFlags, TimerId,
};

const MAX_PENDING_FETCHES: usize = 256;

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

// https://privacycg.github.io/saa-non-cookie-storage/#shared-worker
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum SharedWorkerSameSiteCookies {
    All,
    None,
}

impl SharedWorkerSameSiteCookies {
    pub(crate) fn from_idl(value: &str) -> Option<Self> {
        match value {
            "all" => Some(Self::All),
            "none" => Some(Self::None),
            _ => None,
        }
    }

    pub(crate) const fn cookie_policy(self) -> net::SameSiteCookiePolicy {
        match self {
            Self::All => net::SameSiteCookiePolicy::All,
            Self::None => net::SameSiteCookiePolicy::NoneOnly,
        }
    }
}

pub(crate) struct CreatedWorker {
    pub(crate) id: WorkerId,
    pub(crate) parent: FrameId,
    pub(crate) url: Url,
    pub(crate) name: String,
    pub(crate) module: bool,
    // https://html.spec.whatwg.org/multipage/workers.html#dom-sharedworker
    pub(crate) shared: bool,
    // https://html.spec.whatwg.org/multipage/workers.html#dom-sharedworker
    pub(crate) credentials: net::cors::CredentialsMode,
    pub(crate) extended_lifetime: bool,
    // https://privacycg.github.io/saa-non-cookie-storage/#shared-worker
    pub(crate) same_site_cookies: SharedWorkerSameSiteCookies,
    pub(crate) origin: url::Origin,
    // https://html.spec.whatwg.org/multipage/workers.html#dom-worker
    pub(crate) worker_origin: url::Origin,
    pub(crate) top_level: url::Origin,
    // https://github.com/privacycg/CHIPS#computing-the-cookie-partition-key
    pub(crate) partition_key: Option<String>,
    pub(crate) policy: PolicyContainer,
    pub(crate) sandbox: SandboxFlags,
    pub(crate) referrer_url: Url,
    pub(crate) local_script: Option<String>,
}

#[derive(Clone, Copy)]
pub(crate) struct CreatedWorkerOptions {
    pub(crate) module: bool,
    // https://html.spec.whatwg.org/multipage/workers.html#dom-sharedworker
    pub(crate) shared: bool,
    pub(crate) credentials: net::cors::CredentialsMode,
    pub(crate) extended_lifetime: bool,
    pub(crate) same_site_cookies: SharedWorkerSameSiteCookies,
}

impl CreatedWorkerOptions {
    pub(crate) const fn dedicated(module: bool, credentials: net::cors::CredentialsMode) -> Self {
        Self {
            module,
            shared: false,
            credentials,
            extended_lifetime: false,
            same_site_cookies: SharedWorkerSameSiteCookies::All,
        }
    }
}

impl CreatedWorker {
    pub(crate) fn from_creator(
        world: &World,
        id: WorkerId,
        url: Url,
        name: String,
        options: CreatedWorkerOptions,
    ) -> Self {
        let worker_origin = if url.scheme() == "data" {
            url::Origin::new_opaque()
        } else {
            world.origin.clone()
        };
        let local_script = (url.scheme() == "blob")
            .then(|| world.object_url_contents(url.as_str()))
            .flatten()
            .map(|contents| contents.to_string());
        let top_level = world
            .top_level
            .clone()
            .unwrap_or_else(|| world.origin.clone());
        let partition_key = net::site_for_origin(&top_level);
        Self {
            id,
            parent: world.frame(),
            url,
            name,
            module: options.module,
            shared: options.shared,
            credentials: options.credentials,
            extended_lifetime: options.extended_lifetime,
            same_site_cookies: options.same_site_cookies,
            origin: world.origin.clone(),
            worker_origin,
            top_level,
            partition_key,
            policy: world.policy.clone(),
            sandbox: world.sandbox,
            referrer_url: world.document_url.clone(),
            local_script,
        }
    }
}

// https://html.spec.whatwg.org/multipage/workers.html#dom-sharedworker
// https://storage.spec.whatwg.org/#obtain-a-storage-key-for-non-storage-purposes
#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct SharedWorkerKey {
    pub(crate) origin: url::Origin,
    pub(crate) top_level_site: Option<String>,
    pub(crate) url: Url,
    pub(crate) name: String,
    // https://privacycg.github.io/saa-non-cookie-storage/#shared-worker
    pub(crate) same_site_cookies: SharedWorkerSameSiteCookies,
}

impl SharedWorkerKey {
    pub(crate) fn new(
        origin: url::Origin,
        top_level_site: Option<String>,
        url: Url,
        name: String,
        same_site_cookies: SharedWorkerSameSiteCookies,
    ) -> Self {
        Self {
            origin,
            top_level_site,
            url,
            name,
            same_site_cookies,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SharedWorkerOptions {
    pub(crate) module: bool,
    pub(crate) credentials: net::cors::CredentialsMode,
    pub(crate) extended_lifetime: bool,
    pub(crate) same_site_cookies: SharedWorkerSameSiteCookies,
}

pub(crate) enum WorkerRequest {
    // https://html.spec.whatwg.org/multipage/workers.html#dom-sharedworker
    CreateShared {
        created: Box<CreatedWorker>,
        endpoint: u64,
    },
    // https://html.spec.whatwg.org/multipage/web-messaging.html#broadcasting-to-other-browsing-contexts
    BroadcastPost {
        origin: url::Origin,
        name: String,
        payload: String,
        channel: u64,
    },
    PostToParent {
        parent: FrameId,
        worker: WorkerId,
        payload: String,
        ports: Vec<u64>,
    },
    SetTimer {
        worker: WorkerId,
        js_id: PageTimerId,
        delay: Duration,
    },
    ClearTimer {
        worker: WorkerId,
        js_id: PageTimerId,
    },
    // https://html.spec.whatwg.org/multipage/workers.html#dom-sharedworkerglobalscope-close
    Close {
        id: WorkerId,
    },
    // https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model
    LoadFailed {
        worker: WorkerId,
        parent: FrameId,
    },
    // https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model
    SharedLoadFailed {
        worker: WorkerId,
    },
    // https://w3c.github.io/webappsec-csp/#report-violation
    CspViolation {
        parent: FrameId,
        violations: Vec<crate::csp::Violation>,
    },
    Error(Box<WorkerError>),
}

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

pub(crate) enum WorkerCommand {
    Post {
        payload: String,
        ports: Vec<u64>,
    },
    PortMessage {
        endpoint: u64,
        payload: String,
        ports: Vec<u64>,
    },
    PortClosed {
        endpoint: u64,
    },
    Broadcast {
        origin: String,
        name: String,
        payload: String,
        source: Option<u64>,
    },
    FireTimer {
        js_id: PageTimerId,
    },
    NestedMessage {
        worker: u64,
        payload: String,
        ports: Vec<u64>,
    },
    NestedError(Box<WorkerError>),
    NestedLoadFailure {
        worker: u64,
    },
    CspViolation {
        violations: Vec<crate::csp::Violation>,
    },
    Terminate,
}

pub(crate) struct WorkerThreadState {
    // https://html.spec.whatwg.org/multipage/workers.html#terminate-a-worker
    terminate: Arc<AtomicBool>,
    active: Arc<AtomicUsize>,
    closed: Arc<AtomicBool>,
    pending_fetches: Arc<AtomicUsize>,
}

pub(crate) struct WorkerDirectory {
    runtime: Arc<crate::js::SharedRuntime>,
    workers: Mutex<BTreeMap<WorkerId, WorkerHandle>>,
}

impl WorkerDirectory {
    pub(crate) fn new(runtime: Arc<crate::js::SharedRuntime>) -> Self {
        Self {
            runtime,
            workers: Mutex::new(BTreeMap::new()),
        }
    }

    fn workers(&self) -> MutexGuard<'_, BTreeMap<WorkerId, WorkerHandle>> {
        self.workers.lock().unwrap_or_else(PoisonError::into_inner)
    }

    // https://bellard.org/quickjs/quickjs.html
    fn thread_runtime(self: &Arc<Self>, js_runtime: JsRuntimeHandle) -> FrameRuntime {
        FrameRuntime {
            common: Arc::clone(&self.runtime),
            identity: Rc::new(RefCell::new(self.runtime.initial_identity.clone())),
            js_runtime,
            documents: Rc::new(RefCell::new(crate::documents::DocumentStore::default())),
            registry: Rc::new(RefCell::new(crate::js::RealmRegistry::default())),
            pending_storage: Rc::new(RefCell::new(Vec::new())),
            turn_deadline: Rc::new(std::cell::Cell::new(None)),
            worker_dir: Arc::clone(self),
            font_ctx: self.runtime.font_ctx.clone(),
        }
    }

    // https://html.spec.whatwg.org/multipage/workers.html#dom-worker
    pub(crate) fn spawn_dedicated(self: &Arc<Self>, created: CreatedWorker) -> WorkerId {
        // https://html.spec.whatwg.org/multipage/workers.html#dom-worker — UA cap matching Firefox dom.workers.maxPerDomain
        const MAX_DEDICATED_WORKERS: usize = 512;
        let id = created.id;
        let (commands, received) = std::sync::mpsc::channel();
        let state = WorkerThreadState {
            terminate: Arc::new(AtomicBool::new(false)),
            active: Arc::new(AtomicUsize::new(1)),
            closed: Arc::new(AtomicBool::new(false)),
            pending_fetches: Arc::new(AtomicUsize::new(0)),
        };
        let handle = WorkerHandle {
            origin: created.worker_origin.clone(),
            commands,
            terminate: Arc::clone(&state.terminate),
            active: Arc::clone(&state.active),
            closed: Arc::clone(&state.closed),
            pending_fetches: Arc::clone(&state.pending_fetches),
            timers: HashMap::new(),
            js_timers: HashMap::new(),
            next_timer: TimerId::default(),
            closed_local: false,
        };
        {
            let mut workers = self.workers();
            workers.retain(|_, worker| !worker.is_closed());
            if workers.len() >= MAX_DEDICATED_WORKERS {
                state.closed.store(true, Ordering::Release);
                state.active.store(0, Ordering::Release);
                self.runtime
                    .worker_requests
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(WorkerRequest::LoadFailed {
                        worker: id,
                        parent: created.parent,
                    });
                self.runtime.wake.notify_one();
                return id;
            }
            workers.insert(id, handle);
        }

        let directory = Arc::clone(self);
        let thread_state = WorkerThreadState {
            terminate: Arc::clone(&state.terminate),
            active: Arc::clone(&state.active),
            closed: Arc::clone(&state.closed),
            pending_fetches: Arc::clone(&state.pending_fetches),
        };
        let parent = created.parent;
        let spawned = std::thread::Builder::new()
            .name(format!("tinybrowser-worker-{}", id.get()))
            .spawn(move || run_worker(&created, &directory, &received, &thread_state));
        if let Err(error) = spawned {
            // https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model
            let _ = error;
            state.closed.store(true, Ordering::Release);
            state.active.store(0, Ordering::Release);
            self.runtime
                .worker_requests
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(WorkerRequest::LoadFailed { worker: id, parent });
            self.runtime.wake.notify_one();
        }
        id
    }

    pub(crate) fn send(&self, id: WorkerId, command: WorkerCommand) -> bool {
        let mut workers = self.workers();
        let Some(worker) = workers.get_mut(&id) else {
            return false;
        };
        worker.send(command)
    }

    pub(crate) fn terminate(&self, id: WorkerId) {
        if let Some(worker) = self.workers().get_mut(&id) {
            worker.terminate();
        }
    }

    pub(crate) fn with_workers<R>(
        &self,
        f: impl FnOnce(&mut BTreeMap<WorkerId, WorkerHandle>) -> R,
    ) -> R {
        f(&mut self.workers())
    }

    pub(crate) fn terminate_all(&self) {
        let mut workers = self.workers();
        for worker in workers.values_mut() {
            worker.terminate();
        }
        workers.clear();
    }

    pub(crate) fn remove_closed(&self) {
        self.workers().retain(|_, worker| !worker.is_closed());
    }
}

pub(crate) struct WorkerHandle {
    origin: url::Origin,
    commands: Sender<WorkerCommand>,
    terminate: Arc<AtomicBool>,
    active: Arc<AtomicUsize>,
    closed: Arc<AtomicBool>,
    pending_fetches: Arc<AtomicUsize>,
    timers: HashMap<TimerId, WorkerTimer>,
    js_timers: HashMap<PageTimerId, TimerId>,
    next_timer: TimerId,
    closed_local: bool,
}

impl WorkerHandle {
    pub(crate) fn has_origin(&self, origin: &url::Origin) -> bool {
        self.origin == *origin
    }

    fn send(&mut self, command: WorkerCommand) -> bool {
        if self.closed_local {
            return false;
        }
        self.active.fetch_add(1, Ordering::AcqRel);
        if self.commands.send(command).is_err() {
            self.active.fetch_sub(1, Ordering::AcqRel);
            return false;
        }
        true
    }

    pub(crate) fn queue_broadcast(
        &mut self,
        origin: String,
        name: String,
        payload: String,
        source: Option<u64>,
    ) {
        self.send(WorkerCommand::Broadcast {
            origin,
            name,
            payload,
            source,
        });
    }

    pub(crate) fn drain_step(&mut self) -> bool {
        if self.closed_local {
            return false;
        }
        self.fire_due_timers();
        if self.active.load(Ordering::Acquire) == 0 {
            return false;
        }
        std::thread::yield_now();
        true
    }

    pub(crate) fn has_ready_work(&self) -> bool {
        if self.closed_local {
            return false;
        }
        if self.active.load(Ordering::Acquire) > 0 {
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

    pub(crate) fn has_pending_dials(&self) -> bool {
        self.pending_fetches.load(Ordering::Acquire) > 0
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.closed_local || self.closed.load(Ordering::Acquire)
    }

    pub(crate) fn schedule_timer(&mut self, delay: Duration, js_id: PageTimerId) {
        let id = self.next_timer.take_next();
        self.timers.insert(
            id,
            WorkerTimer {
                when: Instant::now().checked_add(delay),
                fired: false,
                js_id,
            },
        );
        self.js_timers.insert(js_id, id);
    }

    pub(crate) fn clear_timer(&mut self, js_id: PageTimerId) {
        if let Some(id) = self.js_timers.remove(&js_id) {
            self.timers.remove(&id);
        }
    }

    pub(crate) fn terminate(&mut self) {
        self.closed_local = true;
        self.terminate.store(true, Ordering::Release);
        self.timers.clear();
        self.js_timers.clear();
        self.active.fetch_add(1, Ordering::AcqRel);
        if self.commands.send(WorkerCommand::Terminate).is_err() {
            self.active.fetch_sub(1, Ordering::AcqRel);
        }
    }

    fn fire_due_timers(&mut self) {
        while let Some(id) = self.due_timer() {
            let Some(timer) = self.timers.get(&id) else {
                continue;
            };
            let js_id = timer.js_id;
            self.send(WorkerCommand::FireTimer { js_id });
        }
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

fn run_worker(
    created: &CreatedWorker,
    directory: &Arc<WorkerDirectory>,
    commands: &Receiver<WorkerCommand>,
    state: &WorkerThreadState,
) {
    let js_runtime = SharedJsRuntime::default();
    let runtime = directory.thread_runtime(js_runtime.handle());
    let worker = Worker::create(
        created,
        created.id,
        &runtime,
        &js_runtime,
        Arc::clone(&state.terminate),
        Arc::clone(&state.pending_fetches),
    );
    let mut worker = match worker {
        Ok(worker) => worker,
        Err(failure) => {
            // https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model
            runtime.push_worker_request_waking(WorkerRequest::LoadFailed {
                worker: failure.worker,
                parent: failure.parent,
            });
            state.active.fetch_sub(1, Ordering::AcqRel);
            state.closed.store(true, Ordering::Release);
            return;
        }
    };
    // https://html.spec.whatwg.org/multipage/workers.html#dom-worker
    worker.drain_until_idle(&state.terminate);
    state.active.fetch_sub(1, Ordering::AcqRel);
    if worker.is_closed() || state.terminate.load(Ordering::Acquire) {
        state.closed.store(true, Ordering::Release);
        return;
    }
    loop {
        // https://fetch.spec.whatwg.org/#fetch-processresponse
        let received = if state.pending_fetches.load(Ordering::Acquire) > 0 {
            match commands.recv_timeout(Duration::from_millis(2)) {
                Ok(command) => Ok(command),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    state.active.fetch_add(1, Ordering::AcqRel);
                    worker.drain_until_idle(&state.terminate);
                    state.active.fetch_sub(1, Ordering::AcqRel);
                    if worker.is_closed() || state.terminate.load(Ordering::Acquire) {
                        state.closed.store(true, Ordering::Release);
                        return;
                    }
                    continue;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        } else {
            commands.recv().map_err(|_| ())
        };
        let Ok(command) = received else {
            break;
        };
        let terminate = matches!(command, WorkerCommand::Terminate);
        if !terminate {
            worker.apply_command(command);
            worker.drain_until_idle(&state.terminate);
        }
        state.active.fetch_sub(1, Ordering::AcqRel);
        if terminate || worker.is_closed() || state.terminate.load(Ordering::Acquire) {
            state.closed.store(true, Ordering::Release);
            return;
        }
    }
    state.active.fetch_sub(1, Ordering::AcqRel);
    state.closed.store(true, Ordering::Release);
}

enum WorkerTask {
    Message {
        payload: String,
        ports: Vec<u64>,
    },
    // https://html.spec.whatwg.org/multipage/web-messaging.html#message-port-post-message-steps
    PortMessage {
        endpoint: u64,
        payload: String,
        ports: Vec<u64>,
    },
    PortClosed {
        endpoint: u64,
    },
    // https://html.spec.whatwg.org/multipage/workers.html#shared-workers-and-the-sharedworkerglobalscope-interface
    Connect {
        endpoint: u64,
    },
    Broadcast {
        origin: String,
        name: String,
        payload: String,
        source: Option<u64>,
    },
    Timer(TimerId),
}

// https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model
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

// https://html.spec.whatwg.org/multipage/workers.html#dom-worker
enum WorkerScript {
    Classic(String),
    Module,
}

pub(crate) struct Worker {
    id: WorkerId,
    parent: FrameId,
    // https://html.spec.whatwg.org/multipage/workers.html#shared-workers-and-the-sharedworkerglobalscope-interface
    shared: bool,
    // https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model
    url: Url,
    world: Rc<RefCell<World>>,
    js: JsRealm,
    script: Option<WorkerScript>,
    tasks: VecDeque<WorkerTask>,
    timers: HashMap<TimerId, WorkerTimer>,
    js_timers: HashMap<PageTimerId, TimerId>,
    next_timer: TimerId,
    // https://fetch.spec.whatwg.org/#fetch-processresponse
    fetch_tx: Sender<(JsFetchId, Option<DialOutcome>)>,
    fetch_rx: Receiver<(JsFetchId, Option<DialOutcome>)>,
    pending_fetches: Arc<AtomicUsize>,
    fetches: HashMap<JsFetchId, DialCancellation>,
    closed: bool,
}

impl Worker {
    // https://html.spec.whatwg.org/multipage/workers.html#dom-worker
    pub(crate) fn create(
        created: &CreatedWorker,
        id: WorkerId,
        runtime: &FrameRuntime,
        js_runtime: &SharedJsRuntime,
        terminate: Arc<AtomicBool>,
        pending_fetches: Arc<AtomicUsize>,
    ) -> Result<Self, WorkerLoadFailure> {
        let failure = WorkerLoadFailure {
            worker: id,
            parent: created.parent,
        };
        let (script, worker_url, own_embedder_policy, response_csp) = if created.module {
            // https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model
            let (url, embedder_policy, csp) =
                fetch_module_url(created, runtime).map_err(|()| failure)?;
            (WorkerScript::Module, url, embedder_policy, csp)
        } else {
            let (source, response_url, embedder_policy, csp) =
                fetch_worker_source(created, runtime).map_err(|()| failure)?;
            // https://infra.spec.whatwg.org/#utf-8-decode
            (
                WorkerScript::Classic(String::from_utf8_lossy(&source).into_owned()),
                response_url,
                embedder_policy,
                csp,
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
            // https://html.spec.whatwg.org/multipage/workers.html#dom-worker
            state.origin = created.worker_origin.clone();
            state.top_level = Some(if created.url.scheme() == "data" {
                created.worker_origin.clone()
            } else {
                created.top_level.clone()
            });
            state.policy = created.policy.clone();
            // https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model
            state.policy.csp = response_csp;
            state.state.worker_owner_csp = Some(created.policy.csp.clone());
            if let Some(compatible) = own_embedder_policy {
                state.policy.cross_origin_embedder_policy = compatible;
            }
            // https://html.spec.whatwg.org/multipage/webappapis.html#concept-settings-object-cross-origin-isolated-capability
            state.policy.cross_origin_isolated = state.policy.cross_origin_isolated
                && state.policy.cross_origin_embedder_policy.compatible();
            state.sandbox = created.sandbox;
            state.referrer_url = created.referrer_url.clone();
            state.document_referrer = Some(created.referrer_url.clone());
            state.same_site_cookie_policy = created.same_site_cookies.cookie_policy();
            // https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model
            if created.module {
                state.state.module_client_origin = Some(created.origin.clone());
                state.state.module_credentials = Some(created.credentials);
            }
        }
        let js = JsRealm::new_worker(
            &js_runtime.handle(),
            Rc::clone(&world),
            Arc::clone(&runtime.stop),
            terminate,
        )
        .map_err(|_| failure)?;
        let (fetch_tx, fetch_rx) = std::sync::mpsc::channel();
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

    // https://html.spec.whatwg.org/multipage/workers.html#dom-worker-postmessage
    pub(crate) fn deliver_message(
        &self,
        worker: WorkerId,
        payload: &str,
        ports: &[u64],
    ) -> Result<(), JsError> {
        self.js.deliver_worker_event(worker.get(), payload, ports)
    }

    // https://html.spec.whatwg.org/multipage/workers.html#runtime-script-errors-in-workers
    pub(crate) fn deliver_error(&self, error: &WorkerError) -> Result<(), JsError> {
        self.js.deliver_worker_error(
            error.worker.get(),
            &error.message,
            &error.filename,
            error.lineno,
            error.colno,
        )
    }

    // https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model
    pub(crate) fn deliver_load_failure(&self, worker: WorkerId) -> Result<(), JsError> {
        self.js.deliver_worker_load_error(worker.get())
    }

    pub(crate) fn deliver_csp_violations(&self, violations: Vec<crate::csp::Violation>) {
        self.world
            .borrow()
            .runtime
            .push_worker_request_waking(WorkerRequest::CspViolation {
                parent: self.parent,
                violations,
            });
    }

    pub(crate) fn queue_message(&mut self, payload: String, ports: Vec<u64>) {
        // https://html.spec.whatwg.org/multipage/workers.html#terminate-a-worker
        if !self.closed {
            self.tasks.push_back(WorkerTask::Message { payload, ports });
        }
    }

    // https://html.spec.whatwg.org/multipage/web-messaging.html#message-port-post-message-steps
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

    // https://html.spec.whatwg.org/multipage/workers.html#dom-sharedworkerglobalscope-onconnect
    pub(crate) fn queue_connect(&mut self, endpoint: u64) {
        if self.closed {
            self.forget_endpoints(&[endpoint]);
            return;
        }
        self.tasks.push_back(WorkerTask::Connect { endpoint });
    }

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

    pub(crate) fn has_origin(&self, origin: &url::Origin) -> bool {
        self.world.borrow().origin == *origin
    }

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

    fn drain_until_idle(&mut self, terminate: &Arc<AtomicBool>) {
        while !terminate.load(Ordering::Acquire) {
            if !self.drain_step() {
                break;
            }
        }
    }

    fn apply_command(&mut self, command: WorkerCommand) {
        match command {
            WorkerCommand::Post { payload, ports } => self.queue_message(payload, ports),
            WorkerCommand::PortMessage {
                endpoint,
                payload,
                ports,
            } => self.queue_port_message(endpoint, payload, ports),
            WorkerCommand::PortClosed { endpoint } => self.queue_port_closed(endpoint),
            WorkerCommand::Broadcast {
                origin,
                name,
                payload,
                source,
            } => self.queue_broadcast(origin, name, payload, source),
            WorkerCommand::FireTimer { js_id } => {
                if let Err(error) = self.js.fire_timer(js_id) {
                    self.report_error(&error);
                }
            }
            WorkerCommand::NestedMessage {
                worker,
                payload,
                ports,
            } => {
                let _ = self.deliver_message(WorkerId::new(worker), &payload, &ports);
            }
            WorkerCommand::NestedError(error) => {
                let _ = self.deliver_error(&error);
            }
            WorkerCommand::CspViolation { violations } => {
                self.world.borrow().runtime.push_worker_request_waking(
                    WorkerRequest::CspViolation {
                        parent: self.parent,
                        violations,
                    },
                );
            }
            WorkerCommand::NestedLoadFailure { worker } => {
                let _ = self.deliver_load_failure(WorkerId::new(worker));
            }
            WorkerCommand::Terminate => {}
        }
    }

    pub(crate) fn drain_step(&mut self) -> bool {
        if self.world.borrow().worker_close_requested.get() {
            self.close();
        }
        if self.closed {
            return false;
        }
        self.adopt_fetches();
        if let Some(script) = self.script.take() {
            {
                let _turn = self.js.enter_turn();
                match script {
                    // https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model
                    WorkerScript::Classic(source) => {
                        match self.js.eval_worker_script(&source, self.url.as_str()) {
                            Ok(()) | Err(WorkerScriptError::Runtime) => {}
                            Err(WorkerScriptError::Parse) => {
                                self.report_load_failure();
                                self.closed = true;
                                return false;
                            }
                            // https://html.spec.whatwg.org/multipage/workers.html#terminate-a-worker
                            Err(WorkerScriptError::Interrupted) => {
                                self.closed = true;
                                return false;
                            }
                        }
                    }
                    // https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model
                    WorkerScript::Module => match self.js.eval_worker_module(self.url.as_str()) {
                        Ok(()) | Err(WorkerScriptError::Runtime) => {}
                        Err(WorkerScriptError::Parse) => {
                            // https://w3c.github.io/webappsec-csp/#report-violation
                            self.adopt_js_work();
                            self.report_load_failure();
                            self.closed = true;
                            return false;
                        }
                        Err(WorkerScriptError::Interrupted) => {
                            self.adopt_js_work();
                            self.closed = true;
                            return false;
                        }
                    },
                }
            }
            // https://w3c.github.io/webappsec-csp/#report-violation
            self.adopt_js_work();
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
                    // https://html.spec.whatwg.org/multipage/web-messaging.html#transfer-receiving-steps
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

    // https://html.spec.whatwg.org/multipage/workers.html#close-a-worker
    pub(crate) fn close(&mut self) {
        self.closed = true;
        // https://fetch.spec.whatwg.org/#keepalive-flag
        self.start_queued_keepalive_fetches();
        self.discard_queued_tasks();
    }

    fn start_queued_keepalive_fetches(&mut self) {
        let pending = {
            let world = self.world.borrow();
            world.take_queued_keepalive_fetches()
        };
        for request in pending {
            self.apply_realm_request(request);
        }
    }

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

    // https://html.spec.whatwg.org/multipage/web-messaging.html#message-port-post-message-steps
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
            // https://html.spec.whatwg.org/multipage/web-messaging.html#message-port-post-message-steps
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
        // https://html.spec.whatwg.org/multipage/workers.html#runtime-script-errors-2
        if self.shared {
            return;
        }
        let error = WorkerError::from_js(self.id, self.parent, &self.url, error);
        self.world
            .borrow()
            .runtime
            .push_worker_request_waking(WorkerRequest::Error(Box::new(error)));
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
            .push_worker_request_waking(request);
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
                if self.shared {
                    let id = self.schedule_timer(timeout.delay, timeout.js_id);
                    self.js_timers.insert(timeout.js_id, id);
                } else {
                    self.world
                        .borrow()
                        .runtime
                        .push_worker_request(WorkerRequest::SetTimer {
                            worker: self.id,
                            js_id: timeout.js_id,
                            delay: timeout.delay,
                        });
                }
            }
            RealmRequest::ClearTimer(timeout) => {
                let js_id = timeout.js_id;
                if self.shared {
                    if let Some(id) = self.js_timers.remove(&js_id) {
                        self.timers.remove(&id);
                    }
                } else {
                    self.world
                        .borrow()
                        .runtime
                        .push_worker_request(WorkerRequest::ClearTimer {
                            worker: self.id,
                            js_id,
                        });
                }
            }
            RealmRequest::Fetch(fetch) => self.start_fetch(*fetch),
            RealmRequest::AbortFetch(request) => {
                if let Some(cancel) = self.fetches.remove(&request.js_id) {
                    cancel();
                }
            }
            RealmRequest::CspViolation(_, violations) => {
                // https://w3c.github.io/webappsec-csp/#report-violation
                self.world.borrow().runtime.push_worker_request_waking(
                    WorkerRequest::CspViolation {
                        parent: self.parent,
                        violations,
                    },
                );
            }
            RealmRequest::RejectionTasks => self.js.fire_rejection_tasks(),
            RealmRequest::Script(_)
            | RealmRequest::HistoryPush { .. }
            | RealmRequest::HistoryTraverse { .. }
            | RealmRequest::FragmentNavigation { .. }
            | RealmRequest::CookieChanges
            // https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#textFieldSelection
            | RealmRequest::SelectionChanged(_)
            | RealmRequest::ToggleEvent { .. }
            | RealmRequest::DialogClose(_)
            | RealmRequest::WorkerBlocked(_, _)
            | RealmRequest::DeclarativeRefresh(_)
            | RealmRequest::HtmlWrite(_)
            | RealmRequest::DocumentStream(_)
            | RealmRequest::Binding { .. }
            | RealmRequest::Console { .. }
            | RealmRequest::ExceptionThrown { .. } => {}
        }
    }

    // https://fetch.spec.whatwg.org/#fetch-processresponse
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
        let (origin, referrer_url, referrer_policy, cookie_policy, partition_key, gate) = {
            let world = self.world.borrow();
            (
                world.origin.clone(),
                world.referrer_url.clone(),
                world.policy.referrer_policy,
                world.same_site_cookie_policy,
                world.partition_key(),
                crate::RedirectGate::new(
                    world.policy.csp.clone(),
                    &world.origin,
                    crate::csp::Destination::Connect,
                ),
            )
        };
        let mut dial = DialRequest::new(RequestPolicy::Fetch(fetch.options.cors), &url, &origin);
        dial.cookie_policy = cookie_policy.into();
        dial.partition_key = partition_key;
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
                // https://fetch.spec.whatwg.org/#concept-network-error
                let _ = tx.send((js_id, outcome.ok()));
            }),
        );
        self.fetches.insert(js_id, cancel);
    }

    fn adopt_fetches(&mut self) {
        let _turn = self.js.enter_turn();
        while let Ok((js_id, outcome)) = self.fetch_rx.try_recv() {
            self.pending_fetches.fetch_sub(1, Ordering::Release);
            self.fetches.remove(&js_id);
            // https://cookiestore.spec.whatwg.org/#process-cookie-changes
            let cookies_stored = outcome
                .as_ref()
                .is_some_and(|outcome| outcome.cookies_stored);
            if let Err(error) = self.js.finish_js_fetch(js_id, outcome) {
                self.report_error(&error);
            }
            if cookies_stored {
                self.world.borrow().notify_cookie_changes();
            }
            self.adopt_js_work();
        }
    }

    fn fail_fetch(&mut self, js_id: JsFetchId) {
        if let Err(error) = self.js.finish_js_fetch(js_id, None) {
            self.report_error(&error);
        }
    }

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

// https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model
fn fetch_module_url(
    created: &CreatedWorker,
    runtime: &FrameRuntime,
) -> Result<
    (
        Url,
        Option<crate::protocol::CrossOriginEmbedderPolicy>,
        crate::csp::List,
    ),
    (),
> {
    if created.url.scheme() == "data" || created.url.scheme() == "blob" {
        // https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model
        return Ok((created.url.clone(), None, created.policy.csp.clone()));
    }
    if created.url.scheme() != "http" && created.url.scheme() != "https" {
        return Err(());
    }
    let mut dial = DialRequest::new(
        RequestPolicy::ModuleWorkerScript {
            credentials: created.credentials,
        },
        &created.url,
        &created.origin,
    );
    dial.referrer = ReferrerRequest::Client {
        url: created.referrer_url.clone(),
    };
    dial.referrer_policy = created.policy.referrer_policy.unwrap_or_default();
    dial.cookie_policy = created.same_site_cookies.cookie_policy().into();
    dial.partition_key.clone_from(&created.partition_key);
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
    Ok((
        outcome.final_url,
        Some(crate::protocol::cross_origin_embedder_policy(
            &outcome.headers,
        )),
        response_csp(&outcome.headers),
    ))
}

// https://html.spec.whatwg.org/multipage/browsers.html#initialize-a-policy-container-from-a-fetch-response
fn response_csp(headers: &[(String, String)]) -> crate::csp::List {
    crate::csp::List::from_headers(
        net::combined_header_in(headers, "content-security-policy").as_deref(),
        net::combined_header_in(headers, "content-security-policy-report-only").as_deref(),
    )
}

// https://html.spec.whatwg.org/multipage/workers.html#fetch-a-classic-worker-script
fn fetch_worker_source(
    created: &CreatedWorker,
    runtime: &FrameRuntime,
) -> Result<
    (
        Vec<u8>,
        Url,
        Option<crate::protocol::CrossOriginEmbedderPolicy>,
        crate::csp::List,
    ),
    (),
> {
    if created.url.scheme() == "data" {
        let (_, body) = crate::engine::decode_data_url_bounded(created.url.as_str()).ok_or(())?;
        return Ok((body, created.url.clone(), None, created.policy.csp.clone()));
    }
    if created.url.scheme() == "blob" {
        // https://html.spec.whatwg.org/multipage/webappapis.html#fetch-a-classic-worker-script
        return match &created.local_script {
            Some(source) => Ok((
                source.clone().into_bytes(),
                created.url.clone(),
                None,
                created.policy.csp.clone(),
            )),
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
    dial.cookie_policy = created.same_site_cookies.cookie_policy().into();
    dial.partition_key.clone_from(&created.partition_key);
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
    // https://html.spec.whatwg.org/multipage/workers.html#fetch-a-classic-worker-script
    if outcome.final_url.origin() != created.origin {
        return Err(());
    }
    if let Some(content_type) = &outcome.content_type
        && !crate::js::javascript_mime_essence(content_type_essence(content_type))
    {
        return Err(());
    }
    match outcome.body {
        crate::protocol::ResponseBody::Bytes(body) => Ok((
            body,
            outcome.final_url,
            Some(crate::protocol::cross_origin_embedder_policy(
                &outcome.headers,
            )),
            response_csp(&outcome.headers),
        )),
        crate::protocol::ResponseBody::Streamed(_) | crate::protocol::ResponseBody::Undecodable => {
            Err(())
        }
    }
}

fn content_type_essence(value: &str) -> &str {
    value.split(';').next().unwrap_or_default().trim()
}
