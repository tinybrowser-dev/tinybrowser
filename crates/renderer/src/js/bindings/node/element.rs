//! Element interfaces: `Element`, `HTMLElement`, `SVG` and `MathML` elements,
//! ARIA and editing mixins, hyperlink utilities, and generic reflection.

use super::super::{
    AttrArgument, CollectionKind, JsNamedNodeMap, JsTokenList, LegacyNullString, NodeContext,
    NodeOrString, OptString, WebIdlString, after_attribute_change, attached_attr_id, attr_owner,
    attr_state, attr_wrapper, attribute_local_name, attribute_value, blur_node, deref_weak,
    document_is_html_content, dom_string, element_box_ctx, element_click,
    fixup_focus_after_removal, focus_node, host, host_node_id, is_focusable, js_string_to_utf8,
    live_collection, make_weak, rect_object, remove_attribute_sync, schedule_mutation_delivery,
    set_attribute_node, set_attribute_sync, string_value, throw_dom, throw_dom_error, touch_attr,
    valid_attribute_local_name, validate_and_extract, with_node_data, world, world_for_node,
    wrap_node,
};
use super::forms::fixup_controls_on_insert;
use crate::dom::adoption::materialize_children;
use std::collections::HashSet;

use blitz_dom::NodeData;

use rquickjs::{Array, Class, Ctx, Exception, Function, Persistent, Result, Value};

use crate::dom::insertion::{
    fragment_children, iframe_containers_in_subtrees, place_journaled, replace_all_journaled,
    replace_parsed, unlink_journaled,
};
use crate::dom::serialization;
use crate::dom::tree::{
    BlitzId, NodeId, element_node_name, html_namespace, qualified_name, sibling,
};
use crate::dom_string::DomString;

use crate::documents::JournalEntry;
use crate::js::world::{NodeReference, SameObject};

use super::JsNode;
use super::core::{adopt_node, insert_tree_node, insertion_tree_nodes, upgrade_parsed_tree};
use super::document::{
    document_first_child, fragment_base_url, html_fragment_context, parse_html_fragment_snapshots,
    place_adjacent_rest, register_inserted_iframes,
};

include!(concat!(env!("OUT_DIR"), "/Element.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLElement.rs"));
include!(concat!(env!("OUT_DIR"), "/SVGElement.rs"));
include!(concat!(env!("OUT_DIR"), "/MathMLElement.rs"));
include!(concat!(env!("OUT_DIR"), "/ARIAMixin.rs"));
include!(concat!(env!("OUT_DIR"), "/ElementCSSInlineStyle.rs"));
include!(concat!(env!("OUT_DIR"), "/ElementContentEditable.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLHyperlinkElementUtils.rs"));

impl<'js> element_generated::Element<'js> for JsNode {
    // https://dom.spec.whatwg.org/#dom-element-getelementsbytagname
    fn get_elements_by_tag_name(
        &self,
        ctx: Ctx<'js>,
        arg_0: rquickjs::String<'js>,
    ) -> Result<Value<'js>> {
        self.get_elements_by_tag_name(ctx, WebIdlString(arg_0.to_string()?))
    }

    // https://dom.spec.whatwg.org/#dom-element-getelementsbytagnamens
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

    // https://dom.spec.whatwg.org/#dom-element-getelementsbyclassname
    fn get_elements_by_class_name(
        &self,
        ctx: Ctx<'js>,
        arg_0: rquickjs::String<'js>,
    ) -> Result<Value<'js>> {
        self.get_elements_by_class_name(ctx, WebIdlString(arg_0.to_string()?))
    }

    // https://dom.spec.whatwg.org/#dom-element-matches
    fn matches(&self, ctx: Ctx<'js>, arg_0: rquickjs::String<'js>) -> Result<bool> {
        self.matches(ctx, WebIdlString(arg_0.to_string()?))
    }

    /// <https://dom.spec.whatwg.org/#dom-element-webkitmatchesselector>
    fn webkit_matches_selector(&self, ctx: Ctx<'js>, arg_0: rquickjs::String<'js>) -> Result<bool> {
        // Legacy alias of `matches(selectors)`.
        self.matches(ctx, WebIdlString(arg_0.to_string()?))
    }

    // https://dom.spec.whatwg.org/#dom-element-closest
    fn closest(&self, ctx: Ctx<'js>, arg_0: rquickjs::String<'js>) -> Result<Value<'js>> {
        self.closest(ctx, WebIdlString(arg_0.to_string()?))
    }

    // https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-innerhtml
    fn get_inner_html(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        self.inner_html(ctx)
    }

    // https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-innerhtml
    fn set_inner_html(&self, ctx: &Ctx<'js>, value: rquickjs::String<'js>) -> Result<()> {
        self.set_inner_html(ctx, LegacyNullString(value.to_string()?))
    }

    // https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-outerhtml
    fn get_outer_html(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        self.outer_html(ctx)
    }

    // https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-outerhtml
    fn set_outer_html(&self, ctx: &Ctx<'js>, value: rquickjs::String<'js>) -> Result<()> {
        self.set_outer_html(ctx, LegacyNullString(value.to_string()?))
    }

    // https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-insertadjacenthtml
    fn insert_adjacent_html(
        &self,
        ctx: Ctx<'js>,
        arg_0: rquickjs::String<'js>,
        arg_1: rquickjs::String<'js>,
    ) -> Result<()> {
        self.insert_adjacent_html(
            ctx,
            WebIdlString(arg_0.to_string()?),
            WebIdlString(arg_1.to_string()?),
        )
    }

    /// <https://dom.spec.whatwg.org/#dom-element-insertadjacentelement>
    fn insert_adjacent_element(
        &self,
        ctx: Ctx<'js>,
        arg_0: rquickjs::String<'js>,
        arg_1: Value<'js>,
    ) -> Result<Value<'js>> {
        let element = require_context_element(&ctx, self.handle.get().0)?;
        let node = require_element_argument(&ctx, &arg_1)?;
        match insert_adjacent(&ctx, element, &arg_0.to_string()?, node)? {
            Some(node) => wrap_node(&ctx, node),
            None => Ok(Value::new_null(ctx)),
        }
    }

    /// <https://dom.spec.whatwg.org/#dom-element-insertadjacenttext>
    fn insert_adjacent_text(
        &self,
        ctx: Ctx<'js>,
        arg_0: rquickjs::String<'js>,
        arg_1: rquickjs::String<'js>,
    ) -> Result<()> {
        let element = require_context_element(&ctx, self.handle.get().0)?;
        let text = {
            let world = world(&ctx)?;
            let world = world.borrow();
            let Some(mut parsed) = world.document_mut(element) else {
                return Err(Exception::throw_type(&ctx, "no document"));
            };
            let node = parsed
                .document
                .base
                .mutate()
                .create_text_node(&arg_1.to_string()?);
            NodeId {
                document: element.document,
                node,
            }
        };
        insert_adjacent(&ctx, element, &arg_0.to_string()?, text)?;
        Ok(())
    }

    // https://drafts.csswg.org/cssom-view/#dom-element-getboundingclientrect
    fn get_bounding_client_rect(&self, ctx: Ctx<'js>) -> Result<Value<'js>> {
        self.get_bounding_client_rect(ctx)
    }

    // https://drafts.csswg.org/cssom-view/#dom-element-getclientrects
    fn get_client_rects(&self, ctx: Ctx<'js>) -> Result<Value<'js>> {
        self.get_client_rects(ctx)
    }

    // https://drafts.csswg.org/cssom-view/#dom-element-scrollintoview
    fn scroll_into_view(
        &self,
        ctx: Ctx<'js>,
        arg_0: element_generated::BooleanOrScrollIntoViewOptions,
    ) -> Result<()> {
        let (behavior, block, inline) = match arg_0 {
            // `false` asks for the end edge; every other boolean is the
            // default start edge. The inline axis always defaults to nearest
            // (<https://drafts.csswg.org/cssom-view/#dom-element-scrollintoview>).
            element_generated::BooleanOrScrollIntoViewOptions::Boolean(false) => (
                element_generated::ScrollBehavior::Auto,
                element_generated::ScrollLogicalPosition::End,
                element_generated::ScrollLogicalPosition::Nearest,
            ),
            element_generated::BooleanOrScrollIntoViewOptions::Boolean(true) => (
                element_generated::ScrollBehavior::Auto,
                element_generated::ScrollLogicalPosition::Start,
                element_generated::ScrollLogicalPosition::Nearest,
            ),
            element_generated::BooleanOrScrollIntoViewOptions::ScrollIntoViewOptions(options) => {
                (options.behavior, options.block, options.inline)
            }
        };
        self.scroll_into_view(ctx, behavior.into(), block.into(), inline.into())
    }

    // https://drafts.csswg.org/cssom-view/#dom-element-scrollleft
    fn get_scroll_left(&self, ctx: &Ctx<'js>) -> Result<f64> {
        self.scroll_left(ctx)
    }

    // https://drafts.csswg.org/#dom-element-scrollleft
    fn set_scroll_left(&self, ctx: &Ctx<'js>, value: f64) -> Result<()> {
        self.set_scroll_left(ctx, value)
    }

    // https://drafts.csswg.org/cssom-view/#dom-element-scrolltop
    fn get_scroll_top(&self, ctx: &Ctx<'js>) -> Result<f64> {
        self.scroll_top(ctx)
    }

    // https://drafts.csswg.org/#dom-element-scrolltop
    fn set_scroll_top(&self, ctx: &Ctx<'js>, value: f64) -> Result<()> {
        self.set_scroll_top(ctx, value)
    }

    // https://drafts.csswg.org/cssom-view/#dom-element-clientwidth
    fn get_client_width(&self, ctx: &Ctx<'js>) -> Result<i32> {
        self.client_width(ctx)
    }

    // https://drafts.csswg.org/cssom-view/#dom-element-clientheight
    fn get_client_height(&self, ctx: &Ctx<'js>) -> Result<i32> {
        self.client_height(ctx)
    }

    // https://drafts.csswg.org/cssom-view/#dom-element-scrollwidth
    fn get_scroll_width(&self, ctx: &Ctx<'js>) -> Result<i32> {
        self.scroll_width(ctx)
    }

    // https://drafts.csswg.org/cssom-view/#dom-element-scrollheight
    fn get_scroll_height(&self, ctx: &Ctx<'js>) -> Result<i32> {
        self.scroll_height(ctx)
    }

    // https://dom.spec.whatwg.org/#dom-element-attachshadow
    fn attach_shadow(
        &self,
        ctx: Ctx<'js>,
        arg_0: element_generated::ShadowRootInit,
    ) -> Result<Value<'js>> {
        self.attach_shadow(ctx, arg_0)
    }

    // https://dom.spec.whatwg.org/#dom-element-shadowroot
    fn get_shadow_root(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        self.shadow_root(ctx)
    }

    // https://dom.spec.whatwg.org/#dom-element-tagname
    fn get_tag_name(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        let name = self.tag_name(ctx)?;
        rquickjs::String::from_str(ctx.clone(), &name)
    }

    // https://dom.spec.whatwg.org/#dom-element-localname
    fn get_local_name(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        let name = self.local_name(ctx)?;
        rquickjs::String::from_str(ctx.clone(), &name)
    }

    // https://dom.spec.whatwg.org/#dom-element-prefix
    fn get_prefix(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        let value = self.prefix(ctx)?;
        if value.is_null() || value.is_undefined() {
            Ok(None)
        } else {
            rquickjs::FromJs::from_js(ctx, value).map(Some)
        }
    }

    // https://dom.spec.whatwg.org/#dom-element-namespaceuri
    fn get_namespace_uri(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        let value = self.namespace_uri(ctx)?;
        if value.is_null() || value.is_undefined() {
            Ok(None)
        } else {
            rquickjs::FromJs::from_js(ctx, value).map(Some)
        }
    }

    // https://dom.spec.whatwg.org/#dom-element-id
    fn get_id(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        let id = self.id(ctx)?;
        rquickjs::String::from_str(ctx.clone(), &id)
    }

    // https://dom.spec.whatwg.org/#dom-element-id
    fn set_id(&self, ctx: &Ctx<'js>, value: rquickjs::String<'js>) -> Result<()> {
        self.set_id(ctx, WebIdlString(js_string_to_utf8(&value)?))
    }

    // https://dom.spec.whatwg.org/#dom-element-classname
    fn get_class_name(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        let name = self.class_name(ctx)?;
        rquickjs::String::from_str(ctx.clone(), &name)
    }

    // https://dom.spec.whatwg.org/#dom-element-classname
    fn set_class_name(&self, ctx: &Ctx<'js>, value: rquickjs::String<'js>) -> Result<()> {
        self.set_class_name(ctx, WebIdlString(value.to_string()?))
    }

    // https://dom.spec.whatwg.org/#dom-element-classlist
    fn get_class_list(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        self.class_list(ctx)
    }

    // https://dom.spec.whatwg.org/#dom-element-getattribute
    fn get_attribute(
        &self,
        ctx: Ctx<'js>,
        arg_0: rquickjs::String<'js>,
    ) -> Result<Option<rquickjs::String<'js>>> {
        let value = self.get_attribute(ctx.clone(), WebIdlString(arg_0.to_string()?))?;
        if value.is_null() || value.is_undefined() {
            Ok(None)
        } else {
            rquickjs::FromJs::from_js(&ctx, value).map(Some)
        }
    }

    // https://dom.spec.whatwg.org/#dom-element-setattribute
    fn set_attribute(
        &self,
        ctx: Ctx<'js>,
        arg_0: rquickjs::String<'js>,
        arg_1: rquickjs::String<'js>,
    ) -> Result<()> {
        let units = crate::dom_string::DomString::from_utf16(arg_1.to_utf16()?);
        let name = arg_0.to_string()?;
        self.set_attribute(
            ctx.clone(),
            WebIdlString(name.clone()),
            WebIdlString(js_string_to_utf8(&arg_1)?),
        )?;
        // The tree attribute drops unpaired surrogates so CSS `id` matching
        // stays distinct from U+FFFD. The getter returns the original units
        // (<https://infra.spec.whatwg.org/#javascript-string-convert>).
        if units.has_unpaired_surrogate() {
            let local = attribute_local_name(&ctx, self.handle.get().0, &name);
            let world = world(&ctx)?;
            if let Some(mut parsed) = world.borrow().document_mut(self.handle.get().0) {
                parsed
                    .document
                    .remember_attribute(self.handle.get().0.node, &local, units);
            }
        }
        Ok(())
    }

    // https://dom.spec.whatwg.org/#dom-element-hasattribute
    fn has_attribute(&self, ctx: Ctx<'js>, arg_0: rquickjs::String<'js>) -> Result<bool> {
        self.has_attribute(ctx, WebIdlString(arg_0.to_string()?))
    }

    // https://dom.spec.whatwg.org/#dom-element-removeattribute
    fn remove_attribute(&self, ctx: Ctx<'js>, arg_0: rquickjs::String<'js>) -> Result<()> {
        self.remove_attribute(ctx, WebIdlString(arg_0.to_string()?))
    }

    // https://dom.spec.whatwg.org/#dom-element-toggleattribute
    fn toggle_attribute(
        &self,
        ctx: Ctx<'js>,
        arg_0: rquickjs::String<'js>,
        arg_1: Option<bool>,
    ) -> Result<bool> {
        self.toggle_attribute(ctx, WebIdlString(arg_0.to_string()?), arg_1)
    }

    // https://dom.spec.whatwg.org/#dom-element-getattributens
    fn get_attribute_ns(
        &self,
        ctx: Ctx<'js>,
        arg_0: Option<rquickjs::String<'js>>,
        arg_1: rquickjs::String<'js>,
    ) -> Result<Option<rquickjs::String<'js>>> {
        let value = self.get_attribute_ns(
            ctx.clone(),
            OptString(arg_0.map(|name| name.to_string()).transpose()?),
            WebIdlString(arg_1.to_string()?),
        )?;
        if value.is_null() || value.is_undefined() {
            Ok(None)
        } else {
            rquickjs::FromJs::from_js(&ctx, value).map(Some)
        }
    }

    // https://dom.spec.whatwg.org/#dom-element-setattributens
    fn set_attribute_ns(
        &self,
        ctx: Ctx<'js>,
        arg_0: Option<rquickjs::String<'js>>,
        arg_1: rquickjs::String<'js>,
        arg_2: rquickjs::String<'js>,
    ) -> Result<()> {
        self.set_attribute_ns(
            ctx,
            OptString(arg_0.map(|name| name.to_string()).transpose()?),
            WebIdlString(arg_1.to_string()?),
            WebIdlString(arg_2.to_string()?),
        )
    }

    // https://dom.spec.whatwg.org/#dom-element-hasattributens
    fn has_attribute_ns(
        &self,
        ctx: Ctx<'js>,
        arg_0: Option<rquickjs::String<'js>>,
        arg_1: rquickjs::String<'js>,
    ) -> Result<bool> {
        self.has_attribute_ns(
            ctx,
            OptString(arg_0.map(|name| name.to_string()).transpose()?),
            WebIdlString(arg_1.to_string()?),
        )
    }

    // https://dom.spec.whatwg.org/#dom-element-removeattributens
    fn remove_attribute_ns(
        &self,
        ctx: Ctx<'js>,
        arg_0: Option<rquickjs::String<'js>>,
        arg_1: rquickjs::String<'js>,
    ) -> Result<()> {
        self.remove_attribute_ns(
            ctx,
            OptString(arg_0.map(|name| name.to_string()).transpose()?),
            WebIdlString(arg_1.to_string()?),
        )
    }

    // https://dom.spec.whatwg.org/#dom-element-getattributenames
    fn get_attribute_names(&self, ctx: Ctx<'js>) -> Result<Vec<String>> {
        self.get_attribute_names(ctx)
    }

    // https://dom.spec.whatwg.org/#dom-element-attributes
    fn get_attributes(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        self.attributes(ctx)
    }

    // https://dom.spec.whatwg.org/#dom-element-hasattributes
    fn has_attributes(&self, ctx: Ctx<'js>) -> Result<bool> {
        self.has_attributes(ctx)
    }

    // https://dom.spec.whatwg.org/#dom-element-getattributenode
    fn get_attribute_node(
        &self,
        ctx: Ctx<'js>,
        arg_0: rquickjs::String<'js>,
    ) -> Result<Value<'js>> {
        self.get_attribute_node(ctx, WebIdlString(arg_0.to_string()?))
    }

    // https://dom.spec.whatwg.org/#dom-element-getattributenodens
    fn get_attribute_node_ns(
        &self,
        ctx: Ctx<'js>,
        arg_0: Option<rquickjs::String<'js>>,
        arg_1: rquickjs::String<'js>,
    ) -> Result<Value<'js>> {
        self.get_attribute_node_ns(
            ctx,
            OptString(arg_0.map(|name| name.to_string()).transpose()?),
            WebIdlString(arg_1.to_string()?),
        )
    }

    // https://dom.spec.whatwg.org/#dom-element-setattributenode
    fn set_attribute_node(&self, ctx: Ctx<'js>, arg_0: Value<'js>) -> Result<Value<'js>> {
        let attr = AttrArgument::from_value(&arg_0)?;
        self.set_attribute_node(ctx, attr)
    }

    // https://dom.spec.whatwg.org/#dom-element-setattributenodens
    fn set_attribute_node_ns(&self, ctx: Ctx<'js>, arg_0: Value<'js>) -> Result<Value<'js>> {
        let attr = AttrArgument::from_value(&arg_0)?;
        self.set_attribute_node_ns(ctx, attr)
    }

    // https://dom.spec.whatwg.org/#dom-element-removeattributenode
    fn remove_attribute_node(&self, ctx: Ctx<'js>, arg_0: Value<'js>) -> Result<Value<'js>> {
        let attr = AttrArgument::from_value(&arg_0)?;
        self.remove_attribute_node(ctx, attr)
    }
}

impl<'js> html_element_generated::HTMLElement<'js> for JsNode {
    // https://html.spec.whatwg.org/multipage/dom.html#dom-dataset
    fn get_dataset(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        self.dataset(ctx)
    }

    // The cryptographic nonce: reading answers the slot, writing sets it
    // without touching the content attribute
    // (<https://html.spec.whatwg.org/multipage/urls-and-fetching.html#dom-nonce>).
    fn get_nonce(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        let value = nonce_of(ctx, self.handle.get().0)?;
        rquickjs::String::from_str(ctx.clone(), &value)
    }

    // https://html.spec.whatwg.org/multipage/urls-and-fetching.html#dom-nonce
    fn set_nonce(&self, ctx: &Ctx<'js>, value: rquickjs::String<'js>) -> Result<()> {
        set_nonce_slot(ctx, self.handle.get().0, &value.to_string()?)
    }

    // The computed writing-suggestions value: the nearest ancestor-or-self
    // with the attribute decides (`false` only for an ASCII
    // case-insensitive "false"; any other value means true), defaulting to
    // true
    // (<https://html.spec.whatwg.org/multipage/editing.html#dom-writingsuggestions>).
    fn get_writing_suggestions(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        let owner = world_for_node(ctx, self.handle.get().0)?;
        let borrowed = owner.borrow();
        let Some(parsed) = borrowed.document(self.handle.get().0) else {
            return rquickjs::String::from_str(ctx.clone(), "true");
        };
        let base = &parsed.document.base;
        rquickjs::String::from_str(
            ctx.clone(),
            writing_suggestions_value(base, self.handle.get().0.node),
        )
    }

    // The `autocapitalize` state keyword: `off`/`none` read "none", `on`/
    // `sentences` read "sentences", missing or empty reads "", and anything
    // else reads "sentences" (the invalid value default)
    // (<https://html.spec.whatwg.org/multipage/editing.html#dom-autocapitalize>).
    fn get_autocapitalize(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        let raw = content_attr(ctx, self.handle.get().0, "autocapitalize");
        rquickjs::String::from_str(ctx.clone(), autocapitalize_keyword(raw.as_deref()))
    }

    // The `tabindex` content attribute parses as an integer; absence,
    // parse failure, or overflow reads as the element default (0 for the
    // focusable locals, -1 otherwise)
    // (<https://html.spec.whatwg.org/multipage/interaction.html#dom-tabindex>).
    fn get_tab_index(&self, ctx: &Ctx<'js>) -> Result<i32> {
        let owner = world_for_node(ctx, self.handle.get().0)?;
        let borrowed = owner.borrow();
        let Some(parsed) = borrowed.document(self.handle.get().0) else {
            return Ok(-1);
        };
        let base = &parsed.document.base;
        Ok(
            crate::dom::tree::attr(base, self.handle.get().0.node, "tabindex")
                .and_then(crate::dom::parse::parse_integer)
                .unwrap_or_else(|| tab_index_default(base, self.handle.get().0.node)),
        )
    }

    // https://html.spec.whatwg.org/multipage/interaction.html#dom-click
    fn click(&self, ctx: Ctx<'js>) -> Result<()> {
        element_click(&ctx, self.handle.get().0)
    }

    // https://html.spec.whatwg.org/multipage/interaction.html#dom-focus
    fn focus(&self, ctx: Ctx<'js>, _options: html_element_generated::FocusOptions) -> Result<()> {
        focus_element(&ctx, self.handle.get().0)
    }

    // https://html.spec.whatwg.org/multipage/interaction.html#dom-blur
    fn blur(&self, ctx: Ctx<'js>) -> Result<()> {
        blur_node(&ctx, self.handle.get().0)
    }

    // https://drafts.csswg.org/cssom-view/#dom-htmlelement-offsetwidth
    fn get_offset_width(&self, ctx: &Ctx<'js>) -> Result<i32> {
        self.offset_width(ctx)
    }

    // https://drafts.csswg.org/cssom-view/#dom-htmlelement-offsetheight
    fn get_offset_height(&self, ctx: &Ctx<'js>) -> Result<i32> {
        self.offset_height(ctx)
    }

    // https://html.spec.whatwg.org/multipage/dom.html#the-innertext-idl-attribute
    fn get_inner_text(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        self.get_inner_text(ctx)
    }

    // https://html.spec.whatwg.org/multipage/dom.html#the-innertext-idl-attribute
    fn set_inner_text(&self, ctx: &Ctx<'js>, value: rquickjs::String<'js>) -> Result<()> {
        self.set_inner_text(ctx, LegacyNullString(value.to_string()?))
    }

    // https://html.spec.whatwg.org/multipage/dom.html#the-innertext-idl-attribute
    fn get_outer_text(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        self.get_outer_text(ctx)
    }

    // https://html.spec.whatwg.org/multipage/dom.html#the-innertext-idl-attribute
    fn set_outer_text(&self, ctx: &Ctx<'js>, value: rquickjs::String<'js>) -> Result<()> {
        self.set_outer_text(ctx, LegacyNullString(value.to_string()?))
    }
}

impl<'js> svg_element_generated::SVGElement<'js> for JsNode {
    // https://html.spec.whatwg.org/multipage/dom.html#dom-dataset
    fn get_dataset(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        self.dataset(ctx)
    }

    // https://html.spec.whatwg.org/multipage/urls-and-fetching.html#dom-nonce
    fn get_nonce(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        let value = nonce_of(ctx, self.handle.get().0)?;
        rquickjs::String::from_str(ctx.clone(), &value)
    }

    // https://html.spec.whatwg.org/multipage/urls-and-fetching.html#dom-nonce
    fn set_nonce(&self, ctx: &Ctx<'js>, value: rquickjs::String<'js>) -> Result<()> {
        set_nonce_slot(ctx, self.handle.get().0, &value.to_string()?)
    }

    // https://html.spec.whatwg.org/multipage/interaction.html#dom-focus
    fn focus(&self, ctx: Ctx<'js>, _options: svg_element_generated::FocusOptions) -> Result<()> {
        focus_element(&ctx, self.handle.get().0)
    }

    // https://html.spec.whatwg.org/multipage/interaction.html#dom-blur
    fn blur(&self, ctx: Ctx<'js>) -> Result<()> {
        blur_node(&ctx, self.handle.get().0)
    }
}

impl<'js> math_ml_element_generated::MathMLElement<'js> for JsNode {
    // https://html.spec.whatwg.org/multipage/dom.html#dom-dataset
    fn get_dataset(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        self.dataset(ctx)
    }

    // https://html.spec.whatwg.org/multipage/urls-and-fetching.html#dom-nonce
    fn get_nonce(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        let value = nonce_of(ctx, self.handle.get().0)?;
        rquickjs::String::from_str(ctx.clone(), &value)
    }

    // https://html.spec.whatwg.org/multipage/urls-and-fetching.html#dom-nonce
    fn set_nonce(&self, ctx: &Ctx<'js>, value: rquickjs::String<'js>) -> Result<()> {
        set_nonce_slot(ctx, self.handle.get().0, &value.to_string()?)
    }

    // https://html.spec.whatwg.org/multipage/interaction.html#dom-focus
    fn focus(
        &self,
        ctx: Ctx<'js>,
        _options: math_ml_element_generated::FocusOptions,
    ) -> Result<()> {
        focus_element(&ctx, self.handle.get().0)
    }

    // https://html.spec.whatwg.org/multipage/interaction.html#dom-blur
    fn blur(&self, ctx: Ctx<'js>) -> Result<()> {
        blur_node(&ctx, self.handle.get().0)
    }
}

/// `ARIAMixin` is a spec mixin included by `Element`: nullable string
/// reflection over `role` and the `aria-*` content attributes
/// (<https://w3c.github.io/aria/#introstates>).
impl<'js> aria_mixin_generated::ARIAMixin<'js> for JsNode {
    // https://w3c.github.io/aria/#role
    fn get_role(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "role")
    }

    // https://w3c.github.io/aria/#role
    fn set_role(&self, ctx: &Ctx<'js>, value: Option<rquickjs::String<'js>>) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "role", value)
    }

    // https://w3c.github.io/aria/#aria-atomic
    fn get_aria_atomic(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-atomic")
    }

    // https://w3c.github.io/aria/#aria-atomic
    fn set_aria_atomic(&self, ctx: &Ctx<'js>, value: Option<rquickjs::String<'js>>) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-atomic", value)
    }

    // https://w3c.github.io/aria/#aria-autocomplete
    fn get_aria_auto_complete(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-autocomplete")
    }

    // https://w3c.github.io/aria/#aria-autocomplete
    fn set_aria_auto_complete(
        &self,
        ctx: &Ctx<'js>,
        value: Option<rquickjs::String<'js>>,
    ) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-autocomplete", value)
    }

    // https://w3c.github.io/aria/#aria-braillelabel
    fn get_aria_braille_label(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-braillelabel")
    }

    // https://w3c.github.io/aria/#aria-braillelabel
    fn set_aria_braille_label(
        &self,
        ctx: &Ctx<'js>,
        value: Option<rquickjs::String<'js>>,
    ) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-braillelabel", value)
    }

    // https://w3c.github.io/aria/#aria-brailleroledescription
    fn get_aria_braille_role_description(
        &self,
        ctx: &Ctx<'js>,
    ) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-brailleroledescription")
    }

    // https://w3c.github.io/aria/#aria-brailleroledescription
    fn set_aria_braille_role_description(
        &self,
        ctx: &Ctx<'js>,
        value: Option<rquickjs::String<'js>>,
    ) -> Result<()> {
        aria_set(
            ctx,
            self.handle.get().0,
            "aria-brailleroledescription",
            value,
        )
    }

    // https://w3c.github.io/aria/#aria-busy
    fn get_aria_busy(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-busy")
    }

    // https://w3c.github.io/aria/#aria-busy
    fn set_aria_busy(&self, ctx: &Ctx<'js>, value: Option<rquickjs::String<'js>>) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-busy", value)
    }

    // https://w3c.github.io/aria/#aria-checked
    fn get_aria_checked(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-checked")
    }

    // https://w3c.github.io/aria/#aria-checked
    fn set_aria_checked(&self, ctx: &Ctx<'js>, value: Option<rquickjs::String<'js>>) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-checked", value)
    }

    // https://w3c.github.io/aria/#aria-colcount
    fn get_aria_col_count(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-colcount")
    }

    // https://w3c.github.io/aria/#aria-colcount
    fn set_aria_col_count(
        &self,
        ctx: &Ctx<'js>,
        value: Option<rquickjs::String<'js>>,
    ) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-colcount", value)
    }

    // https://w3c.github.io/aria/#aria-colindex
    fn get_aria_col_index(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-colindex")
    }

    // https://w3c.github.io/aria/#aria-colindex
    fn set_aria_col_index(
        &self,
        ctx: &Ctx<'js>,
        value: Option<rquickjs::String<'js>>,
    ) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-colindex", value)
    }

    // https://w3c.github.io/aria/#aria-colindextext
    fn get_aria_col_index_text(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-colindextext")
    }

    // https://w3c.github.io/aria/#aria-colindextext
    fn set_aria_col_index_text(
        &self,
        ctx: &Ctx<'js>,
        value: Option<rquickjs::String<'js>>,
    ) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-colindextext", value)
    }

    // https://w3c.github.io/aria/#aria-colspan
    fn get_aria_col_span(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-colspan")
    }

    // https://w3c.github.io/aria/#aria-colspan
    fn set_aria_col_span(
        &self,
        ctx: &Ctx<'js>,
        value: Option<rquickjs::String<'js>>,
    ) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-colspan", value)
    }

    // https://w3c.github.io/aria/#aria-current
    fn get_aria_current(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-current")
    }

    // https://w3c.github.io/aria/#aria-current
    fn set_aria_current(&self, ctx: &Ctx<'js>, value: Option<rquickjs::String<'js>>) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-current", value)
    }

    // https://w3c.github.io/aria/#aria-description
    fn get_aria_description(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-description")
    }

    // https://w3c.github.io/aria/#aria-description
    fn set_aria_description(
        &self,
        ctx: &Ctx<'js>,
        value: Option<rquickjs::String<'js>>,
    ) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-description", value)
    }

    // https://w3c.github.io/aria/#aria-disabled
    fn get_aria_disabled(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-disabled")
    }

    // https://w3c.github.io/aria/#aria-disabled
    fn set_aria_disabled(
        &self,
        ctx: &Ctx<'js>,
        value: Option<rquickjs::String<'js>>,
    ) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-disabled", value)
    }

    // https://w3c.github.io/aria/#aria-expanded
    fn get_aria_expanded(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-expanded")
    }

    // https://w3c.github.io/aria/#aria-expanded
    fn set_aria_expanded(
        &self,
        ctx: &Ctx<'js>,
        value: Option<rquickjs::String<'js>>,
    ) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-expanded", value)
    }

    // https://w3c.github.io/aria/#aria-haspopup
    fn get_aria_has_popup(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-haspopup")
    }

    // https://w3c.github.io/aria/#aria-haspopup
    fn set_aria_has_popup(
        &self,
        ctx: &Ctx<'js>,
        value: Option<rquickjs::String<'js>>,
    ) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-haspopup", value)
    }

    // https://w3c.github.io/aria/#aria-hidden
    fn get_aria_hidden(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-hidden")
    }

    // https://w3c.github.io/aria/#aria-hidden
    fn set_aria_hidden(&self, ctx: &Ctx<'js>, value: Option<rquickjs::String<'js>>) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-hidden", value)
    }

    // https://w3c.github.io/aria/#aria-invalid
    fn get_aria_invalid(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-invalid")
    }

    // https://w3c.github.io/aria/#aria-invalid
    fn set_aria_invalid(&self, ctx: &Ctx<'js>, value: Option<rquickjs::String<'js>>) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-invalid", value)
    }

    // https://w3c.github.io/aria/#aria-keyshortcuts
    fn get_aria_key_shortcuts(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-keyshortcuts")
    }

    // https://w3c.github.io/aria/#aria-keyshortcuts
    fn set_aria_key_shortcuts(
        &self,
        ctx: &Ctx<'js>,
        value: Option<rquickjs::String<'js>>,
    ) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-keyshortcuts", value)
    }

    // https://w3c.github.io/aria/#aria-label
    fn get_aria_label(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-label")
    }

    // https://w3c.github.io/aria/#aria-label
    fn set_aria_label(&self, ctx: &Ctx<'js>, value: Option<rquickjs::String<'js>>) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-label", value)
    }

    // https://w3c.github.io/aria/#aria-level
    fn get_aria_level(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-level")
    }

    // https://w3c.github.io/aria/#aria-level
    fn set_aria_level(&self, ctx: &Ctx<'js>, value: Option<rquickjs::String<'js>>) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-level", value)
    }

    // https://w3c.github.io/aria/#aria-live
    fn get_aria_live(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-live")
    }

    // https://w3c.github.io/aria/#aria-live
    fn set_aria_live(&self, ctx: &Ctx<'js>, value: Option<rquickjs::String<'js>>) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-live", value)
    }

    // https://w3c.github.io/aria/#aria-modal
    fn get_aria_modal(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-modal")
    }

    // https://w3c.github.io/aria/#aria-modal
    fn set_aria_modal(&self, ctx: &Ctx<'js>, value: Option<rquickjs::String<'js>>) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-modal", value)
    }

    // https://w3c.github.io/aria/#aria-multiline
    fn get_aria_multi_line(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-multiline")
    }

    // https://w3c.github.io/aria/#aria-multiline
    fn set_aria_multi_line(
        &self,
        ctx: &Ctx<'js>,
        value: Option<rquickjs::String<'js>>,
    ) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-multiline", value)
    }

    // https://w3c.github.io/aria/#aria-multiselectable
    fn get_aria_multi_selectable(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-multiselectable")
    }

    // https://w3c.github.io/aria/#aria-multiselectable
    fn set_aria_multi_selectable(
        &self,
        ctx: &Ctx<'js>,
        value: Option<rquickjs::String<'js>>,
    ) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-multiselectable", value)
    }

    // https://w3c.github.io/aria/#aria-orientation
    fn get_aria_orientation(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-orientation")
    }

    // https://w3c.github.io/aria/#aria-orientation
    fn set_aria_orientation(
        &self,
        ctx: &Ctx<'js>,
        value: Option<rquickjs::String<'js>>,
    ) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-orientation", value)
    }

    // https://w3c.github.io/aria/#aria-placeholder
    fn get_aria_placeholder(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-placeholder")
    }

    // https://w3c.github.io/aria/#aria-placeholder
    fn set_aria_placeholder(
        &self,
        ctx: &Ctx<'js>,
        value: Option<rquickjs::String<'js>>,
    ) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-placeholder", value)
    }

    // https://w3c.github.io/aria/#aria-posinset
    fn get_aria_pos_in_set(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-posinset")
    }

    // https://w3c.github.io/aria/#aria-posinset
    fn set_aria_pos_in_set(
        &self,
        ctx: &Ctx<'js>,
        value: Option<rquickjs::String<'js>>,
    ) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-posinset", value)
    }

    // https://w3c.github.io/aria/#aria-pressed
    fn get_aria_pressed(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-pressed")
    }

    // https://w3c.github.io/aria/#aria-pressed
    fn set_aria_pressed(&self, ctx: &Ctx<'js>, value: Option<rquickjs::String<'js>>) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-pressed", value)
    }

    // https://w3c.github.io/aria/#aria-readonly
    fn get_aria_read_only(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-readonly")
    }

    // https://w3c.github.io/aria/#aria-readonly
    fn set_aria_read_only(
        &self,
        ctx: &Ctx<'js>,
        value: Option<rquickjs::String<'js>>,
    ) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-readonly", value)
    }

    // https://w3c.github.io/aria/#aria-relevant
    fn get_aria_relevant(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-relevant")
    }

    // https://w3c.github.io/aria/#aria-relevant
    fn set_aria_relevant(
        &self,
        ctx: &Ctx<'js>,
        value: Option<rquickjs::String<'js>>,
    ) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-relevant", value)
    }

    // https://w3c.github.io/aria/#aria-required
    fn get_aria_required(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-required")
    }

    // https://w3c.github.io/aria/#aria-required
    fn set_aria_required(
        &self,
        ctx: &Ctx<'js>,
        value: Option<rquickjs::String<'js>>,
    ) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-required", value)
    }

    // https://w3c.github.io/aria/#aria-roledescription
    fn get_aria_role_description(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-roledescription")
    }

    // https://w3c.github.io/aria/#aria-roledescription
    fn set_aria_role_description(
        &self,
        ctx: &Ctx<'js>,
        value: Option<rquickjs::String<'js>>,
    ) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-roledescription", value)
    }

    // https://w3c.github.io/aria/#aria-rowcount
    fn get_aria_row_count(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-rowcount")
    }

    // https://w3c.github.io/aria/#aria-rowcount
    fn set_aria_row_count(
        &self,
        ctx: &Ctx<'js>,
        value: Option<rquickjs::String<'js>>,
    ) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-rowcount", value)
    }

    // https://w3c.github.io/aria/#aria-rowindex
    fn get_aria_row_index(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-rowindex")
    }

    // https://w3c.github.io/aria/#aria-rowindex
    fn set_aria_row_index(
        &self,
        ctx: &Ctx<'js>,
        value: Option<rquickjs::String<'js>>,
    ) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-rowindex", value)
    }

    // https://w3c.github.io/aria/#aria-rowindextext
    fn get_aria_row_index_text(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-rowindextext")
    }

    // https://w3c.github.io/aria/#aria-rowindextext
    fn set_aria_row_index_text(
        &self,
        ctx: &Ctx<'js>,
        value: Option<rquickjs::String<'js>>,
    ) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-rowindextext", value)
    }

    // https://w3c.github.io/aria/#aria-rowspan
    fn get_aria_row_span(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-rowspan")
    }

    // https://w3c.github.io/aria/#aria-rowspan
    fn set_aria_row_span(
        &self,
        ctx: &Ctx<'js>,
        value: Option<rquickjs::String<'js>>,
    ) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-rowspan", value)
    }

    // https://w3c.github.io/aria/#aria-selected
    fn get_aria_selected(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-selected")
    }

    // https://w3c.github.io/aria/#aria-selected
    fn set_aria_selected(
        &self,
        ctx: &Ctx<'js>,
        value: Option<rquickjs::String<'js>>,
    ) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-selected", value)
    }

    // https://w3c.github.io/aria/#aria-setsize
    fn get_aria_set_size(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-setsize")
    }

    // https://w3c.github.io/aria/#aria-setsize
    fn set_aria_set_size(
        &self,
        ctx: &Ctx<'js>,
        value: Option<rquickjs::String<'js>>,
    ) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-setsize", value)
    }

    // https://w3c.github.io/aria/#aria-sort
    fn get_aria_sort(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-sort")
    }

    // https://w3c.github.io/aria/#aria-sort
    fn set_aria_sort(&self, ctx: &Ctx<'js>, value: Option<rquickjs::String<'js>>) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-sort", value)
    }

    // https://w3c.github.io/aria/#aria-valuemax
    fn get_aria_value_max(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-valuemax")
    }

    // https://w3c.github.io/aria/#aria-valuemax
    fn set_aria_value_max(
        &self,
        ctx: &Ctx<'js>,
        value: Option<rquickjs::String<'js>>,
    ) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-valuemax", value)
    }

    // https://w3c.github.io/aria/#aria-valuemin
    fn get_aria_value_min(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-valuemin")
    }

    // https://w3c.github.io/aria/#aria-valuemin
    fn set_aria_value_min(
        &self,
        ctx: &Ctx<'js>,
        value: Option<rquickjs::String<'js>>,
    ) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-valuemin", value)
    }

    // https://w3c.github.io/aria/#aria-valuenow
    fn get_aria_value_now(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-valuenow")
    }

    // https://w3c.github.io/aria/#aria-valuenow
    fn set_aria_value_now(
        &self,
        ctx: &Ctx<'js>,
        value: Option<rquickjs::String<'js>>,
    ) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-valuenow", value)
    }

    // https://w3c.github.io/aria/#aria-valuetext
    fn get_aria_value_text(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-valuetext")
    }

    // https://w3c.github.io/aria/#aria-valuetext
    fn set_aria_value_text(
        &self,
        ctx: &Ctx<'js>,
        value: Option<rquickjs::String<'js>>,
    ) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-valuetext", value)
    }
}

/// `ElementCSSInlineStyle` is a spec mixin included by `HTMLElement`,
/// `SVGElement`, and `MathMLElement`, so the one contract installs on all
/// three prototypes from the IDL includes
/// (<https://drafts.csswg.org/cssom/#the-elementcssinlinestyle-mixin>).
impl<'js> element_css_inline_style_generated::ElementCSSInlineStyle<'js> for JsNode {
    // https://drafts.csswg.org/cssom/#dom-elementcssinlinestyle-style
    fn get_style(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        self.style(ctx)
    }
}

// `ElementContentEditable` is a spec mixin included by `HTMLElement`: the
// enumerated `enterkeyhint`/`inputmode` are generated from the metadata table
// rather than written here
// (<https://html.spec.whatwg.org/multipage/interaction.html#attr-enterkeyhint>).

/// `HTMLHyperlinkElementUtils` is a spec mixin included by `HTMLAnchorElement`
/// and `HTMLAreaElement`, so the one contract installs on both prototypes
/// from the IDL includes
/// (<https://html.spec.whatwg.org/multipage/links.html#htmlhyperlinked>).
impl<'js> html_hyperlink_element_utils_generated::HTMLHyperlinkElementUtils<'js> for JsNode {
    // https://html.spec.whatwg.org/multipage/links.html#dom-hyperlink-href
    fn get_href(&self, ctx: &Ctx<'js>) -> Result<crate::dom_string::DomString> {
        host::reflect_url_string(ctx, self.handle.get().0, "href")
    }
}

impl JsNode {
    // The element's border box from the render pipeline's layout; zero when
    // the element generates no box (for example `display: none`).
    pub(super) fn get_bounding_client_rect<'js>(&self, ctx: Ctx<'js>) -> Result<Value<'js>> {
        match element_box_ctx(&ctx, self.handle.get().0)? {
            Some((left, top, width, height)) => {
                let (scroll_x, scroll_y) = viewport_offset(&ctx, self.handle.get().0)?;
                Ok(rect_object(&ctx, left - scroll_x, top - scroll_y, width, height)?.into_value())
            }
            None => Ok(rect_object(&ctx, 0.0, 0.0, 0.0, 0.0)?.into_value()),
        }
    }

    pub(super) fn get_client_rects<'js>(&self, ctx: Ctx<'js>) -> Result<Value<'js>> {
        let array = Array::new(ctx.clone())?;
        if let Some((left, top, width, height)) = element_box_ctx(&ctx, self.handle.get().0)? {
            let (scroll_x, scroll_y) = viewport_offset(&ctx, self.handle.get().0)?;
            array.set(
                0,
                rect_object(&ctx, left - scroll_x, top - scroll_y, width, height)?,
            )?;
        }
        Ok(array.into_value())
    }

    /// Scrolls this element's boxes into view with the requested alignment
    /// (<https://drafts.csswg.org/cssom-view/#dom-element-scrollintoview>).
    ///
    /// Blitz owns the scroll engine: it aligns the border box against the
    /// viewport using the block and inline positions and applies the requested
    /// behavior. Per-element scroll containers do not participate yet
    /// (`docs/limitations.md`).
    pub(super) fn scroll_into_view(
        &self,
        ctx: Ctx<'_>,
        behavior: blitz_dom::ScrollBehavior,
        block: blitz_dom::ScrollLogicalPosition,
        inline: blitz_dom::ScrollLogicalPosition,
    ) -> Result<()> {
        let world = world_for_node(&ctx, self.handle.get().0)?;
        let world = world.borrow();
        let Some(mut parsed) = world.document_mut(self.handle.get().0) else {
            return Ok(());
        };
        let id = self.handle.get().0;
        parsed.document.base.resolve(0.0);
        // A boxless element has nothing to bring into view
        // (<https://drafts.csswg.org/cssom-view/#dom-element-scrollintoview>).
        if !parsed
            .document
            .base
            .get_node(id.node)
            .is_some_and(blitz_dom::Node::has_boxes)
        {
            return Ok(());
        }
        parsed
            .document
            .base
            .scroll_into_view(id.node, behavior, block, inline);
        Ok(())
    }

    // https://drafts.csswg.org/cssom-view/#dom-element-scrollleft
    pub(super) fn scroll_left(&self, ctx: &Ctx<'_>) -> Result<f64> {
        self.scroll_axis(ctx, false)
    }

    pub(super) fn set_scroll_left(&self, ctx: &Ctx<'_>, value: f64) -> Result<()> {
        self.set_scroll_axis(ctx, value, false)
    }

    // https://drafts.csswg.org/cssom-view/#dom-element-scrolltop
    pub(super) fn scroll_top(&self, ctx: &Ctx<'_>) -> Result<f64> {
        self.scroll_axis(ctx, true)
    }

    pub(super) fn set_scroll_top(&self, ctx: &Ctx<'_>, value: f64) -> Result<()> {
        self.set_scroll_axis(ctx, value, true)
    }

    /// One axis of `scrollTop`/`scrollLeft`, including the quirks-mode body
    /// and root-element rules
    /// (<https://drafts.csswg.org/cssom-view/#dom-element-scrolltop>).
    fn scroll_axis(&self, ctx: &Ctx<'_>, vertical: bool) -> Result<f64> {
        let world = world(ctx)?;
        let world = world.borrow();
        let id = self.handle.get().0;
        // An element whose document is not the active document reports zero.
        if world.main_document_id() != Some(id.document) {
            return Ok(0.0);
        }
        let Some(mut parsed) = world.document_mut(id) else {
            return Ok(0.0);
        };
        parsed.document.base.resolve(0.0);
        Ok(super::scrolling::scroll_position(&parsed, id, vertical))
    }

    /// The `scrollTop`/`scrollLeft` setter: non-finite values become zero, and
    /// the other axis keeps its current position
    /// (<https://drafts.csswg.org/cssom-view/#dom-element-scrolltop>).
    fn set_scroll_axis(&self, ctx: &Ctx<'_>, value: f64, vertical: bool) -> Result<()> {
        let world = world(ctx)?;
        let world = world.borrow();
        let id = self.handle.get().0;
        if world.main_document_id() != Some(id.document) {
            return Ok(());
        }
        let Some(mut parsed) = world.document_mut(id) else {
            return Ok(());
        };
        parsed.document.base.resolve(0.0);
        let current = super::scrolling::scroll_position(&parsed, id, !vertical);
        let (x, y) = if vertical {
            (current, value)
        } else {
            (value, current)
        };
        super::scrolling::scroll_to(&mut parsed, id, x, y, blitz_dom::ScrollBehavior::Auto);
        Ok(())
    }

    /// Runs `read` over this wrapper's Blitz layout node after flushing
    /// pending layout; metrics are zero without one
    /// (<https://drafts.csswg.org/cssom-view/#extension-to-the-element-interface>).
    pub(super) fn layout_metric(
        &self,
        ctx: &Ctx<'_>,
        read: impl FnOnce(&blitz_dom::Node) -> f32,
    ) -> Result<f32> {
        let world = world(ctx)?;
        let world = world.borrow();
        let Some(mut parsed) = world.document_mut(self.handle.get().0) else {
            return Ok(0.0);
        };
        let base = &mut parsed.document.base;
        base.resolve(0.0);
        let Some(node) = base.get_node(self.handle.get().0.node) else {
            return Ok(0.0);
        };
        Ok(read(node))
    }

    /// Whether this wrapper is its document's root element.
    pub(super) fn is_document_element(&self, ctx: &Ctx<'_>) -> Result<bool> {
        let world = world(ctx)?;
        let world = world.borrow();
        let Some(parsed) = world.document(self.handle.get().0) else {
            return Ok(false);
        };
        Ok(
            document_first_child(&parsed, |data| matches!(data, NodeData::Element(_)))
                == Some(self.handle.get().0),
        )
    }

    // https://drafts.csswg.org/cssom-view/#dom-element-clientwidth
    pub(super) fn client_width(&self, ctx: &Ctx<'_>) -> Result<i32> {
        if self.is_document_element(ctx)? {
            let (width, _) = world(ctx)?.borrow().viewport_size.get();
            #[expect(
                clippy::cast_precision_loss,
                reason = "viewport sides are at most 4096, exactly representable"
            )]
            return Ok(cssom_round(width as f32));
        }
        Ok(cssom_round(
            self.layout_metric(ctx, blitz_dom::Node::client_width)?,
        ))
    }

    // https://drafts.csswg.org/cssom-view/#dom-element-clientheight
    pub(super) fn client_height(&self, ctx: &Ctx<'_>) -> Result<i32> {
        if self.is_document_element(ctx)? {
            let (_, height) = world(ctx)?.borrow().viewport_size.get();
            #[expect(
                clippy::cast_precision_loss,
                reason = "viewport sides are at most 4096, exactly representable"
            )]
            return Ok(cssom_round(height as f32));
        }
        Ok(cssom_round(
            self.layout_metric(ctx, blitz_dom::Node::client_height)?,
        ))
    }

    // https://drafts.csswg.org/cssom-view/#dom-element-scrollwidth
    pub(super) fn scroll_width(&self, ctx: &Ctx<'_>) -> Result<i32> {
        Ok(cssom_round(
            self.layout_metric(ctx, blitz_dom::Node::scroll_width)?,
        ))
    }

    // https://drafts.csswg.org/cssom-view/#dom-element-scrollheight
    pub(super) fn scroll_height(&self, ctx: &Ctx<'_>) -> Result<i32> {
        Ok(cssom_round(
            self.layout_metric(ctx, blitz_dom::Node::scroll_height)?,
        ))
    }

    // https://drafts.csswg.org/cssom-view/#dom-element-offsetwidth
    pub(super) fn offset_width(&self, ctx: &Ctx<'_>) -> Result<i32> {
        Ok(cssom_round(self.layout_metric(ctx, |node| {
            node.final_layout().size.width
        })?))
    }

    // https://drafts.csswg.org/cssom-view/#dom-element-offsetheight
    pub(super) fn offset_height(&self, ctx: &Ctx<'_>) -> Result<i32> {
        Ok(cssom_round(self.layout_metric(ctx, |node| {
            node.final_layout().size.height
        })?))
    }

    pub(super) fn get_attribute<'js>(
        &self,
        ctx: Ctx<'js>,
        name: WebIdlString,
    ) -> Result<Value<'js>> {
        if !valid_attribute_local_name(&name.0) {
            return Ok(Value::new_null(ctx));
        }
        // HTML elements match ASCII-lowercased
        // (<https://dom.spec.whatwg.org/#concept-element-attributes-get-by-name>).
        let local = attribute_local_name(&ctx, self.handle.get().0, &name.0);
        let world = world(&ctx)?;
        let found = {
            let borrowed = world.borrow();
            let Some(parsed) = borrowed.document(self.handle.get().0) else {
                return Ok(Value::new_null(ctx));
            };
            if let Some(exact) = parsed
                .document
                .attribute_units(self.handle.get().0.node, &local)
            {
                Some(exact.units().into_owned())
            } else {
                crate::dom::tree::attr_by_qualified_name(
                    &parsed.document.base,
                    self.handle.get().0.node,
                    &local,
                )
                .map(|value| value.encode_utf16().collect::<Vec<_>>())
            }
        };
        match found {
            Some(units) => Ok(rquickjs::String::from_utf16(ctx, &units)?.into_value()),
            None => Ok(Value::new_null(ctx)),
        }
    }

    pub(super) fn set_attribute(
        &self,
        ctx: Ctx<'_>,
        name: WebIdlString,
        value: WebIdlString,
    ) -> Result<()> {
        if !valid_attribute_local_name(&name.0) {
            return Err(throw_dom(
                &ctx,
                "InvalidCharacterError",
                "attribute name is not a valid attribute local name",
            ));
        }
        let local = attribute_local_name(&ctx, self.handle.get().0, &name.0);
        set_attribute_sync(&ctx, self.handle.get().0, &local, &value.0)
    }

    pub(super) fn id(&self, ctx: &Ctx<'_>) -> Result<String> {
        attribute_value(ctx, self.handle.get().0, "id")
    }

    pub(super) fn set_id(&self, ctx: &Ctx<'_>, value: WebIdlString) -> Result<()> {
        self.set_attribute(ctx.clone(), WebIdlString("id".into()), value)
    }

    // https://dom.spec.whatwg.org/#dom-element-tagname
    pub(super) fn tag_name(&self, ctx: &Ctx<'_>) -> Result<String> {
        let uppercase = document_is_html_content(ctx, self.handle.get().0);
        with_node_data(ctx, self.handle.get().0, |data| match data {
            Some(NodeData::Element(element)) => element_node_name(&element.name, uppercase),
            _ => String::new(),
        })
    }

    // https://dom.spec.whatwg.org/#dom-element-localname
    pub(super) fn local_name(&self, ctx: &Ctx<'_>) -> Result<String> {
        with_node_data(ctx, self.handle.get().0, |data| match data {
            Some(NodeData::Element(element)) => element.name.local.to_string(),
            _ => String::new(),
        })
    }

    // https://dom.spec.whatwg.org/#dom-element-prefix
    pub(super) fn prefix<'js>(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        let prefix = with_node_data(ctx, self.handle.get().0, |data| match data {
            Some(NodeData::Element(element)) => element
                .name
                .prefix
                .as_ref()
                .filter(|prefix| !prefix.is_empty())
                .map(ToString::to_string),
            _ => None,
        })?;
        match prefix {
            Some(prefix) => string_value(ctx, &prefix),
            None => Ok(Value::new_null(ctx.clone())),
        }
    }

    // https://dom.spec.whatwg.org/#dom-element-namespaceuri
    pub(super) fn namespace_uri<'js>(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        let namespace = with_node_data(ctx, self.handle.get().0, |data| match data {
            Some(NodeData::Element(element)) => {
                (!element.name.ns.as_str().is_empty()).then(|| element.name.ns.to_string())
            }
            _ => None,
        })?;
        match namespace {
            Some(namespace) => string_value(ctx, &namespace),
            None => Ok(Value::new_null(ctx.clone())),
        }
    }

    // https://dom.spec.whatwg.org/#dom-element-classname
    pub(super) fn class_name(&self, ctx: &Ctx<'_>) -> Result<String> {
        attribute_value(ctx, self.handle.get().0, "class")
    }

    pub(super) fn set_class_name(&self, ctx: &Ctx<'_>, value: WebIdlString) -> Result<()> {
        set_attribute_sync(ctx, self.handle.get().0, "class", &value.0)
    }

    // https://dom.spec.whatwg.org/#dom-element-classlist
    pub(super) fn class_list<'js>(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        let world_rc = world(ctx)?;
        if let Some(saved) = world_rc
            .borrow()
            .same_object(self.handle.get().0, SameObject::ClassList)
            && let Some(value) = deref_weak(ctx, saved)?
        {
            return Ok(value);
        }
        let class = host::instance_for_node(
            ctx,
            self.handle.get().0,
            JsTokenList {
                element: self.handle.get(),
                attribute: markup5ever::local_name!("class"),
            },
        )?;
        let value = Class::into_value(class);
        let weak = make_weak(ctx, value.clone())?;
        world_rc.borrow_mut().intern_same_object(
            self.handle.get().0,
            SameObject::ClassList,
            Persistent::save(ctx, weak),
        );
        Ok(value)
    }

    // https://html.spec.whatwg.org/multipage/dom.html#concept-domstringmap-pairs
    pub(super) fn dataset<'js>(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        let world_rc = world(ctx)?;
        if let Some(saved) = world_rc
            .borrow()
            .same_object(self.handle.get().0, SameObject::Dataset)
            && let Some(value) = deref_weak(ctx, saved)?
        {
            return Ok(value);
        }
        let factory: Function = crate::js::bridge::object(ctx)?.get("__tbMakeDataset")?;
        let element = wrap_node(ctx, self.handle.get().0)?;
        let value: Value = factory.call((element,))?;
        let weak = make_weak(ctx, value.clone())?;
        world_rc.borrow_mut().intern_same_object(
            self.handle.get().0,
            SameObject::Dataset,
            Persistent::save(ctx, weak),
        );
        Ok(value)
    }

    // https://dom.spec.whatwg.org/#dom-element-hasattribute
    pub(super) fn has_attribute(&self, ctx: Ctx<'_>, name: WebIdlString) -> Result<bool> {
        if !valid_attribute_local_name(&name.0) {
            return Ok(false);
        }
        // HTML elements match ASCII-lowercased, like `getAttribute`.
        let local = attribute_local_name(&ctx, self.handle.get().0, &name.0);
        let world = world(&ctx)?;
        let parsed = world.borrow();
        let Some(parsed) = parsed.document(self.handle.get().0) else {
            return Ok(false);
        };
        Ok(crate::dom::tree::attr_by_qualified_name(
            &parsed.document.base,
            self.handle.get().0.node,
            &local,
        )
        .is_some())
    }

    // https://dom.spec.whatwg.org/#dom-element-getattributens
    pub(super) fn get_attribute_ns<'js>(
        &self,
        ctx: Ctx<'js>,
        namespace: OptString,
        local: WebIdlString,
    ) -> Result<Value<'js>> {
        let namespace = namespace.0.unwrap_or_default();
        let world = world(&ctx)?;
        let found = world
            .borrow()
            .document(self.handle.get().0)
            .and_then(|parsed| {
                attr_ns_value(
                    &parsed.document.base,
                    self.handle.get().0.node,
                    &namespace,
                    &local.0,
                )
            });
        match found {
            Some(value) => string_value(&ctx, &value),
            None => Ok(Value::new_null(ctx)),
        }
    }

    // https://dom.spec.whatwg.org/#dom-element-hasattributens
    pub(super) fn has_attribute_ns(
        &self,
        ctx: Ctx<'_>,
        namespace: OptString,
        local: WebIdlString,
    ) -> Result<bool> {
        let namespace = namespace.0.unwrap_or_default();
        let world = world(&ctx)?;
        let parsed = world.borrow();
        let Some(parsed) = parsed.document(self.handle.get().0) else {
            return Ok(false);
        };
        Ok(attr_ns_value(
            &parsed.document.base,
            self.handle.get().0.node,
            &namespace,
            &local.0,
        )
        .is_some())
    }

    // https://dom.spec.whatwg.org/#dom-element-setattributens
    pub(super) fn set_attribute_ns(
        &self,
        ctx: Ctx<'_>,
        namespace: OptString,
        qualified: WebIdlString,
        value: WebIdlString,
    ) -> Result<()> {
        // setAttributeNS never lowercases its name, even on HTML elements
        // (<https://dom.spec.whatwg.org/#dom-element-setattributens>).
        let name = validate_and_extract(
            &ctx,
            namespace.0.as_deref(),
            &qualified.0,
            NodeContext::Attribute,
        )?;
        let namespace = name.ns.to_string();
        let local = name.local.to_string();
        let previous_value;
        {
            let world = world(&ctx)?;
            let world = world.borrow();
            let Some(mut parsed) = world.document_mut(self.handle.get().0) else {
                return Err(Exception::throw_type(&ctx, "no document"));
            };
            let old_value = attr_ns_value(
                &parsed.document.base,
                self.handle.get().0.node,
                &namespace,
                &local,
            );
            previous_value = old_value.clone();
            // Reuse the stored qualified name when the attribute is already
            // present, so the write replaces instead of duplicating it.
            let stored = parsed
                .document
                .base
                .get_node(self.handle.get().0.node)
                .and_then(|node| node.data.downcast_element())
                .and_then(|element| {
                    element
                        .attrs
                        .iter()
                        .find(|attribute| {
                            attribute.name.ns.as_str() == namespace
                                && attribute.name.local.as_str() == local
                        })
                        .map(|attribute| attribute.name.clone())
                })
                .unwrap_or(name);
            parsed
                .document
                .base
                .mutate()
                .set_attribute(self.handle.get().0.node, stored, &value.0);
            parsed.document.record(JournalEntry::Attributes {
                target: self.handle.get().0,
                // attributeName is the attribute's local name.
                // https://dom.spec.whatwg.org/#handle-attribute-changes
                name: local.clone(),
                namespace: namespace.clone(),
                old_value,
            });
        }
        touch_attr(&ctx, self.handle.get().0, &namespace, &local, &value.0)?;
        if namespace.is_empty() {
            after_attribute_change(&ctx, self.handle.get().0, &local, previous_value.as_deref())?;
        }
        schedule_mutation_delivery(&ctx)
    }

    // https://dom.spec.whatwg.org/#dom-element-removeattribute
    pub(super) fn remove_attribute(&self, ctx: Ctx<'_>, name: WebIdlString) -> Result<()> {
        if !valid_attribute_local_name(&name.0) {
            return Ok(());
        }
        let local = attribute_local_name(&ctx, self.handle.get().0, &name.0);
        remove_attribute_sync(&ctx, self.handle.get().0, "", &local, false)
    }

    // https://dom.spec.whatwg.org/#dom-element-removeattributens
    pub(super) fn remove_attribute_ns(
        &self,
        ctx: Ctx<'_>,
        namespace: OptString,
        local: WebIdlString,
    ) -> Result<()> {
        let namespace = namespace.0.unwrap_or_default();
        remove_attribute_sync(&ctx, self.handle.get().0, &namespace, &local.0, true)
    }

    // https://dom.spec.whatwg.org/#dom-element-getattributenames
    pub(super) fn get_attribute_names(&self, ctx: Ctx<'_>) -> Result<Vec<String>> {
        let world = world(&ctx)?;
        Ok(world
            .borrow()
            .document(self.handle.get().0)
            .and_then(|parsed| {
                parsed
                    .document
                    .base
                    .get_node(self.handle.get().0.node)
                    .and_then(|node| node.data.downcast_element())
                    .map(|element| {
                        element
                            .attrs
                            .iter()
                            .map(|attribute| qualified_name(&attribute.name))
                            .collect::<Vec<String>>()
                    })
            })
            .unwrap_or_default())
    }

    // https://dom.spec.whatwg.org/#dom-element-attributes
    pub(super) fn attributes<'js>(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        let world_rc = world(ctx)?;
        if let Some(saved) = world_rc
            .borrow()
            .same_object(self.handle.get().0, SameObject::Attributes)
            && let Some(value) = deref_weak(ctx, saved)?
        {
            return Ok(value);
        }
        let class = host::instance_for_node(
            ctx,
            self.handle.get().0,
            JsNamedNodeMap {
                element: self.handle.get(),
            },
        )?;
        let value = Class::into_value(class);
        let weak = make_weak(ctx, value.clone())?;
        world_rc.borrow_mut().intern_same_object(
            self.handle.get().0,
            SameObject::Attributes,
            Persistent::save(ctx, weak),
        );
        Ok(value)
    }

    // https://dom.spec.whatwg.org/#dom-element-hasattributes
    pub(super) fn has_attributes(&self, ctx: Ctx<'_>) -> Result<bool> {
        let world = world(&ctx)?;
        let parsed = world.borrow();
        let Some(parsed) = parsed.document(self.handle.get().0) else {
            return Ok(false);
        };
        Ok(parsed
            .document
            .base
            .get_node(self.handle.get().0.node)
            .and_then(|node| node.data.downcast_element())
            .is_some_and(|element| !element.attrs.is_empty()))
    }

    // https://dom.spec.whatwg.org/#dom-element-getattributenode
    pub(super) fn get_attribute_node<'js>(
        &self,
        ctx: Ctx<'js>,
        name: WebIdlString,
    ) -> Result<Value<'js>> {
        if !valid_attribute_local_name(&name.0) {
            return Ok(Value::new_null(ctx));
        }
        let name = attribute_local_name(&ctx, self.handle.get().0, &name.0);
        match attached_attr_id(&ctx, self.handle.get().0, "", &name)? {
            Some(id) => attr_wrapper(&ctx, id),
            None => Ok(Value::new_null(ctx)),
        }
    }

    // https://dom.spec.whatwg.org/#dom-element-getattributenodens
    pub(super) fn get_attribute_node_ns<'js>(
        &self,
        ctx: Ctx<'js>,
        namespace: OptString,
        local: WebIdlString,
    ) -> Result<Value<'js>> {
        let namespace = namespace.0.unwrap_or_default();
        match attached_attr_id(&ctx, self.handle.get().0, &namespace, &local.0)? {
            Some(id) => attr_wrapper(&ctx, id),
            None => Ok(Value::new_null(ctx)),
        }
    }

    // https://dom.spec.whatwg.org/#dom-element-setattributenode
    pub(super) fn set_attribute_node<'js>(
        &self,
        ctx: Ctx<'js>,
        attr: AttrArgument<'js>,
    ) -> Result<Value<'js>> {
        set_attribute_node(&ctx, self.handle.get().0, &attr.into_value())
    }

    // https://dom.spec.whatwg.org/#dom-element-setattributenodens
    pub(super) fn set_attribute_node_ns<'js>(
        &self,
        ctx: Ctx<'js>,
        attr: AttrArgument<'js>,
    ) -> Result<Value<'js>> {
        set_attribute_node(&ctx, self.handle.get().0, &attr.into_value())
    }

    // https://dom.spec.whatwg.org/#dom-element-removeattributenode
    pub(super) fn remove_attribute_node<'js>(
        &self,
        ctx: Ctx<'js>,
        attr: AttrArgument<'js>,
    ) -> Result<Value<'js>> {
        let (id, scope) = {
            let attr = attr.borrow();
            (attr.id, attr.scope.0)
        };
        let state = attr_state(&ctx, scope, id)?;
        let attached_here = attr_owner(&ctx, scope, id) == Some(self.handle.get().0) && {
            let registry = world(&ctx)?.borrow().registry();
            registry.borrow().attributes.attached(
                self.handle.get().0,
                &state.namespace,
                &state.local,
            ) == Some(id)
        };
        if !attached_here {
            return Err(throw_dom(
                &ctx,
                "NotFoundError",
                "attribute is not attached to this element",
            ));
        }
        remove_attribute_sync(
            &ctx,
            self.handle.get().0,
            &state.namespace,
            &state.local,
            true,
        )?;
        Ok(attr.into_value())
    }

    // https://dom.spec.whatwg.org/#dom-element-toggleattribute
    pub(super) fn toggle_attribute(
        &self,
        ctx: Ctx<'_>,
        name: WebIdlString,
        force: Option<bool>,
    ) -> Result<bool> {
        if !valid_attribute_local_name(&name.0) {
            return Err(throw_dom(
                &ctx,
                "InvalidCharacterError",
                "attribute name is not a valid attribute local name",
            ));
        }
        let local = attribute_local_name(&ctx, self.handle.get().0, &name.0);
        let exists = {
            let world = world(&ctx)?;
            let world = world.borrow();
            let Some(parsed) = world.document(self.handle.get().0) else {
                return Err(Exception::throw_type(&ctx, "no document"));
            };
            crate::dom::tree::attr_by_qualified_name(
                &parsed.document.base,
                self.handle.get().0.node,
                &local,
            )
            .is_some()
        };
        let should_exist = force.unwrap_or(!exists);
        if should_exist == exists {
            return Ok(should_exist);
        }
        if should_exist {
            set_attribute_sync(&ctx, self.handle.get().0, &local, "")?;
        } else {
            remove_attribute_sync(&ctx, self.handle.get().0, "", &local, false)?;
        }
        Ok(should_exist)
    }

    // https://dom.spec.whatwg.org/#dom-element-getelementsbytagnamens
    pub(super) fn get_elements_by_tag_name_ns<'js>(
        &self,
        ctx: Ctx<'js>,
        namespace: OptString,
        local: WebIdlString,
    ) -> Result<Value<'js>> {
        // The namespace variant matches the local name exactly, in HTML
        // documents too: the lowercasing rule belongs to
        // `getElementsByTagName` alone
        // (<https://dom.spec.whatwg.org/#concept-getelementsbytagnamens>).
        live_collection(
            &ctx,
            self.handle.get().0,
            CollectionKind::ElementsByTagNs {
                namespace: namespace.0.unwrap_or_default(),
                local: local.0,
            },
            Some("HTMLCollection"),
        )
    }

    // https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-element-innerhtml
    pub(super) fn inner_html<'js>(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        let container = self.handle.get().0;
        let owner = world_for_node(ctx, container)?;
        let (container, html) = {
            let world = owner.borrow();
            let Some(parsed) = world.document(container) else {
                return Err(Exception::throw_type(ctx, "no document"));
            };
            let base = &parsed.document.base;
            let Some(node) = base.get_node(container.node) else {
                return Err(Exception::throw_type(ctx, "stale node"));
            };
            // `innerHTML` belongs to `Element` and `ShadowRoot`, not to the
            // other fragment kinds
            // (<https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-element-innerhtml>).
            if node.data.downcast_element().is_none()
                && !matches!(node.data, NodeData::ShadowRoot { .. })
            {
                return Err(Exception::throw_type(ctx, "innerHTML requires an element"));
            }
            // innerHTML on a template reads its template contents, not its
            // (empty) children. After `content` is read, those nodes live in
            // the inert template-contents document
            // (<https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-element-innerhtml>,
            // <https://html.spec.whatwg.org/multipage/scripting.html#template-contents>).
            (
                template_contents_target(&parsed.document, container),
                parsed.content_type == "text/html",
            )
        };
        drop(owner);
        let markup = world(ctx)?
            .borrow()
            .with_document(container, |parsed| {
                if html {
                    serialization::serialize_html_children(&parsed.document, container.node)
                } else {
                    serialization::serialize_xml_children(&parsed.document, container.node)
                }
            })
            .unwrap_or_default();
        dom_string(ctx, &markup)
    }

    // https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-element-outerhtml
    pub(super) fn outer_html<'js>(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        let world = world(ctx)?;
        if world.borrow().document(self.handle.get().0).is_none() {
            return Err(Exception::throw_type(ctx, "no document"));
        }
        let markup = crate::document::dom::element_outer_html(&world.borrow(), self.handle.get().0)
            .ok_or_else(|| Exception::throw_type(ctx, "outerHTML requires an element"))?;
        dom_string(ctx, &markup)
    }

    // https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-element-innerhtml
    pub(super) fn set_inner_html(&self, ctx: &Ctx<'_>, value: LegacyNullString) -> Result<()> {
        // Adoption by copy leaves a stale handle behind; parse into the live
        // container (https://dom.spec.whatwg.org/#concept-node-adopt).
        let element = super::super::realm_registry(ctx)?
            .borrow_mut()
            .live_node_id(self.handle.get().0);
        // A shadow root's fragment parsing context is its host
        // (<https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-shadowroot-innerhtml>).
        let context = super::document::inner_html_fragment_context(ctx, element)?;

        let snapshots = {
            let (base_url, font_ctx) = fragment_base_url(ctx, element);
            parse_html_fragment_snapshots(&value.0, &context, &base_url, font_ctx)
        };

        let container = element;
        // innerHTML on a template replaces its template contents, not its
        // (empty) children
        // (<https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-element-innerhtml>).
        let container = world_for_node(ctx, container)?
            .borrow()
            .document(container)
            .map_or(container, |parsed| {
                template_contents_target(&parsed.document, container)
            });
        let owner = world_for_node(ctx, container)?;
        let world = owner.borrow();
        let Some(mut parsed) = world.document_mut(container) else {
            return Err(Exception::throw_type(ctx, "no document"));
        };
        let replacement =
            materialize_children(&mut parsed.document, container.document, &snapshots)
                .map_err(|err| throw_dom_error(ctx, err))?;
        let added = fragment_children(&parsed.document, replacement);
        let iframes = replace_all_journaled(&mut parsed, container, added.clone());
        let controls = crate::dom::forms::descendant_controls(
            &parsed.document.base,
            container.document,
            container.node,
        );
        drop(parsed);
        world.destroy_child_navigables(&iframes);
        drop(world);
        fixup_controls_on_insert(ctx, container.document, &controls)?;
        register_inserted_iframes(ctx, &added)?;
        upgrade_parsed_tree(ctx, container)?;
        schedule_mutation_delivery(ctx)
    }

    // https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-element-insertadjacenthtml
    pub(super) fn insert_adjacent_html(
        &self,
        ctx: Ctx<'_>,
        position: WebIdlString,
        text: WebIdlString,
    ) -> Result<()> {
        let position = position.0.to_ascii_lowercase();
        if !matches!(
            position.as_str(),
            "beforebegin" | "afterbegin" | "beforeend" | "afterend"
        ) {
            return Err(throw_dom(&ctx, "SyntaxError", "invalid position"));
        }
        // Adoption by copy leaves a stale handle behind; parse against the
        // live container (https://dom.spec.whatwg.org/#concept-node-adopt).
        let element = self.handle.get().0;
        let context = with_node_data(&ctx, element, |data| match data {
            Some(NodeData::Element(element)) => Some(html_fragment_context(&element.name)),
            _ => None,
        })?
        .ok_or_else(|| Exception::throw_type(&ctx, "insertAdjacentHTML requires an element"))?;
        let snapshots = {
            let (base_url, font_ctx) = fragment_base_url(&ctx, element);
            parse_html_fragment_snapshots(&text.0, &context, &base_url, font_ctx)
        };
        let world = world(&ctx)?;
        let world = world.borrow();
        let Some(mut parsed) = world.document_mut(element) else {
            return Err(Exception::throw_type(&ctx, "no document"));
        };
        let needs_parent = matches!(position.as_str(), "beforebegin" | "afterend");
        let parent = parsed
            .document
            .base
            .get_node(element.node)
            .and_then(|node| node.parent)
            .map(|node| NodeId {
                document: element.document,
                node,
            });
        if needs_parent && parent.is_none() {
            return Err(throw_dom(
                &ctx,
                "NoModificationAllowedError",
                "element has no parent",
            ));
        }
        let fragment = materialize_children(&mut parsed.document, element.document, &snapshots)
            .map_err(|err| throw_dom_error(&ctx, err))?;
        let moved = fragment_children(&parsed.document, fragment);
        match position.as_str() {
            "beforebegin" => {
                let parent = parent.ok_or_else(|| {
                    throw_dom(&ctx, "NoModificationAllowedError", "element has no parent")
                })?;
                for child in &moved {
                    place_journaled(&mut parsed, parent, *child, Some(element));
                }
            }
            _ => place_adjacent_rest(&ctx, &mut parsed, position.as_str(), element, moved.clone())?,
        }
        let controls: Vec<_> = moved
            .iter()
            .flat_map(|root| {
                crate::dom::forms::descendant_controls(
                    &parsed.document.base,
                    root.document,
                    root.node,
                )
            })
            .collect();
        drop(parsed);
        drop(world);
        fixup_controls_on_insert(&ctx, element.document, &controls)?;
        register_inserted_iframes(&ctx, &moved)?;
        // Custom elements in parsed markup upgrade, like the innerHTML and
        // outerHTML paths below.
        upgrade_parsed_tree(
            &ctx,
            if needs_parent {
                parent.ok_or_else(|| {
                    throw_dom(&ctx, "NoModificationAllowedError", "element has no parent")
                })?
            } else {
                element
            },
        )?;
        schedule_mutation_delivery(&ctx)
    }

    // https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-element-outerhtml
    pub(super) fn set_outer_html(&self, ctx: &Ctx<'_>, value: LegacyNullString) -> Result<()> {
        // Adoption by copy leaves a stale handle behind; replace the live
        // element (https://dom.spec.whatwg.org/#concept-node-adopt).
        let element = self.handle.get().0;
        let world_rc = world(ctx)?;
        let (parent, context) = {
            let world = world_rc.borrow();
            let Some(parsed) = world.document(element) else {
                return Err(Exception::throw_type(ctx, "no document"));
            };
            let base = &parsed.document.base;
            let Some(parent) = base
                .get_node(element.node)
                .and_then(|node| node.parent)
                .map(|node| NodeId {
                    document: element.document,
                    node,
                })
            else {
                // A parentless element has nothing to replace.
                return Ok(());
            };
            if base.root_node().id == parent.node {
                return Err(throw_dom(
                    ctx,
                    "NoModificationAllowedError",
                    "the parent of the element is a Document",
                ));
            }
            // A DocumentFragment has no parsing context of its own; a
            // body element stands in
            // (<https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-element-outerhtml>).
            let context = base
                .get_node(parent.node)
                .and_then(|node| node.data.downcast_element())
                .map_or_else(
                    || "body".to_owned(),
                    |element| html_fragment_context(&element.name),
                );
            (parent, context)
        };
        let snapshots = {
            let (base_url, font_ctx) = fragment_base_url(ctx, element);
            parse_html_fragment_snapshots(&value.0, &context, &base_url, font_ctx)
        };

        let world = world_rc.borrow();
        let Some(mut parsed) = world.document_mut(element) else {
            return Err(Exception::throw_type(ctx, "no document"));
        };
        let replacement = materialize_children(&mut parsed.document, element.document, &snapshots)
            .map_err(|err| throw_dom_error(ctx, err))?;
        drop(parsed);
        drop(world);
        let replacement = super::core::replace_target(ctx, parent, replacement)?;
        let world = world_rc.borrow();
        let Some(mut parsed) = world.document_mut(element) else {
            return Err(Exception::throw_type(ctx, "no document"));
        };
        let target = element;
        let added = replace_parsed(&mut parsed, parent, target, replacement);
        drop(parsed);
        drop(world);
        register_inserted_iframes(ctx, &added)?;
        upgrade_parsed_tree(ctx, parent)?;
        schedule_mutation_delivery(ctx)
    }

    pub(super) fn style<'js>(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        let world_rc = world(ctx)?;
        if let Some(saved) = world_rc
            .borrow()
            .same_object(self.handle.get().0, SameObject::Style)
            && let Some(value) = deref_weak(ctx, saved)?
        {
            return Ok(value);
        }
        let factory: Function = crate::js::bridge::object(ctx)?.get("__tbMakeStyle")?;
        let element = wrap_node(ctx, self.handle.get().0)?;
        let value: Value = factory.call((element,))?;
        let weak = make_weak(ctx, value.clone())?;
        world_rc.borrow_mut().intern_same_object(
            self.handle.get().0,
            SameObject::Style,
            Persistent::save(ctx, weak),
        );
        Ok(value)
    }
}
/// The value of the `(namespace, local)` attribute on `id`, if present.
pub(super) fn attr_ns_value(
    base: &blitz_dom::BaseDocument,
    id: BlitzId,
    namespace: &str,
    local: &str,
) -> Option<String> {
    base.get_node(id)
        .and_then(|node| node.data.downcast_element())
        .and_then(|element| {
            element
                .attrs
                .iter()
                .find(|attribute| {
                    attribute.name.ns.as_str() == namespace
                        && attribute.name.local.as_str() == local
                })
                .map(|attribute| attribute.value.clone())
        })
}

/// [Insert adjacent](https://dom.spec.whatwg.org/#concept-element-insert-adjacent).
///
/// Returns the inserted node, or `None` when `beforebegin` / `afterend` has
/// no parent.
pub(super) fn insert_adjacent(
    ctx: &Ctx<'_>,
    element: NodeId,
    where_: &str,
    node: NodeId,
) -> Result<Option<NodeId>> {
    let position = where_.to_ascii_lowercase();
    let (parent, reference) = match position.as_str() {
        "beforebegin" => {
            let Some(parent) = tree_parent(ctx, element)? else {
                return Ok(None);
            };
            (parent, Some(element))
        }
        "afterbegin" => {
            let world = world(ctx)?;
            let world = world.borrow();
            let Some(parsed) = world.document(element) else {
                return Err(Exception::throw_type(ctx, "no document"));
            };
            let first = parsed
                .document
                .base
                .get_node(element.node)
                .and_then(|node| node.children.first().copied())
                .map(|node| NodeId {
                    document: element.document,
                    node,
                });
            (element, first)
        }
        "beforeend" => (element, None),
        "afterend" => {
            let Some(parent) = tree_parent(ctx, element)? else {
                return Ok(None);
            };
            let world = world(ctx)?;
            let world = world.borrow();
            let Some(parsed) = world.document(element) else {
                return Err(Exception::throw_type(ctx, "no document"));
            };
            let next = sibling(&parsed.document.base, element.node, true).map(|node| NodeId {
                document: element.document,
                node,
            });
            (parent, next)
        }
        _ => {
            return Err(throw_dom(ctx, "SyntaxError", "invalid position"));
        }
    };
    let (node, reference) = insertion_tree_nodes(
        ctx,
        parent,
        NodeReference::Tree(node),
        reference.map(NodeReference::Tree),
    )?;
    let node = adopt_node(ctx, parent, node)?;
    insert_tree_node(ctx, parent, node, reference)?;
    Ok(Some(node))
}

/// The context object of an `Element` operation.
pub(super) fn require_context_element(ctx: &Ctx<'_>, id: NodeId) -> Result<NodeId> {
    let element = {
        let world = world(ctx)?;
        let world = world.borrow();
        world.document(id).is_some_and(|parsed| {
            parsed
                .document
                .base
                .get_node(id.node)
                .is_some_and(|node| node.data.downcast_element().is_some())
        })
    };
    if element {
        Ok(id)
    } else {
        Err(Exception::throw_type(ctx, "argument is not an Element"))
    }
}

/// An `Element` argument
/// (<https://webidl.spec.whatwg.org/#es-interface>).
pub(super) fn require_element_argument<'js>(ctx: &Ctx<'js>, value: &Value<'js>) -> Result<NodeId> {
    let Some(id) = host_node_id(ctx, value) else {
        return Err(Exception::throw_type(ctx, "argument is not an Element"));
    };
    require_context_element(ctx, id)
}

/// `id`'s parent, or `None` when it is parentless.
pub(super) fn tree_parent(ctx: &Ctx<'_>, id: NodeId) -> Result<Option<NodeId>> {
    let world = world(ctx)?;
    let world = world.borrow();
    let Some(parsed) = world.document(id) else {
        return Err(Exception::throw_type(ctx, "no document"));
    };
    Ok(parsed
        .document
        .base
        .get_node(id.node)
        .and_then(|node| node.parent)
        .map(|node| NodeId {
            document: id.document,
            node,
        }))
}

/// Node ids among `(Node or DOMString)` arguments, for sibling exclusion
/// (<https://dom.spec.whatwg.org/#dom-childnode-before>).
pub(super) fn argument_node_ids(nodes: &[NodeOrString<'_>]) -> HashSet<BlitzId> {
    nodes
        .iter()
        .filter_map(|node| match node {
            NodeOrString::Node(reference) => reference.tree().map(|id| id.node),
            NodeOrString::String(_) => None,
        })
        .collect()
}

/// The nearest sibling of `id` in `forward` direction that is not in
/// `excluded`.
pub(super) fn sibling_not_in(
    base: &blitz_dom::BaseDocument,
    id: BlitzId,
    forward: bool,
    excluded: &HashSet<BlitzId>,
) -> Option<BlitzId> {
    let node = base.get_node(id)?;
    let parent = node.parent?;
    let siblings = &base.get_node(parent)?.children;
    let position = siblings.iter().position(|sibling| *sibling == id)?;
    if forward {
        siblings[position + 1..]
            .iter()
            .copied()
            .find(|sibling| !excluded.contains(sibling))
    } else {
        siblings[..position]
            .iter()
            .copied()
            .rev()
            .find(|sibling| !excluded.contains(sibling))
    }
}

/// Moves focus to `id` when it is focusable
/// (<https://html.spec.whatwg.org/multipage/interaction.html#dom-focus>).
pub(super) fn focus_element(ctx: &Ctx<'_>, id: NodeId) -> Result<()> {
    if is_focusable(ctx, id)? {
        focus_node(ctx, id)?;
    }
    Ok(())
}

/// One content attribute's raw value, when the attribute is present.
pub(super) fn content_attr(ctx: &Ctx<'_>, id: NodeId, name: &str) -> Option<String> {
    let owner = world_for_node(ctx, id).ok()?;
    let borrowed = owner.borrow();
    borrowed.document(id).and_then(|parsed| {
        crate::dom::tree::attr(&parsed.document.base, id.node, name).map(str::to_owned)
    })
}

/// Whether `data` is an element node, for [`document_first_child`] searches.
pub(super) fn is_element_data(data: &NodeData) -> bool {
    matches!(data, NodeData::Element(_))
}

/// `unsigned long` reflection: a non-negative integer within `long` range,
/// otherwise the default
/// (<https://html.spec.whatwg.org/multipage/common-dom-interfaces.html#reflecting-content-attributes-in-idl-attributes>).
pub(super) fn reflect_ulong(raw: Option<&str>, default: u32) -> u32 {
    raw.and_then(crate::dom::parse::parse_integer)
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or(default)
}

/// Clamped `unsigned long` reflection (used by `colSpan`, `rowSpan`, `span`):
/// full-range non-negative parsing, then clamping; parse failure reads the
/// default. Unlike plain reflection the range is not capped at 2^31 - 1
/// before clamping, so `4294967296` clamps to the edge rather than the
/// default
/// (<https://html.spec.whatwg.org/multipage/common-dom-interfaces.html#reflecting-content-attributes-in-idl-attributes>).
pub(super) fn reflect_clamped_ulong(raw: Option<&str>, default: u32, min: u32, max: u32) -> u32 {
    let Some(text) = raw else {
        return default;
    };
    let mut chars = text.chars().peekable();
    while crate::dom::parse::is_html_whitespace(chars.peek()) {
        chars.next();
    }
    let mut negative = false;
    if chars.peek() == Some(&'+') {
        chars.next();
    } else if chars.peek() == Some(&'-') {
        negative = true;
        chars.next();
    }
    let mut digits = 0_u32;
    let mut result: u64 = 0;
    while let Some(digit) = chars.peek().and_then(|c| c.to_digit(10)) {
        result = result.saturating_mul(10).saturating_add(u64::from(digit));
        digits += 1;
        chars.next();
    }
    if digits == 0 {
        return default;
    }
    if result == 0 {
        return 0_u32.clamp(min, max);
    }
    if negative {
        return default;
    }
    u32::try_from(result).map_or(max, |value| value.clamp(min, max))
}

/// `double` reflection: a floating-point value or the default
/// (<https://html.spec.whatwg.org/multipage/common-dom-interfaces.html#reflecting-content-attributes-in-idl-attributes>).
pub(super) fn reflect_double(raw: Option<&str>, default: f64) -> f64 {
    raw.and_then(crate::dom::parse::parse_double)
        .unwrap_or(default)
}

/// Limited `double` reflection (used by `progress.max`): a positive
/// floating-point value or the default
/// (<https://html.spec.whatwg.org/multipage/common-dom-interfaces.html#reflecting-content-attributes-in-idl-attributes>).
pub(super) fn reflect_limited_double(raw: Option<&str>, default: f64) -> f64 {
    match raw.and_then(crate::dom::parse::parse_double) {
        Some(value) if value > 0.0 => value,
        _ => default,
    }
}

/// Whether `node` is the first `summary` element child of a `details`
/// parent: only that one is focusable
/// (<https://html.spec.whatwg.org/multipage/interaction.html#focusable-area>).
pub(super) fn is_first_summary_child(
    base: &blitz_dom::BaseDocument,
    parent: BlitzId,
    node: BlitzId,
) -> bool {
    if !crate::dom::tree::is_html_tag(base.get_node(parent).map(|tree| &tree.data), "details") {
        return false;
    }
    base.get_node(parent).is_some_and(|parent| {
        parent.children.iter().find_map(|child| {
            base.get_node(*child).and_then(|tree| {
                tree.data.downcast_element().and_then(|element| {
                    (element.name.ns == html_namespace()
                        && element.name.local.as_str() == "summary")
                        .then_some(*child)
                })
            })
        }) == Some(node)
    })
}

/// The computed `writingSuggestions` value for `node`: the nearest
/// ancestor-or-self carrying the attribute decides, ASCII
/// case-insensitively (`false` reads "false", any other present value
/// reads "true"); with no such ancestor the value is "true"
/// (<https://html.spec.whatwg.org/multipage/editing.html#writing-suggestions>).
pub(super) fn writing_suggestions_value(
    base: &blitz_dom::BaseDocument,
    node: BlitzId,
) -> &'static str {
    let mut cursor = Some(node);
    while let Some(current) = cursor {
        if let Some(value) = crate::dom::tree::attr(base, current, "writingsuggestions") {
            return if value.eq_ignore_ascii_case("false") {
                "false"
            } else {
                "true"
            };
        }
        cursor = base.get_node(current).and_then(|tree| tree.parent);
    }
    "true"
}

/// The `autocapitalize` IDL keyword for a raw content-attribute value:
/// `off`/`none` read "none", `on`/`sentences` read "sentences", missing or
/// empty reads "", and any other value reads "sentences"
/// (<https://html.spec.whatwg.org/multipage/editing.html#autocapitalization>).
pub(super) fn autocapitalize_keyword(raw: Option<&str>) -> &'static str {
    let Some(value) = raw else {
        return "";
    };
    match value.to_ascii_lowercase().as_str() {
        "none" | "off" => "none",
        "words" => "words",
        "characters" => "characters",
        "" => "",
        // "sentences", "on", and anything else: the invalid value default.
        _ => "sentences",
    }
}

/// The `tabindex` default for one element: 0 for the focusable locals
/// (buttons, `select`/`textarea`/`iframe`, editing hosts, and the first
/// `summary` of a `details`), -1 otherwise. Connection and disabled state
/// do not move the IDL default (a detached `button` still reads 0; verified
/// live against the `WebDriver` build, which has no layout)
/// (<https://html.spec.whatwg.org/multipage/interaction.html#dom-tabindex>,
/// <https://html.spec.whatwg.org/multipage/interaction.html#focusable-area>).
///
/// This follows Chromium rather than the spec's bare local-name list: `a`
/// needs an `href`, `input type=hidden` is never a default focusable area,
/// and `audio`/`video` need `controls`. `area`, `frame`, `object`, and the
/// SVG/`MathML` `a` elements from the spec list read -1 here.
pub(super) fn tab_index_default(base: &blitz_dom::BaseDocument, node: BlitzId) -> i32 {
    let Some(tree) = base.get_node(node) else {
        return -1;
    };
    let Some(element) = tree.data.downcast_element() else {
        return -1;
    };
    if element.name.ns != html_namespace() {
        return -1;
    }
    // Editing hosts are focusable: `contenteditable` in the True or
    // Plaintext-Only state (empty, `true`, or `plaintext-only`, ASCII
    // case-insensitive).
    if crate::dom::tree::attr(base, node, "contenteditable").is_some_and(|state| {
        matches!(
            state.to_ascii_lowercase().as_str(),
            "" | "true" | "plaintext-only"
        )
    }) {
        return 0;
    }
    // The first `summary` child of a `details` is focusable.
    if element.name.local.as_str() == "summary"
        && let Some(parent) = tree.parent
        && is_first_summary_child(base, parent, node)
    {
        return 0;
    }
    match element.name.local.as_str() {
        "input" => {
            if crate::dom::tree::attr(base, node, "type")
                .is_some_and(|kind| kind.eq_ignore_ascii_case("hidden"))
            {
                -1
            } else {
                0
            }
        }
        "a" => {
            if crate::dom::tree::attr(base, node, "href").is_some() {
                0
            } else {
                -1
            }
        }
        "button" | "select" | "textarea" | "iframe" => 0,
        "audio" | "video" if crate::dom::tree::attr(base, node, "controls").is_some() => 0,
        _ => -1,
    }
}

/// The cryptographic nonce of `id`: the IDL-set slot when present, else
/// the content attribute, else empty
/// (<https://html.spec.whatwg.org/multipage/urls-and-fetching.html#nonce-attributes>).
pub(in crate::js::bindings) fn nonce_of(ctx: &Ctx<'_>, id: NodeId) -> Result<String> {
    let owner = world_for_node(ctx, id)?;
    if let Some(slot) = owner.borrow().state.nonce_slots.get(&id) {
        return Ok(slot.clone());
    }
    Ok(content_attr(ctx, id, "nonce").unwrap_or_default())
}

/// Writes the IDL `nonce` slot without touching the content attribute,
/// shared by the `HTMLElement`, `SVGElement`, and `MathMLElement` setters
/// (<https://html.spec.whatwg.org/multipage/urls-and-fetching.html#dom-nonce>).
pub(super) fn set_nonce_slot(ctx: &Ctx<'_>, id: NodeId, value: &str) -> Result<()> {
    let owner = world_for_node(ctx, id)?;
    owner
        .borrow_mut()
        .state
        .nonce_slots
        .insert(id, value.to_owned());
    Ok(())
}

/// One nullable `aria-*` (or `role`) reflection read: the content attribute
/// value, or null when absent.
pub(super) fn aria_get<'js>(
    ctx: &Ctx<'js>,
    id: NodeId,
    name: &str,
) -> Result<Option<rquickjs::String<'js>>> {
    match content_attr(ctx, id, name) {
        Some(value) => Ok(Some(rquickjs::String::from_str(ctx.clone(), &value)?)),
        None => Ok(None),
    }
}

/// One nullable `aria-*` (or `role`) reflection write: null removes the
/// content attribute.
pub(super) fn aria_set(
    ctx: &Ctx<'_>,
    id: NodeId,
    name: &str,
    value: Option<rquickjs::String<'_>>,
) -> Result<()> {
    match value {
        Some(value) => set_attribute_sync(ctx, id, name, &value.to_string()?),
        None => remove_attribute_sync(ctx, id, "", name, false),
    }
}

/// `crossorigin` reflection: absent reads null, the empty string reads
/// `anonymous`, anything else matches ASCII case-insensitively with an
/// `anonymous` invalid default; setting null removes the attribute
/// (<https://html.spec.whatwg.org/multipage/urls-and-fetching.html#dom-crossorigin>).
pub(super) fn reflect_cross_origin<'js>(
    ctx: &Ctx<'js>,
    id: NodeId,
) -> Result<Option<rquickjs::String<'js>>> {
    let Some(setting) = content_attr(ctx, id, "crossorigin")
        .as_deref()
        .map(blitz_traits::net::CorsSetting::parse)
    else {
        return Ok(None);
    };
    Ok(Some(rquickjs::String::from_str(
        ctx.clone(),
        setting.keyword(),
    )?))
}

pub(super) fn set_cross_origin(
    ctx: &Ctx<'_>,
    id: NodeId,
    value: Option<rquickjs::String<'_>>,
) -> Result<()> {
    // https://html.spec.whatwg.org/multipage/urls-and-fetching.html#dom-crossorigin
    match value {
        Some(value) => set_attribute_sync(ctx, id, "crossorigin", &value.to_string()?),
        None => remove_attribute_sync(ctx, id, "", "crossorigin", false),
    }
}

/// The first child of `parsed`'s document root satisfying `want`.
/// Rounds a CSSOM View extent to the nearest integer, with non-finite values
/// reading as zero
/// (<https://drafts.csswg.org/cssom-view/#extension-to-the-element-interface>).
pub(super) fn cssom_round(value: f32) -> i32 {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "float-to-int casts saturate and map NaN to zero"
    )]
    return value.round() as i32;
}

/// Current viewport offset for `id`'s document.
/// <https://drafts.csswg.org/cssom-view/#scrolling-viewport>
pub(super) fn viewport_offset(ctx: &Ctx<'_>, id: NodeId) -> Result<(f64, f64)> {
    let world = world_for_node(ctx, id)?;
    let world = world.borrow();
    let Some(parsed) = world.document(id) else {
        return Ok((0.0, 0.0));
    };
    let scroll = parsed.document.base.viewport_scroll();
    Ok((scroll.x, scroll.y))
}

/// The node whose children are a template's contents: the inert fragment
/// after `content` is first read, otherwise the same-document fragment
/// (<https://html.spec.whatwg.org/multipage/scripting.html#template-contents>).
fn template_contents_target(doc: &crate::documents::BlitzDocument, id: NodeId) -> NodeId {
    if let Some((document, node)) = doc.template_owner(id.node) {
        return NodeId { document, node };
    }
    crate::dom::tree::template_contents(&doc.base, id.node).map_or(id, |node| NodeId {
        document: id.document,
        node,
    })
}

pub(super) fn install(ctx: &Ctx<'_>) -> Result<()> {
    element_generated::install(ctx)?;
    html_element_generated::install(ctx)?;
    svg_element_generated::install(ctx)?;
    math_ml_element_generated::install(ctx)?;
    aria_mixin_generated::install(ctx)?;
    element_css_inline_style_generated::install(ctx)?;
    element_content_editable_generated::install(ctx)?;
    html_hyperlink_element_utils_generated::install(ctx)?;
    Ok(())
}

impl JsNode {
    // https://html.spec.whatwg.org/multipage/dom.html#the-innertext-idl-attribute
    pub(super) fn get_inner_text<'js>(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        let text = super::super::inner_text::rendered_text(ctx, self.handle.get().0)?;
        rquickjs::String::from_str(ctx.clone(), &text)
    }

    // https://html.spec.whatwg.org/multipage/dom.html#the-innertext-idl-attribute
    pub(super) fn get_outer_text<'js>(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        let text = super::super::inner_text::rendered_text(ctx, self.handle.get().0)?;
        rquickjs::String::from_str(ctx.clone(), &text)
    }

    /// Setting `outerText` replaces the element with the rendered text
    /// fragment, then merges with adjacent text siblings (but does not
    /// fully normalize). Without a parent it throws; the setter is a no-op
    /// on non-HTML elements, which never install it.
    /// (<https://html.spec.whatwg.org/multipage/dom.html#the-innertext-idl-attribute>).
    #[expect(
        clippy::too_many_lines,
        reason = "outerText setter steps in spec order; splitting them is a refactor, not a lint fix"
    )]
    pub(super) fn set_outer_text(&self, ctx: &Ctx<'_>, value: LegacyNullString) -> Result<()> {
        let value = value.0;
        let parent = {
            let world = world(ctx)?;
            let world = world.borrow();
            world.document(self.handle.get().0).and_then(|parsed| {
                parsed
                    .document
                    .base
                    .get_node(self.handle.get().0.node)
                    .and_then(|node| node.parent)
                    .map(|parent| NodeId {
                        document: self.handle.get().0.document,
                        node: parent,
                    })
            })
        };
        let Some(parent) = parent else {
            return Err(throw_dom(
                ctx,
                "NoModificationAllowedError",
                "outerText needs a parent",
            ));
        };
        // Replacing the document element would put text under `Document`.
        let is_document_parent = {
            let world = world(ctx)?;
            let world = world.borrow();
            world.document(self.handle.get().0).is_some_and(|parsed| {
                parsed
                    .document
                    .base
                    .get_node(parent.node)
                    .is_some_and(|node| matches!(node.data, NodeData::Document(_)))
            })
        };
        if is_document_parent {
            return Err(throw_dom(
                ctx,
                "HierarchyRequestError",
                "outerText cannot replace the document element",
            ));
        }
        let world = world(ctx)?;
        let world = world.borrow();
        let Some(mut parsed) = world.document_mut(self.handle.get().0) else {
            return Ok(());
        };
        // Child navigables die with the replaced element: collect their
        // containers before removal, destroy after the borrows drop.
        let iframes = iframe_containers_in_subtrees(&parsed, &[self.handle.get().0]);
        let mut added = crate::dom::text::rendered_text_fragment(&mut parsed, &value, true);
        for child in &added {
            place_journaled(&mut parsed, parent, *child, Some(self.handle.get().0));
        }
        unlink_journaled(&mut parsed, self.handle.get().0);
        // Merge with the previous text sibling, then the following one.
        if let Some(first) = added.first().copied() {
            let previous = sibling(&parsed.document.base, first.node, false).map(|node| NodeId {
                document: first.document,
                node,
            });
            if let Some(previous) = previous
                && crate::dom::text::is_mergeable_text_node(&parsed.document.base, previous.node)
                && crate::dom::text::is_mergeable_text_node(&parsed.document.base, first.node)
            {
                let previous_text = String::from(parsed.document.character_data(previous.node));
                let old_value = DomString::from(previous_text.clone());
                let mut data = previous_text;
                data.push_str(&String::from(parsed.document.character_data(first.node)));
                parsed
                    .document
                    .base
                    .mutate()
                    .set_node_text(previous.node, &data);
                parsed.document.record(JournalEntry::CharacterData {
                    target: previous,
                    old_value,
                });
                unlink_journaled(&mut parsed, first);
                if added.last() == Some(&first) {
                    added[0] = previous;
                } else {
                    added.remove(0);
                }
            }
        }
        if let Some(last) = added.last().copied() {
            let next = sibling(&parsed.document.base, last.node, true).map(|node| NodeId {
                document: last.document,
                node,
            });
            if let Some(next) = next
                && crate::dom::text::is_mergeable_text_node(&parsed.document.base, next.node)
                && crate::dom::text::is_mergeable_text_node(&parsed.document.base, last.node)
            {
                let last_text = String::from(parsed.document.character_data(last.node));
                let old_value = DomString::from(last_text.clone());
                let mut data = last_text;
                data.push_str(&String::from(parsed.document.character_data(next.node)));
                parsed
                    .document
                    .base
                    .mutate()
                    .set_node_text(last.node, &data);
                parsed.document.record(JournalEntry::CharacterData {
                    target: last,
                    old_value,
                });
                unlink_journaled(&mut parsed, next);
            }
        }
        drop(parsed);
        drop(world);
        world_for_node(ctx, self.handle.get().0)?
            .borrow()
            .destroy_child_navigables(&iframes);
        fixup_focus_after_removal(ctx, self.handle.get().0)?;
        schedule_mutation_delivery(ctx)
    }

    /// Setting `innerText` replaces all children with the rendered text
    /// fragment: newlines become `br` elements with no empty text around
    /// them (an all-empty value yields one empty text node)
    /// (<https://html.spec.whatwg.org/multipage/dom.html#set-the-inner-text-steps>).
    pub(super) fn set_inner_text(&self, ctx: &Ctx<'_>, value: LegacyNullString) -> Result<()> {
        let value = value.0;
        let html = world(ctx)?
            .borrow()
            .document(self.handle.get().0)
            .is_some_and(|parsed| {
                parsed
                    .document
                    .base
                    .get_node(self.handle.get().0.node)
                    .is_some_and(|node| {
                        node.data
                            .downcast_element()
                            .is_some_and(|element| element.name.ns == html_namespace())
                    })
            });
        if !html {
            return Ok(());
        }
        let world = world(ctx)?;
        let world = world.borrow();
        let Some(mut parsed) = world.document_mut(self.handle.get().0) else {
            return Ok(());
        };
        let added = crate::dom::text::rendered_text_fragment(&mut parsed, &value, false);
        let iframes = replace_all_journaled(&mut parsed, self.handle.get().0, added);
        drop(parsed);
        world.destroy_child_navigables(&iframes);
        drop(world);
        schedule_mutation_delivery(ctx)
    }
}

impl From<element_generated::ScrollBehavior> for blitz_dom::ScrollBehavior {
    fn from(value: element_generated::ScrollBehavior) -> Self {
        match value {
            element_generated::ScrollBehavior::Auto => Self::Auto,
            element_generated::ScrollBehavior::Instant => Self::Instant,
            element_generated::ScrollBehavior::Smooth => Self::Smooth,
        }
    }
}

impl From<element_generated::ScrollLogicalPosition> for blitz_dom::ScrollLogicalPosition {
    fn from(value: element_generated::ScrollLogicalPosition) -> Self {
        match value {
            element_generated::ScrollLogicalPosition::Start => Self::Start,
            element_generated::ScrollLogicalPosition::Center => Self::Center,
            element_generated::ScrollLogicalPosition::End => Self::End,
            element_generated::ScrollLogicalPosition::Nearest => Self::Nearest,
        }
    }
}
