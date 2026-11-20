// https://wicg.github.io/ua-client-hints/#http-ua-hints

use std::fmt::Write as _;

use http::header::{HeaderName, HeaderValue};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use url::Url;

use crate::HeaderError;

pub const DEFAULT_USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/152.0.0.0 Safari/537.36";

const DEFAULT_FULL_VERSION: &str = "152.0.7977.82";

const DEFAULT_LANGUAGES: [&str; 2] = ["en-US", "en"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserAgent(HeaderValue);

impl UserAgent {
    #[must_use]
    pub const fn from_static(value: &'static str) -> Self {
        Self(HeaderValue::from_static(value))
    }

    pub fn parse(value: &str) -> Result<Self, HeaderError> {
        let header =
            HeaderValue::from_str(value).map_err(|_| HeaderError::InvalidValue(value.into()))?;
        header
            .to_str()
            .map_err(|_| HeaderError::InvalidValue(value.into()))?;
        Ok(Self(header))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.to_str().unwrap_or_default()
    }

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

const DEFAULT_IDENTITY_USER_AGENT: UserAgent = UserAgent::from_static(DEFAULT_USER_AGENT);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Brand {
    pub brand: String,
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

// https://chromium.googlesource.com/chromium/src/+/main/third_party/blink/public/common/user_agent/user_agent_metadata.h
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub user_agent: UserAgent,
    pub brands: Vec<Brand>,
    pub full_version_list: Vec<Brand>,
    pub product: String,
    pub platform: String,
    pub platform_version: String,
    pub architecture: String,
    pub model: String,
    pub mobile: bool,
    pub bitness: String,
    pub wow64: bool,
    pub form_factors: Vec<String>,
    pub navigator_platform: String,
    pub languages: Vec<String>,
    pub vendor: String,
    pub product_sub: String,
}

impl Default for Identity {
    fn default() -> Self {
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
    // https://wicg.github.io/ua-client-hints/#sec-ch-ua
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

    #[must_use]
    pub fn sec_ch_ua_mobile(&self) -> &'static str {
        if self.mobile { "?1" } else { "?0" }
    }

    #[must_use]
    pub fn sec_ch_ua_platform(&self) -> String {
        format!("\"{}\"", quote(&self.platform))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum InvalidIdentity {
    #[error("an identity needs at least one language")]
    NoLanguages,
    #[error("not a language tag: {0:?}")]
    Language(String),
    #[error(transparent)]
    Header(#[from] HeaderError),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidIdentity {
    identity: Identity,
    hints: ClientHints,
    accept_language: HeaderValue,
}

impl ValidIdentity {
    #[must_use]
    pub fn hints(&self) -> &ClientHints {
        &self.hints
    }

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

// https://source.chromium.org/chromium/chromium/src/+/main:net/http/http_util.cc
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

fn is_language_tag(tag: &str) -> bool {
    !tag.is_empty()
        && tag
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientHints {
    ua: HeaderValue,
    mobile: HeaderValue,
    platform: HeaderValue,
}

impl ClientHints {
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

    // https://wicg.github.io/ua-client-hints/#sec-ch-ua
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

    #[test]
    fn accept_language_counts_each_language_once() {
        let languages = ["en-US", "en", "en-US", "fr"].map(str::to_owned).to_vec();
        assert_eq!(
            accept_language(&languages).expect("field value"),
            "en-US,en;q=0.9,fr;q=0.8"
        );
    }

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
