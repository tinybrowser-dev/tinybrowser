//! URL decomposition for `<a>` and `<area>`
//! (<https://html.spec.whatwg.org/multipage/links.html#url-decomposition-idl-attributes>).
//!
//! The renderer links the spec-grade `url` crate, so the decomposition is a
//! thin binding: parse the element's href against the document base URL, read
//! or set one component, and serialize. Failed parses return no parts; the
//! IDL getters turn that into their per-attribute failure value, and setters
//! leave the attribute unchanged.

use rquickjs::{Ctx, Result, prelude::Func};

use std::borrow::Cow;

use url::quirks;

/// Component indexes shared with the JavaScript accessors.
const PROTOCOL: u32 = 0;
const USERNAME: u32 = 1;
const PASSWORD: u32 = 2;
const HOST: u32 = 3;
const HOSTNAME: u32 = 4;
const PORT: u32 = 5;
const PATHNAME: u32 = 6;
const SEARCH: u32 = 7;
const HASH: u32 = 8;

/// Installs the decomposition hooks the JS shim calls.
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

/// Runs the `application/x-www-form-urlencoded` parser over `input`
/// (<https://url.spec.whatwg.org/#concept-urlencoded-parser>).
///
/// Pairs cross as two-element arrays: plain tuples have no `rquickjs`
/// conversion impls.
fn parse_params(input: &str) -> Vec<Vec<String>> {
    url::form_urlencoded::parse(input.as_bytes())
        .into_owned()
        .map(|(name, value)| vec![name, value])
        .collect()
}

/// Runs the `application/x-www-form-urlencoded` serializer over `pairs`
/// (<https://url.spec.whatwg.org/#concept-urlencoded-serializer>).
fn serialize_params(pairs: &[Vec<String>]) -> String {
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    serializer.extend_pairs(pairs.iter().filter_map(|pair| {
        let (name, rest) = pair.split_first()?;
        Some((name.as_str(), rest.first().map_or("", String::as_str)))
    }));
    serializer.finish()
}

/// The decomposition components of `spec` resolved against `base`, or `None`
/// when it does not parse.
///
/// `label` is the document encoding the IDL attributes parse with
/// ([`encoding_parse_url`]).
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
        // An empty query or fragment is still `null`/`""` in the
        // serialization but reads back as the empty string
        // (<https://url.spec.whatwg.org/#dom-url-search>).
        url.query()
            .filter(|query| !query.is_empty())
            .map_or_else(String::new, |query| format!("?{query}")),
        url.fragment()
            .filter(|fragment| !fragment.is_empty())
            .map_or_else(String::new, |fragment| format!("#{fragment}")),
        url.origin().ascii_serialization(),
    ])
}

/// Sets one component of `spec` resolved against `base` and returns the new
/// serialization; `None` leaves the caller's attribute untouched. `label` is
/// the document encoding the initial parse uses
/// ([`encoding_parse_url`]); the component's own value always encodes UTF-8
/// (<https://url.spec.whatwg.org/#concept-basic-url-parser>).
fn set_part(spec: &str, base: &str, part: u32, value: &str, label: &str) -> Option<String> {
    let mut url = resolve(spec, base, label)?;
    // Setters run the basic URL parser on their input, which drops ASCII tab
    // and newline; `username` and `password` instead percent-encode the value
    // as-is (<https://url.spec.whatwg.org/#concept-basic-url-parser>).
    let stripped = strip_ascii_tab_newline(value);
    let changed = match part {
        PROTOCOL => quirks::set_protocol(&mut url, &stripped).is_ok(),
        USERNAME => quirks::set_username(&mut url, value).is_ok(),
        PASSWORD => quirks::set_password(&mut url, value).is_ok(),
        HOST => set_host(&mut url, &stripped, true),
        HOSTNAME => set_host(&mut url, &stripped, false),
        PORT => {
            // A value that is non-empty but loses everything to tab/newline
            // stripping leaves the port untouched; browsers do not clear it
            // (<https://url.spec.whatwg.org/#dom-url-port>).
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

/// The browser `host`/`hostname` setters: a `file:` URL rejects a port and
/// maps a `localhost` host to the empty host
/// (<https://url.spec.whatwg.org/#concept-host-setter>).
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

/// Removes ASCII tab and newline, as the basic URL parser does before every
/// setter parse (<https://url.spec.whatwg.org/#concept-basic-url-parser>).
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

/// Parses `spec` relative to `base`, falling back to an absolute parse
/// (<https://url.spec.whatwg.org/#concept-basic-url-parser>). Special schemes
/// with a matching base scheme take the relative path (`http:foo.com` joins
/// the base), so base-relative parsing must come first.
///
/// `label` is the document encoding, resolved the way HTML resolves encoding
/// labels (an unknown label is UTF-8).
fn resolve(spec: &str, base: &str, label: &str) -> Option<url::Url> {
    let encoding = encoding_rs::Encoding::for_label(label.as_bytes()).unwrap_or(encoding_rs::UTF_8);
    url::Url::parse(base)
        .ok()
        .and_then(|base| encoding_parse_url(spec, &base, encoding))
        .or_else(|| url::Url::parse(spec).ok())
}

/// Parses `href` against `base` with `encoding` as the URL parser's encoding
/// override: the query's non-ASCII bytes percent-encode in `encoding` rather
/// than UTF-8 (<https://url.spec.whatwg.org/#concept-url-parser>'s encoding
/// override, used by HTML's "parse a URL" with the node document's character
/// encoding,
/// <https://html.spec.whatwg.org/multipage/urls-and-fetching.html#parse-a-url>).
///
/// Only the query `href` itself names is re-encoded: a base URL's query is
/// already encoded, so a relative reference with no `?` inherits it unchanged.
pub(super) fn encoding_parse_url(
    href: &str,
    base: &url::Url,
    encoding: &'static encoding_rs::Encoding,
) -> Option<url::Url> {
    let mut url = base.join(href).ok()?;
    if encoding.name() == encoding_rs::UTF_8.name() {
        return Some(url);
    }
    let Some((_, raw)) = href.split_once('?') else {
        return Some(url);
    };
    let raw = raw.split('#').next().unwrap_or_default();
    let (bytes, _, _) = encoding.encode(raw);
    let mut encoded = String::with_capacity(bytes.len());
    for byte in bytes.as_ref() {
        if byte.is_ascii() {
            // Every HTML encoding encodes ASCII identically; `set_query`
            // applies the query percent-encode set to what remains.
            encoded.push(*byte as char);
        } else {
            encoded.push('%');
            encoded.push(HEX[(byte >> 4) as usize] as char);
            encoded.push(HEX[(byte & 0xf) as usize] as char);
        }
    }
    url.set_query(Some(&encoded));
    Some(url)
}

const HEX: &[u8; 16] = b"0123456789ABCDEF";

/// `host` serialization: host plus a non-default port, with IPv6 literals in
/// brackets (<https://url.spec.whatwg.org/#concept-url-host>).
fn host(url: &url::Url) -> String {
    // `host_str` strips the brackets; serialization puts them back.
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
        // U+65E5 is 0x93 0xFA in Shift_JIS and 0xE6 0x97 0xA5 in UTF-8.
        let shift_jis =
            encoding_parse_url("x?q=\u{65E5}", &base, encoding_rs::SHIFT_JIS).expect("parse");
        assert_eq!(shift_jis.as_str(), "https://example.com/x?q=%93%FA");
        let utf8 = encoding_parse_url("x?q=\u{65E5}", &base, encoding_rs::UTF_8).expect("parse");
        assert_eq!(utf8.as_str(), "https://example.com/x?q=%E6%97%A5");
    }

    #[test]
    fn a_query_the_href_does_not_name_is_left_alone() {
        let base = url::Url::parse("https://example.com/p?already=1").unwrap();
        // A relative reference with no `?` keeps the base's query when the
        // URL parser inherits it, as a fragment-only reference does.
        let fragment = encoding_parse_url("#top", &base, encoding_rs::SHIFT_JIS).expect("parse");
        assert_eq!(fragment.as_str(), "https://example.com/p?already=1#top");
        // A reference whose own query is empty keeps the `?`.
        let empty = encoding_parse_url("x?", &base, encoding_rs::SHIFT_JIS).expect("parse");
        assert_eq!(empty.as_str(), "https://example.com/x?");
    }
}
