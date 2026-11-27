// https://fetch.spec.whatwg.org/#concept-http-network-fetch

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

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

struct ExchangeState {
    answer: Option<Answer>,
    waker: Option<Waker>,
}

struct Exchange {
    state: Mutex<ExchangeState>,
}

impl Exchange {
    fn new() -> Self {
        Self {
            state: Mutex::new(ExchangeState {
                answer: None,
                waker: None,
            }),
        }
    }
}

pub(crate) struct HostTransport {
    exchanges: Mutex<HashMap<u64, Arc<Exchange>>>,
    max_body_bytes: u64,
    next_id: AtomicU64,
}

impl HostTransport {
    pub(crate) fn new(max_body_bytes: u64) -> Self {
        Self {
            exchanges: Mutex::new(HashMap::new()),
            max_body_bytes,
            next_id: AtomicU64::new(1),
        }
    }

    pub(crate) fn complete(&self, id: u64, answer: Answer) -> bool {
        let Some(exchange) = lock(&self.exchanges).remove(&id) else {
            return false;
        };
        let waker = {
            let mut state = lock(&exchange.state);
            state.answer = Some(answer);
            state.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
        true
    }

    pub(crate) fn abandon(&self, id: u64) -> bool {
        lock(&self.exchanges).remove(&id).is_some()
    }

    pub(crate) fn exchange_for(
        &self,
        owner: u64,
        request: WireRequest,
        budget: HopBudget,
    ) -> Pin<Box<dyn Future<Output = Result<WireResponse, NetError>> + Send + '_>> {
        Box::pin(async move {
            let id = self.next_id.fetch_add(1, Ordering::Relaxed);
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
            let exchange = Arc::new(Exchange::new());
            lock(&self.exchanges).insert(id, Arc::clone(&exchange));
            if !host::start_fetch(&host_request) {
                let _removed = self.abandon(id);
                return Err(refused("the host refused the exchange"));
            }
            let answer = AnswerSlot {
                id,
                transport: self,
                exchange: &exchange,
                done: false,
            }
            .await;
            let FetchResponse {
                status,
                http2,
                headers,
                body,
            } = answer.map_err(|error| {
                Self::map_error(
                    error,
                    request.url.host_str().unwrap_or_default(),
                    budget.timeout_kind(TimeoutKind::PerCall),
                )
            })?;
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

pub(crate) struct OwnerTransport {
    inner: Arc<HostTransport>,
    owner: u64,
}

impl OwnerTransport {
    pub(crate) fn new(inner: Arc<HostTransport>, owner: u64) -> Self {
        Self { inner, owner }
    }
}

impl Transport for OwnerTransport {
    fn exchange(
        &self,
        request: WireRequest,
        budget: HopBudget,
    ) -> Pin<Box<dyn Future<Output = Result<WireResponse, NetError>> + Send + '_>> {
        self.inner.exchange_for(self.owner, request, budget)
    }
}

impl Transport for HostTransport {
    fn exchange(
        &self,
        _request: WireRequest,
        _budget: HopBudget,
    ) -> Pin<Box<dyn Future<Output = Result<WireResponse, NetError>> + Send + '_>> {
        Box::pin(async { Err(refused("the fetch request has no owner")) })
    }
}

impl HostTransport {
    fn map_error(error: FetchError, host: &str, timeout_kind: TimeoutKind) -> NetError {
        let transport = match error {
            FetchError::Dns => TransportError::Dns(host.into()),
            FetchError::Connect => TransportError::Connect("host exchange failed".into()),
            FetchError::Tls => TransportError::Tls("host tls failure".into()),
            FetchError::Timeout => TransportError::Timeout(timeout_kind),
            FetchError::Limit => TransportError::BodyTooLarge,
            FetchError::QueueFull => TransportError::Capacity,
            FetchError::Cancelled => TransportError::Connect("host cancelled the exchange".into()),
        };
        NetError::Transport(transport)
    }
}

struct AnswerSlot<'a> {
    id: u64,
    transport: &'a HostTransport,
    exchange: &'a Exchange,
    done: bool,
}

impl Future for AnswerSlot<'_> {
    type Output = Answer;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Answer> {
        let mut state = lock(&self.exchange.state);
        if let Some(answer) = state.answer.take() {
            drop(state);
            self.done = true;
            Poll::Ready(answer)
        } else {
            state.waker = Some(cx.waker().clone());
            Poll::Pending
        }
    }
}

impl Drop for AnswerSlot<'_> {
    fn drop(&mut self) {
        if !self.done && self.transport.abandon(self.id) {
            host::cancel_fetch(self.id);
        }
    }
}

fn refused(reason: &str) -> NetError {
    NetError::Transport(TransportError::Connect(reason.into()))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;
    use std::task::Wake;

    use renderer::DialFailure;

    use super::*;

    struct FlagWaker(AtomicBool);

    impl Wake for FlagWaker {
        fn wake(self: Arc<Self>) {
            self.0.store(true, Ordering::SeqCst);
        }

        fn wake_by_ref(self: &Arc<Self>) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[test]
    fn host_failures_map_to_dial_failures() {
        let cases = [
            (FetchError::Dns, DialFailure::Dns),
            (FetchError::Connect, DialFailure::Connect),
            (FetchError::Tls, DialFailure::Tls),
            (FetchError::Timeout, DialFailure::Timeout),
            (FetchError::Limit, DialFailure::Limit),
            (FetchError::QueueFull, DialFailure::QueueFull),
            (FetchError::Cancelled, DialFailure::Connect),
        ];
        for (error, expected) in cases {
            let mapped = HostTransport::map_error(error, "example.test", TimeoutKind::PerCall);
            assert_eq!(dial::dial_failure(&mapped), expected, "{error:?}");
        }
        let dns = HostTransport::map_error(FetchError::Dns, "missing.test", TimeoutKind::PerCall);
        assert!(
            matches!(dns, NetError::Transport(TransportError::Dns(ref host)) if host.as_ref() == "missing.test")
        );
    }

    #[test]
    fn non_ascii_header_bytes_survive_isomorphic_round_trip() {
        let raw = b"caf\xE9";
        let decoded = isomorphic_decode(raw);
        assert_eq!(isomorphic_encode(&decoded).as_deref(), Some(raw.as_slice()));
    }

    #[test]
    fn complete_wakes_registered_waker_and_abandon_prevents_delivery() {
        let transport = HostTransport::new(1024);
        let exchange = Arc::new(Exchange::new());
        lock(&transport.exchanges).insert(7, Arc::clone(&exchange));

        let flag = Arc::new(FlagWaker(AtomicBool::new(false)));
        let waker = Waker::from(Arc::clone(&flag));
        let mut cx = Context::from_waker(&waker);
        let mut slot = AnswerSlot {
            id: 7,
            transport: &transport,
            exchange: &exchange,
            done: false,
        };
        assert!(Pin::new(&mut slot).poll(&mut cx).is_pending());
        assert!(!flag.0.load(Ordering::SeqCst));

        assert!(transport.complete(7, Err(FetchError::Timeout)));
        assert!(flag.0.load(Ordering::SeqCst));
        assert!(matches!(
            Pin::new(&mut slot).poll(&mut cx),
            Poll::Ready(Err(FetchError::Timeout))
        ));

        let abandoned = Arc::new(Exchange::new());
        lock(&transport.exchanges).insert(42, Arc::clone(&abandoned));
        assert!(transport.abandon(42));
        assert!(!transport.complete(42, Err(FetchError::Connect)));
        assert!(lock(&abandoned.state).answer.is_none());
    }
}
