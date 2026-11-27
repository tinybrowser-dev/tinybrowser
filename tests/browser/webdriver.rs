use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::fixture::{Fixture, PIXEL, TestServer, ok, renderer_children};
use serde_json::{Value, json};

fn start(extra_args: Vec<String>) -> (String, Fixture) {
    start_in(Fixture::new("tinybrowser-webdriver"), extra_args)
}

fn start_in(mut fixture: Fixture, extra_args: Vec<String>) -> (String, Fixture) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    drop(listener);
    let mut command = Command::new(env!("CARGO_BIN_EXE_tinybrowser"));
    command.arg("serve");
    command.arg(format!("--port={port}"));
    command.args(extra_args);
    let child = command
        .env("XDG_RUNTIME_DIR", fixture.runtime())
        .env("XDG_DATA_HOME", fixture.data())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn WebDriver");
    fixture.append_child(child);
    (format!("127.0.0.1:{port}"), fixture)
}

struct HttpResponse {
    status: u16,
    content_type: Option<String>,
    body: Vec<u8>,
}

fn send(addr: &str, method: &str, path: &str, body: Option<&str>) -> HttpResponse {
    let payload = body.unwrap_or("");
    let mut last_error = None;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        match TcpStream::connect(addr) {
            Ok(mut stream) => {
                stream
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .expect("read timeout is settable");
                let req = format!(
                    "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                    payload.len()
                );
                stream.write_all(req.as_bytes()).expect("write");
                let mut buf = Vec::new();
                stream.read_to_end(&mut buf).expect("read");
                let split = buf
                    .windows(4)
                    .position(|window| window == b"\r\n\r\n")
                    .expect("http header terminator");
                let head = String::from_utf8_lossy(&buf[..split]);
                let mut lines = head.split("\r\n");
                let status = lines
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .and_then(|code| code.parse().ok())
                    .expect("status line");
                let content_type = lines.find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-type")
                        .then(|| value.trim().to_owned())
                });
                return HttpResponse {
                    status,
                    content_type,
                    body: buf[split + 4..].to_vec(),
                };
            }
            Err(error) => {
                last_error = Some(error);
                thread::sleep(Duration::from_millis(10));
            }
        }
    }
    panic!("webdriver not reachable: {last_error:?}");
}

fn request(addr: &str, method: &str, path: &str, body: Option<&str>) -> Value {
    serde_json::from_slice(&send(addr, method, path, body).body).expect("json")
}

const TLS_SERVER_CERT_PEM: &str = include_str!("../fixtures/tls/loopback-127.0.0.1.crt.pem");

const TLS_SERVER_KEY_PEM: &str = include_str!("../fixtures/tls/loopback-127.0.0.1.key.pem");

fn https_server() -> (String, TestServer) {
    let identity = native_tls::Identity::from_pkcs8(
        TLS_SERVER_CERT_PEM.as_bytes(),
        TLS_SERVER_KEY_PEM.as_bytes(),
    )
    .expect("the test identity parses");
    let server = TestServer::start_tls(
        vec![(
            "/",
            ok(
                "text/html",
                b"<!doctype html><script>\
                  if (parent !== window) parent.postMessage(String(isSecureContext), '*');\
                  </script><title>trusted</title>",
            ),
        )],
        identity,
    );
    (format!("https://{}/", server.address()), server)
}

fn navigate_new_session(addr: &str, url: &str) -> Value {
    let created = request(addr, "POST", "/session", Some("{}"));
    let id = created["value"]["sessionId"]
        .as_str()
        .expect("session id")
        .to_owned();
    request(
        addr,
        "POST",
        &format!("/session/{id}/url"),
        Some(&json!({ "url": url }).to_string()),
    )
}

#[test]
fn a_tls_ca_from_the_command_line_makes_an_https_navigation_load() {
    let (url, _server) = https_server();

    let untrusted = Fixture::new("tinybrowser-webdriver-tls-ca-untrusted");
    let (addr, _fixture) = start_in(untrusted, Vec::new());
    let refused = navigate_new_session(&addr, &url);
    assert!(
        refused["value"]["error"].is_string(),
        "an unknown certificate must fail the navigation: {refused}"
    );

    let trusted = Fixture::new("tinybrowser-webdriver-tls-ca-trusted");
    let ca = trusted.data().join("test-ca.pem");
    std::fs::write(&ca, TLS_SERVER_CERT_PEM).expect("write CA");
    let (addr, _fixture) = start_in(trusted, vec![format!("--tls-ca={}", ca.display())]);
    let loaded = navigate_new_session(&addr, &url);
    assert_eq!(loaded["value"], Value::Null, "{loaded}");
}

// https://html.spec.whatwg.org/multipage/webappapis.html#secure-contexts
#[test]
fn an_https_child_of_an_insecure_parent_is_not_a_secure_context() {
    let (url, _server) = https_server();
    let parent = TestServer::start(
        vec![(
            "/",
            ok(
                "text/html",
                format!(
                    "<!doctype html><iframe src=\"{url}\"></iframe>\
                     <script>addEventListener('message', event => window.__child = event.data)</script>"
                )
                .as_bytes(),
            ),
        )],
        None,
    );

    let fixture = Fixture::new("tinybrowser-webdriver-secure-context");
    let ca = fixture.data().join("test-ca.pem");
    std::fs::write(&ca, TLS_SERVER_CERT_PEM).expect("write CA");
    let (addr, _fixture) = start_in(
        fixture,
        vec![
            format!("--tls-ca={}", ca.display()),
            "--resolve=*.test=127.0.0.1".to_owned(),
        ],
    );
    let id = create_session(&addr);
    navigate(
        &addr,
        &id,
        &format!("http://example.test:{}/", parent.address().port()),
    );

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let reported = request(
            &addr,
            "POST",
            &format!("/session/{id}/execute/sync"),
            Some(r#"{"script":"return window.__child ?? 'pending'","args":[]}"#),
        );
        if reported["value"] == json!("false") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the https child never reported its bit: {reported}"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn session_execute_script_roundtrip() {
    let (addr, _fixture) = start(Vec::new());

    let status = request(&addr, "GET", "/status", None);
    assert_eq!(status["value"]["ready"], json!(true));

    let created = request(&addr, "POST", "/session", Some("{}"));
    let id = created["value"]["sessionId"]
        .as_str()
        .expect("session id")
        .to_owned();

    let added = request(
        &addr,
        "POST",
        &format!("/session/{id}/execute/sync"),
        Some(r#"{"script":"return 1+1","args":[]}"#),
    );
    assert_eq!(added["value"].as_f64(), Some(2.0));

    let nested = request(
        &addr,
        "POST",
        &format!("/session/{id}/execute/sync"),
        Some(r#"{"script":"return [1,['x']]","args":[]}"#),
    );
    assert_eq!(nested["value"], json!([1.0, ["x"]]));

    let async_result = request(
        &addr,
        "POST",
        &format!("/session/{id}/execute/async"),
        Some(r#"{"script":"arguments[0](3)","args":[]}"#),
    );
    assert_eq!(async_result["value"].as_f64(), Some(3.0));

    let delayed = request(
        &addr,
        "POST",
        &format!("/session/{id}/execute/async"),
        Some(
            r#"{"script":"var cb = arguments[0]; setTimeout(function() { cb(4); }, 0);","args":[]}"#,
        ),
    );
    assert_eq!(delayed["value"].as_f64(), Some(4.0));

    let nan = request(
        &addr,
        "POST",
        &format!("/session/{id}/execute/sync"),
        Some(r#"{"script":"return NaN","args":[]}"#),
    );
    assert_eq!(nan["value"], json!(null));

    let enumerated = request(
        &addr,
        "POST",
        &format!("/session/{id}/execute/sync"),
        Some(
            r#"{"script":"return Object.keys(globalThis).filter(function(k){return k.indexOf('__tb_webdriver')===0||k.indexOf('__wd_')===0})","args":[]}"#,
        ),
    );
    assert_eq!(enumerated["value"], json!([]));
    let click_type = request(
        &addr,
        "POST",
        &format!("/session/{id}/execute/sync"),
        Some(r#"{"script":"return typeof __tb_webdriver_click","args":[]}"#),
    );
    assert_eq!(click_type["value"], json!("undefined"));
}

#[test]
fn execute_sync_waits_for_returned_promise_or_script_timeout() {
    let (addr, _fixture) = start(Vec::new());

    let created = request(&addr, "POST", "/session", Some("{}"));
    let id = created["value"]["sessionId"]
        .as_str()
        .expect("session id")
        .to_owned();

    let resolved = request(
        &addr,
        "POST",
        &format!("/session/{id}/execute/sync"),
        Some(r#"{"script":"return Promise.resolve(7)","args":[]}"#),
    );
    assert_eq!(resolved["value"].as_f64(), Some(7.0));

    let delayed = request(
        &addr,
        "POST",
        &format!("/session/{id}/execute/sync"),
        Some(
            r#"{"script":"return new Promise(function(resolve) { setTimeout(function() { resolve(8); }, 0); })","args":[]}"#,
        ),
    );
    assert_eq!(delayed["value"].as_f64(), Some(8.0));

    let rejected = request(
        &addr,
        "POST",
        &format!("/session/{id}/execute/sync"),
        Some(r#"{"script":"return Promise.reject()","args":[]}"#),
    );
    assert_eq!(rejected["value"]["error"], json!("javascript error"));

    let empty_reject = request(
        &addr,
        "POST",
        &format!("/session/{id}/execute/sync"),
        Some(r#"{"script":"return Promise.reject('')","args":[]}"#),
    );
    assert_eq!(empty_reject["value"]["error"], json!("javascript error"));

    let wait_object = request(
        &addr,
        "POST",
        &format!("/session/{id}/execute/sync"),
        Some(r#"{"script":"return { __wd_wait: true }","args":[]}"#),
    );
    assert_eq!(wait_object["value"], json!({"__wd_wait": true}));

    request(
        &addr,
        "POST",
        &format!("/session/{id}/timeouts"),
        Some(r#"{"script":200}"#),
    );
    let started = Instant::now();
    let timed_out = request(
        &addr,
        "POST",
        &format!("/session/{id}/execute/sync"),
        Some(
            r#"{"script":"return new Promise(function(resolve) { setTimeout(function() { resolve(1); }, 30000); })","args":[]}"#,
        ),
    );
    assert_eq!(timed_out["value"]["error"], json!("script timeout"));
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "script timeout waited {:?}",
        started.elapsed()
    );

    request(
        &addr,
        "POST",
        &format!("/session/{id}/timeouts"),
        Some(r#"{"script":400}"#),
    );
    let started = Instant::now();
    let remaining_budget = request(
        &addr,
        "POST",
        &format!("/session/{id}/execute/sync"),
        Some(
            r#"{"script":"var t = Date.now(); while (Date.now() - t < 250) {} return new Promise(function(resolve) { setTimeout(function() { resolve(1); }, 250); });","args":[]}"#,
        ),
    );
    assert_eq!(remaining_budget["value"]["error"], json!("script timeout"));
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "shared script timeout waited {:?}",
        started.elapsed()
    );
}

#[test]
fn navigate_returns_after_load_not_after_timers() {
    let server = TestServer::start(
        vec![(
            "/",
            ok(
                "text/html",
                b"<!doctype html><script>window.early = 1; setTimeout(function() { window.late = 1; }, 30000);</script>",
            ),
        )],
        None,
    );
    let server_addr = server.address();

    let (addr, _fixture) = start(Vec::new());

    let created = request(&addr, "POST", "/session", Some("{}"));
    let id = created["value"]["sessionId"]
        .as_str()
        .expect("session id")
        .to_owned();

    let started = Instant::now();
    let navigated = request(
        &addr,
        "POST",
        &format!("/session/{id}/url"),
        Some(&format!(r#"{{"url":"http://{server_addr}/"}}"#)),
    );
    assert_eq!(navigated["value"], json!(null));
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "navigate waited for host timers"
    );

    let early = request(
        &addr,
        "POST",
        &format!("/session/{id}/execute/sync"),
        Some(r#"{"script":"return window.early","args":[]}"#),
    );
    assert_eq!(early["value"].as_f64(), Some(1.0));

    let late = request(
        &addr,
        "POST",
        &format!("/session/{id}/execute/sync"),
        Some(r#"{"script":"return typeof window.late","args":[]}"#),
    );
    assert_eq!(late["value"], json!("undefined"));
}

#[test]
fn image_load_capture_stops_at_the_document() {
    let server = TestServer::start(
        vec![
            (
                "/",
                ok(
                    "text/html",
                    b"<!doctype html><title>load path</title><script>\
window.windowHits = [];\
window.documentHits = [];\
window.addEventListener('load', (event) => { window.windowHits.push(event.target === window ? 'window' : event.target.tagName); }, true);\
document.addEventListener('load', (event) => { window.documentHits.push(event.target.tagName); }, true);\
</script><img src=\"/pixel.png\">",
                ),
            ),
            ("/pixel.png", ok("image/png", PIXEL)),
        ],
        None,
    );
    let server_addr = server.address();

    let (addr, _fixture) = start(Vec::new());
    let created = request(&addr, "POST", "/session", Some("{}"));
    let id = created["value"]["sessionId"]
        .as_str()
        .expect("session id")
        .to_owned();
    let navigated = request(
        &addr,
        "POST",
        &format!("/session/{id}/url"),
        Some(&format!(r#"{{"url":"http://{server_addr}/"}}"#)),
    );
    assert_eq!(navigated["value"], json!(null));

    let hits = request(
        &addr,
        "POST",
        &format!("/session/{id}/execute/sync"),
        Some(
            r#"{"script":"return JSON.stringify({window: window.windowHits, document: window.documentHits})","args":[]}"#,
        ),
    );
    let hits: Value =
        serde_json::from_str(hits["value"].as_str().expect("hits string")).expect("hits json");
    // https://dom.spec.whatwg.org/#get-the-parent
    assert_eq!(
        hits["window"],
        json!(["window"]),
        "image load reached window"
    );
    assert_eq!(
        hits["document"],
        json!(["IMG"]),
        "image load skipped document"
    );
}

#[test]
fn new_window_uses_builder_resolve_map() {
    let server = TestServer::start(
        vec![(
            "/",
            ok("text/html", b"<!doctype html><title>mapped</title>"),
        )],
        None,
    );
    let server_addr = server.address();

    let (addr, _fixture) = start(vec!["--resolve=*.test=127.0.0.1".to_owned()]);

    let created = request(&addr, "POST", "/session", Some("{}"));
    let id = created["value"]["sessionId"]
        .as_str()
        .expect("session id")
        .to_owned();

    let opened = request(
        &addr,
        "POST",
        &format!("/session/{id}/window/new"),
        Some("{}"),
    );
    let handle = opened["value"]["handle"]
        .as_str()
        .expect("handle")
        .to_owned();
    request(
        &addr,
        "POST",
        &format!("/session/{id}/window"),
        Some(&format!(r#"{{"handle":"{handle}"}}"#)),
    );

    let navigated = request(
        &addr,
        "POST",
        &format!("/session/{id}/url"),
        Some(&format!(
            r#"{{"url":"http://web-platform.test:{}/"}}"#,
            server_addr.port()
        )),
    );
    assert_eq!(navigated["value"], json!(null));

    let current = request(&addr, "GET", &format!("/session/{id}/url"), None);
    assert_eq!(
        current["value"].as_str().expect("url"),
        format!("http://web-platform.test:{}/", server_addr.port())
    );
}

#[test]
fn one_session_delete_closes_tabs_close_last_window_invalidates() {
    let (addr, _fixture) = start(Vec::new());

    let created = request(&addr, "POST", "/session", Some("{}"));
    let id = created["value"]["sessionId"]
        .as_str()
        .expect("session id")
        .to_owned();
    let second = request(&addr, "POST", "/session", Some("{}"));
    assert_eq!(second["value"]["error"], json!("session not created"));

    request(
        &addr,
        "POST",
        &format!("/session/{id}/window/new"),
        Some("{}"),
    );
    request(&addr, "DELETE", &format!("/session/{id}"), None);
    let gone = request(&addr, "GET", &format!("/session/{id}/window"), None);
    assert_eq!(gone["value"]["error"], json!("invalid session id"));

    let created = request(&addr, "POST", "/session", Some("{}"));
    let id = created["value"]["sessionId"]
        .as_str()
        .expect("session id")
        .to_owned();
    let handles = request(&addr, "GET", &format!("/session/{id}/window/handles"), None);
    assert_eq!(
        handles["value"].as_array().map(Vec::len),
        Some(1),
        "a new session starts with one window after DELETE closed the old tabs"
    );
    let closed = request(&addr, "DELETE", &format!("/session/{id}/window"), None);
    assert_eq!(closed["value"], json!([]));
    let invalid = request(&addr, "GET", &format!("/session/{id}/window"), None);
    assert_eq!(invalid["value"]["error"], json!("invalid session id"));
}

// https://html.spec.whatwg.org/multipage/window-object.html#dom-window-close
#[test]
fn a_page_close_during_execute_returns() {
    let page = spawn_page("<!doctype html><title>closer</title>");
    let (addr, _fixture) = start(Vec::new());
    let id = create_session(&addr);
    navigate(
        &addr,
        &id,
        &format!("http://127.0.0.1:{}/", page.address().port()),
    );

    let closed = request(
        &addr,
        "POST",
        &format!("/session/{id}/execute/sync"),
        Some(r#"{"script":"window.close(); return 1","args":[]}"#),
    );
    assert_eq!(closed["value"].as_f64(), Some(1.0), "{closed}");
}

// https://w3c.github.io/webdriver/#get-window-handles
// https://w3c.github.io/webdriver/#switch-to-window
#[test]
fn a_script_popup_is_a_switchable_window_handle() {
    let page = b"<!doctype html><title>opener</title>";
    let popup = b"<!doctype html><title>popup</title>";
    let server = TestServer::start(
        vec![
            ("/", ok("text/html", page)),
            ("/popup", ok("text/html", popup)),
        ],
        None,
    );
    let origin = format!("http://{}", server.address());
    let (addr, _fixture) = start(Vec::new());
    let id = create_session(&addr);
    navigate(&addr, &id, &origin);

    let before = request(&addr, "GET", &format!("/session/{id}/window/handles"), None);
    assert_eq!(before["value"].as_array().map(Vec::len), Some(1));
    let current = request(&addr, "GET", &format!("/session/{id}/window"), None);
    let opener = current["value"].as_str().expect("handle").to_owned();

    let popup_url = format!("http://localhost:{}/popup", server.address().port());
    let opened_popup = request(
        &addr,
        "POST",
        &format!("/session/{id}/execute/sync"),
        Some(
            &json!({
                "script": format!("return window.open({popup_url:?}, '_blank') !== null"),
                "args": [],
            })
            .to_string(),
        ),
    );
    assert_eq!(opened_popup["value"], json!(true), "{opened_popup}");

    let handles = wait_for_handles(&addr, &id, 2);
    let popup_handle = handles
        .iter()
        .find(|handle| **handle != opener)
        .cloned()
        .expect("the popup handle");
    let switched = request(
        &addr,
        "POST",
        &format!("/session/{id}/window"),
        Some(&json!({"handle": popup_handle}).to_string()),
    );
    assert_eq!(switched["value"], Value::Null, "{switched}");
    wait_for_url(&addr, &id, &popup_url);

    let closed = request(&addr, "DELETE", &format!("/session/{id}/window"), None);
    assert_eq!(
        closed["value"].as_array().map(Vec::len),
        Some(1),
        "the opener's handle remains: {closed}"
    );
    wait_for_handles(&addr, &id, 1);
}

fn wait_for_handles(addr: &str, id: &str, count: usize) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let reply = request(addr, "GET", &format!("/session/{id}/window/handles"), None);
        let handles: Vec<String> = reply["value"].as_array().map_or_default(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        });
        if handles.len() == count {
            return handles;
        }
        assert!(
            Instant::now() < deadline,
            "the session never had {count} window handles: {reply}"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

fn wait_for_url(addr: &str, id: &str, expected: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let reply = request(addr, "GET", &format!("/session/{id}/url"), None);
        if reply["value"] == json!(expected) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the current window never reached {expected}: {reply}"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

const ELEMENT_KEY: &str = "element-6066-11e4-a52e-4f735466cecf";
fn spawn_page(html: &'static str) -> TestServer {
    TestServer::start(
        vec![("/", ok("text/html; charset=utf-8", html.as_bytes()))],
        None,
    )
}

fn create_session(addr: &str) -> String {
    let created = request(addr, "POST", "/session", Some("{}"));
    created["value"]["sessionId"]
        .as_str()
        .expect("session id")
        .to_owned()
}

fn navigate(addr: &str, id: &str, url: &str) {
    let navigated = request(
        addr,
        "POST",
        &format!("/session/{id}/url"),
        Some(&json!({ "url": url }).to_string()),
    );
    assert_eq!(navigated["value"], Value::Null);
}

fn find(addr: &str, id: &str, selector: &str) -> String {
    let body = json!({"using": "css selector", "value": selector}).to_string();
    let found = request(addr, "POST", &format!("/session/{id}/element"), Some(&body));
    found["value"][ELEMENT_KEY]
        .as_str()
        .expect("element id")
        .to_owned()
}

#[test]
fn element_roundtrip() {
    let page = spawn_page("<!doctype html><button id=b>B</button><input id=i>");
    let page_port = page.address().port();
    let (addr, _fixture) = start(Vec::new());
    let id = create_session(&addr);
    navigate(&addr, &id, &format!("http://127.0.0.1:{page_port}/"));

    let held = request(
        &addr,
        "POST",
        &format!("/session/{id}/execute/sync"),
        Some(
            r#"{"script":"window.__clicks=0; document.getElementById('b').addEventListener('click', e => { window.__clicks += e.isTrusted ? 10 : 1 }); window.__input = document.getElementById('i'); return 1","args":[]}"#,
        ),
    );
    assert_eq!(held["value"].as_f64(), Some(1.0));

    let button = find(&addr, &id, "#b");
    let clicked = request(
        &addr,
        "POST",
        &format!("/session/{id}/element/{button}/click"),
        Some("{}"),
    );
    assert_eq!(clicked["value"], Value::Null);
    let clicks = request(
        &addr,
        "POST",
        &format!("/session/{id}/execute/sync"),
        Some(r#"{"script":"return window.__clicks","args":[]}"#),
    );
    assert_eq!(clicks["value"].as_f64(), Some(10.0));

    let input = find(&addr, &id, "#i");
    let sent = request(
        &addr,
        "POST",
        &format!("/session/{id}/element/{input}/value"),
        Some(r#"{"text":"hi"}"#),
    );
    assert_eq!(sent["value"], Value::Null);
    let value = request(
        &addr,
        "POST",
        &format!("/session/{id}/execute/sync"),
        Some(r#"{"script":"return window.__input.value","args":[]}"#),
    );
    assert_eq!(value["value"], json!("hi"));

    let script_ref = request(
        &addr,
        "POST",
        &format!("/session/{id}/execute/sync"),
        Some(r#"{"script":"return document.getElementById('b')","args":[]}"#),
    );
    let script_ref = script_ref["value"][ELEMENT_KEY]
        .as_str()
        .expect("script element id")
        .to_owned();
    let clicked_again = request(
        &addr,
        "POST",
        &format!("/session/{id}/element/{script_ref}/click"),
        Some("{}"),
    );
    assert_eq!(clicked_again["value"], Value::Null);

    let many = request(
        &addr,
        "POST",
        &format!("/session/{id}/elements"),
        Some(r#"{"using":"css selector","value":"*"}"#),
    );
    assert!(
        many["value"]
            .as_array()
            .is_some_and(|items| items.len() > 1)
    );

    let bad = request(
        &addr,
        "POST",
        &format!("/session/{id}/element"),
        Some(r#"{"using":"css selector","value":"??"}"#),
    );
    assert_eq!(bad["value"]["error"], json!("invalid selector"));

    let rect = request(
        &addr,
        "GET",
        &format!("/session/{id}/element/{button}/rect"),
        None,
    );
    assert!(
        rect["value"]["width"]
            .as_f64()
            .is_some_and(|width| width > 0.0)
    );
}

#[test]
fn cookie_roundtrip() {
    let page = spawn_page("<!doctype html><title>cookies</title>");
    let page_port = page.address().port();
    let (addr, _fixture) = start(Vec::new());
    let id = create_session(&addr);
    navigate(&addr, &id, &format!("http://127.0.0.1:{page_port}/"));

    let script_set = request(
        &addr,
        "POST",
        &format!("/session/{id}/execute/sync"),
        Some(r#"{"script":"document.cookie='a=1; path=/'; return document.cookie","args":[]}"#),
    );
    assert_eq!(script_set["value"], json!("a=1"));
    let named = request(&addr, "GET", &format!("/session/{id}/cookie/a"), None);
    assert_eq!(named["value"]["value"], json!("1"));
    assert_eq!(named["value"]["sameSite"], json!("None"));
    let added = request(
        &addr,
        "POST",
        &format!("/session/{id}/cookie"),
        Some(r#"{"cookie":{"name":"h","value":"2","path":"/","httpOnly":true}}"#),
    );
    assert_eq!(added["value"], Value::Null);
    let http_only = request(&addr, "GET", &format!("/session/{id}/cookie/h"), None);
    assert_eq!(http_only["value"]["httpOnly"], json!(true));
    let all = request(&addr, "GET", &format!("/session/{id}/cookie"), None);
    assert_eq!(all["value"].as_array().map(Vec::len), Some(2));
    let cleared = request(&addr, "DELETE", &format!("/session/{id}/cookie"), None);
    assert_eq!(cleared["value"], Value::Null);
    let empty = request(
        &addr,
        "POST",
        &format!("/session/{id}/execute/sync"),
        Some(r#"{"script":"return document.cookie","args":[]}"#),
    );
    assert_eq!(empty["value"], json!(""));
}

#[test]
fn window_rect_roundtrip() {
    let (addr, _fixture) = start(Vec::new());
    let id = create_session(&addr);

    let initial = request(&addr, "GET", &format!("/session/{id}/window/rect"), None);
    assert_eq!(initial["value"]["width"], json!(800));
    let set = request(
        &addr,
        "POST",
        &format!("/session/{id}/window/rect"),
        Some(r#"{"x":150,"y":175}"#),
    );
    assert_eq!(set["value"]["x"], json!(150));
    assert_eq!(set["value"]["y"], json!(175));
    let got = request(&addr, "GET", &format!("/session/{id}/window/rect"), None);
    assert_eq!(got["value"]["x"], json!(150));
    assert_eq!(got["value"]["y"], json!(175));

    let bad = request(
        &addr,
        "POST",
        &format!("/session/{id}/window/rect"),
        Some(r#"{"width":-1}"#),
    );
    assert_eq!(bad["value"]["error"], json!("invalid argument"));
    let unchanged = request(&addr, "GET", &format!("/session/{id}/window/rect"), None);
    assert_eq!(unchanged["value"]["width"], json!(800));
}

#[test]
fn execute_sync_interrupts_infinite_loop() {
    let (addr, _fixture) = start(Vec::new());
    let created = request(&addr, "POST", "/session", Some("{}"));
    let id = created["value"]["sessionId"]
        .as_str()
        .expect("session id")
        .to_owned();
    request(
        &addr,
        "POST",
        &format!("/session/{id}/timeouts"),
        Some(r#"{"script":200}"#),
    );
    let started = Instant::now();
    let timed_out = request(
        &addr,
        "POST",
        &format!("/session/{id}/execute/sync"),
        Some(r#"{"script":"while(true){}","args":[]}"#),
    );
    assert_eq!(timed_out["value"]["error"], json!("script timeout"));
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "interrupt waited {:?}",
        started.elapsed()
    );
}

#[test]
fn unknown_element_click_and_perform_actions_validation() {
    let (addr, _fixture) = start(Vec::new());
    let created = request(&addr, "POST", "/session", Some("{}"));
    let id = created["value"]["sessionId"]
        .as_str()
        .expect("session id")
        .to_owned();
    let click = request(
        &addr,
        "POST",
        &format!("/session/{id}/element/1/click"),
        Some("{}"),
    );
    assert_eq!(click["value"]["error"], json!("no such element"));
    let actions = request(&addr, "POST", &format!("/session/{id}/actions"), Some("{}"));
    assert_eq!(actions["value"]["error"], json!("invalid argument"));
    let empty = request(
        &addr,
        "POST",
        &format!("/session/{id}/actions"),
        Some(r#"{"actions":[]}"#),
    );
    assert_eq!(empty["value"], Value::Null);
    let released = request(&addr, "DELETE", &format!("/session/{id}/actions"), None);
    assert_eq!(released["value"], Value::Null);
}

type BidiSocket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn bidi_call(
    socket: &mut BidiSocket,
    next_id: &mut u64,
    method: &str,
    params: Value,
) -> Value {
    use futures_util::{SinkExt as _, StreamExt as _};

    let id = *next_id;
    *next_id += 1;
    let payload = json!({"id": id, "method": method, "params": params}).to_string();
    socket
        .send(tokio_tungstenite::tungstenite::Message::text(payload))
        .await
        .expect("send");
    loop {
        let message = socket.next().await.expect("reply").expect("frame");
        if let tokio_tungstenite::tungstenite::Message::Text(text) = message {
            let reply: Value = serde_json::from_str(&text).expect("bidi reply json");
            if reply["id"] == json!(id) {
                return reply;
            }
        }
    }
}

#[test]
fn bidi_session_roundtrip() {
    let (addr, _fixture) = start(Vec::new());
    let created = request(
        &addr,
        "POST",
        "/session",
        Some(r#"{"capabilities":{"alwaysMatch":{"webSocketUrl":true}}}"#),
    );
    let id = created["value"]["sessionId"]
        .as_str()
        .expect("session id")
        .to_owned();
    let url = created["value"]["capabilities"]["webSocketUrl"]
        .as_str()
        .expect("webSocketUrl capability")
        .to_owned();
    assert!(url.ends_with(&id), "{url} must name the session");

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let (mut socket, _response) = tokio_tungstenite::connect_async(&url)
            .await
            .expect("bidi websocket");
        let mut next_id = 1_u64;
        let status = bidi_call(&mut socket, &mut next_id, "session.status", json!({})).await;
        assert_eq!(status["result"]["ready"], json!(false), "{status}");
        assert_eq!(status["result"]["message"], json!("session already started"));

        let tree = bidi_call(&mut socket, &mut next_id, "browsingContext.getTree", json!({})).await;
        let contexts = tree["result"]["contexts"]
            .as_array()
            .expect("context list");
        assert!(!contexts.is_empty(), "{tree}");
        let context = contexts[0]["context"]
            .as_str()
            .expect("context id")
            .to_owned();

        let navigated = bidi_call(
            &mut socket,
            &mut next_id,
            "browsingContext.navigate",
            json!({"context": context, "url": "data:text/html,<title>bidi</title>", "wait": "complete"}),
        )
        .await;
        assert_eq!(
            navigated["result"]["url"],
            json!("data:text/html,<title>bidi</title>")
        );

        let evaluated = bidi_call(
            &mut socket,
            &mut next_id,
            "script.evaluate",
            json!({
                "expression": "document.title",
                "target": {"context": context},
                "awaitPromise": false,
                "resultOwnership": "none",
            }),
        )
        .await;
        assert_eq!(
            evaluated["result"]["result"]["value"],
            json!("bidi"),
            "{evaluated}"
        );
    });
}

#[test]
fn take_screenshot_returns_png_bytes() {
    let (addr, _fixture) = start(Vec::new());
    let created = request(&addr, "POST", "/session", Some("{}"));
    let id = created["value"]["sessionId"]
        .as_str()
        .expect("session id")
        .to_owned();
    let response = send(&addr, "GET", &format!("/session/{id}/screenshot"), None);
    assert_eq!(response.status, 200, "take screenshot status");
    assert_eq!(
        response.content_type.as_deref(),
        Some("application/json; charset=utf-8"),
        "take screenshot must return JSON"
    );
    let shot: Value = serde_json::from_slice(&response.body).expect("json body");
    let encoded = shot["value"].as_str().expect("base64 screenshot string");
    let png = base64_decode(encoded);
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "not a PNG");
    let width = u32::from_be_bytes(png[16..20].try_into().expect("width"));
    let height = u32::from_be_bytes(png[20..24].try_into().expect("height"));
    assert_eq!((width, height), (800, 600), "unexpected viewport capture");
}

fn base64_decode(input: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0u8;
    for byte in input.bytes() {
        let val = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' | b'\n' | b'\r' | b' ' => continue,
            other => panic!("invalid base64 byte: {other}"),
        };
        buf = (buf << 6) | u32::from(val);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buf >> bits) & 0xff) as u8);
        }
    }
    out
}

#[test]
fn renderer_death_fails_a_pending_navigation_and_the_session_recovers() {
    let server = TestServer::start(
        vec![
            (
                "/",
                ok(
                    "text/html",
                    b"<!doctype html><title>held</title><img src=\"/held\">",
                ),
            ),
            ("/held", ok("image/png", PIXEL)),
        ],
        Some("/held"),
    );
    let url = format!("http://{}/", server.address());
    let (addr, mut fixture) = start_in(
        Fixture::new("tinybrowser-webdriver-renderer-death"),
        Vec::new(),
    );
    let daemon = fixture.children()[0].id();

    let created = request(&addr, "POST", "/session", Some("{}"));
    let id = created["value"]["sessionId"]
        .as_str()
        .expect("session id")
        .to_owned();
    let body = json!({"url": url}).to_string();
    let navigate_addr = addr.clone();
    let navigate_id = id.clone();
    let pending = thread::spawn(move || {
        let started = Instant::now();
        let reply = request(
            &navigate_addr,
            "POST",
            &format!("/session/{navigate_id}/url"),
            Some(&body),
        );
        (started.elapsed(), reply)
    });
    server.wait("/held");

    let renderers = renderer_children(daemon);
    assert!(!renderers.is_empty(), "the document has no renderer");
    for pid in renderers {
        let killed = Command::new("kill")
            .args(["-9", &pid.to_string()])
            .status()
            .expect("kill");
        assert!(killed.success(), "SIGKILL renderer {pid}");
    }

    let (elapsed, reply) = pending.join().expect("pending navigation panicked");
    assert_eq!(reply["value"]["error"], json!("unknown error"), "{reply}");
    assert!(
        elapsed < Duration::from_secs(5),
        "pending navigation never failed after the renderer died: {elapsed:?}"
    );

    let crashed_exec = request(
        &addr,
        "POST",
        &format!("/session/{id}/execute/sync"),
        Some(r#"{"script":"return 2+2","args":[]}"#),
    );
    assert_eq!(
        crashed_exec["value"]["error"],
        json!("unknown error"),
        "{crashed_exec}"
    );
    assert!(
        crashed_exec["value"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("crashed")),
        "{crashed_exec}"
    );

    server.release();
    let recovered = request(
        &addr,
        "POST",
        &format!("/session/{id}/url"),
        Some(&json!({"url": format!("http://{}/", server.address())}).to_string()),
    );
    assert_eq!(recovered["value"], Value::Null, "{recovered}");

    let executed = request(
        &addr,
        "POST",
        &format!("/session/{id}/execute/sync"),
        Some(r#"{"script":"return 2+2","args":[]}"#),
    );
    assert_eq!(executed["value"], json!(4.0), "{executed}");
}

#[test]
fn element_click_activates_user() {
    let html = "<!doctype html>\
        <button id=go style='width:80px;height:30px'>go</button>\
        <script>\
        window.activated = null;\
        document.getElementById('go').addEventListener('click', () => {\
            window.activated = navigator.userActivation.isActive && navigator.userActivation.hasBeenActive;\
        });\
        </script>";
    let server = TestServer::start(vec![("/page", ok("text/html", html.as_bytes()))], None);
    let (addr, _fixture) = start(Vec::new());
    let created = request(&addr, "POST", "/session", Some("{}"));
    let id = created["value"]["sessionId"]
        .as_str()
        .expect("session id")
        .to_owned();
    let url = format!("http://{}/page", server.address());
    let nav = request(
        &addr,
        "POST",
        &format!("/session/{id}/url"),
        Some(&json!({"url": url}).to_string()),
    );
    assert_eq!(nav["value"], Value::Null, "{nav}");
    let found = request(
        &addr,
        "POST",
        &format!("/session/{id}/element"),
        Some(r##"{"using":"css selector","value":"#go"}"##),
    );
    let elem = found["value"]["element-6066-11e4-a52e-4f735466cecf"]
        .as_str()
        .expect("element id")
        .to_owned();
    let clicked = request(
        &addr,
        "POST",
        &format!("/session/{id}/element/{elem}/click"),
        Some("{}"),
    );
    assert_eq!(clicked["value"], Value::Null, "{clicked}");
    let checked = request(
        &addr,
        "POST",
        &format!("/session/{id}/execute/sync"),
        Some(r#"{"script":"return window.activated","args":[]}"#),
    );
    assert_eq!(checked["value"], json!(true), "{checked}");
}

#[test]
fn send_keys_preserves_focus_keycode_and_fractional_pointer_move() {
    let html = "<!doctype html>\
        <input id=a><input id=b>\
        <script>\
        window.codes = [];\
        window.addEventListener('keydown', e => window.codes.push([e.key, e.keyCode, document.activeElement.id]));\
        document.getElementById('a').focus();\
        </script>";
    let server = TestServer::start(vec![("/page", ok("text/html", html.as_bytes()))], None);
    let (addr, _fixture) = start(Vec::new());
    let created = request(&addr, "POST", "/session", Some("{}"));
    let id = created["value"]["sessionId"]
        .as_str()
        .expect("session id")
        .to_owned();
    let url = format!("http://{}/page", server.address());
    let nav = request(
        &addr,
        "POST",
        &format!("/session/{id}/url"),
        Some(&json!({"url": url}).to_string()),
    );
    assert_eq!(nav["value"], Value::Null, "{nav}");
    let body = request(
        &addr,
        "POST",
        &format!("/session/{id}/element"),
        Some(r#"{"using":"tag name","value":"body"}"#),
    );
    let body_id = body["value"]["element-6066-11e4-a52e-4f735466cecf"]
        .as_str()
        .expect("body id")
        .to_owned();
    let sent = request(
        &addr,
        "POST",
        &format!("/session/{id}/element/{body_id}/value"),
        Some(&json!({"text": "\u{E004}x"}).to_string()),
    );
    assert_eq!(sent["value"], Value::Null, "{sent}");
    let res = request(
        &addr,
        "POST",
        &format!("/session/{id}/execute/sync"),
        Some(
            r#"{"script":"return [window.codes, document.activeElement.id, document.getElementById('b').value]","args":[]}"#,
        ),
    );
    assert_eq!(res["value"][0][0][0], json!("Tab"), "{res}");
    assert_eq!(res["value"][0][0][1].as_f64(), Some(9.0), "{res}");
    assert_eq!(res["value"][0][0][2], json!("a"), "{res}");
    assert_eq!(res["value"][0][1][0], json!("x"), "{res}");
    assert_eq!(res["value"][0][1][1].as_f64(), Some(88.0), "{res}");
    assert_eq!(res["value"][0][1][2], json!("b"), "{res}");
    assert_eq!(res["value"][1], json!("b"), "{res}");
    assert_eq!(res["value"][2], json!("x"), "{res}");

    let actions = json!({
        "actions": [{
            "type": "pointer",
            "id": "mouse",
            "parameters": {"pointerType": "mouse"},
            "actions": [
                {"type": "pointerMove", "duration": 0, "x": 12.5, "y": 20.25, "width": 1.5, "height": 2.5}
            ]
        }]
    })
    .to_string();
    let moved = request(
        &addr,
        "POST",
        &format!("/session/{id}/actions"),
        Some(&actions),
    );
    assert_eq!(moved["value"], Value::Null, "{moved}");
}

#[test]
fn xpath_locator_and_dom_xpath_api() {
    let html = "<!doctype html>\
        <ul id=list><li class=item>alpha</li><li class=item>beta</li></ul>";
    let server = TestServer::start(vec![("/page", ok("text/html", html.as_bytes()))], None);
    let (addr, _fixture) = start(Vec::new());
    let created = request(&addr, "POST", "/session", Some("{}"));
    let id = created["value"]["sessionId"]
        .as_str()
        .expect("session id")
        .to_owned();
    let url = format!("http://{}/page", server.address());
    let nav = request(
        &addr,
        "POST",
        &format!("/session/{id}/url"),
        Some(&json!({"url": url}).to_string()),
    );
    assert_eq!(nav["value"], Value::Null, "{nav}");

    let found_all = request(
        &addr,
        "POST",
        &format!("/session/{id}/elements"),
        Some(r#"{"using":"xpath","value":"//ul[@id='list']/li[@class='item']"}"#),
    );
    assert_eq!(
        found_all["value"].as_array().map(Vec::len),
        Some(2),
        "{found_all}"
    );

    let found_one = request(
        &addr,
        "POST",
        &format!("/session/{id}/element"),
        Some(r#"{"using":"xpath","value":"//li[text()='beta']"}"#),
    );
    let elem = found_one["value"]["element-6066-11e4-a52e-4f735466cecf"]
        .as_str()
        .expect("element id")
        .to_owned();
    let text = request(
        &addr,
        "GET",
        &format!("/session/{id}/element/{elem}/text"),
        None,
    );
    assert_eq!(text["value"], json!("beta"), "{text}");

    let invalid = request(
        &addr,
        "POST",
        &format!("/session/{id}/element"),
        Some(r#"{"using":"xpath","value":"//li["}"#),
    );
    assert_eq!(
        invalid["value"]["error"],
        json!("invalid selector"),
        "{invalid}"
    );

    let dom_api = request(
        &addr,
        "POST",
        &format!("/session/{id}/execute/sync"),
        Some(
            r#"{"script":"const ev = new XPathEvaluator(); const expr = ev.createExpression('//li', null); const it = expr.evaluate(document, XPathResult.ORDERED_NODE_ITERATOR_TYPE, null); const first = it.iterateNext().textContent; document.getElementById('list').appendChild(document.createElement('li')); let invalidated = it.invalidIteratorState; let threw = ''; try { it.iterateNext(); } catch (e) { threw = e.name; } return [first, invalidated, threw, document.evaluate('count(//li)', document, null, XPathResult.NUMBER_TYPE, null).numberValue];","args":[]}"#,
        ),
    );
    assert_eq!(dom_api["value"][0], json!("alpha"), "{dom_api}");
    assert_eq!(dom_api["value"][1], json!(true), "{dom_api}");
    assert_eq!(dom_api["value"][2], json!("InvalidStateError"), "{dom_api}");
    assert_eq!(dom_api["value"][3].as_f64(), Some(3.0), "{dom_api}");
}
