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
pub(crate) struct AttachToTarget {
    pub(crate) target_id: String,
    pub(crate) flatten: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CreateTarget {
    pub(crate) url: String,
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
