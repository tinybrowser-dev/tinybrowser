use std::fmt;

use enumerated::Enumerated;
use serde::{Deserialize, Serialize};
use url::Url;

pub use webstorage::{STORAGE_QUOTA_BYTES, StorageChange, StorageError};

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

pub const MAX_RESPONSE_BODY_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct FrameId(u64);

impl FrameId {
    pub const MAIN: Self = Self(0);

    #[must_use]
    pub fn new(raw: u64) -> Self {
        Self(raw)
    }

    #[must_use]
    pub fn get(self) -> u64 {
        self.0
    }

    #[must_use]
    pub fn worker(id: u64) -> Self {
        Self((1u64 << 63) | id)
    }

    // https://html.spec.whatwg.org/multipage/workers.html#dedicated-workers-and-the-dedicatedworkerglobalscope-interface
    #[must_use]
    pub const fn as_worker(self) -> Option<u64> {
        if self.0 >> 63 == 0 {
            None
        } else {
            Some(self.0 & !(1u64 << 63))
        }
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StorageKind {
    Local,
    Session,
    // https://w3c.github.io/IndexedDB/#database
    IndexedDb,
}

impl StorageKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Session => "session",
            Self::IndexedDb => "indexeddb",
        }
    }

    #[must_use]
    pub fn parse(token: &str) -> Option<Self> {
        match token {
            "local" => Some(Self::Local),
            "session" => Some(Self::Session),
            "indexeddb" => Some(Self::IndexedDb),
            _ => None,
        }
    }
}

// https://html.spec.whatwg.org/multipage/browsers.html#sandboxing-flag-set
macro_rules! sandbox_flags {
    ($($(#[$doc:meta])* $variant:ident),+ $(,)?) => {
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        #[repr(u8)]
        pub enum SandboxFlag {
            $($(#[$doc])* $variant),+
        }

        impl SandboxFlag {
            const ALL: &'static [Self] = &[$(Self::$variant),+];

            const fn bit(self) -> u32 {
                1 << (self as u32)
            }
        }
    };
}

sandbox_flags! {
    Navigation,
    AuxiliaryNavigation,
    TopLevelNavigationWithoutUserActivation,
    TopLevelNavigationWithUserActivation,
    Origin,
    Forms,
    PointerLock,
    Scripts,
    AutomaticFeatures,
    DocumentDomain,
    PropagatesToAuxiliary,
    Modals,
    OrientationLock,
    Presentation,
    Downloads,
    CustomProtocolsNavigation,
}

// https://html.spec.whatwg.org/multipage/browsers.html#sandboxing-flag-set
// https://html.spec.whatwg.org/multipage/browsers.html#popup-sandboxing-flag-set
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct SandboxFlags(u32);

impl SandboxFlags {
    const ALL: u32 = Self::bits(SandboxFlag::ALL);

    const fn bits(flags: &[SandboxFlag]) -> u32 {
        let mut bits = 0;
        let mut index = 0;
        while index < flags.len() {
            bits |= flags[index].bit();
            index += 1;
        }
        bits
    }

    #[must_use]
    pub const fn empty() -> Self {
        Self(0)
    }

    // https://html.spec.whatwg.org/multipage/iframe-embed-object.html#attr-iframe-sandbox
    const TOKENS: &'static [(&'static str, &'static [SandboxFlag])] = &[
        (
            "allow-popups",
            &[
                SandboxFlag::AuxiliaryNavigation,
                SandboxFlag::CustomProtocolsNavigation,
            ],
        ),
        (
            "allow-top-navigation",
            &[
                SandboxFlag::TopLevelNavigationWithoutUserActivation,
                SandboxFlag::TopLevelNavigationWithUserActivation,
                SandboxFlag::CustomProtocolsNavigation,
            ],
        ),
        (
            "allow-top-navigation-by-user-activation",
            &[SandboxFlag::TopLevelNavigationWithUserActivation],
        ),
        ("allow-same-origin", &[SandboxFlag::Origin]),
        ("allow-forms", &[SandboxFlag::Forms]),
        ("allow-pointer-lock", &[SandboxFlag::PointerLock]),
        (
            "allow-scripts",
            &[SandboxFlag::Scripts, SandboxFlag::AutomaticFeatures],
        ),
        (
            "allow-popups-to-escape-sandbox",
            &[SandboxFlag::PropagatesToAuxiliary],
        ),
        ("allow-modals", &[SandboxFlag::Modals]),
        ("allow-orientation-lock", &[SandboxFlag::OrientationLock]),
        ("allow-presentation", &[SandboxFlag::Presentation]),
        ("allow-downloads", &[SandboxFlag::Downloads]),
        (
            "allow-top-navigation-to-custom-protocols",
            &[SandboxFlag::CustomProtocolsNavigation],
        ),
    ];

    // https://dom.spec.whatwg.org/#concept-domtokenlist-validation
    #[must_use]
    pub fn supports(token: &str) -> bool {
        Self::TOKENS
            .iter()
            .any(|(keyword, _)| keyword.eq_ignore_ascii_case(token))
    }

    // https://html.spec.whatwg.org/multipage/browsers.html#parse-a-sandboxing-directive
    #[must_use]
    pub fn parse(attribute: Option<&str>) -> Self {
        let Some(attribute) = attribute else {
            return Self::default();
        };
        let mut flags = Self(Self::ALL);
        for token in attribute.split_ascii_whitespace() {
            if let Some((_, cleared)) = Self::TOKENS
                .iter()
                .find(|(keyword, _)| keyword.eq_ignore_ascii_case(token))
            {
                for flag in *cleared {
                    flags.0 &= !flag.bit();
                }
            }
        }
        flags
    }

    // https://w3c.github.io/webappsec-csp/#sandbox
    #[must_use]
    pub fn from_csp_headers(headers: &[(String, String)]) -> Self {
        crate::csp::List::from_headers(
            net::combined_header_in(headers, "content-security-policy").as_deref(),
            None,
        )
        .sandbox_flags()
    }

    #[must_use]
    pub fn contains(self, flag: SandboxFlag) -> bool {
        self.0 & flag.bit() != 0
    }

    // https://html.spec.whatwg.org/multipage/browsers.html#determining-the-creation-sandboxing-flags
    #[must_use]
    pub fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    #[must_use]
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    // https://html.spec.whatwg.org/multipage/browsers.html#sandboxed-auxiliary-navigation-browsing-context-flag
    #[must_use]
    pub fn blocks_auxiliary_navigation(self) -> bool {
        self.contains(SandboxFlag::AuxiliaryNavigation)
    }

    // https://html.spec.whatwg.org/multipage/browsers.html#sandboxed-navigation-browsing-context-flag
    #[must_use]
    pub fn blocks_navigation(self) -> bool {
        self.contains(SandboxFlag::Navigation)
    }

    // https://html.spec.whatwg.org/multipage/browsers.html#sandbox-propagates-to-auxiliary-browsing-contexts-flag
    #[must_use]
    pub fn propagates_to_auxiliary(self) -> bool {
        self.contains(SandboxFlag::PropagatesToAuxiliary)
    }

    // https://html.spec.whatwg.org/multipage/browsers.html#sandboxed-scripts-browsing-context-flag
    #[must_use]
    pub fn scripts_disabled(self) -> bool {
        self.contains(SandboxFlag::Scripts)
    }

    // https://html.spec.whatwg.org/multipage/browsers.html#sandboxed-origin-browsing-context-flag
    #[must_use]
    pub fn origin_is_opaque(self) -> bool {
        self.contains(SandboxFlag::Origin)
    }

    // https://html.spec.whatwg.org/multipage/browsers.html#sandbox-propagates-to-auxiliary-browsing-contexts-flag
    #[must_use]
    pub fn auxiliary_flags(self) -> Self {
        if self.propagates_to_auxiliary() {
            self
        } else {
            Self::empty()
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DialFailure {
    Dns,
    Connect,
    Tls,
    Timeout,
    Limit,
    QueueFull,
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DialKind {
    JsFetch,
    ClassicScript,
    ModuleScript,
    // https://html.spec.whatwg.org/multipage/links.html#link-type-stylesheet
    Stylesheet,
    Image,
    Font,
    FrameLoad,
    // https://w3c.github.io/webappsec-csp/#report-violation — sends no-cors with redirect mode "error", matching Chromium over literal Fetch pre-send rejection
    CspReport,
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

// https://fetch.spec.whatwg.org/#concept-request-referrer
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReferrerRequest {
    #[default]
    NoReferrer,
    // https://w3c.github.io/webappsec-referrer-policy/#determine-requests-referrer
    Client {
        url: Url,
    },
    Url(Url),
}

impl ReferrerRequest {
    // https://fetch.spec.whatwg.org/#concept-request-referrer
    #[must_use]
    pub fn from_idl(value: &str, client_url: &Url) -> Option<Self> {
        if value.is_empty() {
            return Some(Self::NoReferrer);
        }
        if value.eq_ignore_ascii_case("about:client") {
            return Some(Self::Client {
                url: client_url.clone(),
            });
        }
        Url::parse(value).ok().map(Self::Url)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DialRequest {
    pub policy: RequestPolicy,
    pub url: Url,
    // https://w3c.github.io/webappsec-csp/#sandbox
    pub frame: Option<FrameId>,
    // https://fetch.spec.whatwg.org/#concept-request-origin
    pub initiator: SerializedOrigin,
    pub delivery: BodyDelivery,
    // https://fetch.spec.whatwg.org/#concept-request-method
    pub method: net::Method,
    pub body: Vec<u8>,
    pub content_type: Option<String>,
    pub headers: Vec<(String, String)>,
    // https://fetch.spec.whatwg.org/#concept-request-referrer
    pub referrer: ReferrerRequest,
    // https://xhr.spec.whatwg.org/#the-timeout-attribute
    pub timeout_ms: u64,
    pub csp: Option<Box<crate::RedirectGate>>,
    // https://fetch.spec.whatwg.org/#keepalive-flag
    pub keepalive: bool,
    pub body_len: u64,
    // https://www.w3.org/TR/SRI/#verify-integrity
    pub integrity: Option<String>,
    // https://fetch.spec.whatwg.org/#concept-request-cache-mode
    pub cache: RequestCache,
    // https://fetch.spec.whatwg.org/#concept-request-referrer-policy
    pub referrer_policy: net::ReferrerPolicy,
    // https://privacycg.github.io/saa-non-cookie-storage/#shared-worker
    pub cookie_policy: CookiePolicy,
    // https://github.com/privacycg/CHIPS#computing-the-cookie-partition-key
    pub partition_key: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CookiePolicy {
    #[default]
    All,
    NoneOnly,
}

impl From<net::SameSiteCookiePolicy> for CookiePolicy {
    fn from(policy: net::SameSiteCookiePolicy) -> Self {
        match policy {
            net::SameSiteCookiePolicy::All => Self::All,
            net::SameSiteCookiePolicy::NoneOnly => Self::NoneOnly,
        }
    }
}

impl DialRequest {
    #[must_use]
    pub fn new(policy: RequestPolicy, url: &Url, initiator: &url::Origin) -> Self {
        Self {
            policy,
            url: url.clone(),
            frame: None,
            initiator: SerializedOrigin::from(initiator),
            delivery: BodyDelivery::Buffer,
            method: net::Method::GET,
            body: Vec::new(),
            content_type: None,
            headers: Vec::new(),
            referrer: ReferrerRequest::NoReferrer,
            timeout_ms: 0,
            csp: None,
            keepalive: false,
            body_len: 0,
            integrity: None,
            cache: RequestCache::Default,
            referrer_policy: net::ReferrerPolicy::default(),
            cookie_policy: CookiePolicy::All,
            partition_key: None,
        }
    }
}

enumerated::enumerated! {
    #[derive(serde::Serialize, serde::Deserialize)]
    pub BodyDelivery {
        Buffer => "buffer",
        Stream => "stream",
    }
}

impl BodyDelivery {
    #[must_use]
    pub fn from_idl(value: &str) -> Option<Self> {
        Self::KEYWORDS
            .iter()
            .copied()
            .find(|delivery| delivery.keyword() == value)
    }
}

enumerated::enumerated! {
    // https://fetch.spec.whatwg.org/#concept-request-cache-mode
    #[derive(Default, serde::Serialize, serde::Deserialize)]
    pub RequestCache {
        #[default]
        Default => "default",
        NoStore => "no-store",
        Reload => "reload",
        NoCache => "no-cache",
        ForceCache => "force-cache",
        OnlyIfCached => "only-if-cached",
    }
}

impl RequestCache {
    // https://fetch.spec.whatwg.org/#requestinit
    #[must_use]
    pub fn from_idl(value: &str) -> Option<Self> {
        Self::KEYWORDS
            .iter()
            .copied()
            .find(|mode| mode.keyword() == value)
    }
}

enumerated::enumerated! {
    // https://fetch.spec.whatwg.org/#concept-request-mode
    #[derive(serde::Serialize, serde::Deserialize)]
    #[serde(rename_all = "kebab-case")]
    pub CorsMode {
        NoCors => "no-cors",
        SameOrigin => "same-origin",
        Cors => "cors",
    }
}

impl CorsMode {
    // https://fetch.spec.whatwg.org/#requestinit
    #[must_use]
    pub fn from_idl(value: &str) -> Option<Self> {
        Self::KEYWORDS
            .iter()
            .copied()
            .find(|mode| mode.keyword() == value)
    }

    #[must_use]
    pub fn request_mode(self) -> net::cors::RequestMode {
        match self {
            Self::NoCors => net::cors::RequestMode::NoCors,
            Self::SameOrigin => net::cors::RequestMode::SameOrigin,
            Self::Cors => net::cors::RequestMode::Cors,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorsRequest {
    // https://fetch.spec.whatwg.org/#concept-request-mode
    pub mode: CorsMode,
    // https://fetch.spec.whatwg.org/#concept-request-credentials-mode
    pub credentials: net::cors::CredentialsMode,
    // https://fetch.spec.whatwg.org/#concept-request-redirect-mode
    pub redirect: net::cors::RedirectMode,
}

// https://fetch.spec.whatwg.org/#concept-request-mode
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RequestPolicy {
    Fetch(CorsRequest),
    // https://html.spec.whatwg.org/multipage/urls-and-fetching.html#create-a-potential-cors-request
    ClassicScript {
        crossorigin: Option<CrossOrigin>,
    },
    // https://html.spec.whatwg.org/multipage/workers.html#fetch-a-classic-worker-script
    WorkerScript,
    // https://html.spec.whatwg.org/multipage/webappapis.html#fetch-a-single-module-script
    ModuleWorkerScript {
        credentials: net::cors::CredentialsMode,
    },
    // https://html.spec.whatwg.org/multipage/webappapis.html#fetch-a-single-module-script
    ModuleScript {
        credentials: net::cors::CredentialsMode,
    },
    // https://html.spec.whatwg.org/multipage/semantics.html#create-a-link-request
    Stylesheet {
        crossorigin: Option<CrossOrigin>,
    },
    Image {
        crossorigin: Option<CrossOrigin>,
    },
    // https://drafts.csswg.org/css-fonts-4/#font-fetching-requirements
    Font,
    FrameLoad,
    // https://w3c.github.io/reporting/#try-delivery
    CspReportTo,
    // https://w3c.github.io/webappsec-csp/#report-violation — sends no-cors with redirect mode "error", matching Chromium over literal Fetch pre-send rejection
    CspReportUri,
}

impl RequestPolicy {
    #[must_use]
    pub fn kind(self) -> DialKind {
        match self {
            Self::Fetch(_) => DialKind::JsFetch,
            Self::ClassicScript { .. } | Self::WorkerScript => DialKind::ClassicScript,
            Self::ModuleScript { .. } | Self::ModuleWorkerScript { .. } => DialKind::ModuleScript,
            Self::Stylesheet { .. } => DialKind::Stylesheet,
            Self::Image { .. } => DialKind::Image,
            Self::Font => DialKind::Font,
            Self::FrameLoad => DialKind::FrameLoad,
            Self::CspReportTo | Self::CspReportUri => DialKind::CspReport,
        }
    }

    #[must_use]
    pub fn cors(self) -> net::cors::CorsParameters {
        match self {
            Self::Fetch(cors) => net::cors::CorsParameters {
                mode: cors.mode.request_mode(),
                credentials: cors.credentials,
                redirect: cors.redirect,
            },
            Self::ClassicScript { crossorigin }
            | Self::Stylesheet { crossorigin }
            | Self::Image { crossorigin } => potential_cors(crossorigin),
            Self::WorkerScript => net::cors::CorsParameters {
                mode: net::cors::RequestMode::SameOrigin,
                credentials: net::cors::CredentialsMode::SameOrigin,
                redirect: net::cors::RedirectMode::Follow,
            },
            Self::ModuleWorkerScript { credentials } => net::cors::CorsParameters {
                mode: net::cors::RequestMode::SameOrigin,
                credentials,
                redirect: net::cors::RedirectMode::Follow,
            },
            Self::ModuleScript { credentials } => cors_protocol(credentials),
            Self::Font | Self::CspReportTo => cors_protocol(net::cors::CredentialsMode::SameOrigin),
            Self::CspReportUri => net::cors::CorsParameters {
                mode: net::cors::RequestMode::NoCors,
                credentials: net::cors::CredentialsMode::SameOrigin,
                redirect: net::cors::RedirectMode::Error,
            },
            Self::FrameLoad => net::cors::CorsParameters {
                mode: net::cors::RequestMode::Navigate,
                credentials: net::cors::CredentialsMode::Include,
                redirect: net::cors::RedirectMode::Follow,
            },
        }
    }

    // https://fetch.spec.whatwg.org/#concept-filtered-response
    // https://fetch.spec.whatwg.org/#concept-internal-response
    #[must_use]
    pub fn exposes_response(self) -> bool {
        matches!(self, Self::Fetch(_))
    }
}

fn cors_protocol(credentials: net::cors::CredentialsMode) -> net::cors::CorsParameters {
    net::cors::CorsParameters {
        mode: net::cors::RequestMode::Cors,
        credentials,
        redirect: net::cors::RedirectMode::Follow,
    }
}

// https://html.spec.whatwg.org/multipage/urls-and-fetching.html#create-a-potential-cors-request
fn potential_cors(crossorigin: Option<CrossOrigin>) -> net::cors::CorsParameters {
    match crossorigin {
        None => net::cors::CorsParameters {
            mode: net::cors::RequestMode::NoCors,
            credentials: net::cors::CredentialsMode::Include,
            redirect: net::cors::RedirectMode::Follow,
        },
        Some(cross) => cors_protocol(cross.credentials()),
    }
}

// https://html.spec.whatwg.org/multipage/urls-and-fetching.html#cors-settings-attributes
pub fn cross_origin_credentials(crossorigin: Option<CrossOrigin>) -> net::cors::CredentialsMode {
    // https://html.spec.whatwg.org/multipage/urls-and-fetching.html#cors-settings-attributes
    crossorigin.map_or(
        net::cors::CredentialsMode::SameOrigin,
        CrossOrigin::credentials,
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StreamId(u64);

impl StreamId {
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    #[must_use]
    pub const fn mint(id: u64) -> Self {
        Self(id)
    }

    #[must_use]
    pub const fn from_page(id: u64) -> Self {
        Self(id)
    }
}

// https://fetch.spec.whatwg.org/#concept-response-body
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ResponseBody {
    Bytes(Vec<u8>),
    // https://fetch.spec.whatwg.org/#concept-http-network-fetch
    Undecodable,
    Streamed(StreamId),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DialOutcome {
    pub status: u16,
    pub status_text: String,
    // https://fetch.spec.whatwg.org/#concept-filtered-response-opaque
    pub final_url: Url,
    // https://fetch.spec.whatwg.org/#concept-header-list-get-decode-split
    pub content_type: Option<String>,
    pub headers: Vec<(String, String)>,
    pub body: ResponseBody,
    // https://fetch.spec.whatwg.org/#concept-response-type
    pub response_type: net::cors::ResponseType,
    // https://fetch.spec.whatwg.org/#concept-request-response-tainting
    pub crossed_origin: bool,
    pub redirect_count: u32,
    // https://fetch.spec.whatwg.org/#forbidden-response-header-name
    // https://cookiestore.spec.whatwg.org/#process-cookie-changes
    pub cookies_stored: bool,
    // https://html.spec.whatwg.org/multipage/document-lifecycle.html#creating-and-initializing-a-document-object
    pub referrer: Option<Url>,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SerializedOrigin {
    Opaque,
    Tuple(url::Origin),
}

impl SerializedOrigin {
    #[must_use]
    pub fn parse(spec: &str) -> Option<Self> {
        if spec == "null" {
            return Some(Self::Opaque);
        }
        Url::parse(spec).ok().map(|url| Self::from(&url.origin()))
    }

    #[must_use]
    pub fn is_opaque(&self) -> bool {
        matches!(self, Self::Opaque)
    }

    // https://fetch.spec.whatwg.org/#concept-request-origin
    #[must_use]
    pub fn to_url(&self) -> Option<Url> {
        match self {
            Self::Opaque => None,
            Self::Tuple(origin) => net::origin_url(origin),
        }
    }
}

impl From<&url::Origin> for SerializedOrigin {
    fn from(origin: &url::Origin) -> Self {
        match origin {
            url::Origin::Opaque(_) => Self::Opaque,
            url::Origin::Tuple(..) => Self::Tuple(origin.clone()),
        }
    }
}

impl std::fmt::Display for SerializedOrigin {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Opaque => formatter.write_str("null"),
            Self::Tuple(origin) => formatter.write_str(&origin.ascii_serialization()),
        }
    }
}

impl Serialize for SerializedOrigin {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for SerializedOrigin {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).ok_or_else(|| serde::de::Error::custom("invalid origin"))
    }
}

// https://html.spec.whatwg.org/multipage/webstorage.html#the-localstorage-attribute
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct StorageKey(url::Origin);

impl StorageKey {
    #[must_use]
    pub fn from_origin(origin: &url::Origin) -> Option<Self> {
        matches!(origin, url::Origin::Tuple(..)).then(|| Self(origin.clone()))
    }

    #[must_use]
    pub fn parse(spec: &str) -> Option<Self> {
        match SerializedOrigin::parse(spec)? {
            SerializedOrigin::Tuple(origin) => Some(Self(origin)),
            SerializedOrigin::Opaque => None,
        }
    }

    #[must_use]
    pub fn origin(&self) -> &url::Origin {
        &self.0
    }
}

impl std::fmt::Display for StorageKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0.ascii_serialization())
    }
}

impl Serialize for StorageKey {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for StorageKey {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).ok_or_else(|| serde::de::Error::custom("a storage key is a tuple origin"))
    }
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
mod sandbox_flag_tests {
    use super::{SandboxFlag, SandboxFlags};

    #[test]
    fn an_absent_attribute_sets_no_flags() {
        assert!(!SandboxFlags::parse(None).contains(SandboxFlag::Origin));
        assert!(!SandboxFlags::parse(None).scripts_disabled());
    }

    #[test]
    fn every_keyword_clears_exactly_the_flags_the_standard_names() {
        let expected: &[(&str, &[SandboxFlag])] = &[
            (
                "allow-popups",
                &[
                    SandboxFlag::AuxiliaryNavigation,
                    SandboxFlag::CustomProtocolsNavigation,
                ],
            ),
            (
                "allow-top-navigation",
                &[
                    SandboxFlag::TopLevelNavigationWithoutUserActivation,
                    SandboxFlag::TopLevelNavigationWithUserActivation,
                    SandboxFlag::CustomProtocolsNavigation,
                ],
            ),
            (
                "allow-top-navigation-by-user-activation",
                &[SandboxFlag::TopLevelNavigationWithUserActivation],
            ),
            ("allow-same-origin", &[SandboxFlag::Origin]),
            ("allow-forms", &[SandboxFlag::Forms]),
            ("allow-pointer-lock", &[SandboxFlag::PointerLock]),
            (
                "allow-scripts",
                &[SandboxFlag::Scripts, SandboxFlag::AutomaticFeatures],
            ),
            (
                "allow-popups-to-escape-sandbox",
                &[SandboxFlag::PropagatesToAuxiliary],
            ),
            ("allow-modals", &[SandboxFlag::Modals]),
            ("allow-orientation-lock", &[SandboxFlag::OrientationLock]),
            ("allow-presentation", &[SandboxFlag::Presentation]),
            ("allow-downloads", &[SandboxFlag::Downloads]),
            (
                "allow-top-navigation-to-custom-protocols",
                &[SandboxFlag::CustomProtocolsNavigation],
            ),
        ];
        for (keyword, cleared) in expected {
            // https://html.spec.whatwg.org/multipage/iframe-embed-object.html#attr-iframe-sandbox
            for spelling in [(*keyword).to_owned(), keyword.to_ascii_uppercase()] {
                let flags = SandboxFlags::parse(Some(&spelling));
                for flag in SandboxFlag::ALL {
                    assert_eq!(
                        flags.contains(*flag),
                        !cleared.contains(flag),
                        "{spelling}: flag {flag:?}"
                    );
                }
            }
        }
        for (keyword, _) in SandboxFlags::TOKENS {
            assert!(
                expected.iter().any(|(listed, _)| listed == keyword),
                "{keyword} is supported but not in this table"
            );
        }
        assert_eq!(
            SandboxFlag::ALL.len(),
            16,
            "a new flag is added to `SandboxFlag::ALL`"
        );
        let bare = SandboxFlags::parse(Some(""));
        for (_, cleared) in expected {
            for flag in *cleared {
                assert!(bare.contains(*flag), "{flag:?} missing from a bare sandbox");
            }
        }
        assert!(
            bare.contains(SandboxFlag::Navigation) && bare.contains(SandboxFlag::DocumentDomain)
        );
        for flag in SandboxFlag::ALL {
            let clearable = expected.iter().any(|(_, flags)| flags.contains(flag));
            let always_set = matches!(flag, SandboxFlag::Navigation | SandboxFlag::DocumentDomain);
            assert!(
                clearable || always_set,
                "{flag:?} is neither clearable nor always set"
            );
        }
    }

    // https://html.spec.whatwg.org/multipage/browsers.html#parse-a-sandboxing-directive
    #[test]
    fn a_sandbox_with_two_keywords_clears_both() {
        let flags = SandboxFlags::parse(Some("allow-scripts allow-forms"));
        assert!(!flags.scripts_disabled());
        assert!(!flags.contains(SandboxFlag::Forms));
        assert!(
            flags.contains(SandboxFlag::Origin),
            "a keyword it does not name stays clear"
        );
    }

    #[test]
    fn supports_ignores_ascii_case_and_rejects_unlisted_keywords() {
        assert!(SandboxFlags::supports("allow-scripts"));
        assert!(SandboxFlags::supports("ALLOW-SCRIPTS"));
        assert!(SandboxFlags::supports("Allow-Same-Origin"));
        assert!(!SandboxFlags::supports("allow-frames"));
        assert!(!SandboxFlags::supports(
            "allow-storage-access-by-user-activation"
        ));
    }

    #[test]
    fn an_embedder_sandbox_is_unioned_into_the_child() {
        let parent = SandboxFlags::parse(Some("allow-scripts"));
        let child = SandboxFlags::parse(Some("allow-same-origin")).union(parent);
        assert!(child.scripts_disabled());
        assert!(child.origin_is_opaque());
    }

    #[test]
    fn sandbox_propagation_lets_an_escaping_popup_out() {
        let propagating = SandboxFlags::parse(Some("allow-popups"));
        assert!(propagating.auxiliary_flags().origin_is_opaque());
        let escaping = SandboxFlags::parse(Some("allow-popups-to-escape-sandbox"));
        assert_eq!(
            escaping.auxiliary_flags(),
            SandboxFlags::empty(),
            "an escaping popup starts unsandboxed"
        );
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
