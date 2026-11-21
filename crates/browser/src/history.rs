// https://html.spec.whatwg.org/multipage/history.html#joint-session-history
// https://html.spec.whatwg.org/multipage/browsing-the-web.html#session-history-entry

use std::collections::HashMap;

use renderer::HistorySnapshot as RendererHistorySnapshot;
use renderer::{FrameId, HistoryHandling};
use url::Url;

// https://html.spec.whatwg.org/multipage/nav-history-apis.html#shared-history-push/replace-state-steps — UA-defined session history limits (128 entries, 640 KiB state, 64 KiB URL, 4 KiB title, 8 MiB total).
const MAX_HISTORY_ENTRIES: usize = 128;
const MAX_HISTORY_STATE_BYTES: usize = 640 * 1024;
const MAX_HISTORY_URL_BYTES: usize = 64 * 1024;
const MAX_HISTORY_TITLE_BYTES: usize = 4 * 1024;
const MAX_HISTORY_TOTAL_BYTES: usize = 8 * 1024 * 1024;

fn truncate_utf8(mut text: String, max_bytes: usize) -> String {
    if text.len() > max_bytes {
        let mut end = max_bytes;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}

// https://html.spec.whatwg.org/multipage/browsing-the-web.html#session-history
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EntryId(u64);

impl EntryId {
    #[must_use]
    pub fn new(raw: u64) -> Self {
        Self(raw)
    }

    #[must_use]
    pub fn get(self) -> u64 {
        self.0
    }
}

// https://html.spec.whatwg.org/multipage/browsing-the-web.html#traverse-the-history
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HistoryStep {
    // https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-history-go
    Delta(i32),
    // https://chromedevtools.github.io/devtools-protocol/tot/Page/#method-navigateToHistoryEntry
    Entry(EntryId),
}

pub(crate) enum Commit {
    Push {
        url: Url,
        // https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName
        name: Option<renderer::ContextName>,
    },
    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName
    Traverse {
        entry: EntryId,
        url: Url,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryEntry {
    id: EntryId,
    url: Url,
    title: String,
}

impl HistoryEntry {
    #[must_use]
    pub fn id(&self) -> EntryId {
        self.id
    }

    #[must_use]
    pub fn url(&self) -> &Url {
        &self.url
    }

    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistorySnapshot {
    entries: Vec<HistoryEntry>,
    current_index: usize,
}

impl HistorySnapshot {
    #[must_use]
    pub fn entries(&self) -> &[HistoryEntry] {
        &self.entries
    }

    #[must_use]
    pub fn current_index(&self) -> usize {
        self.current_index
    }
}

#[derive(Clone)]
struct Entry {
    id: EntryId,
    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#getting-all-used-history-steps
    step: u64,
    local_index: usize,
    url: Url,
    title: String,
    state: Option<String>,
    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#document-state
    document: u64,
}

// https://html.spec.whatwg.org/multipage/browsing-the-web.html#finalize-a-cross-document-navigation
#[derive(Clone)]
enum PendingChange {
    Push {
        name: Option<renderer::ContextName>,
    },
    Replace {
        entry: EntryId,
        name: Option<renderer::ContextName>,
    },
}

#[derive(Clone)]
pub(crate) struct SessionHistory {
    entries: Vec<Entry>,
    frames: HashMap<FrameId, Vec<EntryId>>,
    pending: HashMap<FrameId, PendingChange>,
    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#tn-current-session-history-step
    current_step: u64,
    next_document: u64,
    next_entry: u64,
    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#document-state
    // https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-name
    names: HashMap<u64, renderer::ContextName>,
}

impl SessionHistory {
    pub(crate) fn new(blank: Url, name: Option<renderer::ContextName>) -> Self {
        let mut history = Self {
            entries: Vec::new(),
            frames: HashMap::new(),
            pending: HashMap::new(),
            current_step: 0,
            next_document: 1,
            next_entry: 1,
            names: name.map_or_default(|name| HashMap::from([(0, name)])),
        };
        history.entries.push(Entry {
            id: EntryId(0),
            step: 0,
            local_index: 0,
            url: blank,
            title: String::new(),
            state: None,
            document: 0,
        });
        history.frames.insert(FrameId::MAIN, vec![EntryId(0)]);
        history
    }

    // https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-history-length
    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#getting-the-history-object-length-and-index
    pub(crate) fn renderer_snapshot_for(&self, frame: FrameId) -> Option<RendererHistorySnapshot> {
        self.frames.get(&frame)?;
        Some(self.build_snapshot(frame, self.active_entry(frame), None))
    }

    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#getting-the-history-object-length-and-index
    pub(crate) fn renderer_snapshot_for_entry(
        &self,
        entry: EntryId,
    ) -> Option<RendererHistorySnapshot> {
        let found = self.locate(entry)?;
        let frame = self.frame_of(entry)?;
        let steps = self.used_steps();
        let index = steps.iter().position(|step| *step == found.step)?;
        Some(self.build_snapshot(frame, Some(entry), Some((steps.len(), index))))
    }

    pub(crate) fn renderer_snapshot(&self) -> RendererHistorySnapshot {
        self.build_snapshot(FrameId::MAIN, self.active_entry(FrameId::MAIN), None)
    }

    fn build_snapshot(
        &self,
        frame: FrameId,
        entry: Option<EntryId>,
        joint: Option<(usize, usize)>,
    ) -> RendererHistorySnapshot {
        let (length, index) = joint.unwrap_or_else(|| {
            let steps = self.used_steps();
            (steps.len(), self.current_step_index(&steps))
        });
        let located = entry.and_then(|id| self.locate(id));
        let local_index = entry
            .and_then(|id| self.frame_position(frame, id))
            .or_else(|| located.map(|found| found.local_index))
            .unwrap_or(0);
        RendererHistorySnapshot {
            length,
            index,
            local_length: self.frame_len(frame),
            local_index,
            state: located.and_then(|found| found.state.clone()),
        }
    }

    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#getting-all-used-history-steps
    fn used_steps(&self) -> Vec<u64> {
        let mut steps: Vec<u64> = self.entries.iter().map(|entry| entry.step).collect();
        steps.sort_unstable();
        steps.dedup();
        steps
    }

    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#getting-the-used-step
    fn current_step_index(&self, steps: &[u64]) -> usize {
        steps
            .iter()
            .rposition(|step| *step <= self.current_step)
            .unwrap_or(0)
    }

    pub(crate) fn frames(&self) -> Vec<FrameId> {
        self.frames.keys().copied().collect()
    }

    pub(crate) fn snapshot(&self) -> HistorySnapshot {
        let ids = self.frames.get(&FrameId::MAIN).cloned().unwrap_or_default();
        let entries: Vec<HistoryEntry> = ids
            .iter()
            .filter_map(|id| self.locate(*id))
            .map(|entry| HistoryEntry {
                id: entry.id,
                url: entry.url.clone(),
                title: entry.title.clone(),
            })
            .collect();
        let current_index = self
            .active_entry(FrameId::MAIN)
            .and_then(|id| self.locate(id))
            .map_or(0, |entry| entry.local_index);
        HistorySnapshot {
            entries,
            current_index,
        }
    }

    pub(crate) fn set_current_title(&mut self, title: String) {
        self.set_title(FrameId::MAIN, title);
    }

    pub(crate) fn set_title(&mut self, frame: FrameId, title: String) {
        let Some(id) = self.active_entry(frame) else {
            return;
        };
        let title = truncate_utf8(title, MAX_HISTORY_TITLE_BYTES);
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == id) {
            entry.title = title;
        }
    }

    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#navigate
    pub(crate) fn add_frame(
        &mut self,
        frame: FrameId,
        url: Url,
        name: Option<renderer::ContextName>,
    ) {
        if frame == FrameId::MAIN || self.frames.contains_key(&frame) {
            return;
        }
        let document = self.take_document();
        if let Some(name) = name {
            self.names.insert(document, name);
        }
        let id = EntryId(self.next_entry);
        self.next_entry = self.next_entry.saturating_add(1);
        self.entries.push(Entry {
            id,
            step: self.current_step,
            local_index: 0,
            url,
            title: String::new(),
            state: None,
            document,
        });
        self.frames.insert(frame, vec![id]);
    }

    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#finalize-a-cross-document-navigation
    pub(crate) fn begin(
        &mut self,
        frame: FrameId,
        url: Url,
        replace: bool,
        local_index: usize,
        name: Option<renderer::ContextName>,
        pending: bool,
    ) -> Option<EntryId> {
        if frame == FrameId::MAIN {
            return None;
        }
        if !self.frames.contains_key(&frame) {
            self.add_frame(frame, url.clone(), None);
        }
        let current = self.active_entry(frame);
        if pending {
            let change = if replace {
                let entry = match current {
                    Some(_) if local_index < self.frame_len(frame) => {
                        self.frame_id_at(frame, local_index)?
                    }
                    Some(current) => current,
                    None => return None,
                };
                PendingChange::Replace { entry, name }
            } else {
                PendingChange::Push { name }
            };
            self.pending.insert(frame, change);
            return current;
        }
        self.pending.remove(&frame);
        if replace && let Some(current) = current {
            let target = if local_index < self.frame_len(frame) {
                self.frame_id_at(frame, local_index)?
            } else {
                current
            };
            self.replace_entry(target, url, name);
            return Some(target);
        }
        self.truncate_forward();
        let step = self.current_step.saturating_add(1);
        let id = self.insert_entry(frame, step, local_index, url, name, None);
        self.current_step = step;
        Some(id)
    }

    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#finalize-a-cross-document-navigation
    pub(crate) fn commit_url(&mut self, frame: FrameId, url: Url) {
        if let Some(change) = self.pending.remove(&frame) {
            match change {
                PendingChange::Push { name, .. } => {
                    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#finalize-a-cross-document-navigation
                    self.truncate_forward();
                    let step = self.current_step.saturating_add(1);
                    let local_index = self.frame_len(frame);
                    self.insert_entry(frame, step, local_index, url, name, None);
                    self.current_step = step;
                }
                PendingChange::Replace { entry, name, .. } => {
                    self.replace_entry(entry, url, name);
                }
            }
            return;
        }
        let Some(id) = self.active_entry(frame) else {
            return;
        };
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == id) {
            entry.url = url;
        }
    }

    pub(crate) fn discard_pending(&mut self, frame: FrameId) {
        self.pending.remove(&frame);
    }

    // https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-name
    pub(crate) fn sync_current_name(&mut self, name: Option<&renderer::ContextName>) {
        let Some(id) = self.active_entry(FrameId::MAIN) else {
            return;
        };
        let Some(document) = self.locate(id).map(|entry| entry.document) else {
            return;
        };
        match name {
            Some(name) => {
                self.names.insert(document, name.clone());
            }
            None => {
                self.names.remove(&document);
            }
        }
    }

    pub(crate) fn name(&self, entry: EntryId) -> Option<renderer::ContextName> {
        self.locate(entry)
            .and_then(|entry| self.names.get(&entry.document).cloned())
    }

    pub(crate) fn update(
        &mut self,
        frame: FrameId,
        url: Url,
        state: String,
        handling: HistoryHandling,
    ) {
        if state.len() > MAX_HISTORY_STATE_BYTES || url.as_str().len() > MAX_HISTORY_URL_BYTES {
            return;
        }
        let Some(active) = self.active_entry(frame) else {
            return;
        };
        match handling {
            HistoryHandling::Replace => {
                if let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == active) {
                    entry.url = url;
                    entry.state = Some(state);
                }
                self.evict_excess_entries(active);
            }
            HistoryHandling::Push => {
                let Some(document) = self.locate(active).map(|entry| entry.document) else {
                    return;
                };
                let Some(title) = self.locate(active).map(|entry| entry.title.clone()) else {
                    return;
                };
                self.truncate_forward();
                let step = self.current_step.saturating_add(1);
                let local_index = self.frame_len(frame);
                let id = self.insert_entry(frame, step, local_index, url, None, Some(state));
                if let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == id) {
                    entry.document = document;
                    entry.title = title;
                }
                self.current_step = step;
            }
        }
    }

    pub(crate) fn commit(&mut self, commit: Commit) -> bool {
        match commit {
            Commit::Traverse { entry, url } => {
                let Some(index) = self.locate_index(entry) else {
                    return false;
                };
                // https://html.spec.whatwg.org/multipage/browsing-the-web.html#populating-a-session-history-entry
                let document = self.entries[index].document;
                let step = self.entries[index].step;
                let next = self.take_document();
                let name = self.names.get(&document).cloned();
                self.entries[index].url = url;
                self.entries[index].document = next;
                if let Some(name) = name {
                    self.names.insert(next, name);
                }
                if !self
                    .entries
                    .iter()
                    .any(|candidate| candidate.document == document)
                {
                    self.names.remove(&document);
                }
                self.current_step = step;
                true
            }
            Commit::Push { url, name } => {
                self.truncate_forward();
                let step = self.current_step.saturating_add(1);
                let local_index = self.frame_len(FrameId::MAIN);
                self.insert_entry(FrameId::MAIN, step, local_index, url, name, None);
                self.current_step = step;
                true
            }
        }
    }

    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#traverse-the-history-by-a-delta
    pub(crate) fn target_step(&self, step: HistoryStep, _caller: FrameId) -> Option<u64> {
        match step {
            HistoryStep::Entry(entry) => self.locate(entry).map(|entry| entry.step),
            HistoryStep::Delta(delta) => {
                let steps = self.used_steps();
                let current = self.current_step_index(&steps);
                let target = current.checked_add_signed(delta as isize)?;
                steps.get(target).copied()
            }
        }
    }

    pub(crate) fn active_entry_at(&self, frame: FrameId, at_step: u64) -> Option<EntryId> {
        let ids = self.frames.get(&frame)?;
        ids.iter()
            .rev()
            .find(|id| self.locate(**id).is_some_and(|entry| entry.step <= at_step))
            .or_else(|| ids.first())
            .copied()
    }

    pub(crate) fn active_entry(&self, frame: FrameId) -> Option<EntryId> {
        self.active_entry_at(frame, self.current_step)
    }

    pub(crate) fn current_step(&self) -> u64 {
        self.current_step
    }

    pub(crate) fn set_current_step(&mut self, step: u64) {
        self.current_step = step;
    }

    pub(crate) fn current_entry(&self) -> EntryId {
        self.active_entry(FrameId::MAIN).unwrap_or(EntryId(0))
    }

    pub(crate) fn entry_url(&self, entry: EntryId) -> Option<&Url> {
        self.locate(entry).map(|entry| &entry.url)
    }

    pub(crate) fn local_index(&self, entry: EntryId) -> Option<usize> {
        self.locate(entry).map(|entry| entry.local_index)
    }

    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#document-state
    pub(crate) fn same_document(&self, left: EntryId, right: EntryId) -> bool {
        match (self.locate(left), self.locate(right)) {
            (Some(left), Some(right)) => left.document == right.document,
            _ => false,
        }
    }

    pub(crate) fn traverse_same_document(&mut self, entry: EntryId) -> bool {
        let Some(target) = self.locate(entry) else {
            return false;
        };
        self.current_step = target.step;
        true
    }

    // https://html.spec.whatwg.org/multipage/history.html#joint-session-history
    pub(crate) fn remove_frame(&mut self, frame: FrameId) {
        if frame == FrameId::MAIN {
            return;
        }
        let Some(ids) = self.frames.remove(&frame) else {
            return;
        };
        self.pending.remove(&frame);
        let documents: Vec<u64> = ids
            .iter()
            .filter_map(|id| self.locate(*id).map(|entry| entry.document))
            .collect();
        self.entries.retain(|entry| !ids.contains(&entry.id));
        for document in documents {
            if !self.entries.iter().any(|entry| entry.document == document) {
                self.names.remove(&document);
            }
        }
        self.clamp_current_step();
    }

    pub(crate) fn clear_children(&mut self) {
        let children: Vec<FrameId> = self
            .frames
            .keys()
            .copied()
            .filter(|frame| *frame != FrameId::MAIN)
            .collect();
        for frame in children {
            self.remove_frame(frame);
        }
    }

    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#clear-the-forward-session-history
    fn truncate_forward(&mut self) {
        let removed: Vec<u64> = self
            .entries
            .iter()
            .filter(|entry| entry.step > self.current_step)
            .map(|entry| entry.document)
            .collect();
        self.entries.retain(|entry| entry.step <= self.current_step);
        let live: Vec<EntryId> = self.entries.iter().map(|entry| entry.id).collect();
        for ids in self.frames.values_mut() {
            ids.retain(|id| live.contains(id));
        }
        for document in removed {
            if !self.entries.iter().any(|entry| entry.document == document) {
                self.names.remove(&document);
            }
        }
    }

    fn clamp_current_step(&mut self) {
        if let Some(step) = self
            .entries
            .iter()
            .filter(|entry| entry.step <= self.current_step)
            .map(|entry| entry.step)
            .max()
        {
            self.current_step = step;
        } else {
            self.current_step = 0;
        }
    }

    fn frame_len(&self, frame: FrameId) -> usize {
        self.frames.get(&frame).map_or(0, Vec::len)
    }

    fn frame_id_at(&self, frame: FrameId, local_index: usize) -> Option<EntryId> {
        self.frames.get(&frame)?.get(local_index).copied()
    }

    fn frame_of(&self, entry: EntryId) -> Option<FrameId> {
        self.frames
            .iter()
            .find(|(_, ids)| ids.contains(&entry))
            .map(|(frame, _)| *frame)
    }

    fn frame_position(&self, frame: FrameId, entry: EntryId) -> Option<usize> {
        self.frames
            .get(&frame)?
            .iter()
            .position(|candidate| *candidate == entry)
    }

    fn take_document(&mut self) -> u64 {
        let document = self.next_document;
        self.next_document = self.next_document.saturating_add(1);
        document
    }

    fn insert_entry(
        &mut self,
        frame: FrameId,
        step: u64,
        local_index: usize,
        url: Url,
        name: Option<renderer::ContextName>,
        state: Option<String>,
    ) -> EntryId {
        let document = self.take_document();
        if let Some(name) = name {
            self.names.insert(document, name);
        }
        let id = EntryId(self.next_entry);
        self.next_entry = self.next_entry.saturating_add(1);
        self.entries.push(Entry {
            id,
            step,
            local_index,
            url,
            title: String::new(),
            state,
            document,
        });
        let ids = self.frames.entry(frame).or_default();
        if local_index < ids.len() {
            ids.insert(local_index, id);
        } else {
            ids.push(id);
        }
        self.evict_excess_entries(id);
        id
    }

    fn total_bytes(&self) -> usize {
        self.entries
            .iter()
            .map(|entry| {
                entry.url.as_str().len()
                    + entry.title.len()
                    + entry.state.as_ref().map_or(0, String::len)
            })
            .sum()
    }

    fn evict_excess_entries(&mut self, keep: EntryId) {
        while self.entries.len() > MAX_HISTORY_ENTRIES
            || (self.entries.len() > 1 && self.total_bytes() > MAX_HISTORY_TOTAL_BYTES)
        {
            let Some(evict_index) = self.entries.iter().position(|entry| entry.id != keep) else {
                break;
            };
            let evicted = self.entries.remove(evict_index);
            if let Some(evicted_frame) = self.frame_of(evicted.id)
                && let Some(ids) = self.frames.get_mut(&evicted_frame)
            {
                ids.retain(|candidate| *candidate != evicted.id);
                for (index, entry_id) in ids.iter().enumerate() {
                    if let Some(entry) = self.entries.iter_mut().find(|e| e.id == *entry_id) {
                        entry.local_index = index;
                    }
                }
            }
            if !self
                .entries
                .iter()
                .any(|entry| entry.document == evicted.document)
            {
                self.names.remove(&evicted.document);
            }
        }
    }

    fn replace_entry(&mut self, id: EntryId, url: Url, name: Option<renderer::ContextName>) {
        let Some(index) = self.locate_index(id) else {
            return;
        };
        let old_document = self.entries[index].document;
        let document = self.take_document();
        self.entries[index].url = url;
        self.entries[index].title = String::new();
        self.entries[index].state = None;
        self.entries[index].document = document;
        if let Some(name) = name {
            self.names.insert(document, name);
        }
        if !self
            .entries
            .iter()
            .any(|entry| entry.document == old_document)
        {
            self.names.remove(&old_document);
        }
    }

    fn locate(&self, entry: EntryId) -> Option<&Entry> {
        self.entries.iter().find(|candidate| candidate.id == entry)
    }

    fn locate_index(&self, entry: EntryId) -> Option<usize> {
        self.entries
            .iter()
            .position(|candidate| candidate.id == entry)
    }
}

#[cfg(test)]
mod tests {
    use super::{Commit, HistoryStep, SessionHistory};
    use renderer::FrameId;
    use url::Url;

    fn url(text: &str) -> Url {
        Url::parse(text).expect("valid url")
    }

    fn push(text: &str, name: Option<renderer::ContextName>) -> Commit {
        Commit::Push {
            url: url(text),
            name,
        }
    }

    #[test]
    fn back_then_navigate_truncates_the_forward_branch() {
        let mut history = SessionHistory::new(url("about:blank"), None);
        history.commit(push("https://example.com/a", None));
        history.commit(push("https://example.com/b", None));

        let target = history
            .target_step(HistoryStep::Delta(-1), FrameId::MAIN)
            .expect("back target");
        let entry = history
            .active_entry_at(FrameId::MAIN, target)
            .expect("entry");
        assert!(history.traverse_same_document(entry));

        history.commit(push("https://example.com/c", None));

        let snapshot = history.snapshot();
        let urls: Vec<&str> = snapshot
            .entries()
            .iter()
            .map(|entry| entry.url().as_str())
            .collect();
        assert_eq!(
            urls,
            [
                "about:blank",
                "https://example.com/a",
                "https://example.com/c"
            ]
        );
        assert_eq!(snapshot.current_index(), 2);
        assert_eq!(snapshot.entries()[1].id(), entry);
    }

    #[test]
    fn titles_are_recorded_per_entry() {
        let mut history = SessionHistory::new(url("about:blank"), None);
        history.commit(push("https://example.com/a", None));
        history.set_current_title("Alpha".into());
        history.commit(push("https://example.com/b", None));
        history.set_current_title("Beta".into());

        let snapshot = history.snapshot();
        assert_eq!(snapshot.entries()[1].title(), "Alpha");
        assert_eq!(snapshot.entries()[2].title(), "Beta");
    }

    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName
    // https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-name
    #[test]
    fn a_target_name_rides_the_entry_it_committed_with() {
        let named = |raw: &str| renderer::ContextName::parse(raw);
        let mut history = SessionHistory::new(url("about:blank"), named("start"));
        history.commit(push("https://example.com/a", named("first")));
        let first = history.active_entry_at(FrameId::MAIN, 1).expect("entry");
        assert_eq!(history.name(first), named("first"));

        history.sync_current_name(named("renamed").as_ref());
        assert_eq!(history.name(first), named("renamed"));

        history.update(
            FrameId::MAIN,
            url("https://example.com/hash"),
            "{}".into(),
            renderer::HistoryHandling::Push,
        );
        let pushed = history.current_entry();
        assert_eq!(history.name(pushed), named("renamed"));
        assert_eq!(history.name(first), named("renamed"));

        history.commit(push("https://example.com/b", None));
        let current = history.current_entry();
        assert_eq!(history.name(current), None);
        assert_eq!(history.name(first), named("renamed"));

        assert!(history.commit(Commit::Traverse {
            entry: first,
            url: url("https://example.com/a"),
        }));
        assert_eq!(history.name(first), named("renamed"));
        assert_eq!(history.name(pushed), named("renamed"));

        history.sync_current_name(None);
        assert_eq!(history.name(first), None);
        assert_eq!(history.name(pushed), named("renamed"));
        assert_eq!(history.name(current), None);
    }

    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#populating-a-session-history-entry
    #[test]
    fn a_traversal_repopulates_only_its_own_entry() {
        let mut history = SessionHistory::new(url("about:blank"), None);
        history.commit(push("https://example.com/a", None));
        history.update(
            FrameId::MAIN,
            url("https://example.com/a#hash"),
            "{}".into(),
            renderer::HistoryHandling::Push,
        );
        let pushed = history.current_entry();
        history.commit(push("https://example.com/b", None));

        let target = history
            .target_step(HistoryStep::Delta(-1), FrameId::MAIN)
            .expect("back target");
        let entry = history
            .active_entry_at(FrameId::MAIN, target)
            .expect("entry");
        assert_eq!(entry, pushed);
        assert!(!history.same_document(entry, history.current_entry()));
        assert!(history.commit(Commit::Traverse {
            entry,
            url: url("https://example.com/a#hash"),
        }));

        let target = history
            .target_step(HistoryStep::Delta(-1), FrameId::MAIN)
            .expect("sibling target");
        let sibling = history
            .active_entry_at(FrameId::MAIN, target)
            .expect("sibling");
        assert!(!history.same_document(sibling, entry));
        assert!(history.commit(Commit::Traverse {
            entry: sibling,
            url: url("https://example.com/a"),
        }));
    }

    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#getting-the-history-object-length-and-index
    #[test]
    fn a_child_length_is_the_joint_step_count() {
        let child = FrameId::new(1);
        let mut history = SessionHistory::new(url("about:blank"), None);
        history.add_frame(child, url("about:blank"), None);
        assert_eq!(history.renderer_snapshot_for(child).unwrap().length, 1);

        history
            .begin(
                child,
                url("https://example.com/child"),
                true,
                0,
                None,
                false,
            )
            .expect("child entry");
        assert_eq!(history.renderer_snapshot_for(child).unwrap().length, 1);
        assert_eq!(
            history.renderer_snapshot_for(FrameId::MAIN).unwrap().length,
            1,
            "the parent sees the child's step too"
        );

        history.commit(push("https://example.com/main", None));
        assert_eq!(
            history.renderer_snapshot_for(child).unwrap().length,
            2,
            "a main-frame push makes the joint list longer for the child too"
        );
        assert_eq!(
            history.renderer_snapshot_for(FrameId::MAIN).unwrap().length,
            2
        );
    }

    #[test]
    fn a_child_push_adds_one_joint_step() {
        let child = FrameId::new(1);
        let mut history = SessionHistory::new(url("about:blank"), None);
        history.add_frame(child, url("about:blank"), None);
        history.begin(child, url("https://example.com/one"), true, 0, None, false);
        history.begin(child, url("https://example.com/two"), false, 1, None, false);
        assert_eq!(
            history.renderer_snapshot_for(FrameId::MAIN).unwrap().length,
            2
        );
        let back = history
            .target_step(HistoryStep::Delta(-1), FrameId::MAIN)
            .expect("back");
        assert_eq!(back, 0);
        let child_entry = history.active_entry_at(child, back).expect("child entry");
        assert_eq!(
            history.entry_url(child_entry).map(Url::as_str),
            Some("https://example.com/one")
        );
    }

    #[test]
    fn removing_a_frame_drops_its_steps_from_the_joint_length() {
        let child = FrameId::new(1);
        let mut history = SessionHistory::new(url("about:blank"), None);
        history.add_frame(child, url("about:blank"), None);
        history.begin(child, url("https://example.com/one"), false, 1, None, false);
        assert_eq!(
            history.renderer_snapshot_for(FrameId::MAIN).unwrap().length,
            2
        );
        history.remove_frame(child);
        assert_eq!(
            history.renderer_snapshot_for(FrameId::MAIN).unwrap().length,
            1
        );
    }

    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#finalize-a-cross-document-navigation
    #[test]
    fn a_pending_navigation_takes_its_step_at_commit() {
        let child = FrameId::new(1);
        let mut history = SessionHistory::new(url("about:blank"), None);
        history.add_frame(child, url("about:blank"), None);
        assert_eq!(
            history.renderer_snapshot_for(FrameId::MAIN).unwrap().length,
            1
        );

        history.begin(
            child,
            url("https://example.com/start"),
            false,
            1,
            None,
            true,
        );
        assert_eq!(
            history.renderer_snapshot_for(FrameId::MAIN).unwrap().length,
            1,
            "a pending load is not a step"
        );

        history.commit_url(child, url("https://example.com/final"));
        assert_eq!(
            history.renderer_snapshot_for(FrameId::MAIN).unwrap().length,
            2
        );
        let active = history.active_entry(child).expect("child entry");
        assert_eq!(
            history.entry_url(active).map(Url::as_str),
            Some("https://example.com/final"),
            "the commit carries the redirect's final URL"
        );
    }

    #[test]
    fn an_uncommitted_replace_changes_nothing() {
        let child = FrameId::new(1);
        let mut history = SessionHistory::new(url("about:blank"), None);
        history.add_frame(child, url("about:blank"), None);
        history.begin(child, url("https://example.com/never"), true, 0, None, true);
        let active = history.active_entry(child).expect("child entry");
        assert_eq!(
            history.entry_url(active).map(Url::as_str),
            Some("about:blank")
        );
        assert_eq!(
            history.renderer_snapshot_for(FrameId::MAIN).unwrap().length,
            1
        );
    }

    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#clear-the-forward-session-history
    #[test]
    fn a_frame_with_no_entries_left_can_navigate_again() {
        let child = FrameId::new(1);
        let mut history = SessionHistory::new(url("about:blank"), None);
        history.commit(push("https://example.com/main", None));
        history.add_frame(child, url("about:blank"), None);
        history.begin(
            child,
            url("https://example.com/child"),
            false,
            1,
            None,
            false,
        );

        history.set_current_step(0);
        history.commit(push("https://example.com/replaced", None));
        assert_eq!(
            history.renderer_snapshot_for(child).unwrap().length,
            2,
            "the child still mirrors the joint steps"
        );

        history.begin(
            child,
            url("https://example.com/again"),
            false,
            0,
            None,
            false,
        );
        assert_eq!(
            history.renderer_snapshot_for(child).unwrap().length,
            3,
            "the next child navigation is recorded"
        );
    }

    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#url-and-history-update-steps
    #[test]
    fn a_push_after_back_shortens_the_synchronous_length() {
        let mut history = SessionHistory::new(url("about:blank"), None);
        let mut snapshot = history.renderer_snapshot();
        assert_eq!((snapshot.length, snapshot.index), (1, 0));

        history.commit(push("https://example.com/a", None));
        history.commit(push("https://example.com/b", None));
        snapshot = history.renderer_snapshot();
        assert_eq!((snapshot.length, snapshot.index), (3, 2), "blank, a, b");

        let target = history
            .target_step(HistoryStep::Delta(-1), FrameId::MAIN)
            .expect("back");
        let entry = history
            .active_entry_at(FrameId::MAIN, target)
            .expect("entry");
        snapshot = history
            .renderer_snapshot_for_entry(entry)
            .expect("entry snapshot");
        assert_eq!((snapshot.length, snapshot.index), (3, 1));

        assert!(history.traverse_same_document(entry));
        history.commit(push("https://example.com/c", None));
        snapshot = history.renderer_snapshot();
        assert_eq!((snapshot.length, snapshot.index), (3, 2));
    }

    #[test]
    fn push_state_caps_entry_count_and_state_size() {
        let mut history = SessionHistory::new(url("https://example.com/0"), None);
        let oversized = "x".repeat(super::MAX_HISTORY_STATE_BYTES + 1);
        history.update(
            FrameId::MAIN,
            url("https://example.com/huge"),
            oversized,
            super::HistoryHandling::Push,
        );
        assert_eq!(history.entries.len(), 1);
        assert_eq!(
            history.entry_url(history.current_entry()).map(Url::as_str),
            Some("https://example.com/0")
        );

        for i in 1..=(super::MAX_HISTORY_ENTRIES + 50) {
            history.update(
                FrameId::MAIN,
                url(&format!("https://example.com/{i}")),
                format!("state-{i}"),
                super::HistoryHandling::Push,
            );
        }
        assert_eq!(history.entries.len(), super::MAX_HISTORY_ENTRIES);
        assert_eq!(
            history.entry_url(history.current_entry()).map(Url::as_str),
            Some(format!("https://example.com/{}", super::MAX_HISTORY_ENTRIES + 50).as_str())
        );

        let oversized_url = url(&format!(
            "https://example.com/{}",
            "u".repeat(super::MAX_HISTORY_URL_BYTES + 1)
        ));
        history.update(
            FrameId::MAIN,
            oversized_url,
            "ok".into(),
            super::HistoryHandling::Push,
        );
        assert_eq!(
            history.entry_url(history.current_entry()).map(Url::as_str),
            Some(format!("https://example.com/{}", super::MAX_HISTORY_ENTRIES + 50).as_str())
        );

        history.set_current_title("t".repeat(super::MAX_HISTORY_TITLE_BYTES + 100));
        let title_len = history
            .snapshot()
            .entries()
            .last()
            .expect("entry")
            .title()
            .len();
        assert_eq!(title_len, super::MAX_HISTORY_TITLE_BYTES);

        let chunk = "s".repeat(512 * 1024);
        for i in 0..30 {
            history.update(
                FrameId::MAIN,
                url(&format!("https://example.com/big/{i}")),
                chunk.clone(),
                super::HistoryHandling::Push,
            );
        }
        assert!(history.total_bytes() <= super::MAX_HISTORY_TOTAL_BYTES);
    }
}
