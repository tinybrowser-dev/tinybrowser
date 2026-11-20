//! One params struct per CDP method.
//!
//! The protocol names fields in camelCase and marks some optional. Decoding
//! each request into its own struct means a typo in a required field, or a
//! wrong type, is an invalid-params error at the boundary instead of a silent
//! default deep in a handler. A typo in an optional field is still dropped:
//! refusing unknown fields would break forward compatibility with clients
//! that send newer protocol fields.
//!
//! Optional fields stay `Option` rather than carrying a hidden `serde`
//! default: the handler that needs a value decides what a missing one means,
//! and that decision is visible where it is used.

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use browser::ScreenshotClip;

use crate::dispatch::DispatchError;

/// Decodes one request's params, turning a serde failure into the protocol's
/// invalid-params error.
pub(crate) fn parse<T: DeserializeOwned>(params: &Value) -> Result<T, DispatchError> {
    // `Value` is a deserializer by reference, so the request is decoded in
    // place instead of copied first.
    T::deserialize(params).map_err(|error| DispatchError::InvalidParams(error.to_string()))
}

/// `Target.setAutoAttach`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SetAutoAttach {
    pub(crate) auto_attach: bool,
}

/// `Target.attachToTarget`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AttachToTarget {
    pub(crate) target_id: String,
    pub(crate) flatten: Option<bool>,
}

/// `Target.createTarget`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CreateTarget {
    pub(crate) url: String,
}

/// `Target.closeTarget`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CloseTarget {
    pub(crate) target_id: String,
}

/// `Target.detachFromTarget`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DetachFromTarget {
    pub(crate) session_id: Option<String>,
}

/// `Target.getTargetInfo`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GetTargetInfo {
    pub(crate) target_id: Option<String>,
}

/// `Page.navigate`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Navigate {
    pub(crate) url: String,
}

/// `Page.navigateToHistoryEntry`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NavigateToHistoryEntry {
    pub(crate) entry_id: u64,
}

/// `Page.captureScreenshot`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CaptureScreenshot {
    pub(crate) format: Option<String>,
    /// The renderer's own crop window, so the protocol spelling and the
    /// renderer's type are one definition. `scale` is tolerated as an unknown
    /// key: the renderer rasters at device scale.
    pub(crate) clip: Option<ScreenshotClip>,
    pub(crate) frame_id: Option<String>,
}

/// `Page.createIsolatedWorld`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CreateIsolatedWorld {
    pub(crate) frame_id: Option<String>,
    pub(crate) world_name: Option<String>,
}

/// `Page.setLifecycleEventsEnabled`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SetLifecycleEventsEnabled {
    pub(crate) enabled: bool,
}

/// `Page.addScriptToEvaluateOnNewDocument`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AddInitScript {
    pub(crate) source: String,
    pub(crate) run_immediately: Option<bool>,
}

/// `Page.removeScriptToEvaluateOnNewDocument`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RemoveInitScript {
    pub(crate) identifier: String,
}

/// `Emulation.setDeviceMetricsOverride`. The protocol requires integer
/// `width`/`height`; zero on either side clears the override.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SetDeviceMetrics {
    pub(crate) width: u64,
    pub(crate) height: u64,
    pub(crate) mobile: Option<bool>,
    pub(crate) device_scale_factor: Option<f64>,
}

/// One `Emulation.setEmulatedMedia` feature. Only its value decides whether
/// the request selects the engine defaults; the media query name is not
/// evaluated because the engine has one media table.
#[derive(Debug, Deserialize)]
pub(crate) struct MediaFeature {
    pub(crate) value: String,
}

/// `Emulation.setEmulatedMedia`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SetEmulatedMedia {
    pub(crate) media: Option<String>,
    pub(crate) features: Option<Vec<MediaFeature>>,
}

/// `Network.setCacheDisabled`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SetCacheDisabled {
    pub(crate) cache_disabled: bool,
}

/// The settings `Runtime.evaluate` and `Runtime.callFunctionOn` share. Every
/// field is optional in the protocol; the handler decides each default.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RuntimeOptions {
    pub(crate) return_by_value: Option<bool>,
    pub(crate) await_promise: Option<bool>,
    pub(crate) object_group: Option<String>,
    pub(crate) timeout: Option<f64>,
    pub(crate) frame_id: Option<String>,
}

/// `Runtime.evaluate`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RuntimeEvaluate {
    pub(crate) expression: String,
    #[serde(flatten)]
    pub(crate) options: RuntimeOptions,
}

/// `Runtime.callFunctionOn`. Each argument is a handle, a literal, or an
/// unserializable primitive, so the array is typed and the script builder
/// translates each variant once.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RuntimeCallFunctionOn {
    pub(crate) function_declaration: String,
    pub(crate) object_id: Option<String>,
    pub(crate) arguments: Option<Vec<CallArgument>>,
    #[serde(flatten)]
    pub(crate) options: RuntimeOptions,
}

/// One `Runtime.callFunctionOn` argument.
///
/// The protocol's `CallArgument` is a union, not a bag: a handle, a JSON
/// literal, or one of the primitives JSON cannot carry (`NaN`, `±Infinity`,
/// `-0`, a `BigInt`). `Empty` keeps the object's own leniency for `{}` and for
/// fields this adapter does not read.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum CallArgument {
    /// A remote handle the page already holds.
    Object {
        #[serde(rename = "objectId")]
        object_id: String,
    },
    /// A primitive JSON cannot represent.
    Unserializable {
        #[serde(rename = "unserializableValue")]
        value: String,
    },
    /// A JSON literal.
    Value { value: Value },
    /// No value: Python's `None`/JS `undefined` through a client that sends an
    /// empty object.
    Empty {},
}

/// `Runtime.getProperties`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GetProperties {
    pub(crate) object_id: String,
    pub(crate) own_properties: Option<bool>,
    pub(crate) non_indexed_properties_only: Option<bool>,
    pub(crate) accessor_properties_only: Option<bool>,
    pub(crate) generate_preview: Option<bool>,
}

/// `Runtime.releaseObject`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReleaseObject {
    pub(crate) object_id: String,
}

/// `Runtime.releaseObjectGroup`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReleaseObjectGroup {
    pub(crate) object_group: String,
}

/// A DOM method that addresses one node. The protocol lets a caller name it by
/// `nodeId`, `backendNodeId`, or `objectId`; the shape is shared because the
/// resolution rule is.
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

/// `DOM.querySelector` / `DOM.querySelectorAll`. Both require the node.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct QuerySelector {
    pub(crate) node_id: u64,
    pub(crate) selector: String,
}

/// `DOM.getNodeForLocation`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NodeForLocation {
    pub(crate) x: f64,
    pub(crate) y: f64,
}

/// `CSS.getComputedStyleForNode`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ComputedStyleForNode {
    pub(crate) node_id: u64,
}

/// `Input.dispatchMouseEvent`.
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

/// `Input.dispatchKeyEvent`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct KeyEvent {
    #[serde(rename = "type")]
    pub(crate) kind: String,
    pub(crate) key: Option<String>,
    pub(crate) text: Option<String>,
}

/// `Input.insertText`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InsertText {
    pub(crate) text: String,
}

/// One `Input.dispatchTouchEvent` point.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TouchPoint {
    pub(crate) x: f64,
    pub(crate) y: f64,
}

/// `Input.dispatchTouchEvent`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TouchEvent {
    #[serde(rename = "type")]
    pub(crate) kind: String,
    pub(crate) touch_points: Vec<TouchPoint>,
}

/// `Input.emulateTouchFromMouseEvent`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EmulateTouchFromMouse {
    #[serde(rename = "type")]
    pub(crate) kind: String,
    pub(crate) x: f64,
    pub(crate) y: f64,
}
