//! Renderer state shared by every frame realm of one page engine: the browsing
//! context tree, `MessagePort` endpoints, and the delivery queue between
//! realms.
//!
//! One page engine hosts every frame of its tab and each frame is its own
//! `QuickJS` realm, so no JS value can cross. This module owns what must:
//! which frame holds which port, where a serialized message goes, and the
//! order it arrives in. Serialization itself happens in the sender realm (a
//! versioned payload string); the target realm decodes it when the task runs
//! ([window post message steps](https://html.spec.whatwg.org/multipage/web-messaging.html#window-post-message-steps)).

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::dom::tree::NodeId;
use crate::protocol::{FrameId, SandboxFlags, TargetName};

/// Handle every realm of one page engine holds to its shared state.
///
/// The handle is an `Arc<Mutex<_>>`, not an `Rc<RefCell<_>>`, because a worker
/// realm may run on its own OS thread: `Shared` is plain data (the frame tree,
/// the port table, and the delivery queue), and it is the only state a worker
/// thread needs from the page engine. Every access still goes through
/// [`SharedHandle::borrow`], so call sites keep the shape the single-threaded
/// engine had, and the lock is held only for the duration of one operation.
#[derive(Clone)]
pub(crate) struct SharedHandle(Arc<Mutex<Shared>>);

impl SharedHandle {
    pub(crate) fn new(shared: Shared) -> Self {
        Self(Arc::new(Mutex::new(shared)))
    }

    /// Reads the shared state, blocking while another realm holds it.
    ///
    /// A realm that panicked while holding the lock poisons it, but the shared
    /// state is plain data with no invariant a partial update can break: every
    /// accessor mutates one map or one queue. Recovering the guard keeps the
    /// rest of the page alive instead of turning one script's panic into a
    /// browser-wide failure.
    pub(crate) fn borrow(&self) -> MutexGuard<'_, Shared> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Writes the shared state, blocking while another realm holds it.
    pub(crate) fn borrow_mut(&self) -> MutexGuard<'_, Shared> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Child frames one page engine will host; the main frame is not
/// counted. Encoding-label WPT files create one iframe per label, so this
/// has to clear the largest `dom/nodes` charset map. Realms are deferred
/// for scriptless nested documents, so an idle frame costs a `Document`
/// without a JS heap; the cap bounds documents, not realms
/// (<https://html.spec.whatwg.org/multipage/document-sequences.html#navigable>).
pub(crate) const MAX_FRAMES: usize = 256;

/// Prefix every payload carries; a payload from another build (or a future
/// worker boundary) is refused instead of misdecoded.
pub(crate) const PAYLOAD_VERSION: &str = "tb1:";

/// How a frame came to exist, which decides its parent and container.
///
/// A child navigable is owned by a navigable container element
/// (<https://html.spec.whatwg.org/multipage/document-sequences.html#child-navigable>);
/// an auxiliary browsing context is its own top-level traversable and keeps
/// the browsing context that opened it
/// (<https://html.spec.whatwg.org/multipage/document-sequences.html#auxiliary-browsing-context>).
#[derive(Clone, Copy, Debug)]
pub(crate) enum FrameOrigin {
    /// The `container` element in `parent`'s document owns this frame.
    Container { parent: FrameId, container: NodeId },
    /// `window.open` created this frame as an auxiliary top-level context.
    /// `opener_visible` is false for `noopener`/`noreferrer`, which sever the
    /// opener relationship while the context stays in the same browsing
    /// context group
    /// (<https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-open>).
    Auxiliary {
        opener: FrameId,
        opener_visible: bool,
    },
}

/// The browsing context tree of one tab.
///
/// `children` order is container tree order, which is what `window.length`
/// and the indexed getter expose: a script-inserted `iframe` placed before
/// an existing one sorts earlier, and a `moveBefore` re-sorts the moved
/// frame with its new siblings
/// (<https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-length>,
/// <https://html.spec.whatwg.org/multipage/document-sequences.html#document-tree-child-navigable>).
#[derive(Default)]
pub(crate) struct FrameTree {
    /// How each frame other than the main frame came to exist.
    origins: HashMap<FrameId, FrameOrigin>,
    /// Each frame's direct child navigables, in container tree order.
    children: HashMap<FrameId, Vec<FrameId>>,
    /// Reverse of the `Container` origins, for `contentWindow`.
    owners: HashMap<NodeId, FrameId>,
    /// Browsing context names (`window.name`)
    /// (<https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-name>).
    names: HashMap<FrameId, String>,
    /// Each top-level context's popup sandboxing flag set: the main frame's
    /// for a tab the browser hosts, an auxiliary context's otherwise. It is
    /// the opener's active set at creation, which every document the context
    /// commits inherits until the context dies
    /// (<https://html.spec.whatwg.org/multipage/browsers.html#popup-sandboxing-flag-set>).
    popup_sandbox: HashMap<FrameId, SandboxFlags>,
    /// Whether the tab's main frame is an auxiliary browsing context whose
    /// opener is non-null, which keeps its target name across a navigation to
    /// another origin
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName>).
    ///
    /// The main frame has no [`FrameOrigin`] entry, and only the browser knows
    /// how the tab was opened, so the mount supplies this.
    main_auxiliary_with_opener: bool,
}

impl FrameTree {
    /// How many frames the tree holds, so spawns can respect the cap without
    /// walking it.
    pub(crate) fn len(&self) -> usize {
        self.origins.len()
    }

    /// Adds `child` as a child navigable owned by `container`, sorting before
    /// `before` (or last when `None`) so the siblings match container tree
    /// order rather than creation order.
    pub(crate) fn add_container(
        &mut self,
        parent: FrameId,
        child: FrameId,
        container: NodeId,
        before: Option<FrameId>,
    ) {
        self.origins
            .insert(child, FrameOrigin::Container { parent, container });
        self.owners.insert(container, child);
        let siblings = self.children.entry(parent).or_default();
        match before.and_then(|target| siblings.iter().position(|frame| *frame == target)) {
            Some(index) => siblings.insert(index, child),
            None => siblings.push(child),
        }
        self.names.entry(child).or_default();
    }

    /// Adds `child` as an auxiliary top-level context opened by `opener`
    /// (<https://html.spec.whatwg.org/multipage/document-sequences.html#creating-a-new-auxiliary-browsing-context>).
    /// `opener_visible` reports whether `window.opener` links the two, and
    /// `sandbox` is the popup sandboxing flag set the context gives every
    /// document it commits
    /// (<https://html.spec.whatwg.org/multipage/browsers.html#popup-sandboxing-flag-set>).
    pub(crate) fn add_auxiliary(
        &mut self,
        opener: FrameId,
        child: FrameId,
        opener_visible: bool,
        sandbox: SandboxFlags,
    ) {
        self.origins.insert(
            child,
            FrameOrigin::Auxiliary {
                opener,
                opener_visible,
            },
        );
        self.names.entry(child).or_default();
        self.set_popup_sandbox(child, sandbox);
    }

    /// The popup sandboxing flag set of `frame`: empty for a child navigable,
    /// whose flags come from its container and embedder instead, and the
    /// opener's active set for every top-level context web content opened
    /// (<https://html.spec.whatwg.org/multipage/browsers.html#determining-the-creation-sandboxing-flags>).
    pub(crate) fn popup_sandbox(&self, frame: FrameId) -> SandboxFlags {
        self.popup_sandbox
            .get(&frame)
            .copied()
            .unwrap_or_else(SandboxFlags::empty)
    }

    /// Records the popup sandboxing flag set a top-level context imposes on
    /// every document it commits. An empty set drops the entry, so a mount
    /// that replaces a tab's flags cannot leave the old set behind.
    pub(crate) fn set_popup_sandbox(&mut self, frame: FrameId, sandbox: SandboxFlags) {
        if sandbox == SandboxFlags::empty() {
            self.popup_sandbox.remove(&frame);
        } else {
            self.popup_sandbox.insert(frame, sandbox);
        }
    }

    /// Re-sorts an existing child before `before` (or last when `None`)
    /// after its container moved. A missing child or target degrades to a
    /// no-op or an end push, never a loss.
    pub(crate) fn reposition(&mut self, child: FrameId, before: Option<FrameId>) {
        let mut parent = None;
        for (candidate, siblings) in &mut self.children {
            if let Some(position) = siblings.iter().position(|frame| *frame == child) {
                siblings.remove(position);
                parent = Some(*candidate);
                break;
            }
        }
        let Some(parent) = parent else {
            return;
        };
        let siblings = self.children.entry(parent).or_default();
        match before.and_then(|target| siblings.iter().position(|frame| *frame == target)) {
            Some(index) => siblings.insert(index, child),
            None => siblings.push(child),
        }
    }

    /// The browsing context name of `frame`.
    pub(crate) fn name(&self, frame: FrameId) -> &str {
        self.names.get(&frame).map_or("", String::as_str)
    }

    /// Sets the browsing context name of `frame`.
    pub(crate) fn set_name(&mut self, frame: FrameId, name: String) {
        self.names.insert(frame, name);
    }

    /// Records whether the tab's main frame is an auxiliary context with a
    /// live opener, which decides whether a cross-origin navigation clears the
    /// tab's name
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName>).
    pub(crate) fn set_main_auxiliary_with_opener(&mut self, auxiliary: bool) {
        self.main_auxiliary_with_opener = auxiliary;
    }

    /// Applies the target-name reset a document commit performs: a top-level
    /// context that commits a document from another origin clears its name,
    /// unless it is an auxiliary context with a live opener.
    ///
    /// A child navigable keeps its name: it has a parent, and its name is the
    /// `iframe`'s `name` attribute's, not a target name it chose
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName>).
    ///
    /// Returns whether a non-empty name went away, so the caller can tell the
    /// browser that the tab's cross-tab lookup no longer answers to it.
    pub(crate) fn reset_name_on_cross_origin_navigation(
        &mut self,
        frame: FrameId,
        same_origin: bool,
    ) -> bool {
        if same_origin || self.keeps_name_across_origins(frame) {
            return false;
        }
        self.names
            .remove(&frame)
            .is_some_and(|name| !name.is_empty())
    }

    /// Whether `frame` keeps its target name when it commits a document from
    /// another origin.
    fn keeps_name_across_origins(&self, frame: FrameId) -> bool {
        match self.origins.get(&frame) {
            Some(FrameOrigin::Container { .. }) => true,
            // The exemption is the opener browsing context being non-null, so
            // a context whose opener has since been destroyed no longer
            // qualifies
            // (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName>).
            Some(FrameOrigin::Auxiliary {
                opener,
                opener_visible,
            }) => *opener_visible && self.is_alive(*opener),
            // The main frame is a top-level context; only the browser knows
            // whether it is an auxiliary one with a live opener.
            None => self.main_auxiliary_with_opener,
        }
    }

    /// Removes one frame and returns how it came to exist.
    pub(crate) fn remove(&mut self, child: FrameId) -> Option<FrameOrigin> {
        let origin = self.origins.remove(&child)?;
        if let FrameOrigin::Container { parent, container } = origin {
            if let Some(siblings) = self.children.get_mut(&parent) {
                siblings.retain(|frame| *frame != child);
            }
            self.owners.remove(&container);
        }
        self.children.remove(&child);
        self.names.remove(&child);
        self.popup_sandbox.remove(&child);
        Some(origin)
    }

    /// Whether the tree holds `frame`.
    pub(crate) fn contains(&self, frame: FrameId) -> bool {
        self.origins.contains_key(&frame)
    }

    /// Whether `frame` is a browsing context this renderer still hosts. The
    /// main frame has no tree entry: it lives as long as the renderer does.
    pub(crate) fn is_alive(&self, frame: FrameId) -> bool {
        frame == FrameId::MAIN || self.contains(frame)
    }

    /// The parent browsing context of `frame`, when it is a child navigable.
    pub(crate) fn parent(&self, frame: FrameId) -> Option<FrameId> {
        match self.origins.get(&frame) {
            Some(FrameOrigin::Container { parent, .. }) => Some(*parent),
            Some(FrameOrigin::Auxiliary { .. }) | None => None,
        }
    }

    /// The browsing context that opened `frame`, when it is auxiliary and the
    /// opener relationship is still visible (not severed by
    /// `noopener`/`noreferrer`).
    pub(crate) fn opener(&self, frame: FrameId) -> Option<FrameId> {
        match self.origins.get(&frame) {
            Some(FrameOrigin::Auxiliary {
                opener,
                opener_visible: true,
            }) => Some(*opener),
            Some(FrameOrigin::Auxiliary { .. } | FrameOrigin::Container { .. }) | None => None,
        }
    }

    /// The browsing context that opened `frame`, whether or not the opener
    /// relationship is visible to script. The opener is still the popup's
    /// permitted sandboxed navigator.
    pub(crate) fn auxiliary_opener(&self, frame: FrameId) -> Option<FrameId> {
        match self.origins.get(&frame) {
            Some(FrameOrigin::Auxiliary { opener, .. }) => Some(*opener),
            Some(FrameOrigin::Container { .. }) | None => None,
        }
    }

    /// The direct child navigables of `frame`, in container tree order.
    pub(crate) fn children(&self, frame: FrameId) -> &[FrameId] {
        self.children.get(&frame).map_or(&[], Vec::as_slice)
    }

    /// The `iframe` that owns `frame`.
    pub(crate) fn container(&self, frame: FrameId) -> Option<NodeId> {
        match self.origins.get(&frame) {
            Some(FrameOrigin::Container { container, .. }) => Some(*container),
            Some(FrameOrigin::Auxiliary { .. }) | None => None,
        }
    }

    /// The child frame of `container`, when its browsing context exists.
    pub(crate) fn frame_for_container(&self, container: NodeId) -> Option<FrameId> {
        self.owners.get(&container).copied()
    }

    /// The navigable a `target` value chooses from `current`, per the rules for
    /// choosing a navigable
    /// (<https://html.spec.whatwg.org/multipage/document-sequences.html#the-rules-for-choosing-a-navigable>).
    ///
    /// `None` is the spec's "will not find a navigable" option, or "new": the
    /// rules asked for a new top-level traversable. `window.open` turns that
    /// answer into an auxiliary context for a URL this renderer can host and
    /// falls back to a browser tab otherwise.
    pub(crate) fn choose_navigable(
        &self,
        target: &TargetName,
        current: FrameId,
    ) -> Option<FrameId> {
        match target {
            TargetName::Current => Some(current),
            TargetName::Parent => Some(self.parent(current).unwrap_or(current)),
            TargetName::Top => Some(self.top_level(current)),
            TargetName::Blank => None,
            TargetName::Named(name) => {
                let top = self.top_level(current);
                [current, top]
                    .into_iter()
                    .find_map(|subtree| self.find_by_target_name(subtree, name.as_str()))
                    .or_else(|| self.find_by_auxiliary_name(top, name.as_str()))
            }
        }
    }

    /// The top-level traversable of `frame`
    /// (<https://html.spec.whatwg.org/multipage/document-sequences.html#nav-top>).
    pub(crate) fn top_level(&self, frame: FrameId) -> FrameId {
        let mut top = frame;
        while let Some(parent) = self.parent(top) {
            top = parent;
        }
        top
    }

    /// Whether `frame` is `ancestor` or a descendant of it. Sandbox navigation
    /// restrictions allow a sandboxed document to navigate itself and the
    /// contexts nested inside it
    /// (<https://html.spec.whatwg.org/multipage/browsers.html#sandboxed-navigation-browsing-context-flag>).
    pub(crate) fn is_inclusive_descendant(&self, ancestor: FrameId, frame: FrameId) -> bool {
        let mut current = Some(frame);
        while let Some(candidate) = current {
            if candidate == ancestor {
                return true;
            }
            current = self.parent(candidate);
        }
        false
    }

    /// "Find a navigable by target name"
    /// (<https://html.spec.whatwg.org/multipage/document-sequences.html#find-a-navigable-by-target-name>).
    ///
    /// The searched subtrees are implementation-defined, and the spec's first
    /// option is «traversable, current», searched in reverse: `choose_navigable`
    /// therefore passes this frame first, then the traversable. The browsing
    /// context group search that follows the subtrees covers the auxiliary
    /// contexts this renderer hosts; contexts in another renderer process are
    /// searched by the browser tab's host instead.
    fn find_by_target_name(&self, subtree: FrameId, target: &str) -> Option<FrameId> {
        // The inclusive descendant navigables of a document put the document's
        // own navigable first, then each container's subtree in container tree
        // order, which is the order `children` keeps.
        if self.name(subtree) == target {
            return Some(subtree);
        }
        self.children(subtree)
            .iter()
            .find_map(|child| self.find_by_target_name(*child, target))
    }

    /// The browsing context group search: every auxiliary top-level context
    /// this renderer hosts, excluding `current`.
    ///
    /// Familiarity comes from the opener chain, not from the origins: an
    /// auxiliary context and its opener share a top-level traversable by
    /// construction, and every frame this renderer hosts belongs to one tab
    /// (<https://html.spec.whatwg.org/multipage/document-sequences.html#familiar>).
    fn find_by_auxiliary_name(&self, current: FrameId, target: &str) -> Option<FrameId> {
        let mut auxiliaries: Vec<FrameId> = self
            .origins
            .iter()
            .filter_map(|(frame, origin)| {
                matches!(origin, FrameOrigin::Auxiliary { .. }).then_some(*frame)
            })
            .collect();
        auxiliaries.sort();
        auxiliaries
            .into_iter()
            .filter(|frame| *frame != current)
            .find_map(|frame| self.find_by_target_name(frame, target))
    }
}

/// One endpoint of a channel. Two endpoints entangled to each other form the
/// channel; messages queue on the receiving endpoint and run as tasks on the
/// frame that owns it.
///
/// The queue and the entanglement live here rather than in JS so a port can be
/// transferred to another realm (or, later, another thread) and still be the
/// same port ([transferable objects](https://html.spec.whatwg.org/multipage/structured-data.html#transferable-objects)).
#[derive(Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each bool mirrors a MessagePort state flag"
)]
pub(crate) struct PortEndpoint {
    /// The other endpoint, while entangled.
    peer: Option<u64>,
    /// The frame whose realm holds the port object, while it is materialized.
    owner: Option<FrameId>,
    /// Whether the port message queue is enabled (`start()` / `onmessage`).
    enabled: bool,
    /// `close()` ran, or the port's document was destroyed.
    closed: bool,
    /// Set while the endpoint is inside a transfer that has not been
    /// received yet; the object is unusable on the sending side.
    detached: bool,
    /// The frame that may receive this endpoint, recorded when it is
    /// detached for a transfer and re-aimed at delivery. Only that frame's
    /// realm may materialize the port, so a hostile page cannot adopt an
    /// endpoint by guessing its id
    /// (<https://html.spec.whatwg.org/multipage/web-messaging.html#transfer-receiving-steps>).
    recipient: Option<FrameId>,
    /// The peer disentangled while this endpoint was detached: the received
    /// port fires `close` as soon as it is materialized.
    close_pending: bool,
    /// Payloads waiting for the queue to enable.
    queue: VecDeque<QueuedPortMessage>,
}

/// One message waiting in a port's queue, with the endpoints it transfers.
#[derive(Default)]
struct QueuedPortMessage {
    payload: String,
    ports: Vec<u64>,
}

/// Every channel endpoint in the renderer process.
#[derive(Default)]
pub(crate) struct PortTable {
    next: u64,
    ports: HashMap<u64, PortEndpoint>,
}

impl PortTable {
    fn alloc(&mut self) -> u64 {
        let id = self.next;
        self.next = self.next.saturating_add(1);
        self.ports.insert(id, PortEndpoint::default());
        id
    }

    /// A fresh entangled pair owned by `owner`.
    pub(crate) fn new_pair(&mut self, owner: FrameId) -> (u64, u64) {
        let port1 = self.alloc();
        let port2 = self.alloc();
        let first = self.ports.get_mut(&port1).expect("fresh endpoint");
        first.peer = Some(port2);
        first.owner = Some(owner);
        first.recipient = Some(owner);
        let second = self.ports.get_mut(&port2).expect("fresh endpoint");
        second.peer = Some(port1);
        second.owner = Some(owner);
        second.recipient = Some(owner);
        (port1, port2)
    }

    /// Aims a detached endpoint at the frame whose realm will receive it. The
    /// engine calls this when a message carrying the endpoint is delivered
    /// (<https://html.spec.whatwg.org/multipage/web-messaging.html#transfer-receiving-steps>).
    pub(crate) fn route(&mut self, endpoint: u64, recipient: FrameId) {
        if let Some(port) = self.ports.get_mut(&endpoint)
            && port.detached
        {
            port.recipient = Some(recipient);
        }
    }

    /// Sends one payload through `from`; the receiving endpoint's queue either
    /// produces delivery tasks now or holds them until it is enabled.
    pub(crate) fn post(
        &mut self,
        from: u64,
        payload: String,
        ports: Vec<u64>,
        deliveries: &mut Vec<Delivery>,
    ) {
        let Some(port) = self.ports.get(&from) else {
            return;
        };
        if port.closed || port.detached {
            return;
        }
        let Some(peer_id) = port.peer else {
            return;
        };
        let Some(peer) = self.ports.get_mut(&peer_id) else {
            return;
        };
        if peer.closed {
            return;
        }
        peer.queue.push_back(QueuedPortMessage { payload, ports });
        if peer.enabled && !peer.detached {
            while let Some(message) = peer.queue.pop_front() {
                deliveries.push(Delivery::PortMessage {
                    endpoint: peer_id,
                    payload: message.payload,
                    ports: message.ports,
                });
            }
        }
    }

    /// Puts a delivery back on `endpoint`'s queue because its task ran in the
    /// wrong realm, or before the endpoint was materialized there.
    pub(crate) fn requeue(
        &mut self,
        endpoint: u64,
        payload: String,
        ports: Vec<u64>,
        deliveries: &mut Vec<Delivery>,
    ) {
        let Some(port) = self.ports.get_mut(&endpoint) else {
            return;
        };
        if port.closed {
            return;
        }
        // The requeued message was posted before whatever is already queued
        // while the endpoint was in transit, so it goes back at the front
        // (<https://html.spec.whatwg.org/multipage/web-messaging.html#port-message-queue>).
        port.queue.push_front(QueuedPortMessage { payload, ports });
        if port.enabled && !port.detached {
            while let Some(message) = port.queue.pop_front() {
                deliveries.push(Delivery::PortMessage {
                    endpoint,
                    payload: message.payload,
                    ports: message.ports,
                });
            }
        }
    }

    /// Re-aims a `close` event at an endpoint that moved realms.
    pub(crate) fn requeue_close(&mut self, endpoint: u64, deliveries: &mut Vec<Delivery>) {
        let Some(port) = self.ports.get_mut(&endpoint) else {
            return;
        };
        if port.closed {
            return;
        }
        if port.detached {
            // The received port fires `close` as soon as it materializes.
            port.close_pending = true;
        } else {
            deliveries.push(Delivery::PortClosed { endpoint });
        }
    }

    /// Enables `endpoint`'s queue and delivers whatever waited in it.
    pub(crate) fn start(&mut self, endpoint: u64, deliveries: &mut Vec<Delivery>) {
        let Some(port) = self.ports.get_mut(&endpoint) else {
            return;
        };
        if port.closed {
            return;
        }
        port.enabled = true;
        if port.detached {
            return;
        }
        while let Some(message) = port.queue.pop_front() {
            deliveries.push(Delivery::PortMessage {
                endpoint,
                payload: message.payload,
                ports: message.ports,
            });
        }
    }

    /// Disentangles `endpoint` and fires `close` at its peer
    /// (<https://html.spec.whatwg.org/multipage/web-messaging.html#disentangle>).
    pub(crate) fn close(&mut self, endpoint: u64, deliveries: &mut Vec<Delivery>) {
        let Some(port) = self.ports.get_mut(&endpoint) else {
            return;
        };
        if port.closed {
            return;
        }
        port.closed = true;
        port.enabled = false;
        port.queue.clear();
        if let Some(peer_id) = port.peer.take()
            && let Some(peer) = self.ports.get_mut(&peer_id)
        {
            peer.peer = None;
            if !peer.closed {
                if peer.detached {
                    // In transit: the received port fires `close` when
                    // materialized.
                    peer.close_pending = true;
                } else if peer.owner.is_some() {
                    deliveries.push(Delivery::PortClosed { endpoint: peer_id });
                }
            }
        }
        // The row is done: the JS handle keeps its own `closed` flag, and an
        // endpoint without a peer can never be addressed again.
        self.ports.remove(&endpoint);
    }

    /// Takes `endpoint` out of its realm for a transfer. The sender may only
    /// detach the port its own realm holds.
    ///
    /// Chromium (`MessagePort::DisentanglePorts`) and Firefox
    /// (`MessagePort` transfer steps) both move the port's queue with its
    /// identity and clear the enabled flag for the received port; the queue
    /// therefore lives on the endpoint, not on the realm.
    pub(crate) fn detach(&mut self, endpoint: u64, frame: FrameId) -> bool {
        let Some(port) = self.ports.get_mut(&endpoint) else {
            return false;
        };
        if port.closed || port.detached || port.owner != Some(frame) {
            return false;
        }
        port.owner = None;
        port.detached = true;
        // Until a message carries this endpoint, only the sender may
        // re-adopt it (in-realm `structuredClone`); a post re-aims it at the
        // message's target before the target realm can materialize it.
        port.recipient = Some(frame);
        // The transferred port's queue stays, but the received port starts
        // disabled again
        // (<https://html.spec.whatwg.org/multipage/web-messaging.html#transfer-receiving-steps>).
        port.enabled = false;
        true
    }

    /// Materializes a transferred `endpoint` in `frame`'s realm and reports
    /// whether the received port must fire `close` immediately. Only the
    /// frame the transfer is addressed to may receive it.
    pub(crate) fn adopt(&mut self, endpoint: u64, frame: FrameId) -> Option<bool> {
        let port = self.ports.get_mut(&endpoint)?;
        if !port.detached || port.recipient != Some(frame) {
            return None;
        }
        port.detached = false;
        port.owner = Some(frame);
        port.recipient = None;
        Some(std::mem::take(&mut port.close_pending))
    }

    /// The peer of `endpoint`, for the shim's "posted to itself" check.
    pub(crate) fn peer(&self, endpoint: u64) -> Option<u64> {
        self.ports.get(&endpoint).and_then(|port| port.peer)
    }

    /// Whether `frame` is the endpoint's recorded sender and may still hand
    /// it on in a transfer list.
    ///
    /// The post host functions are reachable from page script, so a transfer
    /// list must name only endpoints the caller detached; otherwise a hostile
    /// frame could aim another frame's in-transit port at a target it chooses
    /// (<https://html.spec.whatwg.org/multipage/web-messaging.html#transfer-receiving-steps>).
    pub(crate) fn in_transit_from(&self, endpoint: u64, frame: FrameId) -> bool {
        self.ports
            .get(&endpoint)
            .is_some_and(|port| port.detached && !port.closed && port.recipient == Some(frame))
    }

    /// Whether every endpoint in `endpoints` may be handed on by `frame`.
    pub(crate) fn all_in_transit_from(&self, endpoints: &[u64], frame: FrameId) -> bool {
        endpoints
            .iter()
            .all(|endpoint| self.in_transit_from(*endpoint, frame))
    }

    /// The frame whose realm holds `endpoint`'s port object, when it is
    /// materialized somewhere.
    pub(crate) fn owner(&self, endpoint: u64) -> Option<FrameId> {
        self.ports.get(&endpoint).and_then(|port| port.owner)
    }

    /// Closes every endpoint the frame owns, firing `close` at the peers.
    /// Called when the frame's realm (or the frame itself) goes away.
    pub(crate) fn close_frame_ports(&mut self, frame: FrameId, deliveries: &mut Vec<Delivery>) {
        let owned: Vec<u64> = self
            .ports
            .iter()
            .filter_map(|(&id, port)| (port.owner == Some(frame) && !port.closed).then_some(id))
            .collect();
        for endpoint in &owned {
            self.close(*endpoint, deliveries);
        }
    }

    /// Drops an endpoint that no realm can ever receive again (its frame was
    /// destroyed or the carrying message was dropped), disentangling its peer
    /// so the surviving port fires `close`
    /// (<https://html.spec.whatwg.org/multipage/web-messaging.html#disentangle>).
    pub(crate) fn forget(&mut self, endpoint: u64, deliveries: &mut Vec<Delivery>) {
        let Some(port) = self.ports.remove(&endpoint) else {
            return;
        };
        let Some(peer_id) = port.peer else {
            return;
        };
        let Some(peer) = self.ports.get_mut(&peer_id) else {
            return;
        };
        peer.peer = None;
        if peer.closed {
            return;
        }
        if peer.detached {
            peer.close_pending = true;
        } else if peer.owner.is_some() {
            deliveries.push(Delivery::PortClosed { endpoint: peer_id });
        }
    }
}

/// Identity of one document instance in a frame. A navigation replaces the
/// instance, so a task or delivery stamped with an older id belongs to a
/// document whose tasks are no longer runnable
/// (<https://html.spec.whatwg.org/multipage/webappapis.html#concept-task-runnable>).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DocumentId(pub(crate) u64);

/// One task the engine must run in a named frame's realm.
pub(crate) enum Delivery {
    /// A posted window message, bound to the target frame's document as of
    /// post time
    /// ([window post message steps](https://html.spec.whatwg.org/multipage/web-messaging.html#window-post-message-steps)).
    WindowMessage {
        /// The frame whose realm must decode and dispatch the message.
        target: FrameId,
        /// The target frame's document at post time: a navigation that
        /// committed before the engine routes the delivery replaced it, and
        /// the message dies with the document it was posted to.
        document: DocumentId,
        /// The frame whose realm encoded the message.
        source: FrameId,
        /// The sender's origin at post time, serialized.
        origin: String,
        /// The target origin the sender asked for.
        target_origin: crate::protocol::TargetOrigin,
        payload: String,
        /// Transferred endpoints, in transfer-list order.
        ports: Vec<u64>,
    },
    /// A message through a channel endpoint.
    PortMessage {
        endpoint: u64,
        payload: String,
        ports: Vec<u64>,
    },
    /// The peer of a closed endpoint: fire `close` at this port.
    PortClosed { endpoint: u64 },
}

/// Everything the frames of one renderer process share outside the `QuickJS`
/// heap.
pub(crate) struct Shared {
    pub(crate) tree: FrameTree,
    pub(crate) ports: PortTable,
    /// Completed sends waiting to be routed to their frame's task queue.
    pub(crate) deliveries: Vec<Delivery>,
    /// Frame ids are minted here so a frame's browsing context can be created
    /// from inside a script (an `iframe` insertion), not only from the engine.
    next_frame: u64,
    /// Document ids are minted here so a delivery can name the document it
    /// was posted to; the engine compares that identity at routing time. The
    /// map holds each live frame's current document instance.
    document_ids: HashMap<FrameId, DocumentId>,
    /// Monotonic document-instance counter backing `begin_document`.
    next_document: u64,
    /// Frames whose browsing contexts a navigation destroyed, with the
    /// `iframe` containers that owned them; the engine drops their documents
    /// at its next turn.
    removed_frames: Vec<(FrameId, Option<NodeId>)>,
}

impl Default for Shared {
    fn default() -> Self {
        Self {
            tree: FrameTree::default(),
            ports: PortTable::default(),
            deliveries: Vec::new(),
            // `FrameId::MAIN` is 0 and is not minted here.
            next_frame: 1,
            document_ids: HashMap::new(),
            next_document: 0,
            removed_frames: Vec::new(),
        }
    }
}

impl Shared {
    /// A fresh frame id. Ids are never reused, so a window proxy for a dead
    /// frame can never alias a live one.
    pub(crate) fn allocate_frame(&mut self) -> FrameId {
        let frame = FrameId::new(self.next_frame);
        self.next_frame = self.next_frame.saturating_add(1);
        frame
    }

    /// Records a new document instance for `frame` and returns its identity:
    /// a document is created and again replaced at every navigation.
    pub(crate) fn begin_document(&mut self, frame: FrameId) -> DocumentId {
        let id = DocumentId(self.next_document);
        self.next_document = self.next_document.saturating_add(1);
        self.document_ids.insert(frame, id);
        id
    }

    /// The current document instance of `frame`.
    pub(crate) fn document_id(&self, frame: FrameId) -> Option<DocumentId> {
        self.document_ids.get(&frame).copied()
    }

    /// Drops `frame`'s document identity alongside its tree entry.
    pub(crate) fn forget_document(&mut self, frame: FrameId) {
        self.document_ids.remove(&frame);
    }

    /// Takes the pending deliveries for the engine to route.
    pub(crate) fn take_deliveries(&mut self) -> Vec<Delivery> {
        std::mem::take(&mut self.deliveries)
    }

    /// Disentangles every port the frame owns; used when its realm (or the
    /// frame itself) is destroyed
    /// (<https://html.spec.whatwg.org/multipage/web-messaging.html#disentangle>).
    pub(crate) fn close_frame_ports(&mut self, frame: FrameId) {
        self.ports.close_frame_ports(frame, &mut self.deliveries);
    }

    /// Drops transferred endpoints whose message will never be delivered,
    /// firing `close` at their peers.
    pub(crate) fn forget_endpoints(&mut self, endpoints: &[u64]) {
        for endpoint in endpoints {
            self.ports.forget(*endpoint, &mut self.deliveries);
        }
    }

    /// Requeues a port delivery whose task ran in a realm the port has left.
    pub(crate) fn requeue_port_message(&mut self, endpoint: u64, payload: String, ports: Vec<u64>) {
        self.ports
            .requeue(endpoint, payload, ports, &mut self.deliveries);
    }

    /// Re-aims a `close` event at an endpoint that moved realms.
    pub(crate) fn requeue_port_close(&mut self, endpoint: u64) {
        self.ports.requeue_close(endpoint, &mut self.deliveries);
    }

    /// Detaches every descendant browsing context of `frame` (a navigation
    /// replaces that frame's document, so its children are destroyed) and
    /// closes the ports they owned.
    pub(crate) fn detach_child_frames(&mut self, frame: FrameId) {
        let mut removed = Vec::new();
        let mut pending: Vec<FrameId> = self.tree.children(frame).to_vec();
        while let Some(child) = pending.pop() {
            pending.extend(self.tree.children(child).iter().copied());
            removed.push(child);
        }
        for child in &removed {
            let container = self.tree.remove(*child).and_then(|origin| match origin {
                FrameOrigin::Container { container, .. } => Some(container),
                FrameOrigin::Auxiliary { .. } => None,
            });
            self.forget_document(*child);
            self.close_frame_ports(*child);
            self.removed_frames.push((*child, container));
        }
    }

    /// Removes one child navigable after its descendants, when the DOM removes
    /// the `iframe` that hosts it. Every removed frame is queued so the engine
    /// drops its document and tells the browser to forget its sandbox record.
    pub(crate) fn remove_child_navigable(&mut self, frame: FrameId, container: NodeId) {
        self.detach_child_frames(frame);
        self.tree.remove(frame);
        self.forget_document(frame);
        self.close_frame_ports(frame);
        self.removed_frames.push((frame, Some(container)));
    }

    /// Removes an auxiliary top-level context its own script closed, with every
    /// child navigable it owns. A child iframe or the main frame cannot close
    /// its browsing context this way.
    pub(crate) fn close_auxiliary(&mut self, frame: FrameId) {
        if self.tree.opener(frame).is_none() {
            return;
        }
        self.detach_child_frames(frame);
        self.tree.remove(frame);
        self.forget_document(frame);
        self.close_frame_ports(frame);
        self.removed_frames.push((frame, None));
    }

    /// Takes the frames whose documents the engine must drop.
    pub(crate) fn take_removed_frames(&mut self) -> Vec<(FrameId, Option<NodeId>)> {
        std::mem::take(&mut self.removed_frames)
    }
}

#[cfg(test)]
mod tests {
    use super::{FrameTree, TargetName};
    use crate::dom::tree::NodeId;
    use crate::protocol::{FrameId, SandboxFlags};

    /// The tree every case reads, as `(parent, child)` edges and the names the
    /// frames carry. Frames are `1` (top) with children `2` and `3`, `2` with
    /// child `4`, `3` with child `5`; `2` and `4` share the name `alpha`, so
    /// the search order is visible in the result.
    ///
    /// Containers are synthetic: only the shape and the names matter to
    /// choosing a navigable, and nothing here navigates.
    fn tree() -> FrameTree {
        let mut tree = FrameTree::default();
        for (parent, child) in [(1, 2), (1, 3), (2, 4), (3, 5)] {
            let (parent, child) = (FrameId::new(parent), FrameId::new(child));
            let container = NodeId {
                document: 0,
                node: crate::dom::tree::BlitzId::from_u64(child.get()),
            };
            tree.add_container(parent, child, container, None);
        }
        for (frame, name) in [(2, "alpha"), (3, "beta"), (4, "alpha"), (5, "gamma")] {
            tree.names.insert(FrameId::new(frame), name.to_owned());
        }
        tree
    }

    /// The chosen frame, or `0` for "nothing is navigable", so a failure reads
    /// as one number.
    fn chosen(target: &str, current: u64) -> u64 {
        tree()
            .choose_navigable(&TargetName::parse(target), FrameId::new(current))
            .map_or(0, FrameId::get)
    }

    #[test]
    fn keywords_choose_the_current_relative_and_top_level_frame() {
        // The empty string and `_self` are the current navigable, ASCII
        // case-insensitively.
        assert_eq!(chosen("", 4), 4);
        assert_eq!(chosen("_self", 2), 2);
        assert_eq!(chosen("_SELF", 2), 2);
        // `_parent` is the parent, and the current navigable when there is
        // none.
        assert_eq!(chosen("_parent", 4), 2);
        assert_eq!(chosen("_parent", 1), 1);
        // `_top` is the top-level traversable.
        assert_eq!(chosen("_top", 4), 1);
        assert_eq!(chosen("_top", 1), 1);
        // `_blank` asks for a top-level traversable this engine cannot create,
        // so nothing is chosen.
        assert_eq!(chosen("_blank", 1), 0);
        assert_eq!(chosen("_BLANK", 4), 0);
    }

    #[test]
    fn a_name_is_searched_in_this_frames_subtree_before_the_whole_tree() {
        // `alpha` names both `2` and `4`. A frame inside `4`'s subtree finds
        // itself first, and the top-level search finds `2` first in tree order.
        assert_eq!(chosen("alpha", 4), 4);
        assert_eq!(chosen("alpha", 1), 2);
        assert_eq!(chosen("alpha", 5), 2);
        // A name outside the current subtree is found through the traversable.
        assert_eq!(chosen("beta", 4), 3);
        assert_eq!(chosen("gamma", 1), 5);
        // A name nothing holds asks for a new top-level traversable.
        assert_eq!(chosen("missing", 1), 0);
    }

    #[test]
    fn an_auxiliary_context_is_its_own_top_level_and_is_found_by_name() {
        let mut tree = tree();
        let popup = FrameId::new(9);
        tree.add_auxiliary(FrameId::new(1), popup, true, SandboxFlags::empty());
        tree.set_name(popup, "popup".to_owned());
        // The name lookup and `_top` resolution prove the auxiliary is in the
        // browsing context group and is its own top-level traversable; the
        // accessors that only echo `add_auxiliary`'s arguments are not
        // asserted
        // (<https://html.spec.whatwg.org/multipage/document-sequences.html#the-rules-for-choosing-a-navigable>).
        assert_eq!(
            tree.choose_navigable(&TargetName::parse("popup"), FrameId::new(4)),
            Some(popup)
        );
        assert_eq!(
            tree.choose_navigable(&TargetName::parse("_top"), popup),
            Some(popup)
        );
        assert!(tree.is_inclusive_descendant(popup, popup));
        assert!(!tree.is_inclusive_descendant(FrameId::new(1), popup));
    }

    #[test]
    fn a_noopener_auxiliary_hides_its_opener_but_stays_navigable() {
        let mut tree = tree();
        let popup = FrameId::new(9);
        tree.add_auxiliary(FrameId::new(1), popup, false, SandboxFlags::empty());
        tree.set_name(popup, "popup".to_owned());
        assert_eq!(tree.opener(popup), None);
        assert_eq!(tree.auxiliary_opener(popup), Some(FrameId::new(1)));
        assert_eq!(
            tree.choose_navigable(&TargetName::parse("popup"), FrameId::new(4)),
            Some(popup)
        );
    }

    /// A top-level context clears its target name when it commits a document
    /// from another origin, and keeps it when the origins match
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName>).
    #[test]
    fn a_cross_origin_commit_clears_a_top_level_name() {
        let mut tree = tree();
        tree.set_name(FrameId::MAIN, "shared".to_owned());
        assert!(!tree.reset_name_on_cross_origin_navigation(FrameId::MAIN, true));
        assert_eq!(tree.name(FrameId::MAIN), "shared");
        assert!(tree.reset_name_on_cross_origin_navigation(FrameId::MAIN, false));
        assert_eq!(tree.name(FrameId::MAIN), "");
    }

    /// An auxiliary context with a live opener is the one top-level exception:
    /// the name survives a cross-origin commit. Without the opener, or after
    /// the opener is gone, the reset applies
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName>).
    #[test]
    fn an_auxiliary_context_keeps_its_name_only_while_its_opener_is_live() {
        let mut tree = tree();
        let with_opener = FrameId::new(9);
        let without_opener = FrameId::new(10);
        let orphan = FrameId::new(11);
        tree.add_auxiliary(FrameId::MAIN, with_opener, true, SandboxFlags::empty());
        tree.add_auxiliary(FrameId::MAIN, without_opener, false, SandboxFlags::empty());
        tree.add_auxiliary(FrameId::new(2), orphan, true, SandboxFlags::empty());
        tree.set_name(with_opener, "kept".to_owned());
        tree.set_name(without_opener, "cleared".to_owned());
        tree.set_name(orphan, "orphan".to_owned());
        assert!(!tree.reset_name_on_cross_origin_navigation(with_opener, false));
        assert_eq!(tree.name(with_opener), "kept");
        assert!(tree.reset_name_on_cross_origin_navigation(without_opener, false));
        assert_eq!(tree.name(without_opener), "");
        // The exemption is the opener browsing context being non-null, so a
        // context whose opener has since been destroyed no longer qualifies.
        tree.remove(FrameId::new(2));
        assert!(tree.reset_name_on_cross_origin_navigation(orphan, false));
        assert_eq!(tree.name(orphan), "");
    }

    /// A child navigable has a parent, so the reset never applies to it: its
    /// name is the `iframe`'s `name` attribute's
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName>).
    #[test]
    fn a_child_navigable_keeps_its_name_across_origins() {
        let mut tree = tree();
        tree.set_name(FrameId::new(2), "frame".to_owned());
        assert!(!tree.reset_name_on_cross_origin_navigation(FrameId::new(2), false));
        assert_eq!(tree.name(FrameId::new(2)), "frame");
    }

    /// The main frame is a top-level context too, and the mount decides
    /// whether the tab behind it is an auxiliary one with a live opener.
    #[test]
    fn the_main_frame_name_clears_unless_the_tab_keeps_it() {
        let mut tree = tree();
        tree.set_name(FrameId::new(1), "tab".to_owned());
        tree.set_main_auxiliary_with_opener(true);
        assert!(!tree.reset_name_on_cross_origin_navigation(FrameId::new(1), false));
        assert_eq!(tree.name(FrameId::new(1)), "tab");
        tree.set_main_auxiliary_with_opener(false);
        assert!(tree.reset_name_on_cross_origin_navigation(FrameId::new(1), false));
        assert_eq!(tree.name(FrameId::new(1)), "");
    }

    /// A name the frame does not have is not a change, so the caller never
    /// tells the browser the tab was renamed when it was not.
    #[test]
    fn clearing_an_absent_name_reports_no_change() {
        let mut tree = tree();
        assert!(!tree.reset_name_on_cross_origin_navigation(FrameId::new(1), false));
        tree.set_name(FrameId::new(1), String::new());
        assert!(!tree.reset_name_on_cross_origin_navigation(FrameId::new(1), false));
    }

    /// An auxiliary context's popup set is created with it and dies with it.
    #[test]
    fn the_popup_sandboxing_flag_set_belongs_to_the_auxiliary_context() {
        let mut tree = FrameTree::default();
        let sandbox = SandboxFlags::parse(Some("allow-scripts allow-popups"));
        tree.add_auxiliary(FrameId::MAIN, FrameId::new(9), true, sandbox);
        assert_eq!(tree.popup_sandbox(FrameId::new(9)), sandbox);
        assert_eq!(
            tree.popup_sandbox(FrameId::new(10)),
            SandboxFlags::empty(),
            "a frame that is not the context has none"
        );
        tree.remove(FrameId::new(9));
        assert_eq!(
            tree.popup_sandbox(FrameId::new(9)),
            SandboxFlags::empty(),
            "removing the context drops its set"
        );
    }

    /// A tab the browser hosts has no origin entry of its own, so its popup
    /// set comes with the mount and must be replaceable: a later mount that
    /// carries the empty set cannot leave the old flags behind
    /// (<https://html.spec.whatwg.org/multipage/browsers.html#popup-sandboxing-flag-set>).
    #[test]
    fn the_main_frames_popup_set_comes_and_goes_with_the_mount() {
        let mut tree = FrameTree::default();
        let sandbox = SandboxFlags::parse(Some("allow-scripts allow-popups"));
        tree.set_popup_sandbox(FrameId::MAIN, sandbox);
        assert_eq!(tree.popup_sandbox(FrameId::MAIN), sandbox);
        tree.set_popup_sandbox(FrameId::MAIN, SandboxFlags::empty());
        assert_eq!(tree.popup_sandbox(FrameId::MAIN), SandboxFlags::empty());
    }
}
