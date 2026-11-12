//! The page engine for one renderer process: the shared `QuickJS` heap, the Tokio
//! waiter, and the frame registry.
//!
//! One `QuickJS` `Runtime` and one Tokio waiter per renderer process;
//! `Document` is one frame. Child frames join the same engine sharing both,
//! while each keeps its own realm: JS values never cross realms, so the
//! engine routes serialized payloads and owns the channel endpoints
//! ([`crate::messaging`]).

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::Notify;
use tokio::time::Instant;
use url::Url;

use crate::RemoteValue;
use crate::document::{CreatedFrame, Document, LocalDocumentState, WindowMessage, about_blank};
use crate::js::FrameRuntime;
use crate::js::Stop;
use crate::js::world::same_document_url;

/// The engine's default frame viewport in device pixels, used until CDP
/// emulation (or a mount) sets another size.
pub const DEFAULT_VIEWPORT: (u32, u32) = (800, 600);

use crate::documents::DocumentStore;
use crate::js::{
    FrameNavigation, NavigationReferrer, NavigationTarget, PolicyContainer, RealmRegistry,
    SharedJsRuntime,
};
use crate::messaging::{Delivery, SharedHandle};
use crate::protocol::{
    BrowserEvent, BrowserServices, FrameId, Mount, RendererEvent, ResponseHead, TabError,
};
use crate::storage::PendingStorageEvent;
use crate::worker::{
    CreatedWorker, SharedWorkerKey, SharedWorkerOptions, Worker, WorkerCommand, WorkerDirectory,
    WorkerError, WorkerHandle, WorkerId, WorkerRequest,
};

/// One live shared worker realm, plus the `SharedWorker` objects connected to
/// it.
struct SharedRealm {
    /// The worker in `Engine::workers` that hosts this realm.
    id: WorkerId,
    /// The options every connection must repeat; a connection with other
    /// options never joins this realm
    /// (<https://html.spec.whatwg.org/multipage/workers.html#shared-worker-creation-steps>,
    /// steps 5 and 6).
    options: SharedWorkerOptions,
    /// Every `SharedWorker` object attached to this realm, with the frame that
    /// owns it. A load failure fires a plain `error` event at each one
    /// (<https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model>,
    /// step 14).
    owners: Vec<(FrameId, WorkerId)>,
}

/// One child navigable's replay store, owned by the engine that hosts its
/// documents.
///
/// The browser owns the joint entry list and its steps. This store keeps only
/// what cannot cross the wire: the request or local document snapshot needed
/// to install an entry again. It is keyed by the frame's local position, which
/// the browser sends with every traversal
/// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#session-history-entry>).
#[derive(Default)]
struct FrameHistory {
    entries: Vec<FrameHistoryEntry>,
    /// The frame's active position, mirroring the browser's latest snapshot.
    index: usize,
    /// A started navigation whose document has not committed.
    ///
    /// Its payload stays out of `entries` until the commit, so a load that
    /// fails or is superseded leaves no phantom entry behind
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#finalize-a-cross-document-navigation>).
    pending: Option<PendingEntry>,
}

/// One uncommitted navigation, held until its document exists.
struct PendingEntry {
    entry: FrameHistoryEntry,
    /// The local position the navigation replaces, or `None` when it appends.
    replace: Option<usize>,
}

#[derive(Clone, Default)]
struct FrameHistoryEntry {
    /// The navigation that produced the entry, replayed when its document has
    /// no local snapshot (a network or `data:` load).
    navigation: Option<FrameNavigation>,
    /// The saved state of an `about:blank` or `about:srcdoc` document.
    state: Option<LocalDocumentState>,
    /// The URL and classic state of a same-document entry. The document stays
    /// active across the step, so restoring the entry only swaps the URL and
    /// fires `popstate`
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#url-and-history-update-steps>).
    same_document: Option<SameDocumentEntry>,
}

/// One `pushState()`/`replaceState()` entry.
#[derive(Clone)]
struct SameDocumentEntry {
    url: String,
    /// The serialized classic history state the realm encoded.
    state: String,
}

impl FrameHistory {
    /// Adds an entry after the active one. The browser has already decided the
    /// joint position and cleared the forward branch across every frame.
    fn push(&mut self, entry: FrameHistoryEntry) {
        self.entries.truncate(self.index.saturating_add(1));
        self.entries.push(entry);
        self.index = self.entries.len().saturating_sub(1);
    }

    /// Replaces the active entry, which is what a navigation whose target
    /// document is the initial `about:blank` does
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#navigate>).
    fn replace(&mut self, entry: FrameHistoryEntry) {
        if let Some(slot) = self.entries.get_mut(self.index) {
            *slot = entry;
        } else {
            self.push(entry);
        }
    }

    /// Stages a navigation that has not committed.
    fn stage(&mut self, entry: FrameHistoryEntry, replace: bool) {
        self.pending = Some(PendingEntry {
            entry,
            replace: replace.then_some(self.index),
        });
    }

    /// Turns the staged navigation into a real entry after its commit.
    fn commit_pending(&mut self) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        match pending.replace {
            Some(index) => {
                if let Some(slot) = self.entries.get_mut(index) {
                    *slot = pending.entry;
                } else {
                    self.push(pending.entry);
                }
            }
            None => self.push(pending.entry),
        }
    }

    /// Drops a staged navigation whose load failed, was superseded, or was
    /// traversed away from.
    fn discard_pending(&mut self) {
        self.pending = None;
    }

    /// Inserts a same-document entry so later local positions line up with the
    /// browser's joint list. Its document stays active, so it has no replay
    /// payload of its own.
    fn push_same_document(&mut self, entry: SameDocumentEntry) {
        self.push(FrameHistoryEntry {
            same_document: Some(entry),
            ..FrameHistoryEntry::default()
        });
    }

    /// Replaces the active entry's same-document URL and state, keeping its
    /// local position and document.
    fn replace_same_document(&mut self, entry: SameDocumentEntry) {
        if let Some(slot) = self.entries.get_mut(self.index) {
            slot.same_document = Some(entry);
        }
    }

    fn at(&self, index: usize) -> Option<&FrameHistoryEntry> {
        self.entries.get(index)
    }
}

/// The joint session history a standalone carrier owns.
///
/// A browser-hosted engine's tab actor owns the authoritative list. The
/// embedded and WPT carriers have no tab actor, so the engine keeps the used
/// steps itself: one step records the local entry each live frame presents at
/// that step, and a traversal moves every frame whose entry changes
/// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#joint-session-history>,
/// <https://html.spec.whatwg.org/multipage/browsing-the-web.html#traverse-the-history-by-a-delta>).
#[derive(Default)]
struct StandaloneHistory {
    /// One map per used step. A step counts every frame that existed when it
    /// was the active one; a frame created later has no entry in it and
    /// presents its first local entry there, position 0.
    steps: Vec<BTreeMap<FrameId, usize>>,
    /// Position of the active step in `steps`; a delta counts steps, so two
    /// frame entries that share a step are one step
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#getting-the-history-object-length-and-index>).
    index: usize,
}

impl StandaloneHistory {
    /// A fresh page: one step, with no frame entries yet.
    fn seed() -> Self {
        Self {
            steps: vec![BTreeMap::new()],
            index: 0,
        }
    }

    /// Adds a frame to the active step. Its initial document shares the step
    /// that was current when the frame appeared
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#navigate>).
    fn join(&mut self, frame: FrameId, local_index: usize) {
        self.steps[self.index].insert(frame, local_index);
    }

    /// The entries every frame presents at the active step.
    fn active(&self) -> &BTreeMap<FrameId, usize> {
        &self.steps[self.index]
    }

    /// Adds a step after the active one, which clears the forward branch of
    /// every frame
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#clear-the-forward-session-history>).
    fn push(&mut self, frame: FrameId, local_index: usize) {
        self.steps.truncate(self.index.saturating_add(1));
        let mut step = self.steps[self.index].clone();
        step.insert(frame, local_index);
        self.steps.push(step);
        self.index = self.steps.len().saturating_sub(1);
    }

    /// Replaces a frame's entry in the active step without adding one.
    fn replace(&mut self, frame: FrameId, local_index: usize) {
        self.steps[self.index].insert(frame, local_index);
    }

    /// Drops a frame's entries from every step. A step stays used while any
    /// other frame presents an entry at it, and the main frame presents one at
    /// every step the tab has taken
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#getting-the-used-step>).
    fn leave(&mut self, frame: FrameId) {
        for step in &mut self.steps {
            step.remove(&frame);
        }
    }

    /// The step `delta` counts away from the active one, or `None` when the
    /// step is off either end of the list.
    fn target(&self, delta: i32) -> Option<usize> {
        self.index
            .checked_add_signed(delta as isize)
            .filter(|target| *target < self.steps.len())
    }

    /// The local entry `frame` presents at `step`. A frame created after that
    /// step presents its first entry
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#traverse-the-history-by-a-delta>).
    fn local_at(&self, step: usize, frame: FrameId) -> usize {
        self.steps[step].get(&frame).copied().unwrap_or(0)
    }
}

/// Everything one pass over the documents produced outside their task queues.
#[derive(Default)]
struct PendingWork {
    removed_containers: Vec<crate::dom::tree::NodeId>,
    navigations: Vec<(FrameId, FrameNavigation)>,
    traversals: Vec<(FrameId, i32)>,
    history_updates: Vec<(FrameId, crate::document::HistoryUpdate)>,
    streams: Vec<(FrameId, bool)>,
    /// Whether a realm started on a script another realm inserted.
    ran_scripts: bool,
}

/// One renderer process's page engine.
///
/// The main frame is the tab's top-level document; child frames share the
/// engine's heap and wake handle, so same-site frames can pass JavaScript
/// objects synchronously.
pub struct Engine {
    /// The handles every frame document shares.
    runtime: FrameRuntime,
    frames: BTreeMap<FrameId, Document>,
    js_runtime: SharedJsRuntime,
    /// Page-scoped init scripts shared by every frame's world, with their
    /// identifiers (<https://chromedevtools.github.io/devtools-protocol/tot/Page/#method-addScriptToEvaluateOnNewDocument>).
    init_scripts: Rc<RefCell<Vec<(u64, String)>>>,
    /// Replay store of every child navigable and auxiliary context this
    /// engine hosts.
    frame_history: HashMap<FrameId, FrameHistory>,
    /// Joint-history events the engine produces outside a document's own task
    /// queue: frame creation and removal, and child navigation starts. They
    /// are drained with the documents' browser events.
    pending_events: Vec<(FrameId, BrowserEvent)>,
    /// Traversals a child frame queued during the last drain. They follow the
    /// same drain's `HistoryUpdated` events, because a script that calls
    /// `pushState()` and then `back()` must see the push first
    /// (<https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-history-back>).
    deferred_events: Vec<(FrameId, BrowserEvent)>,
    /// The joint session history a standalone carrier owns; `None` when a
    /// browser tab owns it. With no browser, a child traversal resolves
    /// against this list instead of becoming a browser event
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#joint-session-history>).
    joint_history: Option<StandaloneHistory>,
    /// JS-truth predicates the browser waits on, evaluated after each drain.
    js_waiters: Vec<JsWaiter>,
    /// Shared worker realms and any worker realm that runs in this engine.
    /// A dedicated worker runs on its own OS thread and lives in
    /// `runtime.worker_dir`; this map holds the shared realms, which the page
    /// engine hosts
    /// (<https://html.spec.whatwg.org/multipage/workers.html#shared-worker-creation-steps>).
    workers: BTreeMap<WorkerId, Worker>,
    /// Shared worker realms keyed by `(origin, url, name)`. The realm itself
    /// lives in `workers`; this map records the options its connections agreed
    /// on and the `SharedWorker` objects to notify
    /// (<https://html.spec.whatwg.org/multipage/workers.html#shared-worker-creation-steps>).
    shared_workers: HashMap<SharedWorkerKey, SharedRealm>,
}

/// One JS-truth predicate: the browser registers it once and hears only when
/// it evaluates to `true`, so it never polls the page.
struct JsWaiter {
    id: u64,
    frame: FrameId,
    source: crate::protocol::ScriptSource,
}

/// What [`Engine::navigate_frame`] did with a navigation, which decides whether
/// the caller owes the frame's replay store a history entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NavOutcome {
    /// A new document load is in flight.
    Loaded,
    /// The frame kept the document it had: no usable URL, or a blank `src`.
    KeptDocument,
    /// A same-document fragment navigation ran. The frame's own realm records
    /// the history entry and delivers `hashchange`, so the caller must not
    /// record a second one.
    Fragment,
}

impl Engine {
    /// Builds an engine whose effects go through `services`.
    ///
    /// `stop` cancels every frame at once; `wake` is notified when a dial
    /// completion or stop arrives, so a carrier can wait instead of polling.
    /// `identity` is the browser's, and every page this engine hosts reports
    /// it verbatim.
    ///
    /// This builds a private font database. A host that creates many engines
    /// on one thread should build [`crate::Fonts`] once and call
    /// [`Self::with_fonts`] instead.
    #[must_use]
    pub fn new(
        services: Arc<dyn BrowserServices>,
        stop: Arc<Stop>,
        wake: Arc<Notify>,
        identity: net::ValidIdentity,
    ) -> Self {
        Self::with_fonts(services, stop, wake, identity, &crate::Fonts::default())
    }

    /// Builds an engine that shares `fonts` with any other engine handed the
    /// same database.
    #[must_use]
    pub fn with_fonts(
        services: Arc<dyn BrowserServices>,
        stop: Arc<Stop>,
        wake: Arc<Notify>,
        identity: net::ValidIdentity,
        fonts: &crate::Fonts,
    ) -> Self {
        let js_runtime = SharedJsRuntime::default();
        let init_scripts: Rc<RefCell<Vec<(u64, String)>>> = Rc::new(RefCell::new(Vec::new()));
        let shared = crate::messaging::SharedHandle::new(crate::messaging::Shared::default());
        let worker_requests = Arc::new(Mutex::new(Vec::new()));
        let next_worker = Arc::new(AtomicU64::new(1));
        let next_broadcast = Arc::new(AtomicU64::new(1));
        let object_urls = Arc::new(Mutex::new(std::collections::HashMap::new()));
        let shared_buffers = Arc::new(crate::js::SharedBufferTable::default());
        let worker_dir = Arc::new(WorkerDirectory::new(
            Arc::clone(&services),
            identity.clone(),
            Arc::clone(&wake),
            Arc::clone(&stop),
            shared.clone(),
            Arc::clone(&worker_requests),
            Arc::clone(&next_worker),
            Arc::clone(&next_broadcast),
            fonts.0.clone(),
            Arc::clone(&object_urls),
            Arc::clone(&shared_buffers),
        ));
        let runtime = FrameRuntime {
            services,
            identity,
            js_runtime: js_runtime.handle(),
            wake,
            stop,
            documents: Rc::new(RefCell::new(DocumentStore::default())),
            registry: Rc::new(RefCell::new(RealmRegistry::default())),
            shared,
            pending_storage: Rc::new(RefCell::new(Vec::new())),
            turn_deadline: Rc::new(std::cell::Cell::new(None)),
            worker_requests,
            next_worker,
            next_broadcast,
            worker_dir,
            object_urls,
            shared_buffers,
            font_ctx: fonts.0.clone(),
        };
        let main = Document::with_shared(FrameId::MAIN, &runtime, &init_scripts);
        let mut frames = BTreeMap::new();
        frames.insert(FrameId::MAIN, main);
        Self {
            runtime,
            frames,
            js_runtime,
            init_scripts,
            frame_history: HashMap::new(),
            pending_events: Vec::new(),
            deferred_events: Vec::new(),
            joint_history: None,
            js_waiters: Vec::new(),
            workers: BTreeMap::new(),
            shared_workers: HashMap::new(),
        }
    }

    /// Marks this engine as having no browser tab behind it, so it owns the
    /// joint session history every frame's `history` object mirrors.
    pub fn use_standalone_history(&mut self) {
        self.joint_history = Some(StandaloneHistory::seed());
    }

    /// Applies the worker events and timer requests a realm or a worker thread
    /// queued while a script was running.
    fn adopt_worker_requests(&mut self) -> bool {
        let requests = self.runtime.take_worker_requests();
        let mut applied = false;
        for request in requests {
            applied = true;
            match request {
                WorkerRequest::CreateShared { created, endpoint } => {
                    self.adopt_shared_create(&created, endpoint);
                }
                WorkerRequest::BroadcastPost {
                    origin,
                    name,
                    payload,
                    channel,
                } => {
                    self.broadcast_local(&origin, &name, &payload, channel);
                }
                WorkerRequest::SetTimer {
                    worker,
                    js_id,
                    delay,
                } => {
                    self.runtime.worker_dir.with_workers(|workers| {
                        if let Some(handle) = workers.get_mut(&worker) {
                            handle.schedule_timer(delay, js_id);
                        }
                    });
                }
                WorkerRequest::ClearTimer { worker, js_id } => {
                    self.runtime.worker_dir.with_workers(|workers| {
                        if let Some(handle) = workers.get_mut(&worker) {
                            handle.clear_timer(js_id);
                        }
                    });
                }
                WorkerRequest::Close { id } => {
                    if let Some(worker) = self.workers.get_mut(&id) {
                        worker.close();
                    }
                }
                WorkerRequest::Error(error) => {
                    self.push_worker_error(*error);
                }
                WorkerRequest::LoadFailed { worker, parent } => {
                    self.push_worker_load_failed(parent, worker);
                }
                WorkerRequest::SharedLoadFailed { worker } => {
                    self.fail_shared_realm(worker);
                }
                WorkerRequest::CspViolation { parent, violations } => {
                    self.push_csp_violations(parent, violations);
                }
                WorkerRequest::PostToParent {
                    parent,
                    worker,
                    payload,
                    ports,
                } => {
                    self.push_worker_message(parent, worker, payload, ports);
                }
            }
        }
        self.prune_shared_workers();
        applied
    }

    /// Attaches one `SharedWorker` connection to its realm, creating the realm
    /// for the first connection.
    ///
    /// The realm is keyed by the creator's storage key, script URL, and name.
    /// A connection that repeats the key but changes the options never joins:
    /// its `SharedWorker` object fires a plain `error` event instead
    /// (<https://html.spec.whatwg.org/multipage/workers.html#dom-sharedworker>).
    fn adopt_shared_create(&mut self, created: &CreatedWorker, endpoint: u64) {
        let key = SharedWorkerKey::new(
            created.origin.clone(),
            created.partition_key.clone(),
            created.url.clone(),
            created.name.clone(),
            created.same_site_cookies,
        );
        let options = SharedWorkerOptions {
            module: created.module,
            credentials: created.credentials,
            extended_lifetime: created.extended_lifetime,
            same_site_cookies: created.same_site_cookies,
        };
        let existing = self
            .shared_workers
            .get(&key)
            .map(|realm| (realm.id, realm.options));
        match existing {
            // A live realm with the same options takes the connection. A
            // record whose realm is already gone is stale: the key selects the
            // next realm instead.
            Some((id, existing)) if self.workers.contains_key(&id) => {
                if existing != options {
                    // The realm exists with other options: the connection is
                    // refused before its port can carry a message.
                    self.runtime
                        .shared
                        .borrow_mut()
                        .forget_endpoints(&[endpoint]);
                    self.push_worker_load_failed(created.parent, created.id);
                    return;
                }
                if let Some(realm) = self.shared_workers.get_mut(&key) {
                    realm.owners.push((created.parent, created.id));
                }
                // The worker-side endpoint belongs to the shared realm's frame
                // before the `connect` event runs
                // (<https://html.spec.whatwg.org/multipage/web-messaging.html#transfer-receiving-steps>).
                Self::route_endpoints(&self.runtime.shared, &[endpoint], FrameId::worker(id.get()));
                if let Some(worker) = self.workers.get_mut(&id) {
                    worker.queue_connect(endpoint);
                }
            }
            Some(_) | None => {
                self.shared_workers.remove(&key);
                if let Ok(mut worker) = Worker::create(
                    created,
                    created.id,
                    &self.runtime,
                    &self.js_runtime,
                    Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                ) {
                    let id = worker.id();
                    Self::route_endpoints(
                        &self.runtime.shared,
                        &[endpoint],
                        FrameId::worker(id.get()),
                    );
                    worker.queue_connect(endpoint);
                    self.workers.insert(id, worker);
                    self.shared_workers.insert(
                        key,
                        SharedRealm {
                            id,
                            options,
                            owners: vec![(created.parent, created.id)],
                        },
                    );
                } else {
                    self.runtime
                        .shared
                        .borrow_mut()
                        .forget_endpoints(&[endpoint]);
                    self.push_worker_load_failed(created.parent, created.id);
                }
            }
        }
    }

    /// Fires the plain `error` event a shared worker's script failure raises at
    /// every `SharedWorker` object connected to it
    /// (<https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model>,
    /// step 14).
    fn fail_shared_realm(&mut self, id: WorkerId) {
        let Some(key) = self
            .shared_workers
            .iter()
            .find_map(|(key, realm)| (realm.id == id).then(|| key.clone()))
        else {
            return;
        };
        let Some(realm) = self.shared_workers.remove(&key) else {
            return;
        };
        for (owner, worker) in realm.owners {
            self.push_worker_load_failed(owner, worker);
        }
    }

    /// Drops shared realm records whose realm is gone, disentangling the
    /// owner ports it held, and forgets owners whose frame no longer exists.
    fn prune_shared_workers(&mut self) {
        let dead: Vec<WorkerId> = self
            .shared_workers
            .values()
            .filter(|realm| !self.workers.contains_key(&realm.id))
            .map(|realm| realm.id)
            .collect();
        for id in dead {
            self.shared_workers.retain(|_, realm| realm.id != id);
            // Writing `close()` in a shared worker destroys the realm, and
            // every connected `MessagePort` is disentangled with it
            // (<https://html.spec.whatwg.org/multipage/workers.html#dom-sharedworkerglobalscope-close>).
            self.runtime
                .shared
                .borrow_mut()
                .close_frame_ports(FrameId::worker(id.get()));
        }
        let frames: Vec<FrameId> = self.frames.keys().copied().collect();
        for realm in self.shared_workers.values_mut() {
            realm
                .owners
                .retain(|(frame, _)| frames.contains(frame) || frame.as_worker().is_some());
        }
    }

    /// Queues one message on whichever worker owns `frame`: a shared realm runs
    /// in this engine, a dedicated worker on its own thread
    /// (<https://html.spec.whatwg.org/multipage/web-messaging.html#message-port-post-message-steps>).
    fn queue_worker_port_message(
        &mut self,
        frame: FrameId,
        endpoint: u64,
        payload: String,
        ports: Vec<u64>,
    ) -> bool {
        let Some(raw) = frame.as_worker() else {
            return false;
        };
        let id = WorkerId::new(raw);
        if let Some(worker) = self.workers.get_mut(&id) {
            worker.queue_port_message(endpoint, payload, ports);
            return true;
        }
        let sent = self.runtime.worker_dir.send(
            id,
            WorkerCommand::PortMessage {
                endpoint,
                payload,
                ports: ports.clone(),
            },
        );
        if !sent {
            self.runtime.shared.borrow_mut().forget_endpoints(&ports);
        }
        sent
    }

    /// Queues one `close` on whichever worker owns `frame`.
    fn queue_worker_port_closed(&mut self, frame: FrameId, endpoint: u64) -> bool {
        let Some(raw) = frame.as_worker() else {
            return false;
        };
        let id = WorkerId::new(raw);
        if let Some(worker) = self.workers.get_mut(&id) {
            worker.queue_port_closed(endpoint);
            return true;
        }
        self.runtime
            .worker_dir
            .send(id, WorkerCommand::PortClosed { endpoint })
    }

    /// Queues one message a nested worker posted at its owner's `Worker`
    /// object. The owner is a document or another worker global; both own the
    /// `Worker` object in their own realm
    /// (<https://html.spec.whatwg.org/multipage/workers.html#the-worker's-lifetime>).
    fn push_worker_message(
        &mut self,
        owner: FrameId,
        worker: WorkerId,
        payload: String,
        ports: Vec<u64>,
    ) {
        if self.frames.contains_key(&owner) {
            Self::route_endpoints(&self.runtime.shared, &ports, owner);
            if let Some(document) = self.frames.get_mut(&owner) {
                document.push_worker_message(worker, payload, ports);
            }
            return;
        }
        if let Some(raw) = owner.as_worker() {
            Self::route_endpoints(&self.runtime.shared, &ports, owner);
            let id = WorkerId::new(raw);
            if let Some(parent) = self.workers.get_mut(&id) {
                let _ = parent.deliver_message(worker, &payload, &ports);
                return;
            }
            if self.runtime.worker_dir.send(
                id,
                WorkerCommand::NestedMessage {
                    worker: worker.get(),
                    payload,
                    ports: ports.clone(),
                },
            ) {
                return;
            }
        }
        self.runtime.shared.borrow_mut().forget_endpoints(&ports);
    }

    /// Queues one error a worker reported at its owner's `Worker` object.
    fn push_worker_error(&mut self, error: WorkerError) {
        if let Some(document) = self.frames.get_mut(&error.parent) {
            document.push_worker_error(error);
            return;
        }
        let parent = error.parent;
        if let Some(raw) = parent.as_worker() {
            let id = WorkerId::new(raw);
            if let Some(worker) = self.workers.get_mut(&id) {
                let _ = worker.deliver_error(&error);
                return;
            }
            self.runtime
                .worker_dir
                .send(id, WorkerCommand::NestedError(Box::new(error)));
        }
    }

    /// Queues the plain `error` event a worker that never started fires at its
    /// owner's `Worker` object.
    /// Reports a worker's blocked-request violations at the document that
    /// owns the worker chain. A shared realm lives in this engine; a dedicated
    /// worker forwards through its command channel until a document hears it
    /// (<https://w3c.github.io/webappsec-csp/#report-violation>).
    fn push_csp_violations(&mut self, owner: FrameId, violations: Vec<crate::csp::Violation>) {
        if let Some(document) = self.frames.get_mut(&owner) {
            document.push_csp_violations(violations);
            return;
        }
        if let Some(raw) = owner.as_worker() {
            let id = WorkerId::new(raw);
            if let Some(parent) = self.workers.get_mut(&id) {
                parent.deliver_csp_violations(violations);
                return;
            }
            self.runtime
                .worker_dir
                .send(id, WorkerCommand::CspViolation { violations });
        }
    }

    fn push_worker_load_failed(&mut self, owner: FrameId, worker: WorkerId) {
        if let Some(document) = self.frames.get_mut(&owner) {
            document.push_worker_load_failed(worker);
            return;
        }
        if let Some(raw) = owner.as_worker() {
            let id = WorkerId::new(raw);
            if let Some(parent) = self.workers.get_mut(&id) {
                let _ = parent.deliver_load_failure(worker);
                return;
            }
            self.runtime.worker_dir.send(
                id,
                WorkerCommand::NestedLoadFailure {
                    worker: worker.get(),
                },
            );
        }
    }

    /// Adopts every child frame the realms created for themselves (an `iframe`
    /// insertion inside a script) and starts any `src` load it still needs.
    ///
    /// Returns whether any frame was adopted, so the reconcile loop runs again
    /// before the engine sleeps.
    fn adopt_pending_frames(&mut self) -> bool {
        // Frames are materialized in tree order with scripts. Scanning every
        // document here would start parser `src` loads too early.
        let mut adopted = false;
        let mut batch = Vec::new();
        loop {
            for (&parent, document) in &mut self.frames {
                for created in document.take_new_frames() {
                    batch.push((parent, created));
                }
            }
            if batch.is_empty() {
                return adopted;
            }
            adopted = true;
            for (parent, created) in batch.drain(..) {
                let (frame, container, document) = match created {
                    CreatedFrame::Child {
                        frame,
                        container,
                        document,
                    } => (frame, container, document),
                    CreatedFrame::Auxiliary { frame, document } => {
                        // Report before the auxiliary's realm can run: a
                        // `window.open` from it must find the browser's
                        // sandbox record
                        // (<https://html.spec.whatwg.org/multipage/browsers.html#popup-sandboxing-flag-set>).
                        self.runtime.services.frame_created(
                            frame,
                            parent,
                            crate::FrameKind::Auxiliary,
                            document.world().borrow().sandbox,
                        );
                        self.frames.insert(frame, document);
                        // The auxiliary document was created inside the opening
                        // script, so its realm could only start now.
                        if let Some(document) = self.frames.get_mut(&frame) {
                            document.ensure_js_ok();
                        }
                        self.seed_frame_history(frame);
                        continue;
                    }
                };
                // The container's `sandbox` attribute and the embedder's
                // active flags decide every document this frame loads
                // (<https://html.spec.whatwg.org/multipage/browsers.html#determining-the-creation-sandboxing-flags>).
                let sandbox = self
                    .frames
                    .get(&parent)
                    .map_or_else(crate::protocol::SandboxFlags::default, |document| {
                        crate::document::sandboxing_flags(&document.world(), container)
                    });
                // Report the frame before its first navigation can run a
                // script, so the browser owns the set any `window.open` call
                // is checked against.
                self.runtime.services.frame_created(
                    frame,
                    parent,
                    crate::FrameKind::Child,
                    sandbox,
                );
                self.frames.insert(frame, document);
                self.seed_frame_history(frame);
                // `srcdoc` takes priority over `src`
                // (<https://html.spec.whatwg.org/multipage/iframe-embed-object.html#attr-iframe-srcdoc>).
                let srcdoc = self
                    .frames
                    .get(&parent)
                    .and_then(|document| document.frame_srcdoc(container));
                if let Some(markup) = srcdoc {
                    let navigation =
                        FrameNavigation::srcdoc(NavigationTarget::Container(container), markup)
                            .with_sandbox(sandbox);
                    self.navigate_frame(frame, parent, &navigation);
                    self.record_navigation(frame, parent, &navigation, true);
                } else {
                    let src = self
                        .frames
                        .get(&parent)
                        .and_then(|document| document.frame_src(container));
                    if let Some(src) = src {
                        let navigation =
                            FrameNavigation::get(NavigationTarget::Container(container), src)
                                .with_referrer_policy(self.frames.get(&parent).map_or_default(
                                    |document| document.frame_referrer_policy(container),
                                ))
                                .with_sandbox(sandbox);
                        self.navigate_frame(frame, parent, &navigation);
                        self.record_navigation(frame, parent, &navigation, true);
                    }
                }
                if let Some(document) = self.frames.get_mut(&parent) {
                    document.mark_frame_load_pending(container);
                    document.flush_frame_proxy_sets(frame);
                }
                self.publish_frame_document(container, frame);
            }
        }
    }

    /// Starts a child or auxiliary frame's history with the document it was
    /// created on. The browser adds the matching joint entry
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#session-history-entry>).
    fn seed_frame_history(&mut self, frame: FrameId) {
        self.record_frame_history(frame, None, false, false, None);
    }

    /// Adds one started navigation to the frame's replay store and tells the
    /// browser to add its joint entry.
    ///
    /// A local-scheme document is saved whole, so traversing back to it
    /// restores its origin, policy container, and referrer instead of
    /// re-fetching; every other entry keeps the request to replay
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#session-history-entry>).
    fn record_navigation(
        &mut self,
        frame: FrameId,
        initiator: FrameId,
        navigation: &FrameNavigation,
        replace: bool,
    ) {
        // An `iframe` whose `src` is empty keeps the document it already has;
        // that is not a navigation and adds no entry.
        if navigation.srcdoc.is_none()
            && navigation.spec.is_empty()
            && matches!(&navigation.target, NavigationTarget::Container(_))
        {
            return;
        }
        let url = if navigation.srcdoc.is_some() {
            Url::parse("about:srcdoc").ok()
        } else {
            self.frames
                .get(&initiator)
                .and_then(|document| document.resolve_frame_url(&navigation.spec))
        };
        // An unresolvable `src` is not a navigation: the frame keeps the
        // document it has
        // (<https://html.spec.whatwg.org/multipage/urls-and-fetching.html#parse-a-url>).
        let Some(url) = url else {
            return;
        };
        let snapshot = url.scheme() == "about" && matches!(url.path(), "blank" | "srcdoc");
        self.record_frame_history(
            frame,
            Some(navigation.clone()),
            snapshot,
            replace,
            Some(url),
        );
    }

    /// Appends or replaces one entry in the frame's replay store and reports
    /// the change to the browser, which owns the entry count and position.
    fn record_frame_history(
        &mut self,
        frame: FrameId,
        navigation: Option<FrameNavigation>,
        snapshot: bool,
        replace: bool,
        url: Option<Url>,
    ) {
        if frame == FrameId::MAIN {
            return;
        }
        let state = snapshot
            .then(|| self.frames.get(&frame).map(Document::local_document_state))
            .flatten();
        let initial = navigation.is_none();
        let entry = FrameHistoryEntry {
            navigation,
            state,
            same_document: None,
        };
        // An HTTP load commits in a later task, so its entry is staged until
        // the `Navigated` event. Every local scheme installs synchronously and
        // is real now.
        let standalone = self.joint_history.is_some();
        let pending = !standalone
            && url
                .as_ref()
                .is_some_and(|url| matches!(url.scheme(), "http" | "https"));
        let history = self.frame_history.entry(frame).or_default();
        let local_index = if pending {
            history.stage(entry, replace);
            history.index
        } else {
            history.discard_pending();
            if replace {
                history.replace(entry);
            } else {
                history.push(entry);
            }
            history.index
        };
        let event = match url {
            Some(url) => BrowserEvent::NavigationStarted {
                url: url.to_string(),
                replace,
                local_index,
                pending,
            },
            None => BrowserEvent::FrameAdded {
                url: self.frames.get(&frame).map_or_else(
                    || about_blank().to_string(),
                    |document| document.url().to_string(),
                ),
            },
        };
        if standalone {
            // No browser owns the joint list, so this engine is the authority.
            if let Some(joint) = self.joint_history.as_mut() {
                if initial {
                    joint.join(frame, local_index);
                } else if replace {
                    joint.replace(frame, local_index);
                } else {
                    joint.push(frame, local_index);
                }
            }
            self.standalone_navigated(frame);
        } else {
            self.pending_events.push((frame, event));
        }
    }

    /// Points `frame`'s mirrored history at the joint position its new entry
    /// occupies and tells every other frame that the step count changed.
    ///
    /// A cross-document entry's classic state starts null, and its document
    /// inherits the mirror across the install; the browser-hosted path gets
    /// the same values from the tab actor's `SetHistory`
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#getting-the-history-object-length-and-index>).
    fn standalone_navigated(&mut self, frame: FrameId) {
        let Some(snapshot) = self.standalone_snapshot(frame, None) else {
            return;
        };
        if let Some(document) = self.frames.get_mut(&frame) {
            document.update_history_mirror(&snapshot);
        }
        self.sync_standalone_histories();
    }

    /// Republishes the joint step count and position to every frame whose
    /// mirror disagrees with it, keeping each frame's classic state.
    fn sync_standalone_histories(&mut self) {
        let frames: Vec<FrameId> = self.frames.keys().copied().collect();
        for frame in frames {
            let Some(current) = self
                .frames
                .get(&frame)
                .map(|document| document.world().borrow().history.clone())
            else {
                continue;
            };
            let Some(joint) = self.joint_history.as_ref() else {
                return;
            };
            if current.length == joint.steps.len() && current.index == joint.index {
                continue;
            }
            let Some(snapshot) = self.standalone_snapshot(frame, current.state) else {
                continue;
            };
            if let Some(document) = self.frames.get_mut(&frame) {
                document.update_history_mirror(&snapshot);
            }
        }
    }

    /// The mirror one frame publishes for the joint position it holds, with
    /// `state` as its classic history state.
    fn standalone_snapshot(
        &self,
        frame: FrameId,
        state: Option<String>,
    ) -> Option<crate::protocol::HistorySnapshot> {
        let joint = self.joint_history.as_ref()?;
        let index = joint.index;
        let (local_length, local_index) = self.frame_history.get(&frame).map_or_else(
            || {
                (
                    joint.local_at(index, frame).saturating_add(1),
                    joint.local_at(index, frame),
                )
            },
            |history| (history.entries.len(), history.index),
        );
        Some(crate::protocol::HistorySnapshot {
            length: joint.steps.len(),
            index,
            local_length,
            local_index,
            state,
        })
    }

    /// Replaces one frame's mirrored joint history without moving its
    /// document.
    ///
    /// The replay store's own index is a position in this frame's entry list;
    /// the snapshot's index is the tab's current step, so the two must not be
    /// conflated. Only an explicit traversal moves the replay position.
    fn set_frame_history(&mut self, frame: FrameId, history: &crate::protocol::HistorySnapshot) {
        if frame != FrameId::MAIN
            && let Some(replay) = self.frame_history.get_mut(&frame)
        {
            // A push in another frame can clear this frame's forward branch;
            // the replay store follows the browser so local positions agree.
            if history.local_length < replay.entries.len() {
                replay.entries.truncate(history.local_length);
            }
            replay.index = if replay.entries.is_empty() {
                0
            } else {
                history.local_index.min(replay.entries.len() - 1)
            };
        }
        if let Some(document) = self.frames.get_mut(&frame) {
            document.update_history_mirror(history);
        }
    }

    /// Installs the entry a joint traversal selected in one child frame.
    ///
    /// A same-document step only swaps the URL and classic state. Every other
    /// step restores the saved local document when the entry has one, or
    /// replays the request the entry stored
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#apply-the-history-step>).
    fn install_frame_history(
        &mut self,
        frame: FrameId,
        index: usize,
        url: &str,
        history: &crate::protocol::HistorySnapshot,
        same_document: bool,
        reload: bool,
    ) {
        if frame == FrameId::MAIN {
            return;
        }
        // A traversal owns the frame now: drop a load that has not committed
        // and cancel its dial, so its late completion cannot install a
        // document the browser no longer names.
        if let Some(replay) = self.frame_history.get_mut(&frame) {
            replay.discard_pending();
        }
        if let Some(document) = self.frames.get_mut(&frame) {
            document.cancel_frame_navigation();
        }
        if same_document && !reload {
            let Some(document) = self.frames.get_mut(&frame) else {
                return;
            };
            if let Some(replay) = self.frame_history.get_mut(&frame) {
                replay.index = index;
            }
            let _ = document.traverse_history(url, history);
            return;
        }
        let Some((navigation, state)) = self
            .frame_history
            .get(&frame)
            .and_then(|history| history.at(index))
            .map(|entry| (entry.navigation.clone(), entry.state.clone()))
        else {
            // A same-document entry stored no replay payload; restoring the
            // document's URL and state is the closest available step.
            let Some(document) = self.frames.get_mut(&frame) else {
                return;
            };
            let _ = document.traverse_history(url, history);
            return;
        };
        if let Some(replay) = self.frame_history.get_mut(&frame) {
            replay.index = index;
        }
        let (embedder, container) = {
            let shared = self.runtime.shared.borrow();
            (shared.tree.parent(frame), shared.tree.container(frame))
        };
        if let Some(state) = state {
            let Some(document) = self.frames.get_mut(&frame) else {
                return;
            };
            let srcdoc = navigation
                .as_ref()
                .and_then(|navigation| navigation.srcdoc.as_deref());
            document.restore_local_document(&state, srcdoc);
            document.set_history(history);
            if let (Some(embedder), Some(container)) = (embedder, container) {
                self.mark_frame_load_pending(container, embedder);
            }
            if let Some(container) = container {
                self.publish_frame_document(container, frame);
            }
        } else if let Some(mut navigation) = navigation {
            // The entry stores the request, not the response: replay it as a
            // fresh load. The referrer policy belongs to the parent that is
            // navigating now, not the entry's stale snapshot
            // (<https://w3c.github.io/webappsec-referrer-policy/#determine-requests-referrer>).
            if let NavigationReferrer::Client { policy } = &mut navigation.referrer {
                *policy = None;
            }
            self.navigate_frame(frame, embedder.unwrap_or(frame), &navigation);
        } else {
            // Nothing to replay: keep the frame on its document and only
            // publish the joint position.
            if let Some(document) = self.frames.get_mut(&frame) {
                document.set_history(history);
            }
        }
    }

    /// The document of one frame.
    ///
    /// # Errors
    ///
    /// [`TabError::UnknownFrame`] when the engine does not host `frame`.
    fn frame_mut(&mut self, frame: FrameId) -> Result<&mut Document, TabError> {
        self.frames
            .get_mut(&frame)
            .ok_or(TabError::UnknownFrame { frame: frame.get() })
    }

    /// Replaces one frame's document from a host mount.
    ///
    /// # Errors
    ///
    /// [`TabError::UnknownFrame`] when the engine does not host `frame`.
    pub fn mount_frame(&mut self, frame: FrameId, mount: &Mount) -> Result<(), TabError> {
        // A main-frame mount replaces the page-scoped init scripts before the
        // document's realm can run, so the fresh tree sees the current list.
        if frame == FrameId::MAIN {
            self.init_scripts
                .borrow_mut()
                .clone_from(&mount.head.init_scripts);
        }
        // A fresh page starts a new joint history. A standalone carrier has no
        // tab actor to publish the step count, so the mounted document carries
        // it from its first script onward.
        let mut mounted = None;
        if frame == FrameId::MAIN && self.joint_history.is_some() {
            self.joint_history = Some(StandaloneHistory::seed());
            let mut copy = mount.clone();
            copy.head.history = self.standalone_snapshot(frame, None).unwrap_or_default();
            mounted = Some(copy);
        }
        let mount = mounted.as_ref().unwrap_or(mount);
        self.remove_descendants(frame);
        self.frame_mut(frame)?.mount(mount);
        self.reconcile_frames();
        Ok(())
    }

    /// Opens a response body for `frame`, whose bytes arrive through
    /// [`Engine::write_body`] until [`Engine::end_body`].
    ///
    /// # Errors
    ///
    /// [`TabError::UnknownFrame`] when the engine does not host the frame.
    pub fn open_body(
        &mut self,
        frame: FrameId,
        head: &ResponseHead,
        tab: &crate::protocol::TabState,
    ) -> Result<(), TabError> {
        if frame == FrameId::MAIN {
            self.init_scripts
                .borrow_mut()
                .clone_from(&head.init_scripts);
        }
        self.remove_descendants(frame);
        let document = self.frame_mut(frame)?;
        document.set_history(&head.history);
        // A browser-initiated navigation has no source document, so the new
        // document reports no referrer
        // (<https://html.spec.whatwg.org/multipage/document-lifecycle.html#creating-and-initializing-a-document-object>).
        document.set_document_referrer(None);
        document.install_tab_state(tab);
        document.install_head(head, None);
        Ok(())
    }

    /// Feeds more response bytes to a frame opened by [`Engine::open_body`].
    ///
    /// # Errors
    ///
    /// [`TabError::UnknownFrame`] when the engine does not host the frame.
    pub fn write_body(&mut self, frame: FrameId, bytes: &[u8]) -> Result<(), TabError> {
        self.frame_mut(frame)?.write_body(bytes);
        self.reconcile_frames();
        Ok(())
    }

    /// Ends a response body opened by [`Engine::open_body`].
    ///
    /// # Errors
    ///
    /// [`TabError::UnknownFrame`] when the engine does not host the frame.
    pub fn end_body(&mut self, frame: FrameId) -> Result<(), TabError> {
        self.frame_mut(frame)?.end_body();
        self.reconcile_frames();
        Ok(())
    }

    /// Abandons a response body that will not be finished.
    ///
    /// # Errors
    ///
    /// [`TabError::UnknownFrame`] when the engine does not host the frame.
    pub fn abort_body(&mut self, frame: FrameId) -> Result<(), TabError> {
        self.frame_mut(frame)?.abort_body();
        self.reconcile_frames();
        Ok(())
    }

    /// Evaluates `source` in one frame and returns its string coercion.
    ///
    /// # Errors
    ///
    /// [`TabError::UnknownFrame`] or [`TabError::Script`].
    pub fn eval_in(&mut self, frame: FrameId, source: &str) -> Result<String, TabError> {
        let result = self.frame_mut(frame)?.eval(source);
        self.reconcile_frames();
        result
    }

    /// Evaluates `source` in one frame and interns node handles for the protocol.
    ///
    /// # Errors
    ///
    /// [`TabError::UnknownFrame`] or [`TabError::Script`].
    pub fn execute_remote_in(
        &mut self,
        frame: FrameId,
        source: crate::ScriptSource<&str>,
        timeout: Option<Duration>,
    ) -> Result<RemoteValue, TabError> {
        let result = self.frame_mut(frame)?.execute_remote(source, timeout);
        self.reconcile_frames();
        result
    }

    /// Sets `frame`'s persistent viewport and re-lays out its active document.
    ///
    /// # Errors
    ///
    /// [`TabError::UnknownFrame`] when the engine does not host `frame`.
    pub fn set_viewport(
        &mut self,
        frame: FrameId,
        width: u32,
        height: u32,
    ) -> Result<(), TabError> {
        if frame == FrameId::MAIN {
            // Child frames share the tab's viewport; Blitz does not couple
            // iframe layout to the child, so the tab size is the correct
            // approximation until it does.
            let frames: Vec<FrameId> = self.frames.keys().copied().collect();
            for id in frames {
                if let Some(document) = self.frames.get_mut(&id) {
                    document.set_viewport(width, height);
                }
            }
            return Ok(());
        }
        self.frame_mut(frame)?.set_viewport(width, height);
        Ok(())
    }

    /// Registers a page-scoped init script for every frame's future realms
    /// and, when `run_immediately`, evaluates it in every existing frame now
    /// (<https://chromedevtools.github.io/devtools-protocol/tot/Page/#method-addScriptToEvaluateOnNewDocument>).
    pub fn add_init_script(&mut self, id: u64, source: &str, run_immediately: bool) {
        self.init_scripts.borrow_mut().push((id, source.to_owned()));
        if !run_immediately {
            return;
        }
        let frames: Vec<FrameId> = self.frames.keys().copied().collect();
        for frame in frames {
            if let Some(document) = self.frames.get_mut(&frame) {
                document.run_init_script(source);
            }
        }
    }

    /// Removes a page-scoped init script.
    pub fn remove_init_script(&mut self, id: u64) {
        self.init_scripts
            .borrow_mut()
            .retain(|(script_id, _)| *script_id != id);
    }

    /// The frame's scrollable content size in CSS pixels.
    ///
    /// # Errors
    ///
    /// [`TabError::UnknownFrame`] when the engine does not host `frame`.
    pub fn content_size(&mut self, frame: FrameId) -> Result<(f32, f32), TabError> {
        Ok(self.frame_mut(frame)?.content_size())
    }

    /// Answers one typed devtools DOM request from `frame`'s live tree.
    ///
    /// # Errors
    ///
    /// [`TabError::UnknownFrame`] when the engine does not host `frame`, or
    /// [`TabError::Dom`] when the tree cannot answer the request.
    pub fn dom(
        &mut self,
        frame: FrameId,
        request: &crate::DomRequest,
    ) -> Result<crate::DomReply, TabError> {
        self.frame_mut(frame)?.dom(request)
    }

    /// Renders one frame to a PNG.
    ///
    /// The document's `<style>` sheets are applied. The viewport and crop
    /// window come from the caller (Playwright's `clip`); the base viewport
    /// is the shared 800x600 virtual size.
    ///
    /// Viewport sides convert saturating at least 1 (the painter rejects
    /// over-cap sizes).
    ///
    /// # Errors
    ///
    /// [`TabError::UnknownFrame`] when the frame is not mounted and
    /// [`TabError::Render`] when the pipeline refuses the document.
    pub fn screenshot_frame(
        &mut self,
        frame: FrameId,
        request: &crate::ScreenshotRequest,
    ) -> Result<Vec<u8>, TabError> {
        let image = self.render_frame(frame, request)?;
        crate::render::encode_png(&image).map_err(|error| TabError::Render {
            message: error.to_string(),
        })
    }

    /// Renders one frame and returns the raw pixels.
    ///
    /// The reftest lane compares these buffers directly: PNG encoding and
    /// decoding is pure overhead when both sides are rendered here.
    ///
    /// # Errors
    ///
    /// [`TabError::UnknownFrame`] when the frame is not mounted and
    /// [`TabError::Render`] when the pipeline refuses the document.
    pub fn render_frame(
        &mut self,
        frame: FrameId,
        request: &crate::ScreenshotRequest,
    ) -> Result<crate::RgbaImage, TabError> {
        let document = self
            .frames
            .get(&frame)
            .ok_or(TabError::UnknownFrame { frame: frame.get() })?;
        let world = document.world();
        let image = {
            let world = world.borrow();
            let Some(mut parsed) = world.main_document_mut() else {
                return Err(TabError::RendererUnavailable {
                    message: "no document to render".into(),
                });
            };
            // The capture surface may exceed the persistent viewport (a clip
            // or full-page shot), so it applies for this capture only and is
            // restored afterwards, keeping layout in step with `window`
            // metrics
            // (<https://chromedevtools.github.io/devtools-protocol/tot/Page/#method-captureScreenshot>).
            let (width, height) = capture_sides(request.viewport_width, request.viewport_height)?;
            let previous = parsed.document.base.viewport().clone();
            parsed
                .document
                .base
                .set_viewport(blitz_traits::shell::Viewport::new(
                    width,
                    height,
                    1.0,
                    blitz_traits::shell::ColorScheme::Light,
                ));
            crate::render::resolve_until_settled(&mut parsed.document.base);
            let image = crate::render::paint(&mut parsed.document.base, width, height).map_err(
                |error| TabError::Render {
                    message: error.to_string(),
                },
            )?;
            parsed.document.base.set_viewport(previous);
            image
        };
        match request.clip {
            Some(clip) => image
                .crop(clip.x, clip.y, clip.width, clip.height)
                .map_err(|error| TabError::Render {
                    message: error.to_string(),
                }),
            None => Ok(image),
        }
    }

    /// Removes and returns every pending event, with its frame.
    ///
    /// # Errors
    ///
    /// [`TabError::RendererUnavailable`] when a frame retained more events
    /// than its limit; the queue empties and the excess is lost.
    pub fn take_events(&mut self) -> Result<Vec<(FrameId, RendererEvent)>, TabError> {
        let mut events = Vec::new();
        for (&frame, document) in &mut self.frames {
            let pending = document
                .take_events()
                .map_err(|()| TabError::RendererUnavailable {
                    message: "the retained event limit was exceeded".into(),
                })?;
            events.extend(pending.into_iter().map(|event| (frame, event)));
        }
        Ok(events)
    }

    /// Removes and returns every pending event the browser acts on, with its
    /// frame. Renderer-internal progress is discarded: it has no browser-side
    /// state and never crosses the IPC boundary.
    ///
    /// # Errors
    ///
    /// [`TabError::RendererUnavailable`] when a frame retained more events
    /// than its limit; the queue empties and the excess is lost.
    pub fn take_browser_events(&mut self) -> Result<Vec<(FrameId, BrowserEvent)>, TabError> {
        let mut out = std::mem::take(&mut self.pending_events);
        for (frame, event) in self.take_events()? {
            if frame != FrameId::MAIN
                && matches!(
                    &event,
                    RendererEvent::Browser(BrowserEvent::Navigated { .. })
                )
            {
                // The document exists now, so the staged navigation becomes a
                // real replay entry with the same local position the browser
                // promotes.
                if let Some(history) = self.frame_history.get_mut(&frame) {
                    history.commit_pending();
                }
            }
            // A same-document push adds a local position whose replay is its
            // URL and classic state. Keep the store aligned with the browser's
            // list so a later traversal names the right index.
            if frame != FrameId::MAIN
                && let RendererEvent::Browser(BrowserEvent::HistoryUpdated {
                    url,
                    state,
                    handling: crate::protocol::HistoryHandling::Push,
                }) = &event
            {
                self.frame_history
                    .entry(frame)
                    .or_default()
                    .push_same_document(SameDocumentEntry {
                        url: url.clone(),
                        state: state.clone(),
                    });
            }
            if let Some(event) = event.into_browser() {
                out.push((frame, event));
            }
        }
        // Traversals queued during this drain follow its synchronous history
        // updates, so `pushState(); back()` applies the push first.
        out.extend(std::mem::take(&mut self.deferred_events));
        Ok(out)
    }

    /// Registers a JS-truth predicate for [`Self::settle_js_waiters`].
    pub fn add_js_waiter(
        &mut self,
        id: u64,
        frame: FrameId,
        source: crate::protocol::ScriptSource,
    ) {
        self.js_waiters.push(JsWaiter { id, frame, source });
    }

    /// Drops a waiter that the browser no longer needs.
    pub fn cancel_js_waiter(&mut self, id: u64) {
        self.js_waiters.retain(|waiter| waiter.id != id);
    }

    /// Evaluates every registered predicate and returns the ids that became
    /// true. The browser hears only those ids, so it never polls page state.
    pub fn settle_js_waiters(&mut self) -> Vec<u64> {
        if self.js_waiters.is_empty() {
            return Vec::new();
        }
        let waiters = std::mem::take(&mut self.js_waiters);
        let mut settled = Vec::new();
        let mut pending = Vec::with_capacity(waiters.len());
        for waiter in waiters {
            let hit = matches!(
                self.execute_remote_in(waiter.frame, waiter.source.as_ref(), None),
                Ok(RemoteValue::Bool(true))
            );
            if hit {
                settled.push(waiter.id);
            } else {
                pending.push(waiter);
            }
        }
        self.js_waiters = pending;
        settled
    }

    /// Runs every immediately ready task in every frame until the shared
    /// script deadline. Never blocks.
    ///
    /// Returns whether ready work was left queued because the deadline passed.
    /// The session then reads its inbox before draining again, so a batch of
    /// runaway timers cannot hold the renderer past the request timeout.
    #[must_use]
    pub fn drain_ready(&mut self) -> bool {
        self.drain_ready_until(std::time::Instant::now() + crate::TURN_SCRIPT_BUDGET)
    }

    /// [`Engine::drain_ready`] bounded by `deadline`.
    ///
    /// Returns whether ready work remains queued after `deadline`.
    #[must_use]
    pub fn drain_ready_until(&mut self, deadline: std::time::Instant) -> bool {
        // Reconcile around every task so a task that inserts an iframe is
        // visible to the next task: `contentDocument` is non-null from the
        // task after the connection, not inside the connecting task itself
        // (https://html.spec.whatwg.org/multipage/iframe-embed-object.html#dom-iframe-contentdocument).
        //
        // Frames take one task per pass, round-robin: messages posted to
        // different frames must run in send order, and draining one frame dry
        // would let a later message overtake an earlier one
        // (<https://html.spec.whatwg.org/multipage/webappapis.html#task-queue>).
        //
        // One deadline covers the whole drain. Each task still has its own
        // entry budget, clamped to this instant, and a task that would start
        // after the instant stays queued.
        let _turn = self.runtime.open_turn(deadline);
        self.reconcile_frames();
        loop {
            if self.runtime.stop.is_set() {
                return false;
            }
            if std::time::Instant::now() >= deadline {
                return self.has_ready_work();
            }
            self.drain_storage_events();
            let mut ran = false;
            let frames: Vec<FrameId> = self.frames.keys().copied().collect();
            for frame in frames {
                if std::time::Instant::now() >= deadline {
                    return self.has_ready_work();
                }
                let more = self
                    .frames
                    .get_mut(&frame)
                    .is_some_and(Document::drain_step);
                if more {
                    ran = true;
                    self.reconcile_frames();
                }
            }
            let worker_ids: Vec<WorkerId> = self.workers.keys().copied().collect();
            for id in worker_ids {
                if std::time::Instant::now() >= deadline {
                    return self.has_ready_work();
                }
                if self.workers.get_mut(&id).is_some_and(Worker::drain_step) {
                    ran = true;
                    // A worker task's own requests (a post, a close, an error)
                    // are observable before its next task runs: apply them now
                    // instead of waiting for every owner to go idle.
                    self.reconcile_frames();
                }
            }
            // A dedicated worker's realm runs on its own thread. The engine
            // only schedules its commands and timers and observes whether its
            // thread still has work
            // (<https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model>).
            let mut dedicated_ran = false;
            self.runtime.worker_dir.with_workers(|workers| {
                for worker in workers.values_mut() {
                    if std::time::Instant::now() >= deadline {
                        break;
                    }
                    if worker.drain_step() {
                        dedicated_ran = true;
                    }
                }
            });
            if dedicated_ran {
                ran = true;
                self.reconcile_frames();
            }
            self.workers.retain(|_, worker| !worker.is_closed());
            self.runtime.worker_dir.remove_closed();
            if !ran {
                // A pass that ran no task can still have adopted realm work:
                // stepping the document moves its outbox into a navigation or
                // a history traversal, which only a reconcile applies. Report
                // idle only once the reconcile finds nothing.
                if self.reconcile_frames() {
                    continue;
                }
                break;
            }
        }
        false
    }

    /// Whether a drain would run anything: a queued task, a queued dial, work
    /// waiting in a realm's outbox, or a timer that is already due.
    ///
    /// A caller that may enter JavaScript outside a drain (evaluating a JS
    /// predicate, for instance) can leave tasks behind, so it asks this before
    /// waiting for the next event.
    #[must_use]
    pub fn has_ready_work(&self) -> bool {
        self.frames.values().any(Document::has_ready_work)
            || self.workers.values().any(Worker::has_ready_work)
            || self
                .runtime
                .worker_dir
                .with_workers(|workers| workers.values().any(WorkerHandle::has_ready_work))
            || self.runtime.has_worker_requests()
    }

    /// Whether any frame's document has a dial queued or in flight.
    ///
    /// A host that is driven by in-process effect completions waits on this
    /// instead of treating a page with no queued task as finished.
    #[must_use]
    pub fn has_pending_dials(&self) -> bool {
        self.frames.values().any(Document::has_pending_dials)
            || self.runtime.shared.has_keepalive_fetches()
            || self.workers.values().any(Worker::has_pending_dials)
            || self
                .runtime
                .worker_dir
                .with_workers(|workers| workers.values().any(WorkerHandle::has_pending_dials))
    }

    /// Queues one remote `postMessage` payload on the main frame's task
    /// source; the frame dispatches a trusted `message` event with a null
    /// source after checking the intended target origin against whichever
    /// document runs the task.
    pub fn receive_remote_window_message(&mut self, message: crate::protocol::PostedMessage) {
        if let Some(document) = self.frames.get_mut(&FrameId::MAIN) {
            document.push_remote_message(message);
        }
    }

    /// Activates an entry that belongs to the main frame's current document.
    ///
    /// # Errors
    ///
    /// [`TabError::UnknownFrame`] when the main frame is not mounted and
    /// [`TabError::InvalidUrl`] when `url` does not parse or is cross-origin.
    pub fn traverse_history(
        &mut self,
        url: &str,
        history: &crate::protocol::HistorySnapshot,
    ) -> Result<(), TabError> {
        let document = self
            .frames
            .get_mut(&FrameId::MAIN)
            .ok_or(TabError::UnknownFrame {
                frame: FrameId::MAIN.get(),
            })?;
        document.traverse_history(url, history)
    }

    /// Replaces one frame's mirrored joint history without moving its
    /// document.
    pub fn set_history(&mut self, frame: FrameId, history: &crate::protocol::HistorySnapshot) {
        self.set_frame_history(frame, history);
    }

    /// Applies a frame's traversal when no browser is attached.
    ///
    /// The embedded carrier has no tab actor, so the engine's own joint list
    /// resolves the step: a delta counts used steps away from the active one,
    /// and every frame whose entry changes moves with it
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#traverse-the-history-by-a-delta>).
    pub fn traverse_embedded_history(&mut self, frame: FrameId, delta: i32) {
        // `go(0)` reloads the caller's active entry without moving the joint
        // list
        // (<https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-history-go>).
        if delta == 0 {
            let Some((index, url, same_document, state, length)) = self.embedded_entry(frame, None)
            else {
                return;
            };
            let snapshot = self.standalone_snapshot(frame, state).unwrap_or(
                crate::protocol::HistorySnapshot {
                    length,
                    index,
                    local_length: length,
                    local_index: index,
                    state: None,
                },
            );
            self.install_frame_history(frame, index, &url, &snapshot, same_document, true);
            return;
        }
        let Some(joint) = self.joint_history.as_ref() else {
            return;
        };
        let Some(target) = joint.target(delta) else {
            return;
        };
        let length = joint.steps.len();
        let current_step = joint.index;
        let target_entries = joint.steps[target].clone();
        let current_entries = joint.steps[current_step].clone();
        let moves: Vec<(FrameId, usize)> = self
            .frames
            .keys()
            .copied()
            .filter_map(|id| {
                let current = self.frame_history.get(&id).map_or_else(
                    || current_entries.get(&id).copied().unwrap_or(0),
                    |history| history.index,
                );
                let target = target_entries.get(&id).copied().unwrap_or(0);
                (target != current).then_some((id, target))
            })
            .collect();
        // A standalone engine records no main-frame entry, so a step that
        // would move the main frame cannot be applied. Refuse the whole step
        // rather than moving part of the tab.
        if moves.iter().any(|(frame, _)| *frame == FrameId::MAIN) {
            return;
        }
        for (frame, index) in moves {
            let Some((_, url, same_document, state, _)) = self.embedded_entry(frame, Some(index))
            else {
                continue;
            };
            let snapshot = self.standalone_snapshot_at(frame, target, length, index, state);
            self.install_frame_history(frame, index, &url, &snapshot, same_document, false);
        }
        if let Some(joint) = self.joint_history.as_mut() {
            joint.index = target;
        }
        self.sync_standalone_histories();
    }

    /// The replay payload one frame's entry carries: the URL to install,
    /// whether the step is same-document, and the classic state it restores.
    fn embedded_entry(
        &self,
        frame: FrameId,
        index: Option<usize>,
    ) -> Option<(usize, String, bool, Option<String>, usize)> {
        let history = self.frame_history.get(&frame)?;
        let index = index.unwrap_or(history.index);
        let entry = history.at(index)?;
        if let Some(entry) = &entry.same_document {
            return Some((
                index,
                entry.url.clone(),
                true,
                Some(entry.state.clone()),
                history.entries.len(),
            ));
        }
        let url = entry
            .navigation
            .as_ref()
            .map_or_else(String::new, |navigation| navigation.spec.clone());
        Some((index, url, false, None, history.entries.len()))
    }

    /// The mirror for an entry a standalone traversal selected, whose joint
    /// position is `step`.
    fn standalone_snapshot_at(
        &self,
        frame: FrameId,
        step: usize,
        length: usize,
        index: usize,
        state: Option<String>,
    ) -> crate::protocol::HistorySnapshot {
        let local_length = self
            .frame_history
            .get(&frame)
            .map_or(1, |history| history.entries.len());
        crate::protocol::HistorySnapshot {
            length,
            index: step,
            local_length,
            local_index: index,
            state,
        }
    }

    /// Applies one same-document history update to the standalone joint list.
    ///
    /// A push adds the step its realm already applied to its own mirror. The
    /// main frame has no replay store, so only its position in the step map is
    /// recorded, and a later step that would move it is refused.
    fn apply_standalone_history_update(
        &mut self,
        frame: FrameId,
        update: crate::document::HistoryUpdate,
    ) {
        use crate::protocol::HistoryHandling;
        let Some(joint) = self.joint_history.as_mut() else {
            return;
        };
        match update.handling {
            HistoryHandling::Push => {
                let entry = SameDocumentEntry {
                    url: update.url,
                    state: update.state,
                };
                let local_index = if frame == FrameId::MAIN {
                    joint
                        .active()
                        .get(&frame)
                        .copied()
                        .unwrap_or(0)
                        .saturating_add(1)
                } else {
                    let history = self.frame_history.entry(frame).or_default();
                    history.push_same_document(entry);
                    history.index
                };
                joint.push(frame, local_index);
            }
            HistoryHandling::Replace => {
                if frame != FrameId::MAIN
                    && let Some(history) = self.frame_history.get_mut(&frame)
                {
                    history.replace_same_document(SameDocumentEntry {
                        url: update.url,
                        state: update.state,
                    });
                }
            }
        }
        self.sync_standalone_histories();
    }

    /// Activates one child frame's entry under the browser's joint step.
    ///
    /// # Errors
    ///
    /// [`TabError::UnknownFrame`] when the engine does not host `frame`.
    pub fn traverse_frame_history(
        &mut self,
        frame: FrameId,
        index: usize,
        url: &str,
        history: &crate::protocol::HistorySnapshot,
        same_document: bool,
        reload: bool,
    ) -> Result<(), TabError> {
        if !self.frames.contains_key(&frame) {
            return Err(TabError::UnknownFrame { frame: frame.get() });
        }
        self.install_frame_history(frame, index, url, history, same_document, reload);
        Ok(())
    }

    /// Queues one `BroadcastChannel` message on every same-origin frame of
    /// this engine
    /// (<https://html.spec.whatwg.org/multipage/web-messaging.html#broadcasting-to-other-browsing-contexts>).
    pub fn receive_broadcast_message(
        &mut self,
        origin: &crate::protocol::StorageKey,
        name: &str,
        payload: &str,
        source: Option<u64>,
    ) {
        for document in self.frames.values_mut() {
            // Two opaque origins never share a `BroadcastChannel`, and the
            // key has no opaque case at all
            // (<https://html.spec.whatwg.org/multipage/web-messaging.html#broadcasting-to-other-browsing-contexts>).
            if document.storage_key().as_ref() != Some(origin) {
                continue;
            }
            if !document.world().borrow().is_attached() {
                continue;
            }
            document.push_broadcast_message(
                origin.to_string(),
                name.to_owned(),
                payload.to_owned(),
                source,
            );
        }
        // A worker has no frame entry but its realm can hold channels.
        let origin_string = origin.to_string();
        for worker in self.workers.values_mut() {
            // Two opaque origins never share a `BroadcastChannel`, and the
            // key has no opaque case at all
            // (<https://html.spec.whatwg.org/multipage/web-messaging.html#broadcasting-to-other-browsing-contexts>).
            if !worker.has_origin(origin.origin()) {
                continue;
            }
            worker.queue_broadcast(
                origin_string.clone(),
                name.to_owned(),
                payload.to_owned(),
                source,
            );
        }
        self.runtime.worker_dir.with_workers(|workers| {
            for worker in workers.values_mut() {
                if !worker.has_origin(origin.origin()) {
                    continue;
                }
                worker.queue_broadcast(
                    origin_string.clone(),
                    name.to_owned(),
                    payload.to_owned(),
                    source,
                );
            }
        });
    }

    /// Delivers one opaque-origin `BroadcastChannel` post inside this engine.
    ///
    /// An opaque origin has no storage key for the browser to route, but
    /// realms that share one opaque origin are in the same agent cluster, so
    /// they are exactly the reachable channels
    /// (<https://html.spec.whatwg.org/multipage/web-messaging.html#broadcasting-to-other-browsing-contexts>).
    fn broadcast_local(&mut self, origin: &url::Origin, name: &str, payload: &str, channel: u64) {
        let origin_string = origin.unicode_serialization();
        for document in self.frames.values_mut() {
            let world = document.world();
            if world.borrow().origin != *origin || !world.borrow().is_attached() {
                continue;
            }
            document.push_broadcast_message(
                origin_string.clone(),
                name.to_owned(),
                payload.to_owned(),
                Some(channel),
            );
        }
        for worker in self.workers.values_mut() {
            if !worker.has_origin(origin) {
                continue;
            }
            worker.queue_broadcast(
                origin_string.clone(),
                name.to_owned(),
                payload.to_owned(),
                Some(channel),
            );
        }
        self.runtime.worker_dir.with_workers(|workers| {
            for worker in workers.values_mut() {
                if !worker.has_origin(origin) {
                    continue;
                }
                worker.queue_broadcast(
                    origin_string.clone(),
                    name.to_owned(),
                    payload.to_owned(),
                    Some(channel),
                );
            }
        });
    }

    /// Queues one `localStorage` change that the browser broadcast. The source
    /// frame is set only when the change came from this same assignment;
    /// other renderers and assignments pass `None`.
    pub fn receive_storage_event(&mut self, event: PendingStorageEvent) {
        self.runtime.pending_storage.borrow_mut().push(event);
    }

    /// Moves changes queued by this engine's own script onto the
    /// `storage`-event task of every receiving frame.
    fn drain_storage_events(&mut self) {
        let pending: Vec<PendingStorageEvent> =
            std::mem::take(&mut *self.runtime.pending_storage.borrow_mut());
        for event in pending {
            self.queue_storage_event(&event);
        }
    }

    /// Queues one change on every frame of `event.origin`, excluding the
    /// window whose script made it
    /// (<https://html.spec.whatwg.org/multipage/webstorage.html#concept-storage-broadcast>).
    fn queue_storage_event(&mut self, event: &PendingStorageEvent) {
        let frames: Vec<FrameId> = self.frames.keys().copied().collect();
        for frame in frames {
            if event.source == Some(frame) {
                continue;
            }
            let Some(document) = self.frames.get_mut(&frame) else {
                continue;
            };
            if document.storage_key().as_ref() != Some(&event.origin) {
                continue;
            }
            document.push_storage_event(event.clone());
        }
    }

    /// Earliest timer deadline across every frame and every dedicated worker.
    #[must_use]
    pub fn next_deadline(&self) -> Option<Instant> {
        self.frames
            .values()
            .filter_map(Document::next_deadline)
            .chain(self.workers.values().filter_map(Worker::next_deadline))
            .chain(self.runtime.worker_dir.with_workers(|workers| {
                workers
                    .values()
                    .filter_map(WorkerHandle::next_deadline)
                    .min()
            }))
            .min()
    }

    /// Applies everything the documents produced outside their task queues:
    /// adopted frames, navigations, history traversals, deliveries, and the
    /// load events they unblock. Returns whether anything was applied.
    fn reconcile_frames(&mut self) -> bool {
        let mut applied = false;
        loop {
            let workers = self.adopt_worker_requests();
            let adopted = self.adopt_pending_frames();
            let standalone = self.joint_history.is_some();
            let removed = self.runtime.shared.borrow_mut().take_removed_frames();
            for (frame, container) in &removed {
                if let Some(container) = container {
                    self.runtime.registry.borrow_mut().forget_frame(*container);
                }
                self.runtime
                    .registry
                    .borrow_mut()
                    .forget_frame_world(*frame);
                self.drop_frame_document(*frame);
                self.frame_history.remove(frame);
                if let Some(joint) = self.joint_history.as_mut() {
                    joint.leave(*frame);
                }
                if !standalone {
                    self.pending_events
                        .push((*frame, BrowserEvent::FrameRemoved));
                }
                // A parent navigation or `window.close()` removes frames
                // through this queue, not `remove_subtree`, so report them
                // here as well; the browser's teardown is idempotent.
                self.runtime.services.frame_destroyed(*frame);
            }
            let PendingWork {
                removed_containers,
                navigations,
                traversals,
                history_updates,
                streams,
                ran_scripts,
            } = self.take_pending_work();
            let deliveries = self.runtime.shared.borrow_mut().take_deliveries();
            let had_work = workers
                || adopted
                || ran_scripts
                || !removed_containers.is_empty()
                || !navigations.is_empty()
                || !traversals.is_empty()
                || !history_updates.is_empty()
                || !streams.is_empty()
                || !deliveries.is_empty();
            for container in removed_containers {
                self.remove_subtree(container);
            }
            // A same-document push must take its step before the traversal the
            // same script queued after it, so `pushState(); back()` sees the
            // push
            // (<https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-history-back>).
            if standalone {
                for (frame, update) in history_updates {
                    self.apply_standalone_history_update(frame, update);
                }
            }
            self.apply_navigations(navigations);
            // An auxiliary context's realm starts after `window.open`
            // returned, so the opener's proxy writes are replayed here
            // (<https://html.spec.whatwg.org/multipage/window-object.html#windowproxy-set>).
            let replayed = self.flush_pending_proxy_sets();
            if standalone {
                for (frame, delta) in traversals {
                    self.traverse_embedded_history(frame, delta);
                }
            } else {
                self.deferred_events.extend(
                    traversals
                        .into_iter()
                        .map(|(frame, delta)| (frame, BrowserEvent::HistoryTraversal { delta })),
                );
            }
            self.publish_streams(streams);
            self.apply_deliveries(deliveries);
            self.flush_embedder_csp();
            let fired_load = self.fire_ready_frame_loads();
            if !had_work && !fired_load && !replayed {
                break;
            }
            applied = true;
        }
        applied
    }

    /// Drains the work every hosted document produced outside its task queue.
    ///
    /// The realms queue navigations, traversals, and history updates in their
    /// own outboxes; one pass converts them to engine-level requests, and
    /// [`Self::reconcile_frames`] applies them in a fixed order.
    fn take_pending_work(&mut self) -> PendingWork {
        let mut work = PendingWork::default();
        let frame_ids: Vec<FrameId> = self.frames.keys().copied().collect();
        for frame in frame_ids {
            let Some(document) = self.frames.get_mut(&frame) else {
                continue;
            };
            // A script appended to a document another realm created could not
            // run there; its document's realm starts here, so its effects
            // belong to this turn.
            work.ran_scripts |= document.run_deferred_scripts();
            work.removed_containers
                .extend(document.take_removed_containers());
            work.navigations.extend(
                document
                    .take_frame_navigations()
                    .into_iter()
                    .map(|navigation| (frame, navigation)),
            );
            work.traversals.extend(
                document
                    .take_frame_traversals()
                    .into_iter()
                    .map(|delta| (frame, delta)),
            );
            work.history_updates.extend(
                document
                    .take_history_updates()
                    .into_iter()
                    .map(|update| (frame, update)),
            );
            if let Some(closed) = document.take_stream_publish() {
                work.streams.push((frame, closed));
            }
        }
        work
    }

    /// Replays `WindowProxy` writes made before a frame's realm existed.
    ///
    /// `window.open` and `iframe.contentWindow` return a proxy before the new
    /// document's realm starts, so writes such as `popup.onload = ...` are held
    /// by the writer's realm. Every writer realm is asked, because a frame can
    /// be written through by its parent, its opener, or a sibling. A navigation
    /// resets the flag, so the writes land on the document that replaces the
    /// initial `about:blank`
    /// (<https://html.spec.whatwg.org/multipage/window-object.html#windowproxy-set>).
    fn flush_pending_proxy_sets(&mut self) -> bool {
        let pending: Vec<FrameId> = self
            .frames
            .iter()
            .filter(|(_, document)| document.needs_proxy_sets_replay())
            .map(|(&frame, _)| frame)
            .collect();
        let mut replayed = false;
        for frame in pending {
            // A frame without a realm keeps its writes: they belong to the
            // document whose realm starts next. `fire_ready_frame_loads` starts
            // the realm of a frame whose `load` is held for this replay.
            if !self.frames.get(&frame).is_some_and(Document::has_realm) {
                continue;
            }
            // A frame loading a new document keeps them too: the navigation
            // replaces the document a replay would land on, so they belong to
            // the document that replaces it
            // (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#navigate>).
            if self
                .frames
                .get(&frame)
                .is_some_and(Document::has_pending_frame_load)
            {
                continue;
            }
            let writers: Vec<FrameId> = self.frames.keys().copied().collect();
            for writer in writers {
                if let Some(document) = self.frames.get_mut(&writer) {
                    document.flush_frame_proxy_sets(frame);
                }
            }
            // Marked only after every writer flushed: a writer that cannot run
            // (a stopped realm) must leave the frame waiting rather than drop
            // its writes.
            if let Some(document) = self.frames.get_mut(&frame) {
                document.mark_proxy_sets_replayed();
            }
            replayed = true;
        }
        replayed
    }

    /// Drops a frame's document after handing off the `keepalive` requests it
    /// queued but never sent: a `keepalive` fetch outlives the environment that
    /// made it, and the environment is gone as soon as this document is
    /// (<https://fetch.spec.whatwg.org/#keepalive-flag>).
    fn drop_frame_document(&mut self, frame: FrameId) {
        if let Some(mut document) = self.frames.remove(&frame) {
            document.flush_keepalive_fetches();
        }
    }

    /// Removes a frame and every descendant it owns before the backing
    /// document is replaced or disconnected. Lifecycle events can arrive only
    /// for the direct iframe, so recurse through the frame tree explicitly to
    /// avoid stale realms and pending loads.
    fn remove_subtree(&mut self, container: crate::dom::tree::NodeId) {
        let Some(child) = self
            .runtime
            .shared
            .borrow()
            .tree
            .frame_for_container(container)
        else {
            return;
        };
        let descendants: Vec<crate::dom::tree::NodeId> = self
            .runtime
            .shared
            .borrow()
            .tree
            .children(child)
            .iter()
            .filter_map(|&nested| self.runtime.shared.borrow().tree.container(nested))
            .collect();
        for nested in descendants {
            self.remove_subtree(nested);
        }
        if let Some(parent) = self.runtime.shared.borrow().tree.parent(child)
            && let Some(document) = self.frames.get_mut(&parent)
        {
            document.cancel_frame_load(container);
        }
        self.runtime.registry.borrow_mut().forget_frame(container);
        self.runtime.registry.borrow_mut().forget_frame_world(child);
        {
            let mut shared = self.runtime.shared.borrow_mut();
            shared.tree.remove(child);
            shared.forget_document(child);
        }
        // Dropping the document closes its ports and its realm.
        self.drop_frame_document(child);
        self.frame_history.remove(&child);
        if let Some(joint) = self.joint_history.as_mut() {
            joint.leave(child);
        }
        if self.joint_history.is_none() {
            self.pending_events
                .push((child, BrowserEvent::FrameRemoved));
        }
        self.runtime.services.frame_destroyed(child);
    }

    fn remove_descendants(&mut self, parent: FrameId) {
        let containers: Vec<crate::dom::tree::NodeId> = self
            .runtime
            .shared
            .borrow()
            .tree
            .children(parent)
            .iter()
            .filter_map(|&child| self.runtime.shared.borrow().tree.container(child))
            .collect();
        for container in containers {
            self.remove_subtree(container);
        }
    }

    /// Navigates `child` to one URL, resolving a relative URL against the
    /// document that queued the navigation and dispatching on its scheme
    /// (<https://html.spec.whatwg.org/multipage/iframe-embed-object.html#process-the-iframe-attributes>).
    ///
    /// The embedder is read from the tree: it is the initiator for every
    /// navigation a fresh `iframe` starts, and only a link that targets another
    /// navigable splits the two.
    fn navigate_frame(
        &mut self,
        child: FrameId,
        initiator: FrameId,
        navigation: &FrameNavigation,
    ) -> NavOutcome {
        let (embedder, container) = {
            let shared = self.runtime.shared.borrow();
            (shared.tree.parent(child), shared.tree.container(child))
        };
        // A `srcdoc` attribute takes priority over `src`: the frame navigates
        // to `about:srcdoc` with the attribute's value as the document source
        // (<https://html.spec.whatwg.org/multipage/iframe-embed-object.html#process-the-iframe-attributes>).
        if let Some(markup) = &navigation.srcdoc {
            self.install_sandbox(child, navigation);
            self.load_frame_srcdoc(child, initiator, markup);
            if let (Some(embedder), Some(container)) = (embedder, container) {
                self.mark_frame_load_pending(container, embedder);
            }
            return NavOutcome::Loaded;
        }
        // An empty `src` is a container with no URL to load; it keeps its
        // `about:blank`. Every other navigation resolves an empty string
        // against the initiator document, which is that document's own URL
        // (<https://html.spec.whatwg.org/multipage/urls-and-fetching.html#parse-a-url>),
        // so a link or form that names no URL reloads its document instead of
        // blanking it.
        let blank = navigation.spec.is_empty()
            && matches!(navigation.target, NavigationTarget::Container(_));
        let resolved = (!blank)
            .then(|| {
                self.frames
                    .get(&initiator)
                    .and_then(|document| document.resolve_frame_url(&navigation.spec))
            })
            .flatten();
        let Some(url) = resolved else {
            if blank {
                // About:blank, the initial document of every frame without a
                // usable `src`, inherits the initiator's captured origin
                // (<https://html.spec.whatwg.org/multipage/browsers.html#determining-the-origin>).
                // A frame the engine just created is already on that document;
                // reloading it would throw away its realm and anything a script
                // has already set on it.
                self.install_sandbox(child, navigation);
                self.keep_initial_blank(child, initiator);
            }
            if let (Some(embedder), Some(container)) = (embedder, container) {
                self.mark_frame_load_pending(container, embedder);
            }
            return NavOutcome::KeptDocument;
        };
        // A URL that differs from the target frame's document URL only in its
        // fragment is a same-document navigation: the document and its realm
        // survive, the URL and the history entry change, and `hashchange` is
        // queued for that document's next turn. Loading it instead would
        // replace the page, so a link to `#fragment` would reload the document
        // (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#navigate-to-a-fragment>).
        if url.fragment().is_some()
            && let Some(document) = self.frames.get(&child)
            && same_document_url(&document.world().borrow().document_url, &url)
        {
            document.queue_fragment_navigation(url);
            return NavOutcome::Fragment;
        }
        self.load_frame_url(child, &url, initiator, navigation);
        if let (Some(embedder), Some(container)) = (embedder, container) {
            self.mark_frame_load_pending(container, embedder);
        }
        NavOutcome::Loaded
    }

    /// Whether `initiator`'s active sandboxing flag set lets it navigate
    /// `chosen`. A sandboxed document may always navigate itself and the
    /// contexts nested in it; the top-level context needs
    /// `allow-top-navigation`; a sibling or ancestor needs no sandboxed
    /// navigation flag; and a popup the document opened is its permitted
    /// sandboxed navigator
    /// (<https://html.spec.whatwg.org/multipage/browsers.html#sandboxed-navigation-browsing-context-flag>).
    fn sandbox_allows_navigation(&self, initiator: FrameId, chosen: FrameId) -> bool {
        if initiator == chosen {
            return true;
        }
        let Some(source) = self.frames.get(&initiator) else {
            return true;
        };
        let (flags, activated) = {
            let world = source.world();
            let world = world.borrow();
            (world.sandbox, world.user_activation_active())
        };
        let shared = self.runtime.shared.borrow();
        let tree = &shared.tree;
        if tree.is_inclusive_descendant(initiator, chosen)
            || tree.auxiliary_opener(chosen) == Some(initiator)
        {
            return true;
        }
        if tree.top_level(initiator) == chosen {
            let flag = if activated {
                crate::protocol::SandboxFlag::TopLevelNavigationWithUserActivation
            } else {
                crate::protocol::SandboxFlag::TopLevelNavigationWithoutUserActivation
            };
            return !flags.contains(flag);
        }
        !flags.contains(crate::protocol::SandboxFlag::Navigation)
    }

    /// Installs the flags a navigation commits on `child`'s document. Local
    /// schemes install synchronously, so this runs immediately before the
    /// install; an HTTP response carries the flags until its commit.
    fn install_sandbox(&mut self, child: FrameId, navigation: &FrameNavigation) {
        if let Some(document) = self.frames.get(&child) {
            document
                .world()
                .borrow_mut()
                .install_sandbox(navigation.sandbox);
        }
    }

    /// The request referrer and policy a navigation sends: the navigating
    /// element's policy when it names one, the initiator document's otherwise.
    /// The client environment resolves the source in the browser
    /// (<https://w3c.github.io/webappsec-referrer-policy/#determine-requests-referrer>,
    /// <https://html.spec.whatwg.org/multipage/links.html#hyperlink-referrer-policy>).
    fn navigation_referrer(
        &self,
        initiator: FrameId,
        navigation: &FrameNavigation,
    ) -> (crate::protocol::ReferrerRequest, net::ReferrerPolicy) {
        let policy = match navigation.referrer {
            NavigationReferrer::NoReferrer => {
                return (
                    crate::protocol::ReferrerRequest::NoReferrer,
                    net::ReferrerPolicy::NoReferrer,
                );
            }
            NavigationReferrer::Client { policy } => policy,
        };
        match self.frames.get(&initiator) {
            Some(document) => {
                let world = document.world();
                let world = world.borrow();
                (
                    crate::protocol::ReferrerRequest::Client {
                        url: world.referrer_url.clone(),
                    },
                    policy.or(world.policy.referrer_policy).unwrap_or_default(),
                )
            }
            None => (
                crate::protocol::ReferrerRequest::NoReferrer,
                policy.unwrap_or_default(),
            ),
        }
    }

    /// The referrer a local-scheme document reports. It is the navigation's
    /// determined referrer, exactly as an HTTP navigation's `document.referrer`
    /// is the header the browser sent, so a `no-referrer` or `origin` policy
    /// cannot leak the parent's path to a `data:` or `blob:` child
    /// (<https://w3c.github.io/webappsec-referrer-policy/#determine-requests-referrer>).
    fn local_frame_referrer(
        &self,
        initiator: FrameId,
        navigation: &FrameNavigation,
        target: &Url,
    ) -> Option<Url> {
        let (referrer, policy) = self.navigation_referrer(initiator, navigation);
        let referrer = match referrer {
            crate::protocol::ReferrerRequest::NoReferrer => net::Referrer::NoReferrer,
            crate::protocol::ReferrerRequest::Url(url) => net::Referrer::Url(url),
            crate::protocol::ReferrerRequest::Client { url } => match self.frames.get(&initiator) {
                Some(document) => {
                    let world = document.world();
                    let world = world.borrow();
                    net::Referrer::Client(net::ReferrerClient {
                        origin: world.origin.clone(),
                        url,
                    })
                }
                None => net::Referrer::NoReferrer,
            },
        };
        net::determine_referrer(policy, &referrer, target)
    }

    /// The policy a local-scheme document inherits from its initiator
    /// (<https://html.spec.whatwg.org/multipage/browsers.html#determining-navigation-params-policy-container>).
    fn inherited_policy(&self, parent: FrameId) -> PolicyContainer {
        self.frames
            .get(&parent)
            .map_or_default(|document| document.world().borrow().policy.clone())
    }

    /// Gives a frame's freshly installed local-scheme document the
    /// initiator's policy container.
    fn inherit_policy(&self, child: FrameId, parent: FrameId) {
        let policy = self.inherited_policy(parent);
        if let Some(document) = self.frames.get(&child) {
            document.world().borrow_mut().policy = policy;
        }
    }

    /// Keeps a frame's initial `about:blank` document while it still has one,
    /// so a script-set realm survives; otherwise installs the inherited blank.
    fn keep_initial_blank(&mut self, child: FrameId, parent: FrameId) {
        let inherited = self.frames.get(&parent).map(|document| {
            (
                document.inherited_url(),
                document.world().borrow().cookie_url(),
                document.referrer_url(),
                document.world().borrow().origin.clone(),
            )
        });
        if self
            .frames
            .get(&child)
            .is_some_and(Document::is_initial_blank)
        {
            self.inherit_policy(child, parent);
            return;
        }
        let Some(document) = self.frames.get_mut(&child) else {
            return;
        };
        match inherited {
            Some((url, cookie_url, referrer, origin)) => {
                document.load_about_blank(Some(&url), &cookie_url, Some(origin));
                document.set_document_referrer(Some(referrer));
            }
            None => {
                document.load_about_blank(Some(&about_blank()), &about_blank(), None);
            }
        }
        self.inherit_policy(child, parent);
    }

    /// Whether `url` is a nested cross-document navigation that `frame-src`
    /// governs. Same-document navigations and `about:` are not.
    fn frame_src_applies(&self, child: FrameId, url: &Url) -> bool {
        if self.runtime.shared.borrow().tree.parent(child).is_none() {
            return false;
        }
        if !matches!(url.scheme(), "http" | "https" | "data" | "blob") {
            return false;
        }
        let Some(document) = self.frames.get(&child) else {
            return false;
        };
        let mut current = document.url().clone();
        let mut next = url.clone();
        current.set_fragment(None);
        next.set_fragment(None);
        current != next
    }

    /// Ancestor document URLs nearest-first for a nested frame's `frame-ancestors`
    /// check at commit. Empty for a top-level navigation, which skips it
    /// (<https://w3c.github.io/webappsec-csp/#frame-ancestors-navigation-response>).
    fn ancestor_urls(&self, embedder: Option<FrameId>) -> Vec<Url> {
        let mut ancestors = Vec::new();
        let mut cursor = embedder;
        while let Some(frame) = cursor {
            let Some(document) = self.frames.get(&frame) else {
                break;
            };
            ancestors.push(document.url().clone());
            cursor = self.runtime.shared.borrow().tree.parent(frame);
        }
        ancestors
    }

    /// Reports the embedder's `frame-src` violations and returns whether the
    /// enforced policy refuses `url`
    /// (<https://w3c.github.io/webappsec-csp/#should-request-be-blocked-by-content-security-policy>).
    fn frame_src_blocked(&mut self, embedder: FrameId, child: FrameId, url: &Url) -> bool {
        let violations = {
            let Some(document) = self.frames.get(&embedder) else {
                return false;
            };
            let world = document.world();
            let world = world.borrow();
            world.policy.csp.check_request(
                &world.origin,
                &world.document_url,
                url,
                crate::csp::Destination::Frame,
                None,
            )
        };
        if violations.is_empty() {
            return false;
        }
        let container = self.runtime.shared.borrow().tree.container(child);
        if let Some(document) = self.frames.get_mut(&embedder) {
            let target = container.or_else(|| document.document_node());
            if let Some(target) = target {
                document.fire_csp_violations(target, &violations);
            }
        }
        crate::csp::blocks(&violations)
    }

    /// Dispatches `frame-src` redirect violations into the embedding document.
    fn flush_embedder_csp(&mut self) {
        let frames: Vec<FrameId> = self.frames.keys().copied().collect();
        let mut reports = Vec::new();
        for frame in frames {
            if let Some(document) = self.frames.get_mut(&frame) {
                reports.extend(document.take_embedder_csp_reports());
            }
        }
        for (embedder, violations) in reports {
            let Some(document) = self.frames.get_mut(&embedder) else {
                continue;
            };
            let Some(target) = document.document_node() else {
                continue;
            };
            document.fire_csp_violations(target, &violations);
        }
    }

    /// Starts the load one resolved frame URL names, dispatching on its scheme
    /// (<https://html.spec.whatwg.org/multipage/iframe-embed-object.html#process-the-iframe-attributes>).
    fn load_frame_url(
        &mut self,
        child: FrameId,
        url: &Url,
        initiator: FrameId,
        navigation: &FrameNavigation,
    ) {
        // Nested navigations are `frame-src` on the embedder. A same-document
        // navigation is not, and `about:` stays allowed: blocking it would
        // only show `about:blank` again
        // (<https://w3c.github.io/webappsec-csp/#effective-directive-for-a-request>).
        let embedder = self.runtime.shared.borrow().tree.parent(child);
        let check_frame_src = self.frame_src_applies(child, url);
        if check_frame_src
            && let Some(embedder) = embedder
            && self.frame_src_blocked(embedder, child, url)
        {
            return;
        }
        let frame_gate = (check_frame_src && matches!(url.scheme(), "http" | "https"))
            .then(|| {
                embedder.and_then(|embedder| {
                    self.frames
                        .get(&embedder)
                        .map(|document| document.redirect_gate(crate::csp::Destination::Frame))
                })
            })
            .flatten();
        match url.scheme() {
            "about" => {
                self.install_sandbox(child, navigation);
                self.keep_initial_blank(child, initiator);
            }
            "data" => {
                self.install_sandbox(child, navigation);
                self.load_data_frame(child, initiator, url, navigation);
            }
            "javascript" => self.load_javascript_frame_url(child, initiator, url),
            "blob" => {
                self.install_sandbox(child, navigation);
                self.load_blob_frame(child, initiator, url, navigation);
            }
            "http" | "https" => {
                // The document the navigation started from supplies both the
                // authorized origin and the client URL. The origin is
                // inherited, not derived from the document URL: an
                // `about:srcdoc` document has no HTTP URL of its own
                // (<https://html.spec.whatwg.org/multipage/browsers.html#concept-origin>).
                let origin = self
                    .frames
                    .get(&initiator)
                    .map_or_else(|| url.origin(), Document::dial_initiator);
                // Top-level navigations send the source document's referrer
                // like subresource requests do; without this every
                // navigation is a no-referrer navigation regardless of
                // policy
                // (<https://w3c.github.io/webappsec-referrer-policy/#determine-requests-referrer>).
                let (referrer, referrer_policy) = self.navigation_referrer(initiator, navigation);
                // The embedder's `frame-src` gate re-checks every redirect hop,
                // and the ancestor URLs back the child response's
                // `frame-ancestors` check at commit — both only when the
                // embedder's gate applies
                // (<https://w3c.github.io/webappsec-csp/#frame-ancestors-navigation-response>).
                let (embedder, gate, ancestors) =
                    if let (Some(embedder), Some(gate)) = (embedder, frame_gate) {
                        (
                            Some(embedder),
                            Some(gate),
                            self.ancestor_urls(Some(embedder)),
                        )
                    } else {
                        (None, None, Vec::new())
                    };
                // The environment's creator bit travels with the navigation:
                // a frame's document inherits it even when its own URL is
                // trustworthy
                // (<https://html.spec.whatwg.org/multipage/webappapis.html#secure-contexts>).
                let creator_secure = self
                    .frames
                    .get(&initiator)
                    .map(|document| document.world().borrow().policy.secure_context);
                // A nested document is cross-origin isolated only when its
                // embedder is, so the embedder's capability travels with the
                // navigation
                // (<https://html.spec.whatwg.org/multipage/webappapis.html#concept-settings-object-cross-origin-isolated-capability>).
                let creator_isolated = self
                    .frames
                    .get(&initiator)
                    .map(|document| document.world().borrow().policy.cross_origin_isolated);
                // A navigation that replaces a top-level document belongs to
                // the destination site's cookie partition, because that is the
                // page the request is for. A nested frame stays in the
                // initiator's partition
                // (<https://github.com/privacycg/CHIPS#computing-the-cookie-partition-key>).
                let top_level_navigation = {
                    let shared = self.runtime.shared.borrow();
                    shared.tree.top_level(child) == child
                };
                let partition_key = if top_level_navigation {
                    net::site_for_origin(&url.origin())
                } else {
                    self.frames
                        .get(&initiator)
                        .and_then(|document| document.world().borrow().partition_key())
                };
                if let Some(document) = self.frames.get_mut(&child) {
                    document.set_creator_secure_context(creator_secure);
                    document.set_creator_cross_origin_isolated(creator_isolated);
                    let mut request = crate::protocol::DialRequest::new(
                        crate::protocol::RequestPolicy::FrameLoad,
                        url,
                        &origin,
                    );
                    request.frame = Some(child);
                    request.partition_key = partition_key;
                    request.referrer = referrer;
                    request.method.clone_from(&navigation.method);
                    request.body.clone_from(&navigation.body);
                    request.content_type.clone_from(&navigation.content_type);
                    request.csp = gate.map(Box::new);
                    request.referrer_policy = referrer_policy;
                    document.navigate(embedder, request, ancestors, navigation.sandbox);
                }
            }
            _ => {
                if let Some(document) = self.frames.get_mut(&child) {
                    document.load_about_blank(None, &about_blank(), None);
                }
                self.inherit_policy(child, initiator);
            }
        }
    }

    /// Navigates to a `javascript:` URL returns when the initiator is not same
    /// origin-domain with the target's active document
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#navigate-to-a-javascript:-url>).
    fn load_javascript_frame_url(&mut self, child: FrameId, initiator: FrameId, url: &Url) {
        let same_origin = match (self.frames.get(&initiator), self.frames.get(&child)) {
            (Some(source), Some(target)) => {
                source.world().borrow().origin == target.world().borrow().origin
            }
            _ => false,
        };
        if !same_origin {
            return;
        }
        let script = percent_decode(url.as_str());
        let script = script
            .strip_prefix("javascript:")
            .unwrap_or(&script)
            .to_owned();
        let source = url.as_str().to_owned();
        let initial = self
            .frames
            .get(&child)
            .is_some_and(Document::is_initial_blank);
        // The inline check for a `javascript:` navigation uses the initiator's
        // policy, not the target frame's, and the violation names the
        // navigable's container element in the initiator's document; the
        // target frame has no listener for it
        // (<https://w3c.github.io/webappsec-csp/#should-block-navigation-request>,
        // <https://w3c.github.io/webappsec-csp/#report-violation>).
        let container = self.runtime.shared.borrow().tree.container(child);
        let violations = self
            .frames
            .get(&initiator)
            .map_or_default(|document| document.javascript_url_violations(&source));
        let blocked = crate::csp::blocks(&violations);
        if !violations.is_empty()
            && let Some(document) = self.frames.get_mut(&initiator)
            && let Some(target) = container.or_else(|| document.document_node())
        {
            let mut violations = violations;
            for violation in &mut violations {
                violation.element = container;
            }
            document.fire_csp_violations(target, &violations);
        }
        if let Some(document) = self.frames.get_mut(&child) {
            if initial {
                // The script runs in the document the browser already created;
                // a string result still replaces it.
                document.eval_frame_script(&script, blocked);
            } else {
                document.load_javascript_frame(&script, blocked);
            }
        }
    }

    /// Loads a frame's `about:srcdoc` document, inheriting the creator's base
    /// URL, origin, and policy container
    /// (<https://html.spec.whatwg.org/multipage/iframe-embed-object.html#attr-iframe-srcdoc>).
    fn load_frame_srcdoc(&mut self, child: FrameId, parent: FrameId, markup: &str) {
        let inherited = self.frames.get(&parent).map(|document| {
            let world = document.world();
            let world = world.borrow();
            (
                document.base_url(),
                world.cookie_url(),
                world.referrer_url.clone(),
                world.origin.clone(),
                world.policy.clone(),
            )
        });
        let Some(document) = self.frames.get_mut(&child) else {
            return;
        };
        if let Some((base, cookie_url, referrer_url, origin, policy)) = inherited {
            document.load_frame_srcdoc(&base, &cookie_url, &referrer_url, origin, &policy, markup);
        } else {
            let blank = about_blank();
            document.load_frame_srcdoc(
                &blank,
                &blank,
                &blank,
                url::Origin::new_opaque(),
                &PolicyContainer::default(),
                markup,
            );
        }
    }

    /// A `data:` frame URL decodes in-process. A malformed URL fails the
    /// navigation and leaves the frame on its current document
    /// (<https://fetch.spec.whatwg.org/#data-url-processor>).
    fn load_data_frame(
        &mut self,
        child: FrameId,
        parent: FrameId,
        url: &Url,
        navigation: &FrameNavigation,
    ) {
        let policy = self.inherited_policy(parent);
        let referrer = self.local_frame_referrer(parent, navigation, url);
        let Some((content_type, body)) = decode_data_url(url.as_str()) else {
            if let Some(document) = self.frames.get_mut(&child) {
                document.load_about_blank(None, &about_blank(), None);
                document.set_document_referrer(referrer);
            }
            self.inherit_policy(child, parent);
            return;
        };
        if let Some(document) = self.frames.get_mut(&child) {
            // A `data:` URL inherits the initiator's policy container
            // (<https://html.spec.whatwg.org/multipage/browsers.html#determining-navigation-params-policy-container>).
            let head = ResponseHead::local(url.clone(), Some(content_type), Some(policy));
            document.set_document_referrer(referrer);
            document.install_frame_response(&head, &body, None);
        }
    }

    /// A `blob:` frame URL reads the parent's Blob URL Store. A missing entry
    /// is a network error. The child's origin is the entry's creator
    /// (<https://w3c.github.io/FileAPI/#blob-url-entry>).
    fn load_blob_frame(
        &mut self,
        child: FrameId,
        parent: FrameId,
        url: &Url,
        navigation: &FrameNavigation,
    ) {
        let mut key = url.clone();
        key.set_fragment(None);
        let loaded = self.frames.get(&parent).and_then(|document| {
            let key = key.as_str();
            Some((
                document.object_url_contents(key)?,
                document.object_url_type(key),
                document.object_url_origin(key)?,
            ))
        });
        let Some((contents, content_type, origin)) = loaded else {
            return;
        };
        let policy = self.inherited_policy(parent);
        let referrer = self.local_frame_referrer(parent, navigation, url);
        if let Some(document) = self.frames.get_mut(&child) {
            // A `blob:` URL inherits the initiator's policy container, and the
            // child's origin is the store entry's creator
            // (<https://html.spec.whatwg.org/multipage/browsers.html#determining-navigation-params-policy-container>).
            let head = ResponseHead::local(
                url.clone(),
                content_type.as_deref().map(String::from),
                Some(policy),
            );
            document.set_document_referrer(referrer);
            document.install_frame_response(&head, contents.as_bytes(), Some(origin));
        }
    }

    /// Records a frame load so the parent's `load` event waits for it
    /// (<https://html.spec.whatwg.org/multipage/parsing.html#delay-the-load-event>).
    /// A later container navigation supersedes the insertion firing, so the
    /// recorded entry is cleared: the navigation's own `load` must fire.
    fn mark_frame_load_pending(&mut self, container: crate::dom::tree::NodeId, parent: FrameId) {
        if let Some(document) = self.frames.get_mut(&parent) {
            document.mark_frame_load_pending(container);
            let _ = document
                .world()
                .borrow_mut()
                .insertion_load_fired
                .remove(&container);
        }
    }

    fn apply_navigations(&mut self, navigations: Vec<(FrameId, FrameNavigation)>) {
        for (queued_on, mut navigation) in navigations {
            // A cross-frame `Location` setter queues on the target but is
            // initiated by the incumbent document: the referrer, the policy
            // container, and the sandbox rules read the source while the
            // target name resolves against the frame the call was made on
            // (<https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-location-href>).
            let initiator = navigation.source.unwrap_or(queued_on);
            let chosen = {
                let shared = self.runtime.shared.borrow();
                let tree = &shared.tree;
                match &navigation.target {
                    NavigationTarget::Container(container) => tree
                        .frame_for_container(*container)
                        .map(|frame| (frame, Some(*container))),
                    NavigationTarget::Frame(frame) => tree
                        .contains(*frame)
                        .then(|| (*frame, tree.container(*frame))),
                    NavigationTarget::Target(target) => tree
                        .choose_navigable(target, queued_on)
                        .map(|frame| (frame, tree.container(frame))),
                }
            };
            let Some((chosen, container)) = chosen else {
                continue;
            };
            if !self.sandbox_allows_navigation(initiator, chosen) {
                continue;
            }
            // The target container's `sandbox` attribute and its embedder's
            // active flags travel with the navigation and take effect when the
            // document commits, so a navigation that fails or is blocked leaves
            // the current document under its own flags
            // (<https://html.spec.whatwg.org/multipage/browsers.html#determining-the-creation-sandboxing-flags>).
            if let Some(container) = container
                && let Some(embedder) = self.runtime.shared.borrow().tree.parent(chosen)
                && let Some(embedder) = self.frames.get(&embedder)
            {
                navigation.sandbox =
                    crate::document::sandboxing_flags(&embedder.world(), container);
            }
            // A navigation whose target is still the initial `about:blank`
            // replaces that entry instead of adding a second one
            // (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#navigate>).
            let replace = self
                .frames
                .get(&chosen)
                .is_some_and(Document::is_initial_blank);
            // Only a load adds an entry to the frame's replay store; a
            // same-document navigation reports its entry from the frame's own
            // realm, which also delivers `hashchange`.
            if self.navigate_frame(chosen, initiator, &navigation) == NavOutcome::Loaded {
                self.record_navigation(chosen, initiator, &navigation, replace);
            }
            if let Some(container) = container {
                self.publish_frame_document(container, chosen);
            }
        }
    }

    fn publish_streams(&mut self, streams: Vec<(FrameId, bool)>) {
        for (frame, closed) in streams {
            let container = self.runtime.shared.borrow().tree.container(frame);
            if let Some(container) = container {
                self.publish_frame_document(container, frame);
                if closed {
                    let parent = self.runtime.shared.borrow().tree.parent(frame);
                    if let Some(parent) = parent {
                        self.mark_frame_load_pending(container, parent);
                    }
                }
            }
        }
    }

    /// Routes finished sends to their target frame's task queue. The message
    /// queues on the document it was posted to, and the intended target origin
    /// is checked when the task runs
    /// ([window post message steps](https://html.spec.whatwg.org/multipage/web-messaging.html#window-post-message-steps)).
    fn apply_deliveries(&mut self, deliveries: Vec<Delivery>) {
        for delivery in deliveries {
            match delivery {
                Delivery::WindowMessage {
                    target,
                    document,
                    source,
                    origin,
                    target_origin,
                    payload,
                    ports,
                } => {
                    // A task is not runnable once its document is not fully
                    // active
                    // (<https://html.spec.whatwg.org/multipage/webappapis.html#concept-task-runnable>).
                    match self.frames.get_mut(&target) {
                        Some(document_now) if document_now.matches_document(document) => {
                            Self::route_endpoints(&self.runtime.shared, &ports, target);
                            document_now.push_window_message(WindowMessage {
                                source,
                                origin,
                                target_origin,
                                payload,
                                ports,
                            });
                        }
                        _ => self.runtime.shared.borrow_mut().forget_endpoints(&ports),
                    }
                }
                Delivery::PortMessage {
                    endpoint,
                    payload,
                    ports,
                } => {
                    let owner = self.runtime.shared.borrow().ports.owner(endpoint);
                    match owner {
                        Some(frame) if self.frames.contains_key(&frame) => {
                            Self::route_endpoints(&self.runtime.shared, &ports, frame);
                            if let Some(document) = self.frames.get_mut(&frame) {
                                document.push_port_message(endpoint, payload, ports);
                            }
                        }
                        // A worker owns its own realm and task queue; the
                        // message queues there, and the task re-checks the
                        // endpoint's owner in case the port moved again
                        // (<https://html.spec.whatwg.org/multipage/web-messaging.html#message-port-post-message-steps>).
                        Some(frame) if frame.as_worker().is_some() => {
                            Self::route_endpoints(&self.runtime.shared, &ports, frame);
                            self.queue_worker_port_message(frame, endpoint, payload, ports);
                        }
                        Some(_) => {
                            // The owning frame is gone; nothing can receive it.
                            self.runtime.shared.borrow_mut().forget_endpoints(&ports);
                        }
                        None => {
                            // The port is in transit; the received realm
                            // flushes its queue when the port is enabled
                            // (<https://html.spec.whatwg.org/multipage/web-messaging.html#transfer-receiving-steps>).
                            self.runtime
                                .shared
                                .borrow_mut()
                                .requeue_port_message(endpoint, payload, ports);
                        }
                    }
                }
                Delivery::PortClosed { endpoint } => {
                    let owner = self.runtime.shared.borrow().ports.owner(endpoint);
                    match owner {
                        Some(frame) if self.frames.contains_key(&frame) => {
                            if let Some(document) = self.frames.get_mut(&frame) {
                                document.push_port_closed(endpoint);
                            }
                        }
                        Some(frame) if frame.as_worker().is_some() => {
                            self.queue_worker_port_closed(frame, endpoint);
                        }
                        Some(_) => {}
                        None => self
                            .runtime
                            .shared
                            .borrow_mut()
                            .requeue_port_close(endpoint),
                    }
                }
            }
        }
    }

    /// Aims every transferred endpoint at `target`, whose realm materializes
    /// it when the carrying message is delivered
    /// (<https://html.spec.whatwg.org/multipage/web-messaging.html#transfer-receiving-steps>).
    fn route_endpoints(shared: &SharedHandle, endpoints: &[u64], target: FrameId) {
        let mut shared = shared.borrow_mut();
        for endpoint in endpoints {
            shared.ports.route(*endpoint, target);
        }
    }

    /// Publishes `frame`'s document root as the active document of
    /// `container`, which is what `contentDocument` resolves through
    /// (<https://html.spec.whatwg.org/multipage/iframe-embed-object.html#dom-iframe-contentdocument>).
    fn publish_frame_document(&mut self, container: crate::dom::tree::NodeId, frame: FrameId) {
        let document = self.frames.get(&frame).and_then(Document::document_root);
        if let Some(document) = document {
            self.runtime
                .registry
                .borrow_mut()
                .set_frame_document(container, document);
        }
    }

    fn fire_ready_frame_loads(&mut self) -> bool {
        // Encoding-label maps append ~100 iframes; skip a realm for that
        // burst. A handful of navigated HTML frames still get a realm
        // before `load` so `contentDocument` wrappers have prototypes
        // (<https://html.spec.whatwg.org/multipage/window-object.html#the-window-object>).
        //
        // The count is global, not per parent: sharding the same iframes
        // across parents must not flip realm creation. Sorted for
        // determinism: without it the first-N cutoff depends on HashMap order
        // and scheduling, silently changing `instanceof` realms run to run.
        const EAGER_FRAME_REALM_CAP: usize = 8;
        let mut frame_ids: Vec<FrameId> = self.frames.keys().copied().collect();
        frame_ids.sort();
        let total_pending: usize = frame_ids
            .iter()
            .filter_map(|parent| self.frames.get(parent))
            .map(|document| document.pending_frame_loads().len())
            .sum();
        let eager_realm = total_pending <= EAGER_FRAME_REALM_CAP;
        let mut fired = false;
        for parent in frame_ids {
            let Some(document) = self.frames.get(&parent) else {
                continue;
            };
            let pending = document.pending_frame_loads();
            for container in pending {
                let Some(child) = self
                    .runtime
                    .shared
                    .borrow()
                    .tree
                    .frame_for_container(container)
                else {
                    if let Some(document) = self.frames.get_mut(&parent) {
                        document.cancel_frame_load(container);
                    }
                    continue;
                };
                let waiting = self
                    .frames
                    .get(&child)
                    .is_none_or(Document::waiting_for_load);
                if waiting {
                    // A frame whose `load` waits for the opener's or parent's
                    // pre-realm `WindowProxy` writes still needs its realm: the
                    // replay targets that realm, and `load` waits for the
                    // replay. Without this the frame would wait for a realm
                    // that only the replay can unblock
                    // (<https://html.spec.whatwg.org/multipage/window-object.html#windowproxy-set>).
                    let awaiting_replay = self
                        .frames
                        .get(&child)
                        .is_some_and(Document::awaits_proxy_replay);
                    if awaiting_replay && let Some(document) = self.frames.get_mut(&child) {
                        document.ensure_js_ok();
                    }
                    continue;
                }
                // A navigated frame always gets its realm before `load`:
                // `contentWindow.location` must be readable in the load
                // handler, and the cap only exists to keep a burst of blank
                // parser-created iframes cheap. A blank frame has no URL of
                // its own to report.
                let navigated = self
                    .frames
                    .get(&child)
                    .is_some_and(|document| !document.is_initial_blank());
                if (eager_realm || navigated)
                    && let Some(document) = self.frames.get_mut(&child)
                {
                    document.ensure_js_ok();
                }
                self.publish_frame_document(container, child);
                if let Some(document) = self.frames.get_mut(&parent) {
                    fired |= document.finish_frame_load(container);
                }
            }
        }
        fired
    }

    pub fn shutdown(&mut self) {
        self.workers.clear();
        self.runtime.worker_dir.terminate_all();
        self.shared_workers.clear();
        for document in self.frames.values_mut() {
            document.shutdown();
        }
    }

    pub fn release(&mut self) {
        self.workers.clear();
        self.runtime.worker_dir.terminate_all();
        self.shared_workers.clear();
        for document in self.frames.values_mut() {
            document.release();
        }
    }
}

/// Decodes a `data:` URL into its MIME type and body bytes, or `None` when
/// the URL is malformed or its base64 payload is not decodable
/// (<https://fetch.spec.whatwg.org/#data-url-processor>).
///
/// An empty MIME defaults to `text/plain;charset=US-ASCII`, as the Fetch
/// processor requires; every success carries a MIME, so callers never see
/// `None` content types from this function.
#[must_use]
pub fn decode_data_url(raw: &str) -> Option<(String, Vec<u8>)> {
    let url = data_url::DataUrl::process(raw).ok()?;
    let (body, _fragment) = url.decode_to_vec().ok()?;
    Some((url.mime_type().to_string(), body))
}

/// Room for `data:` plus a MIME type before the comma.
///
/// A percent-encoded payload uses three characters per body byte (`%XX`).
/// The raw ceiling is three times [`crate::MAX_RESPONSE_BODY_BYTES`] plus this
/// allowance, so a body that fits the cap is not rejected for its encoding.
const DATA_URL_PREFIX_ALLOWANCE: usize = 1024;

/// Decodes a `data:` URL that fits the response body cap.
///
/// The raw-length check runs before decode. A `data:` URL is materialized
/// entirely in this process. Percent-encoding is the long form: one body
/// byte is `%XX`. Base64 is shorter (~4/3). The raw ceiling is three times
/// [`crate::MAX_RESPONSE_BODY_BYTES`] plus `DATA_URL_PREFIX_ALLOWANCE`. The
/// decoded body is then held to the cap. `None` means the raw URL is past
/// the ceiling, the URL is not decodable, or the body is past the cap.
///
/// <https://fetch.spec.whatwg.org/#data-url-processor>
#[must_use]
pub fn decode_data_url_bounded(raw: &str) -> Option<(String, Vec<u8>)> {
    let raw_ceiling = crate::protocol::MAX_RESPONSE_BODY_BYTES
        .saturating_mul(3)
        .saturating_add(DATA_URL_PREFIX_ALLOWANCE);
    if raw.len() > raw_ceiling {
        return None;
    }
    let (content_type, body) = decode_data_url(raw)?;
    if body.len() > crate::protocol::MAX_RESPONSE_BODY_BYTES {
        return None;
    }
    Some((content_type, body))
}

/// Decodes a URL into its UTF-8 text, percent-escapes included.
fn percent_decode(source: &str) -> String {
    percent_encoding::percent_decode_str(source)
        .decode_utf8_lossy()
        .into_owned()
}

/// Capture surface sides: at least 1 and at most the painter cap. A capture
/// is transient, so an out-of-range request is clamped rather than rejected.
fn capture_sides(width: u32, height: u32) -> Result<(u32, u32), TabError> {
    if width == 0 || height == 0 {
        return Err(TabError::Render {
            message: crate::render::RenderError::InvalidViewport.to_string(),
        });
    }
    Ok((
        width.min(crate::render::MAX_VIEWPORT_SIDE),
        height.min(crate::render::MAX_VIEWPORT_SIDE),
    ))
}

impl Drop for Engine {
    fn drop(&mut self) {
        // Drop realms before the wrapper cache so cached JS values never
        // outlive the QuickJS heap. The engine's own runtime handle is the
        // last one and drops with the struct.
        self.workers.clear();
        self.runtime.worker_dir.terminate_all();
        self.shared_workers.clear();
        self.frames.clear();
        self.runtime.registry.borrow_mut().clear();
        self.js_runtime.collect();
    }
}

#[cfg(test)]
mod frame_history_tests {
    use super::{
        FrameHistory, FrameHistoryEntry, FrameId, FrameNavigation, NavigationTarget,
        SameDocumentEntry,
    };

    fn marker(spec: &str) -> FrameHistoryEntry {
        FrameHistoryEntry {
            navigation: Some(FrameNavigation::get(
                NavigationTarget::Frame(FrameId::MAIN),
                spec.to_owned(),
            )),
            state: None,
            same_document: None,
        }
    }

    fn active_spec(history: &FrameHistory) -> String {
        history
            .at(history.index)
            .and_then(|entry| entry.navigation.as_ref())
            .expect("active navigation")
            .spec
            .clone()
    }

    #[test]
    fn replace_overwrites_only_the_active_entry() {
        let mut history = FrameHistory::default();
        for spec in [
            "http://marker.test/zero",
            "http://marker.test/one",
            "http://marker.test/two",
        ] {
            history.push(marker(spec));
        }
        // Step back to the middle entry, then replace it.
        history.index = 1;
        history.replace(marker("http://marker.test/replaced"));
        assert_eq!(active_spec(&history), "http://marker.test/replaced");
        assert_eq!(
            history
                .at(0)
                .and_then(|entry| entry.navigation.as_ref())
                .expect("left neighbour")
                .spec,
            "http://marker.test/zero"
        );
        assert_eq!(
            history
                .at(2)
                .and_then(|entry| entry.navigation.as_ref())
                .expect("right neighbour")
                .spec,
            "http://marker.test/two"
        );
    }

    #[test]
    fn replace_pushes_when_the_history_is_empty() {
        let mut history = FrameHistory::default();
        history.replace(marker("http://marker.test/pushed"));
        assert_eq!(active_spec(&history), "http://marker.test/pushed");
        assert_eq!(history.index, 0);
        assert!(
            history.at(1).is_none(),
            "the replacement is the first and only entry"
        );
    }

    /// A same-document push keeps its own slot so the next document
    /// navigation's browser index and the replay store's position agree, and a
    /// traversal back to it restores its URL and classic state.
    #[test]
    fn a_same_document_push_keeps_positions_aligned() {
        let mut history = FrameHistory::default();
        history.push(marker("http://marker.test/one"));
        history.push_same_document(SameDocumentEntry {
            url: "http://marker.test/one#pushed".to_owned(),
            state: "null".to_owned(),
        });
        history.push(marker("http://marker.test/two"));
        assert_eq!(history.index, 2);
        assert!(history.at(1).expect("placeholder").navigation.is_none());
        assert_eq!(
            history
                .at(1)
                .and_then(|entry| entry.same_document.as_ref())
                .expect("same-document entry")
                .url,
            "http://marker.test/one#pushed"
        );
        assert_eq!(
            history
                .at(2)
                .and_then(|entry| entry.navigation.as_ref())
                .expect("navigation")
                .spec,
            "http://marker.test/two"
        );
    }

    /// An HTTP load is staged until its document commits, so a load that never
    /// commits leaves no entry behind
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#finalize-a-cross-document-navigation>).
    #[test]
    fn a_staged_navigation_is_invisible_until_commit() {
        let mut history = FrameHistory::default();
        history.push(marker("http://marker.test/zero"));
        history.stage(marker("http://marker.test/one"), false);
        assert!(history.at(0).is_some());
        assert!(history.at(1).is_none(), "the stage is not an entry yet");
        assert_eq!(history.entries.len(), 1);

        history.commit_pending();
        assert_eq!(history.entries.len(), 2);
        assert_eq!(active_spec(&history), "http://marker.test/one");
    }

    #[test]
    fn a_discarded_stage_leaves_the_active_entry() {
        let mut history = FrameHistory::default();
        history.push(marker("http://marker.test/zero"));
        history.stage(marker("http://marker.test/never"), true);
        history.discard_pending();
        assert_eq!(history.entries.len(), 1);
        assert_eq!(active_spec(&history), "http://marker.test/zero");
        assert!(history.pending.is_none());
    }
}

#[cfg(test)]
mod navigator_tests {
    use super::{Engine, FrameId, Mount, ResponseHead};
    use crate::test_services::NullServices;
    use std::sync::Arc;
    use url::Url;

    /// One engine whose identity is not the built-in one: whatever the carrier
    /// spawns it with is what every page reports, header and hints together
    /// (<https://wicg.github.io/ua-client-hints/#http-ua-hints>).
    #[test]
    fn navigator_reports_the_identity_the_engine_was_built_with() {
        let identity = net::ValidIdentity::try_from(net::Identity {
            user_agent: net::UserAgent::parse("probe/1").expect("user agent"),
            brands: vec![net::Brand {
                brand: "Probe".to_owned(),
                version: "7".to_owned(),
            }],
            full_version_list: vec![net::Brand {
                brand: "Chromium".to_owned(),
                version: "7.0.0.0".to_owned(),
            }],
            platform: "ProbeOS".to_owned(),
            mobile: true,
            form_factors: vec!["Mobile".to_owned()],
            navigator_platform: "ProbeOS 1.0".to_owned(),
            languages: vec!["fr-FR".to_owned(), "fr".to_owned()],
            ..net::Identity::default()
        })
        .expect("identity");
        let mut engine = Engine::new(
            Arc::new(NullServices::default()),
            Arc::new(crate::js::Stop::new()),
            Arc::new(tokio::sync::Notify::new()),
            identity,
        );
        engine
            .mount_frame(
                FrameId::MAIN,
                &Mount {
                    head: ResponseHead::local(
                        Url::parse("https://example.test/").expect("URL"),
                        Some("text/html".to_owned()),
                        None,
                    ),
                    tab: crate::protocol::TabState::default(),
                    body: b"<!doctype html><title>identity</title>".to_vec(),
                },
            )
            .expect("mount");
        let _ready = engine.drain_ready();
        let mut evaluate = |source: &str| engine.eval_in(FrameId::MAIN, source).expect("evaluate");
        assert_eq!(evaluate("navigator.userAgent"), "probe/1");
        assert_eq!(
            evaluate("JSON.stringify(navigator.userAgentData.brands)"),
            r#"[{"brand":"Probe","version":"7"}]"#
        );
        assert_eq!(evaluate("navigator.userAgentData.mobile"), "true");
        assert_eq!(evaluate("navigator.userAgentData.platform"), "ProbeOS");
        assert_eq!(evaluate("navigator.platform"), "ProbeOS 1.0");
        assert_eq!(evaluate("navigator.language"), "fr-FR");
        assert_eq!(evaluate("navigator.languages.join(',')"), "fr-FR,fr");
        assert_eq!(evaluate("Object.isFrozen(navigator.languages)"), "true");
        assert_eq!(evaluate("navigator.vendor"), "Google Inc.");
        assert_eq!(evaluate("navigator.productSub"), "20030107");
    }
}

#[cfg(test)]
mod data_url_tests {
    use super::decode_data_url_bounded;
    use crate::protocol::MAX_RESPONSE_BODY_BYTES;

    #[test]
    fn accepts_a_small_data_url() {
        let (content_type, body) = decode_data_url_bounded("data:text/plain,hi").expect("decode");
        assert_eq!(content_type, "text/plain");
        assert_eq!(body, b"hi");
    }

    #[test]
    fn rejects_a_malformed_data_url() {
        assert!(decode_data_url_bounded("data:text/plain;base64,!!!!").is_none());
    }

    #[test]
    fn rejects_a_raw_url_past_the_ceiling() {
        let raw_ceiling = MAX_RESPONSE_BODY_BYTES
            .saturating_mul(3)
            .saturating_add(super::DATA_URL_PREFIX_ALLOWANCE);
        let raw = format!("data:text/plain,{}", "x".repeat(raw_ceiling));
        assert!(raw.len() > raw_ceiling);
        assert!(decode_data_url_bounded(&raw).is_none());
    }

    #[test]
    fn rejects_a_decoded_body_past_the_cap() {
        let raw = format!(
            "data:text/plain,{}",
            "x".repeat(MAX_RESPONSE_BODY_BYTES.saturating_add(1))
        );
        assert!(decode_data_url_bounded(&raw).is_none());
    }

    fn percent_encoded(bytes: usize) -> String {
        let mut raw = String::with_capacity("data:text/plain,".len() + bytes.saturating_mul(3));
        raw.push_str("data:text/plain,");
        raw.push_str(&"%00".repeat(bytes));
        raw
    }

    #[test]
    fn accepts_a_percent_encoded_body_just_under_the_cap() {
        let raw = percent_encoded(MAX_RESPONSE_BODY_BYTES - 1);
        let (_content_type, body) = decode_data_url_bounded(&raw).expect("under the cap");
        assert_eq!(body.len(), MAX_RESPONSE_BODY_BYTES - 1);
    }

    #[test]
    fn rejects_a_percent_encoded_body_just_over_the_cap() {
        let raw = percent_encoded(MAX_RESPONSE_BODY_BYTES + 1);
        assert!(decode_data_url_bounded(&raw).is_none());
    }
}

#[cfg(test)]
mod messaging_tests {
    use super::{Engine, FrameId, Mount, ResponseHead};
    use crate::test_services::NullServices;
    use std::sync::Arc;
    use url::Url;

    fn engine() -> Engine {
        Engine::new(
            Arc::new(NullServices::default()),
            Arc::new(crate::js::Stop::new()),
            Arc::new(tokio::sync::Notify::new()),
            net::ValidIdentity::default(),
        )
    }

    fn mount(url: &str, body: &str) -> Mount {
        Mount {
            head: ResponseHead::local(
                Url::parse(url).expect("mount URL"),
                Some("text/html".to_owned()),
                None,
            ),
            tab: crate::protocol::TabState::default(),
            body: body.as_bytes().to_vec(),
        }
    }

    /// A mount with a real HTTP response, so the policy container sees the
    /// response headers.
    fn mount_http(url: &str, body: &str, headers: &[(&str, &str)]) -> Mount {
        let headers: Vec<(String, String)> = headers
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        Mount {
            head: ResponseHead::http(
                200,
                Url::parse(url).expect("mount URL"),
                Some("text/html".to_owned()),
                &headers,
            ),
            tab: crate::protocol::TabState::default(),
            body: body.as_bytes().to_vec(),
        }
    }

    /// An `indexedDB` database opens, upgrades, and stores a record through the
    /// host storage seam. The factory is the same object on every access
    /// (<https://w3c.github.io/IndexedDB/#dom-windoworworkerglobalscope-indexeddb>),
    /// and key comparison orders a number before a string
    /// (<https://w3c.github.io/IndexedDB/#compare-two-keys>).
    #[test]
    fn indexed_db_opens_upgrades_and_reads_a_record() {
        let mut engine = engine();
        engine
            .mount_frame(
                FrameId::MAIN,
                &mount("https://example.test/indexeddb", "<!doctype html><p>idb"),
            )
            .expect("mount");
        let _ready = engine.drain_ready();
        engine
            .eval_in(
                FrameId::MAIN,
                r"
                (function () {
                  window.__idb = null;
                  const open = (name, version, upgrade) => new Promise((resolve, reject) => {
                    const request = indexedDB.open(name, version);
                    request.onupgradeneeded = event => {
                      if (upgrade) upgrade(event.target.result);
                    };
                    request.onsuccess = () => resolve(request.result);
                    request.onerror = () => reject(request.error);
                  });
                  (async function () {
                    try {
                      const db = await open('records', 1, database => {
                        const store = database.createObjectStore('books', { keyPath: 'isbn' });
                        store.put({ isbn: 1, title: 'one' });
                        store.put({ isbn: 2, title: 'two' });
                      });
                      const value = await new Promise((resolve, reject) => {
                        const transaction = db.transaction('books');
                        const request = transaction.objectStore('books').get(1);
                        request.onsuccess = () => resolve(request.result);
                        request.onerror = () => reject(request.error);
                      });
                      db.close();
                      const again = await open('records', 1);
                      const count = await new Promise((resolve, reject) => {
                        const transaction = again.transaction('books');
                        const request = transaction.objectStore('books').count();
                        request.onsuccess = () => resolve(request.result);
                        request.onerror = () => reject(request.error);
                      });
                      const names = Array.from(again.objectStoreNames);
                      again.close();
                      window.__idb = JSON.stringify({
                        title: value.title,
                        count,
                        names,
                        cmp: indexedDB.cmp(1, 'a'),
                        same: indexedDB === self.indexedDB,
                      });
                    } catch (error) {
                      window.__idb = JSON.stringify({ error: error.name + ': ' + error.message });
                    }
                  })();
                })()
                ",
            )
            .expect("start");
        for _ in 0..40 {
            // A zero-delay timer is due only after the clock advances; the
            // realm's task queue is drained between the timers.
            std::thread::sleep(std::time::Duration::from_millis(2));
            let _ready = engine.drain_ready();
            let done = engine
                .eval_in(FrameId::MAIN, "window.__idb !== null")
                .expect("probe");
            if done == "true" {
                break;
            }
        }
        assert_eq!(
            engine
                .eval_in(FrameId::MAIN, "String(window.__idb)")
                .expect("result"),
            r#"{"title":"one","count":2,"names":["books"],"cmp":-1,"same":true}"#
        );
    }

    /// The per-operation guards every object-store write shares: a read-only
    /// transaction refuses a write, an explicit key is invalid with an inline
    /// key path, and a store deleted during the upgrade is inert
    /// (<https://w3c.github.io/IndexedDB/#add-or-put-operation>). `databases()`
    /// snapshots at call time, so a database whose upgrade transaction is still
    /// running is not listed
    /// (<https://w3c.github.io/IndexedDB/#dom-idbfactory-databases>).
    #[test]
    fn indexed_db_enforces_object_store_guards() {
        let mut engine = engine();
        engine
            .mount_frame(
                FrameId::MAIN,
                &mount(
                    "https://example.test/indexeddb-guards",
                    "<!doctype html><p>idb",
                ),
            )
            .expect("mount");
        let _ready = engine.drain_ready();
        engine
            .eval_in(
                FrameId::MAIN,
                r"
                (function () {
                  window.__guards = null;
                  const open = (name, version, upgrade) => new Promise((resolve, reject) => {
                    const request = indexedDB.open(name, version);
                    request.onupgradeneeded = event => {
                      if (upgrade) upgrade(event.target.result);
                    };
                    request.onsuccess = () => resolve(request.result);
                    request.onerror = () => reject(request.error);
                  });
                  (async function () {
                    const errors = { __proto__: null };
                    try {
                      let snapshot;
                      const db = await open('guards', 1, database => {
                        database.createObjectStore('books', { keyPath: 'isbn' });
                        // The upgrade transaction has not committed, so the
                        // database is absent from the call-time snapshot.
                        snapshot = indexedDB.databases();
                      });
                      const empty = (await snapshot).length;
                      try {
                        db.transaction('books').objectStore('books').put({ isbn: 1 });
                      } catch (error) {
                        errors.readonly = error.name;
                      }
                      try {
                        db.transaction('books', 'readwrite').objectStore('books')
                          .put({ isbn: 1 }, 1);
                      } catch (error) {
                        errors.inline = error.name;
                      }
                      db.close();
                      const db2 = await open('guards', 2, database => {
                        database.createObjectStore('auto', { autoIncrement: true })
                          .put({ value: 'first' });
                        const gone = database.createObjectStore('gone');
                        database.deleteObjectStore('gone');
                        try {
                          gone.put({ value: 'x' });
                        } catch (error) {
                          errors.deleted = error.name;
                        }
                      });
                      const key = await new Promise((resolve, reject) => {
                        const request = db2.transaction('auto', 'readwrite')
                          .objectStore('auto').put({ value: 'second' });
                        request.onsuccess = () => resolve(request.result);
                        request.onerror = () => reject(request.error);
                      });
                      try {
                        indexedDB.cmp();
                      } catch (error) {
                        errors.cmp = error.name;
                      }
                      const names = Array.from(db2.objectStoreNames);
                      db2.close();
                      window.__guards = JSON.stringify({ empty, key, names, errors });
                    } catch (error) {
                      window.__guards = JSON.stringify({
                        error: error.name + ': ' + error.message,
                      });
                    }
                  })();
                })()
                ",
            )
            .expect("start");
        for _ in 0..40 {
            std::thread::sleep(std::time::Duration::from_millis(2));
            let _ready = engine.drain_ready();
            let done = engine
                .eval_in(FrameId::MAIN, "window.__guards !== null")
                .expect("probe");
            if done == "true" {
                break;
            }
        }
        assert_eq!(
            engine
                .eval_in(FrameId::MAIN, "String(window.__guards)")
                .expect("result"),
            r#"{"empty":0,"key":2,"names":["auto","books"],"errors":{"readonly":"ReadOnlyError","inline":"DataError","deleted":"InvalidStateError","cmp":"TypeError"}}"#
        );
    }

    /// An undeclared global name throws `ReferenceError` even though the
    /// Window named properties proxy sits on the global's prototype chain. Its
    /// `[[Get]]` trap answers `undefined` for a missing name, so resolution has
    /// to ask `[[HasProperty]]` first, as the object environment record does
    /// (<https://tc39.es/ecma262/#sec-object-environment-records-hasbinding>).
    #[test]
    fn an_undeclared_global_name_throws_a_reference_error() {
        let mut engine = engine();
        engine
            .mount_frame(
                FrameId::MAIN,
                &mount("https://example.test/reference", "<!doctype html><p>probe"),
            )
            .expect("mount");
        let _ready = engine.drain_ready();
        let probe = engine
            .eval_in(
                FrameId::MAIN,
                r"
                (function () {
                  const read = () => {
                    try { return String(noExistingVariable); }
                    catch (error) { return error.name; }
                  };
                  return JSON.stringify({
                    typeofName: typeof noExistingVariable,
                    contains: 'noExistingVariable' in globalThis,
                    read: read(),
                    evalRead: (() => {
                      try { return String(eval('noExistingVariable')); }
                      catch (error) { return error.name; }
                    })(),
                    functionRead: (() => {
                      try { return String(Function('return noExistingVariable')()); }
                      catch (error) { return error.name; }
                    })(),
                    strictRead: (() => {
                      try { return String((function () {
                        'use strict';
                        return noExistingVariable;
                      })()); }
                      catch (error) { return error.name; }
                    })(),
                  });
                })()
                ",
            )
            .expect("probe");
        assert_eq!(
            probe,
            r#"{"typeofName":"undefined","contains":false,"read":"ReferenceError","evalRead":"ReferenceError","functionRead":"ReferenceError","strictRead":"ReferenceError"}"#
        );
    }

    /// A data-URL document has an opaque origin, so `indexedDB` exists but
    /// `open()` and `deleteDatabase()` throw `SecurityError` synchronously and
    /// `databases()` rejects with one
    /// (<https://w3c.github.io/IndexedDB/#dom-idbfactory-open>).
    #[test]
    fn indexed_db_rejects_an_opaque_origin() {
        let mut engine = engine();
        engine
            .mount_frame(
                FrameId::MAIN,
                &mount("data:text/html,<p>opaque", "<!doctype html><p>opaque"),
            )
            .expect("mount");
        let _ready = engine.drain_ready();
        let probe = engine
            .eval_in(
                FrameId::MAIN,
                r"
                (function () {
                  const factory = indexedDB;
                  let openError;
                  let deleteError;
                  try { factory.open('db'); } catch (error) { openError = error.name; }
                  try { factory.deleteDatabase('db'); } catch (error) { deleteError = error.name; }
                  const databases = factory.databases();
                  databases.catch(() => {});
                  return JSON.stringify({
                    factory: Object.prototype.toString.call(factory),
                    openError,
                    deleteError,
                    rejected: databases instanceof Promise,
                    cmp: factory.cmp(1, 2),
                    same: indexedDB === self.indexedDB,
                  });
                })()
                ",
            )
            .expect("probe");
        assert_eq!(
            probe,
            r#"{"factory":"[object IDBFactory]","openError":"SecurityError","deleteError":"SecurityError","rejected":true,"cmp":-1,"same":true}"#
        );
    }

    /// A transferred stream moves; it does not copy. The sender's half is
    /// locked, and the receiver reads the source's chunks through the paired
    /// endpoint. A stream that is not in the transfer list is not serializable
    /// (<https://streams.spec.whatwg.org/#readable-stream-transfer>).
    #[test]
    fn a_transferred_stream_delivers_its_chunks() {
        let mut engine = engine();
        engine
            .mount_frame(
                FrameId::MAIN,
                &mount("https://example.test/stream", "<!doctype html><p>stream"),
            )
            .expect("mount");
        let _ready = engine.drain_ready();
        let probe = engine
            .eval_in(
                FrameId::MAIN,
                r"
                (function () {
                  let notTransferred;
                  try { structuredClone(new ReadableStream()); }
                  catch (error) { notTransferred = error.name; }
                  const source = new ReadableStream({
                    start(controller) {
                      controller.enqueue('one');
                      controller.enqueue('two');
                      controller.close();
                    },
                  });
                  let copy;
                  let cloneError = null;
                  try { copy = structuredClone(source, { transfer: [source] }); }
                  catch (error) { cloneError = error.name + ': ' + error.message; }
                  window.__chunks = [];
                  window.__done = false;
                  window.__error = null;
                  if (copy !== undefined) {
                    (async function () {
                      const reader = copy.getReader();
                      for (;;) {
                        const step = await reader.read();
                        if (step.done) break;
                        window.__chunks.push(step.value);
                      }
                      window.__done = true;
                    })().catch(error => {
                      window.__error = error.name + ': ' + error.message;
                      window.__done = true;
                    });
                  }
                  return JSON.stringify({
                    notTransferred,
                    cloneError,
                    locked: source.locked,
                    prototype: copy !== undefined
                      && Object.getPrototypeOf(copy) === ReadableStream.prototype,
                  });
                })()
                ",
            )
            .expect("transfer");
        assert_eq!(
            probe,
            r#"{"notTransferred":"DataCloneError","cloneError":null,"locked":true,"prototype":true}"#
        );
        for _ in 0..20 {
            let _ready = engine.drain_ready();
            if engine
                .eval_in(FrameId::MAIN, "String(window.__done)")
                .expect("done")
                == "true"
            {
                break;
            }
        }
        assert_eq!(
            engine
                .eval_in(FrameId::MAIN, "String(window.__done)")
                .expect("done"),
            "true",
            "the transferred stream must close after its two chunks"
        );
        assert_eq!(
            engine
                .eval_in(FrameId::MAIN, "String(window.__error)")
                .expect("error"),
            "null",
            "the transferred stream must not error"
        );
        assert_eq!(
            engine
                .eval_in(FrameId::MAIN, "JSON.stringify(window.__chunks)")
                .expect("chunks"),
            r#"["one","two"]"#
        );
    }

    /// Cross-origin isolation needs a secure origin, a `same-origin` opener
    /// policy, and an embedder policy compatible with isolation. A shared
    /// buffer may be cloned only when the sender is isolated
    /// (<https://html.spec.whatwg.org/multipage/structured-data.html#structuredserializeinternal>).
    #[test]
    fn cross_origin_isolation_gates_shared_array_buffer_cloning() {
        let mut isolated = engine();
        isolated
            .mount_frame(
                FrameId::MAIN,
                &mount_http(
                    "https://example.test/isolated",
                    "<!doctype html><p>isolated",
                    &[
                        ("Cross-Origin-Opener-Policy", "same-origin"),
                        ("Cross-Origin-Embedder-Policy", "require-corp"),
                    ],
                ),
            )
            .expect("mount");
        let _ready = isolated.drain_ready();
        let probe = isolated
            .eval_in(
                FrameId::MAIN,
                r"
                (function () {
                  const copy = structuredClone(new SharedArrayBuffer(8));
                  return JSON.stringify({
                    crossOriginIsolated: self.crossOriginIsolated,
                    secure: self.isSecureContext,
                    bytes: copy.byteLength,
                    shared: copy instanceof SharedArrayBuffer,
                  });
                })()
                ",
            )
            .expect("clone");
        assert_eq!(
            probe,
            r#"{"crossOriginIsolated":true,"secure":true,"bytes":8,"shared":true}"#
        );

        let mut open = engine();
        open.mount_frame(
            FrameId::MAIN,
            &mount_http(
                "https://example.test/open",
                "<!doctype html><p>open",
                &[("Cross-Origin-Embedder-Policy", "require-corp")],
            ),
        )
        .expect("mount");
        let _ready = open.drain_ready();
        let probe = open
            .eval_in(
                FrameId::MAIN,
                r"
                (function () {
                  let error;
                  try { structuredClone(new SharedArrayBuffer(8)); }
                  catch (caught) { error = caught.name + ':' + caught.code; }
                  return JSON.stringify({
                    crossOriginIsolated: self.crossOriginIsolated,
                    error,
                  });
                })()
                ",
            )
            .expect("clone");
        assert_eq!(
            probe,
            r#"{"crossOriginIsolated":false,"error":"DataCloneError:25"}"#
        );

        let mut insecure = engine();
        insecure
            .mount_frame(
                FrameId::MAIN,
                &mount_http(
                    "http://example.test/insecure",
                    "<!doctype html><p>insecure",
                    &[
                        ("Cross-Origin-Opener-Policy", "same-origin"),
                        ("Cross-Origin-Embedder-Policy", "require-corp"),
                    ],
                ),
            )
            .expect("mount");
        let _ready = insecure.drain_ready();
        let probe = insecure
            .eval_in(
                FrameId::MAIN,
                "JSON.stringify({ crossOriginIsolated: self.crossOriginIsolated, secure: self.isSecureContext })",
            )
            .expect("probe");
        assert_eq!(
            probe, r#"{"crossOriginIsolated":false,"secure":false}"#,
            "a policy header cannot isolate an insecure origin"
        );
    }

    #[test]
    fn message_posted_before_navigation_never_fires_in_the_replacement_document() {
        let mut engine = engine();
        engine
            .mount_frame(
                FrameId::MAIN,
                &mount("https://example.test/one", "<!doctype html><p>one"),
            )
            .expect("mount");
        let _ready = engine.drain_ready();
        // Post through the real host path: the delivery captures the target
        // window's document at post time
        // (<https://html.spec.whatwg.org/multipage/webappapis.html#queue-a-global-task>).
        engine
            .eval_in(FrameId::MAIN, "postMessage('queued', '*')")
            .expect("post");
        // A navigation commits before the engine routes the delivery: the new
        // document listens for `message`, and the sender's message must not
        // reach it.
        engine
            .mount_frame(
                FrameId::MAIN,
                &mount(
                    "https://example.test/two",
                    "<!doctype html><script>window.__ready = true; window.__hits = 0; window.onmessage = function () { window.__hits++; };</script>",
                ),
            )
            .expect("replace");
        let _ready = engine.drain_ready();
        assert_eq!(
            engine
                .eval_in(FrameId::MAIN, "String(window.__hits)")
                .expect("hits"),
            "0",
            "the replacement document must not receive the old document's message"
        );
        assert_eq!(
            engine
                .eval_in(FrameId::MAIN, "String(window.__ready)")
                .expect("ready"),
            "true",
            "the new document must have run its script"
        );
    }

    #[test]
    fn remote_message_checks_the_target_origin_and_reports_the_sender_origin() {
        let mut engine = engine();
        engine
            .mount_frame(
                FrameId::MAIN,
                &mount(
                    "https://example.test/",
                    "<!doctype html><script>window.__hits = 0; window.__origin = ''; window.onmessage = function (event) { window.__hits++; window.__origin = event.origin; };</script>",
                ),
            )
            .expect("mount");
        let _ready = engine.drain_ready();
        let payload = r#"tb1:{"root":0,"nodes":[["string","cross-tab"]]}"#.to_owned();
        // A target origin that names a different document is checked when the
        // task runs, so the message never reaches this realm.
        engine.receive_remote_window_message(crate::protocol::PostedMessage {
            origin: crate::protocol::SerializedOrigin::parse("https://other.test").expect("origin"),
            target_origin: crate::protocol::TargetOrigin::parse("https://different.test", None)
                .expect("target"),
            payload: payload.clone(),
        });
        let _ready = engine.drain_ready();
        assert_eq!(
            engine
                .eval_in(FrameId::MAIN, "String(window.__hits)")
                .expect("hits"),
            "0",
            "a mismatched target origin must not deliver"
        );
        // The matching target origin delivers, and `event.origin` is the
        // sender's origin at post time, not an empty string.
        engine.receive_remote_window_message(crate::protocol::PostedMessage {
            origin: crate::protocol::SerializedOrigin::parse("https://other.test").expect("origin"),
            target_origin: crate::protocol::TargetOrigin::parse("https://example.test", None)
                .expect("target"),
            payload,
        });
        let _ready = engine.drain_ready();
        assert_eq!(
            engine
                .eval_in(FrameId::MAIN, "String(window.__hits)")
                .expect("hits"),
            "1",
            "the matching target origin must deliver"
        );
        assert_eq!(
            engine
                .eval_in(FrameId::MAIN, "window.__origin")
                .expect("origin"),
            "https://other.test",
            "the delivered event carries the sender's origin"
        );
    }

    #[test]
    fn slash_target_origin_resolves_to_the_sender_origin_with_its_identity() {
        let mut engine = engine();
        // A `data:` document's origin is opaque, and `/` on an opaque sender
        // still delivers to the sender itself: same origin with itself.
        engine
            .mount_frame(
                FrameId::MAIN,
                &mount(
                    "data:text/html,opaque",
                    "<!doctype html><script>window.__hits = 0; window.onmessage = function () { window.__hits++; };</script>",
                ),
            )
            .expect("mount");
        engine
            .eval_in(FrameId::MAIN, "window.postMessage('self', '/');")
            .expect("post");
        let _ready = engine.drain_ready();
        assert_eq!(
            engine
                .eval_in(FrameId::MAIN, "String(window.__hits)")
                .expect("hits"),
            "1",
            "`/` must resolve to the sender's origin, and a document is same origin with itself"
        );
    }

    /// A standalone carrier owns the joint steps, so a main-frame
    /// `history.back()` moves every frame whose entry changes at the target
    /// step
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#traverse-the-history-by-a-delta>).
    #[test]
    fn a_standalone_main_step_restores_a_child_entry() {
        let mut engine = engine();
        engine.use_standalone_history();
        engine
            .mount_frame(
                FrameId::MAIN,
                &mount(
                    "https://example.test/",
                    "<!doctype html><iframe srcdoc=\"<p>one</p>\"></iframe>",
                ),
            )
            .expect("mount");
        let _ready = engine.drain_ready();
        engine
            .eval_in(
                FrameId::MAIN,
                "document.querySelector('iframe').srcdoc = '<p>two</p>'",
            )
            .expect("second load");
        let _ready = engine.drain_ready();
        let body = |engine: &mut Engine| {
            engine
                .eval_in(
                    FrameId::MAIN,
                    "document.querySelector('iframe').contentDocument.body.innerHTML",
                )
                .expect("child body")
        };
        assert_eq!(body(&mut engine), "<p>two</p>");
        // The main frame presents the same entry at both steps, so only the
        // child moves. A step that tried to move the main frame would be
        // refused instead of moving half the tab.
        engine.traverse_embedded_history(FrameId::MAIN, -1);
        let _ready = engine.drain_ready();
        assert_eq!(body(&mut engine), "<p>one</p>");
        assert_eq!(
            engine
                .eval_in(FrameId::MAIN, "String(history.length)")
                .expect("length"),
            "2",
            "the mirror counts joint steps, not one frame's entries"
        );
    }

    #[test]
    fn an_about_blank_navigation_keeps_its_own_history_entry() {
        let mut engine = engine();
        engine
            .mount_frame(
                FrameId::MAIN,
                &mount(
                    "http://a.test/",
                    "<!doctype html><iframe name=f srcdoc=\"<p>one</p>\"></iframe>",
                ),
            )
            .expect("mount");
        let _ready = engine.drain_ready();
        // Navigating by name to `about:blank` is a navigation like any other:
        // it adds a session history entry instead of replacing the frame's
        // initial blank, so going back returns to it
        // (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#session-history-entry>).
        engine
            .eval_in(FrameId::MAIN, "window.open('about:blank', 'f')")
            .expect("blank navigation");
        let _ready = engine.drain_ready();
        engine
            .eval_in(
                FrameId::MAIN,
                "document.querySelector('iframe').srcdoc = '<p>two</p>'",
            )
            .expect("srcdoc navigation");
        let _ready = engine.drain_ready();
        let child = engine
            .frames
            .keys()
            .copied()
            .find(|frame| *frame != FrameId::MAIN)
            .expect("child frame");
        assert_eq!(
            engine
                .frame_history
                .get(&child)
                .expect("child history")
                .entries
                .len(),
            3,
            "the first srcdoc load, the blank navigation, and the second srcdoc load are three replay entries"
        );
        // The browser owns `history.length`; the renderer only mirrors the
        // snapshot it sends. One main entry plus three child entries is four.
        let active = crate::protocol::HistorySnapshot {
            length: 4,
            index: 2,
            local_length: 3,
            local_index: 2,
            state: None,
        };
        engine.set_history(child, &active);
        let iframe = "document.querySelector('iframe').contentWindow";
        assert_eq!(
            engine
                .eval_in(FrameId::MAIN, &format!("String({iframe}.history.length)"))
                .expect("length"),
            "4",
            "the mirror reports the joint length the browser sent"
        );
        // A browser traversal to the blank entry restores the blank document
        // the navigation created.
        let blank = crate::protocol::HistorySnapshot {
            length: 4,
            index: 1,
            local_length: 3,
            local_index: 1,
            state: None,
        };
        engine
            .traverse_frame_history(child, 1, "about:blank", &blank, false, false)
            .expect("traverse");
        let _ready = engine.drain_ready();
        assert_eq!(
            engine
                .eval_in(FrameId::MAIN, &format!("String({iframe}.location.href)"))
                .expect("href"),
            "about:blank",
            "going back restores the blank document the navigation created"
        );
    }

    /// A script that pushes and then goes back queues a traversal that must
    /// follow the push it just made, not overtake it
    /// (<https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-history-back>).
    #[test]
    fn a_child_push_state_precedes_the_back_it_queues() {
        let mut engine = engine();
        engine
            .mount_frame(
                FrameId::MAIN,
                &mount("https://example.test/", "<!doctype html><iframe></iframe>"),
            )
            .expect("mount");
        let _ready = engine.drain_ready();
        let child = engine
            .frames
            .keys()
            .copied()
            .find(|frame| *frame != FrameId::MAIN)
            .expect("child frame");
        // A `blob:` document keeps the parent's tuple origin, so `pushState`
        // is the same-origin call this test needs
        // (<https://w3c.github.io/FileAPI/#blob-url-entry-environment>).
        engine
            .eval_in(
                FrameId::MAIN,
                "const f = document.querySelector('iframe');\
                 f.src = URL.createObjectURL(new Blob(['<p>one</p>'], {type: 'text/html'}));",
            )
            .expect("blob load");
        let _ready = engine.drain_ready();
        let outcome = engine
            .eval_in(
                FrameId::MAIN,
                "const w = document.querySelector('iframe').contentWindow;\
                 let r; try { w.history.pushState({}, '', '#pushed'); w.history.back(); r = 'ok'; }\
                 catch (e) { r = String(e); } r",
            )
            .expect("script");
        assert_eq!(outcome, "ok", "the child history calls succeeded");
        let _ready = engine.drain_ready();
        let events = engine.take_browser_events().expect("events");
        let history: Vec<&crate::protocol::BrowserEvent> = events
            .iter()
            .filter(|(frame, _)| *frame == child)
            .map(|(_, event)| event)
            .filter(|event| {
                matches!(
                    event,
                    crate::protocol::BrowserEvent::HistoryUpdated { .. }
                        | crate::protocol::BrowserEvent::HistoryTraversal { .. }
                )
            })
            .collect();
        assert!(
            matches!(
                history.first(),
                Some(crate::protocol::BrowserEvent::HistoryUpdated {
                    handling: crate::protocol::HistoryHandling::Push,
                    ..
                })
            ),
            "the push reaches the browser before the back: {history:?}"
        );
        assert!(
            matches!(
                history.get(1),
                Some(crate::protocol::BrowserEvent::HistoryTraversal { delta: -1 })
            ),
            "the back follows it: {history:?}"
        );
    }

    /// A forward-branch clear reaches every frame's replay store, so a later
    /// traversal does not name an entry the joint list dropped
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#clear-the-forward-session-history>).
    #[test]
    fn a_history_sync_truncates_a_cleared_forward_branch() {
        let mut engine = engine();
        engine
            .mount_frame(
                FrameId::MAIN,
                &mount(
                    "https://example.test/",
                    "<!doctype html><iframe srcdoc=\"<p>one</p>\"></iframe>",
                ),
            )
            .expect("mount");
        let _ready = engine.drain_ready();
        let child = engine
            .frames
            .keys()
            .copied()
            .find(|frame| *frame != FrameId::MAIN)
            .expect("child frame");
        engine
            .eval_in(
                FrameId::MAIN,
                "document.querySelector('iframe').srcdoc = '<p>two</p>'",
            )
            .expect("second load");
        let _ready = engine.drain_ready();
        assert_eq!(
            engine
                .frame_history
                .get(&child)
                .expect("history")
                .entries
                .len(),
            2
        );

        engine.set_history(
            child,
            &crate::protocol::HistorySnapshot {
                length: 2,
                index: 0,
                local_length: 1,
                local_index: 0,
                state: None,
            },
        );
        let history = engine.frame_history.get(&child).expect("history");
        assert_eq!(history.entries.len(), 1, "the cleared entry is gone");
        assert_eq!(history.index, 0);
    }

    /// Several mutations made by separate tasks in one batch reach the next
    /// rendering opportunity as one style/layout pass, not one per task.
    #[test]
    fn mutations_in_one_batch_settle_once() {
        let mut engine = engine();
        engine
            .mount_frame(
                FrameId::MAIN,
                &mount(
                    "https://example.test/",
                    "<!doctype html><div id=d style=\"width:1px;height:1px\"></div>",
                ),
            )
            .expect("mount");
        let _ready = engine.drain_ready();
        engine
            .frames
            .get_mut(&FrameId::MAIN)
            .expect("main frame")
            .reset_blitz_resolve_passes();
        // Fifty timer callbacks run as fifty separate tasks before the engine
        // goes idle; a per-task settle would cost fifty passes.
        engine
            .eval_in(
                FrameId::MAIN,
                "var d = document.getElementById('d'); \
                 for (let i = 0; i < 50; i++) { \
                   setTimeout(() => { d.style.width = (i + 2) + 'px'; }, 0); \
                 }",
            )
            .expect("schedule mutations");
        let _ready = engine.drain_ready();
        assert_eq!(
            engine
                .frames
                .get(&FrameId::MAIN)
                .expect("main frame")
                .blitz_resolve_passes(),
            1,
            "fifty tasks' mutations before the next paint must share one resolve"
        );
    }

    /// `WebAssembly.Memory` owns a real shared block, and `grow()` extends it
    /// in place so a view taken before the growth sees the new length
    /// (<https://webassembly.github.io/spec/js-api/#dom-memory-grow>).
    #[test]
    fn web_assembly_memory_grow_extends_a_shared_buffer_in_place() {
        let mut engine = engine();
        engine
            .mount_frame(
                FrameId::MAIN,
                &mount("https://example.test/memory", "<!doctype html><p>memory"),
            )
            .expect("mount");
        let _ready = engine.drain_ready();
        let probe = engine
            .eval_in(
                FrameId::MAIN,
                r"
                (function () {
                  const memory = new WebAssembly.Memory({shared: true, initial: 1, maximum: 2});
                  const buffer = memory.buffer;
                  const view = new Uint8Array(buffer);
                  view[0] = 7;
                  const previous = memory.grow(1);
                  let overflow;
                  try { memory.grow(1); } catch (error) { overflow = error.name; }
                  let missingMaximum;
                  try { new WebAssembly.Memory({shared: true, initial: 1}); }
                  catch (error) { missingMaximum = error.name; }
                  return JSON.stringify({
                    previous,
                    sameBuffer: memory.buffer === buffer,
                    shared: buffer instanceof SharedArrayBuffer,
                    growable: buffer.growable,
                    byteLength: memory.buffer.byteLength,
                    viewLength: view.length,
                    firstByte: view[0],
                    overflow,
                    missingMaximum,
                  });
                })()
                ",
            )
            .expect("probe");
        assert_eq!(
            probe,
            r#"{"previous":1,"sameBuffer":true,"shared":true,"growable":true,"byteLength":131072,"viewLength":131072,"firstByte":7,"overflow":"RangeError","missingMaximum":"TypeError"}"#
        );
    }

    /// A non-shared memory's buffer is fixed-length: `grow()` detaches the old
    /// buffer and installs a new fixed one, so `resizable` stays false and
    /// `maxByteLength` equals `byteLength` even when the memory has room to
    /// grow (<https://webassembly.github.io/spec/js-api/#ref-for-refresh-the-memory-buffer>).
    #[test]
    fn web_assembly_memory_grow_replaces_a_fixed_buffer() {
        let mut engine = engine();
        engine
            .mount_frame(
                FrameId::MAIN,
                &mount("https://example.test/memory", "<!doctype html><p>memory"),
            )
            .expect("mount");
        let _ready = engine.drain_ready();
        let probe = engine
            .eval_in(
                FrameId::MAIN,
                r"
                (function () {
                  const memory = new WebAssembly.Memory({initial: 1, maximum: 2});
                  const buffer = memory.buffer;
                  new Uint8Array(buffer)[0] = 9;
                  const resizable = buffer.resizable;
                  const maxByteLength = buffer.maxByteLength;
                  const previous = memory.grow(1);
                  const after = memory.buffer;
                  let overflow;
                  try { memory.grow(1); } catch (error) { overflow = error.name; }
                  const fixed = new WebAssembly.Memory({initial: 1, maximum: 1});
                  const fixedBuffer = fixed.buffer;
                  const zero = fixed.grow(0);
                  return JSON.stringify({
                    previous,
                    resizable,
                    maxByteLength,
                    oldByteLength: buffer.byteLength,
                    oldDetached: buffer.detached,
                    byteLength: after.byteLength,
                    firstByte: new Uint8Array(after)[0],
                    resizableAfter: after.resizable,
                    overflow,
                    zero,
                    zeroReplaced: fixed.buffer !== fixedBuffer,
                    zeroDetached: fixedBuffer.detached,
                    zeroByteLength: fixed.buffer.byteLength,
                  });
                })()
                ",
            )
            .expect("probe");
        assert_eq!(
            probe,
            r#"{"previous":1,"resizable":false,"maxByteLength":65536,"oldByteLength":0,"oldDetached":true,"byteLength":131072,"firstByte":9,"resizableAfter":false,"overflow":"RangeError","zero":1,"zeroReplaced":true,"zeroDetached":true,"zeroByteLength":65536}"#
        );
    }

    /// The descriptor follows the memory type's bounds and address type:
    /// `i32` truncates a fraction, negatives and missing fields are type
    /// errors, and a page count past the limit is a range error. An `i64`
    /// memory takes `BigInt` values and returns one from `grow()`.
    #[test]
    fn web_assembly_memory_validates_its_descriptor() {
        let mut engine = engine();
        engine
            .mount_frame(
                FrameId::MAIN,
                &mount("https://example.test/memory", "<!doctype html><p>memory"),
            )
            .expect("mount");
        let _ready = engine.drain_ready();
        let probe = engine
            .eval_in(
                FrameId::MAIN,
                r"
                (function () {
                  const name = (make) => {
                    try { make(); return 'none'; } catch (error) { return error.name; }
                  };
                  const memory = new WebAssembly.Memory({initial: 1n, maximum: 2n, address: 'i64'});
                  const grown = memory.grow(1n);
                  return JSON.stringify({
                    maximumBelowInitial: name(() => new WebAssembly.Memory({initial: 2, maximum: 1})),
                    tooManyPages: name(() => new WebAssembly.Memory({initial: 65537})),
                    negative: name(() => new WebAssembly.Memory({initial: -1})),
                    missingInitial: name(() => new WebAssembly.Memory({maximum: 1})),
                    sharedWithoutMaximum: name(() => new WebAssembly.Memory({shared: true, initial: 1})),
                    nonObject: name(() => new WebAssembly.Memory(null)),
                    fraction: new WebAssembly.Memory({initial: 1.9}).buffer.byteLength,
                    numberForI64: name(() => new WebAssembly.Memory({initial: 1, address: 'i64'})),
                    grown: String(grown),
                    i64ByteLength: memory.buffer.byteLength,
                    tag: Object.prototype.toString.call(memory),
                  });
                })()
                ",
            )
            .expect("probe");
        assert_eq!(
            probe,
            r#"{"maximumBelowInitial":"RangeError","tooManyPages":"RangeError","negative":"TypeError","missingInitial":"TypeError","sharedWithoutMaximum":"TypeError","nonObject":"TypeError","fraction":65536,"numberForI64":"TypeError","grown":"1","i64ByteLength":131072,"tag":"[object WebAssembly.Memory]"}"#
        );
    }
}
