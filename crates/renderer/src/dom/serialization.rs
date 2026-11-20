// https://html.spec.whatwg.org/multipage/parsing.html#serialising-html-fragments
// https://w3c.github.io/DOM-Parsing/#xml-serialization

use blitz_dom::NodeData;
use blitz_traits::node_id::NodeId as BlitzId;
use markup5ever::QualName;

use crate::documents::BlitzDocument;
use crate::dom::tree::{html_namespace, qualified_name, svg_namespace};
use crate::dom_string::DomString;

#[derive(Default)]
pub(crate) struct HtmlOutput(Vec<u16>);

impl HtmlOutput {
    fn push_str(&mut self, text: &str) {
        self.0.extend(text.encode_utf16());
    }

    fn push_units(&mut self, units: &[u16]) {
        self.0.extend_from_slice(units);
    }

    pub(crate) fn finish(self) -> DomString {
        DomString::from_utf16(self.0)
    }
}

// https://html.spec.whatwg.org/multipage/parsing.html#serialises-as-void
fn serializes_as_void(name: &QualName) -> bool {
    name.ns == html_namespace()
        && matches!(
            name.local.as_str(),
            "area"
                | "base"
                | "basefont"
                | "bgsound"
                | "br"
                | "col"
                | "embed"
                | "frame"
                | "hr"
                | "img"
                | "input"
                | "keygen"
                | "link"
                | "menuitem"
                | "meta"
                | "param"
                | "source"
                | "track"
                | "wbr"
        )
}

// https://html.spec.whatwg.org/multipage/parsing.html#serialising-html-fragments
fn push_html_element_name(output: &mut HtmlOutput, name: &QualName) {
    if name.ns == html_namespace()
        || name.ns == svg_namespace()
        || name.ns.as_str() == "http://www.w3.org/1998/Math/MathML"
    {
        output.push_str(name.local.as_str());
    } else if let Some(prefix) = name.prefix.as_ref().filter(|prefix| !prefix.is_empty()) {
        output.push_str(prefix.as_str());
        output.push_str(":");
        output.push_str(name.local.as_str());
    } else {
        output.push_str(name.local.as_str());
    }
}

// https://html.spec.whatwg.org/multipage/parsing.html#attribute-s-serialized-name
fn push_html_attribute_name(output: &mut HtmlOutput, name: &QualName) {
    if name.ns.as_str().is_empty() {
        output.push_str(name.local.as_str());
    } else if name.ns.as_str() == "http://www.w3.org/XML/1998/namespace" {
        output.push_str("xml:");
        output.push_str(name.local.as_str());
    } else if name.ns.as_str() == "http://www.w3.org/2000/xmlns/" {
        if name.local.as_str() == "xmlns" {
            output.push_str("xmlns");
        } else {
            output.push_str("xmlns:");
            output.push_str(name.local.as_str());
        }
    } else if name.ns.as_str() == "http://www.w3.org/1999/xlink" {
        output.push_str("xlink:");
        output.push_str(name.local.as_str());
    } else if let Some(prefix) = name.prefix.as_ref().filter(|prefix| !prefix.is_empty()) {
        output.push_str(prefix.as_str());
        output.push_str(":");
        output.push_str(name.local.as_str());
    } else {
        output.push_str(name.local.as_str());
    }
}

fn push_escaped_html_text(output: &mut HtmlOutput, text: &str) {
    for unit in text.encode_utf16() {
        match unit {
            0x26 => output.push_str("&amp;"),
            0x00a0 => output.push_str("&nbsp;"),
            0x3c => output.push_str("&lt;"),
            0x3e => output.push_str("&gt;"),
            _ => output.push_units(&[unit]),
        }
    }
}

fn push_escaped_html_attribute(output: &mut HtmlOutput, value: &str) {
    for unit in value.encode_utf16() {
        match unit {
            0x26 => output.push_str("&amp;"),
            0x00a0 => output.push_str("&nbsp;"),
            0x22 => output.push_str("&quot;"),
            _ => output.push_units(&[unit]),
        }
    }
}

// https://html.spec.whatwg.org/multipage/parsing.html#serialising-html-fragments
fn serialize_html_element(
    doc: &BlitzDocument,
    id: BlitzId,
    name: &QualName,
    attributes: &[blitz_dom::Attribute],
    output: &mut HtmlOutput,
) {
    let base = &doc.base;
    output.push_str("<");
    push_html_element_name(output, name);
    for attribute in attributes {
        output.push_str(" ");
        push_html_attribute_name(output, &attribute.name);
        output.push_str("=\"");
        push_escaped_html_attribute(output, &attribute.value);
        output.push_str("\"");
    }
    output.push_str(">");
    if serializes_as_void(name) {
        return;
    }
    // https://html.spec.whatwg.org/multipage/parsing.html#serialising-html-fragments
    let children: Vec<BlitzId> = base.get_node(id).map_or_default(|node| {
        if crate::dom::tree::is_template_tag(name) {
            crate::dom::tree::template_contents(base, id)
                .and_then(|fragment| {
                    base.get_node(fragment)
                        .map(|node| node.children.iter().copied().collect())
                })
                .unwrap_or_default()
        } else {
            node.children.iter().copied().collect()
        }
    });
    for child in &children {
        serialize_html_node(doc, *child, Some(name), output);
    }
    output.push_str("</");
    push_html_element_name(output, name);
    output.push_str(">");
}

fn serialize_html_node(
    doc: &BlitzDocument,
    id: BlitzId,
    parent: Option<&QualName>,
    output: &mut HtmlOutput,
) {
    let base = &doc.base;
    let Some(node) = base.get_node(id) else {
        return;
    };
    match &node.data {
        NodeData::Element(element) => {
            let name = element.name.clone();
            let attributes = element.attrs.iter().cloned().collect::<Vec<_>>();
            serialize_html_element(doc, id, &name, &attributes, output);
        }
        NodeData::AnonymousBlock(_) | NodeData::Fragment { .. } | NodeData::ShadowRoot { .. } => {
            for child in node.children.clone() {
                serialize_html_node(doc, child, parent, output);
            }
        }
        NodeData::Text(data) => {
            let raw_text = parent.is_some_and(|name| {
                name.ns == html_namespace()
                    && matches!(
                        name.local.as_str(),
                        "style"
                            | "script"
                            | "xmp"
                            | "iframe"
                            | "noembed"
                            | "noscript"
                            | "noframes"
                            | "plaintext"
                    )
            });
            if raw_text {
                output.push_str(&data.content);
            } else {
                push_escaped_html_text(output, &data.content);
            }
        }
        NodeData::Comment { contents } => {
            output.push_str("<!--");
            output.push_str(contents);
            output.push_str("-->");
        }
        // https://html.spec.whatwg.org/multipage/parsing.html#serialising-html-fragments
        NodeData::CDataSection { contents } => {
            let raw_text = parent.is_some_and(|name| {
                name.ns == html_namespace()
                    && matches!(
                        name.local.as_str(),
                        "style"
                            | "script"
                            | "xmp"
                            | "iframe"
                            | "noembed"
                            | "noscript"
                            | "noframes"
                            | "plaintext"
                    )
            });
            if raw_text {
                output.push_str(contents);
            } else {
                push_escaped_html_text(output, contents);
            }
        }
        NodeData::Doctype { name, .. } => {
            output.push_str("<!DOCTYPE ");
            output.push_str(name);
            output.push_str(">");
        }
        NodeData::ProcessingInstruction {
            target, contents, ..
        } => {
            output.push_str("<?");
            output.push_str(target);
            output.push_str(" ");
            output.push_str(contents);
            output.push_str("?>");
        }
        NodeData::Document(_) => {}
    }
}

pub(crate) fn serialize_html_children(doc: &BlitzDocument, parent: BlitzId) -> DomString {
    let base = &doc.base;
    let mut output = HtmlOutput(Vec::new());
    if let Some(node) = base.get_node(parent) {
        for child in &node.children.clone() {
            serialize_html_node(doc, *child, None, &mut output);
        }
    }
    output.finish()
}

pub(crate) fn serialize_html_outer(
    doc: &BlitzDocument,
    id: BlitzId,
    name: &QualName,
    attributes: &[blitz_dom::Attribute],
) -> DomString {
    let mut output = HtmlOutput(Vec::new());
    serialize_html_element(doc, id, name, attributes, &mut output);
    output.finish()
}

// https://html.spec.whatwg.org/multipage/parsing.html#serialising-html-fragments
pub(crate) fn splice_before_close_tag(shell: DomString, children: &DomString) -> DomString {
    let units = shell.units();
    let Some(end) = units.iter().rposition(|unit| *unit == u16::from(b'<')) else {
        return shell;
    };
    let mut merged = units[..end].to_vec();
    merged.extend(children.units().iter().copied());
    merged.extend_from_slice(&units[end..]);
    DomString::from_utf16(merged)
}

fn push_escaped_xml_text(output: &mut HtmlOutput, text: &str) {
    for unit in text.encode_utf16() {
        match unit {
            0x26 => output.push_str("&amp;"),
            0x3c => output.push_str("&lt;"),
            0x3e => output.push_str("&gt;"),
            _ => output.push_units(&[unit]),
        }
    }
}

fn push_escaped_xml_attribute(output: &mut HtmlOutput, value: &str) {
    for unit in value.encode_utf16() {
        match unit {
            0x26 => output.push_str("&amp;"),
            0x22 => output.push_str("&quot;"),
            0x3c => output.push_str("&lt;"),
            0x3e => output.push_str("&gt;"),
            0x09 => output.push_str("&#x9;"),
            0x0a => output.push_str("&#xA;"),
            0x0d => output.push_str("&#xD;"),
            _ => output.push_units(&[unit]),
        }
    }
}

// https://w3c.github.io/DOM-Parsing/#xml-serialization
pub(crate) fn serialize_xml_element(
    doc: &BlitzDocument,
    id: BlitzId,
    name: &QualName,
    attributes: &[blitz_dom::Attribute],
    output: &mut HtmlOutput,
) {
    let base = &doc.base;
    output.push_str("<");
    output.push_str(&qualified_name(name));
    for attribute in attributes {
        output.push_str(" ");
        output.push_str(&qualified_name(&attribute.name));
        output.push_str("=\"");
        push_escaped_xml_attribute(output, &attribute.value);
        output.push_str("\"");
    }
    let children: Vec<BlitzId> = base.get_node(id).map_or_default(|node| {
        if crate::dom::tree::is_template_tag(name) {
            crate::dom::tree::template_contents(base, id)
                .and_then(|fragment| {
                    base.get_node(fragment)
                        .map(|node| node.children.iter().copied().collect())
                })
                .unwrap_or_default()
        } else {
            node.children.iter().copied().collect()
        }
    });
    if children.is_empty() {
        output.push_str("/>");
        return;
    }
    output.push_str(">");
    for child in children {
        serialize_xml_node(doc, child, output);
    }
    output.push_str("</");
    output.push_str(&qualified_name(name));
    output.push_str(">");
}

pub(crate) fn serialize_xml_node(doc: &BlitzDocument, id: BlitzId, output: &mut HtmlOutput) {
    let base = &doc.base;
    let Some(node) = base.get_node(id) else {
        return;
    };
    match &node.data {
        NodeData::Document(_) | NodeData::AnonymousBlock(_) => {
            for child in &node.children.clone() {
                serialize_xml_node(doc, *child, output);
            }
        }
        NodeData::Element(element) => {
            let name = element.name.clone();
            let attributes = element.attrs.iter().cloned().collect::<Vec<_>>();
            serialize_xml_element(doc, id, &name, &attributes, output);
        }
        NodeData::Text(data) => push_escaped_xml_text(output, &data.content),
        NodeData::Comment { contents } => {
            output.push_str("<!--");
            output.push_str(contents);
            output.push_str("-->");
        }
        // https://w3c.github.io/DOM-Parsing/#xml-serializing-a-documenttype-node
        // https://w3c.github.io/DOM-Parsing/#xml-serializing-a-processinginstruction-node
        // https://w3c.github.io/DOM-Parsing/#xml-serializing-a-cdatasection-node
        NodeData::Doctype {
            name,
            public_id,
            system_id,
        } => {
            output.push_str("<!DOCTYPE ");
            output.push_str(name);
            if !public_id.is_empty() {
                output.push_str(" PUBLIC \"");
                output.push_str(public_id);
                output.push_str("\"");
                if !system_id.is_empty() {
                    output.push_str(" \"");
                    output.push_str(system_id);
                    output.push_str("\"");
                }
            } else if !system_id.is_empty() {
                output.push_str(" SYSTEM \"");
                output.push_str(system_id);
                output.push_str("\"");
            }
            output.push_str(">");
        }
        NodeData::ProcessingInstruction {
            target, contents, ..
        } => {
            output.push_str("<?");
            output.push_str(target);
            output.push_str(" ");
            output.push_str(contents);
            output.push_str("?>");
        }
        NodeData::CDataSection { contents } => {
            output.push_str("<![CDATA[");
            output.push_str(contents);
            output.push_str("]]>");
        }
        NodeData::Fragment { .. } | NodeData::ShadowRoot { .. } => {
            for child in node.children.clone() {
                serialize_xml_node(doc, child, output);
            }
        }
    }
}

pub(crate) fn serialize_xml_children(doc: &BlitzDocument, parent: BlitzId) -> DomString {
    let base = &doc.base;
    let mut output = HtmlOutput(Vec::new());
    if let Some(node) = base.get_node(parent) {
        for child in &node.children.clone() {
            serialize_xml_node(doc, *child, &mut output);
        }
    }
    output.finish()
}
