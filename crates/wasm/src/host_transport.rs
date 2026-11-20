// https://fetch.spec.whatwg.org/#concept-http-network-fetch

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

pub(crate) type Answer = Result<FetchResponse, FetchError>;

struct Exchange {
    answer: Mutex<Option<Answer>>,
}

struct PollContext {
    owner: u64,
    opened: Option<u64>,
}

pub(crate) struct HostTransport {
    exchanges: Mutex<HashMap<u64, Arc<Exchange>>>,
    polled: Mutex<Option<PollContext>>,
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

    pub(crate) fn begin_poll(&self, owner: u64) {
        *lock(&self.polled) = Some(PollContext {
            owner,
            opened: None,
        });
    }

    pub(crate) fn end_poll(&self) -> Option<u64> {
        lock(&self.polled).take().and_then(|context| context.opened)
    }

    pub(crate) fn complete(&self, id: u64, answer: Answer) {
        if let Some(exchange) = lock(&self.exchanges).remove(&id) {
            *lock(&exchange.answer) = Some(answer);
        }
    }

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
            FetchError::Cancelled => TransportError::Connect("host cancelled the exchange".into()),
        };
        NetError::Transport(transport)
    }
}

struct AnswerSlot<'a> {
    exchange: &'a Exchange,
}

impl Future for AnswerSlot<'_> {
    type Output = Answer;

    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Answer> {
        match lock(&self.exchange.answer).take() {
            Some(answer) => Poll::Ready(answer),
            None => Poll::Pending,
        }
    }
}

fn refused(reason: &str) -> NetError {
    NetError::Transport(TransportError::Connect(reason.into()))
}
