// https://dom.spec.whatwg.org/#xpath

use std::cell::Cell;

use blitz_dom::NodeData;
use rquickjs::class::{Trace, Tracer};
use rquickjs::prelude::This;
use rquickjs::{Class, Coerced, Ctx, Exception, FromJs, Result, Value};

use crate::dom::tree::NodeId;
use dom::xpath::{
    CompiledExpr, NsResolver, NullResolver, XPathError, XPathNode, XPathValue, to_boolean,
    to_number, to_string,
};

use super::host::NodeReference;
use super::{
    attached_attr_id, attr_owner, attr_state, attr_wrapper, throw_dom, world, world_for_node,
    wrap_node,
};

const ANY_TYPE: u16 = 0;
const NUMBER_TYPE: u16 = 1;
const STRING_TYPE: u16 = 2;
const BOOLEAN_TYPE: u16 = 3;
const UNORDERED_NODE_ITERATOR_TYPE: u16 = 4;
const ORDERED_NODE_ITERATOR_TYPE: u16 = 5;
const UNORDERED_NODE_SNAPSHOT_TYPE: u16 = 6;
const ORDERED_NODE_SNAPSHOT_TYPE: u16 = 7;
const ANY_UNORDERED_NODE_TYPE: u16 = 8;
const FIRST_ORDERED_NODE_TYPE: u16 = 9;

// https://dom.spec.whatwg.org/#interface-xpathresult
#[derive(rquickjs::JsLifetime)]
pub(crate) struct JsXPathResult {
    result_type: u16,
    number_value: f64,
    string_value: String,
    boolean_value: bool,
    nodes: Vec<XPathNode>,
    doc_id: u32,
    iterator_index: Cell<usize>,
    snapshot_generation: u64,
}

impl<'js> Trace<'js> for JsXPathResult {
    fn trace<'a>(&self, _tracer: Tracer<'a, 'js>) {}
}

include!(concat!(env!("OUT_DIR"), "/XPathResult.rs"));

impl JsXPathResult {
    fn is_iterator(&self) -> bool {
        matches!(
            self.result_type,
            UNORDERED_NODE_ITERATOR_TYPE | ORDERED_NODE_ITERATOR_TYPE
        )
    }

    fn is_snapshot(&self) -> bool {
        matches!(
            self.result_type,
            UNORDERED_NODE_SNAPSHOT_TYPE | ORDERED_NODE_SNAPSHOT_TYPE
        )
    }

    fn is_single_node(&self) -> bool {
        matches!(
            self.result_type,
            ANY_UNORDERED_NODE_TYPE | FIRST_ORDERED_NODE_TYPE
        )
    }

    fn current_generation(&self, ctx: &Ctx<'_>) -> u64 {
        let Ok(world_rc) = world(ctx) else {
            return self.snapshot_generation;
        };
        let borrowed = world_rc.borrow();
        borrowed
            .store()
            .get(self.doc_id)
            .map_or(self.snapshot_generation, |parsed| {
                parsed.document.side.mutation_generation
            })
    }

    fn is_invalidated(&self, ctx: &Ctx<'_>) -> bool {
        self.is_iterator() && self.current_generation(ctx) != self.snapshot_generation
    }
}

impl<'js> x_path_result_generated::XPathResult<'js> for JsXPathResult {
    // https://dom.spec.whatwg.org/#dom-xpathresult-resulttype
    fn get_result_type(&self, _ctx: &Ctx<'js>) -> Result<u16> {
        Ok(self.result_type)
    }

    // https://dom.spec.whatwg.org/#dom-xpathresult-numbervalue
    fn get_number_value(&self, ctx: &Ctx<'js>) -> Result<f64> {
        if self.result_type != NUMBER_TYPE {
            return Err(Exception::throw_type(ctx, "result is not a NUMBER_TYPE"));
        }
        Ok(self.number_value)
    }

    // https://dom.spec.whatwg.org/#dom-xpathresult-stringvalue
    fn get_string_value(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        if self.result_type != STRING_TYPE {
            return Err(Exception::throw_type(ctx, "result is not a STRING_TYPE"));
        }
        rquickjs::String::from_str(ctx.clone(), &self.string_value)
    }

    // https://dom.spec.whatwg.org/#dom-xpathresult-booleanvalue
    fn get_boolean_value(&self, ctx: &Ctx<'js>) -> Result<bool> {
        if self.result_type != BOOLEAN_TYPE {
            return Err(Exception::throw_type(ctx, "result is not a BOOLEAN_TYPE"));
        }
        Ok(self.boolean_value)
    }

    // https://dom.spec.whatwg.org/#dom-xpathresult-singlenodevalue
    fn get_single_node_value(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        if !self.is_single_node() {
            return Err(Exception::throw_type(
                ctx,
                "result is not a single-node XPathResult",
            ));
        }
        match self.nodes.first() {
            Some(node) => wrap_xpath_node(ctx, self.doc_id, node),
            None => Ok(Value::new_null(ctx.clone())),
        }
    }

    // https://dom.spec.whatwg.org/#dom-xpathresult-invaliditeratorstate
    fn get_invalid_iterator_state(&self, ctx: &Ctx<'js>) -> Result<bool> {
        Ok(self.is_invalidated(ctx))
    }

    // https://dom.spec.whatwg.org/#dom-xpathresult-snapshotlength
    fn get_snapshot_length(&self, ctx: &Ctx<'js>) -> Result<usize> {
        if !self.is_snapshot() {
            return Err(Exception::throw_type(
                ctx,
                "result is not a snapshot XPathResult",
            ));
        }
        Ok(self.nodes.len())
    }

    // https://dom.spec.whatwg.org/#dom-xpathresult-iteratenext
    fn iterate_next(&self, ctx: Ctx<'js>) -> Result<Value<'js>> {
        if !self.is_iterator() {
            return Err(Exception::throw_type(
                &ctx,
                "result is not an iterator XPathResult",
            ));
        }
        if self.is_invalidated(&ctx) {
            return Err(throw_dom(
                &ctx,
                "InvalidStateError",
                "the document has been mutated since the result was returned",
            ));
        }
        let idx = self.iterator_index.get();
        let Some(node) = self.nodes.get(idx) else {
            return Ok(Value::new_null(ctx));
        };
        self.iterator_index.set(idx + 1);
        wrap_xpath_node(&ctx, self.doc_id, node)
    }

    // https://dom.spec.whatwg.org/#dom-xpathresult-snapshotitem
    fn snapshot_item(&self, ctx: Ctx<'js>, index: u32) -> Result<Value<'js>> {
        if !self.is_snapshot() {
            return Err(Exception::throw_type(
                &ctx,
                "result is not a snapshot XPathResult",
            ));
        }
        let idx = usize::try_from(index).unwrap_or(usize::MAX);
        match self.nodes.get(idx) {
            Some(node) => wrap_xpath_node(&ctx, self.doc_id, node),
            None => Ok(Value::new_null(ctx)),
        }
    }
}

// https://dom.spec.whatwg.org/#interface-xpathexpression
#[derive(rquickjs::JsLifetime)]
pub(crate) struct JsXPathExpression {
    compiled: CompiledExpr,
}

impl<'js> Trace<'js> for JsXPathExpression {
    fn trace<'a>(&self, _tracer: Tracer<'a, 'js>) {}
}

include!(concat!(env!("OUT_DIR"), "/XPathExpression.rs"));

impl<'js> x_path_expression_generated::XPathExpression<'js> for JsXPathExpression {
    // https://dom.spec.whatwg.org/#dom-xpathexpression-evaluate
    fn evaluate(
        &self,
        ctx: Ctx<'js>,
        context_node: NodeReference,
        result_type: u16,
        result: Value<'js>,
    ) -> Result<Value<'js>> {
        evaluate_compiled(&ctx, &self.compiled, context_node, result_type, result)
    }
}

// https://dom.spec.whatwg.org/#interface-xpathevaluator
#[derive(rquickjs::JsLifetime)]
pub(crate) struct JsXPathEvaluator;

impl<'js> Trace<'js> for JsXPathEvaluator {
    fn trace<'a>(&self, _tracer: Tracer<'a, 'js>) {}
}

include!(concat!(env!("OUT_DIR"), "/XPathEvaluator.rs"));

impl<'js> x_path_evaluator_generated::XPathEvaluator<'js> for JsXPathEvaluator {
    // https://dom.spec.whatwg.org/#dom-xpathevaluator-xpathevaluator
    fn constructor(_ctx: &Ctx<'js>) -> Result<Self> {
        Ok(Self)
    }

    // https://dom.spec.whatwg.org/#dom-xpathevaluatorbase-createexpression
    fn create_expression(
        &self,
        ctx: Ctx<'js>,
        expression: rquickjs::String<'js>,
        resolver: Value<'js>,
    ) -> Result<Value<'js>> {
        create_expression(&ctx, expression, resolver)
    }

    // https://dom.spec.whatwg.org/#dom-xpathevaluatorbase-creatensresolver
    fn create_ns_resolver(&self, ctx: Ctx<'js>, node: NodeReference) -> Result<Value<'js>> {
        create_ns_resolver(&ctx, node)
    }

    // https://dom.spec.whatwg.org/#dom-xpathevaluatorbase-evaluate
    fn evaluate(
        &self,
        ctx: Ctx<'js>,
        expression: rquickjs::String<'js>,
        context_node: NodeReference,
        resolver: Value<'js>,
        result_type: u16,
        result: Value<'js>,
    ) -> Result<Value<'js>> {
        evaluate_expression(
            &ctx,
            expression,
            context_node,
            resolver,
            result_type,
            result,
        )
    }
}

struct JsNsResolver<'js> {
    ctx: Ctx<'js>,
    target: Value<'js>,
}

fn resolver_realm<'js>(ctx: &Ctx<'js>, target: &Value<'js>) -> Ctx<'js> {
    if let Some(func) = target.as_function()
        && let Ok(realm) = func.realm()
    {
        return realm;
    }
    if let Some(obj) = target.as_object()
        && let Some(proto) = obj.get_prototype()
        && let Ok(ctor) = proto.get::<_, Value>("constructor")
        && let Some(func) = ctor.as_function()
        && let Ok(realm) = func.realm()
    {
        return realm;
    }
    ctx.clone()
}

impl JsNsResolver<'_> {
    fn invoke(&self, prefix: &str) -> Result<Option<String>> {
        let value = if let Some(func) = self.target.as_function() {
            func.call::<_, Value>((prefix,))?
        } else if let Some(obj) = self.target.as_object() {
            let prop: Value = obj.get("lookupNamespaceURI")?;
            let Some(func) = prop.as_function() else {
                let realm = resolver_realm(&self.ctx, &self.target);
                return Err(Exception::throw_type(
                    &realm,
                    "XPathNSResolver.lookupNamespaceURI is not callable",
                ));
            };
            func.call::<_, Value>((This(obj.clone()), prefix))?
        } else {
            return Err(Exception::throw_type(
                &self.ctx,
                "XPathNSResolver must be an object or function",
            ));
        };
        if value.is_null() || value.is_undefined() {
            return Ok(None);
        }
        let coerced = Coerced::<rquickjs::String>::from_js(&self.ctx, value)?;
        Ok(Some(coerced.0.to_string()?))
    }
}

impl NsResolver for JsNsResolver<'_> {
    // https://webidl.spec.whatwg.org/#call-a-user-objects-operation
    fn lookup(&mut self, prefix: &str) -> std::result::Result<Option<String>, XPathError> {
        match self.invoke(prefix) {
            Ok(uri) => Ok(uri),
            Err(err) => {
                let realm = resolver_realm(&self.ctx, &self.target);
                if err.is_exception() {
                    let caught = self.ctx.catch();
                    let caught = if caught.is_undefined() {
                        realm.catch()
                    } else {
                        caught
                    };
                    super::report_exception_value(&realm, caught, 0, "");
                }
                Ok(None)
            }
        }
    }
}

fn compile_with_js_resolver<'js>(
    ctx: &Ctx<'js>,
    source: &str,
    resolver: Value<'js>,
) -> Result<CompiledExpr> {
    if resolver.is_null() || resolver.is_undefined() {
        return CompiledExpr::compile(source, &mut NullResolver)
            .map_err(|err| xpath_error_to_js(ctx, err));
    }
    if !resolver.is_function() && !resolver.is_object() {
        return Err(Exception::throw_type(
            ctx,
            "XPathNSResolver must be a function, object, or null",
        ));
    }
    let mut js_resolver = JsNsResolver {
        ctx: ctx.clone(),
        target: resolver,
    };
    CompiledExpr::compile(source, &mut js_resolver).map_err(|err| xpath_error_to_js(ctx, err))
}

fn xpath_error_to_js(ctx: &Ctx<'_>, err: XPathError) -> rquickjs::Error {
    match err {
        XPathError::Syntax(msg) => throw_dom(ctx, "SyntaxError", &msg),
        XPathError::Namespace(msg) => throw_dom(ctx, "NamespaceError", &msg),
        XPathError::Type(msg) => Exception::throw_type(ctx, &msg),
    }
}

pub(super) fn create_expression<'js>(
    ctx: &Ctx<'js>,
    expression: rquickjs::String<'js>,
    resolver: Value<'js>,
) -> Result<Value<'js>> {
    let source = expression.to_string()?;
    let compiled = compile_with_js_resolver(ctx, &source, resolver)?;
    Ok(Class::instance(ctx.clone(), JsXPathExpression { compiled })?.into_value())
}

// https://dom.spec.whatwg.org/#dom-xpathevaluatorbase-creatensresolver
pub(super) fn create_ns_resolver<'js>(ctx: &Ctx<'js>, node: NodeReference) -> Result<Value<'js>> {
    match node {
        NodeReference::Tree(id) => wrap_node(ctx, id),
        NodeReference::Attribute { id, .. } => attr_wrapper(ctx, id),
    }
}

pub(super) fn evaluate_expression<'js>(
    ctx: &Ctx<'js>,
    expression: rquickjs::String<'js>,
    context_node: NodeReference,
    resolver: Value<'js>,
    result_type: u16,
    result: Value<'js>,
) -> Result<Value<'js>> {
    let source = expression.to_string()?;
    let compiled = compile_with_js_resolver(ctx, &source, resolver)?;
    evaluate_compiled(ctx, &compiled, context_node, result_type, result)
}

fn validate_result_arg(ctx: &Ctx<'_>, result: &Value<'_>) -> Result<()> {
    if result.is_null() || result.is_undefined() {
        return Ok(());
    }
    if Class::<JsXPathResult>::from_value(result).is_err() {
        return Err(Exception::throw_type(
            ctx,
            "result argument must be an XPathResult or null",
        ));
    }
    Ok(())
}

fn context_node_to_xpath(ctx: &Ctx<'_>, context_node: NodeReference) -> Result<(u32, XPathNode)> {
    match context_node {
        NodeReference::Tree(id) => {
            let owner = world_for_node(ctx, id)?;
            let is_doctype = owner.borrow().document(id).is_some_and(|parsed| {
                parsed
                    .document
                    .base
                    .get_node(id.node)
                    .is_some_and(|n| matches!(n.data, NodeData::Doctype { .. }))
            });
            if is_doctype {
                return Err(throw_dom(
                    ctx,
                    "NotSupportedError",
                    "DocumentType is not a valid XPath context node",
                ));
            }
            Ok((id.document, XPathNode::Tree(id.node)))
        }
        NodeReference::Attribute { scope, id: attr_id } => {
            let Some(owner_node) = attr_owner(ctx, scope, attr_id) else {
                return Err(throw_dom(
                    ctx,
                    "NotSupportedError",
                    "detached Attr cannot be evaluated as XPath context node",
                ));
            };
            let state = attr_state(ctx, scope, attr_id)?;
            let owner = world_for_node(ctx, owner_node)?;
            let attr_index = owner
                .borrow()
                .document(owner_node)
                .and_then(|parsed| {
                    let el = parsed
                        .document
                        .base
                        .get_node(owner_node.node)?
                        .data
                        .downcast_element()?;
                    el.attrs
                        .iter()
                        .position(|a| *a.name.ns == state.namespace && *a.name.local == state.local)
                })
                .unwrap_or(0);
            Ok((
                owner_node.document,
                XPathNode::Attr {
                    owner: owner_node.node,
                    attr_index,
                    ns: state.namespace,
                    local: state.local,
                    prefix: state.prefix,
                    value: state.value,
                },
            ))
        }
    }
}

fn evaluate_compiled<'js>(
    ctx: &Ctx<'js>,
    compiled: &CompiledExpr,
    context_node: NodeReference,
    requested_type: u16,
    result: Value<'js>,
) -> Result<Value<'js>> {
    validate_result_arg(ctx, &result)?;
    if requested_type > FIRST_ORDERED_NODE_TYPE {
        return Err(throw_dom(
            ctx,
            "NotSupportedError",
            "unsupported XPathResult type",
        ));
    }
    let (doc_id, xpath_node) = context_node_to_xpath(ctx, context_node)?;
    let world_rc = match context_node {
        NodeReference::Tree(id) => world_for_node(ctx, id)?,
        NodeReference::Attribute { scope, id } => match attr_owner(ctx, scope, id) {
            Some(owner) => world_for_node(ctx, owner)?,
            None => world(ctx)?,
        },
    };
    let borrowed = world_rc.borrow();
    let store = borrowed.store();
    let Some(parsed) = store.get(doc_id) else {
        return Err(throw_dom(ctx, "InvalidStateError", "stale document"));
    };
    let is_html = !parsed.xml_document;
    let snapshot_generation = parsed.document.side.mutation_generation;
    let value = compiled
        .evaluate(&parsed.document.base, is_html, xpath_node)
        .map_err(|err| xpath_error_to_js(ctx, err))?;
    let js_result = build_result(
        ctx,
        &parsed.document.base,
        doc_id,
        snapshot_generation,
        requested_type,
        value,
    )?;
    drop(store);
    drop(borrowed);
    Ok(Class::instance(ctx.clone(), js_result)?.into_value())
}

fn build_result(
    ctx: &Ctx<'_>,
    base: &blitz_dom::BaseDocument,
    doc_id: u32,
    snapshot_generation: u64,
    requested_type: u16,
    value: XPathValue,
) -> Result<JsXPathResult> {
    let resolved_type = if requested_type == ANY_TYPE {
        match &value {
            XPathValue::Number(_) => NUMBER_TYPE,
            XPathValue::String(_) => STRING_TYPE,
            XPathValue::Boolean(_) => BOOLEAN_TYPE,
            XPathValue::NodeSet(_) => UNORDERED_NODE_ITERATOR_TYPE,
        }
    } else {
        requested_type
    };
    let mut out = JsXPathResult {
        result_type: resolved_type,
        number_value: f64::NAN,
        string_value: String::new(),
        boolean_value: false,
        nodes: Vec::new(),
        doc_id,
        iterator_index: Cell::new(0),
        snapshot_generation,
    };
    match resolved_type {
        NUMBER_TYPE => out.number_value = to_number(base, &value),
        STRING_TYPE => out.string_value = to_string(base, &value),
        BOOLEAN_TYPE => out.boolean_value = to_boolean(&value),
        UNORDERED_NODE_ITERATOR_TYPE
        | ORDERED_NODE_ITERATOR_TYPE
        | UNORDERED_NODE_SNAPSHOT_TYPE
        | ORDERED_NODE_SNAPSHOT_TYPE
        | ANY_UNORDERED_NODE_TYPE
        | FIRST_ORDERED_NODE_TYPE => {
            let XPathValue::NodeSet(nodes) = value else {
                return Err(Exception::throw_type(
                    ctx,
                    "XPath expression did not evaluate to a node-set",
                ));
            };
            out.nodes = nodes;
        }
        _ => {
            return Err(throw_dom(
                ctx,
                "NotSupportedError",
                "unsupported XPathResult type",
            ));
        }
    }
    Ok(out)
}

fn wrap_xpath_node<'js>(ctx: &Ctx<'js>, doc_id: u32, node: &XPathNode) -> Result<Value<'js>> {
    match node {
        XPathNode::Tree(id) => wrap_node(
            ctx,
            NodeId {
                document: doc_id,
                node: *id,
            },
        ),
        XPathNode::Attr {
            owner, ns, local, ..
        } => {
            let owner_id = NodeId {
                document: doc_id,
                node: *owner,
            };
            match attached_attr_id(ctx, owner_id, ns, local)? {
                Some(id) => attr_wrapper(ctx, id),
                None => Ok(Value::new_null(ctx.clone())),
            }
        }
    }
}
