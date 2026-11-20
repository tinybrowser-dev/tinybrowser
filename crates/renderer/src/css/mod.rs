pub(crate) mod color;
pub(crate) mod supports;

use style::context::QuirksMode;
use style::parser::ParserContext;
use style::stylesheets::{CssRuleType, Namespaces, Origin, UrlExtraData};
use style::substitution::AttrTaint;
use style_traits::ParsingMode;

pub(crate) fn url_data() -> UrlExtraData {
    UrlExtraData(style::servo_arc::Arc::new(net::about_blank()))
}

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
