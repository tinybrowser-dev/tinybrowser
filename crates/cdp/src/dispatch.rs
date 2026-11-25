use std::io;
use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use browser::{
    AxNode, AxPropertyValue, BrowserHandle, CookieFilter, CookieRecord, CookieSameSite,
    DEFAULT_VIEWPORT, DirectCookie, DomNode, DomReply, DomRequest, FrameId, FrameInfo, RemoteValue,
    ScreenshotFormat, ScreenshotRequest, TabError, TabEvent, TabHandle, TabId, UserContextId,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::params::CallArgument;

const MAX_SCREENSHOT_DIM: f64 = 4096.0;

#[derive(Clone)]
pub(crate) enum Target {
    Page(TabHandle),
    Browser,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TargetClass {
    Page,
    Browser,
    Both,
}

impl TargetClass {
    fn accepts(self, target: &Target) -> bool {
        match (self, target) {
            (Self::Both, Target::Page(_) | Target::Browser)
            | (Self::Page, Target::Page(_))
            | (Self::Browser, Target::Browser) => true,
            (Self::Page, Target::Browser) | (Self::Browser, Target::Page(_)) => false,
        }
    }

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

#[derive(Clone, Copy)]
pub(crate) enum Handler {
    Accept(Accepted),
    ClearBrowserCookies,
    ClearBrowserCache,
    SetCacheDisabled,
    GetTargetInfo,
    StorageGetCookies,
    StorageSetCookies,
    StorageClearCookies,
    Page(PageHandler),
    Browser(BrowserHandler),
}

#[derive(Clone, Copy)]
pub(crate) enum PageHandler {
    Enable,
    Disable,
    Navigate,
    NavigateToHistoryEntry,
    SetDocumentContent,
    HandleJavaScriptDialog,
    CaptureScreenshot,
    SetDefaultBackgroundColorOverride,
    NetworkEnable,
    NetworkDisable,
    NetworkGetResponseBody,
    NetworkSetExtraHttpHeaders,
    NetworkGetCookies,
    NetworkGetAllCookies,
    NetworkSetCookie,
    NetworkSetCookies,
    NetworkDeleteCookies,
    NetworkSetBlockedUrls,
    FetchEnable,
    FetchDisable,
    FetchContinueRequest,
    FetchFulfillRequest,
    FetchFailRequest,
    CreateIsolatedWorld,
    SetLifecycleEventsEnabled,
    RuntimeEvaluate,
    RuntimeCallFunctionOn,
    RuntimeCompileScript,
    RuntimeRunScript,
    RuntimeEnable,
    RuntimeDisable,
    RuntimeAddBinding,
    RuntimeReleaseObject,
    RuntimeReleaseObjectGroup,
    CssEnable,
    CssDisable,
    TracingEnd,
    Script(ScriptMethod),
}

#[derive(Clone, Copy)]
pub(crate) enum BrowserHandler {
    GetVersion,
    Close,
    SetAutoAttach,
    SetDiscoverTargets,
    GetTargets,
    CreateTarget,
    CloseTarget,
    CreateBrowserContext,
    DisposeBrowserContext,
    GetBrowserContexts,
    AttachToTarget,
    AttachToBrowserTarget,
    DetachFromTarget,
}

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
    SetUserAgentOverride,
    SetTimezoneOverride,
    SetLocaleOverride,
    SetGeolocationOverride,
    ClearGeolocationOverride,
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
    DomScrollIntoViewIfNeeded,
    DomFocus,
    DomSetAttributeValue,
    DomRemoveAttribute,
    DomRemoveNode,
    DomSetFileInputFiles,
    InputMouseEvent,
    InputKeyEvent,
    InputInsertText,
    InputTouchEvent,
    InputEmulateTouchFromMouse,
    CssGetComputedStyleForNode,
    AccessibilityGetFullAxTree,
}

#[derive(Clone, Copy)]
pub(crate) enum Accepted {
    WaitingForDebugger,
    DomainWithoutEvents,
    FocusIsNotEngineState,
    PageHasNoChildTargets,
    RendererOwnsFonts,
    NoDownloadPipeline,
    DefaultEmulatedMedia,
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
        method: "Target.setDiscoverTargets",
        target: TargetClass::Browser,
        handler: Handler::Browser(BrowserHandler::SetDiscoverTargets),
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
        method: "Target.createBrowserContext",
        target: TargetClass::Browser,
        handler: Handler::Browser(BrowserHandler::CreateBrowserContext),
    },
    Route {
        method: "Target.disposeBrowserContext",
        target: TargetClass::Browser,
        handler: Handler::Browser(BrowserHandler::DisposeBrowserContext),
    },
    Route {
        method: "Target.getBrowserContexts",
        target: TargetClass::Browser,
        handler: Handler::Browser(BrowserHandler::GetBrowserContexts),
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
        method: "Page.setDocumentContent",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::SetDocumentContent),
    },
    Route {
        method: "Page.handleJavaScriptDialog",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::HandleJavaScriptDialog),
    },
    Route {
        method: "Page.bringToFront",
        target: TargetClass::Page,
        handler: Handler::Accept(Accepted::FocusIsNotEngineState),
    },
    Route {
        method: "Page.stopLoading",
        target: TargetClass::Page,
        handler: Handler::Accept(Accepted::DomainWithoutEvents),
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
        method: "Runtime.compileScript",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::RuntimeCompileScript),
    },
    Route {
        method: "Runtime.runScript",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::RuntimeRunScript),
    },
    Route {
        method: "Emulation.setDefaultBackgroundColorOverride",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::SetDefaultBackgroundColorOverride),
    },
    Route {
        method: "Network.getResponseBody",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::NetworkGetResponseBody),
    },
    Route {
        method: "Network.setExtraHTTPHeaders",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::NetworkSetExtraHttpHeaders),
    },
    Route {
        method: "Network.getCookies",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::NetworkGetCookies),
    },
    Route {
        method: "Network.getAllCookies",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::NetworkGetAllCookies),
    },
    Route {
        method: "Network.setCookie",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::NetworkSetCookie),
    },
    Route {
        method: "Network.setCookies",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::NetworkSetCookies),
    },
    Route {
        method: "Network.deleteCookies",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::NetworkDeleteCookies),
    },
    Route {
        method: "Network.setBlockedURLs",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::NetworkSetBlockedUrls),
    },
    Route {
        method: "Fetch.enable",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::FetchEnable),
    },
    Route {
        method: "Fetch.disable",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::FetchDisable),
    },
    Route {
        method: "Fetch.continueRequest",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::FetchContinueRequest),
    },
    Route {
        method: "Fetch.fulfillRequest",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::FetchFulfillRequest),
    },
    Route {
        method: "Fetch.failRequest",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::FetchFailRequest),
    },
    Route {
        method: "Runtime.getProperties",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::Script(ScriptMethod::GetProperties)),
    },
    Route {
        method: "Runtime.addBinding",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::RuntimeAddBinding),
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
        method: "DOM.enable",
        target: TargetClass::Page,
        handler: Handler::Accept(Accepted::DomainWithoutEvents),
    },
    Route {
        method: "DOM.disable",
        target: TargetClass::Page,
        handler: Handler::Accept(Accepted::DomainWithoutEvents),
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
        method: "DOM.scrollIntoViewIfNeeded",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::Script(ScriptMethod::DomScrollIntoViewIfNeeded)),
    },
    Route {
        method: "DOM.focus",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::Script(ScriptMethod::DomFocus)),
    },
    Route {
        method: "DOM.setAttributeValue",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::Script(ScriptMethod::DomSetAttributeValue)),
    },
    Route {
        method: "DOM.removeAttribute",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::Script(ScriptMethod::DomRemoveAttribute)),
    },
    Route {
        method: "DOM.removeNode",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::Script(ScriptMethod::DomRemoveNode)),
    },
    Route {
        method: "DOM.requestChildNodes",
        target: TargetClass::Page,
        handler: Handler::Accept(Accepted::DomainWithoutEvents),
    },
    Route {
        method: "DOM.setFileInputFiles",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::Script(ScriptMethod::DomSetFileInputFiles)),
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
        method: "Input.setInterceptDrags",
        target: TargetClass::Page,
        handler: Handler::Accept(Accepted::DomainWithoutEvents),
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
        method: "Accessibility.enable",
        target: TargetClass::Page,
        handler: Handler::Accept(Accepted::DomainWithoutEvents),
    },
    Route {
        method: "Accessibility.disable",
        target: TargetClass::Page,
        handler: Handler::Accept(Accepted::DomainWithoutEvents),
    },
    Route {
        method: "Accessibility.getFullAXTree",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::Script(
            ScriptMethod::AccessibilityGetFullAxTree,
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
        method: "Storage.getCookies",
        target: TargetClass::Both,
        handler: Handler::StorageGetCookies,
    },
    Route {
        method: "Storage.setCookies",
        target: TargetClass::Both,
        handler: Handler::StorageSetCookies,
    },
    Route {
        method: "Storage.clearCookies",
        target: TargetClass::Both,
        handler: Handler::StorageClearCookies,
    },
    Route {
        method: "Log.enable",
        target: TargetClass::Both,
        handler: Handler::Accept(Accepted::DomainWithoutEvents),
    },
    Route {
        method: "Log.disable",
        target: TargetClass::Both,
        handler: Handler::Accept(Accepted::DomainWithoutEvents),
    },
    Route {
        method: "Network.enable",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::NetworkEnable),
    },
    Route {
        method: "Network.enable",
        target: TargetClass::Browser,
        handler: Handler::Accept(Accepted::DomainWithoutEvents),
    },
    Route {
        method: "Network.disable",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::NetworkDisable),
    },
    Route {
        method: "Network.disable",
        target: TargetClass::Browser,
        handler: Handler::Accept(Accepted::DomainWithoutEvents),
    },
    Route {
        method: "Network.clearBrowserCache",
        target: TargetClass::Both,
        handler: Handler::ClearBrowserCache,
    },
    Route {
        method: "Network.setCacheDisabled",
        target: TargetClass::Both,
        handler: Handler::SetCacheDisabled,
    },
    Route {
        method: "Network.setUserAgentOverride",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::Script(ScriptMethod::SetUserAgentOverride)),
    },
    Route {
        method: "Emulation.setUserAgentOverride",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::Script(ScriptMethod::SetUserAgentOverride)),
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
        method: "Emulation.setTimezoneOverride",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::Script(ScriptMethod::SetTimezoneOverride)),
    },
    Route {
        method: "Emulation.setLocaleOverride",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::Script(ScriptMethod::SetLocaleOverride)),
    },
    Route {
        method: "Emulation.setGeolocationOverride",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::Script(ScriptMethod::SetGeolocationOverride)),
    },
    Route {
        method: "Emulation.clearGeolocationOverride",
        target: TargetClass::Page,
        handler: Handler::Page(PageHandler::Script(ScriptMethod::ClearGeolocationOverride)),
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

pub(crate) fn route(method: &str, target: &Target) -> Option<Handler> {
    ROUTES
        .iter()
        .find(|route| route.method == method && route.target.accepts(target))
        .map(|route| route.handler)
}

pub(crate) fn accept(reason: Accepted, params: &Value) -> Result<Value, DispatchError> {
    if premise_holds(reason, params)? {
        Ok(json!({}))
    } else {
        Err(DispatchError::MethodNotFound)
    }
}

fn premise_holds(reason: Accepted, params: &Value) -> Result<bool, DispatchError> {
    match reason {
        Accepted::DefaultEmulatedMedia => {
            let params: crate::params::SetEmulatedMedia = crate::params::parse(params)?;
            Ok(emulated_media_is_default(&params))
        }
        Accepted::WaitingForDebugger
        | Accepted::DomainWithoutEvents
        | Accepted::FocusIsNotEngineState
        | Accepted::PageHasNoChildTargets
        | Accepted::RendererOwnsFonts
        | Accepted::NoDownloadPipeline => Ok(true),
    }
}

pub(crate) async fn run_script(
    method: ScriptMethod,
    params: &Value,
    tab: &TabHandle,
    world: Option<&str>,
) -> Result<Value, DispatchError> {
    match method {
        ScriptMethod::GetFrameTree => frame_tree(tab, false).await,
        ScriptMethod::GetResourceTree => frame_tree(tab, true).await,
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
        ScriptMethod::SetUserAgentOverride => set_user_agent_override(params, tab).await,
        ScriptMethod::SetTimezoneOverride => set_timezone_override(params),
        ScriptMethod::SetLocaleOverride => set_locale_override(params),
        ScriptMethod::SetGeolocationOverride => set_geolocation_override(params),
        ScriptMethod::ClearGeolocationOverride => Ok(json!({})),
        ScriptMethod::GetStorageKey => storage_key(tab).await,
        ScriptMethod::DomGetDocument => dom_get_document(tab).await,
        ScriptMethod::DomQuerySelector => dom_query_selector(tab, params).await,
        ScriptMethod::DomQuerySelectorAll => dom_query_selector_all(tab, params).await,
        ScriptMethod::DomDescribeNode => dom_describe_node(tab, params).await,
        ScriptMethod::DomGetOuterHtml => dom_outer_html(tab, params).await,
        ScriptMethod::DomGetAttributes => dom_attributes(tab, params).await,
        ScriptMethod::DomResolveNode => dom_resolve_node(tab, params, world).await,
        ScriptMethod::DomGetNodeForLocation => dom_node_for_location(tab, params).await,
        ScriptMethod::DomGetBoxModel => dom_box_model(tab, params).await,
        ScriptMethod::DomGetContentQuads => dom_content_quads(tab, params).await,
        ScriptMethod::DomScrollIntoViewIfNeeded => {
            dom_scroll_into_view_if_needed(tab, params).await
        }
        ScriptMethod::DomFocus => dom_focus(tab, params).await,
        ScriptMethod::DomSetAttributeValue => dom_set_attribute_value(tab, params).await,
        ScriptMethod::DomRemoveAttribute => dom_remove_attribute(tab, params).await,
        ScriptMethod::DomRemoveNode => dom_remove_node(tab, params).await,
        ScriptMethod::DomSetFileInputFiles => dom_set_file_input_files(tab, params).await,
        ScriptMethod::InputMouseEvent => input_mouse_event(tab, params).await,
        ScriptMethod::InputKeyEvent => input_key_event(tab, params).await,
        ScriptMethod::InputInsertText => input_insert_text(tab, params).await,
        ScriptMethod::InputTouchEvent => input_touch_event(tab, params).await,
        ScriptMethod::InputEmulateTouchFromMouse => {
            input_emulate_touch_from_mouse(tab, params).await
        }
        ScriptMethod::CssGetComputedStyleForNode => css_computed_style(tab, params).await,
        ScriptMethod::AccessibilityGetFullAxTree => accessibility_full_tree(tab).await,
    }
}

pub(crate) fn cdp_frame_id(tab: TabId, frame: FrameId) -> String {
    if frame == FrameId::MAIN {
        tab.to_string()
    } else {
        format!("{tab}.{}", frame.get())
    }
}

pub(crate) fn url_security_origin(url: &str) -> String {
    let Ok(url) = url::Url::parse(url) else {
        return "null".to_owned();
    };
    net::origin_url(&url.origin()).map_or_else(
        || "null".to_owned(),
        |origin| origin[..url::Position::BeforePath].to_owned(),
    )
}

fn build_frame_tree_node(
    tab: TabId,
    frame: &FrameInfo,
    all: &[FrameInfo],
    with_resources: bool,
) -> Value {
    let mut frame_obj = serde_json::Map::new();
    frame_obj.insert("id".into(), json!(cdp_frame_id(tab, frame.id)));
    if let Some(parent) = frame.parent {
        frame_obj.insert("parentId".into(), json!(cdp_frame_id(tab, parent)));
    }
    frame_obj.insert("loaderId".into(), json!(""));
    frame_obj.insert("url".into(), json!(frame.url));
    frame_obj.insert(
        "securityOrigin".into(),
        json!(url_security_origin(&frame.url)),
    );
    frame_obj.insert("mimeType".into(), json!("text/html"));
    let children: Vec<Value> = all
        .iter()
        .filter(|child| child.parent == Some(frame.id))
        .map(|child| build_frame_tree_node(tab, child, all, with_resources))
        .collect();
    let mut node = serde_json::Map::new();
    node.insert("frame".into(), Value::Object(frame_obj));
    if !children.is_empty() {
        node.insert("childFrames".into(), Value::Array(children));
    }
    if with_resources {
        node.insert("resources".into(), json!([]));
    }
    Value::Object(node)
}

async fn frame_tree(tab: &TabHandle, with_resources: bool) -> Result<Value, DispatchError> {
    let frames = tab
        .frames()
        .await
        .map_err(|error| DispatchError::Failed(error.to_string()))?;
    if let Some(main) = frames.iter().find(|f| f.id == FrameId::MAIN) {
        return Ok(json!({
            "frameTree": build_frame_tree_node(tab.id(), main, &frames, with_resources)
        }));
    }
    let url = tab
        .document_url()
        .await
        .map_err(|error| DispatchError::Failed(error.to_string()))?;
    let mut node = json!({
        "frame": {
            "id": tab.id().to_string(),
            "loaderId": "",
            "url": url,
            "securityOrigin": url_security_origin(&url),
            "mimeType": "text/html",
        }
    });
    if with_resources && let Some(obj) = node.as_object_mut() {
        obj.insert("resources".into(), json!([]));
    }
    Ok(json!({"frameTree": node}))
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

// https://chromedevtools.github.io/devtools-protocol/tot/Page/#method-getNavigationHistory
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

pub(crate) fn isolated_world_of_handle(object_id: &str) -> Option<&str> {
    let rest = object_id.strip_prefix("iw:")?;
    let (world, _) = rest.rsplit_once(':')?;
    (!world.is_empty()).then_some(world)
}

// https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#method-getProperties
async fn runtime_get_properties(tab: &TabHandle, params: &Value) -> Result<Value, DispatchError> {
    let params: crate::params::GetProperties = crate::params::parse(params)?;
    if params.object_id.is_empty() {
        return Err(DispatchError::InvalidParams("objectId".to_owned()));
    }
    let script = get_properties_script(&params);
    let reply = execute_json_in(tab, isolated_world_of_handle(&params.object_id), &script).await?;
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

pub(crate) async fn runtime_release_object(
    tab: &TabHandle,
    params: &Value,
) -> Result<Value, DispatchError> {
    let params: crate::params::ReleaseObject = crate::params::parse(params)?;
    let script = RUNTIME_RELEASE.replace(
        "__ARGS__",
        &json!({"objectId": params.object_id}).to_string(),
    );
    let source = match isolated_world_of_handle(&params.object_id) {
        Some(world) => browser::ScriptSource::Isolated {
            world,
            source: script.as_str(),
        },
        None => browser::ScriptSource::Browser(script.as_str()),
    };
    tab.execute_script(browser::ExecuteScriptOptions {
        frame: FrameId::MAIN,
        source,
        timeout: None,
    })
    .await
    .map_err(|error| DispatchError::Failed(error.to_string()))?;
    Ok(json!({}))
}

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

async fn execute_json_in(
    tab: &TabHandle,
    world: Option<&str>,
    script: &str,
) -> Result<Value, DispatchError> {
    let source = match world {
        Some(world) => browser::ScriptSource::Isolated {
            world,
            source: script,
        },
        None => browser::ScriptSource::Browser(script),
    };
    let value = tab
        .execute_script(browser::ExecuteScriptOptions {
            frame: FrameId::MAIN,
            source,
            timeout: None,
        })
        .await
        .map_err(|error| DispatchError::Failed(error.to_string()))?;
    let RemoteValue::String(text) = value else {
        return Err(DispatchError::BadHelperReply("browser"));
    };
    serde_json::from_str(&text).map_err(|error| DispatchError::Failed(error.to_string()))
}

fn emulated_media_is_default(params: &crate::params::SetEmulatedMedia) -> bool {
    if params
        .media
        .as_deref()
        .is_some_and(|media| !media.is_empty() && media != "screen")
    {
        return false;
    }
    match &params.features {
        None => true,
        Some(features) => features.iter().all(|feature| {
            feature.value.is_empty()
                || matches!(
                    (feature.name.as_str(), feature.value.as_str()),
                    ("prefers-color-scheme", "light")
                        | (
                            "prefers-reduced-motion" | "prefers-contrast",
                            "no-preference"
                        )
                        | ("forced-colors", "none")
                )
        }),
    }
}

async fn add_init_script(params: &Value, tab: &TabHandle) -> Result<Value, DispatchError> {
    let params: crate::params::AddInitScript = crate::params::parse(params)?;
    let id = tab
        .add_init_script(params.source, params.run_immediately.unwrap_or(false))
        .await
        .map_err(|error| DispatchError::Failed(error.to_string()))?;
    Ok(json!({"identifier": id.to_string()}))
}

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

// https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#method-addBinding
pub(crate) async fn add_binding(params: &Value, tab: &TabHandle) -> Result<Value, DispatchError> {
    let params: crate::params::AddBinding = crate::params::parse(params)?;
    tab.add_binding(&params.name, true)
        .await
        .map_err(|error| DispatchError::Failed(error.to_string()))?;
    Ok(json!({}))
}

// https://chromedevtools.github.io/devtools-protocol/tot/Emulation/#method-setDeviceMetricsOverride
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

async fn clear_device_metrics(tab: &TabHandle) -> Result<Value, DispatchError> {
    set_viewport(tab, DEFAULT_VIEWPORT.0, DEFAULT_VIEWPORT.1).await
}

// https://chromedevtools.github.io/devtools-protocol/tot/Emulation/#method-setUserAgentOverride
async fn set_user_agent_override(params: &Value, tab: &TabHandle) -> Result<Value, DispatchError> {
    let params: crate::params::UserAgentOverride = crate::params::parse(params)?;
    let override_identity = if params.user_agent.is_empty() {
        None
    } else {
        let user_agent = browser::UserAgent::parse(&params.user_agent)
            .map_err(|error| DispatchError::InvalidParams(error.to_string()))?;
        let mut identity = browser::Identity::custom(user_agent);
        if let Some(platform) = params.platform {
            identity.navigator_platform = platform;
        }
        if let Some(accept_language) = params.accept_language {
            let languages: Vec<String> = accept_language
                .split(',')
                .map(|part| part.split(';').next().unwrap_or_default().trim().to_owned())
                .filter(|tag| !tag.is_empty())
                .collect();
            if !languages.is_empty() {
                identity.languages = languages;
            }
        }
        if let Some(meta) = params.user_agent_metadata {
            if let Some(brands) = meta.brands {
                identity.brands = brands
                    .into_iter()
                    .map(|b| browser::Brand {
                        brand: b.brand,
                        version: b.version,
                    })
                    .collect();
            }
            if let Some(full) = meta.full_version_list {
                identity.full_version_list = full
                    .into_iter()
                    .map(|b| browser::Brand {
                        brand: b.brand,
                        version: b.version,
                    })
                    .collect();
            }
            if let Some(platform) = meta.platform {
                identity.platform = platform;
            }
            if let Some(platform_version) = meta.platform_version {
                identity.platform_version = platform_version;
            }
            if let Some(architecture) = meta.architecture {
                identity.architecture = architecture;
            }
            if let Some(model) = meta.model {
                identity.model = model;
            }
            if let Some(mobile) = meta.mobile {
                identity.mobile = mobile;
            }
            if let Some(bitness) = meta.bitness {
                identity.bitness = bitness;
            }
            if let Some(wow64) = meta.wow64 {
                identity.wow64 = wow64;
            }
            if let Some(form_factors) = meta.form_factors {
                identity.form_factors = form_factors;
            }
        }
        let valid = browser::ValidIdentity::try_from(identity)
            .map_err(|error| DispatchError::InvalidParams(error.to_string()))?;
        Some(valid)
    };
    tab.set_identity(override_identity)
        .await
        .map_err(|error| DispatchError::Failed(error.to_string()))?;
    Ok(json!({}))
}

// https://chromedevtools.github.io/devtools-protocol/tot/Emulation/#method-setTimezoneOverride
fn set_timezone_override(params: &Value) -> Result<Value, DispatchError> {
    let params: crate::params::SetTimezoneOverride = crate::params::parse(params)?;
    let _ = params.timezone_id;
    Ok(json!({}))
}

// https://chromedevtools.github.io/devtools-protocol/tot/Emulation/#method-setLocaleOverride
fn set_locale_override(params: &Value) -> Result<Value, DispatchError> {
    let params: crate::params::SetLocaleOverride = crate::params::parse(params)?;
    let _ = params.locale;
    Ok(json!({}))
}

// https://chromedevtools.github.io/devtools-protocol/tot/Emulation/#method-setGeolocationOverride
fn set_geolocation_override(params: &Value) -> Result<Value, DispatchError> {
    let params: crate::params::SetGeolocationOverride = crate::params::parse(params)?;
    if let Some(lat) = params.latitude
        && !(-90.0..=90.0).contains(&lat)
    {
        return Err(DispatchError::InvalidParams("latitude".to_owned()));
    }
    if let Some(lon) = params.longitude
        && !(-180.0..=180.0).contains(&lon)
    {
        return Err(DispatchError::InvalidParams("longitude".to_owned()));
    }
    if let Some(acc) = params.accuracy
        && acc < 0.0
    {
        return Err(DispatchError::InvalidParams("accuracy".to_owned()));
    }
    Ok(json!({}))
}

async fn set_viewport(tab: &TabHandle, width: u32, height: u32) -> Result<Value, DispatchError> {
    tab.set_viewport(width, height)
        .await
        .map_err(|error| DispatchError::Failed(error.to_string()))?;
    Ok(json!({}))
}

fn narrow_metric(value: u64, name: &str) -> Result<u32, DispatchError> {
    u32::try_from(value).map_err(|_| {
        DispatchError::Failed(format!(
            "Emulation.setDeviceMetricsOverride {name} does not fit 32 bits"
        ))
    })
}

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

async fn storage_key(tab: &TabHandle) -> Result<Value, DispatchError> {
    let reply = dom_request(tab, DomRequest::GetStorageKey).await?;
    let DomReply::StorageKey { key } = reply else {
        return Err(DispatchError::BadHelperReply("Storage.getStorageKey"));
    };
    Ok(json!({"storageKey": key}))
}

pub(crate) async fn dom_get_document(tab: &TabHandle) -> Result<Value, DispatchError> {
    let reply = dom_request(tab, DomRequest::GetDocument).await?;
    let DomReply::Document { root } = reply else {
        return Err(DispatchError::BadHelperReply("DOM.getDocument"));
    };
    Ok(json!({"root": document_node(&root)}))
}

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
    let modifiers = params.modifiers.unwrap_or(0);
    if kind == "mouseWheel" {
        let actions = json!({"actions": [{"type": "wheel", "id": "wheel", "actions": [{
            "type": "scroll", "x": params.x, "y": params.y,
            "deltaX": params.delta_x.unwrap_or(0.0),
            "deltaY": params.delta_y.unwrap_or(0.0),
            "modifiers": modifiers,
        }]}]});
        return run_actions(tab, &actions).await;
    }
    let item = match kind {
        "mousePressed" => json!({
            "type": "pointerDown",
            "x": params.x,
            "y": params.y,
            "button": button,
            "clickCount": params.click_count.unwrap_or(1),
            "modifiers": modifiers,
        }),
        "mouseReleased" => json!({
            "type": "pointerUp",
            "x": params.x,
            "y": params.y,
            "button": button,
            "clickCount": params.click_count.unwrap_or(1),
            "modifiers": modifiers,
        }),
        _ => json!({
            "type": "pointerMove",
            "origin": "viewport",
            "x": params.x,
            "y": params.y,
            "modifiers": modifiers,
        }),
    };
    let actions = json!({"actions": [{
        "type": "pointer", "id": "mouse", "parameters": {"pointerType": "mouse"},
        "actions": [item],
    }]});
    run_actions(tab, &actions).await
}

fn cdp_key_value<'a>(key: &'a str, text: &'a str, kind: &str) -> &'a str {
    if kind == "char" {
        return text;
    }
    match key {
        "Backspace" => "\u{E003}",
        "Tab" => "\u{E004}",
        "Enter" => "\u{E006}",
        "Shift" => "\u{E008}",
        "Control" => "\u{E009}",
        "Alt" => "\u{E00A}",
        "Escape" => "\u{E00C}",
        " " => "\u{E00D}",
        "PageUp" => "\u{E00E}",
        "PageDown" => "\u{E00F}",
        "End" => "\u{E010}",
        "Home" => "\u{E011}",
        "ArrowLeft" => "\u{E012}",
        "ArrowUp" => "\u{E013}",
        "ArrowRight" => "\u{E014}",
        "ArrowDown" => "\u{E015}",
        "Delete" => "\u{E017}",
        "Meta" => "\u{E03D}",
        "" => text,
        other => other,
    }
}

pub(crate) async fn input_key_event(
    tab: &TabHandle,
    params: &Value,
) -> Result<Value, DispatchError> {
    let params: crate::params::KeyEvent = crate::params::parse(params)?;
    let kind = params.kind.as_str();
    let key = params.key.as_deref().unwrap_or("");
    let text = params.text.as_deref().unwrap_or("");
    let value = cdp_key_value(key, text, kind);
    if value.is_empty() {
        return Ok(json!({}));
    }
    let down = kind != "keyUp";
    let actions = json!({"actions": [{"type": "key", "id": "keyboard", "actions": [{
        "type": if down { "keyDown" } else { "keyUp" },
        "value": value,
        "modifiers": params.modifiers.unwrap_or(0),
    }]}]});
    run_actions(tab, &actions).await
}

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

async fn run_actions(tab: &TabHandle, actions: &Value) -> Result<Value, DispatchError> {
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

async fn dom_request(tab: &TabHandle, request: DomRequest) -> Result<DomReply, DispatchError> {
    tab.dom(request)
        .await
        .map_err(|error| DispatchError::Failed(error.to_string()))
}

enum TargetNode {
    Default,
    Node(u64),
    Missing,
}

fn has_node_identifier(node: &crate::params::NodeTarget) -> bool {
    node.object.is_some() || node.backend.is_some() || node.node.is_some()
}

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

fn missing_node() -> DispatchError {
    DispatchError::Failed("Could not find node with given id".to_owned())
}

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

pub(crate) async fn dom_resolve_node(
    tab: &TabHandle,
    params: &Value,
    world: Option<&str>,
) -> Result<Value, DispatchError> {
    let target: crate::params::NodeTarget = crate::params::parse(params)?;
    let node = match target_node(tab, &target).await? {
        TargetNode::Default => 0,
        TargetNode::Node(id) => id,
        TargetNode::Missing => return Ok(json!({"object": {"type": "undefined"}})),
    };
    let reply = dom_request(
        tab,
        DomRequest::ResolveNode {
            node,
            world: world.map(str::to_owned),
        },
    )
    .await?;
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

pub(crate) async fn dom_box_model(tab: &TabHandle, params: &Value) -> Result<Value, DispatchError> {
    let target: crate::params::NodeTarget = crate::params::parse(params)?;
    let node = match target_node(tab, &target).await? {
        TargetNode::Default => 0,
        TargetNode::Node(id) => id,
        TargetNode::Missing => {
            return Err(DispatchError::Failed("Could not compute box model.".into()));
        }
    };
    let reply = dom_request(tab, DomRequest::GetBoxModel { node }).await?;
    let DomReply::BoxModel { model } = reply else {
        return Err(DispatchError::BadHelperReply("DOM.getBoxModel"));
    };
    let Some(model) = model else {
        return Err(DispatchError::Failed("Could not compute box model.".into()));
    };
    Ok(json!({
        "model": {
            "content": model.content,
            "padding": model.padding,
            "border": model.border,
            "margin": model.margin,
            "width": model.width,
            "height": model.height,
        },
    }))
}

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

// https://chromedevtools.github.io/devtools-protocol/tot/DOM/#method-scrollIntoViewIfNeeded
pub(crate) async fn dom_scroll_into_view_if_needed(
    tab: &TabHandle,
    params: &Value,
) -> Result<Value, DispatchError> {
    let target: crate::params::NodeTarget = crate::params::parse(params)?;
    let node = match target_node(tab, &target).await? {
        TargetNode::Default => 0,
        TargetNode::Node(id) => id,
        TargetNode::Missing => return Ok(json!({})),
    };
    let reply = dom_request(tab, DomRequest::ScrollIntoViewIfNeeded { node }).await?;
    let DomReply::Unit = reply else {
        return Err(DispatchError::BadHelperReply("DOM.scrollIntoViewIfNeeded"));
    };
    Ok(json!({}))
}

async fn resolve_target_object_id(
    tab: &TabHandle,
    target: &crate::params::NodeTarget,
) -> Result<String, DispatchError> {
    if let Some(object_id) = &target.object {
        return Ok(object_id.clone());
    }
    let node = match target_node(tab, target).await? {
        TargetNode::Node(id) => id,
        TargetNode::Default | TargetNode::Missing => return Err(missing_node()),
    };
    let reply = dom_request(tab, DomRequest::ResolveNode { node, world: None }).await?;
    let DomReply::Resolved { object } = reply else {
        return Err(DispatchError::BadHelperReply("DOM.resolveNode"));
    };
    object.map(|obj| obj.object_id).ok_or_else(missing_node)
}

// https://chromedevtools.github.io/devtools-protocol/tot/DOM/#method-focus
async fn dom_focus(tab: &TabHandle, params: &Value) -> Result<Value, DispatchError> {
    let target: crate::params::NodeTarget = crate::params::parse(params)?;
    if !has_node_identifier(&target) {
        return Err(DispatchError::InvalidParams(
            "nodeId, backendNodeId, or objectId".to_owned(),
        ));
    }
    let object_id = resolve_target_object_id(tab, &target).await?;
    let script = format!(
        "(() => {{ const el = host.__tb_handles[{}]; if (!el || typeof el.focus !== 'function') return false; el.focus(); return true; }})()",
        json_string(&object_id)
    );
    tab.execute_browser_script(&script)
        .await
        .map_err(|error| DispatchError::Failed(error.to_string()))?;
    Ok(json!({}))
}

// https://chromedevtools.github.io/devtools-protocol/tot/DOM/#method-setAttributeValue
async fn dom_set_attribute_value(tab: &TabHandle, params: &Value) -> Result<Value, DispatchError> {
    let params: crate::params::DomSetAttributeValue = crate::params::parse(params)?;
    let target = crate::params::NodeTarget {
        node: Some(params.node_id),
        backend: None,
        object: None,
    };
    let object_id = resolve_target_object_id(tab, &target).await?;
    let script = format!(
        "(() => {{ const el = host.__tb_handles[{}]; if (!el || typeof el.setAttribute !== 'function') return false; el.setAttribute({}, {}); return true; }})()",
        json_string(&object_id),
        json_string(&params.name),
        json_string(&params.value),
    );
    tab.execute_browser_script(&script)
        .await
        .map_err(|error| DispatchError::Failed(error.to_string()))?;
    Ok(json!({}))
}

// https://chromedevtools.github.io/devtools-protocol/tot/DOM/#method-removeAttribute
async fn dom_remove_attribute(tab: &TabHandle, params: &Value) -> Result<Value, DispatchError> {
    let params: crate::params::DomRemoveAttribute = crate::params::parse(params)?;
    let target = crate::params::NodeTarget {
        node: Some(params.node_id),
        backend: None,
        object: None,
    };
    let object_id = resolve_target_object_id(tab, &target).await?;
    let script = format!(
        "(() => {{ const el = host.__tb_handles[{}]; if (!el || typeof el.removeAttribute !== 'function') return false; el.removeAttribute({}); return true; }})()",
        json_string(&object_id),
        json_string(&params.name),
    );
    tab.execute_browser_script(&script)
        .await
        .map_err(|error| DispatchError::Failed(error.to_string()))?;
    Ok(json!({}))
}

// https://chromedevtools.github.io/devtools-protocol/tot/DOM/#method-removeNode
async fn dom_remove_node(tab: &TabHandle, params: &Value) -> Result<Value, DispatchError> {
    let params: crate::params::DomRemoveNode = crate::params::parse(params)?;
    let target = crate::params::NodeTarget {
        node: Some(params.node_id),
        backend: None,
        object: None,
    };
    let object_id = resolve_target_object_id(tab, &target).await?;
    let script = format!(
        "(() => {{ const el = host.__tb_handles[{}]; if (!el || !el.parentNode) return false; el.parentNode.removeChild(el); return true; }})()",
        json_string(&object_id),
    );
    tab.execute_browser_script(&script)
        .await
        .map_err(|error| DispatchError::Failed(error.to_string()))?;
    Ok(json!({}))
}

// https://chromedevtools.github.io/devtools-protocol/tot/DOM/#method-setFileInputFiles
async fn dom_set_file_input_files(tab: &TabHandle, params: &Value) -> Result<Value, DispatchError> {
    let params: crate::params::DomSetFileInputFiles = crate::params::parse(params)?;
    if !has_node_identifier(&params.target) {
        return Err(DispatchError::InvalidParams(
            "nodeId, backendNodeId, or objectId".to_owned(),
        ));
    }
    let object_id = resolve_target_object_id(tab, &params.target).await?;
    let names: Vec<String> = params
        .files
        .iter()
        .map(|raw| {
            Path::new(raw)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(raw)
                .to_owned()
        })
        .collect();
    let script = format!(
        "(() => {{\n\
           const el = host.__tb_handles[{}];\n\
           if (!el || el.nodeType !== 1) return false;\n\
           const names = {};\n\
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
           return true;\n\
         }})()",
        json_string(&object_id),
        json!(names),
    );
    tab.execute_browser_script(&script)
        .await
        .map_err(|error| DispatchError::Failed(error.to_string()))?;
    Ok(json!({}))
}

// https://chromedevtools.github.io/devtools-protocol/tot/Network/#type-Cookie
pub(crate) fn cdp_cookie_json(record: &CookieRecord) -> Value {
    let expires = record.expiry.map_or(-1.0, |exp| {
        exp.duration_since(UNIX_EPOCH)
            .map_or(0.0, |d| d.as_secs_f64())
    });
    let same_site = match record.same_site {
        CookieSameSite::Strict => "Strict",
        CookieSameSite::Lax => "Lax",
        CookieSameSite::None | CookieSameSite::Default => "None",
    };
    json!({
        "name": record.name,
        "value": record.value,
        "domain": record.domain,
        "path": record.path,
        "expires": expires,
        "size": record.name.len() + record.value.len(),
        "httpOnly": record.http_only,
        "secure": record.secure,
        "session": record.expiry.is_none(),
        "sameSite": same_site,
        "priority": "Medium",
        "sameParty": false,
        "sourceScheme": if record.secure { "Secure" } else { "NonSecure" },
        "sourcePort": if record.secure { 443 } else { 80 },
    })
}

pub(crate) fn cookie_param_to_direct(
    param: &crate::params::CookieParam,
    fallback_url: Option<&str>,
) -> Result<DirectCookie, DispatchError> {
    let parsed_url = param
        .url
        .as_deref()
        .or(fallback_url)
        .and_then(|u| url::Url::parse(u).ok());
    let (domain, host_only) = if let Some(ref d) = param.domain {
        let stripped = d.strip_prefix('.').unwrap_or(d);
        (stripped.to_ascii_lowercase(), !d.starts_with('.'))
    } else if let Some(host) = parsed_url.as_ref().and_then(url::Url::host_str) {
        (host.to_ascii_lowercase(), true)
    } else {
        return Err(DispatchError::InvalidParams(
            "cookie requires domain or url".to_owned(),
        ));
    };
    let path = param
        .path
        .clone()
        .or_else(|| parsed_url.as_ref().map(|u| u.path().to_owned()))
        .unwrap_or_else(|| "/".to_owned());
    let secure = param
        .secure
        .unwrap_or_else(|| parsed_url.as_ref().is_some_and(|u| u.scheme() == "https"));
    let http_only = param.http_only.unwrap_or(false);
    let same_site = match param.same_site.as_deref() {
        Some("Strict") => CookieSameSite::Strict,
        Some("Lax") => CookieSameSite::Lax,
        Some("None") => CookieSameSite::None,
        _ => CookieSameSite::Default,
    };
    let expiry = param.expires.and_then(|ts| {
        if ts < 0.0 || !ts.is_finite() {
            None
        } else {
            Duration::try_from_secs_f64(ts)
                .ok()
                .and_then(|d| UNIX_EPOCH.checked_add(d))
        }
    });
    Ok(DirectCookie {
        name: param.name.clone(),
        value: param.value.clone(),
        domain,
        host_only,
        path,
        secure,
        http_only,
        same_site,
        expiry,
        partition_key: None,
    })
}

pub(crate) async fn set_cdp_cookies(
    browser: &BrowserHandle,
    user_context: &UserContextId,
    cookies: &[crate::params::CookieParam],
    fallback_url: Option<&str>,
) -> Result<bool, DispatchError> {
    let mut all_ok = true;
    for param in cookies {
        let direct = cookie_param_to_direct(param, fallback_url)?;
        let stored = browser
            .set_direct_cookie_in(user_context, direct)
            .await
            .map_err(|error| DispatchError::Failed(error.to_string()))?;
        all_ok &= stored;
    }
    Ok(all_ok)
}

pub(crate) async fn delete_cdp_cookies(
    browser: &BrowserHandle,
    user_context: &UserContextId,
    params: &crate::params::DeleteCookies,
) -> Result<(), DispatchError> {
    let parsed_url = params.url.as_deref().and_then(|u| url::Url::parse(u).ok());
    let domain = params.domain.clone().or_else(|| {
        parsed_url
            .as_ref()
            .and_then(url::Url::host_str)
            .map(str::to_ascii_lowercase)
    });
    let filter = CookieFilter {
        name: Some(params.name.clone()),
        domain,
        path: params.path.clone(),
        ..CookieFilter::default()
    };
    browser
        .delete_cookies_in(user_context, filter)
        .await
        .map_err(|error| DispatchError::Failed(error.to_string()))?;
    Ok(())
}

pub(crate) async fn css_stylesheets(tab: &TabHandle) -> Result<Vec<Value>, DispatchError> {
    let reply = dom_request(tab, DomRequest::GetStyleSheets).await?;
    let DomReply::StyleSheets { sheets } = reply else {
        return Err(DispatchError::BadHelperReply("CSS.enable"));
    };
    Ok(sheets
        .into_iter()
        .map(|sheet| {
            json!({
                "styleSheetId": sheet.style_sheet_id,
                "sourceURL": sheet.source_url,
                "origin": "regular",
                "title": sheet.title,
                "disabled": sheet.disabled,
                "isInline": sheet.is_inline,
                "startLine": 0,
                "startColumn": 0,
                "endLine": 0,
                "endColumn": 0,
                "length": sheet.length,
            })
        })
        .collect())
}

pub(crate) async fn css_computed_style(
    tab: &TabHandle,
    params: &Value,
) -> Result<Value, DispatchError> {
    let params: crate::params::ComputedStyleForNode = crate::params::parse(params)?;
    let reply = dom_request(
        tab,
        DomRequest::GetComputedStyle {
            node: params.node_id,
        },
    )
    .await?;
    let DomReply::ComputedStyle { properties } = reply else {
        return Err(DispatchError::BadHelperReply("CSS.getComputedStyleForNode"));
    };
    let computed_style: Vec<Value> = properties
        .into_iter()
        .map(|(name, value)| json!({"name": name, "value": value}))
        .collect();
    Ok(json!({"computedStyle": computed_style}))
}

// https://chromedevtools.github.io/devtools-protocol/tot/Accessibility/#method-getFullAXTree
pub(crate) async fn accessibility_full_tree(tab: &TabHandle) -> Result<Value, DispatchError> {
    let reply = dom_request(tab, DomRequest::GetFullAxTree).await?;
    let DomReply::AxTree { nodes } = reply else {
        return Err(DispatchError::BadHelperReply("Accessibility.getFullAXTree"));
    };
    let nodes: Vec<Value> = nodes.iter().map(ax_node_json).collect();
    Ok(json!({"nodes": nodes}))
}

fn ax_node_json(node: &AxNode) -> Value {
    let mut entry = serde_json::Map::new();
    entry.insert("nodeId".to_owned(), json!(node.node_id));
    entry.insert("ignored".to_owned(), json!(node.ignored));
    entry.insert(
        "role".to_owned(),
        json!({"type": "role", "value": node.role}),
    );
    if let Some(name) = &node.name {
        entry.insert(
            "name".to_owned(),
            json!({"type": "computedString", "value": name}),
        );
    }
    if let Some(value) = &node.value {
        entry.insert(
            "value".to_owned(),
            json!({"type": "string", "value": value}),
        );
    }
    if !node.properties.is_empty() {
        let props: Vec<Value> = node
            .properties
            .iter()
            .map(|prop| {
                let value = match &prop.value {
                    AxPropertyValue::Bool(flag) => json!({"type": "boolean", "value": flag}),
                    AxPropertyValue::Int(num) => json!({"type": "integer", "value": num}),
                    AxPropertyValue::String(text) => json!({"type": "string", "value": text}),
                };
                json!({"name": prop.name, "value": value})
            })
            .collect();
        entry.insert("properties".to_owned(), Value::Array(props));
    }
    if let Some(parent_id) = &node.parent_id {
        entry.insert("parentId".to_owned(), json!(parent_id));
    }
    if !node.child_ids.is_empty() {
        entry.insert("childIds".to_owned(), json!(node.child_ids));
    }
    if let Some(backend_id) = node.backend_dom_node_id {
        entry.insert("backendDOMNodeId".to_owned(), json!(backend_id));
    }
    Value::Object(entry)
}

fn viewport_rect(width: f64, height: f64) -> Value {
    json!({"pageX": 0, "pageY": 0, "clientWidth": width, "clientHeight": height})
}

pub(crate) async fn capture_screenshot(
    tab: &TabHandle,
    params: &Value,
    omit_background: bool,
) -> Result<Value, DispatchError> {
    let params: crate::params::CaptureScreenshot = crate::params::parse(params)?;
    let format = match params.format.as_deref().unwrap_or("png") {
        s if s.eq_ignore_ascii_case("png") => ScreenshotFormat::Png,
        s if s.eq_ignore_ascii_case("jpeg") => ScreenshotFormat::Jpeg {
            quality: params.quality.unwrap_or(80),
        },
        s if s.eq_ignore_ascii_case("webp") => ScreenshotFormat::Webp,
        other => {
            return Err(DispatchError::Failed(format!(
                "unsupported screenshot format: {other}"
            )));
        }
    };
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
        format,
        omit_background,
    };
    let frame = requested_frame(tab, params.frame_id.as_deref())?;
    let png = tab
        .screenshot_frame(frame, request)
        .await
        .map_err(|error| DispatchError::Failed(error.to_string()))?;
    Ok(json!({"data": BASE64_STANDARD.encode(&png)}))
}

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

pub(crate) async fn wait_for_navigation(
    mut events: mpsc::Receiver<TabEvent>,
    duration: Duration,
) -> Result<(), &'static str> {
    let result = tokio::time::timeout(duration, async {
        loop {
            match events.recv().await {
                Some(TabEvent::Navigated) => return Ok(()),
                Some(TabEvent::NavigationFailed(reason)) => {
                    return Err(match reason {
                        browser::NavigationFailure::Aborted => "net::ERR_ABORTED",
                        browser::NavigationFailure::ConnectionRefused => {
                            "net::ERR_CONNECTION_REFUSED"
                        }
                        browser::NavigationFailure::CertAuthorityInvalid => {
                            "net::ERR_CERT_AUTHORITY_INVALID"
                        }
                        browser::NavigationFailure::Failed => "net::ERR_FAILED",
                    });
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
    open_url_with_referrer(tab, url, None).await
}

pub(crate) async fn open_url_with_referrer(
    tab: &TabHandle,
    url: &str,
    referrer: Option<&str>,
) -> Result<(), DispatchError> {
    if url.is_empty() {
        tab.load_html("<!doctype html><title></title>")
            .await
            .map_err(|err| DispatchError::Failed(err.to_string()))?;
        return Ok(());
    }
    tab.goto_with_referrer(url, referrer)
        .await
        .map_err(|err| match err {
            TabError::InvalidUrl { .. } => {
                DispatchError::Failed("Cannot navigate to invalid URL".to_owned())
            }
            other => DispatchError::Failed(other.to_string()),
        })?;
    Ok(())
}

pub(crate) fn arguments_expression(arguments: &[CallArgument]) -> String {
    let parts: Vec<String> = arguments.iter().map(argument_expression).collect();
    format!("[{}]", parts.join(", "))
}

pub(crate) fn missing_handles_expression(
    object_id: Option<&str>,
    arguments: &[CallArgument],
) -> String {
    let mut checks = Vec::new();
    let mut check = |id: &str| {
        checks.push(format!(
            "host.__tb_handles[{}] === undefined",
            json_string(id)
        ));
    };
    if let Some(id) = object_id {
        check(id);
    }
    for argument in arguments {
        if let CallArgument::Object { object_id } = argument {
            check(object_id);
        }
    }
    if checks.is_empty() {
        "false".to_owned()
    } else {
        checks.join(" || ")
    }
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

pub(crate) fn json_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_owned())
}

pub(crate) fn exception_reply(error: &TabError) -> Value {
    json!({
        "result": {"type": "undefined"},
        "exceptionDetails": {"text": error.to_string()},
    })
}

pub(crate) fn exception_text_reply(text: &str) -> Value {
    json!({
        "result": {"type": "undefined"},
        "exceptionDetails": {"text": text},
    })
}

pub(crate) const RUNTIME_SCHEDULE: &str = r#"(() => {
  if (__MISSING__) return "missing-handle";
  const settle = (id, make) => {
    let text;
    try { text = host.stringify(make()); }
    catch (e) { text = host.stringify({ __proto__: null, error: String((e && e.message) || e) }); }
    host.__tbCompleteEvaluation(id, text);
  };
  const id = __ID__;
  Promise.resolve((__SOURCE__)).then(
    (v) => settle(id, () => (__MAKE__)),
    (e) => settle(id, () => ({ __proto__: null, error: String((e && e.message) || e) })),
  );
  return "scheduled";
})()"#;

pub(crate) const RUNTIME_HANDLE: &str = r#"(() => {
  if (__MISSING__) return "missing-handle";
  const v = (__SOURCE__);
  return host.stringify(host.remoteObject(v, { __proto__: null, objectGroup: __GROUP__ }));
})()"#;

// https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#method-evaluate — replies live outside the realm: a document.open() step replaces it mid-evaluation.
pub(crate) const RUNTIME_READ: &str = r"(() => {
  const text = host.__tbTakeEvaluation(__ID__);
  return (text === null || text === undefined)
    ? host.stringify({ __proto__: null, pending: true })
    : text;
})()";

pub(crate) const RUNTIME_RELEASE: &str = r#"(() => {
  const args = __ARGS__;
  if (args.objectGroup !== undefined) host.__tb_release_group(args.objectGroup);
  else host.__tb_release_handle(args.objectId);
  return "released";
})()"#;

pub(crate) const RUNTIME_CLEAR_HANDLES: &str = "host.__tb_clear_handles()";

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

pub(crate) async fn target_info(
    browser: &BrowserHandle,
    id: TabId,
    browser_context_id: &str,
) -> Value {
    let (url, title, opener) = match browser.tab(id).await {
        Ok(tab) => {
            let url = tab
                .document_url()
                .await
                .unwrap_or_else(|_| "about:blank".into());
            let title = tab.document_title().await.unwrap_or_default();
            let opener = tab.opener().await.ok().flatten();
            (url, title, opener)
        }
        Err(_) => ("about:blank".into(), String::new(), None),
    };
    let mut info = json!({
        "targetId": id.to_string(),
        "type": "page",
        "title": title,
        "url": url,
        "attached": false,
        "canAccessOpener": opener.is_some(),
        "browserContextId": browser_context_id,
    });
    if let Some(op) = opener
        && let Some(obj) = info.as_object_mut()
    {
        obj.insert("openerId".into(), json!(op.to_string()));
    }
    info
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
    InvalidParams(String),
    BadHelperReply(&'static str),
    Failed(String),
}

impl DispatchError {
    pub(crate) fn code(&self) -> i32 {
        match self {
            Self::MethodNotFound => -32601,
            Self::InvalidParams(_) => -32602,
            Self::BadHelperReply(_) | Self::Failed(_) => -32000,
        }
    }

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
