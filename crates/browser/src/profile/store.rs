//! Durable profile backing: the cookie store and its file codec.
//!
//! The browser context owns live state and uses this store only at open and
//! persistence boundaries.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use net::{Agent, CookieRecord, CookieSameSite};

use super::Profile;
use crate::storage::LocalStorage;

const COOKIES_VERSION: &str = "tinybrowser-cookies-v1";
const LOCAL_STORAGE_VERSION: &str = "tinybrowser-localstorage-v1";
static COOKIE_TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Durable backing for one Profile under `XDG_DATA_HOME`.
pub(crate) struct ProfileStore {
    root: PathBuf,
    _lock: File,
    disk: Mutex<()>,
}

impl ProfileStore {
    /// Opens and exclusively locks the on-disk store for `profile` under `data_home`.
    ///
    /// # Errors
    ///
    /// The directory cannot be created or another process owns the profile.
    pub(crate) fn open_in(data_home: &Path, profile: &Profile) -> io::Result<Self> {
        let root = data_home
            .join("tinybrowser")
            .join("profiles")
            .join(profile.name().as_str());
        fs::create_dir_all(&root)?;
        restrict(&root, 0o700)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.join("lock"))?;
        lock.try_lock().map_err(io::Error::from)?;
        Ok(Self {
            root,
            _lock: lock,
            disk: Mutex::new(()),
        })
    }

    pub(crate) fn load_into(&self, agent: &Agent) -> io::Result<()> {
        let path = self.cookies_path();
        let _disk = self.lock_disk();
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        let records = std::str::from_utf8(&bytes)
            .ok()
            .and_then(|text| parse_cookies(text).ok());
        if let Some(records) = records {
            agent.import_cookies(records);
            return Ok(());
        }
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        fs::rename(
            &path,
            path.with_file_name(format!("cookies.corrupt.{stamp}")),
        )?;
        Ok(())
    }

    pub(crate) fn save_from(&self, agent: &Agent) -> io::Result<()> {
        let dir = &self.root;
        let _disk = self.lock_disk();
        fs::create_dir_all(dir)?;
        restrict(dir, 0o700)?;
        let path = dir.join("cookies");
        let seq = COOKIE_TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let tmp = dir.join(format!("cookies.{}.{seq}.tmp", std::process::id()));
        let encoded = encode_cookies(&agent.export_cookies());
        let write_result = (|| {
            let mut file = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
            restrict(&tmp, 0o600)?;
            file.write_all(encoded.as_bytes())?;
            file.sync_all()?;
            fs::rename(&tmp, &path)?;
            restrict(&path, 0o600)?;
            File::open(dir)?.sync_all()?;
            Ok(())
        })();
        if let Err(error) = write_result {
            let _remove_result = fs::remove_file(&tmp);
            return Err(error);
        }
        Ok(())
    }

    fn lock_disk(&self) -> std::sync::MutexGuard<'_, ()> {
        self.disk
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Loads the profile's local storage areas into `storage`.
    ///
    /// # Errors
    ///
    /// The file exists but cannot be read or moved aside when corrupt.
    pub(crate) fn load_local_storage(&self, storage: &LocalStorage) -> io::Result<()> {
        let path = self.local_storage_path();
        let _disk = self.lock_disk();
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        let rows = std::str::from_utf8(&bytes)
            .ok()
            .and_then(|text| parse_local_storage(text).ok());
        if let Some(rows) = rows {
            storage.import(rows);
            return Ok(());
        }
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        fs::rename(
            &path,
            path.with_file_name(format!("localstorage.corrupt.{stamp}")),
        )?;
        Ok(())
    }

    /// Persists every dirty local storage area.
    ///
    /// # Errors
    ///
    /// The file cannot be written or the directory cannot be synced.
    pub(crate) fn save_local_storage(&self, storage: &LocalStorage) -> io::Result<()> {
        let dir = &self.root;
        let _disk = self.lock_disk();
        if !storage.is_dirty() {
            return Ok(());
        }
        fs::create_dir_all(dir)?;
        restrict(dir, 0o700)?;
        let path = dir.join("localstorage");
        let seq = COOKIE_TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let tmp = dir.join(format!("localstorage.{}.{seq}.tmp", std::process::id()));
        let encoded = encode_local_storage(&storage.export());
        let write_result = (|| {
            let mut file = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
            restrict(&tmp, 0o600)?;
            file.write_all(encoded.as_bytes())?;
            file.sync_all()?;
            fs::rename(&tmp, &path)?;
            restrict(&path, 0o600)?;
            File::open(dir)?.sync_all()?;
            Ok(())
        })();
        if let Err(error) = write_result {
            let _remove_result = fs::remove_file(&tmp);
            return Err(error);
        }
        storage.mark_clean();
        Ok(())
    }

    fn cookies_path(&self) -> PathBuf {
        self.root.join("cookies")
    }

    fn local_storage_path(&self) -> PathBuf {
        self.root.join("localstorage")
    }
}

/// Restricts `path` to `mode` on Unix; a no-op elsewhere.
fn restrict(path: &Path, mode: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
    Ok(())
}

fn encode_cookies(records: &[CookieRecord]) -> String {
    let mut out = String::from(COOKIES_VERSION);
    out.push('\n');
    for record in records {
        push_escaped(&mut out, &record.name);
        out.push('\t');
        push_escaped(&mut out, &record.value);
        out.push('\t');
        out.push_str(&optional_millis(record.expiry));
        out.push('\t');
        push_escaped(&mut out, &record.domain);
        out.push('\t');
        push_escaped(&mut out, &record.path);
        out.push('\t');
        out.push_str(&unix_millis(record.created).to_string());
        out.push('\t');
        out.push_str(&unix_millis(record.last_access).to_string());
        out.push('\t');
        out.push(if record.host_only { '1' } else { '0' });
        out.push('\t');
        out.push(if record.secure { '1' } else { '0' });
        out.push('\t');
        out.push(if record.http_only { '1' } else { '0' });
        out.push('\t');
        out.push_str(same_site_token(record.same_site));
        // Appended after the versioned fields: a file written before CHIPS
        // has no twelfth field, which parses as the unpartitioned cookie it
        // was.
        out.push('\t');
        if let Some(key) = &record.partition_key {
            push_escaped(&mut out, key);
        }
        out.push('\n');
    }
    out
}

fn parse_cookies(text: &str) -> io::Result<Vec<CookieRecord>> {
    let mut lines = text.lines();
    let Some(version) = lines.next() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "empty cookie file",
        ));
    };
    if version != COOKIES_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unsupported cookie file",
        ));
    }
    let mut records = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        records.push(parse_cookie_line(line)?);
    }
    Ok(records)
}

fn parse_cookie_line(line: &str) -> io::Result<CookieRecord> {
    let mut parts = line.split('\t');
    let name = unescape(next_field(&mut parts, "name")?);
    let value = unescape(next_field(&mut parts, "value")?);
    let expiry = parse_optional_millis(next_field(&mut parts, "expiry")?)?;
    let domain = unescape(next_field(&mut parts, "domain")?);
    let path = unescape(next_field(&mut parts, "path")?);
    let created = millis_to_time(parse_i64(next_field(&mut parts, "created")?)?);
    let last_access = millis_to_time(parse_i64(next_field(&mut parts, "last_access")?)?);
    let host_only = parse_flag(next_field(&mut parts, "host_only")?)?;
    let secure = parse_flag(next_field(&mut parts, "secure")?)?;
    let http_only = parse_flag(next_field(&mut parts, "http_only")?)?;
    let same_site = parse_same_site(next_field(&mut parts, "same_site")?)?;
    let partition_key = parts.next().map(unescape).filter(|key| !key.is_empty());
    Ok(CookieRecord {
        name,
        value,
        expiry,
        domain,
        path,
        created,
        last_access,
        host_only,
        secure,
        http_only,
        same_site,
        partition_key,
    })
}

fn next_field<'a>(parts: &mut std::str::Split<'a, char>, name: &str) -> io::Result<&'a str> {
    parts.next().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("missing cookie field {name}"),
        )
    })
}

fn push_escaped(out: &mut String, value: &str) {
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            other => out.push(other),
        }
    }
}

fn unescape(value: &str) -> String {
    let mut out = String::new();
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('\\') | None => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
        }
    }
    out
}

fn unix_millis(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => i64::try_from(duration.as_millis()).unwrap_or(i64::MAX),
        Err(error) => -i64::try_from(error.duration().as_millis()).unwrap_or(i64::MAX),
    }
}

fn millis_to_time(millis: i64) -> SystemTime {
    let duration = Duration::from_millis(millis.unsigned_abs());
    if millis >= 0 {
        UNIX_EPOCH + duration
    } else {
        UNIX_EPOCH.checked_sub(duration).unwrap_or(UNIX_EPOCH)
    }
}

fn optional_millis(time: Option<SystemTime>) -> String {
    time.map_or_default(|expiry| unix_millis(expiry).to_string())
}

fn parse_optional_millis(raw: &str) -> io::Result<Option<SystemTime>> {
    if raw.is_empty() {
        return Ok(None);
    }
    Ok(Some(millis_to_time(parse_i64(raw)?)))
}

fn parse_i64(raw: &str) -> io::Result<i64> {
    raw.parse()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "cookie timestamp"))
}

fn parse_flag(raw: &str) -> io::Result<bool> {
    match raw {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err(io::Error::new(io::ErrorKind::InvalidData, "cookie flag")),
    }
}

fn same_site_token(value: CookieSameSite) -> &'static str {
    match value {
        CookieSameSite::Strict => "Strict",
        CookieSameSite::Lax => "Lax",
        CookieSameSite::None => "None",
        CookieSameSite::Default => "Default",
    }
}

fn parse_same_site(raw: &str) -> io::Result<CookieSameSite> {
    match raw {
        "Strict" => Ok(CookieSameSite::Strict),
        "Lax" => Ok(CookieSameSite::Lax),
        "None" => Ok(CookieSameSite::None),
        "Default" => Ok(CookieSameSite::Default),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "cookie same-site",
        )),
    }
}

/// One `origin \t key \t value` row per local storage entry.
fn encode_local_storage(rows: &[(String, String, String)]) -> String {
    let mut out = String::from(LOCAL_STORAGE_VERSION);
    out.push('\n');
    for (origin, key, value) in rows {
        push_escaped(&mut out, origin);
        out.push('\t');
        push_escaped(&mut out, key);
        out.push('\t');
        push_escaped(&mut out, value);
        out.push('\n');
    }
    out
}

fn parse_local_storage(text: &str) -> io::Result<Vec<(String, String, String)>> {
    let mut lines = text.lines();
    let Some(version) = lines.next() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "empty local storage file",
        ));
    };
    if version != LOCAL_STORAGE_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unsupported local storage file",
        ));
    }
    let mut rows = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split('\t');
        let mut field = |name: &str| {
            parts.next().map(unescape).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("missing local storage field {name}"),
                )
            })
        };
        let origin = field("origin")?;
        let key = field("key")?;
        let value = field("value")?;
        rows.push((origin, key, value));
    }
    Ok(rows)
}
