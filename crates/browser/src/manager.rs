use std::collections::HashMap;
use std::io;
use std::sync::Weak;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, PoisonError};

use tokio::sync::{Semaphore, mpsc, watch};

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
    spare_gen: watch::Sender<u64>,
}

#[derive(Default)]
struct ManagerState {
    spare: Option<Arc<RendererHandle>>,
    sites: HashMap<RendererLock, Vec<Weak<RendererHandle>>>,
    spawning_spare: bool,
    last_spawn_error: Option<String>,
}

impl RendererProcessManager {
    pub(crate) fn new(
        sessions: Arc<SessionStorage>,
        browser: crate::browser::BrowserHandle,
        tabs: TabRegistry,
        identity: net::ValidIdentity,
        sandbox: RendererSandbox,
    ) -> Self {
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
            spare_gen,
        });
        tokio::spawn(release_loop(Arc::downgrade(&inner), release_rx));
        fill_spare(&inner);
        Self { inner }
    }

    pub(crate) async fn ensure_ready(&self) -> io::Result<()> {
        let mut waited_for_inflight = false;
        loop {
            let mut gen_rx = self.inner.spare_gen.subscribe();
            {
                let mut state = self.inner.state.lock().await;
                if has_live_renderer(&state) {
                    state.last_spawn_error = None;
                    return Ok(());
                }
                if state.spawning_spare {
                    waited_for_inflight = true;
                } else if waited_for_inflight {
                    let message = state
                        .last_spawn_error
                        .clone()
                        .unwrap_or_else(|| "renderer failed to start".to_owned());
                    return Err(io::Error::other(message));
                } else {
                    state.spawning_spare = true;
                    break;
                }
            }
            if gen_rx.changed().await.is_err() {
                return Err(io::Error::other("renderer manager stopped"));
            }
        }
        let result = spawn_slot(&self.inner, None).await;
        let mut state = self.inner.state.lock().await;
        state.spawning_spare = false;
        let outcome = match result {
            Ok(Some(spare)) => {
                state.spare = Some(spare);
                state.last_spawn_error = None;
                Ok(())
            }
            Ok(None) if has_live_renderer(&state) => {
                state.last_spawn_error = None;
                Ok(())
            }
            Ok(None) => {
                let error = io::Error::other("renderer process budget exhausted");
                state.last_spawn_error = Some(error.to_string());
                Err(error)
            }
            Err(error) => {
                state.last_spawn_error = Some(error.to_string());
                Err(error)
            }
        };
        let next = self.inner.spare_gen.borrow().wrapping_add(1);
        let _ = self.inner.spare_gen.send(next);
        outcome
    }

    pub(crate) async fn health(&self) -> Result<(), String> {
        let state = self.inner.state.lock().await;
        if has_live_renderer(&state) {
            return Ok(());
        }
        if let Some(error) = &state.last_spawn_error {
            return Err(error.clone());
        }
        Ok(())
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
        let spare = state
            .spare
            .take()
            .filter(|spare| spare.alive.load(Ordering::Relaxed));
        let process = if let Some(spare) = spare {
            spare.bind(&lock, &partition, &self.inner.registry, &self.inner.tabs)?;
            state.last_spawn_error = None;
            spare
        } else {
            match spawn_slot(&self.inner, Some((lock.clone(), partition.clone()))).await {
                Ok(Some(process)) => {
                    state.last_spawn_error = None;
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
                        .ok_or_else(|| io::Error::other("renderer process budget exhausted"))?
                }
                Err(error) => {
                    state.last_spawn_error = Some(error.to_string());
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
        let mut shutdown = false;
        {
            let mut state = inner.state.lock().await;
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
        }
        if shutdown {
            process.shutdown().await;
        }
        drop(process);
        fill_spare(&inner);
    }
}

fn has_live_renderer(state: &ManagerState) -> bool {
    state
        .spare
        .as_ref()
        .is_some_and(|spare| spare.alive.load(Ordering::Relaxed))
        || state.sites.values().any(|processes| {
            processes.iter().any(|candidate| {
                candidate
                    .upgrade()
                    .is_some_and(|process| process.alive.load(Ordering::Relaxed))
            })
        })
}

fn fill_spare(inner: &Arc<ManagerInner>) {
    let inner = Arc::clone(inner);
    tokio::spawn(async move {
        {
            let mut state = inner.state.lock().await;
            if state.spare.is_some() || state.spawning_spare {
                return;
            }
            state.spawning_spare = true;
        }
        let result = spawn_slot(&inner, None).await;
        let mut state = inner.state.lock().await;
        state.spawning_spare = false;
        match result {
            Ok(Some(spare)) => {
                state.spare = Some(spare);
                state.last_spawn_error = None;
            }
            Ok(None) => {}
            Err(error) => {
                state.last_spawn_error = Some(error.to_string());
            }
        }
        let next = inner.spare_gen.borrow().wrapping_add(1);
        let _ = inner.spare_gen.send(next);
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
