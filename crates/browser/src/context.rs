//! Live browser context and its default storage partition.

use std::io;
use std::path::Path;
use std::sync::Arc;

use url::Url;

use crate::actor::TabId;
use crate::broadcast::RendererEventHub;
use crate::network::{NetworkContext, TabNetworkHandle};
use crate::profile::Profile;
use crate::profile::store::ProfileStore;
use crate::storage::{SessionStorage, StorageAreas};

/// Services shared by renderer assignments in one storage partition.
#[derive(Clone)]
pub(crate) struct PartitionServices {
    pub(crate) local_storage: Arc<StorageAreas>,
    pub(crate) indexed_db: Arc<StorageAreas>,
    pub(crate) events: Arc<RendererEventHub>,
}

/// Network and site data shared by documents in one partition.
struct StoragePartition {
    network: NetworkContext,
    local_storage: Arc<StorageAreas>,
    indexed_db: Arc<StorageAreas>,
    events: Arc<RendererEventHub>,
}

/// One live profile and its default storage partition.
pub(crate) struct BrowserContext {
    store: Arc<ProfileStore>,
    partition: StoragePartition,
    sessions: Arc<SessionStorage>,
}

impl BrowserContext {
    /// Opens one context and restores its durable partition state.
    ///
    /// # Errors
    ///
    /// The profile directory cannot be created or exclusively locked, or stored
    /// profile data could not be read or quarantined.
    pub(crate) fn open(
        data_home: &Path,
        profile: &Profile,
        network: NetworkContext,
    ) -> io::Result<Self> {
        let store = ProfileStore::open_in(data_home, profile)?;
        store.load_into(&network.agent())?;
        let local_storage = Arc::new(StorageAreas::default());
        store.load_local_storage(&local_storage)?;
        let indexed_db = Arc::new(StorageAreas::default());
        store.load_indexed_db(&indexed_db)?;
        Ok(Self {
            store: Arc::new(store),
            partition: StoragePartition {
                network,
                local_storage,
                indexed_db,
                events: Arc::new(RendererEventHub::default()),
            },
            sessions: Arc::new(SessionStorage::default()),
        })
    }

    #[must_use]
    pub(crate) fn tab_network(&self) -> TabNetworkHandle {
        self.partition.network.tab_handle()
    }

    #[must_use]
    pub(crate) fn partition_services(&self) -> PartitionServices {
        PartitionServices {
            local_storage: Arc::clone(&self.partition.local_storage),
            indexed_db: Arc::clone(&self.partition.indexed_db),
            events: Arc::clone(&self.partition.events),
        }
    }

    #[must_use]
    pub(crate) fn session_storage(&self) -> Arc<SessionStorage> {
        Arc::clone(&self.sessions)
    }

    pub(crate) fn create_tab(&self, tab: TabId) {
        self.sessions.create(tab);
    }

    pub(crate) fn copy_session(&self, source: TabId, target: TabId) {
        self.sessions.copy(source, target);
    }

    pub(crate) fn close_tab(&self, tab: TabId) {
        self.sessions.remove_namespace(tab);
    }

    pub(crate) async fn persist(&self) -> io::Result<()> {
        let store = Arc::clone(&self.store);
        let agent = self.partition.network.agent();
        let local_storage = Arc::clone(&self.partition.local_storage);
        let indexed_db = Arc::clone(&self.partition.indexed_db);
        tokio::task::spawn_blocking(move || {
            store.save_from(&agent)?;
            store.save_local_storage(&local_storage)?;
            store.save_indexed_db(&indexed_db)
        })
        .await
        .map_err(io::Error::other)?
    }

    #[must_use]
    pub(crate) fn cookie_records(&self, url: &Url) -> Vec<net::CookieRecord> {
        self.partition.network.cookie_records(url)
    }

    pub(crate) fn clear_cookies(&self) {
        self.partition.network.clear_cookies();
    }

    pub(crate) fn add_cookie(&self, cookie: &str, url: &Url) -> bool {
        self.partition.network.add_cookie(cookie, url)
    }
}
