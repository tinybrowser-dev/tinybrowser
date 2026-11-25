use std::collections::{HashMap, HashSet, VecDeque};
use std::io;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use browser::{
    BrowserHandle, BrowserLifecycleEvent, ConsoleLevel, ExecuteScriptOptions, FrameId,
    InterceptDecision, InterceptId, InterceptPhase, NetworkEvent, NetworkRequestId, PromptBehavior,
    PromptBehaviorMap, PromptEvent, RemoteValue, RunUntilJsTrueOptions, TabEvent, TabHandle, TabId,
    UrlPattern, UserContextId,
};
use futures_util::{SinkExt as _, StreamExt as _};
use http::{Method, StatusCode};
use serde_json::{Value, json};
use server::ws::{WebSocket, WsError, WsMessage};
use server::{Request, Response, Shutdown};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tungstenite::client::IntoClientRequest;
use tungstenite::protocol::{Message, WebSocket as ClientSocket};

mod dispatch;
mod params;
use dispatch::{
    BrowserHandler, DispatchError, Handler, PageHandler, RUNTIME_CLEAR_HANDLES, RUNTIME_HANDLE,
    RUNTIME_READ, RUNTIME_SCHEDULE, Target, accept, add_binding, arguments_expression,
    attach_session, capture_screenshot, cdp_cookie_json, cdp_frame_id, css_stylesheets,
    delete_cdp_cookies, exception_reply, exception_text_reply, json_io, json_string,
    missing_handles_expression, open_url, open_url_with_referrer, requested_frame, route,
    run_script, runtime_release_object, runtime_release_object_group, set_cdp_cookies, target_id,
    target_info, url_security_origin, wait_for_navigation, ws_io,
};

const DEFAULT_BROWSER_CONTEXT_ID: &str = "tinybrowser-default";
// https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#method-evaluate
const AWAIT_PROMISE_TIMEOUT: Duration = Duration::from_secs(30);

fn cdp_context_id(uc: &UserContextId) -> String {
    if uc.is_default() {
        DEFAULT_BROWSER_CONTEXT_ID.to_owned()
    } else {
        uc.as_str().to_owned()
    }
}

fn user_context_from_cdp(id: &str) -> UserContextId {
    if id == DEFAULT_BROWSER_CONTEXT_ID {
        UserContextId::default_context()
    } else {
        UserContextId::new(id)
    }
}

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
    let tab = if let Some(raw) = raw {
        let Ok(id) = raw.parse::<u64>() else {
            return status_response(StatusCode::BAD_REQUEST, "invalid page target");
        };
        let Ok(tab) = state.browser.tab(TabId::new(id)).await else {
            return status_response(StatusCode::NOT_FOUND, "unknown page target");
        };
        Some(tab)
    } else {
        None
    };
    server::ws::upgrade(request, move |socket| run_socket(socket, state, tab))
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
    auto_attach_session: Option<String>,
    auto_attached_tabs: HashSet<TabId>,
    discover_targets: bool,
    network_enabled: HashSet<TabId>,
    prompt_tabs: HashSet<TabId>,
    fetch_intercepts: HashMap<TabId, Vec<InterceptId>>,
    compiled_scripts: HashMap<String, String>,
    next_script: u64,
    events: Vec<Value>,
    loader_ids: HashMap<TabId, String>,
    // https://chromedevtools.github.io/devtools-protocol/tot/Page/#event-lifecycleEvent
    current_loaders: HashMap<TabId, String>,
    // https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#event-consoleAPICalled
    default_contexts: HashMap<TabId, u64>,
    // https://chromedevtools.github.io/devtools-protocol/tot/Page/#method-setLifecycleEventsEnabled
    lifecycle_events: bool,
    isolated_worlds: HashMap<TabId, Vec<String>>,
    isolated_contexts: HashMap<u64, (TabId, String)>,
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
                        arguments: request.arguments.unwrap_or_default(),
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
        arguments: Vec<params::CallArgument>,
    },
}

// https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#method-callFunctionOn
const MISSING_HANDLE: &str = "Could not find object with given id";

struct RuntimeTarget<'a> {
    tab: &'a TabHandle,
    frame: FrameId,
    isolated_world: Option<&'a str>,
    allow_unsafe_eval: bool,
}

impl RuntimeTarget<'_> {
    async fn execute(&self, script: &str) -> Result<RemoteValue, Value> {
        self.tab
            .execute_script(ExecuteScriptOptions {
                frame: self.frame,
                source: self.script_source(script),
                timeout: None,
            })
            .await
            .map_err(|error| exception_reply(&error))
    }

    fn script_source<'a>(&'a self, script: &'a str) -> browser::ScriptSource<&'a str> {
        match self.isolated_world {
            Some(world) => browser::ScriptSource::Isolated {
                world,
                source: script,
            },
            None if !self.allow_unsafe_eval => browser::ScriptSource::BrowserStrict(script),
            None => browser::ScriptSource::Browser(script),
        }
    }
}

struct RuntimeScript {
    source: String,
    missing: String,
}

enum RuntimeSchedule {
    Evaluation,
    MissingHandle,
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
                self.push_lifecycle_pair(
                    &mut messages,
                    tab_id,
                    "load",
                    "Page.loadEventFired",
                    timestamp,
                    sess,
                );
            }
            TabEvent::DomContentLoaded => {
                self.push_lifecycle_pair(
                    &mut messages,
                    tab_id,
                    "DOMContentLoaded",
                    "Page.domContentEventFired",
                    timestamp,
                    sess,
                );
            }
            TabEvent::NavigationFailed(_)
            | TabEvent::FrameDomContentLoaded { .. }
            | TabEvent::FrameLoad { .. } => {}
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
                let url = tab.document_url().await.unwrap_or_default();
                self.push_execution_contexts(&mut messages, tab_id, &frame_id, &url, sess);
            }
            TabEvent::Binding { name, payload } => {
                self.push_binding_called(&mut messages, tab_id, &name, &payload, sess);
            }
            TabEvent::Console { level, args } => {
                // https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#event-consoleAPICalled
                self.push_console_called(&mut messages, tab_id, level, &args, timestamp, sess);
            }
            TabEvent::FrameAttached { frame, parent, .. } => {
                push_frame_attached(&mut messages, tab_id, frame, parent, sess);
            }
            TabEvent::FrameNavigated { frame, url } => {
                push_frame_navigated(&mut messages, tab_id, frame, &url, sess);
            }
            TabEvent::FrameDetached { frame } => {
                push_frame_detached(&mut messages, tab_id, frame, sess);
            }
            event @ TabEvent::ExceptionThrown { .. } => {
                self.push_exception_event(&mut messages, tab_id, &event, timestamp, sess);
            }
        }
        messages
    }

    fn push_lifecycle_pair(
        &self,
        messages: &mut Vec<Value>,
        tab_id: TabId,
        lifecycle_name: &str,
        page_event: &str,
        timestamp: f64,
        sess: Option<&str>,
    ) {
        self.push_lifecycle_event(messages, tab_id, lifecycle_name, timestamp, sess);
        push_event(messages, page_event, &json!({"timestamp": timestamp}), sess);
    }

    // https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#event-bindingCalled
    fn push_binding_called(
        &self,
        messages: &mut Vec<Value>,
        tab_id: TabId,
        name: &str,
        payload: &str,
        sess: Option<&str>,
    ) {
        let context = self
            .default_contexts
            .get(&tab_id)
            .copied()
            .unwrap_or_default();
        push_event(
            messages,
            "Runtime.bindingCalled",
            &json!({"name": name, "payload": payload, "executionContextId": context}),
            sess,
        );
    }

    fn push_exception_event(
        &self,
        messages: &mut Vec<Value>,
        tab_id: TabId,
        event: &TabEvent,
        timestamp: f64,
        sess: Option<&str>,
    ) {
        let TabEvent::ExceptionThrown {
            text,
            filename,
            line,
            column,
            ..
        } = event
        else {
            return;
        };
        let context = self
            .default_contexts
            .get(&tab_id)
            .copied()
            .unwrap_or_default();
        push_event(
            messages,
            "Runtime.exceptionThrown",
            &json!({
                "timestamp": timestamp,
                "exceptionDetails": {
                    "exceptionId": 1,
                    "text": text,
                    "lineNumber": line,
                    "columnNumber": column,
                    "url": filename,
                    "executionContextId": context,
                },
            }),
            sess,
        );
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
                "securityOrigin": url_security_origin(&url),
                "mimeType": "text/html",
            }},
        });
        attach_session(&mut navigated, session);
        messages.push(navigated);
        self.push_execution_contexts(messages, tab_id, &frame_id, &url, session);
        let _ = tab.execute_browser_script(RUNTIME_CLEAR_HANDLES).await;
    }

    // https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#event-executionContextCreated
    fn push_execution_contexts(
        &mut self,
        messages: &mut Vec<Value>,
        tab_id: TabId,
        frame_id: &str,
        url: &str,
        session: Option<&str>,
    ) {
        let origin = url_security_origin(url);
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
        self.isolated_contexts
            .retain(|_, (owner, _)| *owner != tab_id);
        for world in worlds {
            let context_id = self.next_context;
            self.next_context = self.next_context.saturating_add(1);
            let default = world.is_empty();
            if default {
                self.default_contexts.insert(tab_id, context_id);
            } else {
                self.isolated_contexts
                    .insert(context_id, (tab_id, world.clone()));
            }
            let mut created = json!({
                "method": "Runtime.executionContextCreated",
                "params": {"context": {
                    "id": context_id,
                    "origin": origin,
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

    async fn context_of(&self, id: TabId) -> String {
        match self.browser.tab(id).await {
            Ok(tab) => cdp_context_id(tab.user_context()),
            Err(_) => DEFAULT_BROWSER_CONTEXT_ID.to_owned(),
        }
    }

    async fn resolve_user_context(
        &self,
        raw: Option<&str>,
        fallback: &UserContextId,
    ) -> Result<UserContextId, DispatchError> {
        let Some(id) = raw else {
            return Ok(fallback.clone());
        };
        let uc = user_context_from_cdp(id);
        let known = self
            .browser
            .user_contexts()
            .await
            .map_err(|err| DispatchError::Failed(err.to_string()))?;
        if !known.contains(&uc) {
            return Err(DispatchError::InvalidParams("browserContextId".to_owned()));
        }
        Ok(uc)
    }

    fn sessions_for_tab(&self, tab_id: TabId) -> Vec<Option<String>> {
        let mut out: Vec<Option<String>> = self
            .sessions
            .iter()
            .filter_map(|(sid, target)| match target {
                Target::Page(tab) if tab.id() == tab_id => Some(Some(sid.clone())),
                _ => None,
            })
            .collect();
        if self.tab.as_ref().is_some_and(|t| t.id() == tab_id) {
            out.push(None);
        }
        out
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
                let context_id = self.context_of(id).await;
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
        self.auto_attach_session = session.map(str::to_owned);
        if auto_attach {
            for id in self.browser.tabs().await.unwrap_or_default() {
                if self.auto_attached_tabs.contains(&id) {
                    continue;
                }
                if let Ok(tab) = self.browser.tab(id).await {
                    let session_id = self.mint(Target::Page(tab.clone()));
                    self.auto_attached_tabs.insert(id);
                    self.push_attached(session, &session_id, &tab).await;
                }
            }
        }
        Ok(json!({}))
    }

    // https://chromedevtools.github.io/devtools-protocol/tot/Target/#method-setDiscoverTargets
    async fn set_discover_targets(
        &mut self,
        params: &Value,
        session: Option<&str>,
    ) -> Result<Value, DispatchError> {
        let params: params::SetDiscoverTargets = params::parse(params)?;
        self.discover_targets = params.discover;
        if params.discover {
            for id in self.browser.tabs().await.unwrap_or_default() {
                let context_id = self.context_of(id).await;
                let info = target_info(&self.browser, id, &context_id).await;
                self.push_session_event(
                    session,
                    "Target.targetCreated",
                    &json!({ "targetInfo": info }),
                );
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
        let context_id = cdp_context_id(tab.user_context());
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
        let target_uc = target_user_context(target);
        match handler {
            Handler::Accept(reason) => accept(reason, params).map(DispatchResult::Immediate),
            Handler::ClearBrowserCookies => {
                self.browser
                    .clear_cookies_in(&target_uc)
                    .await
                    .map_err(|error| DispatchError::Failed(error.to_string()))?;
                Ok(DispatchResult::Immediate(json!({})))
            }
            Handler::ClearBrowserCache => {
                self.browser
                    .clear_cache_in(&target_uc)
                    .await
                    .map_err(|error| DispatchError::Failed(error.to_string()))?;
                Ok(DispatchResult::Immediate(json!({})))
            }
            Handler::SetCacheDisabled => {
                let params: params::SetCacheDisabled = params::parse(params)?;
                self.browser
                    .set_cache_disabled_in(&target_uc, params.cache_disabled)
                    .await
                    .map_err(|error| DispatchError::Failed(error.to_string()))?;
                Ok(DispatchResult::Immediate(json!({})))
            }
            Handler::StorageGetCookies
            | Handler::StorageSetCookies
            | Handler::StorageClearCookies => self
                .dispatch_storage_cookies(handler, params, &target_uc)
                .await
                .map(DispatchResult::Immediate),
            Handler::GetTargetInfo => match target {
                Target::Page(tab) => {
                    let context_id = cdp_context_id(tab.user_context());
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

    async fn dispatch_storage_cookies(
        &self,
        handler: Handler,
        params: &Value,
        fallback: &UserContextId,
    ) -> Result<Value, DispatchError> {
        match handler {
            Handler::StorageGetCookies => {
                let params: params::StorageBrowserContext = params::parse(params)?;
                let uc = self
                    .resolve_user_context(params.browser_context_id.as_deref(), fallback)
                    .await?;
                let records = self
                    .browser
                    .all_cookie_records_in(&uc)
                    .await
                    .map_err(|error| DispatchError::Failed(error.to_string()))?;
                let cookies: Vec<Value> = records.iter().map(cdp_cookie_json).collect();
                Ok(json!({ "cookies": cookies }))
            }
            Handler::StorageSetCookies => {
                let params: params::SetCookies = params::parse(params)?;
                let uc = self
                    .resolve_user_context(params.browser_context_id.as_deref(), fallback)
                    .await?;
                set_cdp_cookies(&self.browser, &uc, &params.cookies, None).await?;
                Ok(json!({}))
            }
            Handler::StorageClearCookies => {
                let params: params::StorageBrowserContext = params::parse(params)?;
                let uc = self
                    .resolve_user_context(params.browser_context_id.as_deref(), fallback)
                    .await?;
                self.browser
                    .clear_cookies_in(&uc)
                    .await
                    .map_err(|error| DispatchError::Failed(error.to_string()))?;
                Ok(json!({}))
            }
            _ => Err(DispatchError::MethodNotFound),
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
            PageHandler::RuntimeRunScript => {
                let req: params::RunScript = params::parse(params)?;
                let Some(expression) = self.compiled_scripts.get(&req.script_id).cloned() else {
                    return Err(DispatchError::Failed("unknown scriptId".to_owned()));
                };
                self.dispatch_runtime(
                    RuntimeSource::Evaluate { expression },
                    req.options,
                    tab,
                    request_id,
                    session,
                )
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
            | PageHandler::RuntimeRunScript
            | PageHandler::Navigate => Err(DispatchError::MethodNotFound),
            PageHandler::Enable => {
                self.subscribe_tab(tab, session)?;
                self.prompt_tabs.insert(tab.id());
                self.browser.prompt_broker().set_tab_behavior(
                    tab.id(),
                    Some(PromptBehaviorMap::uniform(PromptBehavior::Ignore)),
                );
                Ok(json!({}))
            }
            PageHandler::Disable => {
                self.unsubscribe_tab(tab.id(), session);
                self.prompt_tabs.remove(&tab.id());
                self.browser
                    .prompt_broker()
                    .set_tab_behavior(tab.id(), None);
                Ok(json!({}))
            }
            PageHandler::NavigateToHistoryEntry => {
                self.navigate_to_history_entry(params, tab).await
            }
            PageHandler::SetDocumentContent => set_document_content(tab, params).await,
            PageHandler::HandleJavaScriptDialog => self.handle_javascript_dialog(tab, params),
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
            PageHandler::NetworkEnable => {
                self.network_enabled.insert(tab.id());
                Ok(json!({}))
            }
            PageHandler::NetworkDisable => {
                self.network_enabled.remove(&tab.id());
                Ok(json!({}))
            }
            PageHandler::NetworkGetResponseBody => self.get_response_body(params),
            PageHandler::NetworkSetExtraHttpHeaders => set_extra_http_headers(tab, params).await,
            PageHandler::NetworkGetCookies => self.network_get_cookies(tab, params).await,
            PageHandler::NetworkGetAllCookies => self.network_get_all_cookies(tab).await,
            PageHandler::NetworkSetCookie => self.network_set_cookie(tab, params).await,
            PageHandler::NetworkSetCookies => self.network_set_cookies(tab, params).await,
            PageHandler::NetworkDeleteCookies => {
                let params: params::DeleteCookies = params::parse(params)?;
                delete_cdp_cookies(&self.browser, tab.user_context(), &params).await?;
                Ok(json!({}))
            }
            PageHandler::NetworkSetBlockedUrls => {
                let params: params::SetBlockedUrls = params::parse(params)?;
                self.browser
                    .network_broker()
                    .set_blocked_urls(tab.id(), params.urls);
                Ok(json!({}))
            }
            PageHandler::FetchEnable => self.fetch_enable(tab, params),
            PageHandler::FetchDisable => {
                self.clear_fetch_intercepts(tab.id());
                Ok(json!({}))
            }
            PageHandler::FetchContinueRequest => self.fetch_continue_request(params),
            PageHandler::FetchFulfillRequest => self.fetch_fulfill_request(params),
            PageHandler::FetchFailRequest => self.fetch_fail_request(params),
            PageHandler::RuntimeCompileScript => self.runtime_compile_script(tab, params).await,
            PageHandler::CreateIsolatedWorld => self.create_isolated_world(params, tab, session),
            PageHandler::SetLifecycleEventsEnabled => {
                let params: params::SetLifecycleEventsEnabled = params::parse(params)?;
                self.lifecycle_events = params.enabled;
                Ok(json!({}))
            }
            PageHandler::RuntimeEnable => self.runtime_enable(tab, session).await,
            PageHandler::RuntimeDisable | PageHandler::CssDisable => Ok(json!({})),
            PageHandler::RuntimeAddBinding => add_binding(params, tab).await,
            PageHandler::RuntimeReleaseObject => runtime_release_object(tab, params).await,
            PageHandler::RuntimeReleaseObjectGroup => {
                runtime_release_object_group(tab, params).await
            }
            PageHandler::CssEnable => self.css_enable(tab, session).await,
            PageHandler::TracingEnd => Ok(self.tracing_end(session)),
            PageHandler::Script(method) => {
                let world = params
                    .get("executionContextId")
                    .and_then(Value::as_u64)
                    .and_then(|id| self.isolated_contexts.get(&id))
                    .filter(|(owner, _)| *owner == tab.id())
                    .map(|(_, name)| name.clone());
                run_script(method, params, tab, world.as_deref()).await
            }
        }
    }

    fn handle_javascript_dialog(
        &self,
        tab: &TabHandle,
        params: &Value,
    ) -> Result<Value, DispatchError> {
        let params: params::HandleJavaScriptDialog = params::parse(params)?;
        if !self
            .browser
            .prompt_broker()
            .handle_prompt(tab.id(), params.accept, params.prompt_text)
        {
            return Err(DispatchError::Failed("No dialog is showing".to_owned()));
        }
        Ok(json!({}))
    }

    async fn network_get_cookies(
        &self,
        tab: &TabHandle,
        params: &Value,
    ) -> Result<Value, DispatchError> {
        let params: params::GetCookies = params::parse(params)?;
        let urls: Vec<String> = match params.urls {
            Some(list) => list,
            None => vec![tab.document_url().await.unwrap_or_default()],
        };
        let mut seen = HashSet::new();
        let mut cookies = Vec::new();
        for raw in urls {
            let Ok(parsed) = url::Url::parse(&raw) else {
                continue;
            };
            if let Ok(records) = self
                .browser
                .cookie_records_in(tab.user_context(), &parsed)
                .await
            {
                for rec in records {
                    let key = (rec.name.clone(), rec.domain.clone(), rec.path.clone());
                    if seen.insert(key) {
                        cookies.push(cdp_cookie_json(&rec));
                    }
                }
            }
        }
        Ok(json!({ "cookies": cookies }))
    }

    async fn network_get_all_cookies(&self, tab: &TabHandle) -> Result<Value, DispatchError> {
        let records = self
            .browser
            .all_cookie_records_in(tab.user_context())
            .await
            .map_err(|err| DispatchError::Failed(err.to_string()))?;
        let cookies: Vec<Value> = records.iter().map(cdp_cookie_json).collect();
        Ok(json!({ "cookies": cookies }))
    }

    async fn network_set_cookie(
        &self,
        tab: &TabHandle,
        params: &Value,
    ) -> Result<Value, DispatchError> {
        let param: params::CookieParam = params::parse(params)?;
        let current_url = tab.document_url().await.ok();
        let success = set_cdp_cookies(
            &self.browser,
            tab.user_context(),
            &[param],
            current_url.as_deref(),
        )
        .await?;
        Ok(json!({ "success": success }))
    }

    async fn network_set_cookies(
        &self,
        tab: &TabHandle,
        params: &Value,
    ) -> Result<Value, DispatchError> {
        let params: params::SetCookies = params::parse(params)?;
        let current_url = tab.document_url().await.ok();
        set_cdp_cookies(
            &self.browser,
            tab.user_context(),
            &params.cookies,
            current_url.as_deref(),
        )
        .await?;
        Ok(json!({}))
    }

    async fn runtime_enable(
        &mut self,
        tab: &TabHandle,
        session: Option<&str>,
    ) -> Result<Value, DispatchError> {
        self.default_contexts.insert(tab.id(), tab.id().get());
        let url = tab.document_url().await.unwrap_or_default();
        self.push_session_event(
            session,
            "Runtime.executionContextCreated",
            &json!({"context": {
                "id": tab.id().get(),
                "origin": url_security_origin(&url),
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

    async fn css_enable(
        &mut self,
        tab: &TabHandle,
        session: Option<&str>,
    ) -> Result<Value, DispatchError> {
        for mut header in css_stylesheets(tab).await? {
            if let Some(object) = header.as_object_mut() {
                object.insert("frameId".into(), json!(tab.id().to_string()));
            }
            self.push_session_event(session, "CSS.styleSheetAdded", &json!({"header": header}));
        }
        Ok(json!({}))
    }

    fn tracing_end(&mut self, session: Option<&str>) -> Value {
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
        json!({})
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
            self.isolated_contexts
                .insert(context_id, (tab.id(), name.clone()));
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
            BrowserHandler::SetDiscoverTargets => self.set_discover_targets(params, session).await,
            BrowserHandler::GetTargets => self.target_list().await,
            BrowserHandler::CreateTarget => self.create_target(params, session).await,
            BrowserHandler::CloseTarget => self.close_target(params, session).await,
            // https://chromedevtools.github.io/devtools-protocol/tot/Target/#method-createBrowserContext
            BrowserHandler::CreateBrowserContext => {
                let uc = self
                    .browser
                    .create_user_context()
                    .await
                    .map_err(|err| DispatchError::Failed(err.to_string()))?;
                Ok(json!({ "browserContextId": uc.as_str() }))
            }
            // https://chromedevtools.github.io/devtools-protocol/tot/Target/#method-disposeBrowserContext
            BrowserHandler::DisposeBrowserContext => {
                self.dispose_browser_context(params, session).await
            }
            // https://chromedevtools.github.io/devtools-protocol/tot/Target/#method-getBrowserContexts
            BrowserHandler::GetBrowserContexts => {
                let mut ids: Vec<String> = self
                    .browser
                    .user_contexts()
                    .await
                    .map_err(|err| DispatchError::Failed(err.to_string()))?
                    .into_iter()
                    .filter(|uc| !uc.is_default())
                    .map(|uc| uc.as_str().to_owned())
                    .collect();
                ids.sort();
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
        let uc = self
            .resolve_user_context(
                params.browser_context_id.as_deref(),
                &UserContextId::default_context(),
            )
            .await?;
        let url = params.url.as_str();
        let tab = self
            .browser
            .create_tab_in(uc)
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
        if self.auto_attach {
            let session_id = self.mint(Target::Page(tab.clone()));
            self.auto_attached_tabs.insert(tab.id());
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
        let uc = user_context_from_cdp(&params.browser_context_id);
        if uc.is_default() {
            return Err(DispatchError::Failed(
                "cannot dispose default browser context".to_owned(),
            ));
        }
        let known = self
            .browser
            .user_contexts()
            .await
            .map_err(|err| DispatchError::Failed(err.to_string()))?;
        if !known.contains(&uc) {
            return Err(DispatchError::Failed(format!(
                "unknown browser context: {}",
                params.browser_context_id
            )));
        }
        let tabs = self.browser.tabs_in(&uc).await.unwrap_or_default();
        for id in tabs {
            let _ = self.close_tab_id(id, session).await;
        }
        self.browser
            .remove_user_context(&uc)
            .await
            .map_err(|err| DispatchError::Failed(err.to_string()))?;
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
        self.cleanup_tab_state(id);
        Ok(())
    }

    fn cleanup_tab_state(&mut self, id: TabId) {
        self.sessions.retain(|_, session| match session {
            Target::Page(tab) => tab.id() != id,
            Target::Browser => true,
        });
        self.unsubscribe_tab_id(id);
        self.auto_attached_tabs.remove(&id);
        self.network_enabled.remove(&id);
        if self.prompt_tabs.remove(&id) {
            self.browser.prompt_broker().set_tab_behavior(id, None);
        }
        self.clear_fetch_intercepts(id);
        self.loader_ids.remove(&id);
        self.current_loaders.remove(&id);
        self.isolated_worlds.remove(&id);
        self.isolated_contexts.retain(|_, (owner, _)| *owner != id);
        self.omit_background.remove(&id);
        if self.tab.as_ref().is_some_and(|tab| tab.id() == id) {
            self.tab = None;
        }
    }

    async fn target_list(&self) -> Result<Value, DispatchError> {
        let mut target_infos = Vec::new();
        for id in self
            .browser
            .tabs()
            .await
            .map_err(|error| DispatchError::Failed(error.to_string()))?
        {
            let context_id = self.context_of(id).await;
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
        let params: params::GetResponseBody = params::parse(params)?;
        if let Some(bytes) = self.response_bodies.get(&params.request_id) {
            return Ok(json!({
                "body": BASE64_STANDARD.encode(bytes),
                "base64Encoded": true,
            }));
        }
        let raw_num = params
            .request_id
            .strip_prefix("net-")
            .unwrap_or(&params.request_id);
        if let Ok(num) = raw_num.parse::<u64>()
            && let Some(bytes) = self
                .browser
                .network_broker()
                .response_body(NetworkRequestId::new(num))
        {
            return Ok(json!({
                "body": BASE64_STANDARD.encode(&bytes),
                "base64Encoded": true,
            }));
        }
        Err(DispatchError::Failed(
            "No resource with given identifier found".into(),
        ))
    }

    fn clear_fetch_intercepts(&mut self, tab_id: TabId) {
        if let Some(ids) = self.fetch_intercepts.remove(&tab_id) {
            let broker = self.browser.network_broker();
            for id in ids {
                broker.remove_intercept(id);
            }
        }
    }

    fn clear_all_fetch_intercepts(&mut self) {
        let all: Vec<TabId> = self.fetch_intercepts.keys().copied().collect();
        for tab_id in all {
            self.clear_fetch_intercepts(tab_id);
        }
        let prompt_tabs: Vec<TabId> = self.prompt_tabs.drain().collect();
        let prompt_broker = self.browser.prompt_broker();
        for tab_id in prompt_tabs {
            prompt_broker.set_tab_behavior(tab_id, None);
        }
    }

    // https://chromedevtools.github.io/devtools-protocol/tot/Fetch/#method-enable
    fn fetch_enable(&mut self, tab: &TabHandle, params: &Value) -> Result<Value, DispatchError> {
        let params: params::FetchEnable = params::parse(params)?;
        self.clear_fetch_intercepts(tab.id());
        let patterns = params.patterns.unwrap_or_default();
        let broker = self.browser.network_broker();
        let mut ids = Vec::new();
        if patterns.is_empty() {
            let id = broker.add_intercept(
                vec![InterceptPhase::BeforeRequestSent],
                Some(vec![tab.id()]),
                vec![UrlPattern::Wildcard("*".to_owned())],
            );
            ids.push(id);
        } else {
            for pat in patterns {
                let phase = match pat.request_stage.as_deref() {
                    Some("Response") => InterceptPhase::ResponseStarted,
                    _ => InterceptPhase::BeforeRequestSent,
                };
                let url_pat =
                    UrlPattern::Wildcard(pat.url_pattern.unwrap_or_else(|| "*".to_owned()));
                let id = broker.add_intercept(vec![phase], Some(vec![tab.id()]), vec![url_pat]);
                ids.push(id);
            }
        }
        self.fetch_intercepts.insert(tab.id(), ids);
        Ok(json!({}))
    }

    fn parse_fetch_request_id(raw: &str) -> Result<NetworkRequestId, DispatchError> {
        let stripped = raw.strip_prefix("net-").unwrap_or(raw);
        let num = stripped
            .parse::<u64>()
            .map_err(|_| DispatchError::InvalidParams("requestId".to_owned()))?;
        Ok(NetworkRequestId::new(num))
    }

    // https://chromedevtools.github.io/devtools-protocol/tot/Fetch/#method-continueRequest
    fn fetch_continue_request(&self, params: &Value) -> Result<Value, DispatchError> {
        let params: params::FetchContinueRequest = params::parse(params)?;
        let req_id = Self::parse_fetch_request_id(&params.request_id)?;
        let headers = params
            .headers
            .map(|list| list.into_iter().map(|h| (h.name, h.value)).collect());
        let body = params
            .post_data
            .map(|b64| {
                BASE64_STANDARD
                    .decode(b64)
                    .map_err(|_| DispatchError::InvalidParams("postData".to_owned()))
            })
            .transpose()?;
        self.browser
            .network_broker()
            .resolve_intercept(
                req_id,
                InterceptDecision::ContinueRequest {
                    url: params.url,
                    method: params.method,
                    headers,
                    body,
                },
            )
            .map_err(|_| DispatchError::Failed("Invalid InterceptionId.".to_owned()))?;
        Ok(json!({}))
    }

    // https://chromedevtools.github.io/devtools-protocol/tot/Fetch/#method-fulfillRequest
    fn fetch_fulfill_request(&self, params: &Value) -> Result<Value, DispatchError> {
        let params: params::FetchFulfillRequest = params::parse(params)?;
        let req_id = Self::parse_fetch_request_id(&params.request_id)?;
        let headers = params
            .response_headers
            .unwrap_or_default()
            .into_iter()
            .map(|h| (h.name, h.value))
            .collect();
        let body = match params.body {
            Some(b64) => BASE64_STANDARD
                .decode(b64)
                .map_err(|_| DispatchError::InvalidParams("body".to_owned()))?,
            None => Vec::new(),
        };
        let reason_phrase = params.response_phrase.unwrap_or_else(|| "OK".to_owned());
        self.browser
            .network_broker()
            .resolve_intercept(
                req_id,
                InterceptDecision::ProvideResponse {
                    status: params.response_code,
                    reason_phrase,
                    headers,
                    body,
                },
            )
            .map_err(|_| DispatchError::Failed("Invalid InterceptionId.".to_owned()))?;
        Ok(json!({}))
    }

    // https://chromedevtools.github.io/devtools-protocol/tot/Fetch/#method-failRequest
    fn fetch_fail_request(&self, params: &Value) -> Result<Value, DispatchError> {
        let params: params::FetchFailRequest = params::parse(params)?;
        let req_id = Self::parse_fetch_request_id(&params.request_id)?;
        self.browser
            .network_broker()
            .resolve_intercept(req_id, InterceptDecision::FailRequest)
            .map_err(|_| DispatchError::Failed("Invalid InterceptionId.".to_owned()))?;
        Ok(json!({}))
    }

    // https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#method-compileScript
    async fn runtime_compile_script(
        &mut self,
        tab: &TabHandle,
        params: &Value,
    ) -> Result<Value, DispatchError> {
        let params: params::CompileScript = params::parse(params)?;
        let _ = params.execution_context_id;
        let check = format!(
            "(() => {{ try {{ new Function({}); return ''; }} catch (e) {{ return String(e && e.message ? e.message : e); }} }})()",
            json_string(&params.expression)
        );
        match tab.execute_browser_script(&check).await {
            Ok(RemoteValue::String(err)) if !err.is_empty() => Ok(json!({
                "exceptionDetails": {
                    "exceptionId": 1,
                    "text": err,
                    "lineNumber": 0,
                    "columnNumber": 0,
                }
            })),
            Ok(_) => {
                if params.persist_script.unwrap_or(false) {
                    self.next_script = self.next_script.saturating_add(1);
                    let id = format!("script-{}", self.next_script);
                    self.compiled_scripts.insert(id.clone(), params.expression);
                    Ok(json!({ "scriptId": id }))
                } else {
                    Ok(json!({}))
                }
            }
            Err(err) => Ok(json!({
                "exceptionDetails": {
                    "exceptionId": 1,
                    "text": err.to_string(),
                    "lineNumber": 0,
                    "columnNumber": 0,
                }
            })),
        }
    }

    async fn handle_lifecycle_event(&mut self, event: BrowserLifecycleEvent) -> Vec<Value> {
        let mut messages = Vec::new();
        match event {
            BrowserLifecycleEvent::TabCreated {
                tab, user_context, ..
            } => {
                let context_id = cdp_context_id(&user_context);
                if self.discover_targets {
                    let info = target_info(&self.browser, tab, &context_id).await;
                    push_event(
                        &mut messages,
                        "Target.targetCreated",
                        &json!({ "targetInfo": info }),
                        self.auto_attach_session.as_deref(),
                    );
                }
                if self.auto_attach
                    && !self.auto_attached_tabs.contains(&tab)
                    && let Ok(handle) = self.browser.tab(tab).await
                {
                    let session_id = self.mint(Target::Page(handle.clone()));
                    self.auto_attached_tabs.insert(tab);
                    let mut info = target_info(&self.browser, tab, &context_id).await;
                    if let Some(obj) = info.as_object_mut() {
                        obj.insert("attached".into(), json!(true));
                    }
                    push_event(
                        &mut messages,
                        "Target.attachedToTarget",
                        &json!({
                            "sessionId": session_id,
                            "targetInfo": info,
                            "waitingForDebugger": false,
                        }),
                        self.auto_attach_session.as_deref(),
                    );
                }
            }
            BrowserLifecycleEvent::TabClosed { tab, .. } => {
                if self.auto_attach {
                    let detached: Vec<String> = self
                        .sessions
                        .iter()
                        .filter(|(_, target)| matches!(target, Target::Page(t) if t.id() == tab))
                        .map(|(sid, _)| sid.clone())
                        .collect();
                    for session_id in detached {
                        push_event(
                            &mut messages,
                            "Target.detachedFromTarget",
                            &json!({
                                "sessionId": session_id,
                                "targetId": tab.to_string(),
                            }),
                            self.auto_attach_session.as_deref(),
                        );
                    }
                }
                if self.discover_targets {
                    push_event(
                        &mut messages,
                        "Target.targetDestroyed",
                        &json!({ "targetId": tab.to_string() }),
                        self.auto_attach_session.as_deref(),
                    );
                }
                self.cleanup_tab_state(tab);
            }
        }
        messages
    }

    fn handle_network_event(&self, event: NetworkEvent) -> Vec<Value> {
        let mut messages = Vec::new();
        let timestamp = self.clock_origin.elapsed().as_secs_f64();
        match event {
            NetworkEvent::BeforeRequestSent { .. } => {
                self.push_before_request_sent(&mut messages, event, timestamp);
            }
            NetworkEvent::ResponseStarted { .. } => {
                self.push_response_started(&mut messages, event);
            }
            NetworkEvent::ResponseCompleted { .. } => {
                self.push_response_completed(&mut messages, event, timestamp);
            }
            NetworkEvent::FetchError {
                request_id,
                tab,
                is_navigation,
                error_text,
                ..
            } => {
                if let Some(tab_id) = tab
                    && !is_navigation
                    && self.network_enabled.contains(&tab_id)
                {
                    let failed = json!({
                        "requestId": format!("net-{request_id}"),
                        "timestamp": timestamp,
                        "type": "Other",
                        "errorText": error_text,
                    });
                    for sess in self.sessions_for_tab(tab_id) {
                        push_event(
                            &mut messages,
                            "Network.loadingFailed",
                            &failed,
                            sess.as_deref(),
                        );
                    }
                }
            }
        }
        messages
    }

    fn push_before_request_sent(
        &self,
        messages: &mut Vec<Value>,
        event: NetworkEvent,
        timestamp: f64,
    ) {
        let NetworkEvent::BeforeRequestSent {
            request_id,
            tab: Some(tab_id),
            frame,
            is_navigation,
            url,
            method,
            headers,
            resource_type,
            is_blocked,
            intercepts,
            ..
        } = event
        else {
            return;
        };
        let net_id = format!("net-{request_id}");
        let frame_id = cdp_frame_id(tab_id, frame);
        let header_map: serde_json::Map<String, Value> = headers
            .into_iter()
            .map(|(k, v)| (k, Value::String(v)))
            .collect();
        let owned_fetch = is_blocked
            && self
                .fetch_intercepts
                .get(&tab_id)
                .is_some_and(|mine| intercepts.iter().any(|id| mine.contains(id)));
        let sessions = self.sessions_for_tab(tab_id);
        if owned_fetch {
            let params = json!({
                "requestId": net_id,
                "request": {
                    "url": url,
                    "method": method,
                    "headers": Value::Object(header_map.clone()),
                },
                "frameId": frame_id,
                "resourceType": resource_type,
            });
            for sess in &sessions {
                push_event(messages, "Fetch.requestPaused", &params, sess.as_deref());
            }
        }
        if !is_navigation && self.network_enabled.contains(&tab_id) {
            let params = json!({
                "requestId": net_id,
                "loaderId": self.current_loader(tab_id),
                "documentURL": url,
                "request": {
                    "url": url,
                    "method": method,
                    "headers": Value::Object(header_map),
                },
                "timestamp": timestamp,
                "type": resource_type,
                "frameId": frame_id,
                "initiator": {"type": "other"},
            });
            for sess in &sessions {
                push_event(
                    messages,
                    "Network.requestWillBeSent",
                    &params,
                    sess.as_deref(),
                );
            }
        }
    }

    fn push_response_started(&self, messages: &mut Vec<Value>, event: NetworkEvent) {
        let NetworkEvent::ResponseStarted {
            request_id,
            tab: Some(tab_id),
            frame,
            url,
            status,
            status_text,
            headers,
            is_blocked,
            intercepts,
            ..
        } = event
        else {
            return;
        };
        let owned_fetch = is_blocked
            && self
                .fetch_intercepts
                .get(&tab_id)
                .is_some_and(|mine| intercepts.iter().any(|id| mine.contains(id)));
        if !owned_fetch {
            return;
        }
        let resp_headers: Vec<Value> = headers
            .into_iter()
            .map(|(name, value)| json!({"name": name, "value": value}))
            .collect();
        let params = json!({
            "requestId": format!("net-{request_id}"),
            "request": {
                "url": url,
                "method": "GET",
                "headers": {},
            },
            "frameId": cdp_frame_id(tab_id, frame),
            "resourceType": "Other",
            "responseStatusCode": status,
            "responseStatusText": status_text,
            "responseHeaders": resp_headers,
        });
        for sess in self.sessions_for_tab(tab_id) {
            push_event(messages, "Fetch.requestPaused", &params, sess.as_deref());
        }
    }

    fn push_response_completed(
        &self,
        messages: &mut Vec<Value>,
        event: NetworkEvent,
        timestamp: f64,
    ) {
        let NetworkEvent::ResponseCompleted {
            request_id,
            tab: Some(tab_id),
            frame,
            is_navigation,
            url,
            status,
            status_text,
            headers,
            mime_type,
            from_cache,
            encoded_data_length,
            ..
        } = event
        else {
            return;
        };
        if is_navigation || !self.network_enabled.contains(&tab_id) {
            return;
        }
        let net_id = format!("net-{request_id}");
        let header_map: serde_json::Map<String, Value> = headers
            .into_iter()
            .map(|(k, v)| (k, Value::String(v)))
            .collect();
        let received = json!({
            "requestId": net_id,
            "loaderId": self.current_loader(tab_id),
            "frameId": cdp_frame_id(tab_id, frame),
            "timestamp": timestamp,
            "type": "Other",
            "response": {
                "url": url,
                "status": status,
                "statusText": status_text,
                "headers": Value::Object(header_map),
                "mimeType": mime_type,
                "connectionReused": false,
                "connectionId": 0,
                "fromDiskCache": from_cache,
                "fromPrefetchCache": false,
                "encodedDataLength": encoded_data_length,
            },
        });
        let finished = json!({
            "requestId": net_id,
            "timestamp": timestamp,
            "encodedDataLength": encoded_data_length,
        });
        for sess in self.sessions_for_tab(tab_id) {
            push_event(
                messages,
                "Network.responseReceived",
                &received,
                sess.as_deref(),
            );
            push_event(
                messages,
                "Network.loadingFinished",
                &finished,
                sess.as_deref(),
            );
        }
    }

    async fn handle_prompt_event(&self, event: PromptEvent) -> Vec<Value> {
        let mut messages = Vec::new();
        match event {
            PromptEvent::Opened { info, .. } => {
                if !self.prompt_tabs.contains(&info.tab) {
                    return messages;
                }
                let url = match self.browser.tab(info.tab).await {
                    Ok(tab) => tab.document_url().await.unwrap_or_default(),
                    Err(_) => String::new(),
                };
                let params = json!({
                    "url": url,
                    "message": info.message,
                    "type": info.kind.as_str(),
                    "hasBrowserHandler": true,
                    "defaultPrompt": info.default_value.unwrap_or_default(),
                });
                for sess in self.sessions_for_tab(info.tab) {
                    push_event(
                        &mut messages,
                        "Page.javascriptDialogOpening",
                        &params,
                        sess.as_deref(),
                    );
                }
            }
            PromptEvent::Closed {
                tab,
                accepted,
                user_text,
                ..
            } => {
                if !self.prompt_tabs.contains(&tab) {
                    return messages;
                }
                let params = json!({
                    "result": accepted,
                    "userInput": user_text.unwrap_or_default(),
                });
                for sess in self.sessions_for_tab(tab) {
                    push_event(
                        &mut messages,
                        "Page.javascriptDialogClosed",
                        &params,
                        sess.as_deref(),
                    );
                }
            }
        }
        messages
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
        let allow_unsafe_eval = options.allow_unsafe_eval_blocked_by_csp.unwrap_or(true);
        let context_world = options
            .context_id
            .or(options.execution_context_id)
            .and_then(|id| self.isolated_contexts.get(&id))
            .map(|(_, name)| name.clone());
        let await_timeout = match options.timeout.filter(|millis| *millis > 0.0) {
            Some(millis) => Duration::try_from_secs_f64(millis / 1000.0)
                .map_err(|_| DispatchError::InvalidParams("timeout".to_owned()))?,
            None => AWAIT_PROMISE_TIMEOUT,
        };
        let isolated_world = context_world.or_else(|| runtime_source_world(&source));
        let script = runtime_source_expression(source);
        let target = RuntimeTarget {
            tab,
            frame,
            isolated_world: isolated_world.as_deref(),
            allow_unsafe_eval,
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
            match Self::schedule_runtime_slot(&target, &script, &make, &eval_id).await {
                Ok(RuntimeSchedule::Evaluation) => {}
                Ok(RuntimeSchedule::MissingHandle) => {
                    return Err(DispatchError::Failed(MISSING_HANDLE.to_owned()));
                }
                Err(reply) => return Ok(DispatchResult::Immediate(reply)),
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
        Self::runtime_handle(&target, &script, object_group)
            .await
            .map(DispatchResult::Immediate)
    }

    async fn runtime_handle(
        target: &RuntimeTarget<'_>,
        script: &RuntimeScript,
        object_group: &str,
    ) -> Result<Value, DispatchError> {
        let handle_script = RUNTIME_HANDLE
            .replace("__MISSING__", &script.missing)
            .replace("__GROUP__", &json_string(object_group))
            .replace("__SOURCE__", &script.source);
        let value = match target.execute(&handle_script).await {
            Ok(value) => value,
            Err(reply) => return Ok(reply),
        };
        let RemoteValue::String(text) = value else {
            return Ok(exception_text_reply("unexpected script result"));
        };
        if text == "missing-handle" {
            return Err(DispatchError::Failed(MISSING_HANDLE.to_owned()));
        }
        match serde_json::from_str::<Value>(&text) {
            Ok(parsed) => Ok(json!({"result": parsed})),
            Err(_) => Ok(exception_text_reply("unexpected script result")),
        }
    }

    async fn schedule_runtime_slot(
        target: &RuntimeTarget<'_>,
        script: &RuntimeScript,
        make: &str,
        id: &str,
    ) -> Result<RuntimeSchedule, Value> {
        let schedule = RUNTIME_SCHEDULE
            .replace("__MISSING__", &script.missing)
            .replace("__ID__", id)
            .replace("__MAKE__", make)
            .replace("__SOURCE__", &script.source);
        match target.execute(&schedule).await? {
            RemoteValue::String(text) if text == "missing-handle" => {
                Ok(RuntimeSchedule::MissingHandle)
            }
            _ => Ok(RuntimeSchedule::Evaluation),
        }
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

fn runtime_source_world(source: &RuntimeSource) -> Option<String> {
    match source {
        RuntimeSource::Evaluate { .. } => None,
        RuntimeSource::CallFunctionOn { object_id, .. } => object_id
            .as_deref()
            .and_then(dispatch::isolated_world_of_handle)
            .map(str::to_owned),
    }
}

fn runtime_source_expression(source: RuntimeSource) -> RuntimeScript {
    match source {
        RuntimeSource::Evaluate { expression } => RuntimeScript {
            source: format!("host.evaluatePage({})", json_string(&expression)),
            missing: "false".to_owned(),
        },
        RuntimeSource::CallFunctionOn {
            declaration,
            object_id,
            arguments,
        } => {
            let missing = missing_handles_expression(object_id.as_deref(), &arguments);
            let receiver = object_id.map_or_else(
                || "undefined".to_owned(),
                |id| format!("host.__tb_handles[{}]", json_string(&id)),
            );
            RuntimeScript {
                source: format!(
                    "host.apply(host.evaluatePage({}), {receiver}, {})",
                    json_string(&format!("({declaration})")),
                    arguments_expression(&arguments)
                ),
                missing,
            }
        }
    }
}

async fn set_extra_http_headers(tab: &TabHandle, params: &Value) -> Result<Value, DispatchError> {
    let params: params::SetExtraHttpHeaders = params::parse(params)?;
    let mut headers = Vec::with_capacity(params.headers.len());
    for (name, value) in params.headers {
        let Some(text) = value.as_str() else {
            return Err(DispatchError::InvalidParams("headers".to_owned()));
        };
        headers.push((name, text.to_owned()));
    }
    tab.set_extra_headers(headers)
        .await
        .map_err(|error| DispatchError::InvalidParams(error.to_string()))?;
    Ok(json!({}))
}

async fn next_tab_event(receiver: &mut Option<mpsc::Receiver<TabEvent>>) -> Option<TabEvent> {
    match receiver {
        Some(receiver) => receiver.recv().await,
        None => std::future::pending().await,
    }
}

fn target_user_context(target: &Target) -> UserContextId {
    match target {
        Target::Page(tab) => tab.user_context().clone(),
        Target::Browser => UserContextId::default_context(),
    }
}

fn push_frame_attached(
    messages: &mut Vec<Value>,
    tab_id: TabId,
    frame: FrameId,
    parent: FrameId,
    sess: Option<&str>,
) {
    push_event(
        messages,
        "Page.frameAttached",
        &json!({
            "frameId": cdp_frame_id(tab_id, frame),
            "parentFrameId": cdp_frame_id(tab_id, parent),
        }),
        sess,
    );
}

fn push_frame_navigated(
    messages: &mut Vec<Value>,
    tab_id: TabId,
    frame: FrameId,
    url: &str,
    sess: Option<&str>,
) {
    push_event(
        messages,
        "Page.frameNavigated",
        &json!({
            "frame": {
                "id": cdp_frame_id(tab_id, frame),
                "parentId": tab_id.to_string(),
                "loaderId": "",
                "url": url,
                "securityOrigin": url_security_origin(url),
                "mimeType": "text/html",
            }
        }),
        sess,
    );
}

fn push_frame_detached(
    messages: &mut Vec<Value>,
    tab_id: TabId,
    frame: FrameId,
    sess: Option<&str>,
) {
    push_event(
        messages,
        "Page.frameDetached",
        &json!({
            "frameId": cdp_frame_id(tab_id, frame),
            "reason": "remove",
        }),
        sess,
    );
}

async fn set_document_content(tab: &TabHandle, params: &Value) -> Result<Value, DispatchError> {
    let params: params::SetDocumentContent = params::parse(params)?;
    let frame = requested_frame(tab, Some(params.frame_id.as_str()))?;
    if frame == FrameId::MAIN {
        tab.load_html(&params.html)
            .await
            .map_err(|err| DispatchError::Failed(err.to_string()))?;
    } else {
        let script = format!(
            "(() => {{ document.open(); document.write({}); document.close(); }})()",
            json_string(&params.html)
        );
        tab.execute_script(ExecuteScriptOptions {
            frame,
            source: browser::ScriptSource::Browser(&script),
            timeout: None,
        })
        .await
        .map_err(|err| DispatchError::Failed(err.to_string()))?;
    }
    Ok(json!({}))
}

fn new_conn(
    state: AppState,
    tab: Option<TabHandle>,
    deferred_tx: mpsc::UnboundedSender<Value>,
) -> Conn {
    let (tab_events_tx, tab_events_rx) = mpsc::channel(256);
    Conn {
        browser: state.browser,
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
        auto_attach_session: None,
        auto_attached_tabs: HashSet::new(),
        discover_targets: false,
        network_enabled: HashSet::new(),
        prompt_tabs: HashSet::new(),
        fetch_intercepts: HashMap::new(),
        compiled_scripts: HashMap::new(),
        next_script: 1,
        events: Vec::new(),
        loader_ids: HashMap::new(),
        current_loaders: HashMap::new(),
        default_contexts: HashMap::new(),
        lifecycle_events: false,
        isolated_worlds: HashMap::new(),
        isolated_contexts: HashMap::new(),
        omit_background: HashMap::new(),
        response_bodies: HashMap::new(),
        response_body_order: VecDeque::new(),
        next_loader: 1,
        next_context: 1000,
        next_evaluation: 1,
    }
}

async fn run_socket(mut socket: WebSocket, state: AppState, tab: Option<TabHandle>) {
    let (deferred_tx, mut deferred_rx) = mpsc::unbounded_channel();
    let mut lifecycle_rx = state.browser.subscribe();
    let mut network_rx = state.browser.network_broker().subscribe();
    let mut prompt_rx = state.browser.prompt_broker().subscribe();
    let mut conn = new_conn(state, tab, deferred_tx);
    loop {
        let wake = tokio::select! {
            event = conn.tab_events_rx.recv() => SocketWake::Event(event),
            lifecycle = lifecycle_rx.recv() => SocketWake::Lifecycle(lifecycle),
            net = network_rx.recv() => SocketWake::Network(net),
            prompt = prompt_rx.recv() => SocketWake::Prompt(prompt),
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
            SocketWake::Lifecycle(Ok(event)) => {
                let messages = conn.handle_lifecycle_event(event).await;
                if send_messages(&mut socket, messages).await.is_err() {
                    break;
                }
            }
            SocketWake::Network(Ok(event)) => {
                let messages = conn.handle_network_event(event);
                if send_messages(&mut socket, messages).await.is_err() {
                    break;
                }
            }
            SocketWake::Prompt(Ok(event)) => {
                let messages = conn.handle_prompt_event(event).await;
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
            SocketWake::Lifecycle(Err(_))
            | SocketWake::Network(Err(_))
            | SocketWake::Prompt(Err(_))
            | SocketWake::Deferred(None)
            | SocketWake::Incoming(Some(Ok(_))) => {}
        }
    }
    conn.clear_all_fetch_intercepts();
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
    Lifecycle(Result<BrowserLifecycleEvent, broadcast::error::RecvError>),
    Network(Result<NetworkEvent, broadcast::error::RecvError>),
    Prompt(Result<PromptEvent, broadcast::error::RecvError>),
    Deferred(Option<Value>),
    Incoming(Option<Result<WsMessage, WsError>>),
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

async fn send_messages(socket: &mut WebSocket, messages: Vec<Value>) -> Result<(), WsError> {
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
            deferred_tx: None,
            clock_origin: Instant::now(),
            auto_attach: false,
            auto_attach_session: None,
            auto_attached_tabs: HashSet::new(),
            discover_targets: false,
            network_enabled: HashSet::new(),
            prompt_tabs: HashSet::new(),
            fetch_intercepts: HashMap::new(),
            compiled_scripts: HashMap::new(),
            next_script: 1,
            events: Vec::new(),
            loader_ids: HashMap::new(),
            current_loaders: HashMap::new(),
            default_contexts: HashMap::new(),
            lifecycle_events: false,
            isolated_worlds: HashMap::new(),
            isolated_contexts: HashMap::new(),
            omit_background: HashMap::new(),
            response_bodies: HashMap::new(),
            response_body_order: VecDeque::new(),
            next_loader: 1,
            next_context: 1000,
            next_evaluation: 1,
        }
    }

    #[test]
    fn missing_handles_are_checked_before_evaluation() {
        let arguments: Vec<params::CallArgument> = serde_json::from_value(json!([
            {"objectId": "h2"},
            {"value": 1},
            {},
        ]))
        .expect("arguments");
        assert_eq!(
            missing_handles_expression(Some("h1"), &arguments),
            r#"host.__tb_handles["h1"] === undefined || host.__tb_handles["h2"] === undefined"#
        );
        assert_eq!(missing_handles_expression(None, &[]), "false");
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
