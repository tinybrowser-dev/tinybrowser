use std::collections::HashSet;
use std::sync::Mutex;

use crate::Parsed;

static MIME_ESSENCES: Mutex<Option<HashSet<&'static str>>> = Mutex::new(None);

pub(crate) const PARSERERROR_NS: &str = "http://www.mozilla.org/newlayout/xml/parsererror.xml";

// https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-domparser-parsefromstring
pub(crate) fn parse_document(
    input: &str,
    content_type: &'static str,
    base_url: String,
    font_ctx: parley::FontContext,
) -> Parsed {
    parse_with_config(
        input,
        content_type,
        crate::ReadyState::Complete,
        false,
        blitz_dom::DocumentConfig {
            base_url: Some(base_url),
            font_ctx: Some(font_ctx),
            ua_stylesheets: Vec::new(),
            ..blitz_dom::DocumentConfig::default()
        },
    )
}

// https://dom.spec.whatwg.org/#concept-document-quirks
pub(crate) fn parse_navigated(
    input: &str,
    content_type: &'static str,
    config: blitz_dom::DocumentConfig,
) -> Parsed {
    parse_with_config(
        input,
        content_type,
        crate::ReadyState::Loading,
        true,
        config,
    )
}

// https://html.spec.whatwg.org/multipage/xhtml.html#parsing-xhtml-documents
fn parse_with_config(
    input: &str,
    content_type: &'static str,
    ready_state: crate::ReadyState,
    xml_document: bool,
    config: blitz_dom::DocumentConfig,
) -> Parsed {
    let base: blitz_dom::BaseDocument = blitz_html::HtmlDocument::from_xml(input, config).into();
    let mut document = crate::documents::BlitzDocument::from_base(base);
    let errors = document.base.take_parse_errors();
    if !errors.is_empty() {
        return parser_error_document(document, content_type, ready_state, xml_document, &errors);
    }
    let mut parsed = Parsed {
        id: 0,
        document,
        quirks_mode: markup5ever::interface::QuirksMode::NoQuirks,
        content_type,
        xml_document,
        ready_state,
        url: None,
        character_set: "UTF-8",
    };
    // https://html.spec.whatwg.org/multipage/input.html#the-input-element
    crate::dom::forms::initialize_controls(&mut parsed);
    parsed
}

// https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-domparser-parsefromstring
fn parser_error_document(
    mut document: crate::documents::BlitzDocument,
    content_type: &'static str,
    ready_state: crate::ReadyState,
    xml_document: bool,
    errors: &[String],
) -> Parsed {
    let root = document.base.root_node().id;
    document.base.mutate().remove_and_drop_all_children(root);
    let name = markup5ever::QualName::new(
        None,
        markup5ever::Namespace::from(PARSERERROR_NS),
        markup5ever::LocalName::from("parsererror"),
    );
    let element = document.base.mutate().create_element(name, Vec::new());
    document.base.mutate().append_children(root, &[element]);
    let message = errors.join("\n");
    if !message.is_empty() {
        let text = document.base.mutate().create_text_node(&message);
        document.base.mutate().append_children(element, &[text]);
    }
    Parsed {
        id: 0,
        document,
        quirks_mode: markup5ever::interface::QuirksMode::NoQuirks,
        content_type,
        xml_document,
        ready_state,
        url: None,
        character_set: "UTF-8",
    }
}

// https://mimesniff.spec.whatwg.org/#xml-mime-type
pub(crate) fn navigated_content_type(header: &str) -> Option<&'static str> {
    let essence = header.split(';').next()?.trim().to_ascii_lowercase();
    let (kind, subtype) = essence.split_once('/')?;
    if kind.is_empty() || !(subtype == "xml" || subtype.ends_with("+xml")) {
        return None;
    }
    Some(match essence.as_str() {
        "text/xml" => "text/xml",
        "application/xhtml+xml" => "application/xhtml+xml",
        "image/svg+xml" => "image/svg+xml",
        _ => "application/xml",
    })
}

// https://html.spec.whatwg.org/multipage/nav-history-apis.html#concept-document-content-type
// https://html.spec.whatwg.org/multipage/dom.html#dom-document-contenttype
pub(crate) fn document_content_type(header: &str) -> &'static str {
    if let Some(xml) = navigated_content_type(header) {
        return xml;
    }
    let essence = header
        .split(';')
        .next()
        .unwrap_or(header)
        .trim()
        .to_ascii_lowercase();
    if essence.is_empty() {
        return "application/octet-stream";
    }
    match essence.as_str() {
        "text/html" => "text/html",
        "text/css" => "text/css",
        "text/plain" => "text/plain",
        "image/jpeg" => "image/jpeg",
        "image/png" => "image/png",
        "image/gif" => "image/gif",
        "image/bmp" => "image/bmp",
        "application/octet-stream" => "application/octet-stream",
        _ => intern_mime_essence(essence),
    }
}

fn intern_mime_essence(essence: String) -> &'static str {
    const MAX_INTERNED_ESSENCES: usize = 1024;
    let mut guard = MIME_ESSENCES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let interned = guard.get_or_insert_with(HashSet::new);
    if let Some(existing) = interned.get(essence.as_str()) {
        return existing;
    }
    if interned.len() >= MAX_INTERNED_ESSENCES {
        return "application/octet-stream";
    }
    let leaked: &'static str = Box::leak(essence.into_boxed_str());
    interned.insert(leaked);
    leaked
}
