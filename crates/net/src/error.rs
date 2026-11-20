/// Which timeout budget was exceeded.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TimeoutKind {
    #[error("global")]
    Global,
    #[error("per-call")]
    PerCall,
    #[error("recv-body")]
    RecvBody,
}

/// Failure to dial, complete TLS, or transfer bytes.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    /// DNS lookup failed for this host.
    #[error("dns lookup failed for {0}")]
    Dns(Box<str>),
    /// TCP or proxy CONNECT failed. The string is a short reason, such as `CONNECT 403`.
    #[error("connection failed: {0}")]
    Connect(Box<str>),
    /// TLS handshake or certificate verification failed.
    #[error("tls failure: {0}")]
    Tls(Box<str>),
    #[error("{0} timeout exceeded")]
    Timeout(TimeoutKind),
    /// The exchange was refused because the transport is at capacity. A host
    /// with its own admission control reports this instead of stalling.
    #[error("transport at capacity")]
    Capacity,
    /// The response body was larger than the cap the hop carried
    /// ([`crate::WireRequest`] has no such field: the cap travels on the
    /// exchange the caller framed, as `max-body-bytes` on the host contract).
    #[error("response body exceeds the hop's cap")]
    BodyTooLarge,
    #[error("io error: {0}")]
    Io(#[source] std::io::Error),
}

/// Failure from [`crate::Agent::send`] or body reads.
#[derive(Debug, thiserror::Error)]
pub enum NetError {
    #[error("transport: {0}")]
    Transport(#[source] TransportError),
    /// The request could not be represented as HTTP, or a CORS or redirect
    /// rule refused it, so no response reaches the caller.
    #[error("protocol violation: backend rejected the assembled request")]
    RejectedRequest,
    /// The redirect hop count reached
    /// [`crate::AgentOptions::max_redirects`](crate::AgentOptions).
    #[error("redirect cap exceeded")]
    RedirectLimit,
    #[error("content decoding failed: {0}")]
    Decode(Box<str>),
}
