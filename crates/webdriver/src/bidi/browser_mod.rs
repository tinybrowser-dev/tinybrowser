// https://w3c.github.io/webdriver-bidi/#module-browser

use std::sync::Arc;

use browser::{FrameId, UserContextId};
use serde_json::{Value, json};

use crate::bidi::BidiState;
use crate::bidi::protocol::{BidiError, BidiResult, optional_bool, require_object, require_str};
use crate::session::{
    CapabilityError, format_context_id, is_safe_i64, parse_bidi_prompt_handler, parse_context_id,
    validate_proxy_object,
};

// https://w3c.github.io/webdriver-bidi/#command-browser-close
pub(crate) async fn close(state: &Arc<BidiState>) -> BidiResult {
    let shutdown = state.sessions.lock().await.shutdown.clone();
    shutdown.request();
    Ok(json!({}))
}

// https://w3c.github.io/webdriver-bidi/#command-browser-createUserContext
pub(crate) async fn create_user_context(state: &Arc<BidiState>, params: &Value) -> BidiResult {
    let map = require_object(params, "params")?;
    let _accept_insecure_certs = optional_bool(map, "acceptInsecureCerts")?;
    if let Some(proxy) = map.get("proxy") {
        validate_proxy_object(proxy).map_err(|err| match err {
            CapabilityError::InvalidArgument(msg) | CapabilityError::SessionNotCreated(msg) => {
                BidiError::invalid_argument(msg)
            }
        })?;
    }
    if let Some(prompt_behavior) = map.get("unhandledPromptBehavior") {
        parse_bidi_prompt_handler(prompt_behavior).map_err(|err| match err {
            CapabilityError::InvalidArgument(msg) | CapabilityError::SessionNotCreated(msg) => {
                BidiError::invalid_argument(msg)
            }
        })?;
    }
    let id = state
        .browser
        .create_user_context()
        .await
        .map_err(|err| BidiError::unknown_error(err.to_string()))?;
    Ok(json!({ "userContext": id.as_str() }))
}

// https://w3c.github.io/webdriver-bidi/#command-browser-getUserContexts
pub(crate) fn get_user_contexts(state: &Arc<BidiState>) -> BidiResult {
    let contexts = state
        .browser
        .user_contexts()
        .map_err(|err| BidiError::unknown_error(err.to_string()))?;
    let list: Vec<Value> = contexts
        .into_iter()
        .map(|uc| json!({ "userContext": uc.as_str() }))
        .collect();
    Ok(json!({ "userContexts": list }))
}

// https://w3c.github.io/webdriver-bidi/#command-browser-removeUserContext
pub(crate) async fn remove_user_context(state: &Arc<BidiState>, params: &Value) -> BidiResult {
    let map = require_object(params, "params")?;
    let raw = require_str(map, "userContext")?;
    let Some(id) = UserContextId::parse(raw) else {
        return Err(BidiError::no_such_user_context(format!(
            "unknown user context: {raw}"
        )));
    };
    if id.is_default() {
        return Err(BidiError::invalid_argument(
            "cannot remove default user context",
        ));
    }
    state
        .browser
        .remove_user_context(&id)
        .await
        .map_err(|err| match err {
            browser::BrowserError::UnknownUserContext => {
                BidiError::no_such_user_context(format!("unknown user context: {raw}"))
            }
            other => BidiError::unknown_error(other.to_string()),
        })?;
    Ok(json!({}))
}

// https://w3c.github.io/webdriver-bidi/#command-browser-getClientWindows
pub(crate) async fn get_client_windows(state: &Arc<BidiState>) -> BidiResult {
    let ids = state
        .browser
        .tabs()
        .map_err(|err| BidiError::unknown_error(err.to_string()))?;
    let mut windows = Vec::with_capacity(ids.len());
    for (idx, id) in ids.into_iter().enumerate() {
        let (width, height) = if let Ok(tab) = state.browser.tab(id) {
            tab.viewport_size()
                .await
                .unwrap_or(browser::DEFAULT_VIEWPORT)
        } else {
            browser::DEFAULT_VIEWPORT
        };
        windows.push(json!({
            "active": idx == 0,
            "clientWindow": format_context_id(id, FrameId::MAIN),
            "height": height,
            "state": "normal",
            "width": width,
            "x": 0,
            "y": 0,
        }));
    }
    Ok(json!({ "clientWindows": windows }))
}

fn parse_js_int(val: &Value, field: &str, non_negative: bool) -> Result<i64, BidiError> {
    let n = if let Some(v) = val.as_i64() {
        v
    } else if let Some(v) = val.as_u64() {
        i64::try_from(v)
            .map_err(|_| BidiError::invalid_argument(format!("{field} out of range")))?
    } else {
        return Err(BidiError::invalid_argument(format!(
            "{field} must be an integer"
        )));
    };
    if !is_safe_i64(n) || (non_negative && n < 0) {
        return Err(BidiError::invalid_argument(format!("{field} out of range")));
    }
    Ok(n)
}

// https://w3c.github.io/webdriver-bidi/#command-browser-setClientWindowState
pub(crate) async fn set_client_window_state(state: &Arc<BidiState>, params: &Value) -> BidiResult {
    let map = require_object(params, "params")?;
    let window_id = require_str(map, "clientWindow")?;
    let window_state = require_str(map, "state")?;
    if !matches!(
        window_state,
        "fullscreen" | "maximized" | "minimized" | "normal"
    ) {
        return Err(BidiError::invalid_argument(format!(
            "invalid client window state: {window_state}"
        )));
    }
    let width = match map.get("width") {
        None => None,
        Some(v) => Some(parse_js_int(v, "width", true)?),
    };
    let height = match map.get("height") {
        None => None,
        Some(v) => Some(parse_js_int(v, "height", true)?),
    };
    let x = match map.get("x") {
        None => 0,
        Some(v) => parse_js_int(v, "x", false)?,
    };
    let y = match map.get("y") {
        None => 0,
        Some(v) => parse_js_int(v, "y", false)?,
    };
    let Some((tab_id, FrameId::MAIN)) = parse_context_id(window_id) else {
        return Err(BidiError::unknown_error(format!(
            "unknown clientWindow: {window_id}"
        )));
    };
    let tab = state
        .browser
        .tab(tab_id)
        .map_err(|_| BidiError::unknown_error(format!("unknown clientWindow: {window_id}")))?;
    let (cur_w, cur_h) = tab
        .viewport_size()
        .await
        .unwrap_or(browser::DEFAULT_VIEWPORT);
    let new_w = width
        .and_then(|v| u32::try_from(v).ok())
        .filter(|v| *v > 0)
        .unwrap_or(cur_w);
    let new_h = height
        .and_then(|v| u32::try_from(v).ok())
        .filter(|v| *v > 0)
        .unwrap_or(cur_h);
    if window_state == "normal" && (width.is_some() || height.is_some()) {
        let _ = tab.set_viewport(new_w, new_h).await;
    }
    let (final_w, final_h) = tab.viewport_size().await.unwrap_or((new_w, new_h));
    Ok(json!({
        "active": true,
        "clientWindow": window_id,
        "height": final_h,
        "state": window_state,
        "width": final_w,
        "x": x,
        "y": y,
    }))
}

// https://w3c.github.io/webdriver-bidi/#command-browser-setDownloadBehavior
pub(crate) fn set_download_behavior(state: &Arc<BidiState>, params: &Value) -> BidiResult {
    let map = require_object(params, "params")?;
    let Some(behavior) = map.get("downloadBehavior") else {
        return Err(BidiError::invalid_argument("downloadBehavior is required"));
    };
    match behavior {
        Value::Null => {}
        Value::Object(obj) => {
            let kind = require_str(obj, "type")?;
            match kind {
                "allowed" => {
                    let _folder = require_str(obj, "destinationFolder")?;
                }
                "denied" => {}
                other => {
                    return Err(BidiError::invalid_argument(format!(
                        "invalid downloadBehavior type: {other}"
                    )));
                }
            }
        }
        _ => {
            return Err(BidiError::invalid_argument(
                "downloadBehavior must be an object or null",
            ));
        }
    }
    if let Some(ucs) = map.get("userContexts") {
        let Some(arr) = ucs.as_array() else {
            return Err(BidiError::invalid_argument("userContexts must be an array"));
        };
        if arr.is_empty() {
            return Err(BidiError::invalid_argument(
                "userContexts must not be empty",
            ));
        }
        let known = state.browser.user_contexts().unwrap_or_default();
        for item in arr {
            let Some(raw) = item.as_str() else {
                return Err(BidiError::invalid_argument("userContext must be a string"));
            };
            let Some(uc) = UserContextId::parse(raw) else {
                return Err(BidiError::no_such_user_context(format!(
                    "unknown user context: {raw}"
                )));
            };
            if !known.contains(&uc) {
                return Err(BidiError::no_such_user_context(format!(
                    "unknown user context: {raw}"
                )));
            }
        }
    }
    Ok(json!({}))
}
