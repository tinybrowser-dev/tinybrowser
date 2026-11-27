// https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#constructing-form-data-set
// https://html.spec.whatwg.org/multipage/input.html#concept-fe-value
// https://html.spec.whatwg.org/multipage/form-elements.html#concept-option-selectedness

use super::{focus, host_node_id, owning_world, world, world_for_node};
use crate::dom::forms::{self, PendingEntry, encode_form_bytes, urlencode_form};
use crate::dom::input::{
    InputType, NumberBufferState, number_buffer_state, step_mismatch, value_as_number,
    value_in_units,
};
use crate::dom::tree::{NodeId, is_html_element};
use crate::js::{FrameNavigation, World};
use rquickjs::prelude::Opt;
use rquickjs::{Array, Ctx, Exception, IntoJs, Object, Persistent, Result, TypedArray, Value};
use std::cell::RefCell;
use std::rc::Rc;

fn optional<'js, T: IntoJs<'js>>(value: Option<T>, ctx: &Ctx<'js>) -> Result<Value<'js>> {
    match value {
        Some(value) => value.into_js(ctx),
        None => Ok(Value::new_null(ctx.clone())),
    }
}

pub(super) fn install(_ctx: &Ctx<'_>, globals: &Object<'_>) -> Result<()> {
    globals.set(
        "__tbFormEntries",
        rquickjs::prelude::Func::from(form_entries),
    )?;
    globals.set(
        "__tbFormNavigate",
        rquickjs::prelude::Func::from(form_navigate),
    )?;
    globals.set("__tbFormPlan", rquickjs::prelude::Func::from(form_plan))?;
    globals.set(
        "__tbSetInputFiles",
        rquickjs::prelude::Func::from(set_input_files),
    )?;
    globals.set(
        "__tbInputFileList",
        rquickjs::prelude::Func::from(input_file_list),
    )?;
    globals.set(
        "__tbInputFileObjects",
        rquickjs::prelude::Func::from(input_file_objects),
    )?;
    globals.set(
        "__tbEncodeFormBytes",
        rquickjs::prelude::Func::from(encode_form_bytes_js),
    )?;
    globals.set(
        "__tbUrlEncodeForm",
        rquickjs::prelude::Func::from(|text: String, label: String| urlencode_form(&text, &label)),
    )?;
    globals.set(
        "__tbSetOptionSelectedness",
        rquickjs::prelude::Func::from(store_option_selectedness),
    )?;
    globals.set(
        "__tbFormCanSubmit",
        rquickjs::prelude::Func::from(form_can_submit),
    )?;
    globals.set(
        "__tbFireSubmitEvent",
        rquickjs::prelude::Func::from(fire_submit_event_js),
    )?;
    globals.set(
        "__tbSameOriginWithTop",
        rquickjs::prelude::Func::from(same_origin_with_top),
    )?;
    globals.set("__tbParseFloat", rquickjs::prelude::Func::from(parse_float))?;
    globals.set(
        "__tbParseNonNegativeInteger",
        rquickjs::prelude::Func::from(parse_non_negative_integer_js),
    )?;
    globals.set(
        "__tbValueInUnits",
        rquickjs::prelude::Func::from(value_in_units_js),
    )?;
    globals.set(
        "__tbValueAsNumber",
        rquickjs::prelude::Func::from(value_as_number_js),
    )?;
    globals.set(
        "__tbStepMismatch",
        rquickjs::prelude::Func::from(step_mismatch_js),
    )?;
    globals.set(
        "__tbDecimalScale",
        rquickjs::prelude::Func::from(decimal_scale_js),
    )?;
    globals.set(
        "__tbDefaultStep",
        rquickjs::prelude::Func::from(default_step_js),
    )?;
    globals.set(
        "__tbNumberBufferState",
        rquickjs::prelude::Func::from(number_buffer_state_js),
    )?;
    install_control_state(globals)?;
    Ok(())
}

fn install_control_state(globals: &Object<'_>) -> Result<()> {
    globals.set("__tbUserEdited", rquickjs::prelude::Func::from(user_edited))?;
    globals.set(
        "__tbMarkUserEdited",
        rquickjs::prelude::Func::from(mark_user_edited),
    )?;
    globals.set("__tbBadInput", rquickjs::prelude::Func::from(bad_input))?;
    globals.set(
        "__tbSetBadInput",
        rquickjs::prelude::Func::from(set_bad_input),
    )?;
    globals.set(
        "__tbCustomValidity",
        rquickjs::prelude::Func::from(custom_validity),
    )?;
    globals.set(
        "__tbSetCustomValidity",
        rquickjs::prelude::Func::from(set_custom_validity),
    )?;
    globals.set(
        "__tbNumberBuffer",
        rquickjs::prelude::Func::from(number_buffer),
    )?;
    globals.set(
        "__tbSetNumberBuffer",
        rquickjs::prelude::Func::from(set_number_buffer),
    )?;
    globals.set("__tbCaret", rquickjs::prelude::Func::from(caret))?;
    globals.set("__tbSetCaret", rquickjs::prelude::Func::from(set_caret))?;
    globals.set(
        "__tbOutputValueMode",
        rquickjs::prelude::Func::from(output_value_mode),
    )?;
    globals.set(
        "__tbSetOutputValueMode",
        rquickjs::prelude::Func::from(set_output_value_mode),
    )?;
    globals.set(
        "__tbOutputDefaultValue",
        rquickjs::prelude::Func::from(output_default_value),
    )?;
    globals.set(
        "__tbSetOutputDefaultValue",
        rquickjs::prelude::Func::from(set_output_default_value),
    )?;
    Ok(())
}

fn control_of<'js>(ctx: &Ctx<'js>, element: &Value<'js>) -> Option<(NodeId, Rc<RefCell<World>>)> {
    let node = host_node_id(ctx, element)?;
    let world = owning_world(ctx, node)?;
    Some((node, world))
}

fn user_edited<'js>(ctx: Ctx<'js>, element: Value<'js>) -> bool {
    control_of(&ctx, &element).is_some_and(|(node, world)| {
        world
            .borrow()
            .with_control_state(node, |control| control.user_edited)
            .unwrap_or(false)
    })
}

fn mark_user_edited<'js>(ctx: Ctx<'js>, element: Value<'js>) {
    if let Some((node, world)) = control_of(&ctx, &element) {
        world.borrow().with_control_mut(node, |control| {
            control.user_edited = true;
        });
    }
}

fn bad_input<'js>(ctx: Ctx<'js>, element: Value<'js>) -> bool {
    control_of(&ctx, &element).is_some_and(|(node, world)| {
        world
            .borrow()
            .with_control_state(node, |control| control.bad_input)
            .unwrap_or(false)
    })
}

fn set_bad_input<'js>(ctx: Ctx<'js>, element: Value<'js>, value: bool) {
    if let Some((node, world)) = control_of(&ctx, &element) {
        world.borrow().with_control_mut(node, |control| {
            control.bad_input = value;
        });
    }
}

fn custom_validity<'js>(ctx: Ctx<'js>, element: Value<'js>) -> Result<Value<'js>> {
    let units = control_of(&ctx, &element)
        .and_then(|(node, world)| {
            world
                .borrow()
                .with_control_state(node, |control| control.custom_validity.clone())
        })
        .unwrap_or_default();
    Ok(rquickjs::String::from_utf16(ctx, &units)?.into_value())
}

fn set_custom_validity<'js>(ctx: Ctx<'js>, element: Value<'js>, message: rquickjs::String<'js>) {
    let Ok(units) = message.to_utf16() else {
        return;
    };
    let mut normalized = Vec::with_capacity(units.len());
    let mut index = 0;
    while index < units.len() {
        if units[index] == u16::from(b'\r') {
            normalized.push(u16::from(b'\n'));
            if units.get(index + 1) == Some(&u16::from(b'\n')) {
                index += 1;
            }
        } else {
            normalized.push(units[index]);
        }
        index += 1;
    }
    if let Some((node, world)) = control_of(&ctx, &element) {
        world.borrow().with_control_mut(node, |control| {
            control.custom_validity = normalized;
        });
    }
}

fn number_buffer<'js>(ctx: Ctx<'js>, element: Value<'js>) -> Result<Value<'js>> {
    let buffer = control_of(&ctx, &element).and_then(|(node, world)| {
        world
            .borrow()
            .with_control_state(node, |control| control.number_buffer.clone())
            .flatten()
    });
    optional(buffer, &ctx)
}

fn set_number_buffer<'js>(ctx: Ctx<'js>, element: Value<'js>, value: String) {
    if let Some((node, world)) = control_of(&ctx, &element) {
        world.borrow().with_control_mut(node, |control| {
            control.number_buffer = Some(value);
        });
    }
}

fn caret<'js>(ctx: Ctx<'js>, element: Value<'js>) -> Result<Value<'js>> {
    let caret = control_of(&ctx, &element).and_then(|(node, world)| {
        world
            .borrow()
            .with_control_state(node, |control| control.caret)
            .flatten()
    });
    optional(caret, &ctx)
}

fn set_caret<'js>(ctx: Ctx<'js>, element: Value<'js>, position: u32) {
    if let Some((node, world)) = control_of(&ctx, &element) {
        world.borrow().with_control_mut(node, |control| {
            control.caret = Some(position);
        });
    }
}

fn output_value_mode<'js>(ctx: Ctx<'js>, element: Value<'js>) -> bool {
    control_of(&ctx, &element).is_some_and(|(node, world)| {
        world
            .borrow()
            .with_control_state(node, |control| control.output_value_mode)
            .unwrap_or(false)
    })
}

fn set_output_value_mode<'js>(ctx: Ctx<'js>, element: Value<'js>, value: bool) {
    if let Some((node, world)) = control_of(&ctx, &element) {
        world.borrow().with_control_mut(node, |control| {
            control.output_value_mode = value;
        });
    }
}

fn output_default_value<'js>(ctx: Ctx<'js>, element: Value<'js>) -> Result<Value<'js>> {
    let units = control_of(&ctx, &element).and_then(|(node, world)| {
        world
            .borrow()
            .with_control_state(node, |control| control.output_default_value.clone())
            .flatten()
    });
    match units {
        Some(units) => Ok(rquickjs::String::from_utf16(ctx, &units)?.into_value()),
        None => Ok(Value::new_null(ctx)),
    }
}

fn set_output_default_value<'js>(ctx: Ctx<'js>, element: Value<'js>, value: rquickjs::String<'js>) {
    let Ok(units) = value.to_utf16() else {
        return;
    };
    if let Some((node, world)) = control_of(&ctx, &element) {
        world.borrow().with_control_mut(node, |control| {
            control.output_default_value = Some(units);
        });
    }
}

// https://encoding.spec.whatwg.org/#encode
fn encode_form_bytes_js(ctx: Ctx<'_>, text: String, label: String) -> Result<TypedArray<'_, u8>> {
    TypedArray::new(ctx, encode_form_bytes(&text, &label))
}

fn parse_float(ctx: Ctx<'_>, value: String) -> Result<Value<'_>> {
    optional(crate::dom::parse::parse_double(&value), &ctx)
}

fn parse_non_negative_integer_js(ctx: Ctx<'_>, value: String) -> Result<Value<'_>> {
    optional(crate::dom::parse::parse_non_negative_integer(&value), &ctx)
}

fn decimal_scale_js(ctx: Ctx<'_>, value: String) -> Result<Value<'_>> {
    optional(
        crate::dom::microsyntaxes::authored_decimal_scale(&value),
        &ctx,
    )
}

fn default_step_js(ctx: Ctx<'_>, type_name: String) -> Result<Value<'_>> {
    optional(
        crate::dom::input::default_step(InputType::from_attribute(Some(&type_name))),
        &ctx,
    )
}

fn input_kind(type_name: &str) -> InputType {
    InputType::from_attribute(Some(type_name))
}

fn value_in_units_js(ctx: Ctx<'_>, type_name: String, text: String) -> Result<Value<'_>> {
    optional(value_in_units(input_kind(&type_name), &text), &ctx)
}

fn value_as_number_js(ctx: Ctx<'_>, type_name: String, text: String) -> Result<Value<'_>> {
    optional(value_as_number(input_kind(&type_name), &text), &ctx)
}

fn step_mismatch_js<'js>(ctx: Ctx<'js>, element: Value<'js>) -> Result<bool> {
    let Some(id) = host_node_id(&ctx, &element) else {
        return Ok(false);
    };
    let world = world(&ctx)?;
    let world = world.borrow();
    let Some(parsed) = world.document(id) else {
        return Ok(false);
    };
    Ok(step_mismatch(&parsed.document.base, id.node))
}

fn number_buffer_state_js(text: String) -> u8 {
    match number_buffer_state(&text) {
        NumberBufferState::Invalid => 0,
        NumberBufferState::Empty => 1,
        NumberBufferState::Partial => 2,
        NumberBufferState::Complete => 3,
        NumberBufferState::TrailingDot => 4,
    }
}

// https://html.spec.whatwg.org/multipage/form-elements.html#dom-select-showpicker
fn same_origin_with_top(ctx: Ctx<'_>) -> Result<bool> {
    let current = world(&ctx)?;
    let frame = current.borrow().frame();
    let top = current
        .borrow()
        .runtime
        .shared
        .borrow()
        .tree
        .top_level(frame);
    if top == frame {
        return Ok(true);
    }
    let Some(top_world) = current.borrow().frame_world(top) else {
        return Ok(false);
    };
    let same = current.borrow().doc().origin == top_world.borrow().doc().origin;
    Ok(same)
}

// https://html.spec.whatwg.org/multipage/forms.html#dom-form-submit
fn form_can_submit<'js>(ctx: Ctx<'js>, form: Value<'js>) -> bool {
    let Some(node) = host_node_id(&ctx, &form) else {
        return false;
    };
    let Some(owner) = owning_world(&ctx, node) else {
        return false;
    };
    !focus::cannot_navigate(&owner.borrow(), node)
}

// https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#form-submission-algorithm
fn fire_submit_event_js<'js>(ctx: Ctx<'js>, form: Value<'js>, event: Value<'js>) -> Result<bool> {
    let Some(form) = host_node_id(&ctx, &form) else {
        return Ok(false);
    };
    crate::js::events::dispatch_shim_event_trusted(
        &ctx,
        crate::js::world::EventTargetKey::Node(form),
        event,
    )
}

// https://encoding.spec.whatwg.org/#names-and-labels
// https://encoding.spec.whatwg.org/#concept-encoding-get
// https://html.spec.whatwg.org/multipage/form-elements.html#dom-option-option
fn store_option_selectedness<'js>(
    ctx: Ctx<'js>,
    element: Value<'js>,
    selected: bool,
) -> Result<()> {
    let Some(node) = host_node_id(&ctx, &element) else {
        return Ok(());
    };
    let world = world_for_node(&ctx, node)?;
    let world = world.borrow();
    let Some(mut parsed) = world.document_mut(node) else {
        return Ok(());
    };
    forms::set_option_selectedness(&mut parsed, node.node, selected);
    Ok(())
}

pub(super) fn form_entries<'js>(
    ctx: Ctx<'js>,
    form: Value<'js>,
    submitter: Opt<Value<'js>>,
) -> Result<Array<'js>> {
    let entries = Array::new(ctx.clone())?;
    let Some(id) = host_node_id(&ctx, &form) else {
        return Ok(entries);
    };
    let world_rc = world(&ctx)?;
    let is_form = world_rc
        .borrow()
        .document(id)
        .is_some_and(|parsed| is_html_element(&parsed.document.base, id.node, "form"));
    if !is_form {
        return Ok(entries);
    }
    let submitter = submitter.0.and_then(|value| host_node_id(&ctx, &value));
    let pending = {
        let world = world_rc.borrow();
        let Some(mut parsed) = world.document_mut(id) else {
            return Ok(entries);
        };
        crate::dom::forms::collect_pending_entries(&mut parsed, id, submitter)
    };
    for entry in pending {
        match entry {
            PendingEntry::Text(name, value) => {
                let index = entries.len();
                entries.set(index, name)?;
                entries.set(index + 1, value)?;
            }
            PendingEntry::Files(name, node) => {
                push_file_entries(&ctx, &entries, &world_rc, node, &name)?;
            }
        }
    }
    Ok(entries)
}

// https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#form-submission-algorithm
pub(super) fn form_plan<'js>(
    ctx: Ctx<'js>,
    form: Value<'js>,
    submitter: Opt<Value<'js>>,
) -> Result<Value<'js>> {
    let Some(id) = host_node_id(&ctx, &form) else {
        return Ok(Value::new_null(ctx));
    };
    let world = world(&ctx)?;
    let world = world.borrow();
    let Some(parsed) = world.document(id) else {
        return Ok(Value::new_null(ctx));
    };
    let submitter = submitter.0.and_then(|value| host_node_id(&ctx, &value));
    let submitter = submitter.filter(|submitter| submitter.document == id.document);
    let Some(plan) = parsed
        .document
        .base
        .plan_form_submission(id.node, submitter.map(|submitter| submitter.node))
    else {
        return Ok(Value::new_null(ctx.clone()));
    };
    let array = Array::new(ctx.clone())?;
    array.set(0, plan.action.to_string())?;
    array.set(1, plan.method.as_str())?;
    array.set(2, plan.enctype.as_str())?;
    array.set(3, plan.target.unwrap_or_default())?;
    Ok(array.into_value())
}

// https://html.spec.whatwg.org/multipage/input.html#dom-input-files
fn push_file_entries<'js>(
    ctx: &Ctx<'js>,
    entries: &Array<'js>,
    world: &Rc<RefCell<World>>,
    node: NodeId,
    name: &str,
) -> Result<()> {
    let files: Vec<Value<'js>> = {
        let world = world.borrow();
        world.input_files(node).map_or_default(|saved| {
            saved
                .iter()
                .filter_map(|saved| saved.clone().restore(ctx).ok())
                .collect()
        })
    };
    let no_files = files.is_empty();
    for file in files {
        let at = entries.len();
        entries.set(at, name)?;
        entries.set(at + 1, file)?;
    }
    if no_files
        && let Ok(empty) = crate::js::bridge::object(ctx)?.get::<_, Value>("__tbEmptyFile")
        && let Some(function) = empty.as_function()
        && let Ok(file) = function.call::<_, Value>(())
    {
        let at = entries.len();
        entries.set(at, name)?;
        entries.set(at + 1, file)?;
    }
    Ok(())
}

// https://html.spec.whatwg.org/multipage/input.html#dom-input-files
fn input_file_list<'js>(ctx: Ctx<'js>, element: Value<'js>) -> Result<Value<'js>> {
    let Some(node) = host_node_id(&ctx, &element) else {
        return Ok(Value::new_null(ctx));
    };
    let saved = world(&ctx)?
        .borrow()
        .input_file_list(node)
        .and_then(|saved| saved.restore(&ctx).ok());
    Ok(saved.unwrap_or_else(|| Value::new_null(ctx)))
}

// https://html.spec.whatwg.org/multipage/input.html#dom-input-files
fn input_file_objects<'js>(ctx: Ctx<'js>, element: Value<'js>) -> Result<Array<'js>> {
    let array = Array::new(ctx.clone())?;
    let Some(node) = host_node_id(&ctx, &element) else {
        return Ok(array);
    };
    let files: Vec<Value<'js>> = {
        let world = world(&ctx)?;
        let world = world.borrow();
        world.input_files(node).map_or_default(|saved| {
            saved
                .iter()
                .filter_map(|saved| saved.clone().restore(&ctx).ok())
                .collect()
        })
    };
    for (index, file) in files.into_iter().enumerate() {
        array.set(index, file)?;
    }
    Ok(array)
}

pub(super) fn set_input_files<'js>(
    ctx: Ctx<'js>,
    element: Value<'js>,
    files: Value<'js>,
) -> Result<()> {
    let Some(node) = host_node_id(&ctx, &element) else {
        return Ok(());
    };
    let world_rc = world(&ctx)?;
    let mut stored = Vec::new();
    if let Some(object) = files.as_object() {
        let length = object.get::<_, f64>("length").unwrap_or(0.0);
        if length.is_finite() && length > 0.0 {
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a file count is a small non-negative integer"
            )]
            let length = (length as usize).min(1024);
            for index in 0..length {
                let Ok(file) = object.get::<_, Value>(index.to_string()) else {
                    continue;
                };
                if !file.is_undefined() && !file.is_null() {
                    stored.push(Persistent::save(&ctx, file));
                }
            }
        }
    }
    let mut world = world_rc.borrow_mut();
    world.set_input_files(node, stored);
    world.set_input_file_list(node, Persistent::save(&ctx, files.clone()));
    Ok(())
}

// https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#form-submission-algorithm
// https://html.spec.whatwg.org/multipage/links.html#cannot-navigate
pub(super) fn form_navigate<'js>(
    ctx: Ctx<'js>,
    url: String,
    target: String,
    method: String,
    body: TypedArray<'js, u8>,
    content_type: Option<String>,
    form: Value<'js>,
) -> Result<()> {
    let Some(node) = host_node_id(&ctx, &form) else {
        return Ok(());
    };
    let Some(owner) = owning_world(&ctx, node) else {
        return Ok(());
    };
    // https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#attr-fs-method
    let method = match method.as_str() {
        "GET" => net::Method::GET,
        "POST" => net::Method::POST,
        other => {
            return Err(Exception::throw_type(
                &ctx,
                &format!("form method: {other:?} is neither GET nor POST"),
            ));
        }
    };
    if focus::cannot_navigate(&owner.borrow(), node) {
        return Ok(());
    }
    let world_rc = world(&ctx)?;
    // https://html.spec.whatwg.org/multipage/browsers.html#sandboxed-forms-browsing-context-flag
    if world_rc
        .borrow()
        .doc()
        .sandbox
        .contains(crate::protocol::SandboxFlag::Forms)
    {
        return Ok(());
    }
    let mut navigation = FrameNavigation::get(
        crate::js::NavigationTarget::Target(crate::protocol::TargetName::parse(&target)),
        url,
    );
    navigation.method = method;
    navigation.body = body.to_vec();
    navigation.content_type = content_type;
    world_rc.borrow_mut().queue_frame_navigation(navigation);
    Ok(())
}
