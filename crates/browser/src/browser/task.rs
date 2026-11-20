use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tokio::sync::mpsc;

use crate::actor::{NewTab, RemoteWindowMessage, TabHandle, TabId, TabTask};
use crate::context::BrowserContext;
use crate::exchange::ServerInput;
use crate::manager::RendererProcessManager;

use super::{
    BrowserError, BrowserHandle, BrowserServer, Command, Notice, OpenWindowOptions, Reply,
};

/// One tab's target names.
///
/// Two values because a `window.name` assignment reaches the browser through a
/// synchronous host call, while the session-history entry it belongs to is
/// written later, when the tab actor commits or traverses. The live value is
/// what a `window.open` searches and what the next browser-driven commit
/// resolves; the document value is what the entry the document is leaving
/// takes, and it survives the renderer's cross-origin reset so a reset that
/// lands before the browser's commit cannot lose it
/// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName>).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct TabNames {
    /// The browsing context's target name. A cross-origin reset clears it
    /// before the new document installs.
    pub(crate) live: Option<renderer::ContextName>,
    /// The active document state's navigable target name. `window.name` writes
    /// it, and the next commit copies it onto the entry the document leaves
    /// with, so a reset that lands first cannot lose it.
    pub(crate) document: Option<renderer::ContextName>,
}

impl TabNames {
    /// The pair a commit installs: one name for the browsing context and the
    /// document state it just settled.
    pub(crate) fn from_name(name: Option<&renderer::ContextName>) -> Self {
        Self {
            live: name.cloned(),
            document: name.cloned(),
        }
    }
}

/// All browser-owned state for one tab, kept under a single lock so creation,
/// renames, sandbox updates, and close cleanup are always atomic.
#[derive(Default)]
struct TabRecord {
    task: Option<TabTask>,
    opener: Option<TabId>,
    opener_frame: Option<renderer::FrameId>,
    names: TabNames,
    /// Popup sandboxing flag set inherited by this tab's documents.
    sandbox: renderer::SandboxFlags,
    /// Enforced CSP `sandbox` flags of the tab's committed main document.
    main_csp: renderer::SandboxFlags,
    /// Active sandboxing flag set of each child frame the browser routes.
    /// Cleared on renderer replacement without touching `main_csp`.
    frames: HashMap<renderer::FrameId, FrameSandbox>,
    /// Whether `window.open` created this tab (script-closable even with
    /// `noopener` and before its first navigation).
    script_created: bool,
}

impl TabRecord {
    fn is_live(&self) -> bool {
        self.task.is_some()
    }
}

/// The browser's tab table, shared with renderer service tasks.
///
/// Service tasks resolve cross-tab calls through this registry, so the browser
/// owner task never waits on a tab coordinator and the wait-for graph stays
/// acyclic. `postMessage` enqueues on the target tab's delivery mailbox and
/// returns; a busy target cannot block the sender's renderer.
#[derive(Clone)]
pub(crate) struct TabRegistry {
    records: Arc<Mutex<HashMap<TabId, TabRecord>>>,
}

/// One frame's sandbox flags, as the browser owns them.
#[derive(Clone, Copy, Debug)]
struct FrameSandbox {
    /// The flags the frame was created with: the `iframe sandbox` attribute
    /// unioned with the parent's active set, or the popup set of an auxiliary
    /// context. A navigation cannot change these.
    creation: renderer::SandboxFlags,
    /// The flags the frame's current document runs under: `creation` unioned
    /// with the enforced CSP `sandbox` directives of its last response.
    active: renderer::SandboxFlags,
}

impl TabRegistry {
    fn new() -> Self {
        Self {
            records: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<TabId, TabRecord>> {
        self.records.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The tab `caller`'s browsing context group holds under `name`, when one
    /// is open. The group is the opener chain's root, so a name opened by one
    /// tab is visible to every tab in the same group and to no other.
    pub(crate) fn find_by_name(
        &self,
        caller: TabId,
        name: &renderer::ContextName,
    ) -> Option<TabId> {
        let records = self.lock();
        let root = group_root(&records, caller);
        records
            .iter()
            .find(|(id, record)| {
                record
                    .names
                    .live
                    .as_ref()
                    .is_some_and(|live| live.as_str() == name.as_str())
                    && group_root(&records, **id) == root
            })
            .map(|(id, _)| *id)
    }

    /// Records the target name of a tab that is not in the table yet.
    pub(crate) fn insert_name(&self, id: TabId, name: Option<&renderer::ContextName>) {
        let mut records = self.lock();
        records.entry(id).or_default().names = TabNames::from_name(name);
    }

    /// Applies a name change the renderer reported, unless its tab has closed.
    pub(crate) fn rename(&self, id: TabId, change: renderer::NameChange) {
        let mut records = self.lock();
        let Some(record) = records.get_mut(&id).filter(|r| r.is_live()) else {
            return;
        };
        match change {
            renderer::NameChange::Assigned(name) => {
                record.names = TabNames::from_name(name.as_ref());
            }
            // The renderer's cross-origin reset lands before the browser
            // commits the new document. The name belongs to the document being
            // left, so its entry still takes it when the browser commits
            // (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName>).
            renderer::NameChange::Reset => {
                record.names.live = None;
            }
        }
    }

    /// Installs the document-state name a renderer-driven commit settled.
    pub(crate) fn settle_document_name(
        &self,
        id: TabId,
        outgoing: Option<&renderer::ContextName>,
        name: Option<renderer::ContextName>,
    ) {
        let mut records = self.lock();
        let Some(record) = records.get_mut(&id).filter(|r| r.is_live()) else {
            return;
        };
        if record.names.document.as_ref() != outgoing {
            return;
        }
        record.names.document = name;
    }

    /// Installs the names a browser-driven commit settled, returning the pair
    /// it replaced. `None` means the tab closed while the navigation was in
    /// flight.
    pub(crate) fn install_commit(
        &self,
        id: TabId,
        name: Option<&renderer::ContextName>,
    ) -> Option<TabNames> {
        let mut records = self.lock();
        let record = records.get_mut(&id).filter(|r| r.is_live())?;
        let previous = std::mem::replace(&mut record.names, TabNames::from_name(name));
        Some(previous)
    }

    /// Puts back the pair [`Self::install_commit`] replaced, unless a script
    /// renamed the tab after the commit, in which case the newer value wins.
    pub(crate) fn restore_commit(
        &self,
        id: TabId,
        previous: Option<TabNames>,
        installed: &TabNames,
    ) {
        let Some(previous) = previous else {
            return;
        };
        let mut records = self.lock();
        let Some(record) = records.get_mut(&id).filter(|r| r.is_live()) else {
            return;
        };
        if &record.names == installed {
            record.names = previous;
        }
    }

    /// The target name of one tab, when it has one.
    pub(crate) fn name(&self, id: TabId) -> Option<renderer::ContextName> {
        self.lock()
            .get(&id)
            .and_then(|record| record.names.live.clone())
    }

    /// The active document state's navigable target name
    /// (<https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-name>).
    pub(crate) fn document_name(&self, id: TabId) -> Option<renderer::ContextName> {
        self.lock()
            .get(&id)
            .and_then(|record| record.names.document.clone())
    }

    /// Registers a tab id with no coordinator behind it, for unit tests that
    /// exercise the registry's own rules.
    #[cfg(test)]
    fn add_fixture(&self, id: TabId) {
        self.insert(id, TabTask::fixture(id));
    }

    /// The handle of one live tab.
    pub(crate) fn tab(&self, id: TabId) -> Option<TabHandle> {
        self.lock()
            .get(&id)
            .and_then(|record| record.task.as_ref())
            .map(|task| task.handle.clone())
    }

    /// The opener of one live tab.
    pub(crate) fn opener(&self, id: TabId) -> Option<TabId> {
        self.lock().get(&id).and_then(|record| record.opener)
    }

    /// Records the popup sandboxing flag set a tab's documents inherit
    /// (<https://html.spec.whatwg.org/multipage/browsers.html#popup-sandboxing-flag-set>).
    pub(crate) fn set_sandbox(&self, id: TabId, sandbox: renderer::SandboxFlags) {
        self.lock().entry(id).or_default().sandbox = sandbox;
    }

    /// The popup sandboxing flag set of one live tab; empty when it has none.
    #[cfg(test)]
    pub(crate) fn sandbox(&self, id: TabId) -> renderer::SandboxFlags {
        self.lock().get(&id).map_or_default(|record| record.sandbox)
    }

    /// The enforced CSP `sandbox` flags of the tab's committed main document.
    pub(crate) fn main_csp(&self, id: TabId) -> renderer::SandboxFlags {
        self.lock()
            .get(&id)
            .map_or_default(|record| record.main_csp)
    }

    /// Records a frame the renderer created, before its document can run
    /// script
    /// (<https://html.spec.whatwg.org/multipage/browsers.html#determining-the-creation-sandboxing-flags>,
    /// <https://html.spec.whatwg.org/multipage/browsers.html#popup-sandboxing-flag-set>).
    pub(crate) fn register_frame(
        &self,
        tab: TabId,
        frame: renderer::FrameId,
        parent: renderer::FrameId,
        kind: renderer::FrameKind,
        reported: renderer::SandboxFlags,
    ) -> bool {
        if frame == renderer::FrameId::MAIN {
            return false;
        }
        let mut records = self.lock();
        let record = records.entry(tab).or_default();
        if record.frames.contains_key(&frame) {
            return false;
        }
        let parent_active = match record.frames.get(&parent) {
            Some(parent_record) => parent_record.active,
            None if parent == renderer::FrameId::MAIN => record.sandbox,
            // An honest renderer reports a parent before the child that it
            // embeds; a missing record is a bug or a forgery, and the full set
            // is the only safe answer.
            None => renderer::SandboxFlags::parse(Some("")),
        };
        let creation = match kind {
            renderer::FrameKind::Child => parent_active.union(reported),
            renderer::FrameKind::Auxiliary => parent_active.auxiliary_flags(),
        };
        record.frames.insert(
            frame,
            FrameSandbox {
                creation,
                active: creation,
            },
        );
        true
    }

    /// The active sandboxing flag set of `frame` in `tab`.
    pub(crate) fn frame_sandbox(
        &self,
        tab: TabId,
        frame: renderer::FrameId,
    ) -> Option<renderer::SandboxFlags> {
        let records = self.lock();
        if frame == renderer::FrameId::MAIN {
            let (sandbox, main_csp) = records
                .get(&tab)
                .map_or_default(|r| (r.sandbox, r.main_csp));
            return Some(sandbox.union(main_csp));
        }
        records
            .get(&tab)
            .and_then(|record| record.frames.get(&frame))
            .map(|entry| entry.active)
    }

    /// Replaces the main frame's CSP `sandbox` for a commit.
    pub(crate) fn reset_main_sandbox(&self, tab: TabId, csp: renderer::SandboxFlags) {
        self.lock().entry(tab).or_default().main_csp = csp;
    }

    /// Narrows a frame's active set for the response it just navigated to.
    pub(crate) fn commit_frame_sandbox(
        &self,
        tab: TabId,
        frame: renderer::FrameId,
        csp: renderer::SandboxFlags,
    ) {
        let mut records = self.lock();
        if frame == renderer::FrameId::MAIN {
            records.entry(tab).or_default().main_csp = csp;
            return;
        }
        if let Some(entry) = records
            .get_mut(&tab)
            .and_then(|record| record.frames.get_mut(&frame))
        {
            entry.active = entry.creation.union(csp);
        }
    }

    /// Drops a child frame the renderer tore down. Returns false only for the
    /// main frame.
    pub(crate) fn remove_frame(&self, tab: TabId, frame: renderer::FrameId) -> bool {
        if frame == renderer::FrameId::MAIN {
            return false;
        }
        if let Some(record) = self.lock().get_mut(&tab) {
            record.frames.remove(&frame);
        }
        true
    }

    /// Drops every child frame record for a tab whose renderer is being
    /// replaced; the main frame's CSP survives.
    pub(crate) fn clear_frames(&self, tab: TabId) {
        if let Some(record) = self.lock().get_mut(&tab) {
            record.frames.clear();
        }
    }

    /// Queues one `postMessage` delivery on the target tab's mailbox.
    pub(crate) fn deliver(&self, id: TabId, message: RemoteWindowMessage) -> bool {
        let sender = self
            .lock()
            .get(&id)
            .and_then(|record| record.task.as_ref())
            .map(TabTask::deliveries);
        match sender {
            Some(sender) => match sender.try_send(message) {
                Ok(()) => {
                    logging::debug!(
                        target: "browser::tabs",
                        "window message queued for tab {id}"
                    );
                    true
                }
                Err(mpsc::error::TrySendError::Full(_)) => {
                    logging::debug!(
                        target: "browser::tabs",
                        "window message dropped: mailbox full for tab {id}"
                    );
                    false
                }
                Err(mpsc::error::TrySendError::Closed(_)) => false,
            },
            None => false,
        }
    }

    fn ids(&self) -> Vec<TabId> {
        self.lock()
            .iter()
            .filter(|(_, record)| record.is_live())
            .map(|(id, _)| *id)
            .collect()
    }

    fn insert(&self, id: TabId, task: TabTask) {
        self.lock().entry(id).or_default().task = Some(task);
    }

    /// Atomically removes a tab and all of its associated state (names,
    /// opener, sandboxes, child frames, and script-created flag).
    fn remove(&self, id: TabId) -> Option<TabTask> {
        self.lock().remove(&id)?.task
    }

    fn drain(&self) -> Vec<TabTask> {
        self.lock()
            .drain()
            .filter_map(|(_, record)| record.task)
            .collect()
    }

    /// Records that `id` was opened by `opener`'s frame `frame`.
    fn set_opener(&self, id: TabId, opener: TabId, frame: renderer::FrameId) {
        let mut records = self.lock();
        let record = records.entry(id).or_default();
        record.opener = Some(opener);
        record.opener_frame = Some(frame);
    }

    /// The frame inside `id`'s opener that opened it.
    pub(crate) fn opener_frame(&self, id: TabId) -> Option<renderer::FrameId> {
        self.lock().get(&id).and_then(|record| record.opener_frame)
    }

    /// Records that `window.open` created `id`
    /// (<https://html.spec.whatwg.org/multipage/nav-history-apis.html#script-closable>).
    fn mark_script_created(&self, id: TabId) {
        self.lock().entry(id).or_default().script_created = true;
    }

    /// Whether script created `id`, rather than a form, a link, or the browser
    /// itself.
    pub(crate) fn is_script_created(&self, id: TabId) -> bool {
        self.lock()
            .get(&id)
            .is_some_and(|record| record.script_created)
    }
}

/// The top-level tab of the browsing context group `tab` belongs to: the end
/// of its opener chain. Tab ids are never reused, so a group whose root has
/// closed keeps its identity until the last member closes.
fn group_root(records: &HashMap<TabId, TabRecord>, tab: TabId) -> TabId {
    let mut root = tab;
    // A chain longer than the table is a cycle, which cannot be built; stop
    // rather than loop if one ever is.
    for _ in 0..=records.len() {
        match records.get(&root).and_then(|record| record.opener) {
            Some(opener) => root = opener,
            None => break,
        }
    }
    root
}

pub(crate) struct BrowserTask {
    server: BrowserServer,
    context: BrowserContext,
    renderers: Arc<RendererProcessManager>,
    tabs: TabRegistry,
    next_tab: Option<u64>,
}

enum DispatchOutcome {
    Continue(Reply),
    Stop(Reply),
}

impl BrowserTask {
    pub(super) fn new(
        server: BrowserServer,
        context: BrowserContext,
        browser: BrowserHandle,
    ) -> Self {
        let tabs = TabRegistry::new();
        let identity = browser.identity().clone();
        let renderers = Arc::new(RendererProcessManager::new(
            context.partition_services(),
            context.session_storage(),
            browser,
            tabs.clone(),
            identity,
        ));
        Self {
            server,
            context,
            renderers,
            tabs,
            next_tab: Some(1),
        }
    }

    pub(super) async fn run(mut self) {
        loop {
            let Some(input) = self.server.recv().await else {
                break;
            };
            match input {
                ServerInput::Call { id, body } => match self.dispatch(body).await {
                    DispatchOutcome::Continue(reply) => {
                        if self.server.reply(id, reply).await.is_err() {
                            break;
                        }
                    }
                    DispatchOutcome::Stop(reply) => {
                        let _result = self.server.reply(id, reply).await;
                        return;
                    }
                },
                ServerInput::Notify(Notice::Close) => break,
                ServerInput::RequestChunk
                | ServerInput::RequestEnd
                | ServerInput::Cancel { .. } => {}
            }
        }
        let _result = self.shutdown().await;
    }

    async fn dispatch(&mut self, command: Command) -> DispatchOutcome {
        let reply = match command {
            Command::CreateTab => Reply::CreateTab(self.create_tab(NewTab::browser_created())),
            Command::Tabs => Reply::Tabs(self.tabs.ids()),
            Command::Tab { id } => Reply::Tab(self.tabs.tab(id).ok_or(BrowserError::UnknownTab)),
            Command::CloseTab { id } => Reply::CloseTab(self.close_tab(id)),
            Command::CookieRecords { url } => {
                Reply::CookieRecords(self.context.cookie_records(&url))
            }
            Command::ClearCookies => {
                self.context.clear_cookies();
                Reply::ClearCookies
            }
            Command::AddCookie { cookie, url } => {
                Reply::AddCookie(self.context.add_cookie(&cookie, &url))
            }
            Command::OpenWindow(request) => Reply::OpenWindow(self.open_window(request)),
            Command::Close => {
                let result = self.shutdown().await;
                return DispatchOutcome::Stop(Reply::Close(result));
            }
        };
        DispatchOutcome::Continue(reply)
    }

    fn create_tab(&mut self, tab: NewTab) -> Result<TabHandle, BrowserError> {
        let raw = self.next_tab.ok_or(BrowserError::TabIdExhausted)?;
        self.next_tab = raw.checked_add(1);
        let id = TabId::new(raw);
        self.context.create_tab(id);
        self.tabs.insert_name(id, tab.name.as_ref());
        self.tabs.set_sandbox(id, tab.popup_sandbox);
        if let (Some(opener), Some(frame)) = (tab.opener, tab.opener_frame) {
            self.tabs.set_opener(id, opener, frame);
        }
        let task = TabTask::spawn(
            id,
            self.context.tab_network(),
            Arc::clone(&self.renderers),
            self.tabs.clone(),
            tab,
        );
        let handle = task.handle.clone();
        self.tabs.insert(id, task);
        Ok(handle)
    }

    fn close_tab(&mut self, id: TabId) -> Result<(), BrowserError> {
        let Some(mut task) = self.tabs.remove(id) else {
            return Err(BrowserError::UnknownTab);
        };
        // Never wait on the tab coordinator here: the browser owner task must
        // not join a wait-for cycle with a renderer that needs it. The task
        // keeps its own lifecycle and reaps itself.
        tokio::spawn(async move { task.shutdown().await });
        self.context.close_tab(id);
        Ok(())
    }

    fn open_window(&mut self, request: OpenWindowOptions) -> Result<TabId, BrowserError> {
        let OpenWindowOptions {
            url,
            target,
            source,
            source_frame,
            opener,
            initiator,
            popup_sandbox,
        } = request;
        // A popup keeps its target name across a cross-origin navigation only
        // while its opener browsing context is live
        // (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName>).
        let opener_tab = source.filter(|_| opener == renderer::OpenerPolicy::Keep);
        let handle = self.create_tab(NewTab {
            name: target.new_context_name().cloned(),
            opener: opener_tab,
            opener_frame: opener_tab.map(|_| source_frame),
            popup_sandbox,
            // The initial `about:blank` inherits the opener's origin, which is
            // what the first navigation's target-name reset compares against
            // (<https://html.spec.whatwg.org/multipage/browsers.html#determining-the-origin>).
            document_origin: initiator.origin.clone(),
            // The blank also inherits the opener's secure-context bit, so a
            // popup opened by a `data:` document is insecure
            // (<https://html.spec.whatwg.org/multipage/webappapis.html#secure-contexts>).
            creator_secure_context: Some(initiator.secure_context),
        })?;
        let id = handle.id();
        self.tabs.mark_script_created(id);
        if let Some(opener_tab) = opener_tab {
            self.context.copy_session(opener_tab, id);
        }
        // A fresh tab is already on `about:blank`, so neither a missing URL
        // nor an explicit `about:blank` needs a navigation.
        if let Some(url) = url.filter(|url| url.as_str() != "about:blank") {
            tokio::spawn(async move {
                let _result = handle.goto_from(url.as_str(), initiator).await;
            });
        }
        Ok(id)
    }

    async fn shutdown(&mut self) -> std::io::Result<()> {
        for mut task in self.tabs.drain() {
            task.shutdown().await;
        }
        self.context.persist().await
    }
}

#[cfg(test)]
mod tests {
    use super::TabRegistry;
    use crate::actor::TabId;

    fn name(raw: &str) -> renderer::ContextName {
        renderer::ContextName::parse(raw).expect("a test name is non-empty")
    }

    /// A cleared name is removed, not stored as an empty one. `ContextName`
    /// cannot hold `""`, so the page-side lookup can no longer ask for the
    /// name that would match every cleared tab (#235).
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName>).
    #[test]
    fn a_cleared_name_is_no_name() {
        let tabs = TabRegistry::new();
        let opener = TabId::new(1);
        let popup = TabId::new(2);
        tabs.set_opener(popup, opener, renderer::FrameId::MAIN);
        tabs.insert_name(popup, Some(&name("peer")));
        assert_eq!(tabs.name(popup), Some(name("peer")));
        assert_eq!(tabs.find_by_name(opener, &name("peer")), Some(popup));
        tabs.insert_name(popup, None);
        assert_eq!(tabs.name(popup), None);
        assert_eq!(tabs.find_by_name(opener, &name("peer")), None);
    }

    /// A reset clears the live name and leaves the document state's name for
    /// the entry the leaving document owns; an assignment of the empty string
    /// clears both (#235).
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName>).
    #[tokio::test]
    async fn a_reset_clears_only_the_live_name() {
        let tabs = TabRegistry::new();
        let tab = TabId::new(1);
        tabs.add_fixture(tab);
        tabs.rename(tab, renderer::NameChange::Assigned(Some(name("peer"))));
        assert_eq!(tabs.name(tab), Some(name("peer")));
        assert_eq!(tabs.document_name(tab), Some(name("peer")));

        tabs.rename(tab, renderer::NameChange::Reset);
        assert_eq!(tabs.name(tab), None);
        assert_eq!(
            tabs.document_name(tab),
            Some(name("peer")),
            "the document being left keeps its own name"
        );

        tabs.rename(tab, renderer::NameChange::Assigned(None));
        assert_eq!(tabs.name(tab), None);
        assert_eq!(tabs.document_name(tab), None);
    }

    /// A renderer-driven commit settles the new document's name unless a script
    /// renamed the tab after the commit event was queued.
    #[tokio::test]
    async fn a_commit_settles_the_name_unless_a_script_renamed_it() {
        let tabs = TabRegistry::new();
        let tab = TabId::new(1);
        tabs.add_fixture(tab);
        tabs.rename(tab, renderer::NameChange::Assigned(Some(name("old"))));

        // The commit replaced `old` with no name, and nothing renamed since.
        let previous = tabs.install_commit(tab, None).expect("live tab");
        assert_eq!(previous.document, Some(name("old")));
        tabs.settle_document_name(tab, Some(&name("old")), None);
        assert_eq!(tabs.document_name(tab), None);

        // The new document set its own name before the browser handled the
        // event, so the commit's value loses.
        tabs.rename(tab, renderer::NameChange::Assigned(Some(name("new"))));
        tabs.settle_document_name(tab, Some(&name("old")), None);
        assert_eq!(tabs.document_name(tab), Some(name("new")));
    }

    /// A failed mount puts back what it replaced, unless a script renamed the
    /// tab while the mount was running.
    #[tokio::test]
    async fn a_failed_mount_restores_the_names_it_replaced() {
        let tabs = TabRegistry::new();
        let tab = TabId::new(1);
        tabs.add_fixture(tab);
        let installed = super::TabNames::from_name(Some(&name("kept")));
        let previous = tabs.install_commit(tab, Some(&name("kept")));
        assert_eq!(previous, Some(super::TabNames::default()));
        tabs.restore_commit(tab, previous, &installed);
        assert_eq!(tabs.name(tab), None);

        // The installed value is no longer current, so restoring would lose
        // the newer assignment.
        let previous = tabs.install_commit(tab, Some(&name("kept")));
        tabs.rename(tab, renderer::NameChange::Assigned(Some(name("newer"))));
        tabs.restore_commit(tab, previous, &installed);
        assert_eq!(tabs.name(tab), Some(name("newer")));
    }

    /// A closed tab takes its names with it, and a coordinator still finishing
    /// a commit cannot put one back
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName>).
    #[tokio::test]
    async fn a_closed_tab_takes_its_names_with_it() {
        let tabs = TabRegistry::new();
        let opener = TabId::new(1);
        let popup = TabId::new(2);
        tabs.add_fixture(opener);
        tabs.add_fixture(popup);
        tabs.set_opener(popup, opener, renderer::FrameId::MAIN);
        tabs.insert_name(popup, Some(&name("peer")));
        assert_eq!(tabs.find_by_name(opener, &name("peer")), Some(popup));

        let _task = tabs.remove(popup).expect("live tab");
        assert_eq!(tabs.name(popup), None);
        tabs.rename(popup, renderer::NameChange::Assigned(Some(name("peer"))));
        assert_eq!(tabs.name(popup), None);
        tabs.settle_document_name(popup, None, Some(name("peer")));
        assert_eq!(tabs.name(popup), None);
        assert_eq!(tabs.install_commit(popup, Some(&name("peer"))), None);
        assert_eq!(tabs.find_by_name(opener, &name("peer")), None);
    }

    /// The lookup is scoped to the opener chain: a name one group holds is not
    /// visible to a tab outside it
    /// (<https://html.spec.whatwg.org/multipage/document-sequences.html#the-rules-for-choosing-a-navigable>).
    #[test]
    fn a_name_is_found_through_the_opener_chain_only() {
        let tabs = TabRegistry::new();
        let opener = TabId::new(1);
        let stranger = TabId::new(2);
        let popup = TabId::new(3);
        tabs.set_opener(popup, opener, renderer::FrameId::MAIN);
        tabs.insert_name(popup, Some(&name("peer")));
        assert_eq!(tabs.find_by_name(opener, &name("peer")), Some(popup));
        assert_eq!(tabs.find_by_name(popup, &name("peer")), Some(popup));
        assert_eq!(tabs.find_by_name(stranger, &name("peer")), None);
    }

    /// The browser's own sandbox copy is what a `window.open` is checked
    /// against and what a new tab inherits, so it must survive a renderer
    /// replacement and disappear with its tab
    /// (<https://html.spec.whatwg.org/multipage/browsers.html#popup-sandboxing-flag-set>).
    #[test]
    fn a_tab_keeps_its_popup_sandbox_until_it_closes() {
        let tabs = TabRegistry::new();
        let plain = TabId::new(1);
        let sandboxed = TabId::new(2);
        assert!(!tabs.sandbox(sandboxed).blocks_navigation());
        tabs.set_sandbox(
            sandboxed,
            renderer::SandboxFlags::parse(Some("allow-scripts allow-popups")),
        );
        assert!(tabs.sandbox(sandboxed).blocks_navigation());
        assert!(tabs.sandbox(sandboxed).origin_is_opaque());
        assert!(!tabs.sandbox(plain).blocks_navigation());
        tabs.remove(sandboxed);
        assert!(!tabs.sandbox(sandboxed).blocks_navigation());
    }

    /// The browser owns each frame's sandbox set: a child's report can only
    /// add to its parent's, a response's CSP `sandbox` can only narrow the
    /// current document, an auxiliary context takes the opener's popup set,
    /// and a renderer replacement clears records whose frame ids restart
    /// (<https://html.spec.whatwg.org/multipage/browsers.html#determining-the-creation-sandboxing-flags>,
    /// <https://w3c.github.io/webappsec-csp/#sandbox>).
    #[test]
    fn a_browser_owns_each_frames_sandbox_set() {
        let tabs = TabRegistry::new();
        let tab = TabId::new(1);
        let main = renderer::FrameId::MAIN;
        let child = renderer::FrameId::new(7);
        tabs.set_sandbox(tab, renderer::SandboxFlags::empty());

        // An unsandboxed child stays unsandboxed when it reports nothing.
        assert!(tabs.register_frame(
            tab,
            child,
            main,
            renderer::FrameKind::Child,
            renderer::SandboxFlags::empty(),
        ));
        assert_eq!(
            tabs.frame_sandbox(tab, child),
            Some(renderer::SandboxFlags::empty())
        );

        // The main frame and a duplicate id are not legitimate reports.
        assert!(!tabs.register_frame(
            tab,
            main,
            main,
            renderer::FrameKind::Child,
            renderer::SandboxFlags::empty(),
        ));
        assert!(!tabs.register_frame(
            tab,
            child,
            main,
            renderer::FrameKind::Child,
            renderer::SandboxFlags::parse(Some("")),
        ));
        assert!(!tabs.remove_frame(tab, main));

        // The reported attribute can only add to the parent's set.
        let sandboxed =
            renderer::SandboxFlags::parse(Some("allow-scripts allow-popups allow-same-origin"));
        let restricted = renderer::FrameId::new(11);
        assert!(tabs.register_frame(tab, restricted, main, renderer::FrameKind::Child, sandboxed,));
        let active = tabs.frame_sandbox(tab, restricted).expect("restricted");
        assert!(active.blocks_navigation());
        assert!(
            !active.origin_is_opaque(),
            "the attribute allows same-origin"
        );

        // A response's CSP `sandbox` narrows the same frame: the creation
        // flags still block navigation, and the CSP removes the same-origin
        // release.
        let grandchild = renderer::FrameId::new(8);
        assert!(tabs.register_frame(
            tab,
            grandchild,
            restricted,
            renderer::FrameKind::Child,
            renderer::SandboxFlags::empty(),
        ));
        tabs.commit_frame_sandbox(
            tab,
            grandchild,
            renderer::SandboxFlags::parse(Some("allow-scripts allow-popups")),
        );
        let active = tabs.frame_sandbox(tab, grandchild).expect("grandchild");
        assert!(active.blocks_navigation(), "the parent flag survives");
        assert!(active.origin_is_opaque(), "the CSP removed same-origin");

        // An auxiliary context takes exactly the opener's popup set:
        // `allow-popups-to-escape-sandbox` empties it.
        let escaping = renderer::SandboxFlags::parse(Some(
            "allow-scripts allow-popups allow-popups-to-escape-sandbox",
        ));
        let opener = renderer::FrameId::new(9);
        assert!(tabs.register_frame(tab, opener, main, renderer::FrameKind::Child, escaping,));
        let auxiliary = renderer::FrameId::new(10);
        assert!(tabs.register_frame(
            tab,
            auxiliary,
            opener,
            renderer::FrameKind::Auxiliary,
            renderer::SandboxFlags::empty(),
        ));
        assert!(
            tabs.frame_sandbox(tab, auxiliary)
                .expect("auxiliary")
                .is_empty(),
            "the escaping popup starts unsandboxed"
        );

        // A renderer replacement restarts frame ids, so the child records must
        // go; the main frame keeps the tab set and the committed CSP.
        tabs.reset_main_sandbox(tab, renderer::SandboxFlags::parse(Some("allow-scripts")));
        tabs.clear_frames(tab);
        assert_eq!(tabs.frame_sandbox(tab, child), None);
        assert_eq!(
            tabs.frame_sandbox(tab, main),
            Some(renderer::SandboxFlags::parse(Some("allow-scripts")))
        );
    }
}
