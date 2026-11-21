// https://fetch.spec.whatwg.org/#concept-response-body

use crate::dial_failure;
use renderer::DialFailure;

pub enum BodyEvent {
    Chunk(Vec<u8>),
    Done(Vec<u8>),
    DecodeError,
    Failed(DialFailure),
}

pub struct BodyReader {
    body: net::Body,
    integrity: Option<String>,
    collect: bool,
    collected: Vec<u8>,
    total: usize,
}

impl BodyReader {
    #[must_use]
    pub async fn new(response: net::Response, integrity: Option<String>, collect: bool) -> Self {
        Self {
            body: response.into_body().await,
            integrity,
            collect,
            collected: Vec::new(),
            total: 0,
        }
    }

    pub async fn next(&mut self) -> BodyEvent {
        match self.body.read_chunk().await {
            Ok(Some(chunk)) => {
                self.total = self.total.saturating_add(chunk.len());
                if self.total > renderer::MAX_RESPONSE_BODY_BYTES {
                    return BodyEvent::Failed(DialFailure::Limit);
                }
                if self.collect {
                    self.collected.extend_from_slice(&chunk);
                }
                BodyEvent::Chunk(chunk)
            }
            Ok(None) => {
                if let Some(integrity) = self.integrity.as_deref()
                    && !renderer::integrity_verifies(integrity, &self.collected)
                {
                    return BodyEvent::Failed(DialFailure::Connect);
                }
                BodyEvent::Done(std::mem::take(&mut self.collected))
            }
            Err(net::NetError::Decode(_)) => BodyEvent::DecodeError,
            Err(error) => BodyEvent::Failed(dial_failure(&error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;

    use base64::Engine as _;
    use bytes::Bytes;
    use net::{
        Agent, AgentOptions, HopBudget, Method, NetError, Request, TimeoutKind, Transport,
        TransportError, WireRequest, WireResponse, buffered_body,
    };
    use sha2::{Digest as _, Sha256};

    use super::*;

    struct FixedTransport(Bytes);

    impl Transport for FixedTransport {
        fn exchange(
            &self,
            _request: WireRequest,
            _budget: HopBudget,
        ) -> Pin<Box<dyn Future<Output = Result<WireResponse, NetError>> + Send + '_>> {
            let body = self.0.clone();
            Box::pin(async move {
                Ok(WireResponse {
                    status: 200,
                    headers: net::HeaderMap::new(),
                    body: buffered_body(body),
                    http2: false,
                })
            })
        }
    }

    async fn read_all(body: Bytes, integrity: Option<String>) -> BodyEvent {
        let agent = Agent::with_transport(
            AgentOptions::default(),
            Arc::new(FixedTransport(body)) as Arc<dyn Transport>,
        );
        let request = Request::new(
            Method::GET,
            url::Url::parse("https://example.test/res").unwrap(),
        );
        let response = agent.send(request).await.expect("send");
        let mut reader = BodyReader::new(response, integrity, true).await;
        loop {
            match reader.next().await {
                BodyEvent::Chunk(_) => {}
                terminal => return terminal,
            }
        }
    }

    fn sha256_sri(bytes: &[u8]) -> String {
        let digest = Sha256::digest(bytes);
        format!(
            "sha256-{}",
            base64::engine::general_purpose::STANDARD.encode(digest)
        )
    }

    #[tokio::test]
    async fn body_reader_enforces_sri_and_size_cap() {
        let ok = read_all(Bytes::from_static(b"hi"), Some(sha256_sri(b"hi"))).await;
        assert!(matches!(ok, BodyEvent::Done(ref data) if data == b"hi"));

        let bad_sri = read_all(Bytes::from_static(b"hi"), Some(sha256_sri(b"ho"))).await;
        assert!(matches!(bad_sri, BodyEvent::Failed(DialFailure::Connect)));

        let oversized = Bytes::from(vec![0u8; renderer::MAX_RESPONSE_BODY_BYTES + 1]);
        let capped = read_all(oversized, None).await;
        assert!(matches!(capped, BodyEvent::Failed(DialFailure::Limit)));
    }

    #[test]
    fn dial_failure_maps_every_net_error_variant() {
        let cases = [
            (
                NetError::Transport(TransportError::Dns("x".into())),
                DialFailure::Dns,
            ),
            (
                NetError::Transport(TransportError::Connect("x".into())),
                DialFailure::Connect,
            ),
            (
                NetError::Transport(TransportError::Io(std::io::Error::other("x"))),
                DialFailure::Connect,
            ),
            (
                NetError::Transport(TransportError::Tls("x".into())),
                DialFailure::Tls,
            ),
            (
                NetError::Transport(TransportError::Timeout(TimeoutKind::PerCall)),
                DialFailure::Timeout,
            ),
            (
                NetError::Transport(TransportError::Capacity),
                DialFailure::QueueFull,
            ),
            (
                NetError::Transport(TransportError::BodyTooLarge),
                DialFailure::Limit,
            ),
            (NetError::RedirectLimit, DialFailure::Limit),
            (NetError::RejectedRequest, DialFailure::Connect),
            (NetError::Decode("x".into()), DialFailure::Connect),
        ];
        for (error, expected) in cases {
            assert_eq!(dial_failure(&error), expected, "{error:?}");
        }
    }
}
