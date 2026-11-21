// https://fetch.spec.whatwg.org/#http-network-or-cache-fetch
// https://www.rfc-editor.org/rfc/rfc9111

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Instant, SystemTime};

use net::{Clock, Method, Request, Response, isomorphic_decode, isomorphic_encode};
use renderer::{DialFailure, DialOutcome, DialRequest, RequestCache, RequestPolicy, ResponseBody};
use url::Url;

pub const NO_PARTITION: u64 = 0;

const MAX_ENTRIES: usize = 64;
const MAX_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct CacheKey {
    method: Method,
    url: Url,
    credentials: net::cors::CredentialsMode,
    initiator: url::Origin,
    partition: u64,
}

#[derive(Clone)]
struct CachedResponse {
    outcome: DialOutcome,
    stored_at: Instant,
    stored_wall: SystemTime,
    // https://www.rfc-editor.org/rfc/rfc9111#section-4.1
    request_headers: Vec<(String, String)>,
    seq: u64,
}

impl CachedResponse {
    // https://www.rfc-editor.org/rfc/rfc9111#section-4
    fn stored_exchange(
        outcome: &DialOutcome,
        stored_at: Instant,
        stored_wall: SystemTime,
        request_headers: Vec<(String, String)>,
        seq: u64,
    ) -> Self {
        let mut outcome = outcome.clone();
        outcome.cookies_stored = false;
        Self {
            outcome,
            stored_at,
            stored_wall,
            request_headers,
            seq,
        }
    }
}

type Entries = HashMap<CacheKey, Vec<CachedResponse>>;

// https://www.rfc-editor.org/rfc/rfc9111#section-4.3.1
// https://www.rfc-editor.org/rfc/rfc9110#section-14.2
#[derive(Clone, Debug, Default)]
pub struct Validators {
    etag: Option<String>,
    last_modified: Option<String>,
    range: Option<String>,
}

impl Validators {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.etag.is_none() && self.last_modified.is_none() && self.range.is_none()
    }

    pub fn apply(&self, request: &mut Request) -> Result<(), DialFailure> {
        if let Some(etag) = self.etag.as_deref().and_then(isomorphic_encode) {
            request
                .headers
                .insert("If-None-Match", etag)
                .map_err(|_| DialFailure::Connect)?;
        }
        if let Some(modified) = self.last_modified.as_deref().and_then(isomorphic_encode) {
            request
                .headers
                .insert("If-Modified-Since", modified)
                .map_err(|_| DialFailure::Connect)?;
        }
        if let Some(range) = self.range.as_deref() {
            request
                .headers
                .insert("Range", range.to_owned())
                .map_err(|_| DialFailure::Connect)?;
        }
        Ok(())
    }
}

pub enum CacheDecision {
    Hit(DialOutcome),
    Miss {
        mode: RequestCache,
        stored: Option<DialOutcome>,
        validators: Validators,
    },
}

pub struct HttpCache {
    clock: Clock,
    entries: Mutex<Entries>,
    next_seq: AtomicU64,
    disabled: AtomicBool,
}

impl HttpCache {
    #[must_use]
    pub fn new(clock: Clock) -> Self {
        Self {
            clock,
            entries: Mutex::new(HashMap::new()),
            next_seq: AtomicU64::new(0),
            disabled: AtomicBool::new(false),
        }
    }

    pub fn set_disabled(&self, disabled: bool) {
        self.disabled.store(disabled, Ordering::Relaxed);
    }

    #[must_use]
    pub fn is_disabled(&self) -> bool {
        self.disabled.load(Ordering::Relaxed)
    }

    // https://fetch.spec.whatwg.org/#http-network-or-cache-fetch
    // https://www.rfc-editor.org/rfc/rfc9111#section-4.1
    pub fn read(
        &self,
        request: &DialRequest,
        sent: &[(String, String)],
        initiator: &url::Origin,
        partition: u64,
    ) -> Result<CacheDecision, DialFailure> {
        let mode = effective_mode(request);
        if self.is_disabled() {
            if matches!(mode, RequestCache::OnlyIfCached) {
                return Err(DialFailure::Connect);
            }
            return Ok(Self::miss(RequestCache::NoStore, None));
        }
        let directives = RequestDirectives::parse(sent);
        let stored = self.lookup(request, sent, initiator, partition);
        // https://fetch.spec.whatwg.org/#concept-request-cache-mode
        if matches!(mode, RequestCache::OnlyIfCached) {
            let Some(entry) = stored else {
                return Err(DialFailure::Connect);
            };
            let outcome = self
                .serve(&entry, request, sent)?
                .ok_or(DialFailure::Connect)?;
            return Ok(CacheDecision::Hit(outcome));
        }
        // https://www.rfc-editor.org/rfc/rfc9111#section-5.2.1.7
        if directives.only_if_cached {
            let hit = stored
                .filter(|entry| usable(entry, &directives, &self.clock))
                .map(|entry| {
                    self.serve(&entry, request, sent)
                        .map(|outcome| outcome.unwrap_or_else(|| gateway_timeout(request)))
                })
                .transpose()?;
            return Ok(CacheDecision::Hit(
                hit.unwrap_or_else(|| gateway_timeout(request)),
            ));
        }
        // https://fetch.spec.whatwg.org/#request-cache-directives
        if matches!(mode, RequestCache::NoStore) || directives.no_store {
            return Ok(Self::miss(mode, None));
        }
        match mode {
            RequestCache::ForceCache => {
                if directives.no_cache {
                    // https://www.rfc-editor.org/rfc/rfc9111#section-5.2.1.4
                    return Ok(Self::miss(mode, stored));
                }
                let answered = match stored {
                    Some(entry) => self.serve(&entry, request, sent)?,
                    None => None,
                };
                Ok(match answered {
                    Some(outcome) => CacheDecision::Hit(outcome),
                    // https://fetch.spec.whatwg.org/#concept-request-cache-mode
                    None => Self::miss(mode, None),
                })
            }
            RequestCache::Reload | RequestCache::NoStore | RequestCache::OnlyIfCached => {
                Ok(Self::miss(mode, None))
            }
            RequestCache::NoCache => Ok(Self::miss(mode, stored)),
            RequestCache::Default => {
                let Some(entry) = stored else {
                    return Ok(Self::miss(mode, None));
                };
                if usable(&entry, &directives, &self.clock)
                    && let Some(outcome) = self.serve(&entry, request, sent)?
                {
                    return Ok(CacheDecision::Hit(outcome));
                }
                // https://www.rfc-editor.org/rfc/rfc9111#section-3.3
                let completion = completion_range(&entry, sent);
                let mut miss = Self::miss(mode, Some(entry));
                if let CacheDecision::Miss { validators, .. } = &mut miss {
                    validators.range = completion;
                }
                Ok(miss)
            }
        }
    }

    fn serve(
        &self,
        entry: &CachedResponse,
        request: &DialRequest,
        sent: &[(String, String)],
    ) -> Result<Option<DialOutcome>, DialFailure> {
        let Some(outcome) = answer(entry, sent) else {
            return Ok(None);
        };
        if !crate::integrity_ok(request, &outcome) {
            return Err(DialFailure::Connect);
        }
        Ok(Some(self.aged(entry, outcome)))
    }

    // https://www.rfc-editor.org/rfc/rfc9111#section-4
    fn aged(&self, entry: &CachedResponse, mut outcome: DialOutcome) -> DialOutcome {
        let age = current_age(entry, &self.clock);
        set_header(&mut outcome.headers, "age", &age.to_string());
        outcome
    }

    fn miss(mode: RequestCache, stored: Option<CachedResponse>) -> CacheDecision {
        let validators = match &stored {
            Some(entry) if !matches!(mode, RequestCache::Reload) => validators(entry),
            _ => Validators::default(),
        };
        CacheDecision::Miss {
            mode,
            stored: stored.map(|entry| entry.outcome),
            validators,
        }
    }

    // https://www.rfc-editor.org/rfc/rfc9111#section-4.4
    pub fn remember(
        &self,
        request: &DialRequest,
        sent: &[(String, String)],
        initiator: &url::Origin,
        partition: u64,
        outcome: &DialOutcome,
    ) {
        let directives = RequestDirectives::parse(sent);
        if !request.method.is_safe() {
            self.invalidate_unsafe(request, partition, outcome);
            // https://www.rfc-editor.org/rfc/rfc9110#section-8.7
            // https://www.rfc-editor.org/rfc/rfc9111#section-5.2.1.5
            if !self.is_disabled() && (200..400).contains(&outcome.status) && !directives.no_store {
                self.store_content_location(request, sent, initiator, partition, outcome);
            }
            return;
        }
        if self.is_disabled()
            || directives.no_store
            || !storable(request, outcome, effective_mode(request))
        {
            return;
        }
        let entry = CachedResponse::stored_exchange(
            outcome,
            self.clock.monotonic(),
            self.clock.now(),
            sent.to_vec(),
            self.next_seq.fetch_add(1, Ordering::Relaxed),
        );
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        insert_cached(
            &mut entries,
            cache_key(request, initiator, partition),
            entry,
        );
    }

    // https://www.rfc-editor.org/rfc/rfc9111#section-4.4
    pub fn invalidate_unsafe(&self, request: &DialRequest, partition: u64, outcome: &DialOutcome) {
        if request.method.is_safe() || !(200..400).contains(&outcome.status) {
            return;
        }
        self.invalidate(&request.url, partition);
        for name in ["location", "content-location"] {
            let Some(target) = header_url(outcome, name, &request.url) else {
                continue;
            };
            if target.origin() == request.url.origin() {
                self.invalidate(&target, partition);
            }
        }
    }

    // https://www.rfc-editor.org/rfc/rfc9110#section-8.7
    fn store_content_location(
        &self,
        request: &DialRequest,
        sent: &[(String, String)],
        initiator: &url::Origin,
        partition: u64,
        outcome: &DialOutcome,
    ) {
        let Some(target) = content_location_target(request, outcome) else {
            return;
        };
        let mut get = request.clone();
        get.method = Method::GET;
        get.url = target;
        let mut headers = sent.to_vec();
        headers
            .retain(|(name, _)| !is_conditional_name(name) && !name.eq_ignore_ascii_case("range"));
        let entry = CachedResponse::stored_exchange(
            outcome,
            self.clock.monotonic(),
            self.clock.now(),
            headers,
            self.next_seq.fetch_add(1, Ordering::Relaxed),
        );
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        insert_cached(&mut entries, cache_key(&get, initiator, partition), entry);
    }

    pub fn clear(&self) {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }

    // https://www.rfc-editor.org/rfc/rfc9111#section-4.4
    pub fn invalidate(&self, url: &Url, partition: u64) {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|key, _| key.url != *url || key.partition != partition);
    }

    #[must_use]
    pub fn may_store(
        &self,
        request: &DialRequest,
        sent: &[(String, String)],
        head: &DialOutcome,
    ) -> bool {
        if self.is_disabled() || RequestDirectives::parse(sent).no_store {
            return false;
        }
        if !request.method.is_safe() {
            return (200..400).contains(&head.status)
                && content_location_target(request, head).is_some();
        }
        storable(request, head, effective_mode(request))
    }

    // https://www.rfc-editor.org/rfc/rfc9111#section-4.3.4
    #[must_use]
    pub fn merge_not_modified(
        cached: &DialOutcome,
        response: &Response,
        validators: &Validators,
    ) -> Option<DialOutcome> {
        let updates: Vec<(String, String)> = response
            .filtered()
            .headers()
            .into_iter()
            .map(|(name, value)| (name.to_owned(), isomorphic_decode(value)))
            .collect();
        let fresh = combined_header(&updates, "etag");
        if !not_modified_updates(validators, fresh.as_deref()) {
            return None;
        }
        let mut merged = cached.clone();
        merged.final_url = response.final_url().clone();
        merged.referrer = response.referrer().cloned();
        merged.crossed_origin = response.crossed_origin();
        // https://www.rfc-editor.org/rfc/rfc6265bis.html#section-5.2
        // https://www.rfc-editor.org/rfc/rfc9111#section-4.3.4
        merged.cookies_stored = response.cookies_stored();
        // https://www.rfc-editor.org/rfc/rfc9111#section-4.2.3
        merged
            .headers
            .retain(|(name, _)| !name.eq_ignore_ascii_case("age"));
        for (name, value) in updates {
            if is_merge_protected(&name) {
                continue;
            }
            set_header(&mut merged.headers, &name, &value);
        }
        Some(merged)
    }

    fn lookup(
        &self,
        request: &DialRequest,
        sent: &[(String, String)],
        initiator: &url::Origin,
        partition: u64,
    ) -> Option<CachedResponse> {
        if !request.policy.exposes_response() {
            return None;
        }
        let key = cache_key(request, initiator, partition);
        let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        let usable_entry = |entry: &CachedResponse| {
            vary_matches(entry, sent) && cache_usable(request, initiator, entry)
        };
        if let Some(variants) = entries.get(&key)
            && let Some(entry) = newest(variants.iter().filter(|entry| usable_entry(entry)))
        {
            return Some(entry.clone());
        }
        // https://wicg.github.io/nav-speculation/no-vary-search.html
        entries.iter().find_map(|(stored_key, variants)| {
            (stored_key.method == key.method
                && stored_key.credentials == key.credentials
                && stored_key.initiator == key.initiator
                && stored_key.partition == key.partition
                && stored_key.url != key.url)
                .then(|| {
                    newest(variants.iter().filter(|entry| {
                        no_vary_matches(&stored_key.url, &request.url, &entry.outcome)
                            && usable_entry(entry)
                    }))
                    .cloned()
                })
                .flatten()
        })
    }
}

// https://www.rfc-editor.org/rfc/rfc9111#section-5.2.1.7
fn gateway_timeout(request: &DialRequest) -> DialOutcome {
    DialOutcome {
        status: 504,
        status_text: "Gateway Timeout".to_owned(),
        final_url: request.url.clone(),
        content_type: None,
        headers: Vec::new(),
        body: ResponseBody::Bytes(Vec::new()),
        response_type: net::cors::ResponseType::Basic,
        crossed_origin: false,
        redirect_count: 0,
        cookies_stored: false,
        referrer: None,
    }
}

// https://www.rfc-editor.org/rfc/rfc9111#section-5.2.1
#[derive(Default)]
struct RequestDirectives {
    only_if_cached: bool,
    no_store: bool,
    no_cache: bool,
    max_age: Option<u64>,
    min_fresh: Option<u64>,
    max_stale: MaxStale,
}

#[derive(Default)]
enum MaxStale {
    #[default]
    Unset,
    Unbounded,
    Limit(u64),
}

impl RequestDirectives {
    fn parse(headers: &[(String, String)]) -> Self {
        let mut parsed = Self::default();
        for directive in net::get_decode_split_decoded(headers, "cache-control").unwrap_or_default()
        {
            let (name, value) = directive
                .split_once('=')
                .map_or((directive.as_str(), None), |(name, value)| {
                    (name, Some(value))
                });
            let seconds = value.and_then(|value| value.trim().parse::<u64>().ok());
            match name.trim().to_ascii_lowercase().as_str() {
                "only-if-cached" => parsed.only_if_cached = true,
                "no-store" => parsed.no_store = true,
                "no-cache" => parsed.no_cache = true,
                "max-age" => parsed.max_age = seconds,
                "min-fresh" => parsed.min_fresh = seconds,
                "max-stale" => {
                    parsed.max_stale = seconds.map_or(MaxStale::Unbounded, MaxStale::Limit);
                }
                _ => {}
            }
        }
        parsed
    }
}

// https://fetch.spec.whatwg.org/#http-network-or-cache-fetch
fn effective_mode(request: &DialRequest) -> RequestCache {
    match request.cache {
        RequestCache::Default if has_page_conditional(request) => RequestCache::NoStore,
        mode => mode,
    }
}

fn cache_key(request: &DialRequest, initiator: &url::Origin, partition: u64) -> CacheKey {
    CacheKey {
        method: request.method.clone(),
        url: request.url.clone(),
        credentials: request.policy.cors().credentials,
        initiator: initiator.clone(),
        partition,
    }
}

fn has_page_conditional(request: &DialRequest) -> bool {
    request
        .headers
        .iter()
        .any(|(name, _)| is_conditional_name(name))
}

fn directive(headers: &[(String, String)], wanted: &str) -> Option<String> {
    net::get_decode_split_decoded(headers, "cache-control")?
        .iter()
        .find_map(|directive| {
            let (name, value) = directive
                .split_once('=')
                .map_or((directive.as_str(), ""), |(name, value)| (name, value));
            name.trim_matches([' ', '\t'])
                .eq_ignore_ascii_case(wanted)
                .then(|| value.trim_matches([' ', '\t']).to_owned())
        })
}

fn has_directive(headers: &[(String, String)], wanted: &str) -> bool {
    directive(headers, wanted).is_some()
}

fn directive_seconds(headers: &[(String, String)], wanted: &str) -> Option<u64> {
    directive(headers, wanted)?.parse::<u64>().ok()
}

fn header_time(headers: &[(String, String)], name: &str) -> Option<SystemTime> {
    let value = net::combined_header_in(headers, name)?;
    httpdate::parse_http_date(&value).ok()
}

// https://www.rfc-editor.org/rfc/rfc9111#section-4.2
fn usable(entry: &CachedResponse, request: &RequestDirectives, clock: &Clock) -> bool {
    if request.no_cache {
        return false;
    }
    let age = current_age(entry, clock);
    if request.max_age.is_some_and(|max_age| age >= max_age) {
        return false;
    }
    let Some(lifetime) = freshness_lifetime(entry) else {
        return stale_usable(entry, request, age, 0);
    };
    if request
        .min_fresh
        .is_some_and(|min_fresh| lifetime.saturating_sub(age) < min_fresh)
    {
        return false;
    }
    if age < lifetime {
        return true;
    }
    stale_usable(entry, request, age, lifetime)
}

fn stale_usable(
    entry: &CachedResponse,
    request: &RequestDirectives,
    age: u64,
    lifetime: u64,
) -> bool {
    if has_directive(&entry.outcome.headers, "no-cache")
        || has_directive(&entry.outcome.headers, "must-revalidate")
    {
        return false;
    }
    match request.max_stale {
        MaxStale::Unbounded => true,
        MaxStale::Limit(limit) => age.saturating_sub(lifetime) <= limit,
        MaxStale::Unset => false,
    }
}

// https://www.rfc-editor.org/rfc/rfc9111#section-4.2.3
fn current_age(entry: &CachedResponse, clock: &Clock) -> u64 {
    let resident = clock
        .monotonic()
        .saturating_duration_since(entry.stored_at)
        .as_secs();
    let date = header_time(&entry.outcome.headers, "date").unwrap_or(entry.stored_wall);
    let apparent = entry
        .stored_wall
        .duration_since(date)
        .map_or(0, |duration| duration.as_secs());
    let age_value = net::combined_header_in(&entry.outcome.headers, "age")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    apparent.max(age_value).saturating_add(resident)
}

// https://www.rfc-editor.org/rfc/rfc9111#section-4.2.1
fn freshness_lifetime(entry: &CachedResponse) -> Option<u64> {
    let headers = &entry.outcome.headers;
    // https://www.rfc-editor.org/rfc/rfc9111#section-5.2.2.10
    if has_directive(headers, "no-cache") || pragma_no_cache(headers) {
        return Some(0);
    }
    if let Some(seconds) = directive_seconds(headers, "max-age") {
        return Some(seconds);
    }
    if headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("expires"))
    {
        // https://www.rfc-editor.org/rfc/rfc9111#section-5.3
        let Some(expires) = header_time(headers, "expires") else {
            return Some(0);
        };
        let date = header_time(headers, "date").unwrap_or(entry.stored_wall);
        return expires
            .duration_since(date)
            .ok()
            .map(|duration| duration.as_secs());
    }
    heuristic_lifetime(entry)
}

// https://www.rfc-editor.org/rfc/rfc9111#section-5.4
fn pragma_no_cache(headers: &[(String, String)]) -> bool {
    if has_directive(headers, "immutable") {
        return false;
    }
    net::get_decode_split_decoded(headers, "pragma").is_some_and(|values| {
        values
            .iter()
            .any(|value| value.eq_ignore_ascii_case("no-cache"))
    })
}

// https://www.rfc-editor.org/rfc/rfc9111#section-4.2.2
fn heuristic_lifetime(entry: &CachedResponse) -> Option<u64> {
    let headers = &entry.outcome.headers;
    if !has_directive(headers, "public") && !heuristically_cacheable(entry.outcome.status) {
        return None;
    }
    let modified = header_time(headers, "last-modified")?;
    let date = header_time(headers, "date").unwrap_or(entry.stored_wall);
    let interval = date.duration_since(modified).ok()?;
    Some(interval.as_secs() / 10)
}

// https://www.rfc-editor.org/rfc/rfc9110#section-15.1
fn heuristically_cacheable(status: u16) -> bool {
    matches!(
        status,
        200 | 203 | 204 | 300 | 301 | 308 | 404 | 405 | 410 | 414 | 501
    )
}

fn explicit_freshness(outcome: &DialOutcome) -> bool {
    has_directive(&outcome.headers, "max-age")
        || has_directive(&outcome.headers, "s-maxage")
        || outcome
            .headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("expires"))
}

fn vary_star(outcome: &DialOutcome) -> bool {
    net::get_decode_split_decoded(&outcome.headers, "vary")
        .is_some_and(|fields| fields.iter().any(|field| field == "*"))
}

fn vary_matches(entry: &CachedResponse, sent: &[(String, String)]) -> bool {
    let Some(vary) = net::get_decode_split_decoded(&entry.outcome.headers, "vary") else {
        return true;
    };
    vary.iter().filter(|field| !field.is_empty()).all(|field| {
        if field == "*" {
            return false;
        }
        let stored = net::combined_header_in(&entry.request_headers, field);
        stored == net::combined_header_in(sent, field)
    })
}

// https://www.rfc-editor.org/rfc/rfc9111#section-4.3.4
fn not_modified_updates(validators: &Validators, fresh_etag: Option<&str>) -> bool {
    if validators.etag.is_none() && validators.last_modified.is_none() {
        return false;
    }
    match (validators.etag.as_deref(), fresh_etag) {
        (Some(stored), Some(fresh)) => strong_match(stored, fresh),
        _ => true,
    }
}

// https://www.rfc-editor.org/rfc/rfc9111#section-4
fn newest<'a>(variants: impl Iterator<Item = &'a CachedResponse>) -> Option<&'a CachedResponse> {
    variants.max_by_key(|entry| {
        (
            header_time(&entry.outcome.headers, "date"),
            entry.stored_at,
            entry.seq,
        )
    })
}

fn set_header(headers: &mut Vec<(String, String)>, name: &str, value: &str) {
    if let Some((_, existing)) = headers
        .iter_mut()
        .find(|(existing, _)| existing.eq_ignore_ascii_case(name))
    {
        value.clone_into(existing);
    } else {
        headers.push((name.to_ascii_lowercase(), value.to_owned()));
    }
}

fn combined_header(headers: &[(String, String)], name: &str) -> Option<String> {
    net::combined_header_in(headers, name)
}

// https://www.rfc-editor.org/rfc/rfc9110#section-8.8.3.2
fn strong_match(a: &str, b: &str) -> bool {
    !a.trim_start().starts_with("W/") && !b.trim_start().starts_with("W/") && a.trim() == b.trim()
}

// https://www.rfc-editor.org/rfc/rfc9111#section-3.2
fn is_merge_protected(name: &str) -> bool {
    name.eq_ignore_ascii_case("content-length")
        || name.eq_ignore_ascii_case("content-encoding")
        || name.eq_ignore_ascii_case("content-range")
        || name.eq_ignore_ascii_case("transfer-encoding")
        || name.eq_ignore_ascii_case("age")
}

// https://www.rfc-editor.org/rfc/rfc9111#section-3
fn storable(request: &DialRequest, outcome: &DialOutcome, mode: RequestCache) -> bool {
    request.method == Method::GET
        && !has_page_conditional(request)
        && request.policy.exposes_response()
        && storable_outcome(outcome, mode)
}

fn storable_outcome(outcome: &DialOutcome, mode: RequestCache) -> bool {
    !matches!(mode, RequestCache::NoStore)
        // https://fetch.spec.whatwg.org/#concept-http-network-fetch
        && !matches!(outcome.body, ResponseBody::Undecodable)
        // https://www.rfc-editor.org/rfc/rfc9111#section-3
        && (200..600).contains(&outcome.status)
        && outcome.status != 304
        && !outcome.response_type.is_opaque()
        && !has_directive(&outcome.headers, "no-store")
        && !vary_star(outcome)
        && (explicit_freshness(outcome)
            || has_directive(&outcome.headers, "public")
            || heuristically_cacheable(outcome.status))
}

fn cache_usable(request: &DialRequest, initiator: &url::Origin, entry: &CachedResponse) -> bool {
    if entry.outcome.response_type.is_opaque() {
        return false;
    }
    // https://fetch.spec.whatwg.org/#http-cache
    let RequestPolicy::Fetch(cors) = request.policy else {
        return false;
    };
    match cors.mode {
        // https://fetch.spec.whatwg.org/#concept-request-mode
        renderer::CorsMode::SameOrigin => {
            request.url.origin() == *initiator && entry.outcome.final_url.origin() == *initiator
        }
        renderer::CorsMode::NoCors => {
            !matches!(entry.outcome.response_type, net::cors::ResponseType::Cors)
        }
        renderer::CorsMode::Cors => true,
    }
}

fn validators(entry: &CachedResponse) -> Validators {
    Validators {
        etag: net::combined_header_in(&entry.outcome.headers, "etag"),
        last_modified: net::combined_header_in(&entry.outcome.headers, "last-modified"),
        range: None,
    }
}

fn stored_bytes(outcome: &DialOutcome) -> usize {
    match &outcome.body {
        ResponseBody::Bytes(bytes) => bytes.len(),
        ResponseBody::Undecodable | ResponseBody::Streamed(_) => 0,
    }
}

// https://www.rfc-editor.org/rfc/rfc9110#section-14.2
fn answer(entry: &CachedResponse, sent: &[(String, String)]) -> Option<DialOutcome> {
    let stored = complete_partial(&entry.outcome);
    let Some(range) = net::combined_header_in(sent, "range") else {
        return (stored.status != 206).then_some(stored);
    };
    // https://www.rfc-editor.org/rfc/rfc9110#section-13.1.5
    if let Some(condition) = net::combined_header_in(sent, "if-range")
        && !condition.trim_start().starts_with("W/")
    {
        let matches = net::combined_header_in(&stored.headers, "etag")
            .is_some_and(|etag| strong_match(&etag, &condition))
            || net::combined_header_in(&stored.headers, "last-modified")
                .is_some_and(|modified| modified.trim() == condition.trim());
        if !matches {
            return (stored.status != 206).then_some(stored);
        }
    }
    // https://www.rfc-editor.org/rfc/rfc9111#section-3.3
    if stored.status == 206
        && net::combined_header_in(&entry.request_headers, "range").as_deref() == Some(range.trim())
    {
        return Some(stored);
    }
    ranged(&stored, &range)
}

// https://www.rfc-editor.org/rfc/rfc9111#section-3.3
fn complete_partial(outcome: &DialOutcome) -> DialOutcome {
    if outcome.status != 206 {
        return outcome.clone();
    }
    let (Some((start, total)), Some(len)) = (
        content_range(&outcome.headers),
        u64::try_from(stored_bytes(outcome)).ok(),
    ) else {
        return outcome.clone();
    };
    if start != 0 || len != total {
        return outcome.clone();
    }
    let mut complete = outcome.clone();
    complete.status = 200;
    "OK".clone_into(&mut complete.status_text);
    complete
        .headers
        .retain(|(name, _)| !name.eq_ignore_ascii_case("content-range"));
    complete
}

// https://www.rfc-editor.org/rfc/rfc9110#section-14.2
fn completion_range(entry: &CachedResponse, sent: &[(String, String)]) -> Option<String> {
    if entry.outcome.status != 206 || net::combined_header_in(sent, "range").is_some() {
        return None;
    }
    let (start, total) = content_range(&entry.outcome.headers)?;
    let stored_len = u64::try_from(stored_bytes(&entry.outcome)).ok()?;
    if start != 0 || stored_len >= total {
        return None;
    }
    Some(format!("bytes={stored_len}-"))
}

fn ranged(outcome: &DialOutcome, value: &str) -> Option<DialOutcome> {
    let ResponseBody::Bytes(body) = &outcome.body else {
        return None;
    };
    if net::get_decode_split_decoded(&outcome.headers, "content-encoding").is_some_and(|codings| {
        codings
            .iter()
            .any(|coding| !coding.is_empty() && !coding.eq_ignore_ascii_case("identity"))
    }) {
        return None;
    }
    let (stored_start, total) = match outcome.status {
        200 => (0, u64::try_from(body.len()).ok()?),
        206 => content_range(&outcome.headers)?,
        _ => return None,
    };
    // https://www.rfc-editor.org/rfc/rfc9110#section-14.2
    let visible_total = if outcome.status == 206 {
        stored_start.checked_add(u64::try_from(body.len()).ok()?)?
    } else {
        total
    };
    let stored_end = stored_start.checked_add(u64::try_from(body.len()).ok()?)?;
    let (start, end) = parse_range(value, visible_total)?;
    if start < stored_start || end >= stored_end {
        return None;
    }
    let from = usize::try_from(start - stored_start).ok()?;
    let to = usize::try_from(end - stored_start).ok()?;
    let slice = body.get(from..=to)?.to_vec();
    let mut headers: Vec<(String, String)> = outcome
        .headers
        .iter()
        .filter(|(name, _)| {
            !name.eq_ignore_ascii_case("content-range")
                && !name.eq_ignore_ascii_case("content-length")
        })
        .cloned()
        .collect();
    headers.push((
        "content-range".to_owned(),
        format!("bytes {start}-{end}/{total}"),
    ));
    headers.push(("content-length".to_owned(), slice.len().to_string()));
    Some(DialOutcome {
        status: 206,
        status_text: "Partial Content".to_owned(),
        headers,
        body: ResponseBody::Bytes(slice),
        ..outcome.clone()
    })
}

fn content_range(headers: &[(String, String)]) -> Option<(u64, u64)> {
    let value = net::combined_header_in(headers, "content-range")?;
    let (unit, rest) = value.split_once(' ')?;
    if !unit.eq_ignore_ascii_case("bytes") {
        return None;
    }
    let (range, total) = rest.split_once('/')?;
    let (start, _) = range.split_once('-')?;
    Some((start.trim().parse().ok()?, total.trim().parse().ok()?))
}

fn parse_range(value: &str, total: u64) -> Option<(u64, u64)> {
    let spec = value.trim().strip_prefix("bytes=")?.trim();
    if spec.contains(',') {
        return None;
    }
    let (start, end) = spec.split_once('-')?;
    let (start, end) = (start.trim(), end.trim());
    if start.is_empty() {
        let suffix: u64 = end.parse().ok()?;
        if suffix == 0 || total == 0 {
            return None;
        }
        return Some((total.saturating_sub(suffix), total - 1));
    }
    let start: u64 = start.parse().ok()?;
    if start >= total {
        return None;
    }
    let end: u64 = if end.is_empty() {
        total - 1
    } else {
        end.parse::<u64>().ok()?.min(total - 1)
    };
    (start <= end).then_some((start, end))
}

fn insert_cached(entries: &mut Entries, key: CacheKey, entry: CachedResponse) {
    let body_len = stored_bytes(&entry.outcome);
    if body_len > renderer::MAX_RESPONSE_BODY_BYTES {
        return;
    }
    // https://www.rfc-editor.org/rfc/rfc9111#section-4.1
    let variants = entries.entry(key).or_default();
    if let Some(slot) = variants
        .iter_mut()
        .find(|existing| same_representation(existing, &entry))
    {
        *slot = entry;
    } else {
        variants.push(entry);
    }
    while total_entries(entries) > MAX_ENTRIES || total_bytes(entries) > MAX_BYTES {
        let Some((oldest, index)) = oldest_entry(entries) else {
            break;
        };
        let Some(variants) = entries.get_mut(&oldest) else {
            break;
        };
        if index < variants.len() {
            variants.remove(index);
        }
        if variants.is_empty() {
            entries.remove(&oldest);
        }
    }
}

fn total_entries(entries: &Entries) -> usize {
    entries.values().map(Vec::len).sum()
}

fn total_bytes(entries: &Entries) -> usize {
    entries
        .values()
        .flatten()
        .map(|entry| stored_bytes(&entry.outcome))
        .sum()
}

fn oldest_entry(entries: &Entries) -> Option<(CacheKey, usize)> {
    entries
        .iter()
        .flat_map(|(key, variants)| {
            variants
                .iter()
                .enumerate()
                .map(move |(index, entry)| ((entry.stored_at, entry.seq), key.clone(), index))
        })
        .min_by_key(|(age, _, _)| *age)
        .map(|(_, key, index)| (key, index))
}

fn same_representation(a: &CachedResponse, b: &CachedResponse) -> bool {
    let fields = net::get_decode_split_decoded(&a.outcome.headers, "vary")
        .or_else(|| net::get_decode_split_decoded(&b.outcome.headers, "vary"))
        .unwrap_or_default();
    fields
        .iter()
        .filter(|field| !field.is_empty())
        .all(|field| {
            net::combined_header_in(&a.request_headers, field)
                == net::combined_header_in(&b.request_headers, field)
        })
}

// https://wicg.github.io/nav-speculation/no-vary-search.html
fn no_vary_matches(stored: &Url, requested: &Url, outcome: &DialOutcome) -> bool {
    let Some(header) = net::combined_header_in(&outcome.headers, "no-vary-search") else {
        return false;
    };
    let Some(rule) = NoVarySearch::parse(&header) else {
        return false;
    };
    stored.scheme() == requested.scheme()
        && stored.username() == requested.username()
        && stored.password() == requested.password()
        && stored.host_str() == requested.host_str()
        && stored.port_or_known_default() == requested.port_or_known_default()
        && stored.path() == requested.path()
        && rule.query_matches(stored.query(), requested.query())
}

enum VaryingParams {
    All,
    Except(Vec<String>),
    Only(Vec<String>),
}

struct NoVarySearch {
    params: VaryingParams,
    key_order: bool,
}

impl NoVarySearch {
    fn parse(value: &str) -> Option<Self> {
        let mut params: Option<Option<Vec<String>>> = None;
        let mut except = Vec::new();
        let mut key_order = false;
        for member in net::split_header_value(value) {
            let (name, argument) = member
                .split_once('=')
                .map_or((member.as_str(), None), |(name, argument)| {
                    (name, Some(argument.trim()))
                });
            match name.trim().to_ascii_lowercase().as_str() {
                "params" => {
                    params = Some(match argument {
                        None | Some("?1") => None,
                        Some(argument) => Some(parse_param_list(argument)?),
                    });
                }
                "except" => except = parse_param_list(argument?)?,
                "key-order" => match argument {
                    None | Some("?1") => key_order = true,
                    Some("?0") => key_order = false,
                    Some(_) => return None,
                },
                // https://www.ietf.org/archive/id/draft-ietf-httpbis-no-vary-search-03.html#section-3
                _ => {}
            }
        }
        let params = match (params, except) {
            (None, except) if except.is_empty() => VaryingParams::All,
            (Some(None), except) => VaryingParams::Except(except),
            (Some(Some(list)), except) if except.is_empty() => VaryingParams::Only(list),
            (None | Some(Some(_)), _) => return None,
        };
        Some(Self { params, key_order })
    }

    fn query_matches(&self, stored: Option<&str>, requested: Option<&str>) -> bool {
        // https://www.ietf.org/archive/id/draft-ietf-httpbis-no-vary-search-03.html#section-6
        if self.is_default() {
            return stored == requested;
        }
        let mut stored = self.kept_params(stored);
        let mut requested = self.kept_params(requested);
        if self.key_order {
            stored.sort();
            requested.sort();
        }
        stored == requested
    }

    fn is_default(&self) -> bool {
        matches!(self.params, VaryingParams::All) && !self.key_order
    }

    fn kept_params(&self, query: Option<&str>) -> Vec<(String, String)> {
        url::form_urlencoded::parse(query.unwrap_or_default().as_bytes())
            .filter(|(name, _)| match &self.params {
                VaryingParams::All => true,
                VaryingParams::Except(kept) => kept.iter().any(|candidate| candidate == name),
                VaryingParams::Only(ignored) => !ignored.iter().any(|candidate| candidate == name),
            })
            .map(|(name, value)| (name.into_owned(), value.into_owned()))
            .collect()
    }
}

// https://www.ietf.org/archive/id/draft-ietf-httpbis-no-vary-search-03.html#section-5.3
fn parse_param_list(value: &str) -> Option<Vec<String>> {
    let inner = value.strip_prefix('(')?.strip_suffix(')')?;
    let mut names = Vec::new();
    let mut rest = inner.trim_matches([' ', '\t']);
    while !rest.is_empty() {
        let quoted = rest.strip_prefix('"')?;
        let end = quoted.find('"')?;
        names.push(parse_key(&quoted[..end]));
        rest = quoted[end + 1..].trim_matches([' ', '\t']);
    }
    Some(names)
}

// https://www.ietf.org/archive/id/draft-ietf-httpbis-no-vary-search-03.html#section-5.3
fn parse_key(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                decoded.push(b' ');
                index += 1;
            }
            b'%' if index + 2 < bytes.len() => {
                let hex = |byte: u8| u8::try_from(char::from(byte).to_digit(16)?).ok();
                if let (Some(high), Some(low)) = (hex(bytes[index + 1]), hex(bytes[index + 2])) {
                    decoded.push(high * 16 + low);
                    index += 3;
                } else {
                    decoded.push(b'%');
                    index += 1;
                }
            }
            byte => {
                decoded.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn header_url(outcome: &DialOutcome, name: &str, base: &Url) -> Option<Url> {
    let value = net::combined_header_in(&outcome.headers, name)?;
    base.join(value.trim()).ok()
}

// https://www.rfc-editor.org/rfc/rfc9110#section-8.7
fn content_location_target(request: &DialRequest, outcome: &DialOutcome) -> Option<Url> {
    let target = header_url(outcome, "content-location", &request.url)?;
    (target.origin() == request.url.origin()
        && request.policy.exposes_response()
        && storable_outcome(outcome, effective_mode(request)))
    .then_some(target)
}

fn is_conditional_name(name: &str) -> bool {
    name.eq_ignore_ascii_case("if-none-match")
        || name.eq_ignore_ascii_case("if-modified-since")
        || name.eq_ignore_ascii_case("if-match")
        || name.eq_ignore_ascii_case("if-unmodified-since")
        || name.eq_ignore_ascii_case("if-range")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant, SystemTime};

    fn origin() -> url::Origin {
        Url::parse("http://a.test/page").expect("page URL").origin()
    }

    fn request(mode: RequestCache, url: &str) -> DialRequest {
        let url = Url::parse(url).expect("request URL");
        let mut request = DialRequest::new(
            RequestPolicy::Fetch(renderer::CorsRequest {
                mode: renderer::CorsMode::Cors,
                credentials: net::cors::CredentialsMode::Include,
                redirect: net::cors::RedirectMode::Follow,
            }),
            &url,
            &origin(),
        );
        request.cache = mode;
        request
    }

    fn outcome(headers: &[(&str, &str)], body: &[u8]) -> DialOutcome {
        DialOutcome {
            status: 200,
            status_text: "OK".to_owned(),
            final_url: Url::parse("http://a.test/data").expect("URL"),
            content_type: None,
            headers: headers
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect(),
            body: ResponseBody::Bytes(body.to_vec()),
            response_type: net::cors::ResponseType::Basic,
            crossed_origin: false,
            redirect_count: 0,
            cookies_stored: false,
            referrer: None,
        }
    }

    fn store(cache: &HttpCache, request: &DialRequest, outcome: &DialOutcome) {
        cache.remember(request, &request.headers, &origin(), NO_PARTITION, outcome);
    }

    fn decision(cache: &HttpCache, request: &DialRequest) -> Result<CacheDecision, DialFailure> {
        cache.read(request, &request.headers, &origin(), NO_PARTITION)
    }

    fn hit(cache: &HttpCache, request: &DialRequest) -> Option<DialOutcome> {
        match decision(cache, request).expect("read") {
            CacheDecision::Hit(hit) => Some(hit),
            CacheDecision::Miss { .. } => None,
        }
    }

    struct ManualClock {
        wall_base: SystemTime,
        monotonic_base: Instant,
        offset: AtomicU64,
    }

    impl ManualClock {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                wall_base: SystemTime::now(),
                monotonic_base: Instant::now(),
                offset: AtomicU64::new(0),
            })
        }

        fn advance(&self, seconds: u64) {
            self.offset.fetch_add(seconds, Ordering::Relaxed);
        }

        fn clock(self: &Arc<Self>) -> Clock {
            Clock::new(Arc::clone(self))
        }
    }

    impl net::ClockSource for ManualClock {
        fn wall(&self) -> SystemTime {
            self.wall_base + Duration::from_secs(self.offset.load(Ordering::Relaxed))
        }

        fn monotonic(&self) -> Instant {
            self.monotonic_base + Duration::from_secs(self.offset.load(Ordering::Relaxed))
        }
    }

    #[test]
    fn a_fresh_entry_answers_and_a_stale_one_does_not() {
        let manual = ManualClock::new();
        let cache = HttpCache::new(manual.clock());
        let mut first = request(RequestCache::Default, "http://a.test/data");
        first.method = Method::GET;
        store(
            &cache,
            &first,
            &outcome(&[("cache-control", "max-age=60")], b"hit"),
        );
        assert_eq!(hit(&cache, &first).expect("fresh hit").body_bytes(), b"hit");
        manual.advance(61);
        assert!(hit(&cache, &first).is_none(), "an expired entry is a miss");
    }

    // https://www.rfc-editor.org/rfc/rfc9111#section-4
    #[test]
    fn a_cache_hit_does_not_claim_a_stored_cookie() {
        let manual = ManualClock::new();
        let cache = HttpCache::new(manual.clock());
        let mut first = request(RequestCache::Default, "http://a.test/data");
        first.method = Method::GET;
        let mut stored = outcome(&[("cache-control", "max-age=60")], b"hit");
        stored.cookies_stored = true;
        store(&cache, &first, &stored);
        let answered = hit(&cache, &first).expect("fresh hit");
        assert!(
            !answered.cookies_stored,
            "the answer is a cache hit, not a response that stored a cookie"
        );
    }

    #[test]
    fn an_invalid_expires_is_stale_even_with_a_heuristic_last_modified() {
        let cache = HttpCache::new(Clock::default());
        let mut first = request(RequestCache::Default, "http://a.test/data");
        first.method = Method::GET;
        store(
            &cache,
            &first,
            &outcome(
                &[
                    ("expires", "0"),
                    ("last-modified", "Thu, 01 Jan 1970 00:00:00 GMT"),
                ],
                b"stale",
            ),
        );
        assert!(hit(&cache, &first).is_none());
    }

    #[test]
    fn a_private_cache_ignores_s_maxage() {
        let manual = ManualClock::new();
        let cache = HttpCache::new(manual.clock());
        let mut first = request(RequestCache::Default, "http://a.test/data");
        first.method = Method::GET;
        store(
            &cache,
            &first,
            &outcome(&[("cache-control", "max-age=1, s-maxage=3600")], b"x"),
        );
        manual.advance(2);
        assert!(
            hit(&cache, &first).is_none(),
            "a private cache uses max-age, not s-maxage"
        );
    }

    #[test]
    fn request_directives_narrow_the_stored_response() {
        let manual = ManualClock::new();
        let cache = HttpCache::new(manual.clock());
        let mut first = request(RequestCache::Default, "http://a.test/data");
        first.method = Method::GET;
        store(
            &cache,
            &first,
            &outcome(&[("cache-control", "max-age=3600")], b"x"),
        );
        for directive in ["max-age=0", "no-cache", "no-store", "min-fresh=4000"] {
            let mut narrowed = request(RequestCache::Default, "http://a.test/data");
            narrowed.method = Method::GET;
            narrowed.headers = vec![("cache-control".to_owned(), directive.to_owned())];
            assert!(hit(&cache, &narrowed).is_none(), "{directive} must miss");
        }
        let mut stale_ok = request(RequestCache::Default, "http://a.test/data");
        stale_ok.method = Method::GET;
        manual.advance(10);
        stale_ok.headers = vec![("cache-control".to_owned(), "max-stale=60".to_owned())];
        store(
            &cache,
            &first,
            &outcome(&[("cache-control", "max-age=1")], b"x"),
        );
        assert!(
            hit(&cache, &stale_ok).is_some(),
            "max-stale permits a stale entry"
        );
    }

    #[test]
    fn only_if_cached_mode_without_an_entry_is_a_network_error() {
        let cache = HttpCache::new(Clock::default());
        let mut request = request(RequestCache::OnlyIfCached, "http://a.test/data");
        request.method = Method::GET;
        assert!(
            matches!(decision(&cache, &request), Err(DialFailure::Connect)),
            "an only-if-cached miss is a network error"
        );
    }

    #[test]
    fn only_if_cached_mode_serves_a_stale_entry() {
        let manual = ManualClock::new();
        let cache = HttpCache::new(manual.clock());
        let mut first = request(RequestCache::Default, "http://a.test/data");
        first.method = Method::GET;
        store(
            &cache,
            &first,
            &outcome(&[("cache-control", "max-age=1")], b"x"),
        );
        manual.advance(60);
        let mut stale = request(RequestCache::OnlyIfCached, "http://a.test/data");
        stale.method = Method::GET;
        assert_eq!(
            hit(&cache, &stale)
                .expect("a stale entry answers")
                .body_bytes(),
            b"x"
        );
    }

    #[test]
    fn the_only_if_cached_directive_without_an_entry_is_a_504() {
        let cache = HttpCache::new(Clock::default());
        let mut request = request(RequestCache::Default, "http://a.test/data");
        request.method = Method::GET;
        request.headers = vec![("cache-control".to_owned(), "only-if-cached".to_owned())];
        let outcome = hit(&cache, &request).expect("a synthetic response");
        assert_eq!(outcome.status, 504);
        assert!(matches!(&outcome.body, ResponseBody::Bytes(bytes) if bytes.is_empty()));
    }

    #[test]
    fn a_304_revalidation_offers_both_validators() {
        let cache = HttpCache::new(Clock::default());
        let mut first = request(RequestCache::Default, "http://a.test/data");
        first.method = Method::GET;
        store(
            &cache,
            &first,
            &outcome(
                &[
                    ("cache-control", "max-age=0"),
                    ("etag", "\"e\""),
                    ("last-modified", "Thu, 01 Jan 1970 00:00:00 GMT"),
                ],
                b"x",
            ),
        );
        let CacheDecision::Miss { validators, .. } = decision(&cache, &first).expect("read") else {
            panic!("a zero lifetime is a miss");
        };
        let mut outbound = Request::new(Method::GET, first.url);
        validators.apply(&mut outbound).expect("apply");
        assert!(outbound.headers.get("if-none-match").is_some());
        assert!(outbound.headers.get("if-modified-since").is_some());
    }

    #[test]
    fn vary_keeps_each_representation() {
        let cache = HttpCache::new(Clock::default());
        let mut one = request(RequestCache::Default, "http://a.test/data");
        one.method = Method::GET;
        one.headers = vec![("foo".to_owned(), "1".to_owned())];
        let mut two = one.clone();
        two.headers = vec![("foo".to_owned(), "2".to_owned())];
        let headers = [("cache-control", "max-age=60"), ("vary", "foo")];
        store(&cache, &one, &outcome(&headers, b"one"));
        store(&cache, &two, &outcome(&headers, b"two"));
        assert_eq!(hit(&cache, &one).expect("one").body_bytes(), b"one");
        assert_eq!(hit(&cache, &two).expect("two").body_bytes(), b"two");
    }

    #[test]
    fn no_vary_search_matches_a_different_query() {
        let cache = HttpCache::new(Clock::default());
        let mut first = request(RequestCache::Default, "http://a.test/data?a=1&b=2");
        first.method = Method::GET;
        store(
            &cache,
            &first,
            &outcome(
                &[
                    ("cache-control", "max-age=60"),
                    ("no-vary-search", "params"),
                ],
                b"x",
            ),
        );
        let mut second = request(RequestCache::Default, "http://a.test/data?a=9&b=8");
        second.method = Method::GET;
        assert!(hit(&cache, &second).is_some());
        let mut other_path = request(RequestCache::Default, "http://a.test/other?a=9");
        other_path.method = Method::GET;
        assert!(hit(&cache, &other_path).is_none());
    }

    #[test]
    fn an_unsafe_response_invalidates_its_content_location() {
        let cache = HttpCache::new(Clock::default());
        let mut get = request(RequestCache::Default, "http://a.test/data");
        get.method = Method::GET;
        store(
            &cache,
            &get,
            &outcome(&[("cache-control", "max-age=60")], b"old"),
        );
        let mut post = request(RequestCache::Default, "http://a.test/form");
        post.method = Method::parse("POST").expect("POST");
        cache.remember(
            &post,
            &post.headers,
            &origin(),
            NO_PARTITION,
            &outcome(&[("content-location", "/data")], b"new"),
        );
        assert!(
            hit(&cache, &get).is_none(),
            "the Content-Location URL is invalidated"
        );
    }

    #[test]
    fn a_range_request_is_sliced_from_a_stored_body() {
        let cache = HttpCache::new(Clock::default());
        let mut first = request(RequestCache::Default, "http://a.test/data");
        first.method = Method::GET;
        store(
            &cache,
            &first,
            &outcome(&[("cache-control", "max-age=60")], b"0123456789"),
        );
        let mut ranged = request(RequestCache::Default, "http://a.test/data");
        ranged.method = Method::GET;
        ranged.headers = vec![("range".to_owned(), "bytes=2-5".to_owned())];
        let hit = hit(&cache, &ranged).expect("a partial hit");
        assert_eq!(hit.status, 206);
        assert_eq!(hit.body_bytes(), b"2345");
    }

    #[test]
    fn a_complete_request_asks_to_complete_a_stored_partial_response() {
        let cache = HttpCache::new(Clock::default());
        let mut first = request(RequestCache::Default, "http://a.test/data");
        first.method = Method::GET;
        first.headers = vec![("range".to_owned(), "bytes=0-4".to_owned())];
        let mut partial = outcome(
            &[
                ("cache-control", "max-age=60"),
                ("content-range", "bytes 0-4/10"),
            ],
            b"01234",
        );
        partial.status = 206;
        partial.status_text = "Partial Content".to_owned();
        store(&cache, &first, &partial);
        let mut complete = request(RequestCache::Default, "http://a.test/data");
        complete.method = Method::GET;
        let CacheDecision::Miss { validators, .. } = decision(&cache, &complete).expect("read")
        else {
            panic!("a partial response cannot answer a complete request");
        };
        let mut outbound = Request::new(Method::GET, complete.url);
        validators.apply(&mut outbound).expect("apply");
        assert_eq!(outbound.headers.get("range"), Some(b"bytes=5-".as_slice()));
    }

    #[test]
    fn eviction_drops_the_oldest_representation() {
        let manual = ManualClock::new();
        let cache = HttpCache::new(manual.clock());
        let mut requests = Vec::new();
        for index in 0..=MAX_ENTRIES {
            let mut item = request(RequestCache::Default, &format!("http://a.test/{index}"));
            item.method = Method::GET;
            store(
                &cache,
                &item,
                &outcome(&[("cache-control", "max-age=60")], b"x"),
            );
            manual.advance(1);
            requests.push(item);
        }
        assert!(
            hit(&cache, &requests[0]).is_none(),
            "the oldest entry is evicted"
        );
        assert!(hit(&cache, &requests[MAX_ENTRIES]).is_some());
    }

    #[test]
    fn a_same_origin_request_is_not_answered_by_a_cross_origin_redirect() {
        let cache = HttpCache::new(Clock::default());
        let mut first = request(RequestCache::Default, "http://a.test/data");
        first.method = Method::GET;
        let mut redirected = outcome(&[("cache-control", "max-age=60")], b"x");
        redirected.final_url = Url::parse("http://b.test/data").expect("URL");
        store(&cache, &first, &redirected);
        let mut same_origin = request(RequestCache::Default, "http://a.test/data");
        same_origin.method = Method::GET;
        same_origin.policy = RequestPolicy::Fetch(renderer::CorsRequest {
            mode: renderer::CorsMode::SameOrigin,
            credentials: net::cors::CredentialsMode::SameOrigin,
            redirect: net::cors::RedirectMode::Follow,
        });
        assert!(
            hit(&cache, &same_origin).is_none(),
            "a cross-origin redirect's response is not same-origin"
        );
    }

    #[test]
    fn vary_compares_the_headers_that_went_on_the_wire() {
        let cache = HttpCache::new(Clock::default());
        let mut first = request(RequestCache::Default, "http://a.test/data");
        first.method = Method::GET;
        let mut logged_in = first.headers.clone();
        logged_in.push(("cookie".to_owned(), "sid=1".to_owned()));
        cache.remember(
            &first,
            &logged_in,
            &origin(),
            NO_PARTITION,
            &outcome(
                &[("cache-control", "max-age=60"), ("vary", "cookie")],
                b"private",
            ),
        );
        assert!(
            hit(&cache, &first).is_none(),
            "a request with no cookie is a different representation"
        );
        assert!(
            matches!(
                cache
                    .read(&first, &logged_in, &origin(), NO_PARTITION)
                    .expect("read"),
                CacheDecision::Hit(_)
            ),
            "the matching cookie is served"
        );
    }

    #[test]
    fn a_304_only_updates_the_representation_the_dial_named() {
        assert!(
            !not_modified_updates(&Validators::default(), None),
            "an unconditional dial cannot be updated"
        );
        let validators = Validators {
            etag: Some("\"one\"".to_owned()),
            ..Validators::default()
        };
        assert!(not_modified_updates(&validators, Some("\"one\"")));
        assert!(!not_modified_updates(&validators, Some("\"two\"")));
        assert!(
            !not_modified_updates(&validators, Some("W/\"one\"")),
            "a weak validator names no representation"
        );
        assert!(not_modified_updates(&validators, None));
    }

    #[test]
    fn a_hit_reports_its_current_age() {
        let manual = ManualClock::new();
        let cache = HttpCache::new(manual.clock());
        let mut first = request(RequestCache::Default, "http://a.test/data");
        first.method = Method::GET;
        store(
            &cache,
            &first,
            &outcome(&[("cache-control", "max-age=60")], b"x"),
        );
        manual.advance(30);
        let hit = hit(&cache, &first).expect("fresh");
        assert_eq!(
            net::combined_header_in(&hit.headers, "age").as_deref(),
            Some("30")
        );
    }

    #[test]
    fn only_a_stored_prefix_is_completed() {
        let cache = HttpCache::new(Clock::default());
        let mut request_with_range = request(RequestCache::Default, "http://a.test/data");
        request_with_range.method = Method::GET;
        request_with_range.headers = vec![("range".to_owned(), "bytes=4-8".to_owned())];
        let mut middle = outcome(
            &[
                ("cache-control", "max-age=3600"),
                ("content-range", "bytes 4-8/10"),
            ],
            b"45678",
        );
        middle.status = 206;
        store(&cache, &request_with_range, &middle);
        let mut plain = request(RequestCache::Default, "http://a.test/data");
        plain.method = Method::GET;
        assert!(
            hit(&cache, &plain).is_none(),
            "a middle range does not answer a plain request"
        );
        let CacheDecision::Miss { validators, .. } = decision(&cache, &plain).expect("read") else {
            panic!("a plain request dials");
        };
        assert!(
            validators
                .apply(&mut Request::new(Method::GET, plain.url))
                .is_ok(),
            "no completion range is asked for a middle range"
        );
    }

    #[test]
    fn a_stored_partial_covering_everything_answers_a_plain_request() {
        let cache = HttpCache::new(Clock::default());
        let mut ranged = request(RequestCache::Default, "http://a.test/data");
        ranged.method = Method::GET;
        ranged.headers = vec![("range".to_owned(), "bytes=0-4".to_owned())];
        let mut whole = outcome(
            &[
                ("cache-control", "max-age=3600"),
                ("content-range", "bytes 0-4/5"),
            ],
            b"01234",
        );
        whole.status = 206;
        store(&cache, &ranged, &whole);
        let mut plain = request(RequestCache::Default, "http://a.test/data");
        plain.method = Method::GET;
        let hit = hit(&cache, &plain).expect("a complete representation");
        assert_eq!(hit.status, 200);
        assert_eq!(hit.body_bytes(), b"01234");
    }

    #[test]
    fn a_range_is_not_sliced_when_if_range_names_another_representation() {
        let cache = HttpCache::new(Clock::default());
        let mut first = request(RequestCache::Default, "http://a.test/data");
        first.method = Method::GET;
        store(
            &cache,
            &first,
            &outcome(
                &[("cache-control", "max-age=60"), ("etag", "\"e\"")],
                b"0123456789",
            ),
        );
        // https://fetch.spec.whatwg.org/#http-network-or-cache-fetch
        let mut conditional = request(RequestCache::ForceCache, "http://a.test/data");
        conditional.method = Method::GET;
        conditional.headers = vec![
            ("range".to_owned(), "bytes=0-1".to_owned()),
            ("if-range".to_owned(), "\"other\"".to_owned()),
        ];
        let whole = hit(&cache, &conditional).expect("the whole representation");
        assert_eq!(whole.status, 200);
        assert_eq!(whole.body_bytes(), b"0123456789");
        conditional.headers[1].1 = "W/\"e\"".to_owned();
        let slice = hit(&cache, &conditional).expect("the range");
        assert_eq!(slice.status, 206);
        assert_eq!(slice.body_bytes(), b"01");
    }

    #[test]
    fn force_cache_still_validates_a_no_cache_directive() {
        let cache = HttpCache::new(Clock::default());
        let mut first = request(RequestCache::Default, "http://a.test/data");
        first.method = Method::GET;
        store(
            &cache,
            &first,
            &outcome(&[("cache-control", "max-age=60")], b"x"),
        );
        let mut forced = request(RequestCache::ForceCache, "http://a.test/data");
        forced.method = Method::GET;
        forced.headers = vec![("cache-control".to_owned(), "no-cache".to_owned())];
        let CacheDecision::Miss { .. } = decision(&cache, &forced).expect("read") else {
            panic!("a no-cache directive is not answered from the cache");
        };
    }

    #[test]
    fn invalidation_stays_in_its_partition() {
        let cache = HttpCache::new(Clock::default());
        let mut get = request(RequestCache::Default, "http://a.test/data");
        get.method = Method::GET;
        cache.remember(
            &get,
            &get.headers,
            &origin(),
            1,
            &outcome(&[("cache-control", "max-age=60")], b"one"),
        );
        cache.remember(
            &get,
            &get.headers,
            &origin(),
            2,
            &outcome(&[("cache-control", "max-age=60")], b"two"),
        );
        cache.invalidate(&get.url, 1);
        assert!(matches!(
            cache.read(&get, &get.headers, &origin(), 1).expect("read"),
            CacheDecision::Miss { .. }
        ));
        assert!(matches!(
            cache.read(&get, &get.headers, &origin(), 2).expect("read"),
            CacheDecision::Hit(_)
        ));
    }

    #[test]
    fn no_vary_search_decodes_keys_and_ignores_unknown_members() {
        let cache = HttpCache::new(Clock::default());
        let mut first = request(
            RequestCache::Default,
            "http://a.test/data?%C3%A9+%E6%B0%97=1&b=2",
        );
        first.method = Method::GET;
        store(
            &cache,
            &first,
            &outcome(
                &[
                    ("cache-control", "max-age=60"),
                    (
                        "no-vary-search",
                        "params=(\"%C3%A9+%E6%B0%97\"), unknown-member=?1",
                    ),
                ],
                b"x",
            ),
        );
        let mut same = request(
            RequestCache::Default,
            "http://a.test/data?%C3%A9%20%E6%B0%97=9&b=2",
        );
        same.method = Method::GET;
        assert!(hit(&cache, &same).is_some(), "the decoded key matches");
        let mut other = request(
            RequestCache::Default,
            "http://b.test/data?%C3%A9+%E6%B0%97=9&b=2",
        );
        other.method = Method::GET;
        assert!(hit(&cache, &other).is_none(), "another host never matches");
    }

    #[test]
    fn an_unsafe_content_location_is_stored_only_with_its_body() {
        let cache = HttpCache::new(Clock::default());
        let mut post = request(RequestCache::Default, "http://a.test/form");
        post.method = Method::parse("POST").expect("POST");
        let headers = [
            ("cache-control", "max-age=60"),
            ("content-location", "/data"),
        ];
        let head = outcome(&headers, b"");
        assert!(
            cache.may_store(&post, &post.headers, &head),
            "a successful POST with a Content-Location needs its body"
        );
        let mut saved = request(RequestCache::Default, "http://a.test/data");
        saved.method = Method::GET;
        assert!(
            hit(&cache, &saved).is_none(),
            "the head alone stores nothing"
        );
        let mut complete = head.clone();
        complete.body = ResponseBody::Bytes(b"new".to_vec());
        cache.remember(&post, &post.headers, &origin(), NO_PARTITION, &complete);
        assert_eq!(
            hit(&cache, &saved).expect("the stored result").body_bytes(),
            b"new"
        );
        post.headers = vec![("cache-control".to_owned(), "no-store".to_owned())];
        assert!(
            !cache.may_store(&post, &post.headers, &head),
            "a no-store request buffers nothing"
        );
    }

    trait BodyBytes {
        fn body_bytes(&self) -> &[u8];
    }

    impl BodyBytes for DialOutcome {
        fn body_bytes(&self) -> &[u8] {
            match &self.body {
                ResponseBody::Bytes(bytes) => bytes,
                ResponseBody::Undecodable | ResponseBody::Streamed(_) => &[],
            }
        }
    }
}
