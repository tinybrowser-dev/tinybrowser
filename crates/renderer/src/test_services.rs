//! Browser-service doubles shared by the renderer's unit tests.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use url::Url;

use crate::protocol::{
    BrowsingContextHost, DialCancellation, DialCompletion, DialFailure, DialRequest, FrameId,
    MessagingHost, NetworkHost, StorageChange, StorageError, StorageHost, StorageKind,
};

/// Services that answer every network, browsing-context, and messaging call
/// with its empty or failing result. Storage keeps one in-memory area per
/// origin so the storage shims, including `indexedDB`, can be tested end to
/// end.
#[derive(Default)]
pub(crate) struct NullServices {
    storage: Mutex<HashMap<(StorageKind, crate::protocol::StorageKey, String), String>>,
}

impl NetworkHost for NullServices {
    fn start_dial(&self, _request: DialRequest, completion: DialCompletion) -> DialCancellation {
        completion(Err(DialFailure::Connect));
        Box::new(|| {})
    }

    fn cookies_for(&self, _url: &Url, _partition_key: Option<&str>) -> String {
        String::new()
    }

    fn set_cookie(&self, _value: &str, _url: &Url, _partition_key: Option<&str>) {}
}

impl StorageHost for NullServices {
    fn storage_get(
        &self,
        kind: StorageKind,
        origin: &crate::protocol::StorageKey,
        key: &str,
    ) -> Option<String> {
        self.storage
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&(kind, origin.clone(), key.to_owned()))
            .cloned()
    }

    fn storage_keys(&self, kind: StorageKind, origin: &crate::protocol::StorageKey) -> Vec<String> {
        let storage = self.storage.lock().unwrap_or_else(PoisonError::into_inner);
        let mut keys: Vec<String> = storage
            .keys()
            .filter(|(key_kind, key_origin, _)| *key_kind == kind && key_origin == origin)
            .map(|(_, _, key)| key.clone())
            .collect();
        keys.sort();
        keys
    }

    fn storage_set(
        &self,
        kind: StorageKind,
        origin: &crate::protocol::StorageKey,
        _url: &str,
        key: &str,
        value: &str,
        _source: FrameId,
    ) -> Result<Option<StorageChange>, StorageError> {
        let mut storage = self.storage.lock().unwrap_or_else(PoisonError::into_inner);
        let old_value = storage.insert((kind, origin.clone(), key.to_owned()), value.to_owned());
        if old_value.as_deref() == Some(value) {
            return Ok(None);
        }
        Ok(Some(StorageChange {
            key: Some(key.to_owned()),
            old_value,
            new_value: Some(value.to_owned()),
        }))
    }

    fn storage_remove(
        &self,
        kind: StorageKind,
        origin: &crate::protocol::StorageKey,
        _url: &str,
        key: &str,
        _source: FrameId,
    ) -> Option<StorageChange> {
        let mut storage = self.storage.lock().unwrap_or_else(PoisonError::into_inner);
        let old_value = storage.remove(&(kind, origin.clone(), key.to_owned()))?;
        Some(StorageChange {
            key: Some(key.to_owned()),
            old_value: Some(old_value),
            new_value: None,
        })
    }

    fn storage_clear(
        &self,
        kind: StorageKind,
        origin: &crate::protocol::StorageKey,
        _url: &str,
        _source: FrameId,
    ) -> Option<StorageChange> {
        let mut storage = self.storage.lock().unwrap_or_else(PoisonError::into_inner);
        let keys: Vec<(StorageKind, crate::protocol::StorageKey, String)> = storage
            .keys()
            .filter(|(key_kind, key_origin, _)| *key_kind == kind && key_origin == origin)
            .cloned()
            .collect();
        if keys.is_empty() {
            return None;
        }
        for key in &keys {
            storage.remove(key);
        }
        Some(StorageChange {
            key: None,
            old_value: None,
            new_value: None,
        })
    }
}

impl BrowsingContextHost for NullServices {
    fn window_open(
        &self,
        _url: Option<&Url>,
        _target: &crate::protocol::TargetName,
        _opener: crate::protocol::OpenerPolicy,
        _source: &crate::protocol::NavigationSource,
    ) -> Option<crate::protocol::WindowId> {
        None
    }

    fn navigate_named(
        &self,
        _name: &crate::protocol::ContextName,
        _url: Option<&Url>,
        _opener: crate::protocol::OpenerPolicy,
        _source: &crate::protocol::NavigationSource,
    ) -> Option<crate::protocol::WindowId> {
        None
    }

    fn set_window_name(&self, _change: crate::protocol::NameChange) {}

    fn window_close(&self, _window: crate::protocol::WindowId) {}

    fn window_opener(&self) -> Option<crate::protocol::WindowId> {
        None
    }

    fn window_post_message(
        &self,
        _window: crate::protocol::WindowId,
        _message: crate::protocol::PostedMessage,
    ) {
    }

    fn remote_session_get(
        &self,
        _window: crate::protocol::WindowId,
        _origin: &crate::protocol::StorageKey,
        _key: &str,
    ) -> Option<String> {
        None
    }
}

impl MessagingHost for NullServices {
    fn broadcast_post(
        &self,
        _origin: &crate::protocol::StorageKey,
        _name: &str,
        _payload: &str,
        _channel: u64,
    ) {
    }
}
