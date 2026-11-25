use std::collections::HashMap;
use std::io;
use std::sync::Weak;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, PoisonError};

use tokio::sync::{Semaphore, mpsc};

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
}

#[derive(Default)]
struct ManagerState {
    spare: Option<Arc<RendererHandle>>,
    sites: HashMap<RendererLock, Vec<Weak<RendererHandle>>>,
    spawning_spare: bool,
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
        });
        tokio::spawn(release_loop(Arc::downgrade(&inner), release_rx));
        fill_spare(&inner);
        Self { inner }
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
            spare
        } else if let Some(process) =
            spawn_slot(&self.inner, Some((lock.clone(), partition.clone()))).await?
        {
            process
        } else {
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
        let result = match spawn_slot(&inner, None).await {
            Ok(Some(spare)) => Ok(spare),
            Ok(None) => Err(io::Error::other("renderer process budget exhausted")),
            Err(error) => Err(error),
        };
        let mut state = inner.state.lock().await;
        state.spawning_spare = false;
        if let Ok(spare) = result {
            state.spare = Some(spare);
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
