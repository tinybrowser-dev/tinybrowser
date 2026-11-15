//! An `HTMLSelectElement`'s indexed properties.
//!
//! `HTMLSelectElement` is a Web IDL legacy platform object with an indexed
//! getter over the same options as `select.options`, and no named properties:
//! a name is answered only by its `namedItem()` method
//! (<https://html.spec.whatwg.org/multipage/form-elements.html#dom-select-length>).
//! Every key that is not a supported index falls through to the ordinary
//! property path.
//!
//! Every node wrapper shares the `JsNode` class, so the exotic hooks in
//! `node::mod` route the `HTMLSelectElement` brand here.

use rquickjs::{
    Atom, Ctx, Result, Value,
    class::{ExoticDefineResult, ExoticSetResult, PropertyDescriptor, PropertyName},
};

use crate::dom::tree::NodeId;

use super::super::{array_index, atom_name, owning_world, wrap_node};

/// The descriptor of the select's indexed property `atom`, or `None` when
/// `atom` is not a supported index
/// (<https://webidl.spec.whatwg.org/#legacy-platform-object-getownproperty>).
pub(super) fn indexed_property<'js>(
    ctx: &Ctx<'js>,
    select: NodeId,
    atom: &Atom<'js>,
) -> Result<Option<PropertyDescriptor<'js>>> {
    let Some(index) = atom_index(ctx, atom) else {
        return Ok(None);
    };
    let Some(option) = option_at(ctx, select, index) else {
        return Ok(None);
    };
    let value = wrap_node(ctx, option)?;
    // The indexed getter has no identifier and the interface has no indexed
    // property setter, so the property is a non-writable but configurable,
    // enumerable data property
    // (<https://webidl.spec.whatwg.org/#legacy-platform-object-getownproperty>).
    Ok(Some(PropertyDescriptor::new_value(
        value, true, true, false,
    )))
}

/// The select's supported property indices, for `[[OwnPropertyKeys]]`
/// (<https://webidl.spec.whatwg.org/#legacy-platform-object-ownpropertykeys>).
pub(super) fn indexed_property_names<'js>(
    ctx: &Ctx<'js>,
    select: NodeId,
) -> Result<Vec<PropertyName<'js>>> {
    let count = options(ctx, select).len();
    let mut properties = Vec::with_capacity(count);
    for index in 0..count {
        properties.push(PropertyName {
            atom: Atom::from_str(ctx.clone(), &index.to_string())?,
            is_enumerable: true,
        });
    }
    Ok(properties)
}

/// `[[Set]]`: a supported index has no setter, so the write fails; any other
/// key is an ordinary expando
/// (<https://webidl.spec.whatwg.org/#legacy-platform-object-set>).
pub(super) fn set_indexed_property<'js>(
    ctx: &Ctx<'js>,
    select: NodeId,
    atom: &Atom<'js>,
    object: &Value<'js>,
    receiver: &Value<'js>,
) -> ExoticSetResult {
    if object != receiver {
        return ExoticSetResult::Fallthrough;
    }
    let Some(index) = atom_index(ctx, atom) else {
        return ExoticSetResult::Fallthrough;
    };
    if option_at(ctx, select, index).is_some() {
        return ExoticSetResult::Handled(false);
    }
    ExoticSetResult::Fallthrough
}

/// `[[Delete]]`: a supported index has no deleter, so the deletion fails. An
/// own property is handled by the ordinary path before this hook runs
/// (<https://webidl.spec.whatwg.org/#legacy-platform-object-delete>).
pub(super) fn delete_indexed_property<'js>(
    ctx: &Ctx<'js>,
    select: NodeId,
    atom: &Atom<'js>,
) -> bool {
    let Some(index) = atom_index(ctx, atom) else {
        return true;
    };
    option_at(ctx, select, index).is_none()
}

/// `[[DefineOwnProperty]]`: defining over a supported index fails, because the
/// interface has no indexed property setter; any other key falls through to
/// the ordinary definition that creates the expando
/// (<https://webidl.spec.whatwg.org/#legacy-platform-object-defineownproperty>).
pub(super) fn define_indexed_property<'js>(
    ctx: &Ctx<'js>,
    select: NodeId,
    atom: &Atom<'js>,
) -> ExoticDefineResult {
    let Some(index) = atom_index(ctx, atom) else {
        return ExoticDefineResult::Fallthrough;
    };
    if option_at(ctx, select, index).is_some() {
        return ExoticDefineResult::Handled(false);
    }
    ExoticDefineResult::Fallthrough
}

/// The array index `atom` names, or `None` for a symbol or a non-canonical
/// index string such as `"03"`
/// (<https://tc39.es/ecma262/#array-index>).
fn atom_index<'js>(ctx: &Ctx<'js>, atom: &Atom<'js>) -> Option<u32> {
    atom_name(ctx, atom).and_then(|name| array_index(&name))
}

/// The option at supported index `index`, if the select has that many.
fn option_at(ctx: &Ctx<'_>, select: NodeId, index: u32) -> Option<NodeId> {
    options(ctx, select)
        .get(usize::try_from(index).unwrap_or(usize::MAX))
        .copied()
}

/// The select's options in tree order, the same list its `options` collection
/// answers
/// (<https://html.spec.whatwg.org/multipage/form-elements.html#concept-select-option-list>).
fn options(ctx: &Ctx<'_>, select: NodeId) -> Vec<NodeId> {
    let Some(world) = owning_world(ctx, select) else {
        return Vec::new();
    };
    let world = world.borrow();
    let Some(parsed) = world.document(select) else {
        return Vec::new();
    };
    crate::dom::forms::select_options(&parsed.document.base, select.document, select.node)
}
