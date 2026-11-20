use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use crate::fixture::{Fixture, TestServer, evaluate, load_page, ok};
use serde_json::json;

fn start() -> (Fixture, cdp::Client) {
    let mut fixture = Fixture::new("tinybrowser-cdp");
    fixture.spawn_daemon();
    let _ = fixture.wait_json();
    let client = fixture.connect();
    (fixture, client)
}

#[test]
fn browser_target_page_runtime_flatten_and_method_not_found() {
    let (_fixture, mut client) = start();

    let version = client
        .call("Browser.getVersion", &json!({}), None)
        .expect("version");
    let identity = net::Identity::default();
    assert_eq!(version["product"], json!(identity.product));
    assert_eq!(version["userAgent"], json!(identity.user_agent.as_str()));

    let created = client
        .call("Target.createTarget", &json!({"url": "about:blank"}), None)
        .expect("create");
    let target_id = created["targetId"].as_str().expect("target id").to_owned();

    let attached = client
        .call(
            "Target.attachToTarget",
            &json!({"targetId": target_id, "flatten": true}),
            None,
        )
        .expect("attach");
    let session = attached["sessionId"].as_str().expect("session").to_owned();

    let evaluated = client
        .call(
            "Runtime.evaluate",
            &json!({"expression": "1 + 1", "returnByValue": true}),
            Some(&session),
        )
        .expect("eval");
    assert_eq!(evaluated["result"]["value"].as_f64(), Some(2.0));

    let null_value = client
        .call(
            "Runtime.evaluate",
            &json!({"expression": "null", "returnByValue": true}),
            Some(&session),
        )
        .expect("null");
    assert_eq!(null_value["result"]["subtype"], json!("null"));
    assert_eq!(null_value["result"]["value"], json!(null));

    let undefined_value = client
        .call(
            "Runtime.evaluate",
            &json!({"expression": "undefined", "returnByValue": true}),
            Some(&session),
        )
        .expect("undefined");
    assert_eq!(undefined_value["result"]["type"], json!("undefined"));
    assert!(undefined_value["result"].get("subtype").is_none());

    let primitive = client
        .call(
            "Runtime.evaluate",
            &json!({"expression": "1 + 1"}),
            Some(&session),
        )
        .expect("primitive");
    assert_eq!(primitive["result"]["value"].as_f64(), Some(2.0));
    assert!(
        primitive["result"].get("objectId").is_none(),
        "primitive must not carry an objectId"
    );

    let object = client
        .call(
            "Runtime.evaluate",
            &json!({"expression": "({a: 1})"}),
            Some(&session),
        )
        .expect("object");
    assert!(
        object["result"]["objectId"].is_string(),
        "objects come back as handles"
    );
    assert!(object["result"].get("value").is_none());

    let missing = client.call("Foo.bar", &json!({}), None);
    let err = missing.expect_err("method-not-found");
    assert!(err.to_string().contains("wasn't found"), "{err}");

    client
        .call("Target.closeTarget", &json!({"targetId": target_id}), None)
        .expect("close");

    let bad = client.call("Target.createTarget", &json!({"url": "notaurl"}), None);
    assert!(bad.is_err(), "invalid createTarget url");
}

#[test]
fn browser_session_routes_commands_and_events() {
    let (_fixture, mut client) = start();

    let attached = client
        .call("Target.attachToBrowserTarget", &json!({}), None)
        .expect("attach to browser target");
    let session = attached["sessionId"]
        .as_str()
        .expect("browser session")
        .to_owned();

    let version = client
        .call("Browser.getVersion", &json!({}), Some(&session))
        .expect("browser session command");
    let identity = net::Identity::default();
    assert_eq!(version["product"], json!(identity.product));
    assert_eq!(version["userAgent"], json!(identity.user_agent.as_str()));

    client
        .call(
            "Target.setAutoAttach",
            &json!({
                "autoAttach": true,
                "waitForDebuggerOnStart": true,
                "flatten": true,
            }),
            Some(&session),
        )
        .expect("auto attach from browser session");

    let attached_event = loop {
        let event = client
            .read_event(Duration::from_secs(2))
            .expect("read event")
            .expect("attached event");
        if event["method"] == json!("Target.attachedToTarget") {
            break event;
        }
    };
    assert_eq!(
        attached_event["sessionId"],
        json!(session),
        "browser-session events must carry the browser session id"
    );
}

#[test]
fn get_properties_reports_descriptors_and_releases_groups() {
    let (_fixture, mut client) = start();
    let created = client
        .call("Target.createTarget", &json!({"url": "about:blank"}), None)
        .expect("create");
    let target_id = created["targetId"].as_str().expect("id").to_owned();
    let attached = client
        .call(
            "Target.attachToTarget",
            &json!({"targetId": target_id, "flatten": true}),
            None,
        )
        .expect("attach");
    let session = attached["sessionId"].as_str().expect("session").to_owned();

    let object = client
        .call(
            "Runtime.evaluate",
            &json!({
                "expression": "({ a: 1, get b() { throw new Error('getter ran'); }, [Symbol.for('s')]: 2 })",
                "objectGroup": "props",
            }),
            Some(&session),
        )
        .expect("evaluate object");
    let object_id = object["result"]["objectId"]
        .as_str()
        .expect("object handle")
        .to_owned();

    let properties = client
        .call(
            "Runtime.getProperties",
            &json!({
                "objectId": object_id,
                "ownProperties": true,
                "generatePreview": true,
                "objectGroup": "props",
            }),
            Some(&session),
        )
        .expect("getProperties");
    let descriptors = properties["result"].as_array().expect("descriptors");
    let a = descriptors
        .iter()
        .find(|entry| entry["name"] == json!("a"))
        .expect("a descriptor");
    assert_eq!(a["value"]["value"], json!(1));
    let b = descriptors
        .iter()
        .find(|entry| entry["name"] == json!("b"))
        .expect("b accessor");
    assert!(b["get"]["objectId"].is_string());
    assert!(
        b.get("value").is_none(),
        "accessor must not invoke its getter"
    );
    assert_eq!(
        properties["internalProperties"][0]["name"],
        json!("[[Prototype]]")
    );

    client
        .call(
            "Runtime.releaseObjectGroup",
            &json!({"objectGroup": "props"}),
            Some(&session),
        )
        .expect("release group");
    let missing = client
        .call(
            "Runtime.getProperties",
            &json!({"objectId": object_id, "ownProperties": true}),
            Some(&session),
        )
        .expect_err("released handle");
    assert!(
        missing
            .to_string()
            .contains("Could not find object with given id"),
        "{missing}"
    );
}

#[test]
fn clear_browser_cookies_deletes_page_cookies_from_both_target_kinds() {
    let server = TestServer::start(
        vec![("/", ok("text/html", b"<!doctype html><p>hi</p>"))],
        None,
    );
    let server_addr = server.address();

    let (_fixture, mut client) = start();
    let session = crate::fixture::load_page(
        &mut client,
        &format!("http://{server_addr}/"),
        Duration::from_secs(10),
    );

    let set_cookie = "document.cookie = 'a=1; path=/'";
    let read_cookie = "document.cookie";

    evaluate(&mut client, &session, set_cookie);
    assert_eq!(evaluate(&mut client, &session, read_cookie), json!("a=1"));
    client
        .call("Network.clearBrowserCookies", &json!({}), None)
        .expect("clear cookies from the root browser target");
    assert_eq!(
        evaluate(&mut client, &session, read_cookie),
        json!(""),
        "root clear must delete the page's cookies"
    );

    evaluate(&mut client, &session, set_cookie);
    assert_eq!(evaluate(&mut client, &session, read_cookie), json!("a=1"));
    client
        .call("Network.clearBrowserCookies", &json!({}), Some(&session))
        .expect("clear cookies from a page session");
    assert_eq!(
        evaluate(&mut client, &session, read_cookie),
        json!(""),
        "page-session clear must delete the page's cookies"
    );
}

#[test]
fn page_navigate_loads_http_document() {
    let server = TestServer::start(
        vec![("/", ok("text/html", b"<!doctype html><p id=ok>hi</p>"))],
        None,
    );
    let server_addr = server.address();

    let (_fixture, mut client) = start();
    let session = crate::fixture::load_page(
        &mut client,
        &format!("http://{server_addr}/"),
        Duration::from_secs(2),
    );
    let evaluated = client
        .call(
            "Runtime.evaluate",
            &json!({"expression": "document.getElementsByTagName('p')[0].firstChild.data", "returnByValue": true}),
            Some(&session),
        )
        .expect("eval");
    assert_eq!(evaluated["result"]["value"], json!("hi"));
}

// https://httpwg.org/http-extensions/draft-ietf-httpbis-rfc6265bis.html#name-same-site-requests
#[test]
fn page_navigate_from_a_fresh_tab_is_same_site_for_cookies() {
    let server = TestServer::start(
        vec![
            ("/", ok("text/html", b"<!doctype html><p>first</p>")),
            ("/again", ok("text/html", b"<!doctype html><p>again</p>")),
        ],
        None,
    );
    let origin = format!("http://{}/", server.address());
    let (_fixture, mut client) = start();
    let session = load_page(&mut client, &origin, Duration::from_secs(10));
    evaluate(
        &mut client,
        &session,
        "document.cookie = 'sid=1; SameSite=Strict; Path=/'",
    );
    assert_eq!(
        evaluate(&mut client, &session, "document.cookie"),
        json!("sid=1")
    );

    load_page(
        &mut client,
        &format!("{origin}again"),
        Duration::from_secs(10),
    );
    let head = server.wait("/again").to_ascii_lowercase();
    assert!(
        head.contains("cookie: sid=1"),
        "a browser-initiated navigation is same-site for cookies: {head}"
    );
    assert!(head.contains("sec-fetch-site: none"), "{head}");
}

#[test]
fn navigate_to_history_entry_takes_the_entry_id() {
    let server = TestServer::start(
        vec![
            ("/one", ok("text/html", b"<!doctype html><p>one</p>")),
            ("/two", ok("text/html", b"<!doctype html><p>two</p>")),
        ],
        None,
    );
    let origin = format!("http://{}/", server.address());
    let (_fixture, mut client) = start();
    let session = load_page(
        &mut client,
        &format!("{origin}one"),
        Duration::from_secs(10),
    );
    client
        .call(
            "Page.navigate",
            &json!({"url": format!("{origin}two")}),
            Some(&session),
        )
        .expect("navigate");
    server.wait("/two");
    crate::fixture::wait_for_load(&mut client, &session, Duration::from_secs(10));

    let history = client
        .call("Page.getNavigationHistory", &json!({}), Some(&session))
        .expect("history");
    assert_eq!(history["currentIndex"], json!(2));
    let first = history["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .find(|entry| entry["url"] == json!(format!("{origin}one")))
        .expect("first entry")["id"]
        .as_u64()
        .expect("entry id");

    client
        .call(
            "Page.navigateToHistoryEntry",
            &json!({"entryId": first}),
            Some(&session),
        )
        .expect("traverse to the first entry");
    server.wait_count("/one", 2);
    let after = client
        .call("Page.getNavigationHistory", &json!({}), Some(&session))
        .expect("history after");
    assert_eq!(after["currentIndex"], json!(1));

    let unknown = client.call(
        "Page.navigateToHistoryEntry",
        &json!({"entryId": 9999}),
        Some(&session),
    );
    let error = unknown.expect_err("an unknown entry id is refused");
    assert!(
        error.to_string().contains("unknown history entry"),
        "{error}"
    );
}

fn http_get(addr: std::net::SocketAddr, path: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).expect("http connect");
    let request = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).expect("http write");
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).expect("http read");
    let text = String::from_utf8_lossy(&buf);
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(0);
    let body = text
        .split("\r\n\r\n")
        .nth(1)
        .unwrap_or("")
        .trim()
        .to_owned();
    (status, body)
}

#[test]
fn json_discovery_page_socket_close_target_and_browser_close() {
    let (fixture, mut client) = start();
    let addr = fixture.endpoint();
    let created = client
        .call("Target.createTarget", &json!({"url": "about:blank"}), None)
        .expect("create");
    let target_id = created["targetId"].as_str().expect("target id").to_owned();

    let (status, version) = http_get(addr, "/json/version");
    assert_eq!(status, 200);
    let (slash_status, _) = http_get(addr, "/json/version/");
    assert_eq!(slash_status, 200, "legacy clients append a trailing slash");
    let version: serde_json::Value = serde_json::from_str(&version).expect("version json");
    assert_eq!(version["Browser"], json!(net::Identity::default().product));
    assert_eq!(
        version["webSocketDebuggerUrl"],
        json!(format!("ws://{addr}/devtools/browser"))
    );

    let (status, list) = http_get(addr, "/json/list");
    assert_eq!(status, 200);
    let list: serde_json::Value = serde_json::from_str(&list).expect("list json");
    let entry = list
        .as_array()
        .and_then(|items| items.iter().find(|item| item["id"] == json!(target_id)))
        .expect("created target in discovery");
    let page_url = entry["webSocketDebuggerUrl"].as_str().expect("page ws");
    assert!(
        page_url.ends_with(&format!("/devtools/page/{target_id}")),
        "{page_url}"
    );

    let mut page_client =
        cdp::Client::connect_path(addr, &format!("/devtools/page/{target_id}")).expect("page ws");
    let evaluated = page_client
        .call(
            "Runtime.evaluate",
            &json!({"expression": "1+1", "returnByValue": true}),
            None,
        )
        .expect("page eval");
    assert_eq!(evaluated["result"]["value"].as_f64(), Some(2.0));

    let missing_page = cdp::Client::connect_path(addr, "/devtools/page/not-a-number");
    assert!(missing_page.is_err(), "invalid page path must not upgrade");
    let unknown_path = cdp::Client::connect_path(addr, "/foo");
    assert!(
        unknown_path.is_err(),
        "unknown upgrade path must not become browser"
    );

    let unflat = client.call(
        "Target.attachToTarget",
        &json!({"targetId": target_id}),
        None,
    );
    let err = unflat.expect_err("flatten required");
    assert!(err.to_string().contains("flatten"), "{err}");

    let attached = client
        .call(
            "Target.attachToTarget",
            &json!({"targetId": target_id, "flatten": true}),
            None,
        )
        .expect("attach");
    let session = attached["sessionId"].as_str().expect("session").to_owned();
    client
        .call("Target.closeTarget", &json!({"targetId": target_id}), None)
        .expect("close target");
    let after_close = client.call(
        "Runtime.evaluate",
        &json!({"expression": "1"}),
        Some(&session),
    );
    let err = after_close.expect_err("closed session");
    assert!(
        err.to_string().contains("unknown session") || err.to_string().contains("stopped"),
        "{err}"
    );

    client
        .call("Browser.close", &json!({}), None)
        .expect("browser close");
    let after_browser = client.call("Browser.getVersion", &json!({}), None);
    assert!(after_browser.is_err(), "getVersion after Browser.close");
}

#[test]
fn dom_outer_html_serializes_elements_and_documents() {
    let (_fixture, mut client) = start();
    let server = TestServer::start(
        vec![(
            "/",
            ok(
                "text/html",
                b"<!doctype html><html><body><p id=\"x\" class=\"a\">hi</p></body></html>",
            ),
        )],
        None,
    );
    let server_addr = server.address();
    let session = crate::fixture::load_page(
        &mut client,
        &format!("http://{server_addr}/"),
        Duration::from_secs(10),
    );

    let document = client
        .call("DOM.getDocument", &json!({}), Some(&session))
        .expect("document");
    let root = document["root"]["nodeId"].as_u64().expect("root id");
    let found = client
        .call(
            "DOM.querySelector",
            &json!({"nodeId": root, "selector": "p"}),
            Some(&session),
        )
        .expect("query");
    let paragraph = found["nodeId"].as_u64().expect("paragraph id");

    let element = client
        .call(
            "DOM.getOuterHTML",
            &json!({"nodeId": paragraph}),
            Some(&session),
        )
        .expect("element outerHTML");
    assert_eq!(
        element,
        json!({"outerHTML": "<p id=\"x\" class=\"a\">hi</p>"})
    );

    let whole = client
        .call("DOM.getOuterHTML", &json!({"nodeId": root}), Some(&session))
        .expect("document outerHTML");
    let html = whole["outerHTML"].as_str().expect("document html");
    assert!(html.contains("<p id=\"x\" class=\"a\">hi</p>"), "{html}");

    let unknown = client
        .call(
            "DOM.getOuterHTML",
            &json!({"nodeId": 999_999}),
            Some(&session),
        )
        .expect_err("unknown node");
    assert!(
        unknown
            .to_string()
            .contains("Could not find node with given id"),
        "{unknown}"
    );
}

#[test]
fn dom_attributes_require_an_element_and_an_identifier() {
    let (_fixture, mut client) = start();
    let server = TestServer::start(
        vec![(
            "/",
            ok(
                "text/html",
                b"<!doctype html><html><body><p id=\"x\" class=\"a\">hi</p></body></html>",
            ),
        )],
        None,
    );
    let server_addr = server.address();
    let session = crate::fixture::load_page(
        &mut client,
        &format!("http://{server_addr}/"),
        Duration::from_secs(10),
    );

    let document = client
        .call("DOM.getDocument", &json!({}), Some(&session))
        .expect("document");
    let root = document["root"]["nodeId"].as_u64().expect("root id");
    let found = client
        .call(
            "DOM.querySelector",
            &json!({"nodeId": root, "selector": "p"}),
            Some(&session),
        )
        .expect("query");
    let paragraph = found["nodeId"].as_u64().expect("paragraph id");

    let attributes = client
        .call(
            "DOM.getAttributes",
            &json!({"nodeId": paragraph}),
            Some(&session),
        )
        .expect("attributes");
    assert_eq!(attributes, json!({"attributes": ["id", "x", "class", "a"]}));

    let document_error = client
        .call(
            "DOM.getAttributes",
            &json!({"nodeId": root}),
            Some(&session),
        )
        .expect_err("document has no attributes");
    assert!(
        document_error
            .to_string()
            .contains("Node is not an Element"),
        "{document_error}"
    );

    let missing = client
        .call("DOM.getAttributes", &json!({}), Some(&session))
        .expect_err("missing identifier");
    assert!(
        missing
            .to_string()
            .contains("nodeId, backendNodeId, or objectId"),
        "{missing}"
    );

    let unknown = client
        .call(
            "DOM.getAttributes",
            &json!({"nodeId": 999_999}),
            Some(&session),
        )
        .expect_err("unknown node");
    assert!(
        unknown
            .to_string()
            .contains("Could not find node with given id"),
        "{unknown}"
    );
}

#[test]
fn dom_resolve_node_returns_an_object_id_runtime_can_use() {
    let (_fixture, mut client) = start();
    let server = TestServer::start(
        vec![(
            "/",
            ok(
                "text/html",
                b"<!doctype html><html><body><p id=\"x\">hi</p></body></html>",
            ),
        )],
        None,
    );
    let server_addr = server.address();
    let session = crate::fixture::load_page(
        &mut client,
        &format!("http://{server_addr}/"),
        Duration::from_secs(10),
    );

    let document = client
        .call("DOM.getDocument", &json!({}), Some(&session))
        .expect("document");
    let root = document["root"]["nodeId"].as_u64().expect("root id");
    let found = client
        .call(
            "DOM.querySelector",
            &json!({"nodeId": root, "selector": "p"}),
            Some(&session),
        )
        .expect("query");
    let paragraph = found["nodeId"].as_u64().expect("paragraph id");

    let resolved = client
        .call(
            "DOM.resolveNode",
            &json!({"nodeId": paragraph}),
            Some(&session),
        )
        .expect("resolve");
    assert_eq!(resolved["object"]["subtype"], json!("node"));
    assert_eq!(resolved["object"]["description"], json!("P"));
    let object_id = resolved["object"]["objectId"]
        .as_str()
        .expect("object id")
        .to_owned();

    let evaluated = client
        .call(
            "Runtime.callFunctionOn",
            &json!({
                "objectId": object_id,
                "functionDeclaration": "function() { return this.tagName; }",
                "returnByValue": true,
            }),
            Some(&session),
        )
        .expect("call");
    assert_eq!(evaluated["result"]["value"], json!("P"));

    let again = client
        .call(
            "DOM.resolveNode",
            &json!({"nodeId": paragraph}),
            Some(&session),
        )
        .expect("resolve again");
    assert_eq!(again["object"]["objectId"], resolved["object"]["objectId"]);
}
