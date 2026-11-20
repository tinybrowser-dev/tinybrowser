// https://w3c.github.io/webappsec-csp/#create-violation-for-global

use serde_json::json;

use crate::fixture::{TestServer, evaluate, ok, open_page, server};

const PAGE: &str = r"<!doctype html><body>
<script>
window.__violations = [];
document.addEventListener('securitypolicyviolation', event => {
  window.__violations.push([event.sourceFile, event.lineNumber, event.columnNumber]);
});
Error.prepareStackTrace = () => '    at x (http://evil.test/spoof.js:42:7)';
Error.stackTraceLimit = 0;
const script = document.createElement('script');
script.src = 'blocked.js';
document.body.append(script);
</script>
";

#[test]
fn a_violation_names_the_page_script_that_triggered_it() {
    let (listener, origin) = server();
    TestServer::serve_on(
        listener,
        vec![(
            "/page.html",
            ok("text/html", PAGE.as_bytes())
                .header("Content-Security-Policy", "script-src 'unsafe-inline'"),
        )],
    );
    let page = format!("http://{origin}/page.html");
    let (_fixture, mut client, session) = open_page("tinybrowser-csp-source", &page);

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let violations = loop {
        let reported = evaluate(&mut client, &session, "window.__violations");
        let violations: Vec<Vec<serde_json::Value>> =
            serde_json::from_value(reported.clone()).unwrap_or_default();
        if !violations.is_empty() {
            break violations;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no violation was reported: {reported}"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    };

    let [source_file, line_number, column_number] = &violations[0][..] else {
        panic!("a violation carries sourceFile, lineNumber, columnNumber: {violations:?}");
    };
    assert_eq!(
        source_file,
        &json!(page),
        "the violation names the page script, not the spoofed one: {violations:?}"
    );
    assert_eq!(
        line_number,
        &json!(9),
        "the capture keeps its real position: {violations:?}"
    );
    assert_ne!(column_number, &json!(7), "{violations:?}");
    assert!(
        column_number.as_u64().unwrap_or_default() > 0,
        "{violations:?}"
    );
}
