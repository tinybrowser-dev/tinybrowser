mod task;

pub(crate) use self::task::{TabNames, TabRegistry};

use std::convert::Infallible;
use std::fmt;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use url::Url;

use self::task::BrowserTask;
use crate::actor::{NavigationInitiator, TabHandle, TabId};
use crate::context::BrowserContext;
use crate::exchange;
use crate::network::NetworkContext;
use crate::profile::Profile;
use crate::profile::default_data_home;

const BROWSER_COMMAND_CAPACITY: usize = 256;

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
    CreateTab,
    Tabs,
    Tab { id: TabId },
    CloseTab { id: TabId },
    Close,
    CookieRecords { url: Url },
    ClearCookies,
    AddCookie { cookie: String, url: Url },
    OpenWindow(OpenWindowOptions),
}

pub(super) enum Reply {
    CreateTab(Result<TabHandle, BrowserError>),
    Tabs(Vec<TabId>),
    Tab(Result<TabHandle, BrowserError>),
    CloseTab(Result<(), BrowserError>),
    Close(io::Result<()>),
    CookieRecords(Vec<net::CookieRecord>),
    ClearCookies,
    AddCookie(bool),
    OpenWindow(Result<TabId, BrowserError>),
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
        let network = NetworkContext::new(options.network);
        let identity = Arc::new(network.identity());
        let data_home = match options.data_home {
            Some(data_home) => data_home,
            None => default_data_home().map_err(BrowserOpenError::Profile)?,
        };
        let context = BrowserContext::open(&data_home, &options.profile, network)
            .map_err(BrowserOpenError::Profile)?;
        let (client, server) = exchange::local(BROWSER_COMMAND_CAPACITY);
        let handle = BrowserHandle { client, identity };
        let task = BrowserTask::new(server, context, handle.clone());
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

    pub async fn create_tab(&self) -> Result<TabHandle, BrowserError> {
        match self.call(Command::CreateTab).await? {
            Reply::CreateTab(result) => result,
            _ => Err(BrowserError::Protocol),
        }
    }

    pub async fn tabs(&self) -> Result<Vec<TabId>, BrowserError> {
        match self.call(Command::Tabs).await? {
            Reply::Tabs(tabs) => Ok(tabs),
            _ => Err(BrowserError::Protocol),
        }
    }

    pub async fn tab(&self, id: TabId) -> Result<TabHandle, BrowserError> {
        match self.call(Command::Tab { id }).await? {
            Reply::Tab(result) => result,
            _ => Err(BrowserError::Protocol),
        }
    }

    pub async fn close_tab(&self, id: TabId) -> Result<(), BrowserError> {
        match self.call(Command::CloseTab { id }).await? {
            Reply::CloseTab(result) => result,
            _ => Err(BrowserError::Protocol),
        }
    }

    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.client.is_closed()
    }

    pub async fn cookie_records(&self, url: &Url) -> Result<Vec<net::CookieRecord>, BrowserError> {
        match self
            .call(Command::CookieRecords { url: url.clone() })
            .await?
        {
            Reply::CookieRecords(records) => Ok(records),
            _ => Err(BrowserError::Protocol),
        }
    }

    pub async fn clear_cookies(&self) -> Result<(), BrowserError> {
        match self.call(Command::ClearCookies).await? {
            Reply::ClearCookies => Ok(()),
            _ => Err(BrowserError::Protocol),
        }
    }

    pub async fn add_cookie(&self, cookie: &str, url: &Url) -> Result<bool, BrowserError> {
        match self
            .call(Command::AddCookie {
                cookie: cookie.to_owned(),
                url: url.clone(),
            })
            .await?
        {
            Reply::AddCookie(stored) => Ok(stored),
            _ => Err(BrowserError::Protocol),
        }
    }

    pub(crate) async fn open_window(
        &self,
        request: OpenWindowOptions,
    ) -> Result<TabId, BrowserError> {
        match self.call(Command::OpenWindow(request)).await? {
            Reply::OpenWindow(result) => result,
            _ => Err(BrowserError::Protocol),
        }
    }

    pub async fn close(&self) -> io::Result<()> {
        match self.call(Command::Close).await.map_err(|_| stopped())? {
            Reply::Close(result) => {
                self.client.wait_closed().await;
                result
            }
            _ => Err(io::Error::other("browser protocol mismatch")),
        }
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
    TabIdExhausted,
    Stopped,
    Protocol,
}

impl fmt::Display for BrowserError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownTab => formatter.write_str("unknown tab"),
            Self::TabIdExhausted => formatter.write_str("tab identity space exhausted"),
            Self::Stopped => formatter.write_str("browser stopped"),
            Self::Protocol => formatter.write_str("browser protocol mismatch"),
        }
    }
}

impl std::error::Error for BrowserError {}
