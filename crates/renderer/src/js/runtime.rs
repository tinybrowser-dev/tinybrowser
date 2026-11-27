use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ops::Deref;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use tokio::sync::Notify;

use crate::documents::DocumentStore;
use crate::js::world::{ObjectUrlEntry, object_url_key};
use crate::messaging::SharedHandle;
use crate::protocol::BrowserServices;
use crate::worker::{WorkerDirectory, WorkerId, WorkerRequest};

use super::JsRuntimeHandle;
use super::RealmRegistry;

pub(crate) type WorkerRequests = Arc<Mutex<Vec<WorkerRequest>>>;

// https://w3c.github.io/FileAPI/#blob-url-store
pub(crate) type ObjectUrlStore = Arc<Mutex<HashMap<String, ObjectUrlEntry>>>;

// https://html.spec.whatwg.org/multipage/structured-data.html#structuredserializeinternal
#[derive(Default)]
pub(crate) struct SharedBufferTable {
    entries: Mutex<SharedBufferEntries>,
}

#[derive(Default)]
struct SharedBufferEntries {
    next: u64,
    by_address: HashMap<usize, u64>,
    buffers: HashMap<u64, rquickjs::SharedBufferLease>,
}

impl SharedBufferTable {
    pub(crate) fn register(&self, lease: rquickjs::SharedBufferLease) -> u64 {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        let address = lease.address();
        if let Some(id) = entries.by_address.get(&address) {
            return *id;
        }
        let id = entries.next;
        entries.next = entries.next.saturating_add(1);
        entries.by_address.insert(address, id);
        entries.buffers.insert(id, lease);
        id
    }

    pub(crate) fn get(&self, id: u64) -> Option<rquickjs::SharedBufferLease> {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .buffers
            .get(&id)
            .cloned()
    }
}

#[derive(Clone)]
pub(crate) struct SharedRuntime {
    pub(crate) services: Arc<dyn BrowserServices>,
    pub(crate) clock: net::Clock,
    pub(crate) animation_epoch: Instant,
    // https://wicg.github.io/ua-client-hints/#http-ua-hints
    pub(crate) initial_identity: net::ValidIdentity,
    pub(crate) wake: Arc<Notify>,
    pub(crate) stop: Arc<Stop>,
    pub(crate) shared: SharedHandle,
    pub(crate) worker_requests: WorkerRequests,
    pub(crate) next_worker: Arc<AtomicU64>,
    pub(crate) next_broadcast: Arc<AtomicU64>,
    pub(crate) object_urls: ObjectUrlStore,
    pub(crate) shared_buffers: Arc<SharedBufferTable>,
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
        Self {
            services,
            clock,
            animation_epoch,
            initial_identity,
            wake,
            stop,
            shared: SharedHandle::new(crate::messaging::Shared::default()),
            worker_requests: Arc::new(Mutex::new(Vec::new())),
            next_worker: Arc::new(AtomicU64::new(1)),
            next_broadcast: Arc::new(AtomicU64::new(1)),
            object_urls: Arc::new(Mutex::new(HashMap::new())),
            shared_buffers: Arc::new(SharedBufferTable::default()),
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
}

#[derive(Clone)]
pub(crate) struct FrameRuntime {
    pub(crate) common: Arc<SharedRuntime>,
    // https://wicg.github.io/ua-client-hints/#http-ua-hints
    pub(crate) identity: Rc<RefCell<net::ValidIdentity>>,
    pub(crate) js_runtime: JsRuntimeHandle,
    pub(crate) documents: Rc<RefCell<DocumentStore>>,
    pub(crate) registry: Rc<RefCell<RealmRegistry>>,
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

    fn requests(&self) -> MutexGuard<'_, Vec<WorkerRequest>> {
        self.worker_requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn push_worker_request_waking(&self, request: WorkerRequest) {
        self.requests().push(request);
        self.wake.notify_one();
    }

    pub(crate) fn take_worker_requests(&self) -> Vec<WorkerRequest> {
        std::mem::take(&mut *self.requests())
    }

    pub(crate) fn has_worker_requests(&self) -> bool {
        !self.requests().is_empty()
    }

    pub(crate) fn next_worker_id(&self) -> WorkerId {
        WorkerId::new(self.next_worker.fetch_add(1, Ordering::Relaxed))
    }

    pub(crate) fn next_broadcast_id(&self) -> u64 {
        self.next_broadcast.fetch_add(1, Ordering::Relaxed)
    }

    fn object_urls(&self) -> MutexGuard<'_, HashMap<String, ObjectUrlEntry>> {
        self.object_urls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn object_url_contents(&self, url: &str, origin: &url::Origin) -> Option<Arc<str>> {
        let store = self.object_urls();
        let entry = store.get(object_url_key(url))?;
        (entry.origin == *origin).then(|| Arc::clone(&entry.contents))
    }

    pub(crate) fn object_url_type(&self, url: &str, origin: &url::Origin) -> Option<Arc<str>> {
        let store = self.object_urls();
        let entry = store.get(object_url_key(url))?;
        (entry.origin == *origin).then(|| Arc::clone(&entry.content_type))
    }

    pub(crate) fn object_url_origin(&self, url: &str, origin: &url::Origin) -> Option<url::Origin> {
        let store = self.object_urls();
        let entry = store.get(object_url_key(url))?;
        (entry.origin == *origin).then(|| entry.origin.clone())
    }

    pub(crate) fn stored_object_url_origin(&self, url: &str) -> Option<url::Origin> {
        self.object_urls()
            .get(object_url_key(url))
            .map(|entry| entry.origin.clone())
    }

    pub(crate) fn insert_object_url(&self, url: String, entry: ObjectUrlEntry) {
        self.object_urls().insert(url, entry);
    }

    // https://w3c.github.io/FileAPI/#lifeTime
    pub(crate) fn take_object_urls(
        &self,
        mut unload: impl FnMut(&ObjectUrlEntry) -> bool,
    ) -> usize {
        let mut store = self.object_urls();
        let mut released = 0usize;
        store.retain(|_, entry| {
            if unload(entry) {
                released = released.saturating_add(entry.contents.len() + entry.content_type.len());
                false
            } else {
                true
            }
        });
        released
    }

    pub(crate) fn remove_object_url(&self, url: &str) -> Option<usize> {
        let mut store = self.object_urls();
        store
            .remove(object_url_key(url))
            .map(|entry| entry.contents.len() + entry.content_type.len())
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
