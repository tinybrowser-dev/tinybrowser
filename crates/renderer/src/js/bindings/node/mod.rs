//! The single `Node` wrapper class and its members.

use std::cell::Cell;

use rquickjs::{
    Atom, Ctx, Exception, Result, Value,
    class::{ExoticSetResult, PropertyDescriptor, PropertyName, Trace},
};

use crate::dom::tree::NodeId;
use crate::js::world::Handle;

pub(super) mod core;
pub(crate) mod dialog;
pub(super) mod document;
pub(super) mod element;
pub(super) mod forms;
pub(super) mod html_elements;
pub(super) mod media;
mod named;
pub(super) mod offset;
pub(super) mod scrolling;
mod zoom;

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
#[rquickjs::class(rename = "Node", exotic)]
pub(crate) struct JsNode {
    /// The node this wrapper points at. Adoption can retarget a wrapper
    /// while its own native method is still on the stack (a listener that
    /// moves the dispatching target into another document), so the identity
    /// is interior-mutable: retargeting needs no `borrow_mut` on the class.
    #[qjs(skip_trace)]
    pub(crate) handle: Cell<Handle>,
    /// Whether this wrapper is a `Document`, the only node interface with
    /// named properties. Set at construction and never changes: a document is
    /// never adopted into another document, which is the only way a wrapper
    /// changes its brand.
    #[qjs(skip_trace)]
    pub(crate) document: Cell<bool>,
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

#[rquickjs::exotic]
impl JsNode {
    // https://webidl.spec.whatwg.org/#legacy-platform-object-getownproperty
    #[qjs(get_own_property)]
    fn named_property<'js>(
        &self,
        ctx: &Ctx<'js>,
        atom: Atom<'js>,
        object: Value<'js>,
    ) -> Result<Option<PropertyDescriptor<'js>>> {
        if !self.document.get() {
            return Ok(None);
        }
        named::named_property(ctx, self.handle.get().0, &atom, &object)
    }

    // https://webidl.spec.whatwg.org/#legacy-platform-object-ownpropertykeys
    #[qjs(get_own_property_names)]
    fn named_property_names<'js>(
        &self,
        ctx: &Ctx<'js>,
        object: Value<'js>,
    ) -> Result<Vec<PropertyName<'js>>> {
        if !self.document.get() {
            return Ok(Vec::new());
        }
        named::named_property_names(ctx, self.handle.get().0, &object)
    }

    // https://webidl.spec.whatwg.org/#legacy-platform-object-set
    #[qjs(set)]
    fn set_named_property<'js>(
        &self,
        ctx: &Ctx<'js>,
        atom: Atom<'js>,
        object: Value<'js>,
        receiver: Value<'js>,
        value: Value<'js>,
    ) -> Result<ExoticSetResult> {
        // A different receiver means `Reflect.set` or a `super` write, which
        // the ordinary prototype walk handles
        // (<https://webidl.spec.whatwg.org/#legacy-platform-object-set>).
        if !self.document.get() || object != receiver {
            return Ok(ExoticSetResult::Fallthrough);
        }
        named::set_named_property(ctx, self.handle.get().0, &atom, &object, value)
    }

    // https://webidl.spec.whatwg.org/#legacy-platform-object-delete
    #[qjs(delete)]
    fn delete_named_property<'js>(
        &self,
        ctx: &Ctx<'js>,
        atom: Atom<'js>,
        object: Value<'js>,
    ) -> Result<bool> {
        if !self.document.get() {
            return Ok(false);
        }
        named::delete_named_property(ctx, self.handle.get().0, &atom, &object)
    }
}
