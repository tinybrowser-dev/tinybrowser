//! The fetch core's byte mover over the host contract.
//!
//! The fetch core shapes every request: redirects, cookies, CORS, `Sec-Fetch-*`,
//! and the referrer are decided above this module. [`HostTransport`] turns one
//! shaped [`WireRequest`] into one `host.start-fetch` and hands the answer back
//! as a [`WireResponse`], which is the same port the native hyper engine
//! implements
//! (<https://fetch.spec.whatwg.org/#concept-http-network-fetch>).

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use bytes::Bytes;
use net::{
    HopBudget, NetError, TimeoutKind, Transport, TransportError, WireRequest, WireResponse,
    buffered_body, isomorphic_decode, isomorphic_encode,
};

use crate::bindings::tinybrowser::browser::host;
use crate::bindings::tinybrowser::browser::types::{
    FetchError, FetchRequest, FetchResponse, Header,
};
use crate::lock;

/// The host's answer to one exchange, in the host contract's own types.
pub(crate) type Answer = Result<FetchResponse, FetchError>;

/// One exchange, parked until the host completes it.
struct Exchange {
    /// Deposited once by [`HostTransport::complete`] and taken once by the
    /// future that is waiting on it.
    answer: Mutex<Option<Answer>>,
}

/// The tab whose dial is being polled right now.
///
/// A dial's future is polled only from [`crate::DialRegistry::poll`], which the
/// host drives through `engine.complete-fetch`, so this slot is set for exactly
/// the poll that opens an exchange. The host contract forbids completing an
/// exchange before `start-fetch` returns, so no poll nests inside another and
/// one slot is enough.
struct PollContext {
    owner: u64,
    /// The exchange this poll opened, for the driver to learn what a pending
    /// dial waits on.
    opened: Option<u64>,
}

/// The [`Transport`] a component runs the fetch core against.
///
/// One instance serves the whole component instance, because the agent owns the
/// cookie jar: every tab's dials must pass through the same transport for the
/// jar to see one set of responses.
pub(crate) struct HostTransport {
    exchanges: Mutex<HashMap<u64, Arc<Exchange>>>,
    polled: Mutex<Option<PollContext>>,
    /// Cap on one response body, from the renderer's own limit.
    max_body_bytes: u64,
    next_id: AtomicU64,
}

impl HostTransport {
    pub(crate) fn new(max_body_bytes: u64) -> Self {
        Self {
            exchanges: Mutex::new(HashMap::new()),
            polled: Mutex::new(None),
            max_body_bytes,
            next_id: AtomicU64::new(1),
        }
    }

    /// Marks the start of one dial's poll, attributed to `owner`.
    pub(crate) fn begin_poll(&self, owner: u64) {
        *lock(&self.polled) = Some(PollContext {
            owner,
            opened: None,
        });
    }

    /// Ends the poll and reports the exchange it opened, when it opened one.
    pub(crate) fn end_poll(&self) -> Option<u64> {
        lock(&self.polled).take().and_then(|context| context.opened)
    }

    /// Deposits the host's answer. Returns whether an exchange was waiting:
    /// `false` means the request was cancelled or its tab went away.
    pub(crate) fn complete(&self, id: u64, answer: Answer) -> bool {
        let Some(exchange) = lock(&self.exchanges).remove(&id) else {
            return false;
        };
        *lock(&exchange.answer) = Some(answer);
        true
    }

    /// Drops an exchange nobody will resume, because its dial was cancelled.
    pub(crate) fn abandon(&self, id: u64) {
        lock(&self.exchanges).remove(&id);
    }
}

impl Transport for HostTransport {
    fn exchange(
        &self,
        request: WireRequest,
        budget: HopBudget,
    ) -> Pin<Box<dyn Future<Output = Result<WireResponse, NetError>> + Send + '_>> {
        Box::pin(async move {
            let id = self.next_id.fetch_add(1, Ordering::Relaxed);
            let owner = {
                let mut polled = lock(&self.polled);
                let Some(context) = polled.as_mut() else {
                    return Err(refused("the fetch core ran outside a dial poll"));
                };
                context.owner
            };
            let host_request = FetchRequest {
                id,
                owner,
                method: request.method.as_str().to_owned(),
                url: request.url.to_string(),
                headers: request
                    .headers
                    .iter()
                    .map(|(name, value)| Header {
                        name: name.to_owned(),
                        value: isomorphic_decode(value),
                    })
                    .collect(),
                body: request.body.as_ref().map(|body| body.to_vec()),
                timeout_ms: budget
                    .remaining()
                    .map(|left| u64::try_from(left.as_millis()).unwrap_or(u64::MAX)),
                max_body_bytes: self.max_body_bytes,
            };
            let exchange = Arc::new(Exchange {
                answer: Mutex::new(None),
            });
            lock(&self.exchanges).insert(id, Arc::clone(&exchange));
            if !host::start_fetch(&host_request) {
                // The host cannot serve this request at all, so no completion
                // is owed and none may be waited for.
                self.abandon(id);
                return Err(refused("the host refused the exchange"));
            }
            if let Some(context) = lock(&self.polled).as_mut() {
                context.opened = Some(id);
            }
            let answer = AnswerSlot {
                exchange: &exchange,
            }
            .await;
            let FetchResponse {
                status,
                http2,
                headers,
                body,
            } = answer.map_err(|error| Self::map_error(error, &request, &budget))?;
            let mut mapped = net::HeaderMap::new();
            for Header { name, value } in headers {
                let value = isomorphic_encode(&value)
                    .ok_or_else(|| NetError::Decode(format!("{name}: not isomorphic").into()))?;
                mapped
                    .insert(&name, value)
                    .map_err(|error| NetError::Decode(error.to_string().into()))?;
            }
            Ok(WireResponse {
                status,
                headers: mapped,
                body: buffered_body(Bytes::from(body)),
                http2,
            })
        })
    }
}

impl HostTransport {
    /// The fetch error the host contract reports, in the core's vocabulary.
    ///
    /// The kinds are distinct because the page engine reports them distinct:
    /// a capacity refusal is not a connect failure, and a body over the cap is
    /// a limit rather than a network error.
    fn map_error(error: FetchError, request: &WireRequest, budget: &HopBudget) -> NetError {
        let transport = match error {
            FetchError::Dns => {
                TransportError::Dns(request.url.host_str().unwrap_or_default().into())
            }
            FetchError::Connect => TransportError::Connect("host exchange failed".into()),
            FetchError::Tls => TransportError::Tls("host tls failure".into()),
            FetchError::Timeout => {
                TransportError::Timeout(budget.timeout_kind(TimeoutKind::PerCall))
            }
            FetchError::Limit => TransportError::BodyTooLarge,
            FetchError::QueueFull => TransportError::Capacity,
            // The component cancels by dropping the dial, so a host that
            // reports this ran past its cancellation or never had one.
            FetchError::Cancelled => TransportError::Connect("host cancelled the exchange".into()),
        };
        NetError::Transport(transport)
    }
}

/// The answer one exchange waits for.
struct AnswerSlot<'a> {
    exchange: &'a Exchange,
}

impl Future for AnswerSlot<'_> {
    type Output = Answer;

    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Answer> {
        // The driver polls this future only after depositing an answer, so
        // `Pending` here never waits on a waker: the component has no executor
        // and needs none.
        match lock(&self.exchange.answer).take() {
            Some(answer) => Poll::Ready(answer),
            None => Poll::Pending,
        }
    }
}

fn refused(reason: &str) -> NetError {
    NetError::Transport(TransportError::Connect(reason.into()))
}
