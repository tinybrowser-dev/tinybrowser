use std::borrow::Cow;

use super::node::insert_converted_node;
use super::{adopt_across_documents, throw_dom, world};

use crate::dom::tree::NodeId;

use rquickjs::{Ctx, Exception, Function, Object, Result, Value};

pub(crate) fn option_truthy<'js>(ctx: &Ctx<'js>, options: &Object<'js>, key: &str) -> Result<bool> {
    let value: Value = options.get(key)?;
    if value.is_undefined() || value.is_null() {
        return Ok(false);
    }
    to_boolean(ctx, &value)
}

// https://webidl.spec.whatwg.org/#es-boolean
pub(crate) fn to_boolean<'js>(ctx: &Ctx<'js>, value: &Value<'js>) -> Result<bool> {
    if let Some(boolean) = value.as_bool() {
        return Ok(boolean);
    }
    if value.is_null() || value.is_undefined() {
        return Ok(false);
    }
    if let Some(number) = value.as_number() {
        return Ok(number != 0.0 && !number.is_nan());
    }
    if let Some(string) = value.as_string() {
        return Ok(!string.to_string().is_ok_and(|string| string.is_empty()));
    }
    let world_rc = world(ctx)?;
    if let Some(boolean) = world_rc.borrow().pristine_boolean.clone() {
        let boolean: Function = boolean.restore(ctx)?;
        return boolean.call((value.clone(),));
    }
    let boolean: Function = ctx.globals().get("Boolean")?;
    boolean.call((value.clone(),))
}

pub(crate) struct OptString(pub(crate) Option<String>);

// https://webidl.spec.whatwg.org/#es-DOMString
pub(crate) struct WebIdlString(pub(crate) String);

// https://webidl.spec.whatwg.org/#LegacyNullToEmptyString
pub(crate) struct LegacyNullString(pub(crate) String);

pub(crate) struct WebIdlCodeUnits(pub(crate) crate::dom_string::DomString);

// https://webidl.spec.whatwg.org/#es-unsigned-long
pub(crate) struct WebIdlUnsignedLong(pub(crate) u32);

// https://webidl.spec.whatwg.org/#es-DOMString
pub(crate) fn webidl_to_string<'js>(ctx: &Ctx<'js>, value: Value<'js>) -> Result<String> {
    webidl_to_js_string(ctx, value)?.to_string()
}

// https://drafts.csswg.org/css-syntax/#consume-escaped-code-point
// https://infra.spec.whatwg.org/#javascript-string-convert
pub(crate) fn js_string_to_utf8(value: &rquickjs::String<'_>) -> Result<String> {
    if let Ok(text) = value.to_string() {
        Ok(text)
    } else {
        let units = value.to_utf16()?;
        Ok(char::decode_utf16(units)
            .filter_map(std::result::Result::<char, std::char::DecodeUtf16Error>::ok)
            .collect())
    }
}

// https://webidl.spec.whatwg.org/#es-DOMString
pub(crate) fn webidl_to_units<'js>(ctx: &Ctx<'js>, value: Value<'js>) -> Result<Vec<u16>> {
    webidl_to_js_string(ctx, value)?.to_utf16()
}

fn webidl_to_js_string<'js>(ctx: &Ctx<'js>, value: Value<'js>) -> Result<rquickjs::String<'js>> {
    let converted: rquickjs::Coerced<rquickjs::String> = rquickjs::FromJs::from_js(ctx, value)?;
    Ok(converted.0)
}

pub(crate) enum NodeOrString<'js> {
    Node(super::host::NodeReference),
    String(rquickjs::String<'js>),
}

// https://dom.spec.whatwg.org/#convert-nodes-into-a-node
pub(crate) fn convert_union_nodes_into_node<'js>(
    ctx: &Ctx<'js>,
    document: NodeId,
    nodes: Vec<NodeOrString<'js>>,
) -> Result<NodeId> {
    let mut pieces = Vec::with_capacity(nodes.len());
    for node in nodes {
        match node {
            NodeOrString::Node(reference) => {
                let Some(id) = reference.tree() else {
                    return Err(throw_dom(
                        ctx,
                        "HierarchyRequestError",
                        "attributes cannot be inserted",
                    ));
                };
                pieces.push(Piece::Node(id));
            }
            NodeOrString::String(string) => {
                pieces.push(Piece::Text(string.to_string()?));
            }
        }
    }
    assemble_nodes_into_node(ctx, document, pieces)
}

enum Piece {
    Node(NodeId),
    Text(String),
}

fn assemble_nodes_into_node(
    ctx: &Ctx<'_>,
    document: NodeId,
    mut pieces: Vec<Piece>,
) -> Result<NodeId> {
    for piece in &mut pieces {
        if let Piece::Node(id) = piece {
            *id = adopt_across_documents(ctx, document, *id)?;
        }
    }
    if pieces.len() == 1 {
        return match pieces.pop() {
            Some(Piece::Node(id)) => Ok(id),
            Some(Piece::Text(text)) => {
                let world = world(ctx)?;
                let world = world.borrow();
                let Some(mut parsed) = world.document_mut(document) else {
                    return Err(Exception::throw_type(ctx, "no document"));
                };
                let node = parsed.document.base.mutate().create_text_node(&text);
                Ok(NodeId {
                    document: document.document,
                    node,
                })
            }
            None => Err(Exception::throw_internal(ctx, "empty node list")),
        };
    }
    let fragment = {
        let world = world(ctx)?;
        let world = world.borrow();
        let Some(mut parsed) = world.document_mut(document) else {
            return Err(Exception::throw_type(ctx, "no document"));
        };
        NodeId {
            document: document.document,
            node: parsed.document.create_fragment(),
        }
    };
    // https://dom.spec.whatwg.org/#convert-nodes-into-a-node
    for piece in pieces {
        let id = match piece {
            Piece::Node(id) => id,
            Piece::Text(text) => {
                let world = world(ctx)?;
                let world = world.borrow();
                let Some(mut parsed) = world.document_mut(document) else {
                    return Err(Exception::throw_type(ctx, "no document"));
                };
                let node = parsed.document.base.mutate().create_text_node(&text);
                NodeId {
                    document: document.document,
                    node,
                }
            }
        };
        insert_converted_node(ctx, fragment, id)?;
    }
    Ok(fragment)
}

// https://dom.spec.whatwg.org/#scope-match-a-selectors-string
pub(crate) fn select_error(ctx: &Ctx<'_>, err: &style_traits::ParseError) -> rquickjs::Error {
    throw_dom(ctx, "SyntaxError", &format!("{err:?}"))
}

// https://drafts.csswg.org/css-pseudo/#selectordef-first-line
// https://drafts.csswg.org/selectors-4/#pseudo-element-selectors
// https://dom.spec.whatwg.org/#scope-match-a-selectors-string
pub(crate) fn selector_matching_elements(selectors: &str) -> Cow<'_, str> {
    if !contains_ascii_ignore_case(selectors, b"first-line") {
        return Cow::Borrowed(selectors);
    }
    let mut out = String::new();
    let mut index = 0;
    let mut changed = false;
    while index < selectors.len() {
        if let Some(end) = skip_selector_literal(selectors, index) {
            if changed {
                out.push_str(&selectors[index..end]);
            }
            index = end;
            continue;
        }
        if let Some(end) = first_line_pseudo(selectors, index) {
            if !changed {
                out.push_str(&selectors[..index]);
                changed = true;
            }
            out.push_str("::before");
            index = end;
            continue;
        }
        let Some(next) = selectors[index..].chars().next() else {
            break;
        };
        if changed {
            out.push(next);
        }
        index += next.len_utf8();
    }
    if changed {
        Cow::Owned(out)
    } else {
        Cow::Borrowed(selectors)
    }
}

fn contains_ascii_ignore_case(haystack: &str, needle: &[u8]) -> bool {
    haystack
        .as_bytes()
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle))
}

fn skip_selector_literal(selectors: &str, index: usize) -> Option<usize> {
    let rest = &selectors[index..];
    if rest.starts_with("/*") {
        return rest.find("*/").map(|end| index + end + 2);
    }
    match rest.chars().next()? {
        quote @ ('"' | '\'') => Some(end_of_selector_string(selectors, index, quote)),
        '\\' => {
            let mut cursor = index + 1;
            if let Some(escaped) = selectors[cursor..].chars().next() {
                cursor += escaped.len_utf8();
            }
            Some(cursor)
        }
        _ => None,
    }
}

fn end_of_selector_string(selectors: &str, index: usize, quote: char) -> usize {
    let mut cursor = index + quote.len_utf8();
    while cursor < selectors.len() {
        let Some(character) = selectors[cursor..].chars().next() else {
            break;
        };
        cursor += character.len_utf8();
        if character == '\\' {
            if let Some(escaped) = selectors[cursor..].chars().next() {
                cursor += escaped.len_utf8();
            }
            continue;
        }
        if character == quote {
            return cursor;
        }
    }
    selectors.len()
}

fn first_line_pseudo(selectors: &str, index: usize) -> Option<usize> {
    let rest = &selectors.as_bytes()[index..];
    if !rest.starts_with(b":") {
        return None;
    }
    let colon_len = if rest.starts_with(b"::") { 2 } else { 1 };
    let name = rest.get(colon_len..)?;
    let token = b"first-line";
    if name.len() < token.len() || !name[..token.len()].eq_ignore_ascii_case(token) {
        return None;
    }
    let after = index + colon_len + token.len();
    let continues = selectors[after..]
        .chars()
        .next()
        .is_some_and(is_css_name_continue);
    (!continues).then_some(after)
}

// https://drafts.csswg.org/css-syntax/#ident-token-diagram
fn is_css_name_continue(character: char) -> bool {
    character == '\\' || character == '-' || character == '_' || character.is_alphanumeric()
}

// https://webidl.spec.whatwg.org/#abstract-opdef-converttoint
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "`rem_euclid` returns a value in [0, 2^32), so the cast is exact and non-negative"
)]
pub(crate) fn webidl_unsigned_long(number: f64) -> u32 {
    if !number.is_finite() || number == 0.0 {
        return 0;
    }
    let modulo = number.trunc().rem_euclid(4_294_967_296.0);
    modulo as u32
}
