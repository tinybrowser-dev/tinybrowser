// https://html.spec.whatwg.org/multipage/webappapis.html#event-loop

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use crate::dom::tree::NodeId;

// https://tc39.es/ecma262/#sec-code-realms
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct RealmId(pub(crate) u64);

impl RealmId {
    pub(crate) const NONE: Self = Self(0);
}

// https://html.spec.whatwg.org/multipage/webappapis.html#task-source
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TaskSource {
    DomManipulation,
    UserInteraction,
    Networking,
    Timer,
    PostedMessage,
}

impl TaskSource {
    const COUNT: usize = 5;

    const ALL: [Self; Self::COUNT] = [
        Self::DomManipulation,
        Self::UserInteraction,
        Self::Networking,
        Self::Timer,
        Self::PostedMessage,
    ];

    const fn index(self) -> usize {
        match self {
            Self::DomManipulation => 0,
            Self::UserInteraction => 1,
            Self::Networking => 2,
            Self::Timer => 3,
            Self::PostedMessage => 4,
        }
    }
}

// https://html.spec.whatwg.org/multipage/webappapis.html#event-loop
pub(crate) enum RealmTask {
    // https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#set-the-selection-range
    SelectionChanged(NodeId),
    // https://html.spec.whatwg.org/multipage/interactive-elements.html#queue-a-details-toggle-event-task
    // https://html.spec.whatwg.org/multipage/interactive-elements.html#queue-a-dialog-toggle-event-task
    ToggleEvent {
        id: NodeId,
        old_state: String,
        new_state: String,
    },
    // https://html.spec.whatwg.org/multipage/interactive-elements.html#the-dialog-element:close-event
    DialogClose(NodeId),
    // https://html.spec.whatwg.org/multipage/webappapis.html#notify-about-rejected-promises
    RejectionTasks(RealmId),
}

struct TaskQueueInner<T> {
    queues: [VecDeque<(u64, T)>; TaskSource::COUNT],
    next_seq: u64,
}

impl<T> Default for TaskQueueInner<T> {
    fn default() -> Self {
        Self {
            queues: std::array::from_fn(|_| VecDeque::new()),
            next_seq: 0,
        }
    }
}

// https://html.spec.whatwg.org/multipage/webappapis.html#task-queue
pub(crate) struct TaskQueue<T>(Rc<RefCell<TaskQueueInner<T>>>);

impl<T> Clone for TaskQueue<T> {
    fn clone(&self) -> Self {
        Self(Rc::clone(&self.0))
    }
}

impl<T> Default for TaskQueue<T> {
    fn default() -> Self {
        Self(Rc::new(RefCell::new(TaskQueueInner::default())))
    }
}

impl<T> TaskQueue<T> {
    pub(crate) fn push(&self, source: TaskSource, kind: T) {
        let mut inner = self.0.borrow_mut();
        let seq = inner.next_seq;
        inner.next_seq = inner.next_seq.wrapping_add(1);
        inner.queues[source.index()].push_back((seq, kind));
    }

    pub(crate) fn pop_front(&self) -> Option<T> {
        self.pop_front_with_source().map(|(_, kind)| kind)
    }

    pub(crate) fn pop_front_with_source(&self) -> Option<(TaskSource, T)> {
        let mut inner = self.0.borrow_mut();
        let best = TaskSource::ALL
            .into_iter()
            .filter_map(|source| {
                inner.queues[source.index()]
                    .front()
                    .map(|(seq, _)| (source, *seq))
            })
            .min_by_key(|(_, seq)| *seq)
            .map(|(source, _)| source)?;
        inner.queues[best.index()]
            .pop_front()
            .map(|(_, kind)| (best, kind))
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0
            .borrow()
            .queues
            .iter()
            .all(std::collections::VecDeque::is_empty)
    }

    pub(crate) fn append(&self, other: &mut VecDeque<(TaskSource, T)>) {
        while let Some((source, kind)) = other.pop_front() {
            self.push(source, kind);
        }
    }

    pub(crate) fn take_all(&self) -> Vec<T> {
        let mut drained = Vec::new();
        while let Some(task) = self.pop_front() {
            drained.push(task);
        }
        drained
    }
}

// https://html.spec.whatwg.org/multipage/webappapis.html#queue-a-task
#[derive(Clone)]
pub(crate) struct TaskSink(Rc<dyn Fn(TaskSource, RealmTask)>);

impl TaskSink {
    pub(crate) fn noop() -> Self {
        Self(Rc::new(|_, _| {}))
    }

    pub(crate) fn new(f: impl Fn(TaskSource, RealmTask) + 'static) -> Self {
        Self(Rc::new(f))
    }

    pub(crate) fn push(&self, source: TaskSource, task: RealmTask) {
        (self.0)(source, task);
    }
}

// https://html.spec.whatwg.org/multipage/interactive-elements.html#queue-a-details-toggle-event-task
// https://html.spec.whatwg.org/multipage/interactive-elements.html#queue-a-dialog-toggle-event-task
#[derive(Clone, Default)]
pub(crate) struct ToggleCoalescer(Rc<RefCell<Vec<(NodeId, String, String)>>>);

impl ToggleCoalescer {
    pub(crate) fn push(&self, node: NodeId, old_state: String, new_state: String) -> bool {
        let mut toggles = self.0.borrow_mut();
        if let Some(pending) = toggles.iter_mut().find(|(id, _, _)| *id == node) {
            pending.2 = new_state;
            return false;
        }
        toggles.push((node, old_state, new_state));
        true
    }

    pub(crate) fn take(&self, node: NodeId) -> Option<(String, String)> {
        let mut toggles = self.0.borrow_mut();
        let index = toggles.iter().position(|(id, _, _)| *id == node)?;
        let (_, old_state, new_state) = toggles.remove(index);
        Some((old_state, new_state))
    }

    pub(crate) fn clear(&self) {
        self.0.borrow_mut().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::{RealmId, TaskQueue, TaskSource, ToggleCoalescer};
    use crate::dom::tree::{BlitzId, NodeId};

    #[test]
    fn task_queue_preserves_fifo_order_across_task_sources() {
        let queue = TaskQueue::default();
        queue.push(TaskSource::UserInteraction, 1u32);
        queue.push(TaskSource::DomManipulation, 2u32);
        queue.push(TaskSource::Timer, 3u32);
        assert!(!queue.is_empty());

        assert_eq!(
            queue.pop_front_with_source(),
            Some((TaskSource::UserInteraction, 1))
        );
        assert_eq!(
            queue.pop_front_with_source(),
            Some((TaskSource::DomManipulation, 2))
        );
        assert_eq!(queue.pop_front_with_source(), Some((TaskSource::Timer, 3)));
        assert!(queue.is_empty());
        assert_ne!(RealmId(1), RealmId::NONE);
    }

    #[test]
    fn toggle_coalescer_preserves_initial_old_state_and_updates_new_state() {
        let coalescer = ToggleCoalescer::default();
        let node = NodeId {
            document: 1,
            node: BlitzId::from_u64(7),
        };
        assert!(coalescer.push(node, "closed".to_owned(), "open".to_owned()));
        assert!(!coalescer.push(node, "open".to_owned(), "closed".to_owned()));
        assert_eq!(
            coalescer.take(node),
            Some(("closed".to_owned(), "closed".to_owned()))
        );
        assert_eq!(coalescer.take(node), None);
    }
}
