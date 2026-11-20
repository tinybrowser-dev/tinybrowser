// https://dom.spec.whatwg.org/#concept-node-adopt
// https://dom.spec.whatwg.org/#concept-node-clone

use blitz_dom::NodeData;
use markup5ever::QualName;

use crate::documents::{BlitzDocument, DocumentStore, JournalEntry};
use crate::dom::TreeError;
use crate::dom::tree::NodeId;

pub(crate) enum ImportSnapshot {
    Element {
        name: QualName,
        attributes: Vec<blitz_dom::Attribute>,
        // https://html.spec.whatwg.org/multipage/input.html#input-element-cloning-steps
        form_state: blitz_dom::FormControlState,
        children: Vec<ImportSnapshot>,
    },
    Text(crate::dom_string::DomString),
    Comment(crate::dom_string::DomString),
    DocumentType {
        name: String,
        public_id: String,
        system_id: String,
    },
    ProcessingInstruction {
        target: String,
        data: crate::dom_string::DomString,
        attributes: Vec<(String, String)>,
    },
    CData(crate::dom_string::DomString),
    Fragment(Vec<ImportSnapshot>),
}

pub(crate) fn snapshot_children(
    doc: &BlitzDocument,
    document: u32,
    children: impl IntoIterator<Item = blitz_traits::node_id::NodeId>,
) -> Vec<ImportSnapshot> {
    children
        .into_iter()
        .filter_map(|child| {
            import_snapshot(
                doc,
                NodeId {
                    document,
                    node: child,
                },
                true,
            )
        })
        .collect()
}

pub(crate) fn import_snapshot(
    doc: &BlitzDocument,
    id: NodeId,
    deep: bool,
) -> Option<ImportSnapshot> {
    let node = doc.base.get_node(id.node)?;
    match &node.data {
        NodeData::Element(element) => {
            let name = element.name.clone();
            let attributes = element.attrs.iter().cloned().collect();
            // https://html.spec.whatwg.org/multipage/scripting.html#the-template-element
            let children = if deep {
                if crate::dom::tree::is_template_tag(&name) {
                    doc.base
                        .get_node(id.node)
                        .and_then(|node| node.data.downcast_element())
                        .and_then(|element| element.template_contents)
                        .map(|fragment| {
                            doc.base.get_node(fragment).map_or_default(|contents| {
                                contents.children.iter().copied().collect::<Vec<_>>()
                            })
                        })
                        .map_or_default(|child_ids| snapshot_children(doc, id.document, child_ids))
                } else {
                    snapshot_children(doc, id.document, node.children.iter().copied())
                }
            } else {
                Vec::new()
            };
            Some(ImportSnapshot::Element {
                name,
                attributes,
                form_state: element.form_state.clone(),
                children,
            })
        }
        NodeData::Text(_) => Some(ImportSnapshot::Text(doc.character_data(id.node))),
        NodeData::Comment { .. } => Some(ImportSnapshot::Comment(doc.character_data(id.node))),
        NodeData::Doctype {
            name,
            public_id,
            system_id,
        } => Some(ImportSnapshot::DocumentType {
            name: name.clone(),
            public_id: public_id.clone(),
            system_id: system_id.clone(),
        }),
        NodeData::ProcessingInstruction {
            target, attributes, ..
        } => Some(ImportSnapshot::ProcessingInstruction {
            target: target.clone(),
            data: doc.character_data(id.node),
            attributes: attributes.clone(),
        }),
        NodeData::CDataSection { .. } => Some(ImportSnapshot::CData(doc.character_data(id.node))),
        NodeData::AnonymousBlock(_) | NodeData::Fragment { .. } => {
            let children = if deep {
                snapshot_children(doc, id.document, node.children.iter().copied())
            } else {
                Vec::new()
            };
            Some(ImportSnapshot::Fragment(children))
        }
        // https://dom.spec.whatwg.org/#dom-node-clonenode
        NodeData::ShadowRoot { .. } | NodeData::Document(_) => None,
    }
}

pub(crate) fn materialize_import(
    doc: &mut BlitzDocument,
    document: u32,
    snapshot: &ImportSnapshot,
) -> std::result::Result<NodeId, TreeError> {
    match snapshot {
        ImportSnapshot::Element {
            name,
            attributes,
            form_state,
            children,
        } => {
            let blitz_id = doc
                .base
                .mutate()
                .create_element(name.clone(), attributes.clone());
            if let Some(element) = doc
                .base
                .get_node_mut(blitz_id)
                .and_then(|node| node.element_data_mut())
            {
                element.adopt_cloned_state(form_state.clone());
            }
            // https://html.spec.whatwg.org/multipage/input.html#the-input-element
            crate::dom::input::sync_sanitized_value(doc, blitz_id);
            let parent = if crate::dom::tree::is_template_tag(name) {
                doc.base.mutate().template_contents(blitz_id)
            } else {
                blitz_id
            };
            for child in children {
                let child_id = materialize_import(doc, document, child)?;
                doc.base.mutate().append_children(parent, &[child_id.node]);
            }
            Ok(NodeId {
                document,
                node: blitz_id,
            })
        }
        ImportSnapshot::Text(data) => {
            let blitz_id = doc.create_text(data);
            Ok(NodeId {
                document,
                node: blitz_id,
            })
        }
        ImportSnapshot::Comment(data) => {
            let blitz_id = doc.create_comment(data);
            Ok(NodeId {
                document,
                node: blitz_id,
            })
        }
        ImportSnapshot::DocumentType {
            name,
            public_id,
            system_id,
        } => {
            let blitz_id = doc.create_doctype(name, public_id, system_id);
            Ok(NodeId {
                document,
                node: blitz_id,
            })
        }
        ImportSnapshot::ProcessingInstruction {
            target,
            data,
            attributes,
        } => {
            let text = data.to_string_lossy().into_owned();
            let blitz_id = doc.create_processing_instruction_with_attributes(
                target,
                &text,
                attributes.clone(),
            );
            Ok(NodeId {
                document,
                node: blitz_id,
            })
        }
        ImportSnapshot::CData(data) => {
            let text = data.to_string_lossy().into_owned();
            let blitz_id = doc.create_cdata_section(&text);
            Ok(NodeId {
                document,
                node: blitz_id,
            })
        }
        ImportSnapshot::Fragment(children) => materialize_children(doc, document, children),
    }
}

pub(crate) fn materialize_children(
    doc: &mut BlitzDocument,
    document: u32,
    snapshots: &[ImportSnapshot],
) -> std::result::Result<NodeId, TreeError> {
    let fragment = doc.create_fragment();
    for snapshot in snapshots {
        let child = materialize_import(doc, document, snapshot)?;
        doc.base.mutate().append_children(fragment, &[child.node]);
    }
    Ok(NodeId {
        document,
        node: fragment,
    })
}

// https://html.spec.whatwg.org/multipage/scripting.html#the-template-element
pub(crate) fn lockstep_children(
    from_base: &blitz_dom::BaseDocument,
    from: blitz_traits::node_id::NodeId,
    to_base: &blitz_dom::BaseDocument,
    to: blitz_traits::node_id::NodeId,
) -> Vec<(blitz_traits::node_id::NodeId, blitz_traits::node_id::NodeId)> {
    children_for_walk(from_base, from)
        .into_iter()
        .zip(children_for_walk(to_base, to))
        .collect()
}

fn children_for_walk(
    base: &blitz_dom::BaseDocument,
    id: blitz_traits::node_id::NodeId,
) -> Vec<blitz_traits::node_id::NodeId> {
    let template_contents = base
        .get_node(id)
        .and_then(|node| node.data.downcast_element())
        .filter(|element| crate::dom::tree::is_template_tag(&element.name))
        .and_then(|element| element.template_contents);
    match template_contents {
        Some(fragment) => base
            .get_node(fragment)
            .map_or_default(|node| node.children.to_vec()),
        None => base
            .get_node(id)
            .map_or_default(|node| node.children.to_vec()),
    }
}

pub(crate) fn detach_for_adopt(
    doc: &mut BlitzDocument,
    id: NodeId,
) -> std::result::Result<(), TreeError> {
    let Some(parent) = doc.base.get_node(id.node).and_then(|node| node.parent) else {
        return Ok(());
    };
    let (previous, next, position) = {
        let parent_node = doc.base.get_node(parent).ok_or(TreeError::Stale)?;
        let position = parent_node
            .children
            .iter()
            .position(|child| *child == id.node)
            .ok_or(TreeError::NotFound)?;
        let previous = position
            .checked_sub(1)
            .and_then(|index| parent_node.children.get(index).copied())
            .map(|node| NodeId {
                document: id.document,
                node,
            });
        let next = parent_node
            .children
            .get(position + 1)
            .copied()
            .map(|node| NodeId {
                document: id.document,
                node,
            });
        (previous, next, position)
    };
    // https://dom.spec.whatwg.org/#concept-node-adopt
    if !doc.side.ranges.is_empty() {
        let index = u32::try_from(position).unwrap_or(u32::MAX);
        let parent_id = NodeId {
            document: id.document,
            node: parent,
        };
        crate::dom::range::adjust_for_remove(&doc.base, &mut doc.side.ranges, parent_id, index, id);
    }
    doc.base.mutate().remove_node(id.node);
    doc.record(JournalEntry::ChildList {
        target: NodeId {
            document: id.document,
            node: parent,
        },
        added: Vec::new(),
        removed: vec![id],
        previous,
        next,
    });
    Ok(())
}

pub(crate) fn clone_within_document(
    doc: &mut BlitzDocument,
    doc_id: u32,
    id: NodeId,
    deep: bool,
) -> std::result::Result<NodeId, TreeError> {
    if deep {
        let cloned = doc.base.mutate().deep_clone_node(id.node);
        doc.copy_script_started(id.node, cloned);
        return Ok(NodeId {
            document: doc_id,
            node: cloned,
        });
    }
    let snapshot = import_snapshot(doc, id, false).ok_or(TreeError::Hierarchy)?;
    let fresh = materialize_import(doc, doc_id, &snapshot)?;
    // https://html.spec.whatwg.org/multipage/scripting.html#script-processing-model:cloning-steps
    doc.copy_script_started(id.node, fresh.node);
    Ok(fresh)
}

pub(crate) fn adopt_across_store(
    store: &mut DocumentStore,
    parent: NodeId,
    node: NodeId,
) -> Result<NodeId, TreeError> {
    if node.document == parent.document {
        return Ok(node);
    }
    let snapshot = store
        .get(node.document)
        .and_then(|parsed| import_snapshot(&parsed.document, node, true))
        .ok_or(TreeError::Hierarchy)?;
    store
        .get_mut(node.document)
        .ok_or(TreeError::Stale)
        .and_then(|parsed| detach_for_adopt(&mut parsed.document, node))?;
    let fresh = {
        let target = store.get_mut(parent.document).ok_or(TreeError::Stale)?;
        materialize_import(&mut target.document, parent.document, &snapshot)?
    };
    let to_mark = {
        let source = store.get(node.document).ok_or(TreeError::Stale)?;
        let target = store.get(parent.document).ok_or(TreeError::Stale)?;
        let mut to_mark = Vec::new();
        let mut stack = vec![(node.node, fresh.node)];
        while let Some((from, to)) = stack.pop() {
            if source.document.script_already_started(from) {
                to_mark.push(to);
            }
            // https://html.spec.whatwg.org/multipage/scripting.html#the-template-element
            stack.extend(lockstep_children(
                &source.document.base,
                from,
                &target.document.base,
                to,
            ));
        }
        to_mark
    };
    if !to_mark.is_empty() {
        let target = store.get_mut(parent.document).ok_or(TreeError::Stale)?;
        for id in to_mark {
            target.document.mark_script_started(id);
        }
    }
    Ok(fresh)
}
