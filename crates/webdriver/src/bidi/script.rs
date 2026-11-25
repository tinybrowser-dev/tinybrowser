// https://w3c.github.io/webdriver-bidi/#module-script

use std::sync::Arc;
use std::time::Duration;

use browser::{FrameId, RemoteValue, TabHandle, TabId, UserContextId};
use serde_json::{Value, json};

use crate::bidi::browsing_context::resolve_context;
use crate::bidi::protocol::{
    BIDI_RUNTIME_JS, BidiError, BidiResult, optional_bool, optional_str, require_object,
    require_str,
};
use crate::bidi::{BIDI_CHANNEL_BINDING, BidiState};
use crate::session::{PreloadScriptEntry, format_context_id, random_id};

async fn resolve_target(
    state: &Arc<BidiState>,
    target: &Value,
) -> Result<(TabHandle, FrameId, String, Option<String>), BidiError> {
    let obj = require_object(target, "target")?;
    if let Some(realm) = optional_str(obj, "realm")? {
        if let Some(ctx) = realm.strip_prefix("realm-") {
            let (tab, frame) = resolve_context(state, ctx).await?;
            return Ok((tab, frame, ctx.to_owned(), None));
        }
        return Err(BidiError::new(
            "no such frame",
            format!("unknown realm: {realm}"),
        ));
    }
    let ctx = require_str(obj, "context")?;
    let sandbox = optional_str(obj, "sandbox")?
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let (tab, frame) = resolve_context(state, ctx).await?;
    Ok((tab, frame, ctx.to_owned(), sandbox))
}

fn validate_ownership(map: &serde_json::Map<String, Value>) -> Result<&'static str, BidiError> {
    match optional_str(map, "resultOwnership")? {
        None | Some("none") => Ok("none"),
        Some("root") => Ok("root"),
        Some(other) => Err(BidiError::invalid_argument(format!(
            "invalid resultOwnership: {other}"
        ))),
    }
}

fn validate_serialization_options(
    map: &serde_json::Map<String, Value>,
) -> Result<(Value, Value, &'static str), BidiError> {
    let Some(val) = map.get("serializationOptions") else {
        return Ok((Value::Null, json!(0), "none"));
    };
    let obj = require_object(val, "serializationOptions")?;
    let max_obj = match obj.get("maxObjectDepth") {
        None | Some(Value::Null) => Value::Null,
        Some(v) => {
            if v.as_u64().is_none() {
                return Err(BidiError::invalid_argument(
                    "maxObjectDepth must be a non-negative integer or null",
                ));
            }
            v.clone()
        }
    };
    let max_dom = match obj.get("maxDomDepth") {
        None => json!(0),
        Some(Value::Null) => Value::Null,
        Some(v) => {
            if v.as_u64().is_none() {
                return Err(BidiError::invalid_argument(
                    "maxDomDepth must be a non-negative integer or null",
                ));
            }
            v.clone()
        }
    };
    let include_shadow = match optional_str(obj, "includeShadowTree")? {
        None | Some("none") => "none",
        Some("open") => "open",
        Some("all") => "all",
        Some(other) => {
            return Err(BidiError::invalid_argument(format!(
                "invalid includeShadowTree: {other}"
            )));
        }
    };
    Ok((max_obj, max_dom, include_shadow))
}

// https://w3c.github.io/webdriver-bidi/#command-script-evaluate
pub(crate) async fn evaluate(state: &Arc<BidiState>, params: &Value) -> BidiResult {
    let map = require_object(params, "params")?;
    let expression = require_str(map, "expression")?;
    let target = map
        .get("target")
        .ok_or_else(|| BidiError::invalid_argument("target is required"))?;
    let await_promise = optional_bool(map, "awaitPromise")?
        .ok_or_else(|| BidiError::invalid_argument("awaitPromise is required"))?;
    let ownership = validate_ownership(map)?;
    let (max_obj_depth, max_dom_depth, include_shadow) = validate_serialization_options(map)?;
    let _user_activation = optional_bool(map, "userActivation")?;
    let (tab, frame, ctx_id, sandbox) = resolve_target(state, target).await?;
    let _ = tab.add_binding(BIDI_CHANNEL_BINDING, true).await;

    let slot_id = {
        let mut guard = state.sessions.lock().await;
        guard.next_call_slot()
    };
    let realm_id = format!("realm-{ctx_id}");
    let run_script = format!(
        "(() => {{ {BIDI_RUNTIME_JS}\n\
         if (!host.__tbBidiSlots) host.__tbBidiSlots = Object.create(null);\n\
         const slot = {{ done: false, result: null }};\n\
         host.__tbBidiSlots[{slot_id}] = slot;\n\
         const contextId = {};\n\
         const realmId = {};\n\
         const ownership = {};\n\
         const maxObjDepth = {};\n\
         const maxDomDepth = {};\n\
         const includeShadow = {};\n\
         const awaitPromise = {};\n\
         function finishOk(val) {{\n\
           slot.result = JSON.stringify({{\n\
             type: 'success',\n\
             realm: realmId,\n\
             result: __tbBidi.serialize(val, maxObjDepth, maxDomDepth, contextId, ownership, undefined, undefined, includeShadow),\n\
           }});\n\
           slot.done = true;\n\
         }}\n\
         function finishErr(err) {{\n\
           slot.result = JSON.stringify({{\n\
             type: 'exception',\n\
             realm: realmId,\n\
             exceptionDetails: __tbBidi.exceptionDetails(err, contextId, ownership),\n\
           }});\n\
           slot.done = true;\n\
         }}\n\
         try {{\n\
           const value = host.evaluatePage({});\n\
           if (awaitPromise && value && typeof value.then === 'function') {{\n\
             value.then(finishOk, finishErr);\n\
           }} else {{\n\
             finishOk(value);\n\
           }}\n\
         }} catch (err) {{\n\
           finishErr(err);\n\
         }}\n\
         return slot.done ? slot.result : null;\n\
         }})()",
        json!(ctx_id),
        json!(realm_id),
        json!(ownership),
        max_obj_depth,
        max_dom_depth,
        json!(include_shadow),
        json!(await_promise),
        json!(expression)
    );
    execute_and_collect(&tab, frame, sandbox.as_deref(), &run_script, slot_id).await
}

// https://w3c.github.io/webdriver-bidi/#command-script-callFunction
pub(crate) async fn call_function(state: &Arc<BidiState>, params: &Value) -> BidiResult {
    let map = require_object(params, "params")?;
    let fn_decl = require_str(map, "functionDeclaration")?;
    let target = map
        .get("target")
        .ok_or_else(|| BidiError::invalid_argument("target is required"))?;
    let await_promise = optional_bool(map, "awaitPromise")?
        .ok_or_else(|| BidiError::invalid_argument("awaitPromise is required"))?;
    let ownership = validate_ownership(map)?;
    let (max_obj_depth, max_dom_depth, include_shadow) = validate_serialization_options(map)?;
    let _user_activation = optional_bool(map, "userActivation")?;
    let arguments = match map.get("arguments") {
        None => Value::Array(Vec::new()),
        Some(Value::Array(arr)) => Value::Array(arr.clone()),
        Some(_) => return Err(BidiError::invalid_argument("arguments must be an array")),
    };
    let this_arg = map.get("this").cloned().unwrap_or(Value::Null);
    let (tab, frame, ctx_id, sandbox) = resolve_target(state, target).await?;
    let _ = tab.add_binding(BIDI_CHANNEL_BINDING, true).await;

    let slot_id = {
        let mut guard = state.sessions.lock().await;
        guard.next_call_slot()
    };
    let realm_id = format!("realm-{ctx_id}");
    let wrapped_fn = format!("({fn_decl})");
    let run_script = format!(
        "(() => {{ {BIDI_RUNTIME_JS}\n\
         if (!host.__tbBidiSlots) host.__tbBidiSlots = Object.create(null);\n\
         const slot = {{ done: false, result: null }};\n\
         host.__tbBidiSlots[{slot_id}] = slot;\n\
         const contextId = {};\n\
         const realmId = {};\n\
         const ownership = {};\n\
         const maxObjDepth = {};\n\
         const maxDomDepth = {};\n\
         const includeShadow = {};\n\
         const awaitPromise = {};\n\
         const rawArgs = {};\n\
         const rawThis = {};\n\
         function finishOk(val) {{\n\
           slot.result = JSON.stringify({{\n\
             type: 'success',\n\
             realm: realmId,\n\
             result: __tbBidi.serialize(val, maxObjDepth, maxDomDepth, contextId, ownership, undefined, undefined, includeShadow),\n\
           }});\n\
           slot.done = true;\n\
         }}\n\
         function finishErr(err) {{\n\
           slot.result = JSON.stringify({{\n\
             type: 'exception',\n\
             realm: realmId,\n\
             exceptionDetails: __tbBidi.exceptionDetails(err, contextId, ownership),\n\
           }});\n\
           slot.done = true;\n\
         }}\n\
         let decodedArgs, decodedThis;\n\
         try {{\n\
           decodedArgs = rawArgs.map(a => __tbBidi.deserialize(a, contextId));\n\
           decodedThis = rawThis === null ? undefined : __tbBidi.deserialize(rawThis, contextId);\n\
         }} catch (err) {{\n\
           slot.result = JSON.stringify({{ __tbBidiError: String(err && err.message ? err.message : err) }});\n\
           slot.done = true;\n\
           return slot.result;\n\
         }}\n\
         try {{\n\
           const fn = host.evaluatePage({});\n\
           const value = host.apply(fn, decodedThis, decodedArgs);\n\
           if (awaitPromise && value && typeof value.then === 'function') {{\n\
             value.then(finishOk, finishErr);\n\
           }} else {{\n\
             finishOk(value);\n\
           }}\n\
         }} catch (err) {{\n\
           finishErr(err);\n\
         }}\n\
         return slot.done ? slot.result : null;\n\
         }})()",
        json!(ctx_id),
        json!(realm_id),
        json!(ownership),
        max_obj_depth,
        max_dom_depth,
        json!(include_shadow),
        json!(await_promise),
        arguments,
        this_arg,
        json!(wrapped_fn)
    );
    execute_and_collect(&tab, frame, sandbox.as_deref(), &run_script, slot_id).await
}

async fn execute_and_collect(
    tab: &TabHandle,
    frame: FrameId,
    sandbox: Option<&str>,
    script: &str,
    slot_id: u64,
) -> BidiResult {
    let source = match sandbox {
        Some(world) => browser::ScriptSource::Isolated {
            world,
            source: script,
        },
        None => browser::ScriptSource::Browser(script),
    };
    let initial = tab
        .execute_script(browser::ExecuteScriptOptions {
            frame,
            source,
            timeout: Some(Duration::from_secs(30)),
        })
        .await
        .map_err(|err| BidiError::unknown_error(err.to_string()))?;

    let raw = if let RemoteValue::String(s) = initial {
        s
    } else {
        let done_expr = format!(
            "Boolean(host.__tbBidiSlots && host.__tbBidiSlots[{slot_id}] && host.__tbBidiSlots[{slot_id}].done)"
        );
        let done_source = match sandbox {
            Some(world) => browser::ScriptSource::Isolated {
                world,
                source: done_expr.as_str(),
            },
            None => browser::ScriptSource::Browser(done_expr.as_str()),
        };
        let settled = tab
            .run_until_js_true(browser::RunUntilJsTrueOptions {
                frame,
                source: done_source,
                timeout: Duration::from_secs(30),
            })
            .await
            .map_err(|err| BidiError::unknown_error(err.to_string()))?;
        if !settled {
            return Err(BidiError::unknown_error("script promise timed out"));
        }
        let read_expr = format!("host.__tbBidiSlots[{slot_id}].result");
        let read_source = match sandbox {
            Some(world) => browser::ScriptSource::Isolated {
                world,
                source: read_expr.as_str(),
            },
            None => browser::ScriptSource::Browser(read_expr.as_str()),
        };
        match tab
            .execute_script(browser::ExecuteScriptOptions {
                frame,
                source: read_source,
                timeout: Some(Duration::from_secs(10)),
            })
            .await
            .map_err(|err| BidiError::unknown_error(err.to_string()))?
        {
            RemoteValue::String(s) => s,
            _ => return Err(BidiError::unknown_error("missing script result")),
        }
    };
    let cleanup = format!("host.__tbBidiSlots && delete host.__tbBidiSlots[{slot_id}]");
    let _ = tab
        .execute_script(browser::ExecuteScriptOptions {
            frame,
            source: browser::ScriptSource::Browser(&cleanup),
            timeout: Some(Duration::from_secs(5)),
        })
        .await;

    let parsed: Value =
        serde_json::from_str(&raw).map_err(|err| BidiError::unknown_error(err.to_string()))?;
    if let Some(err_str) = parsed.get("__tbBidiError").and_then(Value::as_str) {
        if let Some(msg) = err_str.strip_prefix("__tb_bidi_no_such_node:") {
            return Err(BidiError::no_such_node(msg));
        }
        if let Some(msg) = err_str.strip_prefix("__tb_bidi_no_such_handle:") {
            return Err(BidiError::no_such_handle(msg));
        }
        let msg = err_str
            .strip_prefix("__tb_bidi_invalid:")
            .unwrap_or(err_str);
        return Err(BidiError::invalid_argument(msg));
    }
    Ok(parsed)
}

fn validate_preload_arguments(map: &serde_json::Map<String, Value>) -> Result<(), BidiError> {
    let Some(args_val) = map.get("arguments") else {
        return Ok(());
    };
    let Some(args) = args_val.as_array() else {
        return Err(BidiError::invalid_argument("arguments must be an array"));
    };
    for item in args {
        let arg_obj = require_object(item, "preload argument")?;
        let typ = require_str(arg_obj, "type")?;
        if typ != "channel" {
            return Err(BidiError::invalid_argument(
                "preload script arguments must be channel values",
            ));
        }
        let val = arg_obj
            .get("value")
            .ok_or_else(|| BidiError::invalid_argument("channel value is required"))?;
        let val_obj = require_object(val, "channel.value")?;
        let _channel = require_str(val_obj, "channel")?;
        let _ = validate_serialization_options(val_obj)?;
        if let Some(own) = optional_str(val_obj, "ownership")?
            && !matches!(own, "none" | "root")
        {
            return Err(BidiError::invalid_argument(format!(
                "invalid ownership: {own}"
            )));
        }
    }
    Ok(())
}

async fn parse_preload_contexts(
    state: &Arc<BidiState>,
    map: &serde_json::Map<String, Value>,
) -> Result<Option<Vec<TabId>>, BidiError> {
    match map.get("contexts") {
        None => Ok(None),
        Some(Value::Array(arr)) if !arr.is_empty() => {
            let mut list = Vec::with_capacity(arr.len());
            for item in arr {
                let Some(raw) = item.as_str() else {
                    return Err(BidiError::invalid_argument("context must be a string"));
                };
                let (tab, frame) = resolve_context(state, raw).await?;
                if frame != FrameId::MAIN {
                    return Err(BidiError::invalid_argument(
                        "preload script context must be top-level",
                    ));
                }
                list.push(tab.id());
            }
            Ok(Some(list))
        }
        Some(_) => Err(BidiError::invalid_argument(
            "contexts must be a non-empty array",
        )),
    }
}

async fn parse_preload_user_contexts(
    state: &Arc<BidiState>,
    map: &serde_json::Map<String, Value>,
) -> Result<Option<Vec<UserContextId>>, BidiError> {
    match map.get("userContexts") {
        None => Ok(None),
        Some(Value::Array(arr)) if !arr.is_empty() => {
            let known = state.browser.user_contexts().await.unwrap_or_default();
            let mut list = Vec::with_capacity(arr.len());
            for item in arr {
                let Some(raw) = item.as_str() else {
                    return Err(BidiError::invalid_argument("userContext must be a string"));
                };
                let uc = UserContextId::new(raw);
                if !known.contains(&uc) {
                    return Err(BidiError::no_such_user_context(raw));
                }
                list.push(uc);
            }
            Ok(Some(list))
        }
        Some(_) => Err(BidiError::invalid_argument(
            "userContexts must be a non-empty array",
        )),
    }
}

// https://w3c.github.io/webdriver-bidi/#command-script-addPreloadScript
pub(crate) async fn add_preload_script(state: &Arc<BidiState>, params: &Value) -> BidiResult {
    let map = require_object(params, "params")?;
    let fn_decl = require_str(map, "functionDeclaration")?;
    let _sandbox = optional_str(map, "sandbox")?;
    validate_preload_arguments(map)?;
    if map.contains_key("contexts") && map.contains_key("userContexts") {
        return Err(BidiError::invalid_argument(
            "cannot specify both contexts and userContexts",
        ));
    }
    let contexts = parse_preload_contexts(state, map).await?;
    let user_contexts = parse_preload_user_contexts(state, map).await?;

    let script_id = random_id().unwrap_or_else(|_| "preload-1".to_owned());
    let wrapped = format!("(({fn_decl})())");
    let mut tab_script_ids = Vec::new();
    let target_tabs = state.browser.tabs().await.unwrap_or_default();
    for tab_id in target_tabs {
        if let Some(ref ctxs) = contexts
            && !ctxs.contains(&tab_id)
        {
            continue;
        }
        if let Ok(tab) = state.browser.tab(tab_id).await {
            if let Some(ref ucs) = user_contexts
                && !ucs.contains(tab.user_context())
            {
                continue;
            }
            if let Ok(init_id) = tab.add_init_script(wrapped.clone(), false).await {
                tab_script_ids.push((tab_id, init_id));
            }
        }
    }
    let sid = state.session_id.lock().await.clone();
    if let Some(sid) = sid {
        let mut guard = state.sessions.lock().await;
        if let Some(session) = guard.open.get_mut(&sid) {
            session.preload_scripts.insert(
                script_id.clone(),
                PreloadScriptEntry {
                    function_declaration: fn_decl.to_owned(),
                    contexts,
                    user_contexts,
                    tab_script_ids,
                },
            );
        }
    }
    Ok(json!({ "script": script_id }))
}

// https://w3c.github.io/webdriver-bidi/#command-script-removePreloadScript
pub(crate) async fn remove_preload_script(state: &Arc<BidiState>, params: &Value) -> BidiResult {
    let map = require_object(params, "params")?;
    let script_id = require_str(map, "script")?;
    let removed = {
        let sid = state.session_id.lock().await.clone();
        let mut guard = state.sessions.lock().await;
        sid.and_then(|id| guard.open.get_mut(&id))
            .and_then(|s| s.preload_scripts.remove(script_id))
    };
    let Some(entry) = removed else {
        return Err(BidiError::no_such_script(format!(
            "unknown preload script: {script_id}"
        )));
    };
    for (tab_id, init_id) in entry.tab_script_ids {
        if let Ok(tab) = state.browser.tab(tab_id).await {
            let _ = tab.remove_init_script(init_id).await;
        }
    }
    Ok(json!({}))
}

// https://w3c.github.io/webdriver-bidi/#command-script-disown
pub(crate) async fn disown(state: &Arc<BidiState>, params: &Value) -> BidiResult {
    let map = require_object(params, "params")?;
    let target = map
        .get("target")
        .ok_or_else(|| BidiError::invalid_argument("target is required"))?;
    let Some(handles) = map.get("handles").and_then(Value::as_array) else {
        return Err(BidiError::invalid_argument("handles must be an array"));
    };
    if !handles.iter().all(Value::is_string) {
        return Err(BidiError::invalid_argument(
            "handles must be an array of strings",
        ));
    }
    let (tab, frame, _ctx_id, sandbox) = resolve_target(state, target).await?;
    let script = format!(
        "(() => {{\n\
           if (host.__tbBidiState && host.__tbBidiState.handles) {{\n\
             for (const h of {}) delete host.__tbBidiState.handles[h];\n\
           }}\n\
         }})()",
        Value::Array(handles.clone())
    );
    let source = match sandbox.as_deref() {
        Some(world) => browser::ScriptSource::Isolated {
            world,
            source: script.as_str(),
        },
        None => browser::ScriptSource::Browser(script.as_str()),
    };
    let _ = tab
        .execute_script(browser::ExecuteScriptOptions {
            frame,
            source,
            timeout: Some(Duration::from_secs(10)),
        })
        .await;
    Ok(json!({}))
}

// https://w3c.github.io/webdriver-bidi/#command-script-getRealms
pub(crate) async fn get_realms(state: &Arc<BidiState>, params: &Value) -> BidiResult {
    let map = require_object(params, "params")?;
    let filter_ctx = optional_str(map, "context")?;
    let filter_type = optional_str(map, "type")?;
    if let Some(t) = filter_type
        && !matches!(
            t,
            "window"
                | "dedicated-worker"
                | "shared-worker"
                | "service-worker"
                | "worker"
                | "paint-worklet"
                | "audio-worklet"
                | "worklet"
        )
    {
        return Err(BidiError::invalid_argument(format!(
            "invalid realm type: {t}"
        )));
    }
    if filter_type.is_some_and(|t| t != "window") {
        return Ok(json!({ "realms": [] }));
    }
    let mut realms = Vec::new();
    if let Some(ctx) = filter_ctx {
        let (tab, frame) = resolve_context(state, ctx).await?;
        let url = tab.document_url_in(frame).await.unwrap_or_default();
        let origin = url::Url::parse(&url)
            .map_or_else(|_| "null".to_owned(), |u| u.origin().ascii_serialization());
        realms.push(json!({
            "realm": format!("realm-{ctx}"),
            "origin": origin,
            "type": "window",
            "context": ctx,
        }));
    } else if let Ok(ids) = state.browser.tabs().await {
        for id in ids {
            if let Ok(tab) = state.browser.tab(id).await
                && let Ok(frames) = tab.frames().await
            {
                for frame in frames {
                    let ctx = format_context_id(id, frame.id);
                    let origin = url::Url::parse(&frame.url)
                        .map_or_else(|_| "null".to_owned(), |u| u.origin().ascii_serialization());
                    realms.push(json!({
                        "realm": format!("realm-{ctx}"),
                        "origin": origin,
                        "type": "window",
                        "context": ctx,
                    }));
                }
            }
        }
    }
    Ok(json!({ "realms": realms }))
}
