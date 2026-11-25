use std::collections::HashMap;
use std::fmt;
use std::io;
use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use url::Url;

use crate::actor::TabId;
use crate::broadcast::RendererEventHub;
use crate::intercept::NetworkBroker;
use crate::network::{NetworkContext, TabNetworkHandle};
use crate::profile::Profile;
use crate::profile::store::ProfileStore;
use crate::storage::{CookieStore, SessionStorage, StorageAreas};

// https://w3c.github.io/webdriver-bidi/#type-browser-UserContext
pub const DEFAULT_USER_CONTEXT: &str = "default";

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UserContextId(String);

impl UserContextId {
    #[must_use]
    pub fn default_context() -> Self {
        Self(DEFAULT_USER_CONTEXT.to_owned())
    }

    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn is_default(&self) -> bool {
        self.0 == DEFAULT_USER_CONTEXT
    }
}

impl Default for UserContextId {
    fn default() -> Self {
        Self::default_context()
    }
}

impl fmt::Display for UserContextId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone)]
pub(crate) struct PartitionServices {
    pub(crate) cookies: Arc<CookieStore>,
    pub(crate) local_storage: Arc<StorageAreas>,
    pub(crate) indexed_db: Arc<StorageAreas>,
    pub(crate) events: Arc<RendererEventHub>,
}

struct StoragePartition {
    network: NetworkContext,
    services: PartitionServices,
}

pub(crate) struct BrowserContext {
    store: Arc<ProfileStore>,
    base_network: net::AgentOptions,
    broker: Arc<NetworkBroker>,
    default_partition: StoragePartition,
    ephemeral: HashMap<UserContextId, StoragePartition>,
    next_user_context: u64,
    sessions: Arc<SessionStorage>,
}

impl BrowserContext {
    pub(crate) fn open(
        data_home: &Path,
        profile: &Profile,
        network_options: net::AgentOptions,
        broker: Arc<NetworkBroker>,
    ) -> io::Result<Self> {
        let store = ProfileStore::open_in(data_home, profile)?;
        let cookies = Arc::new(CookieStore::new(network_options.clock.clone()));
        store.load_cookies(&cookies)?;
        let mut default_options = network_options.clone();
        default_options.cookie_jar = Some(cookies.jar());
        let network = NetworkContext::new(default_options, Arc::clone(&broker));
        let local_storage = Arc::new(StorageAreas::default());
        store.load_local_storage(&local_storage)?;
        let indexed_db = Arc::new(StorageAreas::default());
        store.load_indexed_db(&indexed_db)?;
        Ok(Self {
            store: Arc::new(store),
            base_network: network_options,
            broker,
            default_partition: StoragePartition {
                network,
                services: PartitionServices {
                    cookies,
                    local_storage,
                    indexed_db,
                    events: Arc::new(RendererEventHub::default()),
                },
            },
            ephemeral: HashMap::new(),
            next_user_context: 1,
            sessions: Arc::new(SessionStorage::default()),
        })
    }

    fn partition(&self, context: &UserContextId) -> Option<&StoragePartition> {
        if context.is_default() {
            Some(&self.default_partition)
        } else {
            self.ephemeral.get(context)
        }
    }

    pub(crate) fn create_user_context(&mut self) -> UserContextId {
        let raw = self.next_user_context;
        self.next_user_context = self.next_user_context.saturating_add(1);
        let id = UserContextId::new(format!("user-context-{raw}"));
        let cookies = Arc::new(CookieStore::new(self.base_network.clock.clone()));
        let mut options = self.base_network.clone();
        options.cookie_jar = Some(cookies.jar());
        let network = NetworkContext::new(options, Arc::clone(&self.broker));
        self.ephemeral.insert(
            id.clone(),
            StoragePartition {
                network,
                services: PartitionServices {
                    cookies,
                    local_storage: Arc::new(StorageAreas::default()),
                    indexed_db: Arc::new(StorageAreas::default()),
                    events: Arc::new(RendererEventHub::default()),
                },
            },
        );
        id
    }

    pub(crate) fn remove_user_context(&mut self, id: &UserContextId) -> bool {
        if id.is_default() {
            return false;
        }
        self.ephemeral.remove(id).is_some()
    }

    #[must_use]
    pub(crate) fn has_user_context(&self, id: &UserContextId) -> bool {
        id.is_default() || self.ephemeral.contains_key(id)
    }

    #[must_use]
    pub(crate) fn user_contexts(&self) -> Vec<UserContextId> {
        let mut list = Vec::with_capacity(1 + self.ephemeral.len());
        list.push(UserContextId::default_context());
        let mut extra: Vec<UserContextId> = self.ephemeral.keys().cloned().collect();
        extra.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        list.extend(extra);
        list
    }

    #[must_use]
    pub(crate) fn identity(&self) -> net::ValidIdentity {
        self.default_partition.network.identity()
    }

    #[must_use]
    pub(crate) fn tab_network(&self, context: &UserContextId) -> Option<TabNetworkHandle> {
        self.partition(context)
            .map(|partition| partition.network.tab_handle())
    }

    #[must_use]
    pub(crate) fn partition_services(&self, context: &UserContextId) -> Option<PartitionServices> {
        self.partition(context)
            .map(|partition| partition.services.clone())
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
        let cookies = Arc::clone(&self.default_partition.services.cookies);
        let local_storage = Arc::clone(&self.default_partition.services.local_storage);
        let indexed_db = Arc::clone(&self.default_partition.services.indexed_db);
        tokio::task::spawn_blocking(move || {
            store.save_cookies(&cookies)?;
            store.save_local_storage(&local_storage)?;
            store.save_indexed_db(&indexed_db)
        })
        .await
        .map_err(io::Error::other)?
    }

    #[must_use]
    pub(crate) fn cookie_records(
        &self,
        context: &UserContextId,
        url: &Url,
    ) -> Vec<net::CookieRecord> {
        self.partition(context)
            .map_or_default(|partition| partition.services.cookies.cookie_records(url))
    }

    #[must_use]
    pub(crate) fn all_cookie_records(&self, context: &UserContextId) -> Vec<net::CookieRecord> {
        self.partition(context)
            .map_or_default(|partition| partition.services.cookies.all_records())
    }

    #[must_use]
    pub(crate) fn matching_cookie_records(
        &self,
        context: &UserContextId,
        filter: &net::CookieFilter,
    ) -> Vec<net::CookieRecord> {
        self.partition(context)
            .map_or_default(|partition| partition.services.cookies.matching_records(filter))
    }

    pub(crate) fn clear_cookies(&self, context: &UserContextId) {
        if let Some(partition) = self.partition(context) {
            partition.services.cookies.clear();
        }
    }

    pub(crate) fn clear_cache(&self, context: &UserContextId) {
        if let Some(partition) = self.partition(context) {
            partition.network.clear_cache();
        }
    }

    pub(crate) fn set_cache_disabled(&self, context: &UserContextId, disabled: bool) {
        if let Some(partition) = self.partition(context) {
            partition.network.set_cache_disabled(disabled);
        }
    }

    pub(crate) fn add_cookie(&self, context: &UserContextId, cookie: &str, url: &Url) -> bool {
        self.partition(context)
            .is_some_and(|partition| partition.services.cookies.add_cookie(cookie, url))
    }

    pub(crate) fn set_direct_cookie(
        &self,
        context: &UserContextId,
        cookie: &net::DirectCookie,
    ) -> bool {
        self.partition(context)
            .is_some_and(|partition| partition.services.cookies.set_direct(cookie.clone()))
    }

    pub(crate) fn delete_cookies(
        &self,
        context: &UserContextId,
        filter: &net::CookieFilter,
    ) -> usize {
        self.partition(context).map_or(0, |partition| {
            partition.services.cookies.delete_matching(filter)
        })
    }

    pub(crate) fn delete_cookies_for_url(
        &self,
        context: &UserContextId,
        url: &Url,
        name: Option<&str>,
    ) -> usize {
        self.partition(context).map_or(0, |partition| {
            partition.services.cookies.delete_for_url(url, name)
        })
    }
}
