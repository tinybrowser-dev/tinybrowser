use std::cell::{Cell, RefCell};
use std::collections::HashMap;
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
pub(crate) struct FrameRuntime {
    pub(crate) services: Arc<dyn BrowserServices>,
    // https://wicg.github.io/ua-client-hints/#http-ua-hints
    pub(crate) identity: Rc<RefCell<net::ValidIdentity>>,
    pub(crate) js_runtime: JsRuntimeHandle,
    pub(crate) wake: Arc<Notify>,
    pub(crate) stop: Arc<Stop>,
    pub(crate) documents: Rc<RefCell<DocumentStore>>,
    pub(crate) registry: Rc<RefCell<RealmRegistry>>,
    pub(crate) shared: SharedHandle,
    pub(crate) pending_storage: Rc<RefCell<Vec<crate::storage::PendingStorageEvent>>>,
    pub(crate) turn_deadline: Rc<Cell<Option<Instant>>>,
    pub(crate) worker_requests: WorkerRequests,
    pub(crate) next_worker: Arc<AtomicU64>,
    pub(crate) next_broadcast: Arc<AtomicU64>,
    pub(crate) worker_dir: Arc<WorkerDirectory>,
    pub(crate) object_urls: ObjectUrlStore,
    pub(crate) shared_buffers: Arc<SharedBufferTable>,
    pub(crate) font_ctx: parley::FontContext,
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

    pub(crate) fn push_worker_request(&self, request: WorkerRequest) {
        self.requests().push(request);
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
