// https://w3c.github.io/webdriver-bidi/#module-network
// https://chromedevtools.github.io/devtools-protocol/tot/Network/
// https://chromedevtools.github.io/devtools-protocol/tot/Fetch/

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use renderer::FrameId;
use tokio::sync::{broadcast, oneshot};
use url::Url;

use crate::actor::TabId;
use crate::automation::AutomationSessionId;
use crate::context::UserContextId;

const NETWORK_EVENT_CAPACITY: usize = 1024;
const MAX_CACHED_BODIES: usize = 64;
const MAX_CACHED_BODY_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct NetworkRequestId(u64);

impl NetworkRequestId {
    #[must_use]
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for NetworkRequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct InterceptId(u64);

impl InterceptId {
    #[must_use]
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for InterceptId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "intercept-{}", self.0)
    }
}

// https://w3c.github.io/webdriver-bidi/#type-network-InterceptPhase
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InterceptPhase {
    BeforeRequestSent,
    ResponseStarted,
}

// https://w3c.github.io/webdriver-bidi/#type-network-UrlPattern
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UrlPattern {
    String(String),
    Pattern {
        protocol: Option<String>,
        hostname: Option<String>,
        port: Option<String>,
        pathname: Option<String>,
        search: Option<String>,
    },
    Wildcard(String),
}

impl UrlPattern {
    // https://w3c.github.io/webdriver-bidi/#match-url-pattern
    #[must_use]
    pub fn matches(&self, url: &Url) -> bool {
        match self {
            Self::String(expected) => url_matches_string_pattern(expected, url),
            Self::Pattern {
                protocol,
                hostname,
                port,
                pathname,
                search,
            } => {
                if let Some(proto) = protocol {
                    let expected = proto.trim_end_matches(':');
                    if !url.scheme().eq_ignore_ascii_case(expected) {
                        return false;
                    }
                }
                if let Some(host) = hostname {
                    let actual = url.host_str().unwrap_or_default();
                    if !actual.eq_ignore_ascii_case(host) {
                        return false;
                    }
                }
                if let Some(port_pattern) = port {
                    let actual = url
                        .port_or_known_default()
                        .map_or_default(|p| p.to_string());
                    if port_pattern.is_empty() {
                        if url.port().is_some() {
                            return false;
                        }
                    } else if actual != *port_pattern {
                        return false;
                    }
                }
                if let Some(path_pattern) = pathname {
                    let matched = if path_pattern.starts_with('/') {
                        url.path() == path_pattern
                    } else {
                        url.path().trim_start_matches('/') == path_pattern
                    };
                    if !matched {
                        return false;
                    }
                }
                if let Some(search_pattern) = search {
                    let expected = search_pattern.strip_prefix('?').unwrap_or(search_pattern);
                    let actual = url.query().unwrap_or_default();
                    if actual != expected {
                        return false;
                    }
                }
                true
            }
            Self::Wildcard(pattern) => wildcard_matches(pattern, url.as_str()),
        }
    }
}

fn url_matches_string_pattern(pattern: &str, url: &Url) -> bool {
    let mut normalized = url.clone();
    normalized.set_fragment(None);
    if let Ok(parsed) = Url::parse(pattern) {
        let mut expected = parsed;
        expected.set_fragment(None);
        return normalized == expected;
    }
    wildcard_matches(pattern, normalized.as_str())
}

#[must_use]
pub fn wildcard_matches(pattern: &str, text: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if pattern.is_empty() {
        return text.is_empty();
    }
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star_p, mut star_t) = (None, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star_p = Some(pi);
            star_t = ti;
            pi += 1;
        } else if let Some(sp) = star_p {
            pi = sp + 1;
            star_t += 1;
            ti = star_t;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InterceptScope {
    Global,
    Tabs(Vec<TabId>),
    UserContexts(Vec<UserContextId>),
}

#[derive(Clone, Debug)]
pub struct InterceptRule {
    pub id: InterceptId,
    pub owner: AutomationSessionId,
    pub phases: Vec<InterceptPhase>,
    pub scope: InterceptScope,
    pub patterns: Vec<UrlPattern>,
}

impl InterceptRule {
    fn matches(
        &self,
        phase: InterceptPhase,
        tab: Option<TabId>,
        user_context: &UserContextId,
        url: &Url,
    ) -> bool {
        if !self.phases.contains(&phase) {
            return false;
        }
        match &self.scope {
            InterceptScope::Global => {}
            InterceptScope::Tabs(tabs) => {
                let Some(tab) = tab else {
                    return false;
                };
                if !tabs.contains(&tab) {
                    return false;
                }
            }
            InterceptScope::UserContexts(user_contexts) => {
                if !user_contexts.contains(user_context) {
                    return false;
                }
            }
        }
        if self.patterns.is_empty() {
            return true;
        }
        self.patterns.iter().any(|pattern| pattern.matches(url))
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RequestMeta {
    pub(crate) request_id: NetworkRequestId,
    pub(crate) tab: Option<TabId>,
    pub(crate) user_context: UserContextId,
    pub(crate) frame: FrameId,
    pub(crate) is_navigation: bool,
    pub(crate) url: Url,
    pub(crate) method: String,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) resource_type: String,
}

#[derive(Clone, Debug)]
pub(crate) struct ResponseMeta {
    pub(crate) url: Url,
    pub(crate) status: u16,
    pub(crate) status_text: String,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) mime_type: String,
    pub(crate) from_cache: bool,
}

#[derive(Clone, Debug)]
pub enum NetworkEvent {
    BeforeRequestSent {
        request_id: NetworkRequestId,
        tab: Option<TabId>,
        frame: FrameId,
        is_navigation: bool,
        url: String,
        method: String,
        headers: Vec<(String, String)>,
        resource_type: String,
        timestamp_ms: u64,
        is_blocked: bool,
        intercepts: Vec<InterceptId>,
    },
    ResponseStarted {
        request_id: NetworkRequestId,
        tab: Option<TabId>,
        frame: FrameId,
        is_navigation: bool,
        url: String,
        method: String,
        request_headers: Vec<(String, String)>,
        resource_type: String,
        status: u16,
        status_text: String,
        headers: Vec<(String, String)>,
        mime_type: String,
        from_cache: bool,
        timestamp_ms: u64,
        is_blocked: bool,
        intercepts: Vec<InterceptId>,
    },
    ResponseCompleted {
        request_id: NetworkRequestId,
        tab: Option<TabId>,
        frame: FrameId,
        is_navigation: bool,
        url: String,
        method: String,
        request_headers: Vec<(String, String)>,
        resource_type: String,
        status: u16,
        status_text: String,
        headers: Vec<(String, String)>,
        mime_type: String,
        from_cache: bool,
        encoded_data_length: usize,
        timestamp_ms: u64,
    },
    FetchError {
        request_id: NetworkRequestId,
        tab: Option<TabId>,
        frame: FrameId,
        is_navigation: bool,
        url: String,
        method: String,
        request_headers: Vec<(String, String)>,
        resource_type: String,
        error_text: String,
        timestamp_ms: u64,
    },
}

#[derive(Clone, Debug)]
pub enum InterceptDecision {
    ContinueRequest {
        url: Option<String>,
        method: Option<String>,
        headers: Option<Vec<(String, String)>>,
        body: Option<Vec<u8>>,
    },
    ContinueResponse {
        status: Option<u16>,
        reason_phrase: Option<String>,
        headers: Option<Vec<(String, String)>>,
    },
    ProvideResponse {
        status: u16,
        reason_phrase: String,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    },
    FailRequest,
}

pub(crate) enum BeforeRequestAction {
    Proceed,
    Continue {
        url: Option<String>,
        method: Option<String>,
        headers: Option<Vec<(String, String)>>,
        body: Option<Vec<u8>>,
    },
    ProvideResponse {
        status: u16,
        reason_phrase: String,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    },
    Fail,
}

pub(crate) enum ResponseAction {
    Proceed,
    Modify {
        status: Option<u16>,
        reason_phrase: Option<String>,
        headers: Option<Vec<(String, String)>>,
    },
    ProvideResponse {
        status: u16,
        reason_phrase: String,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    },
    Fail,
}

struct PendingIntercept {
    tab: Option<TabId>,
    phase: InterceptPhase,
    intercepts: Vec<(InterceptId, AutomationSessionId)>,
    responder: oneshot::Sender<InterceptDecision>,
}

struct PendingGuard<'a> {
    broker: &'a NetworkBroker,
    request_id: NetworkRequestId,
}

impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        self.broker
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pending
            .remove(&self.request_id);
    }
}

struct CachedBody {
    request_id: NetworkRequestId,
    tab: Option<TabId>,
    bytes: Vec<u8>,
}

#[derive(Default)]
struct BrokerState {
    intercepts: Vec<InterceptRule>,
    pending: HashMap<NetworkRequestId, PendingIntercept>,
    blocked_urls: HashMap<(AutomationSessionId, TabId), Vec<String>>,
    bodies: VecDeque<CachedBody>,
}

impl BrokerState {
    fn drain_unblocked_where(
        &mut self,
        mut remove_entry: impl FnMut(InterceptId, AutomationSessionId) -> bool,
    ) -> Vec<PendingIntercept> {
        let mut unblocked = Vec::new();
        let mut kept = HashMap::new();
        for (req_id, mut pending) in self.pending.drain() {
            pending
                .intercepts
                .retain(|(id, owner)| !remove_entry(*id, *owner));
            if pending.intercepts.is_empty() {
                unblocked.push(pending);
            } else {
                kept.insert(req_id, pending);
            }
        }
        self.pending = kept;
        unblocked
    }
}

fn continue_unblocked(unblocked: Vec<PendingIntercept>) {
    for pending in unblocked {
        let decision = match pending.phase {
            InterceptPhase::BeforeRequestSent => InterceptDecision::ContinueRequest {
                url: None,
                method: None,
                headers: None,
                body: None,
            },
            InterceptPhase::ResponseStarted => InterceptDecision::ContinueResponse {
                status: None,
                reason_phrase: None,
                headers: None,
            },
        };
        let _ = pending.responder.send(decision);
    }
}

pub struct NetworkBroker {
    next_request: AtomicU64,
    next_intercept: AtomicU64,
    state: Mutex<BrokerState>,
    events: broadcast::Sender<NetworkEvent>,
}

impl Default for NetworkBroker {
    fn default() -> Self {
        Self::new()
    }
}

impl NetworkBroker {
    #[must_use]
    pub fn new() -> Self {
        let (events, _) = broadcast::channel(NETWORK_EVENT_CAPACITY);
        Self {
            next_request: AtomicU64::new(1),
            next_intercept: AtomicU64::new(1),
            state: Mutex::new(BrokerState::default()),
            events,
        }
    }

    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<NetworkEvent> {
        self.events.subscribe()
    }

    #[must_use]
    pub fn next_request_id(&self) -> NetworkRequestId {
        NetworkRequestId(self.next_request.fetch_add(1, Ordering::Relaxed))
    }

    #[must_use]
    pub fn is_active(&self) -> bool {
        if self.events.receiver_count() > 0 {
            return true;
        }
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        !state.intercepts.is_empty() || !state.blocked_urls.is_empty()
    }

    pub fn add_intercept(
        &self,
        owner: AutomationSessionId,
        phases: Vec<InterceptPhase>,
        scope: InterceptScope,
        patterns: Vec<UrlPattern>,
    ) -> InterceptId {
        let id = InterceptId(self.next_intercept.fetch_add(1, Ordering::Relaxed));
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .intercepts
            .push(InterceptRule {
                id,
                owner,
                phases,
                scope,
                patterns,
            });
        id
    }

    pub fn remove_intercept(&self, owner: AutomationSessionId, id: InterceptId) -> bool {
        let (removed, unblocked) = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            let before = state.intercepts.len();
            state
                .intercepts
                .retain(|rule| !(rule.id == id && rule.owner == owner));
            let removed = state.intercepts.len() < before;
            let unblocked = if removed {
                state.drain_unblocked_where(|item_id, item_owner| {
                    item_id == id && item_owner == owner
                })
            } else {
                Vec::new()
            };
            (removed, unblocked)
        };
        continue_unblocked(unblocked);
        removed
    }

    pub fn clear_owner_intercepts(&self, owner: AutomationSessionId) {
        let unblocked = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.intercepts.retain(|rule| rule.owner != owner);
            state.drain_unblocked_where(|_, item_owner| item_owner == owner)
        };
        continue_unblocked(unblocked);
    }

    pub fn clear_owner_tab(&self, owner: AutomationSessionId, tab: TabId) {
        let unblocked = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.blocked_urls.remove(&(owner, tab));
            let mut removed_ids = Vec::new();
            state.intercepts.retain(|rule| {
                let drop_rule = rule.owner == owner
                    && matches!(&rule.scope, InterceptScope::Tabs(tabs) if tabs.len() == 1 && tabs[0] == tab);
                if drop_rule {
                    removed_ids.push(rule.id);
                    false
                } else {
                    true
                }
            });
            if removed_ids.is_empty() {
                Vec::new()
            } else {
                state.drain_unblocked_where(|item_id, item_owner| {
                    item_owner == owner && removed_ids.contains(&item_id)
                })
            }
        };
        continue_unblocked(unblocked);
    }

    pub fn clear_owner(&self, owner: AutomationSessionId) {
        let unblocked = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.intercepts.retain(|rule| rule.owner != owner);
            state
                .blocked_urls
                .retain(|(rule_owner, _), _| *rule_owner != owner);
            state.drain_unblocked_where(|_, item_owner| item_owner == owner)
        };
        continue_unblocked(unblocked);
    }

    pub fn set_blocked_urls(&self, owner: AutomationSessionId, tab: TabId, patterns: Vec<String>) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if patterns.is_empty() {
            state.blocked_urls.remove(&(owner, tab));
        } else {
            state.blocked_urls.insert((owner, tab), patterns);
        }
    }

    #[must_use]
    pub fn is_blocked_url(&self, tab: Option<TabId>, url: &str) -> bool {
        let Some(tab) = tab else {
            return false;
        };
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.blocked_urls.iter().any(|((_, rule_tab), patterns)| {
            *rule_tab == tab
                && patterns
                    .iter()
                    .any(|pattern| wildcard_matches(pattern, url))
        })
    }

    pub fn resolve_intercept(
        &self,
        owner: AutomationSessionId,
        request_id: NetworkRequestId,
        decision: InterceptDecision,
    ) -> Result<(), InterceptResolveError> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(pending) = state.pending.get(&request_id) else {
            return Err(InterceptResolveError::NotFound);
        };
        if !pending
            .intercepts
            .iter()
            .any(|(_, rule_owner)| *rule_owner == owner)
        {
            return Err(InterceptResolveError::NotOwned);
        }
        let phase_ok = match (&pending.phase, &decision) {
            (
                InterceptPhase::BeforeRequestSent,
                InterceptDecision::ContinueRequest { .. }
                | InterceptDecision::ProvideResponse { .. }
                | InterceptDecision::FailRequest,
            )
            | (
                InterceptPhase::ResponseStarted,
                InterceptDecision::ContinueResponse { .. }
                | InterceptDecision::ProvideResponse { .. }
                | InterceptDecision::FailRequest,
            ) => true,
            (InterceptPhase::BeforeRequestSent, InterceptDecision::ContinueResponse { .. })
            | (InterceptPhase::ResponseStarted, InterceptDecision::ContinueRequest { .. }) => false,
        };
        if !phase_ok {
            return Err(InterceptResolveError::WrongPhase);
        }
        if let Some(pending) = state.pending.remove(&request_id) {
            let _ = pending.responder.send(decision);
        }
        Ok(())
    }

    #[must_use]
    pub fn response_body(&self, request_id: NetworkRequestId) -> Option<Vec<u8>> {
        self.response_body_with_tab(request_id)
            .map(|(_, bytes)| bytes)
    }

    #[must_use]
    pub fn response_body_with_tab(
        &self,
        request_id: NetworkRequestId,
    ) -> Option<(Option<TabId>, Vec<u8>)> {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state
            .bodies
            .iter()
            .find(|entry| entry.request_id == request_id)
            .map(|entry| (entry.tab, entry.bytes.clone()))
    }

    pub fn record_body(&self, request_id: NetworkRequestId, tab: Option<TabId>, body: &[u8]) {
        if self.events.receiver_count() == 0 || body.len() > MAX_CACHED_BODY_BYTES {
            return;
        }
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.bodies.len() >= MAX_CACHED_BODIES {
            state.bodies.pop_front();
        }
        state.bodies.push_back(CachedBody {
            request_id,
            tab,
            bytes: body.to_vec(),
        });
    }

    pub fn clear_tab(&self, tab: TabId) {
        let removed: Vec<PendingIntercept> = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            state
                .blocked_urls
                .retain(|(_, rule_tab), _| *rule_tab != tab);
            let mut removed = Vec::new();
            let mut kept = HashMap::new();
            for (key, pending) in state.pending.drain() {
                if pending.tab == Some(tab) {
                    removed.push(pending);
                } else {
                    kept.insert(key, pending);
                }
            }
            state.pending = kept;
            removed
        };
        for pending in removed {
            let _ = pending.responder.send(InterceptDecision::FailRequest);
        }
    }

    pub(crate) fn emit_before_request_unblocked(&self, meta: &RequestMeta) {
        let _ = self.events.send(NetworkEvent::BeforeRequestSent {
            request_id: meta.request_id,
            tab: meta.tab,
            frame: meta.frame,
            is_navigation: meta.is_navigation,
            url: meta.url.to_string(),
            method: meta.method.clone(),
            headers: meta.headers.clone(),
            resource_type: meta.resource_type.clone(),
            timestamp_ms: now_millis(),
            is_blocked: false,
            intercepts: Vec::new(),
        });
    }

    pub(crate) async fn before_request_sent(&self, meta: &RequestMeta) -> BeforeRequestAction {
        let (matching, rx) = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            let matched_rules: Vec<(InterceptId, AutomationSessionId)> = state
                .intercepts
                .iter()
                .filter(|rule| {
                    rule.matches(
                        InterceptPhase::BeforeRequestSent,
                        meta.tab,
                        &meta.user_context,
                        &meta.url,
                    )
                })
                .map(|rule| (rule.id, rule.owner))
                .collect();
            let matching: Vec<InterceptId> = matched_rules.iter().map(|(id, _)| *id).collect();
            let rx = if matched_rules.is_empty() {
                None
            } else {
                let (tx, rx) = oneshot::channel();
                state.pending.insert(
                    meta.request_id,
                    PendingIntercept {
                        tab: meta.tab,
                        phase: InterceptPhase::BeforeRequestSent,
                        intercepts: matched_rules,
                        responder: tx,
                    },
                );
                Some(rx)
            };
            (matching, rx)
        };
        let is_blocked = !matching.is_empty();
        let _guard = rx.as_ref().map(|_| PendingGuard {
            broker: self,
            request_id: meta.request_id,
        });
        let _ = self.events.send(NetworkEvent::BeforeRequestSent {
            request_id: meta.request_id,
            tab: meta.tab,
            frame: meta.frame,
            is_navigation: meta.is_navigation,
            url: meta.url.to_string(),
            method: meta.method.clone(),
            headers: meta.headers.clone(),
            resource_type: meta.resource_type.clone(),
            timestamp_ms: now_millis(),
            is_blocked,
            intercepts: matching,
        });
        let Some(rx) = rx else {
            return BeforeRequestAction::Proceed;
        };
        match rx.await {
            Ok(InterceptDecision::ContinueRequest {
                url,
                method,
                headers,
                body,
            }) => BeforeRequestAction::Continue {
                url,
                method,
                headers,
                body,
            },
            Ok(InterceptDecision::ProvideResponse {
                status,
                reason_phrase,
                headers,
                body,
            }) => BeforeRequestAction::ProvideResponse {
                status,
                reason_phrase,
                headers,
                body,
            },
            Ok(InterceptDecision::FailRequest) => BeforeRequestAction::Fail,
            Ok(InterceptDecision::ContinueResponse { .. }) | Err(_) => BeforeRequestAction::Proceed,
        }
    }

    pub(crate) fn emit_response_started_unblocked(&self, req: &RequestMeta, resp: &ResponseMeta) {
        let _ = self.events.send(NetworkEvent::ResponseStarted {
            request_id: req.request_id,
            tab: req.tab,
            frame: req.frame,
            is_navigation: req.is_navigation,
            url: resp.url.to_string(),
            method: req.method.clone(),
            request_headers: req.headers.clone(),
            resource_type: req.resource_type.clone(),
            status: resp.status,
            status_text: resp.status_text.clone(),
            headers: resp.headers.clone(),
            mime_type: resp.mime_type.clone(),
            from_cache: resp.from_cache,
            timestamp_ms: now_millis(),
            is_blocked: false,
            intercepts: Vec::new(),
        });
    }

    pub(crate) async fn response_started(
        &self,
        req: &RequestMeta,
        resp: &ResponseMeta,
    ) -> ResponseAction {
        let (matching, rx) = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            let matched_rules: Vec<(InterceptId, AutomationSessionId)> = state
                .intercepts
                .iter()
                .filter(|rule| {
                    rule.matches(
                        InterceptPhase::ResponseStarted,
                        req.tab,
                        &req.user_context,
                        &resp.url,
                    )
                })
                .map(|rule| (rule.id, rule.owner))
                .collect();
            let matching: Vec<InterceptId> = matched_rules.iter().map(|(id, _)| *id).collect();
            let rx = if matched_rules.is_empty() {
                None
            } else {
                let (tx, rx) = oneshot::channel();
                state.pending.insert(
                    req.request_id,
                    PendingIntercept {
                        tab: req.tab,
                        phase: InterceptPhase::ResponseStarted,
                        intercepts: matched_rules,
                        responder: tx,
                    },
                );
                Some(rx)
            };
            (matching, rx)
        };
        let is_blocked = !matching.is_empty();
        let _guard = rx.as_ref().map(|_| PendingGuard {
            broker: self,
            request_id: req.request_id,
        });
        let _ = self.events.send(NetworkEvent::ResponseStarted {
            request_id: req.request_id,
            tab: req.tab,
            frame: req.frame,
            is_navigation: req.is_navigation,
            url: resp.url.to_string(),
            method: req.method.clone(),
            request_headers: req.headers.clone(),
            resource_type: req.resource_type.clone(),
            status: resp.status,
            status_text: resp.status_text.clone(),
            headers: resp.headers.clone(),
            mime_type: resp.mime_type.clone(),
            from_cache: resp.from_cache,
            timestamp_ms: now_millis(),
            is_blocked,
            intercepts: matching,
        });
        let Some(rx) = rx else {
            return ResponseAction::Proceed;
        };
        match rx.await {
            Ok(InterceptDecision::ContinueResponse {
                status,
                reason_phrase,
                headers,
            }) => ResponseAction::Modify {
                status,
                reason_phrase,
                headers,
            },
            Ok(InterceptDecision::ProvideResponse {
                status,
                reason_phrase,
                headers,
                body,
            }) => ResponseAction::ProvideResponse {
                status,
                reason_phrase,
                headers,
                body,
            },
            Ok(InterceptDecision::FailRequest) => ResponseAction::Fail,
            Ok(InterceptDecision::ContinueRequest { .. }) | Err(_) => ResponseAction::Proceed,
        }
    }

    pub(crate) fn response_completed(
        &self,
        req: &RequestMeta,
        resp: &ResponseMeta,
        encoded_data_length: usize,
    ) {
        let _ = self.events.send(NetworkEvent::ResponseCompleted {
            request_id: req.request_id,
            tab: req.tab,
            frame: req.frame,
            is_navigation: req.is_navigation,
            url: resp.url.to_string(),
            method: req.method.clone(),
            request_headers: req.headers.clone(),
            resource_type: req.resource_type.clone(),
            status: resp.status,
            status_text: resp.status_text.clone(),
            headers: resp.headers.clone(),
            mime_type: resp.mime_type.clone(),
            from_cache: resp.from_cache,
            encoded_data_length,
            timestamp_ms: now_millis(),
        });
    }

    pub(crate) fn fetch_error(&self, req: &RequestMeta, url: &Url, error_text: &str) {
        let _ = self.events.send(NetworkEvent::FetchError {
            request_id: req.request_id,
            tab: req.tab,
            frame: req.frame,
            is_navigation: req.is_navigation,
            url: url.to_string(),
            method: req.method.clone(),
            request_headers: req.headers.clone(),
            resource_type: req.resource_type.clone(),
            error_text: error_text.to_owned(),
            timestamp_ms: now_millis(),
        });
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InterceptResolveError {
    NotFound,
    NotOwned,
    WrongPhase,
}

#[must_use]
pub fn now_millis() -> u64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    u64::try_from(millis).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(id: u64, tab: TabId, user_context: UserContextId, url: &str) -> RequestMeta {
        RequestMeta {
            request_id: NetworkRequestId::new(id),
            tab: Some(tab),
            user_context,
            frame: FrameId::MAIN,
            is_navigation: false,
            url: Url::parse(url).expect("url"),
            method: "GET".to_owned(),
            headers: Vec::new(),
            resource_type: "Fetch".to_owned(),
        }
    }

    #[tokio::test]
    async fn resolve_intercept_rejects_non_owner_and_user_context_scope_matches_dynamically() {
        let broker = std::sync::Arc::new(NetworkBroker::new());
        let owner_a = AutomationSessionId::new(1);
        let owner_b = AutomationSessionId::new(2);
        let uc_a = UserContextId::parse("user-context-a").expect("uc a");
        let uc_b = UserContextId::parse("user-context-b").expect("uc b");

        broker.add_intercept(
            owner_a,
            vec![InterceptPhase::BeforeRequestSent],
            InterceptScope::UserContexts(vec![uc_a.clone()]),
            vec![UrlPattern::Wildcard("*".to_owned())],
        );

        let unblocked = broker
            .before_request_sent(&meta(1, TabId::new(2), uc_b, "http://example.test/b"))
            .await;
        assert!(matches!(unblocked, BeforeRequestAction::Proceed));

        let broker_task = std::sync::Arc::clone(&broker);
        let request = meta(2, TabId::new(1), uc_a, "http://example.test/a");
        let pending = tokio::spawn(async move { broker_task.before_request_sent(&request).await });

        tokio::task::yield_now().await;
        assert_eq!(
            broker.resolve_intercept(
                owner_b,
                NetworkRequestId::new(2),
                InterceptDecision::FailRequest,
            ),
            Err(InterceptResolveError::NotOwned)
        );
        assert_eq!(
            broker.resolve_intercept(
                owner_a,
                NetworkRequestId::new(2),
                InterceptDecision::FailRequest,
            ),
            Ok(())
        );
        let outcome = pending.await.expect("join");
        assert!(matches!(outcome, BeforeRequestAction::Fail));
    }

    #[tokio::test]
    async fn clear_owner_unblocks_pending_intercepts_and_removes_blocked_urls() {
        let broker = std::sync::Arc::new(NetworkBroker::new());
        let owner = AutomationSessionId::new(7);
        let tab = TabId::new(3);
        let uc = UserContextId::default_context();

        broker.set_blocked_urls(owner, tab, vec!["*blocked*".to_owned()]);
        assert!(broker.is_blocked_url(Some(tab), "http://example.test/blocked"));

        broker.add_intercept(
            owner,
            vec![InterceptPhase::BeforeRequestSent],
            InterceptScope::Tabs(vec![tab]),
            vec![UrlPattern::Wildcard("*".to_owned())],
        );

        let broker_task = std::sync::Arc::clone(&broker);
        let request = meta(10, tab, uc, "http://example.test/ok");
        let pending = tokio::spawn(async move { broker_task.before_request_sent(&request).await });
        tokio::task::yield_now().await;

        broker.clear_owner(owner);
        assert!(!broker.is_blocked_url(Some(tab), "http://example.test/blocked"));
        let outcome = pending.await.expect("join");
        assert!(matches!(outcome, BeforeRequestAction::Continue { .. }));
    }
}
