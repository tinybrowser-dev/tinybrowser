use std::cell::{Cell, RefCell};
use std::ops::Deref;
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use tokio::sync::Notify;

use crate::documents::DocumentStore;
use crate::messaging::SharedHandle;
use crate::protocol::BrowserServices;
use crate::worker::{WorkerDirectory, WorkerId, WorkerRequest};

use super::JsRuntimeHandle;
use super::RealmRegistry;

pub(crate) struct SharedRuntime {
    pub(crate) services: Arc<dyn BrowserServices>,
    pub(crate) clock: net::Clock,
    pub(crate) animation_epoch: Instant,
    // https://wicg.github.io/ua-client-hints/#http-ua-hints
    pub(crate) initial_identity: net::ValidIdentity,
    pub(crate) wake: Arc<Notify>,
    pub(crate) stop: Arc<Stop>,
    pub(crate) shared: SharedHandle,
    worker_requests_tx: Sender<WorkerRequest>,
    worker_requests_rx: Mutex<Receiver<WorkerRequest>>,
    pending_worker_requests: AtomicUsize,
    pub(crate) next_worker: Arc<AtomicU64>,
    pub(crate) next_broadcast: Arc<AtomicU64>,
    pub(crate) next_blob_scope: Arc<AtomicU64>,
    pub(crate) font_ctx: parley::FontContext,
}

impl SharedRuntime {
    pub(crate) fn new(
        services: Arc<dyn BrowserServices>,
        initial_identity: net::ValidIdentity,
        wake: Arc<Notify>,
        stop: Arc<Stop>,
        font_ctx: parley::FontContext,
    ) -> Self {
        let clock = services.clock();
        let animation_epoch = clock.monotonic();
        let (worker_requests_tx, worker_requests_rx) = std::sync::mpsc::channel();
        Self {
            services,
            clock,
            animation_epoch,
            initial_identity,
            wake,
            stop,
            shared: SharedHandle::new(crate::messaging::Shared::default()),
            worker_requests_tx,
            worker_requests_rx: Mutex::new(worker_requests_rx),
            pending_worker_requests: AtomicUsize::new(0),
            next_worker: Arc::new(AtomicU64::new(1)),
            next_broadcast: Arc::new(AtomicU64::new(1)),
            next_blob_scope: Arc::new(AtomicU64::new(1)),
            font_ctx,
        }
    }

    pub(crate) fn now(&self) -> Instant {
        self.clock.monotonic()
    }

    // https://drafts.csswg.org/cssom-view/#scroll-an-element
    pub(crate) fn animation_seconds(&self) -> f64 {
        self.clock
            .monotonic()
            .saturating_duration_since(self.animation_epoch)
            .as_secs_f64()
    }

    pub(crate) fn push_worker_request_waking(&self, request: WorkerRequest) {
        self.pending_worker_requests.fetch_add(1, Ordering::AcqRel);
        if self.worker_requests_tx.send(request).is_err() {
            self.pending_worker_requests.fetch_sub(1, Ordering::AcqRel);
            return;
        }
        self.wake.notify_one();
    }

    pub(crate) fn take_worker_requests(&self) -> Vec<WorkerRequest> {
        let rx = self
            .worker_requests_rx
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let mut out = Vec::new();
        while let Ok(request) = rx.try_recv() {
            self.pending_worker_requests.fetch_sub(1, Ordering::AcqRel);
            out.push(request);
        }
        out
    }

    pub(crate) fn has_worker_requests(&self) -> bool {
        self.pending_worker_requests.load(Ordering::Acquire) > 0
    }
}

#[derive(Clone)]
pub(crate) struct FrameRuntime {
    pub(crate) common: Arc<SharedRuntime>,
    pub(crate) engine_id: u64,
    // https://wicg.github.io/ua-client-hints/#http-ua-hints
    pub(crate) identity: Rc<RefCell<net::ValidIdentity>>,
    pub(crate) js_runtime: JsRuntimeHandle,
    pub(crate) documents: Rc<RefCell<DocumentStore>>,
    pub(crate) registry: Rc<RefCell<RealmRegistry>>,
    pub(crate) pending_windows: Weak<RefCell<Vec<(u64, crate::Engine)>>>,
    pub(crate) pending_storage: Rc<RefCell<Vec<crate::storage::PendingStorageEvent>>>,
    pub(crate) turn_deadline: Rc<Cell<Option<Instant>>>,
    pub(crate) event_seq: Rc<Cell<u64>>,
    pub(crate) worker_dir: Arc<WorkerDirectory>,
    pub(crate) font_ctx: parley::FontContext,
}

impl Deref for FrameRuntime {
    type Target = SharedRuntime;

    fn deref(&self) -> &Self::Target {
        &self.common
    }
}

impl FrameRuntime {
    pub(crate) fn open_turn(&self, deadline: Instant) -> super::ScriptTurn {
        super::ScriptTurn::enter_at(&self.turn_deadline, deadline)
    }

    pub(crate) fn next_worker_id(&self) -> WorkerId {
        WorkerId::new(self.next_worker.fetch_add(1, Ordering::Relaxed))
    }

    pub(crate) fn next_broadcast_id(&self) -> u64 {
        self.next_broadcast.fetch_add(1, Ordering::Relaxed)
    }

    pub(crate) fn next_blob_scope(&self) -> u64 {
        self.next_blob_scope.fetch_add(1, Ordering::Relaxed)
    }
}

pub struct Stop {
    flag: AtomicBool,
}

impl Default for Stop {
    fn default() -> Self {
        Self::new()
    }
}

impl Stop {
    #[must_use]
    pub fn new() -> Self {
        Self {
            flag: AtomicBool::new(false),
        }
    }

    pub fn request(&self) {
        self.flag.store(true, Ordering::Relaxed);
    }

    #[must_use]
    pub fn is_set(&self) -> bool {
        self.flag.load(Ordering::Relaxed)
    }
}
