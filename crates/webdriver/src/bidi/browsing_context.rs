// https://w3c.github.io/webdriver-bidi/#module-browsingContext

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use browser::{
    FrameId, FrameInfo, RemoteValue, ScreenshotClip, ScreenshotFormat, ScreenshotRequest, TabError,
    TabEvent, TabHandle, TabId, UserContextId,
};
use serde_json::{Map, Value, json};

use crate::bidi::BidiState;
use crate::bidi::protocol::{
    BIDI_RUNTIME_JS, BidiError, BidiResult, optional_bool, optional_str, require_object,
    require_str,
};
use crate::session::{format_context_id, parse_context_id, random_id};

pub(crate) async fn resolve_context(
    state: &Arc<BidiState>,
    raw: &str,
) -> Result<(TabHandle, FrameId), BidiError> {
    let Some((tab_id, frame_id)) = parse_context_id(raw) else {
        return Err(BidiError::no_such_frame(format!("unknown context: {raw}")));
    };
    let tab = state
        .browser
        .tab(tab_id)
        .await
        .map_err(|_| BidiError::no_such_frame(format!("unknown context: {raw}")))?;
    if frame_id != FrameId::MAIN {
        let frames = tab
            .frames()
            .await
            .map_err(|err| BidiError::unknown_error(err.to_string()))?;
        if !frames.iter().any(|f| f.id == frame_id) {
            return Err(BidiError::no_such_frame(format!("unknown frame: {raw}")));
        }
    }
    Ok((tab, frame_id))
}

// https://w3c.github.io/webdriver-bidi/#command-browsingContext-create
pub(crate) async fn create(state: &Arc<BidiState>, params: &Value) -> BidiResult {
    let map = require_object(params, "params")?;
    let kind = require_str(map, "type")?;
    if !matches!(kind, "tab" | "window") {
        return Err(BidiError::invalid_argument(format!("invalid type: {kind}")));
    }
    let _background = optional_bool(map, "background")?;
    let reference_context = optional_str(map, "referenceContext")?;
    let user_context = match optional_str(map, "userContext")? {
        Some(raw) => {
            let uc = UserContextId::new(raw);
            let known = state.browser.user_contexts().await.unwrap_or_default();
            if !known.contains(&uc) {
                return Err(BidiError::no_such_user_context(format!(
                    "unknown user context: {raw}"
                )));
            }
            uc
        }
        None => {
            if let Some(ref_ctx) = reference_context {
                let (tab, frame) = resolve_context(state, ref_ctx).await?;
                if frame != FrameId::MAIN {
                    return Err(BidiError::invalid_argument(
                        "referenceContext must be a top-level browsing context",
                    ));
                }
                tab.user_context().clone()
            } else {
                UserContextId::default_context()
            }
        }
    };
    if let Some(ref_ctx) = reference_context {
        let (_tab, frame) = resolve_context(state, ref_ctx).await?;
        if frame != FrameId::MAIN {
            return Err(BidiError::invalid_argument(
                "referenceContext must be a top-level browsing context",
            ));
        }
    }
    let tab = state
        .browser
        .create_tab_in(user_context.clone())
        .await
        .map_err(|err| BidiError::unknown_error(err.to_string()))?;
    let _ = tab.load_html("<!doctype html><title></title>").await;
    state.attach_tab(tab.id()).await;
    apply_session_preload_scripts(state, &tab).await;
    Ok(json!({
        "context": format_context_id(tab.id(), FrameId::MAIN),
        "userContext": user_context.as_str(),
    }))
}

async fn apply_session_preload_scripts(state: &Arc<BidiState>, tab: &TabHandle) {
    let sid = state.session_id.lock().await.clone();
    let entries = {
        let guard = state.sessions.lock().await;
        sid.as_deref()
            .and_then(|id| guard.open.get(id))
            .map_or_default(|s| {
                s.preload_scripts
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect::<Vec<_>>()
            })
    };
    let mut added = Vec::new();
    for (script_id, entry) in entries {
        if let Some(ref contexts) = entry.contexts
            && !contexts.contains(&tab.id())
        {
            continue;
        }
        if let Some(ref ucs) = entry.user_contexts
            && !ucs.contains(tab.user_context())
        {
            continue;
        }
        let code = format!("(({}\n)())", entry.function_declaration);
        if let Ok(init_id) = tab.add_init_script(code, false).await {
            added.push((script_id, init_id));
        }
    }
    if !added.is_empty()
        && let Some(sid) = sid
    {
        let mut guard = state.sessions.lock().await;
        if let Some(session) = guard.open.get_mut(&sid) {
            for (script_id, init_id) in added {
                if let Some(entry) = session.preload_scripts.get_mut(&script_id) {
                    entry.tab_script_ids.push((tab.id(), init_id));
                }
            }
        }
    }
}

// https://w3c.github.io/webdriver-bidi/#command-browsingContext-close
pub(crate) async fn close(state: &Arc<BidiState>, params: &Value) -> BidiResult {
    let map = require_object(params, "params")?;
    let ctx = require_str(map, "context")?;
    let _prompt_unload = optional_bool(map, "promptUnload")?;
    let (tab, frame) = resolve_context(state, ctx).await?;
    if frame != FrameId::MAIN {
        return Err(BidiError::invalid_argument(
            "cannot close a non-top-level browsing context",
        ));
    }
    state
        .browser
        .close_tab(tab.id())
        .await
        .map_err(|_| BidiError::no_such_frame(format!("unknown context: {ctx}")))?;
    Ok(json!({}))
}

// https://w3c.github.io/webdriver-bidi/#command-browsingContext-getTree
pub(crate) async fn get_tree(state: &Arc<BidiState>, params: &Value) -> BidiResult {
    let map = require_object(params, "params")?;
    let max_depth = match map.get("maxDepth") {
        None => None,
        Some(v) => {
            let Some(n) = v.as_u64() else {
                return Err(BidiError::invalid_argument(
                    "maxDepth must be a non-negative integer",
                ));
            };
            Some(usize::try_from(n).unwrap_or(usize::MAX))
        }
    };
    let root = optional_str(map, "root")?;
    let contexts = if let Some(root_id) = root {
        let (tab, frame) = resolve_context(state, root_id).await?;
        vec![build_tree_info(&tab, frame, max_depth, 0).await?]
    } else {
        let ids = state
            .browser
            .tabs()
            .await
            .map_err(|err| BidiError::unknown_error(err.to_string()))?;
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if let Ok(tab) = state.browser.tab(id).await {
                out.push(build_tree_info(&tab, FrameId::MAIN, max_depth, 0).await?);
            }
        }
        out
    };
    Ok(json!({ "contexts": contexts }))
}

async fn build_tree_info(
    tab: &TabHandle,
    root: FrameId,
    max_depth: Option<usize>,
    depth: usize,
) -> Result<Value, BidiError> {
    let frames = tab
        .frames()
        .await
        .map_err(|err| BidiError::unknown_error(err.to_string()))?;
    let opener = tab.opener().await.ok().flatten();
    let tab_id = tab.id();
    let uc = tab.user_context().as_str();
    let Some(node) = frames.iter().find(|f| f.id == root) else {
        return Err(BidiError::no_such_frame("frame not found"));
    };
    Ok(build_frame_json(
        tab_id, uc, opener, node, &frames, max_depth, depth,
    ))
}

fn build_frame_json(
    tab_id: TabId,
    user_context: &str,
    opener: Option<TabId>,
    frame: &FrameInfo,
    all: &[FrameInfo],
    max_depth: Option<usize>,
    depth: usize,
) -> Value {
    let children = if max_depth.is_some_and(|max| depth >= max) {
        Value::Null
    } else {
        let kids: Vec<Value> = all
            .iter()
            .filter(|f| f.parent == Some(frame.id))
            .map(|child| {
                build_frame_json(tab_id, user_context, None, child, all, max_depth, depth + 1)
            })
            .collect();
        Value::Array(kids)
    };
    let mut obj = Map::new();
    obj.insert("context".into(), json!(format_context_id(tab_id, frame.id)));
    obj.insert("url".into(), json!(frame.url));
    obj.insert("children".into(), children);
    if depth == 0 {
        obj.insert(
            "parent".into(),
            match frame.parent {
                Some(p) => json!(format_context_id(tab_id, p)),
                None => Value::Null,
            },
        );
    }
    obj.insert("userContext".into(), json!(user_context));
    obj.insert(
        "originalOpener".into(),
        match opener {
            Some(op) if frame.id == FrameId::MAIN => json!(format_context_id(op, FrameId::MAIN)),
            _ => Value::Null,
        },
    );
    obj.insert(
        "clientWindow".into(),
        json!(format_context_id(tab_id, FrameId::MAIN)),
    );
    Value::Object(obj)
}

// https://w3c.github.io/webdriver-bidi/#command-browsingContext-navigate
pub(crate) async fn navigate(state: &Arc<BidiState>, params: &Value) -> BidiResult {
    let map = require_object(params, "params")?;
    let ctx = require_str(map, "context")?;
    let url_spec = require_str(map, "url")?;
    let wait = optional_str(map, "wait")?.unwrap_or("none");
    if !matches!(wait, "none" | "interactive" | "complete") {
        return Err(BidiError::invalid_argument(format!(
            "invalid wait readiness: {wait}"
        )));
    }
    let (tab, frame) = resolve_context(state, ctx).await?;
    let base_str = tab
        .document_url_in(frame)
        .await
        .unwrap_or_else(|_| "about:blank".to_owned());
    let resolved = url::Url::parse(url_spec)
        .or_else(|_| url::Url::parse(&base_str).and_then(|base| base.join(url_spec)))
        .map_err(|_| BidiError::invalid_argument(format!("invalid url: {url_spec}")))?;
    let nav_id = random_id().unwrap_or_else(|_| "nav-1".to_owned());
    let events = tab.subscribe().ok();

    if frame == FrameId::MAIN {
        tab.goto(resolved.as_str()).await.map_err(|err| match err {
            TabError::InvalidUrl { spec } => BidiError::unknown_error(spec),
            other => BidiError::unknown_error(other.to_string()),
        })?;
    } else {
        let script = format!("window.location.href = {}", json!(resolved.as_str()));
        tab.execute_script(browser::ExecuteScriptOptions {
            frame,
            source: browser::ScriptSource::Browser(&script),
            timeout: Some(Duration::from_secs(30)),
        })
        .await
        .map_err(|err| BidiError::unknown_error(err.to_string()))?;
    }

    wait_readiness(&tab, frame, wait, events).await?;
    let final_url = if wait == "none" {
        resolved.to_string()
    } else {
        tab.document_url_in(frame)
            .await
            .unwrap_or_else(|_| resolved.to_string())
    };
    Ok(json!({
        "navigation": nav_id,
        "url": final_url,
    }))
}

async fn wait_readiness(
    tab: &TabHandle,
    frame: FrameId,
    wait: &str,
    mut events: Option<tokio::sync::mpsc::Receiver<TabEvent>>,
) -> Result<(), BidiError> {
    match wait {
        "interactive" => {
            if let Some(rx) = events.as_mut() {
                let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
                loop {
                    match tokio::time::timeout_at(deadline, rx.recv()).await {
                        Ok(Some(TabEvent::DomContentLoaded | TabEvent::Load))
                            if frame == FrameId::MAIN =>
                        {
                            return Ok(());
                        }
                        Ok(Some(
                            TabEvent::FrameDomContentLoaded { frame: f, .. }
                            | TabEvent::FrameLoad { frame: f, .. },
                        )) if f == frame => return Ok(()),
                        Ok(Some(TabEvent::NavigationFailed(_))) if frame == FrameId::MAIN => {
                            return Err(BidiError::unknown_error("navigation failed"));
                        }
                        Ok(Some(_)) => {}
                        Ok(None) | Err(_) => return Ok(()),
                    }
                }
            }
            Ok(())
        }
        "complete" => {
            if frame == FrameId::MAIN {
                let loaded = tab
                    .run_until_load_timeout(Duration::from_secs(30))
                    .await
                    .map_err(|err| BidiError::unknown_error(err.to_string()))?;
                if !loaded {
                    return Err(BidiError::unknown_error("navigation timed out"));
                }
                if tab.last_navigation_failed().await.unwrap_or(false) {
                    return Err(BidiError::unknown_error("navigation failed"));
                }
            } else if let Some(rx) = events.as_mut() {
                let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
                while let Ok(Some(ev)) = tokio::time::timeout_at(deadline, rx.recv()).await {
                    if matches!(ev, TabEvent::FrameLoad { frame: f, .. } if f == frame) {
                        break;
                    }
                }
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

// https://w3c.github.io/webdriver-bidi/#command-browsingContext-reload
pub(crate) async fn reload(state: &Arc<BidiState>, params: &Value) -> BidiResult {
    let map = require_object(params, "params")?;
    let ctx = require_str(map, "context")?;
    let _ignore_cache = optional_bool(map, "ignoreCache")?;
    let wait = optional_str(map, "wait")?.unwrap_or("none");
    if !matches!(wait, "none" | "interactive" | "complete") {
        return Err(BidiError::invalid_argument(format!(
            "invalid wait readiness: {wait}"
        )));
    }
    let (tab, frame) = resolve_context(state, ctx).await?;
    let nav_id = random_id().unwrap_or_else(|_| "nav-1".to_owned());
    let events = tab.subscribe().ok();
    if frame == FrameId::MAIN {
        tab.reload()
            .await
            .map_err(|err| BidiError::unknown_error(err.to_string()))?;
    } else {
        tab.execute_script(browser::ExecuteScriptOptions {
            frame,
            source: browser::ScriptSource::Browser("window.location.reload()"),
            timeout: Some(Duration::from_secs(30)),
        })
        .await
        .map_err(|err| BidiError::unknown_error(err.to_string()))?;
    }
    wait_readiness(&tab, frame, wait, events).await?;
    let final_url = tab.document_url_in(frame).await.unwrap_or_default();
    Ok(json!({
        "navigation": nav_id,
        "url": final_url,
    }))
}

// https://w3c.github.io/webdriver-bidi/#command-browsingContext-traverseHistory
pub(crate) async fn traverse_history(state: &Arc<BidiState>, params: &Value) -> BidiResult {
    let map = require_object(params, "params")?;
    let ctx = require_str(map, "context")?;
    let Some(delta) = map.get("delta").and_then(Value::as_i64) else {
        return Err(BidiError::invalid_argument("delta must be an integer"));
    };
    let delta =
        i32::try_from(delta).map_err(|_| BidiError::invalid_argument("delta out of i32 range"))?;
    let (tab, frame) = resolve_context(state, ctx).await?;
    if frame != FrameId::MAIN {
        return Err(BidiError::invalid_argument(
            "traverseHistory requires a top-level context",
        ));
    }
    let history = tab
        .history()
        .await
        .map_err(|err| BidiError::unknown_error(err.to_string()))?;
    let current = i64::try_from(history.current_index()).unwrap_or(0);
    let target = current + i64::from(delta);
    let len = i64::try_from(history.entries().len()).unwrap_or(0);
    if target < 0 || target >= len {
        return Err(BidiError::new(
            "no such history entry",
            "history delta out of bounds",
        ));
    }
    if delta != 0 {
        tab.traverse_history(delta)
            .await
            .map_err(|err| BidiError::unknown_error(err.to_string()))?;
    }
    Ok(json!({}))
}

// https://w3c.github.io/webdriver-bidi/#command-browsingContext-activate
pub(crate) async fn activate(state: &Arc<BidiState>, params: &Value) -> BidiResult {
    let map = require_object(params, "params")?;
    let ctx = require_str(map, "context")?;
    let (_tab, frame) = resolve_context(state, ctx).await?;
    if frame != FrameId::MAIN {
        return Err(BidiError::invalid_argument(
            "activate requires a top-level browsing context",
        ));
    }
    Ok(json!({}))
}

// https://w3c.github.io/webdriver-bidi/#command-browsingContext-setViewport
pub(crate) async fn set_viewport(state: &Arc<BidiState>, params: &Value) -> BidiResult {
    let map = require_object(params, "params")?;
    let ctx = optional_str(map, "context")?;
    let user_contexts = map.get("userContexts");
    if ctx.is_some() && user_contexts.is_some() {
        return Err(BidiError::invalid_argument(
            "cannot specify both context and userContexts",
        ));
    }
    if let Some(dpr) = map.get("devicePixelRatio")
        && !dpr.is_null()
        && dpr.as_f64().is_none_or(|v| !v.is_finite() || v <= 0.0)
    {
        return Err(BidiError::invalid_argument(
            "devicePixelRatio must be a positive number or null",
        ));
    }
    let viewport = match map.get("viewport") {
        None => None,
        Some(Value::Null) => Some(browser::DEFAULT_VIEWPORT),
        Some(Value::Object(vp)) => {
            let Some(w) = vp.get("width").and_then(Value::as_u64) else {
                return Err(BidiError::invalid_argument(
                    "viewport.width must be a non-negative integer",
                ));
            };
            let Some(h) = vp.get("height").and_then(Value::as_u64) else {
                return Err(BidiError::invalid_argument(
                    "viewport.height must be a non-negative integer",
                ));
            };
            let w = u32::try_from(w)
                .ok()
                .filter(|v| *v > 0)
                .ok_or_else(|| BidiError::invalid_argument("viewport.width out of range"))?;
            let h = u32::try_from(h)
                .ok()
                .filter(|v| *v > 0)
                .ok_or_else(|| BidiError::invalid_argument("viewport.height out of range"))?;
            Some((w, h))
        }
        Some(_) => {
            return Err(BidiError::invalid_argument(
                "viewport must be an object or null",
            ));
        }
    };
    if let Some(ctx) = ctx {
        let (tab, frame) = resolve_context(state, ctx).await?;
        if frame != FrameId::MAIN {
            return Err(BidiError::invalid_argument(
                "setViewport requires a top-level context",
            ));
        }
        if let Some((w, h)) = viewport {
            tab.set_viewport(w, h)
                .await
                .map_err(|err| BidiError::unknown_error(err.to_string()))?;
        }
    } else if let Some(ucs) = user_contexts {
        let Some(arr) = ucs.as_array() else {
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
            if let Some((w, h)) = viewport
                && let Ok(tabs) = state.browser.tabs_in(&uc).await
            {
                for id in tabs {
                    if let Ok(tab) = state.browser.tab(id).await {
                        let _ = tab.set_viewport(w, h).await;
                    }
                }
            }
        }
    } else if let Some((w, h)) = viewport
        && let Ok(tabs) = state.browser.tabs().await
    {
        for id in tabs {
            if let Ok(tab) = state.browser.tab(id).await {
                let _ = tab.set_viewport(w, h).await;
            }
        }
    }
    Ok(json!({}))
}

fn parse_screenshot_format(map: &Map<String, Value>) -> Result<ScreenshotFormat, BidiError> {
    match map.get("format") {
        None => Ok(ScreenshotFormat::Png),
        Some(Value::Object(fmt)) => {
            let mime = require_str(fmt, "type")?;
            match mime {
                "image/png" => Ok(ScreenshotFormat::Png),
                "image/jpeg" => {
                    let quality = match fmt.get("quality") {
                        None => 80,
                        Some(v) => {
                            let Some(q) = v.as_f64() else {
                                return Err(BidiError::invalid_argument(
                                    "quality must be a number",
                                ));
                            };
                            if !(0.0..=1.0).contains(&q) {
                                return Err(BidiError::invalid_argument(
                                    "quality must be in [0, 1]",
                                ));
                            }
                            #[expect(
                                clippy::cast_possible_truncation,
                                clippy::cast_sign_loss,
                                reason = "quality is validated within [0.0, 1.0]"
                            )]
                            {
                                (q * 100.0).round() as u8
                            }
                        }
                    };
                    Ok(ScreenshotFormat::Jpeg { quality })
                }
                other => Err(BidiError::invalid_argument(format!(
                    "unsupported image format: {other}"
                ))),
            }
        }
        Some(_) => Err(BidiError::invalid_argument("format must be an object")),
    }
}

async fn resolve_screenshot_clip(
    map: &Map<String, Value>,
    tab: &TabHandle,
    frame: FrameId,
) -> Result<Option<ScreenshotClip>, BidiError> {
    let Some(clip_val) = map.get("clip") else {
        return Ok(None);
    };
    let clip_obj = require_object(clip_val, "clip")?;
    let clip_type = require_str(clip_obj, "type")?;
    match clip_type {
        "box" => {
            let x = clip_obj
                .get("x")
                .and_then(Value::as_f64)
                .ok_or_else(|| BidiError::invalid_argument("clip.x must be a number"))?;
            let y = clip_obj
                .get("y")
                .and_then(Value::as_f64)
                .ok_or_else(|| BidiError::invalid_argument("clip.y must be a number"))?;
            let width = clip_obj
                .get("width")
                .and_then(Value::as_f64)
                .ok_or_else(|| BidiError::invalid_argument("clip.width must be a number"))?;
            let height = clip_obj
                .get("height")
                .and_then(Value::as_f64)
                .ok_or_else(|| BidiError::invalid_argument("clip.height must be a number"))?;
            if width <= 0.0 || height <= 0.0 {
                return Err(BidiError::unable_to_capture_screen(
                    "clip rectangle has non-positive size",
                ));
            }
            #[expect(
                clippy::cast_possible_truncation,
                reason = "CSS pixel clip coordinates fit in f32"
            )]
            Ok(Some(ScreenshotClip {
                x: x.max(0.0) as f32,
                y: y.max(0.0) as f32,
                width: width as f32,
                height: height as f32,
            }))
        }
        "element" => resolve_element_clip(clip_obj, tab, frame).await.map(Some),
        other => Err(BidiError::invalid_argument(format!(
            "invalid clip type: {other}"
        ))),
    }
}

async fn resolve_element_clip(
    clip_obj: &Map<String, Value>,
    tab: &TabHandle,
    frame: FrameId,
) -> Result<ScreenshotClip, BidiError> {
    let el = clip_obj
        .get("element")
        .ok_or_else(|| BidiError::invalid_argument("clip.element is required"))?;
    let shared_id = el
        .get("sharedId")
        .and_then(Value::as_str)
        .ok_or_else(|| BidiError::invalid_argument("sharedId is required"))?;
    let script = format!(
        "(() => {{ {BIDI_RUNTIME_JS}\n\
         const el = __tbBidi.st.sharedById[{}];\n\
         if (!el || typeof el.getBoundingClientRect !== 'function') return null;\n\
         const r = el.getBoundingClientRect();\n\
         return JSON.stringify({{x:r.x,y:r.y,width:r.width,height:r.height}});\n\
         }})()",
        json!(shared_id)
    );
    let rect = match tab
        .execute_script(browser::ExecuteScriptOptions {
            frame,
            source: browser::ScriptSource::Browser(&script),
            timeout: Some(Duration::from_secs(10)),
        })
        .await
    {
        Ok(RemoteValue::String(text)) => serde_json::from_str::<Value>(&text)
            .map_err(|e| BidiError::unknown_error(e.to_string()))?,
        _ => return Err(BidiError::no_such_node("element not found")),
    };
    let w = rect.get("width").and_then(Value::as_f64).unwrap_or(0.0);
    let h = rect.get("height").and_then(Value::as_f64).unwrap_or(0.0);
    if w <= 0.0 || h <= 0.0 {
        return Err(BidiError::unable_to_capture_screen(
            "element has zero dimensions",
        ));
    }
    #[expect(
        clippy::cast_possible_truncation,
        reason = "CSS pixel clip coordinates fit in f32"
    )]
    Ok(ScreenshotClip {
        x: rect
            .get("x")
            .and_then(Value::as_f64)
            .unwrap_or(0.0)
            .max(0.0) as f32,
        y: rect
            .get("y")
            .and_then(Value::as_f64)
            .unwrap_or(0.0)
            .max(0.0) as f32,
        width: w as f32,
        height: h as f32,
    })
}

// https://w3c.github.io/webdriver-bidi/#command-browsingContext-captureScreenshot
pub(crate) async fn capture_screenshot(state: &Arc<BidiState>, params: &Value) -> BidiResult {
    let map = require_object(params, "params")?;
    let ctx = require_str(map, "context")?;
    let (tab, frame) = resolve_context(state, ctx).await?;
    let origin = optional_str(map, "origin")?.unwrap_or("viewport");
    if !matches!(origin, "viewport" | "document") {
        return Err(BidiError::invalid_argument(format!(
            "invalid origin: {origin}"
        )));
    }
    let format = parse_screenshot_format(map)?;
    let clip = resolve_screenshot_clip(map, &tab, frame).await?;
    if let Some(img_size) = map.get("imageSize") {
        let size_obj = require_object(img_size, "imageSize")?;
        for field in ["maxWidth", "maxHeight"] {
            if let Some(v) = size_obj.get(field) {
                let Some(n) = v.as_u64() else {
                    return Err(BidiError::invalid_argument(format!(
                        "imageSize.{field} must be a positive integer"
                    )));
                };
                if n == 0 || n > crate::session::MAX_SAFE_INT {
                    return Err(BidiError::invalid_argument(format!(
                        "imageSize.{field} out of range"
                    )));
                }
            }
        }
    }
    let (vw, vh) = tab
        .viewport_size()
        .await
        .unwrap_or(browser::DEFAULT_VIEWPORT);
    let (vw, vh) = if origin == "document" {
        if let Ok((cw, ch)) = tab.content_size().await {
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "content size is non-negative CSS pixels clamped to >= viewport"
            )]
            (
                (cw.ceil().max(0.0) as u32).max(vw),
                (ch.ceil().max(0.0) as u32).max(vh),
            )
        } else {
            (vw, vh)
        }
    } else {
        (vw, vh)
    };
    let bytes = tab
        .screenshot_frame(
            frame,
            ScreenshotRequest {
                viewport_width: vw,
                viewport_height: vh,
                clip,
                format,
                omit_background: false,
            },
        )
        .await
        .map_err(|err| BidiError::unable_to_capture_screen(err.to_string()))?;
    Ok(json!({ "data": BASE64.encode(&bytes) }))
}

fn build_locate_nodes_script(
    ctx: &str,
    locator: &Value,
    start_nodes: &Value,
    max_node_count: Option<u64>,
    ser_opts: &Value,
) -> String {
    format!(
        "(() => {{ {BIDI_RUNTIME_JS}\n\
         const contextId = {};\n\
         const locator = {};\n\
         const startNodes = {};\n\
         const maxCount = {};\n\
         const serOpts = {};\n\
         let roots = [document];\n\
         if (Array.isArray(startNodes)) {{\n\
           roots = [];\n\
           for (const s of startNodes) {{\n\
             if (!s || typeof s.sharedId !== 'string') return JSON.stringify({{error:'invalid argument',message:'startNode requires sharedId'}});\n\
             const n = __tbBidi.st.sharedById[s.sharedId];\n\
             if (!n) return JSON.stringify({{error:'no such node',message:'unknown start node'}});\n\
             roots.push(n);\n\
           }}\n\
         }}\n\
         const found = [];\n\
         const seen = new Set();\n\
         const pushNode = n => {{ if (n && !seen.has(n)) {{ seen.add(n); found.push(n); }} }};\n\
         for (const root of roots) {{\n\
           if (locator.type === 'css') {{\n\
             if (typeof locator.value !== 'string') return JSON.stringify({{error:'invalid argument',message:'css value must be string'}});\n\
             try {{ for (const el of root.querySelectorAll(locator.value)) pushNode(el); }}\n\
             catch (e) {{ return JSON.stringify({{error:'invalid selector',message:String(e)}}); }}\n\
           }} else if (locator.type === 'xpath') {{\n\
             if (typeof locator.value !== 'string') return JSON.stringify({{error:'invalid argument',message:'xpath value must be string'}});\n\
             try {{\n\
               const snap = document.evaluate(locator.value, root, null, 7, null);\n\
               for (let i = 0; i < snap.snapshotLength; i++) pushNode(snap.snapshotItem(i));\n\
             }} catch (e) {{ return JSON.stringify({{error:'invalid selector',message:String(e)}}); }}\n\
           }} else if (locator.type === 'innerText') {{\n\
             if (typeof locator.value !== 'string' || locator.value === '') return JSON.stringify({{error:'invalid argument',message:'innerText value must be non-empty string'}});\n\
             const ignoreCase = Boolean(locator.ignoreCase);\n\
             const matchType = locator.matchType || 'full';\n\
             const target = ignoreCase ? locator.value.toLowerCase() : locator.value;\n\
             const all = root.querySelectorAll ? root.querySelectorAll('*') : [];\n\
             for (const el of all) {{\n\
               const raw = (el.innerText !== undefined ? el.innerText : (el.textContent || '')).trim();\n\
               const txt = ignoreCase ? raw.toLowerCase() : raw;\n\
               const matched = matchType === 'partial' ? txt.includes(target) : txt === target;\n\
               if (matched) pushNode(el);\n\
             }}\n\
           }} else if (locator.type === 'accessibility') {{\n\
             const val = locator.value || {{}};\n\
             const all = root.querySelectorAll ? root.querySelectorAll('*') : [];\n\
             for (const el of all) {{\n\
               const role = el.getAttribute('role') || '';\n\
               const name = el.getAttribute('aria-label') || (el.textContent || '').trim();\n\
               if (val.role && role !== val.role) continue;\n\
               if (val.name && name !== val.name) continue;\n\
               pushNode(el);\n\
             }}\n\
           }}\n\
         }}\n\
         const limited = maxCount !== null ? found.slice(0, maxCount) : found;\n\
         const maxDomDepth = serOpts.maxDomDepth === undefined ? 0 : serOpts.maxDomDepth;\n\
         const maxObjDepth = serOpts.maxObjectDepth === undefined ? null : serOpts.maxObjectDepth;\n\
         const includeShadow = serOpts.includeShadowTree || 'none';\n\
         const nodes = limited.map(n => __tbBidi.serialize(n, maxObjDepth, maxDomDepth, contextId, 'none', undefined, undefined, includeShadow));\n\
         return JSON.stringify({{nodes}});\n\
         }})()",
        json!(ctx),
        locator,
        start_nodes,
        json!(max_node_count),
        ser_opts
    )
}

// https://w3c.github.io/webdriver-bidi/#command-browsingContext-locateNodes
pub(crate) async fn locate_nodes(state: &Arc<BidiState>, params: &Value) -> BidiResult {
    let map = require_object(params, "params")?;
    let ctx = require_str(map, "context")?;
    let locator = map
        .get("locator")
        .ok_or_else(|| BidiError::invalid_argument("locator is required"))?;
    let locator_obj = require_object(locator, "locator")?;
    let loc_type = require_str(locator_obj, "type")?;
    if !matches!(
        loc_type,
        "css" | "xpath" | "innerText" | "context" | "accessibility"
    ) {
        return Err(BidiError::invalid_argument(format!(
            "unsupported locator type: {loc_type}"
        )));
    }
    let max_node_count = match map.get("maxNodeCount") {
        None => None,
        Some(v) => {
            let Some(n) = v.as_u64() else {
                return Err(BidiError::invalid_argument(
                    "maxNodeCount must be a positive integer",
                ));
            };
            if n == 0 {
                return Err(BidiError::invalid_argument(
                    "maxNodeCount must be at least 1",
                ));
            }
            Some(n)
        }
    };
    let start_nodes = map.get("startNodes").cloned().unwrap_or(Value::Null);
    let ser_opts = map
        .get("serializationOptions")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let (tab, frame) = resolve_context(state, ctx).await?;
    let script = build_locate_nodes_script(ctx, locator, &start_nodes, max_node_count, &ser_opts);
    match tab
        .execute_script(browser::ExecuteScriptOptions {
            frame,
            source: browser::ScriptSource::Browser(&script),
            timeout: Some(Duration::from_secs(30)),
        })
        .await
    {
        Ok(RemoteValue::String(raw)) => {
            let parsed: Value =
                serde_json::from_str(&raw).map_err(|e| BidiError::unknown_error(e.to_string()))?;
            if let Some(code) = parsed.get("error").and_then(Value::as_str) {
                let msg = parsed
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("locator failed");
                let static_code = match code {
                    "invalid selector" => "invalid selector",
                    "no such node" => "no such node",
                    _ => "invalid argument",
                };
                return Err(BidiError::new(static_code, msg));
            }
            Ok(parsed)
        }
        Ok(_) => Ok(json!({ "nodes": [] })),
        Err(err) => Err(BidiError::unknown_error(err.to_string())),
    }
}

// https://w3c.github.io/webdriver-bidi/#command-browsingContext-handleUserPrompt
pub(crate) async fn handle_user_prompt(state: &Arc<BidiState>, params: &Value) -> BidiResult {
    let map = require_object(params, "params")?;
    let ctx = require_str(map, "context")?;
    let accept = optional_bool(map, "accept")?.unwrap_or(true);
    let user_text = optional_str(map, "userText")?.map(str::to_owned);
    let (tab, _frame) = resolve_context(state, ctx).await?;
    if !state
        .browser
        .prompt_broker()
        .handle_prompt(tab.id(), accept, user_text)
    {
        return Err(BidiError::no_such_alert("no user prompt is open"));
    }
    Ok(json!({}))
}

// https://w3c.github.io/webdriver-bidi/#command-browsingContext-setBypassCSP
pub(crate) async fn set_bypass_csp(state: &Arc<BidiState>, params: &Value) -> BidiResult {
    let map = require_object(params, "params")?;
    match map.get("bypass") {
        Some(Value::Bool(_) | Value::Null) => {}
        _ => {
            return Err(BidiError::invalid_argument(
                "bypass must be a boolean or null",
            ));
        }
    }
    if let Some(ctxs) = map.get("contexts").and_then(Value::as_array) {
        for item in ctxs {
            let Some(raw) = item.as_str() else {
                return Err(BidiError::invalid_argument("context must be a string"));
            };
            let _ = resolve_context(state, raw).await?;
        }
    }
    Ok(json!({}))
}
