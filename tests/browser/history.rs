// https://html.spec.whatwg.org/multipage/history.html#joint-session-history
// https://html.spec.whatwg.org/multipage/browsing-the-web.html#traverse-the-history

use std::time::{Duration, Instant};

use crate::fixture::{TestServer, evaluate, ok, open_page};

fn wait_for_value(
    client: &mut cdp::Client,
    session: &str,
    expression: &str,
    expected: &serde_json::Value,
) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let value = evaluate(client, session, expression);
        if value == *expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{expression} never became {expected}: last value {value}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn joint_history_page() -> (TestServer, cdp::Client, String, crate::fixture::Fixture) {
    let page = b"<!doctype html><body><iframe id=f src=/child1></iframe>";
    let child = |name: &'static str| {
        format!("<!doctype html><body>{name}<script>window.__marker = {name:?};</script>")
    };
    let server = TestServer::start(
        vec![
            ("/", ok("text/html", page)),
            ("/child1", ok("text/html", child("one").as_bytes())),
            ("/child2", ok("text/html", child("two").as_bytes())),
        ],
        None,
    );
    let origin = format!("http://{}", server.address());
    let (fixture, client, session) = open_page("tinybrowser-joint-history", &format!("{origin}/"));
    (server, client, session, fixture)
}

#[test]
fn a_failed_child_navigation_does_not_add_a_joint_step() {
    let (_server, mut client, session, _fixture) = joint_history_page();
    let frame = "document.getElementById('f')";
    wait_for_value(
        &mut client,
        &session,
        &format!("{frame}.contentWindow.__marker"),
        &serde_json::json!("one"),
    );
    assert_eq!(evaluate(&mut client, &session, "history.length"), 2);

    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#finalize-a-cross-document-navigation
    evaluate(
        &mut client,
        &session,
        &format!("{frame}.contentWindow.location.href = 'http://127.0.0.1:9/never'"),
    );
    std::thread::sleep(Duration::from_millis(300));
    wait_for_value(
        &mut client,
        &session,
        &format!("{frame}.contentWindow.__marker"),
        &serde_json::json!("one"),
    );
    assert_eq!(
        evaluate(&mut client, &session, "history.length"),
        2,
        "a failed child load leaves the joint history alone"
    );

    evaluate(
        &mut client,
        &session,
        &format!("{frame}.contentWindow.location.href = '/child2'"),
    );
    wait_for_value(
        &mut client,
        &session,
        &format!("{frame}.contentWindow.__marker"),
        &serde_json::json!("two"),
    );
    assert_eq!(evaluate(&mut client, &session, "history.length"), 3);
}

#[test]
fn a_child_reports_the_joint_step_count_and_the_parent_backs_into_it() {
    let (_server, mut client, session, _fixture) = joint_history_page();
    let frame = "document.getElementById('f')";
    wait_for_value(
        &mut client,
        &session,
        &format!("{frame}.contentWindow.__marker"),
        &serde_json::json!("one"),
    );

    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#getting-all-used-history-steps
    assert_eq!(evaluate(&mut client, &session, "history.length"), 2);
    assert_eq!(
        evaluate(
            &mut client,
            &session,
            &format!("{frame}.contentWindow.history.length")
        ),
        2,
        "a child's length is the tab's step count, not its own entry list"
    );

    evaluate(
        &mut client,
        &session,
        &format!("{frame}.contentWindow.location.href = '/child2'"),
    );
    wait_for_value(
        &mut client,
        &session,
        &format!("{frame}.contentWindow.__marker"),
        &serde_json::json!("two"),
    );
    assert_eq!(evaluate(&mut client, &session, "history.length"), 3);
    assert_eq!(
        evaluate(
            &mut client,
            &session,
            &format!("{frame}.contentWindow.history.length")
        ),
        3
    );

    evaluate(&mut client, &session, "history.back()");
    wait_for_value(
        &mut client,
        &session,
        &format!("{frame}.contentWindow.__marker"),
        &serde_json::json!("one"),
    );
    assert_eq!(
        evaluate(
            &mut client,
            &session,
            &format!("{frame}.contentWindow.history.length")
        ),
        3,
        "back moves the position, not the step count"
    );

    evaluate(&mut client, &session, "history.forward()");
    wait_for_value(
        &mut client,
        &session,
        &format!("{frame}.contentWindow.__marker"),
        &serde_json::json!("two"),
    );
}
