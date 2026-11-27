// https://www.w3.org/TR/1999/REC-xpath-19991116/#section-Basics

use super::XPathError;
use super::lexer::{Token, tokenize};

#[derive(Clone, Debug)]
pub(crate) enum Expr {
    Or(Box<Expr>, Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Neg(Box<Expr>),
    Union(Box<Expr>, Box<Expr>),
    Path {
        base: Box<Expr>,
        steps: Vec<Step>,
    },
    LocationPath {
        absolute: bool,
        steps: Vec<Step>,
    },
    Filter {
        expr: Box<Expr>,
        predicates: Vec<Expr>,
    },
    FunctionCall {
        prefix: Option<String>,
        name: String,
        args: Vec<Expr>,
    },
    Variable {
        prefix: Option<String>,
        name: String,
    },
    Literal(String),
    Number(f64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BinOp {
    Eq,
    NotEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
    Add,
    Sub,
    Mul,
    Div,
    Mod,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Axis {
    Ancestor,
    AncestorOrSelf,
    Attribute,
    Child,
    Descendant,
    DescendantOrSelf,
    Following,
    FollowingSibling,
    Namespace,
    Parent,
    Preceding,
    PrecedingSibling,
    Current,
}

impl Axis {
    pub(crate) fn is_reverse(self) -> bool {
        matches!(
            self,
            Self::Ancestor | Self::AncestorOrSelf | Self::Preceding | Self::PrecedingSibling
        )
    }

    fn from_name(name: &str) -> Option<Self> {
        match name {
            "ancestor" => Some(Self::Ancestor),
            "ancestor-or-self" => Some(Self::AncestorOrSelf),
            "attribute" => Some(Self::Attribute),
            "child" => Some(Self::Child),
            "descendant" => Some(Self::Descendant),
            "descendant-or-self" => Some(Self::DescendantOrSelf),
            "following" => Some(Self::Following),
            "following-sibling" => Some(Self::FollowingSibling),
            "namespace" => Some(Self::Namespace),
            "parent" => Some(Self::Parent),
            "preceding" => Some(Self::Preceding),
            "preceding-sibling" => Some(Self::PrecedingSibling),
            "self" => Some(Self::Current),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum NodeTest {
    Wildcard,
    PrefixWildcard {
        prefix: String,
        resolved_ns: Option<String>,
    },
    Name {
        prefix: Option<String>,
        local: String,
        resolved_ns: Option<String>,
    },
    Comment,
    Text,
    ProcessingInstruction(Option<String>),
    Node,
}

#[derive(Clone, Debug)]
pub(crate) struct Step {
    pub(crate) axis: Axis,
    pub(crate) node_test: NodeTest,
    pub(crate) predicates: Vec<Expr>,
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn peek_at(&self, offset: usize) -> Option<&Token> {
        self.tokens.get(self.pos + offset)
    }

    fn bump(&mut self) -> Option<Token> {
        let tok = self.tokens.get(self.pos).cloned()?;
        self.pos += 1;
        Some(tok)
    }

    fn expect(&mut self, expected: &Token) -> Result<(), XPathError> {
        if self.peek() == Some(expected) {
            self.pos += 1;
            Ok(())
        } else {
            Err(XPathError::Syntax(format!(
                "expected {expected:?}, found {:?}",
                self.peek()
            )))
        }
    }

    fn parse_expr(&mut self) -> Result<Expr, XPathError> {
        self.parse_or()
    }

    fn parse_or(&mut self) -> Result<Expr, XPathError> {
        let mut expr = self.parse_and()?;
        while self.peek() == Some(&Token::Or) {
            self.pos += 1;
            let rhs = self.parse_and()?;
            expr = Expr::Or(Box::new(expr), Box::new(rhs));
        }
        Ok(expr)
    }

    fn parse_and(&mut self) -> Result<Expr, XPathError> {
        let mut expr = self.parse_equality()?;
        while self.peek() == Some(&Token::And) {
            self.pos += 1;
            let rhs = self.parse_equality()?;
            expr = Expr::And(Box::new(expr), Box::new(rhs));
        }
        Ok(expr)
    }

    fn parse_equality(&mut self) -> Result<Expr, XPathError> {
        let mut expr = self.parse_relational()?;
        loop {
            let op = match self.peek() {
                Some(Token::Eq) => BinOp::Eq,
                Some(Token::NotEq) => BinOp::NotEq,
                _ => break,
            };
            self.pos += 1;
            let rhs = self.parse_relational()?;
            expr = Expr::Binary(op, Box::new(expr), Box::new(rhs));
        }
        Ok(expr)
    }

    fn parse_relational(&mut self) -> Result<Expr, XPathError> {
        let mut expr = self.parse_additive()?;
        loop {
            let op = match self.peek() {
                Some(Token::Lt) => BinOp::Lt,
                Some(Token::LtEq) => BinOp::LtEq,
                Some(Token::Gt) => BinOp::Gt,
                Some(Token::GtEq) => BinOp::GtEq,
                _ => break,
            };
            self.pos += 1;
            let rhs = self.parse_additive()?;
            expr = Expr::Binary(op, Box::new(expr), Box::new(rhs));
        }
        Ok(expr)
    }

    fn parse_additive(&mut self) -> Result<Expr, XPathError> {
        let mut expr = self.parse_multiplicative()?;
        loop {
            let op = match self.peek() {
                Some(Token::Plus) => BinOp::Add,
                Some(Token::Minus) => BinOp::Sub,
                _ => break,
            };
            self.pos += 1;
            let rhs = self.parse_multiplicative()?;
            expr = Expr::Binary(op, Box::new(expr), Box::new(rhs));
        }
        Ok(expr)
    }

    fn parse_multiplicative(&mut self) -> Result<Expr, XPathError> {
        let mut expr = self.parse_unary()?;
        loop {
            let op = match self.peek() {
                Some(Token::Multiply) => BinOp::Mul,
                Some(Token::Div) => BinOp::Div,
                Some(Token::Mod) => BinOp::Mod,
                _ => break,
            };
            self.pos += 1;
            let rhs = self.parse_unary()?;
            expr = Expr::Binary(op, Box::new(expr), Box::new(rhs));
        }
        Ok(expr)
    }

    fn parse_unary(&mut self) -> Result<Expr, XPathError> {
        if self.peek() == Some(&Token::Minus) {
            self.pos += 1;
            return Ok(Expr::Neg(Box::new(self.parse_unary()?)));
        }
        self.parse_union()
    }

    fn parse_union(&mut self) -> Result<Expr, XPathError> {
        let mut expr = self.parse_path()?;
        while self.peek() == Some(&Token::Pipe) {
            self.pos += 1;
            let rhs = self.parse_path()?;
            expr = Expr::Union(Box::new(expr), Box::new(rhs));
        }
        Ok(expr)
    }

    fn is_primary_start(&self) -> bool {
        match self.peek() {
            Some(Token::LParen | Token::Literal(_) | Token::Number(_) | Token::Variable(_, _)) => {
                true
            }
            Some(Token::Name(prefix, name)) => {
                self.peek_at(1) == Some(&Token::LParen)
                    && (prefix.is_some()
                        || !matches!(
                            name.as_str(),
                            "comment" | "text" | "processing-instruction" | "node"
                        ))
            }
            _ => false,
        }
    }

    fn parse_path(&mut self) -> Result<Expr, XPathError> {
        if self.is_primary_start() {
            let filter = self.parse_filter()?;
            let mut steps = Vec::new();
            match self.peek() {
                Some(Token::Slash) => {
                    self.pos += 1;
                    self.parse_relative_steps(&mut steps)?;
                }
                Some(Token::DoubleSlash) => {
                    self.pos += 1;
                    steps.push(descendant_or_self_step());
                    self.parse_relative_steps(&mut steps)?;
                }
                _ => return Ok(filter),
            }
            return Ok(Expr::Path {
                base: Box::new(filter),
                steps,
            });
        }
        self.parse_location_path()
    }

    fn parse_filter(&mut self) -> Result<Expr, XPathError> {
        let expr = self.parse_primary()?;
        let predicates = self.parse_predicates()?;
        if predicates.is_empty() {
            Ok(expr)
        } else {
            Ok(Expr::Filter {
                expr: Box::new(expr),
                predicates,
            })
        }
    }

    fn parse_primary(&mut self) -> Result<Expr, XPathError> {
        match self.bump() {
            Some(Token::LParen) => {
                let inner = self.parse_expr()?;
                self.expect(&Token::RParen)?;
                Ok(inner)
            }
            Some(Token::Literal(value)) => Ok(Expr::Literal(value)),
            Some(Token::Number(value)) => Ok(Expr::Number(value)),
            Some(Token::Variable(prefix, name)) => Ok(Expr::Variable { prefix, name }),
            Some(Token::Name(prefix, name)) => {
                self.expect(&Token::LParen)?;
                let mut args = Vec::new();
                if self.peek() != Some(&Token::RParen) {
                    loop {
                        args.push(self.parse_expr()?);
                        if self.peek() == Some(&Token::Comma) {
                            self.pos += 1;
                        } else {
                            break;
                        }
                    }
                }
                self.expect(&Token::RParen)?;
                Ok(Expr::FunctionCall { prefix, name, args })
            }
            other => Err(XPathError::Syntax(format!(
                "expected primary expression, found {other:?}"
            ))),
        }
    }

    fn parse_location_path(&mut self) -> Result<Expr, XPathError> {
        let mut steps = Vec::new();
        match self.peek() {
            Some(Token::Slash) => {
                self.pos += 1;
                if self.starts_step() {
                    self.parse_relative_steps(&mut steps)?;
                }
                Ok(Expr::LocationPath {
                    absolute: true,
                    steps,
                })
            }
            Some(Token::DoubleSlash) => {
                self.pos += 1;
                steps.push(descendant_or_self_step());
                self.parse_relative_steps(&mut steps)?;
                Ok(Expr::LocationPath {
                    absolute: true,
                    steps,
                })
            }
            _ => {
                self.parse_relative_steps(&mut steps)?;
                Ok(Expr::LocationPath {
                    absolute: false,
                    steps,
                })
            }
        }
    }

    fn starts_step(&self) -> bool {
        matches!(
            self.peek(),
            Some(
                Token::Dot
                    | Token::DotDot
                    | Token::At
                    | Token::Wildcard
                    | Token::PrefixWildcard(_)
                    | Token::Name(_, _)
            )
        )
    }

    fn parse_relative_steps(&mut self, steps: &mut Vec<Step>) -> Result<(), XPathError> {
        steps.push(self.parse_step()?);
        loop {
            match self.peek() {
                Some(Token::Slash) => {
                    self.pos += 1;
                    steps.push(self.parse_step()?);
                }
                Some(Token::DoubleSlash) => {
                    self.pos += 1;
                    steps.push(descendant_or_self_step());
                    steps.push(self.parse_step()?);
                }
                _ => break,
            }
        }
        Ok(())
    }

    fn parse_step(&mut self) -> Result<Step, XPathError> {
        match self.peek() {
            Some(Token::Dot) => {
                self.pos += 1;
                return Ok(Step {
                    axis: Axis::Current,
                    node_test: NodeTest::Node,
                    predicates: Vec::new(),
                });
            }
            Some(Token::DotDot) => {
                self.pos += 1;
                return Ok(Step {
                    axis: Axis::Parent,
                    node_test: NodeTest::Node,
                    predicates: Vec::new(),
                });
            }
            _ => {}
        }
        let axis = self.parse_axis()?;
        let node_test = self.parse_node_test()?;
        let predicates = self.parse_predicates()?;
        Ok(Step {
            axis,
            node_test,
            predicates,
        })
    }

    fn parse_axis(&mut self) -> Result<Axis, XPathError> {
        if self.peek() == Some(&Token::At) {
            self.pos += 1;
            return Ok(Axis::Attribute);
        }
        if let Some(Token::Name(None, name)) = self.peek().cloned()
            && self.peek_at(1) == Some(&Token::DoubleColon)
        {
            self.pos += 2;
            return Axis::from_name(&name)
                .ok_or_else(|| XPathError::Syntax(format!("unknown axis '{name}'")));
        }
        Ok(Axis::Child)
    }

    fn parse_node_test(&mut self) -> Result<NodeTest, XPathError> {
        match self.bump() {
            Some(Token::Wildcard) => Ok(NodeTest::Wildcard),
            Some(Token::PrefixWildcard(prefix)) => Ok(NodeTest::PrefixWildcard {
                prefix,
                resolved_ns: None,
            }),
            Some(Token::Name(None, name)) if self.peek() == Some(&Token::LParen) => {
                self.pos += 1;
                let test = match name.as_str() {
                    "comment" => NodeTest::Comment,
                    "text" => NodeTest::Text,
                    "node" => NodeTest::Node,
                    "processing-instruction" => {
                        let target = if let Some(Token::Literal(lit)) = self.peek().cloned() {
                            self.pos += 1;
                            Some(lit)
                        } else {
                            None
                        };
                        NodeTest::ProcessingInstruction(target)
                    }
                    _ => {
                        return Err(XPathError::Syntax(format!(
                            "unknown node type test '{name}()'"
                        )));
                    }
                };
                self.expect(&Token::RParen)?;
                Ok(test)
            }
            Some(Token::Name(prefix, local)) => Ok(NodeTest::Name {
                prefix,
                local,
                resolved_ns: None,
            }),
            other => Err(XPathError::Syntax(format!(
                "expected node test, found {other:?}"
            ))),
        }
    }

    fn parse_predicates(&mut self) -> Result<Vec<Expr>, XPathError> {
        let mut predicates = Vec::new();
        while self.peek() == Some(&Token::LBracket) {
            self.pos += 1;
            predicates.push(self.parse_expr()?);
            self.expect(&Token::RBracket)?;
        }
        Ok(predicates)
    }
}

fn descendant_or_self_step() -> Step {
    Step {
        axis: Axis::DescendantOrSelf,
        node_test: NodeTest::Node,
        predicates: Vec::new(),
    }
}

pub(crate) fn parse_expr(src: &str) -> Result<Expr, XPathError> {
    let tokens = tokenize(src)?;
    if tokens.is_empty() {
        return Err(XPathError::Syntax("empty XPath expression".into()));
    }
    let mut parser = Parser { tokens, pos: 0 };
    let expr = parser.parse_expr()?;
    if parser.pos < parser.tokens.len() {
        return Err(XPathError::Syntax(format!(
            "unexpected trailing token {:?}",
            parser.peek()
        )));
    }
    Ok(expr)
}
