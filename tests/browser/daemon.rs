use std::path::Path;
use std::time::{Duration, Instant};

use crate::fixture::Fixture;

fn pid_alive(pid: u32) -> bool {
    Path::new("/proc").join(pid.to_string()).exists()
}

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
fn stale_lock_is_replaced() {
    let mut fixture = Fixture::new("tinybrowser-daemon");
    let dir = fixture.runtime().join("tinybrowser/default");
    std::fs::create_dir_all(&dir).expect("dir");
    // A pid above `pid_max` can never name a live process, so the registration
    // is stale on every machine; a plausible-looking pid (like 999999) can be
    // in use and would make this test depend on the host's process table.
    let stale = u32::MAX.to_string();
    std::fs::write(dir.join("lock"), format!("{stale}\n")).expect("stale lock");
    std::fs::write(
        dir.join("daemon.json"),
        format!(r#"{{"pid":{stale},"host":"127.0.0.1","port":1}}"#),
    )
    .expect("stale json");
    let child_id = fixture.spawn_daemon();
    let deadline = Instant::now() + Duration::from_secs(5);
    let info = loop {
        if let Ok(text) = std::fs::read_to_string(dir.join("daemon.json"))
            && let Ok(value) = serde_json::from_str::<serde_json::Value>(&text)
            && value["pid"].as_u64() != Some(u64::from(u32::MAX))
            && pid_alive(u32::try_from(value["pid"].as_u64().unwrap_or(0)).unwrap_or(0))
        {
            break value;
        }
        assert!(
            pid_alive(child_id),
            "daemon exited before replacing stale registration"
        );
        assert!(
            Instant::now() < deadline,
            "stale daemon.json not replaced: {:?}",
            std::fs::read_to_string(dir.join("daemon.json"))
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
