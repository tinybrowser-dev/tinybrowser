use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::verdict;

pub const RECORD_SCHEMA: u32 = 2;
pub const FAILURES_FILE: &str = "failures.jsonl.gz";
const MAX_LISTED: usize = 40;
const SUMMARY_FILES: usize = 50;
const SUMMARY_SUBTESTS: usize = 20;

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct Counters {
    #[serde(default)]
    pub tests: u64,
    #[serde(default)]
    pub pass: u64,
    #[serde(default)]
    pub expected_fail: u64,
    #[serde(default)]
    pub unexpected: u64,
    #[serde(default)]
    pub timeout: u64,
    #[serde(default)]
    pub crash: u64,
    #[serde(default)]
    pub error: u64,
    #[serde(default)]
    pub skip: u64,
    #[serde(default)]
    pub subtests: u64,
    #[serde(default)]
    pub subfail: u64,
    #[serde(default)]
    pub subtest_unreported: u64,
    #[serde(default)]
    pub time_ms: f64,
    #[serde(flatten)]
    pub subtest_statuses: BTreeMap<String, u64>,
}

impl Counters {
    pub fn add_assign(&mut self, other: &Self) {
        self.tests += other.tests;
        self.pass += other.pass;
        self.expected_fail += other.expected_fail;
        self.unexpected += other.unexpected;
        self.timeout += other.timeout;
        self.crash += other.crash;
        self.error += other.error;
        self.skip += other.skip;
        self.subtests += other.subtests;
        self.subfail += other.subfail;
        self.subtest_unreported += other.subtest_unreported;
        self.time_ms += other.time_ms;
        for (key, value) in &other.subtest_statuses {
            *self.subtest_statuses.entry(key.clone()).or_insert(0) += value;
        }
    }

    #[must_use]
    pub fn subtest_count(&self, key: &str) -> u64 {
        if key == "subtest_unreported" {
            self.subtest_unreported
        } else {
            self.subtest_statuses.get(key).copied().unwrap_or(0)
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Failure {
    pub test: String,
    #[serde(default)]
    pub subtest: String,
    #[serde(default = "default_fail_status")]
    pub status: String,
}

fn default_fail_status() -> String {
    String::from("FAIL")
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RunRecord {
    pub schema: u32,
    pub product_revision: Option<String>,
    pub wpt_revision: Option<String>,
    pub time_start: Option<f64>,
    pub time_end: Option<f64>,
    pub totals: Counters,
    pub areas: BTreeMap<String, Counters>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failures: Option<Vec<Failure>>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct IndexEntry {
    pub sha: Option<String>,
    pub date: String,
    pub wpt_revision: Option<String>,
    pub totals: Counters,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Regression {
    pub sha: Option<String>,
    pub previous_sha: Option<String>,
    pub wpt_revision: Option<String>,
    pub comparable: bool,
    pub regressions: Vec<Failure>,
    pub new_timeouts: Vec<Failure>,
    #[serde(default)]
    pub newly_measured: Vec<Failure>,
    pub fixes: Vec<Failure>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Attention {
    pub test: String,
    pub subtest: String,
    pub detail: String,
}

#[derive(Clone, Debug)]
pub struct FoldOutput {
    pub merged: Value,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiffResult {
    pub regressions: Vec<Failure>,
    pub new_timeouts: Vec<Failure>,
    pub newly_measured: Vec<Failure>,
    pub fixes: Vec<Failure>,
}

#[derive(Clone, Debug)]
pub struct PublishSummary {
    pub sha: String,
    pub date: String,
    pub diff: Regression,
}

#[must_use]
pub fn is_hard_status(status: &str) -> bool {
    matches!(status, "TIMEOUT" | "CRASH" | "ERROR")
}

#[must_use]
pub fn is_timeout_status(status: &str) -> bool {
    matches!(status, "TIMEOUT" | "EXTERNAL-TIMEOUT")
}

#[must_use]
pub fn is_unmeasured_status(status: &str) -> bool {
    is_hard_status(status) || is_timeout_status(status)
}

#[must_use]
pub fn group_of(test: &str) -> String {
    let parts: Vec<&str> = test.split('/').filter(|part| !part.is_empty()).collect();
    if parts.len() <= 1 {
        return String::from("(root)");
    }
    if parts[0] == "third_party" && parts.get(1) == Some(&"test262") {
        return String::from("test262");
    }
    if parts[1].contains('.') {
        return parts[0].to_owned();
    }
    format!("{}/{}", parts[0], parts[1])
}

#[must_use]
pub fn is_expected(entry: &Value) -> bool {
    let status = entry.get("status").and_then(Value::as_str);
    if let Some(status) = status
        && let Some(intermittent) = entry.get("known_intermittent").and_then(Value::as_array)
        && intermittent
            .iter()
            .any(|item| item.as_str() == Some(status))
    {
        return true;
    }
    match entry.get("expected").and_then(Value::as_str) {
        None => true,
        Some(expected) => status == Some(expected),
    }
}

#[must_use]
pub fn unexpected_subs(subtests: &[Value]) -> Vec<&Value> {
    subtests
        .iter()
        .filter(|sub| {
            sub.get("status").and_then(Value::as_str) != Some("SKIP") && !is_expected(sub)
        })
        .collect()
}

fn format_status_detail(status: &str, result: &Value) -> String {
    let message = result
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if message.is_empty() {
        status.to_owned()
    } else {
        format!("{status} {message}")
    }
}

fn tally_subtests(row: &mut Counters, subtests: &[Value]) -> usize {
    let sub_failed = subtests
        .iter()
        .filter(|sub| {
            !matches!(
                sub.get("status").and_then(Value::as_str),
                Some("PASS" | "SKIP")
            )
        })
        .count();
    row.subtests += u64::try_from(subtests.len()).unwrap_or(u64::MAX);
    row.subfail += u64::try_from(sub_failed).unwrap_or(u64::MAX);
    for sub in subtests {
        let status_name = sub
            .get("status")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map_or_else(
                || String::from("unknown"),
                |value| value.to_ascii_lowercase().replace('-', "_"),
            );
        let key = format!("subtest_{status_name}");
        if key == "subtest_unreported" {
            row.subtest_unreported += 1;
        } else {
            *row.subtest_statuses.entry(key).or_insert(0) += 1;
        }
    }
    sub_failed
}

#[must_use]
pub fn classify_results(results: &[Value]) -> (BTreeMap<String, Counters>, Vec<Attention>) {
    let mut rows: BTreeMap<String, Counters> = BTreeMap::new();
    let mut attention: Vec<Attention> = Vec::new();
    for result in results {
        let test = result.get("test").and_then(Value::as_str).unwrap_or("?");
        let row = rows.entry(group_of(test)).or_default();
        row.tests += 1;
        row.time_ms += result
            .get("duration")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        let subtests = result
            .get("subtests")
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice);
        let mismatched = unexpected_subs(subtests);
        let sub_failed = tally_subtests(row, subtests);
        let status = verdict(result);
        if status == "SKIP" {
            row.skip += 1;
        } else if is_hard_status(status) {
            match status {
                "TIMEOUT" => row.timeout += 1,
                "CRASH" => row.crash += 1,
                "ERROR" => row.error += 1,
                _ => {}
            }
            attention.push(Attention {
                test: test.to_owned(),
                subtest: String::new(),
                detail: format_status_detail(status, result),
            });
        } else if !is_expected(result) || !mismatched.is_empty() {
            row.unexpected += 1;
            if !is_expected(result) {
                attention.push(Attention {
                    test: test.to_owned(),
                    subtest: String::new(),
                    detail: format_status_detail(status, result),
                });
            }
            for sub in mismatched {
                attention.push(Attention {
                    test: test.to_owned(),
                    subtest: sub
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("?")
                        .to_owned(),
                    detail: sub
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or("ERROR")
                        .to_owned(),
                });
            }
        } else if sub_failed > 0 || !matches!(status, "PASS" | "OK") {
            row.expected_fail += 1;
        } else {
            row.pass += 1;
        }
    }
    (rows, attention)
}

#[must_use]
pub fn format_results_table(results: &[Value]) -> Option<String> {
    if results.is_empty() {
        return None;
    }
    let (rows, attention) = classify_results(results);
    let width = rows.keys().map(String::len).max().unwrap_or(0);
    let mut out = String::new();
    let header = format!(
        "{:<width$}  tests  pass  expfail  unexp  timeout  crash  error  skip  subtests  subfail  test time",
        "directory"
    );
    let divider = "-".repeat(header.len());
    out.push_str(&header);
    out.push('\n');
    out.push_str(&divider);
    out.push('\n');

    let format_line = |name: &str, row: &Counters| -> String {
        format!(
            "{:<width$}  {:5}  {:4}  {:7}  {:5}  {:7}  {:5}  {:5}  {:4}  {:8}  {:7}  {:8.1}s",
            name,
            row.tests,
            row.pass,
            row.expected_fail,
            row.unexpected,
            row.timeout,
            row.crash,
            row.error,
            row.skip,
            row.subtests,
            row.subfail,
            row.time_ms / 1000.0
        )
    };

    let mut totals = Counters::default();
    for (directory, row) in &rows {
        totals.add_assign(row);
        out.push_str(&format_line(directory, row));
        out.push('\n');
    }
    out.push_str(&divider);
    out.push('\n');
    out.push_str(&format_line("TOTAL", &totals));
    out.push_str("\n\n");
    let _ = writeln!(out, "needs attention ({} entries):", attention.len());
    for entry in attention.iter().take(MAX_LISTED) {
        let sub = if entry.subtest.is_empty() {
            "(test)"
        } else {
            entry.subtest.as_str()
        };
        let _ = writeln!(out, "  {:10} {} :: {sub}", entry.detail, entry.test);
    }
    if attention.len() > MAX_LISTED {
        let _ = writeln!(out, "  ... and {} more", attention.len() - MAX_LISTED);
    }
    Some(out)
}

pub fn load_report(path: &Path) -> Result<Value, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let data: Value = serde_json::from_str(&text)
        .map_err(|error| format!("{} is not valid JSON ({error})", path.display()))?;
    if !data.is_object() {
        return Err(format!("{} is not a wptreport object", path.display()));
    }
    Ok(data)
}

fn fold_metadata(
    report: &Path,
    data: &Value,
    run_info: &mut Option<Value>,
    subsuites: &mut Map<String, Value>,
    warnings: &mut Vec<String>,
) {
    match data.get("run_info") {
        Some(info) if info.is_object() => {
            if let Some(existing) = run_info.as_ref() {
                if existing != info {
                    warnings.push(format!(
                        "{}: run_info differs from the first report",
                        report.display()
                    ));
                }
            } else {
                *run_info = Some(info.clone());
            }
        }
        Some(Value::Null) | None => {}
        Some(_) => {
            warnings.push(format!(
                "{}: run_info is not an object; ignored",
                report.display()
            ));
        }
    }
    if let Some(obj) = data.get("subsuites").and_then(Value::as_object) {
        for (key, value) in obj {
            if let Some(existing) = subsuites.get(key) {
                if existing != value {
                    warnings.push(format!(
                        "{}: subsuite '{key}' differs; first wins",
                        report.display()
                    ));
                }
            } else {
                subsuites.insert(key.clone(), value.clone());
            }
        }
    }
}

pub fn fold_reports(loaded: &[(PathBuf, Value)]) -> Result<FoldOutput, String> {
    let mut results: Vec<Value> = Vec::new();
    let mut where_seen: BTreeMap<(String, String), usize> = BTreeMap::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut run_info: Option<Value> = None;
    let mut subsuites: Map<String, Value> = Map::new();
    let mut starts: Vec<f64> = Vec::new();
    let mut ends: Vec<f64> = Vec::new();
    let mut empty: Vec<String> = Vec::new();

    for (index, (report, data)) in loaded.iter().enumerate() {
        let chunk = match data.get("results") {
            None | Some(Value::Null) => &[][..],
            Some(Value::Array(items)) => items.as_slice(),
            Some(_) => {
                warnings.push(format!(
                    "{}: results is not a list; skipped",
                    report.display()
                ));
                continue;
            }
        };
        if chunk.is_empty() {
            empty.push(report.display().to_string());
        }
        for result in chunk {
            let Some(test) = result
                .as_object()
                .and_then(|obj| obj.get("test"))
                .and_then(Value::as_str)
                .filter(|test| !test.is_empty())
            else {
                return Err(format!("a result in {} has no test id", report.display()));
            };
            let subsuite = result
                .get("subsuite")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let key = (subsuite, test.to_owned());
            if let Some(&first) = where_seen.get(&key) {
                if first == index {
                    return Err(format!("{test} appears twice in {}", report.display()));
                }
                return Err(format!(
                    "{test} appears in {} and {}",
                    loaded[first].0.display(),
                    report.display()
                ));
            }
            where_seen.insert(key, index);
            results.push(result.clone());
        }
        fold_metadata(report, data, &mut run_info, &mut subsuites, &mut warnings);
        if let Some(start) = data.get("time_start").and_then(Value::as_f64) {
            starts.push(start);
        }
        if let Some(end) = data.get("time_end").and_then(Value::as_f64) {
            ends.push(end);
        }
    }

    if !empty.is_empty() {
        warnings.push(format!("no results in {}", empty.join(", ")));
    }

    results.sort_by(|left, right| {
        let l_sub = left.get("subsuite").and_then(Value::as_str).unwrap_or("");
        let r_sub = right.get("subsuite").and_then(Value::as_str).unwrap_or("");
        let l_test = left.get("test").and_then(Value::as_str).unwrap_or("");
        let r_test = right.get("test").and_then(Value::as_str).unwrap_or("");
        (l_sub, l_test).cmp(&(r_sub, r_test))
    });

    let mut merged = Map::new();
    merged.insert(String::from("results"), Value::Array(results));
    if let Some(info) = run_info {
        merged.insert(String::from("run_info"), info);
    }
    if !subsuites.is_empty() {
        merged.insert(String::from("subsuites"), Value::Object(subsuites));
    }
    if let Some(min_start) = starts.into_iter().reduce(f64::min) {
        merged.insert(String::from("time_start"), json!(min_start));
    }
    if let Some(max_end) = ends.into_iter().reduce(f64::max) {
        merged.insert(String::from("time_end"), json!(max_end));
    }
    Ok(FoldOutput {
        merged: Value::Object(merged),
        warnings,
    })
}

#[must_use]
pub fn epoch_seconds(value: Option<&Value>) -> Option<f64> {
    let number = value?.as_f64()?;
    if number <= 0.0 {
        return None;
    }
    if number > 1e11 {
        Some(number / 1000.0)
    } else {
        Some(number)
    }
}

pub fn read_corpus(path: &Path) -> Result<BTreeMap<String, u64>, String> {
    let raw = std::fs::read(path)
        .map_err(|error| format!("corpus: cannot read {}: {error}", path.display()))?;
    let bytes = if raw.len() >= 2 && raw[0] == 0x1f && raw[1] == 0x8b {
        let mut decoder = GzDecoder::new(&raw[..]);
        let mut out = Vec::new();
        decoder
            .read_to_end(&mut out)
            .map_err(|error| format!("corpus: cannot read {}: {error}", path.display()))?;
        out
    } else {
        raw
    };
    let data: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("corpus: cannot read {}: {error}", path.display()))?;
    let Some(obj) = data.as_object() else {
        return Err(format!("corpus: {} is not a JSON object", path.display()));
    };
    let mut counts = BTreeMap::new();
    for (test, total) in obj {
        if let Some(count) = total.as_u64()
            && count > 0
        {
            counts.insert(test.clone(), count);
        }
    }
    if counts.is_empty() {
        return Err(format!("corpus: {} has no subtest counts", path.display()));
    }
    Ok(counts)
}

pub fn enroll_subtests(
    rows: &mut BTreeMap<String, Counters>,
    reported: &BTreeMap<String, u64>,
    corpus: &BTreeMap<String, u64>,
) -> u64 {
    let mut missing = 0;
    for (test, &expected) in corpus {
        let seen = reported.get(test).copied().unwrap_or(0);
        if expected <= seen {
            continue;
        }
        let gap = expected - seen;
        let row = rows.entry(group_of(test)).or_default();
        row.subtests += gap;
        row.subtest_unreported += gap;
        missing += gap;
    }
    missing
}

#[must_use]
pub fn build_record(
    merged: &Value,
    product_revision: Option<String>,
    corpus: Option<&BTreeMap<String, u64>>,
) -> RunRecord {
    let results = merged
        .get("results")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    let (mut rows, attention) = classify_results(results);
    if let Some(corpus) = corpus {
        let mut reported = BTreeMap::new();
        for result in results {
            if let Some(test) = result.get("test").and_then(Value::as_str)
                && !test.is_empty()
            {
                let count = result
                    .get("subtests")
                    .and_then(Value::as_array)
                    .map_or(0, |subs| u64::try_from(subs.len()).unwrap_or(u64::MAX));
                reported.insert(test.to_owned(), count);
            }
        }
        enroll_subtests(&mut rows, &reported, corpus);
    }
    let mut totals = Counters::default();
    for row in rows.values() {
        totals.add_assign(row);
    }
    let wpt_revision = merged
        .get("run_info")
        .and_then(Value::as_object)
        .and_then(|info| info.get("revision"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let failures: Vec<Failure> = attention
        .into_iter()
        .map(|entry| Failure {
            test: entry.test,
            subtest: entry.subtest,
            status: entry
                .detail
                .split_once(' ')
                .map_or(entry.detail.as_str(), |(head, _)| head)
                .to_owned(),
        })
        .collect();
    RunRecord {
        schema: RECORD_SCHEMA,
        product_revision,
        wpt_revision,
        time_start: epoch_seconds(merged.get("time_start")),
        time_end: epoch_seconds(merged.get("time_end")),
        totals,
        areas: rows,
        failures: Some(failures),
    }
}

#[must_use]
pub fn diff_failures(previous: &[Failure], current: &[Failure], comparable: bool) -> DiffResult {
    let before: BTreeMap<(&str, &str), &Failure> = previous
        .iter()
        .map(|entry| ((entry.test.as_str(), entry.subtest.as_str()), entry))
        .collect();
    let after: BTreeMap<(&str, &str), &Failure> = current
        .iter()
        .map(|entry| ((entry.test.as_str(), entry.subtest.as_str()), entry))
        .collect();
    if !comparable {
        return DiffResult {
            regressions: Vec::new(),
            new_timeouts: Vec::new(),
            newly_measured: Vec::new(),
            fixes: Vec::new(),
        };
    }

    let failing_tests: BTreeSet<&str> = after.keys().map(|(test, _)| *test).collect();
    let mut regressions = Vec::new();
    let mut new_timeouts = Vec::new();
    let mut newly_measured = Vec::new();

    for (&key, &entry) in &after {
        if before.contains_key(&key) {
            continue;
        }
        if is_timeout_status(&entry.status) {
            new_timeouts.push(entry.clone());
            continue;
        }
        let file_row = before.get(&(key.0, ""));
        if !key.1.is_empty()
            && let Some(row) = file_row
            && is_unmeasured_status(&row.status)
        {
            newly_measured.push(entry.clone());
        } else {
            regressions.push(entry.clone());
        }
    }

    let mut fixes = Vec::new();
    for (&key, &entry) in &before {
        if after.contains_key(&key) {
            continue;
        }
        if !key.1.is_empty() || !failing_tests.contains(key.0) {
            fixes.push(entry.clone());
        }
    }

    DiffResult {
        regressions,
        new_timeouts,
        newly_measured,
        fixes,
    }
}

#[must_use]
pub fn read_failures(path: &Path) -> Vec<Failure> {
    let Ok(file) = File::open(path) else {
        return Vec::new();
    };
    let reader = BufReader::new(GzDecoder::new(file));
    let mut out = Vec::new();
    for line in reader.lines() {
        let Ok(line) = line else {
            return Vec::new();
        };
        let Ok(entry) = serde_json::from_str::<Failure>(&line) else {
            continue;
        };
        if !entry.test.is_empty() {
            out.push(entry);
        }
    }
    out
}

pub fn write_failures(path: &Path, failures: &[Failure]) -> io::Result<()> {
    let file = File::create(path)?;
    let mut encoder = GzEncoder::new(BufWriter::new(file), Compression::default());
    for entry in failures {
        serde_json::to_writer(&mut encoder, entry)?;
        encoder.write_all(b"\n")?;
    }
    encoder.finish()?.flush()
}

#[must_use]
pub fn format_utc_timestamp(seconds: u64) -> String {
    let days = i64::try_from(seconds / 86_400).unwrap_or(0);
    let rem = seconds % 86_400;
    let hour = rem / 3600;
    let minute = (rem % 3600) / 60;
    let second = rem % 60;

    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = u64::try_from(z.rem_euclid(146_097)).unwrap_or(0);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = i64::try_from(yoe).unwrap_or(0) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = y + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

#[must_use]
pub fn record_date(record: &RunRecord) -> String {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "positive unix seconds fit in u64"
    )]
    let seconds = record
        .time_end
        .filter(|value| value.is_finite() && *value > 0.0)
        .map_or_else(
            || {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |duration| duration.as_secs())
            },
            |value| value as u64,
        );
    format_utc_timestamp(seconds)
}

#[must_use]
pub fn index_entry(record: &RunRecord) -> IndexEntry {
    IndexEntry {
        sha: record.product_revision.clone(),
        date: record_date(record),
        wpt_revision: record.wpt_revision.clone(),
        totals: record.totals.clone(),
    }
}

pub fn publish(
    record_path: &Path,
    data_dir: &Path,
    sha: Option<&str>,
) -> Result<PublishSummary, String> {
    let text = std::fs::read_to_string(record_path)
        .map_err(|error| format!("cannot read {}: {error}", record_path.display()))?;
    let mut record: RunRecord = serde_json::from_str(&text)
        .map_err(|error| format!("{} is not valid JSON ({error})", record_path.display()))?;
    if let Some(sha) = sha
        && !sha.is_empty()
    {
        record.product_revision = Some(sha.to_owned());
    }
    let Some(product_revision) = record
        .product_revision
        .clone()
        .filter(|value| !value.is_empty())
    else {
        return Err(String::from(
            "the record has no product revision; pass --sha",
        ));
    };

    std::fs::create_dir_all(data_dir)
        .map_err(|error| format!("cannot create {}: {error}", data_dir.display()))?;
    let latest_path = data_dir.join("latest.json");
    let failures_path = data_dir.join(FAILURES_FILE);

    let current_failures = record.failures.take().unwrap_or_default();
    let previous_failures = read_failures(&failures_path);
    let previous: Option<RunRecord> = if latest_path.is_file() {
        if let Some(prev) = std::fs::read_to_string(&latest_path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
        {
            Some(prev)
        } else {
            eprintln!("publish: ignoring unreadable latest.json");
            None
        }
    } else {
        None
    };

    let comparable = previous
        .as_ref()
        .is_some_and(|prev| prev.wpt_revision == record.wpt_revision);
    let diff_parts = diff_failures(&previous_failures, &current_failures, comparable);
    let diff = Regression {
        sha: record.product_revision.clone(),
        previous_sha: previous
            .as_ref()
            .and_then(|prev| prev.product_revision.clone()),
        wpt_revision: record.wpt_revision.clone(),
        comparable,
        regressions: diff_parts.regressions,
        new_timeouts: diff_parts.new_timeouts,
        newly_measured: diff_parts.newly_measured,
        fixes: diff_parts.fixes,
    };

    let latest_json = serde_json::to_string(&record).map_err(|error| error.to_string())?;
    std::fs::write(&latest_path, latest_json)
        .map_err(|error| format!("cannot write {}: {error}", latest_path.display()))?;
    write_failures(&failures_path, &current_failures)
        .map_err(|error| format!("cannot write {}: {error}", failures_path.display()))?;
    let diff_path = data_dir.join("regressions.json");
    let diff_json = serde_json::to_string(&diff).map_err(|error| error.to_string())?;
    std::fs::write(&diff_path, diff_json)
        .map_err(|error| format!("cannot write {}: {error}", diff_path.display()))?;

    let index_path = data_dir.join("index.json");
    let mut index: Vec<IndexEntry> = if index_path.is_file() {
        if let Some(items) = std::fs::read_to_string(&index_path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
        {
            items
        } else {
            eprintln!("publish: ignoring unreadable index.json");
            Vec::new()
        }
    } else {
        Vec::new()
    };
    let entry = index_entry(&record);
    let date = entry.date.clone();
    index.retain(|item| item.sha != entry.sha);
    index.insert(0, entry);
    let index_json = serde_json::to_string(&index).map_err(|error| error.to_string())?;
    std::fs::write(&index_path, index_json)
        .map_err(|error| format!("cannot write {}: {error}", index_path.display()))?;

    Ok(PublishSummary {
        sha: product_revision,
        date,
        diff,
    })
}

#[must_use]
pub fn format_regression_summary(diff: &Regression) -> String {
    if !diff.comparable {
        return String::from(
            "### WPT regressions\n\nNot comparable: the first publish, or the WPT pin changed.\n",
        );
    }
    let previous = diff
        .previous_sha
        .as_deref()
        .map_or("", |sha| &sha[..sha.len().min(8)]);
    let files: Vec<&Failure> = diff
        .regressions
        .iter()
        .filter(|entry| entry.subtest.is_empty())
        .collect();
    let tests: Vec<&Failure> = diff
        .regressions
        .iter()
        .filter(|entry| !entry.subtest.is_empty())
        .collect();

    let mut out = format!("### WPT regressions vs `{previous}`\n");
    if diff.regressions.is_empty() {
        out.push_str("No new failures.\n");
    }
    for entry in files.iter().take(SUMMARY_FILES) {
        let _ = writeln!(out, "- `{}` ({})", entry.test, entry.status);
    }
    if files.len() > SUMMARY_FILES {
        let _ = writeln!(out, "- ... and {} more files", files.len() - SUMMARY_FILES);
    }
    if !tests.is_empty() {
        let _ = writeln!(out, "\n{} new subtest failures, e.g.", tests.len());
        for entry in tests.iter().take(SUMMARY_SUBTESTS) {
            let _ = writeln!(
                out,
                "- `{}` :: `{}` ({})",
                entry.test, entry.subtest, entry.status
            );
        }
        if tests.len() > SUMMARY_SUBTESTS {
            let _ = writeln!(
                out,
                "- ... and {} more subtests",
                tests.len() - SUMMARY_SUBTESTS
            );
        }
    }
    let _ = writeln!(
        out,
        "\nNew timeouts: {}; newly measured: {}; fixes: {}.",
        diff.new_timeouts.len(),
        diff.newly_measured.len(),
        diff.fixes.len()
    );
    out
}

#[must_use]
pub fn emit_typescript() -> String {
    format!(
        r"/**
 * Generated by `cargo run -p wpt-report -- emit-ts`. Do not edit by hand.
 * Single owner: `tools/wpt-report/src/record.rs`.
 */

export const RECORD_SCHEMA = {RECORD_SCHEMA} as const

export type Counters = {{
  tests: number
  pass: number
  expected_fail: number
  unexpected: number
  timeout: number
  crash: number
  error: number
  skip: number
  subtests: number
  subfail: number
  subtest_unreported: number
  time_ms: number
  [key: `subtest_${{string}}`]: number
}}

export type Failure = {{
  test: string
  subtest: string
  status: string
}}

export type RunRecord = {{
  schema: typeof RECORD_SCHEMA
  product_revision: string | null
  wpt_revision: string | null
  time_start: number | null
  time_end: number | null
  totals: Counters
  areas: Record<string, Counters>
  failures?: Array<Failure>
}}

export type IndexEntry = {{
  sha: string | null
  date: string
  wpt_revision: string | null
  totals: Counters
}}

export type Regression = {{
  sha: string | null
  previous_sha: string | null
  wpt_revision: string | null
  comparable: boolean
  regressions: Array<Failure>
  new_timeouts: Array<Failure>
  newly_measured: Array<Failure>
  fixes: Array<Failure>
}}

export type Snapshot = {{
  latest: RunRecord | null
  index: Array<IndexEntry>
  regressions: Regression | null
}}
"
    )
}
