//! Document interfaces: `Document`, `DocumentFragment`, `ShadowRoot`,
//! templates, and framed documents.

use super::super::{
    CollectionKind, JsImplementation, LegacyNullString, NodeContext, OptString, WebIdlCodeUnits,
    WebIdlString, attr_wrapper, child_value, deref_weak, document_base_url_string,
    document_is_html, document_is_html_content, document_url_string, element_at_point_ctx,
    elements_by_tag, find_element_by_id, fixup_focus_after_removal, host, host_node_id,
    import_attr, is_main_document, live_collection, make_weak, new_detached_attr,
    node_is_shadow_root, required_node, schedule_mutation_delivery, set_attribute_sync, throw_dom,
    throw_dom_error, valid_attribute_local_name, valid_element_local_name, validate_and_extract,
    with_node_data, world, world_for_node, wrap_new_document_in_world, wrap_node,
};
use crate::dom::adoption::{ImportSnapshot, import_snapshot, materialize_import};
use rquickjs::function::Rest;

use std::cell::RefCell;
use std::rc::Rc;

use blitz_dom::NodeData;
use markup5ever::{LocalName, Namespace, QualName};

use rquickjs::{Ctx, Exception, Function, Persistent, Result, Value, context::EvalOptions};

use crate::js::events;
use crate::js::world::World;

use crate::dom::insertion::{
    InsertionStep, collect_html_insertion_steps, collect_nonce_elements, insert_parsed,
    place_journaled, replace_parsed,
};
use crate::dom::tree::{
    BlitzId, NodeId, html_namespace, is_connected, is_html_element, qualified_name, sibling,
    svg_namespace,
};

use super::super::adopt_into_document;
use crate::js::world::{DocumentStreamCommand, NodeReference, SameObject};

use crate::ReadyState;

use crate::js::bindings::enumerated::{Defaults, Dir, reflect_enumerated};

use super::JsNode;
use super::core::{adopt_node, document_value};
use super::element::{content_attr, is_element_data};

include!(concat!(env!("OUT_DIR"), "/Document.rs"));
include!(concat!(env!("OUT_DIR"), "/DocumentFragment.rs"));
include!(concat!(env!("OUT_DIR"), "/ShadowRoot.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLTemplateElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLIFrameElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLFrameElement.rs"));

impl<'js> document_generated::Document<'js> for JsNode {
    // https://dom.spec.whatwg.org/#dom-document-implementation
    fn get_implementation(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        self.implementation(ctx)
    }

    // https://dom.spec.whatwg.org/#dom-document-documentelement
    fn get_document_element(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        self.document_element(ctx)
    }

    // https://html.spec.whatwg.org/multipage/dom.html#dom-document-body
    fn get_body(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        self.body(ctx)
    }

    // https://html.spec.whatwg.org/multipage/dom.html#dom-document-body
    fn set_body(&self, ctx: &Ctx<'js>, value: Value<'js>) -> Result<()> {
        self.set_body(ctx, value)
    }

    // https://html.spec.whatwg.org/multipage/dom.html#dom-document-head
    fn get_head(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        self.head(ctx)
    }

    // https://html.spec.whatwg.org/multipage/dom.html#dom-document-currentscript
    fn get_current_script(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        self.current_script(ctx)
    }

    // https://dom.spec.whatwg.org/#dom-document-doctype
    fn get_doctype(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        Ok(self.doctype(ctx))
    }

    // https://dom.spec.whatwg.org/#dom-document-documenturi
    fn get_document_uri(&self, ctx: &Ctx<'js>) -> Result<crate::dom_string::DomString> {
        self.get_url(ctx)
    }

    // https://dom.spec.whatwg.org/#dom-document-url
    fn get_url(&self, ctx: &Ctx<'js>) -> Result<crate::dom_string::DomString> {
        Ok(self.url(ctx))
    }

    // https://html.spec.whatwg.org/multipage/dom.html#dom-document-referrer
    fn get_referrer(&self, ctx: &Ctx<'js>) -> Result<crate::dom_string::DomString> {
        Ok(
            crate::js::bindings::document::document_referrer_string(ctx, self.handle.get().0)
                .into(),
        )
    }

    // https://dom.spec.whatwg.org/#dom-document-compatmode
    fn get_compat_mode(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        let mode = self.compat_mode(ctx)?;
        rquickjs::String::from_str(ctx.clone(), &mode)
    }

    /// <https://encoding.spec.whatwg.org/#dom-document-characterset>
    fn get_character_set(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        rquickjs::String::from_str(ctx.clone(), self.character_set(ctx))
    }

    /// <https://encoding.spec.whatwg.org/#dom-document-charset>
    fn get_charset(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        self.get_character_set(ctx)
    }

    /// <https://encoding.spec.whatwg.org/#dom-document-inputencoding>
    fn get_input_encoding(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        self.get_character_set(ctx)
    }

    // https://dom.spec.whatwg.org/#dom-document-contenttype
    fn get_content_type(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        let content_type = self.content_type(ctx)?;
        rquickjs::String::from_str(ctx.clone(), content_type)
    }

    // https://html.spec.whatwg.org/multipage/dom.html#dom-document-readystate
    fn get_ready_state(&self, ctx: &Ctx<'js>) -> Result<document_generated::DocumentReadyState> {
        Ok(match self.ready_state(ctx)?.as_str() {
            "interactive" => document_generated::DocumentReadyState::Interactive,
            "complete" => document_generated::DocumentReadyState::Complete,
            _ => document_generated::DocumentReadyState::Loading,
        })
    }

    // https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-document-defaultview
    fn get_default_view(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        self.default_view(ctx)
    }

    // https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-document-location
    fn get_location(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        self.location(ctx)
    }

    // https://html.spec.whatwg.org/multipage/interaction.html#dom-document-hasfocus
    fn has_focus(&self, ctx: Ctx<'js>) -> Result<bool> {
        Ok(self.has_focus(&ctx))
    }

    // https://dom.spec.whatwg.org/#dom-document-activeelement
    fn get_active_element(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        self.active_element(ctx)
    }

    // https://html.spec.whatwg.org/multipage/dom.html#dom-document-title
    fn get_title(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        self.title(ctx)
    }

    // https://html.spec.whatwg.org/multipage/dom.html#dom-document-title
    fn set_title(&self, ctx: &Ctx<'js>, value: rquickjs::String<'js>) -> Result<()> {
        // `document.title` keeps every code unit, including lone surrogates.
        self.set_title(
            ctx,
            WebIdlCodeUnits(crate::dom_string::DomString::from_utf16(value.to_utf16()?)),
        )
    }

    // `document.dir` reflects the `dir` attribute of the document element
    // (<https://html.spec.whatwg.org/multipage/dom.html#dom-document-dir>).
    fn get_dir(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        // Shared borrows nest: `content_attr` borrows the same world.
        let owner = world_for_node(ctx, self.handle.get().0)?;
        let value = owner
            .borrow()
            .document(self.handle.get().0)
            .and_then(|parsed| document_first_child(&parsed, is_element_data))
            .and_then(|element| content_attr(ctx, element, "dir"));
        rquickjs::String::from_str(
            ctx.clone(),
            reflect_enumerated::<Dir>(value.as_deref(), Defaults::NONE),
        )
    }

    // https://html.spec.whatwg.org/multipage/dom.html#dom-document-dir
    fn set_dir(&self, ctx: &Ctx<'js>, value: rquickjs::String<'js>) -> Result<()> {
        let owner = world_for_node(ctx, self.handle.get().0)?;
        let element = owner
            .borrow()
            .document(self.handle.get().0)
            .and_then(|parsed| document_first_child(&parsed, is_element_data));
        if let Some(element) = element {
            set_attribute_sync(ctx, element, "dir", &value.to_string()?)?;
        }
        Ok(())
    }

    // Obsolete document color attributes reflect the matching `body`
    // content attributes
    // (<https://html.spec.whatwg.org/multipage/obsolete.html#dom-document-bgcolor>).
    fn get_fg_color(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        document_body_attr(ctx, self.handle.get().0, "text")
    }

    // https://html.spec.whatwg.org/multipage/obsolete.html#dom-document-bgcolor
    fn set_fg_color(&self, ctx: &Ctx<'js>, value: rquickjs::String<'js>) -> Result<()> {
        set_document_body_attr(ctx, self.handle.get().0, "text", &value.to_string()?)
    }

    // https://html.spec.whatwg.org/multipage/obsolete.html#dom-document-bgcolor
    fn get_link_color(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        document_body_attr(ctx, self.handle.get().0, "link")
    }

    // https://html.spec.whatwg.org/multipage/obsolete.html#dom-document-bgcolor
    fn set_link_color(&self, ctx: &Ctx<'js>, value: rquickjs::String<'js>) -> Result<()> {
        set_document_body_attr(ctx, self.handle.get().0, "link", &value.to_string()?)
    }

    // https://html.spec.whatwg.org/multipage/obsolete.html#dom-document-bgcolor
    fn get_vlink_color(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        document_body_attr(ctx, self.handle.get().0, "vlink")
    }

    // https://html.spec.whatwg.org/multipage/obsolete.html#dom-document-bgcolor
    fn set_vlink_color(&self, ctx: &Ctx<'js>, value: rquickjs::String<'js>) -> Result<()> {
        set_document_body_attr(ctx, self.handle.get().0, "vlink", &value.to_string()?)
    }

    // https://html.spec.whatwg.org/multipage/obsolete.html#dom-document-bgcolor
    fn get_alink_color(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        document_body_attr(ctx, self.handle.get().0, "alink")
    }

    // https://html.spec.whatwg.org/multipage/obsolete.html#dom-document-bgcolor
    fn set_alink_color(&self, ctx: &Ctx<'js>, value: rquickjs::String<'js>) -> Result<()> {
        set_document_body_attr(ctx, self.handle.get().0, "alink", &value.to_string()?)
    }

    // https://html.spec.whatwg.org/multipage/obsolete.html#dom-document-bgcolor
    fn get_bg_color(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        document_body_attr(ctx, self.handle.get().0, "bgcolor")
    }

    // https://html.spec.whatwg.org/multipage/obsolete.html#dom-document-bgcolor
    fn set_bg_color(&self, ctx: &Ctx<'js>, value: rquickjs::String<'js>) -> Result<()> {
        set_document_body_attr(ctx, self.handle.get().0, "bgcolor", &value.to_string()?)
    }

    // https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-document-write
    fn write(&self, ctx: Ctx<'js>, arg_0: Vec<rquickjs::String<'js>>) -> Result<()> {
        let mut text = Vec::with_capacity(arg_0.len());
        for part in arg_0 {
            text.push(WebIdlString(part.to_string()?));
        }
        self.write(ctx, text)
    }

    // https://dom.spec.whatwg.org/#dom-document-getelementsbyclassname
    fn get_elements_by_class_name(
        &self,
        ctx: Ctx<'js>,
        arg_0: rquickjs::String<'js>,
    ) -> Result<Value<'js>> {
        self.get_elements_by_class_name(ctx, WebIdlString(arg_0.to_string()?))
    }

    // https://dom.spec.whatwg.org/#dom-document-createevent
    fn create_event(&self, ctx: Ctx<'js>, arg_0: rquickjs::String<'js>) -> Result<Value<'js>> {
        let interface = arg_0.to_string()?;
        events::create_event(&ctx, &interface)
    }

    // https://dom.spec.whatwg.org/#dom-document-createelement
    fn create_element(
        &self,
        ctx: Ctx<'js>,
        arg_0: rquickjs::String<'js>,
        _options: document_generated::DOMStringOrElementCreationOptions,
    ) -> Result<Value<'js>> {
        self.create_element(ctx, WebIdlString(arg_0.to_string()?))
    }

    // https://dom.spec.whatwg.org/#dom-document-createelementns
    fn create_element_ns(
        &self,
        ctx: Ctx<'js>,
        arg_0: Option<rquickjs::String<'js>>,
        arg_1: rquickjs::String<'js>,
        _options: document_generated::DOMStringOrElementCreationOptions,
    ) -> Result<Value<'js>> {
        self.create_element_ns(
            ctx,
            OptString(arg_0.map(|name| name.to_string()).transpose()?),
            WebIdlString(arg_1.to_string()?),
        )
    }

    // https://dom.spec.whatwg.org/#dom-document-createtextnode
    fn create_text_node(&self, ctx: Ctx<'js>, arg_0: rquickjs::String<'js>) -> Result<Value<'js>> {
        self.create_text_node(
            ctx,
            WebIdlCodeUnits(crate::dom_string::DomString::from_utf16(arg_0.to_utf16()?)),
        )
    }

    // https://dom.spec.whatwg.org/#dom-document-createcomment
    fn create_comment(&self, ctx: Ctx<'js>, arg_0: rquickjs::String<'js>) -> Result<Value<'js>> {
        self.create_comment(
            ctx,
            WebIdlCodeUnits(crate::dom_string::DomString::from_utf16(arg_0.to_utf16()?)),
        )
    }

    // https://dom.spec.whatwg.org/#dom-document-createprocessinginstruction
    fn create_processing_instruction(
        &self,
        ctx: Ctx<'js>,
        arg_0: rquickjs::String<'js>,
        arg_1: rquickjs::String<'js>,
    ) -> Result<Value<'js>> {
        self.create_processing_instruction(
            ctx,
            WebIdlString(arg_0.to_string()?),
            WebIdlCodeUnits(crate::dom_string::DomString::from_utf16(arg_1.to_utf16()?)),
        )
    }

    // https://dom.spec.whatwg.org/#dom-document-createcdatasection
    fn create_cdata_section(
        &self,
        ctx: Ctx<'js>,
        arg_0: rquickjs::String<'js>,
    ) -> Result<Value<'js>> {
        self.create_cdata_section(
            ctx,
            WebIdlCodeUnits(crate::dom_string::DomString::from_utf16(arg_0.to_utf16()?)),
        )
    }

    // https://dom.spec.whatwg.org/#dom-document-createattribute
    fn create_attribute(&self, ctx: Ctx<'js>, arg_0: rquickjs::String<'js>) -> Result<Value<'js>> {
        self.create_attribute(ctx, WebIdlString(arg_0.to_string()?))
    }

    // https://dom.spec.whatwg.org/#dom-document-createattributens
    fn create_attribute_ns(
        &self,
        ctx: Ctx<'js>,
        arg_0: Option<rquickjs::String<'js>>,
        arg_1: rquickjs::String<'js>,
    ) -> Result<Value<'js>> {
        self.create_attribute_ns(
            ctx,
            OptString(arg_0.map(|name| name.to_string()).transpose()?),
            WebIdlString(arg_1.to_string()?),
        )
    }

    // https://dom.spec.whatwg.org/#dom-document-createdocumentfragment
    fn create_document_fragment(&self, ctx: Ctx<'js>) -> Result<Value<'js>> {
        self.create_document_fragment(ctx)
    }

    // https://dom.spec.whatwg.org/#dom-document-createrange
    fn create_range(&self, ctx: Ctx<'js>) -> Result<Value<'js>> {
        super::super::range::create_range_value(&ctx, self.handle.get().0)
    }

    // https://dom.spec.whatwg.org/#dom-document-importnode
    fn import_node(
        &self,
        ctx: Ctx<'js>,
        arg_0: NodeReference,
        arg_1: document_generated::BooleanOrImportNodeOptions,
    ) -> Result<Value<'js>> {
        // `ImportNodeOptions` inverts the boolean form: selecting only the
        // node itself is a shallow import.
        let deep = match arg_1 {
            document_generated::BooleanOrImportNodeOptions::Boolean(deep) => deep,
            document_generated::BooleanOrImportNodeOptions::ImportNodeOptions(options) => {
                !options.self_only
            }
        };
        match arg_0 {
            NodeReference::Tree(id) => {
                let node = wrap_node(&ctx, id)?;
                self.import_node(ctx, node, deep)
            }
            NodeReference::Attribute { scope, id } => {
                import_attr(&ctx, self.handle.get().0, scope, id)
            }
        }
    }

    // https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-document-open
    // Both arguments are ignored upstream; the navigation overload stays
    // absent with the rest of navigation.
    fn open(
        &self,
        ctx: Ctx<'js>,
        _arg_0: Option<rquickjs::String<'js>>,
        _arg_1: Option<rquickjs::String<'js>>,
    ) -> Result<Value<'js>> {
        self.open_document(ctx)
    }

    // https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-document-close
    fn close(&self, ctx: Ctx<'js>) -> Result<()> {
        self.close_document(ctx)
    }

    // https://dom.spec.whatwg.org/#dom-nonelementparentnode-getelementbyid
    fn get_element_by_id(&self, ctx: Ctx<'js>, arg_0: rquickjs::String<'js>) -> Result<Value<'js>> {
        self.get_element_by_id(ctx, WebIdlString(arg_0.to_string()?))
    }

    // https://dom.spec.whatwg.org/#dom-document-getelementsbytagname
    fn get_elements_by_tag_name(
        &self,
        ctx: Ctx<'js>,
        arg_0: rquickjs::String<'js>,
    ) -> Result<Value<'js>> {
        self.get_elements_by_tag_name(ctx, WebIdlString(arg_0.to_string()?))
    }

    // https://dom.spec.whatwg.org/#dom-document-getelementsbytagnamens
    fn get_elements_by_tag_name_ns(
        &self,
        ctx: Ctx<'js>,
        arg_0: Option<rquickjs::String<'js>>,
        arg_1: rquickjs::String<'js>,
    ) -> Result<Value<'js>> {
        self.get_elements_by_tag_name_ns(
            ctx,
            OptString(arg_0.map(|name| name.to_string()).transpose()?),
            WebIdlString(arg_1.to_string()?),
        )
    }

    // https://html.spec.whatwg.org/multipage/dom.html#dom-document-getelementsbyname
    fn get_elements_by_name(
        &self,
        ctx: Ctx<'js>,
        arg_0: rquickjs::String<'js>,
    ) -> Result<Value<'js>> {
        self.get_elements_by_name(ctx, WebIdlString(arg_0.to_string()?))
    }

    // https://drafts.csswg.org/cssom-view/#dom-document-elementfrompoint
    fn element_from_point(&self, ctx: Ctx<'js>, arg_0: f64, arg_1: f64) -> Result<Value<'js>> {
        self.element_from_point(ctx, arg_0, arg_1)
    }

    // https://drafts.csswg.org/cssom-view/#dom-document-elementsfrompoint
    fn elements_from_point(
        &self,
        ctx: Ctx<'js>,
        arg_0: f64,
        arg_1: f64,
    ) -> Result<Vec<Value<'js>>> {
        self.elements_from_point(ctx, arg_0, arg_1)
    }

    // https://dom.spec.whatwg.org/#dom-document-adoptnode
    fn adopt_node(&self, ctx: Ctx<'js>, arg_0: host::NodeReference) -> Result<Value<'js>> {
        self.adopt_node(ctx, arg_0)
    }

    // https://html.spec.whatwg.org/multipage/dom.html#dom-document-scripts
    fn get_scripts(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        self.get_scripts(ctx)
    }

    // https://html.spec.whatwg.org/multipage/dom.html#dom-document-links
    fn get_links(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        self.get_links(ctx)
    }
}

impl<'js> document_fragment_generated::DocumentFragment<'js> for JsNode {
    // https://dom.spec.whatwg.org/#dom-nonelementparentnode-getelementbyid
    fn get_element_by_id(&self, ctx: Ctx<'js>, id: rquickjs::String<'js>) -> Result<Value<'js>> {
        element_by_id(&ctx, self.handle.get().0, &id.to_string()?)
    }
}

impl<'js> shadow_root_generated::ShadowRoot<'js> for JsNode {
    // https://dom.spec.whatwg.org/#dom-shadowroot-host
    fn get_host(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        self.host(ctx)
    }

    // https://dom.spec.whatwg.org/#dom-shadowroot-mode
    fn get_mode(&self, ctx: &Ctx<'js>) -> Result<shadow_root_generated::ShadowRootMode> {
        Ok(match self.mode(ctx)?.as_str() {
            "closed" => shadow_root_generated::ShadowRootMode::Closed,
            _ => shadow_root_generated::ShadowRootMode::Open,
        })
    }

    // https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-innerhtml
    fn get_inner_html(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        self.inner_html(ctx)
    }

    // https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-innerhtml
    fn set_inner_html(&self, ctx: &Ctx<'js>, value: rquickjs::String<'js>) -> Result<()> {
        self.set_inner_html(ctx, LegacyNullString(value.to_string()?))
    }

    // https://dom.spec.whatwg.org/#dom-documentorshadowroot-activeelement
    fn get_active_element(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        self.active_element(ctx)
    }
}

impl<'js> html_template_element_generated::HTMLTemplateElement<'js> for JsNode {
    // https://html.spec.whatwg.org/multipage/scripting.html#dom-template-contents
    fn get_content(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        let owner = world_for_node(ctx, self.handle.get().0)?;
        if let Some((document, fragment)) = owner
            .borrow()
            .document(self.handle.get().0)
            .and_then(|parsed| parsed.document.template_owner(self.handle.get().0.node))
        {
            let fragment = NodeId {
                document,
                node: fragment,
            };
            let fragment = owner
                .borrow()
                .registry()
                .borrow_mut()
                .live_node_id(fragment);
            return wrap_node(ctx, fragment);
        }
        let (snapshots, host_children) = {
            let borrowed = owner.borrow();
            let Some(mut parsed) = borrowed.document_mut(self.handle.get().0) else {
                return Err(Exception::throw_type(ctx, "no document"));
            };
            let fragment = parsed
                .document
                .base
                .mutate()
                .template_contents(self.handle.get().0.node);
            let children = parsed
                .document
                .base
                .get_node(fragment)
                .map_or_default(|node| node.children.iter().copied().collect::<Vec<_>>());
            let snapshots = crate::dom::adoption::snapshot_children(
                &parsed.document,
                self.handle.get().0.document,
                children.iter().copied(),
            );
            (snapshots, children)
        };
        let font = owner.borrow().runtime.font_ctx.clone();
        let mut inert = crate::Parsed::script("text/html", font);
        let fragment =
            crate::dom::adoption::materialize_children(&mut inert.document, 0, &snapshots)
                .map_err(|err| throw_dom_error(ctx, err))?;
        let root = owner.borrow_mut().add_document(inert);
        // Register the owner so a wrapper can resolve it later; a document
        // added without a registry entry reads back as a stale node
        // (<https://dom.spec.whatwg.org/#concept-node-adopt>).
        owner
            .borrow()
            .registry()
            .borrow_mut()
            .insert_document(root.document_id(), &owner);
        let fragment = NodeId {
            document: root.document,
            node: fragment.node,
        };
        {
            let borrowed = owner.borrow();
            let Some(mut parsed) = borrowed.document_mut(self.handle.get().0) else {
                return Err(Exception::throw_type(ctx, "no document"));
            };
            parsed.document.set_template_owner(
                self.handle.get().0.node,
                fragment.document,
                fragment.node,
            );
            let mut mutator = parsed.document.base.mutate();
            for child in host_children {
                mutator.remove_node(child);
            }
        }
        wrap_node(ctx, fragment)
    }
}

impl<'js> htmli_frame_element_generated::HTMLIFrameElement<'js> for JsNode {
    // https://html.spec.whatwg.org/multipage/iframe-embed-object.html#dom-iframe-contentdocument
    fn get_content_document(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        self.content_document(ctx.clone())
    }

    // https://html.spec.whatwg.org/multipage/iframe-embed-object.html#dom-iframe-sandbox
    fn get_sandbox(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        self.sandbox(ctx)
    }

    // https://html.spec.whatwg.org/multipage/iframe-embed-object.html#dom-iframe-contentwindow
    fn get_content_window(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        self.content_window(ctx.clone())
    }

    // `srcdoc` answers the content attribute
    // (<https://html.spec.whatwg.org/multipage/iframe-embed-object.html#dom-iframe-srcdoc>).
    fn get_srcdoc(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        host::reflect_string(ctx, self.handle.get().0, "srcdoc")
    }

    // https://html.spec.whatwg.org/multipage/iframe-embed-object.html#dom-iframe-srcdoc
    fn set_srcdoc(&self, ctx: &Ctx<'js>, value: rquickjs::String<'js>) -> Result<()> {
        set_attribute_sync(ctx, self.handle.get().0, "srcdoc", &value.to_string()?)
    }
}

impl<'js> html_frame_element_generated::HTMLFrameElement<'js> for JsNode {
    // https://html.spec.whatwg.org/multipage/obsolete.html#dom-frame-contentdocument
    fn get_content_document(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        self.content_document(ctx.clone())
    }

    // https://html.spec.whatwg.org/multipage/obsolete.html#dom-frame-contentwindow
    fn get_content_window(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        self.content_window(ctx.clone())
    }
}

impl JsNode {
    // https://dom.spec.whatwg.org/#dom-document-activeelement
    pub(super) fn active_element<'js>(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        let document = self.handle.get().0;
        let world = world(ctx)?;
        let active = world.borrow().active_element(document.document_id());
        if let Some(node) = active
            && world
                .borrow()
                .document(node)
                .is_some_and(|parsed| is_connected(&parsed.document.base, node.node))
        {
            return wrap_node(ctx, node);
        }
        let fallback = {
            let world = world.borrow();
            let Some(parsed) = world.document(document) else {
                return Ok(Value::new_null(ctx.clone()));
            };
            document_first(&parsed, "body").or_else(|| {
                document_first_child(&parsed, |data| matches!(data, NodeData::Element(_)))
            })
        };
        match fallback {
            Some(node) => wrap_node(ctx, node),
            None => Ok(Value::new_null(ctx.clone())),
        }
    }

    // https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-document-defaultview
    pub(super) fn default_view<'js>(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        // Return this document's browsing-context WindowProxy, not the
        // calling realm's `globalThis`
        // (<https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-document-defaultview>).
        let Ok(owner) = world_for_node(ctx, self.handle.get().0) else {
            return Ok(Value::new_null(ctx.clone()));
        };
        let owner = owner.borrow();
        if !owner.is_main_document(self.handle.get().0) {
            return Ok(Value::new_null(ctx.clone()));
        }
        match owner.window_object() {
            Some(window) => Ok(window.restore(ctx)?.into_value()),
            None => Ok(ctx.globals().into_value()),
        }
    }

    // https://html.spec.whatwg.org/multipage/interaction.html#dom-document-hasfocus
    pub(super) fn has_focus(&self, ctx: &Ctx<'_>) -> bool {
        is_main_document(ctx, self.handle.get().0)
    }

    // https://drafts.csswg.org/cssom-view/#dom-document-elementfrompoint
    pub(super) fn element_from_point<'js>(
        &self,
        ctx: Ctx<'js>,
        x: f64,
        y: f64,
    ) -> Result<Value<'js>> {
        let Some(node) = element_at_point_ctx(&ctx, self.handle.get().0, x, y)? else {
            return Ok(Value::new_null(ctx));
        };
        wrap_node(&ctx, node)
    }

    // The no-layout hit test: the deepest element whose virtual box contains
    // the point (see `element_at_point`).
    pub(super) fn elements_from_point<'js>(
        &self,
        ctx: Ctx<'js>,
        x: f64,
        y: f64,
    ) -> Result<Vec<Value<'js>>> {
        let mut elements = Vec::new();
        let Some(node) = element_at_point_ctx(&ctx, self.handle.get().0, x, y)? else {
            return Ok(elements);
        };
        // The hit-test stack: the element and its ancestors, topmost first.
        let world = world_for_node(&ctx, node)?;
        let mut cursor = Some(node);
        while let Some(current) = cursor {
            elements.push(wrap_node(&ctx, current)?);
            cursor = world.borrow().node_parent(current);
        }
        Ok(elements)
    }

    // https://dom.spec.whatwg.org/#dom-document-createelement
    pub(super) fn create_element<'js>(
        &self,
        ctx: Ctx<'js>,
        tag: WebIdlString,
    ) -> Result<Value<'js>> {
        create_html_element(&ctx, self.handle.get().0, &tag.0)
    }

    // https://dom.spec.whatwg.org/#dom-document-createelementns
    // https://dom.spec.whatwg.org/#internal-createelementns-steps
    pub(super) fn create_element_ns<'js>(
        &self,
        ctx: Ctx<'js>,
        ns: OptString,
        tag: WebIdlString,
    ) -> Result<Value<'js>> {
        let name = validate_and_extract(&ctx, ns.0.as_deref(), &tag.0, NodeContext::Element)?;
        create_element_named(&ctx, self.handle.get().0, name)
    }

    pub(super) fn create_text_node<'js>(
        &self,
        ctx: Ctx<'js>,
        data: WebIdlCodeUnits,
    ) -> Result<Value<'js>> {
        create_node(&ctx, self.handle.get().0, |parsed| {
            parsed.document.create_text(&data.0)
        })
    }

    // https://dom.spec.whatwg.org/#dom-document-createcomment
    pub(super) fn create_comment<'js>(
        &self,
        ctx: Ctx<'js>,
        data: WebIdlCodeUnits,
    ) -> Result<Value<'js>> {
        create_node(&ctx, self.handle.get().0, |parsed| {
            parsed.document.create_comment(&data.0)
        })
    }

    // https://dom.spec.whatwg.org/#dom-document-createprocessinginstruction
    pub(super) fn create_processing_instruction<'js>(
        &self,
        ctx: Ctx<'js>,
        target: WebIdlString,
        data: WebIdlCodeUnits,
    ) -> Result<Value<'js>> {
        if !xml5ever::names::is_name(&target.0) {
            return Err(throw_dom(
                &ctx,
                "InvalidCharacterError",
                "target does not match the XML Name production",
            ));
        }
        if data.0.to_string_lossy().contains("?>") {
            return Err(throw_dom(&ctx, "InvalidCharacterError", "data contains ?>"));
        }
        // https://dom.spec.whatwg.org/#processinginstruction-initialize
        // The current algorithm checks the Name production and `?>`. It does
        // not reject an `xml` target.
        let text = data.0.to_string_lossy().into_owned();
        let target = target.0;
        create_node(&ctx, self.handle.get().0, |parsed| {
            parsed
                .document
                .create_processing_instruction(&target, &text)
        })
    }

    // https://dom.spec.whatwg.org/#dom-document-createcdatasection
    pub(super) fn create_cdata_section<'js>(
        &self,
        ctx: Ctx<'js>,
        data: WebIdlCodeUnits,
    ) -> Result<Value<'js>> {
        // An HTML document is one whose content type is `text/html`. An
        // XHTML document is an XML document and can have CDATA sections
        // (<https://dom.spec.whatwg.org/#html-document>,
        // <https://dom.spec.whatwg.org/#dom-document-createcdatasection>).
        if document_is_html_content(&ctx, self.handle.get().0) {
            return Err(throw_dom(
                &ctx,
                "NotSupportedError",
                "CDATA sections are not supported in HTML documents",
            ));
        }
        if data.0.to_string_lossy().contains("]]>") {
            return Err(throw_dom(
                &ctx,
                "InvalidCharacterError",
                "data contains ]]>",
            ));
        }
        let text = data.0.to_string_lossy().into_owned();
        create_node(&ctx, self.handle.get().0, |parsed| {
            parsed.document.create_cdata_section(&text)
        })
    }

    // https://dom.spec.whatwg.org/#dom-document-createattribute
    pub(super) fn create_attribute<'js>(
        &self,
        ctx: Ctx<'js>,
        name: WebIdlString,
    ) -> Result<Value<'js>> {
        // An HTML document ASCII-lowercases the name before the Name check
        // (<https://dom.spec.whatwg.org/#dom-document-createattribute>).
        let name = if document_is_html_content(&ctx, self.handle.get().0) {
            name.0.to_ascii_lowercase()
        } else {
            name.0
        };
        if !valid_attribute_local_name(&name) {
            return Err(throw_dom(
                &ctx,
                "InvalidCharacterError",
                "attribute name is not a valid attribute local name",
            ));
        }
        let id = new_detached_attr(
            &ctx,
            self.handle.get().0,
            String::new(),
            None,
            name.clone(),
            name,
        )?;
        attr_wrapper(&ctx, id)
    }

    // https://dom.spec.whatwg.org/#dom-document-createattributens
    pub(super) fn create_attribute_ns<'js>(
        &self,
        ctx: Ctx<'js>,
        namespace: OptString,
        qualified: WebIdlString,
    ) -> Result<Value<'js>> {
        let name = validate_and_extract(
            &ctx,
            namespace.0.as_deref(),
            &qualified.0,
            NodeContext::Attribute,
        )?;
        let prefix = name
            .prefix
            .as_ref()
            .filter(|prefix| !prefix.is_empty())
            .map(ToString::to_string);
        let id = new_detached_attr(
            &ctx,
            self.handle.get().0,
            name.ns.to_string(),
            prefix,
            name.local.to_string(),
            qualified_name(&name),
        )?;
        attr_wrapper(&ctx, id)
    }

    // https://dom.spec.whatwg.org/#dom-document-createdocumentfragment
    pub(super) fn create_document_fragment<'js>(&self, ctx: Ctx<'js>) -> Result<Value<'js>> {
        create_node(&ctx, self.handle.get().0, |parsed| {
            parsed.document.create_fragment()
        })
    }

    // https://dom.spec.whatwg.org/#dom-document-importnode
    pub(super) fn import_node<'js>(
        &self,
        ctx: Ctx<'js>,
        node: Value<'js>,
        deep: bool,
    ) -> Result<Value<'js>> {
        let source_id = required_node(&ctx, &node)?;
        let world_rc = world_for_node(&ctx, self.handle.get().0)?;
        let is_document = {
            let world = world_rc.borrow();
            let Some(source) = world.document(source_id) else {
                return Err(Exception::throw_type(&ctx, "stale node"));
            };
            source.document.base.root_node().id == source_id.node
        };
        if is_document {
            return Err(throw_dom(
                &ctx,
                "NotSupportedError",
                "cannot import a document",
            ));
        }
        if node_is_shadow_root(&ctx, source_id)? {
            // https://dom.spec.whatwg.org/#dom-document-importnode
            return Err(throw_dom(
                &ctx,
                "NotSupportedError",
                "cannot import a shadow root",
            ));
        }
        let tree = world_rc
            .borrow()
            .store()
            .get(source_id.document)
            .and_then(|parsed| {
                crate::dom::adoption::import_snapshot(&parsed.document, source_id, deep)
            })
            .ok_or_else(|| Exception::throw_type(&ctx, "stale node"))?;
        let world = world_rc.borrow();
        let Some(mut target) = world.document_mut(self.handle.get().0) else {
            return Err(Exception::throw_type(&ctx, "no document"));
        };
        let id = materialize_import(&mut target.document, self.handle.get().0.document, &tree)
            .map_err(|err| throw_dom_error(&ctx, err))?;
        drop(target);
        drop(world);
        wrap_node(&ctx, id)
    }

    // https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-document-open
    pub(super) fn open_document<'js>(&self, ctx: Ctx<'js>) -> Result<Value<'js>> {
        world_for_node(&ctx, self.handle.get().0)?
            .borrow_mut()
            .queue_document_stream(DocumentStreamCommand::Open);
        wrap_node(&ctx, self.handle.get().0)
    }

    // https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#document-write-steps
    pub(super) fn write(&self, ctx: Ctx<'_>, text: Vec<WebIdlString>) -> Result<()> {
        // Every argument stringifies and concatenates in order
        // (<https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-document-write>).
        let html: String = text.iter().map(|part| part.0.as_str()).collect();
        world_for_node(&ctx, self.handle.get().0)?
            .borrow_mut()
            .queue_write(html)
            .map_err(|()| Exception::throw_range(&ctx, "document stream budget exceeded"))
    }

    // https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-document-close
    pub(super) fn close_document(&self, ctx: Ctx<'_>) -> Result<()> {
        world_for_node(&ctx, self.handle.get().0)?
            .borrow_mut()
            .queue_document_stream(DocumentStreamCommand::Close);
        Ok(())
    }

    pub(super) fn get_element_by_id<'js>(
        &self,
        ctx: Ctx<'js>,
        id: WebIdlString,
    ) -> Result<Value<'js>> {
        element_by_id(&ctx, self.handle.get().0, &id.0)
    }

    // https://dom.spec.whatwg.org/#dom-document-implementation
    pub(super) fn implementation<'js>(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        let world_rc = world(ctx)?;
        if let Some(saved) = world_rc.borrow().implementation(self.handle.get().0)
            && let Some(value) = deref_weak(ctx, saved)?
        {
            return Ok(value);
        }
        let value = host::instance(
            ctx,
            JsImplementation {
                document: self.handle.get(),
            },
        )?
        .into_value();
        let weak = make_weak(ctx, value.clone())?;
        world_rc
            .borrow_mut()
            .intern_implementation(self.handle.get().0, Persistent::save(ctx, weak));
        Ok(value)
    }

    pub(super) fn get_elements_by_tag_name<'js>(
        &self,
        ctx: Ctx<'js>,
        name: WebIdlString,
    ) -> Result<Value<'js>> {
        elements_by_tag(&ctx, self.handle.get().0, &name.0)
    }

    pub(super) fn body<'js>(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        document_value(ctx, self.handle.get().0, |parsed| {
            document_first(parsed, "body")
        })
    }

    // https://html.spec.whatwg.org/multipage/dom.html#dom-document-body
    pub(super) fn set_body<'js>(&self, ctx: &Ctx<'js>, value: Value<'js>) -> Result<()> {
        // A string or other non-node throws, like any interface conversion.
        let Some(id) = host_node_id(ctx, &value) else {
            return Err(Exception::throw_type(ctx, "body must be an element"));
        };
        let is_body = with_node_data(ctx, id, |data| match data {
            Some(NodeData::Element(element)) => {
                element.name.ns == html_namespace()
                    && (element.name.local.as_str() == "body"
                        || element.name.local.as_str() == "frameset")
            }
            _ => false,
        })?;
        if !is_body {
            return Err(throw_dom(
                ctx,
                "HierarchyRequestError",
                "body must be a body or frameset element",
            ));
        }
        let world_rc = world(ctx)?;
        let root_element = {
            let world = world_rc.borrow();
            let Some(parsed) = world.document(self.handle.get().0) else {
                return Err(Exception::throw_type(ctx, "stale node"));
            };
            document_first_child(&parsed, |data| matches!(data, NodeData::Element(_)))
        };
        let Some(root_element) = root_element else {
            return Err(throw_dom(
                ctx,
                "HierarchyRequestError",
                "document has no document element",
            ));
        };
        let current = current_body_child(ctx, &world_rc, root_element)?;
        if current == Some(id) {
            return Ok(());
        }
        let node = adopt_node(ctx, self.handle.get().0, id)?;
        {
            let world = world_rc.borrow();
            let Some(parsed) = world.document(self.handle.get().0) else {
                return Err(Exception::throw_type(ctx, "no document"));
            };
            // The new body cannot live inside the subtree it joins.
            if node.document == self.handle.get().0.document {
                let base = &parsed.document.base;
                let mut cursor = Some(root_element.node);
                while let Some(current) = cursor {
                    if current == node.node {
                        return Err(throw_dom(
                            ctx,
                            "HierarchyRequestError",
                            "node cannot contain itself",
                        ));
                    }
                    cursor = base
                        .get_node(current)
                        .and_then(|candidate| candidate.parent);
                }
            }
        }
        {
            let world = world_rc.borrow();
            let Some(mut parsed) = world.document_mut(self.handle.get().0) else {
                return Err(Exception::throw_type(ctx, "no document"));
            };
            if let Some(current) = current {
                drop(parsed);
                drop(world);
                let replacement = super::core::replace_target(ctx, root_element, node)?;
                let world = world_rc.borrow();
                let Some(mut parsed) = world.document_mut(self.handle.get().0) else {
                    return Err(Exception::throw_type(ctx, "no document"));
                };
                replace_parsed(&mut parsed, root_element, current, replacement);
                drop(parsed);
                drop(world);
                fixup_focus_after_removal(ctx, current)?;
            } else {
                insert_parsed(&mut parsed, root_element, node, None);
                drop(parsed);
                drop(world);
            }
        }
        schedule_mutation_delivery(ctx)
    }

    // https://html.spec.whatwg.org/multipage/dom.html#dom-document-head
    pub(super) fn head<'js>(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        document_value(ctx, self.handle.get().0, |parsed| {
            document_first(parsed, "head")
        })
    }

    // https://html.spec.whatwg.org/multipage/dom.html#dom-document-currentscript
    pub(super) fn current_script<'js>(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        let world = world(ctx)?;
        let current = world.borrow().state.current_script;
        match current {
            Some(id) if id.document_id() == self.handle.get().0.document_id() => wrap_node(ctx, id),
            _ => Ok(Value::new_null(ctx.clone())),
        }
    }

    pub(super) fn document_element<'js>(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        document_value(ctx, self.handle.get().0, |parsed| {
            document_first_child(parsed, |data| matches!(data, NodeData::Element(_)))
        })
    }

    // https://dom.spec.whatwg.org/#dom-document-doctype
    pub(super) fn doctype<'js>(&self, ctx: &Ctx<'js>) -> Value<'js> {
        let found =
            world(ctx).ok().and_then(|world| {
                world
                    .borrow()
                    .document(self.handle.get().0)
                    .and_then(|parsed| {
                        let root = parsed.document.base.root_node().id;
                        let children: Vec<BlitzId> = parsed
                            .document
                            .base
                            .get_node(root)
                            .map_or_default(|root| root.children.iter().copied().collect());
                        children
                            .into_iter()
                            .find(|child| {
                                parsed.document.base.get_node(*child).is_some_and(|node| {
                                    matches!(node.data, NodeData::Doctype { .. })
                                })
                            })
                            .map(|node| NodeId {
                                document: parsed.id,
                                node,
                            })
                    })
            });
        child_value(ctx, found).unwrap_or_else(|_| Value::new_null(ctx.clone()))
    }

    // https://dom.spec.whatwg.org/#dom-document-readyState
    pub(super) fn ready_state(&self, ctx: &Ctx<'_>) -> Result<String> {
        let world = world(ctx)?;
        let world = world.borrow();
        let Some(parsed) = world.document(self.handle.get().0) else {
            return Ok(String::new());
        };
        if parsed.document.base.root_node().id != self.handle.get().0.node {
            return Ok(String::new());
        }
        Ok(match parsed.ready_state {
            ReadyState::Loading => "loading".into(),
            ReadyState::Interactive => "interactive".into(),
            ReadyState::Complete => "complete".into(),
        })
    }

    // https://dom.spec.whatwg.org/#dom-document-url
    pub(super) fn url(&self, ctx: &Ctx<'_>) -> crate::dom_string::DomString {
        document_url_string(ctx, self.handle.get().0).into()
    }

    // https://html.spec.whatwg.org/multipage/urls-and-fetching.html#dom-document-baseuri
    pub(super) fn base_uri(&self, ctx: &Ctx<'_>) -> crate::dom_string::DomString {
        document_base_url_string(ctx, self.handle.get().0).into()
    }

    // https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-document-location
    pub(super) fn location<'js>(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        if is_main_document(ctx, self.handle.get().0) {
            return ctx.globals().get("location");
        }
        Ok(Value::new_null(ctx.clone()))
    }

    /// <https://encoding.spec.whatwg.org/#dom-document-characterset>
    pub(super) fn character_set(&self, ctx: &Ctx<'_>) -> &'static str {
        document_character_set(ctx, self.handle.get().0)
    }

    // https://dom.spec.whatwg.org/#dom-document-contenttype
    pub(super) fn content_type(&self, ctx: &Ctx<'_>) -> Result<&'static str> {
        let world_rc = world(ctx)?;
        let parsed = world_rc.borrow();
        Ok(parsed
            .document(self.handle.get().0)
            .map_or("text/html", |parsed| parsed.content_type))
    }

    // https://dom.spec.whatwg.org/#dom-document-compatmode
    pub(super) fn compat_mode(&self, ctx: &Ctx<'_>) -> Result<String> {
        let world = world(ctx)?;
        let parsed = world.borrow();
        let quirks = parsed
            .document(self.handle.get().0)
            .is_some_and(|parsed| parsed.quirks_mode == markup5ever::interface::QuirksMode::Quirks);
        Ok(if quirks { "BackCompat" } else { "CSS1Compat" }.into())
    }

    // https://html.spec.whatwg.org/multipage/dom.html#dom-document-getelementsbyname
    pub(super) fn get_elements_by_name<'js>(
        &self,
        ctx: Ctx<'js>,
        name: WebIdlString,
    ) -> Result<Value<'js>> {
        live_collection(
            &ctx,
            self.handle.get().0,
            CollectionKind::ElementsByName(name.0),
            None,
        )
    }

    // https://dom.spec.whatwg.org/#dom-element-attachshadow
    pub(super) fn attach_shadow<'js>(
        &self,
        ctx: Ctx<'js>,
        init: super::element::element_generated::ShadowRootInit,
    ) -> Result<Value<'js>> {
        let closed = matches!(
            init.mode,
            super::element::element_generated::ShadowRootMode::Closed
        );
        let id = self.handle.get().0;
        let world_rc = world(&ctx)?;
        let shadow = {
            let world = world_rc.borrow();
            let Some(mut parsed) = world.document_mut(id) else {
                return Err(Exception::throw_type(&ctx, "stale node"));
            };
            let eligible = parsed
                .document
                .base
                .get_node(id.node)
                .and_then(|node| node.element_data())
                .is_some_and(|element| {
                    element.name.ns == html_namespace()
                        && valid_shadow_host(element.name.local.as_ref())
                        && element.shadow_root.is_none()
                });
            if !eligible {
                // A non-HTML element, a name that cannot host a shadow, or an
                // element that already hosts one
                // (<https://dom.spec.whatwg.org/#dom-element-attachshadow>).
                return Err(throw_dom(
                    &ctx,
                    "NotSupportedError",
                    "element cannot host a shadow root",
                ));
            }
            parsed
                .document
                .base
                .mutate()
                .attach_shadow_root(id.node, closed)
        };
        wrap_node(
            &ctx,
            NodeId {
                document: id.document,
                node: shadow,
            },
        )
    }

    // https://dom.spec.whatwg.org/#dom-element-shadowroot
    pub(super) fn shadow_root<'js>(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        let id = self.handle.get().0;
        let found = world(ctx).ok().and_then(|world| {
            let world = world.borrow();
            let parsed = world.document(id)?;
            let node = parsed.document.base.get_node(id.node)?;
            let element = node.element_data()?;
            let shadow = element.shadow_root?;
            let closed = matches!(
                parsed.document.base.get_node(shadow).map(|node| &node.data),
                Some(NodeData::ShadowRoot { closed: true, .. })
            );
            if closed { None } else { Some(shadow) }
        });
        match found {
            Some(shadow) => wrap_node(
                ctx,
                NodeId {
                    document: id.document,
                    node: shadow,
                },
            ),
            None => Ok(Value::new_null(ctx.clone())),
        }
    }

    // https://dom.spec.whatwg.org/#dom-shadowroot-host
    pub(super) fn host<'js>(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        let id = self.handle.get().0;
        let host = with_node_data(ctx, id, |data| match data {
            Some(NodeData::ShadowRoot { host, .. }) => Some(*host),
            _ => None,
        })?;
        let Some(host) = host else {
            return Err(Exception::throw_type(ctx, "not a shadow root"));
        };
        wrap_node(
            ctx,
            NodeId {
                document: id.document,
                node: host,
            },
        )
    }

    // https://dom.spec.whatwg.org/#dom-shadowroot-mode
    pub(super) fn mode(&self, ctx: &Ctx<'_>) -> Result<String> {
        let id = self.handle.get().0;
        let mode = with_node_data(ctx, id, |data| match data {
            Some(NodeData::ShadowRoot { closed, .. }) => {
                Some(if *closed { "closed" } else { "open" })
            }
            _ => None,
        })?;
        mode.map(str::to_owned)
            .ok_or_else(|| Exception::throw_type(ctx, "not a shadow root"))
    }

    // https://html.spec.whatwg.org/multipage/iframe-embed-object.html#dom-iframe-contentdocument
    pub(super) fn content_document<'js>(&self, ctx: Ctx<'js>) -> Result<Value<'js>> {
        if !iframe_frame(&ctx, self.handle.get().0)? {
            return Ok(Value::new_null(ctx));
        }
        let world = world(&ctx)?;
        let root = world.borrow().frame_document(self.handle.get().0);
        match root {
            Some(root) => {
                // Captured origins: `Url::origin()` is a new opaque value for
                // `about:blank`, `data:`, and `blob:`
                // (<https://html.spec.whatwg.org/multipage/browsers.html#determining-the-origin>).
                let parent_origin = world.borrow().origin.clone();
                let child_origin = world
                    .borrow()
                    .owner_world(root)
                    .map(|child| child.borrow().origin.clone());
                if child_origin.is_some_and(|origin| origin == parent_origin) {
                    wrap_node(&ctx, root)
                } else {
                    Ok(Value::new_null(ctx))
                }
            }
            None => Ok(Value::new_null(ctx)),
        }
    }

    // https://html.spec.whatwg.org/multipage/iframe-embed-object.html#dom-iframe-contentwindow
    pub(super) fn content_window<'js>(&self, ctx: Ctx<'js>) -> Result<Value<'js>> {
        if !iframe_frame(&ctx, self.handle.get().0)? {
            return Ok(Value::new_null(ctx));
        }
        let frame = world(&ctx)?
            .borrow()
            .frame_for_container(self.handle.get().0);
        match frame {
            Some(frame) => {
                let proxy: Function = crate::js::bridge::object(&ctx)?.get("__tbFrameProxy")?;
                proxy.call((crate::js::js_number(frame.get()),))
            }
            None => Ok(Value::new_null(ctx)),
        }
    }

    /// `HTMLIFrameElement.sandbox`: the `DOMTokenList` over the `sandbox`
    /// content attribute. `[SameObject]` keeps one list per element, so
    /// `iframe.sandbox === iframe.sandbox` and a mutation through one handle
    /// shows through the other
    /// (<https://html.spec.whatwg.org/multipage/iframe-embed-object.html#dom-iframe-sandbox>).
    pub(super) fn sandbox<'js>(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        let world_rc = world(ctx)?;
        if let Some(saved) = world_rc
            .borrow()
            .same_object(self.handle.get().0, SameObject::Sandbox)
            && let Some(value) = deref_weak(ctx, saved)?
        {
            return Ok(value);
        }
        let list = host::instance_for_node(
            ctx,
            self.handle.get().0,
            crate::js::bindings::JsTokenList {
                element: self.handle.get(),
                attribute: markup5ever::local_name!("sandbox"),
            },
        )?;
        let value = rquickjs::Class::into_value(list);
        let weak = make_weak(ctx, value.clone())?;
        world_rc.borrow_mut().intern_same_object(
            self.handle.get().0,
            SameObject::Sandbox,
            Persistent::save(ctx, weak),
        );
        Ok(value)
    }
}

pub(in crate::js::bindings) fn construct_node<'js>(
    ctx: Ctx<'js>,
    name: WebIdlString,
    args: Rest<Value<'js>>,
) -> Result<Value<'js>> {
    let owner = world(&ctx)?;
    construct_node_in_owner(&ctx, &owner, &name, args)
}

pub(in crate::js::bindings) fn construct_node_in_frame<'js>(
    ctx: Ctx<'js>,
    frame: f64,
    name: WebIdlString,
    args: Rest<Value<'js>>,
) -> Result<Value<'js>> {
    let Some(frame) = crate::js::bridge::frame_id(frame) else {
        return Err(Exception::throw_type(&ctx, "no document"));
    };
    let current = world(&ctx)?;
    let Some(owner) = current.borrow().frame_world(frame) else {
        return Err(Exception::throw_type(&ctx, "no document"));
    };
    construct_node_in_owner(&ctx, &owner, &name, args)
}

/// The shared construction backend: allocates in `owner`'s document and
/// wraps for the calling realm.
pub(super) fn construct_node_in_owner<'js>(
    ctx: &Ctx<'js>,
    owner: &Rc<RefCell<World>>,
    name: &WebIdlString,
    args: Rest<Value<'js>>,
) -> Result<Value<'js>> {
    let mut arguments = args.0.into_iter();
    let first = arguments.next();
    let document = || {
        owner
            .borrow()
            .main_document_root()
            .ok_or_else(|| Exception::throw_type(ctx, "no document"))
    };
    match name.0.as_str() {
        "Text" => {
            let data = constructor_units(ctx, first)?;
            create_node(ctx, document()?, |parsed| {
                parsed.document.create_text(&data)
            })
        }
        "Comment" => {
            let data = constructor_units(ctx, first)?;
            create_node(ctx, document()?, |parsed| {
                parsed.document.create_comment(&data)
            })
        }
        "DocumentFragment" => {
            create_node(ctx, document()?, |parsed| parsed.document.create_fragment())
        }
        "Range" => crate::js::bindings::range::new_range(ctx, document()?),
        "Document" => {
            // The `Document` constructor creates an XML document that still
            // implements `Document`, not `XMLDocument`
            // (<https://dom.spec.whatwg.org/#dom-document-document>).
            let font_ctx = owner.borrow().runtime.font_ctx.clone();
            let mut parsed = crate::Parsed::script("application/xml", font_ctx);
            parsed.xml_document = false;
            wrap_new_document_in_world(ctx, parsed, owner)
        }
        "XMLDocument" => {
            // <https://dom.spec.whatwg.org/#dom-xmldocument-xmldocument>
            let font_ctx = owner.borrow().runtime.font_ctx.clone();
            wrap_new_document_in_world(
                ctx,
                crate::Parsed::script("application/xml", font_ctx),
                owner,
            )
        }
        "HTMLElement" => {
            // https://html.spec.whatwg.org/multipage/custom-elements.html#html-element-constructors
            // During upgrade, the construction stack supplies the existing
            // element rather than allocating a second wrapper.
            let id = world(ctx)?
                .borrow_mut()
                .state
                .custom_construction
                .pop()
                .ok_or_else(|| Exception::throw_type(ctx, "Illegal constructor"))?;
            wrap_node(ctx, id)
        }
        other => Err(Exception::throw_type(
            ctx,
            &format!("{other} is not a constructor"),
        )),
    }
}

pub(in crate::js::bindings) fn install_custom_construction(ctx: &Ctx<'_>) -> Result<()> {
    let globals = crate::js::bridge::object(ctx)?;
    globals.set(
        "__tbPushCustomConstruction",
        rquickjs::prelude::Func::from(push_custom_construction),
    )?;
    globals.set(
        "__tbDiscardCustomConstruction",
        rquickjs::prelude::Func::from(discard_custom_construction),
    )?;
    Ok(())
}

pub(super) fn push_custom_construction<'js>(ctx: Ctx<'js>, value: Value<'js>) -> Result<()> {
    let id = host_node_id(&ctx, &value)
        .ok_or_else(|| Exception::throw_type(&ctx, "custom element candidate is not an element"))?;
    let is_element = with_node_data(&ctx, id, |data| matches!(data, Some(NodeData::Element(_))))?;
    if !is_element {
        return Err(Exception::throw_type(
            &ctx,
            "custom element candidate is not an element",
        ));
    }
    world(&ctx)?.borrow_mut().state.custom_construction.push(id);
    Ok(())
}

pub(super) fn discard_custom_construction<'js>(ctx: Ctx<'js>, value: Value<'js>) -> Result<()> {
    let Some(id) = host_node_id(&ctx, &value) else {
        return Ok(());
    };
    let world = world(&ctx)?;
    let mut world = world.borrow_mut();
    if world.state.custom_construction.last() == Some(&id) {
        world.state.custom_construction.pop();
    }
    Ok(())
}

/// Constructor `DOMString` that keeps every UTF-16 code unit, for the
/// character-data constructors (`new Text(…)`).
pub(super) fn constructor_units<'js>(
    ctx: &Ctx<'js>,
    value: Option<Value<'js>>,
) -> Result<crate::dom_string::DomString> {
    match value {
        None => Ok(crate::dom_string::DomString::default()),
        Some(value) if value.is_undefined() => Ok(crate::dom_string::DomString::default()),
        Some(value) => Ok(crate::dom_string::DomString::from_utf16(
            super::super::webidl_to_units(ctx, value)?,
        )),
    }
}

/// Creates a detached node in `document`'s tree and wraps it. Detached
/// creation records no mutation: no observer can watch a node with no parent.
pub(in crate::js::bindings) fn create_node<'js>(
    ctx: &Ctx<'js>,
    document: NodeId,
    make: impl FnOnce(&mut crate::Parsed) -> BlitzId,
) -> Result<Value<'js>> {
    let world = world(ctx)?;
    let world = world.borrow();
    let Some(mut parsed) = world.document_mut(document) else {
        return Err(Exception::throw_type(ctx, "no document"));
    };
    let store = parsed.id;
    let blitz = make(&mut parsed);
    let id = NodeId {
        document: store,
        node: blitz,
    };
    drop(parsed);
    drop(world);
    wrap_node(ctx, id)
}

pub(super) fn create_html_element<'js>(
    ctx: &Ctx<'js>,
    document: NodeId,
    tag: &str,
) -> Result<Value<'js>> {
    // https://dom.spec.whatwg.org/#dom-document-createelement: validate, then
    // lowercase for an HTML document.
    if !valid_element_local_name(tag) {
        return Err(throw_dom(
            ctx,
            "InvalidCharacterError",
            "tag name is not a valid element local name",
        ));
    }
    // XHTML documents use the HTML namespace but keep case; only text/html
    // is an "HTML document" for lowercasing
    // (<https://dom.spec.whatwg.org/#internal-createelementns-steps>).
    let namespace = if document_is_html(ctx, document) {
        html_namespace()
    } else {
        Namespace::from("")
    };
    let local = if document_is_html_content(ctx, document) {
        tag.to_ascii_lowercase()
    } else {
        tag.to_owned()
    };
    let name = QualName::new(None, namespace, LocalName::from(local));
    create_element_named(ctx, document, name)
}

pub(super) fn create_element_named<'js>(
    ctx: &Ctx<'js>,
    document: NodeId,
    name: QualName,
) -> Result<Value<'js>> {
    let value = create_node(ctx, document, |parsed| {
        parsed
            .document
            .base
            .mutate()
            .create_element(name, Vec::new())
    })?;
    Ok(value)
}

/// The context string the HTML fragment parser expects for an element with
/// this qualified name: html5lib's `svg `/`math ` prefixes for foreign
/// namespaces
/// (<https://html.spec.whatwg.org/multipage/parsing.html#html-fragment-parsing-algorithm>).
pub(in crate::js::bindings) fn html_fragment_context(name: &QualName) -> String {
    if name.ns == svg_namespace() {
        format!("svg {}", name.local)
    } else if name.ns.as_str() == "http://www.w3.org/1998/Math/MathML" {
        format!("math {}", name.local)
    } else if name.ns == html_namespace() {
        name.local.to_string()
    } else {
        // Preserve foreign-namespace contexts through the `ns:` roundtrip so
        // fragment parsing sees the real context element name instead of an
        // HTML impostor
        // (<https://html.spec.whatwg.org/multipage/parsing.html#html-fragment-parsing-algorithm>).
        format!("ns:{}|{}", name.ns, name.local)
    }
}

/// Places fragment children for the non-`beforebegin` `insertAdjacentHTML`
/// positions: `afterbegin` prepends, `afterend` appends after the target,
/// `beforeend` appends inside.
pub(super) fn place_adjacent_rest(
    ctx: &Ctx<'_>,
    parsed: &mut crate::Parsed,
    position: &str,
    target: NodeId,
    moved: Vec<NodeId>,
) -> Result<()> {
    match position {
        "afterbegin" => {
            let reference = parsed
                .document
                .base
                .get_node(target.node)
                .and_then(|node| node.children.first().copied())
                .map(|node| NodeId {
                    document: target.document,
                    node,
                });
            for child in moved {
                place_journaled(parsed, target, child, reference);
            }
        }
        "afterend" => {
            let parent = parsed
                .document
                .base
                .get_node(target.node)
                .and_then(|node| node.parent)
                .map(|node| NodeId {
                    document: target.document,
                    node,
                })
                .ok_or_else(|| {
                    throw_dom(ctx, "NoModificationAllowedError", "element has no parent")
                })?;
            let mut reference =
                sibling(&parsed.document.base, target.node, true).map(|next| NodeId {
                    document: target.document,
                    node: next,
                });
            for child in moved {
                place_journaled(parsed, parent, child, reference);
                reference = Some(child);
            }
        }
        _ => {
            for child in moved {
                place_journaled(parsed, target, child, None);
            }
        }
    }
    Ok(())
}

/// The base URL and shared font context a fragment scratch document parses
/// under: the live document's base, so eager subresource loads resolve
/// instead of panicking in Blitz. Falls back closed when the document is gone.
pub(in crate::js::bindings) fn fragment_base_url(
    ctx: &Ctx<'_>,
    id: NodeId,
) -> (String, parley::FontContext) {
    world(ctx)
        .ok()
        .and_then(|world| {
            let world = world.borrow();
            world.document(id).map(|parsed| {
                (
                    parsed.document.base.base_url().as_str().to_owned(),
                    world.runtime.font_ctx.clone(),
                )
            })
        })
        .unwrap_or_else(|| {
            (
                crate::render::INVALID_BASE_URL.to_owned(),
                parley::FontContext::default(),
            )
        })
}

/// Parses `markup` as an HTML fragment in `context` and snapshots the
/// resulting nodes for insertion into a document.
pub(in crate::js::bindings) fn parse_html_fragment_snapshots(
    markup: &str,
    context: &str,
    base_url: &str,
    font_ctx: parley::FontContext,
) -> Vec<ImportSnapshot> {
    // The fragment parses into a scratch context element; only the parsed
    // children are snapshotted for insertion into a live document
    // (<https://html.spec.whatwg.org/multipage/parsing.html#html-fragment-parsing-algorithm>).
    // The scratch document carries the live base URL so eager subresource
    // loads resolve instead of panicking in Blitz. It never renders, so it
    // shares fonts and skips UA sheets.
    let context_name = fragment_context_name(context);
    let mut base = blitz_dom::BaseDocument::new(blitz_dom::DocumentConfig {
        base_url: Some(base_url.to_owned()),
        font_ctx: Some(font_ctx),
        ua_stylesheets: Vec::new(),
        ..blitz_dom::DocumentConfig::default()
    });
    let context_id = {
        let mut mutator = base.mutate();
        let context_id = mutator.create_element(context_name, Vec::new());
        blitz_html::DocumentHtmlParser::parse_inner_html_into_mutator(
            &mut mutator,
            context_id,
            markup,
        );
        context_id
    };
    let scratch = crate::documents::BlitzDocument::from_base(base);
    // Fragment parsing into a template context fills the context's
    // template contents, not its children: snapshot from the contents.
    let children: Vec<BlitzId> =
        scratch
            .base
            .get_node(context_id)
            .map_or_default(|node| match &node.data {
                blitz_dom::NodeData::Element(element)
                    if crate::dom::tree::is_template_tag(&element.name) =>
                {
                    element
                        .template_contents
                        .and_then(|fragment| {
                            scratch
                                .base
                                .get_node(fragment)
                                .map(|node| node.children.iter().copied().collect())
                        })
                        .unwrap_or_default()
                }
                _ => node.children.iter().copied().collect(),
            });
    children
        .into_iter()
        .filter_map(|child| {
            import_snapshot(
                &scratch,
                NodeId {
                    document: 0,
                    node: child,
                },
                true,
            )
        })
        .collect()
}

/// The context element name for fragment parsing: html5lib's `svg `/`math `
/// prefixes select the foreign namespace. An `ns:<namespace>|<local>` spec
/// selects an arbitrary namespace, so foreign-namespace contexts keep their
/// identity instead of collapsing to HTML.
pub(in crate::js::bindings) fn fragment_context_name(spec: &str) -> QualName {
    const SVG: &str = "http://www.w3.org/2000/svg";
    const MATHML: &str = "http://www.w3.org/1998/Math/MathML";
    if let Some(rest) = spec.strip_prefix("ns:")
        && let Some((namespace, local)) = rest.split_once('|')
    {
        return QualName::new(None, Namespace::from(namespace), LocalName::from(local));
    }
    if let Some(local) = spec.strip_prefix("svg ") {
        QualName::new(None, Namespace::from(SVG), LocalName::from(local))
    } else if let Some(local) = spec.strip_prefix("math ") {
        QualName::new(None, Namespace::from(MATHML), LocalName::from(local))
    } else {
        QualName::new(None, html_namespace(), LocalName::from(spec))
    }
}

/// The current `body`/`frameset` child of the document element owning
/// `root`, if any.
pub(super) fn current_body_child(
    ctx: &Ctx<'_>,
    world_rc: &Rc<RefCell<World>>,
    root: NodeId,
) -> Result<Option<NodeId>> {
    let world = world_rc.borrow();
    let Some(parsed) = world.document(root) else {
        return Err(Exception::throw_type(ctx, "stale node"));
    };
    let base = &parsed.document.base;
    Ok(base.get_node(root.node).and_then(|candidate| {
        candidate.children.iter().find_map(|child| {
            let node = base.get_node(*child)?;
            match &node.data {
                NodeData::Element(element) => (element.name.ns == html_namespace()
                    && (element.name.local.as_str() == "body"
                        || element.name.local.as_str() == "frameset"))
                    .then_some(NodeId {
                        document: parsed.id,
                        node: *child,
                    }),
                _ => None,
            }
        })
    }))
}

/// The first node matching `selector` under `parsed`'s document root.
pub(super) fn document_first(parsed: &crate::Parsed, selector: &str) -> Option<NodeId> {
    let root = parsed.document.base.root_node().id;
    parsed
        .document
        .base
        .query_selector_in(root, selector)
        .ok()
        .flatten()
        .map(|node| NodeId {
            document: parsed.id,
            node,
        })
}

pub(super) fn document_first_child(
    parsed: &crate::Parsed,
    want: fn(&NodeData) -> bool,
) -> Option<NodeId> {
    let base = &parsed.document.base;
    base.get_node(base.root_node().id).and_then(|root| {
        root.children.iter().find_map(|child| {
            let node = base.get_node(*child)?;
            want(&node.data).then_some(NodeId {
                document: parsed.id,
                node: *child,
            })
        })
    })
}

/// The character set of `id`'s document for encoding-aware APIs, UTF-8
/// when the node has no document.
pub(super) fn document_character_set(ctx: &Ctx<'_>, id: NodeId) -> &'static str {
    world(ctx)
        .ok()
        .and_then(|world| {
            world
                .borrow()
                .document(id)
                .map(|parsed| parsed.character_set)
        })
        .unwrap_or("UTF-8")
}

pub(super) fn element_by_id<'js>(ctx: &Ctx<'js>, root: NodeId, id: &str) -> Result<Value<'js>> {
    let owner = world_for_node(ctx, root)?;
    let found = {
        let owner = owner.borrow();
        let Some(parsed) = owner.document(root) else {
            return Ok(Value::new_null(ctx.clone()));
        };
        find_element_by_id(&parsed.document.base, parsed.id, root.node, id)
    };
    match found {
        Some(node) => wrap_node(ctx, node),
        None => Ok(Value::new_null(ctx.clone())),
    }
}

/// Registers browsing contexts for iframes inserted by markup setters.
/// Those paths skip script insertion steps (innerHTML must not run scripts)
/// but still create iframe browsing contexts. The iframe insertion steps
/// only run for an iframe that became connected, so roots outside a
/// document (template contents, detached subtrees) are skipped
/// (<https://html.spec.whatwg.org/multipage/iframe-embed-object.html#the-iframe-element:html-element-insertion-steps>).
pub(super) fn register_inserted_iframes(ctx: &Ctx<'_>, inserted: &[NodeId]) -> Result<()> {
    let mut steps = Vec::new();
    {
        let world = world(ctx)?;
        let world = world.borrow();
        for root in inserted {
            let Some(parsed) = world.document(*root) else {
                continue;
            };
            if !is_connected(&parsed.document.base, root.node) {
                continue;
            }
            collect_html_insertion_steps(&parsed.document.base, *root, &mut steps);
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
            // Markup inserted through the parser's own paths runs the base
            // check in the parse walk; script-created markup runs it here
            // (<https://w3c.github.io/webappsec-csp/#is-base-allowed-for-document>).
            InsertionStep::Base(id) => {
                world(ctx)?.borrow_mut().apply_base_element(id);
            }
            InsertionStep::Script(_) | InsertionStep::Meta(_) => {}
        }
    }
    Ok(())
}

/// `meta name=referrer` insertion steps set the document's policy
/// (<https://html.spec.whatwg.org/multipage/semantics.html#meta-referrer>,
/// <https://w3c.github.io/webappsec-referrer-policy/#parse-referrer-policy-from-meta>).
pub(crate) fn apply_meta_referrer(ctx: &Ctx<'_>, id: NodeId) {
    let policy = {
        let world = world(ctx);
        let Ok(world) = world else {
            return;
        };
        let world = world.borrow();
        let Some(parsed) = world.document(id) else {
            return;
        };
        let base = &parsed.document.base;
        let name = crate::dom::tree::attr(base, id.node, "name").unwrap_or_default();
        if !name.eq_ignore_ascii_case("referrer") {
            return;
        }
        net::ReferrerPolicy::from_meta(
            crate::dom::tree::attr(base, id.node, "content").unwrap_or_default(),
        )
    };
    let Some(policy) = policy else {
        return;
    };
    let Ok(world) = world(ctx) else {
        return;
    };
    world.borrow_mut().policy.referrer_policy = Some(policy);
}

/// `meta http-equiv=refresh` insertion or attribute change runs the shared
/// declarative refresh steps with the element's current content
/// (<https://html.spec.whatwg.org/multipage/semantics.html#attr-meta-http-equiv-refresh>).
pub(crate) fn apply_meta_refresh(ctx: &Ctx<'_>, id: NodeId) {
    let Ok(world) = world(ctx) else {
        return;
    };
    let value = {
        let world = world.borrow();
        let Some(parsed) = world.document(id) else {
            return;
        };
        let base = &parsed.document.base;
        let Some(equiv) = crate::dom::tree::attr(base, id.node, "http-equiv") else {
            return;
        };
        if !equiv.eq_ignore_ascii_case("refresh") {
            return;
        }
        let Some(content) = crate::dom::tree::attr(base, id.node, "content") else {
            return;
        };
        if content.is_empty() {
            return;
        }
        content.to_owned()
    };
    world.borrow_mut().queue_declarative_refresh(value);
}

/// `meta http-equiv=content-security-policy` insertion appends one policy
/// (<https://html.spec.whatwg.org/multipage/semantics.html#attr-meta-http-equiv-content-security-policy>).
pub(super) fn apply_meta_csp(ctx: &Ctx<'_>, id: NodeId) {
    let Ok(world) = world(ctx) else {
        return;
    };
    world.borrow_mut().apply_meta_policy(id);
}

/// [Prepares](https://html.spec.whatwg.org/multipage/webappapis.html#prepare-the-script-element)
/// a classic inline script. Empty scripts return without setting already
/// started. External and module scripts are not prepared here.
pub(crate) fn prepare_classic_script(ctx: &Ctx<'_>, id: NodeId) -> Result<()> {
    enum Kind {
        Inline(String, u32),
        External,
    }
    let kind = {
        let world = world(ctx)?;
        let world = world.borrow();
        let Some(parsed) = world.document(id) else {
            return Ok(());
        };
        if parsed.document.script_already_started(id.node) {
            return Ok(());
        }
        if !crate::dom::tree::is_connected(&parsed.document.base, id.node) {
            return Ok(());
        }
        match crate::js::script_at(&world, id) {
            Some(crate::js::Script::Classic(crate::js::ScriptSource::Inline { source, line })) => {
                Kind::Inline(source, line)
            }
            Some(crate::js::Script::Classic(crate::js::ScriptSource::Src(_))) => Kind::External,
            _ => return Ok(()),
        }
    };
    // Script insertion clears the parser-inserted flag
    // (<https://html.spec.whatwg.org/multipage/scripting.html#parser-inserted>).
    world(ctx)?.borrow_mut().clear_parser_inserted(id);
    let (source, base_line) = match kind {
        Kind::Inline(source, line) => {
            // The script runs in its own document's realm, not the caller's.
            // Enter that realm now when it exists; the nested-context fork
            // permits it while the caller's script is still on the stack. Only
            // a document with no realm at all (a `window.open()` document the
            // engine has not installed yet) defers to its own drain.
            let caller = world(ctx)?;
            let owner = world_for_node(ctx, id)?;
            if !Rc::ptr_eq(&caller, &owner) {
                let owner_context = owner.borrow().realm_context.clone();
                if let Some(owner_context) = owner_context {
                    return owner_context.with(|owner_ctx| prepare_classic_script(&owner_ctx, id));
                }
                owner.borrow_mut().queue_deferred_script(id);
                return Ok(());
            }
            (source, line)
        }
        Kind::External => {
            // Parser-inserted scripts never reach this path; the parser
            // runs them from `Document::run_scripts`. The fetch for a
            // script-inserted element starts in `Document::adopt_js_work`,
            // which marks already-started at launch so a second prepare
            // schedules nothing
            // (<https://html.spec.whatwg.org/multipage/scripting.html#prepare-the-script-element>).
            let world = world(ctx)?;
            world.borrow_mut().queue_script(id);
            return Ok(());
        }
    };
    if source.is_empty() {
        return Ok(());
    }
    {
        let world = world(ctx)?;
        let world = world.borrow();
        if let Some(mut parsed) = world.document_mut(id) {
            parsed.document.mark_script_started(id.node);
        }
    }
    let violations = {
        let world = world(ctx)?;
        let world = world.borrow();
        let slot = world.state.nonce_slots.get(&id).map(String::as_str);
        let (nonce, nonceable) = world.document(id).map_or_else(
            || (String::new(), false),
            |parsed| crate::csp::element_nonce(&parsed.document.base, id.node, slot),
        );
        world.policy.csp.check_inline(
            &world.document_url,
            &crate::csp::InlineScript {
                source: &source,
                nonce: &nonce,
                nonceable,
                parser_inserted: false,
                kind: crate::csp::InlineKind::Script,
            },
        )
    };
    if !violations.is_empty() {
        super::super::fire_csp_violations(ctx, id, &violations)?;
    }
    if crate::csp::blocks(&violations) {
        return Ok(());
    }
    let previous = world(ctx)?.borrow().state.current_script;
    world(ctx)?.borrow_mut().state.current_script = Some(id);
    let mut options = EvalOptions::default();
    options.strict = false;
    // Text-set scripts run under the document URL so their stacks attribute
    // to page script, matching parser-inserted scripts.
    let filename = world_for_node(ctx, id)?
        .borrow()
        .document_url
        .as_str()
        .to_owned();
    options.filename = Some(filename.clone());
    if let Err(error) = ctx.eval_with_options::<Value, _>(source, options) {
        // A script inserted from script reports through the same pipeline as
        // a parser script: the exception reaches `window.onerror` and the
        // `error` event instead of staying pending
        // (<https://html.spec.whatwg.org/multipage/webappapis.html#run-a-classic-script>).
        crate::js::report_script_error(ctx, &error, base_line, &filename);
    }
    world(ctx)?.borrow_mut().state.current_script = previous;
    Ok(())
}

/// Sets the `nonce` content attribute to the empty string on HTML, SVG, and
/// `MathML` elements that just became browsing-context connected. An
/// IDL-set slot survives: only the content attribute clears, so a later IDL
/// read still answers the assigned nonce. Without a slot, the current
/// attribute value seeds the slot first (matching the attribute-change
/// steps, where the value feeds the slot). No script runs between the
/// clear and the restore, so the transient slot is unobservable.
pub(super) fn clear_nonce_on_connect(ctx: &Ctx<'_>, inserted: &[NodeId]) -> Result<()> {
    let mut ids = Vec::new();
    {
        let world = world(ctx)?;
        let world = world.borrow();
        for root in inserted {
            let Some(parsed) = world.document(*root) else {
                continue;
            };
            collect_nonce_elements(&parsed.document.base, *root, &mut ids);
        }
    }
    for id in ids {
        let owner = world_for_node(ctx, id)?;
        let saved = owner
            .borrow()
            .state
            .nonce_slots
            .get(&id)
            .cloned()
            .or_else(|| {
                owner.borrow().document(id).and_then(|parsed| {
                    crate::dom::tree::attr(&parsed.document.base, id.node, "nonce")
                        .map(str::to_owned)
                })
            });
        set_attribute_sync(ctx, id, "nonce", "")?;
        if let Some(slot) = saved {
            owner.borrow_mut().state.nonce_slots.insert(id, slot);
        }
    }
    Ok(())
}

/// Whether `id` is an HTML `iframe` or `frame` container.
///
/// The getter does not create a browsing context: that is the iframe
/// element's insertion steps, which run in tree order with script insertion
/// steps so an earlier script cannot observe a later sibling's
/// `contentWindow`
/// (<https://html.spec.whatwg.org/multipage/iframe-embed-object.html#dom-iframe-contentwindow>,
/// <https://html.spec.whatwg.org/multipage/iframe-embed-object.html#the-iframe-element:html-element-insertion-steps>).
/// `frame` shares the lookup: the engine has no frameset navigation pipeline,
/// so no frame document ever exists yet and the callers below return null,
/// matching the pre-migration binary.
pub(super) fn iframe_frame(ctx: &Ctx<'_>, id: NodeId) -> Result<bool> {
    let owner = world_for_node(ctx, id)?;
    Ok(owner.borrow().document(id).is_some_and(|parsed| {
        is_html_element(&parsed.document.base, id.node, "iframe")
            || is_html_element(&parsed.document.base, id.node, "frame")
    }))
}

/// One `body` content attribute of `document`, or the empty string when the
/// body or attribute is missing.
pub(super) fn document_body_attr<'js>(
    ctx: &Ctx<'js>,
    document: NodeId,
    name: &str,
) -> Result<rquickjs::String<'js>> {
    let owner = world_for_node(ctx, document)?;
    let value = owner
        .borrow()
        .document(document)
        .and_then(|parsed| document_first(&parsed, "body"))
        .and_then(|body| content_attr(ctx, body, name))
        .unwrap_or_default();
    rquickjs::String::from_str(ctx.clone(), &value)
}

/// Writes one `body` content attribute of `document`, ignoring a missing body.
pub(super) fn set_document_body_attr(
    ctx: &Ctx<'_>,
    document: NodeId,
    name: &str,
    value: &str,
) -> Result<()> {
    let owner = world_for_node(ctx, document)?;
    let body = owner
        .borrow()
        .document(document)
        .and_then(|parsed| document_first(&parsed, "body"));
    if let Some(body) = body {
        set_attribute_sync(ctx, body, name, value)?;
    }
    Ok(())
}

pub(super) fn install(ctx: &Ctx<'_>) -> Result<()> {
    document_generated::install(ctx)?;
    document_fragment_generated::install(ctx)?;
    shadow_root_generated::install(ctx)?;
    html_template_element_generated::install(ctx)?;
    htmli_frame_element_generated::install(ctx)?;
    html_frame_element_generated::install(ctx)?;
    Ok(())
}

impl JsNode {
    // https://html.spec.whatwg.org/multipage/dom.html#dom-document-scripts
    pub(super) fn get_scripts<'js>(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        let world_rc = world(ctx)?;
        if let Some(saved) = world_rc
            .borrow()
            .same_object(self.handle.get().0, SameObject::Scripts)
            && let Some(value) = deref_weak(ctx, saved)?
        {
            return Ok(value);
        }
        let value = live_collection(
            ctx,
            self.handle.get().0,
            CollectionKind::HtmlScripts,
            Some("HTMLCollection"),
        )?;
        let weak = make_weak(ctx, value.clone())?;
        world_rc.borrow_mut().intern_same_object(
            self.handle.get().0,
            SameObject::Scripts,
            Persistent::save(ctx, weak),
        );
        Ok(value)
    }

    /// <https://html.spec.whatwg.org/multipage/dom.html#dom-document-links>
    pub(super) fn get_links<'js>(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        let world_rc = world(ctx)?;
        if let Some(saved) = world_rc
            .borrow()
            .same_object(self.handle.get().0, SameObject::Links)
            && let Some(value) = deref_weak(ctx, saved)?
        {
            return Ok(value);
        }
        let value = live_collection(
            ctx,
            self.handle.get().0,
            CollectionKind::HtmlLinks,
            Some("HTMLCollection"),
        )?;
        let weak = make_weak(ctx, value.clone())?;
        world_rc.borrow_mut().intern_same_object(
            self.handle.get().0,
            SameObject::Links,
            Persistent::save(ctx, weak),
        );
        Ok(value)
    }

    /// <https://dom.spec.whatwg.org/#dom-document-adoptnode>
    pub(super) fn adopt_node<'js>(
        &self,
        ctx: Ctx<'js>,
        node: host::NodeReference,
    ) -> Result<Value<'js>> {
        match node {
            NodeReference::Tree(id) => {
                let adopted = adopt_into_document(&ctx, self.handle.get().0, id)?;
                schedule_mutation_delivery(&ctx)?;
                wrap_node(&ctx, adopted)
            }
            NodeReference::Attribute { scope, id } => {
                import_attr(&ctx, self.handle.get().0, scope, id)
            }
        }
    }
}

/// Names that may host a shadow root, plus a valid custom element name
/// (<https://html.spec.whatwg.org/multipage/dom.html#valid-shadow-host-name>,
/// <https://html.spec.whatwg.org/multipage/custom-elements.html#valid-custom-element-name>).
fn valid_shadow_host(local: &str) -> bool {
    matches!(
        local,
        "article"
            | "aside"
            | "blockquote"
            | "body"
            | "div"
            | "footer"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "header"
            | "main"
            | "nav"
            | "p"
            | "section"
            | "span"
            | "search"
    ) || valid_custom_element_name(local)
}

/// `PCENChar` (<https://html.spec.whatwg.org/multipage/custom-elements.html#prod-pcenchar>).
fn is_pcen_char(ch: char) -> bool {
    matches!(ch, '-' | '.' | '0'..='9' | '_' | 'a'..='z' | '\u{00B7}')
        || ('\u{00C0}'..='\u{00D6}').contains(&ch)
        || ('\u{00D8}'..='\u{00F6}').contains(&ch)
        || ('\u{00F8}'..='\u{037D}').contains(&ch)
        || ('\u{037F}'..='\u{1FFF}').contains(&ch)
        || ('\u{200C}'..='\u{200D}').contains(&ch)
        || ('\u{203F}'..='\u{2040}').contains(&ch)
        || ('\u{2070}'..='\u{218F}').contains(&ch)
        || ('\u{2C00}'..='\u{2FEF}').contains(&ch)
        || ('\u{3001}'..='\u{D7FF}').contains(&ch)
        || ('\u{F900}'..='\u{FDCF}').contains(&ch)
        || ('\u{FDF0}'..='\u{FFFD}').contains(&ch)
        || ('\u{10000}'..='\u{EFFFF}').contains(&ch)
}

fn valid_custom_element_name(local: &str) -> bool {
    const FORBIDDEN: &[&str] = &[
        "annotation-xml",
        "color-profile",
        "font-face",
        "font-face-src",
        "font-face-uri",
        "font-face-format",
        "font-face-name",
        "missing-glyph",
    ];
    if FORBIDDEN.contains(&local) {
        return false;
    }
    // PotentialCustomElementName is `[a-z] PCENChar* '-' PCENChar*`
    // (<https://html.spec.whatwg.org/multipage/custom-elements.html#valid-custom-element-name>).
    let mut chars = local.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_lowercase() {
        return false;
    }
    let mut hyphen = false;
    for ch in chars {
        if ch == '-' {
            hyphen = true;
        }
        if !is_pcen_char(ch) {
            return false;
        }
    }
    hyphen
}

/// Fragment context for `innerHTML`: the element itself, or a shadow root's host
/// (<https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-shadowroot-innerhtml>).
pub(super) fn inner_html_fragment_context(ctx: &Ctx<'_>, id: NodeId) -> Result<String> {
    let owner = world_for_node(ctx, id)?;
    let parsed = owner.borrow();
    let Some(parsed) = parsed.document(id) else {
        return Err(Exception::throw_type(ctx, "no document"));
    };
    let Some(node) = parsed.document.base.get_node(id.node) else {
        return Err(Exception::throw_type(ctx, "stale node"));
    };
    let name = match &node.data {
        NodeData::Element(element) => element.name.clone(),
        NodeData::ShadowRoot { host, .. } => parsed
            .document
            .base
            .get_node(*host)
            .and_then(|host| host.data.downcast_element())
            .map(|element| element.name.clone())
            .ok_or_else(|| Exception::throw_type(ctx, "shadow root has no host"))?,
        _ => {
            return Err(Exception::throw_type(
                ctx,
                "innerHTML requires an element or shadow root",
            ));
        }
    };
    Ok(html_fragment_context(&name))
}
