use rquickjs::{Class, Ctx, FromJs, Persistent, Result, Value, prelude::Func};

use super::range::{JsRange, wrap_range};
use super::{host_node_id, world};
use crate::documents::SelectionDirection;
use crate::dom::range::{BoundaryPoint, RangeState};

pub(crate) fn install(ctx: &Ctx<'_>) -> Result<()> {
    let host = crate::js::bridge::object(ctx)?;
    host.set("__tbSelectionRange", Func::from(selection_range))?;
    host.set("__tbSelectionDirection", Func::from(selection_direction))?;
    host.set("__tbSelectionAddRange", Func::from(add_range))?;
    host.set("__tbSelectionSetRange", Func::from(set_range))?;
    host.set("__tbSelectionRemoveRange", Func::from(remove_range))?;
    host.set("__tbSelectionClear", Func::from(clear))?;
    Ok(())
}

// https://w3c.github.io/selection-api/#selection-interface
fn selection_range<'js>(ctx: Ctx<'js>, document: Value<'js>) -> Result<Value<'js>> {
    let Some(document) = host_node_id(&ctx, &document) else {
        return Ok(Value::new_null(ctx));
    };
    let caller = world(&ctx)?;
    let slot = {
        let borrowed = caller.borrow();
        let Some(parsed) = borrowed.document(document) else {
            return Ok(Value::new_null(ctx.clone()));
        };
        parsed
            .document
            .document_selection()
            .map(|selection| selection.range_slot)
    };
    let Some(slot) = slot else {
        caller
            .borrow_mut()
            .caches
            .selection_range
            .remove(&document.document);
        return Ok(Value::new_null(ctx));
    };
    let cached = caller
        .borrow()
        .caches
        .selection_range
        .get(&document.document)
        .filter(|(cached_slot, _)| *cached_slot == slot)
        .map(|(_, value)| value.clone());
    if let Some(saved) = cached {
        return saved.restore(&ctx);
    }
    let retained = {
        let borrowed = caller.borrow();
        let Some(mut parsed) = borrowed.document_mut_by_id(document.document) else {
            return Ok(Value::new_null(ctx.clone()));
        };
        parsed.document.side.ranges.retain(slot)
    };
    if !retained {
        return Ok(Value::new_null(ctx));
    }
    let range = wrap_range(&ctx, document.document, slot)?;
    caller.borrow_mut().caches.selection_range.insert(
        document.document,
        (slot, Persistent::save(&ctx, range.clone())),
    );
    Ok(range)
}

// https://w3c.github.io/selection-api/#dom-selection-direction
fn selection_direction<'js>(ctx: Ctx<'js>, document: Value<'js>) -> Result<&'static str> {
    let Some(document) = host_node_id(&ctx, &document) else {
        return Ok("none");
    };
    let caller = world(&ctx)?;
    let borrowed = caller.borrow();
    let Some(parsed) = borrowed.document(document) else {
        return Ok("none");
    };
    Ok(parsed
        .document
        .document_selection()
        .map_or("none", |selection| selection.direction.as_str()))
}

// https://w3c.github.io/selection-api/#dom-selection-addrange
fn add_range<'js>(ctx: Ctx<'js>, document: Value<'js>, candidate: Value<'js>) -> Result<()> {
    let Some(document) = host_node_id(&ctx, &document) else {
        return Ok(());
    };
    let Ok(range) = Class::<JsRange>::from_js(&ctx, candidate.clone()) else {
        return Ok(());
    };
    let (range_doc, slot) = range.borrow().registration();
    if range_doc != document.document {
        return Ok(());
    }
    let caller = world(&ctx)?;
    let attached = {
        let borrowed = caller.borrow();
        let Some(mut parsed) = borrowed.document_mut_by_id(document.document) else {
            return Ok(());
        };
        parsed
            .document
            .set_document_selection_slot(slot, SelectionDirection::Directionless)
    };
    if attached {
        caller
            .borrow_mut()
            .caches
            .selection_range
            .insert(document.document, (slot, Persistent::save(&ctx, candidate)));
    }
    Ok(())
}

// https://w3c.github.io/selection-api/#selection-interface
fn set_range<'js>(
    ctx: Ctx<'js>,
    document: Value<'js>,
    start: Value<'js>,
    start_offset: u32,
    end: Value<'js>,
    end_offset: u32,
    direction: String,
) -> Result<()> {
    let Some(document) = host_node_id(&ctx, &document) else {
        return Ok(());
    };
    let (Some(start), Some(end)) = (host_node_id(&ctx, &start), host_node_id(&ctx, &end)) else {
        return Ok(());
    };
    let caller = world(&ctx)?;
    {
        let borrowed = caller.borrow();
        let Some(mut parsed) = borrowed.document_mut_by_id(document.document) else {
            return Ok(());
        };
        // https://dom.spec.whatwg.org/#concept-range-bp-set
        let end_point = BoundaryPoint {
            node: end,
            offset: end_offset.min(crate::dom::range::node_length(&parsed.document.base, end)),
        };
        let start_point = if crate::dom::range::same_root(&parsed.document.base, start, end) {
            let candidate = BoundaryPoint {
                node: start,
                offset: start_offset
                    .min(crate::dom::range::node_length(&parsed.document.base, start)),
            };
            if crate::dom::range::position(
                &parsed.document.base,
                document.document,
                candidate,
                end_point,
            ) == std::cmp::Ordering::Greater
            {
                end_point
            } else {
                candidate
            }
        } else {
            end_point
        };
        let state = RangeState {
            start: start_point,
            end: end_point,
            attr: None,
        };
        parsed
            .document
            .set_document_selection_range(state, SelectionDirection::parse(&direction));
    }
    caller
        .borrow_mut()
        .caches
        .selection_range
        .remove(&document.document);
    Ok(())
}

// https://w3c.github.io/selection-api/#dom-selection-removerange
fn remove_range<'js>(ctx: Ctx<'js>, document: Value<'js>, candidate: Value<'js>) -> Result<bool> {
    let Some(document) = host_node_id(&ctx, &document) else {
        return Ok(false);
    };
    let Ok(range) = Class::<JsRange>::from_js(&ctx, candidate) else {
        return Ok(false);
    };
    let (range_doc, slot) = range.borrow().registration();
    if range_doc != document.document {
        return Ok(false);
    }
    let caller = world(&ctx)?;
    let matched = {
        let borrowed = caller.borrow();
        let Some(mut parsed) = borrowed.document_mut_by_id(document.document) else {
            return Ok(false);
        };
        if parsed
            .document
            .document_selection()
            .is_some_and(|selection| selection.range_slot == slot)
        {
            parsed.document.clear_document_selection();
            true
        } else {
            false
        }
    };
    if matched {
        caller
            .borrow_mut()
            .caches
            .selection_range
            .remove(&document.document);
    }
    Ok(matched)
}

// https://w3c.github.io/selection-api/#dom-selection-removeallranges
fn clear<'js>(ctx: Ctx<'js>, document: Value<'js>) -> Result<()> {
    let Some(document) = host_node_id(&ctx, &document) else {
        return Ok(());
    };
    let caller = world(&ctx)?;
    {
        let borrowed = caller.borrow();
        if let Some(mut parsed) = borrowed.document_mut_by_id(document.document) {
            parsed.document.clear_document_selection();
        }
    }
    caller
        .borrow_mut()
        .caches
        .selection_range
        .remove(&document.document);
    Ok(())
}
