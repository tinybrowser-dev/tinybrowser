// https://drafts.csswg.org/css-conditional-3/#the-css-namespace
// https://drafts.csswg.org/cssom/#css-property-to-idl-attribute

use style::properties::{PropertyId, SourcePropertyDeclaration, parse_one_declaration_into};
use style::stylesheets::{CssRuleType, Origin};
use style::values::Parser;

use super::{parser_context, url_data};

// https://drafts.csswg.org/cssom/#css-property-to-idl-attribute
#[must_use]
pub fn property_supported(name: &str) -> bool {
    match PropertyId::parse_enabled_for_all_content(name) {
        Ok(id) => id.non_custom_id().is_some(),
        Err(()) => false,
    }
}

// https://drafts.csswg.org/css-conditional-3/#dom-css-supports
// https://drafts.csswg.org/css-variables-1/#defining-variables
#[must_use]
pub fn supports_declaration(property: &str, value: &str) -> bool {
    let Ok(id) = PropertyId::parse_enabled_for_all_content(property) else {
        return false;
    };
    if id.non_custom_id().is_none() && value.is_empty() {
        return false;
    }
    let mut source = SourcePropertyDeclaration::default();
    parse_one_declaration_into(
        &mut source,
        id,
        value,
        Origin::Author,
        &url_data(),
        None,
        style_traits::ParsingMode::DEFAULT,
        style::context::QuirksMode::NoQuirks,
        CssRuleType::Style,
    )
    .is_ok()
}

// https://drafts.csswg.org/css-conditional-3/#dom-css-supports
#[must_use]
pub fn supports_condition(text: &str) -> bool {
    parse_condition(text) || parse_condition(&format!("({text})"))
}

fn parse_condition(text: &str) -> bool {
    let url_data = url_data();
    let mut context = parser_context(&url_data);
    let mut parser = Parser::new(text);
    let Ok(condition) = style::stylesheets::supports_rule::SupportsCondition::parse(&mut parser)
    else {
        return false;
    };
    parser.is_exhausted() && condition.eval(&mut context)
}
