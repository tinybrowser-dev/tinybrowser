// https://html.spec.whatwg.org/multipage/common-dom-interfaces.html#limited-to-only-known-values

pub(crate) use enumerated::Enumerated;
use enumerated::enumerated;

// https://html.spec.whatwg.org/multipage/common-dom-interfaces.html#limited-to-only-known-values
#[derive(Clone, Copy)]
pub(crate) struct Defaults<T> {
    pub(crate) missing: Option<T>,
    pub(crate) invalid: Option<T>,
}

impl<T> Defaults<T> {
    pub(crate) const NONE: Self = Self {
        missing: None,
        invalid: None,
    };
}

// https://html.spec.whatwg.org/multipage/common-dom-interfaces.html#limited-to-only-known-values
pub(crate) fn reflect_enumerated<T: Enumerated>(
    raw: Option<&str>,
    defaults: Defaults<T>,
) -> &'static str {
    let Some(raw) = raw else {
        return defaults.missing.map_or("", |value| value.keyword());
    };
    T::parse(raw)
        .or(defaults.invalid)
        .map_or("", |value| value.keyword())
}

// https://w3c.github.io/aria/#enumerated-attribute-values-html
pub(crate) fn reflect_enumerated_nullable<T: Enumerated>(
    raw: Option<&str>,
    defaults: Defaults<T>,
) -> Option<&'static str> {
    let Some(raw) = raw else {
        return defaults.missing.map(Enumerated::keyword);
    };
    T::parse(raw).or(defaults.invalid).map(Enumerated::keyword)
}

enumerated! {
    // https://html.spec.whatwg.org/multipage/dom.html#the-dir-attribute
    pub(crate) Dir {
        Ltr => "ltr",
        Rtl => "rtl",
        Auto => "auto",
    }
}

enumerated! {
    // https://html.spec.whatwg.org/multipage/interaction.html#attr-enterkeyhint
    pub(crate) EnterKeyHint {
        Enter => "enter",
        Done => "done",
        Go => "go",
        Next => "next",
        Previous => "previous",
        Search => "search",
        Send => "send",
    }
}

enumerated! {
    // https://html.spec.whatwg.org/multipage/interaction.html#attr-inputmode
    pub(crate) InputMode {
        None => "none",
        Text => "text",
        Tel => "tel",
        Url => "url",
        Email => "email",
        Numeric => "numeric",
        Decimal => "decimal",
        Search => "search",
    }
}

enumerated! {
    // https://html.spec.whatwg.org/multipage/urls-and-fetching.html#attr-loading
    pub(crate) Loading {
        Lazy => "lazy",
        Eager => "eager",
    }
}

enumerated! {
    // https://html.spec.whatwg.org/multipage/media.html#attr-media-preload
    pub(crate) Preload {
        None => "none",
        Metadata => "metadata",
        Auto => "auto",
    }
}

enumerated! {
    // https://html.spec.whatwg.org/multipage/embedded-content.html#attr-img-decoding
    pub(crate) Decoding {
        Async => "async",
        Sync => "sync",
        Auto => "auto",
    }
}

enumerated! {
    // https://html.spec.whatwg.org/multipage/urls-and-fetching.html#fetch-priority-attributes
    pub(crate) FetchPriority {
        High => "high",
        Low => "low",
        Auto => "auto",
    }
}

// https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#attr-fs-method
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FormMethod {
    Get,
    Post,
    Dialog,
}

impl Enumerated for FormMethod {
    const KEYWORDS: &'static [Self] = &[Self::Get, Self::Post, Self::Dialog];

    fn keyword(self) -> &'static str {
        let engine = match self {
            Self::Get => blitz_dom::form::FormMethod::Get,
            Self::Post => blitz_dom::form::FormMethod::Post,
            Self::Dialog => blitz_dom::form::FormMethod::Dialog,
        };
        engine.as_str()
    }

    fn parse(value: &str) -> Option<Self> {
        use std::str::FromStr;
        Some(match blitz_dom::form::FormMethod::from_str(value).ok()? {
            blitz_dom::form::FormMethod::Get => Self::Get,
            blitz_dom::form::FormMethod::Post => Self::Post,
            blitz_dom::form::FormMethod::Dialog => Self::Dialog,
        })
    }
}

// https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#attr-fs-enctype
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FormEnctype {
    UrlEncoded,
    Multipart,
    TextPlain,
}

impl Enumerated for FormEnctype {
    const KEYWORDS: &'static [Self] = &[Self::UrlEncoded, Self::Multipart, Self::TextPlain];

    fn keyword(self) -> &'static str {
        let engine = match self {
            Self::UrlEncoded => blitz_dom::form::RequestContentType::FormUrlEncoded,
            Self::Multipart => blitz_dom::form::RequestContentType::MultipartFormData,
            Self::TextPlain => blitz_dom::form::RequestContentType::TextPlain,
        };
        engine.as_str()
    }

    fn parse(value: &str) -> Option<Self> {
        use std::str::FromStr;
        Some(
            match blitz_dom::form::RequestContentType::from_str(value).ok()? {
                blitz_dom::form::RequestContentType::FormUrlEncoded => Self::UrlEncoded,
                blitz_dom::form::RequestContentType::MultipartFormData => Self::Multipart,
                blitz_dom::form::RequestContentType::TextPlain => Self::TextPlain,
            },
        )
    }
}

enumerated! {
    // https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#attr-fe-autocomplete
    pub(crate) Autocomplete {
        On => "on",
        Off => "off",
    }
}

enumerated! {
    // https://html.spec.whatwg.org/multipage/tables.html#attr-th-scope
    pub(crate) TableScope {
        Row => "row",
        Col => "col",
        RowGroup => "rowgroup",
        ColGroup => "colgroup",
    }
}

enumerated! {
    // https://html.spec.whatwg.org/multipage/semantics.html#attr-link-as
    pub(crate) LinkAs {
        Fetch => "fetch",
        Audio => "audio",
        AudioWorklet => "audioworklet",
        Document => "document",
        Embed => "embed",
        Font => "font",
        Frame => "frame",
        Iframe => "iframe",
        Image => "image",
        Json => "json",
        Manifest => "manifest",
        Object => "object",
        PaintWorklet => "paintworklet",
        Report => "report",
        Script => "script",
        ServiceWorker => "serviceworker",
        SharedWorker => "sharedworker",
        Style => "style",
        Track => "track",
        Video => "video",
        Worker => "worker",
        Xslt => "xslt",
    }
}

enumerated! {
    // https://html.spec.whatwg.org/multipage/media.html#attr-track-kind
    pub(crate) TrackKind {
        Subtitles => "subtitles",
        Captions => "captions",
        Descriptions => "descriptions",
        Chapters => "chapters",
        Metadata => "metadata",
    }
}

// https://w3c.github.io/aria/#enumerated-attribute-values-html

enumerated! {
    pub(crate) AriaTrueFalse {
        True => "true",
        False => "false",
    }
}

enumerated! {
    pub(crate) AriaTristate {
        True => "true",
        False => "false",
        Mixed => "mixed",
    }
}

enumerated! {
    pub(crate) AriaHasPopup {
        True => "true",
        False => "false",
        Menu => "menu",
        Dialog => "dialog",
        Listbox => "listbox",
        Tree => "tree",
        Grid => "grid",
    }
}

enumerated! {
    pub(crate) AriaOrientation {
        Horizontal => "horizontal",
        Vertical => "vertical",
    }
}

enumerated! {
    // https://w3c.github.io/aria/#aria-autocomplete
    pub(crate) AriaAutoComplete {
        Inline => "inline",
        List => "list",
        Both => "both",
        None => "none",
    }
}

enumerated! {
    // https://w3c.github.io/aria/#aria-current
    pub(crate) AriaCurrent {
        Page => "page",
        Step => "step",
        Location => "location",
        Date => "date",
        Time => "time",
        True => "true",
        False => "false",
    }
}

enumerated! {
    // https://w3c.github.io/aria/#aria-invalid
    pub(crate) AriaInvalid {
        True => "true",
        False => "false",
        Spelling => "spelling",
        Grammar => "grammar",
    }
}

enumerated! {
    pub(crate) AriaLive {
        Polite => "polite",
        Assertive => "assertive",
        Off => "off",
    }
}

enumerated! {
    pub(crate) AriaSort {
        Ascending => "ascending",
        Descending => "descending",
        Other => "other",
        None => "none",
    }
}

#[cfg(test)]
mod reflection_tests {
    use super::{
        AriaTrueFalse, Defaults, TrackKind, reflect_enumerated, reflect_enumerated_nullable,
    };

    // https://html.spec.whatwg.org/multipage/media.html#dom-track-kind
    #[test]
    fn reflection_answers_each_state_once() {
        let track = Defaults {
            missing: Some(TrackKind::Subtitles),
            invalid: Some(TrackKind::Metadata),
        };
        assert_eq!(reflect_enumerated::<TrackKind>(None, track), "subtitles");
        assert_eq!(
            reflect_enumerated::<TrackKind>(Some("captions"), track),
            "captions"
        );
        assert_eq!(
            reflect_enumerated::<TrackKind>(Some("CAPTIONS"), track),
            "captions",
            "a keyword matches ASCII case-insensitively"
        );
        assert_eq!(
            reflect_enumerated::<TrackKind>(Some("bogus"), track),
            "metadata",
            "an invalid value reads the invalid-value default"
        );
        assert_eq!(
            reflect_enumerated::<TrackKind>(None, Defaults::NONE),
            "",
            "no missing-value default reads the empty string"
        );
        assert_eq!(
            reflect_enumerated::<TrackKind>(Some("bogus"), Defaults::NONE),
            "",
            "no invalid-value default reads the empty string"
        );
    }

    // https://w3c.github.io/aria/#enumerated-attribute-values-html
    #[test]
    fn nullable_reflection_reports_the_undefined_state_as_null() {
        let atomic = Defaults {
            missing: None,
            invalid: Some(AriaTrueFalse::False),
        };
        assert_eq!(
            reflect_enumerated_nullable::<AriaTrueFalse>(None, atomic),
            None,
            "a removed `aria-atomic` is the Undefined state"
        );
        assert_eq!(
            reflect_enumerated_nullable::<AriaTrueFalse>(Some("TRUE"), atomic),
            Some("true"),
            "a keyword matches ASCII case-insensitively and reads canonically"
        );
        assert_eq!(
            reflect_enumerated_nullable::<AriaTrueFalse>(Some("bogus"), atomic),
            Some("false"),
            "an invalid value reads the invalid-value default"
        );
        let pressed = Defaults::<AriaTrueFalse>::NONE;
        assert_eq!(
            reflect_enumerated_nullable::<AriaTrueFalse>(Some("bogus"), pressed),
            None,
            "no invalid-value default is the Undefined state"
        );
    }
}
