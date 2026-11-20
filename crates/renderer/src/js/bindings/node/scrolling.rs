// https://drafts.csswg.org/cssom-view/#scrolling

use blitz_dom::{Node, NodeData};
use markup5ever::interface::QuirksMode;
use rquickjs::{Ctx, Result, Value};
use style::values::computed::Overflow;
use stylo_taffy::CopyStyleGetters;

use crate::Parsed;
use crate::dom::tree::{NodeId, html_namespace};

use super::super::{active_world, host_node_id, world};
use super::document::document_first_child;

// https://drafts.csswg.org/cssom-view/#scrolling-box
fn scrollable_overflow(overflow: Overflow) -> bool {
    !matches!(overflow, Overflow::Visible | Overflow::Clip)
}

fn overflow(node: &Node, vertical: bool) -> Option<Overflow> {
    let styles = node.primary_styles()?;
    Some(if vertical {
        styles.clone_overflow_y()
    } else {
        styles.clone_overflow_x()
    })
}

// https://drafts.csswg.org/css-overflow-3/#overflow-propagation
fn overflow_propagates_to_viewport(parsed: &Parsed, element: NodeId) -> bool {
    parsed
        .document
        .base
        .overflow_propagates_to_viewport(element.node)
}

// https://drafts.csswg.org/css-overflow-3/#overflow-propagation
fn used_overflow(parsed: &Parsed, element: NodeId, vertical: bool) -> Option<Overflow> {
    if overflow_propagates_to_viewport(parsed, element) {
        return Some(Overflow::Visible);
    }
    overflow(parsed.document.base.get_node(element.node)?, vertical)
}

// https://dom.spec.whatwg.org/#document-element
pub(crate) fn root_element(parsed: &Parsed) -> Option<NodeId> {
    document_first_child(parsed, |data| matches!(data, NodeData::Element(_)))
}

// https://html.spec.whatwg.org/multipage/dom.html#the-body-element-2
pub(super) fn body_element(parsed: &Parsed) -> Option<NodeId> {
    let base = &parsed.document.base;
    let html = base.get_node(root_element(parsed)?.node)?;
    let element = html.data.downcast_element()?;
    if element.name.ns != html_namespace() || element.name.local.as_str() != "html" {
        return None;
    }
    html.children.iter().find_map(|child| {
        let node = base.get_node(*child)?;
        let element = node.data.downcast_element()?;
        (element.name.ns == html_namespace() && element.name.local.as_str() == "body").then_some(
            NodeId {
                document: parsed.id,
                node: *child,
            },
        )
    })
}

// https://drafts.csswg.org/cssom-view/#potentially-scrollable
// https://drafts.csswg.org/cssom-view/#dom-document-scrollingelement
fn potentially_scrollable_in(
    parsed: &Parsed,
    element: NodeId,
    vertical: bool,
    parent_clip_counts_as_hidden: bool,
) -> bool {
    let base = &parsed.document.base;
    let Some(node) = base.get_node(element.node) else {
        return false;
    };
    if !node.has_boxes() {
        return false;
    }
    if !used_overflow(parsed, element, vertical).is_some_and(scrollable_overflow) {
        return false;
    }
    let Some(parent) = node.parent.and_then(|parent| base.get_node(parent)) else {
        return false;
    };
    match overflow(parent, vertical) {
        Some(Overflow::Visible) | None => false,
        Some(Overflow::Clip) => parent_clip_counts_as_hidden,
        Some(_) => true,
    }
}

fn potentially_scrollable(parsed: &Parsed, element: NodeId) -> bool {
    potentially_scrollable_in(parsed, element, false, false)
        || potentially_scrollable_in(parsed, element, true, false)
}

fn potentially_scrollable_for_scrolling_element(parsed: &Parsed, element: NodeId) -> bool {
    potentially_scrollable_in(parsed, element, false, true)
        || potentially_scrollable_in(parsed, element, true, true)
}

// https://drafts.csswg.org/cssom-view/#dom-element-scrollheight
pub(super) fn viewport_scroll_area(parsed: &Parsed, vertical: bool, viewport: f64) -> f64 {
    let content = root_element(parsed)
        .and_then(|root| parsed.document.base.get_node(root.node))
        .map_or(0.0, |node| {
            f64::from(if vertical {
                node.scroll_height()
            } else {
                node.scroll_width()
            })
        });
    content.max(viewport)
}

// https://drafts.csswg.org/cssom-view/#dom-element-scrollheight
pub(super) fn scroll_area_is_viewport(parsed: &Parsed, element: NodeId, vertical: bool) -> bool {
    let quirks = parsed.quirks_mode == QuirksMode::Quirks;
    if root_element(parsed) == Some(element) {
        return !quirks;
    }
    quirks
        && body_element(parsed) == Some(element)
        && !potentially_scrollable_in(parsed, element, vertical, false)
}

// https://drafts.csswg.org/cssom-view/#dom-element-clientwidth
pub(super) fn client_area_is_viewport(parsed: &Parsed, element: NodeId) -> bool {
    if root_element(parsed) == Some(element) {
        return parsed.quirks_mode != QuirksMode::Quirks;
    }
    parsed.quirks_mode == QuirksMode::Quirks && body_element(parsed) == Some(element)
}

// https://drafts.csswg.org/cssom-view/#dom-document-scrollingelement
pub(super) fn scrolling_element(parsed: &mut Parsed) -> Option<NodeId> {
    parsed.document.base.resolve(crate::animation_seconds());
    if parsed.quirks_mode == QuirksMode::Quirks {
        let body = body_element(parsed)?;
        return (!potentially_scrollable_for_scrolling_element(parsed, body)).then_some(body);
    }
    root_element(parsed)
}

fn viewport_offset(parsed: &Parsed, vertical: bool) -> f64 {
    let scroll = parsed.document.base.viewport_scroll();
    if vertical { scroll.y } else { scroll.x }
}

// https://drafts.csswg.org/cssom-view/#dom-element-scrolltop
// https://drafts.csswg.org/css-viewport/#zoom-dom-cssom — root Element.scroll() applies both x and y coordinates, matching Chrome and WPT over CSSOM View's scrollX discard
fn scroll_position(parsed: &Parsed, element: NodeId, vertical: bool) -> f64 {
    let offset = scroll_position_scaled(parsed, element, vertical);
    let zoom = parsed
        .document
        .base
        .get_node(element.node)
        .map_or(1.0, super::zoom::effective_zoom);
    offset / zoom
}

fn scroll_position_scaled(parsed: &Parsed, element: NodeId, vertical: bool) -> f64 {
    let quirks = parsed.quirks_mode == QuirksMode::Quirks;
    if root_element(parsed) == Some(element) {
        return if quirks {
            0.0
        } else {
            viewport_offset(parsed, vertical)
        };
    }
    if quirks && body_element(parsed) == Some(element) && !potentially_scrollable(parsed, element) {
        return viewport_offset(parsed, vertical);
    }
    let Some(node) = parsed.document.base.get_node(element.node) else {
        return 0.0;
    };
    if !node.has_boxes()
        || !used_overflow(parsed, element, vertical).is_some_and(scrollable_overflow)
    {
        return 0.0;
    }
    let offset = node.scroll_offset();
    if vertical { offset.y } else { offset.x }
}

// https://drafts.csswg.org/cssom-view/#dom-element-scrolltop
fn scroll_to(
    parsed: &mut Parsed,
    element: NodeId,
    x: f64,
    y: f64,
    behavior: blitz_dom::ScrollBehavior,
) {
    // https://drafts.csswg.org/cssom-view/#normalize-non-finite-values
    let x = if x.is_finite() { x } else { 0.0 };
    let y = if y.is_finite() { y } else { 0.0 };
    let quirks = parsed.quirks_mode == QuirksMode::Quirks;
    parsed.document.base.resolve(crate::animation_seconds());
    let root = root_element(parsed);
    let target = if root == Some(element) {
        if quirks {
            return;
        }
        root
    } else if quirks
        && body_element(parsed) == Some(element)
        && !potentially_scrollable(parsed, element)
    {
        root
    } else {
        Some(element)
    };
    let Some(target) = target else {
        return;
    };
    if Some(target) != root
        && !used_overflow(parsed, target, false).is_some_and(scrollable_overflow)
        && !used_overflow(parsed, target, true).is_some_and(scrollable_overflow)
    {
        return;
    }
    // https://drafts.csswg.org/css-viewport/#zoom-dom-cssom — root Element.scroll() applies both x and y coordinates, matching Chrome and WPT over CSSOM View's scrollX discard
    let (x, y) = match parsed.document.base.get_node(target.node) {
        Some(node) => (super::zoom::scaled(node, x), super::zoom::scaled(node, y)),
        None => (x, y),
    };
    parsed.document.base.scroll_to(target.node, x, y, behavior);
}

// https://drafts.csswg.org/cssom-view/#dom-element-scrolltop
pub(crate) fn element_scroll_position(ctx: &Ctx<'_>, id: NodeId, vertical: bool) -> f64 {
    let Some(world) = active_world(ctx, id) else {
        return 0.0;
    };
    let world = world.borrow();
    let Some(mut parsed) = world.document_mut(id) else {
        return 0.0;
    };
    parsed.document.base.resolve(crate::animation_seconds());
    scroll_position(&parsed, id, vertical)
}

// https://drafts.csswg.org/cssom-view/#dom-element-scroll
pub(crate) fn element_scroll(
    ctx: &Ctx<'_>,
    id: NodeId,
    x: f64,
    y: f64,
    behavior: blitz_dom::ScrollBehavior,
) {
    let Some(world) = active_world(ctx, id) else {
        return;
    };
    let world = world.borrow();
    let Some(mut parsed) = world.document_mut(id) else {
        return;
    };
    scroll_to(&mut parsed, id, x, y, behavior);
}

fn scroll_behavior(value: &str) -> blitz_dom::ScrollBehavior {
    match value {
        "instant" => blitz_dom::ScrollBehavior::Instant,
        "smooth" => blitz_dom::ScrollBehavior::Smooth,
        _ => blitz_dom::ScrollBehavior::Auto,
    }
}

pub(crate) fn install(ctx: &Ctx<'_>) -> Result<()> {
    let bridge = crate::js::bridge::object(ctx)?;
    bridge.set(
        "__tb_viewport_scroll",
        rquickjs::prelude::Func::from(viewport_scroll_offset),
    )?;
    bridge.set(
        "__tb_scroll_viewport",
        rquickjs::prelude::Func::from(scroll_viewport),
    )?;
    bridge.set(
        "__tb_element_scroll_position",
        rquickjs::prelude::Func::from(host_element_scroll_position),
    )?;
    bridge.set(
        "__tb_scroll_element",
        rquickjs::prelude::Func::from(host_scroll_element),
    )?;
    Ok(())
}

// https://drafts.csswg.org/cssom-view/#dom-window-scrollx
fn viewport_scroll_offset(ctx: Ctx<'_>) -> Vec<f64> {
    let Ok(world) = world(&ctx) else {
        return vec![0.0, 0.0];
    };
    let world = world.borrow();
    let Some(document) = world.main_document_id() else {
        return vec![0.0, 0.0];
    };
    let Some(mut parsed) = world.document_mut_by_id(document) else {
        return vec![0.0, 0.0];
    };
    parsed.document.base.resolve(crate::animation_seconds());
    let scroll = parsed.document.base.viewport_scroll();
    vec![scroll.x, scroll.y]
}

// https://drafts.csswg.org/cssom-view/#dom-window-scroll
fn scroll_viewport(ctx: Ctx<'_>, x: f64, y: f64, behavior: String) {
    let Ok(world) = world(&ctx) else {
        return;
    };
    let world = world.borrow();
    let Some(document) = world.main_document_id() else {
        return;
    };
    let Some(mut parsed) = world.document_mut_by_id(document) else {
        return;
    };
    let behavior = scroll_behavior(&behavior);
    parsed.document.base.resolve(crate::animation_seconds());
    let Some(root) = root_element(&parsed) else {
        return;
    };
    parsed.document.base.scroll_to(root.node, x, y, behavior);
}

fn host_element_scroll_position<'js>(ctx: Ctx<'js>, element: Value<'js>, vertical: bool) -> f64 {
    let Some(id) = host_node_id(&ctx, &element) else {
        return 0.0;
    };
    element_scroll_position(&ctx, id, vertical)
}

fn host_scroll_element<'js>(ctx: Ctx<'js>, element: Value<'js>, x: f64, y: f64, behavior: String) {
    let Some(id) = host_node_id(&ctx, &element) else {
        return;
    };
    let behavior = scroll_behavior(&behavior);
    element_scroll(&ctx, id, x, y, behavior);
}
