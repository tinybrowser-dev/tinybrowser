//! In-memory WPT origin: every request reads a file from the checkout.
//!
//! Nothing is served over a socket. `testharnessreport.js` is replaced with a
//! small reporter so the host can read the harness result without `WebDriver`'s
//! message queue. Response metadata follows wptserve: the content-type table
//! comes from `wptserve.constants` (passed by `cli.py`), and headers are read
//! the way `wptserve.handlers.load_headers` reads them.
//!
//! The host runs the *real* fetch core: [`dial::outbound_request`] builds the
//! request, [`net::Agent`] applies tainting, CORS, redirects, cookies,
//! `Sec-Fetch-*`, and referrer, and only the bottom exchange is this in-memory
//! transport. A verdict here therefore exercises the same policy the native
//! browser does (#233).

use std::collections::HashMap;
use std::future::Future;
use std::path::{Component, Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use bytes::Bytes;
use dial::{DialHead, dial_failure, outbound_request};
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

/// The origin wptserve gives ordinary test documents.
pub const BASE: &str = "http://web-platform.test:8000/";

/// The origin for a test whose filename carries the `https` flag.
pub const HTTPS_BASE: &str = "https://web-platform.test:8443/";

/// The origin for an `h2` test: `server_url` maps the protocol to TLS on
/// wptserve's h2 port (`tools/serve/serve.py`).
pub const H2_BASE: &str = "https://web-platform.test:9000/";

/// The origin a test's filename flags select.
///
/// wptrunner reads the flags out of the last path segment (`foo.https.any.html`
/// carries `https` and `any`) plus the `wpt_flags` query parameter, and serves
/// `https`, `serviceworker`, and `serviceworker-module` tests over TLS
/// (`tools/manifest/item.py::Item.https`, `tools/wptrunner/wptrunner/wpttest.py::
/// server_protocol`). The in-process transport answers every host and scheme
/// from the checkout, so the origin only has to match what the page observes.
#[must_use]
pub fn base_for(relative: &str) -> &'static str {
    let (path, query) = relative.split_once('?').unwrap_or((relative, ""));
    let name = path.rsplit('/').next().unwrap_or(path);
    let mut flags: Vec<String> = Vec::new();
    let mut segments = name.split('.');
    // The first segment is the test's name and the last is the extension.
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

/// The per-dial deadline the harness gives the fetch core. The in-memory
/// transport never needs it, but a request that names one keeps the core on the
/// same path a browser uses.
const DIAL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

/// Replaces wptrunner's report script. The page records the same completion
/// payload the `WebDriver` harness reads from its message queue.
const REPORT: &str = r#"
(function () {
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

/// The in-process harness's `testdriver-vendor.js`.
///
/// WPT's checked-in file is empty because each browser supplies its own
/// implementation. The harness backs the cookie commands with the page's own
/// `document.cookie`: every cookie these tests set is a non-`HttpOnly`,
/// path-`/` cookie on the test's origin, so the document view is the whole set
/// they ask about. The rest of the vendor surface stays unimplemented and
/// fails closed through `testdriver.js`'s defaults.
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
  internal.delete_all_cookies = async function (_context) {
    for (const pair of pairs()) {
      const equals = pair.indexOf('=');
      const name = equals < 0 ? pair : pair.slice(0, equals);
      document.cookie = name + '=; expires=Thu, 01 Jan 1970 00:00:00 GMT; path=/';
    }
  };
})();
";

/// Why the in-memory origin could not answer a request.
enum Unserved {
    /// A handler route: wptserve runs a program for this path.
    Handler,
    /// No file at this path. wptserve answers 404; the sidecar does too.
    Missing,
    /// The request itself is refused before any server sees it.
    Refused(NetError),
}

/// The headers and content type wptserve answers one resource with.
pub(crate) struct Headers {
    /// The `Content-Type` a `.headers` file overrides, when it does.
    pub(crate) content_type: Option<String>,
    /// Every header, in wptserve's concatenation order with later values
    /// replacing earlier ones of the same name.
    pub(crate) list: Vec<(String, String)>,
}

/// The request facts a `.sub.headers` template can read.
pub(crate) struct RequestContext<'a> {
    /// The URL being served.
    pub(crate) url: &'a Url,
    /// Request headers.
    pub(crate) headers: &'a [(String, String)],
}

/// One response the in-memory origin produced.
struct Served {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

/// The files under one checkout, independent of any fetch state.
///
/// Split from [`FsServices`] so the transport can hold it while the service
/// holds the agent whose transport is it: no reference cycle, and a cookie jar
/// is not reachable from the file mapper.
struct Origin {
    root: PathBuf,
    /// `wptserve.constants.content_types`, as a JSON map handed over by
    /// `cli.py`. An extension it does not name is `application/octet-stream`,
    /// exactly like `guess_content_type`.
    content_types: HashMap<String, String>,
}

impl Origin {
    fn new(root: PathBuf, content_types: HashMap<String, String>) -> Self {
        Self {
            root,
            content_types,
        }
    }

    /// The content type `guess_content_type` returns for `file`.
    fn content_type_for(&self, file: &Path) -> String {
        file.extension()
            .and_then(|extension| extension.to_str())
            .and_then(|extension| self.content_types.get(extension))
            .cloned()
            .unwrap_or_else(|| "application/octet-stream".to_owned())
    }

    /// Reads the headers wptserve sends for `file`.
    fn headers_for(file: &Path, request: &RequestContext<'_>) -> Headers {
        let mut list = Vec::new();
        // Directory headers apply to the whole directory, then the file's own
        // headers follow
        // (`wptserve/handlers.py::load_headers`).
        if let Some(directory) = file.parent() {
            Self::load_headers(&directory.join("__dir__"), request, &mut list);
        }
        Self::load_headers(file, request, &mut list);
        let content_type = list
            .iter()
            .rev()
            .find(|(name, _)| name == "content-type")
            .map(|(_, value)| value.clone());
        // The response carries one `Content-Type`, set from the field above;
        // leaving the raw header in the list would send it twice.
        list.retain(|(name, _)| name != "content-type");
        Headers { content_type, list }
    }

    /// Appends one `__dir__` or resource header block.
    ///
    /// A `.sub.headers` sibling wins over `.headers`, and its templates are
    /// expanded against the request. An expression this harness cannot expand
    /// is a visible error: dropping the header would change the page silently.
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

    /// Answers one request from the checkout.
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
        let relative = Path::new(path.trim_start_matches('/'));
        // A test URL is trusted input, but a `..` segment would read outside
        // the checkout. Refuse it instead of canonicalizing every request.
        if relative
            .components()
            .any(|component| component == Component::ParentDir)
        {
            return Err(Unserved::Refused(connect("path escapes the checkout")));
        }
        // wptserve executes a `.py` handler for such a path; the file is a
        // program, not a response body.
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
        // wptserve's `?pipe=` pipeline: `header(Name, Value)` appends a
        // response header and `sub` expands `{{...}}` in the body. The
        // remaining pipes (for example `trickle`) change timing, not the bytes
        // or headers this harness serves.
        let pipes = Pipes::from_url(url);
        // `.any.html` and `.window.html` are synthesized from their script. A
        // wrapper's source can be missing too, and then wptserve answers 404
        // just as it does for any absent file.
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
        // Both a `.sub.` path and an explicit `sub` pipe expand the body, and
        // either way the `header(...)` pipes still contribute response headers:
        // `sub` is an independent pipe, not a substitute for the whole
        // pipeline.
        let substitute = sub_pipe || path.contains(".sub.");
        // A `.sub.` path is a wptserve pipe: the body's `{{...}}` expressions
        // are expanded against the request, with the escaping its extension
        // selects (`wptserve/handlers.py::wrap_pipeline`). A body this module
        // cannot expand is a server error, exactly as wptserve raises and
        // answers 500.
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

/// The `?pipe=` pipeline a wptserve URL selects.
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

    /// Appends the pipeline's headers and reports whether it asked for `sub`.
    fn apply(&self, list: &mut Vec<(String, String)>) -> bool {
        list.extend(self.headers.iter().cloned());
        self.substitute
    }
}

/// Appends one `Content-Type` field to a wptserve header list.
fn with_content_type(
    mut list: Vec<(String, String)>,
    content_type: String,
) -> Vec<(String, String)> {
    list.push(("content-type".to_owned(), content_type));
    list
}

/// A network error this origin raises for a resource no browser could fetch.
fn connect(reason: &str) -> NetError {
    NetError::Transport(TransportError::Connect(reason.into()))
}

/// The in-memory byte mover under the real fetch core.
struct FrameTransport {
    origin: Arc<Origin>,
    /// The wptserve process that runs handler routes, when the run has one.
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
                        // Without a sidecar a handler is a network error, not
                        // its own source, and an absent file is a failed
                        // fetch rather than a 404.
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

/// A host that maps every URL path to a file under one checkout.
pub struct FsServices {
    /// Kept for the tests that inspect the checkout root and content types.
    /// Production reaches the origin through the transport below.
    #[cfg(test)]
    origin: Arc<Origin>,
    /// The real fetch core. Every policy decision is this crate's; only the
    /// one exchange underneath is the in-memory transport.
    agent: net::Agent,
    storage: Mutex<HashMap<(StorageKind, StorageKey, String), String>>,
}

impl FsServices {
    /// Creates a host rooted at `root`.
    ///
    /// `sidecar` is the wptserve process that answers handler routes and 404s;
    /// without one, those requests are network errors.
    #[must_use]
    pub fn new(
        root: PathBuf,
        content_types: HashMap<String, String>,
        sidecar: Option<Sidecar>,
    ) -> Self {
        let origin = Arc::new(Origin::new(root, content_types));
        let transport = Arc::new(FrameTransport {
            origin: Arc::clone(&origin),
            sidecar,
        });
        Self {
            #[cfg(test)]
            origin,
            agent: net::Agent::with_transport(net::AgentOptions::default(), transport),
            storage: Mutex::new(HashMap::new()),
        }
    }

    /// Stores a response's `Set-Cookie` fields in the jar.
    ///
    /// The main document is mounted from bytes the harness read itself, so its
    /// headers never pass through [`net::Agent`]'s response path. A test that
    /// sets a cookie from a `.headers` file still expects the jar to hold it,
    /// exactly as a navigation that fetched the document would
    /// (<https://fetch.spec.whatwg.org/#http-network-or-cache-fetch>).
    pub(crate) fn store_set_cookies(&self, headers: &[(String, String)], url: &Url) {
        for (name, value) in headers {
            if name == "set-cookie" {
                self.agent.set_cookie(value, url);
            }
        }
    }

    /// Drops cookies and storage between files.
    ///
    /// A worker builds a fresh engine per file, so state a file leaves behind
    /// must not reach the next one. Upstream also clears browser state between
    /// tests.
    pub fn clear(&self) {
        self.agent.clear_cookies();
        self.storage
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }

    /// Reads the headers wptserve sends for `file`.
    pub(crate) fn headers_for(file: &Path, request: &RequestContext<'_>) -> Headers {
        Origin::headers_for(file, request)
    }
}

/// Runs one dial through the real fetch core and buffers its body.
fn fetch(agent: &net::Agent, request: &DialRequest) -> Result<DialOutcome, DialFailure> {
    let initiator = request
        .initiator
        .to_url()
        .map_or_else(|| net::about_blank().origin(), |url| url.origin());
    let sheets = HashMap::new();
    let outbound = outbound_request(request, &initiator, Instant::now() + DIAL_DEADLINE, &sheets)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| DialFailure::Connect)?;
    runtime.block_on(async move {
        let response = agent
            .send(outbound)
            .await
            .map_err(|error| dial_failure(&error))?;
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
        Ok(head.into_outcome(body))
    })
}

/// Appends `suffix` to a path that has no extension of its own, such as
/// `dir/__dir__`.
fn path_with_suffix(base: &Path, suffix: &str) -> PathBuf {
    PathBuf::from(format!("{}{suffix}", base.display()))
}

/// Sets one header, replacing an earlier value of the same name.
///
/// wptserve's `ResponseHeaders.set` overwrites, so a file's header wins over
/// the directory's and only one value is sent.
fn set_header(list: &mut Vec<(String, String)>, name: &str, value: &str) {
    let name = name.to_ascii_lowercase();
    if let Some(entry) = list.iter_mut().find(|(existing, _)| *existing == name) {
        value.clone_into(&mut entry.1);
    } else {
        list.push((name, value.to_owned()));
    }
}

impl NetworkHost for FsServices {
    fn dial_sync(&self, request: DialRequest) -> Result<DialOutcome, DialFailure> {
        // A synchronous request is allowed to block its caller: the harness
        // answers it here on the engine thread, while the browser process does
        // the same over a synchronous IPC round-trip
        // (<https://xhr.spec.whatwg.org/#the-send()-method>).
        //
        // The engine thread is already inside its own current-thread runtime,
        // so the fetch runs on a fresh thread and this one waits for it.
        let agent = self.agent.clone();
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("wpt-fetch-sync".to_owned())
            .spawn(move || {
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    fetch(&agent, &request)
                }))
                .unwrap_or(Err(DialFailure::Connect));
                let _ = send.send(outcome);
            })
            .map_err(|_| DialFailure::Connect)?;
        receive.recv().unwrap_or(Err(DialFailure::Connect))
    }

    fn start_dial(&self, request: DialRequest, completion: DialCompletion) -> DialCancellation {
        // The engine thread must not block on the fetch, and it owns a
        // current-thread runtime that cannot be re-entered. A dedicated thread
        // with its own runtime keeps the core async without a nested
        // `block_on`.
        let agent = self.agent.clone();
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
                        fetch(&agent, &request)
                    }))
                    .unwrap_or(Err(DialFailure::Connect))
                };
                completion(outcome);
            })
            .expect("spawn the WPT fetch thread");
        Box::new(move || cancel_flag.store(true, Ordering::Release))
    }

    fn cookies_for(&self, url: &Url, partition_key: Option<&str>) -> String {
        self.agent.cookies_for_partition(url, partition_key)
    }

    fn set_cookie(&self, value: &str, url: &Url, partition_key: Option<&str>) {
        let _stored = self.agent.set_cookie_partition(value, url, partition_key);
    }
}

impl StorageHost for FsServices {
    fn storage_get(&self, kind: StorageKind, origin: &StorageKey, key: &str) -> Option<String> {
        self.storage
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&(kind, origin.clone(), key.to_owned()))
            .cloned()
    }

    fn storage_keys(&self, kind: StorageKind, origin: &StorageKey) -> Vec<String> {
        self.storage
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .keys()
            .filter(|(key_kind, key_origin, _)| *key_kind == kind && key_origin == origin)
            .map(|(_, _, key)| key.clone())
            .collect()
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
        self.storage
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert((kind, origin.clone(), key.to_owned()), value.to_owned());
        Ok(None)
    }

    fn storage_remove(
        &self,
        kind: StorageKind,
        origin: &StorageKey,
        _url: &str,
        key: &str,
        _source: FrameId,
    ) -> Option<StorageChange> {
        let old_value = self
            .storage
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&(kind, origin.clone(), key.to_owned()))?;
        Some(StorageChange {
            key: Some(key.to_owned()),
            old_value: Some(old_value),
            new_value: None,
        })
    }

    fn storage_clear(
        &self,
        kind: StorageKind,
        origin: &StorageKey,
        _url: &str,
        _source: FrameId,
    ) -> Option<StorageChange> {
        let mut storage = self.storage.lock().unwrap_or_else(PoisonError::into_inner);
        let before = storage.len();
        storage.retain(|(key_kind, key_origin, _), _| !(*key_kind == kind && key_origin == origin));
        if storage.len() == before {
            return None;
        }
        Some(StorageChange {
            key: None,
            old_value: None,
            new_value: None,
        })
    }
}

impl BrowsingContextHost for FsServices {
    fn window_open(
        &self,
        _url: Option<&Url>,
        _target: &TargetName,
        _opener: OpenerPolicy,
        _source: &renderer::NavigationSource,
    ) -> Option<WindowId> {
        None
    }

    /// This host runs one engine and has no tab carrier: `window.open` creates
    /// an auxiliary context in the same engine, so WPT files that need a popup
    /// can run here.
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

    /// A target name only matters to a browser that keeps a cross-tab table.
    /// This host has one tab, so the name has nowhere to go.
    fn set_window_name(&self, _change: renderer::NameChange) {}

    fn window_opener(&self) -> Option<WindowId> {
        None
    }

    fn window_post_message(&self, _window: WindowId, _message: PostedMessage) {}

    fn remote_session_get(
        &self,
        _window: WindowId,
        _origin: &StorageKey,
        _key: &str,
    ) -> Option<String> {
        None
    }
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
        )
    }

    fn outcome(services: &FsServices, path: &str) -> Result<DialOutcome, DialFailure> {
        let url =
            url::Url::parse(&format!("http://web-platform.test:8000{path}")).expect("test URL");
        fetch(
            &services.agent,
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

        // Serving the handler's source would hand a test a 200 that no browser
        // ever sees for that URL.
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
        // Directory headers apply to every file below the directory.
        std::fs::write(
            root.join("dir/__dir__.headers"),
            "X-Content-Type-Options: nosniff\n",
        )
        .expect("dir headers");
        // A `.sub.headers` sibling wins over `.headers`, and expands.
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
        // The table is wptserve's: `txt` is text/plain, unknown is binary.
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
        fetch(&services.agent, &request).expect("navigation");
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
