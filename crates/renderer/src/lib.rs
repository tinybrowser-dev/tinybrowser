use std::sync::Arc;

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

#[derive(Clone)]
#[cfg_attr(not(target_os = "linux"), derive(Default))]
pub struct Fonts(parley::FontContext);

#[cfg(target_os = "linux")]
impl Default for Fonts {
    fn default() -> Self {
        static SYSTEM_FONTS: std::sync::OnceLock<Fonts> = std::sync::OnceLock::new();
        SYSTEM_FONTS.get_or_init(load_linux_system_fonts).clone()
    }
}

#[cfg(target_os = "linux")]
fn collect_linux_font_files(
    dir: &std::path::Path,
    seen_dirs: &mut std::collections::HashSet<std::path::PathBuf>,
    out: &mut Vec<std::path::PathBuf>,
) {
    let Ok(canonical) = std::fs::canonicalize(dir) else {
        return;
    };
    if !seen_dirs.insert(canonical) {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_linux_font_files(&path, seen_dirs, out);
        } else if path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| {
                ext.eq_ignore_ascii_case("ttf")
                    || ext.eq_ignore_ascii_case("otf")
                    || ext.eq_ignore_ascii_case("ttc")
                    || ext.eq_ignore_ascii_case("otc")
            })
        {
            out.push(path);
        }
    }
}

#[cfg(target_os = "linux")]
fn apply_linux_generic_families(
    font_ctx: &mut parley::FontContext,
    aliases: &[fontconfig_parser::Alias],
    first_family: Option<parley::fontique::FamilyId>,
) {
    use parley::fontique::GenericFamily;

    let generic_specs: [(&str, GenericFamily, &[&str]); 6] = [
        (
            "sans-serif",
            GenericFamily::SansSerif,
            &["DejaVu Sans", "Liberation Sans", "Noto Sans", "FreeSans"],
        ),
        (
            "serif",
            GenericFamily::Serif,
            &[
                "DejaVu Serif",
                "Liberation Serif",
                "Noto Serif",
                "FreeSerif",
            ],
        ),
        (
            "monospace",
            GenericFamily::Monospace,
            &[
                "DejaVu Sans Mono",
                "Liberation Mono",
                "Noto Sans Mono",
                "FreeMono",
            ],
        ),
        (
            "system-ui",
            GenericFamily::SystemUi,
            &["DejaVu Sans", "Liberation Sans", "Noto Sans", "FreeSans"],
        ),
        (
            "cursive",
            GenericFamily::Cursive,
            &["DejaVu Sans", "Liberation Sans", "Noto Sans"],
        ),
        (
            "fantasy",
            GenericFamily::Fantasy,
            &["DejaVu Serif", "Liberation Serif", "Noto Serif"],
        ),
    ];

    for (name, generic, fallbacks) in generic_specs {
        let mut ids = Vec::new();
        for alias in aliases {
            if alias.alias.eq_ignore_ascii_case(name) {
                for candidate in alias
                    .prefer
                    .iter()
                    .chain(alias.accept.iter())
                    .chain(alias.default.iter())
                {
                    if let Some(id) = font_ctx.collection.family_id(candidate)
                        && !ids.contains(&id)
                    {
                        ids.push(id);
                    }
                }
            }
        }
        for candidate in fallbacks {
            if let Some(id) = font_ctx.collection.family_id(candidate)
                && !ids.contains(&id)
            {
                ids.push(id);
            }
        }
        if ids.is_empty()
            && let Some(id) = first_family
        {
            ids.push(id);
        }
        if !ids.is_empty() {
            font_ctx
                .collection
                .set_generic_families(generic, ids.into_iter());
        }
    }
}

#[cfg(target_os = "linux")]
fn load_linux_system_fonts() -> Fonts {
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};

    use parley::fontique::Blob;

    let mut config = fontconfig_parser::FontConfig::default();
    if let Ok(path) = std::env::var("FONTCONFIG_FILE") {
        let _ = config.merge_config(Path::new(&path));
    } else {
        let _ = config.merge_config(Path::new("/etc/fonts/fonts.conf"));
    }

    let mut dirs: Vec<PathBuf> = config.dirs.into_iter().map(|dir| dir.path).collect();
    dirs.push(PathBuf::from("/usr/share/fonts"));
    dirs.push(PathBuf::from("/usr/local/share/fonts"));
    if let Ok(xdg_data) = std::env::var("XDG_DATA_HOME")
        && !xdg_data.is_empty()
    {
        dirs.push(PathBuf::from(xdg_data).join("fonts"));
    } else if let Ok(home) = std::env::var("HOME")
        && !home.is_empty()
    {
        dirs.push(PathBuf::from(&home).join(".local/share/fonts"));
        dirs.push(PathBuf::from(home).join(".fonts"));
    }

    let mut seen_dirs = HashSet::new();
    let mut files = Vec::new();
    for dir in dirs {
        collect_linux_font_files(&dir, &mut seen_dirs, &mut files);
    }

    let mut font_ctx = parley::FontContext::default();
    let mut first_family = None;
    for path in files {
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let registered = font_ctx
            .collection
            .register_fonts(Blob::new(Arc::new(bytes)), None);
        if first_family.is_none()
            && let Some((family, _)) = registered.first()
        {
            first_family = Some(*family);
        }
    }

    apply_linux_generic_families(&mut font_ctx, &config.aliases, first_family);
    Fonts(font_ctx)
}

impl Fonts {
    pub fn register_font(&mut self, data: Arc<dyn AsRef<[u8]> + Send + Sync>) -> usize {
        self.0
            .collection
            .register_fonts(parley::fontique::Blob::new(data), None)
            .len()
    }

    pub fn register_fallback_font(&mut self, data: Vec<u8>) -> usize {
        use parley::fontique::{Blob, GenericFamily};

        let decoded = crate::js::fonts::decode_font(data);
        if decoded.is_empty() {
            return 0;
        }
        let registered = self
            .0
            .collection
            .register_fonts(Blob::new(Arc::new(decoded)), None);
        let families: Vec<_> = registered.iter().map(|(id, _)| *id).collect();
        if !families.is_empty() {
            for generic in [
                GenericFamily::SansSerif,
                GenericFamily::Serif,
                GenericFamily::Monospace,
                GenericFamily::SystemUi,
            ] {
                self.0
                    .collection
                    .append_generic_families(generic, families.iter().copied());
            }
        }
        families.len()
    }
}

pub use js::{Stop, TURN_SCRIPT_BUDGET};
pub use protocol::{
    AxNode, AxProperty, AxPropertyValue, BodyDelivery, BrowserEvent, BrowserServices,
    BrowsingContextHost, ConsoleLevel, ContextName, CookiePolicy, CorsMode, CorsRequest,
    DialCancellation, DialCompletion, DialFailure, DialKind, DialOutcome, DialRequest, DomBoxModel,
    DomNode, DomRemoteNode, DomReply, DomRequest, DomStyleSheet, FrameId, FrameKind,
    HistoryHandling, HistorySnapshot, HttpHead, MAX_RESPONSE_BODY_BYTES, MessagingHost, Mount,
    NameChange, NavigationSource, NetworkHost, OpenerPolicy, PolicyContainer, PostedMessage,
    ReferrerRequest, RendererEvent, RequestCache, RequestPolicy, ResourceLimit, ResponseBody,
    ResponseHead, ResponseSource, STORAGE_QUOTA_BYTES, SandboxFlags, ScreenshotClip,
    ScreenshotFormat, ScreenshotRequest, ScriptCookie, ScriptFailure, ScriptSource,
    SerializedOrigin, StorageChange, StorageError, StorageHost, StorageKey, StorageKind, StreamId,
    TabError, TabState, TargetName, TargetOrigin, TimerId, WindowId,
};
pub use remote::RemoteValue;
pub use storage::PendingStorageEvent;

// https://drafts.csswg.org/cssom-view/#scroll-an-element
pub(crate) fn animation_seconds() -> f64 {
    static EPOCH: std::sync::OnceLock<tokio::time::Instant> = std::sync::OnceLock::new();
    EPOCH
        .get_or_init(tokio::time::Instant::now)
        .elapsed()
        .as_secs_f64()
}

// https://html.spec.whatwg.org/multipage/dom.html#current-document-readiness
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ReadyState {
    Loading,
    Interactive,
    Complete,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DocumentLoadState {
    pub ready_state: ReadyState,
    pub url: String,
}

pub(crate) struct Parsed {
    pub id: u32,
    pub document: BlitzDocument,
    pub quirks_mode: QuirksMode,
    pub content_type: &'static str,
    // https://dom.spec.whatwg.org/#xmldocument
    // https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-domparser-parsefromstring
    pub xml_document: bool,
    // https://html.spec.whatwg.org/multipage/dom.html#current-document-readiness
    pub ready_state: ReadyState,
    // https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-domparser-parsefromstring
    pub url: Option<String>,
    // https://encoding.spec.whatwg.org/#dom-document-characterset
    pub character_set: &'static str,
}

impl Parsed {
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

// https://html.spec.whatwg.org/multipage/parsing.html#the-initial-insertion-mode — treats an empty HTML 4.01 system identifier as missing (quirks mode), matching the spec and Chromium over html5ever
#[must_use]
pub(crate) fn parse_html(input: &str, mut config: blitz_dom::DocumentConfig) -> Parsed {
    let quirks_mode = sniff_quirks_mode(input);
    config.quirks_mode = Some(style_quirks(quirks_mode));
    let base: blitz_dom::BaseDocument = blitz_html::HtmlDocument::from_html(input, config).into();
    let document = BlitzDocument::from_base(base);
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
const QUIRKS_EXACT: &[&str] = &[
    "-//w3o//dtd w3 html strict 3.0//en//",
    "-/w3c/dtd html 4.0 transitional/en",
    "html",
];
const LIMITED_PREFIXES: &[&str] = &[
    "-//w3c//dtd xhtml 1.0 frameset//",
    "-//w3c//dtd xhtml 1.0 transitional//",
];
const HTML4_PREFIXES: &[&str] = &[
    "-//w3c//dtd html 4.01 frameset//",
    "-//w3c//dtd html 4.01 transitional//",
];

// https://html.spec.whatwg.org/multipage/parsing.html#the-initial-insertion-mode — treats an empty HTML 4.01 system identifier as missing (quirks mode), matching the spec and Chromium over html5ever
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
        while chars.peek().is_some_and(char::is_ascii_whitespace) {
            chars.next();
        }
        if chars.peek() != Some(&'>') {
            system = Some(quoted_string(&mut chars)?);
        }
    } else if word.eq_ignore_ascii_case("system") {
        system = Some(quoted_string(&mut chars)?);
    } else if !word.is_empty() || matches!(chars.peek(), Some('"' | '\'')) {
        return None;
    }
    if !chars.any(|char| char == '>') {
        return None;
    }
    Some((name, public, system))
}

fn skip_ignored_prefix(mut input: &str) -> &str {
    loop {
        let trimmed = input.trim_start_matches(['\u{feff}', ' ', '\t', '\n', '\x0c', '\r']);
        let Some(comment) = trimmed.strip_prefix("<!--") else {
            return trimmed;
        };
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

    // https://html.spec.whatwg.org/multipage/parsing.html#the-initial-insertion-mode — treats an empty HTML 4.01 system identifier as missing (quirks mode), matching the spec and Chromium over html5ever
    #[test]
    fn quirks_sniff_follows_the_spec_table() {
        let cases = [
            ("<!DOCTYPE html>", QuirksMode::NoQuirks),
            ("<!doctype html>", QuirksMode::NoQuirks),
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
            ("<!DOCTYPE html PUBLIC \"HTML\">", QuirksMode::Quirks),
            ("<!DOCTYPE html PUBLIC \"HTML5\">", QuirksMode::NoQuirks),
            (
                "<!DOCTYPE html PUBLIC \"-//W3O//DTD W3 HTML Strict 3.0//EN//\">",
                QuirksMode::Quirks,
            ),
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
            (
                "<!DOCTYPE html SYSTEM \"http://www.ibm.com/data/dtd/v11/ibmxhtml1-transitional.dtd\">",
                QuirksMode::Quirks,
            ),
            (
                "<!DOCTYPE html SYSTEM \"HTTP://WWW.IBM.COM/DATA/DTD/V11/IBMXHTML1-TRANSITIONAL.DTD\">",
                QuirksMode::Quirks,
            ),
            ("<!DOCTYPE svg>", QuirksMode::Quirks),
            ("<!DOCTYPE html PUBLIC \"foo>bar\">", QuirksMode::Quirks),
            ("<!DOCTYPE html \"foo\">", QuirksMode::Quirks),
            ("<!-- comment --><!DOCTYPE html>", QuirksMode::NoQuirks),
            ("<!--><!DOCTYPE html>", QuirksMode::NoQuirks),
            ("<!---><!DOCTYPE html>", QuirksMode::NoQuirks),
            ("<!-- x --!><!DOCTYPE html>", QuirksMode::NoQuirks),
            ("<!--> x --><!DOCTYPE html>", QuirksMode::Quirks),
            ("<!-- a --!> b --><!DOCTYPE html>", QuirksMode::Quirks),
            ("<!DOCTYPE html SYSTEM>", QuirksMode::Quirks),
            ("<!DOCTYPE html PUBLIC \"x\" foo>", QuirksMode::Quirks),
        ];
        for (input, expected) in cases {
            assert_eq!(sniff_quirks_mode(input), expected, "{input}");
        }
    }

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
            if input == "\u{feff}<!DOCTYPE html>" {
                assert_eq!(sniff_quirks_mode(input), QuirksMode::NoQuirks);
            }
        }
    }
}
