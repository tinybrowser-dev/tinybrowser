use crate::fixture::Fixture;

fn run(fixture: &Fixture, args: &[&str]) -> std::process::Output {
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_tinybrowser"))
        .args(args)
        .env("XDG_RUNTIME_DIR", fixture.runtime())
        .env("XDG_DATA_HOME", fixture.data())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if child.try_wait().expect("wait").is_some() {
            return child.wait_with_output().expect("output");
        }
        if Instant::now() >= deadline {
            child.kill().expect("kill");
            let output = child.wait_with_output().expect("output");
            panic!(
                "{args:?} did not exit within 10s; stderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn short_v_prints_the_version() {
    let fixture = Fixture::new("tinybrowser-version");
    let output = run(&fixture, &["-v"]);
    assert!(output.status.success());
    let out = String::from_utf8_lossy(&output.stdout);
    assert!(out.contains("tinybrowser"), "{out}");
    assert!(out.contains(env!("CARGO_PKG_VERSION")), "{out}");
}

#[test]
fn version_short_circuits_anywhere() {
    let fixture = Fixture::new("tinybrowser-version-global");
    for args in [
        &["--profile=default", "-v"][..],
        &["--renderer", "--version"],
        &["--port=0", "-V"],
    ] {
        let output = run(&fixture, args);
        assert!(output.status.success(), "{args:?}");
        let out = String::from_utf8_lossy(&output.stdout);
        assert!(out.contains("tinybrowser"), "{args:?}: {out}");
        assert!(out.contains(env!("CARGO_PKG_VERSION")), "{args:?}: {out}");
    }
}

#[test]
fn rejects_unknown_log_level() {
    let fixture = Fixture::new("tinybrowser-log-level");
    let output = run(&fixture, &["--log-level=noisy"]);
    assert_eq!(output.status.code(), Some(2));
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(err.contains("unknown log level"), "{err}");
}

#[test]
fn help_documents_workflow_endpoints_and_flags() {
    let fixture = Fixture::new("tinybrowser-help");
    for flag in ["--help", "-h"] {
        let output = run(&fixture, &[flag]);
        assert!(output.status.success(), "{flag}");
        let out = String::from_utf8_lossy(&output.stdout);
        assert!(out.contains("Usage: tinybrowser [OPTIONS]"), "{out}");
        assert!(out.contains("Workflow:"), "{out}");
        assert!(
            out.contains("Endpoints (http://127.0.0.1:<port>):"),
            "{out}"
        );
        assert!(out.contains("--port <PORT>"), "{out}");
        assert!(out.contains("--profile <NAME>"), "{out}");
        assert!(out.contains("--renderer"), "{out}");
        assert!(out.contains("daemon.json"), "{out}");
    }
}

#[test]
fn subcommands_and_positionals_are_rejected() {
    let fixture = Fixture::new("tinybrowser-no-subcommands");
    for args in [
        &["daemon"][..],
        &["webdriver"],
        &["renderer"],
        &["help"],
        &["help", "webdriver"],
        &["--daemon"],
        &["--webdriver=9"],
        &["--", "daemon"],
        &["--", "nonsense"],
    ] {
        let output = run(&fixture, args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        let err = String::from_utf8_lossy(&output.stderr);
        assert!(err.contains("unexpected argument"), "{args:?}: {err}");
    }
}

#[test]
fn repeated_single_value_flags_are_rejected() {
    let fixture = Fixture::new("tinybrowser-repeated-flags");
    for args in [
        &["--verbose", "--verbose"][..],
        &["--log-level=info", "--log-level=debug"],
        &["--profile=a", "--profile=b"],
        &["--port=1", "--port=2"],
        &["--renderer", "--renderer"],
    ] {
        let output = run(&fixture, args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
    }
}

#[test]
fn rejects_an_invalid_proxy_before_starting() {
    let fixture = Fixture::new("tinybrowser-proxy");
    let output = run(&fixture, &["--port=0", "--proxy=http://proxy.test/path"]);
    assert_eq!(output.status.code(), Some(2));
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(err.contains("proxy URI"), "{err}");
}

#[test]
fn rejects_an_invalid_resolve_spec_before_starting() {
    let fixture = Fixture::new("tinybrowser-resolve");
    let output = run(&fixture, &["--port=0", "--resolve=not-a-spec"]);
    assert_eq!(output.status.code(), Some(2));
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(err.contains("resolve spec"), "{err}");
}

#[test]
fn rejects_an_unparseable_tls_ca_before_starting() {
    let fixture = Fixture::new("tinybrowser-tls-ca");
    let ca = fixture.data().join("not-a-cert.pem");
    std::fs::write(&ca, b"not a certificate").expect("write CA");
    let output = run(
        &fixture,
        &["--port=0", &format!("--tls-ca={}", ca.display())],
    );
    assert_eq!(output.status.code(), Some(2));
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(
        err.contains(&format!("--tls-ca {}:", ca.display())),
        "{err}"
    );
    assert!(!err.contains("unexpected argument"), "{err}");
}

#[test]
fn rejects_a_missing_tls_ca_file_before_starting() {
    let fixture = Fixture::new("tinybrowser-tls-ca-missing");
    let missing = fixture.data().join("missing/test-ca.pem");
    let output = run(
        &fixture,
        &["--port=0", &format!("--tls-ca={}", missing.display())],
    );
    assert_eq!(output.status.code(), Some(2));
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(
        err.contains(&format!("--tls-ca {}:", missing.display())),
        "{err}"
    );
    assert!(!err.contains("unexpected argument"), "{err}");
}

#[test]
fn help_wins_over_an_unreadable_tls_ca() {
    let fixture = Fixture::new("tinybrowser-help-tls-ca");
    let output = run(&fixture, &["--tls-ca=/missing", "--help"]);
    assert!(output.status.success(), "{output:?}");
    let out = String::from_utf8_lossy(&output.stdout);
    assert!(out.contains("Usage:"), "{out}");
}

#[test]
fn renderer_rejects_daemon_flags_in_either_order() {
    let fixture = Fixture::new("tinybrowser-renderer-flag-conflict");
    for args in [
        &["--renderer", "--profile=default"][..],
        &["--profile=default", "--renderer"],
        &["--renderer", "--port=9"],
        &["--port=9", "--renderer"],
        &["--renderer", "--resolve=*.test=127.0.0.1"],
        &["--resolve=*.test=127.0.0.1", "--renderer"],
        &["--renderer", "--proxy=http://127.0.0.1:1"],
        &["--proxy=http://127.0.0.1:1", "--renderer"],
        &["--renderer", "--tls-ca=/missing"],
        &["--tls-ca=/missing", "--renderer"],
    ] {
        let output = run(&fixture, args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        let err = String::from_utf8_lossy(&output.stderr);
        assert!(err.contains("unexpected argument"), "{args:?}: {err}");
    }
}

#[test]
fn log_file_flag_names_the_log_file() {
    let fixture = Fixture::new("tinybrowser-log-file");
    let path = fixture.data().join("explicit.log");
    let output = run(
        &fixture,
        &[
            "--log-file",
            path.to_str().expect("utf-8 path"),
            "--renderer",
        ],
    );
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let logged = std::fs::read_to_string(&path).expect("log file created");
    assert!(logged.contains("INFO renderer ready"), "{logged}");
}

#[test]
fn log_file_rejects_missing_and_repeated_values() {
    let fixture = Fixture::new("tinybrowser-log-file-bad");
    for args in [
        &["--log-file"][..],
        &["--log-file="],
        &["--log-file=a.log", "--log-file=b.log"],
    ] {
        let output = run(&fixture, args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
    }
}
