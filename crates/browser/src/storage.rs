// https://html.spec.whatwg.org/multipage/webstorage.html#the-localstorage-attribute
// https://w3c.github.io/IndexedDB/#database

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use net::{
    Clock, CookieFilter, CookieJar, CookieOp, CookieRecord, DirectCookie, InitiatorKind,
    RetrievalKind, SameSiteCookiePolicy, ScriptCookieWrite,
};
use renderer::ScriptCookie;
use url::Url;

use crate::actor::TabId;

pub(crate) use webstorage::StorageAreas;
pub(crate) type SessionStorage = webstorage::SessionStorage<TabId>;

// https://httpwg.org/http-extensions/draft-ietf-httpbis-rfc6265bis.html
pub(crate) struct CookieStore {
    jar: Arc<Mutex<CookieJar>>,
    clock: Clock,
}

impl CookieStore {
    pub(crate) fn new(clock: Clock) -> Self {
        Self {
            jar: Arc::new(Mutex::new(CookieJar::default())),
            clock,
        }
    }

    pub(crate) fn jar(&self) -> Arc<Mutex<CookieJar>> {
        Arc::clone(&self.jar)
    }

    fn lock(&self) -> MutexGuard<'_, CookieJar> {
        self.jar.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn fetch_op<'a>(
        &self,
        url: &'a Url,
        origin: &'a url::Origin,
        kind: RetrievalKind,
        partition_key: Option<&'a str>,
    ) -> CookieOp<'a> {
        CookieOp {
            url,
            now: self.clock.now(),
            kind,
            initiator_kind: InitiatorKind::Fetch,
            method_is_safe: true,
            initiator: Some(origin),
            cross_site_redirect: false,
            same_site_policy: SameSiteCookiePolicy::All,
            partition_key,
        }
    }

    pub(crate) fn cookies_for(&self, url: &Url, partition_key: Option<&str>) -> String {
        let origin = url.origin();
        self.lock().cookie_string(self.fetch_op(
            url,
            &origin,
            RetrievalKind::NonHttp,
            partition_key,
        ))
    }

    pub(crate) fn set_cookie(&self, value: &str, url: &Url, partition_key: Option<&str>) {
        let origin = url.origin();
        let _stored = self.lock().store(
            value,
            self.fetch_op(url, &origin, RetrievalKind::NonHttp, partition_key),
        );
    }

    pub(crate) fn cookie_store_records(
        &self,
        url: &Url,
        partition_key: Option<&str>,
    ) -> Vec<ScriptCookie> {
        let origin = url.origin();
        self.lock()
            .records_for(self.fetch_op(url, &origin, RetrievalKind::NonHttp, partition_key))
            .into_iter()
            .map(|record| ScriptCookie::from_record(&record))
            .collect()
    }

    pub(crate) fn cookie_store_store(
        &self,
        write: &ScriptCookieWrite,
        url: &Url,
        partition_key: Option<&str>,
    ) {
        let origin = url.origin();
        self.lock().store_script(
            write,
            self.fetch_op(url, &origin, RetrievalKind::NonHttp, partition_key),
        );
    }

    pub(crate) fn cookie_records(&self, url: &Url) -> Vec<CookieRecord> {
        let origin = url.origin();
        self.lock()
            .records_for(self.fetch_op(url, &origin, RetrievalKind::Http, None))
    }

    pub(crate) fn add_cookie(&self, cookie: &str, url: &Url) -> bool {
        let origin = url.origin();
        self.lock().store(
            cookie,
            self.fetch_op(url, &origin, RetrievalKind::Http, None),
        )
    }

    pub(crate) fn all_records(&self) -> Vec<CookieRecord> {
        let now = self.clock.now();
        self.lock().all_records(now)
    }

    pub(crate) fn matching_records(&self, filter: &CookieFilter) -> Vec<CookieRecord> {
        let now = self.clock.now();
        self.lock().matching_records(filter, now)
    }

    pub(crate) fn set_direct(&self, cookie: DirectCookie) -> bool {
        let now = self.clock.now();
        self.lock().set_direct(cookie, now)
    }

    pub(crate) fn delete_matching(&self, filter: &CookieFilter) -> usize {
        let now = self.clock.now();
        self.lock().delete_matching(filter, now)
    }

    pub(crate) fn delete_for_url(&self, url: &Url, name: Option<&str>) -> usize {
        let origin = url.origin();
        self.lock()
            .delete_for_url(self.fetch_op(url, &origin, RetrievalKind::Http, None), name)
    }

    pub(crate) fn clear(&self) {
        self.lock().clear();
    }

    pub(crate) fn import(&self, records: Vec<CookieRecord>) {
        self.lock().restore(records, self.clock.now());
    }

    pub(crate) fn export(&self) -> Vec<CookieRecord> {
        self.lock().snapshot()
    }
}
