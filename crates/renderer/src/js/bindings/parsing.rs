use super::{
    NodeContext, create_node, retarget_wrapper, throw_dom, throw_dom_error, validate_and_extract,
    world, world_for_node, wrap_new_document, wrap_new_document_in_world, wrap_node,
};

use crate::dom::adoption::{detach_for_adopt, import_snapshot, materialize_import};

use crate::dom::tree::NodeId;
use crate::js::world::Handle;

use markup5ever::{LocalName, QualName};

use rquickjs::{Ctx, Exception, Result, Value, class::Trace, prelude::Func};

// https://dom.spec.whatwg.org/#interface-domimplementation
#[derive(Trace, rquickjs::JsLifetime)]
pub struct JsImplementation {
    pub(crate) document: Handle,
}

include!(concat!(env!("OUT_DIR"), "/DOMImplementation.rs"));

impl<'js> dom_implementation_generated::DOMImplementation<'js> for JsImplementation {
    // https://dom.spec.whatwg.org/#dom-domimplementation-hasfeature
    fn has_feature(&self, _ctx: Ctx<'js>) -> Result<bool> {
        Ok(true)
    }

    // https://dom.spec.whatwg.org/#dom-domimplementation-createdocumenttype
    fn create_document_type(
        &self,
        ctx: Ctx<'js>,
        name: rquickjs::String<'js>,
        public_id: rquickjs::String<'js>,
        system_id: rquickjs::String<'js>,
    ) -> Result<Value<'js>> {
        let name = name.to_string()?;
        let public_id = public_id.to_string()?;
        let system_id = system_id.to_string()?;
        if !valid_doctype_name(&name) {
            return Err(throw_dom(
                &ctx,
                "InvalidCharacterError",
                "doctype name contains invalid characters",
            ));
        }
        // https://dom.spec.whatwg.org/#dom-domimplementation-createdocumenttype
        create_node(&ctx, self.document.0, |parsed| {
            parsed
                .document
                .create_doctype(&name, &public_id, &system_id)
        })
    }

    // https://dom.spec.whatwg.org/#dom-domimplementation-createdocument
    fn create_document(
        &self,
        ctx: Ctx<'js>,
        namespace: Option<rquickjs::String<'js>>,
        qualified: rquickjs::String<'js>,
        doctype: Option<super::host::NodeReference>,
    ) -> Result<Value<'js>> {
        let namespace = namespace
            .map(|value| value.to_string())
            .transpose()?
            .unwrap_or_default();
        let qualified = qualified.to_string()?;
        let content_type = match namespace.as_str() {
            "http://www.w3.org/1999/xhtml" => "application/xhtml+xml",
            "http://www.w3.org/2000/svg" => "image/svg+xml",
            _ => "application/xml",
        };
        // https://dom.spec.whatwg.org/#dom-domimplementation-createdocument
        let root = if qualified.is_empty() {
            None
        } else {
            Some(validate_and_extract(
                &ctx,
                (!namespace.is_empty()).then_some(namespace.as_str()),
                &qualified,
                NodeContext::Element,
            )?)
        };
        let world_rc = world_for_node(&ctx, self.document.0)?;
        let font_ctx = world_rc.borrow().runtime.font_ctx.clone();
        let parsed = crate::Parsed::script(content_type, font_ctx);
        // https://dom.spec.whatwg.org/#dom-domimplementation-createdocument
        let document_root = world_rc.borrow_mut().add_document(parsed);
        world_rc
            .borrow()
            .registry()
            .borrow_mut()
            .insert_document(document_root.document_id(), &world_rc);
        if let Some(super::host::NodeReference::Tree(doctype_id)) = doctype {
            append_adopted_doctype(&ctx, document_root, doctype_id)?;
        }
        if let Some(name) = root {
            let world = world_rc.borrow();
            let Some(mut parsed) = world.document_mut(document_root) else {
                return Err(rquickjs::Exception::throw_type(&ctx, "no document"));
            };
            let element = parsed
                .document
                .base
                .mutate()
                .create_element(name, Vec::new());
            parsed
                .document
                .base
                .mutate()
                .append_children(document_root.node, &[element]);
        }
        wrap_node(&ctx, document_root)
    }

    // https://dom.spec.whatwg.org/#dom-domimplementation-createhtmldocument
    fn create_html_document(
        &self,
        ctx: Ctx<'js>,
        title: Option<rquickjs::String<'js>>,
    ) -> Result<Value<'js>> {
        let font_ctx = world(&ctx)?.borrow().runtime.font_ctx.clone();
        let mut parsed = crate::Parsed::script("text/html", font_ctx);
        // https://dom.spec.whatwg.org/#dom-domimplementation-createhtmldocument
        let document_root = parsed.document.base.root_node().id;
        let doctype = parsed.document.create_doctype("html", "", "");
        parsed
            .document
            .base
            .mutate()
            .append_children(document_root, &[doctype]);
        let html = parsed
            .document
            .base
            .mutate()
            .create_element(html_element_name("html"), Vec::new());
        parsed
            .document
            .base
            .mutate()
            .append_children(document_root, &[html]);
        let head = parsed
            .document
            .base
            .mutate()
            .create_element(html_element_name("head"), Vec::new());
        parsed.document.base.mutate().append_children(html, &[head]);
        if let Some(title) = title {
            let title_text = title.to_string()?;
            let title_element = parsed
                .document
                .base
                .mutate()
                .create_element(html_element_name("title"), Vec::new());
            parsed
                .document
                .base
                .mutate()
                .append_children(head, &[title_element]);
            let text = parsed.document.base.mutate().create_text_node(&title_text);
            parsed
                .document
                .base
                .mutate()
                .append_children(title_element, &[text]);
        }
        let body = parsed
            .document
            .base
            .mutate()
            .create_element(html_element_name("body"), Vec::new());
        parsed.document.base.mutate().append_children(html, &[body]);
        wrap_new_document_in_world(&ctx, parsed, &world_for_node(&ctx, self.document.0)?)
    }
}

// https://dom.spec.whatwg.org/#concept-node-adopt
fn append_adopted_doctype(ctx: &Ctx<'_>, document: NodeId, doctype: NodeId) -> Result<()> {
    let source_world = world_for_node(ctx, doctype)?;
    let snapshot = {
        let source = source_world.borrow();
        let Some(parsed) = source.document(doctype) else {
            return Err(rquickjs::Exception::throw_type(ctx, "no document"));
        };
        import_snapshot(&parsed.document, doctype, true)
            .ok_or_else(|| throw_dom(ctx, "HierarchyRequestError", "node cannot be adopted"))?
    };
    {
        let source = source_world.borrow();
        let Some(mut parsed) = source.document_mut(doctype) else {
            return Err(rquickjs::Exception::throw_type(ctx, "no document"));
        };
        detach_for_adopt(&mut parsed.document, doctype).map_err(|err| throw_dom_error(ctx, err))?;
    }
    let fresh = {
        let target_world = world_for_node(ctx, document)?;
        let target = target_world.borrow();
        let Some(mut parsed) = target.document_mut(document) else {
            return Err(rquickjs::Exception::throw_type(ctx, "no document"));
        };
        let fresh = materialize_import(&mut parsed.document, document.document, &snapshot)
            .map_err(|err| throw_dom_error(ctx, err))?;
        parsed
            .document
            .base
            .mutate()
            .append_children(document.node, &[fresh.node]);
        fresh
    };
    retarget_wrapper(ctx, doctype, fresh)
}

fn html_element_name(local: &str) -> QualName {
    QualName::new(
        None,
        crate::dom::tree::html_namespace(),
        LocalName::from(local),
    )
}

// https://dom.spec.whatwg.org/#valid-doctype-name
fn valid_doctype_name(name: &str) -> bool {
    !name
        .chars()
        .any(|c| matches!(c, '\t' | '\n' | '\u{c}' | '\r' | ' ' | '\0' | '>'))
}

// https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-parsing-and-serialization
#[derive(Trace, rquickjs::JsLifetime)]
pub struct JsDomParser {
    // https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-domparser-parsefromstring
    url: String,
}

include!(concat!(env!("OUT_DIR"), "/DOMParser.rs"));

impl<'js> dom_parser_generated::DOMParser<'js> for JsDomParser {
    // https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-domparser-domparser
    fn constructor(ctx: &Ctx<'js>) -> Result<Self> {
        Ok(Self {
            url: world(ctx)?.borrow().document_url.as_str().to_owned(),
        })
    }

    // https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-domparser-parsefromstring
    fn parse_from_string(
        &self,
        ctx: Ctx<'js>,
        source: rquickjs::String<'js>,
        type_: dom_parser_generated::DOMParserSupportedType,
    ) -> Result<Value<'js>> {
        parse_string_document(&ctx, &source, type_.as_str(), &self.url)
    }
}

// https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-domparser-parsefromstring
// https://xhr.spec.whatwg.org/#document-response
fn parse_string_document<'js>(
    ctx: &Ctx<'js>,
    source: &rquickjs::String<'js>,
    content_type: &'static str,
    url: &str,
) -> Result<Value<'js>> {
    // https://crbug.com/40814739
    let source = String::from_utf16_lossy(&source.to_utf16()?);
    let font_ctx = world(ctx)?.borrow().runtime.font_ctx.clone();
    let base = url.parse::<url::Url>().map_or_else(
        |_| crate::render::INVALID_BASE_URL.to_owned(),
        |url| crate::render::blitz_base_url(&url),
    );
    let mut parsed = if content_type == "text/html" {
        let mut parsed = crate::parse_html(
            &source,
            blitz_dom::DocumentConfig {
                base_url: Some(base),
                font_ctx: Some(font_ctx),
                ua_stylesheets: Vec::new(),
                ..blitz_dom::DocumentConfig::default()
            },
        );
        parsed.content_type = content_type;
        parsed.ready_state = crate::ReadyState::Complete;
        parsed
    } else {
        crate::xml::parse_document(&source, content_type, base, font_ctx)
    };
    parsed.document.mark_all_scripts_started();
    parsed.url = Some(url.to_owned());
    wrap_new_document(ctx, parsed)
}

// https://xhr.spec.whatwg.org/#document-response
pub(super) fn install_xhr_parse(ctx: &Ctx<'_>) -> Result<()> {
    crate::js::bridge::object(ctx)?.set("__tbXhrParseDocument", Func::from(xhr_parse_document))?;
    Ok(())
}

// https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-document-parsehtmlunsafe
pub(super) fn install_document_parse(ctx: &Ctx<'_>) -> Result<()> {
    crate::js::bridge::object(ctx)?
        .set("__tbParseHTMLDocument", Func::from(parse_html_document))?;
    Ok(())
}

// https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-parsehtmlunsafe
fn parse_html_document<'js>(ctx: Ctx<'js>, source: Value<'js>) -> Result<Value<'js>> {
    let source = source.get::<rquickjs::String>()?;
    parse_string_document(&ctx, &source, "text/html", "about:blank")
}

fn xhr_parse_document<'js>(
    ctx: Ctx<'js>,
    source: rquickjs::String<'js>,
    content_type: String,
    url: String,
) -> Result<Value<'js>> {
    let content_type: &'static str = match content_type.as_str() {
        "text/html" => "text/html",
        "text/xml" => "text/xml",
        "application/xml" => "application/xml",
        "application/xhtml+xml" => "application/xhtml+xml",
        "image/svg+xml" => "image/svg+xml",
        _ => {
            return Err(Exception::throw_type(
                &ctx,
                "Unknown XML response content type.",
            ));
        }
    };
    parse_string_document(&ctx, &source, content_type, &url)
}

// https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-domparser-parsefromstring
pub(crate) const INSTALL_DOMPARSER_CTOR_DEFLATE: &[u8] = include_bytes!(concat!(
    env!("OUT_DIR"),
    "/js_blobs/dom_parser_ctor.deflate"
));

pub(crate) fn install_domparser_ctor_js(ctx: &Ctx<'_>) -> Result<&'static str> {
    static CACHE: std::sync::OnceLock<Box<str>> = std::sync::OnceLock::new();
    crate::js::blob::decompress(ctx, INSTALL_DOMPARSER_CTOR_DEFLATE, &CACHE)
}

// https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#xmlserializer
#[derive(Trace, rquickjs::JsLifetime)]
pub struct JsXmlSerializer;

include!(concat!(env!("OUT_DIR"), "/XMLSerializer.rs"));

impl<'js> xml_serializer_generated::XMLSerializer<'js> for JsXmlSerializer {
    fn constructor(_ctx: &Ctx<'js>) -> Result<Self> {
        Ok(Self)
    }

    // https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-xmlserializer-serializetostring
    fn serialize_to_string(
        &self,
        ctx: Ctx<'js>,
        root: super::host::NodeReference,
    ) -> Result<rquickjs::String<'js>> {
        // https://w3c.github.io/DOM-Parsing/#dfn-xml-serialization-algorithm
        let Some(id) = root.tree() else {
            return rquickjs::String::from_str(ctx.clone(), "");
        };
        let owner = world_for_node(&ctx, id)?;
        let text = owner.borrow().document(id).map_or_else(
            crate::dom_string::DomString::default,
            |parsed| {
                let mut output = crate::dom::serialization::HtmlOutput::default();
                crate::dom::serialization::serialize_xml_node(
                    &parsed.document,
                    id.node,
                    &mut output,
                );
                output.finish()
            },
        );
        crate::js::bindings::dom_string(&ctx, &text)
    }
}
