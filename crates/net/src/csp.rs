// https://w3c.github.io/webappsec-csp/#parse-serialized-policy
// https://w3c.github.io/webappsec-csp/#should-request-be-blocked-by-content-security-policy
// https://w3c.github.io/webappsec-csp/#should-elements-inline-type-behavior-be-blocked-by-content-security-policy

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256, Sha384, Sha512};
use url::{Host, Origin, Url};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Source {
    Header,
    Meta,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Disposition {
    Enforce,
    Report,
}

impl Disposition {
    // https://w3c.github.io/webappsec-csp/#deprecated-serialize-violation
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Enforce => "enforce",
            Self::Report => "report",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Policy {
    directives: Vec<(String, Vec<String>)>,
    disposition: Disposition,
    // https://w3c.github.io/webappsec-csp/#parse-serialized-policy
    serialized: String,
}

// https://w3c.github.io/webappsec-csp/#report-violation
#[derive(Clone)]
pub struct Violation {
    // https://w3c.github.io/webappsec-csp/#violation-effective-directive
    pub effective_directive: String,
    pub original_policy: String,
    pub disposition: Disposition,
    pub blocked_uri: String,
    pub sample: Option<String>,
    // https://w3c.github.io/webappsec-csp/#deprecated-serialize-violation
    pub document_uri: String,
    // https://w3c.github.io/webappsec-csp/#strip-url-for-use-in-reports
    pub source_file: Option<String>,
    pub line_number: u32,
    pub column_number: u32,
    pub status_code: u16,
    // https://w3c.github.io/webappsec-csp/#directive-report-uri
    pub report_uris: Vec<String>,
    // https://w3c.github.io/webappsec-csp/#directive-report-to
    pub report_to: Vec<String>,
    // https://w3c.github.io/webappsec-csp/#report-violation
    pub element: Option<(u32, u64)>,
}

impl Violation {
    // https://w3c.github.io/webappsec-csp/#disposition
    #[must_use]
    pub fn blocks(&self) -> bool {
        self.disposition == Disposition::Enforce
    }
}

#[must_use]
pub fn blocks(violations: &[Violation]) -> bool {
    violations.iter().any(Violation::blocks)
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct List {
    policies: Vec<Policy>,
}

impl List {
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            policies: Vec::new(),
        }
    }
}

pub struct InlineScript<'a> {
    pub source: &'a str,
    pub nonce: &'a str,
    // https://w3c.github.io/webappsec-csp/#is-element-nonceable
    pub nonceable: bool,
    pub parser_inserted: bool,
    pub kind: InlineKind,
}

#[derive(Clone, Copy)]
pub enum InlineKind {
    Script,
    Navigation,
    ScriptAttribute,
    Evaluation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Destination {
    Connect,
    Script,
    StyleSheet,
    Img,
    Font,
    Frame,
    Worker,
}

impl Destination {
    #[must_use]
    pub fn is_image(self) -> bool {
        self == Self::Img
    }
}

// https://w3c.github.io/webappsec-csp/#effective-directive
#[derive(Clone, Copy, PartialEq, Eq)]
enum Effective {
    ScriptSrcElem,
    ScriptSrcAttr,
    ScriptSrc,
    StyleSrcElem,
    ConnectSrc,
    ImgSrc,
    FontSrc,
    FrameSrc,
    WorkerSrc,
}

impl Effective {
    fn name(self) -> &'static str {
        match self {
            Self::ScriptSrcElem => "script-src-elem",
            Self::ScriptSrcAttr => "script-src-attr",
            Self::ScriptSrc => "script-src",
            Self::StyleSrcElem => "style-src-elem",
            Self::ConnectSrc => "connect-src",
            Self::ImgSrc => "img-src",
            Self::FontSrc => "font-src",
            Self::FrameSrc => "frame-src",
            Self::WorkerSrc => "worker-src",
        }
    }

    // https://w3c.github.io/webappsec-csp/#directive-fallback-list
    fn fallback(self) -> &'static [&'static str] {
        match self {
            Self::ScriptSrcElem => &["script-src-elem", "script-src", "default-src"],
            Self::ScriptSrcAttr => &["script-src-attr", "script-src", "default-src"],
            Self::ScriptSrc => &["script-src", "default-src"],
            Self::StyleSrcElem => &["style-src-elem", "style-src", "default-src"],
            Self::ConnectSrc => &["connect-src", "default-src"],
            Self::ImgSrc => &["img-src", "default-src"],
            Self::FontSrc => &["font-src", "default-src"],
            // https://w3c.github.io/webappsec-csp/#directive-fallback-list
            Self::FrameSrc => &["frame-src", "child-src", "default-src"],
            Self::WorkerSrc => &["worker-src", "child-src", "script-src", "default-src"],
        }
    }
}

impl Destination {
    fn effective(self) -> Effective {
        match self {
            Self::Connect => Effective::ConnectSrc,
            Self::Script => Effective::ScriptSrcElem,
            Self::StyleSheet => Effective::StyleSrcElem,
            Self::Img => Effective::ImgSrc,
            Self::Font => Effective::FontSrc,
            Self::Frame => Effective::FrameSrc,
            Self::Worker => Effective::WorkerSrc,
        }
    }
}

impl InlineScript<'_> {
    fn effective(&self) -> Effective {
        match self.kind {
            InlineKind::Script | InlineKind::Navigation => Effective::ScriptSrcElem,
            InlineKind::ScriptAttribute => Effective::ScriptSrcAttr,
            InlineKind::Evaluation => Effective::ScriptSrc,
        }
    }

    // https://w3c.github.io/webappsec-csp/#violation-blocked-uri
    fn blocked_uri(&self) -> String {
        match self.kind {
            InlineKind::Evaluation => "eval".to_owned(),
            InlineKind::Script | InlineKind::Navigation | InlineKind::ScriptAttribute => {
                "inline".to_owned()
            }
        }
    }

    // https://w3c.github.io/webappsec-csp/#violation-sample
    fn sample(&self) -> String {
        self.source.chars().take(40).collect()
    }
}

#[derive(Clone, Copy)]
pub struct ScriptRequest<'a> {
    pub nonce: &'a str,
    pub parser_inserted: bool,
    pub integrity: &'a str,
}

// https://w3c.github.io/webappsec-csp/#violation-blocked-uri
// https://crbug.com/633306
fn blocked_uri(origin: &Origin, effective: Effective, url: &Url) -> String {
    if effective == Effective::FrameSrc && url.origin() != *origin {
        let port = url
            .port_or_known_default()
            .map_or_else(String::new, |port| format!(":{port}"));
        return format!(
            "{}://{}{}",
            url.scheme(),
            url.host_str().unwrap_or_default(),
            port
        );
    }
    strip_url(url)
}

// https://w3c.github.io/webappsec-csp/#strip-url-for-use-in-reports
fn strip_url(url: &Url) -> String {
    if !matches!(url.scheme(), "http" | "https" | "ws" | "wss") {
        return url.scheme().to_owned();
    }
    let mut clean = url.clone();
    clean.set_fragment(None);
    let _ = clean.set_username("");
    let _ = clean.set_password(None);
    clean.as_str().to_owned()
}

#[must_use]
pub fn strip_url_for_reports(raw: &str) -> String {
    Url::parse(raw).map_or_else(|_| String::new(), |url| strip_url(&url))
}

fn report_uris(policy: &Policy) -> Vec<String> {
    policy
        .directives
        .iter()
        .find(|(name, _)| name == "report-uri")
        .map_or_else(Vec::new, |(_, values)| values.clone())
}

fn report_to(policy: &Policy) -> Vec<String> {
    policy
        .directives
        .iter()
        .find(|(name, _)| name == "report-to")
        .map_or_else(Vec::new, |(_, values)| values.clone())
}

#[must_use]
pub fn parse_reporting_endpoints_value(combined: &str) -> Vec<(String, String)> {
    let mut endpoints = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut items = Vec::new();
    for c in combined.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                current.push(c);
            }
            ',' if !quoted => {
                items.push(std::mem::take(&mut current));
            }
            c => current.push(c),
        }
    }
    items.push(current);
    for item in items {
        let (name, url) = item.split_once('=').unwrap_or((item.as_str(), ""));
        let name = name.trim().to_owned();
        let url = url.trim().trim_matches('"').to_owned();
        if !name.is_empty() && !url.is_empty() {
            endpoints.push((name, url));
        }
    }
    endpoints
}

impl List {
    // https://w3c.github.io/webappsec-upgrade-insecure-requests/#upgrade-insecure-requests
    #[must_use]
    pub fn upgrades_insecure_requests(&self) -> bool {
        self.policies.iter().any(|policy| {
            policy.disposition == Disposition::Enforce
                && policy
                    .directives
                    .iter()
                    .any(|(name, _)| name == "upgrade-insecure-requests")
        })
    }

    // https://w3c.github.io/webappsec-csp/#sandbox
    #[must_use]
    pub fn sandbox_flags(&self) -> crate::fetch::SandboxFlags {
        let mut flags = crate::fetch::SandboxFlags::empty();
        for policy in self
            .policies
            .iter()
            .filter(|policy| policy.disposition == Disposition::Enforce)
        {
            if let Some((_, value)) = policy.directives.iter().find(|(name, _)| name == "sandbox") {
                flags = flags.union(crate::fetch::SandboxFlags::parse(Some(&value.join(" "))));
            }
        }
        flags
    }

    #[must_use]
    pub fn from_headers(enforce: Option<&str>, report_only: Option<&str>) -> Self {
        let mut policies = Vec::new();
        if let Some(header) = enforce {
            policies.extend(parse_policy_list(
                header,
                Source::Header,
                Disposition::Enforce,
            ));
        }
        if let Some(header) = report_only {
            policies.extend(parse_policy_list(
                header,
                Source::Header,
                Disposition::Report,
            ));
        }
        Self { policies }
    }

    // https://html.spec.whatwg.org/multipage/browsers.html#initialize-a-policy-container-from-a-fetch-response
    pub fn append(&mut self, other: Self) {
        self.policies.extend(other.policies);
    }

    // https://w3c.github.io/webappsec-csp/#meta-element
    pub fn push_meta(&mut self, content: &str) {
        let mut policy = parse_serialized(content, Source::Meta, Disposition::Enforce);
        policy.directives.retain(|(name, _)| {
            !matches!(
                name.as_str(),
                "report-uri" | "report-to" | "frame-ancestors" | "sandbox"
            )
        });
        if !policy.directives.is_empty() {
            self.policies.push(policy);
        }
    }

    // https://w3c.github.io/webappsec-csp/#should-request-be-blocked-by-content-security-policy
    #[must_use]
    pub fn check_request(
        &self,
        origin: &Origin,
        document_url: &Url,
        url: &Url,
        destination: Destination,
        script: Option<ScriptRequest<'_>>,
    ) -> Vec<Violation> {
        self.violations(origin, document_url, url, destination, script, 0)
    }

    // https://w3c.github.io/webappsec-csp/#should-block-response
    // https://w3c.github.io/webappsec-csp/#create-violation-for-request
    #[must_use]
    pub fn check_redirected_request(
        &self,
        origin: &Origin,
        document_url: &Url,
        response_url: &Url,
        blocked_url: &Url,
        destination: Destination,
    ) -> Vec<Violation> {
        let mut violations =
            self.violations(origin, document_url, response_url, destination, None, 1);
        let blocked = blocked_uri(origin, destination.effective(), blocked_url);
        for violation in &mut violations {
            violation.blocked_uri.clone_from(&blocked);
        }
        violations
    }

    fn violations(
        &self,
        origin: &Origin,
        document_url: &Url,
        url: &Url,
        destination: Destination,
        script: Option<ScriptRequest<'_>>,
        redirects: u32,
    ) -> Vec<Violation> {
        let effective = destination.effective();
        let mut violations = Vec::new();
        for policy in &self.policies {
            if policy_request_violation(
                policy,
                origin,
                document_url,
                url,
                destination,
                script,
                redirects,
            ) {
                violations.push(Violation {
                    // https://w3c.github.io/webappsec-csp/#get-the-effective-directive
                    effective_directive: effective.name().to_owned(),
                    original_policy: policy.serialized.clone(),
                    disposition: policy.disposition,
                    blocked_uri: blocked_uri(origin, effective, url),
                    sample: None,
                    document_uri: strip_url(document_url),
                    source_file: None,
                    line_number: 0,
                    column_number: 0,
                    status_code: 0,
                    report_uris: report_uris(policy),
                    report_to: report_to(policy),
                    element: None,
                });
            }
        }
        violations
    }

    // https://w3c.github.io/webappsec-csp/#is-base-allowed-for-document — blockedURI reports the resolved URL, matching Chrome and WPT rather than "inline"
    #[must_use]
    pub fn check_base(&self, origin: &Origin, document_url: &Url, base: &Url) -> Vec<Violation> {
        let mut violations = Vec::new();
        for policy in &self.policies {
            let Some(list) = directive_value(policy, "base-uri") else {
                continue;
            };
            // https://w3c.github.io/webappsec-csp/#directive-fallback-list
            if url_matches_list(base, list, origin, document_url, 0) {
                continue;
            }
            violations.push(Violation {
                effective_directive: "base-uri".to_owned(),
                original_policy: policy.serialized.clone(),
                disposition: policy.disposition,
                blocked_uri: strip_url(base),
                sample: None,
                document_uri: strip_url(document_url),
                source_file: None,
                line_number: 0,
                column_number: 0,
                status_code: 0,
                report_uris: report_uris(policy),
                report_to: report_to(policy),
                element: None,
            });
        }
        violations
    }

    // https://w3c.github.io/webappsec-csp/#should-elements-inline-type-behavior-be-blocked-by-content-security-policy
    #[must_use]
    pub fn check_inline(&self, document_url: &Url, inline: &InlineScript<'_>) -> Vec<Violation> {
        let effective = inline.effective();
        let mut violations = Vec::new();
        for policy in &self.policies {
            if let Some(list) = policy_inline_violation(policy, inline) {
                violations.push(Violation {
                    effective_directive: effective.name().to_owned(),
                    original_policy: policy.serialized.clone(),
                    disposition: policy.disposition,
                    blocked_uri: inline.blocked_uri(),
                    // https://w3c.github.io/webappsec-csp/#violation-sample
                    sample: has_keyword(list, "'report-sample'").then(|| inline.sample()),
                    document_uri: strip_url(document_url),
                    source_file: None,
                    line_number: 0,
                    column_number: 0,
                    status_code: 0,
                    report_uris: report_uris(policy),
                    report_to: report_to(policy),
                    element: None,
                });
            }
        }
        violations
    }

    // https://w3c.github.io/webappsec-csp/#frame-ancestors-navigation-response
    #[must_use]
    pub fn check_frame_ancestors(&self, child_url: &Url, ancestors: &[Url]) -> Vec<Violation> {
        let origin = child_url.origin();
        let mut violations = Vec::new();
        for policy in &self.policies {
            let Some(list) = directive_value(policy, "frame-ancestors") else {
                continue;
            };
            let allowed = ancestors
                .iter()
                .all(|ancestor| url_matches_list(ancestor, list, &origin, child_url, 0));
            if !allowed {
                violations.push(Violation {
                    effective_directive: "frame-ancestors".to_owned(),
                    original_policy: policy.serialized.clone(),
                    disposition: policy.disposition,
                    blocked_uri: strip_url(child_url),
                    sample: None,
                    document_uri: strip_url(child_url),
                    source_file: None,
                    line_number: 0,
                    column_number: 0,
                    status_code: 0,
                    report_uris: report_uris(policy),
                    report_to: report_to(policy),
                    element: None,
                });
            }
        }
        violations
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct TupleOrigin {
    scheme: String,
    host: String,
    port: u16,
}

impl TupleOrigin {
    fn from_origin(origin: &Origin) -> Option<Self> {
        match origin {
            Origin::Tuple(scheme, host, port) => Some(Self {
                scheme: scheme.clone(),
                host: host.to_string(),
                port: *port,
            }),
            Origin::Opaque(_) => None,
        }
    }

    fn to_origin(&self) -> Origin {
        let host = Host::parse(&self.host).unwrap_or(Host::Domain(self.host.clone()));
        Origin::Tuple(self.scheme.clone(), host, self.port)
    }
}

// https://w3c.github.io/webappsec-csp/#should-request-be-blocked-by-content-security-policy
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RedirectGate {
    list: List,
    origin: Option<TupleOrigin>,
    destination: Destination,
}

impl RedirectGate {
    #[must_use]
    pub fn new(list: List, origin: &Origin, destination: Destination) -> Self {
        Self {
            list,
            origin: TupleOrigin::from_origin(origin),
            destination,
        }
    }

    #[must_use]
    pub fn allows(&self, document_url: &Url, url: &Url, redirects: u32) -> bool {
        let origin = self
            .origin
            .as_ref()
            .map_or_else(Origin::new_opaque, TupleOrigin::to_origin);
        !blocks(&self.list.violations(
            &origin,
            document_url,
            url,
            self.destination,
            None,
            redirects,
        ))
    }

    // https://w3c.github.io/webappsec-csp/#create-violation-for-request
    pub fn redirected_violations(
        &self,
        document_url: &Url,
        response_url: &Url,
        blocked_url: &Url,
    ) -> Vec<Violation> {
        let origin = self
            .origin
            .as_ref()
            .map_or_else(Origin::new_opaque, TupleOrigin::to_origin);
        self.list.check_redirected_request(
            &origin,
            document_url,
            response_url,
            blocked_url,
            self.destination,
        )
    }
}

// https://w3c.github.io/webappsec-csp/#should-request-be-blocked-by-content-security-policy
fn policy_request_violation(
    policy: &Policy,
    origin: &Origin,
    document_url: &Url,
    url: &Url,
    destination: Destination,
    script: Option<ScriptRequest<'_>>,
    redirects: u32,
) -> bool {
    let effective = destination.effective();
    for &name in effective.fallback() {
        let Some(list) = directive_value(policy, name) else {
            continue;
        };
        // https://w3c.github.io/webappsec-csp/#effective-directive
        let script_semantics = matches!(name, "script-src" | "script-src-elem");
        return request_check_blocks(
            script_semantics,
            list,
            origin,
            document_url,
            url,
            script,
            redirects,
        );
    }
    false
}

fn policy_inline_violation<'a>(
    policy: &'a Policy,
    inline: &InlineScript<'_>,
) -> Option<&'a [String]> {
    let effective = inline.effective();
    for &name in effective.fallback() {
        let Some(list) = directive_value(policy, name) else {
            continue;
        };
        return inline_check_blocks(effective, list, inline).then_some(list);
    }
    None
}

fn parse_policy_list(header: &str, source: Source, disposition: Disposition) -> Vec<Policy> {
    split_commas(header)
        .filter_map(|token| {
            let policy = parse_serialized(token, source, disposition);
            (!policy.directives.is_empty()).then_some(policy)
        })
        .collect()
}

// https://w3c.github.io/webappsec-csp/#parse-serialized-policy
fn parse_serialized(serialized: &str, _source: Source, disposition: Disposition) -> Policy {
    let mut directives = Vec::new();
    for token in serialized.split(';') {
        let token = trim_ascii(token);
        if token.is_empty() || !token.is_ascii() {
            continue;
        }
        let mut parts = split_ascii_whitespace(token);
        let Some(name) = parts.next() else {
            continue;
        };
        let name = name.to_ascii_lowercase();
        if directives.iter().any(|(existing, _)| existing == &name) {
            continue;
        }
        let value = parts.map(str::to_owned).collect();
        directives.push((name, value));
    }
    Policy {
        directives,
        disposition,
        serialized: trim_ascii(serialized).to_owned(),
    }
}

fn request_check_blocks(
    script_semantics: bool,
    list: &[String],
    origin: &Origin,
    document_url: &Url,
    url: &Url,
    script: Option<ScriptRequest<'_>>,
    redirects: u32,
) -> bool {
    // https://w3c.github.io/webappsec-csp/#script-pre-request
    if redirects == 0 && script_semantics {
        let script = script.unwrap_or(ScriptRequest {
            nonce: "",
            parser_inserted: true,
            integrity: "",
        });
        if nonce_matches(script.nonce, list) {
            return false;
        }
        if integrity_matches(list, script.integrity) {
            return false;
        }
        if has_keyword(list, "'strict-dynamic'") {
            return script.parser_inserted;
        }
    }
    !url_matches_list(url, list, origin, document_url, redirects)
}

// https://w3c.github.io/webappsec-csp/#does-integrity-metadata-match-source-list
fn integrity_matches(list: &[String], integrity: &str) -> bool {
    let expressions: Vec<(&str, &str)> = list
        .iter()
        .filter_map(|expression| hash_source(expression))
        .collect();
    if expressions.is_empty() {
        return false;
    }
    let sources = parse_integrity_metadata(integrity);
    if sources.is_empty() {
        return false;
    }
    sources.iter().all(|(algorithm, value)| {
        expressions
            .iter()
            .any(|(expected_algorithm, expected_value)| {
                expected_algorithm.eq_ignore_ascii_case(algorithm) && *expected_value == *value
            })
    })
}

// https://www.w3.org/TR/SRI/#parse-metadata
fn parse_integrity_metadata(metadata: &str) -> Vec<(String, String)> {
    metadata
        .split_ascii_whitespace()
        .filter_map(|entry| {
            let (algorithm, rest) = entry.split_once('-')?;
            let value = rest.split_once('?').map_or(rest, |(value, _)| value);
            let algorithm = if algorithm.eq_ignore_ascii_case("sha256") {
                "sha256"
            } else if algorithm.eq_ignore_ascii_case("sha384") {
                "sha384"
            } else if algorithm.eq_ignore_ascii_case("sha512") {
                "sha512"
            } else {
                return None;
            };
            is_base64_value(value).then(|| (algorithm.to_owned(), value.to_owned()))
        })
        .collect()
}

// https://www.w3.org/TR/SRI/#parse-metadata
fn normalize_base64(value: &str) -> String {
    let mut normalized = value.replace('-', "+").replace('_', "/");
    while !normalized.len().is_multiple_of(4) {
        normalized.push('=');
    }
    normalized
}

// https://www.w3.org/TR/SRI/#does-response-match-metadatalist
#[must_use]
pub fn integrity_verifies(integrity: &str, bytes: &[u8]) -> bool {
    let integrity = integrity.trim();
    if integrity.is_empty() {
        return true;
    }
    let sources = parse_integrity_metadata(integrity);
    let Some(strongest) = sources
        .iter()
        .map(|(algorithm, _)| algorithm.as_str())
        .max_by_key(|algorithm| match *algorithm {
            "sha512" => 2,
            "sha384" => 1,
            _ => 0,
        })
    else {
        return true;
    };
    sources.iter().any(|(algorithm, expected)| {
        algorithm == strongest && {
            let digest = match strongest {
                "sha512" => base64::engine::general_purpose::STANDARD.encode(Sha512::digest(bytes)),
                "sha384" => base64::engine::general_purpose::STANDARD.encode(Sha384::digest(bytes)),
                _ => base64::engine::general_purpose::STANDARD.encode(Sha256::digest(bytes)),
            };
            digest == normalize_base64(expected)
        }
    })
}

// https://w3c.github.io/webappsec-csp/#should-block-inline
fn inline_check_blocks(effective: Effective, list: &[String], inline: &InlineScript<'_>) -> bool {
    match effective {
        Effective::ScriptSrc => evaluation_blocks(list),
        Effective::ScriptSrcElem | Effective::ScriptSrcAttr => !element_matches(list, inline),
        Effective::StyleSrcElem
        | Effective::ConnectSrc
        | Effective::ImgSrc
        | Effective::FontSrc
        | Effective::FrameSrc
        | Effective::WorkerSrc => false,
    }
}

// https://w3c.github.io/webappsec-csp/#can-compile-strings
// https://w3c.github.io/webappsec-csp/#directive-script-src
fn evaluation_blocks(list: &[String]) -> bool {
    !has_keyword(list, "'unsafe-eval'")
}

fn directive_value<'a>(policy: &'a Policy, name: &str) -> Option<&'a [String]> {
    policy
        .directives
        .iter()
        .find(|(directive, _)| directive == name)
        .map(|(_, value)| value.as_slice())
}

// https://w3c.github.io/webappsec-csp/#match-nonce-to-source-list
fn nonce_matches(nonce: &str, list: &[String]) -> bool {
    if nonce.is_empty() {
        return false;
    }
    list.iter().any(|expression| {
        strip_prefix_ignore_ascii_case(expression, "'nonce-")
            .and_then(|rest| rest.strip_suffix('\''))
            .is_some_and(|value| value == nonce && is_base64_value(value))
    })
}

fn has_keyword(list: &[String], keyword: &str) -> bool {
    list.iter()
        .any(|expression| expression.eq_ignore_ascii_case(keyword))
}

// https://w3c.github.io/webappsec-csp/#match-element-to-source-list
fn element_matches(list: &[String], inline: &InlineScript<'_>) -> bool {
    if allows_all_inline(list, inline.kind) {
        return true;
    }
    // https://w3c.github.io/webappsec-csp/#match-element-to-source-list
    let nonce_applies = matches!(inline.kind, InlineKind::Script);
    if nonce_applies && inline.nonceable && nonce_matches(inline.nonce, list) {
        return true;
    }
    let unsafe_hashes = has_keyword(list, "'unsafe-hashes'");
    let hash_applies = matches!(inline.kind, InlineKind::Script) || unsafe_hashes;
    if !hash_applies {
        return false;
    }
    for expression in list {
        if expression.eq_ignore_ascii_case("'strict-dynamic'")
            && matches!(inline.kind, InlineKind::Script)
            && !inline.parser_inserted
        {
            return true;
        }
        if hash_matches(expression, inline.source) {
            return true;
        }
    }
    false
}

// https://w3c.github.io/webappsec-csp/#allow-all-inline
fn allows_all_inline(list: &[String], kind: InlineKind) -> bool {
    let mut allow = false;
    for expression in list {
        if is_nonce_source(expression) || is_hash_source(expression) {
            return false;
        }
        if matches!(
            kind,
            InlineKind::Script | InlineKind::ScriptAttribute | InlineKind::Navigation
        ) && expression.eq_ignore_ascii_case("'strict-dynamic'")
        {
            return false;
        }
        if expression.eq_ignore_ascii_case("'unsafe-inline'") {
            allow = true;
        }
    }
    allow
}

fn hash_matches(expression: &str, source: &str) -> bool {
    let Some((algorithm, expected)) = hash_source(expression) else {
        return false;
    };
    let digest = match algorithm {
        "sha256" => {
            base64::engine::general_purpose::STANDARD.encode(Sha256::digest(source.as_bytes()))
        }
        "sha384" => {
            base64::engine::general_purpose::STANDARD.encode(Sha384::digest(source.as_bytes()))
        }
        "sha512" => {
            base64::engine::general_purpose::STANDARD.encode(Sha512::digest(source.as_bytes()))
        }
        _ => return false,
    };
    let expected = expected.replace('-', "+").replace('_', "/");
    digest == expected
}

fn is_nonce_source(expression: &str) -> bool {
    let Some(rest) = strip_prefix_ignore_ascii_case(expression, "'nonce-") else {
        return false;
    };
    let Some(value) = rest.strip_suffix('\'') else {
        return false;
    };
    is_base64_value(value)
}

fn is_hash_source(expression: &str) -> bool {
    hash_source(expression).is_some()
}

fn hash_source(expression: &str) -> Option<(&str, &str)> {
    let rest = expression.strip_prefix('\'')?;
    let (algorithm, rest) = rest.split_once('-')?;
    let algorithm = if algorithm.eq_ignore_ascii_case("sha256") {
        "sha256"
    } else if algorithm.eq_ignore_ascii_case("sha384") {
        "sha384"
    } else if algorithm.eq_ignore_ascii_case("sha512") {
        "sha512"
    } else {
        return None;
    };
    let value = rest.strip_suffix('\'')?;
    is_base64_value(value).then_some((algorithm, value))
}

fn is_base64_value(value: &str) -> bool {
    let mut saw_data = false;
    let mut padding = 0;
    for byte in value.bytes() {
        if padding > 0 {
            if byte == b'=' {
                padding += 1;
                if padding > 2 {
                    return false;
                }
                continue;
            }
            return false;
        }
        if byte == b'=' {
            if !saw_data {
                return false;
            }
            padding = 1;
            continue;
        }
        if byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'-' | b'_') {
            saw_data = true;
        } else {
            return false;
        }
    }
    saw_data
}

// https://w3c.github.io/webappsec-csp/#match-url-to-source-list
fn url_matches_list(
    url: &Url,
    list: &[String],
    origin: &Origin,
    document_url: &Url,
    redirects: u32,
) -> bool {
    if list.is_empty() {
        return false;
    }
    if list.len() == 1 && list[0].eq_ignore_ascii_case("'none'") {
        return false;
    }
    list.iter()
        .any(|expression| url_matches_expression(url, expression, origin, document_url, redirects))
}

fn url_matches_expression(
    url: &Url,
    expression: &str,
    origin: &Origin,
    document_url: &Url,
    redirects: u32,
) -> bool {
    if expression == "*" {
        return url.scheme() == "http"
            || url.scheme() == "https"
            || origin_scheme(origin)
                .is_some_and(|scheme| scheme.eq_ignore_ascii_case(url.scheme()));
    }
    if let Some(parsed) = parse_source(expression) {
        return match parsed {
            SourceExpr::Scheme(scheme) => scheme_part_matches(&scheme, url.scheme()),
            SourceExpr::Host {
                scheme,
                host,
                port,
                path,
            } => host_source_matches(
                url,
                scheme.as_deref(),
                &host,
                port.as_ref(),
                path.as_deref(),
                origin,
                redirects,
            ),
        };
    }
    if expression.eq_ignore_ascii_case("'self'") {
        return self_matches(url, origin, document_url);
    }
    false
}

enum SourceExpr {
    Scheme(String),
    Host {
        scheme: Option<String>,
        host: String,
        port: Option<PortPart>,
        path: Option<String>,
    },
}

enum PortPart {
    Star,
    Number(u16),
}

fn parse_source(expression: &str) -> Option<SourceExpr> {
    if expression.eq_ignore_ascii_case("'self'")
        || expression.starts_with('\'')
        || expression == "*"
    {
        return None;
    }
    if let Some(scheme) = expression.strip_suffix(':')
        && is_scheme(scheme)
        && !scheme.contains('/')
    {
        return Some(SourceExpr::Scheme(scheme.to_ascii_lowercase()));
    }
    let (scheme, rest) = if let Some((scheme, rest)) = expression.split_once("://") {
        if !is_scheme(scheme) {
            return None;
        }
        (Some(scheme.to_ascii_lowercase()), rest)
    } else {
        (None, expression)
    };
    if rest.is_empty() {
        return None;
    }
    let (hostport, path) = match rest.find('/') {
        Some(index) => (&rest[..index], Some(rest[index..].to_owned())),
        None => (rest, None),
    };
    if path
        .as_deref()
        .is_some_and(|path| path.contains(';') || path.contains(','))
    {
        return None;
    }
    let (host, port) = split_host_port(hostport)?;
    if !valid_host_part(host) {
        return None;
    }
    Some(SourceExpr::Host {
        scheme,
        host: host.to_ascii_lowercase(),
        port,
        path,
    })
}

fn host_source_matches(
    url: &Url,
    scheme: Option<&str>,
    host: &str,
    port: Option<&PortPart>,
    path: Option<&str>,
    origin: &Origin,
    redirects: u32,
) -> bool {
    if scheme.is_some_and(|scheme| !scheme_part_matches(scheme, url.scheme())) {
        return false;
    }
    if scheme.is_none() {
        let Some(origin_scheme) = origin_scheme(origin) else {
            return false;
        };
        if !scheme_part_matches(origin_scheme, url.scheme()) {
            return false;
        }
    }
    if !host_part_matches(host, url) {
        return false;
    }
    if !port_part_matches(port, url) {
        return false;
    }
    if redirects == 0
        && let Some(path) = path.filter(|path| !path.is_empty())
        && !path_part_matches(path, url.path())
    {
        return false;
    }
    true
}

// https://w3c.github.io/webappsec-csp/#match-hosts
fn host_part_matches(pattern: &str, url: &Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    if pattern == "*" {
        return true;
    }
    // https://w3c.github.io/webappsec-csp/#match-hosts
    if let Some(remaining) = pattern.strip_prefix('*')
        && remaining.starts_with('.')
    {
        return host.to_ascii_lowercase().ends_with(remaining);
    }
    pattern.eq_ignore_ascii_case(host)
}

// https://w3c.github.io/webappsec-csp/#match-ports
fn port_part_matches(port: Option<&PortPart>, url: &Url) -> bool {
    let input = match port {
        None => None,
        Some(PortPart::Star) => return true,
        Some(PortPart::Number(number)) => Some(*number),
    };
    let url_port = url.port();
    if input == url_port {
        return true;
    }
    let default = default_port(url.scheme());
    if url_port.is_none() {
        return input == default;
    }
    input.is_none() && url_port == default
}

// https://w3c.github.io/webappsec-csp/#match-paths
fn path_part_matches(path_a: &str, path_b: &str) -> bool {
    if path_a.is_empty() {
        return true;
    }
    if path_a == "/" && path_b.is_empty() {
        return true;
    }
    let exact = !path_a.ends_with('/');
    let mut parts_a: Vec<&str> = path_a.split('/').collect();
    let parts_b: Vec<&str> = path_b.split('/').collect();
    if parts_a.len() > parts_b.len() {
        return false;
    }
    if exact && parts_a.len() != parts_b.len() {
        return false;
    }
    if !exact {
        parts_a.pop();
    }
    for (piece_a, piece_b) in parts_a.iter().zip(parts_b.iter()) {
        let decoded_a = percent_encoding::percent_decode_str(piece_a).decode_utf8_lossy();
        let decoded_b = percent_encoding::percent_decode_str(piece_b).decode_utf8_lossy();
        if decoded_a != decoded_b {
            return false;
        }
    }
    true
}

// https://w3c.github.io/webappsec-csp/#match-schemes
fn scheme_part_matches(expression_scheme: &str, url_scheme: &str) -> bool {
    expression_scheme.eq_ignore_ascii_case(url_scheme)
        || (expression_scheme.eq_ignore_ascii_case("http")
            && url_scheme.eq_ignore_ascii_case("https"))
        || (expression_scheme.eq_ignore_ascii_case("ws")
            && matches!(
                url_scheme.to_ascii_lowercase().as_str(),
                "wss" | "http" | "https"
            ))
        || (expression_scheme.eq_ignore_ascii_case("wss")
            && url_scheme.eq_ignore_ascii_case("https"))
}

fn self_matches(url: &Url, origin: &Origin, document_url: &Url) -> bool {
    if url.scheme() == "blob" {
        return false;
    }
    if same_origin(url, origin, document_url) {
        return true;
    }
    let Origin::Tuple(origin_scheme, origin_host, origin_port) = origin else {
        return false;
    };
    if !hosts_equal(origin_host, url) {
        return false;
    }
    if !self_ports(origin_scheme, *origin_port, url) {
        return false;
    }
    url.scheme() == "https"
        || url.scheme() == "wss"
        || (origin_scheme == "http" && (url.scheme() == "http" || url.scheme() == "ws"))
}

fn same_origin(url: &Url, origin: &Origin, document_url: &Url) -> bool {
    match origin {
        Origin::Tuple(..) => url.origin() == *origin,
        Origin::Opaque(_) => {
            let mut left = url.clone();
            left.set_fragment(None);
            let mut right = document_url.clone();
            right.set_fragment(None);
            left == right
        }
    }
}

fn self_ports(origin_scheme: &str, origin_port: u16, url: &Url) -> bool {
    let url_port = url.port_or_known_default();
    let same = url_port == Some(origin_port);
    let origin_default = default_port(origin_scheme) == Some(origin_port);
    let url_default = url.port().is_none();
    same || (origin_default && url_default)
}

fn hosts_equal(origin_host: &Host<String>, url: &Url) -> bool {
    match (origin_host, url.host()) {
        (Host::Domain(left), Some(Host::Domain(right))) => left.eq_ignore_ascii_case(right),
        (Host::Ipv4(left), Some(Host::Ipv4(right))) => left == &right,
        (Host::Ipv6(left), Some(Host::Ipv6(right))) => left == &right,
        _ => false,
    }
}

fn origin_scheme(origin: &Origin) -> Option<&str> {
    match origin {
        Origin::Tuple(scheme, _, _) => Some(scheme.as_str()),
        Origin::Opaque(_) => None,
    }
}

fn default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "http" | "ws" => Some(80),
        "https" | "wss" => Some(443),
        "ftp" => Some(21),
        _ => None,
    }
}

// https://w3c.github.io/webappsec-csp/#grammardef-host-part
fn split_host_port(hostport: &str) -> Option<(&str, Option<PortPart>)> {
    if hostport.starts_with('[') {
        let end = hostport.find(']')?;
        let host = &hostport[..=end];
        let after = &hostport[end + 1..];
        if after.is_empty() {
            return Some((host, None));
        }
        let port = after.strip_prefix(':')?;
        return Some((host, Some(parse_port_part(port)?)));
    }
    let Some((host, port)) = hostport.rsplit_once(':') else {
        return Some((hostport, None));
    };
    if port.is_empty() {
        return None;
    }
    Some((host, Some(parse_port_part(port)?)))
}

fn parse_port_part(port: &str) -> Option<PortPart> {
    if port == "*" {
        return Some(PortPart::Star);
    }
    if !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit()) {
        let number = port.parse::<u16>().ok()?;
        return Some(PortPart::Number(number));
    }
    None
}

fn valid_host_part(host: &str) -> bool {
    if host == "*" {
        return true;
    }
    let rest = host.strip_prefix("*.").unwrap_or(host);
    if rest.is_empty() {
        return false;
    }
    let body = rest.strip_suffix('.').unwrap_or(rest);
    if body.is_empty() {
        return false;
    }
    body.split('.').all(|label| {
        !label.is_empty()
            && label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    })
}

fn is_scheme(value: &str) -> bool {
    let mut chars = value.chars();
    matches!(chars.next(), Some(character) if character.is_ascii_alphabetic())
        && chars.all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '+' | '-' | '.')
        })
}

fn split_commas(header: &str) -> impl Iterator<Item = &str> {
    header
        .split(',')
        .map(trim_ascii)
        .filter(|token| !token.is_empty())
}

fn split_ascii_whitespace(value: &str) -> impl Iterator<Item = &str> {
    value
        .split(|character: char| ['\t', '\n', '\u{000c}', '\r', ' '].contains(&character))
        .filter(|part| !part.is_empty())
}

fn trim_ascii(value: &str) -> &str {
    value.trim_matches(|character: char| ['\t', '\n', '\u{000c}', '\r', ' '].contains(&character))
}

fn strip_prefix_ignore_ascii_case<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    if value.len() < prefix.len() {
        return None;
    }
    let (head, tail) = value.split_at(prefix.len());
    head.eq_ignore_ascii_case(prefix).then_some(tail)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(spec: &str) -> Url {
        Url::parse(spec).expect("test URL")
    }

    fn inline(source: &str, parser_inserted: bool) -> InlineScript<'_> {
        InlineScript {
            source,
            nonce: "",
            nonceable: false,
            parser_inserted,
            kind: InlineKind::Script,
        }
    }

    #[test]
    fn directive_names_lowercase_and_first_occurrence_wins() {
        let list = List::from_headers(Some("script-SRC 'self'; SCRIPT-SRC 'none'"), None);
        let origin = url("https://example.test/").origin();
        let document_url = url("https://example.test/page");
        assert!(
            list.check_request(
                &origin,
                &document_url,
                &url("https://example.test/a.js"),
                Destination::Script,
                None,
            )
            .is_empty()
        );
    }

    #[test]
    fn non_ascii_directives_are_dropped() {
        let list = List::from_headers(Some("script-src \u{e9}"), None);
        let origin = url("https://example.test/").origin();
        let document_url = url("https://example.test/page");
        assert!(
            list.check_request(
                &origin,
                &document_url,
                &url("https://other.test/a.js"),
                Destination::Script,
                None,
            )
            .is_empty()
        );
    }

    #[test]
    fn wildcard_host_matches_subdomains_only() {
        assert!(host_part_matches(
            "*.example.test",
            &url("https://a.example.test/")
        ));
        assert!(!host_part_matches(
            "*.example.test",
            &url("https://example.test/")
        ));
    }

    #[test]
    fn ports_compare_with_scheme_defaults() {
        assert!(port_part_matches(
            Some(&PortPart::Star),
            &url("https://example.test:8443/")
        ));
        assert!(port_part_matches(
            Some(&PortPart::Number(443)),
            &url("https://example.test/")
        ));
        assert!(!port_part_matches(
            Some(&PortPart::Number(443)),
            &url("http://example.test/")
        ));
    }

    #[test]
    fn paths_match_exactly_or_by_directory() {
        assert!(path_part_matches("/a/b", "/a/b"));
        assert!(!path_part_matches("/a/b", "/a/c"));
        assert!(path_part_matches("/a/", "/a/b"));
        assert!(!path_part_matches("/a", "/a/b"));
        assert!(!path_part_matches("/a/b/", "/a/b"));
    }

    #[test]
    fn split_host_port_table() {
        let cases = [
            ("example.test", Some(("example.test", None))),
            ("example.test:443", Some(("example.test", Some(443u16)))),
            ("example.test:", None),
            ("example.test:abc", None),
            ("example.test:99999", None),
            ("[::1]", Some(("[::1]", None))),
            ("[::1]:443", Some(("[::1]", Some(443u16)))),
            ("[2001:db8::1]:8443", Some(("[2001:db8::1]", Some(8443u16)))),
            ("[::1]:", None),
            ("[::1]:abc", None),
            ("[::1]:99999", None),
            ("[::1", None),
        ];
        for (input, expect) in cases {
            let parsed = split_host_port(input);
            match expect {
                None => assert!(parsed.is_none(), "{input}"),
                Some((host, None)) => {
                    let (got, port) = parsed.expect(input);
                    assert_eq!(got, host, "{input}");
                    assert!(port.is_none(), "{input}");
                }
                Some((host, Some(number))) => {
                    let (got, port) = parsed.expect(input);
                    assert_eq!(got, host, "{input}");
                    assert!(
                        matches!(port, Some(PortPart::Number(value)) if value == number),
                        "{input}"
                    );
                }
            }
        }
        let (host, port) = split_host_port("example.test:*").expect("star");
        assert_eq!(host, "example.test");
        assert!(matches!(port, Some(PortPart::Star)));
        let (host, port) = split_host_port("[::1]:*").expect("ipv6 star");
        assert_eq!(host, "[::1]");
        assert!(matches!(port, Some(PortPart::Star)));
    }

    #[test]
    fn percent_encoded_paths_decode_before_comparing() {
        assert!(path_part_matches("/%61/b", "/a/b"));
        assert!(path_part_matches("/a%2Fb", "/a%2Fb"));
    }

    #[test]
    fn self_matches_secure_variants() {
        let origin = url("http://example.test/").origin();
        let document_url = url("http://example.test/page");
        assert!(self_matches(
            &url("http://example.test/a"),
            &origin,
            &document_url
        ));
        assert!(self_matches(
            &url("https://example.test/a"),
            &origin,
            &document_url
        ));
        assert!(self_matches(
            &url("ws://example.test/a"),
            &origin,
            &document_url
        ));
        assert!(!self_matches(
            &url("http://other.test/a"),
            &origin,
            &document_url
        ));
    }

    #[test]
    fn base64_value_accepts_both_alphabets_and_padding() {
        assert!(is_base64_value("abc-123_"));
        assert!(is_base64_value("YWJj=="));
        assert!(!is_base64_value("a=b"));
        assert!(!is_base64_value(""));
    }

    #[test]
    fn base_uri_blocks_only_a_policy_that_names_it() {
        let origin = Url::parse("http://a.test/page").expect("page URL").origin();
        let document = Url::parse("http://a.test/page").expect("page URL");
        let other = Url::parse("http://b.test/").expect("base URL");
        let list = List::from_headers(Some("base-uri http://b.test/"), None);
        assert!(
            list.check_base(&origin, &document, &other).is_empty(),
            "a matching base is allowed"
        );
        let own = Url::parse("http://a.test/").expect("base URL");
        let blocked = list.check_base(&origin, &document, &own);
        assert_eq!(blocked.len(), 1);
        assert_eq!(blocked[0].effective_directive, "base-uri");
        assert_eq!(blocked[0].blocked_uri, "http://a.test/");
        assert!(blocked[0].blocks());
        let silent = List::from_headers(Some("default-src 'none'"), None);
        assert!(silent.check_base(&origin, &document, &own).is_empty());
        let report = List::from_headers(Some("default-src 'none'"), Some("base-uri 'none'"));
        let reported = report.check_base(&origin, &document, &own);
        assert_eq!(reported.len(), 1);
        assert!(!reported[0].blocks());
    }

    #[test]
    fn evaluation_needs_unsafe_eval_and_not_a_matching_hash() {
        let hash_only = vec!["'sha256-wmuLCpoj8EMqfQlPnt5NIMgKkCK62CxAkAiewI0zZps='".to_owned()];
        assert!(evaluation_blocks(&hash_only));
        let unsafe_eval = vec!["'unsafe-eval'".to_owned()];
        assert!(!evaluation_blocks(&unsafe_eval));
        let wasm = vec!["'wasm-unsafe-eval'".to_owned()];
        assert!(
            evaluation_blocks(&wasm),
            "WebAssembly compilation is not string compilation"
        );
        let nested = vec!["'unsafe-inline'".to_owned(), "'unsafe-eval'".to_owned()];
        assert!(!evaluation_blocks(&nested));
    }

    #[test]
    fn hash_sources_match_their_digest() {
        let expression = "'sha256-wmuLCpoj8EMqfQlPnt5NIMgKkCK62CxAkAiewI0zZps='";
        assert!(hash_matches(expression, "t1.done();"));
        assert!(!hash_matches(expression, "t2.done();"));
    }

    #[test]
    fn report_urls_strip() {
        assert_eq!(
            strip_url_for_reports("http://user:pass@example.test/a?b=1#frag"),
            "http://example.test/a?b=1"
        );
        assert_eq!(
            strip_url_for_reports("data:text/javascript,alert(1)"),
            "data"
        );
        assert_eq!(
            strip_url_for_reports("blob:https://example.test/uuid"),
            "blob"
        );
        assert_eq!(strip_url_for_reports("not a url ["), "");
    }

    #[test]
    fn integrity_metadata_must_subset_policy_hashes() {
        let list = vec![
            "'sha256-L7/UQ9VWpyG7C9RDEC4ctS5hI3Zcw+ta+haPGlByG9c='".to_owned(),
            "'sha512-rYCVMxWV5nq8IsMo+UZNObWtEiWGok/vDN8BMoEQi41s0znSes6E1Q2aag3Lw3u2J1w2rqH7uF2ws6FpQhfSOA=='".to_owned(),
        ];
        assert!(integrity_matches(
            &list,
            "sha256-L7/UQ9VWpyG7C9RDEC4ctS5hI3Zcw+ta+haPGlByG9c="
        ));
        assert!(integrity_matches(
            &list,
            "sha256-L7/UQ9VWpyG7C9RDEC4ctS5hI3Zcw+ta+haPGlByG9c= sha512-rYCVMxWV5nq8IsMo+UZNObWtEiWGok/vDN8BMoEQi41s0znSes6E1Q2aag3Lw3u2J1w2rqH7uF2ws6FpQhfSOA=="
        ));
        assert!(!integrity_matches(
            &list,
            "sha256-L7/UQ9VWpyG7C9RDEC4ctS5hI3Zcw+ta+haPGlByG9c= sha256-xyz"
        ));
        assert!(integrity_matches(
            &list,
            "sha256-L7/UQ9VWpyG7C9RDEC4ctS5hI3Zcw+ta+haPGlByG9c= sha999-xyz"
        ));
        assert!(!integrity_matches(&list, "sha256-xyz"));
        assert!(!integrity_matches(&list, ""));
        assert!(!integrity_matches(&["'self'".to_owned()], "sha256-xyz"));
    }

    #[test]
    fn integrity_verifies_strongest_algorithm() {
        let body = b"window.postMessage(document.currentScript.id, \"*\");\n";
        let sha256 = "sha256-L7/UQ9VWpyG7C9RDEC4ctS5hI3Zcw+ta+haPGlByG9c=";
        assert!(integrity_verifies(sha256, body));
        assert!(integrity_verifies("", body));
        assert!(!integrity_verifies("sha256-xyz", body));
        // https://www.w3.org/TR/SRI/#does-response-match-metadatalist
        assert!(integrity_verifies("sha999-xyz", body));
        assert!(integrity_verifies(
            "sha256-L7/UQ9VWpyG7C9RDEC4ctS5hI3Zcw+ta+haPGlByG9c=?opt",
            body
        ));
        assert!(integrity_verifies(
            "sha256-L7/UQ9VWpyG7C9RDEC4ctS5hI3Zcw+ta+haPGlByG9c= sha256-xyz",
            body
        ));
    }

    #[test]
    fn strict_dynamic_allows_script_insertion_and_blocks_parser_insertion() {
        let list = vec!["'strict-dynamic'".to_owned()];
        assert!(element_matches(&list, &inline("run();", false)));
        assert!(!element_matches(&list, &inline("run();", true)));
    }

    #[test]
    fn unsafe_hashes_gates_hashes_for_non_script_inline() {
        let hash = "'sha256-wmuLCpoj8EMqfQlPnt5NIMgKkCK62CxAkAiewI0zZps='";
        let attribute = InlineScript {
            source: "t1.done();",
            nonce: "",
            nonceable: false,
            parser_inserted: false,
            kind: InlineKind::ScriptAttribute,
        };
        assert!(!element_matches(&[hash.to_owned()], &attribute));
        assert!(element_matches(
            &["'unsafe-hashes'".to_owned(), hash.to_owned()],
            &attribute
        ));
    }
}
