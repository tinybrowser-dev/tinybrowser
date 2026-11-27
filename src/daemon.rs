use std::ffi::OsString;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, BufRead as _, Read as _, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use browser::{
    AgentOptions, Browser, BrowserHandle, BrowserOptions, Profile, ProfileName, RendererSandbox,
    RendererState,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use server::{Method, Request, Response, Shutdown, StatusCode};

const START_TIMEOUT: Duration = Duration::from_secs(15);
const STOP_TIMEOUT: Duration = Duration::from_secs(5);
const ENDPOINT_WAIT_TIMEOUT: Duration = Duration::from_secs(5);
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const HANDSHAKE_ENV: &str = "TINYBROWSER_START_HANDSHAKE";
const CONTROL_STATUS_PATH: &str = "/_tinybrowser/status";
const CONTROL_STOP_PATH: &str = "/_tinybrowser/stop";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DaemonEndpoint {
    pub pid: u32,
    pub host: String,
    pub port: u16,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct DaemonReady {
    profile: String,
    pid: u32,
    host: String,
    port: u16,
    url: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ControlStatus {
    ready: bool,
    message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct DaemonStatusOutput {
    running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    ready: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    profile: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    host: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum StartHandshake {
    Ready(DaemonReady),
    Failed { message: String },
}

pub struct HandshakeChannel {
    writer: Box<dyn Write + Send>,
}

impl HandshakeChannel {
    pub fn from_env() -> Option<Self> {
        std::env::var_os(HANDSHAKE_ENV)?;
        #[cfg(unix)]
        {
            use std::os::fd::AsFd as _;
            use std::os::unix::net::UnixStream;

            let fd = io::stdin().as_fd().try_clone_to_owned().ok()?;
            let stream = UnixStream::from(fd);
            Some(Self {
                writer: Box::new(stream),
            })
        }
        #[cfg(not(unix))]
        {
            Some(Self {
                writer: Box::new(io::stdout()),
            })
        }
    }

    fn send_ready(mut self, ready: &DaemonReady) {
        if let Ok(line) = serde_json::to_string(&StartHandshake::Ready(ready.clone())) {
            let _ = writeln!(self.writer, "{line}");
            let _ = self.writer.flush();
        }
    }

    pub fn send_failed(mut self, message: &str) {
        let payload = StartHandshake::Failed {
            message: message.to_owned(),
        };
        if let Ok(line) = serde_json::to_string(&payload) {
            let _ = writeln!(self.writer, "{line}");
            let _ = self.writer.flush();
        }
    }
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

    fn to_ready(&self, profile: &ProfileName) -> DaemonReady {
        DaemonReady {
            profile: profile.as_str().to_owned(),
            pid: self.pid,
            host: self.host.clone(),
            port: self.port,
            url: self.url(),
        }
    }
}

pub async fn serve(
    profile: &Profile,
    data_home: &Path,
    port: Option<u16>,
    network: AgentOptions,
    sandbox: RendererSandbox,
    mut handshake: Option<HandshakeChannel>,
) -> io::Result<()> {
    let result = serve_inner(profile, data_home, port, network, sandbox, &mut handshake).await;
    if let Err(ref error) = result
        && let Some(channel) = handshake.take()
    {
        channel.send_failed(&error.to_string());
    }
    result
}

async fn serve_inner(
    profile: &Profile,
    data_home: &Path,
    port: Option<u16>,
    network: AgentOptions,
    sandbox: RendererSandbox,
    handshake: &mut Option<HandshakeChannel>,
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
        announce_ready(handshake, &existing.to_ready(profile.name()));
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
    announce_ready(handshake, &endpoint.to_ready(profile.name()));
    let result = serve_http(&listener, browser.handle()).await;
    remove_endpoint(&runtime);
    let close = browser.close().await;
    result.and(close)
}

fn announce_ready(handshake: &mut Option<HandshakeChannel>, ready: &DaemonReady) {
    if let Some(channel) = handshake.take() {
        channel.send_ready(ready);
    } else {
        emit_serialized(ready);
    }
}

pub fn start(
    profile: &Profile,
    data_home: &Path,
    port: Option<u16>,
    serve_args: &[OsString],
) -> io::Result<()> {
    let runtime = profile_runtime_dir(profile.name(), data_home);
    fs::create_dir_all(&runtime)?;
    restrict_dir(&runtime)?;
    if let Some(lock) = acquire_lock(&runtime)? {
        drop(lock);
    } else {
        let existing = wait_for_endpoint(&runtime, ENDPOINT_WAIT_TIMEOUT)?;
        check_port_conflict(profile.name(), port, &existing)?;
        let _ = probe_control_status(&existing)?;
        emit_serialized(&existing.to_ready(profile.name()));
        return Ok(());
    }
    let mut command = Command::new(std::env::current_exe()?);
    command.args(serve_args).env(HANDSHAKE_ENV, "1");
    detach_command(&mut command);
    spawn_and_await_handshake(command)
}

#[cfg(unix)]
fn spawn_and_await_handshake(mut command: Command) -> io::Result<()> {
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixStream;

    let (parent_end, child_end) = UnixStream::pair()?;
    command
        .stdin(Stdio::from(OwnedFd::from(child_end)))
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = command.spawn()?;
    parent_end.set_read_timeout(Some(START_TIMEOUT))?;
    let mut reader = io::BufReader::new(parent_end);
    let mut line = String::new();
    match reader.read_line(&mut line) {
        Ok(0) => {
            let status = child.wait()?;
            Err(io::Error::other(format!(
                "daemon exited before completing startup handshake ({status})"
            )))
        }
        Ok(_) => decode_handshake_line(line.trim(), &mut child),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            ) =>
        {
            let _ = child.kill();
            let _ = child.wait();
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "timed out waiting for daemon to start",
            ))
        }
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            Err(error)
        }
    }
}

#[cfg(not(unix))]
fn spawn_and_await_handshake(mut command: Command) -> io::Result<()> {
    use std::sync::mpsc;

    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = command.spawn()?;
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(io::Error::other("daemon handshake pipe missing"));
    };
    let (ready_tx, ready_rx) = mpsc::channel();
    let _reader = thread::spawn(move || {
        let mut reader = io::BufReader::new(stdout);
        let mut line = String::new();
        let result = reader.read_line(&mut line).map(|_| line);
        let _ = ready_tx.send(result);
    });
    match ready_rx.recv_timeout(START_TIMEOUT) {
        Ok(Ok(line)) if !line.trim().is_empty() => decode_handshake_line(line.trim(), &mut child),
        Ok(Ok(_) | Err(_)) | Err(mpsc::RecvTimeoutError::Disconnected) => {
            let status = child.wait()?;
            Err(io::Error::other(format!(
                "daemon exited before completing startup handshake ({status})"
            )))
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            let _ = child.kill();
            let _ = child.wait();
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "timed out waiting for daemon to start",
            ))
        }
    }
}

fn decode_handshake_line(trimmed: &str, child: &mut std::process::Child) -> io::Result<()> {
    let parsed: StartHandshake = serde_json::from_str(trimmed)
        .map_err(|error| io::Error::other(format!("invalid daemon startup response: {error}")))?;
    match parsed {
        StartHandshake::Ready(ready) => {
            emit_serialized(&ready);
            Ok(())
        }
        StartHandshake::Failed { message } => {
            let _ = child.wait();
            Err(io::Error::other(message))
        }
    }
}

pub fn status(profile: &Profile, data_home: &Path) -> io::Result<bool> {
    let runtime = profile_runtime_dir(profile.name(), data_home);
    if !runtime.exists() {
        emit_serialized(&stopped_status(profile.name()));
        return Ok(false);
    }
    if let Some(_lock) = acquire_lock(&runtime)? {
        remove_endpoint(&runtime);
        emit_serialized(&stopped_status(profile.name()));
        return Ok(false);
    }
    let endpoint = wait_for_endpoint(&runtime, PROBE_TIMEOUT)?;
    match probe_control_status(&endpoint) {
        Ok(control) => {
            emit_serialized(&DaemonStatusOutput {
                running: true,
                ready: Some(control.ready),
                message: Some(control.message),
                profile: profile.name().as_str().to_owned(),
                pid: Some(endpoint.pid),
                host: Some(endpoint.host.clone()),
                port: Some(endpoint.port),
                url: Some(endpoint.url()),
            });
            Ok(control.ready)
        }
        Err(error) => {
            emit_serialized(&DaemonStatusOutput {
                running: true,
                ready: Some(false),
                message: Some(error.to_string()),
                profile: profile.name().as_str().to_owned(),
                pid: Some(endpoint.pid),
                host: Some(endpoint.host.clone()),
                port: Some(endpoint.port),
                url: Some(endpoint.url()),
            });
            Ok(false)
        }
    }
}

fn stopped_status(profile: &ProfileName) -> DaemonStatusOutput {
    DaemonStatusOutput {
        running: false,
        ready: None,
        message: None,
        profile: profile.as_str().to_owned(),
        pid: None,
        host: None,
        port: None,
        url: None,
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
    let _ = request_control_stop(&endpoint);
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

fn emit_serialized<T: Serialize>(value: &T) {
    if let Ok(line) = serde_json::to_string(value) {
        let stdout = io::stdout();
        let mut out = stdout.lock();
        let _ = writeln!(out, "{line}");
        let _ = out.flush();
    }
}

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

fn control_http_request(
    endpoint: &DaemonEndpoint,
    method: &str,
    path: &str,
) -> io::Result<Vec<u8>> {
    let addr = endpoint.addr()?;
    let mut stream = TcpStream::connect_timeout(&addr, PROBE_TIMEOUT)?;
    stream.set_read_timeout(Some(PROBE_TIMEOUT))?;
    stream.set_write_timeout(Some(PROBE_TIMEOUT))?;
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response)?;
    let split = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid control response"))?;
    Ok(response[split + 4..].to_vec())
}

fn probe_control_status(endpoint: &DaemonEndpoint) -> io::Result<ControlStatus> {
    let body = control_http_request(endpoint, "GET", CONTROL_STATUS_PATH)?;
    serde_json::from_slice(&body)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, format!("{error}")))
}

fn request_control_stop(endpoint: &DaemonEndpoint) -> io::Result<()> {
    let _ = control_http_request(endpoint, "POST", CONTROL_STOP_PATH)?;
    Ok(())
}

#[derive(Clone)]
struct CombinedRouter {
    browser: BrowserHandle,
    shutdown: Shutdown,
    cdp: cdp::Router,
    webdriver: webdriver::Router,
}

async fn serve_http(listener: &TcpListener, browser: &BrowserHandle) -> io::Result<()> {
    let bound = listener.local_addr()?;
    let shutdown = Shutdown::new();
    spawn_signal_watcher(shutdown.clone());
    let router = CombinedRouter {
        browser: browser.clone(),
        shutdown: shutdown.clone(),
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
    match request.uri().path() {
        CONTROL_STATUS_PATH => serve_control_status(&request, &router.browser).await,
        CONTROL_STOP_PATH => serve_control_stop(&request, &router.shutdown),
        path if is_webdriver_path(path) => router.webdriver.handle(request).await,
        _ => router.cdp.handle(request).await,
    }
}

fn check_control_guards(request: &Request) -> Option<Response> {
    if !server::is_loopback_host(request) {
        return Some(server::text(
            StatusCode::FORBIDDEN,
            "non-loopback Host header rejected",
        ));
    }
    if server::has_origin(request) {
        return Some(server::text(
            StatusCode::FORBIDDEN,
            "Origin header rejected",
        ));
    }
    if server::has_browser_fetch_metadata(request) {
        return Some(server::text(
            StatusCode::FORBIDDEN,
            "Sec-Fetch-* header rejected",
        ));
    }
    None
}

async fn serve_control_status(request: &Request, browser: &BrowserHandle) -> Response {
    if let Some(rejected) = check_control_guards(request) {
        return rejected;
    }
    if request.method() != Method::GET && request.method() != Method::HEAD {
        return server::method_not_allowed("GET,HEAD");
    }
    let status = match browser.renderer_health().await {
        RendererState::Live | RendererState::Starting | RendererState::NoneLive => ControlStatus {
            ready: true,
            message: "ready".to_owned(),
        },
        RendererState::Failed(error) => ControlStatus {
            ready: false,
            message: error.to_string(),
        },
    };
    match serde_json::to_value(&status) {
        Ok(value) => server::json(StatusCode::OK, &value),
        Err(_) => server::text(StatusCode::INTERNAL_SERVER_ERROR, "status encode failed"),
    }
}

fn serve_control_stop(request: &Request, shutdown: &Shutdown) -> Response {
    if let Some(rejected) = check_control_guards(request) {
        return rejected;
    }
    if request.method() != Method::POST {
        return server::method_not_allowed("POST");
    }
    shutdown.request();
    server::json(StatusCode::OK, &json!({ "stopping": true }))
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
    serde_json::from_str(&text)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, format!("{error}")))
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
    let body = serde_json::to_string(endpoint)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, format!("{error}")))?;
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
        file.write_all(body.as_bytes())?;
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
