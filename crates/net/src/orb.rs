//! Opaque Response Blocking (ORB).
//!
//! A `no-cors` response to a cross-origin request is *opaque*: page script
//! cannot read it. The engine, though, asks for the unsafe internal response so
//! it can decode an image, compile a script, or parse a sheet. That is exactly
//! the pair a page can abuse: label the dial an image, and read a cross-origin
//! document's body. ORB is the check that decides whether the body may cross
//! that boundary at all
//! (<https://chromium.googlesource.com/chromium/src/+/main/services/network/public/cpp/orb/>,
//! <https://github.com/annevk/orb>).
//!
//! The rules below are Chromium's ORB v0.2 (`orb_impl.cc`, `orb_mimetypes.cc`,
//! `orb_sniffers.cc` at `main`), which is the implementation the web is tested
//! against. Fetch leaves the decision to the user agent; where the draft and
//! Chromium disagree, Chromium is followed because dropping a response the
//! shipped browsers deliver would be a conformance regression of its own.
//!
//! The decision needs at most the first 1024 bytes of the body. [`blocks`]
//! takes them as an option: `None` means the bytes have not arrived yet, which
//! is Chromium's `SniffMore` and allows the response until they do.

/// How many leading bytes ORB sniffs
/// (<https://chromium.googlesource.com/chromium/src/+/main/services/network/public/cpp/orb/orb_impl.cc>, step 5).
pub const SNIFF_LIMIT: usize = 1024;

/// MIME types ORB blocks without sniffing: a body of one of these is not a
/// subresource a `no-cors` request may read, and mislabelling a real image or
/// script as one of them is not something web servers do
/// (<https://chromium.googlesource.com/chromium/src/+/main/services/network/public/cpp/orb/orb_mimetypes.cc>).
const NEVER_SNIFFED: &[&str] = &[
    "application/gzip",
    "application/x-gzip",
    "application/zip",
    "text/event-stream",
    "application/msexcel",
    "application/mspowerpoint",
    "application/msword",
    "application/msword-template",
    "application/pdf",
    "application/vnd.ces-quickpoint",
    "application/vnd.ces-quicksheet",
    "application/vnd.ces-quickword",
    "application/vnd.ms-excel",
    "application/vnd.ms-excel.sheet.macroenabled.12",
    "application/vnd.ms-powerpoint",
    "application/vnd.ms-powerpoint.presentation.macroenabled.12",
    "application/vnd.ms-word",
    "application/vnd.ms-word.document.12",
    "application/vnd.ms-word.document.macroenabled.12",
    "application/vnd.msword",
    "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    "application/vnd.openxmlformats-officedocument.presentationml.template",
    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    "application/vnd.openxmlformats-officedocument.spreadsheetml.template",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.template",
    "application/vnd.presentation-openxml",
    "application/vnd.presentation-openxmlm",
    "application/vnd.spreadsheet-openxml",
    "application/vnd.wordprocessing-openxml",
    "text/csv",
    "text/pdf",
    "application/proto",
    "application/rtf",
    "application/x-7z-compressed",
    "text/calendar",
    "text/rtf",
    "text/tab-separated-values",
    "text/vcard",
    "text/x-vcard",
    "multipart/signed",
    "multipart/byteranges",
];

/// Substrings that make a MIME type never-sniffed: office documents share a
/// prefix, protobuf has several spellings, and YAML has no registered type.
const NEVER_SNIFFED_SUBSTRINGS: &[&str] = &["opendocument", "protobuf", "yaml"];

/// Suffixes that make a MIME type never-sniffed.
const NEVER_SNIFFED_SUFFIXES: &[&str] = &["+proto"];

/// MIME type families Chromium allows without sniffing: a sheet, an SVG (an
/// image, not a document for this decision), and audio/video containers reach
/// their consumer without a body check
/// (<https://chromium.googlesource.com/chromium/src/+/main/services/network/public/cpp/orb/orb_mimetypes.cc>).
fn opaque_safelisted(mime: &str) -> bool {
    mime == "text/css"
        || mime == "image/svg+xml"
        || mime.starts_with("audio/")
        || mime.starts_with("video/")
        || matches!(
            mime,
            "application/ogg"
                | "application/dash+xml"
                | "application/vnd.apple.mpegurl"
                | "text/vtt"
        )
}

/// The parts of a MIME type ORB distinguishes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Kind {
    /// `text/html`.
    Html,
    /// `application/json`, `text/json`, `+json`, and the protobuf spelling.
    Json,
    /// `application/xml`, `text/xml`, `+xml`, except the image and DASH types.
    Xml,
    /// `text/plain`.
    Plain,
    /// A response body no image, script, or sheet could be.
    NeverSniffed,
    /// Anything else, including JavaScript types, which are sniffed anyway.
    Other,
}

/// Classifies one MIME type, lowercased and without parameters
/// (<https://chromium.googlesource.com/chromium/src/+/main/services/network/public/cpp/orb/orb_mimetypes.cc>, `GetCanonicalMimeType`).
fn kind(mime: &str) -> Kind {
    // The image and DASH types end in `+xml` but are not XML documents for
    // this decision.
    if mime == "image/svg+xml" || mime == "application/dash+xml" {
        return Kind::Other;
    }
    if mime == "text/html" {
        return Kind::Html;
    }
    if mime == "application/json"
        || mime == "text/json"
        || mime == "application/json+protobuf"
        || mime.ends_with("+json")
    {
        return Kind::Json;
    }
    if mime == "application/xml" || mime == "text/xml" || mime.ends_with("+xml") {
        return Kind::Xml;
    }
    if mime == "text/plain" {
        return Kind::Plain;
    }
    if never_sniffed(mime) {
        return Kind::NeverSniffed;
    }
    Kind::Other
}

fn never_sniffed(mime: &str) -> bool {
    NEVER_SNIFFED.contains(&mime)
        || NEVER_SNIFFED_SUBSTRINGS
            .iter()
            .any(|needle| mime.contains(needle))
        || NEVER_SNIFFED_SUFFIXES
            .iter()
            .any(|suffix| mime.ends_with(suffix))
}

/// The MIME type's essence: lowercased, with parameters removed.
fn essence(mime_type: &str) -> String {
    mime_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
}

/// Whether an engine-facing opaque response must not deliver its body.
///
/// `mime_type` is the response's declared type, `nosniff` is whether the
/// response carried `X-Content-Type-Options: nosniff`, and `body` is the first
/// [`SNIFF_LIMIT`] bytes (or the whole body when it is shorter). `None` means
/// no bytes are available yet.
///
/// A blocked response is delivered as an empty body with no headers, which is
/// what Chromium's `kEmptyResponse` handling does for a destination that asked
/// for a subresource.
#[must_use]
pub fn blocks(mime_type: Option<&str>, nosniff: bool, body: Option<&[u8]>) -> bool {
    // Step 1: no MIME type is not a failure to block on. Chromium allows it:
    // a server that omits the type has not said the body is a document, and
    // blocking would take out every untyped image on the web.
    let Some(mime_type) = mime_type else {
        return false;
    };
    let essence = essence(mime_type);
    if essence.is_empty() {
        return false;
    }
    // Step 3.i: an opaque-safelisted type is allowed before any sniffing.
    if opaque_safelisted(&essence) {
        return false;
    }
    match kind(&essence) {
        // Step 3.ii: never-sniffed types are blocked outright.
        Kind::NeverSniffed => return true,
        // Step 3.iv: with `nosniff`, an HTML/JSON/XML/text type is blocked
        // without reading the body. Without it, a server may have mislabelled
        // an image or script, so the body decides.
        Kind::Html | Kind::Json | Kind::Xml | Kind::Plain => {
            if nosniff {
                return true;
            }
        }
        Kind::Other => {}
    }
    // Step 5: wait for the first bytes. Until they arrive, allow.
    let Some(bytes) = body else {
        return false;
    };
    let bytes = &bytes[..bytes.len().min(SNIFF_LIMIT)];
    // Step 7/9: media and images are allowed whatever the declared type says.
    if sniff_media(bytes) {
        return false;
    }
    // Steps 12-16: a body that sniffs as HTML, XML, or JSON (or carries an
    // XSSI parser breaker) is a document, not a subresource. JavaScript MIME
    // types arrive here too, which is why a script that starts with `[` is
    // still allowed: only JSON's `{"..." :` shape and the parser breakers
    // match.
    if sniff_html(bytes) || sniff_xml(bytes) || sniff_fetch_only(bytes) {
        return true;
    }
    false
}

/// The ASCII whitespace ORB skips.
const WHITESPACE: &[u8] = b"\t\n\x0c\r ";

/// Skips a UTF-8 byte-order mark.
fn trim_bom(data: &[u8]) -> &[u8] {
    data.strip_prefix(b"\xef\xbb\xbf").unwrap_or(data)
}

/// Skips ASCII whitespace.
fn trim_whitespace(data: &[u8]) -> &[u8] {
    let start = data.iter().position(|byte| !WHITESPACE.contains(byte));
    start.map_or(&[], |start| &data[start..])
}

/// Whether `data` starts with `prefix`, ASCII case-insensitively.
fn starts_with_ignore_case(data: &[u8], prefix: &[u8]) -> bool {
    data.len() >= prefix.len() && data[..prefix.len()].eq_ignore_ascii_case(prefix)
}

/// The HTML signatures ORB matches after whitespace and comments
/// (<https://chromium.googlesource.com/chromium/src/+/main/services/network/public/cpp/orb/orb_sniffers.cc>, `SniffForHTML`).
///
/// `<!--` is deliberately absent: it opens a JavaScript comment too, and the
/// comment skipper below handles it.
const HTML_SIGNATURES: &[&[u8]] = &[
    b"<!doctype html",
    b"<script",
    b"<html",
    b"<head",
    b"<iframe",
    b"<h1",
    b"<div",
    b"<font",
    b"<table",
    b"<a",
    b"<style",
    b"<title",
    b"<b",
    b"<p",
];

/// Whether the body begins with HTML markup, skipping leading whitespace and
/// well-formed HTML comments.
fn sniff_html(data: &[u8]) -> bool {
    let mut data = trim_bom(data);
    loop {
        data = trim_whitespace(data);
        if data.is_empty() {
            // Entirely whitespace so far: not a decision.
            return false;
        }
        if HTML_SIGNATURES
            .iter()
            .any(|signature| starts_with_ignore_case(data, signature))
        {
            return true;
        }
        // An HTML comment at the start is also a JavaScript comment. Skip a
        // terminated one and the rest of its line, which is how an HTML/JS
        // polyglot hides its markup from the JS parser
        // (<https://crbug.com/839945>). An unterminated one decides nothing.
        let Some(comment) = skip_html_comment(data) else {
            return false;
        };
        data = comment;
    }
}

/// Skips one terminated `<!-- ... -->` comment plus its line, or returns
/// `None` when there is no terminated comment at the start.
fn skip_html_comment(data: &[u8]) -> Option<&[u8]> {
    let rest = data.strip_prefix(b"<!--")?;
    let end = find(rest, b"-->")? + 3;
    let after = &rest[end..];
    Some(match after_line_end(after) {
        Some(next) => &after[next..],
        None => &[],
    })
}

/// The index immediately after the first LF, CR, or JavaScript line separator
/// (U+2028 / U+2029).
fn after_line_end(data: &[u8]) -> Option<usize> {
    for (index, &byte) in data.iter().enumerate() {
        if byte == b'\n' || byte == b'\r' {
            return Some(index + 1);
        }
        if byte == 0xe2
            && data
                .get(index + 1..index + 3)
                .is_some_and(|tail| tail == [0x80, 0xa8] || tail == [0x80, 0xa9])
        {
            return Some(index + 3);
        }
    }
    None
}

/// The first index of `needle` in `haystack`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Whether the body begins with an XML declaration.
fn sniff_xml(data: &[u8]) -> bool {
    trim_whitespace(trim_bom(data)).starts_with(b"<?xml")
}

/// The prefixes that break a response as a script: XSSI sentinels and
/// infinite loops, which only a fetch/XHR consumer is meant to see
/// (<https://chromium.googlesource.com/chromium/src/+/main/services/network/public/cpp/orb/orb_sniffers.cc>, `SniffForFetchOnlyResource`).
const SCRIPT_BREAKING_PREFIXES: &[&[u8]] = &[
    b")]}'",
    b"{}&&",
    b"{} &&",
    b"for(;;);",
    b"while(1);",
    b"for (;;);",
    b"while (1);",
];

/// Whether the body is a fetch-only resource: an XSSI parser breaker, or JSON.
fn sniff_fetch_only(data: &[u8]) -> bool {
    let data = trim_bom(data);
    if SCRIPT_BREAKING_PREFIXES
        .iter()
        .any(|prefix| data.starts_with(prefix))
    {
        return true;
    }
    sniff_json(data)
}

/// Whether the body starts with a JSON object: `{` then a quoted string then
/// `:`. That shape is a JavaScript syntax error, so a script cannot start with
/// it, while a JSON document always does
/// (<https://chromium.googlesource.com/chromium/src/+/main/services/network/public/cpp/orb/orb_sniffers.cc>, `SniffForJSON`).
fn sniff_json(data: &[u8]) -> bool {
    #[derive(Clone, Copy, Eq, PartialEq)]
    enum State {
        Start,
        LeftBrace,
        LeftQuote,
        Escape,
        RightQuote,
    }

    let mut state = State::Start;
    for &byte in trim_bom(data) {
        if !matches!(state, State::LeftQuote | State::Escape) && WHITESPACE.contains(&byte) {
            continue;
        }
        match state {
            State::Start => {
                if byte != b'{' {
                    return false;
                }
                state = State::LeftBrace;
            }
            State::LeftBrace => {
                if byte != b'"' {
                    return false;
                }
                state = State::LeftQuote;
            }
            State::LeftQuote => {
                if byte == b'"' {
                    state = State::RightQuote;
                } else if byte == b'\\' {
                    state = State::Escape;
                }
            }
            State::Escape => state = State::LeftQuote,
            State::RightQuote => return byte == b':',
        }
    }
    // The body ended before the shape was decided: allow, as Chromium's
    // `kMaybe` does once the body is complete.
    false
}

/// Whether the body begins with a media container, which ORB allows whatever
/// the declared type was. Covers the formats a mislabelling server actually
/// serves: GIF, PNG, JPEG, WebP, BMP, ICO, RIFF (WebP/WAVE/AVI), Ogg, MP3,
/// MP4/QuickTime, WebM/Matroska, MPEG-TS, FLAC, and WOFF fonts (which the
/// renderer also fetches `no-cors`).
fn sniff_media(data: &[u8]) -> bool {
    const SIGNATURES: &[&[u8]] = &[
        b"GIF87a",
        b"GIF89a",
        b"\x89PNG\r\n\x1a\n",
        b"\xff\xd8\xff",
        b"RIFF",
        b"BM",
        b"\x00\x00\x01\x00",
        b"OggS",
        b"ID3",
        b"\xff\xfb",
        b"\xff\xf3",
        b"\xff\xf2",
        b"fLaC",
        b"\x1a\x45\xdf\xa3",
        b"wOFF",
        b"wOF2",
        b"ftyp",
        b"\x47\x40",
        b"\x00\x00\x01\xba",
        b"\x00\x00\x01\xb3",
    ];
    if SIGNATURES
        .iter()
        .any(|signature| data.starts_with(signature))
    {
        return true;
    }
    // WebP is `RIFF` + four size bytes + `WEBP`; the RIFF signature above is
    // enough for the decision (a WAV or AVI is media too).
    false
}

#[cfg(test)]
mod tests {
    use super::{Kind, blocks, kind};

    #[test]
    fn never_sniffed_types_are_blocked_without_a_body() {
        assert!(blocks(Some("application/zip"), false, None));
        assert!(blocks(Some("application/pdf"), false, None));
        assert!(blocks(Some("text/csv"), false, None));
        assert!(blocks(
            Some("application/vnd.oasis.opendocument.text"),
            false,
            None
        ));
        assert!(!blocks(Some("image/png"), false, None));
    }

    #[test]
    fn an_omitted_type_is_allowed() {
        assert!(!blocks(None, false, Some(b"<!doctype html><p>x")));
    }

    #[test]
    fn safelisted_types_skip_sniffing() {
        assert!(!blocks(Some("text/css"), true, Some(b"<!doctype html>")));
        assert!(!blocks(
            Some("image/svg+xml"),
            true,
            Some(b"<?xml version=1?>")
        ));
        assert!(!blocks(Some("audio/mpeg"), true, None));
        assert!(!blocks(Some("video/mp4"), true, None));
    }

    #[test]
    fn nosniff_blocks_a_document_type_without_reading_the_body() {
        assert!(blocks(Some("text/html"), true, None));
        assert!(blocks(Some("application/json"), true, None));
        assert!(blocks(Some("text/plain"), true, None));
        assert!(blocks(Some("application/xhtml+xml"), true, None));
    }

    #[test]
    fn without_nosniff_the_body_decides() {
        // A real image mislabelled as HTML is still an image.
        assert!(!blocks(
            Some("text/html"),
            false,
            Some(b"\x89PNG\r\n\x1a\n....")
        ));
        // A document labelled as an image is a document.
        assert!(blocks(
            Some("image/png"),
            false,
            Some(b"<!doctype html><script>alert(1)</script>")
        ));
        assert!(blocks(Some("image/png"), false, Some(b"  <html>")));
        assert!(blocks(
            Some("image/png"),
            false,
            Some(b"<?xml version=\"1.0\"?>")
        ));
        assert!(blocks(Some("image/png"), false, Some(b"{\"a\": 1}")));
        assert!(blocks(Some("image/png"), false, Some(b")]}'\n[1]")));
        // A plain image or an unrecognisable body is allowed.
        assert!(!blocks(
            Some("application/octet-stream"),
            false,
            Some(b"just text")
        ));
    }

    #[test]
    fn an_html_comment_at_the_start_does_not_hide_markup_for_long() {
        assert!(blocks(
            Some("text/plain"),
            false,
            Some(b"<!-- c -->\n<script>alert(1)</script>")
        ));
        // U+2028 (E2 80 A8) is a real JS line terminator, whereas an em-dash
        // U+2014 (E2 80 94) shares the 0xE2 lead byte and must not end the
        // comment line (mutation: checking `byte == 0xe2 && data.len() > 2`
        // treats the em-dash as a line break and mis-sniffs the rest of the
        // comment line).
        assert!(blocks(
            Some("text/plain"),
            false,
            Some("<!-- c -->\u{2028}<script>alert(1)</script>".as_bytes())
        ));
        assert!(!blocks(
            Some("text/plain"),
            false,
            Some("<!-- c -->\u{2014}<script>alert(1)</script>".as_bytes())
        ));
    }

    #[test]
    fn javascript_mime_types_are_sniffed_but_not_blocked_as_json_lists() {
        assert!(!blocks(Some("text/javascript"), false, Some(b"[1, 2, 3]")));
        assert!(!blocks(Some("text/javascript"), false, Some(b"var x = 1;")));
    }

    #[test]
    fn the_mime_classifier_matches_the_reference_table() {
        assert_eq!(kind("text/html"), Kind::Html);
        assert_eq!(kind("application/json"), Kind::Json);
        assert_eq!(kind("application/ld+json"), Kind::Json);
        assert_eq!(kind("image/svg+xml"), Kind::Other);
        assert_eq!(kind("application/dash+xml"), Kind::Other);
        assert_eq!(kind("application/xhtml+xml"), Kind::Xml);
        assert_eq!(kind("text/plain"), Kind::Plain);
        assert_eq!(kind("application/grpc-web+proto"), Kind::NeverSniffed);
        assert_eq!(kind("application/x-yaml"), Kind::NeverSniffed);
        assert_eq!(kind("text/javascript"), Kind::Other);
    }
}
