use std::borrow::Cow;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll};
use std::time::{Duration, Instant, SystemTime};

use crate::InitiatorKind;
use crate::error::{NetError, TimeoutKind, TransportError};
use crate::protocol::{HeaderMap, Method};
use crate::referrer::{Referrer, ReferrerPolicy, determine_referrer};
use crate::resolve::ResolveRule;
use crate::transport::{
    CallBudget, HopBudget, HttpEngine, Proxy, Transport, TrustedCa, WireBody, WireRequest,
    WireResponse, basic_authorization,
};
use async_compression::tokio::bufread::{BrotliDecoder, DeflateDecoder, GzipDecoder, ZlibDecoder};
use bytes::Bytes;
use cookies::{CookieJar, CookieOp, RetrievalKind, SameSiteCookiePolicy};
use hyper::body::Body as HttpBody;
use hyper::header::{self, HeaderValue};
use percent_encoding::percent_decode_str;
use tokio::io::{AsyncBufReadExt as _, AsyncRead, AsyncReadExt as _, BufReader, ReadBuf};
use url::Url;

const DEFAULT_MAX_REDIRECTS: u32 = 20;

/// One clock's wall and monotonic reads.
///
/// A value type with state, not a pair of `fn` pointers: a test or embedder
/// that needs a manual clock owns the state and passes it in, instead of
/// reaching for a process-global static because a function pointer cannot
/// capture anything.
pub trait ClockSource: Send + Sync {
    /// Current wall-clock time.
    fn wall(&self) -> SystemTime;
    /// Current monotonic instant.
    fn monotonic(&self) -> Instant;
}

impl<T: ClockSource + ?Sized> ClockSource for std::sync::Arc<T> {
    fn wall(&self) -> SystemTime {
        (**self).wall()
    }

    fn monotonic(&self) -> Instant {
        (**self).monotonic()
    }
}

/// Wall and monotonic clock reads used by one [`Agent`].
///
/// The default reads the system clocks. Embedders and tests pass their own
/// [`ClockSource`] so cookie expiry and every timeout path stay deterministic.
#[derive(Clone)]
pub struct Clock(std::sync::Arc<dyn ClockSource>);

impl Clock {
    /// Builds a clock from any [`ClockSource`].
    #[must_use]
    pub fn new(source: impl ClockSource + 'static) -> Self {
        Self(std::sync::Arc::new(source))
    }

    /// Current wall-clock time.
    #[must_use]
    pub fn now(&self) -> SystemTime {
        self.0.wall()
    }

    /// Current monotonic instant.
    #[must_use]
    pub fn monotonic(&self) -> Instant {
        self.0.monotonic()
    }
}

impl Default for Clock {
    fn default() -> Self {
        Self(std::sync::Arc::new(SystemClock))
    }
}

/// The process's own clocks.
struct SystemClock;

impl ClockSource for SystemClock {
    fn wall(&self) -> SystemTime {
        SystemTime::now()
    }

    fn monotonic(&self) -> Instant {
        Instant::now()
    }
}

impl std::fmt::Debug for Clock {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Clock")
    }
}

/// Constructor inputs for [`Agent::new`]. Every field is already validated, so
/// a bad value fails where the configuration was read instead of at the first
/// request. Debug output records whether a proxy is configured, not its URI or
/// credentials.
///
/// `identity` keeps only a validated user agent and its hints, `proxy` only its
/// parsed destination and credentials, and `tls_cas` only parsed certificates.
#[derive(Clone)]
pub struct AgentOptions {
    /// The browser identity: `User-Agent`, client hints, and the values the
    /// renderer and CDP report for it.
    pub identity: crate::ValidIdentity,
    /// Cap on the whole call, including redirects.
    pub timeout_global: Option<Duration>,
    /// Cap on a single hop.
    pub timeout_per_call: Option<Duration>,
    /// Maximum redirect hops. `0` returns the redirect response without following.
    pub max_redirects: u32,
    /// HTTP CONNECT proxy, parsed at the CLI or embedding boundary.
    pub proxy: Option<Proxy>,
    /// Wall and monotonic clock reads.
    pub clock: Clock,
    /// Parsed `--resolve=PATTERN=ADDR` rewrites, first match wins.
    pub resolve: Vec<ResolveRule>,
    /// Certificate authorities to trust in addition to the platform's, e.g. a
    /// private test CA. Repeatable.
    pub tls_cas: Vec<TrustedCa>,
}

impl std::fmt::Debug for AgentOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentOptions")
            .field("has_proxy", &self.proxy.is_some())
            .field("timeout_global", &self.timeout_global)
            .field("timeout_per_call", &self.timeout_per_call)
            .field("max_redirects", &self.max_redirects)
            .field("has_resolve", &!self.resolve.is_empty())
            .field("extra_tls_cas", &self.tls_cas.len())
            .finish_non_exhaustive()
    }
}

impl Default for AgentOptions {
    fn default() -> Self {
        Self {
            identity: crate::ValidIdentity::default(),
            timeout_global: None,
            timeout_per_call: None,
            max_redirects: DEFAULT_MAX_REDIRECTS,
            proxy: None,
            clock: Clock::default(),
            resolve: Vec::new(),
            tls_cas: Vec::new(),
        }
    }
}

/// Shared HTTP client: connection pool, cookie jar, and proxy/timeout settings.
///
/// Cloning is cheap. Debug output does not include proxy credentials.
#[derive(Clone)]
pub struct Agent {
    transport: Arc<dyn Transport>,
    identity: crate::ValidIdentity,
    max_redirects: u32,
    jar: Arc<Mutex<CookieJar>>,
    clock: Clock,
    timeout_global: Option<Duration>,
    timeout_per_call: Option<Duration>,
}

impl std::fmt::Debug for Agent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Agent")
            .field("max_redirects", &self.max_redirects)
            .field("jar", &self.jar)
            .field("timeout_global", &self.timeout_global)
            .field("timeout_per_call", &self.timeout_per_call)
            .finish_non_exhaustive()
    }
}

impl Agent {
    /// Builds an agent from `options`, the only constructor. Nothing fails
    /// here: the boundary that read the configuration already parsed every
    /// value.
    #[must_use]
    pub fn new(options: AgentOptions) -> Agent {
        let AgentOptions {
            identity,
            timeout_global,
            timeout_per_call,
            max_redirects,
            proxy,
            clock,
            resolve,
            tls_cas,
        } = options;
        let transport = Arc::new(HttpEngine::new(proxy, resolve.into(), &tls_cas));
        Agent {
            transport,
            identity,
            max_redirects,
            jar: Arc::new(Mutex::new(CookieJar::default())),
            clock,
            timeout_global,
            timeout_per_call,
        }
    }

    /// Builds an agent whose bytes move through `transport` instead of hyper.
    ///
    /// Every fetch rule — tainting, CORS, redirects, cookies, `Sec-Fetch-*`,
    /// referrer — still runs in this crate; only the one HTTP exchange is
    /// delegated. A host without sockets (wasm, an in-memory conformance
    /// origin) therefore runs the same fetch algorithm as the native browser
    /// (<https://fetch.spec.whatwg.org/#concept-http-network-fetch>).
    #[must_use]
    pub fn with_transport(options: AgentOptions, transport: Arc<dyn Transport>) -> Agent {
        let clock = options.clock.clone();
        Agent {
            transport,
            identity: options.identity,
            max_redirects: options.max_redirects,
            jar: Arc::new(Mutex::new(CookieJar::default())),
            clock,
            timeout_global: options.timeout_global,
            timeout_per_call: options.timeout_per_call,
        }
    }

    /// The timeout budget one `send` call starts with.
    fn call_budget(&self) -> CallBudget {
        CallBudget::new(
            self.timeout_global,
            self.timeout_per_call,
            self.clock.clone(),
        )
    }

    /// The identity every request, page, and CDP answer reports.
    #[must_use]
    pub fn identity(&self) -> &crate::ValidIdentity {
        &self.identity
    }

    /// Borrows the live jar, recovering from a poisoned lock.
    fn jar(&self) -> MutexGuard<'_, CookieJar> {
        self.jar.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Operation for a `document.cookie` or `WebDriver` view of `url`.
    fn fetch_op<'a>(
        &self,
        url: &'a Url,
        origin: &'a url::Origin,
        kind: RetrievalKind,
        partition_key: Option<&'a str>,
    ) -> CookieOp<'a> {
        CookieOp {
            url,
            now: self.clock.now(),
            kind,
            initiator_kind: InitiatorKind::Fetch,
            method_is_safe: true,
            initiator: Some(origin),
            cross_site_redirect: false,
            same_site_policy: SameSiteCookiePolicy::All,
            partition_key,
        }
    }

    /// `document.cookie` getter for `uri`: non-HTTP cookies, semicolon-separated,
    /// restricted to `partition_key`'s partition plus the unpartitioned jar.
    #[must_use]
    pub fn cookies_for_partition(&self, uri: &Url, partition_key: Option<&str>) -> String {
        let origin = uri.origin();
        self.jar()
            .cookie_string(self.fetch_op(uri, &origin, RetrievalKind::NonHttp, partition_key))
    }

    /// `document.cookie` getter without a top-level site, which sees only
    /// unpartitioned cookies.
    #[must_use]
    pub fn cookies_for(&self, uri: &Url) -> String {
        self.cookies_for_partition(uri, None)
    }

    /// `document.cookie` setter for `uri`. Invalid `Set-Cookie` lines are ignored.
    pub fn set_cookie_partition(
        &self,
        value: &str,
        uri: &Url,
        partition_key: Option<&str>,
    ) -> bool {
        let origin = uri.origin();
        self.jar().store(
            value,
            self.fetch_op(uri, &origin, RetrievalKind::NonHttp, partition_key),
        )
    }

    /// Stores one typed `CookieStore` write. Unlike a `Set-Cookie` line, a
    /// path holding U+003B (;) reaches the jar intact
    /// (<https://cookiestore.spec.whatwg.org/#set-a-cookie>).
    pub fn set_script_cookie_partition(
        &self,
        write: &crate::ScriptCookieWrite,
        uri: &Url,
        partition_key: Option<&str>,
    ) -> bool {
        let origin = uri.origin();
        self.jar().store_script(
            write,
            self.fetch_op(uri, &origin, RetrievalKind::NonHttp, partition_key),
        )
    }

    /// `document.cookie` setter for a request with no top-level site.
    pub fn set_cookie(&self, value: &str, uri: &Url) {
        let _stored = self.set_cookie_partition(value, uri, None);
    }

    /// Stores one `Set-Cookie` line with HTTP-level rules, so an `HttpOnly`
    /// cookie is allowed where `document.cookie` would refuse it. Returns
    /// whether the jar stored the cookie
    /// (<https://w3c.github.io/webdriver/#add-cookie>).
    pub fn store_cookie_http(&self, value: &str, uri: &Url) -> bool {
        let origin = uri.origin();
        self.jar().store(
            value,
            self.fetch_op(uri, &origin, RetrievalKind::Http, None),
        )
    }

    /// Cookies visible to `uri`, including session and `HttpOnly` cookies.
    /// This is the `WebDriver` cookie view
    /// (<https://w3c.github.io/webdriver/#get-all-cookies>).
    #[must_use]
    pub fn cookie_records(&self, uri: &Url) -> Vec<crate::CookieRecord> {
        self.cookie_records_partition(uri, None)
    }

    /// Cookies visible to `uri` in one partition, plus the unpartitioned jar.
    #[must_use]
    pub fn cookie_records_partition(
        &self,
        uri: &Url,
        partition_key: Option<&str>,
    ) -> Vec<crate::CookieRecord> {
        let origin = uri.origin();
        self.jar()
            .records_for(self.fetch_op(uri, &origin, RetrievalKind::Http, partition_key))
    }

    /// The cookies a `CookieStore` query for `uri` sees: `HttpOnly` cookies
    /// are excluded, and `partition_key`'s partition is included beside the
    /// unpartitioned jar
    /// (<https://cookiestore.spec.whatwg.org/#query-cookies>).
    #[must_use]
    pub fn script_cookie_records_partition(
        &self,
        uri: &Url,
        partition_key: Option<&str>,
    ) -> Vec<crate::CookieRecord> {
        let origin = uri.origin();
        self.jar()
            .records_for(self.fetch_op(uri, &origin, RetrievalKind::NonHttp, partition_key))
    }

    /// Every live cookie in the jar, including session cookies
    /// (<https://w3c.github.io/webdriver/#get-all-cookies>).
    #[must_use]
    pub fn all_cookie_records(&self) -> Vec<crate::CookieRecord> {
        self.jar().all_records(self.clock.now())
    }

    /// Drops every cookie from the live jar
    /// (<https://w3c.github.io/webdriver/#delete-all-cookies>).
    pub fn clear_cookies(&self) {
        self.jar().clear();
    }

    /// Persistent cookies from the live jar. Session cookies are omitted.
    #[must_use]
    pub fn export_cookies(&self) -> Vec<crate::CookieRecord> {
        self.jar().snapshot()
    }

    /// Loads `records` into the live jar, replacing matching identities.
    pub fn import_cookies(&self, records: Vec<crate::CookieRecord>) {
        self.jar().restore(records, self.clock.now());
    }

    /// Adds the `Cookie` header and the default `User-Agent` for one hop.
    fn prepare_outbound(
        &self,
        headers: &mut HeaderMap,
        operation: CookieOp<'_>,
    ) -> Result<(), NetError> {
        let cookie = self.jar().cookie_string(operation);
        if !cookie.is_empty() {
            let value = match headers.value("cookie") {
                Some(existing) => {
                    format!("{}; {cookie}", String::from_utf8_lossy(existing.as_bytes()))
                }
                None => cookie,
            };
            let value = HeaderValue::from_str(&value).map_err(|_| NetError::RejectedRequest)?;
            headers.set(header::COOKIE, value);
        }
        if headers.value("user-agent").is_none() {
            headers.append(
                header::USER_AGENT,
                self.identity.user_agent.header().clone(),
            );
        }
        Ok(())
    }

    /// Runs the preflight for a non-safelisted cross-origin `cors` request
    /// (<https://fetch.spec.whatwg.org/#cors-preflight-fetch>). The preflight
    /// carries the same `Referer` and `User-Agent` as the request it gates.
    /// `origin_header` is the request's serialized origin: the document origin,
    /// or `null` once a redirect has tainted it.
    async fn cors_preflight(
        &self,
        settings: &crate::cors::CorsSettings,
        origin_header: &HeaderValue,
        hop: &Hop,
        headers: &HeaderMap,
        wire: &Url,
        budget: HopBudget,
    ) -> Result<(), NetError> {
        let method = &hop.method;
        let unsafe_names = crate::cors::unsafe_request_header_names(headers);
        if !crate::cors::needs_preflight(method, &unsafe_names) {
            return Ok(());
        }
        let mut preflight = crate::cors::preflight_headers(method, &unsafe_names, origin_header);
        let origin = origin_header
            .to_str()
            .map_err(|_| NetError::RejectedRequest)?;
        if let Some(referrer) = headers.value("referer") {
            // A preflight's Referer is the request's stripped referrer without
            // the origin path slash
            // (<https://w3c.github.io/webappsec-referrer-policy/#determine-requests-referrer>).
            let referrer = referrer.to_str().map_err(|_| NetError::RejectedRequest)?;
            let referrer = referrer.strip_suffix('/').unwrap_or(referrer);
            preflight.append(
                header::REFERER,
                HeaderValue::from_str(referrer).map_err(|_| NetError::RejectedRequest)?,
            );
        }
        preflight.append(
            header::USER_AGENT,
            self.identity.user_agent.header().clone(),
        );
        let preflight_response = self
            .transport
            .exchange(
                WireRequest {
                    method: Method::OPTIONS,
                    url: wire.clone(),
                    headers: preflight,
                    body: None,
                },
                budget,
            )
            .await?;
        let status = preflight_response.status;
        let preflight_headers = preflight_response.headers;
        drop(preflight_response.body);
        if !crate::cors::preflight_ok(
            status,
            &preflight_headers,
            origin,
            settings.credentials.includes(),
            method,
            &unsafe_names,
        ) {
            return Err(NetError::RejectedRequest);
        }
        Ok(())
    }

    /// Applies the request-side CORS rules for one hop: the preflight a
    /// `cors`-tainted hop needs, and the `Origin` header
    /// (<https://fetch.spec.whatwg.org/#http-fetch>,
    /// <https://fetch.spec.whatwg.org/#append-a-request-origin-header>).
    async fn apply_cors_request(
        &self,
        settings: &crate::cors::CorsSettings,
        hop: &Hop,
        wire: &Url,
        headers: &mut HeaderMap,
        budget: HopBudget,
    ) -> Result<(), NetError> {
        let serialized_origin = hop.append_origin(settings, wire);
        let origin_header =
            HeaderValue::from_str(&serialized_origin).map_err(|_| NetError::RejectedRequest)?;
        // Case 2 of main fetch sends same-origin, still-`basic` requests
        // straight to scheme fetch, so only a `cors`-tainted hop can need a
        // preflight (<https://fetch.spec.whatwg.org/#main-fetch>).
        if hop.response_type == crate::cors::ResponseType::Cors {
            self.cors_preflight(settings, &origin_header, hop, headers, wire, budget)
                .await?;
        }
        // `Origin` rides on every hop whose response tainting is `cors`, and
        // on every other request whose method is neither GET nor HEAD
        // (<https://fetch.spec.whatwg.org/#append-a-request-origin-header>).
        // A same-origin `cors` POST that survives a 307/308 redirect sends
        // `Origin` here, which `fetch/api/redirect/redirect-origin.any.js`
        // expects to be absent; step 3 of the algorithm does not exempt that
        // case, and Chrome and Firefox fail the same two subtests.
        if hop.response_type == crate::cors::ResponseType::Cors
            || !matches!(hop.method.as_str(), "GET" | "HEAD")
        {
            headers.set(header::ORIGIN, origin_header);
        }
        Ok(())
    }

    /// Stores every `Set-Cookie` line and reports whether the jar kept one,
    /// which can be an insertion, a replacement, or an expiry
    /// (<https://httpwg.org/http-extensions/draft-ietf-httpbis-rfc6265bis.html#section-5.6>).
    fn store_set_cookie_lines(
        &self,
        operation: CookieOp<'_>,
        lines: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> bool {
        let mut jar = self.jar();
        let now = self.clock.now();
        let mut stored = false;
        for line in lines {
            stored |= jar.store(line.as_ref(), CookieOp { now, ..operation });
        }
        stored
    }

    /// Sends `request` and follows HTTP redirects per
    /// [HTTP redirect fetch](https://fetch.spec.whatwg.org/#http-redirect-fetch).
    ///
    /// Status codes are response data, not errors. The URL must be `http` or `https`.
    ///
    /// # Errors
    ///
    /// [`NetError::RejectedRequest`] for a non-`http`/`https` URL, a rejected
    /// request (a failed CORS check or preflight, a redirect the CORS rules
    /// refuse, or a merged `Cookie` header that is not a valid field value), or
    /// an unusable `Location`. [`NetError::RedirectLimit`] when the redirect
    /// cap is exceeded. [`NetError::Transport`] when a hop fails.
    pub async fn send(&self, request: Request) -> Result<Response, NetError> {
        if !matches!(request.url.scheme(), "http" | "https") {
            return Err(NetError::RejectedRequest);
        }
        let dial = Dial {
            cors: &request.cors,
            client_hints: self.identity.hints(),
            initiator_kind: request.initiator_kind,
            initiator: request.initiator.as_ref(),
            on_redirect: request.on_redirect.as_ref(),
            same_site_cookie_policy: request.same_site_cookie_policy,
            partition_key: request.partition_key.as_deref(),
        };
        let mut hop = Hop {
            method: request.method,
            headers: request.headers,
            body: request.body.map(Bytes::from),
            // Once any hop is cross-origin under `cors` (or `no-cors`), every
            // later hop keeps that tainting
            // (<https://fetch.spec.whatwg.org/#main-fetch>).
            response_type: crate::cors::ResponseType::Basic,
            previous_url: None,
            origin_tainted: false,
            crossed_origin: false,
            referrer_policy: request.referrer_policy,
        };
        // Defaults every browser request carries. They live here, not in the
        // renderer, so XHR and subresources get the same values and page
        // scripts cannot override them.
        if hop.headers.value("accept").is_none() {
            hop.headers
                .append(header::ACCEPT, HeaderValue::from_static("*/*"));
        }
        if hop.headers.value("accept-language").is_none() {
            hop.headers.append(
                header::ACCEPT_LANGUAGE,
                self.identity.accept_language().clone(),
            );
        }
        let empty_body = hop.body.as_ref().is_none_or(Bytes::is_empty);
        if empty_body
            && (hop.method == Method::POST
                || hop.method == Method::PUT
                || hop.method == Method::PATCH)
            && hop.headers.value("content-length").is_none()
        {
            hop.headers
                .append(header::CONTENT_LENGTH, HeaderValue::from_static("0"));
        }
        // `Sec-Fetch-Site` is one answer for the request's whole URL list:
        // the initiator's relationship to each URL, `none` when the user agent
        // started it. `net` owns it for every request, subresources included,
        // so a page cannot write it and no hop can disagree
        // (<https://w3c.github.io/webappsec-fetch-metadata/#abstract-opdef-set-site>).
        let mut fetch_site = request
            .initiator
            .as_ref()
            .map(|origin| FetchSite::between(origin, &request.url));
        let fetch_metadata = FetchMetadata::capture(&hop.headers);
        fetch_metadata.append(&mut hop.headers, &request.url, fetch_site);
        let mut url = request.url;
        // The referrer source and policy are typed request state, and main
        // fetch runs "Determine request's Referrer" before the first hop as
        // well as on every redirect
        // (<https://fetch.spec.whatwg.org/#main-fetch>,
        // <https://w3c.github.io/webappsec-referrer-policy/#determine-requests-referrer>).
        let mut referrer = request.referrer;
        let mut referrer_policy = request.referrer_policy;
        let mut determined = determine_referrer(referrer_policy, &referrer, &url);
        Self::write_referer(&mut hop.headers, determined.as_ref())?;
        let mut followed = 0u32;
        let mut cross_site_redirect = false;
        // A redirect hop stores its own `Set-Cookie` before the next one is
        // requested, so the flag the caller sees covers the whole exchange,
        // not just the response that ended it
        // (<https://www.rfc-editor.org/rfc/rfc6265bis.html#section-5.2>).
        let mut stored_any = false;
        let mut budget = self.call_budget();
        // The request's own deadline only tightens the engine's global
        // timeout; a later deadline must not stretch it.
        if let Some(deadline) = request.deadline {
            budget.tighten_global(deadline);
        }

        loop {
            let mut response = self
                .exchange(&dial, &url, &mut hop, cross_site_redirect, budget.clone())
                .await?;
            response.redirect_count = followed;
            // The referrer this hop went out with. A navigation stores it as
            // the new document's referrer, and a redirect changes it exactly
            // the way the header changed.
            response.referrer.clone_from(&determined);
            stored_any |= response.cookies_stored();
            let hop_status = response.status();
            if self.max_redirects == 0 {
                cors_gate(&response, &dial, &hop)?;
                response.cookies_stored = stored_any;
                return Ok(response);
            }
            // A redirect response's `Referrer-Policy` field updates the
            // request's policy for the remaining hops
            // (<https://w3c.github.io/webappsec-referrer-policy/#set-requests-referrer-policy-on-redirect>).
            let redirect_policy = ReferrerPolicy::from_header(
                response
                    .internal_headers()
                    .get_all("referrer-policy")
                    .filter_map(|value| std::str::from_utf8(value).ok()),
            );
            let next = match classify_response(response, &url, &dial, &hop)? {
                HopOutcome::Final(mut response) => {
                    response.cookies_stored = stored_any;
                    return Ok(*response);
                }
                HopOutcome::Redirect { next } => next,
            };
            if let Some(policy) = redirect_policy {
                referrer_policy = policy;
                hop.referrer_policy = policy;
            }
            if followed == self.max_redirects {
                return Err(NetError::RedirectLimit);
            }
            // `followed + 1` is the redirect count the next hop will carry.
            // A false gate refuses the hop before any bytes are sent.
            if let Some(gate) = dial.on_redirect
                && !gate(&next, followed + 1)
            {
                return Err(NetError::RejectedRequest);
            }
            cross_site_redirect |= !cookies::schemeful_same_site(&url, &next);
            apply_redirect_policy(hop_status, &url, &next, &mut hop);
            fetch_site = FetchSite::widen(fetch_site, request.initiator.as_ref(), &next);
            fetch_metadata.append(&mut hop.headers, &next, fetch_site);
            // Main fetch runs "determine request's referrer" again for the
            // redirect hop, with the referrer the previous hop sent as the
            // source; a `no referrer` result stops the chain
            // (<https://fetch.spec.whatwg.org/#main-fetch>,
            // <https://w3c.github.io/webappsec-referrer-policy/#determine-requests-referrer>).
            referrer = match determined.take() {
                Some(url) => Referrer::Url(url),
                None => Referrer::NoReferrer,
            };
            determined = determine_referrer(referrer_policy, &referrer, &next);
            Self::write_referer(&mut hop.headers, determined.as_ref())?;
            url = next;
            followed += 1;
        }
    }

    /// Writes the `Referer` field from the typed determination, replacing any
    /// value left by the previous hop. A caller-supplied value never survives:
    /// the page cannot spell the header, and the browser boundary does not
    /// pass one through.
    fn write_referer(headers: &mut HeaderMap, referrer: Option<&Url>) -> Result<(), NetError> {
        match referrer {
            Some(referrer) => {
                let value = HeaderValue::from_str(referrer.as_str())
                    .map_err(|_| NetError::RejectedRequest)?;
                headers.set(header::REFERER, value);
            }
            None => headers.remove("referer"),
        }
        Ok(())
    }

    /// One HTTP hop: the response-tainting gate, the CORS request rules, the
    /// send, and the cookie store. Mutates `hop` only to acquire its CORS
    /// tainting (<https://fetch.spec.whatwg.org/#http-fetch>).
    async fn exchange(
        &self,
        dial: &Dial<'_>,
        url: &Url,
        hop: &mut Hop,
        cross_site_redirect: bool,
        budget: CallBudget,
    ) -> Result<Response, NetError> {
        apply_tainting(dial, url, hop)?;
        if budget.is_expired() {
            return Err(NetError::Transport(TransportError::Timeout(
                TimeoutKind::Global,
            )));
        }
        let budget = budget.start_hop();
        let mut wire = url.clone();
        wire.set_fragment(None);
        let cross_origin = dial.cors.origin != wire.origin();
        let operation = CookieOp {
            url,
            now: self.clock.now(),
            kind: RetrievalKind::Http,
            initiator_kind: dial.initiator_kind,
            method_is_safe: hop.method.is_safe(),
            initiator: dial.initiator,
            cross_site_redirect,
            same_site_policy: dial.same_site_cookie_policy,
            partition_key: dial.partition_key,
        };
        let mut hop_headers = hop.headers.clone();
        self.apply_cors_request(dial.cors, hop, &wire, &mut hop_headers, budget.clone())
            .await?;
        self.prepare_outbound(&mut hop_headers, operation)?;
        prepare_credentials(dial, url, cross_origin, &mut hop_headers)?;
        let send_credentials =
            crate::cors::credentials_admitted(dial.cors.credentials, cross_origin);
        let wire_response = self
            .transport
            .exchange(
                WireRequest {
                    method: hop.method.clone(),
                    url: wire,
                    headers: hop_headers,
                    body: hop.body.clone(),
                },
                budget.clone(),
            )
            .await?;
        let WireResponse {
            status,
            headers: response_headers,
            body: reader,
            http2,
        } = wire_response;
        let content_encoding: Vec<String> =
            crate::get_decode_split(&response_headers, "content-encoding")
                .into_iter()
                .flatten()
                .map(|coding| coding.to_ascii_lowercase())
                .filter(|coding| !coding.is_empty())
                .collect();
        let mut response = Response {
            status,
            status_text: reason_phrase(status, http2),
            headers: response_headers,
            final_url: url.clone(),
            body: RawBody {
                inner: reader,
                budget,
                current: None,
                offset: 0,
                shared_error: Arc::new(Mutex::new(None)),
                shared_input: Arc::new(AtomicBool::new(false)),
                timer: None,
            },
            content_encoding,
            redirect_count: 0,
            // The filtered projection is decided once, here, from the tainting
            // the hop reached and the credentials mode it was sent under
            // (<https://fetch.spec.whatwg.org/#concept-filtered-response>).
            filtering: match hop.response_type {
                crate::cors::ResponseType::Basic => crate::cors::Filtering::Basic,
                crate::cors::ResponseType::Cors => crate::cors::Filtering::Cors {
                    credentials_include: dial.credentials_include(),
                },
                crate::cors::ResponseType::Opaque => crate::cors::Filtering::Opaque,
                crate::cors::ResponseType::OpaqueRedirect => crate::cors::Filtering::OpaqueRedirect,
            },
            referrer: None,
            crossed_origin: hop.crossed_origin,
            cookies_stored: false,
        };
        // Only when the credentials mode admits cookies for this origin. The
        // jar is the only reader: the response's internal header list keeps
        // each `Set-Cookie`, and page script never sees it.
        if send_credentials {
            response.cookies_stored = self.store_set_cookie_lines(
                operation,
                response
                    .internal_headers()
                    .get_all("set-cookie")
                    .filter_map(|v| std::str::from_utf8(v).ok()),
            );
        }
        Ok(response)
    }
}

/// Decides whether a redirect hop may be followed.
pub type RedirectGate = Arc<dyn Fn(&Url, u32) -> bool + Send + Sync>;

/// Applies the sticky CORS tainting and redirect-taint rules for one hop.
///
/// Tainting never resets: a `no-cors` request that crosses an origin is opaque,
/// and a later hop back to the request's origin does not make it basic again
/// (<https://fetch.spec.whatwg.org/#concept-request-response-tainting>). Once a
/// hop crosses an origin that is neither the previous URL's nor the request's
/// own, the serialized origin becomes `null` for this and every later hop
/// (<https://fetch.spec.whatwg.org/#concept-request-tainted-origin>).
fn apply_tainting(dial: &Dial<'_>, url: &Url, hop: &mut Hop) -> Result<(), NetError> {
    let taint = hop_taint(dial.cors, url)?;
    if taint != crate::cors::ResponseType::Basic {
        hop.response_type = taint;
    }
    if url.origin() != dial.cors.origin {
        hop.crossed_origin = true;
    }
    if let Some(previous) = hop.previous_url.as_ref()
        && previous.origin() != url.origin()
        && dial.cors.origin != previous.origin()
    {
        hop.origin_tainted = true;
    }
    hop.previous_url = Some(url.clone());
    Ok(())
}

/// One outbound request as plain data. Set fields directly, then hand it to
/// [`Agent::send`].
///
/// Default [`InitiatorKind`] is [`InitiatorKind::Navigation`].
pub struct Request {
    /// Request method.
    pub method: Method,
    /// Absolute URL.
    pub url: Url,
    /// Outbound headers. [`HeaderMap::insert`] appends values with the same
    /// name (RFC 9110 §5.2); call [`HeaderMap::remove`] first to replace.
    pub headers: HeaderMap,
    /// Initiator class used for `SameSite` and (later) `Sec-Fetch-*`.
    pub initiator_kind: InitiatorKind,
    /// The request's initiator origin, or `None` when the user agent started
    /// it. `Sec-Fetch-Site`, the `SameSite` cookie context, and the request
    /// origin all read this one field, so they cannot disagree about who
    /// asked (<https://w3c.github.io/webappsec-fetch-metadata/#abstract-opdef-set-site>,
    /// <https://httpwg.org/http-extensions/draft-ietf-httpbis-rfc6265bis.html#name-same-site-requests>).
    pub initiator: Option<url::Origin>,
    /// Request body bytes. Redirects that convert to GET drop this.
    pub body: Option<Vec<u8>>,
    /// Absolute deadline for this call, covering every redirect hop.
    /// Overrides [`AgentOptions::timeout_global`]; the per-call timeout still
    /// applies hop by hop.
    pub deadline: Option<Instant>,
    /// Fetch parameters: mode, credentials, redirect handling, and origin. The
    /// caller fills `origin` from the document it is fetching for, so a
    /// compromised renderer cannot claim another origin.
    pub cors: crate::cors::CorsSettings,
    /// Called with the next URL and the redirect count that hop will carry
    /// (`1` for the first redirect) before the hop is followed. `false`
    /// rejects the dial. Unset follows every redirect the cap allows.
    pub on_redirect: Option<RedirectGate>,
    /// Fetch's request referrer: a client, an explicit URL, or no referrer
    /// (<https://fetch.spec.whatwg.org/#concept-request-referrer>).
    pub referrer: Referrer,
    /// The concrete policy main fetch resolves before the first hop. Empty
    /// policies are filled from the client's policy container, so this is
    /// never unset here.
    pub referrer_policy: ReferrerPolicy,
    /// Which `SameSite` cookies this request may send and store. A
    /// `SharedWorker` with `sameSiteCookies: "none"` sets [`SameSiteCookiePolicy::NoneOnly`]
    /// for every request its global scope makes
    /// (<https://privacycg.github.io/saa-non-cookie-storage/#shared-worker>).
    pub same_site_cookie_policy: SameSiteCookiePolicy,
    /// The site of the top-level document this request belongs to. It is the
    /// partition key for `Partitioned` cookies; `None` means the caller has no
    /// top-level site, so only unpartitioned cookies are visible and a
    /// `Partitioned` one cannot be stored
    /// (<https://github.com/privacycg/CHIPS#computing-the-cookie-partition-key>).
    pub partition_key: Option<String>,
}

impl std::fmt::Debug for Request {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Request")
            .field("method", &self.method)
            .field("url", &self.url)
            .field("headers", &self.headers)
            .field("initiator_kind", &self.initiator_kind)
            .field("initiator", &self.initiator)
            .field("body", &self.body)
            .field("deadline", &self.deadline)
            .field("cors", &self.cors)
            .field("on_redirect", &self.on_redirect.is_some())
            .field("referrer", &self.referrer)
            .field("referrer_policy", &self.referrer_policy)
            .field("same_site_cookie_policy", &self.same_site_cookie_policy)
            .field("partition_key", &self.partition_key)
            .finish()
    }
}

impl Request {
    /// A navigation request for `method` and absolute `url` with no headers,
    /// body, initiator, or deadline. Its origin starts as the target URL's
    /// origin; callers that know the initiator replace it.
    #[must_use]
    pub fn new(method: Method, url: Url) -> Self {
        let cors = crate::cors::CorsSettings {
            mode: crate::cors::RequestMode::Navigate,
            credentials: crate::cors::CredentialsMode::Include,
            redirect: crate::cors::RedirectMode::Follow,
            origin: url.origin(),
        };
        Self {
            method,
            url,
            headers: HeaderMap::new(),
            initiator_kind: InitiatorKind::default(),
            initiator: None,
            body: None,
            deadline: None,
            cors,
            on_redirect: None,
            referrer: Referrer::NoReferrer,
            referrer_policy: ReferrerPolicy::default(),
            same_site_cookie_policy: SameSiteCookiePolicy::All,
            partition_key: None,
        }
    }
}

fn followable_location(status: u16, headers: &HeaderMap) -> Option<Vec<u8>> {
    if !matches!(status, 301 | 302 | 303 | 307 | 308) {
        return None;
    }
    headers.get("location").map(<[u8]>::to_vec)
}

// <https://fetch.spec.whatwg.org/#http-redirect-fetch>
fn resolve_location(current: &Url, location: &[u8]) -> Result<Url, NetError> {
    // Location is a byte sequence, not UTF-8. Percent-encode every non-ASCII
    // byte before URL parsing so invalid UTF-8 survives as `%FF` rather than
    // failing the redirect (<https://fetch.spec.whatwg.org/#concept-location-url>).
    let location = location
        .iter()
        .map(|byte| {
            if byte.is_ascii() {
                char::from(*byte).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect::<String>();
    let mut next = current
        .join(&location)
        .map_err(|_| NetError::RejectedRequest)?;
    if !matches!(next.scheme(), "http" | "https") {
        return Err(NetError::RejectedRequest);
    }
    if next.fragment().is_none()
        && let Some(fragment) = current.fragment()
    {
        next.set_fragment(Some(fragment));
    }
    Ok(next)
}

// https://fetch.spec.whatwg.org/#http-redirect-fetch
fn apply_redirect_policy(status: u16, current: &Url, next: &Url, hop: &mut Hop) {
    let post_to_get = matches!(status, 301 | 302) && hop.method == Method::POST;
    let see_other = status == 303 && hop.method != Method::GET && hop.method != Method::HEAD;
    if post_to_get || see_other {
        hop.method = Method::GET;
        hop.body = None;
        hop.headers.remove("content-encoding");
        hop.headers.remove("content-language");
        hop.headers.remove("content-location");
        hop.headers.remove("content-type");
        hop.headers.remove("content-length");
        hop.headers.remove("transfer-encoding");
    }
    if current.origin() != next.origin() {
        hop.headers.remove("authorization");
        hop.headers.remove("cookie");
        hop.headers.remove("host");
    }
    // A hint a caller put in the request's own header list is for trustworthy
    // origins only, the same predicate the navigation headers and the cookie jar
    // use (<https://wicg.github.io/ua-client-hints/#secure-transport>). The user
    // agent's own hints are not here: this hop writes them from the identity
    // after this point, and only when the hop's URL is trustworthy
    // (<https://wicg.github.io/ua-client-hints/#http-ua-hints>).
    if !secure_context::is_potentially_trustworthy(next) {
        hop.headers.remove("sec-ch-prefers-color-scheme");
    }
}

/// What the caller does after one hop.
enum HopOutcome {
    /// The response is the final result.
    Final(Box<Response>),
    /// The response redirects; fetch `next`.
    Redirect { next: Url },
}

/// `Sec-Fetch-Site`'s three computed values, ordered from narrowest to widest
/// so a redirect chain can only widen the answer
/// (<https://w3c.github.io/webappsec-fetch-metadata/#abstract-opdef-set-site>).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum FetchSite {
    SameOrigin,
    SameSite,
    CrossSite,
}

impl FetchSite {
    /// The relationship between a request's initiator origin and one URL in
    /// its list.
    fn between(origin: &url::Origin, target: &Url) -> Self {
        let target = target.origin();
        if *origin == target {
            Self::SameOrigin
        } else if cookies::origins_same_site(origin, &target) {
            Self::SameSite
        } else {
            Self::CrossSite
        }
    }

    /// The answer for the next URL in the list. `None` is `none` and stays
    /// `none`: no site is responsible, and a redirect does not invent one. A
    /// chain that touched a wider relationship never narrows.
    fn widen(current: Option<Self>, initiator: Option<&url::Origin>, target: &Url) -> Option<Self> {
        current.map(|current| match initiator {
            Some(origin) => current.max(Self::between(origin, target)),
            None => current,
        })
    }

    const fn token(self) -> &'static str {
        match self {
            Self::SameOrigin => "same-origin",
            Self::SameSite => "same-site",
            Self::CrossSite => "cross-site",
        }
    }
}

/// Writes `Sec-Fetch-Site` for one hop, replacing any caller value. `None` is
/// the `none` token: the user agent, not a document, started the request.
fn write_fetch_site(headers: &mut HeaderMap, site: Option<FetchSite>) {
    headers.set(
        header::HeaderName::from_static("sec-fetch-site"),
        HeaderValue::from_static(site.map_or("none", FetchSite::token)),
    );
}

/// The Fetch metadata headers that describe the request itself: its mode,
/// destination, and whether user activation started it. `Sec-Fetch-Site` is not
/// here because the answer is per URL, so the request's whole list decides it.
///
/// The caller writes these where it knows the destination and mode; this
/// captures them once, and every hop appends them or drops them together
/// (<https://w3c.github.io/webappsec-fetch-metadata/#fetch-integration>).
struct FetchMetadata {
    mode: Option<HeaderValue>,
    dest: Option<HeaderValue>,
    user: Option<HeaderValue>,
}

impl FetchMetadata {
    fn capture(headers: &HeaderMap) -> Self {
        Self {
            mode: headers.value("sec-fetch-mode").cloned(),
            dest: headers.value("sec-fetch-dest").cloned(),
            user: headers.value("sec-fetch-user").cloned(),
        }
    }

    /// Applies "append the Fetch metadata headers" to one hop. All four are one
    /// group: a URL that is not potentially trustworthy carries none of them,
    /// so a destination served over plain `http` learns nothing about its
    /// requester, and a hop that redirects from `http` to `https` carries them
    /// again.
    fn append(&self, headers: &mut HeaderMap, url: &Url, site: Option<FetchSite>) {
        if !secure_context::is_potentially_trustworthy(url) {
            for name in [
                "sec-fetch-site",
                "sec-fetch-mode",
                "sec-fetch-dest",
                "sec-fetch-user",
            ] {
                headers.remove(name);
            }
            return;
        }
        write_fetch_site(headers, site);
        for (name, value) in [
            ("sec-fetch-mode", &self.mode),
            ("sec-fetch-dest", &self.dest),
            ("sec-fetch-user", &self.user),
        ] {
            match value {
                Some(value) => headers.set(header::HeaderName::from_static(name), value.clone()),
                None => headers.remove(name),
            }
        }
    }
}

/// Writes the headers one hop carries after the CORS decision: the
/// URL-credentials `Authorization`, the `Cookie` gate, and the client hints.
///
/// Credentials mode gates both the `Cookie` header and the URL-credentials
/// `Authorization` header
/// (<https://fetch.spec.whatwg.org/#concept-request-credentials-mode>,
/// <https://fetch.spec.whatwg.org/#http-network-or-cache-fetch>). The hints are
/// written here, not into the caller's header list, so they never change the
/// CORS-preflight decision
/// (<https://wicg.github.io/ua-client-hints/#http-ua-hints>).
fn prepare_credentials(
    dial: &Dial<'_>,
    url: &Url,
    cross_origin: bool,
    headers: &mut HeaderMap,
) -> Result<(), NetError> {
    if crate::cors::credentials_admitted(dial.cors.credentials, cross_origin) {
        apply_url_credentials(headers, url)?;
    } else {
        headers.remove("cookie");
    }
    dial.client_hints.write(headers, url);
    Ok(())
}

/// Request-scoped values shared by every hop of one `send` call.
struct Dial<'a> {
    cors: &'a crate::cors::CorsSettings,
    client_hints: &'a crate::ClientHints,
    initiator_kind: InitiatorKind,
    initiator: Option<&'a url::Origin>,
    on_redirect: Option<&'a RedirectGate>,
    same_site_cookie_policy: SameSiteCookiePolicy,
    /// The site of the top-level document this request belongs to, which
    /// selects the `Partitioned` cookies it may send and store.
    partition_key: Option<&'a str>,
}

impl Dial<'_> {
    /// Whether the credentials mode includes credentials, for the CORS check
    /// and response filtering
    /// (<https://fetch.spec.whatwg.org/#concept-request-credentials-mode>).
    fn credentials_include(&self) -> bool {
        self.cors.credentials.includes()
    }
}

/// The request state a redirect may rewrite, plus the sticky CORS tainting.
struct Hop {
    method: Method,
    headers: HeaderMap,
    body: Option<Bytes>,
    response_type: crate::cors::ResponseType,
    /// The previous hop's URL, for the redirect-taint computation
    /// (<https://fetch.spec.whatwg.org/#concept-request-tainted-origin>).
    previous_url: Option<Url>,
    /// Whether the serialized request origin has collapsed to `null` because a
    /// hop crossed an origin that is neither the previous URL's nor the
    /// request's own
    /// (<https://fetch.spec.whatwg.org/#serializing-a-request-origin>).
    origin_tainted: bool,
    /// Whether any hop's URL origin differs from the request's origin. A
    /// `no-cors` request that crosses it gets an opaque response, and the
    /// tainting never resets, so a chain that returns to the request's origin
    /// stays opaque
    /// (<https://fetch.spec.whatwg.org/#concept-request-response-tainting>).
    crossed_origin: bool,
    /// The request's referrer policy, which decides whether a non-CORS request
    /// with a body reports its origin or `null`
    /// (<https://fetch.spec.whatwg.org/#append-a-request-origin-header>). A
    /// redirect's `Referrer-Policy` field updates it for the remaining hops
    /// (<https://w3c.github.io/webappsec-referrer-policy/#set-requests-referrer-policy-on-redirect>).
    referrer_policy: ReferrerPolicy,
}

impl Hop {
    /// The byte-serialized request origin CORS compares, `null` once a redirect
    /// has tainted the request
    /// (<https://fetch.spec.whatwg.org/#serializing-a-request-origin>). The
    /// request `Origin` header and the response CORS check both read it here.
    fn serialized_origin<'a>(&self, settings: &'a crate::cors::CorsSettings) -> Cow<'a, str> {
        if self.origin_tainted {
            Cow::Borrowed("null")
        } else {
            Cow::Owned(settings.origin.ascii_serialization())
        }
    }

    /// The value "append a request `Origin` header" writes for this hop
    /// (<https://fetch.spec.whatwg.org/#append-a-request-origin-header>).
    ///
    /// A `cors`-tainted hop, and any `GET` or `HEAD`, always names the origin.
    /// Every other request can be reported as `null`, and which policies do
    /// that is the referrer policy's decision: `no-referrer` always,
    /// `same-origin` across an origin boundary, and the three downgrade-aware
    /// policies when an `https` origin reaches a non-`https` URL.
    fn append_origin<'a>(
        &self,
        settings: &'a crate::cors::CorsSettings,
        url: &Url,
    ) -> Cow<'a, str> {
        let origin = self.serialized_origin(settings);
        if origin == "null"
            || self.response_type == crate::cors::ResponseType::Cors
            || matches!(self.method.as_str(), "GET" | "HEAD")
        {
            return origin;
        }
        let null = match self.referrer_policy {
            ReferrerPolicy::NoReferrer => true,
            ReferrerPolicy::NoReferrerWhenDowngrade
            | ReferrerPolicy::StrictOrigin
            | ReferrerPolicy::StrictOriginWhenCrossOrigin => {
                crate::origin_url(&settings.origin).is_some_and(|origin| origin.scheme() == "https")
                    && url.scheme() != "https"
            }
            ReferrerPolicy::SameOrigin => settings.origin != url.origin(),
            ReferrerPolicy::Origin
            | ReferrerPolicy::OriginWhenCrossOrigin
            | ReferrerPolicy::UnsafeUrl => false,
        };
        if null { Cow::Borrowed("null") } else { origin }
    }
}

/// The response tainting a hop acquires
/// (<https://fetch.spec.whatwg.org/#concept-request-response-tainting>). A
/// `same-origin` request across an origin boundary is a network error.
fn hop_taint(
    settings: &crate::cors::CorsSettings,
    url: &Url,
) -> Result<crate::cors::ResponseType, NetError> {
    if settings.origin == url.origin() {
        return Ok(crate::cors::ResponseType::Basic);
    }
    match settings.mode {
        crate::cors::RequestMode::Navigate => Ok(crate::cors::ResponseType::Basic),
        crate::cors::RequestMode::SameOrigin => Err(NetError::RejectedRequest),
        crate::cors::RequestMode::Cors => Ok(crate::cors::ResponseType::Cors),
        // A cross-origin `no-cors` request is opaque. A non-following
        // redirect mode is decided when a 3xx response arrives, not before the
        // request is sent: main fetch would refuse it up front, but CSP's
        // `report-uri` delivery is a `no-cors` request whose redirect mode is
        // `"error"`, and Chromium sends it anyway (docs/upstream.md). A
        // `manual` request would hand page script an opaque-redirect response
        // that main fetch forbids, so that one still fails before sending
        // (<https://fetch.spec.whatwg.org/#main-fetch>).
        crate::cors::RequestMode::NoCors => match settings.redirect {
            crate::cors::RedirectMode::Manual => Err(NetError::RejectedRequest),
            crate::cors::RedirectMode::Follow | crate::cors::RedirectMode::Error => {
                Ok(crate::cors::ResponseType::Opaque)
            }
        },
    }
}

/// The page's view of one response
/// (<https://fetch.spec.whatwg.org/#concept-filtered-response>).
///
/// Every accessor returns the filtered value: an opaque response has status 0,
/// an empty status message, no header list, and no URL, so a caller cannot
/// reach the internal response through this type.
pub struct FilteredResponse<'a> {
    response: &'a Response,
}

impl<'a> FilteredResponse<'a> {
    /// The filtered status code: 0 for an opaque response.
    #[must_use]
    pub fn status(&self) -> u16 {
        if self.response.is_opaque() {
            0
        } else {
            self.response.status
        }
    }

    /// The filtered status message: empty for an opaque response.
    #[must_use]
    pub fn status_text(&self) -> &'a str {
        if self.response.is_opaque() {
            ""
        } else {
            &self.response.status_text
        }
    }

    /// The filtered URL, or `None` for an opaque response, whose URL list is
    /// empty
    /// (<https://fetch.spec.whatwg.org/#concept-filtered-response-opaque>).
    #[must_use]
    pub fn url(&self) -> Option<&'a Url> {
        if self.response.filtering == crate::cors::Filtering::Opaque {
            None
        } else {
            Some(&self.response.final_url)
        }
    }

    /// How the response was filtered.
    #[must_use]
    pub fn response_type(&self) -> crate::cors::ResponseType {
        self.response.response_type()
    }

    /// The header list page script may read, in wire order: a `basic` response
    /// keeps every field but the forbidden response-header names, a `cors`
    /// response keeps its safelisted and explicitly exposed fields, and an
    /// opaque response keeps none
    /// (<https://fetch.spec.whatwg.org/#concept-filtered-response>).
    ///
    /// The entries borrow the internal map; nothing is cloned.
    #[must_use]
    pub fn headers(&self) -> Vec<(&'a str, &'a [u8])> {
        match self.response.filtering {
            crate::cors::Filtering::Basic => self
                .response
                .headers
                .iter()
                .filter(|(name, _)| !crate::cors::is_forbidden_response_header(name))
                .collect(),
            crate::cors::Filtering::Cors {
                credentials_include,
            } => {
                let rule =
                    crate::cors::ExposedHeaders::new(&self.response.headers, credentials_include);
                self.response
                    .headers
                    .iter()
                    .filter(|(name, _)| rule.keeps(name))
                    .collect()
            }
            crate::cors::Filtering::Opaque | crate::cors::Filtering::OpaqueRedirect => Vec::new(),
        }
    }
}

/// The CORS check for a `cors`-tainted response
/// (<https://fetch.spec.whatwg.org/#cors-check>). Every hop, redirect or
/// final, must pass it.
fn cors_gate(response: &Response, dial: &Dial<'_>, hop: &Hop) -> Result<(), NetError> {
    if hop.response_type != crate::cors::ResponseType::Cors {
        return Ok(());
    }
    let origin = hop.serialized_origin(dial.cors);
    if !crate::cors::cors_check(
        response.internal_headers(),
        &origin,
        dial.credentials_include(),
    ) {
        return Err(NetError::RejectedRequest);
    }
    Ok(())
}

/// Applies the response-side CORS rules for one hop, in spec order: the CORS
/// check, then the redirect handling (the credentials rule, the redirect
/// mode, and the final filtering)
/// (<https://fetch.spec.whatwg.org/#http-fetch>,
/// <https://fetch.spec.whatwg.org/#http-redirect-fetch>).
fn classify_response(
    response: Response,
    current_url: &Url,
    dial: &Dial<'_>,
    hop: &Hop,
) -> Result<HopOutcome, NetError> {
    cors_gate(&response, dial, hop)?;
    let settings = dial.cors;
    // Manual mode never validates or follows Location; the response is an
    // opaque-redirect filtered response
    // (<https://fetch.spec.whatwg.org/#http-redirect-fetch>).
    if settings.redirect == crate::cors::RedirectMode::Manual
        && matches!(response.status(), 301 | 302 | 303 | 307 | 308)
    {
        let mut response = response;
        response.filtering = crate::cors::Filtering::OpaqueRedirect;
        return Ok(HopOutcome::Final(Box::new(response)));
    }
    let Some(location) = followable_location(response.status(), response.internal_headers()) else {
        return Ok(HopOutcome::Final(Box::new(response)));
    };
    let next = resolve_location(current_url, &location)?;
    // A redirect to a URL that includes credentials is a network error
    // once the request is `cors`-tainted or would cross an origin
    // (<https://fetch.spec.whatwg.org/#http-redirect-fetch>, steps 9-10).
    let redirects_with_credentials = !next.username().is_empty() || next.password().is_some();
    if redirects_with_credentials
        && (hop.response_type == crate::cors::ResponseType::Cors
            || (settings.mode == crate::cors::RequestMode::Cors
                && settings.origin != next.origin()))
    {
        return Err(NetError::RejectedRequest);
    }
    match settings.redirect {
        crate::cors::RedirectMode::Error => {
            return Err(NetError::RejectedRequest);
        }
        // Manual returned above; unreachable here.
        crate::cors::RedirectMode::Manual | crate::cors::RedirectMode::Follow => {}
    }
    Ok(HopOutcome::Redirect { next })
}

/// The reason phrase one status code carries, or an empty string over HTTP/2,
/// which does not send one
/// (<https://fetch.spec.whatwg.org/#concept-response-status-message>).
fn reason_phrase(status: u16, http2: bool) -> String {
    if http2 {
        return String::new();
    }
    hyper::StatusCode::from_u16(status)
        .ok()
        .and_then(|status| status.canonical_reason())
        .unwrap_or("")
        .to_owned()
}

fn apply_url_credentials(headers: &mut HeaderMap, url: &Url) -> Result<(), NetError> {
    // https://fetch.spec.whatwg.org/#http-network-or-cache-fetch
    if headers.value("authorization").is_some() || url.username().is_empty() {
        return Ok(());
    }
    let username = percent_decode_str(url.username()).decode_utf8_lossy();
    let password = url
        .password()
        .map_or_default(|password| percent_decode_str(password).decode_utf8_lossy());
    let value = basic_authorization(&username, &password);
    let value = HeaderValue::from_str(&value).map_err(|_| NetError::RejectedRequest)?;
    headers.set(header::AUTHORIZATION, value);
    Ok(())
}

/// Streaming response body. Dropping it closes the socket.
pub struct Body {
    reader: Box<dyn AsyncRead + Send + Unpin>,
    shared_error: Arc<Mutex<Option<NetError>>>,
    shared_input: Arc<AtomicBool>,
}

impl std::fmt::Debug for Body {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Body").finish_non_exhaustive()
    }
}

impl Body {
    /// Wraps the raw socket body in the decoders named by `Content-Encoding`.
    async fn decoded(raw: RawBody, codings: &[String]) -> Self {
        let shared_error = Arc::clone(&raw.shared_error);
        let shared_input = Arc::clone(&raw.shared_input);
        Self::wrap(Box::new(raw), codings, shared_error, shared_input).await
    }

    async fn wrap(
        mut reader: Box<dyn AsyncRead + Send + Unpin>,
        codings: &[String],
        shared_error: Arc<Mutex<Option<NetError>>>,
        shared_input: Arc<AtomicBool>,
    ) -> Self {
        for coding in codings.iter().rev() {
            match coding.as_str() {
                "identity" => {}
                "gzip" | "x-gzip" => {
                    let mut decoder = GzipDecoder::new(BufReader::new(reader));
                    // A gzip response may carry several members
                    // (<https://www.rfc-editor.org/rfc/rfc7230#section-4.1.3>).
                    decoder.multiple_members(true);
                    reader = Box::new(decoder);
                }
                "br" => {
                    reader = Box::new(BrotliDecoder::new(BufReader::new(reader)));
                }
                "deflate" => {
                    let mut buffered = BufReader::new(reader);
                    let zlib = match buffered.fill_buf().await {
                        Ok(header) => {
                            header.len() >= 2
                                && (header[0] & 0x0f) == 8
                                && (u16::from(header[0]) << 8 | u16::from(header[1])) % 31 == 0
                        }
                        Err(error) => {
                            set_body_error(
                                &shared_error,
                                NetError::Decode(error.to_string().into()),
                            );
                            false
                        }
                    };
                    reader = if zlib {
                        Box::new(ZlibDecoder::new(buffered))
                    } else {
                        Box::new(DeflateDecoder::new(buffered))
                    };
                }
                // Unsupported coding: leave the remaining bytes untouched
                // (<https://fetch.spec.whatwg.org/#concept-http-network-fetch>).
                _ => break,
            }
        }
        Self {
            reader,
            shared_error,
            shared_input,
        }
    }

    /// Next decoded chunk, or `None` at end of body.
    ///
    /// # Errors
    ///
    /// [`NetError::Transport`] when the socket read fails or the deadline
    /// expires; [`NetError::Decode`] when a decoder rejects the bytes. The
    /// carrier's byte budget is enforced by whoever reads the chunks, so it is
    /// not an error here.
    pub async fn read_chunk(&mut self) -> Result<Option<Vec<u8>>, NetError> {
        let mut buffer = vec![0; 16 * 1024];
        match self.reader.read(&mut buffer).await {
            Ok(0) => Ok(None),
            Ok(read) => {
                buffer.truncate(read);
                Ok(Some(buffer))
            }
            Err(error) => {
                let shared = self
                    .shared_error
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take();
                if shared.is_none()
                    && error.kind() == std::io::ErrorKind::UnexpectedEof
                    && !self.shared_input.load(Ordering::Relaxed)
                {
                    return Ok(None);
                }
                Err(shared.unwrap_or_else(|| NetError::Decode(error.to_string().into())))
            }
        }
    }
}

/// Raw socket body before content decoders.
struct RawBody {
    inner: WireBody,
    budget: HopBudget,
    current: Option<Bytes>,
    offset: usize,
    shared_error: Arc<Mutex<Option<NetError>>>,
    shared_input: Arc<AtomicBool>,
    timer: Option<Pin<Box<tokio::time::Sleep>>>,
}

impl std::fmt::Debug for RawBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RawBody").finish_non_exhaustive()
    }
}

impl AsyncRead for RawBody {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        if let Some(deadline) = this.budget.deadline() {
            let timer = this
                .timer
                .get_or_insert_with(|| Box::pin(tokio::time::sleep_until(deadline.into())));
            if timer.as_mut().poll(cx).is_ready() {
                set_body_error(
                    &this.shared_error,
                    NetError::Transport(TransportError::Timeout(
                        this.budget.timeout_kind(TimeoutKind::RecvBody),
                    )),
                );
                return Poll::Ready(Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "body read timed out",
                )));
            }
        }
        loop {
            if let Some(current) = this.current.as_ref() {
                if this.offset < current.len() {
                    let remaining = &current[this.offset..];
                    let take = remaining.len().min(buffer.remaining());
                    buffer.put_slice(&remaining[..take]);
                    this.offset += take;
                    return Poll::Ready(Ok(()));
                }
                this.current = None;
                this.offset = 0;
            }
            match Pin::new(&mut this.inner).poll_frame(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Some(Ok(frame))) => {
                    if let Ok(data) = frame.into_data() {
                        this.shared_input.store(true, Ordering::Relaxed);
                        this.current = Some(data);
                        this.offset = 0;
                    }
                }
                Poll::Ready(Some(Err(error))) => {
                    set_body_error(&this.shared_error, error);
                    return Poll::Ready(Err(std::io::Error::other("body read failed")));
                }
                Poll::Ready(None) => return Poll::Ready(Ok(())),
            }
        }
    }
}

fn set_body_error(slot: &Mutex<Option<NetError>>, error: NetError) {
    let mut slot = slot.lock().unwrap_or_else(PoisonError::into_inner);
    if slot.is_none() {
        *slot = Some(error);
    }
}

/// HTTP response after redirects. Status codes are data, including 4xx and 5xx.
#[derive(Debug)]
pub struct Response {
    status: u16,
    status_text: String,
    headers: HeaderMap,
    final_url: Url,
    body: RawBody,
    content_encoding: Vec<String>,
    redirect_count: u32,
    filtering: crate::cors::Filtering,
    referrer: Option<Url>,
    crossed_origin: bool,
    /// Whether the jar stored a cookie from this response's `Set-Cookie`
    /// fields.
    cookies_stored: bool,
}

impl Response {
    /// HTTP status code.
    #[must_use]
    pub fn status(&self) -> u16 {
        self.status
    }

    /// HTTP reason phrase, for the `Response.statusText` getter.
    #[must_use]
    pub fn status_text(&self) -> &str {
        &self.status_text
    }

    /// How the response was filtered for page script: basic, CORS, or opaque
    /// (<https://fetch.spec.whatwg.org/#concept-response-type>).
    #[must_use]
    pub fn response_type(&self) -> crate::cors::ResponseType {
        self.filtering.response_type()
    }

    /// Whether the filtered projection is opaque: no status, URL, headers, or
    /// body reach page script
    /// (<https://fetch.spec.whatwg.org/#concept-filtered-response-opaque>).
    #[must_use]
    pub fn is_opaque(&self) -> bool {
        self.filtering.is_opaque()
    }

    /// Whether any hop's URL origin differed from the request's origin, which
    /// is what a `no-cors` request turns into an opaque response
    /// (<https://fetch.spec.whatwg.org/#concept-request-response-tainting>).
    #[must_use]
    pub fn crossed_origin(&self) -> bool {
        self.crossed_origin
    }

    /// The internal response's header list, before any filtering: every field
    /// the server sent, `Set-Cookie` included.
    ///
    /// A page `fetch`/XHR reports [`Response::filtered`] instead. An internal
    /// consumer — an image decoder, a script compiler, a style sheet parser —
    /// reads this one, because the spec's own algorithms read the unsafe
    /// response, not the filtered one
    /// (<https://fetch.spec.whatwg.org/#concept-internal-response>).
    #[must_use]
    pub fn internal_headers(&self) -> &HeaderMap {
        &self.headers
    }

    /// The filtered response page script may observe: its status, URL, headers,
    /// and redirect flag already projected through [`Filtering`](crate::cors::Filtering). The internal
    /// response stays out of reach through this type
    /// (<https://fetch.spec.whatwg.org/#concept-filtered-response>).
    #[must_use]
    pub fn filtered(&self) -> FilteredResponse<'_> {
        FilteredResponse { response: self }
    }

    /// URL after redirects. Fragments from the original request are preserved.
    #[must_use]
    pub fn final_url(&self) -> &Url {
        &self.final_url
    }

    /// Number of redirect hops followed for this response.
    #[must_use]
    pub fn redirect_count(&self) -> u32 {
        self.redirect_count
    }

    /// The stripped referrer this response's final hop was sent with, after
    /// `Determine request's Referrer` ran for that hop. A navigation stores it
    /// as the new document's own referrer
    /// (<https://html.spec.whatwg.org/multipage/document-lifecycle.html#creating-and-initializing-a-document-object>).
    #[must_use]
    pub fn referrer(&self) -> Option<&Url> {
        self.referrer.as_ref()
    }

    /// Whether the jar stored a cookie from this response's `Set-Cookie`
    /// fields.
    #[must_use]
    pub fn cookies_stored(&self) -> bool {
        self.cookies_stored
    }

    /// Consumes the response and returns its decoded body stream.
    ///
    /// This is the *internal* response's body. A page `fetch` gets it only when
    /// [`Filtering::is_opaque`](crate::cors::Filtering::is_opaque) is false; an
    /// opaque filtered response has a null body
    /// (<https://fetch.spec.whatwg.org/#concept-filtered-response-opaque>).
    pub async fn into_body(self) -> Body {
        Body::decoded(self.body, &self.content_encoding).await
    }
}

#[cfg(test)]
mod redirect_policy_tests {
    use super::{
        FetchSite, HeaderMap, Hop, Method, ReferrerPolicy, Url, apply_redirect_policy,
        write_fetch_site,
    };
    use crate::cors::ResponseType;

    fn url(value: &str) -> Url {
        Url::parse(value).expect("url")
    }

    fn hop_with_hint() -> Hop {
        let mut headers = HeaderMap::new();
        headers
            .insert("sec-ch-prefers-color-scheme", b"dark")
            .expect("hint header");
        Hop {
            method: Method::GET,
            headers,
            body: None,
            response_type: ResponseType::Basic,
            previous_url: None,
            origin_tainted: false,
            crossed_origin: false,
            referrer_policy: ReferrerPolicy::default(),
        }
    }

    /// One hop of a cross-origin `POST` whose request carries `policy` as its
    /// referrer policy, for the `Origin` append rule
    /// (<https://fetch.spec.whatwg.org/#append-a-request-origin-header>).
    fn posting_hop(policy: ReferrerPolicy) -> (Hop, crate::cors::CorsSettings) {
        let origin = Url::parse("http://a.test/page").expect("origin").origin();
        let hop = Hop {
            method: Method::POST,
            headers: HeaderMap::new(),
            body: None,
            response_type: ResponseType::Basic,
            previous_url: None,
            origin_tainted: false,
            crossed_origin: false,
            referrer_policy: policy,
        };
        let settings = crate::cors::CorsSettings {
            mode: crate::cors::RequestMode::NoCors,
            credentials: crate::cors::CredentialsMode::Include,
            redirect: crate::cors::RedirectMode::Follow,
            origin,
        };
        (hop, settings)
    }

    /// A non-CORS `POST` reports `null` where the request's referrer policy
    /// hides the origin, and the serialized origin everywhere else
    /// (<https://fetch.spec.whatwg.org/#append-a-request-origin-header>).
    #[test]
    fn a_non_cors_post_origin_follows_the_referrer_policy() {
        let cross = url("http://www1.test/resource");
        let same = url("http://a.test/resource");
        let cases = [
            (ReferrerPolicy::SameOrigin, &cross, "null"),
            (ReferrerPolicy::SameOrigin, &same, "http://a.test"),
            (ReferrerPolicy::NoReferrer, &same, "null"),
            (ReferrerPolicy::Origin, &cross, "http://a.test"),
            (
                ReferrerPolicy::StrictOriginWhenCrossOrigin,
                &cross,
                "http://a.test",
            ),
            (ReferrerPolicy::UnsafeUrl, &cross, "http://a.test"),
        ];
        for (policy, target, expected) in cases {
            let (hop, settings) = posting_hop(policy);
            assert_eq!(
                hop.append_origin(&settings, target),
                expected,
                "policy {policy:?} to {target}"
            );
        }
    }

    /// A caller-supplied client hint is dropped when the redirect hop is not
    /// potentially trustworthy (<https://wicg.github.io/ua-client-hints/#secure-transport>).
    #[test]
    fn a_redirect_to_an_untrustworthy_origin_drops_the_client_hint() {
        let mut hop = hop_with_hint();
        apply_redirect_policy(
            302,
            &url("http://a.test/"),
            &url("http://b.test/"),
            &mut hop,
        );
        assert_eq!(hop.headers.get("sec-ch-prefers-color-scheme"), None);
    }

    /// The first hop's token names the relationship between the request's
    /// initiator and the target: origin first, then site, then nothing shared
    /// (<https://w3c.github.io/webappsec-fetch-metadata/#abstract-opdef-set-site>).
    #[test]
    fn fetch_site_names_the_first_hop_relationship() {
        let origin = url("https://a.test/page").origin();
        assert_eq!(
            FetchSite::between(&origin, &url("https://a.test/other")),
            FetchSite::SameOrigin
        );
        assert_eq!(
            FetchSite::between(&origin, &url("https://sub.a.test/")),
            FetchSite::SameSite
        );
        assert_eq!(
            FetchSite::between(&origin, &url("https://b.test/")),
            FetchSite::CrossSite
        );
        assert_eq!(
            FetchSite::between(&origin, &url("http://a.test/")),
            FetchSite::CrossSite,
            "a scheme change is a site change"
        );
        assert_eq!(
            FetchSite::between(&url("about:blank").origin(), &url("https://a.test/")),
            FetchSite::CrossSite,
            "an opaque origin is not same-site with anything"
        );
    }

    /// `Sec-Fetch-Site` covers the whole URL list, so a chain widens and never
    /// narrows: a redirect back to the request's origin keeps the worst
    /// relationship any hop had.
    #[test]
    fn fetch_site_widens_along_a_redirect_chain() {
        let origin = url("https://a.test/page").origin();
        let mut site = Some(FetchSite::SameOrigin);
        site = FetchSite::widen(site, Some(&origin), &url("https://b.a.test/"));
        assert_eq!(site, Some(FetchSite::SameSite));
        site = FetchSite::widen(site, Some(&origin), &url("https://c.test/"));
        assert_eq!(site, Some(FetchSite::CrossSite));
        site = FetchSite::widen(site, Some(&origin), &url("https://a.test/back"));
        assert_eq!(
            site,
            Some(FetchSite::CrossSite),
            "a same-origin hop cannot narrow the chain back"
        );
    }

    /// A request the user agent started keeps `none`: no site is responsible,
    /// and no redirect gives one a site to be responsible for. Whichever value
    /// the request had, `net` writes the header itself.
    #[test]
    fn fetch_site_none_survives_redirects_and_is_always_written() {
        let site = FetchSite::widen(None, None, &url("https://b.test/"));
        assert_eq!(site, None);
        let mut headers = HeaderMap::new();
        write_fetch_site(&mut headers, site);
        assert_eq!(headers.get("sec-fetch-site"), Some(b"none".as_slice()));

        let mut headers = HeaderMap::new();
        headers
            .insert("sec-fetch-site", "cross-site")
            .expect("header");
        write_fetch_site(&mut headers, Some(FetchSite::SameOrigin));
        assert_eq!(
            headers.get("sec-fetch-site"),
            Some(b"same-origin".as_slice()),
            "a caller value never survives the computed answer"
        );
    }
}

#[cfg(test)]
mod decode_tests {
    use super::{Arc, AtomicBool, Body, Mutex, NetError};
    use flate2::Compression;
    use flate2::write::{DeflateEncoder, GzEncoder};
    use std::io::Write as _;
    use tokio::io::{AsyncWriteExt as _, duplex};

    async fn read_all(mut body: Body) -> Result<Vec<u8>, NetError> {
        let mut out = Vec::new();
        while let Some(chunk) = body.read_chunk().await? {
            out.extend_from_slice(&chunk);
        }
        Ok(out)
    }

    async fn decode(bytes: &[u8], coding: &str) -> Result<Vec<u8>, NetError> {
        let (mut writer, reader) = duplex(bytes.len().saturating_add(32).max(64));
        writer.write_all(bytes).await.expect("write");
        drop(writer);
        let body = Body::wrap(
            Box::new(reader),
            &[coding.to_owned()],
            Arc::new(Mutex::new(None)),
            Arc::new(AtomicBool::new(true)),
        )
        .await;
        read_all(body).await
    }

    fn gzip(data: &[u8]) -> Vec<u8> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(data).expect("gzip");
        encoder.finish().expect("gzip finish")
    }

    #[tokio::test]
    async fn decodes_every_gzip_member() {
        let mut body = gzip(b"one");
        body.extend(gzip(b"two"));
        assert_eq!(decode(&body, "gzip").await.expect("gzip"), b"onetwo");
    }

    #[tokio::test]
    async fn decodes_raw_deflate() {
        let mut encoder = DeflateEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(b"deflated").expect("deflate");
        let body = encoder.finish().expect("deflate finish");
        assert_eq!(
            decode(&body, "deflate").await.expect("deflate"),
            b"deflated"
        );
    }

    #[tokio::test]
    async fn leaves_unsupported_coding_untouched() {
        assert_eq!(
            decode(b"zstd bytes", "zstd").await.expect("passthrough"),
            b"zstd bytes"
        );
    }

    #[tokio::test]
    async fn empty_body_is_not_a_decode_error() {
        let body = Body::wrap(
            Box::new(tokio::io::empty()),
            &["gzip".to_owned()],
            Arc::new(Mutex::new(None)),
            Arc::new(AtomicBool::new(false)),
        )
        .await;
        assert_eq!(read_all(body).await.expect("empty"), Vec::<u8>::new());
    }
}

#[cfg(test)]
mod transport_port_tests {
    //! The fetch core must not depend on hyper: a host supplies only the one
    //! exchange, and every policy step still runs above it (#233).

    use std::future::Future;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex, PoisonError};

    use bytes::Bytes;
    use http_body_util::BodyExt as _;
    use url::Url;

    use super::{Agent, AgentOptions, Method, NetError, Request};
    use crate::cors::{CorsSettings, CredentialsMode, RedirectMode, RequestMode, ResponseType};
    use crate::transport::{HopBudget, Transport, WireRequest, WireResponse};

    /// A transport that records the request it was handed and answers with one
    /// header.
    struct FakeTransport {
        allow_origin: Option<&'static str>,
        seen: Arc<Mutex<Vec<(String, String)>>>,
    }

    impl Transport for FakeTransport {
        fn exchange(
            &self,
            request: WireRequest,
            _budget: HopBudget,
        ) -> Pin<Box<dyn Future<Output = Result<WireResponse, NetError>> + Send + '_>> {
            let allow_origin = self.allow_origin;
            let seen = Arc::clone(&self.seen);
            Box::pin(async move {
                let mut recorded = seen.lock().unwrap_or_else(PoisonError::into_inner);
                for (name, value) in request.headers.iter() {
                    recorded.push((
                        name.to_ascii_lowercase(),
                        String::from_utf8_lossy(value).into_owned(),
                    ));
                }
                drop(recorded);
                let mut headers = crate::HeaderMap::new();
                if let Some(origin) = allow_origin {
                    headers
                        .insert("access-control-allow-origin", origin)
                        .expect("a static header value");
                }
                let body = http_body_util::Full::new(Bytes::from_static(b"body"))
                    .map_err(|infallible| match infallible {})
                    .boxed_unsync();
                Ok(WireResponse {
                    status: 200,
                    headers,
                    body,
                    http2: false,
                })
            })
        }
    }

    fn cross_origin_request() -> Request {
        let initiator = Url::parse("http://a.test/page")
            .expect("initiator")
            .origin();
        let mut request = Request::new(
            Method::GET,
            Url::parse("http://b.test/resource").expect("target"),
        );
        request.cors = CorsSettings {
            mode: RequestMode::Cors,
            credentials: CredentialsMode::SameOrigin,
            redirect: RedirectMode::Follow,
            origin: initiator.clone(),
        };
        request.initiator = Some(initiator);
        request
    }

    /// A `cors` response without `Access-Control-Allow-Origin` is a network
    /// error even when the bytes came from a non-hyper transport
    /// (<https://fetch.spec.whatwg.org/#cors-check>).
    #[tokio::test]
    async fn a_cross_origin_cors_response_without_acao_is_rejected() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let agent = Agent::with_transport(
            AgentOptions::default(),
            Arc::new(FakeTransport {
                allow_origin: None,
                seen: Arc::clone(&seen),
            }),
        );
        let error = agent
            .send(cross_origin_request())
            .await
            .expect_err("no ACAO");
        assert!(matches!(error, NetError::RejectedRequest));
        let seen = seen.lock().unwrap_or_else(PoisonError::into_inner);
        assert!(
            seen.iter().all(|(name, _)| !name.starts_with("sec-fetch-")),
            "a plain-http target gets no Fetch metadata headers" // <https://w3c.github.io/webappsec-fetch-metadata/#fetch-integration>
        );
        assert_eq!(
            seen.iter()
                .find(|(name, _)| name == "origin")
                .map(|(_, value)| value.as_str()),
            Some("http://a.test"),
            "the core writes Origin for a cors request"
        );
    }

    /// The same exchange with an approving header reaches the caller as a
    /// `cors` response.
    #[tokio::test]
    async fn a_cross_origin_cors_response_with_acao_is_cors_typed() {
        let agent = Agent::with_transport(
            AgentOptions::default(),
            Arc::new(FakeTransport {
                allow_origin: Some("*"),
                seen: Arc::new(Mutex::new(Vec::new())),
            }),
        );
        let response = agent
            .send(cross_origin_request())
            .await
            .expect("ACAO admits the response");
        assert_eq!(response.status(), 200);
        assert_eq!(response.response_type(), ResponseType::Cors);
    }

    /// One scripted response: status plus static headers.
    type ScriptedResponse = (u16, Vec<(&'static str, &'static str)>);

    /// A scripted transport: one response per exchange, in order.
    struct ScriptedTransport {
        script: Mutex<std::collections::VecDeque<ScriptedResponse>>,
        seen: Arc<Mutex<Vec<(String, String)>>>,
    }

    impl Transport for ScriptedTransport {
        fn exchange(
            &self,
            request: WireRequest,
            _budget: HopBudget,
        ) -> Pin<Box<dyn Future<Output = Result<WireResponse, NetError>> + Send + '_>> {
            self.seen
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((request.method.as_str().to_owned(), request.url.to_string()));
            let (status, scripted) = self
                .script
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .pop_front()
                .expect("one scripted response per exchange");
            Box::pin(async move {
                let mut headers = crate::HeaderMap::new();
                for (name, value) in scripted {
                    headers.insert(name, value).expect("a static header value");
                }
                let body = http_body_util::Full::new(Bytes::from_static(b"body"))
                    .map_err(|infallible| match infallible {})
                    .boxed_unsync();
                Ok(WireResponse {
                    status,
                    headers,
                    body,
                    http2: false,
                })
            })
        }
    }

    /// A redirect runs a second exchange through the same port, and the final
    /// URL and count reach the caller
    /// (<https://fetch.spec.whatwg.org/#http-redirect-fetch>).
    #[tokio::test]
    async fn a_redirect_crosses_the_same_port() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let agent = Agent::with_transport(
            AgentOptions::default(),
            Arc::new(ScriptedTransport {
                script: Mutex::new(std::collections::VecDeque::from([
                    (302, vec![("location", "http://b.test/final")]),
                    (200, vec![]),
                ])),
                seen: Arc::clone(&seen),
            }),
        );
        let response = agent
            .send(Request::new(
                Method::GET,
                Url::parse("http://b.test/start").expect("start URL"),
            ))
            .await
            .expect("redirect followed");
        assert_eq!(response.final_url().as_str(), "http://b.test/final");
        assert_eq!(response.redirect_count(), 1);
        let seen = seen.lock().unwrap_or_else(PoisonError::into_inner);
        assert_eq!(seen.len(), 2, "one exchange per hop");
        assert_eq!(seen[0].1, "http://b.test/start");
        assert_eq!(seen[1].1, "http://b.test/final");
    }

    /// A non-safelisted cross-origin request sends its preflight through the
    /// same port before the actual hop
    /// (<https://fetch.spec.whatwg.org/#cors-preflight-fetch>).
    #[tokio::test]
    async fn a_cors_preflight_uses_the_same_port() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let agent = Agent::with_transport(
            AgentOptions::default(),
            Arc::new(ScriptedTransport {
                script: Mutex::new(std::collections::VecDeque::from([
                    (
                        204,
                        vec![
                            ("access-control-allow-origin", "http://a.test"),
                            ("access-control-allow-methods", "PUT"),
                            ("access-control-allow-headers", "x-custom"),
                        ],
                    ),
                    (200, vec![("access-control-allow-origin", "http://a.test")]),
                ])),
                seen: Arc::clone(&seen),
            }),
        );
        let mut request = cross_origin_request();
        request.method = Method::PUT;
        request
            .headers
            .insert("x-custom", "1")
            .expect("a test header");
        let response = agent.send(request).await.expect("preflight then request");
        assert_eq!(response.status(), 200);
        let seen = seen.lock().unwrap_or_else(PoisonError::into_inner);
        assert_eq!(seen[0].0, "OPTIONS", "the preflight goes first");
        assert_eq!(seen[1].0, "PUT", "then the actual request");
    }
}
