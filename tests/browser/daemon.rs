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
