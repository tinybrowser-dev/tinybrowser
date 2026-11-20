// https://html.spec.whatwg.org/multipage/browsers.html#the-origin-interface

use rquickjs::{Ctx, Object, Result, prelude::Func};

pub(super) fn install(ctx: &Ctx<'_>) -> Result<()> {
    let bridge = crate::js::bridge::object(ctx)?;
    bridge.set("__tbOriginFromString", Func::from(origin_from_string))?;
    bridge.set("__tbOriginSameSite", Func::from(origin_same_site))?;
    Ok(())
}

// https://url.spec.whatwg.org/#concept-url-origin
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

// https://html.spec.whatwg.org/multipage/browsers.html#same-site
fn origin_same_site(left: String, right: String) -> bool {
    let (Ok(left), Ok(right)) = (url::Url::parse(&left), url::Url::parse(&right)) else {
        return false;
    };
    net::origins_same_site(&left.origin(), &right.origin())
}

#[cfg(test)]
mod tests {
    use super::origin_same_site;

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
        assert!(origin_same_site(
            "wss://a.example".into(),
            "https://b.a.example".into()
        ));
    }
}
