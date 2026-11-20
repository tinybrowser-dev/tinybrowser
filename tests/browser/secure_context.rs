use std::fmt::Write as _;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::fixture::{Fixture, TestServer, attach, evaluate, ok, open_page, server, wait_for_load};

fn data_url(markup: &str) -> String {
    let mut url = String::from("data:text/html,");
    for byte in markup.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                url.push(char::from(byte));
            }
            _ => {
                let _ = write!(url, "%{byte:02X}");
            }
        }
    }
    url
}

const SRCDOC_PAGE: &str = r#"<!doctype html><body>
<iframe srcdoc="<script>
parent.__child = [
  isSecureContext,
  'userAgentData' in navigator,
  typeof NavigatorUAData,
  typeof crypto.randomUUID,
];
</script>"></iframe>
"#;

const SELF_REPORT: &str = "[
  String(isSecureContext),
  String('userAgentData' in navigator),
  typeof NavigatorUAData,
  typeof crypto.randomUUID,
].join(',')";

fn wait_for(client: &mut cdp::Client, session: &str, expression: &str, expected: &Value) -> Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let value = evaluate(client, session, expression);
        if &value == expected {
            return value;
        }
        assert!(Instant::now() < deadline, "{expression}: {value}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

// https://html.spec.whatwg.org/multipage/webappapis.html#secure-contexts
#[test]
fn a_data_document_is_never_secure_and_its_child_inherits_it() {
    let (_fixture, mut client, session) =
        open_page("tinybrowser-secure-context-data", &data_url(SRCDOC_PAGE));

    assert_eq!(
        evaluate(&mut client, &session, SELF_REPORT),
        json!("false,false,undefined,undefined"),
        "a data: top-level document is not a secure context"
    );
    wait_for(
        &mut client,
        &session,
        "window.__child ?? 'pending'",
        &json!([false, false, "undefined", "undefined"]),
    );
}

// https://html.spec.whatwg.org/multipage/webappapis.html#secure-contexts
#[test]
fn a_child_inherits_its_creators_secure_context() {
    let (listener, page) = server();
    let body = format!("<!doctype html><body>{SRCDOC_PAGE}");
    TestServer::serve_on(listener, vec![("/", ok("text/html", body.as_bytes()))]);
    let (_fixture, mut client, session) = open_page(
        "tinybrowser-secure-context-child",
        &format!("http://{page}/"),
    );

    assert_eq!(
        evaluate(&mut client, &session, SELF_REPORT),
        json!("true,true,function,function")
    );
    // https://webidl.spec.whatwg.org/#idl-attributes
    assert_eq!(
        evaluate(
            &mut client,
            &session,
            "Object.getOwnPropertyDescriptor(Navigator.prototype, 'userAgentData').get.name"
        ),
        json!("get userAgentData")
    );
    wait_for(
        &mut client,
        &session,
        "window.__child ?? 'pending'",
        &json!([true, true, "function", "function"]),
    );
}

fn target_ids(client: &mut cdp::Client) -> Vec<String> {
    let targets = client
        .call("Target.getTargets", &json!({}), None)
        .expect("targets");
    targets["targetInfos"]
        .as_array()
        .expect("target list")
        .iter()
        .filter_map(|target| target["targetId"].as_str().map(str::to_owned))
        .collect()
}

fn open_page_with_popup(prefix: &str, url: &str) -> (Fixture, cdp::Client, String, String) {
    let mut fixture = Fixture::new(prefix);
    fixture.spawn_daemon();
    fixture.wait_json();
    let mut client = fixture.connect();
    let before = target_ids(&mut client);
    let created = client
        .call("Target.createTarget", &json!({"url": "about:blank"}), None)
        .expect("create");
    let page = created["targetId"].as_str().expect("id").to_owned();
    let session = attach(&mut client, &page);
    client
        .call("Page.navigate", &json!({"url": url}), Some(&session))
        .expect("navigate");
    wait_for_load(&mut client, &session, Duration::from_secs(10));
    let mut after = target_ids(&mut client);
    after.retain(|id| id != &page && !before.contains(id));
    let popup = after.into_iter().next().expect("popup target");
    (fixture, client, session, popup)
}

// https://html.spec.whatwg.org/multipage/webappapis.html#secure-contexts
#[test]
fn a_popup_inherits_its_openers_insecure_context() {
    let markers = TestServer::start(vec![], None);
    let marker_port = markers.address().port();
    let page = format!(
        r#"<!doctype html><script>
const blank = open('about:blank');
const blobMarkup = '<body onload="new Image().src = \'http://127.0.0.1:{marker_port}/\' + isSecureContext">';
const blob = open(URL.createObjectURL(new Blob([blobMarkup], {{type: 'text/html'}})));
</script>"#
    );
    let (_fixture, mut client, session, popup) =
        open_page_with_popup("tinybrowser-secure-context-popup", &data_url(&page));

    assert_eq!(
        evaluate(&mut client, &session, "String(isSecureContext)"),
        json!("false")
    );
    let popup_session = attach(&mut client, &popup);
    assert_eq!(
        evaluate(&mut client, &popup_session, "String(isSecureContext)"),
        json!("false"),
        "an about:blank popup inherits the data document's insecure context"
    );
    markers.wait("/false");
}

// https://html.spec.whatwg.org/multipage/webappapis.html#secure-contexts
#[test]
fn a_popup_inherits_its_openers_secure_context() {
    let page = r#"<!doctype html><script>
const blank = open('about:blank');
const blobMarkup = '<scr' + 'ipt>opener.postMessage(String(isSecureContext), "*")</scr' + 'ipt>';
const blob = open(URL.createObjectURL(new Blob([blobMarkup], {type: 'text/html'})));
window.__blobBit = null;
addEventListener('message', (event) => { window.__blobBit = event.data; });
</script>"#;
    let (listener, address) = server();
    TestServer::serve_on(listener, vec![("/", ok("text/html", page.as_bytes()))]);
    let (_fixture, mut client, session, popup) = open_page_with_popup(
        "tinybrowser-secure-context-popup-secure",
        &format!("http://127.0.0.1:{}/", address.port()),
    );

    let popup_session = attach(&mut client, &popup);
    assert_eq!(
        evaluate(&mut client, &popup_session, "String(isSecureContext)"),
        json!("true"),
        "an about:blank popup inherits the loopback document's secure context"
    );
    wait_for(&mut client, &session, "window.__blobBit", &json!("true"));
}
