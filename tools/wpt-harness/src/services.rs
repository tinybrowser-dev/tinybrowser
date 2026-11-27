use std::collections::HashMap;
use std::future::Future;
use std::path::{Component, Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use bytes::Bytes;
use dial::cache::NO_PARTITION;
use dial::{
    CacheDecision, DialHead, HttpCache, dial_failure, outbound_request, refused_redirect,
    watch_redirects,
};
use http_body_util::BodyExt;
use net::{NetError, TransportError, WireRequest, WireResponse};
use renderer::{
    BrowsingContextHost, DialCancellation, DialCompletion, DialFailure, DialOutcome, DialRequest,
    FrameId, MAX_RESPONSE_BODY_BYTES, MessagingHost, NetworkHost, OpenerPolicy, PostedMessage,
    ResponseBody, StorageChange, StorageError, StorageHost, StorageKey, StorageKind, TargetName,
    WindowId,
};
use url::Url;

use crate::sidecar::Sidecar;
use crate::template::{self, Context};
use crate::wrapper::Wrapper;

pub const BASE: &str = "http://web-platform.test:8000/";

pub const HTTPS_BASE: &str = "https://web-platform.test:8443/";

pub const H2_BASE: &str = "https://web-platform.test:9000/";

#[must_use]
pub fn base_for(relative: &str) -> &'static str {
    let (path, query) = relative.split_once('?').unwrap_or((relative, ""));
    let name = path.rsplit('/').next().unwrap_or(path);
    let mut flags: Vec<String> = Vec::new();
    let mut segments = name.split('.');
    let _ = segments.next();
    let mut tail: Vec<&str> = segments.collect();
    let _ = tail.pop();
    flags.extend(tail.into_iter().map(str::to_owned));
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        if key == "wpt_flags" {
            flags.extend(value.split('|').map(str::to_owned));
        }
    }
    if flags.iter().any(|flag| flag == "h2") {
        H2_BASE
    } else if flags.iter().any(|flag| {
        matches!(
            flag.as_str(),
            "https" | "serviceworker" | "serviceworker-module"
        )
    }) {
        HTTPS_BASE
    } else {
        BASE
    }
}

const DIAL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

const REPORT: &str = r#"
(function () {
  var reported = new WeakMap();
  var completed = new WeakSet();
  function isReportedTest(test) {
    return test !== null && typeof test === 'object' &&
        'index' in test && 'status' in test;
  }
  addEventListener('message', function (event) {
    var source = event.source;
    var data = event.data;
    if (!source || !data || typeof data !== 'object') {
      return;
    }
    if (data.type === 'result' && isReportedTest(data.test)) {
      var seen = reported.get(source);
      if (!seen) {
        seen = new Set();
        reported.set(source, seen);
      }
      if (seen.has(data.test.name)) {
        event.stopImmediatePropagation();
        return;
      }
      seen.add(data.test.name);
    } else if (data.type === 'complete' && Array.isArray(data.tests) &&
               data.status && typeof data.status === 'object') {
      if (completed.has(source)) {
        event.stopImmediatePropagation();
        return;
      }
      completed.add(source);
    }
  }, true);
  add_completion_callback(function (tests, harness_status) {
    window.__tinybrowser_result = JSON.stringify({
      status: harness_status.status,
      message: harness_status.message,
      stack: harness_status.stack,
      tests: tests.map(function (test) {
        return {
          name: test.name,
          status: test.status,
          message: test.message,
          stack: test.stack
        };
      })
    });
  });
  setup({output: 0, timeout_multiplier: 1, explicit_timeout: false, debug: false,
         message_events: ["completion"]});
})();
"#;

const TESTDRIVER_VENDOR: &str = r"
(function () {
  const internal = window.test_driver_internal = window.test_driver_internal || {};
  const pairs = () => String(document.cookie || '')
    .split('; ')
    .filter(pair => pair.length !== 0);
  internal.get_all_cookies = async function (_context) {
    return pairs().map(pair => {
      const equals = pair.indexOf('=');
      return {
        name: equals < 0 ? pair : pair.slice(0, equals),
        value: equals < 0 ? '' : pair.slice(equals + 1),
      };
    });
  };
  internal.get_named_cookie = async function (name, _context) {
    const response = await fetch('/resources/testdriver-cookie?name=' +
        encodeURIComponent(String(name)));
    if (!response.ok) {
      return null;
    }
    const body = await response.text();
    return body === 'null' ? null : JSON.parse(body);
  };
  internal.delete_all_cookies = async function (_context) {
    for (const pair of pairs()) {
      const equals = pair.indexOf('=');
      const name = equals < 0 ? pair : pair.slice(0, equals);
      document.cookie = name + '=; expires=Thu, 01 Jan 1970 00:00:00 GMT; path=/';
    }
  };
})();
";

enum Unserved {
    Handler,
    Missing,
    Refused(NetError),
}

pub(crate) struct Headers {
    pub(crate) content_type: Option<String>,
    pub(crate) list: Vec<(String, String)>,
}

pub(crate) struct RequestContext<'a> {
    pub(crate) url: &'a Url,
    pub(crate) headers: &'a [(String, String)],
}

struct Served {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

struct Origin {
    root: PathBuf,
    content_types: HashMap<String, String>,
    cookies: Arc<Mutex<Vec<net::CookieRecord>>>,
}

impl Origin {
    fn new(
        root: PathBuf,
        content_types: HashMap<String, String>,
        cookies: Arc<Mutex<Vec<net::CookieRecord>>>,
    ) -> Self {
        Self {
            root,
            content_types,
            cookies,
        }
    }

    fn content_type_for(&self, file: &Path) -> String {
        file.extension()
            .and_then(|extension| extension.to_str())
            .and_then(|extension| self.content_types.get(extension))
            .cloned()
            .unwrap_or_else(|| "application/octet-stream".to_owned())
    }

    fn headers_for(file: &Path, request: &RequestContext<'_>) -> Headers {
        let mut list = Vec::new();
        if let Some(directory) = file.parent() {
            Self::load_headers(&directory.join("__dir__"), request, &mut list);
        }
        Self::load_headers(file, request, &mut list);
        let content_type = list
            .iter()
            .rev()
            .find(|(name, _)| name == "content-type")
            .map(|(_, value)| value.clone());
        list.retain(|(name, _)| name != "content-type");
        Headers { content_type, list }
    }

    fn load_headers(base: &Path, request: &RequestContext<'_>, list: &mut Vec<(String, String)>) {
        let sub = path_with_suffix(base, ".sub.headers");
        let plain = path_with_suffix(base, ".headers");
        let (path, expand) = if sub.is_file() {
            (sub, true)
        } else if plain.is_file() {
            (plain, false)
        } else {
            return;
        };
        let Ok(bytes) = std::fs::read(&path) else {
            return;
        };
        let text = if expand {
            let context = Context {
                url: request.url,
                headers: request.headers,
            };
            match template::substitute(&String::from_utf8_lossy(&bytes), &context) {
                Ok(text) => text,
                Err(error) => {
                    eprintln!("wpt-harness: {}: {error}", path.display());
                    return;
                }
            }
        } else {
            String::from_utf8_lossy(&bytes).into_owned()
        };
        for line in text.lines() {
            if line.is_empty() {
                continue;
            }
            let Some((name, value)) = line.split_once(':') else {
                continue;
            };
            set_header(list, name.trim(), value.trim());
        }
    }

    fn respond(&self, url: &Url, request_headers: &[(String, String)]) -> Result<Served, Unserved> {
        let path = percent_encoding::percent_decode_str(url.path())
            .decode_utf8()
            .map_err(|_| Unserved::Refused(connect("malformed path")))?;
        if path == "/resources/testharnessreport.js" {
            return Ok(Served {
                status: 200,
                headers: vec![("content-type".to_owned(), "text/javascript".to_owned())],
                body: REPORT.as_bytes().to_vec(),
            });
        }
        if path == "/resources/testdriver-vendor.js" {
            return Ok(Served {
                status: 200,
                headers: vec![("content-type".to_owned(), "text/javascript".to_owned())],
                body: TESTDRIVER_VENDOR.as_bytes().to_vec(),
            });
        }
        if path == "/resources/testdriver-cookie" {
            return Ok(driver_cookie(url, &self.cookies));
        }
        let relative = Path::new(path.trim_start_matches('/'));
        if relative
            .components()
            .any(|component| component == Component::ParentDir)
        {
            return Err(Unserved::Refused(connect("path escapes the checkout")));
        }
        if relative
            .extension()
            .is_some_and(|extension| extension == "py")
        {
            return Err(Unserved::Handler);
        }
        let context = RequestContext {
            url,
            headers: request_headers,
        };
        let pipes = Pipes::from_url(url);
        if let Some((wrapper, source)) = Wrapper::for_url(&path) {
            let file = self.root.join(&source);
            let Ok(body) = std::fs::read(&file) else {
                return Err(Unserved::Missing);
            };
            let mut headers = Self::headers_for(&file, &context);
            pipes.apply(&mut headers.list);
            let js_url = &url.path()[..url.path().len() - ".html".len()];
            let html = wrapper.render(&body, &format!("{js_url}.js"));
            let content_type = headers
                .content_type
                .unwrap_or_else(|| "text/html".to_owned());
            return Ok(Served {
                status: 200,
                headers: with_content_type(headers.list, content_type),
                body: html.into_bytes(),
            });
        }
        let file = self.root.join(relative);
        let Ok(body) = std::fs::read(&file) else {
            return Err(Unserved::Missing);
        };
        let mut headers = Self::headers_for(&file, &context);
        let content_type = headers
            .content_type
            .clone()
            .unwrap_or_else(|| self.content_type_for(&file));
        let sub_pipe = pipes.apply(&mut headers.list);
        let substitute = sub_pipe || path.contains(".sub.");
        let body = if substitute {
            let text = String::from_utf8_lossy(&body);
            let template_context = Context {
                url: context.url,
                headers: context.headers,
            };
            match template::substitute_escaped(
                &text,
                &template_context,
                template::Escape::for_path(&path),
            ) {
                Ok(expanded) => expanded.into_bytes(),
                Err(error) => {
                    eprintln!("wpt-harness: {path}: {error}");
                    return Ok(Served {
                        status: 500,
                        headers: with_content_type(headers.list, content_type),
                        body: error.into_bytes(),
                    });
                }
            }
        } else {
            body
        };
        Ok(Served {
            status: 200,
            headers: with_content_type(headers.list, content_type),
            body,
        })
    }
}

// https://w3c.github.io/webdriver/#get-named-cookie
fn driver_cookie(url: &Url, cookies: &Mutex<Vec<net::CookieRecord>>) -> Served {
    let Some((_, name)) = url
        .query_pairs()
        .find(|(key, _)| key == "name")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
    else {
        return Served {
            status: 400,
            headers: vec![("content-type".to_owned(), "text/plain".to_owned())],
            body: b"missing name".to_vec(),
        };
    };
    let host = url.host_str().unwrap_or_default();
    let cookies = cookies.lock().unwrap_or_else(PoisonError::into_inner);
    let record = cookies
        .iter()
        .find(|record| record.name == name && cookie_domain_covers(record, host));
    let body = match record {
        Some(record) => serde_json::to_vec(&DriverCookie::new(record))
            .expect("a driver cookie is plain JSON scalars"),
        None => b"null".to_vec(),
    };
    Served {
        status: 200,
        headers: vec![
            ("content-type".to_owned(), "application/json".to_owned()),
            ("cache-control".to_owned(), "no-store".to_owned()),
        ],
        body,
    }
}

// https://httpwg.org/http-extensions/draft-ietf-httpbis-rfc6265bis.html#section-5.1.3
fn cookie_domain_covers(record: &net::CookieRecord, host: &str) -> bool {
    if record.host_only {
        record.domain == host
    } else {
        record.domain == host
            || host
                .strip_suffix(&record.domain)
                .is_some_and(|prefix| prefix.ends_with('.'))
    }
}

// https://w3c.github.io/webdriver/#dfn-serialized-cookie
// https://w3c.github.io/webdriver/#dfn-table-for-cookie-conversion
#[derive(serde::Serialize)]
struct DriverCookie<'a> {
    name: &'a str,
    value: &'a str,
    path: &'a str,
    domain: &'a str,
    secure: bool,
    #[serde(rename = "httpOnly")]
    http_only: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    expiry: Option<f64>,
    #[serde(rename = "sameSite")]
    same_site: &'static str,
}

impl<'a> DriverCookie<'a> {
    fn new(record: &'a net::CookieRecord) -> Self {
        Self {
            name: &record.name,
            value: &record.value,
            path: &record.path,
            domain: &record.domain,
            secure: record.secure,
            http_only: record.http_only,
            expiry: record
                .expiry
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|elapsed| elapsed.as_secs_f64()),
            same_site: driver_same_site(record.same_site),
        }
    }
}

fn driver_same_site(same_site: net::CookieSameSite) -> &'static str {
    match same_site {
        net::CookieSameSite::Strict => "Strict",
        net::CookieSameSite::Lax | net::CookieSameSite::Default => "Lax",
        net::CookieSameSite::None => "None",
    }
}

fn publish_cookies(agent: &net::Agent, view: &Mutex<Vec<net::CookieRecord>>) {
    let records = agent.all_cookie_records();
    *view.lock().unwrap_or_else(PoisonError::into_inner) = records;
}

struct Pipes {
    headers: Vec<(String, String)>,
    substitute: bool,
}

impl Pipes {
    fn from_url(url: &Url) -> Self {
        let mut pipes = Self {
            headers: Vec::new(),
            substitute: false,
        };
        for (key, value) in url.query_pairs() {
            if key != "pipe" {
                continue;
            }
            for pipe in value.split('|') {
                if pipe == "sub" {
                    pipes.substitute = true;
                } else if let Some(arguments) = pipe
                    .strip_prefix("header(")
                    .and_then(|rest| rest.strip_suffix(')'))
                    && let Some((name, value)) = arguments.split_once(',')
                {
                    pipes
                        .headers
                        .push((name.trim().to_owned(), value.trim().to_owned()));
                }
            }
        }
        pipes
    }

    fn apply(&self, list: &mut Vec<(String, String)>) -> bool {
        list.extend(self.headers.iter().cloned());
        self.substitute
    }
}

fn with_content_type(
    mut list: Vec<(String, String)>,
    content_type: String,
) -> Vec<(String, String)> {
    list.push(("content-type".to_owned(), content_type));
    list
}

fn connect(reason: &str) -> NetError {
    NetError::Transport(TransportError::Connect(reason.into()))
}

struct FrameTransport {
    origin: Arc<Origin>,
    sidecar: Option<Sidecar>,
}

impl net::Transport for FrameTransport {
    fn exchange(
        &self,
        request: WireRequest,
        _budget: net::HopBudget,
    ) -> Pin<Box<dyn Future<Output = Result<WireResponse, NetError>> + Send + '_>> {
        Box::pin(async move {
            let headers: Vec<(String, String)> = request
                .headers
                .iter()
                .map(|(name, value)| (name.to_owned(), net::isomorphic_decode(value)))
                .collect();
            let served = match self.origin.respond(&request.url, &headers) {
                Ok(served) => served,
                Err(Unserved::Refused(error)) => return Err(error),
                Err(Unserved::Handler | Unserved::Missing) => {
                    let Some(sidecar) = self.sidecar else {
                        return Err(connect("wptserve is not running"));
                    };
                    return sidecar.exchange(&request);
                }
            };
            let mut mapped = net::HeaderMap::new();
            for (name, value) in served.headers {
                mapped
                    .insert(&name, value)
                    .map_err(|_| connect("invalid response header"))?;
            }
            let body = http_body_util::Full::new(Bytes::from(served.body))
                .map_err(|infallible| match infallible {})
                .boxed_unsync();
            Ok(WireResponse {
                status: served.status,
                headers: mapped,
                body,
                http2: false,
            })
        })
    }
}

pub struct HarnessClock {
    wall_base: std::time::SystemTime,
    monotonic_base: std::time::Instant,
    offset_nanos: std::sync::atomic::AtomicU64,
}

impl HarnessClock {
    fn new() -> Self {
        Self {
            wall_base: std::time::SystemTime::now(),
            monotonic_base: std::time::Instant::now(),
            offset_nanos: std::sync::atomic::AtomicU64::new(0),
        }
    }

    pub fn advance(&self, delay: std::time::Duration) {
        let nanos = u64::try_from(delay.as_nanos()).unwrap_or(u64::MAX);
        self.offset_nanos.fetch_add(nanos, Ordering::Relaxed);
    }

    fn elapsed(&self) -> std::time::Duration {
        std::time::Duration::from_nanos(self.offset_nanos.load(Ordering::Relaxed))
    }
}

impl net::ClockSource for HarnessClock {
    fn wall(&self) -> std::time::SystemTime {
        self.wall_base + self.elapsed()
    }

    fn monotonic(&self) -> std::time::Instant {
        self.monotonic_base + self.elapsed()
    }

    fn is_manual(&self) -> bool {
        true
    }
}

pub struct FsServices {
    #[cfg(test)]
    origin: Arc<Origin>,
    agent: net::Agent,
    // https://fetch.spec.whatwg.org/#http-cache
    cache: Arc<HttpCache>,
    clock: net::Clock,
    manual_clock: Option<Arc<HarnessClock>>,
    storage: webstorage::StorageService<u64>,
    object_urls: webstorage::ObjectUrlStore<()>,
    cookies: Arc<Mutex<Vec<net::CookieRecord>>>,
}

impl FsServices {
    #[must_use]
    pub fn new(
        root: PathBuf,
        content_types: HashMap<String, String>,
        sidecar: Option<Sidecar>,
        virtual_time: bool,
    ) -> Self {
        let cookies = Arc::new(Mutex::new(Vec::new()));
        let origin = Arc::new(Origin::new(root, content_types, Arc::clone(&cookies)));
        let transport = Arc::new(FrameTransport {
            origin: Arc::clone(&origin),
            sidecar,
        });
        let (clock, manual_clock) = if virtual_time {
            let manual = Arc::new(HarnessClock::new());
            (net::Clock::new(Arc::clone(&manual)), Some(manual))
        } else {
            (net::Clock::default(), None)
        };
        let options = net::AgentOptions {
            clock: clock.clone(),
            ..net::AgentOptions::default()
        };
        let cache = Arc::new(HttpCache::new(clock.clone()));
        let storage = webstorage::StorageService::default();
        storage.create_session(0);
        Self {
            #[cfg(test)]
            origin,
            agent: net::Agent::with_transport(options, transport),
            cache,
            clock,
            manual_clock,
            storage,
            object_urls: webstorage::ObjectUrlStore::default(),
            cookies,
        }
    }

    pub fn advance_clock(&self, delay: std::time::Duration) {
        if let Some(clock) = &self.manual_clock {
            clock.advance(delay);
        }
    }

    // https://fetch.spec.whatwg.org/#http-network-or-cache-fetch
    pub(crate) fn store_set_cookies(&self, headers: &[(String, String)], url: &Url) {
        for (name, value) in headers {
            if name == "set-cookie" {
                self.agent.set_cookie(value, url);
            }
        }
        self.publish_cookies();
    }

    fn publish_cookies(&self) {
        publish_cookies(&self.agent, &self.cookies);
    }

    pub fn clear(&self) {
        self.agent.clear_cookies();
        self.publish_cookies();
        self.cache.clear();
        self.storage.clear_all();
        self.object_urls.unload_owner(());
        self.storage.create_session(0);
    }

    pub(crate) fn headers_for(file: &Path, request: &RequestContext<'_>) -> Headers {
        Origin::headers_for(file, request)
    }
}

fn fetch(
    agent: &net::Agent,
    cache: &HttpCache,
    request: &DialRequest,
) -> Result<DialOutcome, DialFailure> {
    let outcome = fetch_inner(agent, cache, request)?;
    // https://fetch.spec.whatwg.org/#concept-main-fetch
    if !dial::integrity_ok(request, &outcome) {
        return Err(DialFailure::Connect);
    }
    Ok(outcome)
}

fn fetch_inner(
    agent: &net::Agent,
    cache: &HttpCache,
    request: &DialRequest,
) -> Result<DialOutcome, DialFailure> {
    let initiator = request
        .initiator
        .to_url()
        .map_or_else(|| net::about_blank().origin(), |url| url.origin());
    let sheets = HashMap::new();
    let mut outbound =
        outbound_request(request, &initiator, Instant::now() + DIAL_DEADLINE, &sheets)?;
    // https://www.rfc-editor.org/rfc/rfc9111#section-4.1
    let sent = dial::sent_headers(&outbound);
    let (stored, validators) = match cache.read(request, &sent, &initiator, NO_PARTITION)? {
        CacheDecision::Hit(hit) => return Ok(hit),
        CacheDecision::Miss {
            stored, validators, ..
        } => (stored, validators),
    };
    validators.apply(&mut outbound)?;
    let blocked_redirect = watch_redirects(request, &initiator, &mut outbound);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| DialFailure::Connect)?;
    runtime.block_on(async move {
        let response = match agent.send(outbound).await {
            Ok(response) => response,
            Err(error) => {
                // https://w3c.github.io/webappsec-csp/#report-violation
                if let Some(done) = refused_redirect(&blocked_redirect) {
                    return Ok(done);
                }
                return Err(dial_failure(&error));
            }
        };
        if response.status() == 304
            && let Some(stored) = stored
        {
            let Some(merged) = HttpCache::merge_not_modified(&stored, &response, &validators)
            else {
                return Err(DialFailure::Connect);
            };
            cache.remember(request, &sent, &initiator, NO_PARTITION, &merged);
            return Ok(merged);
        }
        let head = DialHead::read(request, &response);
        let body = if head.null_body() {
            ResponseBody::Bytes(Vec::new())
        } else {
            let mut reader = response.into_body().await;
            let mut bytes = Vec::new();
            loop {
                match reader.read_chunk().await {
                    Ok(Some(chunk)) => {
                        bytes.extend_from_slice(&chunk);
                        if bytes.len() > MAX_RESPONSE_BODY_BYTES {
                            return Err(DialFailure::Limit);
                        }
                    }
                    Ok(None) => break,
                    Err(_) => return Err(DialFailure::Connect),
                }
            }
            ResponseBody::Bytes(bytes)
        };
        let outcome = head.into_outcome(body);
        cache.remember(request, &sent, &initiator, NO_PARTITION, &outcome);
        Ok(outcome)
    })
}

fn path_with_suffix(base: &Path, suffix: &str) -> PathBuf {
    PathBuf::from(format!("{}{suffix}", base.display()))
}

fn set_header(list: &mut Vec<(String, String)>, name: &str, value: &str) {
    let name = name.to_ascii_lowercase();
    if let Some(entry) = list.iter_mut().find(|(existing, _)| *existing == name) {
        value.clone_into(&mut entry.1);
    } else {
        list.push((name, value.to_owned()));
    }
}

impl NetworkHost for FsServices {
    fn clock(&self) -> net::Clock {
        self.clock.clone()
    }

    fn dial_sync(&self, request: DialRequest) -> Result<DialOutcome, DialFailure> {
        // https://xhr.spec.whatwg.org/#the-send(
        let agent = self.agent.clone();
        let cache = Arc::clone(&self.cache);
        let cookies = Arc::clone(&self.cookies);
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("wpt-fetch-sync".to_owned())
            .spawn(move || {
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    fetch(&agent, &cache, &request)
                }))
                .unwrap_or(Err(DialFailure::Connect));
                publish_cookies(&agent, &cookies);
                let _ = send.send(outcome);
            })
            .map_err(|_| DialFailure::Connect)?;
        receive.recv().unwrap_or(Err(DialFailure::Connect))
    }

    fn start_dial(&self, request: DialRequest, completion: DialCompletion) -> DialCancellation {
        let agent = self.agent.clone();
        let cache = Arc::clone(&self.cache);
        let cookies = Arc::clone(&self.cookies);
        let cancelled = Arc::new(AtomicBool::new(false));
        let thread_flag = Arc::clone(&cancelled);
        let cancel_flag = Arc::clone(&cancelled);
        std::thread::Builder::new()
            .name("wpt-fetch".to_owned())
            .spawn(move || {
                let outcome = if thread_flag.load(Ordering::Acquire) {
                    Err(DialFailure::Cancelled)
                } else {
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        fetch(&agent, &cache, &request)
                    }))
                    .unwrap_or(Err(DialFailure::Connect))
                };
                publish_cookies(&agent, &cookies);
                completion(outcome);
            })
            .expect("spawn the WPT fetch thread");
        Box::new(move || cancel_flag.store(true, Ordering::Release))
    }
}

impl StorageHost for FsServices {
    fn cookies_for(&self, url: &Url, partition_key: Option<&str>) -> String {
        self.agent.cookies_for_partition(url, partition_key)
    }

    fn set_cookie(&self, value: &str, url: &Url, partition_key: Option<&str>) {
        let _stored = self.agent.set_cookie_partition(value, url, partition_key);
        self.publish_cookies();
    }

    fn cookie_store_records(
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

    fn cookie_store_store(
        &self,
        write: &net::ScriptCookieWrite,
        url: &Url,
        partition_key: Option<&str>,
    ) {
        self.agent
            .set_script_cookie_partition(write, url, partition_key);
        self.publish_cookies();
    }

    fn remote_session_get(
        &self,
        _window: WindowId,
        _origin: &StorageKey,
        _key: &str,
    ) -> Option<String> {
        None
    }
    fn storage_get(&self, kind: StorageKind, origin: &StorageKey, key: &str) -> Option<String> {
        self.storage.get(kind, 0, origin, key)
    }

    fn storage_keys(&self, kind: StorageKind, origin: &StorageKey) -> Vec<String> {
        self.storage.keys(kind, 0, origin)
    }

    fn storage_set(
        &self,
        kind: StorageKind,
        origin: &StorageKey,
        _url: &str,
        key: &str,
        value: &str,
        _source: FrameId,
    ) -> Result<Option<StorageChange>, StorageError> {
        self.storage.set(kind, 0, origin, key, value)
    }

    fn storage_remove(
        &self,
        kind: StorageKind,
        origin: &StorageKey,
        _url: &str,
        key: &str,
        _source: FrameId,
    ) -> Option<StorageChange> {
        self.storage.remove(kind, 0, origin, key)
    }

    fn storage_clear(
        &self,
        kind: StorageKind,
        origin: &StorageKey,
        _url: &str,
        _source: FrameId,
    ) -> Option<StorageChange> {
        self.storage.clear(kind, 0, origin)
    }

    fn create_object_url(
        &self,
        scope: u64,
        origin: &renderer::SerializedOrigin,
        contents: String,
        content_type: String,
    ) -> Option<String> {
        let origin = match origin {
            renderer::SerializedOrigin::Tuple(origin) => Some(origin.clone()),
            renderer::SerializedOrigin::Opaque => None,
        };
        self.object_urls
            .create((), scope, origin, contents, content_type)
    }

    fn resolve_object_url(
        &self,
        scope: u64,
        origin: &renderer::SerializedOrigin,
        url: &str,
    ) -> Option<renderer::BlobRecord> {
        let origin = match origin {
            renderer::SerializedOrigin::Tuple(origin) => Some(origin),
            renderer::SerializedOrigin::Opaque => None,
        };
        self.object_urls.resolve((), scope, origin, url)
    }

    fn revoke_object_url(&self, scope: u64, origin: &renderer::SerializedOrigin, url: &str) {
        let origin = match origin {
            renderer::SerializedOrigin::Tuple(origin) => Some(origin),
            renderer::SerializedOrigin::Opaque => None,
        };
        self.object_urls.revoke((), scope, origin, url);
    }

    fn unload_object_urls(&self, scope: u64) {
        self.object_urls.unload_scope((), scope);
    }
}

impl BrowsingContextHost for FsServices {
    fn window_open(
        &self,
        _url: Option<&Url>,
        _target: &TargetName,
        _opener: OpenerPolicy,
        _source: &renderer::NavigationSource,
    ) -> Option<renderer::OpenedWindow> {
        None
    }

    fn opens_window_as_tab(&self) -> bool {
        false
    }

    fn navigate_named(
        &self,
        _name: &renderer::ContextName,
        _url: Option<&Url>,
        _opener: OpenerPolicy,
        _source: &renderer::NavigationSource,
    ) -> Option<WindowId> {
        None
    }

    fn window_close(&self, _window: WindowId) {}

    fn set_window_name(&self, _change: renderer::NameChange) {}

    fn window_opener(&self) -> Option<WindowId> {
        None
    }

    fn window_post_message(&self, _window: WindowId, _message: PostedMessage) {}
}

impl MessagingHost for FsServices {
    fn broadcast_post(&self, _origin: &StorageKey, _name: &str, _payload: &str, _channel: u64) {}
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use renderer::{DialFailure, DialOutcome};

    use super::{FsServices, RequestContext, fetch};

    fn root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "wpt-harness-services-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("dir")).expect("root created");
        root
    }

    fn services(name: &str) -> FsServices {
        FsServices::new(
            root(name),
            HashMap::from([("html".to_owned(), "text/html".to_owned())]),
            None,
            true,
        )
    }

    fn outcome(services: &FsServices, path: &str) -> Result<DialOutcome, DialFailure> {
        let url =
            url::Url::parse(&format!("http://web-platform.test:8000{path}")).expect("test URL");
        fetch(
            &services.agent,
            &services.cache,
            &renderer::DialRequest::new(
                renderer::RequestPolicy::Image { crossorigin: None },
                &url,
                &url.origin(),
            ),
        )
    }

    fn headers(services: &FsServices, url: &str) -> super::Headers {
        let url = url::Url::parse(url).expect("URL");
        FsServices::headers_for(
            &services.origin.root.join("dir/data.txt"),
            &RequestContext {
                url: &url,
                headers: &[],
            },
        )
    }

    fn driver_record(name: &str, domain: &str, host_only: bool) -> net::CookieRecord {
        net::CookieRecord {
            name: name.to_owned(),
            value: "value".to_owned(),
            expiry: None,
            domain: domain.to_owned(),
            path: "/".to_owned(),
            created: std::time::SystemTime::UNIX_EPOCH,
            last_access: std::time::SystemTime::UNIX_EPOCH,
            host_only,
            secure: true,
            http_only: false,
            same_site: net::CookieSameSite::Strict,
            partition_key: None,
        }
    }

    #[test]
    fn the_driver_cookie_route_scopes_by_host() {
        let services = services("driver-cookie");
        let route = |host: &str, name: &str| {
            url::Url::parse(&format!(
                "http://{host}:8000/resources/testdriver-cookie?name={name}"
            ))
            .expect("route URL")
        };
        let page = route("web-platform.test", "a");
        *services
            .cookies
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            vec![driver_record("a", "web-platform.test", true)];

        let Ok(served) = services.origin.respond(&page, &[]) else {
            panic!("the testdriver cookie route must answer");
        };
        let body: serde_json::Value = serde_json::from_slice(&served.body).expect("cookie JSON");
        assert_eq!(body["name"], "a");
        assert_eq!(body["value"], "value");
        assert_eq!(body["path"], "/");
        assert_eq!(body["domain"], "web-platform.test");
        assert_eq!(body["secure"], true);
        assert_eq!(body["httpOnly"], false);
        assert_eq!(body["sameSite"], "Strict");
        assert!(
            body.get("expiry").is_none(),
            "a session cookie must not carry an expiry"
        );

        let sub = route("sub.web-platform.test", "a");
        let Ok(served) = services.origin.respond(&sub, &[]) else {
            panic!("the testdriver cookie route must answer");
        };
        assert_eq!(served.body, b"null");

        *services
            .cookies
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            vec![driver_record("a", "web-platform.test", false)];
        let Ok(served) = services.origin.respond(&sub, &[]) else {
            panic!("the testdriver cookie route must answer");
        };
        let body: serde_json::Value = serde_json::from_slice(&served.body).expect("cookie JSON");
        assert_eq!(body["domain"], "web-platform.test");
    }

    #[test]
    fn a_static_file_is_served_and_a_handler_is_not() {
        let services = services("handler");
        let root = services.origin.root.clone();
        std::fs::write(root.join("dir/data.txt"), b"from a file").expect("file written");
        std::fs::write(
            root.join("dir/handler.py"),
            b"def main(request, response): ...",
        )
        .expect("handler written");

        let served = outcome(&services, "/dir/data.txt").expect("static file");
        assert!(
            matches!(&served.body, renderer::ResponseBody::Bytes(bytes) if bytes == b"from a file")
        );

        assert!(
            outcome(&services, "/dir/handler.py").is_err(),
            "a wptserve handler is a network error, not its own source"
        );
        assert!(
            outcome(&services, "/dir/missing.txt").is_err(),
            "an absent file is a network error"
        );
        std::fs::remove_dir_all(&root).expect("root removed");
    }

    #[test]
    fn content_types_and_header_files_follow_wptserve() {
        let services = services("headers");
        let root = services.origin.root.clone();
        std::fs::write(root.join("dir/data.txt"), b"x").expect("file");
        std::fs::write(
            root.join("dir/__dir__.headers"),
            "X-Content-Type-Options: nosniff\n",
        )
        .expect("dir headers");
        std::fs::write(root.join("dir/data.txt.headers"), "X-Plain: yes\n").expect("headers");
        std::fs::write(
            root.join("dir/data.txt.sub.headers"),
            "Access-Control-Allow-Origin: {{location[server]}}\n",
        )
        .expect("sub headers");
        let headers = headers(&services, "http://web-platform.test:8000/dir/data.txt");
        assert_eq!(
            headers.list,
            vec![
                ("x-content-type-options".to_owned(), "nosniff".to_owned()),
                (
                    "access-control-allow-origin".to_owned(),
                    "http://web-platform.test:8000".to_owned()
                ),
            ],
            "the .sub.headers sibling replaces the plain one"
        );
        assert_eq!(headers.content_type, None);
        assert_eq!(
            services
                .origin
                .content_type_for(std::path::Path::new("a.txt")),
            "application/octet-stream",
            "only extensions the passed table names are known"
        );
        std::fs::remove_dir_all(&root).expect("root removed");
    }

    #[test]
    fn a_set_cookie_header_reaches_the_jar() {
        let services = services("cookies");
        let root = services.origin.root.clone();
        std::fs::write(root.join("dir/page.html"), b"x").expect("file");
        std::fs::write(root.join("dir/page.html.headers"), "Set-Cookie: NAME=1\n")
            .expect("headers");
        let url = url::Url::parse("http://web-platform.test:8000/dir/page.html").expect("test URL");
        let request =
            renderer::DialRequest::new(renderer::RequestPolicy::FrameLoad, &url, &url.origin());
        fetch(&services.agent, &services.cache, &request).expect("navigation");
        assert!(
            services.agent.cookies_for(&url).contains("NAME=1"),
            "a navigation stores the response's Set-Cookie: {}",
            services.agent.cookies_for(&url)
        );
        std::fs::remove_dir_all(&root).expect("root removed");
    }

    #[test]
    fn a_sub_body_is_expanded_with_wptserve_escaping() {
        let services = services("sub-body");
        let root = services.origin.root.clone();
        std::fs::write(
            root.join("dir/t.sub.html"),
            b"<p>{{host}}</p><p>{{GET[q]}}</p>",
        )
        .expect("template body");
        std::fs::write(root.join("dir/plain.txt"), b"{{host}}").expect("plain body");
        std::fs::write(root.join("dir/raw.sub.txt"), b"{{GET[q]}}").expect("raw template");

        let served = outcome(&services, "/dir/t.sub.html?q=a%26b%3Cc").expect("template");
        let renderer::ResponseBody::Bytes(bytes) = served.body else {
            panic!("a template has a byte body");
        };
        assert_eq!(
            String::from_utf8(bytes).expect("UTF-8"),
            "<p>web-platform.test</p><p>a&amp;b&lt;c</p>",
            "markup escapes each substituted value"
        );

        let served = outcome(&services, "/dir/raw.sub.txt?q=a%26b").expect("raw template");
        let renderer::ResponseBody::Bytes(bytes) = served.body else {
            panic!("a template has a byte body");
        };
        assert_eq!(
            String::from_utf8(bytes).expect("UTF-8"),
            "a&b",
            "a non-markup pipe does not escape"
        );

        let served = outcome(&services, "/dir/plain.txt").expect("plain body");
        let renderer::ResponseBody::Bytes(bytes) = served.body else {
            panic!("a plain file has a byte body");
        };
        assert_eq!(
            String::from_utf8(bytes).expect("UTF-8"),
            "{{host}}",
            "a file without .sub. is not a template"
        );
        std::fs::remove_dir_all(&root).expect("root removed");
    }

    #[test]
    fn a_test_filename_flag_selects_its_origin() {
        use super::{BASE, H2_BASE, HTTPS_BASE, base_for};

        assert_eq!(base_for("dom/nodes/plain.html"), BASE);
        assert_eq!(base_for("dom/nodes/thing.https.html"), HTTPS_BASE);
        assert_eq!(base_for("dom/nodes/thing.https.any.html?1-10"), HTTPS_BASE);
        assert_eq!(base_for("dom/nodes/thing.h2.html"), H2_BASE);
        assert_eq!(base_for("websockets/thing.any.html?wpt_flags=h2"), H2_BASE);
        assert_eq!(
            base_for("dom/nodes/thing.any.html?wpt_flags=https"),
            HTTPS_BASE
        );
    }

    #[test]
    fn an_any_wrapper_is_served_from_its_script() {
        let services = services("wrapper");
        let root = services.origin.root.clone();
        std::fs::write(
            root.join("dir/multi.any.js"),
            "// META: global=window\ntest(() => {}, 'x');\n",
        )
        .expect("source");
        let served = outcome(&services, "/dir/multi.any.html").expect("wrapper");
        let renderer::ResponseBody::Bytes(bytes) = served.body else {
            panic!("a wrapper has a byte body");
        };
        let html = String::from_utf8(bytes).expect("HTML");
        assert!(html.contains("self.GLOBAL"));
        assert!(html.contains("<script src=\"/dir/multi.any.js\"></script>"));
        std::fs::remove_dir_all(&root).expect("root removed");
    }
}
