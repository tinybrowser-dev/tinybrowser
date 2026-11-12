//! Host calls that expose a document's parsed stylesheets as CSSOM rule lists.
//!
//! The style engine already keeps a `CSSRuleList` per `<style>` and
//! `<link rel=stylesheet>` owner
//! (<https://drafts.csswg.org/cssom/#concept-css-rule-list>). These functions
//! are the only bridge: `cssom.js` builds the live list objects on top.
//! Each call addresses a CSSOM sheet id, not the owner element, so a sheet
//! that has been replaced or removed stays readable
//! (<https://drafts.csswg.org/cssom/#remove-a-css-style-sheet>).

use blitz_dom::{CssRuleInfo, CssomError};
use rquickjs::{Array, Ctx, Exception, Object, Result, Value};

use super::{host_node_id, throw_dom, world};
use crate::dom::tree::NodeId;

pub(super) fn install(ctx: &Ctx<'_>) -> Result<()> {
    let bridge = crate::js::bridge::object(ctx)?;
    bridge.set(
        "__tb_sheet_owner_nodes",
        rquickjs::prelude::Func::from(sheet_owner_nodes),
    )?;
    bridge.set(
        "__tb_node_sheet_id",
        rquickjs::prelude::Func::from(node_sheet_id),
    )?;
    bridge.set(
        "__tb_sheet_origin_state",
        rquickjs::prelude::Func::from(sheet_origin_state),
    )?;
    bridge.set(
        "__tb_sheet_rule_count",
        rquickjs::prelude::Func::from(sheet_rule_count),
    )?;
    bridge.set(
        "__tb_sheet_rule_info",
        rquickjs::prelude::Func::from(sheet_rule_info),
    )?;
    bridge.set(
        "__tb_sheet_style_css_text",
        rquickjs::prelude::Func::from(sheet_style_css_text),
    )?;
    bridge.set(
        "__tb_sheet_style_get_property",
        rquickjs::prelude::Func::from(sheet_style_get_property),
    )?;
    bridge.set(
        "__tb_sheet_style_names",
        rquickjs::prelude::Func::from(sheet_style_names),
    )?;
    bridge.set(
        "__tb_sheet_style_set_property",
        rquickjs::prelude::Func::from(sheet_style_set_property),
    )?;
    bridge.set(
        "__tb_sheet_rule_set_selector_text",
        rquickjs::prelude::Func::from(sheet_rule_set_selector_text),
    )?;
    bridge.set(
        "__tb_sheet_insert_rule",
        rquickjs::prelude::Func::from(sheet_insert_rule),
    )?;
    bridge.set(
        "__tb_sheet_delete_rule",
        rquickjs::prelude::Func::from(sheet_delete_rule),
    )?;
    Ok(())
}

/// `path` is a comma-separated list of rule-list indexes, empty for the
/// stylesheet's own list. A segment that is not an index fails the whole path
/// (<https://drafts.csswg.org/cssom/#concept-css-rule-list>).
fn rule_path(path: &str) -> Option<Vec<usize>> {
    if path.is_empty() {
        return Some(Vec::new());
    }
    let mut indexes = Vec::new();
    for part in path.split(',') {
        if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        let index = part.parse::<u32>().ok()?;
        indexes.push(usize::try_from(index).unwrap_or(usize::MAX));
    }
    Some(indexes)
}

fn sheet_id(id: f64) -> Option<u64> {
    crate::js::bridge::enforce_range_u64(id).filter(|id| *id >= 1)
}

/// Web IDL `unsigned long` already applied on the JS side (`index >>> 0`), so
/// the number names a `usize` here. A value outside that range is a caller bug,
/// not index 0
/// (<https://webidl.spec.whatwg.org/#idl-unsigned-long>).
fn rule_index(ctx: &Ctx<'_>, index: f64) -> Result<usize> {
    crate::js::bridge::enforce_range_u32(index)
        .and_then(|index| usize::try_from(index).ok())
        .ok_or_else(|| Exception::throw_type(ctx, "invalid rule index"))
}

fn cssom_throw(ctx: &Ctx<'_>, err: CssomError) -> rquickjs::Error {
    throw_dom(ctx, err.dom_exception_name(), "CSSOM operation failed")
}

/// The owner nodes of `document`'s author sheets, in tree order. The JS
/// `StyleSheetList` reads each owner's `.sheet`, so the list itself stays live
/// (<https://drafts.csswg.org/cssom/#dom-document-stylesheets>).
fn sheet_owner_nodes<'js>(ctx: Ctx<'js>, document: Value<'js>) -> Result<Array<'js>> {
    let array = Array::new(ctx.clone())?;
    let Ok(world) = world(&ctx) else {
        return Ok(array);
    };
    let Some(id) = host_node_id(&ctx, &document) else {
        return Ok(array);
    };
    let world = world.borrow();
    let Some(parsed) = world.document(id) else {
        return Ok(array);
    };
    let document_id = parsed.id;
    let owners: Vec<NodeId> = parsed
        .document
        .base
        .stylesheet_owner_nodes()
        .map(|node| NodeId {
            document: document_id,
            node,
        })
        .collect();
    drop(parsed);
    for (index, id) in owners.into_iter().enumerate() {
        array.set(index, super::wrap_node(&ctx, id)?)?;
    }
    Ok(array)
}

fn node_sheet_id<'js>(ctx: Ctx<'js>, element: Value<'js>) -> f64 {
    let Ok(world) = world(&ctx) else {
        return -1.0;
    };
    let Some(id) = host_node_id(&ctx, &element) else {
        return -1.0;
    };
    let world = world.borrow();
    let Some(parsed) = world.document(id) else {
        return -1.0;
    };
    parsed
        .document
        .base
        .node_cssom_sheet_id(id.node)
        .map_or(-1.0, |sheet| {
            #[expect(
                clippy::cast_precision_loss,
                reason = "sheet ids stay inside the exact integer range of f64"
            )]
            {
                sheet as f64
            }
        })
}

fn sheet_origin_state<'js>(ctx: Ctx<'js>, element: Value<'js>, id: f64) -> f64 {
    let Ok(world) = world(&ctx) else {
        return -1.0;
    };
    let Some(node) = host_node_id(&ctx, &element) else {
        return -1.0;
    };
    let Some(sheet) = sheet_id(id) else {
        return -1.0;
    };
    let world = world.borrow();
    let Some(parsed) = world.document(node) else {
        return -1.0;
    };
    match parsed.document.base.cssom_sheet_origin_clean(sheet) {
        Some(true) => 1.0,
        Some(false) => 0.0,
        None => -1.0,
    }
}

fn with_path(path: &str) -> Option<Vec<usize>> {
    rule_path(path)
}

fn sheet_rule_count<'js>(ctx: Ctx<'js>, element: Value<'js>, id: f64, path: String) -> f64 {
    let Ok(world) = world(&ctx) else {
        return -1.0;
    };
    let Some(node) = host_node_id(&ctx, &element) else {
        return -1.0;
    };
    let Some(sheet) = sheet_id(id) else {
        return -1.0;
    };
    let Some(path) = with_path(&path) else {
        return -1.0;
    };
    let world = world.borrow();
    let Some(parsed) = world.document(node) else {
        return -1.0;
    };
    parsed
        .document
        .base
        .cssom_rule_count(sheet, &path)
        .map_or(-1.0, |count| {
            f64::from(u32::try_from(count).unwrap_or(u32::MAX))
        })
}

fn sheet_rule_info<'js>(
    ctx: Ctx<'js>,
    element: Value<'js>,
    id: f64,
    path: String,
) -> Result<Value<'js>> {
    let world = world(&ctx)?;
    let Some(node) = host_node_id(&ctx, &element) else {
        return Ok(Value::new_null(ctx));
    };
    let Some(sheet) = sheet_id(id) else {
        return Ok(Value::new_null(ctx));
    };
    let Some(path) = with_path(&path) else {
        return Ok(Value::new_null(ctx));
    };
    let world = world.borrow();
    let Some(parsed) = world.document(node) else {
        return Ok(Value::new_null(ctx.clone()));
    };
    let Some(info) = parsed.document.base.cssom_rule_info(sheet, &path) else {
        return Ok(Value::new_null(ctx.clone()));
    };
    rule_info_object(&ctx, info).map(Value::from)
}

fn rule_info_object<'js>(ctx: &Ctx<'js>, info: CssRuleInfo) -> Result<Object<'js>> {
    let object = Object::new(ctx.clone())?;
    object.set("interface", info.interface)?;
    object.set("type", info.rule_type)?;
    object.set("cssText", info.css_text)?;
    object.set("hasChildRules", info.has_child_rules)?;
    object.set("hasStyle", info.has_style)?;
    for (name, value) in info.attributes {
        object.set(name, value)?;
    }
    Ok(object)
}

fn sheet_style_css_text<'js>(ctx: Ctx<'js>, element: Value<'js>, id: f64, path: String) -> String {
    let Ok(world) = world(&ctx) else {
        return String::new();
    };
    let Some(node) = host_node_id(&ctx, &element) else {
        return String::new();
    };
    let Some(sheet) = sheet_id(id) else {
        return String::new();
    };
    let Some(path) = with_path(&path) else {
        return String::new();
    };
    let world = world.borrow();
    let Some(parsed) = world.document(node) else {
        return String::new();
    };
    parsed
        .document
        .base
        .cssom_rule_style_css_text(sheet, &path)
        .unwrap_or_default()
}

fn sheet_style_get_property<'js>(
    ctx: Ctx<'js>,
    element: Value<'js>,
    id: f64,
    path: String,
    name: String,
) -> Result<Value<'js>> {
    let world = world(&ctx)?;
    let Some(node) = host_node_id(&ctx, &element) else {
        return Ok(Value::new_null(ctx));
    };
    let Some(sheet) = sheet_id(id) else {
        return Ok(Value::new_null(ctx));
    };
    let Some(path) = with_path(&path) else {
        return Ok(Value::new_null(ctx));
    };
    let world = world.borrow();
    let Some(parsed) = world.document(node) else {
        return Ok(Value::new_null(ctx.clone()));
    };
    let Some((value, important)) = parsed
        .document
        .base
        .cssom_rule_style_get_property(sheet, &path, &name)
    else {
        return Ok(Value::new_null(ctx.clone()));
    };
    let object = Object::new(ctx.clone())?;
    object.set("value", value)?;
    object.set("important", important)?;
    Ok(Value::from(object))
}

fn sheet_style_names<'js>(
    ctx: Ctx<'js>,
    element: Value<'js>,
    id: f64,
    path: String,
) -> Result<Value<'js>> {
    let world = world(&ctx)?;
    let Some(node) = host_node_id(&ctx, &element) else {
        return Ok(Value::new_null(ctx));
    };
    let Some(sheet) = sheet_id(id) else {
        return Ok(Value::new_null(ctx));
    };
    let Some(path) = with_path(&path) else {
        return Ok(Value::new_null(ctx));
    };
    let world = world.borrow();
    let Some(parsed) = world.document(node) else {
        return Ok(Value::new_null(ctx.clone()));
    };
    let Some(names) = parsed
        .document
        .base
        .cssom_rule_style_property_names(sheet, &path)
    else {
        return Ok(Value::new_null(ctx.clone()));
    };
    let array = Array::new(ctx.clone())?;
    for (index, name) in names.iter().enumerate() {
        array.set(index, name.as_str())?;
    }
    Ok(Value::from(array))
}

fn sheet_style_set_property<'js>(
    ctx: Ctx<'js>,
    element: Value<'js>,
    id: f64,
    path: String,
    name: String,
    value: String,
    important: bool,
) -> Result<()> {
    let world = world(&ctx)?;
    let Some(node) = host_node_id(&ctx, &element) else {
        return Err(Exception::throw_type(&ctx, "not a stylesheet owner"));
    };
    let Some(sheet) = sheet_id(id) else {
        return Err(Exception::throw_type(&ctx, "not a stylesheet"));
    };
    let Some(path) = with_path(&path) else {
        return Err(Exception::throw_type(&ctx, "bad rule path"));
    };
    let world = world.borrow();
    let Some(mut parsed) = world.document_mut(node) else {
        return Err(Exception::throw_type(&ctx, "no document"));
    };
    parsed
        .document
        .base
        .cssom_rule_style_set_property(sheet, &path, &name, &value, important)
        .map_err(|err| cssom_throw(&ctx, err))
}

/// Replaces a style rule's selectors (`CSSStyleRule.selectorText` setter). An
/// invalid selector list leaves the rule unchanged and throws nothing
/// (<https://drafts.csswg.org/cssom/#dom-cssstylerule-selectortext>).
fn sheet_rule_set_selector_text<'js>(
    ctx: Ctx<'js>,
    element: Value<'js>,
    id: f64,
    path: String,
    selectors: String,
) -> Result<()> {
    let world = world(&ctx)?;
    let Some(node) = host_node_id(&ctx, &element) else {
        return Err(Exception::throw_type(&ctx, "not a stylesheet owner"));
    };
    let Some(sheet) = sheet_id(id) else {
        return Err(Exception::throw_type(&ctx, "not a stylesheet"));
    };
    let Some(path) = with_path(&path) else {
        return Err(Exception::throw_type(&ctx, "bad rule path"));
    };
    let world = world.borrow();
    let current = world
        .document(node)
        .and_then(|parsed| parsed.document.base.node_cssom_sheet_id(node.node));
    if current != Some(sheet) {
        return Err(throw_dom(&ctx, "InvalidStateError", "stylesheet is gone"));
    }
    let Some(mut parsed) = world.document_mut(node) else {
        return Err(Exception::throw_type(&ctx, "no document"));
    };
    parsed
        .document
        .base
        .stylesheet_rule_set_selector_text(node.node, &path, &selectors)
        .map_err(|err| cssom_throw(&ctx, err))
}

fn sheet_insert_rule<'js>(
    ctx: Ctx<'js>,
    element: Value<'js>,
    id: f64,
    path: String,
    rule: String,
    index: f64,
) -> Result<f64> {
    let world = world(&ctx)?;
    let Some(node) = host_node_id(&ctx, &element) else {
        return Err(Exception::throw_type(&ctx, "not a stylesheet owner"));
    };
    let Some(sheet) = sheet_id(id) else {
        return Err(Exception::throw_type(&ctx, "not a stylesheet"));
    };
    let Some(path) = with_path(&path) else {
        return Err(Exception::throw_type(&ctx, "bad rule path"));
    };
    let index = rule_index(&ctx, index)?;
    let world = world.borrow();
    let Some(mut parsed) = world.document_mut(node) else {
        return Err(Exception::throw_type(&ctx, "no document"));
    };
    match parsed
        .document
        .base
        .cssom_insert_rule(sheet, &path, &rule, index)
    {
        Ok(inserted) => Ok(f64::from(u32::try_from(inserted).unwrap_or(u32::MAX))),
        Err(err) => Err(cssom_throw(&ctx, err)),
    }
}

fn sheet_delete_rule<'js>(
    ctx: Ctx<'js>,
    element: Value<'js>,
    id: f64,
    path: String,
    index: f64,
) -> Result<()> {
    let world = world(&ctx)?;
    let Some(node) = host_node_id(&ctx, &element) else {
        return Err(Exception::throw_type(&ctx, "not a stylesheet owner"));
    };
    let Some(sheet) = sheet_id(id) else {
        return Err(Exception::throw_type(&ctx, "not a stylesheet"));
    };
    let Some(path) = with_path(&path) else {
        return Err(Exception::throw_type(&ctx, "bad rule path"));
    };
    let index = rule_index(&ctx, index)?;
    let world = world.borrow();
    let Some(mut parsed) = world.document_mut(node) else {
        return Err(Exception::throw_type(&ctx, "no document"));
    };
    parsed
        .document
        .base
        .cssom_delete_rule(sheet, &path, index)
        .map_err(|err| cssom_throw(&ctx, err))
}
