// https://html.spec.whatwg.org/multipage/browsing-the-web.html#session-history-entry
// https://html.spec.whatwg.org/multipage/browsing-the-web.html#joint-session-history

use std::collections::BTreeMap;

use crate::document::LocalDocumentState;
use crate::js::FrameNavigation;
use crate::protocol::{EntryId, FrameId};

#[derive(Clone, Default)]
pub(super) struct FrameHistoryEntry {
    pub(super) navigation: Option<FrameNavigation>,
    pub(super) state: Option<LocalDocumentState>,
    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#url-and-history-update-steps
    pub(super) same_document: Option<SameDocumentEntry>,
}

#[derive(Clone)]
pub(super) struct SameDocumentEntry {
    pub(super) url: String,
    pub(super) state: String,
}

// https://html.spec.whatwg.org/multipage/browsing-the-web.html#joint-session-history
// https://html.spec.whatwg.org/multipage/browsing-the-web.html#traverse-the-history-by-a-delta
#[derive(Default)]
pub(super) struct StandaloneHistory {
    pub(super) steps: Vec<BTreeMap<FrameId, EntryId>>,
    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#getting-the-history-object-length-and-index
    pub(super) index: usize,
}

impl StandaloneHistory {
    pub(super) fn seed() -> Self {
        Self {
            steps: vec![BTreeMap::new()],
            index: 0,
        }
    }

    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#navigate
    pub(super) fn join(&mut self, frame: FrameId, entry: EntryId) {
        self.steps[self.index].insert(frame, entry);
    }

    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#clear-the-forward-session-history
    pub(super) fn push(&mut self, frame: FrameId, entry: EntryId) -> Vec<EntryId> {
        let mut evicted = Vec::new();
        for dropped in self.steps.drain(self.index.saturating_add(1)..) {
            evicted.extend(dropped.into_values());
        }
        let mut step = self.steps[self.index].clone();
        step.insert(frame, entry);
        self.steps.push(step);
        self.index = self.steps.len().saturating_sub(1);
        self.prune_unused(evicted)
    }

    pub(super) fn replace(&mut self, frame: FrameId, entry: EntryId) -> Vec<EntryId> {
        let previous = self.steps[self.index].insert(frame, entry);
        self.prune_unused(previous.into_iter().collect())
    }

    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#getting-the-used-step
    pub(super) fn leave(&mut self, frame: FrameId) -> Vec<EntryId> {
        let mut removed = Vec::new();
        for step in &mut self.steps {
            if let Some(entry) = step.remove(&frame) {
                removed.push(entry);
            }
        }
        self.prune_unused(removed)
    }

    pub(super) fn target(&self, delta: i32) -> Option<usize> {
        self.index
            .checked_add_signed(delta as isize)
            .filter(|target| *target < self.steps.len())
    }

    pub(super) fn entry_at(&self, step: usize, frame: FrameId) -> Option<EntryId> {
        self.steps.get(step)?.get(&frame).copied()
    }

    fn prune_unused(&self, candidates: Vec<EntryId>) -> Vec<EntryId> {
        candidates
            .into_iter()
            .filter(|id| {
                !self
                    .steps
                    .iter()
                    .any(|step| step.values().any(|live| live == id))
            })
            .collect()
    }
}
