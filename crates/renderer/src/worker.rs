// https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model
// https://html.spec.whatwg.org/multipage/workers.html#dedicated-workers-and-the-dedicatedworkerglobalscope-interface
// https://html.spec.whatwg.org/multipage/workers.html#dom-worker

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

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
use crate::timers::TimerQueue;

const MAX_PENDING_FETCHES: usize = 256;
const NO_DEADLINE: u64 = u64::MAX;

fn encode_deadline(deadline: Option<Instant>, epoch: Instant) -> u64 {
    deadline.map_or(NO_DEADLINE, |when| {
        let nanos = when.saturating_duration_since(epoch).as_nanos();
        u64::try_from(nanos)
            .unwrap_or(NO_DEADLINE - 1)
            .min(NO_DEADLINE - 1)
    })
}

fn decode_deadline(raw: u64, epoch: Instant) -> Option<Instant> {
    (raw != NO_DEADLINE)
        .then(|| epoch.checked_add(Duration::from_nanos(raw)))
        .flatten()
}

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
        let doc = world.doc();
        let worker_origin = if url.scheme() == "data" {
            url::Origin::new_opaque()
        } else {
            doc.origin.clone()
        };
        let local_script = (url.scheme() == "blob")
            .then(|| world.object_url_contents(url.as_str()))
            .flatten()
            .map(|contents| contents.to_string());
        let top_level = doc.top_level.clone().unwrap_or_else(|| doc.origin.clone());
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
            origin: doc.origin.clone(),
            worker_origin,
            top_level,
            partition_key,
            policy: doc.policy.clone(),
            sandbox: doc.sandbox,
            referrer_url: doc.document_url.clone(),
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
        message: crate::messaging::SerializedMessage,
        channel: u64,
    },
    PostToParent {
        parent: FrameId,
        worker: WorkerId,
        message: crate::messaging::SerializedMessage,
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
        message: crate::messaging::SerializedMessage,
    },
    PortMessage {
        endpoint: u64,
        message: crate::messaging::SerializedMessage,
    },
    PortClosed {
        endpoint: u64,
    },
    // https://html.spec.whatwg.org/multipage/workers.html#dom-sharedworkerglobalscope-onconnect
    Connect {
        endpoint: u64,
    },
    Broadcast {
        origin: String,
        name: String,
        message: crate::messaging::SerializedMessage,
        source: Option<u64>,
    },
    // https://fetch.spec.whatwg.org/#fetch-processresponse
    FetchDone {
        js_id: JsFetchId,
        outcome: Option<Box<DialOutcome>>,
    },
    NestedMessage {
        worker: u64,
        message: crate::messaging::SerializedMessage,
    },
    NestedError(Box<WorkerError>),
    NestedLoadFailure {
        worker: u64,
    },
    CspViolation {
        violations: Vec<crate::csp::Violation>,
    },
    // https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model
    ScriptLoaded {
        outcome: Option<Box<DialOutcome>>,
    },
    Wake,
    Terminate,
}

struct BusyGuard {
    active: Arc<AtomicUsize>,
    wake: Arc<tokio::sync::Notify>,
}

impl BusyGuard {
    fn acquire(active: &Arc<AtomicUsize>, wake: &Arc<tokio::sync::Notify>) -> Self {
        active.fetch_add(1, Ordering::AcqRel);
        Self {
            active: Arc::clone(active),
            wake: Arc::clone(wake),
        }
    }
}

impl Drop for BusyGuard {
    fn drop(&mut self) {
        if self.active.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.wake.notify_one();
        }
    }
}

struct PendingFetchGuard(Arc<AtomicUsize>);

impl PendingFetchGuard {
    fn acquire(counter: &Arc<AtomicUsize>) -> Self {
        counter.fetch_add(1, Ordering::AcqRel);
        Self(Arc::clone(counter))
    }
}

impl Drop for PendingFetchGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

struct QueuedCommand {
    command: WorkerCommand,
    guard: BusyGuard,
}

struct WorkerThreadState {
    // https://html.spec.whatwg.org/multipage/workers.html#terminate-a-worker
    terminate: Arc<AtomicBool>,
    active: Arc<AtomicUsize>,
    closed: Arc<AtomicBool>,
    pending_fetches: Arc<AtomicUsize>,
    next_deadline_nanos: Arc<AtomicU64>,
    wake: Arc<tokio::sync::Notify>,
}

impl WorkerThreadState {
    fn clone_shared(&self) -> Self {
        Self {
            terminate: Arc::clone(&self.terminate),
            active: Arc::clone(&self.active),
            closed: Arc::clone(&self.closed),
            pending_fetches: Arc::clone(&self.pending_fetches),
            next_deadline_nanos: Arc::clone(&self.next_deadline_nanos),
            wake: Arc::clone(&self.wake),
        }
    }
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
            engine_id: 0,
            identity: Rc::new(RefCell::new(self.runtime.initial_identity.clone())),
            js_runtime,
            documents: Rc::new(RefCell::new(crate::documents::DocumentStore::default())),
            registry: Rc::new(RefCell::new(crate::js::RealmRegistry::default())),
            pending_windows: std::rc::Weak::new(),
            pending_storage: Rc::new(RefCell::new(Vec::new())),
            turn_deadline: Rc::new(std::cell::Cell::new(None)),
            event_seq: Rc::new(std::cell::Cell::new(0)),
            worker_dir: Arc::clone(self),
            font_ctx: self.runtime.font_ctx.clone(),
        }
    }

    fn report_spawn_failure(&self, id: WorkerId, parent: FrameId, shared: bool) {
        let request = if shared {
            WorkerRequest::SharedLoadFailed { worker: id }
        } else {
            WorkerRequest::LoadFailed { worker: id, parent }
        };
        self.runtime.push_worker_request_waking(request);
    }

    // https://html.spec.whatwg.org/multipage/workers.html#dom-worker
    // https://html.spec.whatwg.org/multipage/workers.html#dom-sharedworker
    pub(crate) fn spawn_worker(self: &Arc<Self>, created: CreatedWorker) -> WorkerId {
        // https://html.spec.whatwg.org/multipage/workers.html#dom-worker — UA cap matching Firefox dom.workers.maxPerDomain
        const MAX_WORKERS: usize = 512;
        let id = created.id;
        let parent = created.parent;
        let shared = created.shared;
        let (commands, received) = std::sync::mpsc::channel();
        let commands_tx = commands.clone();
        let state = WorkerThreadState {
            terminate: Arc::new(AtomicBool::new(false)),
            active: Arc::new(AtomicUsize::new(0)),
            closed: Arc::new(AtomicBool::new(false)),
            pending_fetches: Arc::new(AtomicUsize::new(0)),
            next_deadline_nanos: Arc::new(AtomicU64::new(NO_DEADLINE)),
            wake: Arc::clone(&self.runtime.wake),
        };
        let handle = WorkerHandle {
            origin: created.worker_origin.clone(),
            commands,
            terminate: Arc::clone(&state.terminate),
            active: Arc::clone(&state.active),
            closed: Arc::clone(&state.closed),
            pending_fetches: Arc::clone(&state.pending_fetches),
            next_deadline_nanos: Arc::clone(&state.next_deadline_nanos),
            wake: Arc::clone(&state.wake),
            closed_local: false,
        };
        {
            let mut workers = self.workers();
            workers.retain(|_, worker| !worker.is_closed());
            if workers.len() >= MAX_WORKERS {
                state.closed.store(true, Ordering::Release);
                self.report_spawn_failure(id, parent, shared);
                return id;
            }
            workers.insert(id, handle);
        }

        let directory = Arc::clone(self);
        let thread_state = state.clone_shared();
        let startup_guard = BusyGuard::acquire(&state.active, &state.wake);
        let spawned = std::thread::Builder::new()
            .name(format!("tinybrowser-worker-{}", id.get()))
            .spawn(move || {
                run_worker(
                    &created,
                    &directory,
                    commands_tx,
                    &received,
                    &thread_state,
                    startup_guard,
                );
            });
        if spawned.is_err() {
            // https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model
            state.closed.store(true, Ordering::Release);
            self.report_spawn_failure(id, parent, shared);
        }
        id
    }

    pub(crate) fn is_live(&self, id: WorkerId) -> bool {
        self.workers()
            .get(&id)
            .is_some_and(|worker| !worker.is_closed())
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
    commands: Sender<QueuedCommand>,
    terminate: Arc<AtomicBool>,
    active: Arc<AtomicUsize>,
    closed: Arc<AtomicBool>,
    pending_fetches: Arc<AtomicUsize>,
    next_deadline_nanos: Arc<AtomicU64>,
    wake: Arc<tokio::sync::Notify>,
    closed_local: bool,
}

impl WorkerHandle {
    pub(crate) fn has_origin(&self, origin: &url::Origin) -> bool {
        self.origin == *origin
    }

    fn send(&mut self, command: WorkerCommand) -> bool {
        if self.is_closed() {
            return false;
        }
        let queued = QueuedCommand {
            command,
            guard: BusyGuard::acquire(&self.active, &self.wake),
        };
        self.commands.send(queued).is_ok()
    }

    pub(crate) fn queue_broadcast(
        &mut self,
        origin: String,
        name: String,
        message: crate::messaging::SerializedMessage,
        source: Option<u64>,
    ) {
        self.send(WorkerCommand::Broadcast {
            origin,
            name,
            message,
            source,
        });
    }

    pub(crate) fn wake_due_timer(&mut self, now: Instant, epoch: Instant) -> bool {
        if self.is_closed() {
            return false;
        }
        if self.next_deadline(epoch).is_some_and(|when| when <= now)
            && self.next_deadline_nanos.swap(NO_DEADLINE, Ordering::AcqRel) != NO_DEADLINE
        {
            return self.send(WorkerCommand::Wake);
        }
        false
    }

    pub(crate) fn has_due_timer(&self, now: Instant, epoch: Instant) -> bool {
        !self.is_closed() && self.next_deadline(epoch).is_some_and(|when| when <= now)
    }

    pub(crate) fn is_active(&self) -> bool {
        !self.is_closed() && self.active.load(Ordering::Acquire) > 0
    }

    pub(crate) fn next_deadline(&self, epoch: Instant) -> Option<Instant> {
        if self.is_closed() {
            return None;
        }
        decode_deadline(self.next_deadline_nanos.load(Ordering::Acquire), epoch)
    }

    pub(crate) fn has_pending_dials(&self) -> bool {
        !self.is_closed() && self.pending_fetches.load(Ordering::Acquire) > 0
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.closed_local || self.closed.load(Ordering::Acquire)
    }

    pub(crate) fn terminate(&mut self) {
        if self.closed_local {
            return;
        }
        self.closed_local = true;
        self.terminate.store(true, Ordering::Release);
        self.next_deadline_nanos
            .store(NO_DEADLINE, Ordering::Release);
        let queued = QueuedCommand {
            command: WorkerCommand::Terminate,
            guard: BusyGuard::acquire(&self.active, &self.wake),
        };
        let _ = self.commands.send(queued);
    }
}

impl Drop for WorkerHandle {
    fn drop(&mut self) {
        self.terminate();
    }
}

fn finish_worker_thread(directory: &WorkerDirectory, state: &WorkerThreadState) {
    state
        .next_deadline_nanos
        .store(NO_DEADLINE, Ordering::Release);
    state.closed.store(true, Ordering::Release);
    directory.runtime.wake.notify_one();
}

fn with_message_buffers<T>(
    world: &Rc<RefCell<World>>,
    message: &crate::messaging::SerializedMessage,
    operation: impl FnOnce() -> T,
) -> T {
    let previous = world
        .borrow()
        .install_message_shared_buffers(message.shared_buffers.clone());
    let result = operation();
    world.borrow().restore_message_shared_buffers(previous);
    result
}

struct LoadedScript {
    script: WorkerScript,
    url: Url,
    embedder_policy: Option<crate::protocol::CrossOriginEmbedderPolicy>,
    csp: crate::csp::List,
}

enum InitialScript {
    Ready(Result<LoadedScript, ()>),
    Dial(Box<DialRequest>),
}

// https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model
fn await_script_dial(
    created: &CreatedWorker,
    runtime: &FrameRuntime,
    dial: DialRequest,
    commands_tx: &Sender<QueuedCommand>,
    commands: &Receiver<QueuedCommand>,
    state: &WorkerThreadState,
    startup_guard: BusyGuard,
) -> Option<(Result<LoadedScript, ()>, Vec<WorkerCommand>, BusyGuard)> {
    if state.terminate.load(Ordering::Acquire) {
        return None;
    }
    let _pending = PendingFetchGuard::acquire(&state.pending_fetches);
    let tx = commands_tx.clone();
    let active = Arc::clone(&state.active);
    let wake = Arc::clone(&state.wake);
    let cancel = runtime.services.start_dial(
        dial,
        Box::new(move |outcome| {
            let queued = QueuedCommand {
                command: WorkerCommand::ScriptLoaded {
                    outcome: outcome.ok().map(Box::new),
                },
                guard: BusyGuard::acquire(&active, &wake),
            };
            let _ = tx.send(queued);
        }),
    );
    drop(startup_guard);
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut deferred = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            cancel();
            return Some((
                Err(()),
                deferred,
                BusyGuard::acquire(&state.active, &state.wake),
            ));
        }
        match commands.recv_timeout(remaining) {
            Ok(QueuedCommand {
                command: WorkerCommand::Terminate,
                ..
            })
            | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                cancel();
                return None;
            }
            Ok(QueuedCommand {
                command: WorkerCommand::ScriptLoaded { outcome },
                guard,
            }) => {
                let loaded = finish_dialed_script(created, outcome.map(|outcome| *outcome));
                return Some((loaded, deferred, guard));
            }
            Ok(QueuedCommand { command, .. }) => deferred.push(command),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                cancel();
                return Some((
                    Err(()),
                    deferred,
                    BusyGuard::acquire(&state.active, &state.wake),
                ));
            }
        }
    }
}

fn report_worker_load_failure(created: &CreatedWorker, runtime: &FrameRuntime) {
    // https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model
    let request = if created.shared {
        WorkerRequest::SharedLoadFailed { worker: created.id }
    } else {
        WorkerRequest::LoadFailed {
            worker: created.id,
            parent: created.parent,
        }
    };
    runtime.push_worker_request_waking(request);
}

fn run_worker(
    created: &CreatedWorker,
    directory: &Arc<WorkerDirectory>,
    commands_tx: Sender<QueuedCommand>,
    commands: &Receiver<QueuedCommand>,
    state: &WorkerThreadState,
    startup_guard: BusyGuard,
) {
    let js_runtime = SharedJsRuntime::default();
    let runtime = directory.thread_runtime(js_runtime.handle());
    let (loaded, deferred, startup_guard) = match prepare_worker_script(created) {
        InitialScript::Ready(loaded) => (loaded, Vec::new(), startup_guard),
        InitialScript::Dial(dial) => {
            let Some(result) = await_script_dial(
                created,
                &runtime,
                *dial,
                &commands_tx,
                commands,
                state,
                startup_guard,
            ) else {
                finish_worker_thread(directory, state);
                return;
            };
            result
        }
    };
    if state.terminate.load(Ordering::Acquire) {
        finish_worker_thread(directory, state);
        return;
    }
    let Ok(loaded) = loaded else {
        report_worker_load_failure(created, &runtime);
        finish_worker_thread(directory, state);
        return;
    };
    let Ok(mut worker) =
        Worker::from_loaded(created, loaded, &runtime, &js_runtime, commands_tx, state)
    else {
        report_worker_load_failure(created, &runtime);
        finish_worker_thread(directory, state);
        return;
    };
    let epoch = runtime.common.animation_epoch;
    // https://html.spec.whatwg.org/multipage/workers.html#dom-worker
    worker.drain_until_idle(&state.terminate);
    for command in deferred {
        if worker.is_closed() || state.terminate.load(Ordering::Acquire) {
            break;
        }
        worker.apply_command(command);
        worker.drain_until_idle(&state.terminate);
    }
    if worker.is_closed() || state.terminate.load(Ordering::Acquire) {
        finish_worker_thread(directory, state);
        return;
    }
    let next_deadline = worker.timers.next_deadline();
    state
        .next_deadline_nanos
        .store(encode_deadline(next_deadline, epoch), Ordering::Release);
    drop(startup_guard);
    run_worker_loop(&mut worker, &runtime, commands, state, next_deadline, epoch);
    finish_worker_thread(directory, state);
}

fn run_worker_loop(
    worker: &mut Worker,
    runtime: &FrameRuntime,
    commands: &Receiver<QueuedCommand>,
    state: &WorkerThreadState,
    mut next_deadline: Option<Instant>,
    epoch: Instant,
) {
    loop {
        let received = if !runtime.common.clock.is_manual()
            && let Some(deadline) = next_deadline
        {
            let wait = deadline.saturating_duration_since(runtime.common.now());
            match commands.recv_timeout(wait) {
                Ok(queued) => Ok(queued),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    let _guard = BusyGuard::acquire(&state.active, &state.wake);
                    worker.drain_until_idle(&state.terminate);
                    if worker.is_closed() || state.terminate.load(Ordering::Acquire) {
                        return;
                    }
                    next_deadline = worker.timers.next_deadline();
                    state
                        .next_deadline_nanos
                        .store(encode_deadline(next_deadline, epoch), Ordering::Release);
                    continue;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
            }
        } else {
            commands.recv().map_err(|_| ())
        };
        let Ok(QueuedCommand {
            command,
            guard: _guard,
        }) = received
        else {
            return;
        };
        if matches!(command, WorkerCommand::Terminate) {
            return;
        }
        worker.apply_command(command);
        worker.drain_until_idle(&state.terminate);
        if worker.is_closed() || state.terminate.load(Ordering::Acquire) {
            return;
        }
        next_deadline = worker.timers.next_deadline();
        state
            .next_deadline_nanos
            .store(encode_deadline(next_deadline, epoch), Ordering::Release);
    }
}

enum WorkerTask {
    Message {
        message: crate::messaging::SerializedMessage,
    },
    // https://html.spec.whatwg.org/multipage/web-messaging.html#message-port-post-message-steps
    PortMessage {
        endpoint: u64,
        message: crate::messaging::SerializedMessage,
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
        message: crate::messaging::SerializedMessage,
        source: Option<u64>,
    },
    Timer(TimerId),
    // https://html.spec.whatwg.org/multipage/webappapis.html#notify-about-rejected-promises
    RejectionTasks,
}

// https://html.spec.whatwg.org/multipage/workers.html#dom-worker
enum WorkerScript {
    Classic(String),
    Module,
}

struct Worker {
    id: WorkerId,
    parent: FrameId,
    // https://html.spec.whatwg.org/multipage/workers.html#shared-workers-and-the-sharedworkerglobalscope-interface
    shared: bool,
    // https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model
    url: Url,
    world: Rc<RefCell<World>>,
    js: JsRealm,
    script: Option<WorkerScript>,
    tasks: crate::task_queue::TaskQueue<WorkerTask>,
    timers: TimerQueue<PageTimerId>,
    js_timers: HashMap<PageTimerId, TimerId>,
    commands: Sender<QueuedCommand>,
    active: Arc<AtomicUsize>,
    wake: Arc<tokio::sync::Notify>,
    pending_fetches: Arc<AtomicUsize>,
    fetches: HashMap<JsFetchId, (DialCancellation, PendingFetchGuard)>,
    closed: bool,
}

fn worker_task_sink(
    tasks: crate::task_queue::TaskQueue<WorkerTask>,
) -> crate::task_queue::TaskSink {
    crate::task_queue::TaskSink::new(move |source, task| match task {
        crate::task_queue::RealmTask::RejectionTasks(_) => {
            tasks.push(source, WorkerTask::RejectionTasks);
        }
        crate::task_queue::RealmTask::SelectionChanged(_)
        | crate::task_queue::RealmTask::ToggleEvent { .. }
        | crate::task_queue::RealmTask::DialogClose(_) => {}
    })
}

impl Worker {
    // https://html.spec.whatwg.org/multipage/workers.html#dom-worker
    fn from_loaded(
        created: &CreatedWorker,
        loaded: LoadedScript,
        runtime: &FrameRuntime,
        js_runtime: &SharedJsRuntime,
        commands: Sender<QueuedCommand>,
        state: &WorkerThreadState,
    ) -> Result<Self, ()> {
        let id = created.id;
        let LoadedScript {
            script,
            url: worker_url,
            embedder_policy: own_embedder_policy,
            csp: response_csp,
        } = loaded;
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
            let world = world.borrow();
            let mut doc = world.doc_mut();
            // https://html.spec.whatwg.org/multipage/workers.html#dom-worker
            doc.origin = created.worker_origin.clone();
            doc.top_level = Some(if created.url.scheme() == "data" {
                created.worker_origin.clone()
            } else {
                created.top_level.clone()
            });
            doc.policy = created.policy.clone();
            // https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model
            doc.policy.csp = response_csp;
            doc.state.worker_owner_csp = Some(created.policy.csp.clone());
            if let Some(compatible) = own_embedder_policy {
                doc.policy.cross_origin_embedder_policy = compatible;
            }
            // https://html.spec.whatwg.org/multipage/webappapis.html#concept-settings-object-cross-origin-isolated-capability
            doc.policy.cross_origin_isolated = doc.policy.cross_origin_isolated
                && doc.policy.cross_origin_embedder_policy.compatible();
            doc.sandbox = created.sandbox;
            doc.referrer_url = created.referrer_url.clone();
            doc.document_referrer = Some(created.referrer_url.clone());
            doc.same_site_cookie_policy = created.same_site_cookies.cookie_policy();
            // https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model
            if created.module {
                doc.state.module_client_origin = Some(created.origin.clone());
                doc.state.module_credentials = Some(created.credentials);
            }
        }
        let tasks = crate::task_queue::TaskQueue::default();
        world
            .borrow_mut()
            .set_task_sink(worker_task_sink(tasks.clone()));
        let js = JsRealm::new_worker(
            &js_runtime.handle(),
            Rc::clone(&world),
            Arc::clone(&runtime.stop),
            Arc::clone(&state.terminate),
        )
        .map_err(|_| ())?;
        Ok(Self {
            id,
            parent: created.parent,
            shared: created.shared,
            url: worker_url,
            world,
            js,
            script: Some(script),
            tasks,
            timers: TimerQueue::default(),
            js_timers: HashMap::new(),
            commands,
            active: Arc::clone(&state.active),
            wake: Arc::clone(&state.wake),
            pending_fetches: Arc::clone(&state.pending_fetches),
            fetches: HashMap::new(),
            closed: false,
        })
    }

    fn now(&self) -> Instant {
        self.world.borrow().runtime.now()
    }

    // https://html.spec.whatwg.org/multipage/workers.html#dom-worker-postmessage
    fn deliver_message(
        &self,
        worker: WorkerId,
        message: &crate::messaging::SerializedMessage,
    ) -> Result<(), JsError> {
        with_message_buffers(&self.world, message, || {
            self.js
                .deliver_worker_event(worker.get(), &message.data, &message.ports)
        })
    }

    // https://html.spec.whatwg.org/multipage/workers.html#runtime-script-errors-in-workers
    fn deliver_error(&self, error: &WorkerError) -> Result<(), JsError> {
        self.js.deliver_worker_error(
            error.worker.get(),
            &error.message,
            &error.filename,
            error.lineno,
            error.colno,
        )
    }

    // https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model
    fn deliver_load_failure(&self, worker: WorkerId) -> Result<(), JsError> {
        self.js.deliver_worker_load_error(worker.get())
    }

    fn queue_message(&mut self, message: crate::messaging::SerializedMessage) {
        // https://html.spec.whatwg.org/multipage/workers.html#terminate-a-worker
        if !self.closed {
            self.tasks.push(
                crate::task_queue::TaskSource::PostedMessage,
                WorkerTask::Message { message },
            );
        }
    }

    // https://html.spec.whatwg.org/multipage/web-messaging.html#message-port-post-message-steps
    fn queue_port_message(&mut self, endpoint: u64, message: crate::messaging::SerializedMessage) {
        if self.closed {
            self.forget_endpoints(&message.ports);
            return;
        }
        self.tasks.push(
            crate::task_queue::TaskSource::PostedMessage,
            WorkerTask::PortMessage { endpoint, message },
        );
    }

    fn queue_port_closed(&mut self, endpoint: u64) {
        if !self.closed {
            self.tasks.push(
                crate::task_queue::TaskSource::PostedMessage,
                WorkerTask::PortClosed { endpoint },
            );
        }
    }

    // https://html.spec.whatwg.org/multipage/workers.html#dom-sharedworkerglobalscope-onconnect
    fn queue_connect(&mut self, endpoint: u64) {
        if self.closed {
            self.forget_endpoints(&[endpoint]);
            return;
        }
        self.tasks.push(
            crate::task_queue::TaskSource::PostedMessage,
            WorkerTask::Connect { endpoint },
        );
    }

    fn queue_broadcast(
        &mut self,
        origin: String,
        name: String,
        message: crate::messaging::SerializedMessage,
        source: Option<u64>,
    ) {
        if !self.closed {
            self.tasks.push(
                crate::task_queue::TaskSource::PostedMessage,
                WorkerTask::Broadcast {
                    origin,
                    name,
                    message,
                    source,
                },
            );
        }
    }

    fn forget_endpoints(&self, endpoints: &[u64]) {
        if endpoints.is_empty() {
            return;
        }
        self.world
            .borrow()
            .shared()
            .lock()
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
            WorkerCommand::Post { message } => self.queue_message(message),
            WorkerCommand::PortMessage { endpoint, message } => {
                self.queue_port_message(endpoint, message);
            }
            WorkerCommand::PortClosed { endpoint } => self.queue_port_closed(endpoint),
            WorkerCommand::Connect { endpoint } => self.queue_connect(endpoint),
            WorkerCommand::Broadcast {
                origin,
                name,
                message,
                source,
            } => self.queue_broadcast(origin, name, message, source),
            WorkerCommand::FetchDone { js_id, outcome } => {
                self.finish_fetch(js_id, outcome.map(|outcome| *outcome));
            }
            WorkerCommand::NestedMessage { worker, message } => {
                let _ = self.deliver_message(WorkerId::new(worker), &message);
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
            WorkerCommand::ScriptLoaded { .. } | WorkerCommand::Wake | WorkerCommand::Terminate => {
            }
        }
    }

    fn run_script(&mut self, script: WorkerScript) -> bool {
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
        !self.closed
    }

    fn drain_step(&mut self) -> bool {
        if self.world.borrow().worker_close_requested.get() {
            self.close();
        }
        if self.closed {
            return false;
        }
        if let Some(script) = self.script.take() {
            return self.run_script(script);
        }
        self.adopt_js_work();
        let now = self.now();
        while let Some(id) = self.timers.take_due(now) {
            self.tasks
                .push(crate::task_queue::TaskSource::Timer, WorkerTask::Timer(id));
        }
        let Some(task) = self.tasks.pop_front() else {
            return false;
        };
        let _turn = self.js.enter_turn();
        match task {
            WorkerTask::Message { message } => {
                let delivered = with_message_buffers(&self.world, &message, || {
                    self.js
                        .deliver_worker_event(self.id.get(), &message.data, &message.ports)
                });
                if let Err(error) = delivered {
                    // https://html.spec.whatwg.org/multipage/web-messaging.html#transfer-receiving-steps
                    self.forget_endpoints(&message.ports);
                    self.report_error(&error);
                }
            }
            WorkerTask::PortMessage { endpoint, message } => {
                self.deliver_port_message(endpoint, message);
            }
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
                message,
                source,
            } => {
                let delivered = with_message_buffers(&self.world, &message, || {
                    self.js
                        .deliver_broadcast_message(&origin, &name, &message.data, source)
                });
                if let Err(error) = delivered {
                    self.report_error(&error);
                }
            }
            WorkerTask::Timer(id) => {
                if let Some(js_id) = self.timers.remove(id) {
                    self.js_timers.remove(&js_id);
                    if let Err(error) = self.js.fire_timer(js_id) {
                        self.report_error(&error);
                    }
                }
            }
            WorkerTask::RejectionTasks => {
                self.js.fire_rejection_tasks();
            }
        }
        !self.closed
    }

    const fn is_closed(&self) -> bool {
        self.closed
    }

    // https://html.spec.whatwg.org/multipage/workers.html#close-a-worker
    fn close(&mut self) {
        self.closed = true;
        self.timers.clear();
        self.js_timers.clear();
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
                WorkerTask::Message { message } | WorkerTask::PortMessage { message, .. } => {
                    self.forget_endpoints(&message.ports);
                }
                WorkerTask::Connect { endpoint } => self.forget_endpoints(&[endpoint]),
                WorkerTask::PortClosed { .. }
                | WorkerTask::Broadcast { .. }
                | WorkerTask::Timer(_)
                | WorkerTask::RejectionTasks => {}
            }
        }
    }

    // https://html.spec.whatwg.org/multipage/web-messaging.html#message-port-post-message-steps
    fn deliver_port_message(
        &mut self,
        endpoint: u64,
        message: crate::messaging::SerializedMessage,
    ) {
        let frame = self.world.borrow().frame();
        let shared = self.world.borrow().shared();
        let Some(message) = shared.take_port_message_unless_owned(endpoint, frame, message) else {
            return;
        };
        let delivered = with_message_buffers(&self.world, &message, || {
            self.js
                .deliver_port_message(endpoint, &message.data, &message.ports)
        });
        match delivered {
            Ok(true) => {}
            // https://html.spec.whatwg.org/multipage/web-messaging.html#message-port-post-message-steps
            Ok(false) => {
                if let Err(error) = self.js.deliver_port_message_error(endpoint) {
                    self.report_error(&error);
                }
            }
            Err(error) => {
                self.forget_endpoints(&message.ports);
                self.report_error(&error);
            }
        }
    }

    fn deliver_port_close(&mut self, endpoint: u64) {
        let frame = self.world.borrow().frame();
        let shared = self.world.borrow().shared();
        if !shared.take_port_close_unless_owned(endpoint, frame) {
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
                let now = self.now();
                let id = self.timers.schedule(now, timeout.delay, timeout.js_id);
                self.js_timers.insert(timeout.js_id, id);
            }
            RealmRequest::ClearTimer(timeout) => {
                if let Some(id) = self.js_timers.remove(&timeout.js_id) {
                    self.timers.remove(id);
                }
            }
            RealmRequest::Fetch(fetch) => self.start_fetch(*fetch),
            RealmRequest::AbortFetch(request) => {
                if let Some((cancel, _pending)) = self.fetches.remove(&request.js_id) {
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
            RealmRequest::Script(_)
            | RealmRequest::HistoryPush { .. }
            | RealmRequest::HistoryTraverse { .. }
            | RealmRequest::FragmentNavigation { .. }
            | RealmRequest::CookieChanges
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
            let partition_key = world.partition_key();
            let doc = world.doc();
            (
                doc.origin.clone(),
                doc.referrer_url.clone(),
                doc.policy.referrer_policy,
                doc.same_site_cookie_policy,
                partition_key,
                crate::RedirectGate::new(
                    doc.policy.csp.clone(),
                    &doc.origin,
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
        let commands = self.commands.clone();
        let active = Arc::clone(&self.active);
        let wake = Arc::clone(&self.wake);
        let pending = PendingFetchGuard::acquire(&self.pending_fetches);
        let cancel = self.world.borrow().runtime.services.start_dial(
            dial,
            Box::new(move |outcome| {
                // https://fetch.spec.whatwg.org/#concept-network-error
                let queued = QueuedCommand {
                    command: WorkerCommand::FetchDone {
                        js_id,
                        outcome: outcome.ok().map(Box::new),
                    },
                    guard: BusyGuard::acquire(&active, &wake),
                };
                let _ = commands.send(queued);
            }),
        );
        self.fetches.insert(js_id, (cancel, pending));
    }

    fn finish_fetch(&mut self, js_id: JsFetchId, outcome: Option<DialOutcome>) {
        self.fetches.remove(&js_id);
        if self.closed {
            return;
        }
        let _turn = self.js.enter_turn();
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

    fn fail_fetch(&mut self, js_id: JsFetchId) {
        if let Err(error) = self.js.finish_js_fetch(js_id, None) {
            self.report_error(&error);
        }
    }
}

// https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model
fn prepare_worker_script(created: &CreatedWorker) -> InitialScript {
    if created.module {
        if created.url.scheme() == "data" || created.url.scheme() == "blob" {
            // https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model
            return InitialScript::Ready(Ok(LoadedScript {
                script: WorkerScript::Module,
                url: created.url.clone(),
                embedder_policy: None,
                csp: created.policy.csp.clone(),
            }));
        }
        if created.url.scheme() != "http" && created.url.scheme() != "https" {
            return InitialScript::Ready(Err(()));
        }
        return InitialScript::Dial(Box::new(script_dial_request(
            created,
            RequestPolicy::ModuleWorkerScript {
                credentials: created.credentials,
            },
        )));
    }
    if created.url.scheme() == "data" {
        let Some((_, body)) = crate::engine::decode_data_url_bounded(created.url.as_str()) else {
            return InitialScript::Ready(Err(()));
        };
        // https://infra.spec.whatwg.org/#utf-8-decode
        return InitialScript::Ready(Ok(LoadedScript {
            script: WorkerScript::Classic(String::from_utf8_lossy(&body).into_owned()),
            url: created.url.clone(),
            embedder_policy: None,
            csp: created.policy.csp.clone(),
        }));
    }
    if created.url.scheme() == "blob" {
        // https://html.spec.whatwg.org/multipage/webappapis.html#fetch-a-classic-worker-script
        return match &created.local_script {
            Some(source) => InitialScript::Ready(Ok(LoadedScript {
                script: WorkerScript::Classic(source.clone()),
                url: created.url.clone(),
                embedder_policy: None,
                csp: created.policy.csp.clone(),
            })),
            None => InitialScript::Ready(Err(())),
        };
    }
    if created.url.scheme() != "http" && created.url.scheme() != "https" {
        return InitialScript::Ready(Err(()));
    }
    InitialScript::Dial(Box::new(script_dial_request(
        created,
        RequestPolicy::WorkerScript,
    )))
}

fn script_dial_request(created: &CreatedWorker, policy: RequestPolicy) -> DialRequest {
    let mut dial = DialRequest::new(policy, &created.url, &created.origin);
    dial.referrer = ReferrerRequest::Client {
        url: created.referrer_url.clone(),
    };
    dial.referrer_policy = created.policy.referrer_policy.unwrap_or_default();
    dial.cookie_policy = created.same_site_cookies.cookie_policy().into();
    dial.partition_key.clone_from(&created.partition_key);
    dial.timeout_ms = 30_000;
    dial
}

// https://html.spec.whatwg.org/multipage/workers.html#fetch-a-classic-worker-script
fn finish_dialed_script(
    created: &CreatedWorker,
    outcome: Option<DialOutcome>,
) -> Result<LoadedScript, ()> {
    let outcome = outcome.ok_or(())?;
    if !(200..300).contains(&outcome.status) {
        return Err(());
    }
    if created.module {
        return Ok(LoadedScript {
            script: WorkerScript::Module,
            url: outcome.final_url,
            embedder_policy: Some(crate::protocol::cross_origin_embedder_policy(
                &outcome.headers,
            )),
            csp: response_csp(&outcome.headers),
        });
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
        crate::protocol::ResponseBody::Bytes(body) => Ok(LoadedScript {
            // https://infra.spec.whatwg.org/#utf-8-decode
            script: WorkerScript::Classic(String::from_utf8_lossy(&body).into_owned()),
            url: outcome.final_url,
            embedder_policy: Some(crate::protocol::cross_origin_embedder_policy(
                &outcome.headers,
            )),
            csp: response_csp(&outcome.headers),
        }),
        crate::protocol::ResponseBody::Streamed(_) | crate::protocol::ResponseBody::Undecodable => {
            Err(())
        }
    }
}

// https://html.spec.whatwg.org/multipage/browsers.html#initialize-a-policy-container-from-a-fetch-response
fn response_csp(headers: &[(String, String)]) -> crate::csp::List {
    crate::csp::List::from_headers(
        net::combined_header_in(headers, "content-security-policy").as_deref(),
        net::combined_header_in(headers, "content-security-policy-report-only").as_deref(),
    )
}

fn content_type_essence(value: &str) -> &str {
    value.split(';').next().unwrap_or_default().trim()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use tokio::sync::Notify;
    use url::Url;

    use super::{
        BusyGuard, CreatedWorker, NO_DEADLINE, SharedWorkerSameSiteCookies, WorkerDirectory,
        WorkerId, decode_deadline, encode_deadline,
    };
    use crate::js::{SharedRuntime, Stop};
    use crate::protocol::{BrowserServices, FrameId, PolicyContainer, SandboxFlags};
    use crate::test_services::NullServices;

    #[test]
    fn deadline_encoding_round_trips_and_reserves_sentinel() {
        let epoch = Instant::now();
        assert_eq!(encode_deadline(None, epoch), NO_DEADLINE);
        assert_eq!(decode_deadline(NO_DEADLINE, epoch), None);

        let target = epoch + Duration::from_millis(250);
        let raw = encode_deadline(Some(target), epoch);
        assert_ne!(raw, NO_DEADLINE);
        assert_eq!(decode_deadline(raw, epoch), Some(target));
    }

    #[test]
    fn busy_guard_balances_active_count_on_drop() {
        let active = Arc::new(AtomicUsize::new(0));
        let wake = Arc::new(Notify::new());
        {
            let first = BusyGuard::acquire(&active, &wake);
            assert_eq!(active.load(Ordering::Acquire), 1);
            {
                let _second = BusyGuard::acquire(&active, &wake);
                assert_eq!(active.load(Ordering::Acquire), 2);
            }
            assert_eq!(active.load(Ordering::Acquire), 1);
            drop(first);
        }
        assert_eq!(active.load(Ordering::Acquire), 0);
    }

    #[test]
    fn terminate_cancels_in_flight_script_dial_immediately() {
        let started = Arc::new(AtomicBool::new(false));
        let cancelled = Arc::new(AtomicBool::new(false));
        let services: Arc<dyn BrowserServices> = Arc::new(NullServices::hanging_dial(
            Arc::clone(&started),
            Arc::clone(&cancelled),
        ));
        let common = Arc::new(SharedRuntime::new(
            services,
            net::ValidIdentity::default(),
            Arc::new(Notify::new()),
            Arc::new(Stop::new()),
            parley::FontContext::default(),
        ));
        let dir = Arc::new(WorkerDirectory::new(Arc::clone(&common)));
        let url = Url::parse("https://example.test/slow-worker.js").expect("valid url");
        let origin = url.origin();
        let id = dir.spawn_worker(CreatedWorker {
            id: WorkerId::new(1),
            parent: FrameId::MAIN,
            url: url.clone(),
            name: String::new(),
            module: false,
            shared: false,
            credentials: net::cors::CredentialsMode::SameOrigin,
            extended_lifetime: false,
            same_site_cookies: SharedWorkerSameSiteCookies::All,
            origin: origin.clone(),
            worker_origin: origin.clone(),
            top_level: origin,
            partition_key: None,
            policy: PolicyContainer::default(),
            sandbox: SandboxFlags::default(),
            referrer_url: url,
            local_script: None,
        });

        let wait_started = Instant::now();
        while !started.load(Ordering::Acquire) {
            assert!(
                wait_started.elapsed() < Duration::from_secs(2),
                "worker did not start script dial"
            );
            std::thread::sleep(Duration::from_millis(2));
        }

        let terminate_started = Instant::now();
        dir.terminate(id);
        while !cancelled.load(Ordering::Acquire) {
            assert!(
                terminate_started.elapsed() < Duration::from_secs(2),
                "terminate did not cancel in-flight script dial"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(terminate_started.elapsed() < Duration::from_secs(2));
        assert!(!dir.is_live(id));
    }
}
