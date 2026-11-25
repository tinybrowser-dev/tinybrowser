use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use dial::body::{BodyEvent, BodyReader};
use dial::{Admission, BodySink, DialHead, Dialer, HttpCache, StreamInput, StreamStart};
use net::{Agent, AgentOptions, InitiatorKind, Method, Request};
use renderer::{DialFailure, DialKind, DialOutcome, DialRequest, ResponseBody, StreamId};
use tokio::sync::Semaphore;
use tokio::sync::mpsc::UnboundedSender;
use url::Url;

use crate::actor::{NavigationInitiator, TabId};
use crate::intercept::{
    BeforeRequestAction, NetworkBroker, RequestMeta, ResponseAction, ResponseMeta,
};
use crate::wire::RendererAssignmentId;

// https://fetch.spec.whatwg.org/#keepalive-flag
const KEEPALIVE_QUOTA: usize = 65_536;

struct KeepaliveReservation {
    counter: Arc<AtomicUsize>,
    bytes: usize,
}

impl KeepaliveReservation {
    fn acquire(
        counter: &Arc<AtomicUsize>,
        request: &DialRequest,
    ) -> Result<Option<Self>, DialFailure> {
        if !request.keepalive {
            return Ok(None);
        }
        let Ok(length) = usize::try_from(request.body_len) else {
            return Err(DialFailure::Limit);
        };
        if length > KEEPALIVE_QUOTA {
            return Err(DialFailure::Limit);
        }
        let mut current = counter.load(Ordering::Relaxed);
        loop {
            let Some(next) = current.checked_add(length) else {
                return Err(DialFailure::Limit);
            };
            if next > KEEPALIVE_QUOTA {
                return Err(DialFailure::Limit);
            }
            match counter.compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Relaxed)
            {
                Ok(_) => {
                    return Ok(Some(Self {
                        counter: Arc::clone(counter),
                        bytes: length,
                    }));
                }
                Err(observed) => current = observed,
            }
        }
    }
}

impl Drop for KeepaliveReservation {
    fn drop(&mut self) {
        self.counter.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

pub(crate) use dial::dial_failure;

struct DialPermits {
    _global: tokio::sync::OwnedSemaphorePermit,
    _tab: tokio::sync::OwnedSemaphorePermit,
}

struct TabAdmission<'a> {
    global: &'a Arc<Semaphore>,
    tab: &'a Arc<Semaphore>,
}

impl Admission for TabAdmission<'_> {
    type Permit = DialPermits;

    fn acquire(
        &self,
        deadline: Instant,
    ) -> Pin<Box<dyn Future<Output = Result<DialPermits, DialFailure>> + Send + '_>> {
        Box::pin(async move {
            let global = TabNetworkHandle::acquire(self.global, deadline).await?;
            let tab = TabNetworkHandle::acquire(self.tab, deadline).await?;
            Ok(DialPermits {
                _global: global,
                _tab: tab,
            })
        })
    }
}

struct TabBodySink<'a> {
    handle: &'a TabNetworkHandle,
    owner: RendererAssignmentId,
}

impl BodySink<DialPermits> for TabBodySink<'_> {
    fn start_stream(&self, input: StreamInput<'_, DialPermits>) -> StreamStart<DialPermits> {
        StreamStart::Streamed(Box::new(self.handle.start_stream(
            input.request,
            input.sent,
            input.initiator,
            self.owner,
            *input.response,
            input.permit,
        )))
    }
}

#[cfg(test)]
fn stored_bytes(outcome: &DialOutcome) -> usize {
    match &outcome.body {
        ResponseBody::Bytes(bytes) => bytes.len(),
        ResponseBody::Undecodable | ResponseBody::Streamed(_) => 0,
    }
}

enum StreamMessage {
    Chunk(Vec<u8>),
    End,
    Error(DialFailure),
}

const STREAM_CHUNK_DEPTH: usize = 16;

struct StreamEntry {
    owner: RendererAssignmentId,
    rx: Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<StreamMessage>>>,
    task: tokio::task::JoinHandle<()>,
}

type StreamMap = Arc<Mutex<HashMap<StreamId, StreamEntry>>>;

fn abort_stream(entry: Option<StreamEntry>) {
    if let Some(entry) = entry {
        entry.task.abort();
    }
}

pub(crate) enum NavBody {
    Stream(net::Body),
    Bytes(Vec<u8>),
}

pub(crate) struct NavOutcome {
    pub head: renderer::ResponseHead,
    pub body: NavBody,
    pub status: u16,
    pub status_text: String,
    pub final_url: Url,
    pub headers: Vec<(String, String)>,
    pub request_meta: Option<RequestMeta>,
    _permit: Option<tokio::sync::OwnedSemaphorePermit>,
}

pub(crate) struct NetworkContext {
    agent: Agent,
    permits: NetworkPermits,
    cache: Arc<HttpCache>,
    streams: StreamMap,
    next_stream: Arc<AtomicU64>,
    broker: Arc<NetworkBroker>,
}

impl NetworkContext {
    pub(crate) fn new(mut options: AgentOptions, broker: Arc<NetworkBroker>) -> Self {
        if options.timeout_per_call.is_none() {
            options.timeout_per_call = Some(dial::PAGE_FETCH_TIMEOUT);
        }
        let clock = options.clock.clone();
        Self {
            agent: Agent::new(options),
            cache: Arc::new(HttpCache::new(clock)),
            permits: NetworkPermits::new(),
            streams: Arc::new(Mutex::new(HashMap::new())),
            next_stream: Arc::new(AtomicU64::new(1)),
            broker,
        }
    }

    #[must_use]
    pub(crate) fn tab_handle(&self) -> TabNetworkHandle {
        TabNetworkHandle {
            dialer: Dialer::new(self.agent.clone(), Arc::clone(&self.cache)),
            permits: self.permits.clone(),
            tab: Arc::new(Semaphore::new(MAX_TAB_DIALS)),
            keepalive: Arc::new(AtomicUsize::new(0)),
            streams: Arc::clone(&self.streams),
            next_stream: Arc::clone(&self.next_stream),
            broker: Arc::clone(&self.broker),
            client: None,
        }
    }

    pub(crate) fn identity(&self) -> net::ValidIdentity {
        self.agent.identity().clone()
    }

    pub(crate) fn clear_cache(&self) {
        self.cache.clear();
    }

    pub(crate) fn set_cache_disabled(&self, disabled: bool) {
        self.cache.set_disabled(disabled);
    }
}

#[derive(Clone)]
struct NetworkPermits {
    global: Arc<Semaphore>,
    navigation: Arc<Semaphore>,
}

impl NetworkPermits {
    fn new() -> Self {
        Self {
            global: Arc::new(Semaphore::new(MAX_GLOBAL_DIALS)),
            navigation: Arc::new(Semaphore::new(MAX_NAVIGATION_DIALS)),
        }
    }
}

const MAX_GLOBAL_DIALS: usize = 32;
const MAX_NAVIGATION_DIALS: usize = 8;
const MAX_TAB_DIALS: usize = 8;

#[derive(Clone)]
pub(crate) struct TabNetworkHandle {
    dialer: Dialer,
    permits: NetworkPermits,
    tab: Arc<Semaphore>,
    keepalive: Arc<AtomicUsize>,
    streams: StreamMap,
    next_stream: Arc<AtomicU64>,
    broker: Arc<NetworkBroker>,
    client: Option<u64>,
}

impl TabNetworkHandle {
    #[must_use]
    pub(crate) fn with_tab(mut self, tab: crate::TabId) -> Self {
        self.client = Some(tab.get());
        self
    }

    #[must_use]
    pub(crate) fn broker(&self) -> &Arc<NetworkBroker> {
        &self.broker
    }

    fn tab_id(&self) -> Option<TabId> {
        self.client.map(TabId::new)
    }

    pub(crate) fn set_identity(&self, tab: crate::TabId, identity: Option<net::ValidIdentity>) {
        self.dialer.agent().set_client_identity(tab.get(), identity);
    }

    // https://chromedevtools.github.io/devtools-protocol/tot/Network/#method-setExtraHTTPHeaders
    pub(crate) fn set_extra_headers(
        &self,
        tab: crate::TabId,
        headers: &[(String, String)],
    ) -> Result<(), net::HeaderError> {
        self.dialer
            .agent()
            .set_client_extra_headers(tab.get(), headers)
    }

    #[must_use]
    pub(crate) fn identity(&self, tab: crate::TabId) -> net::ValidIdentity {
        self.dialer.agent().identity_for(Some(tab.get()))
    }

    pub(crate) fn dial_navigation(
        &self,
        epoch: u64,
        url: Url,
        initiator: Option<NavigationInitiator>,
        reply: UnboundedSender<(u64, Result<NavOutcome, DialFailure>)>,
        mut cancel: tokio::sync::watch::Receiver<bool>,
    ) -> tokio::task::JoinHandle<()> {
        let network = self.clone();
        tokio::spawn(async move {
            let result = tokio::select! {
                biased;
                _ = cancel.changed() => Err(DialFailure::Cancelled),
                result = network.navigate(&url, initiator.as_ref()) => result,
            };
            let _send_result = reply.send((epoch, result));
        })
    }

    async fn navigate(
        &self,
        url: &Url,
        source: Option<&NavigationInitiator>,
    ) -> Result<NavOutcome, DialFailure> {
        let started = Instant::now();
        let redacted = crate::actor::log_url(url);
        logging::trace!(target: "browser::net", "nav start {redacted}");
        let deadline = started + dial::PAGE_FETCH_TIMEOUT;
        let permits = Self::acquire(&self.permits.navigation, deadline).await?;
        let tab = self.tab_id();
        let mut request_meta = self.broker.is_active().then(|| RequestMeta {
            request_id: self.broker.next_request_id(),
            tab,
            frame: renderer::FrameId::MAIN,
            is_navigation: true,
            url: url.clone(),
            method: Method::GET.as_str().to_owned(),
            headers: Vec::new(),
            resource_type: "Document".to_owned(),
        });
        if self.broker.is_blocked_url(tab, url.as_str()) {
            if let Some(ref meta) = request_meta {
                self.broker.emit_before_request_unblocked(meta);
                self.broker
                    .fetch_error(meta, url, "net::ERR_BLOCKED_BY_CLIENT");
            }
            return Err(DialFailure::Cancelled);
        }
        let prepared = match self
            .intercept_before_nav(request_meta.as_mut(), url)
            .await?
        {
            NavIntercept::Synthetic(outcome) => return Ok(*outcome),
            NavIntercept::Request(prepared) => prepared,
        };
        let target_url = prepared.url.clone();
        let request = self.build_nav_request(prepared, source, deadline);
        let response = match self.dialer.agent().send(request).await {
            Ok(response) => response,
            Err(error) => {
                logging::trace!(
                    target: "browser::net",
                    "nav failed {redacted} in {:?}: {error:?}",
                    started.elapsed()
                );
                let failure = dial_failure(&error);
                if let Some(ref meta) = request_meta {
                    self.broker
                        .fetch_error(meta, &target_url, &format!("{failure:?}"));
                }
                return Err(failure);
            }
        };
        logging::trace!(
            target: "browser::net",
            "nav head {redacted} -> {} {} in {:?}",
            response.status(),
            crate::actor::log_url(response.final_url()),
            started.elapsed()
        );
        self.finish_nav_response(response, request_meta, permits)
            .await
    }

    async fn intercept_before_nav(
        &self,
        request_meta: Option<&mut RequestMeta>,
        url: &Url,
    ) -> Result<NavIntercept, DialFailure> {
        let mut prepared = PreparedNavRequest {
            url: url.clone(),
            method: Method::GET,
            headers: None,
            body: None,
        };
        let Some(meta) = request_meta else {
            return Ok(NavIntercept::Request(prepared));
        };
        match self.broker.before_request_sent(meta).await {
            BeforeRequestAction::Proceed => Ok(NavIntercept::Request(prepared)),
            BeforeRequestAction::Continue {
                url: next_url,
                method: next_method,
                headers,
                body,
            } => {
                if let Some(spec) = next_url
                    && let Ok(parsed) = Url::parse(&spec)
                {
                    prepared.url = parsed.clone();
                    meta.url = parsed;
                }
                if let Some(name) = next_method
                    && let Ok(parsed) = Method::parse(&name)
                {
                    parsed.as_str().clone_into(&mut meta.method);
                    prepared.method = parsed;
                }
                if let Some(ref hdrs) = headers {
                    meta.headers.clone_from(hdrs);
                }
                prepared.headers = headers;
                prepared.body = body;
                Ok(NavIntercept::Request(prepared))
            }
            BeforeRequestAction::ProvideResponse {
                status,
                reason_phrase,
                headers,
                body,
            } => Ok(NavIntercept::Synthetic(Box::new(
                self.synthetic_nav_outcome(
                    meta.clone(),
                    SyntheticResponse {
                        url: prepared.url,
                        status,
                        reason_phrase,
                        headers,
                        body,
                        emit_started: true,
                    },
                ),
            ))),
            BeforeRequestAction::Fail => {
                self.broker
                    .fetch_error(meta, &prepared.url, "net::ERR_FAILED");
                Err(DialFailure::Connect)
            }
        }
    }

    fn build_nav_request(
        &self,
        prepared: PreparedNavRequest,
        source: Option<&NavigationInitiator>,
        deadline: Instant,
    ) -> Request {
        let mut request = Request::new(prepared.method, prepared.url.clone());
        request.client = self.client;
        dial::apply_navigation_headers(&mut request, &prepared.url);
        if let Some(headers) = prepared.headers {
            for (name, value) in headers {
                let _ = request.headers.insert(&name, value);
            }
        }
        if let Some(body) = prepared.body {
            request.body = Some(body);
        }
        request.initiator_kind = InitiatorKind::Navigation;
        // https://w3c.github.io/webappsec-fetch-metadata/#abstract-opdef-set-site
        request.initiator = source.map(|source| source.origin.clone());
        request.cors = net::cors::CorsSettings {
            mode: net::cors::RequestMode::Navigate,
            credentials: net::cors::CredentialsMode::Include,
            redirect: net::cors::RedirectMode::Follow,
            origin: source.map_or_else(opaque_origin, |source| source.origin.clone()),
        };
        request.deadline = Some(deadline);
        if let Some(source) = source {
            // https://w3c.github.io/webappsec-referrer-policy/#determine-requests-referrer
            request.referrer = match &source.referrer {
                Some(referrer) => net::Referrer::Client(net::ReferrerClient {
                    origin: source.origin.clone(),
                    url: referrer.clone(),
                }),
                None => net::Referrer::NoReferrer,
            };
            request.referrer_policy = source.referrer_policy;
        }
        request
    }

    async fn finish_nav_response(
        &self,
        response: net::Response,
        request_meta: Option<RequestMeta>,
        permits: tokio::sync::OwnedSemaphorePermit,
    ) -> Result<NavOutcome, DialFailure> {
        let mut status = response.status();
        // https://html.spec.whatwg.org/multipage/document-lifecycle.html#initialise-the-document-object
        let net_head = DialHead::internal(&response);
        let final_url = net_head.final_url().clone();
        let mut status_text = net_head.status_text().to_owned();
        let mut headers = net_head.headers().to_vec();
        let mut head = net_head.into_document_head();
        let mime_type = mime_from_headers(&headers);
        if let Some(ref meta) = request_meta {
            let resp_meta = ResponseMeta {
                url: final_url.clone(),
                status,
                status_text: status_text.clone(),
                headers: headers.clone(),
                mime_type,
                from_cache: false,
            };
            match self.broker.response_started(meta, &resp_meta).await {
                ResponseAction::Proceed => {}
                ResponseAction::Modify {
                    status: new_status,
                    reason_phrase,
                    headers: new_headers,
                } => {
                    if let Some(code) = new_status {
                        status = code;
                    }
                    if let Some(phrase) = reason_phrase {
                        status_text = phrase;
                    }
                    if let Some(hdrs) = new_headers {
                        headers = hdrs;
                    }
                    let ct = net::combined_header_in(&headers, "content-type");
                    head = renderer::ResponseHead::http(status, final_url.clone(), ct, &headers);
                }
                ResponseAction::ProvideResponse {
                    status,
                    reason_phrase,
                    headers,
                    body,
                } => {
                    return Ok(self.synthetic_nav_outcome(
                        meta.clone(),
                        SyntheticResponse {
                            url: final_url,
                            status,
                            reason_phrase,
                            headers,
                            body,
                            emit_started: false,
                        },
                    ));
                }
                ResponseAction::Fail => {
                    self.broker.fetch_error(meta, &final_url, "net::ERR_FAILED");
                    return Err(DialFailure::Connect);
                }
            }
        }
        let body = response.into_body().await;
        Ok(NavOutcome {
            head,
            body: NavBody::Stream(body),
            status,
            status_text,
            final_url,
            headers,
            request_meta,
            _permit: Some(permits),
        })
    }

    fn synthetic_nav_outcome(&self, meta: RequestMeta, resp: SyntheticResponse) -> NavOutcome {
        let ct = net::combined_header_in(&resp.headers, "content-type");
        let mime = mime_from_headers(&resp.headers);
        let head = renderer::ResponseHead::http(resp.status, resp.url.clone(), ct, &resp.headers);
        let resp_meta = ResponseMeta {
            url: resp.url.clone(),
            status: resp.status,
            status_text: resp.reason_phrase.clone(),
            headers: resp.headers.clone(),
            mime_type: mime,
            from_cache: false,
        };
        if resp.emit_started {
            self.broker
                .emit_response_started_unblocked(&meta, &resp_meta);
        }
        self.broker
            .record_body(meta.request_id, meta.tab, &resp.body);
        self.broker
            .response_completed(&meta, &resp_meta, resp.body.len());
        NavOutcome {
            head,
            body: NavBody::Bytes(resp.body),
            status: resp.status,
            status_text: resp.reason_phrase,
            final_url: resp.url,
            headers: resp.headers,
            request_meta: Some(meta),
            _permit: None,
        }
    }

    pub(crate) async fn dial_request(
        &self,
        request: &DialRequest,
        initiator: &url::Origin,
        owner: RendererAssignmentId,
    ) -> Result<DialOutcome, DialFailure> {
        let started = Instant::now();
        let _keepalive = KeepaliveReservation::acquire(&self.keepalive, request)?;
        logging::trace!(
            target: "browser::net",
            "request start {:?} {}",
            request.policy.kind(),
            crate::actor::log_url(&request.url)
        );
        let tab = self.tab_id();
        let frame = request.frame.unwrap_or(renderer::FrameId::MAIN);
        let is_navigation = request.policy == renderer::RequestPolicy::FrameLoad;
        let mut request_meta = self.broker.is_active().then(|| RequestMeta {
            request_id: self.broker.next_request_id(),
            tab,
            frame,
            is_navigation,
            url: request.url.clone(),
            method: request.method.as_str().to_owned(),
            headers: request.headers.clone(),
            resource_type: resource_type_for(request.policy.kind()).to_owned(),
        });
        if self.broker.is_blocked_url(tab, request.url.as_str()) {
            if let Some(ref meta) = request_meta {
                self.broker.emit_before_request_unblocked(meta);
                self.broker
                    .fetch_error(meta, &request.url, "net::ERR_BLOCKED_BY_CLIENT");
            }
            return Err(DialFailure::Cancelled);
        }
        let modified_request = match self
            .intercept_before_dial(request_meta.as_mut(), request)
            .await?
        {
            DialIntercept::Synthetic(outcome) => return Ok(outcome),
            DialIntercept::Continue(modified) => modified,
        };
        let effective = modified_request.as_ref().unwrap_or(request);
        let admission = TabAdmission {
            global: &self.permits.global,
            tab: &self.tab,
        };
        let sink = TabBodySink {
            handle: self,
            owner,
        };
        let result = self
            .dialer
            .dial(
                effective,
                initiator,
                owner.get(),
                self.client,
                &admission,
                &sink,
            )
            .await;
        let result = match result {
            Ok(outcome) => {
                self.intercept_dial_response(request_meta.as_ref(), owner, outcome)
                    .await
            }
            Err(failure) => {
                if let Some(ref meta) = request_meta {
                    self.broker
                        .fetch_error(meta, &effective.url, &format!("{failure:?}"));
                }
                Err(failure)
            }
        };
        trace_dial_result(effective, started, result.as_ref());
        result
    }

    async fn intercept_before_dial(
        &self,
        request_meta: Option<&mut RequestMeta>,
        request: &DialRequest,
    ) -> Result<DialIntercept, DialFailure> {
        let Some(meta) = request_meta else {
            return Ok(DialIntercept::Continue(None));
        };
        match self.broker.before_request_sent(meta).await {
            BeforeRequestAction::Proceed => Ok(DialIntercept::Continue(None)),
            BeforeRequestAction::Continue {
                url,
                method,
                headers,
                body,
            } => {
                let mut next = request.clone();
                if let Some(spec) = url
                    && let Ok(parsed) = Url::parse(&spec)
                {
                    meta.url = parsed.clone();
                    next.url = parsed;
                }
                if let Some(name) = method
                    && let Ok(parsed) = Method::parse(&name)
                {
                    parsed.as_str().clone_into(&mut meta.method);
                    next.method = parsed;
                }
                if let Some(hdrs) = headers {
                    meta.headers.clone_from(&hdrs);
                    next.headers = hdrs;
                }
                if let Some(bytes) = body {
                    next.body = bytes;
                }
                Ok(DialIntercept::Continue(Some(next)))
            }
            BeforeRequestAction::ProvideResponse {
                status,
                reason_phrase,
                headers,
                body,
            } => Ok(DialIntercept::Synthetic(self.synthetic_dial_outcome(
                meta,
                SyntheticResponse {
                    url: request.url.clone(),
                    status,
                    reason_phrase,
                    headers,
                    body,
                    emit_started: true,
                },
            ))),
            BeforeRequestAction::Fail => {
                self.broker
                    .fetch_error(meta, &request.url, "net::ERR_FAILED");
                Err(DialFailure::Connect)
            }
        }
    }

    async fn intercept_dial_response(
        &self,
        request_meta: Option<&RequestMeta>,
        owner: RendererAssignmentId,
        mut outcome: DialOutcome,
    ) -> Result<DialOutcome, DialFailure> {
        let Some(meta) = request_meta else {
            return Ok(outcome);
        };
        let mime = mime_from_headers(&outcome.headers);
        let resp_meta = ResponseMeta {
            url: outcome.final_url.clone(),
            status: outcome.status,
            status_text: outcome.status_text.clone(),
            headers: outcome.headers.clone(),
            mime_type: mime,
            from_cache: false,
        };
        match self.broker.response_started(meta, &resp_meta).await {
            ResponseAction::Proceed => {
                self.record_dial_completed(meta, &outcome, &resp_meta);
                Ok(outcome)
            }
            ResponseAction::Modify {
                status,
                reason_phrase,
                headers,
            } => {
                if let Some(code) = status {
                    outcome.status = code;
                }
                if let Some(phrase) = reason_phrase {
                    outcome.status_text = phrase;
                }
                if let Some(hdrs) = headers {
                    outcome.content_type = net::combined_header_in(&hdrs, "content-type");
                    outcome.headers = hdrs;
                }
                let updated = ResponseMeta {
                    url: outcome.final_url.clone(),
                    status: outcome.status,
                    status_text: outcome.status_text.clone(),
                    headers: outcome.headers.clone(),
                    mime_type: mime_from_headers(&outcome.headers),
                    from_cache: false,
                };
                self.record_dial_completed(meta, &outcome, &updated);
                Ok(outcome)
            }
            ResponseAction::ProvideResponse {
                status,
                reason_phrase,
                headers,
                body,
            } => {
                if let ResponseBody::Streamed(stream) = outcome.body {
                    self.cancel_stream(owner, stream);
                }
                Ok(self.synthetic_dial_outcome(
                    meta,
                    SyntheticResponse {
                        url: outcome.final_url,
                        status,
                        reason_phrase,
                        headers,
                        body,
                        emit_started: false,
                    },
                ))
            }
            ResponseAction::Fail => {
                if let ResponseBody::Streamed(stream) = outcome.body {
                    self.cancel_stream(owner, stream);
                }
                self.broker
                    .fetch_error(meta, &outcome.final_url, "net::ERR_FAILED");
                Err(DialFailure::Connect)
            }
        }
    }

    fn record_dial_completed(
        &self,
        meta: &RequestMeta,
        outcome: &DialOutcome,
        resp: &ResponseMeta,
    ) {
        let body_len = match &outcome.body {
            ResponseBody::Bytes(bytes) => {
                self.broker.record_body(meta.request_id, meta.tab, bytes);
                bytes.len()
            }
            ResponseBody::Undecodable | ResponseBody::Streamed(_) => 0,
        };
        self.broker.response_completed(meta, resp, body_len);
    }

    fn synthetic_dial_outcome(&self, meta: &RequestMeta, resp: SyntheticResponse) -> DialOutcome {
        let content_type = net::combined_header_in(&resp.headers, "content-type");
        let mime = mime_from_headers(&resp.headers);
        let resp_meta = ResponseMeta {
            url: resp.url.clone(),
            status: resp.status,
            status_text: resp.reason_phrase.clone(),
            headers: resp.headers.clone(),
            mime_type: mime,
            from_cache: false,
        };
        if resp.emit_started {
            self.broker
                .emit_response_started_unblocked(meta, &resp_meta);
        }
        self.broker
            .record_body(meta.request_id, meta.tab, &resp.body);
        self.broker
            .response_completed(meta, &resp_meta, resp.body.len());
        DialOutcome {
            status: resp.status,
            status_text: resp.reason_phrase,
            final_url: resp.url,
            content_type,
            headers: resp.headers,
            body: ResponseBody::Bytes(resp.body),
            response_type: net::cors::ResponseType::Basic,
            crossed_origin: false,
            redirect_count: 0,
            cookies_stored: false,
            referrer: None,
        }
    }

    fn start_stream(
        &self,
        request: &DialRequest,
        sent: &[(String, String)],
        initiator: &url::Origin,
        owner: RendererAssignmentId,
        response: net::Response,
        permits: DialPermits,
    ) -> DialOutcome {
        let cache = self.dialer.cache();
        let head = DialHead::read(request, &response);
        let opaque = head.null_body();
        let mut outcome = head.into_outcome(ResponseBody::Bytes(Vec::new()));
        // https://www.rfc-editor.org/rfc/rfc9111#section-4.4
        cache.invalidate_unsafe(request, owner.get(), &outcome);
        // https://fetch.spec.whatwg.org/#concept-filtered-response-opaque
        if opaque {
            return outcome;
        }
        let (tx, rx) = tokio::sync::mpsc::channel(STREAM_CHUNK_DEPTH);
        let store = cache
            .may_store(request, sent, &outcome)
            .then(|| PendingStore {
                outcome: outcome.clone(),
                cache: Arc::clone(cache),
                request: request.clone(),
                sent: sent.to_vec(),
                initiator: initiator.clone(),
                partition: owner.get(),
            });
        let task = spawn_stream_body(StreamJob {
            response,
            integrity: request.integrity.clone(),
            store,
            permits,
            kind: request.policy.kind(),
            url: crate::actor::log_url(&request.url),
            tx,
        });
        let id = StreamId::mint(self.next_stream.fetch_add(1, Ordering::Relaxed));
        self.streams
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                id,
                StreamEntry {
                    owner,
                    rx: Arc::new(tokio::sync::Mutex::new(rx)),
                    task,
                },
            );
        outcome.body = ResponseBody::Streamed(id);
        outcome
    }

    pub(crate) async fn read_stream(
        &self,
        owner: RendererAssignmentId,
        id: StreamId,
    ) -> Result<Option<Vec<u8>>, DialFailure> {
        let rx = self
            .streams
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&id)
            .filter(|entry| entry.owner == owner)
            .map(|entry| Arc::clone(&entry.rx));
        let Some(rx) = rx else {
            return Ok(None);
        };
        let mut rx = rx.lock().await;
        match rx.recv().await {
            Some(StreamMessage::Chunk(bytes)) => Ok(Some(bytes)),
            Some(StreamMessage::End) | None => {
                self.remove_stream(id);
                Ok(None)
            }
            Some(StreamMessage::Error(failure)) => {
                self.remove_stream(id);
                Err(failure)
            }
        }
    }

    pub(crate) fn cancel_stream(&self, owner: RendererAssignmentId, id: StreamId) {
        use std::collections::hash_map::Entry;
        let mut streams = self
            .streams
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Entry::Occupied(entry) = streams.entry(id)
            && entry.get().owner == owner
        {
            abort_stream(Some(entry.remove()));
        }
    }

    fn remove_stream(&self, id: StreamId) {
        let mut streams = self
            .streams
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        abort_stream(streams.remove(&id));
    }

    pub(crate) fn release_streams(&self, owner: RendererAssignmentId) {
        let mut streams = self
            .streams
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let abandoned: Vec<StreamId> = streams
            .iter()
            .filter(|(_, entry)| entry.owner == owner)
            .map(|(id, _)| *id)
            .collect();
        for id in abandoned {
            abort_stream(streams.remove(&id));
        }
    }

    async fn acquire(
        semaphore: &Arc<Semaphore>,
        deadline: Instant,
    ) -> Result<tokio::sync::OwnedSemaphorePermit, DialFailure> {
        let permit = tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            Arc::clone(semaphore).acquire_owned(),
        )
        .await;
        match permit {
            Ok(Ok(permit)) => Ok(permit),
            Ok(Err(_)) => Err(DialFailure::Cancelled),
            Err(_) => Err(DialFailure::QueueFull),
        }
    }
}

struct PendingStore {
    outcome: DialOutcome,
    cache: Arc<HttpCache>,
    request: DialRequest,
    sent: Vec<(String, String)>,
    initiator: url::Origin,
    partition: u64,
}

struct StreamJob {
    response: net::Response,
    integrity: Option<String>,
    store: Option<PendingStore>,
    permits: DialPermits,
    kind: DialKind,
    url: String,
    tx: tokio::sync::mpsc::Sender<StreamMessage>,
}

fn spawn_stream_body(job: StreamJob) -> tokio::task::JoinHandle<()> {
    let StreamJob {
        response,
        integrity,
        store,
        permits,
        kind,
        url,
        tx,
    } = job;
    tokio::spawn(async move {
        let _permits = permits;
        let collect = store.is_some() || integrity.is_some();
        let mut reader = BodyReader::new(response, integrity, collect).await;
        loop {
            match reader.next().await {
                BodyEvent::Chunk(chunk) => {
                    if tx.send(StreamMessage::Chunk(chunk)).await.is_err() {
                        return;
                    }
                }
                BodyEvent::Done(body) => {
                    if let Some(mut store) = store {
                        store.outcome.body = ResponseBody::Bytes(body);
                        store.cache.remember(
                            &store.request,
                            &store.sent,
                            &store.initiator,
                            store.partition,
                            &store.outcome,
                        );
                    }
                    let _ = tx.send(StreamMessage::End).await;
                    logging::trace!(target: "browser::net", "stream done {kind:?} {url}");
                    return;
                }
                BodyEvent::DecodeError => {
                    let _ = tx.send(StreamMessage::Error(DialFailure::Connect)).await;
                    logging::trace!(target: "browser::net", "stream decode error {kind:?} {url}");
                    return;
                }
                BodyEvent::Failed(failure) => {
                    let _ = tx.send(StreamMessage::Error(failure)).await;
                    logging::trace!(
                        target: "browser::net",
                        "stream failed {kind:?} {url}: {failure:?}"
                    );
                    return;
                }
            }
        }
    })
}

fn trace_dial_result(
    request: &DialRequest,
    started: Instant,
    result: Result<&DialOutcome, &DialFailure>,
) {
    let url = crate::actor::log_url(&request.url);
    match result {
        Ok(outcome) => logging::trace!(
            target: "browser::net",
            "request done {:?} {url} -> {} in {:?}",
            request.policy.kind(),
            outcome.status,
            started.elapsed()
        ),
        Err(failure) => logging::trace!(
            target: "browser::net",
            "request failed {:?} {url} in {:?}: {failure:?}",
            request.policy.kind(),
            started.elapsed()
        ),
    }
}

fn opaque_origin() -> url::Origin {
    net::about_blank().origin()
}

struct PreparedNavRequest {
    url: Url,
    method: Method,
    headers: Option<Vec<(String, String)>>,
    body: Option<Vec<u8>>,
}

enum NavIntercept {
    Request(PreparedNavRequest),
    Synthetic(Box<NavOutcome>),
}

enum DialIntercept {
    Continue(Option<DialRequest>),
    Synthetic(DialOutcome),
}

struct SyntheticResponse {
    url: Url,
    status: u16,
    reason_phrase: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    emit_started: bool,
}

#[must_use]
pub fn mime_from_headers(headers: &[(String, String)]) -> String {
    net::combined_header_in(headers, "content-type").map_or_default(|raw| {
        raw.split(';')
            .next()
            .map_or("", str::trim)
            .to_ascii_lowercase()
    })
}

fn resource_type_for(kind: DialKind) -> &'static str {
    match kind {
        DialKind::FrameLoad => "Document",
        DialKind::ClassicScript | DialKind::ModuleScript => "Script",
        DialKind::Stylesheet => "Stylesheet",
        DialKind::Image => "Image",
        DialKind::Font => "Font",
        DialKind::JsFetch => "Fetch",
        DialKind::CspReport => "Other",
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use dial::dialer::orb_filter;
    use dial::outbound_request;
    use renderer::BodyDelivery;

    use super::*;
    use dial::referrer_authorized;
    use renderer::RequestPolicy;
    use std::time::Duration;

    fn dial(policy: RequestPolicy, url: &str) -> DialRequest {
        let url = Url::parse(url).expect("dial URL");
        let initiator = Url::parse("http://a.test/page").expect("page URL");
        DialRequest::new(policy, &url, &initiator.origin())
    }

    // https://w3c.github.io/webappsec-referrer-policy/#integration-with-css
    #[test]
    fn a_sheet_referrer_belongs_to_the_document_that_loaded_it() {
        let loader = Url::parse("http://a.test/page").expect("page URL").origin();
        let other = Url::parse("http://b.test/page").expect("page URL").origin();
        let sheet = Url::parse("http://cdn.test/site.css").expect("sheet URL");
        let mut sheets = HashMap::new();
        sheets.insert(loader.clone(), HashSet::from([sheet.clone()]));

        assert!(
            referrer_authorized(&sheet, &loader, &sheets),
            "the document that loaded the sheet may name it"
        );
        assert!(
            !referrer_authorized(&sheet, &other, &sheets),
            "another origin in the tab may not borrow it"
        );
        let fragment = Url::parse("http://cdn.test/site.css#x").expect("sheet URL");
        assert!(
            referrer_authorized(&fragment, &loader, &sheets),
            "the comparison is on the fragment-stripped URL"
        );
    }

    // https://github.com/annevk/orb
    #[test]
    fn orb_stops_an_opaque_document_and_trims_an_opaque_head() {
        fn opaque(content_type: &str, body: &[u8]) -> DialOutcome {
            DialOutcome {
                status: 200,
                status_text: "OK".into(),
                final_url: Url::parse("http://b.test/img").expect("image URL"),
                content_type: Some(content_type.to_owned()),
                headers: vec![
                    ("content-type".to_owned(), content_type.to_owned()),
                    ("set-cookie".to_owned(), "leak=1".to_owned()),
                    ("cache-control".to_owned(), "max-age=60".to_owned()),
                ],
                body: ResponseBody::Bytes(body.to_vec()),
                response_type: net::cors::ResponseType::Opaque,
                crossed_origin: true,
                redirect_count: 0,
                cookies_stored: false,
                referrer: None,
            }
        }

        let request = dial(
            RequestPolicy::Image { crossorigin: None },
            "http://b.test/img",
        );
        let blocked = orb_filter(&request, opaque("image/png", b"<!doctype html>"));
        assert!(
            matches!(&blocked.body, ResponseBody::Bytes(bytes) if bytes.is_empty()),
            "a document labelled as an image keeps no body"
        );
        assert!(blocked.headers.is_empty(), "a blocked response has no head");
        assert_eq!(blocked.content_type, None);

        let kept = orb_filter(
            &request,
            opaque("image/png", b"\x89PNG\r\n\x1a\nimage bytes"),
        );
        assert!(
            matches!(&kept.body, ResponseBody::Bytes(bytes) if !bytes.is_empty()),
            "a real image keeps its body"
        );
        assert_eq!(
            kept.headers,
            [("content-type".to_owned(), "image/png".to_owned())],
            "an opaque head keeps only what decides how the body is read"
        );

        let fetch = dial(
            RequestPolicy::Fetch(renderer::CorsRequest {
                mode: renderer::CorsMode::NoCors,
                credentials: net::cors::CredentialsMode::Include,
                redirect: net::cors::RedirectMode::Follow,
            }),
            "http://b.test/img",
        );
        let untouched = orb_filter(&fetch, opaque("text/html", b"<!doctype html>"));
        assert!(
            matches!(&untouched.body, ResponseBody::Bytes(bytes) if !bytes.is_empty()),
            "the page-facing path keeps its own filtering"
        );
    }

    fn js_fetch(mode: renderer::CorsMode, url: &str) -> DialRequest {
        dial(
            RequestPolicy::Fetch(renderer::CorsRequest {
                mode,
                credentials: net::cors::CredentialsMode::SameOrigin,
                redirect: net::cors::RedirectMode::Follow,
            }),
            url,
        )
    }

    type WireCase = (
        RequestPolicy,
        net::cors::RequestMode,
        net::cors::CredentialsMode,
        net::cors::RedirectMode,
        &'static str,
        &'static str,
    );

    fn wire_table() -> Vec<WireCase> {
        use net::cors::{CredentialsMode, RedirectMode, RequestMode};

        let anonymous = Some(net::cors::CrossOrigin::Anonymous);
        let fetch = |mode, credentials| {
            RequestPolicy::Fetch(renderer::CorsRequest {
                mode,
                credentials,
                redirect: RedirectMode::Follow,
            })
        };
        let mut table = vec![
            (
                fetch(renderer::CorsMode::NoCors, CredentialsMode::Omit),
                RequestMode::NoCors,
                CredentialsMode::Omit,
                RedirectMode::Follow,
                "no-cors",
                "empty",
            ),
            (
                RequestPolicy::ClassicScript { crossorigin: None },
                RequestMode::NoCors,
                CredentialsMode::Include,
                RedirectMode::Follow,
                "no-cors",
                "script",
            ),
            (
                RequestPolicy::ClassicScript {
                    crossorigin: anonymous,
                },
                RequestMode::Cors,
                CredentialsMode::SameOrigin,
                RedirectMode::Follow,
                "cors",
                "script",
            ),
            (
                RequestPolicy::Stylesheet { crossorigin: None },
                RequestMode::NoCors,
                CredentialsMode::Include,
                RedirectMode::Follow,
                "no-cors",
                "style",
            ),
            (
                RequestPolicy::Image {
                    crossorigin: anonymous,
                },
                RequestMode::Cors,
                CredentialsMode::SameOrigin,
                RedirectMode::Follow,
                "cors",
                "image",
            ),
            (
                RequestPolicy::Font,
                RequestMode::Cors,
                CredentialsMode::SameOrigin,
                RedirectMode::Follow,
                "cors",
                "font",
            ),
            (
                RequestPolicy::FrameLoad,
                RequestMode::Navigate,
                CredentialsMode::Include,
                RedirectMode::Follow,
                "navigate",
                "iframe",
            ),
            (
                RequestPolicy::CspReportTo,
                RequestMode::Cors,
                CredentialsMode::SameOrigin,
                RedirectMode::Follow,
                "cors",
                "report",
            ),
            (
                RequestPolicy::CspReportUri,
                RequestMode::NoCors,
                CredentialsMode::SameOrigin,
                RedirectMode::Error,
                "no-cors",
                "report",
            ),
        ];
        table.extend(wire_module_table());
        table
    }

    // https://html.spec.whatwg.org/multipage/webappapis.html#fetch-a-single-module-script
    fn wire_module_table() -> [WireCase; 2] {
        use net::cors::{CredentialsMode, RedirectMode, RequestMode};

        [
            (
                RequestPolicy::ModuleScript {
                    credentials: CredentialsMode::Include,
                },
                RequestMode::Cors,
                CredentialsMode::Include,
                RedirectMode::Follow,
                "cors",
                "script",
            ),
            (
                RequestPolicy::ModuleWorkerScript {
                    credentials: CredentialsMode::Include,
                },
                RequestMode::SameOrigin,
                CredentialsMode::Include,
                RedirectMode::Follow,
                "same-origin",
                "script",
            ),
        ]
    }

    #[test]
    fn every_policy_reaches_the_wire_with_its_own_parameters() {
        let initiator = Url::parse("http://a.test/page").expect("page URL").origin();
        let deadline = Instant::now() + Duration::from_secs(5);
        let sheets = HashMap::new();
        for (policy, mode, credentials, redirect, fetch_mode, destination) in wire_table() {
            let request = dial(policy, "http://b.test/x");
            let outbound = outbound_request(&request, &initiator, deadline, &sheets)
                .expect("outbound request");
            assert_eq!(outbound.cors.mode, mode, "{:?}", policy.kind());
            assert_eq!(
                outbound.cors.credentials,
                credentials,
                "{:?}",
                policy.kind()
            );
            assert_eq!(outbound.cors.redirect, redirect, "{:?}", policy.kind());
            assert_eq!(
                outbound.cors.origin,
                initiator.clone(),
                "the browser fills in the origin it authorized"
            );
            assert_eq!(
                outbound.headers.get("sec-fetch-mode"),
                Some(fetch_mode.as_bytes()),
                "{:?}",
                policy.kind()
            );
            assert_eq!(
                outbound.headers.get("sec-fetch-dest"),
                Some(destination.as_bytes()),
                "{:?}",
                policy.kind()
            );
        }
    }

    // https://fetch.spec.whatwg.org/#concept-headers-guard
    #[test]
    fn a_no_cors_body_cannot_smuggle_a_non_safelisted_content_type() {
        use net::cors::{CredentialsMode, RedirectMode};

        let initiator = Url::parse("http://a.test/page").expect("page URL").origin();
        let deadline = Instant::now() + Duration::from_secs(5);
        let sheets = HashMap::new();
        let mut request = dial(
            RequestPolicy::Fetch(renderer::CorsRequest {
                mode: renderer::CorsMode::NoCors,
                credentials: CredentialsMode::Include,
                redirect: RedirectMode::Follow,
            }),
            "http://b.test/x",
        );
        request.method = net::Method::POST;
        request.content_type = Some("application/json".to_owned());
        let outbound =
            outbound_request(&request, &initiator, deadline, &sheets).expect("outbound request");
        assert!(
            outbound.headers.get("content-type").is_none(),
            "a JSON type would have needed a preflight"
        );

        request.content_type = Some("text/plain;charset=UTF-8".to_owned());
        let outbound =
            outbound_request(&request, &initiator, deadline, &sheets).expect("outbound request");
        assert_eq!(
            outbound.headers.get("content-type"),
            Some(b"text/plain;charset=UTF-8".as_slice()),
            "a safelisted type still reaches the wire"
        );

        let mut report = dial(RequestPolicy::CspReportUri, "http://b.test/report");
        report.method = net::Method::POST;
        report.content_type = Some("application/csp-report".to_owned());
        let outbound =
            outbound_request(&report, &initiator, deadline, &sheets).expect("outbound report");
        assert_eq!(
            outbound.headers.get("content-type"),
            Some(b"application/csp-report".as_slice()),
            "an engine-built request keeps the type its spec names"
        );
    }

    #[tokio::test]
    async fn a_cancelled_stream_releases_its_permit_for_the_next_dial() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let mut context =
            NetworkContext::new(AgentOptions::default(), Arc::new(NetworkBroker::new()));
        context.permits.global = Arc::new(Semaphore::new(1));
        let handle = context.tab_handle();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let address = listener.local_addr().expect("address");
        let origin = format!("http://{address}");
        let server = tokio::spawn(async move {
            for index in 0..2 {
                let (mut socket, _) = listener.accept().await.expect("accept");
                tokio::spawn(async move {
                    let mut buffer = [0u8; 1024];
                    let _ = socket.read(&mut buffer).await;
                    if index == 0 {
                        socket
                            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1024\r\n\r\npartial")
                            .await
                            .expect("stalled head");
                        tokio::time::sleep(Duration::from_secs(30)).await;
                    } else {
                        socket
                            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                            .await
                            .expect("complete head");
                    }
                });
            }
        });
        let initiator = Url::parse(&format!("{origin}/page")).expect("page URL");
        let owner = RendererAssignmentId::new(1);
        let mut stream_request =
            js_fetch(renderer::CorsMode::SameOrigin, &format!("{origin}/stream"));
        stream_request.initiator = renderer::SerializedOrigin::from(&initiator.origin());
        stream_request.referrer = renderer::ReferrerRequest::Client {
            url: initiator.clone(),
        };
        stream_request.delivery = BodyDelivery::Stream;
        let outcome = handle
            .dial_request(&stream_request, &initiator.origin(), owner)
            .await
            .expect("stream head");
        let ResponseBody::Streamed(stream) = outcome.body else {
            panic!("the outcome is a stream");
        };
        assert_eq!(
            handle.permits.global.available_permits(),
            0,
            "the body task holds the permits"
        );
        handle.cancel_stream(owner, stream);
        tokio::time::timeout(Duration::from_secs(1), async {
            while handle.permits.global.available_permits() == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("cancelling the stream releases the permit");

        let mut next_request = js_fetch(renderer::CorsMode::SameOrigin, &format!("{origin}/done"));
        next_request.initiator = renderer::SerializedOrigin::from(&initiator.origin());
        next_request.referrer = renderer::ReferrerRequest::Client {
            url: initiator.clone(),
        };
        let next = tokio::time::timeout(
            Duration::from_secs(1),
            handle.dial_request(&next_request, &initiator.origin(), owner),
        )
        .await
        .expect("the next dial gets the released permit")
        .expect("next dial");
        assert!(
            matches!(&next.body, ResponseBody::Bytes(body) if body == b"ok"),
            "the next dial reads its body"
        );
        server.abort();
    }

    // https://github.com/annevk/orb
    #[tokio::test]
    async fn orb_blocks_a_cross_origin_document_before_the_engine_reads_it() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let body: &[u8] = b"<!doctype html><p>secret</p>";
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            tokio::spawn(async move {
                let mut buffer = [0u8; 1024];
                let _ = socket.read(&mut buffer).await;
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: text/html\r\nSet-Cookie: a=1\r\n\r\n",
                    body.len()
                );
                socket
                    .write_all(head.as_bytes())
                    .await
                    .expect("response head");
                socket.write_all(body).await.expect("response body");
            });
        });
        let context = NetworkContext::new(AgentOptions::default(), Arc::new(NetworkBroker::new()));
        let handle = context.tab_handle();
        let initiator = Url::parse("http://localhost/page")
            .expect("page URL")
            .origin();
        let owner = RendererAssignmentId::new(1);

        let image = dial(
            RequestPolicy::Image { crossorigin: None },
            &format!("http://{address}/doc"),
        );
        let outcome = handle
            .dial_request(&image, &initiator, owner)
            .await
            .expect("image dial");
        assert!(
            matches!(&outcome.body, ResponseBody::Bytes(body) if body.is_empty()),
            "a document labelled as an image reaches the engine empty"
        );
        assert_eq!(outcome.content_type, None, "a blocked response has no type");
        assert!(
            outcome.headers.is_empty(),
            "a blocked response has no header list"
        );
        server.abort();
    }

    // https://fetch.spec.whatwg.org/#concept-internal-response
    #[tokio::test]
    async fn an_opaque_subresource_is_readable_by_the_engine_and_not_by_script() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            for _ in 0..2 {
                let (mut socket, _) = listener.accept().await.expect("accept");
                tokio::spawn(async move {
                    let mut buffer = [0u8; 1024];
                    let _ = socket.read(&mut buffer).await;
                    socket
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nContent-Type: image/png\r\nSet-Cookie: a=1\r\n\r\npng",
                        )
                        .await
                        .expect("response");
                });
            }
        });
        let context = NetworkContext::new(AgentOptions::default(), Arc::new(NetworkBroker::new()));
        let handle = context.tab_handle();
        let initiator = Url::parse("http://localhost/page")
            .expect("page URL")
            .origin();
        let owner = RendererAssignmentId::new(1);
        let url = format!("http://{address}/img");

        let image = dial(RequestPolicy::Image { crossorigin: None }, &url);
        let outcome = handle
            .dial_request(&image, &initiator, owner)
            .await
            .expect("image dial");
        assert_eq!(
            outcome.response_type,
            net::cors::ResponseType::Opaque,
            "a cross-origin no-cors image is opaque"
        );
        assert_eq!(outcome.status, 200, "the engine reads the unsafe status");
        assert_eq!(
            outcome.content_type.as_deref(),
            Some("image/png"),
            "the image decoder needs the served type"
        );
        assert!(
            matches!(&outcome.body, ResponseBody::Bytes(body) if body == b"png"),
            "the engine reads the unsafe body"
        );
        assert!(
            outcome
                .headers
                .iter()
                .all(|(name, _)| !name.eq_ignore_ascii_case("set-cookie")),
            "an HttpOnly cookie value must not reach the renderer"
        );

        let page = js_fetch(renderer::CorsMode::NoCors, &url);
        let outcome = handle
            .dial_request(&page, &initiator, owner)
            .await
            .expect("page dial");
        assert_eq!(
            outcome.status, 0,
            "an opaque filtered response has no status"
        );
        assert!(
            matches!(&outcome.body, ResponseBody::Bytes(body) if body.is_empty()),
            "an opaque filtered response has a null body"
        );
        assert!(
            outcome.headers.is_empty(),
            "an opaque filtered response has no headers"
        );
        server.abort();
    }

    #[tokio::test]
    async fn revalidation_sends_a_non_ascii_etag_as_one_byte_per_code_point() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let context = NetworkContext::new(AgentOptions::default(), Arc::new(NetworkBroker::new()));
        let handle = context.tab_handle();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let address = listener.local_addr().expect("address");
        let origin = format!("http://{address}");
        let (replayed_tx, replayed_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let mut replayed_tx = Some(replayed_tx);
            for index in 0..2 {
                let (mut socket, _) = listener.accept().await.expect("accept");
                let mut buffer = [0u8; 2048];
                let read = socket.read(&mut buffer).await.expect("request");
                if index == 0 {
                    socket
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nETag: \"\xE9\"\r\nSet-Cookie: sid=secret; HttpOnly\r\nContent-Length: 2\r\n\r\nhi",
                        )
                        .await
                        .expect("first response");
                } else {
                    let _ = replayed_tx
                        .take()
                        .expect("one replay")
                        .send(buffer[..read].to_vec());
                    socket
                        .write_all(
                            b"HTTP/1.1 304 Not Modified\r\nETag: \"\xE9\"\r\nSet-Cookie: sid=secret; HttpOnly\r\n\r\n",
                        )
                        .await
                        .expect("304");
                }
            }
        });
        let initiator = Url::parse(&format!("{origin}/page")).expect("page URL");
        let owner = RendererAssignmentId::new(1);
        let mut request = js_fetch(renderer::CorsMode::SameOrigin, &format!("{origin}/etag"));
        request.initiator = renderer::SerializedOrigin::from(&initiator.origin());
        request.referrer = renderer::ReferrerRequest::Client {
            url: initiator.clone(),
        };
        let first = handle
            .dial_request(&request, &initiator.origin(), owner)
            .await
            .expect("first dial");
        assert_eq!(first.status, 200);
        assert!(
            !first
                .headers
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case("set-cookie")),
            "an HttpOnly cookie value must not reach page script"
        );

        let revalidated = handle
            .dial_request(&request, &initiator.origin(), owner)
            .await
            .expect("revalidation");
        assert_eq!(revalidated.status, 200, "a 304 merges into the stored head");
        assert_eq!(stored_bytes(&revalidated), 2);
        assert!(matches!(&revalidated.body, ResponseBody::Bytes(body) if body == b"hi"));
        assert!(
            !revalidated
                .headers
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case("set-cookie")),
            "a 304 merge must not add the cookie the page path filtered out"
        );

        let replayed = replayed_rx.await.expect("second request");
        assert!(
            replayed
                .windows(b"if-none-match: \"\xE9\"".len())
                .any(|window| window == b"if-none-match: \"\xE9\""),
            "the validator went back as the bytes the server sent: {:?}",
            String::from_utf8_lossy(&replayed)
        );
        server.abort();
    }

    #[tokio::test]
    async fn exhausted_permits_fail_with_queue_full_before_the_deadline() {
        let permits = Arc::new(Semaphore::new(1));
        let _held = Arc::clone(&permits).acquire_owned().await.expect("permit");

        let error = TabNetworkHandle::acquire(&permits, Instant::now() + Duration::from_millis(50))
            .await
            .expect_err("second permit must time out");
        assert_eq!(error, DialFailure::QueueFull);
    }

    #[tokio::test]
    async fn released_permits_are_reusable() {
        let permits = Arc::new(Semaphore::new(1));
        {
            let _held = Arc::clone(&permits).acquire_owned().await.expect("permit");
            let error =
                TabNetworkHandle::acquire(&permits, Instant::now() + Duration::from_millis(20))
                    .await
                    .expect_err("held permit");
            assert_eq!(error, DialFailure::QueueFull);
        }
        let _again =
            TabNetworkHandle::acquire(&permits, Instant::now() + Duration::from_millis(50))
                .await
                .expect("permit after release");
    }

    // https://wicg.github.io/user-preference-media-features-headers/#sec-ch-prefers-color-scheme
    #[test]
    fn chrome_navigation_headers_are_limited_to_trustworthy_urls() {
        let secure = Url::parse("https://example.com/").expect("https url");
        let mut secure_request = net::Request::new(net::Method::GET, secure.clone());
        dial::apply_navigation_headers(&mut secure_request, &secure);
        assert!(
            secure_request
                .headers
                .get("sec-ch-prefers-color-scheme")
                .is_some(),
            "the preference hint reaches a trustworthy URL"
        );

        let insecure = Url::parse("http://example.com/").expect("http url");
        let mut insecure_request = net::Request::new(net::Method::GET, insecure.clone());
        dial::apply_navigation_headers(&mut insecure_request, &insecure);
        assert!(
            insecure_request
                .headers
                .get("sec-ch-prefers-color-scheme")
                .is_none(),
            "the preference hint stays off plain http"
        );
        assert!(
            secure_request.headers.get("sec-fetch-mode").is_some(),
            "the Fetch metadata group is built here and reaches trustworthy hops"
        );
        assert!(insecure_request.headers.get("accept").is_some());
        // https://w3c.github.io/webappsec-fetch-metadata/#fetch-integration
    }
}
