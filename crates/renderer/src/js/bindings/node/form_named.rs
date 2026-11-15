//! A form element's indexed and named properties.
//!
//! `HTMLFormElement` is a Web IDL legacy platform object with both an indexed
//! getter (`form[0]`) and a named getter (`form[name]`), the latter overriding
//! built-in prototype members and answering a live `RadioNodeList` when a name
//! matches more than one control
//! (<https://html.spec.whatwg.org/multipage/forms.html#dom-form-elements>,
//! <https://html.spec.whatwg.org/multipage/forms.html#dom-form-nameditem>,
//! <https://webidl.spec.whatwg.org/#legacy-platform-object>).
//!
//! Every node wrapper shares the `JsNode` class, so the exotic hooks in
//! `node::mod` route form brands here before the `Document` named-property
//! path.

use rquickjs::{
    Atom, Ctx, Exception, Function, Result, Value,
    class::{
        ExoticDefineResult, ExoticSetResult, PropertyDefinition, PropertyDescriptor, PropertyName,
    },
};

use crate::dom::tree::{self, BlitzId, NodeId};

use super::super::{array_index, atom_name, owning_world, wrap_node};

/// One entry of the supported property names algorithm: a name contributed by
/// an element's `id` or `name`, or a past name, with the ordering keys the
/// algorithm sorts by
/// (<https://html.spec.whatwg.org/multipage/forms.html#dom-form-nameditem>).
struct SourcedName {
    name: String,
    element: NodeId,
    /// `0` for `id`, `1` for `name`, `2` for a past name.
    source: u8,
    /// Age within the same element and source; past names are ordered oldest
    /// first.
    age: usize,
}

/// The descriptor of the form's indexed or named property `atom`, or `None`
/// when it names neither
/// (<https://webidl.spec.whatwg.org/#legacy-platform-object-getownproperty>).
pub(super) fn named_property<'js>(
    ctx: &Ctx<'js>,
    form: NodeId,
    atom: &Atom<'js>,
    _object: &Value<'js>,
) -> Result<Option<PropertyDescriptor<'js>>> {
    let Some(name) = atom_name(ctx, atom) else {
        return Ok(None);
    };
    if let Some(index) = array_index(&name) {
        // An unsupported index hides named properties as well, so the lookup
        // stops here instead of falling back to a control with that name
        // (<https://webidl.spec.whatwg.org/#legacy-platform-object-getownproperty>).
        return Ok(indexed_element(ctx, form, index)?
            .map(|value| PropertyDescriptor::new_value(value, true, true, false)));
    }
    let Some(value) = named_value(ctx, form, &name)? else {
        return Ok(None);
    };
    // `[LegacyUnenumerableNamedProperties]` makes named properties
    // non-enumerable, and `HTMLFormElement` has no named property setter, so
    // they are not writable either
    // (<https://webidl.spec.whatwg.org/#legacy-platform-object-getownproperty>,
    // <https://webidl.spec.whatwg.org/#LegacyUnenumerableNamedProperties>).
    Ok(Some(PropertyDescriptor::new_value(
        value, true, false, false,
    )))
}

/// The form's supported property names for `[[OwnPropertyKeys]]`: the
/// supported indices as strings, then the supported property names, skipping
/// any name an own property already shadows
/// (<https://webidl.spec.whatwg.org/#legacy-platform-object-ownpropertykeys>).
pub(super) fn named_property_names<'js>(
    ctx: &Ctx<'js>,
    form: NodeId,
    object: &Value<'js>,
) -> Result<Vec<PropertyName<'js>>> {
    let Some(world) = owning_world(ctx, form) else {
        return Ok(Vec::new());
    };
    let world = world.borrow_mut();
    let Some(parsed) = world.document(form) else {
        return Ok(Vec::new());
    };
    let base = &parsed.document.base;
    let count = listed_elements(base, form).len();
    // Past names whose element left the form are dropped as the map is read;
    // the spec removes them when the element changes form owner, and the form
    // owner is resolved on read rather than stored.
    let mut past = world.state.form_past_names.borrow_mut();
    let names = supported_names(base, form, &mut past);
    let object = object
        .as_object()
        .ok_or_else(|| Exception::throw_type(ctx, "form target is not an object"))?;
    let mut properties = Vec::with_capacity(count + names.len());
    for index in 0..count {
        properties.push(PropertyName {
            atom: Atom::from_str(ctx.clone(), &index.to_string())?,
            is_enumerable: true,
        });
    }
    for name in names {
        if object.contains_own_key(name.as_str())? {
            continue;
        }
        properties.push(PropertyName {
            atom: Atom::from_str(ctx.clone(), &name)?,
            is_enumerable: false,
        });
    }
    Ok(properties)
}

/// `[[Set]]`: a supported index or name has no setter, so the write fails; any
/// other key is an ordinary expando
/// (<https://webidl.spec.whatwg.org/#legacy-platform-object-set>).
pub(super) fn set_named_property<'js>(
    ctx: &Ctx<'js>,
    form: NodeId,
    atom: &Atom<'js>,
    object: &Value<'js>,
    receiver: &Value<'js>,
) -> Result<ExoticSetResult> {
    if object != receiver {
        return Ok(ExoticSetResult::Fallthrough);
    }
    let Some(name) = atom_name(ctx, atom) else {
        return Ok(ExoticSetResult::Fallthrough);
    };
    if !supports_property(ctx, form, &name)? {
        return Ok(ExoticSetResult::Fallthrough);
    }
    Ok(ExoticSetResult::Handled(false))
}

/// `[[Delete]]`: a supported index or name has no deleter, so the deletion
/// fails. An own property is handled by the ordinary path before this hook
/// runs, so this only sees names that would otherwise be deleted as named
/// properties
/// (<https://webidl.spec.whatwg.org/#legacy-platform-object-delete>).
pub(super) fn delete_named_property<'js>(
    ctx: &Ctx<'js>,
    form: NodeId,
    atom: &Atom<'js>,
    _object: &Value<'js>,
) -> Result<bool> {
    let Some(name) = atom_name(ctx, atom) else {
        return Ok(true);
    };
    Ok(!supports_property(ctx, form, &name)?)
}

/// `[[DefineOwnProperty]]`: defining over a supported index or name fails,
/// because the form has neither an indexed nor a named property setter. Any
/// other key falls through to the ordinary definition that creates the
/// expando
/// (<https://webidl.spec.whatwg.org/#legacy-platform-object-defineownproperty>).
pub(super) fn define_named_property<'js>(
    ctx: &Ctx<'js>,
    form: NodeId,
    atom: &Atom<'js>,
    _descriptor: PropertyDefinition<'js>,
) -> Result<ExoticDefineResult> {
    let Some(name) = atom_name(ctx, atom) else {
        return Ok(ExoticDefineResult::Fallthrough);
    };
    if supports_property(ctx, form, &name)? {
        return Ok(ExoticDefineResult::Handled(false));
    }
    Ok(ExoticDefineResult::Fallthrough)
}

/// Whether `name` is a supported property index or name of the form.
fn supports_property(ctx: &Ctx<'_>, form: NodeId, name: &str) -> Result<bool> {
    if let Some(index) = array_index(name) {
        return Ok(indexed_element(ctx, form, index)?.is_some());
    }
    let Some(world) = owning_world(ctx, form) else {
        return Ok(false);
    };
    let world = world.borrow_mut();
    let Some(parsed) = world.document(form) else {
        return Ok(false);
    };
    let base = &parsed.document.base;
    Ok(
        supported_names(base, form, &mut world.state.form_past_names.borrow_mut())
            .contains(&name.to_owned()),
    )
}

/// The element at supported index `index`
/// (<https://html.spec.whatwg.org/multipage/forms.html#dom-form-elements>).
fn indexed_element<'js>(ctx: &Ctx<'js>, form: NodeId, index: u32) -> Result<Option<Value<'js>>> {
    let Some(world) = owning_world(ctx, form) else {
        return Ok(None);
    };
    let element = {
        let world = world.borrow();
        let Some(parsed) = world.document(form) else {
            return Ok(None);
        };
        listed_elements(&parsed.document.base, form)
            .get(usize::try_from(index).unwrap_or(usize::MAX))
            .copied()
    };
    element.map(|element| wrap_node(ctx, element)).transpose()
}

/// The value of the form named property `name`: the single matching control,
/// the live `RadioNodeList` when several match, or a past name's element
/// (<https://html.spec.whatwg.org/multipage/forms.html#dom-form-nameditem>).
fn named_value<'js>(ctx: &Ctx<'js>, form: NodeId, name: &str) -> Result<Option<Value<'js>>> {
    let Some(world) = owning_world(ctx, form) else {
        return Ok(None);
    };
    let matches = {
        let world = world.borrow();
        let Some(parsed) = world.document(form) else {
            return Ok(None);
        };
        named_matches(&parsed.document.base, form, name)
    };
    match matches.as_slice() {
        [] => past_name(ctx, form, name)
            .map(|element| wrap_node(ctx, element))
            .transpose(),
        [element] => {
            record_past_name(ctx, form, name, *element);
            wrap_node(ctx, *element).map(Some)
        }
        _ => {
            let world_rc =
                owning_world(ctx, form).ok_or_else(|| Exception::throw_type(ctx, "stale node"))?;
            let cached = world_rc
                .borrow()
                .state
                .form_named_lists
                .get(&(form, name.to_owned()))
                .cloned();
            if let Some(saved) = cached
                && let Some(value) = super::super::deref_weak(ctx, saved)?
            {
                return Ok(Some(value));
            }
            let function: Function =
                crate::js::bridge::object(ctx)?.get("__tbFormRadioNodeList")?;
            let list: Value = function.call((wrap_node(ctx, form)?, name))?;
            let weak = super::super::make_weak(ctx, list.clone())?;
            world_rc.borrow_mut().state.form_named_lists.insert(
                (form, name.to_owned()),
                rquickjs::Persistent::save(ctx, weak),
            );
            Ok(Some(list))
        }
    }
}

/// The elements the named getter considers: the form's listed elements except
/// image buttons, then the form-associated `img` elements when no listed
/// element matches
/// (<https://html.spec.whatwg.org/multipage/forms.html#dom-form-nameditem>).
fn named_matches(base: &blitz_dom::BaseDocument, form: NodeId, name: &str) -> Vec<NodeId> {
    let candidates = named_candidates(base, form);
    let listed = candidates
        .iter()
        .filter(|element| !tree::is_html_element(base, element.node, "img"))
        .filter(|element| matches_name(base, element.node, name))
        .copied()
        .collect::<Vec<_>>();
    if !listed.is_empty() {
        return listed;
    }
    candidates
        .into_iter()
        .filter(|element| tree::is_html_element(base, element.node, "img"))
        .filter(|element| matches_name(base, element.node, name))
        .collect()
}

/// The form's named-getter candidates, in tree order: listed elements whose
/// form owner is the form, minus image buttons, plus `img` elements whose form
/// owner is the form.
fn named_candidates(base: &blitz_dom::BaseDocument, form: NodeId) -> Vec<NodeId> {
    let mut candidates = listed_elements(base, form);
    candidates.extend(
        tree_elements(base, form.document)
            .into_iter()
            .filter(|element| {
                tree::is_html_element(base, element.node, "img")
                    && tree::form_owner_of(base, form.document, element.node) == Some(form)
            }),
    );
    candidates.sort_by(|a, b| tree::tree_order(base, form.document, a.node, b.node));
    candidates
}

/// The form's listed elements whose form owner is the form, excluding image
/// buttons, in tree order
/// (<https://html.spec.whatwg.org/multipage/forms.html#dom-form-elements>).
fn listed_elements(base: &blitz_dom::BaseDocument, form: NodeId) -> Vec<NodeId> {
    tree_elements(base, form.document)
        .into_iter()
        .filter(|element| {
            let is_input = tree::is_html_element(base, element.node, "input");
            let listed = is_input
                || [
                    "button", "fieldset", "object", "output", "select", "textarea",
                ]
                .iter()
                .any(|local| tree::is_html_element(base, element.node, local));
            if !listed {
                return false;
            }
            if is_input && is_image_input(base, element.node) {
                return false;
            }
            tree::form_owner_of(base, form.document, element.node) == Some(form)
        })
        .collect()
}

/// Every element descendant of the tree's root, in tree order.
fn tree_elements(base: &blitz_dom::BaseDocument, document: u32) -> Vec<NodeId> {
    let mut elements = Vec::new();
    let mut stack: Vec<BlitzId> = base
        .get_node(base.root_node().id)
        .map_or_default(|node| node.children.iter().rev().copied().collect());
    while let Some(node) = stack.pop() {
        if let Some(tree) = base.get_node(node) {
            if tree.data.downcast_element().is_some() {
                elements.push(NodeId { document, node });
            }
            stack.extend(tree.children.iter().rev().copied());
        }
    }
    elements
}

fn is_image_input(base: &blitz_dom::BaseDocument, node: BlitzId) -> bool {
    tree::attr(base, node, "type").is_some_and(|value| value.eq_ignore_ascii_case("image"))
}

fn matches_name(base: &blitz_dom::BaseDocument, node: BlitzId, name: &str) -> bool {
    tree::attr(base, node, "id").is_some_and(|value| value == name)
        || tree::attr(base, node, "name").is_some_and(|value| value == name)
}

/// The supported property names, in the algorithm's order
/// (<https://html.spec.whatwg.org/multipage/forms.html#dom-form-nameditem>).
fn supported_names(
    base: &blitz_dom::BaseDocument,
    form: NodeId,
    past_names: &mut std::collections::HashMap<NodeId, Vec<(String, NodeId)>>,
) -> Vec<String> {
    let mut sourced = Vec::new();
    for element in named_candidates(base, form) {
        if let Some(id) = tree::attr(base, element.node, "id").filter(|id| !id.is_empty()) {
            sourced.push(SourcedName {
                name: id.to_owned(),
                element,
                source: 0,
                age: 0,
            });
        }
        if let Some(name) = tree::attr(base, element.node, "name").filter(|name| !name.is_empty()) {
            sourced.push(SourcedName {
                name: name.to_owned(),
                element,
                source: 1,
                age: 0,
            });
        }
    }
    if let Some(past) = past_names.get_mut(&form) {
        past.retain(|(_, element)| {
            tree::form_owner_of(base, form.document, element.node) == Some(form)
                && tree::is_connected(base, element.node)
        });
        for (age, (name, element)) in past.iter().enumerate() {
            sourced.push(SourcedName {
                name: name.clone(),
                element: *element,
                source: 2,
                age,
            });
        }
    }
    sourced.sort_by(|a, b| {
        if a.element == b.element {
            a.source.cmp(&b.source).then(a.age.cmp(&b.age))
        } else {
            tree::tree_order(base, form.document, a.element.node, b.element.node)
        }
    });
    let mut names: Vec<String> = Vec::with_capacity(sourced.len());
    for entry in sourced {
        if !names.contains(&entry.name) {
            names.push(entry.name);
        }
    }
    names
}

/// Records a single-control named getter answer in the form's past names map,
/// replacing an entry with the same name
/// (<https://html.spec.whatwg.org/multipage/forms.html#past-names-map>).
fn record_past_name(ctx: &Ctx<'_>, form: NodeId, name: &str, element: NodeId) {
    let Some(world) = owning_world(ctx, form) else {
        return;
    };
    let world = world.borrow_mut();
    let mut map = world.state.form_past_names.borrow_mut();
    let past = map.entry(form).or_default();
    past.retain(|(existing, _)| existing != name);
    past.push((name.to_owned(), element));
}

/// The element a past name answers, dropping the entry when its element left
/// the form
/// (<https://html.spec.whatwg.org/multipage/forms.html#past-names-map>).
fn past_name(ctx: &Ctx<'_>, form: NodeId, name: &str) -> Option<NodeId> {
    let world = owning_world(ctx, form)?;
    let world = world.borrow_mut();
    let parsed = world.document(form)?;
    let base = &parsed.document.base;
    let mut map = world.state.form_past_names.borrow_mut();
    let past = map.get_mut(&form)?;
    let mut found = None;
    past.retain(|(existing, element)| {
        if existing != name {
            return true;
        }
        if tree::form_owner_of(base, form.document, element.node) != Some(form)
            || !tree::is_connected(base, element.node)
        {
            return false;
        }
        found = Some(*element);
        true
    });
    found
}
