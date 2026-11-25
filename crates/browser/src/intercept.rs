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

#[derive(Clone, Debug)]
pub struct InterceptRule {
    pub id: InterceptId,
    pub phases: Vec<InterceptPhase>,
    pub contexts: Option<Vec<TabId>>,
    pub patterns: Vec<UrlPattern>,
}

impl InterceptRule {
    fn matches(&self, phase: InterceptPhase, tab: Option<TabId>, url: &Url) -> bool {
        if !self.phases.contains(&phase) {
            return false;
        }
        if let Some(contexts) = &self.contexts {
            let Some(tab) = tab else {
                return false;
            };
            if !contexts.contains(&tab) {
                return false;
            }
        }
        if self.patterns.is_empty() {
            return true;
        }
        self.patterns.iter().any(|pattern| pattern.matches(url))
    }
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
    intercepts: Vec<InterceptId>,
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

#[derive(Default)]
struct BrokerState {
    intercepts: Vec<InterceptRule>,
    pending: HashMap<NetworkRequestId, PendingIntercept>,
    blocked_urls: HashMap<TabId, Vec<String>>,
    bodies: VecDeque<(NetworkRequestId, Vec<u8>)>,
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
        phases: Vec<InterceptPhase>,
        contexts: Option<Vec<TabId>>,
        patterns: Vec<UrlPattern>,
    ) -> InterceptId {
        let id = InterceptId(self.next_intercept.fetch_add(1, Ordering::Relaxed));
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .intercepts
            .push(InterceptRule {
                id,
                phases,
                contexts,
                patterns,
            });
        id
    }

    pub fn remove_intercept(&self, id: InterceptId) -> bool {
        let (removed, unblocked) = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            let before = state.intercepts.len();
            state.intercepts.retain(|rule| rule.id != id);
            let removed = state.intercepts.len() < before;
            let mut unblocked = Vec::new();
            let mut kept = HashMap::new();
            for (req_id, mut pending) in state.pending.drain() {
                pending.intercepts.retain(|item| *item != id);
                if pending.intercepts.is_empty() {
                    unblocked.push(pending);
                } else {
                    kept.insert(req_id, pending);
                }
            }
            state.pending = kept;
            (removed, unblocked)
        };
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
        removed
    }

    pub fn set_blocked_urls(&self, tab: TabId, patterns: Vec<String>) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if patterns.is_empty() {
            state.blocked_urls.remove(&tab);
        } else {
            state.blocked_urls.insert(tab, patterns);
        }
    }

    #[must_use]
    pub fn is_blocked_url(&self, tab: Option<TabId>, url: &str) -> bool {
        let Some(tab) = tab else {
            return false;
        };
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.blocked_urls.get(&tab).is_some_and(|patterns| {
            patterns
                .iter()
                .any(|pattern| wildcard_matches(pattern, url))
        })
    }

    pub fn resolve_intercept(
        &self,
        request_id: NetworkRequestId,
        decision: InterceptDecision,
    ) -> Result<(), InterceptResolveError> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(pending) = state.pending.get(&request_id) else {
            return Err(InterceptResolveError::NotFound);
        };
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
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state
            .bodies
            .iter()
            .find(|(id, _)| *id == request_id)
            .map(|(_, bytes)| bytes.clone())
    }

    pub fn record_body(&self, request_id: NetworkRequestId, body: &[u8]) {
        if self.events.receiver_count() == 0 || body.len() > MAX_CACHED_BODY_BYTES {
            return;
        }
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.bodies.len() >= MAX_CACHED_BODIES {
            state.bodies.pop_front();
        }
        state.bodies.push_back((request_id, body.to_vec()));
    }

    pub fn clear_tab(&self, tab: TabId) {
        let removed: Vec<PendingIntercept> = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.blocked_urls.remove(&tab);
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

    #[expect(
        clippy::too_many_arguments,
        reason = "network lifecycle hook carries the structured request metadata"
    )]
    pub(crate) async fn before_request_sent(
        &self,
        request_id: NetworkRequestId,
        tab: Option<TabId>,
        frame: FrameId,
        is_navigation: bool,
        url: &Url,
        method: &str,
        headers: Vec<(String, String)>,
        resource_type: &str,
    ) -> BeforeRequestAction {
        let (matching, rx) = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            let matching: Vec<InterceptId> = state
                .intercepts
                .iter()
                .filter(|rule| rule.matches(InterceptPhase::BeforeRequestSent, tab, url))
                .map(|rule| rule.id)
                .collect();
            let rx = if matching.is_empty() {
                None
            } else {
                let (tx, rx) = oneshot::channel();
                state.pending.insert(
                    request_id,
                    PendingIntercept {
                        tab,
                        phase: InterceptPhase::BeforeRequestSent,
                        intercepts: matching.clone(),
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
            request_id,
        });
        let _ = self.events.send(NetworkEvent::BeforeRequestSent {
            request_id,
            tab,
            frame,
            is_navigation,
            url: url.to_string(),
            method: method.to_owned(),
            headers,
            resource_type: resource_type.to_owned(),
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

    #[expect(
        clippy::too_many_arguments,
        reason = "network lifecycle hook carries the structured response metadata"
    )]
    pub(crate) async fn response_started(
        &self,
        request_id: NetworkRequestId,
        tab: Option<TabId>,
        frame: FrameId,
        is_navigation: bool,
        url: &Url,
        status: u16,
        status_text: &str,
        headers: Vec<(String, String)>,
        mime_type: &str,
        from_cache: bool,
    ) -> ResponseAction {
        let (matching, rx) = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            let matching: Vec<InterceptId> = state
                .intercepts
                .iter()
                .filter(|rule| rule.matches(InterceptPhase::ResponseStarted, tab, url))
                .map(|rule| rule.id)
                .collect();
            let rx = if matching.is_empty() {
                None
            } else {
                let (tx, rx) = oneshot::channel();
                state.pending.insert(
                    request_id,
                    PendingIntercept {
                        tab,
                        phase: InterceptPhase::ResponseStarted,
                        intercepts: matching.clone(),
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
            request_id,
        });
        let _ = self.events.send(NetworkEvent::ResponseStarted {
            request_id,
            tab,
            frame,
            is_navigation,
            url: url.to_string(),
            status,
            status_text: status_text.to_owned(),
            headers,
            mime_type: mime_type.to_owned(),
            from_cache,
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

    #[expect(
        clippy::too_many_arguments,
        reason = "network lifecycle hook carries the structured response completion metadata"
    )]
    pub(crate) fn response_completed(
        &self,
        request_id: NetworkRequestId,
        tab: Option<TabId>,
        frame: FrameId,
        is_navigation: bool,
        url: &Url,
        status: u16,
        status_text: &str,
        headers: Vec<(String, String)>,
        mime_type: &str,
        from_cache: bool,
        encoded_data_length: usize,
    ) {
        let _ = self.events.send(NetworkEvent::ResponseCompleted {
            request_id,
            tab,
            frame,
            is_navigation,
            url: url.to_string(),
            status,
            status_text: status_text.to_owned(),
            headers,
            mime_type: mime_type.to_owned(),
            from_cache,
            encoded_data_length,
            timestamp_ms: now_millis(),
        });
    }

    pub(crate) fn fetch_error(
        &self,
        request_id: NetworkRequestId,
        tab: Option<TabId>,
        frame: FrameId,
        is_navigation: bool,
        url: &Url,
        error_text: &str,
    ) {
        let _ = self.events.send(NetworkEvent::FetchError {
            request_id,
            tab,
            frame,
            is_navigation,
            url: url.to_string(),
            error_text: error_text.to_owned(),
            timestamp_ms: now_millis(),
        });
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InterceptResolveError {
    NotFound,
    WrongPhase,
}

#[must_use]
pub fn now_millis() -> u64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    u64::try_from(millis).unwrap_or(u64::MAX)
}
