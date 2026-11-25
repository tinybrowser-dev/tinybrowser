use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use browser::ScreenshotClip;

use crate::dispatch::DispatchError;

pub(crate) fn parse<T: DeserializeOwned>(params: &Value) -> Result<T, DispatchError> {
    T::deserialize(params).map_err(|error| DispatchError::InvalidParams(error.to_string()))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SetAutoAttach {
    pub(crate) auto_attach: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SetDiscoverTargets {
    pub(crate) discover: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SetDocumentContent {
    pub(crate) frame_id: String,
    pub(crate) html: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HandleJavaScriptDialog {
    pub(crate) accept: bool,
    pub(crate) prompt_text: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GetCookies {
    pub(crate) urls: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CookieParam {
    pub(crate) name: String,
    pub(crate) value: String,
    pub(crate) url: Option<String>,
    pub(crate) domain: Option<String>,
    pub(crate) path: Option<String>,
    pub(crate) secure: Option<bool>,
    pub(crate) http_only: Option<bool>,
    pub(crate) same_site: Option<String>,
    pub(crate) expires: Option<f64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SetCookies {
    pub(crate) cookies: Vec<CookieParam>,
    pub(crate) browser_context_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DeleteCookies {
    pub(crate) name: String,
    pub(crate) url: Option<String>,
    pub(crate) domain: Option<String>,
    pub(crate) path: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StorageBrowserContext {
    pub(crate) browser_context_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SetBlockedUrls {
    pub(crate) urls: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FetchRequestPattern {
    pub(crate) url_pattern: Option<String>,
    pub(crate) request_stage: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FetchEnable {
    pub(crate) patterns: Option<Vec<FetchRequestPattern>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FetchHeaderEntry {
    pub(crate) name: String,
    pub(crate) value: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FetchContinueRequest {
    pub(crate) request_id: String,
    pub(crate) url: Option<String>,
    pub(crate) method: Option<String>,
    pub(crate) post_data: Option<String>,
    pub(crate) headers: Option<Vec<FetchHeaderEntry>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FetchContinueResponse {
    pub(crate) request_id: String,
    pub(crate) response_code: Option<u16>,
    pub(crate) response_phrase: Option<String>,
    pub(crate) response_headers: Option<Vec<FetchHeaderEntry>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FetchFulfillRequest {
    pub(crate) request_id: String,
    pub(crate) response_code: u16,
    pub(crate) response_headers: Option<Vec<FetchHeaderEntry>>,
    pub(crate) body: Option<String>,
    pub(crate) response_phrase: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FetchFailRequest {
    pub(crate) request_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CompileScript {
    pub(crate) expression: String,
    pub(crate) persist_script: Option<bool>,
    pub(crate) execution_context_id: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RunScript {
    pub(crate) script_id: String,
    #[serde(flatten)]
    pub(crate) options: RuntimeOptions,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DomSetAttributeValue {
    pub(crate) node_id: u64,
    pub(crate) name: String,
    pub(crate) value: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DomRemoveAttribute {
    pub(crate) node_id: u64,
    pub(crate) name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DomRemoveNode {
    pub(crate) node_id: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DomSetFileInputFiles {
    pub(crate) files: Vec<String>,
    #[serde(flatten)]
    pub(crate) target: NodeTarget,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SetTimezoneOverride {
    pub(crate) timezone_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SetLocaleOverride {
    pub(crate) locale: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SetGeolocationOverride {
    pub(crate) latitude: Option<f64>,
    pub(crate) longitude: Option<f64>,
    pub(crate) accuracy: Option<f64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AttachToTarget {
    pub(crate) target_id: String,
    pub(crate) flatten: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CreateTarget {
    pub(crate) url: String,
    pub(crate) browser_context_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DisposeBrowserContext {
    pub(crate) browser_context_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CloseTarget {
    pub(crate) target_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DetachFromTarget {
    pub(crate) session_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GetTargetInfo {
    pub(crate) target_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Navigate {
    pub(crate) url: String,
    pub(crate) referrer: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RgbaColor {
    pub(crate) a: Option<f64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DefaultBackgroundColorOverride {
    pub(crate) color: Option<RgbaColor>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GetResponseBody {
    pub(crate) request_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SetExtraHttpHeaders {
    pub(crate) headers: serde_json::Map<String, Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NavigateToHistoryEntry {
    pub(crate) entry_id: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CaptureScreenshot {
    pub(crate) format: Option<String>,
    pub(crate) quality: Option<u8>,
    pub(crate) clip: Option<ScreenshotClip>,
    pub(crate) frame_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CreateIsolatedWorld {
    pub(crate) frame_id: Option<String>,
    pub(crate) world_name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SetLifecycleEventsEnabled {
    pub(crate) enabled: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AddInitScript {
    pub(crate) source: String,
    pub(crate) run_immediately: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AddBinding {
    pub(crate) name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RemoveInitScript {
    pub(crate) identifier: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SetDeviceMetrics {
    pub(crate) width: u64,
    pub(crate) height: u64,
    pub(crate) mobile: Option<bool>,
    pub(crate) device_scale_factor: Option<f64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct MediaFeature {
    #[serde(default)]
    pub(crate) name: String,
    pub(crate) value: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SetEmulatedMedia {
    pub(crate) media: Option<String>,
    pub(crate) features: Option<Vec<MediaFeature>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SetCacheDisabled {
    pub(crate) cache_disabled: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RuntimeOptions {
    pub(crate) return_by_value: Option<bool>,
    pub(crate) await_promise: Option<bool>,
    pub(crate) object_group: Option<String>,
    pub(crate) timeout: Option<f64>,
    pub(crate) frame_id: Option<String>,
    pub(crate) context_id: Option<u64>,
    pub(crate) execution_context_id: Option<u64>,
    #[serde(rename = "allowUnsafeEvalBlockedByCSP")]
    pub(crate) allow_unsafe_eval_blocked_by_csp: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RuntimeEvaluate {
    pub(crate) expression: String,
    #[serde(flatten)]
    pub(crate) options: RuntimeOptions,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RuntimeCallFunctionOn {
    pub(crate) function_declaration: String,
    pub(crate) object_id: Option<String>,
    pub(crate) arguments: Option<Vec<CallArgument>>,
    #[serde(flatten)]
    pub(crate) options: RuntimeOptions,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum CallArgument {
    Object {
        #[serde(rename = "objectId")]
        object_id: String,
    },
    Unserializable {
        #[serde(rename = "unserializableValue")]
        value: String,
    },
    Value {
        value: Value,
    },
    Empty {},
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GetProperties {
    pub(crate) object_id: String,
    pub(crate) own_properties: Option<bool>,
    pub(crate) non_indexed_properties_only: Option<bool>,
    pub(crate) accessor_properties_only: Option<bool>,
    pub(crate) generate_preview: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReleaseObject {
    pub(crate) object_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReleaseObjectGroup {
    pub(crate) object_group: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NodeTarget {
    #[serde(rename = "nodeId")]
    pub(crate) node: Option<u64>,
    #[serde(rename = "backendNodeId")]
    pub(crate) backend: Option<u64>,
    #[serde(rename = "objectId")]
    pub(crate) object: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct QuerySelector {
    pub(crate) node_id: u64,
    pub(crate) selector: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NodeForLocation {
    pub(crate) x: f64,
    pub(crate) y: f64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ComputedStyleForNode {
    pub(crate) node_id: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MouseEvent {
    #[serde(rename = "type")]
    pub(crate) kind: String,
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) button: Option<String>,
    pub(crate) click_count: Option<u32>,
    pub(crate) modifiers: Option<u32>,
    pub(crate) delta_x: Option<f64>,
    pub(crate) delta_y: Option<f64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct KeyEvent {
    #[serde(rename = "type")]
    pub(crate) kind: String,
    pub(crate) key: Option<String>,
    pub(crate) text: Option<String>,
    pub(crate) modifiers: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InsertText {
    pub(crate) text: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TouchPoint {
    pub(crate) x: f64,
    pub(crate) y: f64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TouchEvent {
    #[serde(rename = "type")]
    pub(crate) kind: String,
    pub(crate) touch_points: Vec<TouchPoint>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EmulateTouchFromMouse {
    #[serde(rename = "type")]
    pub(crate) kind: String,
    pub(crate) x: f64,
    pub(crate) y: f64,
}

// https://chromedevtools.github.io/devtools-protocol/tot/Emulation/#type-UserAgentBrandVersion
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UserAgentBrandVersion {
    pub(crate) brand: String,
    pub(crate) version: String,
}

// https://chromedevtools.github.io/devtools-protocol/tot/Emulation/#type-UserAgentMetadata
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UserAgentMetadata {
    pub(crate) brands: Option<Vec<UserAgentBrandVersion>>,
    pub(crate) full_version_list: Option<Vec<UserAgentBrandVersion>>,
    pub(crate) platform: Option<String>,
    pub(crate) platform_version: Option<String>,
    pub(crate) architecture: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) mobile: Option<bool>,
    pub(crate) bitness: Option<String>,
    pub(crate) wow64: Option<bool>,
    pub(crate) form_factors: Option<Vec<String>>,
}

// https://chromedevtools.github.io/devtools-protocol/tot/Emulation/#method-setUserAgentOverride
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UserAgentOverride {
    pub(crate) user_agent: String,
    pub(crate) accept_language: Option<String>,
    pub(crate) platform: Option<String>,
    pub(crate) user_agent_metadata: Option<UserAgentMetadata>,
}
