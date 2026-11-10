//! Browser-service doubles shared by the renderer's unit tests.

use url::Url;

use crate::protocol::{
    BrowsingContextHost, DialCancellation, DialCompletion, DialFailure, DialRequest, FrameId,
    MessagingHost, NetworkHost, StorageChange, StorageError, StorageHost, StorageKind,
};

/// Services that answer every host call with its empty or failing result.
pub(crate) struct NullServices;

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
        _kind: StorageKind,
        _origin: &crate::protocol::StorageKey,
        _key: &str,
    ) -> Option<String> {
        None
    }

    fn storage_keys(
        &self,
        _kind: StorageKind,
        _origin: &crate::protocol::StorageKey,
    ) -> Vec<String> {
        Vec::new()
    }

    fn storage_set(
        &self,
        _kind: StorageKind,
        _origin: &crate::protocol::StorageKey,
        _url: &str,
        _key: &str,
        _value: &str,
        _source: FrameId,
    ) -> Result<Option<StorageChange>, StorageError> {
        Ok(None)
    }

    fn storage_remove(
        &self,
        _kind: StorageKind,
        _origin: &crate::protocol::StorageKey,
        _url: &str,
        _key: &str,
        _source: FrameId,
    ) -> Option<StorageChange> {
        None
    }

    fn storage_clear(
        &self,
        _kind: StorageKind,
        _origin: &crate::protocol::StorageKey,
        _url: &str,
        _source: FrameId,
    ) -> Option<StorageChange> {
        None
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
