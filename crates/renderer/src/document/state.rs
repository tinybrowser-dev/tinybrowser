use std::collections::VecDeque;
use std::time::Duration;

use url::Url;

use super::Task;
use crate::dom::tree::NodeId;
use crate::protocol::{FrameId, PolicyContainer, ResponseHead, ResponseSource, TimerId};

// https://html.spec.whatwg.org/multipage/semantics.html#attr-meta-http-equiv-refresh
#[derive(Default)]
pub(super) enum RefreshState {
    #[default]
    Idle,
    Pending {
        target: Url,
        delay: Duration,
    },
    Armed {
        timer: TimerId,
        target: Url,
    },
    Fired,
}

impl RefreshState {
    pub(super) fn is_scheduled(&self) -> bool {
        !matches!(self, Self::Idle)
    }

    pub(super) fn schedule(&mut self, target: Url, delay: Duration) {
        if matches!(self, Self::Idle) {
            *self = Self::Pending { target, delay };
        }
    }

    pub(super) fn arm(&mut self, schedule_timer: impl FnOnce(Duration) -> TimerId) {
        let Self::Pending { target, delay } = std::mem::take(self) else {
            return;
        };
        let timer = schedule_timer(delay);
        *self = Self::Armed { timer, target };
    }

    pub(super) fn take_due(&mut self, id: TimerId) -> Option<Url> {
        let Self::Armed { timer, .. } = self else {
            return None;
        };
        if *timer != id {
            return None;
        }
        let Self::Armed { target, .. } = std::mem::replace(self, Self::Fired) else {
            return None;
        };
        Some(target)
    }

    pub(super) fn reset(&mut self) -> Option<TimerId> {
        match std::mem::take(self) {
            Self::Armed { timer, .. } => Some(timer),
            Self::Idle | Self::Pending { .. } | Self::Fired => None,
        }
    }
}

#[derive(Default)]
pub(super) struct UiEventQueue {
    scroll: Vec<blitz_dom::PendingScrollEvent>,
    scroll_queued: bool,
    animation: Vec<blitz_dom::animation_events::PendingAnimationEvent>,
    animation_queued: bool,
    toggles: Vec<(NodeId, String, String)>,
}

impl UiEventQueue {
    pub(super) fn push_scroll(&mut self, events: Vec<blitz_dom::PendingScrollEvent>) -> bool {
        if events.is_empty() {
            return false;
        }
        self.scroll.extend(events);
        if self.scroll_queued {
            false
        } else {
            self.scroll_queued = true;
            true
        }
    }

    pub(super) fn take_scroll(&mut self) -> Vec<blitz_dom::PendingScrollEvent> {
        self.scroll_queued = false;
        std::mem::take(&mut self.scroll)
    }

    pub(super) fn push_animation(
        &mut self,
        events: Vec<blitz_dom::animation_events::PendingAnimationEvent>,
    ) -> bool {
        if events.is_empty() {
            return false;
        }
        self.animation.extend(events);
        if self.animation_queued {
            false
        } else {
            self.animation_queued = true;
            true
        }
    }

    pub(super) fn take_animation(
        &mut self,
    ) -> Vec<blitz_dom::animation_events::PendingAnimationEvent> {
        self.animation_queued = false;
        std::mem::take(&mut self.animation)
    }

    pub(super) fn push_toggle(
        &mut self,
        node: NodeId,
        old_state: String,
        new_state: String,
    ) -> bool {
        if let Some(pending) = self.toggles.iter_mut().find(|(id, _, _)| *id == node) {
            pending.2 = new_state;
            return false;
        }
        self.toggles.push((node, old_state, new_state));
        true
    }

    pub(super) fn take_toggle(&mut self, node: NodeId) -> Option<(String, String)> {
        let index = self.toggles.iter().position(|(id, _, _)| *id == node)?;
        let (_, old_state, new_state) = self.toggles.remove(index);
        Some((old_state, new_state))
    }
}

#[derive(Default)]
pub(super) struct CspState {
    pub(super) pending_events: VecDeque<Task>,
    pub(super) reporting_endpoints: Vec<(String, String)>,
    pub(super) http_status: u16,
    pub(super) embedder_reports: Vec<(FrameId, Vec<crate::csp::Violation>)>,
}

impl CspState {
    pub(super) fn set_reporting_endpoints(&mut self, endpoints: Option<&str>) {
        self.reporting_endpoints =
            endpoints.map_or_default(crate::csp::parse_reporting_endpoints_value);
    }

    pub(super) fn clear_pending_events(&mut self) {
        self.pending_events.clear();
    }
}

pub(super) struct ResponseStage {
    pub(super) character_set: &'static str,
    pub(super) content_type: Option<String>,
    pub(super) policy: Option<PolicyContainer>,
    pub(super) creator_secure_context: Option<bool>,
    pub(super) creator_cross_origin_isolated: Option<bool>,
    pub(super) inherited_policy: Option<PolicyContainer>,
    pub(super) sniff_header: Vec<u8>,
    pub(super) apply_mime_sniff: bool,
    pub(super) mime_guard: crate::sniff::MimeGuard,
}

impl Default for ResponseStage {
    fn default() -> Self {
        Self {
            character_set: "UTF-8",
            content_type: None,
            policy: None,
            creator_secure_context: None,
            creator_cross_origin_isolated: None,
            inherited_policy: None,
            sniff_header: Vec::new(),
            apply_mime_sniff: false,
            mime_guard: crate::sniff::MimeGuard::default(),
        }
    }
}

impl ResponseStage {
    pub(super) fn reset_for_parse(&mut self) {
        self.content_type = None;
        self.policy = None;
        self.sniff_header.clear();
        self.apply_mime_sniff = false;
        self.mime_guard = crate::sniff::MimeGuard::default();
        self.character_set = "UTF-8";
    }

    pub(super) fn stage_head(&mut self, head: &ResponseHead, top_level: bool) {
        let policy = head.source.policy();
        self.content_type.clone_from(&head.content_type);
        let creator = match &head.source {
            ResponseSource::Http(_) => self.creator_secure_context,
            ResponseSource::Local { inherited } => inherited
                .as_ref()
                .map(|p| p.secure_context)
                .or(self.creator_secure_context),
        };
        let secure_context = secure_context::is_secure_environment(&head.url, creator);
        let creator_isolated = match &head.source {
            ResponseSource::Http(_) => self.creator_cross_origin_isolated,
            ResponseSource::Local { inherited } => inherited
                .as_ref()
                .map(|p| p.cross_origin_isolated)
                .or(self.creator_cross_origin_isolated),
        };
        let cross_origin_isolated = match &head.source {
            ResponseSource::Local { .. } => policy.cross_origin_isolated,
            ResponseSource::Http(_) => {
                secure_context
                    && policy.cross_origin_embedder_policy.compatible()
                    && (if top_level {
                        policy.cross_origin_opener_policy
                            == crate::protocol::CrossOriginOpenerPolicy::SameOrigin
                    } else {
                        creator_isolated.unwrap_or(false)
                    })
            }
        };
        self.policy = Some(PolicyContainer {
            csp: policy.csp.clone(),
            sandbox: policy.sandbox,
            referrer_policy: policy.referrer_policy,
            secure_context,
            cross_origin_opener_policy: policy.cross_origin_opener_policy,
            cross_origin_embedder_policy: policy.cross_origin_embedder_policy,
            cross_origin_isolated,
        });
        self.apply_mime_sniff = true;
        self.mime_guard = crate::sniff::MimeGuard {
            no_sniff: head.source.no_sniff(),
            http: head.source.is_http(),
        };
    }

    pub(super) fn push_sniff_bytes(&mut self, bytes: &[u8]) {
        if self.sniff_header.len() < crate::sniff::RESOURCE_HEADER_LEN {
            let room = crate::sniff::RESOURCE_HEADER_LEN - self.sniff_header.len();
            self.sniff_header.extend(bytes.iter().copied().take(room));
        }
    }

    pub(super) fn sniff_content_type(&mut self) {
        if !self.apply_mime_sniff {
            return;
        }
        let raw = self.content_type.as_deref();
        let supplied = raw.and_then(crate::sniff::extract_mime_type);
        let apache = self.mime_guard.http && raw.is_some_and(crate::sniff::apache_bug_content_type);
        self.content_type = Some(crate::sniff::sniff_mime(
            &self.sniff_header,
            supplied.as_deref(),
            self.mime_guard.no_sniff,
            apache,
        ));
    }
}
