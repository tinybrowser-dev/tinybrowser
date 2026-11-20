//! CDP dispatch helpers: session methods, runtime script shapes, and replies.
//!
//! Split out of [`crate`] so the socket state machine and the method surface
//! have separate reasons to change.

use std::io;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use browser::{
    BrowserHandle, DEFAULT_VIEWPORT, DomNode, DomReply, DomRequest, FrameId, RemoteValue,
    ScreenshotRequest, TabError, TabEvent, TabHandle, TabId,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::params::CallArgument;

use crate::DEFAULT_BROWSER_CONTEXT_ID;

/// Largest capture or emulated dimension the renderer accepts.
const MAX_SCREENSHOT_DIM: f64 = 4096.0;

/// The target a request arrived on, with what that kind can address.
///
/// One value instead of a session enum flattened into a `(kind, tab)` pair:
/// a page target owns the tab it drives, and a browser target owns no tab at
/// all, so a handler that needs one cannot be handed a browser target.
#[derive(Clone)]
pub(crate) enum Target {
    /// A page target: the tab the request addresses.
    Page(TabHandle),
    /// The browser-wide target.
    Browser,
}

/// Which targets a method serves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TargetClass {
    Page,
    Browser,
    Both,
}

impl TargetClass {
    /// Whether a request on `target` may run a method of this class.
    ///
    /// The match is exhaustive over both closed enums so a new target kind
    /// must state which methods it serves instead of inheriting a bit it was
    /// never given.
    fn accepts(self, target: &Target) -> bool {
        match (self, target) {
            (Self::Both, Target::Page(_) | Target::Browser)
            | (Self::Page, Target::Page(_))
            | (Self::Browser, Target::Browser) => true,
            (Self::Page, Target::Browser) | (Self::Browser, Target::Page(_)) => false,
        }
    }

    /// Whether two classes can serve one request, which makes two routes for
    /// the same method ambiguous.
    #[cfg(test)]
    fn overlaps(self, other: Self) -> bool {
        match (self, other) {
            (Self::Both, Self::Both | Self::Page | Self::Browser)
            | (Self::Page | Self::Browser, Self::Both)
            | (Self::Page, Self::Page)
            | (Self::Browser, Self::Browser) => true,
            (Self::Page, Self::Browser) | (Self::Browser, Self::Page) => false,
        }
    }
}

/// What one protocol method does on one target kind.
#[derive(Clone, Copy)]
pub(crate) enum Handler {
    /// The requested effect is already the engine's fixed behavior.
    Accept(Accepted),
    ClearBrowserCookies,
    GetTargetInfo,
    Page(PageHandler),
    Browser(BrowserHandler),
}

/// Page-session handlers that need connection state.
#[derive(Clone, Copy)]
pub(crate) enum PageHandler {
    Enable,
    Disable,
    Navigate,
    NavigateToHistoryEntry,
    CaptureScreenshot,
    CreateIsolatedWorld,
    SetLifecycleEventsEnabled,
    RuntimeEvaluate,
    RuntimeCallFunctionOn,
    RuntimeEnable,
    RuntimeDisable,
    RuntimeReleaseObject,
    RuntimeReleaseObjectGroup,
    CssEnable,
    CssDisable,
    TracingEnd,
    Script(ScriptMethod),
}

/// Browser-session handlers.
#[derive(Clone, Copy)]
pub(crate) enum BrowserHandler {
    GetVersion,
    Close,
    SetAutoAttach,
    GetTargets,
    CreateTarget,
    CloseTarget,
    AttachToTarget,
    AttachToBrowserTarget,
    DetachFromTarget,
}

/// Page handlers that run one page-side helper and return its JSON reply.
#[derive(Clone, Copy)]
pub(crate) enum ScriptMethod {
    GetFrameTree,
    GetLayoutMetrics,
    GetResourceTree,
    Reload,
    AddScriptToEvaluateOnNewDocument,
    RemoveScriptToEvaluateOnNewDocument,
    GetNavigationHistory,
    GetProperties,
    SetDeviceMetricsOverride,
    ClearDeviceMetricsOverride,
    GetStorageKey,
    DomGetDocument,
    DomQuerySelector,
    DomQuerySelectorAll,
    DomDescribeNode,
    DomGetOuterHtml,
    DomGetAttributes,
    DomResolveNode,
    DomGetNodeForLocation,
    DomGetBoxModel,
    DomGetContentQuads,
    InputMouseEvent,
    InputKeyEvent,
    InputInsertText,
    InputTouchEvent,
    InputEmulateTouchFromMouse,
    CssGetComputedStyleForNode,
}

/// Why a command is an accepted no-op. The reason is data so the registry
/// states the justification instead of a comment beside a string list.
#[derive(Clone, Copy)]
pub(crate) enum Accepted {
    /// Nothing waits for a debugger, so there is no waiter to release.
    WaitingForDebugger,
    /// The domain emits no events, so its subscription toggles change nothing.
    DomainWithoutEvents,
    /// Focus is not engine state.
    FocusIsNotEngineState,
    /// A page target has no child targets.
    PageHasNoChildTargets,
    /// The renderer owns its font stack; there is no font configuration.
    RendererOwnsFonts,
    /// The engine has no download pipeline, so no download can start.
    NoDownloadPipeline,
    /// The engine has no HTTP cache, so clearing it is already true.
    NoHttpCache,
    /// The request selects the engine's existing media defaults.
    DefaultEmulatedMedia,
    /// The engine has no HTTP cache, so caching is already disabled.
    CacheAlreadyDisabled,
}

struct Route {
    method: &'static str,
    target: TargetClass,
    handler: Handler,
}

const ROUTES: &[Route] = &[
    Route {
        method: "Browser.getVersion",
        target: TargetClass::Browser,
        handler: Handler::Browser(BrowserHandler::GetVersion),
    },
    Route {
        method: "Browser.close",
        target: TargetClass::Browser,
        handler: Handler::Browser(BrowserHandler::Close),
    },
    Route {
        method: "Browser.setDownloadBehavior",
        target: TargetClass::Browser,
        handler: Handler::Accept(Accepted::NoDownloadPipeline),
    },
    Route {
        method: "Target.setAutoAttach",
        target: TargetClass::Browser,
        handler: Handler::Browser(BrowserHandler::SetAutoAttach),
    },
    Route {
        method: "Target.setAutoAttach",
        target: TargetClass::Page,
        handler: Handler::Accept(Accepted::PageHasNoChildTargets),
    },
    Route {
        method: "Target.getTargetInfo",
        target: TargetClass::Both,
        handler: Handler::GetTargetInfo,
    },
    Route {
        method: "Target.getTargets",
        target: TargetClass::Browser,
        handler: Handler::Browser(BrowserHandler::GetTargets),
    },
    Route {
        method: "Target.createTarget",
        target: TargetClass::Browser,
        handler: Handler::Browser(BrowserHandler::CreateTarget),
    },
    Route {
        method: "Target.closeTarget",
        target: TargetClass::Browser,
        handler: Handler::Browser(BrowserHandler::CloseTarget),
    },
    Route {
        method: "Target.attachToTarget",
        target: TargetClass::Browser,
        handler: Handler::Browser(BrowserHandler::AttachToTarget),
    },
    Route {
        method: "Target.attachToBrowserTarget",
        target: TargetClass::Browser,
        handler: Handler::Browser(BrowserHandler::AttachToBrowserTarget),
    },
    Route {
        method: "Target.detachFromTarget",
        target: TargetClass::Browser,
        handler: Handler::Browser(BrowserHandler::DetachFromTarget),
    },
    Route {
        method: "Network.clearBrowserCookies",
        target: TargetClass::Both,
        handler: Handler::ClearBrowserCookies,
    },
    Route {
        method: "Page.enable",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::Enable),
    },
    Route {
        method: "Page.disable",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::Disable),
    },
    Route {
        method: "Page.navigate",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::Navigate),
    },
    Route {
        method: "Page.navigateToHistoryEntry",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::NavigateToHistoryEntry),
    },
    Route {
        method: "Page.captureScreenshot",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::CaptureScreenshot),
    },
    Route {
        method: "Page.createIsolatedWorld",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::CreateIsolatedWorld),
    },
    Route {
        method: "Page.setLifecycleEventsEnabled",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::SetLifecycleEventsEnabled),
    },
    Route {
        method: "Page.getFrameTree",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::Script(ScriptMethod::GetFrameTree)),
    },
    Route {
        method: "Page.getLayoutMetrics",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::Script(ScriptMethod::GetLayoutMetrics)),
    },
    Route {
        method: "Page.getResourceTree",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::Script(ScriptMethod::GetResourceTree)),
    },
    Route {
        method: "Page.reload",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::Script(ScriptMethod::Reload)),
    },
    Route {
        method: "Page.addScriptToEvaluateOnNewDocument",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::Script(
            ScriptMethod::AddScriptToEvaluateOnNewDocument,
        )),
    },
    Route {
        method: "Page.removeScriptToEvaluateOnNewDocument",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::Script(
            ScriptMethod::RemoveScriptToEvaluateOnNewDocument,
        )),
    },
    Route {
        method: "Page.getNavigationHistory",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::Script(ScriptMethod::GetNavigationHistory)),
    },
    Route {
        method: "Page.setFontFamilies",
        target: TargetClass::Page,
        handler: Handler::Accept(Accepted::RendererOwnsFonts),
    },
    Route {
        method: "Runtime.enable",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::RuntimeEnable),
    },
    Route {
        method: "Runtime.disable",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::RuntimeDisable),
    },
    Route {
        method: "Runtime.evaluate",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::RuntimeEvaluate),
    },
    Route {
        method: "Runtime.callFunctionOn",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::RuntimeCallFunctionOn),
    },
    Route {
        method: "Runtime.getProperties",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::Script(ScriptMethod::GetProperties)),
    },
    Route {
        method: "Runtime.releaseObject",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::RuntimeReleaseObject),
    },
    Route {
        method: "Runtime.releaseObjectGroup",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::RuntimeReleaseObjectGroup),
    },
    Route {
        method: "Runtime.runIfWaitingForDebugger",
        target: TargetClass::Page,
        handler: Handler::Accept(Accepted::WaitingForDebugger),
    },
    Route {
        method: "DOM.getDocument",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::Script(ScriptMethod::DomGetDocument)),
    },
    Route {
        method: "DOM.querySelector",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::Script(ScriptMethod::DomQuerySelector)),
    },
    Route {
        method: "DOM.querySelectorAll",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::Script(ScriptMethod::DomQuerySelectorAll)),
    },
    Route {
        method: "DOM.describeNode",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::Script(ScriptMethod::DomDescribeNode)),
    },
    Route {
        method: "DOM.getOuterHTML",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::Script(ScriptMethod::DomGetOuterHtml)),
    },
    Route {
        method: "DOM.getAttributes",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::Script(ScriptMethod::DomGetAttributes)),
    },
    Route {
        method: "DOM.resolveNode",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::Script(ScriptMethod::DomResolveNode)),
    },
    Route {
        method: "DOM.getNodeForLocation",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::Script(ScriptMethod::DomGetNodeForLocation)),
    },
    Route {
        method: "DOM.getBoxModel",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::Script(ScriptMethod::DomGetBoxModel)),
    },
    Route {
        method: "DOM.getContentQuads",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::Script(ScriptMethod::DomGetContentQuads)),
    },
    Route {
        method: "Input.dispatchMouseEvent",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::Script(ScriptMethod::InputMouseEvent)),
    },
    Route {
        method: "Input.dispatchKeyEvent",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::Script(ScriptMethod::InputKeyEvent)),
    },
    Route {
        method: "Input.insertText",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::Script(ScriptMethod::InputInsertText)),
    },
    Route {
        method: "Input.dispatchTouchEvent",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::Script(ScriptMethod::InputTouchEvent)),
    },
    Route {
        method: "Input.emulateTouchFromMouseEvent",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::Script(
            ScriptMethod::InputEmulateTouchFromMouse,
        )),
    },
    Route {
        method: "CSS.enable",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::CssEnable),
    },
    Route {
        method: "CSS.disable",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::CssDisable),
    },
    Route {
        method: "CSS.getComputedStyleForNode",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::Script(
            ScriptMethod::CssGetComputedStyleForNode,
        )),
    },
    Route {
        method: "Tracing.end",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::TracingEnd),
    },
    Route {
        method: "Storage.getStorageKey",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::Script(ScriptMethod::GetStorageKey)),
    },
    Route {
        method: "Log.enable",
        target: TargetClass::Both,
        handler: Handler::Accept(Accepted::DomainWithoutEvents),
    },
    Route {
        method: "Network.enable",
        target: TargetClass::Both,
        handler: Handler::Accept(Accepted::DomainWithoutEvents),
    },
    Route {
        method: "Network.clearBrowserCache",
        target: TargetClass::Both,
        handler: Handler::Accept(Accepted::NoHttpCache),
    },
    Route {
        method: "Network.setCacheDisabled",
        target: TargetClass::Both,
        handler: Handler::Accept(Accepted::CacheAlreadyDisabled),
    },
    Route {
        method: "Emulation.setFocusEmulationEnabled",
        target: TargetClass::Both,
        handler: Handler::Accept(Accepted::FocusIsNotEngineState),
    },
    Route {
        method: "Emulation.setEmulatedMedia",
        target: TargetClass::Page,
        handler: Handler::Accept(Accepted::DefaultEmulatedMedia),
    },
    Route {
        method: "Emulation.setDeviceMetricsOverride",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::Script(ScriptMethod::SetDeviceMetricsOverride)),
    },
    Route {
        method: "Emulation.clearDeviceMetricsOverride",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::Script(
            ScriptMethod::ClearDeviceMetricsOverride,
        )),
    },
    Route {
        method: "Overlay.enable",
        target: TargetClass::Both,
        handler: Handler::Accept(Accepted::DomainWithoutEvents),
    },
    Route {
        method: "Overlay.disable",
        target: TargetClass::Both,
        handler: Handler::Accept(Accepted::DomainWithoutEvents),
    },
];

/// Looks up the handler for `method` on `kind`. An unregistered pair is
/// `MethodNotFound`; there is no fallback table.
pub(crate) fn route(method: &str, target: &Target) -> Option<Handler> {
    ROUTES
        .iter()
        .find(|route| route.method == method && route.target.accepts(target))
        .map(|route| route.handler)
}

/// Answers an accepted no-op. Conditional entries prove their premise from the
/// params and otherwise report `MethodNotFound`, exactly like a real handler
/// that cannot honor the request. A malformed request is still an
/// invalid-params error, never a "method not found".
pub(crate) fn accept(reason: Accepted, params: &Value) -> Result<Value, DispatchError> {
    if premise_holds(reason, params)? {
        Ok(json!({}))
    } else {
        Err(DispatchError::MethodNotFound)
    }
}

/// Whether an accepted no-op applies. Conditional variants prove their premise
/// here, once; the rest hold unconditionally.
fn premise_holds(reason: Accepted, params: &Value) -> Result<bool, DispatchError> {
    match reason {
        Accepted::DefaultEmulatedMedia => {
            let params: crate::params::SetEmulatedMedia = crate::params::parse(params)?;
            Ok(emulated_media_is_default(&params))
        }
        Accepted::CacheAlreadyDisabled => {
            let params: crate::params::SetCacheDisabled = crate::params::parse(params)?;
            Ok(params.cache_disabled)
        }
        Accepted::WaitingForDebugger
        | Accepted::DomainWithoutEvents
        | Accepted::FocusIsNotEngineState
        | Accepted::PageHasNoChildTargets
        | Accepted::RendererOwnsFonts
        | Accepted::NoDownloadPipeline
        | Accepted::NoHttpCache => Ok(true),
    }
}

/// Runs one stateless page handler and returns its protocol reply.
pub(crate) async fn run_script(
    method: ScriptMethod,
    params: &Value,
    tab: &TabHandle,
) -> Result<Value, DispatchError> {
    match method {
        ScriptMethod::GetFrameTree | ScriptMethod::GetResourceTree => frame_tree(tab).await,
        ScriptMethod::GetLayoutMetrics => layout_metrics(tab).await,
        ScriptMethod::Reload => {
            let url = tab
                .document_url()
                .await
                .map_err(|error| DispatchError::Failed(error.to_string()))?;
            open_url(tab, &url).await?;
            Ok(json!({}))
        }
        ScriptMethod::AddScriptToEvaluateOnNewDocument => add_init_script(params, tab).await,
        ScriptMethod::RemoveScriptToEvaluateOnNewDocument => remove_init_script(params, tab).await,
        ScriptMethod::GetNavigationHistory => navigation_history(tab).await,
        ScriptMethod::GetProperties => runtime_get_properties(tab, params).await,
        ScriptMethod::SetDeviceMetricsOverride => set_device_metrics(params, tab).await,
        ScriptMethod::ClearDeviceMetricsOverride => clear_device_metrics(tab).await,
        ScriptMethod::GetStorageKey => storage_key(tab).await,
        ScriptMethod::DomGetDocument => dom_get_document(tab).await,
        ScriptMethod::DomQuerySelector => dom_query_selector(tab, params).await,
        ScriptMethod::DomQuerySelectorAll => dom_query_selector_all(tab, params).await,
        ScriptMethod::DomDescribeNode => dom_describe_node(tab, params).await,
        ScriptMethod::DomGetOuterHtml => dom_outer_html(tab, params).await,
        ScriptMethod::DomGetAttributes => dom_attributes(tab, params).await,
        ScriptMethod::DomResolveNode => dom_resolve_node(tab, params).await,
        ScriptMethod::DomGetNodeForLocation => dom_node_for_location(tab, params).await,
        ScriptMethod::DomGetBoxModel => dom_box_model(tab, params).await,
        ScriptMethod::DomGetContentQuads => dom_content_quads(tab, params).await,
        ScriptMethod::InputMouseEvent => input_mouse_event(tab, params).await,
        ScriptMethod::InputKeyEvent => input_key_event(tab, params).await,
        ScriptMethod::InputInsertText => input_insert_text(tab, params).await,
        ScriptMethod::InputTouchEvent => input_touch_event(tab, params).await,
        ScriptMethod::InputEmulateTouchFromMouse => {
            input_emulate_touch_from_mouse(tab, params).await
        }
        ScriptMethod::CssGetComputedStyleForNode => css_computed_style(tab, params).await,
    }
}

async fn frame_tree(tab: &TabHandle) -> Result<Value, DispatchError> {
    let url = tab
        .document_url()
        .await
        .map_err(|error| DispatchError::Failed(error.to_string()))?;
    Ok(json!({"frameTree": {"frame": {
        "id": tab.id().to_string(),
        "loaderId": "",
        "url": url,
        "mimeType": "text/html",
    }}}))
}

async fn layout_metrics(tab: &TabHandle) -> Result<Value, DispatchError> {
    let (width, height) = tab
        .viewport_size()
        .await
        .map_err(|error| DispatchError::Failed(error.to_string()))?;
    let (content_width, content_height) = tab
        .content_size()
        .await
        .map_err(|error| DispatchError::Failed(error.to_string()))?;
    let width = f64::from(width);
    let height = f64::from(height);
    let content_width = f64::from(content_width);
    let content_height = f64::from(content_height);
    Ok(json!({
        "layoutViewport": viewport_rect(width, height),
        "visualViewport": {
            "offsetX": 0,
            "offsetY": 0,
            "pageX": 0,
            "pageY": 0,
            "clientWidth": width,
            "clientHeight": height,
            "scale": 1,
        },
        "contentSize": {"x": 0, "y": 0, "width": content_width, "height": content_height},
        "cssLayoutViewport": viewport_rect(width, height),
        "cssVisualViewport": {
            "offsetX": 0,
            "offsetY": 0,
            "pageX": 0,
            "pageY": 0,
            "clientWidth": width,
            "clientHeight": height,
            "scale": 1,
        },
        "cssContentSize": {"x": 0, "y": 0, "width": content_width, "height": content_height},
    }))
}

/// `Page.getNavigationHistory`: the tab's committed session-history entries in
/// traversal order and the active index.
/// <https://chromedevtools.github.io/devtools-protocol/tot/Page/#method-getNavigationHistory>
async fn navigation_history(tab: &TabHandle) -> Result<Value, DispatchError> {
    let history = tab
        .history()
        .await
        .map_err(|error| DispatchError::Failed(error.to_string()))?;
    let entries: Vec<Value> = history
        .entries()
        .iter()
        .map(|entry| {
            json!({
                "id": entry.id().get(),
                "url": entry.url().as_str(),
                "userTypedURL": entry.url().as_str(),
                "title": entry.title(),
                "transitionType": "other",
            })
        })
        .collect();
    Ok(json!({"currentIndex": history.current_index(), "entries": entries}))
}

fn get_properties_script(params: &crate::params::GetProperties) -> String {
    // The page helper reads booleans and an object-group string. The protocol
    // has no `objectGroup` here, so property handles stay out of a group; the
    // absent optional flags take their documented `false`.
    let arguments = json!({
        "objectId": params.object_id,
        "objectGroup": "",
        "ownProperties": params.own_properties.unwrap_or(false),
        "nonIndexedPropertiesOnly": params.non_indexed_properties_only.unwrap_or(false),
        "accessorPropertiesOnly": params.accessor_properties_only.unwrap_or(false),
        "generatePreview": params.generate_preview.unwrap_or(false),
    });
    RUNTIME_GET_PROPERTIES.replace("__ARGS__", &arguments.to_string())
}

/// `Runtime.getProperties`: own and inherited properties of a page-side handle,
/// plus its `[[Prototype]]` internal property. Accessors are reported as
/// `get`/`set` pairs; getters never run. `privateProperties` is absent because
/// JavaScript has no reflection API for private names.
/// <https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#method-getProperties>
async fn runtime_get_properties(tab: &TabHandle, params: &Value) -> Result<Value, DispatchError> {
    let params: crate::params::GetProperties = crate::params::parse(params)?;
    if params.object_id.is_empty() {
        return Err(DispatchError::InvalidParams("objectId".to_owned()));
    }
    let reply = execute_json(tab, &get_properties_script(&params)).await?;
    match reply.get("ok").and_then(Value::as_bool) {
        Some(true) => Ok(reply.get("value").cloned().unwrap_or(Value::Null)),
        Some(false) => Err(DispatchError::Failed(
            reply
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("Runtime.getProperties failed")
                .to_owned(),
        )),
        None => Err(DispatchError::Failed(
            "Runtime.getProperties returned a malformed reply".into(),
        )),
    }
}

/// `Runtime.releaseObject`: forget one page-side handle.
pub(crate) async fn runtime_release_object(
    tab: &TabHandle,
    params: &Value,
) -> Result<Value, DispatchError> {
    let params: crate::params::ReleaseObject = crate::params::parse(params)?;
    let script = RUNTIME_RELEASE.replace(
        "__ARGS__",
        &json!({"objectId": params.object_id}).to_string(),
    );
    tab.execute_browser_script(&script)
        .await
        .map_err(|error| DispatchError::Failed(error.to_string()))?;
    Ok(json!({}))
}

/// `Runtime.releaseObjectGroup`: forget every handle in one object group.
pub(crate) async fn runtime_release_object_group(
    tab: &TabHandle,
    params: &Value,
) -> Result<Value, DispatchError> {
    let params: crate::params::ReleaseObjectGroup = crate::params::parse(params)?;
    let script = RUNTIME_RELEASE.replace(
        "__ARGS__",
        &json!({"objectGroup": params.object_group}).to_string(),
    );
    tab.execute_browser_script(&script)
        .await
        .map_err(|error| DispatchError::Failed(error.to_string()))?;
    Ok(json!({}))
}

/// Runs a browser helper that returns a JSON string and parses it.
async fn execute_json(tab: &TabHandle, script: &str) -> Result<Value, DispatchError> {
    let value = tab
        .execute_browser_script(script)
        .await
        .map_err(|error| DispatchError::Failed(error.to_string()))?;
    let RemoteValue::String(text) = value else {
        return Err(DispatchError::BadHelperReply("browser"));
    };
    serde_json::from_str(&text).map_err(|error| DispatchError::Failed(error.to_string()))
}

/// True when `Emulation.setEmulatedMedia` selects the engine defaults: empty
/// media and every feature value empty.
fn emulated_media_is_default(params: &crate::params::SetEmulatedMedia) -> bool {
    if params
        .media
        .as_deref()
        .is_some_and(|media| !media.is_empty())
    {
        return false;
    }
    match &params.features {
        None => true,
        Some(features) => features.iter().all(|feature| feature.value.is_empty()),
    }
}

/// `Page.addScriptToEvaluateOnNewDocument`: register `source` page-wide and
/// return its identifier.
///
/// `worldName` is accepted but ignored: this engine has no isolated worlds,
/// so scripts (including Playwright's utility-world registration, whose
/// source is empty) run in the main world. That is a known isolation gap
/// (docs/progress.md).
async fn add_init_script(params: &Value, tab: &TabHandle) -> Result<Value, DispatchError> {
    let params: crate::params::AddInitScript = crate::params::parse(params)?;
    let id = tab
        .add_init_script(params.source, params.run_immediately.unwrap_or(false))
        .await
        .map_err(|error| DispatchError::Failed(error.to_string()))?;
    Ok(json!({"identifier": id.to_string()}))
}

/// `Page.removeScriptToEvaluateOnNewDocument`: forget a registered script.
async fn remove_init_script(params: &Value, tab: &TabHandle) -> Result<Value, DispatchError> {
    let params: crate::params::RemoveInitScript = crate::params::parse(params)?;
    let id = params
        .identifier
        .parse::<u64>()
        .map_err(|_| DispatchError::InvalidParams("identifier".to_owned()))?;
    tab.remove_init_script(id)
        .await
        .map_err(|error| DispatchError::Failed(error.to_string()))?;
    Ok(json!({}))
}

/// `Emulation.setDeviceMetricsOverride`: persist the emulated viewport on the
/// tab and the renderer. The protocol requires integer `width`/`height`, and
/// zero on either side clears the override; `mobile`, non-default
/// `deviceScaleFactor`, and sizes beyond the renderer's cap are refused
/// rather than silently approximated
/// (<https://chromedevtools.github.io/devtools-protocol/tot/Emulation/#method-setDeviceMetricsOverride>).
async fn set_device_metrics(params: &Value, tab: &TabHandle) -> Result<Value, DispatchError> {
    let params: crate::params::SetDeviceMetrics = crate::params::parse(params)?;
    let width = narrow_metric(params.width, "width")?;
    let height = narrow_metric(params.height, "height")?;
    if width == 0 || height == 0 {
        return clear_device_metrics(tab).await;
    }
    if params.mobile == Some(true) {
        return Err(DispatchError::Failed(
            "mobile emulation is not supported".into(),
        ));
    }
    if params
        .device_scale_factor
        .is_some_and(|scale| (scale - 1.0).abs() > f64::EPSILON)
    {
        return Err(DispatchError::Failed(
            "device scale factors other than 1 are not supported".into(),
        ));
    }
    if f64::from(width) > MAX_SCREENSHOT_DIM || f64::from(height) > MAX_SCREENSHOT_DIM {
        return Err(DispatchError::Failed(format!(
            "viewport exceeds the {MAX_SCREENSHOT_DIM} pixel cap"
        )));
    }
    set_viewport(tab, width, height).await
}

/// `Emulation.clearDeviceMetricsOverride`: restore the default viewport.
async fn clear_device_metrics(tab: &TabHandle) -> Result<Value, DispatchError> {
    set_viewport(tab, DEFAULT_VIEWPORT.0, DEFAULT_VIEWPORT.1).await
}

/// Applies one viewport size to the tab.
async fn set_viewport(tab: &TabHandle, width: u32, height: u32) -> Result<Value, DispatchError> {
    tab.set_viewport(width, height)
        .await
        .map_err(|error| DispatchError::Failed(error.to_string()))?;
    Ok(json!({}))
}

/// One emulation metric as the renderer's `u32` viewport dimension. The
/// protocol's integer decoding happened in [`crate::params::SetDeviceMetrics`];
/// this only narrows a value that does not fit.
fn narrow_metric(value: u64, name: &str) -> Result<u32, DispatchError> {
    u32::try_from(value).map_err(|_| {
        DispatchError::Failed(format!(
            "Emulation.setDeviceMetricsOverride {name} does not fit 32 bits"
        ))
    })
}

/// Capture surface side from a clip extent: at least 1, at most the renderer
/// cap, so a bad client value cannot ask for an unpaintable surface.
fn capture_side(extent: f64) -> u32 {
    if !extent.is_finite() || extent < 1.0 {
        return 1;
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "extent is finite, positive, and capped at MAX_SCREENSHOT_DIM"
    )]
    return extent.min(MAX_SCREENSHOT_DIM) as u32;
}

/// `Storage.getStorageKey`: the tab's origin.
async fn storage_key(tab: &TabHandle) -> Result<Value, DispatchError> {
    let value = tab
        .execute_browser_script("String(location.origin)")
        .await
        .map_err(|error| DispatchError::Failed(error.to_string()))?;
    let RemoteValue::String(key) = value else {
        return Err(DispatchError::BadHelperReply("Storage.getStorageKey"));
    };
    Ok(json!({"storageKey": key}))
}

/// `DOM.getDocument`: serialize the live tree from the renderer's typed node
/// map.
pub(crate) async fn dom_get_document(tab: &TabHandle) -> Result<Value, DispatchError> {
    let reply = dom_request(tab, DomRequest::GetDocument).await?;
    let DomReply::Document { root } = reply else {
        return Err(DispatchError::BadHelperReply("DOM.getDocument"));
    };
    Ok(json!({"root": document_node(&root)}))
}

/// `Input.dispatchMouseEvent`: one pointer step through the page-side
/// performer, preceded by a move so press/release land on the right target.
pub(crate) async fn input_mouse_event(
    tab: &TabHandle,
    params: &Value,
) -> Result<Value, DispatchError> {
    let params: crate::params::MouseEvent = crate::params::parse(params)?;
    let kind = params.kind.as_str();
    let button = match params.button.as_deref().unwrap_or("left") {
        "middle" => 1,
        "right" => 2,
        "back" => 3,
        "forward" => 4,
        _ => 0,
    };
    if kind == "mouseWheel" {
        let actions = json!({"actions": [{"type": "wheel", "id": "wheel", "actions": [{
            "type": "scroll", "x": params.x, "y": params.y,
            "deltaX": params.delta_x.unwrap_or(0.0),
            "deltaY": params.delta_y.unwrap_or(0.0),
        }]}]});
        return run_actions(tab, &actions).await;
    }
    let mut items =
        vec![json!({"type": "pointerMove", "origin": "viewport", "x": params.x, "y": params.y})];
    match kind {
        "mousePressed" => items.push(json!({"type": "pointerDown", "button": button})),
        "mouseReleased" => items.push(json!({"type": "pointerUp", "button": button})),
        _ => {}
    }
    let actions = json!({"actions": [{
        "type": "pointer", "id": "mouse", "parameters": {"pointerType": "mouse"},
        "actions": items,
    }]});
    run_actions(tab, &actions).await
}

/// `Input.dispatchKeyEvent`: one key step through the page-side performer.
/// `char` events carry their text in `text`.
pub(crate) async fn input_key_event(
    tab: &TabHandle,
    params: &Value,
) -> Result<Value, DispatchError> {
    let params: crate::params::KeyEvent = crate::params::parse(params)?;
    let kind = params.kind.as_str();
    let key = params.key.as_deref().unwrap_or("");
    let text = params.text.as_deref().unwrap_or("");
    let value = if kind == "char" { text } else { key };
    if value.is_empty() {
        return Ok(json!({}));
    }
    let down = kind != "keyUp";
    let actions = json!({"actions": [{"type": "key", "id": "keyboard", "actions": [{
        "type": if down { "keyDown" } else { "keyUp" },
        "value": value,
    }]}]});
    run_actions(tab, &actions).await
}

/// `Input.insertText`: type into the focused element.
pub(crate) async fn input_insert_text(
    tab: &TabHandle,
    params: &Value,
) -> Result<Value, DispatchError> {
    let params: crate::params::InsertText = crate::params::parse(params)?;
    if params.text.is_empty() {
        return Ok(json!({}));
    }
    let actions = json!({"actions": [{"type": "key", "id": "keyboard", "actions": [
        {"type": "insertText", "value": params.text},
    ]}]});
    run_actions(tab, &actions).await
}

/// `Input.dispatchTouchEvent`: the first touch point as a touch pointer.
pub(crate) async fn input_touch_event(
    tab: &TabHandle,
    params: &Value,
) -> Result<Value, DispatchError> {
    let params: crate::params::TouchEvent = crate::params::parse(params)?;
    let kind = params.kind.as_str();
    let Some(point) = params.touch_points.first() else {
        return Ok(json!({}));
    };
    let mut items =
        vec![json!({"type": "pointerMove", "origin": "viewport", "x": point.x, "y": point.y})];
    match kind {
        "touchStart" => items.push(json!({"type": "pointerDown", "button": 0})),
        "touchEnd" => items.push(json!({"type": "pointerUp", "button": 0})),
        "touchCancel" => items.push(json!({"type": "pointerCancel"})),
        _ => {}
    }
    let actions = json!({"actions": [{
        "type": "pointer", "id": "touch", "parameters": {"pointerType": "touch"},
        "actions": items,
    }]});
    run_actions(tab, &actions).await
}

/// `Input.emulateTouchFromMouseEvent`: the mouse shape, touch-typed.
pub(crate) async fn input_emulate_touch_from_mouse(
    tab: &TabHandle,
    params: &Value,
) -> Result<Value, DispatchError> {
    let params: crate::params::EmulateTouchFromMouse = crate::params::parse(params)?;
    let kind = params.kind.as_str();
    let mut items =
        vec![json!({"type": "pointerMove", "origin": "viewport", "x": params.x, "y": params.y})];
    match kind {
        "mousePressed" => items.push(json!({"type": "pointerDown", "button": 0})),
        "mouseReleased" => items.push(json!({"type": "pointerUp", "button": 0})),
        _ => {}
    }
    let actions = json!({"actions": [{
        "type": "pointer", "id": "touch", "parameters": {"pointerType": "touch"},
        "actions": items,
    }]});
    run_actions(tab, &actions).await
}

/// Runs a page-side action sequence and reports the CDP-shaped reply.
async fn run_actions(tab: &TabHandle, actions: &Value) -> Result<Value, DispatchError> {
    // The page performer takes the source array; callers may pass either the
    // array itself or a `{"actions": [...]}` envelope.
    let sources = actions.get("actions").unwrap_or(actions);
    let script = format!(
        "(function(){{return host.__tbWebDriverActions({});}})()",
        serde_json::to_string(sources).unwrap_or_else(|_| "[]".to_owned())
    );
    tab.execute_browser_script(&script)
        .await
        .map_err(|error| DispatchError::Failed(error.to_string()))?;
    Ok(json!({}))
}

/// Sends one typed DOM request to the tab's main frame.
async fn dom_request(tab: &TabHandle, request: DomRequest) -> Result<DomReply, DispatchError> {
    tab.dom(request)
        .await
        .map_err(|error| DispatchError::Failed(error.to_string()))
}

/// The node a DOM method targets, from any of `objectId` (a runtime handle),
/// `backendNodeId`, or `nodeId`.
enum TargetNode {
    /// No identifier: the method's document default.
    Default,
    /// A renderer node id.
    Node(u64),
    /// An `objectId` named a handle that does not wrap a node.
    Missing,
}

/// Whether a CDP node target names any identifier.
fn has_node_identifier(node: &crate::params::NodeTarget) -> bool {
    node.object.is_some() || node.backend.is_some() || node.node.is_some()
}

/// Resolves a CDP node target against the renderer handle table and node map.
async fn target_node(
    tab: &TabHandle,
    node: &crate::params::NodeTarget,
) -> Result<TargetNode, DispatchError> {
    if let Some(object_id) = &node.object {
        let reply = dom_request(
            tab,
            DomRequest::ResolveObject {
                object_id: object_id.clone(),
            },
        )
        .await?;
        let DomReply::NodeId { node_id } = reply else {
            return Err(DispatchError::BadHelperReply("DOM"));
        };
        return Ok(if node_id == 0 {
            TargetNode::Missing
        } else {
            TargetNode::Node(node_id)
        });
    }
    match node.backend.or(node.node) {
        Some(id) => Ok(TargetNode::Node(id)),
        None => Ok(TargetNode::Default),
    }
}

/// The error every required-node method answers when a handle names no node.
fn missing_node() -> DispatchError {
    DispatchError::Failed("Could not find node with given id".to_owned())
}

/// One `DOM.Node` reply from the renderer's typed node.
fn document_node(node: &DomNode) -> Value {
    let mut entry = serde_json::Map::new();
    entry.insert("nodeId".to_owned(), json!(node.node_id));
    entry.insert("backendNodeId".to_owned(), json!(node.backend_node_id));
    entry.insert("nodeType".to_owned(), json!(node.node_type));
    entry.insert("nodeName".to_owned(), json!(node.node_name));
    if node.node_type == 1 {
        entry.insert("localName".to_owned(), json!(node.local_name));
        entry.insert("nodeValue".to_owned(), json!(""));
        entry.insert("attributes".to_owned(), json!(node.attributes));
    } else if node.node_type == 9 {
        entry.insert("nodeValue".to_owned(), json!(""));
        entry.insert("documentURL".to_owned(), json!(node.document_url));
        entry.insert("baseURL".to_owned(), json!(node.base_url));
        entry.insert(
            "compatibilityMode".to_owned(),
            json!(node.compatibility_mode),
        );
    } else {
        entry.insert("nodeValue".to_owned(), json!(node.node_value));
    }
    entry.insert("childNodeCount".to_owned(), json!(node.child_node_count));
    if !node.children.is_empty() {
        entry.insert(
            "children".to_owned(),
            json!(node.children.iter().map(document_node).collect::<Vec<_>>()),
        );
    }
    Value::Object(entry)
}

/// One `DOM.describeNode` node: shape and child count, without children.
fn described_node(node: &DomNode) -> Value {
    let mut entry = serde_json::Map::new();
    entry.insert("nodeId".to_owned(), json!(node.node_id));
    entry.insert("backendNodeId".to_owned(), json!(node.backend_node_id));
    entry.insert("nodeType".to_owned(), json!(node.node_type));
    entry.insert("nodeName".to_owned(), json!(node.node_name));
    entry.insert("childNodeCount".to_owned(), json!(node.child_node_count));
    if node.node_type == 1 {
        entry.insert("localName".to_owned(), json!(node.local_name));
        entry.insert("attributes".to_owned(), json!(node.attributes));
    }
    Value::Object(entry)
}

/// `DOM.querySelector`: the first matching node's id.
pub(crate) async fn dom_query_selector(
    tab: &TabHandle,
    params: &Value,
) -> Result<Value, DispatchError> {
    let params: crate::params::QuerySelector = crate::params::parse(params)?;
    let reply = dom_request(
        tab,
        DomRequest::QuerySelector {
            node: params.node_id,
            selector: params.selector,
        },
    )
    .await?;
    let DomReply::NodeId { node_id } = reply else {
        return Err(DispatchError::BadHelperReply("DOM.querySelector"));
    };
    Ok(json!({"nodeId": node_id}))
}

/// `DOM.querySelectorAll`: every matching node's id.
pub(crate) async fn dom_query_selector_all(
    tab: &TabHandle,
    params: &Value,
) -> Result<Value, DispatchError> {
    let params: crate::params::QuerySelector = crate::params::parse(params)?;
    let reply = dom_request(
        tab,
        DomRequest::QuerySelectorAll {
            node: params.node_id,
            selector: params.selector,
        },
    )
    .await?;
    let DomReply::NodeIds { node_ids } = reply else {
        return Err(DispatchError::BadHelperReply("DOM.querySelectorAll"));
    };
    Ok(json!({"nodeIds": node_ids}))
}

/// `DOM.describeNode`: the node's shape, optionally with children.
pub(crate) async fn dom_describe_node(
    tab: &TabHandle,
    params: &Value,
) -> Result<Value, DispatchError> {
    let target: crate::params::NodeTarget = crate::params::parse(params)?;
    let node = match target_node(tab, &target).await? {
        TargetNode::Default => 0,
        TargetNode::Node(id) => id,
        TargetNode::Missing => return Ok(json!({"node": null})),
    };
    let reply = dom_request(tab, DomRequest::DescribeNode { node }).await?;
    let DomReply::Described { node } = reply else {
        return Err(DispatchError::BadHelperReply("DOM.describeNode"));
    };
    Ok(json!({
        "node": node.as_ref().map(described_node),
    }))
}

/// `DOM.getOuterHTML`: one node's serialized markup.
pub(crate) async fn dom_outer_html(
    tab: &TabHandle,
    params: &Value,
) -> Result<Value, DispatchError> {
    let target: crate::params::NodeTarget = crate::params::parse(params)?;
    if !has_node_identifier(&target) {
        return Err(DispatchError::InvalidParams(
            "nodeId, backendNodeId, or objectId".to_owned(),
        ));
    }
    let node = match target_node(tab, &target).await? {
        TargetNode::Node(id) => id,
        TargetNode::Default | TargetNode::Missing => return Err(missing_node()),
    };
    let reply = dom_request(tab, DomRequest::GetOuterHtml { node }).await?;
    let DomReply::OuterHtml { outer_html } = reply else {
        return Err(DispatchError::BadHelperReply("DOM.getOuterHTML"));
    };
    Ok(json!({"outerHTML": outer_html}))
}

/// `DOM.getAttributes`: one element's flattened attribute pairs.
pub(crate) async fn dom_attributes(
    tab: &TabHandle,
    params: &Value,
) -> Result<Value, DispatchError> {
    let target: crate::params::NodeTarget = crate::params::parse(params)?;
    if !has_node_identifier(&target) {
        return Err(DispatchError::InvalidParams(
            "nodeId, backendNodeId, or objectId".to_owned(),
        ));
    }
    let node = match target_node(tab, &target).await? {
        TargetNode::Node(id) => id,
        TargetNode::Default | TargetNode::Missing => return Err(missing_node()),
    };
    let reply = dom_request(tab, DomRequest::GetAttributes { node }).await?;
    let DomReply::Attributes { attributes } = reply else {
        return Err(DispatchError::BadHelperReply("DOM.getAttributes"));
    };
    Ok(json!({"attributes": attributes}))
}

/// `DOM.resolveNode`: intern the node in the runtime handle table.
pub(crate) async fn dom_resolve_node(
    tab: &TabHandle,
    params: &Value,
) -> Result<Value, DispatchError> {
    let target: crate::params::NodeTarget = crate::params::parse(params)?;
    let node = match target_node(tab, &target).await? {
        TargetNode::Default => 0,
        TargetNode::Node(id) => id,
        TargetNode::Missing => return Ok(json!({"object": {"type": "undefined"}})),
    };
    let reply = dom_request(tab, DomRequest::ResolveNode { node }).await?;
    let DomReply::Resolved { object } = reply else {
        return Err(DispatchError::BadHelperReply("DOM.resolveNode"));
    };
    Ok(json!({
        "object": object.map_or_else(
            || json!({"type": "undefined"}),
            |object| json!({
                "type": "object",
                "subtype": "node",
                "objectId": object.object_id,
                "className": object.class_name,
                "description": object.description,
            }),
        ),
    }))
}

/// `DOM.getNodeForLocation`: hit-test the viewport point.
pub(crate) async fn dom_node_for_location(
    tab: &TabHandle,
    params: &Value,
) -> Result<Value, DispatchError> {
    let params: crate::params::NodeForLocation = crate::params::parse(params)?;
    let reply = dom_request(
        tab,
        DomRequest::GetNodeForLocation {
            x: params.x,
            y: params.y,
        },
    )
    .await?;
    let DomReply::NodeForLocation { node_id } = reply else {
        return Err(DispatchError::BadHelperReply("DOM.getNodeForLocation"));
    };
    Ok(json!({"nodeId": node_id}))
}

/// `DOM.getBoxModel`: the node's border box as content/padding/border/margin
/// quads (the engine has no separate boxes, so all four are the border box).
pub(crate) async fn dom_box_model(tab: &TabHandle, params: &Value) -> Result<Value, DispatchError> {
    let target: crate::params::NodeTarget = crate::params::parse(params)?;
    let node = match target_node(tab, &target).await? {
        TargetNode::Default => 0,
        TargetNode::Node(id) => id,
        TargetNode::Missing => return Ok(json!({"model": null})),
    };
    let reply = dom_request(tab, DomRequest::GetBoxModel { node }).await?;
    let DomReply::BoxModel { model } = reply else {
        return Err(DispatchError::BadHelperReply("DOM.getBoxModel"));
    };
    Ok(json!({
        "model": model.map(|model| json!({
            "content": model.content,
            "padding": model.padding,
            "border": model.border,
            "margin": model.margin,
            "width": model.width,
            "height": model.height,
        })),
    }))
}

/// `DOM.getContentQuads`: one quad per client rect of the node.
pub(crate) async fn dom_content_quads(
    tab: &TabHandle,
    params: &Value,
) -> Result<Value, DispatchError> {
    let target: crate::params::NodeTarget = crate::params::parse(params)?;
    let node = match target_node(tab, &target).await? {
        TargetNode::Default => 0,
        TargetNode::Node(id) => id,
        TargetNode::Missing => return Ok(json!({"quads": []})),
    };
    let reply = dom_request(tab, DomRequest::GetContentQuads { node }).await?;
    let DomReply::ContentQuads { quads } = reply else {
        return Err(DispatchError::BadHelperReply("DOM.getContentQuads"));
    };
    Ok(json!({"quads": quads}))
}

/// `CSS.enable`: one `CSS.styleSheetAdded` per document stylesheet.
pub(crate) async fn css_stylesheets(tab: &TabHandle) -> Result<Vec<Value>, DispatchError> {
    const SCRIPT: &str = r#"(function(){
      host.__tb_css_id = host.__tb_css_id || 0;
      const out = host.array();
      for (const sheet of document.styleSheets) {
        const id = String(++host.__tb_css_id);
        host.push(out, {__proto__: null,
          styleSheetId: id,
          sourceURL: sheet.href || "",
          origin: "regular",
          title: sheet.title || "",
          disabled: !!sheet.disabled,
          isInline: !sheet.href,
          startLine: 0, startColumn: 0, endLine: 0, endColumn: 0,
          length: sheet.cssRules ? sheet.cssRules.length : 0,
        });
      }
      return host.stringify(out);
    })()"#;
    let value = browser_json(tab, SCRIPT).await?;
    Ok(value.as_array().cloned().unwrap_or_default())
}

/// `CSS.getComputedStyleForNode`: the resolved style as name/value pairs.
pub(crate) async fn css_computed_style(
    tab: &TabHandle,
    params: &Value,
) -> Result<Value, DispatchError> {
    const TEMPLATE: &str = r"(function(){
      const n = host.__tb_handles[__OBJECT__];
      if (!n || !globalThis.getComputedStyle) return host.stringify({__proto__: null, computedStyle: []});
      const style = globalThis.getComputedStyle(n);
      const computedStyle = host.array();
      for (let i = 0; i < style.length; i++) {
        const name = style.item(i);
        host.push(computedStyle, {__proto__: null, name: name, value: style.getPropertyValue(name)});
      }
      return host.stringify({__proto__: null, computedStyle: computedStyle});
    })()";
    let params: crate::params::ComputedStyleForNode = crate::params::parse(params)?;
    let reply = dom_request(
        tab,
        DomRequest::ResolveNode {
            node: params.node_id,
        },
    )
    .await?;
    let DomReply::Resolved {
        object: Some(object),
    } = reply
    else {
        return Ok(json!({"computedStyle": []}));
    };
    let script = TEMPLATE.replace("__OBJECT__", &json_string(&object.object_id));
    browser_json(tab, &script).await
}

/// Evaluates a browser-owned expression that returns a JSON string and parses
/// the reply. No DOM node lookup goes through page script.
async fn browser_json(tab: &TabHandle, script: &str) -> Result<Value, DispatchError> {
    let value = tab
        .execute_browser_script(script)
        .await
        .map_err(|error| DispatchError::Failed(error.to_string()))?;
    let RemoteValue::String(text) = value else {
        return Err(DispatchError::BadHelperReply("browser helper"));
    };
    serde_json::from_str(&text).map_err(|error| DispatchError::Failed(error.to_string()))
}

/// One viewport rectangle for `Page.getLayoutMetrics`.
fn viewport_rect(width: f64, height: f64) -> Value {
    json!({"pageX": 0, "pageY": 0, "clientWidth": width, "clientHeight": height})
}

/// `Page.captureScreenshot`: render the tab and return base64 PNG data.
///
/// Playwright always sends `clip`; it is forwarded to the renderer, which
/// rasters the requested viewport and crops before encoding. Non-PNG formats
/// are refused rather than mislabeled.
pub(crate) async fn capture_screenshot(
    tab: &TabHandle,
    params: &Value,
) -> Result<Value, DispatchError> {
    let params: crate::params::CaptureScreenshot = crate::params::parse(params)?;
    let format = params.format.as_deref().unwrap_or("png");
    if !format.eq_ignore_ascii_case("png") {
        return Err(DispatchError::Failed(
            "only png screenshots are supported".into(),
        ));
    }
    let clip = params.clip;
    let viewport_width = capture_side(clip.map_or(f64::from(DEFAULT_VIEWPORT.0), |clip| {
        f64::from(clip.x + clip.width)
    }));
    let viewport_height = capture_side(clip.map_or(f64::from(DEFAULT_VIEWPORT.1), |clip| {
        f64::from(clip.y + clip.height)
    }));
    let request = ScreenshotRequest {
        viewport_width,
        viewport_height,
        clip,
    };
    let frame = requested_frame(tab, params.frame_id.as_deref())?;
    let png = tab
        .screenshot_frame(frame, request)
        .await
        .map_err(|error| DispatchError::Failed(error.to_string()))?;
    Ok(json!({"data": BASE64_STANDARD.encode(&png)}))
}

/// Resolves the optional renderer-frame extension used by automation clients
/// that need to address a child realm directly. Standard CDP calls omit it
/// and therefore address the main frame.
pub(crate) fn requested_frame(
    tab: &TabHandle,
    raw: Option<&str>,
) -> Result<FrameId, DispatchError> {
    let Some(raw) = raw else {
        return Ok(FrameId::MAIN);
    };
    let target_id = tab.id().to_string();
    if raw == target_id || raw == "main" {
        return Ok(FrameId::MAIN);
    }
    let (_, renderer_frame) = raw
        .split_once('.')
        .filter(|(target, renderer_frame)| {
            *target == target_id && !renderer_frame.is_empty() && !renderer_frame.contains('.')
        })
        .ok_or_else(|| DispatchError::Failed("invalid frameId".into()))?;
    let renderer_frame = renderer_frame
        .parse::<u64>()
        .map_err(|_| DispatchError::Failed("invalid frameId".into()))?;
    if renderer_frame == FrameId::MAIN.get() {
        return Err(DispatchError::Failed("invalid frameId".into()));
    }
    Ok(FrameId::new(renderer_frame))
}

/// Waits for the next navigation outcome on a temporary subscription.
pub(crate) async fn wait_for_navigation(
    mut events: mpsc::Receiver<TabEvent>,
    duration: Duration,
) -> Result<(), &'static str> {
    let result = tokio::time::timeout(duration, async {
        loop {
            match events.recv().await {
                Some(TabEvent::Navigated) => return Ok(()),
                Some(TabEvent::NavigationFailed) => {
                    return Err("net::ERR_FAILED");
                }
                Some(_) => {}
                None => return Err("net::ERR_ABORTED"),
            }
        }
    })
    .await;
    match result {
        Ok(outcome) => outcome,
        Err(_) => Err("net::ERR_TIMED_OUT"),
    }
}

pub(crate) async fn open_url(tab: &TabHandle, url: &str) -> Result<(), DispatchError> {
    if url.is_empty() || url == "about:blank" {
        tab.load_html("<!doctype html><title></title>")
            .await
            .map_err(|err| DispatchError::Failed(err.to_string()))?;
        return Ok(());
    }
    tab.goto(url)
        .await
        .map_err(|err| DispatchError::Failed(err.to_string()))?;
    Ok(())
}

/// Builds the JS argument array for `Runtime.callFunctionOn`.
pub(crate) fn arguments_expression(arguments: &[CallArgument]) -> String {
    let parts: Vec<String> = arguments.iter().map(argument_expression).collect();
    format!("[{}]", parts.join(", "))
}

fn argument_expression(argument: &CallArgument) -> String {
    match argument {
        CallArgument::Object { object_id } => {
            format!("host.__tb_handles[{}]", json_string(object_id))
        }
        CallArgument::Unserializable { value } => unserializable_literal(value),
        CallArgument::Value { value } => {
            serde_json::to_string(value).unwrap_or_else(|_| "undefined".to_owned())
        }
        CallArgument::Empty {} => "undefined".to_owned(),
    }
}

/// One `unserializableValue` as the JS literal it names. The protocol lists
/// exactly these forms; anything else reaches the page as `undefined` rather
/// than as a string, which is what a failed decode would produce.
fn unserializable_literal(value: &str) -> String {
    match value {
        "NaN" | "Infinity" | "-Infinity" | "-0" => value.to_owned(),
        _ => match value.strip_suffix('n') {
            Some(digits) if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) => {
                value.to_owned()
            }
            _ => "undefined".to_owned(),
        },
    }
}

/// JS string literal for embedding in generated source.
pub(crate) fn json_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_owned())
}

/// A failed runtime call, shaped like a CDP exception.
pub(crate) fn exception_reply(error: &TabError) -> Value {
    json!({
        "result": {"type": "undefined"},
        "exceptionDetails": {"text": error.to_string()},
    })
}

/// A failed runtime call with a literal message.
pub(crate) fn exception_text_reply(text: &str) -> Value {
    json!({
        "result": {"type": "undefined"},
        "exceptionDetails": {"text": text},
    })
}

/// Schedules `__SOURCE__` into `host.__SLOT__[__ID__]` and captures its
/// settled value. Results key by evaluation ID; a single shared slot would
/// let overlapping evaluations overwrite each other.
pub(crate) const RUNTIME_SCHEDULE: &str = r#"(() => {
  const slot = { __proto__: null, done: false };
  host.__SLOT__[__ID__] = slot;
  Promise.resolve((__SOURCE__)).then(
    (v) => { slot.done = true; slot.value = v; },
    (e) => { slot.done = true; slot.error = String((e && e.message) || e); }
  );
  return "scheduled";
})()"#;

/// Evaluates `__SOURCE__` and serializes the value with the bridge's one
/// `RemoteObject` serializer. Handles join `__GROUP__`.
pub(crate) const RUNTIME_HANDLE: &str = r"(() => {
  const v = (__SOURCE__);
  return host.stringify(host.remoteObject(v, { __proto__: null, objectGroup: __GROUP__ }));
})()";

/// Reads the awaited handle result and serializes it with the bridge's one
/// `RemoteObject` serializer.
pub(crate) const RUNTIME_HANDLE_READ: &str = r"(() => {
  const slot = host.__tb_async_handles && host.__tb_async_handles[__ID__];
  if (!slot || !slot.done) return host.stringify({ __proto__: null, pending: true });
  delete host.__tb_async_handles[__ID__];
  if (slot.error !== undefined) return host.stringify({ __proto__: null, error: slot.error });
  return host.stringify({ __proto__: null, value: host.remoteObject(slot.value, { __proto__: null, objectGroup: __GROUP__ }) });
})()";

/// Releases one handle or every handle in one object group.
pub(crate) const RUNTIME_RELEASE: &str = r#"(() => {
  const args = __ARGS__;
  if (args.objectGroup !== undefined) host.__tb_release_group(args.objectGroup);
  else host.__tb_release_handle(args.objectId);
  return "released";
})()"#;

/// Drops every handle owned by the outgoing execution context.
pub(crate) const RUNTIME_CLEAR_HANDLES: &str = "host.__tb_clear_handles()";

/// Enumerates a page-side handle's own and inherited properties as CDP
/// `PropertyDescriptor` JSON. Accessor descriptors expose `get`/`set`
/// `RemoteObject`s instead of invoking them. Values that are not primitives
/// become handles in the request's object group.
pub(crate) const RUNTIME_GET_PROPERTIES: &str = r#"(() => {
  const args = __ARGS__;
  const objectId = args.objectId;
  if (!(objectId in host.__tb_handles))
    return host.stringify({ __proto__: null, ok: false, error: "Could not find object with given id" });
  const target = host.__tb_handles[objectId];
  const ownProperties = args.ownProperties;
  const accessorsOnly = args.accessorPropertiesOnly;
  const generatePreview = args.generatePreview;
  const remote = (value, preview) => host.remoteObject(value, {
    __proto__: null,
    objectGroup: args.objectGroup,
    generatePreview: preview,
  });
  const result = [];
  const pushDescriptor = (name, descriptor, symbol, isOwn) => {
    const accessor = !('value' in descriptor);
    if (accessorsOnly && !accessor) return;
    const entry = { __proto__: null,
      name: name,
      configurable: !!descriptor.configurable,
      enumerable: !!descriptor.enumerable,
    };
    if (symbol) entry.symbol = remote(symbol, false);
    if (accessor) {
      if (descriptor.get) entry.get = remote(descriptor.get, false);
      if (descriptor.set) entry.set = remote(descriptor.set, false);
    } else {
      entry.value = remote(descriptor.value, generatePreview);
      entry.writable = !!descriptor.writable;
    }
    if (!ownProperties) entry.isOwn = isOwn;
    result[result.length] = entry;
  };
  const seenNames = { __proto__: null };
  const seenSymbols = new host.__tb_set();
  for (let object = target; object !== null && object !== undefined;
       object = host.__tb_getPrototypeOf(object)) {
    const isOwn = object === target;
    const names = host.__tb_getOwnPropertyNames(object);
    for (let index = 0; index < names.length; index++) {
      const name = names[index];
      if (seenNames[name]) continue;
      seenNames[name] = true;
      const descriptor = host.__tb_getOwnPropertyDescriptor(object, name);
      if (descriptor) pushDescriptor(name, descriptor, null, isOwn);
    }
    const symbols = host.__tb_getOwnPropertySymbols(object);
    for (let index = 0; index < symbols.length; index++) {
      const symbol = symbols[index];
      if (host.apply(host.__tb_set_has, seenSymbols, [symbol])) continue;
      host.apply(host.__tb_set_add, seenSymbols, [symbol]);
      const descriptor = host.__tb_getOwnPropertyDescriptor(object, symbol);
      if (descriptor) {
        const name = host.apply(host.__tb_symbolToString, symbol, []);
        pushDescriptor(name, descriptor, symbol, isOwn);
      }
    }
    if (ownProperties) break;
  }
  const internal = [];
  if (!accessorsOnly) {
    internal[internal.length] = { __proto__: null,
      name: "[[Prototype]]",
      value: remote(host.__tb_getPrototypeOf(target), false),
    };
  }
  return host.stringify({ __proto__: null, ok: true, value: {
    __proto__: null,
    result: result,
    internalProperties: internal,
  }});
})()"#;

/// Reads the captured async result and serializes it with the bridge's one
/// `RemoteObject` serializer, asking for a by-value reply.
pub(crate) const RUNTIME_READ: &str = r#"(() => {
  const s = host.__tb_async && host.__tb_async[__ID__];
  if (!s || !s.done) return host.stringify({__proto__: null, pending: true });
  delete host.__tb_async[__ID__];
  if (s.error !== undefined) return host.stringify({__proto__: null, error: s.error });
  const remote = host.remoteObject(s.value, { __proto__: null, returnByValue: true });
  try { return host.stringify(remote); }
  catch (e) { return host.stringify({__proto__: null, type: "object"}); }
})()"#;

pub(crate) async fn target_info(browser: &BrowserHandle, id: TabId) -> Value {
    let (url, title) = match browser.tab(id).await {
        Ok(tab) => {
            let url = tab
                .document_url()
                .await
                .unwrap_or_else(|_| "about:blank".into());
            let title = tab.document_title().await.unwrap_or_default();
            (url, title)
        }
        Err(_) => ("about:blank".into(), String::new()),
    };
    json!({
        "targetId": id.to_string(),
        "type": "page",
        "title": title,
        "url": url,
        "attached": false,
        "canAccessOpener": false,
        "browserContextId": DEFAULT_BROWSER_CONTEXT_ID,
    })
}

pub(crate) fn target_id(raw: &str) -> Result<TabId, DispatchError> {
    let id = raw
        .parse::<u64>()
        .map_err(|_| DispatchError::InvalidParams("targetId".to_owned()))?;
    Ok(TabId::new(id))
}

#[derive(Debug)]
pub(crate) enum DispatchError {
    MethodNotFound,
    /// A required parameter is missing or malformed. The message names the
    /// field, or carries serde's field path when a struct failed to decode.
    InvalidParams(String),
    /// A browser or page helper answered in a shape the dispatch cannot read.
    BadHelperReply(&'static str),
    Failed(String),
}

impl DispatchError {
    /// The JSON-RPC error code for the reply.
    pub(crate) fn code(&self) -> i32 {
        match self {
            Self::MethodNotFound => -32601,
            Self::InvalidParams(_) => -32602,
            Self::BadHelperReply(_) | Self::Failed(_) => -32000,
        }
    }

    /// The JSON-RPC error message for the reply.
    pub(crate) fn message(&self, method: &str) -> String {
        match self {
            Self::MethodNotFound => format!("'{method}' wasn't found"),
            Self::InvalidParams(name) => format!("Invalid params: {name}"),
            Self::BadHelperReply(helper) => {
                format!("the {helper} helper did not return a string result")
            }
            Self::Failed(message) => message.clone(),
        }
    }
}

pub(crate) fn attach_session(reply: &mut Value, session: Option<&str>) {
    if let Some(session) = session
        && let Some(object) = reply.as_object_mut()
    {
        object.insert("sessionId".into(), json!(session));
    }
}

pub(crate) fn ws_io(err: tungstenite::Error) -> io::Error {
    io::Error::other(err)
}

pub(crate) fn json_io(err: serde_json::Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, err)
}

#[cfg(test)]
mod tests {
    use super::{ROUTES, arguments_expression, capture_side, get_properties_script};
    use crate::params::{self, CallArgument, GetProperties, SetDeviceMetrics};
    use serde_json::json;

    #[test]
    fn device_metrics_require_integers_in_range() {
        let params: SetDeviceMetrics =
            params::parse(&json!({"width": 1024, "height": 768})).expect("integer metrics");
        assert_eq!(params.width, 1024);
        assert_eq!(params.height, 768);
        assert!(params::parse::<SetDeviceMetrics>(&json!({"height": 768})).is_err());
        assert!(params::parse::<SetDeviceMetrics>(&json!({"width": 10.5, "height": 1})).is_err());
        assert!(params::parse::<SetDeviceMetrics>(&json!({"width": -1, "height": 1})).is_err());
        assert!(params::parse::<SetDeviceMetrics>(&json!({"width": 1, "height": 1})).is_ok());
    }

    /// The page helper reads six keys with fixed types; a `null` where it
    /// expects `false` changes how it walks the chain.
    #[test]
    fn get_properties_arguments_keep_the_helper_shape() {
        let params: GetProperties = params::parse(&json!({"objectId": "h1"})).expect("params");
        let script = get_properties_script(&params);
        for expected in [
            r#""objectId":"h1""#,
            r#""objectGroup":"""#,
            r#""ownProperties":false"#,
            r#""nonIndexedPropertiesOnly":false"#,
            r#""accessorPropertiesOnly":false"#,
            r#""generatePreview":false"#,
        ] {
            assert!(
                script.contains(expected),
                "{expected} missing from {script}"
            );
        }
    }

    /// Each `CallArgument` variant becomes the JS expression the protocol
    /// names, and an unserializable primitive is not silently lost.
    #[test]
    fn call_arguments_translate_each_variant() {
        let arguments: Vec<CallArgument> = serde_json::from_value(json!([
            {"objectId": "h1"},
            {"value": {"a": 1}},
            {"value": null},
            {"unserializableValue": "NaN"},
            {"unserializableValue": "-0"},
            {"unserializableValue": "123n"},
            {},
        ]))
        .expect("arguments");
        assert_eq!(
            arguments_expression(&arguments),
            r#"[host.__tb_handles["h1"], {"a":1}, null, NaN, -0, 123n, undefined]"#
        );
    }

    #[test]
    fn capture_sides_are_bounded() {
        assert_eq!(capture_side(f64::NAN), 1);
        assert_eq!(capture_side(0.0), 1);
        assert_eq!(capture_side(1500.0), 1500);
        assert_eq!(capture_side(10_000.0), 4096);
    }

    #[test]
    fn every_registered_pair_has_one_handler() {
        for (index, route) in ROUTES.iter().enumerate() {
            for other in &ROUTES[index + 1..] {
                assert!(
                    route.method != other.method || !route.target.overlaps(other.target),
                    "{} has overlapping handlers for the same target kind",
                    route.method
                );
            }
        }
    }
}
