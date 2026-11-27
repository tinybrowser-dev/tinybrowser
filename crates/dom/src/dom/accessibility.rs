// https://www.w3.org/TR/html-aam-1.0/
// https://www.w3.org/TR/accname-1.2/
// https://chromedevtools.github.io/devtools-protocol/tot/Accessibility/

use std::collections::HashMap;

use blitz_dom::{BaseDocument, CopyStyleGetters, NodeData};
use style::properties::generated::longhands::visibility::computed_value::T as StyloVisibility;

use crate::dom::tree::{BlitzId, NodeId, attr, html_namespace};
use crate::protocol::{AxNode, AxProperty, AxPropertyValue};

const MAX_AX_NODES: usize = 16_384;

#[must_use]
pub fn build_ax_tree<S: std::hash::BuildHasher>(
    base: &BaseDocument,
    root: NodeId,
    ids: &HashMap<NodeId, u64, S>,
    focused: Option<NodeId>,
) -> Vec<AxNode> {
    let mut builder = AxBuilder {
        base,
        document: root.document,
        ids,
        focused,
        id_map: index_element_ids(base, root.node),
        label_map: index_for_labels(base, root.node),
        nodes: Vec::new(),
    };
    builder.visit(root.node, None);
    builder.nodes
}

struct AxBuilder<'a, S> {
    base: &'a BaseDocument,
    document: u32,
    ids: &'a HashMap<NodeId, u64, S>,
    focused: Option<NodeId>,
    id_map: HashMap<String, BlitzId>,
    label_map: HashMap<String, Vec<BlitzId>>,
    nodes: Vec<AxNode>,
}

impl<S: std::hash::BuildHasher> AxBuilder<'_, S> {
    fn visit(&mut self, node: BlitzId, parent_ax: Option<&str>) -> Option<String> {
        if self.nodes.len() >= MAX_AX_NODES {
            return None;
        }
        let tree = self.base.get_node(node)?;
        if is_skipped_subtree(self.base, node) {
            return None;
        }
        let ax_id = (self.nodes.len() + 1).to_string();
        let node_id = NodeId {
            document: self.document,
            node,
        };
        let backend_dom_node_id = self.ids.get(&node_id).copied();
        match &tree.data {
            NodeData::Document(_) => {
                let name = document_title(self.base, node);
                let index = self.nodes.len();
                self.nodes.push(AxNode {
                    node_id: ax_id.clone(),
                    ignored: false,
                    role: "RootWebArea".to_owned(),
                    name,
                    value: None,
                    properties: Vec::new(),
                    parent_id: parent_ax.map(str::to_owned),
                    child_ids: Vec::new(),
                    backend_dom_node_id,
                });
                let children = self.visit_children(node, &ax_id);
                self.nodes[index].child_ids = children;
                Some(ax_id)
            }
            NodeData::Text(text) => {
                let collapsed = collapse_ws(&text.content);
                if collapsed.is_empty() {
                    return None;
                }
                self.nodes.push(AxNode {
                    node_id: ax_id.clone(),
                    ignored: false,
                    role: "StaticText".to_owned(),
                    name: Some(collapsed),
                    value: None,
                    properties: Vec::new(),
                    parent_id: parent_ax.map(str::to_owned),
                    child_ids: Vec::new(),
                    backend_dom_node_id,
                });
                Some(ax_id)
            }
            NodeData::Element(element) => {
                let html = element.name.ns == html_namespace();
                let local = element.name.local.as_str();
                let (role, heading_level) = compute_role(self.base, node, html, local);
                let ignored =
                    role == "none" || role == "presentation" || is_invisible(self.base, node);
                let name = self.compute_name(node, html, local, role);
                let value = compute_value(self.base, node, html, local);
                let properties = self.compute_properties(node, html, local, heading_level);
                let index = self.nodes.len();
                self.nodes.push(AxNode {
                    node_id: ax_id.clone(),
                    ignored,
                    role: role.to_owned(),
                    name,
                    value,
                    properties,
                    parent_id: parent_ax.map(str::to_owned),
                    child_ids: Vec::new(),
                    backend_dom_node_id,
                });
                let children = self.visit_children(node, &ax_id);
                self.nodes[index].child_ids = children;
                Some(ax_id)
            }
            _ => None,
        }
    }

    fn visit_children(&mut self, node: BlitzId, parent_ax: &str) -> Vec<String> {
        let Some(tree) = self.base.get_node(node) else {
            return Vec::new();
        };
        let mut child_ids = Vec::new();
        for &child in &tree.children {
            if self.nodes.len() >= MAX_AX_NODES {
                break;
            }
            if let Some(child_ax) = self.visit(child, Some(parent_ax)) {
                child_ids.push(child_ax);
            }
        }
        child_ids
    }

    // https://www.w3.org/TR/accname-1.2/#mapping_additional_nd_te
    fn compute_name(&self, node: BlitzId, html: bool, local: &str, role: &str) -> Option<String> {
        if let Some(ids) = attr(self.base, node, "aria-labelledby") {
            let mut parts = Vec::new();
            for token in ids.split_ascii_whitespace() {
                if let Some(&target) = self.id_map.get(token) {
                    let text = subtree_text(self.base, target);
                    if !text.is_empty() {
                        parts.push(text);
                    }
                }
            }
            if !parts.is_empty() {
                return Some(parts.join(" "));
            }
        }
        if let Some(label) = attr(self.base, node, "aria-label") {
            let collapsed = collapse_ws(label);
            if !collapsed.is_empty() {
                return Some(collapsed);
            }
        }
        if html && is_labelable(local) {
            if let Some(id) = attr(self.base, node, "id")
                && let Some(labels) = self.label_map.get(id)
            {
                let mut parts = Vec::new();
                for &label_node in labels {
                    let text = subtree_text(self.base, label_node);
                    if !text.is_empty() {
                        parts.push(text);
                    }
                }
                if !parts.is_empty() {
                    return Some(parts.join(" "));
                }
            }
            if let Some(label_ancestor) = enclosing_label(self.base, node) {
                let text = subtree_text(self.base, label_ancestor);
                if !text.is_empty() {
                    return Some(text);
                }
            }
        }
        if html
            && matches!(local, "img" | "area")
            && let Some(alt) = attr(self.base, node, "alt")
        {
            return Some(collapse_ws(alt));
        }
        if html && local == "input" {
            let input_type = attr(self.base, node, "type").unwrap_or("text");
            if input_type.eq_ignore_ascii_case("image")
                && let Some(alt) = attr(self.base, node, "alt")
            {
                return Some(collapse_ws(alt));
            }
            if matches_ignore_case(input_type, &["button", "submit", "reset"])
                && let Some(value) = attr(self.base, node, "value")
            {
                let collapsed = collapse_ws(value);
                if !collapsed.is_empty() {
                    return Some(collapsed);
                }
            }
        }
        if name_from_content(role) {
            let text = subtree_text(self.base, node);
            if !text.is_empty() {
                return Some(text);
            }
        }
        if let Some(placeholder) = attr(self.base, node, "placeholder") {
            let collapsed = collapse_ws(placeholder);
            if !collapsed.is_empty() {
                return Some(collapsed);
            }
        }
        attr(self.base, node, "title")
            .map(collapse_ws)
            .filter(|title| !title.is_empty())
    }

    fn compute_properties(
        &self,
        node: BlitzId,
        html: bool,
        local: &str,
        heading_level: Option<i64>,
    ) -> Vec<AxProperty> {
        let mut props = Vec::new();
        if let Some(level) = heading_level {
            props.push(AxProperty {
                name: "level".to_owned(),
                value: AxPropertyValue::Int(level),
            });
        }
        if self.focused
            == Some(NodeId {
                document: self.document,
                node,
            })
        {
            props.push(AxProperty {
                name: "focused".to_owned(),
                value: AxPropertyValue::Bool(true),
            });
        }
        if attr(self.base, node, "disabled").is_some()
            || attr(self.base, node, "aria-disabled")
                .is_some_and(|value| value.eq_ignore_ascii_case("true"))
        {
            props.push(AxProperty {
                name: "disabled".to_owned(),
                value: AxPropertyValue::Bool(true),
            });
        }
        if attr(self.base, node, "required").is_some()
            || attr(self.base, node, "aria-required")
                .is_some_and(|value| value.eq_ignore_ascii_case("true"))
        {
            props.push(AxProperty {
                name: "required".to_owned(),
                value: AxPropertyValue::Bool(true),
            });
        }
        if attr(self.base, node, "readonly").is_some()
            || attr(self.base, node, "aria-readonly")
                .is_some_and(|value| value.eq_ignore_ascii_case("true"))
        {
            props.push(AxProperty {
                name: "readonly".to_owned(),
                value: AxPropertyValue::Bool(true),
            });
        }
        if let Some(checked) = checked_state(self.base, node, html, local) {
            props.push(AxProperty {
                name: "checked".to_owned(),
                value: AxPropertyValue::String(checked.to_owned()),
            });
        }
        for (aria_attr, prop_name) in [
            ("aria-expanded", "expanded"),
            ("aria-selected", "selected"),
            ("aria-pressed", "pressed"),
        ] {
            if let Some(raw) = attr(self.base, node, aria_attr) {
                if raw.eq_ignore_ascii_case("true") {
                    props.push(AxProperty {
                        name: prop_name.to_owned(),
                        value: AxPropertyValue::Bool(true),
                    });
                } else if raw.eq_ignore_ascii_case("false") {
                    props.push(AxProperty {
                        name: prop_name.to_owned(),
                        value: AxPropertyValue::Bool(false),
                    });
                }
            }
        }
        if html
            && matches!(local, "a" | "area")
            && let Some(href) = attr(self.base, node, "href")
        {
            props.push(AxProperty {
                name: "url".to_owned(),
                value: AxPropertyValue::String(href.to_owned()),
            });
        }
        props
    }
}

fn is_skipped_subtree(base: &BaseDocument, node: BlitzId) -> bool {
    let Some(tree) = base.get_node(node) else {
        return true;
    };
    let Some(element) = tree.data.downcast_element() else {
        return false;
    };
    if attr(base, node, "aria-hidden").is_some_and(|value| value.eq_ignore_ascii_case("true")) {
        return true;
    }
    if element.name.ns == html_namespace() {
        if matches!(
            element.name.local.as_str(),
            "head"
                | "meta"
                | "link"
                | "base"
                | "title"
                | "script"
                | "style"
                | "template"
                | "noscript"
        ) {
            return true;
        }
        if attr(base, node, "hidden")
            .is_some_and(|value| !value.eq_ignore_ascii_case("until-found"))
        {
            return true;
        }
    }
    tree.primary_styles()
        .is_some_and(|styles| styles.clone_display().is_none())
}

fn is_invisible(base: &BaseDocument, node: BlitzId) -> bool {
    base.get_node(node)
        .and_then(|tree| tree.primary_styles())
        .is_some_and(|styles| styles.get_inherited_box().visibility != StyloVisibility::Visible)
}

// https://www.w3.org/TR/html-aam-1.0/#html-element-role-mappings
fn compute_role<'a>(
    base: &'a BaseDocument,
    node: BlitzId,
    html: bool,
    local: &str,
) -> (&'a str, Option<i64>) {
    if let Some(explicit) = attr(base, node, "role")
        && let Some(first) = explicit.split_ascii_whitespace().next()
    {
        let level = attr(base, node, "aria-level").and_then(|value| value.parse::<i64>().ok());
        return (first, level);
    }
    if !html {
        return ("generic", None);
    }
    match local {
        "a" | "area" if attr(base, node, "href").is_some() => ("link", None),
        "button" | "summary" => ("button", None),
        "h1" => ("heading", Some(1)),
        "h2" => ("heading", Some(2)),
        "h3" => ("heading", Some(3)),
        "h4" => ("heading", Some(4)),
        "h5" => ("heading", Some(5)),
        "h6" => ("heading", Some(6)),
        "img" => {
            if attr(base, node, "alt").is_some_and(str::is_empty) {
                ("none", None)
            } else {
                ("image", None)
            }
        }
        "input" => (input_role(base, node), None),
        "textarea" => ("textbox", None),
        "select" => {
            let multiple = attr(base, node, "multiple").is_some()
                || attr(base, node, "size")
                    .and_then(|value| value.parse::<u32>().ok())
                    .is_some_and(|size| size > 1);
            (if multiple { "listbox" } else { "combobox" }, None)
        }
        "option" => ("option", None),
        "nav" => ("navigation", None),
        "main" => ("main", None),
        "header" => ("banner", None),
        "footer" => ("contentinfo", None),
        "aside" => ("complementary", None),
        "form" => ("form", None),
        "article" => ("article", None),
        "section" => ("region", None),
        "ul" | "ol" | "menu" => ("list", None),
        "li" => ("listitem", None),
        "table" => ("table", None),
        "tr" => ("row", None),
        "th" => ("columnheader", None),
        "td" => ("cell", None),
        "dialog" => ("dialog", None),
        "progress" => ("progressbar", None),
        "hr" => ("separator", None),
        "p" => ("paragraph", None),
        _ => ("generic", None),
    }
}

fn input_role(base: &BaseDocument, node: BlitzId) -> &'static str {
    let kind = attr(base, node, "type").unwrap_or("text");
    if kind.eq_ignore_ascii_case("hidden") {
        "none"
    } else if kind.eq_ignore_ascii_case("checkbox") {
        "checkbox"
    } else if kind.eq_ignore_ascii_case("radio") {
        "radio"
    } else if matches_ignore_case(kind, &["button", "submit", "reset", "image"]) {
        "button"
    } else if kind.eq_ignore_ascii_case("range") {
        "slider"
    } else if kind.eq_ignore_ascii_case("number") {
        "spinbutton"
    } else if kind.eq_ignore_ascii_case("search") {
        "searchbox"
    } else {
        "textbox"
    }
}

fn compute_value(base: &BaseDocument, node: BlitzId, html: bool, local: &str) -> Option<String> {
    if let Some(value) =
        attr(base, node, "aria-valuetext").or_else(|| attr(base, node, "aria-valuenow"))
    {
        return Some(value.to_owned());
    }
    if html && matches!(local, "input" | "textarea") {
        return attr(base, node, "value").map(str::to_owned);
    }
    None
}

fn checked_state(
    base: &BaseDocument,
    node: BlitzId,
    html: bool,
    local: &str,
) -> Option<&'static str> {
    if let Some(raw) = attr(base, node, "aria-checked") {
        if raw.eq_ignore_ascii_case("true") {
            return Some("true");
        }
        if raw.eq_ignore_ascii_case("mixed") {
            return Some("mixed");
        }
        if raw.eq_ignore_ascii_case("false") {
            return Some("false");
        }
    }
    if html && local == "input" {
        let kind = attr(base, node, "type").unwrap_or("text");
        if kind.eq_ignore_ascii_case("checkbox") || kind.eq_ignore_ascii_case("radio") {
            return Some(if attr(base, node, "checked").is_some() {
                "true"
            } else {
                "false"
            });
        }
    }
    None
}

fn is_labelable(local: &str) -> bool {
    matches!(
        local,
        "button" | "input" | "meter" | "output" | "progress" | "select" | "textarea"
    )
}

fn name_from_content(role: &str) -> bool {
    matches!(
        role,
        "button"
            | "link"
            | "heading"
            | "option"
            | "listitem"
            | "cell"
            | "columnheader"
            | "rowheader"
            | "tab"
            | "menuitem"
            | "checkbox"
            | "radio"
    )
}

fn matches_ignore_case(value: &str, candidates: &[&str]) -> bool {
    candidates
        .iter()
        .any(|candidate| value.eq_ignore_ascii_case(candidate))
}

fn index_element_ids(base: &BaseDocument, root: BlitzId) -> HashMap<String, BlitzId> {
    let mut map = HashMap::new();
    let mut stack = vec![root];
    while let Some(current) = stack.pop() {
        let Some(tree) = base.get_node(current) else {
            continue;
        };
        if let Some(id) = attr(base, current, "id")
            && !id.is_empty()
        {
            map.entry(id.to_owned()).or_insert(current);
        }
        stack.extend(tree.children.iter().rev());
    }
    map
}

fn index_for_labels(base: &BaseDocument, root: BlitzId) -> HashMap<String, Vec<BlitzId>> {
    let mut map: HashMap<String, Vec<BlitzId>> = HashMap::new();
    let mut stack = vec![root];
    while let Some(current) = stack.pop() {
        let Some(tree) = base.get_node(current) else {
            continue;
        };
        if let Some(element) = tree.data.downcast_element()
            && element.name.ns == html_namespace()
            && element.name.local.as_str() == "label"
            && let Some(target) = attr(base, current, "for")
            && !target.is_empty()
        {
            map.entry(target.to_owned()).or_default().push(current);
        }
        stack.extend(tree.children.iter().rev());
    }
    map
}

fn enclosing_label(base: &BaseDocument, node: BlitzId) -> Option<BlitzId> {
    let mut cursor = base.get_node(node).and_then(|tree| tree.parent);
    while let Some(current) = cursor {
        let tree = base.get_node(current)?;
        if let Some(element) = tree.data.downcast_element()
            && element.name.ns == html_namespace()
            && element.name.local.as_str() == "label"
        {
            return Some(current);
        }
        cursor = tree.parent;
    }
    None
}

fn document_title(base: &BaseDocument, root: BlitzId) -> Option<String> {
    let mut stack = vec![root];
    while let Some(current) = stack.pop() {
        let Some(tree) = base.get_node(current) else {
            continue;
        };
        if let Some(element) = tree.data.downcast_element()
            && element.name.ns == html_namespace()
            && element.name.local.as_str() == "title"
        {
            let text = subtree_text(base, current);
            return (!text.is_empty()).then_some(text);
        }
        stack.extend(tree.children.iter().rev());
    }
    None
}

fn subtree_text(base: &BaseDocument, root: BlitzId) -> String {
    let mut raw = String::new();
    let mut stack = vec![root];
    while let Some(current) = stack.pop() {
        let Some(tree) = base.get_node(current) else {
            continue;
        };
        match &tree.data {
            NodeData::Text(text) => raw.push_str(&text.content),
            NodeData::Element(_) => {
                if current != root && is_skipped_subtree(base, current) {
                    continue;
                }
                stack.extend(tree.children.iter().rev());
            }
            _ => {}
        }
    }
    collapse_ws(&raw)
}

fn collapse_ws(input: &str) -> String {
    let mut out = String::new();
    let mut pending_space = false;
    for ch in input.chars() {
        if ch.is_whitespace() {
            pending_space = !out.is_empty();
        } else {
            if pending_space {
                out.push(' ');
                pending_space = false;
            }
            out.push(ch);
        }
    }
    out
}
