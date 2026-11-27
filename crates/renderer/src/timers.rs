use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::protocol::TimerId;

struct ScheduledTimer<T> {
    // https://html.spec.whatwg.org/multipage/timers-and-user-prompts.html#dom-settimeout
    when: Option<Instant>,
    fired: bool,
    payload: T,
}

pub(crate) struct TimerQueue<T> {
    timers: HashMap<TimerId, ScheduledTimer<T>>,
    next_id: TimerId,
}

impl<T> Default for TimerQueue<T> {
    fn default() -> Self {
        Self {
            timers: HashMap::new(),
            next_id: TimerId::default(),
        }
    }
}

impl<T> TimerQueue<T> {
    pub(crate) fn schedule(&mut self, now: Instant, delay: Duration, payload: T) -> TimerId {
        let id = self.next_id.take_next();
        self.timers.insert(
            id,
            ScheduledTimer {
                when: now.checked_add(delay),
                fired: false,
                payload,
            },
        );
        id
    }

    pub(crate) fn remove(&mut self, id: TimerId) -> Option<T> {
        self.timers.remove(&id).map(|timer| timer.payload)
    }

    pub(crate) fn retain(&mut self, mut predicate: impl FnMut(&TimerId, &T) -> bool) {
        self.timers
            .retain(|id, timer| predicate(id, &timer.payload));
    }

    pub(crate) fn clear(&mut self) {
        self.timers.clear();
    }

    pub(crate) fn has_due(&self, now: Instant) -> bool {
        self.timers
            .values()
            .any(|timer| !timer.fired && timer.when.is_some_and(|when| when <= now))
    }

    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.timers
            .values()
            .filter(|timer| !timer.fired)
            .filter_map(|timer| timer.when)
            .min()
    }

    // https://html.spec.whatwg.org/multipage/timers-and-user-prompts.html#timer-initialisation-steps
    // https://html.spec.whatwg.org/multipage/webappapis.html#task-queue
    pub(crate) fn take_due(&mut self, now: Instant) -> Option<TimerId> {
        let (id, timer) = self
            .timers
            .iter_mut()
            .filter(|(_, timer)| !timer.fired && timer.when.is_some_and(|when| when <= now))
            .min_by_key(|(id, timer)| (timer.when, id.get()))?;
        timer.fired = true;
        Some(*id)
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.timers.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::TimerQueue;

    #[test]
    fn due_timers_fire_earliest_then_by_registration_order() {
        let base = Instant::now();
        let mut queue = TimerQueue::default();
        let late = queue.schedule(base, Duration::from_millis(50), "late");
        let early_a = queue.schedule(base, Duration::from_millis(10), "early_a");
        let early_b = queue.schedule(base, Duration::from_millis(10), "early_b");

        assert_eq!(
            queue.next_deadline(),
            Some(base + Duration::from_millis(10))
        );
        assert!(!queue.has_due(base + Duration::from_millis(9)));
        assert!(queue.has_due(base + Duration::from_millis(10)));

        let at_100 = base + Duration::from_millis(100);
        assert_eq!(queue.take_due(at_100), Some(early_a));
        assert_eq!(queue.take_due(at_100), Some(early_b));
        assert_eq!(queue.take_due(at_100), Some(late));
        assert_eq!(queue.take_due(at_100), None);
        assert_eq!(queue.remove(early_a), Some("early_a"));
    }

    #[test]
    fn cancelling_a_timer_removes_it_before_delivery() {
        let base = Instant::now();
        let mut queue = TimerQueue::default();
        let id = queue.schedule(base, Duration::from_millis(20), 42u32);
        assert_eq!(queue.remove(id), Some(42));
        assert!(queue.is_empty());
        assert_eq!(queue.next_deadline(), None);
        assert_eq!(queue.take_due(base + Duration::from_secs(1)), None);
    }
}
