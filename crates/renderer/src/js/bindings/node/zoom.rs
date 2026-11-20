// https://drafts.csswg.org/css-viewport/#zoom-dom-cssom

use blitz_dom::Node;

// https://drafts.csswg.org/cssom-view/#dom-element-currentcsszoom
pub(super) fn effective_zoom(node: &Node) -> f64 {
    if !node.has_boxes() {
        return 1.0;
    }
    node.primary_styles()
        .map_or(1.0, |style| f64::from(style.effective_zoom.value()))
}

// https://drafts.csswg.org/css-viewport/#unscaled
pub(super) fn unscaled(node: &Node, value: f64) -> f64 {
    value / effective_zoom(node)
}

// https://drafts.csswg.org/css-viewport/#unscaled
pub(super) fn scaled(node: &Node, value: f64) -> f64 {
    value * effective_zoom(node)
}
