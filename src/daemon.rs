//! Named profile daemon: loopback CDP, runtime-dir registration, startup lock.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};

use browser::{Browser, BrowserOptions, Profile, ProfileName};
use serde_json::json;

/// Registration recorded at `$XDG_RUNTIME_DIR/tinybrowser/<profile>/daemon.json`.
#[derive(Clone, Debug)]
pub struct DaemonEndpoint {
    /// Process id of the daemon.
    pub pid: u32,
    /// Loopback host, always `127.0.0.1`.
    pub host: String,
    /// Bound port.
    pub port: u16,
}

/// Runs the profile daemon until the process exits.
///
/// # Errors
///
/// Bind, registration, or serve failure.
pub async fn run(profile: &Profile, data_home: &Path) -> io::Result<()> {
    let runtime = profile_runtime_dir(profile.name())?;
    fs::create_dir_all(&runtime)?;
    restrict_dir(&runtime)?;
    let Some(_lock) = acquire_lock(&runtime)? else {
        logging::info!(
            target: "daemon",
            "profile {} already has a running daemon",
            profile.name().as_str()
        );
        return Ok(());
    };
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let addr = listener.local_addr()?;
    logging::debug!(target: "daemon", "runtime dir {}", runtime.display());
    logging::info!(
        target: "daemon",
        "serving profile {} on 127.0.0.1:{}",
        profile.name().as_str(),
        addr.port()
    );
    let browser = Browser::new(BrowserOptions {
        data_home: Some(data_home.to_path_buf()),
        profile: profile.clone(),
        ..BrowserOptions::default()
    })
    .map_err(io::Error::from)?;
    // Real browsers start with one page target; clients (Playwright included)
    // assume at least one top-level traversable exists.
    let initial = browser
        .handle()
        .create_tab()
        .await
        .map_err(|error| io::Error::other(error.to_string()))?;
    initial
        .load_html("<!doctype html><title></title>")
        .await
        .map_err(|error| io::Error::other(error.to_string()))?;
    // Publish only once the browser can actually serve; a start-up failure
    // must not leave an endpoint pointing at a dead port.
    write_endpoint(
        &runtime,
        &DaemonEndpoint {
            pid: std::process::id(),
            host: "127.0.0.1".into(),
            port: addr.port(),
        },
    )?;
    let result = cdp::serve(&listener, browser.handle()).await;
    if result.is_err() {
        let _close_result = browser.close().await;
    }
    // The lock file stays: the kernel lock on its open description is what
    // says "held", and unlinking it would let a third process create a second
    // lock file while this one is still owned.
    result
}

fn profile_runtime_dir(name: &ProfileName) -> io::Result<PathBuf> {
    Ok(runtime_home()?.join("tinybrowser").join(name.as_str()))
}

fn runtime_home() -> io::Result<PathBuf> {
    if let Some(dir) = env_runtime_dir() {
        return Ok(dir);
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "XDG_RUNTIME_DIR is unset",
    ))
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
        Ok(()) => {
            file.set_len(0)?;
            writeln!(&file, "{}", std::process::id())?;
            file.sync_all()?;
            Ok(Some(file))
        }
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(error)) => Err(error),
    }
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
    let tmp = runtime.join("daemon.json.tmp");
    let body = json!({
        "pid": endpoint.pid,
        "host": endpoint.host,
        "port": endpoint.port,
    });
    fs::write(&tmp, body.to_string())?;
    restrict_file(&tmp)?;
    fs::rename(&tmp, &path)?;
    restrict_file(&path)
}
