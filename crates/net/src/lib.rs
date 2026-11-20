// https://fetch.spec.whatwg.org/

mod client;
pub mod cors;
mod error;
mod identity;
pub mod orb;
mod protocol;
pub mod referrer;
#[cfg(feature = "native")]
mod resolve;
mod transport;

pub use client::{Agent, AgentOptions, Body, Clock, ClockSource, Request, Response};
pub use cookies::{
    CookieRecord, CookieSameSite, InitiatorKind, SameSiteCookiePolicy, ScriptCookieWrite,
    is_public_suffix, origins_same_site, site, site_for_origin,
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
#[cfg(feature = "native")]
pub use resolve::{InvalidResolveRule, ResolveRule};
#[cfg(feature = "native")]
pub use transport::native::{InvalidCa, InvalidProxy, Proxy, TrustedCa};
pub use transport::{HopBudget, Transport, WireBody, WireRequest, WireResponse, buffered_body};

#[must_use]
pub fn origin_url(origin: &url::Origin) -> Option<url::Url> {
    let url::Origin::Tuple(..) = origin else {
        return None;
    };
    Some(
        url::Url::parse(&origin.ascii_serialization()).expect("a tuple origin serializes as a URL"),
    )
}

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
