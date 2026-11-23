use std::collections::{HashMap, HashSet, VecDeque};
use std::io;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use browser::{
    BrowserHandle, ConsoleLevel, ExecuteScriptOptions, FrameId, RemoteValue, RunUntilJsTrueOptions,
    TabEvent, TabHandle, TabId,
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
    RUNTIME_READ, RUNTIME_SCHEDULE, Target, accept, add_binding, arguments_expression,
    attach_session, capture_screenshot, css_stylesheets, exception_reply, exception_text_reply,
    json_io, json_string, open_url, open_url_with_referrer, requested_frame, route, run_script,
    runtime_release_object, runtime_release_object_group, target_id, target_info,
    wait_for_navigation, ws_io,
};

const DEFAULT_BROWSER_CONTEXT_ID: &str = "tinybrowser-default";
// https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#method-evaluate
const AWAIT_PROMISE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub struct Router {
    state: AppState,
}

impl Router {
    #[must_use]
    pub fn new(browser: BrowserHandle, bound: SocketAddr, shutdown: Shutdown) -> Self {
        Self {
            state: AppState {
                browser,
                bound,
                stop: shutdown,
                contexts: Arc::new(Mutex::new(BrowserContexts::default())),
            },
        }
    }

    pub async fn handle(&self, request: Request) -> Response {
        serve_request(request, self.state.clone()).await
    }
}

pub async fn serve(listener: &TcpListener, browser: &BrowserHandle) -> io::Result<()> {
    let bound = listener.local_addr()?;
    let shutdown = Shutdown::new();
    let router = Router::new(browser.clone(), bound, shutdown.clone());
    server::serve(listener, shutdown, router, |request, router| async move {
        router.handle(request).await
    })
    .await
}

async fn serve_request(request: Request, state: AppState) -> Response {
    if !server::is_loopback_host(&request) {
        return status_response(StatusCode::FORBIDDEN, "non-loopback Host header rejected");
    }
    if server::has_origin(&request) {
        return status_response(StatusCode::FORBIDDEN, "Origin header rejected");
    }
    if server::has_browser_fetch_metadata(&request) {
        return status_response(StatusCode::FORBIDDEN, "Sec-Fetch-* header rejected");
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

fn get_only(request: &Request, response: Response) -> Response {
    if request.method() == Method::GET || request.method() == Method::HEAD {
        return response;
    }
    server::method_not_allowed("GET,HEAD")
}

fn json_route(payload: &Value) -> Response {
    server::json(StatusCode::OK, payload)
}

fn route_path(path: &str) -> &str {
    if path.len() > 1
        && let Some(stripped) = path.strip_suffix('/')
    {
        return stripped;
    }
    path
}

// https://www.rfc-editor.org/rfc/rfc6455#section-4.2.1
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

#[derive(Clone, Copy)]
enum HandshakeRejection {
    HttpVersion,
    Method,
    NotWebsocket,
    MissingKey,
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
        let config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
            .max_message_size(Some(8 * 1024 * 1024))
            .max_frame_size(Some(1024 * 1024));
        let socket =
            WebSocketStream::from_raw_socket(TokioIo::new(io), Role::Server, Some(config)).await;
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

#[derive(Default)]
struct BrowserContexts {
    next: u64,
    created: HashSet<String>,
    tabs: HashMap<TabId, String>,
}

impl BrowserContexts {
    fn create(&mut self) -> String {
        self.next = self.next.saturating_add(1);
        let id = format!("ctx-{}", self.next);
        self.created.insert(id.clone());
        id
    }

    fn contains(&self, id: &str) -> bool {
        id == DEFAULT_BROWSER_CONTEXT_ID || self.created.contains(id)
    }

    fn list(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.created.iter().cloned().collect();
        ids.sort();
        ids
    }

    fn assign_tab(&mut self, tab: TabId, context_id: &str) {
        if context_id != DEFAULT_BROWSER_CONTEXT_ID {
            self.tabs.insert(tab, context_id.to_owned());
        }
    }

    fn remove_tab(&mut self, tab: TabId) {
        self.tabs.remove(&tab);
    }

    fn context_of(&self, tab: TabId) -> String {
        self.tabs
            .get(&tab)
            .cloned()
            .unwrap_or_else(|| DEFAULT_BROWSER_CONTEXT_ID.to_owned())
    }

    fn dispose(&mut self, context_id: &str) -> Option<Vec<TabId>> {
        if !self.created.remove(context_id) {
            return None;
        }
        let tabs: Vec<TabId> = self
            .tabs
            .iter()
            .filter(|(_, assigned)| assigned.as_str() == context_id)
            .map(|(tab, _)| *tab)
            .collect();
        for tab in &tabs {
            self.tabs.remove(tab);
        }
        Some(tabs)
    }
}

#[derive(Clone)]
struct AppState {
    browser: BrowserHandle,
    bound: SocketAddr,
    stop: Shutdown,
    contexts: Arc<Mutex<BrowserContexts>>,
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

pub struct Client {
    socket: ClientSocket<TcpStream>,
    next_id: i64,
    events: VecDeque<Value>,
}

impl Client {
    pub fn connect(addr: SocketAddr) -> io::Result<Self> {
        connect_ws(addr, "/devtools/browser")
    }

    pub fn connect_path(addr: SocketAddr, path: &str) -> io::Result<Self> {
        connect_ws(addr, path)
    }

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

struct Conn {
    browser: BrowserHandle,
    contexts: Arc<Mutex<BrowserContexts>>,
    sessions: HashMap<String, Target>,
    next_session: u64,
    tab: Option<TabHandle>,
    stop: Shutdown,
    subscriptions: Vec<TabSubscription>,
    tab_events_tx: mpsc::Sender<ConnEvent>,
    tab_events_rx: mpsc::Receiver<ConnEvent>,
    deferred_tx: Option<mpsc::UnboundedSender<Value>>,
    clock_origin: Instant,
    auto_attach: bool,
    events: Vec<Value>,
    loader_ids: HashMap<TabId, String>,
    // https://chromedevtools.github.io/devtools-protocol/tot/Page/#event-lifecycleEvent
    current_loaders: HashMap<TabId, String>,
    // https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#event-consoleAPICalled
    default_contexts: HashMap<TabId, u64>,
    // https://chromedevtools.github.io/devtools-protocol/tot/Page/#method-setLifecycleEventsEnabled
    lifecycle_events: bool,
    isolated_worlds: HashMap<TabId, Vec<String>>,
    omit_background: HashMap<TabId, bool>,
    response_bodies: HashMap<String, Vec<u8>>,
    response_body_order: VecDeque<String>,
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

struct Outcome {
    reply: Value,
    close: bool,
    deferred: bool,
}

enum DispatchResult {
    Immediate(Value),
    Deferred,
}

enum RuntimeEntry {
    Evaluate,
    CallFunctionOn,
}

impl RuntimeEntry {
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
        let tab_id = tab.id();
        let sess = session.as_deref();
        match incoming.event {
            TabEvent::Navigated => {
                self.push_navigated(&mut messages, &tab, sess).await;
                self.push_lifecycle_event(&mut messages, tab_id, "init", timestamp, sess);
            }
            TabEvent::SameDocumentNavigation => {
                let url = tab.document_url().await.unwrap_or_default();
                push_event(
                    &mut messages,
                    "Page.navigatedWithinDocument",
                    &json!({"frameId": tab_id.to_string(), "url": url}),
                    sess,
                );
            }
            TabEvent::Load => {
                self.push_lifecycle_event(&mut messages, tab_id, "load", timestamp, sess);
                push_event(
                    &mut messages,
                    "Page.loadEventFired",
                    &json!({"timestamp": timestamp}),
                    sess,
                );
            }
            TabEvent::DomContentLoaded => {
                self.push_lifecycle_event(
                    &mut messages,
                    tab_id,
                    "DOMContentLoaded",
                    timestamp,
                    sess,
                );
                push_event(
                    &mut messages,
                    "Page.domContentEventFired",
                    &json!({"timestamp": timestamp}),
                    sess,
                );
            }
            TabEvent::NavigationFailed(_) => {}
            TabEvent::NavigationRequested { url } => {
                self.push_navigation_requested(&mut messages, tab_id, &url, timestamp, sess);
            }
            TabEvent::NavigationResponded {
                status,
                status_text,
                url,
                headers,
                body,
            } => {
                let response = NavigationResponse {
                    status,
                    status_text,
                    url,
                    headers,
                    body,
                };
                self.push_navigation_responded(&mut messages, tab_id, response, timestamp, sess);
            }
            TabEvent::RealmReplaced => {
                // https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#document-open-steps
                let frame_id = tab_id.to_string();
                self.push_execution_contexts(&mut messages, tab_id, &frame_id, sess);
            }
            TabEvent::Binding { name, payload } => {
                // https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#event-bindingCalled
                let context = self
                    .default_contexts
                    .get(&tab_id)
                    .copied()
                    .unwrap_or_default();
                push_event(
                    &mut messages,
                    "Runtime.bindingCalled",
                    &json!({"name": name, "payload": payload, "executionContextId": context}),
                    sess,
                );
            }
            TabEvent::Console { level, args } => {
                // https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#event-consoleAPICalled
                self.push_console_called(&mut messages, tab_id, level, &args, timestamp, sess);
            }
        }
        messages
    }

    // https://chromedevtools.github.io/devtools-protocol/tot/Page/#event-lifecycleEvent
    fn push_lifecycle_event(
        &self,
        messages: &mut Vec<Value>,
        tab: TabId,
        name: &str,
        timestamp: f64,
        session: Option<&str>,
    ) {
        if !self.lifecycle_events {
            return;
        }
        let mut lifecycle = json!({
            "method": "Page.lifecycleEvent",
            "params": {
                "frameId": tab.to_string(),
                "loaderId": self.current_loader(tab),
                "name": name,
                "timestamp": timestamp,
            },
        });
        attach_session(&mut lifecycle, session);
        messages.push(lifecycle);
    }

    // https://chromedevtools.github.io/devtools-protocol/tot/Network/#event-requestWillBeSent
    fn push_navigation_requested(
        &mut self,
        messages: &mut Vec<Value>,
        tab: TabId,
        url: &str,
        timestamp: f64,
        session: Option<&str>,
    ) {
        let loader_id = if let Some(id) = self.loader_ids.get(&tab) {
            id.clone()
        } else {
            let id = self.next_loader_id();
            self.loader_ids.insert(tab, id.clone());
            id
        };
        let mut sent = json!({
            "method": "Network.requestWillBeSent",
            "params": {
                "requestId": loader_id,
                "loaderId": loader_id,
                "documentURL": url,
                "request": {
                    "url": url,
                    "method": "GET",
                    "headers": {},
                },
                "timestamp": timestamp,
                "type": "Document",
                "frameId": tab.to_string(),
                "initiator": {"type": "other"},
            },
        });
        attach_session(&mut sent, session);
        messages.push(sent);
    }

    // https://chromedevtools.github.io/devtools-protocol/tot/Network/#event-responseReceived
    fn push_navigation_responded(
        &mut self,
        messages: &mut Vec<Value>,
        tab: TabId,
        response: NavigationResponse,
        timestamp: f64,
        session: Option<&str>,
    ) {
        let Some(request_id) = self.loader_ids.get(&tab).cloned() else {
            return;
        };
        if response.body.len() <= 4 * 1024 * 1024 {
            while self.response_body_order.len() >= 32 {
                if let Some(oldest) = self.response_body_order.pop_front() {
                    self.response_bodies.remove(&oldest);
                }
            }
            if !self.response_bodies.contains_key(&request_id) {
                self.response_body_order.push_back(request_id.clone());
            }
            self.response_bodies
                .insert(request_id.clone(), response.body);
        }
        let mut header_map = serde_json::Map::new();
        for (name, value) in response.headers {
            header_map.insert(name, Value::String(value));
        }
        let headers = Value::Object(header_map);
        let mut received = json!({
            "method": "Network.responseReceived",
            "params": {
                "requestId": request_id,
                "timestamp": timestamp,
                "type": "Document",
                "response": {
                    "url": response.url,
                    "status": response.status,
                    "statusText": response.status_text,
                    "headers": headers,
                    "mimeType": "",
                    "connectionReused": false,
                    "connectionId": 0,
                    "fromDiskCache": false,
                    "fromPrefetchCache": false,
                    "encodedDataLength": 0,
                    "timing": {
                        "requestTime": timestamp,
                        "proxyStart": -1,
                        "proxyEnd": -1,
                        "dnsStart": -1,
                        "dnsEnd": -1,
                        "connectStart": -1,
                        "connectEnd": -1,
                        "sslStart": -1,
                        "sslEnd": -1,
                        "workerStart": -1,
                        "workerReady": -1,
                        "workerFetchStart": -1,
                        "workerRespondWithSetCacheKey": -1,
                        "sendStart": 0,
                        "sendEnd": 0,
                        "pushStart": 0,
                        "pushEnd": 0,
                        "receiveHeadersEnd": 0,
                    },
                },
            },
        });
        attach_session(&mut received, session);
        messages.push(received);
        let mut finished = json!({
            "method": "Network.loadingFinished",
            "params": {
                "requestId": request_id,
                "timestamp": timestamp,
                "encodedDataLength": 0,
            },
        });
        attach_session(&mut finished, session);
        messages.push(finished);
    }

    // https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#event-consoleAPICalled
    fn push_console_called(
        &self,
        messages: &mut Vec<Value>,
        tab: TabId,
        level: ConsoleLevel,
        args: &str,
        timestamp: f64,
        session: Option<&str>,
    ) {
        let Ok(args) = serde_json::from_str::<Value>(args) else {
            return;
        };
        let context = self.default_contexts.get(&tab).copied().unwrap_or_default();
        let mut called = json!({
            "method": "Runtime.consoleAPICalled",
            "params": {
                "type": level.as_str(),
                "args": args,
                "executionContextId": context,
                "timestamp": timestamp,
            },
        });
        attach_session(&mut called, session);
        messages.push(called);
    }

    // https://chromedevtools.github.io/devtools-protocol/tot/Page/#event-lifecycleEvent
    fn current_loader(&self, tab: TabId) -> String {
        self.current_loaders.get(&tab).cloned().unwrap_or_default()
    }

    // https://chromedevtools.github.io/devtools-protocol/tot/Page/#type-Frame
    fn next_loader_id(&mut self) -> String {
        let id = format!("{}", self.next_loader);
        self.next_loader = self.next_loader.saturating_add(1);
        id
    }

    async fn push_navigated(
        &mut self,
        messages: &mut Vec<Value>,
        tab: &TabHandle,
        session: Option<&str>,
    ) {
        let tab_id = tab.id();
        let frame_id = tab_id.to_string();
        // https://chromedevtools.github.io/devtools-protocol/tot/Page/#type-Frame
        let loader_id = match self.loader_ids.remove(&tab_id) {
            Some(id) => id,
            None => self.next_loader_id(),
        };
        self.current_loaders.insert(tab_id, loader_id.clone());
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
        self.push_execution_contexts(messages, tab_id, &frame_id, session);
        let _ = tab.execute_browser_script(RUNTIME_CLEAR_HANDLES).await;
    }

    // https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#event-executionContextCreated
    fn push_execution_contexts(
        &mut self,
        messages: &mut Vec<Value>,
        tab_id: TabId,
        frame_id: &str,
        session: Option<&str>,
    ) {
        let mut cleared = json!({
            "method": "Runtime.executionContextsCleared",
            "params": {},
        });
        attach_session(&mut cleared, session);
        messages.push(cleared);
        let mut worlds = vec![String::new()];
        if let Some(isolated) = self.isolated_worlds.get(&tab_id) {
            worlds.extend(isolated.iter().cloned());
        }
        for world in worlds {
            let context_id = self.next_context;
            self.next_context = self.next_context.saturating_add(1);
            let default = world.is_empty();
            if default {
                self.default_contexts.insert(tab_id, context_id);
            }
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

    fn context_of(&self, id: TabId) -> String {
        self.contexts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .context_of(id)
    }

    // https://chromedevtools.github.io/devtools-protocol/tot/Target/#method-getTargetInfo
    async fn target_info_for(&mut self, params: &Value) -> Result<Value, DispatchError> {
        let params: params::GetTargetInfo = params::parse(params)?;
        match params.target_id {
            Some(raw) => {
                let id = raw
                    .parse::<u64>()
                    .map(TabId::new)
                    .map_err(|_| DispatchError::InvalidParams("targetId".to_owned()))?;
                let context_id = self.context_of(id);
                Ok(json!({"targetInfo": target_info(&self.browser, id, &context_id).await}))
            }
            None => Ok(json!({
                "targetInfo": {
                    "targetId": "browser",
                    "type": "browser",
                    "title": "",
                    "url": "",
                    "attached": true,
                    "canAccessOpener": false,
                }
            })),
        }
    }

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

    fn mint(&mut self, session: Target) -> String {
        let session_id = format!("s{}", self.next_session);
        self.next_session = self.next_session.saturating_add(1);
        self.sessions.insert(session_id.clone(), session);
        session_id
    }

    async fn push_attached(&mut self, session: Option<&str>, session_id: &str, tab: &TabHandle) {
        let context_id = self.context_of(tab.id());
        let mut info = target_info(&self.browser, tab.id(), &context_id).await;
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
                    deferred: false,
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
                deferred: false,
            };
        };
        let empty = json!({});
        let params = parsed
            .get("params")
            .filter(|value| !value.is_null())
            .unwrap_or(&empty);
        let session = parsed.get("sessionId").and_then(Value::as_str);
        match self.dispatch(method, params, &id, session).await {
            Ok(DispatchResult::Immediate(result)) => {
                let mut reply = json!({"id": id, "result": result});
                attach_session(&mut reply, session);
                Outcome {
                    reply,
                    close: method == "Browser.close",
                    deferred: false,
                }
            }
            Ok(DispatchResult::Deferred) => Outcome {
                reply: Value::Null,
                close: false,
                deferred: true,
            },
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
                    deferred: false,
                }
            }
        }
    }

    async fn dispatch(
        &mut self,
        method: &str,
        params: &Value,
        request_id: &Value,
        session: Option<&str>,
    ) -> Result<DispatchResult, DispatchError> {
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
        self.dispatch_handler(handler, params, &target, request_id, session)
            .await
    }

    async fn dispatch_handler(
        &mut self,
        handler: Handler,
        params: &Value,
        target: &Target,
        request_id: &Value,
        session: Option<&str>,
    ) -> Result<DispatchResult, DispatchError> {
        match handler {
            Handler::Accept(reason) => accept(reason, params).map(DispatchResult::Immediate),
            Handler::ClearBrowserCookies => {
                self.browser
                    .clear_cookies()
                    .await
                    .map_err(|error| DispatchError::Failed(error.to_string()))?;
                Ok(DispatchResult::Immediate(json!({})))
            }
            Handler::ClearBrowserCache => {
                self.browser
                    .clear_cache()
                    .await
                    .map_err(|error| DispatchError::Failed(error.to_string()))?;
                Ok(DispatchResult::Immediate(json!({})))
            }
            Handler::SetCacheDisabled => {
                let params: params::SetCacheDisabled = params::parse(params)?;
                self.browser
                    .set_cache_disabled(params.cache_disabled)
                    .await
                    .map_err(|error| DispatchError::Failed(error.to_string()))?;
                Ok(DispatchResult::Immediate(json!({})))
            }
            Handler::GetTargetInfo => match target {
                Target::Page(tab) => {
                    let context_id = self.context_of(tab.id());
                    let mut info = target_info(&self.browser, tab.id(), &context_id).await;
                    if let Some(object) = info.as_object_mut() {
                        object.insert("attached".into(), json!(true));
                    }
                    Ok(DispatchResult::Immediate(json!({"targetInfo": info})))
                }
                Target::Browser => self
                    .target_info_for(params)
                    .await
                    .map(DispatchResult::Immediate),
            },
            Handler::Page(handler) => match target {
                Target::Page(tab) => {
                    self.dispatch_page(handler, params, tab, request_id, session)
                        .await
                }
                Target::Browser => Err(DispatchError::MethodNotFound),
            },
            Handler::Browser(handler) => self
                .dispatch_browser(handler, params, session)
                .await
                .map(DispatchResult::Immediate),
        }
    }

    async fn dispatch_page(
        &mut self,
        handler: PageHandler,
        params: &Value,
        tab: &TabHandle,
        request_id: &Value,
        session: Option<&str>,
    ) -> Result<DispatchResult, DispatchError> {
        match handler {
            PageHandler::RuntimeEvaluate => {
                let (source, options) = RuntimeEntry::Evaluate.decode(params)?;
                self.dispatch_runtime(source, options, tab, request_id, session)
                    .await
            }
            PageHandler::RuntimeCallFunctionOn => {
                let (source, options) = RuntimeEntry::CallFunctionOn.decode(params)?;
                self.dispatch_runtime(source, options, tab, request_id, session)
                    .await
            }
            PageHandler::Navigate => self.navigate_tab(params, tab, request_id, session).await,
            other => self
                .dispatch_page_sync(other, params, tab, session)
                .await
                .map(DispatchResult::Immediate),
        }
    }

    async fn dispatch_page_sync(
        &mut self,
        handler: PageHandler,
        params: &Value,
        tab: &TabHandle,
        session: Option<&str>,
    ) -> Result<Value, DispatchError> {
        match handler {
            PageHandler::RuntimeEvaluate
            | PageHandler::RuntimeCallFunctionOn
            | PageHandler::Navigate => Err(DispatchError::MethodNotFound),
            PageHandler::Enable => {
                self.subscribe_tab(tab, session)?;
                Ok(json!({}))
            }
            PageHandler::Disable => {
                self.unsubscribe_tab(tab.id(), session);
                Ok(json!({}))
            }
            PageHandler::NavigateToHistoryEntry => {
                self.navigate_to_history_entry(params, tab).await
            }
            PageHandler::CaptureScreenshot => {
                let omit = self
                    .omit_background
                    .get(&tab.id())
                    .copied()
                    .unwrap_or(false);
                capture_screenshot(tab, params, omit).await
            }
            PageHandler::SetDefaultBackgroundColorOverride => {
                self.set_default_background_color_override(params, tab)
            }
            PageHandler::NetworkGetResponseBody => self.get_response_body(params),
            PageHandler::NetworkSetExtraHttpHeaders => {
                let params: params::SetExtraHttpHeaders = params::parse(params)?;
                if !params.headers.values().all(Value::is_string) {
                    return Err(DispatchError::InvalidParams("headers".to_owned()));
                }
                Ok(json!({}))
            }
            PageHandler::CreateIsolatedWorld => self.create_isolated_world(params, tab, session),
            PageHandler::SetLifecycleEventsEnabled => {
                let params: params::SetLifecycleEventsEnabled = params::parse(params)?;
                self.lifecycle_events = params.enabled;
                Ok(json!({}))
            }
            PageHandler::RuntimeEnable => {
                self.default_contexts.insert(tab.id(), tab.id().get());
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
            PageHandler::RuntimeAddBinding => add_binding(params, tab).await,
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

    // https://chromedevtools.github.io/devtools-protocol/tot/Page/#method-navigateToHistoryEntry
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
            // https://chromedevtools.github.io/devtools-protocol/tot/Target/#method-createBrowserContext
            BrowserHandler::CreateBrowserContext => {
                let id = self
                    .contexts
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .create();
                Ok(json!({ "browserContextId": id }))
            }
            // https://chromedevtools.github.io/devtools-protocol/tot/Target/#method-disposeBrowserContext
            BrowserHandler::DisposeBrowserContext => {
                self.dispose_browser_context(params, session).await
            }
            // https://chromedevtools.github.io/devtools-protocol/tot/Target/#method-getBrowserContexts
            BrowserHandler::GetBrowserContexts => {
                let ids = self
                    .contexts
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .list();
                Ok(json!({ "browserContextIds": ids }))
            }
            BrowserHandler::AttachToTarget => {
                let params: params::AttachToTarget = params::parse(params)?;
                let id = target_id(&params.target_id)?;
                if params.flatten != Some(true) {
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

    async fn create_target(
        &mut self,
        params: &Value,
        session: Option<&str>,
    ) -> Result<Value, DispatchError> {
        let params: params::CreateTarget = params::parse(params)?;
        if let Some(context_id) = params.browser_context_id.as_deref()
            && !self
                .contexts
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .contains(context_id)
        {
            return Err(DispatchError::InvalidParams("browserContextId".to_owned()));
        }
        let url = params.url.as_str();
        let tab = self
            .browser
            .create_tab()
            .await
            .map_err(|err| DispatchError::Failed(err.to_string()))?;
        let open_result = if url.is_empty() || url == "about:blank" {
            tab.load_html("<!doctype html><title></title>")
                .await
                .map_err(|err| DispatchError::Failed(err.to_string()))
        } else {
            open_url(&tab, url).await
        };
        if let Err(error) = open_result {
            let _result = self.browser.close_tab(tab.id()).await;
            return Err(error);
        }
        if let Some(context_id) = params.browser_context_id.as_deref() {
            self.contexts
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .assign_tab(tab.id(), context_id);
        }
        if self.auto_attach {
            let session_id = self.mint(Target::Page(tab.clone()));
            self.push_attached(session, &session_id, &tab).await;
        }
        Ok(json!({ "targetId": tab.id().to_string() }))
    }

    async fn dispose_browser_context(
        &mut self,
        params: &Value,
        session: Option<&str>,
    ) -> Result<Value, DispatchError> {
        let params: params::DisposeBrowserContext = params::parse(params)?;
        let Some(tabs) = self
            .contexts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .dispose(&params.browser_context_id)
        else {
            return Err(DispatchError::Failed(format!(
                "unknown browser context: {}",
                params.browser_context_id
            )));
        };
        for id in tabs {
            let _ = self.close_tab_id(id, session).await;
        }
        Ok(json!({}))
    }

    async fn close_target(
        &mut self,
        params: &Value,
        session: Option<&str>,
    ) -> Result<Value, DispatchError> {
        let params: params::CloseTarget = params::parse(params)?;
        let id = target_id(&params.target_id)?;
        self.close_tab_id(id, session).await?;
        Ok(json!({ "success": true }))
    }

    async fn close_tab_id(
        &mut self,
        id: TabId,
        session: Option<&str>,
    ) -> Result<(), DispatchError> {
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
        self.contexts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove_tab(id);
        self.sessions.retain(|_, session| match session {
            Target::Page(tab) => tab.id() != id,
            Target::Browser => true,
        });
        self.unsubscribe_tab_id(id);
        self.loader_ids.remove(&id);
        self.current_loaders.remove(&id);
        self.isolated_worlds.remove(&id);
        self.omit_background.remove(&id);
        if self.tab.as_ref().is_some_and(|tab| tab.id() == id) {
            self.tab = None;
        }
        Ok(())
    }

    async fn target_list(&self) -> Result<Value, DispatchError> {
        let mut target_infos = Vec::new();
        for id in self
            .browser
            .tabs()
            .await
            .map_err(|error| DispatchError::Failed(error.to_string()))?
        {
            let context_id = self.context_of(id);
            target_infos.push(target_info(&self.browser, id, &context_id).await);
        }
        Ok(json!({ "targetInfos": target_infos }))
    }

    async fn navigate_tab(
        &mut self,
        params: &Value,
        tab: &TabHandle,
        request_id: &Value,
        session: Option<&str>,
    ) -> Result<DispatchResult, DispatchError> {
        let params: params::Navigate = params::parse(params)?;
        let url = if params.url.is_empty() {
            "about:blank"
        } else {
            params.url.as_str()
        };
        let frame_id = tab.id().to_string();
        let loader_id = self.next_loader_id();
        // https://chromedevtools.github.io/devtools-protocol/tot/Page/#event-frameNavigated
        self.loader_ids.insert(tab.id(), loader_id.clone());
        let events = tab
            .subscribe()
            .map_err(|error| DispatchError::Failed(error.to_string()))?;
        open_url_with_referrer(tab, url, params.referrer.as_deref()).await?;
        if let Some(tx) = self.deferred_tx.clone() {
            let req_id = request_id.clone();
            let session = session.map(str::to_owned);
            tokio::spawn(async move {
                let result = match wait_for_navigation(events, Duration::from_secs(30)).await {
                    Ok(()) => json!({"frameId": frame_id, "loaderId": loader_id}),
                    Err(error_text) => json!({
                        "frameId": frame_id,
                        "loaderId": loader_id,
                        "errorText": error_text,
                    }),
                };
                let mut reply = json!({"id": req_id, "result": result});
                attach_session(&mut reply, session.as_deref());
                let _ = tx.send(reply);
            });
            return Ok(DispatchResult::Deferred);
        }
        match wait_for_navigation(events, Duration::from_secs(30)).await {
            Ok(()) => Ok(DispatchResult::Immediate(json!({
                "frameId": frame_id,
                "loaderId": loader_id,
            }))),
            Err(error_text) => Ok(DispatchResult::Immediate(json!({
                "frameId": frame_id,
                "loaderId": loader_id,
                "errorText": error_text,
            }))),
        }
    }

    fn set_default_background_color_override(
        &mut self,
        params: &Value,
        tab: &TabHandle,
    ) -> Result<Value, DispatchError> {
        let params: params::DefaultBackgroundColorOverride = params::parse(params)?;
        let omit = params
            .color
            .and_then(|color| color.a)
            .is_some_and(|alpha| alpha == 0.0);
        if omit {
            self.omit_background.insert(tab.id(), true);
        } else {
            self.omit_background.remove(&tab.id());
        }
        Ok(json!({}))
    }

    fn get_response_body(&self, params: &Value) -> Result<Value, DispatchError> {
        use base64::Engine as _;
        let params: params::GetResponseBody = params::parse(params)?;
        let Some(bytes) = self.response_bodies.get(&params.request_id) else {
            return Err(DispatchError::Failed(
                "No resource with given identifier found".into(),
            ));
        };
        Ok(json!({
            "body": base64::engine::general_purpose::STANDARD.encode(bytes),
            "base64Encoded": true,
        }))
    }

    fn push_session_event(&mut self, session: Option<&str>, method: &str, params: &Value) {
        let mut message = json!({"method": method, "params": params});
        attach_session(&mut message, session);
        self.events.push(message);
    }

    async fn dispatch_runtime(
        &mut self,
        source: RuntimeSource,
        options: params::RuntimeOptions,
        tab: &TabHandle,
        request_id: &Value,
        session: Option<&str>,
    ) -> Result<DispatchResult, DispatchError> {
        let frame = requested_frame(tab, options.frame_id.as_deref())?;
        let return_by_value = options.return_by_value.unwrap_or(false);
        let await_promise = options.await_promise.unwrap_or(false);
        let object_group = options.object_group.as_deref().unwrap_or("");
        let await_timeout = match options.timeout.filter(|millis| *millis > 0.0) {
            Some(millis) => Duration::try_from_secs_f64(millis / 1000.0)
                .map_err(|_| DispatchError::InvalidParams("timeout".to_owned()))?,
            None => AWAIT_PROMISE_TIMEOUT,
        };
        let source = match source {
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
        if return_by_value || await_promise {
            let evaluation = self.next_evaluation;
            self.next_evaluation = self.next_evaluation.saturating_add(1);
            let eval_id = json_string(&evaluation.to_string());
            let make = if return_by_value {
                "host.remoteObject(v, { __proto__: null, returnByValue: true })".to_owned()
            } else {
                format!(
                    "{{ __proto__: null, value: host.remoteObject(v, {{ __proto__: null, objectGroup: {} }}) }}",
                    json_string(object_group)
                )
            };
            if let Err(reply) =
                Self::schedule_runtime_slot(tab, frame, &source, &make, &eval_id).await
            {
                return Ok(DispatchResult::Immediate(reply));
            }
            match Self::try_read_runtime_slot(tab, frame, &eval_id).await {
                Ok(Some(parsed)) => {
                    return Ok(DispatchResult::Immediate(format_runtime_result(
                        &parsed,
                        return_by_value,
                    )));
                }
                Err(reply) => return Ok(DispatchResult::Immediate(reply)),
                Ok(None) => {}
            }
            if let Some(tx) = self.deferred_tx.clone() {
                let tab = tab.clone();
                let req_id = request_id.clone();
                let session = session.map(str::to_owned);
                tokio::spawn(async move {
                    let result = match Self::wait_and_read_runtime_slot(
                        &tab,
                        frame,
                        await_timeout,
                        &eval_id,
                    )
                    .await
                    {
                        Ok(parsed) => format_runtime_result(&parsed, return_by_value),
                        Err(reply) => reply,
                    };
                    let mut reply = json!({"id": req_id, "result": result});
                    attach_session(&mut reply, session.as_deref());
                    let _ = tx.send(reply);
                });
                return Ok(DispatchResult::Deferred);
            }
            let result =
                match Self::wait_and_read_runtime_slot(tab, frame, await_timeout, &eval_id).await {
                    Ok(parsed) => format_runtime_result(&parsed, return_by_value),
                    Err(reply) => reply,
                };
            return Ok(DispatchResult::Immediate(result));
        }
        Ok(DispatchResult::Immediate(
            self.runtime_handle(tab, frame, &source, object_group).await,
        ))
    }

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

    async fn schedule_runtime_slot(
        tab: &TabHandle,
        frame: FrameId,
        source: &str,
        make: &str,
        id: &str,
    ) -> Result<(), Value> {
        let schedule = RUNTIME_SCHEDULE
            .replace("__ID__", id)
            .replace("__MAKE__", make)
            .replace("__SOURCE__", source);
        tab.execute_script(ExecuteScriptOptions {
            frame,
            source: browser::ScriptSource::Browser(&schedule),
            timeout: None,
        })
        .await
        .map(|_| ())
        .map_err(|error| exception_reply(&error))
    }

    async fn try_read_runtime_slot(
        tab: &TabHandle,
        frame: FrameId,
        id: &str,
    ) -> Result<Option<Value>, Value> {
        let read = RUNTIME_READ.replace("__ID__", id);
        let value = tab
            .execute_script(ExecuteScriptOptions {
                frame,
                source: browser::ScriptSource::Browser(&read),
                timeout: None,
            })
            .await
            .map_err(|error| exception_reply(&error))?;
        if let RemoteValue::String(text) = value
            && let Ok(parsed) = serde_json::from_str::<Value>(&text)
        {
            if parsed.get("pending").and_then(Value::as_bool) == Some(true) {
                return Ok(None);
            }
            if let Some(error) = parsed.get("error").and_then(Value::as_str) {
                return Err(exception_text_reply(error));
            }
            return Ok(Some(parsed));
        }
        Err(exception_text_reply("unexpected script result"))
    }

    async fn wait_and_read_runtime_slot(
        tab: &TabHandle,
        frame: FrameId,
        timeout: Duration,
        id: &str,
    ) -> Result<Value, Value> {
        let ready = format!("Boolean(host.__tbHasEvaluation({id}))");
        // https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#method-callFunctionOn
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
                let _ = tab
                    .execute_script(ExecuteScriptOptions {
                        frame,
                        source: browser::ScriptSource::Browser(&format!(
                            "(() => {{ host.__tbTakeEvaluation({id}); }})()"
                        )),
                        timeout: None,
                    })
                    .await;
                return Err(exception_text_reply("awaitPromise timed out"));
            }
            Err(error) => return Err(exception_reply(&error)),
        }
        match Self::try_read_runtime_slot(tab, frame, id).await? {
            Some(parsed) => Ok(parsed),
            None => Err(exception_text_reply("unexpected script result")),
        }
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
    let (deferred_tx, mut deferred_rx) = mpsc::unbounded_channel();
    let mut conn = Conn {
        browser: state.browser,
        contexts: state.contexts,
        sessions: HashMap::new(),
        next_session: 1,
        tab,
        stop: state.stop,
        subscriptions: Vec::new(),
        tab_events_tx,
        tab_events_rx,
        deferred_tx: Some(deferred_tx),
        clock_origin: Instant::now(),
        auto_attach: false,
        events: Vec::new(),
        loader_ids: HashMap::new(),
        current_loaders: HashMap::new(),
        default_contexts: HashMap::new(),
        lifecycle_events: false,
        isolated_worlds: HashMap::new(),
        omit_background: HashMap::new(),
        response_bodies: HashMap::new(),
        response_body_order: VecDeque::new(),
        next_loader: 1,
        next_context: 1000,
        next_evaluation: 1,
    };
    loop {
        let wake = tokio::select! {
            event = conn.tab_events_rx.recv() => SocketWake::Event(event),
            reply = deferred_rx.recv() => SocketWake::Deferred(reply),
            incoming = socket.next() => SocketWake::Incoming(incoming),
        };
        match wake {
            SocketWake::Event(Some(event)) => {
                let messages = conn.event_messages(event).await;
                if send_messages(&mut socket, messages).await.is_err() {
                    break;
                }
            }
            SocketWake::Deferred(Some(reply)) => {
                if socket
                    .send(WsMessage::text(reply.to_string()))
                    .await
                    .is_err()
                {
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
                if !outcome.deferred
                    && socket
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
            SocketWake::Deferred(None) | SocketWake::Incoming(Some(Ok(_))) => {}
        }
    }
    conn.stop_subscriptions();
    let _result = socket.send(WsMessage::Close(None)).await;
}

struct NavigationResponse {
    status: u16,
    status_text: String,
    url: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

enum SocketWake {
    Event(Option<ConnEvent>),
    Deferred(Option<Value>),
    Incoming(Option<Result<WsMessage, tungstenite::Error>>),
}

fn push_event(messages: &mut Vec<Value>, method: &str, params: &Value, session: Option<&str>) {
    let mut message = json!({"method": method, "params": params});
    attach_session(&mut message, session);
    messages.push(message);
}

fn format_runtime_result(parsed: &Value, return_by_value: bool) -> Value {
    if return_by_value {
        json!({"result": parsed})
    } else {
        json!({"result": parsed.get("value").cloned().unwrap_or(Value::Null)})
    }
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
            contexts: Arc::new(Mutex::new(BrowserContexts::default())),
            sessions: HashMap::new(),
            next_session: 1,
            tab: None,
            stop: Shutdown::new(),
            subscriptions: Vec::new(),
            tab_events_tx,
            tab_events_rx,
            deferred_tx: None,
            clock_origin: Instant::now(),
            auto_attach: false,
            events: Vec::new(),
            loader_ids: HashMap::new(),
            current_loaders: HashMap::new(),
            default_contexts: HashMap::new(),
            lifecycle_events: false,
            isolated_worlds: HashMap::new(),
            omit_background: HashMap::new(),
            response_bodies: HashMap::new(),
            response_body_order: VecDeque::new(),
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

        let cache_disabled = json!({
            "id": 3,
            "method": "Network.setCacheDisabled",
            "params": {"cacheDisabled": true},
            "sessionId": session,
        });
        let outcome = conn.dispatch_text(&cache_disabled.to_string()).await;
        assert_eq!(outcome.reply["result"], json!({}));

        let cache_enabled = json!({
            "id": 4,
            "method": "Network.setCacheDisabled",
            "params": {"cacheDisabled": false},
            "sessionId": session,
        });
        let outcome = conn.dispatch_text(&cache_enabled.to_string()).await;
        assert_eq!(outcome.reply["result"], json!({}));

        let clear_cache = json!({
            "id": 5,
            "method": "Network.clearBrowserCache",
            "params": {},
            "sessionId": session,
        });
        let outcome = conn.dispatch_text(&clear_cache.to_string()).await;
        assert_eq!(outcome.reply["result"], json!({}));

        let get_properties = json!({
            "id": 6,
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
