use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

pub const GRACE: Duration = Duration::from_secs(30);

struct Slot {
    active: Mutex<Option<(usize, u64)>>,
}

pub struct Watchdog {
    epoch: Instant,
    slots: Vec<Slot>,
}

impl Watchdog {
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

    pub fn enter(&self, worker: usize, case: usize, budget: Duration) {
        let expires = self.millis(Instant::now() + budget + GRACE);
        *self.slots[worker]
            .active
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some((case, expires));
    }

    pub fn leave(&self, worker: usize) {
        *self.slots[worker]
            .active
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
    }

    pub fn expired(&self) -> Option<usize> {
        self.expired_at(Instant::now())
    }

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

    #[test]
    fn a_file_within_its_budget_is_not_expired() {
        let watchdog = Watchdog::new(2);
        watchdog.enter(1, 7, Duration::from_secs(5));
        assert_eq!(
            watchdog.expired_at(watchdog.epoch + Duration::from_secs(6)),
            None
        );
    }

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
