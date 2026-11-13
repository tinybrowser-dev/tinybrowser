//! CSSOM View element metrics: `offsetParent`, `offsetLeft`, `offsetTop`,
//! `offsetWidth`, and `offsetHeight`
//! (<https://drafts.csswg.org/cssom-view/#extensions-to-the-htmlelement-interface>).
//!
//! Blitz's `BaseDocument::offset_rect` owns the layout arithmetic, including the
//! first fragment box of a non-atomic inline and the static-body rule. This
//! module owns the CSSOM View rules around it: which elements have an associated
//! box, which ancestor is the offset parent, and the body and table-part cases.

use rquickjs::{Ctx, Result, Value};
use style::values::computed::{PositionProperty, TransformStyle};
use stylo_taffy::convert::{containing_block_claims, inline_containing_block_claims};
use stylo_taffy::{CopyStyleGetters, WritingModeExt};

use crate::Parsed;
use crate::dom::tree::{BlitzId, NodeId, is_html_tag, shadow_including_ancestors};

use super::super::{active_world, child_value};
use super::scrolling;

// https://drafts.csswg.org/cssom-view/#dom-htmlelement-offsetparent
pub(super) fn offset_parent<'js>(ctx: &Ctx<'js>, id: NodeId) -> Result<Value<'js>> {
    // The document borrow must end before wrapping the node, because wrapping
    // reads the realm's wrappers from the same store.
    let found = active_world(ctx, id).and_then(|world| {
        let world = world.borrow();
        let mut parsed = world.document_mut(id)?;
        offset_parent_id(&mut parsed, id)
    });
    child_value(ctx, found)
}

// https://drafts.csswg.org/cssom-view/#dom-htmlelement-offsetleft
pub(super) fn offset_left(ctx: &Ctx<'_>, id: NodeId) -> i32 {
    offset_origin(ctx, id).map_or(0, |(x, _)| cssom_round(x))
}

// https://drafts.csswg.org/cssom-view/#dom-htmlelement-offsettop
pub(super) fn offset_top(ctx: &Ctx<'_>, id: NodeId) -> i32 {
    offset_origin(ctx, id).map_or(0, |(_, y)| cssom_round(y))
}

// https://drafts.csswg.org/cssom-view/#dom-htmlelement-offsetwidth
pub(super) fn offset_width(ctx: &Ctx<'_>, id: NodeId) -> i32 {
    offset_rect(ctx, id).map_or(0, |rect| cssom_round(rect.width))
}

// https://drafts.csswg.org/cssom-view/#dom-htmlelement-offsetheight
pub(super) fn offset_height(ctx: &Ctx<'_>, id: NodeId) -> i32 {
    offset_rect(ctx, id).map_or(0, |rect| cssom_round(rect.height))
}

/// The element's border box, positioned relative to the offset parent's
/// padding edge, or to the initial containing block when there is no offset
/// parent
/// (<https://drafts.csswg.org/cssom-view/#dom-htmlelement-offsettop>).
///
/// The rectangle is Blitz's, so its offset parent is the layout crate's
/// (`is_offset_parent`), which does not know `table` or a `transform`-style
/// fixed-position containing block. Inside such an ancestor the rectangle is
/// measured from a different box than the `offsetParent` reported above
/// (`docs/upstream.md`). For a static `body` ancestor the rectangle stays
/// relative to the initial containing block instead of the body's padding edge,
/// which is what Blink does even though the spec names the body's padding edge
/// (`docs/upstream.md`).
fn offset_rect(ctx: &Ctx<'_>, id: NodeId) -> Option<blitz_dom::BoundingRect> {
    measure(ctx, id, |base, node| {
        if has_associated_box(base, node) {
            base.offset_rect(node.id)
        } else {
            None
        }
    })
}

/// The element's offset origin: the first box's top border edge for
/// `offsetLeft`/`offsetTop`, which differs from the rectangle's union origin
/// when an inline spans several line boxes
/// (<https://drafts.csswg.org/cssom-view/#dom-htmlelement-offsettop>).
///
/// A zero-width non-atomic inline has no fragment box at all, so its position
/// comes from `empty_inline_rect`.
fn offset_origin(ctx: &Ctx<'_>, id: NodeId) -> Option<(f64, f64)> {
    measure(ctx, id, |base, node| {
        if !has_associated_box(base, node) || is_html_body(node) {
            return None;
        }
        base.offset_rect(node.id)
            .or_else(|| empty_inline_rect(base, node))
            .map(|rect| (rect.x, rect.y))
    })
}

/// Runs `read` against this element's node after flushing style and layout,
/// without holding the document borrow across the result.
///
/// Unlike `layout_metric` in `element.rs`, the offset metrics need the active
/// document: an element in a `DOMParser` or `createHTMLDocument` result has no
/// box, and these getters answer zero for it.
fn measure<T>(
    ctx: &Ctx<'_>,
    id: NodeId,
    read: impl FnOnce(&blitz_dom::BaseDocument, &blitz_dom::Node) -> Option<T>,
) -> Option<T> {
    let world = active_world(ctx, id)?;
    let world = world.borrow();
    let mut parsed = world.document_mut(id)?;
    let base = &mut parsed.document.base;
    base.resolve(0.0);
    let node = base.get_node(id.node)?;
    read(base, node)
}

/// `HTMLElement.offsetParent`'s algorithm
/// (<https://drafts.csswg.org/cssom-view/#dom-htmlelement-offsetparent>).
///
/// The walk is the shadow-including ancestor chain, because Blitz has no flat
/// tree (`docs/limitations.md`). A fixed-positioned element whose containing
/// block is an ancestor answers with the nearest ancestor that establishes a
/// fixed-position containing block; every other element answers with the
/// nearest containing block for absolutely positioned descendants, the body, or,
/// when static, a `td`/`th`/`table`. Chrome does not stop at the fixed-position
/// containing block for a fixed element when a positioned ancestor is nearer
/// (probed), but the spec's condition is about the element's own containing
/// block, so tinybrowser follows the spec (`docs/upstream.md`).
fn offset_parent_id(parsed: &mut Parsed, id: NodeId) -> Option<NodeId> {
    let root = scrolling::root_element(parsed)?;
    let base = &mut parsed.document.base;
    base.resolve(0.0);
    let node = base.get_node(id.node)?;
    if !has_associated_box(base, node) || root == id || is_html_body(node) {
        return None;
    }
    let position = node.primary_styles()?.clone_position();
    let in_fixed_containing_block = position == PositionProperty::Fixed
        && shadow_including_ancestors(base, node.parent)
            .any(|ancestor| containing_block(ancestor).fixed);
    if position == PositionProperty::Fixed && !in_fixed_containing_block {
        return None;
    }
    shadow_including_ancestors(base, node.parent).find_map(|ancestor| {
        // A boxless ancestor (`display: contents`) establishes no containing
        // block. The element itself has a box, so no ancestor is
        // `display: none`.
        if !ancestor.has_boxes() {
            return None;
        }
        let claims = containing_block(ancestor);
        let candidate = NodeId {
            document: id.document,
            node: ancestor.id,
        };
        if in_fixed_containing_block {
            return claims.fixed.then_some(candidate);
        }
        (claims.absolute
            || is_html_body(ancestor)
            || (position == PositionProperty::Static && is_html_table_part(ancestor)))
        .then_some(candidate)
    })
}

/// Whether a node has an associated box, which a `display: none` or
/// `display: contents` element lacks, and so does every descendant of a
/// `display: none` ancestor
/// (<https://drafts.csswg.org/cssom-view/#dom-element-checkvisibility>).
///
/// Blitz's `Node::has_boxes` answers for the node alone, so the ancestor walk
/// stays here rather than in the fork (`docs/upstream.md`).
pub(super) fn has_associated_box(base: &blitz_dom::BaseDocument, node: &blitz_dom::Node) -> bool {
    node.has_boxes()
        && !shadow_including_ancestors(base, node.parent).any(|ancestor| {
            ancestor.primary_styles().is_some_and(|styles| {
                matches!(
                    styles.clone_display().inside(),
                    style::values::specified::box_::DisplayInside::None
                )
            })
        })
}

/// The position of a zero-width non-atomic inline, which has no fragment box
/// of its own
/// (<https://drafts.csswg.org/cssom-view/#dom-htmlelement-offsettop>).
///
/// Blitz's inline layout flattens a non-atomic inline into style spans and
/// records fragments only for glyphs and atomic inline boxes, so an empty span
/// has none. Use the next boxed sibling's first fragment (where the caret would
/// be) or, with no following sibling, the end of the previous one along the
/// inline axis. The guess is wrong across a line break; the gap is recorded in
/// `docs/upstream.md`.
fn empty_inline_rect(
    base: &blitz_dom::BaseDocument,
    node: &blitz_dom::Node,
) -> Option<blitz_dom::BoundingRect> {
    if !node.is_non_atomic_inline() {
        return None;
    }
    if let Some(rect) = sibling_offset(base, node.id, true) {
        return Some(blitz_dom::BoundingRect {
            width: 0.0,
            height: 0.0,
            ..rect
        });
    }
    let rect = sibling_offset(base, node.id, false)?;
    // The build disables `writing-mode`, so this is horizontal-tb today; the
    // side is still chosen from the computed writing mode for when the
    // feature lands (`docs/limitations.md`).
    let writing_mode = node.primary_styles()?.writing_mode;
    let (x, y) = if writing_mode.swaps_axes() {
        (rect.x, rect.y + rect.height)
    } else if writing_mode.direction().is_rtl() {
        (rect.x, rect.y)
    } else {
        (rect.x + rect.width, rect.y)
    };
    Some(blitz_dom::BoundingRect {
        x,
        y,
        width: 0.0,
        height: 0.0,
    })
}

/// The nearest element sibling's offset rectangle in `forward` order, for
/// placing a zero-width non-atomic inline
/// (<https://drafts.csswg.org/cssom-view/#dom-htmlelement-offsettop>).
///
/// A sibling without a box (another empty inline, or a `display: none` one) is
/// skipped.
fn sibling_offset(
    base: &blitz_dom::BaseDocument,
    id: BlitzId,
    forward: bool,
) -> Option<blitz_dom::BoundingRect> {
    let node = base.get_node(id)?;
    let parent = base.get_node(node.parent?)?;
    let mut index = parent.children.iter().position(|child| *child == id)?;
    loop {
        index = if forward {
            index + 1
        } else {
            index.checked_sub(1)?
        };
        let sibling = *parent.children.get(index)?;
        let sibling = base.get_node(sibling)?;
        if sibling.data.downcast_element().is_none() || !has_associated_box(base, sibling) {
            continue;
        }
        if let Some(rect) = base.offset_rect(sibling.id) {
            return Some(rect);
        }
    }
}

/// The containing blocks a node establishes for absolutely and fixed
/// positioned descendants
/// (<https://drafts.csswg.org/css-position/#def-cb>).
struct ContainingBlock {
    absolute: bool,
    fixed: bool,
}

fn containing_block(node: &blitz_dom::Node) -> ContainingBlock {
    let Some(styles) = node.primary_styles() else {
        return ContainingBlock {
            absolute: false,
            fixed: false,
        };
    };
    // Transforms and containment do not apply to a non-atomic inline, so its
    // fixed-position containing block comes only from a filter, a backdrop
    // filter, or `will-change`
    // (<https://drafts.csswg.org/css-transforms-1/#transformable-element>).
    let claims = if node.is_non_atomic_inline() {
        inline_containing_block_claims(&styles)
    } else {
        containing_block_claims(&styles)
    };
    // `transform-style: preserve-3d` establishes a containing block for all
    // descendants, absolutely and fixed positioned included
    // (<https://drafts.csswg.org/css-transforms-2/#transform-style-property>),
    // and the layout crate's predicate does not include it, so layout and
    // `offsetParent` disagree inside such an ancestor (`docs/upstream.md`).
    let preserve_3d = !node.is_non_atomic_inline()
        && styles.get_box().transform_style == TransformStyle::Preserve3d;
    ContainingBlock {
        absolute: claims.absolute || preserve_3d,
        fixed: claims.fixed || preserve_3d,
    }
}

/// Whether a node is an HTML `body` element.
///
/// CSSOM View names "the body element" (`document.body`) in the offset
/// algorithm, but browsers apply the body rule to every `body` element: a
/// nested `body` answers 0 for `offsetTop`/`offsetLeft` and null for
/// `offsetParent`, which `offsetParent-body-and-html.html` requires. Chrome
/// agrees (probed), so tinybrowser follows the browser. A `body` ancestor is
/// still the offset parent of a non-body descendant.
fn is_html_body(node: &blitz_dom::Node) -> bool {
    is_html_tag(Some(&node.data), "body")
}

/// Whether a node is an HTML `td`, `th`, or `table` element, the ancestors that
/// are offset parents for a static element
/// (<https://drafts.csswg.org/cssom-view/#dom-htmlelement-offsetparent>).
fn is_html_table_part(node: &blitz_dom::Node) -> bool {
    ["td", "th", "table"]
        .iter()
        .any(|local| is_html_tag(Some(&node.data), local))
}

/// Rounds a layout coordinate to CSSOM's `long`
/// (<https://drafts.csswg.org/cssom-view/#dom-htmlelement-offsettop>).
pub(super) fn cssom_round(value: f64) -> i32 {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "float-to-int casts saturate and map NaN to zero"
    )]
    return value.round() as i32;
}
