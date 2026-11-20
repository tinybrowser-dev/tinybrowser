// https://html.spec.whatwg.org/multipage/web-messaging.html#broadcasting-to-other-browsing-contexts

use std::sync::{Mutex, PoisonError};

use renderer::{FrameId, StorageKey, StorageKind};

use crate::wire::RendererAssignmentId;

#[derive(Clone, Debug)]
pub(crate) struct BroadcastMessage {
    pub(crate) origin: StorageKey,
    pub(crate) name: String,
    pub(crate) payload: String,
    pub(crate) source: (RendererAssignmentId, u64),
}

#[derive(Clone, Debug)]
pub(crate) struct StorageBroadcast {
    pub(crate) origin: StorageKey,
    pub(crate) kind: StorageKind,
    pub(crate) key: Option<String>,
    pub(crate) old_value: Option<String>,
    pub(crate) new_value: Option<String>,
    pub(crate) url: String,
    pub(crate) source: (RendererAssignmentId, FrameId),
}

#[derive(Clone, Debug)]
pub(crate) enum ContextEvent {
    Storage(StorageBroadcast),
    Broadcast(BroadcastMessage),
}

pub(crate) type EventSink = Box<dyn Fn(&ContextEvent) -> bool + Send + Sync>;

#[derive(Default)]
pub(crate) struct RendererEventHub {
    listeners: Mutex<Vec<EventSink>>,
}

impl RendererEventHub {
    pub(crate) fn subscribe(&self, sink: EventSink) {
        self.listeners
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(sink);
    }

    pub(crate) fn broadcast(&self, event: &ContextEvent) {
        let mut listeners = self
            .listeners
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        listeners.retain(|listener| listener(event));
    }
}
