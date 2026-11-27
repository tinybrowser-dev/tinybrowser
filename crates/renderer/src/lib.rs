use std::sync::Arc;

pub use ::dom::dom;
pub use ::dom::{
    DocumentLoadState, INVALID_BASE_URL, Parsed, ReadyState, RedirectGate, csp, css, documents,
    dom_string, integrity_verifies, names, parse_html, protocol, sniff, xml,
};
mod document;
mod embedded;
mod engine;
mod js;
mod messaging;
mod remote;
mod render;
mod storage;
#[cfg(test)]
mod test_services;
mod timers;
mod window_features;
mod worker;

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
const LINUX_GENERIC_FAMILIES: [(&str, parley::fontique::GenericFamily, &[&str]); 9] = {
    use parley::fontique::GenericFamily;
    [
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
        (
            "emoji",
            GenericFamily::Emoji,
            &[
                "Noto Color Emoji",
                "Twemoji",
                "EmojiOne Color",
                "Noto Emoji",
                "Symbola",
                "DejaVu Sans",
            ],
        ),
        (
            "math",
            GenericFamily::Math,
            &[
                "STIX Two Math",
                "Latin Modern Math",
                "Libertinus Math",
                "Noto Sans Math",
                "DejaVu Math TeX Gyre",
                "Cambria Math",
                "FreeSerif",
                "DejaVu Sans",
            ],
        ),
        (
            "fangsong",
            GenericFamily::FangSong,
            &[
                "Noto Serif CJK SC",
                "Noto Sans CJK SC",
                "WenQuanYi Zen Hei",
                "AR PL UMing CN",
            ],
        ),
    ]
};

#[cfg(target_os = "linux")]
const LINUX_SCRIPT_FALLBACKS: [([u8; 4], &[&str], &[&str]); 10] = [
    (
        *b"Arab",
        &[
            "Noto Sans Arabic",
            "Noto Naskh Arabic",
            "Amiri",
            "DejaVu Sans",
            "FreeSans",
        ],
        &["arabic", "naskh", "unifont"],
    ),
    (
        *b"Hebr",
        &[
            "Noto Sans Hebrew",
            "Noto Serif Hebrew",
            "DejaVu Sans",
            "FreeSans",
        ],
        &["hebrew", "unifont"],
    ),
    (
        *b"Deva",
        &[
            "Noto Sans Devanagari",
            "Noto Serif Devanagari",
            "Lohit Devanagari",
            "FreeSans",
            "DejaVu Sans",
        ],
        &["devanagari", "unifont"],
    ),
    (
        *b"Beng",
        &[
            "Noto Sans Bengali",
            "Noto Serif Bengali",
            "Lohit Bengali",
            "FreeSans",
        ],
        &["bengali", "unifont"],
    ),
    (
        *b"Thai",
        &[
            "Noto Sans Thai",
            "Noto Serif Thai",
            "Loma",
            "Waree",
            "Garuda",
            "FreeSans",
            "DejaVu Sans",
        ],
        &["thai", "unifont"],
    ),
    (
        *b"Tibt",
        &["Noto Serif Tibetan", "Noto Sans Tibetan", "Jomolhari"],
        &["tibetan", "unifont"],
    ),
    (
        *b"Ethi",
        &["Noto Sans Ethiopic", "Abyssinica SIL", "FreeSerif"],
        &["ethiopic", "unifont"],
    ),
    (
        *b"Cyrl",
        &["DejaVu Sans", "Liberation Sans", "Noto Sans", "FreeSans"],
        &["unifont"],
    ),
    (
        *b"Grek",
        &["DejaVu Sans", "Liberation Sans", "Noto Sans", "FreeSans"],
        &["unifont"],
    ),
    (
        *b"Latn",
        &["DejaVu Sans", "Liberation Sans", "Noto Sans", "FreeSans"],
        &["unifont"],
    ),
];

#[cfg(target_os = "linux")]
const LINUX_CJK_SIMPLIFIED: (&[&str], &[&str]) = (
    &[
        "Noto Sans CJK SC",
        "Noto Serif CJK SC",
        "Source Han Sans SC",
        "Source Han Serif SC",
        "WenQuanYi Zen Hei",
        "WenQuanYi Micro Hei",
        "Droid Sans Fallback",
        "Noto Sans CJK JP",
        "Noto Sans CJK TC",
        "Noto Sans CJK KR",
    ],
    &["cjk sc", "han sans sc", "wqy", "cjk", "unifont"],
);

#[cfg(target_os = "linux")]
const LINUX_CJK_TRADITIONAL: (&[&str], &[&str]) = (
    &[
        "Noto Sans CJK TC",
        "Noto Serif CJK TC",
        "Source Han Sans TC",
        "Noto Sans CJK HK",
        "Noto Sans CJK SC",
        "Droid Sans Fallback",
    ],
    &["cjk tc", "cjk hk", "cjk", "unifont"],
);

#[cfg(target_os = "linux")]
const LINUX_CJK_HONG_KONG: (&[&str], &[&str]) = (
    &[
        "Noto Sans CJK HK",
        "Noto Serif CJK HK",
        "Noto Sans CJK TC",
        "Noto Sans CJK SC",
        "Droid Sans Fallback",
    ],
    &["cjk hk", "cjk tc", "cjk", "unifont"],
);

#[cfg(target_os = "linux")]
const LINUX_CJK_JAPANESE: (&[&str], &[&str]) = (
    &[
        "Noto Sans CJK JP",
        "Noto Serif CJK JP",
        "Source Han Sans JP",
        "IPAGothic",
        "IPAexGothic",
        "VL Gothic",
        "TakaoPGothic",
        "Noto Sans CJK SC",
        "Droid Sans Fallback",
    ],
    &["cjk jp", "han sans jp", "ipa", "cjk", "unifont"],
);

#[cfg(target_os = "linux")]
const LINUX_CJK_KOREAN: (&[&str], &[&str]) = (
    &[
        "Noto Sans CJK KR",
        "Noto Serif CJK KR",
        "Source Han Sans KR",
        "NanumGothic",
        "UnDotum",
        "Baekmuk Gulim",
        "Noto Sans CJK SC",
        "Droid Sans Fallback",
    ],
    &["cjk kr", "han sans kr", "nanum", "hangul", "cjk", "unifont"],
);

#[cfg(target_os = "linux")]
const LINUX_COMMON_FALLBACKS: (&[&str], &[&str]) = (
    &[
        "DejaVu Sans",
        "Noto Sans",
        "Noto Sans Symbols",
        "Noto Sans Symbols 2",
        "Noto Sans Math",
        "STIX Two Math",
        "Latin Modern Math",
        "Libertinus Math",
        "DejaVu Math TeX Gyre",
        "Noto Color Emoji",
        "Noto Emoji",
        "Symbola",
        "FreeSerif",
        "FreeSans",
    ],
    &["symbol", "math", "emoji", "unifont"],
);

#[cfg(target_os = "linux")]
fn apply_linux_generic_families(
    font_ctx: &mut parley::FontContext,
    aliases: &[fontconfig_parser::Alias],
    first_family: Option<parley::fontique::FamilyId>,
) {
    for (name, generic, fallbacks) in LINUX_GENERIC_FAMILIES {
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
fn collect_matching_families(
    font_ctx: &mut parley::FontContext,
    installed: &[(parley::fontique::FamilyId, String)],
    exact: &[&str],
    substrings: &[&str],
) -> Vec<parley::fontique::FamilyId> {
    let mut ids = Vec::new();
    for name in exact {
        if let Some(id) = font_ctx.collection.family_id(name)
            && !ids.contains(&id)
        {
            ids.push(id);
        }
    }
    for needle in substrings {
        let needle_lower = needle.to_ascii_lowercase();
        for (id, family) in installed {
            if family.to_ascii_lowercase().contains(&needle_lower) && !ids.contains(id) {
                ids.push(*id);
            }
        }
    }
    ids
}

#[cfg(target_os = "linux")]
fn set_fallback_if_nonempty(
    font_ctx: &mut parley::FontContext,
    key: parley::fontique::FallbackKey,
    ids: &[parley::fontique::FamilyId],
) {
    if !ids.is_empty() {
        font_ctx.collection.set_fallbacks(key, ids.iter().copied());
    }
}

#[cfg(target_os = "linux")]
fn apply_linux_cjk_fallbacks(
    font_ctx: &mut parley::FontContext,
    installed: &[(parley::fontique::FamilyId, String)],
) {
    use parley::fontique::{FallbackKey, Script};

    let simplified = collect_matching_families(
        font_ctx,
        installed,
        LINUX_CJK_SIMPLIFIED.0,
        LINUX_CJK_SIMPLIFIED.1,
    );
    let traditional = collect_matching_families(
        font_ctx,
        installed,
        LINUX_CJK_TRADITIONAL.0,
        LINUX_CJK_TRADITIONAL.1,
    );
    let hong_kong = collect_matching_families(
        font_ctx,
        installed,
        LINUX_CJK_HONG_KONG.0,
        LINUX_CJK_HONG_KONG.1,
    );
    let japanese = collect_matching_families(
        font_ctx,
        installed,
        LINUX_CJK_JAPANESE.0,
        LINUX_CJK_JAPANESE.1,
    );
    let korean =
        collect_matching_families(font_ctx, installed, LINUX_CJK_KOREAN.0, LINUX_CJK_KOREAN.1);

    let hani = Script::from_bytes(*b"Hani");
    for (key, ids) in [
        (FallbackKey::new(hani, None), simplified.as_slice()),
        (FallbackKey::from((hani, "zh-TW")), traditional.as_slice()),
        (FallbackKey::from((hani, "zh-HK")), hong_kong.as_slice()),
        (FallbackKey::from((hani, "zh-MO")), hong_kong.as_slice()),
        (FallbackKey::from((hani, "zh-SG")), simplified.as_slice()),
        (FallbackKey::from((hani, "ja")), japanese.as_slice()),
        (FallbackKey::from((hani, "ko")), korean.as_slice()),
        (
            FallbackKey::new(Script::from_bytes(*b"Hira"), None),
            japanese.as_slice(),
        ),
        (
            FallbackKey::new(Script::from_bytes(*b"Kana"), None),
            japanese.as_slice(),
        ),
        (
            FallbackKey::new(Script::from_bytes(*b"Hang"), None),
            korean.as_slice(),
        ),
    ] {
        set_fallback_if_nonempty(font_ctx, key, ids);
    }
}

#[cfg(target_os = "linux")]
fn apply_linux_script_fallbacks(
    font_ctx: &mut parley::FontContext,
    aliases: &[fontconfig_parser::Alias],
) {
    use parley::fontique::{FallbackKey, Script};

    let installed: Vec<(parley::fontique::FamilyId, String)> = font_ctx
        .collection
        .family_ids()
        .collect::<Vec<_>>()
        .into_iter()
        .filter_map(|id| Some((id, font_ctx.collection.family_name(id)?.to_owned())))
        .collect();

    let mut alias_families = Vec::new();
    for alias in aliases {
        if alias.alias.eq_ignore_ascii_case("sans-serif")
            || alias.alias.eq_ignore_ascii_case("serif")
        {
            for candidate in alias
                .prefer
                .iter()
                .chain(alias.accept.iter())
                .chain(alias.default.iter())
            {
                if let Some(id) = font_ctx.collection.family_id(candidate)
                    && !alias_families.contains(&id)
                {
                    alias_families.push(id);
                }
            }
        }
    }

    apply_linux_cjk_fallbacks(font_ctx, &installed);

    for (tag, exact, substrings) in LINUX_SCRIPT_FALLBACKS {
        let mut ids = collect_matching_families(font_ctx, &installed, exact, substrings);
        for id in &alias_families {
            if !ids.contains(id) {
                ids.push(*id);
            }
        }
        set_fallback_if_nonempty(
            font_ctx,
            FallbackKey::new(Script::from_bytes(tag), None),
            &ids,
        );
    }

    let mut common = collect_matching_families(
        font_ctx,
        &installed,
        LINUX_COMMON_FALLBACKS.0,
        LINUX_COMMON_FALLBACKS.1,
    );
    for id in &alias_families {
        if !common.contains(id) {
            common.push(*id);
        }
    }
    for tag in [*b"Zyyy", *b"Zinh"] {
        set_fallback_if_nonempty(
            font_ctx,
            FallbackKey::new(Script::from_bytes(tag), None),
            &common,
        );
    }
}

#[cfg(target_os = "linux")]
fn load_linux_system_fonts() -> Fonts {
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};

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
    let mut unique_dirs = Vec::new();
    for dir in dirs {
        if let Ok(canonical) = std::fs::canonicalize(&dir)
            && seen_dirs.insert(canonical.clone())
        {
            unique_dirs.push(canonical);
        }
    }

    let mut font_ctx = parley::FontContext::default();
    font_ctx.collection.load_fonts_from_paths(&unique_dirs);
    let first_family = font_ctx.collection.family_ids().next();
    apply_linux_generic_families(&mut font_ctx, &config.aliases, first_family);
    apply_linux_script_fallbacks(&mut font_ctx, &config.aliases);
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
    PromptKind, PromptOutcome, ReferrerRequest, RendererEvent, RequestCache, RequestPolicy,
    ResourceLimit, ResponseBody, ResponseHead, ResponseSource, STORAGE_QUOTA_BYTES, SandboxFlags,
    ScreenshotClip, ScreenshotFormat, ScreenshotRequest, ScriptCookie, ScriptFailure, ScriptSource,
    SerializedOrigin, StorageChange, StorageError, StorageHost, StorageKey, StorageKind, StreamId,
    TabError, TabState, TargetName, TargetOrigin, TimerId, WindowId,
};
pub use remote::RemoteValue;
pub use storage::PendingStorageEvent;

#[cfg(all(test, target_os = "linux"))]
mod tests {
    #[test]
    fn linux_system_fonts_configure_script_fallbacks() {
        let mut fonts = super::Fonts::default();
        let installed: Vec<(parley::fontique::FamilyId, String)> = fonts
            .0
            .collection
            .family_ids()
            .collect::<Vec<_>>()
            .into_iter()
            .filter_map(|id| Some((id, fonts.0.collection.family_name(id)?.to_owned())))
            .collect();
        if installed.is_empty() {
            return;
        }
        let has_cjk = installed.iter().any(|(_, name)| {
            let name = name.to_ascii_lowercase();
            name.contains("cjk") || name.contains("han") || name.contains("wqy")
        });
        if has_cjk {
            let has_hani = fonts
                .0
                .collection
                .fallback_families(parley::fontique::FallbackKey::new(
                    parley::fontique::Script::from_bytes(*b"Hani"),
                    None,
                ))
                .next()
                .is_some();
            assert!(has_hani, "Linux font loader must configure Hani fallbacks");
        }
        let has_common_source = installed.iter().any(|(_, name)| {
            super::LINUX_COMMON_FALLBACKS
                .0
                .iter()
                .any(|candidate| name.eq_ignore_ascii_case(candidate))
        });
        if has_common_source {
            let has_common = fonts
                .0
                .collection
                .fallback_families(parley::fontique::FallbackKey::new(
                    parley::fontique::Script::from_bytes(*b"Zyyy"),
                    None,
                ))
                .next()
                .is_some();
            assert!(
                has_common,
                "Linux font loader must configure Common (Zyyy) fallbacks"
            );
        }
    }
}
