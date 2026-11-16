//! Cookie access from a frame the tab's renderer is not locked to.
//!
//! One renderer hosts every frame of its tab, so a cross-site iframe's document
//! URL is a legitimate claim that still names another site's origin data. The
//! browser answers it with no cookies and keeps the page alive: killing the
//! renderer would let any cross-site iframe take its page down
//! (<https://chromium.googlesource.com/chromium/src/+/main/docs/process_model_and_site_isolation.md#process-locks>).

use crate::fixture::{TestServer, evaluate, ok, open_page, poll_until, server};

/// The frame reports both cookie surfaces at once: `document.cookie` is the
/// synchronous path and `cookieStore.getAll()` the asynchronous one.
const FRAME: &str = "<!doctype html><script>\
cookieStore.getAll().then(\
  records => parent.postMessage(JSON.stringify([document.cookie, records.length]), '*'),\
  error => parent.postMessage('rejected:' + error.name, '*'));\
</script>";

#[test]
fn a_cross_site_frame_reads_no_cookies_and_keeps_the_page_alive() {
    let (listener, address) = server();
    let port = address.port();
    let page = format!(
        r#"<!doctype html><script>
document.cookie = 'page=1; path=/';
window.__answer = null;
window.addEventListener('message', function (event) {{
  window.__answer = String(event.data);
}});
</script><iframe src="http://localhost:{port}/frame"></iframe>"#
    );
    TestServer::serve_on(
        listener,
        vec![
            ("/start", ok("text/html", page.as_bytes())),
            ("/frame", ok("text/html", FRAME.as_bytes())),
        ],
    );
    let (_fixture, mut client, session) = open_page(
        "tinybrowser-cookies-cross-site-frame",
        &format!("http://127.0.0.1:{port}/start"),
    );

    let answer = poll_until("the frame's cookie answer", || {
        evaluate(&mut client, &session, "window.__answer")
            .as_str()
            .map(str::to_owned)
    });
    assert_eq!(
        answer, r#"["",0]"#,
        "a frame outside the renderer's site lock sees no cookies, and the page survives the read"
    );
}
