//! Node adoption, cloning, and document import.

use super::{throw_dom, throw_dom_error, world, world_for_node, wrap_new_document};

use crate::dom::adoption::{adopt_across_store, import_snapshot, materialize_import};
use crate::dom::tree::NodeId;

use rquickjs::{Ctx, Exception, Result, Value};

/// Same-document insert returns `node`. A node from another document adopts
/// into the parent's document: the subtree snapshots, the original detaches,
/// and an equivalent subtree materializes in the target
/// ([adopt](https://dom.spec.whatwg.org/#concept-node-adopt)).
///
/// Blitz ids are per-tree, so the copy is a different id. Wrappers this realm
/// already holds follow that copy, including descendants, so
/// `parent.appendChild(node)` leaves `node.parentNode === parent`.
pub(crate) fn adopt_across_documents(
    ctx: &Ctx<'_>,
    parent: NodeId,
    node: NodeId,
) -> Result<NodeId> {
    if node.document == parent.document {
        return Ok(node);
    }
    // https://dom.spec.whatwg.org/#concept-node-adopt
    // Blitz ids are per-tree slot+version, so the target arena cannot alias
    // the source slots: adoption snapshots the source subtree and
    // materializes an equivalent detached subtree in the target instead.
    // `deep_clone_node` remains the path for same-document deep clones (see
    // `clone_within_document` in `dom`).
    let store = world(ctx)?.borrow().runtime.documents.clone();
    let fresh = adopt_across_store(&mut store.borrow_mut(), parent, node)
        .map_err(|err| throw_dom_error(ctx, err))?;
    retarget_adopted_subtree(ctx, node, fresh)?;
    Ok(fresh)
}

/// [Adopts](https://dom.spec.whatwg.org/#dom-document-adoptnode) `node` into
/// `document`. A document throws; a same-document node is only removed from
/// its parent.
pub(crate) fn adopt_into_document(ctx: &Ctx<'_>, document: NodeId, node: NodeId) -> Result<NodeId> {
    let store = world(ctx)?.borrow().runtime.documents.clone();
    let is_document = store
        .borrow()
        .get(node.document)
        .is_some_and(|parsed| parsed.document.base.root_node().id == node.node);
    if is_document {
        return Err(throw_dom(
            ctx,
            "NotSupportedError",
            "cannot adopt a document",
        ));
    }
    if super::node_is_shadow_root(ctx, node)? {
        // https://dom.spec.whatwg.org/#dom-document-adoptnode
        return Err(throw_dom(
            ctx,
            "HierarchyRequestError",
            "cannot adopt a shadow root",
        ));
    }
    if node.document == document.document {
        store
            .borrow_mut()
            .get_mut(node.document)
            .ok_or_else(|| Exception::throw_type(ctx, "no document"))
            .and_then(|parsed| {
                crate::dom::adoption::detach_for_adopt(&mut parsed.document, node)
                    .map_err(|err| throw_dom_error(ctx, err))
            })?;
        return Ok(node);
    }
    adopt_across_documents(ctx, document, node)
}

/// Points wrappers for `from` and its descendants at the copied subtree `to`.
///
/// The two trees match child for child because `to` was materialized from a
/// deep snapshot of `from`. Each lookup borrows one document: the store is a
/// single `RefCell`.
fn retarget_adopted_subtree(ctx: &Ctx<'_>, from: NodeId, to: NodeId) -> Result<()> {
    // Iterative to bound the Rust stack on adversarial deep trees. On a
    // child-count mismatch (snapshot filtering can drop unsnapshotable
    // nodes) that subtree is skipped, not thrown: throwing here would leave
    // the already-detached source and materialized copy half-retargeted
    // with no rollback, while `zip` would misalign every following sibling.
    let mut stack = vec![(from, to)];
    let mut adopted = Vec::new();
    while let Some((from, to)) = stack.pop() {
        adopted.push((from, to));
        super::retarget_wrapper(ctx, from, to)?;
        // The cryptographic nonce slot follows the adopted node into its
        // new identity; content attributes travel with the materialized
        // copy already.
        if let (Ok(from_owner), Ok(to_owner)) = (world_for_node(ctx, from), world_for_node(ctx, to))
        {
            if std::rc::Rc::ptr_eq(&from_owner, &to_owner) {
                let mut world = from_owner.borrow_mut();
                if let Some(slot) = world.state.nonce_slots.remove(&from) {
                    world.state.nonce_slots.insert(to, slot);
                }
                if let Some(control) = world.state.controls.remove(&from) {
                    world.state.controls.insert(to, control);
                    // The exposed value is re-materialized from the adopted
                    // snapshot, so an edit buffer from the old document no
                    // longer describes it. Custom validity and the output
                    // defaults are not edit state and travel with the node
                    // (<https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#the-constraint-validation-api>).
                    if let Some(control) = world.state.controls.get_mut(&to) {
                        control.clear_edit_position();
                    }
                }
            } else {
                let slot = from_owner.borrow().state.nonce_slots.get(&from).cloned();
                if let Some(slot) = slot {
                    from_owner.borrow_mut().state.nonce_slots.remove(&from);
                    to_owner.borrow_mut().state.nonce_slots.insert(to, slot);
                }
                let control = from_owner.borrow().state.controls.get(&from).cloned();
                if let Some(control) = control {
                    from_owner.borrow_mut().state.controls.remove(&from);
                    let mut target = to_owner.borrow_mut();
                    target.state.controls.insert(to, control);
                    // The exposed value is re-materialized from the adopted
                    // snapshot, so an edit buffer from the old document no
                    // longer describes it. Custom validity and the output
                    // defaults are not edit state and travel with the node
                    // (<https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#the-constraint-validation-api>).
                    if let Some(control) = target.state.controls.get_mut(&to) {
                        control.clear_edit_position();
                    }
                }
            }
        }
        // A template's contents fragment follows its host: retarget it so a
        // previously fetched `content` wrapper keeps pointing at live content.
        let contents = |id: NodeId| {
            world_for_node(ctx, id).ok().and_then(|owner| {
                let owner = owner.borrow();
                owner.document(id).and_then(|parsed| {
                    crate::dom::tree::template_contents(&parsed.document.base, id.node).map(
                        |fragment| NodeId {
                            document: id.document,
                            node: fragment,
                        },
                    )
                })
            })
        };
        if let Some(from_contents) = contents(from)
            && let Some(to_contents) = contents(to)
        {
            super::retarget_wrapper(ctx, from_contents, to_contents)?;
        }
        let from_children = adopted_children(ctx, from)?;
        let to_children = adopted_children(ctx, to)?;
        if from_children.len() == to_children.len() {
            stack.extend(from_children.into_iter().zip(to_children));
        }
    }
    // Element references keep naming the same element across adoption, which
    // means following its new tree id and filing the entry under the
    // document that now owns the element
    // (<https://html.spec.whatwg.org/multipage/common-dom-interfaces.html#reflecting-content-attributes-in-idl-attributes:element>).
    if let Ok(owner) = world(ctx) {
        let store = owner.borrow().runtime.documents.clone();
        if let Ok(mut documents) = store.try_borrow_mut() {
            documents.retarget_element_references(&adopted);
        }
    }
    Ok(())
}

/// The live children of an adopted node for wrapper retargeting.
/// Template children live in the template contents fragment, so those are
/// retargeted instead of the (empty) element children.
fn adopted_children(ctx: &Ctx<'_>, id: NodeId) -> Result<Vec<NodeId>> {
    let owner = world_for_node(ctx, id)?;
    let owner = owner.borrow();
    let Some(parsed) = owner.document(id) else {
        return Err(Exception::throw_type(ctx, "no document"));
    };
    let Some(node) = parsed.document.base.get_node(id.node) else {
        return Ok(Vec::new());
    };
    if let Some(element) = node.data.downcast_element()
        && crate::dom::tree::is_template_tag(&element.name)
        && let Some(fragment) = element.template_contents
    {
        return Ok(crate::dom::tree::child_ids(
            &parsed.document.base,
            id.document,
            fragment,
        ));
    }
    Ok(crate::dom::tree::child_ids(
        &parsed.document.base,
        id.document,
        id.node,
    ))
}

/// Copies cryptographic nonce slots from a cloned subtree to its clone,
/// walking both trees in lockstep: cloning preserves child order, so pairs
/// align; a length mismatch skips that subtree rather than misaligning
/// siblings. Content attributes travel with the clone already; only IDL-set
/// slots need copying
/// (<https://html.spec.whatwg.org/multipage/urls-and-fetching.html#nonce-attributes>,
/// cloning steps).
pub(crate) fn copy_nonce_subtree(
    owner: &std::rc::Rc<std::cell::RefCell<crate::js::world::World>>,
    from: NodeId,
    to: NodeId,
) {
    let pairs: Vec<(NodeId, NodeId)> = {
        let world = owner.borrow();
        let (Some(from_parsed), Some(to_parsed)) = (world.document(from), world.document(to))
        else {
            return;
        };
        let mut pairs = Vec::new();
        let mut stack = vec![(from.node, to.node)];
        while let Some((from_node, to_node)) = stack.pop() {
            pairs.push((
                NodeId {
                    document: from.document,
                    node: from_node,
                },
                NodeId {
                    document: to.document,
                    node: to_node,
                },
            ));
            let from_children = from_parsed
                .document
                .base
                .get_node(from_node)
                .map_or_default(|tree| tree.children.to_vec());
            let to_children = to_parsed
                .document
                .base
                .get_node(to_node)
                .map_or_default(|tree| tree.children.to_vec());
            if from_children.len() == to_children.len() {
                stack.extend(from_children.into_iter().zip(to_children));
            }
        }
        pairs
    };
    let mut world = owner.borrow_mut();
    for (from_id, to_id) in pairs {
        if let Some(slot) = world.state.nonce_slots.get(&from_id).cloned() {
            world.state.nonce_slots.insert(to_id, slot);
        }
    }
}

/// [Clones](https://dom.spec.whatwg.org/#concept-node-clone) a document into a
/// new tree in this world. A document clone is a different document, not a
/// node in the same arena, so the document node itself is never snapshotted.
pub(crate) fn clone_document<'js>(ctx: &Ctx<'js>, id: NodeId, deep: bool) -> Result<Value<'js>> {
    let world_rc = world(ctx)?;
    let (content_type, xml_document, quirks_mode, snapshots) = {
        let world = world_rc.borrow();
        let Some(parsed) = world.document(id) else {
            return Err(Exception::throw_type(ctx, "no document"));
        };
        let snapshots = if deep {
            let root = parsed.document.base.root_node().id;
            parsed.document.base.get_node(root).map_or_default(|node| {
                node.children
                    .iter()
                    .filter_map(|kid| {
                        import_snapshot(
                            &parsed.document,
                            NodeId {
                                document: id.document,
                                node: *kid,
                            },
                            true,
                        )
                    })
                    .collect::<Vec<_>>()
            })
        } else {
            Vec::new()
        };
        (
            parsed.content_type,
            parsed.xml_document,
            parsed.quirks_mode,
            snapshots,
        )
    };
    let font_ctx = world(ctx)?.borrow().runtime.font_ctx.clone();
    let mut parsed = crate::Parsed::script(content_type, font_ctx);
    parsed.xml_document = xml_document;
    parsed.quirks_mode = quirks_mode;
    let root = parsed.document.base.root_node().id;
    for child in &snapshots {
        let child_id = materialize_import(&mut parsed.document, 0, child)
            .map_err(|err| throw_dom_error(ctx, err))?;
        parsed
            .document
            .base
            .mutate()
            .append_children(root, &[child_id.node]);
    }
    wrap_new_document(ctx, parsed)
}
