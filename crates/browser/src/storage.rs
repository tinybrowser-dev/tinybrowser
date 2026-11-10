//! Browser-owned per-origin key/value areas.
//!
//! `localStorage` and `indexedDB` each keep one area per origin, shared by
//! every document in the profile and persisted with it
//! (<https://html.spec.whatwg.org/multipage/webstorage.html#the-localstorage-attribute>,
//! <https://w3c.github.io/IndexedDB/#database>).
//! The browser process owns the maps so independent renderers observe one
//! store; renderers call in over the service path. `sessionStorage` is scoped
//! to one top-level browsing context and remains browser-owned across renderer
//! swaps.
//!
//! The mutation methods implement the spec's `Storage` algorithms and return
//! the change a `storage` event needs; `None` means the mutation was a no-op.
//! A caller of `indexedDB` ignores the change and writes opaque encoded rows.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};

use renderer::{StorageChange, StorageError, StorageKey};
use webstorage::StorageArea;

use crate::actor::TabId;

/// One profile's per-origin string areas.
#[derive(Default)]
pub(crate) struct StorageAreas {
    areas: Mutex<HashMap<StorageKey, StorageArea>>,
    dirty: AtomicBool,
}

/// Ephemeral storage namespaces keyed by top-level browsing context.
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

    /// Copies a top-level browsing context's session-storage namespace
    /// (<https://html.spec.whatwg.org/multipage/document-sequences.html#copy-session-storage>).
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
        namespace.entry(origin.clone()).or_default().set(key, value)
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
    /// `Storage.getItem(key)`
    /// (<https://html.spec.whatwg.org/multipage/webstorage.html#dom-storage-getitem>).
    pub(crate) fn get(&self, origin: &StorageKey, key: &str) -> Option<String> {
        self.areas
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(origin)
            .and_then(|area| area.get(key))
    }

    /// `Storage.key(index)` support: every key, in this area's iteration order.
    pub(crate) fn keys(&self, origin: &StorageKey) -> Vec<String> {
        self.areas
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(origin)
            .map_or_default(StorageArea::keys)
    }

    /// `Storage.setItem(key, value)`
    /// (<https://html.spec.whatwg.org/multipage/webstorage.html#dom-storage-setitem>).
    pub(crate) fn set(
        &self,
        origin: &StorageKey,
        key: &str,
        value: &str,
    ) -> Result<Option<StorageChange>, StorageError> {
        let mut areas = self.areas.lock().unwrap_or_else(PoisonError::into_inner);
        let area = areas.entry(origin.clone()).or_default();
        let change = area.set(key, value)?;
        let Some(change) = change else {
            return Ok(None);
        };
        drop(areas);
        self.dirty.store(true, Ordering::SeqCst);
        Ok(Some(change))
    }

    /// `Storage.removeItem(key)`
    /// (<https://html.spec.whatwg.org/multipage/webstorage.html#dom-storage-removeitem>).
    pub(crate) fn remove(&self, origin: &StorageKey, key: &str) -> Option<StorageChange> {
        let mut areas = self.areas.lock().unwrap_or_else(PoisonError::into_inner);
        let change = areas.get_mut(origin)?.remove(key)?;
        drop(areas);
        self.dirty.store(true, Ordering::SeqCst);
        Some(change)
    }

    /// `Storage.clear()`
    /// (<https://html.spec.whatwg.org/multipage/webstorage.html#dom-storage-clear>).
    pub(crate) fn clear(&self, origin: &StorageKey) -> Option<StorageChange> {
        let mut areas = self.areas.lock().unwrap_or_else(PoisonError::into_inner);
        let area = areas.get_mut(origin)?;
        let change = area.clear()?;
        drop(areas);
        self.dirty.store(true, Ordering::SeqCst);
        Some(change)
    }

    /// Whether unsaved mutations happened since the last [`Self::mark_clean`].
    pub(crate) fn is_dirty(&self) -> bool {
        self.dirty.load(Ordering::SeqCst)
    }

    pub(crate) fn mark_clean(&self) {
        self.dirty.store(false, Ordering::SeqCst);
    }

    /// Every `(origin, key, value)` row, for the profile store.
    ///
    /// Rows are sorted by origin then key, so the persisted bytes do not
    /// depend on hash iteration order.
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

    /// Replaces the areas with the profile store's rows.
    ///
    /// A stored origin is parsed back into a [`StorageKey`], which normalizes
    /// every spelling of one origin to one area and drops a row that names no
    /// tuple origin. A profile written before the key was typed may hold
    /// `https://example.test/path` or `about:blank`, and neither becomes a
    /// second area.
    pub(crate) fn import(&self, rows: impl IntoIterator<Item = (String, String, String)>) {
        let mut areas = self.areas.lock().unwrap_or_else(PoisonError::into_inner);
        areas.clear();
        for (origin, key, value) in rows {
            let Some(origin) = StorageKey::parse(&origin) else {
                continue;
            };
            let area = areas.entry(origin).or_default();
            let _result = area.set(&key, &value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::StorageAreas;
    use renderer::StorageKey;

    /// One origin spelled four ways is one area, and a profile written before
    /// the key was typed normalizes on load: a full URL becomes its origin and
    /// `about:blank` (which names no tuple origin) is dropped instead of
    /// standing for every opaque document.
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
