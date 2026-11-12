//! The adapter between the carrier wire types and the fetch core.
//!
//! [`renderer::DialRequest`] is what a page engine asks for; [`net::Request`] is
//! what the fetch algorithm understands. This crate owns that one conversion
//! plus the reverse projection of a [`net::Response`] into
//! [`renderer::DialOutcome`], so the native browser and every in-process host
//! run one copy of the policy boundary
//! (<https://fetch.spec.whatwg.org/#concept-request>,
//! <https://fetch.spec.whatwg.org/#concept-response>).

pub mod cache;

pub use cache::{CacheDecision, HttpCache, Validators};

use std::collections::{HashMap, HashSet};
use std::hash::BuildHasher;
use std::time::Instant;

use net::{InitiatorKind, Request, isomorphic_decode, isomorphic_encode};
use renderer::{DialFailure, DialKind, DialOutcome, DialRequest, ResponseBody, ResponseHead};
use url::Url;

/// Everything a dial outcome reports except its body. Each delivery path fills
/// in the body it actually has, so a streamed outcome is never built from an
/// empty buffered one
/// (<https://fetch.spec.whatwg.org/#concept-response>).
pub struct DialHead {
    status: u16,
    status_text: String,
    /// The internal response URL; a filtered opaque view hides it from page
    /// script but the engine still names it.
    final_url: Url,
    content_type: Option<String>,
    /// The response's header list, minus the forbidden response-header names:
    /// cookies live in the browser's jar, and an `HttpOnly` one must not reach
    /// the renderer.
    headers: Vec<(String, String)>,
    response_type: net::cors::ResponseType,
    crossed_origin: bool,
    redirect_count: u32,
    referrer: Option<Url>,
    /// A page-visible opaque response has a null body
    /// (<https://fetch.spec.whatwg.org/#concept-filtered-response-opaque>).
    null_body: bool,
}

impl DialHead {
    /// The head of a response an engine consumer reads: the *unsafe* response,
    /// which keeps the status, URL, header list, and body the server sent
    /// (<https://fetch.spec.whatwg.org/#concept-internal-response>).
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
            null_body: false,
        }
    }

    /// The head of a response page script reads: the *filtered* response, whose
    /// status, URL, headers, and redirect flag are already masked
    /// (<https://fetch.spec.whatwg.org/#concept-filtered-response>).
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
            null_body: filtered.response_type().is_opaque(),
        }
    }

    /// The head of one response, in the view its consumer may read.
    #[must_use]
    pub fn read(request: &DialRequest, response: &net::Response) -> Self {
        if request.policy.exposes_response() {
            Self::filtered(response)
        } else {
            Self::internal(response)
        }
    }

    /// Whether this is an opaque response with a null body.
    #[must_use]
    pub fn null_body(&self) -> bool {
        self.null_body
    }

    /// The document head a navigation mounts, from the same fields a fetch
    /// outcome reports.
    ///
    /// The top-level and child-frame paths cannot disagree about how a header
    /// becomes policy, endpoints, or the nosniff flag: both read the header
    /// list here and hand it to one derivation
    /// (<https://html.spec.whatwg.org/multipage/document-lifecycle.html#initialise-the-document-object>).
    ///
    /// # Panics
    ///
    /// When the head is the page-fetch view of an opaque response, which names
    /// no document to mount.
    #[must_use]
    pub fn into_document_head(self) -> ResponseHead {
        ResponseHead::http(
            self.status,
            self.final_url,
            self.content_type,
            &self.headers,
        )
    }

    /// The outcome this head answers with, once its body is known.
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
            referrer: self.referrer,
        }
    }
}

/// Builds the `net` request for one page dial: URL, method, CORS parameters,
/// headers, body, and initiator.
///
/// JS header values are `ByteString`s: each UTF-16 code unit is one byte. The
/// renderer carries them as Rust `String`, so UTF-8 would turn `0x80..=0xFF`
/// into two wire bytes.
///
/// `sheets` is the explicit-referrer authority: the style sheet URLs the
/// initiator's origin loaded. A host with no style-sheet bookkeeping passes an
/// empty map, which reduces the rule to same-origin referrers.
///
/// # Errors
///
/// [`DialFailure::Connect`] when the request names a forbidden method, a
/// header value that is not a byte string, or a referrer the caller may not
/// name.
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
    // A `no-cors` request page script builds may only use CORS-safelisted
    // methods and headers: the page-side `request-no-cors` guard is advisory,
    // so the network boundary enforces it here. Otherwise a page could send an
    // unpreflighted, state-changing cross-origin request with an arbitrary
    // method and headers (<https://fetch.spec.whatwg.org/#concept-headers-guard>).
    //
    // The guard is the page's, not the engine's: a dial the engine builds sets
    // the headers its own algorithm names, and CSP's `report-uri` POST carries
    // a non-safelisted `application/csp-report` on a `no-cors` request by
    // construction (<https://w3c.github.io/webappsec-csp/#report-violation>).
    let page_no_cors =
        request.policy.exposes_response() && cors.mode == net::cors::RequestMode::NoCors;
    if page_no_cors && !net::cors::is_cors_safelisted_method(&request.method) {
        return Err(DialFailure::Connect);
    }
    let mut outbound = Request::new(request.method.clone(), url);
    // The caller owns the origin check: it authorizes `initiator` before
    // calling this adapter. The native browser checks the renderer's site lock
    // at the dial boundary, so nothing outside that site can be named. Inside
    // it, one origin is not distinguishable from another: the only frame record
    // the browser could consult is the one the same renderer reports, so a
    // per-frame table would add a leaky invariant without adding authority.
    // The site lock *is* the trust boundary, the same one `postMessage`,
    // `BroadcastChannel`, and storage use, and the one Chromium's process model
    // draws
    // (<https://chromium.googlesource.com/chromium/src/+/main/docs/process_model_and_site_isolation.md#process-locks>).
    // The WPT host has no renderer process to defend against and passes the
    // test page's own claim.
    //
    // `SameSite` cannot be affected by which same-site origin is claimed,
    // because the cookie jar compares sites, not origins
    // (`cookies::CookieOp::is_same_site_request`).
    outbound.cors = cors.with_origin(initiator.clone());
    if let Some(content_type) = &request.content_type {
        let content_type = isomorphic_encode(content_type).ok_or(DialFailure::Connect)?;
        // The body's type is appended through the same guard as every other
        // field, so a `no-cors` body cannot smuggle a type that would have
        // needed a preflight
        // (<https://fetch.spec.whatwg.org/#dom-request>,
        // <https://fetch.spec.whatwg.org/#concept-headers-guard>).
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
        if kind == DialKind::JsFetch && net::cors::is_forbidden_request_header(name, &encoded) {
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
    // The request's cache mode names request headers HTTP-network-or-cache
    // fetch appends itself, and a page header of the same name wins
    // (<https://fetch.spec.whatwg.org/#http-network-or-cache-fetch>).
    cache_mode_headers(request, &mut outbound);
    let destination = match kind {
        DialKind::JsFetch => "empty",
        DialKind::ClassicScript | DialKind::ModuleScript => "script",
        DialKind::Stylesheet => "style",
        DialKind::Image => "image",
        DialKind::Font => "font",
        DialKind::FrameLoad => "iframe",
        DialKind::CspReport => "report",
    };
    static_header(&mut outbound, "Sec-Fetch-Dest", destination);
    static_header(&mut outbound, "Sec-Fetch-Mode", cors.mode.as_str());
    if !request.body.is_empty() {
        outbound.body = Some(request.body.clone());
    }
    outbound.initiator_kind = InitiatorKind::Fetch;
    outbound.initiator = Some(initiator.clone());
    outbound.deadline = Some(deadline);
    // The renderer picks the referrer kind and policy; the browser owns the
    // client environment and the origin check. A referrer URL that is neither
    // the authorized initiator's origin nor a style sheet this tab loaded is a
    // renderer bug or forgery, so it fails closed to no referrer instead of
    // leaking. A sheet-initiated request names the sheet's URL, which can be
    // cross-origin to both the document and the target
    // (<https://fetch.spec.whatwg.org/#concept-request-referrer>,
    // <https://w3c.github.io/webappsec-referrer-policy/#integration-with-css>).
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

/// The `DialFailure` one fetch error reports to the page engine.
#[must_use]
pub fn dial_failure(error: &net::NetError) -> DialFailure {
    use net::{NetError, TransportError};
    match error {
        NetError::Transport(TransportError::Dns(_)) => DialFailure::Dns,
        NetError::Transport(TransportError::Tls(_)) => DialFailure::Tls,
        NetError::Transport(TransportError::Timeout(_)) => DialFailure::Timeout,
        NetError::RedirectLimit => DialFailure::Limit,
        NetError::Transport(TransportError::Connect(_) | TransportError::Io(_))
        | NetError::RejectedRequest
        | NetError::Decode(_) => DialFailure::Connect,
    }
}

/// Whether an explicit referrer URL is one the renderer may name: same origin
/// as the authorized initiator, or a style sheet this tab's network served.
/// The sheet arm is what authorizes CSS-initiated requests, whose referrer is
/// the sheet's URL on whatever host the sheet came from
/// (<https://w3c.github.io/webappsec-referrer-policy/#integration-with-css>).
#[must_use]
pub fn referrer_authorized<S: BuildHasher>(
    referrer: &Url,
    initiator: &url::Origin,
    sheets: &HashMap<url::Origin, HashSet<Url, S>, S>,
) -> bool {
    if referrer.origin() == *initiator {
        return true;
    }
    // Only the sheets this request's initiator loaded. A sheet another
    // document fetched is a different principal's URL, and naming it here
    // would leak that URL (and the policy it was served with).
    let Some(loaded) = sheets.get(initiator) else {
        return false;
    };
    let mut stripped = referrer.clone();
    stripped.set_fragment(None);
    loaded.contains(&stripped)
}

/// Appends the request headers Fetch's HTTP-network-or-cache fetch derives
/// from the cache mode, leaving a header the page already sent untouched
/// (<https://fetch.spec.whatwg.org/#http-network-or-cache-fetch>).
fn cache_mode_headers(request: &DialRequest, outbound: &mut Request) {
    let add = |outbound: &mut Request, name: &str, value: &str| {
        if net::combined_header_in(&request.headers, name).is_none() {
            static_header(outbound, name, value);
        }
    };
    match request.cache {
        // A `no-cache` request asks every cache on the path to validate.
        renderer::RequestCache::NoCache => add(outbound, "Cache-Control", "max-age=0"),
        // A `no-store` or `reload` request asks them to bypass storage.
        renderer::RequestCache::NoStore | renderer::RequestCache::Reload => {
            add(outbound, "Pragma", "no-cache");
            add(outbound, "Cache-Control", "no-cache");
        }
        renderer::RequestCache::Default
        | renderer::RequestCache::ForceCache
        | renderer::RequestCache::OnlyIfCached => {}
    }
}

/// Sets a request header this process chose, not the page. The value is a
/// static HTTP token, so a rejected one is a programming error, and the panic
/// names the header that has none.
/// Whether `outcome`'s body satisfies the request's integrity metadata
/// (<https://fetch.spec.whatwg.org/#concept-main-fetch>).
///
/// An opaque response is never verified: its body is not readable, so a
/// request that named metadata cannot be shown the bytes the metadata would
/// cover.
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

/// The header list one outbound request will put on the wire, decoded the way
/// a response's `Vary` comparison reads it
/// (<https://www.rfc-editor.org/rfc/rfc9111#section-4.1>).
///
/// The HTTP cache compares against this list, not the page's own: `Cookie`,
/// `Accept`, `User-Agent`, and the fetch-metadata headers are added while the
/// request is built, and a `Vary` naming one of them must see the value that
/// was actually sent.
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

/// Every `Content-Type` value from the response as one string, the way
/// getting a list header does: values combined with `", "`, each byte the
/// same code point
/// (<https://fetch.spec.whatwg.org/#concept-header-list-get-decode-split>,
/// <https://infra.spec.whatwg.org/#isomorphic-decode>). The renderer extracts
/// the MIME type from it
/// (<https://fetch.spec.whatwg.org/#concept-header-extract-mime-type>).
fn content_type_header<'a>(headers: impl Iterator<Item = (&'a str, &'a [u8])>) -> Option<String> {
    let values: Vec<String> = headers
        .filter(|(name, _)| name.eq_ignore_ascii_case("content-type"))
        .map(|(_, value)| isomorphic_decode(value))
        .collect();
    net::combined_header(values.iter().map(String::as_str))
}

/// The header list the renderer receives from an internal response: every field
/// the engine may read, minus the forbidden response-header names
/// (<https://fetch.spec.whatwg.org/#forbidden-response-header-name>).
fn internal_headers(response: &net::Response) -> Vec<(String, String)> {
    response
        .internal_headers()
        .iter()
        .filter(|(name, _)| !net::cors::is_forbidden_response_header(name))
        .map(|(name, value)| (name.to_owned(), isomorphic_decode(value)))
        .collect()
}
