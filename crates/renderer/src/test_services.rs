use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use url::Url;

use crate::protocol::{
    BrowsingContextHost, DialCancellation, DialCompletion, DialFailure, DialRequest, FrameId,
    MessagingHost, NetworkHost, StorageChange, StorageError, StorageHost, StorageKind,
};

#[derive(Default)]
pub(crate) struct NullServices {
    storage: Mutex<HashMap<(StorageKind, crate::protocol::StorageKey, String), String>>,
    object_urls: webstorage::ObjectUrlStore<()>,
    // https://html.spec.whatwg.org/multipage/nav-history-apis.html#window-open-steps
    local_window: Mutex<Option<crate::protocol::LocalWindowInit>>,
    hanging_dial: Option<(Arc<AtomicBool>, Arc<AtomicBool>)>,
}

impl NullServices {
    pub(crate) fn hanging_dial(started: Arc<AtomicBool>, cancelled: Arc<AtomicBool>) -> Self {
        Self {
            hanging_dial: Some((started, cancelled)),
            ..Self::default()
        }
    }

    pub(crate) fn opening_local_window(
        assignment: u64,
        tab: crate::protocol::TabState,
        navigates: bool,
    ) -> Self {
        let services = Self::default();
        *services
            .local_window
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(crate::protocol::LocalWindowInit {
            assignment,
            tab,
            navigates,
        });
        services
    }
}

impl NetworkHost for NullServices {
    fn start_dial(&self, _request: DialRequest, completion: DialCompletion) -> DialCancellation {
        if let Some((started, cancelled)) = &self.hanging_dial {
            started.store(true, Ordering::Release);
            let cancelled = Arc::clone(cancelled);
            return Box::new(move || {
                cancelled.store(true, Ordering::Release);
            });
        }
        completion(Err(DialFailure::Connect));
        Box::new(|| {})
    }
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

    fn remote_session_get(
        &self,
        _window: crate::protocol::WindowId,
        _origin: &crate::protocol::StorageKey,
        _key: &str,
    ) -> Option<String> {
        None
    }

    fn cookies_for(&self, _url: &Url, _partition_key: Option<&str>) -> String {
        String::new()
    }

    fn set_cookie(&self, _value: &str, _url: &Url, _partition_key: Option<&str>) {}

    fn cookie_store_records(
        &self,
        _url: &Url,
        _partition_key: Option<&str>,
    ) -> Vec<crate::protocol::ScriptCookie> {
        Vec::new()
    }

    fn cookie_store_store(
        &self,
        _write: &net::ScriptCookieWrite,
        _url: &Url,
        _partition_key: Option<&str>,
    ) {
    }

    fn create_object_url(
        &self,
        scope: u64,
        origin: &crate::protocol::SerializedOrigin,
        contents: String,
        content_type: String,
    ) -> Option<String> {
        let origin = match origin {
            crate::protocol::SerializedOrigin::Tuple(origin) => Some(origin.clone()),
            crate::protocol::SerializedOrigin::Opaque => None,
        };
        self.object_urls
            .create((), scope, origin, contents, content_type)
    }

    fn resolve_object_url(
        &self,
        scope: u64,
        origin: &crate::protocol::SerializedOrigin,
        url: &str,
    ) -> Option<crate::protocol::BlobRecord> {
        let origin = match origin {
            crate::protocol::SerializedOrigin::Tuple(origin) => Some(origin),
            crate::protocol::SerializedOrigin::Opaque => None,
        };
        self.object_urls.resolve((), scope, origin, url)
    }

    fn revoke_object_url(&self, scope: u64, origin: &crate::protocol::SerializedOrigin, url: &str) {
        let origin = match origin {
            crate::protocol::SerializedOrigin::Tuple(origin) => Some(origin),
            crate::protocol::SerializedOrigin::Opaque => None,
        };
        self.object_urls.revoke((), scope, origin, url);
    }

    fn unload_object_urls(&self, scope: u64) {
        self.object_urls.unload_scope((), scope);
    }
}

impl BrowsingContextHost for NullServices {
    fn window_open(
        &self,
        _url: Option<&Url>,
        _target: &crate::protocol::TargetName,
        _opener: crate::protocol::OpenerPolicy,
        _source: &crate::protocol::NavigationSource,
    ) -> Option<crate::protocol::OpenedWindow> {
        let local = self
            .local_window
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()?;
        Some(crate::protocol::OpenedWindow {
            id: local.tab.id?,
            local: Some(local),
        })
    }

    fn for_assignment(
        &self,
        _assignment: u64,
    ) -> Option<std::sync::Arc<dyn crate::protocol::BrowserServices>> {
        Some(std::sync::Arc::new(Self::default()))
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
