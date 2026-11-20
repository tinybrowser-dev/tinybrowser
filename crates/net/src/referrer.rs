// https://w3c.github.io/webappsec-referrer-policy/

use enumerated::Enumerated;
use serde::{Deserialize, Serialize};
use url::{Origin, Url};

use secure_context::is_potentially_trustworthy;

enumerated::enumerated! {
    // https://w3c.github.io/webappsec-referrer-policy/#referrer-policies
    #[derive(Default, Serialize, Deserialize)]
    #[serde(rename_all = "kebab-case")]
    pub ReferrerPolicy {
        NoReferrer => "no-referrer",
        NoReferrerWhenDowngrade => "no-referrer-when-downgrade",
        SameOrigin => "same-origin",
        Origin => "origin",
        StrictOrigin => "strict-origin",
        OriginWhenCrossOrigin => "origin-when-cross-origin",
        #[default]
        StrictOriginWhenCrossOrigin => "strict-origin-when-cross-origin",
        UnsafeUrl => "unsafe-url",
    }
}

impl ReferrerPolicy {
    // https://w3c.github.io/webappsec-referrer-policy/#parse-referrer-policy-from-header
    #[must_use]
    pub fn from_header<'a>(values: impl IntoIterator<Item = &'a str>) -> Option<Self> {
        let mut policy = None;
        for value in values {
            for token in header_list_members(value)? {
                if let Some(parsed) = Self::parse(token) {
                    policy = Some(parsed);
                }
            }
        }
        policy
    }

    // https://html.spec.whatwg.org/multipage/semantics.html#meta-referrer
    #[must_use]
    pub fn from_meta(content: &str) -> Option<Self> {
        let token = content.trim();
        if token.eq_ignore_ascii_case("never") {
            return Some(Self::NoReferrer);
        }
        if token.eq_ignore_ascii_case("default") {
            return Some(Self::StrictOriginWhenCrossOrigin);
        }
        if token.eq_ignore_ascii_case("always") {
            return Some(Self::UnsafeUrl);
        }
        if token.eq_ignore_ascii_case("origin-when-crossorigin") {
            return Some(Self::OriginWhenCrossOrigin);
        }
        Self::parse(token)
    }
}

// https://www.rfc-editor.org/rfc/rfc9110#section-5.6.1
fn header_list_members(value: &str) -> Option<Vec<&str>> {
    let mut members = Vec::new();
    for member in value.split(',') {
        let member = member.trim_matches([' ', '\t']);
        if member.is_empty() {
            continue;
        }
        if !member
            .bytes()
            .all(|byte| byte.is_ascii_alphabetic() || byte == b'-')
        {
            return None;
        }
        members.push(member);
    }
    Some(members)
}

// https://fetch.spec.whatwg.org/#concept-request-referrer
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum Referrer {
    #[default]
    NoReferrer,
    Client(ReferrerClient),
    Url(Url),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReferrerClient {
    pub origin: Origin,
    pub url: Url,
}

// https://w3c.github.io/webappsec-referrer-policy/#determine-requests-referrer
#[must_use]
pub fn determine_referrer(
    policy: ReferrerPolicy,
    referrer: &Referrer,
    target: &Url,
) -> Option<Url> {
    let source = match referrer {
        Referrer::NoReferrer => return None,
        Referrer::Client(client) => {
            if matches!(client.origin, Origin::Opaque(_)) {
                return None;
            }
            &client.url
        }
        Referrer::Url(url) => url,
    };
    if !matches!(source.scheme(), "http" | "https") {
        return None;
    }
    let referrer_url = strip_url(source, false);
    let referrer_origin = strip_url(source, true);
    let referrer_url = if referrer_url.as_str().len() > 4096 {
        referrer_origin.clone()
    } else {
        referrer_url
    };
    let same_origin = referrer_url.origin() == target.origin();
    let source_trustworthy = is_potentially_trustworthy(&referrer_url);
    let target_trustworthy = is_potentially_trustworthy(target);
    let downgrade = source_trustworthy && !target_trustworthy;
    match policy {
        ReferrerPolicy::NoReferrer => None,
        ReferrerPolicy::Origin => Some(referrer_origin),
        ReferrerPolicy::UnsafeUrl => Some(referrer_url),
        ReferrerPolicy::StrictOrigin => (!downgrade).then_some(referrer_origin),
        ReferrerPolicy::StrictOriginWhenCrossOrigin => {
            if same_origin {
                Some(referrer_url)
            } else if downgrade {
                None
            } else {
                Some(referrer_origin)
            }
        }
        ReferrerPolicy::SameOrigin => same_origin.then_some(referrer_url),
        ReferrerPolicy::OriginWhenCrossOrigin => Some(if same_origin {
            referrer_url
        } else {
            referrer_origin
        }),
        ReferrerPolicy::NoReferrerWhenDowngrade => (!downgrade).then_some(referrer_url),
    }
}

// https://w3c.github.io/webappsec-referrer-policy/#strip-url
fn strip_url(url: &Url, origin_only: bool) -> Url {
    let mut stripped = url.clone();
    let _ = stripped.set_username("");
    let _ = stripped.set_password(None);
    stripped.set_fragment(None);
    if origin_only {
        stripped.set_path("");
        stripped.set_query(None);
    }
    stripped
}
