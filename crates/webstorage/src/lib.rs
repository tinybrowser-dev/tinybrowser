use serde::{Deserialize, Serialize};

// https://html.spec.whatwg.org/multipage/webstorage.html#disk-space-2 — spec leaves quota to the user agent; tinybrowser uses 5 MiB
pub const STORAGE_QUOTA_BYTES: usize = 5 * 1024 * 1024;

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

    // https://html.spec.whatwg.org/multipage/webstorage.html#dom-storage-setitem
    pub fn set(&mut self, key: &str, value: &str) -> Result<Option<StorageChange>, StorageError> {
        let existing = self.entries.iter().position(|(stored, _)| stored == key);
        let old = existing.map(|index| self.entries[index].1.clone());
        if old.as_deref() == Some(value) {
            return Ok(None);
        }
        let used: usize = self
            .entries
            .iter()
            .map(|(stored_key, stored_value)| stored_key.len() + stored_value.len())
            .sum();
        let replaced = old.as_ref().map_or(0, |old| key.len() + old.len());
        if used - replaced + key.len() + value.len() > STORAGE_QUOTA_BYTES {
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
