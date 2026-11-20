// https://drafts.csswg.org/css-color-4/#parse-a-css-color-value
// https://html.spec.whatwg.org/multipage/canvas.html#dom-context-2d-fillstyle

use style::color::{AbsoluteColor, ColorSpace};
use style::device::Device;
use style::values::Parser;
use style::values::specified::Color as SpecifiedColor;
use style_traits::ToCss;

pub(crate) struct ParsedColor {
    pub(crate) absolute: AbsoluteColor,
    pub(crate) bytes: [u8; 4],
    pub(crate) value: [f32; 4],
    pub(crate) serialized: Option<String>,
}

// https://drafts.csswg.org/css-color-4/#currentcolor-color
fn parse_absolute(
    input: &str,
    device: Option<&Device>,
    current: &AbsoluteColor,
) -> Option<AbsoluteColor> {
    let url_data = crate::css::url_data();
    let context = crate::css::parser_context(&url_data);
    let mut parser = Parser::new(input);
    let computed = SpecifiedColor::parse_and_compute(&context, &mut parser, device).ok()?;
    Some(computed.resolve_to_absolute(current))
}

pub(crate) fn parse_exact(
    input: &str,
    device: Option<&Device>,
    current: &AbsoluteColor,
) -> Option<ParsedColor> {
    let absolute = parse_absolute(input, device, current)?;
    let srgb = absolute.to_color_space(ColorSpace::Srgb);
    let value = [
        srgb.components.0,
        srgb.components.1,
        srgb.components.2,
        srgb.alpha,
    ];
    // https://drafts.csswg.org/css-color-4/#serializing-color-values
    let serialized = if absolute.is_legacy_syntax() {
        None
    } else {
        Some(absolute.to_css_string())
    };
    Some(ParsedColor {
        absolute,
        bytes: to_bytes(value),
        value,
        serialized,
    })
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "channels are clamped to 0..1 before scaling to 0..255"
)]
pub(crate) fn to_bytes([r, g, b, a]: [f32; 4]) -> [u8; 4] {
    let channel = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    [channel(r), channel(g), channel(b), channel(a)]
}

#[cfg(test)]
mod tests {
    use super::parse_exact;
    use style::color::AbsoluteColor;

    const BLACK: AbsoluteColor = AbsoluteColor::BLACK;

    #[test]
    fn color_mix_is_premultiplied() {
        let transparent_red =
            parse_exact("color-mix(in srgb, transparent, red)", None, &BLACK).unwrap();
        assert_eq!(transparent_red.bytes, [255, 0, 0, 128]);
    }

    #[test]
    fn color_mix_rejects_out_of_range_weights() {
        // https://drafts.csswg.org/css-values-5/#funcdef-color-mix
        assert!(parse_exact("color-mix(in srgb, red 150%, blue)", None, &BLACK).is_none());
        assert!(parse_exact("color-mix(in srgb, red -10%, blue)", None, &BLACK).is_none());
    }

    #[test]
    fn system_color_without_device_fails() {
        assert!(parse_exact("Canvas", None, &BLACK).is_none());
    }

    #[test]
    fn currentcolor_resolves_to_the_supplied_color() {
        let red = parse_exact("red", None, &BLACK).unwrap();
        let parsed = parse_exact("currentcolor", None, &red.absolute).unwrap();
        assert_eq!(parsed.bytes, [255, 0, 0, 255]);
    }
}
