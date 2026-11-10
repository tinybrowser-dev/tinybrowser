//! Process-wide handles shared by every frame document.
//!
//! [`FrameRuntime`] is the renderer's shared runtime: browser services, the JS
//! heap, the wake handle, the stop flag, and the cross-frame stores. It lives
//! beside the realm bookkeeping it carries so that `document` and `engine`
//! read it from below rather than defining it.

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

/// The queue worker threads and realms use to hand lifecycle, message, and
/// timer events to the engine.
pub(crate) type WorkerRequests = Arc<Mutex<Vec<WorkerRequest>>>;

/// The Blob URL Store, shared by every realm in the renderer process.
///
/// A `blob:` URL created by a document must be fetchable from a worker thread,
/// and a worker's own URL must be readable by the document that created it
/// (<https://w3c.github.io/FileAPI/#blob-url-store>). The store therefore lives
/// in process-wide state rather than in a thread's [`RealmRegistry`].
pub(crate) type ObjectUrlStore = Arc<Mutex<HashMap<String, ObjectUrlEntry>>>;

/// The process-wide registry of `SharedArrayBuffer` blocks in transit between
/// realms.
///
/// A cloned `SharedArrayBuffer` names the same block, not a copy
/// (<https://html.spec.whatwg.org/multipage/structured-data.html#structuredserializeinternal>).
/// The sender registers its block and names the entry in the serialized
/// message; the receiver wraps the same bytes in its own runtime. The lease
/// keeps one reference, so the block outlives the side that created it.
///
/// Entries live for the engine's lifetime: one per distinct block, keyed by
/// the block's address. A bounded strategy would need to know when the last
/// message naming an entry has been delivered, which a broadcast does not
/// expose.
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
    /// Registers `lease`'s block and returns the id a serialized message names.
    ///
    /// Two views of one buffer, or two posts of it, register the same block
    /// once.
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

    /// Takes one reference to the block a message named.
    pub(crate) fn get(&self, id: u64) -> Option<rquickjs::SharedBufferLease> {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .buffers
            .get(&id)
            .cloned()
    }
}

/// The process-wide handles every frame document shares: services, the JS
/// heap, the wake handle, the stop flag, and the cross-frame stores.
#[derive(Clone)]
pub(crate) struct FrameRuntime {
    pub(crate) services: Arc<dyn BrowserServices>,
    /// The browser identity this process was spawned with: the same value the
    /// network uses for `User-Agent` and the client hints, so a page, a CSP
    /// report, and a request cannot name different products
    /// (<https://wicg.github.io/ua-client-hints/#http-ua-hints>).
    pub(crate) identity: net::ValidIdentity,
    pub(crate) js_runtime: JsRuntimeHandle,
    pub(crate) wake: Arc<Notify>,
    pub(crate) stop: Arc<Stop>,
    pub(crate) documents: Rc<RefCell<DocumentStore>>,
    pub(crate) registry: Rc<RefCell<RealmRegistry>>,
    pub(crate) shared: SharedHandle,
    /// Storage changes waiting for the `storage`-event task in their receiving
    /// frames.
    pub(crate) pending_storage: Rc<RefCell<Vec<crate::storage::PendingStorageEvent>>>,
    /// Deadline for the current renderer turn, shared by every realm. A drain
    /// sets it once; serial tasks clamp to it instead of each taking a fresh
    /// budget (`ScriptTurn`).
    pub(crate) turn_deadline: Rc<Cell<Option<Instant>>>,
    /// Worker lifecycle and message requests a realm or a worker thread queued
    /// for the engine. A worker thread is another OS thread, so the queue is a
    /// mutex rather than a `RefCell`.
    pub(crate) worker_requests: WorkerRequests,
    /// Worker ids are minted by the realm that constructs one, so the
    /// constructor can name its worker before the engine materializes it.
    pub(crate) next_worker: Arc<AtomicU64>,
    /// `BroadcastChannel` ids are minted here, not per realm: a message names
    /// its sender's channel so the same channel does not receive its own post,
    /// and that comparison spans realms.
    pub(crate) next_broadcast: Arc<AtomicU64>,
    /// The dedicated workers of this page engine, shared with every worker
    /// thread so a nested `new Worker()` can spawn another thread and a realm
    /// can post to a worker without a trip through the engine.
    pub(crate) worker_dir: Arc<WorkerDirectory>,
    /// The Blob URL Store, shared with every worker thread.
    pub(crate) object_urls: ObjectUrlStore,
    /// `SharedArrayBuffer` blocks a message may name, shared with every worker
    /// thread.
    pub(crate) shared_buffers: Arc<SharedBufferTable>,
    /// Parley's font context, built once per process: construction scans
    /// system fonts (~25ms), so every `BaseDocument` clones this instead of
    /// rescanning. Cloning is cheap; Blitz shares the source cache.
    pub(crate) font_ctx: parley::FontContext,
}

impl FrameRuntime {
    /// Opens one turn on the shared deadline. Nested enters join it; dropping
    /// the owner clears the cell.
    pub(crate) fn open_turn(&self, deadline: Instant) -> super::ScriptTurn {
        super::ScriptTurn::enter_at(&self.turn_deadline, deadline)
    }

    fn requests(&self) -> MutexGuard<'_, Vec<WorkerRequest>> {
        self.worker_requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Queues one worker event for the engine.
    pub(crate) fn push_worker_request(&self, request: WorkerRequest) {
        self.requests().push(request);
    }

    /// Queues one worker event and wakes a host that is waiting on [`Self::wake`].
    pub(crate) fn push_worker_request_waking(&self, request: WorkerRequest) {
        self.requests().push(request);
        self.wake.notify_one();
    }

    /// Takes every queued worker event.
    pub(crate) fn take_worker_requests(&self) -> Vec<WorkerRequest> {
        std::mem::take(&mut *self.requests())
    }

    /// Whether any worker event is waiting for the engine.
    pub(crate) fn has_worker_requests(&self) -> bool {
        !self.requests().is_empty()
    }

    /// Mints a worker id.
    pub(crate) fn next_worker_id(&self) -> WorkerId {
        WorkerId::new(self.next_worker.fetch_add(1, Ordering::Relaxed))
    }

    /// Mints a `BroadcastChannel` id.
    pub(crate) fn next_broadcast_id(&self) -> u64 {
        self.next_broadcast.fetch_add(1, Ordering::Relaxed)
    }

    fn object_urls(&self) -> MutexGuard<'_, HashMap<String, ObjectUrlEntry>> {
        self.object_urls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// The contents of a `blob:` URL this origin created.
    pub(crate) fn object_url_contents(&self, url: &str, origin: &url::Origin) -> Option<Arc<str>> {
        let store = self.object_urls();
        let entry = store.get(object_url_key(url))?;
        (entry.origin == *origin).then(|| Arc::clone(&entry.contents))
    }

    /// The `Content-Type` a `blob:` URL was created from.
    pub(crate) fn object_url_type(&self, url: &str, origin: &url::Origin) -> Option<Arc<str>> {
        let store = self.object_urls();
        let entry = store.get(object_url_key(url))?;
        (entry.origin == *origin).then(|| Arc::clone(&entry.content_type))
    }

    /// The creator origin of a `blob:` URL this origin may use.
    pub(crate) fn object_url_origin(&self, url: &str, origin: &url::Origin) -> Option<url::Origin> {
        let store = self.object_urls();
        let entry = store.get(object_url_key(url))?;
        (entry.origin == *origin).then(|| entry.origin.clone())
    }

    /// The stored origin of a `blob:` URL, without the origin check. Used to
    /// decide whether an opaque-origin document may read the URL.
    pub(crate) fn stored_object_url_origin(&self, url: &str) -> Option<url::Origin> {
        self.object_urls()
            .get(object_url_key(url))
            .map(|entry| entry.origin.clone())
    }

    pub(crate) fn insert_object_url(&self, url: String, entry: ObjectUrlEntry) {
        self.object_urls().insert(url, entry);
    }

    /// File API unloading cleanup: drops matching blob URLs and returns the
    /// bytes they held
    /// (<https://w3c.github.io/FileAPI/#lifeTime>).
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

    /// Removes one blob URL, returning the bytes it held.
    pub(crate) fn remove_object_url(&self, url: &str) -> Option<usize> {
        let mut store = self.object_urls();
        store
            .remove(object_url_key(url))
            .map(|entry| entry.contents.len() + entry.content_type.len())
    }
}

/// Stop flag shared by a renderer loop, the JS interrupt handler, and dials.
///
/// Requesting a stop interrupts running script and marks the flag; every
/// engine sharing it winds down at its next turn.
pub struct Stop {
    flag: AtomicBool,
}

impl Default for Stop {
    fn default() -> Self {
        Self::new()
    }
}

impl Stop {
    /// A fresh, unset stop flag.
    #[must_use]
    pub fn new() -> Self {
        Self {
            flag: AtomicBool::new(false),
        }
    }

    /// Requests a stop: interrupts `QuickJS` and marks the flag.
    pub fn request(&self) {
        self.flag.store(true, Ordering::Relaxed);
    }

    /// Whether [`Stop::request`] has run.
    #[must_use]
    pub fn is_set(&self) -> bool {
        self.flag.load(Ordering::Relaxed)
    }
}
