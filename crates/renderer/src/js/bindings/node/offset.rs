// https://drafts.csswg.org/cssom-view/#extensions-to-the-htmlelement-interface

use rquickjs::{Ctx, Result, Value};
use style::values::computed::{PositionProperty, TransformStyle};
use stylo_taffy::convert::{containing_block_claims, inline_containing_block_claims};
use stylo_taffy::{CopyStyleGetters, WritingModeExt};

use crate::Parsed;
use crate::dom::tree::{BlitzId, NodeId, is_html_tag, shadow_including_ancestors};

use super::super::{active_world, child_value};
use super::scrolling;

// https://drafts.csswg.org/cssom-view/#dom-htmlelement-offsetparent — fixed-positioned element stops at the nearest fixed-position containing block per spec (unlike Chrome), and every HTML body element uses the body offset rule (matching Chrome)
pub(super) fn offset_parent<'js>(ctx: &Ctx<'js>, id: NodeId) -> Result<Value<'js>> {
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

// https://drafts.csswg.org/cssom-view/#dom-htmlelement-offsettop — static body offset stays relative to the initial containing block, matching Blink
pub(super) fn offset_top(ctx: &Ctx<'_>, id: NodeId) -> i32 {
    offset_origin(ctx, id).map_or(0, |(_, y)| cssom_round(y))
}

// https://drafts.csswg.org/cssom-view/#dom-htmlimageelement-x
// https://drafts.csswg.org/cssom-view/#dom-htmlimageelement-y
pub(super) fn image_edge(ctx: &Ctx<'_>, id: NodeId, vertical: bool) -> i32 {
    measure(ctx, id, |base, node| {
        if !has_associated_box(base, node) {
            return None;
        }
        let rect = base.get_client_bounding_rect(node.id)?;
        let scroll = base.viewport_scroll();
        Some(if vertical {
            rect.y + scroll.y
        } else {
            rect.x + scroll.x
        })
    })
    .map_or(0, cssom_round)
}

// https://drafts.csswg.org/cssom-view/#dom-htmlelement-offsetwidth
pub(super) fn offset_width(ctx: &Ctx<'_>, id: NodeId) -> i32 {
    offset_rect(ctx, id).map_or(0, |rect| cssom_round(rect.width))
}

// https://drafts.csswg.org/cssom-view/#dom-htmlelement-offsetheight
pub(super) fn offset_height(ctx: &Ctx<'_>, id: NodeId) -> i32 {
    offset_rect(ctx, id).map_or(0, |rect| cssom_round(rect.height))
}

// https://drafts.csswg.org/cssom-view/#dom-htmlelement-offsettop — static body offset stays relative to the initial containing block, matching Blink
fn offset_rect(ctx: &Ctx<'_>, id: NodeId) -> Option<blitz_dom::BoundingRect> {
    measure(ctx, id, |base, node| {
        if has_associated_box(base, node) {
            base.offset_rect(node.id).map(|rect| unscaled(node, rect))
        } else {
            None
        }
    })
}

// https://drafts.csswg.org/css-viewport/#zoom-dom-cssom
fn unscaled(node: &blitz_dom::Node, rect: blitz_dom::BoundingRect) -> blitz_dom::BoundingRect {
    blitz_dom::BoundingRect {
        x: super::zoom::unscaled(node, rect.x),
        y: super::zoom::unscaled(node, rect.y),
        width: super::zoom::unscaled(node, rect.width),
        height: super::zoom::unscaled(node, rect.height),
    }
}

// https://drafts.csswg.org/cssom-view/#dom-htmlelement-offsettop — static body offset stays relative to the initial containing block, matching Blink
fn offset_origin(ctx: &Ctx<'_>, id: NodeId) -> Option<(f64, f64)> {
    measure(ctx, id, |base, node| {
        if !has_associated_box(base, node) || is_html_body(node) {
            return None;
        }
        base.offset_rect(node.id)
            .or_else(|| empty_inline_rect(base, node))
            .map(|rect| unscaled(node, rect))
            .map(|rect| (rect.x, rect.y))
    })
}

fn measure<T>(
    ctx: &Ctx<'_>,
    id: NodeId,
    read: impl FnOnce(&blitz_dom::BaseDocument, &blitz_dom::Node) -> Option<T>,
) -> Option<T> {
    let world = active_world(ctx, id)?;
    let world = world.borrow();
    let mut parsed = world.document_mut(id)?;
    let base = &mut parsed.document.base;
    base.resolve_if_needed(crate::animation_seconds());
    let node = base.get_node(id.node)?;
    read(base, node)
}

// https://drafts.csswg.org/cssom-view/#dom-htmlelement-offsetparent — fixed-positioned element stops at the nearest fixed-position containing block per spec (unlike Chrome), and every HTML body element uses the body offset rule (matching Chrome)
fn offset_parent_id(parsed: &mut Parsed, id: NodeId) -> Option<NodeId> {
    let root = scrolling::root_element(parsed)?;
    let base = &mut parsed.document.base;
    base.resolve_if_needed(crate::animation_seconds());
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

// https://drafts.csswg.org/cssom-view/#dom-element-checkvisibility
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

// https://drafts.csswg.org/cssom-view/#dom-htmlelement-offsettop — static body offset stays relative to the initial containing block, matching Blink
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
    // https://drafts.csswg.org/css-writing-modes-4/#logical-to-physical
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

// https://drafts.csswg.org/cssom-view/#dom-htmlelement-offsettop — static body offset stays relative to the initial containing block, matching Blink
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

// https://drafts.csswg.org/css-position/#def-cb
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
    // https://drafts.csswg.org/css-transforms-1/#transformable-element
    let claims = if node.is_non_atomic_inline() {
        inline_containing_block_claims(&styles)
    } else {
        containing_block_claims(&styles)
    };
    // https://drafts.csswg.org/css-transforms-2/#transform-style-property
    let preserve_3d = !node.is_non_atomic_inline()
        && styles.get_box().transform_style == TransformStyle::Preserve3d;
    ContainingBlock {
        absolute: claims.absolute || preserve_3d,
        fixed: claims.fixed || preserve_3d,
    }
}

fn is_html_body(node: &blitz_dom::Node) -> bool {
    is_html_tag(Some(&node.data), "body")
}

// https://drafts.csswg.org/cssom-view/#dom-htmlelement-offsetparent — fixed-positioned element stops at the nearest fixed-position containing block per spec (unlike Chrome), and every HTML body element uses the body offset rule (matching Chrome)
fn is_html_table_part(node: &blitz_dom::Node) -> bool {
    ["td", "th", "table"]
        .iter()
        .any(|local| is_html_tag(Some(&node.data), local))
}

// https://drafts.csswg.org/cssom-view/#dom-htmlelement-offsettop — static body offset stays relative to the initial containing block, matching Blink
pub(super) fn cssom_round(value: f64) -> i32 {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "float-to-int casts saturate and map NaN to zero"
    )]
    return value.round() as i32;
}
