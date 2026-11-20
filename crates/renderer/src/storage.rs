use crate::protocol::{FrameId, StorageKey, StorageKind};

#[derive(Clone, Debug)]
pub struct PendingStorageEvent {
    // https://html.spec.whatwg.org/multipage/webstorage.html#concept-storage-broadcast
    pub source: Option<FrameId>,
    pub origin: StorageKey,
    pub kind: StorageKind,
    pub key: Option<String>,
    pub old_value: Option<String>,
    pub new_value: Option<String>,
    pub url: String,
}

impl PendingStorageEvent {
    #[must_use]
    pub fn broadcast(
        origin: StorageKey,
        kind: StorageKind,
        key: Option<String>,
        old_value: Option<String>,
        new_value: Option<String>,
        url: String,
    ) -> Self {
        Self {
            source: None,
            origin,
            kind,
            key,
            old_value,
            new_value,
            url,
        }
    }
}
