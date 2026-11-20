// https://html.spec.whatwg.org/multipage/common-dom-interfaces.html#reflecting-content-attributes-in-idl-attributes:element

use rquickjs::{Array, Ctx, Exception, Function, Object, Result, Symbol, Value};

use crate::documents::ElementReference;
use crate::dom::tree::{BlitzId, NodeId, attr, shadow_including_parent};
use crate::js::bindings::{
    find_element_by_id, host_node_id, remove_attribute_sync, set_attribute_sync, world_for_node,
    wrap_node,
};
use crate::js::world::ElementReferenceCache;

pub(crate) fn get_element_reference<'js>(
    ctx: &Ctx<'js>,
    element: NodeId,
    content: &str,
) -> Result<Value<'js>> {
    let world = world_for_node(ctx, element)?;
    let found = {
        let world = world.borrow();
        let Some(parsed) = world.document(element) else {
            return Ok(Value::new_null(ctx.clone()));
        };
        attr_associated_element(&parsed, element, content)
    };
    match found {
        Some(node) => wrap_node(ctx, node),
        None => Ok(Value::new_null(ctx.clone())),
    }
}

// https://html.spec.whatwg.org/multipage/common-dom-interfaces.html#reflecting-content-attributes-in-idl-attributes:element
pub(crate) fn get_element_reference_list<'js>(
    ctx: &Ctx<'js>,
    element: NodeId,
    content: &str,
) -> Result<Value<'js>> {
    let world = world_for_node(ctx, element)?;
    let nodes = {
        let world = world.borrow();
        let Some(parsed) = world.document(element) else {
            return Ok(Value::new_null(ctx.clone()));
        };
        attr_associated_elements(&parsed, element, content)
    };
    let Some(nodes) = nodes else {
        return Ok(Value::new_null(ctx.clone()));
    };
    let cached = world
        .borrow()
        .state
        .element_reference_cache
        .get(&(element, content.to_owned()))
        .map(|entry| (entry.nodes.clone(), entry.object.clone()));
    if let Some((contents, object)) = cached
        && contents == nodes
    {
        return object.restore(ctx);
    }
    let array = Array::new(ctx.clone())?;
    for (index, node) in nodes.iter().enumerate() {
        array.set(index, wrap_node(ctx, *node)?)?;
    }
    let value = freeze(ctx, array.into_value())?;
    world.borrow_mut().state.element_reference_cache.insert(
        (element, content.to_owned()),
        ElementReferenceCache {
            nodes,
            object: rquickjs::Persistent::save(ctx, value.clone()),
        },
    );
    Ok(value)
}

pub(crate) fn set_element_reference<'js>(
    ctx: &Ctx<'js>,
    element: NodeId,
    content: &str,
    value: Value<'js>,
) -> Result<()> {
    if value.is_null() || value.is_undefined() {
        return remove_attribute_sync(ctx, element, "", content, false);
    }
    let target = element_argument(ctx, &value)?;
    // https://html.spec.whatwg.org/multipage/common-dom-interfaces.html#reflecting-content-attributes-in-idl-attributes:element
    set_attribute_sync(ctx, element, content, "")?;
    let world = world_for_node(ctx, element)?;
    if let Some(mut parsed) = world.borrow().document_mut(element) {
        parsed
            .document
            .set_element_reference(element.node, content, target);
    }
    Ok(())
}

pub(crate) fn set_element_reference_list<'js>(
    ctx: &Ctx<'js>,
    element: NodeId,
    content: &str,
    value: Value<'js>,
) -> Result<()> {
    if value.is_null() || value.is_undefined() {
        return remove_attribute_sync(ctx, element, "", content, false);
    }
    let elements = element_list_argument(ctx, &value)?;
    set_attribute_sync(ctx, element, content, "")?;
    let world = world_for_node(ctx, element)?;
    if let Some(mut parsed) = world.borrow().document_mut(element) {
        parsed
            .document
            .set_element_reference_list(element.node, content, elements);
    }
    Ok(())
}

// https://html.spec.whatwg.org/multipage/common-dom-interfaces.html#reflecting-content-attributes-in-idl-attributes:element
fn attr_associated_element(
    parsed: &crate::Parsed,
    element: NodeId,
    content: &str,
) -> Option<NodeId> {
    let base = &parsed.document.base;
    if let Some(ElementReference::Element(target)) =
        parsed.document.element_reference(element.node, content)
    {
        return in_reflection_scope(base, element, *target).then_some(*target);
    }
    let value = attr(base, element.node, content)?;
    element_with_id(base, parsed.id, element.node, value)
}

// https://html.spec.whatwg.org/multipage/common-dom-interfaces.html#reflecting-content-attributes-in-idl-attributes:element
fn attr_associated_elements(
    parsed: &crate::Parsed,
    element: NodeId,
    content: &str,
) -> Option<Vec<NodeId>> {
    let base = &parsed.document.base;
    if let Some(ElementReference::Elements(elements)) =
        parsed.document.element_reference(element.node, content)
    {
        return Some(
            elements
                .iter()
                .copied()
                .filter(|target| in_reflection_scope(base, element, *target))
                .collect(),
        );
    }
    let value = attr(base, element.node, content)?;
    Some(
        value
            .split([' ', '\t', '\n', '\u{c}', '\r'])
            .filter(|token| !token.is_empty())
            .filter_map(|id| element_with_id(base, parsed.id, element.node, id))
            .collect(),
    )
}

// https://html.spec.whatwg.org/multipage/common-dom-interfaces.html#reflecting-content-attributes-in-idl-attributes:element
fn in_reflection_scope(base: &blitz_dom::BaseDocument, element: NodeId, candidate: NodeId) -> bool {
    if element.document != candidate.document {
        return false;
    }
    let mut ancestor = shadow_including_parent(base, element.node);
    while let Some(id) = ancestor {
        if is_inclusive_descendant(base, id, candidate.node) {
            return true;
        }
        ancestor = shadow_including_parent(base, id);
    }
    false
}

fn is_inclusive_descendant(
    base: &blitz_dom::BaseDocument,
    ancestor: BlitzId,
    node: BlitzId,
) -> bool {
    let mut current = Some(node);
    while let Some(id) = current {
        if id == ancestor {
            return true;
        }
        current = base.get_node(id).and_then(|node| node.parent);
    }
    false
}

// https://dom.spec.whatwg.org/#concept-id
fn element_with_id(
    base: &blitz_dom::BaseDocument,
    document: u32,
    element: BlitzId,
    id: &str,
) -> Option<NodeId> {
    find_element_by_id(base, document, tree_root(base, element), id)
}

fn tree_root(base: &blitz_dom::BaseDocument, node: BlitzId) -> BlitzId {
    let mut root = node;
    while let Some(parent) = base.get_node(root).and_then(|node| node.parent) {
        root = parent;
    }
    root
}

// https://webidl.spec.whatwg.org/#es-interface
fn element_argument<'js>(ctx: &Ctx<'js>, value: &Value<'js>) -> Result<NodeId> {
    let node = host_node_id(ctx, value)
        .ok_or_else(|| Exception::throw_type(ctx, "value is not an Element"))?;
    let world = world_for_node(ctx, node)?;
    let is_element = world.borrow().document(node).is_some_and(|parsed| {
        parsed
            .document
            .base
            .get_node(node.node)
            .and_then(|node| node.element_data())
            .is_some()
    });
    if !is_element {
        return Err(Exception::throw_type(ctx, "value is not an Element"));
    }
    Ok(node)
}

// https://webidl.spec.whatwg.org/#es-frozen-array
fn element_list_argument<'js>(ctx: &Ctx<'js>, value: &Value<'js>) -> Result<Vec<NodeId>> {
    let not_iterable = || Exception::throw_type(ctx, "value is not iterable");
    let object = value.as_object().ok_or_else(not_iterable)?;
    let method: Value = object.get(Symbol::iterator(ctx.clone()))?;
    let method = method.as_function().ok_or_else(not_iterable)?;
    let mut call = rquickjs::function::Args::new(ctx.clone(), 0);
    call.this(object.clone())?;
    let iterator: Object = method.call_arg(call)?;
    let next: Function = iterator.get("next")?;
    let mut elements = Vec::new();
    loop {
        let mut call = rquickjs::function::Args::new(ctx.clone(), 0);
        call.this(iterator.clone())?;
        let step: Object = next.call_arg(call)?;
        if step.get::<_, bool>("done")? {
            return Ok(elements);
        }
        let item: Value = step.get("value")?;
        elements.push(element_argument(ctx, &item)?);
    }
}

// https://webidl.spec.whatwg.org/#es-frozen-array
fn freeze<'js>(ctx: &Ctx<'js>, value: Value<'js>) -> Result<Value<'js>> {
    let object: Object = ctx.globals().get("Object")?;
    let freeze: Function = object.get("freeze")?;
    freeze.call((value,))
}
