use std::convert::Infallible;
use std::fmt;
use std::future::pending;
use std::sync::Arc;
use std::time::Duration;

use crate::wire::{Command as RendererCommand, Reply};
use renderer::{
    BrowserEvent, ConsoleLevel, DialFailure, FrameId, MAX_RESPONSE_BODY_BYTES, Mount, RemoteValue,
    ResourceLimit, ResponseHead, TabError,
};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep_until, timeout};
use url::Url;

use crate::assignment::{Assignment, RendererWake};
use crate::browser::{TabNames, TabRegistry};
use crate::context::{PartitionServices, UserContextId};
use crate::exchange::{self, RequestId, ServerInput};
use crate::frames::FrameInfo;
use crate::history::{Commit, EntryId, HistorySnapshot, HistoryStep, SessionHistory};
use crate::manager::RendererProcessManager;
use crate::network::{NavBody, NavOutcome, TabNetworkHandle, dial_failure};
use crate::site::Site;

const EVENT_SUBSCRIBER_CAPACITY: usize = 256;
const COMMAND_CAPACITY: usize = 256;
const DELIVERY_CAPACITY: usize = 256;
const MAX_WAITERS: usize = 256;
const MAX_SUBSCRIBERS: usize = 256;
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

pub(crate) fn log_url(url: &Url) -> String {
    let mut logged = url.clone();
    if logged.set_username("").is_ok() {
        let _ = logged.set_password(None);
    }
    logged.set_query(None);
    logged.set_fragment(None);
    logged.to_string()
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub struct TabId(u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavigationFailure {
    Aborted,
    ConnectionRefused,
    CertAuthorityInvalid,
    Failed,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TabEvent {
    Navigated,
    SameDocumentNavigation,
    Load,
    DomContentLoaded,
    NavigationFailed(NavigationFailure),
    // https://chromedevtools.github.io/devtools-protocol/tot/Network/#event-requestWillBeSent
    NavigationRequested {
        url: String,
    },
    // https://chromedevtools.github.io/devtools-protocol/tot/Network/#event-responseReceived
    NavigationResponded {
        status: u16,
        status_text: String,
        url: String,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    },
    // https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#document-open-steps
    RealmReplaced,
    // https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#event-bindingCalled
    Binding {
        name: String,
        payload: String,
    },
    // https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#event-consoleAPICalled
    Console {
        level: ConsoleLevel,
        args: String,
    },
    // https://chromedevtools.github.io/devtools-protocol/tot/Page/#event-frameAttached
    FrameAttached {
        frame: FrameId,
        parent: FrameId,
        url: String,
    },
    // https://chromedevtools.github.io/devtools-protocol/tot/Page/#event-frameNavigated
    FrameNavigated {
        frame: FrameId,
        parent: FrameId,
        url: String,
    },
    // https://chromedevtools.github.io/devtools-protocol/tot/Page/#event-frameDetached
    FrameDetached {
        frame: FrameId,
    },
    // https://w3c.github.io/webdriver-bidi/#event-browsingContext-domContentLoaded
    FrameDomContentLoaded {
        frame: FrameId,
        url: String,
    },
    // https://w3c.github.io/webdriver-bidi/#event-browsingContext-load
    FrameLoad {
        frame: FrameId,
        url: String,
    },
    // https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#event-exceptionThrown
    ExceptionThrown {
        frame: FrameId,
        text: String,
        filename: String,
        line: u32,
        column: u32,
    },
}

impl TabId {
    #[must_use]
    pub fn new(raw: u64) -> Self {
        Self(raw)
    }

    #[must_use]
    pub fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for TabId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

struct GotoRequest {
    url: String,
    initiator: Option<NavigationInitiator>,
}

enum Command {
    LoadHtml {
        html: String,
        reply: ReplySender<()>,
    },
    Goto {
        request: GotoRequest,
        reply: ReplySender<()>,
    },
    Execute {
        frame: FrameId,
        source: renderer::ScriptSource,
        timeout: Option<Duration>,
        reply: ReplySender<RemoteValue>,
    },
    AddInitScript {
        source: String,
        run_immediately: bool,
        reply: ReplySender<u64>,
    },
    // https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#method-addBinding
    AddBinding {
        name: String,
        run_immediately: bool,
        reply: ReplySender<()>,
    },
    RemoveInitScript {
        id: u64,
        reply: ReplySender<()>,
    },
    SetViewport {
        frame: FrameId,
        width: u32,
        height: u32,
        reply: ReplySender<()>,
    },
    SetIdentity {
        identity: Option<Box<net::ValidIdentity>>,
        reply: ReplySender<()>,
    },
    SetExtraHeaders {
        headers: Vec<(String, String)>,
        reply: ReplySender<()>,
    },
    ViewportSize {
        reply: ReplySender<(u32, u32)>,
    },
    ContentSize {
        frame: FrameId,
        reply: ReplySender<(f32, f32)>,
    },
    Dom {
        frame: FrameId,
        request: renderer::DomRequest,
        reply: ReplySender<renderer::DomReply>,
    },
    Screenshot {
        frame: FrameId,
        request: renderer::ScreenshotRequest,
        reply: ReplySender<Vec<u8>>,
    },
    RunUntilLoadTimeout {
        timeout: Duration,
        reply: ReplySender<bool>,
    },
    RunUntilJsTrue {
        frame: FrameId,
        source: renderer::ScriptSource,
        timeout: Duration,
        reply: ReplySender<bool>,
    },
    DocumentUrl {
        frame: FrameId,
        reply: ReplySender<String>,
    },
    DocumentTitle {
        frame: FrameId,
        reply: ReplySender<String>,
    },
    Frames {
        reply: ReplySender<Vec<FrameInfo>>,
    },
    Reload {
        reply: ReplySender<()>,
    },
    StopLoading {
        reply: ReplySender<()>,
    },
    Opener {
        reply: ReplySender<Option<TabId>>,
    },
    WindowName {
        reply: ReplySender<Option<String>>,
    },
    History {
        reply: ReplySender<HistorySnapshot>,
    },
    TraverseHistory {
        step: HistoryStep,
        reply: ReplySender<()>,
    },
    LastNavigationFailed {
        reply: ReplySender<bool>,
    },
    Shutdown {
        reply: ReplySender<()>,
    },
}

type ReplySender<T> = oneshot::Sender<Result<T, TabError>>;

type TabClient = exchange::Client<Command, Infallible, Infallible, (), TabEvent>;
type TabServer = exchange::Server<Infallible, (), TabEvent, Command, Infallible>;
type TabNotifier = exchange::Notifier<Infallible, (), TabEvent>;

struct ServeContext<'a> {
    tab: &'a mut Tab,
    waiters: &'a mut Vec<Waiter>,
    id: RequestId,
}

enum TabOutcome {
    Continue,
    Retained,
    Shutdown,
}

struct CommitNames {
    previous: Option<TabNames>,
    installed: TabNames,
}

struct Waiter {
    id: RequestId,
    deadline: Instant,
    kind: WaiterKind,
    reply: ReplySender<bool>,
}

enum WaiterKind {
    Load,
    Js { renderer_id: u64, frame: FrameId },
}

impl Waiter {
    fn renderer_waiter(&self) -> Option<u64> {
        match self.kind {
            WaiterKind::Load => None,
            WaiterKind::Js { renderer_id, .. } => Some(renderer_id),
        }
    }
}

#[derive(Clone)]
pub struct TabHandle {
    id: TabId,
    user_context: UserContextId,
    client: TabClient,
}

pub struct ExecuteScriptOptions<'a> {
    pub frame: FrameId,
    pub source: renderer::ScriptSource<&'a str>,
    pub timeout: Option<Duration>,
}

pub struct RunUntilJsTrueOptions<'a> {
    pub frame: FrameId,
    pub source: renderer::ScriptSource<&'a str>,
    pub timeout: Duration,
}

impl TabHandle {
    #[must_use]
    pub fn id(&self) -> TabId {
        self.id
    }

    #[must_use]
    pub fn user_context(&self) -> &UserContextId {
        &self.user_context
    }

    pub async fn load_html(&self, html: &str) -> Result<(), TabError> {
        self.ask(|reply| Command::LoadHtml {
            html: html.to_owned(),
            reply,
        })
        .await
    }

    pub async fn goto(&self, url: &str) -> Result<(), TabError> {
        self.goto_with_referrer(url, None).await
    }

    // https://chromedevtools.github.io/devtools-protocol/tot/Page/#method-navigate
    pub async fn goto_with_referrer(
        &self,
        url: &str,
        referrer: Option<&str>,
    ) -> Result<(), TabError> {
        let initiator = referrer
            .and_then(|spec| Url::parse(spec).ok())
            .map(|referrer_url| NavigationInitiator {
                origin: referrer_url.origin(),
                referrer: Some(referrer_url),
                referrer_policy: net::ReferrerPolicy::UnsafeUrl,
                secure_context: true,
            });
        self.ask(|reply| Command::Goto {
            request: GotoRequest {
                url: url.to_owned(),
                initiator,
            },
            reply,
        })
        .await
    }

    // https://html.spec.whatwg.org/multipage/nav-history-apis.html#window-open-steps
    pub(crate) async fn goto_from(
        &self,
        url: &str,
        initiator: NavigationInitiator,
    ) -> Result<(), TabError> {
        self.ask(|reply| Command::Goto {
            request: GotoRequest {
                url: url.to_owned(),
                initiator: Some(initiator),
            },
            reply,
        })
        .await
    }

    pub async fn execute_script(
        &self,
        options: ExecuteScriptOptions<'_>,
    ) -> Result<RemoteValue, TabError> {
        let ExecuteScriptOptions {
            frame,
            source,
            timeout,
        } = options;
        self.ask(|reply| Command::Execute {
            frame,
            source: source.into_owned(),
            timeout,
            reply,
        })
        .await
    }

    pub async fn execute_browser_script(&self, source: &str) -> Result<RemoteValue, TabError> {
        self.execute_script(ExecuteScriptOptions {
            frame: FrameId::MAIN,
            source: renderer::ScriptSource::Browser(source),
            timeout: None,
        })
        .await
    }

    pub async fn viewport_size(&self) -> Result<(u32, u32), TabError> {
        self.ask(|reply| Command::ViewportSize { reply }).await
    }

    pub async fn content_size(&self) -> Result<(f32, f32), TabError> {
        self.ask(|reply| Command::ContentSize {
            frame: FrameId::MAIN,
            reply,
        })
        .await
    }

    pub async fn dom(&self, request: renderer::DomRequest) -> Result<renderer::DomReply, TabError> {
        self.dom_in(FrameId::MAIN, request).await
    }

    pub async fn dom_in(
        &self,
        frame: FrameId,
        request: renderer::DomRequest,
    ) -> Result<renderer::DomReply, TabError> {
        self.ask(|reply| Command::Dom {
            frame,
            request,
            reply,
        })
        .await
    }

    pub async fn set_viewport(&self, width: u32, height: u32) -> Result<(), TabError> {
        self.ask(|reply| Command::SetViewport {
            frame: FrameId::MAIN,
            width,
            height,
            reply,
        })
        .await
    }

    // https://chromedevtools.github.io/devtools-protocol/tot/Emulation/#method-setUserAgentOverride
    pub async fn set_identity(&self, identity: Option<net::ValidIdentity>) -> Result<(), TabError> {
        self.ask(|reply| Command::SetIdentity {
            identity: identity.map(Box::new),
            reply,
        })
        .await
    }

    // https://chromedevtools.github.io/devtools-protocol/tot/Network/#method-setExtraHTTPHeaders
    pub async fn set_extra_headers(&self, headers: Vec<(String, String)>) -> Result<(), TabError> {
        self.ask(|reply| Command::SetExtraHeaders { headers, reply })
            .await
    }

    pub async fn add_init_script(
        &self,
        source: String,
        run_immediately: bool,
    ) -> Result<u64, TabError> {
        self.ask(|reply| Command::AddInitScript {
            source,
            run_immediately,
            reply,
        })
        .await
    }

    pub async fn remove_init_script(&self, id: u64) -> Result<(), TabError> {
        self.ask(|reply| Command::RemoveInitScript { id, reply })
            .await
    }

    // https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#method-addBinding
    pub async fn add_binding(&self, name: &str, run_immediately: bool) -> Result<(), TabError> {
        self.ask(|reply| Command::AddBinding {
            name: name.to_owned(),
            run_immediately,
            reply,
        })
        .await
    }

    pub async fn screenshot(
        &self,
        request: renderer::ScreenshotRequest,
    ) -> Result<Vec<u8>, TabError> {
        self.screenshot_frame(FrameId::MAIN, request).await
    }

    pub async fn screenshot_frame(
        &self,
        frame: FrameId,
        request: renderer::ScreenshotRequest,
    ) -> Result<Vec<u8>, TabError> {
        self.ask(|reply| Command::Screenshot {
            frame,
            request,
            reply,
        })
        .await
    }

    pub async fn run_until_load_timeout(&self, timeout: Duration) -> Result<bool, TabError> {
        self.ask(|reply| Command::RunUntilLoadTimeout { timeout, reply })
            .await
    }

    pub async fn run_until_js_true(
        &self,
        options: RunUntilJsTrueOptions<'_>,
    ) -> Result<bool, TabError> {
        let RunUntilJsTrueOptions {
            frame,
            source,
            timeout,
        } = options;
        self.ask(|reply| Command::RunUntilJsTrue {
            frame,
            source: source.into_owned(),
            timeout,
            reply,
        })
        .await
    }

    pub async fn document_url(&self) -> Result<String, TabError> {
        self.document_url_in(FrameId::MAIN).await
    }

    pub async fn document_url_in(&self, frame: FrameId) -> Result<String, TabError> {
        self.ask(|reply| Command::DocumentUrl { frame, reply })
            .await
    }

    pub async fn document_title(&self) -> Result<String, TabError> {
        self.document_title_in(FrameId::MAIN).await
    }

    pub async fn document_title_in(&self, frame: FrameId) -> Result<String, TabError> {
        self.ask(|reply| Command::DocumentTitle { frame, reply })
            .await
    }

    pub async fn frames(&self) -> Result<Vec<FrameInfo>, TabError> {
        self.ask(|reply| Command::Frames { reply }).await
    }

    pub async fn reload(&self) -> Result<(), TabError> {
        self.ask(|reply| Command::Reload { reply }).await
    }

    pub async fn stop_loading(&self) -> Result<(), TabError> {
        self.ask(|reply| Command::StopLoading { reply }).await
    }

    pub async fn opener(&self) -> Result<Option<TabId>, TabError> {
        self.ask(|reply| Command::Opener { reply }).await
    }

    pub async fn window_name(&self) -> Result<Option<String>, TabError> {
        self.ask(|reply| Command::WindowName { reply }).await
    }

    pub async fn history(&self) -> Result<HistorySnapshot, TabError> {
        self.ask(|reply| Command::History { reply }).await
    }

    pub async fn traverse_history(&self, delta: i32) -> Result<(), TabError> {
        self.ask(|reply| Command::TraverseHistory {
            step: HistoryStep::Delta(delta),
            reply,
        })
        .await
    }

    pub async fn traverse_to_entry(&self, entry: EntryId) -> Result<(), TabError> {
        self.ask(|reply| Command::TraverseHistory {
            step: HistoryStep::Entry(entry),
            reply,
        })
        .await
    }

    pub fn subscribe(&self) -> Result<mpsc::Receiver<TabEvent>, TabError> {
        self.client
            .subscribe(EVENT_SUBSCRIBER_CAPACITY, MAX_SUBSCRIBERS)
            .map_err(|error| match error {
                exchange::Error::SubscriberLimit => TabError::ResourceLimit {
                    resource: ResourceLimit::TabSubscribers,
                },
                exchange::Error::Closed | exchange::Error::Protocol => TabError::ActorStopped,
            })
    }

    pub async fn last_navigation_failed(&self) -> Result<bool, TabError> {
        self.ask(|reply| Command::LastNavigationFailed { reply })
            .await
    }

    async fn ask<T>(&self, command: impl FnOnce(ReplySender<T>) -> Command) -> Result<T, TabError> {
        let (reply, result) = oneshot::channel();
        self.client
            .call(command(reply))
            .await
            .map_err(|_| TabError::ActorStopped)?;
        result.await.unwrap_or(Err(TabError::ActorStopped))
    }
}

pub(crate) struct RemoteWindowMessage {
    pub(crate) message: renderer::PostedMessage,
}

pub(crate) struct TabTask {
    pub handle: TabHandle,
    deliveries: mpsc::Sender<RemoteWindowMessage>,
    join: Option<JoinHandle<()>>,
}

impl TabTask {
    pub(crate) fn spawn(
        id: TabId,
        network: TabNetworkHandle,
        partition: PartitionServices,
        renderers: Arc<RendererProcessManager>,
        tabs: TabRegistry,
        tab: NewTab,
    ) -> Self {
        let (client, server) = exchange::local(COMMAND_CAPACITY);
        let handle = TabHandle {
            id,
            user_context: tab.user_context.clone(),
            client,
        };
        let tab = Tab::new(
            id,
            network.with_tab(id),
            partition,
            renderers,
            server.notifier(),
            tabs,
            tab,
        );
        let (deliveries, delivery_rx) = mpsc::channel(DELIVERY_CAPACITY);
        let join = tokio::spawn(coordinator_loop(server, tab, delivery_rx));
        Self {
            handle,
            deliveries,
            join: Some(join),
        }
    }

    #[cfg(test)]
    pub(crate) fn fixture(id: TabId) -> Self {
        let (client, _server) = exchange::local(1);
        let (deliveries, _receiver) = mpsc::channel(1);
        Self {
            handle: TabHandle {
                id,
                user_context: UserContextId::default_context(),
                client,
            },
            deliveries,
            join: None,
        }
    }

    pub(crate) fn deliveries(&self) -> mpsc::Sender<RemoteWindowMessage> {
        self.deliveries.clone()
    }

    pub(crate) async fn shutdown(&mut self) {
        let _result = timeout(
            SHUTDOWN_TIMEOUT,
            self.handle.ask(|reply| Command::Shutdown { reply }),
        )
        .await;
        if let Some(mut join) = self.join.take()
            && timeout(SHUTDOWN_TIMEOUT, &mut join).await.is_err()
        {
            join.abort();
            let _result = join.await;
        }
    }
}

impl Drop for TabTask {
    fn drop(&mut self) {
        if let Some(join) = self.join.take() {
            join.abort();
        }
    }
}

// https://w3c.github.io/webappsec-referrer-policy/#determine-requests-referrer
#[derive(Clone, Debug)]
pub(crate) struct NavigationInitiator {
    pub(crate) origin: url::Origin,
    pub(crate) referrer: Option<Url>,
    pub(crate) referrer_policy: net::ReferrerPolicy,
    // https://html.spec.whatwg.org/multipage/webappapis.html#secure-contexts
    pub(crate) secure_context: bool,
}

struct ActiveNavigation {
    url: Url,
    initiator: Option<NavigationInitiator>,
    epoch: u64,
    submitted: bool,
}

pub(crate) struct NewTab {
    pub(crate) user_context: UserContextId,
    // https://html.spec.whatwg.org/multipage/document-sequences.html#browsing-context-name
    pub(crate) name: Option<renderer::ContextName>,
    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName
    pub(crate) opener: Option<TabId>,
    // https://html.spec.whatwg.org/multipage/browsers.html#sandboxed-navigation-browsing-context-flag
    pub(crate) opener_frame: Option<renderer::FrameId>,
    // https://html.spec.whatwg.org/multipage/browsers.html#popup-sandboxing-flag-set
    pub(crate) popup_sandbox: renderer::SandboxFlags,
    // https://html.spec.whatwg.org/multipage/browsers.html#determining-the-origin
    pub(crate) document_origin: url::Origin,
    // https://html.spec.whatwg.org/multipage/webappapis.html#secure-contexts
    pub(crate) creator_secure_context: Option<bool>,
}

impl NewTab {
    pub(crate) fn browser_created(user_context: UserContextId) -> Self {
        Self {
            user_context,
            name: None,
            opener: None,
            opener_frame: None,
            popup_sandbox: renderer::SandboxFlags::empty(),
            document_origin: url::Origin::new_opaque(),
            creator_secure_context: None,
        }
    }
}

struct Tab {
    id: TabId,
    user_context: UserContextId,
    partition: PartitionServices,
    renderers: Arc<RendererProcessManager>,
    network: TabNetworkHandle,
    renderer: Option<Arc<Assignment>>,
    pending_mount: Option<Mount>,
    // https://html.spec.whatwg.org/multipage/document-sequences.html#the-rules-for-choosing-a-navigable
    tabs: TabRegistry,
    // https://html.spec.whatwg.org/multipage/browsers.html#determining-the-origin
    document_origin: url::Origin,
    // https://html.spec.whatwg.org/multipage/browsers.html#popup-sandboxing-flag-set
    popup_sandbox: renderer::SandboxFlags,
    site: Option<Site>,
    events_rx: Option<mpsc::Receiver<RendererWake>>,
    document_url: Url,
    // https://chromedevtools.github.io/devtools-protocol/tot/Emulation/#method-setDeviceMetricsOverride — override is tab state and outlives the CDP session, unlike Chromium
    viewport: (u32, u32),
    // https://chromedevtools.github.io/devtools-protocol/tot/Page/#method-addScriptToEvaluateOnNewDocument
    init_scripts: Vec<(u64, String)>,
    // https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#method-addBinding
    bindings: Vec<String>,
    next_init_script: u64,
    history: SessionHistory,
    traversal: Option<crate::history::EntryId>,
    document_loaded: bool,
    navigation_failed: bool,
    nav: Option<ActiveNavigation>,
    nav_epoch: u64,
    dial_tx: mpsc::UnboundedSender<(u64, Result<NavOutcome, DialFailure>)>,
    dial_rx: mpsc::UnboundedReceiver<(u64, Result<NavOutcome, DialFailure>)>,
    dial_cancel: Option<watch::Sender<bool>>,
    dial_guard: Option<JoinHandle<()>>,
    events: TabNotifier,
}

impl Tab {
    fn new(
        id: TabId,
        network: TabNetworkHandle,
        partition: PartitionServices,
        renderers: Arc<RendererProcessManager>,
        events: TabNotifier,
        tabs: TabRegistry,
        tab: NewTab,
    ) -> Self {
        let (dial_tx, dial_rx) = mpsc::unbounded_channel();
        let initial_name = tabs.name(id);
        let pending_mount = blank_mount(renderer::TabState {
            id: Some(renderer::WindowId::new(id.get())),
            name: initial_name.clone(),
            auxiliary_with_opener: tabs.opener(id).is_some(),
            popup_sandbox: tab.popup_sandbox,
            secure_context: tab.creator_secure_context,
        });
        Self {
            id,
            user_context: tab.user_context,
            partition,
            renderers,
            network,
            renderer: None,
            pending_mount: Some(pending_mount),
            tabs,
            document_origin: tab.document_origin,
            popup_sandbox: tab.popup_sandbox,
            site: None,
            events_rx: None,
            document_url: net::about_blank(),
            viewport: renderer::DEFAULT_VIEWPORT,
            init_scripts: Vec::new(),
            bindings: Vec::new(),
            next_init_script: 1,
            history: SessionHistory::new(net::about_blank(), initial_name),
            traversal: None,
            document_loaded: false,
            navigation_failed: false,
            nav: None,
            nav_epoch: 0,
            dial_tx,
            dial_rx,
            dial_cancel: None,
            dial_guard: None,
            events,
        }
    }

    async fn load_html(&mut self, html: &str) -> Result<(), TabError> {
        let url = self.document_url.clone();
        let site = Site::for_url(&url)
            .or_else(|| self.site.clone())
            .unwrap_or_else(|| Site::opaque(self.id));
        let mount = Mount {
            head: ResponseHead {
                history: self.history.renderer_snapshot(),
                viewport: Some(self.viewport),
                init_scripts: self.init_scripts.clone(),
                bindings: self.bindings.clone(),
                ..ResponseHead::local(
                    url.clone(),
                    Some("text/html; charset=utf-8".to_owned()),
                    None,
                )
            },
            tab: self.current_tab_state(),
            body: html.as_bytes().to_vec(),
        };
        if self.renderer.is_none() && self.document_url.scheme() == "about" {
            self.pending_mount = Some(mount);
            self.document_loaded = true;
            self.record_event(TabEvent::DomContentLoaded).await;
            self.record_event(TabEvent::Load).await;
            self.history.set_current_title(String::new());
            return Ok(());
        }
        self.mount(&site, mount).await?;
        let title = self.document_title().await;
        self.history.set_current_title(title);
        Ok(())
    }

    async fn document_title(&mut self) -> String {
        self.document_title_in(FrameId::MAIN).await
    }

    async fn document_title_in(&mut self, frame: FrameId) -> String {
        if self.renderer.is_none() {
            return String::new();
        }
        match execute_script(
            self,
            frame,
            renderer::ScriptSource::Browser("String(document.title)".to_owned()),
            None,
        )
        .await
        {
            Ok(RemoteValue::String(title)) => title,
            _ => String::new(),
        }
    }

    fn goto(&mut self, spec: &str, initiator: Option<NavigationInitiator>) -> Result<(), TabError> {
        let url = self.resolve_url(spec)?;
        self.traversal = None;
        self.nav_epoch = self.nav_epoch.saturating_add(1);
        let epoch = self.nav_epoch;
        self.navigation_failed = false;
        self.nav = Some(ActiveNavigation {
            url,
            initiator,
            epoch,
            submitted: false,
        });
        Ok(())
    }

    fn resolve_url(&self, spec: &str) -> Result<Url, TabError> {
        let url = Url::parse(spec)
            .or_else(|_| self.document_url.join(spec))
            .map_err(|_| TabError::InvalidUrl { spec: spec.into() })?;
        if !(matches!(url.scheme(), "http" | "https" | "data")
            || (url.scheme() == "about" && url.path() == "blank"))
        {
            return Err(TabError::InvalidUrl { spec: spec.into() });
        }
        Ok(url)
    }

    async fn ensure_renderer(&mut self, site: &Site) -> Result<(), TabError> {
        if self.renderer.is_some() && self.site.as_ref() == Some(site) {
            return Ok(());
        }
        let handle = self
            .renderers
            .acquire(
                self.id,
                self.user_context.clone(),
                site.clone(),
                self.network.clone(),
                self.partition.clone(),
            )
            .await
            .map_err(|error| renderer_unavailable(&error.to_string()))?;
        self.drop_renderer();
        self.events_rx = Some(handle.subscribe());
        self.site = Some(site.clone());
        self.renderer = Some(handle);
        Ok(())
    }

    async fn mount(&mut self, site: &Site, mount: Mount) -> Result<(), TabError> {
        self.ensure_renderer(site).await?;
        self.pending_mount = None;
        self.document_loaded = false;
        let result = self
            .renderer
            .as_ref()
            .ok_or(TabError::ActorStopped)?
            .mount(FrameId::MAIN, mount)
            .await
            .and_then(reply_unit);
        if result.is_err() {
            self.drop_renderer();
        }
        result
    }

    async fn mount_stream(
        &mut self,
        site: &Site,
        mount: Mount,
        body: NavBody,
    ) -> Result<Vec<u8>, TabError> {
        match body {
            NavBody::Bytes(bytes) => {
                let captured = bytes.clone();
                self.mount(
                    site,
                    Mount {
                        body: bytes,
                        ..mount
                    },
                )
                .await?;
                Ok(captured)
            }
            NavBody::Stream(mut body) => {
                self.ensure_renderer(site).await?;
                self.pending_mount = None;
                self.document_loaded = false;
                let renderer = self.renderer.as_ref().ok_or(TabError::ActorStopped)?;
                let stream = renderer.start_response(FrameId::MAIN, &mount).await?;
                let mut received = 0usize;
                let mut captured = Vec::new();
                loop {
                    match body.read_chunk().await {
                        Ok(Some(chunk)) => {
                            received = received.saturating_add(chunk.len());
                            if received > MAX_RESPONSE_BODY_BYTES {
                                let _ = stream.abort(DialFailure::Limit).await;
                                self.drop_renderer();
                                return Err(renderer_unavailable(&format!(
                                    "navigation body exceeds {MAX_RESPONSE_BODY_BYTES} bytes ({received} received)"
                                )));
                            }
                            captured.extend_from_slice(&chunk);
                            for piece in chunk.chunks(crate::wire::channel::MAX_BODY_CHUNK_BYTES) {
                                if let Err(error) = stream.write(piece.to_vec()).await {
                                    drop(stream);
                                    self.drop_renderer();
                                    return Err(error);
                                }
                            }
                        }
                        Ok(None) => break,
                        Err(error) => {
                            let failure = dial_failure(&error);
                            let _ = stream.abort(failure).await;
                            self.drop_renderer();
                            return Err(renderer_unavailable(&error.to_string()));
                        }
                    }
                }
                let result = stream.finish().await.and_then(reply_unit);
                if result.is_err() {
                    self.drop_renderer();
                }
                result.map(|()| captured)
            }
        }
    }

    fn drop_renderer(&mut self) {
        self.site = None;
        self.events_rx = None;
        self.tabs.clear_frames(self.id);
        self.history.clear_children();
        self.renderer = None;
    }

    async fn deliver_window_message(
        &mut self,
        RemoteWindowMessage { message }: RemoteWindowMessage,
    ) -> Result<(), TabError> {
        if self.renderer.is_none() {
            self.mount_virtual().await?;
        }
        match self
            .renderer_request(RendererCommand::WindowMessage(message))
            .await?
        {
            Reply::Unit(result) => result,
            _ => Err(TabError::ActorStopped),
        }
    }

    async fn renderer_request(&mut self, command: RendererCommand) -> Result<Reply, TabError> {
        if self.renderer.is_none() {
            self.mount_virtual().await?;
        }
        let Some(renderer) = self.renderer.clone() else {
            return Err(TabError::ActorStopped);
        };
        let result = renderer.request(command).await;
        if result.is_err() {
            self.drop_renderer();
        }
        result
    }

    async fn renderer_document_state(&mut self) -> Option<renderer::DocumentLoadState> {
        self.renderer.as_ref()?;
        let reply = self
            .renderer_request(RendererCommand::DocumentState {
                frame: FrameId::MAIN,
            })
            .await;
        let Ok(Reply::DocumentState(Ok(state))) = reply else {
            return None;
        };
        Some(state)
    }

    async fn renderer_request_bytes(
        &mut self,
        command: RendererCommand,
    ) -> Result<Vec<u8>, TabError> {
        if self.renderer.is_none() {
            self.mount_virtual().await?;
        }
        let Some(renderer) = self.renderer.clone() else {
            return Err(TabError::ActorStopped);
        };
        let result = renderer.request_bytes(command).await;
        if result.is_err() {
            self.drop_renderer();
        }
        result
    }

    async fn mount_virtual(&mut self) -> Result<(), TabError> {
        let mut mount = self
            .pending_mount
            .take()
            .unwrap_or_else(|| blank_mount(self.current_tab_state()));
        mount.head.viewport = Some(self.viewport);
        mount.head.init_scripts = self.init_scripts.clone();
        mount.head.bindings = self.bindings.clone();
        let site = Site::for_url(&self.document_url)
            .or_else(|| self.site.clone())
            .unwrap_or_else(|| Site::opaque(self.id));
        self.mount(&site, mount).await
    }

    // https://fetch.spec.whatwg.org/#data-url-processor
    fn take_data_navigation(&mut self) -> Option<Url> {
        let local = self.nav.as_ref().is_some_and(|nav| {
            !nav.submitted
                && (nav.url.scheme() == "data"
                    || (nav.url.scheme() == "about" && nav.url.path() == "blank"))
        });
        if !local {
            return None;
        }
        self.nav.take().map(|nav| nav.url)
    }

    async fn commit_data_navigation(&mut self, url: Url) -> bool {
        let (content_type, body) = if url.scheme() == "about" && url.path() == "blank" {
            (
                "text/html; charset=utf-8".to_owned(),
                b"<!doctype html><title></title>".to_vec(),
            )
        } else if let Some(decoded) = renderer::decode_data_url_bounded(url.as_str()) {
            decoded
        } else {
            self.navigation_failed = true;
            return false;
        };
        let site = Site::for_url(&url).unwrap_or_else(|| Site::opaque(self.id));
        let previous_csp = self.tabs.main_csp(self.id);
        self.tabs
            .reset_main_sandbox(self.id, renderer::SandboxFlags::empty());
        self.sync_outgoing_name();
        let state = self.commit_state(&url);
        let commit = self.history_commit(&url, &state);
        let mut history = self.history.clone();
        if !history.commit(commit) {
            self.tabs.reset_main_sandbox(self.id, previous_csp);
            self.navigation_failed = true;
            return false;
        }
        let previous_names = self.begin_commit(&state);
        let mount = Mount {
            head: ResponseHead {
                history: history.renderer_snapshot(),
                viewport: Some(self.viewport),
                init_scripts: self.init_scripts.clone(),
                bindings: self.bindings.clone(),
                ..ResponseHead::local(url.clone(), Some(content_type), None)
            },
            tab: state,
            body,
        };
        if let Err(error) = self.mount(&site, mount).await {
            self.abort_commit(previous_names);
            self.tabs.reset_main_sandbox(self.id, previous_csp);
            logging::error!(
                target: "browser::tab",
                "data navigation mount failed for {}: {error}",
                log_url(&url)
            );
            self.navigation_failed = true;
            return false;
        }
        self.history = history;
        self.advance_document_url(url);
        self.navigation_failed = false;
        let title = self.document_title().await;
        self.history.set_current_title(title);
        true
    }

    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName
    fn auxiliary_with_opener(&self) -> bool {
        self.tabs
            .opener(self.id)
            .is_some_and(|opener| self.tabs.tab(opener).is_some())
    }

    fn current_tab_state(&self) -> renderer::TabState {
        renderer::TabState {
            id: Some(renderer::WindowId::new(self.id.get())),
            name: self.tabs.name(self.id),
            auxiliary_with_opener: self.auxiliary_with_opener(),
            popup_sandbox: self.popup_sandbox,
            secure_context: None,
        }
    }

    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName
    fn tab_state_at(&self, url: &Url) -> renderer::TabState {
        let mut state = self.current_tab_state();
        if !self.keeps_target_name_at(url) {
            state.name = None;
        }
        state
    }

    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName
    // https://html.spec.whatwg.org/multipage/browsers.html#sandboxed-origin-browsing-context-flag
    fn keeps_target_name_at(&self, url: &Url) -> bool {
        if self.auxiliary_with_opener() || url.scheme() == "about" {
            return true;
        }
        // https://w3c.github.io/webappsec-csp/#sandbox
        let active = self
            .tabs
            .frame_sandbox(self.id, renderer::FrameId::MAIN)
            .unwrap_or(self.popup_sandbox);
        !active.origin_is_opaque() && url.origin() == self.document_origin
    }

    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName
    fn advance_document_url(&mut self, url: Url) {
        if url.scheme() != "about" {
            self.document_origin = url.origin();
            self.tabs
                .set_main_origin(self.id, self.document_origin.clone());
        }
        self.tabs.set_main_url(self.id, url.to_string());
        self.document_url = url;
    }

    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#finalize-a-cross-document-navigation
    async fn commit_main_navigation(
        &mut self,
        url: Url,
        outgoing_name: Option<&renderer::ContextName>,
    ) {
        if self
            .site
            .as_ref()
            .is_some_and(|site| !site.allows_url(&url))
        {
            return;
        }
        // https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName
        self.history.sync_current_name(outgoing_name);
        let name = self.tabs.name(self.id);
        self.history.commit(Commit::Push {
            url: url.clone(),
            name: name.clone(),
        });
        self.tabs.settle_document_name(self.id, outgoing_name, name);
        self.advance_document_url(url);
        self.document_loaded = false;
        self.navigation_failed = false;
        self.sync_histories().await;
        self.record_event(TabEvent::Navigated).await;
    }

    // https://html.spec.whatwg.org/multipage/document-sequences.html#navigable
    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName
    fn commit_state(&self, url: &Url) -> renderer::TabState {
        match self.traversal {
            Some(entry) => renderer::TabState {
                name: self.history.name(entry),
                ..self.current_tab_state()
            },
            None => self.tab_state_at(url),
        }
    }

    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName
    fn begin_commit(&mut self, state: &renderer::TabState) -> CommitNames {
        let installed = TabNames::from_name(state.name.as_ref());
        let previous = self.tabs.install_commit(self.id, state.name.as_ref());
        CommitNames {
            previous,
            installed,
        }
    }

    fn abort_commit(&mut self, names: CommitNames) {
        self.tabs
            .restore_commit(self.id, names.previous, &names.installed);
    }

    async fn launch_navigation(&mut self) {
        let Some(nav) = self.nav.as_ref() else {
            return;
        };
        if nav.submitted {
            return;
        }
        let epoch = nav.epoch;
        let url = nav.url.clone();
        let initiator = nav.initiator.clone();
        self.cancel_dial();
        let (cancel, cancel_rx) = watch::channel(false);
        let guard = self.network.dial_navigation(
            epoch,
            url.clone(),
            initiator,
            self.dial_tx.clone(),
            cancel_rx,
        );
        self.dial_cancel = Some(cancel);
        self.dial_guard = Some(guard);
        if let Some(nav) = self.nav.as_mut() {
            nav.submitted = true;
        }
        let _ = self
            .events
            .notify(TabEvent::NavigationRequested {
                url: url.to_string(),
            })
            .await;
    }

    fn cancel_dial(&mut self) {
        if let Some(cancel) = self.dial_cancel.take() {
            let _ = cancel.send(true);
        }
        if let Some(guard) = self.dial_guard.take() {
            guard.abort();
        }
    }

    async fn handle_navigation(&mut self, epoch: u64, result: Result<NavOutcome, DialFailure>) {
        let active = self.nav.as_ref().is_some_and(|nav| nav.epoch == epoch);
        if !active {
            return;
        }
        let url = self.nav.as_ref().map(|nav| nav.url.clone());
        self.dial_cancel = None;
        self.dial_guard = None;
        self.nav = None;
        self.document_loaded = false;
        match result {
            Ok(outcome) => self.handle_navigation_ok(outcome).await,
            Err(failure) => self.handle_navigation_err(url.as_ref(), &failure).await,
        }
    }

    async fn handle_navigation_ok(&mut self, outcome: NavOutcome) {
        let status = outcome.status;
        // https://html.spec.whatwg.org/multipage/browsing-the-web.html#process-a-navigate-response
        if status == 204 || status == 205 {
            if let Some(request_id) = outcome.request_id
                && matches!(outcome.body, NavBody::Stream(_))
            {
                self.network.broker().response_completed(
                    request_id,
                    Some(self.id),
                    FrameId::MAIN,
                    true,
                    &outcome.final_url,
                    status,
                    &outcome.status_text,
                    outcome.headers.clone(),
                    "",
                    false,
                    0,
                );
            }
            self.traversal = None;
            self.navigation_failed = true;
            self.record_event(TabEvent::NavigationFailed(NavigationFailure::Aborted))
                .await;
            return;
        }
        let status_text = outcome.status_text.clone();
        let final_url = outcome.final_url.clone();
        let url = final_url.to_string();
        let headers = outcome.headers.clone();
        let request_id = outcome.request_id;
        let streamed = matches!(outcome.body, NavBody::Stream(_));
        if let Some(body) = self.commit_navigation(outcome).await {
            if let Some(id) = request_id
                && streamed
            {
                let mime =
                    net::combined_header_in(&headers, "content-type").map_or_default(|raw| {
                        raw.split(';')
                            .next()
                            .map_or("", str::trim)
                            .to_ascii_lowercase()
                    });
                self.network.broker().record_body(id, &body);
                self.network.broker().response_completed(
                    id,
                    Some(self.id),
                    FrameId::MAIN,
                    true,
                    &final_url,
                    status,
                    &status_text,
                    headers.clone(),
                    &mime,
                    false,
                    body.len(),
                );
            }
            // https://chromedevtools.github.io/devtools-protocol/tot/Network/#event-responseReceived
            self.record_event(TabEvent::NavigationResponded {
                status,
                status_text,
                url,
                headers,
                body,
            })
            .await;
            self.record_event(TabEvent::Navigated).await;
        } else {
            if let Some(id) = request_id
                && streamed
            {
                self.network.broker().fetch_error(
                    id,
                    Some(self.id),
                    FrameId::MAIN,
                    true,
                    &final_url,
                    "net::ERR_FAILED",
                );
            }
            self.record_event(TabEvent::NavigationFailed(NavigationFailure::Failed))
                .await;
        }
    }

    async fn handle_navigation_err(&mut self, url: Option<&Url>, failure: &DialFailure) {
        logging::error!(
            target: "browser::tab",
            "navigation dial failed for {}: {failure:?}",
            url.map(log_url).as_deref().unwrap_or("unknown")
        );
        let detail = format!("{failure:?}").to_ascii_lowercase();
        let reason = if detail.contains("refused") {
            NavigationFailure::ConnectionRefused
        } else if detail.contains("tls")
            || detail.contains("cert")
            || detail.contains("ssl")
            || detail.contains("handshake")
        {
            NavigationFailure::CertAuthorityInvalid
        } else {
            NavigationFailure::Failed
        };
        self.traversal = None;
        self.navigation_failed = true;
        self.record_event(TabEvent::NavigationFailed(reason)).await;
    }

    async fn commit_navigation(&mut self, outcome: NavOutcome) -> Option<Vec<u8>> {
        let final_url = outcome.head.url.clone();
        let site = Site::for_url(&final_url).unwrap_or_else(|| Site::opaque(self.id));
        // https://w3c.github.io/webappsec-csp/#sandbox
        let csp = outcome.head.source.policy().sandbox;
        let previous_csp = self.tabs.main_csp(self.id);
        self.tabs.reset_main_sandbox(self.id, csp);
        self.sync_outgoing_name();
        let state = self.commit_state(&final_url);
        let commit = self.history_commit(&final_url, &state);
        let mut history = self.history.clone();
        if !history.commit(commit) {
            self.tabs.reset_main_sandbox(self.id, previous_csp);
            logging::error!(
                target: "browser::tab",
                "navigation rejected by history for {}",
                log_url(&final_url)
            );
            self.navigation_failed = true;
            return None;
        }
        let head = ResponseHead {
            history: history.renderer_snapshot(),
            viewport: Some(self.viewport),
            init_scripts: self.init_scripts.clone(),
            bindings: self.bindings.clone(),
            ..outcome.head
        };
        let previous_names = self.begin_commit(&state);
        let body = match self
            .mount_stream(
                &site,
                Mount {
                    head,
                    tab: state,
                    body: Vec::new(),
                },
                outcome.body,
            )
            .await
        {
            Ok(body) => body,
            Err(error) => {
                self.abort_commit(previous_names);
                self.tabs.reset_main_sandbox(self.id, previous_csp);
                logging::error!(
                    target: "browser::tab",
                    "navigation mount failed for {}: {error}",
                    log_url(&final_url)
                );
                self.navigation_failed = true;
                return None;
            }
        };
        self.history = history;
        self.advance_document_url(final_url);
        let title = self.document_title().await;
        self.history.set_current_title(title);
        Some(body)
    }

    async fn handle_renderer_event(&mut self, frame: FrameId, event: &BrowserEvent) {
        match event {
            BrowserEvent::Navigated { url, outgoing_name } => {
                self.handle_renderer_navigated(frame, url, outgoing_name.as_ref())
                    .await;
            }
            BrowserEvent::Load => self.handle_renderer_load(frame).await,
            BrowserEvent::DomContentLoaded => {
                if frame == FrameId::MAIN {
                    self.record_event(TabEvent::DomContentLoaded).await;
                } else {
                    let url = self
                        .tabs
                        .frame_url(self.id, frame)
                        .unwrap_or_else(|| "about:blank".to_owned());
                    self.record_event(TabEvent::FrameDomContentLoaded { frame, url })
                        .await;
                }
            }
            BrowserEvent::NavigationStarted {
                url,
                replace,
                local_index,
                pending,
            } => {
                if let Ok(url) = Url::parse(url) {
                    self.history
                        .begin(frame, url, *replace, *local_index, None, *pending);
                    if !*pending {
                        self.sync_histories().await;
                    }
                }
            }
            BrowserEvent::FrameAdded { url } => self.handle_frame_added(frame, url).await,
            BrowserEvent::FrameRemoved => {
                self.history.remove_frame(frame);
                self.sync_histories().await;
                self.record_event(TabEvent::FrameDetached { frame }).await;
            }
            BrowserEvent::HistoryUpdated {
                url,
                state,
                handling,
            } => {
                self.apply_history_update(frame, url, state, *handling)
                    .await;
            }
            BrowserEvent::HistoryTraversal { delta } => {
                let _result = self.traverse(HistoryStep::Delta(*delta), frame).await;
            }
            // https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#document-open-steps
            BrowserEvent::RealmReplaced => {
                self.record_event(TabEvent::RealmReplaced).await;
            }
            // https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#event-bindingCalled
            BrowserEvent::Binding { name, payload } => {
                self.record_event(TabEvent::Binding {
                    name: name.clone(),
                    payload: payload.clone(),
                })
                .await;
            }
            // https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#event-consoleAPICalled
            BrowserEvent::Console { level, args } => {
                self.record_event(TabEvent::Console {
                    level: *level,
                    args: args.clone(),
                })
                .await;
            }
            // https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#event-exceptionThrown
            BrowserEvent::ExceptionThrown {
                text,
                filename,
                line,
                column,
            } => {
                self.record_event(TabEvent::ExceptionThrown {
                    frame,
                    text: text.clone(),
                    filename: filename.clone(),
                    line: *line,
                    column: *column,
                })
                .await;
            }
        }
    }

    async fn handle_renderer_navigated(
        &mut self,
        frame: FrameId,
        url: &str,
        outgoing_name: Option<&renderer::ContextName>,
    ) {
        let Ok(url) = Url::parse(url) else {
            return;
        };
        if frame != FrameId::MAIN {
            // https://html.spec.whatwg.org/multipage/browsing-the-web.html#finalize-a-cross-document-navigation
            let url_str = url.to_string();
            self.tabs
                .set_child_frame_url(self.id, frame, url_str.clone());
            self.history.commit_url(frame, url);
            self.sync_histories().await;
            let parent = self
                .tabs
                .frame_parent(self.id, frame)
                .unwrap_or(FrameId::MAIN);
            self.record_event(TabEvent::FrameNavigated {
                frame,
                parent,
                url: url_str,
            })
            .await;
            return;
        }
        self.commit_main_navigation(url, outgoing_name).await;
    }

    async fn handle_renderer_load(&mut self, frame: FrameId) {
        if frame != FrameId::MAIN {
            let url = self
                .tabs
                .frame_url(self.id, frame)
                .unwrap_or_else(|| "about:blank".to_owned());
            self.record_event(TabEvent::FrameLoad { frame, url }).await;
            return;
        }
        let expected = self
            .nav
            .as_ref()
            .map_or_else(|| self.document_url.clone(), |nav| nav.url.clone());
        let Some(state) = self.renderer_document_state().await else {
            return;
        };
        if state.ready_state != renderer::ReadyState::Complete || state.url != expected.as_str() {
            return;
        }
        self.document_loaded = true;
        self.record_event(TabEvent::Load).await;
    }

    async fn handle_frame_added(&mut self, frame: FrameId, url: &str) {
        let parsed = Url::parse(url).unwrap_or_else(|_| net::about_blank());
        let url_str = parsed.to_string();
        self.tabs
            .set_child_frame_url(self.id, frame, url_str.clone());
        self.history.add_frame(frame, parsed, None);
        self.sync_histories().await;
        let parent = self
            .tabs
            .frame_parent(self.id, frame)
            .unwrap_or(FrameId::MAIN);
        self.record_event(TabEvent::FrameAttached {
            frame,
            parent,
            url: url_str,
        })
        .await;
    }

    async fn apply_history_update(
        &mut self,
        frame: FrameId,
        url: &str,
        state: &str,
        handling: renderer::HistoryHandling,
    ) {
        let Ok(url) = Url::parse(url) else {
            return;
        };
        let expected = if frame == FrameId::MAIN {
            Some(self.document_url.origin())
        } else {
            self.history
                .active_entry(frame)
                .and_then(|entry| self.history.entry_url(entry))
                .map(Url::origin)
        };
        if expected.is_none_or(|origin| url.origin() != origin) {
            return;
        }
        self.history
            .update(frame, url.clone(), state.to_owned(), handling);
        if frame == FrameId::MAIN {
            self.tabs.set_main_url(self.id, url.to_string());
            self.document_url = url;
            self.record_event(TabEvent::SameDocumentNavigation).await;
        } else {
            self.tabs
                .set_child_frame_url(self.id, frame, url.to_string());
        }
        self.sync_histories().await;
    }

    async fn sync_histories(&mut self) {
        if self.renderer.is_none() {
            return;
        }
        let snapshots: Vec<(FrameId, renderer::HistorySnapshot)> = self
            .history
            .frames()
            .into_iter()
            .filter_map(|frame| {
                self.history
                    .renderer_snapshot_for(frame)
                    .map(|snapshot| (frame, snapshot))
            })
            .collect();
        for (frame, history) in snapshots {
            let _result = self
                .renderer_request(RendererCommand::SetHistory { frame, history })
                .await;
        }
    }

    fn history_commit(&mut self, url: &Url, state: &renderer::TabState) -> Commit {
        match self.traversal.take() {
            Some(entry) => Commit::Traverse {
                entry,
                url: url.clone(),
            },
            None => Commit::Push {
                url: url.clone(),
                name: state.name.clone(),
            },
        }
    }

    // https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-name
    fn sync_outgoing_name(&mut self) {
        let name = self.tabs.document_name(self.id);
        self.history.sync_current_name(name.as_ref());
    }

    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#traverse-the-history-by-a-delta
    async fn traverse(&mut self, step: HistoryStep, caller: FrameId) -> Result<(), TabError> {
        if caller == FrameId::MAIN {
            self.sync_outgoing_name();
        }
        // https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-history-go
        if step == HistoryStep::Delta(0) {
            return self.reload_frame(caller).await;
        }
        let Some(target_step) = self.history.target_step(step, caller) else {
            return match step {
                HistoryStep::Entry(entry) => {
                    Err(TabError::UnknownHistoryEntry { entry: entry.get() })
                }
                HistoryStep::Delta(_) => Ok(()),
            };
        };
        let previous_step = self.history.current_step();
        let mut main_move = None;
        let mut child_moves = Vec::new();
        for frame in self.history.frames() {
            let Some(target) = self.history.active_entry_at(frame, target_step) else {
                continue;
            };
            let current = self.history.active_entry_at(frame, previous_step);
            if current == Some(target) {
                continue;
            }
            let same_document =
                current.is_some_and(|current| self.history.same_document(current, target));
            if frame == FrameId::MAIN {
                main_move = Some((target, same_document));
            } else {
                child_moves.push((frame, target, same_document));
            }
        }
        let mut main_reloads = false;
        if let Some((entry, same_document)) = main_move {
            let url = self
                .history
                .entry_url(entry)
                .cloned()
                .ok_or(TabError::UnknownHistoryEntry { entry: entry.get() })?;
            if same_document {
                let Some(history) = self.history.renderer_snapshot_for_entry(entry) else {
                    return Ok(());
                };
                let result = self
                    .renderer_request(RendererCommand::HistoryTraverse {
                        url: url.to_string(),
                        history,
                    })
                    .await;
                if matches!(result, Ok(Reply::Unit(Ok(()))))
                    && self.history.traverse_same_document(entry)
                {
                    self.document_url = url;
                    self.record_event(TabEvent::SameDocumentNavigation).await;
                }
            } else {
                main_reloads = true;
                if self.goto(url.as_str(), None).is_ok() {
                    self.traversal = Some(entry);
                }
            }
        }
        for (frame, entry, same_document) in child_moves {
            let Some(url) = self.history.entry_url(entry).cloned() else {
                continue;
            };
            let Some(index) = self.history.local_index(entry) else {
                continue;
            };
            let Some(history) = self.history.renderer_snapshot_for_entry(entry) else {
                continue;
            };
            self.history.discard_pending(frame);
            let _result = self
                .renderer_request(RendererCommand::HistoryTraverseFrame {
                    frame,
                    index,
                    url: url.to_string(),
                    history,
                    same_document,
                    reload: false,
                })
                .await;
        }
        if main_reloads {
            return Ok(());
        }
        self.history.set_current_step(target_step);
        // https://html.spec.whatwg.org/multipage/browsing-the-web.html#getting-the-history-object-length-and-index
        self.sync_histories().await;
        Ok(())
    }

    async fn reload_frame(&mut self, frame: FrameId) -> Result<(), TabError> {
        if frame == FrameId::MAIN {
            let url = self.document_url.to_string();
            if self.goto(&url, None).is_ok() {
                self.traversal = Some(self.history.current_entry());
            }
            return Ok(());
        }
        let Some(entry) = self.history.active_entry(frame) else {
            return Ok(());
        };
        let Some(url) = self.history.entry_url(entry).cloned() else {
            return Ok(());
        };
        let Some(index) = self.history.local_index(entry) else {
            return Ok(());
        };
        let Some(history) = self.history.renderer_snapshot_for_entry(entry) else {
            return Ok(());
        };
        self.history.discard_pending(frame);
        let _result = self
            .renderer_request(RendererCommand::HistoryTraverseFrame {
                frame,
                index,
                url: url.to_string(),
                history,
                same_document: false,
                reload: true,
            })
            .await;
        Ok(())
    }

    async fn record_event(&self, event: TabEvent) {
        let _result = self.events.notify(event).await;
    }

    fn waiting_for_load(&self) -> bool {
        self.nav.is_some() || (!self.document_loaded && !self.navigation_failed)
    }

    fn stop_renderer(&mut self) {
        self.cancel_dial();
        self.drop_renderer();
    }
}

fn blank_mount(tab: renderer::TabState) -> Mount {
    Mount {
        head: ResponseHead::local(
            net::about_blank(),
            Some("text/html; charset=utf-8".to_owned()),
            None,
        ),
        tab,
        body: b"<!doctype html><title></title>".to_vec(),
    }
}

impl Drop for Tab {
    fn drop(&mut self) {
        self.cancel_dial();
    }
}

enum Wake {
    Command(Option<ServerInput<Command, Infallible>>),
    Navigation(Box<Option<(u64, Result<NavOutcome, DialFailure>)>>),
    Renderer(Option<RendererWake>),
    Delivery(Option<RemoteWindowMessage>),
    WaiterDeadline,
}

async fn coordinator_loop(
    mut server: TabServer,
    mut tab: Tab,
    delivery_rx: mpsc::Receiver<RemoteWindowMessage>,
) {
    let mut waiters = Vec::new();
    let mut delivery_rx = Some(delivery_rx);
    loop {
        if let Some(url) = tab.take_data_navigation() {
            let mounted = tab.commit_data_navigation(url).await;
            tab.record_event(if mounted {
                TabEvent::Navigated
            } else {
                TabEvent::NavigationFailed(NavigationFailure::Failed)
            })
            .await;
            continue;
        }
        tab.launch_navigation().await;
        let deadline = next_waiter_deadline(&waiters);
        let wake = {
            let dial_rx = &mut tab.dial_rx;
            let events_rx = &mut tab.events_rx;
            tokio::select! {
                command = server.recv() => Wake::Command(command),
                navigation = dial_rx.recv() => Wake::Navigation(Box::new(navigation)),
                event = next_renderer_event(events_rx) => Wake::Renderer(event),
                delivery = next_delivery(&mut delivery_rx) => Wake::Delivery(delivery),
                () = wait_for_deadline(deadline) => Wake::WaiterDeadline,
            }
        };
        match wake {
            Wake::Command(Some(ServerInput::Call { id, body })) => {
                if handle_command(&mut tab, &server, id, body, &mut waiters).await {
                    return;
                }
            }
            Wake::Command(Some(ServerInput::Cancel { id })) => {
                let mut canceled = Vec::new();
                waiters.retain(|waiter| {
                    let keep = waiter.id != id && !waiter.reply.is_closed();
                    if !keep {
                        canceled.push(waiter.renderer_waiter());
                    }
                    keep
                });
                for renderer_id in canceled.into_iter().flatten() {
                    let _result = tab
                        .renderer_request(RendererCommand::CancelWait { id: renderer_id })
                        .await;
                }
            }
            Wake::Command(Some(ServerInput::RequestChunk | ServerInput::RequestEnd))
            | Wake::WaiterDeadline => {}
            Wake::Command(Some(ServerInput::Notify(_)) | None) => break,
            Wake::Navigation(boxed) => {
                if let Some((epoch, result)) = *boxed {
                    tab.handle_navigation(epoch, result).await;
                }
            }
            Wake::Renderer(Some(RendererWake::WaiterSettled(id))) => {
                settle_waiter(&server, &mut waiters, id).await;
            }
            Wake::Renderer(Some(RendererWake::Event(frame, event))) => {
                let navigated = matches!(event, BrowserEvent::Navigated { .. });
                tab.handle_renderer_event(frame, &event).await;
                if navigated {
                    resolve_frame_waiters(&mut tab, &server, &mut waiters, frame).await;
                }
            }
            Wake::Renderer(None) => {
                let loading = tab.waiting_for_load();
                tab.drop_renderer();
                if loading {
                    tab.cancel_dial();
                    tab.nav = None;
                    tab.traversal = None;
                    tab.navigation_failed = true;
                    tab.record_event(TabEvent::NavigationFailed(NavigationFailure::Failed))
                        .await;
                }
                fail_waiters(&mut tab, &server, &mut waiters, &TabError::ActorStopped).await;
            }
            Wake::Delivery(Some(message)) => {
                let _result = tab.deliver_window_message(message).await;
            }
            Wake::Delivery(None) => {
                delivery_rx = None;
            }
        }
        resolve_waiters(&mut tab, &server, &mut waiters).await;
    }
    tab.stop_renderer();
}

async fn next_renderer_event(
    events: &mut Option<mpsc::Receiver<RendererWake>>,
) -> Option<RendererWake> {
    match events {
        Some(receiver) => receiver.recv().await,
        None => pending().await,
    }
}

async fn next_delivery(
    deliveries: &mut Option<mpsc::Receiver<RemoteWindowMessage>>,
) -> Option<RemoteWindowMessage> {
    match deliveries {
        Some(receiver) => receiver.recv().await,
        None => pending().await,
    }
}

async fn wait_for_deadline(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => sleep_until(deadline).await,
        None => pending().await,
    }
}

fn next_waiter_deadline(waiters: &[Waiter]) -> Option<Instant> {
    waiters.iter().map(|waiter| waiter.deadline).min()
}

async fn handle_command(
    tab: &mut Tab,
    server: &TabServer,
    id: RequestId,
    command: Command,
    waiters: &mut Vec<Waiter>,
) -> bool {
    let outcome = if command.abandoned() {
        TabOutcome::Continue
    } else {
        command.serve(ServeContext { tab, waiters, id }).await
    };
    if !matches!(outcome, TabOutcome::Retained) {
        let _ack = server.reply(id, ()).await;
    }
    matches!(outcome, TabOutcome::Shutdown)
}

impl Command {
    fn abandoned(&self) -> bool {
        match self {
            Command::Shutdown { .. } => false,
            Command::LoadHtml { reply, .. }
            | Command::Goto { reply, .. }
            | Command::Reload { reply }
            | Command::StopLoading { reply }
            | Command::TraverseHistory { reply, .. }
            | Command::RemoveInitScript { reply, .. }
            | Command::SetViewport { reply, .. }
            | Command::SetIdentity { reply, .. }
            | Command::SetExtraHeaders { reply, .. }
            | Command::AddBinding { reply, .. } => reply.is_closed(),
            Command::Execute { reply, .. } => reply.is_closed(),
            Command::AddInitScript { reply, .. } => reply.is_closed(),
            Command::ViewportSize { reply } => reply.is_closed(),
            Command::ContentSize { reply, .. } => reply.is_closed(),
            Command::Dom { reply, .. } => reply.is_closed(),
            Command::Screenshot { reply, .. } => reply.is_closed(),
            Command::RunUntilLoadTimeout { reply, .. } | Command::RunUntilJsTrue { reply, .. } => {
                reply.is_closed()
            }
            Command::DocumentUrl { reply, .. } | Command::DocumentTitle { reply, .. } => {
                reply.is_closed()
            }
            Command::Frames { reply } => reply.is_closed(),
            Command::Opener { reply } => reply.is_closed(),
            Command::WindowName { reply } => reply.is_closed(),
            Command::History { reply } => reply.is_closed(),
            Command::LastNavigationFailed { reply } => reply.is_closed(),
        }
    }

    async fn serve(self, ctx: ServeContext<'_>) -> TabOutcome {
        let ServeContext { tab, waiters, id } = ctx;
        match self {
            Command::LoadHtml { html, reply } => answer(reply, tab.load_html(&html).await),
            Command::Goto { request, reply } => {
                let _result = reply.send(tab.goto(&request.url, request.initiator));
            }
            Command::Execute {
                frame,
                source,
                timeout,
                reply,
            } => answer(reply, execute_script(tab, frame, source, timeout).await),
            Command::AddInitScript {
                source,
                run_immediately,
                reply,
            } => answer(reply, add_init_script(tab, source, run_immediately).await),
            // https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#method-addBinding
            Command::AddBinding {
                name,
                run_immediately,
                reply,
            } => answer(reply, add_binding(tab, name, run_immediately).await),
            Command::RemoveInitScript { id: script, reply } => {
                answer(reply, remove_init_script(tab, script).await);
            }
            Command::SetViewport {
                frame,
                width,
                height,
                reply,
            } => answer(reply, set_viewport(tab, frame, width, height).await),
            Command::SetIdentity { identity, reply } => {
                answer(reply, set_identity(tab, identity.map(|value| *value)).await);
            }
            Command::SetExtraHeaders { headers, reply } => {
                answer(reply, set_extra_headers(tab, &headers));
            }
            Command::ViewportSize { reply } => answer(reply, Ok(tab.viewport)),
            Command::ContentSize { frame, reply } => answer(
                reply,
                tab.renderer_request(RendererCommand::ContentSize { frame })
                    .await
                    .and_then(reply_content_size),
            ),
            Command::Dom {
                frame,
                request,
                reply,
            } => serve_dom(tab, frame, request, reply).await,
            Command::Screenshot {
                frame,
                request,
                reply,
            } => answer(reply, screenshot_frame(tab, frame, request).await),
            Command::RunUntilLoadTimeout { timeout, reply } => {
                if retain_load_waiter(waiters, id, timeout, reply) {
                    return TabOutcome::Retained;
                }
            }
            Command::RunUntilJsTrue {
                frame,
                source,
                timeout,
                reply,
            } => {
                if serve_js_waiter(tab, waiters, id, frame, source, timeout, reply).await {
                    return TabOutcome::Retained;
                }
            }
            Command::DocumentUrl { .. }
            | Command::DocumentTitle { .. }
            | Command::Frames { .. }
            | Command::Reload { .. }
            | Command::StopLoading { .. }
            | Command::Opener { .. }
            | Command::WindowName { .. }
            | Command::History { .. }
            | Command::TraverseHistory { .. }
            | Command::LastNavigationFailed { .. } => self.serve_inspection(tab).await,
            Command::Shutdown { reply } => {
                tab.stop_renderer();
                let _result = reply.send(Ok(()));
                return TabOutcome::Shutdown;
            }
        }
        TabOutcome::Continue
    }

    async fn serve_inspection(self, tab: &mut Tab) {
        match self {
            Command::DocumentUrl { frame, reply } => {
                let result = if frame == FrameId::MAIN {
                    Ok(tab.document_url.to_string())
                } else {
                    tab.tabs
                        .frame_url(tab.id, frame)
                        .ok_or(TabError::UnknownFrame { frame: frame.get() })
                };
                let _result = reply.send(result);
            }
            Command::DocumentTitle { frame, reply } => {
                let title = tab.document_title_in(frame).await;
                let _result = reply.send(Ok(title));
            }
            Command::Frames { reply } => {
                let _result = reply.send(Ok(tab.tabs.frame_snapshot(tab.id)));
            }
            Command::Reload { reply } => {
                let url = tab.document_url.to_string();
                let _result = reply.send(tab.goto(&url, None));
            }
            Command::StopLoading { reply } => {
                tab.cancel_dial();
                tab.nav = None;
                let _result = reply.send(Ok(()));
            }
            Command::Opener { reply } => {
                let _result = reply.send(Ok(tab.tabs.opener(tab.id)));
            }
            Command::WindowName { reply } => {
                let name = tab.tabs.name(tab.id).map(|name| name.as_str().to_owned());
                let _result = reply.send(Ok(name));
            }
            Command::History { reply } => {
                let _result = reply.send(Ok(tab.history.snapshot()));
            }
            Command::TraverseHistory { step, reply } => {
                let result = tab.traverse(step, FrameId::MAIN).await;
                let _result = reply.send(result);
            }
            Command::LastNavigationFailed { reply } => {
                let _result = reply.send(Ok(tab.navigation_failed));
            }
            _ => {}
        }
    }
}

fn set_extra_headers(tab: &mut Tab, headers: &[(String, String)]) -> Result<(), TabError> {
    tab.network
        .set_extra_headers(tab.id, headers)
        .map_err(|error| TabError::Dom {
            message: error.to_string(),
        })
}

async fn execute_script(
    tab: &mut Tab,
    frame: FrameId,
    source: renderer::ScriptSource,
    timeout: Option<Duration>,
) -> Result<RemoteValue, TabError> {
    tab.renderer_request(RendererCommand::ExecuteScript {
        frame,
        source,
        timeout_ms: timeout.map(millis),
    })
    .await
    .and_then(reply_value)
}

async fn add_init_script(
    tab: &mut Tab,
    source: String,
    run_immediately: bool,
) -> Result<u64, TabError> {
    let id = tab.next_init_script;
    tab.next_init_script = tab.next_init_script.wrapping_add(1);
    tab.init_scripts.push((id, source.clone()));
    if tab.renderer.is_some() {
        tab.renderer_request(RendererCommand::AddInitScript {
            id,
            source,
            run_immediately,
        })
        .await
        .and_then(reply_unit)
        .map(|()| id)
    } else {
        Ok(id)
    }
}

async fn remove_init_script(tab: &mut Tab, id: u64) -> Result<(), TabError> {
    tab.init_scripts.retain(|(script_id, _)| *script_id != id);
    if tab.renderer.is_some() {
        tab.renderer_request(RendererCommand::RemoveInitScript { id })
            .await
            .and_then(reply_unit)
    } else {
        Ok(())
    }
}

// https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#method-addBinding
async fn add_binding(tab: &mut Tab, name: String, run_immediately: bool) -> Result<(), TabError> {
    if !tab.bindings.contains(&name) {
        tab.bindings.push(name.clone());
    }
    if run_immediately && tab.renderer.is_none() {
        tab.mount_virtual().await?;
    }
    if tab.renderer.is_some() {
        tab.renderer_request(RendererCommand::AddBinding {
            name,
            run_immediately,
        })
        .await
        .and_then(reply_unit)
    } else {
        Ok(())
    }
}

async fn set_viewport(
    tab: &mut Tab,
    frame: FrameId,
    width: u32,
    height: u32,
) -> Result<(), TabError> {
    let result = tab
        .renderer_request(RendererCommand::SetViewport {
            frame,
            width,
            height,
        })
        .await
        .and_then(reply_unit);
    if result.is_ok() {
        tab.viewport = (width, height);
    }
    result
}

async fn set_identity(tab: &mut Tab, identity: Option<net::ValidIdentity>) -> Result<(), TabError> {
    tab.network.set_identity(tab.id, identity);
    let effective = tab.network.identity(tab.id);
    if tab.renderer.is_some() {
        tab.renderer_request(RendererCommand::SetIdentity {
            identity: Box::new(effective),
        })
        .await
        .and_then(reply_unit)?;
    }
    Ok(())
}

async fn screenshot_frame(
    tab: &mut Tab,
    frame: FrameId,
    request: renderer::ScreenshotRequest,
) -> Result<Vec<u8>, TabError> {
    tab.renderer_request_bytes(RendererCommand::Screenshot { frame, request })
        .await
}

async fn serve_dom(
    tab: &mut Tab,
    frame: FrameId,
    request: renderer::DomRequest,
    reply: ReplySender<renderer::DomReply>,
) {
    let result = tab
        .renderer_request(RendererCommand::Dom { frame, request })
        .await
        .and_then(reply_dom);
    let _result = reply.send(result);
}

async fn serve_js_waiter(
    tab: &mut Tab,
    waiters: &mut Vec<Waiter>,
    id: RequestId,
    frame: FrameId,
    source: renderer::ScriptSource,
    timeout: Duration,
    reply: ReplySender<bool>,
) -> bool {
    let renderer_id = id.get();
    let registered = tab
        .renderer_request(RendererCommand::WaitForJs {
            id: renderer_id,
            frame,
            source,
        })
        .await;
    match registered {
        Ok(_) => {
            if retain_js_waiter(waiters, id, frame, renderer_id, timeout, reply) {
                return true;
            }
            let _result = tab
                .renderer_request(RendererCommand::CancelWait { id: renderer_id })
                .await;
            false
        }
        Err(error) => {
            let _result = reply.send(Err(error));
            false
        }
    }
}

fn retain_load_waiter(
    waiters: &mut Vec<Waiter>,
    id: RequestId,
    timeout: Duration,
    reply: ReplySender<bool>,
) -> bool {
    retain_waiter(
        waiters,
        Waiter {
            id,
            deadline: Instant::now() + timeout,
            kind: WaiterKind::Load,
            reply,
        },
    )
}

fn retain_js_waiter(
    waiters: &mut Vec<Waiter>,
    id: RequestId,
    frame: FrameId,
    renderer_id: u64,
    timeout: Duration,
    reply: ReplySender<bool>,
) -> bool {
    retain_waiter(
        waiters,
        Waiter {
            id,
            deadline: Instant::now() + timeout,
            kind: WaiterKind::Js { renderer_id, frame },
            reply,
        },
    )
}

fn retain_waiter(waiters: &mut Vec<Waiter>, waiter: Waiter) -> bool {
    if waiter.reply.is_closed() {
        return false;
    }
    if waiters.len() < MAX_WAITERS {
        waiters.push(waiter);
        return true;
    }
    let error = TabError::ResourceLimit {
        resource: ResourceLimit::TabWaiters,
    };
    let _result = waiter.reply.send(Err(error));
    false
}

async fn fail_waiters(
    tab: &mut Tab,
    server: &TabServer,
    waiters: &mut Vec<Waiter>,
    error: &TabError,
) {
    for waiter in waiters.drain(..) {
        if tab.renderer.is_some()
            && let Some(id) = waiter.renderer_waiter()
        {
            let _result = tab
                .renderer_request(RendererCommand::CancelWait { id })
                .await;
        }
        let _result = waiter.reply.send(Err(error.clone()));
        let _ack = server.reply(waiter.id, ()).await;
    }
}

async fn resolve_frame_waiters(
    tab: &mut Tab,
    server: &TabServer,
    waiters: &mut Vec<Waiter>,
    frame: FrameId,
) {
    let mut pending = Vec::new();
    for waiter in std::mem::take(waiters) {
        let navigated = matches!(
            waiter.kind,
            WaiterKind::Js {
                frame: wait_frame,
                ..
            } if wait_frame == frame
        );
        if navigated {
            cancel_renderer_waiter(tab, &waiter).await;
            let _result = waiter.reply.send(Ok(false));
            let _ack = server.reply(waiter.id, ()).await;
        } else {
            pending.push(waiter);
        }
    }
    *waiters = pending;
}

async fn resolve_waiters(tab: &mut Tab, server: &TabServer, waiters: &mut Vec<Waiter>) {
    let now = Instant::now();
    let mut pending = Vec::new();
    for waiter in std::mem::take(waiters) {
        if waiter.reply.is_closed() {
            cancel_renderer_waiter(tab, &waiter).await;
            continue;
        }
        let result = match waiter.kind {
            WaiterKind::Load if !tab.waiting_for_load() => Some(Ok(true)),
            WaiterKind::Load | WaiterKind::Js { .. } if now >= waiter.deadline => Some(Ok(false)),
            WaiterKind::Load | WaiterKind::Js { .. } => None,
        };
        if let Some(result) = result {
            if result == Ok(false) {
                cancel_renderer_waiter(tab, &waiter).await;
            }
            let _result = waiter.reply.send(result);
            let _ack = server.reply(waiter.id, ()).await;
        } else {
            pending.push(waiter);
        }
    }
    *waiters = pending;
}

async fn settle_waiter(server: &TabServer, waiters: &mut Vec<Waiter>, renderer_id: u64) {
    let Some(index) = waiters
        .iter()
        .position(|waiter| waiter.renderer_waiter() == Some(renderer_id))
    else {
        return;
    };
    let waiter = waiters.remove(index);
    let _result = waiter.reply.send(Ok(true));
    let _ack = server.reply(waiter.id, ()).await;
}

async fn cancel_renderer_waiter(tab: &mut Tab, waiter: &Waiter) {
    if let Some(id) = waiter.renderer_waiter() {
        let _result = tab
            .renderer_request(RendererCommand::CancelWait { id })
            .await;
    }
}

fn renderer_unavailable(message: &str) -> TabError {
    TabError::RendererUnavailable {
        message: message.to_owned(),
    }
}

fn answer<T>(reply: ReplySender<T>, result: Result<T, TabError>) {
    let _result = reply.send(result);
}

fn reply_unit(reply: Reply) -> Result<(), TabError> {
    match reply {
        Reply::Unit(result) => result,
        _ => Err(TabError::ActorStopped),
    }
}

fn reply_value(reply: Reply) -> Result<RemoteValue, TabError> {
    match reply {
        Reply::Value(result) => result,
        _ => Err(TabError::ActorStopped),
    }
}

fn reply_content_size(reply: Reply) -> Result<(f32, f32), TabError> {
    match reply {
        Reply::ContentSize(result) => result,
        _ => Err(TabError::ActorStopped),
    }
}

fn reply_dom(reply: Reply) -> Result<renderer::DomReply, TabError> {
    match reply {
        Reply::Dom(result) => result.map(|reply| *reply),
        _ => Err(TabError::ActorStopped),
    }
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}
