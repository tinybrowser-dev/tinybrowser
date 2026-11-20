// https://chromium.googlesource.com/chromium/src/+/main/services/network/public/cpp/orb/
// https://github.com/annevk/orb — follows Chromium ORB v0.2 where the draft and Chromium disagree

// https://chromium.googlesource.com/chromium/src/+/main/services/network/public/cpp/orb/orb_impl.cc
pub const SNIFF_LIMIT: usize = 1024;

// https://chromium.googlesource.com/chromium/src/+/main/services/network/public/cpp/orb/orb_mimetypes.cc
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

const NEVER_SNIFFED_SUBSTRINGS: &[&str] = &["opendocument", "protobuf", "yaml"];

const NEVER_SNIFFED_SUFFIXES: &[&str] = &["+proto"];

// https://chromium.googlesource.com/chromium/src/+/main/services/network/public/cpp/orb/orb_mimetypes.cc
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Kind {
    Html,
    Json,
    Xml,
    Plain,
    NeverSniffed,
    Other,
}

// https://chromium.googlesource.com/chromium/src/+/main/services/network/public/cpp/orb/orb_mimetypes.cc
fn kind(mime: &str) -> Kind {
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

fn essence(mime_type: &str) -> String {
    mime_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
}

#[must_use]
pub fn blocks(mime_type: Option<&str>, nosniff: bool, body: Option<&[u8]>) -> bool {
    let Some(mime_type) = mime_type else {
        return false;
    };
    let essence = essence(mime_type);
    if essence.is_empty() {
        return false;
    }
    if opaque_safelisted(&essence) {
        return false;
    }
    match kind(&essence) {
        Kind::NeverSniffed => return true,
        Kind::Html | Kind::Json | Kind::Xml | Kind::Plain => {
            if nosniff {
                return true;
            }
        }
        Kind::Other => {}
    }
    let Some(bytes) = body else {
        return false;
    };
    let bytes = &bytes[..bytes.len().min(SNIFF_LIMIT)];
    if sniff_media(bytes) {
        return false;
    }
    if sniff_html(bytes) || sniff_xml(bytes) || sniff_fetch_only(bytes) {
        return true;
    }
    false
}

const WHITESPACE: &[u8] = b"\t\n\x0c\r ";

fn trim_bom(data: &[u8]) -> &[u8] {
    data.strip_prefix(b"\xef\xbb\xbf").unwrap_or(data)
}

fn trim_whitespace(data: &[u8]) -> &[u8] {
    let start = data.iter().position(|byte| !WHITESPACE.contains(byte));
    start.map_or(&[], |start| &data[start..])
}

fn starts_with_ignore_case(data: &[u8], prefix: &[u8]) -> bool {
    data.len() >= prefix.len() && data[..prefix.len()].eq_ignore_ascii_case(prefix)
}

// https://chromium.googlesource.com/chromium/src/+/main/services/network/public/cpp/orb/orb_sniffers.cc
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

fn sniff_html(data: &[u8]) -> bool {
    let mut data = trim_bom(data);
    loop {
        data = trim_whitespace(data);
        if data.is_empty() {
            return false;
        }
        if HTML_SIGNATURES
            .iter()
            .any(|signature| starts_with_ignore_case(data, signature))
        {
            return true;
        }
        // https://crbug.com/839945
        let Some(comment) = skip_html_comment(data) else {
            return false;
        };
        data = comment;
    }
}

fn skip_html_comment(data: &[u8]) -> Option<&[u8]> {
    let rest = data.strip_prefix(b"<!--")?;
    let end = find(rest, b"-->")? + 3;
    let after = &rest[end..];
    Some(match after_line_end(after) {
        Some(next) => &after[next..],
        None => &[],
    })
}

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

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn sniff_xml(data: &[u8]) -> bool {
    trim_whitespace(trim_bom(data)).starts_with(b"<?xml")
}

// https://chromium.googlesource.com/chromium/src/+/main/services/network/public/cpp/orb/orb_sniffers.cc
const SCRIPT_BREAKING_PREFIXES: &[&[u8]] = &[
    b")]}'",
    b"{}&&",
    b"{} &&",
    b"for(;;);",
    b"while(1);",
    b"for (;;);",
    b"while (1);",
];

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

// https://chromium.googlesource.com/chromium/src/+/main/services/network/public/cpp/orb/orb_sniffers.cc
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
    false
}

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
    SIGNATURES
        .iter()
        .any(|signature| data.starts_with(signature))
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
        assert!(!blocks(
            Some("text/html"),
            false,
            Some(b"\x89PNG\r\n\x1a\n....")
        ));
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
