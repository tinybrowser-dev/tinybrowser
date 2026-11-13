//! CSSOM View scrolling: which box a scroll call addresses, and an element's
//! scroll position.
//!
//! The scroll engine is Blitz's (`BaseDocument::scroll_to`, `scroll_by`,
//! `scroll_into_view`, and the per-node scroll offsets); this module owns the
//! CSSOM View algorithms that pick the scrolling element and route a root or
//! body call to the viewport
//! (<https://drafts.csswg.org/cssom-view/#scrolling>).

use blitz_dom::{Node, NodeData};
use markup5ever::interface::QuirksMode;
use style::values::computed::Overflow;
use stylo_taffy::CopyStyleGetters;

use crate::Parsed;
use crate::dom::tree::{NodeId, html_namespace};

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
/// The body element's parent and the body itself must each have an overflow
/// that creates a scrolling box. `overflow: clip` on the parent counts as
/// `hidden` for the body, so only `visible` disqualifies it there.
fn potentially_scrollable_in(parsed: &Parsed, element: NodeId, vertical: bool) -> bool {
    let base = &parsed.document.base;
    let Some(node) = base.get_node(element.node) else {
        return false;
    };
    // "has an associated box": a boxless element is never potentially
    // scrollable.
    if !node.has_boxes() {
        return false;
    }
    if !overflow(node, vertical).is_some_and(scrollable_overflow) {
        return false;
    }
    let Some(parent) = node.parent.and_then(|parent| base.get_node(parent)) else {
        return false;
    };
    overflow(parent, vertical).is_some_and(|overflow| overflow != Overflow::Visible)
}

/// Whether an element is potentially scrollable in at least one axis.
pub(super) fn potentially_scrollable(parsed: &Parsed, element: NodeId) -> bool {
    potentially_scrollable_in(parsed, element, false)
        || potentially_scrollable_in(parsed, element, true)
}

/// The document's `scrollingElement`
/// (<https://drafts.csswg.org/cssom-view/#dom-document-scrollingelement>).
///
/// In quirks mode the body element scrolls the viewport unless it is itself
/// potentially scrollable; otherwise the root element is the answer.
pub(super) fn scrolling_element(parsed: &mut Parsed) -> Option<NodeId> {
    parsed.document.base.resolve(0.0);
    if parsed.quirks_mode == QuirksMode::Quirks {
        let body = body_element(parsed)?;
        return (!potentially_scrollable(parsed, body)).then_some(body);
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
pub(crate) fn scroll_position(parsed: &Parsed, element: NodeId, vertical: bool) -> f64 {
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
    if !node.has_boxes() || !overflow(node, vertical).is_some_and(scrollable_overflow) {
        return 0.0;
    }
    let offset = node.scroll_offset();
    if vertical { offset.y } else { offset.x }
}

/// Scroll an element to an absolute position, following the root/body routing
/// of `scrollTop`/`scrollLeft` and `Element.scroll()`
/// (<https://drafts.csswg.org/cssom-view/#dom-element-scrolltop>).
pub(crate) fn scroll_to(
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
    let target = if root_element(parsed) == Some(element) {
        if quirks {
            return;
        }
        root_element(parsed)
    } else if quirks
        && body_element(parsed) == Some(element)
        && !potentially_scrollable(parsed, element)
    {
        root_element(parsed)
    } else {
        Some(element)
    };
    let Some(target) = target else {
        return;
    };
    parsed.document.base.scroll_to(target.node, x, y, behavior);
}

/// The scroll behavior an IDL `ScrollBehavior` names. An out-of-range spelling
/// cannot reach here: the shim converts the enum first.
pub(crate) fn scroll_behavior(value: &str) -> blitz_dom::ScrollBehavior {
    match value {
        "instant" => blitz_dom::ScrollBehavior::Instant,
        "smooth" => blitz_dom::ScrollBehavior::Smooth,
        // `auto`, and any spelling the shim should already have rejected,
        // take the IDL default.
        _ => blitz_dom::ScrollBehavior::Auto,
    }
}
