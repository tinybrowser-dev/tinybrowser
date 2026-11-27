// https://www.w3.org/TR/1999/REC-xpath-19991116/#exprlex

use super::XPathError;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Token {
    LParen,
    RParen,
    LBracket,
    RBracket,
    Dot,
    DotDot,
    At,
    Comma,
    DoubleColon,
    Slash,
    DoubleSlash,
    Pipe,
    Plus,
    Minus,
    Eq,
    NotEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
    Multiply,
    Wildcard,
    And,
    Or,
    Mod,
    Div,
    Literal(String),
    Number(f64),
    Variable(Option<String>, String),
    PrefixWildcard(String),
    Name(Option<String>, String),
}

fn expects_operand(prev: Option<&Token>) -> bool {
    matches!(
        prev,
        None | Some(
            Token::At
                | Token::DoubleColon
                | Token::LParen
                | Token::LBracket
                | Token::Comma
                | Token::Slash
                | Token::DoubleSlash
                | Token::Pipe
                | Token::Plus
                | Token::Minus
                | Token::Eq
                | Token::NotEq
                | Token::Lt
                | Token::LtEq
                | Token::Gt
                | Token::GtEq
                | Token::Multiply
                | Token::And
                | Token::Or
                | Token::Mod
                | Token::Div
        )
    )
}

fn is_xpath_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\r' | b'\n')
}

// https://www.w3.org/TR/xml/#NT-NameStartChar
fn is_ncname_start(ch: char) -> bool {
    matches!(
        ch,
        'A'..='Z'
            | '_'
            | 'a'..='z'
            | '\u{C0}'..='\u{D6}'
            | '\u{D8}'..='\u{F6}'
            | '\u{F8}'..='\u{2FF}'
            | '\u{370}'..='\u{37D}'
            | '\u{37F}'..='\u{1FFF}'
            | '\u{200C}'..='\u{200D}'
            | '\u{2070}'..='\u{218F}'
            | '\u{2C00}'..='\u{2FEF}'
            | '\u{3001}'..='\u{D7FF}'
            | '\u{F900}'..='\u{FDCF}'
            | '\u{FDF0}'..='\u{FFFD}'
            | '\u{10000}'..='\u{EFFFF}'
    )
}

// https://www.w3.org/TR/xml/#NT-NameChar
fn is_ncname_continue(ch: char) -> bool {
    is_ncname_start(ch)
        || matches!(
            ch,
            '-' | '.' | '0'..='9' | '\u{B7}' | '\u{0300}'..='\u{036F}' | '\u{203F}'..='\u{2040}'
        )
}

struct Scanner<'a> {
    src: &'a str,
    pos: usize,
}

impl<'a> Scanner<'a> {
    fn rest(&self) -> &'a str {
        self.src.get(self.pos..).unwrap_or("")
    }

    fn peek_byte(&self) -> Option<u8> {
        self.src.as_bytes().get(self.pos).copied()
    }

    fn peek_second_byte(&self) -> Option<u8> {
        self.src.as_bytes().get(self.pos + 1).copied()
    }

    fn skip_spaces(&mut self) {
        while self.peek_byte().is_some_and(is_xpath_space) {
            self.pos += 1;
        }
    }

    fn scan_literal(&mut self, quote: u8) -> Result<String, XPathError> {
        self.pos += 1;
        let start = self.pos;
        while let Some(byte) = self.peek_byte() {
            if byte == quote {
                let text = self.src.get(start..self.pos).unwrap_or("").to_owned();
                self.pos += 1;
                return Ok(text);
            }
            self.pos += 1;
        }
        Err(XPathError::Syntax("unterminated string literal".into()))
    }

    fn scan_number(&mut self) -> Result<f64, XPathError> {
        let start = self.pos;
        while self.peek_byte().is_some_and(|b| b.is_ascii_digit()) {
            self.pos += 1;
        }
        if self.peek_byte() == Some(b'.') {
            self.pos += 1;
            while self.peek_byte().is_some_and(|b| b.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        let slice = self.src.get(start..self.pos).unwrap_or("");
        slice
            .parse::<f64>()
            .map_err(|_| XPathError::Syntax(format!("invalid number literal: {slice}")))
    }

    fn scan_ncname(&mut self) -> Result<String, XPathError> {
        let mut chars = self.rest().chars();
        let Some(first) = chars.next() else {
            return Err(XPathError::Syntax("expected identifier".into()));
        };
        if !is_ncname_start(first) {
            return Err(XPathError::Syntax(format!(
                "unexpected character '{first}'"
            )));
        }
        let start = self.pos;
        self.pos += first.len_utf8();
        for ch in chars {
            if is_ncname_continue(ch) {
                self.pos += ch.len_utf8();
            } else {
                break;
            }
        }
        Ok(self.src.get(start..self.pos).unwrap_or("").to_owned())
    }

    fn scan_qname_or_wildcard(&mut self, operator_pos: bool) -> Result<Token, XPathError> {
        let name = self.scan_ncname()?;
        if operator_pos {
            let op = match name.as_str() {
                "and" => Some(Token::And),
                "or" => Some(Token::Or),
                "mod" => Some(Token::Mod),
                "div" => Some(Token::Div),
                _ => None,
            };
            if let Some(token) = op {
                return Ok(token);
            }
            return Err(XPathError::Syntax(format!(
                "expected operator, found '{name}'"
            )));
        }
        if self.peek_byte() == Some(b':') && self.peek_second_byte() != Some(b':') {
            self.pos += 1;
            if self.peek_byte() == Some(b'*') {
                self.pos += 1;
                return Ok(Token::PrefixWildcard(name));
            }
            let local = self.scan_ncname()?;
            return Ok(Token::Name(Some(name), local));
        }
        Ok(Token::Name(None, name))
    }

    fn scan_punct(&mut self, operator_pos: bool) -> Option<Token> {
        let byte = self.peek_byte()?;
        let next = self.peek_second_byte();
        let (tok, len) = match (byte, next) {
            (b'(', _) => (Token::LParen, 1),
            (b')', _) => (Token::RParen, 1),
            (b'[', _) => (Token::LBracket, 1),
            (b']', _) => (Token::RBracket, 1),
            (b'@', _) => (Token::At, 1),
            (b',', _) => (Token::Comma, 1),
            (b'|', _) => (Token::Pipe, 1),
            (b'+', _) => (Token::Plus, 1),
            (b'-', _) => (Token::Minus, 1),
            (b'=', _) => (Token::Eq, 1),
            (b'!', Some(b'=')) => (Token::NotEq, 2),
            (b'<', Some(b'=')) => (Token::LtEq, 2),
            (b'<', _) => (Token::Lt, 1),
            (b'>', Some(b'=')) => (Token::GtEq, 2),
            (b'>', _) => (Token::Gt, 1),
            (b'/', Some(b'/')) => (Token::DoubleSlash, 2),
            (b'/', _) => (Token::Slash, 1),
            (b':', Some(b':')) => (Token::DoubleColon, 2),
            (b'.', Some(b'.')) => (Token::DotDot, 2),
            (b'.', Some(b)) if b.is_ascii_digit() => return None,
            (b'.', _) => (Token::Dot, 1),
            (b'*', _) => (
                if operator_pos {
                    Token::Multiply
                } else {
                    Token::Wildcard
                },
                1,
            ),
            _ => return None,
        };
        self.pos += len;
        Some(tok)
    }
}

pub(crate) fn tokenize(src: &str) -> Result<Vec<Token>, XPathError> {
    let mut scanner = Scanner { src, pos: 0 };
    let mut tokens = Vec::new();
    loop {
        scanner.skip_spaces();
        let Some(byte) = scanner.peek_byte() else {
            break;
        };
        let operator_pos = !expects_operand(tokens.last());
        if let Some(tok) = scanner.scan_punct(operator_pos) {
            tokens.push(tok);
            continue;
        }
        let tok = match byte {
            b'\'' | b'"' => Token::Literal(scanner.scan_literal(byte)?),
            b'0'..=b'9' | b'.' => Token::Number(scanner.scan_number()?),
            b'$' => {
                scanner.pos += 1;
                let first = scanner.scan_ncname()?;
                if scanner.peek_byte() == Some(b':') && scanner.peek_second_byte() != Some(b':') {
                    scanner.pos += 1;
                    let local = scanner.scan_ncname()?;
                    Token::Variable(Some(first), local)
                } else {
                    Token::Variable(None, first)
                }
            }
            _ => scanner.scan_qname_or_wildcard(operator_pos)?,
        };
        tokens.push(tok);
    }
    Ok(tokens)
}
