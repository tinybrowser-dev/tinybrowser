// https://w3c.github.io/webdriver-bidi/#module-session

use std::sync::Arc;

use browser::{TabId, UserContextId};
use serde_json::{Value, json};

use crate::bidi::protocol::{BidiError, BidiResult, require_object};
use crate::bidi::{BidiState, is_known_event_or_module};
use crate::session::{CapabilityError, parse_context_id, process_capabilities};

// https://w3c.github.io/webdriver-bidi/#command-session-status
pub(crate) async fn status(state: &Arc<BidiState>) -> BidiResult {
    let guard = state.sessions.lock().await;
    let ready = guard.open.is_empty();
    let message = if ready {
        "ready"
    } else {
        "session already started"
    };
    Ok(json!({
        "ready": ready,
        "message": message,
    }))
}

// https://w3c.github.io/webdriver-bidi/#command-session-new
pub(crate) async fn new(state: &Arc<BidiState>, params: &Value) -> BidiResult {
    let matched = match process_capabilities(params) {
        Ok(caps) => caps,
        Err(CapabilityError::InvalidArgument(msg)) => return Err(BidiError::invalid_argument(msg)),
        Err(CapabilityError::SessionNotCreated(msg)) => {
            return Err(BidiError::session_not_created(msg));
        }
    };
    let (id, capabilities) = {
        let mut guard = state.sessions.lock().await;
        if !guard.open.is_empty() {
            return Err(BidiError::session_not_created("already have a session"));
        }
        guard
            .create(matched)
            .await
            .map_err(BidiError::session_not_created)?
    };
    *state.session_id.lock().await = Some(id.clone());
    state.attach_all_existing_tabs().await;
    Ok(json!({
        "sessionId": id,
        "capabilities": capabilities,
    }))
}

// https://w3c.github.io/webdriver-bidi/#command-session-end
pub(crate) async fn end(state: &Arc<BidiState>) -> BidiResult {
    state.clear_intercepts();
    let sid = state.session_id.lock().await.take();
    if let Some(sid) = sid {
        let tabs: Vec<TabId> = {
            let mut guard = state.sessions.lock().await;
            guard.sync_windows(&sid).await;
            guard
                .open
                .remove(&sid)
                .map_or_default(|s| s.windows.into_values().map(|w| w.tab.id()).collect())
        };
        state.browser.prompt_broker().set_default_behavior(None);
        state.browser.prompt_broker().set_hold_for_classic(false);
        let _ = state.browser.clear_cookies().await;
        for tab_id in tabs {
            let _ = state.browser.close_tab(tab_id).await;
        }
    }
    Ok(json!({}))
}

// https://w3c.github.io/webdriver-bidi/#command-session-subscribe
pub(crate) async fn subscribe(state: &Arc<BidiState>, params: &Value) -> BidiResult {
    let map = require_object(params, "params")?;
    let events = parse_event_list(map)?;
    let contexts = parse_optional_contexts(state, map).await?;
    let user_contexts = parse_optional_user_contexts(state, map).await?;
    if contexts.is_some() && user_contexts.is_some() {
        return Err(BidiError::invalid_argument(
            "cannot specify both contexts and userContexts",
        ));
    }
    let subscription_id = crate::session::random_id().unwrap_or_else(|_| "sub-1".to_owned());
    let subscribes_log = crate::bidi::expand_events(&events)
        .iter()
        .any(|e| e == "log.entryAdded");
    state.subscriptions.lock().await.subscribe(
        subscription_id.clone(),
        &events,
        contexts.as_deref(),
        user_contexts.as_deref(),
    );
    state.attach_all_existing_tabs().await;
    if subscribes_log {
        let mut buf = state.log_buffer.lock().await;
        let mut remaining = std::collections::VecDeque::new();
        while let Some(entry) = buf.pop_front() {
            let matches = if let Some(ref ctxs) = contexts {
                ctxs.contains(&entry.tab)
            } else if let Some(ref ucs) = user_contexts {
                ucs.contains(&entry.user_context)
            } else {
                true
            };
            if matches {
                let _ = state.out_tx.send(crate::bidi::protocol::event_message(
                    "log.entryAdded",
                    &entry.params,
                ));
            } else {
                remaining.push_back(entry);
            }
        }
        *buf = remaining;
    }
    Ok(json!({ "subscription": subscription_id }))
}

// https://w3c.github.io/webdriver-bidi/#command-session-unsubscribe
pub(crate) async fn unsubscribe(state: &Arc<BidiState>, params: &Value) -> BidiResult {
    let map = require_object(params, "params")?;
    let has_subs = map.contains_key("subscriptions");
    let has_events = map.contains_key("events");
    if has_subs && has_events {
        return Err(BidiError::invalid_argument(
            "cannot specify both subscriptions and events",
        ));
    }
    if has_subs {
        if map.contains_key("contexts") {
            return Err(BidiError::invalid_argument(
                "cannot specify both subscriptions and contexts",
            ));
        }
        let Some(subs) = map.get("subscriptions").and_then(Value::as_array) else {
            return Err(BidiError::invalid_argument(
                "subscriptions must be an array",
            ));
        };
        if subs.is_empty() {
            return Err(BidiError::invalid_argument(
                "subscriptions must not be empty",
            ));
        }
        let mut ids = Vec::with_capacity(subs.len());
        for item in subs {
            let Some(id) = item.as_str() else {
                return Err(BidiError::invalid_argument(
                    "subscription id must be a string",
                ));
            };
            ids.push(id.to_owned());
        }
        state.subscriptions.lock().await.unsubscribe_by_ids(&ids)?;
        return Ok(json!({}));
    }
    let events = parse_event_list(map)?;
    let contexts = parse_optional_contexts(state, map).await?;
    state
        .subscriptions
        .lock()
        .await
        .unsubscribe_by_events(&events, contexts.as_deref())?;
    Ok(json!({}))
}

fn parse_event_list(map: &serde_json::Map<String, Value>) -> Result<Vec<String>, BidiError> {
    let Some(arr) = map.get("events").and_then(Value::as_array) else {
        return Err(BidiError::invalid_argument("events must be an array"));
    };
    if arr.is_empty() {
        return Err(BidiError::invalid_argument("events must not be empty"));
    }
    let mut events = Vec::with_capacity(arr.len());
    for item in arr {
        let Some(name) = item.as_str() else {
            return Err(BidiError::invalid_argument("event name must be a string"));
        };
        if !is_known_event_or_module(name) {
            return Err(BidiError::invalid_argument(format!(
                "unknown event: {name}"
            )));
        }
        events.push(name.to_owned());
    }
    Ok(events)
}

async fn parse_optional_contexts(
    state: &Arc<BidiState>,
    map: &serde_json::Map<String, Value>,
) -> Result<Option<Vec<TabId>>, BidiError> {
    let Some(val) = map.get("contexts") else {
        return Ok(None);
    };
    let Some(arr) = val.as_array() else {
        return Err(BidiError::invalid_argument("contexts must be an array"));
    };
    if arr.is_empty() {
        return Err(BidiError::invalid_argument("contexts must not be empty"));
    }
    let live_tabs = state.browser.tabs().await.unwrap_or_default();
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let Some(raw) = item.as_str() else {
            return Err(BidiError::invalid_argument("context id must be a string"));
        };
        let Some((tab, _)) = parse_context_id(raw) else {
            return Err(BidiError::no_such_frame(format!("unknown context: {raw}")));
        };
        if !live_tabs.contains(&tab) {
            return Err(BidiError::no_such_frame(format!("unknown context: {raw}")));
        }
        out.push(tab);
    }
    Ok(Some(out))
}

async fn parse_optional_user_contexts(
    state: &Arc<BidiState>,
    map: &serde_json::Map<String, Value>,
) -> Result<Option<Vec<UserContextId>>, BidiError> {
    let Some(val) = map.get("userContexts") else {
        return Ok(None);
    };
    let Some(arr) = val.as_array() else {
        return Err(BidiError::invalid_argument("userContexts must be an array"));
    };
    if arr.is_empty() {
        return Err(BidiError::invalid_argument(
            "userContexts must not be empty",
        ));
    }
    let known = state.browser.user_contexts().await.unwrap_or_default();
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let Some(raw) = item.as_str() else {
            return Err(BidiError::invalid_argument(
                "userContext id must be a string",
            ));
        };
        let uc = UserContextId::new(raw);
        if !known.contains(&uc) {
            return Err(BidiError::no_such_user_context(format!(
                "unknown user context: {raw}"
            )));
        }
        out.push(uc);
    }
    Ok(Some(out))
}
