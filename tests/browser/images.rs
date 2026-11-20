use serde_json::json;

use crate::fixture::{PIXEL, TestServer, evaluate, ok, open_page, poll_until, redirect, server};

// https://html.spec.whatwg.org/multipage/images.html#updating-the-image-data
#[test]
fn an_empty_src_fires_error_and_does_not_hold_load() {
    let page = r#"<!doctype html><body>
<img id="img" src="">
<script>
window.__events = [];
const image = document.getElementById('img');
image.addEventListener('error', () => window.__events.push('error'));
image.addEventListener('load', () => window.__events.push('load'));
</script>
"#;
    let (listener, origin) = server();
    TestServer::serve_on(listener, vec![("/", ok("text/html", page.as_bytes()))]);
    let (_fixture, mut client, session) =
        open_page("tinybrowser-image-empty-src", &format!("http://{origin}/"));
    assert_eq!(
        evaluate(&mut client, &session, "window.__events.join(',')"),
        json!("error")
    );
    assert_eq!(
        evaluate(
            &mut client,
            &session,
            "String(document.getElementById('img').complete)"
        ),
        json!("true")
    );
}

// https://html.spec.whatwg.org/multipage/images.html#reacting-to-dom-mutations
#[test]
fn moving_a_loaded_image_does_not_reload_it() {
    let foreign = TestServer::start(vec![("/pixel.png", ok("image/png", PIXEL))], None);
    let page = format!(
        r#"<!doctype html><body>
<div id="first"></div><div id="second"></div>
<img id="img" src="http://{0}/pixel.png">
<script>
window.__loads = 0;
const image = document.getElementById('img');
image.addEventListener('load', () => window.__loads++);
</script>
"#,
        foreign.address()
    );
    let (listener, origin) = server();
    TestServer::serve_on(listener, vec![("/", ok("text/html", page.as_bytes()))]);
    let (_fixture, mut client, session) =
        open_page("tinybrowser-image-move", &format!("http://{origin}/"));
    poll_until("the first load", || {
        (evaluate(&mut client, &session, "window.__loads") == json!(1)).then_some(())
    });

    evaluate(
        &mut client,
        &session,
        "document.getElementById('second').append(document.getElementById('img'))",
    );
    assert_eq!(evaluate(&mut client, &session, "window.__loads"), json!(1));
    assert_eq!(
        foreign.history("/pixel.png").len(),
        1,
        "a move must not dial again"
    );
}

// https://html.spec.whatwg.org/multipage/embedded-content.html#dom-img-currentsrc
// https://html.spec.whatwg.org/multipage/images.html#updating-the-image-data
#[test]
fn current_src_follows_the_response_redirect() {
    let page = r#"<!doctype html><body>
<img id="img" src="/first.png">
<script>
window.__current = "";
const image = document.getElementById('img');
image.addEventListener('load', () => window.__current = image.currentSrc);
</script>
"#;
    let server = TestServer::start(
        vec![
            ("/", ok("text/html", page.as_bytes())),
            ("/first.png", redirect("/final.png")),
            ("/final.png", ok("image/png", PIXEL)),
        ],
        None,
    );
    let origin = server.address();
    let (_fixture, mut client, session) =
        open_page("tinybrowser-image-currentsrc", &format!("http://{origin}/"));
    poll_until("the redirected load", || {
        (evaluate(&mut client, &session, "window.__current") != json!("")).then_some(())
    });
    assert_eq!(
        evaluate(&mut client, &session, "window.__current"),
        json!(format!("http://{origin}/final.png")),
        "currentSrc must report the response URL, not the requested one"
    );
}

// https://html.spec.whatwg.org/multipage/images.html#updating-the-image-data
#[test]
fn reassigning_the_same_source_fires_one_load() {
    let page = r#"<!doctype html><body><img id="img">"#;
    let server = TestServer::start(
        vec![
            ("/", ok("text/html", page.as_bytes())),
            ("/slow.png", ok("image/png", PIXEL)),
        ],
        Some("/slow.png"),
    );
    let origin = server.address();
    let (_fixture, mut client, session) = open_page(
        "tinybrowser-image-same-source",
        &format!("http://{origin}/"),
    );
    evaluate(
        &mut client,
        &session,
        "window.__loads = 0; const image = document.getElementById('img'); image.addEventListener('load', () => window.__loads++); image.src = '/slow.png'; image.src = image.src;",
    );
    server.wait("/slow.png");
    server.release();
    poll_until("the load", || {
        (evaluate(&mut client, &session, "window.__loads") == json!(1)).then_some(())
    });
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert_eq!(evaluate(&mut client, &session, "window.__loads"), json!(1));
}

// https://html.spec.whatwg.org/multipage/embedded-content.html#dom-img-complete
// https://html.spec.whatwg.org/multipage/images.html#updating-the-image-data
#[test]
fn complete_reads_the_source_and_the_live_request() {
    let page = r#"<!doctype html><body><img id="img"><script>
window.__errors = 0;
document.getElementById('img').addEventListener('error', () => window.__errors++);
</script>"#;
    let server = TestServer::start(
        vec![
            ("/", ok("text/html", page.as_bytes())),
            ("/slow.png", ok("image/png", PIXEL)),
        ],
        None,
    );
    let origin = server.address();
    let (_fixture, mut client, session) =
        open_page("tinybrowser-image-complete", &format!("http://{origin}/"));
    let set = |client: &mut cdp::Client, session: &str, script: &str| {
        evaluate(
            client,
            session,
            &format!(
                "(() => {{ const image = document.getElementById('img'); {script} return String(image.complete); }})()"
            ),
        )
    };

    assert_eq!(
        set(&mut client, &session, "image.src = '/slow.png';"),
        json!("false")
    );
    assert_eq!(
        set(&mut client, &session, "image.removeAttribute('src');"),
        json!("true")
    );
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert_eq!(
        evaluate(&mut client, &session, "window.__errors"),
        json!(0),
        "aborting a source-less update fires no error"
    );

    assert_eq!(
        set(&mut client, &session, "image.src = 'http://malformed:url';"),
        json!("false")
    );
    poll_until("the malformed failure", || {
        (evaluate(&mut client, &session, "window.__errors") == json!(1)).then_some(())
    });
    assert_eq!(
        evaluate(
            &mut client,
            &session,
            "String(document.getElementById('img').complete)"
        ),
        json!("true")
    );

    assert_eq!(set(&mut client, &session, "image.src = '';"), json!("true"));
}

// https://html.spec.whatwg.org/multipage/images.html#updating-the-image-data
// https://html.spec.whatwg.org/multipage/images.html#parsing-a-srcset-attribute
#[test]
fn an_unselectable_source_set_does_not_hold_the_document_load() {
    let page = r#"<!doctype html><body>
<img id="set"><img id="slow">
<script>
document.getElementById('set').srcset = '/pixel.png 1w';
document.getElementById('slow').src = '/slow.png';
</script>"#;
    let (listener, origin) = server();
    TestServer::serve_on_after(
        listener,
        vec![
            ("/", ok("text/html", page.as_bytes())),
            ("/pixel.png", ok("image/png", PIXEL)),
            ("/slow.png", ok("image/png", PIXEL)),
        ],
        std::time::Duration::from_millis(300),
    );
    let (_fixture, _client, _session) = open_page(
        "tinybrowser-image-srcset-load",
        &format!("http://{origin}/"),
    );
}

// https://html.spec.whatwg.org/multipage/images.html#selecting-an-image-source
#[test]
fn srcset_selects_the_candidate_for_the_device_pixel_ratio() {
    let page = r#"<!doctype html><body>
<img id="set"><img id="fallback">
<script>
window.__current = "";
const set = document.getElementById('set');
set.addEventListener('load', () => window.__current = set.currentSrc);
set.srcset = '/one.png 1x, /two.png 2x';
const fallback = document.getElementById('fallback');
fallback.addEventListener('load', () => window.__fallback = fallback.currentSrc);
fallback.src = '/three.png';
fallback.srcset = '/four.png 2x';
</script>"#;
    let server = TestServer::start(
        vec![
            ("/", ok("text/html", page.as_bytes())),
            ("/one.png", ok("image/png", PIXEL)),
            ("/two.png", ok("image/png", PIXEL)),
            ("/three.png", ok("image/png", PIXEL)),
            ("/four.png", ok("image/png", PIXEL)),
        ],
        None,
    );
    let origin = server.address();
    let (_fixture, mut client, session) =
        open_page("tinybrowser-image-srcset", &format!("http://{origin}/"));
    poll_until("both selected sources to load", || {
        (evaluate(&mut client, &session, "window.__current") != json!("")
            && evaluate(&mut client, &session, "window.__fallback") != json!(""))
        .then_some(())
    });
    assert_eq!(
        evaluate(&mut client, &session, "window.__current"),
        json!(format!("http://{origin}/one.png")),
        "the 1x candidate wins at a device pixel ratio of 1"
    );
    assert_eq!(
        evaluate(&mut client, &session, "window.__fallback"),
        json!(format!("http://{origin}/three.png")),
        "src is the 1x fallback when the set has no 1x entry"
    );
}

// https://html.spec.whatwg.org/multipage/images.html#updating-the-image-data
#[test]
fn a_stale_source_failure_does_not_fail_the_next_request() {
    let page = r"<!doctype html><body><script>
window.__events = [];
const image = document.createElement('img');
image.addEventListener('load', () => window.__events.push('load'));
image.addEventListener('error', () => window.__events.push('error'));
image.src = 'http://';
image.src = '/pixel.png';
document.body.append(image);
</script>";
    let server = TestServer::start(
        vec![
            ("/", ok("text/html", page.as_bytes())),
            ("/pixel.png", ok("image/png", PIXEL)),
        ],
        None,
    );
    let origin = server.address();
    let (_fixture, mut client, session) =
        open_page("tinybrowser-image-stale", &format!("http://{origin}/"));
    poll_until("the surviving request's load", || {
        (evaluate(&mut client, &session, "window.__events.length") == json!(1)).then_some(())
    });
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert_eq!(
        evaluate(&mut client, &session, "window.__events.join(',')"),
        json!("load"),
        "the malformed source's failure must not fail the request that replaced it"
    );
}
