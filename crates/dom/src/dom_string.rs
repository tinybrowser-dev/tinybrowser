use std::borrow::Cow;
use std::fmt;

// https://infra.spec.whatwg.org/#string
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DomString(Repr);

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Repr {
    Empty,
    Utf8(String),
    Utf16(Vec<u16>),
}

impl Default for DomString {
    fn default() -> Self {
        Self(Repr::Empty)
    }
}

impl DomString {
    #[must_use]
    pub fn from_utf16(units: impl Into<Vec<u16>>) -> Self {
        let units = units.into();
        if units.is_empty() {
            return Self::default();
        }
        match String::from_utf16(&units) {
            Ok(text) => Self(Repr::Utf8(text)),
            Err(_) => Self(Repr::Utf16(units)),
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        matches!(self.0, Repr::Empty)
    }

    // https://infra.spec.whatwg.org/#javascript-string-convert
    #[must_use]
    pub fn has_unpaired_surrogate(&self) -> bool {
        matches!(self.0, Repr::Utf16(_))
    }

    #[must_use]
    pub fn len(&self) -> usize {
        match &self.0 {
            Repr::Empty => 0,
            Repr::Utf8(text) => text.encode_utf16().count(),
            Repr::Utf16(units) => units.len(),
        }
    }

    #[must_use]
    pub fn units(&self) -> Cow<'_, [u16]> {
        match &self.0 {
            Repr::Empty => Cow::Borrowed(&[]),
            Repr::Utf8(text) => Cow::Owned(text.encode_utf16().collect()),
            Repr::Utf16(units) => Cow::Borrowed(units),
        }
    }

    #[must_use]
    pub fn to_string_lossy(&self) -> Cow<'_, str> {
        match &self.0 {
            Repr::Empty => Cow::Borrowed(""),
            Repr::Utf8(text) => Cow::Borrowed(text),
            Repr::Utf16(units) => Cow::Owned(String::from_utf16_lossy(units)),
        }
    }

    pub fn push_str(&mut self, extra: &str) {
        if extra.is_empty() {
            return;
        }
        match &mut self.0 {
            Repr::Empty => self.0 = Repr::Utf8(extra.to_owned()),
            Repr::Utf8(text) => text.push_str(extra),
            Repr::Utf16(units) => units.extend(extra.encode_utf16()),
        }
    }

    pub fn push_dom(&mut self, extra: &Self) {
        match &extra.0 {
            Repr::Empty => {}
            Repr::Utf8(text) => self.push_str(text),
            Repr::Utf16(units) => match &mut self.0 {
                Repr::Empty => self.0 = Repr::Utf16(units.clone()),
                Repr::Utf8(text) => {
                    let mut merged: Vec<u16> = text.encode_utf16().collect();
                    merged.extend_from_slice(units);
                    self.0 = Self::from_utf16(merged).0;
                }
                Repr::Utf16(existing) => {
                    existing.extend_from_slice(units);
                    let merged = std::mem::take(existing);
                    self.0 = Self::from_utf16(merged).0;
                }
            },
        }
    }
    // https://infra.spec.whatwg.org/#strip-and-collapse-ascii-whitespace
    #[must_use]
    pub fn strip_and_collapse_ascii_whitespace(&self) -> Self {
        let mut units = Vec::new();
        let mut pending_space = false;
        for unit in self.units().iter().copied() {
            if matches!(unit, 0x09 | 0x0a | 0x0c | 0x0d | 0x20) {
                pending_space = !units.is_empty();
                continue;
            }
            if pending_space {
                units.push(0x20);
                pending_space = false;
            }
            units.push(unit);
        }
        Self::from_utf16(units)
    }
}

impl From<&str> for DomString {
    fn from(text: &str) -> Self {
        if text.is_empty() {
            Self::default()
        } else {
            Self(Repr::Utf8(text.to_owned()))
        }
    }
}

impl From<String> for DomString {
    fn from(text: String) -> Self {
        if text.is_empty() {
            Self::default()
        } else {
            Self(Repr::Utf8(text))
        }
    }
}

impl From<&String> for DomString {
    fn from(text: &String) -> Self {
        Self::from(text.as_str())
    }
}

impl From<Vec<u16>> for DomString {
    fn from(units: Vec<u16>) -> Self {
        Self::from_utf16(units)
    }
}

impl From<&[u16]> for DomString {
    fn from(units: &[u16]) -> Self {
        Self::from_utf16(units.to_vec())
    }
}

impl From<DomString> for String {
    fn from(value: DomString) -> Self {
        value.to_string_lossy().into_owned()
    }
}

impl fmt::Display for DomString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_string_lossy())
    }
}

#[cfg(test)]
mod tests {
    use super::DomString;

    #[test]
    fn normalizes_valid_utf16_to_utf8() {
        assert_eq!(
            DomString::from_utf16(vec![0x61, 0x62]),
            DomString::from("ab")
        );
        assert_eq!(DomString::from_utf16(Vec::new()), DomString::default());
        assert_eq!(
            DomString::from_utf16(vec![0xd83d, 0xde00]),
            DomString::from("\u{1f600}")
        );
    }

    #[test]
    fn preserves_unpaired_surrogates() {
        let value = DomString::from_utf16(vec![0x61, 0xd800, 0x62]);
        assert_eq!(value.len(), 3);
        assert_eq!(&*value.units(), &[0x61, 0xd800, 0x62]);
        assert_eq!(value.to_string_lossy(), "a\u{fffd}b");
        assert_eq!(value, DomString::from_utf16(vec![0x61, 0xd800, 0x62]));
        assert_ne!(value, DomString::from("ab"));
    }

    #[test]
    fn concatenation_keeps_code_units() {
        let mut value = DomString::from_utf16(vec![0x61, 0xd800]);
        value.push_str("b");
        assert_eq!(&*value.units(), &[0x61, 0xd800, 0x62]);
        let mut prefix = DomString::from_utf16(vec![0xd800]);
        prefix.push_dom(&DomString::from("x"));
        assert_eq!(&*prefix.units(), &[0xd800, 0x78]);
    }

    #[test]
    fn reports_unpaired_surrogates() {
        assert!(!DomString::from("ab").has_unpaired_surrogate());
        assert!(DomString::from_utf16(vec![0xd800]).has_unpaired_surrogate());
    }
}
