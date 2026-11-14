//! A `Document`'s named properties
//! (<https://html.spec.whatwg.org/multipage/dom.html#dom-document-nameditem>).
//!
//! `Document` is the only node interface that supports named properties, but
//! every node wrapper shares the `JsNode` class, so the exotic hooks in
//! `node::mod` answer `None` for every other node before reaching here.

use rquickjs::{
    Atom, Ctx, Exception, Function, Persistent, Result, Value,
    class::{ExoticSetResult, PropertyDescriptor, PropertyName},
    object::Property,
};

use crate::dom::tree::{NodeId, is_html_element};
use crate::js::bindings::{
    CollectionKind, atom_name, collect_document_named, deref_weak, document_has_named_property,
    document_supported_names, live_collection, make_weak, named_key_visible, world_for_node,
    wrap_node,
};

/// The descriptor of the Document named property `atom` names, or `None` when
/// no named element carries it, or when a prototype member shadows it
/// (<https://webidl.spec.whatwg.org/#legacy-platform-object-getownproperty>).
pub(super) fn named_property<'js>(
    ctx: &Ctx<'js>,
    document: NodeId,
    atom: &Atom<'js>,
    object: &Value<'js>,
) -> Result<Option<PropertyDescriptor<'js>>> {
    let Some(name) = atom_name(ctx, atom) else {
        return Ok(None);
    };
    // Document named properties no longer override built-ins, which is the
    // direction of <https://github.com/whatwg/html/pull/13161> and what
    // Firefox Nightly ships: a prototype member keeps its own value.
    if !named_key_visible(object, &name)? {
        return Ok(None);
    }
    let Some(value) = named_value(ctx, document, &name)? else {
        return Ok(None);
    };
    // `Document` has no named property setter, so the property is a
    // non-writable but configurable, enumerable data property
    // (<https://webidl.spec.whatwg.org/#legacy-platform-object-getownproperty>).
    Ok(Some(PropertyDescriptor::new_value(
        value, true, true, false,
    )))
}

/// The Document's supported property names, for `[[OwnPropertyKeys]]`
/// (<https://webidl.spec.whatwg.org/#legacy-platform-object-ownpropertykeys>).
pub(super) fn named_property_names<'js>(
    ctx: &Ctx<'js>,
    document: NodeId,
    object: &Value<'js>,
) -> Result<Vec<PropertyName<'js>>> {
    let world = world_for_node(ctx, document)?;
    let names = {
        let world = world.borrow();
        let Some(parsed) = world.document(document) else {
            return Ok(Vec::new());
        };
        document_supported_names(&parsed.document.base, document.node)
    };
    let mut properties = Vec::new();
    for name in names {
        if !named_key_visible(object, &name)? {
            continue;
        }
        properties.push(PropertyName {
            atom: Atom::from_str(ctx.clone(), &name)?,
            is_enumerable: true,
        });
    }
    Ok(properties)
}

/// `Document`'s `[[Set]]` for a supported name: the named property is ignored
/// and an ordinary own property is created that shadows it from then on
/// (<https://webidl.spec.whatwg.org/#legacy-platform-object-set>).
pub(super) fn set_named_property<'js>(
    ctx: &Ctx<'js>,
    document: NodeId,
    atom: &Atom<'js>,
    object: &Value<'js>,
    value: Value<'js>,
) -> Result<ExoticSetResult> {
    let Some(name) = atom_name(ctx, atom) else {
        return Ok(ExoticSetResult::Fallthrough);
    };
    if !named_key_visible(object, &name)? || !is_supported_name(ctx, document, &name)? {
        return Ok(ExoticSetResult::Fallthrough);
    }
    let target = object
        .as_object()
        .ok_or_else(|| Exception::throw_type(ctx, "named property target is not an object"))?;
    target.prop(
        Atom::from_str(ctx.clone(), &name)?,
        Property::from(value).writable().enumerable().configurable(),
    )?;
    Ok(ExoticSetResult::Handled(true))
}

/// `Document`'s `[[Delete]]` for a supported name: it has no named property
/// deleter, so the deletion fails
/// (<https://webidl.spec.whatwg.org/#legacy-platform-object-delete>).
pub(super) fn delete_named_property<'js>(
    ctx: &Ctx<'js>,
    document: NodeId,
    atom: &Atom<'js>,
    object: &Value<'js>,
) -> Result<bool> {
    let Some(name) = atom_name(ctx, atom) else {
        return Ok(false);
    };
    Ok(!named_key_visible(object, &name)? || !is_supported_name(ctx, document, &name)?)
}

fn is_supported_name(ctx: &Ctx<'_>, document: NodeId, name: &str) -> Result<bool> {
    let world = world_for_node(ctx, document)?;
    let world = world.borrow();
    let Some(parsed) = world.document(document) else {
        return Ok(false);
    };
    Ok(document_has_named_property(
        &parsed.document.base,
        document.node,
        name,
    ))
}

/// The value of the Document named property `name`: the single named element,
/// that element's `contentWindow` when it is an `iframe`, or the cached live
/// `HTMLCollection` when several elements share the name
/// (<https://html.spec.whatwg.org/multipage/dom.html#dom-document-nameditem>).
fn named_value<'js>(ctx: &Ctx<'js>, document: NodeId, name: &str) -> Result<Option<Value<'js>>> {
    let world = world_for_node(ctx, document)?;
    let elements = {
        let world = world.borrow();
        let Some(parsed) = world.document(document) else {
            return Ok(None);
        };
        collect_document_named(
            &parsed.document.base,
            document.document,
            document.node,
            name,
        )
    };
    match elements.as_slice() {
        [] => Ok(None),
        [element] => match iframe_content_window(ctx, *element)? {
            Some(window) => Ok(Some(window)),
            None => wrap_node(ctx, *element).map(Some),
        },
        _ => {
            let cached = world
                .borrow()
                .state
                .document_named
                .get(&(document, name.to_owned()))
                .cloned();
            if let Some(saved) = cached
                && let Some(value) = deref_weak(ctx, saved)?
            {
                return Ok(Some(value));
            }
            let value = live_collection(
                ctx,
                document,
                CollectionKind::DocumentNamed(name.to_owned()),
                Some("HTMLCollection"),
            )?;
            let weak = make_weak(ctx, value.clone())?;
            world
                .borrow_mut()
                .state
                .document_named
                .insert((document, name.to_owned()), Persistent::save(ctx, weak));
            Ok(Some(value))
        }
    }
}

/// The active `WindowProxy` of an `iframe` whose content navigable exists.
/// A `frame`, or an `iframe` without a navigable, is answered as the element
/// itself by the caller
/// (<https://html.spec.whatwg.org/multipage/dom.html#dom-document-nameditem>).
fn iframe_content_window<'js>(ctx: &Ctx<'js>, id: NodeId) -> Result<Option<Value<'js>>> {
    let world = world_for_node(ctx, id)?;
    let frame = {
        let world = world.borrow();
        if !world
            .document(id)
            .is_some_and(|parsed| is_html_element(&parsed.document.base, id.node, "iframe"))
        {
            return Ok(None);
        }
        world.frame_for_container(id)
    };
    let Some(frame) = frame else {
        return Ok(None);
    };
    let proxy: Function = crate::js::bridge::object(ctx)?.get("__tbFrameProxy")?;
    let value: Value = proxy.call((crate::js::js_number(frame.get()),))?;
    Ok(Some(value))
}
