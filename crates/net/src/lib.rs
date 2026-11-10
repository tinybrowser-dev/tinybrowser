//! Blocking HTTP client for the page engine.
//!
//! Types in this crate are the public network surface. Behavior follows
//! [Fetch](https://fetch.spec.whatwg.org/).

mod client;
pub mod cors;
mod error;
mod identity;
pub mod orb;
mod protocol;
pub mod referrer;
mod resolve;
mod transport;

pub use client::{Agent, AgentOptions, Body, Clock, ClockSource, Request, Response};
pub use cookies::{
    CookieRecord, CookieSameSite, InitiatorKind, SameSiteCookiePolicy, origins_same_site, site,
    site_for_origin,
};
pub use error::{NetError, TimeoutKind, TransportError};
pub use identity::{
    Brand, ClientHints, DEFAULT_USER_AGENT, Identity, InvalidIdentity, UserAgent, ValidIdentity,
};
pub use protocol::{
    HeaderError, HeaderMap, InvalidMethod, Method, combined_header, combined_header_in,
    get_decode_split, get_decode_split_decoded, isomorphic_decode, isomorphic_encode,
    split_header_value,
};
pub use referrer::{Referrer, ReferrerClient, ReferrerPolicy, determine_referrer};
pub use resolve::{InvalidResolveRule, ResolveRule};
pub use transport::{
    HopBudget, InvalidCa, InvalidProxy, Proxy, Transport, TrustedCa, WireBody, WireRequest,
    WireResponse,
};

/// The URL a tuple origin serializes to; `None` for an opaque origin.
///
/// The serialization of a tuple origin is always a valid URL, so the `expect`
/// lives here instead of at every reader: `SerializedOrigin::to_url`, redirect
/// reporting, and the site lock all ask this one function.
///
/// # Panics
///
/// Never in practice: `url` serializes a tuple origin as an absolute URL. The
/// panic exists so a future `url` change fails loudly instead of silently
/// turning a tuple origin into an opaque one at three call sites.
#[must_use]
pub fn origin_url(origin: &url::Origin) -> Option<url::Url> {
    let url::Origin::Tuple(..) = origin else {
        return None;
    };
    Some(
        url::Url::parse(&origin.ascii_serialization()).expect("a tuple origin serializes as a URL"),
    )
}

/// The URL a browsing context starts on when it has no creator to inherit
/// from.
///
/// Parsed once: every document path, browser tab, and stylesheet parser that
/// needs the placeholder shares this value instead of re-spelling the URL.
#[must_use]
pub fn about_blank() -> url::Url {
    static ABOUT_BLANK: std::sync::LazyLock<url::Url> = std::sync::LazyLock::new(|| {
        url::Url::parse("about:blank").expect("about:blank is a valid URL")
    });
    ABOUT_BLANK.clone()
}

#[cfg(test)]
mod tests {
    use url::Url;

    /// A tuple origin serializes to a URL; an opaque one has none.
    #[test]
    fn an_origin_produces_its_url_or_nothing() {
        let page = Url::parse("https://a.test:8443/page").expect("URL");
        assert_eq!(
            super::origin_url(&page.origin())
                .expect("tuple origin")
                .as_str(),
            "https://a.test:8443/"
        );
        assert!(super::origin_url(&url::Origin::new_opaque()).is_none());
    }
}
