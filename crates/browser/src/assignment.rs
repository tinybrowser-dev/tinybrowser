use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};

use tokio::sync::mpsc;

use crate::actor::TabId;
use crate::link::{RendererHandle, ResponseWriter};
use crate::network::TabNetworkHandle;
use crate::site::Site;
use crate::wire::{Command as RendererCommand, RendererAssignmentId, Reply};
use renderer::{BrowserEvent, FrameId, Mount, TabError};

const EVENT_SUBSCRIBER_CAPACITY: usize = 4096;

#[derive(Clone, Debug)]
pub(crate) enum RendererWake {
    Event(FrameId, BrowserEvent),
    WaiterSettled(u64),
}

pub(crate) struct Assignment {
    id: RendererAssignmentId,
    process: Arc<RendererHandle>,
    registry: Arc<AssignmentRegistry>,
    pub(crate) tab: TabId,
    pub(crate) site: Site,
    pub(crate) network: TabNetworkHandle,
    subscribers: Mutex<Vec<mpsc::Sender<RendererWake>>>,
}

impl Assignment {
    #[must_use]
    pub(crate) fn id(&self) -> RendererAssignmentId {
        self.id
    }

    #[must_use]
    pub(crate) fn hosted_by(&self, renderer: crate::manager::RendererId) -> bool {
        self.process.id == renderer
    }

    pub(crate) async fn request(&self, command: RendererCommand) -> Result<Reply, TabError> {
        self.process.request(self.id, command).await
    }

    pub(crate) async fn request_bytes(
        &self,
        command: RendererCommand,
    ) -> Result<Vec<u8>, TabError> {
        self.process.request_bytes(self.id, command).await
    }

    pub(crate) async fn mount(&self, frame: FrameId, mount: Mount) -> Result<Reply, TabError> {
        self.process.mount(self.id, frame, mount).await
    }

    pub(crate) async fn start_response(
        &self,
        frame: FrameId,
        mount: &Mount,
    ) -> Result<ResponseWriter, TabError> {
        self.process.start_response(self.id, frame, mount).await
    }

    #[must_use]
    pub(crate) fn subscribe(&self) -> mpsc::Receiver<RendererWake> {
        let (tx, rx) = mpsc::channel(EVENT_SUBSCRIBER_CAPACITY);
        self.subscribers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(tx);
        rx
    }

    pub(crate) fn publish_event(&self, frame: FrameId, event: &BrowserEvent) -> bool {
        self.publish(&RendererWake::Event(frame, event.clone()))
    }

    pub(crate) fn publish_waiter_settled(&self, id: u64) -> bool {
        self.publish(&RendererWake::WaiterSettled(id))
    }

    fn publish(&self, wake: &RendererWake) -> bool {
        let mut saturated = false;
        self.subscribers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|subscriber| match subscriber.try_send(wake.clone()) {
                Ok(()) => true,
                Err(mpsc::error::TrySendError::Closed(_)) => false,
                Err(mpsc::error::TrySendError::Full(_)) => {
                    saturated = true;
                    false
                }
            });
        saturated
    }
}

impl Drop for Assignment {
    fn drop(&mut self) {
        logging::debug!(
            target: "browser::assignment",
            "assignment released: tab={:?} id={:?}",
            self.tab,
            self.id
        );
        self.registry.note_released(self.id);
        self.network.release_streams(self.id);
        self.registry
            .live
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.id);
        self.process.try_notify_release(self.id);
        self.process.assignments.fetch_sub(1, Ordering::Relaxed);
        let _ = self.registry.releases.send(Arc::clone(&self.process));
    }
}

pub(crate) struct AssignmentRegistry {
    live: Mutex<HashMap<RendererAssignmentId, Weak<Assignment>>>,
    next: AtomicU64,
    max_released: AtomicU64,
    releases: mpsc::UnboundedSender<Arc<RendererHandle>>,
}

impl AssignmentRegistry {
    pub(crate) fn new(releases: mpsc::UnboundedSender<Arc<RendererHandle>>) -> Self {
        Self {
            live: Mutex::new(HashMap::new()),
            next: AtomicU64::new(1),
            max_released: AtomicU64::new(0),
            releases,
        }
    }

    pub(crate) fn reserve(
        self: &Arc<Self>,
        process: Arc<RendererHandle>,
        tab: TabId,
        site: Site,
        network: TabNetworkHandle,
    ) -> Arc<Assignment> {
        let id = RendererAssignmentId::new(self.next.fetch_add(1, Ordering::Relaxed));
        let assignment = Arc::new(Assignment {
            id,
            process,
            registry: Arc::clone(self),
            tab,
            site,
            network,
            subscribers: Mutex::new(Vec::new()),
        });
        self.live
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id, Arc::downgrade(&assignment));
        assignment
            .process
            .assignments
            .fetch_add(1, Ordering::Relaxed);
        assignment
    }

    pub(crate) fn resolve(&self, id: RendererAssignmentId) -> Option<Arc<Assignment>> {
        let mut live = self.live.lock().unwrap_or_else(PoisonError::into_inner);
        let weak = live.get(&id)?;
        if let Some(assignment) = weak.upgrade() {
            return Some(assignment);
        }
        live.remove(&id);
        drop(live);
        self.note_released(id);
        None
    }

    pub(crate) fn was_released(&self, id: RendererAssignmentId) -> bool {
        id.get() != 0 && id.get() <= self.max_released.load(Ordering::Acquire)
    }

    fn note_released(&self, id: RendererAssignmentId) {
        self.max_released.fetch_max(id.get(), Ordering::AcqRel);
    }
}
