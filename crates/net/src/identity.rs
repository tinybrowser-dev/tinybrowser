//! One browser identity: the `User-Agent` string, the User-Agent Client
//! Hints, `Accept-Language`, and the product strings a page reads.
//!
//! The header, the `Sec-CH-UA*` request headers, `Accept-Language`,
//! `navigator.userAgent`, `navigator.userAgentData`, `navigator.languages`,
//! `navigator.vendor`, `navigator.productSub`, CSP reports, and CDP's
//! `Browser.getVersion` all read this one value, so a caller that overrides
//! the identity cannot leave one of them claiming another product
//! (<https://wicg.github.io/ua-client-hints/#http-ua-hints>).

use std::fmt::Write as _;

use http::header::{HeaderName, HeaderValue};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use url::Url;

use crate::HeaderError;

/// The built-in identity's `User-Agent` header: the reduced Chrome 152 string.
pub const DEFAULT_USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/152.0.0.0 Safari/537.36";

/// The built-in identity's full build, as `Sec-CH-UA` and the client hints
/// report it.
const DEFAULT_FULL_VERSION: &str = "152.0.7977.82";

/// The built-in identity's `navigator.languages`, most preferred first.
const DEFAULT_LANGUAGES: [&str; 2] = ["en-US", "en"];

/// A validated `User-Agent` field value.
///
/// Nothing in this crate builds one from an unchecked string: the CLI or the
/// embedder parses the value it was given, and every later reader only
/// serializes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserAgent(HeaderValue);

impl UserAgent {
    /// Wraps a value known to be valid at compile time, for a crate constant.
    ///
    /// # Panics
    ///
    /// When `value` contains a byte a field value cannot carry. A call in a
    /// `const` item makes that a compile error, which is how the built-in
    /// identity is checked.
    #[must_use]
    pub const fn from_static(value: &'static str) -> Self {
        Self(HeaderValue::from_static(value))
    }

    /// Parses one `User-Agent` value.
    ///
    /// # Errors
    ///
    /// [`HeaderError::InvalidValue`] when `value` is not a visible ASCII field
    /// value.
    pub fn parse(value: &str) -> Result<Self, HeaderError> {
        let header =
            HeaderValue::from_str(value).map_err(|_| HeaderError::InvalidValue(value.into()))?;
        // `from_str` also admits the obs-text bytes 0x80..=0xFF, which `to_str`
        // refuses. A value the type cannot print is a value `as_str` and the
        // wire would silently lose, so it is invalid here.
        header
            .to_str()
            .map_err(|_| HeaderError::InvalidValue(value.into()))?;
        Ok(Self(header))
    }

    /// The value as a string. The type guarantees visible ASCII, so this is
    /// lossless.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.to_str().unwrap_or_default()
    }

    /// The typed field value, for the request header list.
    pub(crate) fn header(&self) -> &HeaderValue {
        &self.0
    }
}

impl Serialize for UserAgent {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(
            self.0
                .to_str()
                .map_err(|_| serde::ser::Error::custom("a user agent is visible ASCII"))?,
        )
    }
}

impl<'de> Deserialize<'de> for UserAgent {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

/// The built-in identity's user agent, checked at compile time.
const DEFAULT_IDENTITY_USER_AGENT: UserAgent = UserAgent::from_static(DEFAULT_USER_AGENT);

/// One brand in `Sec-CH-UA` or `navigator.userAgentData.brands`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Brand {
    /// The brand name, e.g. `Chromium`.
    pub brand: String,
    /// Its major version, e.g. `152`.
    pub version: String,
}

impl Brand {
    fn new(brand: &str, version: &str) -> Self {
        Self {
            brand: brand.to_owned(),
            version: version.to_owned(),
        }
    }
}

/// One browser identity: the `User-Agent` string and every value a page, a
/// request, a report, or CDP can observe from it.
///
/// The fields mirror Chromium's `UserAgentMetadata`
/// (<https://chromium.googlesource.com/chromium/src/+/main/third_party/blink/public/common/user_agent/user_agent_metadata.h>)
/// plus the `navigator` product strings, so a page's low- and high-entropy
/// hints, its `Navigator.platform`, and the CDP product all name the same
/// product.
///
/// This is the identity as configuration and as the wire carries it. The
/// values derived from it — the three client-hint field values and the
/// `Accept-Language` list — are formatted once by [`ValidIdentity`], which is
/// the type every request and page boundary takes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    /// The `User-Agent` request header.
    pub user_agent: UserAgent,
    /// Low-entropy brands, `Sec-CH-UA` and `navigator.userAgentData.brands`.
    pub brands: Vec<Brand>,
    /// High-entropy `navigator.userAgentData.getHighEntropyValues` list.
    pub full_version_list: Vec<Brand>,
    /// The CDP product token, e.g. `Chrome/152.0.7977.82`.
    pub product: String,
    /// `Sec-CH-UA-Platform`, without the quoted-string form, e.g. `Linux`.
    pub platform: String,
    /// The high-entropy `platformVersion` hint.
    pub platform_version: String,
    /// The high-entropy `architecture` hint.
    pub architecture: String,
    /// The high-entropy `model` hint.
    pub model: String,
    /// `Sec-CH-UA-Mobile`.
    pub mobile: bool,
    /// The high-entropy `bitness` hint.
    pub bitness: String,
    /// The high-entropy `wow64` hint.
    pub wow64: bool,
    /// The high-entropy `formFactors` hint.
    pub form_factors: Vec<String>,
    /// The deprecated `Navigator.platform` string, e.g. `Linux x86_64`. It is
    /// not a client hint; the page still reads it.
    pub navigator_platform: String,
    /// `navigator.languages`, most preferred first: the first tag is
    /// `navigator.language` and the list becomes `Accept-Language`.
    pub languages: Vec<String>,
    /// `navigator.vendor`.
    pub vendor: String,
    /// `navigator.productSub`.
    pub product_sub: String,
}

impl Default for Identity {
    fn default() -> Self {
        // The brand versions are the full version's major, so a build bump is
        // one edit. The reduced UA repeats the major because it is a `const`
        // string; `the_default_identity_names_one_build_everywhere` holds the
        // two together.
        let major = DEFAULT_FULL_VERSION
            .split_once('.')
            .map_or(DEFAULT_FULL_VERSION, |(major, _)| major);
        Self {
            user_agent: DEFAULT_IDENTITY_USER_AGENT,
            brands: vec![
                Brand::new("Not_A Brand", "99"),
                Brand::new("Chromium", major),
                Brand::new("Google Chrome", major),
            ],
            full_version_list: vec![
                Brand::new("Not_A Brand", "99.0.0.0"),
                Brand::new("Chromium", DEFAULT_FULL_VERSION),
                Brand::new("Google Chrome", DEFAULT_FULL_VERSION),
            ],
            product: format!("Chrome/{DEFAULT_FULL_VERSION}"),
            platform: "Linux".to_owned(),
            platform_version: String::new(),
            architecture: "x86".to_owned(),
            model: String::new(),
            mobile: false,
            bitness: "64".to_owned(),
            wow64: false,
            form_factors: vec!["Desktop".to_owned()],
            navigator_platform: "Linux x86_64".to_owned(),
            languages: DEFAULT_LANGUAGES.map(str::to_owned).to_vec(),
            vendor: "Google Inc.".to_owned(),
            product_sub: "20030107".to_owned(),
        }
    }
}

impl Identity {
    /// The `Sec-CH-UA` field value: the brands in spec order, each as a
    /// quoted-string pair
    /// (<https://wicg.github.io/ua-client-hints/#sec-ch-ua>).
    #[must_use]
    pub fn sec_ch_ua(&self) -> String {
        self.brands
            .iter()
            .map(|brand| {
                format!(
                    "\"{}\";v=\"{}\"",
                    quote(&brand.brand),
                    quote(&brand.version)
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// The `Sec-CH-UA-Mobile` field value.
    #[must_use]
    pub fn sec_ch_ua_mobile(&self) -> &'static str {
        if self.mobile { "?1" } else { "?0" }
    }

    /// The `Sec-CH-UA-Platform` field value, quoted.
    #[must_use]
    pub fn sec_ch_ua_platform(&self) -> String {
        format!("\"{}\"", quote(&self.platform))
    }
}

/// Why an [`Identity`] cannot carry a request or a page.
#[derive(Debug, thiserror::Error)]
pub enum InvalidIdentity {
    /// No language is configured, so `navigator.language` and
    /// `Accept-Language` would have nothing to report.
    #[error("an identity needs at least one language")]
    NoLanguages,
    /// A language is not one tag both `navigator.languages` and
    /// `Accept-Language` can carry, so the two would list different things.
    #[error("not a language tag: {0:?}")]
    Language(String),
    /// A brand or the platform is not a valid HTTP field value.
    #[error(transparent)]
    Header(#[from] HeaderError),
}

/// An [`Identity`] whose derived request values all formatted successfully.
///
/// The three client-hint field values and the `Accept-Language` list are
/// formatted once, when the identity crosses this boundary, so no later reader
/// can meet a brand that is not a field value and quietly skip a header.
///
/// [`AgentOptions`](crate::AgentOptions), the browser's network context, and
/// the renderer's handshake take this type, so an unusable identity cannot
/// reach a request or a page. Serde validates on the way in, which is how a
/// handshake carrying a broken brand fails instead of dropping the hints.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidIdentity {
    identity: Identity,
    hints: ClientHints,
    accept_language: HeaderValue,
}

impl ValidIdentity {
    /// The three `Sec-CH-UA*` field values.
    #[must_use]
    pub fn hints(&self) -> &ClientHints {
        &self.hints
    }

    /// The `Accept-Language` field value derived from
    /// [`Identity::languages`].
    #[must_use]
    pub fn accept_language(&self) -> &HeaderValue {
        &self.accept_language
    }
}

impl std::ops::Deref for ValidIdentity {
    type Target = Identity;

    fn deref(&self) -> &Identity {
        &self.identity
    }
}

impl TryFrom<Identity> for ValidIdentity {
    type Error = InvalidIdentity;

    fn try_from(identity: Identity) -> Result<Self, InvalidIdentity> {
        Ok(Self {
            hints: ClientHints::build(&identity)?,
            accept_language: accept_language(&identity.languages)?,
            identity,
        })
    }
}

impl Default for ValidIdentity {
    fn default() -> Self {
        // The built-in identity is formatted once per process at most; its
        // validity is an invariant of `Identity::default`, and every test and
        // caller that builds one exercises it.
        Self::try_from(Identity::default()).expect("the built-in identity is valid")
    }
}

impl Serialize for ValidIdentity {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.identity.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ValidIdentity {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::try_from(Identity::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// The `Accept-Language` field value for `languages`.
///
/// Chromium's q-values: the first language carries `q=1` implicitly, then the
/// quality drops by 0.1 per language and stays at 0.1
/// (<https://source.chromium.org/chromium/chromium/src/+/main:net/http/http_util.cc>).
/// Duplicates are dropped, so a repeated tag does not shift every later
/// quality.
fn accept_language(languages: &[String]) -> Result<HeaderValue, InvalidIdentity> {
    let mut value = String::new();
    let mut seen: Vec<&str> = Vec::new();
    for language in languages {
        if !is_language_tag(language) {
            return Err(InvalidIdentity::Language(language.clone()));
        }
        if seen.contains(&language.as_str()) {
            continue;
        }
        seen.push(language);
        if seen.len() == 1 {
            value.push_str(language);
        } else {
            let quality = 11usize.saturating_sub(seen.len()).max(1);
            write!(value, ",{language};q=0.{quality}")
                .expect("writing a language into a String cannot fail");
        }
    }
    if value.is_empty() {
        return Err(InvalidIdentity::NoLanguages);
    }
    HeaderValue::from_str(&value).map_err(|_| HeaderError::InvalidValue(value.into()).into())
}

/// Whether `tag` is one `navigator.languages` entry and one `Accept-Language`
/// list element at the same time: non-empty, and only ASCII alphanumerics and
/// `-`.
///
/// Anything else either cannot be a field value or would split the list a page
/// reads (a `,`, a `;`, a space), so the two readers would disagree about which
/// languages the identity prefers.
fn is_language_tag(tag: &str) -> bool {
    !tag.is_empty()
        && tag
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

/// The `Sec-CH-UA`, `Sec-CH-UA-Mobile`, and `Sec-CH-UA-Platform` values, ready
/// to be written onto a request.
///
/// Built once per identity: a hop only clones three validated `HeaderValue`s
/// instead of formatting the brands again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientHints {
    ua: HeaderValue,
    mobile: HeaderValue,
    platform: HeaderValue,
}

impl ClientHints {
    /// Formats the three values once, refusing a brand or platform that is not
    /// a field value.
    fn build(identity: &Identity) -> Result<Self, HeaderError> {
        let field = |value: String| {
            HeaderValue::from_str(&value).map_err(|_| HeaderError::InvalidValue(value.into()))
        };
        Ok(Self {
            ua: field(identity.sec_ch_ua())?,
            mobile: field(identity.sec_ch_ua_mobile().to_owned())?,
            platform: field(identity.sec_ch_ua_platform())?,
        })
    }

    /// Writes the hints onto one request, when `url` is potentially
    /// trustworthy: a network attacker must not learn the exact build
    /// (<https://wicg.github.io/ua-client-hints/#sec-ch-ua>).
    pub fn write(&self, headers: &mut crate::HeaderMap, url: &Url) {
        if !secure_context::is_potentially_trustworthy(url) {
            return;
        }
        headers.insert_wire(HeaderName::from_static("sec-ch-ua"), self.ua.clone());
        headers.insert_wire(
            HeaderName::from_static("sec-ch-ua-mobile"),
            self.mobile.clone(),
        );
        headers.insert_wire(
            HeaderName::from_static("sec-ch-ua-platform"),
            self.platform.clone(),
        );
    }
}

/// Escapes a brand or platform for a structured-field string.
fn quote(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len());
    for character in value.chars() {
        if character == '"' || character == '\\' {
            quoted.push('\\');
        }
        quoted.push(character);
    }
    quoted
}

#[cfg(test)]
mod tests {
    use super::{
        Brand, ClientHints, Identity, InvalidIdentity, UserAgent, ValidIdentity, accept_language,
    };
    use url::Url;

    #[test]
    fn the_default_identity_serializes_its_brands_as_the_header() {
        let identity = Identity::default();
        assert_eq!(
            identity.sec_ch_ua(),
            r#""Not_A Brand";v="99", "Chromium";v="152", "Google Chrome";v="152""#
        );
        assert_eq!(identity.sec_ch_ua_mobile(), "?0");
        assert_eq!(identity.sec_ch_ua_platform(), r#""Linux""#);
    }

    /// The reduced UA, the brands, the full version list, and the CDP product
    /// name one build.
    #[test]
    fn the_default_identity_names_one_build_everywhere() {
        let identity = Identity::default();
        assert!(
            identity.user_agent.as_str().contains("Chrome/152.0.0.0"),
            "the reduced UA carries the major version"
        );
        let chromium = identity
            .full_version_list
            .iter()
            .find(|brand| brand.brand == "Chromium")
            .expect("a Chromium brand");
        assert!(
            chromium.version.starts_with("152."),
            "the full version agrees with the UA major"
        );
        assert_eq!(identity.product, format!("Chrome/{}", chromium.version));
    }

    #[test]
    fn a_brand_or_platform_cannot_break_out_of_its_quoted_string() {
        let identity = Identity {
            brands: vec![Brand {
                brand: "a\"b\\c".to_owned(),
                version: "1".to_owned(),
            }],
            platform: "x\"y".to_owned(),
            ..Identity::default()
        };
        assert_eq!(identity.sec_ch_ua(), r#""a\"b\\c";v="1""#);
        assert_eq!(identity.sec_ch_ua_platform(), r#""x\"y""#);
    }

    #[test]
    fn client_hints_reach_only_a_trustworthy_url() {
        let identity = ValidIdentity::default();
        let hints = identity.hints();
        let mut secure = crate::HeaderMap::new();
        hints.write(&mut secure, &url("https://example.test/"));
        assert_eq!(
            secure.get("sec-ch-ua"),
            Some(identity.sec_ch_ua().as_bytes()),
            "the brands reach an https request"
        );
        assert_eq!(secure.get("sec-ch-ua-mobile"), Some(b"?0".as_slice()));
        assert_eq!(
            secure.get("sec-ch-ua-platform"),
            Some(br#""Linux""#.as_slice())
        );

        let mut insecure = crate::HeaderMap::new();
        hints.write(&mut insecure, &url("http://example.test/"));
        assert!(
            insecure.get("sec-ch-ua").is_none(),
            "a network attacker must not learn the exact build"
        );
    }

    /// The hints are field values, so a control byte in a brand is refused
    /// where the identity becomes valid, not on a request mid-page.
    #[test]
    fn an_identity_with_a_broken_brand_cannot_become_valid() {
        let identity = Identity {
            brands: vec![Brand {
                brand: "bad\nbrand".to_owned(),
                version: "1".to_owned(),
            }],
            ..Identity::default()
        };
        assert!(matches!(
            ValidIdentity::try_from(identity),
            Err(InvalidIdentity::Header(_))
        ));
        assert!(ClientHints::build(&Identity::default()).is_ok());
    }

    /// A repeated tag is dropped and the qualities still descend from the
    /// first occurrence, as Chromium's builder does.
    #[test]
    fn accept_language_counts_each_language_once() {
        let languages = ["en-US", "en", "en-US", "fr"].map(str::to_owned).to_vec();
        assert_eq!(
            accept_language(&languages).expect("field value"),
            "en-US,en;q=0.9,fr;q=0.8"
        );
    }

    /// Past the tenth language the quality stays at 0.1 instead of reaching
    /// `q=0`, which would ask for nothing.
    #[test]
    fn accept_language_never_asks_for_zero_quality() {
        let languages = (0..12).map(|index| format!("l{index}")).collect::<Vec<_>>();
        let header = accept_language(&languages).expect("field value");
        let header = header.to_str().expect("visible ASCII");
        assert!(header.ends_with(",l10;q=0.1,l11;q=0.1"), "{header}");
    }

    #[test]
    fn an_identity_needs_a_language() {
        let identity = Identity {
            languages: Vec::new(),
            ..Identity::default()
        };
        assert!(matches!(
            ValidIdentity::try_from(identity),
            Err(InvalidIdentity::NoLanguages)
        ));
    }

    /// A tag that would split the `Accept-Language` list, or empty the page's
    /// list, is refused instead of making the two readers disagree.
    #[test]
    fn a_language_must_be_one_tag() {
        for tag in ["", "en;q=0", "en,fr", "en fr", "*"] {
            let languages = vec![tag.to_owned()];
            assert!(
                matches!(
                    accept_language(&languages),
                    Err(InvalidIdentity::Language(bad)) if bad == tag
                ),
                "{tag:?} is not one language tag"
            );
        }
        assert_eq!(
            accept_language(&["sr-Latn-RS".to_owned()]).expect("a BCP 47 tag"),
            "sr-Latn-RS"
        );
    }

    /// Serde is the second boundary that builds the derived values, so a
    /// handshake carrying a broken brand fails instead of dropping the hints.
    #[test]
    fn a_decoded_identity_is_validated() {
        let mut identity = Identity::default();
        identity.brands[0].brand = "bad\nbrand".to_owned();
        let bytes = postcard::to_allocvec(&identity).expect("encode");
        assert!(postcard::from_bytes::<ValidIdentity>(&bytes).is_err());

        let bytes = postcard::to_allocvec(&Identity::default()).expect("encode");
        let decoded = postcard::from_bytes::<ValidIdentity>(&bytes).expect("valid identity");
        assert_eq!(decoded, ValidIdentity::default());
    }

    fn url(value: &str) -> Url {
        Url::parse(value).expect("url")
    }

    #[test]
    fn a_user_agent_is_validated_when_it_is_parsed() {
        assert!(UserAgent::parse("probe/1").is_ok());
        assert!(
            UserAgent::parse("bad\nvalue").is_err(),
            "a field value cannot carry a newline"
        );
        assert!(
            UserAgent::parse("Mozilla caf\u{e9}").is_err(),
            "obs-text is a field value but not visible ASCII, so `as_str` would be empty"
        );
    }
}
