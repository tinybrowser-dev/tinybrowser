//! Tree mutation: pre-insert classification, insert, replace, and move.
//!
//! These algorithms work on `&mut Parsed` and `NodeId`s only, and they return
//! the realm-side work they cannot do themselves (child navigables to destroy,
//! insertion steps to apply) to the caller that owns the realm
//! (<https://dom.spec.whatwg.org/#concept-node-insert>,
//! <https://dom.spec.whatwg.org/#concept-node-replace>).

use blitz_dom::NodeData;

use crate::documents::JournalEntry;
use crate::dom::TreeError;
use crate::dom::tree::{
    BlitzId, NodeId, attr, html_namespace, is_child_navigable_container, is_element,
    is_html_element, mathml_namespace, sibling, svg_namespace,
};

/// The inserted node's side of ensure pre-insert validity.
pub(crate) enum InsertedNode {
    Fragment {
        elements: usize,
        has_text: bool,
    },
    Element,
    /// A `Text` node, including a CDATA section.
    Text,
    Doctype,
    /// A comment or processing instruction.
    CharacterData,
    Other,
}

/// The inserted node's side of ensure pre-insert validity: fragments
/// report their shape, elements/text/comments report their kind, anything
/// else cannot be inserted
/// (<https://dom.spec.whatwg.org/#concept-node-ensure-pre-insert-validity>).
pub(crate) fn classify_inserted(
    doc: &crate::documents::BlitzDocument,
    id: BlitzId,
) -> InsertedNode {
    // A fragment node splices its children on insertion, so the shape is its
    // children's: the DOM cannot hold a fragment inside a fragment, because
    // inserting one splices it
    // (<https://dom.spec.whatwg.org/#concept-node-insert>).
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

/// Whether `id` is a doctype node.
pub(crate) fn is_doctype(doc: &crate::documents::BlitzDocument, id: BlitzId) -> bool {
    doc.base
        .get_node(id)
        .is_some_and(|node| matches!(node.data, NodeData::Doctype { .. }))
}

/// Whether `parent` has a doctype child that `excluded` does not contain
/// (<https://dom.spec.whatwg.org/#concept-node-ensure-pre-insert-validity>).
pub(crate) fn parent_has_doctype(
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

/// Whether a doctype follows `child` among `parent`'s children.
pub(crate) fn sibling_doctype_follows(
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

/// Whether an element precedes `child` among `parent`'s children.
pub(crate) fn sibling_element_precedes(
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

/// Whether `parent` has an element child that `excluded` does not contain
/// (<https://dom.spec.whatwg.org/#concept-node-ensure-pre-insert-validity>).
pub(crate) fn parent_has_element(
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

/// Siblings around `child` within `parent`, for the mutation journal.
pub(crate) fn siblings_around(
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

/// The HTML `iframe` containers in the inclusive subtrees of `roots`, whose
/// child navigables the caller destroys after releasing the tree borrow.
#[must_use = "destroy these child navigables after releasing the tree borrow"]
pub(crate) fn iframe_containers_in_subtrees(
    parsed: &crate::Parsed,
    roots: &[NodeId],
) -> Vec<NodeId> {
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

/// Records the removal of an already-parented `target`, then detaches it. A
/// node with no parent is already detached; that is a no-op. The removal
/// keeps its own `childList` record, so a move reads as removed plus added
/// (<https://dom.spec.whatwg.org/#concept-node-remove>).
pub(crate) fn unlink_journaled(parsed: &mut crate::Parsed, target: NodeId) {
    let document = target.document;
    let (parent, previous, next) = {
        let base = &parsed.document.base;
        let parent = base.get_node(target.node).and_then(|node| node.parent);
        let (previous, next) = parent.map_or((None, None), |parent| {
            siblings_around(base, document, parent, target.node)
        });
        (parent, previous, next)
    };
    // The index scan only feeds the live-range adjustment.
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

/// Adjusts live ranges for `children` leaving `fragment` through a move into
/// a new parent. The insert algorithm removes the fragment's children first,
/// and each removal runs the pre-remove steps; removing in tree order always
/// takes the first slot
/// (<https://dom.spec.whatwg.org/#concept-node-insert>).
pub(crate) fn adjust_children_leaving(
    parsed: &mut crate::Parsed,
    fragment: NodeId,
    children: &[NodeId],
) {
    if children.is_empty() || parsed.document.side.ranges.is_empty() {
        return;
    }
    let base = &parsed.document.base;
    let ranges = &mut parsed.document.side.ranges;
    for child in children {
        crate::dom::range::adjust_for_remove(base, ranges, fragment, 0, *child);
    }
}

/// Inserts the already-adopted `child` into `parent` before `reference` (or
/// at the end for `None`), recording one `childList` addition. Blitz detaches
/// from the old parent during the insert itself, so callers run
/// `unlink_journaled` first and the removal keeps its own record.
pub(crate) fn place_journaled(
    parsed: &mut crate::Parsed,
    parent: NodeId,
    child: NodeId,
    reference: Option<NodeId>,
) {
    place_inner(parsed, parent, child, reference);
    // The index scan only feeds the live-range adjustment, and an append
    // (null reference) shifts no offsets
    // (<https://dom.spec.whatwg.org/#concept-node-insert>).
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

/// Moves one already-adopted node into `parent`, expanding a fragment into
/// its children. Inserting beside itself stays put
/// (<https://dom.spec.whatwg.org/#concept-node-pre-insert>).
pub(crate) fn insert_parsed(
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

/// Splices an adopted fragment's children into `parent` before `reference`:
/// one removal record on the fragment plus one addition record on the parent
/// (<https://dom.spec.whatwg.org/#concept-node-insert>).
pub(crate) fn splice_parsed(
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
    // The fragment's children leave it before the insert, and each removal
    // runs the live-range pre-remove steps
    // (<https://dom.spec.whatwg.org/#concept-node-insert>).
    adjust_children_leaving(parsed, fragment, &moved);
    parsed.document.record(JournalEntry::ChildList {
        target: fragment,
        added: Vec::new(),
        removed: moved.clone(),
        previous: None,
        next: None,
    });
    // The insert steps shift parent points past the reference child's index
    // by the node count; the index is the reference's before the insert, and
    // an append (null child) shifts nothing
    // (<https://dom.spec.whatwg.org/#concept-node-insert>).
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

/// Replace all with `node` within `parent`
/// (<https://dom.spec.whatwg.org/#concept-node-replace-all>).
///
/// Children of `parent` are removed with observers suppressed. Inserting a
/// fragment then records one empty childList on the fragment (the insert
/// algorithm always queues that record). The parent gets one combined
/// childList for the swap.
#[must_use = "destroy these child navigables after releasing the tree borrow"]
pub(crate) fn replace_all_with_node(
    parsed: &mut crate::Parsed,
    parent: NodeId,
    node: NodeId,
) -> Vec<NodeId> {
    let added = if parsed.document.is_fragment(node.node) {
        let moved = fragment_children(&parsed.document, node);
        // The children leave the fragment before the insert, and each
        // removal runs the live-range pre-remove steps
        // (<https://dom.spec.whatwg.org/#concept-node-insert>).
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

/// Replaces every child of `parent` with `added`: standing children detach
/// silently and one record carries the swap. Replacing with the same
/// contents is silent
/// (<https://dom.spec.whatwg.org/#concept-node-replace-all>).
#[must_use = "destroy these child navigables after releasing the tree borrow"]
pub(crate) fn replace_all_journaled(
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
    // Adjust live ranges while the tree links are intact: after `remove_node`
    // the detached children report no parent, so the descendant check would
    // miss every point inside them. Removing every child in tree order leaves
    // every affected point on `(parent, 0)`, and the following insert before
    // null shifts nothing
    // (<https://dom.spec.whatwg.org/#concept-node-replace-all>).
    if !removed.is_empty() && !parsed.document.side.ranges.is_empty() {
        let base = &parsed.document.base;
        let ranges = &mut parsed.document.side.ranges;
        crate::dom::range::adjust_for_replace_all(base, ranges, parent);
    }
    for kid in &removed {
        parsed.document.base.mutate().remove_node(kid.node);
    }
    for child in &added {
        // Insert adopts: a node with a parent is removed first, and that
        // removal is not suppressed
        // (<https://dom.spec.whatwg.org/#concept-node-adopt>).
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

/// The HTML, SVG, and `MathML` elements under `id` whose `nonce` content
/// attribute clears on browsing-context connection.
pub(crate) fn collect_nonce_elements(
    base: &blitz_dom::BaseDocument,
    id: NodeId,
    out: &mut Vec<NodeId>,
) {
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

pub(crate) enum InsertionStep {
    Iframe(NodeId),
    Script(NodeId),
    Meta(NodeId),
    Base(NodeId),
}

/// The insertion steps an inserted subtree owes, in tree order: iframe
/// contexts, classic scripts, and referrer-policy updates.
pub(crate) fn collect_html_insertion_steps(
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

/// The reference child of an insertion must already be parented at the
/// parent (<https://dom.spec.whatwg.org/#concept-node-ensure-pre-insert-validity>).
pub(crate) fn ensure_parented(
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

/// A node cannot be inserted into its own subtree
/// (<https://dom.spec.whatwg.org/#concept-node-ensure-pre-insert-validity>).
pub(crate) fn ensure_no_cycle(
    base: &blitz_dom::BaseDocument,
    parent: BlitzId,
    node: BlitzId,
) -> Result<(), TreeError> {
    // Host-including inclusive ancestry: a template contents fragment counts
    // as a child of its host, and a shadow root counts as a child of its
    // host, so appending an ancestor of the host into either is a cycle
    // (<https://dom.spec.whatwg.org/#concept-tree-host-including-inclusive-ancestor>).
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

/// Document parent rules from ensure pre-insert validity, steps after the
/// non-document early return
/// (<https://dom.spec.whatwg.org/#concept-node-ensure-pre-insertion-validity>).
///
/// `excluded` is `childrenToExclude`: `replace` passes the replaced child,
/// `replaceChildren` passes the parent's current children, and pre-insert
/// paths pass nothing — a move of the document's own doctype or element
/// therefore throws, matching the spec and Chromium's "only one element on
/// document allowed".
pub(crate) fn ensure_document_content_model(
    doc: &crate::documents::BlitzDocument,
    parent: NodeId,
    reference: Option<NodeId>,
    excluded: &[BlitzId],
    inserted: &InsertedNode,
) -> Result<(), TreeError> {
    match inserted {
        // A text node, including a CDATA section, cannot be a document child.
        // `Other` is unreachable here: the caller rejects it for every parent
        // before this runs, and it fails closed on the same exception rather
        // than admitting documents or anonymous blocks as document children.
        InsertedNode::Text | InsertedNode::Other => {
            return Err(TreeError::Hierarchy);
        }
        // A comment or processing instruction may be a document child.
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
    // `ensure_parented` above guarantees same-document-or-absent, but the
    // filter stays: in release builds a bypassed check must degrade to
    // append-at-end, never to cross-document slot reads.
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

/// A doctype child must precede every element child of its parent
/// (<https://dom.spec.whatwg.org/#concept-node-ensure-pre-insert-validity>).
pub(crate) fn ensure_doctype_position(
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

/// The `child` argument of `move`, after `moveBefore` rewrites a self-reference
/// to the node's next sibling
/// (<https://dom.spec.whatwg.org/#dom-parentnode-movebefore>).
pub(crate) enum MoveTarget {
    /// `child` is null, so the node is appended.
    End,
    /// Insert before this child of the destination.
    Before(NodeId),
    /// An attribute, which is never parented.
    Unparented,
}

/// Preconditions of `move` (<https://dom.spec.whatwg.org/#move>).
pub(crate) fn ensure_can_move(
    doc: &crate::documents::BlitzDocument,
    parent: NodeId,
    node: NodeId,
    target: &MoveTarget,
) -> Result<(), TreeError> {
    if node.document != parent.document {
        return Err(TreeError::Hierarchy);
    }
    let base = &doc.base;
    // `move` compares shadow-including roots, so a node can cross into a
    // connected shadow tree
    // (<https://dom.spec.whatwg.org/#move>).
    if crate::dom::tree::shadow_including_root(base, parent.node)
        != crate::dom::tree::shadow_including_root(base, node.node)
    {
        return Err(TreeError::Hierarchy);
    }
    // Host-including ancestry (template contents and shadow roots via their
    // host) is checked inside `ensure_no_cycle`
    // (<https://dom.spec.whatwg.org/#concept-tree-host-including-inclusive-ancestor>).
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
        // Pre-move validity counts an element child that is not the moved
        // node: moving the document element itself past a doctype-less
        // prolog is not a second element
        // (<https://dom.spec.whatwg.org/#concept-node-ensure-pre-insert-validity>).
        let has_element = base.get_node(parent.node).is_some_and(|root| {
            root.children
                .iter()
                .any(|child| *child != node.node && is_element(&doc.base, *child))
        });
        if has_element {
            return Err(TreeError::Hierarchy);
        }
    }
    // The root and ancestor checks make this parent exist. The spec asserts
    // it (<https://dom.spec.whatwg.org/#move>).
    if base
        .get_node(node.node)
        .and_then(|candidate| candidate.parent)
        .is_none()
    {
        return Err(TreeError::Hierarchy);
    }
    Ok(())
}

/// Applies a validated `move`: reparents `node` without the insertion or
/// removing steps
/// (<https://dom.spec.whatwg.org/#concept-node-move>).
/// The caller owns validity (`ensure_can_move`).
pub(crate) fn move_parsed(
    parsed: &mut crate::Parsed,
    parent: NodeId,
    node: NodeId,
    target: &MoveTarget,
) {
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

/// The index of `child` under `parent`, or `None` when it has none.
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

/// Runs the live-range pre-remove steps for the old child of a replacement.
/// The removal itself carries `suppressObservers`, which does not skip them
/// (<https://dom.spec.whatwg.org/#concept-node-replace>).
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

/// The children of a fragment, as handles in its own document.
pub(crate) fn fragment_children(
    doc: &crate::documents::BlitzDocument,
    fragment: NodeId,
) -> Vec<NodeId> {
    crate::dom::tree::child_ids(&doc.base, fragment.document, fragment.node)
}

/// What takes a replaced child's place
/// (<https://dom.spec.whatwg.org/#concept-node-replace>).
pub(crate) enum Replacement {
    /// One node, in `parent`'s document.
    Node(NodeId),
    /// A fragment's children, still in `parent`'s document. They leave the
    /// fragment inside this operation, so the removal is recorded here.
    FragmentContents {
        fragment: NodeId,
        children: Vec<NodeId>,
    },
    /// Children already adopted into `parent`'s document from a fragment in
    /// another one. Adoption recorded each removal in the old document.
    Adopted(Vec<NodeId>),
}

/// Replaces `old` with `replacement`: a node's own unlink keeps a removal
/// record when it moves, then one combined record carries the replacement
/// (<https://dom.spec.whatwg.org/#concept-node-replace>). Returns the nodes
/// the parent gained, for the insertion fixups.
///
/// `replacement` owns every child list it needs. A fragment in another
/// document has no arena entry here, and a Blitz id from one document's arena
/// names an unrelated node in another's.
pub(crate) fn replace_parsed(
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
    // Let referenceChild be child's next sibling. If that is node, use
    // node's next sibling instead
    // (<https://dom.spec.whatwg.org/#concept-node-replace>).
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
        // The fragment's children leave it through the insert, and each
        // removal runs the live-range pre-remove steps
        // (<https://dom.spec.whatwg.org/#concept-node-insert>).
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
