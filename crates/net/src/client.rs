use std::borrow::Cow;
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll};
use std::time::{Duration, Instant, SystemTime};

use crate::InitiatorKind;
use crate::error::{NetError, TimeoutKind, TransportError};
use crate::protocol::{HeaderMap, Method, basic_authorization};
use crate::referrer::{Referrer, ReferrerPolicy, determine_referrer};
#[cfg(feature = "native")]
use crate::resolve::ResolveRule;
#[cfg(feature = "native")]
use crate::transport::native::{HttpEngine, Proxy, TrustedCa};
use crate::transport::{CallBudget, HopBudget, Transport, WireBody, WireRequest, WireResponse};
use async_compression::tokio::bufread::{BrotliDecoder, DeflateDecoder, GzipDecoder, ZlibDecoder};
use bytes::Bytes;
use cookies::{CookieJar, CookieOp, RetrievalKind, SameSiteCookiePolicy};
use http::header::{self, HeaderName, HeaderValue};
use http_body::Body as _;
use percent_encoding::percent_decode_str;
use tokio::io::{AsyncBufReadExt as _, AsyncRead, AsyncReadExt as _, BufReader, ReadBuf};
use url::Url;

const DEFAULT_MAX_REDIRECTS: u32 = 20;

pub trait ClockSource: Send + Sync {
    fn wall(&self) -> SystemTime;
    fn monotonic(&self) -> Instant;

    fn is_manual(&self) -> bool {
        false
    }
}

impl<T: ClockSource + ?Sized> ClockSource for std::sync::Arc<T> {
    fn wall(&self) -> SystemTime {
        (**self).wall()
    }

    fn monotonic(&self) -> Instant {
        (**self).monotonic()
    }

    fn is_manual(&self) -> bool {
        (**self).is_manual()
    }
}

#[derive(Clone)]
pub struct Clock(std::sync::Arc<dyn ClockSource>);

impl Clock {
    #[must_use]
    pub fn new(source: impl ClockSource + 'static) -> Self {
        Self(std::sync::Arc::new(source))
    }

    #[must_use]
    pub fn now(&self) -> SystemTime {
        self.0.wall()
    }

    #[must_use]
    pub fn monotonic(&self) -> Instant {
        self.0.monotonic()
    }

    #[must_use]
    pub fn is_manual(&self) -> bool {
        self.0.is_manual()
    }
}

impl Default for Clock {
    fn default() -> Self {
        Self(std::sync::Arc::new(SystemClock))
    }
}

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

#[derive(Clone)]
pub struct AgentOptions {
    pub identity: crate::ValidIdentity,
    pub timeout_global: Option<Duration>,
    pub timeout_per_call: Option<Duration>,
    pub max_redirects: u32,
    pub clock: Clock,
    pub cookie_jar: Option<Arc<Mutex<CookieJar>>>,
    #[cfg(feature = "native")]
    pub proxy: Option<Proxy>,
    #[cfg(feature = "native")]
    pub resolve: Vec<ResolveRule>,
    #[cfg(feature = "native")]
    pub tls_cas: Vec<TrustedCa>,
}

impl std::fmt::Debug for AgentOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut debug = f.debug_struct("AgentOptions");
        debug
            .field("timeout_global", &self.timeout_global)
            .field("timeout_per_call", &self.timeout_per_call)
            .field("max_redirects", &self.max_redirects);
        #[cfg(feature = "native")]
        debug
            .field("has_proxy", &self.proxy.is_some())
            .field("has_resolve", &!self.resolve.is_empty())
            .field("extra_tls_cas", &self.tls_cas.len());
        debug.finish_non_exhaustive()
    }
}

impl Default for AgentOptions {
    fn default() -> Self {
        Self {
            identity: crate::ValidIdentity::default(),
            timeout_global: None,
            timeout_per_call: None,
            max_redirects: DEFAULT_MAX_REDIRECTS,
            clock: Clock::default(),
            cookie_jar: None,
            #[cfg(feature = "native")]
            proxy: None,
            #[cfg(feature = "native")]
            resolve: Vec::new(),
            #[cfg(feature = "native")]
            tls_cas: Vec::new(),
        }
    }
}

type ClientExtraHeaders = HashMap<u64, Vec<(HeaderName, HeaderValue)>>;

#[derive(Clone)]
pub struct Agent {
    transport: Arc<dyn Transport>,
    identity: crate::ValidIdentity,
    client_identities: Arc<Mutex<HashMap<u64, crate::ValidIdentity>>>,
    client_extra_headers: Arc<Mutex<ClientExtraHeaders>>,
    preflight_cache: Arc<Mutex<crate::cors::PreflightCache>>,
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
    #[cfg(feature = "native")]
    #[must_use]
    pub fn new(options: AgentOptions) -> Agent {
        let AgentOptions {
            identity,
            timeout_global,
            timeout_per_call,
            max_redirects,
            proxy,
            clock,
            cookie_jar,
            resolve,
            tls_cas,
        } = options;
        let transport = Arc::new(HttpEngine::new(proxy, resolve.into(), &tls_cas));
        Agent {
            transport,
            identity,
            client_identities: Arc::new(Mutex::new(HashMap::new())),
            client_extra_headers: Arc::new(Mutex::new(HashMap::new())),
            preflight_cache: Arc::new(Mutex::new(crate::cors::PreflightCache::default())),
            max_redirects,
            jar: cookie_jar.unwrap_or_else(|| Arc::new(Mutex::new(CookieJar::default()))),
            clock,
            timeout_global,
            timeout_per_call,
        }
    }

    // https://fetch.spec.whatwg.org/#concept-http-network-fetch
    #[must_use]
    pub fn with_transport(options: AgentOptions, transport: Arc<dyn Transport>) -> Agent {
        let clock = options.clock.clone();
        Agent {
            transport,
            identity: options.identity,
            client_identities: Arc::new(Mutex::new(HashMap::new())),
            client_extra_headers: Arc::new(Mutex::new(HashMap::new())),
            preflight_cache: Arc::new(Mutex::new(crate::cors::PreflightCache::default())),
            max_redirects: options.max_redirects,
            jar: options
                .cookie_jar
                .unwrap_or_else(|| Arc::new(Mutex::new(CookieJar::default()))),
            clock,
            timeout_global: options.timeout_global,
            timeout_per_call: options.timeout_per_call,
        }
    }

    fn call_budget(&self) -> CallBudget {
        CallBudget::new(
            self.timeout_global,
            self.timeout_per_call,
            self.clock.clone(),
        )
    }

    #[must_use]
    pub fn identity(&self) -> &crate::ValidIdentity {
        &self.identity
    }

    pub fn set_client_identity(&self, client: u64, identity: Option<crate::ValidIdentity>) {
        let mut overrides = self
            .client_identities
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        match identity {
            Some(identity) => {
                overrides.insert(client, identity);
            }
            None => {
                overrides.remove(&client);
            }
        }
    }

    #[must_use]
    pub fn identity_for(&self, client: Option<u64>) -> crate::ValidIdentity {
        let Some(client) = client else {
            return self.identity.clone();
        };
        self.client_identities
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&client)
            .cloned()
            .unwrap_or_else(|| self.identity.clone())
    }

    // https://chromedevtools.github.io/devtools-protocol/tot/Network/#method-setExtraHTTPHeaders
    pub fn set_client_extra_headers(
        &self,
        client: u64,
        headers: &[(String, String)],
    ) -> Result<(), crate::HeaderError> {
        let mut parsed = Vec::with_capacity(headers.len());
        for (name, value) in headers {
            let header_name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| crate::HeaderError::InvalidName(name.as_str().into()))?;
            let header_value = HeaderValue::from_str(value)
                .map_err(|_| crate::HeaderError::InvalidValue(value.as_str().into()))?;
            parsed.push((header_name, header_value));
        }
        let mut map = self
            .client_extra_headers
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if parsed.is_empty() {
            map.remove(&client);
        } else {
            map.insert(client, parsed);
        }
        Ok(())
    }

    fn extra_headers_for(&self, client: Option<u64>) -> Vec<(HeaderName, HeaderValue)> {
        let Some(client) = client else {
            return Vec::new();
        };
        self.client_extra_headers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&client)
            .cloned()
            .unwrap_or_default()
    }

    fn jar(&self) -> MutexGuard<'_, CookieJar> {
        self.jar.lock().unwrap_or_else(PoisonError::into_inner)
    }

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

    #[must_use]
    pub fn cookies_for_partition(&self, uri: &Url, partition_key: Option<&str>) -> String {
        let origin = uri.origin();
        self.jar()
            .cookie_string(self.fetch_op(uri, &origin, RetrievalKind::NonHttp, partition_key))
    }

    #[must_use]
    pub fn cookies_for(&self, uri: &Url) -> String {
        self.cookies_for_partition(uri, None)
    }

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

    // https://cookiestore.spec.whatwg.org/#set-a-cookie
    pub fn set_script_cookie_partition(
        &self,
        write: &crate::ScriptCookieWrite,
        uri: &Url,
        partition_key: Option<&str>,
    ) {
        let origin = uri.origin();
        self.jar().store_script(
            write,
            self.fetch_op(uri, &origin, RetrievalKind::NonHttp, partition_key),
        );
    }

    pub fn set_cookie(&self, value: &str, uri: &Url) {
        let _stored = self.set_cookie_partition(value, uri, None);
    }

    // https://w3c.github.io/webdriver/#add-cookie
    pub fn store_cookie_http(&self, value: &str, uri: &Url) -> bool {
        let origin = uri.origin();
        self.jar().store(
            value,
            self.fetch_op(uri, &origin, RetrievalKind::Http, None),
        )
    }

    // https://w3c.github.io/webdriver/#get-all-cookies
    #[must_use]
    pub fn cookie_records(&self, uri: &Url) -> Vec<crate::CookieRecord> {
        self.cookie_records_partition(uri, None)
    }

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

    // https://cookiestore.spec.whatwg.org/#query-cookies
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

    // https://w3c.github.io/webdriver/#get-all-cookies
    #[must_use]
    pub fn all_cookie_records(&self) -> Vec<crate::CookieRecord> {
        self.jar().all_records(self.clock.now())
    }

    // https://w3c.github.io/webdriver/#delete-all-cookies
    pub fn clear_cookies(&self) {
        self.jar().clear();
    }

    #[must_use]
    pub fn export_cookies(&self) -> Vec<crate::CookieRecord> {
        self.jar().snapshot()
    }

    pub fn import_cookies(&self, records: Vec<crate::CookieRecord>) {
        self.jar().restore(records, self.clock.now());
    }

    // https://www.rfc-editor.org/rfc/rfc9111#section-4.1
    #[must_use]
    pub fn outbound_cookie_header(&self, request: &Request) -> Option<String> {
        let cross_origin = request.cors.origin != request.url.origin();
        if !crate::cors::credentials_admitted(request.cors.credentials, cross_origin) {
            return None;
        }
        let operation = CookieOp {
            url: &request.url,
            now: self.clock.now(),
            kind: RetrievalKind::Http,
            initiator_kind: request.initiator_kind,
            method_is_safe: request.method.is_safe(),
            initiator: request.initiator.as_ref(),
            cross_site_redirect: false,
            same_site_policy: request.same_site_cookie_policy,
            partition_key: request.partition_key.as_deref(),
        };
        let cookie = self.jar().cookie_string(operation);
        (!cookie.is_empty()).then_some(cookie)
    }

    fn prepare_outbound(
        &self,
        identity: &crate::ValidIdentity,
        headers: &mut HeaderMap,
        operation: CookieOp<'_>,
    ) -> Result<(), NetError> {
        headers.remove("cookie");
        let cookie = self.jar().cookie_string(operation);
        if !cookie.is_empty() {
            let value = HeaderValue::from_str(&cookie).map_err(|_| NetError::RejectedRequest)?;
            headers.set(header::COOKIE, value);
        }
        if headers.value("user-agent").is_none() {
            headers.append(header::USER_AGENT, identity.user_agent.header().clone());
        }
        Ok(())
    }

    // https://fetch.spec.whatwg.org/#cors-preflight-fetch
    async fn cors_preflight(
        &self,
        dial: &Dial<'_>,
        origin_header: &HeaderValue,
        hop: &Hop,
        headers: &HeaderMap,
        wire: &Url,
        budget: HopBudget,
    ) -> Result<(), NetError> {
        let settings = dial.cors;
        let method = &hop.method;
        let unsafe_names = crate::cors::unsafe_request_header_names(headers);
        if !crate::cors::needs_preflight(method, &unsafe_names) {
            return Ok(());
        }
        let origin = origin_header
            .to_str()
            .map_err(|_| NetError::RejectedRequest)?;
        let now = self.clock.monotonic();
        let cached = self
            .preflight_cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .allows(
                origin,
                wire.as_str(),
                settings.credentials.includes(),
                method,
                &unsafe_names,
                now,
            );
        if cached {
            return Ok(());
        }
        let mut preflight = crate::cors::preflight_headers(method, &unsafe_names, origin_header);
        if let Some(referrer) = headers.value("referer") {
            // https://w3c.github.io/webappsec-referrer-policy/#determine-requests-referrer
            let referrer = referrer.to_str().map_err(|_| NetError::RejectedRequest)?;
            let referrer = referrer.strip_suffix('/').unwrap_or(referrer);
            preflight.append(
                header::REFERER,
                HeaderValue::from_str(referrer).map_err(|_| NetError::RejectedRequest)?,
            );
        }
        preflight.append(
            header::USER_AGENT,
            dial.identity.user_agent.header().clone(),
        );
        let preflight_response = self
            .transport
            .exchange(
                WireRequest {
                    method: Method::OPTIONS,
                    url: wire.clone(),
                    headers: preflight,
                    body: None,
                    client: dial.client,
                    initiator: wire_initiator(dial.initiator, dial.partition_key),
                    initiator_kind: dial.initiator_kind,
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
        self.preflight_cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .store(
                origin,
                wire.as_str(),
                settings.credentials.includes(),
                &preflight_headers,
                self.clock.monotonic(),
            );
        Ok(())
    }

    // https://fetch.spec.whatwg.org/#http-fetch
    // https://fetch.spec.whatwg.org/#append-a-request-origin-header — step 3 keeps Origin on a same-origin cors POST after 307/308; Chrome and Firefox fail the same WPT subtests
    async fn apply_cors_request(
        &self,
        dial: &Dial<'_>,
        hop: &Hop,
        wire: &Url,
        headers: &mut HeaderMap,
        budget: HopBudget,
    ) -> Result<(), NetError> {
        let serialized_origin = hop.append_origin(dial.cors, wire);
        let origin_header =
            HeaderValue::from_str(&serialized_origin).map_err(|_| NetError::RejectedRequest)?;
        // https://fetch.spec.whatwg.org/#main-fetch — CSP report-uri sends no-cors with redirect mode "error" and refuses 3xx on response, matching Chromium
        if hop.response_type == crate::cors::ResponseType::Cors {
            self.cors_preflight(dial, &origin_header, hop, headers, wire, budget)
                .await?;
        }
        // https://fetch.spec.whatwg.org/#append-a-request-origin-header — step 3 keeps Origin on a same-origin cors POST after 307/308; Chrome and Firefox fail the same WPT subtests
        if hop.response_type == crate::cors::ResponseType::Cors
            || !matches!(hop.method.as_str(), "GET" | "HEAD")
        {
            headers.set(header::ORIGIN, origin_header);
        }
        Ok(())
    }

    // https://httpwg.org/http-extensions/draft-ietf-httpbis-rfc6265bis.html#section-5.6
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

    // https://fetch.spec.whatwg.org/#http-redirect-fetch
    pub async fn send(&self, request: Request) -> Result<Response, NetError> {
        if !matches!(request.url.scheme(), "http" | "https") {
            return Err(NetError::RejectedRequest);
        }
        let identity = self.identity_for(request.client);
        let dial = Dial {
            cors: &request.cors,
            identity: &identity,
            initiator_kind: request.initiator_kind,
            initiator: request.initiator.as_ref(),
            on_redirect: request.on_redirect.as_ref(),
            same_site_cookie_policy: request.same_site_cookie_policy,
            partition_key: request.partition_key.as_deref(),
            client: request.client,
        };
        let mut hop = Hop {
            method: request.method,
            headers: request.headers,
            body: request.body.map(Bytes::from),
            // https://fetch.spec.whatwg.org/#main-fetch — CSP report-uri sends no-cors with redirect mode "error" and refuses 3xx on response, matching Chromium
            response_type: crate::cors::ResponseType::Basic,
            previous_url: None,
            origin_tainted: false,
            crossed_origin: false,
            referrer_policy: request.referrer_policy,
        };
        // https://chromedevtools.github.io/devtools-protocol/tot/Network/#method-setExtraHTTPHeaders
        for (name, value) in self.extra_headers_for(request.client) {
            hop.headers.set(name, value);
        }
        if hop.headers.value("accept").is_none() {
            hop.headers
                .append(header::ACCEPT, HeaderValue::from_static("*/*"));
        }
        if hop.headers.value("accept-language").is_none() {
            hop.headers
                .append(header::ACCEPT_LANGUAGE, identity.accept_language().clone());
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
        // https://w3c.github.io/webappsec-fetch-metadata/#abstract-opdef-set-site
        let mut fetch_site = request
            .initiator
            .as_ref()
            .map(|origin| FetchSite::between(origin, &request.url));
        let fetch_metadata = FetchMetadata::capture(&hop.headers);
        fetch_metadata.append(&mut hop.headers, &request.url, fetch_site);
        let mut url = request.url;
        // https://fetch.spec.whatwg.org/#main-fetch — CSP report-uri sends no-cors with redirect mode "error" and refuses 3xx on response, matching Chromium
        // https://w3c.github.io/webappsec-referrer-policy/#determine-requests-referrer
        let mut referrer = request.referrer;
        let mut referrer_policy = request.referrer_policy;
        let mut determined = determine_referrer(referrer_policy, &referrer, &url);
        Self::write_referer(&mut hop.headers, determined.as_ref())?;
        let mut followed = 0u32;
        let mut cross_site_redirect = false;
        // https://www.rfc-editor.org/rfc/rfc6265bis.html#section-5.2
        let mut stored_any = false;
        let mut budget = self.call_budget();
        if let Some(deadline) = request.deadline {
            budget.tighten_global(deadline);
        }

        loop {
            let mut response = self
                .exchange(&dial, &url, &mut hop, cross_site_redirect, budget.clone())
                .await?;
            response.redirect_count = followed;
            response.referrer.clone_from(&determined);
            stored_any |= response.cookies_stored();
            let hop_status = response.status();
            if self.max_redirects == 0 {
                cors_gate(&response, &dial, &hop)?;
                response.cookies_stored = stored_any;
                return Ok(response);
            }
            // https://w3c.github.io/webappsec-referrer-policy/#set-requests-referrer-policy-on-redirect
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
            if let Some(gate) = dial.on_redirect
                && !gate(&next, followed + 1)
            {
                return Err(NetError::RejectedRequest);
            }
            cross_site_redirect |= !cookies::schemeful_same_site(&url, &next);
            apply_redirect_policy(hop_status, &url, &next, &mut hop);
            fetch_site = FetchSite::widen(fetch_site, request.initiator.as_ref(), &next);
            fetch_metadata.append(&mut hop.headers, &next, fetch_site);
            // https://fetch.spec.whatwg.org/#main-fetch — CSP report-uri sends no-cors with redirect mode "error" and refuses 3xx on response, matching Chromium
            // https://w3c.github.io/webappsec-referrer-policy/#determine-requests-referrer
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

    // https://fetch.spec.whatwg.org/#http-fetch
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
        self.apply_cors_request(dial, hop, &wire, &mut hop_headers, budget.clone())
            .await?;
        self.prepare_outbound(dial.identity, &mut hop_headers, operation)?;
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
                    client: dial.client,
                    initiator: wire_initiator(dial.initiator, dial.partition_key),
                    initiator_kind: dial.initiator_kind,
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
        let content_encoding = parse_content_encoding(&response_headers)?;
        let mut response = Response {
            status,
            status_text: reason_phrase(status, http2),
            headers: response_headers,
            final_url: url.clone(),
            body: RawBody {
                inner: reader,
                current: None,
                offset: 0,
                shared_error: Arc::new(Mutex::new(None)),
                shared_input: Arc::new(AtomicBool::new(false)),
            },
            content_encoding,
            redirect_count: 0,
            // https://fetch.spec.whatwg.org/#concept-filtered-response
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

pub type RedirectGate = Arc<dyn Fn(&Url, u32) -> bool + Send + Sync>;

// https://fetch.spec.whatwg.org/#concept-request-response-tainting
// https://fetch.spec.whatwg.org/#concept-request-tainted-origin
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

pub struct Request {
    pub method: Method,
    pub url: Url,
    pub headers: HeaderMap,
    pub initiator_kind: InitiatorKind,
    // https://w3c.github.io/webappsec-fetch-metadata/#abstract-opdef-set-site
    // https://httpwg.org/http-extensions/draft-ietf-httpbis-rfc6265bis.html#name-same-site-requests
    pub initiator: Option<url::Origin>,
    pub body: Option<Vec<u8>>,
    pub deadline: Option<Instant>,
    pub cors: crate::cors::CorsSettings,
    pub on_redirect: Option<RedirectGate>,
    // https://fetch.spec.whatwg.org/#concept-request-referrer
    pub referrer: Referrer,
    pub referrer_policy: ReferrerPolicy,
    // https://privacycg.github.io/saa-non-cookie-storage/#shared-worker
    pub same_site_cookie_policy: SameSiteCookiePolicy,
    // https://github.com/privacycg/CHIPS#computing-the-cookie-partition-key
    pub partition_key: Option<String>,
    // https://fetch.spec.whatwg.org/#concept-request-client
    pub client: Option<u64>,
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
            .field("client", &self.client)
            .finish()
    }
}

impl Request {
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
            client: None,
        }
    }
}

fn followable_location(status: u16, headers: &HeaderMap) -> Option<Vec<u8>> {
    if !matches!(status, 301 | 302 | 303 | 307 | 308) {
        return None;
    }
    headers.get("location").map(<[u8]>::to_vec)
}

// https://fetch.spec.whatwg.org/#http-redirect-fetch
fn resolve_location(current: &Url, location: &[u8]) -> Result<Url, NetError> {
    // https://fetch.spec.whatwg.org/#concept-location-url
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
    // https://wicg.github.io/ua-client-hints/#secure-transport
    // https://wicg.github.io/ua-client-hints/#http-ua-hints
    if !secure_context::is_potentially_trustworthy(next) {
        hop.headers.remove("sec-ch-prefers-color-scheme");
    }
}

enum HopOutcome {
    Final(Box<Response>),
    Redirect { next: Url },
}

// https://w3c.github.io/webappsec-fetch-metadata/#abstract-opdef-set-site
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum FetchSite {
    SameOrigin,
    SameSite,
    CrossSite,
}

impl FetchSite {
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

fn write_fetch_site(headers: &mut HeaderMap, site: Option<FetchSite>) {
    headers.set(
        header::HeaderName::from_static("sec-fetch-site"),
        HeaderValue::from_static(site.map_or("none", FetchSite::token)),
    );
}

// https://w3c.github.io/webappsec-fetch-metadata/#fetch-integration
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

// https://fetch.spec.whatwg.org/#concept-request-credentials-mode
// https://fetch.spec.whatwg.org/#http-network-or-cache-fetch
// https://wicg.github.io/ua-client-hints/#http-ua-hints
// https://wicg.github.io/private-network-access/#determine-the-ip-address-space
fn wire_initiator(
    initiator: Option<&url::Origin>,
    partition_key: Option<&str>,
) -> Option<url::Origin> {
    match initiator {
        Some(origin @ url::Origin::Tuple(..)) => Some(origin.clone()),
        Some(url::Origin::Opaque(_)) => partition_key
            .and_then(|site| Url::parse(site).ok())
            .map(|url| url.origin()),
        None => None,
    }
}

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
    dial.identity.hints().write(headers, url);
    Ok(())
}

struct Dial<'a> {
    cors: &'a crate::cors::CorsSettings,
    identity: &'a crate::ValidIdentity,
    initiator_kind: InitiatorKind,
    initiator: Option<&'a url::Origin>,
    on_redirect: Option<&'a RedirectGate>,
    same_site_cookie_policy: SameSiteCookiePolicy,
    partition_key: Option<&'a str>,
    client: Option<u64>,
}

impl Dial<'_> {
    // https://fetch.spec.whatwg.org/#concept-request-credentials-mode
    fn credentials_include(&self) -> bool {
        self.cors.credentials.includes()
    }
}

struct Hop {
    method: Method,
    headers: HeaderMap,
    body: Option<Bytes>,
    response_type: crate::cors::ResponseType,
    // https://fetch.spec.whatwg.org/#concept-request-tainted-origin
    previous_url: Option<Url>,
    // https://fetch.spec.whatwg.org/#serializing-a-request-origin
    origin_tainted: bool,
    // https://fetch.spec.whatwg.org/#concept-request-response-tainting
    crossed_origin: bool,
    // https://fetch.spec.whatwg.org/#append-a-request-origin-header — step 3 keeps Origin on a same-origin cors POST after 307/308; Chrome and Firefox fail the same WPT subtests
    // https://w3c.github.io/webappsec-referrer-policy/#set-requests-referrer-policy-on-redirect
    referrer_policy: ReferrerPolicy,
}

impl Hop {
    // https://fetch.spec.whatwg.org/#serializing-a-request-origin
    fn serialized_origin<'a>(&self, settings: &'a crate::cors::CorsSettings) -> Cow<'a, str> {
        if self.origin_tainted {
            Cow::Borrowed("null")
        } else {
            Cow::Owned(settings.origin.ascii_serialization())
        }
    }

    // https://fetch.spec.whatwg.org/#append-a-request-origin-header — step 3 keeps Origin on a same-origin cors POST after 307/308; Chrome and Firefox fail the same WPT subtests
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

// https://fetch.spec.whatwg.org/#concept-request-response-tainting
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
        // https://fetch.spec.whatwg.org/#main-fetch — CSP report-uri sends no-cors with redirect mode "error" and refuses 3xx on response, matching Chromium
        crate::cors::RequestMode::NoCors => match settings.redirect {
            crate::cors::RedirectMode::Manual => Err(NetError::RejectedRequest),
            crate::cors::RedirectMode::Follow | crate::cors::RedirectMode::Error => {
                Ok(crate::cors::ResponseType::Opaque)
            }
        },
    }
}

// https://fetch.spec.whatwg.org/#concept-filtered-response
pub struct FilteredResponse<'a> {
    response: &'a Response,
}

impl<'a> FilteredResponse<'a> {
    #[must_use]
    pub fn status(&self) -> u16 {
        if self.response.is_opaque() {
            0
        } else {
            self.response.status
        }
    }

    #[must_use]
    pub fn status_text(&self) -> &'a str {
        if self.response.is_opaque() {
            ""
        } else {
            &self.response.status_text
        }
    }

    // https://fetch.spec.whatwg.org/#concept-filtered-response-opaque
    #[must_use]
    pub fn url(&self) -> Option<&'a Url> {
        if self.response.filtering == crate::cors::Filtering::Opaque {
            None
        } else {
            Some(&self.response.final_url)
        }
    }

    #[must_use]
    pub fn response_type(&self) -> crate::cors::ResponseType {
        self.response.response_type()
    }

    // https://fetch.spec.whatwg.org/#concept-filtered-response
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

// https://fetch.spec.whatwg.org/#cors-check
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

// https://fetch.spec.whatwg.org/#http-fetch
// https://fetch.spec.whatwg.org/#http-redirect-fetch
fn classify_response(
    response: Response,
    current_url: &Url,
    dial: &Dial<'_>,
    hop: &Hop,
) -> Result<HopOutcome, NetError> {
    cors_gate(&response, dial, hop)?;
    let settings = dial.cors;
    // https://fetch.spec.whatwg.org/#http-redirect-fetch
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
    // https://fetch.spec.whatwg.org/#http-redirect-fetch
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
        crate::cors::RedirectMode::Manual | crate::cors::RedirectMode::Follow => {}
    }
    Ok(HopOutcome::Redirect { next })
}

// https://fetch.spec.whatwg.org/#concept-response-status-message
fn reason_phrase(status: u16, http2: bool) -> String {
    if http2 {
        return String::new();
    }
    http::StatusCode::from_u16(status)
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

// https://www.rfc-editor.org/rfc/rfc9110#section-8.4 — UA-defined cap matching Chromium's decoding filter limit to prevent stack overflow on chained encodings
const MAX_CONTENT_ENCODING_LAYERS: usize = 5;

fn parse_content_encoding(headers: &HeaderMap) -> Result<Vec<String>, NetError> {
    let codings: Vec<String> = crate::get_decode_split(headers, "content-encoding")
        .into_iter()
        .flatten()
        .map(|coding| coding.to_ascii_lowercase())
        .filter(|coding| !coding.is_empty())
        .collect();
    if codings
        .iter()
        .filter(|coding| coding.as_str() != "identity")
        .count()
        > MAX_CONTENT_ENCODING_LAYERS
    {
        return Err(NetError::Decode("too many content-encoding layers".into()));
    }
    Ok(codings)
}

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
        let mut applied = 0usize;
        for coding in codings.iter().rev() {
            if coding != "identity" {
                if applied >= MAX_CONTENT_ENCODING_LAYERS {
                    set_body_error(
                        &shared_error,
                        NetError::Decode("too many content-encoding layers".into()),
                    );
                    break;
                }
                applied += 1;
            }
            match coding.as_str() {
                "identity" => {}
                "gzip" | "x-gzip" => {
                    let mut decoder = GzipDecoder::new(BufReader::new(reader));
                    // https://www.rfc-editor.org/rfc/rfc7230#section-4.1.3
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
                // https://fetch.spec.whatwg.org/#concept-http-network-fetch
                _ => break,
            }
        }
        Self {
            reader,
            shared_error,
            shared_input,
        }
    }

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

struct RawBody {
    inner: WireBody,
    current: Option<Bytes>,
    offset: usize,
    shared_error: Arc<Mutex<Option<NetError>>>,
    shared_input: Arc<AtomicBool>,
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
    cookies_stored: bool,
}

impl Response {
    #[must_use]
    pub fn status(&self) -> u16 {
        self.status
    }

    #[must_use]
    pub fn status_text(&self) -> &str {
        &self.status_text
    }

    // https://fetch.spec.whatwg.org/#concept-response-type
    #[must_use]
    pub fn response_type(&self) -> crate::cors::ResponseType {
        self.filtering.response_type()
    }

    // https://fetch.spec.whatwg.org/#concept-filtered-response-opaque
    #[must_use]
    pub fn is_opaque(&self) -> bool {
        self.filtering.is_opaque()
    }

    // https://fetch.spec.whatwg.org/#concept-request-response-tainting
    #[must_use]
    pub fn crossed_origin(&self) -> bool {
        self.crossed_origin
    }

    // https://fetch.spec.whatwg.org/#concept-internal-response
    #[must_use]
    pub fn internal_headers(&self) -> &HeaderMap {
        &self.headers
    }

    // https://fetch.spec.whatwg.org/#concept-filtered-response
    #[must_use]
    pub fn filtered(&self) -> FilteredResponse<'_> {
        FilteredResponse { response: self }
    }

    #[must_use]
    pub fn final_url(&self) -> &Url {
        &self.final_url
    }

    #[must_use]
    pub fn redirect_count(&self) -> u32 {
        self.redirect_count
    }

    // https://html.spec.whatwg.org/multipage/document-lifecycle.html#creating-and-initializing-a-document-object
    #[must_use]
    pub fn referrer(&self) -> Option<&Url> {
        self.referrer.as_ref()
    }

    #[must_use]
    pub fn cookies_stored(&self) -> bool {
        self.cookies_stored
    }

    // https://fetch.spec.whatwg.org/#concept-filtered-response-opaque
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

    // https://fetch.spec.whatwg.org/#append-a-request-origin-header — step 3 keeps Origin on a same-origin cors POST after 307/308; Chrome and Firefox fail the same WPT subtests
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

    // https://fetch.spec.whatwg.org/#append-a-request-origin-header — step 3 keeps Origin on a same-origin cors POST after 307/308; Chrome and Firefox fail the same WPT subtests
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

    // https://wicg.github.io/ua-client-hints/#secure-transport
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

    // https://w3c.github.io/webappsec-fetch-metadata/#abstract-opdef-set-site
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

    use std::pin::Pin;
    use std::sync::{Arc, Mutex, PoisonError};

    use bytes::Bytes;
    use http_body_util::BodyExt as _;
    use url::Url;

    use super::{Agent, AgentOptions, Method, NetError, Request};
    use crate::cors::{CorsSettings, CredentialsMode, RedirectMode, RequestMode, ResponseType};
    use crate::transport::{HopBudget, Transport, WireRequest, WireResponse};

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

    // https://fetch.spec.whatwg.org/#cors-check
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

    type ScriptedResponse = (u16, Vec<(&'static str, &'static str)>);

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

    // https://fetch.spec.whatwg.org/#http-redirect-fetch
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

    // https://fetch.spec.whatwg.org/#cors-preflight-fetch
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

    // https://fetch.spec.whatwg.org/#cors-preflight-cache
    #[tokio::test]
    async fn a_cached_cors_preflight_is_reused_on_the_second_request() {
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
                            ("access-control-max-age", "60"),
                        ],
                    ),
                    (200, vec![("access-control-allow-origin", "http://a.test")]),
                    (200, vec![("access-control-allow-origin", "http://a.test")]),
                ])),
                seen: Arc::clone(&seen),
            }),
        );
        for _ in 0..2 {
            let mut request = cross_origin_request();
            request.method = Method::PUT;
            request
                .headers
                .insert("x-custom", "1")
                .expect("a test header");
            let response = agent.send(request).await.expect("request succeeds");
            assert_eq!(response.status(), 200);
        }
        let seen = seen.lock().unwrap_or_else(PoisonError::into_inner);
        let methods: Vec<&str> = seen.iter().map(|entry| entry.0.as_str()).collect();
        assert_eq!(methods, vec!["OPTIONS", "PUT", "PUT"]);
    }

    // https://chromedevtools.github.io/devtools-protocol/tot/Network/#method-setExtraHTTPHeaders
    #[tokio::test]
    async fn client_extra_headers_are_sent_on_outbound_requests() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let agent = Agent::with_transport(
            AgentOptions::default(),
            Arc::new(FakeTransport {
                allow_origin: None,
                seen: Arc::clone(&seen),
            }),
        );
        agent
            .set_client_extra_headers(
                42,
                &[("X-Test-Header".to_owned(), "tinybrowser".to_owned())],
            )
            .expect("valid header");
        let mut request = Request::new(
            Method::GET,
            Url::parse("http://a.test/headers").expect("url"),
        );
        request.client = Some(42);
        let response = agent.send(request).await.expect("request succeeds");
        assert_eq!(response.status(), 200);
        let seen = seen.lock().unwrap_or_else(PoisonError::into_inner);
        assert!(
            seen.iter()
                .any(|(name, value)| name == "x-test-header" && value == "tinybrowser"),
            "expected x-test-header in {seen:?}"
        );
    }
}
