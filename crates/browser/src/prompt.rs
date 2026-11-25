// https://html.spec.whatwg.org/multipage/timers-and-user-prompts.html#simple-dialogs
// https://w3c.github.io/webdriver/#user-prompts
// https://w3c.github.io/webdriver-bidi/#module-browsingContext

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use renderer::{FrameId, PromptKind, PromptOutcome};
use tokio::sync::{broadcast, oneshot};

use crate::actor::TabId;

const PROMPT_EVENT_CAPACITY: usize = 256;

// https://w3c.github.io/webdriver/#dfn-user-prompt-handler
// https://w3c.github.io/webdriver-bidi/#type-session-UserPromptHandlerType
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptBehavior {
    Accept,
    Dismiss,
    AcceptAndNotify,
    DismissAndNotify,
    Ignore,
}

impl PromptBehavior {
    #[must_use]
    pub const fn notifies(self) -> bool {
        matches!(
            self,
            Self::AcceptAndNotify | Self::DismissAndNotify | Self::Ignore
        )
    }

    #[must_use]
    pub const fn as_bidi_str(self) -> &'static str {
        match self {
            Self::Accept | Self::AcceptAndNotify => "accept",
            Self::Dismiss | Self::DismissAndNotify => "dismiss",
            Self::Ignore => "ignore",
        }
    }
}

// https://w3c.github.io/webdriver-bidi/#type-session-UserPromptHandler
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PromptBehaviorMap {
    pub alert: Option<PromptBehavior>,
    pub confirm: Option<PromptBehavior>,
    pub prompt: Option<PromptBehavior>,
    pub default: Option<PromptBehavior>,
}

impl PromptBehaviorMap {
    #[must_use]
    pub const fn uniform(behavior: PromptBehavior) -> Self {
        Self {
            alert: None,
            confirm: None,
            prompt: None,
            default: Some(behavior),
        }
    }

    #[must_use]
    pub fn resolve(self, kind: PromptKind) -> Option<PromptBehavior> {
        let specific = match kind {
            PromptKind::Alert => self.alert,
            PromptKind::Confirm => self.confirm,
            PromptKind::Prompt => self.prompt,
        };
        specific.or(self.default)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActivePromptInfo {
    pub tab: TabId,
    pub frame: FrameId,
    pub kind: PromptKind,
    pub message: String,
    pub default_value: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PromptEvent {
    Opened {
        info: ActivePromptInfo,
        handler: PromptBehavior,
    },
    Closed {
        tab: TabId,
        frame: FrameId,
        kind: PromptKind,
        accepted: bool,
        user_text: Option<String>,
    },
}

struct PendingPrompt {
    info: ActivePromptInfo,
    responder: oneshot::Sender<PromptOutcome>,
}

#[derive(Default)]
struct PromptState {
    default_behavior: Option<PromptBehaviorMap>,
    hold_for_classic: bool,
    tab_behaviors: HashMap<TabId, PromptBehaviorMap>,
    active: HashMap<TabId, PendingPrompt>,
}

pub struct PromptBroker {
    state: Mutex<PromptState>,
    events: broadcast::Sender<PromptEvent>,
}

impl Default for PromptBroker {
    fn default() -> Self {
        Self::new()
    }
}

impl PromptBroker {
    #[must_use]
    pub fn new() -> Self {
        let (events, _) = broadcast::channel(PROMPT_EVENT_CAPACITY);
        Self {
            state: Mutex::new(PromptState::default()),
            events,
        }
    }

    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<PromptEvent> {
        self.events.subscribe()
    }

    pub fn set_default_behavior(&self, behavior: Option<PromptBehaviorMap>) {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .default_behavior = behavior;
    }

    pub fn set_hold_for_classic(&self, hold: bool) {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .hold_for_classic = hold;
    }

    pub fn set_tab_behavior(&self, tab: TabId, behavior: Option<PromptBehaviorMap>) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        match behavior {
            Some(map) => {
                state.tab_behaviors.insert(tab, map);
            }
            None => {
                state.tab_behaviors.remove(&tab);
            }
        }
    }

    #[must_use]
    pub fn active_prompt(&self, tab: TabId) -> Option<ActivePromptInfo> {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .active
            .get(&tab)
            .map(|pending| pending.info.clone())
    }

    pub fn handle_prompt(&self, tab: TabId, accept: bool, user_text: Option<String>) -> bool {
        let pending = self
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .active
            .remove(&tab);
        let Some(pending) = pending else {
            return false;
        };
        let outcome = if accept {
            let text = user_text
                .clone()
                .or_else(|| pending.info.default_value.clone());
            PromptOutcome::Accepted { text }
        } else {
            PromptOutcome::Dismissed
        };
        let _ = pending.responder.send(outcome);
        let _ = self.events.send(PromptEvent::Closed {
            tab: pending.info.tab,
            frame: pending.info.frame,
            kind: pending.info.kind,
            accepted: accept,
            user_text: if accept { user_text } else { None },
        });
        true
    }

    pub fn dismiss_tab(&self, tab: TabId) {
        let pending = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.tab_behaviors.remove(&tab);
            state.active.remove(&tab)
        };
        if let Some(pending) = pending {
            let _ = pending.responder.send(PromptOutcome::Dismissed);
            let _ = self.events.send(PromptEvent::Closed {
                tab: pending.info.tab,
                frame: pending.info.frame,
                kind: pending.info.kind,
                accepted: false,
                user_text: None,
            });
        }
    }

    pub async fn open_prompt(
        &self,
        tab: TabId,
        frame: FrameId,
        kind: PromptKind,
        message: String,
        default_value: Option<String>,
    ) -> PromptOutcome {
        let info = ActivePromptInfo {
            tab,
            frame,
            kind,
            message,
            default_value: default_value.clone(),
        };
        let (behavior, hold) = {
            let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            let resolved = state
                .tab_behaviors
                .get(&tab)
                .and_then(|map| map.resolve(kind))
                .or_else(|| state.default_behavior.and_then(|map| map.resolve(kind)));
            match resolved {
                Some(behavior) => (behavior, state.hold_for_classic),
                None if self.events.receiver_count() > 0 => (PromptBehavior::Ignore, false),
                None => return PromptOutcome::Dismissed,
            }
        };
        if hold || behavior == PromptBehavior::Ignore {
            let (tx, rx) = oneshot::channel();
            {
                let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
                if let Some(previous) = state.active.insert(
                    tab,
                    PendingPrompt {
                        info: info.clone(),
                        responder: tx,
                    },
                ) {
                    let _ = previous.responder.send(PromptOutcome::Dismissed);
                }
            }
            let _ = self.events.send(PromptEvent::Opened {
                info,
                handler: behavior,
            });
            return rx.await.unwrap_or(PromptOutcome::Dismissed);
        }
        match behavior {
            PromptBehavior::Accept | PromptBehavior::AcceptAndNotify => {
                if behavior.notifies() {
                    let _ = self.events.send(PromptEvent::Opened {
                        info: info.clone(),
                        handler: behavior,
                    });
                }
                let _ = self.events.send(PromptEvent::Closed {
                    tab,
                    frame,
                    kind,
                    accepted: true,
                    user_text: default_value.clone(),
                });
                PromptOutcome::Accepted {
                    text: default_value,
                }
            }
            PromptBehavior::Dismiss | PromptBehavior::DismissAndNotify => {
                if behavior.notifies() {
                    let _ = self.events.send(PromptEvent::Opened {
                        info: info.clone(),
                        handler: behavior,
                    });
                }
                let _ = self.events.send(PromptEvent::Closed {
                    tab,
                    frame,
                    kind,
                    accepted: false,
                    user_text: None,
                });
                PromptOutcome::Dismissed
            }
            PromptBehavior::Ignore => unreachable!(),
        }
    }
}
