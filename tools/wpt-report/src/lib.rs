pub mod record;

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Status {
    Pass,
    Fail,
    Timeout,
    Error,
    Skip,
}

impl Status {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Fail => "FAIL",
            Self::Timeout => "TIMEOUT",
            Self::Error => "ERROR",
            Self::Skip => "SKIP",
        }
    }

    #[must_use]
    pub fn is_hard(self) -> bool {
        matches!(self, Self::Timeout | Self::Error)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Testharness,
    Crashtest,
    Reftest,
    Testdriver,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SubtestStatus {
    Pass,
    Fail,
    Timeout,
    NotRun,
    PreconditionFailed,
    Skip,
}

impl SubtestStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Fail => "FAIL",
            Self::Timeout => "TIMEOUT",
            Self::NotRun => "NOTRUN",
            Self::PreconditionFailed | Self::Skip => "SKIP",
        }
    }

    #[must_use]
    pub fn from_harness_code(code: u64) -> Option<Self> {
        match code {
            0 => Some(Self::Pass),
            1 => Some(Self::Fail),
            2 => Some(Self::Timeout),
            3 => Some(Self::NotRun),
            4 => Some(Self::PreconditionFailed),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Subtest {
    pub name: String,
    pub status: SubtestStatus,
    pub message: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Outcome {
    pub id: String,
    pub status: Status,
    pub message: Option<String>,
    pub duration: Duration,
    pub subtests: Vec<Subtest>,
    pub testharness: bool,
}

pub fn write_report(path: &Path, outcomes: &[Outcome]) -> io::Result<()> {
    let results: Vec<Value> = outcomes
        .iter()
        .map(|outcome| {
            let overall = if outcome.testharness {
                match outcome.status {
                    Status::Pass | Status::Fail => "OK",
                    Status::Timeout => "TIMEOUT",
                    Status::Error => "ERROR",
                    Status::Skip => "SKIP",
                }
            } else {
                outcome.status.as_str()
            };
            let subtests: Vec<Value> = if outcome.testharness {
                outcome
                    .subtests
                    .iter()
                    .map(|subtest| {
                        json!({
                            "name": subtest.name,
                            "status": subtest.status.as_str(),
                            "message": subtest.message,
                            "expected": "PASS",
                            "known_intermittent": [],
                        })
                    })
                    .collect()
            } else {
                Vec::new()
            };
            json!({
                "test": outcome.id,
                "subsuite": "",
                "status": overall,
                "message": outcome.message,
                "duration": outcome.duration.as_secs_f64() * 1000.0,
                "known_intermittent": [],
                "subtests": subtests,
            })
        })
        .collect();
    let report = json!({ "results": results });
    let file = File::create(path)?;
    let mut writer = BufWriter::new(file);
    serde_json::to_writer(&mut writer, &report)?;
    writer.write_all(b"\n")
}

pub struct Comparison {
    pub regressions: Vec<String>,
    pub fixes: Vec<String>,
    pub new_failures: Vec<String>,
    pub missing: Vec<String>,
}

impl Comparison {
    #[must_use]
    pub fn failed(&self) -> bool {
        !self.regressions.is_empty() || !self.new_failures.is_empty()
    }
}

pub fn compare(baseline: &Path, outcomes: &[Outcome]) -> io::Result<Comparison> {
    let text = std::fs::read_to_string(baseline)?;
    let report: Value = serde_json::from_str(&text).map_err(io::Error::other)?;
    let results = report
        .get("results")
        .and_then(Value::as_array)
        .ok_or_else(|| io::Error::other("baseline has no results array"))?;
    let mut previous = BTreeMap::new();
    for result in results {
        let Some(id) = result.get("test").and_then(Value::as_str) else {
            continue;
        };
        previous.insert(id.to_owned(), verdict(result).to_owned());
    }
    let mut comparison = Comparison {
        regressions: Vec::new(),
        fixes: Vec::new(),
        new_failures: Vec::new(),
        missing: Vec::new(),
    };
    let mut seen = BTreeSet::new();
    for outcome in outcomes {
        seen.insert(outcome.id.as_str());
        let current = outcome.status.as_str();
        match previous.get(&outcome.id).map(String::as_str) {
            Some("PASS" | "SKIP") if current == "FAIL" => {
                comparison.regressions.push(outcome.id.clone());
            }
            Some("PASS") if matches!(current, "SKIP" | "TIMEOUT" | "ERROR") => {
                comparison.regressions.push(outcome.id.clone());
            }
            Some("FAIL" | "TIMEOUT" | "ERROR" | "CRASH") if current == "PASS" => {
                comparison.fixes.push(outcome.id.clone());
            }
            None if current == "FAIL" => comparison.new_failures.push(outcome.id.clone()),
            _ => {}
        }
    }
    for test in previous.keys() {
        if !seen.contains(test.as_str()) {
            comparison.missing.push(test.clone());
        }
    }
    Ok(comparison)
}

#[must_use]
pub fn verdict(result: &Value) -> &str {
    let status = result.get("status").and_then(Value::as_str);
    if let Some(status) = status
        && matches!(status, "TIMEOUT" | "ERROR" | "CRASH")
    {
        return status;
    }
    let subtests = result.get("subtests").and_then(Value::as_array);
    if let Some(subtests) = subtests
        && subtests.iter().any(|subtest| {
            !matches!(
                subtest.get("status").and_then(Value::as_str),
                Some("PASS" | "SKIP")
            )
        })
    {
        return "FAIL";
    }
    match status {
        Some("OK" | "PASS") => "PASS",
        Some("FAIL") => "FAIL",
        Some(other) => other,
        None => {
            if subtests.is_some_and(|subtests| {
                subtests
                    .iter()
                    .any(|subtest| subtest.get("status").and_then(Value::as_str) == Some("PASS"))
            }) {
                "PASS"
            } else {
                "SKIP"
            }
        }
    }
}

pub fn summarize(outcomes: &[Outcome], wall: Duration, verbose: bool) {
    let mut pass = 0;
    let mut fail = 0;
    let mut timeout = 0;
    let mut error = 0;
    let mut skip = 0;
    let mut failures = Vec::new();
    let mut slowest: Vec<&Outcome> = outcomes.iter().collect();
    for outcome in outcomes {
        match outcome.status {
            Status::Pass => pass += 1,
            Status::Fail => {
                fail += 1;
                failures.push(outcome);
            }
            Status::Timeout => timeout += 1,
            Status::Error => error += 1,
            Status::Skip => skip += 1,
        }
    }
    slowest.sort_by_key(|outcome| std::cmp::Reverse(outcome.duration));
    let count = u32::try_from(outcomes.len()).unwrap_or(u32::MAX);
    println!(
        "{} tests in {:.1?} ({:.2} ms/test): {pass} pass, {fail} fail, {timeout} timeout, \
         {error} error, {skip} skip",
        outcomes.len(),
        wall,
        wall.as_secs_f64() * 1000.0 / f64::from(count)
    );
    if verbose {
        for outcome in failures {
            println!(
                "FAIL {} :: {}",
                outcome.id,
                outcome.message.as_deref().unwrap_or("")
            );
        }
        for outcome in slowest.iter().take(10) {
            println!("SLOW {:>8.1?} {}", outcome.duration, outcome.id);
        }
    }
}

pub fn print_comparison(comparison: &Comparison) {
    for test in &comparison.regressions {
        println!("REGRESSION {test}");
    }
    for test in &comparison.new_failures {
        println!("NEW-FAILURE {test}");
    }
    for test in &comparison.fixes {
        println!("FIXED {test}");
    }
    println!(
        "baseline diff: {} regressions, {} new failures, {} fixes, {} tests not selected",
        comparison.regressions.len(),
        comparison.new_failures.len(),
        comparison.fixes.len(),
        comparison.missing.len()
    );
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{Outcome, Status, compare, verdict};

    fn outcome(id: &str, status: Status) -> Outcome {
        Outcome {
            id: id.into(),
            status,
            message: None,
            duration: Duration::ZERO,
            subtests: Vec::new(),
            testharness: true,
        }
    }

    fn scratch(name: &str, contents: &str) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("wpt-report-{name}-{}.json", std::process::id()));
        std::fs::write(&path, contents).expect("baseline written");
        path
    }

    #[test]
    fn a_diff_separates_regressions_from_fixes() {
        let path = scratch(
            "compare",
            r#"{"results":[
                {"test":"a","subtests":[{"status":"PASS"}]},
                {"test":"b","subtests":[{"status":"FAIL"}]},
                {"test":"c","subtests":[{"status":"SKIP"}]},
                {"test":"d","subtests":[{"status":"PASS"}]},
                {"test":"mixed","subtests":[{"status":"PASS"},{"status":"FAIL"}]},
                {"test":"timed","status":"TIMEOUT","subtests":[]},
                {"test":"broken","status":"ERROR","subtests":[{"status":"PASS"}]},
                {"test":"gone","subtests":[{"status":"PASS"}]}
            ]}"#,
        );
        let outcomes = [
            outcome("a", Status::Fail),
            outcome("b", Status::Pass),
            outcome("c", Status::Pass),
            outcome("d", Status::Skip),
            outcome("mixed", Status::Fail),
            outcome("timed", Status::Pass),
            outcome("broken", Status::Fail),
            outcome("new", Status::Fail),
        ];
        let comparison = compare(&path, &outcomes).expect("compared");
        std::fs::remove_file(&path).expect("baseline removed");
        assert_eq!(comparison.regressions, ["a", "d"]);
        assert_eq!(comparison.fixes, ["b", "timed"]);
        assert_eq!(comparison.new_failures, ["new"]);
        assert_eq!(comparison.missing, ["gone"]);
        assert!(comparison.failed());
    }

    #[test]
    fn a_hard_status_outranks_its_subtests() {
        let path = scratch(
            "hard",
            r#"{"results":[
                {"test":"broken","status":"ERROR","subtests":[{"status":"PASS"}]},
                {"test":"timed","status":"TIMEOUT","subtests":[]},
                {"test":"crashed","status":"CRASH","subtests":[]}
            ]}"#,
        );
        let outcomes = [
            outcome("broken", Status::Pass),
            outcome("timed", Status::Fail),
            outcome("crashed", Status::Skip),
        ];
        let comparison = compare(&path, &outcomes).expect("compared");
        std::fs::remove_file(&path).expect("baseline removed");
        assert_eq!(comparison.fixes, ["broken"]);
        assert_eq!(comparison.regressions, [] as [String; 0]);
        assert_eq!(comparison.new_failures, [] as [String; 0]);
    }

    #[test]
    fn the_verdict_rule_matches_the_shared_cases() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../wpt/verdict-cases.json");
        let text = std::fs::read_to_string(&path).expect("read verdict cases");
        let cases: serde_json::Value = serde_json::from_str(&text).expect("parse cases");
        let cases = cases["cases"].as_array().expect("cases array");
        assert!(!cases.is_empty(), "no cases pinned");
        for case in cases {
            let result = &case["result"];
            let expected = case["verdict"].as_str().expect("verdict string");
            assert_eq!(
                verdict(result),
                expected,
                "{}",
                case["why"].as_str().unwrap_or("case")
            );
        }
    }

    #[test]
    fn website_record_types_match_generated_schema() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../website/src/lib/record-types.ts");
        let actual = std::fs::read_to_string(&path).expect("read website/src/lib/record-types.ts");
        assert_eq!(
            actual,
            crate::record::emit_typescript(),
            "website/src/lib/record-types.ts drifted; run: cargo run -p wpt-report -- emit-ts --out website/src/lib/record-types.ts"
        );
    }

    #[test]
    fn diff_failures_separates_newly_measured_and_timeouts() {
        use crate::record::{Failure, diff_failures};
        let prev = vec![
            Failure {
                test: "/a.html".into(),
                subtest: String::new(),
                status: "TIMEOUT".into(),
            },
            Failure {
                test: "/b.html".into(),
                subtest: "old".into(),
                status: "FAIL".into(),
            },
        ];
        let curr = vec![
            Failure {
                test: "/a.html".into(),
                subtest: "sub1".into(),
                status: "FAIL".into(),
            },
            Failure {
                test: "/c.html".into(),
                subtest: String::new(),
                status: "TIMEOUT".into(),
            },
            Failure {
                test: "/d.html".into(),
                subtest: "new".into(),
                status: "FAIL".into(),
            },
        ];
        let diff = diff_failures(&prev, &curr, true);
        assert_eq!(diff.newly_measured.len(), 1);
        assert_eq!(diff.newly_measured[0].test, "/a.html");
        assert_eq!(diff.new_timeouts.len(), 1);
        assert_eq!(diff.new_timeouts[0].test, "/c.html");
        assert_eq!(diff.regressions.len(), 1);
        assert_eq!(diff.regressions[0].test, "/d.html");
        assert_eq!(diff.fixes.len(), 1);
        assert_eq!(diff.fixes[0].test, "/b.html");
    }
}
