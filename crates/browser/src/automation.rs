// https://w3c.github.io/webdriver-bidi/
// https://chromedevtools.github.io/devtools-protocol/tot/

use std::fmt;

use tokio::sync::broadcast;

use crate::actor::TabId;
use crate::browser::{BrowserHandle, BrowserLifecycleEvent};
use crate::intercept::{
    InterceptDecision, InterceptId, InterceptPhase, InterceptResolveError, InterceptScope,
    NetworkEvent, NetworkRequestId, UrlPattern,
};
use crate::prompt::{PromptBehaviorMap, PromptEvent};

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct AutomationSessionId(u64);

impl AutomationSessionId {
    #[must_use]
    pub(crate) const fn new(raw: u64) -> Self {
        Self(raw)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for AutomationSessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "session-{}", self.0)
    }
}

pub struct AutomationSession {
    id: AutomationSessionId,
    browser: BrowserHandle,
}

impl AutomationSession {
    pub(crate) fn new(id: AutomationSessionId, browser: BrowserHandle) -> Self {
        Self { id, browser }
    }

    #[must_use]
    pub const fn id(&self) -> AutomationSessionId {
        self.id
    }

    #[must_use]
    pub fn add_intercept(
        &self,
        phases: Vec<InterceptPhase>,
        scope: InterceptScope,
        patterns: Vec<UrlPattern>,
    ) -> InterceptId {
        self.browser
            .network_broker()
            .add_intercept(self.id, phases, scope, patterns)
    }

    #[must_use]
    pub fn remove_intercept(&self, id: InterceptId) -> bool {
        self.browser.network_broker().remove_intercept(self.id, id)
    }

    pub fn resolve_intercept(
        &self,
        request_id: NetworkRequestId,
        decision: InterceptDecision,
    ) -> Result<(), InterceptResolveError> {
        self.browser
            .network_broker()
            .resolve_intercept(self.id, request_id, decision)
    }

    pub fn set_blocked_urls(&self, tab: TabId, patterns: Vec<String>) {
        self.browser
            .network_broker()
            .set_blocked_urls(self.id, tab, patterns);
    }

    pub fn set_default_prompt_behavior(
        &self,
        behavior: Option<PromptBehaviorMap>,
        hold_for_classic: bool,
    ) {
        self.browser
            .prompt_broker()
            .set_default_behavior(self.id, behavior, hold_for_classic);
    }

    pub fn set_tab_prompt_behavior(&self, tab: TabId, behavior: Option<PromptBehaviorMap>) {
        self.browser
            .prompt_broker()
            .set_tab_behavior(self.id, tab, behavior);
    }

    pub fn clear_tab_prompt(&self, tab: TabId) {
        self.browser.prompt_broker().clear_owner_tab(self.id, tab);
    }

    pub fn clear_tab(&self, tab: TabId) {
        self.browser.network_broker().clear_owner_tab(self.id, tab);
        self.browser.prompt_broker().clear_owner_tab(self.id, tab);
    }

    pub fn clear_intercepts(&self) {
        self.browser
            .network_broker()
            .clear_owner_intercepts(self.id);
    }

    #[must_use]
    pub fn subscribe_lifecycle(&self) -> broadcast::Receiver<BrowserLifecycleEvent> {
        self.browser.subscribe()
    }

    #[must_use]
    pub fn subscribe_network(&self) -> broadcast::Receiver<NetworkEvent> {
        self.browser.network_broker().subscribe()
    }

    #[must_use]
    pub fn subscribe_prompts(&self) -> broadcast::Receiver<PromptEvent> {
        self.browser.prompt_broker().subscribe()
    }
}

impl Drop for AutomationSession {
    fn drop(&mut self) {
        self.browser.network_broker().clear_owner(self.id);
        self.browser.prompt_broker().clear_owner(self.id);
    }
}
