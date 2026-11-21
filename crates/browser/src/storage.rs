// https://html.spec.whatwg.org/multipage/webstorage.html#the-localstorage-attribute
// https://w3c.github.io/IndexedDB/#database

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use net::{
    Clock, CookieJar, CookieOp, CookieRecord, InitiatorKind, RetrievalKind, SameSiteCookiePolicy,
    ScriptCookieWrite,
};
use renderer::{ScriptCookie, StorageChange, StorageError, StorageKey};
use url::Url;
use webstorage::{SESSION_STORAGE_QUOTA_BYTES, StorageArea, TOTAL_STORAGE_QUOTA_BYTES};

use crate::actor::TabId;

// https://httpwg.org/http-extensions/draft-ietf-httpbis-rfc6265bis.html
pub(crate) struct CookieStore {
    jar: Arc<Mutex<CookieJar>>,
    clock: Clock,
}

impl CookieStore {
    pub(crate) fn new(clock: Clock) -> Self {
        Self {
            jar: Arc::new(Mutex::new(CookieJar::default())),
            clock,
        }
    }

    pub(crate) fn jar(&self) -> Arc<Mutex<CookieJar>> {
        Arc::clone(&self.jar)
    }

    fn lock(&self) -> MutexGuard<'_, CookieJar> {
        self.jar.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn fetch_op<'a>(
        &self,
        url: &'a Url,
        origin: &'a url::Origin,
        kind: RetrievalKind,
        partition_key: Option<&'a str>,
    ) -> CookieOp<'a> {
        CookieOp {
            url,
            now: self.clock.now(),
            kind,
            initiator_kind: InitiatorKind::Fetch,
            method_is_safe: true,
            initiator: Some(origin),
            cross_site_redirect: false,
            same_site_policy: SameSiteCookiePolicy::All,
            partition_key,
        }
    }

    pub(crate) fn cookies_for(&self, url: &Url, partition_key: Option<&str>) -> String {
        let origin = url.origin();
        self.lock().cookie_string(self.fetch_op(
            url,
            &origin,
            RetrievalKind::NonHttp,
            partition_key,
        ))
    }

    pub(crate) fn set_cookie(&self, value: &str, url: &Url, partition_key: Option<&str>) {
        let origin = url.origin();
        let _stored = self.lock().store(
            value,
            self.fetch_op(url, &origin, RetrievalKind::NonHttp, partition_key),
        );
    }

    pub(crate) fn cookie_store_records(
        &self,
        url: &Url,
        partition_key: Option<&str>,
    ) -> Vec<ScriptCookie> {
        let origin = url.origin();
        self.lock()
            .records_for(self.fetch_op(url, &origin, RetrievalKind::NonHttp, partition_key))
            .into_iter()
            .map(|record| ScriptCookie::from_record(&record))
            .collect()
    }

    pub(crate) fn cookie_store_store(
        &self,
        write: &ScriptCookieWrite,
        url: &Url,
        partition_key: Option<&str>,
    ) {
        let origin = url.origin();
        self.lock().store_script(
            write,
            self.fetch_op(url, &origin, RetrievalKind::NonHttp, partition_key),
        );
    }

    pub(crate) fn cookie_records(&self, url: &Url) -> Vec<CookieRecord> {
        let origin = url.origin();
        self.lock()
            .records_for(self.fetch_op(url, &origin, RetrievalKind::Http, None))
    }

    pub(crate) fn add_cookie(&self, cookie: &str, url: &Url) -> bool {
        let origin = url.origin();
        self.lock().store(
            cookie,
            self.fetch_op(url, &origin, RetrievalKind::Http, None),
        )
    }

    pub(crate) fn clear(&self) {
        self.lock().clear();
    }

    pub(crate) fn import(&self, records: Vec<CookieRecord>) {
        self.lock().restore(records, self.clock.now());
    }

    pub(crate) fn export(&self) -> Vec<CookieRecord> {
        self.lock().snapshot()
    }
}

#[derive(Default)]
pub(crate) struct StorageAreas {
    areas: Mutex<HashMap<StorageKey, StorageArea>>,
    dirty: AtomicBool,
}

#[derive(Default)]
pub(crate) struct SessionStorage {
    namespaces: Mutex<HashMap<TabId, HashMap<StorageKey, StorageArea>>>,
}

impl SessionStorage {
    pub(crate) fn create(&self, tab: TabId) {
        self.namespaces
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(tab)
            .or_default();
    }

    // https://html.spec.whatwg.org/multipage/document-sequences.html#copy-session-storage
    pub(crate) fn copy(&self, source: TabId, target: TabId) {
        let mut namespaces = self
            .namespaces
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let namespace = namespaces.get(&source).cloned().unwrap_or_default();
        namespaces.insert(target, namespace);
    }

    pub(crate) fn remove_namespace(&self, tab: TabId) {
        self.namespaces
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&tab);
    }

    pub(crate) fn get(&self, tab: TabId, origin: &StorageKey, key: &str) -> Option<String> {
        self.namespaces
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&tab)
            .and_then(|namespace| namespace.get(origin))
            .and_then(|area| area.get(key))
    }

    pub(crate) fn keys(&self, tab: TabId, origin: &StorageKey) -> Vec<String> {
        self.namespaces
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&tab)
            .and_then(|namespace| namespace.get(origin))
            .map_or_default(StorageArea::keys)
    }

    pub(crate) fn set(
        &self,
        tab: TabId,
        origin: &StorageKey,
        key: &str,
        value: &str,
    ) -> Result<Option<StorageChange>, StorageError> {
        let mut namespaces = self
            .namespaces
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let Some(namespace) = namespaces.get_mut(&tab) else {
            return Ok(None);
        };
        let other_used: usize = namespace
            .iter()
            .filter(|(key_origin, _)| *key_origin != origin)
            .map(|(_, area)| area.used_bytes())
            .sum();
        let limit = SESSION_STORAGE_QUOTA_BYTES.saturating_sub(other_used);
        namespace
            .entry(origin.clone())
            .or_default()
            .set_with_limit(key, value, limit)
    }

    pub(crate) fn remove(
        &self,
        tab: TabId,
        origin: &StorageKey,
        key: &str,
    ) -> Option<StorageChange> {
        self.namespaces
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_mut(&tab)?
            .get_mut(origin)?
            .remove(key)
    }

    pub(crate) fn clear(&self, tab: TabId, origin: &StorageKey) -> Option<StorageChange> {
        self.namespaces
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_mut(&tab)?
            .get_mut(origin)?
            .clear()
    }
}

impl StorageAreas {
    // https://html.spec.whatwg.org/multipage/webstorage.html#dom-storage-getitem
    pub(crate) fn get(&self, origin: &StorageKey, key: &str) -> Option<String> {
        self.areas
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(origin)
            .and_then(|area| area.get(key))
    }

    pub(crate) fn keys(&self, origin: &StorageKey) -> Vec<String> {
        self.areas
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(origin)
            .map_or_default(StorageArea::keys)
    }

    // https://html.spec.whatwg.org/multipage/webstorage.html#dom-storage-setitem
    pub(crate) fn set(
        &self,
        origin: &StorageKey,
        key: &str,
        value: &str,
    ) -> Result<Option<StorageChange>, StorageError> {
        let mut areas = self.areas.lock().unwrap_or_else(PoisonError::into_inner);
        let other_used: usize = areas
            .iter()
            .filter(|(key_origin, _)| *key_origin != origin)
            .map(|(_, area)| area.used_bytes())
            .sum();
        let limit = TOTAL_STORAGE_QUOTA_BYTES.saturating_sub(other_used);
        let area = areas.entry(origin.clone()).or_default();
        let change = area.set_with_limit(key, value, limit)?;
        let Some(change) = change else {
            return Ok(None);
        };
        drop(areas);
        self.dirty.store(true, Ordering::SeqCst);
        Ok(Some(change))
    }

    // https://html.spec.whatwg.org/multipage/webstorage.html#dom-storage-removeitem
    pub(crate) fn remove(&self, origin: &StorageKey, key: &str) -> Option<StorageChange> {
        let mut areas = self.areas.lock().unwrap_or_else(PoisonError::into_inner);
        let change = areas.get_mut(origin)?.remove(key)?;
        drop(areas);
        self.dirty.store(true, Ordering::SeqCst);
        Some(change)
    }

    // https://html.spec.whatwg.org/multipage/webstorage.html#dom-storage-clear
    pub(crate) fn clear(&self, origin: &StorageKey) -> Option<StorageChange> {
        let mut areas = self.areas.lock().unwrap_or_else(PoisonError::into_inner);
        let area = areas.get_mut(origin)?;
        let change = area.clear()?;
        drop(areas);
        self.dirty.store(true, Ordering::SeqCst);
        Some(change)
    }

    pub(crate) fn is_dirty(&self) -> bool {
        self.dirty.load(Ordering::SeqCst)
    }

    pub(crate) fn mark_clean(&self) {
        self.dirty.store(false, Ordering::SeqCst);
    }

    pub(crate) fn export(&self) -> Vec<(String, String, String)> {
        let areas = self.areas.lock().unwrap_or_else(PoisonError::into_inner);
        let mut rows = Vec::new();
        for (origin, area) in areas.iter() {
            for (key, value) in area.entries() {
                rows.push((origin.to_string(), key.to_owned(), value.to_owned()));
            }
        }
        rows.sort();
        rows
    }

    pub(crate) fn import(&self, rows: impl IntoIterator<Item = (String, String, String)>) {
        let mut areas = self.areas.lock().unwrap_or_else(PoisonError::into_inner);
        areas.clear();
        for (origin, key, value) in rows {
            let Some(origin) = StorageKey::parse(&origin) else {
                continue;
            };
            let other_used: usize = areas
                .iter()
                .filter(|(key_origin, _)| *key_origin != &origin)
                .map(|(_, area)| area.used_bytes())
                .sum();
            let limit = TOTAL_STORAGE_QUOTA_BYTES.saturating_sub(other_used);
            let area = areas.entry(origin).or_default();
            let _result = area.set_with_limit(&key, &value, limit);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::StorageAreas;
    use renderer::StorageKey;

    #[test]
    fn storage_normalizes_every_spelling_of_one_origin() {
        let storage = StorageAreas::default();
        storage.import(vec![
            (
                "https://example.test/a?b#c".to_owned(),
                "one".to_owned(),
                "1".to_owned(),
            ),
            (
                "https://example.test/".to_owned(),
                "two".to_owned(),
                "2".to_owned(),
            ),
            (
                "about:blank".to_owned(),
                "opaque".to_owned(),
                "3".to_owned(),
            ),
            ("null".to_owned(), "null".to_owned(), "4".to_owned()),
        ]);
        let key = StorageKey::parse("HTTPS://EXAMPLE.TEST").expect("a tuple origin");
        assert_eq!(storage.get(&key, "one").as_deref(), Some("1"));
        assert_eq!(storage.get(&key, "two").as_deref(), Some("2"));
        assert_eq!(storage.get(&key, "opaque"), None);
        assert_eq!(storage.get(&key, "null"), None);

        let rows = storage.export();
        assert_eq!(rows.len(), 2);
        assert!(
            rows.iter()
                .all(|(origin, _, _)| origin == "https://example.test")
        );
    }
}
