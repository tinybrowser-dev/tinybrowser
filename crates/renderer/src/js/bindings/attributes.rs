//! Attribute, class, and handler-attribute objects and plumbing.

use super::{
    FromJs, OptString, WebIdlString, child_value, deref_weak, make_weak, realm_registry,
    schedule_mutation_delivery, string_value, throw_dom, with_node_data, world, world_for_node,
    wrap_node,
};
use enumerated::Enumerated;

use std::cell::RefCell;
use std::collections::HashSet;

use std::rc::Rc;

use blitz_dom::Dirtiness;
use markup5ever::{LocalName, QualName};

use crate::documents::JournalEntry;
use crate::dom::tree::{NodeId, qualified_name};
use crate::js::world::{
    AttrAttachError, AttrState, EventTargetKey, FrameNavigation, Handle, NavigationTarget, World,
};
use crate::names::qualified_name_eq;

use rquickjs::{Class, Ctx, Exception, Object, Persistent, Result, Value, class::Trace};

use super::node::core::node_generated;
use crate::js::events::{
    JsEvent, add_listener_parsed, dispatch_event, event_target_generated, listener_callback,
    listener_options, remove_capture, remove_listener_parsed, report_exception,
};

/// A `DOMTokenList` bound to one attribute of an element: `Element.classList`
/// and `HTMLIFrameElement.sandbox` are the same interface over different
/// attributes
/// (<https://dom.spec.whatwg.org/#interface-domtokenlist>,
/// <https://html.spec.whatwg.org/multipage/iframe-embed-object.html#dom-iframe-sandbox>).
#[derive(Trace, rquickjs::JsLifetime)]
pub struct JsTokenList {
    pub(crate) element: Handle,
    /// The content attribute whose tokens this list holds. The update steps
    /// write the attribute, so the list must know which one it came from
    /// (<https://dom.spec.whatwg.org/#concept-dtl-update>).
    #[qjs(skip_trace)]
    pub(crate) attribute: LocalName,
}

include!(concat!(env!("OUT_DIR"), "/DOMTokenList.rs"));

impl JsTokenList {
    /// The element this list belongs to. Adoption keeps the list object, so
    /// every operation follows the adopted copy
    /// (<https://dom.spec.whatwg.org/#concept-node-adopt>).
    fn live_element(&self, ctx: &Ctx<'_>) -> Result<NodeId> {
        Ok(world(ctx)?
            .borrow_mut()
            .registry()
            .borrow_mut()
            .live_node_id(self.element.0))
    }
}

impl<'js> dom_token_list_generated::DOMTokenList<'js> for JsTokenList {
    // https://dom.spec.whatwg.org/#dom-domtokenlist-length
    fn get_length(&self, ctx: &Ctx<'js>) -> Result<usize> {
        Ok(token_list_tokens(ctx, self.live_element(ctx)?, &self.attribute)?.len())
    }

    // https://dom.spec.whatwg.org/#dom-domtokenlist-value
    fn get_value(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        let element = self.live_element(ctx)?;
        let world = world(ctx)?;
        let value = world
            .borrow()
            .document(element)
            .and_then(|parsed| {
                crate::dom::tree::attr(&parsed.document.base, element.node, self.attribute.as_ref())
                    .map(ToOwned::to_owned)
            })
            .unwrap_or_default();
        rquickjs::String::from_str(ctx.clone(), &value)
    }

    fn set_value(&self, ctx: &Ctx<'js>, value: rquickjs::String<'js>) -> Result<()> {
        write_tokens(
            ctx,
            self.live_element(ctx)?,
            &self.attribute,
            &value.to_string()?,
        )
    }

    // https://dom.spec.whatwg.org/#dom-domtokenlist-item
    fn item(&self, ctx: Ctx<'js>, index: u32) -> Result<Option<rquickjs::String<'js>>> {
        let tokens = token_list_tokens(&ctx, self.live_element(&ctx)?, &self.attribute)?;
        match usize::try_from(index)
            .ok()
            .and_then(|index| tokens.get(index))
        {
            Some(token) => rquickjs::String::from_str(ctx, token).map(Some),
            None => Ok(None),
        }
    }

    // https://dom.spec.whatwg.org/#dom-domtokenlist-contains
    fn contains(&self, ctx: Ctx<'js>, token: rquickjs::String<'js>) -> Result<bool> {
        // `contains` does not validate its argument
        // (<https://dom.spec.whatwg.org/#dom-domtokenlist-contains>).
        let token = token.to_string()?;
        Ok(token_list_tokens(&ctx, self.live_element(&ctx)?, &self.attribute)?.contains(&token))
    }

    // https://dom.spec.whatwg.org/#dom-domtokenlist-add
    fn add(&self, ctx: Ctx<'js>, tokens: Vec<rquickjs::String<'js>>) -> Result<()> {
        let tokens = tokens
            .iter()
            .map(rquickjs::String::to_string)
            .collect::<Result<Vec<_>>>()?;
        for token in &tokens {
            validate_token(&ctx, token)?;
        }
        let mut current = token_list_tokens(&ctx, self.live_element(&ctx)?, &self.attribute)?;
        for token in tokens {
            if !current.contains(&token) {
                current.push(token);
            }
        }
        // The update steps run even when no token was added, normalizing
        // the attribute (<https://dom.spec.whatwg.org/#dom-domtokenlist-add>).
        write_token_list(&ctx, self.live_element(&ctx)?, &self.attribute, &current)
    }

    // https://dom.spec.whatwg.org/#dom-domtokenlist-remove
    fn remove(&self, ctx: Ctx<'js>, tokens: Vec<rquickjs::String<'js>>) -> Result<()> {
        let tokens = tokens
            .iter()
            .map(rquickjs::String::to_string)
            .collect::<Result<Vec<_>>>()?;
        for token in &tokens {
            validate_token(&ctx, token)?;
        }
        let mut current = token_list_tokens(&ctx, self.live_element(&ctx)?, &self.attribute)?;
        for token in tokens {
            current.retain(|existing| existing != &token);
        }
        write_token_list(&ctx, self.live_element(&ctx)?, &self.attribute, &current)
    }

    // https://dom.spec.whatwg.org/#dom-domtokenlist-toggle
    fn toggle(
        &self,
        ctx: Ctx<'js>,
        token: rquickjs::String<'js>,
        force: Option<bool>,
    ) -> Result<bool> {
        let token = token.to_string()?;
        validate_token(&ctx, &token)?;
        let mut current = token_list_tokens(&ctx, self.live_element(&ctx)?, &self.attribute)?;
        let present = current.contains(&token);
        let should_be_present = force.unwrap_or(!present);
        if should_be_present != present {
            if should_be_present {
                current.push(token);
            } else {
                current.retain(|existing| existing != &token);
            }
            write_token_list(&ctx, self.live_element(&ctx)?, &self.attribute, &current)?;
        }
        Ok(should_be_present)
    }

    // https://dom.spec.whatwg.org/#dom-domtokenlist-replace
    fn replace(
        &self,
        ctx: Ctx<'js>,
        old: rquickjs::String<'js>,
        new: rquickjs::String<'js>,
    ) -> Result<bool> {
        let old = old.to_string()?;
        let new = new.to_string()?;
        validate_token_pair(&ctx, &old, &new)?;
        let mut current = token_list_tokens(&ctx, self.live_element(&ctx)?, &self.attribute)?;
        if !current.contains(&old) {
            return Ok(false);
        }
        for token in &mut current {
            if token == &old {
                token.clone_from(&new);
            }
        }
        // Replacing can duplicate an existing token; the token set keeps the
        // first occurrence (https://github.com/whatwg/dom/issues/443).
        let mut deduped: Vec<String> = Vec::with_capacity(current.len());
        for token in current {
            if !deduped.contains(&token) {
                deduped.push(token);
            }
        }
        write_token_list(&ctx, self.live_element(&ctx)?, &self.attribute, &deduped)?;
        Ok(true)
    }

    // https://dom.spec.whatwg.org/#dom-domtokenlist-supports
    fn supports(&self, ctx: Ctx<'js>, token: rquickjs::String<'js>) -> Result<bool> {
        // `iframe.sandbox` is the one token list with supported tokens
        // (<https://html.spec.whatwg.org/multipage/iframe-embed-object.html#attr-iframe-sandbox>).
        let attribute: &str = self.attribute.as_ref();
        if attribute != "sandbox" {
            // Validation steps step 1: no supported tokens means a `TypeError`
            // (<https://dom.spec.whatwg.org/#concept-domtokenlist-validation>).
            return Err(Exception::throw_type(
                &ctx,
                "DOMTokenList has no supported tokens",
            ));
        }
        let token = token.to_string()?;
        Ok(crate::protocol::SandboxFlags::supports(&token))
    }
}

/// The element's tokens for `attribute`: an ordered set, so duplicates
/// collapse.
fn token_list_tokens(ctx: &Ctx<'_>, id: NodeId, attribute: &LocalName) -> Result<Vec<String>> {
    let world = world(ctx)?;
    let mut tokens: Vec<String> = Vec::new();
    for token in world
        .borrow()
        .document(id)
        .and_then(|parsed| {
            crate::dom::tree::attr(&parsed.document.base, id.node, attribute.as_ref())
                .map(ToOwned::to_owned)
        })
        .unwrap_or_default()
        .split_ascii_whitespace()
    {
        if !tokens.iter().any(|existing| existing == token) {
            tokens.push(token.to_string());
        }
    }
    Ok(tokens)
}

/// [Token validation](https://dom.spec.whatwg.org/#validate-a-token).
fn validate_token(ctx: &Ctx<'_>, token: &str) -> Result<()> {
    if token.is_empty() {
        return Err(throw_dom(ctx, "SyntaxError", "token is empty"));
    }
    if token
        .chars()
        .any(|c| matches!(c, '\t' | '\n' | '\u{c}' | '\r' | ' '))
    {
        return Err(throw_dom(
            ctx,
            "InvalidCharacterError",
            "token contains ASCII whitespace",
        ));
    }
    Ok(())
}

/// `replace` validates emptiness for both tokens before whitespace
/// (<https://dom.spec.whatwg.org/#dom-domtokenlist-replace>).
fn validate_token_pair(ctx: &Ctx<'_>, old: &str, new: &str) -> Result<()> {
    if old.is_empty() || new.is_empty() {
        return Err(throw_dom(ctx, "SyntaxError", "token is empty"));
    }
    if old
        .chars()
        .chain(new.chars())
        .any(|c| matches!(c, '\t' | '\n' | '\u{c}' | '\r' | ' '))
    {
        return Err(throw_dom(
            ctx,
            "InvalidCharacterError",
            "token contains ASCII whitespace",
        ));
    }
    Ok(())
}

/// Writes the token list back as `attribute`. The update steps set the
/// attribute even when the list serializes to the empty string; when the
/// attribute does not exist and the value is empty they do nothing
/// (<https://dom.spec.whatwg.org/#concept-dtl-update>).
fn write_token_list(
    ctx: &Ctx<'_>,
    id: NodeId,
    attribute: &LocalName,
    tokens: &[String],
) -> Result<()> {
    if tokens.is_empty()
        && world(ctx)?.borrow().document(id).is_none_or(|parsed| {
            crate::dom::tree::attr(&parsed.document.base, id.node, attribute.as_ref()).is_none()
        })
    {
        return Ok(());
    }
    write_tokens(ctx, id, attribute, &tokens.join(" "))
}

fn write_tokens(ctx: &Ctx<'_>, id: NodeId, attribute: &LocalName, value: &str) -> Result<()> {
    set_attribute_sync(ctx, id, attribute.as_ref(), value)
}

/// One `Attr` platform object
/// (<https://dom.spec.whatwg.org/#interface-attr>). Identity lives in the
/// agent's Attr registry so `el.attributes[0] === el.getAttributeNode(name)`.
#[derive(Trace, rquickjs::JsLifetime)]
pub struct JsAttr<'js> {
    pub(crate) id: u64,
    pub(crate) scope: Handle,
    child_nodes: AttrChildren<'js>,
}

#[derive(rquickjs::JsLifetime)]
struct AttrChildren<'js>(RefCell<Option<Value<'js>>>);

impl<'js> Trace<'js> for AttrChildren<'js> {
    fn trace<'a>(&self, tracer: rquickjs::class::Tracer<'a, 'js>) {
        // No JavaScript runs while the cache is borrowed or replaced.
        self.0.borrow().trace(tracer);
    }
}

include!(concat!(env!("OUT_DIR"), "/Attr.rs"));

pub(super) type AttrArgument<'js> = Class<'js, JsAttr<'js>>;

/// `Attr` is a `Node` and therefore an `EventTarget`
/// (<https://dom.spec.whatwg.org/#interface-attr>), so it is a third payload
/// of the one `EventTarget` contract. Listeners key on the Attr registry
/// identity, which never changes even when the attribute moves documents.
impl<'js> event_target_generated::EventTarget<'js> for JsAttr<'js> {
    fn constructor(ctx: &Ctx<'js>) -> Result<Self> {
        Err(rquickjs::Exception::throw_type(ctx, "Illegal constructor"))
    }

    // https://dom.spec.whatwg.org/#dom-eventtarget-addeventlistener
    fn add_event_listener(
        &self,
        ctx: Ctx<'js>,
        _this: Object<'js>,
        typ: rquickjs::String<'js>,
        callback: Value<'js>,
        options: event_target_generated::AddEventListenerOptionsOrBoolean,
    ) -> Result<()> {
        let state = attr_state(&ctx, self.scope.0, self.id)?;
        let home = attr_context(&ctx, self.scope.0, self.id)?;
        let callback = listener_callback(&home, callback)?;
        let options = listener_options(options);
        add_listener_parsed(
            &home,
            EventTargetKey::Attribute {
                scope: state.scope,
                id: self.id,
            },
            typ.to_string()?,
            callback,
            options,
        )
    }

    // https://dom.spec.whatwg.org/#dom-eventtarget-removeeventlistener
    fn remove_event_listener(
        &self,
        ctx: Ctx<'js>,
        _this: Object<'js>,
        typ: rquickjs::String<'js>,
        callback: Value<'js>,
        options: event_target_generated::BooleanOrEventListenerOptions,
    ) -> Result<()> {
        let state = attr_state(&ctx, self.scope.0, self.id)?;
        let home = attr_context(&ctx, self.scope.0, self.id)?;
        let callback = listener_callback(&home, callback)?;
        let capture = remove_capture(options);
        remove_listener_parsed(
            &home,
            EventTargetKey::Attribute {
                scope: state.scope,
                id: self.id,
            },
            &typ.to_string()?,
            callback.as_ref(),
            capture,
        )
    }

    // https://dom.spec.whatwg.org/#dom-eventtarget-dispatchevent
    fn dispatch_event(&self, ctx: Ctx<'js>, _this: Object<'js>, event: Value<'js>) -> Result<bool> {
        let state = attr_state(&ctx, self.scope.0, self.id)?;
        let home = attr_context(&ctx, self.scope.0, self.id)?;
        let event = Class::<JsEvent>::from_js(&home, event)?;
        dispatch_event(
            &home,
            EventTargetKey::Attribute {
                scope: state.scope,
                id: self.id,
            },
            &event,
        )
    }
}

impl super::host::SharedClass for JsAttr<'_> {
    // https://dom.spec.whatwg.org/#interface-attr
    // `Attr` implements exactly `Attr`, `Node`, and `EventTarget`; anything
    // else reaching an alternate arm is an incompatible receiver.
    fn require_interface(&self, ctx: &Ctx<'_>, interface: &str) -> Result<()> {
        if matches!(interface, "Attr" | "Node" | "EventTarget") {
            Ok(())
        } else {
            Err(Exception::throw_type(ctx, "incompatible receiver"))
        }
    }
}

impl<'object> JsAttr<'object> {
    // https://dom.spec.whatwg.org/#dom-node-nodevalue
    pub(super) fn node_value<'js>(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        rquickjs::String::from_str(ctx.clone(), &attr_value(ctx, self.scope.0, self.id)?).map(Some)
    }

    pub(super) fn set_node_value<'js>(
        &self,
        ctx: &Ctx<'js>,
        value: Option<rquickjs::String<'js>>,
    ) -> Result<()> {
        // `Attr.nodeValue` follows the node rule: null acts as the empty
        // string (<https://dom.spec.whatwg.org/#dom-node-nodevalue>).
        set_attr_value(
            ctx,
            self.scope.0,
            self.id,
            value
                .map(|value| value.to_string())
                .transpose()?
                .unwrap_or_default(),
        )
    }

    // https://dom.spec.whatwg.org/#dom-node-textcontent
    pub(super) fn text_content<'js>(
        &self,
        ctx: &Ctx<'js>,
    ) -> Result<Option<rquickjs::String<'js>>> {
        self.node_value(ctx)
    }

    pub(super) fn set_text_content<'js>(
        &self,
        ctx: &Ctx<'js>,
        value: Option<rquickjs::String<'js>>,
    ) -> Result<()> {
        self.set_node_value(ctx, value)
    }

    pub(super) fn node_name(&self, ctx: &Ctx<'_>) -> Result<String> {
        Ok(attr_state(ctx, self.scope.0, self.id)?.qualified)
    }

    pub(super) fn owner_document<'js>(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        // https://dom.spec.whatwg.org/#concept-node-document
        let home = attr_context(ctx, self.scope.0, self.id)?;
        wrap_node(&home, attr_state(&home, self.scope.0, self.id)?.document)
    }

    // https://dom.spec.whatwg.org/#dom-node-baseuri
    pub(super) fn base_uri(&self, ctx: &Ctx<'_>) -> Result<crate::dom_string::DomString> {
        let home = attr_context(ctx, self.scope.0, self.id)?;
        let document = attr_state(&home, self.scope.0, self.id)?.document;
        Ok(super::document_base_url_string(&home, document).into())
    }

    // https://dom.spec.whatwg.org/#dom-node-childnodes
    pub(super) fn child_nodes(&self, ctx: &Ctx<'object>) -> Result<Value<'object>> {
        if let Some(saved) = self.child_nodes.0.borrow().clone() {
            return Ok(saved);
        }
        let home = attr_context(ctx, self.scope.0, self.id)?;
        let list = super::live_collection(
            &home,
            self.scope.0,
            super::CollectionKind::Static(Vec::new()),
            None,
        )?;
        *self.child_nodes.0.borrow_mut() = Some(list.clone());
        Ok(list)
    }

    // https://dom.spec.whatwg.org/#dom-node-getrootnode
    pub(super) fn get_root_node<'js>(
        &self,
        ctx: Ctx<'js>,
        _options: super::node::core::node_generated::GetRootNodeOptions,
    ) -> Result<Value<'js>> {
        attr_wrapper(&ctx, self.id)
    }

    // https://dom.spec.whatwg.org/#dom-node-clonenode
    pub(super) fn clone_node<'js>(&self, ctx: Ctx<'js>, _deep: bool) -> Result<Value<'js>> {
        let state = attr_state(&ctx, self.scope.0, self.id)?;
        let clone = new_detached_attr(
            &ctx,
            state.document,
            state.namespace,
            state.prefix,
            state.local,
            state.qualified,
        )?;
        set_attr_value(
            &ctx,
            state.document,
            clone,
            attr_value(&ctx, self.scope.0, self.id)?,
        )?;
        attr_wrapper(&ctx, clone)
    }

    // https://dom.spec.whatwg.org/#dom-node-issamenode
    pub(super) fn is_same_node(&self, other: Option<super::host::NodeReference>) -> bool {
        other
            == Some(super::host::NodeReference::Attribute {
                scope: self.scope.0,
                id: self.id,
            })
    }

    // https://dom.spec.whatwg.org/#concept-node-equals
    pub(super) fn is_equal_node(
        &self,
        ctx: Ctx<'_>,
        other: Option<super::host::NodeReference>,
    ) -> Result<bool> {
        let Some(super::host::NodeReference::Attribute { scope, id }) = other else {
            return Ok(false);
        };
        let a = attr_state(&ctx, self.scope.0, self.id)?;
        let b = attr_state(&ctx, scope, id)?;
        Ok(a.namespace == b.namespace
            && a.local == b.local
            && attr_value(&ctx, self.scope.0, self.id)? == attr_value(&ctx, scope, id)?)
    }

    // https://dom.spec.whatwg.org/#dom-node-contains
    pub(super) fn contains(&self, other: Option<super::host::NodeReference>) -> bool {
        self.is_same_node(other)
    }

    pub(super) fn compare_document_position(
        &self,
        ctx: Ctx<'_>,
        other: super::host::NodeReference,
    ) -> Result<u16> {
        super::compare_node_position(
            &ctx,
            super::host::NodeReference::Attribute {
                scope: self.scope.0,
                id: self.id,
            },
            other,
        )
    }

    // https://dom.spec.whatwg.org/#concept-node-ensure-pre-insert-validity
    #[expect(
        clippy::unused_self,
        reason = "the Node dispatch shape requires the method; the operation ignores the wrapper"
    )]
    pub(super) fn append_child<'js>(
        &self,
        ctx: Ctx<'js>,
        _node: super::host::NodeReference,
    ) -> Result<Value<'js>> {
        Err(throw_dom(
            &ctx,
            "HierarchyRequestError",
            "attributes cannot have children",
        ))
    }
    pub(super) fn insert_before<'js>(
        &self,
        ctx: Ctx<'js>,
        node: super::host::NodeReference,
        _child: Option<super::host::NodeReference>,
    ) -> Result<Value<'js>> {
        self.append_child(ctx, node)
    }
    // https://dom.spec.whatwg.org/#concept-node-pre-remove
    #[expect(
        clippy::unused_self,
        reason = "the Node dispatch shape requires the method; the operation ignores the wrapper"
    )]
    pub(super) fn remove_child<'js>(
        &self,
        ctx: Ctx<'js>,
        _child: super::host::NodeReference,
    ) -> Result<Value<'js>> {
        Err(throw_dom(
            &ctx,
            "NotFoundError",
            "attributes have no children",
        ))
    }
    // https://dom.spec.whatwg.org/#concept-node-replace
    pub(super) fn replace_child<'js>(
        &self,
        ctx: Ctx<'js>,
        node: super::host::NodeReference,
        _child: super::host::NodeReference,
    ) -> Result<Value<'js>> {
        self.append_child(ctx, node)
    }

    // https://dom.spec.whatwg.org/#locate-a-namespace
    pub(super) fn lookup_namespace_uri<'js>(
        &self,
        ctx: Ctx<'js>,
        prefix: Option<rquickjs::String<'js>>,
    ) -> Result<Value<'js>> {
        let prefix = prefix
            .map(|value| value.to_string())
            .transpose()?
            .filter(|value| !value.is_empty());
        let Some(owner) = attr_owner(&ctx, self.scope.0, self.id) else {
            return Ok(Value::new_null(ctx));
        };
        let world = world_for_node(&ctx, owner)?;
        let world = world.borrow();
        let found = world.document(owner).and_then(|parsed| {
            crate::dom::namespaces::locate_namespace(&parsed.document, owner, prefix.as_deref())
        });
        match found {
            Some(namespace) => string_value(&ctx, &namespace),
            None => Ok(Value::new_null(ctx)),
        }
    }
    // https://dom.spec.whatwg.org/#dom-node-lookupprefix
    pub(super) fn lookup_prefix<'js>(
        &self,
        ctx: Ctx<'js>,
        namespace: Option<rquickjs::String<'js>>,
    ) -> Result<Value<'js>> {
        let namespace = namespace
            .map(|value| value.to_string())
            .transpose()?
            .filter(|value| !value.is_empty());
        let (Some(owner), Some(namespace)) = (attr_owner(&ctx, self.scope.0, self.id), namespace)
        else {
            return Ok(Value::new_null(ctx));
        };
        let world = world_for_node(&ctx, owner)?;
        let world = world.borrow();
        let found = world.document(owner).and_then(|parsed| {
            crate::dom::namespaces::locate_prefix(&parsed.document, owner, &namespace)
        });
        match found {
            Some(prefix) => string_value(&ctx, &prefix),
            None => Ok(Value::new_null(ctx)),
        }
    }
    // https://dom.spec.whatwg.org/#dom-node-isdefaultnamespace
    pub(super) fn is_default_namespace<'js>(
        &self,
        ctx: Ctx<'js>,
        namespace: Option<rquickjs::String<'js>>,
    ) -> Result<bool> {
        let namespace = namespace
            .map(|value| value.to_string())
            .transpose()?
            .filter(|value| !value.is_empty());
        let found = self.lookup_namespace_uri(ctx.clone(), None)?;
        let found = if found.is_null() {
            None
        } else {
            Some(rquickjs::String::from_js(&ctx, found)?.to_string()?)
        };
        Ok(found == namespace)
    }
}

/// `Attr` is a `Node`
/// (<https://dom.spec.whatwg.org/#interface-attr>), so it is the second
/// payload of the one `Node` contract. The prototype owner stays `JsNode`
/// by the class-name election; this arm only serves `Attr` receivers that
/// reach `Node.prototype` through the prototype chain.
impl<'js> node_generated::Node<'js> for JsAttr<'js> {
    // https://dom.spec.whatwg.org/#dom-node-nodetype
    fn get_node_type(&self, _ctx: &Ctx<'js>) -> Result<u16> {
        Ok(2)
    }

    // https://dom.spec.whatwg.org/#dom-node-nodename
    fn get_node_name(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        let name = self.node_name(ctx)?;
        rquickjs::String::from_str(ctx.clone(), &name)
    }

    // https://dom.spec.whatwg.org/#dom-node-baseuri
    fn get_base_uri(&self, ctx: &Ctx<'js>) -> Result<crate::dom_string::DomString> {
        self.base_uri(ctx)
    }

    // https://dom.spec.whatwg.org/#dom-node-isconnected
    fn get_is_connected(&self, _ctx: &Ctx<'js>) -> Result<bool> {
        Ok(false)
    }

    // https://dom.spec.whatwg.org/#dom-node-ownerdocument
    fn get_owner_document(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        self.owner_document(ctx)
    }

    // https://dom.spec.whatwg.org/#dom-node-getrootnode
    fn get_root_node(
        &self,
        ctx: Ctx<'js>,
        arg_0: node_generated::GetRootNodeOptions,
    ) -> Result<Value<'js>> {
        self.get_root_node(ctx, arg_0)
    }

    // https://dom.spec.whatwg.org/#dom-node-parentnode
    fn get_parent_node(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        Ok(Value::new_null(ctx.clone()))
    }

    // https://dom.spec.whatwg.org/#dom-node-parentelement
    fn get_parent_element(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        Ok(Value::new_null(ctx.clone()))
    }

    // https://dom.spec.whatwg.org/#dom-node-haschildnodes
    fn has_child_nodes(&self, _ctx: Ctx<'js>) -> Result<bool> {
        Ok(false)
    }

    // https://dom.spec.whatwg.org/#dom-node-childnodes
    fn get_child_nodes(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        self.child_nodes(ctx)
    }

    // https://dom.spec.whatwg.org/#dom-node-firstchild
    fn get_first_child(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        Ok(Value::new_null(ctx.clone()))
    }

    // https://dom.spec.whatwg.org/#dom-node-lastchild
    fn get_last_child(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        Ok(Value::new_null(ctx.clone()))
    }

    // https://dom.spec.whatwg.org/#dom-node-previoussibling
    fn get_previous_sibling(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        Ok(Value::new_null(ctx.clone()))
    }

    // https://dom.spec.whatwg.org/#dom-node-nextsibling
    fn get_next_sibling(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        Ok(Value::new_null(ctx.clone()))
    }

    // https://dom.spec.whatwg.org/#dom-node-nodevalue
    fn get_node_value(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        self.node_value(ctx)
    }

    // https://dom.spec.whatwg.org/#dom-node-nodevalue
    fn set_node_value(&self, ctx: &Ctx<'js>, value: Option<rquickjs::String<'js>>) -> Result<()> {
        self.set_node_value(ctx, value)
    }

    // https://dom.spec.whatwg.org/#dom-node-textcontent
    fn get_text_content(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        self.text_content(ctx)
    }

    // https://dom.spec.whatwg.org/#dom-node-textcontent
    fn set_text_content(&self, ctx: &Ctx<'js>, value: Option<rquickjs::String<'js>>) -> Result<()> {
        self.set_text_content(ctx, value)
    }

    // https://dom.spec.whatwg.org/#dom-node-normalize
    fn normalize(&self, _ctx: Ctx<'js>) -> Result<()> {
        Ok(())
    }

    // https://dom.spec.whatwg.org/#dom-node-clonenode
    fn clone_node(&self, ctx: Ctx<'js>, arg_0: bool) -> Result<Value<'js>> {
        self.clone_node(ctx, arg_0)
    }

    // https://dom.spec.whatwg.org/#dom-node-isequalnode
    fn is_equal_node(
        &self,
        ctx: Ctx<'js>,
        arg_0: Option<super::host::NodeReference>,
    ) -> Result<bool> {
        self.is_equal_node(ctx, arg_0)
    }

    // https://dom.spec.whatwg.org/#dom-node-issamenode
    fn is_same_node(
        &self,
        _ctx: Ctx<'js>,
        arg_0: Option<super::host::NodeReference>,
    ) -> Result<bool> {
        Ok(self.is_same_node(arg_0))
    }

    // https://dom.spec.whatwg.org/#dom-node-comparedocumentposition
    fn compare_document_position(
        &self,
        ctx: Ctx<'js>,
        arg_0: super::host::NodeReference,
    ) -> Result<u16> {
        self.compare_document_position(ctx, arg_0)
    }

    // https://dom.spec.whatwg.org/#dom-node-contains
    fn contains(&self, _ctx: Ctx<'js>, arg_0: Option<super::host::NodeReference>) -> Result<bool> {
        Ok(self.contains(arg_0))
    }

    // https://dom.spec.whatwg.org/#dom-node-lookupprefix
    fn lookup_prefix(
        &self,
        ctx: Ctx<'js>,
        arg_0: Option<rquickjs::String<'js>>,
    ) -> Result<Option<rquickjs::String<'js>>> {
        let value = self.lookup_prefix(ctx.clone(), arg_0)?;
        if value.is_null() || value.is_undefined() {
            Ok(None)
        } else {
            rquickjs::FromJs::from_js(&ctx, value).map(Some)
        }
    }

    // https://dom.spec.whatwg.org/#dom-node-lookupnamespaceuri
    fn lookup_namespace_uri(
        &self,
        ctx: Ctx<'js>,
        arg_0: Option<rquickjs::String<'js>>,
    ) -> Result<Option<rquickjs::String<'js>>> {
        let value = self.lookup_namespace_uri(ctx.clone(), arg_0)?;
        if value.is_null() || value.is_undefined() {
            Ok(None)
        } else {
            rquickjs::FromJs::from_js(&ctx, value).map(Some)
        }
    }

    // https://dom.spec.whatwg.org/#dom-node-isdefaultnamespace
    fn is_default_namespace(
        &self,
        ctx: Ctx<'js>,
        arg_0: Option<rquickjs::String<'js>>,
    ) -> Result<bool> {
        self.is_default_namespace(ctx, arg_0)
    }

    // https://dom.spec.whatwg.org/#dom-node-insertbefore
    fn insert_before(
        &self,
        ctx: Ctx<'js>,
        arg_0: super::host::NodeReference,
        arg_1: Option<super::host::NodeReference>,
    ) -> Result<Value<'js>> {
        self.insert_before(ctx, arg_0, arg_1)
    }

    // https://dom.spec.whatwg.org/#dom-node-appendchild
    fn append_child(&self, ctx: Ctx<'js>, arg_0: super::host::NodeReference) -> Result<Value<'js>> {
        self.append_child(ctx, arg_0)
    }

    // https://dom.spec.whatwg.org/#dom-node-replacechild
    fn replace_child(
        &self,
        ctx: Ctx<'js>,
        arg_0: super::host::NodeReference,
        arg_1: super::host::NodeReference,
    ) -> Result<Value<'js>> {
        self.replace_child(ctx, arg_0, arg_1)
    }

    // https://dom.spec.whatwg.org/#dom-node-removechild
    fn remove_child(&self, ctx: Ctx<'js>, arg_0: super::host::NodeReference) -> Result<Value<'js>> {
        self.remove_child(ctx, arg_0)
    }
}

impl<'js> attr_generated::Attr<'js> for JsAttr<'js> {
    // https://dom.spec.whatwg.org/#dom-attr-name
    fn get_name(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        rquickjs::String::from_str(
            ctx.clone(),
            &attr_state(ctx, self.scope.0, self.id)?.qualified,
        )
    }

    // https://dom.spec.whatwg.org/#dom-attr-localname
    fn get_local_name(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        rquickjs::String::from_str(ctx.clone(), &attr_state(ctx, self.scope.0, self.id)?.local)
    }

    // https://dom.spec.whatwg.org/#dom-attr-prefix
    fn get_prefix(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        attr_state(ctx, self.scope.0, self.id)?
            .prefix
            .map(|prefix| rquickjs::String::from_str(ctx.clone(), &prefix))
            .transpose()
    }

    // https://dom.spec.whatwg.org/#dom-attr-namespaceuri
    fn get_namespace_uri(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        let namespace = attr_state(ctx, self.scope.0, self.id)?.namespace;
        if namespace.is_empty() {
            Ok(None)
        } else {
            rquickjs::String::from_str(ctx.clone(), &namespace).map(Some)
        }
    }

    // https://dom.spec.whatwg.org/#dom-attr-value
    fn get_value(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        rquickjs::String::from_str(ctx.clone(), &attr_value(ctx, self.scope.0, self.id)?)
    }

    // https://dom.spec.whatwg.org/#dom-attr-value
    fn set_value(&self, ctx: &Ctx<'js>, value: rquickjs::String<'js>) -> Result<()> {
        set_attr_value(ctx, self.scope.0, self.id, value.to_string()?)
    }

    // https://dom.spec.whatwg.org/#dom-attr-ownerelement
    fn get_owner_element(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        let home = attr_context(ctx, self.scope.0, self.id)?;
        child_value(&home, attr_owner(&home, self.scope.0, self.id))
    }

    // https://dom.spec.whatwg.org/#dom-attr-specified
    fn get_specified(&self, _ctx: &Ctx<'js>) -> Result<bool> {
        Ok(true)
    }
}

impl<'js> named_node_map_generated::NamedNodeMap<'js> for JsNamedNodeMap {
    // https://dom.spec.whatwg.org/#dom-namednodemap-length
    fn get_length(&self, ctx: &Ctx<'js>) -> Result<usize> {
        self.length(ctx)
    }

    // https://dom.spec.whatwg.org/#dom-namednodemap-item
    fn item(&self, ctx: Ctx<'js>, arg_0: u32) -> Result<Value<'js>> {
        self.item(ctx, arg_0)
    }

    // https://dom.spec.whatwg.org/#dom-namednodemap-getnameditem
    fn get_named_item(&self, ctx: Ctx<'js>, arg_0: rquickjs::String<'js>) -> Result<Value<'js>> {
        self.get_named_item(ctx, arg_0)
    }

    // https://dom.spec.whatwg.org/#dom-namednodemap-getnameditemns
    fn get_named_item_ns(
        &self,
        ctx: Ctx<'js>,
        arg_0: Option<rquickjs::String<'js>>,
        arg_1: rquickjs::String<'js>,
    ) -> Result<Value<'js>> {
        self.get_named_item_ns(
            ctx,
            OptString(arg_0.map(|name| name.to_string()).transpose()?),
            WebIdlString(arg_1.to_string()?),
        )
    }

    // https://dom.spec.whatwg.org/#dom-namednodemap-setnameditem
    fn set_named_item(&self, ctx: Ctx<'js>, arg_0: Value<'js>) -> Result<Value<'js>> {
        let attr = AttrArgument::from_value(&arg_0)?;
        self.set_named_item(ctx, attr)
    }

    // https://dom.spec.whatwg.org/#dom-namednodemap-setnameditemns
    fn set_named_item_ns(&self, ctx: Ctx<'js>, arg_0: Value<'js>) -> Result<Value<'js>> {
        let attr = AttrArgument::from_value(&arg_0)?;
        self.set_named_item_ns(ctx, attr)
    }

    // https://dom.spec.whatwg.org/#dom-namednodemap-removenameditem
    fn remove_named_item(&self, ctx: Ctx<'js>, arg_0: rquickjs::String<'js>) -> Result<Value<'js>> {
        self.remove_named_item(ctx, WebIdlString(arg_0.to_string()?))
    }

    // https://dom.spec.whatwg.org/#dom-namednodemap-removenameditemns
    fn remove_named_item_ns(
        &self,
        ctx: Ctx<'js>,
        arg_0: Option<rquickjs::String<'js>>,
        arg_1: rquickjs::String<'js>,
    ) -> Result<Value<'js>> {
        self.remove_named_item_ns(
            ctx,
            OptString(arg_0.map(|name| name.to_string()).transpose()?),
            WebIdlString(arg_1.to_string()?),
        )
    }

    // Supported property names are the attribute qualified names. An HTML
    // element in an HTML document drops names that are not already ASCII
    // lowercase. A name that repeats keeps its first attribute.
    // https://dom.spec.whatwg.org/#interface-namednodemap
    fn supported_names(&self, ctx: &Ctx<'js>) -> Result<Vec<String>> {
        let element = self.live_element(ctx)?;
        let html = with_node_data(ctx, element, |data| {
            data.and_then(|data| data.downcast_element())
                .is_some_and(|element| element.name.ns == crate::dom::tree::html_namespace())
        })
        .unwrap_or(false)
            && super::document::document_is_html_content(ctx, element);
        let world_rc = world_for_node(ctx, element)?;
        let world = world_rc.borrow();
        let mut seen = HashSet::new();
        Ok(world
            .document(element)
            .map_or_default(|parsed| {
                let base = &parsed.document.base;
                base.get_node(element.node)
                    .and_then(|node| node.data.downcast_element())
                    .map_or_default(|element| {
                        element
                            .attrs
                            .iter()
                            .map(|attribute| qualified_name(&attribute.name))
                            .collect::<Vec<_>>()
                    })
            })
            .into_iter()
            .filter(|name| !html || !name.bytes().any(|byte| byte.is_ascii_uppercase()))
            .filter(|name| seen.insert(name.clone()))
            .collect())
    }
}

/// `NamedNodeMap`, live over the element's attribute list
/// (<https://dom.spec.whatwg.org/#interface-namednodemap>).
#[derive(Trace, rquickjs::JsLifetime)]
pub struct JsNamedNodeMap {
    pub(crate) element: Handle,
}

include!(concat!(env!("OUT_DIR"), "/NamedNodeMap.rs"));

impl JsNamedNodeMap {
    /// The element this map belongs to. Adoption keeps the map object, so
    /// every operation follows the adopted copy
    /// (<https://dom.spec.whatwg.org/#concept-node-adopt>).
    fn live_element(&self, ctx: &Ctx<'_>) -> Result<NodeId> {
        Ok(world(ctx)?
            .borrow_mut()
            .registry()
            .borrow_mut()
            .live_node_id(self.element.0))
    }

    // https://dom.spec.whatwg.org/#dom-namednodemap-length
    fn length(&self, ctx: &Ctx<'_>) -> Result<usize> {
        let element = self.live_element(ctx)?;
        let world = world(ctx)?;
        let parsed = world.borrow();
        let Some(parsed) = parsed.document(element) else {
            return Ok(0);
        };
        Ok(parsed
            .document
            .base
            .get_node(element.node)
            .and_then(|node| node.data.downcast_element())
            .map_or(0, |element| element.attrs.len()))
    }

    // https://dom.spec.whatwg.org/#dom-namednodemap-item
    fn item<'js>(&self, ctx: Ctx<'js>, index: u32) -> Result<Value<'js>> {
        let element = self.live_element(&ctx)?;
        let Some(attribute) = attribute_at(&ctx, element, i64::from(index))? else {
            return Ok(Value::new_null(ctx));
        };
        match attached_attr_id(&ctx, element, &attribute.0, &attribute.1)? {
            Some(id) => attr_wrapper(&ctx, id),
            None => Ok(Value::new_null(ctx)),
        }
    }

    // https://dom.spec.whatwg.org/#dom-namednodemap-getnameditem
    fn get_named_item<'js>(
        &self,
        ctx: Ctx<'js>,
        name: rquickjs::String<'js>,
    ) -> Result<Value<'js>> {
        named_item(&ctx, self.live_element(&ctx)?, &name.to_string()?)
    }

    // https://dom.spec.whatwg.org/#dom-namednodemap-getnameditemns
    fn get_named_item_ns<'js>(
        &self,
        ctx: Ctx<'js>,
        namespace: OptString,
        local: WebIdlString,
    ) -> Result<Value<'js>> {
        let element = self.live_element(&ctx)?;
        let namespace = namespace.0.unwrap_or_default();
        match attached_attr_id(&ctx, element, &namespace, &local.0)? {
            Some(id) => attr_wrapper(&ctx, id),
            None => Ok(Value::new_null(ctx)),
        }
    }

    // https://dom.spec.whatwg.org/#dom-namednodemap-setnameditem
    fn set_named_item<'js>(&self, ctx: Ctx<'js>, attr: AttrArgument<'js>) -> Result<Value<'js>> {
        set_attribute_node(&ctx, self.live_element(&ctx)?, &attr.into_value())
    }

    // https://dom.spec.whatwg.org/#dom-namednodemap-setnameditemns
    fn set_named_item_ns<'js>(&self, ctx: Ctx<'js>, attr: AttrArgument<'js>) -> Result<Value<'js>> {
        set_attribute_node(&ctx, self.live_element(&ctx)?, &attr.into_value())
    }

    // https://dom.spec.whatwg.org/#dom-namednodemap-removenameditem
    fn remove_named_item<'js>(&self, ctx: Ctx<'js>, name: WebIdlString) -> Result<Value<'js>> {
        let element = self.live_element(&ctx)?;
        let world_rc = world(&ctx)?;
        let Some((namespace, local, id)) = named_attribute_id(&ctx, &world_rc, element, &name.0)?
        else {
            return Err(throw_dom(&ctx, "NotFoundError", "no such attribute"));
        };
        let value = attr_wrapper(&ctx, id)?;
        remove_attribute_sync(&ctx, element, &namespace, &local, true)?;
        Ok(value)
    }

    // https://dom.spec.whatwg.org/#dom-namednodemap-removenameditemns
    fn remove_named_item_ns<'js>(
        &self,
        ctx: Ctx<'js>,
        namespace: OptString,
        local: WebIdlString,
    ) -> Result<Value<'js>> {
        let element = self.live_element(&ctx)?;
        let namespace = namespace.0.unwrap_or_default();
        let value = match attached_attr_id(&ctx, element, &namespace, &local.0)? {
            Some(id) => attr_wrapper(&ctx, id)?,
            None => return Err(throw_dom(&ctx, "NotFoundError", "no such attribute")),
        };
        remove_attribute_sync(&ctx, element, &namespace, &local.0, true)?;
        Ok(value)
    }
}

/// `(namespace, local)` of the attribute at `index`, if any.
fn attribute_at(ctx: &Ctx<'_>, element: NodeId, index: i64) -> Result<Option<(String, String)>> {
    let world_rc = world_for_node(ctx, element)?;
    let world = world_rc.borrow();
    let Some(parsed) = world.document(element) else {
        return Ok(None);
    };
    let base = &parsed.document.base;
    let Some(data) = base
        .get_node(element.node)
        .and_then(|node| node.data.downcast_element())
    else {
        return Ok(None);
    };
    Ok(usize::try_from(index)
        .ok()
        .and_then(|index| data.attrs.as_slice().get(index))
        .map(|attribute| {
            (
                attribute.name.ns.to_string(),
                attribute.name.local.to_string(),
            )
        }))
}

/// The attribute with qualified name `name`, as an `Attr` wrapper.
fn named_item<'js>(ctx: &Ctx<'js>, element: NodeId, name: &str) -> Result<Value<'js>> {
    let world_rc = world_for_node(ctx, element)?;
    match named_attribute_id(ctx, &world_rc, element, name)? {
        Some((_, _, id)) => attr_wrapper(ctx, id),
        None => Ok(Value::new_null(ctx.clone())),
    }
}

/// The qualified name `name` matches on `element`.
///
/// An element in the HTML namespace whose node document is an HTML document
/// matches the ASCII-lowercased name. Any other element keeps the name.
/// <https://dom.spec.whatwg.org/#concept-element-attributes-get-by-name>
pub(super) fn attribute_local_name(ctx: &Ctx<'_>, element: NodeId, name: &str) -> String {
    // A missing document falls back to the non-HTML spelling instead of throwing.
    let html = with_node_data(ctx, element, |data| {
        data.and_then(|data| data.downcast_element())
            .is_some_and(|element| element.name.ns == crate::dom::tree::html_namespace())
    })
    .unwrap_or(false)
        && super::document::document_is_html_content(ctx, element);
    if html {
        name.to_ascii_lowercase()
    } else {
        name.to_owned()
    }
}

/// The attached `Attr` id of the attribute with qualified name `name`, with
/// its `(namespace, local)` key. The world must be the one whose document
/// store the caller wants searched.
fn named_attribute_id(
    ctx: &Ctx<'_>,
    world_rc: &Rc<RefCell<World>>,
    element: NodeId,
    name: &str,
) -> Result<Option<(String, String, u64)>> {
    let name = attribute_local_name(ctx, element, name);
    let found = {
        let world = world_rc.borrow();
        let Some(parsed) = world.document(element) else {
            return Ok(None);
        };
        parsed
            .document
            .base
            .get_node(element.node)
            .and_then(|node| node.data.downcast_element())
            .and_then(|element| {
                element
                    .attrs
                    .iter()
                    .find(|attribute| qualified_name_eq(&attribute.name, &name))
                    .map(|attribute| {
                        (
                            attribute.name.ns.to_string(),
                            attribute.name.local.to_string(),
                        )
                    })
            })
    };
    let Some((namespace, local)) = found else {
        return Ok(None);
    };
    Ok(attached_attr_id(ctx, element, &namespace, &local)?.map(|id| (namespace, local, id)))
}

// ── Attr registry helpers ────────────────────────────────────────────────

pub(crate) fn attr_state(ctx: &Ctx<'_>, scope: NodeId, id: u64) -> Result<AttrState> {
    let registry = realm_registry(ctx)?;
    registry
        .borrow()
        .attributes
        .state(id)
        .filter(|state| state.scope == scope)
        .cloned()
        .ok_or_else(|| Exception::throw_type(ctx, "stale attribute"))
}

fn attr_context<'js>(ctx: &Ctx<'js>, scope: NodeId, id: u64) -> Result<Ctx<'js>> {
    let state = attr_state(ctx, scope, id)?;
    let owner = world_for_node(ctx, state.document)?;
    let prototype = owner
        .borrow()
        .brand("Attr")
        .ok_or_else(|| Exception::throw_internal(ctx, "missing Attr prototype"))?;
    Ok(prototype.restore(ctx)?.ctx().clone())
}

/// The attached element for `id`, or `None` when detached.
pub(crate) fn attr_owner(ctx: &Ctx<'_>, scope: NodeId, id: u64) -> Option<NodeId> {
    let state = attr_state(ctx, scope, id).ok()?;
    let owner = state.owner?;
    let world_rc = world_for_node(ctx, owner).ok()?;
    let world = world_rc.borrow();
    let parsed = world.document(owner)?;
    element_attribute(
        &parsed.document.base,
        owner.node,
        &state.namespace,
        &state.local,
    )
    .map(|_| owner)
}

fn attr_value(ctx: &Ctx<'_>, scope: NodeId, id: u64) -> Result<String> {
    let state = attr_state(ctx, scope, id)?;
    if let Some(owner) = attr_owner(ctx, scope, id) {
        let world_rc = world_for_node(ctx, owner)?;
        let world = world_rc.borrow();
        if let Some(parsed) = world.document(owner)
            && let Some(value) = attribute_ns_value(
                &parsed.document.base,
                owner.node,
                &state.namespace,
                &state.local,
            )
        {
            return Ok(value);
        }
    }
    Ok(state.value)
}

fn set_attr_value(ctx: &Ctx<'_>, scope: NodeId, id: u64, value: String) -> Result<()> {
    let home = attr_context(ctx, scope, id)?;
    let snapshot = attr_state(ctx, scope, id)?;
    let world_rc = match snapshot.owner {
        Some(owner) => world_for_node(&home, owner)?,
        None => world(&home)?,
    };
    if let Some(owner) = snapshot.owner {
        let name = QualName::new(
            snapshot.prefix.as_deref().map(markup5ever::Prefix::from),
            markup5ever::Namespace::from(snapshot.namespace.clone()),
            LocalName::from(snapshot.local.clone()),
        );
        let world = world_rc.borrow();
        let old_value = world.document(owner).and_then(|parsed| {
            attribute_ns_value(
                &parsed.document.base,
                owner.node,
                &snapshot.namespace,
                &snapshot.local,
            )
        });
        let Some(mut parsed) = world.document_mut(owner) else {
            return Err(Exception::throw_type(ctx, "stale attribute owner"));
        };
        if parsed.document.base.get_node(owner.node).is_none() {
            return Err(Exception::throw_type(ctx, "stale attribute owner"));
        }
        parsed
            .document
            .base
            .mutate()
            .set_attribute(owner.node, name, &value);
        parsed.document.record(JournalEntry::Attributes {
            target: owner,
            // attributeName is the attribute's local name.
            // https://dom.spec.whatwg.org/#handle-attribute-changes
            name: snapshot.local.clone(),
            namespace: snapshot.namespace,
            old_value,
        });
        drop(parsed);
        drop(world);
    }
    let world = world_rc.borrow();
    let registry = world.registry();
    let mut registry = registry.borrow_mut();
    let state = registry
        .attributes
        .state_mut(id)
        .filter(|state| state.scope == scope)
        .ok_or_else(|| Exception::throw_type(ctx, "stale attribute"))?;
    state.value = value;
    drop(registry);
    drop(world);
    schedule_mutation_delivery(&home)
}

/// The `Attr` id attached at `(element, namespace, local)`, creating the
/// platform object's identity on first access. Clears a stale mapping when
/// the attribute is gone.
pub(crate) fn attached_attr_id(
    ctx: &Ctx<'_>,
    element: NodeId,
    namespace: &str,
    local: &str,
) -> Result<Option<u64>> {
    let world_rc = world_for_node(ctx, element)?;
    let world = world_rc.borrow();
    let info = {
        let Some(parsed) = world.document(element) else {
            return Ok(None);
        };
        element_attribute(&parsed.document.base, element.node, namespace, local)
    };
    let document = world
        .document(element)
        .map(|parsed| NodeId {
            document: element.document,
            node: parsed.document.base.root_node().id,
        })
        .ok_or_else(|| Exception::throw_type(ctx, "stale attribute owner"))?;
    let registry = world.registry();
    drop(world);
    let mut registry = registry.borrow_mut();
    let Some((prefix, qualified, value)) = info else {
        registry.attributes.detach(element, namespace, local);
        return Ok(None);
    };
    if let Some(id) = registry.attributes.attached(element, namespace, local) {
        registry.attributes.touch(element, namespace, local, value);
        return Ok(Some(id));
    }
    let id = registry
        .attributes
        .create(AttrState {
            scope: document,
            document,
            owner: Some(element),
            value,
            namespace: namespace.to_owned(),
            prefix,
            local: local.to_owned(),
            qualified,
        })
        .ok_or_else(|| Exception::throw_internal(ctx, "attribute ids exhausted"))?;
    Ok(Some(id))
}

/// Restores or creates the wrapper for an `Attr` id.
pub(crate) fn attr_wrapper<'js>(ctx: &Ctx<'js>, id: u64) -> Result<Value<'js>> {
    let registry = realm_registry(ctx)?;
    let state = registry
        .borrow()
        .attributes
        .state(id)
        .cloned()
        .ok_or_else(|| Exception::throw_type(ctx, "stale attribute"))?;
    let home = attr_context(ctx, state.scope, id)?;
    if let Some(saved) = registry.borrow().attributes.wrapper(id)
        && let Some(value) = deref_weak(&home, saved)?
    {
        return Ok(value);
    }
    // https://webidl.spec.whatwg.org/#internally-create-a-new-object-implementing-the-interface
    let owner = world_for_node(&home, state.document)?;
    let proto = owner
        .borrow()
        .brand("Attr")
        .ok_or_else(|| Exception::throw_internal(ctx, "missing Attr prototype"))?;
    let proto = proto.restore(ctx)?;
    let class = super::host::instance_proto(
        JsAttr {
            id,
            scope: Handle(state.scope),
            child_nodes: AttrChildren(RefCell::new(None)),
        },
        proto,
    )?;
    let value = Class::into_value(class);
    let weak = make_weak(&home, value.clone())?;
    registry
        .borrow_mut()
        .attributes
        .intern_wrapper(id, Persistent::save(&home, weak));
    Ok(value)
}

/// [Imports](https://dom.spec.whatwg.org/#dom-document-importnode) an `Attr`
/// into `target_document` as a new detached attribute.
pub(crate) fn import_attr<'js>(
    ctx: &Ctx<'js>,
    target_document: NodeId,
    scope: NodeId,
    id: u64,
) -> Result<Value<'js>> {
    let state = attr_state(ctx, scope, id)?;
    let value = attr_value(ctx, scope, id)?;
    let clone = new_detached_attr(
        ctx,
        target_document,
        state.namespace,
        state.prefix,
        state.local,
        state.qualified,
    )?;
    set_attr_value(ctx, target_document, clone, value)?;
    attr_wrapper(ctx, clone)
}

/// Creates a detached `Attr` identity; attaches via `setAttributeNode`.
pub(crate) fn new_detached_attr(
    ctx: &Ctx<'_>,
    scope: NodeId,
    namespace: String,
    prefix: Option<String>,
    local: String,
    qualified: String,
) -> Result<u64> {
    let world_rc = world_for_node(ctx, scope)?;
    let world = world_rc.borrow();
    let document = world
        .document(scope)
        .map(|parsed| NodeId {
            document: scope.document,
            node: parsed.document.base.root_node().id,
        })
        .ok_or_else(|| Exception::throw_type(ctx, "stale attribute document"))?;
    let registry = world.registry();
    drop(world);
    let id = registry
        .borrow_mut()
        .attributes
        .create(AttrState {
            scope: document,
            document,
            owner: None,
            value: String::new(),
            namespace,
            prefix,
            local,
            qualified,
        })
        .ok_or_else(|| Exception::throw_internal(ctx, "attribute ids exhausted"))?;
    Ok(id)
}

/// Keeps an existing attached `Attr` wrapper in sync after a value change.
pub(crate) fn touch_attr(
    ctx: &Ctx<'_>,
    element: NodeId,
    namespace: &str,
    local: &str,
    value: &str,
) -> Result<()> {
    let registry = realm_registry(ctx)?;
    registry
        .borrow_mut()
        .attributes
        .touch(element, namespace, local, value.to_owned());
    schedule_mutation_delivery(ctx)
}

/// Event handler attribute names that the `<body>` element forwards to the
/// window ([WindowEventHandlers](https://html.spec.whatwg.org/multipage/webappapis.html#windoweventhandlers)).
const WINDOW_HANDLER_ATTRIBUTES: &[&str] = &[
    "onafterprint",
    "onbeforeprint",
    "onbeforeunload",
    "onhashchange",
    "onlanguagechange",
    "onmessage",
    "onmessageerror",
    "onoffline",
    "ononline",
    "onpagehide",
    "onpageshow",
    "onpopstate",
    "onrejectionhandled",
    "onstorage",
    "onunhandledrejection",
    "onunload",
];

/// Compiles or clears one element's event handler content attribute.
fn compile_handler_attribute(ctx: &Ctx<'_>, element: NodeId, typ: &str) -> Result<()> {
    let name = format!("on{typ}");
    let body = world(ctx)?.borrow().document(element).and_then(|parsed| {
        crate::dom::tree::attr(&parsed.document.base, element.node, &name).map(ToOwned::to_owned)
    });
    let Some(object) = wrap_node(ctx, element)?.as_object().cloned() else {
        return Ok(());
    };
    // A `body` element's window event handler attributes register on the
    // window itself
    // (<https://html.spec.whatwg.org/multipage/dom.html#body-element-event-handlers>).
    let is_body = world_for_node(ctx, element).is_ok_and(|owner| {
        owner.borrow().document(element).is_some_and(|parsed| {
            crate::dom::tree::is_html_element(&parsed.document.base, element.node, "body")
        })
    });
    let forwarded = WINDOW_HANDLER_ATTRIBUTES.contains(&name.as_str()) && is_body;
    match body {
        Some(body) if !body.trim().is_empty() => {
            // The `onerror` handler takes the spec's five arguments, not the
            // usual single event argument
            // (<https://html.spec.whatwg.org/multipage/webappapis.html#the-event-handler-processing-algorithm>).
            let params = if name == "onerror" {
                &["event", "source", "lineno", "colno", "error"] as &[&str]
            } else {
                &["event"] as &[&str]
            };
            // The inline check belongs to the attribute, so it runs when the
            // attribute is compiled. A handler blocked here leaves its slot
            // null: setting the attribute must neither run the body nor throw
            // (<https://w3c.github.io/webappsec-csp/#should-block-inline>).
            // The setting script's position becomes the violation's source.
            let position = crate::js::error_stack_top(ctx);
            let position = position
                .as_ref()
                .map(|(url, line, column)| (url.as_str(), *line, *column));
            if !crate::js::events::handler_attribute_allowed(
                ctx,
                object.as_value(),
                &body,
                position,
            ) {
                object.set(name.as_str(), Value::new_null(ctx.clone()))?;
                if forwarded {
                    ctx.globals()
                        .set(name.as_str(), Value::new_null(ctx.clone()))?;
                }
                return Ok(());
            }
            match crate::js::events::compile_handler_function(ctx, params, &body) {
                Ok(compiled) => {
                    object.set(name.as_str(), compiled.clone())?;
                    if forwarded {
                        ctx.globals().set(name.as_str(), compiled)?;
                    }
                }
                Err(error) => {
                    // A malformed handler reports the error and clears the
                    // slot; setting the attribute must not throw
                    // (<https://html.spec.whatwg.org/multipage/webappapis.html#event-handler-content-attributes>).
                    report_exception(ctx, &error);
                    object.set(name.as_str(), Value::new_null(ctx.clone()))?;
                    if forwarded {
                        ctx.globals()
                            .set(name.as_str(), Value::new_null(ctx.clone()))?;
                    }
                }
            }
        }
        _ => {
            object.set(name.as_str(), Value::new_null(ctx.clone()))?;
            if forwarded {
                ctx.globals()
                    .set(name.as_str(), Value::new_null(ctx.clone()))?;
            }
        }
    }
    Ok(())
}

/// Records the executing script for an element whose `src` set chose its URL,
/// for later CSP `sourceFile` reporting
/// (<https://w3c.github.io/webappsec-csp/#create-violation-for-global>).
fn note_src_creator(ctx: &Ctx<'_>, element: NodeId) {
    let Some((url, line, column)) = crate::js::error_stack_top(ctx) else {
        return;
    };
    // The entry lives in the element owner's world, where a later violation
    // for its load looks it up.
    let Ok(owner) = world_for_node(ctx, element) else {
        return;
    };
    owner
        .borrow_mut()
        .state
        .creator_stacks
        .insert(element, crate::js::CreatorInfo { url, line, column });
}

/// What a type change does with the current value slot: the sanitized value
/// and the dirty value flag afterwards
/// (<https://html.spec.whatwg.org/multipage/input.html#the-input-element>).
struct InputValueStore {
    value: String,
    dirtiness: Dirtiness,
}

/// Recomputes an input's sanitized stored value after one of the content
/// attributes that feeds its value sanitization changed
/// (<https://html.spec.whatwg.org/multipage/input.html#the-input-element>).
fn apply_input_value_attribute_change(ctx: &Ctx<'_>, element: NodeId, local: &str) -> Result<()> {
    let world_rc = world_for_node(ctx, element)?;
    let drives_value = {
        let world = world_rc.borrow();
        let Some(mut parsed) = world.document_mut(element) else {
            return Ok(());
        };
        if !crate::dom::tree::is_html_element(&parsed.document.base, element.node, "input") {
            return Ok(());
        }
        let dirty = parsed
            .document
            .base
            .get_node(element.node)
            .and_then(|node| node.data.downcast_element())
            .is_some_and(|input| input.form_state.dirty_value());
        crate::dom::input::sync_sanitized_value(&mut parsed.document, element.node);
        local == "value" && !dirty
    };
    // Only a `value` attribute write that still drives the value ends the edit
    // state; `min`/`max`/`step` do not mark a user edit over
    // (<https://html.spec.whatwg.org/multipage/input.html#concept-fe-value>).
    if drives_value {
        world_rc.borrow_mut().clear_control_edit_state(element);
    }
    Ok(())
}

/// Runs the select selectedness rules after an attribute that changes the
/// available or default selection
/// (<https://html.spec.whatwg.org/multipage/form-elements.html#the-select-element>).
fn apply_select_attribute_change(ctx: &Ctx<'_>, element: NodeId, local: &str) -> Result<()> {
    let world = world_for_node(ctx, element)?;
    let world = world.borrow();
    let Some(mut parsed) = world.document_mut(element) else {
        return Ok(());
    };
    let base = &parsed.document.base;
    let is_option = crate::dom::tree::is_html_element(base, element.node, "option");
    let is_select = crate::dom::tree::is_html_element(base, element.node, "select");
    if local == "selected" && is_option {
        crate::dom::forms::option_selected_attribute_changed(&mut parsed, element.node);
        return Ok(());
    }
    if (local == "multiple" || local == "size") && is_select {
        crate::dom::forms::select_attribute_changed(&mut parsed, element.node, local);
        return Ok(());
    }
    if local == "disabled"
        && let Some(select) = crate::dom::forms::option_select_owner(base, element.node)
    {
        crate::dom::forms::select_attribute_changed(&mut parsed, select, "disabled");
    }
    Ok(())
}

/// Runs the input element's type-change steps
/// (<https://html.spec.whatwg.org/multipage/input.html#the-input-element>).
///
/// A change between keywords of the same state leaves the value alone.
/// Leaving value mode for a button state makes the current value the new
/// default, entering value mode reloads the default and clears the dirty
/// flag, and the state's sanitization algorithm runs last.
fn apply_input_type_change(ctx: &Ctx<'_>, element: NodeId, previous: Option<&str>) -> Result<()> {
    let old = crate::dom::input::InputType::from_attribute(previous);
    let world_rc = world_for_node(ctx, element)?;
    let (new, current) = {
        let world = world_rc.borrow();
        let Some(parsed) = world.document(element) else {
            return Ok(());
        };
        let base = &parsed.document.base;
        let Some(new) = crate::dom::input::input_type(base, element.node) else {
            return Ok(());
        };
        (
            new,
            crate::dom::input::raw_value(&parsed, element.node, old),
        )
    };
    if old == new {
        return Ok(());
    }
    let store = input_type_store(ctx, &world_rc, element, old, new, &current)?;
    if let Some(store) = store {
        let world = world_rc.borrow();
        let Some(mut parsed) = world.document_mut(element) else {
            return Ok(());
        };
        parsed
            .document
            .base
            .mutate()
            .set_value(element.node, store.value, store.dirtiness);
    }
    apply_input_selection(&world_rc, element, old, new);
    // The edit buffer, caret, and bad-input flag belong to the previous input
    // type
    // (<https://html.spec.whatwg.org/multipage/input.html#the-input-element>).
    world_rc.borrow_mut().clear_control_edit_state(element);
    Ok(())
}

/// Decides what the current value becomes across a type change
/// (<https://html.spec.whatwg.org/multipage/input.html#the-input-element>).
fn input_type_store(
    ctx: &Ctx<'_>,
    world: &Rc<RefCell<World>>,
    element: NodeId,
    old: crate::dom::input::InputType,
    new: crate::dom::input::InputType,
    current: &str,
) -> Result<Option<InputValueStore>> {
    let (attribute, dirty) = {
        let world = world.borrow();
        let Some(parsed) = world.document(element) else {
            return Ok(None);
        };
        let base = &parsed.document.base;
        (
            crate::dom::tree::attr(base, element.node, "value").map(ToOwned::to_owned),
            base.get_node(element.node)
                .and_then(|node| node.data.downcast_element())
                .is_some_and(|element| element.form_state.dirty_value()),
        )
    };
    let old_mode = old.value_mode();
    let new_mode = new.value_mode();
    // Leaving value mode for a button state makes the current value the new
    // default; the dirty flag stays so a later return to value mode reloads
    // the same string (<https://html.spec.whatwg.org/multipage/input.html#the-input-element>).
    if old_mode == crate::dom::input::ValueMode::Current
        && matches!(
            new_mode,
            crate::dom::input::ValueMode::Default | crate::dom::input::ValueMode::DefaultOn
        )
    {
        let value = sanitize_current(world, element, old, current);
        if !value.is_empty() {
            set_attribute_sync(ctx, element, "value", &value)?;
        }
        return Ok(None);
    }
    if old_mode == crate::dom::input::ValueMode::Current
        && new_mode == crate::dom::input::ValueMode::Current
    {
        // Both states carry a value: keep it and sanitize for the new state.
        // A user-written value stays dirty; a value that followed its
        // attribute keeps following it.
        return Ok(Some(InputValueStore {
            value: sanitize_current(world, element, new, current),
            dirtiness: if dirty {
                Dirtiness::Set
            } else {
                Dirtiness::Keep
            },
        }));
    }
    if old_mode != crate::dom::input::ValueMode::Current
        && new_mode == crate::dom::input::ValueMode::Current
    {
        // Entering value mode reloads the default, clears the dirty flag, and
        // sanitizes the stored value
        // (<https://html.spec.whatwg.org/multipage/input.html#the-input-element>).
        let raw = attribute.unwrap_or_default();
        return Ok(Some(InputValueStore {
            value: sanitize_current(world, element, new, &raw),
            dirtiness: Dirtiness::Clear,
        }));
    }
    Ok(None)
}

/// Moves the text entry cursor after a type change. A state that gains text
/// entry starts at the beginning; the type-change steps do not collapse the
/// cursor to the end
/// (<https://html.spec.whatwg.org/multipage/input.html#the-input-element>).
fn apply_input_selection(
    world: &Rc<RefCell<World>>,
    element: NodeId,
    old: crate::dom::input::InputType,
    new: crate::dom::input::InputType,
) {
    if !old.selection_applies() && new.selection_applies() {
        world.borrow_mut().set_selection(element, 0, 0, 0);
    }
}

fn sanitize_current(
    world: &Rc<RefCell<World>>,
    element: NodeId,
    kind: crate::dom::input::InputType,
    value: &str,
) -> String {
    let world = world.borrow();
    world
        .document_mut(element)
        .map_or_else(String::new, |mut parsed| {
            crate::dom::input::sanitize_with_device(
                &mut parsed.document.base,
                element.node,
                kind,
                value,
            )
        })
}

/// A `meta` element re-runs its pragma algorithms when `name`, `content`, or
/// `http-equiv` changes: `name=referrer` resets the document policy and
/// `http-equiv=refresh` starts a refresh
/// (<https://html.spec.whatwg.org/multipage/semantics.html#meta-referrer>,
/// <https://html.spec.whatwg.org/multipage/semantics.html#attr-meta-http-equiv-refresh>).
fn refresh_meta_pragmas(ctx: &Ctx<'_>, element: NodeId, local: &str) {
    if !matches!(local, "name" | "content" | "http-equiv") {
        return;
    }
    let is_meta = world_for_node(ctx, element).is_ok_and(|owner| {
        owner.borrow().document(element).is_some_and(|parsed| {
            crate::dom::tree::is_html_element(&parsed.document.base, element.node, "meta")
        })
    });
    if !is_meta {
        return;
    }
    crate::js::bindings::node::document::apply_meta_referrer(ctx, element);
    crate::js::bindings::node::document::apply_meta_refresh(ctx, element);
}

/// After an attribute change, runs the element's attribute-change hooks: an
/// `iframe`'s `src` drives its browsing context's navigation
/// (<https://html.spec.whatwg.org/multipage/iframe-embed-object.html#process-the-iframe-attributes>),
/// an `<img>` `src` updates the image data
/// (<https://html.spec.whatwg.org/multipage/images.html#updating-the-image-data>),
/// and an event handler content attribute compiles into its handler property
/// (<https://html.spec.whatwg.org/multipage/webappapis.html#event-handler-content-attributes>).
pub(crate) fn after_attribute_change(
    ctx: &Ctx<'_>,
    element: NodeId,
    local: &str,
    previous: Option<&str>,
) -> Result<()> {
    refresh_meta_pragmas(ctx, element, local);
    if let Some(typ) = local.strip_prefix("on")
        && !typ.is_empty()
        && typ
            .chars()
            .all(|character| character.is_ascii_lowercase() || character.is_ascii_digit())
    {
        compile_handler_attribute(ctx, element, typ)?;
    }
    if local == "type" {
        apply_input_type_change(ctx, element, previous)?;
    }
    if matches!(local, "selected" | "multiple" | "size" | "disabled") {
        apply_select_attribute_change(ctx, element, local)?;
    }
    if matches!(
        local,
        "value" | "min" | "max" | "step" | "multiple" | "colorspace" | "alpha"
    ) {
        apply_input_value_attribute_change(ctx, element, local)?;
    }
    // A `<base href>` (set, changed, or removed) re-runs HTML's set the frozen
    // base URL, which is where `base-uri` is enforced
    // (<https://html.spec.whatwg.org/multipage/semantics.html#the-base-element>,
    // <https://w3c.github.io/webappsec-csp/#is-base-allowed-for-document>).
    if local == "href" {
        let owner = world_for_node(ctx, element)?;
        let is_base = owner.borrow().document(element).is_some_and(|parsed| {
            crate::dom::tree::is_html_element(&parsed.document.base, element.node, "base")
        });
        if is_base {
            owner.borrow_mut().apply_base_element(element);
        }
    }
    if local != "src" && local != "srcdoc" {
        return Ok(());
    }
    let (is_iframe, is_img, is_script) = world_for_node(ctx, element)
        .ok()
        .and_then(|owner| {
            owner.borrow().document(element).map(|parsed| {
                let base = &parsed.document.base;
                (
                    crate::dom::tree::is_html_element(base, element.node, "iframe"),
                    crate::dom::tree::is_html_element(base, element.node, "img"),
                    crate::dom::tree::is_html_element(base, element.node, "script"),
                )
            })
        })
        .unwrap_or((false, false, false));
    if !is_iframe && !is_img && !is_script {
        return Ok(());
    }
    let world = world(ctx)?;
    // A `srcdoc` set, changed, or removed reprocesses the iframe attributes;
    // so does a `src` change, but only while no `srcdoc` is specified
    // (<https://html.spec.whatwg.org/multipage/iframe-embed-object.html#process-the-iframe-attributes>).
    let srcdoc = if is_iframe && matches!(local, "srcdoc" | "src") {
        let has_srcdoc = world.borrow().document(element).is_some_and(|parsed| {
            crate::dom::tree::attr(&parsed.document.base, element.node, "srcdoc").is_some()
        });
        if local == "srcdoc" && has_srcdoc {
            world.borrow().document(element).and_then(|parsed| {
                crate::dom::tree::attr(&parsed.document.base, element.node, "srcdoc")
                    .map(ToOwned::to_owned)
            })
        } else if local == "src" && has_srcdoc {
            // `src` changes while `srcdoc` is present change nothing
            // (<https://html.spec.whatwg.org/multipage/iframe-embed-object.html#process-the-iframe-attributes>).
            return Ok(());
        } else {
            None
        }
    } else {
        None
    };
    let spec = world
        .borrow()
        .document(element)
        .and_then(|parsed| {
            crate::dom::tree::attr(&parsed.document.base, element.node, "src")
                .map(ToOwned::to_owned)
        })
        .unwrap_or_default();
    // The fetch a set triggers attributes to the executing script: a later
    // CSP violation for the load reports it as `sourceFile`, `lineNumber`,
    // and `columnNumber`
    // (<https://w3c.github.io/webappsec-csp/#create-violation-for-global>).
    // Record even when detached: the URL enters the element here, and the
    // insertion hook keeps the earliest trigger, so a script that sets
    // `src` then inserts reports the set, matching the line browsers
    // attribute the mutation to.
    if is_img || is_script {
        note_src_creator(ctx, element);
    }
    // An `<img>` restarts its request from Blitz's attribute hook, which also
    // runs for a detached `new Image()`; the settlement events drive the
    // element's state
    // (<https://html.spec.whatwg.org/multipage/images.html#reacting-to-dom-mutations>).
    if is_img {
        return Ok(());
    }
    // A detached `iframe` has no browsing context yet; insertion reads the
    // current attribute, so queueing here would navigate it twice.
    let connected = world
        .borrow()
        .document(element)
        .is_some_and(|parsed| crate::dom::tree::is_connected(&parsed.document.base, element.node));
    if !connected {
        return Ok(());
    }
    if is_iframe {
        navigate_from_attribute_change(ctx, element, true, &spec, srcdoc)?;
    } else {
        navigate_from_attribute_change(ctx, element, false, &spec, None)?;
    }
    Ok(())
}

/// Starts what a `src`/`srcdoc` change on a connected element triggers: an
/// `iframe` reprocesses its attributes and navigates, a `script` runs its
/// prepare steps
/// (<https://html.spec.whatwg.org/multipage/iframe-embed-object.html#process-the-iframe-attributes>,
/// <https://html.spec.whatwg.org/multipage/scripting.html#prepare-the-script-element>).
fn navigate_from_attribute_change(
    ctx: &Ctx<'_>,
    element: NodeId,
    is_iframe: bool,
    spec: &str,
    srcdoc: Option<String>,
) -> Result<()> {
    let world = world(ctx)?;
    if !is_iframe {
        // `src` set on a connected classic script runs the prepare steps,
        // including the fetch when the element is not already started.
        world.borrow_mut().queue_script(element);
        return Ok(());
    }
    if let Some(markup) = srcdoc {
        world
            .borrow_mut()
            .queue_frame_navigation(FrameNavigation::srcdoc(
                NavigationTarget::Container(element),
                markup,
            ));
        return Ok(());
    }
    // The iframe's own `referrerpolicy` attribute governs its navigation
    // (<https://html.spec.whatwg.org/multipage/iframe-embed-object.html#attr-iframe-referrerpolicy>).
    let referrer_policy = world.borrow().document(element).and_then(|parsed| {
        crate::dom::tree::attr(&parsed.document.base, element.node, "referrerpolicy")
            .and_then(net::ReferrerPolicy::parse)
    });
    world.borrow_mut().queue_frame_navigation(
        FrameNavigation::get(NavigationTarget::Container(element), spec.to_owned())
            .with_referrer_policy(referrer_policy),
    );
    Ok(())
}

/// The value of one event handler content attribute, when the element has it.
pub(crate) fn handler_attribute(ctx: &Ctx<'_>, id: NodeId, name: &str) -> Result<Option<String>> {
    let world = world(ctx)?;
    Ok(world.borrow().document(id).and_then(|parsed| {
        crate::dom::tree::attr(&parsed.document.base, id.node, name).map(ToOwned::to_owned)
    }))
}

/// Whether script explicitly cleared this element's handler property, which
/// must keep a dispatch from falling back to the content attribute.
pub(crate) fn handler_cleared(ctx: &Ctx<'_>, id: NodeId, name: &str) -> Result<bool> {
    let world = world_for_node(ctx, id)?;
    Ok(world.borrow().handler_cleared(Some(id), name))
}

/// Whether script cleared the window-scoped handler property. Body `on*`
/// content attributes forward to the window, so a cleared window flag
/// suppresses the body attribute too
/// (<https://html.spec.whatwg.org/multipage/dom.html#body-element-event-handlers>).
pub(crate) fn window_handler_cleared(ctx: &Ctx<'_>, id: NodeId, name: &str) -> Result<bool> {
    let world = world_for_node(ctx, id)?;
    Ok(world.borrow().handler_cleared(None, name))
}

/// Removes the DOM attribute identified by `(namespace, local)` and syncs
/// the registry; used by `removeAttributeNode` and `NamedNodeMap.remove*`.
pub(crate) fn remove_attribute_sync(
    ctx: &Ctx<'_>,
    element: NodeId,
    namespace: &str,
    local: &str,
    by_namespace: bool,
) -> Result<()> {
    let world_rc = world_for_node(ctx, element)?;
    let removed = {
        let world = world_rc.borrow();
        let Some(mut parsed) = world.document_mut(element) else {
            return Ok(());
        };
        let removed = parsed
            .document
            .base
            .get_node(element.node)
            .and_then(|node| node.data.downcast_element())
            .and_then(|data| {
                data.attrs
                    .iter()
                    .find(|attribute| {
                        if by_namespace {
                            attribute.name.ns.as_str() == namespace
                                && attribute.name.local.as_str() == local
                        } else {
                            qualified_name_eq(&attribute.name, local)
                        }
                    })
                    .map(|attribute| (attribute.name.clone(), attribute.value.clone()))
            });
        if let Some((name, value)) = removed.as_ref() {
            parsed.document.remember_attribute(
                element.node,
                local,
                crate::dom_string::DomString::default(),
            );
            parsed
                .document
                .base
                .mutate()
                .clear_attribute(element.node, name.clone());
            parsed.document.record(JournalEntry::Attributes {
                target: element,
                // attributeName is the attribute's local name.
                // https://dom.spec.whatwg.org/#handle-attribute-changes
                name: name.local.as_str().to_owned(),
                namespace: name.ns.as_str().to_owned(),
                old_value: Some(value.clone()),
            });
        }
        removed
    };
    let (changed, previous) = match removed {
        Some((name, value)) => {
            let namespace = name.ns.as_str().to_owned();
            let changed = name.local.as_str().to_owned();
            // The nonce attribute change steps reset the cryptographic nonce
            // slot on removal (value `null`); only the null-namespace attribute
            // participates
            // (<https://html.spec.whatwg.org/multipage/urls-and-fetching.html#nonce-attributes>).
            if changed == "nonce" && namespace.is_empty() {
                world_rc.borrow_mut().state.nonce_slots.remove(&element);
            }
            let registry = world_rc.borrow().registry();
            registry
                .borrow_mut()
                .attributes
                .touch(element, &namespace, &changed, value.clone());
            registry
                .borrow_mut()
                .attributes
                .detach(element, &namespace, &changed);
            (changed, Some(value))
        }
        None => (local.to_owned(), None),
    };
    if namespace.is_empty() {
        after_attribute_change(ctx, element, &changed, previous.as_deref())?;
    }
    schedule_mutation_delivery(ctx)
}

/// One attribute on an element: its stored `(prefix, qualified name, value)`.
fn element_attribute(
    base: &blitz_dom::BaseDocument,
    id: crate::dom::tree::BlitzId,
    namespace: &str,
    local: &str,
) -> Option<(Option<String>, String, String)> {
    let attribute = base
        .get_node(id)?
        .data
        .downcast_element()?
        .attrs
        .iter()
        .find(|attribute| {
            attribute.name.ns.as_str() == namespace && attribute.name.local.as_str() == local
        })?;
    Some((
        attribute
            .name
            .prefix
            .as_ref()
            .map(markup5ever::Prefix::as_str)
            .filter(|prefix| !prefix.is_empty())
            .map(ToString::to_string),
        qualified_name(&attribute.name),
        attribute.value.clone(),
    ))
}

/// The value of the `(namespace, local)` attribute on `id`, if present.
fn attribute_ns_value(
    base: &blitz_dom::BaseDocument,
    id: crate::dom::tree::BlitzId,
    namespace: &str,
    local: &str,
) -> Option<String> {
    element_attribute(base, id, namespace, local).map(|(_, _, value)| value)
}

/// Sets the `local` content attribute to `value`, syncing the Attr registry
/// and mutation observers; the reflected-attribute write path behind
/// `host::reflect_set_string` and `write_class`
/// (<https://dom.spec.whatwg.org/#concept-element-attributes-set>).
pub(crate) fn set_attribute_sync(
    ctx: &Ctx<'_>,
    element: NodeId,
    local: &str,
    value: &str,
) -> Result<()> {
    let world_rc = world_for_node(ctx, element)?;
    let world = world_rc.borrow();
    let Some(mut parsed) = world.document_mut(element) else {
        return Ok(());
    };
    // Reuse the stored qualified name when an attribute with this local name
    // is already present: `Attributes::set` matches the full `QualName`, so a
    // fresh name would duplicate instead of replacing it.
    let (name, old_value) = {
        let base = &parsed.document.base;
        let Some(data) = base
            .get_node(element.node)
            .and_then(|node| node.data.downcast_element())
        else {
            return Err(Exception::throw_type(
                ctx,
                "attribute target is not an element",
            ));
        };
        // Match the qualified name, so `setAttribute("foo:bar")` updates the
        // existing `foo:bar` attribute instead of creating a second one.
        // https://dom.spec.whatwg.org/#concept-element-attributes-get-by-name
        let name = data
            .attrs
            .iter()
            .find(|attribute| qualified_name_eq(&attribute.name, local))
            .map_or_else(
                // setAttribute creates an attribute whose namespace is null.
                // https://dom.spec.whatwg.org/#concept-element-attributes-set-value
                || {
                    QualName::new(
                        None,
                        markup5ever::Namespace::from(""),
                        LocalName::from(local),
                    )
                },
                |attribute| attribute.name.clone(),
            );
        let old_value = crate::dom::tree::attr_by_qualified_name(base, element.node, local)
            .map(ToOwned::to_owned);
        (name, old_value)
    };
    let namespace = name.ns.as_str().to_owned();
    parsed
        .document
        .base
        .mutate()
        .set_attribute(element.node, name.clone(), value);
    // A plain write has no unpaired surrogate. A caller that still holds the
    // original units remembers them after this returns.
    parsed.document.remember_attribute(
        element.node,
        local,
        crate::dom_string::DomString::from(value),
    );
    parsed.document.record(JournalEntry::Attributes {
        target: element,
        // attributeName is the attribute's local name.
        // https://dom.spec.whatwg.org/#handle-attribute-changes
        name: name.local.as_str().to_owned(),
        namespace: namespace.clone(),
        old_value: old_value.clone(),
    });
    drop(parsed);
    drop(world);
    // The nonce content attribute lives in the null namespace: a namespaced
    // attribute that merely shares the local name must not feed the
    // cryptographic nonce slot
    // (<https://html.spec.whatwg.org/multipage/urls-and-fetching.html#nonce-attributes>).
    if local == "nonce" && namespace.is_empty() {
        // The content attribute feeds the cryptographic nonce slot, so a
        // later IDL read answers the attribute value until the IDL setter
        // overrides it
        // (<https://html.spec.whatwg.org/multipage/urls-and-fetching.html#nonce-attributes>).
        world_rc
            .borrow_mut()
            .state
            .nonce_slots
            .insert(element, value.to_owned());
    }
    touch_attr(ctx, element, &namespace, local, value)?;
    if namespace.is_empty() {
        after_attribute_change(ctx, element, local, old_value.as_deref())?;
    }
    schedule_mutation_delivery(ctx)
}

/// `setAttributeNode` / `setAttributeNodeNS`
/// (<https://dom.spec.whatwg.org/#concept-element-attributes-set>).
pub(crate) fn set_attribute_node<'js>(
    ctx: &Ctx<'js>,
    element: NodeId,
    attr: &Value<'js>,
) -> Result<Value<'js>> {
    let class = Class::<JsAttr>::from_js(ctx, attr.clone())
        .map_err(|_| Exception::throw_type(ctx, "argument is not an Attr"))?;
    let (id, scope) = {
        let attr = class.borrow();
        (attr.id, attr.scope.0)
    };
    let state = attr_state(ctx, scope, id)?;
    let owner = attr_owner(ctx, scope, id);
    if let Some(owner) = owner
        && owner != element
    {
        return Err(throw_dom(
            ctx,
            "InUseAttributeError",
            "attribute is already associated with another element",
        ));
    }
    let value = attr_value(ctx, scope, id)?;
    let previous = attached_attr_id(ctx, element, &state.namespace, &state.local)?;
    // Setting an attribute that is already attached here is a no-op that
    // returns the attribute itself
    // (<https://dom.spec.whatwg.org/#concept-element-attributes-set> step 4).
    if previous == Some(id) {
        return Ok(attr.clone());
    }
    let (previous, old_value) = {
        let world_rc = world_for_node(ctx, element)?;
        let world = world_rc.borrow();
        let mut parsed = world
            .document_mut(element)
            .ok_or_else(|| Exception::throw_type(ctx, "no document"))?;
        let old_value = attribute_ns_value(
            &parsed.document.base,
            element.node,
            &state.namespace,
            &state.local,
        );
        let registry = world.registry();
        let previous = registry
            .borrow_mut()
            .attributes
            .attach(id, &mut parsed.document.base, element, value)
            .map_err(|err| match err {
                AttrAttachError::Stale => Exception::throw_type(ctx, "stale attribute"),
                AttrAttachError::InUse => throw_dom(
                    ctx,
                    "InUseAttributeError",
                    "attribute is already associated with another element",
                ),
                // `attach` rejects a target with no element data; the
                // dispatcher only offers elements, so this is unreachable
                // through the DOM API.
                AttrAttachError::NotAnElement => throw_dom(
                    ctx,
                    "HierarchyRequestError",
                    "attribute target is not an element",
                ),
            })?;
        parsed.document.record(JournalEntry::Attributes {
            target: element,
            // attributeName is the attribute's local name.
            // https://dom.spec.whatwg.org/#handle-attribute-changes
            name: state.local.clone(),
            namespace: state.namespace.clone(),
            old_value: old_value.clone(),
        });
        (previous, old_value)
    };
    if state.namespace.is_empty() {
        after_attribute_change(ctx, element, &state.local, old_value.as_deref())?;
    }
    schedule_mutation_delivery(ctx)?;
    match previous {
        Some(previous) => attr_wrapper(ctx, previous),
        None => Ok(Value::new_null(ctx.clone())),
    }
}
