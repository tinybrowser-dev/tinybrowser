use serde_json::json;

use crate::fixture::{PIXEL, TestServer, evaluate, ok, open_page, poll_until, server};

const COUNT_SETTLEMENTS: &str = "
window.__loaded = 0; window.__errors = 0; window.__widths = []; window.__counted = [];
const images = document.getElementsByTagName('img');
for (let index = 0; index < images.length; index++) {
  const image = images[index];
  const settle = ok => {
    if (window.__counted[index]) return;
    window.__counted[index] = true;
    if (ok) { window.__loaded++; window.__widths.push(image.naturalWidth); }
    else window.__errors++;
  };
  image.addEventListener('load', () => settle(true));
  image.addEventListener('error', () => settle(false));
  if (image.complete) settle(image.naturalWidth > 0);
}
";

// https://html.spec.whatwg.org/multipage/canvas.html#the-image-argument
const CANVAS_TAINT: &str = "((...ids) => {
  const read = id => {
    const canvas = document.createElement('canvas');
    canvas.width = 1; canvas.height = 1;
    const context = canvas.getContext('2d');
    context.drawImage(document.getElementById(id), 0, 0);
    try { context.getImageData(0, 0, 1, 1); return 'clean'; }
    catch (error) { return error.name; }
  };
  return ids.map(read).join(',');
})";

fn wait_settled(client: &mut cdp::Client, session: &str, images: u64) {
    poll_until(&format!("{images} image settlements"), || {
        let loaded = evaluate(
            client,
            session,
            "window.__loaded === undefined ? -1 : window.__loaded + window.__errors",
        );
        (loaded == json!(images)).then_some(())
    });
}

// https://html.spec.whatwg.org/multipage/urls-and-fetching.html#cors-settings-attributes
#[test]
fn a_padded_crossorigin_image_is_a_cors_request_without_credentials() {
    let foreign = TestServer::start(
        vec![
            ("/padded.png", ok("image/png", PIXEL)),
            ("/credentials.png", ok("image/png", PIXEL)),
        ],
        None,
    );
    let page = format!(
        r#"<!doctype html><body>
<script>document.cookie = 'session=1; path=/'</script>
<img src="http://{0}/padded.png" crossorigin=" use-credentials">
<img src="http://{0}/credentials.png" crossorigin="use-credentials">
<script>{COUNT_SETTLEMENTS}</script>
"#,
        foreign.address()
    );
    let (listener, origin) = server();
    TestServer::serve_on(listener, vec![("/", ok("text/html", page.as_bytes()))]);
    let (_fixture, mut client, session) =
        open_page("tinybrowser-crossorigin", &format!("http://{origin}/"));
    wait_settled(&mut client, &session, 2);
    assert_eq!(evaluate(&mut client, &session, "window.__errors"), json!(0));

    let padded = foreign.wait("/padded.png").to_ascii_lowercase();
    assert!(padded.contains("sec-fetch-mode: cors"), "{padded}");
    assert!(
        padded.contains(&format!("origin: http://{origin}")),
        "{padded}"
    );
    assert!(
        !padded.contains("cookie:"),
        "an invalid value must not send credentials: {padded}"
    );

    let credentialed = foreign.wait("/credentials.png").to_ascii_lowercase();
    assert!(
        credentialed.contains("sec-fetch-mode: cors"),
        "{credentialed}"
    );
    assert!(
        credentialed.contains(&format!("origin: http://{origin}")),
        "{credentialed}"
    );
    assert!(
        credentialed.contains("cookie: session=1"),
        "use-credentials admits the cookie: {credentialed}"
    );
}

// https://html.spec.whatwg.org/multipage/semantics.html#create-a-link-request
#[test]
fn a_link_stylesheet_crossorigin_is_a_cors_request() {
    let foreign = TestServer::start(
        vec![("/style.css", ok("text/css", b"p { color: rgb(1, 2, 3); }"))],
        None,
    );
    let page = format!(
        r#"<!doctype html><body>
<script>document.cookie = 'session=1; path=/'</script>
<link rel="stylesheet" href="http://{}/style.css" crossorigin="anonymous">
<p id="p">x</p>
"#,
        foreign.address()
    );
    let (listener, origin) = server();
    TestServer::serve_on(listener, vec![("/", ok("text/html", page.as_bytes()))]);
    let (_fixture, mut client, session) =
        open_page("tinybrowser-crossorigin-link", &format!("http://{origin}/"));

    let head = foreign.wait("/style.css").to_ascii_lowercase();
    assert!(head.contains("sec-fetch-mode: cors"), "{head}");
    assert!(head.contains(&format!("origin: http://{origin}")), "{head}");
    assert!(
        !head.contains("cookie:"),
        "anonymous must not send credentials: {head}"
    );
    assert_eq!(
        evaluate(
            &mut client,
            &session,
            "getComputedStyle(document.getElementById('p')).color"
        ),
        json!("rgb(1, 2, 3)")
    );
}

// https://fetch.spec.whatwg.org/#cors-check
#[test]
fn a_link_stylesheet_crossorigin_without_cors_approval_does_not_apply() {
    let foreign = TestServer::start_without_cors(vec![(
        "/style.css",
        ok("text/css", b"p { color: rgb(1, 2, 3); }"),
    )]);
    let page = format!(
        r#"<!doctype html><body>
<script>window.__errors = 0</script>
<link rel="stylesheet" href="http://{}/style.css" crossorigin="anonymous"
      onerror="window.__errors++">
<p id="p">x</p>
"#,
        foreign.address()
    );
    let (listener, origin) = server();
    TestServer::serve_on(listener, vec![("/", ok("text/html", page.as_bytes()))]);
    let (_fixture, mut client, session) = open_page(
        "tinybrowser-crossorigin-link-refused",
        &format!("http://{origin}/"),
    );

    poll_until("the sheet's error event", || {
        (evaluate(&mut client, &session, "window.__errors") == json!(1)).then_some(())
    });
    assert_ne!(
        evaluate(
            &mut client,
            &session,
            "getComputedStyle(document.getElementById('p')).color"
        ),
        json!("rgb(1, 2, 3)")
    );
}

// https://html.spec.whatwg.org/multipage/urls-and-fetching.html#cors-settings-attributes
#[test]
fn two_images_on_one_url_keep_their_own_crossorigin() {
    let foreign = TestServer::start(vec![("/shared.png", ok("image/png", PIXEL))], None);
    let page = format!(
        r#"<!doctype html><body>
<script>document.cookie = 'session=1; path=/'</script>
<img src="http://{0}/shared.png" crossorigin="anonymous">
<img src="http://{0}/shared.png" crossorigin="use-credentials">
<script>{COUNT_SETTLEMENTS}</script>
"#,
        foreign.address()
    );
    let (listener, origin) = server();
    TestServer::serve_on(listener, vec![("/", ok("text/html", page.as_bytes()))]);
    let (_fixture, mut client, session) = open_page(
        "tinybrowser-crossorigin-shared",
        &format!("http://{origin}/"),
    );
    wait_settled(&mut client, &session, 2);
    assert_eq!(evaluate(&mut client, &session, "window.__loaded"), json!(2));
    assert_eq!(evaluate(&mut client, &session, "window.__errors"), json!(0));
    assert_eq!(
        evaluate(&mut client, &session, "window.__widths.join(',')"),
        json!("1,1")
    );

    let requests = foreign.wait_count("/shared.png", 2);
    assert_eq!(requests.len(), 2, "one request per CORS state");
    assert!(
        requests
            .iter()
            .any(|head| !head.to_ascii_lowercase().contains("cookie:")),
        "the anonymous request must not send the cookie"
    );
    assert!(
        requests
            .iter()
            .any(|head| head.to_ascii_lowercase().contains("cookie: session=1")),
        "the credentialed request must send the cookie"
    );
}

// https://html.spec.whatwg.org/multipage/urls-and-fetching.html#cors-settings-attributes
#[test]
fn one_cors_state_is_one_request() {
    let foreign = TestServer::start(vec![("/canon.png", ok("image/png", PIXEL))], None);
    let page = format!(
        r#"<!doctype html><body>
<img src="http://{0}/canon.png" crossorigin="">
<img src="http://{0}/canon.png" crossorigin="ANONYMOUS">
<script>{COUNT_SETTLEMENTS}</script>
"#,
        foreign.address()
    );
    let (listener, origin) = server();
    TestServer::serve_on(listener, vec![("/", ok("text/html", page.as_bytes()))]);
    let (_fixture, mut client, session) = open_page(
        "tinybrowser-crossorigin-canonical",
        &format!("http://{origin}/"),
    );
    wait_settled(&mut client, &session, 2);
    assert_eq!(evaluate(&mut client, &session, "window.__loaded"), json!(2));
    let requests = foreign.history("/canon.png");
    assert_eq!(
        requests.len(),
        1,
        "one CORS state is one request: {requests:?}"
    );
}

// https://html.spec.whatwg.org/multipage/canvas.html#the-image-argument
#[test]
fn a_no_cors_image_on_a_cors_images_url_still_taints() {
    let foreign = TestServer::start(vec![("/pixel.png", ok("image/png", PIXEL))], None);
    let page = format!(
        r#"<!doctype html><body>
<img id="cors" src="http://{0}/pixel.png" crossorigin="anonymous">
<img id="plain" src="http://{0}/pixel.png">
<script>{COUNT_SETTLEMENTS}</script>
"#,
        foreign.address()
    );
    let (listener, origin) = server();
    TestServer::serve_on(listener, vec![("/", ok("text/html", page.as_bytes()))]);
    let (_fixture, mut client, session) = open_page(
        "tinybrowser-crossorigin-taint",
        &format!("http://{origin}/"),
    );
    wait_settled(&mut client, &session, 2);
    let outcome = evaluate(
        &mut client,
        &session,
        &format!("({CANVAS_TAINT})('cors', 'plain')"),
    );
    assert_eq!(outcome, json!("clean,SecurityError"));
}

// https://html.spec.whatwg.org/multipage/canvas.html#the-image-argument
#[test]
fn a_cached_no_cors_image_does_not_inherit_a_cors_images_clean_state() {
    let foreign = TestServer::start(vec![("/pixel.png", ok("image/png", PIXEL))], None);
    let page = format!(
        r#"<!doctype html><body>
<img id="a" src="http://{0}/pixel.png">
<img id="b" src="http://{0}/pixel.png" crossorigin="anonymous">
<script>{COUNT_SETTLEMENTS}</script>
<script>
window.__insertPlain = () => {{
  const image = document.createElement('img');
  image.id = 'c';
  image.src = 'http://{0}/pixel.png';
  document.body.append(image);
}};
</script>
"#,
        foreign.address()
    );
    let (listener, origin) = server();
    TestServer::serve_on(listener, vec![("/", ok("text/html", page.as_bytes()))]);
    let (_fixture, mut client, session) = open_page(
        "tinybrowser-crossorigin-taint-cache",
        &format!("http://{origin}/"),
    );
    wait_settled(&mut client, &session, 2);
    evaluate(&mut client, &session, "window.__insertPlain()");
    poll_until("the cached image to settle", || {
        (evaluate(
            &mut client,
            &session,
            "document.getElementById('c').complete",
        ) == json!(true))
        .then_some(())
    });
    let outcome = evaluate(
        &mut client,
        &session,
        &format!("({CANVAS_TAINT})('b', 'c')"),
    );
    assert_eq!(outcome, json!("clean,SecurityError"));
}

// https://html.spec.whatwg.org/multipage/images.html#reacting-to-dom-mutations
#[test]
fn a_crossorigin_write_starts_a_new_request() {
    let foreign = TestServer::start(vec![("/reload.png", ok("image/png", PIXEL))], None);
    let page = format!(
        r#"<!doctype html><body>
<script>document.cookie = 'session=1; path=/'</script>
<img id="img" src="http://{}/reload.png">
"#,
        foreign.address()
    );
    let (listener, origin) = server();
    TestServer::serve_on(listener, vec![("/", ok("text/html", page.as_bytes()))]);
    let (_fixture, mut client, session) = open_page(
        "tinybrowser-crossorigin-reload",
        &format!("http://{origin}/"),
    );

    let first = foreign.wait("/reload.png").to_ascii_lowercase();
    assert!(
        !first.contains("sec-fetch-mode: cors"),
        "the first request must be no-cors: {first}"
    );
    evaluate(
        &mut client,
        &session,
        "document.getElementById('img').crossOrigin = 'use-credentials'",
    );
    let requests = foreign.wait_count("/reload.png", 2);
    let second = requests
        .last()
        .expect("second request")
        .to_ascii_lowercase();
    assert!(
        second.contains("sec-fetch-mode: cors"),
        "the new request must be CORS: {second}"
    );
    assert!(
        second.contains(&format!("origin: http://{origin}")),
        "{second}"
    );
    assert!(
        second.contains("cookie: session=1"),
        "use-credentials admits the cookie: {second}"
    );
}

// https://html.spec.whatwg.org/multipage/images.html#reacting-to-dom-mutations
#[test]
fn a_same_state_crossorigin_write_keeps_the_request() {
    let foreign = TestServer::start(vec![("/same.png", ok("image/png", PIXEL))], None);
    let page = format!(
        r#"<!doctype html><body>
<img id="img" src="http://{0}/same.png" crossorigin="">
<script>{COUNT_SETTLEMENTS}</script>
"#,
        foreign.address()
    );
    let (listener, origin) = server();
    TestServer::serve_on(listener, vec![("/", ok("text/html", page.as_bytes()))]);
    let (_fixture, mut client, session) =
        open_page("tinybrowser-crossorigin-same", &format!("http://{origin}/"));
    wait_settled(&mut client, &session, 1);

    evaluate(
        &mut client,
        &session,
        "document.getElementById('img').crossOrigin = 'ANONYMOUS'",
    );
    assert_eq!(
        evaluate(
            &mut client,
            &session,
            "document.getElementById('img').complete"
        ),
        json!(true),
        "the settled request must stay settled"
    );
    let requests = foreign.history("/same.png");
    assert_eq!(
        requests.len(),
        1,
        "a same-state write must not refetch: {requests:?}"
    );
}

// https://html.spec.whatwg.org/multipage/images.html#update-the-image-data
#[test]
fn a_repeated_src_answered_from_cache_fires_load() {
    let foreign = TestServer::start(vec![("/cached.png", ok("image/png", PIXEL))], None);
    let page = format!(
        r#"<!doctype html><body>
<img id="img" src="http://{0}/cached.png">
<script>
window.__loads = 0;
document.getElementById('img').addEventListener('load', () => window.__loads++);
</script>
"#,
        foreign.address()
    );
    let (listener, origin) = server();
    TestServer::serve_on(listener, vec![("/", ok("text/html", page.as_bytes()))]);
    let (_fixture, mut client, session) = open_page(
        "tinybrowser-crossorigin-cached",
        &format!("http://{origin}/"),
    );
    poll_until("the first load", || {
        (evaluate(&mut client, &session, "window.__loads") == json!(1)).then_some(())
    });

    evaluate(
        &mut client,
        &session,
        "document.getElementById('img').src = document.getElementById('img').src",
    );
    poll_until("the cached load", || {
        (evaluate(&mut client, &session, "window.__loads") == json!(2)).then_some(())
    });
    assert_eq!(
        foreign.history("/cached.png").len(),
        1,
        "the cache answer must not dial again"
    );
}

// https://html.spec.whatwg.org/multipage/images.html#updating-the-image-data
#[test]
fn a_failed_cors_request_loads_after_crossorigin_is_removed() {
    let foreign = TestServer::start_without_cors(vec![("/pixel.png", ok("image/png", PIXEL))]);
    let page = format!(
        r#"<!doctype html><body>
<img id="img" src="http://{0}/pixel.png" crossorigin="anonymous">
<script>
window.__errors = 0; window.__loaded = 0;
const image = document.getElementById('img');
image.addEventListener('error', () => window.__errors++);
image.addEventListener('load', () => window.__loaded++);
</script>
"#,
        foreign.address()
    );
    let (listener, origin) = server();
    TestServer::serve_on(listener, vec![("/", ok("text/html", page.as_bytes()))]);
    let (_fixture, mut client, session) = open_page(
        "tinybrowser-crossorigin-recover",
        &format!("http://{origin}/"),
    );
    poll_until("the CORS failure", || {
        (evaluate(&mut client, &session, "window.__errors") == json!(1)).then_some(())
    });

    evaluate(
        &mut client,
        &session,
        "document.getElementById('img').removeAttribute('crossorigin')",
    );
    poll_until("the no-cors load", || {
        (evaluate(&mut client, &session, "window.__loaded") == json!(1)).then_some(())
    });
    assert_eq!(
        evaluate(
            &mut client,
            &session,
            "document.getElementById('img').naturalWidth"
        ),
        json!(1)
    );
    let requests = foreign.wait_count("/pixel.png", 2);
    assert!(
        requests[0]
            .to_ascii_lowercase()
            .contains("sec-fetch-mode: cors"),
        "the first request is CORS: {requests:?}"
    );
    assert!(
        !requests[1]
            .to_ascii_lowercase()
            .contains("sec-fetch-mode: cors"),
        "the recovered request is no-cors: {requests:?}"
    );
}

// https://html.spec.whatwg.org/multipage/links.html#link-type-stylesheet
#[test]
fn a_link_crossorigin_write_refetches_the_sheet() {
    let foreign = TestServer::start(
        vec![("/style.css", ok("text/css", b"p { color: rgb(1, 2, 3); }"))],
        None,
    );
    let page = format!(
        r#"<!doctype html><body>
<script>document.cookie = 'session=1; path=/'</script>
<link id="sheet" rel="stylesheet" href="http://{}/style.css" crossorigin="anonymous">
<p id="p">x</p>
"#,
        foreign.address()
    );
    let (listener, origin) = server();
    TestServer::serve_on(listener, vec![("/", ok("text/html", page.as_bytes()))]);
    let (_fixture, mut client, session) = open_page(
        "tinybrowser-crossorigin-link-reload",
        &format!("http://{origin}/"),
    );
    let first = foreign.wait("/style.css").to_ascii_lowercase();
    assert!(!first.contains("cookie:"), "{first}");

    evaluate(
        &mut client,
        &session,
        "document.getElementById('sheet').crossOrigin = 'use-credentials'",
    );
    let requests = foreign.wait_count("/style.css", 2);
    let second = requests
        .last()
        .expect("second request")
        .to_ascii_lowercase();
    assert!(
        second.contains("cookie: session=1"),
        "the new sheet request uses the new credentials: {second}"
    );
}

fn referer(head: &str) -> Option<&str> {
    head.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("referer")
            .then(|| value.trim_start())
    })
}

// https://html.spec.whatwg.org/multipage/urls-and-fetching.html#referrer-policy-attribute
#[test]
fn each_referrerpolicy_state_reaches_the_dial() {
    let foreign = TestServer::start(
        vec![
            ("/unsafe.png", ok("image/png", PIXEL)),
            ("/none.png", ok("image/png", PIXEL)),
            ("/meta.png", ok("image/png", PIXEL)),
        ],
        None,
    );
    let page = format!(
        r#"<!doctype html><head><meta name="referrer" content="origin"></head><body>
<img src="http://{0}/unsafe.png" referrerpolicy="unsafe-url">
<img src="http://{0}/none.png" referrerpolicy="no-referrer">
<img src="http://{0}/meta.png">
</body>"#,
        foreign.address()
    );
    let (listener, origin) = server();
    TestServer::serve_on(
        listener,
        vec![("/page/index.html", ok("text/html", page.as_bytes()))],
    );
    let (_fixture, _client, _session) = open_page(
        "tinybrowser-referrer-states",
        &format!("http://{origin}/page/index.html"),
    );

    let full = format!("http://{origin}/page/index.html");
    let origin_only = format!("http://{origin}/");
    let unsafe_head = foreign.wait("/unsafe.png");
    assert_eq!(
        referer(&unsafe_head),
        Some(full.as_str()),
        "unsafe-url sends the full URL: {unsafe_head}"
    );
    let none_head = foreign.wait("/none.png");
    assert_eq!(
        referer(&none_head),
        None,
        "no-referrer sends no Referer at all: {none_head}"
    );
    let meta_head = foreign.wait("/meta.png");
    assert_eq!(
        referer(&meta_head),
        Some(origin_only.as_str()),
        "the document's meta fallback decides an element with no attribute: {meta_head}"
    );
}

// https://html.spec.whatwg.org/multipage/images.html#reacting-to-dom-mutations
#[test]
fn a_referrerpolicy_write_that_keeps_the_state_does_not_restart() {
    let foreign = TestServer::start(
        vec![
            ("/named.png", ok("image/png", PIXEL)),
            ("/invalid.png", ok("image/png", PIXEL)),
        ],
        None,
    );
    let page = format!(
        r#"<!doctype html><body>
<img id="named" referrerpolicy="no-referrer" src="http://{0}/named.png">
<img id="invalid" referrerpolicy="bogus" src="http://{0}/invalid.png">
<script>
window.__loads = 0;
for (const name of ['named', 'invalid']) {{
  document.getElementById(name).addEventListener('load', () => window.__loads++);
}}
</script>"#,
        foreign.address()
    );
    let (listener, origin) = server();
    TestServer::serve_on(
        listener,
        vec![("/page/index.html", ok("text/html", page.as_bytes()))],
    );
    let (_fixture, mut client, session) = open_page(
        "tinybrowser-referrer-state",
        &format!("http://{origin}/page/index.html"),
    );
    poll_until("both images to load", || {
        (evaluate(&mut client, &session, "window.__loads") == json!(2)).then_some(())
    });

    evaluate(
        &mut client,
        &session,
        "document.getElementById('named').setAttribute('referrerpolicy', 'NO-REFERRER');\
         document.getElementById('invalid').setAttribute('referrerpolicy', 'other-bogus')",
    );
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert_eq!(
        evaluate(&mut client, &session, "window.__loads"),
        json!(2),
        "a write that keeps the parsed state must not restart the request"
    );
    assert_eq!(foreign.history("/named.png").len(), 1);
    assert_eq!(foreign.history("/invalid.png").len(), 1);
}

// https://html.spec.whatwg.org/multipage/images.html#reacting-to-dom-mutations
// https://html.spec.whatwg.org/multipage/urls-and-fetching.html#referrer-policy-attribute
#[test]
fn a_referrerpolicy_state_change_dials_the_in_flight_request_again() {
    let (foreign_listener, _) = server();
    let foreign = TestServer::serve_on_after(
        foreign_listener,
        vec![("/slow.png", ok("image/png", PIXEL))],
        std::time::Duration::from_millis(400),
    );
    let page = format!(
        r"<!doctype html><body><script>
const image = document.createElement('img');
image.referrerPolicy = 'no-referrer';
image.src = 'http://{}/slow.png';
image.referrerPolicy = 'unsafe-url';
document.body.append(image);
</script>",
        foreign.address()
    );
    let (listener, origin) = server();
    TestServer::serve_on(
        listener,
        vec![("/page/index.html", ok("text/html", page.as_bytes()))],
    );
    let (_fixture, _client, _session) = open_page(
        "tinybrowser-referrer-restart",
        &format!("http://{origin}/page/index.html"),
    );

    let requests = foreign.wait_count("/slow.png", 2);
    let referers: Vec<Option<&str>> = requests.iter().map(|head| referer(head)).collect();
    let full = format!("http://{origin}/page/index.html");
    assert!(
        referers.contains(&Some(full.as_str())),
        "the restarted request carries the state the page set: {requests:?}"
    );
    assert_eq!(
        referers.iter().filter(|value| value.is_none()).count(),
        1,
        "the request sampled before the write kept its no-referrer state: {requests:?}"
    );
}

// https://w3c.github.io/webappsec-referrer-policy/#set-requests-referrer-policy-on-redirect
// https://fetch.spec.whatwg.org/#http-redirect-fetch
#[test]
fn a_redirect_response_policy_decides_the_next_hop() {
    use crate::fixture::redirect;

    let far = TestServer::start(
        vec![
            (
                "/first.png",
                redirect("/second.png").header("Referrer-Policy", "no-referrer"),
            ),
            ("/second.png", ok("image/png", PIXEL)),
        ],
        None,
    );
    let page = format!(
        "<!doctype html><img src=\"http://{}/first.png\">",
        far.address()
    );
    let (listener, origin) = server();
    let _served = TestServer::serve_on(
        listener,
        vec![("/page/index.html", ok("text/html", page.as_bytes()))],
    );
    let (_fixture, _client, _session) = open_page(
        "tinybrowser-referrer-redirect-policy",
        &format!("http://{origin}/page/index.html"),
    );

    let first = far.wait("/first.png");
    let origin_only = format!("http://{origin}/");
    assert_eq!(
        referer(&first),
        Some(origin_only.as_str()),
        "the first hop uses the document's policy: {first}"
    );
    let second = far.wait("/second.png");
    assert_eq!(
        referer(&second),
        None,
        "the redirect response's policy governs the next hop: {second}"
    );
}
