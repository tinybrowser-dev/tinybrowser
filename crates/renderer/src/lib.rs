//! Page engine for one document: Blitz parsing and trees plus `QuickJS`
//! behind a value-only browser interface.
//!
//! The engine owns the frame actor (`document::Document`) and never links `net`.
//! The browser process owns the tab, navigation, the network, and cookies.
//! Style, layout, and paint live in the `render` module via Blitz: one-shot
//! screenshot and geometry through `blitz-dom`/`blitz-paint` and the
//! `anyrender_tiny_skia` backend. A carrier drives this crate: the browser's
//! child transport on a native build, the WebAssembly component on a wasm
//! build. Both feed the same [`Engine`], and both implement [`BrowserServices`]
//! to supply effects.

use markup5ever::interface::QuirksMode;

use crate::documents::BlitzDocument;

mod csp;
pub use csp::{RedirectGate, integrity_verifies};
mod css;
mod document;
mod documents;
mod dom;
pub mod dom_string;
mod embedded;
mod engine;
mod js;
mod messaging;
pub(crate) mod names;
mod protocol;
mod remote;
mod render;
pub mod sniff;
mod storage;
#[cfg(test)]
mod test_services;
mod window_features;
mod worker;
mod xml;

pub use embedded::EmbeddedRenderer;
pub use engine::{DEFAULT_VIEWPORT, Engine, decode_data_url, decode_data_url_bounded};
pub use render::RgbaImage;

/// A system font database engines on one thread can share.
///
/// Building one walks the installed fonts (tens of milliseconds on every
/// engine construction); cloning it is a reference-count bump. A harness that
/// mounts thousands of documents should build one per worker and hand a clone
/// to every engine it creates.
#[derive(Clone, Default)]
pub struct Fonts(parley::FontContext);
pub use js::{Stop, TURN_SCRIPT_BUDGET};
pub use protocol::{
    BodyDelivery, BrowserEvent, BrowserServices, BrowsingContextHost, ContextName, CorsMode,
    CorsRequest, DialCancellation, DialCompletion, DialFailure, DialKind, DialOutcome, DialRequest,
    DomBoxModel, DomNode, DomRemoteNode, DomReply, DomRequest, FrameId, FrameKind, HistoryHandling,
    HistorySnapshot, HttpHead, MAX_RESPONSE_BODY_BYTES, MessagingHost, Mount, NameChange,
    NavigationSource, NetworkHost, OpenerPolicy, PolicyContainer, PostedMessage, ReferrerRequest,
    RendererEvent, RequestCache, RequestPolicy, ResourceLimit, ResponseBody, ResponseHead,
    ResponseSource, STORAGE_QUOTA_BYTES, SandboxFlags, ScreenshotClip, ScreenshotRequest,
    ScriptFailure, ScriptSource, SerializedOrigin, StorageChange, StorageError, StorageHost,
    StorageKey, StorageKind, StreamId, TabError, TabState, TargetName, TargetOrigin, TimerId,
    WindowId,
};
pub use remote::RemoteValue;
pub use storage::PendingStorageEvent;

/// The current document readiness
/// (<https://html.spec.whatwg.org/multipage/dom.html#current-document-readiness>).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReadyState {
    /// The parser is still running.
    Loading,
    /// Parsing finished; `DOMContentLoaded` has fired.
    Interactive,
    /// The document is completely loaded; the `load` event has fired.
    Complete,
}

/// The result of parsing one document.
pub(crate) struct Parsed {
    /// The store id, assigned on insert; `0` means unassigned.
    pub id: u32,
    /// The parsed tree plus its mutation journal.
    pub document: BlitzDocument,
    /// Compatibility mode selected by the doctype (or its absence).
    pub quirks_mode: QuirksMode,
    /// MIME type this document reports from `document.contentType`.
    pub content_type: &'static str,
    /// Whether this object implements `XMLDocument`
    /// (<https://dom.spec.whatwg.org/#xmldocument>).
    ///
    /// `new Document()` and `DOMParser` create XML documents that are still
    /// plain `Document`s
    /// (<https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-domparser-parsefromstring>).
    /// Navigated XML, `new XMLDocument()`, and `createDocument()` use
    /// `XMLDocument`.
    pub xml_document: bool,
    /// The document's readiness; parsed documents start at [`ReadyState::Loading`]
    /// while documents created by script start complete
    /// (<https://html.spec.whatwg.org/multipage/dom.html#current-document-readiness>).
    pub ready_state: ReadyState,
    /// The document's URL when it differs from the world's active document,
    /// such as a `DOMParser` result
    /// (<https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-domparser-parsefromstring>).
    pub url: Option<String>,
    /// The encoding name `document.characterSet` reports
    /// (<https://encoding.spec.whatwg.org/#dom-document-characterset>).
    pub character_set: &'static str,
}

impl Parsed {
    /// An empty script-created document: never rendered, so no UA sheets and
    /// no providers; shares the process font context for speed (a fresh
    /// scan costs ~25ms per document).
    pub(crate) fn script(content_type: &'static str, font_ctx: parley::FontContext) -> Self {
        let config = blitz_dom::DocumentConfig {
            font_ctx: Some(font_ctx),
            ua_stylesheets: Vec::new(),
            base_url: Some(crate::render::INVALID_BASE_URL.to_owned()),
            ..blitz_dom::DocumentConfig::default()
        };
        Self {
            id: 0,
            document: BlitzDocument::new(config),
            quirks_mode: QuirksMode::NoQuirks,
            content_type,
            xml_document: content_type != "text/html",
            ready_state: ReadyState::Complete,
            url: None,
            character_set: "UTF-8",
        }
    }
}

/// Parses a full HTML document into a fresh tree.
///
/// Broken markup recovery is html5ever's job (through `blitz-html`).
/// Script interleaving runs in tree order in `document`.
/// Scripting is enabled, so `noscript` parses as text. The sniffed quirks
/// mode is locked into the style engine before parsing, because html5ever
/// treats an empty system identifier differently from
/// [`sniff_quirks_mode`], and `document.compatMode` follows the sniff
/// (<https://html.spec.whatwg.org/multipage/parsing.html#the-initial-insertion-mode>).
#[must_use]
pub(crate) fn parse_html(input: &str, mut config: blitz_dom::DocumentConfig) -> Parsed {
    let quirks_mode = sniff_quirks_mode(input);
    config.quirks_mode = Some(style_quirks(quirks_mode));
    let base: blitz_dom::BaseDocument = blitz_html::HtmlDocument::from_html(input, config).into();
    let document = BlitzDocument::from_base(base);
    // The sink appends the doctype as a real document child now. HTML never
    // produces PI nodes (`<?...?>` is a bogus comment); the tree derives a
    // processing instruction's pseudo-attributes when it creates the node.
    let mut parsed = Parsed {
        id: 0,
        document,
        quirks_mode,
        content_type: "text/html",
        xml_document: false,
        ready_state: ReadyState::Loading,
        url: None,
        character_set: "UTF-8",
    };
    crate::dom::forms::initialize_controls(&mut parsed);
    parsed
}

fn style_quirks(mode: QuirksMode) -> blitz_dom::StyleQuirksMode {
    match mode {
        QuirksMode::Quirks => blitz_dom::StyleQuirksMode::Quirks,
        QuirksMode::LimitedQuirks => blitz_dom::StyleQuirksMode::LimitedQuirks,
        QuirksMode::NoQuirks => blitz_dom::StyleQuirksMode::NoQuirks,
    }
}

// Quirks-mode public-identifier prefixes, ASCII case-insensitive
// (lowercase here; `sniff_quirks_mode` lowercases the public identifier).
const QUIRKS_PREFIXES: &[&str] = &[
    "+//silmaril//dtd html pro v0r11 19970101//",
    "-//advasoft ltd//dtd html 3.0 aswedit + extensions//",
    "-//as//dtd html 3.0 aswedit + extensions//",
    "-//ietf//dtd html 2.0 level 1//",
    "-//ietf//dtd html 2.0 level 2//",
    "-//ietf//dtd html 2.0 strict level 1//",
    "-//ietf//dtd html 2.0 strict level 2//",
    "-//ietf//dtd html 2.0 strict//",
    "-//ietf//dtd html 2.0//",
    "-//ietf//dtd html 2.1e//",
    "-//ietf//dtd html 3.0//",
    "-//ietf//dtd html 3.2 final//",
    "-//ietf//dtd html 3.2//",
    "-//ietf//dtd html 3//",
    "-//ietf//dtd html level 0//",
    "-//ietf//dtd html level 1//",
    "-//ietf//dtd html level 2//",
    "-//ietf//dtd html level 3//",
    "-//ietf//dtd html strict level 0//",
    "-//ietf//dtd html strict level 1//",
    "-//ietf//dtd html strict level 2//",
    "-//ietf//dtd html strict level 3//",
    "-//ietf//dtd html strict//",
    "-//ietf//dtd html//",
    "-//metrius//dtd metrius presentational//",
    "-//microsoft//dtd internet explorer 2.0 html strict//",
    "-//microsoft//dtd internet explorer 2.0 html//",
    "-//microsoft//dtd internet explorer 2.0 tables//",
    "-//microsoft//dtd internet explorer 3.0 html strict//",
    "-//microsoft//dtd internet explorer 3.0 html//",
    "-//microsoft//dtd internet explorer 3.0 tables//",
    "-//netscape comm. corp.//dtd html//",
    "-//netscape comm. corp.//dtd strict html//",
    "-//o'reilly and associates//dtd html 2.0//",
    "-//o'reilly and associates//dtd html extended 1.0//",
    "-//o'reilly and associates//dtd html extended relaxed 1.0//",
    "-//softquad software//dtd hotmetal pro 6.0::19990601::extensions to html 4.0//",
    "-//softquad//dtd hotmetal pro 4.0::19971010::extensions to html 4.0//",
    "-//spyglass//dtd html 2.0 extended//",
    "-//sq//dtd html 2.0 hotmetal + extensions//",
    "-//sun microsystems corp.//dtd hotjava html//",
    "-//sun microsystems corp.//dtd hotjava strict html//",
    "-//w3c//dtd html 3 1995-03-24//",
    "-//w3c//dtd html 3.2 draft//",
    "-//w3c//dtd html 3.2 final//",
    "-//w3c//dtd html 3.2//",
    "-//w3c//dtd html 3.2s draft//",
    "-//w3c//dtd html 4.0 frameset//",
    "-//w3c//dtd html 4.0 transitional//",
    "-//w3c//dtd html experimental 19960712//",
    "-//w3c//dtd html experimental 970421//",
    "-//w3c//dtd w3 html//",
    "-//w3o//dtd w3 html 3.0//",
    "-//webtechs//dtd mozilla html 2.0//",
    "-//webtechs//dtd mozilla html//",
];
// Quirks-mode public identifiers matched exactly (ASCII
// case-insensitive), not by prefix: `PUBLIC "HTML5"` is standards.
const QUIRKS_EXACT: &[&str] = &[
    "-//w3o//dtd w3 html strict 3.0//en//",
    "-/w3c/dtd html 4.0 transitional/en",
    "html",
];
// Limited-quirks prefixes regardless of the system identifier.
const LIMITED_PREFIXES: &[&str] = &[
    "-//w3c//dtd xhtml 1.0 frameset//",
    "-//w3c//dtd xhtml 1.0 transitional//",
];
// HTML 4.01 frameset/transitional: quirks when the system identifier is
// missing or empty, limited quirks otherwise.
const HTML4_PREFIXES: &[&str] = &[
    "-//w3c//dtd html 4.01 frameset//",
    "-//w3c//dtd html 4.01 transitional//",
];

/// Compatibility mode from the doctype
/// (<https://html.spec.whatwg.org/multipage/parsing.html#the-initial-insertion-mode>,
/// quirks-mode table): a missing doctype, a non-`html` name, or a
/// force-quirks token means quirks. Otherwise the public identifier selects
/// quirks by prefix or exact match, the IBM system identifier selects
/// quirks, the XHTML frameset/transitional prefixes select limited quirks
/// regardless of the system identifier, and the HTML 4.01 frameset/
/// transitional prefixes select quirks when the system identifier is missing
/// or empty and limited quirks otherwise.
/// Everything else means no quirks.
///
/// html5ever's `doctype_error_and_quirks` still treats an empty system
/// identifier as present (limited quirks for HTML 4.01); Chromium's
/// `HTMLConstructionSite::SetCompatibilityModeFromDoctype` tests
/// `system_id.empty()` and matches the spec, so this follows the spec.
fn sniff_quirks_mode(input: &str) -> QuirksMode {
    let Some((name, public, system)) = parse_doctype_ids(input) else {
        return QuirksMode::Quirks;
    };
    if !name.eq_ignore_ascii_case("html") {
        return QuirksMode::Quirks;
    }
    let public = public.to_ascii_lowercase();
    if QUIRKS_PREFIXES
        .iter()
        .any(|prefix| public.starts_with(prefix))
        || QUIRKS_EXACT.contains(&public.as_str())
    {
        return QuirksMode::Quirks;
    }
    if system.as_deref().is_some_and(|system| {
        system.eq_ignore_ascii_case("http://www.ibm.com/data/dtd/v11/ibmxhtml1-transitional.dtd")
    }) {
        return QuirksMode::Quirks;
    }
    if LIMITED_PREFIXES
        .iter()
        .any(|prefix| public.starts_with(prefix))
    {
        return QuirksMode::LimitedQuirks;
    }
    if HTML4_PREFIXES
        .iter()
        .any(|prefix| public.starts_with(prefix))
    {
        if system.as_deref().is_none_or(str::is_empty) {
            return QuirksMode::Quirks;
        }
        return QuirksMode::LimitedQuirks;
    }
    QuirksMode::NoQuirks
}

/// The doctype name and public/system identifiers from leading markup, if a
/// complete `<!DOCTYPE ...>` token is present. Whitespace and closed
/// comments may precede it; quoted identifiers keep their quotes stripped.
/// Anything malformed bails to `None` (quirks): a `>` inside a quoted
/// identifier (abrupt-doctype-identifier), a quoted identifier with no
/// `PUBLIC`/`SYSTEM` keyword (bogus doctype), or a truncated token.
fn parse_doctype_ids(input: &str) -> Option<(String, String, Option<String>)> {
    let rest = skip_ignored_prefix(input);
    if rest.len() < 9
        || !rest
            .get(..9)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("<!doctype"))
    {
        return None;
    }
    let mut chars = rest[9..].chars().peekable();
    while chars.peek().is_some_and(char::is_ascii_whitespace) {
        chars.next();
    }
    let mut name = String::new();
    while let Some(&char) = chars.peek() {
        if char.is_ascii_whitespace() || char == '>' {
            break;
        }
        name.push(char);
        chars.next();
    }
    if name.is_empty() {
        return None;
    }
    while chars.peek().is_some_and(char::is_ascii_whitespace) {
        chars.next();
    }
    let mut word = String::new();
    while let Some(&char) = chars.peek() {
        if char.is_ascii_whitespace() || char == '>' || char == '"' || char == '\'' {
            break;
        }
        word.push(char);
        chars.next();
    }
    let (mut public, mut system) = (String::new(), None);
    if word.eq_ignore_ascii_case("public") {
        public = quoted_string(&mut chars)?;
        // After `PUBLIC "..."`, only whitespace and `>` may follow without
        // a system identifier; anything else must be a quoted identifier
        // (a missing quote is a missing-quote parse error, which sets
        // force-quirks).
        while chars.peek().is_some_and(char::is_ascii_whitespace) {
            chars.next();
        }
        if chars.peek() != Some(&'>') {
            system = Some(quoted_string(&mut chars)?);
        }
    } else if word.eq_ignore_ascii_case("system") {
        // A `SYSTEM` keyword requires a quoted identifier; its absence is a
        // missing-doctype-system-identifier parse error (force-quirks).
        system = Some(quoted_string(&mut chars)?);
    } else if !word.is_empty() {
        return None;
    } else if matches!(chars.peek(), Some('"' | '\'')) {
        // A quoted identifier with no keyword is a bogus doctype, not an
        // empty public identifier.
        return None;
    }
    // The token must terminate: truncated markup is not a doctype.
    if !chars.any(|char| char == '>') {
        return None;
    }
    Some((name, public, system))
}

/// Leading whitespace, BOM, and closed comments, which may precede the
/// doctype without forcing quirks. Abruptly-closed comments (`<!-->`,
/// `<!--->`) and `--!>`-closed comments are closed comments too, so
/// scanning continues past them; an unterminated comment stops the scan so
/// the doctype check fails.
fn skip_ignored_prefix(mut input: &str) -> &str {
    loop {
        let trimmed = input.trim_start_matches(['\u{feff}', ' ', '\t', '\n', '\x0c', '\r']);
        let Some(comment) = trimmed.strip_prefix("<!--") else {
            return trimmed;
        };
        // Abruptly-closed comments (`<!-->`, `<!--->`) close at the first
        // `>`: check them before the longer terminators, then take the
        // earliest `-->`/`--!>` close. A later terminator never wins over
        // character data produced by an earlier close.
        if let Some(rest) = comment
            .strip_prefix('>')
            .or_else(|| comment.strip_prefix("->"))
        {
            input = rest;
        } else if let Some(end) = [("-->", 3), ("--!>", 4)]
            .iter()
            .filter_map(|(marker, len)| comment.find(marker).map(|index| index + len))
            .min()
        {
            input = &comment[end..];
        } else {
            return trimmed;
        }
    }
}

/// One single- or double-quoted string after optional whitespace, or `None`
/// when the next token is not quoted.
fn quoted_string(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Option<String> {
    while chars.peek().is_some_and(char::is_ascii_whitespace) {
        chars.next();
    }
    let quote = *chars.peek()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    chars.next();
    let mut value = String::new();
    for char in chars.by_ref() {
        if char == quote {
            return Some(value);
        }
        // A `>` inside the identifier is an abrupt-doctype-identifier parse
        // error, which sets force-quirks.
        if char == '>' {
            return None;
        }
        value.push(char);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{QuirksMode, sniff_quirks_mode};

    // The quirks table: quirks and limited-quirks public identifiers with
    // and without system identifiers
    // (<https://html.spec.whatwg.org/multipage/parsing.html#the-initial-insertion-mode>).
    #[test]
    fn quirks_sniff_follows_the_spec_table() {
        let cases = [
            ("<!DOCTYPE html>", QuirksMode::NoQuirks),
            ("<!doctype html>", QuirksMode::NoQuirks),
            // XHTML transitional/frameset: limited quirks with or without a
            // system identifier.
            (
                "<!DOCTYPE html PUBLIC \"-//W3C//DTD XHTML 1.0 Transitional//EN\" \"http://www.w3.org/TR/xhtml1/DTD/xhtml1-transitional.dtd\">",
                QuirksMode::LimitedQuirks,
            ),
            (
                "<!DOCTYPE html PUBLIC \"-//W3C//DTD XHTML 1.0 Transitional//EN\">",
                QuirksMode::LimitedQuirks,
            ),
            (
                "<!DOCTYPE html PUBLIC \"-//W3C//DTD XHTML 1.0 Frameset//EN\" \"http://www.w3.org/TR/xhtml1/DTD/xhtml1-frameset.dtd\">",
                QuirksMode::LimitedQuirks,
            ),
            // HTML 4.01 transitional/frameset: quirks when the system
            // identifier is missing or empty, limited quirks otherwise.
            (
                "<!DOCTYPE html PUBLIC \"-//W3C//DTD HTML 4.01 Transitional//EN\">",
                QuirksMode::Quirks,
            ),
            (
                "<!DOCTYPE html PUBLIC \"-//W3C//DTD HTML 4.01 Transitional//EN\" \"http://www.w3.org/TR/html4/loose.dtd\">",
                QuirksMode::LimitedQuirks,
            ),
            (
                "<!DOCTYPE html PUBLIC \"-//W3C//DTD HTML 4.01 Frameset//EN\" \"\">",
                QuirksMode::Quirks,
            ),
            // Exact-match quirks identifiers: no prefix matching, so `HTML5`
            // stays standards.
            ("<!DOCTYPE html PUBLIC \"HTML\">", QuirksMode::Quirks),
            ("<!DOCTYPE html PUBLIC \"HTML5\">", QuirksMode::NoQuirks),
            (
                "<!DOCTYPE html PUBLIC \"-//W3O//DTD W3 HTML Strict 3.0//EN//\">",
                QuirksMode::Quirks,
            ),
            // Prefix quirks identifiers, ASCII case-insensitive.
            (
                "<!DOCTYPE html PUBLIC \"-//W3C//DTD HTML 4.0 Transitional//EN\">",
                QuirksMode::Quirks,
            ),
            (
                "<!DOCTYPE html PUBLIC \"-//w3c//dtd html 4.0 transitional//en\">",
                QuirksMode::Quirks,
            ),
            (
                "<!DOCTYPE html PUBLIC \"-//IETF//DTD HTML 2.0//EN\" \"anything\">",
                QuirksMode::Quirks,
            ),
            // The IBM system identifier, ASCII case-insensitive.
            (
                "<!DOCTYPE html SYSTEM \"http://www.ibm.com/data/dtd/v11/ibmxhtml1-transitional.dtd\">",
                QuirksMode::Quirks,
            ),
            (
                "<!DOCTYPE html SYSTEM \"HTTP://WWW.IBM.COM/DATA/DTD/V11/IBMXHTML1-TRANSITIONAL.DTD\">",
                QuirksMode::Quirks,
            ),
            ("<!DOCTYPE svg>", QuirksMode::Quirks),
            // Tokenizer-level force-quirks: `>` inside a quoted identifier,
            // a quoted identifier with no keyword, comments before the
            // doctype (including abruptly-closed ones).
            ("<!DOCTYPE html PUBLIC \"foo>bar\">", QuirksMode::Quirks),
            ("<!DOCTYPE html \"foo\">", QuirksMode::Quirks),
            ("<!-- comment --><!DOCTYPE html>", QuirksMode::NoQuirks),
            ("<!--><!DOCTYPE html>", QuirksMode::NoQuirks),
            ("<!---><!DOCTYPE html>", QuirksMode::NoQuirks),
            ("<!-- x --!><!DOCTYPE html>", QuirksMode::NoQuirks),
            // An abrupt close wins over a later `-->`/`--!>`: `<!-->`
            // closes at the first `>`, leaving character data before the
            // doctype (quirks).
            ("<!--> x --><!DOCTYPE html>", QuirksMode::Quirks),
            ("<!-- a --!> b --><!DOCTYPE html>", QuirksMode::Quirks),
            // A `SYSTEM` keyword without a quoted identifier, and trailing
            // garbage where the system identifier belongs, are tokenizer
            // force-quirks inputs.
            ("<!DOCTYPE html SYSTEM>", QuirksMode::Quirks),
            ("<!DOCTYPE html PUBLIC \"x\" foo>", QuirksMode::Quirks),
        ];
        for (input, expected) in cases {
            assert_eq!(sniff_quirks_mode(input), expected, "{input}");
        }
    }

    // The quirks sniff slices near attacker-controlled bytes; none of these
    // inputs may panic, whatever mode they return.
    #[test]
    fn quirks_sniff_never_panics_on_partial_or_multibyte_input() {
        let inputs = [
            "",
            "<",
            "<!DOC",
            "<!DOCTYPE",
            "<!DOCTYPE ",
            "<!DOCTYPE a",
            "<!DOCTYPE a SYSTE",
            "<!DOCTYPE a SYSTE\u{e9}>",
            "<!DOCTYPE a PUBLI\u{e9}\">",
            "<ab>\u{65e5}\u{672c}\u{8a9e}</ab>",
            "<!DOCTYPE \u{65e5}\u{672c}\u{8a9e}>",
            "\u{feff}<!DOCTYPE html>",
            "<!-- unterminated",
            "<!DOCTYPE html PUBLIC \"-//W3C//DTD XHTML 1.0 Transitional//EN\"",
            "<?\u{65e5}?>",
            "<!\u{e9}DOCTYPE html>",
        ];
        for input in inputs {
            let _ = sniff_quirks_mode(input);
            // Quirks is the only mode the exact-`html` production escapes.
            if input == "\u{feff}<!DOCTYPE html>" {
                assert_eq!(sniff_quirks_mode(input), QuirksMode::NoQuirks);
            }
        }
    }
}
