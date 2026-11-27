// https://dom.spec.whatwg.org/#xpath
// https://www.w3.org/TR/1999/REC-xpath-19991116/

mod eval;
mod functions;
mod lexer;
mod parser;

use blitz_dom::BaseDocument;
use blitz_traits::node_id::NodeId as BlitzId;

use eval::{EvalContext, build_doc_order, eval_expr, tree_root};
use parser::{Expr, NodeTest, Step, parse_expr};

pub use functions::{to_boolean, to_number, to_string};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum XPathError {
    Syntax(String),
    Namespace(String),
    Type(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum XPathNode {
    Tree(BlitzId),
    Attr {
        owner: BlitzId,
        attr_index: usize,
        ns: String,
        local: String,
        prefix: Option<String>,
        value: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum XPathValue {
    NodeSet(Vec<XPathNode>),
    Boolean(bool),
    Number(f64),
    String(String),
}

// https://dom.spec.whatwg.org/#callbackdef-xpathnsresolver
pub trait NsResolver {
    fn lookup(&mut self, prefix: &str) -> Result<Option<String>, XPathError>;
}

pub struct NullResolver;

impl NsResolver for NullResolver {
    fn lookup(&mut self, _prefix: &str) -> Result<Option<String>, XPathError> {
        Ok(None)
    }
}

#[derive(Clone, Debug)]
pub struct CompiledExpr {
    expr: Expr,
}

impl CompiledExpr {
    // https://dom.spec.whatwg.org/#dom-xpathevaluatorbase-createexpression
    pub fn compile(source: &str, resolver: &mut dyn NsResolver) -> Result<Self, XPathError> {
        let mut expr = parse_expr(source)?;
        resolve_expr_namespaces(&mut expr, resolver)?;
        Ok(Self { expr })
    }

    // https://dom.spec.whatwg.org/#dom-xpathexpression-evaluate
    pub fn evaluate(
        &self,
        base: &BaseDocument,
        is_html: bool,
        context_node: XPathNode,
    ) -> Result<XPathValue, XPathError> {
        let root = tree_root(base, &context_node);
        let order = build_doc_order(base, root);
        let ctx = EvalContext {
            base,
            is_html,
            root,
            order: &order,
            node: context_node,
            position: 1,
            size: 1,
        };
        eval_expr(&ctx, &self.expr)
    }
}

fn resolve_expr_namespaces(
    expr: &mut Expr,
    resolver: &mut dyn NsResolver,
) -> Result<(), XPathError> {
    match expr {
        Expr::Or(a, b) | Expr::And(a, b) | Expr::Binary(_, a, b) | Expr::Union(a, b) => {
            resolve_expr_namespaces(a, resolver)?;
            resolve_expr_namespaces(b, resolver)
        }
        Expr::Neg(a) => resolve_expr_namespaces(a, resolver),
        Expr::Path { base, steps } => {
            resolve_expr_namespaces(base, resolver)?;
            resolve_steps_namespaces(steps, resolver)
        }
        Expr::LocationPath { steps, .. } => resolve_steps_namespaces(steps, resolver),
        Expr::Filter { expr, predicates } => {
            resolve_expr_namespaces(expr, resolver)?;
            for pred in predicates {
                resolve_expr_namespaces(pred, resolver)?;
            }
            Ok(())
        }
        Expr::FunctionCall { prefix, args, .. } => {
            if let Some(p) = prefix {
                resolve_prefix(p, resolver)?;
            }
            for arg in args {
                resolve_expr_namespaces(arg, resolver)?;
            }
            Ok(())
        }
        Expr::Variable { prefix, .. } => {
            if let Some(p) = prefix {
                resolve_prefix(p, resolver)?;
            }
            Ok(())
        }
        Expr::Literal(_) | Expr::Number(_) => Ok(()),
    }
}

fn resolve_steps_namespaces(
    steps: &mut [Step],
    resolver: &mut dyn NsResolver,
) -> Result<(), XPathError> {
    for step in steps {
        match &mut step.node_test {
            NodeTest::PrefixWildcard {
                prefix,
                resolved_ns,
            }
            | NodeTest::Name {
                prefix: Some(prefix),
                resolved_ns,
                ..
            } => {
                *resolved_ns = Some(resolve_prefix(prefix, resolver)?);
            }
            _ => {}
        }
        for pred in &mut step.predicates {
            resolve_expr_namespaces(pred, resolver)?;
        }
    }
    Ok(())
}

fn resolve_prefix(prefix: &str, resolver: &mut dyn NsResolver) -> Result<String, XPathError> {
    match resolver.lookup(prefix)? {
        Some(uri) => Ok(uri),
        None => Err(XPathError::Namespace(format!(
            "unresolved namespace prefix '{prefix}'"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_test_html(html: &str) -> crate::Parsed {
        crate::parse_html(html, blitz_dom::DocumentConfig::default())
    }

    #[test]
    fn evaluates_axes_predicates_and_functions() {
        let parsed = parse_test_html(
            "<html><body><section id='s1'><p class='a'>one</p><p class='b'>two</p><p class='a'>three</p></section></body></html>",
        );
        let root = XPathNode::Tree(parsed.document.base.root_node().id);
        let eval_str = |src: &str| {
            CompiledExpr::compile(src, &mut NullResolver)
                .and_then(|c| c.evaluate(&parsed.document.base, true, root.clone()))
        };
        assert!(matches!(
            eval_str("//section[@id='s1']/p[@class='a']"),
            Ok(XPathValue::NodeSet(ref nodes)) if nodes.len() == 2
        ));
        assert_eq!(
            eval_str("count(//p[2]/preceding-sibling::p[1])"),
            Ok(XPathValue::Number(1.0))
        );
        assert_eq!(
            eval_str("name(//p[1]/ancestor-or-self::section)"),
            Ok(XPathValue::String("section".into()))
        );
        assert_eq!(
            eval_str("normalize-space('  hello \t\n world  ')"),
            Ok(XPathValue::String("hello world".into()))
        );
        assert_eq!(
            eval_str("substring('12345', 1.5, 2.6)"),
            Ok(XPathValue::String("234".into()))
        );
    }

    #[test]
    fn rejects_invalid_syntax_and_unresolved_prefix() {
        assert!(matches!(
            CompiledExpr::compile("//div[", &mut NullResolver),
            Err(XPathError::Syntax(_))
        ));
        assert!(matches!(
            CompiledExpr::compile("//foo:bar", &mut NullResolver),
            Err(XPathError::Namespace(_))
        ));
    }
}
