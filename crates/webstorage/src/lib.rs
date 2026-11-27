// https://html.spec.whatwg.org/multipage/webstorage.html#the-localstorage-attribute
// https://w3c.github.io/IndexedDB/#database

mod blob;

pub use blob::{BlobOrigin, BlobRecord, MAX_OBJECT_URL_BYTES, ObjectUrlStore, object_url_key};

use std::collections::HashMap;
use std::fmt;
use std::hash::Hash;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};

use serde::{Deserialize, Serialize};
use url::Url;

// https://html.spec.whatwg.org/multipage/webstorage.html#disk-space-2 — spec leaves quota to the user agent; tinybrowser uses 5 MiB per origin, 64 MiB aggregate, 16 MiB per tab session.
pub const STORAGE_QUOTA_BYTES: usize = 5 * 1024 * 1024;
pub const TOTAL_STORAGE_QUOTA_BYTES: usize = 64 * 1024 * 1024;
pub const SESSION_STORAGE_QUOTA_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StorageKind {
    Local,
    Session,
    // https://w3c.github.io/IndexedDB/#database
    IndexedDb,
}

impl StorageKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Session => "session",
            Self::IndexedDb => "indexeddb",
        }
    }

    #[must_use]
    pub fn parse(token: &str) -> Option<Self> {
        match token {
            "local" => Some(Self::Local),
            "session" => Some(Self::Session),
            "indexeddb" => Some(Self::IndexedDb),
            _ => None,
        }
    }
}

// https://html.spec.whatwg.org/multipage/webstorage.html#the-localstorage-attribute
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct StorageKey(url::Origin);

impl StorageKey {
    #[must_use]
    pub fn from_origin(origin: &url::Origin) -> Option<Self> {
        matches!(origin, url::Origin::Tuple(..)).then(|| Self(origin.clone()))
    }

    #[must_use]
    pub fn parse(spec: &str) -> Option<Self> {
        if spec == "null" {
            return None;
        }
        let parsed = Url::parse(spec).ok()?;
        Self::from_origin(&parsed.origin())
    }

    #[must_use]
    pub fn origin(&self) -> &url::Origin {
        &self.0
    }
}

impl fmt::Display for StorageKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0.ascii_serialization())
    }
}

impl Serialize for StorageKey {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for StorageKey {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).ok_or_else(|| serde::de::Error::custom("a storage key is a tuple origin"))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageChange {
    pub key: Option<String>,
    pub old_value: Option<String>,
    pub new_value: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StorageError {
    QuotaExceeded,
}

#[derive(Clone, Debug, Default)]
pub struct StorageArea {
    entries: Vec<(String, String)>,
}

impl StorageArea {
    // https://html.spec.whatwg.org/multipage/webstorage.html#dom-storage-getitem
    #[must_use]
    pub fn get(&self, key: &str) -> Option<String> {
        self.entries
            .iter()
            .find(|(stored, _)| stored == key)
            .map(|(_, value)| value.clone())
    }

    // https://html.spec.whatwg.org/multipage/webstorage.html#dom-storage-key
    #[must_use]
    pub fn keys(&self) -> Vec<String> {
        self.entries.iter().map(|(key, _)| key.clone()).collect()
    }

    pub fn entries(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
    }

    #[must_use]
    pub fn used_bytes(&self) -> usize {
        self.entries
            .iter()
            .map(|(stored_key, stored_value)| stored_key.len() + stored_value.len())
            .sum()
    }

    // https://html.spec.whatwg.org/multipage/webstorage.html#dom-storage-setitem
    pub fn set(&mut self, key: &str, value: &str) -> Result<Option<StorageChange>, StorageError> {
        self.set_with_limit(key, value, STORAGE_QUOTA_BYTES)
    }

    // https://html.spec.whatwg.org/multipage/webstorage.html#dom-storage-setitem
    pub fn set_with_limit(
        &mut self,
        key: &str,
        value: &str,
        max_bytes: usize,
    ) -> Result<Option<StorageChange>, StorageError> {
        let existing = self.entries.iter().position(|(stored, _)| stored == key);
        let old = existing.map(|index| self.entries[index].1.clone());
        if old.as_deref() == Some(value) {
            return Ok(None);
        }
        let used = self.used_bytes();
        let replaced = old.as_ref().map_or(0, |old| key.len() + old.len());
        if used - replaced + key.len() + value.len() > max_bytes.min(STORAGE_QUOTA_BYTES) {
            return Err(StorageError::QuotaExceeded);
        }
        if let Some(index) = existing {
            value.clone_into(&mut self.entries[index].1);
        } else {
            self.entries.push((key.to_owned(), value.to_owned()));
        }
        Ok(Some(StorageChange {
            key: Some(key.to_owned()),
            old_value: old,
            new_value: Some(value.to_owned()),
        }))
    }

    // https://html.spec.whatwg.org/multipage/webstorage.html#dom-storage-removeitem
    pub fn remove(&mut self, key: &str) -> Option<StorageChange> {
        let index = self.entries.iter().position(|(stored, _)| stored == key)?;
        let (_, old) = self.entries.remove(index);
        Some(StorageChange {
            key: Some(key.to_owned()),
            old_value: Some(old),
            new_value: None,
        })
    }

    // https://html.spec.whatwg.org/multipage/webstorage.html#dom-storage-clear
    pub fn clear(&mut self) -> Option<StorageChange> {
        if self.entries.is_empty() {
            return None;
        }
        self.entries.clear();
        Some(StorageChange {
            key: None,
            old_value: None,
            new_value: None,
        })
    }
}

fn set_bounded_area(
    areas: &mut HashMap<StorageKey, StorageArea>,
    origin: &StorageKey,
    key: &str,
    value: &str,
    total_limit: usize,
) -> Result<Option<StorageChange>, StorageError> {
    let other_used: usize = areas
        .iter()
        .filter(|(key_origin, _)| *key_origin != origin)
        .map(|(_, area)| area.used_bytes())
        .sum();
    let limit = total_limit.saturating_sub(other_used);
    areas
        .entry(origin.clone())
        .or_default()
        .set_with_limit(key, value, limit)
}

#[derive(Default)]
pub struct StorageAreas {
    areas: Mutex<HashMap<StorageKey, StorageArea>>,
    dirty: AtomicBool,
}

impl StorageAreas {
    // https://html.spec.whatwg.org/multipage/webstorage.html#dom-storage-getitem
    #[must_use]
    pub fn get(&self, origin: &StorageKey, key: &str) -> Option<String> {
        self.areas
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(origin)
            .and_then(|area| area.get(key))
    }

    #[must_use]
    pub fn keys(&self, origin: &StorageKey) -> Vec<String> {
        self.areas
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(origin)
            .map_or_default(StorageArea::keys)
    }

    #[must_use]
    pub fn sorted_entries(&self, origin: &StorageKey) -> Vec<(String, String)> {
        let areas = self.areas.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(area) = areas.get(origin) else {
            return Vec::new();
        };
        let mut entries: Vec<(String, String)> = area
            .entries()
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
            .collect();
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        entries
    }

    // https://html.spec.whatwg.org/multipage/webstorage.html#dom-storage-setitem
    pub fn set(
        &self,
        origin: &StorageKey,
        key: &str,
        value: &str,
    ) -> Result<Option<StorageChange>, StorageError> {
        let mut areas = self.areas.lock().unwrap_or_else(PoisonError::into_inner);
        let change = set_bounded_area(&mut areas, origin, key, value, TOTAL_STORAGE_QUOTA_BYTES)?;
        let Some(change) = change else {
            return Ok(None);
        };
        drop(areas);
        self.dirty.store(true, Ordering::SeqCst);
        Ok(Some(change))
    }

    // https://html.spec.whatwg.org/multipage/webstorage.html#dom-storage-removeitem
    pub fn remove(&self, origin: &StorageKey, key: &str) -> Option<StorageChange> {
        let mut areas = self.areas.lock().unwrap_or_else(PoisonError::into_inner);
        let change = areas.get_mut(origin)?.remove(key)?;
        drop(areas);
        self.dirty.store(true, Ordering::SeqCst);
        Some(change)
    }

    // https://html.spec.whatwg.org/multipage/webstorage.html#dom-storage-clear
    pub fn clear(&self, origin: &StorageKey) -> Option<StorageChange> {
        let mut areas = self.areas.lock().unwrap_or_else(PoisonError::into_inner);
        let area = areas.get_mut(origin)?;
        let change = area.clear()?;
        drop(areas);
        self.dirty.store(true, Ordering::SeqCst);
        Some(change)
    }

    pub fn clear_all(&self) {
        self.areas
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
        self.dirty.store(true, Ordering::SeqCst);
    }

    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.dirty.load(Ordering::SeqCst)
    }

    pub fn mark_clean(&self) {
        self.dirty.store(false, Ordering::SeqCst);
    }

    #[must_use]
    pub fn export(&self) -> Vec<(String, String, String)> {
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

    pub fn import(&self, rows: impl IntoIterator<Item = (String, String, String)>) {
        let mut areas = self.areas.lock().unwrap_or_else(PoisonError::into_inner);
        areas.clear();
        for (origin, key, value) in rows {
            let Some(origin) = StorageKey::parse(&origin) else {
                continue;
            };
            let _result =
                set_bounded_area(&mut areas, &origin, &key, &value, TOTAL_STORAGE_QUOTA_BYTES);
        }
    }
}

pub struct SessionStorage<K> {
    namespaces: Mutex<HashMap<K, HashMap<StorageKey, StorageArea>>>,
}

impl<K> Default for SessionStorage<K> {
    fn default() -> Self {
        Self {
            namespaces: Mutex::new(HashMap::new()),
        }
    }
}

impl<K: Copy + Eq + Hash> SessionStorage<K> {
    pub fn create(&self, namespace: K) {
        self.namespaces
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(namespace)
            .or_default();
    }

    // https://html.spec.whatwg.org/multipage/document-sequences.html#copy-session-storage
    pub fn copy(&self, source: K, target: K) {
        let mut namespaces = self
            .namespaces
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let namespace = namespaces.get(&source).cloned().unwrap_or_default();
        namespaces.insert(target, namespace);
    }

    pub fn remove_namespace(&self, namespace: K) {
        self.namespaces
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&namespace);
    }

    pub fn clear_all(&self) {
        self.namespaces
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }

    #[must_use]
    pub fn get(&self, namespace: K, origin: &StorageKey, key: &str) -> Option<String> {
        self.namespaces
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&namespace)
            .and_then(|areas| areas.get(origin))
            .and_then(|area| area.get(key))
    }

    #[must_use]
    pub fn keys(&self, namespace: K, origin: &StorageKey) -> Vec<String> {
        self.namespaces
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&namespace)
            .and_then(|areas| areas.get(origin))
            .map_or_default(StorageArea::keys)
    }

    #[must_use]
    pub fn sorted_entries(&self, namespace: K, origin: &StorageKey) -> Vec<(String, String)> {
        let namespaces = self
            .namespaces
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let Some(area) = namespaces
            .get(&namespace)
            .and_then(|areas| areas.get(origin))
        else {
            return Vec::new();
        };
        let mut entries: Vec<(String, String)> = area
            .entries()
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
            .collect();
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        entries
    }

    pub fn set(
        &self,
        namespace: K,
        origin: &StorageKey,
        key: &str,
        value: &str,
    ) -> Result<Option<StorageChange>, StorageError> {
        let mut namespaces = self
            .namespaces
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let Some(areas) = namespaces.get_mut(&namespace) else {
            return Ok(None);
        };
        set_bounded_area(areas, origin, key, value, SESSION_STORAGE_QUOTA_BYTES)
    }

    pub fn remove(&self, namespace: K, origin: &StorageKey, key: &str) -> Option<StorageChange> {
        self.namespaces
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_mut(&namespace)?
            .get_mut(origin)?
            .remove(key)
    }

    pub fn clear(&self, namespace: K, origin: &StorageKey) -> Option<StorageChange> {
        self.namespaces
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_mut(&namespace)?
            .get_mut(origin)?
            .clear()
    }
}

pub struct StorageService<K> {
    local: StorageAreas,
    indexed_db: StorageAreas,
    session: SessionStorage<K>,
}

impl<K> Default for StorageService<K> {
    fn default() -> Self {
        Self {
            local: StorageAreas::default(),
            indexed_db: StorageAreas::default(),
            session: SessionStorage::default(),
        }
    }
}

impl<K: Copy + Eq + Hash> StorageService<K> {
    #[must_use]
    pub fn local(&self) -> &StorageAreas {
        &self.local
    }

    #[must_use]
    pub fn indexed_db(&self) -> &StorageAreas {
        &self.indexed_db
    }

    #[must_use]
    pub fn session(&self) -> &SessionStorage<K> {
        &self.session
    }

    pub fn create_session(&self, namespace: K) {
        self.session.create(namespace);
    }

    pub fn copy_session(&self, source: K, target: K) {
        self.session.copy(source, target);
    }

    pub fn remove_session(&self, namespace: K) {
        self.session.remove_namespace(namespace);
    }

    pub fn clear_all(&self) {
        self.local.clear_all();
        self.indexed_db.clear_all();
        self.session.clear_all();
    }

    #[must_use]
    pub fn get(
        &self,
        kind: StorageKind,
        namespace: K,
        origin: &StorageKey,
        key: &str,
    ) -> Option<String> {
        match kind {
            StorageKind::Local => self.local.get(origin, key),
            StorageKind::Session => self.session.get(namespace, origin, key),
            StorageKind::IndexedDb => self.indexed_db.get(origin, key),
        }
    }

    #[must_use]
    pub fn keys(&self, kind: StorageKind, namespace: K, origin: &StorageKey) -> Vec<String> {
        match kind {
            StorageKind::Local => self.local.keys(origin),
            StorageKind::Session => self.session.keys(namespace, origin),
            StorageKind::IndexedDb => self.indexed_db.keys(origin),
        }
    }

    pub fn set(
        &self,
        kind: StorageKind,
        namespace: K,
        origin: &StorageKey,
        key: &str,
        value: &str,
    ) -> Result<Option<StorageChange>, StorageError> {
        match kind {
            StorageKind::Local => self.local.set(origin, key, value),
            StorageKind::Session => self.session.set(namespace, origin, key, value),
            StorageKind::IndexedDb => self.indexed_db.set(origin, key, value),
        }
    }

    pub fn remove(
        &self,
        kind: StorageKind,
        namespace: K,
        origin: &StorageKey,
        key: &str,
    ) -> Option<StorageChange> {
        match kind {
            StorageKind::Local => self.local.remove(origin, key),
            StorageKind::Session => self.session.remove(namespace, origin, key),
            StorageKind::IndexedDb => self.indexed_db.remove(origin, key),
        }
    }

    pub fn clear(
        &self,
        kind: StorageKind,
        namespace: K,
        origin: &StorageKey,
    ) -> Option<StorageChange> {
        match kind {
            StorageKind::Local => self.local.clear(origin),
            StorageKind::Session => self.session.clear(namespace, origin),
            StorageKind::IndexedDb => self.indexed_db.clear(origin),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        STORAGE_QUOTA_BYTES, StorageAreas, StorageChange, StorageError, StorageKey, StorageKind,
        StorageService,
    };

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

    #[test]
    fn storage_service_isolates_kinds_enforces_quotas_and_emits_changes() {
        let service: StorageService<u64> = StorageService::default();
        service.create_session(1);
        let origin = StorageKey::parse("https://app.test").expect("origin");

        assert_eq!(
            service.set(StorageKind::Local, 1, &origin, "k", "v1"),
            Ok(Some(StorageChange {
                key: Some("k".to_owned()),
                old_value: None,
                new_value: Some("v1".to_owned()),
            }))
        );
        assert_eq!(
            service.set(StorageKind::Local, 1, &origin, "k", "v1"),
            Ok(None),
            "unchanged writes produce no storage change event"
        );
        assert_eq!(
            service.set(StorageKind::Session, 1, &origin, "k", "s1"),
            Ok(Some(StorageChange {
                key: Some("k".to_owned()),
                old_value: None,
                new_value: Some("s1".to_owned()),
            }))
        );
        service.copy_session(1, 2);
        assert_eq!(
            service
                .get(StorageKind::Session, 2, &origin, "k")
                .as_deref(),
            Some("s1")
        );
        let _ = service.set(StorageKind::Session, 2, &origin, "k", "s2");
        assert_eq!(
            service
                .get(StorageKind::Session, 1, &origin, "k")
                .as_deref(),
            Some("s1"),
            "copied session namespaces diverge independently"
        );

        let huge = "x".repeat(STORAGE_QUOTA_BYTES + 1);
        assert_eq!(
            service.set(StorageKind::Local, 1, &origin, "big", &huge),
            Err(StorageError::QuotaExceeded)
        );
    }
}
