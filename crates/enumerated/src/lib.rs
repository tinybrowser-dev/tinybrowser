pub trait Enumerated: Copy + 'static {
    const KEYWORDS: &'static [Self];
    const KEYWORD_STRS: &'static [&'static str];

    fn keyword(self) -> &'static str;

    fn parse(value: &str) -> Option<Self> {
        Self::KEYWORDS
            .iter()
            .copied()
            .find(|candidate| candidate.keyword().eq_ignore_ascii_case(value))
    }
}

#[doc(hidden)]
#[must_use]
pub const fn keywords_equal(left: &str, right: &str) -> bool {
    let left = left.as_bytes();
    let right = right.as_bytes();
    if left.len() != right.len() {
        return false;
    }
    let mut index = 0;
    while index < left.len() {
        if !left[index].eq_ignore_ascii_case(&right[index]) {
            return false;
        }
        index += 1;
    }
    true
}

#[macro_export]
macro_rules! enumerated {
    (
        $(#[$doc:meta])*
        $vis:vis $name:ident {
            $(
                $(#[$variant_doc:meta])*
                $variant:ident => $keyword:literal
            ),+ $(,)?
        }
    ) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        $vis enum $name {
            $(
                $(#[$variant_doc])*
                $variant,
            )+
        }

        impl $name {
            #[must_use]
            pub const fn as_keyword(self) -> &'static str {
                match self {
                    $(Self::$variant => $keyword),+
                }
            }
        }

        impl $crate::Enumerated for $name {
            const KEYWORDS: &'static [Self] = &[$(Self::$variant),+];
            const KEYWORD_STRS: &'static [&'static str] = &[$($keyword),+];

            fn keyword(self) -> &'static str {
                self.as_keyword()
            }
        }

        const _: () = {
            let keywords = [$($keyword),+];
            let mut left = 0;
            while left < keywords.len() {
                let mut right = left + 1;
                while right < keywords.len() {
                    assert!(
                        !$crate::keywords_equal(keywords[left], keywords[right]),
                        "an enumerated set spells one keyword twice"
                    );
                    right += 1;
                }
                left += 1;
            }
        };
    };
}

#[cfg(test)]
mod tests {
    use super::{Enumerated, keywords_equal};

    enumerated! {
        #[derive(Default)]
        TestSet {
            #[default]
            First => "first",
            Second => "second",
        }
    }

    #[test]
    fn parse_is_ascii_case_insensitive_and_untrimmed() {
        assert_eq!(TestSet::parse("FIRST"), Some(TestSet::First));
        assert_eq!(TestSet::parse(" first"), None);
        assert_eq!(TestSet::parse("first "), None);
        assert_eq!(TestSet::parse(""), None);
    }

    #[test]
    fn every_keyword_parses_back_to_its_variant() {
        for value in TestSet::KEYWORDS {
            assert_eq!(TestSet::parse(value.keyword()), Some(*value));
        }
    }

    #[test]
    fn a_keyword_comparison_ignores_ascii_case_only() {
        assert!(keywords_equal("first", "FIRST"));
        assert!(!keywords_equal("first", "first "));
        assert!(!keywords_equal("first", "firs"));
    }
}
