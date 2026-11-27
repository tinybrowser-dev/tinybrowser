// https://www.w3.org/TR/1999/REC-xpath-19991116/#section-Expressions

use std::collections::HashMap;

use blitz_dom::{BaseDocument, NodeData};
use blitz_traits::node_id::NodeId as BlitzId;
use markup5ever::ns;

use super::functions::{
    call_function, ieee_eq, node_string_value, str_to_number, to_boolean, to_number, usize_to_f64,
};
use super::parser::{Axis, BinOp, Expr, NodeTest, Step};
use super::{XPathError, XPathNode, XPathValue};

pub(crate) struct EvalContext<'a> {
    pub(crate) base: &'a BaseDocument,
    pub(crate) is_html: bool,
    pub(crate) root: BlitzId,
    pub(crate) order: &'a HashMap<BlitzId, usize>,
    pub(crate) node: XPathNode,
    pub(crate) position: usize,
    pub(crate) size: usize,
}

impl EvalContext<'_> {
    fn with_node(&self, node: XPathNode, position: usize, size: usize) -> Self {
        Self {
            base: self.base,
            is_html: self.is_html,
            root: self.root,
            order: self.order,
            node,
            position,
            size,
        }
    }
}

pub(crate) fn tree_root(base: &BaseDocument, node: &XPathNode) -> BlitzId {
    let mut current = match node {
        XPathNode::Tree(id) => *id,
        XPathNode::Attr { owner, .. } => *owner,
    };
    while let Some(parent) = base.get_node(current).and_then(|n| n.parent) {
        current = parent;
    }
    current
}

pub(crate) fn build_doc_order(base: &BaseDocument, root: BlitzId) -> HashMap<BlitzId, usize> {
    let mut map = HashMap::new();
    let mut stack = vec![root];
    let mut next = 0usize;
    while let Some(id) = stack.pop() {
        map.insert(id, next);
        next += 1;
        if let Some(node) = base.get_node(id) {
            for &child in node.children.iter().rev() {
                stack.push(child);
            }
        }
    }
    map
}

fn node_sort_key(order: &HashMap<BlitzId, usize>, node: &XPathNode) -> (usize, usize) {
    match node {
        XPathNode::Tree(id) => (order.get(id).copied().unwrap_or(usize::MAX), 0),
        XPathNode::Attr {
            owner, attr_index, ..
        } => (
            order.get(owner).copied().unwrap_or(usize::MAX),
            attr_index + 1,
        ),
    }
}

fn sort_and_dedup(order: &HashMap<BlitzId, usize>, nodes: &mut Vec<XPathNode>) {
    nodes.sort_by_key(|node| node_sort_key(order, node));
    nodes.dedup();
}

pub(crate) fn eval_expr(ctx: &EvalContext<'_>, expr: &Expr) -> Result<XPathValue, XPathError> {
    match expr {
        Expr::Or(lhs, rhs) => {
            if to_boolean(&eval_expr(ctx, lhs)?) {
                Ok(XPathValue::Boolean(true))
            } else {
                Ok(XPathValue::Boolean(to_boolean(&eval_expr(ctx, rhs)?)))
            }
        }
        Expr::And(lhs, rhs) => {
            if to_boolean(&eval_expr(ctx, lhs)?) {
                Ok(XPathValue::Boolean(to_boolean(&eval_expr(ctx, rhs)?)))
            } else {
                Ok(XPathValue::Boolean(false))
            }
        }
        Expr::Binary(op, lhs, rhs) => {
            let left = eval_expr(ctx, lhs)?;
            let right = eval_expr(ctx, rhs)?;
            Ok(eval_binary(ctx.base, *op, &left, &right))
        }
        Expr::Neg(inner) => Ok(XPathValue::Number(-to_number(
            ctx.base,
            &eval_expr(ctx, inner)?,
        ))),
        Expr::Union(lhs, rhs) => eval_union(ctx, lhs, rhs),
        Expr::Path { base, steps } => {
            let XPathValue::NodeSet(initial) = eval_expr(ctx, base)? else {
                return Err(XPathError::Type(
                    "path expression base must evaluate to a node-set".into(),
                ));
            };
            Ok(XPathValue::NodeSet(eval_steps(ctx, initial, steps)?))
        }
        Expr::LocationPath { absolute, steps } => {
            let start = if *absolute {
                vec![XPathNode::Tree(ctx.root)]
            } else {
                vec![ctx.node.clone()]
            };
            Ok(XPathValue::NodeSet(eval_steps(ctx, start, steps)?))
        }
        Expr::Filter { expr, predicates } => {
            let XPathValue::NodeSet(mut nodes) = eval_expr(ctx, expr)? else {
                return Err(XPathError::Type(
                    "predicate target must evaluate to a node-set".into(),
                ));
            };
            sort_and_dedup(ctx.order, &mut nodes);
            for predicate in predicates {
                nodes = apply_predicate(ctx, nodes, predicate)?;
            }
            Ok(XPathValue::NodeSet(nodes))
        }
        Expr::FunctionCall { prefix, name, args } => {
            call_function(ctx, prefix.as_deref(), name, args)
        }
        Expr::Variable { name, .. } => Err(XPathError::Syntax(format!(
            "unbound XPath variable '${name}'"
        ))),
        Expr::Literal(s) => Ok(XPathValue::String(s.clone())),
        Expr::Number(n) => Ok(XPathValue::Number(*n)),
    }
}

fn eval_union(ctx: &EvalContext<'_>, lhs: &Expr, rhs: &Expr) -> Result<XPathValue, XPathError> {
    let XPathValue::NodeSet(mut left) = eval_expr(ctx, lhs)? else {
        return Err(XPathError::Type("union operand must be a node-set".into()));
    };
    let XPathValue::NodeSet(right) = eval_expr(ctx, rhs)? else {
        return Err(XPathError::Type("union operand must be a node-set".into()));
    };
    left.extend(right);
    sort_and_dedup(ctx.order, &mut left);
    Ok(XPathValue::NodeSet(left))
}

fn eval_steps(
    ctx: &EvalContext<'_>,
    mut current: Vec<XPathNode>,
    steps: &[Step],
) -> Result<Vec<XPathNode>, XPathError> {
    for step in steps {
        let mut next = Vec::new();
        for node in &current {
            let mut step_nodes = collect_axis_nodes(ctx, node, step.axis, &step.node_test);
            for predicate in &step.predicates {
                step_nodes = apply_predicate(ctx, step_nodes, predicate)?;
            }
            next.extend(step_nodes);
        }
        sort_and_dedup(ctx.order, &mut next);
        current = next;
        if current.is_empty() {
            break;
        }
    }
    Ok(current)
}

fn apply_predicate(
    ctx: &EvalContext<'_>,
    nodes: Vec<XPathNode>,
    predicate: &Expr,
) -> Result<Vec<XPathNode>, XPathError> {
    let size = nodes.len();
    let mut kept = Vec::new();
    for (idx, node) in nodes.into_iter().enumerate() {
        let pos = idx + 1;
        let sub = ctx.with_node(node.clone(), pos, size);
        let value = eval_expr(&sub, predicate)?;
        let matches = match value {
            XPathValue::Number(n) => ieee_eq(usize_to_f64(pos), n),
            other => to_boolean(&other),
        };
        if matches {
            kept.push(node);
        }
    }
    Ok(kept)
}

fn is_xpath_tree_node(data: &NodeData) -> bool {
    !matches!(data, NodeData::Doctype { .. } | NodeData::AnonymousBlock(_))
}

fn is_xmlns_attr(attr: &blitz_dom::Attribute) -> bool {
    attr.name.ns == ns!(xmlns)
        || (attr.name.ns == ns!()
            && (&*attr.name.local == "xmlns" || attr.name.local.starts_with("xmlns:")))
}

fn collect_axis_nodes(
    ctx: &EvalContext<'_>,
    origin: &XPathNode,
    axis: Axis,
    test: &NodeTest,
) -> Vec<XPathNode> {
    let mut out = Vec::new();
    match axis {
        Axis::Current => {
            if matches_node_test(ctx, origin, axis, test) {
                out.push(origin.clone());
            }
        }
        Axis::Parent => {
            if let Some(parent) = node_parent(ctx.base, origin)
                && matches_node_test(ctx, &parent, axis, test)
            {
                out.push(parent);
            }
        }
        Axis::Child => {
            if let XPathNode::Tree(id) = origin
                && let Some(node) = ctx.base.get_node(*id)
            {
                for &child_id in &node.children {
                    let cand = XPathNode::Tree(child_id);
                    if matches_node_test(ctx, &cand, axis, test) {
                        out.push(cand);
                    }
                }
            }
        }
        Axis::Attribute => {
            if let XPathNode::Tree(id) = origin
                && let Some(NodeData::Element(el)) = ctx.base.get_node(*id).map(|n| &n.data)
            {
                for (attr_index, attr) in el.attrs.iter().enumerate() {
                    if is_xmlns_attr(attr) {
                        continue;
                    }
                    let cand = XPathNode::Attr {
                        owner: *id,
                        attr_index,
                        ns: attr.name.ns.to_string(),
                        local: attr.name.local.to_string(),
                        prefix: attr.name.prefix.as_ref().map(ToString::to_string),
                        value: attr.value.clone(),
                    };
                    if matches_node_test(ctx, &cand, axis, test) {
                        out.push(cand);
                    }
                }
            }
        }
        Axis::Namespace => {}
        Axis::Ancestor => collect_ancestors(ctx, origin, false, axis, test, &mut out),
        Axis::AncestorOrSelf => collect_ancestors(ctx, origin, true, axis, test, &mut out),
        Axis::Descendant => {
            if let XPathNode::Tree(id) = origin {
                collect_descendants(ctx, *id, false, axis, test, &mut out);
            }
        }
        Axis::DescendantOrSelf => match origin {
            XPathNode::Tree(id) => collect_descendants(ctx, *id, true, axis, test, &mut out),
            XPathNode::Attr { .. } => {
                if matches_node_test(ctx, origin, axis, test) {
                    out.push(origin.clone());
                }
            }
        },
        Axis::FollowingSibling => collect_siblings(ctx, origin, true, axis, test, &mut out),
        Axis::PrecedingSibling => collect_siblings(ctx, origin, false, axis, test, &mut out),
        Axis::Following => collect_following(ctx, origin, axis, test, &mut out),
        Axis::Preceding => collect_preceding(ctx, origin, axis, test, &mut out),
    }
    if axis.is_reverse() {
        out.sort_by_key(|n| std::cmp::Reverse(node_sort_key(ctx.order, n)));
    }
    out
}

fn node_parent(base: &BaseDocument, node: &XPathNode) -> Option<XPathNode> {
    match node {
        XPathNode::Attr { owner, .. } => Some(XPathNode::Tree(*owner)),
        XPathNode::Tree(id) => base
            .get_node(*id)
            .and_then(|n| n.parent)
            .map(XPathNode::Tree),
    }
}

fn collect_ancestors(
    ctx: &EvalContext<'_>,
    origin: &XPathNode,
    include_self: bool,
    axis: Axis,
    test: &NodeTest,
    out: &mut Vec<XPathNode>,
) {
    if include_self && matches_node_test(ctx, origin, axis, test) {
        out.push(origin.clone());
    }
    let mut cur = node_parent(ctx.base, origin);
    while let Some(node) = cur {
        if matches_node_test(ctx, &node, axis, test) {
            out.push(node.clone());
        }
        cur = node_parent(ctx.base, &node);
    }
}

fn collect_descendants(
    ctx: &EvalContext<'_>,
    id: BlitzId,
    include_self: bool,
    axis: Axis,
    test: &NodeTest,
    out: &mut Vec<XPathNode>,
) {
    if include_self {
        let cand = XPathNode::Tree(id);
        if matches_node_test(ctx, &cand, axis, test) {
            out.push(cand);
        }
    }
    let Some(node) = ctx.base.get_node(id) else {
        return;
    };
    for &child in &node.children {
        collect_descendants(ctx, child, true, axis, test, out);
    }
}

fn collect_siblings(
    ctx: &EvalContext<'_>,
    origin: &XPathNode,
    forward: bool,
    axis: Axis,
    test: &NodeTest,
    out: &mut Vec<XPathNode>,
) {
    let XPathNode::Tree(id) = origin else {
        return;
    };
    let Some(parent_id) = ctx.base.get_node(*id).and_then(|n| n.parent) else {
        return;
    };
    let Some(parent) = ctx.base.get_node(parent_id) else {
        return;
    };
    let Some(pos) = parent.children.iter().position(|&c| c == *id) else {
        return;
    };
    let slice = if forward {
        parent.children.get(pos + 1..).unwrap_or(&[])
    } else {
        parent.children.get(..pos).unwrap_or(&[])
    };
    for &sib in slice {
        let cand = XPathNode::Tree(sib);
        if matches_node_test(ctx, &cand, axis, test) {
            out.push(cand);
        }
    }
}

fn collect_following(
    ctx: &EvalContext<'_>,
    origin: &XPathNode,
    axis: Axis,
    test: &NodeTest,
    out: &mut Vec<XPathNode>,
) {
    let mut cur = match origin {
        XPathNode::Tree(id) => *id,
        XPathNode::Attr { owner, .. } => {
            collect_descendants(ctx, *owner, false, axis, test, out);
            *owner
        }
    };
    while let Some(parent_id) = ctx.base.get_node(cur).and_then(|n| n.parent) {
        if let Some(parent) = ctx.base.get_node(parent_id)
            && let Some(pos) = parent.children.iter().position(|&c| c == cur)
        {
            for &sib in parent.children.get(pos + 1..).unwrap_or(&[]) {
                collect_descendants(ctx, sib, true, axis, test, out);
            }
        }
        cur = parent_id;
    }
}

fn collect_preceding(
    ctx: &EvalContext<'_>,
    origin: &XPathNode,
    axis: Axis,
    test: &NodeTest,
    out: &mut Vec<XPathNode>,
) {
    let mut cur = match origin {
        XPathNode::Tree(id) => *id,
        XPathNode::Attr { owner, .. } => *owner,
    };
    while let Some(parent_id) = ctx.base.get_node(cur).and_then(|n| n.parent) {
        if let Some(parent) = ctx.base.get_node(parent_id)
            && let Some(pos) = parent.children.iter().position(|&c| c == cur)
        {
            for &sib in parent.children.get(..pos).unwrap_or(&[]).iter().rev() {
                collect_descendants(ctx, sib, true, axis, test, out);
            }
        }
        cur = parent_id;
    }
}

// https://dom.spec.whatwg.org/#xpath
fn matches_node_test(ctx: &EvalContext<'_>, node: &XPathNode, axis: Axis, test: &NodeTest) -> bool {
    match node {
        XPathNode::Tree(id) => {
            let Some(data) = ctx.base.get_node(*id).map(|n| &n.data) else {
                return false;
            };
            if !is_xpath_tree_node(data) {
                return false;
            }
            match test {
                NodeTest::Node => true,
                NodeTest::Text => matches!(data, NodeData::Text(_) | NodeData::CDataSection { .. }),
                NodeTest::Comment => matches!(data, NodeData::Comment { .. }),
                NodeTest::ProcessingInstruction(expected) => match data {
                    NodeData::ProcessingInstruction { target, .. } => {
                        expected.as_deref().is_none_or(|exp| exp == target)
                    }
                    _ => false,
                },
                NodeTest::Wildcard | NodeTest::PrefixWildcard { .. } | NodeTest::Name { .. } => {
                    if axis == Axis::Attribute || axis == Axis::Namespace {
                        return false;
                    }
                    let NodeData::Element(el) = data else {
                        return false;
                    };
                    matches_element_name(ctx.is_html, el, test)
                }
            }
        }
        XPathNode::Attr {
            owner, ns, local, ..
        } => match test {
            NodeTest::Node => true,
            NodeTest::Wildcard => axis == Axis::Attribute,
            NodeTest::PrefixWildcard { resolved_ns, .. } => {
                axis == Axis::Attribute && resolved_ns.as_deref().unwrap_or("") == ns.as_str()
            }
            NodeTest::Name {
                prefix,
                local: expected_local,
                resolved_ns,
            } => {
                if axis != Axis::Attribute {
                    return false;
                }
                if prefix.is_some() {
                    resolved_ns.as_deref().unwrap_or("") == ns.as_str() && local == expected_local
                } else {
                    if !ns.is_empty() {
                        return false;
                    }
                    let html_owner = ctx.is_html
                        && ctx
                            .base
                            .get_node(*owner)
                            .and_then(|n| n.data.downcast_element())
                            .is_some_and(|el| el.name.ns == ns!(html));
                    if html_owner {
                        *local == expected_local.to_ascii_lowercase()
                    } else {
                        local == expected_local
                    }
                }
            }
            NodeTest::Comment | NodeTest::Text | NodeTest::ProcessingInstruction(_) => false,
        },
    }
}

fn matches_element_name(is_html: bool, el: &blitz_dom::ElementData, test: &NodeTest) -> bool {
    match test {
        NodeTest::Wildcard => true,
        NodeTest::PrefixWildcard { resolved_ns, .. } => {
            &*el.name.ns == resolved_ns.as_deref().unwrap_or("")
        }
        NodeTest::Name {
            prefix,
            local,
            resolved_ns,
        } => {
            if prefix.is_some() {
                &*el.name.ns == resolved_ns.as_deref().unwrap_or("")
                    && &*el.name.local == local.as_str()
            } else if is_html {
                (el.name.ns == ns!(html) && &*el.name.local == local.to_ascii_lowercase().as_str())
                    || (el.name.ns == ns!() && &*el.name.local == local.as_str())
            } else {
                el.name.ns == ns!() && &*el.name.local == local.as_str()
            }
        }
        _ => false,
    }
}

// https://www.w3.org/TR/1999/REC-xpath-19991116/#booleans
fn eval_binary(base: &BaseDocument, op: BinOp, lhs: &XPathValue, rhs: &XPathValue) -> XPathValue {
    match op {
        BinOp::Add => XPathValue::Number(to_number(base, lhs) + to_number(base, rhs)),
        BinOp::Sub => XPathValue::Number(to_number(base, lhs) - to_number(base, rhs)),
        BinOp::Mul => XPathValue::Number(to_number(base, lhs) * to_number(base, rhs)),
        BinOp::Div => XPathValue::Number(to_number(base, lhs) / to_number(base, rhs)),
        BinOp::Mod => XPathValue::Number(to_number(base, lhs) % to_number(base, rhs)),
        BinOp::Eq | BinOp::NotEq => XPathValue::Boolean(compare_equality(base, op, lhs, rhs)),
        BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq => {
            XPathValue::Boolean(compare_relational(base, op, lhs, rhs))
        }
    }
}

fn compare_equality(base: &BaseDocument, op: BinOp, lhs: &XPathValue, rhs: &XPathValue) -> bool {
    let want_eq = op == BinOp::Eq;
    match (lhs, rhs) {
        (XPathValue::NodeSet(a), XPathValue::NodeSet(b)) => {
            let b_strs: Vec<String> = b.iter().map(|n| node_string_value(base, n)).collect();
            a.iter().any(|na| {
                let sa = node_string_value(base, na);
                b_strs.iter().any(|sb| (sa == *sb) == want_eq)
            })
        }
        (XPathValue::NodeSet(nodes), other) | (other, XPathValue::NodeSet(nodes)) => {
            compare_nodeset_eq(base, nodes, other, want_eq)
        }
        (XPathValue::Boolean(a), other) | (other, XPathValue::Boolean(a)) => {
            (*a == to_boolean(other)) == want_eq
        }
        (XPathValue::Number(a), other) | (other, XPathValue::Number(a)) => {
            let eq = ieee_eq(*a, to_number(base, other));
            if want_eq { eq } else { !eq }
        }
        (XPathValue::String(a), XPathValue::String(b)) => (a == b) == want_eq,
    }
}

fn compare_nodeset_eq(
    base: &BaseDocument,
    nodes: &[XPathNode],
    other: &XPathValue,
    want_eq: bool,
) -> bool {
    match other {
        XPathValue::Boolean(b) => (nodes.is_empty() != *b) == want_eq,
        XPathValue::Number(n) => nodes.iter().any(|node| {
            let eq = ieee_eq(str_to_number(&node_string_value(base, node)), *n);
            if want_eq { eq } else { !eq }
        }),
        XPathValue::String(s) => nodes
            .iter()
            .any(|node| (node_string_value(base, node) == *s) == want_eq),
        XPathValue::NodeSet(_) => false,
    }
}

fn compare_relational(base: &BaseDocument, op: BinOp, lhs: &XPathValue, rhs: &XPathValue) -> bool {
    let cmp = |a: f64, b: f64| match op {
        BinOp::Lt => a < b,
        BinOp::LtEq => a <= b,
        BinOp::Gt => a > b,
        BinOp::GtEq => a >= b,
        _ => false,
    };
    match (lhs, rhs) {
        (XPathValue::NodeSet(a), XPathValue::NodeSet(b)) => {
            let b_nums: Vec<f64> = b
                .iter()
                .map(|n| str_to_number(&node_string_value(base, n)))
                .collect();
            a.iter().any(|na| {
                let va = str_to_number(&node_string_value(base, na));
                b_nums.iter().any(|&vb| cmp(va, vb))
            })
        }
        (XPathValue::NodeSet(nodes), other) => {
            let rhs_num = to_number(base, other);
            nodes
                .iter()
                .any(|n| cmp(str_to_number(&node_string_value(base, n)), rhs_num))
        }
        (other, XPathValue::NodeSet(nodes)) => {
            let lhs_num = to_number(base, other);
            nodes
                .iter()
                .any(|n| cmp(lhs_num, str_to_number(&node_string_value(base, n))))
        }
        _ => cmp(to_number(base, lhs), to_number(base, rhs)),
    }
}
