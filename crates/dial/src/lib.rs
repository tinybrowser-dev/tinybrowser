// https://fetch.spec.whatwg.org/#concept-request
// https://fetch.spec.whatwg.org/#concept-response

pub mod body;
pub mod cache;
pub mod dialer;

pub use dialer::{Admission, BodySink, BufferedOnly, Dialer, StreamInput, StreamStart, Unlimited};

fn document_url(origin: &url::Origin) -> Url {
    net::origin_url(origin).unwrap_or_else(net::about_blank)
}

// https://w3c.github.io/webappsec-csp/#should-request-be-blocked-by-content-security-policy
pub fn watch_redirects(
    request: &DialRequest,
    initiator: &url::Origin,
    outbound: &mut Request,
) -> Arc<Mutex<Option<Url>>> {
    let blocked = Arc::new(Mutex::new(None));
    if let Some(csp) = request.csp.clone() {
        let document_url = document_url(initiator);
        let blocked = Arc::clone(&blocked);
        outbound.on_redirect = Some(Arc::new(move |next, redirects| {
            if csp.allows(&document_url, next, redirects) {
                return true;
            }
            *blocked
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(next.clone());
            false
        }));
    }
    blocked
}

// https://w3c.github.io/webappsec-csp/#report-violation
#[must_use]
pub fn refused_redirect(blocked: &Mutex<Option<Url>>) -> Option<DialOutcome> {
    let url = blocked
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()?;
    Some(DialOutcome {
        status: 0,
        status_text: String::new(),
        final_url: url,
        content_type: None,
        headers: Vec::new(),
        body: ResponseBody::Bytes(Vec::new()),
        response_type: net::cors::ResponseType::Basic,
        crossed_origin: false,
        redirect_count: 0,
        cookies_stored: false,
        referrer: None,
    })
}

pub use cache::{CacheDecision, HttpCache, Validators};

use std::collections::{HashMap, HashSet};
use std::hash::BuildHasher;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use net::{InitiatorKind, Request, isomorphic_decode, isomorphic_encode};
use renderer::{DialFailure, DialKind, DialOutcome, DialRequest, ResponseBody, ResponseHead};
use url::Url;

// https://fetch.spec.whatwg.org/#concept-response
pub struct DialHead {
    status: u16,
    status_text: String,
    final_url: Url,
    content_type: Option<String>,
    headers: Vec<(String, String)>,
    response_type: net::cors::ResponseType,
    crossed_origin: bool,
    redirect_count: u32,
    referrer: Option<Url>,
    cookies_stored: bool,
    // https://fetch.spec.whatwg.org/#concept-filtered-response-opaque
    null_body: bool,
}

impl DialHead {
    // https://fetch.spec.whatwg.org/#concept-internal-response
    #[must_use]
    pub fn internal(response: &net::Response) -> Self {
        Self {
            status: response.status(),
            status_text: response.status_text().to_owned(),
            final_url: response.final_url().clone(),
            content_type: content_type_header(response.internal_headers().iter()),
            headers: internal_headers(response),
            response_type: response.response_type(),
            crossed_origin: response.crossed_origin(),
            redirect_count: response.redirect_count(),
            referrer: response.referrer().cloned(),
            cookies_stored: response.cookies_stored(),
            null_body: false,
        }
    }

    // https://fetch.spec.whatwg.org/#concept-filtered-response
    #[must_use]
    pub fn filtered(response: &net::Response) -> Self {
        let filtered = response.filtered();
        let visible = filtered.headers();
        let content_type = content_type_header(visible.iter().copied());
        let headers = visible
            .into_iter()
            .map(|(name, value)| (name.to_owned(), isomorphic_decode(value)))
            .collect();
        Self {
            status: filtered.status(),
            status_text: filtered.status_text().to_owned(),
            final_url: response.final_url().clone(),
            content_type,
            headers,
            response_type: filtered.response_type(),
            crossed_origin: response.crossed_origin(),
            redirect_count: if filtered.response_type().is_opaque() {
                0
            } else {
                response.redirect_count()
            },
            referrer: response.referrer().cloned(),
            cookies_stored: response.cookies_stored(),
            null_body: filtered.response_type().is_opaque(),
        }
    }

    #[must_use]
    pub fn read(request: &DialRequest, response: &net::Response) -> Self {
        if request.policy.exposes_response() {
            Self::filtered(response)
        } else {
            Self::internal(response)
        }
    }

    #[must_use]
    pub fn null_body(&self) -> bool {
        self.null_body
    }

    // https://html.spec.whatwg.org/multipage/document-lifecycle.html#initialise-the-document-object
    #[must_use]
    pub fn into_document_head(self) -> ResponseHead {
        ResponseHead::http(
            self.status,
            self.final_url,
            self.content_type,
            &self.headers,
        )
    }

    #[must_use]
    pub fn into_outcome(self, body: ResponseBody) -> DialOutcome {
        DialOutcome {
            status: self.status,
            status_text: self.status_text,
            final_url: self.final_url,
            content_type: self.content_type,
            headers: self.headers,
            body,
            response_type: self.response_type,
            crossed_origin: self.crossed_origin,
            redirect_count: self.redirect_count,
            cookies_stored: self.cookies_stored,
            referrer: self.referrer,
        }
    }
}

pub fn outbound_request<S: BuildHasher>(
    request: &DialRequest,
    initiator: &url::Origin,
    deadline: Instant,
    sheets: &HashMap<url::Origin, HashSet<Url, S>, S>,
) -> Result<Request, DialFailure> {
    let url = request.url.clone();
    let kind = request.policy.kind();
    // https://fetch.spec.whatwg.org/#forbidden-method
    if kind == DialKind::JsFetch && request.method.is_forbidden() {
        return Err(DialFailure::Connect);
    }
    let cors = request.policy.cors();
    // https://fetch.spec.whatwg.org/#concept-headers-guard
    // https://w3c.github.io/webappsec-csp/#report-violation
    let page_no_cors =
        request.policy.exposes_response() && cors.mode == net::cors::RequestMode::NoCors;
    if page_no_cors && !net::cors::is_cors_safelisted_method(&request.method) {
        return Err(DialFailure::Connect);
    }
    let mut outbound = Request::new(request.method.clone(), url);
    // https://chromium.googlesource.com/chromium/src/+/main/docs/process_model_and_site_isolation.md#process-locks
    outbound.cors = cors.with_origin(initiator.clone());
    if let Some(content_type) = &request.content_type {
        let content_type = isomorphic_encode(content_type).ok_or(DialFailure::Connect)?;
        // https://fetch.spec.whatwg.org/#dom-request
        // https://fetch.spec.whatwg.org/#concept-headers-guard
        if !page_no_cors
            || net::cors::is_no_cors_safelisted_request_header("content-type", &content_type)
        {
            outbound
                .headers
                .insert("Content-Type", content_type)
                .map_err(|_| DialFailure::Connect)?;
        }
    }
    for (name, value) in &request.headers {
        let encoded = isomorphic_encode(value).ok_or(DialFailure::Connect)?;
        if net::cors::is_forbidden_request_header(name, &encoded) {
            continue;
        }
        if page_no_cors && !net::cors::is_no_cors_safelisted_request_header(name, &encoded) {
            continue;
        }
        outbound
            .headers
            .insert(name, encoded)
            .map_err(|_| DialFailure::Connect)?;
    }
    // https://fetch.spec.whatwg.org/#http-network-or-cache-fetch
    cache_mode_headers(request, &mut outbound);
    let top_level_document =
        kind == DialKind::FrameLoad && request.frame == Some(renderer::FrameId::MAIN);
    // https://w3c.github.io/webappsec-fetch-metadata/#sec-fetch-dest-header
    let destination = match kind {
        DialKind::JsFetch => "empty",
        DialKind::ClassicScript | DialKind::ModuleScript => "script",
        DialKind::Stylesheet => "style",
        DialKind::Image => "image",
        DialKind::Font => "font",
        DialKind::FrameLoad if top_level_document => "document",
        DialKind::FrameLoad => "iframe",
        DialKind::CspReport => "report",
    };
    if kind == DialKind::FrameLoad {
        // https://fetch.spec.whatwg.org/#concept-main-fetch
        static_header(&mut outbound, "Accept", NAVIGATION_ACCEPT);
    }
    if top_level_document {
        // https://w3c.github.io/webappsec-fetch-metadata/#sec-fetch-user-header
        static_header(&mut outbound, "Sec-Fetch-User", "?1");
    }
    static_header(&mut outbound, "Sec-Fetch-Dest", destination);
    static_header(&mut outbound, "Sec-Fetch-Mode", cors.mode.as_str());
    if !request.body.is_empty() {
        outbound.body = Some(request.body.clone());
    }
    outbound.initiator_kind = InitiatorKind::Fetch;
    outbound.initiator = Some(initiator.clone());
    outbound.deadline = Some(deadline);
    // https://fetch.spec.whatwg.org/#concept-request-referrer
    // https://w3c.github.io/webappsec-referrer-policy/#integration-with-css
    outbound.referrer = match &request.referrer {
        renderer::ReferrerRequest::Client { url } if url.origin() == *initiator => {
            net::Referrer::Client(net::ReferrerClient {
                origin: initiator.clone(),
                url: url.clone(),
            })
        }
        renderer::ReferrerRequest::Url(url) if referrer_authorized(url, initiator, sheets) => {
            net::Referrer::Url(url.clone())
        }
        renderer::ReferrerRequest::NoReferrer
        | renderer::ReferrerRequest::Client { .. }
        | renderer::ReferrerRequest::Url(_) => net::Referrer::NoReferrer,
    };
    outbound.referrer_policy = request.referrer_policy;
    outbound.same_site_cookie_policy = match request.cookie_policy {
        renderer::CookiePolicy::All => net::SameSiteCookiePolicy::All,
        renderer::CookiePolicy::NoneOnly => net::SameSiteCookiePolicy::NoneOnly,
    };
    outbound.partition_key.clone_from(&request.partition_key);
    Ok(outbound)
}

pub const PAGE_FETCH_TIMEOUT: Duration = Duration::from_secs(30);

// https://fetch.spec.whatwg.org/#concept-request
#[must_use]
pub fn budget(request: &DialRequest) -> Duration {
    if request.timeout_ms == 0 {
        PAGE_FETCH_TIMEOUT
    } else {
        PAGE_FETCH_TIMEOUT.min(Duration::from_millis(request.timeout_ms))
    }
}

#[must_use]
pub fn dial_failure(error: &net::NetError) -> DialFailure {
    use net::{NetError, TransportError};
    match error {
        NetError::Transport(TransportError::Dns(_)) => DialFailure::Dns,
        NetError::Transport(TransportError::Capacity) => DialFailure::QueueFull,
        NetError::Transport(TransportError::Tls(_)) => DialFailure::Tls,
        NetError::Transport(TransportError::Timeout(_)) => DialFailure::Timeout,
        NetError::Transport(TransportError::BodyTooLarge) | NetError::RedirectLimit => {
            DialFailure::Limit
        }
        NetError::Transport(TransportError::Connect(_) | TransportError::Io(_))
        | NetError::RejectedRequest
        | NetError::Decode(_) => DialFailure::Connect,
    }
}

// https://w3c.github.io/webappsec-referrer-policy/#integration-with-css
#[must_use]
pub fn referrer_authorized<S: BuildHasher>(
    referrer: &Url,
    initiator: &url::Origin,
    sheets: &HashMap<url::Origin, HashSet<Url, S>, S>,
) -> bool {
    if referrer.origin() == *initiator {
        return true;
    }
    let Some(loaded) = sheets.get(initiator) else {
        return false;
    };
    let mut stripped = referrer.clone();
    stripped.set_fragment(None);
    loaded.contains(&stripped)
}

// https://fetch.spec.whatwg.org/#http-network-or-cache-fetch
fn cache_mode_headers(request: &DialRequest, outbound: &mut Request) {
    let add = |outbound: &mut Request, name: &str, value: &str| {
        if net::combined_header_in(&request.headers, name).is_none() {
            static_header(outbound, name, value);
        }
    };
    match request.cache {
        renderer::RequestCache::NoCache => add(outbound, "Cache-Control", "max-age=0"),
        renderer::RequestCache::NoStore | renderer::RequestCache::Reload => {
            add(outbound, "Pragma", "no-cache");
            add(outbound, "Cache-Control", "no-cache");
        }
        renderer::RequestCache::Default
        | renderer::RequestCache::ForceCache
        | renderer::RequestCache::OnlyIfCached => {}
    }
}

// https://wicg.github.io/user-preference-media-features-headers/#sec-ch-prefers-color-scheme
pub const SEC_CH_PREFERS_COLOR_SCHEME: &str = "dark";

// https://fetch.spec.whatwg.org/#concept-main-fetch
const NAVIGATION_ACCEPT: &str = "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8";

// https://www.w3.org/TR/upgrade-insecure-requests/#preference
// https://w3c.github.io/webappsec-fetch-metadata/
// https://wicg.github.io/ua-client-hints/#sec-ch-ua
// https://w3c.github.io/webappsec-secure-contexts/#is-url-trustworthy
pub fn apply_navigation_headers(request: &mut Request, url: &Url) {
    static_header(request, "Upgrade-Insecure-Requests", "1");
    static_header(request, "Accept", NAVIGATION_ACCEPT);
    static_header(request, "Sec-Fetch-Mode", "navigate");
    static_header(request, "Sec-Fetch-User", "?1");
    static_header(request, "Sec-Fetch-Dest", "document");
    if secure_context::is_potentially_trustworthy(url) {
        static_header(
            request,
            "Sec-CH-Prefers-Color-Scheme",
            SEC_CH_PREFERS_COLOR_SCHEME,
        );
    }
}

// https://fetch.spec.whatwg.org/#concept-main-fetch
#[must_use]
pub fn integrity_ok(request: &DialRequest, outcome: &DialOutcome) -> bool {
    let Some(integrity) = request.integrity.as_deref() else {
        return true;
    };
    match &outcome.body {
        ResponseBody::Bytes(body) => {
            !outcome.response_type.is_opaque() && renderer::integrity_verifies(integrity, body)
        }
        ResponseBody::Undecodable | ResponseBody::Streamed(_) => false,
    }
}

// https://www.rfc-editor.org/rfc/rfc9111#section-4.1
#[must_use]
pub fn sent_headers(request: &Request) -> Vec<(String, String)> {
    request
        .headers
        .iter()
        .map(|(name, value)| (name.to_owned(), isomorphic_decode(value)))
        .collect()
}

fn static_header(request: &mut Request, name: &str, value: &str) {
    request
        .headers
        .insert(name, value)
        .unwrap_or_else(|_| panic!("{name}: {value:?} is not a header value"));
}

// https://fetch.spec.whatwg.org/#concept-header-list-get-decode-split
// https://infra.spec.whatwg.org/#isomorphic-decode
// https://fetch.spec.whatwg.org/#concept-header-extract-mime-type
fn content_type_header<'a>(headers: impl Iterator<Item = (&'a str, &'a [u8])>) -> Option<String> {
    let values: Vec<String> = headers
        .filter(|(name, _)| name.eq_ignore_ascii_case("content-type"))
        .map(|(_, value)| isomorphic_decode(value))
        .collect();
    net::combined_header(values.iter().map(String::as_str))
}

// https://fetch.spec.whatwg.org/#forbidden-response-header-name
fn internal_headers(response: &net::Response) -> Vec<(String, String)> {
    response
        .internal_headers()
        .iter()
        .filter(|(name, _)| !net::cors::is_forbidden_response_header(name))
        .map(|(name, value)| (name.to_owned(), isomorphic_decode(value)))
        .collect()
}
