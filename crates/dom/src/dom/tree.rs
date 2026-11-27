use blitz_dom::NodeData;
use blitz_traits::node_id::NodeId as BlitzNodeId;
pub use blitz_traits::node_id::NodeId as BlitzId;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId {
    pub document: u32,
    pub node: BlitzNodeId,
}

impl NodeId {
    #[must_use]
    pub fn document_id(self) -> u32 {
        self.document
    }
}

impl std::fmt::Debug for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "NodeId({}.{})", self.document, self.node.as_u64())
    }
}

impl PartialOrd for NodeId {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for NodeId {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.document, self.node.as_u64()).cmp(&(other.document, other.node.as_u64()))
    }
}

// https://dom.spec.whatwg.org/#concept-shadow-including-parent
pub fn shadow_including_parent(
    base: &blitz_dom::BaseDocument,
    id: BlitzNodeId,
) -> Option<BlitzNodeId> {
    let node = base.get_node(id)?;
    if let Some(parent) = node.parent {
        return Some(parent);
    }
    shadow_root_host(base, id)
}

// https://dom.spec.whatwg.org/#concept-shadow-root
pub fn shadow_root_host(base: &blitz_dom::BaseDocument, id: BlitzNodeId) -> Option<BlitzNodeId> {
    match &base.get_node(id)?.data {
        blitz_dom::NodeData::ShadowRoot { host, .. } => Some(*host),
        _ => None,
    }
}

// https://dom.spec.whatwg.org/#concept-shadow-including-parent
pub fn shadow_including_ancestors(
    base: &blitz_dom::BaseDocument,
    start: Option<BlitzNodeId>,
) -> impl Iterator<Item = &blitz_dom::Node> + '_ {
    std::iter::successors(start.and_then(|id| base.get_node(id)), |node| {
        shadow_including_parent(base, node.id).and_then(|parent| base.get_node(parent))
    })
}

// https://dom.spec.whatwg.org/#concept-shadow-including-root
pub fn shadow_including_root(base: &blitz_dom::BaseDocument, id: BlitzNodeId) -> BlitzNodeId {
    let mut root = id;
    while let Some(parent) = shadow_including_parent(base, root) {
        root = parent;
    }
    root
}

// https://dom.spec.whatwg.org/#connected
pub fn is_connected(base: &blitz_dom::BaseDocument, id: BlitzNodeId) -> bool {
    shadow_including_root(base, id) == base.root_node().id
}

// https://drafts.csswg.org/css-scoping-1/#flat-tree
pub fn is_in_flat_tree(base: &blitz_dom::BaseDocument, id: BlitzNodeId) -> bool {
    let mut current = base.get_node(id);
    while let Some(node) = current {
        let Some(parent_id) = node.parent else {
            return true;
        };
        let Some(parent) = base.get_node(parent_id) else {
            return true;
        };
        if parent
            .data
            .downcast_element()
            .is_some_and(|element| element.shadow_root.is_some())
        {
            let Some(slot_id) = node.assigned_slot_id() else {
                return false;
            };
            current = base.get_node(slot_id);
            continue;
        }
        if parent.is_html_slot() && !parent.assigned_slottables().is_empty() {
            return false;
        }
        current = Some(parent);
    }
    true
}

pub fn is_child_navigable_container(base: &blitz_dom::BaseDocument, id: BlitzNodeId) -> bool {
    base.get_node(id).is_some_and(|node| {
        let blitz_dom::NodeData::Element(element) = &node.data else {
            return false;
        };
        element.name.ns == html_namespace()
            && (element.name.local == markup5ever::local_name!("iframe")
                || element.name.local == markup5ever::local_name!("frame"))
    })
}

// https://html.spec.whatwg.org/multipage/links.html#link-type-stylesheet
pub fn is_stylesheet_link(base: &blitz_dom::BaseDocument, id: BlitzNodeId) -> bool {
    is_html_element(base, id, "link")
        && attr(base, id, "rel").is_some_and(|rel| {
            rel.split_ascii_whitespace()
                .any(|token| token.eq_ignore_ascii_case("stylesheet"))
        })
}

pub fn child_ids(base: &blitz_dom::BaseDocument, document: u32, id: BlitzNodeId) -> Vec<NodeId> {
    base.get_node(id).map_or_else(Vec::new, |node| {
        node.children
            .iter()
            .map(|child| NodeId {
                document,
                node: *child,
            })
            .collect()
    })
}

fn attr_find(
    base: &blitz_dom::BaseDocument,
    id: BlitzNodeId,
    match_name: impl Fn(&markup5ever::QualName) -> bool,
) -> Option<&str> {
    base.get_node(id)?
        .data
        .downcast_element()?
        .attrs
        .iter()
        .find(|attribute| match_name(&attribute.name))
        .map(|attribute| attribute.value.as_str())
}

// https://dom.spec.whatwg.org/#concept-element-attributes-get-by-name
pub fn attr<'a>(base: &'a blitz_dom::BaseDocument, id: BlitzNodeId, name: &str) -> Option<&'a str> {
    let local = markup5ever::LocalName::from(name);
    attr_find(base, id, |qualified| {
        qualified.local == local && qualified.ns == markup5ever::ns!()
    })
}

// https://dom.spec.whatwg.org/#concept-element-attributes-get-by-name
pub fn attr_by_qualified_name<'a>(
    base: &'a blitz_dom::BaseDocument,
    id: BlitzNodeId,
    qualified: &str,
) -> Option<&'a str> {
    attr_find(base, id, |name| {
        crate::names::qualified_name_eq(name, qualified)
    })
}

#[must_use]
pub fn is_handler_attribute_name(local: &str) -> bool {
    local.strip_prefix("on").is_some_and(|typ| {
        !typ.is_empty()
            && typ
                .chars()
                .all(|character| character.is_ascii_lowercase() || character.is_ascii_digit())
    })
}

#[must_use]
pub fn html_namespace() -> markup5ever::Namespace {
    markup5ever::ns!(html)
}

#[must_use]
pub fn svg_namespace() -> markup5ever::Namespace {
    markup5ever::ns!(svg)
}

#[must_use]
pub fn mathml_namespace() -> markup5ever::Namespace {
    markup5ever::ns!(mathml)
}

#[must_use]
pub fn is_html_tag(data: Option<&blitz_dom::NodeData>, local: &str) -> bool {
    match data {
        Some(blitz_dom::NodeData::Element(element)) => {
            element.name.ns == html_namespace() && element.name.local.as_str() == local
        }
        _ => false,
    }
}

// https://html.spec.whatwg.org/multipage/scripting.html#the-template-element
#[must_use]
pub fn is_template_tag(name: &markup5ever::QualName) -> bool {
    name.ns == html_namespace() && name.local.as_str() == "template"
}

pub fn template_contents(base: &blitz_dom::BaseDocument, id: BlitzNodeId) -> Option<BlitzNodeId> {
    base.get_node(id)?
        .data
        .downcast_element()
        .filter(|element| is_template_tag(&element.name))
        .and_then(|element| element.template_contents)
}

pub fn is_html_element(base: &blitz_dom::BaseDocument, id: BlitzNodeId, local: &str) -> bool {
    base.get_node(id)
        .is_some_and(|node| is_html_tag(Some(&node.data), local))
}

// https://dom.spec.whatwg.org/#concept-id
// https://dom.spec.whatwg.org/#dom-nonelementparentnode-getelementbyid
// https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#form-owner
pub fn first_element_with_id(
    base: &blitz_dom::BaseDocument,
    document: u32,
    root: BlitzNodeId,
    id: &str,
) -> Option<NodeId> {
    if id.is_empty() {
        return None;
    }
    let mut stack = vec![root];
    while let Some(current) = stack.pop() {
        let Some(node) = base.get_node(current) else {
            continue;
        };
        if node.data.downcast_element().is_some_and(|element| {
            element.attrs().iter().any(|attribute| {
                attribute.name.local.as_str() == "id"
                    && attribute.name.ns == markup5ever::ns!()
                    && attribute.value == id
            })
        }) {
            return Some(NodeId {
                document,
                node: current,
            });
        }
        stack.extend(node.children.iter().rev().copied());
    }
    None
}

// https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#reset-the-form-owner
pub fn form_owner_of(
    base: &blitz_dom::BaseDocument,
    document: u32,
    node: BlitzId,
) -> Option<NodeId> {
    if let Some(form_id) = attr(base, node, "form")
        && is_connected(base, node)
    {
        if let Some(found) = first_element_with_id(base, document, root_of(base, node), form_id)
            && is_html_element(base, found.node, "form")
        {
            return Some(found);
        }
        return None;
    }
    let mut cursor = base.get_node(node).and_then(|target| target.parent);
    while let Some(current) = cursor {
        if is_html_element(base, current, "form") {
            return Some(NodeId {
                document,
                node: current,
            });
        }
        cursor = base.get_node(current).and_then(|target| target.parent);
    }
    None
}

// https://dom.spec.whatwg.org/#concept-element-qualified-name
#[must_use]
pub fn qualified_name(name: &markup5ever::QualName) -> String {
    match &name.prefix {
        Some(prefix) if !prefix.is_empty() => format!("{prefix}:{}", name.local),
        _ => name.local.to_string(),
    }
}

// https://dom.spec.whatwg.org/#concept-element-html-uppercased-qualified-name
#[must_use]
pub fn element_node_name(name: &markup5ever::QualName, uppercase: bool) -> String {
    let qualified = qualified_name(name);
    if uppercase && name.ns == html_namespace() {
        qualified.to_ascii_uppercase()
    } else {
        qualified
    }
}

// https://dom.spec.whatwg.org/#dom-node-nodetype
#[must_use]
pub fn node_type(data: Option<&NodeData>) -> Option<u16> {
    match data {
        Some(NodeData::Element(_)) => Some(1),
        Some(NodeData::Text(_)) => Some(3),
        Some(NodeData::ProcessingInstruction { .. }) => Some(7),
        Some(NodeData::Comment { .. }) => Some(8),
        Some(NodeData::Document(_)) => Some(9),
        Some(NodeData::Doctype { .. }) => Some(10),
        Some(NodeData::CDataSection { .. }) => Some(4),
        Some(NodeData::Fragment { .. } | NodeData::ShadowRoot { .. }) => Some(11),
        Some(NodeData::AnonymousBlock(_)) | None => None,
    }
}

// https://dom.spec.whatwg.org/#dom-node-nodename
// https://dom.spec.whatwg.org/#concept-element-html-uppercased-qualified-name
pub fn node_name(base: &blitz_dom::BaseDocument, id: BlitzId, html: bool) -> Option<String> {
    let data = base.get_node(id).map(|node| &node.data);
    match data {
        Some(NodeData::Element(element)) => Some(element_node_name(&element.name, html)),
        Some(NodeData::Text(_)) => Some("#text".into()),
        Some(NodeData::CDataSection { .. }) => Some("#cdata-section".into()),
        Some(NodeData::ProcessingInstruction { target, .. }) => Some(target.clone()),
        Some(NodeData::Comment { .. }) => Some("#comment".into()),
        Some(NodeData::Doctype { name, .. }) => Some(name.clone()),
        Some(NodeData::Fragment { .. } | NodeData::ShadowRoot { .. }) => {
            Some("#document-fragment".into())
        }
        Some(NodeData::Document(_)) => Some("#document".into()),
        Some(NodeData::AnonymousBlock(_)) | None => None,
    }
}

pub fn sibling(base: &blitz_dom::BaseDocument, id: BlitzId, forward: bool) -> Option<BlitzId> {
    let node = base.get_node(id)?;
    let parent = node.parent?;
    let siblings = &base.get_node(parent)?.children;
    let position = siblings.iter().position(|sibling| *sibling == id)?;
    if forward {
        siblings.get(position + 1).copied()
    } else {
        position
            .checked_sub(1)
            .and_then(|index| siblings.get(index).copied())
    }
}

pub fn is_element(base: &blitz_dom::BaseDocument, id: BlitzId) -> bool {
    base.get_node(id)
        .is_some_and(|node| matches!(node.data, NodeData::Element(_)))
}

pub fn root_of(base: &blitz_dom::BaseDocument, id: BlitzId) -> BlitzId {
    let mut root = id;
    while let Some(parent) = base.get_node(root).and_then(|node| node.parent) {
        root = parent;
    }
    root
}

pub fn ancestor_chain(base: &blitz_dom::BaseDocument, document: u32, id: BlitzId) -> Vec<NodeId> {
    let mut chain = vec![NodeId { document, node: id }];
    let mut cursor = id;
    while let Some(parent) = base.get_node(cursor).and_then(|node| node.parent) {
        chain.push(NodeId {
            document,
            node: parent,
        });
        cursor = parent;
    }
    chain
}

// https://dom.spec.whatwg.org/#concept-tree-order
pub fn tree_order(
    base: &blitz_dom::BaseDocument,
    document: u32,
    a: BlitzId,
    b: BlitzId,
) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let chain_a = ancestor_chain(base, document, a);
    let chain_b = ancestor_chain(base, document, b);
    let mut common = 0;
    while common < chain_a.len()
        && common < chain_b.len()
        && chain_a[chain_a.len() - 1 - common] == chain_b[chain_b.len() - 1 - common]
    {
        common += 1;
    }
    if common == chain_a.len() {
        return chain_a.len().cmp(&chain_b.len());
    }
    if common == chain_b.len() {
        return Ordering::Greater;
    }
    let Some(parent) = chain_a
        .len()
        .checked_sub(common)
        .and_then(|index| chain_a.get(index))
    else {
        return Ordering::Less;
    };
    let child_a = chain_a[chain_a.len() - 1 - common];
    let child_b = chain_b[chain_b.len() - 1 - common];
    let kids: Vec<NodeId> = base.get_node(parent.node).map_or_default(|node| {
        node.children
            .iter()
            .map(|child| NodeId {
                document,
                node: *child,
            })
            .collect()
    });
    let position_a = kids.iter().position(|&kid| kid == child_a);
    let position_b = kids.iter().position(|&kid| kid == child_b);
    position_a.cmp(&position_b)
}
