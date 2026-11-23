use super::world_for_node;
use crate::dom::tree::NodeId;
use rquickjs::{Ctx, Result};

pub(crate) fn rendered_text(ctx: &Ctx<'_>, id: NodeId) -> Result<String> {
    let owner = world_for_node(ctx, id)?;
    let owner = owner.borrow_mut();
    let Some(mut parsed) = owner.document_mut(id) else {
        return Ok(String::new());
    };
    // https://html.spec.whatwg.org/multipage/dom.html#the-innertext-idl-attribute
    parsed
        .document
        .base
        .resolve_if_needed(crate::animation_seconds());
    Ok(crate::dom::text::rendered_text(&parsed.document.base, id))
}
