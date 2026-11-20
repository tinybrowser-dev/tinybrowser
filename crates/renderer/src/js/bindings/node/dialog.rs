// https://html.spec.whatwg.org/multipage/interactive-elements.html#the-dialog-element

use rquickjs::{Ctx, Result};

use super::element::content_attr;
use crate::dom::tree::{self, BlitzId, NodeId};
use crate::dom_string::DomString;
use crate::js::bindings::{
    focus_node, is_focusable, remove_attribute_sync, set_attribute_sync, world_for_node,
};
use crate::js::events;
use crate::js::world::EventTargetKey;

// https://html.spec.whatwg.org/multipage/interactive-elements.html#the-dialog-element:dom-dialog-returnvalue
pub(super) fn return_value(ctx: &Ctx<'_>, id: NodeId) -> Result<DomString> {
    let owner = world_for_node(ctx, id)?;
    let value = owner.borrow().state.dialog_return_values.get(&id).cloned();
    Ok(value.unwrap_or_default())
}

pub(super) fn set_return_value(ctx: &Ctx<'_>, id: NodeId, value: DomString) -> Result<()> {
    world_for_node(ctx, id)?
        .borrow_mut()
        .state
        .dialog_return_values
        .insert(id, value);
    Ok(())
}

// https://html.spec.whatwg.org/multipage/interactive-elements.html#the-dialog-element:computed-closed-by-state
pub(super) fn closed_by(ctx: &Ctx<'_>, id: NodeId) -> &'static str {
    content_attr(ctx, id, "closedby")
        .map(|value| value.to_ascii_lowercase())
        .and_then(|value| match value.as_str() {
            "any" => Some("any"),
            "closerequest" => Some("closerequest"),
            "none" => Some("none"),
            _ => None,
        })
        .unwrap_or("none")
}

// https://html.spec.whatwg.org/multipage/interactive-elements.html#the-dialog-element:dom-dialog-show
pub(super) fn show(ctx: &Ctx<'_>, id: NodeId) -> Result<()> {
    if has_open(ctx, id) {
        return Ok(());
    }
    let proceed = events::fire_toggle_event(
        ctx,
        EventTargetKey::Node(id),
        "beforetoggle",
        "closed",
        "open",
        None,
        false,
        true,
    )?;
    if !proceed || has_open(ctx, id) {
        return Ok(());
    }
    queue_toggle(ctx, id, "closed", "open")?;
    capture_previously_focused(ctx, id)?;
    set_attribute_sync(ctx, id, "open", "")?;
    dialog_focusing_steps(ctx, id)
}

// https://html.spec.whatwg.org/multipage/interactive-elements.html#the-dialog-element:dom-dialog-close
pub(super) fn close(ctx: &Ctx<'_>, id: NodeId, result: Option<DomString>) -> Result<()> {
    if !has_open(ctx, id) {
        return Ok(());
    }
    events::fire_toggle_event(
        ctx,
        EventTargetKey::Node(id),
        "beforetoggle",
        "open",
        "closed",
        None,
        false,
        false,
    )?;
    if !has_open(ctx, id) {
        return Ok(());
    }
    queue_toggle(ctx, id, "open", "closed")?;
    remove_attribute_sync(ctx, id, "", "open", false)?;
    if let Some(result) = result {
        set_return_value(ctx, id, result)?;
    }
    let previous = world_for_node(ctx, id)?
        .borrow_mut()
        .state
        .dialog_previously_focused
        .remove(&id)
        .flatten();
    if let Some(previous) = previous
        && focus_is_inside(ctx, id)?
    {
        focus_node(ctx, previous)?;
    }
    world_for_node(ctx, id)?.borrow_mut().queue_dialog_close(id);
    Ok(())
}

// https://html.spec.whatwg.org/multipage/interactive-elements.html#the-dialog-element:dom-dialog-requestclose
pub(super) fn request_close(ctx: &Ctx<'_>, id: NodeId, result: Option<DomString>) -> Result<()> {
    if !has_open(ctx, id) {
        return Ok(());
    }
    if !is_connected(ctx, id)? || !is_fully_active(ctx, id)? {
        return Ok(());
    }
    let not_canceled =
        events::fire_trusted_cancelable(ctx, EventTargetKey::Node(id), "cancel", false, true)?;
    if !not_canceled {
        return Ok(());
    }
    close(ctx, id, result)
}

fn has_open(ctx: &Ctx<'_>, id: NodeId) -> bool {
    content_attr(ctx, id, "open").is_some()
}

fn is_connected(ctx: &Ctx<'_>, id: NodeId) -> Result<bool> {
    let owner = world_for_node(ctx, id)?;
    let borrowed = owner.borrow();
    Ok(borrowed
        .document(id)
        .is_some_and(|parsed| tree::is_connected(&parsed.document.base, id.node)))
}

// https://html.spec.whatwg.org/multipage/interactive-elements.html#show-a-modal-dialog
fn is_fully_active(ctx: &Ctx<'_>, id: NodeId) -> Result<bool> {
    let owner = world_for_node(ctx, id)?;
    let borrowed = owner.borrow();
    Ok(borrowed
        .main_document()
        .is_some_and(|parsed| parsed.id == id.document))
}

// https://html.spec.whatwg.org/multipage/interactive-elements.html#the-dialog-element:previously-focused-element
fn capture_previously_focused(ctx: &Ctx<'_>, id: NodeId) -> Result<()> {
    let owner = world_for_node(ctx, id)?;
    let mut world = owner.borrow_mut();
    let focused = world.active_element(id.document);
    world.state.dialog_previously_focused.insert(id, focused);
    Ok(())
}

// https://html.spec.whatwg.org/multipage/interactive-elements.html#the-dialog-element:close-the-dialog
fn focus_is_inside(ctx: &Ctx<'_>, id: NodeId) -> Result<bool> {
    let owner = world_for_node(ctx, id)?;
    let focused = owner.borrow().active_element(id.document);
    let Some(focused) = focused else {
        return Ok(false);
    };
    let borrowed = owner.borrow();
    let Some(parsed) = borrowed.document(id) else {
        return Ok(false);
    };
    Ok(tree::ancestor_chain(&parsed.document.base, id.document, focused.node).contains(&id))
}

fn queue_toggle(ctx: &Ctx<'_>, id: NodeId, old_state: &str, new_state: &str) -> Result<()> {
    world_for_node(ctx, id)?
        .borrow_mut()
        .queue_toggle_task(id, old_state, new_state);
    Ok(())
}

// https://html.spec.whatwg.org/multipage/interactive-elements.html#dialog-focusing-steps
fn dialog_focusing_steps(ctx: &Ctx<'_>, id: NodeId) -> Result<()> {
    if !is_connected(ctx, id)? {
        return Ok(());
    }
    let mut control = None;
    if content_attr(ctx, id, "autofocus").is_some() && is_focusable(ctx, id)? {
        control = Some(id);
    }
    if control.is_none() {
        control = autofocus_descendant(ctx, id)?;
    }
    if control.is_none() {
        control = first_focusable_descendant(ctx, id)?;
    }
    if control.is_none() && is_focusable(ctx, id)? {
        control = Some(id);
    }
    if let Some(control) = control {
        focus_node(ctx, control)?;
    }
    Ok(())
}

fn autofocus_descendant(ctx: &Ctx<'_>, id: NodeId) -> Result<Option<NodeId>> {
    let candidates = descendants_with(ctx, id, |base, node| {
        tree::attr(base, node, "autofocus").is_some()
    })?;
    for candidate in candidates {
        if is_focusable(ctx, candidate)? {
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

fn first_focusable_descendant(ctx: &Ctx<'_>, id: NodeId) -> Result<Option<NodeId>> {
    for candidate in descendants_with(ctx, id, |_, _| true)? {
        if is_focusable(ctx, candidate)? {
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

fn descendants_with(
    ctx: &Ctx<'_>,
    id: NodeId,
    predicate: impl Fn(&blitz_dom::BaseDocument, BlitzId) -> bool,
) -> Result<Vec<NodeId>> {
    let owner = world_for_node(ctx, id)?;
    let mut found = Vec::new();
    let borrowed = owner.borrow();
    let Some(parsed) = borrowed.document(id) else {
        return Ok(found);
    };
    let base = &parsed.document.base;
    let Some(root) = base.get_node(id.node) else {
        return Ok(found);
    };
    let mut stack: Vec<BlitzId> = root.children.iter().rev().copied().collect();
    while let Some(node) = stack.pop() {
        if predicate(base, node) {
            found.push(NodeId {
                document: id.document,
                node,
            });
        }
        if let Some(tree) = base.get_node(node) {
            stack.extend(tree.children.iter().rev().copied());
        }
    }
    Ok(found)
}
