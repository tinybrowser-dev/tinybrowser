use std::fmt;

use http::header::{HeaderName, HeaderValue};

// https://fetch.spec.whatwg.org/#concept-header-list-combine
// https://fetch.spec.whatwg.org/#example-header-value-get-decode-and-split
#[must_use]
pub fn combined_header<'a>(values: impl Iterator<Item = &'a str>) -> Option<String> {
    let values: Vec<&str> = values
        .map(|value| value.trim_matches([' ', '\t']))
        .collect();
    if values.is_empty() {
        None
    } else {
        Some(values.join(", "))
    }
}

// https://fetch.spec.whatwg.org/#concept-header-list-get
#[must_use]
pub fn combined_header_in(headers: &[(String, String)], name: &str) -> Option<String> {
    combined_header(
        headers
            .iter()
            .filter(|(header, _)| header.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str()),
    )
}

// https://fetch.spec.whatwg.org/#header-value-get-decode-and-split
// https://infra.spec.whatwg.org/#isomorphic-decode
#[must_use]
pub fn split_header_value(value: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut member = String::new();
    let mut characters = value.chars();
    while let Some(character) = characters.next() {
        match character {
            ',' => {
                values.push(member.trim_matches([' ', '\t']).to_owned());
                member.clear();
            }
            '"' => {
                member.push(character);
                collect_quoted_string(&mut characters, &mut member);
            }
            _ => member.push(character),
        }
    }
    values.push(member.trim_matches([' ', '\t']).to_owned());
    values
}

// https://fetch.spec.whatwg.org/#collect-an-http-quoted-string
fn collect_quoted_string(characters: &mut std::str::Chars<'_>, member: &mut String) {
    while let Some(character) = characters.next() {
        member.push(character);
        match character {
            '"' => return,
            '\\' => {
                if let Some(escaped) = characters.next() {
                    member.push(escaped);
                }
            }
            _ => {}
        }
    }
}

// https://webidl.spec.whatwg.org/#es-ByteString
// https://infra.spec.whatwg.org/#isomorphic-encode
#[must_use]
pub fn isomorphic_encode(value: &str) -> Option<Vec<u8>> {
    value
        .chars()
        .map(|character| u8::try_from(character).ok())
        .collect()
}

// https://infra.spec.whatwg.org/#isomorphic-decode
#[must_use]
pub fn isomorphic_decode(bytes: &[u8]) -> String {
    bytes.iter().map(|&byte| char::from(byte)).collect()
}

// https://fetch.spec.whatwg.org/#header-value-get-decode-and-split
#[must_use]
pub fn get_decode_split(headers: &HeaderMap, name: &str) -> Option<Vec<String>> {
    let mut combined = Vec::new();
    let mut found = false;
    for value in headers.get_all(name) {
        if found {
            combined.extend_from_slice(b", ");
        }
        combined.extend_from_slice(value);
        found = true;
    }
    found.then(|| split_header_value(&isomorphic_decode(&combined)))
}

// https://fetch.spec.whatwg.org/#header-value-get-decode-and-split
#[must_use]
pub fn get_decode_split_decoded(headers: &[(String, String)], name: &str) -> Option<Vec<String>> {
    combined_header_in(headers, name).map(|value| split_header_value(&value))
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum HeaderError {
    #[error("invalid header name: {0:?}")]
    InvalidName(Box<str>),
    #[error("invalid header value: {0:?}")]
    InvalidValue(Box<str>),
}

#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub struct HeaderMap {
    entries: Vec<(HeaderName, HeaderValue)>,
}

impl HeaderMap {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    // https://httpwg.org/specs/rfc9110.html#field.names
    pub fn insert(&mut self, name: &str, value: impl Into<Vec<u8>>) -> Result<(), HeaderError> {
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| HeaderError::InvalidName(name.into()))?;
        let value = value.into();
        let value = HeaderValue::from_bytes(&value).map_err(|_| {
            HeaderError::InvalidValue(String::from_utf8_lossy(&value).into_owned().into())
        })?;
        self.entries.push((name, value));
        Ok(())
    }

    pub(crate) fn append(&mut self, name: HeaderName, value: HeaderValue) {
        self.entries.push((name, value));
    }

    pub(crate) fn set(&mut self, name: HeaderName, value: HeaderValue) {
        let mut replacement = Some(value);
        self.entries.retain_mut(|(existing_name, existing_value)| {
            if existing_name != &name {
                return true;
            }
            match replacement.take() {
                Some(value) => {
                    *existing_name = name.clone();
                    *existing_value = value;
                    true
                }
                None => false,
            }
        });
        if let Some(value) = replacement {
            self.entries.push((name, value));
        }
    }

    pub(crate) fn insert_wire(&mut self, name: HeaderName, value: HeaderValue) {
        self.entries.push((name, value));
    }

    pub fn remove(&mut self, name: &str) {
        self.entries
            .retain(|(field, _)| !field.as_str().eq_ignore_ascii_case(name));
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&[u8]> {
        self.get_all(name).next()
    }

    pub(crate) fn value(&self, name: &str) -> Option<&HeaderValue> {
        self.entries
            .iter()
            .find(|(field, _)| field.as_str().eq_ignore_ascii_case(name))
            .map(|(_, value)| value)
    }

    pub fn get_all<'s>(&'s self, name: &str) -> impl Iterator<Item = &'s [u8]> {
        self.entries
            .iter()
            .filter(move |(field, _)| field.as_str().eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_bytes())
    }

    #[cfg(feature = "native")]
    pub(crate) fn typed_iter(&self) -> impl Iterator<Item = (&HeaderName, &HeaderValue)> {
        self.entries.iter().map(|(name, value)| (name, value))
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &[u8])> {
        self.entries
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_bytes()))
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("invalid HTTP method: {0:?}")]
pub struct InvalidMethod(Box<str>);

pub(crate) fn basic_authorization(username: &str, password: &str) -> String {
    use base64::Engine as _;
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"))
    )
}

// https://www.rfc-editor.org/rfc/rfc9110#section-9
// https://fetch.spec.whatwg.org/#methods
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Method(http::Method);

impl Method {
    pub const GET: Self = Self(http::Method::GET);
    pub const HEAD: Self = Self(http::Method::HEAD);
    pub const POST: Self = Self(http::Method::POST);
    pub const PUT: Self = Self(http::Method::PUT);
    pub const DELETE: Self = Self(http::Method::DELETE);
    pub const OPTIONS: Self = Self(http::Method::OPTIONS);
    pub const PATCH: Self = Self(http::Method::PATCH);

    // https://fetch.spec.whatwg.org/#methods
    pub fn parse(token: &str) -> Result<Self, InvalidMethod> {
        let standard = [
            Self::GET,
            Self::HEAD,
            Self::POST,
            Self::PUT,
            Self::DELETE,
            Self::OPTIONS,
        ]
        .into_iter()
        .find(|method| token.eq_ignore_ascii_case(method.as_str()));
        if let Some(method) = standard {
            return Ok(method);
        }
        http::Method::from_bytes(token.as_bytes())
            .map(Self)
            .map_err(|_| InvalidMethod(token.into()))
    }

    // https://www.rfc-editor.org/rfc/rfc9110#section-9.2.1
    #[must_use]
    pub fn is_safe(&self) -> bool {
        self.0.is_safe()
    }

    // https://www.rfc-editor.org/rfc/rfc9110#section-9.2.2
    #[must_use]
    pub fn is_idempotent(&self) -> bool {
        self.0.is_idempotent()
    }

    // https://fetch.spec.whatwg.org/#forbidden-method
    #[must_use]
    pub fn is_forbidden(&self) -> bool {
        ["CONNECT", "TRACE", "TRACK"]
            .iter()
            .any(|forbidden| self.0.as_str().eq_ignore_ascii_case(forbidden))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    #[must_use]
    pub(crate) fn as_http(&self) -> &http::Method {
        &self.0
    }
}

impl fmt::Display for Method {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl TryFrom<String> for Method {
    type Error = InvalidMethod;

    fn try_from(token: String) -> Result<Self, Self::Error> {
        Self::parse(&token)
    }
}

impl From<Method> for String {
    fn from(method: Method) -> Self {
        method.0.as_str().to_owned()
    }
}

#[cfg(test)]
mod tests {
    use http::header::{HeaderName, HeaderValue};

    use super::HeaderMap;

    #[test]
    fn insert_appends_and_set_replaces_in_place() {
        let mut map = HeaderMap::new();
        map.insert("x-one", "a").expect("valid field");
        map.insert("X-Two", "b").expect("valid field");
        map.insert("x-one", "c").expect("valid field");
        assert_eq!(
            map.iter().map(|(name, _)| name).collect::<Vec<_>>(),
            ["x-one", "x-two", "x-one"]
        );
        map.set(
            HeaderName::from_static("x-one"),
            HeaderValue::from_static("d"),
        );
        assert_eq!(
            map.iter().map(|(name, _)| name).collect::<Vec<_>>(),
            ["x-one", "x-two"]
        );
        assert_eq!(map.get_all("x-one").collect::<Vec<_>>(), [b"d"]);
    }

    #[test]
    fn insert_rejects_invalid_names_and_values() {
        let mut map = HeaderMap::new();
        assert!(map.insert("bad name", "v").is_err());
        assert!(map.insert("x", "bad\nvalue").is_err());
        assert!(map.is_empty());
    }

    #[test]
    fn splits_header_values_per_spec() {
        assert_eq!(super::split_header_value("nosniff,"), ["nosniff", ""]);
        assert_eq!(super::split_header_value(""), [""]);
        assert_eq!(
            super::split_header_value("text/html;\", x/x"),
            ["text/html;\", x/x"]
        );
        assert_eq!(
            super::split_header_value("x/x;test=\"hi\",y/y"),
            ["x/x;test=\"hi\"", "y/y"]
        );
        assert_eq!(
            super::split_header_value("x / x,,,1"),
            ["x / x", "", "", "1"]
        );
        assert_eq!(super::split_header_value("\"1,2\", 3"), ["\"1,2\"", "3"]);
        assert_eq!(super::split_header_value("\"a\\,b\",c"), ["\"a\\,b\"", "c"]);
    }

    #[test]
    fn joins_repeated_fields_before_splitting() {
        let mut map = HeaderMap::new();
        map.insert("x-list", "a").expect("valid field");
        map.insert("x-list", "b").expect("valid field");
        assert_eq!(
            super::get_decode_split(&map, "x-list"),
            Some(vec!["a".to_owned(), "b".to_owned()])
        );
        assert_eq!(super::get_decode_split(&map, "x-missing"), None);
    }

    // https://fetch.spec.whatwg.org/#example-header-value-get-decode-and-split
    #[test]
    fn combining_keeps_an_empty_field_value() {
        assert_eq!(super::combined_header(std::iter::empty()), None);
        assert_eq!(
            super::combined_header(["nosniff", ""].into_iter()).as_deref(),
            Some("nosniff, ")
        );
        assert_eq!(
            super::split_header_value("nosniff, "),
            ["nosniff".to_owned(), String::new()]
        );
    }
}
