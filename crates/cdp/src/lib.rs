//! CDP adapter over [`browser::BrowserHandle`].
//!
//! Honest first subsets of Browser, Target, Page, and Runtime. Unsupported
//! methods return method-not-found. Flattened `sessionId` routing on the
//! browser socket.
//! Hyper serves the loopback HTTP and WebSocket endpoints. The synchronous
//! [`Client`] used by the CLI and tests stays on tungstenite.
//!
//! Names: the wire object is a CDP **page target** (`"type": "page"`,
//! `Page.*`); it maps to a [`browser::TabHandle`]. CDP's experimental **tab
//! target** is the browser-UI container and is not modeled.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use browser::{
    BrowserHandle, ExecuteScriptOptions, FrameId, RemoteValue, RunUntilJsTrueOptions, TabEvent,
    TabHandle, TabId,
};
use bytes::Bytes;
use futures_util::{SinkExt as _, StreamExt as _};
use http::{HeaderValue, Method, StatusCode, header};
use http_body_util::Full;
use hyper::upgrade::Upgraded;
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use server::{Request, Response, Shutdown};
use sha1::{Digest as _, Sha1};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tungstenite::client::IntoClientRequest;
use tungstenite::protocol::{Message, Role, WebSocket as ClientSocket};

mod dispatch;
mod params;
use dispatch::{
    BrowserHandler, DispatchError, Handler, PageHandler, RUNTIME_CLEAR_HANDLES, RUNTIME_HANDLE,
    RUNTIME_HANDLE_READ, RUNTIME_READ, RUNTIME_SCHEDULE, Target, accept, arguments_expression,
    attach_session, capture_screenshot, css_stylesheets, exception_reply, exception_text_reply,
    json_io, json_string, open_url, requested_frame, route, run_script, runtime_release_object,
    runtime_release_object_group, target_id, target_info, wait_for_navigation, ws_io,
};

/// One default browser context; Playwright requires `browserContextId` on
/// attached targets.
const DEFAULT_BROWSER_CONTEXT_ID: &str = "tinybrowser-default";
/// Fallback wait for `awaitPromise` when the client sends no `timeout`
/// (<https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#method-evaluate>).
/// The protocol's own `timeout` parameter, when present, wins.
const AWAIT_PROMISE_TIMEOUT: Duration = Duration::from_secs(30);

/// Serves CDP HTTP discovery and WebSocket endpoints on `listener`.
///
/// Returns when [`browser::BrowserHandle::close`] runs through `Browser.close`.
///
/// # Errors
///
/// Returns when the listener cannot be converted or serving fails.
pub async fn serve(listener: &TcpListener, browser: &BrowserHandle) -> io::Result<()> {
    let bound = listener.local_addr()?;
    let shutdown = Shutdown::new();
    let state = AppState {
        browser: browser.clone(),
        bound,
        stop: shutdown.clone(),
    };
    server::serve(listener, shutdown, state, serve_request).await
}

async fn serve_request(request: Request, state: AppState) -> Response {
    if !server::is_loopback_host(&request) {
        return status_response(StatusCode::FORBIDDEN, "non-loopback Host header rejected");
    }
    if server::has_origin(&request) {
        return status_response(StatusCode::FORBIDDEN, "Origin header rejected");
    }
    let path = request.uri().path().to_owned();
    let route = route_path(&path);
    match route {
        "/json/version" => get_only(&request, json_route(&version_json(&state))),
        "/json" | "/json/list" => get_only(&request, json_route(&list_json(&state).await)),
        "/devtools/browser" => upgrade_route(request, state, None).await,
        _ => match route.strip_prefix("/devtools/page/") {
            Some(raw) => upgrade_route(request, state, Some(raw.to_owned())).await,
            None => status_response(StatusCode::NOT_FOUND, "not found"),
        },
    }
}

/// A known path answers a method it does not accept with 405 + `Allow:
/// GET,HEAD` and an empty body; HEAD bodies are hyper's job to strip.
fn get_only(request: &Request, response: Response) -> Response {
    if request.method() == Method::GET || request.method() == Method::HEAD {
        return response;
    }
    server::method_not_allowed("GET,HEAD")
}

fn json_route(payload: &Value) -> Response {
    server::json(StatusCode::OK, payload)
}

/// Legacy CDP clients (Playwright included) append a trailing slash to
/// discovery URLs; both spellings route the same way.
fn route_path(path: &str) -> &str {
    if path.len() > 1
        && let Some(stripped) = path.strip_suffix('/')
    {
        return stripped;
    }
    path
}

/// Routes a websocket request: validates the handshake per RFC 6455
/// (<https://www.rfc-editor.org/rfc/rfc6455#section-4.2.1>), resolves a page
/// target when `raw` carries one, then answers the 101 and runs the socket.
async fn upgrade_route(request: Request, state: AppState, raw: Option<String>) -> Response {
    let key = match upgrade_key(&request) {
        Ok(key) => key,
        Err(rejection) => return rejection_response(rejection),
    };
    if let Some(raw) = raw {
        let Ok(id) = raw.parse::<u64>() else {
            return status_response(StatusCode::BAD_REQUEST, "invalid page target");
        };
        let Ok(tab) = state.browser.tab(TabId::new(id)).await else {
            return status_response(StatusCode::NOT_FOUND, "unknown page target");
        };
        return serve_upgraded(request, &key, state, Some(tab));
    }
    serve_upgraded(request, &key, state, None)
}

/// Why a websocket upgrade was refused, with the response it maps to.
#[derive(Clone, Copy)]
enum HandshakeRejection {
    /// Not an HTTP/1.1 request.
    HttpVersion,
    /// Not a GET request.
    Method,
    /// Missing or non-websocket `Upgrade`/`Connection` headers.
    NotWebsocket,
    /// Missing `Sec-WebSocket-Key`.
    MissingKey,
    /// `Sec-WebSocket-Version` is absent or not 13.
    VersionUnsupported,
}

fn upgrade_key(request: &Request) -> Result<String, HandshakeRejection> {
    if request.version() != http::Version::HTTP_11 {
        return Err(HandshakeRejection::HttpVersion);
    }
    if request.method() != Method::GET {
        return Err(HandshakeRejection::Method);
    }
    let headers = request.headers();
    let upgrade = headers
        .get(header::UPGRADE)
        .is_some_and(|value| value.as_bytes().eq_ignore_ascii_case(b"websocket"));
    let connection = headers.get(header::CONNECTION).is_some_and(|value| {
        value.to_str().is_ok_and(|text| {
            text.split(',')
                .any(|token| token.trim().eq_ignore_ascii_case("upgrade"))
        })
    });
    if !upgrade || !connection {
        return Err(HandshakeRejection::NotWebsocket);
    }
    let version_ok = headers
        .get(header::SEC_WEBSOCKET_VERSION)
        .is_some_and(|value| value.as_bytes() == b"13");
    if !version_ok {
        return Err(HandshakeRejection::VersionUnsupported);
    }
    headers
        .get(header::SEC_WEBSOCKET_KEY)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
        .ok_or(HandshakeRejection::MissingKey)
}

fn rejection_response(rejection: HandshakeRejection) -> Response {
    match rejection {
        HandshakeRejection::HttpVersion => {
            status_response(StatusCode::UPGRADE_REQUIRED, "upgrade requires HTTP/1.1")
        }
        HandshakeRejection::Method => status_response(
            StatusCode::METHOD_NOT_ALLOWED,
            "websocket upgrade requires GET",
        ),
        HandshakeRejection::NotWebsocket => {
            status_response(StatusCode::BAD_REQUEST, "not a websocket request")
        }
        HandshakeRejection::MissingKey => {
            status_response(StatusCode::BAD_REQUEST, "missing websocket key")
        }
        // RFC 6455 §4.4: a supported-version server answers a version
        // mismatch with 426 and its own `Sec-WebSocket-Version`.
        HandshakeRejection::VersionUnsupported => {
            let mut response = status_response(
                StatusCode::UPGRADE_REQUIRED,
                "unsupported websocket version",
            );
            response.headers_mut().insert(
                header::SEC_WEBSOCKET_VERSION,
                HeaderValue::from_static("13"),
            );
            response
                .headers_mut()
                .insert(header::CONNECTION, HeaderValue::from_static("close"));
            response
        }
    }
}

fn serve_upgraded(
    request: Request,
    key: &str,
    state: AppState,
    tab: Option<TabHandle>,
) -> Response {
    let accept = accept_key(key);
    let upgraded = hyper::upgrade::on(request);
    tokio::spawn(async move {
        let Ok(io) = upgraded.await else { return };
        let socket = WebSocketStream::from_raw_socket(TokioIo::new(io), Role::Server, None).await;
        run_socket(socket, state, tab).await;
    });
    switching_protocols(&accept)
}

fn switching_protocols(accept: &str) -> Response {
    let mut response = Response::new(Full::new(Bytes::new()));
    *response.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
    response
        .headers_mut()
        .insert(header::UPGRADE, HeaderValue::from_static("websocket"));
    response
        .headers_mut()
        .insert(header::CONNECTION, HeaderValue::from_static("Upgrade"));
    match HeaderValue::from_str(accept) {
        Ok(value) => {
            response
                .headers_mut()
                .insert(header::SEC_WEBSOCKET_ACCEPT, value);
        }
        Err(_) => {
            *response.status_mut() = StatusCode::BAD_REQUEST;
        }
    }
    response
}

const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

fn accept_key(key: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(key.as_bytes());
    hasher.update(WS_GUID.as_bytes());
    BASE64_STANDARD.encode(hasher.finalize())
}

fn status_response(status: StatusCode, message: &'static str) -> Response {
    server::text(status, message)
}

#[derive(Clone)]
struct AppState {
    browser: BrowserHandle,
    bound: SocketAddr,
    stop: Shutdown,
}

fn version_json(state: &AppState) -> Value {
    json!({
        "Browser": state.browser.identity().product,
        "Protocol-Version": "1.3",
        "webSocketDebuggerUrl": format!("ws://{}/devtools/browser", state.bound),
    })
}

async fn list_json(state: &AppState) -> Value {
    let mut targets = Vec::new();
    for id in state.browser.tabs().await.unwrap_or_default() {
        let url = match state.browser.tab(id).await {
            Ok(tab) => tab
                .document_url()
                .await
                .unwrap_or_else(|_| "about:blank".into()),
            Err(_) => "about:blank".into(),
        };
        targets.push(json!({
            "id": id.to_string(),
            "type": "page",
            "url": url,
            "webSocketDebuggerUrl": format!("ws://{}/devtools/page/{id}", state.bound),
        }));
    }
    Value::Array(targets)
}

/// Client for the browser WebSocket.
pub struct Client {
    socket: ClientSocket<TcpStream>,
    next_id: i64,
    events: VecDeque<Value>,
}

impl Client {
    /// Connects to `ws://host:port/devtools/browser`.
    ///
    /// # Errors
    ///
    /// Transport or handshake failure.
    pub fn connect(addr: SocketAddr) -> io::Result<Self> {
        connect_ws(addr, "/devtools/browser")
    }

    /// Connects to `ws://host:port{path}`.
    ///
    /// # Errors
    ///
    /// Transport or handshake failure.
    pub fn connect_path(addr: SocketAddr, path: &str) -> io::Result<Self> {
        connect_ws(addr, path)
    }

    /// Sends `method` and waits for the matching id.
    ///
    /// # Errors
    ///
    /// Transport failure or a CDP error object.
    pub fn call(
        &mut self,
        method: &str,
        params: &Value,
        session_id: Option<&str>,
    ) -> io::Result<Value> {
        self.next_id += 1;
        let id = self.next_id;
        let mut message = json!({
            "id": id,
            "method": method,
            "params": params,
        });
        if let Some(session) = session_id
            && let Some(object) = message.as_object_mut()
        {
            object.insert("sessionId".into(), json!(session));
        }
        self.socket
            .send(Message::Text(message.to_string().into()))
            .map_err(ws_io)?;
        loop {
            let incoming = self.socket.read().map_err(ws_io)?;
            let Message::Text(text) = incoming else {
                continue;
            };
            let parsed: Value = serde_json::from_str(&text).map_err(json_io)?;
            if parsed.get("id") == Some(&json!(id)) {
                if let Some(error) = parsed.get("error") {
                    return Err(io::Error::other(error.to_string()));
                }
                return Ok(parsed.get("result").cloned().unwrap_or(Value::Null));
            }
            if parsed.get("method").is_some() {
                self.events.push_back(parsed);
            }
        }
    }

    /// Reads the next protocol event, returning `None` when `timeout` elapses.
    ///
    /// # Errors
    ///
    /// Transport or JSON decoding failure.
    pub fn read_event(&mut self, timeout: Duration) -> io::Result<Option<Value>> {
        if let Some(event) = self.events.pop_front() {
            return Ok(Some(event));
        }
        self.socket.get_mut().set_read_timeout(Some(timeout))?;
        let result = loop {
            match self.socket.read() {
                Ok(Message::Text(text)) => {
                    let parsed: Value = match serde_json::from_str(&text).map_err(json_io) {
                        Ok(parsed) => parsed,
                        Err(error) => break Err(error),
                    };
                    if parsed.get("method").is_some() {
                        break Ok(Some(parsed));
                    }
                }
                Ok(Message::Ping(payload)) => {
                    if let Err(error) = self.socket.send(Message::Pong(payload)).map_err(ws_io) {
                        break Err(error);
                    }
                }
                Ok(Message::Close(_)) => break Ok(None),
                Err(tungstenite::Error::Io(error))
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    break Ok(None);
                }
                Err(error) => break Err(ws_io(error)),
                Ok(_) => {}
            }
        };
        let reset = self
            .socket
            .get_mut()
            .set_read_timeout(Some(Duration::from_secs(30)));
        match result {
            Err(error) => Err(error),
            Ok(event) => {
                reset?;
                Ok(event)
            }
        }
    }
}

fn connect_ws(addr: SocketAddr, path: &str) -> io::Result<Client> {
    let stream = TcpStream::connect(addr)?;
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    stream.set_write_timeout(Some(Duration::from_secs(30)))?;
    let url = format!("ws://{addr}{path}");
    let request = url
        .as_str()
        .into_client_request()
        .map_err(|error| io::Error::other(error.to_string()))?;
    let (socket, _response) = tungstenite::client::client(request, stream)
        .map_err(|error| io::Error::other(error.to_string()))?;
    Ok(Client {
        socket,
        next_id: 0,
        events: VecDeque::new(),
    })
}

/// Per-WebSocket async protocol state.
struct Conn {
    browser: BrowserHandle,
    sessions: HashMap<String, Target>,
    next_session: u64,
    tab: Option<TabHandle>,
    stop: Shutdown,
    subscriptions: Vec<TabSubscription>,
    tab_events_tx: mpsc::Sender<ConnEvent>,
    tab_events_rx: mpsc::Receiver<ConnEvent>,
    clock_origin: Instant,
    auto_attach: bool,
    events: Vec<Value>,
    loader_ids: HashMap<TabId, String>,
    /// Loader id of each tab's current document, kept across commit for
    /// lifecycle events
    /// (<https://chromedevtools.github.io/devtools-protocol/tot/Page/#event-lifecycleEvent>).
    current_loaders: HashMap<TabId, String>,
    /// Whether the client asked for `Page.lifecycleEvent` notifications
    /// (<https://chromedevtools.github.io/devtools-protocol/tot/Page/#method-setLifecycleEventsEnabled>).
    lifecycle_events: bool,
    isolated_worlds: HashMap<TabId, Vec<String>>,
    next_loader: u64,
    next_context: u64,
    next_evaluation: u64,
}

struct TabSubscription {
    tab_id: TabId,
    session: Option<String>,
    forward: JoinHandle<()>,
}

struct ConnEvent {
    tab: TabHandle,
    session: Option<String>,
    event: TabEvent,
}

/// One text reply plus whether the socket closes after it.
struct Outcome {
    reply: Value,
    close: bool,
}

enum RuntimeEntry {
    Evaluate,
    CallFunctionOn,
}

impl RuntimeEntry {
    /// Decodes this entry's params once: the script and the settings both
    /// `Runtime.evaluate` and `Runtime.callFunctionOn` carry.
    fn decode(
        self,
        params: &Value,
    ) -> Result<(RuntimeSource, params::RuntimeOptions), DispatchError> {
        match self {
            Self::Evaluate => {
                let request: params::RuntimeEvaluate = params::parse(params)?;
                Ok((
                    RuntimeSource::Evaluate {
                        expression: request.expression,
                    },
                    request.options,
                ))
            }
            Self::CallFunctionOn => {
                let request: params::RuntimeCallFunctionOn = params::parse(params)?;
                Ok((
                    RuntimeSource::CallFunctionOn {
                        declaration: request.function_declaration,
                        object_id: request.object_id,
                        arguments: arguments_expression(
                            request.arguments.as_deref().unwrap_or(&[]),
                        ),
                    },
                    request.options,
                ))
            }
        }
    }
}

enum RuntimeSource {
    Evaluate {
        expression: String,
    },
    CallFunctionOn {
        declaration: String,
        object_id: Option<String>,
        arguments: String,
    },
}

impl Conn {
    fn take_queued_events(&mut self) -> Vec<Value> {
        std::mem::take(&mut self.events)
    }

    async fn event_messages(&mut self, incoming: ConnEvent) -> Vec<Value> {
        let mut messages = Vec::new();
        let timestamp = self.clock_origin.elapsed().as_secs_f64();
        let tab = incoming.tab;
        let session = incoming.session;
        match incoming.event {
            TabEvent::Navigated => {
                self.push_navigated(&mut messages, &tab, session.as_deref())
                    .await;
                if self.lifecycle_events {
                    let mut lifecycle = json!({
                        "method": "Page.lifecycleEvent",
                        "params": {
                            "frameId": tab.id().to_string(),
                            "loaderId": self.current_loader(tab.id()),
                            "name": "init",
                            "timestamp": timestamp,
                        },
                    });
                    attach_session(&mut lifecycle, session.as_deref());
                    messages.push(lifecycle);
                }
            }
            TabEvent::SameDocumentNavigation => {
                let url = tab.document_url().await.unwrap_or_default();
                let mut navigated = json!({
                    "method": "Page.navigatedWithinDocument",
                    "params": {"frameId": tab.id().to_string(), "url": url},
                });
                attach_session(&mut navigated, session.as_deref());
                messages.push(navigated);
            }
            TabEvent::Load => {
                if self.lifecycle_events {
                    let mut lifecycle = json!({
                        "method": "Page.lifecycleEvent",
                        "params": {
                            "frameId": tab.id().to_string(),
                            "loaderId": self.current_loader(tab.id()),
                            "name": "load",
                            "timestamp": timestamp,
                        },
                    });
                    attach_session(&mut lifecycle, session.as_deref());
                    messages.push(lifecycle);
                }
                let mut load = json!({
                    "method": "Page.loadEventFired",
                    "params": {"timestamp": timestamp},
                });
                attach_session(&mut load, session.as_deref());
                messages.push(load);
            }
            TabEvent::DomContentLoaded => {
                if self.lifecycle_events {
                    let mut lifecycle = json!({
                        "method": "Page.lifecycleEvent",
                        "params": {
                            "frameId": tab.id().to_string(),
                            "loaderId": self.current_loader(tab.id()),
                            "name": "DOMContentLoaded",
                            "timestamp": timestamp,
                        },
                    });
                    attach_session(&mut lifecycle, session.as_deref());
                    messages.push(lifecycle);
                }
                let mut fired = json!({
                    "method": "Page.domContentEventFired",
                    "params": {"timestamp": timestamp},
                });
                attach_session(&mut fired, session.as_deref());
                messages.push(fired);
            }
            TabEvent::NavigationFailed => {}
        }
        messages
    }

    /// The tab's current document loader id, for lifecycle events that arrive
    /// after commit
    /// (<https://chromedevtools.github.io/devtools-protocol/tot/Page/#event-lifecycleEvent>).
    fn current_loader(&self, tab: TabId) -> String {
        self.current_loaders.get(&tab).cloned().unwrap_or_default()
    }

    /// Mints a fresh loader id for a navigation
    /// (<https://chromedevtools.github.io/devtools-protocol/tot/Page/#type-Frame>).
    fn next_loader_id(&mut self) -> String {
        let id = format!("{}", self.next_loader);
        self.next_loader = self.next_loader.saturating_add(1);
        id
    }

    /// Emits the commit event set: frame commit plus the new document's
    /// execution contexts (default and every known isolated world).
    async fn push_navigated(
        &mut self,
        messages: &mut Vec<Value>,
        tab: &TabHandle,
        session: Option<&str>,
    ) {
        let tab_id = tab.id();
        let frame_id = tab_id.to_string();
        // A browser-initiated `Page.navigate` stored its loader id; a
        // renderer-initiated navigation has none and mints a fresh one
        // (<https://chromedevtools.github.io/devtools-protocol/tot/Page/#type-Frame>).
        let loader_id = match self.loader_ids.remove(&tab_id) {
            Some(id) => id,
            None => self.next_loader_id(),
        };
        self.current_loaders.insert(tab_id, loader_id.clone());
        // Final URL after redirects, not the requested one.
        let url = tab.document_url().await.unwrap_or_default();
        let mut navigated = json!({
            "method": "Page.frameNavigated",
            "params": {"frame": {
                "id": frame_id,
                "loaderId": loader_id,
                "url": url,
                "mimeType": "text/html",
            }},
        });
        attach_session(&mut navigated, session);
        messages.push(navigated);
        // The new document is a new realm: drop announced contexts so clients
        // (Playwright) re-create their utility world.
        let mut cleared = json!({
            "method": "Runtime.executionContextsCleared",
            "params": {},
        });
        attach_session(&mut cleared, session);
        messages.push(cleared);
        // The outgoing realm's object handles die with it; clear the new
        // realm's table too so a handle minted before the event cannot outlive
        // the context it belongs to.
        let _ = tab.execute_browser_script(RUNTIME_CLEAR_HANDLES).await;
        let mut worlds = vec![String::new()];
        if let Some(isolated) = self.isolated_worlds.get(&tab_id) {
            worlds.extend(isolated.iter().cloned());
        }
        for world in worlds {
            let context_id = self.next_context;
            self.next_context = self.next_context.saturating_add(1);
            let default = world.is_empty();
            let mut created = json!({
                "method": "Runtime.executionContextCreated",
                "params": {"context": {
                    "id": context_id,
                    "origin": "://",
                    "name": world,
                    "uniqueId": format!("ctx-{context_id}"),
                    "auxData": {
                        "isDefault": default,
                        "type": if default { "default" } else { "isolated" },
                        "frameId": frame_id,
                    },
                }},
            });
            attach_session(&mut created, session);
            messages.push(created);
        }
    }

    /// Resolves the requested target (or the first live one) for `Target.getTargetInfo`.
    async fn target_info_for(&mut self, params: &Value) -> Result<Value, DispatchError> {
        let params: params::GetTargetInfo = params::parse(params)?;
        let id = match params.target_id {
            Some(raw) => raw
                .parse::<u64>()
                .map(TabId::new)
                .map_err(|_| DispatchError::InvalidParams("targetId".to_owned()))?,
            None => self
                .browser
                .tabs()
                .await
                .map_err(|error| DispatchError::Failed(error.to_string()))?
                .first()
                .copied()
                .ok_or_else(|| DispatchError::Failed("no target".into()))?,
        };
        Ok(json!({"targetInfo": target_info(&self.browser, id).await}))
    }

    /// Enables or disables auto-attach and announces existing targets.
    async fn set_auto_attach(
        &mut self,
        params: &Value,
        session: Option<&str>,
    ) -> Result<Value, DispatchError> {
        let params: params::SetAutoAttach = params::parse(params)?;
        let auto_attach = params.auto_attach;
        self.auto_attach = auto_attach;
        if auto_attach {
            for id in self.browser.tabs().await.unwrap_or_default() {
                if let Ok(tab) = self.browser.tab(id).await {
                    let session_id = self.mint(Target::Page(tab.clone()));
                    self.push_attached(session, &session_id, &tab).await;
                }
            }
        }
        Ok(json!({}))
    }

    /// Registers a flattened session and returns its id.
    fn mint(&mut self, session: Target) -> String {
        let session_id = format!("s{}", self.next_session);
        self.next_session = self.next_session.saturating_add(1);
        self.sessions.insert(session_id.clone(), session);
        session_id
    }

    /// Queues `Target.attachedToTarget` for auto-attach clients, on the session
    /// that asked for auto-attach.
    async fn push_attached(&mut self, session: Option<&str>, session_id: &str, tab: &TabHandle) {
        let mut info = target_info(&self.browser, tab.id()).await;
        if let Some(object) = info.as_object_mut() {
            object.insert("attached".into(), json!(true));
        }
        self.push_session_event(
            session,
            "Target.attachedToTarget",
            &json!({
                "sessionId": session_id,
                "targetInfo": info,
                "waitingForDebugger": false,
            }),
        );
    }

    async fn dispatch_text(&mut self, text: &str) -> Outcome {
        let parsed: Value = match serde_json::from_str(text) {
            Ok(value) => value,
            Err(err) => {
                return Outcome {
                    reply: json!({
                        "id": Value::Null,
                        "error": {"code": -32700, "message": err.to_string()},
                    }),
                    close: false,
                };
            }
        };
        let id = parsed.get("id").cloned().unwrap_or(Value::Null);
        let Some(method) = parsed.get("method").and_then(Value::as_str) else {
            return Outcome {
                reply: json!({
                    "id": id,
                    "error": {"code": -32600, "message": "missing method"},
                }),
                close: false,
            };
        };
        let empty = json!({});
        // A client may send `"params": null` for a method that takes none;
        // that is the same as omitting the object.
        let params = parsed
            .get("params")
            .filter(|value| !value.is_null())
            .unwrap_or(&empty);
        let session = parsed.get("sessionId").and_then(Value::as_str);
        match self.dispatch(method, params, session).await {
            Ok(result) => {
                let mut reply = json!({"id": id, "result": result});
                attach_session(&mut reply, session);
                Outcome {
                    reply,
                    close: method == "Browser.close",
                }
            }
            Err(error) => {
                let mut reply = json!({
                    "id": id,
                    "error": {
                        "code": error.code(),
                        "message": error.message(method),
                    },
                });
                attach_session(&mut reply, session);
                Outcome {
                    reply,
                    close: false,
                }
            }
        }
    }

    async fn dispatch(
        &mut self,
        method: &str,
        params: &Value,
        session: Option<&str>,
    ) -> Result<Value, DispatchError> {
        if method != "Browser.close" && self.browser.is_closed() {
            return Err(DispatchError::Failed("browser closed".into()));
        }
        let target = match session {
            Some(session) => self
                .sessions
                .get(session)
                .cloned()
                .ok_or_else(|| DispatchError::Failed("unknown session".into()))?,
            None => match self.tab.clone() {
                Some(tab) => Target::Page(tab),
                None => Target::Browser,
            },
        };
        let handler = route(method, &target).ok_or(DispatchError::MethodNotFound)?;
        self.dispatch_handler(handler, params, &target, session)
            .await
    }

    async fn dispatch_handler(
        &mut self,
        handler: Handler,
        params: &Value,
        target: &Target,
        session: Option<&str>,
    ) -> Result<Value, DispatchError> {
        match handler {
            Handler::Accept(reason) => accept(reason, params),
            Handler::ClearBrowserCookies => {
                self.browser
                    .clear_cookies()
                    .await
                    .map_err(|error| DispatchError::Failed(error.to_string()))?;
                Ok(json!({}))
            }
            Handler::GetTargetInfo => match target {
                Target::Page(tab) => {
                    let mut info = target_info(&self.browser, tab.id()).await;
                    if let Some(object) = info.as_object_mut() {
                        object.insert("attached".into(), json!(true));
                    }
                    Ok(json!({"targetInfo": info}))
                }
                Target::Browser => self.target_info_for(params).await,
            },
            Handler::Page(handler) => match target {
                Target::Page(tab) => self.dispatch_page(handler, params, tab, session).await,
                // `route` answers a page handler only for a page target, so a
                // browser target cannot reach here.
                Target::Browser => Err(DispatchError::MethodNotFound),
            },
            Handler::Browser(handler) => self.dispatch_browser(handler, params, session).await,
        }
    }

    async fn dispatch_page(
        &mut self,
        handler: PageHandler,
        params: &Value,
        tab: &TabHandle,
        session: Option<&str>,
    ) -> Result<Value, DispatchError> {
        match handler {
            PageHandler::Enable => {
                self.subscribe_tab(tab, session)?;
                Ok(json!({}))
            }
            PageHandler::Disable => {
                self.unsubscribe_tab(tab.id(), session);
                Ok(json!({}))
            }
            PageHandler::Navigate => self.navigate_tab(params, tab).await,
            PageHandler::NavigateToHistoryEntry => {
                self.navigate_to_history_entry(params, tab).await
            }
            PageHandler::CaptureScreenshot => capture_screenshot(tab, params).await,
            PageHandler::CreateIsolatedWorld => self.create_isolated_world(params, tab, session),
            PageHandler::SetLifecycleEventsEnabled => {
                let params: params::SetLifecycleEventsEnabled = params::parse(params)?;
                self.lifecycle_events = params.enabled;
                Ok(json!({}))
            }
            PageHandler::RuntimeEvaluate => {
                let (source, options) = RuntimeEntry::Evaluate.decode(params)?;
                self.dispatch_runtime(source, options, tab).await
            }
            PageHandler::RuntimeCallFunctionOn => {
                let (source, options) = RuntimeEntry::CallFunctionOn.decode(params)?;
                self.dispatch_runtime(source, options, tab).await
            }
            PageHandler::RuntimeEnable => {
                self.push_session_event(
                    session,
                    "Runtime.executionContextCreated",
                    &json!({"context": {
                        "id": tab.id().get(),
                        "origin": "://",
                        "name": "",
                        "uniqueId": format!("ctx-{}", tab.id()),
                        "auxData": {
                            "isDefault": true,
                            "type": "default",
                            "frameId": tab.id().to_string(),
                        },
                    }}),
                );
                Ok(json!({}))
            }
            PageHandler::RuntimeDisable | PageHandler::CssDisable => Ok(json!({})),
            PageHandler::RuntimeReleaseObject => runtime_release_object(tab, params).await,
            PageHandler::RuntimeReleaseObjectGroup => {
                runtime_release_object_group(tab, params).await
            }
            PageHandler::CssEnable => {
                for mut header in css_stylesheets(tab).await? {
                    if let Some(object) = header.as_object_mut() {
                        object.insert("frameId".into(), json!(tab.id().to_string()));
                    }
                    self.push_session_event(
                        session,
                        "CSS.styleSheetAdded",
                        &json!({"header": header}),
                    );
                }
                Ok(json!({}))
            }
            PageHandler::TracingEnd => {
                self.push_session_event(
                    session,
                    "Tracing.dataCollected",
                    &json!({"value": [{
                        "name": "process_name", "ph": "M",
                        "args": {"name": "tinybrowser"},
                    }]}),
                );
                self.push_session_event(
                    session,
                    "Tracing.tracingComplete",
                    &json!({"dataLossOccurred": false}),
                );
                Ok(json!({}))
            }
            PageHandler::Script(method) => run_script(method, params, tab).await,
        }
    }

    /// `Page.createIsolatedWorld`: announce a new execution context for the
    /// requested world name.
    fn create_isolated_world(
        &mut self,
        params: &Value,
        tab: &TabHandle,
        session: Option<&str>,
    ) -> Result<Value, DispatchError> {
        let params: params::CreateIsolatedWorld = params::parse(params)?;
        let context_id = self.next_context;
        self.next_context = self.next_context.saturating_add(1);
        let frame_id = params.frame_id;
        let name = params.world_name.unwrap_or_default();
        if !name.is_empty() {
            let worlds = self.isolated_worlds.entry(tab.id()).or_default();
            if !worlds.contains(&name) {
                worlds.push(name.clone());
            }
        }
        self.push_session_event(
            session,
            "Runtime.executionContextCreated",
            &json!({"context": {
                "id": context_id,
                "origin": "://",
                "name": name,
                "uniqueId": format!("ctx-{context_id}"),
                "auxData": {
                    "isDefault": false,
                    "type": "isolated",
                    "frameId": frame_id,
                },
            }}),
        );
        Ok(json!({"executionContextId": context_id}))
    }

    /// `Page.navigateToHistoryEntry`: activates the named entry. The entry id
    /// is the browser's own identity, so it travels to the browser unchanged;
    /// the browser decides whether it still holds that entry
    /// (<https://chromedevtools.github.io/devtools-protocol/tot/Page/#method-navigateToHistoryEntry>).
    async fn navigate_to_history_entry(
        &mut self,
        params: &Value,
        tab: &TabHandle,
    ) -> Result<Value, DispatchError> {
        let params: params::NavigateToHistoryEntry = params::parse(params)?;
        tab.traverse_to_entry(browser::EntryId::new(params.entry_id))
            .await
            .map_err(|error| DispatchError::Failed(error.to_string()))?;
        Ok(json!({}))
    }

    async fn dispatch_browser(
        &mut self,
        handler: BrowserHandler,
        params: &Value,
        session: Option<&str>,
    ) -> Result<Value, DispatchError> {
        match handler {
            BrowserHandler::GetVersion => Ok(json!({
                "protocolVersion": "1.3",
                "product": self.browser.identity().product.clone(),
                "revision": "0",
                "userAgent": self.browser.identity().user_agent.as_str(),
                "jsVersion": "QuickJS",
            })),
            BrowserHandler::Close => {
                let result = self.browser.close().await;
                self.sessions.clear();
                self.stop_subscriptions();
                self.tab = None;
                self.stop.request();
                result.map_err(|error| DispatchError::Failed(error.to_string()))?;
                Ok(json!({}))
            }
            BrowserHandler::SetAutoAttach => self.set_auto_attach(params, session).await,
            BrowserHandler::GetTargets => self.target_list().await,
            BrowserHandler::CreateTarget => self.create_target(params, session).await,
            BrowserHandler::CloseTarget => self.close_target(params, session).await,
            BrowserHandler::AttachToTarget => {
                let params: params::AttachToTarget = params::parse(params)?;
                let id = target_id(&params.target_id)?;
                if params.flatten != Some(true) {
                    // The protocol marks `flatten` optional, so a client that
                    // omits it is asking for a valid mode this adapter does
                    // not implement; that is a failed request, not a malformed
                    // one.
                    return Err(DispatchError::Failed(
                        "attachToTarget requires flatten".into(),
                    ));
                }
                let tab = self
                    .browser
                    .tab(id)
                    .await
                    .map_err(|err| DispatchError::Failed(err.to_string()))?;
                Ok(json!({ "sessionId": self.mint(Target::Page(tab)) }))
            }
            BrowserHandler::AttachToBrowserTarget => {
                Ok(json!({ "sessionId": self.mint(Target::Browser) }))
            }
            BrowserHandler::DetachFromTarget => {
                let params: params::DetachFromTarget = params::parse(params)?;
                if let Some(session) = params.session_id {
                    self.sessions.remove(&session);
                    self.unsubscribe_session(&session);
                }
                Ok(json!({}))
            }
        }
    }

    /// `Target.createTarget`: open a tab, navigate it, and auto-attach when the
    /// client asked for it.
    async fn create_target(
        &mut self,
        params: &Value,
        session: Option<&str>,
    ) -> Result<Value, DispatchError> {
        let params: params::CreateTarget = params::parse(params)?;
        let url = params.url.as_str();
        let tab = self
            .browser
            .create_tab()
            .await
            .map_err(|err| DispatchError::Failed(err.to_string()))?;
        if let Err(error) = open_url(&tab, url).await {
            let _result = self.browser.close_tab(tab.id()).await;
            return Err(error);
        }
        if self.auto_attach {
            let session_id = self.mint(Target::Page(tab.clone()));
            self.push_attached(session, &session_id, &tab).await;
        }
        Ok(json!({ "targetId": tab.id().to_string() }))
    }

    /// `Target.closeTarget`: announce auto-attached sessions before closing the
    /// tab and dropping its per-target state.
    async fn close_target(
        &mut self,
        params: &Value,
        session: Option<&str>,
    ) -> Result<Value, DispatchError> {
        let params: params::CloseTarget = params::parse(params)?;
        let id = target_id(&params.target_id)?;
        if self.auto_attach {
            let detached: Vec<String> = self
                .sessions
                .iter()
                .filter(|(_, session)| match session {
                    Target::Page(tab) => tab.id() == id,
                    Target::Browser => false,
                })
                .map(|(session, _)| session.clone())
                .collect();
            for session_id in detached {
                self.push_session_event(
                    session,
                    "Target.detachedFromTarget",
                    &json!({
                        "sessionId": session_id,
                        "targetId": id.to_string(),
                    }),
                );
            }
        }
        self.browser
            .close_tab(id)
            .await
            .map_err(|err| DispatchError::Failed(err.to_string()))?;
        self.sessions.retain(|_, session| match session {
            Target::Page(tab) => tab.id() != id,
            Target::Browser => true,
        });
        self.unsubscribe_tab_id(id);
        self.loader_ids.remove(&id);
        self.current_loaders.remove(&id);
        self.isolated_worlds.remove(&id);
        if self.tab.as_ref().is_some_and(|tab| tab.id() == id) {
            self.tab = None;
        }
        Ok(json!({ "success": true }))
    }

    /// Every open tab as a `Target.targetInfo` list.
    async fn target_list(&self) -> Result<Value, DispatchError> {
        let mut target_infos = Vec::new();
        for id in self
            .browser
            .tabs()
            .await
            .map_err(|error| DispatchError::Failed(error.to_string()))?
        {
            target_infos.push(target_info(&self.browser, id).await);
        }
        Ok(json!({ "targetInfos": target_infos }))
    }

    async fn navigate_tab(
        &mut self,
        params: &Value,
        tab: &TabHandle,
    ) -> Result<Value, DispatchError> {
        let params: params::Navigate = params::parse(params)?;
        let url = params.url.as_str();
        let frame_id = tab.id().to_string();
        let loader_id = self.next_loader_id();
        if url.is_empty() || url == "about:blank" {
            open_url(tab, url).await?;
            return Ok(json!({"frameId": frame_id, "loaderId": loader_id}));
        }
        // Store the id so the `frameNavigated` event reports the same loader id
        // (<https://chromedevtools.github.io/devtools-protocol/tot/Page/#event-frameNavigated>).
        self.loader_ids.insert(tab.id(), loader_id.clone());
        let events = tab
            .subscribe()
            .map_err(|error| DispatchError::Failed(error.to_string()))?;
        open_url(tab, url).await?;
        match wait_for_navigation(events, Duration::from_secs(30)).await {
            Ok(()) => Ok(json!({"frameId": frame_id, "loaderId": loader_id})),
            Err(error_text) => Ok(json!({
                "frameId": frame_id,
                "loaderId": loader_id,
                "errorText": error_text,
            })),
        }
    }

    /// Queues an event for one session (or the direct page socket).
    fn push_session_event(&mut self, session: Option<&str>, method: &str, params: &Value) {
        let mut message = json!({"method": method, "params": params});
        attach_session(&mut message, session);
        self.events.push(message);
    }

    /// `Runtime.evaluate` / `Runtime.callFunctionOn` with value or handle
    /// semantics. Handles are retained in private renderer realm state.
    async fn dispatch_runtime(
        &mut self,
        source: RuntimeSource,
        options: params::RuntimeOptions,
        tab: &TabHandle,
    ) -> Result<Value, DispatchError> {
        let frame = requested_frame(tab, options.frame_id.as_deref())?;
        let return_by_value = options.return_by_value.unwrap_or(false);
        let await_promise = options.await_promise.unwrap_or(false);
        let object_group = options.object_group.as_deref().unwrap_or("");
        // `Runtime.evaluate` carries an optional `timeout` in milliseconds;
        // `Runtime.callFunctionOn` has none, so the fallback applies. An
        // out-of-range value is a CDP error, not a panic in the server task.
        let await_timeout = match options.timeout.filter(|millis| *millis > 0.0) {
            Some(millis) => Duration::try_from_secs_f64(millis / 1000.0)
                .map_err(|_| DispatchError::InvalidParams("timeout".to_owned()))?,
            None => AWAIT_PROMISE_TIMEOUT,
        };
        let source = match source {
            // `expression` is a script (statements allowed); indirect eval keeps
            // its completion value without parenthesizing a trailing `;`.
            RuntimeSource::Evaluate { expression } => {
                format!("host.evaluatePage({})", json_string(&expression))
            }
            RuntimeSource::CallFunctionOn {
                declaration,
                object_id,
                arguments,
            } => {
                let receiver = object_id.map_or_else(
                    || "undefined".to_owned(),
                    |id| format!("host.__tb_handles[{}]", json_string(&id)),
                );
                format!(
                    "host.apply(host.evaluatePage({}), {receiver}, {arguments})",
                    json_string(&format!("({declaration})"))
                )
            }
        };
        if return_by_value {
            let id = self.next_evaluation;
            self.next_evaluation = self.next_evaluation.saturating_add(1);
            Ok(Self::runtime_value(tab, frame, &source, await_timeout, id).await)
        } else if await_promise {
            Ok(self
                .runtime_handle_awaited(tab, frame, &source, await_timeout, object_group)
                .await)
        } else {
            Ok(self.runtime_handle(tab, frame, &source, object_group).await)
        }
    }

    /// Awaits a thenable and returns its CDP `RemoteObject`, storing objects
    /// as handles: the `evaluateHandle` shape of
    /// <https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#method-callFunctionOn>.
    async fn runtime_handle_awaited(
        &mut self,
        tab: &TabHandle,
        frame: FrameId,
        source: &str,
        timeout: Duration,
        object_group: &str,
    ) -> Value {
        let evaluation = self.next_evaluation;
        self.next_evaluation = self.next_evaluation.saturating_add(1);
        let id = json_string(&evaluation.to_string());
        let read = RUNTIME_HANDLE_READ
            .replace("__ID__", &id)
            .replace("__GROUP__", &json_string(object_group));
        match Self::await_runtime_slot(
            tab,
            frame,
            source,
            timeout,
            &id,
            "__tb_async_handles",
            &read,
        )
        .await
        {
            Ok(parsed) => json!({"result": parsed.get("value").cloned().unwrap_or(Value::Null)}),
            Err(reply) => reply,
        }
    }

    /// Stores the result in a page-side handle and returns its `objectId`;
    /// primitives are serializable and returned inline.
    async fn runtime_handle(
        &mut self,
        tab: &TabHandle,
        frame: FrameId,
        source: &str,
        object_group: &str,
    ) -> Value {
        let script = RUNTIME_HANDLE
            .replace("__GROUP__", &json_string(object_group))
            .replace("__SOURCE__", source);
        let value = match tab
            .execute_script(ExecuteScriptOptions {
                frame,
                source: browser::ScriptSource::Browser(&script),
                timeout: None,
            })
            .await
        {
            Ok(value) => value,
            Err(error) => return exception_reply(&error),
        };
        if let RemoteValue::String(text) = value
            && let Ok(parsed) = serde_json::from_str::<Value>(&text)
        {
            return json!({"result": parsed});
        }
        exception_text_reply("unexpected script result")
    }

    /// Resolves the value (awaiting a thenable via the waiter) and serializes
    /// it to a CDP `RemoteObject`.
    async fn runtime_value(
        tab: &TabHandle,
        frame: FrameId,
        source: &str,
        timeout: Duration,
        id: u64,
    ) -> Value {
        let id = json_string(&id.to_string());
        let read = RUNTIME_READ.replace("__ID__", &id);
        match Self::await_runtime_slot(tab, frame, source, timeout, &id, "__tb_async", &read).await
        {
            Ok(parsed) => json!({"result": parsed}),
            Err(reply) => reply,
        }
    }

    /// Schedules `source` into `host.<slot>[id]`, waits for it to settle (or
    /// for a main-frame navigation to destroy the realm), cleans up on
    /// timeout, reads the serialized slot via `read`, and surfaces any page
    /// error as a CDP exception reply.
    async fn await_runtime_slot(
        tab: &TabHandle,
        frame: FrameId,
        source: &str,
        timeout: Duration,
        id: &str,
        slot: &str,
        read: &str,
    ) -> Result<Value, Value> {
        let schedule = RUNTIME_SCHEDULE
            .replace("__SLOT__", slot)
            .replace("__ID__", id)
            .replace("__SOURCE__", source);
        if let Err(error) = tab
            .execute_script(ExecuteScriptOptions {
                frame,
                source: browser::ScriptSource::Browser(&schedule),
                timeout: None,
            })
            .await
        {
            return Err(exception_reply(&error));
        }
        let ready = format!("Boolean(host.{slot} && host.{slot}[{id}] && host.{slot}[{id}].done)");
        // A navigation replaces the main frame's realm, so an awaited slot in
        // the old realm never settles. Stop waiting as soon as the navigation
        // commits instead of polling until the timeout, which would also hold
        // back the tab events the socket loop is waiting to deliver
        // (<https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#method-callFunctionOn>).
        let mut events = tab.subscribe().ok();
        let run = tab.run_until_js_true(RunUntilJsTrueOptions {
            frame,
            source: browser::ScriptSource::Browser(&ready),
            timeout,
        });
        tokio::pin!(run);
        let settled = loop {
            tokio::select! {
                result = &mut run => break result,
                event = next_tab_event(&mut events) => match event {
                    Some(TabEvent::Navigated) if frame == FrameId::MAIN => {
                        return Err(exception_text_reply("execution context was destroyed"));
                    }
                    Some(_) => {}
                    None => events = None,
                },
            }
        };
        match settled {
            Ok(true) => {}
            Ok(false) => {
                // Drop the slot so a late settle cannot pile up results; the
                // promise closure keeps its own reference to the slot.
                let _ = tab
                    .execute_script(ExecuteScriptOptions {
                        frame,
                        source: browser::ScriptSource::Browser(&format!(
                            "(() => {{ if (host.{slot}) delete host.{slot}[{id}]; }})()"
                        )),
                        timeout: None,
                    })
                    .await;
                return Err(exception_text_reply("awaitPromise timed out"));
            }
            Err(error) => return Err(exception_reply(&error)),
        }
        let value = match tab
            .execute_script(ExecuteScriptOptions {
                frame,
                source: browser::ScriptSource::Browser(read),
                timeout: None,
            })
            .await
        {
            Ok(value) => value,
            Err(error) => return Err(exception_reply(&error)),
        };
        if let RemoteValue::String(text) = value
            && let Ok(parsed) = serde_json::from_str::<Value>(&text)
        {
            if let Some(error) = parsed.get("error").and_then(Value::as_str) {
                return Err(exception_text_reply(error));
            }
            return Ok(parsed);
        }
        Err(exception_text_reply("unexpected script result"))
    }

    fn subscribe_tab(
        &mut self,
        tab: &TabHandle,
        session: Option<&str>,
    ) -> Result<(), DispatchError> {
        if self
            .subscriptions
            .iter()
            .any(|item| item.tab_id == tab.id() && item.session.as_deref() == session)
        {
            return Ok(());
        }
        let events = tab
            .subscribe()
            .map_err(|error| DispatchError::Failed(error.to_string()))?;
        let tab_for_events = tab.clone();
        let session_for_events = session.map(str::to_owned);
        let tx = self.tab_events_tx.clone();
        let mut events = events;
        let forward = tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                let message = ConnEvent {
                    tab: tab_for_events.clone(),
                    session: session_for_events.clone(),
                    event,
                };
                if tx.send(message).await.is_err() {
                    break;
                }
            }
        });
        self.subscriptions.push(TabSubscription {
            tab_id: tab.id(),
            session: session.map(str::to_owned),
            forward,
        });
        Ok(())
    }

    fn remove_subscriptions(&mut self, mut matches: impl FnMut(&TabSubscription) -> bool) {
        let mut retained = Vec::new();
        for subscription in self.subscriptions.drain(..) {
            if matches(&subscription) {
                subscription.forward.abort();
            } else {
                retained.push(subscription);
            }
        }
        self.subscriptions = retained;
    }

    fn unsubscribe_tab(&mut self, tab_id: TabId, session: Option<&str>) {
        self.remove_subscriptions(|s| s.tab_id == tab_id && s.session.as_deref() == session);
    }

    fn stop_subscriptions(&mut self) {
        self.remove_subscriptions(|_| true);
    }

    fn unsubscribe_tab_id(&mut self, tab_id: TabId) {
        self.remove_subscriptions(|s| s.tab_id == tab_id);
    }

    fn unsubscribe_session(&mut self, session: &str) {
        self.remove_subscriptions(|s| s.session.as_deref() == Some(session));
    }
}

/// Receives the next tab event, or pends forever once the subscription is
/// dropped so a closed channel does not spin.
async fn next_tab_event(receiver: &mut Option<mpsc::Receiver<TabEvent>>) -> Option<TabEvent> {
    match receiver {
        Some(receiver) => receiver.recv().await,
        None => std::future::pending().await,
    }
}

async fn run_socket(
    mut socket: WebSocketStream<TokioIo<Upgraded>>,
    state: AppState,
    tab: Option<TabHandle>,
) {
    let (tab_events_tx, tab_events_rx) = mpsc::channel(256);
    let mut conn = Conn {
        browser: state.browser,
        sessions: HashMap::new(),
        next_session: 1,
        tab,
        stop: state.stop,
        subscriptions: Vec::new(),
        tab_events_tx,
        tab_events_rx,
        clock_origin: Instant::now(),
        auto_attach: false,
        events: Vec::new(),
        loader_ids: HashMap::new(),
        current_loaders: HashMap::new(),
        lifecycle_events: false,
        isolated_worlds: HashMap::new(),
        next_loader: 1,
        next_context: 1000,
        next_evaluation: 1,
    };
    loop {
        let wake = tokio::select! {
            event = conn.tab_events_rx.recv() => SocketWake::Event(event),
            incoming = socket.next() => SocketWake::Incoming(incoming),
        };
        match wake {
            SocketWake::Event(Some(event)) => {
                let messages = conn.event_messages(event).await;
                if send_messages(&mut socket, messages).await.is_err() {
                    break;
                }
            }
            SocketWake::Event(None)
            | SocketWake::Incoming(None | Some(Ok(WsMessage::Close(_)) | Err(_))) => break,
            SocketWake::Incoming(Some(Ok(WsMessage::Text(text)))) => {
                let outcome = conn.dispatch_text(text.as_str()).await;
                let queued = conn.take_queued_events();
                if send_messages(&mut socket, queued).await.is_err() {
                    break;
                }
                let close = outcome.close;
                if socket
                    .send(WsMessage::text(outcome.reply.to_string()))
                    .await
                    .is_err()
                {
                    break;
                }
                if close {
                    break;
                }
            }
            SocketWake::Incoming(Some(Ok(WsMessage::Ping(payload)))) => {
                let _result = socket.send(WsMessage::Pong(payload)).await;
            }
            SocketWake::Incoming(Some(Ok(_))) => {}
        }
    }
    conn.stop_subscriptions();
    let _result = socket.send(WsMessage::Close(None)).await;
}

enum SocketWake {
    Event(Option<ConnEvent>),
    Incoming(Option<Result<WsMessage, tungstenite::Error>>),
}

async fn send_messages(
    socket: &mut WebSocketStream<TokioIo<Upgraded>>,
    messages: Vec<Value>,
) -> Result<(), tungstenite::Error> {
    for message in messages {
        socket.send(WsMessage::text(message.to_string())).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use browser::{Browser, BrowserOptions};

    use super::*;

    static TEST_DIR: AtomicU64 = AtomicU64::new(1);

    fn test_browser() -> (Browser, std::path::PathBuf) {
        let data_home = std::env::temp_dir().join(format!(
            "tinybrowser-cdp-test-{}-{}",
            std::process::id(),
            TEST_DIR.fetch_add(1, Ordering::Relaxed)
        ));
        let browser = Browser::new(BrowserOptions {
            data_home: Some(data_home.clone()),
            ..BrowserOptions::default()
        })
        .expect("test browser");
        (browser, data_home)
    }

    fn test_conn(browser: &Browser) -> Conn {
        let (tab_events_tx, tab_events_rx) = mpsc::channel(8);
        Conn {
            browser: browser.handle().clone(),
            sessions: HashMap::new(),
            next_session: 1,
            tab: None,
            stop: Shutdown::new(),
            subscriptions: Vec::new(),
            tab_events_tx,
            tab_events_rx,
            clock_origin: Instant::now(),
            auto_attach: false,
            events: Vec::new(),
            loader_ids: HashMap::new(),
            current_loaders: HashMap::new(),
            lifecycle_events: false,
            isolated_worlds: HashMap::new(),
            next_loader: 1,
            next_context: 1000,
            next_evaluation: 1,
        }
    }

    #[tokio::test]
    async fn unknown_method_reports_exact_method_not_found() {
        let (browser, data_home) = test_browser();
        let mut conn = test_conn(&browser);

        let outcome = conn
            .dispatch_text(r#"{"id":7,"method":"Foo.bar","params":{}}"#)
            .await;

        assert_eq!(
            outcome.reply,
            json!({
                "id": 7,
                "error": {"code": -32601, "message": "'Foo.bar' wasn't found"},
            })
        );
        assert!(!outcome.close);

        browser.close().await.expect("close");
        let _ = std::fs::remove_dir_all(data_home);
    }

    #[tokio::test]
    async fn page_params_are_parsed_at_the_boundary() {
        let (browser, data_home) = test_browser();
        let mut conn = test_conn(&browser);
        let tab = browser.handle().create_tab().await.expect("tab");
        let session = conn.mint(Target::Page(tab));

        let default_media = json!({
            "id": 1,
            "method": "Emulation.setEmulatedMedia",
            "params": {
                "media": "",
                "features": [
                    {"name": "prefers-color-scheme", "value": ""},
                    {"name": "prefers-reduced-motion", "value": ""},
                ],
            },
            "sessionId": session,
        });
        let outcome = conn.dispatch_text(&default_media.to_string()).await;
        assert_eq!(outcome.reply["result"], json!({}));

        let print_media = json!({
            "id": 2,
            "method": "Emulation.setEmulatedMedia",
            "params": {"media": "print"},
            "sessionId": session,
        });
        let outcome = conn.dispatch_text(&print_media.to_string()).await;
        assert_eq!(outcome.reply["error"]["code"], json!(-32601));

        let cache_enabled = json!({
            "id": 3,
            "method": "Network.setCacheDisabled",
            "params": {"cacheDisabled": false},
            "sessionId": session,
        });
        let outcome = conn.dispatch_text(&cache_enabled.to_string()).await;
        assert_eq!(outcome.reply["error"]["code"], json!(-32601));

        let get_properties = json!({
            "id": 4,
            "method": "Runtime.getProperties",
            "params": {"ownProperties": true},
            "sessionId": session,
        });
        let outcome = conn.dispatch_text(&get_properties.to_string()).await;
        assert_eq!(outcome.reply["error"]["code"], json!(-32602));
        assert!(
            outcome.reply["error"]["message"]
                .as_str()
                .is_some_and(|message| message.starts_with("Invalid params")),
            "{}",
            outcome.reply
        );

        browser.close().await.expect("close");
        let _ = std::fs::remove_dir_all(data_home);
    }
}
