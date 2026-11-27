// https://w3c.github.io/FileAPI/#blob-url-store

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde::{Deserialize, Serialize};

// https://w3c.github.io/FileAPI/#blob-url-store — spec leaves quota to the user agent; tinybrowser caps a partition's blob store at 8 MiB.
pub const MAX_OBJECT_URL_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobRecord {
    pub contents: String,
    pub content_type: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlobOrigin<Owner> {
    Tuple(url::Origin),
    Opaque { owner: Owner, scope: u64 },
}

#[derive(Clone, Debug)]
struct BlobEntry<Owner> {
    contents: Arc<str>,
    content_type: Arc<str>,
    origin: BlobOrigin<Owner>,
    owner: Owner,
    scope: u64,
}

#[derive(Debug)]
struct BlobTable<Owner> {
    entries: HashMap<String, BlobEntry<Owner>>,
    bytes: usize,
}

impl<Owner> Default for BlobTable<Owner> {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            bytes: 0,
        }
    }
}

#[derive(Debug)]
pub struct ObjectUrlStore<Owner> {
    table: Mutex<BlobTable<Owner>>,
}

impl<Owner> Default for ObjectUrlStore<Owner> {
    fn default() -> Self {
        Self {
            table: Mutex::new(BlobTable::default()),
        }
    }
}

// https://w3c.github.io/FileAPI/#blob-url-store
#[must_use]
pub fn object_url_key(url: &str) -> &str {
    url.split_once('#').map_or(url, |(head, _)| head)
}

// https://w3c.github.io/FileAPI/#dfn-createObjectURL
fn object_url_id() -> Option<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).ok()?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let mut id = String::with_capacity(36);
    for (index, byte) in bytes.iter().enumerate() {
        if matches!(index, 4 | 6 | 8 | 10) {
            id.push('-');
        }
        let _ = write!(id, "{byte:02x}");
    }
    Some(id)
}

impl<Owner: Copy + Eq> ObjectUrlStore<Owner> {
    fn lock(&self) -> MutexGuard<'_, BlobTable<Owner>> {
        self.table.lock().unwrap_or_else(PoisonError::into_inner)
    }

    // https://w3c.github.io/FileAPI/#dfn-createObjectURL
    pub fn create(
        &self,
        owner: Owner,
        scope: u64,
        origin: Option<url::Origin>,
        contents: String,
        content_type: String,
    ) -> Option<String> {
        let length = contents.len().checked_add(content_type.len())?;
        let mut table = self.lock();
        let total = table.bytes.checked_add(length)?;
        if total > MAX_OBJECT_URL_BYTES {
            return None;
        }
        let id = object_url_id()?;
        let (serialized, stored_origin) = match origin {
            Some(origin @ url::Origin::Tuple(..)) => {
                (origin.ascii_serialization(), BlobOrigin::Tuple(origin))
            }
            Some(url::Origin::Opaque(_)) | None => {
                ("null".to_owned(), BlobOrigin::Opaque { owner, scope })
            }
        };
        let url = format!("blob:{serialized}/{id}");
        table.entries.insert(
            url.clone(),
            BlobEntry {
                contents: Arc::from(contents),
                content_type: Arc::from(content_type),
                origin: stored_origin,
                owner,
                scope,
            },
        );
        table.bytes = total;
        Some(url)
    }

    fn matches_caller(
        entry: &BlobEntry<Owner>,
        owner: Owner,
        scope: u64,
        caller_origin: Option<&url::Origin>,
    ) -> bool {
        match (&entry.origin, caller_origin) {
            (BlobOrigin::Tuple(stored), Some(caller)) => stored == caller,
            (
                BlobOrigin::Opaque {
                    owner: stored_owner,
                    scope: stored_scope,
                },
                None,
            ) => *stored_owner == owner && *stored_scope == scope,
            _ => false,
        }
    }

    // https://w3c.github.io/FileAPI/#blob-url-resolve
    #[must_use]
    pub fn resolve(
        &self,
        owner: Owner,
        scope: u64,
        caller_origin: Option<&url::Origin>,
        url: &str,
    ) -> Option<BlobRecord> {
        let table = self.lock();
        let entry = table.entries.get(object_url_key(url))?;
        Self::matches_caller(entry, owner, scope, caller_origin).then(|| BlobRecord {
            contents: entry.contents.to_string(),
            content_type: entry.content_type.to_string(),
        })
    }

    // https://w3c.github.io/FileAPI/#blob-url-resolve
    #[must_use]
    pub fn resolve_navigation(&self, url: &str) -> Option<(Vec<u8>, String, Option<url::Origin>)> {
        let table = self.lock();
        let entry = table.entries.get(object_url_key(url))?;
        let origin = match &entry.origin {
            BlobOrigin::Tuple(origin) => Some(origin.clone()),
            BlobOrigin::Opaque { .. } => None,
        };
        Some((
            entry.contents.as_bytes().to_vec(),
            entry.content_type.to_string(),
            origin,
        ))
    }

    // https://w3c.github.io/FileAPI/#dfn-revokeObjectURL
    pub fn revoke(&self, owner: Owner, scope: u64, caller_origin: Option<&url::Origin>, url: &str) {
        let key = object_url_key(url);
        let mut table = self.lock();
        let allowed = table
            .entries
            .get(key)
            .is_some_and(|entry| Self::matches_caller(entry, owner, scope, caller_origin));
        if !allowed {
            return;
        }
        if let Some(removed) = table.entries.remove(key) {
            let released = removed.contents.len() + removed.content_type.len();
            table.bytes = table.bytes.saturating_sub(released);
        }
    }

    // https://w3c.github.io/FileAPI/#lifeTime
    pub fn unload_scope(&self, owner: Owner, scope: u64) {
        let mut table = self.lock();
        let mut released = 0usize;
        table.entries.retain(|_, entry| {
            if entry.owner == owner && entry.scope == scope {
                released = released.saturating_add(entry.contents.len() + entry.content_type.len());
                false
            } else {
                true
            }
        });
        table.bytes = table.bytes.saturating_sub(released);
    }

    // https://w3c.github.io/FileAPI/#lifeTime
    pub fn unload_owner(&self, owner: Owner) {
        let mut table = self.lock();
        let mut released = 0usize;
        table.entries.retain(|_, entry| {
            if entry.owner == owner {
                released = released.saturating_add(entry.contents.len() + entry.content_type.len());
                false
            } else {
                true
            }
        });
        table.bytes = table.bytes.saturating_sub(released);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use url::Url;

    #[test]
    fn tuple_origin_blobs_share_across_owners_and_reject_other_origins() {
        let store: ObjectUrlStore<u64> = ObjectUrlStore::default();
        let a = Url::parse("https://a.test/").expect("url").origin();
        let b = Url::parse("https://b.test/").expect("url").origin();
        let url = store
            .create(1, 10, Some(a.clone()), "hello".into(), "text/plain".into())
            .expect("blob");
        assert!(url.starts_with("blob:https://a.test/"));
        assert_eq!(
            store.resolve(2, 20, Some(&a), &format!("{url}#frag")),
            Some(BlobRecord {
                contents: "hello".into(),
                content_type: "text/plain".into(),
            })
        );
        assert_eq!(store.resolve(2, 20, Some(&b), &url), None);
        store.revoke(2, 20, Some(&b), &url);
        assert!(store.resolve(1, 10, Some(&a), &url).is_some());
        store.revoke(2, 20, Some(&a), &url);
        assert!(store.resolve(1, 10, Some(&a), &url).is_none());
    }

    #[test]
    fn opaque_origin_blobs_only_match_their_creating_scope() {
        let store: ObjectUrlStore<u64> = ObjectUrlStore::default();
        let url = store
            .create(1, 10, None, "secret".into(), "text/html".into())
            .expect("blob");
        assert!(url.starts_with("blob:null/"));
        assert!(store.resolve(1, 10, None, &url).is_some());
        assert!(store.resolve(1, 11, None, &url).is_none());
        assert!(store.resolve(2, 10, None, &url).is_none());
        store.unload_scope(1, 10);
        assert!(store.resolve(1, 10, None, &url).is_none());
    }
}
