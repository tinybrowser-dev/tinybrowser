use std::convert::Infallible;
use std::error::Error;
use std::future::Future;
use std::io;
use std::net::TcpListener;
use std::time::Duration;

const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

use bytes::Bytes;
use http::{HeaderValue, Request as HttpRequest, Response as HttpResponse, StatusCode, header};
use http_body_util::{BodyExt as _, Full, LengthLimitError, Limited};
use hyper::body::{Body, Incoming};
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder;
use serde_json::Value;
use tokio::sync::watch;
use tokio::task::JoinSet;

pub type Request = HttpRequest<Incoming>;

pub type Response = HttpResponse<Full<Bytes>>;

#[derive(Clone)]
pub struct Shutdown {
    tx: watch::Sender<bool>,
    rx: watch::Receiver<bool>,
}

impl Shutdown {
    #[must_use]
    pub fn new() -> Self {
        let (tx, rx) = watch::channel(false);
        Self { tx, rx }
    }

    pub fn request(&self) {
        let _ = self.tx.send(true);
    }

    #[must_use]
    pub fn requested(&self) -> bool {
        *self.rx.borrow()
    }

    fn subscribe(&self) -> watch::Receiver<bool> {
        self.tx.subscribe()
    }
}

impl Default for Shutdown {
    fn default() -> Self {
        Self::new()
    }
}

pub async fn serve<S, H, F>(
    listener: &TcpListener,
    shutdown: Shutdown,
    state: S,
    handler: H,
) -> io::Result<()>
where
    S: Clone + Send + Sync + 'static,
    H: Fn(Request, S) -> F + Clone + Send + Sync + 'static,
    F: Future<Output = Response> + Send + 'static,
{
    let std_listener = listener.try_clone()?;
    std_listener.set_nonblocking(true)?;
    let listener = tokio::net::TcpListener::from_std(std_listener)?;
    let mut stopping = shutdown.subscribe();
    if shutdown.requested() {
        return Ok(());
    }
    let mut tasks = JoinSet::new();
    loop {
        while tasks.try_join_next().is_some() {}
        tokio::select! {
            _ = stopping.changed() => break,
            accepted = listener.accept() => {
                let (stream, _) = match accepted {
                    Ok(accepted) => accepted,
                    Err(_error) => {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        continue;
                    }
                };
                tasks.spawn(connection(
                    stream,
                    state.clone(),
                    handler.clone(),
                    shutdown.subscribe(),
                ));
            }
        }
    }
    if tokio::time::timeout(DRAIN_TIMEOUT, async {
        while tasks.join_next().await.is_some() {}
    })
    .await
    .is_err()
    {
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
    }
    Ok(())
}

async fn connection<S, H, F>(
    stream: tokio::net::TcpStream,
    state: S,
    handler: H,
    mut stop: watch::Receiver<bool>,
) where
    S: Clone + Send + Sync + 'static,
    H: Fn(Request, S) -> F + Clone + Send + Sync + 'static,
    F: Future<Output = Response> + Send + 'static,
{
    let service = service_fn(move |request| {
        let state = state.clone();
        let handler = handler.clone();
        async move { Ok::<_, Infallible>(handler(request, state).await) }
    });
    let mut auto = Builder::new(TokioExecutor::new());
    auto.http2().enable_connect_protocol();
    let conn = auto.serve_connection_with_upgrades(TokioIo::new(stream), service);
    tokio::pin!(conn);
    if !*stop.borrow() {
        tokio::select! {
            _result = conn.as_mut() => return,
            _ = stop.changed() => {}
        }
    }
    conn.as_mut().graceful_shutdown();
    let _drained = conn.as_mut().await;
}

#[must_use]
pub fn is_loopback_host(request: &Request) -> bool {
    let authority = request
        .headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .or_else(|| request.uri().authority().map(http::uri::Authority::as_str));
    authority.is_some_and(is_loopback_authority)
}

fn is_loopback_authority(authority: &str) -> bool {
    if authority.is_empty() {
        return false;
    }
    if let Some(rest) = authority.strip_prefix('[') {
        let Some((host, tail)) = rest.split_once(']') else {
            return false;
        };
        if !valid_optional_port(tail) {
            return false;
        }
        return host
            .parse::<std::net::Ipv6Addr>()
            .is_ok_and(|addr| addr.is_loopback());
    }
    let (host, tail) = match authority.split_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (authority, None),
    };
    if let Some(port) = tail
        && (port.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return false;
    }
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::Ipv4Addr>()
            .is_ok_and(|addr| addr.is_loopback())
}

fn valid_optional_port(tail: &str) -> bool {
    if tail.is_empty() {
        return true;
    }
    tail.strip_prefix(':')
        .is_some_and(|port| !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit()))
}

#[must_use]
pub fn has_origin(request: &Request) -> bool {
    request.headers().contains_key(header::ORIGIN)
}

// https://w3c.github.io/webappsec-fetch-metadata/
#[must_use]
pub fn has_browser_fetch_metadata(request: &Request) -> bool {
    const HEADERS: [&str; 4] = [
        "sec-fetch-site",
        "sec-fetch-mode",
        "sec-fetch-dest",
        "sec-fetch-user",
    ];
    HEADERS
        .iter()
        .any(|name| request.headers().contains_key(*name))
}

#[must_use]
pub fn is_json_content_type(request: &Request) -> bool {
    request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|media| media.trim().eq_ignore_ascii_case("application/json"))
}

#[must_use]
pub fn json(status: StatusCode, payload: &Value) -> Response {
    let mut response = Response::new(Full::new(Bytes::from(payload.to_string())));
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    response
}

#[must_use]
pub fn text(status: StatusCode, message: &str) -> Response {
    let mut response = Response::new(Full::new(Bytes::from(message.to_owned())));
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
}

#[must_use]
pub fn method_not_allowed(allow: &'static str) -> Response {
    let mut response = Response::new(Full::new(Bytes::new()));
    *response.status_mut() = StatusCode::METHOD_NOT_ALLOWED;
    response
        .headers_mut()
        .insert(header::ALLOW, HeaderValue::from_static(allow));
    response
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BodyError {
    TooLarge,
    Read,
}

pub async fn read_body<B>(body: B, limit: usize) -> Result<Bytes, BodyError>
where
    B: Body<Data = Bytes>,
    B::Error: Into<Box<dyn Error + Send + Sync>>,
{
    match Limited::new(body, limit).collect().await {
        Ok(collected) => Ok(collected.to_bytes()),
        Err(error) => {
            if error.downcast_ref::<LengthLimitError>().is_some() {
                Err(BodyError::TooLarge)
            } else {
                Err(BodyError::Read)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;

    use bytes::Bytes;
    use http::{StatusCode, header};
    use http_body_util::Full;
    use serde_json::json;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::time::{Duration, timeout};

    use super::{
        BodyError, Shutdown, is_loopback_authority, json, method_not_allowed, read_body, serve,
        text,
    };

    #[test]
    fn loopback_authority_accepts_only_loopback_hosts() {
        for good in [
            "127.0.0.1",
            "127.0.0.1:9222",
            "127.0.1.1:4444",
            "[::1]",
            "[::1]:9222",
            "localhost",
            "LOCALHOST:9222",
        ] {
            assert!(is_loopback_authority(good), "{good} should be accepted");
        }
        for bad in [
            "",
            "evil.com",
            "evil.com:9222",
            "localhost.evil.com:9222",
            "127.0.0.1:abc",
            "127.0.0.1:",
            "[::1]:",
            "[2001:db8::1]:9222",
            "192.168.1.1:9222",
            "0.0.0.0:9222",
        ] {
            assert!(!is_loopback_authority(bad), "{bad} should be rejected");
        }
    }

    #[test]
    fn responses_carry_their_content_types() {
        let json = json(StatusCode::OK, &json!({"a": 1}));
        assert_eq!(
            json.headers().get(header::CONTENT_TYPE),
            Some(&"application/json; charset=utf-8".parse().expect("header"))
        );

        let text = text(StatusCode::NOT_FOUND, "missing");
        assert_eq!(
            text.headers().get(header::CONTENT_TYPE),
            Some(&"text/plain; charset=utf-8".parse().expect("header"))
        );
        assert_eq!(text.status(), StatusCode::NOT_FOUND);

        let denied = method_not_allowed("GET,HEAD");
        assert_eq!(denied.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(
            denied.headers().get(header::ALLOW),
            Some(&"GET,HEAD".parse().expect("header"))
        );
    }

    #[tokio::test]
    async fn body_reads_are_bounded() {
        let bytes = read_body(Full::new(Bytes::from_static(b"abc")), 3)
            .await
            .expect("within the cap");
        assert_eq!(bytes, Bytes::from_static(b"abc"));

        let error = read_body(Full::new(Bytes::from_static(b"abcd")), 3)
            .await
            .expect_err("over the cap");
        assert_eq!(error, BodyError::TooLarge);
    }

    #[test]
    fn shutdown_is_shared_and_idempotent() {
        let shutdown = Shutdown::new();
        let clone = shutdown.clone();
        assert!(!shutdown.requested());
        clone.request();
        assert!(shutdown.requested());
        shutdown.request();
        assert!(clone.requested());
    }

    #[tokio::test]
    async fn serve_answers_and_drains_on_shutdown() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
        let addr = listener.local_addr().expect("addr");
        let shutdown = Shutdown::new();
        let handle = tokio::spawn({
            let shutdown = shutdown.clone();
            async move {
                serve(&listener, shutdown, 7_u32, |request, state| async move {
                    text(StatusCode::OK, &format!("{}:{state}", request.uri().path()))
                })
                .await
            }
        });

        let mut stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
        stream
            .write_all(b"GET /probe HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n")
            .await
            .expect("write");
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.expect("read");
        let response = String::from_utf8(response).expect("utf8");
        assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
        assert!(response.ends_with("/probe:7"), "{response}");

        shutdown.request();
        let result = timeout(Duration::from_secs(5), handle)
            .await
            .expect("server drains")
            .expect("server task");
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn serve_returns_at_once_when_already_stopped() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
        let shutdown = Shutdown::new();
        shutdown.request();
        let result = timeout(
            Duration::from_secs(1),
            serve(&listener, shutdown, (), |_request, ()| async {
                text(StatusCode::OK, "unreachable")
            }),
        )
        .await
        .expect("an already-stopped server returns");
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn shutdown_drains_an_idle_keep_alive_connection() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
        let addr = listener.local_addr().expect("addr");
        let shutdown = Shutdown::new();
        let handle = tokio::spawn({
            let shutdown = shutdown.clone();
            async move {
                serve(&listener, shutdown, (), |_request, ()| async {
                    text(StatusCode::OK, "ok")
                })
                .await
            }
        });

        let mut stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
        stream
            .write_all(b"GET /idle HTTP/1.1\r\nHost: test\r\n\r\n")
            .await
            .expect("write");
        let mut response = [0_u8; 128];
        let read = stream.read(&mut response).await.expect("read");
        assert!(
            String::from_utf8_lossy(&response[..read]).starts_with("HTTP/1.1 200 OK"),
            "{:?}",
            &response[..read]
        );

        shutdown.request();
        let result = timeout(Duration::from_secs(5), handle)
            .await
            .expect("idle connection drains")
            .expect("server task");
        assert!(result.is_ok());
    }
}
