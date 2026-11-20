use std::cell::RefCell;
use std::rc::Rc;

use fixed_decimal::{
    Decimal, FloatPrecision, Sign, SignDisplay, SignedRoundingMode, UnsignedRoundingMode,
};
use icu_calendar::Gregorian;
use icu_datetime::{
    FixedCalendarDateTimeFormatter, NoCalendarFormatter,
    fieldsets::{T, YMD, YMDE},
    input::{Date, DateTime, Time},
    options::YearStyle,
};
use icu_decimal::{
    DecimalFormatter,
    options::{DecimalFormatterOptions, GroupingStrategy},
};
use icu_experimental::dimension::{
    currency::{
        CurrencyType,
        formatter::{CurrencyFormatter, CurrencyFormatterPreferences},
        options::{CurrencyFormatterOptions, CurrencyUsage},
    },
    percent::{
        formatter::{PercentFormatter, PercentFormatterPreferences},
        options::PercentFormatterOptions,
    },
    provider::currency::fractions::CurrencyFractionsV1,
};
use icu_locale::{LocaleCanonicalizer, fallback::LocaleFallbacker};
use icu_locale_core::Locale;
use icu_provider::{DataProvider, DataRequest, buf::AsDeserializingBufferProvider};
use icu_provider_adapters::fallback::LocaleFallbackProvider;
use icu_provider_blob::BlobDataProvider;
use rquickjs::{Array, Ctx, Exception, Result, prelude::Func};
use writeable::Writeable;

type IntlProvider = LocaleFallbackProvider<BlobDataProvider>;

struct NumberFormatInput {
    locale: String,
    value: NumberFormatValue,
    style: String,
    currency: String,
    currency_display: String,
    currency_sign: String,
    minimum_integer_digits: u8,
    minimum_fraction_digits: u8,
    maximum_fraction_digits: u8,
    minimum_significant_digits: u8,
    maximum_significant_digits: u8,
    use_grouping: bool,
    sign_display: String,
}

enum NumberFormatValue {
    Number(f64),
    Exact(String),
}

struct DateTimeFormatInput {
    locale: String,
    year: i32,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    second: u8,
    date_style: u8,
    time_style: u8,
}

const ICU_DATA: &[u8] = include_bytes!("intl_data.postcard");

// https://402.ecma-international.org/#sec-intl-object
// https://searchfox.org/firefox-main/source/js/src/builtin/intl/NumberFormat.cpp#1128-1307
// https://searchfox.org/firefox-main/source/js/src/builtin/intl/NumberFormat.cpp#1652-1689
// https://searchfox.org/firefox-main/source/js/src/builtin/intl/DateTimeFormat.cpp#497-855
const INSTALL_INTL_DEFLATE: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/js_blobs/intl.deflate"));

fn install_intl_js(ctx: &Ctx<'_>) -> Result<&'static str> {
    static CACHE: std::sync::OnceLock<Box<str>> = std::sync::OnceLock::new();
    super::blob::decompress(ctx, INSTALL_INTL_DEFLATE, &CACHE)
}

struct IntlData {
    canonicalizer: Rc<LocaleCanonicalizer>,
    provider: Rc<IntlProvider>,
}

thread_local! {
    static INTL_DATA: RefCell<Option<IntlData>> = const { RefCell::new(None) };
}

fn intl_data(ctx: &Ctx<'_>) -> Result<(Rc<LocaleCanonicalizer>, Rc<IntlProvider>)> {
    INTL_DATA.with(|cell| {
        let mut slot = cell.borrow_mut();
        if let Some(data) = slot.as_ref() {
            return Ok((Rc::clone(&data.canonicalizer), Rc::clone(&data.provider)));
        }
        let blob = BlobDataProvider::try_new_from_static_blob(ICU_DATA)
            .map_err(|err| Exception::throw_internal(ctx, &err.to_string()))?;
        let canonicalizer = Rc::new(
            LocaleCanonicalizer::try_new_common_with_buffer_provider(&blob)
                .map_err(|err| Exception::throw_internal(ctx, &err.to_string()))?,
        );
        let fallbacker = LocaleFallbacker::try_new_with_buffer_provider(&blob)
            .map_err(|err| Exception::throw_internal(ctx, &err.to_string()))?;
        let data = IntlData {
            canonicalizer,
            provider: Rc::new(LocaleFallbackProvider::new(blob, fallbacker)),
        };
        let handles = (Rc::clone(&data.canonicalizer), Rc::clone(&data.provider));
        *slot = Some(data);
        Ok(handles)
    })
}

pub(super) fn install(ctx: &Ctx<'_>) -> Result<()> {
    let (canonicalizer, provider) = intl_data(ctx)?;

    let number_provider = Rc::clone(&provider);
    let date_provider = Rc::clone(&provider);
    let currency_provider = Rc::clone(&provider);
    let locale_canonicalizer = Rc::clone(&canonicalizer);
    let globals = super::bridge::object(ctx)?;
    globals.set(
        "__tbIntlCanonicalLocale",
        Func::from(move |tag: String| canonicalize_locale(&locale_canonicalizer, &tag)),
    )?;
    globals.set(
        "__tbIntlResolveLocale",
        Func::from(|tag: String| resolve_locale(&tag)),
    )?;
    globals.set(
        "__tbIntlCurrencyDigits",
        Func::from(move |ctx: Ctx<'_>, currency: String| {
            currency_digits(&ctx, &currency_provider, &currency)
        }),
    )?;
    globals.set(
        "__tbIntlFormatNumber",
        Func::from(move |ctx: Ctx<'_>, args: Array<'_>| {
            format_number_args(&ctx, &number_provider, &args)
        }),
    )?;
    globals.set(
        "__tbIntlFormatDateTime",
        Func::from(move |ctx: Ctx<'_>, args: Array<'_>| {
            format_date_time_args(&ctx, &date_provider, &args)
        }),
    )?;
    super::bridge::evaluate(ctx, install_intl_js(ctx)?)
}

fn resolve_locale(tag: &str) -> String {
    tag.parse::<Locale>()
        .map_or_default(|locale| locale.to_string())
}

fn canonicalize_locale(canonicalizer: &LocaleCanonicalizer, tag: &str) -> String {
    let Ok(mut locale) = tag.parse::<Locale>() else {
        return "!".to_owned();
    };
    canonicalizer.canonicalize(&mut locale);
    locale.to_string()
}

fn format_number_args(ctx: &Ctx<'_>, provider: &IntlProvider, args: &Array<'_>) -> Result<String> {
    let number: f64 = args.get(1)?;
    let exact: String = args.get(2)?;
    let input = NumberFormatInput {
        locale: args.get(0)?,
        value: if exact.is_empty() {
            NumberFormatValue::Number(number)
        } else {
            NumberFormatValue::Exact(exact)
        },
        style: args.get(3)?,
        currency: args.get(4)?,
        currency_display: args.get(5)?,
        currency_sign: args.get(6)?,
        minimum_integer_digits: args.get(7)?,
        minimum_fraction_digits: args.get(8)?,
        maximum_fraction_digits: args.get(9)?,
        minimum_significant_digits: args.get(10)?,
        maximum_significant_digits: args.get(11)?,
        use_grouping: args.get(12)?,
        sign_display: args.get(13)?,
    };
    format_number(ctx, provider, &input)
}

fn format_date_time_args(
    ctx: &Ctx<'_>,
    provider: &IntlProvider,
    args: &Array<'_>,
) -> Result<String> {
    let input = DateTimeFormatInput {
        locale: args.get(0)?,
        year: args.get(1)?,
        month: args.get(2)?,
        day: args.get(3)?,
        hour: args.get(4)?,
        minute: args.get(5)?,
        second: args.get(6)?,
        date_style: args.get(7)?,
        time_style: args.get(8)?,
    };
    format_date_time(ctx, provider, &input)
}

// https://402.ecma-international.org/#sec-tointlmathematicalvalue
// https://262.ecma-international.org/#sec-stringnumericliteral
fn parse_exact_decimal(ctx: &Ctx<'_>, value: &str) -> Result<Decimal> {
    let value = value.strip_prefix('+').unwrap_or(value);
    let mut normalized = String::with_capacity(value.len() + 1);
    match value.strip_prefix("-.") {
        Some(fraction) => {
            normalized.push_str("-0.");
            normalized.push_str(fraction);
        }
        None => match value.strip_prefix('.') {
            Some(fraction) => {
                normalized.push_str("0.");
                normalized.push_str(fraction);
            }
            None => normalized.push_str(value),
        },
    }
    if normalized.ends_with('.') {
        normalized.pop();
    }
    normalized
        .parse::<Decimal>()
        .map_err(|err| Exception::throw_internal(ctx, &err.to_string()))
}

fn format_number(
    ctx: &Ctx<'_>,
    provider: &IntlProvider,
    input: &NumberFormatInput,
) -> Result<String> {
    let locale = input
        .locale
        .parse::<Locale>()
        .map_err(|err| Exception::throw_internal(ctx, &err.to_string()))?;
    let mut decimal = match &input.value {
        NumberFormatValue::Number(value) if !value.is_finite() => {
            return format_nonfinite(ctx, provider, input, locale, *value);
        }
        NumberFormatValue::Number(value) => {
            Decimal::try_from_f64(*value, FloatPrecision::RoundTrip)
                .map_err(|err| Exception::throw_internal(ctx, &err.to_string()))?
        }
        NumberFormatValue::Exact(value) => parse_exact_decimal(ctx, value)?,
    };
    prepare_decimal(input, &mut decimal);
    format_prepared_decimal(ctx, provider, input, locale, &decimal)
}

fn prepare_decimal(input: &NumberFormatInput, decimal: &mut Decimal) {
    if input.style == "percent" {
        decimal.multiply_pow10(2);
        decimal.trim_start();
    }
    if input.maximum_significant_digits == 0 {
        let position = -i16::from(input.maximum_fraction_digits);
        decimal.round_with_mode(
            position,
            SignedRoundingMode::Unsigned(UnsignedRoundingMode::HalfExpand),
        );
        decimal.trim_end();
        decimal.pad_end(-i16::from(input.minimum_fraction_digits));
    } else {
        let position =
            decimal.nonzero_magnitude_start() - i16::from(input.maximum_significant_digits) + 1;
        decimal.round_with_mode(
            position,
            SignedRoundingMode::Unsigned(UnsignedRoundingMode::HalfExpand),
        );
        decimal.trim_end();
        let minimum_position =
            decimal.nonzero_magnitude_start() - i16::from(input.minimum_significant_digits) + 1;
        decimal.pad_end(minimum_position);
    }
    decimal.pad_start(i16::from(input.minimum_integer_digits));
    let display = match input.sign_display.as_str() {
        "never" => SignDisplay::Never,
        "always" => SignDisplay::Always,
        "exceptZero" => SignDisplay::ExceptZero,
        "negative" => SignDisplay::Negative,
        _ => SignDisplay::Auto,
    };
    decimal.apply_sign_display(display);
}

fn format_prepared_decimal(
    ctx: &Ctx<'_>,
    provider: &IntlProvider,
    input: &NumberFormatInput,
    locale: Locale,
    decimal: &Decimal,
) -> Result<String> {
    match input.style.as_str() {
        "currency" => format_currency(
            ctx,
            provider,
            locale,
            decimal,
            &input.currency,
            &input.currency_display,
            &input.currency_sign,
        ),
        "percent" => {
            let formatter = PercentFormatter::try_new_with_buffer_provider(
                provider,
                PercentFormatterPreferences::from(locale),
                PercentFormatterOptions::default(),
            )
            .map_err(|err| Exception::throw_internal(ctx, &err.to_string()))?;
            Ok(formatter.format(decimal).write_to_string().into_owned())
        }
        _ => {
            let grouping = if input.use_grouping {
                GroupingStrategy::Auto
            } else {
                GroupingStrategy::Never
            };
            let formatter = DecimalFormatter::try_new_with_buffer_provider(
                provider,
                locale.into(),
                DecimalFormatterOptions::from(grouping),
            )
            .map_err(|err| Exception::throw_internal(ctx, &err.to_string()))?;
            Ok(formatter.format(decimal).write_to_string().into_owned())
        }
    }
}

fn format_nonfinite(
    ctx: &Ctx<'_>,
    provider: &IntlProvider,
    input: &NumberFormatInput,
    locale: Locale,
    value: f64,
) -> Result<String> {
    let mut placeholder = Decimal::from(0);
    if value.is_sign_negative() {
        placeholder.set_sign(Sign::Negative);
    }
    prepare_decimal(input, &mut placeholder);
    let formatted = format_prepared_decimal(ctx, provider, input, locale.clone(), &placeholder)?;

    let mut unsigned = placeholder;
    unsigned.set_sign(Sign::None);
    let number_formatter = DecimalFormatter::try_new_with_buffer_provider(
        provider,
        locale.into(),
        DecimalFormatterOptions::from(GroupingStrategy::Never),
    )
    .map_err(|err| Exception::throw_internal(ctx, &err.to_string()))?;
    let digits = number_formatter
        .format(&unsigned)
        .write_to_string()
        .into_owned();
    let replacement = if value.is_nan() { "NaN" } else { "∞" };
    if !formatted.contains(&digits) {
        return Err(Exception::throw_internal(
            ctx,
            "ICU4X non-finite placeholder did not contain its decimal digits",
        ));
    }
    Ok(formatted.replacen(&digits, replacement, 1))
}

fn currency_digits(ctx: &Ctx<'_>, provider: &IntlProvider, currency: &str) -> Result<u8> {
    let currency = currency
        .parse::<CurrencyType>()
        .map_err(|err| Exception::throw_range(ctx, &err.to_string()))?;
    let fractions = DataProvider::<CurrencyFractionsV1>::load(
        &provider.as_deserializing(),
        DataRequest::default(),
    )
    .map_err(|err| Exception::throw_internal(ctx, &err.to_string()))?
    .payload;
    Ok(fractions.get().resolve(currency).digits)
}

macro_rules! format_currency_value {
    ($formatter:expr, $decimal:expr) => {
        $formatter.map(|formatter| {
            formatter
                .format_fixed_decimal($decimal)
                .write_to_string()
                .into_owned()
        })
    };
}

macro_rules! format_fixed_calendar {
    ($ctx:expr, $provider:expr, $locale:expr, $fieldset:expr, $value:expr) => {
        FixedCalendarDateTimeFormatter::<Gregorian, _>::try_new_with_buffer_provider(
            $provider,
            $locale.into(),
            $fieldset,
        )
        .map(|formatter| formatter.format($value).write_to_string().into_owned())
        .map_err(|err| Exception::throw_internal($ctx, &err.to_string()))
    };
}

fn format_currency(
    ctx: &Ctx<'_>,
    provider: &IntlProvider,
    locale: Locale,
    decimal: &Decimal,
    currency: &str,
    display: &str,
    sign: &str,
) -> Result<String> {
    let currency = currency
        .parse::<CurrencyType>()
        .map_err(|err| Exception::throw_range(ctx, &err.to_string()))?;
    let mut options = CurrencyFormatterOptions::default();
    options.usage = if sign == "accounting" {
        CurrencyUsage::Accounting
    } else {
        CurrencyUsage::Standard
    };
    let preferences = CurrencyFormatterPreferences::from(locale);
    match display {
        "code" => format_currency_value!(
            CurrencyFormatter::try_new_code_with_buffer_provider(
                provider,
                preferences,
                currency,
                options
            ),
            decimal
        ),
        "name" => format_currency_value!(
            CurrencyFormatter::try_new_name_with_buffer_provider(provider, preferences, currency),
            decimal
        ),
        "narrowSymbol" => format_currency_value!(
            CurrencyFormatter::try_new_symbol_narrow_with_buffer_provider(
                provider,
                preferences,
                currency,
                options
            ),
            decimal
        ),
        _ => format_currency_value!(
            CurrencyFormatter::try_new_symbol_with_buffer_provider(
                provider,
                preferences,
                currency,
                options
            ),
            decimal
        ),
    }
    .map_err(|err| Exception::throw_internal(ctx, &err.to_string()))
}

fn format_date_time(
    ctx: &Ctx<'_>,
    provider: &IntlProvider,
    input: &DateTimeFormatInput,
) -> Result<String> {
    let locale = input
        .locale
        .parse::<Locale>()
        .map_err(|err| Exception::throw_internal(ctx, &err.to_string()))?;
    let date = Date::try_new_gregorian(input.year, input.month, input.day)
        .map_err(|err| Exception::throw_range(ctx, &err.to_string()))?;
    let time = Time::try_new(input.hour, input.minute, input.second, 0)
        .map_err(|err| Exception::throw_range(ctx, &err.to_string()))?;
    let datetime = DateTime { date, time };

    match (input.date_style, input.time_style) {
        (0, time_style) => format_time(ctx, provider, locale, time, time_style),
        (date_style, 0) => format_date(ctx, provider, locale, date, date_style),
        (date_style, time_style) => {
            format_combined(ctx, provider, locale, &datetime, date_style, time_style)
        }
    }
}

fn format_time(
    ctx: &Ctx<'_>,
    provider: &IntlProvider,
    locale: Locale,
    time: Time,
    style: u8,
) -> Result<String> {
    let fieldset = if style == 1 { T::hm() } else { T::hms() };
    NoCalendarFormatter::try_new_with_buffer_provider(provider, locale.into(), fieldset)
        .map(|formatter| formatter.format(&time).write_to_string().into_owned())
        .map_err(|err| Exception::throw_internal(ctx, &err.to_string()))
}

fn format_date(
    ctx: &Ctx<'_>,
    provider: &IntlProvider,
    locale: Locale,
    date: Date<Gregorian>,
    style: u8,
) -> Result<String> {
    match style {
        1 => format_fixed_calendar!(ctx, provider, locale, YMD::short(), &date),
        2 => format_fixed_calendar!(ctx, provider, locale, YMD::medium(), &date),
        3 => format_fixed_calendar!(ctx, provider, locale, YMD::long(), &date),
        5 => format_fixed_calendar!(
            ctx,
            provider,
            locale,
            YMD::short().with_year_style(YearStyle::Full),
            &date
        ),
        _ => format_fixed_calendar!(ctx, provider, locale, YMDE::long(), &date),
    }
}

fn format_combined(
    ctx: &Ctx<'_>,
    provider: &IntlProvider,
    locale: Locale,
    datetime: &DateTime<Gregorian>,
    date_style: u8,
    time_style: u8,
) -> Result<String> {
    match (date_style, time_style) {
        (5, 1) => format_fixed_calendar!(
            ctx,
            provider,
            locale,
            YMD::short().with_year_style(YearStyle::Full).with_time_hm(),
            datetime
        ),
        (5, _) => format_fixed_calendar!(
            ctx,
            provider,
            locale,
            YMD::short()
                .with_year_style(YearStyle::Full)
                .with_time_hms(),
            datetime
        ),
        (4..=u8::MAX, 1) => {
            format_fixed_calendar!(ctx, provider, locale, YMDE::long().with_time_hm(), datetime)
        }
        (4..=u8::MAX, _) => format_fixed_calendar!(
            ctx,
            provider,
            locale,
            YMDE::long().with_time_hms(),
            datetime
        ),
        (3, 1) => {
            format_fixed_calendar!(ctx, provider, locale, YMD::long().with_time_hm(), datetime)
        }
        (3, _) => {
            format_fixed_calendar!(ctx, provider, locale, YMD::long().with_time_hms(), datetime)
        }
        (2, 1) => format_fixed_calendar!(
            ctx,
            provider,
            locale,
            YMD::medium().with_time_hm(),
            datetime
        ),
        (2, _) => format_fixed_calendar!(
            ctx,
            provider,
            locale,
            YMD::medium().with_time_hms(),
            datetime
        ),
        (1, 1) => {
            format_fixed_calendar!(ctx, provider, locale, YMD::short().with_time_hm(), datetime)
        }
        _ => format_fixed_calendar!(
            ctx,
            provider,
            locale,
            YMD::short().with_time_hms(),
            datetime
        ),
    }
}
