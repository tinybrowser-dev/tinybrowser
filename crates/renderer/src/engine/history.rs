// https://html.spec.whatwg.org/multipage/browsing-the-web.html#session-history-entry
// https://html.spec.whatwg.org/multipage/browsing-the-web.html#joint-session-history

use std::collections::BTreeMap;

use crate::document::LocalDocumentState;
use crate::js::FrameNavigation;
use crate::protocol::FrameId;

// https://html.spec.whatwg.org/multipage/nav-history-apis.html#shared-history-push/replace-state-steps — UA-defined renderer history limits.
const MAX_HISTORY_ENTRIES: usize = 128;
const MAX_HISTORY_TOTAL_BYTES: usize = 8 * 1024 * 1024;

#[derive(Default)]
pub(super) struct FrameHistory {
    pub(super) entries: Vec<FrameHistoryEntry>,
    pub(super) index: usize,
    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#finalize-a-cross-document-navigation
    pub(super) pending: Option<PendingEntry>,
}

pub(super) struct PendingEntry {
    entry: FrameHistoryEntry,
    replace: Option<usize>,
}

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

impl FrameHistory {
    pub(super) fn push(&mut self, entry: FrameHistoryEntry) {
        self.entries.truncate(self.index.saturating_add(1));
        self.entries.push(entry);
        self.index = self.entries.len().saturating_sub(1);
        self.evict_excess();
    }

    fn evict_excess(&mut self) {
        while self.entries.len() > MAX_HISTORY_ENTRIES
            || (self.entries.len() > 1 && self.total_bytes() > MAX_HISTORY_TOTAL_BYTES)
        {
            self.entries.remove(0);
            self.index = self.index.saturating_sub(1);
        }
    }

    fn total_bytes(&self) -> usize {
        self.entries
            .iter()
            .filter_map(|entry| entry.same_document.as_ref())
            .map(|same| same.url.len() + same.state.len())
            .sum()
    }

    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#navigate
    pub(super) fn replace(&mut self, entry: FrameHistoryEntry) {
        if let Some(slot) = self.entries.get_mut(self.index) {
            *slot = entry;
        } else {
            self.push(entry);
        }
    }

    pub(super) fn stage(&mut self, entry: FrameHistoryEntry, replace: bool) {
        self.pending = Some(PendingEntry {
            entry,
            replace: replace.then_some(self.index),
        });
    }

    pub(super) fn commit_pending(&mut self) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        match pending.replace {
            Some(index) => {
                if let Some(slot) = self.entries.get_mut(index) {
                    *slot = pending.entry;
                } else {
                    self.push(pending.entry);
                }
            }
            None => self.push(pending.entry),
        }
    }

    pub(super) fn discard_pending(&mut self) {
        self.pending = None;
    }

    pub(super) fn push_same_document(&mut self, entry: SameDocumentEntry) {
        self.push(FrameHistoryEntry {
            same_document: Some(entry),
            ..FrameHistoryEntry::default()
        });
    }

    pub(super) fn replace_same_document(&mut self, entry: SameDocumentEntry) {
        if let Some(slot) = self.entries.get_mut(self.index) {
            slot.same_document = Some(entry);
        }
    }

    pub(super) fn at(&self, index: usize) -> Option<&FrameHistoryEntry> {
        self.entries.get(index)
    }
}

// https://html.spec.whatwg.org/multipage/browsing-the-web.html#joint-session-history
// https://html.spec.whatwg.org/multipage/browsing-the-web.html#traverse-the-history-by-a-delta
#[derive(Default)]
pub(super) struct StandaloneHistory {
    pub(super) steps: Vec<BTreeMap<FrameId, usize>>,
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
    pub(super) fn join(&mut self, frame: FrameId, local_index: usize) {
        self.steps[self.index].insert(frame, local_index);
    }

    pub(super) fn active(&self) -> &BTreeMap<FrameId, usize> {
        &self.steps[self.index]
    }

    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#clear-the-forward-session-history
    pub(super) fn push(&mut self, frame: FrameId, local_index: usize) {
        self.steps.truncate(self.index.saturating_add(1));
        let mut step = self.steps[self.index].clone();
        step.insert(frame, local_index);
        self.steps.push(step);
        if self.steps.len() > MAX_HISTORY_ENTRIES {
            let excess = self.steps.len() - MAX_HISTORY_ENTRIES;
            self.steps.drain(..excess);
        }
        self.index = self.steps.len().saturating_sub(1);
    }

    pub(super) fn replace(&mut self, frame: FrameId, local_index: usize) {
        self.steps[self.index].insert(frame, local_index);
    }

    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#getting-the-used-step
    pub(super) fn leave(&mut self, frame: FrameId) {
        for step in &mut self.steps {
            step.remove(&frame);
        }
    }

    pub(super) fn target(&self, delta: i32) -> Option<usize> {
        self.index
            .checked_add_signed(delta as isize)
            .filter(|target| *target < self.steps.len())
    }

    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#traverse-the-history-by-a-delta
    pub(super) fn local_at(&self, step: usize, frame: FrameId) -> usize {
        self.steps[step].get(&frame).copied().unwrap_or(0)
    }
}
