//! CSS value parsers shared by canvas, form controls, and the CSSOM.
//!
//! Everything here parses with the style crate's own grammars, so a value the
//! cascade accepts is accepted here too.

pub(crate) mod color;
pub(crate) mod supports;

use style::context::QuirksMode;
use style::custom_properties::AttrTaint;
use style::parser::ParserContext;
use style::stylesheets::{CssRuleType, Namespaces, Origin, UrlExtraData};
use style_traits::ParsingMode;

/// The URL data a parse with no document behind it resolves URLs against.
///
/// A standalone parse has no base URL, so it resolves the way a blank realm
/// does (`about:blank`).
pub(crate) fn url_data() -> UrlExtraData {
    UrlExtraData(style::servo_arc::Arc::new(net::about_blank()))
}

/// The parse context a standalone CSS value or declaration uses: author
/// origin, style-rule grammar, no error reporter, and no namespaces.
pub(crate) fn parser_context(url_data: &UrlExtraData) -> ParserContext<'_> {
    ParserContext::new(
        Origin::Author,
        url_data,
        Some(CssRuleType::Style),
        ParsingMode::DEFAULT,
        QuirksMode::NoQuirks,
        std::borrow::Cow::Owned(Namespaces::default()),
        None,
        None,
        AttrTaint::default(),
    )
}
