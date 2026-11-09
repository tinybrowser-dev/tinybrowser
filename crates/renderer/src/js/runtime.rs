//! Process-wide handles shared by every frame document.
//!
//! [`FrameRuntime`] is the renderer's shared runtime: browser services, the JS
//! heap, the wake handle, the stop flag, and the cross-frame stores. It lives
//! beside the realm bookkeeping it carries so that `document` and `engine`
//! read it from below rather than defining it.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use tokio::sync::Notify;

use crate::documents::DocumentStore;
use crate::messaging::SharedHandle;
use crate::protocol::BrowserServices;

use super::JsRuntimeHandle;
use super::RealmRegistry;

/// The process-wide handles every frame document shares: services, the JS
/// heap, the wake handle, the stop flag, and the cross-frame stores.
#[derive(Clone)]
pub(crate) struct FrameRuntime {
    pub(crate) services: Arc<dyn BrowserServices>,
    /// The browser identity this process was spawned with: the same value the
    /// network uses for `User-Agent` and the client hints, so a page, a CSP
    /// report, and a request cannot name different products
    /// (<https://wicg.github.io/ua-client-hints/#http-ua-hints>).
    pub(crate) identity: net::ValidIdentity,
    pub(crate) js_runtime: JsRuntimeHandle,
    pub(crate) wake: Arc<Notify>,
    pub(crate) stop: Arc<Stop>,
    pub(crate) documents: Rc<RefCell<DocumentStore>>,
    pub(crate) registry: Rc<RefCell<RealmRegistry>>,
    pub(crate) shared: SharedHandle,
    /// Storage changes waiting for the `storage`-event task in their receiving
    /// frames.
    pub(crate) pending_storage: Rc<RefCell<Vec<crate::storage::PendingStorageEvent>>>,
    /// Deadline for the current renderer turn, shared by every realm. A drain
    /// sets it once; serial tasks clamp to it instead of each taking a fresh
    /// budget (`ScriptTurn`).
    pub(crate) turn_deadline: Rc<Cell<Option<Instant>>>,
    /// Worker lifecycle and message requests the engine materializes after a
    /// script returns. `QuickJS` cannot create a worker's context while the
    /// constructor's script is still running.
    pub(crate) worker_requests: Rc<RefCell<Vec<crate::worker::WorkerRequest>>>,
    /// Worker ids are minted by the realm that constructs one, so the
    /// constructor can name its worker before the engine materializes it.
    pub(crate) next_worker: Rc<Cell<u64>>,
    /// Parley's font context, built once per process: construction scans
    /// system fonts (~25ms), so every `BaseDocument` clones this instead of
    /// rescanning. Cloning is cheap; Blitz shares the source cache.
    pub(crate) font_ctx: parley::FontContext,
}

impl FrameRuntime {
    /// Opens one turn on the shared deadline. Nested enters join it; dropping
    /// the owner clears the cell.
    pub(crate) fn open_turn(&self, deadline: Instant) -> super::ScriptTurn {
        super::ScriptTurn::enter_at(&self.turn_deadline, deadline)
    }
}

/// Stop flag shared by a renderer loop, the JS interrupt handler, and dials.
///
/// Requesting a stop interrupts running script and marks the flag; every
/// engine sharing it winds down at its next turn.
pub struct Stop {
    flag: AtomicBool,
}

impl Default for Stop {
    fn default() -> Self {
        Self::new()
    }
}

impl Stop {
    /// A fresh, unset stop flag.
    #[must_use]
    pub fn new() -> Self {
        Self {
            flag: AtomicBool::new(false),
        }
    }

    /// Requests a stop: interrupts `QuickJS` and marks the flag.
    pub fn request(&self) {
        self.flag.store(true, Ordering::Relaxed);
    }

    /// Whether [`Stop::request`] has run.
    #[must_use]
    pub fn is_set(&self) -> bool {
        self.flag.load(Ordering::Relaxed)
    }
}
