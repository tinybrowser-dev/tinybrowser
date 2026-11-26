use std::ffi::OsString;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, BufRead as _, Read as _, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use browser::{
    AgentOptions, Browser, BrowserHandle, BrowserOptions, Profile, ProfileName, RendererSandbox,
};
use serde_json::{Value, json};
use server::{Request, Response, Shutdown};

const START_TIMEOUT: Duration = Duration::from_secs(15);
const STOP_TIMEOUT: Duration = Duration::from_secs(5);
const ENDPOINT_WAIT_TIMEOUT: Duration = Duration::from_secs(5);
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_CHILD_STDERR_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug)]
pub struct DaemonEndpoint {
    pub pid: u32,
    pub host: String,
    pub port: u16,
}

impl DaemonEndpoint {
    fn addr(&self) -> io::Result<SocketAddr> {
        format!("{}:{}", self.host, self.port)
            .parse()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, format!("{error}")))
    }

    fn url(&self) -> String {
        format!("http://{}:{}", self.host, self.port)
    }

    fn to_ready_json(&self, profile: &ProfileName) -> Value {
        json!({
            "profile": profile.as_str(),
            "pid": self.pid,
            "host": self.host,
            "port": self.port,
            "url": self.url(),
        })
    }
}

pub async fn serve(
    profile: &Profile,
    data_home: &Path,
    port: Option<u16>,
    network: AgentOptions,
    sandbox: RendererSandbox,
) -> io::Result<()> {
    let runtime = profile_runtime_dir(profile.name(), data_home);
    fs::create_dir_all(&runtime)?;
    restrict_dir(&runtime)?;
    let Some(_lock) = acquire_lock(&runtime)? else {
        let existing = wait_for_endpoint(&runtime, ENDPOINT_WAIT_TIMEOUT)?;
        check_port_conflict(profile.name(), port, &existing)?;
        logging::info!(
            target: "daemon",
            "profile {} already has a running daemon on {}:{}",
            profile.name().as_str(),
            existing.host,
            existing.port
        );
        emit_json(&existing.to_ready_json(profile.name()));
        return Ok(());
    };
    logging::debug!(target: "daemon", "runtime dir {}", runtime.display());
    let listener = TcpListener::bind(("127.0.0.1", port.unwrap_or(0)))
        .map_err(|error| io::Error::new(error.kind(), format!("bind failed: {error}")))?;
    let addr = listener.local_addr()?;
    logging::info!(
        target: "daemon",
        "serving profile {} on 127.0.0.1:{}",
        profile.name().as_str(),
        addr.port()
    );
    let browser = Browser::new(BrowserOptions {
        data_home: Some(data_home.to_path_buf()),
        profile: profile.clone(),
        network,
        sandbox,
    })
    .map_err(io::Error::from)?;
    if let Err(error) = browser.handle().ensure_renderer_ready().await {
        let _ = browser.close().await;
        return Err(io::Error::other(format!(
            "renderer failed to start: {error}"
        )));
    }
    let initial = match browser.handle().create_tab().await {
        Ok(tab) => tab,
        Err(error) => {
            let _ = browser.close().await;
            return Err(io::Error::other(error.to_string()));
        }
    };
    if let Err(error) = initial.load_html("<!doctype html><title></title>").await {
        let _ = browser.close().await;
        return Err(io::Error::other(error.to_string()));
    }
    let endpoint = DaemonEndpoint {
        pid: std::process::id(),
        host: "127.0.0.1".into(),
        port: addr.port(),
    };
    if let Err(error) = write_endpoint(&runtime, &endpoint) {
        let _ = browser.close().await;
        return Err(error);
    }
    emit_json(&endpoint.to_ready_json(profile.name()));
    let result = serve_http(&listener, browser.handle()).await;
    remove_endpoint(&runtime);
    let close = browser.close().await;
    result.and(close)
}

pub fn start(
    profile: &Profile,
    data_home: &Path,
    port: Option<u16>,
    forward_args: &[OsString],
) -> io::Result<()> {
    let runtime = profile_runtime_dir(profile.name(), data_home);
    fs::create_dir_all(&runtime)?;
    restrict_dir(&runtime)?;
    if let Some(lock) = acquire_lock(&runtime)? {
        drop(lock);
    } else {
        let existing = wait_for_endpoint(&runtime, ENDPOINT_WAIT_TIMEOUT)?;
        check_port_conflict(profile.name(), port, &existing)?;
        let _ = probe_status(&existing)?;
        emit_json(&existing.to_ready_json(profile.name()));
        return Ok(());
    }
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("serve")
        .args(forward_args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    detach_command(&mut command);
    let mut child = command.spawn()?;
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(io::Error::other("daemon stdout missing"));
    };
    let Some(stderr) = child.stderr.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(io::Error::other("daemon stderr missing"));
    };
    let stderr_handle = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr
            .take(MAX_CHILD_STDERR_BYTES as u64)
            .read_to_end(&mut buf);
        String::from_utf8_lossy(&buf).into_owned()
    });
    let (ready_tx, ready_rx) = mpsc::channel();
    let _stdout_handle = thread::spawn(move || {
        let mut reader = io::BufReader::new(stdout);
        let mut line = String::new();
        let result = reader.read_line(&mut line).map(|_| line);
        let _ = ready_tx.send(result);
    });
    match ready_rx.recv_timeout(START_TIMEOUT) {
        Ok(Ok(line)) if !line.trim().is_empty() => {
            let trimmed = line.trim();
            let parsed: Value = serde_json::from_str(trimmed).map_err(|error| {
                io::Error::other(format!("invalid daemon startup response: {error}"))
            })?;
            emit_json(&parsed);
            Ok(())
        }
        Ok(Ok(_) | Err(_)) | Err(mpsc::RecvTimeoutError::Disconnected) => {
            let status = child.wait()?;
            let captured = stderr_handle.join().unwrap_or_default();
            Err(io::Error::other(extract_daemon_error(&captured, status)))
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            let _ = child.kill();
            let _ = child.wait();
            let captured = stderr_handle.join().unwrap_or_default();
            let detail = extract_last_error(&captured);
            match detail {
                Some(reason) => Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("timed out waiting for daemon to start: {reason}"),
                )),
                None => Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "timed out waiting for daemon to start",
                )),
            }
        }
    }
}

pub fn status(profile: &Profile, data_home: &Path) -> io::Result<bool> {
    let runtime = profile_runtime_dir(profile.name(), data_home);
    if !runtime.exists() {
        emit_json(&json!({
            "running": false,
            "profile": profile.name().as_str(),
        }));
        return Ok(false);
    }
    if let Some(_lock) = acquire_lock(&runtime)? {
        remove_endpoint(&runtime);
        emit_json(&json!({
            "running": false,
            "profile": profile.name().as_str(),
        }));
        return Ok(false);
    }
    let endpoint = wait_for_endpoint(&runtime, PROBE_TIMEOUT)?;
    match probe_status(&endpoint) {
        Ok((ready, message)) => {
            let healthy = ready || message == "session already started";
            emit_json(&json!({
                "running": true,
                "ready": ready,
                "message": message,
                "profile": profile.name().as_str(),
                "pid": endpoint.pid,
                "host": endpoint.host,
                "port": endpoint.port,
                "url": endpoint.url(),
            }));
            Ok(healthy)
        }
        Err(error) => {
            emit_json(&json!({
                "running": true,
                "ready": false,
                "message": error.to_string(),
                "profile": profile.name().as_str(),
                "pid": endpoint.pid,
                "host": endpoint.host,
                "port": endpoint.port,
                "url": endpoint.url(),
            }));
            Ok(false)
        }
    }
}

pub fn stop(profile: &Profile, data_home: &Path) -> io::Result<()> {
    let runtime = profile_runtime_dir(profile.name(), data_home);
    if !runtime.exists() {
        emit_json(&json!({
            "stopped": false,
            "profile": profile.name().as_str(),
        }));
        return Ok(());
    }
    if let Some(_lock) = acquire_lock(&runtime)? {
        remove_endpoint(&runtime);
        emit_json(&json!({
            "stopped": false,
            "profile": profile.name().as_str(),
        }));
        return Ok(());
    }
    let endpoint = wait_for_endpoint(&runtime, PROBE_TIMEOUT)?;
    let addr = endpoint.addr()?;
    if let Ok(mut client) = cdp::Client::connect(addr) {
        let _ = client.call("Browser.close", &json!({}), None);
    }
    let deadline = Instant::now() + STOP_TIMEOUT;
    loop {
        if let Some(_lock) = acquire_lock(&runtime)? {
            remove_endpoint(&runtime);
            emit_json(&json!({
                "stopped": true,
                "profile": profile.name().as_str(),
                "pid": endpoint.pid,
            }));
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "daemon for profile '{}' (pid {}) did not stop within {}s",
                    profile.name().as_str(),
                    endpoint.pid,
                    STOP_TIMEOUT.as_secs()
                ),
            ));
        }
        thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(unix)]
fn detach_command(command: &mut Command) {
    use std::os::unix::process::CommandExt as _;
    command.process_group(0);
}

#[cfg(windows)]
fn detach_command(command: &mut Command) {
    use std::os::windows::process::CommandExt as _;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
}

#[cfg(not(any(unix, windows)))]
fn detach_command(_command: &mut Command) {}

fn emit_json(value: &Value) {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let _ = writeln!(out, "{value}");
    let _ = out.flush();
}

fn check_port_conflict(
    profile: &ProfileName,
    requested: Option<u16>,
    existing: &DaemonEndpoint,
) -> io::Result<()> {
    if let Some(port) = requested
        && port != 0
        && port != existing.port
    {
        return Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            format!(
                "profile '{}' is already running on {}:{} (stop it first or use --profile <NAME>)",
                profile.as_str(),
                existing.host,
                existing.port
            ),
        ));
    }
    Ok(())
}

fn extract_daemon_error(stderr: &str, status: std::process::ExitStatus) -> String {
    if let Some(reason) = extract_last_error(stderr) {
        return reason;
    }
    let trimmed = stderr.trim();
    if !trimmed.is_empty() {
        return trimmed.to_owned();
    }
    format!("daemon exited with {status}")
}

fn extract_last_error(stderr: &str) -> Option<String> {
    stderr
        .lines()
        .rev()
        .find(|line| line.contains(" ERROR "))
        .map(|line| {
            let mut parts = line.splitn(4, ' ');
            match (parts.next(), parts.next(), parts.next(), parts.next()) {
                (Some(_), Some("ERROR"), Some(_), Some(message)) if !message.is_empty() => {
                    message.to_owned()
                }
                _ => line.trim().to_owned(),
            }
        })
}

fn probe_status(endpoint: &DaemonEndpoint) -> io::Result<(bool, String)> {
    let addr = endpoint.addr()?;
    let mut stream = TcpStream::connect_timeout(&addr, PROBE_TIMEOUT)?;
    stream.set_read_timeout(Some(PROBE_TIMEOUT))?;
    stream.set_write_timeout(Some(PROBE_TIMEOUT))?;
    stream.write_all(b"GET /status HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response)?;
    let split = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid /status response"))?;
    let body = &response[split + 4..];
    let parsed: Value = serde_json::from_slice(body)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, format!("{error}")))?;
    let value = parsed
        .get("value")
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing status value"))?;
    let ready = value
        .get("ready")
        .and_then(Value::as_bool)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing ready flag"))?;
    let message = value
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    Ok((ready, message))
}

#[derive(Clone)]
struct CombinedRouter {
    cdp: cdp::Router,
    webdriver: webdriver::Router,
}

async fn serve_http(listener: &TcpListener, browser: &BrowserHandle) -> io::Result<()> {
    let bound = listener.local_addr()?;
    let shutdown = Shutdown::new();
    spawn_signal_watcher(shutdown.clone());
    let router = CombinedRouter {
        cdp: cdp::Router::new(browser.clone(), bound, shutdown.clone()),
        webdriver: webdriver::Router::with_endpoint(browser.clone(), bound, shutdown.clone()),
    };
    server::serve(listener, shutdown, router, serve_request).await
}

fn spawn_signal_watcher(shutdown: Shutdown) {
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            let mut term = signal(SignalKind::terminate()).ok();
            tokio::select! {
                _ = tokio::signal::ctrl_c() => shutdown.request(),
                () = async {
                    if let Some(ref mut stream) = term {
                        let _ = stream.recv().await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                } => shutdown.request(),
                () = shutdown.wait() => {}
            }
        }
        #[cfg(not(unix))]
        {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => shutdown.request(),
                () = shutdown.wait() => {}
            }
        }
    });
}

async fn serve_request(request: Request, router: CombinedRouter) -> Response {
    if is_webdriver_path(request.uri().path()) {
        router.webdriver.handle(request).await
    } else {
        router.cdp.handle(request).await
    }
}

fn is_webdriver_path(path: &str) -> bool {
    matches!(
        path.split('/').find(|segment| !segment.is_empty()),
        Some("status" | "session")
    )
}

fn profile_runtime_dir(name: &ProfileName, data_home: &Path) -> PathBuf {
    if let Some(dir) = env_runtime_dir() {
        return dir.join("tinybrowser").join(name.as_str());
    }
    data_home
        .join("tinybrowser")
        .join("run")
        .join(name.as_str())
}

fn env_runtime_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn acquire_lock(runtime: &Path) -> io::Result<Option<File>> {
    let path = runtime.join("lock");
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)?;
    restrict_file(&path)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(error)) => Err(error),
    }
}

fn read_endpoint(runtime: &Path) -> io::Result<DaemonEndpoint> {
    let path = runtime.join("daemon.json");
    let text = fs::read_to_string(&path)?;
    let parsed: Value = serde_json::from_str(&text)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, format!("{error}")))?;
    let pid = parsed
        .get("pid")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing pid"))?;
    let host = parsed
        .get("host")
        .and_then(Value::as_str)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing host"))?
        .to_owned();
    let port = parsed
        .get("port")
        .and_then(Value::as_u64)
        .and_then(|value| u16::try_from(value).ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing port"))?;
    Ok(DaemonEndpoint { pid, host, port })
}

fn wait_for_endpoint(runtime: &Path, timeout: Duration) -> io::Result<DaemonEndpoint> {
    let deadline = Instant::now() + timeout;
    loop {
        match read_endpoint(runtime) {
            Ok(endpoint) => return Ok(endpoint),
            Err(error) if Instant::now() >= deadline => return Err(error),
            Err(_) => thread::sleep(Duration::from_millis(20)),
        }
    }
}

fn remove_endpoint(runtime: &Path) {
    let _ = fs::remove_file(runtime.join("daemon.json"));
}

fn restrict_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

fn restrict_file(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

fn write_endpoint(runtime: &Path, endpoint: &DaemonEndpoint) -> io::Result<()> {
    let path = runtime.join("daemon.json");
    let tmp = runtime.join(format!("daemon.{}.tmp", endpoint.pid));
    let body = json!({
        "pid": endpoint.pid,
        "host": endpoint.host,
        "port": endpoint.port,
    });
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let write_result = (|| -> io::Result<()> {
        let mut file = options.open(&tmp)?;
        restrict_file(&tmp)?;
        file.write_all(body.to_string().as_bytes())?;
        file.sync_all()?;
        fs::rename(&tmp, &path)?;
        restrict_file(&path)
    })();
    if let Err(error) = write_result {
        let _removed = fs::remove_file(&tmp);
        return Err(error);
    }
    Ok(())
}
