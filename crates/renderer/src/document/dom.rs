// https://chromedevtools.github.io/devtools-protocol/tot/DOM/

use std::collections::HashMap;

use blitz_dom::NodeData;
use markup5ever::interface::QuirksMode;

use super::Document;
use crate::dom::serialization::{
    HtmlOutput, serialize_html_children, serialize_html_outer, serialize_xml_children,
    serialize_xml_element, splice_before_close_tag,
};
use crate::dom::tree::{NodeId, qualified_name};
use crate::dom_string::DomString;
use crate::js::World;
use crate::{DomBoxModel, DomNode, DomReply, DomRequest, DomStyleSheet, Parsed, TabError};

// https://html.spec.whatwg.org/multipage/parsing.html#serialising-html-fragments
pub(crate) fn element_outer_html(world: &World, node: NodeId) -> Option<DomString> {
    let parsed = world.document(node)?;
    let element = parsed
        .document
        .base
        .get_node(node.node)?
        .data
        .downcast_element()?;
    let name = element.name.clone();
    let attributes = element.attrs.iter().cloned().collect::<Vec<_>>();
    let contents = parsed.document.template_owner(node.node);
    let html = parsed.content_type == "text/html";
    let markup = if html {
        serialize_html_outer(&parsed.document, node.node, &name, &attributes)
    } else {
        let mut output = HtmlOutput::default();
        serialize_xml_element(&parsed.document, node.node, &name, &attributes, &mut output);
        output.finish()
    };
    let Some((document, fragment)) = contents else {
        return Some(markup);
    };
    let contents_id = NodeId {
        document,
        node: fragment,
    };
    let children = world
        .with_document(contents_id, |parsed| {
            if html {
                serialize_html_children(&parsed.document, fragment)
            } else {
                serialize_xml_children(&parsed.document, fragment)
            }
        })
        .unwrap_or_default();
    Some(splice_before_close_tag(markup, &children))
}

impl Document {
    pub(crate) fn dom(&mut self, request: &DomRequest) -> Result<DomReply, TabError> {
        match request {
            DomRequest::GetDocument => self.dom_document(),
            DomRequest::QuerySelector { node, selector } => self.dom_query(*node, selector, false),
            DomRequest::QuerySelectorAll { node, selector } => {
                self.dom_query(*node, selector, true)
            }
            DomRequest::DescribeNode { node } => Ok(DomReply::Described {
                node: self.dom_describe(*node),
            }),
            DomRequest::GetOuterHtml { node } => self.dom_outer_html(*node),
            DomRequest::GetAttributes { node } => self.dom_attributes(*node),
            DomRequest::ResolveNode { node } => self.dom_resolve(*node),
            DomRequest::ResolveObject { object_id } => self.dom_resolve_object(object_id),
            DomRequest::GetNodeForLocation { x, y } => Ok(self.dom_node_for_location(*x, *y)),
            DomRequest::GetBoxModel { node } => Ok(self.dom_box_model(*node)),
            DomRequest::GetContentQuads { node } => Ok(self.dom_content_quads(*node)),
            DomRequest::GetComputedStyle { node } => Ok(self.dom_computed_style(*node)),
            DomRequest::GetStyleSheets => Ok(self.dom_style_sheets()),
            DomRequest::GetStorageKey => Ok(self.dom_storage_key()),
            DomRequest::GetFullAxTree => self.dom_full_ax_tree(),
        }
    }

    fn node_by_remote(&self, remote: u64) -> Option<NodeId> {
        self.world.borrow().node_for_remote(remote)
    }

    fn target_node(&self, remote: u64) -> Option<NodeId> {
        if remote == 0 {
            self.world.borrow().main_document_root()
        } else {
            self.node_by_remote(remote)
        }
    }

    fn remote_id(&self, node: NodeId) -> u64 {
        self.world.borrow_mut().remote_id(node)
    }

    fn missing_node() -> TabError {
        TabError::Dom {
            message: "Could not find node with given id".to_owned(),
        }
    }

    fn dom_document(&self) -> Result<DomReply, TabError> {
        let Some(root) = self.world.borrow().main_document_root() else {
            return Err(TabError::Dom {
                message: "no document".to_owned(),
            });
        };
        let mut nodes = Vec::new();
        {
            let world = self.world.borrow();
            world
                .with_document(root, |parsed| collect_nodes(parsed, root, &mut nodes))
                .ok_or_else(|| TabError::Dom {
                    message: "no document".to_owned(),
                })?;
        }
        let mut ids = HashMap::with_capacity(nodes.len());
        {
            let mut world = self.world.borrow_mut();
            for node in nodes {
                ids.insert(node, world.remote_id(node));
            }
        }
        let world = self.world.borrow();
        let root = world
            .with_document(root, |parsed| dom_node(&world, parsed, root, &ids))
            .flatten()
            .ok_or_else(|| TabError::Dom {
                message: "no document".to_owned(),
            })?;
        Ok(DomReply::Document { root })
    }

    fn dom_query(&self, scope: u64, selector: &str, all: bool) -> Result<DomReply, TabError> {
        let Some(scope) = self.node_by_remote(scope) else {
            return Ok(if all {
                DomReply::NodeIds {
                    node_ids: Vec::new(),
                }
            } else {
                DomReply::NodeId { node_id: 0 }
            });
        };
        let selector = crate::js::selector_matching_elements(selector);
        if all {
            let found = {
                let world = self.world.borrow();
                world.with_document(scope, |parsed| {
                    parsed
                        .document
                        .base
                        .query_selector_all_in(scope.node, &selector)
                        .map(|nodes| {
                            nodes
                                .into_iter()
                                .map(|node| NodeId {
                                    document: scope.document,
                                    node,
                                })
                                .collect::<Vec<_>>()
                        })
                        .map_err(|_error| TabError::Dom {
                            message: "Invalid selector".to_owned(),
                        })
                })
            };
            let Some(found) = found else {
                return Ok(DomReply::NodeIds {
                    node_ids: Vec::new(),
                });
            };
            Ok(DomReply::NodeIds {
                node_ids: found?
                    .into_iter()
                    .map(|node| self.remote_id(node))
                    .collect(),
            })
        } else {
            let found = {
                let world = self.world.borrow();
                world.with_document(scope, |parsed| {
                    parsed
                        .document
                        .base
                        .query_selector_in(scope.node, &selector)
                        .map(|node| {
                            node.map(|node| NodeId {
                                document: scope.document,
                                node,
                            })
                        })
                        .map_err(|_error| TabError::Dom {
                            message: "Invalid selector".to_owned(),
                        })
                })
            };
            let Some(found) = found else {
                return Ok(DomReply::NodeId { node_id: 0 });
            };
            Ok(DomReply::NodeId {
                node_id: found?.map_or(0, |node| self.remote_id(node)),
            })
        }
    }

    fn dom_describe(&self, remote: u64) -> Option<DomNode> {
        let node = self.target_node(remote)?;
        let id = self.remote_id(node);
        let world = self.world.borrow();
        world
            .with_document(node, |parsed| describe_node(parsed, node, id))
            .flatten()
    }

    fn dom_outer_html(&self, remote: u64) -> Result<DomReply, TabError> {
        let node = self.node_by_remote(remote).ok_or_else(Self::missing_node)?;
        let element = {
            let world = self.world.borrow();
            world
                .with_document(node, |parsed| {
                    let base = &parsed.document.base;
                    match &base.get_node(node.node)?.data {
                        NodeData::Document(_) => Some(
                            base.get_node(node.node)?
                                .children
                                .iter()
                                .find(|child| {
                                    base.get_node(**child)
                                        .is_some_and(|node| node.data.downcast_element().is_some())
                                })
                                .map(|child| NodeId {
                                    document: node.document,
                                    node: *child,
                                }),
                        ),
                        NodeData::Element(_) => Some(Some(node)),
                        _ => Some(None),
                    }
                })
                .ok_or_else(Self::missing_node)?
        };
        let Some(Some(element)) = element else {
            let is_document = {
                let world = self.world.borrow();
                world
                    .with_document(node, |parsed| {
                        parsed
                            .document
                            .base
                            .get_node(node.node)
                            .is_some_and(|node| matches!(node.data, NodeData::Document(_)))
                    })
                    .unwrap_or(false)
            };
            if is_document {
                return Ok(DomReply::OuterHtml {
                    outer_html: String::new(),
                });
            }
            return Err(TabError::Dom {
                message: "Node does not support outerHTML".to_owned(),
            });
        };
        let markup = {
            let world = self.world.borrow();
            element_outer_html(&world, element)
        }
        .ok_or_else(|| TabError::Dom {
            message: "Node does not support outerHTML".to_owned(),
        })?;
        Ok(DomReply::OuterHtml {
            outer_html: markup.to_string_lossy().into_owned(),
        })
    }

    fn dom_attributes(&self, remote: u64) -> Result<DomReply, TabError> {
        let node = self.node_by_remote(remote).ok_or_else(Self::missing_node)?;
        let world = self.world.borrow();
        let element = world
            .with_document(node, |parsed| {
                parsed
                    .document
                    .base
                    .get_node(node.node)
                    .map(|node| node.data.downcast_element().map(element_attributes))
            })
            .flatten()
            .ok_or_else(Self::missing_node)?;
        element.map_or_else(
            || {
                Err(TabError::Dom {
                    message: "Node is not an Element".to_owned(),
                })
            },
            |attributes| Ok(DomReply::Attributes { attributes }),
        )
    }

    fn dom_resolve(&self, remote: u64) -> Result<DomReply, TabError> {
        let Some(node) = self.target_node(remote) else {
            return Ok(DomReply::Resolved { object: None });
        };
        let Some(realm) = self.js.as_ref() else {
            return Ok(DomReply::Resolved { object: None });
        };
        let object = realm.intern_node(node).map_err(|error| TabError::Dom {
            message: error.to_string(),
        })?;
        Ok(DomReply::Resolved {
            object: Some(object),
        })
    }

    fn dom_resolve_object(&self, object_id: &str) -> Result<DomReply, TabError> {
        let Some(realm) = self.js.as_ref() else {
            return Ok(DomReply::NodeId { node_id: 0 });
        };
        let node = realm
            .node_for_handle(object_id)
            .map_err(|error| TabError::Dom {
                message: error.to_string(),
            })?;
        Ok(DomReply::NodeId {
            node_id: node.map_or(0, |node| self.remote_id(node)),
        })
    }

    fn dom_node_for_location(&self, x: f64, y: f64) -> DomReply {
        let node = {
            let world = self.world.borrow();
            let Some(root) = world.main_document_root() else {
                return DomReply::NodeForLocation { node_id: 0 };
            };
            let (scroll_x, scroll_y) = crate::js::viewport_scroll(&world, root);
            crate::js::element_at_point(&world, root, x - scroll_x, y - scroll_y)
        };
        DomReply::NodeForLocation {
            node_id: node.map_or(0, |node| self.remote_id(node)),
        }
    }

    fn dom_box_model(&self, remote: u64) -> DomReply {
        let Some(node) = self.target_node(remote) else {
            return DomReply::BoxModel { model: None };
        };
        let world = self.world.borrow();
        let Some((left, top, width, height)) = crate::js::element_box(&world, node) else {
            return DomReply::BoxModel { model: None };
        };
        let (scroll_x, scroll_y) = crate::js::viewport_scroll(&world, node);
        let left = left - scroll_x;
        let top = top - scroll_y;
        let right = left + width;
        let bottom = top + height;
        let quad = [left, top, right, top, right, bottom, left, bottom];
        DomReply::BoxModel {
            model: Some(DomBoxModel {
                content: quad,
                padding: quad,
                border: quad,
                margin: quad,
                width: round_extent(width),
                height: round_extent(height),
            }),
        }
    }

    fn dom_content_quads(&self, remote: u64) -> DomReply {
        let Some(node) = self.target_node(remote) else {
            return DomReply::ContentQuads { quads: Vec::new() };
        };
        let world = self.world.borrow();
        let Some((left, top, width, height)) = crate::js::element_box(&world, node) else {
            return DomReply::ContentQuads { quads: Vec::new() };
        };
        let (scroll_x, scroll_y) = crate::js::viewport_scroll(&world, node);
        let left = left - scroll_x;
        let top = top - scroll_y;
        let right = left + width;
        let bottom = top + height;
        DomReply::ContentQuads {
            quads: vec![[left, top, right, top, right, bottom, left, bottom]],
        }
    }

    // https://chromedevtools.github.io/devtools-protocol/tot/CSS/#method-getComputedStyleForNode
    fn dom_computed_style(&self, remote: u64) -> DomReply {
        let Some(node) = self.node_by_remote(remote) else {
            return DomReply::ComputedStyle {
                properties: Vec::new(),
            };
        };
        let world = self.world.borrow();
        let Some(mut parsed) = world.document_mut(node) else {
            return DomReply::ComputedStyle {
                properties: Vec::new(),
            };
        };
        parsed.document.base.resolve(crate::animation_seconds());
        let names = parsed
            .document
            .base
            .computed_style_property_names(node.node, None);
        let properties = names
            .into_iter()
            .map(|name| {
                let value = parsed.document.base.resolved_style_value(node.node, &name);
                (name, value)
            })
            .collect();
        DomReply::ComputedStyle { properties }
    }

    // https://chromedevtools.github.io/devtools-protocol/tot/CSS/#event-styleSheetAdded
    fn dom_style_sheets(&self) -> DomReply {
        let world = self.world.borrow();
        let Some(parsed) = world.main_document() else {
            return DomReply::StyleSheets { sheets: Vec::new() };
        };
        let base = &parsed.document.base;
        let base_url = world.main_base_url();
        let sheets = base
            .stylesheet_owner_nodes()
            .filter_map(|owner| {
                let sheet_id = base.node_cssom_sheet_id(owner)?;
                let href = crate::dom::tree::attr(base, owner, "href").map(|raw| {
                    base_url
                        .join(raw)
                        .map_or_else(|_| raw.to_owned(), |url| url.to_string())
                });
                let title = crate::dom::tree::attr(base, owner, "title")
                    .unwrap_or_default()
                    .to_owned();
                let disabled = base.cssom_sheet_disabled(sheet_id).unwrap_or(false);
                let length = base.cssom_rule_count(sheet_id, &[]).unwrap_or(0);
                Some(DomStyleSheet {
                    style_sheet_id: sheet_id.to_string(),
                    is_inline: href.is_none(),
                    source_url: href.unwrap_or_default(),
                    title,
                    disabled,
                    length,
                })
            })
            .collect();
        DomReply::StyleSheets { sheets }
    }

    // https://chromedevtools.github.io/devtools-protocol/tot/Storage/#method-getStorageKey
    fn dom_storage_key(&self) -> DomReply {
        let key = self.world.borrow().origin.ascii_serialization();
        DomReply::StorageKey { key }
    }

    // https://chromedevtools.github.io/devtools-protocol/tot/Accessibility/#method-getFullAXTree
    fn dom_full_ax_tree(&self) -> Result<DomReply, TabError> {
        let Some(root) = self.world.borrow().main_document_root() else {
            return Err(TabError::Dom {
                message: "no document".to_owned(),
            });
        };
        let mut nodes = Vec::new();
        {
            let world = self.world.borrow();
            if let Some(mut parsed) = world.document_mut(root) {
                parsed.document.base.resolve(crate::animation_seconds());
            }
            world
                .with_document(root, |parsed| collect_nodes(parsed, root, &mut nodes))
                .ok_or_else(|| TabError::Dom {
                    message: "no document".to_owned(),
                })?;
        }
        let mut ids = HashMap::with_capacity(nodes.len());
        {
            let mut world = self.world.borrow_mut();
            for node in nodes {
                ids.insert(node, world.remote_id(node));
            }
        }
        let world = self.world.borrow();
        let focused = world.active_element(root.document);
        let ax_nodes = world
            .with_document(root, |parsed| {
                crate::dom::accessibility::build_ax_tree(&parsed.document.base, root, &ids, focused)
            })
            .ok_or_else(|| TabError::Dom {
                message: "no document".to_owned(),
            })?;
        Ok(DomReply::AxTree { nodes: ax_nodes })
    }
}

fn collect_nodes(parsed: &Parsed, root: NodeId, out: &mut Vec<NodeId>) {
    let Some(node) = parsed.document.base.get_node(root.node) else {
        return;
    };
    out.push(root);
    for child in &node.children {
        collect_nodes(
            parsed,
            NodeId {
                document: root.document,
                node: *child,
            },
            out,
        );
    }
}

fn element_attributes(element: &blitz_dom::ElementData) -> Vec<String> {
    let mut attributes = Vec::with_capacity(element.attrs().len() * 2);
    for attribute in element.attrs() {
        attributes.push(qualified_name(&attribute.name));
        attributes.push(attribute.value.clone());
    }
    attributes
}

fn round_extent(value: f64) -> i64 {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "extent rounding saturates and maps NaN to zero"
    )]
    let rounded = value.round() as i64;
    rounded
}

fn compatibility_mode(mode: QuirksMode) -> &'static str {
    match mode {
        QuirksMode::NoQuirks => "NoQuirks",
        QuirksMode::LimitedQuirks => "LimitedQuirks",
        QuirksMode::Quirks => "Quirks",
    }
}

fn dom_node(
    world: &World,
    parsed: &Parsed,
    node: NodeId,
    ids: &HashMap<NodeId, u64>,
) -> Option<DomNode> {
    let data = &parsed.document.base.get_node(node.node)?.data;
    let node_type = crate::dom::tree::node_type(Some(data))?;
    let html = parsed.content_type == "text/html";
    let node_name = crate::dom::tree::node_name(&parsed.document.base, node.node, html)?;
    let mut local_name = None;
    let mut node_value = String::new();
    let mut attributes = Vec::new();
    let mut document_url = None;
    let mut base_url = None;
    let mut document_compatibility_mode = None;
    match data {
        NodeData::Element(element) => {
            local_name = Some(element.name.local.to_string());
            attributes = element_attributes(element);
        }
        NodeData::Document(_) => {
            document_url = Some(world.document_url_of(node));
            base_url = Some(world.document_base_url_of(node));
            document_compatibility_mode = Some(compatibility_mode(parsed.quirks_mode).to_owned());
        }
        NodeData::Text(_)
        | NodeData::Comment { .. }
        | NodeData::ProcessingInstruction { .. }
        | NodeData::CDataSection { .. }
        | NodeData::Doctype { .. } => {
            node_value = parsed
                .document
                .character_data(node.node)
                .to_string_lossy()
                .into_owned();
        }
        NodeData::Fragment { .. } | NodeData::ShadowRoot { .. } | NodeData::AnonymousBlock(_) => {}
    }
    let children: Vec<DomNode> = parsed
        .document
        .base
        .get_node(node.node)?
        .children
        .iter()
        .filter_map(|child| {
            dom_node(
                world,
                parsed,
                NodeId {
                    document: node.document,
                    node: *child,
                },
                ids,
            )
        })
        .collect();
    Some(DomNode {
        node_id: ids.get(&node).copied().unwrap_or_default(),
        backend_node_id: ids.get(&node).copied().unwrap_or_default(),
        node_type,
        node_name,
        local_name,
        node_value,
        attributes,
        child_node_count: u32::try_from(children.len()).unwrap_or(u32::MAX),
        children,
        document_url,
        base_url,
        compatibility_mode: document_compatibility_mode,
    })
}

fn describe_node(parsed: &Parsed, node: NodeId, id: u64) -> Option<DomNode> {
    let data = &parsed.document.base.get_node(node.node)?.data;
    let node_type = crate::dom::tree::node_type(Some(data))?;
    let html = parsed.content_type == "text/html";
    let node_name = crate::dom::tree::node_name(&parsed.document.base, node.node, html)?;
    let (local_name, attributes) = match data {
        NodeData::Element(element) => (
            Some(element.name.local.to_string()),
            element_attributes(element),
        ),
        _ => (None, Vec::new()),
    };
    let child_node_count = parsed.document.base.get_node(node.node).map_or(0, |node| {
        u32::try_from(node.children.len()).unwrap_or(u32::MAX)
    });
    Some(DomNode {
        node_id: id,
        backend_node_id: id,
        node_type,
        node_name,
        local_name,
        node_value: String::new(),
        attributes,
        children: Vec::new(),
        child_node_count,
        document_url: None,
        base_url: None,
        compatibility_mode: None,
    })
}
