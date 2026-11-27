use std::collections::HashMap;
use std::fmt;
use std::io;
use std::sync::Weak;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, PoisonError};

use tokio::sync::{Semaphore, mpsc, watch};
use tokio::task::JoinHandle;

use crate::actor::TabId;
use crate::assignment::{Assignment, AssignmentRegistry};
use crate::browser::TabRegistry;
use crate::child::RendererSandbox;
use crate::context::{PartitionServices, UserContextId};
use crate::link::{RendererHandle, RendererLock, RendererServices, spawn_process};
use crate::network::TabNetworkHandle;
use crate::site::Site;
use crate::storage::SessionStorage;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct RendererId(pub(crate) u64);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpawnError {
    BudgetExhausted,
    Stopped,
    Launch(String),
}

impl fmt::Display for SpawnError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BudgetExhausted => formatter.write_str("renderer process budget exhausted"),
            Self::Stopped => formatter.write_str("renderer manager stopped"),
            Self::Launch(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for SpawnError {}

impl From<SpawnError> for io::Error {
    fn from(error: SpawnError) -> Self {
        match error {
            SpawnError::Stopped => Self::new(io::ErrorKind::BrokenPipe, error.to_string()),
            SpawnError::BudgetExhausted | SpawnError::Launch(_) => Self::other(error.to_string()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RendererState {
    Live,
    Starting,
    Failed(SpawnError),
    NoneLive,
}

enum Spare {
    Empty,
    Spawning,
    Ready(Arc<RendererHandle>),
    Failed(SpawnError),
    ShuttingDown,
}

pub(crate) struct RendererProcessManager {
    inner: Arc<ManagerInner>,
}

struct ManagerInner {
    identity: net::ValidIdentity,
    sessions: Arc<SessionStorage>,
    browser: crate::browser::BrowserHandle,
    tabs: TabRegistry,
    sandbox: RendererSandbox,
    next: AtomicU64,
    slots: Arc<Semaphore>,
    registry: Arc<AssignmentRegistry>,
    state: tokio::sync::Mutex<ManagerState>,
    spare_task: tokio::sync::Mutex<Option<JoinHandle<()>>>,
    spare_gen: watch::Sender<u64>,
}

struct ManagerState {
    spare: Spare,
    sites: HashMap<RendererLock, Vec<Weak<RendererHandle>>>,
}

impl Default for ManagerState {
    fn default() -> Self {
        Self {
            spare: Spare::Empty,
            sites: HashMap::new(),
        }
    }
}

impl RendererProcessManager {
    pub(crate) fn new(
        sessions: Arc<SessionStorage>,
        browser: crate::browser::BrowserHandle,
        tabs: TabRegistry,
        identity: net::ValidIdentity,
        sandbox: RendererSandbox,
    ) -> Self {
        if sandbox == RendererSandbox::Disabled {
            logging::warn!(
                target: "browser::sandbox",
                "renderer OS sandbox disabled via --no-sandbox"
            );
        }
        let (releases, release_rx) = mpsc::unbounded_channel();
        let (spare_gen, _spare_gen_rx) = watch::channel(0);
        let inner = Arc::new(ManagerInner {
            identity,
            sessions,
            browser,
            tabs,
            sandbox,
            next: AtomicU64::new(1),
            slots: Arc::new(Semaphore::new(renderer_process_limit())),
            registry: Arc::new(AssignmentRegistry::new(releases)),
            state: tokio::sync::Mutex::new(ManagerState::default()),
            spare_task: tokio::sync::Mutex::new(None),
            spare_gen,
        });
        tokio::spawn(release_loop(Arc::downgrade(&inner), release_rx));
        Self { inner }
    }

    pub(crate) fn warm_spare(&self) {
        fill_spare(&self.inner);
    }

    pub(crate) async fn ensure_ready(&self) -> Result<(), SpawnError> {
        let mut waited_for_inflight = false;
        loop {
            let mut gen_rx = self.inner.spare_gen.subscribe();
            {
                let mut state = self.inner.state.lock().await;
                if matches!(state.spare, Spare::ShuttingDown) {
                    return Err(SpawnError::Stopped);
                }
                if has_live_renderer(&state) {
                    if matches!(state.spare, Spare::Failed(_)) {
                        state.spare = Spare::Empty;
                    }
                    return Ok(());
                }
                match &state.spare {
                    Spare::Spawning => {
                        waited_for_inflight = true;
                    }
                    Spare::Failed(error) if waited_for_inflight => {
                        return Err(error.clone());
                    }
                    Spare::Empty | Spare::Failed(_) | Spare::Ready(_) => {
                        state.spare = Spare::Spawning;
                        break;
                    }
                    Spare::ShuttingDown => return Err(SpawnError::Stopped),
                }
            }
            if gen_rx.changed().await.is_err() {
                return Err(SpawnError::Stopped);
            }
        }
        let result = spawn_slot(&self.inner, None).await;
        let mut shutdown_after = None;
        let outcome = {
            let mut state = self.inner.state.lock().await;
            if matches!(state.spare, Spare::ShuttingDown) {
                if let Ok(Some(handle)) = result {
                    shutdown_after = Some(handle);
                }
                Err(SpawnError::Stopped)
            } else {
                match result {
                    Ok(Some(spare)) => {
                        state.spare = Spare::Ready(spare);
                        Ok(())
                    }
                    Ok(None) if has_live_renderer(&state) => {
                        state.spare = Spare::Empty;
                        Ok(())
                    }
                    Ok(None) => {
                        state.spare = Spare::Failed(SpawnError::BudgetExhausted);
                        Err(SpawnError::BudgetExhausted)
                    }
                    Err(error) => {
                        let err = SpawnError::Launch(error.to_string());
                        state.spare = Spare::Failed(err.clone());
                        Err(err)
                    }
                }
            }
        };
        notify_spare(&self.inner);
        if let Some(handle) = shutdown_after {
            handle.shutdown().await;
        }
        outcome
    }

    pub(crate) async fn health(&self) -> RendererState {
        let state = self.inner.state.lock().await;
        if has_live_renderer(&state) {
            return RendererState::Live;
        }
        match &state.spare {
            Spare::Spawning => RendererState::Starting,
            Spare::Failed(error) => RendererState::Failed(error.clone()),
            Spare::ShuttingDown => RendererState::Failed(SpawnError::Stopped),
            Spare::Empty | Spare::Ready(_) => RendererState::NoneLive,
        }
    }

    pub(crate) async fn shutdown(&self) {
        let (spare, live) = {
            let mut state = self.inner.state.lock().await;
            let prev = std::mem::replace(&mut state.spare, Spare::ShuttingDown);
            let spare = match prev {
                Spare::Ready(handle) => Some(handle),
                Spare::Empty | Spare::Spawning | Spare::Failed(_) | Spare::ShuttingDown => None,
            };
            let mut live = Vec::new();
            for processes in state.sites.values() {
                for candidate in processes {
                    if let Some(handle) = candidate.upgrade() {
                        live.push(handle);
                    }
                }
            }
            state.sites.clear();
            (spare, live)
        };
        notify_spare(&self.inner);
        let inflight = self.inner.spare_task.lock().await.take();
        if let Some(handle) = inflight {
            let _ = handle.await;
        }
        if let Some(handle) = spare {
            handle.shutdown().await;
        }
        for handle in live {
            handle.shutdown().await;
        }
    }

    pub(crate) async fn acquire(
        &self,
        tab: TabId,
        user_context: UserContextId,
        site: Site,
        network: TabNetworkHandle,
        partition: PartitionServices,
    ) -> io::Result<Arc<Assignment>> {
        let mut last_error = None;
        for _ in 0..2 {
            let (process, assignment) = self
                .acquire_reserved(
                    tab,
                    user_context.clone(),
                    site.clone(),
                    network.clone(),
                    partition.clone(),
                )
                .await?;
            match process.assign_notify(assignment.id()).await {
                Ok(()) => {
                    fill_spare(&self.inner);
                    return Ok(assignment);
                }
                Err(error) => {
                    process.alive.store(false, Ordering::Relaxed);
                    drop(assignment);
                    last_error = Some(error);
                }
            }
        }
        Err(last_error.unwrap_or_else(|| io::Error::other("renderer assignment failed")))
    }

    async fn acquire_reserved(
        &self,
        tab: TabId,
        user_context: UserContextId,
        site: Site,
        network: TabNetworkHandle,
        partition: PartitionServices,
    ) -> io::Result<(Arc<RendererHandle>, Arc<Assignment>)> {
        let lock = RendererLock {
            user_context: user_context.clone(),
            site: site.clone(),
        };
        let mut state = self.inner.state.lock().await;
        if matches!(state.spare, Spare::ShuttingDown) {
            return Err(SpawnError::Stopped.into());
        }
        let spare = match std::mem::replace(&mut state.spare, Spare::Empty) {
            Spare::Ready(handle) if handle.alive.load(Ordering::Relaxed) => Some(handle),
            Spare::Ready(_) | Spare::Empty => None,
            other @ (Spare::Spawning | Spare::Failed(_) | Spare::ShuttingDown) => {
                state.spare = other;
                None
            }
        };
        let process = if let Some(spare) = spare {
            spare.bind(&lock, &partition, &self.inner.registry, &self.inner.tabs)?;
            spare
        } else {
            match spawn_slot(&self.inner, Some((lock.clone(), partition.clone()))).await {
                Ok(Some(process)) => {
                    if matches!(state.spare, Spare::Failed(_)) {
                        state.spare = Spare::Empty;
                    }
                    process
                }
                Ok(None) => {
                    let candidates = state.sites.entry(lock.clone()).or_default();
                    candidates.retain(|candidate| {
                        candidate
                            .upgrade()
                            .is_some_and(|process| process.alive.load(Ordering::Relaxed))
                    });
                    candidates
                        .iter()
                        .find_map(Weak::upgrade)
                        .ok_or_else(|| io::Error::from(SpawnError::BudgetExhausted))?
                }
                Err(error) => {
                    if !matches!(state.spare, Spare::Spawning | Spare::ShuttingDown) {
                        state.spare = Spare::Failed(SpawnError::Launch(error.to_string()));
                    }
                    return Err(error);
                }
            }
        };
        let processes = state.sites.entry(lock).or_default();
        if !processes.iter().any(|candidate| {
            candidate
                .upgrade()
                .is_some_and(|candidate| Arc::ptr_eq(&candidate, &process))
        }) {
            processes.push(Arc::downgrade(&process));
        }
        let assignment =
            self.inner
                .registry
                .reserve(Arc::clone(&process), tab, site, network, partition);
        Ok((process, assignment))
    }
}

async fn release_loop(
    inner: Weak<ManagerInner>,
    mut releases: mpsc::UnboundedReceiver<Arc<RendererHandle>>,
) {
    while let Some(process) = releases.recv().await {
        let Some(inner) = inner.upgrade() else {
            return;
        };
        let lock = process
            .lock
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let (shutdown, refill) = {
            let mut state = inner.state.lock().await;
            let shutting_down = matches!(state.spare, Spare::ShuttingDown);
            let mut shutdown = false;
            if process.assignments.load(Ordering::Relaxed) == 0 {
                if let Some(lock) = &lock
                    && let Some(processes) = state.sites.get_mut(lock)
                {
                    processes.retain(|candidate| {
                        candidate
                            .upgrade()
                            .is_some_and(|candidate| !Arc::ptr_eq(&candidate, &process))
                    });
                    if processes.is_empty() {
                        state.sites.remove(lock);
                    }
                }
                shutdown = true;
            }
            (shutdown, !shutting_down)
        };
        if shutdown {
            process.shutdown().await;
        }
        drop(process);
        if refill {
            fill_spare(&inner);
        }
    }
}

fn has_live_renderer(state: &ManagerState) -> bool {
    matches!(&state.spare, Spare::Ready(spare) if spare.alive.load(Ordering::Relaxed))
        || state.sites.values().any(|processes| {
            processes.iter().any(|candidate| {
                candidate
                    .upgrade()
                    .is_some_and(|process| process.alive.load(Ordering::Relaxed))
            })
        })
}

fn notify_spare(inner: &ManagerInner) {
    let next = inner.spare_gen.borrow().wrapping_add(1);
    let _ = inner.spare_gen.send(next);
}

fn fill_spare(inner: &Arc<ManagerInner>) {
    let inner_clone = Arc::clone(inner);
    let handle = tokio::spawn(async move {
        {
            let mut state = inner_clone.state.lock().await;
            match &state.spare {
                Spare::Ready(spare) if spare.alive.load(Ordering::Relaxed) => return,
                Spare::Spawning | Spare::ShuttingDown => return,
                Spare::Empty | Spare::Failed(_) | Spare::Ready(_) => {
                    state.spare = Spare::Spawning;
                }
            }
        }
        let result = spawn_slot(&inner_clone, None).await;
        let mut shutdown_after = None;
        {
            let mut state = inner_clone.state.lock().await;
            if matches!(state.spare, Spare::ShuttingDown) {
                if let Ok(Some(handle)) = result {
                    shutdown_after = Some(handle);
                }
            } else {
                match result {
                    Ok(Some(spare)) => state.spare = Spare::Ready(spare),
                    Ok(None) => state.spare = Spare::Empty,
                    Err(error) => {
                        state.spare = Spare::Failed(SpawnError::Launch(error.to_string()));
                    }
                }
            }
        }
        notify_spare(&inner_clone);
        if let Some(handle) = shutdown_after {
            handle.shutdown().await;
        }
    });
    let track = Arc::clone(inner);
    tokio::spawn(async move {
        let mut slot = track.spare_task.lock().await;
        if let Some(previous) = slot.replace(handle)
            && previous.is_finished()
        {
            let _ = previous.await;
        }
    });
}

async fn spawn_slot(
    inner: &Arc<ManagerInner>,
    bound: Option<(RendererLock, PartitionServices)>,
) -> io::Result<Option<Arc<RendererHandle>>> {
    let Ok(slot) = Arc::clone(&inner.slots).try_acquire_owned() else {
        return Ok(None);
    };
    let id = RendererId(inner.next.fetch_add(1, Ordering::Relaxed));
    Ok(Some(Arc::new(
        spawn_process(
            id,
            bound,
            RendererServices {
                identity: inner.identity.clone(),
                sessions: Arc::clone(&inner.sessions),
                browser: inner.browser.clone(),
                registry: Arc::clone(&inner.registry),
                tabs: inner.tabs.clone(),
                sandbox: inner.sandbox,
            },
            slot,
        )
        .await?,
    )))
}

fn renderer_process_limit() -> usize {
    if let Some(limit) = std::env::var_os("TINYBROWSER_RENDERER_PROCESS_LIMIT")
        .and_then(|value| value.to_str()?.parse::<usize>().ok())
        .filter(|limit| *limit > 0)
    {
        return limit;
    }
    if let Some(available_kib) = system_memory_kib() {
        return (available_kib / (64 * 1024)).clamp(1, 128);
    }
    std::thread::available_parallelism()
        .map_or(1, std::num::NonZeroUsize::get)
        .saturating_mul(2)
        .clamp(1, 128)
}

fn system_memory_kib() -> Option<usize> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        let text = std::fs::read_to_string("/proc/meminfo").ok()?;
        parse_meminfo_kib(&text, "MemAvailable:").or_else(|| parse_meminfo_kib(&text, "MemTotal:"))
    }
    #[cfg(target_os = "macos")]
    {
        let output = std::process::Command::new("sysctl")
            .args(["-n", "hw.memsize"])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let bytes = std::str::from_utf8(&output.stdout)
            .ok()?
            .trim()
            .parse::<u64>()
            .ok()?;
        usize::try_from(bytes / 1024).ok()
    }
    #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
    {
        None
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn parse_meminfo_kib(text: &str, prefix: &str) -> Option<usize> {
    text.lines().find_map(|line| {
        line.strip_prefix(prefix)?
            .split_whitespace()
            .next()?
            .parse::<usize>()
            .ok()
    })
}
