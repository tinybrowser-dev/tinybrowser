// https://html.spec.whatwg.org/multipage/web-messaging.html#window-post-message-steps

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::dom::tree::NodeId;
use crate::protocol::{FrameId, SandboxFlags, TargetName};

#[derive(Clone)]
pub(crate) struct SharedHandle(Arc<Mutex<Shared>>);

impl SharedHandle {
    pub(crate) fn new(shared: Shared) -> Self {
        Self(Arc::new(Mutex::new(shared)))
    }

    pub(crate) fn borrow(&self) -> MutexGuard<'_, Shared> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn borrow_mut(&self) -> MutexGuard<'_, Shared> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    // https://fetch.spec.whatwg.org/#keepalive-flag
    pub(crate) fn begin_keepalive_fetch(&self) {
        self.borrow_mut().keepalive_fetches += 1;
    }

    pub(crate) fn end_keepalive_fetch(&self) {
        let mut shared = self.borrow_mut();
        shared.keepalive_fetches = shared.keepalive_fetches.saturating_sub(1);
    }

    pub(crate) fn has_keepalive_fetches(&self) -> bool {
        self.borrow().keepalive_fetches > 0
    }
}

// https://html.spec.whatwg.org/multipage/document-sequences.html#navigable
pub(crate) const MAX_FRAMES: usize = 256;

pub(crate) const PAYLOAD_VERSION: &str = "tb1:";

// https://html.spec.whatwg.org/multipage/document-sequences.html#child-navigable
// https://html.spec.whatwg.org/multipage/document-sequences.html#auxiliary-browsing-context
#[derive(Clone, Copy, Debug)]
pub(crate) enum FrameOrigin {
    Container {
        parent: FrameId,
        container: NodeId,
    },
    // https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-open
    Auxiliary {
        opener: FrameId,
        opener_visible: bool,
    },
}

// https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-length
// https://html.spec.whatwg.org/multipage/document-sequences.html#document-tree-child-navigable
#[derive(Default)]
pub(crate) struct FrameTree {
    origins: HashMap<FrameId, FrameOrigin>,
    children: HashMap<FrameId, Vec<FrameId>>,
    owners: HashMap<NodeId, FrameId>,
    // https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-name
    names: HashMap<FrameId, String>,
    // https://html.spec.whatwg.org/multipage/browsers.html#popup-sandboxing-flag-set
    popup_sandbox: HashMap<FrameId, SandboxFlags>,
    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName
    main_auxiliary_with_opener: bool,
}

impl FrameTree {
    pub(crate) fn len(&self) -> usize {
        self.origins.len()
    }

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

    // https://html.spec.whatwg.org/multipage/document-sequences.html#creating-a-new-auxiliary-browsing-context
    // https://html.spec.whatwg.org/multipage/browsers.html#popup-sandboxing-flag-set
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

    // https://html.spec.whatwg.org/multipage/browsers.html#determining-the-creation-sandboxing-flags
    pub(crate) fn popup_sandbox(&self, frame: FrameId) -> SandboxFlags {
        self.popup_sandbox
            .get(&frame)
            .copied()
            .unwrap_or_else(SandboxFlags::empty)
    }

    pub(crate) fn set_popup_sandbox(&mut self, frame: FrameId, sandbox: SandboxFlags) {
        if sandbox == SandboxFlags::empty() {
            self.popup_sandbox.remove(&frame);
        } else {
            self.popup_sandbox.insert(frame, sandbox);
        }
    }

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

    pub(crate) fn name(&self, frame: FrameId) -> &str {
        self.names.get(&frame).map_or("", String::as_str)
    }

    pub(crate) fn set_name(&mut self, frame: FrameId, name: String) {
        self.names.insert(frame, name);
    }

    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName
    pub(crate) fn set_main_auxiliary_with_opener(&mut self, auxiliary: bool) {
        self.main_auxiliary_with_opener = auxiliary;
    }

    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName
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

    fn keeps_name_across_origins(&self, frame: FrameId) -> bool {
        match self.origins.get(&frame) {
            Some(FrameOrigin::Container { .. }) => true,
            // https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName
            Some(FrameOrigin::Auxiliary {
                opener,
                opener_visible,
            }) => *opener_visible && self.is_alive(*opener),
            None => self.main_auxiliary_with_opener,
        }
    }

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

    pub(crate) fn contains(&self, frame: FrameId) -> bool {
        self.origins.contains_key(&frame)
    }

    pub(crate) fn is_alive(&self, frame: FrameId) -> bool {
        frame == FrameId::MAIN || self.contains(frame)
    }

    pub(crate) fn parent(&self, frame: FrameId) -> Option<FrameId> {
        match self.origins.get(&frame) {
            Some(FrameOrigin::Container { parent, .. }) => Some(*parent),
            Some(FrameOrigin::Auxiliary { .. }) | None => None,
        }
    }

    pub(crate) fn opener(&self, frame: FrameId) -> Option<FrameId> {
        match self.origins.get(&frame) {
            Some(FrameOrigin::Auxiliary {
                opener,
                opener_visible: true,
            }) => Some(*opener),
            Some(FrameOrigin::Auxiliary { .. } | FrameOrigin::Container { .. }) | None => None,
        }
    }

    pub(crate) fn auxiliary_opener(&self, frame: FrameId) -> Option<FrameId> {
        match self.origins.get(&frame) {
            Some(FrameOrigin::Auxiliary { opener, .. }) => Some(*opener),
            Some(FrameOrigin::Container { .. }) | None => None,
        }
    }

    pub(crate) fn children(&self, frame: FrameId) -> &[FrameId] {
        self.children.get(&frame).map_or(&[], Vec::as_slice)
    }

    pub(crate) fn container(&self, frame: FrameId) -> Option<NodeId> {
        match self.origins.get(&frame) {
            Some(FrameOrigin::Container { container, .. }) => Some(*container),
            Some(FrameOrigin::Auxiliary { .. }) | None => None,
        }
    }

    pub(crate) fn frame_for_container(&self, container: NodeId) -> Option<FrameId> {
        self.owners.get(&container).copied()
    }

    // https://html.spec.whatwg.org/multipage/document-sequences.html#the-rules-for-choosing-a-navigable
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

    // https://html.spec.whatwg.org/multipage/document-sequences.html#nav-top
    pub(crate) fn top_level(&self, frame: FrameId) -> FrameId {
        let mut top = frame;
        while let Some(parent) = self.parent(top) {
            top = parent;
        }
        top
    }

    // https://html.spec.whatwg.org/multipage/browsers.html#sandboxed-navigation-browsing-context-flag
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

    // https://html.spec.whatwg.org/multipage/document-sequences.html#find-a-navigable-by-target-name
    fn find_by_target_name(&self, subtree: FrameId, target: &str) -> Option<FrameId> {
        if self.name(subtree) == target {
            return Some(subtree);
        }
        self.children(subtree)
            .iter()
            .find_map(|child| self.find_by_target_name(*child, target))
    }

    // https://html.spec.whatwg.org/multipage/document-sequences.html#familiar
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

// https://html.spec.whatwg.org/multipage/structured-data.html#transferable-objects
#[derive(Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each bool mirrors a MessagePort state flag"
)]
pub(crate) struct PortEndpoint {
    peer: Option<u64>,
    owner: Option<FrameId>,
    enabled: bool,
    closed: bool,
    detached: bool,
    // https://html.spec.whatwg.org/multipage/web-messaging.html#transfer-receiving-steps
    recipient: Option<FrameId>,
    close_pending: bool,
    queue: VecDeque<QueuedPortMessage>,
}

#[derive(Default)]
struct QueuedPortMessage {
    payload: String,
    ports: Vec<u64>,
}

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

    // https://html.spec.whatwg.org/multipage/web-messaging.html#transfer-receiving-steps
    pub(crate) fn route(&mut self, endpoint: u64, recipient: FrameId) {
        if let Some(port) = self.ports.get_mut(&endpoint)
            && port.detached
        {
            port.recipient = Some(recipient);
        }
    }

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
        // https://html.spec.whatwg.org/multipage/web-messaging.html#port-message-queue
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

    pub(crate) fn requeue_close(&mut self, endpoint: u64, deliveries: &mut Vec<Delivery>) {
        let Some(port) = self.ports.get_mut(&endpoint) else {
            return;
        };
        if port.closed {
            return;
        }
        if port.detached {
            port.close_pending = true;
        } else {
            deliveries.push(Delivery::PortClosed { endpoint });
        }
    }

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

    // https://html.spec.whatwg.org/multipage/web-messaging.html#disentangle
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
                    peer.close_pending = true;
                } else if peer.owner.is_some() {
                    deliveries.push(Delivery::PortClosed { endpoint: peer_id });
                }
            }
        }
        self.ports.remove(&endpoint);
    }

    pub(crate) fn detach(&mut self, endpoint: u64, frame: FrameId) -> bool {
        let Some(port) = self.ports.get_mut(&endpoint) else {
            return false;
        };
        if port.closed || port.detached || port.owner != Some(frame) {
            return false;
        }
        port.owner = None;
        port.detached = true;
        port.recipient = Some(frame);
        // https://html.spec.whatwg.org/multipage/web-messaging.html#transfer-receiving-steps
        port.enabled = false;
        true
    }

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

    pub(crate) fn peer(&self, endpoint: u64) -> Option<u64> {
        self.ports.get(&endpoint).and_then(|port| port.peer)
    }

    // https://html.spec.whatwg.org/multipage/web-messaging.html#transfer-receiving-steps
    pub(crate) fn in_transit_from(&self, endpoint: u64, frame: FrameId) -> bool {
        self.ports
            .get(&endpoint)
            .is_some_and(|port| port.detached && !port.closed && port.recipient == Some(frame))
    }

    pub(crate) fn all_in_transit_from(&self, endpoints: &[u64], frame: FrameId) -> bool {
        endpoints
            .iter()
            .all(|endpoint| self.in_transit_from(*endpoint, frame))
    }

    pub(crate) fn owner(&self, endpoint: u64) -> Option<FrameId> {
        self.ports.get(&endpoint).and_then(|port| port.owner)
    }

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

    // https://html.spec.whatwg.org/multipage/web-messaging.html#disentangle
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

// https://html.spec.whatwg.org/multipage/webappapis.html#concept-task-runnable
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DocumentId(pub(crate) u64);

pub(crate) enum Delivery {
    // https://html.spec.whatwg.org/multipage/web-messaging.html#window-post-message-steps
    WindowMessage {
        target: FrameId,
        document: DocumentId,
        source: FrameId,
        origin: String,
        target_origin: crate::protocol::TargetOrigin,
        payload: String,
        ports: Vec<u64>,
    },
    PortMessage {
        endpoint: u64,
        payload: String,
        ports: Vec<u64>,
    },
    PortClosed {
        endpoint: u64,
    },
}

pub(crate) struct Shared {
    pub(crate) tree: FrameTree,
    pub(crate) ports: PortTable,
    pub(crate) deliveries: Vec<Delivery>,
    keepalive_fetches: usize,
    next_frame: u64,
    document_ids: HashMap<FrameId, DocumentId>,
    next_document: u64,
    removed_frames: Vec<(FrameId, Option<NodeId>)>,
}

impl Default for Shared {
    fn default() -> Self {
        Self {
            tree: FrameTree::default(),
            ports: PortTable::default(),
            deliveries: Vec::new(),
            keepalive_fetches: 0,
            next_frame: 1,
            document_ids: HashMap::new(),
            next_document: 0,
            removed_frames: Vec::new(),
        }
    }
}

impl Shared {
    pub(crate) fn allocate_frame(&mut self) -> FrameId {
        let frame = FrameId::new(self.next_frame);
        self.next_frame = self.next_frame.saturating_add(1);
        frame
    }

    pub(crate) fn begin_document(&mut self, frame: FrameId) -> DocumentId {
        let id = DocumentId(self.next_document);
        self.next_document = self.next_document.saturating_add(1);
        self.document_ids.insert(frame, id);
        id
    }

    pub(crate) fn document_id(&self, frame: FrameId) -> Option<DocumentId> {
        self.document_ids.get(&frame).copied()
    }

    pub(crate) fn forget_document(&mut self, frame: FrameId) {
        self.document_ids.remove(&frame);
    }

    pub(crate) fn take_deliveries(&mut self) -> Vec<Delivery> {
        std::mem::take(&mut self.deliveries)
    }

    // https://html.spec.whatwg.org/multipage/web-messaging.html#disentangle
    pub(crate) fn close_frame_ports(&mut self, frame: FrameId) {
        self.ports.close_frame_ports(frame, &mut self.deliveries);
    }

    pub(crate) fn forget_endpoints(&mut self, endpoints: &[u64]) {
        for endpoint in endpoints {
            self.ports.forget(*endpoint, &mut self.deliveries);
        }
    }

    pub(crate) fn requeue_port_message(&mut self, endpoint: u64, payload: String, ports: Vec<u64>) {
        self.ports
            .requeue(endpoint, payload, ports, &mut self.deliveries);
    }

    pub(crate) fn requeue_port_close(&mut self, endpoint: u64) {
        self.ports.requeue_close(endpoint, &mut self.deliveries);
    }

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

    pub(crate) fn remove_child_navigable(&mut self, frame: FrameId, container: NodeId) {
        self.detach_child_frames(frame);
        self.tree.remove(frame);
        self.forget_document(frame);
        self.close_frame_ports(frame);
        self.removed_frames.push((frame, Some(container)));
    }

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

    pub(crate) fn take_removed_frames(&mut self) -> Vec<(FrameId, Option<NodeId>)> {
        std::mem::take(&mut self.removed_frames)
    }
}

#[cfg(test)]
mod tests {
    use super::{FrameTree, TargetName};
    use crate::dom::tree::NodeId;
    use crate::protocol::{FrameId, SandboxFlags};

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

    fn chosen(target: &str, current: u64) -> u64 {
        tree()
            .choose_navigable(&TargetName::parse(target), FrameId::new(current))
            .map_or(0, FrameId::get)
    }

    #[test]
    fn keywords_choose_the_current_relative_and_top_level_frame() {
        assert_eq!(chosen("", 4), 4);
        assert_eq!(chosen("_self", 2), 2);
        assert_eq!(chosen("_SELF", 2), 2);
        assert_eq!(chosen("_parent", 4), 2);
        assert_eq!(chosen("_parent", 1), 1);
        assert_eq!(chosen("_top", 4), 1);
        assert_eq!(chosen("_top", 1), 1);
        assert_eq!(chosen("_blank", 1), 0);
        assert_eq!(chosen("_BLANK", 4), 0);
    }

    #[test]
    fn a_name_is_searched_in_this_frames_subtree_before_the_whole_tree() {
        assert_eq!(chosen("alpha", 4), 4);
        assert_eq!(chosen("alpha", 1), 2);
        assert_eq!(chosen("alpha", 5), 2);
        assert_eq!(chosen("beta", 4), 3);
        assert_eq!(chosen("gamma", 1), 5);
        assert_eq!(chosen("missing", 1), 0);
    }

    #[test]
    fn an_auxiliary_context_is_its_own_top_level_and_is_found_by_name() {
        let mut tree = tree();
        let popup = FrameId::new(9);
        tree.add_auxiliary(FrameId::new(1), popup, true, SandboxFlags::empty());
        tree.set_name(popup, "popup".to_owned());
        // https://html.spec.whatwg.org/multipage/document-sequences.html#the-rules-for-choosing-a-navigable
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

    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName
    #[test]
    fn a_cross_origin_commit_clears_a_top_level_name() {
        let mut tree = tree();
        tree.set_name(FrameId::MAIN, "shared".to_owned());
        assert!(!tree.reset_name_on_cross_origin_navigation(FrameId::MAIN, true));
        assert_eq!(tree.name(FrameId::MAIN), "shared");
        assert!(tree.reset_name_on_cross_origin_navigation(FrameId::MAIN, false));
        assert_eq!(tree.name(FrameId::MAIN), "");
    }

    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName
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
        tree.remove(FrameId::new(2));
        assert!(tree.reset_name_on_cross_origin_navigation(orphan, false));
        assert_eq!(tree.name(orphan), "");
    }

    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#resetBCName
    #[test]
    fn a_child_navigable_keeps_its_name_across_origins() {
        let mut tree = tree();
        tree.set_name(FrameId::new(2), "frame".to_owned());
        assert!(!tree.reset_name_on_cross_origin_navigation(FrameId::new(2), false));
        assert_eq!(tree.name(FrameId::new(2)), "frame");
    }

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

    #[test]
    fn clearing_an_absent_name_reports_no_change() {
        let mut tree = tree();
        assert!(!tree.reset_name_on_cross_origin_navigation(FrameId::new(1), false));
        tree.set_name(FrameId::new(1), String::new());
        assert!(!tree.reset_name_on_cross_origin_navigation(FrameId::new(1), false));
    }

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

    // https://html.spec.whatwg.org/multipage/browsers.html#popup-sandboxing-flag-set
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
