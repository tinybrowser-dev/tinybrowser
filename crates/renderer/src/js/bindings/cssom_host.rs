// https://drafts.csswg.org/cssom/#concept-css-rule-list
// https://drafts.csswg.org/cssom/#remove-a-css-style-sheet

use blitz_dom::{CssRuleInfo, CssomError};
use rquickjs::{Array, Ctx, Exception, Object, Result, Value};

use super::{host_node_id, throw_dom, world};
use crate::dom::tree::NodeId;

pub(super) fn install(ctx: &Ctx<'_>) -> Result<()> {
    let bridge = crate::js::bridge::object(ctx)?;
    install_style_functions(&bridge)?;
    install_sheet_functions(&bridge)
}

fn install_style_functions(bridge: &Object<'_>) -> Result<()> {
    bridge.set(
        "__tb_css_supports_declaration",
        rquickjs::prelude::Func::from(css_supports_declaration),
    )?;
    bridge.set(
        "__tb_css_supports_condition",
        rquickjs::prelude::Func::from(css_supports_condition),
    )?;
    bridge.set(
        "__tb_style_property_supported",
        rquickjs::prelude::Func::from(style_property_supported),
    )?;
    bridge.set(
        "__tb_inline_style_css_text",
        rquickjs::prelude::Func::from(inline_style_css_text),
    )?;
    bridge.set(
        "__tb_inline_style_property_names",
        rquickjs::prelude::Func::from(inline_style_property_names),
    )?;
    bridge.set(
        "__tb_inline_style_get_property",
        rquickjs::prelude::Func::from(inline_style_get_property),
    )?;
    bridge.set(
        "__tb_inline_style_set_css_text",
        rquickjs::prelude::Func::from(inline_style_set_css_text),
    )?;
    bridge.set(
        "__tb_inline_style_set_property",
        rquickjs::prelude::Func::from(inline_style_set_property),
    )?;
    bridge.set(
        "__tb_inline_style_remove_property",
        rquickjs::prelude::Func::from(inline_style_remove_property),
    )?;
    Ok(())
}

fn install_sheet_functions(bridge: &Object<'_>) -> Result<()> {
    bridge.set(
        "__tb_sheet_owner_nodes",
        rquickjs::prelude::Func::from(sheet_owner_nodes),
    )?;
    bridge.set(
        "__tb_node_sheet_id",
        rquickjs::prelude::Func::from(node_sheet_id),
    )?;
    bridge.set(
        "__tb_sheet_generation",
        rquickjs::prelude::Func::from(sheet_generation),
    )?;
    bridge.set(
        "__tb_sheet_origin_state",
        rquickjs::prelude::Func::from(sheet_origin_state),
    )?;
    bridge.set(
        "__tb_sheet_disabled",
        rquickjs::prelude::Func::from(sheet_disabled),
    )?;
    bridge.set(
        "__tb_set_sheet_disabled",
        rquickjs::prelude::Func::from(set_sheet_disabled),
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
        "__tb_sheet_style_set_css_text",
        rquickjs::prelude::Func::from(sheet_style_set_css_text),
    )?;
    bridge.set(
        "__tb_sheet_rule_set_selector_text",
        rquickjs::prelude::Func::from(sheet_rule_set_selector_text),
    )?;
    bridge.set(
        "__tb_sheet_rule_set_media",
        rquickjs::prelude::Func::from(sheet_rule_set_media),
    )?;
    bridge.set(
        "__tb_sheet_rule_set_key_text",
        rquickjs::prelude::Func::from(sheet_rule_set_key_text),
    )?;
    bridge.set(
        "__tb_sheet_rule_set_name",
        rquickjs::prelude::Func::from(sheet_rule_set_name),
    )?;
    bridge.set(
        "__tb_sheet_find_keyframe",
        rquickjs::prelude::Func::from(sheet_find_keyframe),
    )?;
    bridge.set(
        "__tb_sheet_import_sheet",
        rquickjs::prelude::Func::from(sheet_import_sheet),
    )?;
    bridge.set(
        "__tb_sheet_insert_rule",
        rquickjs::prelude::Func::from(sheet_insert_rule),
    )?;
    bridge.set(
        "__tb_sheet_delete_rule",
        rquickjs::prelude::Func::from(sheet_delete_rule),
    )?;
    install_constructable_sheet_functions(bridge)
}

// https://drafts.csswg.org/cssom/#the-cssstylesheet-interface
fn install_constructable_sheet_functions(bridge: &Object<'_>) -> Result<()> {
    bridge.set(
        "__tb_create_sheet",
        rquickjs::prelude::Func::from(create_sheet),
    )?;
    bridge.set(
        "__tb_replace_sheet",
        rquickjs::prelude::Func::from(replace_sheet),
    )?;
    bridge.set(
        "__tb_sheet_media_text",
        rquickjs::prelude::Func::from(sheet_media_text),
    )?;
    bridge.set(
        "__tb_set_sheet_media_text",
        rquickjs::prelude::Func::from(set_sheet_media_text),
    )?;
    bridge.set(
        "__tb_cssom_sheet_disabled",
        rquickjs::prelude::Func::from(cssom_sheet_disabled),
    )?;
    bridge.set(
        "__tb_set_cssom_sheet_disabled",
        rquickjs::prelude::Func::from(set_cssom_sheet_disabled),
    )?;
    bridge.set(
        "__tb_set_adopted_sheets",
        rquickjs::prelude::Func::from(set_adopted_sheets),
    )?;
    Ok(())
}

// https://drafts.csswg.org/cssom/#concept-css-rule-list
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

// https://webidl.spec.whatwg.org/#idl-unsigned-long
fn rule_index(ctx: &Ctx<'_>, index: f64) -> Result<usize> {
    crate::js::bridge::enforce_range_u32(index)
        .and_then(|index| usize::try_from(index).ok())
        .ok_or_else(|| Exception::throw_type(ctx, "invalid rule index"))
}

fn cssom_throw(ctx: &Ctx<'_>, err: CssomError) -> rquickjs::Error {
    throw_dom(ctx, err.dom_exception_name(), "CSSOM operation failed")
}

// https://drafts.csswg.org/cssom/#dom-document-stylesheets
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
    let is_shadow = parsed
        .document
        .base
        .get_node(id.node)
        .is_some_and(|node| matches!(node.data, blitz_dom::NodeData::ShadowRoot { .. }));
    let owners: Vec<NodeId> = if is_shadow {
        parsed
            .document
            .base
            .shadow_stylesheet_owner_nodes(id.node)
            .into_iter()
            .map(|node| NodeId {
                document: document_id,
                node,
            })
            .collect()
    } else {
        parsed
            .document
            .base
            .stylesheet_owner_nodes()
            .map(|node| NodeId {
                document: document_id,
                node,
            })
            .collect()
    };
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

fn sheet_generation(ctx: Ctx<'_>) -> f64 {
    let Ok(world) = world(&ctx) else {
        return -1.0;
    };
    let world = world.borrow();
    let Some(parsed) = world.main_document() else {
        return -1.0;
    };
    let generation = parsed.document.base.stylesheet_generation();
    #[expect(
        clippy::cast_precision_loss,
        reason = "the generation counter stays inside the exact integer range of f64"
    )]
    {
        generation as f64
    }
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

// https://drafts.csswg.org/cssom/#dom-stylesheet-disabled
fn sheet_disabled<'js>(ctx: Ctx<'js>, element: Value<'js>, id: f64) -> bool {
    let Ok(world) = world(&ctx) else {
        return false;
    };
    let Some(node) = host_node_id(&ctx, &element) else {
        return false;
    };
    let Some(sheet) = sheet_id(id) else {
        return false;
    };
    let world = world.borrow();
    let Some(parsed) = world.document(node) else {
        return false;
    };
    // https://drafts.csswg.org/cssom/#remove-a-css-style-sheet
    if parsed.document.base.node_cssom_sheet_id(node.node) != Some(sheet) {
        return false;
    }
    parsed.document.base.stylesheet_disabled(node.node)
}

// https://drafts.csswg.org/cssom/#dom-stylesheet-disabled
fn set_sheet_disabled<'js>(ctx: Ctx<'js>, element: Value<'js>, id: f64, disabled: bool) -> bool {
    let Ok(world) = world(&ctx) else {
        return false;
    };
    let Some(node) = host_node_id(&ctx, &element) else {
        return false;
    };
    let Some(sheet) = sheet_id(id) else {
        return false;
    };
    let world = world.borrow_mut();
    let Some(mut parsed) = world.document_mut(node) else {
        return false;
    };
    if parsed.document.base.node_cssom_sheet_id(node.node) != Some(sheet) {
        return false;
    }
    parsed
        .document
        .base
        .set_stylesheet_disabled(node.node, disabled)
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
    object.set("type", cssom_rule_type(info.interface, info.rule_type))?;
    object.set("cssText", info.css_text)?;
    object.set("hasChildRules", info.has_child_rules)?;
    object.set("hasStyle", info.has_style)?;
    for (name, value) in info.attributes {
        object.set(name, value)?;
    }
    Ok(object)
}

// https://drafts.csswg.org/cssom/#dom-cssrule-type
fn cssom_rule_type(interface: &str, engine_type: u16) -> u16 {
    match interface {
        "CSSStyleRule" => 1,
        "CSSImportRule" => 3,
        "CSSMediaRule" => 4,
        "CSSFontFaceRule" => 5,
        "CSSPageRule" => 6,
        "CSSKeyframesRule" => 7,
        "CSSKeyframeRule" => 8,
        "CSSNamespaceRule" => 10,
        "CSSCounterStyleRule" => 11,
        "CSSSupportsRule" => 12,
        "CSSFontFeatureValuesRule" => 14,
        _ if matches!(engine_type, 1 | 3..=8 | 10..=12 | 14) => engine_type,
        _ => 0,
    }
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

// https://drafts.csswg.org/cssom/#dom-cssstyledeclaration-csstext
fn sheet_style_set_css_text<'js>(
    ctx: Ctx<'js>,
    element: Value<'js>,
    id: f64,
    path: String,
    text: String,
) -> Result<()> {
    let world = world(&ctx)?;
    let Some(node) = host_node_id(&ctx, &element) else {
        return Ok(());
    };
    let Some(sheet) = sheet_id(id) else {
        return Ok(());
    };
    let Some(path) = with_path(&path) else {
        return Ok(());
    };
    let world = world.borrow();
    let Some(mut parsed) = world.document_mut(node) else {
        return Ok(());
    };
    // https://drafts.csswg.org/cssom/#dom-cssstyledeclaration-csstext
    let _ = parsed
        .document
        .base
        .cssom_rule_style_set_css_text(sheet, &path, &text);
    Ok(())
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

// https://drafts.csswg.org/cssom/#dom-cssstylerule-selectortext
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

// https://drafts.csswg.org/cssom/#dom-medialist-mediatext
fn sheet_rule_set_media<'js>(
    ctx: Ctx<'js>,
    element: Value<'js>,
    id: f64,
    path: String,
    text: String,
) -> Result<()> {
    let world = world(&ctx)?;
    let Some(node) = host_node_id(&ctx, &element) else {
        return Ok(());
    };
    let Some(sheet) = sheet_id(id) else {
        return Ok(());
    };
    let Some(path) = with_path(&path) else {
        return Ok(());
    };
    let world = world.borrow();
    let Some(mut parsed) = world.document_mut(node) else {
        return Ok(());
    };
    let _ = parsed
        .document
        .base
        .cssom_rule_set_media(sheet, &path, &text);
    Ok(())
}

// https://drafts.csswg.org/css-animations-1/#dom-csskeyframerule-keytext
fn sheet_rule_set_key_text<'js>(
    ctx: Ctx<'js>,
    element: Value<'js>,
    id: f64,
    path: String,
    text: String,
) -> Result<()> {
    let world = world(&ctx)?;
    let Some(node) = host_node_id(&ctx, &element) else {
        return Ok(());
    };
    let Some(sheet) = sheet_id(id) else {
        return Ok(());
    };
    let Some(path) = with_path(&path) else {
        return Ok(());
    };
    let world = world.borrow();
    let Some(mut parsed) = world.document_mut(node) else {
        return Ok(());
    };
    parsed
        .document
        .base
        .cssom_rule_set_key_text(sheet, &path, &text)
        .map_err(|err| cssom_throw(&ctx, err))
}

// https://drafts.csswg.org/css-animations-1/#dom-csskeyframesrule-name
fn sheet_rule_set_name<'js>(
    ctx: Ctx<'js>,
    element: Value<'js>,
    id: f64,
    path: String,
    name: String,
) -> Result<()> {
    let world = world(&ctx)?;
    let Some(node) = host_node_id(&ctx, &element) else {
        return Ok(());
    };
    let Some(sheet) = sheet_id(id) else {
        return Ok(());
    };
    let Some(path) = with_path(&path) else {
        return Ok(());
    };
    let world = world.borrow();
    let Some(mut parsed) = world.document_mut(node) else {
        return Ok(());
    };
    let _ = parsed
        .document
        .base
        .cssom_rule_set_name(sheet, &path, &name);
    Ok(())
}

// https://drafts.csswg.org/css-animations-1/#dom-csskeyframesrule-findrule
fn sheet_find_keyframe<'js>(
    ctx: Ctx<'js>,
    element: Value<'js>,
    id: f64,
    path: String,
    select: String,
) -> i32 {
    let Ok(world) = world(&ctx) else {
        return -1;
    };
    let Some(node) = host_node_id(&ctx, &element) else {
        return -1;
    };
    let Some(sheet) = sheet_id(id) else {
        return -1;
    };
    let Some(path) = with_path(&path) else {
        return -1;
    };
    let world = world.borrow();
    let Some(parsed) = world.document(node) else {
        return -1;
    };
    match parsed
        .document
        .base
        .cssom_find_keyframe(sheet, &path, &select)
    {
        Some(index) => i32::try_from(index).unwrap_or(-1),
        None => -1,
    }
}

// https://drafts.csswg.org/cssom/#dom-document-adoptedstylesheets
fn set_adopted_sheets<'js>(ctx: Ctx<'js>, address: Value<'js>, ids: Array<'js>) -> bool {
    let Ok(world) = world(&ctx) else {
        return false;
    };
    let Some(node) = host_node_id(&ctx, &address) else {
        return false;
    };
    let mut sheets = Vec::with_capacity(ids.len());
    for index in 0..ids.len() {
        let Ok(value) = ids.get::<f64>(index) else {
            return false;
        };
        let Some(id) = sheet_id(value) else {
            return false;
        };
        sheets.push(id);
    }
    let world = world.borrow_mut();
    let Some(mut parsed) = world.document_mut(node) else {
        return false;
    };
    let is_shadow = parsed
        .document
        .base
        .get_node(node.node)
        .is_some_and(|entry| matches!(entry.data, blitz_dom::NodeData::ShadowRoot { .. }));
    if is_shadow {
        parsed
            .document
            .base
            .set_shadow_adopted_stylesheets(node.node, &sheets)
    } else {
        parsed.document.base.set_adopted_stylesheets(&sheets)
    }
}

// https://drafts.csswg.org/cssom/#dom-cssstylesheet-cssstylesheet
fn create_sheet<'js>(
    ctx: Ctx<'js>,
    address: Value<'js>,
    css: String,
    media: String,
    base_url: Option<String>,
    disabled: bool,
) -> f64 {
    let Ok(world) = world(&ctx) else {
        return -1.0;
    };
    let Some(node) = host_node_id(&ctx, &address) else {
        return -1.0;
    };
    let base_url = match base_url {
        Some(url) => match url::Url::parse(&url) {
            Ok(url) => Some(url),
            Err(_) => return -1.0,
        },
        None => None,
    };
    let world = world.borrow_mut();
    let Some(mut parsed) = world.document_mut(node) else {
        return -1.0;
    };
    let id = parsed
        .document
        .base
        .create_constructable_sheet(&css, &media, base_url.as_ref());
    if disabled {
        parsed.document.base.set_cssom_sheet_disabled(id, true);
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "sheet ids stay inside the exact integer range of f64"
    )]
    {
        id as f64
    }
}

// https://drafts.csswg.org/cssom/#dom-cssstylesheet-replacesync
fn replace_sheet<'js>(
    ctx: Ctx<'js>,
    address: Value<'js>,
    id: f64,
    css: String,
    base_url: Option<String>,
) -> bool {
    let Ok(world) = world(&ctx) else {
        return false;
    };
    let Some(node) = host_node_id(&ctx, &address) else {
        return false;
    };
    let Some(sheet) = sheet_id(id) else {
        return false;
    };
    let base_url = match base_url {
        Some(url) => match url::Url::parse(&url) {
            Ok(url) => Some(url),
            Err(_) => return false,
        },
        None => None,
    };
    let world = world.borrow_mut();
    let Some(mut parsed) = world.document_mut(node) else {
        return false;
    };
    parsed
        .document
        .base
        .cssom_replace_sheet(sheet, &css, base_url.as_ref())
}

// https://drafts.csswg.org/cssom/#dom-stylesheet-media
fn sheet_media_text<'js>(ctx: Ctx<'js>, address: Value<'js>, id: f64) -> String {
    let Ok(world) = world(&ctx) else {
        return String::new();
    };
    let Some(node) = host_node_id(&ctx, &address) else {
        return String::new();
    };
    let Some(sheet) = sheet_id(id) else {
        return String::new();
    };
    let world = world.borrow();
    let Some(parsed) = world.document(node) else {
        return String::new();
    };
    parsed
        .document
        .base
        .cssom_sheet_media_text(sheet)
        .unwrap_or_default()
}

// https://drafts.csswg.org/cssom/#dom-medialist-mediatext
fn set_sheet_media_text<'js>(ctx: Ctx<'js>, address: Value<'js>, id: f64, text: String) -> bool {
    let Ok(world) = world(&ctx) else {
        return false;
    };
    let Some(node) = host_node_id(&ctx, &address) else {
        return false;
    };
    let Some(sheet) = sheet_id(id) else {
        return false;
    };
    let world = world.borrow_mut();
    let Some(mut parsed) = world.document_mut(node) else {
        return false;
    };
    parsed
        .document
        .base
        .set_cssom_sheet_media_text(sheet, &text)
}

// https://drafts.csswg.org/cssom/#dom-stylesheet-disabled
fn cssom_sheet_disabled<'js>(ctx: Ctx<'js>, address: Value<'js>, id: f64) -> bool {
    let Ok(world) = world(&ctx) else {
        return false;
    };
    let Some(node) = host_node_id(&ctx, &address) else {
        return false;
    };
    let Some(sheet) = sheet_id(id) else {
        return false;
    };
    let world = world.borrow();
    let Some(parsed) = world.document(node) else {
        return false;
    };
    parsed
        .document
        .base
        .cssom_sheet_disabled(sheet)
        .unwrap_or(false)
}

// https://drafts.csswg.org/cssom/#dom-stylesheet-disabled
fn set_cssom_sheet_disabled<'js>(
    ctx: Ctx<'js>,
    address: Value<'js>,
    id: f64,
    disabled: bool,
) -> bool {
    let Ok(world) = world(&ctx) else {
        return false;
    };
    let Some(node) = host_node_id(&ctx, &address) else {
        return false;
    };
    let Some(sheet) = sheet_id(id) else {
        return false;
    };
    let world = world.borrow_mut();
    let Some(mut parsed) = world.document_mut(node) else {
        return false;
    };
    parsed
        .document
        .base
        .set_cssom_sheet_disabled(sheet, disabled)
}

// https://drafts.csswg.org/cssom/#dom-cssimportrule-stylesheet
fn sheet_import_sheet<'js>(ctx: Ctx<'js>, element: Value<'js>, id: f64, path: String) -> f64 {
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
    let Some(mut parsed) = world.document_mut(node) else {
        return -1.0;
    };
    parsed
        .document
        .base
        .cssom_import_sheet(sheet, &path)
        .map_or(-1.0, |id| {
            #[expect(
                clippy::cast_precision_loss,
                reason = "sheet ids stay inside the exact integer range of f64"
            )]
            {
                id as f64
            }
        })
}

// https://drafts.csswg.org/cssom/#dom-cssstyledeclaration-csstext
fn inline_style_css_text<'js>(ctx: Ctx<'js>, element: Value<'js>) -> Result<Value<'js>> {
    let world = world(&ctx)?;
    let Some(node) = host_node_id(&ctx, &element) else {
        return Ok(Value::new_null(ctx));
    };
    let world = world.borrow();
    let Some(parsed) = world.document(node) else {
        return Ok(Value::new_null(ctx.clone()));
    };
    match parsed.document.base.inline_style_css_text(node.node) {
        Some(css) => Ok(Value::from(rquickjs::String::from_str(ctx, &css)?)),
        None => Ok(Value::new_null(ctx)),
    }
}

// https://drafts.csswg.org/cssom/#dom-cssstyledeclaration-item
fn inline_style_property_names<'js>(ctx: Ctx<'js>, element: Value<'js>) -> Result<Value<'js>> {
    let world = world(&ctx)?;
    let Some(node) = host_node_id(&ctx, &element) else {
        return Ok(Value::new_null(ctx));
    };
    let world = world.borrow();
    let Some(parsed) = world.document(node) else {
        return Ok(Value::new_null(ctx.clone()));
    };
    let Some(names) = parsed.document.base.inline_style_property_names(node.node) else {
        return Ok(Value::new_null(ctx.clone()));
    };
    let array = Array::new(ctx.clone())?;
    for (index, name) in names.iter().enumerate() {
        array.set(index, name.as_str())?;
    }
    Ok(Value::from(array))
}

// https://drafts.csswg.org/cssom/#dom-cssstyledeclaration-getpropertyvalue
fn inline_style_get_property<'js>(
    ctx: Ctx<'js>,
    element: Value<'js>,
    name: String,
) -> Result<Value<'js>> {
    let world = world(&ctx)?;
    let Some(node) = host_node_id(&ctx, &element) else {
        return Ok(Value::new_null(ctx));
    };
    let world = world.borrow();
    let Some(parsed) = world.document(node) else {
        return Ok(Value::new_null(ctx.clone()));
    };
    let Some((value, important)) = parsed
        .document
        .base
        .inline_style_get_property(node.node, &name)
    else {
        return Ok(Value::new_null(ctx.clone()));
    };
    let object = Object::new(ctx.clone())?;
    object.set("value", value)?;
    object.set("important", important)?;
    Ok(Value::from(object))
}

// https://drafts.csswg.org/cssom/#dom-cssstyledeclaration-csstext
fn inline_style_set_css_text<'js>(ctx: Ctx<'js>, element: Value<'js>, text: String) -> Result<()> {
    let world = world(&ctx)?;
    let Some(node) = host_node_id(&ctx, &element) else {
        return Ok(());
    };
    let world = world.borrow();
    let Some(mut parsed) = world.document_mut(node) else {
        return Ok(());
    };
    parsed
        .document
        .base
        .set_inline_style_css_text(node.node, &text);
    Ok(())
}

// https://drafts.csswg.org/cssom/#dom-cssstyledeclaration-setproperty
fn inline_style_set_property<'js>(
    ctx: Ctx<'js>,
    element: Value<'js>,
    name: String,
    value: String,
    important: bool,
) -> Result<bool> {
    let world = world(&ctx)?;
    let Some(node) = host_node_id(&ctx, &element) else {
        return Ok(false);
    };
    let world = world.borrow();
    let Some(mut parsed) = world.document_mut(node) else {
        return Ok(false);
    };
    let base = &mut parsed.document.base;
    let before = base.inline_style_css_text(node.node);
    base.set_style_property_with_importance(node.node, &name, &value, important);
    Ok(before != base.inline_style_css_text(node.node))
}

// https://drafts.csswg.org/cssom/#dom-cssstyledeclaration-removeproperty
fn inline_style_remove_property<'js>(
    ctx: Ctx<'js>,
    element: Value<'js>,
    name: String,
) -> Result<bool> {
    let world = world(&ctx)?;
    let Some(node) = host_node_id(&ctx, &element) else {
        return Ok(false);
    };
    let world = world.borrow();
    let Some(mut parsed) = world.document_mut(node) else {
        return Ok(false);
    };
    let base = &mut parsed.document.base;
    let before = base.inline_style_css_text(node.node);
    base.remove_style_property(node.node, &name);
    Ok(before != base.inline_style_css_text(node.node))
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

// https://drafts.csswg.org/css-conditional-3/#dom-css-supports
fn css_supports_declaration(property: String, value: String) -> bool {
    crate::css::supports::supports_declaration(&property, &value)
}

// https://drafts.csswg.org/css-conditional-3/#dom-css-supports
fn css_supports_condition(text: String) -> bool {
    crate::css::supports::supports_condition(&text)
}

// https://drafts.csswg.org/cssom/#css-property-to-idl-attribute
fn style_property_supported(name: String) -> bool {
    crate::css::supports::property_supported(&name)
}
