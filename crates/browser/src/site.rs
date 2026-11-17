//! Renderer identity: one site instance per scheme plus registrable domain.

use renderer::{SerializedOrigin, StorageKey};
use url::Url;

use crate::actor::TabId;

/// Chrome-style site: scheme plus registrable domain (`https://example.co.uk`),
/// or an opaque per-tab instance for non-HTTP(S) documents.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Site {
    /// One HTTP(S) site: `https://example.co.uk`.
    Tuple(String),
    /// One tab's non-network documents.
    Opaque(TabId),
}

impl Site {
    /// Site for an HTTP(S) URL; `None` for opaque or non-HTTP URLs.
    #[must_use]
    pub(crate) fn for_url(url: &Url) -> Option<Self> {
        if url.scheme() == "http" || url.scheme() == "https" {
            net::site(url).map(Self::Tuple)
        } else {
            None
        }
    }

    /// Opaque instance for one tab's non-HTTP documents.
    #[must_use]
    pub(crate) fn opaque(tab: TabId) -> Self {
        Self::Opaque(tab)
    }

    /// Whether a document URL belongs in a renderer locked to this site.
    ///
    /// The check is on the URL's origin, not its host: a `blob:` URL's origin
    /// is the origin of the URL inside its path
    /// (<https://url.spec.whatwg.org/#origin>), so a document hosted from a
    /// blob URL belongs to the site of the page that created it, and a
    /// same-site renderer may claim it. Every other spelling of a tuple
    /// origin is the site itself. Opaque locks accept only non-network
    /// documents; they are unique per tab and cannot be used to claim
    /// authority for an HTTP(S) principal. Chromium applies the same
    /// browser-owned process-lock invariant before granting access to origin
    /// data:
    /// <https://chromium.googlesource.com/chromium/src/+/main/docs/process_model_and_site_isolation.md#process-locks>.
    #[must_use]
    pub(crate) fn allows_url(&self, url: &Url) -> bool {
        match (self, url.origin()) {
            (Self::Tuple(locked), origin @ url::Origin::Tuple(..)) => {
                net::site_for_origin(&origin).is_some_and(|site| site == *locked)
            }
            // A blob URL whose inner URL is HTTP(S) has a tuple origin, so an
            // opaque lock cannot claim it. Only the schemes that can create a
            // genuinely opaque document pass; `file:` has an opaque origin too
            // but is not a non-network document this tab owns.
            (Self::Opaque(_), url::Origin::Opaque(_)) => {
                matches!(url.scheme(), "about" | "blob" | "data")
            }
            // A tuple lock with an opaque origin and an opaque lock with a
            // tuple origin name no authority this renderer owns.
            (Self::Tuple(_), url::Origin::Opaque(_))
            | (Self::Opaque(_), url::Origin::Tuple(..)) => false,
        }
    }

    /// Parses one document URL and checks it against this lock. This is the
    /// entry point for URLs (cookie operations and referrer checks); origin
    /// claims use [`Self::authorize_origin`].
    pub(crate) fn authorize_url(&self, spec: &str) -> Option<Url> {
        Url::parse(spec).ok().filter(|url| self.allows_url(url))
    }

    /// The tuple origin one renderer claims, when this lock authorizes it.
    ///
    /// The renderer hosts every same-site frame, and a frame's origin differs
    /// from the top-level document's while staying inside the lock:
    /// `https://ads.example.test` inside `https://www.example.test`, or one
    /// host on another port (`site` ignores the port). The lock is the trust
    /// boundary — one renderer process is one authority — and it is the same
    /// bound `postMessage`, `BroadcastChannel`, and storage apply. Recording
    /// per-frame origins would not narrow it: the browser would learn them
    /// from the process it is checking, and every frame in that process is the
    /// same trust unit
    /// (<https://chromium.googlesource.com/chromium/src/+/main/docs/process_model_and_site_isolation.md#process-locks>).
    ///
    /// An opaque claim names no tuple origin and is not authorized here; the
    /// messaging entry point decides whether an opaque sender is allowed.
    #[must_use]
    pub(crate) fn authorize_origin(&self, claim: &SerializedOrigin) -> Option<url::Origin> {
        match claim {
            SerializedOrigin::Tuple(origin) if self.allows_origin(origin) => Some(origin.clone()),
            SerializedOrigin::Tuple(_) | SerializedOrigin::Opaque => None,
        }
    }

    /// The storage key one renderer claims, when this lock authorizes it and
    /// the origin is a tuple origin.
    ///
    /// Storage has no opaque case: the getter throws `SecurityError` for an
    /// opaque document in the renderer, and the browser refuses one that
    /// arrives anyway
    /// (<https://html.spec.whatwg.org/multipage/webstorage.html#the-localstorage-attribute>).
    #[must_use]
    pub(crate) fn storage_key(&self, claim: &SerializedOrigin) -> Option<StorageKey> {
        StorageKey::from_origin(&self.authorize_origin(claim)?)
    }

    /// Whether one message's claimed sender origin may be delivered as coming
    /// from this renderer.
    ///
    /// An opaque origin claims no authority, so it passes: the receiving
    /// document's target-origin check is what rejects a delivery it does not
    /// want. A tuple origin must belong to this site.
    #[must_use]
    pub(crate) fn allows_message_origin(&self, origin: &SerializedOrigin) -> bool {
        match origin {
            SerializedOrigin::Opaque => true,
            SerializedOrigin::Tuple(origin) => self.allows_origin(origin),
        }
    }

    /// Whether a parsed tuple origin belongs in a renderer locked to this site.
    #[must_use]
    fn allows_origin(&self, origin: &url::Origin) -> bool {
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

    /// A document hosted from a blob URL still belongs to the site that
    /// created it: a `blob:` URL's origin is the origin of the URL in its
    /// path, so a same-site renderer may claim it while another site's blob
    /// URL is refused (<https://url.spec.whatwg.org/#origin>).
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

    /// A renderer's origin claim is bounded by the site, not by the top-level
    /// document's origin: every same-site frame lives in the same process, so
    /// its own origin is a legitimate claim while another site's is not
    /// (<https://chromium.googlesource.com/chromium/src/+/main/docs/process_model_and_site_isolation.md#process-locks>).
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

    /// Storage is keyed by the tuple origin, and a claim that is not one is
    /// refused instead of becoming a shared `"null"` area.
    #[test]
    fn storage_keys_are_tuple_origins() {
        let locked =
            Site::for_url(&Url::parse("https://www.example.test/").expect("url")).expect("site");
        let key = locked
            .storage_key(&claim("https://ads.example.test/path?q#f"))
            .expect("a same-site tuple origin");
        assert_eq!(key.to_string(), "https://ads.example.test");
        // Every spelling of one origin is one key.
        for spelling in [
            "https://ads.example.test/",
            "https://ads.example.test/a?b#c",
            "HTTPS://ADS.EXAMPLE.TEST",
        ] {
            assert_eq!(
                locked.storage_key(&claim(spelling)).as_ref(),
                Some(&key),
                "{spelling}"
            );
        }
        assert!(locked.storage_key(&claim("null")).is_none());
        assert!(locked.storage_key(&claim("https://other.test")).is_none());

        let opaque = Site::opaque(TabId::new(7));
        assert!(opaque.storage_key(&claim("null")).is_none());
        assert!(opaque.storage_key(&claim("about:blank")).is_none());
    }

    fn claim(spec: &str) -> SerializedOrigin {
        SerializedOrigin::parse(spec).expect("a serialized origin")
    }
}
