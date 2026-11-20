#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TimeoutKind {
    #[error("global")]
    Global,
    #[error("per-call")]
    PerCall,
    #[error("recv-body")]
    RecvBody,
}

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("dns lookup failed for {0}")]
    Dns(Box<str>),
    #[error("connection failed: {0}")]
    Connect(Box<str>),
    #[error("tls failure: {0}")]
    Tls(Box<str>),
    #[error("{0} timeout exceeded")]
    Timeout(TimeoutKind),
    #[error("transport at capacity")]
    Capacity,
    #[error("response body exceeds the hop's cap")]
    BodyTooLarge,
    #[error("io error: {0}")]
    Io(#[source] std::io::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum NetError {
    #[error("transport: {0}")]
    Transport(#[source] TransportError),
    #[error("protocol violation: backend rejected the assembled request")]
    RejectedRequest,
    #[error("redirect cap exceeded")]
    RedirectLimit,
    #[error("content decoding failed: {0}")]
    Decode(Box<str>),
}
