//! Element interfaces: `Element`, `HTMLElement`, `SVG` and `MathML` elements,
//! ARIA and editing mixins, hyperlink utilities, and generic reflection.

use super::super::{
    AttrArgument, CollectionKind, JsNamedNodeMap, JsTokenList, LegacyNullString, NodeContext,
    NodeOrString, OptString, WebIdlString, active_world, after_attribute_change, attached_attr_id,
    attr_owner, attr_state, attr_wrapper, attribute_local_name, attribute_value, blur_node,
    deref_weak, document_is_html_content, dom_string, element_click, fixup_focus_after_removal,
    focus_node, get_element_reference, get_element_reference_list, host, host_node_id,
    is_focusable, js_string_to_utf8, live_collection, make_weak, rect_object,
    remove_attribute_sync, schedule_mutation_delivery, set_attribute_node, set_attribute_sync,
    set_element_reference, set_element_reference_list, string_value, throw_dom, throw_dom_error,
    touch_attr, valid_attribute_local_name, validate_and_extract, with_node_data, world,
    world_for_node, wrap_node,
};
use super::forms::fixup_controls_on_insert;
use super::offset::{cssom_round, has_associated_box};
use crate::dom::adoption::materialize_children;
use std::collections::HashSet;

use blitz_dom::NodeData;
use style::computed_values::visibility::T as Visibility;
use stylo_taffy::CopyStyleGetters;

use rquickjs::{Array, Class, Ctx, Exception, Function, Persistent, Result, Value};

use crate::dom::insertion::{
    fragment_children, iframe_containers_in_subtrees, place_journaled, replace_all_journaled,
    replace_parsed, unlink_journaled,
};
use crate::dom::serialization;
use crate::dom::tree::{
    BlitzId, NodeId, element_node_name, html_namespace, qualified_name, shadow_including_ancestors,
    sibling,
};
use crate::dom_string::DomString;

use crate::Parsed;
use crate::documents::JournalEntry;
use crate::js::world::{NodeReference, SameObject, World};

use super::JsNode;
use super::core::{adopt_node, insert_tree_node, insertion_tree_nodes, upgrade_parsed_tree};
use super::document::{
    fragment_base_url, html_fragment_context, parse_html_fragment_snapshots, place_adjacent_rest,
    register_inserted_iframes,
};

include!(concat!(env!("OUT_DIR"), "/Element.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLElement.rs"));
include!(concat!(env!("OUT_DIR"), "/SVGElement.rs"));
include!(concat!(env!("OUT_DIR"), "/MathMLElement.rs"));
include!(concat!(env!("OUT_DIR"), "/ARIAMixin.rs"));
include!(concat!(env!("OUT_DIR"), "/ElementCSSInlineStyle.rs"));
include!(concat!(env!("OUT_DIR"), "/ElementContentEditable.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLHyperlinkElementUtils.rs"));

/// Run the scripts in newly inserted markup, in tree order, which is
/// `setHTMLUnsafe(runScripts: true)`: "scripts contained in html will be
/// executed immediately after the node tree is updated"
/// (<https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-element-sethtmlunsafe>).
/// A script that is not connected to the document, such as one inside template
/// contents, is skipped by "prepare the script element"
/// (<https://html.spec.whatwg.org/multipage/scripting.html#prepare-the-script-element>).
fn run_inserted_scripts(ctx: &Ctx<'_>, roots: &[NodeId]) -> Result<()> {
    let mut scripts = Vec::new();
    for root in roots {
        collect_scripts(ctx, *root, &mut scripts)?;
    }
    for script in scripts {
        super::document::prepare_classic_script(ctx, script)?;
    }
    Ok(())
}

fn collect_scripts(ctx: &Ctx<'_>, id: NodeId, out: &mut Vec<NodeId>) -> Result<()> {
    let children: Vec<BlitzId> = {
        let world = world_for_node(ctx, id)?;
        let world = world.borrow();
        let Some(parsed) = world.document(id) else {
            return Ok(());
        };
        let base = &parsed.document.base;
        if crate::dom::tree::is_html_element(base, id.node, "script") {
            out.push(id);
        }
        base.get_node(id.node)
            .map_or_default(|node| node.children.iter().copied().collect())
    };
    for child in children {
        collect_scripts(
            ctx,
            NodeId {
                document: id.document,
                node: child,
            },
            out,
        )?;
    }
    Ok(())
}

/// Replace `target`'s children (or its template contents, or a shadow root's
/// children) with parsed `markup`.
///
/// `innerHTML` and `setHTMLUnsafe` share this: both parse a fragment in the
/// target's context and replace its children, and only `setHTMLUnsafe` runs the
/// scripts afterwards
/// (<https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#fragment-parsing-algorithm-steps>).
pub(super) fn replace_with_markup(
    ctx: &Ctx<'_>,
    target: NodeId,
    markup: &str,
    run_scripts: bool,
) -> Result<()> {
    // Adoption by copy leaves a stale handle behind; parse into the live
    // container (https://dom.spec.whatwg.org/#concept-node-adopt).
    let element = super::super::realm_registry(ctx)?
        .borrow_mut()
        .live_node_id(target);
    // A shadow root's fragment parsing context is its host
    // (<https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-shadowroot-innerhtml>).
    let context = super::document::inner_html_fragment_context(ctx, element)?;

    let snapshots = {
        let (base_url, font_ctx) = fragment_base_url(ctx, element);
        parse_html_fragment_snapshots(markup, &context, &base_url, font_ctx)
    };

    let container = element;
    // A template's markup lands in its template contents, not its (empty)
    // children
    // (<https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-element-innerhtml>).
    let container = world_for_node(ctx, container)?
        .borrow()
        .document(container)
        .map_or(container, |parsed| {
            template_contents_target(&parsed.document, container)
        });
    let owner = world_for_node(ctx, container)?;
    let world = owner.borrow();
    let Some(mut parsed) = world.document_mut(container) else {
        return Err(Exception::throw_type(ctx, "no document"));
    };
    let replacement = materialize_children(&mut parsed.document, container.document, &snapshots)
        .map_err(|err| throw_dom_error(ctx, err))?;
    let added = fragment_children(&parsed.document, replacement);
    let iframes = replace_all_journaled(&mut parsed, container, added.clone());
    let controls = crate::dom::forms::descendant_controls(
        &parsed.document.base,
        container.document,
        container.node,
    );
    drop(parsed);
    world.destroy_child_navigables(&iframes);
    drop(world);
    fixup_controls_on_insert(ctx, container.document, &controls)?;
    register_inserted_iframes(ctx, &added)?;
    // Parsed content attributes never pass through the DOM setter, so their
    // handler change steps run as part of the insertion, before the
    // fragment's scripts
    // (<https://html.spec.whatwg.org/multipage/webappapis.html#event-handler-content-attributes>).
    for root in &added {
        super::super::install_parsed_handlers(ctx, *root)?;
    }
    // Scripts run as part of the insertion, before the custom element
    // reactions `[CEReactions]` flushes when the method returns
    // (<https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-element-sethtmlunsafe>).
    if run_scripts {
        run_inserted_scripts(ctx, &added)?;
    }
    upgrade_parsed_tree(ctx, container)?;
    schedule_mutation_delivery(ctx)
}

/// The host side of `setHTMLUnsafe` for an `Element` or `ShadowRoot`. The
/// interface operation itself installs in JS because its `(TrustedHTML or
/// DOMString)` argument and `SetHTMLUnsafeOptions` dictionary are unions the
/// binding generator does not lower yet; the receiver and target rules stay
/// here, where the tree state lives
/// (<https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-element-sethtmlunsafe>).
pub(crate) fn set_html_unsafe_host<'js>(
    ctx: Ctx<'js>,
    value: Value<'js>,
    html: String,
    run_scripts: bool,
) -> Result<()> {
    let Some(id) = host_node_id(&ctx, &value) else {
        return Err(Exception::throw_type(
            &ctx,
            "setHTMLUnsafe requires an element or shadow root",
        ));
    };
    let target = super::super::realm_registry(&ctx)?
        .borrow_mut()
        .live_node_id(id);
    let is_target = with_node_data(&ctx, target, |data| {
        matches!(
            data,
            Some(NodeData::Element(_) | NodeData::ShadowRoot { .. })
        )
    })?;
    if !is_target {
        return Err(Exception::throw_type(
            &ctx,
            "setHTMLUnsafe requires an element or shadow root",
        ));
    }
    replace_with_markup(&ctx, target, &html, run_scripts)
}

/// Where a `*HTMLUnsafe` insertion method puts the parsed fragment
/// (<https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-element-appendhtmlunsafe>).
#[derive(Clone, Copy)]
pub(crate) enum InsertPosition {
    /// `appendHTMLUnsafe`
    Append,
    /// `prependHTMLUnsafe`
    Prepend,
    /// `beforeHTMLUnsafe`
    Before,
    /// `afterHTMLUnsafe`
    After,
    /// `replaceWithHTMLUnsafe`
    ReplaceWith,
}

impl InsertPosition {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "append" => Some(Self::Append),
            "prepend" => Some(Self::Prepend),
            "before" => Some(Self::Before),
            "after" => Some(Self::After),
            "replaceWith" => Some(Self::ReplaceWith),
            _ => None,
        }
    }

    /// The receiver kinds the method is exposed on: `ParentNode`'s mixin for
    /// the container positions, `ChildNode`'s for the sibling positions
    /// (<https://dom.spec.whatwg.org/#interface-parentnode>,
    /// <https://dom.spec.whatwg.org/#interface-childnode>).
    fn accepts(self, data: &NodeData) -> bool {
        match self {
            Self::Append | Self::Prepend => {
                matches!(data, NodeData::Element(_) | NodeData::ShadowRoot { .. })
            }
            Self::Before | Self::After | Self::ReplaceWith => matches!(
                data,
                NodeData::Element(_)
                    | NodeData::Text(_)
                    | NodeData::CDataSection { .. }
                    | NodeData::Comment { .. }
                    | NodeData::ProcessingInstruction { .. }
            ),
        }
    }
}

/// The resolved insertion point of an `*HTMLUnsafe` method.
struct InsertionPoint {
    /// The node whose children receive the fragment.
    container: NodeId,
    /// The node to insert before; `None` appends.
    reference: Option<NodeId>,
    /// The fragment parsing context element's local name.
    context: String,
    /// The node whose subtree's custom elements upgrade after insertion.
    upgrade_root: NodeId,
    /// The node the insertion replaces (`replaceWithHTMLUnsafe`).
    replaced: Option<NodeId>,
}

/// Resolves where `position` on `target` inserts, applying the spec's "target
/// for HTML insertion" and "parent for HTML insertion" rules
/// (<https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#target-for-html-insertion>,
/// <https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#parent-for-html-insertion>).
fn insertion_point(
    ctx: &Ctx<'_>,
    target: NodeId,
    position: InsertPosition,
) -> Result<Option<InsertionPoint>> {
    let owner = world_for_node(ctx, target)?;
    let owner = owner.borrow();
    let Some(parsed) = owner.document(target) else {
        return Err(Exception::throw_type(ctx, "no document"));
    };
    let Some(node) = parsed.document.base.get_node(target.node) else {
        return Err(Exception::throw_type(ctx, "stale node"));
    };
    if !position.accepts(&node.data) {
        return Err(Exception::throw_type(
            ctx,
            "the method requires an element or character data",
        ));
    }
    match position {
        InsertPosition::Append | InsertPosition::Prepend => {
            container_point(ctx, &owner, &parsed, target, position).map(Some)
        }
        InsertPosition::Before | InsertPosition::After | InsertPosition::ReplaceWith => {
            child_point(ctx, &owner, &parsed, target, position)
        }
    }
}

/// The `appendHTMLUnsafe`/`prependHTMLUnsafe` insertion point
/// (<https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#target-for-html-insertion>).
fn container_point(
    ctx: &Ctx<'_>,
    world: &World,
    parsed: &Parsed,
    target: NodeId,
    position: InsertPosition,
) -> Result<InsertionPoint> {
    let base = &parsed.document.base;
    let Some(node) = base.get_node(target.node) else {
        return Err(Exception::throw_type(ctx, "stale node"));
    };
    let context = match &node.data {
        NodeData::Element(element) => html_fragment_context(&element.name),
        NodeData::ShadowRoot { host, .. } => {
            let host = base
                .get_node(*host)
                .and_then(|host| host.data.downcast_element())
                .ok_or_else(|| Exception::throw_type(ctx, "shadow root has no host"))?;
            html_fragment_context(&host.name)
        }
        // `InsertPosition::accepts` rejected every other kind
        _ => unreachable!(),
    };
    // `target for HTML insertion` unwraps a template to its contents, while
    // the fragment still parses in the template element itself
    // (<https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#target-for-html-insertion>).
    let container = match node.data {
        NodeData::Element(_) => template_contents_target(&parsed.document, target),
        _ => target,
    };
    // The container may live in the template's inert document, so its children
    // come from that document's tree, not `target`'s
    let container_base = world
        .document(container)
        .ok_or_else(|| Exception::throw_type(ctx, "no document"))?;
    let reference = if matches!(position, InsertPosition::Prepend) {
        container_base
            .document
            .base
            .get_node(container.node)
            .and_then(|node| node.children.first().copied())
            .map(|node| NodeId {
                document: container.document,
                node,
            })
    } else {
        None
    };
    Ok(InsertionPoint {
        container,
        reference,
        context,
        upgrade_root: container,
        replaced: None,
    })
}

/// The `beforeHTMLUnsafe`/`afterHTMLUnsafe`/`replaceWithHTMLUnsafe` insertion
/// point; `None` when the receiver has no parent
/// (<https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#parent-for-html-insertion>).
fn child_point(
    ctx: &Ctx<'_>,
    world: &World,
    parsed: &Parsed,
    target: NodeId,
    position: InsertPosition,
) -> Result<Option<InsertionPoint>> {
    let base = &parsed.document.base;
    let Some(node) = base.get_node(target.node) else {
        return Err(Exception::throw_type(ctx, "stale node"));
    };
    let Some(parent) = node.parent.map(|node| NodeId {
        document: target.document,
        node,
    }) else {
        return Ok(None);
    };
    let Some(parent_node) = base.get_node(parent.node) else {
        return Err(Exception::throw_type(ctx, "stale node"));
    };
    let context = parent_context(ctx, world, parsed, &parent_node.data)?;
    // The fragment goes in where the receiver was. `replace`'s reference child
    // is the receiver's next sibling: the receiver is removed first, so it
    // cannot be the anchor
    // (<https://dom.spec.whatwg.org/#concept-node-replace>).
    let reference = match position {
        InsertPosition::Before => Some(target),
        _ => sibling(base, target.node, true).map(|node| NodeId {
            document: target.document,
            node,
        }),
    };
    let replaced = if matches!(position, InsertPosition::ReplaceWith) {
        Some(target)
    } else {
        None
    };
    Ok(Some(InsertionPoint {
        container: parent,
        reference,
        context,
        upgrade_root: parent,
        replaced,
    }))
}

/// The fragment parsing context of a child insertion: the parent element, the
/// host of a `DocumentFragment` or shadow root, and a throw for anything else
/// (<https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#parent-for-html-insertion>).
fn parent_context(
    ctx: &Ctx<'_>,
    world: &World,
    parsed: &Parsed,
    data: &NodeData,
) -> Result<String> {
    let base = &parsed.document.base;
    match data {
        NodeData::Element(element) => {
            // A child of the root `html` element has no HTML insertion parent;
            // the method throws instead of building a tree the document
            // content model rejects
            // (<https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#filter-and-pre-insert-html>).
            if element.name.ns == html_namespace()
                && element.name.local == markup5ever::local_name!("html")
            {
                return Err(throw_dom(
                    ctx,
                    "HierarchyRequestError",
                    "the parent is an html element",
                ));
            }
            Ok(html_fragment_context(&element.name))
        }
        NodeData::Fragment { host: Some(host) } | NodeData::ShadowRoot { host, .. } => {
            let host = base
                .get_node(*host)
                .and_then(|node| node.data.downcast_element())
                .ok_or_else(|| Exception::throw_type(ctx, "fragment has no host"))?;
            Ok(html_fragment_context(&host.name))
        }
        // Template contents live in their own inert document, where the
        // fragment cannot name its host element; the side table carries the
        // back link
        // (<https://html.spec.whatwg.org/multipage/scripting.html#template-contents>).
        NodeData::Fragment { host: None } => {
            let (document, host) = parsed
                .document
                .template_host()
                .ok_or_else(|| no_html_parent(ctx))?;
            let host_document = world
                .document(NodeId {
                    document,
                    node: host,
                })
                .ok_or_else(|| no_html_parent(ctx))?;
            let host = host_document
                .document
                .base
                .get_node(host)
                .and_then(|node| node.data.downcast_element())
                .ok_or_else(|| Exception::throw_type(ctx, "fragment has no host"))?;
            Ok(html_fragment_context(&host.name))
        }
        _ => Err(no_html_parent(ctx)),
    }
}

fn no_html_parent(ctx: &Ctx<'_>) -> rquickjs::Error {
    throw_dom(
        ctx,
        "HierarchyRequestError",
        "the parent cannot receive HTML",
    )
}

/// The host side of the `*HTMLUnsafe` partial-update insertion methods. They
/// install in JS for the same reason `setHTMLUnsafe` does: their
/// `(TrustedHTML or DOMString)` argument and options dictionary are unions the
/// binding generator does not lower yet
/// (<https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#html-insertion-methods>).
pub(crate) fn insert_html_unsafe_host<'js>(
    ctx: Ctx<'js>,
    value: Value<'js>,
    html: String,
    position: String,
    run_scripts: bool,
) -> Result<()> {
    let Some(id) = host_node_id(&ctx, &value) else {
        return Err(Exception::throw_type(
            &ctx,
            "the method requires a Node receiver",
        ));
    };
    let Some(position) = InsertPosition::parse(&position) else {
        return Err(Exception::throw_type(&ctx, "invalid insertion position"));
    };
    let target = super::super::realm_registry(&ctx)?
        .borrow_mut()
        .live_node_id(id);
    let Some(point) = insertion_point(&ctx, target, position)? else {
        return Ok(());
    };
    insert_html_unsafe(&ctx, target, &html, point, run_scripts)
}

/// Parses `markup` in the point's context, places the fragment, and runs the
/// fixups the insertion requires.
fn insert_html_unsafe(
    ctx: &Ctx<'_>,
    target: NodeId,
    markup: &str,
    point: InsertionPoint,
    run_scripts: bool,
) -> Result<()> {
    let snapshots = {
        let (base_url, font_ctx) = fragment_base_url(ctx, target);
        parse_html_fragment_snapshots(markup, &point.context, &base_url, font_ctx)
    };
    let owner = world_for_node(ctx, point.container)?;
    let world = owner.borrow();
    let Some(mut parsed) = world.document_mut(point.container) else {
        return Err(Exception::throw_type(ctx, "no document"));
    };
    let replacement =
        materialize_children(&mut parsed.document, point.container.document, &snapshots)
            .map_err(|err| throw_dom_error(ctx, err))?;
    let added = fragment_children(&parsed.document, replacement);
    // A replaced subtree's child navigables die with it
    // (<https://html.spec.whatwg.org/multipage/document-sequences.html#destroy-a-child-navigable>).
    let mut iframes = Vec::new();
    // `replace` removes the old node, then inserts the fragment where it was
    // (<https://dom.spec.whatwg.org/#concept-node-replace>).
    let reference = match point.replaced {
        Some(replaced) => {
            iframes = iframe_containers_in_subtrees(&parsed, &[replaced]);
            let reference = point.reference;
            unlink_journaled(&mut parsed, replaced);
            reference
        }
        None => point.reference,
    };
    for child in &added {
        place_journaled(&mut parsed, point.container, *child, reference);
    }
    let controls: Vec<_> = added
        .iter()
        .flat_map(|root| {
            crate::dom::forms::descendant_controls(&parsed.document.base, root.document, root.node)
        })
        .collect();
    drop(parsed);
    world.destroy_child_navigables(&iframes);
    drop(world);
    fixup_controls_on_insert(ctx, point.container.document, &controls)?;
    register_inserted_iframes(ctx, &added)?;
    // Parsed content attributes never pass through the DOM setter, so their
    // handler change steps run as part of the insertion, before the
    // fragment's scripts
    // (<https://html.spec.whatwg.org/multipage/webappapis.html#event-handler-content-attributes>).
    for root in &added {
        super::super::install_parsed_handlers(ctx, *root)?;
    }
    // Scripts run as part of the insertion, before the custom element
    // reactions the method's `[CEReactions]` flushes when it returns
    // (<https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-element-appendhtmlunsafe>).
    if run_scripts {
        run_inserted_scripts(ctx, &added)?;
    }
    upgrade_parsed_tree(ctx, point.upgrade_root)?;
    schedule_mutation_delivery(ctx)
}

impl<'js> element_generated::Element<'js> for JsNode {
    // https://dom.spec.whatwg.org/#dom-element-getelementsbytagname
    fn get_elements_by_tag_name(
        &self,
        ctx: Ctx<'js>,
        arg_0: rquickjs::String<'js>,
    ) -> Result<Value<'js>> {
        self.get_elements_by_tag_name(ctx, WebIdlString(arg_0.to_string()?))
    }

    // https://dom.spec.whatwg.org/#dom-element-getelementsbytagnamens
    fn get_elements_by_tag_name_ns(
        &self,
        ctx: Ctx<'js>,
        arg_0: Option<rquickjs::String<'js>>,
        arg_1: rquickjs::String<'js>,
    ) -> Result<Value<'js>> {
        self.get_elements_by_tag_name_ns(
            ctx,
            OptString(arg_0.map(|name| name.to_string()).transpose()?),
            WebIdlString(arg_1.to_string()?),
        )
    }

    // https://dom.spec.whatwg.org/#dom-element-getelementsbyclassname
    fn get_elements_by_class_name(
        &self,
        ctx: Ctx<'js>,
        arg_0: rquickjs::String<'js>,
    ) -> Result<Value<'js>> {
        self.get_elements_by_class_name(ctx, WebIdlString(arg_0.to_string()?))
    }

    // https://dom.spec.whatwg.org/#dom-element-matches
    fn matches(&self, ctx: Ctx<'js>, arg_0: rquickjs::String<'js>) -> Result<bool> {
        self.matches(ctx, WebIdlString(arg_0.to_string()?))
    }

    /// <https://dom.spec.whatwg.org/#dom-element-webkitmatchesselector>
    fn webkit_matches_selector(&self, ctx: Ctx<'js>, arg_0: rquickjs::String<'js>) -> Result<bool> {
        // Legacy alias of `matches(selectors)`.
        self.matches(ctx, WebIdlString(arg_0.to_string()?))
    }

    // https://dom.spec.whatwg.org/#dom-element-closest
    fn closest(&self, ctx: Ctx<'js>, arg_0: rquickjs::String<'js>) -> Result<Value<'js>> {
        self.closest(ctx, WebIdlString(arg_0.to_string()?))
    }

    // https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-innerhtml
    fn get_inner_html(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        self.inner_html(ctx)
    }

    // https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-innerhtml
    fn set_inner_html(&self, ctx: &Ctx<'js>, value: rquickjs::String<'js>) -> Result<()> {
        self.set_inner_html(ctx, LegacyNullString(value.to_string()?))
    }

    // https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-outerhtml
    fn get_outer_html(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        self.outer_html(ctx)
    }

    // https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-outerhtml
    fn set_outer_html(&self, ctx: &Ctx<'js>, value: rquickjs::String<'js>) -> Result<()> {
        self.set_outer_html(ctx, LegacyNullString(value.to_string()?))
    }

    // https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-insertadjacenthtml
    fn insert_adjacent_html(
        &self,
        ctx: Ctx<'js>,
        arg_0: rquickjs::String<'js>,
        arg_1: rquickjs::String<'js>,
    ) -> Result<()> {
        self.insert_adjacent_html(
            ctx,
            WebIdlString(arg_0.to_string()?),
            WebIdlString(arg_1.to_string()?),
        )
    }

    /// <https://dom.spec.whatwg.org/#dom-element-insertadjacentelement>
    fn insert_adjacent_element(
        &self,
        ctx: Ctx<'js>,
        arg_0: rquickjs::String<'js>,
        arg_1: Value<'js>,
    ) -> Result<Value<'js>> {
        let element = require_context_element(&ctx, self.handle.get().0)?;
        let node = require_element_argument(&ctx, &arg_1)?;
        match insert_adjacent(&ctx, element, &arg_0.to_string()?, node)? {
            Some(node) => wrap_node(&ctx, node),
            None => Ok(Value::new_null(ctx)),
        }
    }

    /// <https://dom.spec.whatwg.org/#dom-element-insertadjacenttext>
    fn insert_adjacent_text(
        &self,
        ctx: Ctx<'js>,
        arg_0: rquickjs::String<'js>,
        arg_1: rquickjs::String<'js>,
    ) -> Result<()> {
        let element = require_context_element(&ctx, self.handle.get().0)?;
        let text = {
            let world = world(&ctx)?;
            let world = world.borrow();
            let Some(mut parsed) = world.document_mut(element) else {
                return Err(Exception::throw_type(&ctx, "no document"));
            };
            let node = parsed
                .document
                .base
                .mutate()
                .create_text_node(&arg_1.to_string()?);
            NodeId {
                document: element.document,
                node,
            }
        };
        insert_adjacent(&ctx, element, &arg_0.to_string()?, text)?;
        Ok(())
    }

    // https://drafts.csswg.org/cssom-view/#dom-element-getboundingclientrect
    fn get_bounding_client_rect(&self, ctx: Ctx<'js>) -> Result<Value<'js>> {
        self.get_bounding_client_rect(ctx)
    }

    // https://drafts.csswg.org/cssom-view/#dom-element-getclientrects
    fn get_client_rects(&self, ctx: Ctx<'js>) -> Result<Value<'js>> {
        self.get_client_rects(ctx)
    }

    // https://drafts.csswg.org/cssom-view/#dom-element-checkvisibility
    fn check_visibility(
        &self,
        ctx: Ctx<'js>,
        arg_0: element_generated::CheckVisibilityOptions,
    ) -> Result<bool> {
        Ok(self.check_visibility(ctx, arg_0))
    }

    // https://drafts.csswg.org/cssom-view/#dom-element-scrollintoview
    fn scroll_into_view(
        &self,
        ctx: Ctx<'js>,
        arg_0: element_generated::BooleanOrScrollIntoViewOptions,
    ) -> Result<()> {
        let (behavior, block, inline, container) = match arg_0 {
            // `false` asks for the end edge; every other boolean is the
            // default start edge. The inline axis always defaults to nearest
            // (<https://drafts.csswg.org/cssom-view/#dom-element-scrollintoview>).
            element_generated::BooleanOrScrollIntoViewOptions::Boolean(false) => (
                element_generated::ScrollBehavior::Auto,
                element_generated::ScrollLogicalPosition::End,
                element_generated::ScrollLogicalPosition::Nearest,
                element_generated::ScrollIntoViewContainer::All,
            ),
            element_generated::BooleanOrScrollIntoViewOptions::Boolean(true) => (
                element_generated::ScrollBehavior::Auto,
                element_generated::ScrollLogicalPosition::Start,
                element_generated::ScrollLogicalPosition::Nearest,
                element_generated::ScrollIntoViewContainer::All,
            ),
            element_generated::BooleanOrScrollIntoViewOptions::ScrollIntoViewOptions(options) => (
                options.behavior,
                options.block,
                options.inline,
                options.container,
            ),
        };
        self.scroll_into_view(
            ctx,
            behavior.into(),
            block.into(),
            inline.into(),
            container.into(),
        )
    }

    // https://drafts.csswg.org/cssom-view/#dom-element-scrollleft
    fn get_scroll_left(&self, ctx: &Ctx<'js>) -> Result<f64> {
        Ok(self.scroll_left(ctx))
    }

    // https://drafts.csswg.org/#dom-element-scrollleft
    fn set_scroll_left(&self, ctx: &Ctx<'js>, value: f64) -> Result<()> {
        self.set_scroll_left(ctx, value);
        Ok(())
    }

    // https://drafts.csswg.org/cssom-view/#dom-element-scrolltop
    fn get_scroll_top(&self, ctx: &Ctx<'js>) -> Result<f64> {
        Ok(self.scroll_top(ctx))
    }

    // https://drafts.csswg.org/#dom-element-scrolltop
    fn set_scroll_top(&self, ctx: &Ctx<'js>, value: f64) -> Result<()> {
        self.set_scroll_top(ctx, value);
        Ok(())
    }

    // https://drafts.csswg.org/cssom-view/#dom-element-currentcsszoom
    fn get_current_css_zoom(&self, ctx: &Ctx<'js>) -> Result<f64> {
        self.current_css_zoom(ctx)
    }

    // https://drafts.csswg.org/cssom-view/#dom-element-clientwidth
    fn get_client_width(&self, ctx: &Ctx<'js>) -> Result<i32> {
        self.client_width(ctx)
    }

    // https://drafts.csswg.org/cssom-view/#dom-element-clientheight
    fn get_client_height(&self, ctx: &Ctx<'js>) -> Result<i32> {
        self.client_height(ctx)
    }

    // https://drafts.csswg.org/cssom-view/#dom-element-clientleft
    fn get_client_left(&self, ctx: &Ctx<'js>) -> Result<i32> {
        self.client_left(ctx)
    }

    // https://drafts.csswg.org/cssom-view/#dom-element-clienttop
    fn get_client_top(&self, ctx: &Ctx<'js>) -> Result<i32> {
        self.client_top(ctx)
    }

    // https://drafts.csswg.org/cssom-view/#dom-element-scrollwidth
    fn get_scroll_width(&self, ctx: &Ctx<'js>) -> Result<i32> {
        self.scroll_width(ctx)
    }

    // https://drafts.csswg.org/cssom-view/#dom-element-scrollheight
    fn get_scroll_height(&self, ctx: &Ctx<'js>) -> Result<i32> {
        self.scroll_height(ctx)
    }

    // https://dom.spec.whatwg.org/#dom-element-attachshadow
    fn attach_shadow(
        &self,
        ctx: Ctx<'js>,
        arg_0: element_generated::ShadowRootInit,
    ) -> Result<Value<'js>> {
        self.attach_shadow(ctx, arg_0)
    }

    // https://dom.spec.whatwg.org/#dom-element-shadowroot
    fn get_shadow_root(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        self.shadow_root(ctx)
    }

    // https://dom.spec.whatwg.org/#dom-element-tagname
    fn get_tag_name(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        let name = self.tag_name(ctx)?;
        rquickjs::String::from_str(ctx.clone(), &name)
    }

    // https://dom.spec.whatwg.org/#dom-element-localname
    fn get_local_name(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        let name = self.local_name(ctx)?;
        rquickjs::String::from_str(ctx.clone(), &name)
    }

    // https://dom.spec.whatwg.org/#dom-element-prefix
    fn get_prefix(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        let value = self.prefix(ctx)?;
        if value.is_null() || value.is_undefined() {
            Ok(None)
        } else {
            rquickjs::FromJs::from_js(ctx, value).map(Some)
        }
    }

    // https://dom.spec.whatwg.org/#dom-element-namespaceuri
    fn get_namespace_uri(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        let value = self.namespace_uri(ctx)?;
        if value.is_null() || value.is_undefined() {
            Ok(None)
        } else {
            rquickjs::FromJs::from_js(ctx, value).map(Some)
        }
    }

    // https://dom.spec.whatwg.org/#dom-element-id
    fn get_id(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        let id = self.id(ctx)?;
        rquickjs::String::from_str(ctx.clone(), &id)
    }

    // https://dom.spec.whatwg.org/#dom-element-id
    fn set_id(&self, ctx: &Ctx<'js>, value: rquickjs::String<'js>) -> Result<()> {
        self.set_id(ctx, WebIdlString(js_string_to_utf8(&value)?))
    }

    // https://dom.spec.whatwg.org/#dom-element-slot
    fn get_slot(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        let slot = self.slot(ctx)?;
        rquickjs::String::from_str(ctx.clone(), &slot)
    }

    // https://dom.spec.whatwg.org/#dom-element-slot
    fn set_slot(&self, ctx: &Ctx<'js>, value: rquickjs::String<'js>) -> Result<()> {
        self.set_slot(ctx, WebIdlString(js_string_to_utf8(&value)?))
    }

    // https://dom.spec.whatwg.org/#dom-element-classname
    fn get_class_name(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        let name = self.class_name(ctx)?;
        rquickjs::String::from_str(ctx.clone(), &name)
    }

    // https://dom.spec.whatwg.org/#dom-element-classname
    fn set_class_name(&self, ctx: &Ctx<'js>, value: rquickjs::String<'js>) -> Result<()> {
        self.set_class_name(ctx, WebIdlString(value.to_string()?))
    }

    // https://dom.spec.whatwg.org/#dom-element-classlist
    fn get_class_list(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        self.class_list(ctx)
    }

    // https://dom.spec.whatwg.org/#dom-element-getattribute
    fn get_attribute(
        &self,
        ctx: Ctx<'js>,
        arg_0: rquickjs::String<'js>,
    ) -> Result<Option<rquickjs::String<'js>>> {
        let value = self.get_attribute(ctx.clone(), WebIdlString(arg_0.to_string()?))?;
        if value.is_null() || value.is_undefined() {
            Ok(None)
        } else {
            rquickjs::FromJs::from_js(&ctx, value).map(Some)
        }
    }

    // https://dom.spec.whatwg.org/#dom-element-setattribute
    fn set_attribute(
        &self,
        ctx: Ctx<'js>,
        arg_0: rquickjs::String<'js>,
        arg_1: rquickjs::String<'js>,
    ) -> Result<()> {
        let units = crate::dom_string::DomString::from_utf16(arg_1.to_utf16()?);
        let name = arg_0.to_string()?;
        self.set_attribute(
            ctx.clone(),
            WebIdlString(name.clone()),
            WebIdlString(js_string_to_utf8(&arg_1)?),
        )?;
        // The tree attribute drops unpaired surrogates so CSS `id` matching
        // stays distinct from U+FFFD. The getter returns the original units
        // (<https://infra.spec.whatwg.org/#javascript-string-convert>).
        if units.has_unpaired_surrogate() {
            let local = attribute_local_name(&ctx, self.handle.get().0, &name);
            let world = world(&ctx)?;
            if let Some(mut parsed) = world.borrow().document_mut(self.handle.get().0) {
                parsed
                    .document
                    .remember_attribute(self.handle.get().0.node, &local, units);
            }
        }
        Ok(())
    }

    // https://dom.spec.whatwg.org/#dom-element-hasattribute
    fn has_attribute(&self, ctx: Ctx<'js>, arg_0: rquickjs::String<'js>) -> Result<bool> {
        self.has_attribute(ctx, WebIdlString(arg_0.to_string()?))
    }

    // https://dom.spec.whatwg.org/#dom-element-removeattribute
    fn remove_attribute(&self, ctx: Ctx<'js>, arg_0: rquickjs::String<'js>) -> Result<()> {
        self.remove_attribute(ctx, WebIdlString(arg_0.to_string()?))
    }

    // https://dom.spec.whatwg.org/#dom-element-toggleattribute
    fn toggle_attribute(
        &self,
        ctx: Ctx<'js>,
        arg_0: rquickjs::String<'js>,
        arg_1: Option<bool>,
    ) -> Result<bool> {
        self.toggle_attribute(ctx, WebIdlString(arg_0.to_string()?), arg_1)
    }

    // https://dom.spec.whatwg.org/#dom-element-getattributens
    fn get_attribute_ns(
        &self,
        ctx: Ctx<'js>,
        arg_0: Option<rquickjs::String<'js>>,
        arg_1: rquickjs::String<'js>,
    ) -> Result<Option<rquickjs::String<'js>>> {
        let value = self.get_attribute_ns(
            ctx.clone(),
            OptString(arg_0.map(|name| name.to_string()).transpose()?),
            WebIdlString(arg_1.to_string()?),
        )?;
        if value.is_null() || value.is_undefined() {
            Ok(None)
        } else {
            rquickjs::FromJs::from_js(&ctx, value).map(Some)
        }
    }

    // https://dom.spec.whatwg.org/#dom-element-setattributens
    fn set_attribute_ns(
        &self,
        ctx: Ctx<'js>,
        arg_0: Option<rquickjs::String<'js>>,
        arg_1: rquickjs::String<'js>,
        arg_2: rquickjs::String<'js>,
    ) -> Result<()> {
        self.set_attribute_ns(
            ctx,
            OptString(arg_0.map(|name| name.to_string()).transpose()?),
            WebIdlString(arg_1.to_string()?),
            WebIdlString(arg_2.to_string()?),
        )
    }

    // https://dom.spec.whatwg.org/#dom-element-hasattributens
    fn has_attribute_ns(
        &self,
        ctx: Ctx<'js>,
        arg_0: Option<rquickjs::String<'js>>,
        arg_1: rquickjs::String<'js>,
    ) -> Result<bool> {
        self.has_attribute_ns(
            ctx,
            OptString(arg_0.map(|name| name.to_string()).transpose()?),
            WebIdlString(arg_1.to_string()?),
        )
    }

    // https://dom.spec.whatwg.org/#dom-element-removeattributens
    fn remove_attribute_ns(
        &self,
        ctx: Ctx<'js>,
        arg_0: Option<rquickjs::String<'js>>,
        arg_1: rquickjs::String<'js>,
    ) -> Result<()> {
        self.remove_attribute_ns(
            ctx,
            OptString(arg_0.map(|name| name.to_string()).transpose()?),
            WebIdlString(arg_1.to_string()?),
        )
    }

    // https://dom.spec.whatwg.org/#dom-element-getattributenames
    fn get_attribute_names(&self, ctx: Ctx<'js>) -> Result<Vec<String>> {
        self.get_attribute_names(ctx)
    }

    // https://dom.spec.whatwg.org/#dom-element-attributes
    fn get_attributes(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        self.attributes(ctx)
    }

    // https://dom.spec.whatwg.org/#dom-element-hasattributes
    fn has_attributes(&self, ctx: Ctx<'js>) -> Result<bool> {
        self.has_attributes(ctx)
    }

    // https://dom.spec.whatwg.org/#dom-element-getattributenode
    fn get_attribute_node(
        &self,
        ctx: Ctx<'js>,
        arg_0: rquickjs::String<'js>,
    ) -> Result<Value<'js>> {
        self.get_attribute_node(ctx, WebIdlString(arg_0.to_string()?))
    }

    // https://dom.spec.whatwg.org/#dom-element-getattributenodens
    fn get_attribute_node_ns(
        &self,
        ctx: Ctx<'js>,
        arg_0: Option<rquickjs::String<'js>>,
        arg_1: rquickjs::String<'js>,
    ) -> Result<Value<'js>> {
        self.get_attribute_node_ns(
            ctx,
            OptString(arg_0.map(|name| name.to_string()).transpose()?),
            WebIdlString(arg_1.to_string()?),
        )
    }

    // https://dom.spec.whatwg.org/#dom-element-setattributenode
    fn set_attribute_node(&self, ctx: Ctx<'js>, arg_0: Value<'js>) -> Result<Value<'js>> {
        let attr = AttrArgument::from_value(&arg_0)?;
        self.set_attribute_node(ctx, attr)
    }

    // https://dom.spec.whatwg.org/#dom-element-setattributenodens
    fn set_attribute_node_ns(&self, ctx: Ctx<'js>, arg_0: Value<'js>) -> Result<Value<'js>> {
        let attr = AttrArgument::from_value(&arg_0)?;
        self.set_attribute_node_ns(ctx, attr)
    }

    // https://dom.spec.whatwg.org/#dom-element-removeattributenode
    fn remove_attribute_node(&self, ctx: Ctx<'js>, arg_0: Value<'js>) -> Result<Value<'js>> {
        let attr = AttrArgument::from_value(&arg_0)?;
        self.remove_attribute_node(ctx, attr)
    }
}

impl<'js> html_element_generated::HTMLElement<'js> for JsNode {
    // https://html.spec.whatwg.org/multipage/dom.html#dom-dataset
    fn get_dataset(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        self.dataset(ctx)
    }

    // The cryptographic nonce: reading answers the slot, writing sets it
    // without touching the content attribute
    // (<https://html.spec.whatwg.org/multipage/urls-and-fetching.html#dom-nonce>).
    fn get_nonce(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        let value = nonce_of(ctx, self.handle.get().0)?;
        rquickjs::String::from_str(ctx.clone(), &value)
    }

    // https://html.spec.whatwg.org/multipage/urls-and-fetching.html#dom-nonce
    fn set_nonce(&self, ctx: &Ctx<'js>, value: rquickjs::String<'js>) -> Result<()> {
        set_nonce_slot(ctx, self.handle.get().0, &value.to_string()?)
    }

    // https://html.spec.whatwg.org/multipage/interaction.html#dom-accesskeylabel
    fn get_access_key_label(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        let value = content_attr(ctx, self.handle.get().0, "accesskey");
        let label = access_key_label(value.as_deref().unwrap_or_default());
        rquickjs::String::from_str(ctx.clone(), &label)
    }

    // The translation mode: the nearest ancestor-or-self HTML element whose
    // `translate` attribute is in the yes or no state decides, while the
    // inherit state, a non-HTML element, and a missing parent all fall
    // through to translate-enabled
    // (<https://html.spec.whatwg.org/multipage/dom.html#the-translate-attribute>).
    fn get_translate(&self, ctx: &Ctx<'js>) -> Result<bool> {
        let owner = world_for_node(ctx, self.handle.get().0)?;
        let borrowed = owner.borrow();
        let Some(parsed) = borrowed.document(self.handle.get().0) else {
            return Ok(true);
        };
        let base = &parsed.document.base;
        let mut cursor = Some(self.handle.get().0.node);
        while let Some(current) = cursor {
            let Some(node) = base.get_node(current) else {
                break;
            };
            if let Some(element) = node.data.downcast_element()
                && element.name.ns == html_namespace()
            {
                match crate::dom::tree::attr(base, current, "translate") {
                    // The empty value default is the Yes state.
                    Some(value) if value.is_empty() || value.eq_ignore_ascii_case("yes") => {
                        return Ok(true);
                    }
                    Some(value) if value.eq_ignore_ascii_case("no") => return Ok(false),
                    // The missing and invalid value defaults inherit.
                    _ => {}
                }
            }
            cursor = node.parent;
        }
        Ok(true)
    }

    // https://html.spec.whatwg.org/multipage/dom.html#the-translate-attribute
    fn set_translate(&self, ctx: &Ctx<'js>, value: bool) -> Result<()> {
        set_attribute_sync(
            ctx,
            self.handle.get().0,
            "translate",
            if value { "yes" } else { "no" },
        )
    }

    // The computed writing-suggestions value: the nearest ancestor-or-self
    // with the attribute decides (`false` only for an ASCII
    // case-insensitive "false"; any other value means true), defaulting to
    // true
    // (<https://html.spec.whatwg.org/multipage/editing.html#dom-writingsuggestions>).
    fn get_writing_suggestions(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        let owner = world_for_node(ctx, self.handle.get().0)?;
        let borrowed = owner.borrow();
        let Some(parsed) = borrowed.document(self.handle.get().0) else {
            return rquickjs::String::from_str(ctx.clone(), "true");
        };
        let base = &parsed.document.base;
        rquickjs::String::from_str(
            ctx.clone(),
            writing_suggestions_value(base, self.handle.get().0.node),
        )
    }

    // The `autocapitalize` state keyword: `off`/`none` read "none", `on`/
    // `sentences` read "sentences", missing or empty reads "", and anything
    // else reads "sentences" (the invalid value default)
    // (<https://html.spec.whatwg.org/multipage/editing.html#dom-autocapitalize>).
    fn get_autocapitalize(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        let raw = content_attr(ctx, self.handle.get().0, "autocapitalize");
        rquickjs::String::from_str(ctx.clone(), autocapitalize_keyword(raw.as_deref()))
    }

    // The `tabindex` content attribute parses as an integer; absence,
    // parse failure, or overflow reads as the element default (0 for the
    // focusable locals, -1 otherwise)
    // (<https://html.spec.whatwg.org/multipage/interaction.html#dom-tabindex>).
    fn get_tab_index(&self, ctx: &Ctx<'js>) -> Result<i32> {
        let owner = world_for_node(ctx, self.handle.get().0)?;
        let borrowed = owner.borrow();
        let Some(parsed) = borrowed.document(self.handle.get().0) else {
            return Ok(-1);
        };
        let base = &parsed.document.base;
        Ok(
            crate::dom::tree::attr(base, self.handle.get().0.node, "tabindex")
                .and_then(crate::dom::parse::parse_integer)
                .unwrap_or_else(|| tab_index_default(base, self.handle.get().0.node)),
        )
    }

    // https://html.spec.whatwg.org/multipage/interaction.html#dom-click
    fn click(&self, ctx: Ctx<'js>) -> Result<()> {
        element_click(&ctx, self.handle.get().0)
    }

    // https://html.spec.whatwg.org/multipage/interaction.html#dom-focus
    fn focus(&self, ctx: Ctx<'js>, _options: html_element_generated::FocusOptions) -> Result<()> {
        focus_element(&ctx, self.handle.get().0)
    }

    // https://html.spec.whatwg.org/multipage/interaction.html#dom-blur
    fn blur(&self, ctx: Ctx<'js>) -> Result<()> {
        blur_node(&ctx, self.handle.get().0)
    }

    // https://drafts.csswg.org/cssom-view/#dom-htmlelement-offsetparent
    fn get_offset_parent(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        super::offset::offset_parent(ctx, self.handle.get().0)
    }

    // https://drafts.csswg.org/cssom-view/#dom-htmlelement-offsetleft
    fn get_offset_left(&self, ctx: &Ctx<'js>) -> Result<i32> {
        Ok(super::offset::offset_left(ctx, self.handle.get().0))
    }

    // https://drafts.csswg.org/cssom-view/#dom-htmlelement-offsettop
    fn get_offset_top(&self, ctx: &Ctx<'js>) -> Result<i32> {
        Ok(super::offset::offset_top(ctx, self.handle.get().0))
    }

    // https://drafts.csswg.org/cssom-view/#dom-htmlelement-offsetwidth
    fn get_offset_width(&self, ctx: &Ctx<'js>) -> Result<i32> {
        Ok(super::offset::offset_width(ctx, self.handle.get().0))
    }

    // https://drafts.csswg.org/cssom-view/#dom-htmlelement-offsetheight
    fn get_offset_height(&self, ctx: &Ctx<'js>) -> Result<i32> {
        Ok(super::offset::offset_height(ctx, self.handle.get().0))
    }

    // https://html.spec.whatwg.org/multipage/dom.html#the-innertext-idl-attribute
    fn get_inner_text(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        self.get_inner_text(ctx)
    }

    // https://html.spec.whatwg.org/multipage/dom.html#the-innertext-idl-attribute
    fn set_inner_text(&self, ctx: &Ctx<'js>, value: rquickjs::String<'js>) -> Result<()> {
        self.set_inner_text(ctx, LegacyNullString(value.to_string()?))
    }

    // https://html.spec.whatwg.org/multipage/dom.html#the-innertext-idl-attribute
    fn get_outer_text(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        self.get_outer_text(ctx)
    }

    // https://html.spec.whatwg.org/multipage/dom.html#the-innertext-idl-attribute
    fn set_outer_text(&self, ctx: &Ctx<'js>, value: rquickjs::String<'js>) -> Result<()> {
        self.set_outer_text(ctx, LegacyNullString(value.to_string()?))
    }
}

impl<'js> svg_element_generated::SVGElement<'js> for JsNode {
    // https://html.spec.whatwg.org/multipage/dom.html#dom-dataset
    fn get_dataset(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        self.dataset(ctx)
    }

    // https://html.spec.whatwg.org/multipage/urls-and-fetching.html#dom-nonce
    fn get_nonce(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        let value = nonce_of(ctx, self.handle.get().0)?;
        rquickjs::String::from_str(ctx.clone(), &value)
    }

    // https://html.spec.whatwg.org/multipage/urls-and-fetching.html#dom-nonce
    fn set_nonce(&self, ctx: &Ctx<'js>, value: rquickjs::String<'js>) -> Result<()> {
        set_nonce_slot(ctx, self.handle.get().0, &value.to_string()?)
    }

    // https://html.spec.whatwg.org/multipage/interaction.html#dom-focus
    fn focus(&self, ctx: Ctx<'js>, _options: svg_element_generated::FocusOptions) -> Result<()> {
        focus_element(&ctx, self.handle.get().0)
    }

    // https://html.spec.whatwg.org/multipage/interaction.html#dom-blur
    fn blur(&self, ctx: Ctx<'js>) -> Result<()> {
        blur_node(&ctx, self.handle.get().0)
    }
}

impl<'js> math_ml_element_generated::MathMLElement<'js> for JsNode {
    // https://html.spec.whatwg.org/multipage/dom.html#dom-dataset
    fn get_dataset(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        self.dataset(ctx)
    }

    // https://html.spec.whatwg.org/multipage/urls-and-fetching.html#dom-nonce
    fn get_nonce(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        let value = nonce_of(ctx, self.handle.get().0)?;
        rquickjs::String::from_str(ctx.clone(), &value)
    }

    // https://html.spec.whatwg.org/multipage/urls-and-fetching.html#dom-nonce
    fn set_nonce(&self, ctx: &Ctx<'js>, value: rquickjs::String<'js>) -> Result<()> {
        set_nonce_slot(ctx, self.handle.get().0, &value.to_string()?)
    }

    // https://html.spec.whatwg.org/multipage/interaction.html#dom-focus
    fn focus(
        &self,
        ctx: Ctx<'js>,
        _options: math_ml_element_generated::FocusOptions,
    ) -> Result<()> {
        focus_element(&ctx, self.handle.get().0)
    }

    // https://html.spec.whatwg.org/multipage/interaction.html#dom-blur
    fn blur(&self, ctx: Ctx<'js>) -> Result<()> {
        blur_node(&ctx, self.handle.get().0)
    }
}

/// `ARIAMixin` is a spec mixin included by `Element`: nullable string
/// reflection over `role` and the `aria-*` content attributes
/// (<https://w3c.github.io/aria/#introstates>).
impl<'js> aria_mixin_generated::ARIAMixin<'js> for JsNode {
    // https://w3c.github.io/aria/#aria-activedescendant
    fn get_aria_active_descendant_element(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        get_element_reference(ctx, self.handle.get().0, "aria-activedescendant")
    }

    // https://w3c.github.io/aria/#aria-activedescendant
    fn set_aria_active_descendant_element(&self, ctx: &Ctx<'js>, value: Value<'js>) -> Result<()> {
        set_element_reference(ctx, self.handle.get().0, "aria-activedescendant", value)
    }

    // https://w3c.github.io/aria/#aria-controls
    fn get_aria_controls_elements(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        get_element_reference_list(ctx, self.handle.get().0, "aria-controls")
    }

    // https://w3c.github.io/aria/#aria-controls
    fn set_aria_controls_elements(&self, ctx: &Ctx<'js>, value: Value<'js>) -> Result<()> {
        set_element_reference_list(ctx, self.handle.get().0, "aria-controls", value)
    }

    // https://w3c.github.io/aria/#aria-describedby
    fn get_aria_described_by_elements(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        get_element_reference_list(ctx, self.handle.get().0, "aria-describedby")
    }

    // https://w3c.github.io/aria/#aria-describedby
    fn set_aria_described_by_elements(&self, ctx: &Ctx<'js>, value: Value<'js>) -> Result<()> {
        set_element_reference_list(ctx, self.handle.get().0, "aria-describedby", value)
    }

    // https://w3c.github.io/aria/#aria-details
    fn get_aria_details_elements(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        get_element_reference_list(ctx, self.handle.get().0, "aria-details")
    }

    // https://w3c.github.io/aria/#aria-details
    fn set_aria_details_elements(&self, ctx: &Ctx<'js>, value: Value<'js>) -> Result<()> {
        set_element_reference_list(ctx, self.handle.get().0, "aria-details", value)
    }

    // https://w3c.github.io/aria/#aria-errormessage
    fn get_aria_error_message_elements(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        get_element_reference_list(ctx, self.handle.get().0, "aria-errormessage")
    }

    // https://w3c.github.io/aria/#aria-errormessage
    fn set_aria_error_message_elements(&self, ctx: &Ctx<'js>, value: Value<'js>) -> Result<()> {
        set_element_reference_list(ctx, self.handle.get().0, "aria-errormessage", value)
    }

    // https://w3c.github.io/aria/#aria-flowto
    fn get_aria_flow_to_elements(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        get_element_reference_list(ctx, self.handle.get().0, "aria-flowto")
    }

    // https://w3c.github.io/aria/#aria-flowto
    fn set_aria_flow_to_elements(&self, ctx: &Ctx<'js>, value: Value<'js>) -> Result<()> {
        set_element_reference_list(ctx, self.handle.get().0, "aria-flowto", value)
    }

    // https://w3c.github.io/aria/#aria-labelledby
    fn get_aria_labelled_by_elements(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        get_element_reference_list(ctx, self.handle.get().0, "aria-labelledby")
    }

    // https://w3c.github.io/aria/#aria-labelledby
    fn set_aria_labelled_by_elements(&self, ctx: &Ctx<'js>, value: Value<'js>) -> Result<()> {
        set_element_reference_list(ctx, self.handle.get().0, "aria-labelledby", value)
    }

    // https://w3c.github.io/aria/#aria-owns
    fn get_aria_owns_elements(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        get_element_reference_list(ctx, self.handle.get().0, "aria-owns")
    }

    // https://w3c.github.io/aria/#aria-owns
    fn set_aria_owns_elements(&self, ctx: &Ctx<'js>, value: Value<'js>) -> Result<()> {
        set_element_reference_list(ctx, self.handle.get().0, "aria-owns", value)
    }

    // https://w3c.github.io/aria/#role
    fn get_role(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "role")
    }

    // https://w3c.github.io/aria/#role
    fn set_role(&self, ctx: &Ctx<'js>, value: Option<rquickjs::String<'js>>) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "role", value)
    }

    // https://w3c.github.io/aria/#aria-braillelabel
    fn get_aria_braille_label(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-braillelabel")
    }

    // https://w3c.github.io/aria/#aria-braillelabel
    fn set_aria_braille_label(
        &self,
        ctx: &Ctx<'js>,
        value: Option<rquickjs::String<'js>>,
    ) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-braillelabel", value)
    }

    // https://w3c.github.io/aria/#aria-brailleroledescription
    fn get_aria_braille_role_description(
        &self,
        ctx: &Ctx<'js>,
    ) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-brailleroledescription")
    }

    // https://w3c.github.io/aria/#aria-brailleroledescription
    fn set_aria_braille_role_description(
        &self,
        ctx: &Ctx<'js>,
        value: Option<rquickjs::String<'js>>,
    ) -> Result<()> {
        aria_set(
            ctx,
            self.handle.get().0,
            "aria-brailleroledescription",
            value,
        )
    }

    // https://w3c.github.io/aria/#aria-colcount
    fn get_aria_col_count(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-colcount")
    }

    // https://w3c.github.io/aria/#aria-colcount
    fn set_aria_col_count(
        &self,
        ctx: &Ctx<'js>,
        value: Option<rquickjs::String<'js>>,
    ) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-colcount", value)
    }

    // https://w3c.github.io/aria/#aria-colindex
    fn get_aria_col_index(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-colindex")
    }

    // https://w3c.github.io/aria/#aria-colindex
    fn set_aria_col_index(
        &self,
        ctx: &Ctx<'js>,
        value: Option<rquickjs::String<'js>>,
    ) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-colindex", value)
    }

    // https://w3c.github.io/aria/#aria-colindextext
    fn get_aria_col_index_text(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-colindextext")
    }

    // https://w3c.github.io/aria/#aria-colindextext
    fn set_aria_col_index_text(
        &self,
        ctx: &Ctx<'js>,
        value: Option<rquickjs::String<'js>>,
    ) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-colindextext", value)
    }

    // https://w3c.github.io/aria/#aria-colspan
    fn get_aria_col_span(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-colspan")
    }

    // https://w3c.github.io/aria/#aria-colspan
    fn set_aria_col_span(
        &self,
        ctx: &Ctx<'js>,
        value: Option<rquickjs::String<'js>>,
    ) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-colspan", value)
    }

    // https://w3c.github.io/aria/#aria-description
    fn get_aria_description(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-description")
    }

    // https://w3c.github.io/aria/#aria-description
    fn set_aria_description(
        &self,
        ctx: &Ctx<'js>,
        value: Option<rquickjs::String<'js>>,
    ) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-description", value)
    }

    // https://w3c.github.io/aria/#aria-keyshortcuts
    fn get_aria_key_shortcuts(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-keyshortcuts")
    }

    // https://w3c.github.io/aria/#aria-keyshortcuts
    fn set_aria_key_shortcuts(
        &self,
        ctx: &Ctx<'js>,
        value: Option<rquickjs::String<'js>>,
    ) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-keyshortcuts", value)
    }

    // https://w3c.github.io/aria/#aria-label
    fn get_aria_label(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-label")
    }

    // https://w3c.github.io/aria/#aria-label
    fn set_aria_label(&self, ctx: &Ctx<'js>, value: Option<rquickjs::String<'js>>) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-label", value)
    }

    // https://w3c.github.io/aria/#aria-level
    fn get_aria_level(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-level")
    }

    // https://w3c.github.io/aria/#aria-level
    fn set_aria_level(&self, ctx: &Ctx<'js>, value: Option<rquickjs::String<'js>>) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-level", value)
    }

    // https://w3c.github.io/aria/#aria-placeholder
    fn get_aria_placeholder(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-placeholder")
    }

    // https://w3c.github.io/aria/#aria-placeholder
    fn set_aria_placeholder(
        &self,
        ctx: &Ctx<'js>,
        value: Option<rquickjs::String<'js>>,
    ) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-placeholder", value)
    }

    // https://w3c.github.io/aria/#aria-posinset
    fn get_aria_pos_in_set(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-posinset")
    }

    // https://w3c.github.io/aria/#aria-posinset
    fn set_aria_pos_in_set(
        &self,
        ctx: &Ctx<'js>,
        value: Option<rquickjs::String<'js>>,
    ) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-posinset", value)
    }

    // https://w3c.github.io/aria/#aria-relevant
    fn get_aria_relevant(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-relevant")
    }

    // https://w3c.github.io/aria/#aria-relevant
    fn set_aria_relevant(
        &self,
        ctx: &Ctx<'js>,
        value: Option<rquickjs::String<'js>>,
    ) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-relevant", value)
    }

    // https://w3c.github.io/aria/#aria-roledescription
    fn get_aria_role_description(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-roledescription")
    }

    // https://w3c.github.io/aria/#aria-roledescription
    fn set_aria_role_description(
        &self,
        ctx: &Ctx<'js>,
        value: Option<rquickjs::String<'js>>,
    ) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-roledescription", value)
    }

    // https://w3c.github.io/aria/#aria-rowcount
    fn get_aria_row_count(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-rowcount")
    }

    // https://w3c.github.io/aria/#aria-rowcount
    fn set_aria_row_count(
        &self,
        ctx: &Ctx<'js>,
        value: Option<rquickjs::String<'js>>,
    ) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-rowcount", value)
    }

    // https://w3c.github.io/aria/#aria-rowindex
    fn get_aria_row_index(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-rowindex")
    }

    // https://w3c.github.io/aria/#aria-rowindex
    fn set_aria_row_index(
        &self,
        ctx: &Ctx<'js>,
        value: Option<rquickjs::String<'js>>,
    ) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-rowindex", value)
    }

    // https://w3c.github.io/aria/#aria-rowindextext
    fn get_aria_row_index_text(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-rowindextext")
    }

    // https://w3c.github.io/aria/#aria-rowindextext
    fn set_aria_row_index_text(
        &self,
        ctx: &Ctx<'js>,
        value: Option<rquickjs::String<'js>>,
    ) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-rowindextext", value)
    }

    // https://w3c.github.io/aria/#aria-rowspan
    fn get_aria_row_span(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-rowspan")
    }

    // https://w3c.github.io/aria/#aria-rowspan
    fn set_aria_row_span(
        &self,
        ctx: &Ctx<'js>,
        value: Option<rquickjs::String<'js>>,
    ) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-rowspan", value)
    }

    // https://w3c.github.io/aria/#aria-setsize
    fn get_aria_set_size(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-setsize")
    }

    // https://w3c.github.io/aria/#aria-setsize
    fn set_aria_set_size(
        &self,
        ctx: &Ctx<'js>,
        value: Option<rquickjs::String<'js>>,
    ) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-setsize", value)
    }

    // https://w3c.github.io/aria/#aria-valuemax
    fn get_aria_value_max(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-valuemax")
    }

    // https://w3c.github.io/aria/#aria-valuemax
    fn set_aria_value_max(
        &self,
        ctx: &Ctx<'js>,
        value: Option<rquickjs::String<'js>>,
    ) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-valuemax", value)
    }

    // https://w3c.github.io/aria/#aria-valuemin
    fn get_aria_value_min(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-valuemin")
    }

    // https://w3c.github.io/aria/#aria-valuemin
    fn set_aria_value_min(
        &self,
        ctx: &Ctx<'js>,
        value: Option<rquickjs::String<'js>>,
    ) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-valuemin", value)
    }

    // https://w3c.github.io/aria/#aria-valuenow
    fn get_aria_value_now(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-valuenow")
    }

    // https://w3c.github.io/aria/#aria-valuenow
    fn set_aria_value_now(
        &self,
        ctx: &Ctx<'js>,
        value: Option<rquickjs::String<'js>>,
    ) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-valuenow", value)
    }

    // https://w3c.github.io/aria/#aria-valuetext
    fn get_aria_value_text(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        aria_get(ctx, self.handle.get().0, "aria-valuetext")
    }

    // https://w3c.github.io/aria/#aria-valuetext
    fn set_aria_value_text(
        &self,
        ctx: &Ctx<'js>,
        value: Option<rquickjs::String<'js>>,
    ) -> Result<()> {
        aria_set(ctx, self.handle.get().0, "aria-valuetext", value)
    }
}

/// `ElementCSSInlineStyle` is a spec mixin included by `HTMLElement`,
/// `SVGElement`, and `MathMLElement`, so the one contract installs on all
/// three prototypes from the IDL includes
/// (<https://drafts.csswg.org/cssom/#the-elementcssinlinestyle-mixin>).
impl<'js> element_css_inline_style_generated::ElementCSSInlineStyle<'js> for JsNode {
    // https://drafts.csswg.org/cssom/#dom-elementcssinlinestyle-style
    fn get_style(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        self.style(ctx)
    }
}

// `ElementContentEditable` is a spec mixin included by `HTMLElement`: the
// enumerated `enterkeyhint`/`inputmode` are generated from the metadata table
// rather than written here
// (<https://html.spec.whatwg.org/multipage/interaction.html#attr-enterkeyhint>).

/// `HTMLHyperlinkElementUtils` is a spec mixin included by `HTMLAnchorElement`
/// and `HTMLAreaElement`, so the one contract installs on both prototypes
/// from the IDL includes
/// (<https://html.spec.whatwg.org/multipage/links.html#htmlhyperlinked>).
impl<'js> html_hyperlink_element_utils_generated::HTMLHyperlinkElementUtils<'js> for JsNode {
    // https://html.spec.whatwg.org/multipage/links.html#dom-hyperlink-href
    fn get_href(&self, ctx: &Ctx<'js>) -> Result<crate::dom_string::DomString> {
        host::reflect_url_string(ctx, self.handle.get().0, "href")
    }
}

impl JsNode {
    // The union of the element's box fragments relative to the viewport; zero
    // when the element generates no box (for example `display: none`).
    pub(super) fn get_bounding_client_rect<'js>(&self, ctx: Ctx<'js>) -> Result<Value<'js>> {
        match self.client_rects(&ctx, true).into_iter().next() {
            Some((left, top, width, height)) => {
                Ok(rect_object(&ctx, left, top, width, height)?.into_value())
            }
            None => Ok(rect_object(&ctx, 0.0, 0.0, 0.0, 0.0)?.into_value()),
        }
    }

    pub(super) fn get_client_rects<'js>(&self, ctx: Ctx<'js>) -> Result<Value<'js>> {
        let array = Array::new(ctx.clone())?;
        for (index, (left, top, width, height)) in
            self.client_rects(&ctx, false).into_iter().enumerate()
        {
            array.set(index, rect_object(&ctx, left, top, width, height)?)?;
        }
        Ok(array.into_value())
    }

    /// The element's box fragments relative to the viewport
    /// (<https://drafts.csswg.org/cssom-view/#dom-element-getclientrects>).
    ///
    /// A non-atomic inline is laid out as style spans in its inline root, so
    /// it has one fragment per line box; every other element has one border
    /// box. `bounding` asks for the union of those fragments, which is what
    /// `getBoundingClientRect()` reports; a union of no fragments is a zero
    /// rect. An element with no associated box (for example `display: none`)
    /// generates none.
    fn client_rects(&self, ctx: &Ctx<'_>, bounding: bool) -> Vec<(f64, f64, f64, f64)> {
        let id = self.handle.get().0;
        let Some(world) = active_world(ctx, id) else {
            return Vec::new();
        };
        let world = world.borrow();
        let Some(mut parsed) = world.document_mut(id) else {
            return Vec::new();
        };
        let base = &mut parsed.document.base;
        base.resolve(crate::animation_seconds());
        let Some(node) = base.get_node(id.node) else {
            return Vec::new();
        };
        if !has_associated_box(base, node) {
            return Vec::new();
        }
        let mut rects: Vec<(f64, f64, f64, f64)> = base
            .node_client_rects(id.node)
            .into_iter()
            .map(|rect| (rect.x, rect.y, rect.width, rect.height))
            .collect();
        // An empty non-atomic inline is flattened into its inline root and has
        // no fragment of its own, but it still has a position: the layout
        // box's origin, which is where its caret would be
        // (<https://drafts.csswg.org/css-inline-3/#invisible-line-boxes>).
        if rects.is_empty() {
            let position = node.absolute_position(0.0, 0.0);
            let size = node.final_layout().size;
            let scroll = base.viewport_scroll();
            rects.push((
                f64::from(position.x) - scroll.x,
                f64::from(position.y) - scroll.y,
                f64::from(size.width),
                f64::from(size.height),
            ));
        }
        if bounding {
            let Some((first, rest)) = rects.split_first() else {
                return Vec::new();
            };
            let mut left = first.0;
            let mut top = first.1;
            let mut right = first.0 + first.2;
            let mut bottom = first.1 + first.3;
            for rect in rest {
                left = left.min(rect.0);
                top = top.min(rect.1);
                right = right.max(rect.0 + rect.2);
                bottom = bottom.max(rect.1 + rect.3);
            }
            return vec![(left, top, right - left, bottom - top)];
        }
        rects
    }

    /// Whether this element is potentially visible, following the optional
    /// checks (<https://drafts.csswg.org/cssom-view/#dom-element-checkvisibility>).
    ///
    /// `visibility` is inherited, so this element's computed value already
    /// includes an ancestor's unless a descendant re-declares `visible`;
    /// `opacity` is not inherited, so its gate walks the element and every
    /// ancestor. `content-visibility` is not in the Servo style crate's
    /// property tables (`engine = "gecko"` in `longhands.toml`), so a hidden or
    /// auto ancestor cannot be detected (`docs/upstream.md`).
    pub(super) fn check_visibility(
        &self,
        ctx: Ctx<'_>,
        options: element_generated::CheckVisibilityOptions,
    ) -> bool {
        let id = self.handle.get().0;
        // An element whose document has no browsing context has no boxes, and
        // neither has one whose document has been replaced.
        let Some(world) = active_world(&ctx, id) else {
            return false;
        };
        let world = world.borrow();
        let Some(mut parsed) = world.document_mut(id) else {
            return false;
        };
        let base = &mut parsed.document.base;
        base.resolve(crate::animation_seconds());
        let Some(node) = base.get_node(id.node) else {
            return false;
        };
        if !has_associated_box(base, node) {
            return false;
        }

        // Each option name in the IDL is an alias of its partner
        // (<https://drafts.csswg.org/cssom-view/#dictdef-checkvisibilityoptions>).
        let opacity_requested = options.opacity_property || options.check_opacity;
        let visibility_requested = options.visibility_property || options.check_visibility_css;
        if visibility_requested
            && node
                .primary_styles()
                .is_some_and(|styles| styles.clone_visibility() != Visibility::Visible)
        {
            return false;
        }
        if opacity_requested
            && shadow_including_ancestors(base, Some(id.node)).any(|node| {
                node.primary_styles()
                    .is_some_and(|styles| styles.clone_opacity() == 0.0)
            })
        {
            return false;
        }
        true
    }

    /// Scrolls this element's boxes into view with the requested alignment
    /// (<https://drafts.csswg.org/cssom-view/#dom-element-scrollintoview>).
    ///
    /// Blitz owns the scroll engine: it scrolls every ancestor scrolling box
    /// in the containing block chain, then the viewport, and applies the
    /// requested behavior and `container` limit.
    pub(super) fn scroll_into_view(
        &self,
        ctx: Ctx<'_>,
        behavior: blitz_dom::ScrollBehavior,
        block: blitz_dom::ScrollLogicalPosition,
        inline: blitz_dom::ScrollLogicalPosition,
        container: blitz_dom::ScrollIntoViewContainer,
    ) -> Result<()> {
        let world = world_for_node(&ctx, self.handle.get().0)?;
        let world = world.borrow();
        let Some(mut parsed) = world.document_mut(self.handle.get().0) else {
            return Ok(());
        };
        let id = self.handle.get().0;
        parsed.document.base.resolve(crate::animation_seconds());
        // A boxless element has nothing to bring into view
        // (<https://drafts.csswg.org/cssom-view/#dom-element-scrollintoview>).
        if !parsed
            .document
            .base
            .get_node(id.node)
            .is_some_and(blitz_dom::Node::has_boxes)
        {
            return Ok(());
        }
        parsed
            .document
            .base
            .scroll_into_view(id.node, behavior, block, inline, container);
        Ok(())
    }

    // https://drafts.csswg.org/cssom-view/#dom-element-scrollleft
    pub(super) fn scroll_left(&self, ctx: &Ctx<'_>) -> f64 {
        self.scroll_axis(ctx, false)
    }

    pub(super) fn set_scroll_left(&self, ctx: &Ctx<'_>, value: f64) {
        self.set_scroll_axis(ctx, value, false);
    }

    // https://drafts.csswg.org/cssom-view/#dom-element-scrolltop
    pub(super) fn scroll_top(&self, ctx: &Ctx<'_>) -> f64 {
        self.scroll_axis(ctx, true)
    }

    pub(super) fn set_scroll_top(&self, ctx: &Ctx<'_>, value: f64) {
        self.set_scroll_axis(ctx, value, true);
    }

    /// One axis of `scrollTop`/`scrollLeft`, including the quirks-mode body
    /// and root-element rules
    /// (<https://drafts.csswg.org/cssom-view/#dom-element-scrolltop>).
    fn scroll_axis(&self, ctx: &Ctx<'_>, vertical: bool) -> f64 {
        super::scrolling::element_scroll_position(ctx, self.handle.get().0, vertical)
    }

    /// The `scrollTop`/`scrollLeft` setter: non-finite values become zero, and
    /// the other axis keeps its current position
    /// (<https://drafts.csswg.org/cssom-view/#dom-element-scrolltop>).
    fn set_scroll_axis(&self, ctx: &Ctx<'_>, value: f64, vertical: bool) {
        let id = self.handle.get().0;
        let current = super::scrolling::element_scroll_position(ctx, id, !vertical);
        let (x, y) = if vertical {
            (current, value)
        } else {
            (value, current)
        };
        super::scrolling::element_scroll(ctx, id, x, y, blitz_dom::ScrollBehavior::Auto);
    }

    /// Runs `read` over this wrapper's Blitz layout node after flushing
    /// pending layout; metrics are zero without one
    /// (<https://drafts.csswg.org/cssom-view/#extension-to-the-element-interface>).
    pub(super) fn layout_metric(
        &self,
        ctx: &Ctx<'_>,
        read: impl FnOnce(&blitz_dom::Node) -> f64,
    ) -> Result<f64> {
        let world = world(ctx)?;
        let world = world.borrow();
        let Some(mut parsed) = world.document_mut(self.handle.get().0) else {
            return Ok(0.0);
        };
        let base = &mut parsed.document.base;
        base.resolve(crate::animation_seconds());
        let Some(node) = base.get_node(self.handle.get().0.node) else {
            return Ok(0.0);
        };
        Ok(read(node))
    }

    /// `clientWidth`/`clientHeight` share their steps: a boxless element
    /// answers zero (a non-atomic inline's flattened box has no size of its
    /// own, so Blitz answers zero too), the root element outside quirks mode and
    /// the body element in quirks mode answer the viewport, and everything else
    /// answers its padding edge (<https://drafts.csswg.org/cssom-view/#dom-element-clientwidth>,
    /// <https://drafts.csswg.org/cssom-view/#dom-element-clientheight>).
    fn client_size(&self, ctx: &Ctx<'_>, vertical: bool) -> Result<i32> {
        let id = self.handle.get().0;
        let world = world(ctx)?;
        let world = world.borrow();
        let Some(mut parsed) = world.document_mut(id) else {
            return Ok(0);
        };
        parsed.document.base.resolve(crate::animation_seconds());
        let viewport_client = super::scrolling::client_area_is_viewport(&parsed, id);
        let base = &parsed.document.base;
        let Some(node) = base.get_node(id.node) else {
            return Ok(0);
        };
        if !has_associated_box(base, node) {
            return Ok(0);
        }
        let edge = if viewport_client {
            let (width, height) = world.viewport_size.get();
            f64::from(if vertical { height } else { width })
        } else if let Some(table) = table_offset_edge(base, id, vertical) {
            table
        } else {
            f64::from(if vertical {
                node.client_height()
            } else {
                node.client_width()
            })
        };
        Ok(cssom_round(super::zoom::unscaled(node, edge)))
    }

    // https://drafts.csswg.org/cssom-view/#dom-element-clientwidth
    pub(super) fn client_width(&self, ctx: &Ctx<'_>) -> Result<i32> {
        self.client_size(ctx, false)
    }

    // https://drafts.csswg.org/cssom-view/#dom-element-clientheight
    pub(super) fn client_height(&self, ctx: &Ctx<'_>) -> Result<i32> {
        self.client_size(ctx, true)
    }

    /// `Element.currentCSSZoom`: the element's effective zoom, or `1.0` when it
    /// is not being rendered
    /// (<https://drafts.csswg.org/cssom-view/#dom-element-currentcsszoom>).
    pub(super) fn current_css_zoom(&self, ctx: &Ctx<'_>) -> Result<f64> {
        self.layout_metric(ctx, super::zoom::effective_zoom)
    }

    // https://drafts.csswg.org/cssom-view/#dom-element-clientleft
    pub(super) fn client_left(&self, ctx: &Ctx<'_>) -> Result<i32> {
        Ok(cssom_round(self.layout_metric(ctx, |node| {
            // A table's box is the wrapper box, which has no border of its
            // own; the grid's border belongs to the table's content edge, not
            // its client edge
            // (<https://drafts.csswg.org/cssom-view/#dom-element-clientleft>).
            if crate::dom::tree::is_html_tag(Some(&node.data), "table") {
                return 0.0;
            }
            super::zoom::unscaled(node, f64::from(client_border(node, false)))
        })?))
    }

    // https://drafts.csswg.org/cssom-view/#dom-element-clienttop
    pub(super) fn client_top(&self, ctx: &Ctx<'_>) -> Result<i32> {
        Ok(cssom_round(self.layout_metric(ctx, |node| {
            if crate::dom::tree::is_html_tag(Some(&node.data), "table") {
                return 0.0;
            }
            super::zoom::unscaled(node, f64::from(client_border(node, true)))
        })?))
    }

    /// `scrollWidth`/`scrollHeight` share their steps: the root element outside
    /// quirks mode, and the body element in quirks mode when it does not scroll
    /// itself on the axis, answer the viewport's scrolling area; everything else
    /// answers its own (<https://drafts.csswg.org/cssom-view/#dom-element-scrollwidth>,
    /// <https://drafts.csswg.org/cssom-view/#dom-element-scrollheight>).
    fn scroll_size(&self, ctx: &Ctx<'_>, vertical: bool) -> Result<i32> {
        let id = self.handle.get().0;
        let world = world(ctx)?;
        let world = world.borrow();
        let Some(mut parsed) = world.document_mut(id) else {
            return Ok(0);
        };
        parsed.document.base.resolve(crate::animation_seconds());
        let Some(node) = parsed.document.base.get_node(id.node) else {
            return Ok(0);
        };
        if super::scrolling::scroll_area_is_viewport(&parsed, id, vertical) {
            let (width, height) = world.viewport_size.get();
            let viewport = f64::from(if vertical { height } else { width });
            return Ok(cssom_round(super::zoom::unscaled(
                node,
                super::scrolling::viewport_scroll_area(&parsed, vertical, viewport),
            )));
        }
        // A table's scrolling area starts from its own box (the wrapper box
        // with captions, else the grid's border box), not its padding edge,
        // and grows with whatever overflows it. The generic path would start
        // from the padding edge and drop the captions
        // (<https://drafts.csswg.org/cssom-view/#dom-element-scrollwidth>,
        // <https://drafts.csswg.org/css-tables-3/#table-wrapper-box>).
        if let Some(table) = table_offset_edge(&parsed.document.base, id, vertical) {
            // The scrolling area is at least the table's own box; Blitz's
            // scroll size covers the padding edge and any overflow beyond it
            let scroll = f64::from(if vertical {
                node.scroll_height()
            } else {
                node.scroll_width()
            });
            return Ok(cssom_round(super::zoom::unscaled(node, table.max(scroll))));
        }
        Ok(cssom_round(super::zoom::unscaled(
            node,
            f64::from(if vertical {
                node.scroll_height()
            } else {
                node.scroll_width()
            }),
        )))
    }

    // https://drafts.csswg.org/cssom-view/#dom-element-scrollwidth
    pub(super) fn scroll_width(&self, ctx: &Ctx<'_>) -> Result<i32> {
        self.scroll_size(ctx, false)
    }

    // https://drafts.csswg.org/cssom-view/#dom-element-scrollheight
    pub(super) fn scroll_height(&self, ctx: &Ctx<'_>) -> Result<i32> {
        self.scroll_size(ctx, true)
    }

    pub(super) fn get_attribute<'js>(
        &self,
        ctx: Ctx<'js>,
        name: WebIdlString,
    ) -> Result<Value<'js>> {
        if !valid_attribute_local_name(&name.0) {
            return Ok(Value::new_null(ctx));
        }
        // HTML elements match ASCII-lowercased
        // (<https://dom.spec.whatwg.org/#concept-element-attributes-get-by-name>).
        let local = attribute_local_name(&ctx, self.handle.get().0, &name.0);
        let world = world(&ctx)?;
        let found = {
            let borrowed = world.borrow();
            let Some(parsed) = borrowed.document(self.handle.get().0) else {
                return Ok(Value::new_null(ctx));
            };
            if let Some(exact) = parsed
                .document
                .attribute_units(self.handle.get().0.node, &local)
            {
                Some(exact.units().into_owned())
            } else {
                crate::dom::tree::attr_by_qualified_name(
                    &parsed.document.base,
                    self.handle.get().0.node,
                    &local,
                )
                .map(|value| value.encode_utf16().collect::<Vec<_>>())
            }
        };
        match found {
            Some(units) => Ok(rquickjs::String::from_utf16(ctx, &units)?.into_value()),
            None => Ok(Value::new_null(ctx)),
        }
    }

    pub(super) fn set_attribute(
        &self,
        ctx: Ctx<'_>,
        name: WebIdlString,
        value: WebIdlString,
    ) -> Result<()> {
        if !valid_attribute_local_name(&name.0) {
            return Err(throw_dom(
                &ctx,
                "InvalidCharacterError",
                "attribute name is not a valid attribute local name",
            ));
        }
        let local = attribute_local_name(&ctx, self.handle.get().0, &name.0);
        set_attribute_sync(&ctx, self.handle.get().0, &local, &value.0)
    }

    pub(super) fn id(&self, ctx: &Ctx<'_>) -> Result<String> {
        attribute_value(ctx, self.handle.get().0, "id")
    }

    pub(super) fn set_id(&self, ctx: &Ctx<'_>, value: WebIdlString) -> Result<()> {
        self.set_attribute(ctx.clone(), WebIdlString("id".into()), value)
    }

    pub(super) fn slot(&self, ctx: &Ctx<'_>) -> Result<String> {
        attribute_value(ctx, self.handle.get().0, "slot")
    }

    pub(super) fn set_slot(&self, ctx: &Ctx<'_>, value: WebIdlString) -> Result<()> {
        self.set_attribute(ctx.clone(), WebIdlString("slot".into()), value)
    }

    // https://dom.spec.whatwg.org/#dom-element-tagname
    pub(super) fn tag_name(&self, ctx: &Ctx<'_>) -> Result<String> {
        let uppercase = document_is_html_content(ctx, self.handle.get().0);
        with_node_data(ctx, self.handle.get().0, |data| match data {
            Some(NodeData::Element(element)) => element_node_name(&element.name, uppercase),
            _ => String::new(),
        })
    }

    // https://dom.spec.whatwg.org/#dom-element-localname
    pub(super) fn local_name(&self, ctx: &Ctx<'_>) -> Result<String> {
        with_node_data(ctx, self.handle.get().0, |data| match data {
            Some(NodeData::Element(element)) => element.name.local.to_string(),
            _ => String::new(),
        })
    }

    // https://dom.spec.whatwg.org/#dom-element-prefix
    pub(super) fn prefix<'js>(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        let prefix = with_node_data(ctx, self.handle.get().0, |data| match data {
            Some(NodeData::Element(element)) => element
                .name
                .prefix
                .as_ref()
                .filter(|prefix| !prefix.is_empty())
                .map(ToString::to_string),
            _ => None,
        })?;
        match prefix {
            Some(prefix) => string_value(ctx, &prefix),
            None => Ok(Value::new_null(ctx.clone())),
        }
    }

    // https://dom.spec.whatwg.org/#dom-element-namespaceuri
    pub(super) fn namespace_uri<'js>(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        let namespace = with_node_data(ctx, self.handle.get().0, |data| match data {
            Some(NodeData::Element(element)) => {
                (!element.name.ns.as_str().is_empty()).then(|| element.name.ns.to_string())
            }
            _ => None,
        })?;
        match namespace {
            Some(namespace) => string_value(ctx, &namespace),
            None => Ok(Value::new_null(ctx.clone())),
        }
    }

    // https://dom.spec.whatwg.org/#dom-element-classname
    pub(super) fn class_name(&self, ctx: &Ctx<'_>) -> Result<String> {
        attribute_value(ctx, self.handle.get().0, "class")
    }

    pub(super) fn set_class_name(&self, ctx: &Ctx<'_>, value: WebIdlString) -> Result<()> {
        set_attribute_sync(ctx, self.handle.get().0, "class", &value.0)
    }

    // https://dom.spec.whatwg.org/#dom-element-classlist
    pub(super) fn class_list<'js>(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        self.token_list(
            ctx,
            SameObject::ClassList,
            markup5ever::local_name!("class"),
        )
    }

    /// The `[SameObject]` `DOMTokenList` over one of the element's content
    /// attributes: the first read builds it, later reads answer the same
    /// object
    /// (<https://dom.spec.whatwg.org/#interface-domtokenlist>).
    pub(super) fn token_list<'js>(
        &self,
        ctx: &Ctx<'js>,
        member: SameObject,
        attribute: markup5ever::LocalName,
    ) -> Result<Value<'js>> {
        let world_rc = world(ctx)?;
        if let Some(saved) = world_rc.borrow().same_object(self.handle.get().0, member)
            && let Some(value) = deref_weak(ctx, saved)?
        {
            return Ok(value);
        }
        let list = host::instance_for_node(
            ctx,
            self.handle.get().0,
            JsTokenList {
                element: self.handle.get(),
                attribute,
            },
        )?;
        let value = Class::into_value(list);
        let weak = make_weak(ctx, value.clone())?;
        world_rc.borrow_mut().intern_same_object(
            self.handle.get().0,
            member,
            Persistent::save(ctx, weak),
        );
        Ok(value)
    }

    // https://html.spec.whatwg.org/multipage/dom.html#concept-domstringmap-pairs
    pub(super) fn dataset<'js>(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        let world_rc = world(ctx)?;
        if let Some(saved) = world_rc
            .borrow()
            .same_object(self.handle.get().0, SameObject::Dataset)
            && let Some(value) = deref_weak(ctx, saved)?
        {
            return Ok(value);
        }
        let factory: Function = crate::js::bridge::object(ctx)?.get("__tbMakeDataset")?;
        let element = wrap_node(ctx, self.handle.get().0)?;
        let value: Value = factory.call((element,))?;
        let weak = make_weak(ctx, value.clone())?;
        world_rc.borrow_mut().intern_same_object(
            self.handle.get().0,
            SameObject::Dataset,
            Persistent::save(ctx, weak),
        );
        Ok(value)
    }

    // https://dom.spec.whatwg.org/#dom-element-hasattribute
    pub(super) fn has_attribute(&self, ctx: Ctx<'_>, name: WebIdlString) -> Result<bool> {
        if !valid_attribute_local_name(&name.0) {
            return Ok(false);
        }
        // HTML elements match ASCII-lowercased, like `getAttribute`.
        let local = attribute_local_name(&ctx, self.handle.get().0, &name.0);
        let world = world(&ctx)?;
        let parsed = world.borrow();
        let Some(parsed) = parsed.document(self.handle.get().0) else {
            return Ok(false);
        };
        Ok(crate::dom::tree::attr_by_qualified_name(
            &parsed.document.base,
            self.handle.get().0.node,
            &local,
        )
        .is_some())
    }

    // https://dom.spec.whatwg.org/#dom-element-getattributens
    pub(super) fn get_attribute_ns<'js>(
        &self,
        ctx: Ctx<'js>,
        namespace: OptString,
        local: WebIdlString,
    ) -> Result<Value<'js>> {
        let namespace = namespace.0.unwrap_or_default();
        let world = world(&ctx)?;
        let found = world
            .borrow()
            .document(self.handle.get().0)
            .and_then(|parsed| {
                attr_ns_value(
                    &parsed.document.base,
                    self.handle.get().0.node,
                    &namespace,
                    &local.0,
                )
            });
        match found {
            Some(value) => string_value(&ctx, &value),
            None => Ok(Value::new_null(ctx)),
        }
    }

    // https://dom.spec.whatwg.org/#dom-element-hasattributens
    pub(super) fn has_attribute_ns(
        &self,
        ctx: Ctx<'_>,
        namespace: OptString,
        local: WebIdlString,
    ) -> Result<bool> {
        let namespace = namespace.0.unwrap_or_default();
        let world = world(&ctx)?;
        let parsed = world.borrow();
        let Some(parsed) = parsed.document(self.handle.get().0) else {
            return Ok(false);
        };
        Ok(attr_ns_value(
            &parsed.document.base,
            self.handle.get().0.node,
            &namespace,
            &local.0,
        )
        .is_some())
    }

    // https://dom.spec.whatwg.org/#dom-element-setattributens
    pub(super) fn set_attribute_ns(
        &self,
        ctx: Ctx<'_>,
        namespace: OptString,
        qualified: WebIdlString,
        value: WebIdlString,
    ) -> Result<()> {
        // setAttributeNS never lowercases its name, even on HTML elements
        // (<https://dom.spec.whatwg.org/#dom-element-setattributens>).
        let name = validate_and_extract(
            &ctx,
            namespace.0.as_deref(),
            &qualified.0,
            NodeContext::Attribute,
        )?;
        let namespace = name.ns.to_string();
        let local = name.local.to_string();
        let previous_value;
        {
            let world = world(&ctx)?;
            let world = world.borrow();
            let Some(mut parsed) = world.document_mut(self.handle.get().0) else {
                return Err(Exception::throw_type(&ctx, "no document"));
            };
            let old_value = attr_ns_value(
                &parsed.document.base,
                self.handle.get().0.node,
                &namespace,
                &local,
            );
            previous_value = old_value.clone();
            // Reuse the stored qualified name when the attribute is already
            // present, so the write replaces instead of duplicating it.
            let stored = parsed
                .document
                .base
                .get_node(self.handle.get().0.node)
                .and_then(|node| node.data.downcast_element())
                .and_then(|element| {
                    element
                        .attrs
                        .iter()
                        .find(|attribute| {
                            attribute.name.ns.as_str() == namespace
                                && attribute.name.local.as_str() == local
                        })
                        .map(|attribute| attribute.name.clone())
                })
                .unwrap_or(name);
            parsed
                .document
                .base
                .mutate()
                .set_attribute(self.handle.get().0.node, stored, &value.0);
            parsed.document.record(JournalEntry::Attributes {
                target: self.handle.get().0,
                // attributeName is the attribute's local name.
                // https://dom.spec.whatwg.org/#handle-attribute-changes
                name: local.clone(),
                namespace: namespace.clone(),
                old_value,
            });
        }
        touch_attr(&ctx, self.handle.get().0, &namespace, &local, &value.0)?;
        if namespace.is_empty() {
            after_attribute_change(&ctx, self.handle.get().0, &local, previous_value.as_deref())?;
        }
        schedule_mutation_delivery(&ctx)
    }

    // https://dom.spec.whatwg.org/#dom-element-removeattribute
    pub(super) fn remove_attribute(&self, ctx: Ctx<'_>, name: WebIdlString) -> Result<()> {
        if !valid_attribute_local_name(&name.0) {
            return Ok(());
        }
        let local = attribute_local_name(&ctx, self.handle.get().0, &name.0);
        remove_attribute_sync(&ctx, self.handle.get().0, "", &local, false)
    }

    // https://dom.spec.whatwg.org/#dom-element-removeattributens
    pub(super) fn remove_attribute_ns(
        &self,
        ctx: Ctx<'_>,
        namespace: OptString,
        local: WebIdlString,
    ) -> Result<()> {
        let namespace = namespace.0.unwrap_or_default();
        remove_attribute_sync(&ctx, self.handle.get().0, &namespace, &local.0, true)
    }

    // https://dom.spec.whatwg.org/#dom-element-getattributenames
    pub(super) fn get_attribute_names(&self, ctx: Ctx<'_>) -> Result<Vec<String>> {
        let world = world(&ctx)?;
        Ok(world
            .borrow()
            .document(self.handle.get().0)
            .and_then(|parsed| {
                parsed
                    .document
                    .base
                    .get_node(self.handle.get().0.node)
                    .and_then(|node| node.data.downcast_element())
                    .map(|element| {
                        element
                            .attrs
                            .iter()
                            .map(|attribute| qualified_name(&attribute.name))
                            .collect::<Vec<String>>()
                    })
            })
            .unwrap_or_default())
    }

    // https://dom.spec.whatwg.org/#dom-element-attributes
    pub(super) fn attributes<'js>(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        let world_rc = world(ctx)?;
        if let Some(saved) = world_rc
            .borrow()
            .same_object(self.handle.get().0, SameObject::Attributes)
            && let Some(value) = deref_weak(ctx, saved)?
        {
            return Ok(value);
        }
        let class = host::instance_for_node(
            ctx,
            self.handle.get().0,
            JsNamedNodeMap {
                element: self.handle.get(),
            },
        )?;
        let value = Class::into_value(class);
        let weak = make_weak(ctx, value.clone())?;
        world_rc.borrow_mut().intern_same_object(
            self.handle.get().0,
            SameObject::Attributes,
            Persistent::save(ctx, weak),
        );
        Ok(value)
    }

    // https://dom.spec.whatwg.org/#dom-element-hasattributes
    pub(super) fn has_attributes(&self, ctx: Ctx<'_>) -> Result<bool> {
        let world = world(&ctx)?;
        let parsed = world.borrow();
        let Some(parsed) = parsed.document(self.handle.get().0) else {
            return Ok(false);
        };
        Ok(parsed
            .document
            .base
            .get_node(self.handle.get().0.node)
            .and_then(|node| node.data.downcast_element())
            .is_some_and(|element| !element.attrs.is_empty()))
    }

    // https://dom.spec.whatwg.org/#dom-element-getattributenode
    pub(super) fn get_attribute_node<'js>(
        &self,
        ctx: Ctx<'js>,
        name: WebIdlString,
    ) -> Result<Value<'js>> {
        if !valid_attribute_local_name(&name.0) {
            return Ok(Value::new_null(ctx));
        }
        let name = attribute_local_name(&ctx, self.handle.get().0, &name.0);
        match attached_attr_id(&ctx, self.handle.get().0, "", &name)? {
            Some(id) => attr_wrapper(&ctx, id),
            None => Ok(Value::new_null(ctx)),
        }
    }

    // https://dom.spec.whatwg.org/#dom-element-getattributenodens
    pub(super) fn get_attribute_node_ns<'js>(
        &self,
        ctx: Ctx<'js>,
        namespace: OptString,
        local: WebIdlString,
    ) -> Result<Value<'js>> {
        let namespace = namespace.0.unwrap_or_default();
        match attached_attr_id(&ctx, self.handle.get().0, &namespace, &local.0)? {
            Some(id) => attr_wrapper(&ctx, id),
            None => Ok(Value::new_null(ctx)),
        }
    }

    // https://dom.spec.whatwg.org/#dom-element-setattributenode
    pub(super) fn set_attribute_node<'js>(
        &self,
        ctx: Ctx<'js>,
        attr: AttrArgument<'js>,
    ) -> Result<Value<'js>> {
        set_attribute_node(&ctx, self.handle.get().0, &attr.into_value())
    }

    // https://dom.spec.whatwg.org/#dom-element-setattributenodens
    pub(super) fn set_attribute_node_ns<'js>(
        &self,
        ctx: Ctx<'js>,
        attr: AttrArgument<'js>,
    ) -> Result<Value<'js>> {
        set_attribute_node(&ctx, self.handle.get().0, &attr.into_value())
    }

    // https://dom.spec.whatwg.org/#dom-element-removeattributenode
    pub(super) fn remove_attribute_node<'js>(
        &self,
        ctx: Ctx<'js>,
        attr: AttrArgument<'js>,
    ) -> Result<Value<'js>> {
        let (id, scope) = {
            let attr = attr.borrow();
            (attr.id, attr.scope.0)
        };
        let state = attr_state(&ctx, scope, id)?;
        let attached_here = attr_owner(&ctx, scope, id) == Some(self.handle.get().0) && {
            let registry = world(&ctx)?.borrow().registry();
            registry.borrow().attributes.attached(
                self.handle.get().0,
                &state.namespace,
                &state.local,
            ) == Some(id)
        };
        if !attached_here {
            return Err(throw_dom(
                &ctx,
                "NotFoundError",
                "attribute is not attached to this element",
            ));
        }
        remove_attribute_sync(
            &ctx,
            self.handle.get().0,
            &state.namespace,
            &state.local,
            true,
        )?;
        Ok(attr.into_value())
    }

    // https://dom.spec.whatwg.org/#dom-element-toggleattribute
    pub(super) fn toggle_attribute(
        &self,
        ctx: Ctx<'_>,
        name: WebIdlString,
        force: Option<bool>,
    ) -> Result<bool> {
        if !valid_attribute_local_name(&name.0) {
            return Err(throw_dom(
                &ctx,
                "InvalidCharacterError",
                "attribute name is not a valid attribute local name",
            ));
        }
        let local = attribute_local_name(&ctx, self.handle.get().0, &name.0);
        let exists = {
            let world = world(&ctx)?;
            let world = world.borrow();
            let Some(parsed) = world.document(self.handle.get().0) else {
                return Err(Exception::throw_type(&ctx, "no document"));
            };
            crate::dom::tree::attr_by_qualified_name(
                &parsed.document.base,
                self.handle.get().0.node,
                &local,
            )
            .is_some()
        };
        let should_exist = force.unwrap_or(!exists);
        if should_exist == exists {
            return Ok(should_exist);
        }
        if should_exist {
            set_attribute_sync(&ctx, self.handle.get().0, &local, "")?;
        } else {
            remove_attribute_sync(&ctx, self.handle.get().0, "", &local, false)?;
        }
        Ok(should_exist)
    }

    // https://dom.spec.whatwg.org/#dom-element-getelementsbytagnamens
    pub(super) fn get_elements_by_tag_name_ns<'js>(
        &self,
        ctx: Ctx<'js>,
        namespace: OptString,
        local: WebIdlString,
    ) -> Result<Value<'js>> {
        // The namespace variant matches the local name exactly, in HTML
        // documents too: the lowercasing rule belongs to
        // `getElementsByTagName` alone
        // (<https://dom.spec.whatwg.org/#concept-getelementsbytagnamens>).
        live_collection(
            &ctx,
            self.handle.get().0,
            CollectionKind::ElementsByTagNs {
                namespace: namespace.0.unwrap_or_default(),
                local: local.0,
            },
            Some("HTMLCollection"),
        )
    }

    // https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-element-innerhtml
    pub(super) fn inner_html<'js>(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        let container = self.handle.get().0;
        let owner = world_for_node(ctx, container)?;
        let (container, html) = {
            let world = owner.borrow();
            let Some(parsed) = world.document(container) else {
                return Err(Exception::throw_type(ctx, "no document"));
            };
            let base = &parsed.document.base;
            let Some(node) = base.get_node(container.node) else {
                return Err(Exception::throw_type(ctx, "stale node"));
            };
            // `innerHTML` belongs to `Element` and `ShadowRoot`, not to the
            // other fragment kinds
            // (<https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-element-innerhtml>).
            if node.data.downcast_element().is_none()
                && !matches!(node.data, NodeData::ShadowRoot { .. })
            {
                return Err(Exception::throw_type(ctx, "innerHTML requires an element"));
            }
            // innerHTML on a template reads its template contents, not its
            // (empty) children. After `content` is read, those nodes live in
            // the inert template-contents document
            // (<https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-element-innerhtml>,
            // <https://html.spec.whatwg.org/multipage/scripting.html#template-contents>).
            (
                template_contents_target(&parsed.document, container),
                parsed.content_type == "text/html",
            )
        };
        drop(owner);
        let markup = world(ctx)?
            .borrow()
            .with_document(container, |parsed| {
                if html {
                    serialization::serialize_html_children(&parsed.document, container.node)
                } else {
                    serialization::serialize_xml_children(&parsed.document, container.node)
                }
            })
            .unwrap_or_default();
        dom_string(ctx, &markup)
    }

    // https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-element-outerhtml
    pub(super) fn outer_html<'js>(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        let world = world(ctx)?;
        if world.borrow().document(self.handle.get().0).is_none() {
            return Err(Exception::throw_type(ctx, "no document"));
        }
        let markup = crate::document::dom::element_outer_html(&world.borrow(), self.handle.get().0)
            .ok_or_else(|| Exception::throw_type(ctx, "outerHTML requires an element"))?;
        dom_string(ctx, &markup)
    }

    // https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-element-innerhtml
    pub(super) fn set_inner_html(&self, ctx: &Ctx<'_>, value: LegacyNullString) -> Result<()> {
        self.replace_with_markup(ctx, &value.0, false)
    }

    fn replace_with_markup(&self, ctx: &Ctx<'_>, markup: &str, run_scripts: bool) -> Result<()> {
        replace_with_markup(ctx, self.handle.get().0, markup, run_scripts)
    }

    // https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-element-insertadjacenthtml
    pub(super) fn insert_adjacent_html(
        &self,
        ctx: Ctx<'_>,
        position: WebIdlString,
        text: WebIdlString,
    ) -> Result<()> {
        let position = position.0.to_ascii_lowercase();
        if !matches!(
            position.as_str(),
            "beforebegin" | "afterbegin" | "beforeend" | "afterend"
        ) {
            return Err(throw_dom(&ctx, "SyntaxError", "invalid position"));
        }
        // Adoption by copy leaves a stale handle behind; parse against the
        // live container (https://dom.spec.whatwg.org/#concept-node-adopt).
        let element = self.handle.get().0;
        let context = with_node_data(&ctx, element, |data| match data {
            Some(NodeData::Element(element)) => Some(html_fragment_context(&element.name)),
            _ => None,
        })?
        .ok_or_else(|| Exception::throw_type(&ctx, "insertAdjacentHTML requires an element"))?;
        let snapshots = {
            let (base_url, font_ctx) = fragment_base_url(&ctx, element);
            parse_html_fragment_snapshots(&text.0, &context, &base_url, font_ctx)
        };
        let world = world(&ctx)?;
        let world = world.borrow();
        let Some(mut parsed) = world.document_mut(element) else {
            return Err(Exception::throw_type(&ctx, "no document"));
        };
        let needs_parent = matches!(position.as_str(), "beforebegin" | "afterend");
        let parent = parsed
            .document
            .base
            .get_node(element.node)
            .and_then(|node| node.parent)
            .map(|node| NodeId {
                document: element.document,
                node,
            });
        if needs_parent && parent.is_none() {
            return Err(throw_dom(
                &ctx,
                "NoModificationAllowedError",
                "element has no parent",
            ));
        }
        let fragment = materialize_children(&mut parsed.document, element.document, &snapshots)
            .map_err(|err| throw_dom_error(&ctx, err))?;
        let moved = fragment_children(&parsed.document, fragment);
        match position.as_str() {
            "beforebegin" => {
                let parent = parent.ok_or_else(|| {
                    throw_dom(&ctx, "NoModificationAllowedError", "element has no parent")
                })?;
                for child in &moved {
                    place_journaled(&mut parsed, parent, *child, Some(element));
                }
            }
            _ => place_adjacent_rest(&ctx, &mut parsed, position.as_str(), element, moved.clone())?,
        }
        let controls: Vec<_> = moved
            .iter()
            .flat_map(|root| {
                crate::dom::forms::descendant_controls(
                    &parsed.document.base,
                    root.document,
                    root.node,
                )
            })
            .collect();
        drop(parsed);
        drop(world);
        fixup_controls_on_insert(&ctx, element.document, &controls)?;
        register_inserted_iframes(&ctx, &moved)?;
        // Parsed content attributes never pass through the DOM setter, so
        // their handler change steps run as part of the insertion
        // (<https://html.spec.whatwg.org/multipage/webappapis.html#event-handler-content-attributes>).
        for root in &moved {
            super::super::install_parsed_handlers(&ctx, *root)?;
        }
        // Custom elements in parsed markup upgrade, like the innerHTML and
        // outerHTML paths below.
        upgrade_parsed_tree(
            &ctx,
            if needs_parent {
                parent.ok_or_else(|| {
                    throw_dom(&ctx, "NoModificationAllowedError", "element has no parent")
                })?
            } else {
                element
            },
        )?;
        schedule_mutation_delivery(&ctx)
    }

    // https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-element-outerhtml
    pub(super) fn set_outer_html(&self, ctx: &Ctx<'_>, value: LegacyNullString) -> Result<()> {
        // Adoption by copy leaves a stale handle behind; replace the live
        // element (https://dom.spec.whatwg.org/#concept-node-adopt).
        let element = self.handle.get().0;
        let world_rc = world(ctx)?;
        let (parent, context) = {
            let world = world_rc.borrow();
            let Some(parsed) = world.document(element) else {
                return Err(Exception::throw_type(ctx, "no document"));
            };
            let base = &parsed.document.base;
            let Some(parent) = base
                .get_node(element.node)
                .and_then(|node| node.parent)
                .map(|node| NodeId {
                    document: element.document,
                    node,
                })
            else {
                // A parentless element has nothing to replace.
                return Ok(());
            };
            if base.root_node().id == parent.node {
                return Err(throw_dom(
                    ctx,
                    "NoModificationAllowedError",
                    "the parent of the element is a Document",
                ));
            }
            // A DocumentFragment has no parsing context of its own; a
            // body element stands in
            // (<https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-element-outerhtml>).
            let context = base
                .get_node(parent.node)
                .and_then(|node| node.data.downcast_element())
                .map_or_else(
                    || "body".to_owned(),
                    |element| html_fragment_context(&element.name),
                );
            (parent, context)
        };
        let snapshots = {
            let (base_url, font_ctx) = fragment_base_url(ctx, element);
            parse_html_fragment_snapshots(&value.0, &context, &base_url, font_ctx)
        };

        let world = world_rc.borrow();
        let Some(mut parsed) = world.document_mut(element) else {
            return Err(Exception::throw_type(ctx, "no document"));
        };
        let replacement = materialize_children(&mut parsed.document, element.document, &snapshots)
            .map_err(|err| throw_dom_error(ctx, err))?;
        drop(parsed);
        drop(world);
        let replacement = super::core::replace_target(ctx, parent, replacement)?;
        let world = world_rc.borrow();
        let Some(mut parsed) = world.document_mut(element) else {
            return Err(Exception::throw_type(ctx, "no document"));
        };
        let target = element;
        let added = replace_parsed(&mut parsed, parent, target, replacement);
        drop(parsed);
        drop(world);
        register_inserted_iframes(ctx, &added)?;
        // Parsed content attributes never pass through the DOM setter, so
        // their handler change steps run as part of the insertion
        // (<https://html.spec.whatwg.org/multipage/webappapis.html#event-handler-content-attributes>).
        for root in &added {
            super::super::install_parsed_handlers(ctx, *root)?;
        }
        upgrade_parsed_tree(ctx, parent)?;
        schedule_mutation_delivery(ctx)
    }

    pub(super) fn style<'js>(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        let world_rc = world(ctx)?;
        if let Some(saved) = world_rc
            .borrow()
            .same_object(self.handle.get().0, SameObject::Style)
            && let Some(value) = deref_weak(ctx, saved)?
        {
            return Ok(value);
        }
        let factory: Function = crate::js::bridge::object(ctx)?.get("__tbMakeStyle")?;
        let element = wrap_node(ctx, self.handle.get().0)?;
        let value: Value = factory.call((element,))?;
        let weak = make_weak(ctx, value.clone())?;
        world_rc.borrow_mut().intern_same_object(
            self.handle.get().0,
            SameObject::Style,
            Persistent::save(ctx, weak),
        );
        Ok(value)
    }
}
/// The value of the `(namespace, local)` attribute on `id`, if present.
pub(super) fn attr_ns_value(
    base: &blitz_dom::BaseDocument,
    id: BlitzId,
    namespace: &str,
    local: &str,
) -> Option<String> {
    base.get_node(id)
        .and_then(|node| node.data.downcast_element())
        .and_then(|element| {
            element
                .attrs
                .iter()
                .find(|attribute| {
                    attribute.name.ns.as_str() == namespace
                        && attribute.name.local.as_str() == local
                })
                .map(|attribute| attribute.value.clone())
        })
}

/// [Insert adjacent](https://dom.spec.whatwg.org/#concept-element-insert-adjacent).
///
/// Returns the inserted node, or `None` when `beforebegin` / `afterend` has
/// no parent.
pub(super) fn insert_adjacent(
    ctx: &Ctx<'_>,
    element: NodeId,
    where_: &str,
    node: NodeId,
) -> Result<Option<NodeId>> {
    let position = where_.to_ascii_lowercase();
    let (parent, reference) = match position.as_str() {
        "beforebegin" => {
            let Some(parent) = tree_parent(ctx, element)? else {
                return Ok(None);
            };
            (parent, Some(element))
        }
        "afterbegin" => {
            let world = world(ctx)?;
            let world = world.borrow();
            let Some(parsed) = world.document(element) else {
                return Err(Exception::throw_type(ctx, "no document"));
            };
            let first = parsed
                .document
                .base
                .get_node(element.node)
                .and_then(|node| node.children.first().copied())
                .map(|node| NodeId {
                    document: element.document,
                    node,
                });
            (element, first)
        }
        "beforeend" => (element, None),
        "afterend" => {
            let Some(parent) = tree_parent(ctx, element)? else {
                return Ok(None);
            };
            let world = world(ctx)?;
            let world = world.borrow();
            let Some(parsed) = world.document(element) else {
                return Err(Exception::throw_type(ctx, "no document"));
            };
            let next = sibling(&parsed.document.base, element.node, true).map(|node| NodeId {
                document: element.document,
                node,
            });
            (parent, next)
        }
        _ => {
            return Err(throw_dom(ctx, "SyntaxError", "invalid position"));
        }
    };
    let (node, reference) = insertion_tree_nodes(
        ctx,
        parent,
        NodeReference::Tree(node),
        reference.map(NodeReference::Tree),
    )?;
    let node = adopt_node(ctx, parent, node)?;
    insert_tree_node(ctx, parent, node, reference)?;
    Ok(Some(node))
}

/// The context object of an `Element` operation.
pub(super) fn require_context_element(ctx: &Ctx<'_>, id: NodeId) -> Result<NodeId> {
    let element = {
        let world = world(ctx)?;
        let world = world.borrow();
        world.document(id).is_some_and(|parsed| {
            parsed
                .document
                .base
                .get_node(id.node)
                .is_some_and(|node| node.data.downcast_element().is_some())
        })
    };
    if element {
        Ok(id)
    } else {
        Err(Exception::throw_type(ctx, "argument is not an Element"))
    }
}

/// An `Element` argument
/// (<https://webidl.spec.whatwg.org/#es-interface>).
pub(super) fn require_element_argument<'js>(ctx: &Ctx<'js>, value: &Value<'js>) -> Result<NodeId> {
    let Some(id) = host_node_id(ctx, value) else {
        return Err(Exception::throw_type(ctx, "argument is not an Element"));
    };
    require_context_element(ctx, id)
}

/// `id`'s parent, or `None` when it is parentless.
pub(super) fn tree_parent(ctx: &Ctx<'_>, id: NodeId) -> Result<Option<NodeId>> {
    let world = world(ctx)?;
    let world = world.borrow();
    let Some(parsed) = world.document(id) else {
        return Err(Exception::throw_type(ctx, "no document"));
    };
    Ok(parsed
        .document
        .base
        .get_node(id.node)
        .and_then(|node| node.parent)
        .map(|node| NodeId {
            document: id.document,
            node,
        }))
}

/// Node ids among `(Node or DOMString)` arguments, for sibling exclusion
/// (<https://dom.spec.whatwg.org/#dom-childnode-before>).
pub(super) fn argument_node_ids(nodes: &[NodeOrString<'_>]) -> HashSet<BlitzId> {
    nodes
        .iter()
        .filter_map(|node| match node {
            NodeOrString::Node(reference) => reference.tree().map(|id| id.node),
            NodeOrString::String(_) => None,
        })
        .collect()
}

/// The nearest sibling of `id` in `forward` direction that is not in
/// `excluded`.
pub(super) fn sibling_not_in(
    base: &blitz_dom::BaseDocument,
    id: BlitzId,
    forward: bool,
    excluded: &HashSet<BlitzId>,
) -> Option<BlitzId> {
    let node = base.get_node(id)?;
    let parent = node.parent?;
    let siblings = &base.get_node(parent)?.children;
    let position = siblings.iter().position(|sibling| *sibling == id)?;
    if forward {
        siblings[position + 1..]
            .iter()
            .copied()
            .find(|sibling| !excluded.contains(sibling))
    } else {
        siblings[..position]
            .iter()
            .copied()
            .rev()
            .find(|sibling| !excluded.contains(sibling))
    }
}

/// Moves focus to `id` when it is focusable
/// (<https://html.spec.whatwg.org/multipage/interaction.html#dom-focus>).
///
/// A `label` that is not itself focusable forwards the focus to its labeled
/// control: the spec leaves the label's behavior to the platform, and browsers
/// forward so that a label can be used as the control's focus proxy
/// (<https://html.spec.whatwg.org/multipage/forms.html#the-label-element>).
pub(super) fn focus_element(ctx: &Ctx<'_>, id: NodeId) -> Result<()> {
    if is_focusable(ctx, id)? {
        return focus_node(ctx, id);
    }
    let control = {
        let owner = world_for_node(ctx, id)?;
        let owner = owner.borrow();
        let Some(parsed) = owner.document(id) else {
            return Ok(());
        };
        let base = &parsed.document.base;
        if !crate::dom::tree::is_html_element(base, id.node, "label") {
            return Ok(());
        }
        crate::dom::forms::labeled_control(base, id.document, id.node)
    };
    if let Some(control) = control
        && is_focusable(ctx, control)?
    {
        focus_node(ctx, control)?;
    }
    Ok(())
}

/// One content attribute's raw value, when the attribute is present.
pub(super) fn content_attr(ctx: &Ctx<'_>, id: NodeId, name: &str) -> Option<String> {
    let owner = world_for_node(ctx, id).ok()?;
    let borrowed = owner.borrow();
    borrowed.document(id).and_then(|parsed| {
        crate::dom::tree::attr(&parsed.document.base, id.node, name).map(str::to_owned)
    })
}

/// The `accessKeyLabel` string: the platform's modifier names plus the
/// uppercased key. An attribute that does not hold exactly one key has no
/// label
/// (<https://html.spec.whatwg.org/multipage/interaction.html#dom-accesskeylabel>).
fn access_key_label(value: &str) -> String {
    let mut tokens = value.split_ascii_whitespace();
    let Some(token) = tokens.next() else {
        return String::new();
    };
    if tokens.next().is_some() {
        return String::new();
    }
    let Some(key) = token.chars().next() else {
        return String::new();
    };
    format!("Alt+Shift+{}", key.to_ascii_uppercase())
}

/// The border width `clientLeft`/`clientTop` report on one axis
/// (<https://drafts.csswg.org/cssom-view/#dom-element-clientleft>).
///
/// An element without a box, and a non-atomic inline, report zero. Blitz
/// renders scrollbars on the right and bottom only, so the "scrollbar between
/// the left padding edge and the left border edge" is always zero here.
fn client_border(node: &blitz_dom::Node, vertical: bool) -> f32 {
    if !node.has_boxes() || node.is_non_atomic_inline() {
        return 0.0;
    }
    let border = node.final_layout().border;
    if vertical { border.top } else { border.left }
}

/// Whether `data` is an element node, for
/// [`document_first_child`](super::document::document_first_child) searches.
pub(super) fn is_element_data(data: &NodeData) -> bool {
    matches!(data, NodeData::Element(_))
}

/// `unsigned long` reflection: a non-negative integer within `long` range,
/// otherwise the default
/// (<https://html.spec.whatwg.org/multipage/common-dom-interfaces.html#reflecting-content-attributes-in-idl-attributes>).
pub(super) fn reflect_ulong(raw: Option<&str>, default: u32) -> u32 {
    raw.and_then(crate::dom::parse::parse_integer)
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or(default)
}

/// Clamped `unsigned long` reflection (used by `colSpan`, `rowSpan`, `span`):
/// full-range non-negative parsing, then clamping; parse failure reads the
/// default. Unlike plain reflection the range is not capped at 2^31 - 1
/// before clamping, so `4294967296` clamps to the edge rather than the
/// default
/// (<https://html.spec.whatwg.org/multipage/common-dom-interfaces.html#reflecting-content-attributes-in-idl-attributes>).
pub(super) fn reflect_clamped_ulong(raw: Option<&str>, default: u32, min: u32, max: u32) -> u32 {
    let Some(text) = raw else {
        return default;
    };
    let mut chars = text.chars().peekable();
    while crate::dom::parse::is_html_whitespace(chars.peek()) {
        chars.next();
    }
    let mut negative = false;
    if chars.peek() == Some(&'+') {
        chars.next();
    } else if chars.peek() == Some(&'-') {
        negative = true;
        chars.next();
    }
    let mut digits = 0_u32;
    let mut result: u64 = 0;
    while let Some(digit) = chars.peek().and_then(|c| c.to_digit(10)) {
        result = result.saturating_mul(10).saturating_add(u64::from(digit));
        digits += 1;
        chars.next();
    }
    if digits == 0 {
        return default;
    }
    if result == 0 {
        return 0_u32.clamp(min, max);
    }
    if negative {
        return default;
    }
    u32::try_from(result).map_or(max, |value| value.clamp(min, max))
}

/// `double` reflection: a floating-point value or the default
/// (<https://html.spec.whatwg.org/multipage/common-dom-interfaces.html#reflecting-content-attributes-in-idl-attributes>).
pub(super) fn reflect_double(raw: Option<&str>, default: f64) -> f64 {
    raw.and_then(crate::dom::parse::parse_double)
        .unwrap_or(default)
}

/// Limited `double` reflection (used by `progress.max`): a positive
/// floating-point value or the default
/// (<https://html.spec.whatwg.org/multipage/common-dom-interfaces.html#reflecting-content-attributes-in-idl-attributes>).
pub(super) fn reflect_limited_double(raw: Option<&str>, default: f64) -> f64 {
    match raw.and_then(crate::dom::parse::parse_double) {
        Some(value) if value > 0.0 => value,
        _ => default,
    }
}

/// Whether `node` is the first `summary` element child of a `details`
/// parent: only that one is focusable
/// (<https://html.spec.whatwg.org/multipage/interaction.html#focusable-area>).
pub(crate) fn is_first_summary_child(
    base: &blitz_dom::BaseDocument,
    parent: BlitzId,
    node: BlitzId,
) -> bool {
    if !crate::dom::tree::is_html_tag(base.get_node(parent).map(|tree| &tree.data), "details") {
        return false;
    }
    base.get_node(parent).is_some_and(|parent| {
        parent.children.iter().find_map(|child| {
            base.get_node(*child).and_then(|tree| {
                tree.data.downcast_element().and_then(|element| {
                    (element.name.ns == html_namespace()
                        && element.name.local.as_str() == "summary")
                        .then_some(*child)
                })
            })
        }) == Some(node)
    })
}

/// The computed `writingSuggestions` value for `node`: the nearest
/// ancestor-or-self carrying the attribute decides, ASCII
/// case-insensitively (`false` reads "false", any other present value
/// reads "true"); with no such ancestor the value is "true"
/// (<https://html.spec.whatwg.org/multipage/editing.html#writing-suggestions>).
pub(super) fn writing_suggestions_value(
    base: &blitz_dom::BaseDocument,
    node: BlitzId,
) -> &'static str {
    let mut cursor = Some(node);
    while let Some(current) = cursor {
        if let Some(value) = crate::dom::tree::attr(base, current, "writingsuggestions") {
            return if value.eq_ignore_ascii_case("false") {
                "false"
            } else {
                "true"
            };
        }
        cursor = base.get_node(current).and_then(|tree| tree.parent);
    }
    "true"
}

/// The `autocapitalize` IDL keyword for a raw content-attribute value:
/// `off`/`none` read "none", `on`/`sentences` read "sentences", missing or
/// empty reads "", and any other value reads "sentences"
/// (<https://html.spec.whatwg.org/multipage/editing.html#autocapitalization>).
pub(super) fn autocapitalize_keyword(raw: Option<&str>) -> &'static str {
    let Some(value) = raw else {
        return "";
    };
    match value.to_ascii_lowercase().as_str() {
        "none" | "off" => "none",
        "words" => "words",
        "characters" => "characters",
        "" => "",
        // "sentences", "on", and anything else: the invalid value default.
        _ => "sentences",
    }
}

/// The `tabindex` default for one element: 0 for the focusable locals
/// (buttons, `select`/`textarea`/`iframe`, editing hosts, and the first
/// `summary` of a `details`), -1 otherwise. Connection and disabled state
/// do not move the IDL default (a detached `button` still reads 0; verified
/// live against the `WebDriver` build, which has no layout)
/// (<https://html.spec.whatwg.org/multipage/interaction.html#dom-tabindex>,
/// <https://html.spec.whatwg.org/multipage/interaction.html#focusable-area>).
///
/// This follows Chromium rather than the spec's bare local-name list: `a`
/// needs an `href`, `input type=hidden` is never a default focusable area,
/// and `audio`/`video` need `controls`. `area`, `frame`, `object`, and the
/// SVG/`MathML` `a` elements from the spec list read -1 here.
pub(super) fn tab_index_default(base: &blitz_dom::BaseDocument, node: BlitzId) -> i32 {
    let Some(tree) = base.get_node(node) else {
        return -1;
    };
    let Some(element) = tree.data.downcast_element() else {
        return -1;
    };
    if element.name.ns != html_namespace() {
        return -1;
    }
    // Editing hosts are focusable: `contenteditable` in the True or
    // Plaintext-Only state (empty, `true`, or `plaintext-only`, ASCII
    // case-insensitive).
    if crate::dom::tree::attr(base, node, "contenteditable").is_some_and(|state| {
        matches!(
            state.to_ascii_lowercase().as_str(),
            "" | "true" | "plaintext-only"
        )
    }) {
        return 0;
    }
    // The first `summary` child of a `details` is focusable.
    if element.name.local.as_str() == "summary"
        && let Some(parent) = tree.parent
        && is_first_summary_child(base, parent, node)
    {
        return 0;
    }
    match element.name.local.as_str() {
        "input" => {
            if crate::dom::tree::attr(base, node, "type")
                .is_some_and(|kind| kind.eq_ignore_ascii_case("hidden"))
            {
                -1
            } else {
                0
            }
        }
        "a" => {
            if crate::dom::tree::attr(base, node, "href").is_some() {
                0
            } else {
                -1
            }
        }
        "button" | "select" | "textarea" | "iframe" => 0,
        "audio" | "video" if crate::dom::tree::attr(base, node, "controls").is_some() => 0,
        _ => -1,
    }
}

/// The cryptographic nonce of `id`: the IDL-set slot when present, else
/// the content attribute, else empty
/// (<https://html.spec.whatwg.org/multipage/urls-and-fetching.html#nonce-attributes>).
pub(in crate::js::bindings) fn nonce_of(ctx: &Ctx<'_>, id: NodeId) -> Result<String> {
    let owner = world_for_node(ctx, id)?;
    if let Some(slot) = owner.borrow().state.nonce_slots.get(&id) {
        return Ok(slot.clone());
    }
    Ok(content_attr(ctx, id, "nonce").unwrap_or_default())
}

/// Writes the IDL `nonce` slot without touching the content attribute,
/// shared by the `HTMLElement`, `SVGElement`, and `MathMLElement` setters
/// (<https://html.spec.whatwg.org/multipage/urls-and-fetching.html#dom-nonce>).
pub(super) fn set_nonce_slot(ctx: &Ctx<'_>, id: NodeId, value: &str) -> Result<()> {
    let owner = world_for_node(ctx, id)?;
    owner
        .borrow_mut()
        .state
        .nonce_slots
        .insert(id, value.to_owned());
    Ok(())
}

/// One nullable `aria-*` (or `role`) reflection read: the content attribute
/// value, or null when absent.
pub(super) fn aria_get<'js>(
    ctx: &Ctx<'js>,
    id: NodeId,
    name: &str,
) -> Result<Option<rquickjs::String<'js>>> {
    match content_attr(ctx, id, name) {
        Some(value) => Ok(Some(rquickjs::String::from_str(ctx.clone(), &value)?)),
        None => Ok(None),
    }
}

/// One nullable `aria-*` (or `role`) reflection write: null removes the
/// content attribute.
pub(super) fn aria_set(
    ctx: &Ctx<'_>,
    id: NodeId,
    name: &str,
    value: Option<rquickjs::String<'_>>,
) -> Result<()> {
    match value {
        Some(value) => set_attribute_sync(ctx, id, name, &value.to_string()?),
        None => remove_attribute_sync(ctx, id, "", name, false),
    }
}

/// `crossorigin` reflection: absent reads null, the empty string reads
/// `anonymous`, anything else matches ASCII case-insensitively with an
/// `anonymous` invalid default; setting null removes the attribute
/// (<https://html.spec.whatwg.org/multipage/urls-and-fetching.html#dom-crossorigin>).
pub(super) fn reflect_cross_origin<'js>(
    ctx: &Ctx<'js>,
    id: NodeId,
) -> Result<Option<rquickjs::String<'js>>> {
    let Some(setting) = content_attr(ctx, id, "crossorigin")
        .as_deref()
        .map(blitz_traits::net::CorsSetting::parse)
    else {
        return Ok(None);
    };
    Ok(Some(rquickjs::String::from_str(
        ctx.clone(),
        setting.keyword(),
    )?))
}

pub(super) fn set_cross_origin(
    ctx: &Ctx<'_>,
    id: NodeId,
    value: Option<rquickjs::String<'_>>,
) -> Result<()> {
    // https://html.spec.whatwg.org/multipage/urls-and-fetching.html#dom-crossorigin
    match value {
        Some(value) => set_attribute_sync(ctx, id, "crossorigin", &value.to_string()?),
        None => remove_attribute_sync(ctx, id, "", "crossorigin", false),
    }
}

/// The node whose children are a template's contents: the inert fragment
/// after `content` is first read, otherwise the same-document fragment
/// (<https://html.spec.whatwg.org/multipage/scripting.html#template-contents>).
fn template_contents_target(doc: &crate::documents::BlitzDocument, id: NodeId) -> NodeId {
    if let Some((document, node)) = doc.template_owner(id.node) {
        return NodeId { document, node };
    }
    crate::dom::tree::template_contents(&doc.base, id.node).map_or(id, |node| NodeId {
        document: id.document,
        node,
    })
}

pub(super) fn install(ctx: &Ctx<'_>) -> Result<()> {
    element_generated::install(ctx)?;
    html_element_generated::install(ctx)?;
    svg_element_generated::install(ctx)?;
    math_ml_element_generated::install(ctx)?;
    aria_mixin_generated::install(ctx)?;
    element_css_inline_style_generated::install(ctx)?;
    element_content_editable_generated::install(ctx)?;
    html_hyperlink_element_utils_generated::install(ctx)?;
    Ok(())
}

impl JsNode {
    // https://html.spec.whatwg.org/multipage/dom.html#the-innertext-idl-attribute
    pub(super) fn get_inner_text<'js>(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        let text = super::super::inner_text::rendered_text(ctx, self.handle.get().0)?;
        rquickjs::String::from_str(ctx.clone(), &text)
    }

    // https://html.spec.whatwg.org/multipage/dom.html#the-innertext-idl-attribute
    pub(super) fn get_outer_text<'js>(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        let text = super::super::inner_text::rendered_text(ctx, self.handle.get().0)?;
        rquickjs::String::from_str(ctx.clone(), &text)
    }

    /// Setting `outerText` replaces the element with the rendered text
    /// fragment, then merges with adjacent text siblings (but does not
    /// fully normalize). Without a parent it throws; the setter is a no-op
    /// on non-HTML elements, which never install it.
    /// (<https://html.spec.whatwg.org/multipage/dom.html#the-innertext-idl-attribute>).
    #[expect(
        clippy::too_many_lines,
        reason = "outerText setter steps in spec order; splitting them is a refactor, not a lint fix"
    )]
    pub(super) fn set_outer_text(&self, ctx: &Ctx<'_>, value: LegacyNullString) -> Result<()> {
        let value = value.0;
        let parent = {
            let world = world(ctx)?;
            let world = world.borrow();
            world.document(self.handle.get().0).and_then(|parsed| {
                parsed
                    .document
                    .base
                    .get_node(self.handle.get().0.node)
                    .and_then(|node| node.parent)
                    .map(|parent| NodeId {
                        document: self.handle.get().0.document,
                        node: parent,
                    })
            })
        };
        let Some(parent) = parent else {
            return Err(throw_dom(
                ctx,
                "NoModificationAllowedError",
                "outerText needs a parent",
            ));
        };
        // Replacing the document element would put text under `Document`.
        let is_document_parent = {
            let world = world(ctx)?;
            let world = world.borrow();
            world.document(self.handle.get().0).is_some_and(|parsed| {
                parsed
                    .document
                    .base
                    .get_node(parent.node)
                    .is_some_and(|node| matches!(node.data, NodeData::Document(_)))
            })
        };
        if is_document_parent {
            return Err(throw_dom(
                ctx,
                "HierarchyRequestError",
                "outerText cannot replace the document element",
            ));
        }
        let world = world(ctx)?;
        let world = world.borrow();
        let Some(mut parsed) = world.document_mut(self.handle.get().0) else {
            return Ok(());
        };
        // Child navigables die with the replaced element: collect their
        // containers before removal, destroy after the borrows drop.
        let iframes = iframe_containers_in_subtrees(&parsed, &[self.handle.get().0]);
        let mut added = crate::dom::text::rendered_text_fragment(&mut parsed, &value, true);
        for child in &added {
            place_journaled(&mut parsed, parent, *child, Some(self.handle.get().0));
        }
        unlink_journaled(&mut parsed, self.handle.get().0);
        // Merge with the previous text sibling, then the following one.
        if let Some(first) = added.first().copied() {
            let previous = sibling(&parsed.document.base, first.node, false).map(|node| NodeId {
                document: first.document,
                node,
            });
            if let Some(previous) = previous
                && crate::dom::text::is_mergeable_text_node(&parsed.document.base, previous.node)
                && crate::dom::text::is_mergeable_text_node(&parsed.document.base, first.node)
            {
                let previous_text = String::from(parsed.document.character_data(previous.node));
                let old_value = DomString::from(previous_text.clone());
                let mut data = previous_text;
                data.push_str(&String::from(parsed.document.character_data(first.node)));
                parsed
                    .document
                    .base
                    .mutate()
                    .set_node_text(previous.node, &data);
                parsed.document.record(JournalEntry::CharacterData {
                    target: previous,
                    old_value,
                });
                unlink_journaled(&mut parsed, first);
                if added.last() == Some(&first) {
                    added[0] = previous;
                } else {
                    added.remove(0);
                }
            }
        }
        if let Some(last) = added.last().copied() {
            let next = sibling(&parsed.document.base, last.node, true).map(|node| NodeId {
                document: last.document,
                node,
            });
            if let Some(next) = next
                && crate::dom::text::is_mergeable_text_node(&parsed.document.base, next.node)
                && crate::dom::text::is_mergeable_text_node(&parsed.document.base, last.node)
            {
                let last_text = String::from(parsed.document.character_data(last.node));
                let old_value = DomString::from(last_text.clone());
                let mut data = last_text;
                data.push_str(&String::from(parsed.document.character_data(next.node)));
                parsed
                    .document
                    .base
                    .mutate()
                    .set_node_text(last.node, &data);
                parsed.document.record(JournalEntry::CharacterData {
                    target: last,
                    old_value,
                });
                unlink_journaled(&mut parsed, next);
            }
        }
        drop(parsed);
        drop(world);
        world_for_node(ctx, self.handle.get().0)?
            .borrow()
            .destroy_child_navigables(&iframes);
        fixup_focus_after_removal(ctx, self.handle.get().0)?;
        schedule_mutation_delivery(ctx)
    }

    /// Setting `innerText` replaces all children with the rendered text
    /// fragment: newlines become `br` elements with no empty text around
    /// them (an all-empty value yields one empty text node)
    /// (<https://html.spec.whatwg.org/multipage/dom.html#set-the-inner-text-steps>).
    pub(super) fn set_inner_text(&self, ctx: &Ctx<'_>, value: LegacyNullString) -> Result<()> {
        let value = value.0;
        let html = world(ctx)?
            .borrow()
            .document(self.handle.get().0)
            .is_some_and(|parsed| {
                parsed
                    .document
                    .base
                    .get_node(self.handle.get().0.node)
                    .is_some_and(|node| {
                        node.data
                            .downcast_element()
                            .is_some_and(|element| element.name.ns == html_namespace())
                    })
            });
        if !html {
            return Ok(());
        }
        let world = world(ctx)?;
        let world = world.borrow();
        let Some(mut parsed) = world.document_mut(self.handle.get().0) else {
            return Ok(());
        };
        let added = crate::dom::text::rendered_text_fragment(&mut parsed, &value, false);
        let iframes = replace_all_journaled(&mut parsed, self.handle.get().0, added);
        drop(parsed);
        world.destroy_child_navigables(&iframes);
        drop(world);
        schedule_mutation_delivery(ctx)
    }
}

impl From<element_generated::ScrollBehavior> for blitz_dom::ScrollBehavior {
    fn from(value: element_generated::ScrollBehavior) -> Self {
        match value {
            element_generated::ScrollBehavior::Auto => Self::Auto,
            element_generated::ScrollBehavior::Instant => Self::Instant,
            element_generated::ScrollBehavior::Smooth => Self::Smooth,
        }
    }
}

impl From<element_generated::ScrollIntoViewContainer> for blitz_dom::ScrollIntoViewContainer {
    fn from(value: element_generated::ScrollIntoViewContainer) -> Self {
        match value {
            element_generated::ScrollIntoViewContainer::All => Self::All,
            element_generated::ScrollIntoViewContainer::Nearest => Self::Nearest,
        }
    }
}

impl From<element_generated::ScrollLogicalPosition> for blitz_dom::ScrollLogicalPosition {
    fn from(value: element_generated::ScrollLogicalPosition) -> Self {
        match value {
            element_generated::ScrollLogicalPosition::Start => Self::Start,
            element_generated::ScrollLogicalPosition::Center => Self::Center,
            element_generated::ScrollLogicalPosition::End => Self::End,
            element_generated::ScrollLogicalPosition::Nearest => Self::Nearest,
        }
    }
}

/// A table's `clientWidth`/`clientHeight` is its `offsetWidth`/`offsetHeight`:
/// the table wrapper box when it has captions, else the grid's border box.
/// CSSOM View's generic answer is the padding edge, which for a table would
/// drop the captions and subtract the grid's own border; browsers, and the
/// spec's own table tests, report the offset size instead
/// (<https://drafts.csswg.org/cssom-view/#dom-element-clientwidth>,
/// <https://drafts.csswg.org/css-tables-3/#table-wrapper-box>).
fn table_offset_edge(
    base: &blitz_dom::BaseDocument,
    id: crate::dom::tree::NodeId,
    vertical: bool,
) -> Option<f64> {
    let node = base.get_node(id.node)?;
    if !crate::dom::tree::is_html_tag(Some(&node.data), "table") {
        return None;
    }
    base.offset_rect(id.node)
        .map(|rect| if vertical { rect.height } else { rect.width })
}
