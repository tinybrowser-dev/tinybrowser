// https://dom.spec.whatwg.org/#concept-node-equals

use blitz_dom::NodeData;
use blitz_traits::node_id::NodeId as BlitzId;
use markup5ever::QualName;

use crate::documents::{BlitzDocument, DocumentStore};
use crate::dom::tree::NodeId;
use crate::dom_string::DomString;

// https://dom.spec.whatwg.org/#concept-node-equals
struct EqualView {
    kind: EqualKind,
    children: Vec<BlitzId>,
}

enum EqualKind {
    Document,
    Fragment,
    Element {
        name: QualName,
        attributes: Vec<(QualName, String)>,
    },
    Text(DomString),
    Comment(DomString),
    Doctype {
        name: String,
        public_id: String,
        system_id: String,
    },
    ProcessingInstruction {
        target: String,
        data: DomString,
    },
    CData(DomString),
    Other,
}

fn equal_view(doc: &BlitzDocument, id: BlitzId) -> Option<EqualView> {
    let (kind, children) = {
        let node = doc.base.get_node(id)?;
        let children = node.children.iter().copied().collect();
        let kind = match &node.data {
            NodeData::Document(_) => EqualKind::Document,
            NodeData::Fragment { .. } | NodeData::ShadowRoot { .. } => EqualKind::Fragment,
            NodeData::Element(element) => EqualKind::Element {
                name: element.name.clone(),
                attributes: element
                    .attrs
                    .iter()
                    .map(|attribute| (attribute.name.clone(), attribute.value.clone()))
                    .collect(),
            },
            NodeData::Text(_) => EqualKind::Text(DomString::default()),
            NodeData::Comment { .. } => EqualKind::Comment(DomString::default()),
            NodeData::Doctype {
                name,
                public_id,
                system_id,
            } => EqualKind::Doctype {
                name: name.clone(),
                public_id: public_id.clone(),
                system_id: system_id.clone(),
            },
            NodeData::ProcessingInstruction { target, .. } => EqualKind::ProcessingInstruction {
                target: target.clone(),
                data: DomString::default(),
            },
            NodeData::CDataSection { .. } => EqualKind::CData(DomString::default()),
            NodeData::AnonymousBlock(_) => EqualKind::Other,
        };
        (kind, children)
    };
    let kind = match kind {
        EqualKind::Text(_) => EqualKind::Text(doc.character_data(id)),
        EqualKind::Comment(_) => EqualKind::Comment(doc.character_data(id)),
        EqualKind::ProcessingInstruction { target, .. } => EqualKind::ProcessingInstruction {
            target,
            data: doc.character_data(id),
        },
        EqualKind::CData(_) => EqualKind::CData(doc.character_data(id)),
        other => other,
    };
    Some(EqualView { kind, children })
}

fn kinds_equal(left: &EqualKind, right: &EqualKind) -> bool {
    match (left, right) {
        (EqualKind::Document, EqualKind::Document) | (EqualKind::Fragment, EqualKind::Fragment) => {
            true
        }
        (
            EqualKind::Element {
                name: left_name,
                attributes: left_attributes,
            },
            EqualKind::Element {
                name: right_name,
                attributes: right_attributes,
            },
        ) => {
            // https://dom.spec.whatwg.org/#concept-node-equals
            left_name == right_name
                && left_attributes.len() == right_attributes.len()
                && left_attributes.iter().all(|(name, value)| {
                    right_attributes.iter().any(|(candidate_name, candidate)| {
                        candidate_name.ns == name.ns
                            && candidate_name.local == name.local
                            && candidate == value
                    })
                })
        }
        (EqualKind::Text(left), EqualKind::Text(right))
        | (EqualKind::Comment(left), EqualKind::Comment(right))
        | (EqualKind::CData(left), EqualKind::CData(right)) => left == right,
        (
            EqualKind::Doctype {
                name: left_name,
                public_id: left_public,
                system_id: left_system,
            },
            EqualKind::Doctype {
                name: right_name,
                public_id: right_public,
                system_id: right_system,
            },
        ) => left_name == right_name && left_public == right_public && left_system == right_system,
        (
            EqualKind::ProcessingInstruction {
                target: left_target,
                data: left_data,
            },
            EqualKind::ProcessingInstruction {
                target: right_target,
                data: right_data,
            },
        ) => left_target == right_target && left_data == right_data,
        _ => false,
    }
}

// https://dom.spec.whatwg.org/#concept-node-equals
#[must_use]
pub fn nodes_equal(store: &DocumentStore, left: NodeId, right: NodeId) -> bool {
    let left_view = store
        .get(left.document)
        .and_then(|parsed| equal_view(&parsed.document, left.node));
    let right_view = store
        .get(right.document)
        .and_then(|parsed| equal_view(&parsed.document, right.node));
    let (Some(left_view), Some(right_view)) = (left_view, right_view) else {
        return false;
    };
    if !kinds_equal(&left_view.kind, &right_view.kind) {
        return false;
    }
    if left_view.children.len() != right_view.children.len() {
        return false;
    }
    left_view
        .children
        .into_iter()
        .zip(right_view.children)
        .all(|(left_child, right_child)| {
            nodes_equal(
                store,
                NodeId {
                    document: left.document,
                    node: left_child,
                },
                NodeId {
                    document: right.document,
                    node: right_child,
                },
            )
        })
}
