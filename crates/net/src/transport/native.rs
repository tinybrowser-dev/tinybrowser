//! The native transport: hyper client, native-tls (OpenSSL) TLS, and system
//! DNS with `--resolve` rewrites ahead of it.
//!
//! The browser process owns one Tokio runtime. `net` never implements HTTP
//! framing; `hyper-util` supplies HTTP/1.1, HTTP/2, pooling, and the connector
//! stack, while this module enforces the hop's deadline and maps transport
//! failures. Every fetch rule stays in the fetch core above the port.

use std::convert::Infallible;
use std::fmt;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::str::FromStr;
use std::task::{Context, Poll};

use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Empty, Full};
use hyper::header::HeaderValue;
use hyper_tls::HttpsConnector;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::connect::dns::{GaiResolver, Name};
use hyper_util::client::legacy::connect::proxy::Tunnel;
use hyper_util::rt::TokioExecutor;
use percent_encoding::percent_decode_str;
use tower_service::Service;
use url::Url;

use super::{HopBudget, Transport, WireBody, WireRequest, WireResponse};
use crate::error::{NetError, TimeoutKind, TransportError};
use crate::protocol::{HeaderMap, basic_authorization};
use crate::resolve::{HostMap, ResolveFailure, Target};

/// Resolver that applies ordered `--resolve` rules before system DNS.
#[derive(Clone)]
pub(crate) struct HostResolver {
    host_map: HostMap,
    system: GaiResolver,
}

impl Service<Name> for HostResolver {
    type Response = std::vec::IntoIter<SocketAddr>;
    type Error = ResolveFailure;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, name: Name) -> Self::Future {
        match self.host_map.lookup(name.as_str()) {
            Some(Target::Addr(ip)) => Box::pin(async move {
                // The connector replaces port zero with the URI's port.
                Ok(vec![SocketAddr::new(ip.into(), 0)].into_iter())
            }),
            Some(Target::Fail) => Box::pin(async move { Err(ResolveFailure::Denied) }),
            None => {
                let mut system = self.system.clone();
                Box::pin(async move {
                    let addrs = Service::call(&mut system, name)
                        .await
                        .map_err(ResolveFailure::System)?;
                    Ok(addrs.collect::<Vec<_>>().into_iter())
                })
            }
        }
    }
}

type RequestBody = BoxBody<Bytes, Infallible>;

/// An HTTP CONNECT proxy parsed at the configuration boundary.
///
/// Debug output names the destination and whether credentials are present,
/// never the credentials themselves.
#[derive(Clone)]
pub struct Proxy {
    destination: hyper::Uri,
    auth: Option<HeaderValue>,
}

/// A proxy URI that is not an `http://` HTTP CONNECT authority with a host.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("proxy URI must be an http:// HTTP CONNECT authority")]
pub struct InvalidProxy;

/// A certificate file that is not a PEM-encoded certificate.
#[derive(Debug, thiserror::Error)]
#[error("invalid certificate authority: {0}")]
pub struct InvalidCa(#[source] native_tls::Error);

impl FromStr for Proxy {
    type Err = InvalidProxy;

    fn from_str(proxy: &str) -> Result<Self, Self::Err> {
        let url = Url::parse(proxy).map_err(|_| InvalidProxy)?;
        if url.scheme() != "http" {
            return Err(InvalidProxy);
        }
        if url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
            return Err(InvalidProxy);
        }
        let host = url.host_str().ok_or(InvalidProxy)?;
        if url.password().is_some() && url.username().is_empty() {
            return Err(InvalidProxy);
        }
        let port = url.port().unwrap_or(80);
        let destination = hyper::Uri::builder()
            .scheme("http")
            .authority(format!("{host}:{port}"))
            .path_and_query("/")
            .build()
            .map_err(|_| InvalidProxy)?;
        let auth = if url.username().is_empty() {
            None
        } else {
            let username = percent_decode_str(url.username())
                .decode_utf8()
                .map_err(|_| InvalidProxy)?;
            let password = url
                .password()
                .map(|password| {
                    percent_decode_str(password)
                        .decode_utf8()
                        .map_err(|_| InvalidProxy)
                })
                .transpose()?
                .unwrap_or_default();
            let mut value = HeaderValue::from_str(&basic_authorization(&username, &password))
                .map_err(|_| InvalidProxy)?;
            value.set_sensitive(true);
            Some(value)
        };
        Ok(Self { destination, auth })
    }
}

impl fmt::Debug for Proxy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Proxy")
            .field("destination", &self.destination)
            .field("has_auth", &self.auth.is_some())
            .finish()
    }
}

#[derive(Clone)]
pub(crate) struct HttpEngine {
    client: Client<HttpsConnector<HttpConnector<HostResolver>>, RequestBody>,
    proxied: Option<ProxiedClient>,
}

type ProxiedClient = Client<HttpsConnector<Tunnel<HttpConnector<HostResolver>>>, RequestBody>;

impl fmt::Debug for HttpEngine {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpEngine")
            .field("has_proxy", &self.proxied.is_some())
            .finish_non_exhaustive()
    }
}

/// A PEM certificate authority, already parsed for the TLS connector.
///
/// The configuration boundary builds these, so a malformed file is a usage
/// error instead of a dial-time TLS failure.
#[derive(Clone)]
pub struct TrustedCa(native_tls::Certificate);

impl TrustedCa {
    /// Parses one PEM certificate.
    ///
    /// # Errors
    ///
    /// [`InvalidCa`] when `pem` is not a PEM-encoded certificate.
    pub fn from_pem(pem: &[u8]) -> Result<Self, InvalidCa> {
        native_tls::Certificate::from_pem(pem)
            .map(Self)
            .map_err(InvalidCa)
    }
}

/// `native-tls` connector trusting `cas`.
///
/// ALPN is configured here, not by `hyper-tls`: its `alpn` feature only reports
/// the protocol the server picked. OpenSSL initialization failure is
/// unrecoverable for TLS in this process, and `hyper-tls`'s own constructors
/// panic on it for the same reason.
fn tls_connector(cas: &[TrustedCa]) -> tokio_native_tls::TlsConnector {
    let mut tls = native_tls::TlsConnector::builder();
    for ca in cas {
        tls.add_root_certificate(ca.0.clone());
    }
    let tls = tls
        .request_alpns(&["h2", "http/1.1"])
        .build()
        .expect("openssl tls connector");
    tokio_native_tls::TlsConnector::from(tls)
}

impl HttpEngine {
    pub(crate) fn new(proxy: Option<Proxy>, host_map: HostMap, tls_cas: &[TrustedCa]) -> Self {
        let resolver = HostResolver {
            host_map,
            system: GaiResolver::new(),
        };
        // `HttpConnector` resolves through `HostResolver` and applies Happy
        // Eyeballs (300 ms default) when a name returns multiple addresses.
        let mut http = HttpConnector::new_with_resolver(resolver);
        http.enforce_http(false);
        let tls = tls_connector(tls_cas);
        let proxied = proxy.map(|proxy| {
            let mut tunnel = Tunnel::new(proxy.destination.clone(), http.clone());
            if let Some(auth) = &proxy.auth {
                tunnel = tunnel.with_auth(auth.clone());
            }
            let connector = HttpsConnector::from((tunnel, tls.clone()));
            Client::builder(TokioExecutor::new()).build(connector)
        });
        let connector = HttpsConnector::from((http, tls));
        let client = Client::builder(TokioExecutor::new()).build(connector);
        Self { client, proxied }
    }
}

impl Transport for HttpEngine {
    /// Sends one hop over hyper. Redirects, cookies, and header shaping belong
    /// to the fetch core; this port applies the deadline and maps transport
    /// failures.
    ///
    /// # Errors
    ///
    /// [`NetError::Transport`] for DNS, connect, TLS, timeout, or I/O failure.
    /// [`NetError::RejectedRequest`] when the request cannot be represented.
    fn exchange(
        &self,
        request: WireRequest,
        budget: HopBudget,
    ) -> Pin<Box<dyn Future<Output = Result<WireResponse, NetError>> + Send + '_>> {
        Box::pin(async move {
            let WireRequest {
                method,
                url: wire_url,
                headers,
                body,
            } = request;
            if budget.is_expired() {
                return Err(timed_out(budget.timeout_kind(TimeoutKind::Global)));
            }
            let request_body = match body {
                Some(bytes) => Full::new(bytes).boxed(),
                None => Empty::<Bytes>::new().boxed(),
            };
            let mut request = hyper::Request::builder()
                .method(method.as_http().clone())
                .uri(
                    wire_url
                        .as_str()
                        .parse::<hyper::Uri>()
                        .map_err(|_| NetError::RejectedRequest)?,
                )
                .body(request_body)
                .map_err(|_| NetError::RejectedRequest)?;
            // Browsers state the HTTP/1.1 default explicitly. hyper leaves the
            // field to the caller, and ALPN can only select HTTP/2 over TLS, so a
            // cleartext URL is always the HTTP/1.1 connection this field belongs
            // on (<https://www.rfc-editor.org/rfc/rfc9110#section-7.6.1>).
            if wire_url.scheme() == "http" && headers.value("connection").is_none() {
                request.headers_mut().append(
                    hyper::header::CONNECTION,
                    hyper::header::HeaderValue::from_static("keep-alive"),
                );
            }
            for (name, value) in headers.typed_iter() {
                request.headers_mut().append(name.clone(), value.clone());
            }

            let host = wire_url.host_str().unwrap_or_default().to_owned();
            let request_future = match &self.proxied {
                Some(proxied) => proxied.request(request),
                None => self.client.request(request),
            };
            let response = match within(budget.clone(), TimeoutKind::Global, request_future).await {
                Ok(Ok(response)) => response,
                Ok(Err(error)) => return Err(map_client_error(error, &host)),
                Err(error) => return Err(error),
            };
            // HTTP/2 has no reason phrase; `StatusCode::canonical_reason` would
            // invent one (<https://www.rfc-editor.org/rfc/rfc9113#section-8.3.2>).
            let http2 = response.version() == hyper::Version::HTTP_2;
            let status = response.status().as_u16();
            let mut mapped = HeaderMap::new();
            for (name, value) in response.headers() {
                mapped.insert_wire(name.clone(), value.clone());
            }
            // The port's body is a `http_body` stream so the fetch core decodes
            // it uniformly, whether the bytes come from a socket or from an
            // embedder's in-memory origin.
            let body = response.into_body().map_err(|error| {
                NetError::Transport(TransportError::Io(std::io::Error::other(error)))
            });
            let body = BodyDeadline {
                inner: body.boxed_unsync(),
                budget,
                timer: None,
            }
            .boxed_unsync();
            Ok(WireResponse {
                status,
                headers: mapped,
                body,
                http2,
            })
        })
    }
}

/// The hop deadline applied to a response body.
///
/// A body is read after [`Transport::exchange`] returns, and only the transport
/// can interrupt a read that has not produced a byte yet. Expiry surfaces as
/// [`TransportError::Timeout`] carrying the kind that ran out, because the fetch
/// core reports a stalled body and a stalled socket the same way
/// (<https://fetch.spec.whatwg.org/#concept-http-network-fetch>).
struct BodyDeadline {
    inner: WireBody,
    budget: HopBudget,
    timer: Option<Pin<Box<tokio::time::Sleep>>>,
}

impl http_body::Body for BodyDeadline {
    type Data = Bytes;
    type Error = NetError;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Bytes>, NetError>>> {
        let this = self.get_mut();
        if let Some(deadline) = this.budget.deadline() {
            let timer = this
                .timer
                .get_or_insert_with(|| Box::pin(tokio::time::sleep_until(deadline.into())));
            if timer.as_mut().poll(cx).is_ready() {
                return Poll::Ready(Some(Err(timed_out(
                    this.budget.timeout_kind(TimeoutKind::RecvBody),
                ))));
            }
        }
        Pin::new(&mut this.inner).poll_frame(cx)
    }
}

/// Awaits `future` under the hop deadline. `fallback` names the phase when
/// neither the global nor the per-call deadline is the one that expired.
pub(crate) async fn within<T>(
    budget: HopBudget,
    fallback: TimeoutKind,
    future: impl Future<Output = T>,
) -> Result<T, NetError> {
    match budget.deadline() {
        Some(deadline) => {
            match tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), future).await {
                Ok(value) => Ok(value),
                Err(_) => Err(timed_out(budget.timeout_kind(fallback))),
            }
        }
        None => Ok(future.await),
    }
}

fn timed_out(kind: TimeoutKind) -> NetError {
    NetError::Transport(TransportError::Timeout(kind))
}

fn map_client_error(error: hyper_util::client::legacy::Error, host: &str) -> NetError {
    if error.is_connect() {
        let mut source = std::error::Error::source(&error);
        let mut tls_reason: Option<Box<str>> = None;
        while let Some(cause) = source {
            // The resolver phase fails before any socket exists: `hyper-util`
            // wraps `HostResolver`'s error in its connect error, and the typed
            // cause survives the wrapping.
            if cause.downcast_ref::<ResolveFailure>().is_some() {
                return NetError::Transport(TransportError::Dns(host.into()));
            }
            if cause.downcast_ref::<native_tls::Error>().is_some() && tls_reason.is_none() {
                tls_reason = Some(cause.to_string().into());
            }
            if let Some(io) = cause.downcast_ref::<std::io::Error>() {
                if io.kind() == std::io::ErrorKind::NotFound {
                    return NetError::Transport(TransportError::Dns(host.into()));
                }
                // A connect-phase io error that wraps another error is a
                // protocol failure (TLS handshake, most often); plain OS
                // errors carry no inner error.
                let wrapped = io.get_ref().is_some();
                if wrapped && tls_reason.is_none() {
                    tls_reason = Some(io.to_string().into());
                }
            }
            let text = cause.to_string();
            let lowered = text.to_ascii_lowercase();
            if tls_reason.is_none()
                && (lowered.contains("tls")
                    || lowered.contains("certificate")
                    || lowered.contains("handshake"))
            {
                tls_reason = Some(text.into());
            }
            source = cause.source();
        }
        if let Some(reason) = tls_reason {
            return NetError::Transport(TransportError::Tls(reason));
        }
        return NetError::Transport(TransportError::Connect(error.to_string().into()));
    }
    NetError::Transport(TransportError::Io(std::io::Error::other(error)))
}
