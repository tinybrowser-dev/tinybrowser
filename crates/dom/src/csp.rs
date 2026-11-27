// https://w3c.github.io/webappsec-csp/#parse-serialized-policy
// https://w3c.github.io/webappsec-csp/#should-request-be-blocked-by-content-security-policy

pub use net::csp::*;

#[must_use]
pub fn meta_policy(
    base: &blitz_dom::BaseDocument,
    id: blitz_traits::node_id::NodeId,
) -> Option<String> {
    if !crate::dom::tree::is_html_element(base, id, "meta") {
        return None;
    }
    let equiv = crate::dom::tree::attr(base, id, "http-equiv")?;
    if !equiv.eq_ignore_ascii_case("content-security-policy") {
        return None;
    }
    if !ancestor_is(base, id, "head") {
        return None;
    }
    crate::dom::tree::attr(base, id, "content").map(str::to_owned)
}

fn ancestor_is(
    base: &blitz_dom::BaseDocument,
    id: blitz_traits::node_id::NodeId,
    name: &str,
) -> bool {
    let mut current = Some(id);
    while let Some(node) = current {
        if crate::dom::tree::is_html_element(base, node, name) {
            return true;
        }
        current = base.get_node(node).and_then(|found| found.parent);
    }
    false
}

#[must_use]
pub fn element_nonce(
    base: &blitz_dom::BaseDocument,
    id: blitz_traits::node_id::NodeId,
    slot: Option<&str>,
) -> (String, bool) {
    let Some(element) = base
        .get_node(id)
        .and_then(|node| node.data.downcast_element())
    else {
        return (String::new(), false);
    };
    // https://html.spec.whatwg.org/multipage/urls-and-fetching.html#nonce-attributes
    let mut nonce = slot.unwrap_or_default().to_owned();
    let mut nonceable = !nonce.is_empty();
    for attribute in element.attrs.iter() {
        let name = attribute.name.local.as_str();
        let value = attribute.value.as_str();
        if name.eq_ignore_ascii_case("nonce") && nonce.is_empty() {
            value.clone_into(&mut nonce);
            nonceable = true;
        }
        // https://w3c.github.io/webappsec-csp/#is-element-nonceable
        let lowered_name = name.to_ascii_lowercase();
        let lowered_value = value.to_ascii_lowercase();
        for needle in ["<link", "<script", "<style"] {
            if lowered_name.contains(needle) || lowered_value.contains(needle) {
                nonceable = false;
            }
        }
    }
    if nonce.is_empty() {
        nonceable = false;
    }
    (nonce, nonceable)
}
