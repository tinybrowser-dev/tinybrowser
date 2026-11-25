// https://w3c.github.io/webdriver-bidi/#module-storage

use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use browser::{CookieFilter, CookieRecord, CookieSameSite, DirectCookie, UserContextId};
use serde_json::{Map, Value, json};

use crate::bidi::BidiState;
use crate::bidi::browsing_context::resolve_context;
use crate::bidi::network::decode_bytes_value;
use crate::bidi::protocol::{
    BidiError, BidiResult, optional_bool, optional_str, require_object, require_str,
};

struct ResolvedPartition {
    user_context: UserContextId,
    source_origin: Option<String>,
}

impl ResolvedPartition {
    fn key_json(&self) -> Value {
        let mut map = Map::new();
        map.insert("userContext".into(), json!(self.user_context.as_str()));
        if let Some(ref origin) = self.source_origin {
            map.insert("sourceOrigin".into(), json!(origin));
        }
        Value::Object(map)
    }
}

async fn resolve_partition(
    state: &Arc<BidiState>,
    partition: Option<&Value>,
) -> Result<ResolvedPartition, BidiError> {
    let Some(val) = partition else {
        return Ok(ResolvedPartition {
            user_context: UserContextId::default_context(),
            source_origin: None,
        });
    };
    let obj = require_object(val, "partition")?;
    let typ = require_str(obj, "type")?;
    match typ {
        "context" => {
            let ctx = require_str(obj, "context")?;
            let (tab, frame) = resolve_context(state, ctx).await?;
            let url = tab.document_url_in(frame).await.unwrap_or_default();
            let source_origin = url::Url::parse(&url).ok().and_then(|u| {
                matches!(u.scheme(), "http" | "https").then(|| u.origin().ascii_serialization())
            });
            Ok(ResolvedPartition {
                user_context: tab.user_context().clone(),
                source_origin,
            })
        }
        "storageKey" => {
            let user_context = match optional_str(obj, "userContext")? {
                Some(raw) => {
                    let uc = UserContextId::new(raw);
                    let known = state.browser.user_contexts().await.unwrap_or_default();
                    if !known.contains(&uc) {
                        return Err(BidiError::no_such_user_context(raw));
                    }
                    uc
                }
                None => UserContextId::default_context(),
            };
            let source_origin = optional_str(obj, "sourceOrigin")?.map(str::to_owned);
            Ok(ResolvedPartition {
                user_context,
                source_origin,
            })
        }
        other => Err(BidiError::invalid_argument(format!(
            "invalid partition type: {other}"
        ))),
    }
}

fn parse_same_site(raw: &str) -> Result<CookieSameSite, BidiError> {
    match raw {
        "strict" => Ok(CookieSameSite::Strict),
        "lax" => Ok(CookieSameSite::Lax),
        "none" => Ok(CookieSameSite::None),
        "default" => Ok(CookieSameSite::Default),
        other => Err(BidiError::invalid_argument(format!(
            "invalid sameSite: {other}"
        ))),
    }
}

fn parse_cookie_filter(val: Option<&Value>) -> Result<CookieFilter, BidiError> {
    let Some(val) = val else {
        return Ok(CookieFilter::default());
    };
    let obj = require_object(val, "filter")?;
    let name = optional_str(obj, "name")?.map(str::to_owned);
    let value = match obj.get("value") {
        None => None,
        Some(v) => {
            let bytes = decode_bytes_value(v)?;
            Some(
                String::from_utf8(bytes)
                    .map_err(|_| BidiError::invalid_argument("cookie value must be UTF-8"))?,
            )
        }
    };
    let domain = optional_str(obj, "domain")?.map(str::to_owned);
    let path = optional_str(obj, "path")?.map(str::to_owned);
    let size = match obj.get("size") {
        None => None,
        Some(v) => Some(
            v.as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .ok_or_else(|| {
                    BidiError::invalid_argument("size must be a non-negative integer")
                })?,
        ),
    };
    let http_only = optional_bool(obj, "httpOnly")?;
    let secure = optional_bool(obj, "secure")?;
    let same_site = match optional_str(obj, "sameSite")? {
        None => None,
        Some(raw) => Some(parse_same_site(raw)?),
    };
    let expiry = match obj.get("expiry") {
        None => None,
        Some(v) => Some(
            v.as_u64()
                .ok_or_else(|| BidiError::invalid_argument("expiry must be an integer"))?,
        ),
    };
    Ok(CookieFilter {
        name,
        value,
        domain,
        path,
        size,
        http_only,
        secure,
        same_site,
        expiry,
        partition_key: None,
    })
}

fn bidi_cookie_json(record: &CookieRecord) -> Value {
    let mut map = Map::new();
    map.insert("name".into(), json!(record.name));
    map.insert(
        "value".into(),
        json!({
            "type": "string",
            "value": record.value,
        }),
    );
    map.insert("domain".into(), json!(record.domain));
    map.insert("path".into(), json!(record.path));
    map.insert("size".into(), json!(record.name.len() + record.value.len()));
    map.insert("httpOnly".into(), json!(record.http_only));
    map.insert("secure".into(), json!(record.secure));
    let same_site = match record.same_site {
        CookieSameSite::Strict => "strict",
        CookieSameSite::Lax => "lax",
        CookieSameSite::None | CookieSameSite::Default => "none",
    };
    map.insert("sameSite".into(), json!(same_site));
    if let Some(expiry) = record.expiry
        && let Ok(since) = expiry.duration_since(UNIX_EPOCH)
    {
        map.insert("expiry".into(), json!(since.as_secs()));
    }
    Value::Object(map)
}

// https://w3c.github.io/webdriver-bidi/#command-storage-getCookies
pub(crate) async fn get_cookies(state: &Arc<BidiState>, params: &Value) -> BidiResult {
    let map = require_object(params, "params")?;
    let partition = resolve_partition(state, map.get("partition")).await?;
    let filter = parse_cookie_filter(map.get("filter"))?;
    let records = state
        .browser
        .matching_cookie_records_in(&partition.user_context, filter)
        .await
        .map_err(|err| BidiError::unknown_error(err.to_string()))?;
    let cookies: Vec<Value> = records.iter().map(bidi_cookie_json).collect();
    Ok(json!({
        "cookies": cookies,
        "partitionKey": partition.key_json(),
    }))
}

// https://w3c.github.io/webdriver-bidi/#command-storage-setCookie
pub(crate) async fn set_cookie(state: &Arc<BidiState>, params: &Value) -> BidiResult {
    let map = require_object(params, "params")?;
    let partition = resolve_partition(state, map.get("partition")).await?;
    let cookie_val = map
        .get("cookie")
        .ok_or_else(|| BidiError::invalid_argument("cookie is required"))?;
    let obj = require_object(cookie_val, "cookie")?;
    let name = require_str(obj, "name")?;
    let val_obj = obj
        .get("value")
        .ok_or_else(|| BidiError::invalid_argument("cookie.value is required"))?;
    let value_bytes = decode_bytes_value(val_obj)?;
    let value =
        String::from_utf8(value_bytes).unwrap_or_else(|err| BASE64.encode(err.into_bytes()));
    let domain = require_str(obj, "domain")?;
    let path = optional_str(obj, "path")?.unwrap_or("/");
    let http_only = optional_bool(obj, "httpOnly")?.unwrap_or(false);
    let secure = optional_bool(obj, "secure")?.unwrap_or(false);
    let same_site = match optional_str(obj, "sameSite")? {
        None => CookieSameSite::None,
        Some(raw) => parse_same_site(raw)?,
    };
    let expiry = match obj.get("expiry") {
        None => None,
        Some(v) => {
            let secs = v.as_u64().ok_or_else(|| {
                BidiError::invalid_argument("expiry must be a non-negative integer")
            })?;
            let time = UNIX_EPOCH
                .checked_add(Duration::from_secs(secs))
                .ok_or_else(|| BidiError::invalid_argument("expiry out of range"))?;
            Some(time)
        }
    };
    let stored = state
        .browser
        .set_direct_cookie_in(
            &partition.user_context,
            DirectCookie {
                name: name.to_owned(),
                value,
                domain: domain.to_owned(),
                host_only: !domain.starts_with('.'),
                path: path.to_owned(),
                secure,
                http_only,
                same_site,
                expiry,
                partition_key: None,
            },
        )
        .await
        .map_err(|err| BidiError::unknown_error(err.to_string()))?;
    if !stored {
        return Err(BidiError::unable_to_set_cookie("cookie rejected"));
    }
    Ok(json!({ "partitionKey": partition.key_json() }))
}

// https://w3c.github.io/webdriver-bidi/#command-storage-deleteCookies
pub(crate) async fn delete_cookies(state: &Arc<BidiState>, params: &Value) -> BidiResult {
    let map = require_object(params, "params")?;
    let partition = resolve_partition(state, map.get("partition")).await?;
    let filter = parse_cookie_filter(map.get("filter"))?;
    let _ = state
        .browser
        .delete_cookies_in(&partition.user_context, filter)
        .await
        .map_err(|err| BidiError::unknown_error(err.to_string()))?;
    Ok(json!({ "partitionKey": partition.key_json() }))
}
