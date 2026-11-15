//! Realm wrapper for rendered-text collection.
//!
//! The collection algorithm lives in `crate::dom::text`; this module resolves
//! the calling realm's document and applies it.

use super::world_for_node;
use crate::dom::tree::NodeId;
use rquickjs::{Ctx, Result};

pub(crate) fn rendered_text(ctx: &Ctx<'_>, id: NodeId) -> Result<String> {
    let owner = world_for_node(ctx, id)?;
    let owner = owner.borrow_mut();
    let Some(mut parsed) = owner.document_mut(id) else {
        return Ok(String::new());
    };
    // Rendered-text collection asks each element for its resolved `display`,
    // so layout (and the style traversal that feeds it) has to be current
    // (<https://html.spec.whatwg.org/multipage/dom.html#the-innertext-idl-attribute>).
    parsed.document.base.resolve(crate::animation_seconds());
    Ok(crate::dom::text::rendered_text(&parsed.document.base, id))
}
