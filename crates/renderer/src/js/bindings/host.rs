use rquickjs::atom::PredefinedAtom;
use rquickjs::class::JsClass;
use rquickjs::function::{Constructor, NativeFunc, Params};
use rquickjs::object::{AsProperty, Property, PropertyFlags};
use rquickjs::{
    Class, Coerced, Ctx, Exception, FromJs, Function, Object, Persistent, Result, Value,
};

use super::{world, world_for_node};
use crate::dom::tree::NodeId;
pub(crate) use crate::js::world::NodeReference;

pub(crate) type Dispatch = for<'a, 'js> fn(Operation, &Params<'a, 'js>) -> Result<Value<'js>>;

#[derive(Clone, Copy)]
pub(crate) struct Operation(usize);

impl Operation {
    pub(crate) const fn new(index: usize) -> Self {
        Self(index)
    }

    pub(crate) const fn index(self) -> usize {
        self.0
    }
}

pub(crate) struct Member {
    pub(crate) name: &'static str,
    // https://webidl.spec.whatwg.org/#SecureContext
    pub(crate) secure_context: bool,
    pub(crate) kind: MemberKind,
}

#[derive(Clone, Copy)]
pub(crate) enum MemberKind {
    Attribute {
        getter: Operation,
        setter: Option<Operation>,
    },
    Method {
        operation: Operation,
        length: usize,
    },
}

pub(crate) struct Constant {
    pub(crate) name: &'static str,
    pub(crate) value: u16,
    // https://webidl.spec.whatwg.org/#SecureContext
    pub(crate) secure_context: bool,
}

#[derive(Clone, Copy)]
pub(crate) struct HostCall {
    dispatch: Dispatch,
    operation: Operation,
}

impl HostCall {
    pub(crate) const fn new(dispatch: Dispatch, operation: Operation) -> Self {
        Self {
            dispatch,
            operation,
        }
    }
}

impl NativeFunc for HostCall {
    fn call<'js>(&self, params: Params<'_, 'js>) -> Result<Value<'js>> {
        (self.dispatch)(self.operation, &params)
    }
}

// https://w3c.github.io/webappsec-secure-contexts/#is-settings-object-contextually-secure
pub(crate) fn is_secure_context(ctx: &Ctx<'_>) -> Result<bool> {
    Ok(world(ctx)?.borrow().is_secure_context())
}

pub(crate) fn prototype<'js>(
    ctx: &Ctx<'js>,
    name: &'static str,
    parent_intrinsic: Option<&str>,
    members: &[Member],
    constants: &[Constant],
    dispatch: Dispatch,
) -> Result<Object<'js>> {
    // https://webidl.spec.whatwg.org/#interface-prototype-object
    let realm = world(ctx)?;
    if let Some(prototype) = realm.borrow().brand(name) {
        return prototype.restore(ctx);
    }
    let prototype = Object::new(ctx.clone())?;
    if let Some(parent) = parent_intrinsic {
        let constructor: Object = ctx.globals().get(parent)?;
        let parent: Object = constructor.get("prototype")?;
        prototype.set_prototype(Some(&parent))?;
    }
    install_members(&prototype, members, constants, dispatch)?;
    // https://webidl.spec.whatwg.org/#interface-prototype-object
    prototype.prop(
        PredefinedAtom::SymbolToStringTag,
        Property::from(name).configurable(),
    )?;
    realm
        .borrow_mut()
        .intern_brand(name, Persistent::save(ctx, prototype.clone()));
    Ok(prototype)
}

// https://webidl.spec.whatwg.org/#es-constants
#[inline(never)]
pub(crate) fn install_interface(
    ctx: &Ctx<'_>,
    name: &str,
    members: &[Member],
    constants: &[Constant],
    unscopables: &[&str],
    dispatch: Dispatch,
) -> Result<()> {
    let constructor: Object = ctx.globals().get(name)?;
    let prototype: Object = constructor.get("prototype")?;
    install_members(&prototype, members, constants, dispatch)?;
    install_constants(&constructor, constants)?;
    install_unscopables(&prototype, unscopables)
}

pub(crate) fn install_members(
    prototype: &Object<'_>,
    members: &[Member],
    constants: &[Constant],
    dispatch: Dispatch,
) -> Result<()> {
    // https://webidl.spec.whatwg.org/#es-attributes
    // https://webidl.spec.whatwg.org/#es-operations
    let ctx = prototype.ctx();
    let secure_context = is_secure_context(ctx)?;
    for member in members {
        if member.secure_context && !secure_context {
            continue;
        }
        match member.kind {
            MemberKind::Attribute { getter, setter } => {
                let getter = Function::new_native(ctx.clone(), HostCall::new(dispatch, getter))?
                    .with_name(format!("get {}", member.name))?;
                let setter = setter
                    .map(|operation| {
                        Function::new_native(ctx.clone(), HostCall::new(dispatch, operation))?
                            .with_name(format!("set {}", member.name))?
                            .with_length(1)
                    })
                    .transpose()?;
                prototype.prop(member.name, Attribute { getter, setter })?;
            }
            MemberKind::Method { operation, length } => {
                let function =
                    Function::new_native(ctx.clone(), HostCall::new(dispatch, operation))?
                        .with_name(member.name)?
                        .with_length(length)?;
                prototype.prop(
                    member.name,
                    Property::from(function)
                        .writable()
                        .enumerable()
                        .configurable(),
                )?;
            }
        }
    }
    install_constants(prototype, constants)
}

// https://webidl.spec.whatwg.org/#es-operations
pub(crate) fn install_unscopables(prototype: &Object<'_>, names: &[&str]) -> Result<()> {
    if names.is_empty() {
        return Ok(());
    }
    let ctx = prototype.ctx();
    let existing: Value = prototype.get(PredefinedAtom::SymbolUnscopables)?;
    let unscopables: Object = if existing.is_undefined() || existing.is_null() {
        Object::new(ctx.clone())?
    } else {
        existing
            .into_object()
            .ok_or_else(|| Exception::throw_internal(ctx, "@@unscopables is not an object"))?
    };
    for name in names {
        unscopables.set(*name, true)?;
    }
    prototype.prop(
        PredefinedAtom::SymbolUnscopables,
        Property::from(unscopables).configurable(),
    )
}

// https://html.spec.whatwg.org/multipage/common-dom-interfaces.html#reflect
#[inline(never)]
pub(crate) fn reflect_string<'js>(
    ctx: &Ctx<'js>,
    element: NodeId,
    name: &str,
) -> Result<rquickjs::String<'js>> {
    let world = super::world(ctx)?;
    let value = world
        .borrow()
        .document(element)
        .and_then(|parsed| {
            crate::dom::tree::attr(&parsed.document.base, element.node, name).map(str::to_owned)
        })
        .unwrap_or_default();
    rquickjs::String::from_str(ctx.clone(), &value)
}

// https://html.spec.whatwg.org/multipage/common-dom-interfaces.html#limited-to-only-known-values
#[inline(never)]
pub(crate) fn reflect_enumerated_attribute<'js, T: crate::js::bindings::enumerated::Enumerated>(
    ctx: &Ctx<'js>,
    element: NodeId,
    name: &str,
    defaults: crate::js::bindings::enumerated::Defaults<T>,
) -> Result<rquickjs::String<'js>> {
    let raw = reflected_attribute_value(ctx, element, name);
    let value = crate::js::bindings::enumerated::reflect_enumerated(raw.as_deref(), defaults);
    rquickjs::String::from_str(ctx.clone(), value)
}

fn reflected_attribute_value(ctx: &Ctx<'_>, element: NodeId, name: &str) -> Option<String> {
    let world = super::world(ctx).ok()?;
    let parsed = world.borrow();
    parsed.document(element).and_then(|parsed| {
        crate::dom::tree::attr(&parsed.document.base, element.node, name).map(str::to_owned)
    })
}

// https://w3c.github.io/aria/#enumerated-attribute-values-html
pub(crate) fn reflect_enumerated_attribute_nullable<
    'js,
    T: crate::js::bindings::enumerated::Enumerated,
>(
    ctx: &Ctx<'js>,
    element: NodeId,
    name: &str,
    defaults: crate::js::bindings::enumerated::Defaults<T>,
) -> Result<Option<rquickjs::String<'js>>> {
    let world = super::world(ctx)?;
    let raw = world.borrow().document(element).and_then(|parsed| {
        crate::dom::tree::attr(&parsed.document.base, element.node, name).map(str::to_owned)
    });
    if raw.is_none() {
        // https://w3c.github.io/aria/#enumerated-attribute-values-html
        let nulled = world
            .borrow()
            .document(element)
            .is_some_and(|parsed| parsed.document.is_enumerated_null(element.node, name));
        if nulled {
            return Ok(None);
        }
    }
    match crate::js::bindings::enumerated::reflect_enumerated_nullable(raw.as_deref(), defaults) {
        Some(value) => Ok(Some(rquickjs::String::from_str(ctx.clone(), value)?)),
        None => Ok(None),
    }
}

// https://w3c.github.io/aria/#enumerated-attribute-values-html
pub(crate) fn reflect_set_enumerated_nullable(
    ctx: &Ctx<'_>,
    element: NodeId,
    name: &str,
    value: Option<rquickjs::String<'_>>,
) -> Result<()> {
    if let Some(value) = value {
        reflect_set_string(ctx, element, name, &value)
    } else {
        super::remove_attribute_sync(ctx, element, "", name, false)?;
        let world = super::world_for_node(ctx, element)?;
        if let Some(mut parsed) = world.borrow().document_mut(element) {
            parsed.document.mark_enumerated_null(element.node, name);
        }
        Ok(())
    }
}

// https://html.spec.whatwg.org/multipage/common-dom-interfaces.html#reflect
pub(crate) fn reflect_bool(ctx: &Ctx<'_>, element: NodeId, name: &str) -> Result<bool> {
    let world = super::world(ctx)?;
    Ok(world.borrow().document(element).is_some_and(|parsed| {
        crate::dom::tree::attr(&parsed.document.base, element.node, name).is_some()
    }))
}

pub(crate) fn reflect_set_string(
    ctx: &Ctx<'_>,
    element: NodeId,
    name: &str,
    value: &rquickjs::String<'_>,
) -> Result<()> {
    let value = value.to_string()?;
    super::set_attribute_sync(ctx, element, name, &value)
}

pub(crate) fn reflect_set_bool(
    ctx: &Ctx<'_>,
    element: NodeId,
    name: &str,
    value: bool,
) -> Result<()> {
    if value {
        let empty = rquickjs::String::from_str(ctx.clone(), "")?;
        reflect_set_string(ctx, element, name, &empty)
    } else {
        super::remove_attribute_sync(ctx, element, "", name, false)
    }
}

// https://html.spec.whatwg.org/multipage/common-dom-interfaces.html#reflecting-content-attributes-in-idl-attributes — values above i32::MAX write 0 to match WPT reflection.js idlDomExpected, whereas Chromium writes shortest-string
pub(crate) fn reflect_set_long(
    ctx: &Ctx<'_>,
    element: NodeId,
    name: &str,
    value: i32,
) -> Result<()> {
    super::set_attribute_sync(ctx, element, name, &value.to_string())
}

// https://html.spec.whatwg.org/multipage/common-dom-interfaces.html#reflecting-content-attributes-in-idl-attributes — values above i32::MAX write 0 to match WPT reflection.js idlDomExpected, whereas Chromium writes shortest-string
pub(crate) fn reflect_set_ulong(
    ctx: &Ctx<'_>,
    element: NodeId,
    name: &str,
    value: u32,
) -> Result<()> {
    reflect_set_ulong_defaulting(ctx, element, name, value, 0)
}

// https://html.spec.whatwg.org/multipage/common-dom-interfaces.html#reflecting-content-attributes-in-idl-attributes — values above i32::MAX write 0 to match WPT reflection.js idlDomExpected, whereas Chromium writes shortest-string
pub(crate) fn reflect_set_ulong_defaulting(
    ctx: &Ctx<'_>,
    element: NodeId,
    name: &str,
    value: u32,
    default: u32,
) -> Result<()> {
    let value = if value > i32::MAX as u32 {
        default
    } else {
        value
    };
    super::set_attribute_sync(ctx, element, name, &value.to_string())
}

// https://html.spec.whatwg.org/multipage/common-dom-interfaces.html#reflecting-content-attributes-in-idl-attributes — values above i32::MAX write 0 to match WPT reflection.js idlDomExpected, whereas Chromium writes shortest-string
pub(crate) fn reflect_set_double(
    ctx: &Ctx<'_>,
    element: NodeId,
    name: &str,
    value: f64,
) -> Result<()> {
    let number = rquickjs::Value::new_number(ctx.clone(), value);
    let js = Coerced::<rquickjs::String>::from_js(ctx, number).map(|string| string.0)?;
    super::set_attribute_sync(ctx, element, name, &js.to_string()?)
}

// https://webidl.spec.whatwg.org/#dfn-platform-object
pub(crate) fn instance<'js, T: JsClass<'js>>(ctx: &Ctx<'js>, value: T) -> Result<Class<'js, T>> {
    let prototype = T::prototype(ctx)?
        .ok_or_else(|| Exception::throw_internal(ctx, "native interface has no prototype"))?;
    instance_proto(value, prototype)
}

// https://webidl.spec.whatwg.org/#dfn-platform-object
pub(crate) fn instance_proto<'js, T: JsClass<'js>>(
    value: T,
    prototype: Object<'js>,
) -> Result<Class<'js, T>> {
    Class::instance_proto(value, prototype)
}

// https://dom.spec.whatwg.org/#concept-node
pub(crate) fn instance_for_node<'js, T: JsClass<'js>>(
    ctx: &Ctx<'js>,
    node: NodeId,
    value: T,
) -> Result<Class<'js, T>> {
    let owner = world_for_node(ctx, node)?;
    let prototype = owner
        .borrow()
        .brand(T::NAME)
        .ok_or_else(|| Exception::throw_internal(ctx, "node owner has no interface prototype"))?;
    instance_proto(value, prototype.restore(ctx)?)
}

pub(crate) fn constructor<'js>(
    ctx: &Ctx<'js>,
    name: &str,
    length: usize,
    prototype: &Object<'js>,
    constants: &[Constant],
    callback: HostCall,
) -> Result<Constructor<'js>> {
    // https://webidl.spec.whatwg.org/#es-interface-call
    let function = Function::new_native(ctx.clone(), callback)?.with_constructor(true);
    function.prop("prototype", Property::from(prototype.clone()))?;
    prototype.prop(
        "constructor",
        Property::from(function.clone()).writable().configurable(),
    )?;
    let constructor = function.into_value().into_constructor().ok_or_else(|| {
        Exception::throw_internal(ctx, "native interface object is not a constructor")
    })?;
    constructor.set_name(name)?;
    constructor.set_length(length)?;
    install_constants(&constructor, constants)?;
    Ok(constructor)
}

pub(crate) fn constructor_prototype<'js, T: JsClass<'js>>(
    params: &Params<'_, 'js>,
) -> Result<Object<'js>> {
    // https://webidl.spec.whatwg.org/#internally-create-a-new-object-implementing-the-interface — looks up new.target prototype after argument conversion per WebIDL, unlike Chromium and Firefox
    let target = params
        .this()
        .into_function()
        .ok_or_else(|| Exception::throw_internal(params.ctx(), "constructor has no new.target"))?;
    let prototype: Value = target.get("prototype")?;
    if let Some(prototype) = prototype.into_object() {
        return Ok(prototype);
    }
    T::prototype(&target.realm()?)?
        .ok_or_else(|| Exception::throw_internal(params.ctx(), "native interface has no prototype"))
}

pub(crate) fn install_constants(target: &Object<'_>, constants: &[Constant]) -> Result<()> {
    // https://webidl.spec.whatwg.org/#define-the-constants
    let secure_context = is_secure_context(target.ctx())?;
    for constant in constants {
        if constant.secure_context && !secure_context {
            continue;
        }
        target.prop(constant.name, Property::from(constant.value).enumerable())?;
    }
    Ok(())
}

struct Attribute<'js> {
    getter: Function<'js>,
    setter: Option<Function<'js>>,
}

impl<'js> AsProperty<'js, ()> for Attribute<'js> {
    fn config(self, ctx: &Ctx<'js>) -> Result<(PropertyFlags, Value<'js>, Value<'js>, Value<'js>)> {
        use rquickjs::qjs;
        let flags = qjs::JS_PROP_HAS_GET
            | qjs::JS_PROP_HAS_SET
            | qjs::JS_PROP_HAS_ENUMERABLE
            | qjs::JS_PROP_ENUMERABLE
            | qjs::JS_PROP_HAS_CONFIGURABLE
            | qjs::JS_PROP_CONFIGURABLE;
        let flags = PropertyFlags::try_from(flags)
            .map_err(|_| Exception::throw_internal(ctx, "invalid native property flags"))?;
        Ok((
            flags,
            Value::new_undefined(ctx.clone()),
            self.getter.into_value(),
            self.setter
                .map_or_else(|| Value::new_undefined(ctx.clone()), Function::into_value),
        ))
    }
}

pub(crate) fn require_constructor(params: &Params<'_, '_>) -> Result<()> {
    // https://webidl.spec.whatwg.org/#es-interface-call
    if params.is_constructor() {
        Ok(())
    } else {
        Err(Exception::throw_type(
            params.ctx(),
            "constructor requires new",
        ))
    }
}

#[inline(never)]
pub(crate) fn require_arguments(params: &Params<'_, '_>, minimum: usize) -> Result<()> {
    // https://webidl.spec.whatwg.org/#dfn-overload-resolution-algorithm
    if params.len() >= minimum {
        Ok(())
    } else {
        Err(Exception::throw_type(
            params.ctx(),
            "missing required argument",
        ))
    }
}

#[inline(never)]
pub(crate) fn receiver<'js, T: JsClass<'js>>(params: &Params<'_, 'js>) -> Result<Class<'js, T>> {
    // https://webidl.spec.whatwg.org/#es-attributes
    Class::from_value(&params.this())
        .map_err(|_| Exception::throw_type(params.ctx(), "incompatible receiver"))
}

#[inline(never)]
pub(crate) fn node_argument<'js>(ctx: &Ctx<'js>, value: &Value<'js>) -> Result<NodeReference> {
    // https://webidl.spec.whatwg.org/#js-interface
    // https://dom.spec.whatwg.org/#interface-attr
    if let Some(id) = super::host_node_id(ctx, value) {
        return Ok(NodeReference::Tree(id));
    }
    if let Ok(attr) = Class::<super::JsAttr>::from_value(value) {
        let attr = attr.borrow();
        return Ok(NodeReference::Attribute {
            scope: attr.scope.0,
            id: attr.id,
        });
    }
    Err(Exception::throw_type(ctx, "argument is not a Node"))
}

#[inline(never)]
pub(crate) fn nullable_node_argument<'js>(
    ctx: &Ctx<'js>,
    value: &Value<'js>,
) -> Result<Option<NodeReference>> {
    if value.is_null() || value.is_undefined() {
        Ok(None)
    } else {
        node_argument(ctx, value).map(Some)
    }
}

// https://html.spec.whatwg.org/multipage/urls-and-fetching.html#reflecting-content-attributes-in-idl-attributes — values above i32::MAX write 0 to match WPT reflection.js idlDomExpected, whereas Chromium writes shortest-string
// https://html.spec.whatwg.org/multipage/urls-and-fetching.html#parse-a-url
#[inline(never)]
pub(crate) fn reflect_url_string(
    ctx: &Ctx<'_>,
    id: NodeId,
    content: &str,
) -> Result<crate::dom_string::DomString> {
    let world_rc = world(ctx)?;
    let (raw, encoding) = {
        let world = world_rc.borrow();
        let Some(parsed) = world.document(id) else {
            return Ok(crate::dom_string::DomString::default());
        };
        let raw =
            crate::dom::tree::attr(&parsed.document.base, id.node, content).map(str::to_owned);
        let encoding = encoding_rs::Encoding::for_label(parsed.character_set.as_bytes())
            .unwrap_or(encoding_rs::UTF_8);
        (raw, encoding)
    };
    let Some(raw) = raw else {
        return Ok(crate::dom_string::DomString::default());
    };
    let base = super::document::document_base_url_string(ctx, id);
    Ok(url::Url::parse(&base)
        .ok()
        .and_then(|base| crate::js::url_parts::encoding_parse_url(&raw, &base, encoding))
        .map_or(raw, |url| url.to_string())
        .into())
}

pub(crate) trait SharedClass {
    fn require_interface(&self, ctx: &Ctx<'_>, interface: &str) -> Result<()>;
}

fn node_interface_matches(
    data: Option<&blitz_dom::NodeData>,
    xml_document: bool,
    html_document: bool,
    interface: &str,
) -> Option<bool> {
    use blitz_dom::NodeData;
    let is_character_data = matches!(
        data,
        Some(
            NodeData::Text(_)
                | NodeData::Comment { .. }
                | NodeData::ProcessingInstruction { .. }
                | NodeData::CDataSection { .. }
        )
    );
    Some(match interface {
        // https://dom.spec.whatwg.org/#interface-eventtarget
        "Node" | "EventTarget" => data.is_some(),
        "Document" => matches!(data, Some(NodeData::Document(_))),
        "HTMLDocument" => matches!(data, Some(NodeData::Document(_))) && html_document,
        "XMLDocument" => matches!(data, Some(NodeData::Document(_))) && xml_document,
        // https://dom.spec.whatwg.org/#interface-shadowroot
        "DocumentFragment" => {
            matches!(
                data,
                Some(NodeData::Fragment { .. } | NodeData::ShadowRoot { .. })
            )
        }
        "ShadowRoot" => matches!(data, Some(NodeData::ShadowRoot { .. })),
        // https://dom.spec.whatwg.org/#interface-documentfragment
        "Element" | "ARIAMixin" => matches!(data, Some(NodeData::Element(_))),
        "HTMLElement" | "ElementContentEditable" => {
            matches!(data, Some(NodeData::Element(element)) if element.name.ns == crate::dom::tree::html_namespace())
        }
        "SVGElement" => {
            matches!(data, Some(NodeData::Element(element)) if element.name.ns == crate::dom::tree::svg_namespace())
        }
        "MathMLElement" => {
            matches!(data, Some(NodeData::Element(element)) if element.name.ns == crate::dom::tree::mathml_namespace())
        }
        "CharacterData" => is_character_data,
        // https://dom.spec.whatwg.org/#interface-cdatasection
        "Text" => matches!(
            data,
            Some(NodeData::Text(_) | NodeData::CDataSection { .. })
        ),
        "CDATASection" => matches!(data, Some(NodeData::CDataSection { .. })),
        "Comment" => matches!(data, Some(NodeData::Comment { .. })),
        "DocumentType" => matches!(data, Some(NodeData::Doctype { .. })),
        "ProcessingInstruction" => matches!(data, Some(NodeData::ProcessingInstruction { .. })),
        "ElementCSSInlineStyle" => {
            matches!(
                data,
                Some(NodeData::Element(element))
                if element.name.ns == crate::dom::tree::html_namespace()
                    || element.name.ns == crate::dom::tree::svg_namespace()
                    || element.name.ns == crate::dom::tree::mathml_namespace()
            )
        }
        "ParentNode" => matches!(
            data,
            Some(
                NodeData::Document(_)
                    | NodeData::Element(_)
                    | NodeData::Fragment { .. }
                    | NodeData::ShadowRoot { .. }
            )
        ),
        // https://dom.spec.whatwg.org/#interface-childnode
        "ChildNode" => {
            is_character_data
                || matches!(data, Some(NodeData::Element(_) | NodeData::Doctype { .. }))
        }
        // https://dom.spec.whatwg.org/#interface-nondocumenttypechildnode
        "NonDocumentTypeChildNode" => {
            is_character_data || matches!(data, Some(NodeData::Element(_)))
        }
        "HTMLHyperlinkElementUtils" => {
            crate::dom::tree::is_html_tag(data, "a") || crate::dom::tree::is_html_tag(data, "area")
        }
        _ => {
            return interface
                .starts_with("HTML")
                .then(|| html_element_interface_matches(data, interface));
        }
    })
}

// https://html.spec.whatwg.org/multipage/dom.html#elements-in-the-dom
// https://html.spec.whatwg.org/multipage/dom.html#html-unknown-element
fn html_element_interface_matches(data: Option<&blitz_dom::NodeData>, interface: &str) -> bool {
    let element = match data {
        Some(blitz_dom::NodeData::Element(element))
            if element.name.ns == crate::dom::tree::html_namespace() =>
        {
            element
        }
        _ => return false,
    };
    let dom_interface = super::html_element_interface(element.name.local.as_str());
    dom_interface == interface
        || super::ELEMENT_INTERFACE_PARENTS
            .iter()
            .any(|(child, parent)| *parent == interface && *child == dom_interface)
}

pub(crate) fn require_node_interface(ctx: &Ctx<'_>, id: NodeId, interface: &str) -> Result<()> {
    // https://webidl.spec.whatwg.org/#es-attributes
    // https://webidl.spec.whatwg.org/#es-operations
    let owner = super::world_for_node(ctx, id)?;
    let owner = owner.borrow();
    let document = owner
        .document(id)
        .ok_or_else(|| Exception::throw_type(ctx, "stale node"))?;
    let data = document
        .document
        .base
        .get_node(id.node)
        .map(|node| &node.data);
    match node_interface_matches(
        data,
        document.xml_document,
        document.content_type == "text/html",
        interface,
    ) {
        Some(true) => Ok(()),
        Some(false) => Err(Exception::throw_type(ctx, "incompatible receiver")),
        None => Err(Exception::throw_type(ctx, "unknown node interface")),
    }
}

pub(crate) fn is_interface<'js>(ctx: &Ctx<'js>, value: &Value<'js>, interface: &str) -> bool {
    let Some(id) = super::host_node_id(ctx, value) else {
        return false;
    };
    let Ok(owner) = super::world_for_node(ctx, id) else {
        return false;
    };
    let owner = owner.borrow();
    let Some(document) = owner.document(id) else {
        return false;
    };
    let data = document
        .document
        .base
        .get_node(id.node)
        .map(|node| &node.data);
    node_interface_matches(
        data,
        document.xml_document,
        document.content_type == "text/html",
        interface,
    ) == Some(true)
}

#[inline(never)]
pub(crate) fn this_object<'js>(params: &Params<'_, 'js>) -> Result<Object<'js>> {
    params
        .this()
        .into_object()
        .ok_or_else(|| Exception::throw_internal(params.ctx(), "native receiver has no object"))
}

#[inline(never)]
pub(crate) fn put_forwards<'js>(
    params: &Params<'_, 'js>,
    name: &str,
    target: &str,
    nullable: bool,
) -> Result<Value<'js>> {
    // https://webidl.spec.whatwg.org/#es-attributes
    let ctx = params.ctx();
    let object = this_object(params)?;
    let forwarded: Value = object.get(name)?;
    // https://webidl.spec.whatwg.org/#PutForwards
    if nullable && (forwarded.is_null() || forwarded.is_undefined()) {
        return Ok(Value::new_undefined(ctx.clone()));
    }
    let forwarded = forwarded
        .into_object()
        .ok_or_else(|| Exception::throw_type(ctx, "forwarded attribute is not an object"))?;
    let value = params
        .arg(0)
        .unwrap_or_else(|| Value::new_undefined(ctx.clone()));
    let reflect_set = world(ctx)?
        .borrow()
        .pristine_reflect_set
        .clone()
        .ok_or_else(|| Exception::throw_internal(ctx, "Reflect.set was not captured"))?;
    let reflect_set = reflect_set.restore(ctx)?;
    let _: bool = reflect_set.call((forwarded, target, value))?;
    Ok(Value::new_undefined(ctx.clone()))
}

// https://webidl.spec.whatwg.org/#es-USVString
#[inline(never)]
pub(crate) fn usv_string_argument<'js>(
    params: &Params<'_, 'js>,
    index: usize,
) -> Result<rquickjs::String<'js>> {
    // https://webidl.spec.whatwg.org/#dfn-overload-resolution-algorithm
    let value = params
        .arg(index)
        .unwrap_or_else(|| Value::new_undefined(params.ctx().clone()));
    let coerced: Coerced<rquickjs::String> = FromJs::from_js(params.ctx(), value)?;
    let units = coerced.0.to_utf16()?;
    let lossy = String::from_utf16_lossy(&units);
    rquickjs::String::from_str(params.ctx().clone(), &lossy)
}

#[inline(never)]
pub(crate) fn string_argument<'js>(
    params: &Params<'_, 'js>,
    index: usize,
    default: Option<&str>,
) -> Result<rquickjs::String<'js>> {
    // https://webidl.spec.whatwg.org/#dfn-overload-resolution-algorithm
    let value = params
        .arg(index)
        .unwrap_or_else(|| Value::new_undefined(params.ctx().clone()));
    if value.is_undefined()
        && let Some(default) = default
    {
        return rquickjs::String::from_str(params.ctx().clone(), default);
    }
    // https://webidl.spec.whatwg.org/#es-DOMString
    Coerced::<rquickjs::String>::from_js(params.ctx(), value).map(|string| string.0)
}

// https://webidl.spec.whatwg.org/#es-boolean
#[inline(never)]
pub(crate) fn boolean_argument(params: &Params<'_, '_>, index: usize) -> Result<bool> {
    let ctx = params.ctx();
    let value = params
        .arg(index)
        .unwrap_or_else(|| Value::new_undefined(ctx.clone()));
    super::to_boolean(ctx, &value)
}

#[inline(never)]
pub(crate) fn callback_argument<'js>(ctx: &Ctx<'js>, value: &Value<'js>) -> Result<Function<'js>> {
    // https://webidl.spec.whatwg.org/#js-callback-function
    value
        .clone()
        .into_function()
        .ok_or_else(|| Exception::throw_type(ctx, "argument is not a function"))
}

// https://webidl.spec.whatwg.org/#LegacyNullToEmptyString
#[inline(never)]
pub(crate) fn legacy_null_string_argument<'js>(
    params: &Params<'_, 'js>,
    index: usize,
) -> Result<rquickjs::String<'js>> {
    if params.arg(index).is_some_and(|value| value.is_null()) {
        rquickjs::String::from_str(params.ctx().clone(), "")
    } else {
        string_argument(params, index, None)
    }
}

#[inline(never)]
pub(crate) fn document_type_argument<'js>(
    ctx: &Ctx<'js>,
    value: &Value<'js>,
) -> Result<Option<NodeReference>> {
    // https://webidl.spec.whatwg.org/#js-interface
    // https://webidl.spec.whatwg.org/#js-nullable-type
    if value.is_null() || value.is_undefined() {
        return Ok(None);
    }
    let id = super::required_node(ctx, value)?;
    let is_doctype = super::world_for_node(ctx, id).is_ok_and(|owner| {
        owner.borrow().document(id).is_some_and(|parsed| {
            parsed
                .document
                .base
                .get_node(id.node)
                .is_some_and(|node| matches!(node.data, blitz_dom::NodeData::Doctype { .. }))
        })
    });
    if is_doctype {
        Ok(Some(NodeReference::Tree(id)))
    } else {
        Err(Exception::throw_type(ctx, "argument is not a DocumentType"))
    }
}

#[inline(never)]
pub(crate) fn nullable_string_argument<'js>(
    params: &Params<'_, 'js>,
    index: usize,
) -> Result<Option<rquickjs::String<'js>>> {
    // https://webidl.spec.whatwg.org/#js-nullable-type
    let value = params
        .arg(index)
        .unwrap_or_else(|| Value::new_undefined(params.ctx().clone()));
    if value.is_null() || value.is_undefined() {
        return Ok(None);
    }
    Coerced::<rquickjs::String>::from_js(params.ctx(), value).map(|string| Some(string.0))
}

// https://webidl.spec.whatwg.org/#es-dictionary
pub(crate) fn dict_flag<'js>(
    ctx: &Ctx<'js>,
    object: &Object<'js>,
    key: &str,
) -> Result<Option<bool>> {
    let value: Value = object.get(key)?;
    if value.is_undefined() {
        return Ok(None);
    }
    Coerced::<bool>::from_js(ctx, value).map(|flag| Some(flag.0))
}

// https://webidl.spec.whatwg.org/#es-promise
#[inline(never)]
pub(crate) fn resolved_promise<'js>(ctx: &Ctx<'js>) -> Result<Value<'js>> {
    let (promise, resolve, _) = ctx.promise()?;
    resolve.call::<_, ()>((Value::new_undefined(ctx.clone()),))?;
    Ok(promise.into_value())
}

// https://webidl.spec.whatwg.org/#es-interface
pub(crate) fn dict_required_object<'js>(
    ctx: &Ctx<'js>,
    object: &Object<'js>,
    key: &str,
) -> Result<Persistent<Object<'static>>> {
    let value: Value = object.get(key)?;
    let Some(object) = value.as_object() else {
        return Err(Exception::throw_type(
            ctx,
            "interface dictionary member must be an object",
        ));
    };
    Ok(Persistent::save(ctx, object.clone()))
}

// https://webidl.spec.whatwg.org/#es-dictionary
// https://webidl.spec.whatwg.org/#es-interface
pub(crate) fn dict_object<'js>(
    ctx: &Ctx<'js>,
    object: &Object<'js>,
    key: &str,
) -> Result<Option<Persistent<Object<'static>>>> {
    let value: Value = object.get(key)?;
    // https://webidl.spec.whatwg.org/#es-interface
    if value.is_undefined() {
        return Ok(None);
    }
    let Some(object) = value.as_object() else {
        return Err(Exception::throw_type(
            ctx,
            "interface dictionary member must be an object",
        ));
    };
    Ok(Some(Persistent::save(ctx, object.clone())))
}

// https://webidl.spec.whatwg.org/#js-sequence
pub(crate) fn dict_string_sequence<'js>(
    ctx: &Ctx<'js>,
    object: &Object<'js>,
    key: &str,
) -> Result<Option<Vec<Vec<u16>>>> {
    let value: Value = object.get(key)?;
    if value.is_undefined() {
        return Ok(None);
    }
    let iterable = value
        .into_object()
        .ok_or_else(|| Exception::throw_type(ctx, "sequence must be an iterable object"))?;
    let method: Value = iterable.get(PredefinedAtom::SymbolIterator)?;
    let method = method
        .into_function()
        .ok_or_else(|| Exception::throw_type(ctx, "sequence iterator is not callable"))?;
    let iterator: Value = method.call((rquickjs::prelude::This(iterable),))?;
    let iterator = iterator
        .into_object()
        .ok_or_else(|| Exception::throw_type(ctx, "sequence iterator must return an object"))?;
    let next: Value = iterator.get("next")?;
    let next = next
        .into_function()
        .ok_or_else(|| Exception::throw_type(ctx, "sequence iterator next is not callable"))?;
    let mut items = Vec::new();
    loop {
        let result: Value = next.call((rquickjs::prelude::This(iterator.clone()),))?;
        let result = result.into_object().ok_or_else(|| {
            Exception::throw_type(ctx, "sequence iterator result must be an object")
        })?;
        let done: Coerced<bool> = result.get("done")?;
        if done.0 {
            break;
        }
        let value: Value = result.get("value")?;
        let string = Coerced::<rquickjs::String>::from_js(ctx, value)?.0;
        items.push(string.to_utf16()?);
    }
    Ok(Some(items))
}

// https://webidl.spec.whatwg.org/#es-sequence
#[inline(never)]
pub(crate) fn sequence<'js>(ctx: &Ctx<'js>, values: Vec<Value<'js>>) -> Result<Value<'js>> {
    let array = rquickjs::Array::new(ctx.clone())?;
    for (index, value) in values.into_iter().enumerate() {
        array.as_object().prop(
            index.to_string(),
            Property::from(value).writable().enumerable().configurable(),
        )?;
    }
    Ok(array.into_value())
}

// https://html.spec.whatwg.org/multipage/webappapis.html#report-the-error
pub(crate) fn report_callback_error(ctx: &Ctx<'_>, error: &rquickjs::Error) {
    if error.is_exception() {
        let caught = ctx.catch();
        super::report_exception_value(ctx, caught, 0, "");
    }
}

#[inline(never)]
pub(crate) fn ret<'js, T: rquickjs::IntoJs<'js>>(result: T, ctx: &Ctx<'js>) -> Result<Value<'js>> {
    rquickjs::IntoJs::into_js(result, ctx)
}

#[inline(never)]
pub(crate) fn ret_nullable<'js, T: rquickjs::IntoJs<'js>>(
    result: Option<T>,
    ctx: &Ctx<'js>,
) -> Result<Value<'js>> {
    // https://webidl.spec.whatwg.org/#es-nullable-type
    match result {
        Some(result) => rquickjs::IntoJs::into_js(result, ctx),
        None => Ok(Value::new_null(ctx.clone())),
    }
}

// https://webidl.spec.whatwg.org/#idl-USVString
#[inline(never)]
pub(crate) fn ret_usv<'js>(
    result: &crate::dom_string::DomString,
    ctx: &Ctx<'js>,
) -> Result<Value<'js>> {
    let lossy = result.to_string_lossy();
    rquickjs::IntoJs::into_js(lossy.as_ref(), ctx)
}

#[inline(never)]
pub(crate) fn ret_enum<'js>(result: impl AsRef<str>, ctx: &Ctx<'js>) -> Result<Value<'js>> {
    let result = rquickjs::String::from_str(ctx.clone(), result.as_ref())?;
    rquickjs::IntoJs::into_js(result, ctx)
}

#[inline(never)]
pub(crate) fn ret_undefined<'js>(ctx: &Ctx<'js>) -> Value<'js> {
    Value::new_undefined(ctx.clone())
}

#[inline(never)]
pub(crate) fn ret_string_sequence<'js>(result: Vec<String>, ctx: &Ctx<'js>) -> Result<Value<'js>> {
    let values = result
        .into_iter()
        .map(|item| {
            rquickjs::String::from_str(ctx.clone(), &item).map(rquickjs::String::into_value)
        })
        .collect::<Result<Vec<Value>>>()?;
    sequence(ctx, values)
}

#[inline(never)]
pub(crate) fn fetch<'js>(params: &Params<'_, 'js>, index: usize) -> Value<'js> {
    // https://webidl.spec.whatwg.org/#dfn-overload-resolution-algorithm
    params
        .arg(index)
        .unwrap_or_else(|| Value::new_undefined(params.ctx().clone()))
}

// https://webidl.spec.whatwg.org/#es-long
#[inline(never)]
pub(crate) fn long_argument(params: &Params<'_, '_>, index: usize) -> Result<i32> {
    let value = fetch(params, index);
    let converted: Coerced<i32> = Coerced::from_js(params.ctx(), value)?;
    Ok(converted.0)
}

// https://webidl.spec.whatwg.org/#es-unsigned-long
#[inline(never)]
pub(crate) fn ulong_argument(params: &Params<'_, '_>, index: usize) -> Result<u32> {
    let value = fetch(params, index);
    let converted: Coerced<i32> = Coerced::from_js(params.ctx(), value)?;
    Ok(converted.0.cast_unsigned())
}

// https://webidl.spec.whatwg.org/#es-unsigned-short
#[inline(never)]
pub(crate) fn ushort_argument(params: &Params<'_, '_>, index: usize) -> Result<u16> {
    let value = fetch(params, index);
    let converted: Coerced<i32> = Coerced::from_js(params.ctx(), value)?;
    Ok(u16::try_from(converted.0.rem_euclid(65536)).unwrap_or(u16::MAX))
}

// https://webidl.spec.whatwg.org/#es-double
#[inline(never)]
pub(crate) fn double_argument(params: &Params<'_, '_>, index: usize) -> Result<f64> {
    let value = fetch(params, index);
    let converted: Coerced<f64> = Coerced::from_js(params.ctx(), value)?;
    Ok(converted.0)
}

#[inline(never)]
pub(crate) fn restricted_double_argument(params: &Params<'_, '_>, index: usize) -> Result<f64> {
    let value = fetch(params, index);
    let converted: Coerced<f64> = Coerced::from_js(params.ctx(), value)?;
    if !converted.0.is_finite() {
        return Err(rquickjs::Exception::throw_type(
            params.ctx(),
            "finite double required",
        ));
    }
    Ok(converted.0)
}

#[inline(never)]
pub(crate) fn optional_string_argument<'js>(
    params: &Params<'_, 'js>,
    index: usize,
) -> Result<Option<rquickjs::String<'js>>> {
    if fetch(params, index).is_undefined() {
        return Ok(None);
    }
    // https://webidl.spec.whatwg.org/#es-DOMString
    string_argument(params, index, None).map(Some)
}

#[inline(never)]
pub(crate) fn nullable_ulong_argument(
    params: &Params<'_, '_>,
    index: usize,
) -> Result<Option<u32>> {
    let value = fetch(params, index);
    if value.is_null() || value.is_undefined() {
        return Ok(None);
    }
    // https://webidl.spec.whatwg.org/#es-unsigned-long
    ulong_argument(params, index).map(Some)
}

#[inline(never)]
pub(crate) fn optional_boolean_argument(
    params: &Params<'_, '_>,
    index: usize,
) -> Result<Option<bool>> {
    if fetch(params, index).is_undefined() {
        return Ok(None);
    }
    // https://webidl.spec.whatwg.org/#es-boolean
    boolean_argument(params, index).map(Some)
}

// https://webidl.spec.whatwg.org/#es-boolean
#[inline(never)]
pub(crate) fn boolean_argument_with_default(
    params: &Params<'_, '_>,
    index: usize,
    default: bool,
) -> Result<bool> {
    let value = fetch(params, index);
    if value.is_undefined() {
        return Ok(default);
    }
    super::to_boolean(params.ctx(), &value)
}
