//! The single `Node` wrapper class and its members.

use std::cell::Cell;

use rquickjs::{
    Atom, Ctx, Exception, Result, Value,
    class::{
        ExoticDefineResult, ExoticSetResult, PropertyDefinition, PropertyDescriptor, PropertyName,
        Trace,
    },
};

use crate::dom::tree::NodeId;
use crate::js::world::Handle;

pub(super) mod core;
pub(crate) mod dialog;
pub(super) mod document;
pub(super) mod element;
mod form_named;
pub(super) mod forms;
pub(super) mod html_elements;
pub(super) mod media;
mod named;
pub(super) mod offset;
pub(super) mod scrolling;
mod select_props;
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
    /// The legacy platform object shape this wrapper answers. Set at
    /// construction from the interface brand and never changes: a wrapper
    /// keeps its interface through `adoptNode`, so only construction decides.
    #[qjs(skip_trace)]
    pub(crate) legacy: Cell<LegacyShape>,
}

/// The Web IDL legacy platform object shape a node wrapper answers.
///
/// Most node wrappers are ordinary platform objects with no indexed or named
/// properties. Only these interfaces declare them, so the exotic hooks need
/// the shape rather than a boolean per interface; one value also means a
/// wrapper cannot claim two shapes at once
/// (<https://webidl.spec.whatwg.org/#legacy-platform-object>).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum LegacyShape {
    /// No indexed or named properties.
    None,
    /// A `Document`: named properties only
    /// (<https://html.spec.whatwg.org/multipage/dom.html#dom-document-nameditem>).
    Document,
    /// An `HTMLFormElement`: indexed and named properties
    /// (<https://html.spec.whatwg.org/multipage/forms.html#dom-form-elements>).
    Form,
    /// An `HTMLSelectElement`: indexed properties over its options
    /// (<https://html.spec.whatwg.org/multipage/form-elements.html#dom-select-length>).
    Select,
}

impl LegacyShape {
    /// The shape an interface brand declares.
    pub(crate) fn of_brand(brand: &str) -> Self {
        match brand {
            "Document" | "XMLDocument" | "HTMLDocument" => Self::Document,
            "HTMLFormElement" => Self::Form,
            "HTMLSelectElement" => Self::Select,
            _ => Self::None,
        }
    }
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
        match self.legacy.get() {
            LegacyShape::Form => {
                form_named::named_property(ctx, self.handle.get().0, &atom, &object)
            }
            LegacyShape::Select => select_props::indexed_property(ctx, self.handle.get().0, &atom),
            LegacyShape::Document => {
                named::named_property(ctx, self.handle.get().0, &atom, &object)
            }
            LegacyShape::None => Ok(None),
        }
    }

    // https://webidl.spec.whatwg.org/#legacy-platform-object-ownpropertykeys
    #[qjs(get_own_property_names)]
    fn named_property_names<'js>(
        &self,
        ctx: &Ctx<'js>,
        object: Value<'js>,
    ) -> Result<Vec<PropertyName<'js>>> {
        match self.legacy.get() {
            LegacyShape::Form => {
                form_named::named_property_names(ctx, self.handle.get().0, &object)
            }
            LegacyShape::Select => select_props::indexed_property_names(ctx, self.handle.get().0),
            LegacyShape::Document => named::named_property_names(ctx, self.handle.get().0, &object),
            LegacyShape::None => Ok(Vec::new()),
        }
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
        match self.legacy.get() {
            LegacyShape::Form => {
                form_named::set_named_property(ctx, self.handle.get().0, &atom, &object, &receiver)
            }
            LegacyShape::Select => Ok(select_props::set_indexed_property(
                ctx,
                self.handle.get().0,
                &atom,
                &object,
                &receiver,
            )),
            LegacyShape::Document => {
                if object != receiver {
                    return Ok(ExoticSetResult::Fallthrough);
                }
                named::set_named_property(ctx, self.handle.get().0, &atom, &object, value)
            }
            LegacyShape::None => Ok(ExoticSetResult::Fallthrough),
        }
    }

    // https://webidl.spec.whatwg.org/#legacy-platform-object-delete
    #[qjs(delete)]
    fn delete_named_property<'js>(
        &self,
        ctx: &Ctx<'js>,
        atom: Atom<'js>,
        object: Value<'js>,
    ) -> Result<bool> {
        match self.legacy.get() {
            LegacyShape::Form => {
                form_named::delete_named_property(ctx, self.handle.get().0, &atom, &object)
            }
            LegacyShape::Select => Ok(select_props::delete_indexed_property(
                ctx,
                self.handle.get().0,
                &atom,
            )),
            LegacyShape::Document => {
                named::delete_named_property(ctx, self.handle.get().0, &atom, &object)
            }
            LegacyShape::None => Ok(false),
        }
    }

    // https://webidl.spec.whatwg.org/#legacy-platform-object-defineownproperty
    #[qjs(define_own_property)]
    fn define_named_property<'js>(
        &self,
        ctx: &Ctx<'js>,
        atom: Atom<'js>,
        descriptor: PropertyDefinition<'js>,
    ) -> Result<ExoticDefineResult> {
        match self.legacy.get() {
            LegacyShape::Form => {
                form_named::define_named_property(ctx, self.handle.get().0, &atom, descriptor)
            }
            LegacyShape::Select => Ok(select_props::define_indexed_property(
                ctx,
                self.handle.get().0,
                &atom,
            )),
            LegacyShape::Document | LegacyShape::None => Ok(ExoticDefineResult::Fallthrough),
        }
    }
}
