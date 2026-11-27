// https://dom.spec.whatwg.org/#concept-node-insert
// https://dom.spec.whatwg.org/#concept-node-replace

use blitz_dom::NodeData;

use crate::documents::JournalEntry;
use crate::dom::TreeError;
use crate::dom::tree::{
    BlitzId, NodeId, attr, html_namespace, is_child_navigable_container, is_element,
    is_html_element, mathml_namespace, sibling, svg_namespace,
};

pub enum InsertedNode {
    Fragment { elements: usize, has_text: bool },
    Element,
    Text,
    Doctype,
    CharacterData,
    Other,
}

// https://dom.spec.whatwg.org/#concept-node-ensure-pre-insert-validity
pub fn classify_inserted(doc: &crate::documents::BlitzDocument, id: BlitzId) -> InsertedNode {
    // https://dom.spec.whatwg.org/#concept-node-insert
    if doc.is_fragment(id) {
        let Some(fragment) = doc.base.get_node(id) else {
            return InsertedNode::Other;
        };
        let mut elements = 0;
        let mut has_text = false;
        for child in &fragment.children {
            match doc.base.get_node(*child).map(|node| &node.data) {
                Some(NodeData::Element(_)) => elements += 1,
                Some(NodeData::Text(_) | NodeData::CDataSection { .. }) => has_text = true,
                Some(_) | None => {}
            }
        }
        return InsertedNode::Fragment { elements, has_text };
    }
    match doc.base.get_node(id).map(|node| &node.data) {
        Some(NodeData::Element(_)) => InsertedNode::Element,
        Some(NodeData::Text(_) | NodeData::CDataSection { .. }) => InsertedNode::Text,
        Some(NodeData::Doctype { .. }) => InsertedNode::Doctype,
        Some(NodeData::Comment { .. } | NodeData::ProcessingInstruction { .. }) => {
            InsertedNode::CharacterData
        }
        _ => InsertedNode::Other,
    }
}

pub fn is_doctype(doc: &crate::documents::BlitzDocument, id: BlitzId) -> bool {
    doc.base
        .get_node(id)
        .is_some_and(|node| matches!(node.data, NodeData::Doctype { .. }))
}

// https://dom.spec.whatwg.org/#concept-node-ensure-pre-insert-validity
pub fn parent_has_doctype(
    doc: &crate::documents::BlitzDocument,
    parent: BlitzId,
    excluded: &[BlitzId],
) -> bool {
    doc.base.get_node(parent).is_some_and(|root| {
        root.children
            .iter()
            .any(|child| !excluded.contains(child) && is_doctype(doc, *child))
    })
}

pub fn sibling_doctype_follows(
    doc: &crate::documents::BlitzDocument,
    parent: BlitzId,
    child: BlitzId,
) -> bool {
    let Some(node) = doc.base.get_node(parent) else {
        return false;
    };
    let Some(position) = node.children.iter().position(|kid| *kid == child) else {
        return false;
    };
    node.children
        .iter()
        .skip(position + 1)
        .any(|kid| is_doctype(doc, *kid))
}

pub fn sibling_element_precedes(
    doc: &crate::documents::BlitzDocument,
    parent: BlitzId,
    child: BlitzId,
) -> bool {
    let Some(node) = doc.base.get_node(parent) else {
        return false;
    };
    let Some(position) = node.children.iter().position(|kid| *kid == child) else {
        return false;
    };
    node.children
        .iter()
        .take(position)
        .any(|kid| is_element(&doc.base, *kid))
}

// https://dom.spec.whatwg.org/#concept-node-ensure-pre-insert-validity
pub fn parent_has_element(
    doc: &crate::documents::BlitzDocument,
    parent: BlitzId,
    excluded: &[BlitzId],
) -> bool {
    doc.base.get_node(parent).is_some_and(|root| {
        root.children
            .iter()
            .any(|child| !excluded.contains(child) && is_element(&doc.base, *child))
    })
}

pub fn siblings_around(
    base: &blitz_dom::BaseDocument,
    document: u32,
    parent: BlitzId,
    child: BlitzId,
) -> (Option<NodeId>, Option<NodeId>) {
    let mut previous = None;
    let mut next = None;
    if let Some(node) = base.get_node(parent) {
        let mut seen = false;
        for &kid in &node.children {
            if kid == child {
                seen = true;
                continue;
            }
            let id = NodeId {
                document,
                node: kid,
            };
            if seen && next.is_none() {
                next = Some(id);
                break;
            }
            if !seen {
                previous = Some(id);
            }
        }
    }
    (previous, next)
}

#[must_use = "destroy these child navigables after releasing the tree borrow"]
pub fn iframe_containers_in_subtrees(parsed: &crate::Parsed, roots: &[NodeId]) -> Vec<NodeId> {
    let mut containers = Vec::new();
    for root in roots {
        let mut stack = vec![root.node];
        while let Some(id) = stack.pop() {
            if is_child_navigable_container(&parsed.document.base, id) {
                containers.push(NodeId {
                    document: root.document,
                    node: id,
                });
            }
            if let Some(node) = parsed.document.base.get_node(id) {
                stack.extend(node.children.iter().rev().copied());
            }
        }
    }
    containers
}

// https://dom.spec.whatwg.org/#concept-node-remove
pub fn unlink_journaled(parsed: &mut crate::Parsed, target: NodeId) {
    let document = target.document;
    let (parent, previous, next) = {
        let base = &parsed.document.base;
        let parent = base.get_node(target.node).and_then(|node| node.parent);
        let (previous, next) = parent.map_or((None, None), |parent| {
            siblings_around(base, document, parent, target.node)
        });
        (parent, previous, next)
    };
    let index = if parsed.document.side.ranges.is_empty() {
        None
    } else {
        parent.and_then(|parent| {
            parsed
                .document
                .base
                .get_node(parent)
                .and_then(|node| {
                    node.children
                        .iter()
                        .position(|sibling| *sibling == target.node)
                })
                .and_then(|index| u32::try_from(index).ok())
                .map(|index| (parent, index))
        })
    };
    let Some(parent) = parent else {
        return;
    };
    if let Some(select) = crate::dom::forms::option_select_owner(&parsed.document.base, target.node)
    {
        parsed.document.invalidate_selectedness(select);
    }
    parsed.document.base.mutate().remove_node(target.node);
    if let Some((parent, index)) = index {
        let base = &parsed.document.base;
        let ranges = &mut parsed.document.side.ranges;
        crate::dom::range::adjust_for_remove(
            base,
            ranges,
            NodeId {
                document,
                node: parent,
            },
            index,
            target,
        );
    }
    parsed.document.record(JournalEntry::ChildList {
        target: NodeId {
            document,
            node: parent,
        },
        added: Vec::new(),
        removed: vec![target],
        previous,
        next,
    });
}

// https://dom.spec.whatwg.org/#concept-node-insert
pub fn adjust_children_leaving(parsed: &mut crate::Parsed, fragment: NodeId, children: &[NodeId]) {
    if children.is_empty() || parsed.document.side.ranges.is_empty() {
        return;
    }
    let base = &parsed.document.base;
    let ranges = &mut parsed.document.side.ranges;
    for child in children {
        crate::dom::range::adjust_for_remove(base, ranges, fragment, 0, *child);
    }
}

pub fn place_journaled(
    parsed: &mut crate::Parsed,
    parent: NodeId,
    child: NodeId,
    reference: Option<NodeId>,
) {
    place_inner(parsed, parent, child, reference);
    // https://dom.spec.whatwg.org/#concept-node-insert
    if reference.is_none() || parsed.document.side.ranges.is_empty() {
        return;
    }
    let index = parsed
        .document
        .base
        .get_node(parent.node)
        .and_then(|node| {
            node.children
                .iter()
                .position(|sibling| *sibling == child.node)
        })
        .and_then(|index| u32::try_from(index).ok());
    if let Some(index) = index {
        let ranges = &mut parsed.document.side.ranges;
        crate::dom::range::adjust_for_insert(ranges, parent, index, 1);
    }
}

fn place_inner(
    parsed: &mut crate::Parsed,
    parent: NodeId,
    child: NodeId,
    reference: Option<NodeId>,
) {
    let previous = match reference {
        Some(reference) => {
            siblings_around(
                &parsed.document.base,
                parent.document,
                parent.node,
                reference.node,
            )
            .0
        }
        None => parsed
            .document
            .base
            .get_node(parent.node)
            .and_then(|node| node.children.last().copied())
            .map(|node| NodeId {
                document: parent.document,
                node,
            }),
    };
    match reference {
        Some(reference) => parsed
            .document
            .base
            .mutate()
            .insert_nodes_before(reference.node, &[child.node]),
        None => parsed
            .document
            .base
            .mutate()
            .append_children(parent.node, &[child.node]),
    }
    parsed.document.record(JournalEntry::ChildList {
        target: parent,
        added: vec![child],
        removed: Vec::new(),
        previous,
        next: reference,
    });
}

// https://dom.spec.whatwg.org/#concept-node-pre-insert
pub fn insert_parsed(
    parsed: &mut crate::Parsed,
    parent: NodeId,
    node: NodeId,
    mut reference: Option<NodeId>,
) {
    if parsed.document.is_fragment(node.node) {
        splice_parsed(parsed, parent, node, reference);
        return;
    }
    if reference == Some(node) {
        reference = sibling(&parsed.document.base, node.node, true).map(|next| NodeId {
            document: node.document,
            node: next,
        });
    }
    unlink_journaled(parsed, node);
    place_journaled(parsed, parent, node, reference);
}

// https://dom.spec.whatwg.org/#concept-node-insert
pub fn splice_parsed(
    parsed: &mut crate::Parsed,
    parent: NodeId,
    fragment: NodeId,
    reference: Option<NodeId>,
) {
    let moved: Vec<NodeId> = parsed
        .document
        .base
        .get_node(fragment.node)
        .map_or_default(|node| {
            node.children
                .iter()
                .map(|child| NodeId {
                    document: fragment.document,
                    node: *child,
                })
                .collect()
        });
    if moved.is_empty() {
        return;
    }
    // https://dom.spec.whatwg.org/#concept-node-insert
    adjust_children_leaving(parsed, fragment, &moved);
    parsed.document.record(JournalEntry::ChildList {
        target: fragment,
        added: Vec::new(),
        removed: moved.clone(),
        previous: None,
        next: None,
    });
    // https://dom.spec.whatwg.org/#concept-node-insert
    let reference_index = reference.and_then(|reference| {
        parsed
            .document
            .base
            .get_node(parent.node)
            .and_then(|node| {
                node.children
                    .iter()
                    .position(|sibling| *sibling == reference.node)
            })
            .and_then(|index| u32::try_from(index).ok())
    });
    let previous = match reference {
        Some(reference) => {
            siblings_around(
                &parsed.document.base,
                parent.document,
                parent.node,
                reference.node,
            )
            .0
        }
        None => parsed
            .document
            .base
            .get_node(parent.node)
            .and_then(|node| node.children.last().copied())
            .map(|node| NodeId {
                document: parent.document,
                node,
            }),
    };
    for child in &moved {
        match reference {
            Some(reference) => parsed
                .document
                .base
                .mutate()
                .insert_nodes_before(reference.node, &[child.node]),
            None => parsed
                .document
                .base
                .mutate()
                .append_children(parent.node, &[child.node]),
        }
    }
    if let Some(index) = reference_index {
        let count = u32::try_from(moved.len()).unwrap_or(u32::MAX);
        let ranges = &mut parsed.document.side.ranges;
        crate::dom::range::adjust_for_insert(ranges, parent, index, count);
    }
    parsed.document.record(JournalEntry::ChildList {
        target: parent,
        added: moved,
        removed: Vec::new(),
        previous,
        next: reference,
    });
}

// https://dom.spec.whatwg.org/#concept-node-replace-all
#[must_use = "destroy these child navigables after releasing the tree borrow"]
pub fn replace_all_with_node(
    parsed: &mut crate::Parsed,
    parent: NodeId,
    node: NodeId,
) -> Vec<NodeId> {
    let added = if parsed.document.is_fragment(node.node) {
        let moved = fragment_children(&parsed.document, node);
        // https://dom.spec.whatwg.org/#concept-node-insert
        adjust_children_leaving(parsed, node, &moved);
        for child in &moved {
            parsed.document.base.mutate().remove_node(child.node);
        }
        parsed.document.record(JournalEntry::ChildList {
            target: node,
            added: Vec::new(),
            removed: moved.clone(),
            previous: None,
            next: None,
        });
        moved
    } else {
        vec![node]
    };
    replace_all_journaled(parsed, parent, added)
}

// https://dom.spec.whatwg.org/#concept-node-replace-all
#[must_use = "destroy these child navigables after releasing the tree borrow"]
pub fn replace_all_journaled(
    parsed: &mut crate::Parsed,
    parent: NodeId,
    added: Vec<NodeId>,
) -> Vec<NodeId> {
    let removed: Vec<NodeId> = parsed
        .document
        .base
        .get_node(parent.node)
        .map_or_default(|node| {
            node.children
                .iter()
                .map(|child| NodeId {
                    document: parent.document,
                    node: *child,
                })
                .collect()
        });
    let iframes = iframe_containers_in_subtrees(parsed, &removed);
    // https://dom.spec.whatwg.org/#concept-node-replace-all
    if !removed.is_empty() && !parsed.document.side.ranges.is_empty() {
        let base = &parsed.document.base;
        let ranges = &mut parsed.document.side.ranges;
        crate::dom::range::adjust_for_replace_all(base, ranges, parent);
    }
    for kid in &removed {
        parsed.document.base.mutate().remove_node(kid.node);
    }
    for child in &added {
        // https://dom.spec.whatwg.org/#concept-node-adopt
        unlink_journaled(parsed, *child);
        parsed
            .document
            .base
            .mutate()
            .append_children(parent.node, &[child.node]);
    }
    if !added.is_empty() || !removed.is_empty() {
        parsed.document.record(JournalEntry::ChildList {
            target: parent,
            added,
            removed,
            previous: None,
            next: None,
        });
    }
    iframes
}

pub fn collect_nonce_elements(base: &blitz_dom::BaseDocument, id: NodeId, out: &mut Vec<NodeId>) {
    if let Some(element) = base
        .get_node(id.node)
        .and_then(|node| node.data.downcast_element())
    {
        let html = element.name.ns == html_namespace();
        let svg = element.name.ns == svg_namespace();
        let mathml = element.name.ns == mathml_namespace();
        if (html || svg || mathml)
            && attr(base, id.node, "nonce").is_some_and(|value| !value.is_empty())
        {
            out.push(id);
        }
    }
    let children: Vec<BlitzId> = base
        .get_node(id.node)
        .map_or_default(|node| node.children.iter().copied().collect());
    for child in children {
        collect_nonce_elements(
            base,
            NodeId {
                document: id.document,
                node: child,
            },
            out,
        );
    }
}

pub enum InsertionStep {
    Iframe(NodeId),
    Script(NodeId),
    Meta(NodeId),
    Base(NodeId),
}

pub fn collect_html_insertion_steps(
    base: &blitz_dom::BaseDocument,
    id: NodeId,
    out: &mut Vec<InsertionStep>,
) {
    if is_html_element(base, id.node, "script") {
        out.push(InsertionStep::Script(id));
    } else if is_child_navigable_container(base, id.node) {
        out.push(InsertionStep::Iframe(id));
    } else if is_html_element(base, id.node, "meta") {
        out.push(InsertionStep::Meta(id));
    } else if is_html_element(base, id.node, "base") {
        out.push(InsertionStep::Base(id));
    }
    let children: Vec<BlitzId> = base
        .get_node(id.node)
        .map_or_default(|node| node.children.iter().copied().collect());
    for child in children {
        collect_html_insertion_steps(
            base,
            NodeId {
                document: id.document,
                node: child,
            },
            out,
        );
    }
}

// https://dom.spec.whatwg.org/#concept-node-ensure-pre-insert-validity
pub fn ensure_parented(
    base: &blitz_dom::BaseDocument,
    parent: NodeId,
    reference: NodeId,
) -> Result<(), TreeError> {
    let parented = reference.document == parent.document
        && base
            .get_node(parent.node)
            .is_some_and(|candidate| candidate.children.contains(&reference.node));
    if parented {
        Ok(())
    } else {
        Err(TreeError::NotFound)
    }
}

// https://dom.spec.whatwg.org/#concept-node-ensure-pre-insert-validity
pub fn ensure_no_cycle(
    base: &blitz_dom::BaseDocument,
    parent: BlitzId,
    node: BlitzId,
) -> Result<(), TreeError> {
    // https://dom.spec.whatwg.org/#concept-tree-host-including-inclusive-ancestor
    let mut cursor = Some(parent);
    while let Some(current) = cursor {
        if current == node {
            return Err(TreeError::Hierarchy);
        }
        cursor = base.get_node(current).and_then(|candidate| {
            candidate.parent.or(match &candidate.data {
                NodeData::Fragment { host } => *host,
                NodeData::ShadowRoot { host, .. } => Some(*host),
                _ => None,
            })
        });
    }
    Ok(())
}

// https://dom.spec.whatwg.org/#concept-node-ensure-pre-insertion-validity
pub fn ensure_document_content_model(
    doc: &crate::documents::BlitzDocument,
    parent: NodeId,
    reference: Option<NodeId>,
    excluded: &[BlitzId],
    inserted: &InsertedNode,
) -> Result<(), TreeError> {
    match inserted {
        InsertedNode::Text | InsertedNode::Other => {
            return Err(TreeError::Hierarchy);
        }
        InsertedNode::CharacterData => return Ok(()),
        InsertedNode::Fragment { elements, has_text } => {
            if *elements > 1 || *has_text {
                return Err(TreeError::Hierarchy);
            }
            if *elements == 0 {
                return Ok(());
            }
        }
        InsertedNode::Element => {}
        InsertedNode::Doctype => {
            return ensure_doctype_position(doc, parent, reference, excluded);
        }
    }
    let has_element = parent_has_element(doc, parent.node, excluded);
    let child_id = reference.filter(|child| child.document == parent.document);
    let doctype_follows =
        child_id.is_some_and(|child| sibling_doctype_follows(doc, parent.node, child.node));
    let child_is_doctype = child_id
        .is_some_and(|child| !excluded.contains(&child.node) && is_doctype(doc, child.node));
    if has_element || doctype_follows || child_is_doctype {
        return Err(TreeError::Hierarchy);
    }
    Ok(())
}

// https://dom.spec.whatwg.org/#concept-node-ensure-pre-insert-validity
pub fn ensure_doctype_position(
    doc: &crate::documents::BlitzDocument,
    parent: NodeId,
    reference: Option<NodeId>,
    excluded: &[BlitzId],
) -> Result<(), TreeError> {
    let has_doctype = parent_has_doctype(doc, parent.node, excluded);
    let child_id = reference.filter(|child| child.document == parent.document);
    let element_precedes =
        child_id.is_some_and(|child| sibling_element_precedes(doc, parent.node, child.node));
    let has_element = parent_has_element(doc, parent.node, excluded);
    if has_doctype || element_precedes || (child_id.is_none() && has_element) {
        return Err(TreeError::Hierarchy);
    }
    Ok(())
}

// https://dom.spec.whatwg.org/#dom-parentnode-movebefore
pub enum MoveTarget {
    End,
    Before(NodeId),
    Unparented,
}

// https://dom.spec.whatwg.org/#move
pub fn ensure_can_move(
    doc: &crate::documents::BlitzDocument,
    parent: NodeId,
    node: NodeId,
    target: &MoveTarget,
) -> Result<(), TreeError> {
    if node.document != parent.document {
        return Err(TreeError::Hierarchy);
    }
    let base = &doc.base;
    // https://dom.spec.whatwg.org/#move
    if crate::dom::tree::shadow_including_root(base, parent.node)
        != crate::dom::tree::shadow_including_root(base, node.node)
    {
        return Err(TreeError::Hierarchy);
    }
    // https://dom.spec.whatwg.org/#concept-tree-host-including-inclusive-ancestor
    ensure_no_cycle(base, parent.node, node.node)?;
    match target {
        MoveTarget::Before(child) => {
            if child.document != parent.document {
                return Err(TreeError::NotFound);
            }
            ensure_parented(base, parent, *child)?;
        }
        MoveTarget::Unparented => {
            return Err(TreeError::NotFound);
        }
        MoveTarget::End => {}
    }
    let kind = classify_inserted(doc, node.node);
    let movable = matches!(
        kind,
        InsertedNode::Element | InsertedNode::Text | InsertedNode::CharacterData
    );
    if !movable {
        return Err(TreeError::Hierarchy);
    }
    let parent_is_document = base.root_node().id == parent.node;
    if parent_is_document && matches!(kind, InsertedNode::Text) {
        return Err(TreeError::Hierarchy);
    }
    if parent_is_document && matches!(kind, InsertedNode::Element) {
        // https://dom.spec.whatwg.org/#concept-node-ensure-pre-insert-validity
        let has_element = base.get_node(parent.node).is_some_and(|root| {
            root.children
                .iter()
                .any(|child| *child != node.node && is_element(&doc.base, *child))
        });
        if has_element {
            return Err(TreeError::Hierarchy);
        }
    }
    // https://dom.spec.whatwg.org/#move
    if base
        .get_node(node.node)
        .and_then(|candidate| candidate.parent)
        .is_none()
    {
        return Err(TreeError::Hierarchy);
    }
    Ok(())
}

// https://dom.spec.whatwg.org/#concept-node-move
pub fn move_parsed(parsed: &mut crate::Parsed, parent: NodeId, node: NodeId, target: &MoveTarget) {
    let reference = match target {
        MoveTarget::Before(child) => Some(*child),
        MoveTarget::End | MoveTarget::Unparented => None,
    };
    let document = node.document;
    let (old_parent, previous, next, index) = {
        let base = &parsed.document.base;
        let old_parent = base
            .get_node(node.node)
            .and_then(|candidate| candidate.parent);
        let (previous, next) = old_parent.map_or((None, None), |old_parent| {
            siblings_around(base, document, old_parent, node.node)
        });
        let index = if parsed.document.side.ranges.is_empty() {
            None
        } else {
            old_parent.and_then(|old_parent| {
                base.get_node(old_parent)
                    .and_then(|candidate| {
                        candidate
                            .children
                            .iter()
                            .position(|sibling| *sibling == node.node)
                    })
                    .and_then(|index| u32::try_from(index).ok())
                    .map(|index| (old_parent, index))
            })
        };
        (old_parent, previous, next, index)
    };
    parsed
        .document
        .base
        .mutate()
        .move_node(node.node, parent.node, reference.map(|id| id.node));
    if let Some((old_parent, index)) = index {
        let base = &parsed.document.base;
        let ranges = &mut parsed.document.side.ranges;
        crate::dom::range::adjust_for_remove(
            base,
            ranges,
            NodeId {
                document,
                node: old_parent,
            },
            index,
            node,
        );
    }
    if let Some(old_parent) = old_parent {
        parsed.document.record(JournalEntry::ChildList {
            target: NodeId {
                document,
                node: old_parent,
            },
            added: Vec::new(),
            removed: vec![node],
            previous,
            next,
        });
    }
    let added_previous = sibling(&parsed.document.base, node.node, false).map(|previous| NodeId {
        document,
        node: previous,
    });
    parsed.document.record(JournalEntry::ChildList {
        target: parent,
        added: vec![node],
        removed: Vec::new(),
        previous: added_previous,
        next: reference,
    });
    if reference.is_some() && !parsed.document.side.ranges.is_empty() {
        let index = parsed
            .document
            .base
            .get_node(parent.node)
            .and_then(|candidate| {
                candidate
                    .children
                    .iter()
                    .position(|sibling| *sibling == node.node)
            })
            .and_then(|index| u32::try_from(index).ok());
        if let Some(index) = index {
            let ranges = &mut parsed.document.side.ranges;
            crate::dom::range::adjust_for_insert(ranges, parent, index, 1);
        }
    }
}

fn child_index(parsed: &crate::Parsed, parent: NodeId, child: Option<NodeId>) -> Option<u32> {
    let child = child?;
    parsed
        .document
        .base
        .get_node(parent.node)
        .and_then(|node| {
            node.children
                .iter()
                .position(|sibling| *sibling == child.node)
        })
        .and_then(|index| u32::try_from(index).ok())
}

// https://dom.spec.whatwg.org/#concept-node-replace
fn adjust_replace_removal(parsed: &mut crate::Parsed, parent: NodeId, old: NodeId) {
    if parsed.document.side.ranges.is_empty() {
        return;
    }
    let index = child_index(parsed, parent, Some(old));
    if let Some(index) = index {
        let base = &parsed.document.base;
        let ranges = &mut parsed.document.side.ranges;
        crate::dom::range::adjust_for_remove(base, ranges, parent, index, old);
    }
}

pub fn fragment_children(doc: &crate::documents::BlitzDocument, fragment: NodeId) -> Vec<NodeId> {
    crate::dom::tree::child_ids(&doc.base, fragment.document, fragment.node)
}

// https://dom.spec.whatwg.org/#concept-node-replace
pub enum Replacement {
    Node(NodeId),
    FragmentContents {
        fragment: NodeId,
        children: Vec<NodeId>,
    },
    Adopted(Vec<NodeId>),
}

// https://dom.spec.whatwg.org/#concept-node-replace
pub fn replace_parsed(
    parsed: &mut crate::Parsed,
    parent: NodeId,
    old: NodeId,
    replacement: Replacement,
) -> Vec<NodeId> {
    if let Replacement::Node(node) = replacement
        && node == old
    {
        let reference = sibling(&parsed.document.base, old.node, true).map(|next| NodeId {
            document: old.document,
            node: next,
        });
        unlink_journaled(parsed, node);
        place_journaled(parsed, parent, node, reference);
        return vec![node];
    }
    // https://dom.spec.whatwg.org/#concept-node-replace
    let previous = siblings_around(
        &parsed.document.base,
        parent.document,
        parent.node,
        old.node,
    )
    .0;
    let mut reference = sibling(&parsed.document.base, old.node, true).map(|next| NodeId {
        document: old.document,
        node: next,
    });
    if let Replacement::Node(node) = replacement
        && reference == Some(node)
    {
        reference = sibling(&parsed.document.base, node.node, true).map(|next| NodeId {
            document: node.document,
            node: next,
        });
    }
    let (added, leaving_fragment, unlink) = match replacement {
        Replacement::Node(node) => (vec![node], None, Some(node)),
        Replacement::FragmentContents { fragment, children } => (children, Some(fragment), None),
        Replacement::Adopted(children) => (children, None, None),
    };
    if let Some(fragment) = leaving_fragment {
        // https://dom.spec.whatwg.org/#concept-node-insert
        adjust_children_leaving(parsed, fragment, &added);
        parsed.document.record(JournalEntry::ChildList {
            target: fragment,
            added: Vec::new(),
            removed: added.clone(),
            previous: None,
            next: None,
        });
    } else if let Some(node) = unlink {
        unlink_journaled(parsed, node);
    }
    adjust_replace_removal(parsed, parent, old);
    parsed.document.base.mutate().remove_node(old.node);
    let reference_index = child_index(parsed, parent, reference);
    for child in &added {
        match reference {
            Some(reference) => parsed
                .document
                .base
                .mutate()
                .insert_nodes_before(reference.node, &[child.node]),
            None => parsed
                .document
                .base
                .mutate()
                .append_children(parent.node, &[child.node]),
        }
    }
    if let Some(index) = reference_index {
        let count = u32::try_from(added.len()).unwrap_or(u32::MAX);
        let ranges = &mut parsed.document.side.ranges;
        crate::dom::range::adjust_for_insert(ranges, parent, index, count);
    }
    parsed.document.record(JournalEntry::ChildList {
        target: parent,
        added: added.clone(),
        removed: vec![old],
        previous,
        next: reference,
    });
    added
}
