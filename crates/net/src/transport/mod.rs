//! The byte-mover port.
//!
//! [`Transport`] is the one seam between the fetch core and whatever performs
//! an HTTP exchange. The core shapes the request — redirects, cookies, CORS,
//! `Sec-Fetch-*`, and referrers are all decided above this module — and hands
//! the transport one [`WireRequest`] per hop; the transport returns one
//! [`WireResponse`] and applies nothing else
//! (<https://fetch.spec.whatwg.org/#concept-http-network-fetch>).
//!
//! The native hyper implementation lives in [`native`] behind the `native`
//! feature. An embedder without sockets (the wasm component, an in-memory
//! conformance origin) implements this port instead and runs the same fetch
//! algorithm.

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

/// The deadlines for one `send` call: the global limit covering every
/// redirect hop plus the per-hop limit captured once at call start.
#[derive(Clone, Debug)]
pub(crate) struct CallBudget {
    global: Option<Instant>,
    per_call: Option<Duration>,
    clock: Clock,
}

impl CallBudget {
    /// Starts the budget for one fetch call from the agent's timeouts.
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

    /// A request deadline may only tighten the global timeout, never stretch
    /// it past what the engine configured.
    pub(crate) fn tighten_global(&mut self, deadline: Instant) {
        self.global = Some(self.global.map_or(deadline, |global| global.min(deadline)));
    }

    /// Mints the budget for one hop: the global deadline plus the per-call
    /// deadline measured from now.
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

/// One HTTP exchange handed to a [`Transport`]: the request the fetch core has
/// already shaped, with no policy left for the transport to apply.
///
/// Redirects, cookies, CORS, `Sec-Fetch-*`, and referrers are decided above the
/// port; a transport only moves bytes for one URL
/// (<https://fetch.spec.whatwg.org/#concept-http-network-fetch>).
pub struct WireRequest {
    /// Request method.
    pub method: Method,
    /// Absolute URL. The fragment is already stripped.
    pub url: Url,
    /// Headers in wire order, with every policy header already applied.
    pub headers: HeaderMap,
    /// Body bytes, empty for `GET` and `HEAD`.
    pub body: Option<Bytes>,
}

impl std::fmt::Debug for WireRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WireRequest")
            .field("method", &self.method)
            .field("url", &self.url)
            .field("headers", &self.headers)
            .field("has_body", &self.body.is_some())
            .finish()
    }
}

/// The body a transport returns. It is a `http_body` stream so the fetch core
/// can apply content decoders and the caller can read it chunk by chunk.
pub type WireBody = http_body_util::combinators::UnsyncBoxBody<Bytes, NetError>;

/// The body of an exchange whose bytes are already in memory.
///
/// A host that cannot stream a body — a browser `fetch` that buffered it, an
/// in-memory conformance origin — answers with this instead of a stream. The
/// fetch core decodes it the same way, so only the timing differs.
#[must_use]
pub fn buffered_body(bytes: Bytes) -> WireBody {
    Full::new(bytes)
        .map_err(|never| match never {})
        .boxed_unsync()
}

/// One HTTP exchange's answer: status, headers, and the body stream.
pub struct WireResponse {
    /// Status code.
    pub status: u16,
    /// Response headers, including `Set-Cookie`, before filtering.
    pub headers: HeaderMap,
    /// The body stream.
    pub body: WireBody,
    /// Whether the exchange ran over HTTP/2, which has no reason phrase
    /// (<https://www.rfc-editor.org/rfc/rfc9113#section-8.3.2>).
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

/// The one seam between fetch policy and a byte mover.
///
/// The fetch core calls this once per hop with a [`WireRequest`] and a
/// [`HopBudget`]; the implementation performs one HTTP exchange and nothing
/// else. Hyper, an embedder's own client, and an in-memory test origin all
/// implement the same port, so every host runs the same fetch algorithm
/// (<https://fetch.spec.whatwg.org/#concept-http-network-fetch>).
pub trait Transport: Send + Sync + 'static {
    /// Performs one exchange. The future is boxed so the port stays
    /// object-safe without an async-trait dependency.
    fn exchange(
        &self,
        request: WireRequest,
        budget: HopBudget,
    ) -> Pin<Box<dyn Future<Output = Result<WireResponse, NetError>> + Send + '_>>;
}

/// The deadlines for one hop of a `send` call: the call's global deadline
/// plus the hop's own `timeout_per_call` deadline.
#[derive(Clone, Debug)]
pub struct HopBudget {
    global: Option<Instant>,
    hop: Option<Instant>,
    clock: Clock,
}

impl HopBudget {
    /// The tighter of the call's global deadline and this hop's own deadline.
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

    /// Time left before [`Self::deadline`], or `None` when the hop has no
    /// deadline.
    ///
    /// A transport that can abort in flight (a host `fetch` with an
    /// `AbortSignal`, a socket timeout) arms it with this; the fetch core
    /// re-checks the budget when the hop returns, so a transport that ignores
    /// the deadline is late, not wrong.
    #[must_use]
    pub fn remaining(&self) -> Option<Duration> {
        self.deadline()
            .map(|end| end.saturating_duration_since(self.clock.monotonic()))
    }

    /// The deadline a timeout should be blamed on.
    ///
    /// A transport that aborts a hop in flight reports the timeout with this
    /// kind, so a global budget that ran out is not recorded as a per-call one.
    /// `fallback` names the phase when neither deadline is the expired one.
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
