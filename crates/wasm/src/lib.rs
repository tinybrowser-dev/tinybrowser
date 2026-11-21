mod driver;
mod host_transport;

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::task::{Context, Poll, Wake, Waker};

use dial::{BufferedOnly, Dialer, HttpCache, Unlimited};
use net::{Agent, AgentOptions, Transport};
use renderer::{
    BrowserEvent, BrowsingContextHost, DialCancellation, DialCompletion, DialFailure, DialOutcome,
    DialRequest, EmbeddedRenderer, HistoryHandling, MessagingHost, Mount, NetworkHost,
    RendererEvent, StorageChange, StorageHost, StorageKind,
};
use webstorage::StorageArea;

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
    Event, FrameEvent, Guest, GuestTab, HistoryHandling as HostHistoryHandling, InputEvent,
    PointerEvent, RenderedFrame, WheelEvent,
};
use crate::bindings::tinybrowser::browser::types::{FetchError, FetchResponse};
use crate::host_transport::HostTransport;

struct Network {
    agent: Agent,
    cache: Arc<HttpCache>,
    transport: Arc<HostTransport>,
}

fn network() -> &'static Network {
    static NETWORK: OnceLock<Network> = OnceLock::new();
    NETWORK.get_or_init(|| {
        let transport = Arc::new(HostTransport::new(
            u64::try_from(renderer::MAX_RESPONSE_BODY_BYTES).unwrap_or(u64::MAX),
        ));
        let options = AgentOptions {
            timeout_per_call: Some(dial::PAGE_FETCH_TIMEOUT),
            ..AgentOptions::default()
        };
        let cache = Arc::new(HttpCache::new(options.clock.clone()));
        let agent = Agent::with_transport(options, Arc::clone(&transport) as Arc<dyn Transport>);
        Network {
            agent,
            cache,
            transport,
        }
    })
}

fn fonts() -> &'static Mutex<renderer::Fonts> {
    static FONTS: OnceLock<Mutex<renderer::Fonts>> = OnceLock::new();
    FONTS.get_or_init(|| Mutex::new(renderer::Fonts::default()))
}

struct Component;

impl Guest for Component {
    type Tab = Tab;

    fn register_font(data: Vec<u8>) -> u32 {
        let count = lock(fonts()).register_fallback_font(data);
        u32::try_from(count).unwrap_or(u32::MAX)
    }

    fn complete_fetch(id: u64, result: Result<FetchResponse, FetchError>) -> bool {
        if !network().transport.complete(id, result) {
            return false;
        }
        drain_ready_dials();
        true
    }
}

type PendingNavigation = Arc<Mutex<Option<Result<Mount, String>>>>;

struct Tab {
    owner: u64,
    services: Arc<WasmServices>,
    renderer: RefCell<EmbeddedRenderer>,
    pending_navigation: PendingNavigation,
    active_navigation_dial: RefCell<Option<u64>>,
    navigation_errors: RefCell<Vec<String>>,
}

impl GuestTab for Tab {
    fn new(url: String, html: String) -> Result<Self, String> {
        let parsed = url::Url::parse(&url).map_err(|_| format!("invalid url: {url}"))?;
        let services = Arc::new(WasmServices::new());
        let owner = services.owner;
        let shared_fonts = lock(fonts()).clone();
        let mut renderer = EmbeddedRenderer::with_fonts(
            Arc::clone(&services) as Arc<dyn renderer::BrowserServices>,
            network().agent.identity().clone(),
            &shared_fonts,
        );
        let mount = Mount {
            head: renderer::ResponseHead::local(
                parsed,
                Some("text/html; charset=utf-8".into()),
                None,
            ),
            tab: renderer::TabState::default(),
            body: html.into_bytes(),
        };
        renderer.mount(&mount).map_err(|error| error.to_string())?;
        Ok(Self {
            owner,
            services,
            renderer: RefCell::new(renderer),
            pending_navigation: Arc::new(Mutex::new(None)),
            active_navigation_dial: RefCell::new(None),
            navigation_errors: RefCell::new(Vec::new()),
        })
    }

    fn mount(&self, url: String, html: String) -> Result<(), String> {
        let parsed = url::Url::parse(&url).map_err(|_| format!("invalid url: {url}"))?;
        self.cancel_pending_navigation();
        let mount = Mount {
            head: renderer::ResponseHead::local(
                parsed,
                Some("text/html; charset=utf-8".into()),
                None,
            ),
            tab: renderer::TabState::default(),
            body: html.into_bytes(),
        };
        self.renderer
            .borrow_mut()
            .mount(&mount)
            .map_err(|error| error.to_string())
    }

    fn goto(&self, url: String) -> Result<(), String> {
        let parsed = url::Url::parse(&url).map_err(|_| format!("invalid url: {url}"))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(format!("unsupported url scheme: {}", parsed.scheme()));
        }
        self.cancel_pending_navigation();
        let id = NEXT_DIAL.fetch_add(1, Ordering::Relaxed);
        *self.active_navigation_dial.borrow_mut() = Some(id);
        let dial = Dial {
            task: DialTask::Navigation {
                future: Box::pin(driver::navigate(
                    network().agent.clone(),
                    parsed,
                    self.owner,
                )),
                slot: Arc::clone(&self.pending_navigation),
            },
        };
        DialRegistry::spawn(&self.services.registry, id, dial);
        Ok(())
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

    fn dispatch_input(&self, event: InputEvent) -> Result<(), String> {
        let script = input_action_script(&event)?;
        self.renderer
            .borrow_mut()
            .execute_browser(&script, None)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    fn render(&self, width: u32, height: u32) -> Result<RenderedFrame, String> {
        if width == 0 || height == 0 {
            return Err("viewport dimensions must be positive".into());
        }
        let image = self
            .renderer
            .borrow_mut()
            .render(&renderer::ScreenshotRequest {
                viewport_width: width,
                viewport_height: height,
                clip: None,
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
            Some(std::time::Duration::ZERO)
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
}

impl Tab {
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
            | BrowserEvent::FrameRemoved => None,
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
    fn poll_once(&mut self, context: &mut Context<'_>) -> Poll<()> {
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
        let mut context = Context::from_waker(&waker);
        if dial.poll_once(&mut context).is_pending() {
            lock(&registry.dials).insert(id, dial);
        }
    }
}

fn local_storage() -> &'static Mutex<HashMap<renderer::StorageKey, StorageArea>> {
    static AREAS: OnceLock<Mutex<HashMap<renderer::StorageKey, StorageArea>>> = OnceLock::new();
    AREAS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn indexed_db() -> &'static Mutex<HashMap<renderer::StorageKey, StorageArea>> {
    static AREAS: OnceLock<Mutex<HashMap<renderer::StorageKey, StorageArea>>> = OnceLock::new();
    AREAS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn session_storage() -> &'static Mutex<HashMap<u64, HashMap<renderer::StorageKey, StorageArea>>> {
    static NAMESPACES: OnceLock<Mutex<HashMap<u64, HashMap<renderer::StorageKey, StorageArea>>>> =
        OnceLock::new();
    NAMESPACES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

static NEXT_OWNER: AtomicU64 = AtomicU64::new(1);
static NEXT_DIAL: AtomicU64 = AtomicU64::new(1);

struct WasmServices {
    owner: u64,
    dialer: Dialer,
    registry: Arc<DialRegistry>,
}

impl WasmServices {
    fn new() -> Self {
        let net = network();
        Self {
            owner: NEXT_OWNER.fetch_add(1, Ordering::Relaxed),
            dialer: Dialer::new(net.agent.clone(), Arc::clone(&net.cache)),
            registry: Arc::new(DialRegistry::new()),
        }
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
        let dial = Dial {
            task: DialTask::Subresource {
                future: Box::pin(async move {
                    dialer
                        .dial(
                            &request,
                            &initiator,
                            owner,
                            Some(owner),
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

    fn cookie_store_records(
        &self,
        url: &url::Url,
        partition_key: Option<&str>,
    ) -> Vec<renderer::ScriptCookie> {
        network()
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
        network()
            .agent
            .set_script_cookie_partition(write, url, partition_key);
    }

    fn cookies_for(&self, url: &url::Url, partition_key: Option<&str>) -> String {
        network().agent.cookies_for_partition(url, partition_key)
    }

    fn set_cookie(&self, value: &str, url: &url::Url, partition_key: Option<&str>) {
        let _stored = network()
            .agent
            .set_cookie_partition(value, url, partition_key);
    }
}

impl StorageHost for WasmServices {
    fn storage_get(
        &self,
        kind: StorageKind,
        origin: &renderer::StorageKey,
        key: &str,
    ) -> Option<String> {
        match kind {
            StorageKind::Local => lock(local_storage())
                .get(origin)
                .and_then(|area| area.get(key)),
            StorageKind::Session => lock(session_storage())
                .get(&self.owner)
                .and_then(|namespace| namespace.get(origin))
                .and_then(|area| area.get(key)),
            StorageKind::IndexedDb => lock(indexed_db())
                .get(origin)
                .and_then(|area| area.get(key)),
        }
    }

    fn storage_keys(&self, kind: StorageKind, origin: &renderer::StorageKey) -> Vec<String> {
        match kind {
            StorageKind::Local => lock(local_storage())
                .get(origin)
                .map_or_default(StorageArea::keys),
            StorageKind::Session => lock(session_storage())
                .get(&self.owner)
                .and_then(|namespace| namespace.get(origin))
                .map_or_default(StorageArea::keys),
            StorageKind::IndexedDb => lock(indexed_db())
                .get(origin)
                .map_or_default(StorageArea::keys),
        }
    }

    fn storage_set(
        &self,
        kind: StorageKind,
        origin: &renderer::StorageKey,
        _url: &str,
        key: &str,
        value: &str,
        _source: renderer::FrameId,
    ) -> Result<Option<StorageChange>, renderer::StorageError> {
        match kind {
            StorageKind::Local => lock(local_storage())
                .entry(origin.clone())
                .or_default()
                .set(key, value),
            StorageKind::Session => lock(session_storage())
                .entry(self.owner)
                .or_default()
                .entry(origin.clone())
                .or_default()
                .set(key, value),
            StorageKind::IndexedDb => lock(indexed_db())
                .entry(origin.clone())
                .or_default()
                .set(key, value),
        }
    }

    fn storage_remove(
        &self,
        kind: StorageKind,
        origin: &renderer::StorageKey,
        _url: &str,
        key: &str,
        _source: renderer::FrameId,
    ) -> Option<StorageChange> {
        match kind {
            StorageKind::Local => lock(local_storage()).get_mut(origin)?.remove(key),
            StorageKind::Session => lock(session_storage())
                .get_mut(&self.owner)?
                .get_mut(origin)?
                .remove(key),
            StorageKind::IndexedDb => lock(indexed_db()).get_mut(origin)?.remove(key),
        }
    }

    fn storage_clear(
        &self,
        kind: StorageKind,
        origin: &renderer::StorageKey,
        _url: &str,
        _source: renderer::FrameId,
    ) -> Option<StorageChange> {
        match kind {
            StorageKind::Local => lock(local_storage()).get_mut(origin)?.clear(),
            StorageKind::Session => lock(session_storage())
                .get_mut(&self.owner)?
                .get_mut(origin)?
                .clear(),
            StorageKind::IndexedDb => lock(indexed_db()).get_mut(origin)?.clear(),
        }
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

    fn remote_session_get(
        &self,
        _window: renderer::WindowId,
        _origin: &renderer::StorageKey,
        _key: &str,
    ) -> Option<String> {
        None
    }
}

impl MessagingHost for WasmServices {
    fn broadcast_post(
        &self,
        _origin: &renderer::StorageKey,
        _name: &str,
        _payload: &str,
        _channel: u64,
    ) {
    }
}

impl Drop for WasmServices {
    fn drop(&mut self) {
        let ids: Vec<u64> = lock(&self.registry.dials).keys().copied().collect();
        for id in ids {
            self.registry.cancel(id);
        }
        lock(session_storage()).remove(&self.owner);
    }
}
