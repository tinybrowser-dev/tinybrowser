use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::net::TcpListener;
use std::path::{Path, PathBuf};

use browser::{
    AgentOptions, Browser, BrowserHandle, BrowserOptions, Profile, ProfileName, RendererSandbox,
};
use serde_json::json;
use server::{Request, Response, Shutdown};

#[derive(Clone, Debug)]
pub struct DaemonEndpoint {
    pub pid: u32,
    pub host: String,
    pub port: u16,
}

pub async fn run(
    profile: &Profile,
    data_home: &Path,
    port: Option<u16>,
    network: AgentOptions,
    sandbox: RendererSandbox,
) -> io::Result<()> {
    let registration = match port {
        None => {
            let runtime = profile_runtime_dir(profile.name(), data_home);
            fs::create_dir_all(&runtime)?;
            restrict_dir(&runtime)?;
            let Some(lock) = acquire_lock(&runtime)? else {
                logging::info!(
                    target: "daemon",
                    "profile {} already has a running daemon",
                    profile.name().as_str()
                );
                return Ok(());
            };
            logging::debug!(target: "daemon", "runtime dir {}", runtime.display());
            Some((runtime, lock))
        }
        Some(_) => try_optional_registration(profile.name(), data_home),
    };
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
    let initial = browser
        .handle()
        .create_tab()
        .await
        .map_err(|error| io::Error::other(error.to_string()))?;
    initial
        .load_html("<!doctype html><title></title>")
        .await
        .map_err(|error| io::Error::other(error.to_string()))?;
    if let Some((runtime, _lock)) = &registration {
        write_endpoint(
            runtime,
            &DaemonEndpoint {
                pid: std::process::id(),
                host: "127.0.0.1".into(),
                port: addr.port(),
            },
        )?;
    }
    let result = serve(&listener, browser.handle()).await;
    let close = browser.close().await;
    result.and(close)
}

#[derive(Clone)]
struct CombinedRouter {
    cdp: cdp::Router,
    webdriver: webdriver::Router,
}

async fn serve(listener: &TcpListener, browser: &BrowserHandle) -> io::Result<()> {
    let bound = listener.local_addr()?;
    let shutdown = Shutdown::new();
    let router = CombinedRouter {
        cdp: cdp::Router::new(browser.clone(), bound, shutdown.clone()),
        webdriver: webdriver::Router::with_endpoint(browser.clone(), bound, shutdown.clone()),
    };
    server::serve(listener, shutdown, router, serve_request).await
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

fn try_optional_registration(name: &ProfileName, data_home: &Path) -> Option<(PathBuf, File)> {
    let runtime = profile_runtime_dir(name, data_home);
    fs::create_dir_all(&runtime).ok()?;
    restrict_dir(&runtime).ok()?;
    let lock = acquire_lock(&runtime).ok()??;
    logging::debug!(target: "daemon", "runtime dir {}", runtime.display());
    Some((runtime, lock))
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
    use std::io::Write as _;

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
