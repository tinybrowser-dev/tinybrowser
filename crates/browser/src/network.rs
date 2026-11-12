//! Browser-owned live networking: one [`net::Agent`] behind a value-only handle.
//!
//! Tab coordinators receive [`TabNetworkHandle`]. They do not expose or own
//! [`net::Agent`].

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dial::{CacheDecision, HttpCache, Validators, refused_redirect, watch_redirects};
use net::{Agent, AgentOptions, InitiatorKind, Method, Request};
use renderer::{
    BodyDelivery, DialFailure, DialKind, DialOutcome, DialRequest, ResponseBody, StreamId,
};
use tokio::sync::Semaphore;
use tokio::sync::mpsc::UnboundedSender;
use url::Url;

use crate::actor::NavigationInitiator;
use crate::wire::RendererAssignmentId;

/// Default per-call fetch timeout on a [`TabNetworkHandle`].
pub(crate) const PAGE_FETCH_TIMEOUT: Duration = Duration::from_secs(30);

/// In-flight keepalive request bytes across every realm in this process
/// (<https://fetch.spec.whatwg.org/#keepalive-flag>).
const KEEPALIVE_QUOTA: usize = 65_536;
static KEEPALIVE_IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);

struct KeepaliveReservation(usize);

impl KeepaliveReservation {
    fn acquire(request: &DialRequest) -> Result<Option<Self>, DialFailure> {
        if !request.keepalive {
            return Ok(None);
        }
        let Ok(length) = usize::try_from(request.body_len) else {
            return Err(DialFailure::Limit);
        };
        if length > KEEPALIVE_QUOTA {
            return Err(DialFailure::Limit);
        }
        let mut current = KEEPALIVE_IN_FLIGHT.load(Ordering::Relaxed);
        loop {
            let Some(next) = current.checked_add(length) else {
                return Err(DialFailure::Limit);
            };
            if next > KEEPALIVE_QUOTA {
                return Err(DialFailure::Limit);
            }
            match KEEPALIVE_IN_FLIGHT.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Ok(Some(Self(length))),
                Err(observed) => current = observed,
            }
        }
    }
}

impl Drop for KeepaliveReservation {
    fn drop(&mut self) {
        KEEPALIVE_IN_FLIGHT.fetch_sub(self.0, Ordering::AcqRel);
    }
}

/// The answer from one dial once the response head is in.
enum DialProgress {
    /// The dial has its answer: a refused redirect or a 304 merged into the
    /// stored entry.
    Done(Box<DialOutcome>),
    /// The head is in and the body is still on the wire.
    Response {
        response: Box<net::Response>,
        permits: DialPermits,
    },
}

/// Everything a dial outcome reports except its body lives in the shared
/// [`dial`] adapter, which the WPT harness also runs.
pub(crate) use dial::dial_failure;
use dial::{DialHead, outbound_request};

/// The browser-wide and per-tab permits one dial holds until its body is done.
struct DialPermits {
    _global: tokio::sync::OwnedSemaphorePermit,
    _tab: tokio::sync::OwnedSemaphorePermit,
}

/// The bytes a buffered outcome holds, for the cache tests.
#[cfg(test)]
fn stored_bytes(outcome: &DialOutcome) -> usize {
    match &outcome.body {
        ResponseBody::Bytes(bytes) => bytes.len(),
        ResponseBody::Undecodable | ResponseBody::Streamed(_) => 0,
    }
}

enum StreamMessage {
    Chunk(Vec<u8>),
    End,
    Error(DialFailure),
}

/// Chunks a body task may buffer ahead of the renderer's next read.
const STREAM_CHUNK_DEPTH: usize = 16;

struct StreamEntry {
    owner: RendererAssignmentId,
    rx: Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<StreamMessage>>>,
    task: tokio::task::JoinHandle<()>,
}

type StreamMap = Arc<Mutex<HashMap<StreamId, StreamEntry>>>;

/// Aborts the body task of a stream removed from the table, so the task stops
/// reading and releases its dial permits.
fn abort_stream(entry: Option<StreamEntry>) {
    if let Some(entry) = entry {
        entry.task.abort();
    }
}

/// One completed navigation dial: the response head the tab mounts plus the
/// streaming body.
pub(crate) struct NavOutcome {
    pub head: renderer::ResponseHead,
    pub body: net::Body,
    _permit: tokio::sync::OwnedSemaphorePermit,
}

/// Live network state for one storage partition.
pub(crate) struct NetworkContext {
    agent: Agent,
    permits: NetworkPermits,
    /// The HTTP cache stamps and reads entries with the clock an embedder
    /// injects through [`AgentOptions`], so freshness agrees with it. Dial
    /// deadlines stay on the reactor clock, which is what drives tokio timers.
    cache: Arc<HttpCache>,
    streams: StreamMap,
    next_stream: Arc<AtomicU64>,
}

impl NetworkContext {
    /// Builds the partition network from caller `options`. The only default is
    /// `timeout_per_call`: the identity comes from `options`, whose own default
    /// is the built-in one.
    pub(crate) fn new(mut options: AgentOptions) -> Self {
        if options.timeout_per_call.is_none() {
            options.timeout_per_call = Some(PAGE_FETCH_TIMEOUT);
        }
        let clock = options.clock.clone();
        Self {
            agent: Agent::new(options),
            cache: Arc::new(HttpCache::new(clock)),
            permits: NetworkPermits::new(),
            streams: Arc::new(Mutex::new(HashMap::new())),
            next_stream: Arc::new(AtomicU64::new(1)),
        }
    }

    /// Network capability for one tab, with its own per-tab cap.
    #[must_use]
    pub(crate) fn tab_handle(&self) -> TabNetworkHandle {
        TabNetworkHandle {
            agent: self.agent.clone(),
            permits: self.permits.clone(),
            tab: Arc::new(Semaphore::new(MAX_TAB_DIALS)),
            cache: Arc::clone(&self.cache),
            streams: Arc::clone(&self.streams),
            next_stream: Arc::clone(&self.next_stream),
            sheets: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub(crate) fn agent(&self) -> Agent {
        self.agent.clone()
    }

    /// The identity this partition was built with, for the renderers it
    /// spawns.
    pub(crate) fn identity(&self) -> net::ValidIdentity {
        self.agent.identity().clone()
    }

    /// Cookies visible to `url`, including session and `HttpOnly` cookies.
    pub(crate) fn cookie_records(&self, url: &Url) -> Vec<net::CookieRecord> {
        self.agent.cookie_records(url)
    }

    /// Drops every cookie from the live jar.
    pub(crate) fn clear_cookies(&self) {
        self.agent.clear_cookies();
    }

    /// Stores one `Set-Cookie` line for `url` with HTTP-level rules,
    /// returning whether it was stored.
    pub(crate) fn add_cookie(&self, cookie: &str, url: &Url) -> bool {
        self.agent.store_cookie_http(cookie, url)
    }
}

/// Limits for in-flight dials: browser-wide and per reserved class.
#[derive(Clone)]
struct NetworkPermits {
    global: Arc<Semaphore>,
    navigation: Arc<Semaphore>,
}

impl NetworkPermits {
    fn new() -> Self {
        Self {
            global: Arc::new(Semaphore::new(MAX_GLOBAL_DIALS)),
            navigation: Arc::new(Semaphore::new(MAX_NAVIGATION_DIALS)),
        }
    }
}

const MAX_GLOBAL_DIALS: usize = 32;
const MAX_NAVIGATION_DIALS: usize = 8;
const MAX_TAB_DIALS: usize = 8;

/// Cloneable network capability for one tab.
///
/// Completions return to the tab coordinator as renderer events.
#[derive(Clone)]
pub(crate) struct TabNetworkHandle {
    agent: Agent,
    permits: NetworkPermits,
    tab: Arc<Semaphore>,
    cache: Arc<HttpCache>,
    streams: StreamMap,
    next_stream: Arc<AtomicU64>,
    /// Style sheet URLs this tab's network served, keyed by the origin of the
    /// document that asked for them: final URL and request URL, fragments
    /// stripped. A request a sheet initiates sends the sheet's URL as its
    /// referrer, which is cross-origin to both the document and the target, so
    /// the browser authorizes an explicit referrer against the sheets *that
    /// document* loaded instead of against the document's own origin. Keying
    /// by origin is what stops document B from naming a sheet document A
    /// loaded, which would leak another principal's cross-origin URLs and the
    /// referrer policy they came with
    /// (<https://w3c.github.io/webappsec-referrer-policy/#integration-with-css>).
    sheets: Arc<Mutex<HashMap<url::Origin, HashSet<Url>>>>,
}

impl TabNetworkHandle {
    /// `document.cookie` getter for `url` in one cookie partition.
    #[must_use]
    pub(crate) fn cookies_for(&self, url: &Url, partition_key: Option<&str>) -> String {
        self.agent.cookies_for_partition(url, partition_key)
    }

    /// `document.cookie` setter for `url` in one cookie partition.
    pub(crate) fn set_cookie(&self, value: &str, url: &Url, partition_key: Option<&str>) {
        let _stored = self.agent.set_cookie_partition(value, url, partition_key);
    }

    /// Script-visible cookies for a `CookieStore` query
    /// (<https://cookiestore.spec.whatwg.org/#query-cookies>).
    #[must_use]
    pub(crate) fn cookie_store_records(
        &self,
        url: &Url,
        partition_key: Option<&str>,
    ) -> Vec<renderer::ScriptCookie> {
        self.agent
            .script_cookie_records_partition(url, partition_key)
            .into_iter()
            .map(|record| renderer::ScriptCookie::from_record(&record))
            .collect()
    }

    /// Stores one `CookieStore` line and reports whether the jar accepted it
    /// (<https://cookiestore.spec.whatwg.org/#set-a-cookie>).
    pub(crate) fn cookie_store_store(
        &self,
        write: &net::ScriptCookieWrite,
        url: &Url,
        partition_key: Option<&str>,
    ) {
        let _stored = self
            .agent
            .set_script_cookie_partition(write, url, partition_key);
    }

    /// Spawns one navigation dial and returns its task handle. The caller
    /// aborts the handle to cancel the request. The completion returns on
    /// `reply` tagged with `epoch`.
    ///
    /// `initiator` is the document that started it, when one did. A
    /// browser-issued navigation passes `None`: it sends no referrer,
    /// `Sec-Fetch-Site: none`, and same-site cookies.
    pub(crate) fn dial_navigation(
        &self,
        epoch: u64,
        url: Url,
        initiator: Option<NavigationInitiator>,
        reply: UnboundedSender<(u64, Result<NavOutcome, DialFailure>)>,
        mut cancel: tokio::sync::watch::Receiver<bool>,
    ) -> tokio::task::JoinHandle<()> {
        let network = self.clone();
        tokio::spawn(async move {
            let result = tokio::select! {
                biased;
                _ = cancel.changed() => Err(DialFailure::Cancelled),
                result = network.navigate(&url, initiator.as_ref()) => result,
            };
            let _send_result = reply.send((epoch, result));
        })
    }

    async fn navigate(
        &self,
        url: &Url,
        source: Option<&NavigationInitiator>,
    ) -> Result<NavOutcome, DialFailure> {
        // Browser-seam timeline for `profile` in tools/wpt: a stalled test is
        // read from these records. The macro tests the level before it formats
        // anything, so an off level costs one check and the clock read below.
        let started = Instant::now();
        logging::trace!(target: "browser::net", "nav start {url}");
        let deadline = started + PAGE_FETCH_TIMEOUT;
        let permits = Self::acquire(&self.permits.navigation, deadline).await?;
        let mut request = Request::new(Method::GET, url.clone());
        chrome_navigation_request(&mut request, url);
        request.initiator_kind = InitiatorKind::Navigation;
        // One initiator origin feeds `Sec-Fetch-Site`, the cookie context, and
        // the request origin. `None` means a fresh opaque origin: no document
        // speaks for this navigation and none of the three may pretend one
        // does (<https://w3c.github.io/webappsec-fetch-metadata/#abstract-opdef-set-site>).
        request.initiator = source.map(|source| source.origin.clone());
        request.cors = net::cors::CorsSettings {
            mode: net::cors::RequestMode::Navigate,
            credentials: net::cors::CredentialsMode::Include,
            redirect: net::cors::RedirectMode::Follow,
            origin: source.map_or_else(opaque_origin, |source| source.origin.clone()),
        };
        request.deadline = Some(deadline);
        if let Some(source) = source {
            // The referrer source is the calling document's URL, not the
            // request origin: a `srcdoc` document's own URL cannot be a
            // referrer, so the renderer already walked to its parent's
            // (<https://w3c.github.io/webappsec-referrer-policy/#determine-requests-referrer>).
            request.referrer = match &source.referrer {
                Some(referrer) => net::Referrer::Client(net::ReferrerClient {
                    origin: source.origin.clone(),
                    url: referrer.clone(),
                }),
                None => net::Referrer::NoReferrer,
            };
            request.referrer_policy = source.referrer_policy;
        }
        let response = match self.agent.send(request).await {
            Ok(response) => response,
            Err(error) => {
                logging::trace!(
                    target: "browser::net",
                    "nav failed {url} in {:?}: {error:?}",
                    started.elapsed()
                );
                return Err(dial_failure(&error));
            }
        };
        let status = response.status();
        let final_url = response.final_url();
        logging::trace!(
            target: "browser::net",
            "nav head {url} -> {status} {final_url} in {:?}",
            started.elapsed()
        );
        // One head reader for both dial paths, so the top-level document and a
        // fetch outcome cannot disagree about which header becomes policy,
        // endpoints, or the nosniff flag
        // (<https://html.spec.whatwg.org/multipage/document-lifecycle.html#initialise-the-document-object>).
        let head = DialHead::internal(&response).into_document_head();
        let body = response.into_body().await;
        Ok(NavOutcome {
            head,
            body,
            _permit: permits,
        })
    }

    /// Async HTTP request for one renderer service call.
    pub(crate) async fn dial_request(
        &self,
        request: &DialRequest,
        initiator: &url::Origin,
        owner: RendererAssignmentId,
    ) -> Result<DialOutcome, DialFailure> {
        self.dial_request_unfiltered(request, initiator, owner)
            .await
            .map(|outcome| orb_filter(request, outcome))
    }

    /// The dial itself. Its answer is the *internal* response for engine
    /// consumers, so [`dial_request`](Self::dial_request) filters it before it
    /// leaves the browser. Cache writes and sheet bookkeeping see the
    /// unfiltered answer, which is what a later same-origin request reuses.
    async fn dial_request_unfiltered(
        &self,
        request: &DialRequest,
        initiator: &url::Origin,
        owner: RendererAssignmentId,
    ) -> Result<DialOutcome, DialFailure> {
        let started = Instant::now();
        let _keepalive = KeepaliveReservation::acquire(request)?;
        let deadline = Instant::now() + Self::dial_budget(request);
        let outbound = self.build_outbound(request, initiator, deadline)?;
        // `Vary` names headers this host adds below the renderer, so the cache
        // compares the list that is about to be sent, not the page's
        // (<https://www.rfc-editor.org/rfc/rfc9111#section-4.1>).
        let sent = dial::sent_headers(&outbound);
        let (stored, validators) = match self.cache.read(request, &sent, initiator, owner.get())? {
            CacheDecision::Hit(hit) => {
                trace_dial_result(request, started, Ok(&hit));
                return Ok(hit);
            }
            CacheDecision::Miss {
                stored, validators, ..
            } => (stored, validators),
        };
        logging::trace!(
            target: "browser::net",
            "request start {:?} {}",
            request.policy.kind(),
            request.url
        );
        let result = async {
            match self
                .send_head(request, initiator, stored.as_ref(), &validators, outbound)
                .await?
            {
                DialProgress::Done(outcome) => {
                    self.cache
                        .remember(request, &sent, initiator, owner.get(), &outcome);
                    Ok(*outcome)
                }
                DialProgress::Response { response, permits } => match request.delivery {
                    BodyDelivery::Buffer => {
                        let _permits = permits;
                        let head = DialHead::read(request, &response);
                        let body = if head.null_body() {
                            ResponseBody::Bytes(Vec::new())
                        } else {
                            let mut reader =
                                BodyReader::new(*response, request.integrity.clone(), true).await;
                            loop {
                                match reader.next().await {
                                    BodyEvent::Chunk(_) => {}
                                    BodyEvent::Done(body) => {
                                        break ResponseBody::Bytes(body);
                                    }
                                    BodyEvent::DecodeError => {
                                        break ResponseBody::Undecodable;
                                    }
                                    BodyEvent::Failed(failure) => return Err(failure),
                                }
                            }
                        };
                        let outcome = head.into_outcome(body);
                        self.cache
                            .remember(request, &sent, initiator, owner.get(), &outcome);
                        Ok(outcome)
                    }
                    BodyDelivery::Stream => {
                        Ok(self.start_stream(request, &sent, initiator, owner, *response, permits))
                    }
                },
            }
        }
        .await;
        if let Ok(outcome) = &result {
            self.remember_sheet(request, initiator, outcome.status, &outcome.final_url);
        }
        trace_dial_result(request, started, result.as_ref());
        result
    }

    /// Sends the request and reads the response head.
    async fn send_head(
        &self,
        request: &DialRequest,
        initiator: &url::Origin,
        stored: Option<&DialOutcome>,
        validators: &Validators,
        mut outbound: Request,
    ) -> Result<DialProgress, DialFailure> {
        let deadline = Instant::now() + Self::dial_budget(request);
        let global = Self::acquire(&self.permits.global, deadline).await?;
        let tab = Self::acquire(&self.tab, deadline).await?;
        validators.apply(&mut outbound)?;
        let blocked_redirect = watch_redirects(request, initiator, &mut outbound);
        let response = match self.agent.send(outbound).await {
            Ok(response) => response,
            Err(error) => {
                let done =
                    refused_redirect(&blocked_redirect).ok_or_else(|| dial_failure(&error))?;
                return Ok(DialProgress::Done(Box::new(done)));
            }
        };
        if request.integrity.is_some() && response.response_type().is_opaque() {
            return Err(DialFailure::Connect);
        }
        if response.status() == 304
            && let Some(stored) = stored
        {
            // A 304 that names no representation this dial made conditional
            // cannot update the entry, and treating it as a response would
            // hand the page a body the server never sent
            // (<https://www.rfc-editor.org/rfc/rfc9111#section-4.3.4>).
            let merged = HttpCache::merge_not_modified(stored, &response, validators)
                .ok_or(DialFailure::Connect)?;
            return Ok(DialProgress::Done(Box::new(merged)));
        }
        Ok(DialProgress::Response {
            response: Box::new(response),
            permits: DialPermits {
                _global: global,
                _tab: tab,
            },
        })
    }

    /// Registers the body stream with a task that owns the permits and the
    /// cache write for its bytes.
    fn start_stream(
        &self,
        request: &DialRequest,
        sent: &[(String, String)],
        initiator: &url::Origin,
        owner: RendererAssignmentId,
        response: net::Response,
        permits: DialPermits,
    ) -> DialOutcome {
        let head = DialHead::read(request, &response);
        let opaque = head.null_body();
        let mut outcome = head.into_outcome(ResponseBody::Bytes(Vec::new()));
        // An unsafe method's successful response invalidates what the cache
        // holds for the URL as soon as its head arrives
        // (<https://www.rfc-editor.org/rfc/rfc9111#section-4.4>). A `no-cors`
        // response is opaque with a null body and still invalidates.
        self.cache.invalidate_unsafe(request, owner.get(), &outcome);
        // A page-visible opaque response has a null body, so there is nothing
        // to stream (<https://fetch.spec.whatwg.org/#concept-filtered-response-opaque>).
        if opaque {
            return outcome;
        }
        let (tx, rx) = tokio::sync::mpsc::channel(STREAM_CHUNK_DEPTH);
        let store = self
            .cache
            .may_store(request, sent, &outcome)
            .then(|| PendingStore {
                outcome: outcome.clone(),
                cache: Arc::clone(&self.cache),
                request: request.clone(),
                sent: sent.to_vec(),
                initiator: initiator.clone(),
                partition: owner.get(),
            });
        let task = spawn_stream_body(StreamJob {
            response,
            integrity: request.integrity.clone(),
            store,
            permits,
            kind: request.policy.kind(),
            url: request.url.to_string(),
            tx,
        });
        let id = StreamId::mint(self.next_stream.fetch_add(1, Ordering::Relaxed));
        self.streams
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                id,
                StreamEntry {
                    owner,
                    rx: Arc::new(tokio::sync::Mutex::new(rx)),
                    task,
                },
            );
        outcome.body = ResponseBody::Streamed(id);
        outcome
    }

    pub(crate) async fn read_stream(
        &self,
        owner: RendererAssignmentId,
        id: StreamId,
    ) -> Result<Option<Vec<u8>>, DialFailure> {
        let rx = self
            .streams
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&id)
            .filter(|entry| entry.owner == owner)
            .map(|entry| Arc::clone(&entry.rx));
        let Some(rx) = rx else {
            return Ok(None);
        };
        let mut rx = rx.lock().await;
        match rx.recv().await {
            Some(StreamMessage::Chunk(bytes)) => Ok(Some(bytes)),
            Some(StreamMessage::End) | None => {
                self.remove_stream(id);
                Ok(None)
            }
            Some(StreamMessage::Error(failure)) => {
                self.remove_stream(id);
                Err(failure)
            }
        }
    }

    pub(crate) fn cancel_stream(&self, owner: RendererAssignmentId, id: StreamId) {
        use std::collections::hash_map::Entry;
        let mut streams = self
            .streams
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Entry::Occupied(entry) = streams.entry(id)
            && entry.get().owner == owner
        {
            abort_stream(Some(entry.remove()));
        }
    }

    fn remove_stream(&self, id: StreamId) {
        let mut streams = self
            .streams
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        abort_stream(streams.remove(&id));
    }

    /// Aborts every stream an assignment owns, so a released renderer leaves
    /// no body task reading into memory for nobody.
    pub(crate) fn release_streams(&self, owner: RendererAssignmentId) {
        let mut streams = self
            .streams
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let abandoned: Vec<StreamId> = streams
            .iter()
            .filter(|(_, entry)| entry.owner == owner)
            .map(|(id, _)| *id)
            .collect();
        for id in abandoned {
            abort_stream(streams.remove(&id));
        }
    }

    /// The deadline budget one page dial gets: the per-call cap, lowered by
    /// the request's own `timeout_ms` when it set one.
    fn dial_budget(request: &DialRequest) -> Duration {
        if request.timeout_ms == 0 {
            PAGE_FETCH_TIMEOUT
        } else {
            PAGE_FETCH_TIMEOUT.min(Duration::from_millis(request.timeout_ms))
        }
    }

    /// Records a successful style sheet response's URLs, so a later explicit
    /// referrer naming the sheet can be authorized. A sheet that failed or
    /// never arrived is not recorded: nothing it contained can issue a request
    /// (<https://w3c.github.io/webappsec-referrer-policy/#integration-with-css>).
    fn remember_sheet(
        &self,
        request: &DialRequest,
        initiator: &url::Origin,
        status: u16,
        final_url: &Url,
    ) {
        if request.policy.kind() != DialKind::Stylesheet || !(200..300).contains(&status) {
            return;
        }
        // Every opaque initiator is a fresh unique origin, so a sheet it loads
        // could never authorize a later referrer; recording it would only grow
        // the map. A later request from such a document falls back to sending
        // no referrer, which is what the strip-for-use algorithm answers for a
        // URL with no host anyway
        // (<https://w3c.github.io/webappsec-referrer-policy/#strip-url>).
        // `url`'s `Origin` names opaque origins by not being a tuple.
        if !initiator.is_tuple() {
            return;
        }
        let mut sheets = self
            .sheets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let loaded = sheets.entry(initiator.clone()).or_default();
        for url in [&request.url, final_url] {
            // A referrer never carries a fragment, and the sheet URL the
            // renderer names is the one it was loaded from
            // (<https://w3c.github.io/webappsec-referrer-policy/#strip-url>).
            let mut url = url.clone();
            url.set_fragment(None);
            loaded.insert(url);
        }
    }

    /// Builds one outbound request with this tab's style sheet set as the
    /// explicit-referrer authority. The lock is taken and released inside the
    /// call, so no guard lives across an await.
    fn build_outbound(
        &self,
        request: &DialRequest,
        initiator: &url::Origin,
        deadline: Instant,
    ) -> Result<Request, DialFailure> {
        let sheets = self
            .sheets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        outbound_request(request, initiator, deadline, &sheets)
    }

    async fn acquire(
        semaphore: &Arc<Semaphore>,
        deadline: Instant,
    ) -> Result<tokio::sync::OwnedSemaphorePermit, DialFailure> {
        let permit = tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            Arc::clone(semaphore).acquire_owned(),
        )
        .await;
        match permit {
            Ok(Ok(permit)) => Ok(permit),
            Ok(Err(_)) => Err(DialFailure::Cancelled),
            Err(_) => Err(DialFailure::QueueFull),
        }
    }
}

/// Applies Opaque Response Blocking to one engine-facing opaque response, and
/// trims the head such a response delivers to what its consumer can use.
///
/// A page `fetch` already receives the filtered view, whose body is null and
/// whose headers are masked. Every other dial receives the unsafe internal
/// response so the engine can decode an image, compile a script, or parse a
/// sheet, and without this check it could ask for any cross-origin document and
/// read it. Chromium runs ORB in the network service before a byte reaches a
/// renderer; this is the same boundary
/// (<https://github.com/annevk/orb>).
fn orb_filter(request: &DialRequest, outcome: DialOutcome) -> DialOutcome {
    if request.policy.exposes_response() || !outcome.response_type.is_opaque() {
        return outcome;
    }
    let nosniff = outcome.headers.iter().any(|(name, value)| {
        name.eq_ignore_ascii_case("x-content-type-options")
            && value
                .split(',')
                .any(|option| option.trim().eq_ignore_ascii_case("nosniff"))
    });
    let body = match &outcome.body {
        ResponseBody::Bytes(bytes) => Some(bytes.as_slice()),
        // An undecodable body has no readable bytes, and a streamed one has
        // not been read yet: the MIME-type rules still apply, and the sniffing
        // rules wait for bytes that never arrive (Chromium's `SniffMore`).
        ResponseBody::Undecodable | ResponseBody::Streamed(_) => None,
    };
    if net::orb::blocks(outcome.content_type.as_deref(), nosniff, body) {
        return DialOutcome {
            content_type: None,
            headers: Vec::new(),
            body: ResponseBody::Bytes(Vec::new()),
            ..outcome
        };
    }
    // An opaque head carries the whole served header list, which names
    // cookies, cache policy, and origins the consumer has no say over. The
    // engine may only act on what decides how the body is interpreted.
    let headers = outcome
        .headers
        .iter()
        .filter(|(name, _)| {
            name.eq_ignore_ascii_case("content-type")
                || name.eq_ignore_ascii_case("x-content-type-options")
        })
        .cloned()
        .collect();
    DialOutcome { headers, ..outcome }
}

/// The cache write a streamed body performs once its bytes are complete.
struct PendingStore {
    outcome: DialOutcome,
    cache: Arc<HttpCache>,
    request: DialRequest,
    sent: Vec<(String, String)>,
    initiator: url::Origin,
    partition: u64,
}

/// One event from [`BodyReader`].
enum BodyEvent {
    /// A decoded chunk. The reader has already collected it when the delivery
    /// mode needs the whole body.
    Chunk(Vec<u8>),
    /// The body ended; the collected bytes are what integrity checked.
    Done(Vec<u8>),
    /// The body could not be decoded for its `Content-Encoding`.
    DecodeError,
    /// The transfer failed before the body ended.
    Failed(DialFailure),
}

/// Reads one decoded response body. Both delivery modes drive this reader, so
/// the size cap, integrity check, and error mapping exist once.
struct BodyReader {
    body: net::Body,
    integrity: Option<String>,
    collect: bool,
    collected: Vec<u8>,
    total: usize,
}

impl BodyReader {
    async fn new(response: net::Response, integrity: Option<String>, collect: bool) -> Self {
        Self {
            body: response.into_body().await,
            integrity,
            collect,
            collected: Vec::new(),
            total: 0,
        }
    }

    async fn next(&mut self) -> BodyEvent {
        match self.body.read_chunk().await {
            Ok(Some(chunk)) => {
                self.total = self.total.saturating_add(chunk.len());
                if self.total > renderer::MAX_RESPONSE_BODY_BYTES {
                    return BodyEvent::Failed(DialFailure::Limit);
                }
                if self.collect {
                    self.collected.extend_from_slice(&chunk);
                }
                BodyEvent::Chunk(chunk)
            }
            Ok(None) => {
                if let Some(integrity) = self.integrity.as_deref()
                    && !renderer::integrity_verifies(integrity, &self.collected)
                {
                    return BodyEvent::Failed(DialFailure::Connect);
                }
                BodyEvent::Done(std::mem::take(&mut self.collected))
            }
            Err(net::NetError::Decode(_)) => BodyEvent::DecodeError,
            Err(error) => BodyEvent::Failed(dial_failure(&error)),
        }
    }
}

/// Everything one streamed body task owns until its bytes are done: the
/// response, the permits it borrows, and the cache write it may finish.
struct StreamJob {
    response: net::Response,
    integrity: Option<String>,
    store: Option<PendingStore>,
    permits: DialPermits,
    kind: DialKind,
    url: String,
    tx: tokio::sync::mpsc::Sender<StreamMessage>,
}

fn spawn_stream_body(job: StreamJob) -> tokio::task::JoinHandle<()> {
    let StreamJob {
        response,
        integrity,
        store,
        permits,
        kind,
        url,
        tx,
    } = job;
    tokio::spawn(async move {
        let _permits = permits;
        let collect = store.is_some() || integrity.is_some();
        let mut reader = BodyReader::new(response, integrity, collect).await;
        loop {
            match reader.next().await {
                BodyEvent::Chunk(chunk) => {
                    if tx.send(StreamMessage::Chunk(chunk)).await.is_err() {
                        return;
                    }
                }
                BodyEvent::Done(body) => {
                    if let Some(mut store) = store {
                        store.outcome.body = ResponseBody::Bytes(body);
                        store.cache.remember(
                            &store.request,
                            &store.sent,
                            &store.initiator,
                            store.partition,
                            &store.outcome,
                        );
                    }
                    let _ = tx.send(StreamMessage::End).await;
                    logging::trace!(target: "browser::net", "stream done {kind:?} {url}");
                    return;
                }
                BodyEvent::DecodeError => {
                    let _ = tx.send(StreamMessage::Error(DialFailure::Connect)).await;
                    logging::trace!(target: "browser::net", "stream decode error {kind:?} {url}");
                    return;
                }
                BodyEvent::Failed(failure) => {
                    let _ = tx.send(StreamMessage::Error(failure)).await;
                    logging::trace!(
                        target: "browser::net",
                        "stream failed {kind:?} {url}: {failure:?}"
                    );
                    return;
                }
            }
        }
    })
}

fn trace_dial_result(
    request: &DialRequest,
    started: Instant,
    result: Result<&DialOutcome, &DialFailure>,
) {
    match result {
        Ok(outcome) => logging::trace!(
            target: "browser::net",
            "request done {:?} {} -> {} in {:?}",
            request.policy.kind(),
            request.url,
            outcome.status,
            started.elapsed()
        ),
        Err(failure) => logging::trace!(
            target: "browser::net",
            "request failed {:?} {} in {:?}: {failure:?}",
            request.policy.kind(),
            request.url,
            started.elapsed()
        ),
    }
}

/// A fresh opaque origin.
///
/// A browser-initiated navigation has no initiating document, so nothing can
/// claim its origin. The `url` crate mints an opaque origin for a non-special
/// URL, and every mint is distinct, which is exactly "no principal".
fn opaque_origin() -> url::Origin {
    net::about_blank().origin()
}

/// The browser's default headers for one top-level (or popup) document
/// request, matching Chrome's document navigation.
///
/// `Upgrade-Insecure-Requests` is the document navigation preference
/// (<https://www.w3.org/TR/upgrade-insecure-requests/#preference>), and
/// `Accept` and the other Fetch Metadata headers match Chrome's document
/// navigation request
/// (<https://w3c.github.io/webappsec-fetch-metadata/>). `Sec-Fetch-Site` is
/// the net client's: it reads the request's one initiator origin and widens it
/// across redirects, for navigations and subresources alike.
///
/// Low-entropy UA client hints are sent only to potentially trustworthy URLs
/// (<https://wicg.github.io/ua-client-hints/#sec-ch-ua>,
/// <https://w3c.github.io/webappsec-secure-contexts/#is-url-trustworthy>).
fn chrome_navigation_request(request: &mut Request, url: &Url) {
    static_header(request, "Upgrade-Insecure-Requests", "1");
    static_header(
        request,
        "Accept",
        "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8",
    );
    static_header(request, "Sec-Fetch-Mode", "navigate");
    static_header(request, "Sec-Fetch-User", "?1");
    static_header(request, "Sec-Fetch-Dest", "document");
    if secure_context::is_potentially_trustworthy(url) {
        static_header(
            request,
            "Sec-CH-Prefers-Color-Scheme",
            crate::SEC_CH_PREFERS_COLOR_SCHEME,
        );
    }
}

/// Sets a request header this process chose, not the page. The value is a
/// static HTTP token, so a rejected one is a programming error, and the panic
/// names the header that has none.
fn static_header(request: &mut Request, name: &str, value: &str) {
    request
        .headers
        .insert(name, value)
        .unwrap_or_else(|_| panic!("{name}: {value:?} is not a header value"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use dial::referrer_authorized;
    use renderer::RequestPolicy;
    use std::time::Duration;

    fn dial(policy: RequestPolicy, url: &str) -> DialRequest {
        let url = Url::parse(url).expect("dial URL");
        let initiator = Url::parse("http://a.test/page").expect("page URL");
        DialRequest::new(policy, &url, &initiator.origin())
    }

    /// A sheet's referrer authority belongs to the origin that loaded it.
    /// Another document in the same tab must not be able to name that URL and
    /// leak it (with the policy it was served under) to a target of its
    /// choosing
    /// (<https://w3c.github.io/webappsec-referrer-policy/#integration-with-css>).
    #[test]
    fn a_sheet_referrer_belongs_to_the_document_that_loaded_it() {
        let loader = Url::parse("http://a.test/page").expect("page URL").origin();
        let other = Url::parse("http://b.test/page").expect("page URL").origin();
        let sheet = Url::parse("http://cdn.test/site.css").expect("sheet URL");
        let mut sheets = HashMap::new();
        sheets.insert(loader.clone(), HashSet::from([sheet.clone()]));

        assert!(
            referrer_authorized(&sheet, &loader, &sheets),
            "the document that loaded the sheet may name it"
        );
        assert!(
            !referrer_authorized(&sheet, &other, &sheets),
            "another origin in the tab may not borrow it"
        );
        let fragment = Url::parse("http://cdn.test/site.css#x").expect("sheet URL");
        assert!(
            referrer_authorized(&fragment, &loader, &sheets),
            "the comparison is on the fragment-stripped URL"
        );
    }

    /// A cross-origin `no-cors` subresource receives the *internal* response,
    /// so without ORB a page could label a dial an image and read a
    /// cross-origin document
    /// (<https://github.com/annevk/orb>).
    #[test]
    fn orb_stops_an_opaque_document_and_trims_an_opaque_head() {
        fn opaque(content_type: &str, body: &[u8]) -> DialOutcome {
            DialOutcome {
                status: 200,
                status_text: "OK".into(),
                final_url: Url::parse("http://b.test/img").expect("image URL"),
                content_type: Some(content_type.to_owned()),
                headers: vec![
                    ("content-type".to_owned(), content_type.to_owned()),
                    ("set-cookie".to_owned(), "leak=1".to_owned()),
                    ("cache-control".to_owned(), "max-age=60".to_owned()),
                ],
                body: ResponseBody::Bytes(body.to_vec()),
                response_type: net::cors::ResponseType::Opaque,
                crossed_origin: true,
                redirect_count: 0,
                cookies_stored: false,
                referrer: None,
            }
        }

        let request = dial(
            RequestPolicy::Image { crossorigin: None },
            "http://b.test/img",
        );
        let blocked = orb_filter(&request, opaque("image/png", b"<!doctype html>"));
        assert!(
            matches!(&blocked.body, ResponseBody::Bytes(bytes) if bytes.is_empty()),
            "a document labelled as an image keeps no body"
        );
        assert!(blocked.headers.is_empty(), "a blocked response has no head");
        assert_eq!(blocked.content_type, None);

        let kept = orb_filter(
            &request,
            opaque("image/png", b"\x89PNG\r\n\x1a\nimage bytes"),
        );
        assert!(
            matches!(&kept.body, ResponseBody::Bytes(bytes) if !bytes.is_empty()),
            "a real image keeps its body"
        );
        assert_eq!(
            kept.headers,
            [("content-type".to_owned(), "image/png".to_owned())],
            "an opaque head keeps only what decides how the body is read"
        );

        // A page `fetch` reads the filtered view, which the fetch layer already
        // masks; ORB must not touch it twice.
        let fetch = dial(
            RequestPolicy::Fetch(renderer::CorsRequest {
                mode: renderer::CorsMode::NoCors,
                credentials: net::cors::CredentialsMode::Include,
                redirect: net::cors::RedirectMode::Follow,
            }),
            "http://b.test/img",
        );
        let untouched = orb_filter(&fetch, opaque("text/html", b"<!doctype html>"));
        assert!(
            matches!(&untouched.body, ResponseBody::Bytes(bytes) if !bytes.is_empty()),
            "the page-facing path keeps its own filtering"
        );
    }

    fn js_fetch(mode: renderer::CorsMode, url: &str) -> DialRequest {
        dial(
            RequestPolicy::Fetch(renderer::CorsRequest {
                mode,
                credentials: net::cors::CredentialsMode::SameOrigin,
                redirect: net::cors::RedirectMode::Follow,
            }),
            url,
        )
    }

    /// What each policy actually puts on the wire: the CORS triple, the
    /// fetch-metadata destination and mode, and the origin the browser
    /// authorized. The renderer's table is only a claim until this holds.
    /// One row of the wire table: the policy, its resolved CORS triple, and
    /// the fetch-metadata mode and destination the browser writes.
    type WireCase = (
        RequestPolicy,
        net::cors::RequestMode,
        net::cors::CredentialsMode,
        net::cors::RedirectMode,
        &'static str,
        &'static str,
    );

    fn wire_table() -> Vec<WireCase> {
        use net::cors::{CredentialsMode, RedirectMode, RequestMode};

        let anonymous = Some(net::cors::CrossOrigin::Anonymous);
        let fetch = |mode, credentials| {
            RequestPolicy::Fetch(renderer::CorsRequest {
                mode,
                credentials,
                redirect: RedirectMode::Follow,
            })
        };
        let mut table = vec![
            (
                fetch(renderer::CorsMode::NoCors, CredentialsMode::Omit),
                RequestMode::NoCors,
                CredentialsMode::Omit,
                RedirectMode::Follow,
                "no-cors",
                "empty",
            ),
            (
                RequestPolicy::ClassicScript { crossorigin: None },
                RequestMode::NoCors,
                CredentialsMode::Include,
                RedirectMode::Follow,
                "no-cors",
                "script",
            ),
            (
                RequestPolicy::ClassicScript {
                    crossorigin: anonymous,
                },
                RequestMode::Cors,
                CredentialsMode::SameOrigin,
                RedirectMode::Follow,
                "cors",
                "script",
            ),
            (
                RequestPolicy::Stylesheet { crossorigin: None },
                RequestMode::NoCors,
                CredentialsMode::Include,
                RedirectMode::Follow,
                "no-cors",
                "style",
            ),
            (
                RequestPolicy::Image {
                    crossorigin: anonymous,
                },
                RequestMode::Cors,
                CredentialsMode::SameOrigin,
                RedirectMode::Follow,
                "cors",
                "image",
            ),
            (
                RequestPolicy::Font,
                RequestMode::Cors,
                CredentialsMode::SameOrigin,
                RedirectMode::Follow,
                "cors",
                "font",
            ),
            (
                RequestPolicy::FrameLoad,
                RequestMode::Navigate,
                CredentialsMode::Include,
                RedirectMode::Follow,
                "navigate",
                "iframe",
            ),
            (
                RequestPolicy::CspReportTo,
                RequestMode::Cors,
                CredentialsMode::SameOrigin,
                RedirectMode::Follow,
                "cors",
                "report",
            ),
            (
                RequestPolicy::CspReportUri,
                RequestMode::NoCors,
                CredentialsMode::SameOrigin,
                RedirectMode::Error,
                "no-cors",
                "report",
            ),
        ];
        table.extend(wire_module_table());
        table
    }

    /// Module fetches: a graph is `cors`, a module worker's root script is
    /// `same-origin`, and both carry the credentials mode of their script fetch
    /// options
    /// (<https://html.spec.whatwg.org/multipage/webappapis.html#fetch-a-single-module-script>,
    /// step 4).
    fn wire_module_table() -> [WireCase; 2] {
        use net::cors::{CredentialsMode, RedirectMode, RequestMode};

        [
            (
                RequestPolicy::ModuleScript {
                    credentials: CredentialsMode::Include,
                },
                RequestMode::Cors,
                CredentialsMode::Include,
                RedirectMode::Follow,
                "cors",
                "script",
            ),
            (
                RequestPolicy::ModuleWorkerScript {
                    credentials: CredentialsMode::Include,
                },
                RequestMode::SameOrigin,
                CredentialsMode::Include,
                RedirectMode::Follow,
                "same-origin",
                "script",
            ),
        ]
    }

    #[test]
    fn every_policy_reaches_the_wire_with_its_own_parameters() {
        let initiator = Url::parse("http://a.test/page").expect("page URL").origin();
        let deadline = Instant::now() + Duration::from_secs(5);
        let sheets = HashMap::new();
        for (policy, mode, credentials, redirect, fetch_mode, destination) in wire_table() {
            let request = dial(policy, "http://b.test/x");
            let outbound = outbound_request(&request, &initiator, deadline, &sheets)
                .expect("outbound request");
            assert_eq!(outbound.cors.mode, mode, "{:?}", policy.kind());
            assert_eq!(
                outbound.cors.credentials,
                credentials,
                "{:?}",
                policy.kind()
            );
            assert_eq!(outbound.cors.redirect, redirect, "{:?}", policy.kind());
            assert_eq!(
                outbound.cors.origin,
                initiator.clone(),
                "the browser fills in the origin it authorized"
            );
            assert_eq!(
                outbound.headers.get("sec-fetch-mode"),
                Some(fetch_mode.as_bytes()),
                "{:?}",
                policy.kind()
            );
            assert_eq!(
                outbound.headers.get("sec-fetch-dest"),
                Some(destination.as_bytes()),
                "{:?}",
                policy.kind()
            );
        }
    }

    /// A `no-cors` body's type goes through the same guard as every other
    /// field, so it cannot smuggle a type that would have needed a preflight
    /// (<https://fetch.spec.whatwg.org/#concept-headers-guard>).
    #[test]
    fn a_no_cors_body_cannot_smuggle_a_non_safelisted_content_type() {
        use net::cors::{CredentialsMode, RedirectMode};

        let initiator = Url::parse("http://a.test/page").expect("page URL").origin();
        let deadline = Instant::now() + Duration::from_secs(5);
        let sheets = HashMap::new();
        let mut request = dial(
            RequestPolicy::Fetch(renderer::CorsRequest {
                mode: renderer::CorsMode::NoCors,
                credentials: CredentialsMode::Include,
                redirect: RedirectMode::Follow,
            }),
            "http://b.test/x",
        );
        request.method = net::Method::POST;
        request.content_type = Some("application/json".to_owned());
        let outbound =
            outbound_request(&request, &initiator, deadline, &sheets).expect("outbound request");
        assert!(
            outbound.headers.get("content-type").is_none(),
            "a JSON type would have needed a preflight"
        );

        request.content_type = Some("text/plain;charset=UTF-8".to_owned());
        let outbound =
            outbound_request(&request, &initiator, deadline, &sheets).expect("outbound request");
        assert_eq!(
            outbound.headers.get("content-type"),
            Some(b"text/plain;charset=UTF-8".as_slice()),
            "a safelisted type still reaches the wire"
        );

        // The guard is the page's. A `report-uri` delivery is `no-cors` too,
        // and CSP gives it a non-safelisted type by construction.
        let mut report = dial(RequestPolicy::CspReportUri, "http://b.test/report");
        report.method = net::Method::POST;
        report.content_type = Some("application/csp-report".to_owned());
        let outbound =
            outbound_request(&report, &initiator, deadline, &sheets).expect("outbound report");
        assert_eq!(
            outbound.headers.get("content-type"),
            Some(b"application/csp-report".as_slice()),
            "an engine-built request keeps the type its spec names"
        );
    }

    #[tokio::test]
    async fn a_cancelled_stream_releases_its_permit_for_the_next_dial() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let mut context = NetworkContext::new(AgentOptions::default());
        context.permits.global = Arc::new(Semaphore::new(1));
        let handle = context.tab_handle();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let address = listener.local_addr().expect("address");
        let origin = format!("http://{address}");
        let server = tokio::spawn(async move {
            for index in 0..2 {
                let (mut socket, _) = listener.accept().await.expect("accept");
                tokio::spawn(async move {
                    let mut buffer = [0u8; 1024];
                    let _ = socket.read(&mut buffer).await;
                    if index == 0 {
                        socket
                            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1024\r\n\r\npartial")
                            .await
                            .expect("stalled head");
                        tokio::time::sleep(Duration::from_secs(30)).await;
                    } else {
                        socket
                            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                            .await
                            .expect("complete head");
                    }
                });
            }
        });
        let initiator = Url::parse(&format!("{origin}/page")).expect("page URL");
        let owner = RendererAssignmentId::new(1);
        let mut stream_request =
            js_fetch(renderer::CorsMode::SameOrigin, &format!("{origin}/stream"));
        stream_request.initiator = renderer::SerializedOrigin::from(&initiator.origin());
        stream_request.referrer = renderer::ReferrerRequest::Client {
            url: initiator.clone(),
        };
        stream_request.delivery = BodyDelivery::Stream;
        let outcome = handle
            .dial_request(&stream_request, &initiator.origin(), owner)
            .await
            .expect("stream head");
        let ResponseBody::Streamed(stream) = outcome.body else {
            panic!("the outcome is a stream");
        };
        assert_eq!(
            handle.permits.global.available_permits(),
            0,
            "the body task holds the permits"
        );
        handle.cancel_stream(owner, stream);
        tokio::time::timeout(Duration::from_secs(1), async {
            while handle.permits.global.available_permits() == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("cancelling the stream releases the permit");

        let mut next_request = js_fetch(renderer::CorsMode::SameOrigin, &format!("{origin}/done"));
        next_request.initiator = renderer::SerializedOrigin::from(&initiator.origin());
        next_request.referrer = renderer::ReferrerRequest::Client {
            url: initiator.clone(),
        };
        let next = tokio::time::timeout(
            Duration::from_secs(1),
            handle.dial_request(&next_request, &initiator.origin(), owner),
        )
        .await
        .expect("the next dial gets the released permit")
        .expect("next dial");
        assert!(
            matches!(&next.body, ResponseBody::Bytes(body) if body == b"ok"),
            "the next dial reads its body"
        );
        server.abort();
    }

    /// A cross-origin `no-cors` subresource that is really a document never
    /// reaches the engine, and the opaque head it does see is trimmed to what
    /// decides how a body is interpreted
    /// (<https://github.com/annevk/orb>).
    #[tokio::test]
    async fn orb_blocks_a_cross_origin_document_before_the_engine_reads_it() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let body: &[u8] = b"<!doctype html><p>secret</p>";
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            tokio::spawn(async move {
                let mut buffer = [0u8; 1024];
                let _ = socket.read(&mut buffer).await;
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: text/html\r\nSet-Cookie: a=1\r\n\r\n",
                    body.len()
                );
                socket
                    .write_all(head.as_bytes())
                    .await
                    .expect("response head");
                socket.write_all(body).await.expect("response body");
            });
        });
        let context = NetworkContext::new(AgentOptions::default());
        let handle = context.tab_handle();
        let initiator = Url::parse("http://a.test/page").expect("page URL").origin();
        let owner = RendererAssignmentId::new(1);

        let image = dial(
            RequestPolicy::Image { crossorigin: None },
            &format!("http://{address}/doc"),
        );
        let outcome = handle
            .dial_request(&image, &initiator, owner)
            .await
            .expect("image dial");
        assert!(
            matches!(&outcome.body, ResponseBody::Bytes(body) if body.is_empty()),
            "a document labelled as an image reaches the engine empty"
        );
        assert_eq!(outcome.content_type, None, "a blocked response has no type");
        assert!(
            outcome.headers.is_empty(),
            "a blocked response has no header list"
        );
        server.abort();
    }

    /// A `no-cors` subresource reads the unsafe response — body, status, and
    /// headers — because the engine consumes it, while the same request made
    /// by page script is an opaque filtered response
    /// (<https://fetch.spec.whatwg.org/#concept-internal-response>).
    #[tokio::test]
    async fn an_opaque_subresource_is_readable_by_the_engine_and_not_by_script() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            for _ in 0..2 {
                let (mut socket, _) = listener.accept().await.expect("accept");
                tokio::spawn(async move {
                    let mut buffer = [0u8; 1024];
                    let _ = socket.read(&mut buffer).await;
                    socket
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nContent-Type: image/png\r\nSet-Cookie: a=1\r\n\r\npng",
                        )
                        .await
                        .expect("response");
                });
            }
        });
        let context = NetworkContext::new(AgentOptions::default());
        let handle = context.tab_handle();
        let initiator = Url::parse("http://a.test/page").expect("page URL").origin();
        let owner = RendererAssignmentId::new(1);
        let url = format!("http://{address}/img");

        let image = dial(RequestPolicy::Image { crossorigin: None }, &url);
        let outcome = handle
            .dial_request(&image, &initiator, owner)
            .await
            .expect("image dial");
        assert_eq!(
            outcome.response_type,
            net::cors::ResponseType::Opaque,
            "a cross-origin no-cors image is opaque"
        );
        assert_eq!(outcome.status, 200, "the engine reads the unsafe status");
        assert_eq!(
            outcome.content_type.as_deref(),
            Some("image/png"),
            "the image decoder needs the served type"
        );
        assert!(
            matches!(&outcome.body, ResponseBody::Bytes(body) if body == b"png"),
            "the engine reads the unsafe body"
        );
        assert!(
            outcome
                .headers
                .iter()
                .all(|(name, _)| !name.eq_ignore_ascii_case("set-cookie")),
            "an HttpOnly cookie value must not reach the renderer"
        );

        let page = js_fetch(renderer::CorsMode::NoCors, &url);
        let outcome = handle
            .dial_request(&page, &initiator, owner)
            .await
            .expect("page dial");
        assert_eq!(
            outcome.status, 0,
            "an opaque filtered response has no status"
        );
        assert!(
            matches!(&outcome.body, ResponseBody::Bytes(body) if body.is_empty()),
            "an opaque filtered response has a null body"
        );
        assert!(
            outcome.headers.is_empty(),
            "an opaque filtered response has no headers"
        );
        server.abort();
    }

    /// A stored `ETag` holds isomorphic-decoded bytes, so writing it back must
    /// keep one byte per code point. UTF-8 would send `0xC3 0xA9` where the
    /// server sent `0xE9`, and every revalidation would answer 200.
    #[tokio::test]
    async fn revalidation_sends_a_non_ascii_etag_as_one_byte_per_code_point() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let context = NetworkContext::new(AgentOptions::default());
        let handle = context.tab_handle();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let address = listener.local_addr().expect("address");
        let origin = format!("http://{address}");
        let (replayed_tx, replayed_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let mut replayed_tx = Some(replayed_tx);
            for index in 0..2 {
                let (mut socket, _) = listener.accept().await.expect("accept");
                let mut buffer = [0u8; 2048];
                let read = socket.read(&mut buffer).await.expect("request");
                if index == 0 {
                    socket
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nETag: \"\xE9\"\r\nSet-Cookie: sid=secret; HttpOnly\r\nContent-Length: 2\r\n\r\nhi",
                        )
                        .await
                        .expect("first response");
                } else {
                    let _ = replayed_tx
                        .take()
                        .expect("one replay")
                        .send(buffer[..read].to_vec());
                    socket
                        .write_all(
                            b"HTTP/1.1 304 Not Modified\r\nETag: \"\xE9\"\r\nSet-Cookie: sid=secret; HttpOnly\r\n\r\n",
                        )
                        .await
                        .expect("304");
                }
            }
        });
        let initiator = Url::parse(&format!("{origin}/page")).expect("page URL");
        let owner = RendererAssignmentId::new(1);
        let mut request = js_fetch(renderer::CorsMode::SameOrigin, &format!("{origin}/etag"));
        request.initiator = renderer::SerializedOrigin::from(&initiator.origin());
        request.referrer = renderer::ReferrerRequest::Client {
            url: initiator.clone(),
        };
        let first = handle
            .dial_request(&request, &initiator.origin(), owner)
            .await
            .expect("first dial");
        assert_eq!(first.status, 200);
        assert!(
            !first
                .headers
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case("set-cookie")),
            "an HttpOnly cookie value must not reach page script"
        );

        let revalidated = handle
            .dial_request(&request, &initiator.origin(), owner)
            .await
            .expect("revalidation");
        assert_eq!(revalidated.status, 200, "a 304 merges into the stored head");
        assert_eq!(stored_bytes(&revalidated), 2);
        assert!(matches!(&revalidated.body, ResponseBody::Bytes(body) if body == b"hi"));
        assert!(
            !revalidated
                .headers
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case("set-cookie")),
            "a 304 merge must not add the cookie the page path filtered out"
        );

        let replayed = replayed_rx.await.expect("second request");
        assert!(
            replayed
                .windows(b"if-none-match: \"\xE9\"".len())
                .any(|window| window == b"if-none-match: \"\xE9\""),
            "the validator went back as the bytes the server sent: {:?}",
            String::from_utf8_lossy(&replayed)
        );
        server.abort();
    }

    #[tokio::test]
    async fn exhausted_permits_fail_with_queue_full_before_the_deadline() {
        let permits = Arc::new(Semaphore::new(1));
        let _held = Arc::clone(&permits).acquire_owned().await.expect("permit");

        let error = TabNetworkHandle::acquire(&permits, Instant::now() + Duration::from_millis(50))
            .await
            .expect_err("second permit must time out");
        assert_eq!(error, DialFailure::QueueFull);
    }

    #[tokio::test]
    async fn released_permits_are_reusable() {
        let permits = Arc::new(Semaphore::new(1));
        {
            let _held = Arc::clone(&permits).acquire_owned().await.expect("permit");
            let error =
                TabNetworkHandle::acquire(&permits, Instant::now() + Duration::from_millis(20))
                    .await
                    .expect_err("held permit");
            assert_eq!(error, DialFailure::QueueFull);
        }
        let _again =
            TabNetworkHandle::acquire(&permits, Instant::now() + Duration::from_millis(50))
                .await
                .expect("permit after release");
    }

    /// The identity hints are written by the agent, so this builder names only
    /// the preference hint it owns, and only over a trustworthy transport
    /// (<https://wicg.github.io/user-preference-media-features-headers/#sec-ch-prefers-color-scheme>).
    #[test]
    fn chrome_navigation_headers_are_limited_to_trustworthy_urls() {
        let secure = Url::parse("https://example.com/").expect("https url");
        let mut secure_request = net::Request::new(net::Method::GET, secure.clone());
        chrome_navigation_request(&mut secure_request, &secure);
        assert!(
            secure_request
                .headers
                .get("sec-ch-prefers-color-scheme")
                .is_some(),
            "the preference hint reaches a trustworthy URL"
        );

        let insecure = Url::parse("http://example.com/").expect("http url");
        let mut insecure_request = net::Request::new(net::Method::GET, insecure.clone());
        chrome_navigation_request(&mut insecure_request, &insecure);
        assert!(
            insecure_request
                .headers
                .get("sec-ch-prefers-color-scheme")
                .is_none(),
            "the preference hint stays off plain http"
        );
        assert!(
            secure_request.headers.get("sec-fetch-mode").is_some(),
            "the Fetch metadata group is built here and reaches trustworthy hops"
        );
        assert!(insecure_request.headers.get("accept").is_some());
        // The group is stripped per hop, where the redirect chain is known, so
        // this builder still writes it for a plain-http navigation too
        // (<https://w3c.github.io/webappsec-fetch-metadata/#fetch-integration>).
    }
}
