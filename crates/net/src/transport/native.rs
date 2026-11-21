use std::collections::HashMap;
use std::convert::Infallible;
use std::fmt;
use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::pin::Pin;
use std::str::FromStr;
use std::sync::{Arc, Mutex, PoisonError};
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

// https://wicg.github.io/private-network-access/#ip-address-space
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum AddressSpace {
    Local,
    Private,
    Public,
}

const MAX_HOST_SPACES: usize = 4096;

fn unwrap_embedded_ipv4(ip: IpAddr) -> IpAddr {
    let IpAddr::V6(v6) = ip else {
        return ip;
    };
    if let Some(v4) = v6.to_ipv4_mapped() {
        return IpAddr::V4(v4);
    }
    let s = v6.segments();
    // https://www.rfc-editor.org/rfc/rfc6052#section-2.1
    if s[0] == 0x0064 && s[1] == 0xff9b && s[2] == 0 && s[3] == 0 && s[4] == 0 && s[5] == 0 {
        return IpAddr::V4(Ipv4Addr::from((u32::from(s[6]) << 16) | u32::from(s[7])));
    }
    // https://www.rfc-editor.org/rfc/rfc3056#section-2
    if s[0] == 0x2002 {
        return IpAddr::V4(Ipv4Addr::from((u32::from(s[1]) << 16) | u32::from(s[2])));
    }
    // https://www.rfc-editor.org/rfc/rfc4380#section-4
    if s[0] == 0x2001 && s[1] == 0x0000 {
        let inverted = ((u32::from(s[6]) << 16) | u32::from(s[7])) ^ 0xffff_ffff;
        return IpAddr::V4(Ipv4Addr::from(inverted));
    }
    IpAddr::V6(v6)
}

// https://wicg.github.io/private-network-access/#ip-address-space
pub(crate) fn classify_ip(ip: IpAddr) -> AddressSpace {
    match unwrap_embedded_ipv4(ip) {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            if v4.is_loopback() || octets[0] == 0 {
                AddressSpace::Local
            } else if v4.is_private()
                || v4.is_link_local()
                || (octets[0] == 100 && (octets[1] & 0xc0) == 64)
                || (octets[0] == 192 && octets[1] == 0 && octets[2] == 0)
                || (octets[0] == 198 && (octets[1] & 0xfe) == 18)
            {
                AddressSpace::Private
            } else {
                AddressSpace::Public
            }
        }
        IpAddr::V6(v6) => {
            let s = v6.segments();
            if v6.is_loopback() || v6.is_unspecified() {
                AddressSpace::Local
            } else if (s[0] & 0xfe00) == 0xfc00
                || (s[0] & 0xffc0) == 0xfe80
                || (s[0] == 0x2001 && s[1] == 0x0db8)
                || (s[0] == 0x0064 && s[1] == 0xff9b && s[2] == 0x0001)
            {
                AddressSpace::Private
            } else {
                AddressSpace::Public
            }
        }
    }
}

pub(crate) fn is_cloud_metadata(ip: IpAddr) -> bool {
    const IMDS_V4: Ipv4Addr = Ipv4Addr::new(169, 254, 169, 254);
    const ECS_V4: Ipv4Addr = Ipv4Addr::new(169, 254, 170, 2);
    const ALIBABA_V4: Ipv4Addr = Ipv4Addr::new(100, 100, 100, 200);
    const ORACLE_V4: Ipv4Addr = Ipv4Addr::new(192, 0, 0, 192);
    const IMDS_V6: Ipv6Addr = Ipv6Addr::new(0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0254);
    matches!(
        unwrap_embedded_ipv4(ip),
        IpAddr::V4(IMDS_V4 | ECS_V4 | ALIBABA_V4 | ORACLE_V4) | IpAddr::V6(IMDS_V6)
    )
}

fn normalize_host(host: &str) -> String {
    host.trim_end_matches('.').to_ascii_lowercase()
}

#[derive(Clone)]
pub(crate) struct HostResolver {
    host_map: HostMap,
    system: GaiResolver,
    pinned: Arc<Mutex<HashMap<String, Vec<SocketAddr>>>>,
}

impl Service<Name> for HostResolver {
    type Response = std::vec::IntoIter<SocketAddr>;
    type Error = ResolveFailure;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, name: Name) -> Self::Future {
        let key = normalize_host(name.as_str());
        if let Some(addrs) = self
            .pinned
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&key)
        {
            return Box::pin(async move { Ok(addrs.into_iter()) });
        }
        match self.host_map.lookup(name.as_str()) {
            Some(Target::Addr(ip)) => {
                Box::pin(async move { Ok(vec![SocketAddr::new(ip.into(), 0)].into_iter()) })
            }
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

#[derive(Clone)]
pub struct Proxy {
    destination: hyper::Uri,
    auth: Option<HeaderValue>,
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("proxy URI must be an http:// HTTP CONNECT authority")]
pub struct InvalidProxy;

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
    host_map: HostMap,
    system: GaiResolver,
    pinned: Arc<Mutex<HashMap<String, Vec<SocketAddr>>>>,
    host_spaces: Arc<Mutex<HashMap<String, AddressSpace>>>,
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

#[derive(Clone)]
pub struct TrustedCa(native_tls::Certificate);

impl TrustedCa {
    pub fn from_pem(pem: &[u8]) -> Result<Self, InvalidCa> {
        native_tls::Certificate::from_pem(pem)
            .map(Self)
            .map_err(InvalidCa)
    }
}

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
        let system = GaiResolver::new();
        let pinned = Arc::new(Mutex::new(HashMap::new()));
        let resolver = HostResolver {
            host_map: host_map.clone(),
            system: system.clone(),
            pinned: Arc::clone(&pinned),
        };
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
        Self {
            client,
            proxied,
            host_map,
            system,
            pinned,
            host_spaces: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn initiator_space(&self, initiator: Option<&url::Origin>) -> Option<AddressSpace> {
        let origin = initiator?;
        match origin {
            url::Origin::Opaque(_) => Some(AddressSpace::Public),
            url::Origin::Tuple(_, host, _) => Some(match host {
                url::Host::Ipv4(v4) => classify_ip(IpAddr::V4(*v4)),
                url::Host::Ipv6(v6) => classify_ip(IpAddr::V6(*v6)),
                url::Host::Domain(domain) => {
                    let lower = normalize_host(domain);
                    if lower == "localhost" || lower.ends_with(".localhost") {
                        AddressSpace::Local
                    } else if let Some(Target::Addr(ip)) = self.host_map.lookup(&lower) {
                        classify_ip(ip.into())
                    } else {
                        self.host_spaces
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .get(&lower)
                            .copied()
                            .unwrap_or(AddressSpace::Public)
                    }
                }
            }),
        }
    }

    // https://wicg.github.io/private-network-access/#integration-with-fetch
    async fn enforce_private_network_access(
        &self,
        host: &str,
        initiator: Option<&url::Origin>,
        initiator_kind: cookies::InitiatorKind,
        budget: &HopBudget,
    ) -> Result<(), NetError> {
        if self.proxied.is_some() || host.is_empty() || self.host_map.lookup(host).is_some() {
            return Ok(());
        }
        let lower = normalize_host(host);
        let literal = lower
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
            .unwrap_or(&lower)
            .parse::<IpAddr>()
            .ok();
        let resolved: Vec<SocketAddr> = if let Some(ip) = literal {
            vec![SocketAddr::new(ip, 0)]
        } else {
            let Ok(name) = Name::from_str(&lower) else {
                return Ok(());
            };
            let mut system = self.system.clone();
            let addrs = within(
                budget.clone(),
                TimeoutKind::Global,
                Service::call(&mut system, name),
            )
            .await?
            .map_err(|_| NetError::Transport(TransportError::Dns(host.into())))?;
            addrs.collect()
        };
        if resolved.is_empty() {
            return Ok(());
        }
        if resolved.iter().any(|addr| is_cloud_metadata(addr.ip())) {
            return Err(NetError::RejectedRequest);
        }
        let target_space = resolved
            .iter()
            .map(|addr| classify_ip(addr.ip()))
            .min()
            .unwrap_or(AddressSpace::Public);
        let initiator_space = self.initiator_space(initiator);
        if literal.is_none() {
            let mut spaces = self
                .host_spaces
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if let Some(&previous) = spaces.get(&lower) {
                if target_space < previous {
                    return Err(NetError::RejectedRequest);
                }
            } else if spaces.len() >= MAX_HOST_SPACES {
                return Err(NetError::RejectedRequest);
            } else {
                spaces.insert(lower.clone(), target_space);
            }
        }
        if initiator_kind != cookies::InitiatorKind::Navigation
            && let Some(initiator_space) = initiator_space
            && target_space < initiator_space
        {
            return Err(NetError::RejectedRequest);
        }
        if literal.is_none() {
            let mut pinned = self.pinned.lock().unwrap_or_else(PoisonError::into_inner);
            if !pinned.contains_key(&lower) && pinned.len() >= MAX_HOST_SPACES {
                return Err(NetError::RejectedRequest);
            }
            pinned.insert(lower, resolved);
        }
        Ok(())
    }
}

impl Transport for HttpEngine {
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
                client: _,
                initiator,
                initiator_kind,
            } = request;
            if budget.is_expired() {
                return Err(timed_out(budget.timeout_kind(TimeoutKind::Global)));
            }
            let host = wire_url.host_str().unwrap_or_default().to_owned();
            self.enforce_private_network_access(&host, initiator.as_ref(), initiator_kind, &budget)
                .await?;
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
            // https://www.rfc-editor.org/rfc/rfc9110#section-7.6.1
            if wire_url.scheme() == "http" && headers.value("connection").is_none() {
                request.headers_mut().append(
                    hyper::header::CONNECTION,
                    hyper::header::HeaderValue::from_static("keep-alive"),
                );
            }
            for (name, value) in headers.typed_iter() {
                request.headers_mut().append(name.clone(), value.clone());
            }

            let request_future = match &self.proxied {
                Some(proxied) => proxied.request(request),
                None => self.client.request(request),
            };
            let response = match within(budget.clone(), TimeoutKind::Global, request_future).await {
                Ok(Ok(response)) => response,
                Ok(Err(error)) => return Err(map_client_error(error, &host)),
                Err(error) => return Err(error),
            };
            // https://www.rfc-editor.org/rfc/rfc9113#section-8.3.2
            let http2 = response.version() == hyper::Version::HTTP_2;
            let status = response.status().as_u16();
            let mut mapped = HeaderMap::new();
            for (name, value) in response.headers() {
                mapped.insert_wire(name.clone(), value.clone());
            }
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

// https://fetch.spec.whatwg.org/#concept-http-network-fetch
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

#[cfg(test)]
mod tests {
    use super::{AddressSpace, classify_ip, is_cloud_metadata};
    use std::net::IpAddr;

    #[test]
    fn ip_address_spaces_and_metadata_are_classified() {
        for (addr, expected) in [
            ("127.0.0.1", AddressSpace::Local),
            ("::1", AddressSpace::Local),
            ("::ffff:127.0.0.1", AddressSpace::Local),
            ("0.0.0.0", AddressSpace::Local),
            ("10.0.0.1", AddressSpace::Private),
            ("172.16.0.1", AddressSpace::Private),
            ("192.168.1.1", AddressSpace::Private),
            ("100.64.0.1", AddressSpace::Private),
            ("192.0.0.1", AddressSpace::Private),
            ("198.18.0.1", AddressSpace::Private),
            ("169.254.169.254", AddressSpace::Private),
            ("::ffff:169.254.169.254", AddressSpace::Private),
            ("64:ff9b::127.0.0.1", AddressSpace::Local),
            ("64:ff9b::10.0.0.1", AddressSpace::Private),
            ("64:ff9b:1::1", AddressSpace::Private),
            ("2001:db8::1", AddressSpace::Private),
            ("2002:7f00:0001::1", AddressSpace::Local),
            (
                "2001:0000:4136:e378:8000:63bf:3fff:fdd2",
                AddressSpace::Public,
            ),
            (
                "2001:0000:4136:e378:8000:63bf:80ff:fffe",
                AddressSpace::Local,
            ),
            ("fc00::1", AddressSpace::Private),
            ("fe80::1", AddressSpace::Private),
            ("93.184.216.34", AddressSpace::Public),
            ("2606:2800:220:1::248", AddressSpace::Public),
        ] {
            let ip: IpAddr = addr.parse().expect("ip");
            assert_eq!(classify_ip(ip), expected, "{addr}");
        }
        assert!(is_cloud_metadata("169.254.169.254".parse().expect("ip")));
        assert!(is_cloud_metadata(
            "::ffff:169.254.169.254".parse().expect("ip")
        ));
        assert!(is_cloud_metadata(
            "64:ff9b::169.254.169.254".parse().expect("ip")
        ));
        assert!(is_cloud_metadata("100.100.100.200".parse().expect("ip")));
        assert!(is_cloud_metadata("192.0.0.192".parse().expect("ip")));
        assert!(is_cloud_metadata("fd00:ec2::254".parse().expect("ip")));
        assert!(!is_cloud_metadata("127.0.0.1".parse().expect("ip")));
    }
}
