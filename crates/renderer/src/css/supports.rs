//! `CSS.supports()` and the CSSOM's supported-property predicate.
//!
//! Both answer with the style crate's grammar, so the properties the CSSOM
//! exposes, the properties `CSSStyleDeclaration.setProperty()` accepts, and
//! the properties `CSS.supports()` reports are one set
//! (<https://drafts.csswg.org/css-conditional-3/#the-css-namespace>,
//! <https://drafts.csswg.org/cssom/#css-property-to-idl-attribute>).

use style::properties::{PropertyId, SourcePropertyDeclaration, parse_one_declaration_into};
use style::stylesheets::{CssRuleType, Origin};
use style::values::Parser;

use super::{parser_context, url_data};

/// Whether `name` is a non-custom property the engine supports.
///
/// A `CSSStyleDeclaration` has an IDL attribute for exactly these properties,
/// which is why `'color' in element.style` and `CSS.supports("color", "red")`
/// must agree. Custom properties have no IDL attribute, and `--x` names a
/// different property than `--X`
/// (<https://drafts.csswg.org/cssom/#css-property-to-idl-attribute>).
pub(crate) fn property_supported(name: &str) -> bool {
    match PropertyId::parse_enabled_for_all_content(name) {
        Ok(id) => id.non_custom_id().is_some(),
        Err(()) => false,
    }
}

/// `CSS.supports(property, value)`
/// (<https://drafts.csswg.org/css-conditional-3/#dom-css-supports>).
///
/// A custom property's grammar needs a token, so the empty value is not
/// supported; the style crate would otherwise accept `--x:` as an empty
/// declaration value, and a browser answers `false` for `""` while `" "` is
/// `true` (<https://drafts.csswg.org/css-variables-1/#defining-variables>).
/// The property name is looked up exactly, so surrounding whitespace makes it
/// invalid; the value keeps its own outer whitespace rules.
pub(crate) fn supports_declaration(property: &str, value: &str) -> bool {
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

/// `CSS.supports(conditionText)`
/// (<https://drafts.csswg.org/css-conditional-3/#dom-css-supports>).
///
/// The text is evaluated as a supports condition, and then as one wrapped in
/// parentheses. Wrapping is what accepts a bare declaration: `color: red` is
/// not a `<supports-condition>`, but `(color: red)` is a `<supports-decl>`.
pub(crate) fn supports_condition(text: &str) -> bool {
    parse_condition(text) || parse_condition(&format!("({text})"))
}

/// One `<supports-condition>` parse and evaluation. Text that is not a
/// supports condition evaluates to false, which is also what the grammar says
/// for a `<general-enclosed>` it does not recognise.
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
