//! DOM strings: ordered sequences of UTF-16 code units.

use std::borrow::Cow;
use std::fmt;

/// A DOM string, a sequence of UTF-16 code units
/// (<https://infra.spec.whatwg.org/#string>).
///
/// JavaScript strings and DOM character data may contain unpaired surrogates,
/// which a Rust `String` cannot hold. A `DomString` stores the all-Unicode
/// case as UTF-8 and promotes to a UTF-16 buffer only when an unpaired
/// surrogate requires it, so the common case stays a plain `String` while
/// every code-unit sequence round-trips exactly.
///
/// The representation is private, so a `DomString` is always one of the two
/// canonical forms. Two values are equal when their code units are equal,
/// whichever form each chose.
///
/// ```
/// use renderer::dom_string::DomString;
/// // Ordinary Unicode compacts to a Rust string.
/// assert_eq!(DomString::from("\u{1f600}"), DomString::from_utf16(vec![0xd83d, 0xde00]));
/// // An unpaired surrogate is preserved.
/// assert_eq!(&*DomString::from_utf16(vec![0xd800]).units(), &[0xd800]);
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DomString(Repr);

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Repr {
    /// The empty string.
    Empty,
    /// Code units that form valid UTF-8.
    Utf8(String),
    /// Code units that include at least one unpaired surrogate.
    Utf16(Vec<u16>),
}

impl Default for DomString {
    fn default() -> Self {
        Self(Repr::Empty)
    }
}

impl DomString {
    /// Wraps a UTF-16 buffer, normalizing it to the compact form when it
    /// contains no unpaired surrogate.
    ///
    /// For example, `[0x61, 0x62]` becomes the UTF-8 string `"ab"`, while
    /// `[0xd800]` stays a UTF-16 buffer.
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

    /// Whether the code-unit sequence is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        matches!(self.0, Repr::Empty)
    }

    /// Whether the sequence contains an unpaired surrogate.
    ///
    /// The UTF-8 tree cannot hold these, so they read back as U+FFFD
    /// (see `docs/progress.md`,
    /// <https://infra.spec.whatwg.org/#javascript-string-convert>).
    #[must_use]
    pub fn has_unpaired_surrogate(&self) -> bool {
        matches!(self.0, Repr::Utf16(_))
    }

    /// The length in UTF-16 code units.
    #[must_use]
    pub fn len(&self) -> usize {
        match &self.0 {
            Repr::Empty => 0,
            Repr::Utf8(text) => text.encode_utf16().count(),
            Repr::Utf16(units) => units.len(),
        }
    }

    /// The code units, borrowing the stored buffer where possible.
    #[must_use]
    pub fn units(&self) -> Cow<'_, [u16]> {
        match &self.0 {
            Repr::Empty => Cow::Borrowed(&[]),
            Repr::Utf8(text) => Cow::Owned(text.encode_utf16().collect()),
            Repr::Utf16(units) => Cow::Borrowed(units),
        }
    }

    /// The code units as a UTF-8 string, replacing each unpaired surrogate
    /// with U+FFFD.
    ///
    /// Features that need text as UTF-8 (serialization text, layout, selector
    /// matching) use this; features that must round-trip exact code units use
    /// [`DomString::units`].
    #[must_use]
    pub fn to_string_lossy(&self) -> Cow<'_, str> {
        match &self.0 {
            Repr::Empty => Cow::Borrowed(""),
            Repr::Utf8(text) => Cow::Borrowed(text),
            Repr::Utf16(units) => Cow::Owned(String::from_utf16_lossy(units)),
        }
    }

    /// Appends a UTF-8 string's code units.
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

    /// Appends another DOM string's code units.
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
    /// Strips leading and trailing ASCII whitespace and collapses each
    /// remaining run to one U+0020 SPACE
    /// (<https://infra.spec.whatwg.org/#strip-and-collapse-ascii-whitespace>).
    ///
    /// Operates on code units so an unpaired surrogate survives unchanged.
    pub(crate) fn strip_and_collapse_ascii_whitespace(&self) -> Self {
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
