use super::common::{TestServer, canned_ok, canned_redirect, read_body, scripted};
use std::io::Read;
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::{Duration, Instant};

use net::{
    Agent, AgentOptions, Method, NetError, Proxy, Referrer, ReferrerPolicy, Request, ResolveRule,
    TimeoutKind, TransportError,
};

const OBSERVE_TIMEOUT: Duration = Duration::from_secs(5);
const PROXY_PROBE_FLAG: &str = "NET_CRATE_PROXY_PROBE";

fn default_agent() -> Agent {
    Agent::new(AgentOptions::default())
}

#[tokio::test]
async fn http_transcripts_cover_status_headers_framing_limits_and_failures() {
    for status in [201_u16, 302, 404, 500] {
        let server =
            scripted([
                format!("HTTP/1.1 {status} Whatever\r\nContent-Length: 2\r\n\r\nno").into_bytes(),
            ]);
        let response = default_agent()
            .send(Request::new(Method::GET, server.url("/")))
            .await
            .expect("status is response data");
        assert_eq!(response.status(), status);
        assert_eq!(
            read_body(response.into_body().await).await.expect("body"),
            b"no"
        );
        server.assert_clean();
    }

    let server = scripted([canned_ok(
        &[("Content-Type", "text/plain"), ("X-Mixed-Case", "Value")],
        b"hello",
    )]);
    let requested = server.url("/a/b?c=d");
    let response = default_agent()
        .send(Request::new(Method::GET, requested.clone()))
        .await
        .expect("GET");
    let request = &server.requests()[0];
    assert_eq!(request.method, "GET");
    assert_eq!(request.target, "/a/b?c=d");
    assert_eq!(request.version, "HTTP/1.1");
    assert_eq!(response.status(), 200);
    assert_eq!(response.final_url(), &requested);
    assert_eq!(
        response.internal_headers().get("CONTENT-TYPE"),
        Some(&b"text/plain"[..])
    );
    assert_eq!(
        read_body(response.into_body().await).await.expect("body"),
        b"hello"
    );
    server.assert_clean();

    let server = scripted([b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n".to_vec()]);
    let body = default_agent()
        .send(Request::new(Method::GET, server.url("/chunked")))
        .await
        .expect("chunked")
        .into_body()
        .await;
    assert_eq!(read_body(body).await.expect("body"), b"hello world");
    server.assert_clean();

    let dead = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let dead_url = format!("http://{}/", dead.local_addr().expect("address"));
    drop(dead);
    assert!(matches!(
        default_agent()
            .send(Request::new(
                Method::GET,
                url::Url::parse(&dead_url).expect("url")
            ))
            .await,
        Err(NetError::Transport(_))
    ));

    let server = TestServer::start(|connection| {
        connection.read_request();
        std::thread::sleep(Duration::from_millis(300));
    });
    assert!(matches!(
        Agent::new(AgentOptions {
            timeout_global: Some(Duration::from_millis(60)),
            ..AgentOptions::default()
        })
        .send(Request::new(Method::GET, server.url("/stall")))
        .await,
        Err(NetError::Transport(TransportError::Timeout(
            TimeoutKind::Global
        )))
    ));
    server.assert_clean();
}

fn redirect_server(hop_delay: Duration, redirects: u8) -> (TestServer, Arc<AtomicU8>) {
    let hops = Arc::new(AtomicU8::new(0));
    let server_hops = Arc::clone(&hops);
    let server = TestServer::start(move |connection| {
        connection.read_request();
        std::thread::sleep(hop_delay);
        let number = server_hops.fetch_add(1, Ordering::Relaxed) + 1;
        let payload = if number <= redirects {
            canned_redirect(302, "/next")
        } else {
            canned_ok(&[], b"landed")
        };
        let _ = connection.write_all(&payload);
    });
    (server, hops)
}

#[tokio::test]
async fn timeout_global_covers_every_redirect_hop() {
    let hop_delay = Duration::from_millis(150);
    let global = Duration::from_millis(400);
    let (server, _hops) = redirect_server(hop_delay, 2);
    let started = Instant::now();
    let result = Agent::new(AgentOptions {
        timeout_global: Some(global),
        ..AgentOptions::default()
    })
    .send(Request::new(Method::GET, server.url("/start")))
    .await;
    let elapsed = started.elapsed();
    assert!(
        matches!(
            &result,
            Err(NetError::Transport(TransportError::Timeout(
                TimeoutKind::Global
            )))
        ),
        "expected global timeout, got {result:?}"
    );
    assert!(
        elapsed < global + hop_delay,
        "absolute deadline waited {elapsed:?}"
    );
    server.assert_clean();
}

#[tokio::test]
async fn timeout_per_call_restarts_on_every_redirect_hop() {
    let hop_delay = Duration::from_millis(100);
    let per_call = Duration::from_millis(500);
    let (server, hops) = redirect_server(hop_delay, 6);
    let result = Agent::new(AgentOptions {
        timeout_per_call: Some(per_call),
        ..AgentOptions::default()
    })
    .send(Request::new(Method::GET, server.url("/start")))
    .await;
    assert!(
        matches!(&result, Ok(response) if response.status() == 200),
        "each hop gets its own per-call deadline, got {result:?}"
    );
    assert_eq!(
        hops.load(Ordering::Relaxed),
        7,
        "all seven hops must be served"
    );
    server.assert_clean();
}

#[tokio::test]
async fn timeout_per_call_bounds_a_single_slow_hop() {
    let hop_delay = Duration::from_secs(1);
    let per_call = Duration::from_millis(50);
    let (server, _hops) = redirect_server(hop_delay, 0);
    let started = Instant::now();
    let result = Agent::new(AgentOptions {
        timeout_per_call: Some(per_call),
        ..AgentOptions::default()
    })
    .send(Request::new(Method::GET, server.url("/start")))
    .await;
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_millis(500),
        "per-call deadline waited {} ms",
        elapsed.as_millis()
    );
    assert!(
        matches!(
            result,
            Err(NetError::Transport(TransportError::Timeout(
                TimeoutKind::PerCall
            )))
        ),
        "a slow hop must fail with the per-call deadline, got {result:?}"
    );
    server.assert_clean();
}

#[tokio::test]
async fn response_bodies_stream_and_drop_cancels_the_socket() {
    let first_chunk_delivered = Arc::new(AtomicBool::new(false));
    let server_flag = Arc::clone(&first_chunk_delivered);
    let first = vec![b'a'; 1024];
    let rest = vec![b'b'; 99 * 1024];
    let server = TestServer::start(move |connection| {
        connection.read_request();
        connection
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                    first.len() + rest.len()
                )
                .as_bytes(),
            )
            .expect("head");
        connection.write_all(&first).expect("first chunk");
        let deadline = Instant::now() + OBSERVE_TIMEOUT;
        while !server_flag.load(Ordering::Acquire) {
            assert!(
                Instant::now() < deadline,
                "body was buffered until completion"
            );
            std::thread::yield_now();
        }
        connection.write_all(&rest).expect("remaining body");
    });
    let mut body = default_agent()
        .send(Request::new(Method::GET, server.url("/stream")))
        .await
        .expect("stream")
        .into_body()
        .await;
    let mut collected = Vec::new();
    let mut chunks = 0;
    while let Some(chunk) = body.read_chunk().await.expect("chunk") {
        collected.extend_from_slice(&chunk);
        chunks += 1;
        first_chunk_delivered.store(true, Ordering::Release);
    }
    assert_eq!(
        collected,
        [vec![b'a'; 1024], vec![b'b'; 99 * 1024]].concat()
    );
    assert!(chunks > 1);
    server.assert_clean();

    let peer_closed = Arc::new(AtomicBool::new(false));
    let server_flag = Arc::clone(&peer_closed);
    let server = TestServer::start(move |connection| {
        connection.read_request();
        connection
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 65536\r\n\r\npartial")
            .expect("partial body");
        server_flag.store(connection.await_peer_close(), Ordering::Release);
    });
    {
        let mut body = default_agent()
            .send(Request::new(Method::GET, server.url("/cancel")))
            .await
            .expect("cancel")
            .into_body()
            .await;
        let _ = body.read_chunk().await.expect("partial chunk");
    }
    let deadline = Instant::now() + OBSERVE_TIMEOUT;
    while !peer_closed.load(Ordering::Acquire) {
        assert!(
            Instant::now() < deadline,
            "server never observed the client"
        );
        tokio::task::yield_now().await;
    }
    server.assert_clean();
}

#[tokio::test]
async fn request_shaping_custom_method_fragments_and_rejections_are_wire_visible() {
    let server = scripted([b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".to_vec()]);
    default_agent()
        .send(Request::new(
            Method::parse("propfind").expect("custom method"),
            server.url("/m"),
        ))
        .await
        .expect("custom method request");
    assert_eq!(server.requests()[0].method, "propfind");
    server.assert_clean();

    let server = scripted([b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".to_vec()]);
    let agent = Agent::new(AgentOptions {
        identity: net::ValidIdentity::try_from(net::Identity {
            user_agent: net::UserAgent::from_static("builder/default"),
            ..net::Identity::default()
        })
        .expect("identity"),
        ..AgentOptions::default()
    });
    let mut request = Request::new(Method::POST, server.url("/shape#fragment"));
    request.headers.insert("X-Custom", "alpha").expect("header");
    request
        .headers
        .insert("User-Agent", "request/wins")
        .expect("header");
    request.body = Some(b"name=value".to_vec());
    agent.send(request).await.expect("shaped request");
    let request = &server.requests()[0];
    assert_eq!(request.target, "/shape");
    assert_eq!(request.body, b"name=value");
    assert_eq!(request.header("user-agent"), Some("request/wins"));
    assert_eq!(request.header("content-length"), Some("10"));
    server.assert_clean();

    let server = TestServer::start(|connection| {
        connection.read_request();
        connection
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
            .expect("response");
    });
    let mut request = Request::new(Method::GET, server.url("/invalid"));
    let result = request.headers.insert("X-Bad", "line\r\nInjected: yes");
    assert!(matches!(result, Err(net::HeaderError::InvalidValue(_))));
    assert_eq!(server.requests().len(), 0);
    server.assert_clean();
}

#[tokio::test]
async fn max_redirects_cap_returns_a_redirect_limit() {
    let empty = scripted([canned_redirect(302, ""), canned_redirect(302, "")]);
    assert!(matches!(
        Agent::new(AgentOptions {
            max_redirects: 1,
            ..AgentOptions::default()
        })
        .send(Request::new(Method::GET, empty.url("/empty")))
        .await,
        Err(NetError::RedirectLimit)
    ));
    assert_eq!(empty.requests().len(), 2);
    empty.assert_clean();

    let server = scripted(std::iter::repeat_n(canned_redirect(302, "/loop"), 3));
    assert!(matches!(
        Agent::new(AgentOptions {
            max_redirects: 2,
            ..AgentOptions::default()
        })
        .send(Request::new(Method::GET, server.url("/loop")))
        .await,
        Err(NetError::RedirectLimit)
    ));
    assert_eq!(server.requests().len(), 3);
    server.assert_clean();
}

// https://w3c.github.io/webappsec-referrer-policy/#determine-requests-referrer
// https://fetch.spec.whatwg.org/#http-redirect-fetch
#[tokio::test]
async fn redirect_hops_re_determine_the_referrer() {
    let landed = scripted([canned_ok(&[("Content-Type", "text/plain")], b"landed")]);
    let landed_url = landed.url("/landed");
    let start = scripted([canned_redirect(302, landed_url.as_str())]);
    let source = start.url("/page");
    let mut request = Request::new(Method::GET, start.url("/start"));
    request.referrer = Referrer::Url(source.clone());
    request.referrer_policy = ReferrerPolicy::StrictOriginWhenCrossOrigin;
    default_agent().send(request).await.expect("redirect chain");
    let first = &start.requests()[0];
    assert_eq!(
        first.header("referer"),
        Some(source.as_str()),
        "a same-origin hop sends the stripped source URL"
    );
    let second = &landed.requests()[0];
    let origin = source.join("/").expect("origin URL");
    assert_eq!(
        second.header("referer"),
        Some(origin.as_str()),
        "a cross-origin hop sends only the previous hop's origin"
    );
    start.assert_clean();
    landed.assert_clean();
}

// https://w3c.github.io/webappsec-fetch-metadata/#abstract-opdef-set-site
#[tokio::test]
async fn every_hop_carries_the_fetch_site_relationship() {
    let landed = scripted([canned_ok(&[], b"landed")]);
    let mut cross_site = landed.url("/landed");
    cross_site
        .set_host(Some("localhost"))
        .expect("host is replaceable");
    let start = scripted([canned_redirect(302, cross_site.as_str())]);
    let mut request = Request::new(Method::GET, start.url("/start"));
    request.initiator = Some(start.url("/page").origin());
    default_agent().send(request).await.expect("redirect chain");
    assert_eq!(
        start.requests()[0].header("sec-fetch-site"),
        Some("same-origin"),
        "the first hop is the initiator's own origin"
    );
    assert_eq!(
        landed.requests()[0].header("sec-fetch-site"),
        Some("cross-site"),
        "the chain keeps the widest relationship"
    );

    let server = scripted([canned_ok(&[], b"none")]);
    let request = Request::new(Method::GET, server.url("/"));
    default_agent().send(request).await.expect("no initiator");
    assert_eq!(
        server.requests()[0].header("sec-fetch-site"),
        Some("none"),
        "the user agent, not a document, started it"
    );
}

#[tokio::test]
async fn redirect_gate_can_refuse_the_next_hop() {
    let server = scripted([canned_redirect(302, "/landed"), canned_ok(&[], b"landed")]);
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut request = Request::new(Method::GET, server.url("/start"));
    let seen_gate = Arc::clone(&seen);
    request.on_redirect = Some(Arc::new(move |next, redirects| {
        seen_gate
            .lock()
            .expect("gate log")
            .push((next.path().to_owned(), redirects));
        false
    }));
    let error = default_agent()
        .send(request)
        .await
        .expect_err("refused hop");
    assert!(matches!(error, NetError::RejectedRequest));
    assert_eq!(
        seen.lock().expect("gate log").as_slice(),
        &[("/landed".to_owned(), 1)]
    );
    assert_eq!(server.requests().len(), 1);
    server.assert_clean();
}

// https://fetch.spec.whatwg.org/#forbidden-header-name
#[tokio::test]
async fn cross_origin_redirect_drops_caller_forbidden_headers() {
    let landing = TestServer::start(|connection| {
        let request = connection.read_request();
        assert_ne!(request.header("host"), Some("evil.example"));
        assert!(request.header("content-length").is_none() || request.body.is_empty());
        connection
            .write_all(&canned_ok(&[], b"landed"))
            .expect("landing");
    });
    let first = TestServer::start({
        let location = format!("http://{}/landed", landing.local_addr());
        move |connection| {
            connection.read_request();
            connection
                .write_all(&canned_redirect(302, &location))
                .expect("redirect");
        }
    });
    let mut request = Request::new(Method::POST, first.url("/start"));
    request
        .headers
        .insert("Host", "evil.example")
        .expect("host");
    request
        .headers
        .insert("Content-Length", "7")
        .expect("length");
    request.body = Some(b"field=1".to_vec());
    default_agent()
        .send(request)
        .await
        .expect("cross-origin host stripped");
    first.assert_clean();
    landing.assert_clean();
}

#[tokio::test]
async fn connect_proxy_routes_https_and_reports_denials() {
    let proxy = TestServer::start(|connection| {
        let request = connection.read_request();
        assert_eq!(request.method, "CONNECT");
        assert_eq!(request.target, "origin.test:443");
        assert_eq!(
            request.header("proxy-authorization"),
            Some("Basic dXNlcjpwQHNz")
        );
        connection
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\nNOT-TLS")
            .expect("connect 200");
        std::thread::sleep(Duration::from_millis(300));
    });
    let proxy_uri = format!("http://user:p%40ss@{}", proxy.local_addr());
    let proxy_config = proxy_uri
        .parse::<Proxy>()
        .expect("percent-encoded credentials");
    let err = Agent::new(AgentOptions {
        proxy: Some(proxy_config),
        ..AgentOptions::default()
    })
    .send(Request::new(
        Method::GET,
        url::Url::parse("https://origin.test/").expect("https"),
    ))
    .await
    .expect_err("proxy request");
    assert!(
        matches!(
            err,
            NetError::Transport(TransportError::Connect(_) | TransportError::Tls(_))
        ),
        "unexpected proxy error: {err:?}"
    );
    proxy.assert_clean();

    let denied = TestServer::start(|connection| {
        connection.read_request();
        connection
            .write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")
            .expect("connect 403");
    });
    assert!(matches!(
        Agent::new(AgentOptions {
            proxy: Some(
                format!("http://{}", denied.local_addr())
                    .parse::<Proxy>()
                    .expect("proxy authority")
            ),
            ..AgentOptions::default()
        })
        .send(Request::new(
            Method::GET,
            url::Url::parse("https://origin.test/").expect("https"),
        ))
        .await,
        Err(NetError::Transport(TransportError::Connect(_)))
    ));
    denied.assert_clean();
}

#[test]
fn proxy_parser_rejects_unsupported_or_ambiguous_authorities() {
    for invalid in [
        "",
        "not a uri",
        "socks5://127.0.0.1:1080",
        "ftp://127.0.0.1:8080",
        "http://",
        "http://proxy.test:8080/some/path",
        "http://proxy.test:8080/?x=1",
        "http://proxy.test:8080/#fragment",
        "http://:secret@proxy.test:8080",
    ] {
        assert_eq!(
            invalid.parse::<Proxy>().expect_err("invalid proxy"),
            net::InvalidProxy,
            "{invalid:?}"
        );
    }
}

#[test]
fn proxy_debug_redacts_credentials() {
    let proxy = "http://user:p%40ss@localhost:8080"
        .parse::<Proxy>()
        .expect("proxy");
    let debug = format!("{proxy:?}");
    assert!(!debug.contains("user"), "{debug}");
    assert!(!debug.contains("p@ss"), "{debug}");
    assert!(!debug.contains("p%40ss"), "{debug}");
}

#[tokio::test]
async fn tls_handshake_failure_is_a_transport_tls_error() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("TLS listener");
    let addr = listener.local_addr().expect("address");
    let worker = std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            use std::io::Write as _;
            let _ = stream.write_all(b"NOT-TLS");
        }
    });
    assert!(matches!(
        default_agent()
            .send(Request::new(
                Method::GET,
                url::Url::parse(&format!("https://{addr}/")).expect("https"),
            ))
            .await,
        Err(NetError::Transport(TransportError::Tls(reason))) if !reason.is_empty()
    ));
    worker.join().expect("TLS worker");
}

#[tokio::test]
async fn environment_proxy_variables_are_ignored() {
    if std::env::var(PROXY_PROBE_FLAG).ok().as_deref() == Some("1") {
        let server = TestServer::start(|connection| {
            connection.read_request();
            connection
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .expect("direct response");
        });
        default_agent()
            .send(Request::new(Method::GET, server.url("/direct")))
            .await
            .expect("environment proxy must be ignored");
        server.assert_clean();
    } else {
        let proxy = std::net::TcpListener::bind("127.0.0.1:0").expect("proxy bind");
        let proxy_uri = format!("http://{}", proxy.local_addr().expect("proxy address"));
        let status = Command::new(std::env::current_exe().expect("test executable"))
            .args([
                "--exact",
                "send_loopback::environment_proxy_variables_are_ignored",
            ])
            .env(PROXY_PROBE_FLAG, "1")
            .env("HTTP_PROXY", &proxy_uri)
            .env("http_proxy", &proxy_uri)
            .env("HTTPS_PROXY", &proxy_uri)
            .env("https_proxy", &proxy_uri)
            .env("ALL_PROXY", &proxy_uri)
            .env("all_proxy", &proxy_uri)
            .env_remove("NO_PROXY")
            .env_remove("no_proxy")
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .expect("proxy probe");
        assert!(status.success());
    }
}

#[tokio::test]
async fn ipv6_loopback_send_and_connect_paths_are_exercised() {
    let server = TestServer::start_v6(|connection| {
        let request = connection.read_request();
        assert_eq!(request.target, "/v6");
        connection
            .write_all(&canned_ok(&[], b"v6"))
            .expect("v6 response");
    });
    let response = default_agent()
        .send(Request::new(Method::GET, server.url("/v6")))
        .await
        .expect("ipv6 send");
    assert_eq!(
        read_body(response.into_body().await).await.expect("body"),
        b"v6"
    );
    server.assert_clean();
}

#[tokio::test]
async fn url_credentials_and_scheme_rejections_are_wire_visible() {
    let server = TestServer::start(|connection| {
        let request = connection.read_request();
        assert_eq!(request.header("authorization"), Some("Basic dXNlcjpwQHNz"));
        connection
            .write_all(&canned_ok(&[], b"ok"))
            .expect("auth response");
    });
    let mut url = server.url("/");
    url.set_username("user").expect("username");
    url.set_password(Some("p@ss")).expect("password");
    assert_eq!(url.password(), Some("p%40ss"));
    default_agent()
        .send(Request::new(Method::GET, url))
        .await
        .expect("basic");
    server.assert_clean();

    assert!(matches!(
        default_agent()
            .send(Request::new(
                Method::GET,
                url::Url::parse("ws://127.0.0.1/").expect("ws"),
            ))
            .await,
        Err(NetError::RejectedRequest)
    ));
}

fn cors_settings(
    mode: net::cors::RequestMode,
    credentials: net::cors::CredentialsMode,
    origin: url::Origin,
) -> net::cors::CorsSettings {
    net::cors::CorsSettings {
        mode,
        credentials,
        redirect: net::cors::RedirectMode::Follow,
        origin,
    }
}

#[tokio::test]
async fn same_origin_mode_rejects_a_cross_origin_request_before_sending() {
    let server = scripted([canned_ok(&[], b"must not arrive")]);
    let origin = url::Url::parse("https://a.test/")
        .expect("origin URL")
        .origin();
    let mut request = Request::new(Method::GET, server.url("/"));
    request.cors = cors_settings(
        net::cors::RequestMode::SameOrigin,
        net::cors::CredentialsMode::Omit,
        origin,
    );
    assert!(matches!(
        default_agent().send(request).await,
        Err(NetError::RejectedRequest)
    ));
    assert_eq!(server.requests().len(), 0);
    server.assert_clean();
}

#[tokio::test]
async fn same_origin_mode_rejects_a_redirect_that_crosses_origins() {
    let landing = scripted([canned_ok(&[], b"must not arrive")]);
    let location = landing.url("/landed").to_string();
    let first = TestServer::start(move |connection| {
        connection.read_request();
        connection
            .write_all(&canned_redirect(302, &location))
            .expect("redirect response");
    });
    let origin = first.url("/").origin();
    let mut request = Request::new(Method::GET, first.url("/start"));
    request.cors = cors_settings(
        net::cors::RequestMode::SameOrigin,
        net::cors::CredentialsMode::Omit,
        origin,
    );
    assert!(matches!(
        default_agent().send(request).await,
        Err(NetError::RejectedRequest)
    ));
    assert_eq!(first.requests().len(), 1);
    assert_eq!(landing.requests().len(), 0);
    first.assert_clean();
    landing.assert_clean();
}

#[tokio::test]
async fn same_origin_post_carries_the_origin_header() {
    let server = scripted([canned_ok(&[], b"ok")]);
    let origin = server.url("/").origin();
    let mut request = Request::new(Method::POST, server.url("/submit"));
    request.cors = cors_settings(
        net::cors::RequestMode::SameOrigin,
        net::cors::CredentialsMode::Include,
        origin.clone(),
    );
    request.body = Some(b"field=1".to_vec());
    default_agent()
        .send(request)
        .await
        .expect("same-origin POST");
    assert_eq!(
        server.requests()[0].header("origin"),
        Some(origin.ascii_serialization().as_str())
    );
    server.assert_clean();
}

#[tokio::test]
async fn opaque_origin_serializes_null_and_taints_the_hop() {
    let server = TestServer::start(|connection| {
        let request = connection.read_request();
        assert_eq!(request.header("origin"), Some("null"));
        connection
            .write_all(&canned_ok(
                &[("Access-Control-Allow-Origin", "null")],
                b"opaque",
            ))
            .expect("cors response");
    });
    let origin = url::Url::parse("data:text/plain,opaque")
        .expect("opaque URL")
        .origin();
    let mut request = Request::new(Method::GET, server.url("/"));
    request.cors = cors_settings(
        net::cors::RequestMode::Cors,
        net::cors::CredentialsMode::Omit,
        origin,
    );
    let response = default_agent()
        .send(request)
        .await
        .expect("null passes the CORS check");
    assert_eq!(response.response_type(), net::cors::ResponseType::Cors);
    assert_eq!(server.requests().len(), 1);
    server.assert_clean();
}

#[tokio::test]
async fn redirect_taint_serializes_null_and_null_passes_the_cors_check() {
    let origin = url::Url::parse("https://a.test/")
        .expect("origin URL")
        .origin();
    let landing = TestServer::start(|connection| {
        let request = connection.read_request();
        assert_eq!(request.header("origin"), Some("null"));
        connection
            .write_all(&canned_ok(
                &[("Access-Control-Allow-Origin", "null")],
                b"tainted",
            ))
            .expect("tainted landing response");
    });
    let location = landing.url("/landed").to_string();
    let redirect = TestServer::start(move |connection| {
        connection.read_request();
        connection
            .write_all(
                format!(
                    "HTTP/1.1 302 Found\r\nAccess-Control-Allow-Origin: https://a.test\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )
                .as_bytes(),
            )
            .expect("tainted redirect response");
    });
    let mut request = Request::new(Method::GET, redirect.url("/start"));
    request.cors = cors_settings(
        net::cors::RequestMode::Cors,
        net::cors::CredentialsMode::Omit,
        origin.clone(),
    );
    let response = default_agent()
        .send(request)
        .await
        .expect("null origin passes the CORS check");
    assert_eq!(response.response_type(), net::cors::ResponseType::Cors);
    redirect.assert_clean();
    landing.assert_clean();

    let echoing = TestServer::start(|connection| {
        connection.read_request();
        connection
            .write_all(&canned_ok(
                &[("Access-Control-Allow-Origin", "https://a.test")],
                b"unreadable",
            ))
            .expect("echoing response");
    });
    let location = echoing.url("/landed").to_string();
    let redirect = TestServer::start(move |connection| {
        connection.read_request();
        connection
            .write_all(
                format!(
                    "HTTP/1.1 302 Found\r\nAccess-Control-Allow-Origin: https://a.test\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )
                .as_bytes(),
            )
            .expect("redirect response");
    });
    let mut request = Request::new(Method::GET, redirect.url("/start"));
    request.cors = cors_settings(
        net::cors::RequestMode::Cors,
        net::cors::CredentialsMode::Omit,
        origin,
    );
    assert!(matches!(
        default_agent().send(request).await,
        Err(NetError::RejectedRequest)
    ));
    assert_eq!(echoing.requests().len(), 1);
    redirect.assert_clean();
    echoing.assert_clean();
}

#[tokio::test]
async fn resolve_maps_hit_miss_and_fail() {
    assert_eq!(
        "not-a-spec"
            .parse::<ResolveRule>()
            .expect_err("invalid spec")
            .to_string(),
        "resolve spec \"not-a-spec\" must be PATTERN=IPv4 or PATTERN=fail"
    );

    let server = scripted([canned_ok(&[], b"mapped")]);
    let port = server.local_addr().port();
    let mapped = url::Url::parse(&format!("http://web-platform.test:{port}/")).expect("mapped URL");
    let agent = Agent::new(AgentOptions {
        resolve: vec![
            "nonexistent.*.test=fail"
                .parse::<ResolveRule>()
                .expect("fail rule"),
            "*.test=127.0.0.1"
                .parse::<ResolveRule>()
                .expect("rewrite rule"),
        ],
        ..AgentOptions::default()
    });
    let response = agent
        .send(Request::new(Method::GET, mapped))
        .await
        .expect("mapped hit");
    assert_eq!(
        read_body(response.into_body().await).await.expect("body"),
        b"mapped"
    );
    assert_eq!(
        server.requests()[0]
            .header("Host")
            .map(|value| value.split(':').next()),
        Some(Some("web-platform.test"))
    );
    server.assert_clean();

    let miss_server = scripted([canned_ok(&[], b"direct")]);
    let miss = agent
        .send(Request::new(Method::GET, miss_server.url("/")))
        .await
        .expect("unmapped 127.0.0.1 uses libc");
    assert_eq!(
        read_body(miss.into_body().await).await.expect("body"),
        b"direct"
    );
    miss_server.assert_clean();

    let failed = agent.send(Request::new(
        Method::GET,
        url::Url::parse("http://nonexistent.web-platform.test/").expect("fail URL"),
    ));
    assert!(matches!(
        failed.await,
        Err(NetError::Transport(TransportError::Dns(_)))
    ));
}

#[tokio::test]
async fn request_deadline_expires_with_a_typed_timeout() {
    let server = TestServer::start(|connection| {
        connection.read_request();
        std::thread::sleep(Duration::from_millis(600));
    });
    let mut request = Request::new(Method::GET, server.url("/stall"));
    request.deadline = Some(Instant::now() + Duration::from_millis(80));
    let error = default_agent()
        .send(request)
        .await
        .expect_err("deadline must expire");
    assert!(
        matches!(error, NetError::Transport(TransportError::Timeout(_))),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn https_dials_offer_h2_before_http1_in_alpn() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let address = listener.local_addr().expect("addr");
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let hello = read_client_hello(&mut stream);
        let protocols = client_hello_alpn(&hello);
        std::thread::sleep(Duration::from_millis(600));
        protocols
    });

    let url = url::Url::parse(&format!("https://{address}/")).expect("absolute url");
    let mut request = Request::new(Method::GET, url);
    request.deadline = Some(Instant::now() + Duration::from_millis(80));
    let error = default_agent()
        .send(request)
        .await
        .expect_err("the test listener never completes a TLS handshake");
    assert!(
        matches!(error, NetError::Transport(TransportError::Timeout(_))),
        "unexpected error: {error}"
    );
    assert_eq!(
        server.join().expect("server"),
        vec!["h2".to_owned(), "http/1.1".to_owned()]
    );
}

fn read_client_hello(stream: &mut TcpStream) -> Vec<u8> {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("read timeout is settable");
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        if bytes.len() >= 5 {
            let body = usize::from(u16::from_be_bytes([bytes[3], bytes[4]]));
            if bytes.len() >= 5 + body {
                bytes.truncate(5 + body);
                return bytes;
            }
        }
        let read = stream.read(&mut chunk).expect("client hello is readable");
        assert_ne!(read, 0, "client closed before sending a full ClientHello");
        bytes.extend_from_slice(&chunk[..read]);
    }
}

fn client_hello_alpn(hello: &[u8]) -> Vec<String> {
    assert_eq!(hello.first(), Some(&0x16), "not a TLS handshake record");
    assert_eq!(hello.get(5), Some(&0x01), "not a ClientHello");
    let mut at = 9;
    at += 2 + 32;
    at += 1 + usize::from(hello[at]);
    let suites = usize::from(u16::from_be_bytes([hello[at], hello[at + 1]]));
    at += 2 + suites;
    at += 1 + usize::from(hello[at]);
    let extensions = usize::from(u16::from_be_bytes([hello[at], hello[at + 1]]));
    at += 2;
    let end = at + extensions;
    while at + 4 <= end {
        let kind = usize::from(u16::from_be_bytes([hello[at], hello[at + 1]]));
        let length = usize::from(u16::from_be_bytes([hello[at + 2], hello[at + 3]]));
        at += 4;
        if kind == 16 {
            let list = usize::from(u16::from_be_bytes([hello[at], hello[at + 1]]));
            at += 2;
            let mut protocols = Vec::new();
            let mut entry = at;
            while entry < at + list {
                let len = usize::from(hello[entry]);
                entry += 1;
                protocols.push(String::from_utf8_lossy(&hello[entry..entry + len]).into_owned());
                entry += len;
            }
            return protocols;
        }
        at += length;
    }
    Vec::new()
}

// https://fetch.spec.whatwg.org/#concept-internal-response
#[tokio::test]
async fn an_opaque_response_keeps_the_internal_header_list() {
    let server = scripted([canned_ok(
        &[("x-internal", "kept"), ("content-type", "image/png")],
        b"png",
    )]);
    let origin = url::Url::parse("https://a.test/")
        .expect("origin URL")
        .origin();
    let mut request = Request::new(Method::GET, server.url("/img"));
    request.cors = cors_settings(
        net::cors::RequestMode::NoCors,
        net::cors::CredentialsMode::Include,
        origin,
    );
    let response = default_agent()
        .send(request)
        .await
        .expect("cross-origin no-cors");
    assert_eq!(
        response.response_type(),
        net::cors::ResponseType::Opaque,
        "a cross-origin no-cors response is opaque"
    );
    assert_eq!(
        response.internal_headers().get("x-internal"),
        Some(b"kept".as_slice()),
        "the unsafe response keeps every field the server sent"
    );
    let filtered = response.filtered();
    assert_eq!(filtered.status(), 0, "page script sees no status");
    assert!(
        filtered.headers().is_empty(),
        "page script sees no field of an opaque response"
    );
    assert!(filtered.url().is_none(), "page script sees no URL");
    assert_eq!(
        read_body(response.into_body().await).await.expect("body"),
        b"png",
        "the body still arrives for the engine consumer"
    );
    server.assert_clean();
}

// https://w3c.github.io/webappsec-csp/#report-violation
#[tokio::test]
async fn a_no_cors_error_request_is_sent_and_refuses_a_redirect() {
    let landing = url::Url::parse("https://b.test/landed").expect("landing URL");
    let server = scripted([canned_redirect(302, landing.as_str())]);
    let origin = url::Url::parse("https://a.test/")
        .expect("origin URL")
        .origin();
    let mut request = Request::new(Method::POST, server.url("/report"));
    request.cors = net::cors::CorsSettings {
        redirect: net::cors::RedirectMode::Error,
        ..cors_settings(
            net::cors::RequestMode::NoCors,
            net::cors::CredentialsMode::SameOrigin,
            origin,
        )
    };
    assert!(
        matches!(
            default_agent().send(request).await,
            Err(NetError::RejectedRequest)
        ),
        "the redirect response is a network error"
    );
    assert_eq!(
        server.requests().len(),
        1,
        "the request still reached the collector"
    );
    server.assert_clean();
}

// https://fetch.spec.whatwg.org/#main-fetch
#[tokio::test]
async fn a_no_cors_manual_request_is_refused_before_sending() {
    let server = scripted([canned_ok(&[], b"must not arrive")]);
    let origin = url::Url::parse("https://a.test/")
        .expect("origin URL")
        .origin();
    let mut request = Request::new(Method::GET, server.url("/"));
    request.cors = net::cors::CorsSettings {
        redirect: net::cors::RedirectMode::Manual,
        ..cors_settings(
            net::cors::RequestMode::NoCors,
            net::cors::CredentialsMode::Include,
            origin,
        )
    };
    assert!(matches!(
        default_agent().send(request).await,
        Err(NetError::RejectedRequest)
    ));
    assert_eq!(server.requests().len(), 0);
    server.assert_clean();
}

// https://wicg.github.io/ua-client-hints/#http-ua-hints
#[tokio::test]
async fn a_trustworthy_hop_carries_the_identity_and_its_hints() {
    let server = scripted([canned_ok(&[], b"")]);
    let identity = net::ValidIdentity::try_from(net::Identity {
        user_agent: net::UserAgent::parse("probe/1").expect("user agent"),
        ..net::Identity::default()
    })
    .expect("identity");
    let agent = Agent::new(AgentOptions {
        identity: identity.clone(),
        ..AgentOptions::default()
    });
    agent
        .send(Request::new(Method::GET, server.url("/")))
        .await
        .expect("request");
    let request = &server.requests()[0];
    assert_eq!(
        request.header("user-agent"),
        Some("probe/1"),
        "the header names the configured identity"
    );
    assert_eq!(
        request.header("sec-ch-ua"),
        Some(identity.sec_ch_ua().as_str()),
        "the low-entropy brands agree with the user agent"
    );
    assert_eq!(
        request.header("sec-ch-ua-mobile"),
        Some(identity.sec_ch_ua_mobile())
    );
    assert_eq!(
        request.header("sec-ch-ua-platform"),
        Some(identity.sec_ch_ua_platform().as_str())
    );
    assert_eq!(
        request.header("accept-language"),
        Some("en-US,en;q=0.9"),
        "the language list becomes one header with Chromium's q-values"
    );
    server.assert_clean();
}
