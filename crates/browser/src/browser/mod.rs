mod task;

pub(crate) use self::task::{TabNames, TabRegistry};

use std::convert::Infallible;
use std::fmt;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::broadcast;
use url::Url;

use self::task::BrowserTask;
use crate::actor::{NavigationInitiator, TabHandle, TabId};
use crate::context::{BrowserContext, UserContextId};
use crate::exchange;
use crate::intercept::NetworkBroker;
use crate::profile::Profile;
use crate::profile::default_data_home;
use crate::prompt::PromptBroker;

const BROWSER_COMMAND_CAPACITY: usize = 256;
const LIFECYCLE_EVENT_CAPACITY: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BrowserLifecycleEvent {
    TabCreated {
        tab: TabId,
        user_context: UserContextId,
        opener: Option<TabId>,
    },
    TabClosed {
        tab: TabId,
        user_context: UserContextId,
    },
}

#[derive(Clone, Debug, Default)]
pub struct BrowserOptions {
    pub data_home: Option<PathBuf>,
    pub profile: Profile,
    pub network: net::AgentOptions,
}

pub struct Browser {
    handle: BrowserHandle,
}

#[derive(Clone)]
pub struct BrowserHandle {
    client: BrowserClient,
    // https://wicg.github.io/ua-client-hints/#http-ua-hints
    identity: Arc<net::ValidIdentity>,
    lifecycle: broadcast::Sender<BrowserLifecycleEvent>,
    network_broker: Arc<NetworkBroker>,
    prompt_broker: Arc<PromptBroker>,
}

pub(crate) struct OpenWindowOptions {
    pub(crate) url: Option<Url>,
    pub(crate) target: renderer::TargetName,
    pub(crate) source: Option<TabId>,
    // https://html.spec.whatwg.org/multipage/browsers.html#sandboxed-navigation-browsing-context-flag
    pub(crate) source_frame: renderer::FrameId,
    pub(crate) opener: renderer::OpenerPolicy,
    // https://html.spec.whatwg.org/multipage/nav-history-apis.html#window-open-steps
    pub(crate) initiator: NavigationInitiator,
    // https://html.spec.whatwg.org/multipage/browsers.html#popup-sandboxing-flag-set
    pub(crate) popup_sandbox: renderer::SandboxFlags,
}

pub(super) enum Command {
    CreateTab {
        user_context: UserContextId,
    },
    Tabs,
    TabsIn {
        user_context: UserContextId,
    },
    Tab {
        id: TabId,
    },
    CloseTab {
        id: TabId,
    },
    CreateUserContext,
    RemoveUserContext {
        id: UserContextId,
    },
    UserContexts,
    Close,
    CookieRecords {
        user_context: UserContextId,
        url: Url,
    },
    AllCookieRecords {
        user_context: UserContextId,
    },
    MatchingCookieRecords {
        user_context: UserContextId,
        filter: net::CookieFilter,
    },
    ClearCookies {
        user_context: UserContextId,
    },
    ClearCache {
        user_context: UserContextId,
    },
    SetCacheDisabled {
        user_context: UserContextId,
        disabled: bool,
    },
    AddCookie {
        user_context: UserContextId,
        cookie: String,
        url: Url,
    },
    SetDirectCookie {
        user_context: UserContextId,
        cookie: Box<net::DirectCookie>,
    },
    DeleteCookies {
        user_context: UserContextId,
        filter: net::CookieFilter,
    },
    DeleteCookiesForUrl {
        user_context: UserContextId,
        url: Url,
        name: Option<String>,
    },
    OpenWindow(OpenWindowOptions),
}

pub(super) enum Reply {
    CreateTab(Result<TabHandle, BrowserError>),
    Tabs(Vec<TabId>),
    Tab(Result<TabHandle, BrowserError>),
    CloseTab(Result<(), BrowserError>),
    CreateUserContext(UserContextId),
    RemoveUserContext(Result<(), BrowserError>),
    UserContexts(Vec<UserContextId>),
    Close(io::Result<()>),
    CookieRecords(Vec<net::CookieRecord>),
    ClearCookies,
    ClearCache,
    SetCacheDisabled,
    AddCookie(bool),
    DeletedCount(usize),
    OpenWindow(Result<TabId, BrowserError>),
}

impl Reply {
    fn into_create_tab(self) -> Result<TabHandle, BrowserError> {
        match self {
            Self::CreateTab(result) => result,
            Self::Tabs(_)
            | Self::Tab(_)
            | Self::CloseTab(_)
            | Self::CreateUserContext(_)
            | Self::RemoveUserContext(_)
            | Self::UserContexts(_)
            | Self::Close(_)
            | Self::CookieRecords(_)
            | Self::ClearCookies
            | Self::ClearCache
            | Self::SetCacheDisabled
            | Self::AddCookie(_)
            | Self::DeletedCount(_)
            | Self::OpenWindow(_) => Err(BrowserError::Protocol),
        }
    }

    fn into_tabs(self) -> Result<Vec<TabId>, BrowserError> {
        match self {
            Self::Tabs(tabs) => Ok(tabs),
            Self::CreateTab(_)
            | Self::Tab(_)
            | Self::CloseTab(_)
            | Self::CreateUserContext(_)
            | Self::RemoveUserContext(_)
            | Self::UserContexts(_)
            | Self::Close(_)
            | Self::CookieRecords(_)
            | Self::ClearCookies
            | Self::ClearCache
            | Self::SetCacheDisabled
            | Self::AddCookie(_)
            | Self::DeletedCount(_)
            | Self::OpenWindow(_) => Err(BrowserError::Protocol),
        }
    }

    fn into_tab(self) -> Result<TabHandle, BrowserError> {
        match self {
            Self::Tab(result) => result,
            Self::CreateTab(_)
            | Self::Tabs(_)
            | Self::CloseTab(_)
            | Self::CreateUserContext(_)
            | Self::RemoveUserContext(_)
            | Self::UserContexts(_)
            | Self::Close(_)
            | Self::CookieRecords(_)
            | Self::ClearCookies
            | Self::ClearCache
            | Self::SetCacheDisabled
            | Self::AddCookie(_)
            | Self::DeletedCount(_)
            | Self::OpenWindow(_) => Err(BrowserError::Protocol),
        }
    }

    fn into_close_tab(self) -> Result<(), BrowserError> {
        match self {
            Self::CloseTab(result) => result,
            Self::CreateTab(_)
            | Self::Tabs(_)
            | Self::Tab(_)
            | Self::CreateUserContext(_)
            | Self::RemoveUserContext(_)
            | Self::UserContexts(_)
            | Self::Close(_)
            | Self::CookieRecords(_)
            | Self::ClearCookies
            | Self::ClearCache
            | Self::SetCacheDisabled
            | Self::AddCookie(_)
            | Self::DeletedCount(_)
            | Self::OpenWindow(_) => Err(BrowserError::Protocol),
        }
    }

    fn into_create_user_context(self) -> Result<UserContextId, BrowserError> {
        match self {
            Self::CreateUserContext(id) => Ok(id),
            Self::CreateTab(_)
            | Self::Tabs(_)
            | Self::Tab(_)
            | Self::CloseTab(_)
            | Self::RemoveUserContext(_)
            | Self::UserContexts(_)
            | Self::Close(_)
            | Self::CookieRecords(_)
            | Self::ClearCookies
            | Self::ClearCache
            | Self::SetCacheDisabled
            | Self::AddCookie(_)
            | Self::DeletedCount(_)
            | Self::OpenWindow(_) => Err(BrowserError::Protocol),
        }
    }

    fn into_remove_user_context(self) -> Result<(), BrowserError> {
        match self {
            Self::RemoveUserContext(result) => result,
            Self::CreateTab(_)
            | Self::Tabs(_)
            | Self::Tab(_)
            | Self::CloseTab(_)
            | Self::CreateUserContext(_)
            | Self::UserContexts(_)
            | Self::Close(_)
            | Self::CookieRecords(_)
            | Self::ClearCookies
            | Self::ClearCache
            | Self::SetCacheDisabled
            | Self::AddCookie(_)
            | Self::DeletedCount(_)
            | Self::OpenWindow(_) => Err(BrowserError::Protocol),
        }
    }

    fn into_user_contexts(self) -> Result<Vec<UserContextId>, BrowserError> {
        match self {
            Self::UserContexts(contexts) => Ok(contexts),
            Self::CreateTab(_)
            | Self::Tabs(_)
            | Self::Tab(_)
            | Self::CloseTab(_)
            | Self::CreateUserContext(_)
            | Self::RemoveUserContext(_)
            | Self::Close(_)
            | Self::CookieRecords(_)
            | Self::ClearCookies
            | Self::ClearCache
            | Self::SetCacheDisabled
            | Self::AddCookie(_)
            | Self::DeletedCount(_)
            | Self::OpenWindow(_) => Err(BrowserError::Protocol),
        }
    }

    fn into_cookie_records(self) -> Result<Vec<net::CookieRecord>, BrowserError> {
        match self {
            Self::CookieRecords(records) => Ok(records),
            Self::CreateTab(_)
            | Self::Tabs(_)
            | Self::Tab(_)
            | Self::CloseTab(_)
            | Self::CreateUserContext(_)
            | Self::RemoveUserContext(_)
            | Self::UserContexts(_)
            | Self::Close(_)
            | Self::ClearCookies
            | Self::ClearCache
            | Self::SetCacheDisabled
            | Self::AddCookie(_)
            | Self::DeletedCount(_)
            | Self::OpenWindow(_) => Err(BrowserError::Protocol),
        }
    }

    fn into_clear_cookies(self) -> Result<(), BrowserError> {
        match self {
            Self::ClearCookies => Ok(()),
            Self::CreateTab(_)
            | Self::Tabs(_)
            | Self::Tab(_)
            | Self::CloseTab(_)
            | Self::CreateUserContext(_)
            | Self::RemoveUserContext(_)
            | Self::UserContexts(_)
            | Self::Close(_)
            | Self::CookieRecords(_)
            | Self::ClearCache
            | Self::SetCacheDisabled
            | Self::AddCookie(_)
            | Self::DeletedCount(_)
            | Self::OpenWindow(_) => Err(BrowserError::Protocol),
        }
    }

    fn into_clear_cache(self) -> Result<(), BrowserError> {
        match self {
            Self::ClearCache => Ok(()),
            Self::CreateTab(_)
            | Self::Tabs(_)
            | Self::Tab(_)
            | Self::CloseTab(_)
            | Self::CreateUserContext(_)
            | Self::RemoveUserContext(_)
            | Self::UserContexts(_)
            | Self::Close(_)
            | Self::CookieRecords(_)
            | Self::ClearCookies
            | Self::SetCacheDisabled
            | Self::AddCookie(_)
            | Self::DeletedCount(_)
            | Self::OpenWindow(_) => Err(BrowserError::Protocol),
        }
    }

    fn into_set_cache_disabled(self) -> Result<(), BrowserError> {
        match self {
            Self::SetCacheDisabled => Ok(()),
            Self::CreateTab(_)
            | Self::Tabs(_)
            | Self::Tab(_)
            | Self::CloseTab(_)
            | Self::CreateUserContext(_)
            | Self::RemoveUserContext(_)
            | Self::UserContexts(_)
            | Self::Close(_)
            | Self::CookieRecords(_)
            | Self::ClearCookies
            | Self::ClearCache
            | Self::AddCookie(_)
            | Self::DeletedCount(_)
            | Self::OpenWindow(_) => Err(BrowserError::Protocol),
        }
    }

    fn into_add_cookie(self) -> Result<bool, BrowserError> {
        match self {
            Self::AddCookie(stored) => Ok(stored),
            Self::CreateTab(_)
            | Self::Tabs(_)
            | Self::Tab(_)
            | Self::CloseTab(_)
            | Self::CreateUserContext(_)
            | Self::RemoveUserContext(_)
            | Self::UserContexts(_)
            | Self::Close(_)
            | Self::CookieRecords(_)
            | Self::ClearCookies
            | Self::ClearCache
            | Self::SetCacheDisabled
            | Self::DeletedCount(_)
            | Self::OpenWindow(_) => Err(BrowserError::Protocol),
        }
    }

    fn into_deleted_count(self) -> Result<usize, BrowserError> {
        match self {
            Self::DeletedCount(count) => Ok(count),
            Self::CreateTab(_)
            | Self::Tabs(_)
            | Self::Tab(_)
            | Self::CloseTab(_)
            | Self::CreateUserContext(_)
            | Self::RemoveUserContext(_)
            | Self::UserContexts(_)
            | Self::Close(_)
            | Self::CookieRecords(_)
            | Self::ClearCookies
            | Self::ClearCache
            | Self::SetCacheDisabled
            | Self::AddCookie(_)
            | Self::OpenWindow(_) => Err(BrowserError::Protocol),
        }
    }

    fn into_open_window(self) -> Result<TabId, BrowserError> {
        match self {
            Self::OpenWindow(result) => result,
            Self::CreateTab(_)
            | Self::Tabs(_)
            | Self::Tab(_)
            | Self::CloseTab(_)
            | Self::CreateUserContext(_)
            | Self::RemoveUserContext(_)
            | Self::UserContexts(_)
            | Self::Close(_)
            | Self::CookieRecords(_)
            | Self::ClearCookies
            | Self::ClearCache
            | Self::SetCacheDisabled
            | Self::AddCookie(_)
            | Self::DeletedCount(_) => Err(BrowserError::Protocol),
        }
    }

    fn into_close(self) -> Result<io::Result<()>, io::Error> {
        match self {
            Self::Close(result) => Ok(result),
            Self::CreateTab(_)
            | Self::Tabs(_)
            | Self::Tab(_)
            | Self::CloseTab(_)
            | Self::CreateUserContext(_)
            | Self::RemoveUserContext(_)
            | Self::UserContexts(_)
            | Self::CookieRecords(_)
            | Self::ClearCookies
            | Self::ClearCache
            | Self::SetCacheDisabled
            | Self::AddCookie(_)
            | Self::DeletedCount(_)
            | Self::OpenWindow(_) => Err(io::Error::other("browser protocol mismatch")),
        }
    }
}

#[derive(Clone)]
pub(super) enum Notice {
    Close,
}

pub(super) type BrowserClient = exchange::Client<Command, Infallible, Notice, Reply, Infallible>;
pub(super) type BrowserServer = exchange::Server<Infallible, Reply, Infallible, Command, Notice>;

impl Browser {
    pub fn new(options: BrowserOptions) -> Result<Self, BrowserOpenError> {
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|_| BrowserOpenError::RuntimeUnavailable)?;
        let data_home = match options.data_home {
            Some(data_home) => data_home,
            None => default_data_home().map_err(BrowserOpenError::Profile)?,
        };
        let network_broker = Arc::new(NetworkBroker::new());
        let prompt_broker = Arc::new(PromptBroker::new());
        let context = BrowserContext::open(
            &data_home,
            &options.profile,
            options.network,
            Arc::clone(&network_broker),
        )
        .map_err(BrowserOpenError::Profile)?;
        let identity = Arc::new(context.identity());
        let (client, server) = exchange::local(BROWSER_COMMAND_CAPACITY);
        let (lifecycle, _) = broadcast::channel(LIFECYCLE_EVENT_CAPACITY);
        let handle = BrowserHandle {
            client,
            identity,
            lifecycle: lifecycle.clone(),
            network_broker: Arc::clone(&network_broker),
            prompt_broker: Arc::clone(&prompt_broker),
        };
        let task = BrowserTask::new(
            server,
            context,
            handle.clone(),
            lifecycle,
            network_broker,
            prompt_broker,
        );
        runtime.spawn(task.run());
        Ok(Self { handle })
    }

    #[must_use]
    pub fn handle(&self) -> &BrowserHandle {
        &self.handle
    }

    pub async fn close(self) -> io::Result<()> {
        self.handle.close().await
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        self.handle.request_close();
    }
}

impl BrowserHandle {
    // https://wicg.github.io/ua-client-hints/#http-ua-hints
    #[must_use]
    pub fn identity(&self) -> &net::ValidIdentity {
        &self.identity
    }

    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<BrowserLifecycleEvent> {
        self.lifecycle.subscribe()
    }

    #[must_use]
    pub fn network_broker(&self) -> &Arc<NetworkBroker> {
        &self.network_broker
    }

    #[must_use]
    pub fn prompt_broker(&self) -> &Arc<PromptBroker> {
        &self.prompt_broker
    }

    pub async fn create_tab(&self) -> Result<TabHandle, BrowserError> {
        self.create_tab_in(UserContextId::default_context()).await
    }

    pub async fn create_tab_in(
        &self,
        user_context: UserContextId,
    ) -> Result<TabHandle, BrowserError> {
        self.call(Command::CreateTab { user_context })
            .await?
            .into_create_tab()
    }

    pub async fn tabs(&self) -> Result<Vec<TabId>, BrowserError> {
        self.call(Command::Tabs).await?.into_tabs()
    }

    pub async fn tabs_in(&self, user_context: &UserContextId) -> Result<Vec<TabId>, BrowserError> {
        self.call(Command::TabsIn {
            user_context: user_context.clone(),
        })
        .await?
        .into_tabs()
    }

    pub async fn tab(&self, id: TabId) -> Result<TabHandle, BrowserError> {
        self.call(Command::Tab { id }).await?.into_tab()
    }

    pub async fn close_tab(&self, id: TabId) -> Result<(), BrowserError> {
        self.call(Command::CloseTab { id }).await?.into_close_tab()
    }

    pub async fn create_user_context(&self) -> Result<UserContextId, BrowserError> {
        self.call(Command::CreateUserContext)
            .await?
            .into_create_user_context()
    }

    pub async fn remove_user_context(&self, id: &UserContextId) -> Result<(), BrowserError> {
        self.call(Command::RemoveUserContext { id: id.clone() })
            .await?
            .into_remove_user_context()
    }

    pub async fn user_contexts(&self) -> Result<Vec<UserContextId>, BrowserError> {
        self.call(Command::UserContexts).await?.into_user_contexts()
    }

    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.client.is_closed()
    }

    pub async fn cookie_records(&self, url: &Url) -> Result<Vec<net::CookieRecord>, BrowserError> {
        self.cookie_records_in(&UserContextId::default_context(), url)
            .await
    }

    pub async fn cookie_records_in(
        &self,
        user_context: &UserContextId,
        url: &Url,
    ) -> Result<Vec<net::CookieRecord>, BrowserError> {
        self.call(Command::CookieRecords {
            user_context: user_context.clone(),
            url: url.clone(),
        })
        .await?
        .into_cookie_records()
    }

    pub async fn all_cookie_records_in(
        &self,
        user_context: &UserContextId,
    ) -> Result<Vec<net::CookieRecord>, BrowserError> {
        self.call(Command::AllCookieRecords {
            user_context: user_context.clone(),
        })
        .await?
        .into_cookie_records()
    }

    pub async fn matching_cookie_records_in(
        &self,
        user_context: &UserContextId,
        filter: net::CookieFilter,
    ) -> Result<Vec<net::CookieRecord>, BrowserError> {
        self.call(Command::MatchingCookieRecords {
            user_context: user_context.clone(),
            filter,
        })
        .await?
        .into_cookie_records()
    }

    pub async fn clear_cookies(&self) -> Result<(), BrowserError> {
        self.clear_cookies_in(&UserContextId::default_context())
            .await
    }

    pub async fn clear_cookies_in(&self, user_context: &UserContextId) -> Result<(), BrowserError> {
        self.call(Command::ClearCookies {
            user_context: user_context.clone(),
        })
        .await?
        .into_clear_cookies()
    }

    pub async fn clear_cache(&self) -> Result<(), BrowserError> {
        self.clear_cache_in(&UserContextId::default_context()).await
    }

    pub async fn clear_cache_in(&self, user_context: &UserContextId) -> Result<(), BrowserError> {
        self.call(Command::ClearCache {
            user_context: user_context.clone(),
        })
        .await?
        .into_clear_cache()
    }

    pub async fn set_cache_disabled(&self, disabled: bool) -> Result<(), BrowserError> {
        self.set_cache_disabled_in(&UserContextId::default_context(), disabled)
            .await
    }

    pub async fn set_cache_disabled_in(
        &self,
        user_context: &UserContextId,
        disabled: bool,
    ) -> Result<(), BrowserError> {
        self.call(Command::SetCacheDisabled {
            user_context: user_context.clone(),
            disabled,
        })
        .await?
        .into_set_cache_disabled()
    }

    pub async fn add_cookie(&self, cookie: &str, url: &Url) -> Result<bool, BrowserError> {
        self.add_cookie_in(&UserContextId::default_context(), cookie, url)
            .await
    }

    pub async fn add_cookie_in(
        &self,
        user_context: &UserContextId,
        cookie: &str,
        url: &Url,
    ) -> Result<bool, BrowserError> {
        self.call(Command::AddCookie {
            user_context: user_context.clone(),
            cookie: cookie.to_owned(),
            url: url.clone(),
        })
        .await?
        .into_add_cookie()
    }

    pub async fn set_direct_cookie_in(
        &self,
        user_context: &UserContextId,
        cookie: net::DirectCookie,
    ) -> Result<bool, BrowserError> {
        self.call(Command::SetDirectCookie {
            user_context: user_context.clone(),
            cookie: Box::new(cookie),
        })
        .await?
        .into_add_cookie()
    }

    pub async fn delete_cookies_in(
        &self,
        user_context: &UserContextId,
        filter: net::CookieFilter,
    ) -> Result<usize, BrowserError> {
        self.call(Command::DeleteCookies {
            user_context: user_context.clone(),
            filter,
        })
        .await?
        .into_deleted_count()
    }

    pub async fn delete_cookies_for_url_in(
        &self,
        user_context: &UserContextId,
        url: &Url,
        name: Option<&str>,
    ) -> Result<usize, BrowserError> {
        self.call(Command::DeleteCookiesForUrl {
            user_context: user_context.clone(),
            url: url.clone(),
            name: name.map(str::to_owned),
        })
        .await?
        .into_deleted_count()
    }

    pub(crate) async fn open_window(
        &self,
        request: OpenWindowOptions,
    ) -> Result<TabId, BrowserError> {
        self.call(Command::OpenWindow(request))
            .await?
            .into_open_window()
    }

    pub async fn close(&self) -> io::Result<()> {
        let result = self
            .call(Command::Close)
            .await
            .map_err(|_| stopped())?
            .into_close()?;
        self.client.wait_closed().await;
        result
    }

    async fn call(&self, command: Command) -> Result<Reply, BrowserError> {
        self.client
            .call(command)
            .await
            .map_err(|_| BrowserError::Stopped)
    }

    fn request_close(&self) {
        let _result = self.client.try_notify(Notice::Close);
    }
}

fn stopped() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "browser stopped")
}

#[derive(Debug)]
pub enum BrowserOpenError {
    Profile(io::Error),
    RuntimeUnavailable,
}

impl fmt::Display for BrowserOpenError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Profile(error) => write!(formatter, "profile failed: {error}"),
            Self::RuntimeUnavailable => formatter.write_str("browser runtime unavailable"),
        }
    }
}

impl std::error::Error for BrowserOpenError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Profile(error) => Some(error),
            Self::RuntimeUnavailable => None,
        }
    }
}

impl From<BrowserOpenError> for io::Error {
    fn from(error: BrowserOpenError) -> Self {
        match error {
            BrowserOpenError::Profile(error) => {
                Self::new(error.kind(), format!("profile failed: {error}"))
            }
            BrowserOpenError::RuntimeUnavailable => Self::other("browser runtime unavailable"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrowserError {
    UnknownTab,
    UnknownUserContext,
    TabIdExhausted,
    TooManyTabs,
    Stopped,
    Protocol,
}

impl fmt::Display for BrowserError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownTab => formatter.write_str("unknown tab"),
            Self::UnknownUserContext => formatter.write_str("unknown user context"),
            Self::TabIdExhausted => formatter.write_str("tab identity space exhausted"),
            Self::TooManyTabs => formatter.write_str("script-created tab limit reached"),
            Self::Stopped => formatter.write_str("browser stopped"),
            Self::Protocol => formatter.write_str("browser protocol mismatch"),
        }
    }
}

impl std::error::Error for BrowserError {}
