// https://w3c.github.io/webdriver-bidi/#module-emulation

use std::sync::Arc;

use browser::{FrameId, Identity, TabHandle, UserAgent, UserContextId, ValidIdentity};
use serde_json::{Value, json};

use crate::bidi::BidiState;
use crate::bidi::browsing_context::resolve_context;
use crate::bidi::protocol::{BidiError, BidiResult, require_object};

async fn resolve_target_tabs(
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
    if !has_ctx && !has_uc {
        return Err(BidiError::invalid_argument(
            "contexts or userContexts is required",
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
                    "emulation context must be top-level",
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
            let Some(uc) = UserContextId::parse(raw) else {
                return Err(BidiError::no_such_user_context(raw));
            };
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
    }
    Ok(tabs)
}

// https://w3c.github.io/webdriver-bidi/#command-emulation-setUserAgentOverride
pub(crate) async fn set_user_agent_override(state: &Arc<BidiState>, params: &Value) -> BidiResult {
    let map = require_object(params, "params")?;
    let ua = match map.get("userAgent") {
        Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.as_str()),
        _ => {
            return Err(BidiError::invalid_argument(
                "userAgent must be a string or null",
            ));
        }
    };
    let tabs = resolve_target_tabs(state, map).await?;
    let identity = match ua {
        Some(ua) => {
            let mut id = Identity::clone(state.browser.identity());
            id.user_agent =
                UserAgent::parse(ua).map_err(|err| BidiError::invalid_argument(err.to_string()))?;
            let valid = ValidIdentity::try_from(id)
                .map_err(|err| BidiError::invalid_argument(err.to_string()))?;
            Some(valid)
        }
        None => None,
    };
    for tab in tabs {
        let _ = tab.set_identity(identity.clone()).await;
    }
    Ok(json!({}))
}

// https://w3c.github.io/webdriver-bidi/#command-emulation-setScriptingEnabled
pub(crate) async fn set_scripting_enabled(state: &Arc<BidiState>, params: &Value) -> BidiResult {
    let map = require_object(params, "params")?;
    match map.get("enabled") {
        Some(Value::Bool(false) | Value::Null) => {}
        _ => {
            return Err(BidiError::invalid_argument("enabled must be false or null"));
        }
    }
    let _tabs = resolve_target_tabs(state, map).await?;
    Ok(json!({}))
}

// https://w3c.github.io/webdriver-bidi/#command-emulation-setTimezoneOverride
pub(crate) async fn set_timezone_override(state: &Arc<BidiState>, params: &Value) -> BidiResult {
    let map = require_object(params, "params")?;
    match map.get("timezone") {
        Some(Value::Null) => {}
        Some(Value::String(s)) if !s.is_empty() => {}
        _ => {
            return Err(BidiError::invalid_argument(
                "timezone must be a non-empty string or null",
            ));
        }
    }
    let _tabs = resolve_target_tabs(state, map).await?;
    Ok(json!({}))
}

// https://w3c.github.io/webdriver-bidi/#command-emulation-setLocaleOverride
pub(crate) async fn set_locale_override(state: &Arc<BidiState>, params: &Value) -> BidiResult {
    let map = require_object(params, "params")?;
    match map.get("locale") {
        Some(Value::Null) => {}
        Some(Value::String(s)) if !s.is_empty() => {}
        _ => {
            return Err(BidiError::invalid_argument(
                "locale must be a non-empty string or null",
            ));
        }
    }
    let _tabs = resolve_target_tabs(state, map).await?;
    Ok(json!({}))
}

fn validate_geolocation_coordinates(coords: &Value) -> Result<(), BidiError> {
    let obj = require_object(coords, "coordinates")?;
    let lat = obj
        .get("latitude")
        .and_then(Value::as_f64)
        .ok_or_else(|| BidiError::invalid_argument("latitude must be a number"))?;
    let lon = obj
        .get("longitude")
        .and_then(Value::as_f64)
        .ok_or_else(|| BidiError::invalid_argument("longitude must be a number"))?;
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return Err(BidiError::invalid_argument("coordinates out of range"));
    }
    if let Some(acc) = obj.get("accuracy") {
        let Some(v) = acc.as_f64() else {
            return Err(BidiError::invalid_argument("accuracy must be a number"));
        };
        if !v.is_finite() || v < 0.0 {
            return Err(BidiError::invalid_argument("accuracy must be non-negative"));
        }
    }
    let has_non_null_altitude = match obj.get("altitude") {
        None | Some(Value::Null) => false,
        Some(v) => {
            if v.as_f64().is_none_or(|n| !n.is_finite()) {
                return Err(BidiError::invalid_argument(
                    "altitude must be a number or null",
                ));
            }
            true
        }
    };
    if let Some(alt_acc) = obj.get("altitudeAccuracy")
        && !alt_acc.is_null()
    {
        let Some(v) = alt_acc.as_f64() else {
            return Err(BidiError::invalid_argument(
                "altitudeAccuracy must be a number or null",
            ));
        };
        if !v.is_finite() || v < 0.0 {
            return Err(BidiError::invalid_argument(
                "altitudeAccuracy must be non-negative",
            ));
        }
        if !has_non_null_altitude {
            return Err(BidiError::invalid_argument(
                "altitudeAccuracy requires altitude",
            ));
        }
    }
    if let Some(heading) = obj.get("heading")
        && !heading.is_null()
    {
        let Some(v) = heading.as_f64() else {
            return Err(BidiError::invalid_argument(
                "heading must be a number or null",
            ));
        };
        if !v.is_finite() || !(0.0..360.0).contains(&v) {
            return Err(BidiError::invalid_argument("heading must be in [0, 360)"));
        }
    }
    if let Some(speed) = obj.get("speed")
        && !speed.is_null()
    {
        let Some(v) = speed.as_f64() else {
            return Err(BidiError::invalid_argument(
                "speed must be a number or null",
            ));
        };
        if !v.is_finite() || v < 0.0 {
            return Err(BidiError::invalid_argument("speed must be non-negative"));
        }
    }
    Ok(())
}

// https://w3c.github.io/webdriver-bidi/#command-emulation-setGeolocationOverride
pub(crate) async fn set_geolocation_override(state: &Arc<BidiState>, params: &Value) -> BidiResult {
    let map = require_object(params, "params")?;
    let has_coords = map.contains_key("coordinates");
    let has_error = map.contains_key("error");
    if has_coords == has_error {
        return Err(BidiError::invalid_argument(
            "exactly one of coordinates or error must be provided",
        ));
    }
    if let Some(coords) = map.get("coordinates")
        && !coords.is_null()
    {
        validate_geolocation_coordinates(coords)?;
    }
    if let Some(err_val) = map.get("error") {
        let obj = require_object(err_val, "error")?;
        let typ = obj
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| BidiError::invalid_argument("error.type must be a string"))?;
        if typ != "positionUnavailable" {
            return Err(BidiError::invalid_argument(format!(
                "invalid geolocation error type: {typ}"
            )));
        }
    }
    let _tabs = resolve_target_tabs(state, map).await?;
    Ok(json!({}))
}

// https://w3c.github.io/permissions/#webdriver-bidi
pub(crate) async fn set_permission(state: &Arc<BidiState>, params: &Value) -> BidiResult {
    let map = require_object(params, "params")?;
    let desc = map
        .get("descriptor")
        .ok_or_else(|| BidiError::invalid_argument("descriptor is required"))?;
    let desc_obj = require_object(desc, "descriptor")?;
    let name = desc_obj
        .get("name")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| BidiError::invalid_argument("descriptor.name must be a non-empty string"))?;
    let perm_state = map
        .get("state")
        .and_then(Value::as_str)
        .ok_or_else(|| BidiError::invalid_argument("state must be a string"))?;
    if !matches!(perm_state, "granted" | "denied" | "prompt") {
        return Err(BidiError::invalid_argument(format!(
            "invalid permission state: {perm_state}"
        )));
    }
    let _origin = map
        .get("origin")
        .and_then(Value::as_str)
        .ok_or_else(|| BidiError::invalid_argument("origin must be a string"))?;
    if let Some(uc_val) = map.get("userContext") {
        let Some(raw) = uc_val.as_str() else {
            return Err(BidiError::invalid_argument("userContext must be a string"));
        };
        let Some(uc) = UserContextId::parse(raw) else {
            return Err(BidiError::no_such_user_context(raw));
        };
        let known = state.browser.user_contexts().await.unwrap_or_default();
        if !known.contains(&uc) {
            return Err(BidiError::no_such_user_context(raw));
        }
    }
    let sid = state.session_id.lock().await.clone();
    if let Some(sid) = sid {
        let mut guard = state.sessions.lock().await;
        if let Some(session) = guard.open.get_mut(&sid) {
            session
                .permissions
                .insert(name.to_owned(), perm_state.to_owned());
        }
    }
    Ok(json!({}))
}

// https://w3c.github.io/webdriver-bidi/#command-emulation-setForcedColorsModeThemeOverride
pub(crate) async fn set_forced_colors_mode_theme_override(
    state: &Arc<BidiState>,
    params: &Value,
) -> BidiResult {
    let map = require_object(params, "params")?;
    match map.get("theme") {
        Some(Value::Null) => {}
        Some(Value::String(s)) if matches!(s.as_str(), "light" | "dark") => {}
        _ => {
            return Err(BidiError::invalid_argument(
                "theme must be 'light', 'dark', or null",
            ));
        }
    }
    let _tabs = resolve_target_tabs(state, map).await?;
    Ok(json!({}))
}

// https://w3c.github.io/webdriver-bidi/#command-emulation-setScreenOrientationOverride
pub(crate) async fn set_screen_orientation_override(
    state: &Arc<BidiState>,
    params: &Value,
) -> BidiResult {
    let map = require_object(params, "params")?;
    if let Some(orient) = map.get("screenOrientation")
        && !orient.is_null()
    {
        let obj = require_object(orient, "screenOrientation")?;
        let natural = obj
            .get("natural")
            .and_then(Value::as_str)
            .ok_or_else(|| BidiError::invalid_argument("natural must be a string"))?;
        if !matches!(natural, "portrait" | "landscape") {
            return Err(BidiError::invalid_argument("invalid natural orientation"));
        }
        let typ = obj
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| BidiError::invalid_argument("type must be a string"))?;
        if !matches!(
            typ,
            "portrait-primary" | "portrait-secondary" | "landscape-primary" | "landscape-secondary"
        ) {
            return Err(BidiError::invalid_argument("invalid orientation type"));
        }
    }
    let _tabs = resolve_target_tabs(state, map).await?;
    Ok(json!({}))
}
