//! A per-worker deadline, so a file that never returns ends one process
//! instead of the chunk.
//!
//! The harness budget cannot interrupt code that never comes back to the
//! harness loop: a layout recursion, an endless promise chain, a drain that
//! never yields. Those cannot be unwound either, so the only recovery is the
//! process ending, after which the driver records that file as TIMEOUT and
//! resumes from the journal. This is what notices, and it has to notice before
//! the recursion overflows the stack and aborts with no verdict at all.

use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

/// How long a file may run past its budget before the watchdog gives up on it.
///
/// The budget already cuts a file at 5 s (7 s when it asks for a long timeout),
/// so only a file that never returns to the harness loop reaches the grace.
/// Half a minute is far above the slowest real file and far below a stack
/// overflow on the deepest page the suite carries.
pub const GRACE: Duration = Duration::from_secs(30);

/// One worker's current file and when to give up on it.
///
/// Holding both fields behind one lock prevents a reader from observing an
/// expired deadline paired with a stale or cleared case index.
struct Slot {
    active: Mutex<Option<(usize, u64)>>,
}

/// The deadline of every worker's current file.
pub struct Watchdog {
    epoch: Instant,
    slots: Vec<Slot>,
}

impl Watchdog {
    /// Creates one slot per worker.
    #[must_use]
    pub fn new(workers: usize) -> Self {
        Self {
            epoch: Instant::now(),
            slots: (0..workers)
                .map(|_| Slot {
                    active: Mutex::new(None),
                })
                .collect(),
        }
    }

    /// Starts the clock for `case` on `worker`, which may run for `budget` plus
    /// [`GRACE`].
    pub fn enter(&self, worker: usize, case: usize, budget: Duration) {
        let expires = self.millis(Instant::now() + budget + GRACE);
        *self.slots[worker]
            .active
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some((case, expires));
    }

    /// Marks `worker` idle again.
    pub fn leave(&self, worker: usize) {
        *self.slots[worker]
            .active
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
    }

    /// The case that has been running past its deadline, if any.
    pub fn expired(&self) -> Option<usize> {
        self.expired_at(Instant::now())
    }

    /// The body of [`Self::expired`], with the clock injected for the tests.
    fn expired_at(&self, now: Instant) -> Option<usize> {
        let now = self.millis(now);
        self.slots.iter().find_map(|slot| {
            let (case, expires) = (*slot.active.lock().unwrap_or_else(PoisonError::into_inner))?;
            (now > expires).then_some(case)
        })
    }

    fn millis(&self, at: Instant) -> u64 {
        u64::try_from(at.duration_since(self.epoch).as_millis()).unwrap_or(u64::MAX)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{GRACE, Watchdog};

    /// A file inside its budget is never expired, however long it takes
    /// relative to the other workers.
    #[test]
    fn a_file_within_its_budget_is_not_expired() {
        let watchdog = Watchdog::new(2);
        watchdog.enter(1, 7, Duration::from_secs(5));
        assert_eq!(
            watchdog.expired_at(watchdog.epoch + Duration::from_secs(6)),
            None
        );
    }

    /// The expired worker is named by the case it holds, so the verdict can be
    /// written for the right file, and leaving clears it.
    #[test]
    fn a_file_past_its_budget_and_grace_is_expired_by_case() {
        let watchdog = Watchdog::new(2);
        watchdog.enter(1, 7, Duration::from_secs(5));
        let at = watchdog.epoch + Duration::from_secs(5) + GRACE + Duration::from_millis(1);
        assert_eq!(watchdog.expired_at(at), Some(7));
        watchdog.leave(1);
        assert_eq!(watchdog.expired_at(at), None);
    }
}
