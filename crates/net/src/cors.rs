// https://fetch.spec.whatwg.org/#http-cors-protocol

use enumerated::Enumerated;
use http::header::{self, HeaderValue};

use crate::protocol::{HeaderMap, Method};

// https://fetch.spec.whatwg.org/#concept-request-mode
#[derive(
    Clone, Copy, Debug, Default, Hash, PartialEq, Eq, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum RequestMode {
    Navigate,
    #[default]
    NoCors,
    SameOrigin,
    Cors,
}

impl RequestMode {
    // https://fetch.spec.whatwg.org/#requestinit
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
    // https://fetch.spec.whatwg.org/#concept-request-credentials-mode
    #[derive(Default, Hash, serde::Serialize, serde::Deserialize)]
    #[serde(rename_all = "kebab-case")]
    pub CredentialsMode {
        Omit => "omit",
        #[default]
        SameOrigin => "same-origin",
        Include => "include",
    }
}

impl CredentialsMode {
    #[must_use]
    pub fn includes(self) -> bool {
        matches!(self, Self::Include)
    }

    // https://fetch.spec.whatwg.org/#requestinit
    #[must_use]
    pub fn from_idl(value: &str) -> Option<Self> {
        Self::KEYWORDS
            .iter()
            .copied()
            .find(|mode| mode.keyword() == value)
    }
}

enumerated::enumerated! {
    // https://html.spec.whatwg.org/multipage/urls-and-fetching.html#cors-settings-attributes
    #[derive(serde::Serialize, serde::Deserialize)]
    #[serde(rename_all = "kebab-case")]
    pub CrossOrigin {
        Anonymous => "anonymous",
        UseCredentials => "use-credentials",
    }
}

impl CrossOrigin {
    // https://html.spec.whatwg.org/multipage/urls-and-fetching.html#cors-settings-attributes
    #[must_use]
    pub fn credentials(self) -> CredentialsMode {
        match self {
            Self::Anonymous => CredentialsMode::SameOrigin,
            Self::UseCredentials => CredentialsMode::Include,
        }
    }
}

// https://fetch.spec.whatwg.org/#concept-request-credentials-mode
#[must_use]
pub fn credentials_admitted(mode: CredentialsMode, cross_origin: bool) -> bool {
    match mode {
        CredentialsMode::Omit => false,
        CredentialsMode::SameOrigin => !cross_origin,
        CredentialsMode::Include => true,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CorsParameters {
    // https://fetch.spec.whatwg.org/#concept-request-mode
    pub mode: RequestMode,
    // https://fetch.spec.whatwg.org/#concept-request-credentials-mode
    pub credentials: CredentialsMode,
    // https://fetch.spec.whatwg.org/#concept-request-redirect-mode
    pub redirect: RedirectMode,
}

impl CorsParameters {
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

// https://fetch.spec.whatwg.org/#concept-filtered-response
// https://fetch.spec.whatwg.org/#concept-internal-response
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filtering {
    Basic,
    Cors { credentials_include: bool },
    Opaque,
    OpaqueRedirect,
}

impl Filtering {
    // https://fetch.spec.whatwg.org/#concept-response-type
    #[must_use]
    pub fn response_type(self) -> ResponseType {
        match self {
            Self::Basic => ResponseType::Basic,
            Self::Cors { .. } => ResponseType::Cors,
            Self::Opaque => ResponseType::Opaque,
            Self::OpaqueRedirect => ResponseType::OpaqueRedirect,
        }
    }

    #[must_use]
    pub fn is_opaque(self) -> bool {
        matches!(self, Self::Opaque | Self::OpaqueRedirect)
    }
}

enumerated::enumerated! {
    // https://fetch.spec.whatwg.org/#concept-request-redirect-mode
    #[derive(Default, serde::Serialize, serde::Deserialize)]
    #[serde(rename_all = "kebab-case")]
    pub RedirectMode {
        #[default]
        Follow => "follow",
        Error => "error",
        Manual => "manual",
    }
}

impl RedirectMode {
    // https://fetch.spec.whatwg.org/#requestinit
    #[must_use]
    pub fn from_idl(value: &str) -> Option<Self> {
        Self::KEYWORDS
            .iter()
            .copied()
            .find(|mode| mode.keyword() == value)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CorsSettings {
    pub mode: RequestMode,
    pub credentials: CredentialsMode,
    pub redirect: RedirectMode,
    pub origin: url::Origin,
}

// https://fetch.spec.whatwg.org/#concept-response-type
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ResponseType {
    #[default]
    Basic,
    Cors,
    Opaque,
    OpaqueRedirect,
}

impl ResponseType {
    // https://fetch.spec.whatwg.org/#dom-response-type
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Basic => "basic",
            Self::Cors => "cors",
            Self::Opaque => "opaque",
            Self::OpaqueRedirect => "opaqueredirect",
        }
    }

    #[must_use]
    pub fn is_opaque(self) -> bool {
        matches!(self, Self::Opaque | Self::OpaqueRedirect)
    }
}

#[must_use]
pub fn is_cors_safelisted_method(method: &Method) -> bool {
    matches!(method.as_str(), "GET" | "HEAD" | "POST")
}

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

// https://fetch.spec.whatwg.org/#cors-safelisted-request-header
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
            // https://mimesniff.spec.whatwg.org/#mime-type-essence
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

// https://fetch.spec.whatwg.org/#no-cors-safelisted-request-header
// https://fetch.spec.whatwg.org/#concept-headers-guard
#[must_use]
pub fn is_no_cors_safelisted_request_header(name: &str, value: &[u8]) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "accept" | "accept-language" | "content-language" | "content-type"
    ) && is_cors_safelisted_request_header(name, value)
}

// https://fetch.spec.whatwg.org/#simple-range-header-value
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

// https://fetch.spec.whatwg.org/#forbidden-request-header
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

// https://fetch.spec.whatwg.org/#forbidden-request-header
#[must_use]
pub fn is_forbidden_request_header(name: &str, value: &[u8]) -> bool {
    is_forbidden_request_header_name(name) || is_forbidden_method_header(name, value)
}

// https://fetch.spec.whatwg.org/#forbidden-request-header
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

// https://fetch.spec.whatwg.org/#cors-unsafe-request-header-names
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

// https://fetch.spec.whatwg.org/#cors-preflight-fetch
#[must_use]
pub fn needs_preflight(method: &Method, unsafe_names: &[String]) -> bool {
    !is_cors_safelisted_method(method) || !unsafe_names.is_empty()
}

// https://fetch.spec.whatwg.org/#concept-header-list-get
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

// https://fetch.spec.whatwg.org/#header-value-get-decode-and-split
fn list_values(headers: &HeaderMap, name: &str) -> Vec<String> {
    crate::get_decode_split(headers, name)
        .unwrap_or_default()
        .into_iter()
        .filter(|value| !value.is_empty())
        .collect()
}

// https://fetch.spec.whatwg.org/#cors-check
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
    header(headers, "access-control-allow-credentials")
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("true"))
}

fn list_methods(headers: &HeaderMap, name: &str) -> Option<Vec<http::Method>> {
    list_values(headers, name)
        .into_iter()
        .map(|item| http::Method::from_bytes(item.as_bytes()).ok())
        .collect()
}

fn list_header_names(headers: &HeaderMap, name: &str) -> Option<Vec<header::HeaderName>> {
    list_values(headers, name)
        .into_iter()
        .map(|item| header::HeaderName::from_bytes(item.as_bytes()).ok())
        .collect()
}

// https://fetch.spec.whatwg.org/#cors-preflight-fetch
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
    // https://fetch.spec.whatwg.org/#cors-preflight-fetch
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
    // https://fetch.spec.whatwg.org/#cors-preflight-fetch
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

// https://fetch.spec.whatwg.org/#cors-preflight-fetch
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

// https://fetch.spec.whatwg.org/#forbidden-response-header-name
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

// https://fetch.spec.whatwg.org/#cors-safelisted-response-header-name
pub struct ExposedHeaders {
    names: Vec<String>,
    expose_all: bool,
}

impl ExposedHeaders {
    #[must_use]
    pub fn new(headers: &HeaderMap, credentials_include: bool) -> Self {
        let names: Vec<String> = list_values(headers, "access-control-expose-headers")
            .iter()
            .map(|name| name.to_ascii_lowercase())
            .collect();
        let expose_all = !credentials_include && names.iter().any(|name| name == "*");
        Self { names, expose_all }
    }

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
        assert!(!is_no_cors_safelisted_request_header(
            "content-type",
            b"application/json"
        ));
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
        // https://fetch.spec.whatwg.org/#cors-check
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
        // https://fetch.spec.whatwg.org/#forbidden-request-header
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
