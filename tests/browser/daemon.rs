use std::time::{Duration, Instant};

use crate::fixture::{Fixture, pid_alive};

#[test]
fn daemon_registers_loopback_and_second_start_reuses_it() {
    let mut fixture = Fixture::new("tinybrowser-daemon");
    let first_id = fixture.spawn_daemon();
    let info = fixture.wait_json();
    let pid = u32::try_from(info["pid"].as_u64().expect("pid")).expect("pid u32");
    let port = u16::try_from(info["port"].as_u64().expect("port")).expect("port u16");
    assert_eq!(info["host"], "127.0.0.1");
    assert!(pid_alive(pid));
    assert_eq!(pid, first_id);

    let mut client = cdp::Client::connect(format!("127.0.0.1:{port}").parse().expect("addr"))
        .expect("cdp connect");
    let version = client
        .call("Browser.getVersion", &serde_json::json!({}), None)
        .expect("version");
    assert_eq!(
        version["product"],
        serde_json::json!(net::Identity::default().product)
    );

    {
        use std::io::{Read, Write};
        let mut stream =
            std::net::TcpStream::connect(("127.0.0.1", port)).expect("webdriver connect");
        stream
            .write_all(b"GET /status HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
            .expect("webdriver write");
        let mut response = String::new();
        stream
            .read_to_string(&mut response)
            .expect("webdriver read");
        let body = response.split("\r\n\r\n").nth(1).expect("http body");
        let status: serde_json::Value = serde_json::from_str(body).expect("status json");
        assert_eq!(status["value"]["ready"], serde_json::json!(true));
    }

    fixture.spawn_daemon();
    let status = fixture
        .children()
        .last_mut()
        .expect("second daemon")
        .wait()
        .expect("second wait");
    assert!(status.success(), "second daemon should exit after reuse");
    let again = fixture.wait_json();
    assert_eq!(again["pid"], info["pid"]);
    assert_eq!(again["port"], info["port"]);
}

#[test]
fn killed_daemon_is_replaced() {
    let mut fixture = Fixture::new("tinybrowser-daemon");
    let first_id = fixture.spawn_daemon();
    let info = fixture.wait_json();
    assert_eq!(info["pid"].as_u64().expect("pid"), u64::from(first_id));

    fixture.children()[0].kill().expect("kill daemon");
    let _status = fixture.children()[0].wait().expect("reap daemon");

    let second_id = fixture.spawn_daemon();
    let deadline = Instant::now() + Duration::from_secs(5);
    let info = loop {
        if let Ok(text) =
            std::fs::read_to_string(fixture.runtime().join("tinybrowser/default/daemon.json"))
            && let Ok(value) = serde_json::from_str::<serde_json::Value>(&text)
            && value["pid"].as_u64() == Some(u64::from(second_id))
        {
            break value;
        }
        assert!(
            pid_alive(second_id),
            "the second daemon exited instead of replacing the killed one"
        );
        assert!(
            Instant::now() < deadline,
            "killed daemon's registration stayed"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(info["port"].as_u64().expect("port") > 1);
}

#[test]
fn overlapping_daemons_share_one_endpoint() {
    let mut fixture = Fixture::new("tinybrowser-daemon");
    let first_id = fixture.spawn_daemon();
    let second_id = fixture.spawn_daemon();
    let deadline = Instant::now() + Duration::from_secs(5);
    let winner_pid = loop {
        let first_exited = fixture.children()[0]
            .try_wait()
            .expect("first wait")
            .is_some();
        let second_exited = fixture.children()[1]
            .try_wait()
            .expect("second wait")
            .is_some();
        match (first_exited, second_exited) {
            (true, false) => break second_id,
            (false, true) => break first_id,
            (true, true) => panic!("both overlapping daemons exited"),
            (false, false) => {
                assert!(
                    Instant::now() < deadline,
                    "one overlapping daemon must exit"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    };
    let info = fixture.wait_json();
    assert_eq!(info["pid"].as_u64().expect("pid"), u64::from(winner_pid));
    assert!(pid_alive(winner_pid));
}

fn run_cli(fixture: &Fixture, args: &[&str]) -> std::process::Output {
    use std::process::{Command, Stdio};

    Command::new(env!("CARGO_BIN_EXE_tinybrowser"))
        .args(args)
        .env("XDG_RUNTIME_DIR", fixture.runtime())
        .env("XDG_DATA_HOME", fixture.data())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run cli")
}

#[test]
fn start_status_and_stop_manage_detached_daemon() {
    let fixture = Fixture::new("tinybrowser-lifecycle");

    let initial_status = run_cli(&fixture, &["status"]);
    assert_eq!(initial_status.status.code(), Some(1));
    let initial_json: serde_json::Value =
        serde_json::from_slice(&initial_status.stdout).expect("stopped status json");
    assert_eq!(initial_json["running"], serde_json::json!(false));
    assert_eq!(initial_json["profile"], serde_json::json!("default"));

    let started = run_cli(&fixture, &["start"]);
    assert!(
        started.status.success(),
        "start failed: {}",
        String::from_utf8_lossy(&started.stderr)
    );
    let started_json: serde_json::Value =
        serde_json::from_slice(&started.stdout).expect("start json");
    let pid = u32::try_from(started_json["pid"].as_u64().expect("pid")).expect("pid u32");
    let port = u16::try_from(started_json["port"].as_u64().expect("port")).expect("port u16");
    assert_eq!(started_json["host"], "127.0.0.1");
    assert_eq!(started_json["url"], format!("http://127.0.0.1:{port}"));
    assert!(pid_alive(pid));

    let again = run_cli(&fixture, &["start"]);
    assert!(again.status.success());
    let again_json: serde_json::Value =
        serde_json::from_slice(&again.stdout).expect("second start json");
    assert_eq!(again_json["pid"], started_json["pid"]);
    assert_eq!(again_json["port"], started_json["port"]);

    let conflict = run_cli(
        &fixture,
        &["start", "--port", &port.wrapping_add(1).to_string()],
    );
    assert_eq!(conflict.status.code(), Some(1));
    let conflict_err = String::from_utf8_lossy(&conflict.stderr);
    assert!(conflict_err.contains("already running"), "{conflict_err}");

    let live_status = run_cli(&fixture, &["status"]);
    assert!(live_status.status.success());
    let live_json: serde_json::Value =
        serde_json::from_slice(&live_status.stdout).expect("live status json");
    assert_eq!(live_json["running"], serde_json::json!(true));
    assert_eq!(live_json["ready"], serde_json::json!(true));
    assert_eq!(live_json["pid"], started_json["pid"]);
    assert_eq!(live_json["port"], started_json["port"]);

    let stopped = run_cli(&fixture, &["stop"]);
    assert!(
        stopped.status.success(),
        "stop failed: {}",
        String::from_utf8_lossy(&stopped.stderr)
    );
    let stopped_json: serde_json::Value =
        serde_json::from_slice(&stopped.stdout).expect("stop json");
    assert_eq!(stopped_json["stopped"], serde_json::json!(true));
    assert_eq!(stopped_json["pid"], started_json["pid"]);
    assert!(
        !fixture
            .runtime()
            .join("tinybrowser/default/daemon.json")
            .exists(),
        "daemon.json must be removed on clean stop"
    );

    let stopped_again = run_cli(&fixture, &["stop"]);
    assert!(stopped_again.status.success());
    let stopped_again_json: serde_json::Value =
        serde_json::from_slice(&stopped_again.stdout).expect("second stop json");
    assert_eq!(stopped_again_json["stopped"], serde_json::json!(false));

    let after_status = run_cli(&fixture, &["status"]);
    assert_eq!(after_status.status.code(), Some(1));
}
