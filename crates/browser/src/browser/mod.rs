mod task;

pub(crate) use self::task::{TabNames, TabRegistry};

use std::fmt;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use tokio::sync::{broadcast, mpsc, oneshot, watch};
use url::Url;

use self::task::BrowserTask;
use crate::actor::{NavigationInitiator, TabHandle, TabId};
use crate::automation::{AutomationSession, AutomationSessionId};
use crate::child::RendererSandbox;
use crate::context::{BrowserContext, UserContextId};
use crate::intercept::NetworkBroker;
use crate::manager::{RendererProcessManager, RendererState, SpawnError};
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
    pub sandbox: RendererSandbox,
}

pub struct Browser {
    handle: BrowserHandle,
}

#[derive(Clone)]
pub struct BrowserHandle {
    commands: mpsc::Sender<Command>,
    closed: watch::Receiver<bool>,
    context: Arc<BrowserContext>,
    tabs: TabRegistry,
    renderers: Arc<OnceLock<Arc<RendererProcessManager>>>,
    // https://wicg.github.io/ua-client-hints/#http-ua-hints
    identity: Arc<net::ValidIdentity>,
    lifecycle: broadcast::Sender<BrowserLifecycleEvent>,
    network_broker: Arc<NetworkBroker>,
    prompt_broker: Arc<PromptBroker>,
    next_automation_session: Arc<AtomicU64>,
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
    // https://html.spec.whatwg.org/multipage/browsers.html#process-isolation
    pub(crate) opener_assignment: Option<Arc<crate::assignment::Assignment>>,
}

pub(crate) struct OpenedTab {
    pub(crate) handle: TabHandle,
    pub(crate) local: Option<renderer::LocalWindowInit>,
}

pub(super) enum Command {
    CreateTab {
        user_context: UserContextId,
        reply: oneshot::Sender<Result<TabHandle, BrowserError>>,
    },
    CloseTab {
        id: TabId,
        reply: oneshot::Sender<Result<(), BrowserError>>,
    },
    CreateUserContext {
        reply: oneshot::Sender<UserContextId>,
    },
    RemoveUserContext {
        id: UserContextId,
        reply: oneshot::Sender<Result<(), BrowserError>>,
    },
    OpenWindow {
        options: Box<OpenWindowOptions>,
        reply: oneshot::Sender<Result<OpenedTab, BrowserError>>,
    },
    Close {
        reply: oneshot::Sender<io::Result<()>>,
    },
    RequestClose,
}

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
        let context = Arc::new(
            BrowserContext::open(
                &data_home,
                &options.profile,
                options.network,
                Arc::clone(&network_broker),
            )
            .map_err(BrowserOpenError::Profile)?,
        );
        let identity = Arc::new(context.identity());
        let (commands, commands_rx) = mpsc::channel(BROWSER_COMMAND_CAPACITY);
        let (closed_tx, closed) = watch::channel(false);
        let (lifecycle, _) = broadcast::channel(LIFECYCLE_EVENT_CAPACITY);
        let tabs = TabRegistry::new();
        let renderers_slot = Arc::new(OnceLock::new());
        let handle = BrowserHandle {
            commands,
            closed,
            context: Arc::clone(&context),
            tabs: tabs.clone(),
            renderers: Arc::clone(&renderers_slot),
            identity: Arc::clone(&identity),
            lifecycle: lifecycle.clone(),
            network_broker: Arc::clone(&network_broker),
            prompt_broker: Arc::clone(&prompt_broker),
            next_automation_session: Arc::new(AtomicU64::new(1)),
        };
        let renderers = Arc::new(RendererProcessManager::new(
            context.session_storage(),
            handle.clone(),
            tabs.clone(),
            (*identity).clone(),
            options.sandbox,
        ));
        let _ = renderers_slot.set(Arc::clone(&renderers));
        renderers.warm_spare();
        let task = BrowserTask::new(
            commands_rx,
            closed_tx,
            context,
            tabs,
            renderers,
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

    #[must_use]
    pub fn open_automation_session(&self) -> AutomationSession {
        let raw = self.next_automation_session.fetch_add(1, Ordering::Relaxed);
        AutomationSession::new(AutomationSessionId::new(raw), self.clone())
    }

    fn ensure_open(&self) -> Result<(), BrowserError> {
        if self.is_closed() {
            Err(BrowserError::Stopped)
        } else {
            Ok(())
        }
    }

    async fn call<T>(
        &self,
        build: impl FnOnce(oneshot::Sender<T>) -> Command,
    ) -> Result<T, BrowserError> {
        let (tx, rx) = oneshot::channel();
        self.commands
            .send(build(tx))
            .await
            .map_err(|_| BrowserError::Stopped)?;
        rx.await.map_err(|_| BrowserError::Stopped)
    }

    pub async fn create_tab(&self) -> Result<TabHandle, BrowserError> {
        self.create_tab_in(UserContextId::default_context()).await
    }

    pub async fn create_tab_in(
        &self,
        user_context: UserContextId,
    ) -> Result<TabHandle, BrowserError> {
        self.call(|reply| Command::CreateTab {
            user_context,
            reply,
        })
        .await?
    }

    pub fn tabs(&self) -> Result<Vec<TabId>, BrowserError> {
        self.ensure_open()?;
        Ok(self.tabs.ids())
    }

    pub fn tabs_in(&self, user_context: &UserContextId) -> Result<Vec<TabId>, BrowserError> {
        self.ensure_open()?;
        Ok(self.tabs.ids_in(user_context))
    }

    pub fn tab(&self, id: TabId) -> Result<TabHandle, BrowserError> {
        self.ensure_open()?;
        self.tabs.tab(id).ok_or(BrowserError::UnknownTab)
    }

    pub async fn close_tab(&self, id: TabId) -> Result<(), BrowserError> {
        self.call(|reply| Command::CloseTab { id, reply }).await?
    }

    pub async fn create_user_context(&self) -> Result<UserContextId, BrowserError> {
        self.call(|reply| Command::CreateUserContext { reply })
            .await
    }

    pub async fn remove_user_context(&self, id: &UserContextId) -> Result<(), BrowserError> {
        self.call(|reply| Command::RemoveUserContext {
            id: id.clone(),
            reply,
        })
        .await?
    }

    pub fn user_contexts(&self) -> Result<Vec<UserContextId>, BrowserError> {
        self.ensure_open()?;
        Ok(self.context.user_contexts())
    }

    #[must_use]
    pub fn is_closed(&self) -> bool {
        *self.closed.borrow() || self.commands.is_closed()
    }

    pub async fn ensure_renderer_ready(&self) -> io::Result<()> {
        if self.is_closed() {
            return Err(stopped());
        }
        let Some(renderers) = self.renderers.get() else {
            return Err(stopped());
        };
        renderers.ensure_ready().await.map_err(io::Error::from)
    }

    pub async fn renderer_health(&self) -> RendererState {
        if self.is_closed() {
            return RendererState::Failed(SpawnError::Stopped);
        }
        let Some(renderers) = self.renderers.get() else {
            return RendererState::Failed(SpawnError::Stopped);
        };
        renderers.health().await
    }

    pub fn cookie_records(&self, url: &Url) -> Result<Vec<net::CookieRecord>, BrowserError> {
        self.cookie_records_in(&UserContextId::default_context(), url)
    }

    pub fn cookie_records_in(
        &self,
        user_context: &UserContextId,
        url: &Url,
    ) -> Result<Vec<net::CookieRecord>, BrowserError> {
        self.ensure_open()?;
        Ok(self.context.cookie_records(user_context, url))
    }

    pub fn all_cookie_records_in(
        &self,
        user_context: &UserContextId,
    ) -> Result<Vec<net::CookieRecord>, BrowserError> {
        self.ensure_open()?;
        Ok(self.context.all_cookie_records(user_context))
    }

    pub fn matching_cookie_records_in(
        &self,
        user_context: &UserContextId,
        filter: &net::CookieFilter,
    ) -> Result<Vec<net::CookieRecord>, BrowserError> {
        self.ensure_open()?;
        Ok(self.context.matching_cookie_records(user_context, filter))
    }

    pub fn clear_cookies(&self) -> Result<(), BrowserError> {
        self.clear_cookies_in(&UserContextId::default_context())
    }

    pub fn clear_cookies_in(&self, user_context: &UserContextId) -> Result<(), BrowserError> {
        self.ensure_open()?;
        self.context.clear_cookies(user_context);
        Ok(())
    }

    pub fn clear_cache(&self) -> Result<(), BrowserError> {
        self.clear_cache_in(&UserContextId::default_context())
    }

    pub fn clear_cache_in(&self, user_context: &UserContextId) -> Result<(), BrowserError> {
        self.ensure_open()?;
        self.context.clear_cache(user_context);
        Ok(())
    }

    pub fn set_cache_disabled(&self, disabled: bool) -> Result<(), BrowserError> {
        self.set_cache_disabled_in(&UserContextId::default_context(), disabled)
    }

    pub fn set_cache_disabled_in(
        &self,
        user_context: &UserContextId,
        disabled: bool,
    ) -> Result<(), BrowserError> {
        self.ensure_open()?;
        self.context.set_cache_disabled(user_context, disabled);
        Ok(())
    }

    pub fn add_cookie(&self, cookie: &str, url: &Url) -> Result<bool, BrowserError> {
        self.add_cookie_in(&UserContextId::default_context(), cookie, url)
    }

    pub fn add_cookie_in(
        &self,
        user_context: &UserContextId,
        cookie: &str,
        url: &Url,
    ) -> Result<bool, BrowserError> {
        self.ensure_open()?;
        Ok(self.context.add_cookie(user_context, cookie, url))
    }

    pub fn set_direct_cookie_in(
        &self,
        user_context: &UserContextId,
        cookie: &net::DirectCookie,
    ) -> Result<bool, BrowserError> {
        self.ensure_open()?;
        Ok(self.context.set_direct_cookie(user_context, cookie))
    }

    pub fn delete_cookies_in(
        &self,
        user_context: &UserContextId,
        filter: &net::CookieFilter,
    ) -> Result<usize, BrowserError> {
        self.ensure_open()?;
        Ok(self.context.delete_cookies(user_context, filter))
    }

    pub fn delete_cookies_for_url_in(
        &self,
        user_context: &UserContextId,
        url: &Url,
        name: Option<&str>,
    ) -> Result<usize, BrowserError> {
        self.ensure_open()?;
        Ok(self.context.delete_cookies_for_url(user_context, url, name))
    }

    pub(crate) async fn open_window(
        &self,
        options: OpenWindowOptions,
    ) -> Result<OpenedTab, BrowserError> {
        self.call(|reply| Command::OpenWindow {
            options: Box::new(options),
            reply,
        })
        .await?
    }

    pub async fn close(&self) -> io::Result<()> {
        let result = self
            .call(|reply| Command::Close { reply })
            .await
            .map_err(|_| stopped())?;
        let mut closed = self.closed.clone();
        while !*closed.borrow_and_update() {
            if closed.changed().await.is_err() {
                break;
            }
        }
        result
    }

    fn request_close(&self) {
        let _result = self.commands.try_send(Command::RequestClose);
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
}

impl fmt::Display for BrowserError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownTab => formatter.write_str("unknown tab"),
            Self::UnknownUserContext => formatter.write_str("unknown user context"),
            Self::TabIdExhausted => formatter.write_str("tab identity space exhausted"),
            Self::TooManyTabs => formatter.write_str("script-created tab limit reached"),
            Self::Stopped => formatter.write_str("browser stopped"),
        }
    }
}

impl std::error::Error for BrowserError {}
