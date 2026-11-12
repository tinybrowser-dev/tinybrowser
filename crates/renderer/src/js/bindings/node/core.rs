//! Core node interfaces: `Node`, `CharacterData`, `Text`,
//! `ProcessingInstruction`, `DocumentType`, and the `ParentNode`/`ChildNode` mixins.

use super::super::{
    CollectionKind, NodeOrString, WebIdlString, adopt_across_documents, attr_owner, attr_state,
    character_data, character_data_offset, child_value, clone_document,
    convert_union_nodes_into_node, copy_nonce_subtree, deref_weak, document_is_html_content,
    dom_string, drain_mutation_journal, element_sibling_value, fixup_focus_after_removal,
    live_collection, make_weak, node_is_shadow_root, replace_character_data,
    schedule_mutation_delivery, select_error, selector_matching_elements, set_character_data,
    set_pi_attributes, sibling_value, string_value, throw_dom, throw_dom_error,
    valid_attribute_local_name, with_node_data, world, world_for_node, wrap_node,
};
use crate::dom::adoption::{clone_within_document, materialize_import};

use blitz_dom::NodeData;

use crate::dom_string::DomString;

use rquickjs::{Ctx, Exception, Function, Persistent, Result, Value};

use crate::js::world::World;

use crate::dom::equality;
use crate::dom::insertion::{
    InsertedNode, InsertionStep, MoveTarget, Replacement, classify_inserted,
    collect_html_insertion_steps, ensure_can_move, ensure_document_content_model, ensure_no_cycle,
    ensure_parented, fragment_children, iframe_containers_in_subtrees, insert_parsed, move_parsed,
    replace_all_journaled, replace_all_with_node, replace_parsed, unlink_journaled,
};
use crate::dom::tree::{
    BlitzId, NodeId, ancestor_chain, child_ids, is_connected, is_element, is_html_element, root_of,
    sibling, tree_order,
};

use crate::documents::JournalEntry;
use crate::js::world::{Handle, NodeReference, SameObject};

use super::JsNode;
use super::document::{
    apply_meta_csp, apply_meta_referrer, apply_meta_refresh, clear_nonce_on_connect,
    prepare_classic_script,
};
use super::element::{argument_node_ids, sibling_not_in, tree_parent};
use super::forms::fixup_controls_on_insert;

include!(concat!(env!("OUT_DIR"), "/Node.rs"));
include!(concat!(env!("OUT_DIR"), "/CharacterData.rs"));
include!(concat!(env!("OUT_DIR"), "/Text.rs"));
include!(concat!(env!("OUT_DIR"), "/ProcessingInstruction.rs"));
include!(concat!(env!("OUT_DIR"), "/DocumentType.rs"));
include!(concat!(env!("OUT_DIR"), "/ParentNode.rs"));
include!(concat!(env!("OUT_DIR"), "/ChildNode.rs"));
include!(concat!(env!("OUT_DIR"), "/NonDocumentTypeChildNode.rs"));

impl<'js> node_generated::Node<'js> for JsNode {
    // https://dom.spec.whatwg.org/#dom-node-nodetype
    fn get_node_type(&self, ctx: &Ctx<'js>) -> Result<u16> {
        self.node_type(ctx)
    }

    // https://dom.spec.whatwg.org/#dom-node-nodename
    fn get_node_name(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        self.node_name(ctx)
    }

    // https://dom.spec.whatwg.org/#dom-node-baseuri
    fn get_base_uri(&self, ctx: &Ctx<'js>) -> Result<crate::dom_string::DomString> {
        Ok(self.base_uri(ctx))
    }

    // https://dom.spec.whatwg.org/#dom-node-isconnected
    fn get_is_connected(&self, ctx: &Ctx<'js>) -> Result<bool> {
        self.is_connected(ctx)
    }

    // https://dom.spec.whatwg.org/#dom-node-ownerdocument
    fn get_owner_document(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        self.owner_document(ctx)
    }

    // https://dom.spec.whatwg.org/#dom-node-getrootnode
    fn get_root_node(
        &self,
        ctx: Ctx<'js>,
        arg_0: node_generated::GetRootNodeOptions,
    ) -> Result<Value<'js>> {
        self.get_root_node(ctx, arg_0)
    }

    // https://dom.spec.whatwg.org/#dom-node-parentnode
    fn get_parent_node(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        self.parent_node(ctx)
    }

    // https://dom.spec.whatwg.org/#dom-node-parentelement
    fn get_parent_element(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        self.parent_element(ctx)
    }

    // https://dom.spec.whatwg.org/#dom-node-haschildnodes
    fn has_child_nodes(&self, ctx: Ctx<'js>) -> Result<bool> {
        self.has_child_nodes(ctx)
    }

    // https://dom.spec.whatwg.org/#dom-node-childnodes
    fn get_child_nodes(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        self.child_nodes(ctx)
    }

    // https://dom.spec.whatwg.org/#dom-node-firstchild
    fn get_first_child(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        self.first_child(ctx)
    }

    // https://dom.spec.whatwg.org/#dom-node-lastchild
    fn get_last_child(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        self.last_child(ctx)
    }

    // https://dom.spec.whatwg.org/#dom-node-previoussibling
    fn get_previous_sibling(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        self.previous_sibling(ctx)
    }

    // https://dom.spec.whatwg.org/#dom-node-nextsibling
    fn get_next_sibling(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        self.next_sibling(ctx)
    }

    // https://dom.spec.whatwg.org/#dom-node-nodevalue
    fn get_node_value(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        self.node_value(ctx)
    }

    // https://dom.spec.whatwg.org/#dom-node-nodevalue
    fn set_node_value(&self, ctx: &Ctx<'js>, value: Option<rquickjs::String<'js>>) -> Result<()> {
        self.set_node_value(ctx, value)
    }

    // https://dom.spec.whatwg.org/#dom-node-textcontent
    fn get_text_content(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        self.text_content(ctx)
    }

    // https://dom.spec.whatwg.org/#dom-node-textcontent
    fn set_text_content(&self, ctx: &Ctx<'js>, value: Option<rquickjs::String<'js>>) -> Result<()> {
        self.set_text_content(ctx, value)
    }

    // https://dom.spec.whatwg.org/#dom-node-normalize
    fn normalize(&self, ctx: Ctx<'js>) -> Result<()> {
        self.normalize(ctx)
    }

    // https://dom.spec.whatwg.org/#dom-node-clonenode
    fn clone_node(&self, ctx: Ctx<'js>, arg_0: bool) -> Result<Value<'js>> {
        self.clone_node(ctx, arg_0)
    }

    // https://dom.spec.whatwg.org/#dom-node-isequalnode
    fn is_equal_node(&self, ctx: Ctx<'js>, arg_0: Option<NodeReference>) -> Result<bool> {
        self.is_equal_node(ctx, arg_0)
    }

    // https://dom.spec.whatwg.org/#dom-node-issamenode
    fn is_same_node(&self, _ctx: Ctx<'js>, arg_0: Option<NodeReference>) -> Result<bool> {
        Ok(self.is_same_node(arg_0))
    }

    // https://dom.spec.whatwg.org/#dom-node-comparedocumentposition
    fn compare_document_position(&self, ctx: Ctx<'js>, arg_0: NodeReference) -> Result<u16> {
        self.compare_document_position(ctx, arg_0)
    }

    // https://dom.spec.whatwg.org/#dom-node-contains
    fn contains(&self, ctx: Ctx<'js>, arg_0: Option<NodeReference>) -> Result<bool> {
        self.contains(ctx, arg_0)
    }

    // https://dom.spec.whatwg.org/#dom-node-lookupprefix
    fn lookup_prefix(
        &self,
        ctx: Ctx<'js>,
        arg_0: Option<rquickjs::String<'js>>,
    ) -> Result<Option<rquickjs::String<'js>>> {
        let value = self.lookup_prefix(ctx.clone(), arg_0)?;
        if value.is_null() || value.is_undefined() {
            Ok(None)
        } else {
            rquickjs::FromJs::from_js(&ctx, value).map(Some)
        }
    }

    // https://dom.spec.whatwg.org/#dom-node-lookupnamespaceuri
    fn lookup_namespace_uri(
        &self,
        ctx: Ctx<'js>,
        arg_0: Option<rquickjs::String<'js>>,
    ) -> Result<Option<rquickjs::String<'js>>> {
        let value = self.lookup_namespace_uri(ctx.clone(), arg_0)?;
        if value.is_null() || value.is_undefined() {
            Ok(None)
        } else {
            rquickjs::FromJs::from_js(&ctx, value).map(Some)
        }
    }

    // https://dom.spec.whatwg.org/#dom-node-isdefaultnamespace
    fn is_default_namespace(
        &self,
        ctx: Ctx<'js>,
        arg_0: Option<rquickjs::String<'js>>,
    ) -> Result<bool> {
        self.is_default_namespace(ctx, arg_0)
    }

    // https://dom.spec.whatwg.org/#dom-node-insertbefore
    fn insert_before(
        &self,
        ctx: Ctx<'js>,
        arg_0: NodeReference,
        arg_1: Option<NodeReference>,
    ) -> Result<Value<'js>> {
        self.insert_before(ctx, arg_0, arg_1)
    }

    // https://dom.spec.whatwg.org/#dom-node-appendchild
    fn append_child(&self, ctx: Ctx<'js>, arg_0: NodeReference) -> Result<Value<'js>> {
        self.append_child(ctx, arg_0)
    }

    // https://dom.spec.whatwg.org/#dom-node-replacechild
    fn replace_child(
        &self,
        ctx: Ctx<'js>,
        arg_0: NodeReference,
        arg_1: NodeReference,
    ) -> Result<Value<'js>> {
        self.replace_child(ctx, arg_0, arg_1)
    }

    // https://dom.spec.whatwg.org/#dom-node-removechild
    fn remove_child(&self, ctx: Ctx<'js>, arg_0: NodeReference) -> Result<Value<'js>> {
        self.remove_child(ctx, arg_0)
    }
}

impl<'js> character_data_generated::CharacterData<'js> for JsNode {
    // https://dom.spec.whatwg.org/#dom-characterdata-data
    fn get_data(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        dom_string(ctx, &character_data(ctx, self.handle.get().0)?)
    }

    // https://dom.spec.whatwg.org/#dom-characterdata-data
    fn set_data(&self, ctx: &Ctx<'js>, value: rquickjs::String<'js>) -> Result<()> {
        let data = crate::dom_string::DomString::from_utf16(value.to_utf16()?);
        set_character_data(ctx, self.handle.get().0, &data)
    }

    // https://dom.spec.whatwg.org/#dom-characterdata-length
    fn get_length(&self, ctx: &Ctx<'js>) -> Result<usize> {
        Ok(character_data(ctx, self.handle.get().0)?.len())
    }

    // https://dom.spec.whatwg.org/#dom-characterdata-substringdata
    fn substring_data(
        &self,
        ctx: Ctx<'js>,
        offset: u32,
        count: u32,
    ) -> Result<rquickjs::String<'js>> {
        let units = character_data(&ctx, self.handle.get().0)?
            .units()
            .into_owned();
        let offset = character_data_offset(&ctx, offset, units.len())?;
        let end = offset
            .saturating_add(usize::try_from(count).unwrap_or(usize::MAX))
            .min(units.len());
        rquickjs::String::from_utf16(ctx.clone(), &units[offset..end])
    }

    // https://dom.spec.whatwg.org/#dom-characterdata-appenddata
    fn append_data(&self, ctx: Ctx<'js>, data: rquickjs::String<'js>) -> Result<()> {
        let length = character_data(&ctx, self.handle.get().0)?.units().len();
        replace_character_data(
            &ctx,
            self.handle.get().0,
            u32::try_from(length).unwrap_or(u32::MAX),
            0,
            &crate::dom_string::DomString::from_utf16(data.to_utf16()?),
        )
    }

    // https://dom.spec.whatwg.org/#dom-characterdata-insertdata
    fn insert_data(&self, ctx: Ctx<'js>, offset: u32, data: rquickjs::String<'js>) -> Result<()> {
        let length = character_data(&ctx, self.handle.get().0)?.units().len();
        character_data_offset(&ctx, offset, length)?;
        replace_character_data(
            &ctx,
            self.handle.get().0,
            offset,
            0,
            &crate::dom_string::DomString::from_utf16(data.to_utf16()?),
        )
    }

    // https://dom.spec.whatwg.org/#dom-characterdata-deletedata
    fn delete_data(&self, ctx: Ctx<'js>, offset: u32, count: u32) -> Result<()> {
        let length = character_data(&ctx, self.handle.get().0)?.units().len();
        character_data_offset(&ctx, offset, length)?;
        replace_character_data(
            &ctx,
            self.handle.get().0,
            offset,
            count,
            &crate::dom_string::DomString::default(),
        )
    }

    // https://dom.spec.whatwg.org/#dom-characterdata-replacedata
    fn replace_data(
        &self,
        ctx: Ctx<'js>,
        offset: u32,
        count: u32,
        data: rquickjs::String<'js>,
    ) -> Result<()> {
        let length = character_data(&ctx, self.handle.get().0)?.units().len();
        character_data_offset(&ctx, offset, length)?;
        replace_character_data(
            &ctx,
            self.handle.get().0,
            offset,
            count,
            &crate::dom_string::DomString::from_utf16(data.to_utf16()?),
        )
    }
}

impl<'js> text_generated::Text<'js> for JsNode {
    /// <https://dom.spec.whatwg.org/#dom-text-splittext>
    fn split_text(&self, ctx: Ctx<'js>, offset: u32) -> Result<Value<'js>> {
        split_text_node(&ctx, self.handle.get().0, offset)
    }

    /// <https://dom.spec.whatwg.org/#dom-text-wholetext>
    fn get_whole_text(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        dom_string(ctx, &whole_text(ctx, self.handle.get().0)?)
    }
}

impl<'js> processing_instruction_generated::ProcessingInstruction<'js> for JsNode {
    /// <https://dom.spec.whatwg.org/#dom-processinginstruction-target>
    fn get_target(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        let target = pi_target(ctx, self.handle.get().0);
        rquickjs::String::from_str(ctx.clone(), &target)
    }

    /// <https://dom.spec.whatwg.org/#dom-processinginstruction-hasattributes>
    fn has_attributes(&self, ctx: Ctx<'js>) -> Result<bool> {
        Ok(!pi_attributes(&ctx, self.handle.get().0)?.is_empty())
    }

    /// <https://dom.spec.whatwg.org/#dom-processinginstruction-getattributenames>
    fn get_attribute_names(&self, ctx: Ctx<'js>) -> Result<Vec<String>> {
        Ok(pi_attributes(&ctx, self.handle.get().0)?
            .into_iter()
            .map(|(name, _)| name)
            .collect())
    }

    /// <https://dom.spec.whatwg.org/#dom-processinginstruction-getattribute>
    fn get_attribute(
        &self,
        ctx: Ctx<'js>,
        arg_0: rquickjs::String<'js>,
    ) -> Result<Option<rquickjs::String<'js>>> {
        let name = arg_0.to_string()?;
        match pi_attributes(&ctx, self.handle.get().0)?
            .into_iter()
            .find(|(attribute, _)| attribute == &name)
        {
            Some((_, value)) => rquickjs::String::from_str(ctx, &value).map(Some),
            None => Ok(None),
        }
    }

    /// <https://dom.spec.whatwg.org/#dom-processinginstruction-setattribute>
    fn set_attribute(
        &self,
        ctx: Ctx<'js>,
        arg_0: rquickjs::String<'js>,
        arg_1: rquickjs::String<'js>,
    ) -> Result<()> {
        let name = arg_0.to_string()?;
        if !valid_attribute_local_name(&name) {
            return Err(throw_dom(
                &ctx,
                "InvalidCharacterError",
                "attribute name is not a valid attribute local name",
            ));
        }
        let value = arg_1.to_string()?;
        let mut attributes = pi_attributes(&ctx, self.handle.get().0)?;
        if let Some((_, stored)) = attributes
            .iter_mut()
            .find(|(attribute, _)| attribute == &name)
        {
            *stored = value;
        } else {
            attributes.push((name, value));
        }
        set_pi_attributes(&ctx, self.handle.get().0, attributes)
    }

    /// <https://dom.spec.whatwg.org/#dom-processinginstruction-removeattribute>
    fn remove_attribute(&self, ctx: Ctx<'js>, arg_0: rquickjs::String<'js>) -> Result<()> {
        let name = arg_0.to_string()?;
        let mut attributes = pi_attributes(&ctx, self.handle.get().0)?;
        attributes.retain(|(attribute, _)| attribute != &name);
        set_pi_attributes(&ctx, self.handle.get().0, attributes)
    }

    /// <https://dom.spec.whatwg.org/#dom-processinginstruction-toggleattribute>
    fn toggle_attribute(
        &self,
        ctx: Ctx<'js>,
        arg_0: rquickjs::String<'js>,
        arg_1: Option<bool>,
    ) -> Result<bool> {
        let name = arg_0.to_string()?;
        if !valid_attribute_local_name(&name) {
            return Err(throw_dom(
                &ctx,
                "InvalidCharacterError",
                "attribute name is not a valid attribute local name",
            ));
        }
        let mut attributes = pi_attributes(&ctx, self.handle.get().0)?;
        let exists = attributes.iter().any(|(attribute, _)| attribute == &name);
        if !exists {
            if arg_1.unwrap_or(true) {
                attributes.push((name, String::new()));
                set_pi_attributes(&ctx, self.handle.get().0, attributes)?;
                return Ok(true);
            }
            return Ok(false);
        }
        if arg_1 == Some(true) {
            // Present and force is true: the value and the data stay as they are
            // (<https://dom.spec.whatwg.org/#dom-processinginstruction-toggleattribute>).
            return Ok(true);
        }
        attributes.retain(|(attribute, _)| attribute != &name);
        set_pi_attributes(&ctx, self.handle.get().0, attributes)?;
        Ok(false)
    }

    /// <https://dom.spec.whatwg.org/#dom-processinginstruction-hasattribute>
    fn has_attribute(&self, ctx: Ctx<'js>, arg_0: rquickjs::String<'js>) -> Result<bool> {
        let name = arg_0.to_string()?;
        Ok(pi_attributes(&ctx, self.handle.get().0)?
            .iter()
            .any(|(attribute, _)| attribute == &name))
    }
}

impl<'js> document_type_generated::DocumentType<'js> for JsNode {
    /// <https://dom.spec.whatwg.org/#dom-documenttype-name>
    fn get_name(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        let name = doctype_field(ctx, self.handle.get().0, |name, _, _| name.to_owned());
        rquickjs::String::from_str(ctx.clone(), &name)
    }

    /// <https://dom.spec.whatwg.org/#dom-documenttype-publicid>
    fn get_public_id(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        let public_id = doctype_field(ctx, self.handle.get().0, |_, public_id, _| {
            public_id.to_owned()
        });
        rquickjs::String::from_str(ctx.clone(), &public_id)
    }

    /// <https://dom.spec.whatwg.org/#dom-documenttype-systemid>
    fn get_system_id(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        let system_id = doctype_field(ctx, self.handle.get().0, |_, _, system_id| {
            system_id.to_owned()
        });
        rquickjs::String::from_str(ctx.clone(), &system_id)
    }
}

impl<'js> parent_node_generated::ParentNode<'js> for JsNode {
    // https://dom.spec.whatwg.org/#dom-parentnode-children
    fn get_children(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        live_collection(
            ctx,
            self.handle.get().0,
            CollectionKind::ElementChildren,
            Some("HTMLCollection"),
        )
    }

    // https://dom.spec.whatwg.org/#dom-parentnode-firstelementchild
    fn get_first_element_child(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        let world = world(ctx)?;
        let found = {
            let parsed = world.borrow();
            let Some(parsed) = parsed.document(self.handle.get().0) else {
                return Ok(Value::new_null(ctx.clone()));
            };
            let base = &parsed.document.base;
            base.get_node(self.handle.get().0.node).and_then(|node| {
                node.children
                    .iter()
                    .find(|kid| is_element(base, **kid))
                    .map(|kid| NodeId {
                        document: parsed.id,
                        node: *kid,
                    })
            })
        };
        child_value(ctx, found)
    }

    // https://dom.spec.whatwg.org/#dom-parentnode-lastelementchild
    fn get_last_element_child(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        let world = world(ctx)?;
        let found = {
            let parsed = world.borrow();
            let Some(parsed) = parsed.document(self.handle.get().0) else {
                return Ok(Value::new_null(ctx.clone()));
            };
            let base = &parsed.document.base;
            base.get_node(self.handle.get().0.node).and_then(|node| {
                node.children
                    .iter()
                    .rev()
                    .find(|kid| is_element(base, **kid))
                    .map(|kid| NodeId {
                        document: parsed.id,
                        node: *kid,
                    })
            })
        };
        child_value(ctx, found)
    }

    // https://dom.spec.whatwg.org/#dom-parentnode-childelementcount
    fn get_child_element_count(&self, ctx: &Ctx<'js>) -> Result<usize> {
        let world = world(ctx)?;
        let parsed = world.borrow();
        let Some(parsed) = parsed.document(self.handle.get().0) else {
            return Ok(0);
        };
        let base = &parsed.document.base;
        Ok(base.get_node(self.handle.get().0.node).map_or(0, |node| {
            node.children
                .iter()
                .filter(|kid| is_element(base, **kid))
                .count()
        }))
    }

    // https://dom.spec.whatwg.org/#dom-parentnode-append
    fn append(
        &self,
        ctx: Ctx<'js>,
        nodes: Vec<parent_node_generated::DOMStringOrNode<'js>>,
    ) -> Result<()> {
        let node = convert_union_nodes_into_node(&ctx, self.handle.get().0, union_nodes(nodes))?;
        let (node, _) =
            insertion_tree_nodes(&ctx, self.handle.get().0, NodeReference::Tree(node), None)?;
        insert_tree_node(&ctx, self.handle.get().0, node, None)
    }

    // https://dom.spec.whatwg.org/#dom-parentnode-prepend
    fn prepend(
        &self,
        ctx: Ctx<'js>,
        nodes: Vec<parent_node_generated::DOMStringOrNode<'js>>,
    ) -> Result<()> {
        // Adoption by copy leaves a stale handle behind; prepend to the live
        // parent (https://dom.spec.whatwg.org/#concept-node-adopt).
        let parent = self.handle.get().0;
        let node = convert_union_nodes_into_node(&ctx, parent, union_nodes(nodes))?;
        let reference = {
            let world = world(&ctx)?;
            let world = world.borrow();
            let Some(parsed) = world.document(parent) else {
                return Err(Exception::throw_type(&ctx, "no document"));
            };
            parsed
                .document
                .base
                .get_node(parent.node)
                .and_then(|parent| parent.children.first().copied())
                .map(|node| NodeId {
                    document: parent.document,
                    node,
                })
        };
        let (node, reference) = insertion_tree_nodes(
            &ctx,
            parent,
            NodeReference::Tree(node),
            reference.map(NodeReference::Tree),
        )?;
        insert_tree_node(&ctx, parent, node, reference)
    }

    // https://dom.spec.whatwg.org/#dom-parentnode-replacechildren
    fn replace_children(
        &self,
        ctx: Ctx<'js>,
        nodes: Vec<parent_node_generated::DOMStringOrNode<'js>>,
    ) -> Result<()> {
        // Convert nodes first: append to a fragment records each removal from
        // a previous parent. Then ensure pre-insert validity with this node's
        // current children excluded, then replace all.
        // https://dom.spec.whatwg.org/#convert-nodes-into-a-node
        // https://github.com/whatwg/dom/issues/1045
        let parent = self.handle.get().0;
        let node = convert_union_nodes_into_node(&ctx, parent, union_nodes(nodes))?;
        let excluded = {
            let world = world(&ctx)?;
            world
                .borrow()
                .with_document(parent, |parsed| {
                    child_ids(&parsed.document.base, parent.document, parent.node)
                        .into_iter()
                        .map(|child| child.node)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        let (node, _) =
            insertion_excluding_children(&ctx, parent, NodeReference::Tree(node), None, &excluded)?;
        let world = world(&ctx)?;
        let world = world.borrow();
        let Some(mut parsed) = world.document_mut(parent) else {
            return Err(Exception::throw_type(&ctx, "no document"));
        };
        let iframes = replace_all_with_node(&mut parsed, parent, node);
        let controls = crate::dom::forms::descendant_controls(
            &parsed.document.base,
            parent.document,
            parent.node,
        );
        drop(parsed);
        world.destroy_child_navigables(&iframes);
        drop(world);
        fixup_controls_on_insert(&ctx, parent.document, &controls)?;
        schedule_mutation_delivery(&ctx)
    }

    /// <https://dom.spec.whatwg.org/#dom-parentnode-movebefore>
    fn move_before(
        &self,
        ctx: Ctx<'js>,
        node: NodeReference,
        child: Option<NodeReference>,
    ) -> Result<()> {
        move_before_node(&ctx, self.handle.get().0, node, child)
    }

    // https://dom.spec.whatwg.org/#dom-parentnode-queryselector
    fn query_selector(
        &self,
        ctx: Ctx<'js>,
        selectors: rquickjs::String<'js>,
    ) -> Result<Value<'js>> {
        let selectors = selectors.to_string()?;
        let selectors = selector_matching_elements(&selectors);
        // Adoption by copy leaves a stale handle behind; search the live
        // subtree (https://dom.spec.whatwg.org/#concept-node-adopt).
        let scope = self.handle.get().0;
        let world = world(&ctx)?;
        let found = {
            let parsed = world.borrow();
            let Some(parsed) = parsed.document(scope) else {
                return Ok(Value::new_null(ctx));
            };
            parsed
                .document
                .base
                .query_selector_in(scope.node, &selectors)
                .map_err(|err| select_error(&ctx, &err))?
                .map(|node| NodeId {
                    document: parsed.id,
                    node,
                })
        };
        child_value(&ctx, found)
    }

    // https://dom.spec.whatwg.org/#dom-parentnode-queryselectorall
    fn query_selector_all(
        &self,
        ctx: Ctx<'js>,
        selectors: rquickjs::String<'js>,
    ) -> Result<Value<'js>> {
        let selectors = selectors.to_string()?;
        let selectors = selector_matching_elements(&selectors);
        // Adoption by copy leaves a stale handle behind; search the live
        // subtree (https://dom.spec.whatwg.org/#concept-node-adopt).
        let scope = self.handle.get().0;
        let world = world(&ctx)?;
        let ids = {
            let parsed = world.borrow();
            // A missing document answers with an empty list, not null
            // (<https://dom.spec.whatwg.org/#dom-parentnode-queryselectorall>).
            parsed
                .document(scope)
                .map(|parsed| {
                    parsed
                        .document
                        .base
                        .query_selector_all_in(scope.node, &selectors)
                        .map_err(|err| select_error(&ctx, &err))
                        .map(|ids| {
                            ids.into_iter()
                                .map(|node| {
                                    Handle(NodeId {
                                        document: parsed.id,
                                        node,
                                    })
                                })
                                .collect::<Vec<_>>()
                        })
                })
                .transpose()?
                .unwrap_or_default()
        };
        live_collection(&ctx, scope, CollectionKind::Static(ids), None)
    }
}

impl<'js> child_node_generated::ChildNode<'js> for JsNode {
    // https://dom.spec.whatwg.org/#dom-childnode-before
    fn before(
        &self,
        ctx: Ctx<'js>,
        nodes: Vec<child_node_generated::DOMStringOrNode<'js>>,
    ) -> Result<()> {
        // Parent and the viable sibling are captured before conversion.
        // Converting can move `this` into a fragment
        // (https://dom.spec.whatwg.org/#dom-childnode-before).
        let target = self.handle.get().0;
        let nodes = union_nodes(nodes);
        let excluded = argument_node_ids(&nodes);
        let Some(parent) = tree_parent(&ctx, target)? else {
            return Ok(());
        };
        let viable_previous = {
            let world = world(&ctx)?;
            let world = world.borrow();
            let Some(parsed) = world.document(target) else {
                return Err(Exception::throw_type(&ctx, "no document"));
            };
            sibling_not_in(&parsed.document.base, target.node, false, &excluded)
        };
        let node = convert_union_nodes_into_node(&ctx, target, nodes)?;
        let reference = {
            let world = world(&ctx)?;
            let world = world.borrow();
            let Some(parsed) = world.document(parent) else {
                return Err(Exception::throw_type(&ctx, "no document"));
            };
            let base = &parsed.document.base;
            let child = if let Some(previous) = viable_previous {
                sibling(base, previous, true)
            } else {
                base.get_node(parent.node)
                    .and_then(|node| node.children.first().copied())
            };
            child.map(|node| NodeId {
                document: parent.document,
                node,
            })
        };
        let (node, reference) = insertion_tree_nodes(
            &ctx,
            parent,
            NodeReference::Tree(node),
            reference.map(NodeReference::Tree),
        )?;
        insert_tree_node(&ctx, parent, node, reference)
    }

    // https://dom.spec.whatwg.org/#dom-childnode-after
    fn after(
        &self,
        ctx: Ctx<'js>,
        nodes: Vec<child_node_generated::DOMStringOrNode<'js>>,
    ) -> Result<()> {
        // Parent and the viable sibling are captured before conversion.
        // Converting can move `this` into a fragment
        // (https://dom.spec.whatwg.org/#dom-childnode-after).
        let target = self.handle.get().0;
        let nodes = union_nodes(nodes);
        let excluded = argument_node_ids(&nodes);
        let Some(parent) = tree_parent(&ctx, target)? else {
            return Ok(());
        };
        let viable_next = {
            let world = world(&ctx)?;
            let world = world.borrow();
            let Some(parsed) = world.document(target) else {
                return Err(Exception::throw_type(&ctx, "no document"));
            };
            sibling_not_in(&parsed.document.base, target.node, true, &excluded)
        };
        let node = convert_union_nodes_into_node(&ctx, target, nodes)?;
        let reference = viable_next.map(|node| NodeId {
            document: parent.document,
            node,
        });
        let (node, reference) = insertion_tree_nodes(
            &ctx,
            parent,
            NodeReference::Tree(node),
            reference.map(NodeReference::Tree),
        )?;
        insert_tree_node(&ctx, parent, node, reference)
    }

    // https://dom.spec.whatwg.org/#dom-childnode-replacewith
    fn replace_with(
        &self,
        ctx: Ctx<'js>,
        nodes: Vec<child_node_generated::DOMStringOrNode<'js>>,
    ) -> Result<()> {
        // Parent and the viable sibling are captured before conversion.
        // A parentless child returns before any node is moved
        // (https://dom.spec.whatwg.org/#dom-childnode-replacewith).
        let target = self.handle.get().0;
        let nodes = union_nodes(nodes);
        let excluded = argument_node_ids(&nodes);
        let Some(parent) = tree_parent(&ctx, target)? else {
            return Ok(());
        };
        let viable_next = {
            let world = world(&ctx)?;
            let world = world.borrow();
            let Some(parsed) = world.document(target) else {
                return Err(Exception::throw_type(&ctx, "no document"));
            };
            sibling_not_in(&parsed.document.base, target.node, true, &excluded)
        };
        let node = convert_union_nodes_into_node(&ctx, target, nodes)?;
        if tree_parent(&ctx, target)?.is_some_and(|current| current == parent) {
            let (node, _) = insertion_tree_nodes(
                &ctx,
                parent,
                NodeReference::Tree(node),
                Some(NodeReference::Tree(target)),
            )?;
            let replacement = replace_target(&ctx, parent, node)?;
            let world = world(&ctx)?;
            let world = world.borrow();
            let Some(mut parsed) = world.document_mut(target) else {
                return Err(Exception::throw_type(&ctx, "no document"));
            };
            let moved = replace_parsed(&mut parsed, parent, target, replacement);
            drop(parsed);
            drop(world);
            fixup_controls_on_insert(&ctx, target.document, &moved)?;
            return schedule_mutation_delivery(&ctx);
        }
        let reference = viable_next.map(|node| NodeId {
            document: parent.document,
            node,
        });
        let (node, reference) = insertion_tree_nodes(
            &ctx,
            parent,
            NodeReference::Tree(node),
            reference.map(NodeReference::Tree),
        )?;
        insert_tree_node(&ctx, parent, node, reference)
    }

    // https://dom.spec.whatwg.org/#dom-childnode-remove
    fn remove(&self, ctx: Ctx<'js>) -> Result<()> {
        let world = world(&ctx)?;
        let world = world.borrow();
        let Some(mut parsed) = world.document_mut(self.handle.get().0) else {
            return Ok(());
        };
        if parsed.document.base.root_node().id == self.handle.get().0.node {
            return Err(throw_dom(
                &ctx,
                "HierarchyRequestError",
                "a document cannot be removed",
            ));
        }
        let iframes = iframe_containers_in_subtrees(&parsed, &[self.handle.get().0]);
        unlink_journaled(&mut parsed, self.handle.get().0);
        drop(parsed);
        world.destroy_child_navigables(&iframes);
        drop(world);
        fixup_focus_after_removal(&ctx, self.handle.get().0)?;
        schedule_mutation_delivery(&ctx)
    }
}

impl<'js> non_document_type_child_node_generated::NonDocumentTypeChildNode<'js> for JsNode {
    // https://dom.spec.whatwg.org/#dom-nondocumenttypechildnode-previouselementsibling
    fn get_previous_element_sibling(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        element_sibling_value(ctx, self.handle.get().0, false)
    }

    // https://dom.spec.whatwg.org/#dom-nondocumenttypechildnode-nextelementsibling
    fn get_next_element_sibling(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        element_sibling_value(ctx, self.handle.get().0, true)
    }
}

impl<'js> From<parent_node_generated::DOMStringOrNode<'js>> for NodeOrString<'js> {
    fn from(item: parent_node_generated::DOMStringOrNode<'js>) -> Self {
        match item {
            parent_node_generated::DOMStringOrNode::Node(node) => Self::Node(node),
            parent_node_generated::DOMStringOrNode::DOMString(string) => Self::String(string),
        }
    }
}

impl<'js> From<child_node_generated::DOMStringOrNode<'js>> for NodeOrString<'js> {
    fn from(item: child_node_generated::DOMStringOrNode<'js>) -> Self {
        match item {
            child_node_generated::DOMStringOrNode::Node(node) => Self::Node(node),
            child_node_generated::DOMStringOrNode::DOMString(string) => Self::String(string),
        }
    }
}

impl JsNode {
    pub(super) fn node_type(&self, ctx: &Ctx<'_>) -> Result<u16> {
        // https://dom.spec.whatwg.org/#dom-node-nodetype
        with_node_data(ctx, self.handle.get().0, |data| {
            crate::dom::tree::node_type(data)
                .ok_or_else(|| Exception::throw_type(ctx, "stale node"))
        })?
    }

    // https://dom.spec.whatwg.org/#dom-node-nodename
    // https://dom.spec.whatwg.org/#concept-element-html-uppercased-qualified-name
    pub(super) fn node_name<'js>(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        let uppercase = document_is_html_content(ctx, self.handle.get().0);
        let world = world_for_node(ctx, self.handle.get().0)?;
        let borrowed = world.borrow();
        let Some(parsed) = borrowed.document(self.handle.get().0) else {
            return Err(Exception::throw_type(ctx, "stale node"));
        };
        let name =
            crate::dom::tree::node_name(&parsed.document.base, self.handle.get().0.node, uppercase)
                .ok_or_else(|| Exception::throw_type(ctx, "stale node"))?;
        rquickjs::String::from_str(ctx.clone(), &name)
    }

    // https://dom.spec.whatwg.org/#dom-node-firstchild
    pub(super) fn first_child<'js>(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        // Adoption by copy leaves a stale handle behind; read the live node's
        // children (https://dom.spec.whatwg.org/#concept-node-adopt).
        let id = self.handle.get().0;
        let world = world(ctx)?;
        let id = world.borrow().document(id).and_then(|parsed| {
            parsed
                .document
                .base
                .get_node(id.node)
                .and_then(|node| node.children.first().copied())
                .map(|node| NodeId {
                    document: parsed.id,
                    node,
                })
        });
        child_value(ctx, id)
    }

    // https://dom.spec.whatwg.org/#dom-node-lastchild
    pub(super) fn last_child<'js>(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        let world = world(ctx)?;
        let id = world
            .borrow()
            .document(self.handle.get().0)
            .and_then(|parsed| {
                parsed
                    .document
                    .base
                    .get_node(self.handle.get().0.node)
                    .and_then(|node| node.children.last().copied())
                    .map(|node| NodeId {
                        document: parsed.id,
                        node,
                    })
            });
        child_value(ctx, id)
    }

    // https://dom.spec.whatwg.org/#dom-node-nextsibling
    pub(super) fn next_sibling<'js>(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        sibling_value(ctx, self.handle.get().0, true)
    }

    // https://dom.spec.whatwg.org/#dom-node-previoussibling
    pub(super) fn previous_sibling<'js>(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        sibling_value(ctx, self.handle.get().0, false)
    }

    pub(super) fn parent_node<'js>(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        let world = world(ctx)?;
        let id = world
            .borrow()
            .document(self.handle.get().0)
            .and_then(|parsed| {
                parsed
                    .document
                    .base
                    .get_node(self.handle.get().0.node)
                    .and_then(|node| node.parent)
                    .map(|node| NodeId {
                        document: parsed.id,
                        node,
                    })
            });
        child_value(ctx, id)
    }

    // https://dom.spec.whatwg.org/#dom-node-childnodes
    pub(super) fn child_nodes<'js>(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        // `[SameObject]`: one NodeList per node.
        let world_rc = world(ctx)?;
        if let Some(saved) = world_rc
            .borrow()
            .same_object(self.handle.get().0, SameObject::ChildNodes)
            && let Some(value) = deref_weak(ctx, saved)?
        {
            return Ok(value);
        }
        let value = live_collection(ctx, self.handle.get().0, CollectionKind::Children, None)?;
        let weak = make_weak(ctx, value.clone())?;
        world_rc.borrow_mut().intern_same_object(
            self.handle.get().0,
            SameObject::ChildNodes,
            Persistent::save(ctx, weak),
        );
        Ok(value)
    }

    // https://dom.spec.whatwg.org/#dom-node-appendchild
    pub(super) fn append_child<'js>(
        &self,
        ctx: Ctx<'js>,
        node: NodeReference,
    ) -> Result<Value<'js>> {
        let (node, _) = insertion_tree_nodes(&ctx, self.handle.get().0, node, None)?;
        let node = adopt_node(&ctx, self.handle.get().0, node)?;
        let parent = self.handle.get().0;
        insert_tree_node(&ctx, parent, node, None)?;
        wrap_node(&ctx, node)
    }

    // https://dom.spec.whatwg.org/#dom-node-ownerdocument
    pub(super) fn owner_document<'js>(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        // Adoption by copy leaves a stale handle behind; the owner is the
        // live node's document
        // (https://dom.spec.whatwg.org/#concept-node-adopt).
        let id = self.handle.get().0;
        let world = world(ctx)?;
        let id = world.borrow().document(id).and_then(|parsed| {
            let root = parsed.document.base.root_node().id;
            (root != id.node).then_some(NodeId {
                document: parsed.id,
                node: root,
            })
        });
        child_value(ctx, id)
    }

    // https://dom.spec.whatwg.org/#dom-node-haschildnodes
    pub(super) fn has_child_nodes(&self, ctx: Ctx<'_>) -> Result<bool> {
        let world = world(&ctx)?;
        let parsed = world.borrow();
        let Some(parsed) = parsed.document(self.handle.get().0) else {
            return Ok(false);
        };
        Ok(parsed
            .document
            .base
            .get_node(self.handle.get().0.node)
            .is_some_and(|node| !node.children.is_empty()))
    }

    // https://dom.spec.whatwg.org/#dom-node-nodevalue
    pub(super) fn node_value<'js>(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        let world = world(ctx)?;
        let parsed = world.borrow();
        let Some(parsed) = parsed.document(self.handle.get().0) else {
            return Ok(None);
        };
        let is_character_data = parsed
            .document
            .base
            .get_node(self.handle.get().0.node)
            .is_some_and(|node| {
                matches!(
                    node.data,
                    NodeData::Text(_)
                        | NodeData::Comment { .. }
                        | NodeData::ProcessingInstruction { .. }
                        | NodeData::CDataSection { .. }
                )
            });
        if is_character_data {
            return dom_string(
                ctx,
                &parsed.document.character_data(self.handle.get().0.node),
            )
            .map(Some);
        }
        Ok(None)
    }

    // https://dom.spec.whatwg.org/#dom-node-nodevalue
    pub(super) fn set_node_value(
        &self,
        ctx: &Ctx<'_>,
        value: Option<rquickjs::String<'_>>,
    ) -> Result<()> {
        // Document, DocumentType, and DocumentFragment ignore the setter
        // (<https://dom.spec.whatwg.org/#dom-node-nodevalue>).
        let skip = world(ctx).is_ok_and(|world| {
            world
                .borrow()
                .document(self.handle.get().0)
                .is_some_and(|parsed| {
                    parsed.document.is_fragment(self.handle.get().0.node)
                        || parsed.document.base.root_node().id == self.handle.get().0.node
                        || parsed
                            .document
                            .base
                            .get_node(self.handle.get().0.node)
                            .is_some_and(|node| matches!(node.data, NodeData::Doctype { .. }))
                })
        });
        if skip {
            return Ok(());
        }
        let value = match value {
            Some(value) => crate::dom_string::DomString::from_utf16(value.to_utf16()?),
            None => crate::dom_string::DomString::default(),
        };
        set_character_data(ctx, self.handle.get().0, &value)
    }

    // https://dom.spec.whatwg.org/#dom-node-textcontent
    pub(super) fn text_content<'js>(
        &self,
        ctx: &Ctx<'js>,
    ) -> Result<Option<rquickjs::String<'js>>> {
        let world = world(ctx)?;
        let parsed = world.borrow();
        let Some(parsed) = parsed.document(self.handle.get().0) else {
            return Ok(None);
        };
        let kind = parsed
            .document
            .base
            .get_node(self.handle.get().0.node)
            .map(|node| match &node.data {
                NodeData::Element(_) | NodeData::Fragment { .. } | NodeData::ShadowRoot { .. } => {
                    0u8
                }
                NodeData::Text(_)
                | NodeData::Comment { .. }
                | NodeData::ProcessingInstruction { .. }
                | NodeData::CDataSection { .. } => 1,
                _ => 2,
            });
        match kind {
            Some(0) => {
                let text = parsed
                    .document
                    .descendant_dom_text(self.handle.get().0.node);
                dom_string(ctx, &text).map(Some)
            }
            Some(1) => dom_string(
                ctx,
                &parsed.document.character_data(self.handle.get().0.node),
            )
            .map(Some),
            _ => Ok(None),
        }
    }

    pub(super) fn set_text_content(
        &self,
        ctx: &Ctx<'_>,
        value: Option<rquickjs::String<'_>>,
    ) -> Result<()> {
        let text = match value {
            Some(value) => crate::dom_string::DomString::from_utf16(value.to_utf16()?),
            None => crate::dom_string::DomString::default(),
        };
        // A `CharacterData` node [replaces its data] in place; `Document`
        // nodes ignore the setter; every other node replaces its children
        // with one `Text` node
        // (<https://dom.spec.whatwg.org/#dom-node-textcontent>).
        let world_rc = world(ctx)?;
        let character_data = {
            let world = world_rc.borrow();
            let Some(parsed) = world.document(self.handle.get().0) else {
                return Ok(());
            };
            // A `CharacterData` node [replaces its data] in place; every other
            // non-document node replaces its children with one `Text` node
            parsed
                .document
                .base
                .get_node(self.handle.get().0.node)
                .is_some_and(|node| {
                    matches!(
                        node.data,
                        NodeData::Text(_)
                            | NodeData::Comment { .. }
                            | NodeData::ProcessingInstruction { .. }
                            | NodeData::CDataSection { .. }
                    )
                })
        };
        if character_data {
            return set_character_data(ctx, self.handle.get().0, &text);
        }
        let world = world_rc.borrow();
        let Some(mut parsed) = world.document_mut(self.handle.get().0) else {
            return Ok(());
        };
        // Document and DocumentType ignore the setter
        // (<https://dom.spec.whatwg.org/#dom-node-textcontent>).
        if parsed.document.base.root_node().id == self.handle.get().0.node {
            return Ok(());
        }
        if parsed
            .document
            .base
            .get_node(self.handle.get().0.node)
            .is_some_and(|node| matches!(node.data, NodeData::Doctype { .. }))
        {
            return Ok(());
        }
        let added = if text.is_empty() {
            Vec::new()
        } else {
            let node = parsed.document.create_text(&text);
            vec![NodeId {
                document: parsed.id,
                node,
            }]
        };
        let iframes = replace_all_journaled(&mut parsed, self.handle.get().0, added);
        drop(parsed);
        world.destroy_child_navigables(&iframes);
        drop(world);
        schedule_mutation_delivery(ctx)
    }

    // https://dom.spec.whatwg.org/#dom-element-matches
    pub(super) fn matches(&self, ctx: Ctx<'_>, selectors: WebIdlString) -> Result<bool> {
        let world = world(&ctx)?;
        let parsed = world.borrow();
        let Some(parsed) = parsed.document(self.handle.get().0) else {
            return Ok(false);
        };
        let selectors = selector_matching_elements(&selectors.0);
        parsed
            .document
            .base
            .matches_selector(self.handle.get().0.node, &selectors)
            .map_err(|err| select_error(&ctx, &err))
    }

    // https://dom.spec.whatwg.org/#dom-element-closest
    pub(super) fn closest<'js>(
        &self,
        ctx: Ctx<'js>,
        selectors: WebIdlString,
    ) -> Result<Value<'js>> {
        let world = world(&ctx)?;
        let found = {
            let parsed = world.borrow();
            let Some(parsed) = parsed.document(self.handle.get().0) else {
                return Ok(Value::new_null(ctx));
            };
            let selectors = selector_matching_elements(&selectors.0);
            parsed
                .document
                .base
                .closest(self.handle.get().0.node, &selectors)
                .map_err(|err| select_error(&ctx, &err))?
                .map(|node| NodeId {
                    document: parsed.id,
                    node,
                })
        };
        child_value(&ctx, found)
    }

    // https://dom.spec.whatwg.org/#dom-element-getelementsbyclassname
    pub(super) fn get_elements_by_class_name<'js>(
        &self,
        ctx: Ctx<'js>,
        names: WebIdlString,
    ) -> Result<Value<'js>> {
        live_collection(
            &ctx,
            self.handle.get().0,
            CollectionKind::ElementsByClass(names.0),
            Some("HTMLCollection"),
        )
    }

    // https://dom.spec.whatwg.org/#dom-node-normalize
    #[expect(
        clippy::too_many_lines,
        reason = "Node.normalize steps in spec order; splitting them is a refactor, not a lint fix"
    )]
    pub(super) fn normalize(&self, ctx: Ctx<'_>) -> Result<()> {
        let world = world(&ctx)?;
        let world = world.borrow();
        let Some(mut parsed) = world.document_mut(self.handle.get().0) else {
            return Ok(());
        };
        let document = parsed.id;
        let mut containers: Vec<BlitzId> = vec![self.handle.get().0.node];
        let mut stack = vec![self.handle.get().0.node];
        while let Some(id) = stack.pop() {
            if let Some(node) = parsed.document.base.get_node(id) {
                for kid in &node.children.clone() {
                    if parsed
                        .document
                        .base
                        .get_node(*kid)
                        .is_some_and(|candidate| candidate.data.downcast_element().is_some())
                    {
                        containers.push(*kid);
                        stack.push(*kid);
                    }
                }
            }
        }
        for container in containers {
            let kids: Vec<BlitzId> = parsed
                .document
                .base
                .get_node(container)
                .map_or_default(|node| node.children.iter().copied().collect());
            // In tree order: drop empty Text nodes, merge contiguous runs
            // into the first non-empty one
            // (<https://dom.spec.whatwg.org/#dom-node-normalize>).
            let mut merged: Option<BlitzId> = None;
            for kid in kids {
                let content =
                    parsed
                        .document
                        .base
                        .get_node(kid)
                        .and_then(|node| match &node.data {
                            NodeData::Text(data) => Some(data.content.clone()),
                            _ => None,
                        });
                // Exclusive Text nodes only: CDATA sections hold text but
                // are never merged or dropped
                // (<https://dom.spec.whatwg.org/#dom-node-normalize>).
                if matches!(
                    parsed.document.base.get_node(kid).map(|node| &node.data),
                    Some(NodeData::CDataSection { .. })
                ) {
                    merged = None;
                    continue;
                }
                let Some(content) = content else {
                    merged = None;
                    continue;
                };
                if content.is_empty() {
                    unlink_journaled(
                        &mut parsed,
                        NodeId {
                            document,
                            node: kid,
                        },
                    );
                    continue;
                }
                if let Some(previous) = merged {
                    let previous_content = parsed
                        .document
                        .base
                        .get_node(previous)
                        .and_then(|node| match &node.data {
                            NodeData::Text(data) => Some(data.content.clone()),
                            _ => None,
                        })
                        .unwrap_or_default();
                    let mut joined = DomString::from(previous_content.clone());
                    // The merge target's current length is the accumulated
                    // length the normalize steps shift points by
                    // (<https://dom.spec.whatwg.org/#dom-node-normalize>).
                    let length = u32::try_from(joined.units().len()).unwrap_or(u32::MAX);
                    joined.push_str(&content);
                    let target = NodeId {
                        document,
                        node: previous,
                    };
                    let dying = NodeId {
                        document,
                        node: kid,
                    };
                    if !parsed.document.side.ranges.is_empty() {
                        let index = parsed
                            .document
                            .base
                            .get_node(container)
                            .and_then(|node| node.children.iter().position(|child| *child == kid))
                            .and_then(|index| u32::try_from(index).ok());
                        if let Some(index) = index {
                            crate::dom::range::adjust_for_normalize(
                                &mut parsed.document.side.ranges,
                                target,
                                dying,
                                NodeId {
                                    document,
                                    node: container,
                                },
                                index,
                                length,
                            );
                        }
                    }
                    parsed
                        .document
                        .base
                        .mutate()
                        .set_node_text(previous, &joined.to_string_lossy());
                    parsed.document.record(JournalEntry::CharacterData {
                        target,
                        old_value: DomString::from(previous_content),
                    });
                    unlink_journaled(&mut parsed, dying);
                } else {
                    merged = Some(kid);
                }
            }
        }
        drop(parsed);
        drop(world);
        schedule_mutation_delivery(&ctx)
    }

    // https://dom.spec.whatwg.org/#dom-node-comparedocumentposition
    pub(super) fn compare_document_position(
        &self,
        ctx: Ctx<'_>,
        other: NodeReference,
    ) -> Result<u16> {
        compare_node_position(&ctx, NodeReference::Tree(self.handle.get().0), other)
    }

    // https://dom.spec.whatwg.org/#dom-node-parentelement
    pub(super) fn parent_element<'js>(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        let world_rc = world(ctx)?;
        let parent = {
            let parsed = world_rc.borrow();
            let Some(parsed) = parsed.document(self.handle.get().0) else {
                return Ok(Value::new_null(ctx.clone()));
            };
            parsed
                .document
                .base
                .get_node(self.handle.get().0.node)
                .and_then(|node| node.parent)
                .filter(|parent| is_element(&parsed.document.base, *parent))
                .map(|parent| NodeId {
                    document: parsed.id,
                    node: parent,
                })
        };
        child_value(ctx, parent)
    }

    // https://dom.spec.whatwg.org/#dom-node-issamenode
    pub(super) fn is_same_node(&self, other: Option<NodeReference>) -> bool {
        other == Some(NodeReference::Tree(self.handle.get().0))
    }

    // https://dom.spec.whatwg.org/#dom-node-isequalnode
    pub(super) fn is_equal_node(&self, ctx: Ctx<'_>, other: Option<NodeReference>) -> Result<bool> {
        let Some(NodeReference::Tree(other)) = other else {
            return Ok(false);
        };
        let world = world(&ctx)?;
        let world = world.borrow();
        Ok(equality::nodes_equal(
            &world.store(),
            self.handle.get().0,
            other,
        ))
    }

    // https://dom.spec.whatwg.org/#dom-node-contains
    pub(super) fn contains(&self, ctx: Ctx<'_>, other: Option<NodeReference>) -> Result<bool> {
        let Some(NodeReference::Tree(other)) = other else {
            return Ok(false);
        };
        if other.document != self.handle.get().0.document {
            return Ok(false);
        }
        let world = world(&ctx)?;
        let parsed = world.borrow();
        let Some(parsed) = parsed.document(self.handle.get().0) else {
            return Ok(false);
        };
        let base = &parsed.document.base;
        let mut cursor = Some(other.node);
        while let Some(id) = cursor {
            if id == self.handle.get().0.node {
                return Ok(true);
            }
            cursor = base.get_node(id).and_then(|node| node.parent);
        }
        Ok(false)
    }

    // https://dom.spec.whatwg.org/#dom-node-getrootnode
    pub(super) fn get_root_node<'js>(
        &self,
        ctx: Ctx<'js>,
        options: node_generated::GetRootNodeOptions,
    ) -> Result<Value<'js>> {
        // `composed: true` returns the shadow-including root
        // (<https://dom.spec.whatwg.org/#dom-node-getrootnode>).
        let world = world(&ctx)?;
        let mut root = self.handle.get().0;
        {
            let parsed = world.borrow();
            let Some(parsed) = parsed.document(self.handle.get().0) else {
                return Ok(Value::new_null(ctx));
            };
            let base = &parsed.document.base;
            loop {
                let parent = if options.composed {
                    crate::dom::tree::shadow_including_parent(base, root.node)
                } else {
                    base.get_node(root.node).and_then(|node| node.parent)
                };
                let Some(parent) = parent else {
                    break;
                };
                root = NodeId {
                    document: root.document,
                    node: parent,
                };
            }
        }
        wrap_node(&ctx, root)
    }

    // https://dom.spec.whatwg.org/#dom-node-isconnected
    pub(super) fn is_connected(&self, ctx: &Ctx<'_>) -> Result<bool> {
        let world = world(ctx)?;
        let parsed = world.borrow();
        let Some(parsed) = parsed.document(self.handle.get().0) else {
            return Ok(false);
        };
        Ok(is_connected(
            &parsed.document.base,
            self.handle.get().0.node,
        ))
    }

    // https://dom.spec.whatwg.org/#dom-node-clonenode
    pub(super) fn clone_node<'js>(&self, ctx: Ctx<'js>, deep: bool) -> Result<Value<'js>> {
        let world_rc = world(&ctx)?;
        let is_document = {
            let world = world_rc.borrow();
            let Some(parsed) = world.document(self.handle.get().0) else {
                return Err(Exception::throw_type(&ctx, "no document"));
            };
            self.handle.get().0.node == parsed.document.base.root_node().id
        };
        if is_document {
            return clone_document(&ctx, self.handle.get().0, deep);
        }
        if node_is_shadow_root(&ctx, self.handle.get().0)? {
            // https://dom.spec.whatwg.org/#dom-node-clonenode
            return Err(throw_dom(
                &ctx,
                "NotSupportedError",
                "cannot clone a shadow root",
            ));
        }
        // Known gap: form-state cloning (input checkedness/value) has no Blitz
        // equivalent yet; deep clones carry structure only.
        let clone = if deep {
            let snapshot = world_rc
                .borrow()
                .store()
                .get(self.handle.get().0.document)
                .and_then(|parsed| {
                    crate::dom::adoption::import_snapshot(
                        &parsed.document,
                        self.handle.get().0,
                        deep,
                    )
                })
                .ok_or_else(|| Exception::throw_type(&ctx, "stale node"))?;
            let owner = world_for_node(&ctx, self.handle.get().0)?;
            let world = owner.borrow();
            let Some(mut parsed) = world.document_mut(self.handle.get().0) else {
                return Err(Exception::throw_type(&ctx, "no document"));
            };
            let clone = materialize_import(
                &mut parsed.document,
                self.handle.get().0.document,
                &snapshot,
            )
            .map_err(|err| throw_dom_error(&ctx, err))?;
            // The cloning steps copy the already-started flag onto the clone,
            // including every descendant (<https://html.spec.whatwg.org/multipage/scripting.html#script-processing-model:cloning-steps>).
            // Without it a cloned `<script>` runs again when the clone is
            // connected, so appending a cloned document element re-executes
            // the page's inline scripts.
            parsed
                .document
                .copy_script_started(self.handle.get().0.node, clone.node);
            drop(parsed);
            drop(world);
            // Cloning copies the cryptographic nonce slots into the fresh
            // subtree (<https://html.spec.whatwg.org/multipage/urls-and-fetching.html#nonce-attributes> cloning steps).
            copy_nonce_subtree(&owner, self.handle.get().0, clone);
            clone
        } else {
            let world = world_rc.borrow();
            let Some(mut parsed) = world.document_mut(self.handle.get().0) else {
                return Err(Exception::throw_type(&ctx, "no document"));
            };
            let store = parsed.id;
            let clone =
                clone_within_document(&mut parsed.document, store, self.handle.get().0, deep)
                    .map_err(|err| throw_dom_error(&ctx, err))?;
            drop(parsed);
            drop(world);
            // Cloning copies the cryptographic nonce slots into the fresh
            // subtree (<https://html.spec.whatwg.org/multipage/urls-and-fetching.html#nonce-attributes> cloning steps).
            copy_nonce_subtree(&world_rc, self.handle.get().0, clone);
            clone
        };
        wrap_node(&ctx, clone)
    }

    // https://dom.spec.whatwg.org/#dom-node-insertbefore
    pub(super) fn insert_before<'js>(
        &self,
        ctx: Ctx<'js>,
        node: NodeReference,
        child: Option<NodeReference>,
    ) -> Result<Value<'js>> {
        let (node, child) = insertion_tree_nodes(&ctx, self.handle.get().0, node, child)?;
        let node = adopt_node(&ctx, self.handle.get().0, node)?;
        insert_tree_node(&ctx, self.handle.get().0, node, child)?;
        wrap_node(&ctx, node)
    }

    // https://dom.spec.whatwg.org/#dom-node-removechild
    pub(super) fn remove_child<'js>(
        &self,
        ctx: Ctx<'js>,
        child: NodeReference,
    ) -> Result<Value<'js>> {
        let child = child
            .tree()
            .ok_or_else(|| throw_dom(&ctx, "NotFoundError", "attributes have no parent"))?;
        let world = world(&ctx)?;
        let world = world.borrow();
        let Some(mut parsed) = world.document_mut(self.handle.get().0) else {
            return Err(Exception::throw_type(&ctx, "no document"));
        };
        let parented = parsed
            .document
            .base
            .get_node(child.node)
            .and_then(|node| node.parent)
            == Some(self.handle.get().0.node)
            && child.document == self.handle.get().0.document;
        if !parented {
            return Err(throw_dom(
                &ctx,
                "NotFoundError",
                "child is not a child of this node",
            ));
        }
        let iframes = iframe_containers_in_subtrees(&parsed, &[child]);
        unlink_journaled(&mut parsed, child);
        drop(parsed);
        world.destroy_child_navigables(&iframes);
        drop(world);
        fixup_focus_after_removal(&ctx, child)?;
        schedule_mutation_delivery(&ctx)?;
        wrap_node(&ctx, child)
    }

    // https://dom.spec.whatwg.org/#dom-node-replacechild
    pub(super) fn replace_child<'js>(
        &self,
        ctx: Ctx<'js>,
        node: NodeReference,
        child: NodeReference,
    ) -> Result<Value<'js>> {
        // Adoption copies by snapshot; pre-existing wrappers still point at the
        // detached original instead of following the copy
        // (https://dom.spec.whatwg.org/#concept-node-adopt).
        let parent = self.handle.get().0;
        // `replace` excludes the child being replaced from the document
        // content-model counts
        // (<https://dom.spec.whatwg.org/#concept-node-replace>).
        let exclude = child.tree().map(|id| id.node);
        let (node, _) = insertion_excluding(&ctx, parent, node, Some(child), exclude)?;
        let Some(child) = child.tree() else {
            return Err(throw_dom(
                &ctx,
                "NotFoundError",
                "attributes have no parent",
            ));
        };
        let replacement = replace_target(&ctx, parent, node)?;
        let moved = {
            let world = world(&ctx)?;
            let world = world.borrow();
            let Some(mut parsed) = world.document_mut(parent) else {
                return Err(Exception::throw_type(&ctx, "no document"));
            };
            replace_parsed(&mut parsed, parent, child, replacement)
        };
        fixup_focus_after_removal(&ctx, child)?;
        fixup_controls_on_insert(&ctx, parent.document, &moved)?;
        run_html_insertion_steps(&ctx, parent, &moved)?;
        schedule_mutation_delivery(&ctx)?;
        wrap_node(&ctx, child)
    }

    // https://dom.spec.whatwg.org/#dom-node-lookupnamespaceuri
    pub(super) fn lookup_namespace_uri<'js>(
        &self,
        ctx: Ctx<'js>,
        prefix: Option<rquickjs::String<'js>>,
    ) -> Result<Value<'js>> {
        let prefix = prefix
            .map(|value| value.to_string())
            .transpose()?
            .filter(|value| !value.is_empty());
        let world = world(&ctx)?;
        let parsed = world.borrow();
        let Some(parsed) = parsed.document(self.handle.get().0) else {
            return Ok(Value::new_null(ctx));
        };
        match crate::dom::namespaces::locate_namespace(
            &parsed.document,
            self.handle.get().0,
            prefix.as_deref(),
        ) {
            Some(namespace) => string_value(&ctx, &namespace),
            None => Ok(Value::new_null(ctx)),
        }
    }

    // https://dom.spec.whatwg.org/#dom-node-lookupprefix
    pub(super) fn lookup_prefix<'js>(
        &self,
        ctx: Ctx<'js>,
        namespace: Option<rquickjs::String<'js>>,
    ) -> Result<Value<'js>> {
        let Some(namespace) = namespace
            .map(|value| value.to_string())
            .transpose()?
            .filter(|value| !value.is_empty())
        else {
            return Ok(Value::new_null(ctx));
        };
        let world = world(&ctx)?;
        let parsed = world.borrow();
        let Some(parsed) = parsed.document(self.handle.get().0) else {
            return Ok(Value::new_null(ctx));
        };
        match crate::dom::namespaces::locate_prefix(
            &parsed.document,
            self.handle.get().0,
            &namespace,
        ) {
            Some(prefix) => string_value(&ctx, &prefix),
            None => Ok(Value::new_null(ctx)),
        }
    }

    // https://dom.spec.whatwg.org/#dom-node-isdefaultnamespace
    pub(super) fn is_default_namespace<'js>(
        &self,
        ctx: Ctx<'js>,
        namespace: Option<rquickjs::String<'js>>,
    ) -> Result<bool> {
        let namespace = namespace
            .map(|value| value.to_string())
            .transpose()?
            .filter(|value| !value.is_empty());
        let world = world(&ctx)?;
        let parsed = world.borrow();
        let Some(parsed) = parsed.document(self.handle.get().0) else {
            return Ok(false);
        };
        let found =
            crate::dom::namespaces::locate_namespace(&parsed.document, self.handle.get().0, None);
        Ok(found.as_deref() == namespace.as_deref())
    }
}
pub(in crate::js::bindings) fn insertion_tree_nodes(
    ctx: &Ctx<'_>,
    parent: NodeId,
    node: NodeReference,
    child: Option<NodeReference>,
) -> Result<(NodeId, Option<NodeId>)> {
    insertion_excluding_children(ctx, parent, node, child, &[])
}

/// Validates and stages one insertion, leaving `exclude` out of the
/// parent's child counts so `replaceChildren` validates against the
/// parent without its current children.
pub(super) fn insertion_excluding(
    ctx: &Ctx<'_>,
    parent: NodeId,
    node: NodeReference,
    child: Option<NodeReference>,
    exclude: Option<BlitzId>,
) -> Result<(NodeId, Option<NodeId>)> {
    insertion_excluding_children(ctx, parent, node, child, exclude.as_slice())
}

/// Validates one insertion against an explicit exclusion set.
/// [`insertion_excluding`] covers the single-exclusion callers.
pub(super) fn insertion_excluding_children(
    ctx: &Ctx<'_>,
    parent: NodeId,
    node: NodeReference,
    child: Option<NodeReference>,
    excluded: &[BlitzId],
) -> Result<(NodeId, Option<NodeId>)> {
    // https://dom.spec.whatwg.org/#concept-node-ensure-pre-insert-validity
    // https://dom.spec.whatwg.org/#concept-node-replace
    if matches!(child, Some(NodeReference::Attribute { .. })) {
        return Err(throw_dom(ctx, "NotFoundError", "attributes have no parent"));
    }
    let node = node.tree().ok_or_else(|| {
        throw_dom(
            ctx,
            "HierarchyRequestError",
            "attributes cannot be inserted",
        )
    })?;
    let owner = world_for_node(ctx, parent)?;
    let owner = owner.borrow();
    // One document store backs every tree. Hold one document borrow at a time.
    let parent_is_document = {
        let parsed = owner
            .document(parent)
            .ok_or_else(|| Exception::throw_type(ctx, "stale parent"))?;
        let base = &parsed.document.base;
        // <https://dom.spec.whatwg.org/#concept-node-ensure-pre-insert-validity>
        let parent_is_document = base.root_node().id == parent.node;
        let parent_is_fragment = base.get_node(parent.node).is_some_and(|node| {
            matches!(
                node.data,
                NodeData::Fragment { .. } | NodeData::ShadowRoot { .. }
            )
        });
        if !parent_is_document && !is_element(base, parent.node) && !parent_is_fragment {
            return Err(throw_dom(
                ctx,
                "HierarchyRequestError",
                "parent cannot have children",
            ));
        }
        parent_is_document
    };
    // The reference child must already be parented here.
    let reference = match child {
        Some(NodeReference::Tree(id)) => Some(id),
        Some(NodeReference::Attribute { .. }) | None => None,
    };
    // Step 2 runs before the reference-child check (step 3): an ancestor
    // throws HierarchyRequestError even when `child` is not a child of parent.
    // https://dom.spec.whatwg.org/#concept-node-ensure-pre-insert-validity
    {
        let parsed = owner
            .document(parent)
            .ok_or_else(|| Exception::throw_type(ctx, "stale parent"))?;
        let base = &parsed.document.base;
        if node.document == parent.document {
            ensure_no_cycle(base, parent.node, node.node)
                .map_err(|error| throw_dom_error(ctx, error))?;
        }
        if let Some(reference) = reference {
            ensure_parented(base, parent, reference)
                .map_err(|error| throw_dom_error(ctx, error))?;
        }
    }
    // The node itself must be live, resolved in its own document.
    let inserted = {
        let node_parsed = owner
            .document(node)
            .ok_or_else(|| Exception::throw_type(ctx, "stale node"))?;
        // A document (any document's root) can never be inserted.
        // https://dom.spec.whatwg.org/#concept-node-ensure-pre-insert-validity
        if node_parsed.document.base.root_node().id == node.node {
            return Err(throw_dom(
                ctx,
                "HierarchyRequestError",
                "a document cannot be inserted",
            ));
        }
        classify_inserted(&node_parsed.document, node.node)
    };
    // Fragments, doctypes, elements, and character data insert.
    // https://dom.spec.whatwg.org/#concept-node-ensure-pre-insertion-validity
    if matches!(inserted, InsertedNode::Other) {
        return Err(throw_dom(
            ctx,
            "HierarchyRequestError",
            "node cannot be inserted",
        ));
    }
    // A doctype's parent is a document. Other parents return after that check.
    // https://dom.spec.whatwg.org/#concept-node-ensure-pre-insertion-validity
    if !parent_is_document {
        if matches!(inserted, InsertedNode::Doctype) {
            return Err(throw_dom(
                ctx,
                "HierarchyRequestError",
                "doctype parent is not a document",
            ));
        }
        return Ok((node, reference));
    }
    {
        let parsed = owner
            .document(parent)
            .ok_or_else(|| Exception::throw_type(ctx, "stale parent"))?;
        ensure_document_content_model(&parsed.document, parent, reference, excluded, &inserted)
            .map_err(|error| throw_dom_error(ctx, error))?;
    }
    Ok((node, reference))
}

pub(in crate::js::bindings) fn compare_node_position(
    ctx: &Ctx<'_>,
    this: NodeReference,
    other: NodeReference,
) -> Result<u16> {
    // https://dom.spec.whatwg.org/#dom-node-comparedocumentposition
    const DISCONNECTED: u16 = 1;
    const PRECEDING: u16 = 2;
    const FOLLOWING: u16 = 4;
    const CONTAINS: u16 = 8;
    const CONTAINED_BY: u16 = 16;
    const IMPLEMENTATION_SPECIFIC: u16 = 32;
    if this == other {
        return Ok(0);
    }
    let node1 = match other {
        NodeReference::Tree(id) => Some(id),
        NodeReference::Attribute { scope, id } => attr_owner(ctx, scope, id),
    };
    let node2 = match this {
        NodeReference::Tree(id) => Some(id),
        NodeReference::Attribute { scope, id } => attr_owner(ctx, scope, id),
    };
    let disconnected =
        DISCONNECTED | IMPLEMENTATION_SPECIFIC | if other < this { PRECEDING } else { FOLLOWING };
    let (Some(node1), Some(node2)) = (node1, node2) else {
        return Ok(disconnected);
    };
    if node1.document != node2.document {
        return Ok(disconnected);
    }
    let owner = world_for_node(ctx, node1)?;
    let owner = owner.borrow();
    let parsed = owner
        .document(node1)
        .ok_or_else(|| Exception::throw_type(ctx, "stale node"))?;
    let base = &parsed.document.base;
    let document = parsed.id;
    if let (
        NodeReference::Attribute {
            scope: scope1,
            id: attr1,
        },
        NodeReference::Attribute {
            scope: scope2,
            id: attr2,
        },
    ) = (other, this)
        && node1 == node2
    {
        let a = attr_state(ctx, scope1, attr1)?;
        let b = attr_state(ctx, scope2, attr2)?;
        if let Some(element) = base
            .get_node(node1.node)
            .and_then(|candidate| candidate.data.downcast_element())
        {
            for attribute in element.attrs.iter() {
                if attribute.name.ns.as_str() == a.namespace
                    && attribute.name.local.as_str() == a.local
                {
                    return Ok(IMPLEMENTATION_SPECIFIC | PRECEDING);
                }
                if attribute.name.ns.as_str() == b.namespace
                    && attribute.name.local.as_str() == b.local
                {
                    return Ok(IMPLEMENTATION_SPECIFIC | FOLLOWING);
                }
            }
        }
    }
    if root_of(base, node1.node) != root_of(base, node2.node) {
        return Ok(disconnected);
    }
    let attr1 = matches!(other, NodeReference::Attribute { .. });
    let attr2 = matches!(this, NodeReference::Attribute { .. });
    let node1_ancestor = ancestor_chain(base, document, node2.node).contains(&node1);
    let node2_ancestor = ancestor_chain(base, document, node1.node).contains(&node2);
    if (node1_ancestor && !attr1) || (node1 == node2 && attr2) {
        return Ok(CONTAINS | PRECEDING);
    }
    if (node2_ancestor && !attr2) || (node1 == node2 && attr1) {
        return Ok(CONTAINED_BY | FOLLOWING);
    }
    Ok(
        if tree_order(base, document, node1.node, node2.node) == std::cmp::Ordering::Less {
            PRECEDING
        } else {
            FOLLOWING
        },
    )
}

/// Whether `node` is a `DocumentFragment`.
pub(super) fn tree_is_fragment(ctx: &Ctx<'_>, node: NodeId) -> bool {
    world_for_node(ctx, node)
        .ok()
        .and_then(|world| {
            world
                .borrow()
                .document(node)
                .map(|parsed| parsed.document.is_fragment(node.node))
        })
        .unwrap_or(false)
}

/// The tree children of `node` as cross-document handles.
pub(super) fn tree_children(ctx: &Ctx<'_>, node: NodeId) -> Result<Vec<NodeId>> {
    let owner = world_for_node(ctx, node)?;
    let world = owner.borrow();
    let Some(parsed) = world.document(node) else {
        return Ok(Vec::new());
    };
    Ok(child_ids(&parsed.document.base, parsed.id, node.node))
}

/// Same-document insert returns `node`. A node from another document adopts
/// into the parent's document, preserving fragments as fragments
/// ([adopt](https://dom.spec.whatwg.org/#concept-node-adopt)).
pub(in crate::js::bindings) fn adopt_node(
    ctx: &Ctx<'_>,
    parent: NodeId,
    node: NodeId,
) -> Result<NodeId> {
    if node.document == parent.document {
        return Ok(node);
    }
    // Insert adopts each child of a DocumentFragment, not the fragment
    // (<https://dom.spec.whatwg.org/#concept-node-insert>).
    if tree_is_fragment(ctx, node) {
        return Ok(node);
    }
    adopt_across_documents(ctx, parent, node)
}

/// Adopts each child of a fragment that lives in another document into
/// `parent`'s document. The fragment itself never enters this document, so its
/// arena ids are meaningless here and must not travel
/// (<https://dom.spec.whatwg.org/#concept-node-adopt>).
///
/// Returns `None` when `node` is not such a fragment.
pub(super) fn adopt_foreign_fragment_children(
    ctx: &Ctx<'_>,
    parent: NodeId,
    node: NodeId,
) -> Result<Option<Vec<NodeId>>> {
    if node.document == parent.document || !tree_is_fragment(ctx, node) {
        return Ok(None);
    }
    let children = tree_children(ctx, node)?;
    let mut adopted = Vec::with_capacity(children.len());
    for child in children {
        adopted.push(adopt_across_documents(ctx, parent, child)?);
    }
    Ok(Some(adopted))
}

/// The replacement `node` forms for a child of `parent`: `node` itself,
/// adopted into `parent`'s document, or the children of a fragment
/// (<https://dom.spec.whatwg.org/#concept-node-replace>).
pub(super) fn replace_target(ctx: &Ctx<'_>, parent: NodeId, node: NodeId) -> Result<Replacement> {
    let node = adopt_node(ctx, parent, node)?;
    if let Some(adopted) = adopt_foreign_fragment_children(ctx, parent, node)? {
        return Ok(Replacement::Adopted(adopted));
    }
    let owner = world_for_node(ctx, parent)?;
    let owner = owner.borrow();
    let Some(parsed) = owner.document(parent) else {
        return Err(Exception::throw_type(ctx, "no document"));
    };
    Ok(if parsed.document.is_fragment(node.node) {
        Replacement::FragmentContents {
            fragment: node,
            children: fragment_children(&parsed.document, node),
        }
    } else {
        Replacement::Node(node)
    })
}

/// `ParentNode.moveBefore` (<https://dom.spec.whatwg.org/#dom-parentnode-movebefore>).
///
/// `move` is a separate primitive from insert and remove: it does not adopt,
/// and it does not run the insertion or removing steps
/// (<https://dom.spec.whatwg.org/#move>). Live ranges ride the unlink and
/// place adjustments; node iterators and shadow-tree slot assignment are
/// absent here, so those loops are empty.
pub(super) fn move_before_node(
    ctx: &Ctx<'_>,
    parent: NodeId,
    node: NodeReference,
    child: Option<NodeReference>,
) -> Result<()> {
    let node = node
        .tree()
        .ok_or_else(|| throw_dom(ctx, "HierarchyRequestError", "attributes cannot be moved"))?;
    let target = match child {
        None => MoveTarget::End,
        Some(NodeReference::Attribute { .. }) => MoveTarget::Unparented,
        Some(NodeReference::Tree(child)) if child == node => {
            let world = world(ctx)?;
            let next = world.borrow().with_document(node, |parsed| {
                sibling(&parsed.document.base, node.node, true).map(|next| NodeId {
                    document: node.document,
                    node: next,
                })
            });
            next.flatten().map_or(MoveTarget::End, MoveTarget::Before)
        }
        Some(NodeReference::Tree(child)) => MoveTarget::Before(child),
    };
    ensure_can_move(
        &world(ctx)?
            .borrow()
            .document(parent)
            .ok_or_else(|| Exception::throw_type(ctx, "stale parent"))?
            .document,
        parent,
        node,
        &target,
    )
    .map_err(|error| throw_dom_error(ctx, error))?;
    let old_parent = tree_parent(ctx, node)?;
    {
        let world = world(ctx)?;
        let world = world.borrow();
        let Some(mut parsed) = world.document_mut(parent) else {
            return Err(Exception::throw_type(ctx, "stale parent"));
        };
        move_parsed(&mut parsed, parent, node, &target);
    }
    // The `img`/`source` moving steps run inside the move, before custom
    // element reactions and mutation records
    // (<https://dom.spec.whatwg.org/#move>).
    note_picture_move(ctx, node, old_parent, parent)?;
    // A moved `iframe` container keeps its navigable but takes its new
    // position among its siblings, so `window[i]` keeps following container
    // tree order.
    reposition_moved_frame(ctx, node)?;
    let connected = world(ctx)?
        .borrow()
        .with_document(node, |parsed| {
            is_connected(&parsed.document.base, node.node)
        })
        .unwrap_or(false);
    if connected {
        // Custom-element move reactions, not the insertion/removing steps.
        // https://dom.spec.whatwg.org/#move
        // https://html.spec.whatwg.org/multipage/custom-elements.html#enqueue-a-custom-element-callback-reaction
        note_connected_move(ctx, node)?;
    }
    schedule_mutation_delivery(ctx)
}

/// Runs the `img` and `source` moving steps for a node moved by
/// `ParentNode.moveBefore`: a move across a `picture` boundary changes the
/// image's source set, which counts as a relevant mutation and re-runs
/// "update the image data" for the affected `img`
/// (<https://html.spec.whatwg.org/multipage/embedded-content.html#the-img-element>,
/// <https://html.spec.whatwg.org/multipage/embedded-content.html#the-source-element>,
/// <https://html.spec.whatwg.org/multipage/images.html#relevant-mutations>).
///
/// The spec's moving steps name only the old parent, but a shipped engine
/// reloads when the move brings the element under a `picture` too: Firefox
/// fires a second `load` for an `img` moved into a `picture`, so both sides of
/// the boundary count.
fn note_picture_move(
    ctx: &Ctx<'_>,
    moved: NodeId,
    old_parent: Option<NodeId>,
    new_parent: NodeId,
) -> Result<()> {
    let world_rc = world(ctx)?;
    let mut updates = Vec::new();
    {
        let world = world_rc.borrow();
        let Some(parsed) = world.document(moved) else {
            return Ok(());
        };
        let base = &parsed.document.base;
        let is_picture = |id: NodeId| is_html_element(base, id.node, "picture");
        if is_html_element(base, moved.node, "img") {
            if old_parent.is_some_and(is_picture) || is_picture(new_parent) {
                updates.push(moved);
            }
        } else if is_html_element(base, moved.node, "source") {
            for picture in [old_parent, Some(new_parent)].into_iter().flatten() {
                if !is_picture(picture) {
                    continue;
                }
                updates.extend(
                    child_ids(base, moved.document, picture.node)
                        .into_iter()
                        .filter(|child| is_html_element(base, child.node, "img")),
                );
            }
        }
    }
    let world = world_rc.borrow();
    for element in updates {
        if let Some(mut parsed) = world.document_mut(element) {
            parsed.document.base.restart_image(element.node);
        }
    }
    Ok(())
}

/// Re-sorts the frame of a moved `iframe` container with its new siblings.
/// Containers without a frame (never materialized) need nothing.
pub(super) fn reposition_moved_frame(ctx: &Ctx<'_>, moved: NodeId) -> Result<()> {
    let world_rc = world(ctx)?;
    let frame = world_rc.borrow().frame_for_container(moved);
    let Some(frame) = frame else {
        return Ok(());
    };
    let before = world_rc.borrow().frame_sort_before(moved);
    world_rc
        .borrow()
        .runtime
        .shared
        .borrow_mut()
        .tree
        .reposition(frame, before);
    Ok(())
}

/// Asks the custom-element shim to enqueue `connectedMoveCallback` for `node`
/// and its descendants. The shim also suppresses the removal/insertion
/// reactions those mutation records would otherwise produce.
pub(super) fn note_connected_move(ctx: &Ctx<'_>, node: NodeId) -> Result<()> {
    let Ok(hook) = crate::js::bridge::object(ctx)?.get::<_, Function>("__tbNoteConnectedMove")
    else {
        return Ok(());
    };
    let wrapped = wrap_node(ctx, node)?;
    hook.call((wrapped,))
}

/// Asks the custom-element shim to upgrade elements parsed into `node` using
/// the node document's registry
/// (<https://dom.spec.whatwg.org/#concept-create-element>).
pub(super) fn upgrade_parsed_tree(ctx: &Ctx<'_>, node: NodeId) -> Result<()> {
    let Ok(hook) = crate::js::bridge::object(ctx)?.get::<_, Function>("__tbUpgradeParsedTree")
    else {
        return Ok(());
    };
    let wrapped = wrap_node(ctx, node)?;
    hook.call((wrapped,))
}

/// Inserts the adopted `node` into `parent` before `reference`, expanding
/// fragments, journaling the move, and running the insertion fixups. The
/// caller owns validity and adoption.
pub(in crate::js::bindings) fn insert_converted_node(
    ctx: &Ctx<'_>,
    parent: NodeId,
    node: NodeId,
) -> Result<()> {
    // Convert-nodes-into-a-node appends each node to a fragment. Records
    // queue here; delivery waits for the caller so observers see every
    // removal together
    // (<https://dom.spec.whatwg.org/#convert-nodes-into-a-node>).
    insert_tree_node_inner(ctx, parent, node, None, false)
}

pub(in crate::js::bindings) fn insert_tree_node(
    ctx: &Ctx<'_>,
    parent: NodeId,
    node: NodeId,
    reference: Option<NodeId>,
) -> Result<()> {
    insert_tree_node_inner(ctx, parent, node, reference, true)
}

/// Runs the realm side of an insertion after the tree mutation: form-control
/// fixups, HTML insertion steps (scripts, iframes, referrer policy, nonce
/// clearing), then observer delivery. The tree work lives in `dom`.
pub(super) fn apply_inserted(
    ctx: &Ctx<'_>,
    parent: NodeId,
    moved: &[NodeId],
    deliver: bool,
) -> Result<()> {
    fixup_controls_on_insert(ctx, parent.document, moved)?;
    // Insertion steps run while parent is connected. Queue mutation records
    // first so an inserted script can `takeRecords` its own addition
    // (<https://html.spec.whatwg.org/multipage/scripting.html#script-processing-model:html-element-insertion-steps>,
    // <https://dom.spec.whatwg.org/#concept-node-insert>).
    run_html_insertion_steps(ctx, parent, moved)?;
    if deliver {
        schedule_mutation_delivery(ctx)?;
    }
    Ok(())
}

/// Inserts an adopted tree node, recording journal entries for observers.
pub(super) fn insert_tree_node_inner(
    ctx: &Ctx<'_>,
    parent: NodeId,
    node: NodeId,
    reference: Option<NodeId>,
    deliver: bool,
) -> Result<()> {
    let world_rc = world(ctx)?;
    if let Some(moved) = adopt_foreign_fragment_children(ctx, parent, node)? {
        {
            let world = world_rc.borrow();
            let Some(mut parsed) = world.document_mut(parent) else {
                return Err(Exception::throw_type(ctx, "no document"));
            };
            for child in &moved {
                insert_parsed(&mut parsed, parent, *child, reference);
            }
        }
        return apply_inserted(ctx, parent, &moved, deliver);
    }
    let moved: Vec<NodeId> = {
        let owner = world_for_node(ctx, node)?;
        let world = owner.borrow();
        let Some(parsed) = world.document(node) else {
            return Err(Exception::throw_type(ctx, "no document"));
        };
        if parsed.document.is_fragment(node.node) {
            fragment_children(&parsed.document, node)
        } else {
            vec![node]
        }
    };
    {
        let world = world_rc.borrow();
        let Some(mut parsed) = world.document_mut(parent) else {
            return Err(Exception::throw_type(ctx, "no document"));
        };
        insert_parsed(&mut parsed, parent, node, reference);
    }
    apply_inserted(ctx, parent, &moved, deliver)
}

/// Records the executing script for inserted `<img>` and `<script>` elements,
/// so a later CSP violation for their loads can report `sourceFile`,
/// `lineNumber`, and `columnNumber`. Insertions that trigger no fetch
/// (disconnected parents, non-URL elements) still record; the entry is only
/// read when a violation fires. Parser-inserted content never passes through
/// here, so it reports no source
/// (<https://w3c.github.io/webappsec-csp/#create-violation-for-global>).
fn note_inserted_creators(ctx: &Ctx<'_>, inserted: &[NodeId]) {
    let Ok(world_rc) = world(ctx) else {
        return;
    };
    let targets: Vec<NodeId> = {
        let world = world_rc.borrow();
        let mut targets = Vec::new();
        for root in inserted {
            let Some(parsed) = world.document(*root) else {
                continue;
            };
            let base = &parsed.document.base;
            let mut stack = vec![root.node];
            while let Some(id) = stack.pop() {
                let Some(node) = base.get_node(id) else {
                    continue;
                };
                stack.extend(node.children.iter().copied());
                let is_url_element = node.data.downcast_element().is_some_and(|element| {
                    element.name.ns == crate::dom::tree::html_namespace()
                        && matches!(element.name.local.as_str(), "img" | "script")
                });
                if is_url_element {
                    targets.push(NodeId {
                        document: parsed.id,
                        node: id,
                    });
                }
            }
        }
        targets
    };
    // Only capture a frame when an attribution target exists: most insertions
    // are text and plain elements, and the lookup is pure overhead.
    if targets.is_empty() {
        return;
    }
    let Some(creator) = crate::js::error_stack_top(ctx) else {
        return;
    };
    for target in targets {
        // The entry lives in the element owner's world, where a later
        // violation for its load looks it up: a script mutating another
        // frame's DOM must not attribute to its own realm.
        let Ok(owner_rc) = world_for_node(ctx, target) else {
            continue;
        };
        // The earliest trigger wins: a `src` set already recorded the
        // mutation that chose the URL, and insertion only starts this load
        // when nothing did. A disconnect clears the entry, so a moved
        // element attributes its new load to the move.
        owner_rc
            .borrow_mut()
            .state
            .creator_stacks
            .entry(target)
            .or_insert_with(|| {
                let (url, line, column) = creator.clone();
                crate::js::CreatorInfo { url, line, column }
            });
    }
}

/// HTML insertion and script children-changed steps after a tree insert, in
/// tree order: an earlier script must not observe a later iframe's browsing
/// context, and an earlier iframe must already have one
/// (<https://html.spec.whatwg.org/multipage/scripting.html#script-processing-model:html-element-insertion-steps>,
/// <https://html.spec.whatwg.org/multipage/iframe-embed-object.html#the-iframe-element:html-element-insertion-steps>,
/// <https://html.spec.whatwg.org/multipage/scripting.html#script-processing-model:css-element-children-changed-steps>,
/// <https://dom.spec.whatwg.org/#concept-node-insert>).
pub(super) fn run_html_insertion_steps(
    ctx: &Ctx<'_>,
    parent: NodeId,
    inserted: &[NodeId],
) -> Result<()> {
    let connected = world(ctx)?.borrow().with_document(parent, |parsed| {
        is_connected(&parsed.document.base, parent.node)
    });
    if !connected.unwrap_or(false) {
        return Ok(());
    }
    note_inserted_creators(ctx, inserted);
    drain_mutation_journal(ctx)?;
    let mut steps = Vec::new();
    {
        let world = world(ctx)?;
        let world = world.borrow();
        let Some(parsed) = world.document(parent) else {
            return Ok(());
        };
        let base = &parsed.document.base;
        if is_html_element(base, parent.node, "script") {
            steps.push(InsertionStep::Script(parent));
        }
        for root in inserted {
            collect_html_insertion_steps(base, *root, &mut steps);
        }
    }
    for step in steps {
        match step {
            InsertionStep::Iframe(id) => {
                World::materialize_iframe(&world(ctx)?, id);
                // The initial `load` is an element task, not part of the
                // insertion steps: the engine fires it after the child frame's
                // realm exists, so `contentWindow.location` is readable in the
                // handler
                // (<https://html.spec.whatwg.org/multipage/iframe-embed-object.html#the-iframe-element:html-element-insertion-steps>).
            }
            InsertionStep::Script(id) => prepare_classic_script(ctx, id)?,
            InsertionStep::Meta(id) => {
                apply_meta_referrer(ctx, id);
                apply_meta_csp(ctx, id);
                apply_meta_refresh(ctx, id);
            }
            InsertionStep::Base(id) => {
                world(ctx)?.borrow_mut().apply_base_element(id);
            }
        }
    }
    // Spec gates this on a header-delivered CSP
    // (<https://html.spec.whatwg.org/multipage/urls-and-fetching.html#nonce-attributes>).
    // Chromium and `moveBefore/nonce.html` clear the content attribute
    // whenever the element becomes browsing-context connected; `move` does
    // not disconnect, so the attribute stays.
    clear_nonce_on_connect(ctx, inserted)?;
    Ok(())
}

/// Wraps the first node `find` selects in `id`'s document, or `null`.
pub(super) fn document_value<'js>(
    ctx: &Ctx<'js>,
    id: NodeId,
    find: impl FnOnce(&crate::Parsed) -> Option<NodeId>,
) -> Result<Value<'js>> {
    let world = world(ctx)?;
    let found = world.borrow().document(id).and_then(|parsed| find(&parsed));
    child_value(ctx, found)
}

/// Map generated `(Node or DOMString)` members into the shared insertion
/// representation. The enums differ per module but hold the same types.
pub(super) fn union_nodes<'js, T>(nodes: Vec<T>) -> Vec<NodeOrString<'js>>
where
    NodeOrString<'js>: From<T>,
{
    nodes.into_iter().map(NodeOrString::from).collect()
}

pub(super) fn pi_target(ctx: &Ctx<'_>, id: NodeId) -> String {
    let Ok(world) = world(ctx) else {
        return String::new();
    };
    let world = world.borrow();
    world
        .document(id)
        .and_then(|parsed| {
            parsed
                .document
                .base
                .get_node(id.node)
                .and_then(|node| match &node.data {
                    NodeData::ProcessingInstruction { target, .. } => Some(target.clone()),
                    _ => None,
                })
        })
        .unwrap_or_default()
}

/// The DOM-side pseudo-attribute map. Creation and data writes derive it from
/// the data; an attribute write stores it as given, so it can hold names the
/// XML grammar rejects. The style-sheet loader parses the data itself
/// (<https://dom.spec.whatwg.org/#processinginstruction-initialize>).
pub(super) fn pi_attributes(ctx: &Ctx<'_>, id: NodeId) -> Result<Vec<(String, String)>> {
    let owner = world_for_node(ctx, id)?;
    Ok(owner
        .borrow()
        .document(id)
        .and_then(|parsed| {
            parsed
                .document
                .base
                .get_node(id.node)
                .and_then(|node| match &node.data {
                    NodeData::ProcessingInstruction { attributes, .. } => Some(attributes.clone()),
                    _ => None,
                })
        })
        .unwrap_or_default())
}

pub(super) fn doctype_field(
    ctx: &Ctx<'_>,
    id: NodeId,
    field: fn(&str, &str, &str) -> String,
) -> String {
    let Ok(world) = world(ctx) else {
        return String::new();
    };
    let world = world.borrow();
    world
        .document(id)
        .and_then(|parsed| {
            parsed
                .document
                .base
                .get_node(id.node)
                .and_then(|node| match &node.data {
                    NodeData::Doctype {
                        name,
                        public_id,
                        system_id,
                    } => Some(field(name, public_id, system_id)),
                    _ => None,
                })
        })
        .unwrap_or_default()
}

/// Splits a `Text` node at `offset`
/// (<https://dom.spec.whatwg.org/#concept-text-split>).
///
/// The new node is always a `Text` node per the split-a-Text-node steps,
/// even when `id` is a `CDATASection` (which inherits `Text`). The insert
/// carries the generic range adjustment (spec step 7.1); the split steps
/// then move node points over and bump the equal parent slot; the truncate
/// writes without the generic replacement rule, which the split steps have
/// already accounted for.
pub(in crate::js::bindings) fn split_text_node<'js>(
    ctx: &Ctx<'js>,
    id: NodeId,
    offset: u32,
) -> Result<Value<'js>> {
    let units = character_data(ctx, id)?.units().into_owned();
    let offset = character_data_offset(ctx, offset, units.len())?;
    let new_data = DomString::from_utf16(units[offset..].to_vec());
    let (parent, next, index) = world(ctx)?
        .borrow()
        .with_document(id, |parsed| {
            let base = &parsed.document.base;
            let parent = base
                .get_node(id.node)
                .and_then(|node| node.parent)
                .map(|node| NodeId {
                    document: id.document,
                    node,
                });
            let next = sibling(base, id.node, true).map(|node| NodeId {
                document: id.document,
                node,
            });
            let index = parent.and_then(|parent| {
                base.get_node(parent.node)
                    .and_then(|node| node.children.iter().position(|sibling| *sibling == id.node))
                    .and_then(|index| u32::try_from(index).ok())
            });
            (parent, next, index)
        })
        .unwrap_or((None, None, None));
    let new_id = {
        let world = world(ctx)?;
        let world = world.borrow();
        let Some(mut parsed) = world.document_mut(id) else {
            return Err(Exception::throw_type(ctx, "stale node"));
        };
        // https://dom.spec.whatwg.org/#create-a-text-node
        let blitz = parsed.document.create_text(&new_data);
        NodeId {
            document: parsed.id,
            node: blitz,
        }
    };
    if let Some(parent) = parent {
        // Spec order: the insert shifts parent points past the new node's
        // slot (7.1), the split steps move node points over and bump the
        // equal slot (7.2-7.5), then the truncate writes without the
        // generic replacement rule
        // (<https://dom.spec.whatwg.org/#concept-text-split>).
        {
            let world_rc = world(ctx)?;
            let world = world_rc.borrow();
            let Some(mut parsed) = world.document_mut(id) else {
                return Err(Exception::throw_type(ctx, "stale node"));
            };
            crate::dom::insertion::place_journaled(&mut parsed, parent, new_id, next);
            crate::dom::range::adjust_for_split(
                &mut parsed.document.side.ranges,
                id,
                new_id,
                parent,
                index.unwrap_or(0),
                u32::try_from(offset).unwrap_or(u32::MAX),
            );
            let mut kept = units.clone();
            kept.drain(offset..);
            truncate_split_text(&mut parsed, id, &kept, DomString::from_utf16(units));
        }
        apply_inserted(ctx, parent, &[new_id], true)?;
    } else {
        {
            let world_rc = world(ctx)?;
            let world = world_rc.borrow();
            let Some(mut parsed) = world.document_mut(id) else {
                return Err(Exception::throw_type(ctx, "stale node"));
            };
            // Without a parent the split's range steps do not run at all, so
            // the trailing `replace data` rule collapses the points past the
            // split offset
            // (<https://dom.spec.whatwg.org/#concept-text-split>).
            let offset_units = u32::try_from(offset).unwrap_or(u32::MAX);
            let count = u32::try_from(units.len().saturating_sub(offset)).unwrap_or(u32::MAX);
            crate::dom::range::adjust_for_replace_data(
                &mut parsed.document.side.ranges,
                id,
                offset_units,
                count,
                0,
            );
            let mut kept = units.clone();
            kept.drain(offset..);
            truncate_split_text(&mut parsed, id, &kept, DomString::from_utf16(units));
        }
        schedule_mutation_delivery(ctx)?;
    }
    wrap_node(ctx, new_id)
}

/// Truncate `id`'s data to `kept` for `splitText`, recording the change. The
/// range steps already ran, so this writes without the generic replacement
/// rule that would corrupt a split. Other node kinds cannot reach a split.
fn truncate_split_text(parsed: &mut crate::Parsed, id: NodeId, kept: &[u16], old_value: DomString) {
    let wrote = match parsed
        .document
        .base
        .get_node(id.node)
        .map(|node| &node.data)
    {
        Some(NodeData::Text(_)) => {
            parsed
                .document
                .base
                .mutate()
                .set_node_text(id.node, &String::from_utf16_lossy(kept));
            true
        }
        Some(NodeData::CDataSection { .. }) => {
            let base = &mut parsed.document.base;
            base.snapshot_node_state_only(id.node);
            if let Some(NodeData::CDataSection { contents }) =
                base.get_node_mut(id.node).map(|node| &mut node.data)
            {
                contents.clear();
                contents.push_str(&String::from_utf16_lossy(kept));
            }
            true
        }
        _ => false,
    };
    if wrote {
        parsed.document.record(JournalEntry::CharacterData {
            target: id,
            old_value,
        });
    }
}

/// Concatenates the data of the contiguous `Text` nodes of `id`, in tree
/// order (<https://dom.spec.whatwg.org/#contiguous-text-nodes>).
///
/// A `CDATASection` is a `Text` node, so it stays in the run. A comment or
/// element sibling ends it.
pub(super) fn whole_text(ctx: &Ctx<'_>, id: NodeId) -> Result<DomString> {
    let world = world(ctx)?;
    Ok(world
        .borrow()
        .with_document(id, |parsed| {
            let mut prefix = Vec::new();
            let mut cursor = sibling(&parsed.document.base, id.node, false);
            while let Some(current) = cursor {
                if !is_text_node(&parsed.document.base, current) {
                    break;
                }
                prefix.push(current);
                cursor = sibling(&parsed.document.base, current, false);
            }
            let mut combined = DomString::default();
            for node in prefix.into_iter().rev() {
                append_text_data(&parsed.document, node, &mut combined);
            }
            append_text_data(&parsed.document, id.node, &mut combined);
            let mut cursor = sibling(&parsed.document.base, id.node, true);
            while let Some(current) = cursor {
                if !is_text_node(&parsed.document.base, current) {
                    break;
                }
                append_text_data(&parsed.document, current, &mut combined);
                cursor = sibling(&parsed.document.base, current, true);
            }
            combined
        })
        .unwrap_or_default())
}

/// Whether `id` is a text node for `wholeText`: `Text` or `CDATASection`,
/// both in the contiguous run
/// (<https://dom.spec.whatwg.org/#contiguous-text-nodes>).
pub(super) fn is_text_node(base: &blitz_dom::BaseDocument, id: BlitzId) -> bool {
    matches!(
        base.get_node(id).map(|node| &node.data),
        Some(NodeData::Text(_) | NodeData::CDataSection { .. })
    )
}

/// Appends `id`'s data to `out` when it is a text node, for `wholeText`
/// (<https://dom.spec.whatwg.org/#dom-text-wholetext>).
pub(super) fn append_text_data(
    doc: &crate::documents::BlitzDocument,
    id: BlitzId,
    out: &mut DomString,
) {
    if matches!(
        doc.base.get_node(id).map(|node| &node.data),
        Some(NodeData::Text(_) | NodeData::CDataSection { .. })
    ) {
        out.push_dom(&doc.character_data(id));
    }
}

pub(super) fn install(ctx: &Ctx<'_>) -> Result<()> {
    node_generated::install(ctx)?;
    character_data_generated::install(ctx)?;
    text_generated::install(ctx)?;
    processing_instruction_generated::install(ctx)?;
    document_type_generated::install(ctx)?;
    parent_node_generated::install(ctx)?;
    child_node_generated::install(ctx)?;
    non_document_type_child_node_generated::install(ctx)?;
    Ok(())
}
