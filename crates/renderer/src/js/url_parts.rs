// https://html.spec.whatwg.org/multipage/links.html#url-decomposition-idl-attributes

use rquickjs::{Ctx, Result, prelude::Func};

use std::borrow::Cow;
use std::fmt::Write as _;

use encoding_rs::EncoderResult;
use url::quirks;

const PROTOCOL: u32 = 0;
const USERNAME: u32 = 1;
const PASSWORD: u32 = 2;
const HOST: u32 = 3;
const HOSTNAME: u32 = 4;
const PORT: u32 = 5;
const PATHNAME: u32 = 6;
const SEARCH: u32 = 7;
const HASH: u32 = 8;

pub(super) fn install(ctx: &Ctx<'_>) -> Result<()> {
    super::bridge::object(ctx)?.set(
        "__tbUrlParts",
        Func::from(|spec: String, base: String, label: String| parts(&spec, &base, &label)),
    )?;
    super::bridge::object(ctx)?.set(
        "__tbUrlSetPart",
        Func::from(
            |spec: String, base: String, part: u32, value: String, label: String| {
                set_part(&spec, &base, part, &value, &label)
            },
        ),
    )?;
    super::bridge::object(ctx)?.set(
        "__tbParseParams",
        Func::from(|input: String| parse_params(&input)),
    )?;
    super::bridge::object(ctx)?.set(
        "__tbSerializeParams",
        Func::from(|pairs: Vec<Vec<String>>| serialize_params(&pairs)),
    )?;
    Ok(())
}

// https://url.spec.whatwg.org/#concept-urlencoded-parser
fn parse_params(input: &str) -> Vec<Vec<String>> {
    url::form_urlencoded::parse(input.as_bytes())
        .into_owned()
        .map(|(name, value)| vec![name, value])
        .collect()
}

// https://url.spec.whatwg.org/#concept-urlencoded-serializer
fn serialize_params(pairs: &[Vec<String>]) -> String {
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    serializer.extend_pairs(pairs.iter().filter_map(|pair| {
        let (name, rest) = pair.split_first()?;
        Some((name.as_str(), rest.first().map_or("", String::as_str)))
    }));
    serializer.finish()
}

fn parts(spec: &str, base: &str, label: &str) -> Option<Vec<String>> {
    let url = resolve(spec, base, label)?;
    Some(vec![
        format!("{}:", url.scheme()),
        url.username().to_owned(),
        url.password().unwrap_or_default().to_owned(),
        host(&url),
        url.host_str().unwrap_or_default().to_owned(),
        url.port().map_or_else(String::new, |port| port.to_string()),
        url.path().to_owned(),
        // https://url.spec.whatwg.org/#dom-url-search
        url.query()
            .filter(|query| !query.is_empty())
            .map_or_else(String::new, |query| format!("?{query}")),
        url.fragment()
            .filter(|fragment| !fragment.is_empty())
            .map_or_else(String::new, |fragment| format!("#{fragment}")),
        url.origin().ascii_serialization(),
    ])
}

// https://url.spec.whatwg.org/#concept-basic-url-parser
fn set_part(spec: &str, base: &str, part: u32, value: &str, label: &str) -> Option<String> {
    let mut url = resolve(spec, base, label)?;
    // https://url.spec.whatwg.org/#concept-basic-url-parser
    let stripped = strip_ascii_tab_newline(value);
    let changed = match part {
        PROTOCOL => quirks::set_protocol(&mut url, &stripped).is_ok(),
        USERNAME => quirks::set_username(&mut url, value).is_ok(),
        PASSWORD => quirks::set_password(&mut url, value).is_ok(),
        HOST => set_host(&mut url, &stripped, true),
        HOSTNAME => set_host(&mut url, &stripped, false),
        PORT => {
            // https://url.spec.whatwg.org/#dom-url-port
            if !value.is_empty() && stripped.is_empty() {
                false
            } else {
                quirks::set_port(&mut url, &stripped).is_ok()
            }
        }
        PATHNAME => {
            quirks::set_pathname(&mut url, &stripped);
            true
        }
        SEARCH => {
            quirks::set_search(&mut url, &stripped);
            true
        }
        HASH => {
            quirks::set_hash(&mut url, &stripped);
            true
        }
        _ => false,
    };
    changed.then(|| url.to_string())
}

// https://url.spec.whatwg.org/#concept-host-setter
fn set_host(url: &mut url::Url, value: &str, with_port: bool) -> bool {
    if url.scheme() == "file" && with_port && value.contains(':') {
        return false;
    }
    let changed = if with_port {
        quirks::set_host(url, value)
    } else {
        quirks::set_hostname(url, value)
    };
    if changed.is_err() {
        return false;
    }
    if url.scheme() == "file" && url.host_str() == Some("localhost") {
        return quirks::set_host(url, "").is_ok();
    }
    true
}

// https://url.spec.whatwg.org/#concept-basic-url-parser
fn strip_ascii_tab_newline(value: &str) -> Cow<'_, str> {
    if !value.contains(['\t', '\n', '\r']) {
        return Cow::Borrowed(value);
    }
    Cow::Owned(
        value
            .chars()
            .filter(|character| !matches!(character, '\t' | '\n' | '\r'))
            .collect(),
    )
}

// https://url.spec.whatwg.org/#concept-basic-url-parser
fn resolve(spec: &str, base: &str, label: &str) -> Option<url::Url> {
    let encoding = encoding_rs::Encoding::for_label(label.as_bytes()).unwrap_or(encoding_rs::UTF_8);
    url::Url::parse(base)
        .ok()
        .and_then(|base| encoding_parse_url(spec, &base, encoding))
        .or_else(|| url::Url::parse(spec).ok())
}

// https://url.spec.whatwg.org/#concept-url-parser
// https://html.spec.whatwg.org/multipage/urls-and-fetching.html#parse-a-url
pub(super) fn encoding_parse_url(
    href: &str,
    base: &url::Url,
    encoding: &'static encoding_rs::Encoding,
) -> Option<url::Url> {
    let mut url = base.join(href).ok()?;
    // https://url.spec.whatwg.org/#query-state
    if encoding.name() == encoding_rs::UTF_8.name()
        || !matches!(url.scheme(), "ftp" | "file" | "http" | "https")
    {
        return Some(url);
    }
    let before_fragment = href.split('#').next().unwrap_or_default();
    let Some((_, raw)) = before_fragment.split_once('?') else {
        return Some(url);
    };
    // https://url.spec.whatwg.org/#string-percent-encode-after-encoding
    let mut encoder = encoding.new_encoder();
    let mut query = String::with_capacity(raw.len());
    let mut remaining = raw;
    let mut buffer = [0u8; 256];
    loop {
        let (result, read, written) =
            encoder.encode_from_utf8_without_replacement(remaining, &mut buffer, true);
        for &byte in &buffer[..written] {
            if byte.is_ascii() {
                query.push(byte as char);
            } else {
                query.push('%');
                query.push(HEX[(byte >> 4) as usize] as char);
                query.push(HEX[(byte & 0xf) as usize] as char);
            }
        }
        remaining = &remaining[read..];
        match result {
            EncoderResult::InputEmpty => break,
            EncoderResult::OutputFull => {}
            EncoderResult::Unmappable(ch) => {
                let _ = write!(query, "%26%23{}%3B", ch as u32);
            }
        }
    }
    url.set_query(Some(&query));
    Some(url)
}

const HEX: &[u8; 16] = b"0123456789ABCDEF";

// https://url.spec.whatwg.org/#concept-url-host
fn host(url: &url::Url) -> String {
    let mut host = match url.host() {
        Some(url::Host::Ipv6(address)) => format!("[{address}]"),
        _ => url.host_str().unwrap_or_default().to_owned(),
    };
    if let Some(port) = url.port() {
        host.push(':');
        host.push_str(&port.to_string());
    }
    host
}

#[cfg(test)]
mod tests {
    use super::encoding_parse_url;

    #[test]
    fn a_query_encodes_in_the_override_encoding() {
        let base = url::Url::parse("https://example.com/page").unwrap();
        let shift_jis =
            encoding_parse_url("x?q=\u{65E5}", &base, encoding_rs::SHIFT_JIS).expect("parse");
        assert_eq!(shift_jis.as_str(), "https://example.com/x?q=%93%FA");
        let unmappable =
            encoding_parse_url("x?q=\u{AC00}", &base, encoding_rs::SHIFT_JIS).expect("parse");
        assert_eq!(
            unmappable.as_str(),
            "https://example.com/x?q=%26%2344032%3B"
        );
        let iso = encoding_parse_url("x?q=\u{65E5}\u{AC00}a", &base, encoding_rs::ISO_2022_JP)
            .expect("parse");
        assert_eq!(
            iso.as_str(),
            "https://example.com/x?q=%1B$BF|%1B(B%26%2344032%3Ba"
        );
        let utf8 = encoding_parse_url("x?q=\u{65E5}", &base, encoding_rs::UTF_8).expect("parse");
        assert_eq!(utf8.as_str(), "https://example.com/x?q=%E6%97%A5");
    }

    #[test]
    fn non_special_and_websocket_schemes_always_use_utf8_queries() {
        let base = url::Url::parse("https://example.com/page").unwrap();
        let ws = encoding_parse_url(
            "ws://example.invalid/?x=\u{00E5}",
            &base,
            encoding_rs::WINDOWS_1252,
        )
        .expect("parse");
        assert_eq!(ws.as_str(), "ws://example.invalid/?x=%C3%A5");
        let mailto = encoding_parse_url(
            "mailto:a@b.invalid?x=\u{00E5}",
            &base,
            encoding_rs::WINDOWS_1252,
        )
        .expect("parse");
        assert_eq!(mailto.as_str(), "mailto:a@b.invalid?x=%C3%A5");
    }

    #[test]
    fn a_query_the_href_does_not_name_is_left_alone() {
        let base = url::Url::parse("https://example.com/p?already=1").unwrap();
        let fragment = encoding_parse_url("#top", &base, encoding_rs::SHIFT_JIS).expect("parse");
        assert_eq!(fragment.as_str(), "https://example.com/p?already=1#top");
        let fragment_with_query =
            encoding_parse_url("x#a?b", &base, encoding_rs::SHIFT_JIS).expect("parse");
        assert_eq!(fragment_with_query.as_str(), "https://example.com/x#a?b");
        let empty = encoding_parse_url("x?", &base, encoding_rs::SHIFT_JIS).expect("parse");
        assert_eq!(empty.as_str(), "https://example.com/x?");
    }
}
