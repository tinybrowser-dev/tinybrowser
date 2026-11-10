//! Browser-loadable tinybrowser page engine component.
//!
//! This is the browser side of tinybrowser, compiled as a WebAssembly
//! component. It owns pages, cookies, and the redirect policy of a dial; the
//! host owns the network transport and performs exactly one HTTP exchange per
//! `host.start-fetch` call.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::SystemTime;

use cookies::{
    CookieJar, CookieOp, InitiatorKind, RetrievalKind, SameSiteCookiePolicy, schemeful_same_site,
};
use renderer::{
    BrowserEvent, BrowsingContextHost, DialCompletion, DialFailure, DialKind, DialOutcome,
    DialRequest, EmbeddedRenderer, HistoryHandling, MAX_RESPONSE_BODY_BYTES, MessagingHost, Mount,
    NetworkHost, RendererEvent, StorageChange, StorageHost, StorageKind,
};
use url::Url;
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

use bindings::exports::tinybrowser::browser::engine::{
    Event, FrameEvent, Guest, GuestTab, HistoryHandling as HostHistoryHandling,
};
use bindings::tinybrowser::browser::types::{
    FetchCredentials, FetchError, FetchKind, FetchMode, FetchRedirect, FetchRequest, FetchResponse,
};

/// Redirect hops a single dial may follow.
///
/// Matches the native transport's cap; see
/// <https://fetch.spec.whatwg.org/#http-redirect-fetch>.
const MAX_REDIRECTS: u32 = 20;

/// Statuses the redirect algorithm follows.
///
/// <https://fetch.spec.whatwg.org/#http-redirect-fetch>
const REDIRECT_STATUSES: [u16; 5] = [301, 302, 303, 307, 308];

struct Component;

impl Guest for Component {
    type Tab = Tab;

    fn complete_fetch(id: u64, result: Result<FetchResponse, FetchError>) -> bool {
        let Some(pending) = lock(pending_fetches()).remove(&id) else {
            return false;
        };
        let PendingFetch {
            owner,
            mut chain,
            completion,
        } = pending;
        let Some(completion) = completion else {
            return false;
        };
        match result {
            Err(error) => completion(Err(decode_fetch_error(error))),
            Ok(response) => {
                // A host that cannot see redirects answers with the exchange
                // it ended on, so attribute this response to the URL it
                // reports. Storing it against the requested hop would file one
                // site's cookies under another's name. Store before following:
                // the next hop's header depends on this response.
                let source = reported_url(&chain, &response);
                chain.cross_site_redirect |= !schemeful_same_site(&chain.url, &source);
                store_hop_cookies(&source, &chain, &response);
                // From here the dial stands where this response came from: a
                // relative `Location` resolves against that, not against the
                // hop that was requested before the host moved us.
                chain.url = source;
                match next_hop(&chain, &response) {
                    Ok(Some(next)) => {
                        let redirects = chain.followed.saturating_add(1);
                        if let Some(csp) = &chain.csp {
                            let document = chain.initiator.clone().unwrap_or_else(|| next.clone());
                            if !csp.allows(&document, &next, redirects) {
                                // The renderer reports `final_url` and drops the body.
                                completion(Ok(csp_blocked(&next)));
                                return true;
                            }
                        }
                        chain.followed = chain.followed.saturating_add(1);
                        chain.cross_site_redirect |= !schemeful_same_site(&chain.url, &next);
                        chain.url = next;
                        start_hop(owner, chain, completion);
                    }
                    Ok(None) => {
                        completion(decode_fetch_result(response, &chain.url));
                    }
                    Err(failure) => completion(Err(failure)),
                }
            }
        }
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
        // The component is its own host; a host-supplied identity arrives with
        // the `fetch-request` contract change in #192.
        let mut renderer = EmbeddedRenderer::new(services, net::ValidIdentity::default());
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

/// One dial's state, kept across its redirect hops.
struct Chain {
    dial: u64,
    /// URL of the hop in flight.
    url: Url,
    /// Document URL the dial started from, for `SameSite` and the
    /// credentials-mode origin comparison.
    initiator: Option<Url>,
    kind: DialKind,
    /// The request's mode, credentials mode, and redirect mode. The host
    /// applies them, and the component keeps them because the credentials
    /// mode also decides whether a cookie header is computed at all.
    cors: net::cors::CorsParameters,
    /// Hops already followed.
    followed: u32,
    /// Whether any hop so far moved to another site.
    cross_site_redirect: bool,
    /// Policy checked before each redirect is followed.
    csp: Option<renderer::RedirectGate>,
    /// The cookie partition this dial belongs to, from the regular realm's
    /// top-level site.
    partition_key: Option<String>,
}

struct PendingFetch {
    owner: u64,
    chain: Chain,
    /// Taken exactly once: by the last hop's completion, or by tab teardown.
    completion: Option<DialCompletion>,
}

/// Every exchange in flight, across all tabs of this component instance.
///
/// Entries appear only for dials the renderer asked for, and the renderer caps
/// concurrent dials per document and frames per engine, so a host that honors
/// its completion obligation cannot make this grow without bound.
fn pending_fetches() -> &'static Mutex<HashMap<u64, PendingFetch>> {
    static PENDING: OnceLock<Mutex<HashMap<u64, PendingFetch>>> = OnceLock::new();
    PENDING.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Cookie state shared by every tab in one component instance.
fn cookie_jar() -> &'static Mutex<CookieJar> {
    static JAR: OnceLock<Mutex<CookieJar>> = OnceLock::new();
    JAR.get_or_init(|| Mutex::new(CookieJar::default()))
}

/// The component's local storage areas, keyed by origin. Entries keep
/// insertion order: the storage proxy exposes enumeration and `key(index)`
/// directly, and a `HashMap` would make both unstable.
/// A browser tab has no profile on disk, so the areas die with the component.
fn local_storage() -> &'static Mutex<HashMap<renderer::StorageKey, StorageArea>> {
    static AREAS: OnceLock<Mutex<HashMap<renderer::StorageKey, StorageArea>>> = OnceLock::new();
    AREAS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Session storage namespaces keyed by component tab owner.
fn session_storage() -> &'static Mutex<HashMap<u64, HashMap<renderer::StorageKey, StorageArea>>> {
    static NAMESPACES: OnceLock<Mutex<HashMap<u64, HashMap<renderer::StorageKey, StorageArea>>>> =
        OnceLock::new();
    NAMESPACES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Locks a shared map or jar, recovering from a panicking holder.
///
/// A poisoned lock means some earlier operation panicked. The jar and the
/// pending map hold no cross-entry invariant a panic can break, so the next
/// caller can continue with the state that was committed.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Sends one hop to the host and records it until its completion arrives.
///
/// HTTP is GET-only here, so a redirect never changes the method or drops a
/// body.
fn start_hop(owner: u64, chain: Chain, completion: DialCompletion) {
    let id = NEXT_FETCH.fetch_add(1, Ordering::Relaxed);
    let request = FetchRequest {
        id,
        owner,
        kind: match chain.kind {
            DialKind::JsFetch => FetchKind::JsFetch,
            DialKind::ClassicScript => FetchKind::ClassicScript,
            DialKind::ModuleScript => FetchKind::ModuleScript,
            DialKind::Stylesheet => FetchKind::Stylesheet,
            DialKind::Image => FetchKind::Image,
            DialKind::Font => FetchKind::Font,
            DialKind::FrameLoad => FetchKind::FrameLoad,
            // Unreachable: `start_dial` completes reports without a hop.
            DialKind::CspReport => unreachable!("report-uri has no transport hop"),
        },
        url: chain.url.to_string(),
        initiator: chain.initiator.as_ref().map_or_default(Url::to_string),
        mode: host_mode(chain.cors.mode),
        credentials: host_credentials(chain.cors.credentials),
        redirect: host_redirect(chain.cors.redirect),
        max_body_bytes: u64::try_from(MAX_RESPONSE_BODY_BYTES).unwrap_or(u64::MAX),
        cookie: cookie_header(&chain),
    };
    lock(pending_fetches()).insert(
        id,
        PendingFetch {
            owner,
            chain,
            completion: Some(completion),
        },
    );
    if !bindings::tinybrowser::browser::host::start_fetch(&request) {
        let pending = lock(pending_fetches()).remove(&id);
        if let Some(completion) = pending.and_then(|pending| pending.completion) {
            completion(Err(DialFailure::Connect));
        }
    }
}

/// The host-visible name for a request mode.
fn host_mode(mode: net::cors::RequestMode) -> FetchMode {
    match mode {
        net::cors::RequestMode::Cors => FetchMode::Cors,
        net::cors::RequestMode::NoCors => FetchMode::NoCors,
        net::cors::RequestMode::SameOrigin => FetchMode::SameOrigin,
        net::cors::RequestMode::Navigate => FetchMode::Navigate,
    }
}

/// The host-visible name for a credentials mode.
fn host_credentials(credentials: net::cors::CredentialsMode) -> FetchCredentials {
    match credentials {
        net::cors::CredentialsMode::Omit => FetchCredentials::Omit,
        net::cors::CredentialsMode::SameOrigin => FetchCredentials::SameOrigin,
        net::cors::CredentialsMode::Include => FetchCredentials::Include,
    }
}

/// The host-visible name for a redirect mode.
fn host_redirect(redirect: net::cors::RedirectMode) -> FetchRedirect {
    match redirect {
        net::cors::RedirectMode::Follow => FetchRedirect::Follow,
        net::cors::RedirectMode::Error => FetchRedirect::Error,
        net::cors::RedirectMode::Manual => FetchRedirect::Manual,
    }
}

/// The `Cookie` header for the hop in flight.
fn cookie_header(chain: &Chain) -> String {
    let initiator = chain.initiator.as_ref().map(Url::origin);
    let cross_origin = initiator
        .as_ref()
        .is_some_and(|initiator| *initiator != chain.url.origin());
    if !net::cors::credentials_admitted(chain.cors.credentials, cross_origin) {
        return String::new();
    }
    lock(cookie_jar()).cookie_string(CookieOp {
        url: &chain.url,
        now: SystemTime::now(),
        kind: RetrievalKind::Http,
        initiator_kind: InitiatorKind::Fetch,
        method_is_safe: true,
        initiator: initiator.as_ref(),
        cross_site_redirect: chain.cross_site_redirect,
        same_site_policy: SameSiteCookiePolicy::All,
        partition_key: chain.partition_key.as_deref(),
    })
}

/// Where a response came from, as far as the host can say.
///
/// The hop URL unless the host reports a different final URL, which is how a
/// host that followed redirects itself tells us the bytes are not from the
/// hop we asked for.
fn reported_url(chain: &Chain, response: &FetchResponse) -> Url {
    Url::parse(&response.final_url)
        .ok()
        .filter(|url| matches!(url.scheme(), "http" | "https"))
        .unwrap_or_else(|| chain.url.clone())
}

/// Stores one exchange's `Set-Cookie` lines against the URL that sent them.
///
/// <https://httpwg.org/http-extensions/draft-ietf-httpbis-rfc6265bis.html#name-the-set-cookie-header-field>
fn store_hop_cookies(url: &Url, chain: &Chain, response: &FetchResponse) {
    if response.set_cookies.is_empty() {
        return;
    }
    let now = SystemTime::now();
    let initiator = chain.initiator.as_ref().map(Url::origin);
    let mut jar = lock(cookie_jar());
    for line in &response.set_cookies {
        jar.store(
            line,
            CookieOp {
                url,
                now,
                kind: RetrievalKind::Http,
                initiator_kind: InitiatorKind::Fetch,
                method_is_safe: true,
                initiator: initiator.as_ref(),
                cross_site_redirect: chain.cross_site_redirect,
                same_site_policy: SameSiteCookiePolicy::All,
                partition_key: chain.partition_key.as_deref(),
            },
        );
    }
}

/// The redirect this response asks for, if the dial may follow it.
///
/// Mirrors the native transport's `followable_location`, redirect cap, and
/// `resolve_location`; the fragment carry-over is
/// <https://fetch.spec.whatwg.org/#http-redirect-fetch>.
fn next_hop(chain: &Chain, response: &FetchResponse) -> Result<Option<Url>, DialFailure> {
    if !REDIRECT_STATUSES.contains(&response.status) {
        return Ok(None);
    }
    let Some(location) = response.location.as_deref() else {
        return Ok(None);
    };
    if chain.followed >= MAX_REDIRECTS {
        return Err(DialFailure::Limit);
    }
    let mut next = chain.url.join(location).map_err(|_| DialFailure::Connect)?;
    if !matches!(next.scheme(), "http" | "https") {
        return Err(DialFailure::Connect);
    }
    if next.fragment().is_none()
        && let Some(fragment) = chain.url.fragment()
    {
        next.set_fragment(Some(fragment));
    }
    Ok(Some(next))
}

/// A response the WASM carrier builds without the fetch core.
///
/// The component still moves its own bytes until #192 puts it on the
/// [`net::Transport`] port, so the outcome is spelled out here instead of a
/// `DialOutcome` constructor a real fetch core would have decided.
fn basic_outcome(
    status: u16,
    final_url: Url,
    content_type: Option<String>,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
) -> DialOutcome {
    DialOutcome {
        status,
        status_text: String::new(),
        final_url,
        content_type,
        headers,
        body: renderer::ResponseBody::Bytes(body),
        response_type: net::cors::ResponseType::Basic,
        crossed_origin: false,
        redirect_count: 0,
        referrer: None,
    }
}

/// A redirect the policy refused. Status 0 is not a response the page can
/// use; `final_url` is the blocked hop so the renderer can report it.
fn csp_blocked(url: &Url) -> DialOutcome {
    basic_outcome(0, url.clone(), None, Vec::new(), Vec::new())
}

fn decode_fetch_result(response: FetchResponse, hop_url: &Url) -> Result<DialOutcome, DialFailure> {
    if response.body.len() > MAX_RESPONSE_BODY_BYTES {
        return Err(DialFailure::Limit);
    }
    // A host that cannot see redirects reports the browser's final URL here.
    // Fall back to the hop URL rather than failing a dial over metadata.
    let final_url = Url::parse(&response.final_url)
        .ok()
        .filter(|url| matches!(url.scheme(), "http" | "https"))
        .unwrap_or_else(|| hop_url.clone());
    // This transport answers with the whole body, so a page that asked for a
    // progressive body gets it buffered. The renderer accepts a body in place
    // of a stream, and the page sees a response that was not streamed.
    let mut outcome = basic_outcome(
        response.status,
        final_url,
        response.content_type,
        Vec::new(),
        response.body,
    );
    // No host header list crosses this transport, so the response has none.
    outcome.headers = Vec::new();
    Ok(outcome)
}

fn decode_fetch_error(error: FetchError) -> DialFailure {
    match error {
        FetchError::Dns => DialFailure::Dns,
        FetchError::Connect => DialFailure::Connect,
        FetchError::Tls => DialFailure::Tls,
        FetchError::Timeout => DialFailure::Timeout,
        FetchError::Limit => DialFailure::Limit,
        FetchError::QueueFull => DialFailure::QueueFull,
        FetchError::Cancelled => DialFailure::Cancelled,
    }
}

static NEXT_OWNER: AtomicU64 = AtomicU64::new(1);
static NEXT_FETCH: AtomicU64 = AtomicU64::new(1);

struct WasmServices {
    owner: u64,
}

impl WasmServices {
    fn new() -> Self {
        Self {
            owner: NEXT_OWNER.fetch_add(1, Ordering::Relaxed),
        }
    }
}

impl NetworkHost for WasmServices {
    fn start_dial(
        &self,
        request: DialRequest,
        completion: DialCompletion,
    ) -> renderer::DialCancellation {
        // A dial the component cannot shape is not a transport question, so it
        // never reaches the host.
        // `report-uri` POSTs have no body or method on this GET-only
        // transport (`fetch-request` carries neither), so the report is
        // dropped here instead of sent as a bogus GET. The native carrier
        // sends the real POST; the renderer already fired the event.
        if request.policy.kind() == DialKind::CspReport {
            completion(Ok(basic_outcome(
                204,
                request.url,
                None,
                Vec::new(),
                Vec::new(),
            )));
            return Box::new(|| {});
        }
        let shaped = matches!(request.url.scheme(), "http" | "https").then_some(request.url);
        let Some(url) = shaped else {
            completion(Err(DialFailure::Connect));
            return Box::new(|| {});
        };
        let dial = NEXT_FETCH.fetch_add(1, Ordering::Relaxed);
        start_hop(
            self.owner,
            Chain {
                dial,
                url,
                initiator: request.initiator.to_url(),
                kind: request.policy.kind(),
                cors: request.policy.cors(),
                followed: 0,
                cross_site_redirect: false,
                csp: request.csp.as_deref().cloned(),
                partition_key: request.partition_key,
            },
            completion,
        );
        Box::new(move || {
            let pending = {
                let mut fetches = lock(pending_fetches());
                let id = fetches
                    .iter()
                    .find_map(|(&id, fetch)| (fetch.chain.dial == dial).then_some(id));
                id.and_then(|id| fetches.remove(&id).map(|fetch| (id, fetch)))
            };
            if let Some((id, fetch)) = pending {
                bindings::tinybrowser::browser::host::cancel_fetch(id);
                if let Some(completion) = fetch.completion {
                    completion(Err(DialFailure::Cancelled));
                }
            }
        })
    }

    fn cookies_for(&self, url: &Url, partition_key: Option<&str>) -> String {
        let origin = url.origin();
        lock(cookie_jar()).cookie_string(CookieOp {
            url,
            now: SystemTime::now(),
            kind: RetrievalKind::NonHttp,
            initiator_kind: InitiatorKind::Fetch,
            method_is_safe: true,
            initiator: Some(&origin),
            cross_site_redirect: false,
            same_site_policy: SameSiteCookiePolicy::All,
            partition_key,
        })
    }

    fn set_cookie(&self, value: &str, url: &Url, partition_key: Option<&str>) {
        let origin = url.origin();
        lock(cookie_jar()).store(
            value,
            CookieOp {
                url,
                now: SystemTime::now(),
                kind: RetrievalKind::NonHttp,
                initiator_kind: InitiatorKind::Fetch,
                method_is_safe: true,
                initiator: Some(&origin),
                cross_site_redirect: false,
                same_site_policy: SameSiteCookiePolicy::All,
                partition_key,
            },
        );
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
        }
    }
}

impl BrowsingContextHost for WasmServices {
    fn window_open(
        &self,
        _url: Option<&Url>,
        _target: &renderer::TargetName,
        _opener: renderer::OpenerPolicy,
        _source: &renderer::NavigationSource,
    ) -> Option<renderer::WindowId> {
        None
    }

    fn navigate_named(
        &self,
        _name: &renderer::ContextName,
        _url: Option<&Url>,
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
        let mut cancelled = Vec::new();
        lock(pending_fetches()).retain(|id, fetch| {
            if fetch.owner != self.owner {
                return true;
            }
            if let Some(completion) = fetch.completion.take() {
                cancelled.push((*id, completion));
            }
            false
        });
        for (id, completion) in cancelled {
            bindings::tinybrowser::browser::host::cancel_fetch(id);
            completion(Err(DialFailure::Cancelled));
        }
        lock(session_storage()).remove(&self.owner);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_host_contract_names_every_fetch_policy() {
        use net::cors::{CredentialsMode, RedirectMode, RequestMode};
        for (mode, expected) in [
            (RequestMode::Cors, FetchMode::Cors),
            (RequestMode::NoCors, FetchMode::NoCors),
            (RequestMode::SameOrigin, FetchMode::SameOrigin),
            (RequestMode::Navigate, FetchMode::Navigate),
        ] {
            assert_eq!(host_mode(mode), expected);
        }
        for (credentials, expected) in [
            (CredentialsMode::Omit, FetchCredentials::Omit),
            (CredentialsMode::SameOrigin, FetchCredentials::SameOrigin),
            (CredentialsMode::Include, FetchCredentials::Include),
        ] {
            assert_eq!(host_credentials(credentials), expected);
        }
        for (redirect, expected) in [
            (RedirectMode::Follow, FetchRedirect::Follow),
            (RedirectMode::Error, FetchRedirect::Error),
            (RedirectMode::Manual, FetchRedirect::Manual),
        ] {
            assert_eq!(host_redirect(redirect), expected);
        }
    }

    #[test]
    fn host_failures_keep_their_kind() {
        for (error, expected) in [
            (FetchError::Dns, DialFailure::Dns),
            (FetchError::Connect, DialFailure::Connect),
            (FetchError::Tls, DialFailure::Tls),
            (FetchError::Timeout, DialFailure::Timeout),
            (FetchError::Limit, DialFailure::Limit),
            (FetchError::QueueFull, DialFailure::QueueFull),
            (FetchError::Cancelled, DialFailure::Cancelled),
        ] {
            assert_eq!(decode_fetch_error(error), expected);
        }
    }

    #[test]
    fn a_response_without_a_usable_final_url_keeps_the_hop_url() {
        let hop = Url::parse("https://example.test/docs").expect("valid url");
        let response = FetchResponse {
            status: 200,
            final_url: String::new(),
            content_type: None,
            set_cookies: Vec::new(),
            location: None,
            body: b"hi".to_vec(),
        };
        let outcome = decode_fetch_result(response, &hop).expect("decoded");
        assert!(matches!(&outcome.body, renderer::ResponseBody::Bytes(body) if body == b"hi"));
        assert_eq!(&outcome.final_url, &hop);
    }
}
