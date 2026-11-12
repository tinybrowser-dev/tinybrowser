//! Cookie storage and the site identity its rules are written in terms of.
//!
//! A [`CookieJar`] is pure logic over URLs and time: no sockets, no
//! filesystem, no async runtime. Both the native browser process and the
//! WebAssembly component keep one, so the storage model, `SameSite` handling,
//! and public-suffix checks live here once.
//!
//! [`site`] answers which registrable domain a URL belongs to, which is also
//! what renderer isolation locks on; the two uses share one definition of site
//! on purpose, because they disagreeing would be a security bug.
//!
//! Behavior follows
//! [draft-ietf-httpbis-rfc6265bis](https://httpwg.org/http-extensions/draft-ietf-httpbis-rfc6265bis.html).

use std::collections::HashMap;
use std::fmt;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use publicsuffix::Psl;

use url::{Host, Origin, Url};

// https://httpwg.org/http-extensions/draft-ietf-httpbis-rfc6265bis.html#name-cookie-lifetime-limits
const MAX_LIFETIME: Duration = Duration::from_hours(9600);
// https://httpwg.org/http-extensions/draft-ietf-httpbis-rfc6265bis.html#name-storage-model
const MAX_COOKIES_PER_DOMAIN: usize = 50;
const MAX_COOKIES: usize = 3000;

/// Which initiator owns an HTTP request.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum InitiatorKind {
    /// Top-level navigation.
    #[default]
    Navigation,
    /// Scripted `fetch()`.
    Fetch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SameSite {
    Strict,
    Lax,
    None,
    Default,
}

/// Which `SameSite` cookies one operation may read or write.
///
/// A `SharedWorker`'s `sameSiteCookies` option restricts every request and
/// response its global scope triggers. `none` permits only `SameSite=None`
/// cookies; `all` is the ordinary jar behavior
/// (<https://privacycg.github.io/saa-non-cookie-storage/#shared-worker>).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SameSiteCookiePolicy {
    /// Every cookie the request's own `SameSite` rules allow.
    #[default]
    All,
    /// Only the cookies marked `SameSite=None`.
    NoneOnly,
}

#[derive(Clone)]
struct StoredCookie {
    name: String,
    value: String,
    expiry: Option<SystemTime>,
    domain: String,
    path: String,
    created: SystemTime,
    last_access: SystemTime,
    host_only: bool,
    secure: bool,
    http_only: bool,
    same_site: SameSite,
    /// The top-level site this cookie is partitioned under, or `None` for an
    /// ordinary unpartitioned cookie
    /// (<https://github.com/privacycg/CHIPS>).
    partition_key: Option<String>,
}

/// The cookies one browser profile holds, keyed by domain and path.
///
/// The jar has no clock of its own: every operation takes `now` from its
/// [`CookieOp`], so callers stay in control of time and tests stay
/// deterministic.
#[derive(Clone, Default)]
pub struct CookieJar {
    cookies: Vec<StoredCookie>,
}

impl fmt::Debug for CookieJar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CookieJar")
            .field("len", &self.cookies.len())
            .finish()
    }
}

/// Everything one cookie operation needs that the jar cannot infer.
#[derive(Clone, Copy)]
pub struct CookieOp<'a> {
    /// URL being requested, or the URL a `Set-Cookie` line arrived from.
    pub url: &'a Url,
    /// Wall clock to evaluate expiry against.
    pub now: SystemTime,
    /// Whether the operation reached the jar over HTTP or through script.
    ///
    /// Script cannot see or set `HttpOnly` cookies.
    pub kind: RetrievalKind,
    /// What the request is for; `SameSite=Lax` treats a top-level navigation
    /// differently from a subresource request.
    pub initiator_kind: InitiatorKind,
    /// Whether the request's method is safe per
    /// [RFC 9110](https://www.rfc-editor.org/rfc/rfc9110#section-9.2.1):
    /// `GET`, `HEAD`, `OPTIONS`, or `TRACE`.
    ///
    /// Only consulted for a top-level navigation, where `SameSite=Lax`
    /// cookies go with a cross-site request only when its method is safe;
    /// callers must pass the real method's safety.
    pub method_is_safe: bool,
    /// The request's initiator origin, or `None` when the user agent started
    /// it: a request with no initiating context is same-site
    /// (<https://httpwg.org/http-extensions/draft-ietf-httpbis-rfc6265bis.html#name-same-site-requests>).
    pub initiator: Option<&'a Origin>,
    /// Whether a redirect chain has crossed sites before this request.
    pub cross_site_redirect: bool,
    /// Which `SameSite` values this operation may use.
    pub same_site_policy: SameSiteCookiePolicy,
    /// The site of the top-level document this operation belongs to, which is
    /// the partition key a `Partitioned` cookie is stored under and matched
    /// against. `None` when the caller has no top-level site (a `WebDriver`
    /// view, a local scheme)
    /// (<https://github.com/privacycg/CHIPS#algorithm>).
    pub partition_key: Option<&'a str>,
}

/// How a cookie operation reached the jar.
#[derive(Clone, Copy)]
pub enum RetrievalKind {
    /// An HTTP request or response, which can carry `HttpOnly` cookies.
    Http,
    /// The `document.cookie` getter or setter, which cannot.
    NonHttp,
}

impl CookieJar {
    /// Stores one `Set-Cookie` line, reporting whether it was stored.
    ///
    /// Rejection is not an error: a malformed line, a public-suffix domain, or
    /// a `SameSite` cookie set from a cross-site context all mean "keep the
    /// existing state".
    pub fn store(&mut self, set_cookie: &str, op: CookieOp<'_>) -> bool {
        let Some(parsed) = parse_set_cookie(set_cookie) else {
            return false;
        };
        self.store_parsed(parsed, op)
    }

    /// Stores one cookie a `CookieStore` operation already validated and whose
    /// attributes are typed, not a `Set-Cookie` line. The Cookie Store API's
    /// step 20 appends the `Path` value verbatim, which a line cannot carry
    /// when the value holds U+003B (;): the line parser would read it as the
    /// start of the next attribute
    /// (<https://cookiestore.spec.whatwg.org/#set-a-cookie>).
    pub fn store_script(&mut self, write: &ScriptCookieWrite, op: CookieOp<'_>) -> bool {
        self.store_parsed(
            ParsedSetCookie {
                name: write.name.clone(),
                value: write.value.clone(),
                max_age: write.max_age,
                expires: write.expires_millis.map(unix_millis),
                domain: write.domain.clone(),
                path: Some(write.path.clone()),
                secure: true,
                http_only: false,
                same_site: Some(write.same_site.into()),
                partitioned: write.partitioned,
            },
            op,
        )
    }

    fn store_parsed(&mut self, parsed: ParsedSetCookie, op: CookieOp<'_>) -> bool {
        self.evict_expired(op.now);
        let trustworthy = secure_context::is_potentially_trustworthy(op.url);
        let Some(stored) = receive_cookie(parsed, &op, &self.cookies, trustworthy) else {
            return false;
        };
        // A scope restricted to `SameSite=None` also refuses to store the
        // other values: a `Set-Cookie` its requests carry is as much a cookie
        // operation as a read
        // (<https://privacycg.github.io/saa-non-cookie-storage/#shared-worker>).
        if op.same_site_policy == SameSiteCookiePolicy::NoneOnly
            && stored.same_site != SameSite::None
        {
            return false;
        }
        self.cookies
            .retain(|old| !same_cookie_identity(old, &stored));
        if stored.expiry.is_none_or(|exp| exp > op.now) {
            self.cookies.push(stored);
            self.evict_excess();
        }
        true
    }

    /// The `Cookie` header value for `op`: matching cookies, longest path
    /// first, older cookies first within one path.
    pub fn cookie_string(&mut self, op: CookieOp<'_>) -> String {
        self.evict_expired(op.now);
        let Some(host) = canonicalize_host(op.url) else {
            return String::new();
        };
        let path = op.url.path();
        let trustworthy = secure_context::is_potentially_trustworthy(op.url);
        let mut matched: Vec<usize> = self
            .cookies
            .iter()
            .enumerate()
            .filter(|(_, cookie)| cookie.matches(&host, path, &op, trustworthy))
            .map(|(i, _)| i)
            .collect();
        for &i in &matched {
            self.cookies[i].last_access = op.now;
        }
        matched.sort_by(|&a, &b| {
            let left = &self.cookies[a];
            let right = &self.cookies[b];
            right
                .path
                .len()
                .cmp(&left.path.len())
                .then_with(|| left.created.cmp(&right.created))
        });
        let mut out = String::new();
        for (i, &idx) in matched.iter().enumerate() {
            let cookie = &self.cookies[idx];
            if i > 0 {
                out.push_str("; ");
            }
            if !cookie.name.is_empty() {
                out.push_str(&cookie.name);
                out.push('=');
            }
            out.push_str(&cookie.value);
        }
        out
    }

    /// Cookies that would be sent for `op`, including session and `HttpOnly`
    /// cookies. This is the `WebDriver` view of the jar
    /// (<https://w3c.github.io/webdriver/#get-all-cookies>).
    ///
    /// The order is the retrieval model's: longest path first, then oldest
    /// first, which is what `CookieStore.get()` returns as its first item
    /// (<https://cookiestore.spec.whatwg.org/#query-cookies>,
    /// <https://httpwg.org/http-extensions/draft-ietf-httpbis-rfc6265bis.html#section-5.4>).
    pub fn records_for(&mut self, op: CookieOp<'_>) -> Vec<CookieRecord> {
        self.evict_expired(op.now);
        let Some(host) = canonicalize_host(op.url) else {
            return Vec::new();
        };
        let path = op.url.path();
        let trustworthy = secure_context::is_potentially_trustworthy(op.url);
        let mut matched: Vec<&StoredCookie> = self
            .cookies
            .iter()
            .filter(|cookie| cookie.matches(&host, path, &op, trustworthy))
            .collect();
        matched.sort_by(|left, right| {
            right
                .path
                .len()
                .cmp(&left.path.len())
                .then_with(|| left.created.cmp(&right.created))
        });
        matched.into_iter().map(StoredCookie::record).collect()
    }

    /// Drops every cookie, session or persistent
    /// (<https://w3c.github.io/webdriver/#delete-all-cookies>).
    pub fn clear(&mut self) {
        self.cookies.clear();
    }

    fn evict_expired(&mut self, now: SystemTime) {
        self.cookies
            .retain(|cookie| cookie.expiry.is_none_or(|exp| exp > now));
    }

    fn evict_excess(&mut self) {
        while let Some(domain) = over_quota_domain(&self.cookies) {
            if !evict_one(&mut self.cookies, Some(&domain)) {
                break;
            }
        }
        while self.cookies.len() > MAX_COOKIES {
            if !evict_one(&mut self.cookies, None) {
                break;
            }
        }
    }

    /// Persistent cookies from the live jar. Session cookies are omitted.
    #[must_use]
    pub fn snapshot(&self) -> Vec<CookieRecord> {
        self.cookies
            .iter()
            .filter(|cookie| cookie.expiry.is_some())
            .map(StoredCookie::record)
            .collect()
    }

    /// Loads `records` into the live jar, replacing matching identities.
    ///
    /// Records come from storage a caller controls, so each one is re-checked
    /// against the rules [`CookieJar::store`] applies to a parsed line: a
    /// name and value the `Cookie` header can carry (not both empty, no
    /// control characters other than TAB, no `;` in either, no `=` in the
    /// name, at most 4096 bytes together), a rooted path, a canonical domain
    /// of at most 1024 bytes that is not a public suffix or IP address unless
    /// host-only, the prefix requirements, `SameSite=None` needing `Secure`,
    /// and no weakening of a live cookie's `Secure` flag. Unlike
    /// [`CookieJar::store`], that last rule holds even for a secure request: a
    /// record has no request URL to judge. Expired records are dropped, expiry
    /// is capped, and the storage-model caps apply. Rejected records are
    /// skipped without error.
    pub fn restore(&mut self, records: Vec<CookieRecord>, now: SystemTime) {
        // Expired cookies are gone before anything looks at the live jar, so a
        // dead cookie cannot block or weaken the records that follow. This is
        // the same ordering [`CookieJar::store`] uses.
        self.evict_expired(now);
        for record in records {
            let Some(stored) = self.sanitize(record, now) else {
                continue;
            };
            self.cookies
                .retain(|old| !same_cookie_identity(old, &stored));
            self.cookies.push(stored);
        }
        self.evict_excess();
    }

    /// The stored form of a record, or `None` when the rules reject it.
    fn sanitize(&self, record: CookieRecord, now: SystemTime) -> Option<StoredCookie> {
        if record.expiry.is_some_and(|expiry| expiry <= now) {
            return None;
        }
        if (record.name.is_empty() && record.value.is_empty())
            || record.name.len() + record.value.len() > 4096
            || has_ctl_excluding_htab(&record.name)
            || has_ctl_excluding_htab(&record.value)
            || record.name.contains([';', '='])
            || record.value.contains(';')
        {
            return None;
        }
        if !record.path.starts_with('/') {
            return None;
        }
        // No length cap on the path. The 1024-byte limit is a parse-time rule
        // on the `Path` attribute; a cookie with no attribute stores the
        // request URI's directory, which has no such cap:
        // https://httpwg.org/http-extensions/draft-ietf-httpbis-rfc6265bis.html#name-storage-model
        // The record names its own domain, so require the canonical form the
        // jar writes rather than trusting a raw attribute. Host-only IPv6
        // stores `ip.to_string()` (`::1`, no brackets); `Host::parse` only
        // accepts IPv6 in brackets, so canonicalize an IP literal first.
        let domain = match record.domain.parse::<std::net::IpAddr>() {
            Ok(ip) => ip.to_string(),
            Err(_) => canonicalize_domain_attr(&record.domain)?,
        };
        if record.domain != domain {
            return None;
        }
        let same_site = SameSite::from(record.same_site);
        if same_site == SameSite::None && !record.secure {
            return None;
        }
        // A partitioned cookie requires `Secure`; the record's key is a site
        // string, bounded like the storage-model's other string fields
        // (<https://github.com/privacycg/CHIPS#partitioned-requires-secure>).
        if record.partition_key.is_some() && !record.secure {
            return None;
        }
        if record
            .partition_key
            .as_ref()
            .is_some_and(|key| key.is_empty() || key.len() > 1024)
        {
            return None;
        }
        if !record.host_only && (is_ip(&domain) || is_public_suffix(&domain)) {
            return None;
        }
        if !cookie_prefixes_ok(
            &record.name,
            &record.value,
            record.secure,
            record.host_only,
            Some(&record.path),
        ) {
            return None;
        }
        if overlays_secure_cookie(
            &self.cookies,
            &record.name,
            &domain,
            &record.path,
            record.secure,
            record.partition_key.as_deref(),
        ) {
            return None;
        }
        Some(StoredCookie {
            name: record.name,
            value: record.value,
            expiry: record.expiry.map(|expiry| expiry.min(now + MAX_LIFETIME)),
            domain,
            path: record.path,
            created: record.created,
            last_access: record.last_access,
            host_only: record.host_only,
            secure: record.secure,
            http_only: record.http_only,
            same_site,
            partition_key: record.partition_key,
        })
    }
}

/// One persistent cookie. Session cookies (`expiry` absent) are omitted on export.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CookieRecord {
    /// Cookie name.
    pub name: String,
    /// Cookie value.
    pub value: String,
    /// Absolute expiry. `None` is a session cookie.
    pub expiry: Option<SystemTime>,
    /// Canonical domain.
    pub domain: String,
    /// Path prefix.
    pub path: String,
    /// Creation time.
    pub created: SystemTime,
    /// Last access time.
    pub last_access: SystemTime,
    /// Host-only flag from the storage model.
    pub host_only: bool,
    /// Secure-only flag.
    pub secure: bool,
    /// `HttpOnly` flag.
    pub http_only: bool,
    /// `SameSite` attribute.
    pub same_site: CookieSameSite,
    /// The top-level site this cookie is partitioned under, or `None` for an
    /// ordinary unpartitioned cookie
    /// (<https://github.com/privacycg/CHIPS>).
    pub partition_key: Option<String>,
}

impl CookieRecord {
    /// The storage slot this cookie occupies: its domain, path, partition
    /// key, and host-only flag. A `CookieStore` change event compares two
    /// snapshots slot by slot, because a cookie with the same name can live in
    /// several slots at once
    /// (<https://cookiestore.spec.whatwg.org/#observable-changes>).
    #[must_use]
    pub fn slot(&self) -> String {
        format!(
            "{}\u{1f}{}\u{1f}{}\u{1f}{}",
            u8::from(self.host_only),
            self.domain,
            self.path,
            self.partition_key.as_deref().unwrap_or("-"),
        )
    }

    /// Everything about the cookie a change event observes: its value and its
    /// stored attributes, but not its slot (which [`Self::slot`] owns) or its
    /// last access time (which every read updates). Two values that compare
    /// unequal mean the cookie changed
    /// (<https://cookiestore.spec.whatwg.org/#observable-changes>).
    #[must_use]
    pub fn state(&self) -> String {
        let same_site = match self.same_site {
            CookieSameSite::Strict => "strict",
            CookieSameSite::Lax => "lax",
            CookieSameSite::None => "none",
            CookieSameSite::Default => "default",
        };
        let millis = |time: SystemTime| {
            time.duration_since(SystemTime::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_millis())
        };
        format!(
            "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
            self.value,
            u8::from(self.secure),
            u8::from(self.http_only),
            same_site,
            self.expiry.map_or(0, millis),
            millis(self.created),
        )
    }
}

/// One cookie a `CookieStore` operation asks the jar to store, with every
/// attribute already validated by the API's algorithm
/// (<https://cookiestore.spec.whatwg.org/#set-a-cookie>).
///
/// Typed rather than a `Set-Cookie` line: the API's `path` is stored verbatim,
/// and a line would split a `path` holding U+003B (;) into extra attributes.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ScriptCookieWrite {
    /// Cookie name.
    pub name: String,
    /// Cookie value.
    pub value: String,
    /// `Expires` as Unix milliseconds; `None` when absent.
    pub expires_millis: Option<i64>,
    /// `Max-Age` in seconds; `None` when absent.
    pub max_age: Option<i64>,
    /// The `Domain` attribute, already lowercased by the API; `None` means
    /// host-only.
    pub domain: Option<String>,
    /// The `Path` attribute, always present because the API defaults it.
    pub path: String,
    /// The `SameSite` attribute, always present because the API defaults it.
    pub same_site: CookieSameSite,
    /// Whether the `Partitioned` attribute is present.
    pub partitioned: bool,
}

/// `SameSite` value stored with a cookie.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CookieSameSite {
    /// `SameSite=Strict`.
    Strict,
    /// `SameSite=Lax`.
    Lax,
    /// `SameSite=None`.
    None,
    /// No `SameSite` attribute; default Lax-like behavior in the jar.
    Default,
}

impl From<SameSite> for CookieSameSite {
    fn from(value: SameSite) -> Self {
        match value {
            SameSite::Strict => Self::Strict,
            SameSite::Lax => Self::Lax,
            SameSite::None => Self::None,
            SameSite::Default => Self::Default,
        }
    }
}

impl From<CookieSameSite> for SameSite {
    fn from(value: CookieSameSite) -> Self {
        match value {
            CookieSameSite::Strict => Self::Strict,
            CookieSameSite::Lax => Self::Lax,
            CookieSameSite::None => Self::None,
            CookieSameSite::Default => Self::Default,
        }
    }
}

fn over_quota_domain(cookies: &[StoredCookie]) -> Option<String> {
    if cookies.len() <= MAX_COOKIES_PER_DOMAIN {
        return None;
    }
    let mut counts = HashMap::new();
    for cookie in cookies {
        let count = counts.entry(cookie.domain.as_str()).or_insert(0_usize);
        *count += 1;
        if *count > MAX_COOKIES_PER_DOMAIN {
            return Some(cookie.domain.clone());
        }
    }
    None
}

fn evict_one(cookies: &mut Vec<StoredCookie>, domain: Option<&str>) -> bool {
    let victim = cookies
        .iter()
        .enumerate()
        .filter(|(_, cookie)| domain.is_none_or(|d| cookie.domain == d))
        .min_by(|(_, a), (_, b)| {
            let by_age = a
                .last_access
                .cmp(&b.last_access)
                .then_with(|| a.created.cmp(&b.created));
            if domain.is_none() {
                return by_age;
            }
            match (a.secure, b.secure) {
                (false, true) => std::cmp::Ordering::Less,
                (true, false) => std::cmp::Ordering::Greater,
                _ => by_age,
            }
        })
        .map(|(i, _)| i);
    if let Some(i) = victim {
        cookies.remove(i);
        true
    } else {
        false
    }
}

struct ParsedSetCookie {
    name: String,
    value: String,
    max_age: Option<i64>,
    expires: Option<SystemTime>,
    domain: Option<String>,
    path: Option<String>,
    secure: bool,
    http_only: bool,
    same_site: Option<SameSite>,
    /// Whether the `Partitioned` attribute was present
    /// (<https://github.com/privacycg/CHIPS#the-partitioned-attribute>).
    partitioned: bool,
}

fn parse_set_cookie(input: &str) -> Option<ParsedSetCookie> {
    // https://httpwg.org/http-extensions/draft-ietf-httpbis-rfc6265bis.html#name-cookie-parsing
    if has_ctl_excluding_htab(input) {
        return None;
    }
    let (pair, attrs) = match input.split_once(';') {
        Some((pair, rest)) => (pair, rest),
        None => (input, ""),
    };
    let pair = pair.trim();
    let (name, value) = match pair.split_once('=') {
        Some((n, v)) => (n.trim().to_owned(), v.trim().to_owned()),
        None => (String::new(), pair.to_owned()),
    };
    if name.is_empty() && value.is_empty() {
        return None;
    }
    if name.len() + value.len() > 4096 {
        return None;
    }

    let mut parsed = ParsedSetCookie {
        name,
        value,
        max_age: None,
        expires: None,
        domain: None,
        path: None,
        secure: false,
        http_only: false,
        same_site: None,
        partitioned: false,
    };
    for av in attrs.split(';') {
        let av = av.trim();
        if av.is_empty() {
            continue;
        }
        let (aname, avalue) = match av.split_once('=') {
            Some((n, v)) => (n.trim(), v.trim()),
            None => (av, ""),
        };
        if aname.eq_ignore_ascii_case("Max-Age") {
            if let Some(n) = parse_max_age(avalue) {
                parsed.max_age = Some(n);
            }
        } else if aname.eq_ignore_ascii_case("Expires") {
            if let Some(t) = parse_cookie_date(avalue) {
                parsed.expires = Some(t);
            }
        } else if aname.eq_ignore_ascii_case("Domain") {
            // An empty value is the last Domain attribute too, and it leaves
            // the cookie host-only:
            // https://httpwg.org/http-extensions/draft-ietf-httpbis-rfc6265bis.html#name-storage-model
            let d = avalue.strip_prefix('.').unwrap_or(avalue);
            if d.len() <= 1024 {
                parsed.domain = (!d.is_empty()).then(|| d.to_ascii_lowercase());
            }
        } else if aname.eq_ignore_ascii_case("Path") {
            if avalue.len() <= 1024 && avalue.starts_with('/') {
                parsed.path = Some(avalue.to_owned());
            }
        } else if aname.eq_ignore_ascii_case("Secure") {
            parsed.secure = true;
        } else if aname.eq_ignore_ascii_case("HttpOnly") {
            parsed.http_only = true;
        } else if aname.eq_ignore_ascii_case("Partitioned") {
            parsed.partitioned = true;
        } else if aname.eq_ignore_ascii_case("SameSite") {
            parsed.same_site = Some(if avalue.eq_ignore_ascii_case("Strict") {
                SameSite::Strict
            } else if avalue.eq_ignore_ascii_case("None") {
                SameSite::None
            } else if avalue.eq_ignore_ascii_case("Lax") {
                SameSite::Lax
            } else {
                SameSite::Default
            });
        }
    }
    Some(parsed)
}

fn parse_max_age(value: &str) -> Option<i64> {
    let digits = value.strip_prefix('-').unwrap_or(value);
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some(value.parse().unwrap_or(if value.starts_with('-') {
        i64::MIN
    } else {
        i64::MAX
    }))
}

fn receive_cookie(
    parsed: ParsedSetCookie,
    op: &CookieOp<'_>,
    existing: &[StoredCookie],
    trustworthy: bool,
) -> Option<StoredCookie> {
    let request_url = op.url;
    let now = op.now;
    let kind = op.kind;
    let host = canonicalize_host(request_url)?;
    let expiry = cookie_expiry(&parsed, now);
    let (host_only, domain, path, path_attr) = cookie_scope(&parsed, request_url, &host)?;
    let secure = parsed.secure;
    if secure && !trustworthy {
        return None;
    }
    let http_only = parsed.http_only;
    if http_only && matches!(kind, RetrievalKind::NonHttp) {
        return None;
    }
    // CHIPS: a `Partitioned` cookie is double-keyed on the site of the
    // top-level document the request belongs to, and the attribute requires
    // `Secure`. A context with no top-level site cannot derive the key, so the
    // cookie is refused rather than stored unpartitioned, which would widen
    // its scope
    // (<https://github.com/privacycg/CHIPS#using-set-cookie-with-partitioned>).
    let partition_key = if parsed.partitioned {
        if !secure {
            return None;
        }
        Some(op.partition_key?.to_owned())
    } else {
        None
    };
    // https://httpwg.org/http-extensions/draft-ietf-httpbis-rfc6265bis.html#name-storage-model
    if !trustworthy
        && overlays_secure_cookie(
            existing,
            &parsed.name,
            &domain,
            &path,
            secure,
            partition_key.as_deref(),
        )
    {
        return None;
    }
    let same_site = parsed.same_site.unwrap_or(SameSite::Default);
    if same_site == SameSite::None && !secure {
        return None;
    }
    if same_site != SameSite::None
        && !op.is_same_site_request()
        && op.initiator_kind != InitiatorKind::Navigation
    {
        return None;
    }
    if !cookie_prefixes_ok(
        &parsed.name,
        &parsed.value,
        secure,
        host_only,
        path_attr.as_deref(),
    ) {
        return None;
    }
    if let Some(old) = existing.iter().find(|old| {
        old.name == parsed.name
            && old.domain == domain
            && old.host_only == host_only
            && old.path == path
            && old.partition_key == partition_key
    }) {
        if matches!(kind, RetrievalKind::NonHttp) && old.http_only {
            return None;
        }
        return Some(StoredCookie {
            name: parsed.name,
            value: parsed.value,
            expiry,
            domain,
            path,
            created: old.created,
            last_access: now,
            host_only,
            secure,
            http_only,
            same_site,
            partition_key,
        });
    }
    Some(StoredCookie {
        name: parsed.name,
        value: parsed.value,
        expiry,
        domain,
        path,
        created: now,
        last_access: now,
        host_only,
        secure,
        http_only,
        same_site,
        partition_key,
    })
}

fn cookie_expiry(parsed: &ParsedSetCookie, now: SystemTime) -> Option<SystemTime> {
    if let Some(max_age) = parsed.max_age {
        if max_age <= 0 {
            Some(now)
        } else {
            Some(now + Duration::from_secs(max_age.unsigned_abs()).min(MAX_LIFETIME))
        }
    } else {
        parsed
            .expires
            .map(|expires| expires.min(now + MAX_LIFETIME))
    }
}

fn cookie_scope(
    parsed: &ParsedSetCookie,
    request_url: &Url,
    host: &str,
) -> Option<(bool, String, String, Option<String>)> {
    let mut domain_attr = match parsed.domain.as_deref() {
        Some(raw) => canonicalize_domain_attr(raw)?,
        None => String::new(),
    };
    if !domain_attr.is_empty() {
        if is_ip(host) || is_public_suffix(&domain_attr) {
            if domain_attr != host {
                return None;
            }
            domain_attr.clear();
        } else if !domain_match(host, &domain_attr) {
            return None;
        }
    }
    let host_only = domain_attr.is_empty();
    let domain = if host_only {
        host.to_owned()
    } else {
        domain_attr
    };
    let path_attr = parsed.path.clone();
    let path = parsed
        .path
        .clone()
        .unwrap_or_else(|| default_path(request_url.path()));
    Some((host_only, domain, path, path_attr))
}

fn overlays_secure_cookie(
    existing: &[StoredCookie],
    name: &str,
    domain: &str,
    path: &str,
    secure: bool,
    partition_key: Option<&str>,
) -> bool {
    !secure
        && existing.iter().any(|old| {
            old.secure
                && old.partition_key.as_deref() == partition_key
                && old.name == name
                && (domain_match(&old.domain, domain) || domain_match(domain, &old.domain))
                && path_match(path, &old.path)
        })
}

fn cookie_prefixes_ok(
    name: &str,
    value: &str,
    secure: bool,
    host_only: bool,
    path_attr: Option<&str>,
) -> bool {
    // https://httpwg.org/http-extensions/draft-ietf-httpbis-rfc6265bis.html#name-storage-model
    if name.is_empty() {
        let lvalue = value.to_ascii_lowercase();
        return !lvalue.starts_with("__secure-") && !lvalue.starts_with("__host-");
    }
    let prefix = name.to_ascii_lowercase();
    if prefix.starts_with("__secure-") && !secure {
        return false;
    }
    if prefix.starts_with("__host-") && !(secure && host_only && path_attr == Some("/")) {
        return false;
    }
    true
}

impl StoredCookie {
    /// The storage-model view of this cookie.
    fn record(&self) -> CookieRecord {
        CookieRecord {
            name: self.name.clone(),
            value: self.value.clone(),
            expiry: self.expiry,
            domain: self.domain.clone(),
            path: self.path.clone(),
            created: self.created,
            last_access: self.last_access,
            host_only: self.host_only,
            secure: self.secure,
            http_only: self.http_only,
            same_site: CookieSameSite::from(self.same_site),
            partition_key: self.partition_key.clone(),
        }
    }

    fn matches(
        &self,
        host: &str,
        request_path: &str,
        op: &CookieOp<'_>,
        trustworthy: bool,
    ) -> bool {
        let host_ok = if self.host_only {
            self.domain == host
        } else {
            domain_match(host, &self.domain)
        };
        if !host_ok {
            return false;
        }
        if !path_match(request_path, &self.path) {
            return false;
        }
        // A partitioned cookie is only visible inside the partition it was
        // stored under; an unpartitioned cookie has no key and matches every
        // request
        // (<https://github.com/privacycg/CHIPS#attaching-a-partitioned-cookie-to-a-request>).
        if let Some(key) = &self.partition_key
            && Some(key.as_str()) != op.partition_key
        {
            return false;
        }
        if self.secure && !trustworthy {
            return false;
        }
        if self.http_only && !matches!(op.kind, RetrievalKind::Http) {
            return false;
        }
        samesite_allows(
            self.same_site,
            op.is_same_site_request(),
            op.initiator_kind,
            op.method_is_safe,
        ) && (op.same_site_policy == SameSiteCookiePolicy::All || self.same_site == SameSite::None)
    }
}

fn same_cookie_identity(old: &StoredCookie, stored: &StoredCookie) -> bool {
    old.name == stored.name
        && old.domain == stored.domain
        && old.host_only == stored.host_only
        && old.path == stored.path
        && old.partition_key == stored.partition_key
}

fn samesite_allows(
    same_site: SameSite,
    same_site_request: bool,
    initiator_kind: InitiatorKind,
    method_is_safe: bool,
) -> bool {
    match same_site {
        SameSite::None => true,
        SameSite::Strict => same_site_request,
        SameSite::Lax | SameSite::Default => {
            same_site_request || (initiator_kind == InitiatorKind::Navigation && method_is_safe)
        }
    }
}

impl CookieOp<'_> {
    fn is_same_site_request(&self) -> bool {
        if self.cross_site_redirect {
            return false;
        }
        match self.initiator {
            None => true,
            Some(from) => origins_same_site(from, &self.url.origin()),
        }
    }
}

/// Whether two URLs share a site: same scheme and registrable domain.
///
/// Follows the `SameSite` definition in
/// <https://httpwg.org/http-extensions/draft-ietf-httpbis-rfc6265bis.html#name-same-site-and-cross-site>.
/// The `ws` and `wss` schemes name the `http` and `https` sites. Opaque URLs,
/// and schemes outside that family, are never same-site with anything.
#[must_use]
pub fn schemeful_same_site(a: &Url, b: &Url) -> bool {
    match (site_tuple(a), site_tuple(b)) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

/// Whether two origins share a site: same scheme and registrable domain.
///
/// The origin form of [`schemeful_same_site`], for the places that compare
/// origins rather than URLs: `Sec-Fetch-Site`
/// (<https://w3c.github.io/webappsec-fetch-metadata/#abstract-opdef-set-site>)
/// and the blob-URL opener rule
/// (<https://html.spec.whatwg.org/multipage/nav-history-apis.html#get-noopener-for-window-open>).
/// An opaque origin is same-site with nothing, including another opaque
/// origin: opaque origins are never equal unless they come from one document.
#[must_use]
pub fn origins_same_site(a: &Origin, b: &Origin) -> bool {
    match (origin_site_tuple(a), origin_site_tuple(b)) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

/// Site identity for renderer isolation: scheme plus registrable domain.
///
/// Returns `None` for opaque URLs and for schemes outside the HTTP family;
/// callers keep those in an opaque instance. The `ws` and `wss` schemes are
/// the `http` and `https` sites.
#[must_use]
pub fn site(url: &Url) -> Option<String> {
    let (scheme, domain) = site_tuple(url)?;
    Some(format!("{scheme}://{domain}"))
}

/// The site one tuple origin names, or `None` for an opaque or non-network
/// origin.
///
/// This is the same rule as [`site`] without constructing a URL: the origin's
/// tuple already carries the scheme and host the registrable-domain lookup
/// needs.
#[must_use]
pub fn site_for_origin(origin: &Origin) -> Option<String> {
    let (scheme, domain) = origin_site_tuple(origin)?;
    Some(format!("{scheme}://{domain}"))
}

fn site_tuple(url: &Url) -> Option<(String, String)> {
    let scheme = site_scheme(url.scheme())?;
    Some((
        scheme.to_owned(),
        registrable_domain(&canonicalize_host(url)?),
    ))
}

fn origin_site_tuple(origin: &Origin) -> Option<(String, String)> {
    let Origin::Tuple(scheme, host, _) = origin else {
        return None;
    };
    Some((
        site_scheme(scheme)?.to_owned(),
        registrable_domain(&canonical_host(host)),
    ))
}

/// The HTTP-family scheme whose site `scheme` names, per the `SameSite`
/// definition: `ws` and `wss` name the `http` and `https` sites.
fn site_scheme(scheme: &str) -> Option<&'static str> {
    match scheme {
        "http" | "ws" => Some("http"),
        "https" | "wss" => Some("https"),
        _ => None,
    }
}

fn registrable_domain(host: &str) -> String {
    if is_ip(host) {
        return host.to_owned();
    }
    let host = host.trim_matches('.');
    // A host with no registrable domain is its own site: a public suffix, or a
    // single label. The cookie rules and the renderer lock both key on that
    // answer, so the fallback is the host itself rather than nothing.
    suffix_list()
        .domain(host.as_bytes())
        .and_then(|domain| std::str::from_utf8(domain.as_bytes()).ok())
        .map_or_else(|| host.to_owned(), str::to_owned)
}

/// A Unix millisecond timestamp as a `SystemTime`. Every `i64` millisecond
/// value fits every platform's `SystemTime`, whose range is at least the
/// `i64`-second range that 9.2e15 seconds needs.
fn unix_millis(millis: i64) -> SystemTime {
    let magnitude = Duration::from_millis(millis.unsigned_abs());
    if millis < 0 {
        UNIX_EPOCH
            .checked_sub(magnitude)
            .expect("i64 milliseconds fit in SystemTime")
    } else {
        UNIX_EPOCH
            .checked_add(magnitude)
            .expect("i64 milliseconds fit in SystemTime")
    }
}

/// Whether `domain` is exactly a public suffix, which no `Domain` attribute
/// may name and no site may span
/// (<https://httpwg.org/http-extensions/draft-ietf-httpbis-rfc6265bis.html#section-5.1.3>).
#[must_use]
pub fn is_public_suffix(domain: &str) -> bool {
    let d = domain.trim_matches('.');
    !d.is_empty()
        && suffix_list()
            .suffix(d.as_bytes())
            .is_some_and(|suffix| suffix.as_bytes() == d.as_bytes())
}

/// The vendored Public Suffix List, parsed once per process.
///
/// Refresh it with `tools/update-public-suffix-list`; a stale list would let
/// two tenants of one private suffix share a site, a cookie jar, and a
/// renderer (<https://publicsuffix.org/list/>).
fn suffix_list() -> &'static publicsuffix::List {
    static LIST: OnceLock<publicsuffix::List> = OnceLock::new();
    LIST.get_or_init(|| {
        publicsuffix::List::from_bytes(include_bytes!("public_suffix_list.dat"))
            .expect("the vendored public suffix list is valid")
    })
}

// https://httpwg.org/http-extensions/draft-ietf-httpbis-rfc6265bis.html#section-5.1.3
pub(crate) fn domain_match(host: &str, domain: &str) -> bool {
    if host.eq_ignore_ascii_case(domain) {
        return true;
    }
    if is_ip(host) {
        return false;
    }
    let (host, domain) = (host.as_bytes(), domain.as_bytes());
    host.len() > domain.len()
        && host[host.len() - domain.len() - 1] == b'.'
        && host[host.len() - domain.len()..].eq_ignore_ascii_case(domain)
}

// https://httpwg.org/http-extensions/draft-ietf-httpbis-rfc6265bis.html#section-5.1.4
pub(crate) fn path_match(request_path: &str, cookie_path: &str) -> bool {
    if request_path == cookie_path {
        return true;
    }
    if request_path.starts_with(cookie_path) {
        if cookie_path.ends_with('/') {
            return true;
        }
        return request_path.as_bytes().get(cookie_path.len()) == Some(&b'/');
    }
    false
}

// https://httpwg.org/http-extensions/draft-ietf-httpbis-rfc6265bis.html#name-paths-and-path-match
fn default_path(uri_path: &str) -> String {
    match uri_path.rfind('/') {
        Some(end) if end > 0 && uri_path.starts_with('/') => uri_path[..end].to_owned(),
        _ => "/".to_owned(),
    }
}

fn canonicalize_host(url: &Url) -> Option<String> {
    url.host().map(|host| canonical_host(&host))
}

fn canonical_host<T: AsRef<str>>(host: &Host<T>) -> String {
    match host {
        Host::Domain(domain) => domain.as_ref().to_ascii_lowercase(),
        Host::Ipv4(ip) => ip.to_string(),
        Host::Ipv6(ip) => ip.to_string(),
    }
}

fn canonicalize_domain_attr(raw: &str) -> Option<String> {
    let stripped = raw.strip_prefix('.').unwrap_or(raw);
    if stripped.len() > 1024 {
        return None;
    }
    match Host::parse(stripped).ok()? {
        Host::Domain(d) => Some(d),
        Host::Ipv4(ip) => Some(ip.to_string()),
        Host::Ipv6(ip) => Some(ip.to_string()),
    }
}

fn is_ip(host: &str) -> bool {
    host.parse::<std::net::IpAddr>().is_ok()
}

fn has_ctl_excluding_htab(s: &str) -> bool {
    s.bytes().any(|b| (b < 0x20 && b != b'\t') || b == 0x7F)
}

// https://httpwg.org/http-extensions/draft-ietf-httpbis-rfc6265bis.html#name-dates
fn parse_cookie_date(s: &str) -> Option<SystemTime> {
    let mut hour = None;
    let mut min = None;
    let mut sec = None;
    let mut day = None;
    let mut month = None;
    let mut year = None;

    for token in cookie_date_tokens(s) {
        if hour.is_none()
            && let Some((h, m, s)) = parse_time_token(token)
        {
            hour = Some(h);
            min = Some(m);
            sec = Some(s);
            continue;
        }
        if day.is_none()
            && let Some(d) = parse_day_token(token)
        {
            day = Some(d);
            continue;
        }
        if month.is_none()
            && let Some(m) = month_num(token)
        {
            month = Some(m);
            continue;
        }
        if year.is_none()
            && let Some(y) = parse_year_token(token)
        {
            year = Some(y);
        }
    }

    let year = year?;
    let month = month?;
    let day = day?;
    let hour = hour?;
    let min = min?;
    let sec = sec?;
    if year < 1601 || !(1..=31).contains(&day) || hour > 23 || min > 59 || sec > 59 {
        return None;
    }
    if day > days_in_month(year, month)? {
        return None;
    }
    civil_to_system(year, month, day, hour, min, sec)
}

fn is_cookie_date_delimiter(b: u8) -> bool {
    matches!(b, 0x09 | 0x20..=0x2F | 0x3B..=0x40 | 0x5B..=0x60 | 0x7B..=0x7E)
}

fn cookie_date_tokens(s: &str) -> impl Iterator<Item = &str> {
    s.split(|c| u8::try_from(c).is_ok_and(is_cookie_date_delimiter))
        .filter(|token| !token.is_empty())
}

fn parse_day_token(token: &str) -> Option<u32> {
    let (n, rest) = take_ascii_digits(token.as_bytes(), 1, 2)?;
    if rest.first().is_some_and(u8::is_ascii_digit) {
        return None;
    }
    u32::try_from(n).ok()
}

fn parse_time_token(token: &str) -> Option<(u32, u32, u32)> {
    let bytes = token.as_bytes();
    let (hour, rest) = take_ascii_digits(bytes, 1, 2)?;
    let rest = rest.strip_prefix(b":")?;
    let (min, rest) = take_ascii_digits(rest, 1, 2)?;
    let rest = rest.strip_prefix(b":")?;
    let (sec, rest) = take_ascii_digits(rest, 1, 2)?;
    if rest.first().is_some_and(u8::is_ascii_digit) {
        return None;
    }
    Some((
        u32::try_from(hour).ok()?,
        u32::try_from(min).ok()?,
        u32::try_from(sec).ok()?,
    ))
}

fn parse_year_token(token: &str) -> Option<i32> {
    let (n, rest) = take_ascii_digits(token.as_bytes(), 2, 4)?;
    if rest.first().is_some_and(u8::is_ascii_digit) {
        return None;
    }
    Some(if (70..=99).contains(&n) {
        n + 1900
    } else if (0..=69).contains(&n) {
        n + 2000
    } else {
        n
    })
}

fn take_ascii_digits(bytes: &[u8], min: usize, max: usize) -> Option<(i32, &[u8])> {
    let mut n = 0usize;
    while n < bytes.len() && n < max && bytes[n].is_ascii_digit() {
        n += 1;
    }
    if n < min {
        return None;
    }
    let value = std::str::from_utf8(&bytes[..n]).ok()?.parse().ok()?;
    Some((value, &bytes[n..]))
}

fn month_num(m: &str) -> Option<u32> {
    let b = m.as_bytes();
    if b.len() < 3 {
        return None;
    }
    let key = [
        b[0].to_ascii_lowercase(),
        b[1].to_ascii_lowercase(),
        b[2].to_ascii_lowercase(),
    ];
    Some(match &key {
        b"jan" => 1,
        b"feb" => 2,
        b"mar" => 3,
        b"apr" => 4,
        b"may" => 5,
        b"jun" => 6,
        b"jul" => 7,
        b"aug" => 8,
        b"sep" => 9,
        b"oct" => 10,
        b"nov" => 11,
        b"dec" => 12,
        _ => return None,
    })
}

fn days_in_month(year: i32, month: u32) -> Option<u32> {
    Some(match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) {
                29
            } else {
                28
            }
        }
        _ => return None,
    })
}

fn civil_to_system(
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    min: u32,
    sec: u32,
) -> Option<SystemTime> {
    if !(1..=12).contains(&month) || day == 0 || day > 31 || hour > 23 || min > 59 || sec > 59 {
        return None;
    }
    let y = i64::from(if month <= 2 { year - 1 } else { year });
    let era = y.div_euclid(400);
    let yoe = u64::try_from(y.rem_euclid(400)).ok()?;
    let mp = u64::from(if month > 2 { month - 3 } else { month + 9 });
    let doy = (153 * mp + 2) / 5 + u64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era
        .checked_mul(146_097)?
        .checked_add(i64::try_from(doe).ok()?)?
        - 719_468;
    let secs = days
        .checked_mul(86_400)?
        .checked_add(i64::from(hour) * 3600 + i64::from(min) * 60 + i64::from(sec))?;
    if secs >= 0 {
        Some(UNIX_EPOCH + Duration::from_secs(u64::try_from(secs).ok()?))
    } else {
        UNIX_EPOCH.checked_sub(Duration::from_secs(u64::try_from(-secs).ok()?))
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use url::Url;

    use super::{
        CookieJar, CookieOp, CookieRecord, CookieSameSite, InitiatorKind, RetrievalKind,
        schemeful_same_site, site, site_for_origin,
    };
    /// 2026-10-12T00:00:00Z.
    const NOW: u64 = 1_791_763_200;
    const DAY: Duration = Duration::from_hours(24);

    fn url(raw: &str) -> Url {
        Url::parse(raw).expect("test URL parses")
    }

    /// The site of a tuple origin is the same rule as the site of the URL it
    /// serializes to, without building the throwaway URL.
    #[test]
    fn an_origin_names_its_site() {
        let address = url("https://ads.example.co.uk/page").origin();
        assert_eq!(
            site_for_origin(&address).as_deref(),
            Some("https://example.co.uk")
        );
        assert!(site_for_origin(&url::Origin::new_opaque()).is_none());
        assert!(site_for_origin(&url("data:text/plain,x").origin()).is_none());
    }

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn op(url: &Url, now: SystemTime, kind: RetrievalKind) -> CookieOp<'_> {
        CookieOp {
            url,
            now,
            kind,
            initiator_kind: InitiatorKind::Navigation,
            method_is_safe: true,
            initiator: None,
            cross_site_redirect: false,
            same_site_policy: crate::SameSiteCookiePolicy::All,
            partition_key: None,
        }
    }

    fn http(url: &Url, now: SystemTime) -> CookieOp<'_> {
        op(url, now, RetrievalKind::Http)
    }

    /// An operation that belongs to `top_site`'s partition.
    fn partitioned<'a>(url: &'a Url, now: SystemTime, top_site: &'a str) -> CookieOp<'a> {
        CookieOp {
            partition_key: Some(top_site),
            ..http(url, now)
        }
    }

    /// A read bumps `last_access`, which is not observable, while a value
    /// change keeps the slot and changes the state. This is what the change
    /// event's diff compares.
    #[test]
    fn a_change_state_ignores_access_time_but_sees_the_value() {
        let site = url("https://example.com/");
        let mut jar = CookieJar::default();
        assert!(jar.store("a=1; Path=/; Secure", http(&site, at(NOW))));
        let first = jar
            .records_for(http(&site, at(NOW + 10)))
            .into_iter()
            .next()
            .expect("cookie stored");
        // A request that carries the cookie bumps `last_access`; the change
        // state must not notice.
        assert_eq!(jar.cookie_string(http(&site, at(NOW + 20))), "a=1");
        let later = jar
            .records_for(http(&site, at(NOW + 20)))
            .into_iter()
            .next()
            .expect("cookie still stored");
        assert_ne!(first.last_access, later.last_access);
        assert_eq!(first.state(), later.state());
        assert_eq!(first.slot(), later.slot());

        assert!(jar.store("a=2; Path=/; Secure", http(&site, at(NOW))));
        let changed = jar
            .records_for(http(&site, at(NOW)))
            .into_iter()
            .next()
            .expect("cookie replaced");
        assert_eq!(changed.slot(), first.slot());
        assert_ne!(changed.state(), first.state());
    }

    /// A host-only cookie and a `Domain` cookie with the same name and path
    /// live in different slots, so a change event reports both.
    #[test]
    fn a_change_slot_separates_host_only_and_domain_cookies() {
        let site = url("https://example.com/");
        let mut jar = CookieJar::default();
        assert!(jar.store("a=host; Path=/; Secure", http(&site, at(NOW))));
        assert!(jar.store(
            "a=domain; Path=/; Secure; Domain=example.com",
            http(&site, at(NOW))
        ));
        let records = jar.records_for(http(&site, at(NOW)));
        assert_eq!(records.len(), 2);
        assert_ne!(records[0].slot(), records[1].slot());
    }

    /// `CookieStore.get()` returns the first record, which RFC 6265's
    /// retrieval model orders longest path first
    /// (<https://cookiestore.spec.whatwg.org/#query-cookies>).
    #[test]
    fn records_follow_the_retrieval_order() {
        let root = url("https://example.com/");
        let mut jar = CookieJar::default();
        assert!(jar.store("x=root; Path=/; Secure", http(&root, at(NOW))));
        assert!(jar.store("x=deep; Path=/dir; Secure", http(&root, at(NOW + 1))));
        let records = jar.records_for(http(&url("https://example.com/dir/page"), at(NOW + 2)));
        assert_eq!(
            records
                .iter()
                .map(|record| record.value.as_str())
                .collect::<Vec<_>>(),
            ["deep", "root"]
        );
    }

    #[test]
    fn a_partitioned_cookie_is_only_visible_in_its_own_top_level_site() {
        let site = url("https://example.com/");
        let mut jar = CookieJar::default();
        assert!(jar.store(
            "__Host-p=1; Secure; Path=/; SameSite=None; Partitioned",
            partitioned(&site, at(NOW), "https://a.test"),
        ));
        assert_eq!(
            jar.cookie_string(partitioned(&site, at(NOW), "https://a.test")),
            "__Host-p=1"
        );
        assert_eq!(
            jar.cookie_string(partitioned(&site, at(NOW), "https://b.test")),
            ""
        );
        // A view with no top-level site is not any partition, so the cookie is
        // withheld there too.
        assert_eq!(jar.cookie_string(http(&site, at(NOW))), "");
    }

    #[test]
    fn a_partitioned_cookie_requires_secure_and_a_partition_key() {
        let site = url("https://example.com/");
        let mut jar = CookieJar::default();
        assert!(!jar.store(
            "p=1; Path=/; SameSite=None; Partitioned",
            partitioned(&site, at(NOW), "https://a.test"),
        ));
        assert!(!jar.store(
            "__Host-p=1; Secure; Path=/; SameSite=None; Partitioned",
            http(&site, at(NOW)),
        ));
        assert_eq!(
            jar.cookie_string(partitioned(&site, at(NOW), "https://a.test")),
            ""
        );
    }

    #[test]
    fn two_partitions_hold_the_same_cookie_identity_independently() {
        let site = url("https://example.com/");
        let mut jar = CookieJar::default();
        assert!(jar.store(
            "__Host-p=a; Secure; Path=/; SameSite=None; Partitioned",
            partitioned(&site, at(NOW), "https://a.test"),
        ));
        assert!(jar.store(
            "__Host-p=b; Secure; Path=/; SameSite=None; Partitioned",
            partitioned(&site, at(NOW), "https://b.test"),
        ));
        assert_eq!(
            jar.cookie_string(partitioned(&site, at(NOW), "https://a.test")),
            "__Host-p=a"
        );
        assert_eq!(
            jar.cookie_string(partitioned(&site, at(NOW), "https://b.test")),
            "__Host-p=b"
        );
        // Deleting in one partition leaves the other alone.
        assert!(jar.store(
            "__Host-p=; Secure; Path=/; SameSite=None; Partitioned; Max-Age=0",
            partitioned(&site, at(NOW), "https://a.test"),
        ));
        assert_eq!(
            jar.cookie_string(partitioned(&site, at(NOW), "https://a.test")),
            ""
        );
        assert_eq!(
            jar.cookie_string(partitioned(&site, at(NOW), "https://b.test")),
            "__Host-p=b"
        );
    }

    #[test]
    fn an_unpartitioned_cookie_is_visible_in_every_partition() {
        let site = url("https://example.com/");
        let mut jar = CookieJar::default();
        assert!(jar.store("plain=1; Path=/", http(&site, at(NOW))));
        assert_eq!(
            jar.cookie_string(partitioned(&site, at(NOW), "https://a.test")),
            "plain=1"
        );
        assert_eq!(
            jar.cookie_string(partitioned(&site, at(NOW), "https://b.test")),
            "plain=1"
        );
    }

    #[test]
    fn expired_secure_cookie_does_not_block_a_plain_one() {
        let secure = url("https://example.com/");
        let plain = url("http://example.com/");
        let mut jar = CookieJar::default();
        assert!(jar.store("a=1; Secure; Max-Age=1", http(&secure, at(NOW))));
        assert!(jar.store("a=2", http(&plain, at(NOW + 2))));
        assert_eq!(jar.cookie_string(http(&plain, at(NOW + 2))), "a=2");
    }

    #[test]
    fn expired_http_only_cookie_does_not_block_script() {
        let page = url("https://example.com/");
        let mut jar = CookieJar::default();
        assert!(jar.store("h=1; HttpOnly; Max-Age=1", http(&page, at(NOW))));
        let script = op(&page, at(NOW + 2), RetrievalKind::NonHttp);
        assert!(jar.store("h=2", script));
        assert_eq!(jar.cookie_string(script), "h=2");
    }

    #[test]
    fn replacing_an_expired_cookie_gets_a_fresh_creation_time() {
        let page = url("https://example.com/");
        let mut jar = CookieJar::default();
        assert!(jar.store("x=1; Max-Age=2", http(&page, at(NOW))));
        assert!(jar.store("y=1", http(&page, at(NOW + 1))));
        assert!(jar.store("x=2", http(&page, at(NOW + 3))));
        assert_eq!(jar.cookie_string(http(&page, at(NOW + 3))), "y=1; x=2");
    }

    #[test]
    fn unicode_public_suffix_rules_match_punycode_hosts() {
        let a = url("https://a.公司.cn/");
        let b = url("https://b.公司.cn/");
        assert_eq!(site(&a).as_deref(), Some("https://a.xn--55qx5d.cn"));
        assert_eq!(site(&b).as_deref(), Some("https://b.xn--55qx5d.cn"));
        let mut jar = CookieJar::default();
        assert!(!jar.store("a=1; Domain=公司.cn", http(&a, at(NOW))));
        assert!(!jar.store("a=1; Domain=xn--55qx5d.cn", http(&a, at(NOW))));
    }

    #[test]
    fn wildcard_public_suffix_rule_makes_every_child_a_suffix() {
        assert_eq!(
            site(&url("https://a.b.ck/")).as_deref(),
            Some("https://a.b.ck")
        );
        let mut jar = CookieJar::default();
        let page = url("https://a.b.ck/");
        assert!(!jar.store("a=1; Domain=b.ck", http(&page, at(NOW))));
    }

    #[test]
    fn exception_public_suffix_rule_overrides_the_wildcard() {
        assert_eq!(
            site(&url("https://www.ck/")).as_deref(),
            Some("https://www.ck")
        );
        assert_eq!(
            site(&url("https://a.www.ck/")).as_deref(),
            Some("https://www.ck")
        );
        let mut jar = CookieJar::default();
        let page = url("https://a.www.ck/");
        assert!(jar.store("a=1; Domain=www.ck", http(&page, at(NOW))));
        assert_eq!(
            jar.cookie_string(http(&url("https://b.www.ck/"), at(NOW))),
            "a=1"
        );
    }

    /// A private suffix (`surge.sh` and its neighbours) separates tenants, so
    /// one tenant cannot plant a `Domain=` cookie on another. The list comes
    /// from the `psl` crate, and this is the case the old snapshot missed.
    #[test]
    fn private_suffixes_name_separate_sites() {
        for suffix in ["surge.sh", "cloud.run", "retool.app"] {
            let tenant = url(&format!("https://a.{suffix}/"));
            let expected = format!("https://a.{suffix}");
            assert_eq!(site(&tenant).as_deref(), Some(expected.as_str()));
            let mut jar = CookieJar::default();
            assert!(
                !jar.store(
                    &format!("sid=attacker; Domain={suffix}"),
                    http(&tenant, at(NOW))
                ),
                "{suffix} is a public suffix"
            );
        }
    }

    /// The origin form agrees with the URL form, including the tuple fields a
    /// URL keeps but an origin drops: path, query, and fragment never decide a
    /// site, and a non-default port does not either.
    #[test]
    fn origins_share_a_site_exactly_when_their_urls_do() {
        use super::origins_same_site;

        let cases = [
            ("https://a.test/", "https://a.test/other?q#f", true),
            ("https://a.test/", "https://b.a.test/", true),
            ("https://a.test/", "http://a.test/", false),
            ("https://a.test/", "https://a.test:8443/", true),
            ("https://a.test/", "https://a.other/", false),
            ("http://127.0.0.1:1/", "http://127.0.0.1:2/", true),
            ("http://127.0.0.1/", "http://127.0.0.2/", false),
        ];
        for (left, right, same) in cases {
            assert_eq!(
                origins_same_site(&url(left).origin(), &url(right).origin()),
                same,
                "{left} vs {right}"
            );
            assert_eq!(schemeful_same_site(&url(left), &url(right)), same);
        }
    }

    /// Opaque origins are never same-site, not even with themselves: the
    /// `Origin::Opaque` identity is unique per document.
    #[test]
    fn an_opaque_origin_is_same_site_with_nothing() {
        use super::origins_same_site;

        let opaque = url("about:blank").origin();
        let other = url("data:,").origin();
        assert!(!origins_same_site(&opaque, &opaque));
        assert!(!origins_same_site(&opaque, &other));
        assert!(!origins_same_site(
            &opaque,
            &url("https://a.test/").origin()
        ));
    }

    #[test]
    fn negative_max_age_overflow_deletes_the_cookie() {
        let page = url("https://example.com/");
        let mut jar = CookieJar::default();
        assert!(jar.store("a=1", http(&page, at(NOW))));
        assert!(jar.store("a=1; Max-Age=-99999999999999999999", http(&page, at(NOW))));
        assert_eq!(jar.cookie_string(http(&page, at(NOW))), "");
    }

    #[test]
    fn positive_max_age_overflow_takes_the_capped_lifetime() {
        let page = url("https://example.com/");
        let mut jar = CookieJar::default();
        assert!(jar.store(
            "a=1; Max-Age=99999999999999999999; Expires=Tue, 13 Oct 2026 00:00:00 GMT",
            http(&page, at(NOW))
        ));
        let expiry = jar.snapshot()[0].expiry;
        assert_eq!(expiry, Some(at(NOW) + DAY * 400));
    }

    #[test]
    fn empty_domain_attribute_stores_a_host_only_cookie() {
        let page = url("https://sub.example.com/");
        let child = url("https://x.sub.example.com/");
        for line in [
            "a=1; Domain=",
            "a=1; Domain=.",
            "a=1; Domain=example.com; Domain=",
        ] {
            let mut jar = CookieJar::default();
            assert!(jar.store(line, http(&page, at(NOW))), "{line}");
            let records = jar.records_for(http(&page, at(NOW)));
            assert_eq!(records.len(), 1, "{line}");
            assert!(records[0].host_only, "{line}");
            assert_eq!(records[0].domain, "sub.example.com", "{line}");
            assert_eq!(jar.cookie_string(http(&child, at(NOW))), "", "{line}");
        }
    }

    fn persistent_expiry(line: &str, now: SystemTime) -> Option<SystemTime> {
        let page = url("https://example.com/");
        let mut jar = CookieJar::default();
        assert!(jar.store(line, http(&page, now)), "{line}");
        jar.snapshot().first().and_then(|record| record.expiry)
    }

    #[test]
    fn two_digit_year_69_is_2069() {
        let now = at(3_124_137_600);
        assert_eq!(
            persistent_expiry("a=1; Expires=Tue, 01 Jan 69 00:00:00 GMT", now),
            Some(at(3_124_224_000))
        );
    }

    #[test]
    fn two_digit_year_70_is_1970() {
        let now = UNIX_EPOCH - DAY;
        assert_eq!(
            persistent_expiry("a=1; Expires=Thu, 01 Jan 70 00:00:01 GMT", now),
            Some(at(1))
        );
    }

    #[test]
    fn leap_day_exists_in_2000() {
        let now = at(946_598_400);
        assert_eq!(
            persistent_expiry("a=1; Expires=Tue, 29 Feb 2000 00:00:00 GMT", now),
            Some(at(951_782_400))
        );
    }

    #[test]
    fn leap_day_in_1900_is_not_a_date() {
        let page = url("https://example.com/");
        let mut jar = CookieJar::default();
        assert!(jar.store(
            "a=1; Expires=Thu, 29 Feb 1900 00:00:00 GMT",
            http(&page, at(NOW))
        ));
        assert_eq!(jar.snapshot(), [] as [CookieRecord; 0]);
        assert_eq!(jar.cookie_string(http(&page, at(NOW))), "a=1");
    }

    #[test]
    fn year_1601_is_a_date_and_deletes_the_cookie() {
        let page = url("https://example.com/");
        let mut jar = CookieJar::default();
        assert!(jar.store("a=1", http(&page, at(NOW))));
        assert!(jar.store(
            "a=1; Expires=Mon, 01 Jan 1601 00:00:00 GMT",
            http(&page, at(NOW))
        ));
        assert_eq!(jar.cookie_string(http(&page, at(NOW))), "");
    }

    #[test]
    fn year_1600_is_not_a_date() {
        let page = url("https://example.com/");
        let mut jar = CookieJar::default();
        assert!(jar.store(
            "a=1; Expires=Fri, 31 Dec 1600 23:59:59 GMT",
            http(&page, at(NOW))
        ));
        assert_eq!(jar.snapshot(), [] as [CookieRecord; 0]);
        assert_eq!(jar.cookie_string(http(&page, at(NOW))), "a=1");
    }

    #[test]
    fn dates_before_1970_convert_exactly() {
        let now = UNIX_EPOCH - DAY;
        assert_eq!(
            persistent_expiry("a=1; Expires=Wed, 31 Dec 1969 23:59:59 GMT", now),
            Some(UNIX_EPOCH - Duration::from_secs(1))
        );
    }

    #[test]
    fn secure_request_can_replace_a_secure_cookie_with_a_plain_one() {
        let secure = url("https://example.com/");
        let plain = url("http://example.com/");
        let mut jar = CookieJar::default();
        assert!(jar.store("a=1; Secure", http(&secure, at(NOW))));
        assert!(!jar.store("a=2", http(&plain, at(NOW))));
        assert!(jar.store("a=3", http(&secure, at(NOW))));
        assert_eq!(jar.cookie_string(http(&plain, at(NOW))), "a=3");
    }

    fn record(name: &str, value: &str) -> CookieRecord {
        CookieRecord {
            name: name.to_owned(),
            value: value.to_owned(),
            expiry: Some(at(NOW) + DAY),
            domain: "example.com".to_owned(),
            path: "/".to_owned(),
            created: at(NOW),
            last_access: at(NOW),
            host_only: true,
            secure: false,
            http_only: false,
            same_site: CookieSameSite::Default,
            partition_key: None,
        }
    }

    fn restored(records: Vec<CookieRecord>) -> String {
        let mut jar = CookieJar::default();
        jar.restore(records, at(NOW));
        jar.cookie_string(http(&url("https://example.com/a/b"), at(NOW)))
    }

    #[test]
    fn restore_loads_a_valid_record() {
        assert_eq!(restored(vec![record("a", "1")]), "a=1");
        assert_eq!(restored(vec![record("", "bare")]), "bare");
        assert_eq!(restored(vec![record("a", "x\ty")]), "a=x\ty");
    }

    #[test]
    fn restore_rejects_records_that_would_split_the_cookie_header() {
        for (name, value) in [("a", "x; admin=1"), ("a;b", "1"), ("a=b", "1"), ("", "")] {
            let records = vec![record(name, value), record("ok", "1")];
            assert_eq!(restored(records), "ok=1", "{name:?}={value:?}");
        }
    }

    #[test]
    fn restore_rejects_control_characters() {
        for (name, value) in [("a\n", "1"), ("a", "1\r"), ("a", "\u{7f}"), ("\0", "1")] {
            let records = vec![record(name, value), record("ok", "1")];
            assert_eq!(restored(records), "ok=1", "{name:?}={value:?}");
        }
    }

    #[test]
    fn restore_caps_name_and_value_at_4096_bytes() {
        let at_cap = "v".repeat(4095);
        assert_eq!(restored(vec![record("a", &at_cap)]), format!("a={at_cap}"));
        assert_eq!(restored(vec![record("ab", &at_cap)]), "");
    }

    #[test]
    fn restore_caps_domain_at_1024_bytes() {
        let label = "d".repeat(63);
        let long = format!("{label}.{label}.{label}.{label}.").repeat(4) + "example.com";
        assert!(long.len() > 1024);
        let mut jar = CookieJar::default();
        jar.restore(
            vec![CookieRecord {
                domain: long,
                ..record("a", "1")
            }],
            at(NOW),
        );
        assert_eq!(jar.snapshot(), [] as [CookieRecord; 0]);
    }

    #[test]
    fn restore_keeps_a_cookie_whose_effective_path_exceeds_1024_bytes() {
        // The 1024-byte cap is a parse-time rule on the `Path` attribute. A
        // cookie with no attribute stores the request URI's directory, which
        // has no such cap, so the round trip must preserve it.
        let page = url(&format!("https://example.com/{}/x", "p".repeat(1030)));
        let mut jar = CookieJar::default();
        assert!(jar.store("a=1; Max-Age=3600", http(&page, at(NOW))));
        let records = jar.snapshot();
        assert_eq!(records.len(), 1);
        assert!(records[0].path.len() > 1024);

        let mut restored = CookieJar::default();
        restored.restore(records, at(NOW));
        assert_eq!(restored.cookie_string(http(&page, at(NOW))), "a=1");
    }

    #[test]
    fn restore_keeps_an_ipv6_host_only_cookie() {
        let page = url("http://[::1]:8000/");
        let mut jar = CookieJar::default();
        assert!(jar.store("a=1; Max-Age=3600", http(&page, at(NOW))));
        let records = jar.snapshot();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].domain, "::1");
        assert!(records[0].host_only);

        let mut restored = CookieJar::default();
        restored.restore(records, at(NOW));
        assert_eq!(restored.cookie_string(http(&page, at(NOW))), "a=1");
    }

    #[test]
    fn restore_ignores_an_expired_live_cookie_when_replacing() {
        // `restore` evicts expired cookies before `sanitize` consults the live
        // jar for the secure overlay, so a dead Secure cookie cannot block the
        // replacement a record brings.
        let page = url("https://example.com/");
        let mut jar = CookieJar::default();
        assert!(jar.store("a=1; Secure; Max-Age=1", http(&page, at(NOW))));
        jar.restore(vec![record("a", "2")], at(NOW + 2));
        assert_eq!(jar.cookie_string(http(&page, at(NOW + 2))), "a=2");
    }

    #[test]
    fn restore_rejects_a_non_canonical_or_public_suffix_domain() {
        for (domain, host_only) in [
            ("Example.com", true),
            (".example.com", false),
            ("com", false),
            ("127.0.0.1", false),
        ] {
            let mut jar = CookieJar::default();
            jar.restore(
                vec![CookieRecord {
                    domain: domain.to_owned(),
                    host_only,
                    ..record("a", "1")
                }],
                at(NOW),
            );
            assert!(jar.snapshot().is_empty(), "{domain}");
        }
    }

    #[test]
    fn restore_applies_prefix_and_same_site_rules() {
        let host_not_host_only = CookieRecord {
            secure: true,
            host_only: false,
            ..record("__Host-a", "1")
        };
        let secure_prefix_plain = record("__Secure-a", "1");
        let none_without_secure = CookieRecord {
            same_site: CookieSameSite::None,
            ..record("a", "1")
        };
        for bad in [host_not_host_only, secure_prefix_plain, none_without_secure] {
            let mut jar = CookieJar::default();
            jar.restore(vec![bad.clone()], at(NOW));
            assert!(jar.snapshot().is_empty(), "{bad:?}");
        }
    }

    #[test]
    fn restore_drops_expired_records_and_caps_expiry() {
        let mut jar = CookieJar::default();
        jar.restore(
            vec![
                CookieRecord {
                    expiry: Some(at(NOW)),
                    ..record("old", "1")
                },
                CookieRecord {
                    expiry: Some(at(NOW) + DAY * 1000),
                    ..record("far", "1")
                },
            ],
            at(NOW),
        );
        let snapshot = jar.snapshot();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].name, "far");
        assert_eq!(snapshot[0].expiry, Some(at(NOW) + DAY * 400));
    }

    #[test]
    fn restore_never_weakens_a_live_secure_cookie() {
        let page = url("https://example.com/");
        let mut jar = CookieJar::default();
        assert!(jar.store("a=1; Secure; Max-Age=60", http(&page, at(NOW))));
        jar.restore(vec![record("a", "2")], at(NOW));
        assert_eq!(jar.cookie_string(http(&page, at(NOW))), "a=1");
        jar.restore(
            vec![CookieRecord {
                secure: true,
                ..record("a", "3")
            }],
            at(NOW),
        );
        assert_eq!(jar.cookie_string(http(&page, at(NOW))), "a=3");
    }

    #[test]
    fn lax_cookie_follows_a_cross_site_navigation_only_with_a_safe_method() {
        let page = url("https://example.com/");
        let other = url("https://other.test/").origin();
        let mut jar = CookieJar::default();
        assert!(jar.store("lax=1; SameSite=Lax", http(&page, at(NOW))));
        let cross_site = |method_is_safe| CookieOp {
            initiator: Some(&other),
            method_is_safe,
            ..http(&page, at(NOW))
        };
        assert_eq!(jar.cookie_string(cross_site(true)), "lax=1");
        assert_eq!(jar.cookie_string(cross_site(false)), "");
    }

    /// `SameSite` compares sites, so which same-site origin the renderer names
    /// as the initiator cannot change the decision. That is why the browser
    /// bounding that claim by the site lock is enough for cookies: every origin
    /// it can name is either same-site with the target or not, alike
    /// (<https://httpwg.org/http-extensions/draft-ietf-httpbis-rfc6265bis.html#name-same-site-and-cross-site>).
    #[test]
    fn samesite_reads_the_initiators_site_not_its_origin() {
        let page = url("https://api.example.com/");
        let mut jar = CookieJar::default();
        assert!(jar.store("strict=1; SameSite=Strict", http(&page, at(NOW))));
        for initiator in [
            None,
            Some(url("https://example.com/").origin()),
            Some(url("https://www.example.com/").origin()),
            Some(url("https://www.example.com:8443/").origin()),
            Some(url("https://api.example.com/").origin()),
        ] {
            let request = CookieOp {
                initiator: initiator.as_ref(),
                ..http(&page, at(NOW))
            };
            assert_eq!(
                jar.cookie_string(request),
                "strict=1",
                "{initiator:?} is same-site with the target"
            );
        }
        for initiator in [
            url("https://other.test/").origin(),
            url("http://example.com/").origin(),
        ] {
            let request = CookieOp {
                initiator: Some(&initiator),
                ..http(&page, at(NOW))
            };
            assert_eq!(
                jar.cookie_string(request),
                "",
                "{}",
                initiator.ascii_serialization()
            );
        }
    }

    #[test]
    fn domain_cookie_matches_subdomains_but_not_lookalikes() {
        let page = url("https://www.example.com/");
        let mut jar = CookieJar::default();
        assert!(jar.store("a=1; Domain=EXAMPLE.com", http(&page, at(NOW))));
        for (host, expected) in [
            ("https://example.com/", "a=1"),
            ("https://a.b.example.com/", "a=1"),
            ("https://badexample.com/", ""),
            ("https://example.com.evil.test/", ""),
        ] {
            assert_eq!(
                jar.cookie_string(http(&url(host), at(NOW))),
                expected,
                "{host}"
            );
        }
    }

    #[test]
    fn default_path_is_the_request_directory() {
        let mut jar = CookieJar::default();
        for (request, path) in [
            ("https://example.com/", "/"),
            ("https://example.com/file", "/"),
            ("https://example.com/dir/file", "/dir"),
            ("https://example.com/a/b/", "/a/b"),
        ] {
            jar.clear();
            assert!(jar.store("a=1", http(&url(request), at(NOW))), "{request}");
            let records = jar.records_for(http(&url(request), at(NOW)));
            assert_eq!(records[0].path, path, "{request}");
        }
    }

    #[test]
    fn cookie_date_tokens_split_on_ascii_delimiters_only() {
        assert_eq!(
            persistent_expiry("a=1; Expires=Tue,29-Feb-2000/00:00:00", at(946_598_400)),
            Some(at(951_782_400))
        );
        assert_eq!(
            persistent_expiry("a=1; Expires=Tue 29 Feb 2000 00:00:00 ü", at(946_598_400)),
            Some(at(951_782_400))
        );
        let page = url("https://example.com/");
        let mut jar = CookieJar::default();
        assert!(jar.store(
            "a=1; Expires=Tue 29 üFeb 2000 00:00:00",
            http(&page, at(946_598_400))
        ));
        assert_eq!(jar.snapshot(), [] as [CookieRecord; 0]);
    }

    fn fill(jar: &mut CookieJar, page: &Url, names: impl IntoIterator<Item = String>, from: u64) {
        for (i, name) in (from..).zip(names) {
            assert!(jar.store(&format!("{name}=1"), http(page, at(NOW + i))));
        }
    }

    #[test]
    fn per_domain_eviction_takes_non_secure_cookies_first() {
        let page = url("https://quota.example/");
        let mut jar = CookieJar::default();
        assert!(jar.store("s=1; Secure", http(&page, at(NOW))));
        fill(&mut jar, &page, (0..50).map(|i| format!("n{i}")), 1);
        let header = jar.cookie_string(http(&page, at(NOW + 100)));
        assert_eq!(header.split("; ").count(), 50);
        assert!(header.starts_with("s=1; n1=1"), "{header}");
    }

    #[test]
    fn eviction_breaks_a_last_access_tie_by_creation_time() {
        let page = url("https://quota.example/");
        let mut jar = CookieJar::default();
        fill(&mut jar, &page, (0..50).map(|i| format!("c{i}")), 0);
        assert!(jar.store("c0=2", http(&page, at(NOW + 50))));
        jar.cookie_string(http(&page, at(NOW + 60)));
        assert!(jar.store("c50=1", http(&page, at(NOW + 61))));
        let header = jar.cookie_string(http(&page, at(NOW + 62)));
        assert!(!header.contains("c0="), "{header}");
        assert!(header.starts_with("c1=1; "), "{header}");
    }

    #[test]
    fn per_domain_quota_only_trims_the_domain_over_it() {
        let pages = [
            url("https://a.example/"),
            url("https://b.example/"),
            url("https://c.example/"),
        ];
        let mut jar = CookieJar::default();
        for page in &pages {
            fill(&mut jar, page, (0..50).map(|i| format!("k{i}")), 0);
        }
        assert!(jar.store("extra=1", http(&pages[1], at(NOW + 100))));
        let counts: Vec<usize> = pages
            .iter()
            .map(|page| jar.records_for(http(page, at(NOW + 100))).len())
            .collect();
        assert_eq!(counts, [50, 50, 50]);
        let b = jar.cookie_string(http(&pages[1], at(NOW + 100)));
        assert!(!b.contains("k0="), "{b}");
        assert!(b.contains("extra=1"), "{b}");
    }
}
