//! Live node collections (`NodeList`, `HTMLCollection`).

use super::{install_collections_js, live_collection, world, world_for_node, wrap_node};

use rquickjs::{
    Atom, Ctx, Exception, Function, Object, Result, Value,
    class::{ExoticSetResult, Trace},
    object::Property,
    prelude::Func,
};

use crate::documents::JournalEntry;
use crate::dom::forms::{effective_selected_options, select_options};
use crate::dom::insertion::siblings_around;
use crate::dom::tree::{BlitzId, NodeId, attr, html_namespace, is_element};
use crate::js::world::Handle;
use crate::names::qualified_name_eq;
use markup5ever::{LocalName, QualName};
use std::collections::HashSet;

#[derive(Trace, rquickjs::JsLifetime)]
pub(crate) enum CollectionKind {
    Children,
    ElementChildren,
    /// `getElementsByTagName`. `html_document` is root's node document type
    /// at collection creation
    /// (<https://dom.spec.whatwg.org/#concept-getelementsbytagname>).
    ElementsByTag {
        name: String,
        html_document: bool,
    },
    ElementsByTagNs {
        namespace: String,
        local: String,
    },
    ElementsByClass(String),
    ElementsByName(String),
    SelectOptions,
    SelectedOptions,
    WindowNamed(String),
    /// `Document.scripts`: HTML `script` descendants of the document element
    /// (<https://html.spec.whatwg.org/multipage/dom.html#dom-document-scripts>).
    HtmlScripts,
    /// `Document.links`: HTML `a`/`area` descendants with an `href`
    /// (<https://html.spec.whatwg.org/multipage/dom.html#dom-document-links>).
    HtmlLinks,
    /// `table.tBodies`: HTML `tbody` children of the table
    /// (<https://html.spec.whatwg.org/multipage/tables.html#dom-table-tbodies>).
    TableBodies,
    /// `table.rows`: `tr` children of the table and of its section children,
    /// thead then body then tfoot
    /// (<https://html.spec.whatwg.org/multipage/tables.html#dom-table-rows>).
    TableRows,
    /// `thead`/`tbody`/`tfoot`.rows: `tr` children of the section
    /// (<https://html.spec.whatwg.org/multipage/tables.html#dom-tbody-rows>).
    TableSectionRows,
    /// `tr.cells`: `td`/`th` children of the row
    /// (<https://html.spec.whatwg.org/multipage/tables.html#dom-tr-cells>).
    TableRowCells,
    Static(Vec<Handle>),
}

#[derive(Trace, rquickjs::JsLifetime)]
pub(crate) struct CollectionQuery {
    pub(crate) scope: Handle,
    pub(crate) kind: CollectionKind,
}

impl CollectionQuery {
    fn ids(&self, ctx: &Ctx<'_>) -> Result<Vec<NodeId>> {
        query_ids(ctx, self.scope.0, &self.kind)
    }

    fn item<'js>(&self, ctx: &Ctx<'js>, index: usize) -> Result<Value<'js>> {
        let id = match &self.kind {
            CollectionKind::Static(handles) => {
                if super::realm_registry(ctx)?
                    .borrow()
                    .owner_world(self.scope.0)
                    .is_none()
                {
                    return Ok(Value::new_null(ctx.clone()));
                }
                handles.get(index).map(|handle| handle.0)
            }
            CollectionKind::Children
            | CollectionKind::ElementChildren
            | CollectionKind::ElementsByTag { .. }
            | CollectionKind::ElementsByTagNs { .. }
            | CollectionKind::ElementsByClass(_)
            | CollectionKind::ElementsByName(_)
            | CollectionKind::SelectOptions
            | CollectionKind::SelectedOptions
            | CollectionKind::WindowNamed(_)
            | CollectionKind::HtmlScripts
            | CollectionKind::HtmlLinks
            | CollectionKind::TableBodies
            | CollectionKind::TableRows
            | CollectionKind::TableSectionRows
            | CollectionKind::TableRowCells => self.ids(ctx)?.get(index).copied(),
        };
        match id {
            Some(id) => wrap_node(ctx, id),
            None => Ok(Value::new_null(ctx.clone())),
        }
    }
}

/// The live ids backing one named collection rooted at `scope`.
pub(crate) fn collection_ids(
    ctx: &Ctx<'_>,
    scope: NodeId,
    kind: &CollectionKind,
) -> Result<Vec<NodeId>> {
    query_ids(ctx, scope, kind)
}

/// The live ids of `kind` scoped at `scope`, resolved through the agent's
/// document registry. Blitz trees are walked directly: descendants come from
/// `base.get_node` children stacks, and matching reads element names and
/// attributes off `ElementData`.
fn query_ids(ctx: &Ctx<'_>, scope: NodeId, kind: &CollectionKind) -> Result<Vec<NodeId>> {
    let registry = super::realm_registry(ctx)?;
    let scope = registry.borrow_mut().live_node_id(scope);
    let Some(owner) = registry.borrow().owner_world(scope) else {
        return Ok(Vec::new());
    };
    let owner = owner.borrow();
    if matches!(kind, CollectionKind::SelectedOptions) {
        let Some(mut parsed) = owner.document_mut(scope) else {
            return Ok(Vec::new());
        };
        return Ok(effective_selected_options(&mut parsed, scope.node)
            .into_iter()
            .map(|option| NodeId {
                document: scope.document,
                node: option.node,
            })
            .collect());
    }
    let Some(parsed) = owner.document(scope) else {
        return Ok(Vec::new());
    };
    let base = &parsed.document.base;
    let document = scope.document;
    Ok(match kind {
        CollectionKind::Children => crate::dom::tree::child_ids(base, document, scope.node),
        CollectionKind::ElementChildren => crate::dom::tree::child_ids(base, document, scope.node)
            .into_iter()
            .filter(|kid| is_element(base, kid.node))
            .collect(),
        CollectionKind::ElementsByTag {
            name,
            html_document,
        } => collect_by_tag(base, document, scope.node, name, *html_document),
        CollectionKind::ElementsByTagNs { namespace, local } => {
            collect_by_tag_ns(base, document, scope.node, namespace, local)
        }
        CollectionKind::ElementsByClass(names) => collect_by_class(
            base,
            document,
            scope.node,
            names,
            parsed.quirks_mode == markup5ever::interface::QuirksMode::Quirks,
        ),
        CollectionKind::ElementsByName(name) => collect_by_name(base, document, scope.node, name),
        CollectionKind::SelectOptions => select_options(base, document, scope.node),
        CollectionKind::SelectedOptions => Vec::new(),
        CollectionKind::WindowNamed(name) => collect_window_named(base, document, scope.node, name),
        CollectionKind::HtmlScripts => collect_html_scripts(&parsed.document, document, scope.node),
        CollectionKind::HtmlLinks => collect_html_links(&parsed.document, document, scope.node),
        CollectionKind::TableBodies => table_bodies(base, document, scope.node),
        CollectionKind::TableRows => table_rows(base, document, scope.node),
        CollectionKind::TableSectionRows => table_section_rows(base, document, scope.node),
        CollectionKind::TableRowCells => table_row_cells(base, document, scope.node),
        CollectionKind::Static(handles) => handles.iter().map(|handle| handle.0).collect(),
    })
}

/// HTML `script` descendants of the document element.
///
/// `Document.scripts` is an `HTMLCollection` rooted at the `Document` node
/// whose filter matches `script` elements
/// (<https://html.spec.whatwg.org/multipage/dom.html#dom-document-scripts>).
fn collect_html_scripts(
    doc: &crate::documents::BlitzDocument,
    document: u32,
    scope: BlitzId,
) -> Vec<NodeId> {
    let mut order = Vec::new();
    let mut stack: Vec<BlitzId> = doc
        .base
        .get_node(scope)
        .map_or_default(|node| node.children.iter().rev().copied().collect());
    while let Some(id) = stack.pop() {
        if crate::dom::tree::is_html_element(&doc.base, id, "script")
            && script_is_in_document(doc, id)
        {
            order.push(NodeId { document, node: id });
        }
        if let Some(node) = doc.base.get_node(id) {
            stack.extend(node.children.iter().rev().copied());
        }
    }
    order
}

/// HTML `a`/`area` descendants with an `href` attribute, in tree order.
///
/// `Document.links` is an `HTMLCollection` rooted at the `Document` node whose
/// filter matches `a` and `area` elements that have an `href` attribute
/// (<https://html.spec.whatwg.org/multipage/dom.html#dom-document-links>).
fn collect_html_links(
    doc: &crate::documents::BlitzDocument,
    document: u32,
    scope: BlitzId,
) -> Vec<NodeId> {
    let mut order = Vec::new();
    let mut stack: Vec<BlitzId> = doc
        .base
        .get_node(scope)
        .map_or_default(|node| node.children.iter().rev().copied().collect());
    while let Some(id) = stack.pop() {
        let is_link = crate::dom::tree::is_html_element(&doc.base, id, "a")
            || crate::dom::tree::is_html_element(&doc.base, id, "area");
        if is_link && crate::dom::tree::attr(&doc.base, id, "href").is_some() {
            order.push(NodeId { document, node: id });
        }
        if let Some(node) = doc.base.get_node(id) {
            stack.extend(node.children.iter().rev().copied());
        }
    }
    order
}

/// A document script is connected: marked in-document by insertion and
/// ancestor-connected to the document root.
///
/// `IS_IN_DOCUMENT` is set only in `process_added_subtree` when the parent is
/// already in the document, so a script inside a detached fragment is not a
/// document script
/// (<https://html.spec.whatwg.org/multipage/dom.html#dom-document-scripts>,
/// <https://dom.spec.whatwg.org/#connected>).
fn script_is_in_document(doc: &crate::documents::BlitzDocument, mut id: BlitzId) -> bool {
    if !doc
        .base
        .get_node(id)
        .is_some_and(|node| node.flags.is_in_document())
    {
        return false;
    }
    let root = doc.base.root_node().id;
    loop {
        if id == root {
            return true;
        }
        match doc.base.get_node(id).and_then(|node| node.parent) {
            Some(parent) => id = parent,
            None => return false,
        }
    }
}

/// Descendant Blitz ids of `scope` in tree order, excluding `scope` itself.
fn descendants(base: &blitz_dom::BaseDocument, scope: BlitzId) -> Vec<BlitzId> {
    let mut order = Vec::new();
    let mut stack: Vec<BlitzId> = base
        .get_node(scope)
        .map_or_default(|node| node.children.iter().rev().copied().collect());
    while let Some(id) = stack.pop() {
        order.push(id);
        if let Some(node) = base.get_node(id) {
            stack.extend(node.children.iter().rev().copied());
        }
    }
    order
}

fn element_name(base: &blitz_dom::BaseDocument, id: BlitzId) -> Option<&QualName> {
    base.get_node(id)
        .and_then(|node| node.data.downcast_element())
        .map(|element| &element.name)
}

/// Whether `id` is an `option` element, the Blitz stand-in for the options
/// list. Blitz keeps no form model, so a select's options are simply the
/// `option` elements in its subtree, in tree order; optgroup nesting is
/// covered because the walk descends.
fn is_option_element(base: &blitz_dom::BaseDocument, id: BlitzId) -> bool {
    element_name(base, id)
        .is_some_and(|name| name.ns == html_namespace() && name.local.as_str() == "option")
}

fn collect_by_tag(
    base: &blitz_dom::BaseDocument,
    document: u32,
    scope: BlitzId,
    name: &str,
    html_document: bool,
) -> Vec<NodeId> {
    // HTML documents ASCII-lowercase the query against HTML-namespace
    // names; XML documents (including XHTML) match qualified names
    // exactly. The HTML-document flag is captured when the collection is
    // created, so adopting the root into another document does not change
    // the filter (<https://dom.spec.whatwg.org/#concept-getelementsbytagname>).
    let lowered = name.to_ascii_lowercase();
    descendants(base, scope)
        .into_iter()
        .filter(|&id| {
            let Some(qual) = element_name(base, id) else {
                return false;
            };
            name == "*"
                || if html_document && qual.ns == html_namespace() {
                    qualified_name_eq(qual, &lowered)
                } else {
                    qualified_name_eq(qual, name)
                }
        })
        .map(|node| NodeId { document, node })
        .collect()
}

fn collect_by_name(
    base: &blitz_dom::BaseDocument,
    document: u32,
    scope: BlitzId,
    name: &str,
) -> Vec<NodeId> {
    descendants(base, scope)
        .into_iter()
        .filter(|&id| {
            is_element(base, id) && attr(base, id, "name").is_some_and(|value| value == name)
        })
        .map(|node| NodeId { document, node })
        .collect()
}

/// The Window named objects with `name`: every element whose ID is `name`,
/// plus `embed`, `form`, `img`, and `object` elements whose `name` is `name`,
/// in tree order
/// (<https://html.spec.whatwg.org/multipage/nav-history-apis.html#named-access-on-the-window-object>).
fn collect_window_named(
    base: &blitz_dom::BaseDocument,
    document: u32,
    scope: BlitzId,
    name: &str,
) -> Vec<NodeId> {
    if name.is_empty() {
        return Vec::new();
    }
    descendants(base, scope)
        .into_iter()
        .filter(|&id| {
            if attr(base, id, "id").is_some_and(|value| value == name) {
                return true;
            }
            element_name(base, id).is_some_and(|qual| {
                qual.ns == html_namespace()
                    && matches!(qual.local.as_str(), "embed" | "form" | "img" | "object")
                    && attr(base, id, "name").is_some_and(|value| value == name)
            })
        })
        .map(|node| NodeId { document, node })
        .collect()
}

/// Whether a named getter exposes `name`: some descendant carries it as `id`,
/// or an `embed`/`form`/`img`/`object` carries it as `name`.
fn named_exists(base: &blitz_dom::BaseDocument, scope: BlitzId, name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    descendants(base, scope).into_iter().any(|id| {
        if attr(base, id, "id").is_some_and(|value| value == name) {
            return true;
        }
        element_name(base, id).is_some_and(|qual| {
            qual.ns == html_namespace()
                && matches!(qual.local.as_str(), "embed" | "form" | "img" | "object")
                && attr(base, id, "name").is_some_and(|value| value == name)
        })
    })
}

fn collect_by_tag_ns(
    base: &blitz_dom::BaseDocument,
    document: u32,
    scope: BlitzId,
    namespace: &str,
    local: &str,
) -> Vec<NodeId> {
    descendants(base, scope)
        .into_iter()
        .filter(|&id| {
            element_name(base, id).is_some_and(|name| {
                (namespace == "*" || name.ns.as_str() == namespace)
                    && (local == "*" || name.local.as_str() == local)
            })
        })
        .map(|node| NodeId { document, node })
        .collect()
}

fn collect_by_class(
    base: &blitz_dom::BaseDocument,
    document: u32,
    scope: BlitzId,
    names: &str,
    quirks: bool,
) -> Vec<NodeId> {
    let wanted: Vec<&str> = names.split_ascii_whitespace().collect();
    // An empty class set matches nothing
    // (<https://dom.spec.whatwg.org/#concept-getelementsbyclassname>).
    if wanted.is_empty() {
        return Vec::new();
    }
    descendants(base, scope)
        .into_iter()
        .filter(|&id| {
            if !is_element(base, id) {
                return false;
            }
            // The class is the no-namespace `class` attribute, not `*:class`
            // (<https://dom.spec.whatwg.org/#concept-getelementsbyclassname>).
            let Some(classes) = class_attribute(base, id) else {
                return false;
            };
            let tokens: Vec<&str> = classes.split_ascii_whitespace().collect();
            wanted.iter().all(|want| {
                tokens.iter().any(|token| {
                    // Quirks mode compares ASCII-case-insensitively; Unicode
                    // case folding still does not apply.
                    if quirks {
                        token.eq_ignore_ascii_case(want)
                    } else {
                        *token == *want
                    }
                })
            })
        })
        .map(|node| NodeId { document, node })
        .collect()
}

/// The `class` attribute value of an element, if it has one.
fn class_attribute(base: &blitz_dom::BaseDocument, id: BlitzId) -> Option<&str> {
    base.get_node(id)?
        .data
        .downcast_element()?
        .attrs
        .iter()
        .find(|attribute| {
            attribute.name.ns.as_str().is_empty() && attribute.name.local.as_str() == "class"
        })
        .map(|attribute| attribute.value.as_str())
}

/// Whether `id` is an HTML element named `local`.
fn html_named(base: &blitz_dom::BaseDocument, id: BlitzId, local: &str) -> bool {
    element_name(base, id)
        .is_some_and(|name| name.ns == html_namespace() && name.local.as_str() == local)
}

/// The HTML child elements of `parent` named in `locals`, in tree order.
fn html_child_elements(
    base: &blitz_dom::BaseDocument,
    document: u32,
    parent: BlitzId,
    locals: &[&str],
) -> Vec<NodeId> {
    crate::dom::tree::child_ids(base, document, parent)
        .into_iter()
        .filter(|kid| locals.iter().any(|local| html_named(base, kid.node, local)))
        .collect()
}

/// The `tbody` children of `table`
/// (<https://html.spec.whatwg.org/multipage/tables.html#dom-table-tbodies>).
fn table_bodies(base: &blitz_dom::BaseDocument, document: u32, table: BlitzId) -> Vec<NodeId> {
    html_child_elements(base, document, table, &["tbody"])
}

/// The `tr` children of one table section.
fn table_section_rows(
    base: &blitz_dom::BaseDocument,
    document: u32,
    section: BlitzId,
) -> Vec<NodeId> {
    html_child_elements(base, document, section, &["tr"])
}

/// The `td`/`th` children of one table row
/// (<https://html.spec.whatwg.org/multipage/tables.html#dom-tr-cells>).
fn table_row_cells(base: &blitz_dom::BaseDocument, document: u32, row: BlitzId) -> Vec<NodeId> {
    html_child_elements(base, document, row, &["td", "th"])
}

/// `table.rows` lists `thead` rows first, then in-table `tr`/`tbody` rows,
/// then `tfoot` rows, each group in tree order. This groups by parent kind:
/// unlike `tHead`/`tFoot` (first child only), every `thead`/`tfoot` child
/// contributes to its group
/// (<https://html.spec.whatwg.org/multipage/tables.html#dom-table-rows>).
fn table_rows(base: &blitz_dom::BaseDocument, document: u32, table: BlitzId) -> Vec<NodeId> {
    let mut head = Vec::new();
    let mut body = Vec::new();
    let mut foot = Vec::new();
    for kid in crate::dom::tree::child_ids(base, document, table) {
        if html_named(base, kid.node, "thead") {
            head.extend(table_section_rows(base, document, kid.node));
        } else if html_named(base, kid.node, "tfoot") {
            foot.extend(table_section_rows(base, document, kid.node));
        } else if html_named(base, kid.node, "tbody") {
            body.extend(table_section_rows(base, document, kid.node));
        } else if html_named(base, kid.node, "tr") {
            body.push(kid);
        }
    }
    head.extend(body);
    head.extend(foot);
    head
}

/// Detaches `target` from its parent, recording the `childList` journal entry
/// observers deliver. Blitz performs no hierarchy validation here; callers
/// establish the parent first.
fn detach_node(parsed: &mut crate::Parsed, target: NodeId) {
    let document = target.document;
    let (parent, previous, next) = {
        let base = &parsed.document.base;
        let parent = base.get_node(target.node).and_then(|node| node.parent);
        let (previous, next) = parent.map_or((None, None), |parent| {
            siblings_around(base, document, parent, target.node)
        });
        (parent, previous, next)
    };
    parsed.document.base.mutate().remove_node(target.node);
    if let Some(parent) = parent {
        let target_parent = NodeId {
            document,
            node: parent,
        };
        parsed.document.record(JournalEntry::ChildList {
            target: target_parent,
            added: Vec::new(),
            removed: vec![target],
            previous,
            next,
        });
    }
}

/// Appends `child` to `parent`, recording the `childList` journal entry.
fn append_node(parsed: &mut crate::Parsed, parent: NodeId, child: NodeId) {
    let previous = {
        let base = &parsed.document.base;
        base.get_node(parent.node)
            .and_then(|node| node.children.last().copied())
            .map(|node| NodeId {
                document: parent.document,
                node,
            })
    };
    parsed
        .document
        .base
        .mutate()
        .append_children(parent.node, &[child.node]);
    parsed.document.record(JournalEntry::ChildList {
        target: parent,
        added: vec![child],
        removed: Vec::new(),
        previous,
        next: None,
    });
}

/// Inserts `child` before `reference`, recording the `childList` journal
/// entry on the reference's parent.
fn insert_node_before(parsed: &mut crate::Parsed, reference: NodeId, child: NodeId) {
    let document = reference.document;
    let (parent, previous) = {
        let base = &parsed.document.base;
        let parent = base.get_node(reference.node).and_then(|node| node.parent);
        let previous =
            parent.and_then(|parent| siblings_around(base, document, parent, reference.node).0);
        (parent, previous)
    };
    let Some(parent) = parent else {
        return;
    };
    parsed
        .document
        .base
        .mutate()
        .insert_nodes_before(reference.node, &[child.node]);
    parsed.document.record(JournalEntry::ChildList {
        target: NodeId {
            document,
            node: parent,
        },
        added: vec![child],
        removed: Vec::new(),
        previous,
        next: Some(reference),
    });
}

/// Replaces `old` with `new` in its parent, recording the `childList` journal
/// entry.
fn replace_node(parsed: &mut crate::Parsed, old: NodeId, new: NodeId) {
    let document = old.document;
    let (parent, previous, next) = {
        let base = &parsed.document.base;
        let parent = base.get_node(old.node).and_then(|node| node.parent);
        let (previous, next) = parent.map_or((None, None), |parent| {
            siblings_around(base, document, parent, old.node)
        });
        (parent, previous, next)
    };
    let Some(parent) = parent else {
        return;
    };
    parsed
        .document
        .base
        .mutate()
        .replace_node_with(old.node, &[new.node]);
    parsed.document.record(JournalEntry::ChildList {
        target: NodeId {
            document,
            node: parent,
        },
        added: vec![new],
        removed: vec![old],
        previous,
        next,
    });
}

/// Creates a blank `option` element for a growing options collection.
fn create_option_element(parsed: &mut crate::Parsed, document: u32) -> NodeId {
    let name = QualName::new(None, html_namespace(), LocalName::from("option"));
    let node = parsed
        .document
        .base
        .mutate()
        .create_element(name, Vec::new());
    NodeId { document, node }
}

/// Appends `count` blank options to `select`.
fn append_blank_options(parsed: &mut crate::Parsed, select: NodeId, count: usize) {
    for _ in 0..count {
        let option = create_option_element(parsed, select.document);
        append_node(parsed, select, option);
    }
}

// https://webidl.spec.whatwg.org/#is-an-array-index
pub(super) fn array_index(name: &str) -> Option<u32> {
    let index = name
        .parse::<u32>()
        .ok()
        .filter(|index| *index != u32::MAX)?;
    (index.to_string() == name).then_some(index)
}

/// The property key as a string, or `None` for symbols.
///
/// Symbols (including `Symbol.iterator` and `Symbol("0")`) are never
/// supported indexed or named properties, so every exotic hook falls through
/// to the ordinary path instead of treating a symbol description as a name.
/// Re-interning distinguishes string atoms from symbol atoms that share a
/// description; numeric atoms are checked via both string and `u32` forms.
pub(super) fn atom_name<'js>(ctx: &Ctx<'js>, atom: &Atom<'js>) -> Option<String> {
    let name = atom.to_string().ok()?;
    if let Ok(string_atom) = Atom::from_str(ctx.clone(), &name)
        && string_atom == *atom
    {
        return Some(name);
    }
    if let Some(index) = array_index(&name)
        && let Ok(number_atom) = Atom::from_u32(ctx.clone(), index)
        && number_atom == *atom
    {
        return Some(name);
    }
    None
}

pub(super) fn reject_indexed_write<'js>(
    name: &str,
    object: &Value<'js>,
    receiver: &Value<'js>,
) -> ExoticSetResult {
    // https://webidl.spec.whatwg.org/#legacy-platform-object-set: any array
    // index is rejected (even past the end) so `coll[999] = 1` does not create
    // an expando; `HTMLCollection-supported-property-indices.html` requires it.
    if object == receiver && array_index(name).is_some() {
        ExoticSetResult::Handled(false)
    } else {
        ExoticSetResult::Fallthrough
    }
}

#[derive(Trace, rquickjs::JsLifetime)]
pub(crate) struct JsNodeList {
    pub(crate) query: CollectionQuery,
}

include!(concat!(env!("OUT_DIR"), "/NodeList.rs"));

impl<'js> node_list_generated::NodeList<'js> for JsNodeList {
    // https://dom.spec.whatwg.org/#dom-nodelist-length
    fn get_length(&self, ctx: &Ctx<'js>) -> Result<usize> {
        let registry = super::realm_registry(ctx)?;
        let scope = registry.borrow_mut().live_node_id(self.query.scope.0);
        let Some(world) = registry.borrow().owner_world(scope) else {
            return Ok(0);
        };
        let world = world.borrow();
        let Some(parsed) = world.document(scope) else {
            return Ok(0);
        };
        Ok(match &self.query.kind {
            CollectionKind::Static(handles) => handles.len(),
            CollectionKind::Children => parsed
                .document
                .base
                .get_node(scope.node)
                .map_or(0, |node| node.children.len()),
            CollectionKind::ElementsByName(name) => {
                let base = &parsed.document.base;
                let name = name.clone();
                descendants(base, scope.node)
                    .into_iter()
                    .filter(|&id| {
                        is_element(base, id)
                            && attr(base, id, "name").is_some_and(|value| value == name)
                    })
                    .count()
            }
            CollectionKind::ElementChildren
            | CollectionKind::ElementsByTag { .. }
            | CollectionKind::ElementsByTagNs { .. }
            | CollectionKind::ElementsByClass(_)
            | CollectionKind::SelectOptions
            | CollectionKind::SelectedOptions
            | CollectionKind::WindowNamed(_)
            | CollectionKind::HtmlScripts
            | CollectionKind::HtmlLinks
            | CollectionKind::TableBodies
            | CollectionKind::TableRows
            | CollectionKind::TableSectionRows
            | CollectionKind::TableRowCells => self.query.ids(ctx)?.len(),
        })
    }

    // https://dom.spec.whatwg.org/#dom-nodelist-item
    fn item(&self, ctx: Ctx<'js>, index: u32) -> Result<Value<'js>> {
        self.query.item(&ctx, index as usize)
    }
}

#[derive(Trace, rquickjs::JsLifetime)]
pub(crate) struct JsHtmlCollection {
    pub(crate) query: CollectionQuery,
}

include!(concat!(env!("OUT_DIR"), "/HTMLCollection.rs"));

impl<'js> html_collection_generated::HTMLCollection<'js> for JsHtmlCollection {
    // https://dom.spec.whatwg.org/#dom-htmlcollection-length
    fn get_length(&self, ctx: &Ctx<'js>) -> Result<usize> {
        self.query.ids(ctx).map(|ids| ids.len())
    }

    // https://dom.spec.whatwg.org/#dom-htmlcollection-item
    fn item(&self, ctx: Ctx<'js>, index: u32) -> Result<Value<'js>> {
        self.query.item(&ctx, index as usize)
    }

    fn named_item(&self, ctx: Ctx<'js>, name: rquickjs::String<'js>) -> Result<Value<'js>> {
        named_item(&ctx, &self.query, &name.to_string()?)
    }

    fn supported_names(&self, ctx: &Ctx<'js>) -> Result<Vec<String>> {
        named_keys(ctx, &self.query.ids(ctx)?)
    }
}

#[derive(Trace, rquickjs::JsLifetime)]
pub(crate) struct JsOptionsCollection {
    pub(crate) query: CollectionQuery,
}

include!(concat!(env!("OUT_DIR"), "/HTMLOptionsCollection.rs"));

impl JsOptionsCollection {
    // https://html.spec.whatwg.org/multipage/common-dom-interfaces.html#dom-htmloptionscollection-length
    fn set_length(&self, ctx: &Ctx<'_>, value: u32) -> Result<()> {
        let select = self.query.scope.0;
        let registry = super::realm_registry(ctx)?;
        let Some(owner) = registry.borrow().owner_world(select) else {
            return Ok(());
        };
        let length = value as usize;
        let owner = owner.borrow();
        let Some(mut parsed) = owner.document_mut(select) else {
            return Ok(());
        };
        let current = {
            let base = &parsed.document.base;
            select_options(base, select.document, select.node).len()
        };
        if length == current {
            return Ok(());
        }
        if length > current {
            if length > 100_000 {
                return Ok(());
            }
            append_blank_options(&mut parsed, select, length - current);
            drop(parsed);
            drop(owner);
            super::mutation::schedule_mutation_delivery(ctx)?;
            return Ok(());
        }
        let removed: Vec<NodeId> = {
            let base = &parsed.document.base;
            select_options(base, select.document, select.node)[length..].to_vec()
        };
        for option in removed {
            detach_node(&mut parsed, option);
        }
        drop(parsed);
        drop(owner);
        super::mutation::schedule_mutation_delivery(ctx)?;
        Ok(())
    }

    // https://html.spec.whatwg.org/multipage/common-dom-interfaces.html#dom-htmloptionscollection-add
    fn add(
        &self,
        ctx: Ctx<'_>,
        element: html_options_collection_generated::HTMLOptGroupElementOrHTMLOptionElement,
        before: Option<html_options_collection_generated::HTMLElementOrLong>,
    ) -> Result<()> {
        use super::host::NodeReference;
        use html_options_collection_generated::{
            HTMLElementOrLong, HTMLOptGroupElementOrHTMLOptionElement,
        };
        let select = self.query.scope.0;
        // The union conversion already brand-checks the element, so only a
        // tree node arrives here.
        let element = match element {
            HTMLOptGroupElementOrHTMLOptionElement::HTMLOptGroupElement(reference)
            | HTMLOptGroupElementOrHTMLOptionElement::HTMLOptionElement(reference) => reference,
        };
        let NodeReference::Tree(element) = element else {
            return Err(Exception::throw_type(
                &ctx,
                "add requires an option or optgroup element",
            ));
        };
        match before {
            None => append_add_element(&ctx, select, element),
            Some(HTMLElementOrLong::HTMLElement(NodeReference::Tree(before))) => {
                if before == element {
                    return Ok(());
                }
                insert_before_element(&ctx, select, element, before)
            }
            Some(HTMLElementOrLong::HTMLElement(_)) => Err(Exception::throw_type(
                &ctx,
                "add requires an option or optgroup element",
            )),
            Some(HTMLElementOrLong::Long(index)) => {
                insert_before_index(&ctx, select, element, index)
            }
        }
    }

    // https://html.spec.whatwg.org/multipage/common-dom-interfaces.html#dom-htmloptionscollection-remove
    fn remove(&self, ctx: Ctx<'_>, index: i32) -> Result<()> {
        self.remove_option(&ctx, index)
    }

    fn remove_option(&self, ctx: &Ctx<'_>, index: i32) -> Result<()> {
        let select = self.query.scope.0;
        let Ok(index) = usize::try_from(index) else {
            return Ok(());
        };
        let registry = super::realm_registry(ctx)?;
        let Some(owner) = registry.borrow().owner_world(select) else {
            return Ok(());
        };
        let target = {
            let owner = owner.borrow();
            let Some(parsed) = owner.document(select) else {
                return Ok(());
            };
            let base = &parsed.document.base;
            select_options(base, select.document, select.node)
                .get(index)
                .copied()
        };
        let Some(target) = target else {
            return Ok(());
        };
        let owner = registry
            .borrow()
            .owner_world(select)
            .ok_or_else(|| Exception::throw_internal(ctx, "missing JS world"))?;
        let owner = owner.borrow();
        let Some(mut parsed) = owner.document_mut(select) else {
            return Ok(());
        };
        detach_node(&mut parsed, target);
        drop(parsed);
        drop(owner);
        super::mutation::schedule_mutation_delivery(ctx)?;
        Ok(())
    }

    // https://html.spec.whatwg.org/multipage/form-elements.html#dom-select-selectedindex
    fn selected_index(&self, ctx: &Ctx<'_>) -> Result<i32> {
        let select = self.query.scope.0;
        let registry = super::realm_registry(ctx)?;
        let Some(owner) = registry.borrow().owner_world(select) else {
            return Ok(-1);
        };
        let owner = owner.borrow();
        let Some(mut parsed) = owner.document_mut(select) else {
            return Ok(-1);
        };
        Ok(crate::dom::forms::selected_index_of(
            &mut parsed,
            select.node,
        ))
    }

    fn set_selected_index(&self, ctx: &Ctx<'_>, value: i32) -> Result<()> {
        let select = self.query.scope.0;
        let registry = super::realm_registry(ctx)?;
        let Some(owner) = registry.borrow().owner_world(select) else {
            return Ok(());
        };
        let owner = owner.borrow();
        let Some(mut parsed) = owner.document_mut(select) else {
            return Ok(());
        };
        crate::dom::forms::set_select_selected_index_value(&mut parsed, select.node, value);
        Ok(())
    }
}
impl<'js> html_options_collection_generated::HTMLOptionsCollection<'js> for JsOptionsCollection {
    // https://html.spec.whatwg.org/multipage/common-dom-interfaces.html#dom-htmloptionscollection-length
    fn get_length(&self, ctx: &Ctx<'js>) -> Result<usize> {
        self.query.ids(ctx).map(|ids| ids.len())
    }

    // Inherited `item` installs on this prototype too, derived from the
    // parent chain instead of repeated by hand.
    // https://dom.spec.whatwg.org/#dom-htmlcollection-item
    fn item(&self, ctx: Ctx<'js>, arg_0: u32) -> Result<Value<'js>> {
        self.query.item(&ctx, arg_0 as usize)
    }

    // https://dom.spec.whatwg.org/#dom-htmlcollection-nameditem
    fn named_item(&self, ctx: Ctx<'js>, arg_0: rquickjs::String<'js>) -> Result<Value<'js>> {
        named_item(&ctx, &self.query, &arg_0.to_string()?)
    }

    // https://html.spec.whatwg.org/multipage/common-dom-interfaces.html#dom-htmloptionscollection-length
    fn set_length(&self, ctx: &Ctx<'js>, value: u32) -> Result<()> {
        self.set_length(ctx, value)
    }

    fn supported_names(&self, ctx: &Ctx<'js>) -> Result<Vec<String>> {
        named_keys(ctx, &self.query.ids(ctx)?)
    }

    // https://html.spec.whatwg.org/multipage/common-dom-interfaces.html#dom-htmloptionscollection-add
    fn add(
        &self,
        ctx: Ctx<'js>,
        arg_0: html_options_collection_generated::HTMLOptGroupElementOrHTMLOptionElement,
        arg_1: Option<html_options_collection_generated::HTMLElementOrLong>,
    ) -> Result<()> {
        self.add(ctx, arg_0, arg_1)
    }

    // https://html.spec.whatwg.org/multipage/common-dom-interfaces.html#htmloptionscollection-set-indexed
    fn set_indexed(&self, ctx: Ctx<'js>, index: u32, value: Value<'js>) -> Result<()> {
        let select = self.query.scope.0;
        if value.is_null() || value.is_undefined() {
            let index = i32::try_from(index).unwrap_or(i32::MAX);
            return self.remove_option(&ctx, index);
        }
        let option = require_option_element(&ctx, &value)?;
        let index_usize = index as usize;
        let registry = super::realm_registry(&ctx)?;
        let Some(owner) = registry.borrow().owner_world(select) else {
            return Ok(());
        };
        let current = {
            let owner = owner.borrow();
            let Some(parsed) = owner.document(select) else {
                return Ok(());
            };
            let base = &parsed.document.base;
            select_options(base, select.document, select.node).len()
        };
        if index_usize > current && index_usize >= 100_000 {
            return Ok(());
        }
        if index_usize < current {
            return replace_collection_option(&ctx, select, index_usize, option);
        }
        if index_usize > current {
            grow_collection_options(&ctx, select, index_usize - current)?;
        }
        append_collection_option(&ctx, select, option)
    }

    // https://html.spec.whatwg.org/multipage/common-dom-interfaces.html#dom-htmloptionscollection-remove
    fn remove(&self, ctx: Ctx<'js>, arg_0: i32) -> Result<()> {
        self.remove(ctx, arg_0)
    }

    // https://html.spec.whatwg.org/multipage/form-elements.html#dom-select-selectedindex
    fn get_selected_index(&self, ctx: &Ctx<'js>) -> Result<i32> {
        self.selected_index(ctx)
    }

    // https://html.spec.whatwg.org/multipage/form-elements.html#dom-select-selectedindex
    fn set_selected_index(&self, ctx: &Ctx<'js>, value: i32) -> Result<()> {
        self.set_selected_index(ctx, value)
    }
}

fn require_option_element<'js>(ctx: &Ctx<'js>, value: &Value<'js>) -> Result<NodeId> {
    let Some(option) = super::host_node_id(ctx, value) else {
        return Err(Exception::throw_type(
            ctx,
            "option must be an HTMLOptionElement",
        ));
    };
    let registry = super::realm_registry(ctx)?;
    let Some(owner) = registry.borrow().owner_world(option) else {
        return Err(Exception::throw_type(
            ctx,
            "option must be an HTMLOptionElement",
        ));
    };
    let owner = owner.borrow();
    let Some(parsed) = owner.document(option) else {
        return Err(Exception::throw_type(
            ctx,
            "option must be an HTMLOptionElement",
        ));
    };
    if is_option_element(&parsed.document.base, option.node) {
        Ok(option)
    } else {
        Err(Exception::throw_type(
            ctx,
            "option must be an HTMLOptionElement",
        ))
    }
}

fn append_add_element(ctx: &Ctx<'_>, select: NodeId, element: NodeId) -> Result<()> {
    let adopted = super::clone::adopt_across_documents(ctx, select, element)?;
    let registry = super::realm_registry(ctx)?;
    let Some(owner) = registry.borrow().owner_world(select) else {
        return Ok(());
    };
    let owner = owner.borrow();
    let Some(mut parsed) = owner.document_mut(select) else {
        return Ok(());
    };
    // https://html.spec.whatwg.org/multipage/form-elements.html#dom-select-add
    let parent = parsed
        .document
        .base
        .get_node(adopted.node)
        .and_then(|node| node.parent)
        .map(|parent| NodeId {
            document: adopted.document,
            node: parent,
        });
    if parent == Some(select) {
        return Ok(());
    }
    append_node(&mut parsed, select, adopted);
    drop(parsed);
    drop(owner);
    super::mutation::schedule_mutation_delivery(ctx)?;
    Ok(())
}

fn insert_before_element(
    ctx: &Ctx<'_>,
    select: NodeId,
    element: NodeId,
    before_id: NodeId,
) -> Result<()> {
    let registry = super::realm_registry(ctx)?;
    let Some(select_owner) = registry.borrow().owner_world(select) else {
        return Ok(());
    };
    if !is_select_descendant(&select_owner, select, before_id) {
        return Err(super::throw_dom(
            ctx,
            "NotFoundError",
            "reference is not a child of this select",
        ));
    }
    if before_id == select {
        return Err(super::throw_dom(
            ctx,
            "NotFoundError",
            "reference is not a child of this select",
        ));
    }
    let parent = {
        let select_owner = select_owner.borrow();
        let Some(parsed) = select_owner.document(select) else {
            return Ok(());
        };
        parsed
            .document
            .base
            .get_node(before_id.node)
            .and_then(|node| node.parent)
            .map(|parent| NodeId {
                document: before_id.document,
                node: parent,
            })
    };
    let Some(parent) = parent else {
        return Err(super::throw_dom(
            ctx,
            "NotFoundError",
            "reference is not a child of this select",
        ));
    };
    let adopted = super::clone::adopt_across_documents(ctx, parent, element)?;
    let Some(owner) = registry.borrow().owner_world(select) else {
        return Ok(());
    };
    let owner = owner.borrow();
    let Some(mut parsed) = owner.document_mut(select) else {
        return Ok(());
    };
    insert_node_before(&mut parsed, before_id, adopted);
    drop(parsed);
    drop(owner);
    super::mutation::schedule_mutation_delivery(ctx)?;
    Ok(())
}

fn insert_before_index(ctx: &Ctx<'_>, select: NodeId, element: NodeId, index: i32) -> Result<()> {
    let reference = select_option_at(ctx, select, index)?;
    let Some(reference) = reference else {
        return append_add_element(ctx, select, element);
    };
    let parent = {
        let registry = super::realm_registry(ctx)?;
        let Some(owner) = registry.borrow().owner_world(select) else {
            return Ok(());
        };
        let owner = owner.borrow();
        let Some(parsed) = owner.document(select) else {
            return Ok(());
        };
        parsed
            .document
            .base
            .get_node(reference.node)
            .and_then(|node| node.parent)
            .map(|parent| NodeId {
                document: reference.document,
                node: parent,
            })
    };
    let Some(parent) = parent else {
        return Ok(());
    };
    let adopted = super::clone::adopt_across_documents(ctx, parent, element)?;
    let registry = super::realm_registry(ctx)?;
    let Some(owner) = registry.borrow().owner_world(select) else {
        return Ok(());
    };
    let owner = owner.borrow();
    let Some(mut parsed) = owner.document_mut(select) else {
        return Ok(());
    };
    insert_node_before(&mut parsed, reference, adopted);
    drop(parsed);
    drop(owner);
    super::mutation::schedule_mutation_delivery(ctx)?;
    Ok(())
}

fn select_option_at(ctx: &Ctx<'_>, select: NodeId, before_index: i32) -> Result<Option<NodeId>> {
    let registry = super::realm_registry(ctx)?;
    let Some(owner) = registry.borrow().owner_world(select) else {
        return Ok(None);
    };
    let owner = owner.borrow();
    let Some(parsed) = owner.document(select) else {
        return Ok(None);
    };
    let base = &parsed.document.base;
    let options = select_options(base, select.document, select.node);
    if before_index < 0 {
        return Ok(None);
    }
    let Ok(index) = usize::try_from(before_index) else {
        return Ok(None);
    };
    Ok(options.get(index).copied())
}

fn is_select_descendant(
    select_owner: &std::rc::Rc<std::cell::RefCell<crate::js::world::World>>,
    select: NodeId,
    descendant: NodeId,
) -> bool {
    let select_owner = select_owner.borrow();
    let Some(parsed) = select_owner.document(select) else {
        return false;
    };
    let base = &parsed.document.base;
    let mut current = Some(descendant.node);
    while let Some(id) = current {
        if id == select.node {
            return true;
        }
        current = base.get_node(id).and_then(|node| node.parent);
    }
    false
}

fn replace_collection_option(
    ctx: &Ctx<'_>,
    select: NodeId,
    index: usize,
    option: NodeId,
) -> Result<()> {
    let registry = super::realm_registry(ctx)?;
    let existing = {
        let Some(owner) = registry.borrow().owner_world(select) else {
            return Ok(());
        };
        let owner = owner.borrow();
        let Some(parsed) = owner.document(select) else {
            return Ok(());
        };
        let base = &parsed.document.base;
        let options = select_options(base, select.document, select.node);
        let Some(&existing) = options.get(index) else {
            return Ok(());
        };
        let parent = base
            .get_node(existing.node)
            .and_then(|node| node.parent)
            .map(|parent| NodeId {
                document: existing.document,
                node: parent,
            });
        parent.map(|parent| (existing, parent))
    };
    let Some((existing, parent)) = existing else {
        return Ok(());
    };
    let adopted = super::clone::adopt_across_documents(ctx, parent, option)?;
    let Some(owner) = registry.borrow().owner_world(select) else {
        return Ok(());
    };
    let owner = owner.borrow();
    let Some(mut parsed) = owner.document_mut(select) else {
        return Ok(());
    };
    replace_node(&mut parsed, existing, adopted);
    drop(parsed);
    drop(owner);
    super::mutation::schedule_mutation_delivery(ctx)?;
    Ok(())
}

fn grow_collection_options(ctx: &Ctx<'_>, select: NodeId, delta: usize) -> Result<()> {
    let registry = super::realm_registry(ctx)?;
    let Some(owner) = registry.borrow().owner_world(select) else {
        return Ok(());
    };
    let owner = owner.borrow();
    let Some(mut parsed) = owner.document_mut(select) else {
        return Ok(());
    };
    append_blank_options(&mut parsed, select, delta);
    drop(parsed);
    drop(owner);
    super::mutation::schedule_mutation_delivery(ctx)?;
    Ok(())
}

fn append_collection_option(ctx: &Ctx<'_>, select: NodeId, option: NodeId) -> Result<()> {
    let adopted = super::clone::adopt_across_documents(ctx, select, option)?;
    let registry = super::realm_registry(ctx)?;
    let Some(owner) = registry.borrow().owner_world(select) else {
        return Ok(());
    };
    let owner = owner.borrow();
    let Some(mut parsed) = owner.document_mut(select) else {
        return Ok(());
    };
    append_node(&mut parsed, select, adopted);
    drop(parsed);
    drop(owner);
    super::mutation::schedule_mutation_delivery(ctx)?;
    Ok(())
}

/// A static `NodeList` snapshot as an ordinary object on `NodeList.prototype`.
/// Own indexed properties match legacy platform objects without an indexed
/// setter: enumerable, configurable, and not writable
/// (<https://webidl.spec.whatwg.org/#legacy-platform-object-getownproperty>).
pub(super) fn static_node_list<'js>(
    ctx: &Ctx<'js>,
    scope: NodeId,
    handles: &[Handle],
) -> Result<Value<'js>> {
    let prototype = world_for_node(ctx, scope)?
        .borrow()
        .brand("NodeList")
        .ok_or_else(|| Exception::throw_internal(ctx, "NodeList prototype missing"))?;
    let list = Object::new(ctx.clone())?;
    list.set_prototype(Some(&prototype.restore(ctx)?))?;
    for (index, handle) in handles.iter().enumerate() {
        list.prop(
            index.to_string(),
            Property::from(wrap_node(ctx, handle.0)?)
                .enumerable()
                .configurable(),
        )?;
    }
    let length = u32::try_from(handles.len())
        .map_err(|_| Exception::throw_range(ctx, "NodeList length too large"))?;
    let finish: Function = crate::js::bridge::object(ctx)?.get("__tbFinishStaticNodeList")?;
    let (): () = finish.call((list.clone(), length))?;
    Ok(list.into_value())
}

pub(crate) fn install_collection_brand(ctx: &Ctx<'_>) -> Result<()> {
    crate::js::bridge::object(ctx)?.set("__tbWindowNamedValue", Func::from(window_named_value))?;
    crate::js::bridge::object(ctx)?.set("__tbWindowNamedHas", Func::from(window_named_has))?;
    crate::js::bridge::evaluate(ctx, install_collections_js(ctx)?)?;
    Ok(())
}

/// Determines the value of a Window named property: the single named element,
/// or an `HTMLCollection` when several share the name
/// (<https://html.spec.whatwg.org/multipage/nav-history-apis.html#named-access-on-the-window-object>).
fn window_named_value(ctx: Ctx<'_>, name: String) -> Result<Value<'_>> {
    if name.is_empty() {
        return Ok(Value::new_undefined(ctx.clone()));
    }
    let document = world(&ctx)?.borrow().main_document_root();
    let Some(document) = document else {
        return Ok(Value::new_undefined(ctx.clone()));
    };
    let ids = query_ids(&ctx, document, &CollectionKind::WindowNamed(name.clone()))?;
    match ids.as_slice() {
        [] => Ok(Value::new_undefined(ctx)),
        [id] => wrap_node(&ctx, *id),
        _ => live_collection(
            &ctx,
            document,
            CollectionKind::WindowNamed(name),
            Some("HTMLCollection"),
        ),
    }
}

/// Whether `name` is a supported Window named-property name
/// (<https://html.spec.whatwg.org/multipage/nav-history-apis.html#named-access-on-the-window-object>).
fn window_named_has(ctx: Ctx<'_>, name: String) -> Result<bool> {
    if name.is_empty() {
        return Ok(false);
    }
    let world = world(&ctx)?;
    let world = world.borrow();
    let Some(parsed) = world.main_document() else {
        return Ok(false);
    };
    let base = &parsed.document.base;
    Ok(named_exists(base, base.root_node().id, &name))
}

fn named_item<'js>(ctx: &Ctx<'js>, query: &CollectionQuery, name: &str) -> Result<Value<'js>> {
    // https://dom.spec.whatwg.org/#dom-htmlcollection-nameditem: an empty key
    // returns null without searching; id has global precedence over name, so
    // two passes in tree order. Each lookup resolves through the agent's
    // document registry, not the callback realm, so cross-frame collections
    // keep working after navigation or iframe retirement.
    if name.is_empty() {
        return Ok(Value::new_null(ctx.clone()));
    }
    let ids = query.ids(ctx)?;
    let registry = super::realm_registry(ctx)?;
    for id in &ids {
        let owner = registry.borrow().owner_world(*id);
        let Some(owner) = owner else {
            continue;
        };
        let matches = {
            let owner = owner.borrow();
            let Some(parsed) = owner.document(*id) else {
                continue;
            };
            attr(&parsed.document.base, id.node, "id").is_some_and(|value| value == name)
        };
        if matches {
            return wrap_node(ctx, *id);
        }
    }
    for id in &ids {
        let owner = registry.borrow().owner_world(*id);
        let Some(owner) = owner else {
            continue;
        };
        let matches = {
            let owner = owner.borrow();
            let Some(parsed) = owner.document(*id) else {
                continue;
            };
            let base = &parsed.document.base;
            element_name(base, id.node).is_some_and(|qual| qual.ns == html_namespace())
                && attr(base, id.node, "name").is_some_and(|value| value == name)
        };
        if matches {
            return wrap_node(ctx, *id);
        }
    }
    Ok(Value::new_null(ctx.clone()))
}

/// The supported named properties of an `HTMLCollection`, in tree order.
/// <https://dom.spec.whatwg.org/#interface-htmlcollection>
fn named_keys(ctx: &Ctx<'_>, ids: &[NodeId]) -> Result<Vec<String>> {
    let mut keys: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    {
        let registry = super::realm_registry(ctx)?;
        for &id in ids {
            let owner = registry.borrow().owner_world(id);
            let Some(owner) = owner else {
                continue;
            };
            let owner = owner.borrow();
            let Some(parsed) = owner.document(id) else {
                continue;
            };
            let base = &parsed.document.base;
            if let Some(element_id) = attr(base, id.node, "id")
                && !element_id.is_empty()
                && seen.insert(element_id.to_owned())
            {
                keys.push(element_id.to_owned());
            }
            let exposes_name =
                element_name(base, id.node).is_some_and(|qual| qual.ns == html_namespace());
            if exposes_name
                && let Some(element_name) = attr(base, id.node, "name")
                && !element_name.is_empty()
                && seen.insert(element_name.to_owned())
            {
                keys.push(element_name.to_owned());
            }
        }
    }
    Ok(keys)
}

// https://webidl.spec.whatwg.org/#dfn-named-property-visibility
pub(super) fn named_key_visible(object: &Value<'_>, name: &str) -> Result<bool> {
    let mut prototype = object.as_object().and_then(Object::get_prototype);
    while let Some(current) = prototype {
        if current.contains_own_key(name)? {
            return Ok(false);
        }
        prototype = current.get_prototype();
    }
    Ok(true)
}
