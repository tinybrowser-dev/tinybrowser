// https://html.spec.whatwg.org/multipage/input.html#concept-fe-value

use blitz_dom::{BaseDocument, Dirtiness};
use style::color::{AbsoluteColor, ColorSpace};
use style::device::Device;

use crate::Parsed;
use crate::dom::microsyntaxes::{
    date_value, format_number, local_datetime_value, month_value, time_value, valid_date,
    valid_float, valid_local_datetime, valid_month, valid_time, valid_week, week_value,
};
use crate::dom::parse::parse_double;
use crate::dom::tree::{BlitzId, attr};

// https://html.spec.whatwg.org/multipage/input.html#the-step-attribute
#[expect(
    clippy::cast_precision_loss,
    reason = "f64 represents any day, month, or week count the microsyntax allows"
)]
pub(crate) fn value_in_units(kind: InputType, text: &str) -> Option<f64> {
    match kind {
        InputType::Number | InputType::Range => parse_double(text),
        InputType::Date => date_value(text).map(|days| days as f64),
        InputType::Month => month_value(text).map(|months| months as f64),
        InputType::Week => week_value(text).map(|weeks| weeks as f64),
        InputType::Time => time_value(text),
        InputType::DatetimeLocal => local_datetime_value(text),
        InputType::Hidden
        | InputType::Text
        | InputType::Search
        | InputType::Tel
        | InputType::Url
        | InputType::Email
        | InputType::Password
        | InputType::Color
        | InputType::Checkbox
        | InputType::Radio
        | InputType::File
        | InputType::Submit
        | InputType::Image
        | InputType::Reset
        | InputType::Button => None,
    }
}

// https://html.spec.whatwg.org/multipage/input.html#dom-input-valueasnumber
#[expect(
    clippy::cast_precision_loss,
    reason = "f64 represents any day, month, or week count the microsyntax allows"
)]
pub(crate) fn value_as_number(kind: InputType, text: &str) -> Option<f64> {
    match kind {
        InputType::Number | InputType::Range => parse_double(text),
        InputType::Date => date_value(text).map(|days| days as f64 * 86_400_000.0),
        InputType::Month => month_value(text).map(|months| months as f64),
        InputType::Week => week_value(text).map(|weeks| (weeks * 7 - 3) as f64 * 86_400_000.0),
        InputType::Time => time_value(text).map(|seconds| seconds * 1000.0),
        InputType::DatetimeLocal => local_datetime_value(text).map(|seconds| seconds * 1000.0),
        InputType::Hidden
        | InputType::Text
        | InputType::Search
        | InputType::Tel
        | InputType::Url
        | InputType::Email
        | InputType::Password
        | InputType::Color
        | InputType::Checkbox
        | InputType::Radio
        | InputType::File
        | InputType::Submit
        | InputType::Image
        | InputType::Reset
        | InputType::Button => None,
    }
}

// https://html.spec.whatwg.org/multipage/input.html#attr-input-type
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InputType {
    Hidden,
    Text,
    Search,
    Tel,
    Url,
    Email,
    Password,
    Date,
    Month,
    Week,
    Time,
    DatetimeLocal,
    Number,
    Range,
    Color,
    Checkbox,
    Radio,
    File,
    Submit,
    Image,
    Reset,
    Button,
}

// https://html.spec.whatwg.org/multipage/input.html#dom-input-value
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ValueMode {
    Current,
    Default,
    DefaultOn,
    Filename,
}

impl InputType {
    // https://html.spec.whatwg.org/multipage/common-microsyntaxes.html#enumerated-attribute
    pub(crate) fn from_attribute(value: Option<&str>) -> Self {
        let Some(value) = value else {
            return Self::Text;
        };
        if value.eq_ignore_ascii_case("hidden") {
            Self::Hidden
        } else if value.eq_ignore_ascii_case("search") {
            Self::Search
        } else if value.eq_ignore_ascii_case("tel") {
            Self::Tel
        } else if value.eq_ignore_ascii_case("url") {
            Self::Url
        } else if value.eq_ignore_ascii_case("email") {
            Self::Email
        } else if value.eq_ignore_ascii_case("password") {
            Self::Password
        } else if value.eq_ignore_ascii_case("date") {
            Self::Date
        } else if value.eq_ignore_ascii_case("month") {
            Self::Month
        } else if value.eq_ignore_ascii_case("week") {
            Self::Week
        } else if value.eq_ignore_ascii_case("time") {
            Self::Time
        } else if value.eq_ignore_ascii_case("datetime-local") {
            Self::DatetimeLocal
        } else if value.eq_ignore_ascii_case("number") {
            Self::Number
        } else if value.eq_ignore_ascii_case("range") {
            Self::Range
        } else if value.eq_ignore_ascii_case("color") {
            Self::Color
        } else if value.eq_ignore_ascii_case("checkbox") {
            Self::Checkbox
        } else if value.eq_ignore_ascii_case("radio") {
            Self::Radio
        } else if value.eq_ignore_ascii_case("file") {
            Self::File
        } else if value.eq_ignore_ascii_case("submit") {
            Self::Submit
        } else if value.eq_ignore_ascii_case("image") {
            Self::Image
        } else if value.eq_ignore_ascii_case("reset") {
            Self::Reset
        } else if value.eq_ignore_ascii_case("button") {
            Self::Button
        } else {
            Self::Text
        }
    }

    // https://html.spec.whatwg.org/multipage/input.html#do-not-apply
    pub(crate) fn selection_applies(self) -> bool {
        matches!(
            self,
            Self::Text | Self::Search | Self::Tel | Self::Url | Self::Password
        )
    }

    pub(crate) fn value_mode(self) -> ValueMode {
        match self {
            Self::Checkbox | Self::Radio => ValueMode::DefaultOn,
            Self::Hidden | Self::Submit | Self::Image | Self::Reset | Self::Button => {
                ValueMode::Default
            }
            Self::File => ValueMode::Filename,
            Self::Text
            | Self::Search
            | Self::Tel
            | Self::Url
            | Self::Email
            | Self::Password
            | Self::Date
            | Self::Month
            | Self::Week
            | Self::Time
            | Self::DatetimeLocal
            | Self::Number
            | Self::Range
            | Self::Color => ValueMode::Current,
        }
    }
}

pub(crate) fn input_type(base: &BaseDocument, node: BlitzId) -> Option<InputType> {
    let element = base.get_node(node)?.data.downcast_element()?;
    if element.name.ns != crate::dom::tree::html_namespace()
        || element.name.local.as_str() != "input"
    {
        return None;
    }
    Some(InputType::from_attribute(attr(base, node, "type")))
}

// https://html.spec.whatwg.org/multipage/input.html#concept-fe-value
pub(crate) fn raw_value(parsed: &Parsed, node: BlitzId, kind: InputType) -> String {
    let base = &parsed.document.base;
    let Some(element) = base
        .get_node(node)
        .and_then(|target| target.data.downcast_element())
    else {
        return String::new();
    };
    // https://html.spec.whatwg.org/multipage/input.html#color-state-(type=color
    if kind == InputType::Color && !element.form_state.dirty_value() {
        return attr(base, node, "value").unwrap_or("").to_owned();
    }
    element.form_state.value().to_owned()
}

fn value_slot(base: &BaseDocument, node: BlitzId) -> String {
    base.get_node(node)
        .and_then(|target| target.data.downcast_element())
        .map_or_else(String::new, |element| element.form_state.value().to_owned())
}

// https://html.spec.whatwg.org/multipage/input.html#dom-input-value
pub(crate) fn current_value(parsed: &Parsed, node: BlitzId) -> String {
    let base = &parsed.document.base;
    let Some(kind) = input_type(base, node) else {
        return String::new();
    };
    match kind.value_mode() {
        // https://html.spec.whatwg.org/multipage/input.html#concept-fe-value
        ValueMode::Current => base
            .get_node(node)
            .and_then(|target| target.data.downcast_element())
            .map_or_else(String::new, |element| element.form_state.value().to_owned()),
        ValueMode::Default => attr(base, node, "value").unwrap_or("").to_owned(),
        ValueMode::DefaultOn => attr(base, node, "value").unwrap_or("on").to_owned(),
        ValueMode::Filename => String::new(),
    }
}

// https://html.spec.whatwg.org/multipage/input.html#the-input-element
pub(crate) fn sanitize(
    base: &BaseDocument,
    device: &Device,
    node: BlitzId,
    kind: InputType,
    raw: &str,
) -> String {
    match kind {
        InputType::Text | InputType::Search | InputType::Tel | InputType::Password => {
            strip_newlines(raw)
        }
        InputType::Url => strip_newlines(raw).trim_ascii().to_owned(),
        InputType::Email => {
            // https://html.spec.whatwg.org/multipage/input.html#email-state-(type=email
            let value = if attr(base, node, "multiple").is_some() {
                raw.split(',')
                    .map(str::trim_ascii)
                    .collect::<Vec<_>>()
                    .join(",")
            } else {
                strip_newlines(raw).trim_ascii().to_owned()
            };
            email_domains_to_ascii(&value)
        }
        InputType::Date => {
            if valid_date(raw) {
                raw.to_owned()
            } else {
                String::new()
            }
        }
        InputType::Month => {
            if valid_month(raw) {
                raw.to_owned()
            } else {
                String::new()
            }
        }
        InputType::Week => {
            if valid_week(raw) {
                raw.to_owned()
            } else {
                String::new()
            }
        }
        InputType::Time => valid_time(raw).map_or_default(|_| raw.to_owned()),
        InputType::DatetimeLocal => valid_local_datetime(raw).map_or_default(|(date, time)| {
            format!(
                "{date}T{}",
                crate::dom::microsyntaxes::normalized_time(time)
            )
        }),
        InputType::Number => valid_float(raw).map_or_default(|_| raw.to_owned()),
        InputType::Range => sanitize_range(base, node, raw),
        InputType::Color => sanitize_color(base, device, node, raw),
        InputType::Hidden
        | InputType::Checkbox
        | InputType::Radio
        | InputType::File
        | InputType::Submit
        | InputType::Image
        | InputType::Reset
        | InputType::Button => raw.to_owned(),
    }
}

// https://html.spec.whatwg.org/multipage/input.html#e-mail-state-(type=email
fn email_domains_to_ascii(value: &str) -> String {
    value
        .split(',')
        .map(|address| {
            let Some((local, domain)) = address.rsplit_once('@') else {
                return address.to_owned();
            };
            match url::Host::parse(domain) {
                Ok(host) => format!("{local}@{host}"),
                Err(_) => address.to_owned(),
            }
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn strip_newlines(value: &str) -> String {
    value
        .chars()
        .filter(|character| *character != '\n' && *character != '\r')
        .collect()
}

// https://html.spec.whatwg.org/multipage/input.html#range-state-(type=range
fn sanitize_range(base: &BaseDocument, node: BlitzId, raw: &str) -> String {
    let minimum = attr(base, node, "min")
        .and_then(crate::dom::microsyntaxes::parse_decimal)
        .unwrap_or_else(crate::dom::microsyntaxes::Decimal::zero);
    let maximum = attr(base, node, "max")
        .and_then(crate::dom::microsyntaxes::parse_decimal)
        .unwrap_or_else(|| crate::dom::microsyntaxes::Decimal::from_integer(100));
    let default = crate::dom::microsyntaxes::range_default(&minimum, &maximum);
    let value = if valid_float(raw).is_some() {
        crate::dom::microsyntaxes::parse_decimal(raw).unwrap_or_else(|| default.clone())
    } else {
        default
    };
    if attr(base, node, "step").is_some_and(|step| step.eq_ignore_ascii_case("any")) {
        let clamped = if value
            .compare(&minimum)
            .is_some_and(std::cmp::Ordering::is_lt)
        {
            minimum
        } else if value
            .compare(&maximum)
            .is_some_and(std::cmp::Ordering::is_gt)
        {
            maximum
        } else {
            value
        };
        return format_number(clamped.to_f64());
    }
    let step = attr(base, node, "step")
        .and_then(crate::dom::microsyntaxes::parse_decimal)
        .filter(|step| !step.is_negative() && !step.is_zero())
        .unwrap_or_else(|| crate::dom::microsyntaxes::Decimal::from_integer(1));
    let step_base = attr(base, node, "min")
        .and_then(crate::dom::microsyntaxes::parse_decimal)
        .or_else(|| attr(base, node, "value").and_then(crate::dom::microsyntaxes::parse_decimal))
        .unwrap_or_else(crate::dom::microsyntaxes::Decimal::zero);
    match crate::dom::microsyntaxes::align_range(&minimum, &maximum, &step, &step_base, &value) {
        Some(aligned) => format_number(aligned.to_f64()),
        None => sanitize_range_f64(base, node, value.to_f64()),
    }
}

fn sanitize_range_f64(base: &BaseDocument, node: BlitzId, value: f64) -> String {
    let minimum = attr(base, node, "min")
        .and_then(parse_double)
        .unwrap_or(0.0);
    let maximum = attr(base, node, "max")
        .and_then(parse_double)
        .unwrap_or(100.0);
    if maximum < minimum {
        return format_number(minimum);
    }
    let step = attr(base, node, "step")
        .and_then(parse_double)
        .filter(|step| *step > 0.0)
        .unwrap_or(1.0);
    let step_base = step_base_from_attributes(base, node);
    let mut value = value.clamp(minimum, maximum);
    let quotient = (value - step_base) / step;
    let mut aligned = step_base + (quotient + 0.5).floor() * step;
    if aligned > maximum {
        let steps = ((aligned - maximum) / step).ceil();
        aligned -= steps * step;
    } else if aligned < minimum {
        let steps = ((minimum - aligned) / step).ceil();
        aligned += steps * step;
    }
    if (minimum..=maximum).contains(&aligned) {
        value = aligned;
    }
    format_number(value)
}

fn step_base_from_attributes(base: &BaseDocument, node: BlitzId) -> f64 {
    attr(base, node, "min")
        .and_then(parse_double)
        .or_else(|| attr(base, node, "value").and_then(parse_double))
        .unwrap_or(0.0)
}

// https://html.spec.whatwg.org/multipage/input.html#the-step-attribute
pub(crate) fn default_step(kind: InputType) -> Option<f64> {
    match kind {
        InputType::Number
        | InputType::Range
        | InputType::Date
        | InputType::Month
        | InputType::Week => Some(1.0),
        InputType::Time | InputType::DatetimeLocal => Some(60.0),
        _ => None,
    }
}

fn step_mismatch_f64(value: f64, step: f64, step_base: f64) -> bool {
    if step <= 0.0 {
        return false;
    }
    let remainder = ((value - step_base) % step).abs();
    if remainder == 0.0 {
        return false;
    }
    remainder.min(step - remainder) > step * 1e-9
}

// https://html.spec.whatwg.org/multipage/input.html#the-step-attribute
pub(crate) fn step_mismatch(base: &BaseDocument, node: BlitzId) -> bool {
    let Some(kind) = input_type(base, node) else {
        return false;
    };
    let Some(fallback) = default_step(kind) else {
        return false;
    };
    let raw_step = attr(base, node, "step");
    // https://html.spec.whatwg.org/multipage/input.html#the-step-attribute
    if raw_step.is_some_and(|step| step.eq_ignore_ascii_case("any")) {
        return false;
    }
    if kind == InputType::Number || kind == InputType::Range {
        // https://html.spec.whatwg.org/multipage/input.html#the-step-attribute
        let Some(value) = crate::dom::microsyntaxes::parse_decimal(&value_slot(base, node)) else {
            return false;
        };
        let step = raw_step
            .and_then(crate::dom::microsyntaxes::parse_decimal)
            .filter(|step| !step.is_negative() && !step.is_zero())
            .unwrap_or_else(|| crate::dom::microsyntaxes::Decimal::from_integer(1));
        let step_base = attr(base, node, "min")
            .and_then(crate::dom::microsyntaxes::parse_decimal)
            .or_else(|| {
                attr(base, node, "value").and_then(crate::dom::microsyntaxes::parse_decimal)
            })
            .unwrap_or_else(crate::dom::microsyntaxes::Decimal::zero);
        let scale = value.scale().max(step_base.scale()).max(step.scale());
        let (Some(value_at), Some(step_base_at), Some(step_at)) = (
            value.mantissa_at(scale),
            step_base.mantissa_at(scale),
            step.mantissa_at(scale),
        ) else {
            return step_mismatch_f64(value.to_f64(), step.to_f64(), step_base.to_f64());
        };
        return step_at != 0 && (value_at - step_base_at) % step_at != 0;
    }
    let Some(value) = value_in_units(kind, &value_slot(base, node)) else {
        return false;
    };
    let step = raw_step
        .and_then(parse_double)
        .filter(|step| *step > 0.0)
        .unwrap_or(fallback);
    let mut step_base = attr(base, node, "min").and_then(|text| value_in_units(kind, text));
    if step_base.is_none() {
        step_base = attr(base, node, "value").and_then(|text| value_in_units(kind, text));
    }
    let step_base = step_base.unwrap_or(0.0);
    let remainder = ((value - step_base) % step).abs();
    if remainder == 0.0 {
        return false;
    }
    remainder.min(step - remainder) > step * 1e-9
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NumberBufferState {
    Invalid,
    Empty,
    Partial,
    Complete,
    TrailingDot,
}

// https://html.spec.whatwg.org/multipage/input.html#number-state-(type=number
pub(crate) fn number_buffer_state(text: &str) -> NumberBufferState {
    if text.is_empty() {
        return NumberBufferState::Empty;
    }
    let bytes = text.as_bytes();
    let mut position = 0;
    if matches!(bytes[0], b'+' | b'-') {
        position += 1;
    }
    let integer_start = position;
    while bytes.get(position).is_some_and(u8::is_ascii_digit) {
        position += 1;
    }
    let integer_digits = position - integer_start;
    let mut fraction_digits = 0;
    let mut saw_dot = false;
    if bytes.get(position) == Some(&b'.') {
        saw_dot = true;
        position += 1;
        let fraction_start = position;
        while bytes.get(position).is_some_and(u8::is_ascii_digit) {
            position += 1;
        }
        fraction_digits = position - fraction_start;
    }
    // https://html.spec.whatwg.org/multipage/common-microsyntaxes.html#valid-floating-point-number
    let mantissa_complete = if integer_digits > 0 {
        !saw_dot || fraction_digits > 0
    } else {
        saw_dot && fraction_digits > 0
    };
    let trailing_dot = integer_digits > 0 && saw_dot && fraction_digits == 0;
    if position == bytes.len() {
        return if trailing_dot {
            NumberBufferState::TrailingDot
        } else if mantissa_complete {
            NumberBufferState::Complete
        } else if saw_dot {
            NumberBufferState::Invalid
        } else {
            NumberBufferState::Partial
        };
    }
    if !matches!(bytes[position], b'e' | b'E') {
        return NumberBufferState::Invalid;
    }
    position += 1;
    if matches!(bytes.get(position), Some(b'+' | b'-')) {
        position += 1;
    }
    let exponent_start = position;
    while bytes.get(position).is_some_and(u8::is_ascii_digit) {
        position += 1;
    }
    if position != bytes.len() {
        return NumberBufferState::Invalid;
    }
    if position > exponent_start && mantissa_complete {
        NumberBufferState::Complete
    } else {
        NumberBufferState::Partial
    }
}

pub(crate) fn sanitize_with_device(
    base: &mut BaseDocument,
    node: BlitzId,
    kind: InputType,
    raw: &str,
) -> String {
    let _device = base.stylist_device();
    let device = base.device();
    sanitize(base, device, node, kind, raw)
}

// https://html.spec.whatwg.org/multipage/input.html#update-a-color-well-control-color
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ColorWellOptions {
    display_p3: bool,
    preserve_alpha: bool,
}

impl ColorWellOptions {
    fn from_attributes(base: &BaseDocument, node: BlitzId) -> Self {
        Self {
            display_p3: attr(base, node, "colorspace")
                .is_some_and(|space| space.eq_ignore_ascii_case("display-p3")),
            preserve_alpha: attr(base, node, "alpha").is_some(),
        }
    }

    fn default_value(self) -> String {
        if self.display_p3 {
            "color(display-p3 0 0 0)".to_owned()
        } else if self.preserve_alpha {
            "color(srgb 0 0 0)".to_owned()
        } else {
            "#000000".to_owned()
        }
    }
}

// https://html.spec.whatwg.org/multipage/input.html#update-a-color-well-control-color
fn sanitize_color(base: &BaseDocument, device: &Device, node: BlitzId, raw: &str) -> String {
    let options = ColorWellOptions::from_attributes(base, node);
    let Some(parsed) = crate::css::color::parse_exact(raw, Some(device), &AbsoluteColor::BLACK)
    else {
        return options.default_value();
    };
    if options.display_p3 {
        let p3 = parsed.absolute.to_color_space(ColorSpace::DisplayP3);
        let alpha = if options.preserve_alpha {
            p3.alpha
        } else {
            1.0
        };
        return serialize_color_function(
            "display-p3",
            [p3.components.0, p3.components.1, p3.components.2],
            alpha,
        );
    }
    if options.preserve_alpha {
        serialize_color_function(
            "srgb",
            [parsed.value[0], parsed.value[1], parsed.value[2]],
            parsed.value[3],
        )
    } else {
        let [red, green, blue, _] = parsed.bytes;
        format!("#{red:02x}{green:02x}{blue:02x}")
    }
}

fn serialize_color_function(space: &str, [red, green, blue]: [f32; 3], alpha: f32) -> String {
    let component = |value: f32| {
        let value = (f64::from(value) * 1_000_000.0).round() / 1_000_000.0;
        format_number(value)
    };
    let red = component(red);
    let green = component(green);
    let blue = component(blue);
    if alpha >= 1.0 {
        format!("color({space} {red} {green} {blue})")
    } else {
        format!(
            "color({space} {red} {green} {blue} / {})",
            component(alpha.clamp(0.0, 1.0))
        )
    }
}

// https://html.spec.whatwg.org/multipage/input.html#concept-fe-value
pub(crate) fn sync_sanitized_value(document: &mut crate::documents::BlitzDocument, node: BlitzId) {
    let base = &document.base;
    let Some(kind) = input_type(base, node) else {
        return;
    };
    if kind.value_mode() != ValueMode::Current {
        return;
    }
    let Some(element) = base
        .get_node(node)
        .and_then(|target| target.data.downcast_element())
    else {
        return;
    };
    let dirty = element.form_state.dirty_value();
    if dirty && !matches!(kind, InputType::Range | InputType::Color | InputType::Email) {
        return;
    }
    let raw = if dirty {
        element.form_state.value().to_owned()
    } else {
        attr(base, node, "value").unwrap_or("").to_owned()
    };
    let sanitized = sanitize_with_device(&mut document.base, node, kind, &raw);
    document
        .base
        .mutate()
        .set_value(node, sanitized, Dirtiness::Keep);
}

#[cfg(test)]
mod tests {
    use super::{
        InputType, NumberBufferState, number_buffer_state, value_as_number, value_in_units,
    };

    #[test]
    fn a_number_buffer_is_classified_by_the_engine_grammar() {
        use NumberBufferState::{Complete, Empty, Invalid, Partial, TrailingDot};
        let state = number_buffer_state;
        assert_eq!(state(""), Empty);
        assert_eq!(state("+"), Partial);
        assert_eq!(state("-"), Partial);
        assert_eq!(state("."), Invalid);
        assert_eq!(state("+."), Invalid);
        assert_eq!(state("1."), TrailingDot);
        assert_eq!(state("-1."), TrailingDot);
        assert_eq!(state("1.e2"), Partial);
        assert_eq!(state("1.e+2"), Partial);
        assert_eq!(state(".5"), Complete);
        assert_eq!(state(".5e2"), Complete);
        assert_eq!(state("1.0"), Complete);
        assert_eq!(state("1e"), Partial);
        assert_eq!(state("1e+"), Partial);
        assert_eq!(state("1e2"), Complete);
        assert_eq!(state("e"), Partial);
        assert_eq!(state("e2"), Partial);
        assert_eq!(state("+e"), Partial);
        assert_eq!(state("--1"), Invalid);
        assert_eq!(state("1.2.3"), Invalid);
        assert_eq!(state("1px"), Invalid);
        assert_eq!(state(" 1"), Invalid);
        assert_eq!(state("1 "), Invalid);
    }

    #[test]
    fn a_default_step_covers_exactly_the_stepping_states() {
        use super::default_step;
        assert_eq!(default_step(InputType::Number), Some(1.0));
        assert_eq!(default_step(InputType::Range), Some(1.0));
        assert_eq!(default_step(InputType::Date), Some(1.0));
        assert_eq!(default_step(InputType::Month), Some(1.0));
        assert_eq!(default_step(InputType::Week), Some(1.0));
        assert_eq!(default_step(InputType::Time), Some(60.0));
        assert_eq!(default_step(InputType::DatetimeLocal), Some(60.0));
        assert_eq!(default_step(InputType::Text), None);
        assert_eq!(default_step(InputType::Color), None);
    }

    #[test]
    fn a_step_mismatch_tolerates_only_rounding_noise() {
        use super::step_mismatch_f64;
        assert!(!step_mismatch_f64(0.3, 0.1, 0.0));
        assert!(!step_mismatch_f64(0.0, 0.1, 0.0));
        assert!(step_mismatch_f64(0.35, 0.1, 0.0));
        assert!(!step_mismatch_f64(0.0, 0.0, 0.0));
    }

    #[test]
    fn a_value_in_units_uses_the_state_unit() {
        assert_eq!(value_in_units(InputType::Date, "1970-01-02"), Some(1.0));
        assert_eq!(value_in_units(InputType::Month, "1970-02"), Some(1.0));
        assert_eq!(value_in_units(InputType::Week, "1970-W02"), Some(1.0));
        assert_eq!(value_in_units(InputType::Time, "01:00"), Some(3_600.0));
        assert_eq!(
            value_in_units(InputType::DatetimeLocal, "1970-01-02T00:00"),
            Some(86_400.0)
        );
        assert_eq!(value_in_units(InputType::Number, "1.5"), Some(1.5));
        assert_eq!(value_in_units(InputType::Text, "1"), None);
    }

    #[test]
    fn a_value_as_number_uses_the_states_unit() {
        assert_eq!(
            value_as_number(InputType::Date, "1970-01-02"),
            Some(86_400_000.0)
        );
        assert_eq!(value_as_number(InputType::Month, "1970-02"), Some(1.0));
        assert_eq!(
            value_as_number(InputType::Week, "1970-W02"),
            Some(345_600_000.0)
        );
        assert_eq!(value_as_number(InputType::Time, "01:00"), Some(3_600_000.0));
        assert_eq!(value_as_number(InputType::Number, "1.5"), Some(1.5));
        assert_eq!(value_as_number(InputType::Date, "2019-02-29"), None);
    }
}
