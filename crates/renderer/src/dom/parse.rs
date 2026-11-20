// https://infra.spec.whatwg.org/#ascii-whitespace
pub(crate) fn is_html_whitespace(char: Option<&char>) -> bool {
    matches!(char, Some(' ' | '\t' | '\n' | '\u{c}' | '\r'))
}

// https://infra.spec.whatwg.org/#rules-for-parsing-integers
pub(crate) fn parse_integer(value: &str) -> Option<i32> {
    let mut chars = value.chars().peekable();
    while is_html_whitespace(chars.peek()) {
        chars.next();
    }
    let mut sign: i64 = 1;
    match chars.peek() {
        Some('-') => {
            sign = -1;
            chars.next();
        }
        Some('+') => {
            chars.next();
        }
        _ => {}
    }
    if !chars.peek().is_some_and(char::is_ascii_digit) {
        return None;
    }
    let mut result: i64 = 0;
    while let Some(digit) = chars.peek().and_then(|c| c.to_digit(10)) {
        result = result.checked_mul(10)?.checked_add(i64::from(digit))?;
        chars.next();
    }
    let result = result.checked_mul(sign)?;
    i32::try_from(result).ok()
}

// https://infra.spec.whatwg.org/#rules-for-parsing-non-negative-integers
pub(crate) fn parse_non_negative_integer(value: &str) -> Option<u32> {
    match parse_integer(value)? {
        0 => Some(0),
        parsed @ 1..=2_147_483_647 => u32::try_from(parsed).ok(),
        _ => None,
    }
}

// https://infra.spec.whatwg.org/#rules-for-parsing-floating-point-number-values
pub(crate) fn parse_double(value: &str) -> Option<f64> {
    let bytes = value.as_bytes();
    let mut pos = 0;
    while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t' | b'\n' | b'\x0c' | b'\r') {
        pos += 1;
    }
    let start = pos;
    if pos < bytes.len() && (bytes[pos] == b'-' || bytes[pos] == b'+') {
        pos += 1;
    }
    let mut integer_digits = 0_u32;
    while pos < bytes.len() && bytes[pos].is_ascii_digit() {
        integer_digits += 1;
        pos += 1;
    }
    let mut fraction_digits = 0_u32;
    if pos < bytes.len() && bytes[pos] == b'.' {
        pos += 1;
        while pos < bytes.len() && bytes[pos].is_ascii_digit() {
            fraction_digits += 1;
            pos += 1;
        }
    }
    if integer_digits == 0 && fraction_digits == 0 {
        return None;
    }
    if pos < bytes.len() && (bytes[pos] == b'e' || bytes[pos] == b'E') {
        let mut probe = pos + 1;
        if probe < bytes.len() && (bytes[probe] == b'-' || bytes[probe] == b'+') {
            probe += 1;
        }
        let exponent_start = probe;
        while probe < bytes.len() && bytes[probe].is_ascii_digit() {
            probe += 1;
        }
        if probe > exponent_start {
            pos = probe;
        }
    }
    let parsed: f64 = value[start..pos].parse().ok()?;
    if parsed.is_finite() {
        Some(parsed + 0.0)
    } else {
        None
    }
}
