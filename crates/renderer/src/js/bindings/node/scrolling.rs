//! CSSOM View scrolling: which box a scroll call addresses, and an element's
//! scroll position.
//!
//! The scroll engine is Blitz's (`BaseDocument::scroll_to`, `scroll_by`,
//! `scroll_into_view`, and the per-node scroll offsets); this module owns the
//! CSSOM View algorithms that pick the scrolling element, route a root or body
//! call to the viewport, and gate every call on the node's document being the
//! active one (<https://drafts.csswg.org/cssom-view/#scrolling>).

use blitz_dom::{Node, NodeData};
use markup5ever::interface::QuirksMode;
use rquickjs::{Ctx, Result, Value};
use style::values::computed::Overflow;
use stylo_taffy::CopyStyleGetters;

use crate::Parsed;
use crate::dom::tree::{NodeId, html_namespace};

use super::super::{active_world, host_node_id, world};
use super::document::document_first_child;

/// Whether an axis with this computed overflow has a scrolling box.
///
/// `visible` and `clip` have none; `hidden` does for programmatic scrolls
/// (<https://drafts.csswg.org/cssom-view/#scrolling-box>).
fn scrollable_overflow(overflow: Overflow) -> bool {
    !matches!(overflow, Overflow::Visible | Overflow::Clip)
}

/// One axis of a node's computed `overflow`, when the node has styles.
fn overflow(node: &Node, vertical: bool) -> Option<Overflow> {
    let styles = node.primary_styles()?;
    Some(if vertical {
        styles.clone_overflow_y()
    } else {
        styles.clone_overflow_x()
    })
}

/// Whether an element's `overflow` is applied to the viewport instead of to a
/// scrolling box of its own
/// (<https://drafts.csswg.org/css-overflow-3/#overflow-propagation>).
///
/// The root element's overflow always belongs to the viewport. When the root is
/// an HTML `html` element whose own overflow is `visible` on both axes, the
/// first `body` child's overflow propagates instead, and that `body` has no
/// scrolling box.
fn overflow_propagates_to_viewport(parsed: &Parsed, element: NodeId) -> bool {
    parsed
        .document
        .base
        .overflow_propagates_to_viewport(element.node)
}

/// The used overflow of an element on one axis, with a propagated body's value
/// replaced by `visible`.
///
/// The element a value propagates from gets a used overflow of `visible`
/// (<https://drafts.csswg.org/css-overflow-3/#overflow-propagation>), so it
/// has no scrolling box even though its computed value is `hidden`, `scroll`,
/// or `auto`.
fn used_overflow(parsed: &Parsed, element: NodeId, vertical: bool) -> Option<Overflow> {
    if overflow_propagates_to_viewport(parsed, element) {
        return Some(Overflow::Visible);
    }
    overflow(parsed.document.base.get_node(element.node)?, vertical)
}

/// The document's root element: its first element child
/// (<https://dom.spec.whatwg.org/#document-element>).
pub(crate) fn root_element(parsed: &Parsed) -> Option<NodeId> {
    document_first_child(parsed, |data| matches!(data, NodeData::Element(_)))
}

/// The document's body element
/// (<https://html.spec.whatwg.org/multipage/dom.html#the-body-element-2>).
///
/// This is the first HTML-namespaced `body` that is a child of an
/// HTML-namespaced `html` element, not `document.body`: the latter can also
/// return a `frameset`.
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

/// Whether an element is potentially scrollable in one axis
/// (<https://drafts.csswg.org/cssom-view/#potentially-scrollable>).
///
/// The parent's overflow and the element's own overflow must each create a
/// scrolling box. `parent_clip_counts_as_hidden` is the `scrollingElement`
/// step's override, which applies only there: the scroll getters and setters
/// use the definition as written, where a parent `overflow: clip` does not
/// count (<https://drafts.csswg.org/cssom-view/#dom-document-scrollingelement>).
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
    // "has an associated box": a boxless element is never potentially
    // scrollable.
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

/// Whether an element is potentially scrollable in at least one axis, by the
/// definition the scroll getters and setters use.
fn potentially_scrollable(parsed: &Parsed, element: NodeId) -> bool {
    potentially_scrollable_in(parsed, element, false, false)
        || potentially_scrollable_in(parsed, element, true, false)
}

/// The same question with the `scrollingElement` step's `clip`-as-`hidden`
/// override for the body element's parent.
fn potentially_scrollable_for_scrolling_element(parsed: &Parsed, element: NodeId) -> bool {
    potentially_scrollable_in(parsed, element, false, true)
        || potentially_scrollable_in(parsed, element, true, true)
}

/// The document's `scrollingElement`
/// (<https://drafts.csswg.org/cssom-view/#dom-document-scrollingelement>).
///
/// In quirks mode the answer is the body element when the body is not
/// potentially scrollable, and null otherwise; every other document answers
/// with its root element or null.
pub(super) fn scrolling_element(parsed: &mut Parsed) -> Option<NodeId> {
    parsed.document.base.resolve(0.0);
    if parsed.quirks_mode == QuirksMode::Quirks {
        let body = body_element(parsed)?;
        return (!potentially_scrollable_for_scrolling_element(parsed, body)).then_some(body);
    }
    root_element(parsed)
}

/// The viewport's scroll offset on one axis.
fn viewport_offset(parsed: &Parsed, vertical: bool) -> f64 {
    let scroll = parsed.document.base.viewport_scroll();
    if vertical { scroll.y } else { scroll.x }
}

/// An element's `scrollTop`/`scrollLeft`
/// (<https://drafts.csswg.org/cssom-view/#dom-element-scrolltop>).
///
/// The root element reports the viewport. In quirks mode the body element does
/// too, unless it is potentially scrollable, and the root element reports zero.
fn scroll_position(parsed: &Parsed, element: NodeId, vertical: bool) -> f64 {
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
    // A boxless element, or one without a scrolling box on this axis, stays
    // at the start.
    if !node.has_boxes()
        || !used_overflow(parsed, element, vertical).is_some_and(scrollable_overflow)
    {
        return 0.0;
    }
    let offset = node.scroll_offset();
    if vertical { offset.y } else { offset.x }
}

/// Scroll an element to an absolute position, following the root/body routing
/// of `scrollTop`/`scrollLeft` and `Element.scroll()`
/// (<https://drafts.csswg.org/cssom-view/#dom-element-scrolltop>).
fn scroll_to(
    parsed: &mut Parsed,
    element: NodeId,
    x: f64,
    y: f64,
    behavior: blitz_dom::ScrollBehavior,
) {
    // "Normalize non-finite values": NaN and the infinities become zero
    // (<https://drafts.csswg.org/cssom-view/#normalize-non-finite-values>).
    let x = if x.is_finite() { x } else { 0.0 };
    let y = if y.is_finite() { y } else { 0.0 };
    let quirks = parsed.quirks_mode == QuirksMode::Quirks;
    parsed.document.base.resolve(0.0);
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
    // An element without a scrolling box on either axis has nothing to move; a
    // propagated body has a used overflow of `visible`. This is the element's
    // own box only: CSSOM's parent-overflow rule in "potentially scrollable"
    // belongs to the `scrollingElement` and quirks-body questions, and applying
    // it here would freeze every scroller inside a `body` with visible
    // overflow (`scrollLeftTop.html`).
    if Some(target) != root
        && !used_overflow(parsed, target, false).is_some_and(scrollable_overflow)
        && !used_overflow(parsed, target, true).is_some_and(scrollable_overflow)
    {
        return;
    }
    // CSSOM View's `Element.scroll()` step for the root element delegates to
    // `window.scroll(scrollX, y)`, which would discard x, but Chrome applies
    // both coordinates and `css/cssom-view/scrolling-quirks-vs-nonquirks.html`
    // asserts that (`scroll(50, 60)` leaves scrollLeft at 50). Follow the
    // browser.
    parsed.document.base.scroll_to(target.node, x, y, behavior);
}

/// An element's current scroll position on one axis, for the shim's overloaded
/// `scroll()`/`scrollTo()` family
/// (<https://drafts.csswg.org/cssom-view/#dom-element-scrolltop>).
pub(crate) fn element_scroll_position(ctx: &Ctx<'_>, id: NodeId, vertical: bool) -> f64 {
    let Some(world) = active_world(ctx, id) else {
        return 0.0;
    };
    let world = world.borrow();
    let Some(mut parsed) = world.document_mut(id) else {
        return 0.0;
    };
    parsed.document.base.resolve(0.0);
    scroll_position(&parsed, id, vertical)
}

/// Scroll an element to `(x, y)` (<https://drafts.csswg.org/cssom-view/#dom-element-scroll>).
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

/// The scroll behavior an IDL `ScrollBehavior` names. The shim converts the
/// enum before this call; anything else takes the IDL default.
fn scroll_behavior(value: &str) -> blitz_dom::ScrollBehavior {
    match value {
        "instant" => blitz_dom::ScrollBehavior::Instant,
        "smooth" => blitz_dom::ScrollBehavior::Smooth,
        _ => blitz_dom::ScrollBehavior::Auto,
    }
}

/// Registers the scroll host functions the CSSOM View shims call.
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

/// The viewport's scroll offset `(x, y)`, for `window.scrollX`/`scrollY` and
/// `pageXOffset`/`pageYOffset`
/// (<https://drafts.csswg.org/cssom-view/#dom-window-scrollx>).
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
    parsed.document.base.resolve(0.0);
    let scroll = parsed.document.base.viewport_scroll();
    vec![scroll.x, scroll.y]
}

/// `window.scroll()`/`scrollTo()`: scroll the viewport to `(x, y)`
/// (<https://drafts.csswg.org/cssom-view/#dom-window-scroll>).
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
    parsed.document.base.resolve(0.0);
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
