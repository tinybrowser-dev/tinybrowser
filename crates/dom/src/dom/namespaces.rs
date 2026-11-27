// https://dom.spec.whatwg.org/#locate-a-namespace

use blitz_dom::NodeData;

use crate::dom::tree::{NodeId, is_element};

// https://dom.spec.whatwg.org/#parent-element
pub fn parent_element_of(doc: &crate::documents::BlitzDocument, id: NodeId) -> Option<NodeId> {
    let parent = doc.base.get_node(id.node)?.parent?;
    is_element(&doc.base, parent).then_some(NodeId {
        document: id.document,
        node: parent,
    })
}

pub fn document_element_of(doc: &crate::documents::BlitzDocument, id: NodeId) -> Option<NodeId> {
    doc.base
        .get_node(id.node)?
        .children
        .iter()
        .find_map(|child| {
            is_element(&doc.base, *child).then_some(NodeId {
                document: id.document,
                node: *child,
            })
        })
}

// https://dom.spec.whatwg.org/#locate-a-namespace
pub fn locate_namespace(
    doc: &crate::documents::BlitzDocument,
    node: NodeId,
    prefix: Option<&str>,
) -> Option<String> {
    if doc.is_fragment(node.node) {
        return None;
    }
    let data = &doc.base.get_node(node.node)?.data;
    if matches!(data, NodeData::Doctype { .. }) {
        return None;
    }
    if matches!(data, NodeData::Document(_)) {
        return document_element_of(doc, node)
            .and_then(|element| locate_namespace(doc, element, prefix));
    }
    if !is_element(&doc.base, node.node) {
        return parent_element_of(doc, node)
            .and_then(|element| locate_namespace(doc, element, prefix));
    }
    locate_element_namespace(doc, node, prefix)
}

// https://dom.spec.whatwg.org/#locate-a-namespace
pub fn locate_element_namespace(
    doc: &crate::documents::BlitzDocument,
    id: NodeId,
    prefix: Option<&str>,
) -> Option<String> {
    match prefix {
        Some("xml") => return Some("http://www.w3.org/XML/1998/namespace".to_owned()),
        Some("xmlns") => return Some("http://www.w3.org/2000/xmlns/".to_owned()),
        _ => {}
    }
    let element = doc.base.get_node(id.node)?.data.downcast_element()?;
    let actual = element
        .name
        .prefix
        .as_ref()
        .map(markup5ever::Prefix::as_str)
        .filter(|prefix| !prefix.is_empty());
    if !element.name.ns.as_str().is_empty() && actual == prefix {
        return Some(element.name.ns.as_str().to_owned());
    }
    for attribute in element.attrs.iter() {
        if attribute.name.ns.as_str() != "http://www.w3.org/2000/xmlns/" {
            continue;
        }
        let declaration = match prefix {
            Some(prefix) => {
                attribute
                    .name
                    .prefix
                    .as_ref()
                    .is_some_and(|value| value.as_str() == "xmlns")
                    && attribute.name.local.as_str() == prefix
            }
            None => attribute.name.prefix.is_none() && attribute.name.local.as_str() == "xmlns",
        };
        if declaration {
            return (!attribute.value.is_empty()).then(|| attribute.value.clone());
        }
    }
    parent_element_of(doc, id).and_then(|parent| locate_namespace(doc, parent, prefix))
}

// https://dom.spec.whatwg.org/#locate-a-namespace-prefix
pub fn locate_prefix(
    doc: &crate::documents::BlitzDocument,
    node: NodeId,
    namespace: &str,
) -> Option<String> {
    if doc.is_fragment(node.node) {
        return None;
    }
    let data = &doc.base.get_node(node.node)?.data;
    if matches!(data, NodeData::Doctype { .. }) {
        return None;
    }
    if matches!(data, NodeData::Document(_)) {
        return document_element_of(doc, node)
            .and_then(|element| locate_prefix(doc, element, namespace));
    }
    if !is_element(&doc.base, node.node) {
        return parent_element_of(doc, node)
            .and_then(|element| locate_prefix(doc, element, namespace));
    }
    locate_element_prefix(doc, node, namespace)
}

// https://dom.spec.whatwg.org/#locate-a-namespace-prefix
pub fn locate_element_prefix(
    doc: &crate::documents::BlitzDocument,
    id: NodeId,
    namespace: &str,
) -> Option<String> {
    let element = doc.base.get_node(id.node)?.data.downcast_element()?;
    if element.name.ns.as_str() == namespace
        && let Some(prefix) = element
            .name
            .prefix
            .as_ref()
            .map(markup5ever::Prefix::as_str)
            .filter(|prefix| !prefix.is_empty())
    {
        return Some(prefix.to_owned());
    }
    for attribute in element.attrs.iter() {
        if attribute
            .name
            .prefix
            .as_ref()
            .is_some_and(|prefix| prefix.as_str() == "xmlns")
            && attribute.value == namespace
        {
            return Some(attribute.name.local.as_str().to_owned());
        }
    }
    parent_element_of(doc, id).and_then(|parent| locate_prefix(doc, parent, namespace))
}
