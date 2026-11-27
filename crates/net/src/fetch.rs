// https://fetch.spec.whatwg.org/#concept-request
// https://fetch.spec.whatwg.org/#concept-response

use enumerated::Enumerated;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::cors::CrossOrigin;

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
            crate::combined_header_in(headers, "content-security-policy").as_deref(),
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
            Self::Tuple(origin) => crate::origin_url(origin),
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
    pub method: crate::Method,
    pub body: Vec<u8>,
    pub content_type: Option<String>,
    pub headers: Vec<(String, String)>,
    // https://fetch.spec.whatwg.org/#concept-request-referrer
    pub referrer: ReferrerRequest,
    // https://xhr.spec.whatwg.org/#the-timeout-attribute
    pub timeout_ms: u64,
    pub csp: Option<Box<crate::csp::RedirectGate>>,
    // https://fetch.spec.whatwg.org/#keepalive-flag
    pub keepalive: bool,
    pub body_len: u64,
    // https://www.w3.org/TR/SRI/#verify-integrity
    pub integrity: Option<String>,
    // https://fetch.spec.whatwg.org/#concept-request-cache-mode
    pub cache: RequestCache,
    // https://fetch.spec.whatwg.org/#concept-request-referrer-policy
    pub referrer_policy: crate::ReferrerPolicy,
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

impl From<crate::SameSiteCookiePolicy> for CookiePolicy {
    fn from(policy: crate::SameSiteCookiePolicy) -> Self {
        match policy {
            crate::SameSiteCookiePolicy::All => Self::All,
            crate::SameSiteCookiePolicy::NoneOnly => Self::NoneOnly,
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
            method: crate::Method::GET,
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
            referrer_policy: crate::ReferrerPolicy::default(),
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
    pub fn request_mode(self) -> crate::cors::RequestMode {
        match self {
            Self::NoCors => crate::cors::RequestMode::NoCors,
            Self::SameOrigin => crate::cors::RequestMode::SameOrigin,
            Self::Cors => crate::cors::RequestMode::Cors,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorsRequest {
    // https://fetch.spec.whatwg.org/#concept-request-mode
    pub mode: CorsMode,
    // https://fetch.spec.whatwg.org/#concept-request-credentials-mode
    pub credentials: crate::cors::CredentialsMode,
    // https://fetch.spec.whatwg.org/#concept-request-redirect-mode
    pub redirect: crate::cors::RedirectMode,
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
        credentials: crate::cors::CredentialsMode,
    },
    // https://html.spec.whatwg.org/multipage/webappapis.html#fetch-a-single-module-script
    ModuleScript {
        credentials: crate::cors::CredentialsMode,
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
    pub fn cors(self) -> crate::cors::CorsParameters {
        match self {
            Self::Fetch(cors) => crate::cors::CorsParameters {
                mode: cors.mode.request_mode(),
                credentials: cors.credentials,
                redirect: cors.redirect,
            },
            Self::ClassicScript { crossorigin }
            | Self::Stylesheet { crossorigin }
            | Self::Image { crossorigin } => potential_cors(crossorigin),
            Self::WorkerScript => crate::cors::CorsParameters {
                mode: crate::cors::RequestMode::SameOrigin,
                credentials: crate::cors::CredentialsMode::SameOrigin,
                redirect: crate::cors::RedirectMode::Follow,
            },
            Self::ModuleWorkerScript { credentials } => crate::cors::CorsParameters {
                mode: crate::cors::RequestMode::SameOrigin,
                credentials,
                redirect: crate::cors::RedirectMode::Follow,
            },
            Self::ModuleScript { credentials } => cors_protocol(credentials),
            Self::Font | Self::CspReportTo => {
                cors_protocol(crate::cors::CredentialsMode::SameOrigin)
            }
            Self::CspReportUri => crate::cors::CorsParameters {
                mode: crate::cors::RequestMode::NoCors,
                credentials: crate::cors::CredentialsMode::SameOrigin,
                redirect: crate::cors::RedirectMode::Error,
            },
            Self::FrameLoad => crate::cors::CorsParameters {
                mode: crate::cors::RequestMode::Navigate,
                credentials: crate::cors::CredentialsMode::Include,
                redirect: crate::cors::RedirectMode::Follow,
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

fn cors_protocol(credentials: crate::cors::CredentialsMode) -> crate::cors::CorsParameters {
    crate::cors::CorsParameters {
        mode: crate::cors::RequestMode::Cors,
        credentials,
        redirect: crate::cors::RedirectMode::Follow,
    }
}

// https://html.spec.whatwg.org/multipage/urls-and-fetching.html#create-a-potential-cors-request
fn potential_cors(crossorigin: Option<CrossOrigin>) -> crate::cors::CorsParameters {
    match crossorigin {
        None => crate::cors::CorsParameters {
            mode: crate::cors::RequestMode::NoCors,
            credentials: crate::cors::CredentialsMode::Include,
            redirect: crate::cors::RedirectMode::Follow,
        },
        Some(cross) => cors_protocol(cross.credentials()),
    }
}

// https://html.spec.whatwg.org/multipage/urls-and-fetching.html#cors-settings-attributes
#[must_use]
pub fn cross_origin_credentials(crossorigin: Option<CrossOrigin>) -> crate::cors::CredentialsMode {
    crossorigin.map_or(
        crate::cors::CredentialsMode::SameOrigin,
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
    pub response_type: crate::cors::ResponseType,
    // https://fetch.spec.whatwg.org/#concept-request-response-tainting
    pub crossed_origin: bool,
    pub redirect_count: u32,
    // https://fetch.spec.whatwg.org/#forbidden-response-header-name
    // https://cookiestore.spec.whatwg.org/#process-cookie-changes
    pub cookies_stored: bool,
    // https://html.spec.whatwg.org/multipage/document-lifecycle.html#creating-and-initializing-a-document-object
    pub referrer: Option<Url>,
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
