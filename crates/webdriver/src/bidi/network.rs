// https://w3c.github.io/webdriver-bidi/#module-network

use std::collections::HashSet;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use browser::{
    FrameId, InterceptDecision, InterceptId, InterceptPhase, InterceptResolveError,
    NetworkRequestId, TabHandle, TabId, UrlPattern, UserContextId,
};
use serde_json::{Value, json};

use crate::bidi::browsing_context::resolve_context;
use crate::bidi::protocol::{
    BidiError, BidiResult, optional_bool, optional_str, require_object, require_str,
};
use crate::bidi::{BidiState, DataCollector};
use crate::session::{parse_context_id, random_id};

// https://w3c.github.io/webdriver-bidi/#command-network-addIntercept
pub(crate) async fn add_intercept(state: &Arc<BidiState>, params: &Value) -> BidiResult {
    let map = require_object(params, "params")?;
    let Some(phases_arr) = map.get("phases").and_then(Value::as_array) else {
        return Err(BidiError::invalid_argument("phases must be an array"));
    };
    if phases_arr.is_empty() {
        return Err(BidiError::invalid_argument("phases must not be empty"));
    }
    let mut phases = Vec::with_capacity(phases_arr.len());
    for item in phases_arr {
        let Some(phase) = item.as_str() else {
            return Err(BidiError::invalid_argument("phase must be a string"));
        };
        match phase {
            "beforeRequestSent" => phases.push(InterceptPhase::BeforeRequestSent),
            "responseStarted" => phases.push(InterceptPhase::ResponseStarted),
            "authRequired" => {}
            other => {
                return Err(BidiError::invalid_argument(format!(
                    "invalid intercept phase: {other}"
                )));
            }
        }
    }
    let contexts: Option<Vec<TabId>> = match map.get("contexts") {
        None => None,
        Some(Value::Array(arr)) if !arr.is_empty() => {
            let mut list = Vec::with_capacity(arr.len());
            for item in arr {
                let Some(raw) = item.as_str() else {
                    return Err(BidiError::invalid_argument("context must be a string"));
                };
                let (tab, frame) = resolve_context(state, raw).await?;
                if frame != FrameId::MAIN {
                    return Err(BidiError::invalid_argument(
                        "intercept context must be top-level",
                    ));
                }
                list.push(tab.id());
            }
            Some(list)
        }
        Some(_) => {
            return Err(BidiError::invalid_argument(
                "contexts must be a non-empty array",
            ));
        }
    };
    let patterns = match map.get("urlPatterns") {
        None => Vec::new(),
        Some(Value::Array(arr)) => {
            let mut out = Vec::with_capacity(arr.len());
            for item in arr {
                out.push(parse_url_pattern(item)?);
            }
            out
        }
        Some(_) => {
            return Err(BidiError::invalid_argument("urlPatterns must be an array"));
        }
    };
    let id = state
        .browser
        .network_broker()
        .add_intercept(phases, contexts, patterns);
    state
        .intercepts
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(id);
    Ok(json!({ "intercept": id.to_string() }))
}

fn parse_url_pattern(val: &Value) -> Result<UrlPattern, BidiError> {
    let obj = require_object(val, "urlPattern")?;
    let typ = require_str(obj, "type")?;
    match typ {
        "string" => {
            let pattern = require_str(obj, "pattern")?;
            Ok(UrlPattern::String(pattern.to_owned()))
        }
        "pattern" => Ok(UrlPattern::Pattern {
            protocol: optional_str(obj, "protocol")?.map(str::to_owned),
            hostname: optional_str(obj, "hostname")?.map(str::to_owned),
            port: optional_str(obj, "port")?.map(str::to_owned),
            pathname: optional_str(obj, "pathname")?.map(str::to_owned),
            search: optional_str(obj, "search")?.map(str::to_owned),
        }),
        other => Err(BidiError::invalid_argument(format!(
            "invalid urlPattern type: {other}"
        ))),
    }
}

// https://w3c.github.io/webdriver-bidi/#command-network-removeIntercept
pub(crate) fn remove_intercept(state: &Arc<BidiState>, params: &Value) -> BidiResult {
    let map = require_object(params, "params")?;
    let raw = require_str(map, "intercept")?;
    let id = raw
        .strip_prefix("intercept-")
        .and_then(|s| s.parse::<u64>().ok())
        .map(InterceptId::new)
        .ok_or_else(|| BidiError::no_such_intercept(format!("unknown intercept: {raw}")))?;
    if !state.browser.network_broker().remove_intercept(id) {
        return Err(BidiError::no_such_intercept(format!(
            "unknown intercept: {raw}"
        )));
    }
    state
        .intercepts
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .retain(|existing| *existing != id);
    Ok(json!({}))
}

fn parse_request_id(map: &serde_json::Map<String, Value>) -> Result<NetworkRequestId, BidiError> {
    let raw = require_str(map, "request")?;
    let n = raw
        .parse::<u64>()
        .map_err(|_| BidiError::no_such_request(format!("unknown request: {raw}")))?;
    Ok(NetworkRequestId::new(n))
}

fn parse_bidi_headers(val: &Value) -> Result<Vec<(String, String)>, BidiError> {
    let Some(arr) = val.as_array() else {
        return Err(BidiError::invalid_argument("headers must be an array"));
    };
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let obj = require_object(item, "header")?;
        let name = require_str(obj, "name")?;
        let val_obj = obj
            .get("value")
            .ok_or_else(|| BidiError::invalid_argument("header.value is required"))?;
        let bytes = decode_bytes_value(val_obj)?;
        let text = String::from_utf8(bytes)
            .map_err(|_| BidiError::invalid_argument("header value must be UTF-8"))?;
        out.push((name.to_owned(), text));
    }
    Ok(out)
}

pub(crate) fn decode_bytes_value(val: &Value) -> Result<Vec<u8>, BidiError> {
    let obj = require_object(val, "bytesValue")?;
    let typ = require_str(obj, "type")?;
    let raw = require_str(obj, "value")?;
    match typ {
        "string" => Ok(raw.as_bytes().to_vec()),
        "base64" => BASE64
            .decode(raw)
            .map_err(|_| BidiError::invalid_argument("invalid base64 value")),
        other => Err(BidiError::invalid_argument(format!(
            "invalid bytesValue type: {other}"
        ))),
    }
}

fn map_resolve_error(err: InterceptResolveError) -> BidiError {
    match err {
        InterceptResolveError::NotFound => BidiError::no_such_request("no blocked request"),
        InterceptResolveError::WrongPhase => {
            BidiError::invalid_argument("request is not blocked in the expected phase")
        }
    }
}

// https://w3c.github.io/webdriver-bidi/#command-network-continueRequest
pub(crate) fn continue_request(state: &Arc<BidiState>, params: &Value) -> BidiResult {
    let map = require_object(params, "params")?;
    let request_id = parse_request_id(map)?;
    let url = optional_str(map, "url")?.map(str::to_owned);
    if let Some(ref u) = url
        && url::Url::parse(u).is_err()
    {
        return Err(BidiError::invalid_argument("invalid url"));
    }
    let method = optional_str(map, "method")?.map(str::to_owned);
    let headers = match map.get("headers") {
        None => None,
        Some(v) => Some(parse_bidi_headers(v)?),
    };
    let body = match map.get("body") {
        None => None,
        Some(v) => Some(decode_bytes_value(v)?),
    };
    state
        .browser
        .network_broker()
        .resolve_intercept(
            request_id,
            InterceptDecision::ContinueRequest {
                url,
                method,
                headers,
                body,
            },
        )
        .map_err(map_resolve_error)?;
    Ok(json!({}))
}

// https://w3c.github.io/webdriver-bidi/#command-network-continueResponse
pub(crate) fn continue_response(state: &Arc<BidiState>, params: &Value) -> BidiResult {
    let map = require_object(params, "params")?;
    let request_id = parse_request_id(map)?;
    let status = match map.get("statusCode") {
        None => None,
        Some(v) => {
            let code = v
                .as_u64()
                .and_then(|n| u16::try_from(n).ok())
                .ok_or_else(|| BidiError::invalid_argument("invalid statusCode"))?;
            Some(code)
        }
    };
    let reason_phrase = optional_str(map, "reasonPhrase")?.map(str::to_owned);
    let headers = match map.get("headers") {
        None => None,
        Some(v) => Some(parse_bidi_headers(v)?),
    };
    state
        .browser
        .network_broker()
        .resolve_intercept(
            request_id,
            InterceptDecision::ContinueResponse {
                status,
                reason_phrase,
                headers,
            },
        )
        .map_err(map_resolve_error)?;
    Ok(json!({}))
}

// https://w3c.github.io/webdriver-bidi/#command-network-provideResponse
pub(crate) fn provide_response(state: &Arc<BidiState>, params: &Value) -> BidiResult {
    let map = require_object(params, "params")?;
    let request_id = parse_request_id(map)?;
    let status = match map.get("statusCode") {
        None => 200,
        Some(v) => v
            .as_u64()
            .and_then(|n| u16::try_from(n).ok())
            .ok_or_else(|| BidiError::invalid_argument("invalid statusCode"))?,
    };
    let reason_phrase = optional_str(map, "reasonPhrase")?
        .unwrap_or("OK")
        .to_owned();
    let headers = match map.get("headers") {
        None => Vec::new(),
        Some(v) => parse_bidi_headers(v)?,
    };
    let body = match map.get("body") {
        None => Vec::new(),
        Some(v) => decode_bytes_value(v)?,
    };
    state
        .browser
        .network_broker()
        .resolve_intercept(
            request_id,
            InterceptDecision::ProvideResponse {
                status,
                reason_phrase,
                headers,
                body,
            },
        )
        .map_err(map_resolve_error)?;
    Ok(json!({}))
}

// https://w3c.github.io/webdriver-bidi/#command-network-failRequest
pub(crate) fn fail_request(state: &Arc<BidiState>, params: &Value) -> BidiResult {
    let map = require_object(params, "params")?;
    let request_id = parse_request_id(map)?;
    state
        .browser
        .network_broker()
        .resolve_intercept(request_id, InterceptDecision::FailRequest)
        .map_err(map_resolve_error)?;
    Ok(json!({}))
}

// https://w3c.github.io/webdriver-bidi/#command-network-setCacheBehavior
pub(crate) async fn set_cache_behavior(state: &Arc<BidiState>, params: &Value) -> BidiResult {
    let map = require_object(params, "params")?;
    let behavior = require_str(map, "cacheBehavior")?;
    let disabled = match behavior {
        "default" => false,
        "bypass" => true,
        other => {
            return Err(BidiError::invalid_argument(format!(
                "invalid cacheBehavior: {other}"
            )));
        }
    };
    if let Some(ctxs) = map.get("contexts") {
        let Some(arr) = ctxs.as_array() else {
            return Err(BidiError::invalid_argument("contexts must be an array"));
        };
        if arr.is_empty() {
            return Err(BidiError::invalid_argument("contexts must not be empty"));
        }
        for item in arr {
            let Some(raw) = item.as_str() else {
                return Err(BidiError::invalid_argument("context must be a string"));
            };
            let Some((tab_id, frame)) = parse_context_id(raw) else {
                return Err(BidiError::no_such_frame(raw));
            };
            if frame != FrameId::MAIN {
                return Err(BidiError::invalid_argument(
                    "cacheBehavior context must be top-level",
                ));
            }
            let tab = state
                .browser
                .tab(tab_id)
                .await
                .map_err(|_| BidiError::no_such_frame(raw))?;
            let _ = state
                .browser
                .set_cache_disabled_in(tab.user_context(), disabled)
                .await;
        }
    } else {
        for uc in state
            .browser
            .user_contexts()
            .await
            .unwrap_or_else(|_| vec![UserContextId::default_context()])
        {
            let _ = state.browser.set_cache_disabled_in(&uc, disabled).await;
        }
    }
    Ok(json!({}))
}

// https://fetch.spec.whatwg.org/#header-name
fn is_valid_header_name(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(|b| {
            matches!(
                b,
                b'!' | b'#' | b'$' | b'%' | b'&' | b'\'' | b'*' | b'+' | b'-' | b'.'
                    | b'^' | b'_' | b'`' | b'|' | b'~'
                    | b'0'..=b'9' | b'A'..=b'Z' | b'a'..=b'z'
            )
        })
}

// https://fetch.spec.whatwg.org/#header-value
fn is_valid_header_value(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.first().is_some_and(|b| matches!(b, b' ' | b'\t'))
        || bytes.last().is_some_and(|b| matches!(b, b' ' | b'\t'))
    {
        return false;
    }
    !bytes.iter().any(|b| matches!(b, 0 | b'\r' | b'\n'))
}

async fn resolve_optional_scope_tabs(
    state: &Arc<BidiState>,
    map: &serde_json::Map<String, Value>,
) -> Result<Vec<TabHandle>, BidiError> {
    let has_ctx = map.contains_key("contexts");
    let has_uc = map.contains_key("userContexts");
    if has_ctx && has_uc {
        return Err(BidiError::invalid_argument(
            "cannot specify both contexts and userContexts",
        ));
    }
    let mut tabs = Vec::new();
    if let Some(val) = map.get("contexts") {
        let Some(arr) = val.as_array() else {
            return Err(BidiError::invalid_argument("contexts must be an array"));
        };
        if arr.is_empty() {
            return Err(BidiError::invalid_argument("contexts must not be empty"));
        }
        for item in arr {
            let Some(raw) = item.as_str() else {
                return Err(BidiError::invalid_argument("context must be a string"));
            };
            let (tab, frame) = resolve_context(state, raw).await?;
            if frame != FrameId::MAIN {
                return Err(BidiError::invalid_argument(
                    "context must be a top-level browsing context",
                ));
            }
            tabs.push(tab);
        }
    } else if let Some(val) = map.get("userContexts") {
        let Some(arr) = val.as_array() else {
            return Err(BidiError::invalid_argument("userContexts must be an array"));
        };
        if arr.is_empty() {
            return Err(BidiError::invalid_argument(
                "userContexts must not be empty",
            ));
        }
        let known = state.browser.user_contexts().await.unwrap_or_default();
        for item in arr {
            let Some(raw) = item.as_str() else {
                return Err(BidiError::invalid_argument("userContext must be a string"));
            };
            let uc = UserContextId::new(raw);
            if !known.contains(&uc) {
                return Err(BidiError::no_such_user_context(raw));
            }
            if let Ok(ids) = state.browser.tabs_in(&uc).await {
                for id in ids {
                    if let Ok(tab) = state.browser.tab(id).await {
                        tabs.push(tab);
                    }
                }
            }
        }
    } else if let Ok(ids) = state.browser.tabs().await {
        for id in ids {
            if let Ok(tab) = state.browser.tab(id).await {
                tabs.push(tab);
            }
        }
    }
    Ok(tabs)
}

// https://w3c.github.io/webdriver-bidi/#command-network-setExtraHeaders
pub(crate) async fn set_extra_headers(state: &Arc<BidiState>, params: &Value) -> BidiResult {
    let map = require_object(params, "params")?;
    let Some(headers_val) = map.get("headers") else {
        return Err(BidiError::invalid_argument("headers is required"));
    };
    let headers = parse_bidi_headers(headers_val)?;
    for (name, value) in &headers {
        if !is_valid_header_name(name) {
            return Err(BidiError::invalid_argument(format!(
                "invalid header name: {name}"
            )));
        }
        if !is_valid_header_value(value) {
            return Err(BidiError::invalid_argument(format!(
                "invalid header value for {name}"
            )));
        }
    }
    let tabs = resolve_optional_scope_tabs(state, map).await?;
    for tab in tabs {
        let _ = tab.set_extra_headers(headers.clone()).await;
    }
    Ok(json!({}))
}

const MAX_COLLECTOR_ENCODED_DATA_SIZE: u64 = 100_000_000;

// https://w3c.github.io/webdriver-bidi/#command-network-addDataCollector
pub(crate) async fn add_data_collector(state: &Arc<BidiState>, params: &Value) -> BidiResult {
    let map = require_object(params, "params")?;
    let Some(dt_arr) = map.get("dataTypes").and_then(Value::as_array) else {
        return Err(BidiError::invalid_argument("dataTypes must be an array"));
    };
    if dt_arr.is_empty() {
        return Err(BidiError::invalid_argument("dataTypes must not be empty"));
    }
    let mut data_types = Vec::with_capacity(dt_arr.len());
    for item in dt_arr {
        let Some(dt) = item.as_str() else {
            return Err(BidiError::invalid_argument("dataType must be a string"));
        };
        if !matches!(dt, "request" | "response") {
            return Err(BidiError::invalid_argument(format!(
                "invalid dataType: {dt}"
            )));
        }
        data_types.push(dt.to_owned());
    }
    let Some(max_size) = map.get("maxEncodedDataSize").and_then(Value::as_u64) else {
        return Err(BidiError::invalid_argument(
            "maxEncodedDataSize must be a positive integer",
        ));
    };
    if max_size == 0 || max_size > MAX_COLLECTOR_ENCODED_DATA_SIZE {
        return Err(BidiError::invalid_argument(
            "maxEncodedDataSize out of range",
        ));
    }
    if let Some(ct) = optional_str(map, "collectorType")?
        && ct != "blob"
    {
        return Err(BidiError::invalid_argument(format!(
            "invalid collectorType: {ct}"
        )));
    }
    let _tabs = resolve_optional_scope_tabs(state, map).await?;
    let id = random_id().unwrap_or_else(|_| "collector-1".to_owned());
    state.data_collectors.lock().await.insert(
        id.clone(),
        DataCollector {
            data_types,
            max_encoded_data_size: max_size,
            disowned: HashSet::new(),
        },
    );
    Ok(json!({ "collector": id }))
}

// https://w3c.github.io/webdriver-bidi/#command-network-removeDataCollector
pub(crate) async fn remove_data_collector(state: &Arc<BidiState>, params: &Value) -> BidiResult {
    let map = require_object(params, "params")?;
    let collector_id = require_str(map, "collector")?;
    if state
        .data_collectors
        .lock()
        .await
        .remove(collector_id)
        .is_none()
    {
        return Err(BidiError::new(
            "no such network collector",
            format!("unknown collector: {collector_id}"),
        ));
    }
    Ok(json!({}))
}

// https://w3c.github.io/webdriver-bidi/#command-network-disownData
pub(crate) async fn disown_data(state: &Arc<BidiState>, params: &Value) -> BidiResult {
    let map = require_object(params, "params")?;
    let data_type = require_str(map, "dataType")?;
    if !matches!(data_type, "request" | "response") {
        return Err(BidiError::invalid_argument(format!(
            "invalid dataType: {data_type}"
        )));
    }
    let collector_id = require_str(map, "collector")?;
    let req_raw = require_str(map, "request")?;
    let mut collectors = state.data_collectors.lock().await;
    let Some(collector) = collectors.get_mut(collector_id) else {
        return Err(BidiError::new(
            "no such network collector",
            format!("unknown collector: {collector_id}"),
        ));
    };
    let Ok(req_num) = req_raw.parse::<u64>() else {
        return Err(BidiError::new(
            "no such network data",
            format!("unknown request: {req_raw}"),
        ));
    };
    let request_id = NetworkRequestId::new(req_num);
    if !collector.data_types.iter().any(|t| t == data_type) {
        return Err(BidiError::new(
            "no such network data",
            "collector does not collect this dataType",
        ));
    }
    let key = (data_type.to_owned(), request_id);
    if collector.disowned.contains(&key) {
        return Err(BidiError::new(
            "no such network data",
            "data already disowned",
        ));
    }
    let Some(bytes) = state.browser.network_broker().response_body(request_id) else {
        return Err(BidiError::new(
            "no such network data",
            "network data unavailable",
        ));
    };
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > collector.max_encoded_data_size {
        return Err(BidiError::new(
            "no such network data",
            "data exceeded collector limit",
        ));
    }
    collector.disowned.insert(key);
    Ok(json!({}))
}

// https://w3c.github.io/webdriver-bidi/#command-network-getData
pub(crate) async fn get_data(state: &Arc<BidiState>, params: &Value) -> BidiResult {
    let map = require_object(params, "params")?;
    let data_type = require_str(map, "dataType")?;
    if !matches!(data_type, "request" | "response") {
        return Err(BidiError::invalid_argument(format!(
            "invalid dataType: {data_type}"
        )));
    }
    let collector_id = optional_str(map, "collector")?;
    let disown = optional_bool(map, "disown")?.unwrap_or(false);
    if disown && collector_id.is_none() {
        return Err(BidiError::invalid_argument(
            "disown requires collector to be specified",
        ));
    }
    let req_raw = require_str(map, "request")?;
    let Ok(req_num) = req_raw.parse::<u64>() else {
        return Err(BidiError::new(
            "no such network data",
            format!("unknown request: {req_raw}"),
        ));
    };
    let request_id = NetworkRequestId::new(req_num);
    let mut collectors = state.data_collectors.lock().await;
    if let Some(cid) = collector_id {
        let Some(collector) = collectors.get_mut(cid) else {
            return Err(BidiError::new(
                "no such network collector",
                format!("unknown collector: {cid}"),
            ));
        };
        let key = (data_type.to_owned(), request_id);
        if !collector.data_types.iter().any(|t| t == data_type) || collector.disowned.contains(&key)
        {
            return Err(BidiError::new(
                "no such network data",
                "network data unavailable for collector",
            ));
        }
        let Some(bytes) = state.browser.network_broker().response_body(request_id) else {
            return Err(BidiError::new(
                "no such network data",
                "response body unavailable",
            ));
        };
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > collector.max_encoded_data_size {
            return Err(BidiError::new(
                "no such network data",
                "data exceeded collector limit",
            ));
        }
        if disown {
            collector.disowned.insert(key);
        }
        let val = match String::from_utf8(bytes.clone()) {
            Ok(text) => json!({ "type": "string", "value": text }),
            Err(_) => json!({ "type": "base64", "value": BASE64.encode(&bytes) }),
        };
        return Ok(json!({ "bytes": val }));
    }
    drop(collectors);
    let Some(bytes) = state.browser.network_broker().response_body(request_id) else {
        return Err(BidiError::new(
            "no such network data",
            "response body unavailable",
        ));
    };
    let val = match String::from_utf8(bytes.clone()) {
        Ok(text) => json!({ "type": "string", "value": text }),
        Err(_) => json!({ "type": "base64", "value": BASE64.encode(&bytes) }),
    };
    Ok(json!({ "bytes": val }))
}
