// https://dom.spec.whatwg.org/#ranges

use std::cell::{Cell, RefCell};
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::rc::{Rc, Weak};

use rquickjs::class::{Trace, Tracer};
use rquickjs::{Class, Ctx, Exception, FromJs, Result, Value};

use super::host::{NodeReference, instance_proto};
use super::node::core::{adopt_node, insert_tree_node, insertion_tree_nodes, split_text_node};
use super::node::document::{
    fragment_base_url, html_fragment_context, parse_html_fragment_snapshots,
};
use super::{
    attr_wrapper, character_data, fixup_focus_after_removal, realm_registry,
    replace_character_data, required_node, schedule_mutation_delivery, set_character_data,
    throw_dom, throw_dom_error, world, world_for_node, wrap_node,
};
use crate::dom::adoption::{ImportSnapshot, clone_within_document, materialize_children};
use crate::dom::insertion::{
    iframe_containers_in_subtrees, place_journaled, replace_all_journaled, unlink_journaled,
};
use crate::dom::range as ranges;
use crate::dom::range::{BoundaryPoint, RangeState};
use crate::dom::tree::{NodeId, child_ids, sibling};
use crate::dom_string::DomString;
use crate::js::world::World;

// https://dom.spec.whatwg.org/#interface-range
#[derive(Trace, rquickjs::JsLifetime)]
#[rquickjs::class(rename = "Range")]
pub(crate) struct JsRange {
    registration: Registration,
    owner: Owner,
}

// https://dom.spec.whatwg.org/#concept-range-bp-set
struct Registration {
    document: Cell<u32>,
    slot: Cell<usize>,
}

impl<'js> Trace<'js> for Registration {
    fn trace<'a>(&self, _tracer: Tracer<'a, 'js>) {}
}

struct Owner(Weak<RefCell<World>>);

impl<'js> Trace<'js> for Owner {
    fn trace<'a>(&self, _tracer: Tracer<'a, 'js>) {}
}

impl Drop for JsRange {
    fn drop(&mut self) {
        let Some(world) = self.owner.0.upgrade() else {
            return;
        };
        let Ok(world) = world.try_borrow() else {
            return;
        };
        world.try_free_range(
            self.registration.document.get(),
            self.registration.slot.get(),
        );
    }
}

// https://dom.spec.whatwg.org/#interface-staticrange
#[derive(rquickjs::JsLifetime)]
#[rquickjs::class(rename = "StaticRange")]
pub(crate) struct JsStaticRange {
    start: PointArg,
    end: PointArg,
}

impl<'js> Trace<'js> for JsStaticRange {
    fn trace<'a>(&self, _tracer: Tracer<'a, 'js>) {}
}

impl super::host::SharedClass for JsStaticRange {
    // https://dom.spec.whatwg.org/#interface-staticrange
    fn require_interface(&self, ctx: &Ctx<'_>, interface: &str) -> Result<()> {
        if matches!(interface, "StaticRange" | "AbstractRange") {
            Ok(())
        } else {
            Err(Exception::throw_type(ctx, "incompatible receiver"))
        }
    }
}

impl<'js> abstract_range_generated::AbstractRange<'js> for JsStaticRange {
    // https://dom.spec.whatwg.org/#dom-abstractrange-startcontainer
    fn get_start_container(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        match self.start {
            PointArg::Tree(point) => wrap_node(ctx, point.node),
            PointArg::Attribute(attr) => attr_wrapper(ctx, attr.id),
        }
    }

    // https://dom.spec.whatwg.org/#dom-abstractrange-startoffset
    fn get_start_offset(&self, _ctx: &Ctx<'js>) -> Result<usize> {
        Ok(match self.start {
            PointArg::Tree(point) => usize::try_from(point.offset).unwrap_or(usize::MAX),
            PointArg::Attribute(_) => 0,
        })
    }

    // https://dom.spec.whatwg.org/#dom-abstractrange-endcontainer
    fn get_end_container(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        match self.end {
            PointArg::Tree(point) => wrap_node(ctx, point.node),
            PointArg::Attribute(attr) => attr_wrapper(ctx, attr.id),
        }
    }

    // https://dom.spec.whatwg.org/#dom-abstractrange-endoffset
    fn get_end_offset(&self, _ctx: &Ctx<'js>) -> Result<usize> {
        Ok(match self.end {
            PointArg::Tree(point) => usize::try_from(point.offset).unwrap_or(usize::MAX),
            PointArg::Attribute(_) => 0,
        })
    }

    // https://dom.spec.whatwg.org/#dom-abstractrange-collapsed
    fn get_collapsed(&self, _ctx: &Ctx<'js>) -> Result<bool> {
        Ok(match (self.start, self.end) {
            (PointArg::Tree(start), PointArg::Tree(end)) => start == end,
            (PointArg::Attribute(start), PointArg::Attribute(end)) => start == end,
            (PointArg::Tree(_), PointArg::Attribute(_))
            | (PointArg::Attribute(_), PointArg::Tree(_)) => false,
        })
    }
}

include!(concat!(env!("OUT_DIR"), "/Range.rs"));
include!(concat!(env!("OUT_DIR"), "/AbstractRange.rs"));

pub(crate) fn install(ctx: &Ctx<'_>) -> Result<()> {
    range_generated::install(ctx)?;
    abstract_range_generated::install(ctx)?;
    Ok(())
}

impl super::host::SharedClass for JsRange {
    // https://dom.spec.whatwg.org/#interface-range
    fn require_interface(&self, ctx: &Ctx<'_>, interface: &str) -> Result<()> {
        if matches!(interface, "Range" | "AbstractRange") {
            Ok(())
        } else {
            Err(Exception::throw_type(ctx, "incompatible receiver"))
        }
    }
}

pub(crate) fn new_range<'js>(ctx: &Ctx<'js>, root: NodeId) -> Result<Value<'js>> {
    let point = BoundaryPoint {
        node: root,
        offset: 0,
    };
    let (document, slot) = alloc_range(ctx, root.document, RangeState::collapsed(point))?;
    wrap_range(ctx, document, slot)
}

// https://dom.spec.whatwg.org/#dom-staticrange-staticrange
pub(crate) fn new_static_range<'js>(
    ctx: &Ctx<'js>,
    init: Option<Value<'js>>,
) -> Result<Value<'js>> {
    let init = init.unwrap_or_else(|| Value::new_undefined(ctx.clone()));
    let object = init
        .clone()
        .into_object()
        .ok_or_else(|| Exception::throw_type(ctx, "StaticRangeInit is not an object"))?;
    let required = |name: &str| -> Result<Value<'js>> {
        let value: Value<'js> = object.get(name)?;
        if value.is_undefined() {
            return Err(Exception::throw_type(
                ctx,
                &format!("required member {name} is undefined"),
            ));
        }
        Ok(value)
    };
    let start_container = super::host::node_argument(ctx, &required("startContainer")?)?;
    let start_offset = super::WebIdlUnsignedLong::from_js(ctx, required("startOffset")?)?.0;
    let end_container = super::host::node_argument(ctx, &required("endContainer")?)?;
    let end_offset = super::WebIdlUnsignedLong::from_js(ctx, required("endOffset")?)?.0;
    let range = JsStaticRange {
        start: static_point(ctx, start_container, start_offset)?,
        end: static_point(ctx, end_container, end_offset)?,
    };
    let constructor: rquickjs::Object = ctx.globals().get("StaticRange")?;
    let prototype: rquickjs::Object = constructor.get("prototype")?;
    Ok(Class::into_value(instance_proto(range, prototype)?))
}

// https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-range-createcontextualfragment
fn synthetic_body_name() -> markup5ever::QualName {
    markup5ever::QualName::new(
        None,
        crate::dom::tree::html_namespace(),
        markup5ever::LocalName::from("body"),
    )
}

// https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-range-createcontextualfragment
fn contextual_fragment_context(
    ctx: &Ctx<'_>,
    start: NodeId,
    from_attribute: bool,
) -> Result<(NodeId, markup5ever::QualName, &'static str)> {
    let world_rc = world(ctx)?;
    let world = world_rc.borrow();
    let store = world.store();
    let Some(parsed) = store.get(start.document) else {
        return Err(Exception::throw_type(ctx, "stale range"));
    };
    let content_type = parsed.content_type;
    let base = &parsed.document.base;
    let name = if from_attribute {
        None
    } else {
        match base.get_node(start.node).map(|node| &node.data) {
            Some(blitz_dom::NodeData::Element(element)) => Some((start, element.name.clone())),
            Some(
                blitz_dom::NodeData::Text(_)
                | blitz_dom::NodeData::CDataSection { .. }
                | blitz_dom::NodeData::Comment { .. },
            ) => parent_of_raw(base, start).and_then(|parent| {
                match base.get_node(parent.node).map(|node| &node.data) {
                    Some(blitz_dom::NodeData::Element(element)) => {
                        Some((parent, element.name.clone()))
                    }
                    _ => None,
                }
            }),
            _ => None,
        }
    };
    let (context, name) = name.unwrap_or((start, synthetic_body_name()));
    Ok((context, name, content_type))
}

// https://html.spec.whatwg.org/multipage/parsing.html#html-fragment-parsing-algorithm
fn html_contextual_fragment<'js>(
    ctx: &Ctx<'js>,
    start: NodeId,
    markup: &str,
    name: &markup5ever::QualName,
) -> Result<Value<'js>> {
    let context = if name.local.as_str() == "html" && name.ns == crate::dom::tree::html_namespace()
    {
        "body".to_owned()
    } else {
        html_fragment_context(name)
    };
    let (base_url, font_ctx) = fragment_base_url(ctx, start);
    let snapshots = parse_html_fragment_snapshots(markup, &context, &base_url, font_ctx);
    materialize_fragment_value(ctx, start, &snapshots)
}

fn materialize_fragment_value<'js>(
    ctx: &Ctx<'js>,
    start: NodeId,
    snapshots: &[ImportSnapshot],
) -> Result<Value<'js>> {
    let owner = world_for_node(ctx, start)?;
    let world = owner.borrow();
    let Some(mut parsed) = world.document_mut(start) else {
        return Err(Exception::throw_type(ctx, "stale range"));
    };
    let fragment = materialize_children(&mut parsed.document, start.document, snapshots)
        .map_err(|error| throw_dom_error(ctx, error))?;
    drop(parsed);
    drop(world);
    wrap_node(ctx, fragment)
}

fn escape_xml_attribute(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            '\t' => escaped.push_str("&#9;"),
            '\n' => escaped.push_str("&#10;"),
            '\r' => escaped.push_str("&#13;"),
            character => escaped.push(character),
        }
    }
    escaped
}

// https://html.spec.whatwg.org/multipage/xhtml.html#parsing-xml-fragments
fn xml_scope(
    document: &crate::documents::BlitzDocument,
    context: NodeId,
    default_override: Option<&str>,
) -> (Option<String>, BTreeMap<String, String>) {
    const XML_NAMESPACE: &str = "http://www.w3.org/XML/1998/namespace";
    const XMLNS_NAMESPACE: &str = "http://www.w3.org/2000/xmlns/";
    let mut prefixes = BTreeMap::new();
    prefixes.insert("xml".to_owned(), XML_NAMESPACE.to_owned());
    let mut default = default_override.map(str::to_owned);
    let mut default_seen = default.is_some();
    let mut current = Some(context);
    while let Some(id) = current {
        let Some(node) = document.base.get_node(id.node) else {
            break;
        };
        if let Some(element) = node.data.downcast_element() {
            for attribute in element.attrs.iter() {
                if attribute.name.ns.as_str() != XMLNS_NAMESPACE {
                    continue;
                }
                match attribute
                    .name
                    .prefix
                    .as_ref()
                    .map(markup5ever::Prefix::as_str)
                {
                    Some("xmlns") => {
                        if attribute.name.local.as_str() != "xmlns" {
                            prefixes
                                .entry(attribute.name.local.to_string())
                                .or_insert_with(|| attribute.value.clone());
                        }
                    }
                    None if attribute.name.local.as_str() == "xmlns" && !default_seen => {
                        default_seen = true;
                        if !attribute.value.is_empty() {
                            default = Some(attribute.value.clone());
                        }
                    }
                    _ => {}
                }
            }
        }
        current = node.parent.map(|parent| NodeId {
            document: id.document,
            node: parent,
        });
    }
    (default, prefixes)
}

fn xml_fragment_source(
    default: Option<&str>,
    prefixes: &BTreeMap<String, String>,
    markup: &str,
) -> String {
    const WRAPPER: &str = "tinybrowser-xml-fragment";
    let mut source = String::with_capacity(markup.len() + 128);
    source.push('<');
    source.push_str(WRAPPER);
    for (prefix, uri) in prefixes {
        source.push_str(" xmlns:");
        source.push_str(prefix);
        source.push_str("=\"");
        source.push_str(&escape_xml_attribute(uri));
        source.push('"');
    }
    if let Some(uri) = default {
        source.push_str(" xmlns=\"");
        source.push_str(&escape_xml_attribute(uri));
        source.push('"');
    }
    source.push('>');
    source.push_str(markup);
    source.push_str("</");
    source.push_str(WRAPPER);
    source.push('>');
    source
}

// https://html.spec.whatwg.org/multipage/xhtml.html#parsing-xml-fragments
fn xml_contextual_fragment<'js>(
    ctx: &Ctx<'js>,
    start: NodeId,
    name: &markup5ever::QualName,
    markup: &str,
) -> Result<Value<'js>> {
    let (default, prefixes, base_url, font_ctx) = {
        let world_rc = world(ctx)?;
        let world = world_rc.borrow();
        let Some(parsed) = world.document(start) else {
            return Err(Exception::throw_type(ctx, "stale range"));
        };
        let default_override =
            if name.local.as_str() == "html" && name.ns == crate::dom::tree::html_namespace() {
                Some("http://www.w3.org/1999/xhtml")
            } else {
                None
            };
        let scope = xml_scope(&parsed.document, start, default_override);
        let base_url = parsed.document.base.base_url().as_str().to_owned();
        let font_ctx = world.runtime.font_ctx.clone();
        (scope.0, scope.1, base_url, font_ctx)
    };
    let source = xml_fragment_source(default.as_deref(), &prefixes, markup);
    let mut base = blitz_dom::BaseDocument::new(blitz_dom::DocumentConfig {
        base_url: Some(base_url),
        font_ctx: Some(font_ctx),
        ua_stylesheets: Vec::new(),
        ..blitz_dom::DocumentConfig::default()
    });
    {
        let mut mutator = base.mutate();
        blitz_html::DocumentHtmlParser::parse_xml_into_mutator(&mut mutator, &source);
    }
    let mut scratch = crate::documents::BlitzDocument::from_base(base);
    if !scratch.base.take_parse_errors().is_empty() {
        return Err(throw_dom(
            ctx,
            "SyntaxError",
            "markup is not well-formed XML",
        ));
    }
    let children = {
        let root = scratch.base.root_node().id;
        let top = scratch
            .base
            .get_node(root)
            .map_or_default(|node| node.children.clone());
        if top.len() != 1 {
            return Err(throw_dom(
                ctx,
                "SyntaxError",
                "markup is not well-formed XML",
            ));
        }
        let wrapper = NodeId {
            document: start.document,
            node: top[0],
        };
        scratch
            .base
            .get_node(wrapper.node)
            .map_or_default(|node| node.children.clone())
            .into_iter()
            .map(|child| NodeId {
                document: start.document,
                node: child,
            })
            .collect::<Vec<_>>()
    };
    let snapshots = children
        .into_iter()
        .map(|child| {
            crate::dom::adoption::import_snapshot(&scratch, child, true)
                .ok_or_else(|| throw_dom(ctx, "SyntaxError", "markup is not well-formed XML"))
        })
        .collect::<Result<Vec<ImportSnapshot>>>()?;
    materialize_fragment_value(ctx, start, &snapshots)
}

// https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-range-createcontextualfragment
fn create_contextual_fragment<'js>(
    ctx: &Ctx<'js>,
    start: NodeId,
    from_attribute: bool,
    markup: &str,
) -> Result<Value<'js>> {
    let (context, name, content_type) = contextual_fragment_context(ctx, start, from_attribute)?;
    if content_type == "text/html" {
        html_contextual_fragment(ctx, context, markup, &name)
    } else {
        xml_contextual_fragment(ctx, start, &name, markup)
    }
}

// https://dom.spec.whatwg.org/#dom-document-createrange
pub(crate) fn create_range_value<'js>(ctx: &Ctx<'js>, document: NodeId) -> Result<Value<'js>> {
    let point = BoundaryPoint {
        node: document,
        offset: 0,
    };
    let (store, slot) = alloc_range(ctx, document.document, RangeState::collapsed(point))?;
    wrap_range(ctx, store, slot)
}

// https://webidl.spec.whatwg.org/#internally-create-a-new-object-implementing-the-interface
pub(crate) fn wrap_range<'js>(ctx: &Ctx<'js>, document: u32, slot: usize) -> Result<Value<'js>> {
    let world = world(ctx)?;
    let proto = world
        .borrow()
        .brand("Range")
        .ok_or_else(|| Exception::throw_internal(ctx, "missing Range prototype"))?;
    let proto = proto.restore(ctx)?;
    Ok(Class::into_value(instance_proto(
        JsRange {
            registration: Registration {
                document: Cell::new(document),
                slot: Cell::new(slot),
            },
            owner: Owner(Rc::downgrade(&world)),
        },
        proto,
    )?))
}

impl JsRange {
    fn load(&self, ctx: &Ctx<'_>) -> Result<RangeState> {
        let world_rc = world(ctx)?;
        let world = world_rc.borrow();
        world
            .store()
            .get(self.registration.document.get())
            .and_then(|parsed| {
                parsed
                    .document
                    .side
                    .ranges
                    .get(self.registration.slot.get())
            })
            .ok_or_else(|| Exception::throw_type(ctx, "stale range"))
    }

    // https://dom.spec.whatwg.org/#concept-range-bp-set
    fn save(&self, ctx: &Ctx<'_>, state: RangeState) -> Result<()> {
        let owner = self.registration.document.get();
        let slot = self.registration.slot.get();
        let target = state.document();
        let world_rc = world(ctx)?;
        let world = world_rc.borrow();
        if target == owner {
            let Some(mut parsed) = world.document_mut_by_id(owner) else {
                return Err(Exception::throw_type(ctx, "stale range"));
            };
            if !parsed.document.side.ranges.set(slot, state) {
                return Err(Exception::throw_type(ctx, "stale range"));
            }
            return Ok(());
        }
        {
            let Some(mut parsed) = world.document_mut_by_id(owner) else {
                return Err(Exception::throw_type(ctx, "stale range"));
            };
            parsed.document.side.ranges.free(slot);
        }
        let fresh = {
            let Some(mut parsed) = world.document_mut_by_id(target) else {
                return Err(Exception::throw_type(ctx, "stale range"));
            };
            parsed.document.side.ranges.alloc(state)
        };
        self.registration.document.set(target);
        self.registration.slot.set(fresh);
        Ok(())
    }
}

fn alloc_range(ctx: &Ctx<'_>, document: u32, state: RangeState) -> Result<(u32, usize)> {
    let world_rc = world(ctx)?;
    let world = world_rc.borrow();
    let Some(mut parsed) = world.document_mut_by_id(document) else {
        return Err(Exception::throw_type(ctx, "stale range"));
    };
    let slot = parsed.document.side.ranges.alloc(state);
    Ok((parsed.id, slot))
}

// https://dom.spec.whatwg.org/#concept-range-bp
fn tree_node(ctx: &Ctx<'_>, node: NodeReference) -> Result<NodeId> {
    node.tree()
        .ok_or_else(|| throw_dom(ctx, "InvalidNodeTypeError", "the node has no parent"))
}

#[derive(Clone, Copy)]
enum PointArg {
    Tree(BoundaryPoint),
    Attribute(ranges::AttrPoint),
}

// https://dom.spec.whatwg.org/#concept-range-bp-set
fn check_point(ctx: &Ctx<'_>, node: NodeReference, offset: u32) -> Result<PointArg> {
    match node {
        NodeReference::Attribute { scope, id } => {
            // https://dom.spec.whatwg.org/#concept-node-length
            if offset != 0 {
                return Err(throw_dom(
                    ctx,
                    "IndexSizeError",
                    "an attribute's length is zero",
                ));
            }
            Ok(PointArg::Attribute(ranges::AttrPoint { scope, id }))
        }
        NodeReference::Tree(id) => {
            let point = read_document(ctx, id.document, |base| {
                if ranges::is_doctype(base, id) {
                    return Err(throw_dom(
                        ctx,
                        "InvalidNodeTypeError",
                        "a doctype cannot hold a range boundary",
                    ));
                }
                if offset > ranges::node_length(base, id) {
                    return Err(throw_dom(
                        ctx,
                        "IndexSizeError",
                        "offset is outside the node",
                    ));
                }
                Ok(BoundaryPoint { node: id, offset })
            })?;
            Ok(PointArg::Tree(point))
        }
    }
}

// https://dom.spec.whatwg.org/#interface-staticrange
fn static_point(ctx: &Ctx<'_>, node: NodeReference, offset: u32) -> Result<PointArg> {
    match node {
        NodeReference::Attribute { .. } => Err(throw_dom(
            ctx,
            "InvalidNodeTypeError",
            "an attribute cannot hold a static range boundary",
        )),
        NodeReference::Tree(id) => read_document(ctx, id.document, |base| {
            if ranges::is_doctype(base, id) {
                return Err(throw_dom(
                    ctx,
                    "InvalidNodeTypeError",
                    "a doctype cannot hold a static range boundary",
                ));
            }
            Ok(PointArg::Tree(BoundaryPoint { node: id, offset }))
        }),
    }
}

fn read_document<R>(
    ctx: &Ctx<'_>,
    document: u32,
    read: impl FnOnce(&blitz_dom::BaseDocument) -> Result<R>,
) -> Result<R> {
    let world_rc = world(ctx)?;
    let world = world_rc.borrow();
    let store = world.store();
    let Some(parsed) = store.get(document) else {
        return Err(Exception::throw_type(ctx, "stale range"));
    };
    read(&parsed.document.base)
}

fn same_tree(ctx: &Ctx<'_>, a: NodeId, b: NodeId) -> Result<bool> {
    if a.document != b.document {
        return Ok(false);
    }
    read_document(ctx, a.document, |base| Ok(ranges::same_root(base, a, b)))
}

// https://dom.spec.whatwg.org/#concept-range-set-the-start
fn set_start_point(state: &mut RangeState, base: &blitz_dom::BaseDocument, point: BoundaryPoint) {
    // https://dom.spec.whatwg.org/#concept-range-bp-set
    let left_attribute = state.attr.take().is_some();
    if left_attribute
        || !ranges::same_root(base, state.start.node, point.node)
        || ranges::position(base, point.node.document, point, state.end) == Ordering::Greater
    {
        state.end = point;
    }
    state.start = point;
}

// https://dom.spec.whatwg.org/#concept-range-set-the-end
fn set_end_point(state: &mut RangeState, base: &blitz_dom::BaseDocument, point: BoundaryPoint) {
    let left_attribute = state.attr.take().is_some();
    if left_attribute
        || !ranges::same_root(base, state.end.node, point.node)
        || ranges::position(base, point.node.document, point, state.start) == Ordering::Less
    {
        state.start = point;
    }
    state.end = point;
}

// https://dom.spec.whatwg.org/#concept-range-bp-set
fn apply_start(ctx: &Ctx<'_>, state: &mut RangeState, arg: PointArg) -> Result<()> {
    match arg {
        PointArg::Attribute(attr) => {
            *state = RangeState::attribute(attr);
            Ok(())
        }
        PointArg::Tree(point) => read_document(ctx, point.node.document, |base| {
            set_start_point(state, base, point);
            Ok(())
        }),
    }
}

fn apply_end(ctx: &Ctx<'_>, state: &mut RangeState, arg: PointArg) -> Result<()> {
    match arg {
        PointArg::Attribute(attr) => {
            *state = RangeState::attribute(attr);
            Ok(())
        }
        PointArg::Tree(point) => read_document(ctx, point.node.document, |base| {
            set_end_point(state, base, point);
            Ok(())
        }),
    }
}

fn child_index_of(
    ctx: &Ctx<'_>,
    base: &blitz_dom::BaseDocument,
    parent: NodeId,
    child: NodeId,
) -> Result<u32> {
    child_ids(base, parent.document, parent.node)
        .iter()
        .position(|sibling| *sibling == child)
        .and_then(|index| u32::try_from(index).ok())
        .ok_or_else(|| Exception::throw_type(ctx, "stale range"))
}

// https://dom.spec.whatwg.org/#concept-range-insert
fn insert_node_target(
    ctx: &Ctx<'_>,
    state: &RangeState,
    arg: NodeId,
) -> Result<(NodeId, Option<NodeId>)> {
    read_document(ctx, state.start.node.document, |base| {
        let start_kind = base.get_node(state.start.node.node).map(|node| &node.data);
        if matches!(
            start_kind,
            Some(
                blitz_dom::NodeData::ProcessingInstruction { .. }
                    | blitz_dom::NodeData::Comment { .. }
            )
        ) {
            return Err(throw_dom(
                ctx,
                "HierarchyRequestError",
                "a range starting in a PI or comment takes no nodes",
            ));
        }
        if ranges::is_text(base, state.start.node)
            && parent_of_raw(base, state.start.node).is_none()
        {
            return Err(throw_dom(
                ctx,
                "HierarchyRequestError",
                "a range starting in detached text takes no nodes",
            ));
        }
        if arg == state.start.node {
            return Err(throw_dom(
                ctx,
                "HierarchyRequestError",
                "a range cannot be inserted into itself",
            ));
        }
        if ranges::is_text(base, state.start.node) {
            let reference = state.start.node;
            let parent = parent_of(ctx, base, reference)?;
            Ok((parent, Some(reference)))
        } else {
            let children = child_ids(base, state.start.node.document, state.start.node.node);
            let reference = children.get(state.start.offset as usize).copied();
            Ok((state.start.node, reference))
        }
    })
}

fn bound(offset: u32, length: usize) -> usize {
    (offset as usize).min(length)
}

// https://dom.spec.whatwg.org/#dom-range-deletecontents
fn collapse_point(base: &blitz_dom::BaseDocument, state: &RangeState) -> (NodeId, u32) {
    if ranges::is_inclusive_ancestor(base, state.start.node, state.end.node) {
        return (state.start.node, state.start.offset);
    }
    let mut reference = state.start.node;
    loop {
        let Some(parent) = base
            .get_node(reference.node)
            .and_then(|node| node.parent)
            .map(|parent| NodeId {
                document: reference.document,
                node: parent,
            })
        else {
            return (state.start.node, state.start.offset);
        };
        if ranges::is_inclusive_ancestor(base, parent, state.end.node) {
            let index = child_ids(base, parent.document, parent.node)
                .iter()
                .position(|sibling| *sibling == reference)
                .and_then(|index| u32::try_from(index).ok())
                .unwrap_or(0);
            return (parent, index.saturating_add(1));
        }
        reference = parent;
    }
}

fn collect_contained(
    base: &blitz_dom::BaseDocument,
    document: u32,
    state: &RangeState,
    ancestor: NodeId,
    out: &mut Vec<NodeId>,
) {
    for child in child_ids(base, document, ancestor.node) {
        if ranges::is_contained(base, document, state, child) {
            out.push(child);
        } else if ranges::is_partially_contained(base, state, child) {
            collect_contained(base, document, state, child, out);
        }
    }
}

fn create_fragment_in(ctx: &Ctx<'_>, document: u32) -> Result<NodeId> {
    let world_rc = world(ctx)?;
    let world = world_rc.borrow();
    let Some(mut parsed) = world.document_mut_by_id(document) else {
        return Err(Exception::throw_type(ctx, "stale range"));
    };
    let node = parsed.document.create_fragment();
    Ok(NodeId {
        document: parsed.id,
        node,
    })
}

fn move_nodes_out(ctx: &Ctx<'_>, nodes: &[NodeId], fragment: Option<NodeId>) -> Result<()> {
    if nodes.is_empty() {
        return Ok(());
    }
    let world_rc = world(ctx)?;
    let mut iframes = Vec::new();
    for node in nodes {
        let world = world_rc.borrow();
        let Some(parsed) = world.document(*node) else {
            return Err(Exception::throw_type(ctx, "stale range"));
        };
        iframes.extend(iframe_containers_in_subtrees(&parsed, &[*node]));
    }
    {
        for node in nodes {
            {
                let world = world_rc.borrow();
                let Some(mut parsed) = world.document_mut(*node) else {
                    return Err(Exception::throw_type(ctx, "stale range"));
                };
                unlink_journaled(&mut parsed, *node);
                if let Some(fragment) = fragment {
                    place_journaled(&mut parsed, fragment, *node, None);
                }
            }
            fixup_focus_after_removal(ctx, *node)?;
        }
    }
    world_rc.borrow().destroy_child_navigables(&iframes);
    Ok(())
}

fn split_endpoint_data(data: Vec<u16>, offset: u32, at_start: bool) -> (Vec<u16>, Vec<u16>) {
    let offset = bound(offset, data.len());
    if at_start {
        let mut kept = data;
        let slice = kept.split_off(offset);
        (slice, kept)
    } else {
        let mut slice = data;
        let kept = slice.split_off(offset);
        (slice, kept)
    }
}

fn clone_into_fragment(
    ctx: &Ctx<'_>,
    fragment: NodeId,
    source: NodeId,
    deep: bool,
    data: Option<Vec<u16>>,
) -> Result<NodeId> {
    let clone = {
        let world_rc = world(ctx)?;
        let world = world_rc.borrow();
        let Some(mut parsed) = world.document_mut(source) else {
            return Err(Exception::throw_type(ctx, "stale range"));
        };
        let store = parsed.id;
        clone_within_document(&mut parsed.document, store, source, deep)
            .map_err(|error| throw_dom_error(ctx, error))?
    };
    if let Some(data) = data {
        set_character_data(ctx, clone, &DomString::from_utf16(data))?;
    }
    {
        let world_rc = world(ctx)?;
        let world = world_rc.borrow();
        let Some(mut parsed) = world.document_mut(fragment) else {
            return Err(Exception::throw_type(ctx, "stale range"));
        };
        place_journaled(&mut parsed, fragment, clone, None);
    }
    Ok(clone)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ContentsTransfer {
    Cloned,
    Moved,
}

impl ContentsTransfer {
    fn verb(self) -> &'static str {
        match self {
            Self::Cloned => "cloned",
            Self::Moved => "extracted",
        }
    }
}

type SpanParts = (
    NodeId,
    u32,
    NodeId,
    u32,
    Option<NodeId>,
    Vec<NodeId>,
    Option<NodeId>,
);

fn span_parts(ctx: &Ctx<'_>, state: &RangeState) -> Result<SpanParts> {
    let (snode, soff, enode, eoff) = (
        state.start.node,
        state.start.offset,
        state.end.node,
        state.end.offset,
    );
    let (first, contained, last) = read_document(ctx, snode.document, |base| {
        let ancestor = ranges::common_ancestor(base, snode, enode);
        let want_first = !ranges::is_inclusive_ancestor(base, snode, enode);
        let want_last = !ranges::is_inclusive_ancestor(base, enode, snode);
        Ok(ranges::ancestor_span(
            base,
            state.start.node.document,
            state,
            ancestor,
            want_first,
            want_last,
        )
        .unwrap_or((None, Vec::new(), None)))
    })?;
    Ok((snode, soff, enode, eoff, first, contained, last))
}

fn reject_span_doctypes(ctx: &Ctx<'_>, contained: &[NodeId], verb: &str) -> Result<()> {
    for child in contained {
        let is_doctype = read_document(ctx, child.document, |base| {
            Ok(ranges::is_doctype(base, *child))
        })?;
        if is_doctype {
            return Err(throw_dom(
                ctx,
                "HierarchyRequestError",
                &format!("a doctype cannot be {verb}"),
            ));
        }
    }
    Ok(())
}

fn transfer_endpoint_text(
    ctx: &Ctx<'_>,
    fragment: NodeId,
    node: NodeId,
    offset: u32,
    at_start: bool,
    transfer: ContentsTransfer,
) -> Result<()> {
    let data = character_data(ctx, node)?.units().into_owned();
    let length = u32::try_from(data.len()).unwrap_or(u32::MAX);
    let (slice, _) = split_endpoint_data(data, offset, at_start);
    clone_into_fragment(ctx, fragment, node, false, Some(slice))?;
    if transfer == ContentsTransfer::Moved {
        // https://dom.spec.whatwg.org/#concept-range-extract
        let (from, count) = if at_start {
            (offset, length.saturating_sub(offset))
        } else {
            (0, offset)
        };
        replace_character_data(ctx, node, from, count, &DomString::default())?;
    }
    Ok(())
}

fn transfer_partial_element(
    ctx: &Ctx<'_>,
    fragment: NodeId,
    node: NodeId,
    sub: &RangeState,
    transfer: ContentsTransfer,
) -> Result<()> {
    let clone = clone_into_fragment(ctx, fragment, node, false, None)?;
    let scratch = create_fragment_in(ctx, fragment.document)?;
    produce_contents(ctx, scratch, sub, transfer)?;
    move_nodes_out(
        ctx,
        &read_document(ctx, scratch.document, |base| {
            Ok(child_ids(base, scratch.document, scratch.node))
        })?,
        Some(clone),
    )?;
    Ok(())
}

fn transfer_partial_endpoint(
    ctx: &Ctx<'_>,
    fragment: NodeId,
    sub: &RangeState,
    node: NodeId,
    at_start: bool,
    transfer: ContentsTransfer,
) -> Result<()> {
    let is_character_data = read_document(ctx, node.document, |base| {
        Ok(ranges::is_character_data(base, node))
    })?;
    if is_character_data {
        // https://dom.spec.whatwg.org/#concept-range-extract
        let offset = if at_start {
            sub.start.offset
        } else {
            sub.end.offset
        };
        return transfer_endpoint_text(ctx, fragment, node, offset, at_start, transfer);
    }
    transfer_partial_element(ctx, fragment, node, sub, transfer)
}

fn transfer_middle(
    ctx: &Ctx<'_>,
    fragment: NodeId,
    contained: &[NodeId],
    transfer: ContentsTransfer,
) -> Result<()> {
    if transfer == ContentsTransfer::Moved {
        move_nodes_out(ctx, contained, Some(fragment))
    } else {
        for child in contained {
            clone_into_fragment(ctx, fragment, *child, true, None)?;
        }
        Ok(())
    }
}

// https://dom.spec.whatwg.org/#dom-range-extractcontents
fn extract_into(ctx: &Ctx<'_>, fragment: NodeId, state: &RangeState) -> Result<()> {
    produce_contents(ctx, fragment, state, ContentsTransfer::Moved)
}

// https://dom.spec.whatwg.org/#dom-range-clonecontents
// https://dom.spec.whatwg.org/#dom-range-extractcontents
fn produce_contents(
    ctx: &Ctx<'_>,
    fragment: NodeId,
    state: &RangeState,
    transfer: ContentsTransfer,
) -> Result<()> {
    let (snode, soff, enode, eoff, first, contained, last) = span_parts(ctx, state)?;
    if state.start == state.end {
        return Ok(());
    }
    let same_text = read_document(ctx, snode.document, |base| {
        Ok(snode == enode && ranges::is_character_data(base, snode))
    })?;
    if same_text {
        let data = character_data(ctx, snode)?.units().into_owned();
        let start = bound(soff, data.len());
        let end = bound(eoff, data.len()).max(start);
        clone_into_fragment(ctx, fragment, snode, false, Some(data[start..end].to_vec()))?;
        if transfer == ContentsTransfer::Moved {
            // https://dom.spec.whatwg.org/#concept-range-extract
            replace_character_data(
                ctx,
                snode,
                soff,
                eoff.saturating_sub(soff),
                &DomString::default(),
            )?;
        }
        return Ok(());
    }
    reject_span_doctypes(ctx, &contained, transfer.verb())?;
    if let Some(first) = first {
        let sub = RangeState {
            start: BoundaryPoint {
                node: snode,
                offset: soff,
            },
            end: BoundaryPoint {
                node: first,
                offset: read_document(ctx, first.document, |base| {
                    Ok(ranges::node_length(base, first))
                })?,
            },
            attr: None,
        };
        transfer_partial_endpoint(ctx, fragment, &sub, first, true, transfer)?;
    }
    transfer_middle(ctx, fragment, &contained, transfer)?;
    if let Some(last) = last {
        let sub = RangeState {
            start: BoundaryPoint {
                node: last,
                offset: 0,
            },
            end: BoundaryPoint {
                node: enode,
                offset: eoff,
            },
            attr: None,
        };
        transfer_partial_endpoint(ctx, fragment, &sub, last, false, transfer)?;
    }
    Ok(())
}
// https://dom.spec.whatwg.org/#dom-range-clonecontents
fn clone_into(ctx: &Ctx<'_>, fragment: NodeId, state: &RangeState) -> Result<()> {
    produce_contents(ctx, fragment, state, ContentsTransfer::Cloned)
}

fn parent_of_raw(base: &blitz_dom::BaseDocument, id: NodeId) -> Option<NodeId> {
    base.get_node(id.node)
        .and_then(|node| node.parent)
        .map(|parent| NodeId {
            document: id.document,
            node: parent,
        })
}

fn parent_of(ctx: &Ctx<'_>, base: &blitz_dom::BaseDocument, id: NodeId) -> Result<NodeId> {
    parent_of_raw(base, id)
        .ok_or_else(|| throw_dom(ctx, "InvalidNodeTypeError", "the node has no parent"))
}

// https://dom.spec.whatwg.org/#dom-range-surroundcontents
fn check_surround_parent(ctx: &Ctx<'_>, new_parent: NodeId) -> Result<()> {
    let world_rc = world(ctx)?;
    let world = world_rc.borrow();
    let store = world.store();
    let Some(parsed) = store.get(new_parent.document) else {
        return Err(Exception::throw_type(ctx, "stale range"));
    };
    if parsed.document.is_fragment(new_parent.node) {
        return Err(throw_dom(
            ctx,
            "InvalidNodeTypeError",
            "a fragment cannot surround range contents",
        ));
    }
    let kind = parsed
        .document
        .base
        .get_node(new_parent.node)
        .map(|node| &node.data);
    if matches!(
        kind,
        Some(
            blitz_dom::NodeData::Document(_)
                | blitz_dom::NodeData::Doctype { .. }
                | blitz_dom::NodeData::Fragment { .. }
        )
    ) {
        return Err(throw_dom(
            ctx,
            "InvalidNodeTypeError",
            "only elements can surround range contents",
        ));
    }
    Ok(())
}

// https://dom.spec.whatwg.org/#dom-range-surroundcontents
fn ensure_element_parent(ctx: &Ctx<'_>, new_parent: NodeId) -> Result<()> {
    let takes_children = read_document(ctx, new_parent.document, |base| {
        Ok(base
            .get_node(new_parent.node)
            .is_some_and(|node| node.data.downcast_element().is_some()))
    })?;
    if !takes_children {
        return Err(throw_dom(
            ctx,
            "HierarchyRequestError",
            "only elements can hold surrounded contents",
        ));
    }
    Ok(())
}

impl<'js> range_generated::Range<'js> for JsRange {
    // https://dom.spec.whatwg.org/#dom-range-commonancestorcontainer
    fn get_common_ancestor_container(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        let state = self.load(ctx)?;
        if let Some(attr) = state.attr {
            // https://dom.spec.whatwg.org/#concept-range-common-ancestor
            return attr_wrapper(ctx, attr.id);
        }
        if !same_tree(ctx, state.start.node, state.end.node)? {
            return Ok(Value::new_null(ctx.clone()));
        }
        read_document(ctx, state.start.node.document, |base| {
            let ancestor = ranges::common_ancestor(base, state.start.node, state.end.node);
            wrap_node(ctx, ancestor)
        })
    }

    // https://dom.spec.whatwg.org/#dom-range-setstart
    fn set_start(&self, ctx: Ctx<'js>, node: NodeReference, offset: u32) -> Result<()> {
        let mut state = self.load(&ctx)?;
        apply_start(&ctx, &mut state, check_point(&ctx, node, offset)?)?;
        self.save(&ctx, state)
    }

    // https://dom.spec.whatwg.org/#dom-range-setend
    fn set_end(&self, ctx: Ctx<'js>, node: NodeReference, offset: u32) -> Result<()> {
        let mut state = self.load(&ctx)?;
        apply_end(&ctx, &mut state, check_point(&ctx, node, offset)?)?;
        self.save(&ctx, state)
    }

    // https://dom.spec.whatwg.org/#dom-range-setstartbefore
    fn set_start_before(&self, ctx: Ctx<'js>, node: NodeReference) -> Result<()> {
        let id = tree_node(&ctx, node)?;
        let mut state = self.load(&ctx)?;
        read_document(&ctx, id.document, |base| {
            let parent = parent_of(&ctx, base, id)?;
            let index = child_index_of(&ctx, base, parent, id)?;
            set_start_point(
                &mut state,
                base,
                BoundaryPoint {
                    node: parent,
                    offset: index,
                },
            );
            Ok(())
        })?;
        self.save(&ctx, state)
    }

    // https://dom.spec.whatwg.org/#dom-range-setstartafter
    fn set_start_after(&self, ctx: Ctx<'js>, node: NodeReference) -> Result<()> {
        let id = tree_node(&ctx, node)?;
        let mut state = self.load(&ctx)?;
        read_document(&ctx, id.document, |base| {
            let parent = parent_of(&ctx, base, id)?;
            let index = child_index_of(&ctx, base, parent, id)?;
            set_start_point(
                &mut state,
                base,
                BoundaryPoint {
                    node: parent,
                    offset: index.saturating_add(1),
                },
            );
            Ok(())
        })?;
        self.save(&ctx, state)
    }

    // https://dom.spec.whatwg.org/#dom-range-setendbefore
    fn set_end_before(&self, ctx: Ctx<'js>, node: NodeReference) -> Result<()> {
        let id = tree_node(&ctx, node)?;
        let mut state = self.load(&ctx)?;
        read_document(&ctx, id.document, |base| {
            let parent = parent_of(&ctx, base, id)?;
            let index = child_index_of(&ctx, base, parent, id)?;
            set_end_point(
                &mut state,
                base,
                BoundaryPoint {
                    node: parent,
                    offset: index,
                },
            );
            Ok(())
        })?;
        self.save(&ctx, state)
    }

    // https://dom.spec.whatwg.org/#dom-range-setendafter
    fn set_end_after(&self, ctx: Ctx<'js>, node: NodeReference) -> Result<()> {
        let id = tree_node(&ctx, node)?;
        let mut state = self.load(&ctx)?;
        read_document(&ctx, id.document, |base| {
            let parent = parent_of(&ctx, base, id)?;
            let index = child_index_of(&ctx, base, parent, id)?;
            set_end_point(
                &mut state,
                base,
                BoundaryPoint {
                    node: parent,
                    offset: index.saturating_add(1),
                },
            );
            Ok(())
        })?;
        self.save(&ctx, state)
    }

    // https://dom.spec.whatwg.org/#dom-range-collapse
    fn collapse(&self, ctx: Ctx<'js>, to_start: bool) -> Result<()> {
        let mut state = self.load(&ctx)?;
        if to_start {
            state.end = state.start;
        } else {
            state.start = state.end;
        }
        self.save(&ctx, state)
    }

    // https://dom.spec.whatwg.org/#dom-range-selectnode
    fn select_node(&self, ctx: Ctx<'js>, node: NodeReference) -> Result<()> {
        let id = tree_node(&ctx, node)?;
        let mut state = self.load(&ctx)?;
        read_document(&ctx, id.document, |base| {
            let parent = parent_of(&ctx, base, id)?;
            let index = child_index_of(&ctx, base, parent, id)?;
            state.start = BoundaryPoint {
                node: parent,
                offset: index,
            };
            state.end = BoundaryPoint {
                node: parent,
                offset: index.saturating_add(1),
            };
            Ok(())
        })?;
        self.save(&ctx, state)
    }

    // https://dom.spec.whatwg.org/#dom-range-selectnodecontents
    fn select_node_contents(&self, ctx: Ctx<'js>, node: NodeReference) -> Result<()> {
        let mut state = self.load(&ctx)?;
        match check_point(&ctx, node, 0)? {
            PointArg::Attribute(attr) => {
                // https://dom.spec.whatwg.org/#dom-range-selectnodecontents
                state = RangeState::attribute(attr);
            }
            PointArg::Tree(start) => {
                let id = start.node;
                read_document(&ctx, id.document, |base| {
                    let length = ranges::node_length(base, id);
                    state.start = BoundaryPoint {
                        node: id,
                        offset: 0,
                    };
                    state.end = BoundaryPoint {
                        node: id,
                        offset: length,
                    };
                    state.attr = None;
                    Ok(())
                })?;
            }
        }
        self.save(&ctx, state)
    }

    // https://dom.spec.whatwg.org/#dom-range-compareboundarypoints
    fn compare_boundary_points(&self, ctx: Ctx<'js>, how: u16, source: Value<'js>) -> Result<i16> {
        let source = Class::<JsRange>::from_value(&source)?;
        let source = source.borrow();
        // https://dom.spec.whatwg.org/#dom-range-compareboundarypoints
        if !matches!(how, 0..=3) {
            return Err(throw_dom(
                &ctx,
                "NotSupportedError",
                "unknown boundary comparison",
            ));
        }
        let other = source.load(&ctx)?;
        let state = self.load(&ctx)?;
        // https://dom.spec.whatwg.org/#concept-range-root
        match (state.attr, other.attr) {
            (Some(mine), Some(theirs)) if mine == theirs => return Ok(0),
            (Some(_), _) | (_, Some(_)) => {
                return Err(throw_dom(
                    &ctx,
                    "WrongDocumentError",
                    "ranges live in different trees",
                ));
            }
            (None, None) => {}
        }
        if !same_tree(&ctx, state.start.node, other.start.node)? {
            return Err(throw_dom(
                &ctx,
                "WrongDocumentError",
                "ranges live in different trees",
            ));
        }
        let (mine, theirs) = match how {
            0 => (state.start, other.start),
            1 => (state.end, other.start),
            2 => (state.end, other.end),
            _ => (state.start, other.end),
        };
        read_document(&ctx, state.start.node.document, |base| {
            Ok(
                match ranges::position(base, state.start.node.document, mine, theirs) {
                    Ordering::Less => -1,
                    Ordering::Equal => 0,
                    Ordering::Greater => 1,
                },
            )
        })
    }

    // https://dom.spec.whatwg.org/#dom-range-clonerange
    fn clone_range(&self, ctx: Ctx<'js>) -> Result<Value<'js>> {
        let state = self.load(&ctx)?;
        let (document, slot) = alloc_range(&ctx, self.registration.document.get(), state)?;
        wrap_range(&ctx, document, slot)
    }

    // https://dom.spec.whatwg.org/#dom-range-detach
    fn detach(&self, _ctx: Ctx<'js>) -> Result<()> {
        Ok(())
    }

    // https://dom.spec.whatwg.org/#dom-range-stringifier
    fn stringifier(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        let state = self.load(ctx)?;
        if state.attr.is_some() {
            // https://dom.spec.whatwg.org/#dom-range-stringifier
            return rquickjs::String::from_str(ctx.clone(), "");
        }
        let world_rc = world(ctx)?;
        let world = world_rc.borrow();
        let store = world.store();
        let Some(parsed) = store.get(self.registration.document.get()) else {
            return Err(Exception::throw_type(ctx, "stale range"));
        };
        let units = ranges::range_text(&parsed.document, parsed.id, &state);
        rquickjs::String::from_utf16(ctx.clone(), &units)
    }

    // https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-range-createcontextualfragment
    fn create_contextual_fragment(
        &self,
        ctx: Ctx<'js>,
        arg_0: rquickjs::String<'js>,
    ) -> Result<Value<'js>> {
        let state = self.load(&ctx)?;
        let markup = arg_0.to_string()?;
        create_contextual_fragment(&ctx, state.start.node, state.attr.is_some(), &markup)
    }

    // https://dom.spec.whatwg.org/#dom-range-ispointinrange
    fn is_point_in_range(&self, ctx: Ctx<'js>, node: NodeReference, offset: u32) -> Result<bool> {
        let state = self.load(&ctx)?;
        // https://dom.spec.whatwg.org/#dom-range-ispointinrange
        let same_root = match node {
            NodeReference::Attribute { scope, id } => {
                state.attr == Some(ranges::AttrPoint { scope, id })
            }
            NodeReference::Tree(id) => {
                state.attr.is_none() && same_tree(&ctx, id, state.start.node)?
            }
        };
        if !same_root {
            return Ok(false);
        }
        match check_point(&ctx, node, offset)? {
            PointArg::Attribute(_) => Ok(true),
            PointArg::Tree(point) => {
                let id = point.node;
                read_document(&ctx, id.document, |base| {
                    Ok(
                        ranges::position(base, id.document, point, state.start) != Ordering::Less
                            && ranges::position(base, id.document, point, state.end)
                                != Ordering::Greater,
                    )
                })
            }
        }
    }

    // https://dom.spec.whatwg.org/#dom-range-comparepoint
    fn compare_point(&self, ctx: Ctx<'js>, node: NodeReference, offset: u32) -> Result<i16> {
        let state = self.load(&ctx)?;
        // https://dom.spec.whatwg.org/#dom-range-comparepoint
        let same_root = match node {
            NodeReference::Attribute { scope, id } => {
                state.attr == Some(ranges::AttrPoint { scope, id })
            }
            NodeReference::Tree(id) => {
                state.attr.is_none() && same_tree(&ctx, id, state.start.node)?
            }
        };
        if !same_root {
            return Err(throw_dom(
                &ctx,
                "WrongDocumentError",
                "the point lives in a different tree",
            ));
        }
        match check_point(&ctx, node, offset)? {
            PointArg::Attribute(_) => Ok(0),
            PointArg::Tree(point) => {
                let id = point.node;
                read_document(&ctx, id.document, |base| {
                    if ranges::position(base, id.document, point, state.start) == Ordering::Less {
                        Ok(-1)
                    } else if ranges::position(base, id.document, point, state.end)
                        == Ordering::Greater
                    {
                        Ok(1)
                    } else {
                        Ok(0)
                    }
                })
            }
        }
    }

    // https://dom.spec.whatwg.org/#dom-range-intersectsnode
    fn intersects_node(&self, ctx: Ctx<'js>, node: NodeReference) -> Result<bool> {
        let state = self.load(&ctx)?;
        let id = match node {
            NodeReference::Attribute { scope, id } => {
                // https://dom.spec.whatwg.org/#dom-range-intersectsnode
                return Ok(state.attr == Some(ranges::AttrPoint { scope, id }));
            }
            NodeReference::Tree(id) => id,
        };
        if state.attr.is_some() || !same_tree(&ctx, id, state.start.node)? {
            return Ok(false);
        }
        read_document(&ctx, id.document, |base| {
            let Some(parent) = parent_of_raw(base, id) else {
                return Ok(true);
            };
            let index = child_index_of(&ctx, base, parent, id)?;
            let before_end = ranges::position(
                base,
                id.document,
                BoundaryPoint {
                    node: parent,
                    offset: index,
                },
                state.end,
            ) == Ordering::Less;
            let after_start = ranges::position(
                base,
                id.document,
                BoundaryPoint {
                    node: parent,
                    offset: index.saturating_add(1),
                },
                state.start,
            ) == Ordering::Greater;
            Ok(before_end && after_start)
        })
    }

    // https://dom.spec.whatwg.org/#dom-range-deletecontents
    fn delete_contents(&self, ctx: Ctx<'js>) -> Result<()> {
        let state = self.load(&ctx)?;
        if state.start == state.end {
            return Ok(());
        }
        let (snode, soff, enode, eoff) = (
            state.start.node,
            state.start.offset,
            state.end.node,
            state.end.offset,
        );
        let same_text = read_document(&ctx, snode.document, |base| {
            Ok(snode == enode && ranges::is_character_data(base, snode))
        })?;
        if same_text {
            // https://dom.spec.whatwg.org/#dom-range-deletecontents
            replace_character_data(
                &ctx,
                snode,
                soff,
                eoff.saturating_sub(soff),
                &DomString::default(),
            )?;
            return Ok(());
        }
        let (new_node, new_offset, doomed, start_text, end_text) =
            read_document(&ctx, snode.document, |base| {
                let point = collapse_point(base, &state);
                let ancestor = ranges::common_ancestor(base, snode, enode);
                let mut doomed = Vec::new();
                collect_contained(base, snode.document, &state, ancestor, &mut doomed);
                Ok((
                    point.0,
                    point.1,
                    doomed,
                    ranges::is_character_data(base, snode),
                    ranges::is_character_data(base, enode),
                ))
            })?;
        self.save(
            &ctx,
            RangeState::collapsed(BoundaryPoint {
                node: new_node,
                offset: new_offset,
            }),
        )?;
        if start_text {
            // https://dom.spec.whatwg.org/#dom-range-deletecontents
            replace_character_data(&ctx, snode, soff, u32::MAX, &DomString::default())?;
        }
        move_nodes_out(&ctx, &doomed, None)?;
        if end_text {
            // https://dom.spec.whatwg.org/#dom-range-deletecontents
            replace_character_data(&ctx, enode, 0, eoff, &DomString::default())?;
        }
        schedule_mutation_delivery(&ctx)?;
        Ok(())
    }

    // https://dom.spec.whatwg.org/#dom-range-extractcontents
    fn extract_contents(&self, ctx: Ctx<'js>) -> Result<Value<'js>> {
        let state = self.load(&ctx)?;
        let fragment = create_fragment_in(&ctx, state.start.node.document)?;
        self.extract_into_slot(&ctx, fragment, &state)?;
        wrap_node(&ctx, fragment)
    }

    // https://dom.spec.whatwg.org/#dom-range-clonecontents
    fn clone_contents(&self, ctx: Ctx<'js>) -> Result<Value<'js>> {
        let state = self.load(&ctx)?;
        let fragment = create_fragment_in(&ctx, state.start.node.document)?;
        clone_into(&ctx, fragment, &state)?;
        wrap_node(&ctx, fragment)
    }

    // https://dom.spec.whatwg.org/#dom-range-insertnode
    fn insert_node(&self, ctx: Ctx<'js>, node: NodeReference) -> Result<()> {
        let arg = match node {
            NodeReference::Attribute { .. } => {
                // https://dom.spec.whatwg.org/#concept-node-pre-insert
                return Err(throw_dom(
                    &ctx,
                    "HierarchyRequestError",
                    "an attribute cannot be inserted",
                ));
            }
            NodeReference::Tree(id) => id,
        };
        let state = self.load(&ctx)?;
        if state.attr.is_some() {
            // https://dom.spec.whatwg.org/#concept-range-insert
            return Err(throw_dom(
                &ctx,
                "HierarchyRequestError",
                "an attribute takes no nodes",
            ));
        }
        let (parent, reference) = insert_node_target(&ctx, &state, arg)?;
        let (node, reference) =
            insertion_tree_nodes(&ctx, parent, node, reference.map(NodeReference::Tree))?;
        let node = adopt_node(&ctx, parent, node)?;
        let mut reference = reference;
        if read_document(&ctx, state.start.node.document, |base| {
            Ok(ranges::is_text(base, state.start.node))
        })? {
            // https://dom.spec.whatwg.org/#dom-range-insertnode
            let split = split_text_node(&ctx, state.start.node, state.start.offset)?;
            reference = Some(required_node(&ctx, &split)?);
        }
        if Some(node) == reference {
            reference = read_document(&ctx, parent.document, |base| {
                Ok(reference.and_then(|reference| {
                    sibling(base, reference.node, true).map(|next| NodeId {
                        document: parent.document,
                        node: next,
                    })
                }))
            })?;
        }
        insert_tree_node(&ctx, parent, node, reference)?;
        let state = self.load(&ctx)?;
        if state.start == state.end {
            let new_offset = read_document(&ctx, parent.document, |base| {
                Ok(match reference {
                    Some(reference) => child_ids(base, parent.document, parent.node)
                        .iter()
                        .position(|sibling| *sibling == reference)
                        .and_then(|index| u32::try_from(index).ok())
                        .unwrap_or(0),
                    None => ranges::node_length(base, parent),
                })
            })?;
            self.save(
                &ctx,
                RangeState {
                    start: state.start,
                    end: BoundaryPoint {
                        node: parent,
                        offset: new_offset,
                    },
                    attr: None,
                },
            )?;
        }
        Ok(())
    }

    // https://dom.spec.whatwg.org/#dom-range-surroundcontents
    fn surround_contents(&self, ctx: Ctx<'js>, node: NodeReference) -> Result<()> {
        let new_parent = tree_node(&ctx, node)?;
        let state = self.load(&ctx)?;
        // https://dom.spec.whatwg.org/#dom-range-surroundcontents
        read_document(&ctx, state.start.node.document, |base| {
            let ancestor = ranges::common_ancestor(base, state.start.node, state.end.node);
            let (first, _, last) = ranges::ancestor_span(
                base,
                state.start.node.document,
                &state,
                ancestor,
                true,
                true,
            )
            .unwrap_or((None, Vec::new(), None));
            for part in [first, last].into_iter().flatten() {
                if !ranges::is_text(base, part) {
                    return Err(throw_dom(
                        &ctx,
                        "InvalidStateError",
                        "a partially contained non-text node cannot be surrounded",
                    ));
                }
            }
            Ok(())
        })?;
        check_surround_parent(&ctx, new_parent)?;
        let fragment = create_fragment_in(&ctx, state.start.node.document)?;
        self.extract_into_slot(&ctx, fragment, &state)?;
        {
            let world_rc = world(&ctx)?;
            let world = world_rc.borrow();
            let Some(mut parsed) = world.document_mut(new_parent) else {
                return Err(Exception::throw_type(&ctx, "stale range"));
            };
            let iframes = replace_all_journaled(&mut parsed, new_parent, Vec::new());
            drop(parsed);
            world.destroy_child_navigables(&iframes);
            drop(world);
        }
        self.insert_node(ctx.clone(), NodeReference::Tree(new_parent))?;
        // https://dom.spec.whatwg.org/#concept-node-adopt
        let new_parent = realm_registry(&ctx)?.borrow_mut().live_node_id(new_parent);
        ensure_element_parent(&ctx, new_parent)?;
        move_nodes_out(
            &ctx,
            &read_document(&ctx, fragment.document, |base| {
                Ok(child_ids(base, fragment.document, fragment.node))
            })?,
            Some(new_parent),
        )?;
        let (parent, index) = read_document(&ctx, new_parent.document, |base| {
            let parent = parent_of(&ctx, base, new_parent)?;
            let index = child_index_of(&ctx, base, parent, new_parent)?;
            Ok((parent, index))
        })?;
        self.save(
            &ctx,
            RangeState {
                start: BoundaryPoint {
                    node: parent,
                    offset: index,
                },
                end: BoundaryPoint {
                    node: parent,
                    offset: index.saturating_add(1),
                },
                attr: None,
            },
        )?;
        schedule_mutation_delivery(&ctx)?;
        Ok(())
    }
}

impl<'js> abstract_range_generated::AbstractRange<'js> for JsRange {
    // https://dom.spec.whatwg.org/#dom-abstractrange-startcontainer
    fn get_start_container(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        let state = self.load(ctx)?;
        match state.attr {
            Some(attr) => attr_wrapper(ctx, attr.id),
            None => wrap_node(ctx, state.start.node),
        }
    }

    // https://dom.spec.whatwg.org/#dom-abstractrange-startoffset
    fn get_start_offset(&self, ctx: &Ctx<'js>) -> Result<usize> {
        let state = self.load(ctx)?;
        if state.attr.is_some() {
            return Ok(0);
        }
        Ok(usize::try_from(state.start.offset).unwrap_or(usize::MAX))
    }

    // https://dom.spec.whatwg.org/#dom-abstractrange-endcontainer
    fn get_end_container(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        let state = self.load(ctx)?;
        match state.attr {
            Some(attr) => attr_wrapper(ctx, attr.id),
            None => wrap_node(ctx, state.end.node),
        }
    }

    // https://dom.spec.whatwg.org/#dom-abstractrange-endoffset
    fn get_end_offset(&self, ctx: &Ctx<'js>) -> Result<usize> {
        let state = self.load(ctx)?;
        if state.attr.is_some() {
            return Ok(0);
        }
        Ok(usize::try_from(state.end.offset).unwrap_or(usize::MAX))
    }

    // https://dom.spec.whatwg.org/#dom-abstractrange-collapsed
    fn get_collapsed(&self, ctx: &Ctx<'js>) -> Result<bool> {
        let state = self.load(ctx)?;
        Ok(state.attr.is_some() || state.start == state.end)
    }
}

impl JsRange {
    fn extract_into_slot(&self, ctx: &Ctx<'_>, fragment: NodeId, state: &RangeState) -> Result<()> {
        // https://dom.spec.whatwg.org/#concept-range-extract
        if state.start == state.end {
            return Ok(());
        }
        // https://dom.spec.whatwg.org/#concept-range-extract
        let same_text = read_document(ctx, state.start.node.document, |base| {
            Ok(state.start.node == state.end.node
                && ranges::is_character_data(base, state.start.node))
        })?;
        if !same_text {
            let (_, _, _, _, _, contained, _) = span_parts(ctx, state)?;
            reject_span_doctypes(ctx, &contained, "extracted")?;
        }
        let (new_node, new_offset) = read_document(ctx, state.start.node.document, |base| {
            Ok(collapse_point(base, state))
        })?;
        self.save(
            ctx,
            RangeState::collapsed(BoundaryPoint {
                node: new_node,
                offset: new_offset,
            }),
        )?;
        extract_into(ctx, fragment, state)?;
        Ok(())
    }
}
