// https://w3c.github.io/webdriver-bidi/#module-input

use std::f64::consts::{FRAC_PI_2, TAU};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use browser::RemoteValue;
use serde_json::{Map, Value, json};
use unicode_segmentation::UnicodeSegmentation;

use crate::bidi::BidiState;
use crate::bidi::browsing_context::resolve_context;
use crate::bidi::protocol::{BIDI_RUNTIME_JS, BidiError, BidiResult, require_object, require_str};
use crate::session::MAX_SAFE_INT;

fn validate_non_neg_int(map: &Map<String, Value>, key: &str) -> Result<Option<u64>, BidiError> {
    let Some(val) = map.get(key) else {
        return Ok(None);
    };
    let Some(n) = val.as_u64() else {
        return Err(BidiError::invalid_argument(format!(
            "{key} must be a non-negative integer"
        )));
    };
    if n > MAX_SAFE_INT {
        return Err(BidiError::invalid_argument(format!("{key} out of range")));
    }
    Ok(Some(n))
}

fn require_int_in_safe_range(map: &Map<String, Value>, key: &str) -> Result<i64, BidiError> {
    let Some(val) = map.get(key) else {
        return Err(BidiError::invalid_argument(format!("{key} is required")));
    };
    let Some(n) = val.as_i64() else {
        return Err(BidiError::invalid_argument(format!(
            "{key} must be an integer"
        )));
    };
    let max = i64::try_from(MAX_SAFE_INT).unwrap_or(i64::MAX);
    if !(-max..=max).contains(&n) {
        return Err(BidiError::invalid_argument(format!("{key} out of range")));
    }
    Ok(n)
}

fn require_number(map: &Map<String, Value>, key: &str) -> Result<f64, BidiError> {
    let Some(val) = map.get(key).and_then(Value::as_f64) else {
        return Err(BidiError::invalid_argument(format!(
            "{key} must be a number"
        )));
    };
    if !val.is_finite() {
        return Err(BidiError::invalid_argument(format!("{key} must be finite")));
    }
    Ok(val)
}

fn validate_pointer_common(item: &Map<String, Value>) -> Result<(), BidiError> {
    let _ = validate_non_neg_int(item, "width")?;
    let _ = validate_non_neg_int(item, "height")?;
    if let Some(p) = item.get("pressure")
        && p.as_f64().is_none_or(|v| !v.is_finite())
    {
        return Err(BidiError::invalid_argument("pressure must be a number"));
    }
    if let Some(tp) = item.get("tangentialPressure")
        && tp.as_f64().is_none_or(|v| !v.is_finite())
    {
        return Err(BidiError::invalid_argument(
            "tangentialPressure must be a number",
        ));
    }
    if let Some(twist) = validate_non_neg_int(item, "twist")?
        && twist > 359
    {
        return Err(BidiError::invalid_argument("twist must be in 0..=359"));
    }
    if let Some(alt) = item.get("altitudeAngle")
        && alt
            .as_f64()
            .is_none_or(|v| !v.is_finite() || !(0.0..=FRAC_PI_2).contains(&v))
    {
        return Err(BidiError::invalid_argument(
            "altitudeAngle must be in [0, pi/2]",
        ));
    }
    if let Some(az) = item.get("azimuthAngle")
        && az
            .as_f64()
            .is_none_or(|v| !v.is_finite() || !(0.0..=TAU).contains(&v))
    {
        return Err(BidiError::invalid_argument(
            "azimuthAngle must be in [0, 2*pi]",
        ));
    }
    Ok(())
}

fn validate_origin(origin: &Value, allow_pointer: bool) -> Result<Option<String>, BidiError> {
    if let Some(s) = origin.as_str() {
        if s == "viewport" || (allow_pointer && s == "pointer") {
            return Ok(None);
        }
        return Err(BidiError::invalid_argument(format!("invalid origin: {s}")));
    }
    let obj = require_object(origin, "origin")?;
    let typ = require_str(obj, "type")?;
    if typ != "element" {
        return Err(BidiError::invalid_argument(format!(
            "invalid origin type: {typ}"
        )));
    }
    let el = obj
        .get("element")
        .ok_or_else(|| BidiError::invalid_argument("origin.element is required"))?;
    let el_obj = require_object(el, "origin.element")?;
    let shared_id = require_str(el_obj, "sharedId")?;
    Ok(Some(shared_id.to_owned()))
}

fn normalize_action_sources(actions: &[Value]) -> Result<Vec<Value>, BidiError> {
    let mut normalized = Vec::with_capacity(actions.len());
    for source in actions {
        let obj = require_object(source, "action source")?;
        let typ = require_str(obj, "type")?;
        if !matches!(typ, "none" | "key" | "pointer" | "wheel") {
            return Err(BidiError::invalid_argument(format!(
                "invalid action source type: {typ}"
            )));
        }
        let _id = require_str(obj, "id")?;
        if typ == "pointer"
            && let Some(params_val) = obj.get("parameters")
        {
            let p_obj = require_object(params_val, "parameters")?;
            if let Some(pt_val) = p_obj.get("pointerType") {
                let Some(pt) = pt_val.as_str() else {
                    return Err(BidiError::invalid_argument("pointerType must be a string"));
                };
                if !matches!(pt, "mouse" | "pen" | "touch") {
                    return Err(BidiError::invalid_argument(format!(
                        "invalid pointerType: {pt}"
                    )));
                }
            }
        }
        let Some(items) = obj.get("actions").and_then(Value::as_array) else {
            return Err(BidiError::invalid_argument(
                "source.actions must be an array",
            ));
        };
        let mut norm_source = source.clone();
        if let Some(norm_items) = norm_source.get_mut("actions").and_then(Value::as_array_mut) {
            validate_source_items(typ, items, norm_items)?;
        }
        normalized.push(norm_source);
    }
    Ok(normalized)
}

fn validate_source_items(
    typ: &str,
    items: &[Value],
    norm_items: &mut [Value],
) -> Result<(), BidiError> {
    for (idx, item) in items.iter().enumerate() {
        let item_obj = require_object(item, "action item")?;
        let item_type = require_str(item_obj, "type")?;
        match (typ, item_type) {
            (_, "pause") => {
                let _ = validate_non_neg_int(item_obj, "duration")?;
            }
            ("key", "keyDown" | "keyUp") => {
                let val = require_str(item_obj, "value")?;
                if val.graphemes(true).count() != 1 {
                    return Err(BidiError::invalid_argument(
                        "key action value must be a single character",
                    ));
                }
            }
            ("pointer", "pointerCancel") => {}
            ("pointer", "pointerDown" | "pointerUp") => {
                if validate_non_neg_int(item_obj, "button")?.is_none() {
                    return Err(BidiError::invalid_argument("button is required"));
                }
                validate_pointer_common(item_obj)?;
            }
            ("pointer", "pointerMove") => {
                let _ = require_number(item_obj, "x")?;
                let _ = require_number(item_obj, "y")?;
                let _ = validate_non_neg_int(item_obj, "duration")?;
                validate_pointer_common(item_obj)?;
                if let Some(origin) = item_obj.get("origin")
                    && let Some(shared_id) = validate_origin(origin, true)?
                    && let Some(norm_item) = norm_items.get_mut(idx)
                {
                    norm_item["origin"] = json!({ "__tbBidiSharedId": shared_id });
                }
            }
            ("wheel", "scroll") => {
                let _ = require_int_in_safe_range(item_obj, "x")?;
                let _ = require_int_in_safe_range(item_obj, "y")?;
                let _ = require_int_in_safe_range(item_obj, "deltaX")?;
                let _ = require_int_in_safe_range(item_obj, "deltaY")?;
                let _ = validate_non_neg_int(item_obj, "duration")?;
                if let Some(origin) = item_obj.get("origin")
                    && let Some(shared_id) = validate_origin(origin, false)?
                    && let Some(norm_item) = norm_items.get_mut(idx)
                {
                    norm_item["origin"] = json!({ "__tbBidiSharedId": shared_id });
                }
            }
            _ => {
                return Err(BidiError::invalid_argument(format!(
                    "invalid action type '{item_type}' for source '{typ}'"
                )));
            }
        }
    }
    Ok(())
}

// https://w3c.github.io/webdriver-bidi/#command-input-performActions
pub(crate) async fn perform_actions(state: &Arc<BidiState>, params: &Value) -> BidiResult {
    let map = require_object(params, "params")?;
    let ctx = require_str(map, "context")?;
    let Some(actions) = map.get("actions").and_then(Value::as_array) else {
        return Err(BidiError::invalid_argument("actions must be an array"));
    };
    let (tab, frame) = resolve_context(state, ctx).await?;
    let normalized = normalize_action_sources(actions)?;
    let script = format!(
        "(() => {{ {BIDI_RUNTIME_JS}\n\
         const actions = {};\n\
         const vw = globalThis.innerWidth || 800;\n\
         const vh = globalThis.innerHeight || 600;\n\
         for (const source of actions) {{\n\
           if (!Array.isArray(source.actions)) continue;\n\
           let px = 0, py = 0;\n\
           for (const item of source.actions) {{\n\
             if (!item) continue;\n\
             if (item.origin && typeof item.origin.__tbBidiSharedId === 'string') {{\n\
               const node = __tbBidi.st.sharedById[item.origin.__tbBidiSharedId];\n\
               if (!node) return '__tb_no_such_node';\n\
               if (node.nodeType !== 1) return '__tb_no_such_element';\n\
               const rect = node.getBoundingClientRect ? node.getBoundingClientRect() : {{left:0,top:0,width:0,height:0}};\n\
               item.origin = 'viewport';\n\
               item.x = Math.round(rect.left + rect.width / 2 + (item.x || 0));\n\
               item.y = Math.round(rect.top + rect.height / 2 + (item.y || 0));\n\
             }}\n\
             if (item.type === 'pointerMove' || item.type === 'scroll') {{\n\
               const tx = item.origin === 'pointer' ? px + (item.x || 0) : (item.x || 0);\n\
               const ty = item.origin === 'pointer' ? py + (item.y || 0) : (item.y || 0);\n\
               if (tx < 0 || ty < 0 || tx > vw || ty > vh) return '__tb_oob';\n\
               if (item.type === 'pointerMove') {{ px = tx; py = ty; }}\n\
             }}\n\
           }}\n\
         }}\n\
         return host.__tbWebDriverActions(actions);\n\
         }})()",
        Value::Array(normalized)
    );
    match tab
        .execute_script(browser::ExecuteScriptOptions {
            frame,
            source: browser::ScriptSource::Browser(&script),
            timeout: Some(Duration::from_secs(30)),
        })
        .await
    {
        Ok(RemoteValue::String(s)) if s == "__tb_no_such_node" => {
            Err(BidiError::no_such_node("action origin element not found"))
        }
        Ok(RemoteValue::String(s)) if s == "__tb_no_such_element" => Err(
            BidiError::no_such_element("action origin is not an element"),
        ),
        Ok(RemoteValue::String(s)) if s == "__tb_oob" => Err(BidiError::move_target_out_of_bounds(
            "move target is outside viewport",
        )),
        Ok(RemoteValue::Bool(true)) => Ok(json!({})),
        Ok(_) => Err(BidiError::unknown_error("actions failed")),
        Err(err) => Err(BidiError::invalid_argument(err.to_string())),
    }
}

// https://w3c.github.io/webdriver-bidi/#command-input-releaseActions
pub(crate) async fn release_actions(state: &Arc<BidiState>, params: &Value) -> BidiResult {
    let map = require_object(params, "params")?;
    let ctx = require_str(map, "context")?;
    let (_tab, _frame) = resolve_context(state, ctx).await?;
    Ok(json!({}))
}

// https://w3c.github.io/webdriver-bidi/#command-input-setFiles
pub(crate) async fn set_files(state: &Arc<BidiState>, params: &Value) -> BidiResult {
    let map = require_object(params, "params")?;
    let ctx = require_str(map, "context")?;
    let element = map
        .get("element")
        .ok_or_else(|| BidiError::invalid_argument("element is required"))?;
    let el_obj = require_object(element, "element")?;
    let shared_id = require_str(el_obj, "sharedId")?;
    let Some(files) = map.get("files").and_then(Value::as_array) else {
        return Err(BidiError::invalid_argument("files must be an array"));
    };
    let mut file_names = Vec::with_capacity(files.len());
    for f in files {
        let Some(raw) = f.as_str() else {
            return Err(BidiError::invalid_argument(
                "files must be an array of strings",
            ));
        };
        let name = Path::new(raw)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(raw)
            .to_owned();
        file_names.push(name);
    }
    let (tab, frame) = resolve_context(state, ctx).await?;
    let script = format!(
        "(() => {{ {BIDI_RUNTIME_JS}\n\
         const el = __tbBidi.st.sharedById[{}];\n\
         if (!el) return 'no_such_node';\n\
         if (el.nodeType !== 1) return 'no_such_element';\n\
         if (!el.tagName || el.tagName.toUpperCase() !== 'INPUT' || el.type !== 'file') return 'unable';\n\
         if (el.disabled) return 'unable';\n\
         const names = {};\n\
         if (names.length > 1 && !el.multiple) return 'unable';\n\
         const list = names.map(name => typeof File === 'function' ? new File([], name) : {{ name, size: 0, type: '' }});\n\
         list.item = i => (i >= 0 && i < list.length ? list[i] : null);\n\
         try {{\n\
           if (typeof DataTransfer === 'function') {{\n\
             const dt = new DataTransfer();\n\
             for (const f of list) dt.items.add(f);\n\
             el.files = dt.files;\n\
           }} else {{\n\
             Object.defineProperty(el, 'files', {{ value: list, configurable: true }});\n\
           }}\n\
         }} catch (_) {{\n\
           Object.defineProperty(el, 'files', {{ value: list, configurable: true }});\n\
         }}\n\
         el.dispatchEvent(new Event('input', {{ bubbles: true }}));\n\
         el.dispatchEvent(new Event('change', {{ bubbles: true }}));\n\
         return 'ok';\n\
         }})()",
        json!(shared_id),
        json!(file_names)
    );
    match tab
        .execute_script(browser::ExecuteScriptOptions {
            frame,
            source: browser::ScriptSource::Browser(&script),
            timeout: Some(Duration::from_secs(10)),
        })
        .await
    {
        Ok(RemoteValue::String(s)) if s == "ok" => Ok(json!({})),
        Ok(RemoteValue::String(s)) if s == "no_such_node" => {
            Err(BidiError::no_such_node("element not found"))
        }
        Ok(RemoteValue::String(s)) if s == "no_such_element" => {
            Err(BidiError::no_such_element("node is not an element"))
        }
        Ok(_) => Err(BidiError::new(
            "unable to set file input",
            "element is not a settable file input",
        )),
        Err(err) => Err(BidiError::unknown_error(err.to_string())),
    }
}
