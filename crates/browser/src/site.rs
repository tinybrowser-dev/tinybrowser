use renderer::SerializedOrigin;
use url::Url;

use crate::actor::TabId;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Site {
    Tuple(String),
    Opaque(TabId),
}

impl Site {
    #[must_use]
    pub(crate) fn for_url(url: &Url) -> Option<Self> {
        if url.scheme() == "http" || url.scheme() == "https" {
            net::site(url).map(Self::Tuple)
        } else {
            None
        }
    }

    #[must_use]
    pub(crate) fn opaque(tab: TabId) -> Self {
        Self::Opaque(tab)
    }

    #[must_use]
    pub(crate) fn as_partition_key(&self) -> Option<&str> {
        match self {
            Self::Tuple(site) => Some(site.as_str()),
            Self::Opaque(_) => None,
        }
    }

    // https://url.spec.whatwg.org/#origin
    // https://chromium.googlesource.com/chromium/src/+/main/docs/process_model_and_site_isolation.md#process-locks
    #[must_use]
    pub(crate) fn allows_url(&self, url: &Url) -> bool {
        match url.origin() {
            origin @ url::Origin::Tuple(..) => self.allows_origin(&origin),
            url::Origin::Opaque(_) => {
                matches!(self, Self::Opaque(_)) && matches!(url.scheme(), "about" | "blob" | "data")
            }
        }
    }

    // https://chromium.googlesource.com/chromium/src/+/main/docs/process_model_and_site_isolation.md#process-locks
    #[must_use]
    pub(crate) fn authorize_origin(&self, claim: &SerializedOrigin) -> Option<url::Origin> {
        match claim {
            SerializedOrigin::Tuple(origin) if self.allows_origin(origin) => Some(origin.clone()),
            SerializedOrigin::Tuple(_) | SerializedOrigin::Opaque => None,
        }
    }

    #[must_use]
    pub(crate) fn allows_message_origin(&self, origin: &SerializedOrigin) -> bool {
        match origin {
            SerializedOrigin::Opaque => true,
            SerializedOrigin::Tuple(origin) => self.allows_origin(origin),
        }
    }

    #[must_use]
    pub(crate) fn allows_origin(&self, origin: &url::Origin) -> bool {
        let Self::Tuple(locked) = self else {
            return false;
        };
        net::site_for_origin(origin).is_some_and(|site| site == *locked)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renderer_site_lock_rejects_other_principals() {
        let locked =
            Site::for_url(&Url::parse("https://a.example.test/").expect("url")).expect("site");
        assert!(locked.allows_url(&Url::parse("https://b.example.test/path").expect("url")));
        assert!(!locked.allows_url(&Url::parse("http://a.example.test/").expect("url")));
        assert!(!locked.allows_url(&Url::parse("https://other.test/").expect("url")));
        assert_eq!(
            Site::for_url(&Url::parse("http://a.example.test/").expect("url")),
            Some(Site::Tuple("http://example.test".to_owned())),
        );

        let opaque = Site::opaque(TabId::new(7));
        assert!(opaque.allows_url(&Url::parse("about:blank").expect("url")));
        assert!(!opaque.allows_url(&Url::parse("file:///etc/passwd").expect("url")));
        assert!(!opaque.allows_url(&Url::parse("https://example.test/").expect("url")));
    }

    // https://url.spec.whatwg.org/#origin
    #[test]
    fn a_blob_url_belongs_to_the_site_that_created_it() {
        let locked =
            Site::for_url(&Url::parse("https://www.example.test/").expect("url")).expect("site");
        assert!(
            locked.allows_url(
                &Url::parse("blob:https://ads.example.test/0f8a6a58-2c56-4b49-8bd9-1e6b8f0a4d21")
                    .expect("url")
            )
        );
        assert!(
            !locked.allows_url(
                &Url::parse("blob:https://other.test/0f8a6a58-2c56-4b49-8bd9-1e6b8f0a4d21")
                    .expect("url")
            )
        );
        assert!(!locked.allows_url(&Url::parse("blob:no-inner-url").expect("url")));

        let opaque = Site::opaque(TabId::new(7));
        assert!(
            !opaque.allows_url(&Url::parse("blob:https://www.example.test/id").expect("url")),
            "a blob with a tuple origin names an HTTP(S) principal an opaque lock cannot claim",
        );
        assert!(
            opaque.allows_url(
                &Url::parse("blob:null/0f8a6a58-2c56-4b49-8bd9-1e6b8f0a4d21").expect("url")
            ),
            "a blob with an opaque origin belongs to this tab",
        );
    }

    // https://chromium.googlesource.com/chromium/src/+/main/docs/process_model_and_site_isolation.md#process-locks
    #[test]
    fn a_same_site_frame_may_claim_its_own_origin() {
        let locked =
            Site::for_url(&Url::parse("https://www.example.test/").expect("url")).expect("site");
        for allowed in [
            "https://www.example.test",
            "https://ads.example.test",
            "https://example.test:8443",
        ] {
            assert!(
                locked
                    .authorize_origin(&claim(allowed))
                    .is_some_and(|origin| origin.ascii_serialization() == allowed),
                "{allowed}"
            );
        }
        for rejected in [
            "https://other.test",
            "http://ads.example.test",
            "null",
            "about:blank",
        ] {
            assert!(
                locked.authorize_origin(&claim(rejected)).is_none(),
                "{rejected}"
            );
        }
        let opaque = Site::opaque(TabId::new(7));
        assert!(opaque.authorize_origin(&claim("null")).is_none());
        assert!(
            opaque
                .authorize_origin(&claim("https://www.example.test"))
                .is_none()
        );
    }

    #[test]
    fn storage_keys_are_tuple_origins() {
        use renderer::StorageKey;

        let locked =
            Site::for_url(&Url::parse("https://www.example.test/").expect("url")).expect("site");
        let key = StorageKey::from_origin(
            &locked
                .authorize_origin(&claim("https://ads.example.test/path?q#f"))
                .expect("a same-site tuple origin"),
        )
        .expect("storage key");
        assert_eq!(key.to_string(), "https://ads.example.test");
        for spelling in [
            "https://ads.example.test/",
            "https://ads.example.test/a?b#c",
            "HTTPS://ADS.EXAMPLE.TEST",
        ] {
            let authorized = locked
                .authorize_origin(&claim(spelling))
                .and_then(|origin| StorageKey::from_origin(&origin));
            assert_eq!(authorized.as_ref(), Some(&key), "{spelling}");
        }
        assert!(locked.authorize_origin(&claim("null")).is_none());
        assert!(
            locked
                .authorize_origin(&claim("https://other.test"))
                .is_none()
        );

        let opaque = Site::opaque(TabId::new(7));
        assert!(opaque.authorize_origin(&claim("null")).is_none());
        assert!(opaque.authorize_origin(&claim("about:blank")).is_none());
    }

    fn claim(spec: &str) -> SerializedOrigin {
        SerializedOrigin::parse(spec).expect("a serialized origin")
    }
}
