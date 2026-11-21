// https://fetch.spec.whatwg.org/#concept-http-network-fetch

use std::future::Future;
use std::pin::Pin;
use std::time::{Duration, Instant};

use bytes::Bytes;
use http_body_util::BodyExt as _;
use http_body_util::Full;
use url::Url;

use crate::client::Clock;
use crate::error::{NetError, TimeoutKind};
use crate::protocol::{HeaderMap, Method};

#[cfg(feature = "native")]
pub(crate) mod native;

#[derive(Clone, Debug)]
pub(crate) struct CallBudget {
    global: Option<Instant>,
    per_call: Option<Duration>,
    clock: Clock,
}

impl CallBudget {
    pub(crate) fn new(
        timeout_global: Option<Duration>,
        timeout_per_call: Option<Duration>,
        clock: Clock,
    ) -> Self {
        Self {
            global: timeout_global.map(|limit| clock.monotonic() + limit),
            per_call: timeout_per_call,
            clock,
        }
    }

    pub(crate) fn tighten_global(&mut self, deadline: Instant) {
        self.global = Some(self.global.map_or(deadline, |global| global.min(deadline)));
    }

    pub(crate) fn start_hop(&self) -> HopBudget {
        HopBudget {
            global: self.global,
            hop: self.per_call.map(|limit| self.clock.monotonic() + limit),
            clock: self.clock.clone(),
        }
    }

    pub(crate) fn is_expired(&self) -> bool {
        self.global.is_some_and(|end| self.clock.monotonic() >= end)
    }
}

// https://fetch.spec.whatwg.org/#concept-http-network-fetch
pub struct WireRequest {
    pub method: Method,
    pub url: Url,
    pub headers: HeaderMap,
    pub body: Option<Bytes>,
    // https://fetch.spec.whatwg.org/#concept-request-client
    pub client: Option<u64>,
    // https://wicg.github.io/private-network-access/#ip-address-space
    pub initiator: Option<url::Origin>,
    pub initiator_kind: cookies::InitiatorKind,
}

impl std::fmt::Debug for WireRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WireRequest")
            .field("method", &self.method)
            .field("url", &self.url)
            .field("headers", &self.headers)
            .field("has_body", &self.body.is_some())
            .field("client", &self.client)
            .field("initiator", &self.initiator)
            .field("initiator_kind", &self.initiator_kind)
            .finish()
    }
}

pub type WireBody = http_body_util::combinators::UnsyncBoxBody<Bytes, NetError>;

#[must_use]
pub fn buffered_body(bytes: Bytes) -> WireBody {
    Full::new(bytes)
        .map_err(|never| match never {})
        .boxed_unsync()
}

pub struct WireResponse {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: WireBody,
    // https://www.rfc-editor.org/rfc/rfc9113#section-8.3.2
    pub http2: bool,
}

impl std::fmt::Debug for WireResponse {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WireResponse")
            .field("status", &self.status)
            .field("headers", &self.headers)
            .field("http2", &self.http2)
            .finish_non_exhaustive()
    }
}

// https://fetch.spec.whatwg.org/#concept-http-network-fetch
pub trait Transport: Send + Sync + 'static {
    fn exchange(
        &self,
        request: WireRequest,
        budget: HopBudget,
    ) -> Pin<Box<dyn Future<Output = Result<WireResponse, NetError>> + Send + '_>>;
}

#[derive(Clone, Debug)]
pub struct HopBudget {
    global: Option<Instant>,
    hop: Option<Instant>,
    clock: Clock,
}

impl HopBudget {
    #[must_use]
    pub fn deadline(&self) -> Option<Instant> {
        match (self.global, self.hop) {
            (Some(global), Some(hop)) => Some(global.min(hop)),
            (global, hop) => global.or(hop),
        }
    }

    #[cfg(feature = "native")]
    pub(crate) fn is_expired(&self) -> bool {
        self.remaining() == Some(Duration::ZERO)
    }

    #[must_use]
    pub fn remaining(&self) -> Option<Duration> {
        self.deadline()
            .map(|end| end.saturating_duration_since(self.clock.monotonic()))
    }

    #[must_use]
    pub fn timeout_kind(&self, fallback: TimeoutKind) -> TimeoutKind {
        let now = self.clock.monotonic();
        if self.global.is_some_and(|end| now >= end) {
            TimeoutKind::Global
        } else if self.hop.is_some_and(|end| now >= end) {
            TimeoutKind::PerCall
        } else {
            fallback
        }
    }
}
