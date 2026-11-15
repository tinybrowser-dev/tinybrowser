//! Concrete HTML elements: tables, lists, text-level elements,
//! links, metadata, and the empty marker interfaces.

use super::super::{
    CollectionKind, WebIdlCodeUnits, collection_ids, document_base_url_string, dom_string,
    fixup_focus_after_removal, host, live_collection, schedule_mutation_delivery,
    set_attribute_sync, throw_dom, world, world_for_node,
};

use markup5ever::{LocalName, QualName};

use rquickjs::{Ctx, Result, Value};

use crate::dom::insertion::{
    iframe_containers_in_subtrees, insert_parsed, replace_all_journaled, unlink_journaled,
};
use crate::dom::tree::{NodeId, attr, html_namespace};
use crate::js::world::SameObject;

use super::JsNode;
use super::element::{
    content_attr, reflect_clamped_ulong, reflect_cross_origin, reflect_ulong, set_cross_origin,
};

include!(concat!(env!("OUT_DIR"), "/HTMLAnchorElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLAreaElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLBaseElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLLinkElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLMetaElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLTitleElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLStyleElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLScriptElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLSlotElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLMapElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLObjectElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLEmbedElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLSourceElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLTrackElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLParamElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLTableElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLTableSectionElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLTableRowElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLTableCellElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLTableColElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLTableCaptionElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLBRElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLBodyElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLDataElement.rs"));

include!(concat!(env!("OUT_DIR"), "/HTMLDetailsElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLDialogElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLDirectoryElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLDivElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLDListElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLFontElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLFrameSetElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLHeadElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLHeadingElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLHRElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLHtmlElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLLIElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLMenuElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLModElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLOListElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLParagraphElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLPictureElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLPreElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLQuoteElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLSpanElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLTimeElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLUListElement.rs"));

impl<'js> htmlli_element_generated::HTMLLIElement<'js> for JsNode {
    // `value` is a plain long defaulting to zero
    // (<https://html.spec.whatwg.org/multipage/grouping-content.html#dom-li-value>).
    fn get_value(&self, ctx: &Ctx<'js>) -> Result<i32> {
        Ok(content_attr(ctx, self.handle.get().0, "value")
            .as_deref()
            .and_then(crate::dom::parse::parse_integer)
            .unwrap_or(0))
    }

    // https://html.spec.whatwg.org/multipage/grouping-content.html#dom-li-value
    fn set_value(&self, ctx: &Ctx<'js>, value: i32) -> Result<()> {
        set_attribute_sync(ctx, self.handle.get().0, "value", &value.to_string())
    }
}

impl<'js> htmlo_list_element_generated::HTMLOListElement<'js> for JsNode {
    // `start` defaults to 1
    // (<https://html.spec.whatwg.org/multipage/grouping-content.html#dom-ol-start>).
    fn get_start(&self, ctx: &Ctx<'js>) -> Result<i32> {
        Ok(content_attr(ctx, self.handle.get().0, "start")
            .as_deref()
            .and_then(crate::dom::parse::parse_integer)
            .unwrap_or(1))
    }

    // https://html.spec.whatwg.org/multipage/grouping-content.html#dom-ol-start
    fn set_start(&self, ctx: &Ctx<'js>, value: i32) -> Result<()> {
        set_attribute_sync(ctx, self.handle.get().0, "start", &value.to_string())
    }
}

impl<'js> html_pre_element_generated::HTMLPreElement<'js> for JsNode {
    // `width` is a plain long defaulting to zero
    // (<https://html.spec.whatwg.org/multipage/obsolete.html#dom-pre-width>).
    fn get_width(&self, ctx: &Ctx<'js>) -> Result<i32> {
        Ok(content_attr(ctx, self.handle.get().0, "width")
            .as_deref()
            .and_then(crate::dom::parse::parse_integer)
            .unwrap_or(0))
    }

    // https://html.spec.whatwg.org/multipage/obsolete.html#dom-pre-width
    fn set_width(&self, ctx: &Ctx<'js>, value: i32) -> Result<()> {
        set_attribute_sync(ctx, self.handle.get().0, "width", &value.to_string())
    }
}

impl<'js> html_table_cell_element_generated::HTMLTableCellElement<'js> for JsNode {
    // `colSpan` parses 1..1000, defaulting to 1
    // (<https://html.spec.whatwg.org/multipage/tables.html#dom-tdth-colspan>).
    fn get_col_span(&self, ctx: &Ctx<'js>) -> Result<usize> {
        Ok(usize::try_from(reflect_clamped_ulong(
            content_attr(ctx, self.handle.get().0, "colspan").as_deref(),
            1,
            1,
            1000,
        ))
        .unwrap_or(usize::MAX))
    }

    // https://html.spec.whatwg.org/multipage/tables.html#dom-tdth-colspan
    fn set_col_span(&self, ctx: &Ctx<'js>, value: u32) -> Result<()> {
        host::reflect_set_ulong_defaulting(ctx, self.handle.get().0, "colspan", value, 1)
    }

    // `rowSpan` parses 0..65534, defaulting to 1
    // (<https://html.spec.whatwg.org/multipage/tables.html#dom-tdth-rowspan>).
    fn get_row_span(&self, ctx: &Ctx<'js>) -> Result<usize> {
        Ok(usize::try_from(reflect_clamped_ulong(
            content_attr(ctx, self.handle.get().0, "rowspan").as_deref(),
            1,
            0,
            65534,
        ))
        .unwrap_or(usize::MAX))
    }

    // https://html.spec.whatwg.org/multipage/tables.html#dom-tdth-rowspan
    fn set_row_span(&self, ctx: &Ctx<'js>, value: u32) -> Result<()> {
        host::reflect_set_ulong_defaulting(ctx, self.handle.get().0, "rowspan", value, 1)
    }
}

impl<'js> html_table_col_element_generated::HTMLTableColElement<'js> for JsNode {
    // `span` parses 1..1000, defaulting to 1
    // (<https://html.spec.whatwg.org/multipage/tables.html#dom-col-span>).
    fn get_span(&self, ctx: &Ctx<'js>) -> Result<usize> {
        Ok(usize::try_from(reflect_clamped_ulong(
            content_attr(ctx, self.handle.get().0, "span").as_deref(),
            1,
            1,
            1000,
        ))
        .unwrap_or(usize::MAX))
    }

    // https://html.spec.whatwg.org/multipage/tables.html#dom-col-span
    fn set_span(&self, ctx: &Ctx<'js>, value: u32) -> Result<()> {
        host::reflect_set_ulong_defaulting(ctx, self.handle.get().0, "span", value, 1)
    }
}

impl<'js> html_base_element_generated::HTMLBaseElement<'js> for JsNode {
    // Unlike generic URL reflection, an absent `href` falls back to the
    // document base URL rather than the empty string
    // (<https://html.spec.whatwg.org/multipage/semantics.html#dom-base-href>).
    fn get_href(&self, ctx: &Ctx<'js>) -> Result<crate::dom_string::DomString> {
        // Probe the node's owner document, not the calling realm's: a node
        // reached across documents must answer from its own attributes.
        let present = world_for_node(ctx, self.handle.get().0)
            .ok()
            .and_then(|owner| {
                owner.borrow().with_document(self.handle.get().0, |parsed| {
                    attr(&parsed.document.base, self.handle.get().0.node, "href").is_some()
                })
            })
            .unwrap_or(false);
        if present {
            host::reflect_url_string(ctx, self.handle.get().0, "href")
        } else {
            Ok(document_base_url_string(ctx, self.handle.get().0).into())
        }
    }
}

impl<'js> html_link_element_generated::HTMLLinkElement<'js> for JsNode {
    // https://html.spec.whatwg.org/multipage/semantics.html#dom-link-crossorigin
    fn get_cross_origin(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        reflect_cross_origin(ctx, self.handle.get().0)
    }

    // https://html.spec.whatwg.org/multipage/semantics.html#dom-link-crossorigin
    fn set_cross_origin(&self, ctx: &Ctx<'js>, value: Option<rquickjs::String<'js>>) -> Result<()> {
        set_cross_origin(ctx, self.handle.get().0, value)
    }

    // `[SameObject, PutForwards=value] readonly attribute DOMTokenList blocking`
    // (<https://html.spec.whatwg.org/multipage/urls-and-fetching.html#blocking-attributes>).
    fn get_blocking(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        self.token_list(
            ctx,
            SameObject::Blocking,
            markup5ever::local_name!("blocking"),
        )
    }
}

impl<'js> html_style_element_generated::HTMLStyleElement<'js> for JsNode {
    // `[SameObject, PutForwards=value] readonly attribute DOMTokenList blocking`
    // (<https://html.spec.whatwg.org/multipage/urls-and-fetching.html#blocking-attributes>).
    fn get_blocking(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        self.token_list(
            ctx,
            SameObject::Blocking,
            markup5ever::local_name!("blocking"),
        )
    }
}

impl<'js> html_dialog_element_generated::HTMLDialogElement<'js> for JsNode {
    // https://html.spec.whatwg.org/multipage/interactive-elements.html#the-dialog-element:dom-dialog-returnvalue
    fn get_return_value(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        let value = super::dialog::return_value(ctx, self.handle.get().0)?;
        rquickjs::String::from_str(ctx.clone(), &value.to_string_lossy())
    }

    // https://html.spec.whatwg.org/multipage/interactive-elements.html#the-dialog-element:dom-dialog-returnvalue
    fn set_return_value(&self, ctx: &Ctx<'js>, value: rquickjs::String<'js>) -> Result<()> {
        super::dialog::set_return_value(
            ctx,
            self.handle.get().0,
            crate::dom_string::DomString::from_utf16(value.to_utf16()?),
        )
    }

    // The computed closed-by state: the attribute's keyword, with the Auto
    // state reporting `closerequest` for a modal dialog and `none` otherwise.
    // The generator owns the `[ReflectSetter]` setter
    // (<https://html.spec.whatwg.org/multipage/interactive-elements.html#the-dialog-element:dom-dialog-closedby>).
    fn get_closed_by(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        let state = super::dialog::closed_by(ctx, self.handle.get().0);
        rquickjs::String::from_str(ctx.clone(), state)
    }

    // https://html.spec.whatwg.org/multipage/interactive-elements.html#the-dialog-element:dom-dialog-show
    fn show(&self, ctx: Ctx<'js>) -> Result<()> {
        super::dialog::show(&ctx, self.handle.get().0)
    }

    // https://html.spec.whatwg.org/multipage/interactive-elements.html#the-dialog-element:dom-dialog-close
    fn close(&self, ctx: Ctx<'js>, arg_0: Option<rquickjs::String<'js>>) -> Result<()> {
        let result = arg_0
            .map(|value| {
                value
                    .to_utf16()
                    .map(crate::dom_string::DomString::from_utf16)
            })
            .transpose()?;
        super::dialog::close(&ctx, self.handle.get().0, result)
    }

    // https://html.spec.whatwg.org/multipage/interactive-elements.html#the-dialog-element:dom-dialog-requestclose
    fn request_close(&self, ctx: Ctx<'js>, arg_0: Option<rquickjs::String<'js>>) -> Result<()> {
        let result = arg_0
            .map(|value| {
                value
                    .to_utf16()
                    .map(crate::dom_string::DomString::from_utf16)
            })
            .transpose()?;
        super::dialog::request_close(&ctx, self.handle.get().0, result)
    }
}

impl<'js> html_script_element_generated::HTMLScriptElement<'js> for JsNode {
    // https://html.spec.whatwg.org/multipage/scripting.html#dom-script-crossorigin
    fn get_cross_origin(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        reflect_cross_origin(ctx, self.handle.get().0)
    }

    // https://html.spec.whatwg.org/multipage/scripting.html#dom-script-crossorigin
    fn set_cross_origin(&self, ctx: &Ctx<'js>, value: Option<rquickjs::String<'js>>) -> Result<()> {
        set_cross_origin(ctx, self.handle.get().0, value)
    }

    // `[SameObject, PutForwards=value] readonly attribute DOMTokenList blocking`
    // (<https://html.spec.whatwg.org/multipage/urls-and-fetching.html#blocking-attributes>).
    fn get_blocking(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        self.token_list(
            ctx,
            SameObject::Blocking,
            markup5ever::local_name!("blocking"),
        )
    }

    // https://html.spec.whatwg.org/multipage/scripting.html#dom-script-text
    fn get_text(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        // `text` is the element's child text content: the text node children
        // only, not the text inside a descendant element the way `textContent`
        // answers (<https://html.spec.whatwg.org/multipage/scripting.html#dom-script-text>).
        let world_rc = world(ctx)?;
        let parsed = world_rc.borrow();
        let text = match parsed.document(self.handle.get().0) {
            Some(parsed) => crate::dom::text::child_text_content(
                &parsed.document.base,
                self.handle.get().0.node,
            ),
            None => String::new(),
        };
        rquickjs::String::from_str(ctx.clone(), &text)
    }

    // https://html.spec.whatwg.org/multipage/scripting.html#dom-script-text
    fn set_text(&self, ctx: &Ctx<'js>, value: rquickjs::String<'js>) -> Result<()> {
        // Setting replaces all children with one text node
        // (<https://html.spec.whatwg.org/multipage/scripting.html#dom-script-text>).
        self.set_text_content(ctx, Some(value))
    }
}

impl<'js> html_object_element_generated::HTMLObjectElement<'js> for JsNode {
    /// <https://html.spec.whatwg.org/multipage/forms.html#dom-fae-form>
    fn get_form(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        self.form(ctx.clone())
    }

    // Obsolete `hspace`/`vspace` default to zero
    // (<https://html.spec.whatwg.org/multipage/obsolete.html#dom-object-hspace>).
    fn get_hspace(&self, ctx: &Ctx<'js>) -> Result<usize> {
        Ok(usize::try_from(reflect_ulong(
            content_attr(ctx, self.handle.get().0, "hspace").as_deref(),
            0,
        ))
        .unwrap_or(usize::MAX))
    }

    // https://html.spec.whatwg.org/multipage/obsolete.html#dom-object-hspace
    fn set_hspace(&self, ctx: &Ctx<'js>, value: u32) -> Result<()> {
        host::reflect_set_ulong_defaulting(ctx, self.handle.get().0, "hspace", value, 0)
    }

    // https://html.spec.whatwg.org/multipage/obsolete.html#dom-object-vspace
    fn get_vspace(&self, ctx: &Ctx<'js>) -> Result<usize> {
        Ok(usize::try_from(reflect_ulong(
            content_attr(ctx, self.handle.get().0, "vspace").as_deref(),
            0,
        ))
        .unwrap_or(usize::MAX))
    }

    // https://html.spec.whatwg.org/multipage/obsolete.html#dom-object-vspace
    fn set_vspace(&self, ctx: &Ctx<'js>, value: u32) -> Result<()> {
        host::reflect_set_ulong_defaulting(ctx, self.handle.get().0, "vspace", value, 0)
    }

    // `codeBase` resolves against the document base URL
    // (<https://html.spec.whatwg.org/multipage/obsolete.html#dom-object-codebase>).
    fn get_code_base(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        // `DOMString` preserves code units exactly: convert through UTF-16
        // rather than lossy UTF-8.
        let resolved: crate::dom_string::DomString =
            host::reflect_url_string(ctx, self.handle.get().0, "codebase")?;
        rquickjs::String::from_utf16(ctx.clone(), &resolved.units())
    }

    // https://html.spec.whatwg.org/multipage/obsolete.html#dom-object-codebase
    fn set_code_base(&self, ctx: &Ctx<'js>, value: rquickjs::String<'js>) -> Result<()> {
        set_attribute_sync(ctx, self.handle.get().0, "codebase", &value.to_string()?)
    }
}

impl<'js> html_table_element_generated::HTMLTableElement<'js> for JsNode {
    /// <https://html.spec.whatwg.org/multipage/tables.html#dom-table-tbodies>
    fn get_t_bodies(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        live_collection(
            ctx,
            self.handle.get().0,
            CollectionKind::TableBodies,
            Some("HTMLCollection"),
        )
    }

    /// <https://html.spec.whatwg.org/multipage/tables.html#dom-table-rows>
    fn get_rows(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        live_collection(
            ctx,
            self.handle.get().0,
            CollectionKind::TableRows,
            Some("HTMLCollection"),
        )
    }

    /// <https://html.spec.whatwg.org/multipage/tables.html#dom-table-deleterow>
    fn delete_row(&self, ctx: Ctx<'js>, index: i32) -> Result<()> {
        delete_table_row(&ctx, self.handle.get().0, index)
    }
}

impl<'js> html_table_section_element_generated::HTMLTableSectionElement<'js> for JsNode {
    /// <https://html.spec.whatwg.org/multipage/tables.html#dom-tbody-rows>
    fn get_rows(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        live_collection(
            ctx,
            self.handle.get().0,
            CollectionKind::TableSectionRows,
            Some("HTMLCollection"),
        )
    }
}

impl<'js> html_table_row_element_generated::HTMLTableRowElement<'js> for JsNode {
    /// <https://html.spec.whatwg.org/multipage/tables.html#dom-tr-cells>
    fn get_cells(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        live_collection(
            ctx,
            self.handle.get().0,
            CollectionKind::TableRowCells,
            Some("HTMLCollection"),
        )
    }
}

impl JsNode {
    // https://dom.spec.whatwg.org/#dom-document-title
    pub(super) fn title<'js>(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        let world = world(ctx)?;
        let parsed = world.borrow();
        let Some(parsed) = parsed.document(self.handle.get().0) else {
            return rquickjs::String::from_str(ctx.clone(), "");
        };
        match document_title_element(&parsed) {
            // The getter answers the title element's child text content with
            // ASCII whitespace stripped and collapsed
            // (<https://html.spec.whatwg.org/multipage/dom.html#dom-document-title>).
            Some(title) => dom_string(
                ctx,
                &parsed
                    .document
                    .descendant_dom_text(title.node)
                    .strip_and_collapse_ascii_whitespace(),
            ),
            None => rquickjs::String::from_str(ctx.clone(), ""),
        }
    }

    pub(super) fn set_title(&self, ctx: &Ctx<'_>, value: WebIdlCodeUnits) -> Result<()> {
        let exact = value.0;
        let text = exact.to_string_lossy().into_owned();
        let world = world(ctx)?;
        let world = world.borrow();
        let Some(mut parsed) = world.document_mut(self.handle.get().0) else {
            return Ok(());
        };
        let document = parsed.id;
        let Some(element) =
            crate::dom::namespaces::document_element_of(&parsed.document, self.handle.get().0)
        else {
            return Ok(());
        };
        let svg_root = element_is_svg_svg(&parsed.document, element.node);
        let html_document = matches!(parsed.content_type, "text/html" | "application/xhtml+xml");
        // A document with no document element, or one that is neither HTML nor
        // an SVG `svg`, cannot hold a title
        // (<https://html.spec.whatwg.org/multipage/dom.html#dom-document-title>).
        if !html_document && !svg_root {
            return Ok(());
        }
        let title = match document_title_element(&parsed) {
            Some(title) => title,
            None if svg_root => {
                // An SVG document's title is the first SVG `title` child of
                // its document element.
                let name = QualName::new(None, markup5ever::ns!(svg), LocalName::from("title"));
                let title = parsed
                    .document
                    .base
                    .mutate()
                    .create_element(name, Vec::new());
                let title = NodeId {
                    document,
                    node: title,
                };
                let reference = parsed
                    .document
                    .base
                    .get_node(element.node)
                    .and_then(|node| node.children.first().copied())
                    .map(|node| NodeId { document, node });
                insert_parsed(&mut parsed, element, title, reference);
                title
            }
            None if html_document => {
                // An HTML document appends the new `title` to its `head`; a
                // document with no head element has nowhere to put one, so
                // the write does nothing.
                let Some(head) = document_head_element(&parsed, element) else {
                    return Ok(());
                };
                let name = QualName::new(None, html_namespace(), LocalName::from("title"));
                let title = parsed
                    .document
                    .base
                    .mutate()
                    .create_element(name, Vec::new());
                let title = NodeId {
                    document,
                    node: title,
                };
                insert_parsed(&mut parsed, head, title, None);
                title
            }
            None => return Ok(()),
        };
        let added = if text.is_empty() {
            Vec::new()
        } else {
            let text = parsed.document.create_text(&exact);
            vec![NodeId {
                document: parsed.id,
                node: text,
            }]
        };
        let iframes = replace_all_journaled(&mut parsed, title, added);
        drop(parsed);
        world.destroy_child_navigables(&iframes);
        drop(world);
        schedule_mutation_delivery(ctx)
    }
}

/// The document's title element
/// (<https://html.spec.whatwg.org/multipage/dom.html#the-title-element>).
///
/// An SVG document (its document element is an SVG `svg`) uses an SVG `title`
/// that is a child of the document element. Every other document uses the
/// first `title` in the HTML namespace in tree order, which is why an HTML
/// title still counts in an XML document.
fn document_title_element(parsed: &crate::Parsed) -> Option<NodeId> {
    let base = &parsed.document.base;
    let root = base.root_node().id;
    let document_element = base.get_node(root).and_then(|node| {
        node.children
            .iter()
            .copied()
            .find(|child| base.get_node(*child).is_some_and(is_element_node))
    });
    if document_element.is_some_and(|id| element_is_svg_svg(&parsed.document, id)) {
        return base
            .get_node(document_element?)
            .and_then(|node| {
                node.children.iter().copied().find(|child| {
                    base.get_node(*child)
                        .and_then(|node| node.data.downcast_element())
                        .is_some_and(|data| {
                            data.name.ns == markup5ever::ns!(svg)
                                && data.name.local.as_str() == "title"
                        })
                })
            })
            .map(|node| NodeId {
                document: parsed.id,
                node,
            });
    }
    let mut stack: Vec<_> = base
        .get_node(root)
        .map_or_default(|node| node.children.iter().rev().copied().collect());
    while let Some(id) = stack.pop() {
        let Some(node) = base.get_node(id) else {
            continue;
        };
        if let Some(data) = node.data.downcast_element()
            && data.name.ns == markup5ever::ns!(html)
            && data.name.local.as_str() == "title"
        {
            return Some(NodeId {
                document: parsed.id,
                node: id,
            });
        }
        stack.extend(node.children.iter().rev().copied());
    }
    None
}

/// Whether `node` is an element.
fn is_element_node(node: &blitz_dom::Node) -> bool {
    node.data.downcast_element().is_some()
}

/// Whether `node` is an SVG `svg` element, which makes its document an SVG
/// document for `document.title`.
fn element_is_svg_svg(
    document: &crate::documents::BlitzDocument,
    node: blitz_traits::node_id::NodeId,
) -> bool {
    document
        .base
        .get_node(node)
        .and_then(|node| node.data.downcast_element())
        .is_some_and(|data| {
            data.name.ns == markup5ever::ns!(svg) && data.name.local.as_str() == "svg"
        })
}

/// The `head` element of an HTML document, if the document element has one.
fn document_head_element(parsed: &crate::Parsed, document_element: NodeId) -> Option<NodeId> {
    let base = &parsed.document.base;
    base.get_node(document_element.node)
        .and_then(|node| {
            node.children.iter().copied().find(|child| {
                base.get_node(*child)
                    .and_then(|node| node.data.downcast_element())
                    .is_some_and(|data| {
                        data.name.ns == markup5ever::ns!(html) && data.name.local.as_str() == "head"
                    })
            })
        })
        .map(|node| NodeId {
            document: parsed.id,
            node,
        })
}
/// Removes the `index`th row of `table` in `table.rows` order; `-1` is the
/// last row. Out-of-range indexes throw `IndexSizeError`
/// (<https://html.spec.whatwg.org/multipage/tables.html#dom-table-deleterow>).
pub(super) fn delete_table_row(ctx: &Ctx<'_>, table: NodeId, index: i32) -> Result<()> {
    let rows = collection_ids(ctx, table, &CollectionKind::TableRows)?;
    let length = i32::try_from(rows.len()).unwrap_or(i32::MAX);
    if index < -1 || index >= length {
        return Err(throw_dom(
            ctx,
            "IndexSizeError",
            "row index is out of range",
        ));
    }
    if index == -1 && rows.is_empty() {
        return Ok(());
    }
    let slot = if index == -1 {
        rows.len() - 1
    } else {
        usize::try_from(index).unwrap_or(0)
    };
    let Some(&row) = rows.get(slot) else {
        return Ok(());
    };
    let world = world(ctx)?;
    let world = world.borrow();
    let Some(mut parsed) = world.document_mut(row) else {
        return Ok(());
    };
    let iframes = iframe_containers_in_subtrees(&parsed, &[row]);
    unlink_journaled(&mut parsed, row);
    drop(parsed);
    world.destroy_child_navigables(&iframes);
    drop(world);
    fixup_focus_after_removal(ctx, row)?;
    schedule_mutation_delivery(ctx)
}

pub(super) fn install(ctx: &Ctx<'_>) -> Result<()> {
    html_anchor_element_generated::install(ctx)?;
    html_area_element_generated::install(ctx)?;
    html_base_element_generated::install(ctx)?;
    html_body_element_generated::install(ctx)?;
    htmlbr_element_generated::install(ctx)?;
    html_data_element_generated::install(ctx)?;
    html_details_element_generated::install(ctx)?;
    html_dialog_element_generated::install(ctx)?;
    html_directory_element_generated::install(ctx)?;
    html_div_element_generated::install(ctx)?;
    htmld_list_element_generated::install(ctx)?;
    html_embed_element_generated::install(ctx)?;
    html_font_element_generated::install(ctx)?;
    html_frame_set_element_generated::install(ctx)?;
    html_head_element_generated::install(ctx)?;
    html_heading_element_generated::install(ctx)?;
    htmlhr_element_generated::install(ctx)?;
    html_html_element_generated::install(ctx)?;
    htmlli_element_generated::install(ctx)?;
    html_link_element_generated::install(ctx)?;
    html_map_element_generated::install(ctx)?;
    html_menu_element_generated::install(ctx)?;
    html_meta_element_generated::install(ctx)?;
    html_mod_element_generated::install(ctx)?;
    html_object_element_generated::install(ctx)?;
    htmlo_list_element_generated::install(ctx)?;
    html_paragraph_element_generated::install(ctx)?;
    html_param_element_generated::install(ctx)?;
    html_picture_element_generated::install(ctx)?;
    html_pre_element_generated::install(ctx)?;
    html_quote_element_generated::install(ctx)?;
    html_script_element_generated::install(ctx)?;
    html_slot_element_generated::install(ctx)?;
    html_source_element_generated::install(ctx)?;
    html_span_element_generated::install(ctx)?;
    html_style_element_generated::install(ctx)?;
    html_table_caption_element_generated::install(ctx)?;
    html_table_cell_element_generated::install(ctx)?;
    html_table_col_element_generated::install(ctx)?;
    html_table_element_generated::install(ctx)?;
    html_table_row_element_generated::install(ctx)?;
    html_table_section_element_generated::install(ctx)?;
    html_time_element_generated::install(ctx)?;
    html_title_element_generated::install(ctx)?;
    html_track_element_generated::install(ctx)?;
    htmlu_list_element_generated::install(ctx)?;
    Ok(())
}
