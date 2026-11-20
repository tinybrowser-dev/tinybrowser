//! Cohesive state owners extracted from [`super::Document`].
//!
//! Each collaborator owns one domain's fields and invariants so navigation
//! resets and state transitions cannot leave stale flags behind.

use std::collections::VecDeque;
use std::time::Duration;

use url::Url;

use super::Task;
use crate::dom::tree::NodeId;
use crate::protocol::{FrameId, PolicyContainer, ResponseHead, ResponseSource, TimerId};

/// State machine for a document's one declarative refresh (`Refresh` header or
/// `<meta http-equiv=refresh>`).
///
/// The first valid refresh wins; later ones are ignored
/// (<https://html.spec.whatwg.org/multipage/semantics.html#attr-meta-http-equiv-refresh>).
#[derive(Default)]
pub(super) enum RefreshState {
    /// No declarative refresh has been scheduled for this document.
    #[default]
    Idle,
    /// Parsed and waiting for the document's `load` event before starting the
    /// delay clock.
    Pending { target: Url, delay: Duration },
    /// Clock running on `timer`; navigates to `target` when `timer` fires.
    Armed { timer: TimerId, target: Url },
    /// The refresh has fired; later directives are still ignored.
    Fired,
}

impl RefreshState {
    /// Whether a refresh has already been accepted for this document.
    pub(super) fn is_scheduled(&self) -> bool {
        !matches!(self, Self::Idle)
    }

    /// Records the first parsed refresh target and delay.
    pub(super) fn schedule(&mut self, target: Url, delay: Duration) {
        if matches!(self, Self::Idle) {
            *self = Self::Pending { target, delay };
        }
    }

    /// Arms a pending refresh using `schedule_timer` once the document is
    /// completely loaded.
    pub(super) fn arm(&mut self, schedule_timer: impl FnOnce(Duration) -> TimerId) {
        let Self::Pending { target, delay } = std::mem::take(self) else {
            return;
        };
        let timer = schedule_timer(delay);
        *self = Self::Armed { timer, target };
    }

    /// Consumes the armed refresh target if `id` is its timer.
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

    /// Resets for a new navigation and returns the armed timer to cancel, if
    /// any.
    pub(super) fn reset(&mut self) -> Option<TimerId> {
        match std::mem::take(self) {
            Self::Armed { timer, .. } => Some(timer),
            Self::Idle | Self::Pending { .. } | Self::Fired => None,
        }
    }
}

/// Coalesced UI-event batches waiting for their task-queue turn: scroll steps,
/// CSS animation/transition lifecycle events, and `details`/`dialog` toggles.
#[derive(Default)]
pub(super) struct UiEventQueue {
    scroll: Vec<blitz_dom::PendingScrollEvent>,
    scroll_queued: bool,
    animation: Vec<blitz_dom::animation_events::PendingAnimationEvent>,
    animation_queued: bool,
    toggles: Vec<(NodeId, String, String)>,
}

impl UiEventQueue {
    /// Appends scroll events and returns `true` when the caller should enqueue
    /// `Task::ScrollEvents`.
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

    /// Drains the queued scroll batch when `Task::ScrollEvents` runs.
    pub(super) fn take_scroll(&mut self) -> Vec<blitz_dom::PendingScrollEvent> {
        self.scroll_queued = false;
        std::mem::take(&mut self.scroll)
    }

    /// Appends animation/transition events and returns `true` when the caller
    /// should enqueue `Task::AnimationEvents`.
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

    /// Drains the queued animation batch when `Task::AnimationEvents` runs.
    pub(super) fn take_animation(
        &mut self,
    ) -> Vec<blitz_dom::animation_events::PendingAnimationEvent> {
        self.animation_queued = false;
        std::mem::take(&mut self.animation)
    }

    /// Coalesces a `toggle` event for `node` and returns `true` when the
    /// caller should enqueue `Task::ToggleEvent(node)`.
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

    /// Takes the coalesced `(old_state, new_state)` for `node`, if pending.
    pub(super) fn take_toggle(&mut self, node: NodeId) -> Option<(String, String)> {
        let index = self.toggles.iter().position(|(id, _, _)| *id == node)?;
        let (_, old_state, new_state) = self.toggles.remove(index);
        Some((old_state, new_state))
    }
}

/// CSP reporting state for one document: violation tasks buffered while the
/// parser is active, `Reporting-Endpoints`, committed HTTP status, and
/// embedder-directed `frame-ancestors` violations.
#[derive(Default)]
pub(super) struct CspState {
    pub(super) pending_events: VecDeque<Task>,
    pub(super) reporting_endpoints: Vec<(String, String)>,
    pub(super) http_status: u16,
    pub(super) embedder_reports: Vec<(FrameId, Vec<crate::csp::Violation>)>,
}

impl CspState {
    /// Installs parsed `Reporting-Endpoints` from a response header.
    pub(super) fn set_reporting_endpoints(&mut self, endpoints: Option<&str>) {
        self.reporting_endpoints =
            endpoints.map_or_default(crate::csp::parse_reporting_endpoints_value);
    }

    /// Clears parser-deferred violation events on navigation reset.
    pub(super) fn clear_pending_events(&mut self) {
        self.pending_events.clear();
    }
}

/// Response-head and policy-container state staged between `install_head` and
/// `finish_parse` / `install_parsed`.
pub(super) struct ResponseStage {
    pub(super) character_set: &'static str,
    pub(super) content_type: Option<String>,
    /// Policy container derived from the navigation response headers, or
    /// `None` for script-driven parses (`document.open()`).
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
    /// Clears per-parse fields when a new parser starts, leaving creator bits
    /// and `inherited_policy` untouched.
    pub(super) fn reset_for_parse(&mut self) {
        self.content_type = None;
        self.policy = None;
        self.sniff_header.clear();
        self.apply_mime_sniff = false;
        self.mime_guard = crate::sniff::MimeGuard::default();
        self.character_set = "UTF-8";
    }

    /// Stages the response's MIME, sniffing guard, and computed
    /// [`PolicyContainer`] from `head`.
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

    /// Captures up to `RESOURCE_HEADER_LEN` raw response bytes for MIME
    /// sniffing.
    pub(super) fn push_sniff_bytes(&mut self, bytes: &[u8]) {
        if self.sniff_header.len() < crate::sniff::RESOURCE_HEADER_LEN {
            let room = crate::sniff::RESOURCE_HEADER_LEN - self.sniff_header.len();
            self.sniff_header.extend(bytes.iter().copied().take(room));
        }
    }

    /// Runs the MIME sniffing algorithm on the staged `content_type` when
    /// `apply_mime_sniff` is set.
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
