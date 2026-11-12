//! Workers: one documentless realm per worker.
//!
//! A dedicated worker runs on its own OS thread with its own `QuickJS`
//! runtime, so its script runs in parallel with the page
//! (<https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model>).
//! It shares the page engine's services and cross-realm state (the frame
//! tree, the `MessagePort` table, the Blob URL Store, and the
//! `SharedArrayBuffer` table), and it owns its global
//! (`DedicatedWorkerGlobalScope`), task queue, and timers. It has no document,
//! browsing context, or history
//! (<https://html.spec.whatwg.org/multipage/workers.html#dedicated-workers-and-the-dedicatedworkerglobalscope-interface>).
//!
//! The thread starts inside `new Worker()`, not at the engine's next turn: a
//! page may post to the worker and then spin in the constructor's own
//! synchronous caller
//! (<https://html.spec.whatwg.org/multipage/workers.html#dom-worker>).
//!
//! A `SharedWorker` realm still runs in the page engine, because its identity
//! is shared by several creators and the engine owns that table.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::time::Instant;
use url::Url;

use crate::js::world::World;
use crate::js::{
    FrameRuntime, JsError, JsFetchId, JsFetchRequest, JsRealm, JsRuntimeHandle, PageTimerId,
    RealmRequest, SharedJsRuntime, Stop, WorkerScriptError,
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

/// A `SharedWorker`'s `sameSiteCookies` value, resolved to a concrete choice.
///
/// The IDL member is optional; the default is `all` in a first-party site
/// context and `none` otherwise. The constructor rejects an explicit `all`
/// outside a first-party site context, so a realm only ever sees a legal value
/// (<https://privacycg.github.io/saa-non-cookie-storage/#shared-worker>).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum SharedWorkerSameSiteCookies {
    /// Every `SameSite` value is available to the worker.
    All,
    /// Only `SameSite=None` cookies are sent or stored.
    None,
}

impl SharedWorkerSameSiteCookies {
    /// Parses the IDL enum spelling, which is exact-match.
    pub(crate) fn from_idl(value: &str) -> Option<Self> {
        match value {
            "all" => Some(Self::All),
            "none" => Some(Self::None),
            _ => None,
        }
    }

    /// The jar policy this option selects.
    pub(crate) const fn cookie_policy(self) -> net::SameSiteCookiePolicy {
        match self {
            Self::All => net::SameSiteCookiePolicy::All,
            Self::None => net::SameSiteCookiePolicy::NoneOnly,
        }
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
    /// The `WorkerOptions.sameSiteCookies` a shared worker's connections must
    /// agree on, resolved to a concrete value: the option defaults to `all`
    /// in a first-party site context and `none` otherwise
    /// (<https://privacycg.github.io/saa-non-cookie-storage/#shared-worker>).
    pub(crate) same_site_cookies: SharedWorkerSameSiteCookies,
    /// The creator's origin: the "outside settings object"'s origin, which the
    /// module graph and the script fetch use.
    pub(crate) origin: url::Origin,
    /// The origin the worker realm itself gets. A `data:` worker is opaque and
    /// must match its own broadcast origin, so it cannot reuse the creator's
    /// (<https://html.spec.whatwg.org/multipage/workers.html#dom-worker>).
    pub(crate) worker_origin: url::Origin,
    pub(crate) top_level: url::Origin,
    /// The cookie partition key of the creator's top-level site, which every
    /// request the worker's script, module graph, `fetch`, and XHR make uses
    /// (<https://github.com/privacycg/CHIPS#computing-the-cookie-partition-key>).
    pub(crate) partition_key: Option<String>,
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
    /// The resolved `WorkerOptions.sameSiteCookies`; the constructor rejects
    /// `"all"` outside a first-party site context before it gets here.
    pub(crate) same_site_cookies: SharedWorkerSameSiteCookies,
}

impl CreatedWorkerOptions {
    /// The options of a dedicated worker, which has no shared-realm key.
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
    /// Captures the creator's origin, policy container, sandbox set, and
    /// referrer for a worker created from `world`.
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

/// A shared worker's identity in one engine.
///
/// A `SharedWorkerGlobalScope` is matched by its constructor storage key, its
/// constructor URL, and its name
/// (<https://html.spec.whatwg.org/multipage/workers.html#dom-sharedworker>).
/// The storage key is the creator's origin plus the top-level site of the
/// creator's environment, so each top-level site gets its own realm and one
/// site's data is never shared with another
/// (<https://storage.spec.whatwg.org/#obtain-a-storage-key-for-non-storage-purposes>).
#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct SharedWorkerKey {
    pub(crate) origin: url::Origin,
    /// The storage key's top-level site, `None` only when the creator has no
    /// top-level site (an opaque top-level origin).
    pub(crate) top_level_site: Option<String>,
    pub(crate) url: Url,
    pub(crate) name: String,
    /// Shared workers with other cookie options are a different realm
    /// (<https://privacycg.github.io/saa-non-cookie-storage/#shared-worker>).
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

/// The options two connections to the same shared worker must agree on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SharedWorkerOptions {
    pub(crate) module: bool,
    pub(crate) credentials: net::cors::CredentialsMode,
    pub(crate) extended_lifetime: bool,
    pub(crate) same_site_cookies: SharedWorkerSameSiteCookies,
}

/// One lifecycle or message request a worker host function or worker thread
/// queued for the engine.
pub(crate) enum WorkerRequest {
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
    PostToParent {
        parent: FrameId,
        worker: WorkerId,
        payload: String,
        ports: Vec<u64>,
    },
    /// A timer a worker realm created. The engine owns worker timers so the
    /// page engine's clock drives them, exactly as it drives a document's.
    SetTimer {
        worker: WorkerId,
        js_id: PageTimerId,
        delay: Duration,
    },
    /// `clearTimeout` / `clearInterval` inside a worker realm.
    ClearTimer {
        worker: WorkerId,
        js_id: PageTimerId,
    },
    /// `close()` inside a shared worker realm. A dedicated worker closes its
    /// own thread directly; a shared realm lives in the page engine, so the
    /// engine marks it closed
    /// (<https://html.spec.whatwg.org/multipage/workers.html#dom-sharedworkerglobalscope-close>).
    Close {
        id: WorkerId,
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
    /// A worker's policy blocked a request. The violation event belongs at the
    /// environment's global object, and a worker's global has no page-side
    /// event target, so the request names the frame that owns the worker and
    /// the engine reports it at that document
    /// (<https://w3c.github.io/webappsec-csp/#report-violation>).
    CspViolation {
        parent: FrameId,
        violations: Vec<crate::csp::Violation>,
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

/// One action the engine sends a dedicated worker's thread.
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
    /// A nested worker posted at this worker's `Worker` object.
    NestedMessage {
        worker: u64,
        payload: String,
        ports: Vec<u64>,
    },
    /// A nested worker reported a runtime error at this worker's `Worker`
    /// object.
    NestedError(Box<WorkerError>),
    /// A nested worker never started.
    NestedLoadFailure {
        worker: u64,
    },
    /// A nested worker's policy blocked a request; this worker forwards it
    /// toward the owning document.
    CspViolation {
        violations: Vec<crate::csp::Violation>,
    },
    /// `terminate()` was called. The flag in [`WorkerThreadState`] also
    /// interrupts a thread that is inside script.
    Terminate,
}

/// The state a dedicated worker's thread publishes for the engine.
pub(crate) struct WorkerThreadState {
    /// Set by `terminate()`. The worker's interrupt handler reads it, so a
    /// worker stuck in synchronous script stops
    /// (<https://html.spec.whatwg.org/multipage/workers.html#terminate-a-worker>).
    terminate: Arc<AtomicBool>,
    /// Commands the thread accepted but has not finished, plus one while its
    /// top-level script runs. A nonzero count means a drain still has work.
    active: Arc<AtomicUsize>,
    closed: Arc<AtomicBool>,
    /// Fetch completions the worker thread owns; the engine waits for a drain
    /// instead of judging the page idle while one is in flight.
    pending_fetches: Arc<AtomicUsize>,
}

/// The process-wide handles a worker thread needs to build its own realm.
pub(crate) struct WorkerDirectory {
    services: Arc<dyn crate::protocol::BrowserServices>,
    identity: net::ValidIdentity,
    wake: Arc<tokio::sync::Notify>,
    stop: Arc<Stop>,
    shared: crate::messaging::SharedHandle,
    requests: crate::js::WorkerRequests,
    next_worker: Arc<AtomicU64>,
    next_broadcast: Arc<AtomicU64>,
    font_ctx: parley::FontContext,
    object_urls: crate::js::ObjectUrlStore,
    shared_buffers: Arc<crate::js::SharedBufferTable>,
    /// Live dedicated workers, read and pruned by the engine.
    workers: Mutex<BTreeMap<WorkerId, WorkerHandle>>,
}

impl WorkerDirectory {
    #[allow(
        clippy::too_many_arguments,
        reason = "one argument per process-wide handle the directory shares with worker threads"
    )]
    pub(crate) fn new(
        services: Arc<dyn crate::protocol::BrowserServices>,
        identity: net::ValidIdentity,
        wake: Arc<tokio::sync::Notify>,
        stop: Arc<Stop>,
        shared: crate::messaging::SharedHandle,
        requests: crate::js::WorkerRequests,
        next_worker: Arc<AtomicU64>,
        next_broadcast: Arc<AtomicU64>,
        font_ctx: parley::FontContext,
        object_urls: crate::js::ObjectUrlStore,
        shared_buffers: Arc<crate::js::SharedBufferTable>,
    ) -> Self {
        Self {
            services,
            identity,
            wake,
            stop,
            shared,
            requests,
            next_worker,
            next_broadcast,
            font_ctx,
            object_urls,
            shared_buffers,
            workers: Mutex::new(BTreeMap::new()),
        }
    }

    fn workers(&self) -> MutexGuard<'_, BTreeMap<WorkerId, WorkerHandle>> {
        self.workers.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The thread-local runtime a worker realm runs on. The process-wide
    /// handles are cloned; the realm bookkeeping is fresh, because a `QuickJS`
    /// runtime and its wrappers belong to one thread
    /// (<https://bellard.org/quickjs/quickjs.html>, `JSRuntime`).
    fn thread_runtime(self: &Arc<Self>, js_runtime: JsRuntimeHandle) -> FrameRuntime {
        FrameRuntime {
            services: Arc::clone(&self.services),
            identity: self.identity.clone(),
            js_runtime,
            wake: Arc::clone(&self.wake),
            stop: Arc::clone(&self.stop),
            documents: Rc::new(RefCell::new(crate::documents::DocumentStore::default())),
            registry: Rc::new(RefCell::new(crate::js::RealmRegistry::default())),
            shared: self.shared.clone(),
            pending_storage: Rc::new(RefCell::new(Vec::new())),
            turn_deadline: Rc::new(std::cell::Cell::new(None)),
            worker_requests: Arc::clone(&self.requests),
            next_worker: Arc::clone(&self.next_worker),
            next_broadcast: Arc::clone(&self.next_broadcast),
            worker_dir: Arc::clone(self),
            font_ctx: self.font_ctx.clone(),
            object_urls: Arc::clone(&self.object_urls),
            shared_buffers: Arc::clone(&self.shared_buffers),
        }
    }

    /// Starts a dedicated worker on its own OS thread.
    ///
    /// The thread is spawned here, inside `new Worker()`, not at the engine's
    /// next reconcile: `Worker-creation-happens-in-parallel` posts and then
    /// spins in the same synchronous function, so a start deferred until the
    /// script returns could never win
    /// (<https://html.spec.whatwg.org/multipage/workers.html#dom-worker>).
    pub(crate) fn spawn_dedicated(self: &Arc<Self>, created: CreatedWorker) -> WorkerId {
        let id = created.id;
        let (commands, received) = std::sync::mpsc::channel();
        let state = WorkerThreadState {
            terminate: Arc::new(AtomicBool::new(false)),
            // The top-level script counts as work until it finishes.
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
        self.workers().insert(id, handle);

        let directory = Arc::clone(self);
        let thread_state = WorkerThreadState {
            terminate: Arc::clone(&state.terminate),
            active: Arc::clone(&state.active),
            closed: Arc::clone(&state.closed),
            pending_fetches: Arc::clone(&state.pending_fetches),
        };
        let spawned = std::thread::Builder::new()
            .name(format!("tinybrowser-worker-{}", id.get()))
            .spawn(move || run_worker(&created, &directory, &received, &thread_state));
        if let Err(error) = spawned {
            // The environment could not be created: the `Worker` object hears
            // the same plain `error` event as a script that failed to load
            // (<https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model>,
            // step 14).
            let _ = error;
            state.closed.store(true, Ordering::Release);
            state.active.store(0, Ordering::Release);
        }
        id
    }

    /// Sends one command to a dedicated worker.
    pub(crate) fn send(&self, id: WorkerId, command: WorkerCommand) -> bool {
        let mut workers = self.workers();
        let Some(worker) = workers.get_mut(&id) else {
            return false;
        };
        worker.send(command)
    }

    /// Sets a dedicated worker's terminate flag and wakes its thread.
    pub(crate) fn terminate(&self, id: WorkerId) {
        if let Some(worker) = self.workers().get_mut(&id) {
            worker.terminate();
        }
    }

    /// Runs `f` over the live dedicated workers.
    pub(crate) fn with_workers<R>(
        &self,
        f: impl FnOnce(&mut BTreeMap<WorkerId, WorkerHandle>) -> R,
    ) -> R {
        f(&mut self.workers())
    }

    /// Drops every dedicated worker, interrupting its thread.
    pub(crate) fn terminate_all(&self) {
        let mut workers = self.workers();
        for worker in workers.values_mut() {
            worker.terminate();
        }
        workers.clear();
    }

    /// Removes workers whose thread reported it closed.
    pub(crate) fn remove_closed(&self) {
        self.workers().retain(|_, worker| !worker.is_closed());
    }
}

/// The engine's handle to one dedicated worker running on another thread.
///
/// The realm, its `QuickJS` runtime, and its task queue belong to the worker
/// thread. The handle owns what the engine schedules: commands, the timer
/// mirror, and the flags the thread publishes.
pub(crate) struct WorkerHandle {
    origin: url::Origin,
    commands: Sender<WorkerCommand>,
    terminate: Arc<AtomicBool>,
    active: Arc<AtomicUsize>,
    closed: Arc<AtomicBool>,
    pending_fetches: Arc<AtomicUsize>,
    /// The engine owns worker timers so the page engine's clock drives them.
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

    /// Runs the commands and timers that are ready, and reports whether work
    /// remains. The worker thread runs concurrently, so this never executes
    /// script on the page thread.
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

    /// Schedules a timer a worker realm created, and mirrors the handle the
    /// worker's JS uses.
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
        // Wakes a thread blocked on the channel; a thread inside script sees
        // the flag through its interrupt handler.
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

/// Runs one dedicated worker's realm on its own thread.
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
            // The environment never started: the owner's `Worker` object hears
            // a plain `error` event
            // (<https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model>,
            // step 14).
            runtime.push_worker_request_waking(WorkerRequest::LoadFailed {
                worker: failure.worker,
                parent: failure.parent,
            });
            state.active.fetch_sub(1, Ordering::AcqRel);
            state.closed.store(true, Ordering::Release);
            return;
        }
    };
    // The top-level script runs before any command, and `new Worker()` has
    // already returned on the page thread
    // (<https://html.spec.whatwg.org/multipage/workers.html#dom-worker>).
    worker.drain_until_idle(&state.terminate);
    state.active.fetch_sub(1, Ordering::AcqRel);
    if worker.is_closed() || state.terminate.load(Ordering::Acquire) {
        state.closed.store(true, Ordering::Release);
        return;
    }
    loop {
        // A fetch completion arrives on the worker's own channel, not the
        // command channel, so a thread waiting for commands also polls while a
        // dial is in flight. The engine keeps the page draining on
        // `has_pending_dials` meanwhile
        // (<https://fetch.spec.whatwg.org/#fetch-processresponse>).
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
        terminate: Arc<AtomicBool>,
        pending_fetches: Arc<AtomicUsize>,
    ) -> Result<Self, WorkerLoadFailure> {
        let failure = WorkerLoadFailure {
            worker: id,
            parent: created.parent,
        };
        let (script, worker_url, own_embedder_policy, response_csp) = if created.module {
            // A module worker's url is its response URL: relative imports,
            // `fetch`, and XHR resolve against the redirect target, not the
            // request URL
            // (<https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model>,
            // step 14).
            let (url, embedder_policy, csp) =
                fetch_module_url(created, runtime).map_err(|()| failure)?;
            (WorkerScript::Module, url, embedder_policy, csp)
        } else {
            let (source, response_url, embedder_policy, csp) =
                fetch_worker_source(created, runtime).map_err(|()| failure)?;
            // A script body is decoded with UTF-8 decode, whose replacement
            // mode substitutes U+FFFD rather than failing the load
            // (<https://infra.spec.whatwg.org/#utf-8-decode>).
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
            // The realm's origin was fixed when the constructor captured the
            // worker: a `data:` worker gets its own opaque origin, which is
            // also the origin its broadcasts carry
            // (<https://html.spec.whatwg.org/multipage/workers.html#dom-worker>).
            state.origin = created.worker_origin.clone();
            state.top_level = Some(if created.url.scheme() == "data" {
                created.worker_origin.clone()
            } else {
                created.top_level.clone()
            });
            state.policy = created.policy.clone();
            // The realm's own requests use the script response's policy; the
            // creator's container only governs the script graph, which the
            // outside settings object fetches
            // (<https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model>).
            state.policy.csp = response_csp;
            state.state.worker_owner_csp = Some(created.policy.csp.clone());
            if let Some(compatible) = own_embedder_policy {
                state.policy.cross_origin_embedder_policy = compatible;
            }
            // A worker is cross-origin isolated only when its creator is and
            // its own script response permits it. A `data:` or `blob:` script
            // inherits the whole container, including the decision
            // (<https://html.spec.whatwg.org/multipage/webappapis.html#concept-settings-object-cross-origin-isolated-capability>).
            state.policy.cross_origin_isolated = state.policy.cross_origin_isolated
                && state.policy.cross_origin_embedder_policy.compatible();
            state.sandbox = created.sandbox;
            state.referrer_url = created.referrer_url.clone();
            state.document_referrer = Some(created.referrer_url.clone());
            state.same_site_cookie_policy = created.same_site_cookies.cookie_policy();
            // A module worker's top-level graph is fetched with the outside
            // settings object's origin, which keeps `data:` worker static
            // imports same-origin to the creator; the worker keeps its own
            // origin for dynamic imports after the graph runs
            // (<https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model>,
            // step 8).
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

    /// Forwards a nested worker's blocked-request violations to the engine,
    /// which reports them at the document that owns this realm.
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

    /// Runs the worker's own task queue until it is empty, stopping if the
    /// engine terminated it.
    fn drain_until_idle(&mut self, terminate: &Arc<AtomicBool>) {
        while !terminate.load(Ordering::Acquire) {
            if !self.drain_step() {
                break;
            }
        }
    }

    /// Applies one command the engine sent this worker's thread.
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

    /// Runs one task and reports whether the engine must step the worker
    /// again. A closed worker drains nothing.
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
                            // The graph failed to load, but the violations that
                            // blocked it were queued during the load and still
                            // belong to the document
                            // (<https://w3c.github.io/webappsec-csp/#report-violation>).
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
            // A module graph fetch, or a synchronous XHR, can queue a CSP
            // violation during the top-level script. The environment's task
            // queue still owns it after the script returns, so forward it
            // before the worker can go idle
            // (<https://w3c.github.io/webappsec-csp/#report-violation>).
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

    /// `close()` stops the worker before its next task; the engine drops the
    /// realm after the current drain so it is never destroyed from inside its
    /// own script. Both close and terminate discard the tasks still queued
    /// (<https://html.spec.whatwg.org/multipage/workers.html#close-a-worker>).
    pub(crate) fn close(&mut self) {
        self.closed = true;
        // A `keepalive` request outlives the worker that made it, so the ones
        // still in the realm's queue start before the realm goes away; every
        // other queued request dies with it
        // (<https://fetch.spec.whatwg.org/#keepalive-flag>).
        self.start_queued_keepalive_fetches();
        self.discard_queued_tasks();
    }

    /// Starts the `keepalive` fetches this realm queued but has not run.
    fn start_queued_keepalive_fetches(&mut self) {
        let pending = {
            let world = self.world.borrow();
            world.take_queued_keepalive_fetches()
        };
        for request in pending {
            self.apply_realm_request(request);
        }
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
                // A dedicated worker's thread cannot see the page engine's
                // clock, so its timers are scheduled by the engine handle. A
                // shared worker runs in the page engine and schedules its own.
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
            RealmRequest::ClearTimer(js_id) => {
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
            RealmRequest::AbortFetch(js_id) => {
                if let Some(cancel) = self.fetches.remove(&js_id) {
                    cancel();
                }
            }
            RealmRequest::CspViolation(_, violations) => {
                // A worker cannot dispatch `securitypolicyviolation` at a
                // document; the engine routes it to the document that owns
                // the worker chain
                // (<https://w3c.github.io/webappsec-csp/#report-violation>).
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
            // A worker's fetch stores cookies in the same jar a Window reads.
            // The worker has no `CookieStore` of its own, so every Window is
            // queued and each diffs against its own baseline
            // (<https://cookiestore.spec.whatwg.org/#process-cookie-changes>).
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
        // A `blob:` or `data:` worker has no response headers to take a
        // policy from, so it inherits its creator's
        // (<https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model>).
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

/// The policy container a worker's script response initializes: the policies
/// its own `Content-Security-Policy` and `-Report-Only` headers name
/// (<https://html.spec.whatwg.org/multipage/browsers.html#initialize-a-policy-container-from-a-fetch-response>).
fn response_csp(headers: &[(String, String)]) -> crate::csp::List {
    crate::csp::List::from_headers(
        net::combined_header_in(headers, "content-security-policy").as_deref(),
        net::combined_header_in(headers, "content-security-policy-report-only").as_deref(),
    )
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
        // A `data:` worker script is not MIME-checked; every MIME type is
        // allowed, including an empty one (WPT `workers/data-url.html`). It
        // inherits its creator's policy container.
        let (_, body) = crate::engine::decode_data_url_bounded(created.url.as_str()).ok_or(())?;
        return Ok((body, created.url.clone(), None, created.policy.csp.clone()));
    }
    if created.url.scheme() == "blob" {
        // A `blob:` worker script is the store entry read when the constructor
        // ran. Only a network-scheme response is MIME-checked
        // (<https://html.spec.whatwg.org/multipage/webappapis.html#fetch-a-classic-worker-script>,
        // step 5), so the backing Blob's type is irrelevant
        // (WPT `workers/Worker_script_mimetype.htm`).
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

/// The essence of a `Content-Type` value, without parameters or whitespace.
fn content_type_essence(value: &str) -> &str {
    value.split(';').next().unwrap_or_default().trim()
}
