use std::time::{Duration, Instant};

use crate::fixture::{Fixture, TestServer, ok, pid_alive, renderer_children};
use serde_json::{Value, json};

fn attach(client: &mut cdp::Client, target: &str) -> String {
    let attached = client
        .call(
            "Target.attachToTarget",
            &json!({"targetId": target, "flatten": true}),
            None,
        )
        .expect("attach");
    attached["sessionId"]
        .as_str()
        .expect("sessionId")
        .to_owned()
}

fn create(client: &mut cdp::Client) -> String {
    let result = client
        .call("Target.createTarget", &json!({"url": "about:blank"}), None)
        .expect("createTarget");
    result["targetId"].as_str().expect("targetId").to_owned()
}

fn close(client: &mut cdp::Client, target: &str) {
    client
        .call("Target.closeTarget", &json!({"targetId": target}), None)
        .expect("closeTarget");
}

fn eval(client: &mut cdp::Client, target: &str, script: &str) -> Result<String, String> {
    let session = attach(client, target);
    let result = client
        .call(
            "Runtime.evaluate",
            &json!({"expression": script, "returnByValue": true}),
            Some(&session),
        )
        .map_err(|error| error.to_string())?;
    if let Some(text) = result
        .pointer("/exceptionDetails/text")
        .and_then(Value::as_str)
    {
        return Err(text.to_owned());
    }
    let preview = result.get("result").cloned().unwrap_or(Value::Null);
    Ok(match preview.get("value") {
        Some(value) => value.to_string(),
        None => preview
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("undefined")
            .to_owned(),
    })
}

fn daemon_pid(fixture: &Fixture) -> u32 {
    u32::try_from(fixture.wait_json()["pid"].as_u64().expect("daemon pid")).expect("pid")
}

fn wait_for_renderers(daemon: u32, expect_empty: bool, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        let children = renderer_children(daemon);
        if children.is_empty() == expect_empty {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "renderer children still {}: {children:?}",
            if expect_empty { "present" } else { "missing" }
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn wait_for_renderer_count(daemon: u32, expected: usize, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        if renderer_children(daemon).len() == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "renderer count never reached {expected}: {:?}",
            renderer_children(daemon)
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn target_ids(client: &mut cdp::Client) -> Vec<String> {
    client
        .call("Target.getTargets", &json!({}), None)
        .expect("targets")["targetInfos"]
        .as_array()
        .expect("targetInfos")
        .iter()
        .map(|info| info["targetId"].as_str().expect("targetId").to_owned())
        .collect()
}

fn spawn(fixture: &mut Fixture) -> (cdp::Client, String) {
    fixture.spawn_daemon();
    let _ = fixture.wait_json();
    let mut client = fixture.connect();
    let created = create(&mut client);
    assert!(!created.is_empty(), "create id");
    (client, created)
}

#[test]
fn renderer_process_survives_non_finite_results() {
    let mut fixture = Fixture::new("tinybrowser-renderer");
    let (mut client, created) = spawn(&mut fixture);
    let session = attach(&mut client, &created);

    let infinity = client
        .call(
            "Runtime.evaluate",
            &json!({"expression": "1/0", "returnByValue": true}),
            Some(&session),
        )
        .expect("infinity");
    assert_eq!(infinity["result"]["unserializableValue"], json!("Infinity"));
    assert!(infinity["result"].get("value").is_none());

    assert_eq!(eval(&mut client, &created, "2+2").unwrap(), "4");
    close(&mut client, &created);
}

#[test]
fn close_interrupts_a_running_script_in_the_renderer_process() {
    let mut fixture = Fixture::new("tinybrowser-renderer");
    let (mut client, created) = spawn(&mut fixture);

    let endpoint = fixture.endpoint();
    let target = created.clone();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let runner = std::thread::spawn(move || {
        let mut runner = cdp::Client::connect(endpoint).expect("runner connect");
        let result = eval(&mut runner, &target, "while(true){}");
        let _ = done_tx.send(result);
    });
    std::thread::sleep(Duration::from_millis(300));

    let started = Instant::now();
    close(&mut client, &created);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "close waited on the running script: {:?}",
        started.elapsed()
    );
    let _ = done_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("running eval never returned after close");
    runner.join().expect("runner");
}

#[test]
fn aborted_tab_releases_its_assignment() {
    let server = TestServer::start(
        vec![("/", ok("text/html", b"<!doctype html><title>abort</title>"))],
        None,
    );
    let address = server.address();

    let mut fixture = Fixture::new("tinybrowser-renderer-abort");
    fixture.spawn_daemon_with_env("TINYBROWSER_LOG", "debug");
    let _ = fixture.wait_json();
    let mut client = fixture.connect();
    let target = create(&mut client);
    let session = attach(&mut client, &target);
    client
        .call("Page.enable", &json!({}), Some(&session))
        .expect("enable");
    client
        .call(
            "Page.navigate",
            &json!({"url": format!("http://{address}/")}),
            Some(&session),
        )
        .expect("navigate");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let event = client
            .read_event(Duration::from_millis(500))
            .expect("event read")
            .expect("event");
        if event["method"] == json!("Page.loadEventFired") {
            break;
        }
        assert!(Instant::now() < deadline, "page did not load");
    }

    let endpoint = fixture.endpoint();
    let blocked = target.clone();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let runner = std::thread::spawn(move || {
        let mut runner = cdp::Client::connect(endpoint).expect("runner connect");
        let result = eval(&mut runner, &blocked, "while(true){}");
        let _ = done_tx.send(result);
    });
    std::thread::sleep(Duration::from_millis(300));
    close(&mut client, &target);
    let _ = done_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("blocked eval never returned after close");
    runner.join().expect("runner");

    let path = fixture.data().join("tinybrowser/logs/default.log");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        if text.contains("assignment released") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "aborted coordinator never released its assignment: {text}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn a_wedged_renderer_is_interrupted_and_the_tab_recovers() {
    let server = TestServer::start(
        vec![(
            "/",
            ok("text/html", b"<!doctype html><title>timeout</title>"),
        )],
        None,
    );
    let address = server.address();

    let mut fixture = Fixture::new("tinybrowser-renderer-timeout");
    fixture.spawn_daemon_with_env("TINYBROWSER_RENDERER_REQUEST_TIMEOUT_MS", "500");
    let _ = fixture.wait_json();
    let mut client = fixture.connect();
    let target = create(&mut client);
    let session = attach(&mut client, &target);
    client
        .call("Page.enable", &json!({}), Some(&session))
        .expect("enable");
    let url = format!("http://{address}/");
    client
        .call("Page.navigate", &json!({"url": url}), Some(&session))
        .expect("navigate");
    crate::fixture::wait_for_load(&mut client, &session, Duration::from_secs(5));

    let started = Instant::now();
    let blocked = eval(&mut client, &target, "(() => { while(true){} })()");
    assert!(blocked.is_err(), "wedged eval must fail: {blocked:?}");
    assert!(
        started.elapsed() >= Duration::from_millis(400),
        "eval returned before the request deadline: {:?}",
        started.elapsed()
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "wedged eval did not time out: {:?}",
        started.elapsed()
    );

    client
        .call("Page.navigate", &json!({"url": url}), Some(&session))
        .expect("renavigate");
    crate::fixture::wait_for_load(&mut client, &session, Duration::from_secs(10));
    assert_eq!(eval(&mut client, &target, "2+2").unwrap(), "4");
}

#[test]
fn post_message_to_a_busy_tab_does_not_block_the_sender() {
    let server = TestServer::start(
        vec![(
            "/",
            ok("text/html", b"<!doctype html><title>sender</title>"),
        )],
        None,
    );
    let address = server.address();

    let (peer_port, _peer_server) = serve_page("peer");
    let mut fixture = Fixture::new("tinybrowser-renderer-messaging");
    fixture.spawn_daemon_with_env("TINYBROWSER_LOG", "debug");
    let _ = fixture.wait_json();
    let mut client = fixture.connect();
    let initial = target_ids(&mut client);
    let sender = create(&mut client);
    let session = attach(&mut client, &sender);
    client
        .call("Page.enable", &json!({}), Some(&session))
        .expect("enable");
    client
        .call(
            "Page.navigate",
            &json!({"url": format!("http://{address}/")}),
            Some(&session),
        )
        .expect("navigate");
    crate::fixture::wait_for_load(&mut client, &session, Duration::from_secs(5));
    assert_eq!(
        eval(
            &mut client,
            &sender,
            &format!(
                "window.__peer = window.open('http://localhost:{peer_port}/', 'peer'); !!window.__peer"
            )
        )
        .unwrap(),
        "true"
    );
    let receiver = target_ids(&mut client)
        .into_iter()
        .find(|target| *target != sender && !initial.contains(target))
        .expect("opened target");

    let endpoint = fixture.endpoint();
    let blocked_target = receiver.clone();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let blocking_thread = std::thread::spawn(move || {
        let mut runner = cdp::Client::connect(endpoint).expect("runner connect");
        let result = eval(&mut runner, &blocked_target, "(() => { while(true){} })()");
        let _ = done_tx.send(result);
    });
    std::thread::sleep(Duration::from_millis(300));
    let endpoint = fixture.endpoint();
    let probed_target = receiver.clone();
    let (probe_tx, probe_rx) = std::sync::mpsc::channel();
    let probing_thread = std::thread::spawn(move || {
        let mut runner = cdp::Client::connect(endpoint).expect("runner connect");
        let result = eval(&mut runner, &probed_target, "1");
        let _ = probe_tx.send(result);
    });
    assert!(
        probe_rx.recv_timeout(Duration::from_millis(500)).is_err(),
        "receiver answered while wedged"
    );

    let started = Instant::now();
    let sent = eval(&mut client, &sender, "window.__peer.postMessage('ping')");
    assert!(sent.is_ok(), "postMessage failed: {sent:?}");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "postMessage blocked on the busy receiver: {:?}",
        started.elapsed()
    );

    let path = fixture.data().join("tinybrowser/logs/default.log");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        if text.contains("window message queued for tab") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "delivery never reached the mailbox: {text}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }

    assert_eq!(eval(&mut client, &sender, "2+2").unwrap(), "4");

    close(&mut client, &receiver);
    close(&mut client, &sender);
    let _ = done_rx.recv_timeout(Duration::from_secs(5));
    blocking_thread.join().expect("blocking thread");
    let _ = probe_rx.recv_timeout(Duration::from_secs(5));
    probing_thread.join().expect("probing thread");
}

#[test]
fn window_messages_are_limited_to_related_tabs() {
    let server = TestServer::start(
        vec![(
            "/",
            ok("text/html", b"<!doctype html><title>related</title>"),
        )],
        None,
    );
    let address = server.address();

    let (peer_port, _peer_server) = serve_page("peer");
    let mut fixture = Fixture::new("tinybrowser-renderer-related");
    fixture.spawn_daemon_with_env("TINYBROWSER_LOG", "debug");
    let _ = fixture.wait_json();
    let mut client = fixture.connect();
    let sender = create(&mut client);
    let session = attach(&mut client, &sender);
    client
        .call("Page.enable", &json!({}), Some(&session))
        .expect("enable");
    client
        .call(
            "Page.navigate",
            &json!({"url": format!("http://{address}/")}),
            Some(&session),
        )
        .expect("navigate");
    crate::fixture::wait_for_load(&mut client, &session, Duration::from_secs(5));

    assert_eq!(
        eval(
            &mut client,
            &sender,
            "typeof __tbWindowPostMessage === 'undefined'"
        )
        .unwrap(),
        "true"
    );
    std::thread::sleep(Duration::from_millis(300));
    let path = fixture.data().join("tinybrowser/logs/default.log");
    let log = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        !log.contains("window message queued"),
        "an unrelated target received a message: {log}"
    );
    assert_eq!(eval(&mut client, &sender, "2+2").unwrap(), "4");

    assert_eq!(
        eval(
            &mut client,
            &sender,
            &format!(
                "window.__peer = window.open('http://localhost:{peer_port}/', 'peer'); !!window.__peer"
            )
        )
        .unwrap(),
        "true"
    );
    eval(&mut client, &sender, "window.__peer.postMessage('ping')").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        if text.contains("window message queued") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "related delivery never reached the mailbox: {text}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn legal_storage_values_do_not_kill_the_renderer() {
    let server = TestServer::start(
        vec![(
            "/",
            ok("text/html", b"<!doctype html><title>storage</title>"),
        )],
        None,
    );
    let address = server.address();

    let mut fixture = Fixture::new("tinybrowser-renderer-storage");
    fixture.spawn_daemon();
    let _ = fixture.wait_json();
    let mut client = fixture.connect();
    let target = create(&mut client);
    let session = attach(&mut client, &target);
    client
        .call("Page.enable", &json!({}), Some(&session))
        .expect("enable");
    client
        .call(
            "Page.navigate",
            &json!({"url": format!("http://{address}/")}),
            Some(&session),
        )
        .expect("navigate");
    crate::fixture::wait_for_load(&mut client, &session, Duration::from_secs(5));

    assert_eq!(
        eval(
            &mut client,
            &target,
            "localStorage.setItem('big', '\"'.repeat(2500000)); localStorage.getItem('big').length"
        )
        .unwrap(),
        "2500000"
    );
    assert_eq!(eval(&mut client, &target, "2+2").unwrap(), "4");
}

fn serve_page(title: &'static str) -> (u16, TestServer) {
    let body = format!("<!doctype html><title>{title}</title>");
    let server = TestServer::start(vec![("/", ok("text/html", body.as_bytes()))], None);
    let port = server.address().port();
    (port, server)
}

#[test]
fn blank_targets_stay_virtual_and_idle_processes_collapse_to_one_spare() {
    let mut fixture = Fixture::new("tinybrowser-renderer");
    fixture.spawn_daemon();
    let _ = fixture.wait_json();
    let mut client = fixture.connect();
    let daemon = daemon_pid(&fixture);
    wait_for_renderers(daemon, false, Duration::from_secs(5));
    let initial_targets = target_ids(&mut client);
    assert_eq!(
        initial_targets.len(),
        1,
        "daemon starts with one initial tab"
    );
    let before = renderer_children(daemon).len();

    let created = create(&mut client);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        renderer_children(daemon).len(),
        before,
        "creating a blank target must not consume the spare"
    );

    assert_eq!(eval(&mut client, &created, "2+2").unwrap(), "4");
    wait_for_renderer_count(daemon, before + 1, Duration::from_secs(5));
    close(&mut client, &created);
    wait_for_renderer_count(daemon, before, Duration::from_secs(5));

    close(&mut client, &initial_targets[0]);
    wait_for_renderer_count(daemon, before, Duration::from_secs(5));
}

#[test]
fn renderers_exit_when_the_daemon_is_killed() {
    let mut fixture = Fixture::new("tinybrowser-renderer");
    fixture.spawn_daemon();
    let _ = fixture.wait_json();
    let daemon = daemon_pid(&fixture);
    wait_for_renderers(daemon, false, Duration::from_secs(5));
    let renderers = renderer_children(daemon);
    assert!(!renderers.is_empty(), "initial tab must have a renderer");

    let killed = std::process::Command::new("kill")
        .args(["-9", &daemon.to_string()])
        .status()
        .expect("kill");
    assert!(killed.success(), "SIGKILL daemon");

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let alive: Vec<u32> = renderers
            .iter()
            .copied()
            .filter(|pid| pid_alive(*pid))
            .collect();
        if alive.is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "renderers outlived their daemon: {alive:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn log_level_reaches_the_daemon_file() {
    let mut fixture = Fixture::new("tinybrowser-log-file");
    fixture.spawn_daemon_with_env("TINYBROWSER_LOG", "debug");
    let _ = fixture.wait_json();
    let mut client = fixture.connect();
    let created = create(&mut client);

    let path = fixture.data().join("tinybrowser/logs/default.log");
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut text = String::new();
    loop {
        if let Ok(read) = std::fs::read_to_string(&path)
            && read.contains(" DEBUG daemon ")
            && read.contains(" INFO renderer ready")
        {
            text = read;
            break;
        }
        assert!(
            Instant::now() < deadline,
            "incomplete daemon log at {}: {text}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(
        text.contains(" INFO renderer ready"),
        "renderer ready record missing"
    );
    close(&mut client, &created);
}

#[test]
fn same_site_assignments_share_only_after_the_process_budget() {
    let server = TestServer::start(
        vec![(
            "/",
            ok("text/html", b"<!doctype html><title>shared-site</title>"),
        )],
        None,
    );
    let address = server.address();

    let mut fixture = Fixture::new("tinybrowser-renderer-budget");
    fixture.spawn_daemon_with_env("TINYBROWSER_RENDERER_PROCESS_LIMIT", "1");
    let _ = fixture.wait_json();
    let daemon = daemon_pid(&fixture);
    wait_for_renderer_count(daemon, 1, Duration::from_secs(5));
    let mut client = fixture.connect();
    let first = create(&mut client);
    let second = create(&mut client);
    let first_session = attach(&mut client, &first);
    let second_session = attach(&mut client, &second);
    for session in [&first_session, &second_session] {
        client
            .call("Page.enable", &json!({}), Some(session))
            .expect("enable");
        client
            .call(
                "Page.navigate",
                &json!({"url": format!("http://{address}/")}),
                Some(session),
            )
            .expect("navigate");
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut loaded = Vec::new();
    while loaded.len() < 2 {
        let event = client
            .read_event(Duration::from_millis(500))
            .expect("event read")
            .expect("event");
        if event["method"] == json!("Page.loadEventFired") {
            loaded.push(event["sessionId"].as_str().expect("session").to_owned());
        }
        assert!(Instant::now() < deadline, "both assignments did not load");
    }
    assert_eq!(renderer_children(daemon).len(), 1);
    assert_eq!(
        eval(&mut client, &first, "window.marker = 41").unwrap(),
        "41"
    );
    assert_eq!(
        eval(&mut client, &second, "typeof window.marker").unwrap(),
        "\"undefined\""
    );
    close(&mut client, &first);
    assert_eq!(eval(&mut client, &second, "2+2").unwrap(), "4");
    assert_eq!(renderer_children(daemon).len(), 1);
}
