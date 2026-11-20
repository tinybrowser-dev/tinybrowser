//! The CORS protocol
//! (<https://fetch.spec.whatwg.org/#http-cors-protocol>).
//!
//! Only the pieces the browser process needs to make the cross-origin
//! decision live here: request/response classification, the CORS check, the
//! preflight request shape and its validation, and response-header filtering.
//! The `fetch` algorithm's policy decisions run in the renderer, and the
//! browser enforces the ones that face the network: the request origin, the
//! CORS check, preflights, and redirect gating. Which view a dial reports
//! follows the kind the renderer names; probing that label with response-body
//! sniffing is tracked in #190.

use enumerated::Enumerated;
use http::header::{self, HeaderValue};

use crate::protocol::{HeaderMap, Method};

/// A request's CORS mode
/// (<https://fetch.spec.whatwg.org/#concept-request-mode>).
#[derive(
    Clone, Copy, Debug, Default, Hash, PartialEq, Eq, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum RequestMode {
    /// A document, frame, or other non-fetch-resource navigation. The response
    /// is never CORS-filtered.
    Navigate,
    /// No CORS: same-origin reads, opaque cross-origin reads (`<img>`,
    /// classic scripts, `no-cors`).
    #[default]
    NoCors,
    /// A cross-origin request fails outright.
    SameOrigin,
    /// The full CORS protocol runs and a cross-origin response is readable.
    Cors,
}

impl RequestMode {
    /// The IDL value of this mode, which is also the `Sec-Fetch-Mode` token
    /// (<https://fetch.spec.whatwg.org/#requestinit>).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Navigate => "navigate",
            Self::NoCors => "no-cors",
            Self::SameOrigin => "same-origin",
            Self::Cors => "cors",
        }
    }
}

enumerated::enumerated! {
    /// A request's credentials mode
    /// (<https://fetch.spec.whatwg.org/#concept-request-credentials-mode>).
    #[derive(Default, Hash, serde::Serialize, serde::Deserialize)]
    #[serde(rename_all = "kebab-case")]
    pub CredentialsMode {
        /// Never send or accept credentials; cookies cross the wire only if they
        /// are already same-origin.
        Omit => "omit",
        /// Credentials for same-origin requests only. The `fetch` default.
        #[default]
        SameOrigin => "same-origin",
        /// Credentials for every request.
        Include => "include",
    }
}

impl CredentialsMode {
    /// Whether this mode is `include`.
    #[must_use]
    pub fn includes(self) -> bool {
        matches!(self, Self::Include)
    }

    /// Parses the IDL enum's string value, which is exact-match
    /// (<https://fetch.spec.whatwg.org/#requestinit>).
    #[must_use]
    pub fn from_idl(value: &str) -> Option<Self> {
        Self::KEYWORDS
            .iter()
            .copied()
            .find(|mode| mode.keyword() == value)
    }
}

enumerated::enumerated! {
    /// The keyword set of a present `crossorigin` content attribute, as it
    /// travels on the renderer-to-browser wire
    /// (<https://html.spec.whatwg.org/multipage/urls-and-fetching.html#cors-settings-attributes>).
    ///
    /// This is a wire type, not an attribute parser: an absent attribute is
    /// `None`, the `No CORS` state, and the attribute's invalid value default
    /// (`Anonymous`) is applied where the attribute is read, by
    /// `blitz_traits::net::CorsSetting::parse` in the renderer. A present
    /// attribute is therefore always one of these two keywords here; the
    /// browser process does not link the engine, so it cannot use the
    /// element-level type.
    #[derive(serde::Serialize, serde::Deserialize)]
    #[serde(rename_all = "kebab-case")]
    pub CrossOrigin {
        /// CORS mode with `same-origin` credentials.
        Anonymous => "anonymous",
        /// CORS mode with `include` credentials.
        UseCredentials => "use-credentials",
    }
}

impl CrossOrigin {
    /// The `CORS settings attribute credentials mode`
    /// (<https://html.spec.whatwg.org/multipage/urls-and-fetching.html#cors-settings-attributes>).
    #[must_use]
    pub fn credentials(self) -> CredentialsMode {
        match self {
            Self::Anonymous => CredentialsMode::SameOrigin,
            Self::UseCredentials => CredentialsMode::Include,
        }
    }
}

/// Whether a credentials mode admits credentials for a hop that is (or is
/// not) cross-origin
/// (<https://fetch.spec.whatwg.org/#concept-request-credentials-mode>).
///
/// Every carrier that sends a request with a cookie header applies this rule,
/// so the native agent and the wasm host cannot drift.
#[must_use]
pub fn credentials_admitted(mode: CredentialsMode, cross_origin: bool) -> bool {
    match mode {
        CredentialsMode::Omit => false,
        CredentialsMode::SameOrigin => !cross_origin,
        CredentialsMode::Include => true,
    }
}

/// The CORS parameters a request is sent under, before the browser fills in
/// the origin it authorized.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CorsParameters {
    /// Request mode (<https://fetch.spec.whatwg.org/#concept-request-mode>).
    pub mode: RequestMode,
    /// Credentials mode
    /// (<https://fetch.spec.whatwg.org/#concept-request-credentials-mode>).
    pub credentials: CredentialsMode,
    /// Redirect mode
    /// (<https://fetch.spec.whatwg.org/#concept-request-redirect-mode>).
    pub redirect: RedirectMode,
}

impl CorsParameters {
    /// The same parameters for one authorized origin.
    #[must_use]
    pub fn with_origin(self, origin: url::Origin) -> CorsSettings {
        CorsSettings {
            mode: self.mode,
            credentials: self.credentials,
            redirect: self.redirect,
            origin,
        }
    }
}

/// How a response was filtered for page script
/// (<https://fetch.spec.whatwg.org/#concept-filtered-response>).
///
/// The *internal* response keeps what the server sent. This is which filtered
/// projection a page `fetch` may observe; an engine consumer reads the
/// internal response instead
/// (<https://fetch.spec.whatwg.org/#concept-internal-response>).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filtering {
    /// Same-origin, unfiltered; every field but the forbidden names is
    /// readable.
    Basic,
    /// Passed the CORS check. `credentials_include` decides whether
    /// `Access-Control-Expose-Headers: *` is literal.
    Cors {
        /// Whether the request's credentials mode was `include`.
        credentials_include: bool,
    },
    /// `no-cors` across an origin boundary: no status, URL, headers, or body.
    Opaque,
    /// A `redirect: "manual"` response across an origin boundary: no status or
    /// headers, and the URL is the request's own.
    OpaqueRedirect,
}

impl Filtering {
    /// The response type this projection reports
    /// (<https://fetch.spec.whatwg.org/#concept-response-type>).
    #[must_use]
    pub fn response_type(self) -> ResponseType {
        match self {
            Self::Basic => ResponseType::Basic,
            Self::Cors { .. } => ResponseType::Cors,
            Self::Opaque => ResponseType::Opaque,
            Self::OpaqueRedirect => ResponseType::OpaqueRedirect,
        }
    }

    /// Whether this projection hides the status, URL, headers, and body from
    /// page script.
    #[must_use]
    pub fn is_opaque(self) -> bool {
        matches!(self, Self::Opaque | Self::OpaqueRedirect)
    }
}

enumerated::enumerated! {
    /// How redirects are handled
    /// (<https://fetch.spec.whatwg.org/#concept-request-redirect-mode>).
    #[derive(Default, serde::Serialize, serde::Deserialize)]
    #[serde(rename_all = "kebab-case")]
    pub RedirectMode {
        /// Follow redirects.
        #[default]
        Follow => "follow",
        /// A redirect is a network error.
        Error => "error",
        /// Return an opaque-redirect filtered response; the caller cannot see
        /// the `Location`.
        Manual => "manual",
    }
}

impl RedirectMode {
    /// Parses the IDL enum's string value, which is exact-match
    /// (<https://fetch.spec.whatwg.org/#requestinit>).
    #[must_use]
    pub fn from_idl(value: &str) -> Option<Self> {
        Self::KEYWORDS
            .iter()
            .copied()
            .find(|mode| mode.keyword() == value)
    }
}

/// The fetch parameters of one request. `origin` is the request origin; an
/// opaque origin never equals another origin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CorsSettings {
    /// CORS mode.
    pub mode: RequestMode,
    /// Credentials mode.
    pub credentials: CredentialsMode,
    /// Redirect mode.
    pub redirect: RedirectMode,
    /// Request origin. `SameOrigin` settings gate every hop against it, and a
    /// `cors` hop compares it with the response's
    /// `Access-Control-Allow-Origin` after serialization.
    pub origin: url::Origin,
}

/// A response's filtering type
/// (<https://fetch.spec.whatwg.org/#concept-response-type>), the part the
/// renderer exposes as `Response.type`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ResponseType {
    /// Same-origin, unfiltered.
    #[default]
    Basic,
    /// Passed the CORS check.
    Cors,
    /// `no-cors` across an origin boundary: status and headers are hidden.
    Opaque,
    /// `redirect: "manual"` across an origin boundary: status 0 and no
    /// headers, but `type` is `opaqueredirect`.
    OpaqueRedirect,
}

impl ResponseType {
    /// The IDL value of `Response.type`
    /// (<https://fetch.spec.whatwg.org/#dom-response-type>).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Basic => "basic",
            Self::Cors => "cors",
            Self::Opaque => "opaque",
            Self::OpaqueRedirect => "opaqueredirect",
        }
    }

    /// Whether this type hides the status and body from scripts.
    #[must_use]
    pub fn is_opaque(self) -> bool {
        matches!(self, Self::Opaque | Self::OpaqueRedirect)
    }
}

/// A CORS-safelisted method (§2.2.1): `GET`, `HEAD`, or `POST`.
#[must_use]
pub fn is_cors_safelisted_method(method: &Method) -> bool {
    matches!(method.as_str(), "GET" | "HEAD" | "POST")
}

/// A CORS-unsafe request-header byte: anything below 0x20 other than HT, and a
/// small punctuation set (§2.2.2).
fn is_cors_unsafe_byte(byte: u8) -> bool {
    (byte < 0x20 && byte != 0x09)
        || matches!(
            byte,
            b'"' | b'('
                | b')'
                | b':'
                | b'<'
                | b'>'
                | b'?'
                | b'@'
                | b'['
                | b'\\'
                | b']'
                | b'{'
                | b'}'
                | 0x7F
        )
}

fn contains_unsafe_byte(value: &[u8]) -> bool {
    value.iter().any(|&byte| is_cors_unsafe_byte(byte))
}

/// Whether `(name, value)` is a CORS-safelisted request-header
/// (<https://fetch.spec.whatwg.org/#cors-safelisted-request-header>).
#[must_use]
fn is_cors_safelisted_request_header(name: &str, value: &[u8]) -> bool {
    if value.len() > 128 {
        return false;
    }
    match name.to_ascii_lowercase().as_str() {
        "accept" => !contains_unsafe_byte(value),
        "accept-language" | "content-language" => value.iter().all(|&byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b' ' | b'*' | b',' | b'-' | b'.' | b';' | b'=')
        }),
        "content-type" => {
            if contains_unsafe_byte(value) {
                return false;
            }
            // The essence must be one of the three form types, so
            // `application/json` triggers a preflight
            // (<https://mimesniff.spec.whatwg.org/#mime-type-essence>).
            let essence = String::from_utf8_lossy(value)
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            matches!(
                essence.as_str(),
                "application/x-www-form-urlencoded" | "multipart/form-data" | "text/plain"
            )
        }
        "range" => is_simple_range(value),
        _ => false,
    }
}

/// A no-CORS-safelisted request-header: one of the four names a `no-cors`
/// request may carry, and safelisted by value
/// (<https://fetch.spec.whatwg.org/#no-cors-safelisted-request-header>). The
/// `request-no-cors` guard drops every other name, so a `no-cors` request can
/// only send what a form or `<img>` could
/// (<https://fetch.spec.whatwg.org/#concept-headers-guard>).
#[must_use]
pub fn is_no_cors_safelisted_request_header(name: &str, value: &[u8]) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "accept" | "accept-language" | "content-language" | "content-type"
    ) && is_cors_safelisted_request_header(name, value)
}

/// A single range header value the spec is willing to safelist
/// (<https://fetch.spec.whatwg.org/#simple-range-header-value>): `bytes=N-...`
/// with a numeric start.
fn is_simple_range(value: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(value) else {
        return false;
    };
    let Some(rest) = text.strip_prefix("bytes=") else {
        return false;
    };
    let Some((start, end)) = rest.split_once('-') else {
        return false;
    };
    if start.is_empty() || !start.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    end.is_empty() || end.bytes().all(|byte| byte.is_ascii_digit())
}

/// A forbidden request-header name
/// (<https://fetch.spec.whatwg.org/#forbidden-request-header>). Pages cannot
/// set these, and the browser adds them after the preflight decision, so they
/// never appear in the CORS-unsafe request-header names.
#[must_use]
fn is_forbidden_request_header_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    if name.starts_with("proxy-") || name.starts_with("sec-") {
        return true;
    }
    matches!(
        name.as_str(),
        "accept-charset"
            | "accept-encoding"
            | "access-control-request-headers"
            | "access-control-request-method"
            | "connection"
            | "content-length"
            | "cookie"
            | "cookie2"
            | "date"
            | "dnt"
            | "expect"
            | "host"
            | "keep-alive"
            | "origin"
            | "referer"
            | "set-cookie"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "via"
    )
}

/// Whether a page-supplied request header is forbidden: either its name is
/// forbidden, or it is a method-override header naming a forbidden method
/// (<https://fetch.spec.whatwg.org/#forbidden-request-header>). The JS
/// `Headers` guard and the outbound network filter both answer with this one
/// function, so they cannot disagree about the same header.
#[must_use]
pub fn is_forbidden_request_header(name: &str, value: &[u8]) -> bool {
    is_forbidden_request_header_name(name) || is_forbidden_method_header(name, value)
}

/// Whether a forbidden-method header carries a forbidden method in any
/// comma-separated member
/// (<https://fetch.spec.whatwg.org/#forbidden-request-header>). Members come
/// from the spec's get-decode-split, so a quoted member is compared with its
/// quotes and is not a method.
#[must_use]
fn is_forbidden_method_header(name: &str, value: &[u8]) -> bool {
    if !matches!(
        name.to_ascii_lowercase().as_str(),
        "x-http-method-override" | "x-http-method" | "x-method-override"
    ) {
        return false;
    }
    crate::split_header_value(&crate::isomorphic_decode(value))
        .iter()
        .any(|method| Method::parse(method).is_ok_and(|method| method.is_forbidden()))
}

/// The CORS-unsafe request-header names of `headers`, sorted lowercase
/// (<https://fetch.spec.whatwg.org/#cors-unsafe-request-header-names>).
#[must_use]
pub fn unsafe_request_header_names(headers: &HeaderMap) -> Vec<String> {
    let mut unsafe_names = Vec::new();
    let mut potentially_unsafe = Vec::new();
    let mut safelist_value_size = 0usize;
    for (name, value) in headers.iter() {
        if is_forbidden_request_header_name(name) {
            continue;
        }
        if is_cors_safelisted_request_header(name, value) {
            potentially_unsafe.push(name.to_ascii_lowercase());
            safelist_value_size += value.len();
        } else {
            unsafe_names.push(name.to_ascii_lowercase());
        }
    }
    if safelist_value_size > 1024 {
        unsafe_names.append(&mut potentially_unsafe);
    }
    unsafe_names.sort();
    unsafe_names.dedup();
    unsafe_names
}

/// Whether a request with this method and header list needs a preflight
/// (<https://fetch.spec.whatwg.org/#cors-preflight-fetch>).
#[must_use]
pub fn needs_preflight(method: &Method, unsafe_names: &[String]) -> bool {
    !is_cors_safelisted_method(method) || !unsafe_names.is_empty()
}

/// Every value of `name`, joined with `", "`, decoded
/// (<https://fetch.spec.whatwg.org/#concept-header-list-get>). A non-UTF-8
/// value makes the header unusable for the CORS check.
fn header(headers: &HeaderMap, name: &str) -> Option<String> {
    let mut combined: Option<String> = None;
    for value in headers.get_all(name) {
        let text = std::str::from_utf8(value).ok()?;
        match &mut combined {
            Some(existing) => {
                existing.push_str(", ");
                existing.push_str(text);
            }
            None => combined = Some(text.to_owned()),
        }
    }
    combined
}

/// The values of `name` split on commas and trimmed
/// (<https://fetch.spec.whatwg.org/#header-value-get-decode-and-split>).
fn list_values(headers: &HeaderMap, name: &str) -> Vec<String> {
    crate::get_decode_split(headers, name)
        .unwrap_or_default()
        .into_iter()
        .filter(|value| !value.is_empty())
        .collect()
}

/// The CORS check
/// (<https://fetch.spec.whatwg.org/#cors-check>). `origin` is the serialized
/// request origin.
#[must_use]
pub fn cors_check(headers: &HeaderMap, origin: &str, credentials_include: bool) -> bool {
    let Some(allow_origin) = header(headers, "access-control-allow-origin") else {
        return false;
    };
    let allow_origin = allow_origin.trim();
    if !credentials_include && allow_origin == "*" {
        return true;
    }
    if allow_origin != origin {
        return false;
    }
    if !credentials_include {
        return true;
    }
    // A credentialed response must also opt in, or the caller cannot read it.
    header(headers, "access-control-allow-credentials")
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("true"))
}

/// The non-empty items of a list header
/// (<https://fetch.spec.whatwg.org/#header-value-get-decode-and-split>).
fn list_items(headers: &HeaderMap, name: &str) -> Vec<String> {
    crate::get_decode_split(headers, name)
        .unwrap_or_default()
        .into_iter()
        .filter(|item| !item.is_empty())
        .collect()
}

/// Extracts a `#method` list header, `None` on an item that is not an HTTP
/// token.
fn list_methods(headers: &HeaderMap, name: &str) -> Option<Vec<http::Method>> {
    list_items(headers, name)
        .into_iter()
        .map(|item| http::Method::from_bytes(item.as_bytes()).ok())
        .collect()
}

/// Extracts a `#field-name` list header, `None` on an item that is not an
/// HTTP token.
fn list_header_names(headers: &HeaderMap, name: &str) -> Option<Vec<header::HeaderName>> {
    list_items(headers, name)
        .into_iter()
        .map(|item| header::HeaderName::from_bytes(item.as_bytes()).ok())
        .collect()
}

/// Validates a preflight response
/// (<https://fetch.spec.whatwg.org/#cors-preflight-fetch>).
#[must_use]
pub fn preflight_ok(
    status: u16,
    headers: &HeaderMap,
    origin: &str,
    credentials_include: bool,
    method: &Method,
    unsafe_names: &[String],
) -> bool {
    if !(200..=299).contains(&status) {
        return false;
    }
    if !cors_check(headers, origin, credentials_include) {
        return false;
    }
    // Method: a safelisted method needs no listing; anything else must be
    // named, or `*` when credentials are not included. A malformed
    // `Access-Control-Allow-Methods` fails the preflight
    // (<https://fetch.spec.whatwg.org/#cors-preflight-fetch>, steps 6, 12).
    let Some(methods) = list_methods(headers, "access-control-allow-methods") else {
        return false;
    };
    let wildcard_ok = !credentials_include;
    let method_allowed = methods.iter().any(|value| value == method.as_http())
        || is_cors_safelisted_method(method)
        || (wildcard_ok && methods.iter().any(|value| value.as_str() == "*"));
    if !method_allowed {
        return false;
    }
    // Headers: `Authorization` (the CORS non-wildcard name) must be listed
    // explicitly; every other unsafe name must be listed, or covered by `*`
    // when credentials are not included. A malformed
    // `Access-Control-Allow-Headers` fails the preflight
    // (<https://fetch.spec.whatwg.org/#cors-preflight-fetch>, steps 7, 13-14).
    let Some(allowed) = list_header_names(headers, "access-control-allow-headers") else {
        return false;
    };
    let listed = |name: &str| allowed.iter().any(|value| value == name);
    if unsafe_names.iter().any(|name| name == "authorization") && !listed("authorization") {
        return false;
    }
    let wildcard = wildcard_ok && listed("*");
    wildcard || unsafe_names.iter().all(|name| listed(name))
}

/// The outbound headers of a preflight request
/// (<https://fetch.spec.whatwg.org/#cors-preflight-fetch>). The spec fixes
/// `Accept: */*` on the preflight, independent of the original request.
///
/// Infallible: [`Method`] and a [`HeaderMap`] name are token-validated where
/// they are built, so every value here is already a field value.
pub(crate) fn preflight_headers(
    method: &Method,
    unsafe_names: &[String],
    origin: &HeaderValue,
) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.append(header::ACCEPT, HeaderValue::from_static("*/*"));
    headers.append(header::ORIGIN, origin.clone());
    headers.append(
        header::ACCESS_CONTROL_REQUEST_METHOD,
        HeaderValue::from_str(method.as_str()).expect("a method is an HTTP token"),
    );
    if !unsafe_names.is_empty() {
        headers.append(
            header::ACCESS_CONTROL_REQUEST_HEADERS,
            HeaderValue::from_str(&unsafe_names.join(",")).expect("a field name is an HTTP token"),
        );
    }
    headers
}

/// Whether `name` is a forbidden response-header name, which page script can
/// never read
/// (<https://fetch.spec.whatwg.org/#forbidden-response-header-name>).
#[must_use]
pub fn is_forbidden_response_header(name: &str) -> bool {
    name.eq_ignore_ascii_case("set-cookie") || name.eq_ignore_ascii_case("set-cookie2")
}

fn is_cors_safelisted_response_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "cache-control"
            | "content-language"
            | "content-length"
            | "content-type"
            | "expires"
            | "last-modified"
            | "pragma"
    )
}

/// The rule a `cors` response applies to each header name: CORS-safelisted
/// response headers plus `Access-Control-Expose-Headers`, minus the forbidden
/// response-header names
/// (<https://fetch.spec.whatwg.org/#cors-safelisted-response-header-name>).
///
/// Built once from the internal header list, then asked about each name, so a
/// filtered view does not clone the list it is filtering.
pub struct ExposedHeaders {
    /// `Access-Control-Expose-Headers` names, lowercased.
    names: Vec<String>,
    /// `Access-Control-Expose-Headers: *` without credentials.
    expose_all: bool,
}

impl ExposedHeaders {
    /// Reads the expose rule out of one response's header list.
    #[must_use]
    pub fn new(headers: &HeaderMap, credentials_include: bool) -> Self {
        let names: Vec<String> = list_values(headers, "access-control-expose-headers")
            .iter()
            .map(|name| name.to_ascii_lowercase())
            .collect();
        let expose_all = !credentials_include && names.iter().any(|name| name == "*");
        Self { names, expose_all }
    }

    /// Whether this rule exposes the field named `name`.
    #[must_use]
    pub fn keeps(&self, name: &str) -> bool {
        if is_forbidden_response_header(name) {
            return false;
        }
        let name = name.to_ascii_lowercase();
        is_cors_safelisted_response_header(&name)
            || self.expose_all
            || self.names.iter().any(|exposed| exposed == &name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            let _ = map.insert(name, value.as_bytes());
        }
        map
    }

    #[test]
    fn safelisted_methods_and_headers() {
        assert!(is_cors_safelisted_method(&Method::GET));
        assert!(is_cors_safelisted_method(&Method::POST));
        assert!(!is_cors_safelisted_method(&Method::PUT));
        assert!(is_cors_safelisted_request_header(
            "Content-Type",
            b"text/plain"
        ));
        assert!(is_cors_safelisted_request_header(
            "content-type",
            b"text/plain;charset=UTF-8"
        ));
        assert!(!is_cors_safelisted_request_header(
            "content-type",
            b"application/json"
        ));
        assert!(!is_cors_safelisted_request_header("X-Custom", b"1"));
        assert!(!is_cors_safelisted_request_header(
            "content-language",
            b"a@b"
        ));
    }

    #[test]
    fn no_cors_safelist_is_names_and_values() {
        assert!(is_no_cors_safelisted_request_header(
            "Accept",
            b"text/plain"
        ));
        assert!(is_no_cors_safelisted_request_header(
            "content-type",
            b"application/x-www-form-urlencoded"
        ));
        // The value safelist still applies within the four names.
        assert!(!is_no_cors_safelisted_request_header(
            "content-type",
            b"application/json"
        ));
        // Every other name is refused, safelisted or not.
        assert!(!is_no_cors_safelisted_request_header("X-Custom", b"1"));
        assert!(!is_no_cors_safelisted_request_header("range", b"bytes 0-"));
    }

    #[test]
    fn method_override_members_use_the_header_value_splitter() {
        assert!(is_forbidden_method_header("x-http-method", b" trace , get"));
        assert!(is_forbidden_method_header("x-http-method", b"\ttrack\t"));
        assert!(is_forbidden_method_header("x-http-method", b"get,TRACK"));
        assert!(!is_forbidden_method_header("x-method-override", b"get"));
        assert!(!is_forbidden_method_header(
            "X-HTTP-Method-Override",
            b"connect foo"
        ));
        assert!(!is_forbidden_method_header(
            "x-http-method-override",
            b"connect\x0b"
        ));
        assert!(!is_forbidden_method_header(
            "x-http-method-override",
            br#""a,trace""#
        ));
    }

    #[test]
    fn unsafe_names_are_sorted_lowercase() {
        let list = headers(&[
            ("X-B", "1"),
            ("Content-Type", "application/json"),
            ("X-A", "1"),
        ]);
        assert_eq!(
            unsafe_request_header_names(&list),
            vec![
                "content-type".to_owned(),
                "x-a".to_owned(),
                "x-b".to_owned()
            ]
        );
        assert!(needs_preflight(
            &Method::GET,
            &unsafe_request_header_names(&list)
        ));
        assert!(!needs_preflight(&Method::GET, &[]));
        assert!(needs_preflight(&Method::DELETE, &[]));
    }

    #[test]
    fn cors_check_wildcard_and_credentials() {
        let wildcard = headers(&[("Access-Control-Allow-Origin", "*")]);
        assert!(cors_check(&wildcard, "https://a.test", false));
        assert!(!cors_check(&wildcard, "https://a.test", true));
        // A credentialed response must also send
        // `Access-Control-Allow-Credentials: true`
        // (<https://fetch.spec.whatwg.org/#cors-check>, steps 5-8).
        let echo = headers(&[("Access-Control-Allow-Origin", "https://a.test")]);
        assert!(!cors_check(&echo, "https://a.test", true));
        assert!(!cors_check(&echo, "https://b.test", false));
        let credentialed = headers(&[
            ("Access-Control-Allow-Origin", "https://a.test"),
            ("Access-Control-Allow-Credentials", "true"),
        ]);
        assert!(cors_check(&credentialed, "https://a.test", true));
        assert!(!cors_check(&headers(&[]), "https://a.test", false));
    }

    #[test]
    fn preflight_requires_method_and_headers() {
        let response = headers(&[
            ("Access-Control-Allow-Origin", "https://a.test"),
            ("Access-Control-Allow-Methods", "PUT, DELETE"),
            ("Access-Control-Allow-Headers", "X-Custom"),
        ]);
        assert!(preflight_ok(
            204,
            &response,
            "https://a.test",
            false,
            &Method::PUT,
            &["x-custom".to_owned()]
        ));
        assert!(!preflight_ok(
            204,
            &response,
            "https://a.test",
            false,
            &Method::PATCH,
            &[]
        ));
        assert!(!preflight_ok(
            204,
            &response,
            "https://a.test",
            false,
            &Method::PUT,
            &["x-other".to_owned()]
        ));
        // Credentials require Access-Control-Allow-Credentials: true.
        assert!(!preflight_ok(
            204,
            &response,
            "https://a.test",
            true,
            &Method::PUT,
            &["x-custom".to_owned()]
        ));
    }

    #[test]
    fn preflight_wildcards_decline_authorization() {
        let response = headers(&[
            ("Access-Control-Allow-Origin", "https://a.test"),
            ("Access-Control-Allow-Methods", "*"),
            ("Access-Control-Allow-Headers", "*"),
        ]);
        assert!(preflight_ok(
            200,
            &response,
            "https://a.test",
            false,
            &Method::PATCH,
            &["x-custom".to_owned()]
        ));
        assert!(!preflight_ok(
            200,
            &response,
            "https://a.test",
            false,
            &Method::PATCH,
            &["authorization".to_owned()]
        ));
    }

    #[test]
    fn preflight_safelisted_method_needs_no_allow_methods() {
        let response = headers(&[
            ("Access-Control-Allow-Origin", "*"),
            ("Access-Control-Allow-Headers", "x-custom"),
        ]);
        assert!(preflight_ok(
            200,
            &response,
            "https://a.test",
            false,
            &Method::GET,
            &["x-custom".to_owned()]
        ));
        // A non-safelisted method still needs a match.
        assert!(!preflight_ok(
            200,
            &response,
            "https://a.test",
            false,
            &Method::PUT,
            &[]
        ));
    }

    #[test]
    fn preflight_authorization_needs_explicit_listing() {
        let response = headers(&[
            ("Access-Control-Allow-Origin", "*"),
            ("Access-Control-Allow-Headers", "*, Authorization"),
        ]);
        assert!(preflight_ok(
            200,
            &response,
            "https://a.test",
            false,
            &Method::GET,
            &["authorization".to_owned()]
        ));
    }

    #[test]
    fn malformed_allow_headers_or_methods_fails() {
        let bad_headers = headers(&[
            ("Access-Control-Allow-Origin", "*"),
            (
                "Access-Control-Allow-Headers",
                "x-force-preflight, Bad value",
            ),
        ]);
        assert!(!preflight_ok(
            200,
            &bad_headers,
            "https://a.test",
            false,
            &Method::GET,
            &["x-force-preflight".to_owned()]
        ));
        let bad_methods = headers(&[
            ("Access-Control-Allow-Origin", "*"),
            ("Access-Control-Allow-Methods", "Bad value"),
        ]);
        assert!(!preflight_ok(
            200,
            &bad_methods,
            "https://a.test",
            false,
            &Method::PUT,
            &[]
        ));
    }

    #[test]
    fn preflight_request_carries_accept() {
        let request = preflight_headers(
            &Method::PUT,
            &["x-custom".to_owned()],
            &HeaderValue::from_static("https://a.test"),
        );
        assert_eq!(request.get("accept"), Some(b"*/*".as_slice()));
        assert_eq!(request.get("origin"), Some(b"https://a.test".as_slice()));
        assert_eq!(
            request.get("access-control-request-method"),
            Some(b"PUT".as_slice())
        );
        assert_eq!(
            request.get("access-control-request-headers"),
            Some(b"x-custom".as_slice())
        );
    }

    #[test]
    fn forbidden_names_do_not_trigger_a_preflight() {
        // `Referer` and the `Sec-`/`Proxy-` families are forbidden, so the
        // browser-added values never enter the preflight decision. The
        // browser also adds `User-Agent` after this point, so it is absent
        // from the header list the decision sees
        // (<https://fetch.spec.whatwg.org/#forbidden-request-header>).
        let request = headers(&[("Referer", "https://a.test/"), ("Sec-Fetch-Mode", "cors")]);
        assert_eq!(unsafe_request_header_names(&request), [] as [String; 0]);
        let request = headers(&[("X-Custom", "1")]);
        assert_eq!(
            unsafe_request_header_names(&request),
            vec!["x-custom".to_owned()]
        );
    }

    #[test]
    fn exposed_headers_filter_and_hide_set_cookie() {
        let response = headers(&[
            ("Content-Type", "application/json"),
            ("Cache-Control", "no-store"),
            ("X-Exposed", "1"),
            ("X-Hidden", "2"),
            ("Access-Control-Expose-Headers", "X-Exposed"),
            ("Set-Cookie", "a=b"),
        ]);
        let rule = ExposedHeaders::new(&response, false);
        assert!(rule.keeps("content-type"));
        assert!(rule.keeps("x-exposed"), "an exposed name is readable");
        assert!(!rule.keeps("x-hidden"));
        assert!(!rule.keeps("set-cookie"));
        assert!(!rule.keeps("access-control-expose-headers"));
    }

    #[test]
    fn expose_star_is_literal_with_credentials() {
        let response = headers(&[("X-Thing", "1"), ("Access-Control-Expose-Headers", "*")]);
        assert!(ExposedHeaders::new(&response, false).keeps("x-thing"));
        assert!(
            !ExposedHeaders::new(&response, true).keeps("x-thing"),
            "a credentialed request treats `*` as a literal name"
        );
    }
}
