//! Reading one decoded response body under the carrier's cap and checks.
//!
//! Both delivery modes — buffered and streamed — and every carrier drive this
//! reader, so the size cap, the integrity check, and the mapping from
//! [`net::NetError`] to [`DialFailure`] exist once
//! (<https://fetch.spec.whatwg.org/#concept-response-body>).

use crate::dial_failure;
use renderer::DialFailure;

/// One event from [`BodyReader`].
pub enum BodyEvent {
    /// A decoded chunk. The reader has already collected it when the delivery
    /// mode needs the whole body.
    Chunk(Vec<u8>),
    /// The body ended; the collected bytes are what integrity checked.
    Done(Vec<u8>),
    /// The body could not be decoded for its `Content-Encoding`.
    DecodeError,
    /// The transfer failed before the body ended.
    Failed(DialFailure),
}

/// Reads one decoded response body. Both delivery modes drive this reader, so
/// the size cap, integrity check, and error mapping exist once.
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
