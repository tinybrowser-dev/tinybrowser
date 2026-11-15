//! CSS `zoom` as the CSSOM View geometry members see it.
//!
//! Layout lengths are scaled by the element's effective zoom, but every
//! geometry API except `getBoundingClientRect()`, `getClientRects()`, and
//! `IntersectionObserver` reports unscaled lengths
//! (<https://drafts.csswg.org/css-viewport/#zoom-dom-cssom>).

use blitz_dom::Node;

/// The element's effective zoom: the factor its layout lengths are scaled by.
///
/// An element that is not being rendered has an effective zoom of `1.0`
/// (<https://drafts.csswg.org/cssom-view/#dom-element-currentcsszoom>).
pub(super) fn effective_zoom(node: &Node) -> f64 {
    if !node.has_boxes() {
        return 1.0;
    }
    node.primary_styles()
        .map_or(1.0, |style| f64::from(style.effective_zoom.value()))
}

/// Converts a layout length to a CSSOM View length, which is unscaled
/// (<https://drafts.csswg.org/css-viewport/#unscaled>).
pub(super) fn unscaled(node: &Node, value: f64) -> f64 {
    value / effective_zoom(node)
}

/// Converts a CSSOM View length to a layout length
/// (<https://drafts.csswg.org/css-viewport/#unscaled>).
pub(super) fn scaled(node: &Node, value: f64) -> f64 {
    value * effective_zoom(node)
}
