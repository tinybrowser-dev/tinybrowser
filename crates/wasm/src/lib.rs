mod driver;
mod host_transport;

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::task::{Context as TaskContext, Poll, Wake, Waker};
use std::time::Duration;

use dial::{BufferedOnly, Dialer, HttpCache, Unlimited};
use net::{Agent, AgentOptions, CookieSameSite, Transport};
use renderer::{
    BrowserEvent, BrowsingContextHost, DialCancellation, DialCompletion, DialFailure, DialOutcome,
    DialRequest, EmbeddedRenderer, HistoryHandling, MessagingHost, Mount, NetworkHost, RemoteValue,
    RendererEvent, StorageChange, StorageHost, StorageKey, StorageKind,
};
use webstorage::StorageService;

#[expect(
    unsafe_code,
    clippy::same_length_and_capacity,
    clippy::mem_forget,
    reason = "wit-bindgen maintains the generated component ABI implementation"
)]
mod bindings {
    use super::Component;

    wit_bindgen::generate!({ world: "browser" });
    export!(Component with_types_in self);
}

use crate::bindings::exports::tinybrowser::browser::engine::{
    Cookie, Event, FrameEvent, Guest, GuestContext, GuestTab,
    HistoryHandling as HostHistoryHandling, InputEvent, PointerEvent, RenderedFrame, StorageEntry,
    Tab as ExportTab, WheelEvent,
};
use crate::bindings::tinybrowser::browser::types::{FetchError, FetchResponse};
use crate::host_transport::HostTransport;

fn shared_transport() -> &'static Arc<HostTransport> {
    static TRANSPORT: OnceLock<Arc<HostTransport>> = OnceLock::new();
    TRANSPORT.get_or_init(|| {
        Arc::new(HostTransport::new(
            u64::try_from(renderer::MAX_RESPONSE_BODY_BYTES).unwrap_or(u64::MAX),
        ))
    })
}

struct ContextState {
    agent: Agent,
    jar: Arc<Mutex<net::CookieJar>>,
    identity: net::ValidIdentity,
    cache: Arc<HttpCache>,
    storage: StorageService<u64>,
}

impl ContextState {
    fn new(identity: net::ValidIdentity) -> Self {
        let transport = Arc::clone(shared_transport());
        let jar = Arc::new(Mutex::new(net::CookieJar::default()));
        let options = AgentOptions {
            identity: identity.clone(),
            timeout_per_call: Some(dial::PAGE_FETCH_TIMEOUT),
            cookie_jar: Some(Arc::clone(&jar)),
            ..AgentOptions::default()
        };
        let cache = Arc::new(HttpCache::new(options.clock.clone()));
        let agent = Agent::with_transport(options, transport as Arc<dyn Transport>);
        Self {
            agent,
            jar,
            identity,
            cache,
            storage: StorageService::default(),
        }
    }

    fn agent_for_owner(&self, owner: u64) -> Agent {
        let transport = Arc::new(host_transport::OwnerTransport::new(
            Arc::clone(shared_transport()),
            owner,
        ));
        let options = AgentOptions {
            identity: self.identity.clone(),
            timeout_per_call: Some(dial::PAGE_FETCH_TIMEOUT),
            cookie_jar: Some(Arc::clone(&self.jar)),
            ..AgentOptions::default()
        };
        Agent::with_transport(options, transport as Arc<dyn Transport>)
    }
}

fn default_context() -> &'static Arc<ContextState> {
    static DEFAULT: OnceLock<Arc<ContextState>> = OnceLock::new();
    DEFAULT.get_or_init(|| Arc::new(ContextState::new(net::ValidIdentity::default())))
}

fn fonts() -> &'static Mutex<renderer::Fonts> {
    static FONTS: OnceLock<Mutex<renderer::Fonts>> = OnceLock::new();
    FONTS.get_or_init(|| Mutex::new(renderer::Fonts::default()))
}

struct Component;

impl Guest for Component {
    type Context = Context;
    type Tab = Tab;

    fn register_font(data: Vec<u8>) -> u32 {
        let count = lock(fonts()).register_fallback_font(data);
        u32::try_from(count).unwrap_or(u32::MAX)
    }

    fn complete_fetch(id: u64, result: Result<FetchResponse, FetchError>) -> bool {
        if !shared_transport().complete(id, result) {
            return false;
        }
        drain_ready_dials();
        true
    }
}

struct Context {
    state: Arc<ContextState>,
}

impl GuestContext for Context {
    fn new(user_agent: Option<String>) -> Result<Self, String> {
        let identity = match user_agent {
            Some(spec) => {
                net::ValidIdentity::parse_spec(&spec).map_err(|error| error.to_string())?
            }
            None => net::ValidIdentity::default(),
        };
        Ok(Self {
            state: Arc::new(ContextState::new(identity)),
        })
    }

    fn user_agent(&self) -> String {
        self.state.agent.identity().user_agent.as_str().to_owned()
    }

    fn open_tab(&self, url: String, html: String) -> Result<ExportTab, String> {
        let tab = Tab::in_context(Arc::clone(&self.state), url, html)?;
        Ok(ExportTab::new(tab))
    }

    fn cookies(&self) -> Vec<Cookie> {
        self.state
            .agent
            .all_cookie_records()
            .into_iter()
            .map(|record| Cookie {
                name: record.name,
                value: record.value,
                domain: record.domain,
                path: record.path,
                host_only: record.host_only,
                secure: record.secure,
                http_only: record.http_only,
                same_site: match record.same_site {
                    CookieSameSite::Strict => "Strict",
                    CookieSameSite::Lax => "Lax",
                    CookieSameSite::None => "None",
                    CookieSameSite::Default => "Default",
                }
                .to_owned(),
            })
            .collect()
    }

    fn set_cookie(&self, url: String, header: String) -> Result<bool, String> {
        let parsed = url::Url::parse(&url).map_err(|_| format!("invalid url: {url}"))?;
        Ok(self.state.agent.store_cookie_http(&header, &parsed))
    }

    fn clear_cookies(&self) {
        self.state.agent.clear_cookies();
    }

    fn local_storage(&self, origin: String) -> Vec<StorageEntry> {
        let Some(key) = StorageKey::parse(&origin) else {
            return Vec::new();
        };
        self.state
            .storage
            .local()
            .sorted_entries(&key)
            .into_iter()
            .map(|(key, value)| StorageEntry { key, value })
            .collect()
    }

    fn clear_local_storage(&self, origin: String) {
        let Some(key) = StorageKey::parse(&origin) else {
            return;
        };
        let _change = self.state.storage.local().clear(&key);
    }
}

type PendingNavigation = Arc<Mutex<Option<Result<Mount, String>>>>;

#[derive(Clone)]
enum HistoryEntry {
    Remote(String),
    Local { url: String, html: String },
}

impl HistoryEntry {
    fn url(&self) -> &str {
        match self {
            Self::Remote(url) | Self::Local { url, .. } => url,
        }
    }
}

struct TabHistory {
    entries: Vec<HistoryEntry>,
    index: usize,
}

impl TabHistory {
    fn new(initial: HistoryEntry) -> Self {
        Self {
            entries: vec![initial],
            index: 0,
        }
    }

    fn push(&mut self, entry: HistoryEntry) {
        if self.entries.get(self.index).map(HistoryEntry::url) == Some(entry.url()) {
            self.entries[self.index] = entry;
            return;
        }
        self.entries.truncate(self.index + 1);
        self.entries.push(entry);
        self.index = self.entries.len() - 1;
    }

    fn can_go_back(&self) -> bool {
        self.index > 0
    }

    fn can_go_forward(&self) -> bool {
        self.index + 1 < self.entries.len()
    }

    fn step_back(&mut self) -> Option<HistoryEntry> {
        if self.index == 0 {
            return None;
        }
        self.index -= 1;
        self.entries.get(self.index).cloned()
    }

    fn step_forward(&mut self) -> Option<HistoryEntry> {
        if self.index + 1 >= self.entries.len() {
            return None;
        }
        self.index += 1;
        self.entries.get(self.index).cloned()
    }
}

struct Tab {
    owner: u64,
    context: Arc<ContextState>,
    services: Arc<WasmServices>,
    renderer: RefCell<EmbeddedRenderer>,
    pending_navigation: PendingNavigation,
    active_navigation_dial: RefCell<Option<u64>>,
    navigation_errors: RefCell<Vec<String>>,
    history: RefCell<TabHistory>,
}

impl GuestTab for Tab {
    fn new(url: String, html: String) -> Result<Self, String> {
        Self::in_context(Arc::clone(default_context()), url, html)
    }

    fn mount(&self, url: String, html: String) -> Result<(), String> {
        self.mount_entry(&url, &html)?;
        self.history
            .borrow_mut()
            .push(HistoryEntry::Local { url, html });
        Ok(())
    }

    fn goto(&self, url: String) -> Result<(), String> {
        self.goto_url(&url)?;
        self.history.borrow_mut().push(HistoryEntry::Remote(url));
        Ok(())
    }

    fn back(&self) -> Result<bool, String> {
        let Some(entry) = self.history.borrow_mut().step_back() else {
            return Ok(false);
        };
        self.restore_entry(&entry)?;
        Ok(true)
    }

    fn forward(&self) -> Result<bool, String> {
        let Some(entry) = self.history.borrow_mut().step_forward() else {
            return Ok(false);
        };
        self.restore_entry(&entry)?;
        Ok(true)
    }

    fn can_go_back(&self) -> bool {
        self.history.borrow().can_go_back()
    }

    fn can_go_forward(&self) -> bool {
        self.history.borrow().can_go_forward()
    }

    fn owner(&self) -> u64 {
        self.owner
    }

    fn url(&self) -> Result<String, String> {
        self.with_renderer(|renderer| renderer.eval("location.href"))
    }

    fn eval(&self, source: String) -> Result<String, String> {
        self.with_renderer(|renderer| renderer.eval(&source))
    }

    fn eval_bounded(&self, source: String, timeout_ms: u64) -> Result<String, String> {
        let timeout = Duration::from_millis(timeout_ms.max(1));
        self.renderer
            .borrow_mut()
            .execute(&source, Some(timeout))
            .map(|value| format_remote_value(&value))
            .map_err(|error| error.to_string())
    }

    fn dispatch_input(&self, event: InputEvent) -> Result<(), String> {
        let script = input_action_script(&event)?;
        self.renderer
            .borrow_mut()
            .execute_browser(&script, None)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    fn set_user_agent(&self, value: Option<String>) -> Result<(), String> {
        let override_identity = match value {
            Some(spec) => {
                Some(net::ValidIdentity::parse_spec(&spec).map_err(|error| error.to_string())?)
            }
            None => None,
        };
        self.services.set_identity(override_identity);
        let effective = self.services.identity();
        self.renderer.borrow_mut().set_identity(effective);
        Ok(())
    }

    fn user_agent(&self) -> String {
        self.services.identity().user_agent.as_str().to_owned()
    }

    fn set_viewport(&self, width: u32, height: u32) -> Result<(), String> {
        if width == 0 || height == 0 {
            return Err("viewport dimensions must be positive".into());
        }
        self.renderer
            .borrow_mut()
            .set_viewport(width, height)
            .map_err(|error| error.to_string())
    }

    fn render(&self, width: u32, height: u32) -> Result<RenderedFrame, String> {
        if width == 0 || height == 0 {
            return Err("viewport dimensions must be positive".into());
        }
        let mut renderer = self.renderer.borrow_mut();
        renderer
            .set_viewport(width, height)
            .map_err(|error| error.to_string())?;
        let image = renderer
            .render(&renderer::ScreenshotRequest {
                viewport_width: width,
                viewport_height: height,
                clip: None,
                format: renderer::ScreenshotFormat::Png,
                omit_background: false,
            })
            .map_err(|error| error.to_string())?;
        Ok(RenderedFrame {
            width: image.width,
            height: image.height,
            rgba: image.data,
        })
    }

    fn step(&self) -> Option<u64> {
        drain_ready_dials();
        if let Some(outcome) = lock(&self.pending_navigation).take() {
            *self.active_navigation_dial.borrow_mut() = None;
            match outcome {
                Ok(mount) => {
                    self.services
                        .set_committed_origin(StorageKey::from_origin(&mount.head.url.origin()));
                    if let Err(error) = self.renderer.borrow_mut().mount(&mount) {
                        self.navigation_errors.borrow_mut().push(error.to_string());
                    }
                }
                Err(error) => {
                    self.navigation_errors.borrow_mut().push(error);
                }
            }
        }
        let mut renderer = self.renderer.borrow_mut();
        let delay = if renderer.drain_ready() {
            Some(Duration::ZERO)
        } else {
            renderer.time_until_deadline()
        };
        delay.map(|delay| {
            let milliseconds = delay.as_nanos().saturating_add(999_999) / 1_000_000;
            u64::try_from(milliseconds).unwrap_or(u64::MAX)
        })
    }

    fn events(&self) -> Result<Vec<FrameEvent>, String> {
        let mut out: Vec<FrameEvent> = self
            .renderer
            .borrow_mut()
            .take_events()
            .map(|events| {
                events
                    .into_iter()
                    .filter_map(|(frame, event)| {
                        if frame.get() == 0
                            && let RendererEvent::Browser(BrowserEvent::Navigated {
                                ref url, ..
                            }) = event
                        {
                            self.services.set_committed_origin(StorageKey::parse(url));
                            self.history
                                .borrow_mut()
                                .push(HistoryEntry::Remote(url.clone()));
                        }
                        to_event(event).map(|event| FrameEvent {
                            frame: frame.get(),
                            event,
                        })
                    })
                    .collect()
            })
            .map_err(|error| error.to_string())?;
        for error in self.navigation_errors.borrow_mut().drain(..) {
            out.push(FrameEvent {
                frame: 0,
                event: Event::NavigationFailed(error),
            });
        }
        Ok(out)
    }

    fn title(&self) -> Result<String, String> {
        self.with_renderer(|renderer| renderer.eval("document.title"))
    }

    fn text(&self) -> Result<String, String> {
        self.with_renderer(|renderer| {
            renderer.eval("document.body === null ? '' : document.body.innerText")
        })
    }

    fn session_storage(&self, origin: String) -> Vec<StorageEntry> {
        let Some(key) = StorageKey::parse(&origin) else {
            return Vec::new();
        };
        self.context
            .storage
            .session()
            .sorted_entries(self.owner, &key)
            .into_iter()
            .map(|(key, value)| StorageEntry { key, value })
            .collect()
    }

    fn clear_session_storage(&self, origin: String) {
        let Some(key) = StorageKey::parse(&origin) else {
            return;
        };
        let _change = self.context.storage.session().clear(self.owner, &key);
    }
}

impl Tab {
    fn in_context(context: Arc<ContextState>, url: String, html: String) -> Result<Self, String> {
        let parsed = url::Url::parse(&url).map_err(|_| format!("invalid url: {url}"))?;
        let services = Arc::new(WasmServices::new(
            Arc::clone(&context),
            StorageKey::from_origin(&parsed.origin()),
        ));
        let owner = services.owner;
        let shared_fonts = lock(fonts()).clone();
        let mut renderer = EmbeddedRenderer::with_fonts(
            Arc::clone(&services) as Arc<dyn renderer::BrowserServices>,
            context.agent.identity().clone(),
            &shared_fonts,
        );
        let mount = Mount {
            head: renderer::ResponseHead::local(
                parsed,
                Some("text/html; charset=utf-8".into()),
                None,
            ),
            tab: renderer::TabState::default(),
            body: html.as_bytes().to_vec(),
        };
        renderer.mount(&mount).map_err(|error| error.to_string())?;
        Ok(Self {
            owner,
            context,
            services,
            renderer: RefCell::new(renderer),
            pending_navigation: Arc::new(Mutex::new(None)),
            active_navigation_dial: RefCell::new(None),
            navigation_errors: RefCell::new(Vec::new()),
            history: RefCell::new(TabHistory::new(HistoryEntry::Local { url, html })),
        })
    }

    fn mount_entry(&self, url: &str, html: &str) -> Result<(), String> {
        let parsed = url::Url::parse(url).map_err(|_| format!("invalid url: {url}"))?;
        self.cancel_pending_navigation();
        self.services
            .set_committed_origin(StorageKey::from_origin(&parsed.origin()));
        let mount = Mount {
            head: renderer::ResponseHead::local(
                parsed,
                Some("text/html; charset=utf-8".into()),
                None,
            ),
            tab: renderer::TabState::default(),
            body: html.as_bytes().to_vec(),
        };
        self.renderer
            .borrow_mut()
            .mount(&mount)
            .map_err(|error| error.to_string())
    }

    fn goto_url(&self, url: &str) -> Result<(), String> {
        let parsed = url::Url::parse(url).map_err(|_| format!("invalid url: {url}"))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(format!("unsupported url scheme: {}", parsed.scheme()));
        }
        self.cancel_pending_navigation();
        let id = NEXT_DIAL.fetch_add(1, Ordering::Relaxed);
        *self.active_navigation_dial.borrow_mut() = Some(id);
        let dial = Dial {
            task: DialTask::Navigation {
                future: Box::pin(driver::navigate(
                    self.services.agent.clone(),
                    parsed,
                    self.services.overrides(),
                )),
                slot: Arc::clone(&self.pending_navigation),
            },
        };
        DialRegistry::spawn(&self.services.registry, id, dial);
        Ok(())
    }

    fn restore_entry(&self, entry: &HistoryEntry) -> Result<(), String> {
        match entry {
            HistoryEntry::Remote(url) => self.goto_url(url),
            HistoryEntry::Local { url, html } => self.mount_entry(url, html),
        }
    }

    fn with_renderer(
        &self,
        operation: impl FnOnce(&mut EmbeddedRenderer) -> Result<String, renderer::TabError>,
    ) -> Result<String, String> {
        operation(&mut self.renderer.borrow_mut()).map_err(|error| error.to_string())
    }

    fn cancel_pending_navigation(&self) {
        if let Some(id) = self.active_navigation_dial.borrow_mut().take() {
            self.services.registry.cancel(id);
        }
        let _discarded = lock(&self.pending_navigation).take();
    }
}

fn format_remote_value(value: &RemoteValue) -> String {
    match value {
        RemoteValue::Undefined => "undefined".to_owned(),
        RemoteValue::Null => "null".to_owned(),
        RemoteValue::Bool(flag) => flag.to_string(),
        RemoteValue::Number(number) => number.to_string(),
        RemoteValue::String(text) => text.clone(),
        RemoteValue::List(items) => {
            let parts: Vec<String> = items.iter().map(format_remote_value).collect();
            format!("[{}]", parts.join(", "))
        }
        RemoteValue::Map(entries) => {
            let parts: Vec<String> = entries
                .iter()
                .map(|(key, item)| format!("{key}: {}", format_remote_value(item)))
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
        RemoteValue::Node(id) => format!("[Node #{id}]"),
    }
}

fn finite(value: f64, name: &str) -> Result<f64, String> {
    value
        .is_finite()
        .then_some(value)
        .ok_or_else(|| format!("non-finite {name}"))
}

fn js_string_literal(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c == '\u{2028}' || c == '\u{2029}' => {
                use std::fmt::Write as _;
                let _ = write!(out, "\\u{:04X}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// https://w3c.github.io/webdriver/#perform-actions
fn input_action_script(event: &InputEvent) -> Result<String, String> {
    let payload = match event {
        InputEvent::PointerMove(PointerEvent { x, y, .. }) => {
            let x = finite(*x, "x")?;
            let y = finite(*y, "y")?;
            format!(
                r#"[{{"type":"pointer","id":"mouse","parameters":{{"pointerType":"mouse"}},"actions":[{{"type":"pointerMove","origin":"viewport","x":{x},"y":{y}}}]}}]"#
            )
        }
        InputEvent::PointerDown(PointerEvent { x, y, button }) => {
            let x = finite(*x, "x")?;
            let y = finite(*y, "y")?;
            format!(
                r#"[{{"type":"pointer","id":"mouse","parameters":{{"pointerType":"mouse"}},"actions":[{{"type":"pointerMove","origin":"viewport","x":{x},"y":{y}}},{{"type":"pointerDown","button":{button}}}]}}]"#
            )
        }
        InputEvent::PointerUp(PointerEvent { x, y, button }) => {
            let x = finite(*x, "x")?;
            let y = finite(*y, "y")?;
            format!(
                r#"[{{"type":"pointer","id":"mouse","parameters":{{"pointerType":"mouse"}},"actions":[{{"type":"pointerMove","origin":"viewport","x":{x},"y":{y}}},{{"type":"pointerUp","button":{button}}}]}}]"#
            )
        }
        InputEvent::Wheel(WheelEvent {
            x,
            y,
            delta_x,
            delta_y,
        }) => {
            let x = finite(*x, "x")?;
            let y = finite(*y, "y")?;
            let dx = finite(*delta_x, "delta-x")?;
            let dy = finite(*delta_y, "delta-y")?;
            format!(
                r#"[{{"type":"wheel","id":"wheel","actions":[{{"type":"scroll","x":{x},"y":{y},"deltaX":{dx},"deltaY":{dy}}}]}}]"#
            )
        }
        InputEvent::KeyDown(key) => {
            let key = js_string_literal(key);
            format!(
                r#"[{{"type":"key","id":"keyboard","actions":[{{"type":"keyDown","value":{key}}}]}}]"#
            )
        }
        InputEvent::KeyUp(key) => {
            let key = js_string_literal(key);
            format!(
                r#"[{{"type":"key","id":"keyboard","actions":[{{"type":"keyUp","value":{key}}}]}}]"#
            )
        }
        InputEvent::InsertText(text) => {
            let text = js_string_literal(text);
            format!(
                r#"[{{"type":"key","id":"keyboard","actions":[{{"type":"insertText","value":{text}}}]}}]"#
            )
        }
    };
    Ok(format!(
        "(function(){{return host.__tbWebDriverActions({payload});}})()"
    ))
}

fn to_event(event: RendererEvent) -> Option<Event> {
    match event {
        RendererEvent::Browser(event) => match event {
            BrowserEvent::Navigated { url, .. } => Some(Event::Navigated(url)),
            BrowserEvent::HistoryUpdated {
                url,
                state,
                handling,
            } => Some(Event::HistoryUpdated((
                url,
                state,
                match handling {
                    HistoryHandling::Push => HostHistoryHandling::Push,
                    HistoryHandling::Replace => HostHistoryHandling::Replace,
                },
            ))),
            BrowserEvent::HistoryTraversal { delta } => Some(Event::HistoryTraversal(delta)),
            BrowserEvent::Load => Some(Event::Load),
            BrowserEvent::DomContentLoaded => Some(Event::DomContentLoaded),
            BrowserEvent::NavigationStarted { .. }
            | BrowserEvent::FrameAdded { .. }
            | BrowserEvent::FrameRemoved
            | BrowserEvent::RealmReplaced
            | BrowserEvent::Binding { .. }
            | BrowserEvent::Console { .. }
            | BrowserEvent::ExceptionThrown { .. } => None,
        },
        RendererEvent::Timer(id) => Some(Event::Timer(id.get())),
        RendererEvent::Fetch { status } => Some(Event::Fetch(status)),
        RendererEvent::FetchFailed => Some(Event::FetchFailed),
        RendererEvent::ScriptFailed => Some(Event::ScriptFailed),
    }
}

struct DialRegistry {
    dials: Mutex<HashMap<u64, Dial>>,
}

struct Dial {
    task: DialTask,
}

enum DialTask {
    Subresource {
        future: Pin<Box<dyn Future<Output = Result<DialOutcome, DialFailure>> + Send>>,
        completion: Option<DialCompletion>,
    },
    Navigation {
        future: Pin<Box<dyn Future<Output = Result<Mount, String>> + Send>>,
        slot: PendingNavigation,
    },
}

impl Dial {
    fn poll_once(&mut self, context: &mut TaskContext<'_>) -> Poll<()> {
        match &mut self.task {
            DialTask::Subresource { future, completion } => match future.as_mut().poll(context) {
                Poll::Ready(result) => {
                    if let Some(completion) = completion.take() {
                        completion(result);
                    }
                    Poll::Ready(())
                }
                Poll::Pending => Poll::Pending,
            },
            DialTask::Navigation { future, slot } => match future.as_mut().poll(context) {
                Poll::Ready(result) => {
                    *lock(slot) = Some(result);
                    Poll::Ready(())
                }
                Poll::Pending => Poll::Pending,
            },
        }
    }

    fn fail(&mut self, failure: DialFailure) {
        match &mut self.task {
            DialTask::Subresource { completion, .. } => {
                if let Some(completion) = completion.take() {
                    completion(Err(failure));
                }
            }
            DialTask::Navigation { slot, .. } => {
                *lock(slot) = Some(Err(format!("{failure:?}")));
            }
        }
    }
}

struct DialWaker {
    registry: Weak<DialRegistry>,
    id: u64,
}

impl Wake for DialWaker {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        if let Some(registry) = self.registry.upgrade() {
            lock(ready_queue()).push_back((registry, self.id));
        }
    }
}

impl DialRegistry {
    fn new() -> Self {
        Self {
            dials: Mutex::new(HashMap::new()),
        }
    }

    fn spawn(registry: &Arc<Self>, id: u64, dial: Dial) {
        lock(&registry.dials).insert(id, dial);
        lock(ready_queue()).push_back((Arc::clone(registry), id));
        drain_ready_dials();
    }

    fn cancel(&self, id: u64) {
        let Some(mut dial) = lock(&self.dials).remove(&id) else {
            return;
        };
        dial.fail(DialFailure::Cancelled);
    }
}

type ReadyEntry = (Arc<DialRegistry>, u64);

fn ready_queue() -> &'static Mutex<VecDeque<ReadyEntry>> {
    static READY: OnceLock<Mutex<VecDeque<ReadyEntry>>> = OnceLock::new();
    READY.get_or_init(|| Mutex::new(VecDeque::new()))
}

fn drain_ready_dials() {
    while let Some((registry, id)) = lock(ready_queue()).pop_front() {
        let Some(mut dial) = lock(&registry.dials).remove(&id) else {
            continue;
        };
        let waker = Waker::from(Arc::new(DialWaker {
            registry: Arc::downgrade(&registry),
            id,
        }));
        let mut context = TaskContext::from_waker(&waker);
        if dial.poll_once(&mut context).is_pending() {
            lock(&registry.dials).insert(id, dial);
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

static NEXT_OWNER: AtomicU64 = AtomicU64::new(1);
static NEXT_DIAL: AtomicU64 = AtomicU64::new(1);

struct WasmServices {
    owner: u64,
    context: Arc<ContextState>,
    agent: Agent,
    dialer: Dialer,
    overrides: Mutex<net::RequestOverrides>,
    committed_origin: Mutex<Option<StorageKey>>,
    registry: Arc<DialRegistry>,
}

impl WasmServices {
    fn new(context: Arc<ContextState>, initial_origin: Option<StorageKey>) -> Self {
        let owner = NEXT_OWNER.fetch_add(1, Ordering::Relaxed);
        context.storage.create_session(owner);
        let agent = context.agent_for_owner(owner);
        let dialer = Dialer::new(agent.clone(), Arc::clone(&context.cache));
        Self {
            owner,
            context,
            agent,
            dialer,
            overrides: Mutex::new(net::RequestOverrides::default()),
            committed_origin: Mutex::new(initial_origin),
            registry: Arc::new(DialRegistry::new()),
        }
    }

    fn set_committed_origin(&self, origin: Option<StorageKey>) {
        *lock(&self.committed_origin) = origin;
    }

    fn authorize_storage(&self, origin: &StorageKey) -> Option<StorageKey> {
        let committed = lock(&self.committed_origin);
        committed
            .as_ref()
            .filter(|allowed| *allowed == origin)
            .cloned()
    }

    fn set_identity(&self, identity: Option<net::ValidIdentity>) {
        lock(&self.overrides).identity = identity;
    }

    fn identity(&self) -> net::ValidIdentity {
        lock(&self.overrides)
            .identity
            .clone()
            .unwrap_or_else(|| self.context.identity.clone())
    }

    fn overrides(&self) -> net::RequestOverrides {
        lock(&self.overrides).clone()
    }
}

impl NetworkHost for WasmServices {
    fn start_dial(&self, request: DialRequest, completion: DialCompletion) -> DialCancellation {
        let Some(initiator) = request.initiator.to_url().map(|url| url.origin()) else {
            completion(Err(DialFailure::Connect));
            return Box::new(|| {});
        };
        let id = NEXT_DIAL.fetch_add(1, Ordering::Relaxed);
        let dialer = self.dialer.clone();
        let owner = self.owner;
        let overrides = self.overrides();
        let dial = Dial {
            task: DialTask::Subresource {
                future: Box::pin(async move {
                    dialer
                        .dial(
                            &request,
                            &initiator,
                            owner,
                            overrides,
                            &Unlimited,
                            &BufferedOnly,
                        )
                        .await
                }),
                completion: Some(completion),
            },
        };
        DialRegistry::spawn(&self.registry, id, dial);
        let registry = Arc::clone(&self.registry);
        Box::new(move || registry.cancel(id))
    }
}

impl StorageHost for WasmServices {
    fn cookies_for(&self, url: &url::Url, partition_key: Option<&str>) -> String {
        self.context.agent.cookies_for_partition(url, partition_key)
    }

    fn set_cookie(&self, value: &str, url: &url::Url, partition_key: Option<&str>) {
        let _stored = self
            .context
            .agent
            .set_cookie_partition(value, url, partition_key);
    }

    fn cookie_store_records(
        &self,
        url: &url::Url,
        partition_key: Option<&str>,
    ) -> Vec<renderer::ScriptCookie> {
        self.context
            .agent
            .script_cookie_records_partition(url, partition_key)
            .into_iter()
            .map(|record| renderer::ScriptCookie::from_record(&record))
            .collect()
    }

    fn cookie_store_store(
        &self,
        write: &net::ScriptCookieWrite,
        url: &url::Url,
        partition_key: Option<&str>,
    ) {
        self.context
            .agent
            .set_script_cookie_partition(write, url, partition_key);
    }

    fn remote_session_get(
        &self,
        _window: renderer::WindowId,
        _origin: &StorageKey,
        _key: &str,
    ) -> Option<String> {
        None
    }

    fn storage_get(&self, kind: StorageKind, origin: &StorageKey, key: &str) -> Option<String> {
        let authorized = self.authorize_storage(origin)?;
        self.context.storage.get(kind, self.owner, &authorized, key)
    }

    fn storage_keys(&self, kind: StorageKind, origin: &StorageKey) -> Vec<String> {
        let Some(authorized) = self.authorize_storage(origin) else {
            return Vec::new();
        };
        self.context.storage.keys(kind, self.owner, &authorized)
    }

    fn storage_set(
        &self,
        kind: StorageKind,
        origin: &StorageKey,
        _url: &str,
        key: &str,
        value: &str,
        _source: renderer::FrameId,
    ) -> Result<Option<StorageChange>, renderer::StorageError> {
        let Some(authorized) = self.authorize_storage(origin) else {
            return Ok(None);
        };
        self.context
            .storage
            .set(kind, self.owner, &authorized, key, value)
    }

    fn storage_remove(
        &self,
        kind: StorageKind,
        origin: &StorageKey,
        _url: &str,
        key: &str,
        _source: renderer::FrameId,
    ) -> Option<StorageChange> {
        let authorized = self.authorize_storage(origin)?;
        self.context
            .storage
            .remove(kind, self.owner, &authorized, key)
    }

    fn storage_clear(
        &self,
        kind: StorageKind,
        origin: &StorageKey,
        _url: &str,
        _source: renderer::FrameId,
    ) -> Option<StorageChange> {
        let authorized = self.authorize_storage(origin)?;
        self.context.storage.clear(kind, self.owner, &authorized)
    }
}

impl BrowsingContextHost for WasmServices {
    fn window_open(
        &self,
        _url: Option<&url::Url>,
        _target: &renderer::TargetName,
        _opener: renderer::OpenerPolicy,
        _source: &renderer::NavigationSource,
    ) -> Option<renderer::WindowId> {
        None
    }

    fn navigate_named(
        &self,
        _name: &renderer::ContextName,
        _url: Option<&url::Url>,
        _opener: renderer::OpenerPolicy,
        _source: &renderer::NavigationSource,
    ) -> Option<renderer::WindowId> {
        None
    }

    fn set_window_name(&self, _change: renderer::NameChange) {}

    fn window_close(&self, _window: renderer::WindowId) {}

    fn window_opener(&self) -> Option<renderer::WindowId> {
        None
    }

    fn window_post_message(&self, _window: renderer::WindowId, _message: renderer::PostedMessage) {}
}

impl MessagingHost for WasmServices {
    fn broadcast_post(&self, _origin: &StorageKey, _name: &str, _payload: &str, _channel: u64) {}
}

impl Drop for WasmServices {
    fn drop(&mut self) {
        let ids: Vec<u64> = lock(&self.registry.dials).keys().copied().collect();
        for id in ids {
            self.registry.cancel(id);
        }
        self.context.storage.remove_session(self.owner);
    }
}
