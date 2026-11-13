//! Host helpers for the `Origin` interface
//! (<https://html.spec.whatwg.org/multipage/browsers.html#the-origin-interface>).
//!
//! `Origin.from()` needs the URL parser's origin, including the blob-URL
//! delegation and the opaque-vs-tuple split, and `isSameSite()` needs the
//! registrable-domain rule the cookie rules use. Both live in Rust; the JS shim
//! owns the opaque-origin identity and the interface shape.

use rquickjs::{Ctx, Object, Result, prelude::Func};

pub(super) fn install(ctx: &Ctx<'_>) -> Result<()> {
    let bridge = crate::js::bridge::object(ctx)?;
    bridge.set("__tbOriginFromString", Func::from(origin_from_string))?;
    bridge.set("__tbOriginSameSite", Func::from(origin_same_site))?;
    Ok(())
}

/// Parse `value` as an absolute URL and report its origin.
///
/// `ok` is false when the string is not a URL (the caller throws `TypeError`).
/// An opaque origin has no serialization; a tuple origin carries the ASCII
/// serialization two origins compare with
/// (<https://url.spec.whatwg.org/#concept-url-origin>).
fn origin_from_string(ctx: Ctx<'_>, value: String) -> Result<Object<'_>> {
    let object = Object::new(ctx)?;
    match url::Url::parse(&value) {
        Err(_) => {
            object.set("ok", false)?;
        }
        Ok(url) => {
            object.set("ok", true)?;
            match url.origin() {
                url::Origin::Opaque(_) => object.set("opaque", true)?,
                url::Origin::Tuple(scheme, host, port) => {
                    object.set("opaque", false)?;
                    object.set(
                        "origin",
                        url::Origin::Tuple(scheme, host, port).ascii_serialization(),
                    )?;
                }
            }
        }
    }
    Ok(object)
}

/// Whether two serialized tuple origins share a site: same scheme and
/// registrable domain
/// (<https://html.spec.whatwg.org/multipage/browsers.html#same-site>).
fn origin_same_site(left: String, right: String) -> bool {
    let (Ok(left), Ok(right)) = (url::Url::parse(&left), url::Url::parse(&right)) else {
        return false;
    };
    net::origins_same_site(&left.origin(), &right.origin())
}

#[cfg(test)]
mod tests {
    use super::origin_same_site;

    /// Same-site is the cookie rule's scheme + registrable-domain comparison.
    #[test]
    fn same_site_uses_the_registrable_domain() {
        assert!(origin_same_site(
            "https://a.example".into(),
            "https://b.a.example".into()
        ));
        assert!(!origin_same_site(
            "https://a.example".into(),
            "http://a.example".into()
        ));
        assert!(!origin_same_site(
            "https://a.example".into(),
            "https://b.example".into()
        ));
        // `ws`/`wss` name the `http`/`https` sites.
        assert!(origin_same_site(
            "wss://a.example".into(),
            "https://b.a.example".into()
        ));
    }
}
