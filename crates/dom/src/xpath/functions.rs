// https://www.w3.org/TR/1999/REC-xpath-19991116/#corelib

use blitz_dom::{BaseDocument, NodeData};
use markup5ever::ns;

use super::eval::{EvalContext, eval_expr};
use super::parser::Expr;
use super::{XPathError, XPathNode, XPathValue};

const XPATH_SPACE: [char; 4] = [' ', '\t', '\r', '\n'];

pub(crate) fn usize_to_f64(n: usize) -> f64 {
    f64::from(u32::try_from(n).unwrap_or(u32::MAX))
}

pub(crate) fn ieee_eq(a: f64, b: f64) -> bool {
    a.partial_cmp(&b) == Some(std::cmp::Ordering::Equal)
}

// https://www.w3.org/TR/1999/REC-xpath-19991116/#function-boolean
#[must_use]
pub fn to_boolean(val: &XPathValue) -> bool {
    match val {
        XPathValue::Boolean(b) => *b,
        XPathValue::Number(n) => !n.is_nan() && !ieee_eq(*n, 0.0),
        XPathValue::String(s) => !s.is_empty(),
        XPathValue::NodeSet(nodes) => !nodes.is_empty(),
    }
}

// https://www.w3.org/TR/1999/REC-xpath-19991116/#function-number
#[must_use]
pub fn to_number(base: &BaseDocument, val: &XPathValue) -> f64 {
    match val {
        XPathValue::Number(n) => *n,
        XPathValue::Boolean(b) => {
            if *b {
                1.0
            } else {
                0.0
            }
        }
        XPathValue::String(s) => str_to_number(s),
        XPathValue::NodeSet(_) => str_to_number(&to_string(base, val)),
    }
}

pub(crate) fn str_to_number(s: &str) -> f64 {
    let trimmed = s.trim_matches(XPATH_SPACE);
    if trimmed.is_empty() {
        return f64::NAN;
    }
    let body = trimmed.strip_prefix('-').unwrap_or(trimmed);
    if body.is_empty()
        || !body.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        || body.bytes().filter(|&b| b == b'.').count() > 1
        || body == "."
    {
        return f64::NAN;
    }
    trimmed.parse::<f64>().unwrap_or(f64::NAN)
}

// https://www.w3.org/TR/1999/REC-xpath-19991116/#function-string
#[must_use]
pub fn to_string(base: &BaseDocument, val: &XPathValue) -> String {
    match val {
        XPathValue::String(s) => s.clone(),
        XPathValue::Boolean(b) => if *b { "true" } else { "false" }.to_owned(),
        XPathValue::Number(n) => number_to_string(*n),
        XPathValue::NodeSet(nodes) => nodes
            .first()
            .map_or_else(String::new, |node| node_string_value(base, node)),
    }
}

pub(crate) fn number_to_string(n: f64) -> String {
    if n.is_nan() {
        return "NaN".to_owned();
    }
    if ieee_eq(n, 0.0) {
        return "0".to_owned();
    }
    if n.is_infinite() {
        return if n.is_sign_negative() {
            "-Infinity".to_owned()
        } else {
            "Infinity".to_owned()
        };
    }
    if ieee_eq(n.fract(), 0.0) && n.abs() < 1e20 {
        return format!("{n:.0}");
    }
    format!("{n}")
}

// https://www.w3.org/TR/1999/REC-xpath-19991116/#dt-string-value
pub(crate) fn node_string_value(base: &BaseDocument, node: &XPathNode) -> String {
    match node {
        XPathNode::Attr { value, .. } => value.clone(),
        XPathNode::Tree(id) => {
            let Some(n) = base.get_node(*id) else {
                return String::new();
            };
            match &n.data {
                NodeData::Text(text) => text.content.clone(),
                NodeData::CDataSection { contents }
                | NodeData::Comment { contents }
                | NodeData::ProcessingInstruction { contents, .. } => contents.clone(),
                NodeData::Document(_)
                | NodeData::Element(_)
                | NodeData::Fragment { .. }
                | NodeData::ShadowRoot { .. } => {
                    let mut out = String::new();
                    collect_descendant_text(base, *id, &mut out);
                    out
                }
                NodeData::Doctype { .. } | NodeData::AnonymousBlock(_) => String::new(),
            }
        }
    }
}

fn collect_descendant_text(
    base: &BaseDocument,
    id: blitz_traits::node_id::NodeId,
    out: &mut String,
) {
    let Some(node) = base.get_node(id) else {
        return;
    };
    for &child_id in &node.children {
        let Some(child) = base.get_node(child_id) else {
            continue;
        };
        match &child.data {
            NodeData::Text(text) => out.push_str(&text.content),
            NodeData::CDataSection { contents } => out.push_str(contents),
            NodeData::Element(_) => collect_descendant_text(base, child_id, out),
            _ => {}
        }
    }
}

fn check_arity(name: &str, args: &[Expr], min: usize, max: usize) -> Result<(), XPathError> {
    if args.len() < min || args.len() > max {
        return Err(XPathError::Syntax(format!(
            "wrong number of arguments for {name}()"
        )));
    }
    Ok(())
}

pub(crate) fn call_function(
    ctx: &EvalContext<'_>,
    prefix: Option<&str>,
    name: &str,
    args: &[Expr],
) -> Result<XPathValue, XPathError> {
    if prefix.is_some() {
        return Err(XPathError::Syntax(format!(
            "unknown prefixed function '{name}'"
        )));
    }
    match name {
        "last" => {
            check_arity(name, args, 0, 0)?;
            Ok(XPathValue::Number(usize_to_f64(ctx.size)))
        }
        "position" => {
            check_arity(name, args, 0, 0)?;
            Ok(XPathValue::Number(usize_to_f64(ctx.position)))
        }
        "count" => fn_count(ctx, args),
        "id" => fn_id(ctx, args),
        "local-name" | "namespace-uri" | "name" => fn_name_info(ctx, name, args),
        "string" => fn_string(ctx, args),
        "concat" => fn_concat(ctx, args),
        "starts-with" => fn_two_str_bool(ctx, name, args, |a, b| a.starts_with(b)),
        "contains" => fn_two_str_bool(ctx, name, args, |a, b| a.contains(b)),
        "substring-before" => fn_substring_before(ctx, args),
        "substring-after" => fn_substring_after(ctx, args),
        "substring" => fn_substring(ctx, args),
        "string-length" => fn_string_length(ctx, args),
        "normalize-space" => fn_normalize_space(ctx, args),
        "translate" => fn_translate(ctx, args),
        "boolean" => {
            check_arity(name, args, 1, 1)?;
            let v = eval_expr(ctx, &args[0])?;
            Ok(XPathValue::Boolean(to_boolean(&v)))
        }
        "not" => {
            check_arity(name, args, 1, 1)?;
            let v = eval_expr(ctx, &args[0])?;
            Ok(XPathValue::Boolean(!to_boolean(&v)))
        }
        "true" => {
            check_arity(name, args, 0, 0)?;
            Ok(XPathValue::Boolean(true))
        }
        "false" => {
            check_arity(name, args, 0, 0)?;
            Ok(XPathValue::Boolean(false))
        }
        "lang" => fn_lang(ctx, args),
        "number" => fn_number(ctx, args),
        "sum" => fn_sum(ctx, args),
        "floor" | "ceiling" | "round" => fn_rounding(ctx, name, args),
        _ => Err(XPathError::Syntax(format!("unknown function '{name}'"))),
    }
}

fn fn_count(ctx: &EvalContext<'_>, args: &[Expr]) -> Result<XPathValue, XPathError> {
    check_arity("count", args, 1, 1)?;
    match eval_expr(ctx, &args[0])? {
        XPathValue::NodeSet(nodes) => Ok(XPathValue::Number(usize_to_f64(nodes.len()))),
        _ => Err(XPathError::Type(
            "count() argument must be a node-set".into(),
        )),
    }
}

// https://www.w3.org/TR/1999/REC-xpath-19991116/#function-id
fn fn_id(ctx: &EvalContext<'_>, args: &[Expr]) -> Result<XPathValue, XPathError> {
    check_arity("id", args, 1, 1)?;
    let arg = eval_expr(ctx, &args[0])?;
    let mut ids = Vec::new();
    match arg {
        XPathValue::NodeSet(nodes) => {
            for node in &nodes {
                for token in node_string_value(ctx.base, node)
                    .split(XPATH_SPACE)
                    .filter(|s| !s.is_empty())
                {
                    ids.push(token.to_owned());
                }
            }
        }
        other => {
            for token in to_string(ctx.base, &other)
                .split(XPATH_SPACE)
                .filter(|s| !s.is_empty())
            {
                ids.push(token.to_owned());
            }
        }
    }
    let mut matched = Vec::new();
    let mut seen = std::collections::HashSet::new();
    if !ids.is_empty() {
        collect_elements_by_ids(ctx.base, ctx.root, &ids, &mut seen, &mut matched);
    }
    Ok(XPathValue::NodeSet(matched))
}

fn collect_elements_by_ids(
    base: &BaseDocument,
    node_id: blitz_traits::node_id::NodeId,
    ids: &[String],
    seen: &mut std::collections::HashSet<String>,
    out: &mut Vec<XPathNode>,
) {
    let Some(node) = base.get_node(node_id) else {
        return;
    };
    if let NodeData::Element(el) = &node.data
        && let Some(val) = el.attrs.iter().find_map(|a| {
            (a.name.ns == ns!() && &*a.name.local == "id").then_some(a.value.as_str())
        })
        && ids.iter().any(|wanted| wanted == val)
        && seen.insert(val.to_owned())
    {
        out.push(XPathNode::Tree(node_id));
    }
    for &child in &node.children {
        collect_elements_by_ids(base, child, ids, seen, out);
    }
}

fn fn_name_info(
    ctx: &EvalContext<'_>,
    fn_name: &str,
    args: &[Expr],
) -> Result<XPathValue, XPathError> {
    check_arity(fn_name, args, 0, 1)?;
    let target = if let Some(arg) = args.first() {
        match eval_expr(ctx, arg)? {
            XPathValue::NodeSet(nodes) => nodes.into_iter().next(),
            _ => {
                return Err(XPathError::Type(format!(
                    "{fn_name}() argument must be a node-set"
                )));
            }
        }
    } else {
        Some(ctx.node.clone())
    };
    let Some(node) = target else {
        return Ok(XPathValue::String(String::new()));
    };
    let text = match fn_name {
        "local-name" => node_local_name(ctx.base, &node),
        "namespace-uri" => node_namespace_uri(ctx.base, &node),
        _ => node_qname(ctx.base, &node),
    };
    Ok(XPathValue::String(text))
}

fn node_local_name(base: &BaseDocument, node: &XPathNode) -> String {
    match node {
        XPathNode::Attr { local, .. } => local.clone(),
        XPathNode::Tree(id) => match base.get_node(*id).map(|n| &n.data) {
            Some(NodeData::Element(el)) => el.name.local.to_string(),
            Some(NodeData::ProcessingInstruction { target, .. }) => target.clone(),
            _ => String::new(),
        },
    }
}

fn node_namespace_uri(base: &BaseDocument, node: &XPathNode) -> String {
    match node {
        XPathNode::Attr { ns, .. } => ns.clone(),
        XPathNode::Tree(id) => match base.get_node(*id).map(|n| &n.data) {
            Some(NodeData::Element(el)) => el.name.ns.to_string(),
            _ => String::new(),
        },
    }
}

fn node_qname(base: &BaseDocument, node: &XPathNode) -> String {
    match node {
        XPathNode::Attr { prefix, local, .. } => match prefix {
            Some(p) if !p.is_empty() => format!("{p}:{local}"),
            _ => local.clone(),
        },
        XPathNode::Tree(id) => match base.get_node(*id).map(|n| &n.data) {
            Some(NodeData::Element(el)) => match &el.name.prefix {
                Some(p) if !p.is_empty() => format!("{p}:{}", el.name.local),
                _ => el.name.local.to_string(),
            },
            Some(NodeData::ProcessingInstruction { target, .. }) => target.clone(),
            _ => String::new(),
        },
    }
}

fn fn_string(ctx: &EvalContext<'_>, args: &[Expr]) -> Result<XPathValue, XPathError> {
    check_arity("string", args, 0, 1)?;
    if let Some(arg) = args.first() {
        let val = eval_expr(ctx, arg)?;
        Ok(XPathValue::String(to_string(ctx.base, &val)))
    } else {
        Ok(XPathValue::String(node_string_value(ctx.base, &ctx.node)))
    }
}

fn fn_concat(ctx: &EvalContext<'_>, args: &[Expr]) -> Result<XPathValue, XPathError> {
    check_arity("concat", args, 2, usize::MAX)?;
    let mut out = String::new();
    for arg in args {
        let val = eval_expr(ctx, arg)?;
        out.push_str(&to_string(ctx.base, &val));
    }
    Ok(XPathValue::String(out))
}

fn fn_two_str_bool(
    ctx: &EvalContext<'_>,
    name: &str,
    args: &[Expr],
    pred: fn(&str, &str) -> bool,
) -> Result<XPathValue, XPathError> {
    check_arity(name, args, 2, 2)?;
    let a = to_string(ctx.base, &eval_expr(ctx, &args[0])?);
    let b = to_string(ctx.base, &eval_expr(ctx, &args[1])?);
    Ok(XPathValue::Boolean(pred(&a, &b)))
}

fn fn_substring_before(ctx: &EvalContext<'_>, args: &[Expr]) -> Result<XPathValue, XPathError> {
    check_arity("substring-before", args, 2, 2)?;
    let a = to_string(ctx.base, &eval_expr(ctx, &args[0])?);
    let b = to_string(ctx.base, &eval_expr(ctx, &args[1])?);
    let res = a
        .find(&b)
        .map_or_else(String::new, |idx| a[..idx].to_owned());
    Ok(XPathValue::String(res))
}

fn fn_substring_after(ctx: &EvalContext<'_>, args: &[Expr]) -> Result<XPathValue, XPathError> {
    check_arity("substring-after", args, 2, 2)?;
    let a = to_string(ctx.base, &eval_expr(ctx, &args[0])?);
    let b = to_string(ctx.base, &eval_expr(ctx, &args[1])?);
    let res = a
        .find(&b)
        .map_or_else(String::new, |idx| a[idx + b.len()..].to_owned());
    Ok(XPathValue::String(res))
}

// https://www.w3.org/TR/1999/REC-xpath-19991116/#function-substring
fn fn_substring(ctx: &EvalContext<'_>, args: &[Expr]) -> Result<XPathValue, XPathError> {
    check_arity("substring", args, 2, 3)?;
    let text = to_string(ctx.base, &eval_expr(ctx, &args[0])?);
    let start = xpath_round(to_number(ctx.base, &eval_expr(ctx, &args[1])?));
    let end = if let Some(len_arg) = args.get(2) {
        let len = xpath_round(to_number(ctx.base, &eval_expr(ctx, len_arg)?));
        start + len
    } else {
        f64::INFINITY
    };
    if start.is_nan() || end.is_nan() {
        return Ok(XPathValue::String(String::new()));
    }
    let out: String = text
        .chars()
        .enumerate()
        .filter_map(|(idx, ch)| {
            let pos = usize_to_f64(idx + 1);
            (pos >= start && pos < end).then_some(ch)
        })
        .collect();
    Ok(XPathValue::String(out))
}

fn fn_string_length(ctx: &EvalContext<'_>, args: &[Expr]) -> Result<XPathValue, XPathError> {
    check_arity("string-length", args, 0, 1)?;
    let text = if let Some(arg) = args.first() {
        to_string(ctx.base, &eval_expr(ctx, arg)?)
    } else {
        node_string_value(ctx.base, &ctx.node)
    };
    Ok(XPathValue::Number(usize_to_f64(text.chars().count())))
}

fn fn_normalize_space(ctx: &EvalContext<'_>, args: &[Expr]) -> Result<XPathValue, XPathError> {
    check_arity("normalize-space", args, 0, 1)?;
    let text = if let Some(arg) = args.first() {
        to_string(ctx.base, &eval_expr(ctx, arg)?)
    } else {
        node_string_value(ctx.base, &ctx.node)
    };
    let mut out = String::new();
    for (idx, word) in text
        .split(XPATH_SPACE)
        .filter(|w| !w.is_empty())
        .enumerate()
    {
        if idx > 0 {
            out.push(' ');
        }
        out.push_str(word);
    }
    Ok(XPathValue::String(out))
}

fn fn_translate(ctx: &EvalContext<'_>, args: &[Expr]) -> Result<XPathValue, XPathError> {
    check_arity("translate", args, 3, 3)?;
    let src = to_string(ctx.base, &eval_expr(ctx, &args[0])?);
    let from: Vec<char> = to_string(ctx.base, &eval_expr(ctx, &args[1])?)
        .chars()
        .collect();
    let to: Vec<char> = to_string(ctx.base, &eval_expr(ctx, &args[2])?)
        .chars()
        .collect();
    let mut out = String::with_capacity(src.len());
    for ch in src.chars() {
        if let Some(idx) = from.iter().position(|&f| f == ch) {
            if let Some(&replacement) = to.get(idx) {
                out.push(replacement);
            }
        } else {
            out.push(ch);
        }
    }
    Ok(XPathValue::String(out))
}

// https://www.w3.org/TR/1999/REC-xpath-19991116/#function-lang
fn fn_lang(ctx: &EvalContext<'_>, args: &[Expr]) -> Result<XPathValue, XPathError> {
    check_arity("lang", args, 1, 1)?;
    let target = to_string(ctx.base, &eval_expr(ctx, &args[0])?);
    let mut current = match &ctx.node {
        XPathNode::Tree(id) => Some(*id),
        XPathNode::Attr { owner, .. } => Some(*owner),
    };
    while let Some(id) = current {
        let Some(node) = ctx.base.get_node(id) else {
            break;
        };
        if let NodeData::Element(el) = &node.data
            && let Some(lang) = el.attrs.iter().find_map(|a| {
                ((a.name.ns == ns!(xml) && &*a.name.local == "lang")
                    || (a.name.ns == ns!() && &*a.name.local == "xml:lang"))
                    .then_some(a.value.as_str())
            })
        {
            let matches = lang.eq_ignore_ascii_case(&target)
                || (lang.len() > target.len()
                    && lang
                        .get(..target.len())
                        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(&target))
                    && lang.as_bytes().get(target.len()) == Some(&b'-'));
            return Ok(XPathValue::Boolean(matches));
        }
        current = node.parent;
    }
    Ok(XPathValue::Boolean(false))
}

fn fn_number(ctx: &EvalContext<'_>, args: &[Expr]) -> Result<XPathValue, XPathError> {
    check_arity("number", args, 0, 1)?;
    let val = if let Some(arg) = args.first() {
        eval_expr(ctx, arg)?
    } else {
        XPathValue::String(node_string_value(ctx.base, &ctx.node))
    };
    Ok(XPathValue::Number(to_number(ctx.base, &val)))
}

fn fn_sum(ctx: &EvalContext<'_>, args: &[Expr]) -> Result<XPathValue, XPathError> {
    check_arity("sum", args, 1, 1)?;
    match eval_expr(ctx, &args[0])? {
        XPathValue::NodeSet(nodes) => {
            let mut total = 0.0;
            for node in &nodes {
                total += str_to_number(&node_string_value(ctx.base, node));
            }
            Ok(XPathValue::Number(total))
        }
        _ => Err(XPathError::Type("sum() argument must be a node-set".into())),
    }
}

// https://www.w3.org/TR/1999/REC-xpath-19991116/#function-round
pub(crate) fn xpath_round(n: f64) -> f64 {
    if n.is_nan() || n.is_infinite() || ieee_eq(n, 0.0) {
        return n;
    }
    if (-0.5..0.0).contains(&n) {
        return -0.0;
    }
    (n + 0.5).floor()
}

fn fn_rounding(ctx: &EvalContext<'_>, name: &str, args: &[Expr]) -> Result<XPathValue, XPathError> {
    check_arity(name, args, 1, 1)?;
    let n = to_number(ctx.base, &eval_expr(ctx, &args[0])?);
    let out = match name {
        "floor" => n.floor(),
        "ceiling" => n.ceil(),
        _ => xpath_round(n),
    };
    Ok(XPathValue::Number(out))
}
