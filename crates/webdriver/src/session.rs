// https://w3c.github.io/webdriver/#sessions
// https://w3c.github.io/webdriver-bidi/#module-session

use std::collections::HashMap;
use std::io::Read as _;
use std::net::SocketAddr;
use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use browser::{
    AutomationSession, BrowserHandle, FrameId, PromptBehavior, PromptBehaviorMap, TabHandle, TabId,
    UserContextId,
};
use serde_json::{Map, Value, json};
use server::Shutdown;

pub(crate) const DEFAULT_SCRIPT_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const DEFAULT_PAGE_LOAD_TIMEOUT: Duration = Duration::from_secs(300);
pub(crate) const MAX_SAFE_INT: u64 = 9_007_199_254_740_991;
pub(crate) const MAX_UPLOAD_FILE_BYTES: u64 = 64 * 1024 * 1024;

pub(crate) fn is_safe_i64(n: i64) -> bool {
    n.unsigned_abs() <= MAX_SAFE_INT
}

pub(crate) async fn read_upload_files(paths: Vec<String>) -> Result<Vec<Value>, String> {
    tokio::task::spawn_blocking(move || {
        let mut files = Vec::with_capacity(paths.len());
        for path in paths {
            let metadata =
                std::fs::metadata(&path).map_err(|_| "file path does not exist".to_owned())?;
            if !metadata.is_file() {
                return Err("file path is not a file".to_owned());
            }
            let file =
                std::fs::File::open(&path).map_err(|_| "file path cannot be read".to_owned())?;
            let mut bytes = Vec::new();
            file.take(MAX_UPLOAD_FILE_BYTES + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| "file path cannot be read".to_owned())?;
            if bytes.len() as u64 > MAX_UPLOAD_FILE_BYTES {
                return Err("file exceeds maximum upload size".to_owned());
            }
            let name = Path::new(&path)
                .file_name()
                .map_or_else(|| path.clone(), |name| name.to_string_lossy().into_owned());
            let last_modified = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |duration| {
                    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
                });
            files.push(json!({
                "name": name,
                "path": path,
                "data": BASE64_STANDARD.encode(&bytes),
                "type": "",
                "lastModified": last_modified,
            }));
        }
        Ok(files)
    })
    .await
    .map_err(|err| format!("upload reader failed: {err}"))?
}

// https://w3c.github.io/webdriver/#dfn-page-load-strategy
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum PageLoadStrategy {
    None,
    Eager,
    #[default]
    Normal,
}

impl PageLoadStrategy {
    pub(crate) fn parse(raw: &str) -> Option<Self> {
        match raw {
            "none" => Some(Self::None),
            "eager" => Some(Self::Eager),
            "normal" => Some(Self::Normal),
            _ => None,
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Eager => "eager",
            Self::Normal => "normal",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct MatchedCapabilities {
    pub(crate) accept_insecure_certs: bool,
    pub(crate) page_load_strategy: PageLoadStrategy,
    pub(crate) strict_file_interactability: bool,
    pub(crate) implicit_timeout: Option<Duration>,
    pub(crate) page_load_timeout: Option<Duration>,
    pub(crate) script_timeout: Option<Duration>,
    pub(crate) prompt_behavior: PromptBehaviorMap,
    pub(crate) prompt_behavior_json: Value,
    pub(crate) web_socket_url: bool,
}

impl Default for MatchedCapabilities {
    fn default() -> Self {
        Self {
            accept_insecure_certs: false,
            page_load_strategy: PageLoadStrategy::Normal,
            strict_file_interactability: false,
            implicit_timeout: Some(Duration::ZERO),
            page_load_timeout: Some(DEFAULT_PAGE_LOAD_TIMEOUT),
            script_timeout: Some(DEFAULT_SCRIPT_TIMEOUT),
            prompt_behavior: PromptBehaviorMap::uniform(PromptBehavior::DismissAndNotify),
            prompt_behavior_json: Value::String("dismiss and notify".to_owned()),
            web_socket_url: false,
        }
    }
}

impl MatchedCapabilities {
    pub(crate) fn to_json(&self, session_id: &str, bound: Option<SocketAddr>) -> Value {
        let mut map = Map::new();
        map.insert("browserName".into(), json!("tinybrowser"));
        map.insert("browserVersion".into(), json!(env!("CARGO_PKG_VERSION")));
        map.insert("platformName".into(), json!(platform_name()));
        map.insert(
            "acceptInsecureCerts".into(),
            json!(self.accept_insecure_certs),
        );
        map.insert(
            "pageLoadStrategy".into(),
            json!(self.page_load_strategy.as_str()),
        );
        map.insert("proxy".into(), json!({}));
        map.insert("setWindowRect".into(), json!(true));
        map.insert(
            "strictFileInteractability".into(),
            json!(self.strict_file_interactability),
        );
        map.insert(
            "timeouts".into(),
            json!({
                "implicit": self.implicit_timeout.map(duration_millis),
                "pageLoad": self.page_load_timeout.map(duration_millis),
                "script": self.script_timeout.map(duration_millis),
            }),
        );
        map.insert(
            "unhandledPromptBehavior".into(),
            self.prompt_behavior_json.clone(),
        );
        if self.web_socket_url
            && let Some(addr) = bound
        {
            map.insert(
                "webSocketUrl".into(),
                json!(format!("ws://{addr}/session/{session_id}")),
            );
        }
        Value::Object(map)
    }
}

#[must_use]
pub(crate) fn platform_name() -> &'static str {
    match std::env::consts::OS {
        "macos" => "mac",
        "windows" => "windows",
        other => other,
    }
}

#[derive(Clone, Debug)]
pub(crate) enum CapabilityError {
    InvalidArgument(String),
    SessionNotCreated(String),
}

// https://w3c.github.io/webdriver/#processing-capabilities
pub(crate) fn process_capabilities(body: &Value) -> Result<MatchedCapabilities, CapabilityError> {
    let Some(obj) = body.as_object() else {
        return Err(CapabilityError::InvalidArgument(
            "parameters must be an object".into(),
        ));
    };
    let Some(caps_val) = obj.get("capabilities") else {
        if obj.is_empty() {
            return Ok(MatchedCapabilities::default());
        }
        return Err(CapabilityError::InvalidArgument(
            "capabilities is required".into(),
        ));
    };
    let Some(caps) = caps_val.as_object() else {
        return Err(CapabilityError::InvalidArgument(
            "capabilities must be an object".into(),
        ));
    };
    let always = match caps.get("alwaysMatch") {
        None => Map::new(),
        Some(value) => validate_capabilities_object(value)?,
    };
    let first_list: Vec<Map<String, Value>> = match caps.get("firstMatch") {
        None => vec![Map::new()],
        Some(Value::Array(items)) if !items.is_empty() => {
            let mut parsed = Vec::with_capacity(items.len());
            for item in items {
                parsed.push(validate_capabilities_object(item)?);
            }
            parsed
        }
        Some(_) => {
            return Err(CapabilityError::InvalidArgument(
                "firstMatch must be a non-empty array".into(),
            ));
        }
    };

    let mut merged_list = Vec::with_capacity(first_list.len());
    for first in first_list {
        let mut merged = always.clone();
        for (key, value) in first {
            if merged.contains_key(&key) {
                return Err(CapabilityError::InvalidArgument(format!(
                    "capability '{key}' appears in both alwaysMatch and firstMatch"
                )));
            }
            merged.insert(key, value);
        }
        merged_list.push(merged);
    }

    for merged in merged_list {
        if let Some(matched) = match_capabilities(&merged)? {
            return Ok(matched);
        }
    }
    Err(CapabilityError::SessionNotCreated(
        "no matching capabilities".into(),
    ))
}

fn validate_capabilities_object(value: &Value) -> Result<Map<String, Value>, CapabilityError> {
    let Some(map) = value.as_object() else {
        return Err(CapabilityError::InvalidArgument(
            "capabilities entry must be an object".into(),
        ));
    };
    let mut validated = Map::new();
    for (key, val) in map {
        if val.is_null() {
            continue;
        }
        match key.as_str() {
            "browserName" | "browserVersion" | "platformName" => {
                if !val.is_string() {
                    return Err(CapabilityError::InvalidArgument(format!(
                        "{key} must be a string"
                    )));
                }
            }
            "acceptInsecureCerts" | "strictFileInteractability" | "webSocketUrl" => {
                if !val.is_boolean() {
                    return Err(CapabilityError::InvalidArgument(format!(
                        "{key} must be a boolean"
                    )));
                }
            }
            "pageLoadStrategy" => {
                let Some(raw) = val.as_str() else {
                    return Err(CapabilityError::InvalidArgument(
                        "pageLoadStrategy must be a string".into(),
                    ));
                };
                if PageLoadStrategy::parse(raw).is_none() {
                    return Err(CapabilityError::InvalidArgument(format!(
                        "invalid pageLoadStrategy: {raw}"
                    )));
                }
            }
            "timeouts" => {
                let _ = parse_timeouts_object(val)?;
            }
            "unhandledPromptBehavior" => {
                let _ = parse_prompt_behavior(val)?;
            }
            "proxy" => {
                validate_proxy(val)?;
            }
            other if other.contains(':') => {}
            other => {
                return Err(CapabilityError::InvalidArgument(format!(
                    "unknown capability: {other}"
                )));
            }
        }
        validated.insert(key.clone(), val.clone());
    }
    Ok(validated)
}

fn validate_proxy(value: &Value) -> Result<(), CapabilityError> {
    let Some(obj) = value.as_object() else {
        return Err(CapabilityError::InvalidArgument(
            "proxy must be an object".into(),
        ));
    };
    if obj.is_empty() {
        return Ok(());
    }
    validate_proxy_object(value)
}

// https://w3c.github.io/webdriver/#proxy
// https://w3c.github.io/webdriver-bidi/#type-session-ProxyConfiguration
pub(crate) fn validate_proxy_object(value: &Value) -> Result<(), CapabilityError> {
    let Some(obj) = value.as_object() else {
        return Err(CapabilityError::InvalidArgument(
            "proxy must be an object".into(),
        ));
    };
    let Some(proxy_type) = obj.get("proxyType").and_then(Value::as_str) else {
        return Err(CapabilityError::InvalidArgument(
            "proxy.proxyType must be a string".into(),
        ));
    };
    match proxy_type {
        "direct" | "autodetect" | "system" => Ok(()),
        "pac" => {
            if obj
                .get("proxyAutoconfigUrl")
                .and_then(Value::as_str)
                .is_none()
            {
                return Err(CapabilityError::InvalidArgument(
                    "proxyAutoconfigUrl must be a string".into(),
                ));
            }
            Ok(())
        }
        "manual" => {
            for field in ["httpProxy", "sslProxy"] {
                if let Some(val) = obj.get(field) {
                    let Some(host) = val.as_str() else {
                        return Err(CapabilityError::InvalidArgument(format!(
                            "{field} must be a string"
                        )));
                    };
                    if !is_valid_proxy_host(host) {
                        return Err(CapabilityError::InvalidArgument(format!(
                            "invalid {field}: {host}"
                        )));
                    }
                }
            }
            let has_socks = obj.contains_key("socksProxy");
            let has_version = obj.contains_key("socksVersion");
            if has_socks != has_version {
                return Err(CapabilityError::InvalidArgument(
                    "socksProxy and socksVersion must be provided together".into(),
                ));
            }
            if let Some(val) = obj.get("socksProxy") {
                let Some(host) = val.as_str() else {
                    return Err(CapabilityError::InvalidArgument(
                        "socksProxy must be a string".into(),
                    ));
                };
                if !is_valid_proxy_host(host) {
                    return Err(CapabilityError::InvalidArgument(format!(
                        "invalid socksProxy: {host}"
                    )));
                }
            }
            if let Some(val) = obj.get("socksVersion")
                && val.as_u64().is_none_or(|v| v > 255)
            {
                return Err(CapabilityError::InvalidArgument(
                    "socksVersion must be an integer between 0 and 255".into(),
                ));
            }
            if let Some(val) = obj.get("noProxy") {
                let Some(arr) = val.as_array() else {
                    return Err(CapabilityError::InvalidArgument(
                        "noProxy must be an array of strings".into(),
                    ));
                };
                if !arr.iter().all(Value::is_string) {
                    return Err(CapabilityError::InvalidArgument(
                        "noProxy entries must be strings".into(),
                    ));
                }
            }
            Ok(())
        }
        other => Err(CapabilityError::InvalidArgument(format!(
            "invalid proxyType: {other}"
        ))),
    }
}

fn is_valid_proxy_host(raw: &str) -> bool {
    if raw.is_empty()
        || raw.contains("://")
        || raw.contains('/')
        || raw.contains('?')
        || raw.contains('#')
        || raw.contains('@')
    {
        return false;
    }
    url::Url::parse(&format!("http://{raw}")).is_ok()
}

fn match_capabilities(
    merged: &Map<String, Value>,
) -> Result<Option<MatchedCapabilities>, CapabilityError> {
    let mut out = MatchedCapabilities::default();
    if let Some(name) = merged.get("browserName").and_then(Value::as_str)
        && name != "tinybrowser"
    {
        return Ok(None);
    }
    if let Some(version) = merged.get("browserVersion").and_then(Value::as_str)
        && !env!("CARGO_PKG_VERSION").starts_with(version)
    {
        return Ok(None);
    }
    if let Some(platform) = merged.get("platformName").and_then(Value::as_str)
        && !platform.eq_ignore_ascii_case(platform_name())
    {
        return Ok(None);
    }
    if let Some(accept) = merged.get("acceptInsecureCerts").and_then(Value::as_bool) {
        out.accept_insecure_certs = accept;
    }
    if let Some(strict) = merged
        .get("strictFileInteractability")
        .and_then(Value::as_bool)
    {
        out.strict_file_interactability = strict;
    }
    if let Some(ws) = merged.get("webSocketUrl").and_then(Value::as_bool) {
        out.web_socket_url = ws;
    }
    if let Some(strategy) = merged
        .get("pageLoadStrategy")
        .and_then(Value::as_str)
        .and_then(PageLoadStrategy::parse)
    {
        out.page_load_strategy = strategy;
    }
    if let Some(timeouts) = merged.get("timeouts") {
        let parsed = parse_timeouts_object(timeouts)?;
        parsed.implicit.apply(&mut out.implicit_timeout);
        parsed.page_load.apply(&mut out.page_load_timeout);
        parsed.script.apply(&mut out.script_timeout);
    }
    if let Some(behavior) = merged.get("unhandledPromptBehavior") {
        let (map, canonical) = parse_prompt_behavior(behavior)?;
        out.prompt_behavior = map;
        out.prompt_behavior_json = canonical;
    }
    Ok(Some(out))
}

#[derive(Clone, Copy, Default)]
pub(crate) enum TimeoutPatch {
    #[default]
    Unchanged,
    Clear,
    Set(Duration),
}

impl TimeoutPatch {
    pub(crate) fn apply(self, target: &mut Option<Duration>) {
        match self {
            Self::Unchanged => {}
            Self::Clear => *target = None,
            Self::Set(duration) => *target = Some(duration),
        }
    }
}

#[derive(Default)]
pub(crate) struct ParsedTimeouts {
    pub(crate) implicit: TimeoutPatch,
    pub(crate) page_load: TimeoutPatch,
    pub(crate) script: TimeoutPatch,
}

// https://w3c.github.io/webdriver/#set-timeouts
pub(crate) fn parse_timeouts_object(value: &Value) -> Result<ParsedTimeouts, CapabilityError> {
    let Some(obj) = value.as_object() else {
        return Err(CapabilityError::InvalidArgument(
            "timeouts must be an object".into(),
        ));
    };
    let mut parsed = ParsedTimeouts::default();
    for (key, val) in obj {
        match key.as_str() {
            "implicit" => {
                parsed.implicit = if val.is_null() {
                    TimeoutPatch::Clear
                } else {
                    TimeoutPatch::Set(Duration::from_millis(parse_timeout_ms(val)?))
                };
            }
            "pageLoad" => {
                parsed.page_load = if val.is_null() {
                    TimeoutPatch::Clear
                } else {
                    TimeoutPatch::Set(Duration::from_millis(parse_timeout_ms(val)?))
                };
            }
            "script" => {
                parsed.script = if val.is_null() {
                    TimeoutPatch::Clear
                } else {
                    TimeoutPatch::Set(Duration::from_millis(parse_timeout_ms(val)?))
                };
            }
            _ => {}
        }
    }
    Ok(parsed)
}

fn parse_timeout_ms(value: &Value) -> Result<u64, CapabilityError> {
    if let Some(number) = value.as_u64() {
        if number > MAX_SAFE_INT {
            return Err(CapabilityError::InvalidArgument(
                "timeout exceeds max safe integer".into(),
            ));
        }
        return Ok(number);
    }
    if let Some(float) = value.as_f64() {
        #[expect(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "MAX_SAFE_INT (2^53 - 1) is exactly representable in f64"
        )]
        if float.is_finite() && float >= 0.0 && float.fract() == 0.0 && float <= MAX_SAFE_INT as f64
        {
            return Ok(float as u64);
        }
    }
    Err(CapabilityError::InvalidArgument(
        "timeout must be a non-negative integer".into(),
    ))
}

// https://w3c.github.io/webdriver-bidi/#type-session-UserPromptHandler
pub(crate) fn parse_bidi_prompt_handler(
    value: &Value,
) -> Result<PromptBehaviorMap, CapabilityError> {
    if !value.is_object() {
        return Err(CapabilityError::InvalidArgument(
            "unhandledPromptBehavior must be an object".into(),
        ));
    }
    let (map, _) = parse_prompt_behavior(value)?;
    Ok(map)
}

// https://w3c.github.io/webdriver/#dfn-unhandled-prompt-behavior
// https://w3c.github.io/webdriver-bidi/#type-session-UserPromptHandler
pub(crate) fn parse_prompt_behavior(
    value: &Value,
) -> Result<(PromptBehaviorMap, Value), CapabilityError> {
    if let Some(raw) = value.as_str() {
        let behavior = match raw {
            "dismiss" => PromptBehavior::Dismiss,
            "accept" => PromptBehavior::Accept,
            "dismiss and notify" => PromptBehavior::DismissAndNotify,
            "accept and notify" => PromptBehavior::AcceptAndNotify,
            "ignore" => PromptBehavior::Ignore,
            other => {
                return Err(CapabilityError::InvalidArgument(format!(
                    "invalid unhandledPromptBehavior: {other}"
                )));
            }
        };
        return Ok((
            PromptBehaviorMap::uniform(behavior),
            Value::String(raw.to_owned()),
        ));
    }
    let Some(obj) = value.as_object() else {
        return Err(CapabilityError::InvalidArgument(
            "unhandledPromptBehavior must be a string or object".into(),
        ));
    };
    let mut map = PromptBehaviorMap::default();
    let mut canonical = Map::new();
    for (key, val) in obj {
        let Some(handler_str) = val.as_str() else {
            return Err(CapabilityError::InvalidArgument(format!(
                "prompt handler for '{key}' must be a string"
            )));
        };
        let behavior = match handler_str {
            "accept" => PromptBehavior::Accept,
            "accept and notify" => PromptBehavior::AcceptAndNotify,
            "dismiss" => PromptBehavior::Dismiss,
            "dismiss and notify" => PromptBehavior::DismissAndNotify,
            "ignore" => PromptBehavior::Ignore,
            other => {
                return Err(CapabilityError::InvalidArgument(format!(
                    "invalid prompt handler: {other}"
                )));
            }
        };
        match key.as_str() {
            "alert" => map.alert = Some(behavior),
            "confirm" => map.confirm = Some(behavior),
            "prompt" => map.prompt = Some(behavior),
            "default" => map.default = Some(behavior),
            "beforeUnload" | "file" => {}
            other => {
                return Err(CapabilityError::InvalidArgument(format!(
                    "unknown prompt type: {other}"
                )));
            }
        }
        canonical.insert(key.clone(), Value::String(handler_str.to_owned()));
    }
    Ok((map, Value::Object(canonical)))
}

#[must_use]
pub(crate) fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

// https://w3c.github.io/webdriver/#new-session
pub(crate) fn random_id() -> Result<String, String> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| "failed to generate id".to_owned())?;
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

pub(crate) struct Window {
    pub(crate) tab: TabHandle,
    pub(crate) order: u64,
}

pub(crate) struct Session {
    pub(crate) user_context: UserContextId,
    pub(crate) owns_user_context: bool,
    pub(crate) automation: AutomationSession,
    pub(crate) current: String,
    pub(crate) current_frame: FrameId,
    pub(crate) windows: HashMap<String, Window>,
    pub(crate) implicit_timeout: Option<Duration>,
    pub(crate) script_timeout: Option<Duration>,
    pub(crate) page_load_timeout: Option<Duration>,
    pub(crate) page_load_strategy: PageLoadStrategy,
    pub(crate) prompt_behavior: PromptBehaviorMap,
    pub(crate) prompt_text: Option<String>,
    pub(crate) window_rect: [f64; 4],
    pub(crate) permissions: HashMap<String, String>,
    pub(crate) preload_scripts: HashMap<String, PreloadScriptEntry>,
    pub(crate) strict_file_interactability: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct PreloadScriptEntry {
    pub(crate) function_declaration: String,
    pub(crate) contexts: Option<Vec<TabId>>,
    pub(crate) user_contexts: Option<Vec<UserContextId>>,
    pub(crate) tab_script_ids: Vec<(TabId, u64)>,
}

pub(crate) struct WindowSnapshot {
    pub(crate) browser: BrowserHandle,
    pub(crate) tab: TabHandle,
    pub(crate) handle: String,
    pub(crate) frame: FrameId,
    pub(crate) implicit_timeout: Option<Duration>,
    pub(crate) script_timeout: Option<Duration>,
    pub(crate) page_load_timeout: Option<Duration>,
    pub(crate) page_load_strategy: PageLoadStrategy,
    pub(crate) prompt_behavior: PromptBehaviorMap,
    pub(crate) prompt_text: Option<String>,
    pub(crate) strict_file_interactability: bool,
}

pub(crate) struct Sessions {
    pub(crate) browser: BrowserHandle,
    pub(crate) bound: Option<SocketAddr>,
    pub(crate) shutdown: Shutdown,
    pub(crate) next_window: u64,
    pub(crate) next_slot: u64,
    pub(crate) creating: bool,
    pub(crate) open: HashMap<String, Session>,
}

impl Sessions {
    #[must_use]
    pub(crate) fn new(
        browser: BrowserHandle,
        bound: Option<SocketAddr>,
        shutdown: Shutdown,
    ) -> Self {
        Self {
            browser,
            bound,
            shutdown,
            next_window: 0,
            next_slot: 0,
            creating: false,
            open: HashMap::new(),
        }
    }

    pub(crate) fn next_call_slot(&mut self) -> u64 {
        self.next_slot = self.next_slot.saturating_add(1);
        self.next_slot
    }

    #[must_use]
    pub(crate) fn snapshot(&self, session: &str) -> Option<WindowSnapshot> {
        let found = self.open.get(session)?;
        let window = found.windows.get(&found.current)?;
        Some(WindowSnapshot {
            browser: self.browser.clone(),
            tab: window.tab.clone(),
            handle: found.current.clone(),
            frame: found.current_frame,
            implicit_timeout: found.implicit_timeout,
            script_timeout: found.script_timeout,
            page_load_timeout: found.page_load_timeout,
            page_load_strategy: found.page_load_strategy,
            prompt_behavior: found.prompt_behavior,
            prompt_text: found.prompt_text.clone(),
            strict_file_interactability: found.strict_file_interactability,
        })
    }

    // https://w3c.github.io/webdriver/#get-window-handles
    pub(crate) fn sync_windows(&mut self, session: &str) {
        let browser = self.browser.clone();
        let Some(found) = self.open.get_mut(session) else {
            return;
        };
        let ids = if found.owns_user_context {
            browser.tabs_in(&found.user_context)
        } else {
            browser.tabs()
        };
        let Ok(ids) = ids else {
            return;
        };
        found
            .windows
            .retain(|_, window| ids.contains(&window.tab.id()));
        for id in ids {
            if found.windows.values().any(|window| window.tab.id() == id) {
                continue;
            }
            let Ok(tab) = browser.tab(id) else {
                continue;
            };
            self.next_window = self.next_window.saturating_add(1);
            let order = self.next_window;
            found
                .windows
                .insert(tab_handle_id(id), Window { tab, order });
        }
    }

    pub(crate) fn ordered_handles(&self, session: &str) -> Option<Vec<String>> {
        let found = self.open.get(session)?;
        let mut entries: Vec<(&String, u64)> = found
            .windows
            .iter()
            .map(|(handle, win)| (handle, win.order))
            .collect();
        entries.sort_by_key(|(_, order)| *order);
        Some(entries.into_iter().map(|(h, _)| h.clone()).collect())
    }
}

pub(crate) async fn blank_window(
    browser: &BrowserHandle,
    user_context: UserContextId,
) -> Result<TabHandle, String> {
    let tab = browser
        .create_tab_in(user_context)
        .await
        .map_err(|err| err.to_string())?;
    if let Err(error) = tab.load_html("<!doctype html><title></title>").await {
        let _result = browser.close_tab(tab.id()).await;
        return Err(error.to_string());
    }
    Ok(tab)
}

pub(crate) async fn status(sessions: &tokio::sync::Mutex<Sessions>) -> (bool, String) {
    let browser = {
        let guard = sessions.lock().await;
        if !guard.open.is_empty() || guard.creating {
            return (false, "session already started".to_owned());
        }
        guard.browser.clone()
    };
    match browser.renderer_health().await {
        browser::RendererState::Live
        | browser::RendererState::Starting
        | browser::RendererState::NoneLive => (true, "ready".to_owned()),
        browser::RendererState::Failed(error) => (false, error.to_string()),
    }
}

pub(crate) async fn create_session(
    sessions: &tokio::sync::Mutex<Sessions>,
    caps: MatchedCapabilities,
) -> Result<(String, Value), String> {
    let (browser, bound) = {
        let mut guard = sessions.lock().await;
        if !guard.open.is_empty() || guard.creating {
            return Err("already have a session".into());
        }
        guard.creating = true;
        (guard.browser.clone(), guard.bound)
    };
    let owns_user_context = !caps.web_socket_url;
    let result = match browser.ensure_renderer_ready().await {
        Ok(()) => build_session_tabs(&browser, owns_user_context).await,
        Err(error) => Err(error.to_string()),
    };
    let mut guard = sessions.lock().await;
    guard.creating = false;
    let (user_context, tabs) = result?;
    let id = random_id()?;
    let mut windows = HashMap::new();
    let mut current = None;
    for tab in tabs {
        guard.next_window = guard.next_window.saturating_add(1);
        let handle = tab_handle_id(tab.id());
        if current.is_none() {
            current = Some(handle.clone());
        }
        windows.insert(
            handle,
            Window {
                tab,
                order: guard.next_window,
            },
        );
    }
    let Some(current) = current else {
        if owns_user_context {
            let _ = browser.remove_user_context(&user_context).await;
        }
        return Err("failed to create initial window".into());
    };
    let caps_json = caps.to_json(&id, bound);
    let automation = browser.open_automation_session();
    automation.set_default_prompt_behavior(Some(caps.prompt_behavior), !caps.web_socket_url);
    guard.open.insert(
        id.clone(),
        Session {
            user_context,
            owns_user_context,
            automation,
            current,
            current_frame: FrameId::MAIN,
            windows,
            implicit_timeout: caps.implicit_timeout,
            script_timeout: caps.script_timeout,
            page_load_timeout: caps.page_load_timeout,
            page_load_strategy: caps.page_load_strategy,
            prompt_behavior: caps.prompt_behavior,
            prompt_text: None,
            window_rect: [0.0, 0.0, 800.0, 600.0],
            permissions: HashMap::new(),
            preload_scripts: HashMap::new(),
            strict_file_interactability: caps.strict_file_interactability,
        },
    );
    Ok((id, caps_json))
}

async fn build_session_tabs(
    browser: &BrowserHandle,
    owns_user_context: bool,
) -> Result<(UserContextId, Vec<TabHandle>), String> {
    if owns_user_context {
        let user_context = browser
            .create_user_context()
            .await
            .map_err(|err| err.to_string())?;
        let tab = match blank_window(browser, user_context.clone()).await {
            Ok(tab) => tab,
            Err(error) => {
                let _ = browser.remove_user_context(&user_context).await;
                return Err(error);
            }
        };
        return Ok((user_context, vec![tab]));
    }
    let user_context = UserContextId::default_context();
    let mut tabs = Vec::new();
    if let Ok(ids) = browser.tabs() {
        for tab_id in ids {
            if let Ok(tab) = browser.tab(tab_id) {
                tabs.push(tab);
            }
        }
    }
    if tabs.is_empty() {
        tabs.push(blank_window(browser, user_context.clone()).await?);
    }
    Ok((user_context, tabs))
}

pub(crate) async fn teardown_session(browser: &BrowserHandle, session: Session) {
    let Session {
        user_context,
        owns_user_context,
        automation,
        windows,
        ..
    } = session;
    drop(automation);
    if owns_user_context {
        let _ = browser.remove_user_context(&user_context).await;
    } else {
        let _ = browser.clear_cookies_in(&user_context);
        for window in windows.into_values() {
            let _ = browser.close_tab(window.tab.id()).await;
        }
    }
}

#[must_use]
pub(crate) fn tab_handle_id(id: TabId) -> String {
    format!("t{}", id.get())
}

#[must_use]
pub(crate) fn parse_context_id(raw: &str) -> Option<(TabId, FrameId)> {
    if let Some((tab_part, frame_part)) = raw.split_once(':') {
        let tab = tab_part.strip_prefix('t')?.parse::<u64>().ok()?;
        let frame = frame_part.strip_prefix('f')?.parse::<u64>().ok()?;
        Some((TabId::new(tab), FrameId::new(frame)))
    } else {
        let tab = raw.strip_prefix('t')?.parse::<u64>().ok()?;
        Some((TabId::new(tab), FrameId::MAIN))
    }
}

#[must_use]
pub(crate) fn format_context_id(tab: TabId, frame: FrameId) -> String {
    if frame == FrameId::MAIN {
        tab_handle_id(tab)
    } else {
        format!("t{}:f{}", tab.get(), frame.get())
    }
}
