// https://html.spec.whatwg.org/multipage/common-microsyntaxes.html
// https://tc39.es/ecma262/#sec-numeric-types-number-tostring

// https://html.spec.whatwg.org/multipage/common-microsyntaxes.html#valid-floating-point-number
pub fn valid_float(value: &str) -> Option<f64> {
    let bytes = value.as_bytes();
    let mut position = 0;
    if bytes.first() == Some(&b'-') {
        position += 1;
    }
    let integer_start = position;
    while bytes.get(position).is_some_and(u8::is_ascii_digit) {
        position += 1;
    }
    let integer_digits = position - integer_start;
    let mut fraction_digits = 0;
    if bytes.get(position) == Some(&b'.') {
        position += 1;
        let fraction_start = position;
        while bytes.get(position).is_some_and(u8::is_ascii_digit) {
            position += 1;
        }
        fraction_digits = position - fraction_start;
        if fraction_digits == 0 {
            return None;
        }
    }
    if integer_digits == 0 && fraction_digits == 0 {
        return None;
    }
    if bytes
        .get(position)
        .is_some_and(|byte| *byte == b'e' || *byte == b'E')
    {
        position += 1;
        if bytes
            .get(position)
            .is_some_and(|byte| *byte == b'+' || *byte == b'-')
        {
            position += 1;
        }
        let exponent_start = position;
        while bytes.get(position).is_some_and(u8::is_ascii_digit) {
            position += 1;
        }
        if position == exponent_start {
            return None;
        }
    }
    if position != bytes.len() {
        return None;
    }
    value
        .parse::<f64>()
        .ok()
        .filter(|number| number.is_finite())
}

// https://html.spec.whatwg.org/multipage/common-microsyntaxes.html#best-representation-of-the-number-as-a-floating-point-number
#[must_use]
pub fn format_number(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_owned();
    }
    if value == 0.0 {
        return "0".to_owned();
    }
    if value.is_infinite() {
        return if value > 0.0 {
            "Infinity".to_owned()
        } else {
            "-Infinity".to_owned()
        };
    }
    let negative = value.is_sign_negative();
    let magnitude = value.abs();
    let scientific = format!("{magnitude:e}");
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let exponent = exponent.parse::<i32>().unwrap_or(0);
    let digits: String = mantissa
        .chars()
        .filter(|character| *character != '.')
        .collect();
    let k = i32::try_from(digits.len()).unwrap_or(i32::MAX);
    let n = exponent + 1;
    let body = if k <= n && n <= 21 {
        format!(
            "{digits}{}",
            "0".repeat(usize::try_from(n - k).unwrap_or(0))
        )
    } else if 0 < n && n <= 21 {
        let split = usize::try_from(n).unwrap_or(digits.len());
        format!("{}.{}", &digits[..split], &digits[split..])
    } else if -6 < n && n <= 0 {
        format!(
            "0.{}{}",
            "0".repeat(usize::try_from(-n).unwrap_or(0)),
            digits
        )
    } else {
        let exponent = n - 1;
        let sign = if exponent >= 0 { '+' } else { '-' };
        let mantissa = if k == 1 {
            digits
        } else {
            format!("{}.{}", &digits[..1], &digits[1..])
        };
        format!("{mantissa}e{sign}{}", exponent.abs())
    };
    if negative { format!("-{body}") } else { body }
}

fn is_leap_year(year: u64) -> bool {
    year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
}

fn days_in_month(year: u64, month: u64) -> u64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

fn parse_year(value: &str) -> Option<u64> {
    if value.len() < 4 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse::<u64>().ok().filter(|year| *year > 0)
}

fn parse_two_digits(value: &str) -> Option<u64> {
    if value.len() != 2 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse::<u64>().ok()
}

// https://html.spec.whatwg.org/multipage/common-microsyntaxes.html#valid-date-string
pub fn valid_date(value: &str) -> bool {
    let mut parts = value.split('-');
    let Some(year) = parts.next().and_then(parse_year) else {
        return false;
    };
    let Some(month) = parts.next().and_then(parse_two_digits) else {
        return false;
    };
    let Some(day) = parts.next().and_then(parse_two_digits) else {
        return false;
    };
    if parts.next().is_some() || !(1..=12).contains(&month) {
        return false;
    }
    (1..=days_in_month(year, month)).contains(&day)
}

// https://html.spec.whatwg.org/multipage/common-microsyntaxes.html#valid-month-string
pub fn valid_month(value: &str) -> bool {
    let mut parts = value.split('-');
    let Some(_year) = parts.next().and_then(parse_year) else {
        return false;
    };
    let Some(month) = parts.next().and_then(parse_two_digits) else {
        return false;
    };
    parts.next().is_none() && (1..=12).contains(&month)
}

fn january_first_weekday(year: u64) -> u64 {
    let previous = u128::from(year - 1);
    let days = 365 * previous + previous / 4 - previous / 100 + previous / 400;
    u64::try_from(days % 7).unwrap_or(0)
}

// https://html.spec.whatwg.org/multipage/common-microsyntaxes.html#valid-week-string
pub fn valid_week(value: &str) -> bool {
    let Some((year, rest)) = value.split_once('-') else {
        return false;
    };
    let Some(week) = rest.strip_prefix('W').and_then(parse_two_digits) else {
        return false;
    };
    let Some(year) = parse_year(year) else {
        return false;
    };
    let weeks = if january_first_weekday(year) == 3
        || (is_leap_year(year) && january_first_weekday(year) == 2)
    {
        53
    } else {
        52
    };
    (1..=weeks).contains(&week)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Time {
    pub hour: u64,
    pub minute: u64,
    pub second: u64,
    pub millis: u64,
}

fn days_from_civil(year: u64, month: u64, day: u64) -> i128 {
    let year = i128::from(year);
    let month = i128::from(month);
    let day = i128::from(day);
    let year = year - i128::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_index = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

// https://html.spec.whatwg.org/multipage/common-microsyntaxes.html#valid-date-string
pub fn date_value(value: &str) -> Option<i128> {
    let mut parts = value.split('-');
    let year = parts.next().and_then(parse_year)?;
    let month = parts.next().and_then(parse_two_digits)?;
    let day = parts.next().and_then(parse_two_digits)?;
    if parts.next().is_some() || !(1..=12).contains(&month) {
        return None;
    }
    if !(1..=days_in_month(year, month)).contains(&day) {
        return None;
    }
    Some(days_from_civil(year, month, day))
}

// https://html.spec.whatwg.org/multipage/common-microsyntaxes.html#valid-month-string
pub fn month_value(value: &str) -> Option<i128> {
    let mut parts = value.split('-');
    let year = parts.next().and_then(parse_year)?;
    let month = parts.next().and_then(parse_two_digits)?;
    if parts.next().is_some() || !(1..=12).contains(&month) {
        return None;
    }
    Some((i128::from(year) - 1970) * 12 + i128::from(month) - 1)
}

const EPOCH_MONDAY: i128 = -3;

fn weekday_from_days(days: i128) -> i128 {
    ((days % 7) + 10) % 7
}

// https://html.spec.whatwg.org/multipage/common-microsyntaxes.html#valid-week-string
pub fn week_value(value: &str) -> Option<i128> {
    let (year, rest) = value.split_once('-')?;
    let week = rest.strip_prefix('W').and_then(parse_two_digits)?;
    let year = parse_year(year)?;
    let weeks = if january_first_weekday(year) == 3
        || (is_leap_year(year) && january_first_weekday(year) == 2)
    {
        53
    } else {
        52
    };
    if !(1..=weeks).contains(&week) {
        return None;
    }
    let january_fourth = days_from_civil(year, 1, 4);
    let monday_of_week_one = january_fourth - weekday_from_days(january_fourth);
    Some((monday_of_week_one - EPOCH_MONDAY) / 7 + i128::from(week) - 1)
}

// https://html.spec.whatwg.org/multipage/common-microsyntaxes.html#valid-time-string
#[expect(
    clippy::cast_precision_loss,
    reason = "all four fields are at most 59 or 999, far below 2^53"
)]
#[must_use]
pub fn time_value(value: &str) -> Option<f64> {
    let time = valid_time(value)?;
    Some(
        time.hour as f64 * 3600.0
            + time.minute as f64 * 60.0
            + time.second as f64
            + time.millis as f64 / 1000.0,
    )
}

// https://html.spec.whatwg.org/multipage/common-microsyntaxes.html#valid-local-date-and-time-string
#[expect(
    clippy::cast_precision_loss,
    reason = "f64 represents any day count the microsyntax allows"
)]
#[must_use]
pub fn local_datetime_value(value: &str) -> Option<f64> {
    let separator = value.find(['T', ' '])?;
    let (date, rest) = value.split_at(separator);
    let days = date_value(date)?;
    let seconds = time_value(&rest[1..])?;
    Some(days as f64 * 86_400.0 + seconds)
}

// https://html.spec.whatwg.org/multipage/common-microsyntaxes.html#valid-time-string
#[must_use]
pub fn valid_time(value: &str) -> Option<Time> {
    let (clock, fraction) = match value.split_once('.') {
        Some((clock, fraction)) => (clock, Some(fraction)),
        None => (value, None),
    };
    let mut parts = clock.split(':');
    let hour = parse_two_digits(parts.next()?)?;
    let minute = parse_two_digits(parts.next()?)?;
    let second = match parts.next() {
        Some(second) => Some(parse_two_digits(second)?),
        None => None,
    };
    if parts.next().is_some()
        || hour > 23
        || minute > 59
        || second.is_some_and(|second| second > 59)
    {
        return None;
    }
    if fraction.is_some() && second.is_none() {
        return None;
    }
    let second = second.unwrap_or(0);
    let millis = match fraction {
        Some(fraction) => {
            if fraction.is_empty()
                || fraction.len() > 3
                || !fraction.bytes().all(|byte| byte.is_ascii_digit())
            {
                return None;
            }
            let mut padded = fraction.to_owned();
            while padded.len() < 3 {
                padded.push('0');
            }
            padded.parse::<u64>().ok()?
        }
        None => 0,
    };
    Some(Time {
        hour,
        minute,
        second,
        millis,
    })
}

// https://html.spec.whatwg.org/multipage/common-microsyntaxes.html#valid-local-date-and-time-string
#[must_use]
pub fn valid_local_datetime(value: &str) -> Option<(&str, Time)> {
    let separator = value.find(['T', ' '])?;
    let (date, rest) = value.split_at(separator);
    let time = valid_time(&rest[1..])?;
    valid_date(date).then_some((date, time))
}

// https://html.spec.whatwg.org/multipage/common-microsyntaxes.html#valid-normalized-local-date-and-time-string
#[must_use]
pub fn normalized_time(time: Time) -> String {
    let seconds = if time.second != 0 || time.millis != 0 {
        format!(":{:02}", time.second)
    } else {
        String::new()
    };
    let fraction = if time.millis != 0 {
        let fraction = format!("{:03}", time.millis);
        format!(".{}", fraction.trim_end_matches('0'))
    } else {
        String::new()
    };
    format!("{:02}:{:02}{seconds}{fraction}", time.hour, time.minute)
}

pub fn authored_decimal_scale(text: &str) -> Option<u32> {
    let bytes = text.as_bytes();
    let mut position = 0;
    if bytes
        .get(position)
        .is_some_and(|byte| *byte == b'+' || *byte == b'-')
    {
        position += 1;
    }
    while bytes.get(position).is_some_and(u8::is_ascii_digit) {
        position += 1;
    }
    let mut fraction_digits = 0_i64;
    if bytes.get(position) == Some(&b'.') {
        position += 1;
        while bytes.get(position).is_some_and(u8::is_ascii_digit) {
            fraction_digits += 1;
            position += 1;
        }
    }
    let mut exponent = 0_i64;
    if bytes
        .get(position)
        .is_some_and(|byte| *byte == b'e' || *byte == b'E')
    {
        position += 1;
        let negative = match bytes.get(position) {
            Some(b'-') => {
                position += 1;
                true
            }
            Some(b'+') => {
                position += 1;
                false
            }
            _ => false,
        };
        let start = position;
        while bytes.get(position).is_some_and(u8::is_ascii_digit) {
            exponent = exponent
                .saturating_mul(10)
                .saturating_add(i64::from(bytes[position] - b'0'));
            position += 1;
        }
        if position == start {
            return None;
        }
        if negative {
            exponent = -exponent;
        }
    }
    if position != bytes.len() {
        return None;
    }
    Some(u32::try_from(fraction_digits.saturating_sub(exponent).max(0)).unwrap_or(u32::MAX))
}

// https://html.spec.whatwg.org/multipage/input.html#attr-input-step
#[derive(Clone, Debug)]
pub struct Decimal {
    negative: bool,
    digits: String,
    scale: u32,
}

impl Decimal {
    #[must_use]
    pub fn zero() -> Self {
        Self {
            negative: false,
            digits: "0".to_owned(),
            scale: 0,
        }
    }

    #[must_use]
    pub fn from_integer(value: i128) -> Self {
        let negative = value < 0;
        Self {
            negative,
            digits: value.unsigned_abs().to_string(),
            scale: 0,
        }
    }

    #[must_use]
    pub fn is_negative(&self) -> bool {
        self.negative && self.digits != "0"
    }

    #[must_use]
    pub fn is_zero(&self) -> bool {
        self.digits == "0"
    }

    #[must_use]
    pub fn mantissa_at(&self, scale: u32) -> Option<i128> {
        let zeros = scale.checked_sub(self.scale)?;
        let zeros = usize::try_from(zeros).ok()?;
        let mut text = String::with_capacity(self.digits.len() + zeros);
        text.push_str(&self.digits);
        text.extend(std::iter::repeat_n('0', zeros));
        let magnitude = text.parse::<i128>().ok()?;
        Some(if self.negative { -magnitude } else { magnitude })
    }

    #[must_use]
    pub fn scale(&self) -> u32 {
        self.scale
    }

    fn from_mantissa(mantissa: i128, scale: u32) -> Self {
        let negative = mantissa < 0;
        let mut decimal = Self {
            negative,
            digits: mantissa.unsigned_abs().to_string(),
            scale,
        };
        decimal.normalize();
        decimal
    }

    fn normalize(&mut self) {
        while self.digits.len() > 1 && self.digits.starts_with('0') {
            self.digits.remove(0);
        }
        while self.scale > 0 && self.digits.ends_with('0') {
            self.digits.pop();
            self.scale -= 1;
        }
        if self.digits == "0" {
            self.negative = false;
            self.scale = 0;
        }
    }

    #[must_use]
    pub fn to_f64(&self) -> f64 {
        let sign = if self.negative { "-" } else { "" };
        format!("{sign}{}e-{}", self.digits, self.scale)
            .parse()
            .unwrap_or(0.0)
    }

    #[must_use]
    pub fn compare(&self, other: &Self) -> Option<std::cmp::Ordering> {
        let scale = self.scale.max(other.scale);
        Some(self.mantissa_at(scale)?.cmp(&other.mantissa_at(scale)?))
    }
}

pub fn parse_decimal(input: &str) -> Option<Decimal> {
    let bytes = input.as_bytes();
    let mut position = 0;
    while bytes.get(position).is_some_and(u8::is_ascii_whitespace) {
        position += 1;
    }
    let negative = match bytes.get(position) {
        Some(b'-') => {
            position += 1;
            true
        }
        Some(b'+') => {
            position += 1;
            false
        }
        _ => false,
    };
    let mut digits = String::new();
    let mut fraction_digits = 0_u32;
    while bytes.get(position).is_some_and(u8::is_ascii_digit) {
        digits.push(char::from(bytes[position]));
        position += 1;
    }
    if bytes.get(position) == Some(&b'.') {
        position += 1;
        while bytes.get(position).is_some_and(u8::is_ascii_digit) {
            digits.push(char::from(bytes[position]));
            fraction_digits += 1;
            position += 1;
        }
    }
    if digits.is_empty() {
        return None;
    }
    let mut exponent = 0_i64;
    if bytes
        .get(position)
        .is_some_and(|byte| *byte == b'e' || *byte == b'E')
    {
        let mut cursor = position + 1;
        let exponent_negative = match bytes.get(cursor) {
            Some(b'-') => {
                cursor += 1;
                true
            }
            Some(b'+') => {
                cursor += 1;
                false
            }
            _ => false,
        };
        let start = cursor;
        while bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
            exponent = exponent
                .saturating_mul(10)
                .saturating_add(i64::from(bytes[cursor] - b'0'));
            cursor += 1;
        }
        if cursor > start && exponent_negative {
            exponent = -exponent;
        }
    }
    // https://html.spec.whatwg.org/multipage/common-microsyntaxes.html#rules-for-parsing-floating-point-number-values
    let significant = digits.trim_start_matches('0');
    if significant.is_empty() {
        return Some(Decimal::zero());
    }
    let significant_len = i64::try_from(significant.len()).unwrap_or(i64::MAX);
    let magnitude = significant_len
        .saturating_sub(1)
        .saturating_add(exponent)
        .saturating_sub(i64::from(fraction_digits));
    if magnitude > 309 {
        return None;
    }
    if magnitude < -324 {
        return Some(Decimal::zero());
    }
    let mut scale = i64::from(fraction_digits) - exponent;
    if scale < 0 {
        let zeros = usize::try_from(-scale).ok()?;
        digits.extend(std::iter::repeat_n('0', zeros));
        scale = 0;
    }
    let mut decimal = Decimal {
        negative,
        digits,
        scale: u32::try_from(scale).ok()?,
    };
    decimal.normalize();
    // https://html.spec.whatwg.org/multipage/common-microsyntaxes.html#rules-for-parsing-floating-point-number-values
    let number = decimal.to_f64();
    if !number.is_finite() {
        return None;
    }
    if number == 0.0 {
        return Some(Decimal::zero());
    }
    Some(decimal)
}

// https://html.spec.whatwg.org/multipage/input.html#range-state-(type=range
#[must_use]
pub fn align_range(
    minimum: &Decimal,
    maximum: &Decimal,
    step: &Decimal,
    step_base: &Decimal,
    value: &Decimal,
) -> Option<Decimal> {
    if maximum.compare(minimum)? == std::cmp::Ordering::Less {
        return Some(minimum.clone());
    }
    let scale = value
        .scale
        .max(maximum.scale)
        .max(minimum.scale)
        .max(step.scale)
        .max(step_base.scale);
    let value_at = value.mantissa_at(scale)?;
    let minimum_at = minimum.mantissa_at(scale)?;
    let maximum_at = maximum.mantissa_at(scale)?;
    let step_base_at = step_base.mantissa_at(scale)?;
    let step_at = step.mantissa_at(scale)?;
    if step_at <= 0 {
        return None;
    }
    let numerator = value_at
        .checked_sub(step_base_at)?
        .checked_mul(2)?
        .checked_add(step_at)?;
    let denominator = step_at.checked_mul(2)?;
    let quotient = numerator.div_euclid(denominator);
    let mut aligned = step_base_at.checked_add(quotient.checked_mul(step_at)?)?;
    if aligned > maximum_at {
        let steps = aligned
            .checked_sub(maximum_at)?
            .checked_add(step_at)?
            .checked_sub(1)?
            .div_euclid(step_at);
        aligned = aligned.checked_sub(steps.checked_mul(step_at)?)?;
    } else if aligned < minimum_at {
        let steps = minimum_at
            .checked_sub(aligned)?
            .checked_add(step_at)?
            .checked_sub(1)?
            .div_euclid(step_at);
        aligned = aligned.checked_add(steps.checked_mul(step_at)?)?;
    }
    if aligned < minimum_at || aligned > maximum_at {
        // https://html.spec.whatwg.org/multipage/input.html#range-state-(type=range
        return Some(Decimal::from_mantissa(
            value_at.clamp(minimum_at, maximum_at),
            scale,
        ));
    }
    Some(Decimal::from_mantissa(aligned, scale))
}

pub fn range_default(minimum: &Decimal, maximum: &Decimal) -> Decimal {
    if maximum
        .compare(minimum)
        .is_none_or(std::cmp::Ordering::is_lt)
    {
        return minimum.clone();
    }
    let scale = minimum.scale.max(maximum.scale);
    let Some(minimum_at) = minimum.mantissa_at(scale) else {
        return minimum.clone();
    };
    let Some(maximum_at) = maximum.mantissa_at(scale) else {
        return minimum.clone();
    };
    let Some(difference) = maximum_at.checked_sub(minimum_at) else {
        return minimum.clone();
    };
    let Some(half_scaled) = minimum_at.checked_mul(10).and_then(|minimum| {
        difference
            .checked_mul(5)
            .and_then(|half| minimum.checked_add(half))
    }) else {
        return minimum.clone();
    };
    let Some(scale) = scale.checked_add(1) else {
        return minimum.clone();
    };
    Decimal::from_mantissa(half_scaled, scale)
}

#[cfg(test)]
mod tests {
    use super::{
        authored_decimal_scale, date_value, local_datetime_value, month_value, time_value,
        week_value,
    };

    #[test]
    fn a_date_value_is_days_since_the_epoch() {
        assert_eq!(date_value("1970-01-01"), Some(0));
        assert_eq!(date_value("1970-01-02"), Some(1));
        assert_eq!(date_value("1969-12-31"), Some(-1));
        assert_eq!(date_value("2016-02-29"), Some(16_860));
        assert_eq!(date_value("0000-01-01"), None);
        assert_eq!(date_value("2019-02-29"), None);
        assert!(date_value("30000000000000000-01-01").is_some());
    }

    #[test]
    fn a_month_value_is_months_since_1970_01() {
        assert_eq!(month_value("1970-01"), Some(0));
        assert_eq!(month_value("1970-02"), Some(1));
        assert_eq!(month_value("1969-12"), Some(-1));
        assert_eq!(month_value("2019-13"), None);
        assert!(month_value("30000000000000000-12").is_some());
    }

    #[test]
    fn a_week_value_is_weeks_since_the_epoch_monday() {
        assert_eq!(week_value("1970-W01"), Some(0));
        assert_eq!(week_value("1970-W02"), Some(1));
        assert_eq!(week_value("2015-W53"), Some(2_400));
        assert_eq!(week_value("2016-W53"), None);
        assert_eq!(week_value("2020-W53"), Some(2_661));
        assert_eq!(week_value("2021-W53"), None);
    }

    #[test]
    fn a_time_value_is_seconds_since_midnight() {
        assert_eq!(time_value("00:00"), Some(0.0));
        assert_eq!(time_value("01:02"), Some(3_720.0));
        assert_eq!(time_value("01:02:03.5"), Some(3_723.5));
        assert_eq!(time_value("24:00"), None);
        assert_eq!(time_value("01:02.5"), None);
    }

    #[test]
    fn a_local_datetime_value_combines_days_and_seconds() {
        assert_eq!(local_datetime_value("1970-01-01T00:00"), Some(0.0));
        assert_eq!(local_datetime_value("1970-01-02 01:00"), Some(90_000.0));
        assert_eq!(local_datetime_value("1970-01-01T24:00"), None);
    }

    #[test]
    fn an_authored_decimal_scale_counts_decimal_places() {
        assert_eq!(authored_decimal_scale("1"), Some(0));
        assert_eq!(authored_decimal_scale("1.20"), Some(2));
        assert_eq!(authored_decimal_scale("1e2"), Some(0));
        assert_eq!(authored_decimal_scale("1.2e2"), Some(0));
        assert_eq!(authored_decimal_scale("1.2e-2"), Some(3));
        assert_eq!(
            authored_decimal_scale("1e-99999999999999999999"),
            Some(u32::MAX)
        );
        assert_eq!(authored_decimal_scale("1px"), None);
        assert_eq!(authored_decimal_scale("1e"), None);
    }
}
