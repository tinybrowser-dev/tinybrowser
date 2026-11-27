// https://w3c.github.io/webdriver/

use std::collections::HashSet;
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use browser::{
    CookieRecord, CookieSameSite, FrameId, PromptBehavior, PromptEvent, PromptKind, RemoteValue,
    ScreenshotClip, ScreenshotFormat, ScreenshotRequest, ScriptFailure, TabError, TabEvent,
    TabHandle, TabId, UserContextId,
};
use serde_json::{Map, Value, json};
use tokio::sync::Mutex;
use unicode_segmentation::UnicodeSegmentation;

use crate::session::{
    CapabilityError, PageLoadStrategy, Sessions, Window, WindowSnapshot, duration_millis,
    parse_timeouts_object, process_capabilities, tab_handle_id,
};

pub(crate) const ELEMENT_KEY: &str = "element-6066-11e4-a52e-4f735466cecf";
pub(crate) const SHADOW_KEY: &str = "shadow-6066-11e4-a52e-4f735466cecf";
const WINDOW_KEY: &str = "window-fcc6-11e5-b4f8-330a88ab9d7f";
const FRAME_KEY: &str = "frame-075b-4da1-b6ba-e579c2d3230a";

pub(crate) async fn dispatch(
    method: &str,
    path: &str,
    body: &str,
    sessions: &Mutex<Sessions>,
) -> (u16, Value) {
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if let Some(reply) = dispatch_session_and_nav(method, segments.as_slice(), body, sessions).await
    {
        return reply;
    }
    if let Some(reply) = dispatch_elements(method, segments.as_slice(), body, sessions).await {
        return reply;
    }
    if let Some(reply) = dispatch_misc(method, segments.as_slice(), body, sessions).await {
        return reply;
    }
    error(404, "unknown command", path)
}

async fn dispatch_session_and_nav(
    method: &str,
    segments: &[&str],
    body: &str,
    sessions: &Mutex<Sessions>,
) -> Option<(u16, Value)> {
    let reply = match (method, segments) {
        // https://w3c.github.io/webdriver/#status
        ("GET", ["status"]) => status(sessions).await,
        // https://w3c.github.io/webdriver/#new-session
        ("POST", ["session"]) => create_session(sessions, body).await,
        // https://w3c.github.io/webdriver/#delete-session
        ("DELETE", ["session", session]) => delete_session(sessions, session).await,
        // https://w3c.github.io/webdriver/#timeouts
        ("GET", ["session", session, "timeouts"]) => {
            with_session_sync(sessions, session, |guard| get_timeouts(guard, session)).await
        }
        ("POST", ["session", session, "timeouts"]) => {
            with_session_sync(sessions, session, |guard| {
                set_timeouts(guard, session, body)
            })
            .await
        }
        // https://w3c.github.io/webdriver/#navigation
        ("POST", ["session", session, "url"]) => navigate(sessions, session, body).await,
        ("GET", ["session", session, "url"]) => current_url(sessions, session).await,
        ("POST", ["session", session, "back"]) => traverse_history(sessions, session, -1).await,
        ("POST", ["session", session, "forward"]) => traverse_history(sessions, session, 1).await,
        ("POST", ["session", session, "refresh"]) => refresh(sessions, session).await,
        ("GET", ["session", session, "title"]) => get_title(sessions, session).await,
        ("GET", ["session", session, "source"]) => get_page_source(sessions, session).await,
        // https://w3c.github.io/webdriver/#contexts
        ("GET", ["session", session, "window"]) => {
            with_session_sync(sessions, session, |guard| current_window(guard, session)).await
        }
        ("POST", ["session", session, "window"]) => {
            with_session_sync(sessions, session, |guard| {
                switch_window(guard, session, body)
            })
            .await
        }
        ("DELETE", ["session", session, "window"]) => close_window(sessions, session).await,
        ("GET", ["session", session, "window", "handles"]) => {
            with_session_sync(sessions, session, |guard| window_handles(guard, session)).await
        }
        ("POST", ["session", session, "window", "new"]) => {
            new_window(sessions, session, body).await
        }
        ("GET", ["session", session, "window", "rect"]) => window_rect(sessions, session).await,
        ("POST", ["session", session, "window", "rect"]) => {
            set_window_rect(sessions, session, body).await
        }
        ("POST", ["session", session, "window", op])
            if matches!(*op, "minimize" | "maximize" | "fullscreen") =>
        {
            window_op(sessions, session, op).await
        }
        // https://w3c.github.io/webdriver/#switch-to-frame
        ("POST", ["session", session, "frame"]) => switch_to_frame(sessions, session, body).await,
        ("POST", ["session", session, "frame", "parent"]) => {
            switch_to_parent_frame(sessions, session).await
        }
        (
            _,
            ["status" | "session"]
            | ["session", _]
            | [
                "session",
                _,
                "timeouts" | "url" | "back" | "forward" | "refresh" | "title" | "source" | "window"
                | "frame",
            ]
            | [
                "session",
                _,
                "window",
                "handles" | "new" | "rect" | "minimize" | "maximize" | "fullscreen",
            ]
            | ["session", _, "frame", "parent"],
        ) => method_not_allowed(),
        _ => return None,
    };
    Some(reply)
}

async fn dispatch_elements(
    method: &str,
    segments: &[&str],
    body: &str,
    sessions: &Mutex<Sessions>,
) -> Option<(u16, Value)> {
    let reply = match (method, segments) {
        // https://w3c.github.io/webdriver/#elements
        ("GET", ["session", session, "element", "active"]) => {
            get_active_element(sessions, session).await
        }
        ("POST", ["session", session, "element"]) => {
            find_element(sessions, session, None, false, body).await
        }
        ("POST", ["session", session, "elements"]) => {
            find_elements(sessions, session, None, false, body).await
        }
        ("POST", ["session", session, "element", element, "element"]) => {
            find_element(sessions, session, Some(element), false, body).await
        }
        ("POST", ["session", session, "element", element, "elements"]) => {
            find_elements(sessions, session, Some(element), false, body).await
        }
        ("GET", ["session", session, "element", element, "shadow"]) => {
            get_element_shadow_root(sessions, session, element).await
        }
        ("POST", ["session", session, "shadow", shadow, "element"]) => {
            find_element(sessions, session, Some(shadow), true, body).await
        }
        ("POST", ["session", session, "shadow", shadow, "elements"]) => {
            find_elements(sessions, session, Some(shadow), true, body).await
        }
        ("GET", ["session", session, "element", element, "selected"]) => {
            element_selected(sessions, session, element).await
        }
        ("GET", ["session", session, "element", element, "attribute", name]) => {
            element_attribute(sessions, session, element, name).await
        }
        ("GET", ["session", session, "element", element, "property", name]) => {
            element_property(sessions, session, element, name).await
        }
        ("GET", ["session", session, "element", element, "css", property]) => {
            element_css_value(sessions, session, element, property).await
        }
        ("GET", ["session", session, "element", element, "text"]) => {
            element_text(sessions, session, element).await
        }
        ("GET", ["session", session, "element", element, "name"]) => {
            element_tag_name(sessions, session, element).await
        }
        ("GET", ["session", session, "element", element, "rect"]) => {
            element_rect(sessions, session, element).await
        }
        ("GET", ["session", session, "element", element, "enabled"]) => {
            element_enabled(sessions, session, element).await
        }
        ("GET", ["session", session, "element", element, "computedrole"]) => {
            element_computed_role(sessions, session, element).await
        }
        ("GET", ["session", session, "element", element, "computedlabel"]) => {
            element_computed_label(sessions, session, element).await
        }
        ("POST", ["session", session, "element", element, "click"]) => {
            element_click(sessions, session, element).await
        }
        ("POST", ["session", session, "element", element, "clear"]) => {
            element_clear(sessions, session, element).await
        }
        ("POST", ["session", session, "element", element, "value"]) => {
            element_send_keys(sessions, session, element, body).await
        }
        ("GET", ["session", session, "element", element, "screenshot"]) => {
            take_element_screenshot(sessions, session, element).await
        }
        (
            _,
            ["session", _, "element" | "elements"]
            | ["session", _, "element", "active"]
            | [
                "session",
                _,
                "element",
                _,
                "element" | "elements" | "shadow" | "selected" | "text" | "name" | "rect"
                | "enabled" | "computedrole" | "computedlabel" | "click" | "clear" | "value"
                | "screenshot",
            ]
            | [
                "session",
                _,
                "element",
                _,
                "attribute" | "property" | "css",
                _,
            ]
            | ["session", _, "shadow", _, "element" | "elements"],
        ) => method_not_allowed(),
        _ => return None,
    };
    Some(reply)
}

async fn dispatch_misc(
    method: &str,
    segments: &[&str],
    body: &str,
    sessions: &Mutex<Sessions>,
) -> Option<(u16, Value)> {
    let reply = match (method, segments) {
        // https://w3c.github.io/webdriver/#executing-script
        ("POST", ["session", session, "execute", "sync"]) => {
            execute(sessions, session, body, false).await
        }
        ("POST", ["session", session, "execute", "async"]) => {
            execute(sessions, session, body, true).await
        }
        // https://w3c.github.io/webdriver/#cookies
        ("GET", ["session", session, "cookie"]) => get_cookies(sessions, session).await,
        ("GET", ["session", session, "cookie", name]) => {
            get_named_cookie(sessions, session, name).await
        }
        ("POST", ["session", session, "cookie"]) => add_cookie(sessions, session, body).await,
        ("DELETE", ["session", session, "cookie", name]) => {
            delete_named_cookie(sessions, session, name).await
        }
        ("DELETE", ["session", session, "cookie"]) => delete_cookies(sessions, session).await,
        // https://w3c.github.io/webdriver/#actions
        ("POST", ["session", session, "actions"]) => perform_actions(sessions, session, body).await,
        ("DELETE", ["session", session, "actions"]) => release_actions(sessions, session).await,
        // https://w3c.github.io/webdriver/#user-prompts
        ("POST", ["session", session, "alert", "dismiss"]) => {
            dismiss_alert(sessions, session).await
        }
        ("POST", ["session", session, "alert", "accept"]) => accept_alert(sessions, session).await,
        ("GET", ["session", session, "alert", "text"]) => get_alert_text(sessions, session).await,
        ("POST", ["session", session, "alert", "text"]) => {
            send_alert_text(sessions, session, body).await
        }
        // https://w3c.github.io/webdriver/#screen-capture
        ("GET", ["session", session, "screenshot"]) => take_screenshot(sessions, session).await,
        // https://w3c.github.io/permissions/#webdriver-command-set-permission
        ("POST", ["session", session, "permissions"]) => {
            with_session_sync(sessions, session, |guard| {
                set_permission(guard, session, body)
            })
            .await
        }
        (
            _,
            ["session", _, "execute", "sync" | "async"]
            | [
                "session",
                _,
                "cookie" | "actions" | "screenshot" | "permissions",
            ]
            | ["session", _, "cookie", _]
            | ["session", _, "alert", "dismiss" | "accept" | "text"],
        ) => method_not_allowed(),
        _ => return None,
    };
    Some(reply)
}

async fn status(sessions: &Mutex<Sessions>) -> (u16, Value) {
    let (ready, message) = crate::session::status(sessions).await;
    ok(json!({"ready": ready, "message": message}))
}

async fn create_session(sessions: &Mutex<Sessions>, body: &str) -> (u16, Value) {
    let parsed: Value = if body.trim().is_empty() {
        json!({})
    } else {
        match serde_json::from_str(body) {
            Ok(value) => value,
            Err(err) => return error(400, "invalid argument", &err.to_string()),
        }
    };
    let matched = match process_capabilities(&parsed) {
        Ok(caps) => caps,
        Err(CapabilityError::InvalidArgument(msg)) => return error(400, "invalid argument", &msg),
        Err(CapabilityError::SessionNotCreated(msg)) => {
            return error(500, "session not created", &msg);
        }
    };
    match crate::session::create_session(sessions, matched).await {
        Ok((id, capabilities)) => ok(json!({"sessionId": id, "capabilities": capabilities})),
        Err(err) => error(500, "session not created", &err),
    }
}

async fn snapshot_session(
    sessions: &Mutex<Sessions>,
    session: &str,
) -> Result<WindowSnapshot, (u16, Value)> {
    let snapshot = {
        let mut guard = sessions.lock().await;
        if !guard.open.contains_key(session) {
            return Err(error(404, "invalid session id", session));
        }
        guard.sync_windows(session).await;
        guard
            .snapshot(session)
            .ok_or_else(|| error(404, "no such window", "current window is closed"))?
    };
    if snapshot.frame != FrameId::MAIN {
        let frames = snapshot
            .tab
            .frames()
            .await
            .map_err(|_| error(404, "no such window", "browsing context has been discarded"))?;
        if !frames.iter().any(|f| f.id == snapshot.frame) {
            return Err(error(
                404,
                "no such window",
                "browsing context has been discarded",
            ));
        }
    }
    Ok(snapshot)
}

async fn snapshot_top_level_checking_alert(
    sessions: &Mutex<Sessions>,
    session: &str,
) -> Result<WindowSnapshot, (u16, Value)> {
    let snapshot = {
        let mut guard = sessions.lock().await;
        if !guard.open.contains_key(session) {
            return Err(error(404, "invalid session id", session));
        }
        guard.sync_windows(session).await;
        guard
            .snapshot(session)
            .ok_or_else(|| error(404, "no such window", "current window is closed"))?
    };
    check_unhandled_prompt(&snapshot)?;
    Ok(snapshot)
}

async fn snapshot_checking_alert(
    sessions: &Mutex<Sessions>,
    session: &str,
) -> Result<WindowSnapshot, (u16, Value)> {
    let snapshot = snapshot_session(sessions, session).await?;
    check_unhandled_prompt(&snapshot)?;
    Ok(snapshot)
}

// https://w3c.github.io/webdriver/#dfn-handle-any-user-prompts
fn check_unhandled_prompt(snapshot: &WindowSnapshot) -> Result<(), (u16, Value)> {
    let broker = snapshot.browser.prompt_broker();
    let Some(info) = broker.active_prompt(snapshot.tab.id()) else {
        return Ok(());
    };
    let behavior = snapshot
        .prompt_behavior
        .resolve(info.kind)
        .unwrap_or(PromptBehavior::DismissAndNotify);
    match behavior {
        PromptBehavior::Accept => {
            let _ = broker.handle_prompt(snapshot.tab.id(), true, info.default_value);
            Ok(())
        }
        PromptBehavior::Dismiss => {
            let _ = broker.handle_prompt(snapshot.tab.id(), false, None);
            Ok(())
        }
        PromptBehavior::AcceptAndNotify => {
            let _ = broker.handle_prompt(snapshot.tab.id(), true, info.default_value);
            Err(unexpected_alert(&info.message))
        }
        PromptBehavior::DismissAndNotify => {
            let _ = broker.handle_prompt(snapshot.tab.id(), false, None);
            Err(unexpected_alert(&info.message))
        }
        PromptBehavior::Ignore => Err(unexpected_alert(&info.message)),
    }
}

fn unexpected_alert(text: &str) -> (u16, Value) {
    (
        500,
        json!({
            "value": {
                "error": "unexpected alert open",
                "message": format!("unexpected alert open: {text}"),
                "stacktrace": "",
                "data": { "text": text },
            }
        }),
    )
}

async fn with_session_sync(
    sessions: &Mutex<Sessions>,
    session: &str,
    action: impl FnOnce(&mut Sessions) -> (u16, Value),
) -> (u16, Value) {
    let mut guard = sessions.lock().await;
    guard.sync_windows(session).await;
    action(&mut guard)
}

async fn reset_current_frame(sessions: &Mutex<Sessions>, session: &str) {
    let mut guard = sessions.lock().await;
    if let Some(found) = guard.open.get_mut(session) {
        found.current_frame = FrameId::MAIN;
    }
}

// https://w3c.github.io/webdriver/#navigate-to
async fn navigate(sessions: &Mutex<Sessions>, session: &str, body: &str) -> (u16, Value) {
    let parsed: Value = match serde_json::from_str(body) {
        Ok(value) => value,
        Err(err) => return error(400, "invalid argument", &err.to_string()),
    };
    let Some(url) = parsed.get("url").and_then(Value::as_str) else {
        return error(400, "invalid argument", "missing url");
    };
    reset_current_frame(sessions, session).await;
    let snapshot = match snapshot_checking_alert(sessions, session).await {
        Ok(snapshot) => snapshot,
        Err(reply) => return reply,
    };
    let events = snapshot.tab.subscribe().ok();
    let mut prompt_rx = snapshot.browser.prompt_broker().subscribe();
    let tab_id = snapshot.tab.id();
    match snapshot.tab.goto(url).await {
        Ok(()) => {
            tokio::select! {
                res = wait_for_navigation_strategy(
                    &snapshot.tab,
                    snapshot.page_load_strategy,
                    snapshot.page_load_timeout,
                    events,
                ) => res,
                () = wait_for_prompt_opened(&mut prompt_rx, tab_id) => ok(Value::Null),
            }
        }
        Err(TabError::InvalidUrl { spec }) => error(400, "invalid argument", &spec),
        Err(err) => error(500, "unknown error", &err.to_string()),
    }
}

async fn wait_for_navigation_strategy(
    tab: &TabHandle,
    strategy: PageLoadStrategy,
    timeout: Option<Duration>,
    mut events: Option<tokio::sync::mpsc::Receiver<TabEvent>>,
) -> (u16, Value) {
    let budget = timeout.unwrap_or(Duration::from_secs(3600));
    match strategy {
        PageLoadStrategy::None => ok(Value::Null),
        PageLoadStrategy::Eager => {
            if let Some(rx) = events.as_mut() {
                let deadline = tokio::time::Instant::now() + budget;
                loop {
                    match tokio::time::timeout_at(deadline, rx.recv()).await {
                        Ok(Some(TabEvent::DomContentLoaded | TabEvent::Load) | None) => break,
                        Ok(Some(TabEvent::NavigationFailed(_))) => {
                            return error(500, "unknown error", "navigation failed");
                        }
                        Ok(Some(_)) => {}
                        Err(_) => return error(500, "timeout", "navigation timed out"),
                    }
                }
                ok(Value::Null)
            } else {
                wait_for_normal_load(tab, budget).await
            }
        }
        PageLoadStrategy::Normal => wait_for_normal_load(tab, budget).await,
    }
}

async fn wait_for_normal_load(tab: &TabHandle, timeout: Duration) -> (u16, Value) {
    match tab.run_until_load_timeout(timeout).await {
        Ok(false) => return error(500, "timeout", "navigation timed out"),
        Ok(true) => {}
        Err(err) => return error(500, "unknown error", &err.to_string()),
    }
    match tab.last_navigation_failed().await {
        Ok(true) => error(500, "unknown error", "navigation failed"),
        Ok(false) => ok(Value::Null),
        Err(err) => error(500, "unknown error", &err.to_string()),
    }
}

// https://w3c.github.io/webdriver/#back
// https://w3c.github.io/webdriver/#forward
async fn traverse_history(sessions: &Mutex<Sessions>, session: &str, delta: i32) -> (u16, Value) {
    let snapshot = match snapshot_checking_alert(sessions, session).await {
        Ok(snapshot) => snapshot,
        Err(reply) => return reply,
    };
    reset_current_frame(sessions, session).await;
    let Ok(history) = snapshot.tab.history().await else {
        return ok(Value::Null);
    };
    let current_index = history.current_index();
    let target_index = if delta < 0 {
        let step = usize::try_from(delta.unsigned_abs()).unwrap_or(usize::MAX);
        current_index.checked_sub(step)
    } else {
        let step = usize::try_from(delta).unwrap_or(usize::MAX);
        current_index
            .checked_add(step)
            .filter(|idx| *idx < history.entries().len())
    };
    if target_index.is_none() {
        return ok(Value::Null);
    }
    let events = snapshot.tab.subscribe().ok();
    let mut prompt_rx = snapshot.browser.prompt_broker().subscribe();
    let tab_id = snapshot.tab.id();
    if let Err(err) = snapshot.tab.traverse_history(delta).await {
        return error(500, "unknown error", &err.to_string());
    }
    tokio::select! {
        res = wait_for_navigation_strategy(
            &snapshot.tab,
            snapshot.page_load_strategy,
            snapshot.page_load_timeout,
            events,
        ) => res,
        () = wait_for_prompt_opened(&mut prompt_rx, tab_id) => ok(Value::Null),
    }
}

// https://w3c.github.io/webdriver/#refresh
async fn refresh(sessions: &Mutex<Sessions>, session: &str) -> (u16, Value) {
    let snapshot = match snapshot_checking_alert(sessions, session).await {
        Ok(snapshot) => snapshot,
        Err(reply) => return reply,
    };
    reset_current_frame(sessions, session).await;
    let events = snapshot.tab.subscribe().ok();
    let mut prompt_rx = snapshot.browser.prompt_broker().subscribe();
    let tab_id = snapshot.tab.id();
    match snapshot.tab.reload().await {
        Ok(()) => {
            tokio::select! {
                res = wait_for_navigation_strategy(
                    &snapshot.tab,
                    snapshot.page_load_strategy,
                    snapshot.page_load_timeout,
                    events,
                ) => res,
                () = wait_for_prompt_opened(&mut prompt_rx, tab_id) => ok(Value::Null),
            }
        }
        Err(err) => error(500, "unknown error", &err.to_string()),
    }
}

// https://w3c.github.io/webdriver/#get-current-url
async fn current_url(sessions: &Mutex<Sessions>, session: &str) -> (u16, Value) {
    let snapshot = match snapshot_checking_alert(sessions, session).await {
        Ok(snapshot) => snapshot,
        Err(reply) => return reply,
    };
    match snapshot.tab.document_url().await {
        Ok(url) => ok(json!(url)),
        Err(err) => error(500, "unknown error", &err.to_string()),
    }
}

// https://w3c.github.io/webdriver/#get-title
async fn get_title(sessions: &Mutex<Sessions>, session: &str) -> (u16, Value) {
    let snapshot = match snapshot_checking_alert(sessions, session).await {
        Ok(snapshot) => snapshot,
        Err(reply) => return reply,
    };
    match snapshot.tab.document_title().await {
        Ok(title) => ok(json!(title)),
        Err(err) => error(500, "unknown error", &err.to_string()),
    }
}

// https://w3c.github.io/webdriver/#get-page-source
async fn get_page_source(sessions: &Mutex<Sessions>, session: &str) -> (u16, Value) {
    let snapshot = match snapshot_checking_alert(sessions, session).await {
        Ok(snapshot) => snapshot,
        Err(reply) => return reply,
    };
    let script = "document.documentElement ? document.documentElement.outerHTML : ''";
    match exec_in_frame(&snapshot, script).await {
        Ok(RemoteValue::String(html)) => ok(json!(html)),
        Ok(_) => ok(json!("")),
        Err(err) => script_error(&err),
    }
}

// https://w3c.github.io/webdriver/#take-screenshot
async fn take_screenshot(sessions: &Mutex<Sessions>, session: &str) -> (u16, Value) {
    let snapshot = match snapshot_checking_alert(sessions, session).await {
        Ok(snapshot) => snapshot,
        Err(reply) => return reply,
    };
    let (width, height) = snapshot
        .tab
        .viewport_size()
        .await
        .unwrap_or(browser::DEFAULT_VIEWPORT);
    let request = ScreenshotRequest {
        viewport_width: width,
        viewport_height: height,
        clip: None,
        format: ScreenshotFormat::Png,
        omit_background: false,
    };
    match snapshot.tab.screenshot(request).await {
        Ok(png) => ok(json!(BASE64.encode(&png))),
        Err(err) => error(500, "unable to capture screen", &err.to_string()),
    }
}

// https://w3c.github.io/webdriver/#take-element-screenshot
async fn take_element_screenshot(
    sessions: &Mutex<Sessions>,
    session: &str,
    element: &str,
) -> (u16, Value) {
    let snapshot = match snapshot_checking_alert(sessions, session).await {
        Ok(snapshot) => snapshot,
        Err(reply) => return reply,
    };
    let remote = match resolve_element_remote(&snapshot, element) {
        Ok(remote) => remote,
        Err(reply) => return reply,
    };
    let script = format!(
        "(function(){{const el=host.__tb_webdriver_element({remote});\
         if(el===null)return null;\
         const r=el.getBoundingClientRect();\
         return JSON.stringify({{x:r.x,y:r.y,width:r.width,height:r.height}});}})()"
    );
    let rect_val = match exec_in_frame(&snapshot, &script).await {
        Ok(RemoteValue::String(text)) => match serde_json::from_str::<Value>(&text) {
            Ok(v) => v,
            Err(err) => return error(500, "unknown error", &err.to_string()),
        },
        Ok(_) => return error(404, "stale element reference", "element not found"),
        Err(err) => return script_error(&err),
    };
    let x = rect_val.get("x").and_then(Value::as_f64).unwrap_or(0.0);
    let y = rect_val.get("y").and_then(Value::as_f64).unwrap_or(0.0);
    let w = rect_val.get("width").and_then(Value::as_f64).unwrap_or(0.0);
    let h = rect_val
        .get("height")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    if w <= 0.0 || h <= 0.0 {
        return error(500, "unable to capture screen", "element has zero size");
    }
    let (vw, vh) = snapshot
        .tab
        .viewport_size()
        .await
        .unwrap_or(browser::DEFAULT_VIEWPORT);
    #[expect(
        clippy::cast_possible_truncation,
        reason = "CSS pixel clip coordinates fit in f32"
    )]
    let clip = ScreenshotClip {
        x: x.max(0.0) as f32,
        y: y.max(0.0) as f32,
        width: w as f32,
        height: h as f32,
    };
    let request = ScreenshotRequest {
        viewport_width: vw,
        viewport_height: vh,
        clip: Some(clip),
        format: ScreenshotFormat::Png,
        omit_background: false,
    };
    match snapshot.tab.screenshot_frame(snapshot.frame, request).await {
        Ok(png) => ok(json!(BASE64.encode(&png))),
        Err(err) => error(500, "unable to capture screen", &err.to_string()),
    }
}

// https://w3c.github.io/webdriver/#switch-to-frame
async fn switch_to_frame(sessions: &Mutex<Sessions>, session: &str, body: &str) -> (u16, Value) {
    let parsed: Value = match serde_json::from_str(body) {
        Ok(value) => value,
        Err(err) => return error(400, "invalid argument", &err.to_string()),
    };
    let Some(id_val) = parsed.get("id") else {
        return error(400, "invalid argument", "id is required");
    };
    let snapshot = match snapshot_checking_alert(sessions, session).await {
        Ok(snapshot) => snapshot,
        Err(reply) => return reply,
    };
    if id_val.is_null() {
        reset_current_frame(sessions, session).await;
        return ok(Value::Null);
    }
    let target_frame = if let Some(index) = id_val.as_u64() {
        if index > 65_535 {
            return error(400, "invalid argument", "frame index out of range");
        }
        let Ok(frames) = snapshot.tab.frames().await else {
            return error(500, "unknown error", "failed to inspect frames");
        };
        let children: Vec<FrameId> = frames
            .into_iter()
            .filter(|f| f.parent == Some(snapshot.frame))
            .map(|f| f.id)
            .collect();
        let idx = usize::try_from(index).unwrap_or(usize::MAX);
        match children.get(idx) {
            Some(frame) => *frame,
            None => return error(400, "no such frame", "frame index not found"),
        }
    } else if let Some(element_ref) = id_val.get(ELEMENT_KEY).and_then(Value::as_str) {
        let remote = match resolve_element_remote(&snapshot, element_ref) {
            Ok(remote) => remote,
            Err(reply) => return reply,
        };
        let script = format!(
            "(function(){{\
             const el=host.__tb_webdriver_element({remote});\
             if(el===null)return -1;\
             const tag=el.tagName?el.tagName.toUpperCase():'';\
             if(tag!=='IFRAME'&&tag!=='FRAME')return -2;\
             const frames=Array.from(document.querySelectorAll('iframe,frame'));\
             return frames.indexOf(el);\
             }})()"
        );
        let idx = match exec_in_frame(&snapshot, &script).await {
            Ok(RemoteValue::Number(n)) => {
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "frame index is a small non-negative integer or sentinel -1/-2"
                )]
                let i = n as i64;
                if i == -1 {
                    return error(404, "no such element", "element not found");
                }
                if i < 0 {
                    return error(400, "no such frame", "element is not a frame");
                }
                usize::try_from(i).unwrap_or(usize::MAX)
            }
            _ => return error(400, "no such frame", "failed to locate frame element"),
        };
        let Ok(frames) = snapshot.tab.frames().await else {
            return error(500, "unknown error", "failed to inspect frames");
        };
        let children: Vec<FrameId> = frames
            .into_iter()
            .filter(|f| f.parent == Some(snapshot.frame))
            .map(|f| f.id)
            .collect();
        match children.get(idx) {
            Some(frame) => *frame,
            None => {
                return error(
                    400,
                    "no such frame",
                    "frame element has no browsing context",
                );
            }
        }
    } else {
        return error(
            400,
            "invalid argument",
            "id must be null, integer, or element",
        );
    };
    let mut guard = sessions.lock().await;
    if let Some(found) = guard.open.get_mut(session) {
        found.current_frame = target_frame;
    }
    ok(Value::Null)
}

// https://w3c.github.io/webdriver/#switch-to-parent-frame
async fn switch_to_parent_frame(sessions: &Mutex<Sessions>, session: &str) -> (u16, Value) {
    let snapshot = match snapshot_checking_alert(sessions, session).await {
        Ok(snapshot) => snapshot,
        Err(reply) => return reply,
    };
    let parent = if snapshot.frame == FrameId::MAIN {
        FrameId::MAIN
    } else {
        snapshot
            .tab
            .frames()
            .await
            .ok()
            .and_then(|frames| {
                frames
                    .into_iter()
                    .find(|f| f.id == snapshot.frame)
                    .and_then(|f| f.parent)
            })
            .unwrap_or(FrameId::MAIN)
    };
    let mut guard = sessions.lock().await;
    if let Some(found) = guard.open.get_mut(session) {
        found.current_frame = parent;
    }
    ok(Value::Null)
}

// https://w3c.github.io/webdriver/#executing-script
async fn execute(
    sessions: &Mutex<Sessions>,
    session: &str,
    body: &str,
    asynchronous: bool,
) -> (u16, Value) {
    let parsed: Value = match serde_json::from_str(body) {
        Ok(value) => value,
        Err(err) => return error(400, "invalid argument", &err.to_string()),
    };
    let Some(script) = parsed.get("script").and_then(Value::as_str) else {
        return error(400, "invalid argument", "script must be a string");
    };
    let Some(args_val) = parsed.get("args").filter(|v| v.is_array()) else {
        return error(400, "invalid argument", "args must be an array");
    };
    let snapshot = match snapshot_checking_alert(sessions, session).await {
        Ok(snapshot) => snapshot,
        Err(reply) => return reply,
    };
    let (slot_id, known_handles) = {
        let mut guard = sessions.lock().await;
        let slot_id = guard.next_call_slot();
        let handles = guard.ordered_handles(session).unwrap_or_default();
        (slot_id, handles)
    };
    let started = Instant::now();
    let wrapped = match wrap_script(
        script,
        args_val,
        asynchronous,
        slot_id,
        &snapshot,
        &known_handles,
    ) {
        Ok(code) => code,
        Err(reply) => return reply,
    };
    let mut prompt_rx = snapshot.browser.prompt_broker().subscribe();
    let tab_id = snapshot.tab.id();
    let exec_result = {
        let exec_fut = snapshot.tab.execute_script(browser::ExecuteScriptOptions {
            frame: snapshot.frame,
            source: browser::ScriptSource::Browser(&wrapped),
            timeout: snapshot.script_timeout,
        });
        tokio::select! {
            res = exec_fut => Some(res),
            () = wait_for_prompt_opened(&mut prompt_rx, tab_id) => None,
        }
    };
    let Some(exec_result) = exec_result else {
        return ok(Value::Null);
    };
    match exec_result {
        Err(err) => script_error(&err),
        Ok(_) if asynchronous => {
            tokio::select! {
                res = wait_for_async(
                    &snapshot,
                    slot_id,
                    remaining_opt(started, snapshot.script_timeout),
                ) => res,
                () = wait_for_prompt_opened(&mut prompt_rx, tab_id) => ok(Value::Null),
            }
        }
        Ok(value) => {
            let check = format!(
                "(host.__tbWdSlots && host.__tbWdSlots[{slot_id}] && host.__tbWdSlots[{slot_id}].wait === true)"
            );
            match exec_in_frame(&snapshot, &check).await {
                Ok(RemoteValue::Bool(true)) => {
                    tokio::select! {
                        res = wait_for_async(
                            &snapshot,
                            slot_id,
                            remaining_opt(started, snapshot.script_timeout),
                        ) => res,
                        () = wait_for_prompt_opened(&mut prompt_rx, tab_id) => ok(Value::Null),
                    }
                }
                Ok(_) => {
                    let stale_check = format!(
                        "(host.__tbWdSlots && host.__tbWdSlots[{slot_id}] && host.__tbWdSlots[{slot_id}].stale === true)"
                    );
                    let is_stale = matches!(
                        exec_in_frame(&snapshot, &stale_check).await,
                        Ok(RemoteValue::Bool(true))
                    );
                    let cleanup = format!("host.__tbWdSlots && delete host.__tbWdSlots[{slot_id}]");
                    let _ = exec_in_frame(&snapshot, &cleanup).await;
                    if is_stale {
                        return error(404, "stale element reference", "stale element argument");
                    }
                    ok(encode_scoped(&value, &snapshot.handle, snapshot.frame))
                }
                Err(err) => script_error(&err),
            }
        }
    }
}

async fn wait_for_prompt_opened(
    rx: &mut tokio::sync::broadcast::Receiver<PromptEvent>,
    tab: TabId,
) {
    loop {
        match rx.recv().await {
            Ok(PromptEvent::Opened { info, .. }) if info.tab == tab => return,
            Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                std::future::pending::<()>().await;
            }
        }
    }
}

fn remaining_opt(started: Instant, budget: Option<Duration>) -> Duration {
    budget
        .unwrap_or(Duration::from_secs(3600))
        .saturating_sub(started.elapsed())
}

async fn wait_for_async(
    snapshot: &WindowSnapshot,
    slot_id: u64,
    script_timeout: Duration,
) -> (u16, Value) {
    let done_expr = format!(
        "Boolean(host.__tbWdSlots && host.__tbWdSlots[{slot_id}] && host.__tbWdSlots[{slot_id}].done === true)"
    );
    let settled = snapshot
        .tab
        .run_until_js_true(browser::RunUntilJsTrueOptions {
            frame: snapshot.frame,
            source: browser::ScriptSource::Browser(&done_expr),
            timeout: script_timeout,
        })
        .await;
    match settled {
        Ok(false) => {
            let cleanup = format!("host.__tbWdSlots && delete host.__tbWdSlots[{slot_id}]");
            let _ = exec_in_frame(snapshot, &cleanup).await;
            return error(500, "script timeout", "script timeout");
        }
        Ok(true) => {}
        Err(err) => return error(500, "unknown error", &err.to_string()),
    }
    let stale_expr = format!(
        "Boolean(host.__tbWdSlots && host.__tbWdSlots[{slot_id}] && host.__tbWdSlots[{slot_id}].stale === true)"
    );
    if matches!(
        exec_in_frame(snapshot, &stale_expr).await,
        Ok(RemoteValue::Bool(true))
    ) {
        let cleanup = format!("host.__tbWdSlots && delete host.__tbWdSlots[{slot_id}]");
        let _ = exec_in_frame(snapshot, &cleanup).await;
        return error(404, "stale element reference", "stale element argument");
    }
    let failed_expr = format!(
        "Boolean(host.__tbWdSlots && host.__tbWdSlots[{slot_id}] && host.__tbWdSlots[{slot_id}].failed === true)"
    );
    match exec_in_frame(snapshot, &failed_expr).await {
        Ok(RemoteValue::Bool(true)) => {
            let err_expr = format!("String(host.__tbWdSlots[{slot_id}].error)");
            let message = match exec_in_frame(snapshot, &err_expr).await {
                Ok(RemoteValue::String(text)) => text,
                Ok(_) => "javascript error".to_owned(),
                Err(err) => return script_error(&err),
            };
            let cleanup = format!("host.__tbWdSlots && delete host.__tbWdSlots[{slot_id}]");
            let _ = exec_in_frame(snapshot, &cleanup).await;
            return error(500, "javascript error", &message);
        }
        Ok(_) => {}
        Err(err) => return script_error(&err),
    }
    let val_expr = format!("host.__tbWdSlots[{slot_id}].value");
    let result = match exec_in_frame(snapshot, &val_expr).await {
        Ok(value) => ok(encode_scoped(&value, &snapshot.handle, snapshot.frame)),
        Err(err) => script_error(&err),
    };
    let cleanup = format!("host.__tbWdSlots && delete host.__tbWdSlots[{slot_id}]");
    let _ = exec_in_frame(snapshot, &cleanup).await;
    result
}

fn script_error(err: &TabError) -> (u16, Value) {
    match err {
        TabError::Script(ScriptFailure::Interrupted) => {
            error(500, "script timeout", "script timeout")
        }
        TabError::Script(
            ScriptFailure::Engine { .. } | ScriptFailure::BadTimerId | ScriptFailure::HostMissing,
        )
        | TabError::InvalidUrl { .. }
        | TabError::UnknownFrame { .. }
        | TabError::UnknownHistoryEntry { .. }
        | TabError::ActorStopped
        | TabError::RendererUnavailable { .. }
        | TabError::ResourceLimit { .. }
        | TabError::Render { .. }
        | TabError::Dom { .. } => error(500, "javascript error", &err.to_string()),
    }
}

fn prepare_script_args(
    value: &Value,
    snapshot: &WindowSnapshot,
    known_handles: &[String],
) -> Result<Value, (u16, Value)> {
    match value {
        Value::Object(map) => {
            if let Some(el_val) = map.get(ELEMENT_KEY) {
                let Some(el_ref) = el_val.as_str() else {
                    return Err(error(
                        400,
                        "invalid argument",
                        "element identifier must be a string",
                    ));
                };
                let remote = resolve_element_remote(snapshot, el_ref)?;
                return Ok(json!({"__tbWdElement": remote}));
            }
            if let Some(sh_val) = map.get(SHADOW_KEY) {
                let Some(sh_ref) = sh_val.as_str() else {
                    return Err(error(
                        400,
                        "invalid argument",
                        "shadow root identifier must be a string",
                    ));
                };
                let remote = resolve_element_remote(snapshot, sh_ref)?;
                return Ok(json!({"__tbWdElement": remote}));
            }
            if let Some(win_val) = map.get(WINDOW_KEY) {
                let Some(win_ref) = win_val.as_str() else {
                    return Err(error(
                        400,
                        "invalid argument",
                        "window identifier must be a string",
                    ));
                };
                if !known_handles.iter().any(|h| h == win_ref) {
                    return Err(error(404, "no such window", win_ref));
                }
                return Ok(json!({"__tbWdWindow": win_ref}));
            }
            if let Some(frame_val) = map.get(FRAME_KEY) {
                let Some(frame_ref) = frame_val.as_str() else {
                    return Err(error(
                        400,
                        "invalid argument",
                        "frame identifier must be a string",
                    ));
                };
                return Err(error(404, "no such frame", frame_ref));
            }
            let mut out = Map::with_capacity(map.len());
            for (k, v) in map {
                out.insert(k.clone(), prepare_script_args(v, snapshot, known_handles)?);
            }
            Ok(Value::Object(out))
        }
        Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(prepare_script_args(item, snapshot, known_handles)?);
            }
            Ok(Value::Array(out))
        }
        other => Ok(other.clone()),
    }
}

fn wd_script_setup(slot_id: u64, handle_json: &Value) -> String {
    format!(
        "if (!host.__tbWdSlots) host.__tbWdSlots = Object.create(null);\n\
         const slot = {{ __proto__: null, value: undefined, error: undefined, failed: false, done: false, wait: false, stale: false }};\n\
         host.__tbWdSlots[{slot_id}] = slot;\n\
         function revive(v) {{\n\
           if (v === null || typeof v !== 'object') return v;\n\
           if (typeof v.__tbWdElement === 'number') {{\n\
             const el = host.__tb_webdriver_element(v.__tbWdElement);\n\
             if (el === null) {{ slot.stale = true; slot.done = true; throw new Error('stale'); }}\n\
             return el;\n\
           }}\n\
           if (typeof v.__tbWdWindow === 'string') {{\n\
             if (v.__tbWdWindow === {handle_json}) return globalThis;\n\
             throw new Error('cross-window reference is not accessible');\n\
           }}\n\
           if (Array.isArray(v)) return v.map(revive);\n\
           const out = {{}};\n\
           for (const k of Object.keys(v)) out[k] = revive(v[k]);\n\
           return out;\n\
         }}\n\
         function wdClone(v, seen) {{\n\
           if (v === null || v === undefined) return null;\n\
           const t = typeof v;\n\
           if (t === 'boolean' || t === 'string') return v;\n\
           if (t === 'number') return Number.isFinite(v) ? v : null;\n\
           if (t !== 'object') return null;\n\
           if (v === globalThis || (v && v.window === v && typeof v.document === 'object')) {{\n\
             const key = (v.parent && v.parent !== v) ? '{FRAME_KEY}' : '{WINDOW_KEY}';\n\
             return {{ [key]: {handle_json} }};\n\
           }}\n\
           if (typeof Element !== 'undefined' && v instanceof Element) return v;\n\
           if (typeof ShadowRoot !== 'undefined' && v instanceof ShadowRoot) return v;\n\
           if (seen.has(v)) throw new TypeError('cyclic object value');\n\
           seen.add(v);\n\
           try {{\n\
             if (Array.isArray(v) ||\n\
                 (typeof NodeList !== 'undefined' && v instanceof NodeList) ||\n\
                 (typeof HTMLCollection !== 'undefined' && v instanceof HTMLCollection) ||\n\
                 (typeof FileList !== 'undefined' && v instanceof FileList)) {{\n\
               const arr = [];\n\
               for (let i = 0; i < v.length; i++) arr.push(wdClone(v[i], seen));\n\
               return arr;\n\
             }}\n\
             if (typeof v.toJSON === 'function') return wdClone(v.toJSON(), seen);\n\
             const out = {{}};\n\
             for (const k of Object.keys(v)) out[k] = wdClone(v[k], seen);\n\
             return out;\n\
           }} finally {{\n\
             seen.delete(v);\n\
           }}\n\
         }}"
    )
}

fn wrap_script(
    script: &str,
    args: &Value,
    asynchronous: bool,
    slot_id: u64,
    snapshot: &WindowSnapshot,
    known_handles: &[String],
) -> Result<String, (u16, Value)> {
    let prepared = prepare_script_args(args, snapshot, known_handles)?;
    let args_json = prepared.to_string();
    let handle_json = json!(snapshot.handle);
    let function = Value::String(format!("(function() {{\n{script}\n}})")).to_string();
    let setup = wd_script_setup(slot_id, &handle_json);
    if asynchronous {
        Ok(format!(
            "(() => {{ {setup}\n\
             let args;\n\
             try {{ args = revive({args_json}); }} catch (_) {{ return null; }}\n\
             args[args.length] = function(v) {{\n\
               if (slot.done) return;\n\
               try {{ slot.value = wdClone(v, new Set()); }} catch (e) {{ slot.failed = true; slot.error = String(e && e.message ? e.message : e); }}\n\
               slot.done = true;\n\
             }};\n\
             try {{\n\
               const r = host.apply(host.evaluatePage({function}), null, args);\n\
               if (r && typeof r.then === 'function') {{\n\
                 r.then(undefined, function(e) {{\n\
                   if (slot.done) return;\n\
                   slot.failed = true;\n\
                   slot.error = e == null ? 'undefined' : (e && e.message ? String(e.message) : String(e));\n\
                   slot.done = true;\n\
                 }});\n\
               }}\n\
             }} catch (e) {{\n\
               if (!slot.done) {{\n\
                 slot.failed = true;\n\
                 slot.error = e == null ? 'undefined' : (e && e.message ? String(e.message) : String(e));\n\
                 slot.done = true;\n\
               }}\n\
             }}\n\
             }})()"
        ))
    } else {
        Ok(format!(
            "(function() {{ {setup}\n\
               let args;\n\
               try {{ args = revive({args_json}); }} catch (_) {{ return null; }}\n\
               var result = host.apply(host.evaluatePage({function}), null, args);\n\
               if (result && typeof result.then === 'function') {{\n\
                 result.then(function(v) {{\n\
                   try {{ slot.value = wdClone(v, new Set()); }} catch (e) {{ slot.failed = true; slot.error = String(e && e.message ? e.message : e); }}\n\
                   slot.done = true;\n\
                 }}, function(e) {{\n\
                   slot.failed = true;\n\
                   slot.error = e == null ? 'undefined' : (e && e.message ? String(e.message) : String(e));\n\
                   slot.done = true;\n\
                 }});\n\
                 slot.wait = true;\n\
                 return null;\n\
               }}\n\
               return wdClone(result, new Set());\n\
             }})()"
        ))
    }
}

fn format_element_id(handle: &str, frame: FrameId, remote: u64) -> String {
    if frame == FrameId::MAIN {
        format!("{handle}:{remote}")
    } else {
        format!("{handle}:{}:{remote}", frame.get())
    }
}

fn format_shadow_id(handle: &str, frame: FrameId, remote: u64) -> String {
    if frame == FrameId::MAIN {
        format!("{handle}:s{remote}")
    } else {
        format!("{handle}:{}:s{remote}", frame.get())
    }
}

fn resolve_shadow_remote(snapshot: &WindowSnapshot, shadow: &str) -> Result<u64, (u16, Value)> {
    let parts: Vec<&str> = shadow.split(':').collect();
    let (handle, frame, id) = match parts.as_slice() {
        [handle, id] => (*handle, FrameId::MAIN, *id),
        [handle, frame, id] => {
            let frame = frame
                .parse::<u64>()
                .map_err(|_| error(404, "no such shadow root", "unknown shadow root id"))?;
            (*handle, FrameId::new(frame), *id)
        }
        _ => return Err(error(404, "no such shadow root", "unknown shadow root id")),
    };
    let Some(id) = id.strip_prefix('s') else {
        return Err(error(404, "no such shadow root", "unknown shadow root id"));
    };
    let remote = id
        .parse::<u64>()
        .map_err(|_| error(404, "no such shadow root", "unknown shadow root id"))?;
    if handle != snapshot.handle || frame != snapshot.frame {
        return Err(error(
            404,
            "no such shadow root",
            "shadow root belongs to another browsing context",
        ));
    }
    Ok(remote)
}

fn encode_scoped(value: &RemoteValue, handle: &str, frame: FrameId) -> Value {
    match value {
        RemoteValue::Node(id) => json!({ ELEMENT_KEY: format_element_id(handle, frame, *id) }),
        RemoteValue::List(items) => Value::Array(
            items
                .iter()
                .map(|item| encode_scoped(item, handle, frame))
                .collect(),
        ),
        RemoteValue::Map(entries) => {
            let mut map = Map::new();
            for (key, item) in entries {
                map.insert(key.clone(), encode_scoped(item, handle, frame));
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
            let mut map = Map::new();
            for (key, item) in entries {
                map.insert(key.clone(), encode(item));
            }
            Value::Object(map)
        }
        RemoteValue::Node(id) => json!({ ELEMENT_KEY: id.to_string() }),
    }
}

// https://w3c.github.io/webdriver/#elements
fn resolve_element_remote(snapshot: &WindowSnapshot, element: &str) -> Result<u64, (u16, Value)> {
    let parts: Vec<&str> = element.split(':').collect();
    match parts.as_slice() {
        [handle, id] => {
            let remote = id
                .parse::<u64>()
                .map_err(|_| error(404, "no such element", "unknown element id"))?;
            if *handle != snapshot.handle || snapshot.frame != FrameId::MAIN {
                return Err(error(
                    404,
                    "no such element",
                    "element belongs to another browsing context",
                ));
            }
            Ok(remote)
        }
        [handle, frame_str, id] => {
            let frame_id = frame_str
                .parse::<u64>()
                .map_err(|_| error(404, "no such element", "unknown element id"))?;
            let remote = id
                .parse::<u64>()
                .map_err(|_| error(404, "no such element", "unknown element id"))?;
            if *handle != snapshot.handle || FrameId::new(frame_id) != snapshot.frame {
                return Err(error(
                    404,
                    "no such element",
                    "element belongs to another browsing context",
                ));
            }
            Ok(remote)
        }
        _ => Err(error(404, "no such element", "unknown element id")),
    }
}

async fn exec_in_frame(snapshot: &WindowSnapshot, script: &str) -> Result<RemoteValue, TabError> {
    snapshot
        .tab
        .execute_script(browser::ExecuteScriptOptions {
            frame: snapshot.frame,
            source: browser::ScriptSource::Browser(script),
            timeout: snapshot.script_timeout,
        })
        .await
}

// https://w3c.github.io/webdriver/#find-elements
async fn find_matches_once(
    snapshot: &WindowSnapshot,
    using: &str,
    selector: &str,
    root_remote: Option<u64>,
    is_shadow: bool,
) -> Result<Vec<u64>, (u16, Value)> {
    let root_expr = match (root_remote, is_shadow) {
        (None, _) => "document".to_owned(),
        (Some(remote), false) => format!(
            "(() => {{ const r = host.__tb_webdriver_element({remote}); if (r === null) return null; return r; }})()"
        ),
        (Some(remote), true) => format!(
            "(() => {{ const r = host.__tb_webdriver_element({remote}); if (r === null) return null; return r.shadowRoot || r; }})()"
        ),
    };
    let sel_json = json!(selector);
    let body = match using {
        "css selector" => format!(
            "try {{ return Array.from(root.querySelectorAll({sel_json})); }} catch (e) {{ return '__tb_invalid_selector'; }}"
        ),
        "tag name" => format!(
            "if ({sel_json} === '') return '__tb_invalid_selector';\n\
             try {{ return Array.from(root.getElementsByTagName({sel_json})); }} catch (e) {{ return '__tb_invalid_selector'; }}"
        ),
        "link text" => format!(
            "return Array.from(root.querySelectorAll('a')).filter(a => (a.innerText ?? a.textContent ?? '').trim() === {sel_json});"
        ),
        "partial link text" => format!(
            "return Array.from(root.querySelectorAll('a')).filter(a => (a.innerText ?? a.textContent ?? '').includes({sel_json}));"
        ),
        "xpath" => format!(
            "try {{\n\
               const snap = document.evaluate({sel_json}, root, null, 7, null);\n\
               const out = [];\n\
               for (let i = 0; i < snap.snapshotLength; i++) {{\n\
                 const n = snap.snapshotItem(i);\n\
                 if (n && n.nodeType === 1) out.push(n);\n\
               }}\n\
               return out;\n\
             }} catch (e) {{ return '__tb_invalid_selector'; }}"
        ),
        _ => {
            return Err(error(
                400,
                "invalid argument",
                "unsupported locator strategy",
            ));
        }
    };
    let script = format!(
        "(function(){{\n\
           const root = {root_expr};\n\
           if (root === null) return '__tb_stale';\n\
           {body}\n\
         }})()"
    );
    match exec_in_frame(snapshot, &script).await {
        Ok(RemoteValue::List(items)) => Ok(items
            .into_iter()
            .filter_map(|item| match item {
                RemoteValue::Node(id) => Some(id),
                _ => None,
            })
            .collect()),
        Ok(RemoteValue::String(text)) if text == "__tb_invalid_selector" => Err(error(
            400,
            "invalid selector",
            "the selector is not a valid selector",
        )),
        Ok(RemoteValue::String(text)) if text == "__tb_stale" => Err(error(
            404,
            if is_shadow {
                "detached shadow root"
            } else {
                "stale element reference"
            },
            "root element is no longer attached",
        )),
        Ok(_) => Ok(Vec::new()),
        Err(err) => Err(error(500, "unknown error", &err.to_string())),
    }
}

async fn find_matches_with_implicit(
    snapshot: &WindowSnapshot,
    root_element: Option<&str>,
    is_shadow: bool,
    body: &str,
) -> Result<Vec<u64>, (u16, Value)> {
    let parsed: Value = serde_json::from_str(body)
        .map_err(|err| error(400, "invalid argument", &err.to_string()))?;
    let Some(using) = parsed.get("using").and_then(Value::as_str) else {
        return Err(error(400, "invalid argument", "using is required"));
    };
    let Some(selector) = parsed.get("value").and_then(Value::as_str) else {
        return Err(error(400, "invalid argument", "value is required"));
    };
    if !matches!(
        using,
        "css selector" | "link text" | "partial link text" | "tag name" | "xpath"
    ) {
        return Err(error(
            400,
            "invalid argument",
            "unsupported locator strategy",
        ));
    }
    let root_remote = match root_element {
        Some(el) if is_shadow => Some(resolve_shadow_remote(snapshot, el)?),
        Some(el) => Some(resolve_element_remote(snapshot, el)?),
        None => None,
    };
    let deadline = Instant::now() + snapshot.implicit_timeout.unwrap_or(Duration::ZERO);
    loop {
        let matches = find_matches_once(snapshot, using, selector, root_remote, is_shadow).await?;
        if !matches.is_empty() || Instant::now() >= deadline {
            return Ok(matches);
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

// https://w3c.github.io/webdriver/#find-element
async fn find_element(
    sessions: &Mutex<Sessions>,
    session: &str,
    root: Option<&str>,
    is_shadow: bool,
    body: &str,
) -> (u16, Value) {
    let snapshot = match snapshot_checking_alert(sessions, session).await {
        Ok(snapshot) => snapshot,
        Err(reply) => return reply,
    };
    match find_matches_with_implicit(&snapshot, root, is_shadow, body).await {
        Ok(matches) => match matches.first() {
            Some(id) => ok(json!({
                ELEMENT_KEY: format_element_id(&snapshot.handle, snapshot.frame, *id)
            })),
            None => error(404, "no such element", "no element matches the selector"),
        },
        Err(reply) => reply,
    }
}

// https://w3c.github.io/webdriver/#find-elements
async fn find_elements(
    sessions: &Mutex<Sessions>,
    session: &str,
    root: Option<&str>,
    is_shadow: bool,
    body: &str,
) -> (u16, Value) {
    let snapshot = match snapshot_checking_alert(sessions, session).await {
        Ok(snapshot) => snapshot,
        Err(reply) => return reply,
    };
    match find_matches_with_implicit(&snapshot, root, is_shadow, body).await {
        Ok(matches) => ok(Value::Array(
            matches
                .iter()
                .map(|id| {
                    json!({
                        ELEMENT_KEY: format_element_id(&snapshot.handle, snapshot.frame, *id)
                    })
                })
                .collect(),
        )),
        Err(reply) => reply,
    }
}

// https://w3c.github.io/webdriver/#get-active-element
async fn get_active_element(sessions: &Mutex<Sessions>, session: &str) -> (u16, Value) {
    let snapshot = match snapshot_checking_alert(sessions, session).await {
        Ok(snapshot) => snapshot,
        Err(reply) => return reply,
    };
    match exec_in_frame(&snapshot, "document.activeElement || document.body").await {
        Ok(RemoteValue::Node(id)) => ok(json!({
            ELEMENT_KEY: format_element_id(&snapshot.handle, snapshot.frame, id)
        })),
        Ok(_) => error(404, "no such element", "no active element"),
        Err(err) => script_error(&err),
    }
}

// https://w3c.github.io/webdriver/#get-element-shadow-root
async fn get_element_shadow_root(
    sessions: &Mutex<Sessions>,
    session: &str,
    element: &str,
) -> (u16, Value) {
    let snapshot = match snapshot_checking_alert(sessions, session).await {
        Ok(snapshot) => snapshot,
        Err(reply) => return reply,
    };
    let remote = match resolve_element_remote(&snapshot, element) {
        Ok(remote) => remote,
        Err(reply) => return reply,
    };
    let script = format!(
        "(function(){{\
           const el=host.__tb_webdriver_element({remote});\
           if(el===null)return '__tb_stale';\
           if(!el.shadowRoot)return '__tb_no_shadow';\
           return el.shadowRoot;\
         }})()"
    );
    match exec_in_frame(&snapshot, &script).await {
        Ok(RemoteValue::Node(id)) => ok(json!({
            SHADOW_KEY: format_shadow_id(&snapshot.handle, snapshot.frame, id)
        })),
        Ok(RemoteValue::String(s)) if s == "__tb_no_shadow" => {
            error(404, "no such shadow root", "element has no shadow root")
        }
        Ok(_) => error(404, "stale element reference", "element not found"),
        Err(err) => script_error(&err),
    }
}

// https://w3c.github.io/webdriver/#is-element-selected
async fn element_selected(
    sessions: &Mutex<Sessions>,
    session: &str,
    element: &str,
) -> (u16, Value) {
    let snapshot = match snapshot_checking_alert(sessions, session).await {
        Ok(snapshot) => snapshot,
        Err(reply) => return reply,
    };
    let remote = match resolve_element_remote(&snapshot, element) {
        Ok(remote) => remote,
        Err(reply) => return reply,
    };
    let script = format!(
        "(function(){{\
           const el=host.__tb_webdriver_element({remote});\
           if(el===null)return null;\
           const tag=el.tagName?el.tagName.toUpperCase():'';\
           if(tag==='INPUT'&&(el.type==='checkbox'||el.type==='radio'))return Boolean(el.checked);\
           if(tag==='OPTION')return Boolean(el.selected);\
           return false;\
         }})()"
    );
    match exec_in_frame(&snapshot, &script).await {
        Ok(RemoteValue::Bool(val)) => ok(json!(val)),
        Ok(_) => error(404, "stale element reference", "element not found"),
        Err(err) => script_error(&err),
    }
}

// https://w3c.github.io/webdriver/#get-element-attribute
async fn element_attribute(
    sessions: &Mutex<Sessions>,
    session: &str,
    element: &str,
    name: &str,
) -> (u16, Value) {
    let snapshot = match snapshot_checking_alert(sessions, session).await {
        Ok(snapshot) => snapshot,
        Err(reply) => return reply,
    };
    let remote = match resolve_element_remote(&snapshot, element) {
        Ok(remote) => remote,
        Err(reply) => return reply,
    };
    let name_json = json!(name);
    let script = format!(
        "(function(){{\
           const el=host.__tb_webdriver_element({remote});\
           if(el===null)return '__tb_stale';\
           const n={name_json};\
           const lower=n.toLowerCase();\
           const boolAttrs=new Set(['async','autofocus','autoplay','checked','compact','complete','controls','declare','default','defaultchecked','defaultselected','defer','disabled','ended','formnovalidate','hidden','indeterminate','iscontenteditable','ismap','itemscope','loop','multiple','muted','nohref','noresize','noshade','novalidate','nowrap','open','paused','pubdate','readonly','required','reversed','scoped','seamless','seeking','selected','truespeed','willvalidate']);\
           if(boolAttrs.has(lower)){{return el.hasAttribute(n)?'true':null;}}\
           if(lower==='value'&&'value' in el)return String(el.value);\
           return el.getAttribute(n);\
         }})()"
    );
    match exec_in_frame(&snapshot, &script).await {
        Ok(RemoteValue::String(text)) if text == "__tb_stale" => {
            error(404, "stale element reference", "element not found")
        }
        Ok(val) => ok(encode(&val)),
        Err(err) => script_error(&err),
    }
}

// https://w3c.github.io/webdriver/#get-element-property
async fn element_property(
    sessions: &Mutex<Sessions>,
    session: &str,
    element: &str,
    name: &str,
) -> (u16, Value) {
    let snapshot = match snapshot_checking_alert(sessions, session).await {
        Ok(snapshot) => snapshot,
        Err(reply) => return reply,
    };
    let remote = match resolve_element_remote(&snapshot, element) {
        Ok(remote) => remote,
        Err(reply) => return reply,
    };
    let name_json = json!(name);
    let script = format!(
        "(function(){{\
           const el=host.__tb_webdriver_element({remote});\
           if(el===null)return '__tb_stale';\
           const v=el[{name_json}];\
           return v===undefined?null:v;\
         }})()"
    );
    match exec_in_frame(&snapshot, &script).await {
        Ok(RemoteValue::String(text)) if text == "__tb_stale" => {
            error(404, "stale element reference", "element not found")
        }
        Ok(val) => ok(encode_scoped(&val, &snapshot.handle, snapshot.frame)),
        Err(err) => script_error(&err),
    }
}

// https://w3c.github.io/webdriver/#get-element-css-value
async fn element_css_value(
    sessions: &Mutex<Sessions>,
    session: &str,
    element: &str,
    property: &str,
) -> (u16, Value) {
    let snapshot = match snapshot_checking_alert(sessions, session).await {
        Ok(snapshot) => snapshot,
        Err(reply) => return reply,
    };
    let remote = match resolve_element_remote(&snapshot, element) {
        Ok(remote) => remote,
        Err(reply) => return reply,
    };
    let prop_json = json!(property);
    let script = format!(
        "(function(){{\
           const el=host.__tb_webdriver_element({remote});\
           if(el===null)return '__tb_stale';\
           const cs=getComputedStyle(el);\
           return cs?cs.getPropertyValue({prop_json}):'';\
         }})()"
    );
    match exec_in_frame(&snapshot, &script).await {
        Ok(RemoteValue::String(text)) if text == "__tb_stale" => {
            error(404, "stale element reference", "element not found")
        }
        Ok(RemoteValue::String(text)) => ok(json!(text)),
        Ok(_) => ok(json!("")),
        Err(err) => script_error(&err),
    }
}

// https://w3c.github.io/webdriver/#get-element-text
async fn element_text(sessions: &Mutex<Sessions>, session: &str, element: &str) -> (u16, Value) {
    let snapshot = match snapshot_checking_alert(sessions, session).await {
        Ok(snapshot) => snapshot,
        Err(reply) => return reply,
    };
    let remote = match resolve_element_remote(&snapshot, element) {
        Ok(remote) => remote,
        Err(reply) => return reply,
    };
    let script = format!(
        "(function(){{\
           const el=host.__tb_webdriver_element({remote});\
           if(el===null)return '__tb_stale';\
           return (el.innerText!==undefined?el.innerText:(el.textContent||'')).trim();\
         }})()"
    );
    match exec_in_frame(&snapshot, &script).await {
        Ok(RemoteValue::String(text)) if text == "__tb_stale" => {
            error(404, "stale element reference", "element not found")
        }
        Ok(RemoteValue::String(text)) => ok(json!(text)),
        Ok(_) => ok(json!("")),
        Err(err) => script_error(&err),
    }
}

// https://w3c.github.io/webdriver/#get-element-tag-name
async fn element_tag_name(
    sessions: &Mutex<Sessions>,
    session: &str,
    element: &str,
) -> (u16, Value) {
    let snapshot = match snapshot_checking_alert(sessions, session).await {
        Ok(snapshot) => snapshot,
        Err(reply) => return reply,
    };
    let remote = match resolve_element_remote(&snapshot, element) {
        Ok(remote) => remote,
        Err(reply) => return reply,
    };
    let script = format!(
        "(function(){{\
           const el=host.__tb_webdriver_element({remote});\
           if(el===null)return '__tb_stale';\
           return String(el.tagName||'').toLowerCase();\
         }})()"
    );
    match exec_in_frame(&snapshot, &script).await {
        Ok(RemoteValue::String(text)) if text == "__tb_stale" => {
            error(404, "stale element reference", "element not found")
        }
        Ok(RemoteValue::String(text)) => ok(json!(text)),
        Ok(_) => ok(json!("")),
        Err(err) => script_error(&err),
    }
}

// https://w3c.github.io/webdriver/#is-element-enabled
async fn element_enabled(sessions: &Mutex<Sessions>, session: &str, element: &str) -> (u16, Value) {
    let snapshot = match snapshot_checking_alert(sessions, session).await {
        Ok(snapshot) => snapshot,
        Err(reply) => return reply,
    };
    let remote = match resolve_element_remote(&snapshot, element) {
        Ok(remote) => remote,
        Err(reply) => return reply,
    };
    let script = format!(
        "(function(){{\
           const el=host.__tb_webdriver_element({remote});\
           if(el===null)return null;\
           const tag=el.tagName?el.tagName.toUpperCase():'';\
           if(['BUTTON','INPUT','SELECT','TEXTAREA','OPTGROUP','OPTION','FIELDSET'].includes(tag))return !el.disabled;\
           return true;\
         }})()"
    );
    match exec_in_frame(&snapshot, &script).await {
        Ok(RemoteValue::Bool(val)) => ok(json!(val)),
        Ok(_) => error(404, "stale element reference", "element not found"),
        Err(err) => script_error(&err),
    }
}

// https://w3c.github.io/webdriver/#get-computed-role
async fn element_computed_role(
    sessions: &Mutex<Sessions>,
    session: &str,
    element: &str,
) -> (u16, Value) {
    let snapshot = match snapshot_checking_alert(sessions, session).await {
        Ok(snapshot) => snapshot,
        Err(reply) => return reply,
    };
    let remote = match resolve_element_remote(&snapshot, element) {
        Ok(remote) => remote,
        Err(reply) => return reply,
    };
    let script = format!(
        "(function(){{\
           const el=host.__tb_webdriver_element({remote});\
           if(el===null)return '__tb_stale';\
           const explicit=el.getAttribute('role');\
           if(explicit)return explicit.trim().split(/\\s+/)[0];\
           const tag=(el.tagName||'').toLowerCase();\
           const map={{a:el.hasAttribute('href')?'link':'generic',button:'button',input:el.type==='checkbox'?'checkbox':el.type==='radio'?'radio':'textbox',textarea:'textbox',select:'combobox',img:'img',nav:'navigation',main:'main',header:'banner',footer:'contentinfo',aside:'complementary',h1:'heading',h2:'heading',h3:'heading',h4:'heading',h5:'heading',h6:'heading',ul:'list',ol:'list',li:'listitem',table:'table',tr:'row',td:'cell',th:'columnheader',form:'form',dialog:'dialog'}};\
           return map[tag]||'generic';\
         }})()"
    );
    match exec_in_frame(&snapshot, &script).await {
        Ok(RemoteValue::String(text)) if text == "__tb_stale" => {
            error(404, "stale element reference", "element not found")
        }
        Ok(RemoteValue::String(role)) => ok(json!(role)),
        Ok(_) => ok(json!("")),
        Err(err) => script_error(&err),
    }
}

// https://w3c.github.io/webdriver/#get-computed-label
async fn element_computed_label(
    sessions: &Mutex<Sessions>,
    session: &str,
    element: &str,
) -> (u16, Value) {
    let snapshot = match snapshot_checking_alert(sessions, session).await {
        Ok(snapshot) => snapshot,
        Err(reply) => return reply,
    };
    let remote = match resolve_element_remote(&snapshot, element) {
        Ok(remote) => remote,
        Err(reply) => return reply,
    };
    let script = format!(
        "(function(){{\
           const el=host.__tb_webdriver_element({remote});\
           if(el===null)return '__tb_stale';\
           const aria=el.getAttribute('aria-label');\
           if(aria)return aria;\
           const labelledby=el.getAttribute('aria-labelledby');\
           if(labelledby){{return labelledby.split(/\\s+/).map(id=>{{const t=document.getElementById(id);return t?(t.textContent||'').trim():'';}}).filter(Boolean).join(' ');}}\
           if(el.id){{const lbl=document.querySelector('label[for=\"'+CSS.escape(el.id)+'\"]');if(lbl)return (lbl.textContent||'').trim();}}\
           const alt=el.getAttribute('alt');\
           if(alt)return alt;\
           const title=el.getAttribute('title');\
           if(title)return title;\
           return (el.textContent||'').trim();\
         }})()"
    );
    match exec_in_frame(&snapshot, &script).await {
        Ok(RemoteValue::String(text)) if text == "__tb_stale" => {
            error(404, "stale element reference", "element not found")
        }
        Ok(RemoteValue::String(label)) => ok(json!(label)),
        Ok(_) => ok(json!("")),
        Err(err) => script_error(&err),
    }
}

// https://w3c.github.io/webdriver/#element-click
async fn element_click(sessions: &Mutex<Sessions>, session: &str, element: &str) -> (u16, Value) {
    let snapshot = match snapshot_checking_alert(sessions, session).await {
        Ok(snapshot) => snapshot,
        Err(reply) => return reply,
    };
    let remote = match resolve_element_remote(&snapshot, element) {
        Ok(remote) => remote,
        Err(reply) => return reply,
    };
    let script = format!(
        "(function(){{\
           const el=host.__tb_webdriver_element({remote});\
           if(el===null)return 'stale';\
           const tag=(el.tagName||'').toUpperCase();\
           if(tag==='INPUT'&&el.type==='file')return 'file';\
           if(tag==='OPTION'){{\
             const sel=el.closest('select');\
             if(sel&&!sel.disabled&&!el.disabled){{\
               const prev=el.selected;\
               el.selected=sel.multiple?!prev:true;\
               if(el.selected!==prev){{\
                 sel.dispatchEvent(new Event('input',{{bubbles:true}}));\
                 sel.dispatchEvent(new Event('change',{{bubbles:true}}));\
               }}\
             }}\
             return 'ok';\
           }}\
           return host.__tbWebDriverElementClick(el);\
         }})()"
    );
    let mut prompt_rx = snapshot.browser.prompt_broker().subscribe();
    let tab_id = snapshot.tab.id();
    let result = tokio::select! {
        res = exec_in_frame(&snapshot, &script) => Some(res),
        () = wait_for_prompt_opened(&mut prompt_rx, tab_id) => None,
    };
    let Some(result) = result else {
        return ok(Value::Null);
    };
    match result {
        Ok(RemoteValue::String(s)) if s == "ok" => ok(Value::Null),
        Ok(RemoteValue::String(s)) if s == "file" => {
            error(400, "invalid argument", "cannot click file input")
        }
        Ok(RemoteValue::String(s)) if s == "not_interactable" => error(
            400,
            "element not interactable",
            "element is not pointer-interactable",
        ),
        Ok(RemoteValue::String(s)) if s == "intercepted" => error(
            400,
            "element click intercepted",
            "another element would receive the click",
        ),
        Ok(_) => error(404, "stale element reference", "unknown element id"),
        Err(err) => script_error(&err),
    }
}

// https://w3c.github.io/webdriver/#element-clear
async fn element_clear(sessions: &Mutex<Sessions>, session: &str, element: &str) -> (u16, Value) {
    let snapshot = match snapshot_checking_alert(sessions, session).await {
        Ok(snapshot) => snapshot,
        Err(reply) => return reply,
    };
    let remote = match resolve_element_remote(&snapshot, element) {
        Ok(remote) => remote,
        Err(reply) => return reply,
    };
    let script = format!(
        "(function(){{\
           const el=host.__tb_webdriver_element({remote});\
           if(el===null)return 'stale';\
           const tag=(el.tagName||'').toUpperCase();\
           const cs=typeof getComputedStyle==='function'?getComputedStyle(el):null;\
           const rect=el.getBoundingClientRect?el.getBoundingClientRect():{{width:0,height:0}};\
           const hidden=(cs&&(cs.display==='none'||cs.visibility==='hidden'))||(rect.width===0&&rect.height===0);\
           if(hidden)return 'not_interactable';\
           if(tag==='INPUT'&&el.type==='file'){{el.files=host.__tbCreateFileList([]);return 'ok';}}\
           if((tag==='INPUT'||tag==='TEXTAREA')&&(el.disabled||el.readOnly))return 'invalid';\
           if(tag==='INPUT'||tag==='TEXTAREA'){{\
             const nonClearable=new Set(['button','checkbox','hidden','image','radio','reset','submit']);\
             if(tag==='INPUT'&&nonClearable.has((el.type||'text').toLowerCase()))return 'invalid';\
             const had=el.value!=='';\
             try{{el.focus();}}catch(_){{}}\
             el.value='';\
             if(had){{el.dispatchEvent(new Event('input',{{bubbles:true}}));el.dispatchEvent(new Event('change',{{bubbles:true}}));}}\
             try{{el.blur();}}catch(_){{}}\
             return 'ok';\
           }}\
           if(el.isContentEditable){{\
             try{{el.focus();}}catch(_){{}}\
             el.textContent='';\
             try{{el.blur();}}catch(_){{}}\
             return 'ok';\
           }}\
           return 'invalid';\
         }})()"
    );
    match exec_in_frame(&snapshot, &script).await {
        Ok(RemoteValue::String(s)) if s == "ok" => ok(Value::Null),
        Ok(RemoteValue::String(s)) if s == "not_interactable" => error(
            400,
            "element not interactable",
            "element is not interactable",
        ),
        Ok(RemoteValue::String(s)) if s == "invalid" => {
            error(400, "invalid element state", "element cannot be cleared")
        }
        Ok(_) => error(404, "stale element reference", "element not found"),
        Err(err) => script_error(&err),
    }
}

// https://w3c.github.io/webdriver/#element-send-keys
async fn element_send_keys(
    sessions: &Mutex<Sessions>,
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
        _ => return error(400, "invalid argument", "text must be a string"),
    };
    let snapshot = match snapshot_checking_alert(sessions, session).await {
        Ok(snapshot) => snapshot,
        Err(reply) => return reply,
    };
    let remote = match resolve_element_remote(&snapshot, element) {
        Ok(remote) => remote,
        Err(reply) => return reply,
    };
    let probe = format!(
        "(function(){{const el=host.__tb_webdriver_element({remote});\
         if(el===null)return 'stale';\
         if(el.tagName==='INPUT'&&el.type==='file')return el.multiple?'file-multiple':'file-single';\
         return 'text';}})()"
    );
    match exec_in_frame(&snapshot, &probe).await {
        Ok(RemoteValue::String(mode)) if mode == "file-single" => {
            return send_file_keys(&snapshot, remote, &text, false).await;
        }
        Ok(RemoteValue::String(mode)) if mode == "file-multiple" => {
            return send_file_keys(&snapshot, remote, &text, true).await;
        }
        Ok(RemoteValue::String(mode)) if mode == "text" => {}
        Ok(_) => return error(404, "stale element reference", "unknown element id"),
        Err(err) => return script_error(&err),
    }
    let script = format!(
        "(function(){{const el=host.__tb_webdriver_element({remote});\
         if(el===null)return 'stale';return host.__tbWebDriverSendKeys(el, {});}})()",
        json!(text)
    );
    let mut prompt_rx = snapshot.browser.prompt_broker().subscribe();
    let tab_id = snapshot.tab.id();
    let result = tokio::select! {
        res = exec_in_frame(&snapshot, &script) => Some(res),
        () = wait_for_prompt_opened(&mut prompt_rx, tab_id) => None,
    };
    let Some(result) = result else {
        return ok(Value::Null);
    };
    match result {
        Ok(RemoteValue::String(s)) if s == "ok" => ok(Value::Null),
        Ok(RemoteValue::String(s)) if s == "not_interactable" => error(
            400,
            "element not interactable",
            "element is not keyboard-interactable",
        ),
        Ok(_) => error(404, "stale element reference", "unknown element id"),
        Err(err) => script_error(&err),
    }
}

// https://w3c.github.io/webdriver/#element-send-keys
async fn send_file_keys(
    snapshot: &WindowSnapshot,
    remote: u64,
    text: &str,
    multiple: bool,
) -> (u16, Value) {
    if text.is_empty() {
        return error(400, "invalid argument", "file paths are empty");
    }
    let paths: Vec<String> = text.split('\n').map(str::to_owned).collect();
    if !multiple && paths.len() > 1 {
        return error(
            400,
            "invalid argument",
            "multiple files for a single-file input",
        );
    }
    let files = match crate::session::read_upload_files(paths).await {
        Ok(files) => files,
        Err(message) => return error(400, "invalid argument", &message),
    };
    let script = format!(
        "(function(){{const el=host.__tb_webdriver_element({remote});\
         if(el===null)return 'stale';\
         return host.__tbWebDriverUpload(el, {}, {}, {});}})()",
        serde_json::to_string(&Value::Array(files)).unwrap_or_else(|_| "[]".to_owned()),
        multiple,
        snapshot.strict_file_interactability,
    );
    match exec_in_frame(snapshot, &script).await {
        Ok(RemoteValue::String(s)) if s == "ok" => ok(Value::Null),
        Ok(RemoteValue::String(s)) if s == "not_interactable" => error(
            400,
            "element not interactable",
            "element is not keyboard-interactable",
        ),
        Ok(_) => error(404, "stale element reference", "unknown element id"),
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

// https://w3c.github.io/webdriver/#process-an-input-source-action-sequence
fn validate_actions(actions: &Value) -> Result<(), String> {
    let Some(sources) = actions.as_array() else {
        return Err("actions must be an array".into());
    };
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for source in sources {
        validate_action_source(source, &mut seen)?;
    }
    Ok(())
}

// https://w3c.github.io/webdriver/#dfn-process-an-input-source
fn validate_action_source(
    source: &Value,
    seen: &mut HashSet<(String, String)>,
) -> Result<(), String> {
    let Some(source) = source.as_object() else {
        return Err("input source must be an object".into());
    };
    let Some(kind) = source.get("type").and_then(Value::as_str) else {
        return Err("input source type is required".into());
    };
    if !matches!(kind, "none" | "key" | "pointer" | "wheel") {
        return Err(format!("unknown input source type: {kind}"));
    }
    let Some(id) = source.get("id").and_then(Value::as_str) else {
        return Err("input source id is required".into());
    };
    if !seen.insert((kind.to_owned(), id.to_owned())) {
        return Err(format!("duplicate input source id: {id}"));
    }
    if kind == "pointer"
        && let Some(parameters) = source.get("parameters")
    {
        let Some(parameters) = parameters.as_object() else {
            return Err("pointer parameters must be an object".into());
        };
        if let Some(pointer_type) = parameters.get("pointerType")
            && !matches!(pointer_type.as_str(), Some("mouse" | "pen" | "touch"))
        {
            return Err("unknown pointerType".into());
        }
    }
    let Some(items) = source.get("actions").and_then(Value::as_array) else {
        return Err("input source actions must be an array".into());
    };
    for item in items {
        let Some(item) = item.as_object() else {
            return Err("action must be an object".into());
        };
        let Some(action) = item.get("type").and_then(Value::as_str) else {
            return Err("action type is required".into());
        };
        validate_action(kind, action, item)?;
    }
    Ok(())
}

// https://w3c.github.io/webdriver/#dfn-process-an-input-source-action
fn validate_action(kind: &str, action: &str, item: &Map<String, Value>) -> Result<(), String> {
    if action == "pause" {
        return action_duration(item);
    }
    match kind {
        "none" => Err(format!("unknown null action type: {action}")),
        "key" => validate_key_action(action, item),
        "pointer" => validate_pointer_action(action, item),
        "wheel" => validate_wheel_action(action, item),
        _ => Err("unknown input source type".into()),
    }
}

// https://w3c.github.io/webdriver/#dfn-key-input-source
fn validate_key_action(action: &str, item: &Map<String, Value>) -> Result<(), String> {
    if !matches!(action, "keyDown" | "keyUp") {
        return Err(format!("unknown key action type: {action}"));
    }
    let Some(value) = item.get("value").and_then(Value::as_str) else {
        return Err("key action value must be a string".into());
    };
    if value.graphemes(true).count() != 1 {
        return Err("key action value must be one grapheme cluster".into());
    }
    Ok(())
}

// https://w3c.github.io/webdriver/#dfn-pointer-input-source
fn validate_pointer_action(action: &str, item: &Map<String, Value>) -> Result<(), String> {
    match action {
        "pointerDown" | "pointerUp" => {
            if !item
                .get("button")
                .is_some_and(|value| action_integer(value, 0.0, 4.0))
            {
                return Err("pointer button must be an integer between 0 and 4".into());
            }
            pointer_common_properties(item)
        }
        "pointerMove" => {
            for name in ["x", "y"] {
                if !item.get(name).is_some_and(action_number) {
                    return Err(format!("pointerMove {name} must be a number"));
                }
            }
            action_duration(item)?;
            action_origin(item, true)?;
            pointer_common_properties(item)
        }
        "pointerCancel" => pointer_common_properties(item),
        other => Err(format!("unknown pointer action type: {other}")),
    }
}

// https://w3c.github.io/webdriver/#dfn-wheel-input-source
fn validate_wheel_action(action: &str, item: &Map<String, Value>) -> Result<(), String> {
    if action != "scroll" {
        return Err(format!("unknown wheel action type: {action}"));
    }
    for name in ["x", "y", "deltaX", "deltaY"] {
        if !item
            .get(name)
            .is_some_and(|value| action_safe_integer(value, false))
        {
            return Err(format!("scroll {name} must be an integer"));
        }
    }
    action_duration(item)?;
    action_origin(item, false)
}

// https://w3c.github.io/webdriver/#dfn-actions
fn pointer_common_properties(item: &Map<String, Value>) -> Result<(), String> {
    for name in ["width", "height"] {
        if let Some(value) = item.get(name)
            && !value.as_f64().is_some_and(|n| n.is_finite() && n >= 0.0)
        {
            return Err(format!("{name} must be a non-negative number"));
        }
    }
    for name in [
        "pressure",
        "tangentialPressure",
        "altitudeAngle",
        "azimuthAngle",
    ] {
        if let Some(value) = item.get(name)
            && !action_number(value)
        {
            return Err(format!("{name} must be a number"));
        }
    }
    if let Some(value) = item.get("twist")
        && !action_integer(value, 0.0, 359.0)
    {
        return Err("twist must be an integer between 0 and 359".into());
    }
    for name in ["tiltX", "tiltY"] {
        if let Some(value) = item.get(name)
            && !action_integer(value, -90.0, 90.0)
        {
            return Err(format!("{name} must be an integer between -90 and 90"));
        }
    }
    Ok(())
}

fn action_safe_integer(value: &Value, non_negative: bool) -> bool {
    if let Some(n) = value.as_i64() {
        return crate::session::is_safe_i64(n) && (!non_negative || n >= 0);
    }
    if let Some(n) = value.as_u64() {
        return n <= crate::session::MAX_SAFE_INT;
    }
    value.as_f64().is_some_and(|n| {
        n.is_finite()
            && n.fract() == 0.0
            && (!non_negative || n >= 0.0)
            && format!("{n:.0}")
                .parse::<i64>()
                .is_ok_and(crate::session::is_safe_i64)
    })
}

fn action_integer(value: &Value, min: f64, max: f64) -> bool {
    value.as_f64().is_some_and(|number| {
        number.is_finite() && number.fract() == 0.0 && (min..=max).contains(&number)
    })
}

fn action_number(value: &Value) -> bool {
    value.as_f64().is_some_and(f64::is_finite)
}

fn action_duration(item: &Map<String, Value>) -> Result<(), String> {
    if let Some(value) = item.get("duration")
        && !action_safe_integer(value, true)
    {
        return Err("duration must be a non-negative integer".into());
    }
    Ok(())
}

fn action_origin(item: &Map<String, Value>, pointer: bool) -> Result<(), String> {
    let Some(value) = item.get("origin") else {
        return Ok(());
    };
    if let Some(raw) = value.as_str() {
        if raw == "viewport" || (pointer && raw == "pointer") {
            return Ok(());
        }
        return Err("unknown origin".into());
    }
    let Some(object) = value.as_object() else {
        return Err("origin must be a string or element".into());
    };
    match object.get(ELEMENT_KEY) {
        Some(Value::String(_)) => Ok(()),
        _ => Err("origin must be an element reference".into()),
    }
}

// https://w3c.github.io/webdriver/#perform-actions
async fn perform_actions(sessions: &Mutex<Sessions>, session: &str, body: &str) -> (u16, Value) {
    let parsed: Value = match serde_json::from_str(body) {
        Ok(value) => value,
        Err(err) => return error(400, "invalid argument", &err.to_string()),
    };
    let Some(sources) = parsed.get("actions").and_then(Value::as_array) else {
        return error(400, "invalid argument", "actions is required");
    };
    if let Err(message) = validate_actions(&Value::Array(sources.clone())) {
        return error(400, "invalid argument", &message);
    }
    let snapshot = match snapshot_checking_alert(sessions, session).await {
        Ok(snapshot) => snapshot,
        Err(reply) => return reply,
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
                let remote = match resolve_element_remote(&snapshot, element) {
                    Ok(remote) => remote,
                    Err(reply) => return reply,
                };
                *origin = json!({"__tbRemote": remote});
            }
        }
        resolved.push(source);
    }
    let script = format!(
        "(function(){{\n\
           const actions = {};\n\
           const vw = globalThis.innerWidth || 800;\n\
           const vh = globalThis.innerHeight || 600;\n\
           for (const source of actions) {{\n\
             if (!source || !Array.isArray(source.actions)) continue;\n\
             if (source.type === 'pointer') {{\n\
               const startPos = host.__tbWebDriverPointerPos(source.id);\n\
               let px = startPos[0], py = startPos[1];\n\
               for (const item of source.actions) {{\n\
                 if (!item || item.type !== 'pointerMove') continue;\n\
                 let tx, ty;\n\
                 if (item.origin && typeof item.origin === 'object' && typeof item.origin.__tbRemote === 'number') {{\n\
                   const el = host.__tb_webdriver_element(item.origin.__tbRemote);\n\
                   if (!el || !el.getBoundingClientRect) return '__tb_stale';\n\
                   const r = el.getBoundingClientRect();\n\
                   tx = r.left + r.width / 2 + (item.x || 0);\n\
                   ty = r.top + r.height / 2 + (item.y || 0);\n\
                 }} else if (item.origin === 'pointer') {{\n\
                   tx = px + (item.x || 0);\n\
                   ty = py + (item.y || 0);\n\
                 }} else {{\n\
                   tx = item.x || 0;\n\
                   ty = item.y || 0;\n\
                 }}\n\
                 if (tx < 0 || ty < 0 || tx > vw || ty > vh) return '__tb_oob';\n\
                 px = tx; py = ty;\n\
               }}\n\
             }}\n\
           }}\n\
           return host.__tbWebDriverActions(actions);\n\
         }})()",
        serde_json::to_string(&Value::Array(resolved)).unwrap_or_else(|_| "[]".to_owned())
    );
    match exec_in_frame(&snapshot, &script).await {
        Ok(RemoteValue::Bool(true)) => ok(Value::Null),
        Ok(RemoteValue::String(s)) if s == "__tb_stale" => {
            error(404, "stale element reference", "origin element is stale")
        }
        Ok(RemoteValue::String(s)) if s == "__tb_oob" => error(
            400,
            "move target out of bounds",
            "move target is outside viewport",
        ),
        Ok(_) => error(500, "unknown error", "actions were not performed"),
        Err(err) => {
            let msg = err.to_string();
            if msg.contains("stale element reference") {
                error(404, "stale element reference", &msg)
            } else if msg.contains("move target out of bounds") {
                error(400, "move target out of bounds", &msg)
            } else {
                error(400, "invalid argument", &msg)
            }
        }
    }
}

// https://w3c.github.io/webdriver/#release-actions
async fn release_actions(sessions: &Mutex<Sessions>, session: &str) -> (u16, Value) {
    let snapshot = match snapshot_checking_alert(sessions, session).await {
        Ok(snapshot) => snapshot,
        Err(reply) => return reply,
    };
    match exec_in_frame(&snapshot, "host.__tbWebDriverReleaseActions()").await {
        Ok(_) => ok(Value::Null),
        Err(err) => script_error(&err),
    }
}

// https://w3c.github.io/webdriver/#dismiss-alert
async fn dismiss_alert(sessions: &Mutex<Sessions>, session: &str) -> (u16, Value) {
    let snapshot = match snapshot_session(sessions, session).await {
        Ok(snapshot) => snapshot,
        Err(reply) => return reply,
    };
    if snapshot
        .browser
        .prompt_broker()
        .handle_prompt(snapshot.tab.id(), false, None)
    {
        let mut guard = sessions.lock().await;
        if let Some(found) = guard.open.get_mut(session) {
            found.prompt_text = None;
        }
        ok(Value::Null)
    } else {
        error(404, "no such alert", "no user prompt is open")
    }
}

// https://w3c.github.io/webdriver/#accept-alert
async fn accept_alert(sessions: &Mutex<Sessions>, session: &str) -> (u16, Value) {
    let snapshot = match snapshot_session(sessions, session).await {
        Ok(snapshot) => snapshot,
        Err(reply) => return reply,
    };
    let text = snapshot.prompt_text.clone();
    if snapshot
        .browser
        .prompt_broker()
        .handle_prompt(snapshot.tab.id(), true, text)
    {
        let mut guard = sessions.lock().await;
        if let Some(found) = guard.open.get_mut(session) {
            found.prompt_text = None;
        }
        ok(Value::Null)
    } else {
        error(404, "no such alert", "no user prompt is open")
    }
}

// https://w3c.github.io/webdriver/#get-alert-text
async fn get_alert_text(sessions: &Mutex<Sessions>, session: &str) -> (u16, Value) {
    let snapshot = match snapshot_session(sessions, session).await {
        Ok(snapshot) => snapshot,
        Err(reply) => return reply,
    };
    match snapshot
        .browser
        .prompt_broker()
        .active_prompt(snapshot.tab.id())
    {
        Some(info) => ok(json!(info.message)),
        None => error(404, "no such alert", "no user prompt is open"),
    }
}

// https://w3c.github.io/webdriver/#send-alert-text
async fn send_alert_text(sessions: &Mutex<Sessions>, session: &str, body: &str) -> (u16, Value) {
    let parsed: Value = match serde_json::from_str(body) {
        Ok(value) => value,
        Err(err) => return error(400, "invalid argument", &err.to_string()),
    };
    let Some(text) = parsed.get("text").and_then(Value::as_str) else {
        return error(400, "invalid argument", "text must be a string");
    };
    let snapshot = match snapshot_session(sessions, session).await {
        Ok(snapshot) => snapshot,
        Err(reply) => return reply,
    };
    let Some(info) = snapshot
        .browser
        .prompt_broker()
        .active_prompt(snapshot.tab.id())
    else {
        return error(404, "no such alert", "no user prompt is open");
    };
    if info.kind != PromptKind::Prompt {
        return error(
            400,
            "element not interactable",
            "user prompt does not accept text input",
        );
    }
    let mut guard = sessions.lock().await;
    if let Some(found) = guard.open.get_mut(session) {
        found.prompt_text = Some(text.to_owned());
    }
    ok(Value::Null)
}

async fn context_url(snapshot: &WindowSnapshot) -> Result<url::Url, (u16, Value)> {
    let spec = snapshot
        .tab
        .document_url()
        .await
        .map_err(|err| error(500, "unknown error", &err.to_string()))?;
    url::Url::parse(&spec).map_err(|err| error(500, "unknown error", &err.to_string()))
}

// https://w3c.github.io/webdriver/#cookie
fn cookie_json(record: &CookieRecord) -> Value {
    let mut map = Map::new();
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
async fn get_cookies(sessions: &Mutex<Sessions>, session: &str) -> (u16, Value) {
    let snapshot = match snapshot_checking_alert(sessions, session).await {
        Ok(snapshot) => snapshot,
        Err(reply) => return reply,
    };
    let url = match context_url(&snapshot).await {
        Ok(url) => url,
        Err(reply) => return reply,
    };
    match snapshot
        .browser
        .cookie_records_in(snapshot.tab.user_context(), &url)
        .await
    {
        Ok(records) => ok(json!(records.iter().map(cookie_json).collect::<Vec<_>>())),
        Err(err) => error(500, "unknown error", &err.to_string()),
    }
}

// https://w3c.github.io/webdriver/#get-named-cookie
async fn get_named_cookie(sessions: &Mutex<Sessions>, session: &str, name: &str) -> (u16, Value) {
    let snapshot = match snapshot_checking_alert(sessions, session).await {
        Ok(snapshot) => snapshot,
        Err(reply) => return reply,
    };
    let url = match context_url(&snapshot).await {
        Ok(url) => url,
        Err(reply) => return reply,
    };
    match snapshot
        .browser
        .cookie_records_in(snapshot.tab.user_context(), &url)
        .await
    {
        Ok(records) => match records.iter().find(|record| record.name == name) {
            Some(record) => ok(cookie_json(record)),
            None => error(404, "no such cookie", "no cookie with that name"),
        },
        Err(err) => error(500, "unknown error", &err.to_string()),
    }
}

fn has_cookie_ctl_or_semicolon(value: &str) -> bool {
    value.bytes().any(|b| b == b';' || b <= 0x1f || b == 0x7f)
}

fn validate_cookie_chars(cookie: &Value) -> Result<(), (u16, Value)> {
    let unable = |message| error(500, "unable to set cookie", message);
    if let Some(name) = cookie.get("name").and_then(Value::as_str)
        && (has_cookie_ctl_or_semicolon(name) || name.contains('='))
    {
        return Err(unable("cookie name contains invalid characters"));
    }
    for (field, label) in [
        ("value", "cookie value contains invalid characters"),
        ("path", "cookie path contains invalid characters"),
        ("domain", "cookie domain contains invalid characters"),
    ] {
        if let Some(val) = cookie.get(field).and_then(Value::as_str)
            && has_cookie_ctl_or_semicolon(val)
        {
            return Err(unable(label));
        }
    }
    Ok(())
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
        if expiry > crate::session::MAX_SAFE_INT {
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
async fn add_cookie(sessions: &Mutex<Sessions>, session: &str, body: &str) -> (u16, Value) {
    let snapshot = match snapshot_checking_alert(sessions, session).await {
        Ok(snapshot) => snapshot,
        Err(reply) => return reply,
    };
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
    let url = match context_url(&snapshot).await {
        Ok(url) => url,
        Err(reply) => return reply,
    };
    if !matches!(url.scheme(), "http" | "https") {
        return error(
            400,
            "invalid cookie domain",
            "document is not cookie-averse",
        );
    }
    if let Some(domain) = cookie.get("domain").and_then(Value::as_str) {
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
    if let Err(reply) = validate_cookie_chars(cookie) {
        return reply;
    }
    match snapshot
        .browser
        .add_cookie_in(snapshot.tab.user_context(), &line, &url)
        .await
    {
        Ok(true) => ok(Value::Null),
        Ok(false) => error(500, "unable to set cookie", "the cookie was rejected"),
        Err(err) => error(500, "unknown error", &err.to_string()),
    }
}

// https://w3c.github.io/webdriver/#delete-cookie
async fn delete_named_cookie(
    sessions: &Mutex<Sessions>,
    session: &str,
    name: &str,
) -> (u16, Value) {
    let snapshot = match snapshot_checking_alert(sessions, session).await {
        Ok(snapshot) => snapshot,
        Err(reply) => return reply,
    };
    let url = match context_url(&snapshot).await {
        Ok(url) => url,
        Err(reply) => return reply,
    };
    match snapshot
        .browser
        .delete_cookies_for_url_in(snapshot.tab.user_context(), &url, Some(name))
        .await
    {
        Ok(_) => ok(Value::Null),
        Err(err) => error(500, "unknown error", &err.to_string()),
    }
}

// https://w3c.github.io/webdriver/#delete-all-cookies
async fn delete_cookies(sessions: &Mutex<Sessions>, session: &str) -> (u16, Value) {
    let snapshot = match snapshot_checking_alert(sessions, session).await {
        Ok(snapshot) => snapshot,
        Err(reply) => return reply,
    };
    let url = match context_url(&snapshot).await {
        Ok(url) => url,
        Err(reply) => return reply,
    };
    match snapshot
        .browser
        .delete_cookies_for_url_in(snapshot.tab.user_context(), &url, None)
        .await
    {
        Ok(_) => ok(Value::Null),
        Err(err) => error(500, "unknown error", &err.to_string()),
    }
}

fn rect_number(value: f64) -> Value {
    if value.is_finite()
        && value.fract() == 0.0
        && (-2_147_483_648.0..=2_147_483_647.0).contains(&value)
        && let Ok(int) = format!("{value:.0}").parse::<i64>()
    {
        json!(int)
    } else {
        json!(value)
    }
}

fn session_rect(rect: [f64; 4]) -> Value {
    json!({
        "x": rect_number(rect[0]),
        "y": rect_number(rect[1]),
        "width": rect_number(rect[2]),
        "height": rect_number(rect[3]),
    })
}

// https://w3c.github.io/webdriver/#get-window-rect
async fn window_rect(sessions: &Mutex<Sessions>, session: &str) -> (u16, Value) {
    if let Err(reply) = snapshot_top_level_checking_alert(sessions, session).await {
        return reply;
    }
    let guard = sessions.lock().await;
    match guard.open.get(session) {
        Some(found) => ok(session_rect(found.window_rect)),
        None => error(404, "invalid session id", session),
    }
}

async fn window_op(sessions: &Mutex<Sessions>, session: &str, op: &str) -> (u16, Value) {
    let snapshot = match snapshot_checking_alert(sessions, session).await {
        Ok(snapshot) => snapshot,
        Err(reply) => return reply,
    };
    let rect = {
        let mut guard = sessions.lock().await;
        let Some(found) = guard.open.get_mut(session) else {
            return error(404, "invalid session id", session);
        };
        if matches!(op, "maximize" | "fullscreen") {
            found.window_rect = [0.0, 0.0, 1280.0, 720.0];
        }
        found.window_rect
    };
    if matches!(op, "maximize" | "fullscreen") {
        let _ = snapshot.tab.set_viewport(1280, 720).await;
    }
    ok(session_rect(rect))
}

// https://w3c.github.io/webdriver/#set-window-rect
async fn set_window_rect(sessions: &Mutex<Sessions>, session: &str, body: &str) -> (u16, Value) {
    let parsed: Value = match serde_json::from_str(body) {
        Ok(value) => value,
        Err(err) => return error(400, "invalid argument", &err.to_string()),
    };
    if !parsed.is_object() {
        return error(400, "invalid argument", "parameters must be an object");
    }
    let snapshot = match snapshot_checking_alert(sessions, session).await {
        Ok(snapshot) => snapshot,
        Err(reply) => return reply,
    };
    let updated = {
        let mut guard = sessions.lock().await;
        let Some(found) = guard.open.get_mut(session) else {
            return error(404, "invalid session id", session);
        };
        let mut updated = found.window_rect;
        for (key, index) in [("x", 0), ("y", 1), ("width", 2), ("height", 3)] {
            if let Some(value) = parsed.get(key) {
                if value.is_null() {
                    continue;
                }
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
        updated
    };
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "width and height are validated non-negative within i32 range and clamped to >= 1"
    )]
    let w = (updated[2] as u32).max(1);
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "width and height are validated non-negative within i32 range and clamped to >= 1"
    )]
    let h = (updated[3] as u32).max(1);
    let _ = snapshot.tab.set_viewport(w, h).await;
    ok(session_rect(updated))
}

// https://w3c.github.io/webdriver/#get-element-rect
async fn element_rect(sessions: &Mutex<Sessions>, session: &str, element: &str) -> (u16, Value) {
    let snapshot = match snapshot_checking_alert(sessions, session).await {
        Ok(snapshot) => snapshot,
        Err(reply) => return reply,
    };
    let remote = match resolve_element_remote(&snapshot, element) {
        Ok(remote) => remote,
        Err(reply) => return reply,
    };
    let script = format!(
        "(function(){{const el=host.__tb_webdriver_element({remote});\
         if(el===null)return null;return JSON.stringify(el.getBoundingClientRect());}})()"
    );
    match exec_in_frame(&snapshot, &script).await {
        Ok(RemoteValue::String(text)) => match serde_json::from_str::<Value>(&text) {
            Ok(rect) => ok(json!({
                "x": rect.get("x").cloned().unwrap_or(json!(0)),
                "y": rect.get("y").cloned().unwrap_or(json!(0)),
                "width": rect.get("width").cloned().unwrap_or(json!(0)),
                "height": rect.get("height").cloned().unwrap_or(json!(0)),
            })),
            Err(err) => error(500, "unknown error", &err.to_string()),
        },
        Ok(_) => error(404, "stale element reference", "unknown element id"),
        Err(err) => script_error(&err),
    }
}

fn get_timeouts(sessions: &Sessions, session: &str) -> (u16, Value) {
    match sessions.open.get(session) {
        Some(found) => ok(json!({
            "implicit": found.implicit_timeout.map(duration_millis),
            "pageLoad": found.page_load_timeout.map(duration_millis),
            "script": found.script_timeout.map(duration_millis),
        })),
        None => error(404, "invalid session id", session),
    }
}

fn set_timeouts(sessions: &mut Sessions, session: &str, body: &str) -> (u16, Value) {
    let parsed: Value = match serde_json::from_str(body) {
        Ok(value) => value,
        Err(err) => return error(400, "invalid argument", &err.to_string()),
    };
    let timeouts = match parse_timeouts_object(&parsed) {
        Ok(t) => t,
        Err(CapabilityError::InvalidArgument(msg) | CapabilityError::SessionNotCreated(msg)) => {
            return error(400, "invalid argument", &msg);
        }
    };
    let Some(found) = sessions.open.get_mut(session) else {
        return error(404, "invalid session id", session);
    };
    timeouts.implicit.apply(&mut found.implicit_timeout);
    timeouts.script.apply(&mut found.script_timeout);
    timeouts.page_load.apply(&mut found.page_load_timeout);
    ok(Value::Null)
}

fn current_window(sessions: &Sessions, session: &str) -> (u16, Value) {
    let Some(found) = sessions.open.get(session) else {
        return error(404, "invalid session id", session);
    };
    if !found.windows.contains_key(&found.current) {
        return error(404, "no such window", &found.current);
    }
    ok(json!(found.current))
}

fn window_handles(sessions: &Sessions, session: &str) -> (u16, Value) {
    match sessions.ordered_handles(session) {
        Some(handles) => ok(json!(handles)),
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
    found.current_frame = FrameId::MAIN;
    ok(Value::Null)
}

// https://w3c.github.io/webdriver/#new-window
async fn new_window(sessions: &Mutex<Sessions>, session: &str, body: &str) -> (u16, Value) {
    let parsed: Value = if body.trim().is_empty() {
        json!({})
    } else {
        match serde_json::from_str(body) {
            Ok(value) => value,
            Err(err) => return error(400, "invalid argument", &err.to_string()),
        }
    };
    let kind = match parsed.get("type") {
        Some(Value::String(s)) if s == "window" => "window",
        None | Some(Value::Null | Value::String(_)) => "tab",
        Some(_) => return error(400, "invalid argument", "type must be a string"),
    };
    if let Err(reply) = snapshot_top_level_checking_alert(sessions, session).await {
        return reply;
    }
    let browser = {
        let mut guard = sessions.lock().await;
        guard.sync_windows(session).await;
        if !guard.open.contains_key(session) {
            return error(404, "invalid session id", session);
        }
        guard.browser.clone()
    };
    let tab = match crate::session::blank_window(&browser, UserContextId::default_context()).await {
        Ok(tab) => tab,
        Err(err) => return error(500, "unknown error", &err),
    };
    let handle = tab_handle_id(tab.id());
    let mut guard = sessions.lock().await;
    guard.next_window = guard.next_window.saturating_add(1);
    let order = guard.next_window;
    let Some(found) = guard.open.get_mut(session) else {
        let tab_id = tab.id();
        drop(guard);
        let _ = browser.close_tab(tab_id).await;
        return error(404, "invalid session id", session);
    };
    found.windows.insert(handle.clone(), Window { tab, order });
    ok(json!({"handle": handle, "type": kind}))
}

// https://w3c.github.io/webdriver/#delete-session
async fn delete_session(sessions: &Mutex<Sessions>, session: &str) -> (u16, Value) {
    let (browser, tabs) = {
        let mut guard = sessions.lock().await;
        guard.sync_windows(session).await;
        let Some(found) = guard.open.remove(session) else {
            return ok(Value::Null);
        };
        let tabs: Vec<_> = found
            .windows
            .into_values()
            .map(|window| window.tab.id())
            .collect();
        (guard.browser.clone(), tabs)
    };
    browser.prompt_broker().set_default_behavior(None);
    browser.prompt_broker().set_hold_for_classic(false);
    let _ = browser.clear_cookies().await;
    for tab_id in tabs {
        let _result = browser.close_tab(tab_id).await;
    }
    ok(Value::Null)
}

// https://w3c.github.io/webdriver/#close-window
async fn close_window(sessions: &Mutex<Sessions>, session: &str) -> (u16, Value) {
    if let Err(reply) = snapshot_top_level_checking_alert(sessions, session).await {
        return reply;
    }
    let (browser, tab_id, remaining) = {
        let mut guard = sessions.lock().await;
        guard.sync_windows(session).await;
        let Some(found) = guard.open.get_mut(session) else {
            return error(404, "invalid session id", session);
        };
        let Some(window) = found.windows.remove(&found.current) else {
            return error(404, "no such window", &found.current);
        };
        let tab_id = window.tab.id();
        let remaining = guard.ordered_handles(session).unwrap_or_default();
        if remaining.is_empty() {
            guard.open.remove(session);
        }
        (guard.browser.clone(), tab_id, remaining)
    };
    if remaining.is_empty() {
        browser.prompt_broker().set_default_behavior(None);
        browser.prompt_broker().set_hold_for_classic(false);
        let _ = browser.clear_cookies().await;
    }
    let _result = browser.close_tab(tab_id).await;
    ok(json!(remaining))
}

fn method_not_allowed() -> (u16, Value) {
    error(405, "unknown method", "method not allowed")
}

pub(crate) fn ok(value: Value) -> (u16, Value) {
    (
        200,
        Value::Object(Map::from_iter([("value".into(), value)])),
    )
}

pub(crate) fn error(status: u16, err: &str, message: &str) -> (u16, Value) {
    (
        status,
        json!({"value": {"error": err, "message": message, "stacktrace": ""}}),
    )
}
