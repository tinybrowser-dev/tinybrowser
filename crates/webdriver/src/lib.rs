// https://w3c.github.io/webdriver/

use std::collections::HashMap;
use std::net::TcpListener;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use browser::{
    BrowserHandle, CookieRecord, CookieSameSite, RemoteValue, ScreenshotRequest, ScriptFailure,
    TabError, TabHandle,
};
use http::{Method, StatusCode, header};
use serde_json::{Value, json};
use server::{Request, Response, Shutdown};
use tokio::sync::Mutex;

const ELEMENT_KEY: &str = "element-6066-11e4-a52e-4f735466cecf";
const DEFAULT_SCRIPT_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_PAGE_LOAD_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Clone)]
pub struct Router {
    state: AppState,
}

impl Router {
    #[must_use]
    pub fn new(browser: BrowserHandle) -> Self {
        let sessions = Arc::new(Mutex::new(Sessions {
            browser,
            next_window: 0,
            open: HashMap::new(),
        }));
        Self {
            state: AppState { sessions },
        }
    }

    pub async fn handle(&self, request: Request) -> Response {
        serve_request(request, self.state.clone()).await
    }
}

pub async fn serve(listener: &TcpListener, browser: &BrowserHandle) -> std::io::Result<()> {
    let router = Router::new(browser.clone());
    server::serve(
        listener,
        Shutdown::new(),
        router,
        |request, router| async move { router.handle(request).await },
    )
    .await
}

#[derive(Clone)]
struct AppState {
    sessions: Arc<Mutex<Sessions>>,
}

const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;

async fn serve_request(request: Request, state: AppState) -> Response {
    if !server::is_loopback_host(&request) {
        return server::text(StatusCode::FORBIDDEN, "non-loopback Host header rejected");
    }
    if server::has_origin(&request) {
        return server::text(StatusCode::FORBIDDEN, "Origin header rejected");
    }
    if server::has_browser_fetch_metadata(&request) {
        return server::text(StatusCode::FORBIDDEN, "Sec-Fetch-* header rejected");
    }
    if matches!(
        *request.method(),
        Method::POST | Method::PUT | Method::PATCH
    ) && request.headers().contains_key(header::CONTENT_TYPE)
        && !server::is_json_content_type(&request)
    {
        return server::text(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Content-Type must be application/json",
        );
    }
    let method = request.method().as_str().to_owned();
    let path = request.uri().path().to_owned();
    let body = match server::read_body(request.into_body(), MAX_BODY_BYTES).await {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(server::BodyError::TooLarge) => {
            return server::text(StatusCode::PAYLOAD_TOO_LARGE, "request body too large");
        }
        Err(server::BodyError::Read) => {
            return server::text(StatusCode::BAD_REQUEST, "invalid request body");
        }
    };
    let mut sessions = state.sessions.lock().await;
    let (status, payload) = dispatch(&method, &path, &body, &mut sessions).await;
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    server::json(status, &payload)
}

// https://w3c.github.io/webdriver/#new-session
fn random_session_id() -> Result<String, String> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| "failed to generate session id".to_owned())?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let mut out = String::with_capacity(36);
    for (index, byte) in bytes.iter().enumerate() {
        if matches!(index, 4 | 6 | 8 | 10) {
            out.push('-');
        }
        out.push(HEX[usize::from(byte >> 4)] as char);
        out.push(HEX[usize::from(byte & 0x0f)] as char);
    }
    Ok(out)
}

struct Sessions {
    browser: BrowserHandle,
    next_window: u32,
    open: HashMap<String, Session>,
}

struct Session {
    current: String,
    windows: HashMap<String, Window>,
    implicit_timeout: Duration,
    script_timeout: Duration,
    page_load_timeout: Duration,
    window_rect: [f64; 4],
    permissions: HashMap<String, String>,
}

struct Window {
    tab: TabHandle,
}

impl Sessions {
    // https://w3c.github.io/webdriver/#get-window-handles
    async fn sync_windows(&mut self, session: &str) {
        let browser = self.browser.clone();
        let Ok(ids) = browser.tabs().await else {
            return;
        };
        let Some(found) = self.open.get_mut(session) else {
            return;
        };
        found
            .windows
            .retain(|_, window| ids.contains(&window.tab.id()));
        for id in ids {
            if found.windows.values().any(|window| window.tab.id() == id) {
                continue;
            }
            let Ok(tab) = browser.tab(id).await else {
                continue;
            };
            self.next_window += 1;
            found
                .windows
                .insert(format!("w{}", self.next_window), Window { tab });
        }
        if !found.windows.contains_key(&found.current)
            && let Some(next) = found.windows.keys().next().cloned()
        {
            found.current = next;
        }
    }

    async fn blank_window(&self) -> Result<Window, String> {
        let tab = self
            .browser
            .create_tab()
            .await
            .map_err(|err| err.to_string())?;
        if let Err(error) = tab.load_html("<!doctype html><title></title>").await {
            let _result = self.browser.close_tab(tab.id()).await;
            return Err(error.to_string());
        }
        Ok(Window { tab })
    }

    async fn create(&mut self) -> Result<String, String> {
        let id = random_session_id()?;
        let mut windows = HashMap::new();
        let mut current = None;
        if let Ok(ids) = self.browser.tabs().await {
            for tab_id in ids {
                if let Ok(tab) = self.browser.tab(tab_id).await {
                    self.next_window += 1;
                    let handle = format!("w{}", self.next_window);
                    if current.is_none() {
                        current = Some(handle.clone());
                    }
                    windows.insert(handle, Window { tab });
                }
            }
        }
        let current = if let Some(handle) = current {
            handle
        } else {
            self.next_window += 1;
            let handle = format!("w{}", self.next_window);
            let window = self.blank_window().await?;
            windows.insert(handle.clone(), window);
            handle
        };
        self.open.insert(
            id.clone(),
            Session {
                current,
                windows,
                implicit_timeout: Duration::ZERO,
                script_timeout: DEFAULT_SCRIPT_TIMEOUT,
                page_load_timeout: DEFAULT_PAGE_LOAD_TIMEOUT,
                window_rect: [0.0, 0.0, 800.0, 600.0],
                permissions: HashMap::new(),
            },
        );
        Ok(id)
    }
}

async fn dispatch(method: &str, path: &str, body: &str, sessions: &mut Sessions) -> (u16, Value) {
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    // https://w3c.github.io/webdriver/#get-window-handles
    if let ["session", session, ..] = segments.as_slice() {
        sessions.sync_windows(session).await;
    }
    match (method, segments.as_slice()) {
        ("GET", ["status"]) => ok(json!({"ready": true, "message": "ready"})),
        ("POST", ["session"]) => {
            if !sessions.open.is_empty() {
                return error(500, "session not created", "already have a session");
            }
            match sessions.create().await {
                Ok(id) => {
                    ok(json!({"sessionId": id, "capabilities": {"browserName": "tinybrowser"}}))
                }
                Err(err) => error(500, "session not created", &err),
            }
        }
        ("DELETE", ["session", session]) => delete_session(sessions, session).await,
        ("POST", ["session", session, "url"]) => navigate(sessions, session, body).await,
        ("GET", ["session", session, "url"]) => current_url(sessions, session).await,
        ("GET", ["session", session, "screenshot"]) => take_screenshot(sessions, session).await,
        ("POST", ["session", session, "execute", "sync"]) => {
            execute(sessions, session, body, false).await
        }
        ("POST", ["session", session, "execute", "async"]) => {
            execute(sessions, session, body, true).await
        }
        ("GET", ["session", session, "window"]) => current_window(sessions, session),
        ("GET", ["session", session, "window", "handles"]) => window_handles(sessions, session),
        ("POST", ["session", session, "window"]) => switch_window(sessions, session, body),
        ("POST", ["session", session, "window", "new"]) => new_window(sessions, session).await,
        ("DELETE", ["session", session, "window"]) => close_window(sessions, session).await,
        ("GET", ["session", session, "window", "rect"]) => window_rect(sessions, session),
        ("POST", ["session", session, "window", "rect"]) => {
            set_window_rect(sessions, session, body)
        }
        ("POST", ["session", session, "timeouts"]) => set_timeouts(sessions, session, body),
        ("GET", ["session", session, "timeouts"]) => get_timeouts(sessions, session),
        ("POST", ["session", session, "permissions"]) => set_permission(sessions, session, body),
        ("POST", ["session", session, "element"]) => find_element(sessions, session, body).await,
        ("POST", ["session", session, "elements"]) => find_elements(sessions, session, body).await,
        ("POST", ["session", session, "element", element, "click"]) => {
            element_click(sessions, session, element).await
        }
        ("POST", ["session", session, "element", element, "value"]) => {
            element_send_keys(sessions, session, element, body).await
        }
        ("GET", ["session", session, "cookie"]) => get_cookies(sessions, session).await,
        ("GET", ["session", session, "cookie", name]) => {
            get_named_cookie(sessions, session, name).await
        }
        ("GET", ["session", session, "element", element, "rect"]) => {
            element_rect(sessions, session, element).await
        }
        ("POST", ["session", session, "cookie"]) => add_cookie(sessions, session, body).await,
        ("DELETE", ["session", session, "cookie"]) => delete_cookies(sessions, session).await,
        ("POST", ["session", session, "window", op])
            if matches!(*op, "minimize" | "maximize" | "fullscreen") =>
        {
            window_op(sessions, session)
        }
        ("POST", ["session", session, "actions"]) => perform_actions(sessions, session, body).await,
        ("DELETE", ["session", session, "actions"]) => {
            if sessions.open.contains_key(*session) {
                ok(Value::Null)
            } else {
                error(404, "invalid session id", session)
            }
        }
        _ => error(404, "unknown command", path),
    }
}

async fn navigate(sessions: &mut Sessions, session: &str, body: &str) -> (u16, Value) {
    let Some(url) = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| value.get("url").and_then(Value::as_str).map(str::to_owned))
    else {
        return error(400, "invalid argument", "missing url");
    };
    let page_load_timeout = match sessions.open.get(session) {
        Some(found) => found.page_load_timeout,
        None => return error(404, "invalid session id", session),
    };
    let Some(window) = current(sessions, session) else {
        return error(404, "invalid session id", session);
    };
    match window.tab.goto(&url).await {
        Ok(()) => {
            match window.tab.run_until_load_timeout(page_load_timeout).await {
                Ok(false) => return error(500, "timeout", "navigation timed out"),
                Ok(true) => {}
                Err(err) => return error(500, "unknown error", &err.to_string()),
            }
            match window.tab.last_navigation_failed().await {
                Ok(true) => error(500, "unknown error", "navigation failed"),
                Ok(false) => ok(Value::Null),
                Err(err) => error(500, "unknown error", &err.to_string()),
            }
        }
        Err(TabError::InvalidUrl { spec }) => error(400, "invalid argument", &spec),
        Err(err) => error(500, "unknown error", &err.to_string()),
    }
}

async fn current_url(sessions: &Sessions, session: &str) -> (u16, Value) {
    match current(sessions, session) {
        Some(window) => match window.tab.document_url().await {
            Ok(url) => ok(json!(url)),
            Err(err) => error(500, "unknown error", &err.to_string()),
        },
        None => error(404, "invalid session id", session),
    }
}

// https://w3c.github.io/webdriver/#take-screenshot
async fn take_screenshot(sessions: &Sessions, session: &str) -> (u16, Value) {
    match current(sessions, session) {
        Some(window) => {
            let request = ScreenshotRequest {
                viewport_width: browser::DEFAULT_VIEWPORT.0,
                viewport_height: browser::DEFAULT_VIEWPORT.1,
                clip: None,
                format: browser::ScreenshotFormat::Png,
                omit_background: false,
            };
            match window.tab.screenshot(request).await {
                Ok(png) => ok(json!(BASE64_STANDARD.encode(&png))),
                Err(err) => error(500, "unable to capture screen", &err.to_string()),
            }
        }
        None => error(404, "invalid session id", session),
    }
}

async fn execute(
    sessions: &mut Sessions,
    session: &str,
    body: &str,
    asynchronous: bool,
) -> (u16, Value) {
    let parsed: Value = match serde_json::from_str(body) {
        Ok(value) => value,
        Err(err) => return error(400, "invalid argument", &err.to_string()),
    };
    let script = parsed
        .get("script")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let args = parsed.get("args").cloned().unwrap_or(json!([]));
    let (script_timeout, handle) = match sessions.open.get(session) {
        Some(found) => (found.script_timeout, found.current.clone()),
        None => return error(404, "invalid session id", session),
    };
    let Some(window) = current(sessions, session) else {
        return error(404, "invalid session id", session);
    };
    let started = Instant::now();
    let wrapped = wrap_script(script, &args, asynchronous);
    match window
        .tab
        .execute_script(browser::ExecuteScriptOptions {
            frame: browser::FrameId::MAIN,
            source: browser::ScriptSource::Browser(&wrapped),
            timeout: Some(script_timeout),
        })
        .await
    {
        Err(err) => script_error(&err),
        Ok(_) if asynchronous => {
            wait_for_async(window, &handle, remaining(started, script_timeout)).await
        }
        Ok(value) => match window
            .tab
            .execute_browser_script("host.webDriver.wait === true")
            .await
        {
            Ok(RemoteValue::Bool(true)) => {
                wait_for_async(window, &handle, remaining(started, script_timeout)).await
            }
            Ok(_) => ok(encode_scoped(&value, &handle)),
            Err(err) => script_error(&err),
        },
    }
}

fn remaining(started: Instant, budget: Duration) -> Duration {
    budget.saturating_sub(started.elapsed())
}

async fn wait_for_async(window: &Window, handle: &str, script_timeout: Duration) -> (u16, Value) {
    match window
        .tab
        .run_until_js_true(browser::RunUntilJsTrueOptions {
            frame: browser::FrameId::MAIN,
            source: browser::ScriptSource::Browser("host.webDriver.done === true"),
            timeout: script_timeout,
        })
        .await
    {
        Ok(false) => return error(500, "script timeout", "script timeout"),
        Ok(true) => {}
        Err(err) => return error(500, "unknown error", &err.to_string()),
    }
    match window
        .tab
        .execute_browser_script("host.webDriver.failed === true")
        .await
    {
        Ok(RemoteValue::Bool(true)) => {
            let message = match window
                .tab
                .execute_browser_script("String(host.webDriver.error)")
                .await
            {
                Ok(RemoteValue::String(text)) => text,
                Ok(_) => "javascript error".to_owned(),
                Err(err) => return script_error(&err),
            };
            return error(500, "javascript error", &message);
        }
        Ok(_) => {}
        Err(err) => return script_error(&err),
    }
    match window
        .tab
        .execute_browser_script("host.webDriver.value")
        .await
    {
        Ok(value) => ok(encode_scoped(&value, handle)),
        Err(err) => script_error(&err),
    }
}

fn script_error(err: &TabError) -> (u16, Value) {
    match err {
        TabError::Script(ScriptFailure::Interrupted) => {
            error(500, "script timeout", "script timeout")
        }
        _ => error(500, "javascript error", &err.to_string()),
    }
}

fn wrap_script(script: &str, args: &Value, asynchronous: bool) -> String {
    let args_json = args.to_string();
    let function = Value::String(format!("(function() {{\n{script}\n}})")).to_string();
    let setup = "const slot = { __proto__: null, value: undefined, error: undefined, failed: false, done: false, wait: false }; host.webDriver = slot;";
    if asynchronous {
        format!(
            "(() => {{ {setup}\n\
             const args = {args_json};\n\
             args[args.length] = function(v) {{ slot.value = v === undefined ? null : v; slot.done = true; }};\n\
             host.apply(host.evaluatePage({function}), null, args);\n\
             }})()"
        )
    } else {
        format!(
            "(function() {{ {setup}\n\
               var result = host.apply(host.evaluatePage({function}), null, {args_json});\n\
                if (result && typeof result.then === 'function') {{\n\
                  result.then(function(v) {{\n\
                    slot.value = v === undefined ? null : v;\n\
                    slot.done = true;\n\
                  }}, function(e) {{\n\
                    slot.failed = true;\n\
                    slot.error = e == null ? 'undefined' : (e && e.message ? String(e.message) : String(e));\n\
                    slot.done = true;\n\
                  }});\n\
                  slot.wait = true;\n\
                 return null;\n\
               }}\n\
               return result;\n\
             }})()"
        )
    }
}

fn encode_scoped(value: &RemoteValue, handle: &str) -> Value {
    match value {
        RemoteValue::Node(id) => json!({ ELEMENT_KEY: format!("{handle}:{id}") }),
        RemoteValue::List(items) => Value::Array(
            items
                .iter()
                .map(|item| encode_scoped(item, handle))
                .collect(),
        ),
        RemoteValue::Map(entries) => {
            let mut map = serde_json::Map::new();
            for (key, item) in entries {
                map.insert(key.clone(), encode_scoped(item, handle));
            }
            Value::Object(map)
        }
        other => encode(other),
    }
}

fn encode(value: &RemoteValue) -> Value {
    match value {
        RemoteValue::Undefined | RemoteValue::Null => Value::Null,
        RemoteValue::Bool(flag) => json!(flag),
        RemoteValue::Number(number) => {
            if number.is_finite() {
                json!(number)
            } else {
                Value::Null
            }
        }
        RemoteValue::String(text) => json!(text),
        RemoteValue::List(items) => Value::Array(items.iter().map(encode).collect()),
        RemoteValue::Map(entries) => {
            let mut map = serde_json::Map::new();
            for (key, item) in entries {
                map.insert(key.clone(), encode(item));
            }
            Value::Object(map)
        }
        RemoteValue::Node(id) => json!({ ELEMENT_KEY: id.to_string() }),
    }
}

// https://w3c.github.io/webdriver/#find-elements
async fn find_matches(
    sessions: &Sessions,
    session: &str,
    body: &str,
) -> Result<Vec<u64>, (u16, Value)> {
    // https://w3c.github.io/webdriver/#processing-model
    if current(sessions, session).is_none() {
        return Err(error(404, "invalid session id", session));
    }
    let parsed: Value = serde_json::from_str(body)
        .map_err(|err| error(400, "invalid argument", &err.to_string()))?;
    let Some(using) = parsed.get("using").and_then(Value::as_str) else {
        return Err(error(400, "invalid argument", "using is required"));
    };
    let Some(selector) = parsed.get("value").and_then(Value::as_str) else {
        return Err(error(400, "invalid argument", "value is required"));
    };
    if using != "css selector" {
        return Err(error(
            400,
            "invalid argument",
            "only the css selector strategy is supported",
        ));
    }
    let Some(window) = current(sessions, session) else {
        return Err(error(404, "invalid session id", session));
    };
    // https://w3c.github.io/webdriver/#dfn-implicit-wait
    let (implicit_timeout, script_timeout) = sessions
        .open
        .get(session)
        .map_or((Duration::ZERO, DEFAULT_SCRIPT_TIMEOUT), |found| {
            (found.implicit_timeout, found.script_timeout)
        });
    let timeout = if implicit_timeout.is_zero() {
        script_timeout
    } else {
        implicit_timeout
    };
    let script = format!(
        "(function(){{try{{return Array.from(document.querySelectorAll({}))}}\
         catch(e){{return 'invalid selector'}}}})()",
        json!(selector)
    );
    match window
        .tab
        .execute_script(browser::ExecuteScriptOptions {
            frame: browser::FrameId::MAIN,
            source: browser::ScriptSource::Page(&script),
            timeout: Some(timeout),
        })
        .await
    {
        Ok(RemoteValue::List(items)) => Ok(items
            .into_iter()
            .filter_map(|item| match item {
                RemoteValue::Node(id) => Some(id),
                _ => None,
            })
            .collect()),
        Ok(RemoteValue::String(text)) if text == "invalid selector" => Err(error(
            400,
            "invalid selector",
            "the selector is not a valid CSS selector",
        )),
        Ok(_) => Ok(Vec::new()),
        Err(err) => Err(error(500, "unknown error", &err.to_string())),
    }
}

// https://w3c.github.io/webdriver/#find-element
async fn find_element(sessions: &Sessions, session: &str, body: &str) -> (u16, Value) {
    let Some(handle) = current_handle(sessions, session) else {
        return error(404, "invalid session id", session);
    };
    match find_matches(sessions, session, body).await {
        Ok(matches) => match matches.first() {
            Some(id) => ok(json!({ ELEMENT_KEY: format!("{handle}:{id}") })),
            None => error(404, "no such element", "no element matches the selector"),
        },
        Err(reply) => reply,
    }
}

// https://w3c.github.io/webdriver/#find-elements
async fn find_elements(sessions: &Sessions, session: &str, body: &str) -> (u16, Value) {
    let Some(handle) = current_handle(sessions, session) else {
        return error(404, "invalid session id", session);
    };
    match find_matches(sessions, session, body).await {
        Ok(matches) => ok(Value::Array(
            matches
                .iter()
                .map(|id| json!({ ELEMENT_KEY: format!("{handle}:{id}") }))
                .collect(),
        )),
        Err(reply) => reply,
    }
}

// https://w3c.github.io/webdriver/#elements
// https://w3c.github.io/webdriver/#dfn-stale-element-reference
fn element_remote(element: &str) -> Result<(&str, u64), (u16, Value)> {
    let Some((handle, id)) = element.rsplit_once(':') else {
        return Err(error(404, "no such element", "unknown element id"));
    };
    let id = id
        .parse::<u64>()
        .map_err(|_| error(404, "no such element", "unknown element id"))?;
    Ok((handle, id))
}

fn is_current_handle(sessions: &Sessions, session: &str, handle: &str) -> bool {
    current_handle(sessions, session).is_some_and(|current| current == handle)
}

// https://w3c.github.io/webdriver/#element-click
async fn element_click(sessions: &Sessions, session: &str, element: &str) -> (u16, Value) {
    let Some(window) = current(sessions, session) else {
        return error(404, "invalid session id", session);
    };
    let (handle, remote) = match element_remote(element) {
        Ok(remote) => remote,
        Err(reply) => return reply,
    };
    if !is_current_handle(sessions, session, handle) {
        return error(404, "no such element", "element belongs to another window");
    }
    let script = format!(
        "(function(){{const el=host.__tb_webdriver_element({remote});\
         if(el===null)return false;host.__tb_webdriver_click(el);return true;}})()"
    );
    match window.tab.execute_browser_script(&script).await {
        Ok(RemoteValue::Bool(true)) => ok(Value::Null),
        Ok(_) => error(404, "no such element", "unknown element id"),
        Err(err) => script_error(&err),
    }
}

// https://w3c.github.io/webdriver/#element-send-keys
async fn element_send_keys(
    sessions: &Sessions,
    session: &str,
    element: &str,
    body: &str,
) -> (u16, Value) {
    let parsed: Value = match serde_json::from_str(body) {
        Ok(value) => value,
        Err(err) => return error(400, "invalid argument", &err.to_string()),
    };
    let text = match parsed.get("text") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(items)) => items.iter().filter_map(Value::as_str).collect::<String>(),
        _ => return error(400, "invalid argument", "text is required"),
    };
    let Some(window) = current(sessions, session) else {
        return error(404, "invalid session id", session);
    };
    let (handle, remote) = match element_remote(element) {
        Ok(remote) => remote,
        Err(reply) => return reply,
    };
    if !is_current_handle(sessions, session, handle) {
        return error(404, "no such element", "element belongs to another window");
    }
    let script = format!(
        "(function(){{const el=host.__tb_webdriver_element({remote});\
         if(el===null)return false;host.__tbWebDriverSendKeys(el, {});return true;}})()",
        json!(text)
    );
    match window.tab.execute_browser_script(&script).await {
        Ok(RemoteValue::Bool(true)) => ok(Value::Null),
        Ok(_) => error(404, "no such element", "unknown element id"),
        Err(err) => script_error(&err),
    }
}

// https://w3c.github.io/permissions/#webdriver-command-set-permission
fn set_permission(sessions: &mut Sessions, session: &str, body: &str) -> (u16, Value) {
    let parsed: Value = match serde_json::from_str(body) {
        Ok(value) => value,
        Err(err) => return error(400, "invalid argument", &err.to_string()),
    };
    let Some(name) = parsed
        .get("descriptor")
        .and_then(|descriptor| descriptor.get("name"))
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
    else {
        return error(400, "invalid argument", "descriptor.name is required");
    };
    let Some(state) = parsed.get("state").and_then(Value::as_str) else {
        return error(400, "invalid argument", "state is required");
    };
    if !matches!(state, "granted" | "denied" | "prompt") {
        return error(400, "invalid argument", "unknown permission state");
    }
    let Some(entry) = sessions.open.get_mut(session) else {
        return error(404, "invalid session id", session);
    };
    entry.permissions.insert(name.to_owned(), state.to_owned());
    ok(Value::Null)
}

// https://w3c.github.io/webdriver/#perform-actions
async fn perform_actions(sessions: &Sessions, session: &str, body: &str) -> (u16, Value) {
    let parsed: Value = match serde_json::from_str(body) {
        Ok(value) => value,
        Err(err) => return error(400, "invalid argument", &err.to_string()),
    };
    let Some(sources) = parsed.get("actions").and_then(Value::as_array) else {
        return error(400, "invalid argument", "actions is required");
    };
    let Some(window) = current(sessions, session) else {
        return error(404, "invalid session id", session);
    };
    let mut resolved = Vec::with_capacity(sources.len());
    for source in sources {
        let mut source = source.clone();
        if let Some(items) = source.get_mut("actions").and_then(Value::as_array_mut) {
            for item in items {
                let Some(origin) = item.get_mut("origin") else {
                    continue;
                };
                let Some(element) = origin.get(ELEMENT_KEY).and_then(Value::as_str) else {
                    continue;
                };
                let (handle, remote) = match element_remote(element) {
                    Ok(remote) => remote,
                    Err(reply) => return reply,
                };
                if !is_current_handle(sessions, session, handle) {
                    return error(404, "no such element", "element belongs to another window");
                }
                *origin = json!({"__tbRemote": remote});
            }
        }
        resolved.push(source);
    }
    let script = format!(
        "(function(){{return host.__tbWebDriverActions({});}})()",
        serde_json::to_string(&Value::Array(resolved)).unwrap_or_else(|_| "[]".to_owned())
    );
    match window.tab.execute_browser_script(&script).await {
        Ok(RemoteValue::Bool(true)) => ok(Value::Null),
        Ok(_) => error(500, "unknown error", "actions were not performed"),
        Err(err) => script_error(&err),
    }
}

async fn context_url(sessions: &Sessions, session: &str) -> Result<url::Url, (u16, Value)> {
    let Some(window) = current(sessions, session) else {
        return Err(error(404, "invalid session id", session));
    };
    let spec = window
        .tab
        .document_url()
        .await
        .map_err(|err| error(500, "unknown error", &err.to_string()))?;
    url::Url::parse(&spec).map_err(|err| error(500, "unknown error", &err.to_string()))
}

// https://w3c.github.io/webdriver/#cookie
fn cookie_json(record: &CookieRecord) -> Value {
    let mut map = serde_json::Map::new();
    map.insert("name".into(), json!(record.name));
    map.insert("value".into(), json!(record.value));
    map.insert("path".into(), json!(record.path));
    map.insert("domain".into(), json!(record.domain));
    map.insert("secure".into(), json!(record.secure));
    map.insert("httpOnly".into(), json!(record.http_only));
    let same_site = match record.same_site {
        CookieSameSite::Strict => "Strict",
        CookieSameSite::Lax => "Lax",
        // https://w3c.github.io/webdriver/#cookies
        CookieSameSite::None | CookieSameSite::Default => "None",
    };
    map.insert("sameSite".into(), json!(same_site));
    if let Some(expiry) = record.expiry
        && let Ok(since) = expiry.duration_since(std::time::UNIX_EPOCH)
    {
        map.insert("expiry".into(), json!(since.as_secs()));
    }
    Value::Object(map)
}

// https://w3c.github.io/webdriver/#get-all-cookies
async fn get_cookies(sessions: &Sessions, session: &str) -> (u16, Value) {
    let url = match context_url(sessions, session).await {
        Ok(url) => url,
        Err(reply) => return reply,
    };
    match sessions.browser.cookie_records(&url).await {
        Ok(records) => ok(json!(records.iter().map(cookie_json).collect::<Vec<_>>())),
        Err(err) => error(500, "unknown error", &err.to_string()),
    }
}

// https://w3c.github.io/webdriver/#get-named-cookie
async fn get_named_cookie(sessions: &Sessions, session: &str, name: &str) -> (u16, Value) {
    let url = match context_url(sessions, session).await {
        Ok(url) => url,
        Err(reply) => return reply,
    };
    match sessions.browser.cookie_records(&url).await {
        Ok(records) => match records.iter().find(|record| record.name == name) {
            Some(record) => ok(cookie_json(record)),
            None => error(404, "no such cookie", "no cookie with that name"),
        },
        Err(err) => error(500, "unknown error", &err.to_string()),
    }
}

// https://w3c.github.io/webdriver/#dfn-adding-a-cookie
fn set_cookie_line(cookie: &Value) -> Result<String, (u16, Value)> {
    let invalid = |message| error(400, "invalid argument", message);
    let Some(name) = cookie.get("name").and_then(Value::as_str) else {
        return Err(invalid("cookie name must be a string"));
    };
    let Some(value) = cookie.get("value").and_then(Value::as_str) else {
        return Err(invalid("cookie value must be a string"));
    };
    let mut line = format!("{name}={value}");
    match cookie.get("path") {
        Some(path) => {
            let path = path
                .as_str()
                .ok_or_else(|| invalid("cookie path must be a string"))?;
            line.push_str("; Path=");
            line.push_str(path);
        }
        // https://w3c.github.io/webdriver/#dfn-table-for-cookie-conversion
        None => line.push_str("; Path=/"),
    }
    if let Some(domain) = cookie.get("domain") {
        let domain = domain
            .as_str()
            .ok_or_else(|| invalid("cookie domain must be a string"))?;
        line.push_str("; Domain=");
        line.push_str(domain);
    }
    for (key, attribute) in [("secure", "; Secure"), ("httpOnly", "; HttpOnly")] {
        if let Some(flag) = cookie.get(key)
            && flag
                .as_bool()
                .ok_or_else(|| invalid("cookie flags must be booleans"))?
        {
            line.push_str(attribute);
        }
    }
    if let Some(same_site) = cookie.get("sameSite") {
        let same_site = same_site
            .as_str()
            .ok_or_else(|| invalid("cookie sameSite must be a string"))?;
        if !matches!(same_site, "None" | "Lax" | "Strict") {
            return Err(invalid("cookie sameSite must be None, Lax, or Strict"));
        }
        line.push_str("; SameSite=");
        line.push_str(same_site);
    }
    if let Some(expiry) = cookie.get("expiry") {
        let expiry = expiry
            .as_u64()
            .ok_or_else(|| invalid("cookie expiry must be a non-negative integer"))?;
        if expiry > 9_007_199_254_740_991 {
            return Err(invalid("cookie expiry is out of range"));
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        line.push_str("; Max-Age=");
        line.push_str(&expiry.saturating_sub(now).to_string());
    }
    Ok(line)
}

// https://w3c.github.io/webdriver/#add-cookie
async fn add_cookie(sessions: &Sessions, session: &str, body: &str) -> (u16, Value) {
    if current(sessions, session).is_none() {
        return error(404, "invalid session id", session);
    }
    let parsed: Value = match serde_json::from_str(body) {
        Ok(value) => value,
        Err(err) => return error(400, "invalid argument", &err.to_string()),
    };
    let Some(cookie) = parsed.get("cookie") else {
        return error(400, "invalid argument", "cookie is required");
    };
    let line = match set_cookie_line(cookie) {
        Ok(line) => line,
        Err(reply) => return reply,
    };
    let url = match context_url(sessions, session).await {
        Ok(url) => url,
        Err(reply) => return reply,
    };
    // https://w3c.github.io/webdriver/#add-cookie
    if let Some(domain) = cookie.get("domain").and_then(Value::as_str) {
        // https://w3c.github.io/webdriver/#add-cookie
        let domain = domain.strip_prefix('.').unwrap_or(domain);
        let host = url.host_str().unwrap_or_default();
        if !host.eq_ignore_ascii_case(domain)
            && !host
                .to_ascii_lowercase()
                .ends_with(&format!(".{}", domain.to_ascii_lowercase()))
        {
            return error(
                400,
                "invalid cookie domain",
                "cookie domain does not match the current document",
            );
        }
    }
    match sessions.browser.add_cookie(&line, &url).await {
        Ok(true) => ok(Value::Null),
        Ok(false) => error(500, "unable to set cookie", "the cookie was rejected"),
        Err(err) => error(500, "unknown error", &err.to_string()),
    }
}

// https://w3c.github.io/webdriver/#delete-all-cookies — empties the profile-wide cookie jar, matching Chrome's browser-scoped clearBrowsingData
async fn delete_cookies(sessions: &Sessions, session: &str) -> (u16, Value) {
    if current(sessions, session).is_none() {
        return error(404, "invalid session id", session);
    }
    match sessions.browser.clear_cookies().await {
        Ok(()) => ok(Value::Null),
        Err(err) => error(500, "unknown error", &err.to_string()),
    }
}

fn rect_number(value: f64) -> Value {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "rect values are validated into the i32 range before storage"
    )]
    if value.fract() == 0.0 {
        json!(value as i64)
    } else {
        json!(value)
    }
}

fn session_rect(found: &Session) -> Value {
    json!({
        "x": rect_number(found.window_rect[0]),
        "y": rect_number(found.window_rect[1]),
        "width": rect_number(found.window_rect[2]),
        "height": rect_number(found.window_rect[3]),
    })
}

// https://w3c.github.io/webdriver/#get-window-rect
fn window_rect(sessions: &Sessions, session: &str) -> (u16, Value) {
    match sessions.open.get(session) {
        Some(found) => ok(session_rect(found)),
        None => error(404, "invalid session id", session),
    }
}

fn window_op(sessions: &Sessions, session: &str) -> (u16, Value) {
    window_rect(sessions, session)
}

// https://w3c.github.io/webdriver/#set-window-rect
fn set_window_rect(sessions: &mut Sessions, session: &str, body: &str) -> (u16, Value) {
    let Some(found) = sessions.open.get_mut(session) else {
        return error(404, "invalid session id", session);
    };
    let parsed: Value = match serde_json::from_str(body) {
        Ok(value) => value,
        Err(err) => return error(400, "invalid argument", &err.to_string()),
    };
    // https://w3c.github.io/webdriver/#set-window-rect
    let mut updated = found.window_rect;
    for (key, index) in [("x", 0), ("y", 1), ("width", 2), ("height", 3)] {
        if let Some(value) = parsed.get(key) {
            let Some(number) = value.as_f64() else {
                return error(
                    400,
                    "invalid argument",
                    "window rect values must be numbers",
                );
            };
            let valid = if index < 2 {
                (-2_147_483_648.0..=2_147_483_647.0).contains(&number)
            } else {
                (0.0..=2_147_483_647.0).contains(&number)
            };
            if !valid {
                return error(400, "invalid argument", "window rect value is out of range");
            }
            updated[index] = number;
        }
    }
    found.window_rect = updated;
    ok(session_rect(found))
}

// https://w3c.github.io/webdriver/#get-element-rect
async fn element_rect(sessions: &Sessions, session: &str, element: &str) -> (u16, Value) {
    let Some(window) = current(sessions, session) else {
        return error(404, "invalid session id", session);
    };
    let (handle, remote) = match element_remote(element) {
        Ok(remote) => remote,
        Err(reply) => return reply,
    };
    if !is_current_handle(sessions, session, handle) {
        return error(404, "no such element", "element belongs to another window");
    }
    let script = format!(
        "(function(){{const el=host.__tb_webdriver_element({remote});\
         if(el===null)return null;return JSON.stringify(el.getBoundingClientRect());}})()"
    );
    match window.tab.execute_browser_script(&script).await {
        Ok(RemoteValue::String(text)) => match serde_json::from_str::<Value>(&text) {
            Ok(rect) => ok(json!({
                "x": rect.get("x").cloned().unwrap_or(json!(0)),
                "y": rect.get("y").cloned().unwrap_or(json!(0)),
                "width": rect.get("width").cloned().unwrap_or(json!(0)),
                "height": rect.get("height").cloned().unwrap_or(json!(0)),
            })),
            Err(err) => error(500, "unknown error", &err.to_string()),
        },
        Ok(_) => error(404, "no such element", "unknown element id"),
        Err(err) => script_error(&err),
    }
}

fn get_timeouts(sessions: &Sessions, session: &str) -> (u16, Value) {
    match sessions.open.get(session) {
        Some(found) => ok(json!({
            "implicit": duration_millis(found.implicit_timeout),
            "pageLoad": duration_millis(found.page_load_timeout),
            "script": duration_millis(found.script_timeout),
        })),
        None => error(404, "invalid session id", session),
    }
}

fn set_timeouts(sessions: &mut Sessions, session: &str, body: &str) -> (u16, Value) {
    let parsed: Value = match serde_json::from_str(body) {
        Ok(value) => value,
        Err(err) => return error(400, "invalid argument", &err.to_string()),
    };
    let Some(found) = sessions.open.get_mut(session) else {
        return error(404, "invalid session id", session);
    };
    if let Some(ms) = parsed.get("implicit").and_then(json_millis) {
        found.implicit_timeout = Duration::from_millis(ms);
    }
    if let Some(ms) = parsed.get("script").and_then(json_millis) {
        found.script_timeout = Duration::from_millis(ms);
    }
    if let Some(ms) = parsed.get("pageLoad").and_then(json_millis) {
        found.page_load_timeout = Duration::from_millis(ms);
    }
    ok(Value::Null)
}

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn json_millis(value: &Value) -> Option<u64> {
    match value {
        Value::Number(number) => {
            if let Some(ms) = number.as_u64() {
                Some(ms)
            } else if let Some(ms) = number.as_i64() {
                u64::try_from(ms).ok()
            } else {
                let ms = number.as_f64()?;
                if !ms.is_finite() || ms < 0.0 {
                    return None;
                }
                let duration = Duration::try_from_secs_f64(ms / 1000.0).ok()?;
                u64::try_from(duration.as_millis()).ok()
            }
        }
        _ => None,
    }
}

fn current_window(sessions: &Sessions, session: &str) -> (u16, Value) {
    match sessions.open.get(session) {
        Some(found) => ok(json!(found.current)),
        None => error(404, "invalid session id", session),
    }
}

fn window_handles(sessions: &Sessions, session: &str) -> (u16, Value) {
    match sessions.open.get(session) {
        Some(found) => ok(json!(found.windows.keys().cloned().collect::<Vec<_>>())),
        None => error(404, "invalid session id", session),
    }
}

fn switch_window(sessions: &mut Sessions, session: &str, body: &str) -> (u16, Value) {
    let handle = serde_json::from_str::<Value>(body).ok().and_then(|value| {
        value
            .get("handle")
            .and_then(Value::as_str)
            .map(str::to_owned)
    });
    let Some(handle) = handle else {
        return error(400, "invalid argument", "missing handle");
    };
    let Some(found) = sessions.open.get_mut(session) else {
        return error(404, "invalid session id", session);
    };
    if !found.windows.contains_key(&handle) {
        return error(404, "no such window", &handle);
    }
    found.current = handle;
    ok(Value::Null)
}

async fn new_window(sessions: &mut Sessions, session: &str) -> (u16, Value) {
    if !sessions.open.contains_key(session) {
        return error(404, "invalid session id", session);
    }
    sessions.next_window += 1;
    let handle = format!("w{}", sessions.next_window);
    let window = match sessions.blank_window().await {
        Ok(window) => window,
        Err(err) => return error(500, "unknown error", &err),
    };
    let Some(found) = sessions.open.get_mut(session) else {
        return error(404, "invalid session id", session);
    };
    found.windows.insert(handle.clone(), window);
    ok(json!({"handle": handle, "type": "window"}))
}

// https://w3c.github.io/webdriver/#delete-session
async fn delete_session(sessions: &mut Sessions, session: &str) -> (u16, Value) {
    let Some(found) = sessions.open.remove(session) else {
        return ok(Value::Null);
    };
    for window in found.windows.into_values() {
        let _result = sessions.browser.close_tab(window.tab.id()).await;
    }
    ok(Value::Null)
}

// https://w3c.github.io/webdriver/#close-window
async fn close_window(sessions: &mut Sessions, session: &str) -> (u16, Value) {
    let (tab_id, remaining) = {
        let Some(found) = sessions.open.get_mut(session) else {
            return error(404, "invalid session id", session);
        };
        let Some(window) = found.windows.remove(&found.current) else {
            return error(404, "no such window", &found.current);
        };
        let tab_id = window.tab.id();
        let remaining: Vec<String> = found.windows.keys().cloned().collect();
        if let Some(next) = remaining.first() {
            found.current.clone_from(next);
        }
        (tab_id, remaining)
    };
    let _result = sessions.browser.close_tab(tab_id).await;
    if remaining.is_empty() {
        sessions.open.remove(session);
    }
    ok(json!(remaining))
}

fn current_handle(sessions: &Sessions, session: &str) -> Option<String> {
    sessions
        .open
        .get(session)
        .map(|found| found.current.clone())
}

fn current<'a>(sessions: &'a Sessions, session: &str) -> Option<&'a Window> {
    sessions
        .open
        .get(session)
        .and_then(|found| found.windows.get(&found.current))
}

fn ok(value: Value) -> (u16, Value) {
    (
        200,
        Value::Object(serde_json::Map::from_iter([("value".into(), value)])),
    )
}

fn error(status: u16, err: &str, message: &str) -> (u16, Value) {
    (
        status,
        json!({"value": {"error": err, "message": message, "stacktrace": ""}}),
    )
}
