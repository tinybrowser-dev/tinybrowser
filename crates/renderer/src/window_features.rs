// https://html.spec.whatwg.org/multipage/nav-history-apis.html#concept-window-open-features-tokenize

use crate::protocol::OpenerPolicy;

// https://html.spec.whatwg.org/multipage/nav-history-apis.html#dfn-feature-separator
fn is_separator(byte: u8) -> bool {
    byte.is_ascii_whitespace() || byte == b'=' || byte == b','
}

// https://html.spec.whatwg.org/multipage/nav-history-apis.html#parse-a-boolean-feature
fn parse_boolean(value: &str) -> bool {
    if value.is_empty() || value.eq_ignore_ascii_case("yes") || value.eq_ignore_ascii_case("true") {
        return true;
    }
    let digits = value.trim_start_matches(|character: char| character.is_ascii_whitespace());
    let digits = digits.strip_prefix(['+', '-']).unwrap_or(digits);
    digits
        .bytes()
        .take_while(u8::is_ascii_digit)
        .any(|digit| digit != b'0')
}

// https://html.spec.whatwg.org/multipage/nav-history-apis.html#tokenize-the-features-argument
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct WindowFeatures {
    noopener: bool,
    noreferrer: bool,
}

impl WindowFeatures {
    // https://html.spec.whatwg.org/multipage/nav-history-apis.html#tokenize-the-features-argument
    pub(crate) fn parse(features: &str) -> Self {
        let mut parsed = Self::default();
        let bytes = features.as_bytes();
        let mut position = 0;
        while position < bytes.len() {
            while position < bytes.len() && is_separator(bytes[position]) {
                position += 1;
            }
            let name_start = position;
            while position < bytes.len() && !is_separator(bytes[position]) {
                position += 1;
            }
            let name = &features[name_start..position];
            while position < bytes.len() && bytes[position] != b'=' {
                if bytes[position] == b',' || !is_separator(bytes[position]) {
                    break;
                }
                position += 1;
            }
            let mut value = "";
            if position < bytes.len() && is_separator(bytes[position]) {
                while position < bytes.len() && is_separator(bytes[position]) {
                    if bytes[position] == b',' {
                        break;
                    }
                    position += 1;
                }
                let value_start = position;
                while position < bytes.len() && !is_separator(bytes[position]) {
                    position += 1;
                }
                value = &features[value_start..position];
            }
            if name.is_empty() {
                continue;
            }
            if name.eq_ignore_ascii_case("noopener") {
                parsed.noopener = parse_boolean(value);
            } else if name.eq_ignore_ascii_case("noreferrer") {
                parsed.noreferrer = parse_boolean(value);
            }
        }
        parsed
    }

    // https://html.spec.whatwg.org/multipage/nav-history-apis.html#get-noopener-for-window-open
    pub(crate) fn opener_policy(
        self,
        blob_origin: Option<&url::Origin>,
        top_level_origin: &url::Origin,
    ) -> OpenerPolicy {
        let blob_severs =
            blob_origin.is_some_and(|origin| !net::origins_same_site(origin, top_level_origin));
        if self.noreferrer {
            OpenerPolicy::NoReferrer
        } else if blob_severs || self.noopener {
            OpenerPolicy::NoOpener
        } else {
            OpenerPolicy::Keep
        }
    }
}

#[cfg(test)]
mod tests {
    use url::Url;

    use super::WindowFeatures;
    use crate::protocol::OpenerPolicy;

    fn origin(raw: &str) -> url::Origin {
        Url::parse(raw).expect("test URL").origin()
    }

    fn policy(features: &str) -> OpenerPolicy {
        WindowFeatures::parse(features).opener_policy(None, &origin("https://a.test/"))
    }

    // https://html.spec.whatwg.org/multipage/nav-history-apis.html#concept-window-open-features-tokenize
    #[test]
    fn a_window_feature_list_reads_the_way_the_standard_says() {
        assert_eq!(policy(""), OpenerPolicy::Keep);
        assert_eq!(policy("width=100,height=200"), OpenerPolicy::Keep);
        assert_eq!(policy("noopener"), OpenerPolicy::NoOpener);
        assert_eq!(policy("NOOPENER"), OpenerPolicy::NoOpener);
        assert_eq!(policy("noopener=1"), OpenerPolicy::NoOpener);
        assert_eq!(policy("noopener=yes"), OpenerPolicy::NoOpener);
        assert_eq!(policy("noopener=0"), OpenerPolicy::Keep);
        assert_eq!(policy("noopener=false"), OpenerPolicy::Keep);
        assert_eq!(policy("noopener=00x1"), OpenerPolicy::Keep);
        assert_eq!(policy("noopener=x1"), OpenerPolicy::Keep);
        assert_eq!(policy("noopener=-5"), OpenerPolicy::NoOpener);
        assert_eq!(policy("noopener noreferrer"), OpenerPolicy::NoReferrer);
        assert_eq!(policy("width=100 noopener"), OpenerPolicy::NoOpener);
        assert_eq!(policy("popup,noopener=1"), OpenerPolicy::NoOpener);
        assert_eq!(policy("noreferrer=0"), OpenerPolicy::Keep);
        assert_eq!(policy("noopener=0,noreferrer"), OpenerPolicy::NoReferrer);
        assert_eq!(
            policy("noopener=0,noopener=1"),
            OpenerPolicy::NoOpener,
            "a repeated feature keeps its last value"
        );
    }

    // https://html.spec.whatwg.org/multipage/nav-history-apis.html#get-noopener-for-window-open
    #[test]
    fn a_blob_from_another_site_severs_the_opener() {
        let top = origin("https://a.test/");
        let cross_site = origin("https://b.test/");
        let same_site = origin("https://sub.a.test/");
        assert_eq!(
            WindowFeatures::parse("").opener_policy(Some(&cross_site), &top),
            OpenerPolicy::NoOpener
        );
        assert_eq!(
            WindowFeatures::parse("noopener=0").opener_policy(Some(&cross_site), &top),
            OpenerPolicy::NoOpener
        );
        assert_eq!(
            WindowFeatures::parse("").opener_policy(Some(&same_site), &top),
            OpenerPolicy::Keep
        );
        assert_eq!(
            WindowFeatures::parse("noreferrer").opener_policy(Some(&cross_site), &top),
            OpenerPolicy::NoReferrer
        );
    }
}
