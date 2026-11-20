//! One tab's joint session history, keyed by frame.
//!
//! The browser owns every entry of every fully active navigable in the tab.
//! A renderer mirrors the part it needs for synchronous `history` reads, but
//! it never decides the entry count or the active position: those are the
//! joint list's
//! (<https://html.spec.whatwg.org/multipage/history.html#joint-session-history>,
//! <https://html.spec.whatwg.org/multipage/browsing-the-web.html#session-history-entry>).

use std::collections::HashMap;

use renderer::HistorySnapshot as RendererHistorySnapshot;
use renderer::{FrameId, HistoryHandling};
use url::Url;

/// Stable identity of one session-history entry.
///
/// The value survives list shifts, so a traversal can name its target without
/// depending on an index
/// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#session-history>).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EntryId(u64);

impl EntryId {
    /// An identity from a value a caller already holds, such as a protocol
    /// adapter's entry id. A value that names no entry answers no target; it
    /// is never redirected to a neighbour.
    #[must_use]
    pub fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// The wire value protocol adapters put on their surface.
    #[must_use]
    pub fn get(self) -> u64 {
        self.0
    }
}

/// How a traversal names the entry it activates
/// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#traverse-the-history>).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HistoryStep {
    /// A signed distance from the active entry, as `history.go` speaks
    /// (<https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-history-go>).
    Delta(i32),
    /// An identity the caller already holds, as a CDP history entry id does
    /// (<https://chromedevtools.github.io/devtools-protocol/tot/Page/#method-navigateToHistoryEntry>).
    Entry(EntryId),
}

/// What one browser-driven commit does to the main frame's session history.
pub(crate) enum Commit {
    /// Adds an entry after the active one and drops the whole forward branch.
    Push {
        url: Url,
        /// The document state's navigable target name, resolved after the
        /// commit's target-name reset
        /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName>).
        name: Option<renderer::ContextName>,
    },
    /// Re-populates the entry a traversal selected. That entry keeps the name
    /// already stored in its document state: the entry, not the live browsing
    /// context, is the source
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName>).
    Traverse { entry: EntryId, url: Url },
}

/// One committed session-history entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryEntry {
    id: EntryId,
    url: Url,
    title: String,
}

impl HistoryEntry {
    /// Stable identity that survives list shifts.
    #[must_use]
    pub fn id(&self) -> EntryId {
        self.id
    }

    /// URL committed for this entry.
    #[must_use]
    pub fn url(&self) -> &Url {
        &self.url
    }

    /// Document title recorded when the entry committed.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }
}

/// The main frame's committed history, as protocol adapters project it.
///
/// The joint list is longer: child-frame entries are part of it, but `CDP`'s
/// navigation history is the top-level document's own list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistorySnapshot {
    entries: Vec<HistoryEntry>,
    current_index: usize,
}

impl HistorySnapshot {
    /// Entries in traversal order, oldest first.
    #[must_use]
    pub fn entries(&self) -> &[HistoryEntry] {
        &self.entries
    }

    /// Zero-based position of the active entry.
    #[must_use]
    pub fn current_index(&self) -> usize {
        self.current_index
    }
}

#[derive(Clone)]
struct Entry {
    id: EntryId,
    /// The joint step this entry occupies. Initial blank entries share the
    /// step current when their frame was created; every navigation that pushes
    /// takes the next step and makes it current
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#getting-all-used-history-steps>).
    step: u64,
    /// Position within its own frame's list, which is what a child renderer
    /// keeps its replay payloads under.
    local_index: usize,
    url: Url,
    title: String,
    state: Option<String>,
    /// Identity of the document state this entry presents. Contiguous entries
    /// can share one, and every entry that does presents the same `Document`
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#document-state>).
    document: u64,
}

/// A navigation whose document has not committed.
///
/// The entry stays out of the joint list until the frame reports its commit,
/// so a load that fails, is blocked, or is superseded adds no phantom step
/// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#finalize-a-cross-document-navigation>).
#[derive(Clone)]
enum PendingChange {
    /// A push, which takes the step after the current one at commit time.
    Push { name: Option<renderer::ContextName> },
    /// A replacement of an entry that already occupies a step.
    Replace {
        entry: EntryId,
        name: Option<renderer::ContextName>,
    },
}

/// The tab coordinator owns the list. Renderer realms only receive a snapshot
/// and submit changes after a script turn.
#[derive(Clone)]
pub(crate) struct SessionHistory {
    /// Every entry of every frame, in creation order. Traversal order is the
    /// `step` order, not this order.
    entries: Vec<Entry>,
    /// Each frame's entries in its own traversal order.
    frames: HashMap<FrameId, Vec<EntryId>>,
    /// Started navigations that have not committed, by frame. A later start
    /// replaces one, because the newest load owns the frame.
    pending: HashMap<FrameId, PendingChange>,
    /// The step of the tab's active entry
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#tn-current-session-history-step>).
    current_step: u64,
    next_document: u64,
    next_entry: u64,
    /// The navigable target name of each document state, keyed by the
    /// `document` id its entries share. Storing it once is what makes entries
    /// that share a document state agree about the name by construction; a
    /// missing id means the empty name
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#document-state>,
    /// <https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-name>).
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

    /// State one frame needs for `history.length`, `history.state`, and
    /// traversal, for its active entry.
    ///
    /// `length` is the joint step count, so a child sees every step of the
    /// tab, not only its own
    /// (<https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-history-length>).
    /// `index` is the tab's current step, which every `History` object mirrors
    /// whether or not its own frame moved
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#getting-the-history-object-length-and-index>).
    pub(crate) fn renderer_snapshot_for(&self, frame: FrameId) -> Option<RendererHistorySnapshot> {
        self.frames.get(&frame)?;
        Some(self.build_snapshot(frame, self.active_entry(frame), None))
    }

    /// The mirror for one particular entry.
    ///
    /// `length` counts the used history steps, and `index` is the step's
    /// position among them, because that is what the `History` object exposes:
    /// two entries that share a step (an initial child blank and the document
    /// it belongs to) are one step, not two
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#getting-the-history-object-length-and-index>).
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

    /// The main frame's mirror, for a tab with no renderer yet.
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

    /// Every history step the tab uses, in traversal order.
    ///
    /// Frames created later share their parent's step until their first real
    /// navigation, so a step can hold several entries and the joint length is
    /// the number of steps, not the number of entries
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#getting-all-used-history-steps>).
    fn used_steps(&self) -> Vec<u64> {
        let mut steps: Vec<u64> = self.entries.iter().map(|entry| entry.step).collect();
        steps.sort_unstable();
        steps.dedup();
        steps
    }

    /// The tab's current step in `steps`.
    ///
    /// A step can stop being used when its only entries are removed with a
    /// frame; the greatest used step at or below the current one is then the
    /// active step
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#getting-the-used-step>).
    fn current_step_index(&self, steps: &[u64]) -> usize {
        steps
            .iter()
            .rposition(|step| *step <= self.current_step)
            .unwrap_or(0)
    }

    /// The frames the joint list currently covers.
    pub(crate) fn frames(&self) -> Vec<FrameId> {
        self.frames.keys().copied().collect()
    }

    /// Committed entries and the active index for protocol adapters, projected
    /// onto the main frame.
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

    /// Records the committed document's title on the main frame's active entry.
    pub(crate) fn set_current_title(&mut self, title: String) {
        self.set_title(FrameId::MAIN, title);
    }

    /// Records a committed document's title on its frame's active entry.
    pub(crate) fn set_title(&mut self, frame: FrameId, title: String) {
        let Some(id) = self.active_entry(frame) else {
            return;
        };
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == id) {
            entry.title = title;
        }
    }

    /// Seeds a newly created child frame's initial blank entry.
    ///
    /// The entry shares the step current when the frame appeared: it belongs to
    /// the document state the tab is presenting, and it is replaced by the
    /// frame's first real navigation
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#navigate>).
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

    /// Records a child navigation the renderer started.
    ///
    /// The renderer reports its own local position so a replay can find the
    /// payload later; the browser still decides the joint step and the active
    /// position
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#finalize-a-cross-document-navigation>).
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
        // A committed navigation supersedes any uncommitted one: the newest
        // load owns the frame.
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

    /// Promotes a pending navigation after its document committed, or updates
    /// the URL of a frame's active entry, carrying a redirect's final URL
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#finalize-a-cross-document-navigation>).
    pub(crate) fn commit_url(&mut self, frame: FrameId, url: Url) {
        if let Some(change) = self.pending.remove(&frame) {
            match change {
                PendingChange::Push { name, .. } => {
                    // The step is taken now, not at start, so navigations in
                    // other frames that committed meanwhile are ordered
                    // exactly as the queue ran them
                    // (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#finalize-a-cross-document-navigation>).
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

    /// Drops an uncommitted navigation for `frame`, because a traversal now
    /// owns it.
    pub(crate) fn discard_pending(&mut self, frame: FrameId) {
        self.pending.remove(&frame);
    }

    /// Writes the active document state's navigable target name for the main
    /// frame.
    ///
    /// A commit calls this directly, and the `window.name` setter reaches it
    /// at the next commit or traversal, when the tab actor folds the registry's
    /// value in
    /// (<https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-name>).
    /// The state is shared by every entry that presents the same `Document`,
    /// so a same-document push reads the new value with no copy of its own.
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

    /// The target name stored with a session-history entry's document state.
    pub(crate) fn name(&self, entry: EntryId) -> Option<renderer::ContextName> {
        self.locate(entry)
            .and_then(|entry| self.names.get(&entry.document).cloned())
    }

    /// Applies one same-document history update in `frame`.
    pub(crate) fn update(
        &mut self,
        frame: FrameId,
        url: Url,
        state: String,
        handling: HistoryHandling,
    ) {
        let Some(active) = self.active_entry(frame) else {
            return;
        };
        match handling {
            HistoryHandling::Replace => {
                if let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == active) {
                    entry.url = url;
                    entry.state = Some(state);
                }
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

    /// Applies one browser-driven main-frame commit, or returns false if a
    /// traversal's entry was removed while its response was being fetched.
    /// Entry identities survive list shifts.
    pub(crate) fn commit(&mut self, commit: Commit) -> bool {
        match commit {
            Commit::Traverse { entry, url } => {
                let Some(index) = self.locate_index(entry) else {
                    return false;
                };
                // Repopulating an entry replaces only *its* document state:
                // adjacent entries that shared the old one keep referencing
                // it, so they also keep their document id
                // (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#populating-a-session-history-entry>).
                // The new state starts with the name the entry had.
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

    /// The step a traversal selects, or `None` when it names none.
    ///
    /// A delta counts used steps, not entries: a frame that has never navigated
    /// shares its parent's step, so going back once leaves that frame on the
    /// same document
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#traverse-the-history-by-a-delta>).
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

    /// The entry a frame presents at `at_step`: its greatest entry at or
    /// before that step. A frame created later keeps its first entry, which is
    /// the blank it appeared with.
    pub(crate) fn active_entry_at(&self, frame: FrameId, at_step: u64) -> Option<EntryId> {
        let ids = self.frames.get(&frame)?;
        ids.iter()
            .rev()
            .find(|id| self.locate(**id).is_some_and(|entry| entry.step <= at_step))
            .or_else(|| ids.first())
            .copied()
    }

    /// The entry a frame presents now.
    pub(crate) fn active_entry(&self, frame: FrameId) -> Option<EntryId> {
        self.active_entry_at(frame, self.current_step)
    }

    /// The step of the tab's active entry.
    pub(crate) fn current_step(&self) -> u64 {
        self.current_step
    }

    /// Makes `step` the tab's active entry after a traversal whose documents
    /// already moved.
    pub(crate) fn set_current_step(&mut self, step: u64) {
        self.current_step = step;
    }

    /// The main frame's active entry, for reloads that keep their position.
    pub(crate) fn current_entry(&self) -> EntryId {
        self.active_entry(FrameId::MAIN).unwrap_or(EntryId(0))
    }

    pub(crate) fn entry_url(&self, entry: EntryId) -> Option<&Url> {
        self.locate(entry).map(|entry| &entry.url)
    }

    pub(crate) fn local_index(&self, entry: EntryId) -> Option<usize> {
        self.locate(entry).map(|entry| entry.local_index)
    }

    /// Whether two entries present the same `Document`, which makes a step
    /// between them a same-document traversal
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#document-state>).
    pub(crate) fn same_document(&self, left: EntryId, right: EntryId) -> bool {
        match (self.locate(left), self.locate(right)) {
            (Some(left), Some(right)) => left.document == right.document,
            _ => false,
        }
    }

    /// Makes a same-document traversal target current. `false` when the entry
    /// is gone, which a caller must not mistake for a completed traversal.
    pub(crate) fn traverse_same_document(&mut self, entry: EntryId) -> bool {
        let Some(target) = self.locate(entry) else {
            return false;
        };
        self.current_step = target.step;
        true
    }

    /// Drops a frame the renderer tore down. Its entries leave the joint list,
    /// so a removed iframe stops counting toward every frame's
    /// `history.length`
    /// (<https://html.spec.whatwg.org/multipage/history.html#joint-session-history>).
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

    /// Drops every child frame's entries, as a renderer replacement does.
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

    /// Drops the entries after the active one, and the document-state names
    /// only those entries referenced.
    ///
    /// Clearing the forward branch spans every frame's entry list
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#clear-the-forward-session-history>).
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

    /// The frame whose entry list holds `entry`.
    fn frame_of(&self, entry: EntryId) -> Option<FrameId> {
        self.frames
            .iter()
            .find(|(_, ids)| ids.contains(&entry))
            .map(|(frame, _)| *frame)
    }

    /// The position of `entry` in `frame`'s own list.
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
        id
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
        // The traversal entry keeps its identity at index 1.
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

    /// The target name belongs to the entry's document state, so traversal
    /// reads it back instead of the live browsing-context name, a
    /// same-document push inherits it, and the `window.name` setter writes
    /// the active document state
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName>,
    /// <https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-name>).
    #[test]
    fn a_target_name_rides_the_entry_it_committed_with() {
        let named = |raw: &str| renderer::ContextName::parse(raw);
        let mut history = SessionHistory::new(url("about:blank"), named("start"));
        history.commit(push("https://example.com/a", named("first")));
        let first = history.active_entry_at(FrameId::MAIN, 1).expect("entry");
        assert_eq!(history.name(first), named("first"));

        // The setter writes the active document state.
        history.sync_current_name(named("renamed").as_ref());
        assert_eq!(history.name(first), named("renamed"));

        // A same-document push shares the document state, so it presents the
        // same name with no copy of its own.
        history.update(
            FrameId::MAIN,
            url("https://example.com/hash"),
            "{}".into(),
            renderer::HistoryHandling::Push,
        );
        let pushed = history.current_entry();
        assert_eq!(history.name(pushed), named("renamed"));
        assert_eq!(history.name(first), named("renamed"));

        // A commit that resets the name stores no name on its entry.
        history.commit(push("https://example.com/b", None));
        let current = history.current_entry();
        assert_eq!(history.name(current), None);
        assert_eq!(history.name(first), named("renamed"));

        // Traversal repopulates only its own entry, keeping the name its
        // document state had.
        assert!(history.commit(Commit::Traverse {
            entry: first,
            url: url("https://example.com/a"),
        }));
        assert_eq!(history.name(first), named("renamed"));
        assert_eq!(history.name(pushed), named("renamed"));

        // The sibling kept the old document state, so the traversed entry
        // now has its own and the setter reaches only the active one.
        history.sync_current_name(None);
        assert_eq!(history.name(first), None);
        assert_eq!(history.name(pushed), named("renamed"));
        assert_eq!(history.name(current), None);
    }

    /// Repopulating an entry must not merge its document state with the
    /// siblings that shared it: a later traversal to a sibling is a new
    /// document, not a same-document step
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#populating-a-session-history-entry>).
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

        // The sibling still presents the pre-traversal document object.
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

    /// Every frame's length is the joint step count, while each frame's state
    /// is its own active entry's. A freshly created child shares its parent's
    /// step, so the first navigation of either is one step, not two
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#getting-the-history-object-length-and-index>).
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

    /// A started network navigation is not a step until its document commits,
    /// so a load that never commits leaves the joint history untouched
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#finalize-a-cross-document-navigation>).
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

    /// A pending replacement that never commits keeps the entry it would have
    /// replaced.
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

    /// A frame whose entries all sat on steps the tab left behind can navigate
    /// again, and it still reports the joint count
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#clear-the-forward-session-history>).
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

        // Back to the blank, then push: every child step is forward history
        // and leaves the joint list.
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

    /// A push trims the forward branch, so the synchronous mirror must report
    /// the new step's position, not the old, longer length
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#url-and-history-update-steps>).
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

        // The foreground entry b is cleared and the push takes its position.
        assert!(history.traverse_same_document(entry));
        history.commit(push("https://example.com/c", None));
        snapshot = history.renderer_snapshot();
        assert_eq!((snapshot.length, snapshot.index), (3, 2));
    }
}
