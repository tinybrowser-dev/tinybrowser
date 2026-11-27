// https://chromium.googlesource.com/chromium/src/+/main/docs/process_model_and_site_isolation.md#process-locks

use crate::fixture::{TestServer, evaluate, ok, open_page, poll_until, server};

const FRAME: &str = "<!doctype html><script>\
localStorage.setItem('cross_frame', 'ok');\
document.cookie = 'frame_write=1; Path=/; Secure; SameSite=None';\
cookieStore.set({name: 'store_write', value: '1', sameSite: 'none'}).then(() =>\
  cookieStore.getAll()).then(\
  records => parent.postMessage(JSON.stringify([\
    document.cookie, records.length, localStorage.getItem('cross_frame')\
  ]), '*'),\
  error => parent.postMessage('rejected:' + error.name, '*'));\
</script>";

const SANDBOXED_FRAME: &str = "<!doctype html><script>\
let cookieErr = 'none';\
try { void document.cookie; } catch (e) { cookieErr = e.name; }\
parent.postMessage(cookieErr, '*');\
</script>";

#[test]
fn a_committed_cross_site_frame_reads_and_writes_its_own_cookies_and_storage() {
    let (listener, address) = server();
    let port = address.port();
    let page = "<!doctype html><script>window.__answer = null;\
window.addEventListener('message', e => { window.__answer = String(e.data); });</script>";
    let origin = TestServer::serve_on(
        listener,
        vec![
            ("/start", ok("text/html", page.as_bytes())),
            (
                "/seed",
                ok("text/plain", b"ok")
                    .header("Set-Cookie", "frame=1; Path=/; Secure; SameSite=None"),
            ),
            ("/frame", ok("text/html", FRAME.as_bytes())),
            ("/probe", ok("text/plain", b"ok")),
        ],
    );
    let (_fixture, mut client, session) = open_page(
        "tinybrowser-cookies-cross-site-frame",
        &format!("http://127.0.0.1:{port}/start"),
    );
    let fetch = |path: &str| {
        format!(
            "fetch('http://localhost:{port}{path}', {{credentials: 'include'}}).then(r => r.status)"
        )
    };
    assert_eq!(
        evaluate(&mut client, &session, &fetch("/seed")),
        serde_json::json!(200)
    );
    evaluate(&mut client, &session, &fetch("/probe"));
    assert!(
        origin
            .wait("/probe")
            .to_ascii_lowercase()
            .contains("cookie: frame=1"),
        "control: localhost has a cookie"
    );

    evaluate(
        &mut client,
        &session,
        &format!(
            "document.body.appendChild(Object.assign(document.createElement('iframe'), {{src: 'http://localhost:{port}/frame'}})); 1"
        ),
    );
    let answer = poll_until("the frame's cookie and storage answer", || {
        evaluate(&mut client, &session, "window.__answer")
            .as_str()
            .map(str::to_owned)
    });
    let parsed: serde_json::Value = serde_json::from_str(&answer).expect("json array");
    let cookie_str = parsed[0].as_str().expect("cookie string");
    assert!(cookie_str.contains("frame=1"), "{answer}");
    assert!(cookie_str.contains("frame_write=1"), "{answer}");
    assert!(
        parsed[1].as_u64().unwrap_or(0) >= 3,
        "cookieStore sees seeded + written cookies: {answer}"
    );
    assert_eq!(parsed[2], serde_json::json!("ok"), "{answer}");

    evaluate(&mut client, &session, &fetch("/probe"));
    let after = origin.wait_count("/probe", 2)[1].to_ascii_lowercase();
    assert!(after.contains("frame=1"), "control: {after}");
    assert!(after.contains("frame_write=1"), "{after}");
    assert!(after.contains("store_write=1"), "{after}");
}

#[test]
fn a_sandboxed_cross_site_frame_gets_security_error_on_cookie_access() {
    let (listener, address) = server();
    let port = address.port();
    let page = "<!doctype html><script>window.__answer = null;\
window.addEventListener('message', e => { window.__answer = String(e.data); });</script>";
    let _origin = TestServer::serve_on(
        listener,
        vec![
            ("/start", ok("text/html", page.as_bytes())),
            ("/sandboxed", ok("text/html", SANDBOXED_FRAME.as_bytes())),
        ],
    );
    let (_fixture, mut client, session) = open_page(
        "tinybrowser-cookies-sandboxed-frame",
        &format!("http://127.0.0.1:{port}/start"),
    );
    evaluate(
        &mut client,
        &session,
        &format!(
            "const f = document.createElement('iframe'); f.setAttribute('sandbox', 'allow-scripts'); f.src = 'http://localhost:{port}/sandboxed'; document.body.appendChild(f); 1"
        ),
    );
    let answer = poll_until("sandboxed cookie error", || {
        evaluate(&mut client, &session, "window.__answer")
            .as_str()
            .map(str::to_owned)
    });
    assert_eq!(answer, "SecurityError");
}
