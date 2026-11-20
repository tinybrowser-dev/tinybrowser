// https://html.spec.whatwg.org/multipage/nav-history-apis.html#window-open-steps

use std::time::{Duration, Instant};

use serde_json::json;

use crate::fixture::{Fixture, TestServer, evaluate, ok, open_page, server};

fn open_popup(
    page_host: &str,
    head: &str,
    features: &str,
) -> (TestServer, String, Fixture, cdp::Client, String) {
    let popup = TestServer::start(vec![("/", ok("text/html", b"<!doctype html>peer"))], None);
    let target = format!("http://localhost:{}/", popup.address().port());
    let (listener, address) = server();
    let page = format!(
        r#"<!doctype html><html><head>{head}</head><body>
<script>window.__popup = window.open({target:?}, "peer", {features:?});</script>
"#
    );
    TestServer::serve_on(listener, vec![("/start", ok("text/html", page.as_bytes()))]);
    let page_origin = format!("http://{page_host}:{}", address.port());
    let (fixture, client, session) =
        open_page("tinybrowser-window-open", &format!("{page_origin}/start"));
    (popup, page_origin, fixture, client, session)
}

#[test]
fn a_cross_site_popup_sends_the_openers_origin_as_referrer() {
    let (popup, origin, _fixture, mut client, session) = open_popup("127.0.0.1", "", "");
    let head = popup.wait("/").to_ascii_lowercase();
    assert!(
        head.contains(&format!("referer: {origin}/")),
        "the popup carries the opener's origin: {head}"
    );
    assert!(head.contains("sec-fetch-site: cross-site"), "{head}");
    assert_eq!(
        evaluate(&mut client, &session, "window.__popup !== null"),
        json!(true),
        "with `noopener` absent the call returns a `WindowProxy`"
    );
}

#[test]
fn the_openers_referrer_policy_decides_how_much_travels() {
    let (popup, origin, _fixture, _client, _session) = open_popup(
        "127.0.0.1",
        r#"<meta name="referrer" content="unsafe-url">"#,
        "",
    );
    let head = popup.wait("/").to_ascii_lowercase();
    assert!(
        head.contains(&format!("referer: {origin}/start")),
        "`unsafe-url` sends the full opener URL: {head}"
    );
    assert!(head.contains("sec-fetch-site: cross-site"), "{head}");
}

#[test]
fn noreferrer_opens_the_popup_without_a_referrer() {
    let (popup, _origin, _fixture, _client, _session) = open_popup("127.0.0.1", "", "noreferrer");
    let head = popup.wait("/").to_ascii_lowercase();
    assert!(
        !head.contains("referer:"),
        "`noreferrer` withholds the header: {head}"
    );
    assert!(head.contains("sec-fetch-site: cross-site"), "{head}");
}

#[test]
fn a_same_site_popup_is_same_site_and_not_same_origin() {
    let (popup, origin, _fixture, _client, _session) = open_popup("localhost", "", "");
    let head = popup.wait("/").to_ascii_lowercase();
    assert!(
        head.contains(&format!("referer: {origin}/")),
        "the popup carries the opener's origin: {head}"
    );
    assert!(head.contains("sec-fetch-site: same-site"), "{head}");
}

// https://html.spec.whatwg.org/multipage/document-sequences.html#the-rules-for-choosing-a-navigable
// https://html.spec.whatwg.org/multipage/nav-history-apis.html#window-open-steps
#[test]
fn a_named_popup_is_reused_and_navigated() {
    let popup = TestServer::start(
        vec![
            ("/first", ok("text/html", b"<!doctype html>first")),
            ("/second", ok("text/html", b"<!doctype html>second")),
        ],
        None,
    );
    let port = popup.address().port();
    let (listener, address) = server();
    let page = format!(
        r#"<!doctype html><script>
window.__first = window.open('http://localhost:{port}/first', "shared");
</script>"#
    );
    TestServer::serve_on(listener, vec![("/start", ok("text/html", page.as_bytes()))]);
    let (_fixture, mut client, session) = open_page(
        "tinybrowser-window-open",
        &format!("http://127.0.0.1:{}/start", address.port()),
    );
    popup.wait_count("/first", 1);
    let second = format!(
        r#"window.__second = window.open('http://localhost:{port}/second', "shared");
           window.__first === window.__second"#
    );
    assert_eq!(
        evaluate(&mut client, &session, &second),
        json!(true),
        "the second call with the same name selects the first tab"
    );
    popup.wait_count("/second", 1);
    assert_eq!(
        popup.history("/first").len(),
        1,
        "the reused name did not open a second tab"
    );
    assert_eq!(
        popup.history("/second").len(),
        1,
        "the selected tab navigated to the second URL"
    );
}

// https://html.spec.whatwg.org/multipage/nav-history-apis.html#window-open-steps
#[test]
fn opening_the_current_context_with_no_url_keeps_its_document() {
    let (listener, address) = server();
    let page = "<!doctype html><title>stay</title><script>\
        window.__marker = 1;\
        window.__self = window.open('', '_self');\
        window.__top = window.open('', '_top');\
        </script>";
    TestServer::serve_on(listener, vec![("/start", ok("text/html", page.as_bytes()))]);
    let (_fixture, mut client, session) = open_page(
        "tinybrowser-window-open",
        &format!("http://127.0.0.1:{}/start", address.port()),
    );
    assert_eq!(
        evaluate(
            &mut client,
            &session,
            "[window.__self === window, window.__top === window, window.__marker, document.title].join(',')"
        ),
        json!("true,true,1,stay")
    );
}

// https://html.spec.whatwg.org/multipage/browsers.html#sandboxed-auxiliary-navigation-browsing-context-flag
#[test]
fn a_sandbox_without_allow_popups_cannot_open_a_context_by_spelling() {
    let popup = TestServer::start(
        vec![
            ("/allowed", ok("text/html", b"<!doctype html>allowed")),
            ("/blank", ok("text/html", b"<!doctype html>blank")),
            ("/upper", ok("text/html", b"<!doctype html>upper")),
            ("/named", ok("text/html", b"<!doctype html>named")),
        ],
        None,
    );
    let port = popup.address().port();
    let frame = |sandbox: &str, script: &str| {
        format!("<iframe sandbox=\"{sandbox}\" srcdoc=\"<script>{script}</script>\"></iframe>")
    };
    let page = format!(
        "<!doctype html><script>\
         window.__answers = [];\
         window.addEventListener('message', function (event) {{\
           window.__answers.push(String(event.data));\
         }});\
         </script>{}{}{}",
        frame(
            "allow-scripts allow-popups",
            &format!(
                "parent.postMessage('allowed:' + (window.open('http://localhost:{port}/allowed', '_blank') !== null), '*')"
            ),
        ),
        frame(
            "allow-scripts",
            &format!(
                "parent.postMessage(['refused', \
                 window.open('http://localhost:{port}/blank', '_blank') === null, \
                 window.open('http://localhost:{port}/upper', '_BLANK') === null, \
                 window.open('http://localhost:{port}/named', '_new') === null].join(','), '*')"
            ),
        ),
        frame(
            "allow-scripts",
            "parent.postMessage('self:' + (window.open('', '_self') === window), '*')",
        ),
    );
    let (listener, address) = server();
    TestServer::serve_on(listener, vec![("/start", ok("text/html", page.as_bytes()))]);
    let (_fixture, mut client, session) = open_page(
        "tinybrowser-window-open",
        &format!("http://127.0.0.1:{}/start", address.port()),
    );

    wait_for_answers(&mut client, &session);
    assert_eq!(
        evaluate(
            &mut client,
            &session,
            "window.__answers.slice().sort().join('|')"
        ),
        json!("allowed:true|refused,true,true,true|self:true")
    );
    popup.wait("/allowed");
    std::thread::sleep(std::time::Duration::from_millis(300));
    for path in ["/blank", "/upper", "/named"] {
        assert!(!popup.requested(path), "{path} opened without allow-popups");
    }
}

// https://html.spec.whatwg.org/multipage/nav-history-apis.html#get-noopener-for-window-open
#[test]
fn a_blob_popup_compares_its_entry_with_the_top_level_origin() {
    let (listener, address) = server();
    let port = address.port();
    let page = format!(
        r#"<!doctype html><script>
window.__cross_site_refused = null;
window.addEventListener('message', function (event) {{
  window.__cross_site_refused = event.data;
}});
const blob = new Blob(['<!doctype html>same'], {{type: 'text/html'}});
window.__same_site = window.open(URL.createObjectURL(blob), '_blank');
</script>
<iframe src="http://localhost:{port}/frame"></iframe>"#
    );
    let frame = "<!doctype html><script>
const blob = new Blob(['<!doctype html>peer'], {type: 'text/html'});
parent.postMessage(String(window.open(URL.createObjectURL(blob), '_blank') === null), '*');
</script>";
    TestServer::serve_on(
        listener,
        vec![
            ("/start", ok("text/html", page.as_bytes())),
            ("/frame", ok("text/html", frame.as_bytes())),
        ],
    );
    let (_fixture, mut client, session) = open_page(
        "tinybrowser-window-open",
        &format!("http://127.0.0.1:{port}/start"),
    );

    assert_eq!(
        evaluate(
            &mut client,
            &session,
            "window.__same_site !== null && window.__same_site.opener != null"
        ),
        json!(true),
        "a same-site blob keeps the opener"
    );
    wait_for_value(
        &mut client,
        &session,
        "window.__cross_site_refused !== null",
        &json!(true),
    );
    assert_eq!(
        evaluate(&mut client, &session, "window.__cross_site_refused"),
        json!("true"),
        "a cross-site iframe's blob severs the opener"
    );
}

fn wait_for_value(
    client: &mut cdp::Client,
    session: &str,
    expression: &str,
    expected: &serde_json::Value,
) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if &evaluate(client, session, expression) == expected {
            return;
        }
        assert!(Instant::now() < deadline, "{expression} never became true");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn wait_for_answers(client: &mut cdp::Client, session: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if evaluate(client, session, "window.__answers.length") == json!(3) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the sandboxed frames never reported"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

// https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-name
#[test]
fn a_browser_tab_reports_the_name_it_was_opened_with() {
    let popup = TestServer::start(
        vec![(
            "/",
            ok(
                "text/html",
                b"<!doctype html><script>fetch('/report?name=' + encodeURIComponent(window.name));</script>",
            ),
        )],
        None,
    );
    let port = popup.address().port();
    let (listener, address) = server();
    let page =
        format!("<!doctype html><script>window.open('http://localhost:{port}/', 'peer');</script>");
    TestServer::serve_on(listener, vec![("/start", ok("text/html", page.as_bytes()))]);
    let (_fixture, _client, _session) = open_page(
        "tinybrowser-window-name",
        &format!("http://127.0.0.1:{}/start", address.port()),
    );
    popup.wait("/report?name=peer");
}

// https://html.spec.whatwg.org/multipage/document-sequences.html#the-rules-for-choosing-a-navigable
#[test]
fn a_name_a_page_sets_is_what_the_group_opens_again() {
    let popup = TestServer::start(
        vec![
            (
                "/one",
                ok(
                    "text/html",
                    b"<!doctype html><script>window.name = 'later'; fetch('/ready');</script>",
                ),
            ),
            ("/two", ok("text/html", b"<!doctype html>two")),
        ],
        None,
    );
    let port = popup.address().port();
    let (listener, address) = server();
    let page = format!(
        "<!doctype html><script>\
         window.__first = window.open('http://localhost:{port}/one', '_blank');\
         </script>"
    );
    TestServer::serve_on(listener, vec![("/start", ok("text/html", page.as_bytes()))]);
    let (_fixture, mut client, session) = open_page(
        "tinybrowser-window-name",
        &format!("http://127.0.0.1:{}/start", address.port()),
    );
    popup.wait("/ready");
    let second = format!(
        "window.__second = window.open('http://localhost:{port}/two', 'later');\
         window.__second === window.__first"
    );
    assert_eq!(
        evaluate(&mut client, &session, &second),
        json!(true),
        "the name the popup set selects that tab instead of opening a new one"
    );
    popup.wait("/two");
    assert_eq!(
        popup.history("/one").len(),
        1,
        "the reused tab was navigated, not replaced"
    );
}

// https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName
#[test]
fn a_popups_name_follows_the_opener_it_was_opened_with() {
    for (features, first_name, second_name) in [("", "peer", "peer"), ("noopener", "", "")] {
        let second = TestServer::start(
            vec![(
                "/second",
                ok(
                    "text/html",
                    b"<!doctype html><script>fetch('/report?name=' + encodeURIComponent(window.name));</script>",
                ),
            )],
            None,
        );
        let second_port = second.address().port();
        let first_page = format!(
            "<!doctype html><script>\
             fetch('/first?name=' + encodeURIComponent(window.name)).then(function () {{\
               location.href = 'http://localhost:{second_port}/second';\
             }});\
             </script>"
        );
        let first = TestServer::start(
            vec![("/first", ok("text/html", first_page.as_bytes()))],
            None,
        );
        let first_port = first.address().port();
        let (listener, address) = server();
        let page = format!(
            "<!doctype html><script>\
             window.open('http://localhost:{first_port}/first', 'peer', {features:?});\
             </script>"
        );
        TestServer::serve_on(listener, vec![("/start", ok("text/html", page.as_bytes()))]);
        let (_fixture, _client, _session) = open_page(
            "tinybrowser-window-name",
            &format!("http://127.0.0.1:{}/start", address.port()),
        );
        first.wait(&format!("/first?name={first_name}"));
        second.wait("/second");
        second.wait(&format!("/report?name={second_name}"));
    }
}

// https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName
#[test]
fn a_target_name_is_restored_by_history_traversal() {
    let second = TestServer::start(
        vec![(
            "/second",
            ok(
                "text/html",
                b"<!doctype html><script>\
                  fetch('/seen?where=second&name=' + encodeURIComponent(window.name)).then(function () {\
                    if (!sessionStorage.getItem('visited')) {\
                      sessionStorage.setItem('visited', '1');\
                      history.back();\
                    }\
                  });\
                  </script>",
            ),
        )],
        None,
    );
    let second_port = second.address().port();
    let first_page = format!(
        "<!doctype html><script>\
         if (!sessionStorage.getItem('named')) {{\
           sessionStorage.setItem('named', '1');\
           window.name = 'peer';\
         }}\
         fetch('/seen?where=first&name=' + encodeURIComponent(window.name)).then(function () {{\
           if (!sessionStorage.getItem('visited')) {{\
             sessionStorage.setItem('visited', '1');\
             location.href = 'http://localhost:{second_port}/second';\
           }} else if (!sessionStorage.getItem('forwarded')) {{\
             sessionStorage.setItem('forwarded', '1');\
             history.forward();\
           }}\
         }});\
         </script>"
    );
    let first = TestServer::start(
        vec![("/first", ok("text/html", first_page.as_bytes()))],
        None,
    );
    let first_port = first.address().port();
    let (listener, address) = server();
    let page = format!(
        "<!doctype html><script>\
         window.open('http://localhost:{first_port}/first', '_blank', 'noopener');\
         </script>"
    );
    TestServer::serve_on(listener, vec![("/start", ok("text/html", page.as_bytes()))]);
    let (_fixture, _client, _session) = open_page(
        "tinybrowser-history-name",
        &format!("http://127.0.0.1:{}/start", address.port()),
    );
    first.wait_count("/seen?where=first&name=peer", 1);
    second.wait("/seen?where=second&name=");
    first.wait_count("/seen?where=first&name=peer", 2);
    second.wait_count("/seen?where=second&name=", 2);
}

// https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName
#[test]
fn the_next_documents_name_does_not_overwrite_the_entry_behind_it() {
    let second = TestServer::start(
        vec![(
            "/second",
            ok(
                "text/html",
                b"<!doctype html><script>\
                  window.name = 'second';\
                  fetch('/seen?where=second&name=' + encodeURIComponent(window.name)).then(function () {\
                    history.back();\
                  });\
                  </script>",
            ),
        )],
        None,
    );
    let second_port = second.address().port();
    let first_page = format!(
        "<!doctype html><script>\
         if (!sessionStorage.getItem('named')) {{\
           sessionStorage.setItem('named', '1');\
           window.name = 'first';\
         }}\
         fetch('/seen?where=first&name=' + encodeURIComponent(window.name)).then(function () {{\
           if (!sessionStorage.getItem('left')) {{\
             sessionStorage.setItem('left', '1');\
             location.href = 'http://localhost:{second_port}/second';\
           }}\
         }});\
         </script>"
    );
    let first = TestServer::start(
        vec![("/first", ok("text/html", first_page.as_bytes()))],
        None,
    );
    let first_port = first.address().port();
    let (listener, address) = server();
    let page = format!(
        "<!doctype html><script>\
         window.open('http://localhost:{first_port}/first', '_blank', 'noopener');\
         </script>"
    );
    TestServer::serve_on(listener, vec![("/start", ok("text/html", page.as_bytes()))]);
    let (_fixture, _client, _session) = open_page(
        "tinybrowser-history-name",
        &format!("http://127.0.0.1:{}/start", address.port()),
    );
    first.wait_count("/seen?where=first&name=first", 1);
    second.wait("/seen?where=second&name=second");
    first.wait_count("/seen?where=first&name=first", 2);
}

// https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName
#[test]
fn a_browser_navigation_keeps_the_entrys_name() {
    let second = TestServer::start(
        vec![(
            "/second",
            ok(
                "text/html",
                b"<!doctype html><script>fetch('/seen?where=second');</script>",
            ),
        )],
        None,
    );
    let second_port = second.address().port();
    let first_page = "<!doctype html><script>\
        if (!sessionStorage.getItem('named')) {\
          sessionStorage.setItem('named', '1');\
          window.name = 'kept';\
          history.pushState({}, '', '/first#kept');\
        }\
        fetch('/seen?where=first&name=' + encodeURIComponent(window.name));\
        </script>";
    let first = TestServer::start(
        vec![("/first", ok("text/html", first_page.as_bytes()))],
        None,
    );
    let first_port = first.address().port();
    let (_fixture, mut client, session) = open_page(
        "tinybrowser-history-name",
        &format!("http://localhost:{first_port}/first"),
    );
    first.wait("/seen?where=first&name=kept");
    client
        .call(
            "Page.navigate",
            &json!({"url": format!("http://localhost:{second_port}/second")}),
            Some(&session),
        )
        .expect("navigate");
    second.wait("/seen?where=second");
    evaluate(&mut client, &session, "history.back()");
    first.wait_count("/seen?where=first&name=kept", 2);
}

// https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName
#[test]
fn a_traversal_restores_the_name_the_group_opens() {
    let pages = TestServer::start(
        vec![
            (
                "/one",
                ok(
                    "text/html",
                    b"<!doctype html><script>\
                      fetch('/ready?where=one');\
                      if (sessionStorage.getItem('came-back')) {\
                        fetch('/ready?where=back');\
                      } else {\
                        sessionStorage.setItem('came-back', '1');\
                        location.href = '/two';\
                      }\
                      </script>",
                ),
            ),
            (
                "/two",
                ok(
                    "text/html",
                    b"<!doctype html><script>\
                      window.name = 'other';\
                      fetch('/ready?where=two').then(function () { history.back(); });\
                      </script>",
                ),
            ),
            ("/check", ok("text/html", b"<!doctype html>check")),
        ],
        None,
    );
    let port = pages.address().port();
    let (listener, address) = server();
    let page = format!(
        "<!doctype html><script>\
         window.__first = window.open('http://localhost:{port}/one', 'peer');\
         </script>"
    );
    TestServer::serve_on(listener, vec![("/start", ok("text/html", page.as_bytes()))]);
    let (_fixture, mut client, session) = open_page(
        "tinybrowser-window-name",
        &format!("http://127.0.0.1:{}/start", address.port()),
    );
    pages.wait("/ready?where=one");
    pages.wait("/ready?where=two");
    pages.wait("/ready?where=back");
    assert_eq!(
        evaluate(
            &mut client,
            &session,
            &format!(
                "window.__peer = window.open('http://localhost:{port}/check', 'peer');\
                 window.__peer === window.__first"
            ),
        ),
        json!(true),
        "the traversal did not restore the browsing context's name"
    );
    pages.wait("/check");
    assert_eq!(
        evaluate(
            &mut client,
            &session,
            "window.__stale = window.open('', 'other');\
             window.__stale === window.__first",
        ),
        json!(false),
        "the replaced name still selects the traversed tab"
    );
}

// https://html.spec.whatwg.org/multipage/browsers.html#popup-sandboxing-flag-set
#[test]
fn a_sandboxed_popup_inherits_its_flags_in_the_browser_tab() {
    let popup = TestServer::start(
        vec![(
            "/",
            ok(
                "text/html",
                b"<!doctype html><script>\
                  let storage = 'ok';\
                  try { localStorage.setItem('k', 'v'); } catch (error) { storage = error.name; }\
                  fetch('/report?origin=' + encodeURIComponent(window.origin) + '&storage=' + storage);\
                  </script>",
            ),
        )],
        None,
    );
    let port = popup.address().port();
    let (listener, address) = server();
    let page = format!(
        "<!doctype html><iframe sandbox=\"allow-scripts allow-popups\" \
         srcdoc=\"<script>window.open('http://localhost:{port}/', '_blank')</script>\"></iframe>"
    );
    TestServer::serve_on(listener, vec![("/start", ok("text/html", page.as_bytes()))]);
    let (_fixture, _client, _session) = open_page(
        "tinybrowser-window-sandbox",
        &format!("http://127.0.0.1:{}/start", address.port()),
    );
    popup.wait("/report?origin=null&storage=SecurityError");
}

// https://html.spec.whatwg.org/multipage/browsers.html#sandbox-propagates-to-auxiliary-browsing-contexts-flag
#[test]
fn an_escaping_sandbox_popup_starts_unsandboxed() {
    let popup = TestServer::start(
        vec![(
            "/",
            ok(
                "text/html",
                b"<!doctype html><script>\
                  let storage = 'ok';\
                  try { localStorage.setItem('k', 'v'); } catch (error) { storage = error.name; }\
                  fetch('/report?origin=' + encodeURIComponent(window.origin) + '&storage=' + storage);\
                  </script>",
            ),
        )],
        None,
    );
    let port = popup.address().port();
    let (listener, address) = server();
    let page = format!(
        "<!doctype html><iframe sandbox=\"allow-scripts allow-popups allow-popups-to-escape-sandbox\" \
         srcdoc=\"<script>window.open('http://localhost:{port}/', '_blank')</script>\"></iframe>"
    );
    TestServer::serve_on(listener, vec![("/start", ok("text/html", page.as_bytes()))]);
    let (_fixture, _client, _session) = open_page(
        "tinybrowser-window-sandbox",
        &format!("http://127.0.0.1:{}/start", address.port()),
    );
    let expected = format!("/report?origin=http%3A%2F%2Flocalhost%3A{port}&storage=ok");
    popup.wait(&expected);
}

// https://html.spec.whatwg.org/multipage/browsers.html#popup-sandboxing-flag-set
#[test]
fn a_renderer_hosted_popup_keeps_its_flags_after_a_navigation() {
    let (listener, address) = server();
    let next = "<!doctype html><script>\
        try { opener.top.location.href = '/hijacked'; } catch (error) {}\
        setTimeout(function () { fetch('/report?attempted=1'); }, 0);\
        </script>";
    let page = "<!doctype html><iframe sandbox=\"allow-scripts allow-popups allow-same-origin\" \
        srcdoc=\"<script>window.open('/next', '_blank')</script>\"></iframe>";
    let pages = TestServer::serve_on(
        listener,
        vec![
            ("/start", ok("text/html", page.as_bytes())),
            ("/next", ok("text/html", next.as_bytes())),
            ("/hijacked", ok("text/html", b"<!doctype html>hijacked")),
        ],
    );
    let (_fixture, _client, _session) = open_page(
        "tinybrowser-window-sandbox",
        &format!("http://127.0.0.1:{}/start", address.port()),
    );
    pages.wait("/report?attempted=1");
    assert!(
        !pages.requested("/hijacked"),
        "a sandboxed popup without `allow-top-navigation` cannot navigate its opener's top level"
    );
}

// https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName
#[test]
fn a_scripted_name_does_not_survive_a_cross_origin_navigation() {
    let second = TestServer::start(
        vec![(
            "/second",
            ok(
                "text/html",
                b"<!doctype html><script>fetch('/report?name=' + encodeURIComponent(window.name));</script>",
            ),
        )],
        None,
    );
    let second_port = second.address().port();
    let first_page = format!(
        "<!doctype html><script>\
         window.name = 'later';\
         fetch('/first?name=' + encodeURIComponent(window.name)).then(function () {{\
           location.href = 'http://localhost:{second_port}/second';\
         }});\
         </script>"
    );
    let first = TestServer::start(
        vec![("/first", ok("text/html", first_page.as_bytes()))],
        None,
    );
    let first_port = first.address().port();
    let (listener, address) = server();
    let page = format!(
        "<!doctype html><script>\
         window.open('http://localhost:{first_port}/first', '_blank', 'noopener');\
         </script>"
    );
    TestServer::serve_on(listener, vec![("/start", ok("text/html", page.as_bytes()))]);
    let (_fixture, _client, _session) = open_page(
        "tinybrowser-window-name",
        &format!("http://127.0.0.1:{}/start", address.port()),
    );
    first.wait("/first?name=later");
    second.wait("/second");
    second.wait("/report?name=");
}

// https://html.spec.whatwg.org/multipage/browsers.html#popup-sandboxing-flag-set
#[test]
fn a_sandboxed_popup_stays_sandboxed_after_its_own_navigation() {
    let popup = TestServer::start(
        vec![
            (
                "/start",
                ok(
                    "text/html",
                    b"<!doctype html><script>\
                      fetch('/step').then(function () { location.href = '/next'; });\
                      </script>",
                ),
            ),
            (
                "/next",
                ok(
                    "text/html",
                    b"<!doctype html><script>\
                      let storage = 'ok';\
                      try { localStorage.setItem('k', 'v'); } catch (error) { storage = error.name; }\
                      fetch('/report?origin=' + encodeURIComponent(window.origin) + '&storage=' + storage);\
                      </script>",
                ),
            ),
        ],
        None,
    );
    let port = popup.address().port();
    let (listener, address) = server();
    let page = format!(
        "<!doctype html><iframe sandbox=\"allow-scripts allow-popups\" \
         srcdoc=\"<script>window.open('http://localhost:{port}/start', '_blank')</script>\"></iframe>"
    );
    TestServer::serve_on(listener, vec![("/start", ok("text/html", page.as_bytes()))]);
    let (_fixture, _client, _session) = open_page(
        "tinybrowser-window-sandbox",
        &format!("http://127.0.0.1:{}/start", address.port()),
    );
    popup.wait("/step");
    popup.wait("/next");
    popup.wait("/report?origin=null&storage=SecurityError");
}

// https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName
#[test]
fn a_name_the_new_document_sets_survives_its_mount() {
    let popup = TestServer::start(
        vec![
            (
                "/first",
                ok(
                    "text/html",
                    b"<!doctype html><script>\
                      window.name = 'chosen';\
                      fetch('/first?name=' + encodeURIComponent(window.name));\
                      </script>",
                ),
            ),
            (
                "/second",
                ok(
                    "text/html",
                    b"<!doctype html><script>fetch('/report?name=' + encodeURIComponent(window.name));</script>",
                ),
            ),
        ],
        None,
    );
    let port = popup.address().port();
    let (listener, address) = server();
    let page = format!(
        "<!doctype html><script>\
         window.open('http://localhost:{port}/first', '_blank', 'noopener');\
         </script>"
    );
    TestServer::serve_on(listener, vec![("/start", ok("text/html", page.as_bytes()))]);
    let (_fixture, mut client, _session) = open_page(
        "tinybrowser-window-name",
        &format!("http://127.0.0.1:{}/start", address.port()),
    );
    popup.wait("/first?name=chosen");
    let first = format!("http://localhost:{port}/first");
    let targets = client
        .call("Target.getTargets", &json!({}), None)
        .expect("targets");
    let target_id = targets["targetInfos"]
        .as_array()
        .expect("target list")
        .iter()
        .find(|target| target["url"].as_str() == Some(first.as_str()))
        .and_then(|target| target["targetId"].as_str())
        .expect("the popup target")
        .to_owned();
    let popup_session = crate::fixture::attach(&mut client, &target_id);
    client
        .call(
            "Page.navigate",
            &json!({"url": format!("http://localhost:{port}/second")}),
            Some(&popup_session),
        )
        .expect("navigate the popup");
    popup.wait("/second");
    popup.wait("/report?name=chosen");
}

// https://html.spec.whatwg.org/multipage/semantics.html#attr-meta-http-equiv-refresh
#[test]
fn a_refresh_delay_beyond_the_clock_never_fires() {
    let page = "<!doctype html><meta http-equiv=\"refresh\" content=\"99999999999999999999;url=/next\"><title>overflow</title>";
    let (listener, origin) = server();
    let served = TestServer::serve_on(
        listener,
        vec![
            ("/page/index.html", ok("text/html", page.as_bytes())),
            (
                "/page/next",
                ok("text/html", b"<!doctype html><title>next</title>"),
            ),
        ],
    );
    let (_fixture, mut client, session) = open_page(
        "tinybrowser-refresh-overflow",
        &format!("http://{origin}/page/index.html"),
    );

    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(
        evaluate(&mut client, &session, "location.pathname"),
        json!("/page/index.html"),
        "the saturated delay never fires"
    );
    assert!(
        served.history("/page/next").is_empty(),
        "the page never asks for its refresh target"
    );
}

// https://html.spec.whatwg.org/multipage/browsers.html#popup-sandboxing-flag-set
fn sibling_navigation(sandboxed: bool) -> Vec<String> {
    let victim = TestServer::start(
        vec![
            (
                "/one",
                ok("text/html", b"<!doctype html><title>one</title>"),
            ),
            (
                "/two",
                ok("text/html", b"<!doctype html><title>two</title>"),
            ),
        ],
        None,
    );
    let popup_page = format!(
        "<!doctype html><script>setTimeout(() => {{ window.open('http://{}/two', 'victim'); }}, 500);</script>",
        victim.address()
    );
    let popup = TestServer::start(
        vec![("/popup", ok("text/html", popup_page.as_bytes()))],
        None,
    );
    let sandbox = if sandboxed {
        r#" sandbox="allow-scripts allow-popups""#
    } else {
        ""
    };
    let page = format!(
        r#"<!doctype html><script>window.open('http://{victim}/one', 'victim');</script>
<iframe{sandbox} srcdoc="<script>window.open('http://{popup}/popup', 'sandboxed');</script>"></iframe>"#,
        victim = victim.address(),
        popup = popup.address(),
    );
    let (listener, origin) = server();
    let _served = TestServer::serve_on(
        listener,
        vec![("/page/index.html", ok("text/html", page.as_bytes()))],
    );
    let (_fixture, _client, _session) = open_page(
        "tinybrowser-sandbox-sibling-navigation",
        &format!("http://{origin}/page/index.html"),
    );

    victim.wait("/one");
    popup.wait("/popup");
    std::thread::sleep(Duration::from_millis(1500));
    victim.history("/two")
}

fn sandboxed_frame_navigation(sandboxed: bool) -> Vec<String> {
    let victim = TestServer::start(
        vec![
            (
                "/one",
                ok("text/html", b"<!doctype html><title>one</title>"),
            ),
            (
                "/two",
                ok("text/html", b"<!doctype html><title>two</title>"),
            ),
        ],
        None,
    );
    let sandbox = if sandboxed {
        r#" sandbox="allow-scripts allow-popups""#
    } else {
        ""
    };
    let page = format!(
        r#"<!doctype html><script>window.open('http://{victim}/one', 'victim');</script>
<iframe{sandbox} srcdoc="<script>setTimeout(() => {{ window.open('http://{victim}/two', 'victim'); }}, 500);</script>"></iframe>"#,
        victim = victim.address(),
    );
    let (listener, origin) = server();
    let _served = TestServer::serve_on(
        listener,
        vec![("/page/index.html", ok("text/html", page.as_bytes()))],
    );
    let (_fixture, _client, _session) = open_page(
        "tinybrowser-sandbox-frame-navigation",
        &format!("http://{origin}/page/index.html"),
    );

    victim.wait("/one");
    std::thread::sleep(Duration::from_millis(1500));
    victim.history("/two")
}

// https://html.spec.whatwg.org/multipage/browsers.html#sandboxed-navigation-browsing-context-flag
#[test]
fn a_sandboxed_frame_cannot_navigate_a_sibling_tab_by_name() {
    assert!(
        sandboxed_frame_navigation(true).is_empty(),
        "a sandboxed frame must not navigate a tab it did not open"
    );
}

#[test]
fn an_unsandboxed_frame_may_navigate_a_sibling_tab_by_name() {
    assert_eq!(
        sandboxed_frame_navigation(false).len(),
        1,
        "an unsandboxed frame may navigate a tab in its group"
    );
}

// https://html.spec.whatwg.org/multipage/browsers.html#sandboxed-navigation-browsing-context-flag
#[test]
fn a_sandboxed_popup_cannot_navigate_a_sibling_tab_by_name() {
    assert!(
        sibling_navigation(true).is_empty(),
        "a sandboxed document must not navigate a tab it did not open"
    );
}

#[test]
fn an_unsandboxed_popup_may_navigate_a_sibling_tab_by_name() {
    assert_eq!(
        sibling_navigation(false).len(),
        1,
        "an unsandboxed document may navigate a tab in its group"
    );
}

// https://w3c.github.io/webappsec-csp/#sandbox
#[test]
fn a_csp_sandbox_without_allow_popups_cannot_open_a_popup() {
    let popup = TestServer::start(
        vec![("/popup", ok("text/html", b"<!doctype html>popup"))],
        None,
    );
    let page = format!(
        "<!doctype html><script>window.open('http://{}/popup', '_blank');</script>",
        popup.address()
    );
    let (listener, origin) = server();
    TestServer::serve_on(
        listener,
        vec![(
            "/page/index.html",
            ok("text/html", page.as_bytes())
                .header("Content-Security-Policy", "sandbox allow-scripts"),
        )],
    );
    let (_fixture, _client, _session) = open_page(
        "tinybrowser-csp-sandbox-popup",
        &format!("http://{origin}/page/index.html"),
    );
    std::thread::sleep(Duration::from_millis(800));
    assert!(
        popup.history("/popup").is_empty(),
        "a CSP sandbox without allow-popups blocks the popup"
    );
}

fn target_count(client: &mut cdp::Client) -> usize {
    client
        .call("Target.getTargets", &json!({}), None)
        .expect("targets")["targetInfos"]
        .as_array()
        .map_or(0, Vec::len)
}

fn wait_for_targets(client: &mut cdp::Client, expected: usize) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if target_count(client) == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the browser never had {expected} targets (saw {})",
            target_count(client)
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

// https://html.spec.whatwg.org/multipage/web-messaging.html#broadcasting-to-other-browsing-contexts
#[test]
fn a_popup_and_its_opener_share_a_broadcast_channel() {
    let page = b"<!doctype html><script>\n\
const name = String(Math.round(Math.random()*1e12));\n\
window.__got = null;\n\
const channel = new BroadcastChannel(name);\n\
channel.addEventListener('message', function (event) { window.__got = event.data; });\n\
window.open('/popup', name, undefined);\n\
</script>";
    let popup = b"<!doctype html><script>\n\
const channel = new BroadcastChannel(window.name);\n\
channel.postMessage({hello: 'world', n: 7});\n\
</script>";
    let server = TestServer::start(
        vec![
            ("/", ok("text/html", page)),
            ("/popup", ok("text/html", popup)),
        ],
        None,
    );
    let origin = format!("http://{}", server.address());
    let (_fixture, mut client, session) = open_page("tinybrowser-popup-broadcast", &origin);
    wait_for_value(
        &mut client,
        &session,
        "window.__got && window.__got.hello === 'world' && window.__got.n === 7",
        &json!(true),
    );
}

// https://html.spec.whatwg.org/multipage/window-object.html#dom-window-close
#[test]
fn a_cross_origin_popup_tab_closes_itself_through_window_close() {
    let popup = b"<!doctype html><script>\n\
opener.postMessage('bye', '*');\n\
window.close();\n\
</script>";
    let (listener, address) = server();
    let port = address.port();
    let page = b"<!doctype html><script>\n\
window.addEventListener('message', function (event) { window.__said = event.data; });\n\
</script>";
    TestServer::serve_on(
        listener,
        vec![
            ("/", ok("text/html", page)),
            ("/popup", ok("text/html", popup)),
        ],
    );
    let origin = format!("http://127.0.0.1:{port}/");
    let (_fixture, mut client, session) = open_page("tinybrowser-popup-self-close", &origin);
    let baseline = target_count(&mut client);

    evaluate(
        &mut client,
        &session,
        &format!("window.__popup = window.open('http://localhost:{port}/popup', '_blank')"),
    );
    wait_for_targets(&mut client, baseline + 1);
    wait_for_value(
        &mut client,
        &session,
        "window.__said === 'bye'",
        &json!(true),
    );
    wait_for_targets(&mut client, baseline);
}

#[test]
fn an_opener_closes_its_popup_through_the_proxy() {
    let (listener, address) = server();
    let port = address.port();
    TestServer::serve_on(
        listener,
        vec![
            (
                "/",
                ok("text/html", b"<!doctype html><title>opener</title>"),
            ),
            ("/popup", ok("text/html", b"<!doctype html>peer")),
        ],
    );
    let origin = format!("http://127.0.0.1:{port}/");
    let (_fixture, mut client, session) = open_page("tinybrowser-popup-proxy-close", &origin);
    let baseline = target_count(&mut client);

    evaluate(
        &mut client,
        &session,
        &format!("window.__popup = window.open('http://localhost:{port}/popup')"),
    );
    wait_for_targets(&mut client, baseline + 1);
    evaluate(&mut client, &session, "window.__popup.close()");
    wait_for_targets(&mut client, baseline);
    wait_for_value(&mut client, &session, "window.__popup.closed", &json!(true));
}
