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

// https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct TabNames {
    pub(crate) live: Option<renderer::ContextName>,
    pub(crate) document: Option<renderer::ContextName>,
}

impl TabNames {
    pub(crate) fn from_name(name: Option<&renderer::ContextName>) -> Self {
        Self {
            live: name.cloned(),
            document: name.cloned(),
        }
    }
}

#[derive(Default)]
struct TabRecord {
    task: Option<TabTask>,
    opener: Option<TabId>,
    opener_frame: Option<renderer::FrameId>,
    names: TabNames,
    frames: crate::frames::FrameTree,
    script_created: bool,
}

impl TabRecord {
    fn is_live(&self) -> bool {
        self.task.is_some()
    }
}

#[derive(Clone)]
pub(crate) struct TabRegistry {
    records: Arc<Mutex<HashMap<TabId, TabRecord>>>,
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

    pub(crate) fn insert_name(&self, id: TabId, name: Option<&renderer::ContextName>) {
        let mut records = self.lock();
        records.entry(id).or_default().names = TabNames::from_name(name);
    }

    pub(crate) fn rename(&self, id: TabId, change: renderer::NameChange) {
        let mut records = self.lock();
        let Some(record) = records.get_mut(&id).filter(|r| r.is_live()) else {
            return;
        };
        match change {
            renderer::NameChange::Assigned(name) => {
                record.names = TabNames::from_name(name.as_ref());
            }
            // https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName
            renderer::NameChange::Reset => {
                record.names.live = None;
            }
        }
    }

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

    pub(crate) fn name(&self, id: TabId) -> Option<renderer::ContextName> {
        self.lock()
            .get(&id)
            .and_then(|record| record.names.live.clone())
    }

    // https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-name
    pub(crate) fn document_name(&self, id: TabId) -> Option<renderer::ContextName> {
        self.lock()
            .get(&id)
            .and_then(|record| record.names.document.clone())
    }

    #[cfg(test)]
    fn add_fixture(&self, id: TabId) {
        self.insert(id, TabTask::fixture(id));
    }

    pub(crate) fn tab(&self, id: TabId) -> Option<TabHandle> {
        self.lock()
            .get(&id)
            .and_then(|record| record.task.as_ref())
            .map(|task| task.handle.clone())
    }

    pub(crate) fn opener(&self, id: TabId) -> Option<TabId> {
        self.lock().get(&id).and_then(|record| record.opener)
    }

    // https://html.spec.whatwg.org/multipage/browsers.html#popup-sandboxing-flag-set
    pub(crate) fn set_sandbox(&self, id: TabId, sandbox: renderer::SandboxFlags) {
        self.lock()
            .entry(id)
            .or_default()
            .frames
            .set_popup_sandbox(sandbox);
    }

    #[cfg(test)]
    pub(crate) fn sandbox(&self, id: TabId) -> renderer::SandboxFlags {
        self.lock()
            .get(&id)
            .map_or_default(|record| record.frames.popup_sandbox())
    }

    pub(crate) fn main_csp(&self, id: TabId) -> renderer::SandboxFlags {
        self.lock()
            .get(&id)
            .map_or_default(|record| record.frames.main_csp())
    }

    // https://html.spec.whatwg.org/multipage/browsers.html#determining-the-creation-sandboxing-flags
    // https://html.spec.whatwg.org/multipage/browsers.html#popup-sandboxing-flag-set
    pub(crate) fn register_frame(
        &self,
        tab: TabId,
        frame: renderer::FrameId,
        parent: renderer::FrameId,
        kind: renderer::FrameKind,
        reported: renderer::SandboxFlags,
    ) -> bool {
        self.lock()
            .entry(tab)
            .or_default()
            .frames
            .register(frame, parent, kind, reported)
    }

    pub(crate) fn frame_sandbox(
        &self,
        tab: TabId,
        frame: renderer::FrameId,
    ) -> Option<renderer::SandboxFlags> {
        let records = self.lock();
        match records.get(&tab) {
            Some(record) => record.frames.sandbox(frame),
            None if frame == renderer::FrameId::MAIN => Some(renderer::SandboxFlags::empty()),
            None => None,
        }
    }

    pub(crate) fn reset_main_sandbox(&self, tab: TabId, csp: renderer::SandboxFlags) {
        self.lock()
            .entry(tab)
            .or_default()
            .frames
            .reset_main_csp(csp);
    }

    // https://w3c.github.io/webappsec-csp/#sandbox
    pub(crate) fn commit_child_frame(
        &self,
        tab: TabId,
        frame: renderer::FrameId,
        url: &url::Url,
        csp: renderer::SandboxFlags,
    ) {
        if let Some(record) = self.lock().get_mut(&tab) {
            record.frames.commit_child(frame, url, csp);
        }
    }

    // https://chromium.googlesource.com/chromium/src/+/main/docs/process_model_and_site_isolation.md#process-locks
    pub(crate) fn authorize_origin(
        &self,
        tab: TabId,
        main: &crate::site::Site,
        claim: &renderer::SerializedOrigin,
    ) -> Option<url::Origin> {
        match self.lock().get(&tab) {
            Some(record) => record.frames.authorize_origin(main, claim),
            None => main.authorize_origin(claim),
        }
    }

    pub(crate) fn authorize_url(
        &self,
        tab: TabId,
        main: &crate::site::Site,
        spec: &str,
    ) -> Option<url::Url> {
        match self.lock().get(&tab) {
            Some(record) => record.frames.authorize_url(main, spec),
            None => main.authorize_url(spec),
        }
    }

    pub(crate) fn allows_url(&self, tab: TabId, main: &crate::site::Site, url: &url::Url) -> bool {
        match self.lock().get(&tab) {
            Some(record) => record.frames.allows_url(main, url),
            None => main.allows_url(url),
        }
    }

    pub(crate) fn storage_key(
        &self,
        tab: TabId,
        main: &crate::site::Site,
        claim: &renderer::SerializedOrigin,
    ) -> Option<renderer::StorageKey> {
        renderer::StorageKey::from_origin(&self.authorize_origin(tab, main, claim)?)
    }

    pub(crate) fn allows_message_origin(
        &self,
        tab: TabId,
        main: &crate::site::Site,
        claim: &renderer::SerializedOrigin,
    ) -> bool {
        match self.lock().get(&tab) {
            Some(record) => record.frames.allows_message_origin(main, claim),
            None => main.allows_message_origin(claim),
        }
    }

    pub(crate) fn remove_frame(&self, tab: TabId, frame: renderer::FrameId) -> bool {
        if frame == renderer::FrameId::MAIN {
            return false;
        }
        if let Some(record) = self.lock().get_mut(&tab) {
            return record.frames.remove(frame);
        }
        true
    }

    pub(crate) fn clear_frames(&self, tab: TabId) {
        if let Some(record) = self.lock().get_mut(&tab) {
            record.frames.clear();
        }
    }

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

    fn remove(&self, id: TabId) -> Option<TabTask> {
        self.lock().remove(&id)?.task
    }

    fn drain(&self) -> Vec<TabTask> {
        self.lock()
            .drain()
            .filter_map(|(_, record)| record.task)
            .collect()
    }

    fn set_opener(&self, id: TabId, opener: TabId, frame: renderer::FrameId) {
        let mut records = self.lock();
        let record = records.entry(id).or_default();
        record.opener = Some(opener);
        record.opener_frame = Some(frame);
    }

    pub(crate) fn opener_frame(&self, id: TabId) -> Option<renderer::FrameId> {
        self.lock().get(&id).and_then(|record| record.opener_frame)
    }

    // https://html.spec.whatwg.org/multipage/nav-history-apis.html#script-closable
    fn mark_script_created(&self, id: TabId) {
        self.lock().entry(id).or_default().script_created = true;
    }

    pub(crate) fn is_script_created(&self, id: TabId) -> bool {
        self.lock()
            .get(&id)
            .is_some_and(|record| record.script_created)
    }
}

fn group_root(records: &HashMap<TabId, TabRecord>, tab: TabId) -> TabId {
    let mut root = tab;
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
            Command::ClearCache => {
                self.context.clear_cache();
                Reply::ClearCache
            }
            Command::SetCacheDisabled { disabled } => {
                self.context.set_cache_disabled(disabled);
                Reply::SetCacheDisabled
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
        // https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName
        let opener_tab = source.filter(|_| opener == renderer::OpenerPolicy::Keep);
        let handle = self.create_tab(NewTab {
            name: target.new_context_name().cloned(),
            opener: opener_tab,
            opener_frame: opener_tab.map(|_| source_frame),
            popup_sandbox,
            // https://html.spec.whatwg.org/multipage/browsers.html#determining-the-origin
            document_origin: initiator.origin.clone(),
            // https://html.spec.whatwg.org/multipage/webappapis.html#secure-contexts
            creator_secure_context: Some(initiator.secure_context),
        })?;
        let id = handle.id();
        self.tabs.mark_script_created(id);
        if let Some(opener_tab) = opener_tab {
            self.context.copy_session(opener_tab, id);
        }
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

    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName
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

    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName
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

    #[tokio::test]
    async fn a_commit_settles_the_name_unless_a_script_renamed_it() {
        let tabs = TabRegistry::new();
        let tab = TabId::new(1);
        tabs.add_fixture(tab);
        tabs.rename(tab, renderer::NameChange::Assigned(Some(name("old"))));

        let previous = tabs.install_commit(tab, None).expect("live tab");
        assert_eq!(previous.document, Some(name("old")));
        tabs.settle_document_name(tab, Some(&name("old")), None);
        assert_eq!(tabs.document_name(tab), None);

        tabs.rename(tab, renderer::NameChange::Assigned(Some(name("new"))));
        tabs.settle_document_name(tab, Some(&name("old")), None);
        assert_eq!(tabs.document_name(tab), Some(name("new")));
    }

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

        let previous = tabs.install_commit(tab, Some(&name("kept")));
        tabs.rename(tab, renderer::NameChange::Assigned(Some(name("newer"))));
        tabs.restore_commit(tab, previous, &installed);
        assert_eq!(tabs.name(tab), Some(name("newer")));
    }

    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName
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

    // https://html.spec.whatwg.org/multipage/document-sequences.html#the-rules-for-choosing-a-navigable
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

    // https://html.spec.whatwg.org/multipage/browsers.html#popup-sandboxing-flag-set
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

    // https://html.spec.whatwg.org/multipage/browsers.html#determining-the-creation-sandboxing-flags
    // https://w3c.github.io/webappsec-csp/#sandbox
    #[test]
    fn a_browser_owns_each_frames_sandbox_set() {
        let tabs = TabRegistry::new();
        let tab = TabId::new(1);
        let main = renderer::FrameId::MAIN;
        let child = renderer::FrameId::new(7);
        tabs.set_sandbox(tab, renderer::SandboxFlags::empty());

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

        let grandchild = renderer::FrameId::new(8);
        assert!(tabs.register_frame(
            tab,
            grandchild,
            restricted,
            renderer::FrameKind::Child,
            renderer::SandboxFlags::empty(),
        ));
        tabs.commit_child_frame(
            tab,
            grandchild,
            &url::Url::parse("https://child.example/").expect("url"),
            renderer::SandboxFlags::parse(Some("allow-scripts allow-popups")),
        );
        let active = tabs.frame_sandbox(tab, grandchild).expect("grandchild");
        assert!(active.blocks_navigation(), "the parent flag survives");
        assert!(active.origin_is_opaque(), "the CSP removed same-origin");

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

        tabs.reset_main_sandbox(tab, renderer::SandboxFlags::parse(Some("allow-scripts")));
        tabs.clear_frames(tab);
        assert_eq!(tabs.frame_sandbox(tab, child), None);
        assert_eq!(
            tabs.frame_sandbox(tab, main),
            Some(renderer::SandboxFlags::parse(Some("allow-scripts")))
        );
    }

    #[tokio::test]
    async fn child_frame_origin_is_committed_and_revoked_with_descendants() {
        let tabs = TabRegistry::new();
        let tab = TabId::new(42);
        tabs.add_fixture(tab);
        let main = renderer::FrameId::MAIN;
        let child = renderer::FrameId::new(2);
        let grandchild = renderer::FrameId::new(3);
        let main_site =
            crate::site::Site::for_url(&url::Url::parse("https://main.example/").expect("url"))
                .expect("site");
        let child_claim =
            renderer::SerializedOrigin::parse("https://child.example").expect("origin");

        assert!(tabs.register_frame(
            tab,
            child,
            main,
            renderer::FrameKind::Child,
            renderer::SandboxFlags::empty(),
        ));
        assert!(tabs.register_frame(
            tab,
            grandchild,
            child,
            renderer::FrameKind::Child,
            renderer::SandboxFlags::parse(Some("allow-scripts")),
        ));
        tabs.commit_child_frame(
            tab,
            grandchild,
            &url::Url::parse("https://child.example/").expect("url"),
            renderer::SandboxFlags::empty(),
        );
        assert!(
            tabs.authorize_origin(tab, &main_site, &child_claim)
                .is_none()
        );
        tabs.commit_child_frame(
            tab,
            child,
            &url::Url::parse("https://child.example/").expect("url"),
            renderer::SandboxFlags::empty(),
        );
        assert!(
            tabs.authorize_origin(tab, &main_site, &child_claim)
                .is_some()
        );
        assert!(tabs.remove_frame(tab, child));
        assert!(tabs.frame_sandbox(tab, grandchild).is_none());
        assert!(
            tabs.authorize_origin(tab, &main_site, &child_claim)
                .is_none()
        );
    }
}
