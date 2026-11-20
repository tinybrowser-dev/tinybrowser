//! Browser-loadable tinybrowser page engine component.
//!
//! The component runs the browser's own fetch core: the driver shapes one page
//! dial with [`dial`], [`net::Agent`] applies redirects, cookies, CORS,
//! `Sec-Fetch-*`, and the referrer, and the host transport moves the bytes one
//! exchange at a time through the host contract. The native browser runs the
//! same core over hyper, so the two hosts differ in who owns a socket, not in
//! what a page sees.
//!
//! The component has no executor. A dial's future is polled once when the
//! renderer asks for it, then again each time the host delivers the exchange
//! that future waits on — the same loop `engine.step` already drives.

mod driver;
mod host_transport;

use std::cell::RefCell;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::task::{Context, Poll, Waker};

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
    Event, FrameEvent, Guest, GuestTab, HistoryHandling as HostHistoryHandling,
};
use crate::bindings::tinybrowser::browser::host;
use crate::bindings::tinybrowser::browser::types::{FetchError, FetchResponse};
use crate::host_transport::HostTransport;

/// The component's fetch core and the port it moves bytes through.
///
/// One instance serves every tab: the agent owns the cookie jar, so every dial
/// must pass through the same transport for the jar to see one set of
/// responses.
struct Network {
    agent: Agent,
    transport: Arc<HostTransport>,
}

fn network() -> &'static Network {
    static NETWORK: OnceLock<Network> = OnceLock::new();
    NETWORK.get_or_init(|| {
        let transport = Arc::new(HostTransport::new(
            u64::try_from(renderer::MAX_RESPONSE_BODY_BYTES).unwrap_or(u64::MAX),
        ));
        // Every hop is bounded by the per-call cap; each dial's own deadline
        // travels on its request, so a page's `timeout_ms` can only tighten it.
        let agent = Agent::with_transport(
            AgentOptions {
                timeout_per_call: Some(dial::PAGE_FETCH_TIMEOUT),
                ..AgentOptions::default()
            },
            Arc::clone(&transport) as Arc<dyn Transport>,
        );
        Network { agent, transport }
    })
}

struct Component;

impl Guest for Component {
    type Tab = Tab;

    fn complete_fetch(id: u64, result: Result<FetchResponse, FetchError>) -> bool {
        let Some((registry, dial_id)) = lock(waiting_dials()).remove(&id) else {
            return false;
        };
        let Some(mut dial) = lock(&registry.dials).remove(&dial_id) else {
            return false;
        };
        if !network().transport.complete(id, result) {
            // The exchange is gone, so nothing will resume this future. Fail
            // the dial rather than park the tab with no one to wake it.
            if let Some(completion) = dial.completion.take() {
                completion(Err(DialFailure::Connect));
            }
            return false;
        }
        DialRegistry::poll(&registry, dial_id, dial);
        true
    }
}

struct Tab {
    owner: u64,
    renderer: RefCell<EmbeddedRenderer>,
}

impl GuestTab for Tab {
    fn new(url: String, html: String) -> Result<Self, String> {
        let services = Arc::new(WasmServices::new());
        let owner = services.owner;
        let mut renderer = EmbeddedRenderer::new(services, network().agent.identity().clone());
        let mount = Mount {
            head: renderer::ResponseHead::local(
                url::Url::parse(&url).map_err(|_| format!("invalid url: {url}"))?,
                Some("text/html; charset=utf-8".into()),
                None,
            ),
            tab: renderer::TabState::default(),
            body: html.into_bytes(),
        };
        renderer.mount(&mount).map_err(|error| error.to_string())?;
        Ok(Self {
            owner,
            renderer: RefCell::new(renderer),
        })
    }

    fn owner(&self) -> u64 {
        self.owner
    }

    fn eval(&self, source: String) -> Result<String, String> {
        self.with_renderer(|renderer| renderer.eval(&source))
    }

    fn step(&self) -> Option<u64> {
        let mut renderer = self.renderer.borrow_mut();
        // Fired timers sit in the task queue and are not a deadline. A yield
        // with `time_until_deadline() == None` would idle the host forever.
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
        self.renderer
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
            .map_err(|error| error.to_string())
    }

    fn title(&self) -> Result<String, String> {
        self.with_renderer(|renderer| renderer.eval("document.title"))
    }

    fn text(&self) -> Result<String, String> {
        self.with_renderer(|renderer| {
            renderer.eval("document.body === null ? '' : document.body.textContent")
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
}

/// The renderer's event vocabulary, restricted to what this component emits.
///
/// The embedded engine has no auxiliary browsing contexts and its host owns
/// the single tab, so the browser's joint-history bookkeeping events stay
/// inside the engine instead of crossing the component boundary.
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

/// The dials one tab has in flight.
struct DialRegistry {
    dials: Mutex<HashMap<u64, Dial>>,
}

/// One dial: its fetch future, and who to answer when it ends.
struct Dial {
    owner: u64,
    future: Pin<Box<dyn Future<Output = Result<DialOutcome, DialFailure>> + Send>>,
    /// Taken exactly once: by the poll that finishes the dial, or by a
    /// cancellation.
    completion: Option<DialCompletion>,
    /// The host exchange this dial waits on while it is parked.
    waiting_on: Option<u64>,
}

impl DialRegistry {
    fn new() -> Self {
        Self {
            dials: Mutex::new(HashMap::new()),
        }
    }

    /// Polls one dial once, then either completes it or parks it until the host
    /// answers the exchange that poll opened.
    fn poll(registry: &Arc<Self>, id: u64, mut dial: Dial) {
        let transport = &network().transport;
        let mut context = Context::from_waker(Waker::noop());
        transport.begin_poll(dial.owner);
        let polled = dial.future.as_mut().poll(&mut context);
        let opened = transport.end_poll();
        match polled {
            Poll::Ready(result) => {
                if let Some(completion) = dial.completion.take() {
                    completion(result);
                }
            }
            Poll::Pending => match opened {
                Some(host_id) => {
                    dial.waiting_on = Some(host_id);
                    lock(waiting_dials()).insert(host_id, (Arc::clone(registry), id));
                    lock(&registry.dials).insert(id, dial);
                }
                None => {
                    // The core is waiting on something no exchange will answer.
                    // Fail the dial instead of parking the tab forever.
                    if let Some(completion) = dial.completion.take() {
                        completion(Err(DialFailure::Connect));
                    }
                }
            },
        }
    }

    /// Fails one dial and stops the host work it started.
    fn cancel(&self, id: u64) {
        let Some(mut dial) = lock(&self.dials).remove(&id) else {
            return;
        };
        Self::stop(&mut dial);
        if let Some(completion) = dial.completion.take() {
            completion(Err(DialFailure::Cancelled));
        }
    }

    /// Stops the host exchange a parked dial waits on, if any.
    fn stop(dial: &mut Dial) {
        let Some(host_id) = dial.waiting_on.take() else {
            return;
        };
        lock(waiting_dials()).remove(&host_id);
        network().transport.abandon(host_id);
        host::cancel_fetch(host_id);
    }
}

/// Dials parked on a host exchange, keyed by the exchange the host must answer.
///
/// Every entry is removed by the completion, the cancellation, or the tab
/// teardown that ends it, so the map only ever holds work the host still owes.
/// The tab and dial an exchange resumes when the host answers it.
type Parked = (Arc<DialRegistry>, u64);

fn waiting_dials() -> &'static Mutex<HashMap<u64, Parked>> {
    static WAITING: OnceLock<Mutex<HashMap<u64, Parked>>> = OnceLock::new();
    WAITING.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The component's local storage areas, keyed by origin. Entries keep
/// insertion order: the storage proxy exposes enumeration and `key(index)`
/// directly, and a `HashMap` would make both unstable.
/// A browser tab has no profile on disk, so the areas die with the component.
fn local_storage() -> &'static Mutex<HashMap<renderer::StorageKey, StorageArea>> {
    static AREAS: OnceLock<Mutex<HashMap<renderer::StorageKey, StorageArea>>> = OnceLock::new();
    AREAS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// `indexedDB` rows shared by every realm of this wasm instance.
fn indexed_db() -> &'static Mutex<HashMap<renderer::StorageKey, StorageArea>> {
    static AREAS: OnceLock<Mutex<HashMap<renderer::StorageKey, StorageArea>>> = OnceLock::new();
    AREAS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Session storage namespaces keyed by component tab owner.
fn session_storage() -> &'static Mutex<HashMap<u64, HashMap<renderer::StorageKey, StorageArea>>> {
    static NAMESPACES: OnceLock<Mutex<HashMap<u64, HashMap<renderer::StorageKey, StorageArea>>>> =
        OnceLock::new();
    NAMESPACES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Locks a shared map or registry, recovering from a panicking holder.
///
/// A poisoned lock means some earlier operation panicked. These maps hold no
/// cross-entry invariant a panic can break, so the next caller can continue
/// with the state that was committed.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

static NEXT_OWNER: AtomicU64 = AtomicU64::new(1);
static NEXT_DIAL: AtomicU64 = AtomicU64::new(1);

struct WasmServices {
    owner: u64,
    registry: Arc<DialRegistry>,
}

impl WasmServices {
    fn new() -> Self {
        Self {
            owner: NEXT_OWNER.fetch_add(1, Ordering::Relaxed),
            registry: Arc::new(DialRegistry::new()),
        }
    }
}

impl NetworkHost for WasmServices {
    fn start_dial(&self, request: DialRequest, completion: DialCompletion) -> DialCancellation {
        // An opaque origin has no URL to fetch under and no initiator to name.
        let Some(initiator) = request.initiator.to_url().map(|url| url.origin()) else {
            completion(Err(DialFailure::Connect));
            return Box::new(|| {});
        };
        let id = NEXT_DIAL.fetch_add(1, Ordering::Relaxed);
        let dial = Dial {
            owner: self.owner,
            future: Box::pin(driver::run(network().agent.clone(), request, initiator)),
            completion: Some(completion),
            waiting_on: None,
        };
        DialRegistry::poll(&self.registry, id, dial);
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
        let _stored = network()
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
        let owed: Vec<u64> = {
            let mut waiting = lock(waiting_dials());
            let ids: Vec<u64> = waiting
                .iter()
                .filter(|(_, (registry, _))| Arc::ptr_eq(registry, &self.registry))
                .map(|(id, _)| *id)
                .collect();
            for id in &ids {
                waiting.remove(id);
            }
            ids
        };
        for id in owed {
            network().transport.abandon(id);
            host::cancel_fetch(id);
        }
        let dials = std::mem::take(&mut *lock(&self.registry.dials));
        for (_, mut dial) in dials {
            if let Some(completion) = dial.completion.take() {
                completion(Err(DialFailure::Cancelled));
            }
        }
        lock(session_storage()).remove(&self.owner);
    }
}
