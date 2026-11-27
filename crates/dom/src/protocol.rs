use std::fmt;

use serde::{Deserialize, Serialize};
use url::Url;

pub use net::fetch::{
    BodyDelivery, CookiePolicy, CorsMode, CorsRequest, DialFailure, DialKind, DialOutcome,
    DialRequest, FrameId, MAX_RESPONSE_BODY_BYTES, ReferrerRequest, RequestCache, RequestPolicy,
    ResponseBody, SandboxFlag, SandboxFlags, SerializedOrigin, StreamId, cross_origin_credentials,
};
pub use webstorage::{STORAGE_QUOTA_BYTES, StorageChange, StorageError, StorageKey, StorageKind};

use net::cors::CrossOrigin;

// https://html.spec.whatwg.org/multipage/urls-and-fetching.html#cors-settings-attributes
#[must_use]
pub fn cross_origin(setting: Option<blitz_traits::net::CorsSetting>) -> Option<CrossOrigin> {
    setting.map(|setting| match setting {
        blitz_traits::net::CorsSetting::Anonymous => CrossOrigin::Anonymous,
        blitz_traits::net::CorsSetting::UseCredentials => CrossOrigin::UseCredentials,
    })
}

// https://html.spec.whatwg.org/multipage/urls-and-fetching.html#referrer-policy-attribute
#[must_use]
pub fn referrer_policy(policy: blitz_traits::net::ReferrerPolicy) -> net::ReferrerPolicy {
    match policy {
        blitz_traits::net::ReferrerPolicy::NoReferrer => net::ReferrerPolicy::NoReferrer,
        blitz_traits::net::ReferrerPolicy::NoReferrerWhenDowngrade => {
            net::ReferrerPolicy::NoReferrerWhenDowngrade
        }
        blitz_traits::net::ReferrerPolicy::Origin => net::ReferrerPolicy::Origin,
        blitz_traits::net::ReferrerPolicy::OriginWhenCrossOrigin => {
            net::ReferrerPolicy::OriginWhenCrossOrigin
        }
        blitz_traits::net::ReferrerPolicy::SameOrigin => net::ReferrerPolicy::SameOrigin,
        blitz_traits::net::ReferrerPolicy::StrictOrigin => net::ReferrerPolicy::StrictOrigin,
        blitz_traits::net::ReferrerPolicy::StrictOriginWhenCrossOrigin => {
            net::ReferrerPolicy::StrictOriginWhenCrossOrigin
        }
        blitz_traits::net::ReferrerPolicy::UnsafeUrl => net::ReferrerPolicy::UnsafeUrl,
    }
}

// https://html.spec.whatwg.org/multipage/browsers.html#determining-the-creation-sandboxing-flags
// https://html.spec.whatwg.org/multipage/browsers.html#popup-sandboxing-flag-set
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FrameKind {
    Child,
    Auxiliary,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct WindowId(u64);

impl WindowId {
    #[must_use]
    pub fn new(raw: u64) -> Self {
        Self(raw)
    }

    #[must_use]
    pub fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum ScriptSource<T = String> {
    Page(T),
    Browser(T),
    BrowserStrict(T),
    Isolated { world: T, source: T },
}

impl ScriptSource {
    #[must_use]
    pub fn as_ref(&self) -> ScriptSource<&str> {
        match self {
            Self::Page(source) => ScriptSource::Page(source),
            Self::Browser(source) => ScriptSource::Browser(source),
            Self::BrowserStrict(source) => ScriptSource::BrowserStrict(source),
            Self::Isolated { world, source } => ScriptSource::Isolated { world, source },
        }
    }
}

impl ScriptSource<&str> {
    #[must_use]
    pub fn into_owned(self) -> ScriptSource {
        match self {
            Self::Page(source) => ScriptSource::Page(source.to_owned()),
            Self::Browser(source) => ScriptSource::Browser(source.to_owned()),
            Self::BrowserStrict(source) => ScriptSource::BrowserStrict(source.to_owned()),
            Self::Isolated { world, source } => ScriptSource::Isolated {
                world: world.to_owned(),
                source: source.to_owned(),
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TabError {
    InvalidUrl { spec: String },
    UnknownFrame { frame: u64 },
    UnknownHistoryEntry { entry: u64 },
    Script(ScriptFailure),
    ActorStopped,
    RendererCrashed,
    RendererUnavailable { message: String },
    ResourceLimit { resource: ResourceLimit },
    Render { message: String },
    Dom { message: String },
}

impl fmt::Display for TabError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUrl { spec } => write!(f, "invalid url: {spec}"),
            Self::UnknownFrame { frame } => write!(f, "unknown frame: {frame}"),
            Self::UnknownHistoryEntry { entry } => write!(f, "unknown history entry: {entry}"),
            Self::Script(failure) => write!(f, "script: {failure}"),
            Self::ActorStopped => f.write_str("renderer stopped"),
            Self::RendererCrashed => f.write_str("renderer crashed"),
            Self::RendererUnavailable { message } => {
                write!(f, "renderer unavailable: {message}")
            }
            Self::ResourceLimit { resource } => write!(f, "resource limit reached: {resource}"),
            Self::Render { message } => write!(f, "render: {message}"),
            Self::Dom { message } => write!(f, "dom: {message}"),
        }
    }
}

impl std::error::Error for TabError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResourceLimit {
    TabWaiters,
    TabSubscribers,
}

impl fmt::Display for ResourceLimit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TabWaiters => f.write_str("tab waiters"),
            Self::TabSubscribers => f.write_str("tab subscribers"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScriptFailure {
    Engine { message: Box<str> },
    BadTimerId,
    HostMissing,
    Interrupted,
}

impl fmt::Display for ScriptFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Engine { message } => f.write_str(message),
            Self::BadTimerId => f.write_str("bad timer id"),
            Self::HostMissing => f.write_str("js host missing"),
            Self::Interrupted => f.write_str("interrupted"),
        }
    }
}

impl std::error::Error for ScriptFailure {}

// https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-history-pushstate
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HistoryHandling {
    Push,
    Replace,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct TimerId(u64);

impl TimerId {
    #[must_use]
    pub fn take_next(&mut self) -> Self {
        let id = *self;
        self.0 += 1;
        id
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum RendererEvent {
    Browser(BrowserEvent),
    Timer(TimerId),
    Fetch { status: u16 },
    FetchFailed,
    ScriptFailed,
}

impl RendererEvent {
    #[must_use]
    pub fn into_browser(self) -> Option<BrowserEvent> {
        match self {
            Self::Browser(event) => Some(event),
            Self::Timer(..) | Self::Fetch { .. } | Self::FetchFailed | Self::ScriptFailed => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum BrowserEvent {
    Load,
    // https://html.spec.whatwg.org/multipage/parsing.html#the-end
    DomContentLoaded,
    // https://chromedevtools.github.io/devtools-protocol/tot/Page/#event-frameNavigated
    Navigated {
        url: String,
        // https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName
        outgoing_name: Option<ContextName>,
    },
    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#finalize-a-cross-document-navigation
    NavigationStarted {
        url: String,
        replace: bool,
        local_index: usize,
        // https://html.spec.whatwg.org/multipage/browsing-the-web.html#finalize-a-cross-document-navigation
        pending: bool,
    },
    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#session-history-entry
    FrameAdded {
        url: String,
    },
    // https://html.spec.whatwg.org/multipage/history.html#joint-session-history
    FrameRemoved,
    HistoryUpdated {
        url: String,
        state: String,
        handling: HistoryHandling,
    },
    HistoryTraversal {
        delta: i32,
    },
    // https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#document-open-steps — the renderer replaces the realm the spec keeps, so the replacement is announced.
    RealmReplaced,
    // https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#event-bindingCalled — the payload is pre-serialized by the page.
    Binding {
        name: String,
        payload: String,
    },
    // https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#event-consoleAPICalled — args are pre-serialized RemoteObjects; the CDP layer parses them where the shape is owned.
    Console {
        level: ConsoleLevel,
        args: String,
    },
    // https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#event-exceptionThrown
    // https://w3c.github.io/webdriver-bidi/#type-log-JavascriptLogEntry
    ExceptionThrown {
        text: String,
        filename: String,
        line: u32,
        column: u32,
    },
}

// https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#type-ConsoleApiType
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConsoleLevel {
    Log,
    Debug,
    Info,
    Warning,
    Error,
    Assert,
    Trace,
    Dir,
    Table,
    StartGroup,
    StartGroupCollapsed,
    EndGroup,
}

impl ConsoleLevel {
    // https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#type-ConsoleApiType
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Log => "log",
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Error => "error",
            Self::Assert => "assert",
            Self::Trace => "trace",
            Self::Dir => "dir",
            Self::Table => "table",
            Self::StartGroup => "startGroup",
            Self::StartGroupCollapsed => "startGroupCollapsed",
            Self::EndGroup => "endGroup",
        }
    }

    #[must_use]
    pub fn parse_js(name: &str) -> Option<Self> {
        match name {
            "log" => Some(Self::Log),
            "debug" => Some(Self::Debug),
            "info" => Some(Self::Info),
            "warning" => Some(Self::Warning),
            "error" => Some(Self::Error),
            "assert" => Some(Self::Assert),
            "trace" => Some(Self::Trace),
            "dir" => Some(Self::Dir),
            "table" => Some(Self::Table),
            "startGroup" => Some(Self::StartGroup),
            "startGroupCollapsed" => Some(Self::StartGroupCollapsed),
            "endGroup" => Some(Self::EndGroup),
            _ => None,
        }
    }
}

// https://cookiestore.spec.whatwg.org/#cookie-list-item
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScriptCookie {
    pub name: String,
    pub value: String,
    pub slot: String,
    pub state: String,
}

impl ScriptCookie {
    #[must_use]
    pub fn from_record(record: &net::CookieRecord) -> Self {
        Self {
            name: record.name.clone(),
            value: record.value.clone(),
            slot: record.slot(),
            state: record.state(),
        }
    }
}

// https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-name
// https://html.spec.whatwg.org/multipage/browsers.html#popup-sandboxing-flag-set
// https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TabState {
    pub id: Option<WindowId>,
    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName
    pub name: Option<ContextName>,
    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName
    pub auxiliary_with_opener: bool,
    // https://html.spec.whatwg.org/multipage/browsers.html#popup-sandboxing-flag-set
    pub popup_sandbox: SandboxFlags,
    // https://html.spec.whatwg.org/multipage/webappapis.html#secure-contexts
    pub secure_context: Option<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Mount {
    pub head: ResponseHead,
    pub tab: TabState,
    #[serde(skip, default)]
    pub body: Vec<u8>,
}

// https://html.spec.whatwg.org/multipage/browsers.html#cross-origin-opener-policies
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CrossOriginOpenerPolicy {
    #[default]
    UnsafeNone,
    SameOrigin,
}

// https://html.spec.whatwg.org/multipage/browsers.html#cross-origin-embedder-policies
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CrossOriginEmbedderPolicy {
    #[default]
    UnsafeNone,
    RequireCorp,
    Credentialless,
}

impl CrossOriginEmbedderPolicy {
    // https://html.spec.whatwg.org/multipage/browsers.html#compatible-with-cross-origin-isolation
    #[must_use]
    pub const fn compatible(self) -> bool {
        matches!(self, Self::RequireCorp | Self::Credentialless)
    }
}

// https://html.spec.whatwg.org/multipage/browsers.html#policy-containers
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PolicyContainer {
    pub csp: crate::csp::List,
    // https://w3c.github.io/webappsec-csp/#sandbox
    pub sandbox: SandboxFlags,
    pub referrer_policy: Option<net::ReferrerPolicy>,
    // https://html.spec.whatwg.org/multipage/webappapis.html#secure-contexts
    pub secure_context: bool,
    pub cross_origin_opener_policy: CrossOriginOpenerPolicy,
    pub cross_origin_embedder_policy: CrossOriginEmbedderPolicy,
    // https://html.spec.whatwg.org/multipage/webappapis.html#concept-settings-object-cross-origin-isolated-capability
    pub cross_origin_isolated: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResponseHead {
    pub url: Url,
    // https://fetch.spec.whatwg.org/#concept-header-list-get-decode-split
    pub content_type: Option<String>,
    pub history: HistorySnapshot,
    pub viewport: Option<(u32, u32)>,
    // https://chromedevtools.github.io/devtools-protocol/tot/Page/#method-addScriptToEvaluateOnNewDocument
    pub init_scripts: Vec<(u64, String)>,
    // https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#method-addBinding
    pub bindings: Vec<String>,
    pub source: ResponseSource,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ResponseSource {
    Http(HttpHead),
    Local {
        // https://html.spec.whatwg.org/multipage/browsers.html#determining-navigation-params-policy-container
        inherited: Option<PolicyContainer>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HttpHead {
    // https://w3c.github.io/webappsec-csp/#create-violation-for-global
    pub status: u16,
    pub policy: PolicyContainer,
    // https://w3c.github.io/reporting/#header
    pub reporting_endpoints: Option<String>,
    // https://html.spec.whatwg.org/multipage/document-lifecycle.html#creating-and-initializing-a-document-object
    pub refresh: Option<String>,
    pub no_sniff: bool,
}

impl ResponseSource {
    // https://w3c.github.io/webappsec-csp/#create-violation-for-global
    #[must_use]
    pub fn status(&self) -> u16 {
        match self {
            Self::Http(head) => head.status,
            Self::Local { .. } => 0,
        }
    }

    #[must_use]
    pub fn policy(&self) -> &PolicyContainer {
        static EMPTY: PolicyContainer = PolicyContainer {
            csp: crate::csp::List::empty(),
            sandbox: SandboxFlags::empty(),
            referrer_policy: None,
            secure_context: false,
            cross_origin_opener_policy: CrossOriginOpenerPolicy::UnsafeNone,
            cross_origin_embedder_policy: CrossOriginEmbedderPolicy::UnsafeNone,
            cross_origin_isolated: false,
        };
        match self {
            Self::Http(head) => &head.policy,
            Self::Local { inherited } => inherited.as_ref().unwrap_or(&EMPTY),
        }
    }

    #[must_use]
    pub fn reporting_endpoints(&self) -> Option<&str> {
        match self {
            Self::Http(head) => head.reporting_endpoints.as_deref(),
            Self::Local { .. } => None,
        }
    }

    #[must_use]
    pub fn refresh(&self) -> Option<&str> {
        match self {
            Self::Http(head) => head.refresh.as_deref(),
            Self::Local { .. } => None,
        }
    }

    #[must_use]
    pub fn is_http(&self) -> bool {
        matches!(self, Self::Http(_))
    }

    #[must_use]
    pub fn no_sniff(&self) -> bool {
        match self {
            Self::Http(head) => head.no_sniff,
            Self::Local { .. } => false,
        }
    }
}

// https://fetch.spec.whatwg.org/#concept-header-list-get
fn header_value_is(headers: &[(String, String)], name: &str, expected: &str) -> bool {
    net::combined_header_in(headers, name)
        .is_some_and(|value| value.trim().eq_ignore_ascii_case(expected))
}

// https://html.spec.whatwg.org/multipage/browsers.html#cross-origin-opener-policies
#[must_use]
fn cross_origin_opener_policy(headers: &[(String, String)]) -> CrossOriginOpenerPolicy {
    if header_value_is(headers, "cross-origin-opener-policy", "same-origin") {
        CrossOriginOpenerPolicy::SameOrigin
    } else {
        CrossOriginOpenerPolicy::UnsafeNone
    }
}

// https://html.spec.whatwg.org/multipage/browsers.html#cross-origin-embedder-policies
#[must_use]
pub fn cross_origin_embedder_policy(headers: &[(String, String)]) -> CrossOriginEmbedderPolicy {
    let Some(value) = net::combined_header_in(headers, "cross-origin-embedder-policy") else {
        return CrossOriginEmbedderPolicy::UnsafeNone;
    };
    let value = value.trim();
    if value.eq_ignore_ascii_case("require-corp") {
        CrossOriginEmbedderPolicy::RequireCorp
    } else if value.eq_ignore_ascii_case("credentialless") {
        CrossOriginEmbedderPolicy::Credentialless
    } else {
        CrossOriginEmbedderPolicy::UnsafeNone
    }
}

impl ResponseHead {
    #[must_use]
    pub fn http(
        status: u16,
        final_url: Url,
        content_type: Option<String>,
        headers: &[(String, String)],
    ) -> Self {
        Self {
            url: final_url,
            content_type,
            history: HistorySnapshot::default(),
            viewport: None,
            init_scripts: Vec::new(),
            bindings: Vec::new(),
            source: ResponseSource::Http(HttpHead {
                status,
                policy: {
                    let csp = crate::csp::List::from_headers(
                        net::combined_header_in(headers, "content-security-policy").as_deref(),
                        net::combined_header_in(headers, "content-security-policy-report-only")
                            .as_deref(),
                    );
                    PolicyContainer {
                        // https://w3c.github.io/webappsec-csp/#sandbox
                        sandbox: csp.sandbox_flags(),
                        csp,
                        // https://w3c.github.io/webappsec-referrer-policy/#parse-referrer-policy-from-header
                        referrer_policy: net::ReferrerPolicy::from_header(
                            headers
                                .iter()
                                .filter(|(name, _)| name.eq_ignore_ascii_case("referrer-policy"))
                                .map(|(_, value)| value.as_str()),
                        ),
                        // https://html.spec.whatwg.org/multipage/webappapis.html#secure-contexts
                        secure_context: false,
                        cross_origin_opener_policy: cross_origin_opener_policy(headers),
                        cross_origin_embedder_policy: cross_origin_embedder_policy(headers),
                        // https://html.spec.whatwg.org/multipage/webappapis.html#concept-settings-object-cross-origin-isolated-capability
                        cross_origin_isolated: false,
                    }
                },
                reporting_endpoints: net::combined_header_in(headers, "reporting-endpoints"),
                refresh: headers.iter().find_map(|(name, value)| {
                    name.eq_ignore_ascii_case("refresh").then(|| value.clone())
                }),
                no_sniff: crate::sniff::headers_nosniff(headers),
            }),
        }
    }

    #[must_use]
    pub fn local(
        final_url: Url,
        content_type: Option<String>,
        inherited: Option<PolicyContainer>,
    ) -> Self {
        Self {
            url: final_url,
            content_type,
            history: HistorySnapshot::default(),
            viewport: None,
            init_scripts: Vec::new(),
            bindings: Vec::new(),
            source: ResponseSource::Local { inherited },
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct HistorySnapshot {
    pub length: usize,
    pub index: usize,
    pub local_length: usize,
    pub local_index: usize,
    pub state: Option<String>,
}

impl HistorySnapshot {
    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#url-and-history-update-steps
    pub fn apply(&mut self, state: String, handling: HistoryHandling) {
        self.state = Some(state);
        match handling {
            HistoryHandling::Push => {
                self.index = self.index.saturating_add(1);
                self.length = self.index.saturating_add(1);
            }
            HistoryHandling::Replace => {}
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScreenshotFormat {
    #[default]
    Png,
    Jpeg {
        quality: u8,
    },
    Webp,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScreenshotRequest {
    pub viewport_width: u32,
    pub viewport_height: u32,
    pub clip: Option<ScreenshotClip>,
    #[serde(default)]
    pub format: ScreenshotFormat,
    #[serde(default)]
    pub omit_background: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScreenshotClip {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

// https://chromedevtools.github.io/devtools-protocol/tot/DOM/
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum DomRequest {
    GetDocument,
    QuerySelector {
        node: u64,
        selector: String,
    },
    QuerySelectorAll {
        node: u64,
        selector: String,
    },
    DescribeNode {
        node: u64,
    },
    GetOuterHtml {
        node: u64,
    },
    GetAttributes {
        node: u64,
    },
    ResolveNode {
        node: u64,
        // https://chromedevtools.github.io/devtools-protocol/tot/DOM/#method-resolveNode
        world: Option<String>,
    },
    ResolveObject {
        object_id: String,
    },
    GetNodeForLocation {
        x: f64,
        y: f64,
    },
    GetBoxModel {
        node: u64,
    },
    GetContentQuads {
        node: u64,
    },
    ScrollIntoViewIfNeeded {
        node: u64,
    },
    GetComputedStyle {
        node: u64,
    },
    GetStyleSheets,
    GetStorageKey,
    GetFullAxTree,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DomNode {
    pub node_id: u64,
    pub backend_node_id: u64,
    pub node_type: u16,
    pub node_name: String,
    pub local_name: Option<String>,
    pub node_value: String,
    pub attributes: Vec<String>,
    pub children: Vec<DomNode>,
    pub child_node_count: u32,
    pub document_url: Option<String>,
    pub base_url: Option<String>,
    pub compatibility_mode: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DomRemoteNode {
    pub object_id: String,
    pub class_name: String,
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DomBoxModel {
    pub content: [f64; 8],
    pub padding: [f64; 8],
    pub border: [f64; 8],
    pub margin: [f64; 8],
    pub width: i64,
    pub height: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomStyleSheet {
    pub style_sheet_id: String,
    pub source_url: String,
    pub title: String,
    pub disabled: bool,
    pub is_inline: bool,
    pub length: usize,
}

// https://chromedevtools.github.io/devtools-protocol/tot/Accessibility/#type-AXNode
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AxPropertyValue {
    Bool(bool),
    Int(i64),
    String(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AxProperty {
    pub name: String,
    pub value: AxPropertyValue,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AxNode {
    pub node_id: String,
    pub ignored: bool,
    pub role: String,
    pub name: Option<String>,
    pub value: Option<String>,
    pub properties: Vec<AxProperty>,
    pub parent_id: Option<String>,
    pub child_ids: Vec<String>,
    pub backend_dom_node_id: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum DomReply {
    Document { root: DomNode },
    NodeId { node_id: u64 },
    NodeIds { node_ids: Vec<u64> },
    Described { node: Option<DomNode> },
    OuterHtml { outer_html: String },
    Attributes { attributes: Vec<String> },
    Resolved { object: Option<DomRemoteNode> },
    NodeForLocation { node_id: u64 },
    BoxModel { model: Option<DomBoxModel> },
    ContentQuads { quads: Vec<[f64; 8]> },
    Unit,
    ComputedStyle { properties: Vec<(String, String)> },
    StyleSheets { sheets: Vec<DomStyleSheet> },
    StorageKey { key: String },
    AxTree { nodes: Vec<AxNode> },
}

pub type DialCompletion = Box<dyn FnOnce(Result<DialOutcome, DialFailure>) + Send + 'static>;

pub type DialCancellation = Box<dyn FnOnce() + Send + 'static>;

pub trait NetworkHost: Send + Sync + 'static {
    fn start_dial(&self, request: DialRequest, completion: DialCompletion) -> DialCancellation;

    fn clock(&self) -> net::Clock {
        net::Clock::default()
    }

    fn dial_blocking(
        &self,
        request: DialRequest,
        timeout: std::time::Duration,
    ) -> Result<DialOutcome, DialFailure> {
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        let cancel = self.start_dial(
            request,
            Box::new(move |outcome| {
                let _ = send.send(outcome);
            }),
        );
        if let Ok(outcome) = receive.recv_timeout(timeout) {
            outcome
        } else {
            cancel();
            Err(DialFailure::Timeout)
        }
    }

    fn dial_read(&self, _stream: StreamId) -> Result<Option<Vec<u8>>, DialFailure> {
        Err(DialFailure::Connect)
    }

    fn cancel_stream(&self, _stream: StreamId) {}

    // https://xhr.spec.whatwg.org/#the-send(
    fn dial_sync(&self, _request: DialRequest) -> Result<DialOutcome, DialFailure> {
        Err(DialFailure::Connect)
    }
}

pub trait StorageHost: Send + Sync + 'static {
    // https://html.spec.whatwg.org/multipage/webstorage.html#dom-storage-getitem
    fn storage_get(&self, kind: StorageKind, origin: &StorageKey, key: &str) -> Option<String>;

    // https://html.spec.whatwg.org/multipage/webstorage.html#dom-storage-key
    fn storage_keys(&self, kind: StorageKind, origin: &StorageKey) -> Vec<String>;

    fn storage_set(
        &self,
        kind: StorageKind,
        origin: &StorageKey,
        url: &str,
        key: &str,
        value: &str,
        source: FrameId,
    ) -> Result<Option<StorageChange>, StorageError>;

    fn storage_remove(
        &self,
        kind: StorageKind,
        origin: &StorageKey,
        url: &str,
        key: &str,
        source: FrameId,
    ) -> Option<StorageChange>;

    fn storage_clear(
        &self,
        kind: StorageKind,
        origin: &StorageKey,
        url: &str,
        source: FrameId,
    ) -> Option<StorageChange>;

    fn remote_session_get(
        &self,
        window: WindowId,
        origin: &StorageKey,
        key: &str,
    ) -> Option<String>;

    fn cookies_for(&self, url: &Url, partition_key: Option<&str>) -> String;

    fn set_cookie(&self, value: &str, url: &Url, partition_key: Option<&str>);

    // https://cookiestore.spec.whatwg.org/#query-cookies
    fn cookie_store_records(&self, url: &Url, partition_key: Option<&str>) -> Vec<ScriptCookie>;

    // https://cookiestore.spec.whatwg.org/#set-a-cookie
    fn cookie_store_store(
        &self,
        write: &net::ScriptCookieWrite,
        url: &Url,
        partition_key: Option<&str>,
    );
}

// https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-open
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OpenerPolicy {
    Keep,
    NoOpener,
    NoReferrer,
}

// https://html.spec.whatwg.org/multipage/document-sequences.html#valid-browsing-context-name-or-keyword
// https://html.spec.whatwg.org/multipage/nav-history-apis.html#window-open-steps
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TargetName {
    Current,
    Parent,
    Top,
    Blank,
    Named(ContextName),
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct ContextName(String);

impl ContextName {
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        (!raw.is_empty() && TargetName::keyword(raw).is_none()).then(|| Self(raw.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ContextName {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ContextName {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).ok_or_else(|| serde::de::Error::custom("a target name is non-empty"))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NameChange {
    // https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-name
    Assigned(Option<ContextName>),
    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName
    Reset,
}

impl TargetName {
    #[must_use]
    pub fn parse(raw: &str) -> Self {
        if raw.is_empty() {
            Self::Current
        } else {
            Self::keyword(raw).unwrap_or_else(|| Self::Named(ContextName(raw.to_owned())))
        }
    }

    #[must_use]
    pub fn for_window_open(raw: &str) -> Self {
        if raw.is_empty() {
            Self::Blank
        } else {
            Self::parse(raw)
        }
    }

    fn keyword(raw: &str) -> Option<Self> {
        if raw.eq_ignore_ascii_case("_self") {
            Some(Self::Current)
        } else if raw.eq_ignore_ascii_case("_parent") {
            Some(Self::Parent)
        } else if raw.eq_ignore_ascii_case("_top") {
            Some(Self::Top)
        } else if raw.eq_ignore_ascii_case("_blank") {
            Some(Self::Blank)
        } else {
            None
        }
    }

    // https://html.spec.whatwg.org/multipage/nav-history-apis.html#window-open-steps
    #[must_use]
    pub fn noopener_withholds(&self) -> bool {
        matches!(self, Self::Blank | Self::Named(_))
    }

    #[must_use]
    pub fn new_context_name(&self) -> Option<&ContextName> {
        match self {
            Self::Named(name) => Some(name),
            Self::Current | Self::Parent | Self::Top | Self::Blank => None,
        }
    }

    #[must_use]
    pub fn is_canonical(&self) -> bool {
        match self {
            Self::Named(name) => Self::parse(name.as_str()) == *self,
            Self::Current | Self::Parent | Self::Top | Self::Blank => true,
        }
    }
}

// https://html.spec.whatwg.org/multipage/nav-history-apis.html#window-open-steps
// https://w3c.github.io/webappsec-referrer-policy/#determine-requests-referrer
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NavigationSource {
    pub origin: SerializedOrigin,
    pub referrer: Url,
    pub referrer_policy: net::ReferrerPolicy,
    // https://html.spec.whatwg.org/multipage/browsers.html#sandboxed-navigation-browsing-context-flag
    pub frame: FrameId,
    // https://html.spec.whatwg.org/multipage/webappapis.html#secure-contexts
    pub secure_context: bool,
}

// https://html.spec.whatwg.org/multipage/timers-and-user-prompts.html#simple-dialogs
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PromptKind {
    Alert,
    Confirm,
    Prompt,
}

impl PromptKind {
    #[must_use]
    pub fn parse_js(name: &str) -> Option<Self> {
        match name {
            "alert" => Some(Self::Alert),
            "confirm" => Some(Self::Confirm),
            "prompt" => Some(Self::Prompt),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Alert => "alert",
            Self::Confirm => "confirm",
            Self::Prompt => "prompt",
        }
    }
}

// https://html.spec.whatwg.org/multipage/timers-and-user-prompts.html#simple-dialogs
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PromptOutcome {
    Accepted { text: Option<String> },
    Dismissed,
}

pub trait BrowsingContextHost: Send + Sync + 'static {
    // https://html.spec.whatwg.org/multipage/timers-and-user-prompts.html#simple-dialogs
    fn user_prompt(
        &self,
        _frame: FrameId,
        _kind: PromptKind,
        _message: &str,
        _default_value: Option<&str>,
    ) -> PromptOutcome {
        PromptOutcome::Dismissed
    }
    // https://html.spec.whatwg.org/multipage/nav-history-apis.html#window-open-steps
    // https://html.spec.whatwg.org/multipage/browsers.html#popup-sandboxing-flag-set
    // https://w3c.github.io/webappsec-referrer-policy/#determine-requests-referrer
    fn window_open(
        &self,
        url: Option<&Url>,
        target: &TargetName,
        opener: OpenerPolicy,
        source: &NavigationSource,
    ) -> Option<WindowId>;

    // https://html.spec.whatwg.org/multipage/document-sequences.html#creating-a-new-auxiliary-browsing-context
    fn opens_window_as_tab(&self) -> bool {
        true
    }

    // https://html.spec.whatwg.org/multipage/browsers.html#determining-the-creation-sandboxing-flags
    fn frame_created(
        &self,
        _frame: FrameId,
        _parent: FrameId,
        _kind: crate::FrameKind,
        _sandbox: SandboxFlags,
    ) {
    }

    fn frame_destroyed(&self, _frame: FrameId) {}

    // https://html.spec.whatwg.org/multipage/document-sequences.html#the-rules-for-choosing-a-navigable
    fn navigate_named(
        &self,
        name: &ContextName,
        url: Option<&Url>,
        opener: OpenerPolicy,
        source: &NavigationSource,
    ) -> Option<WindowId>;

    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#navigate
    fn navigate_top_level(&self, _url: &Url, _source: &NavigationSource) -> bool {
        false
    }

    // https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-name
    fn set_window_name(&self, change: NameChange);

    fn window_close(&self, window: WindowId);

    fn window_opener(&self) -> Option<WindowId>;

    fn window_post_message(&self, window: WindowId, message: PostedMessage);
}

pub trait MessagingHost: Send + Sync + 'static {
    fn broadcast_post(&self, origin: &StorageKey, name: &str, payload: &str, channel: u64);
}

// https://html.spec.whatwg.org/multipage/web-messaging.html#window-post-message-steps
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TargetOrigin {
    Any,
    Exact(url::Origin),
}

impl TargetOrigin {
    // https://html.spec.whatwg.org/multipage/web-messaging.html#window-post-message-steps
    #[must_use]
    pub fn parse(spec: &str, sender: Option<&url::Origin>) -> Option<Self> {
        match spec {
            "*" => Some(Self::Any),
            "/" => sender.map(|origin| Self::Exact(origin.clone())),
            other => Url::parse(other).ok().map(|url| Self::Exact(url.origin())),
        }
    }

    #[must_use]
    pub fn admits(&self, origin: &url::Origin) -> bool {
        match self {
            Self::Any => true,
            Self::Exact(target) => target == origin,
        }
    }
}

impl Serialize for TargetOrigin {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Any => serializer.serialize_str("*"),
            Self::Exact(origin) => serializer.serialize_str(&origin.ascii_serialization()),
        }
    }
}

impl<'de> Deserialize<'de> for TargetOrigin {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        match raw.as_str() {
            "*" => Ok(Self::Any),
            other => match SerializedOrigin::parse(other) {
                Some(SerializedOrigin::Opaque) => Ok(Self::Exact(url::Origin::new_opaque())),
                Some(SerializedOrigin::Tuple(origin)) => Ok(Self::Exact(origin)),
                None => Err(serde::de::Error::custom("invalid target origin")),
            },
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PostedMessage {
    pub origin: SerializedOrigin,
    pub target_origin: TargetOrigin,
    pub payload: String,
}

pub trait BrowserServices: NetworkHost + StorageHost + BrowsingContextHost + MessagingHost {}

impl<T> BrowserServices for T where
    T: NetworkHost + StorageHost + BrowsingContextHost + MessagingHost
{
}

#[cfg(test)]
mod request_policy_tests {
    use super::{CorsMode, CorsRequest, RequestPolicy};
    use net::cors::{CredentialsMode, CrossOrigin, RedirectMode, RequestMode};

    fn fetch(mode: CorsMode) -> RequestPolicy {
        RequestPolicy::Fetch(CorsRequest {
            mode,
            credentials: CredentialsMode::SameOrigin,
            redirect: RedirectMode::Follow,
        })
    }

    #[test]
    fn every_policy_resolves_to_the_parameters_its_rules_name() {
        for (policy, mode, credentials) in policy_table() {
            let resolved = policy.cors();
            assert_eq!(
                (resolved.mode, resolved.credentials, resolved.redirect),
                (mode, credentials, RedirectMode::Follow),
                "{:?}",
                policy.kind()
            );
        }
    }

    fn policy_table() -> Vec<(RequestPolicy, RequestMode, CredentialsMode)> {
        let anonymous = Some(CrossOrigin::Anonymous);
        let credentials = Some(CrossOrigin::UseCredentials);
        let mut table = vec![
            (
                fetch(CorsMode::NoCors),
                RequestMode::NoCors,
                CredentialsMode::SameOrigin,
            ),
            (
                fetch(CorsMode::Cors),
                RequestMode::Cors,
                CredentialsMode::SameOrigin,
            ),
            (
                fetch(CorsMode::SameOrigin),
                RequestMode::SameOrigin,
                CredentialsMode::SameOrigin,
            ),
            // https://html.spec.whatwg.org/multipage/urls-and-fetching.html#create-a-potential-cors-request
            (
                RequestPolicy::ClassicScript { crossorigin: None },
                RequestMode::NoCors,
                CredentialsMode::Include,
            ),
            (
                RequestPolicy::ClassicScript {
                    crossorigin: anonymous,
                },
                RequestMode::Cors,
                CredentialsMode::SameOrigin,
            ),
            (
                RequestPolicy::ClassicScript {
                    crossorigin: credentials,
                },
                RequestMode::Cors,
                CredentialsMode::Include,
            ),
            (
                RequestPolicy::Stylesheet { crossorigin: None },
                RequestMode::NoCors,
                CredentialsMode::Include,
            ),
            (
                RequestPolicy::Image { crossorigin: None },
                RequestMode::NoCors,
                CredentialsMode::Include,
            ),
            (
                RequestPolicy::Image {
                    crossorigin: anonymous,
                },
                RequestMode::Cors,
                CredentialsMode::SameOrigin,
            ),
            (
                RequestPolicy::Image {
                    crossorigin: credentials,
                },
                RequestMode::Cors,
                CredentialsMode::Include,
            ),
            // https://drafts.csswg.org/css-fonts-4/#font-fetching-requirements
            // https://w3c.github.io/reporting/#try-delivery
            (
                RequestPolicy::Font,
                RequestMode::Cors,
                CredentialsMode::SameOrigin,
            ),
            (
                RequestPolicy::CspReportTo,
                RequestMode::Cors,
                CredentialsMode::SameOrigin,
            ),
            (
                RequestPolicy::FrameLoad,
                RequestMode::Navigate,
                CredentialsMode::Include,
            ),
        ];
        table.extend(module_policy_table());
        table
    }

    // https://html.spec.whatwg.org/multipage/webappapis.html#fetch-a-single-module-script
    fn module_policy_table() -> [(RequestPolicy, RequestMode, CredentialsMode); 4] {
        [
            (
                RequestPolicy::ModuleScript {
                    credentials: CredentialsMode::SameOrigin,
                },
                RequestMode::Cors,
                CredentialsMode::SameOrigin,
            ),
            (
                RequestPolicy::ModuleScript {
                    credentials: CredentialsMode::Include,
                },
                RequestMode::Cors,
                CredentialsMode::Include,
            ),
            (
                RequestPolicy::ModuleWorkerScript {
                    credentials: CredentialsMode::SameOrigin,
                },
                RequestMode::SameOrigin,
                CredentialsMode::SameOrigin,
            ),
            (
                RequestPolicy::ModuleWorkerScript {
                    credentials: CredentialsMode::Include,
                },
                RequestMode::SameOrigin,
                CredentialsMode::Include,
            ),
        ]
    }

    // https://w3c.github.io/webappsec-csp/#report-violation — sends no-cors with redirect mode "error", matching Chromium over literal Fetch pre-send rejection
    #[test]
    fn a_report_uri_delivery_does_not_follow_redirects() {
        let resolved = RequestPolicy::CspReportUri.cors();
        assert_eq!(resolved.mode, RequestMode::NoCors);
        assert_eq!(resolved.credentials, CredentialsMode::SameOrigin);
        assert_eq!(resolved.redirect, RedirectMode::Error);
    }

    #[test]
    fn a_page_fetch_mode_cannot_be_navigate() {
        assert_eq!(CorsMode::from_idl("navigate"), None);
        assert_eq!(CorsMode::from_idl("no-cors"), Some(CorsMode::NoCors));
        assert_eq!(CorsMode::from_idl("cors"), Some(CorsMode::Cors));
        assert_eq!(
            CorsMode::from_idl("same-origin"),
            Some(CorsMode::SameOrigin)
        );
        assert_eq!(CorsMode::from_idl("cors "), None);
    }
}

#[cfg(test)]
mod response_head_tests {
    use super::{PolicyContainer, ResponseHead};
    use url::Url;

    fn url(spec: &str) -> Url {
        Url::parse(spec).expect("URL")
    }

    #[test]
    fn a_local_head_has_no_http_state() {
        let head = ResponseHead::local(
            url("data:text/html,x"),
            Some("text/html".to_owned()),
            Some(PolicyContainer {
                referrer_policy: Some(net::ReferrerPolicy::NoReferrer),
                ..PolicyContainer::default()
            }),
        );
        assert_eq!(head.source.status(), 0);
        assert_eq!(head.source.refresh(), None);
        assert_eq!(head.source.reporting_endpoints(), None);
        assert!(!head.source.no_sniff());
        assert!(!head.source.is_http());
        assert_eq!(
            head.source.policy().referrer_policy,
            Some(net::ReferrerPolicy::NoReferrer),
            "the inherited container is the only policy source"
        );

        let inherited =
            ResponseHead::local(url("about:blank"), None, Some(PolicyContainer::default()));
        assert_eq!(inherited.source.policy().referrer_policy, None);
    }

    #[test]
    fn an_http_head_derives_its_policy_from_the_headers() {
        let headers = vec![
            (
                "Content-Security-Policy".to_owned(),
                "default-src 'none'".to_owned(),
            ),
            ("Referrer-Policy".to_owned(), "origin".to_owned()),
            ("X-Content-Type-Options".to_owned(), "nosniff".to_owned()),
            ("Refresh".to_owned(), "0; url=/next".to_owned()),
        ];
        let head = ResponseHead::http(
            204,
            url("https://example.test/"),
            Some("text/html".to_owned()),
            &headers,
        );
        assert_eq!(head.source.status(), 204);
        assert!(head.source.is_http());
        assert!(head.source.no_sniff());
        assert_eq!(head.source.refresh(), Some("0; url=/next"));
        assert_eq!(
            head.source.policy().referrer_policy,
            Some(net::ReferrerPolicy::Origin)
        );
        let document = url("https://example.test/");
        let gate = crate::csp::RedirectGate::new(
            head.source.policy().csp.clone(),
            &document.origin(),
            crate::csp::Destination::Script,
        );
        assert!(
            !gate.allows(&document, &url("https://example.test/app.js"), 0),
            "the header's policy is enforced, parsed at the head"
        );
    }
}

#[cfg(test)]
mod serialized_origin_tests {
    use super::SerializedOrigin;
    use url::Url;

    fn url(spec: &str) -> Url {
        Url::parse(spec).expect("URL")
    }

    // https://html.spec.whatwg.org/multipage/browsers.html#serialized-origins
    // https://fetch.spec.whatwg.org/#concept-request-origin
    #[test]
    fn a_serialized_origin_keeps_only_its_origin() {
        let tuple = SerializedOrigin::parse("https://a.test/x?q#f").expect("origin");
        assert_eq!(
            tuple.to_url(),
            Some(url("https://a.test/")),
            "the path and query do not survive"
        );
        assert!(!tuple.is_opaque());

        assert_eq!(
            SerializedOrigin::parse("blob:https://a.test/uuid").expect("origin"),
            SerializedOrigin::Tuple(url("blob:https://a.test/uuid").origin()),
            "a blob URL names the origin its entry was created in"
        );
        assert_eq!(
            SerializedOrigin::parse("null"),
            Some(SerializedOrigin::Opaque)
        );
        assert!(SerializedOrigin::parse("garbage").is_none());
        let opaque = SerializedOrigin::Opaque;
        assert!(opaque.is_opaque());
        assert_eq!(opaque.to_url(), None);
    }
}

#[cfg(test)]
mod target_name_tests {
    use super::{ContextName, TargetName};

    fn name(raw: &str) -> ContextName {
        ContextName::parse(raw).expect("a test name is non-empty")
    }

    // https://html.spec.whatwg.org/multipage/document-sequences.html#valid-browsing-context-name-or-keyword
    // https://html.spec.whatwg.org/multipage/nav-history-apis.html#window-open-steps
    #[test]
    fn a_target_name_parses_once_per_caller() {
        assert_eq!(TargetName::parse(""), TargetName::Current);
        assert_eq!(TargetName::for_window_open(""), TargetName::Blank);
        for spelling in ["_SELF", "_Self", "_self"] {
            assert_eq!(
                TargetName::parse(spelling),
                TargetName::Current,
                "{spelling}"
            );
        }
        for spelling in ["_PARENT", "_Top", "_blank"] {
            assert_eq!(
                TargetName::parse(spelling),
                match spelling.to_ascii_lowercase().as_str() {
                    "_parent" => TargetName::Parent,
                    "_top" => TargetName::Top,
                    _ => TargetName::Blank,
                },
                "{spelling}"
            );
        }
        for spelling in ["blank", "_new", "_BLANKX", "frame", "_current"] {
            assert_eq!(
                TargetName::parse(spelling),
                TargetName::Named(name(spelling)),
                "{spelling}"
            );
            assert_eq!(
                TargetName::for_window_open(spelling),
                TargetName::Named(name(spelling)),
                "{spelling}"
            );
        }
    }

    // https://html.spec.whatwg.org/multipage/nav-history-apis.html#window-open-steps
    #[test]
    fn only_a_named_or_new_target_carries_the_opener_rules() {
        assert!(TargetName::Blank.noopener_withholds());
        let frame = name("frame");
        assert!(TargetName::Named(frame.clone()).noopener_withholds());
        for target in [TargetName::Current, TargetName::Parent, TargetName::Top] {
            assert!(!target.noopener_withholds(), "{target:?}");
            assert_eq!(target.new_context_name(), None, "{target:?}");
        }
        assert_eq!(TargetName::Blank.new_context_name(), None);
        assert_eq!(
            TargetName::Named(frame.clone()).new_context_name(),
            Some(&frame)
        );
        assert_eq!(ContextName::parse(""), None);
        assert_eq!(ContextName::parse("_self"), None);
        assert_eq!(ContextName::parse("_BLANK"), None);
        assert!(TargetName::parse("_self").is_canonical());
        assert!(TargetName::Named(name("frame")).is_canonical());
    }
}

#[cfg(test)]
mod history_snapshot_tests {
    use super::{HistoryHandling, HistorySnapshot};

    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#url-and-history-update-steps
    #[test]
    fn a_push_mirrors_the_step_it_creates() {
        let mut snapshot = HistorySnapshot {
            length: 3,
            index: 1,
            local_length: 1,
            local_index: 1,
            state: None,
        };
        snapshot.apply("{}".to_owned(), HistoryHandling::Push);
        assert_eq!((snapshot.length, snapshot.index), (3, 2));

        let mut snapshot = HistorySnapshot {
            length: 3,
            index: 2,
            local_length: 2,
            local_index: 2,
            state: None,
        };
        snapshot.apply("{}".to_owned(), HistoryHandling::Push);
        assert_eq!((snapshot.length, snapshot.index), (4, 3));
    }

    #[test]
    fn a_replace_keeps_the_position_and_count() {
        let mut snapshot = HistorySnapshot {
            length: 4,
            index: 2,
            local_length: 2,
            local_index: 2,
            state: None,
        };
        snapshot.apply("{}".to_owned(), HistoryHandling::Replace);
        assert_eq!((snapshot.length, snapshot.index), (4, 2));
    }
}
