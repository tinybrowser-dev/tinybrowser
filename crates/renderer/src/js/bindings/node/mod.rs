//! The single `Node` wrapper class and its members.

use std::cell::Cell;

use rquickjs::{Ctx, Exception, Result, class::Trace};

use crate::dom::tree::NodeId;
use crate::js::world::Handle;

pub(super) mod core;
pub(crate) mod document;
pub(super) mod element;
pub(super) mod forms;
pub(super) mod html_elements;
pub(super) mod media;
pub(super) mod scrolling;

pub(super) use core::{compare_node_position, insert_converted_node};
pub(super) use document::{
    construct_node, construct_node_in_frame, create_node, install_custom_construction,
};

pub(super) fn install(ctx: &Ctx<'_>) -> Result<()> {
    core::install(ctx)?;
    document::install(ctx)?;
    element::install(ctx)?;
    forms::install(ctx)?;
    media::install(ctx)?;
    super::geometry::install(ctx)?;
    html_elements::install(ctx)?;
    Ok(())
}

/// The single `Node` wrapper class: one native identity shared by every
/// node interface. Generated contract installers add each interface's
/// members to the prototype brands.js created; the constructor stays
/// illegal.
#[derive(Trace, rquickjs::JsLifetime)]
#[rquickjs::class(rename = "Node")]
pub(crate) struct JsNode {
    /// The node this wrapper points at. Adoption can retarget a wrapper
    /// while its own native method is still on the stack (a listener that
    /// moves the dispatching target into another document), so the identity
    /// is interior-mutable: retargeting needs no `borrow_mut` on the class.
    #[qjs(skip_trace)]
    pub(crate) handle: Cell<Handle>,
}

impl super::host::SharedClass for JsNode {
    // https://webidl.spec.whatwg.org/#es-attributes
    // https://webidl.spec.whatwg.org/#es-operations
    fn require_interface(&self, ctx: &Ctx<'_>, interface: &str) -> Result<()> {
        super::host::require_node_interface(ctx, self.handle.get().0, interface)
    }
}

impl JsNode {
    pub(crate) fn node_id(&self) -> NodeId {
        self.handle.get().0
    }
}

#[rquickjs::methods]

impl JsNode {
    #[qjs(constructor)]
    fn ctor(ctx: Ctx<'_>) -> Result<Self> {
        Err(Exception::throw_type(&ctx, "Illegal constructor"))
    }
}
