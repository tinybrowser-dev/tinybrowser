use rquickjs::{Array, Ctx, Result, Value, prelude::Func};

use super::{host_node_id, realm_registry, world, wrap_node};

pub(crate) fn install(ctx: &Ctx<'_>) -> Result<()> {
    let host = crate::js::bridge::object(ctx)?;
    host.set("__tbSelectionState", Func::from(state))?;
    host.set("__tbSelectionCommit", Func::from(commit))?;
    host.set("__tbSelectionClear", Func::from(clear))?;
    Ok(())
}

// https://w3c.github.io/selection-api/#selection-interface
fn state<'js>(ctx: Ctx<'js>, document: Value<'js>) -> Result<Value<'js>> {
    let Some(document) = host_node_id(&ctx, &document) else {
        return Ok(Value::new_null(ctx));
    };
    let selection = realm_registry(&ctx)?
        .borrow()
        .document_selection(document.document)
        .cloned();
    let Some(selection) = selection else {
        return Ok(Value::new_null(ctx));
    };
    let world = world(&ctx)?;
    let node_alive = |node| {
        world
            .borrow()
            .document(node)
            .is_some_and(|parsed| parsed.document.base.get_node(node.node).is_some())
    };
    let start = selection.start.filter(|(node, _)| node_alive(*node));
    let end = selection.end.filter(|(node, _)| node_alive(*node));
    let values = Array::new(ctx.clone())?;
    values.set(0, selection.version)?;
    values.set(
        1,
        match start {
            Some((node, _)) => wrap_node(&ctx, node)?,
            None => Value::new_null(ctx.clone()),
        },
    )?;
    values.set(2, start.map_or(0, |(_, offset)| offset))?;
    values.set(
        3,
        match end {
            Some((node, _)) => wrap_node(&ctx, node)?,
            None => Value::new_null(ctx.clone()),
        },
    )?;
    values.set(4, end.map_or(0, |(_, offset)| offset))?;
    values.set(5, selection.direction)?;
    Ok(values.into_value())
}

// https://w3c.github.io/selection-api/#selection-interface
fn commit<'js>(
    ctx: Ctx<'js>,
    document: Value<'js>,
    start: Value<'js>,
    start_offset: u32,
    end: Value<'js>,
    end_offset: u32,
    direction: u32,
) -> Result<u32> {
    let Some(document) = host_node_id(&ctx, &document) else {
        return Ok(0);
    };
    let start = host_node_id(&ctx, &start).map(|node| (node, start_offset));
    let end = host_node_id(&ctx, &end).map(|node| (node, end_offset));
    let (Some(start), Some(end)) = (start, end) else {
        return Ok(0);
    };
    let version = realm_registry(&ctx)?
        .borrow_mut()
        .update_document_selection(
            document.document,
            Some(start),
            Some(end),
            u8::try_from(direction).unwrap_or(0),
        );
    Ok(version)
}

// https://w3c.github.io/selection-api/#selection-interface
fn clear<'js>(ctx: Ctx<'js>, document: Value<'js>) -> Result<u32> {
    let Some(document) = host_node_id(&ctx, &document) else {
        return Ok(0);
    };
    let version = realm_registry(&ctx)?
        .borrow_mut()
        .update_document_selection(document.document, None, None, 0);
    Ok(version)
}
