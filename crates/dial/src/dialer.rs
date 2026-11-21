// https://fetch.spec.whatwg.org/#concept-main-fetch

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use net::{Agent, Request};
use renderer::{BodyDelivery, DialFailure, DialKind, DialOutcome, DialRequest, ResponseBody};
use url::Url;

use crate::body::{BodyEvent, BodyReader};
use crate::cache::{CacheDecision, HttpCache, Validators};
use crate::{
    DialHead, dial_failure, outbound_request, refused_redirect, sent_headers, watch_redirects,
};

pub trait Admission: Send + Sync {
    type Permit: Send;

    fn acquire(
        &self,
        deadline: Instant,
    ) -> Pin<Box<dyn Future<Output = Result<Self::Permit, DialFailure>> + Send + '_>>;
}

pub struct Unlimited;

impl Admission for Unlimited {
    type Permit = ();

    fn acquire(
        &self,
        _deadline: Instant,
    ) -> Pin<Box<dyn Future<Output = Result<(), DialFailure>> + Send + '_>> {
        Box::pin(std::future::ready(Ok(())))
    }
}

pub struct StreamInput<'a, P> {
    pub request: &'a DialRequest,
    pub sent: &'a [(String, String)],
    pub initiator: &'a url::Origin,
    pub partition: u64,
    pub response: Box<net::Response>,
    pub permit: P,
    pub cache: &'a Arc<HttpCache>,
}

pub enum StreamStart<P> {
    Streamed(Box<DialOutcome>),
    Buffer {
        response: Box<net::Response>,
        permit: P,
    },
}

pub trait BodySink<P>: Send + Sync {
    fn start_stream(&self, input: StreamInput<'_, P>) -> StreamStart<P>;
}

pub struct BufferedOnly;

impl<P> BodySink<P> for BufferedOnly {
    fn start_stream(&self, input: StreamInput<'_, P>) -> StreamStart<P> {
        StreamStart::Buffer {
            response: input.response,
            permit: input.permit,
        }
    }
}

enum HeadResult<P> {
    Done(Box<DialOutcome>),
    Response {
        response: Box<net::Response>,
        permit: P,
    },
}

#[derive(Clone)]
pub struct Dialer {
    agent: Agent,
    cache: Arc<HttpCache>,
    // https://w3c.github.io/webappsec-referrer-policy/#integration-with-css
    sheets: Arc<Mutex<HashMap<url::Origin, HashSet<Url>>>>,
}

impl Dialer {
    #[must_use]
    pub fn new(agent: Agent, cache: Arc<HttpCache>) -> Self {
        Self {
            agent,
            cache,
            sheets: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    #[must_use]
    pub fn agent(&self) -> &Agent {
        &self.agent
    }

    #[must_use]
    pub fn cache(&self) -> &Arc<HttpCache> {
        &self.cache
    }

    pub async fn dial<A, S>(
        &self,
        request: &DialRequest,
        initiator: &url::Origin,
        partition: u64,
        client: Option<u64>,
        admission: &A,
        sink: &S,
    ) -> Result<DialOutcome, DialFailure>
    where
        A: Admission,
        S: BodySink<A::Permit>,
    {
        let outcome = self
            .dial_unfiltered(request, initiator, partition, client, admission, sink)
            .await?;
        Ok(orb_filter(request, outcome))
    }

    async fn dial_unfiltered<A, S>(
        &self,
        request: &DialRequest,
        initiator: &url::Origin,
        partition: u64,
        client: Option<u64>,
        admission: &A,
        sink: &S,
    ) -> Result<DialOutcome, DialFailure>
    where
        A: Admission,
        S: BodySink<A::Permit>,
    {
        let deadline = Instant::now() + crate::budget(request);
        let mut outbound = self.build_outbound(request, initiator, deadline)?;
        outbound.client = client;
        // https://www.rfc-editor.org/rfc/rfc9111#section-4.1
        let mut sent = sent_headers(&outbound);
        if let Some(cookie) = self.agent.outbound_cookie_header(&outbound) {
            sent.push(("cookie".to_owned(), cookie));
        }
        let (stored, validators) = match self.cache.read(request, &sent, initiator, partition)? {
            CacheDecision::Hit(hit) => return Ok(hit),
            CacheDecision::Miss {
                stored, validators, ..
            } => (stored, validators),
        };
        let result = match self
            .send_head(
                request,
                initiator,
                stored.as_ref(),
                &validators,
                outbound,
                admission,
            )
            .await?
        {
            HeadResult::Done(outcome) => {
                self.cache
                    .remember(request, &sent, initiator, partition, &outcome);
                Ok(*outcome)
            }
            HeadResult::Response { response, permit } => {
                if request.delivery == BodyDelivery::Stream {
                    match sink.start_stream(StreamInput {
                        request,
                        sent: &sent,
                        initiator,
                        partition,
                        response,
                        permit,
                        cache: &self.cache,
                    }) {
                        StreamStart::Streamed(outcome) => Ok(*outcome),
                        StreamStart::Buffer { response, permit } => {
                            self.buffer_response(
                                request, &sent, initiator, partition, *response, permit,
                            )
                            .await
                        }
                    }
                } else {
                    self.buffer_response(request, &sent, initiator, partition, *response, permit)
                        .await
                }
            }
        };
        if let Ok(outcome) = &result {
            self.remember_sheet(request, initiator, outcome.status, &outcome.final_url);
        }
        result
    }

    async fn send_head<A: Admission>(
        &self,
        request: &DialRequest,
        initiator: &url::Origin,
        stored: Option<&DialOutcome>,
        validators: &Validators,
        mut outbound: Request,
        admission: &A,
    ) -> Result<HeadResult<A::Permit>, DialFailure> {
        let deadline = Instant::now() + crate::budget(request);
        let permit = admission.acquire(deadline).await?;
        validators.apply(&mut outbound)?;
        let blocked_redirect = watch_redirects(request, initiator, &mut outbound);
        let response = match self.agent.send(outbound).await {
            Ok(response) => response,
            Err(error) => {
                let done =
                    refused_redirect(&blocked_redirect).ok_or_else(|| dial_failure(&error))?;
                return Ok(HeadResult::Done(Box::new(done)));
            }
        };
        // https://fetch.spec.whatwg.org/#concept-main-fetch
        if request.integrity.is_some() && response.response_type().is_opaque() {
            return Err(DialFailure::Connect);
        }
        if response.status() == 304
            && let Some(stored) = stored
        {
            // https://www.rfc-editor.org/rfc/rfc9111#section-4.3.4
            let merged = HttpCache::merge_not_modified(stored, &response, validators)
                .ok_or(DialFailure::Connect)?;
            return Ok(HeadResult::Done(Box::new(merged)));
        }
        Ok(HeadResult::Response {
            response: Box::new(response),
            permit,
        })
    }

    async fn buffer_response<P>(
        &self,
        request: &DialRequest,
        sent: &[(String, String)],
        initiator: &url::Origin,
        partition: u64,
        response: net::Response,
        permit: P,
    ) -> Result<DialOutcome, DialFailure> {
        let _permit = permit;
        let head = DialHead::read(request, &response);
        let body = if head.null_body() {
            ResponseBody::Bytes(Vec::new())
        } else {
            let mut reader = BodyReader::new(response, request.integrity.clone(), true).await;
            loop {
                match reader.next().await {
                    BodyEvent::Chunk(_) => {}
                    BodyEvent::Done(body) => break ResponseBody::Bytes(body),
                    BodyEvent::DecodeError => break ResponseBody::Undecodable,
                    BodyEvent::Failed(failure) => return Err(failure),
                }
            }
        };
        let outcome = head.into_outcome(body);
        self.cache
            .remember(request, sent, initiator, partition, &outcome);
        Ok(outcome)
    }

    // https://w3c.github.io/webappsec-referrer-policy/#integration-with-css
    fn remember_sheet(
        &self,
        request: &DialRequest,
        initiator: &url::Origin,
        status: u16,
        final_url: &Url,
    ) {
        if request.policy.kind() != DialKind::Stylesheet || !(200..300).contains(&status) {
            return;
        }
        // https://w3c.github.io/webappsec-referrer-policy/#strip-url
        if !initiator.is_tuple() {
            return;
        }
        let mut sheets = self
            .sheets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let loaded = sheets.entry(initiator.clone()).or_default();
        for url in [&request.url, final_url] {
            // https://w3c.github.io/webappsec-referrer-policy/#strip-url
            let mut url = url.clone();
            url.set_fragment(None);
            loaded.insert(url);
        }
    }

    fn build_outbound(
        &self,
        request: &DialRequest,
        initiator: &url::Origin,
        deadline: Instant,
    ) -> Result<Request, DialFailure> {
        let sheets = self
            .sheets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        outbound_request(request, initiator, deadline, &sheets)
    }
}

// https://github.com/annevk/orb
#[must_use]
pub fn orb_filter(request: &DialRequest, outcome: DialOutcome) -> DialOutcome {
    if request.policy.exposes_response() || !outcome.response_type.is_opaque() {
        return outcome;
    }
    let nosniff = outcome.headers.iter().any(|(name, value)| {
        name.eq_ignore_ascii_case("x-content-type-options")
            && value
                .split(',')
                .any(|option| option.trim().eq_ignore_ascii_case("nosniff"))
    });
    let body = match &outcome.body {
        ResponseBody::Bytes(bytes) => Some(bytes.as_slice()),
        ResponseBody::Undecodable | ResponseBody::Streamed(_) => None,
    };
    if net::orb::blocks(outcome.content_type.as_deref(), nosniff, body) {
        return DialOutcome {
            content_type: None,
            headers: Vec::new(),
            body: ResponseBody::Bytes(Vec::new()),
            ..outcome
        };
    }
    let headers = outcome
        .headers
        .iter()
        .filter(|(name, _)| {
            name.eq_ignore_ascii_case("content-type")
                || name.eq_ignore_ascii_case("x-content-type-options")
        })
        .cloned()
        .collect();
    DialOutcome { headers, ..outcome }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use bytes::Bytes;
    use net::{
        AgentOptions, HeaderMap, HopBudget, NetError, Transport, WireRequest, WireResponse,
        buffered_body,
    };
    use renderer::{DialRequest, ReferrerRequest, RequestPolicy};

    use super::*;

    struct RecordingTransport {
        calls: AtomicUsize,
        last_client: Mutex<Option<u64>>,
        last_referer: Mutex<Option<String>>,
    }

    impl RecordingTransport {
        fn new() -> Self {
            Self {
                calls: AtomicUsize::new(0),
                last_client: Mutex::new(None),
                last_referer: Mutex::new(None),
            }
        }
    }

    impl Transport for RecordingTransport {
        fn exchange(
            &self,
            request: WireRequest,
            _budget: HopBudget,
        ) -> Pin<Box<dyn Future<Output = Result<WireResponse, NetError>> + Send + '_>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            *self.last_client.lock().unwrap() = request.client;
            *self.last_referer.lock().unwrap() = request
                .headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("referer"))
                .map(|(_, value)| net::isomorphic_decode(value));
            Box::pin(async move {
                let mut headers = HeaderMap::new();
                headers.insert("cache-control", "max-age=60").unwrap();
                headers.insert("access-control-allow-origin", "*").unwrap();
                headers.insert("content-type", "text/css").unwrap();
                Ok(WireResponse {
                    status: 200,
                    headers,
                    body: buffered_body(Bytes::from_static(b"body{}")),
                    http2: false,
                })
            })
        }
    }

    fn test_request(url: &str, policy: RequestPolicy, referrer: ReferrerRequest) -> DialRequest {
        let url = Url::parse(url).unwrap();
        let origin = Url::parse("https://page.test/").unwrap().origin();
        let mut request = DialRequest::new(policy, &url, &origin);
        request.referrer = referrer;
        request.referrer_policy = net::ReferrerPolicy::NoReferrerWhenDowngrade;
        request
    }

    #[tokio::test]
    async fn dialer_caches_responses_forwards_client_and_authorizes_sheet_referrer() {
        let transport = Arc::new(RecordingTransport::new());
        let options = AgentOptions::default();
        let cache = Arc::new(HttpCache::new(options.clock.clone()));
        let agent = Agent::with_transport(options, Arc::clone(&transport) as Arc<dyn Transport>);
        let dialer = Dialer::new(agent, cache);
        let origin = Url::parse("https://page.test/").unwrap().origin();

        let fetch_req = test_request(
            "https://page.test/data",
            RequestPolicy::Fetch(renderer::CorsRequest {
                mode: renderer::CorsMode::Cors,
                credentials: net::cors::CredentialsMode::Include,
                redirect: net::cors::RedirectMode::Follow,
            }),
            ReferrerRequest::Client {
                url: Url::parse("https://page.test/").unwrap(),
            },
        );
        let first = dialer
            .dial(&fetch_req, &origin, 1, Some(99), &Unlimited, &BufferedOnly)
            .await
            .expect("first dial");
        assert_eq!(first.status, 200);
        assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
        assert_eq!(*transport.last_client.lock().unwrap(), Some(99));

        let cached = dialer
            .dial(&fetch_req, &origin, 1, Some(99), &Unlimited, &BufferedOnly)
            .await
            .expect("cached dial");
        assert_eq!(cached.status, 200);
        assert_eq!(transport.calls.load(Ordering::SeqCst), 1);

        let sheet_req = test_request(
            "https://cdn.test/style.css",
            RequestPolicy::Stylesheet {
                crossorigin: Some(net::cors::CrossOrigin::Anonymous),
            },
            ReferrerRequest::Client {
                url: Url::parse("https://page.test/").unwrap(),
            },
        );
        let _sheet = dialer
            .dial(&sheet_req, &origin, 1, Some(99), &Unlimited, &BufferedOnly)
            .await
            .expect("sheet dial");

        let font_req = test_request(
            "https://cdn.test/font.woff2",
            RequestPolicy::Font,
            ReferrerRequest::Url(Url::parse("https://cdn.test/style.css").unwrap()),
        );
        let _font = dialer
            .dial(&font_req, &origin, 1, Some(99), &Unlimited, &BufferedOnly)
            .await
            .expect("font dial");
        assert_eq!(
            transport.last_referer.lock().unwrap().as_deref(),
            Some("https://cdn.test/style.css")
        );
    }
}
