//! Cookie access from a frame the tab's renderer is not locked to.
//!
//! One renderer hosts every frame of its tab, so a cross-site iframe's document
//! URL is a legitimate claim that still names another site's origin data. The
//! browser answers it with no cookies and keeps the page alive: killing the
//! renderer would let any cross-site iframe take its page down
//! (<https://chromium.googlesource.com/chromium/src/+/main/docs/process_model_and_site_isolation.md#process-locks>).

use crate::fixture::{TestServer, evaluate, ok, open_page, poll_until, server};

/// The frame reports both cookie surfaces at once: `document.cookie` is the
/// synchronous path and `cookieStore` the asynchronous one. It writes through
/// both before reading, so an empty read can only come from the lock and not
/// from a `localhost` jar that was never touched.
const FRAME: &str = "<!doctype html><script>\
document.cookie = 'leak=1; Path=/; Secure; SameSite=None';\
cookieStore.set({name: 'leak2', value: '1', sameSite: 'none'}).catch(() => {}).then(() =>\
  cookieStore.getAll()).then(\
  records => parent.postMessage(JSON.stringify([document.cookie, records.length]), '*'),\
  error => parent.postMessage('rejected:' + error.name, '*'));\
</script>";

#[test]
fn a_cross_site_frame_reads_no_cookies_and_keeps_the_page_alive() {
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
    let answer = poll_until("the frame's cookie answer", || {
        evaluate(&mut client, &session, "window.__answer")
            .as_str()
            .map(str::to_owned)
    });
    assert_eq!(
        answer, r#"["",0]"#,
        "the lock refuses the frame's URL even though its site has cookies"
    );

    evaluate(&mut client, &session, &fetch("/probe"));
    let after = origin.wait_count("/probe", 2)[1].to_ascii_lowercase();
    assert!(after.contains("frame=1"), "control: {after}");
    assert!(
        !after.contains("leak"),
        "a refused frame's writes land nowhere: {after}"
    );
}
