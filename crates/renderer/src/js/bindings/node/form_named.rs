// https://html.spec.whatwg.org/multipage/forms.html#dom-form-elements
// https://html.spec.whatwg.org/multipage/forms.html#dom-form-nameditem
// https://webidl.spec.whatwg.org/#legacy-platform-object

use rquickjs::{
    Atom, Ctx, Exception, Function, Result, Value,
    class::{
        ExoticDefineResult, ExoticSetResult, PropertyDefinition, PropertyDescriptor, PropertyName,
    },
};

use crate::dom::tree::{self, BlitzId, NodeId};

use super::super::{array_index, atom_name, owning_world, wrap_node};

// https://html.spec.whatwg.org/multipage/forms.html#dom-form-nameditem
struct SourcedName {
    name: String,
    element: NodeId,
    source: u8,
    age: usize,
}

// https://webidl.spec.whatwg.org/#legacy-platform-object-getownproperty
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
        // https://webidl.spec.whatwg.org/#legacy-platform-object-getownproperty
        return Ok(indexed_element(ctx, form, index)?
            .map(|value| PropertyDescriptor::new_value(value, true, true, false)));
    }
    let Some(value) = named_value(ctx, form, &name)? else {
        return Ok(None);
    };
    // https://webidl.spec.whatwg.org/#legacy-platform-object-getownproperty
    // https://webidl.spec.whatwg.org/#LegacyUnenumerableNamedProperties
    Ok(Some(PropertyDescriptor::new_value(
        value, true, false, false,
    )))
}

// https://webidl.spec.whatwg.org/#legacy-platform-object-ownpropertykeys
pub(super) fn named_property_names<'js>(
    ctx: &Ctx<'js>,
    form: NodeId,
    object: &Value<'js>,
) -> Result<Vec<PropertyName<'js>>> {
    let Some(world) = owning_world(ctx, form) else {
        return Ok(Vec::new());
    };
    let world = world.borrow();
    let Some(parsed) = world.document(form) else {
        return Ok(Vec::new());
    };
    let base = &parsed.document.base;
    let count = listed_elements(base, form).len();
    let doc = world.doc();
    let mut past = doc.state.form_past_names.borrow_mut();
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

// https://webidl.spec.whatwg.org/#legacy-platform-object-set
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

// https://webidl.spec.whatwg.org/#legacy-platform-object-delete
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

// https://webidl.spec.whatwg.org/#legacy-platform-object-defineownproperty
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

fn supports_property(ctx: &Ctx<'_>, form: NodeId, name: &str) -> Result<bool> {
    if let Some(index) = array_index(name) {
        return Ok(indexed_element(ctx, form, index)?.is_some());
    }
    let Some(world) = owning_world(ctx, form) else {
        return Ok(false);
    };
    let world = world.borrow();
    let Some(parsed) = world.document(form) else {
        return Ok(false);
    };
    let base = &parsed.document.base;
    let doc = world.doc();
    Ok(
        supported_names(base, form, &mut doc.state.form_past_names.borrow_mut())
            .contains(&name.to_owned()),
    )
}

// https://html.spec.whatwg.org/multipage/forms.html#dom-form-elements
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

// https://html.spec.whatwg.org/multipage/forms.html#dom-form-nameditem
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
            let caller = super::super::world(ctx)?;
            let cached = caller
                .borrow()
                .caches
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
            caller.borrow_mut().caches.form_named_lists.insert(
                (form, name.to_owned()),
                rquickjs::Persistent::save(ctx, weak),
            );
            Ok(Some(list))
        }
    }
}

// https://html.spec.whatwg.org/multipage/forms.html#dom-form-nameditem
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

// https://html.spec.whatwg.org/multipage/forms.html#dom-form-elements
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

// https://html.spec.whatwg.org/multipage/forms.html#dom-form-nameditem
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

// https://html.spec.whatwg.org/multipage/forms.html#past-names-map
fn record_past_name(ctx: &Ctx<'_>, form: NodeId, name: &str, element: NodeId) {
    let Some(world) = owning_world(ctx, form) else {
        return;
    };
    let world = world.borrow();
    let doc = world.doc();
    let mut map = doc.state.form_past_names.borrow_mut();
    let past = map.entry(form).or_default();
    past.retain(|(existing, _)| existing != name);
    past.push((name.to_owned(), element));
}

// https://html.spec.whatwg.org/multipage/forms.html#past-names-map
fn past_name(ctx: &Ctx<'_>, form: NodeId, name: &str) -> Option<NodeId> {
    let world = owning_world(ctx, form)?;
    let world = world.borrow();
    let parsed = world.document(form)?;
    let base = &parsed.document.base;
    let doc = world.doc();
    let mut map = doc.state.form_past_names.borrow_mut();
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
