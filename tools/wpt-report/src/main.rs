use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde_json::Value;
use wpt_report::record::{
    Regression, build_record, emit_typescript, fold_reports, format_regression_summary,
    format_results_table, load_report, publish, read_corpus,
};

fn usage() -> &'static str {
    "usage: wpt-report <summarize|merge|publish|summary|emit-ts> [args...]"
}

fn die(scope: &str, message: &str, code: u8) -> ExitCode {
    eprintln!("wpt-report: {scope}: {message}");
    ExitCode::from(code)
}

fn take_value(args: &[String], index: &mut usize, arg: &str) -> Result<String, String> {
    if let Some((_, value)) = arg.split_once('=') {
        return Ok(value.to_owned());
    }
    let next = *index + 1;
    if let Some(value) = args.get(next)
        && !value.starts_with('-')
    {
        *index = next;
        return Ok(value.clone());
    }
    Err(format!("{arg} needs a value"))
}

fn cmd_summarize(args: &[String]) -> ExitCode {
    let Some(path) = args.first().map(PathBuf::from) else {
        return die("summarize", "need a report path", 2);
    };
    let data = match load_report(&path) {
        Ok(data) => data,
        Err(error) => return die("summarize", &error, 1),
    };
    let results = data
        .get("results")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    let Some(table) = format_results_table(results) else {
        return die(
            "summarize",
            &format!("no results in report {}", path.display()),
            1,
        );
    };
    print!("{table}");
    ExitCode::SUCCESS
}

struct MergeOptions {
    reports: Vec<PathBuf>,
    out: Option<PathBuf>,
    summary_out: Option<PathBuf>,
    product_revision: Option<String>,
    corpus_path: Option<PathBuf>,
    corpus_dir: Option<PathBuf>,
    strict: bool,
}

fn parse_merge_args(args: &[String]) -> Result<MergeOptions, String> {
    let mut opts = MergeOptions {
        reports: Vec::new(),
        out: None,
        summary_out: None,
        product_revision: None,
        corpus_path: None,
        corpus_dir: None,
        strict: false,
    };
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        let name = arg.split('=').next().unwrap_or(arg);
        match name {
            "--out" => match take_value(args, &mut i, arg) {
                Ok(value) if !value.is_empty() => opts.out = Some(PathBuf::from(value)),
                _ => return Err(String::from("--out needs a value")),
            },
            "--summary-out" => match take_value(args, &mut i, arg) {
                Ok(value) if !value.is_empty() => opts.summary_out = Some(PathBuf::from(value)),
                _ => return Err(String::from("--summary-out needs a value")),
            },
            "--product-revision" => match take_value(args, &mut i, arg) {
                Ok(value) if !value.is_empty() => opts.product_revision = Some(value),
                _ => return Err(String::from("--product-revision needs a value")),
            },
            "--corpus" => match take_value(args, &mut i, arg) {
                Ok(value) if !value.is_empty() => opts.corpus_path = Some(PathBuf::from(value)),
                _ => return Err(String::from("--corpus needs a value")),
            },
            "--corpus-dir" => match take_value(args, &mut i, arg) {
                Ok(value) if !value.is_empty() => opts.corpus_dir = Some(PathBuf::from(value)),
                _ => return Err(String::from("--corpus-dir needs a value")),
            },
            "--strict-skips" => opts.strict = true,
            _ if arg.starts_with('-') => {
                return Err(format!(
                    "merge takes report files, not {arg} (chunk flags belong to `run`)"
                ));
            }
            _ => opts.reports.push(PathBuf::from(arg)),
        }
        i += 1;
    }
    if opts.reports.is_empty() {
        return Err(String::from("merge needs at least one report file"));
    }
    Ok(opts)
}

fn write_json_file(path: &Path, body: &str) -> Result<(), String> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    fs::write(path, body).map_err(|error| error.to_string())
}

fn resolve_pinned_corpus(
    merged: &Value,
    corpus_dir: Option<&Path>,
) -> Result<Option<std::collections::BTreeMap<String, u64>>, String> {
    let Some(revision) = merged
        .get("run_info")
        .and_then(|info| info.get("revision"))
        .and_then(Value::as_str)
        .filter(|rev| !rev.is_empty())
    else {
        return Ok(None);
    };
    let dir = corpus_dir.unwrap_or_else(|| Path::new("tools/wpt/corpus"));
    let pinned = dir.join(format!("{revision}.json.gz"));
    if !pinned.is_file() {
        return Err(format!(
            "no pinned corpus for WPT revision {revision} at {};              generate it with: uv run python tools/wpt/cli.py corpus --revision {revision}",
            pinned.display()
        ));
    }
    read_corpus(&pinned).map(Some).map_err(|error| {
        eprintln!("{error}");
        format!(
            "cannot read pinned corpus for WPT revision {revision} at {}",
            pinned.display()
        )
    })
}

fn write_summary_record(
    summary_path: &Path,
    merged: &Value,
    product_revision: Option<String>,
    corpus: Option<&std::collections::BTreeMap<String, u64>>,
) -> Result<(), String> {
    let record = build_record(merged, product_revision, corpus);
    let body = serde_json::to_string(&record)
        .map_err(|_| String::from("failed to serialize run record"))?;
    write_json_file(summary_path, &body)?;
    let failure_count = record.failures.as_ref().map_or(0, Vec::len);
    eprintln!(
        "record -> {} ({} tests, {failure_count} failures)",
        summary_path.display(),
        record.totals.tests
    );
    if record.totals.subtest_unreported > 0 && record.totals.subtests > 0 {
        let passing = record.totals.subtest_count("subtest_pass");
        let subtests = record.totals.subtests;
        #[allow(
            clippy::cast_precision_loss,
            reason = "display percentage over subtest counts"
        )]
        let pct = 100.0 * (passing as f64) / (subtests as f64);
        eprintln!(
            "corpus: {} subtests not reported by the run, score {passing}/{subtests} = {pct:.1}%",
            record.totals.subtest_unreported
        );
    }
    Ok(())
}

fn cmd_merge(args: &[String]) -> ExitCode {
    let opts = match parse_merge_args(args) {
        Ok(opts) => opts,
        Err(message) => return die("merge", &message, 2),
    };
    let MergeOptions {
        reports,
        out,
        summary_out,
        product_revision,
        corpus_path,
        corpus_dir,
        strict,
    } = opts;

    let mut corpus = if let Some(path) = &corpus_path {
        match read_corpus(path) {
            Ok(map) => Some(map),
            Err(error) => {
                eprintln!("{error}");
                return die("merge", &format!("cannot use corpus {}", path.display()), 2);
            }
        }
    } else {
        None
    };

    let mut loaded: Vec<(PathBuf, Value)> = Vec::new();
    let mut skipped: usize = 0;
    for report in &reports {
        match load_report(report) {
            Ok(data) => loaded.push((report.clone(), data)),
            Err(problem) => {
                eprintln!("wpt: merge: skipping {problem}");
                skipped += 1;
            }
        }
    }
    if loaded.is_empty() {
        return die(
            "merge",
            &format!("no usable report among {}", reports.len()),
            2,
        );
    }

    let fold = match fold_reports(&loaded) {
        Ok(fold) => fold,
        Err(error) => return die("merge", &error, 2),
    };
    for warning in &fold.warnings {
        eprintln!("wpt: merge: {warning}");
    }

    if summary_out.is_some() && corpus.is_none() {
        match resolve_pinned_corpus(&fold.merged, corpus_dir.as_deref()) {
            Ok(found) => corpus = found,
            Err(error) => return die("merge", &error, 2),
        }
    }

    let results = fold
        .merged
        .get("results")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);

    if let Some(out_path) = &out {
        let Ok(body) = serde_json::to_string(&fold.merged) else {
            return die("merge", "failed to serialize merged report", 2);
        };
        if let Err(error) = write_json_file(out_path, &body) {
            return die("merge", &error, 2);
        }
        eprintln!(
            "merged {} reports -> {} ({} results)",
            loaded.len(),
            out_path.display(),
            results.len()
        );
    }

    if let Some(summary_path) = &summary_out
        && let Err(error) = write_summary_record(
            summary_path,
            &fold.merged,
            product_revision,
            corpus.as_ref(),
        )
    {
        return die("merge", &error, 2);
    }

    let code = if let Some(table) = format_results_table(results) {
        print!("{table}");
        0
    } else {
        eprintln!("no results to summarize");
        1
    };

    if strict && skipped > 0 {
        eprintln!(
            "wpt: merge: {skipped} of {} chunk reports were unusable",
            reports.len()
        );
        return ExitCode::from(if code == 0 { 2 } else { code });
    }
    ExitCode::from(code)
}

fn cmd_publish(args: &[String]) -> ExitCode {
    let mut record_path: Option<PathBuf> = None;
    let mut data_dir: Option<PathBuf> = None;
    let mut sha: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        let name = arg.split('=').next().unwrap_or(arg);
        match name {
            "--record" => match take_value(args, &mut i, arg) {
                Ok(value) if !value.is_empty() => record_path = Some(PathBuf::from(value)),
                _ => return die("publish", "--record needs a value", 2),
            },
            "--data" => match take_value(args, &mut i, arg) {
                Ok(value) if !value.is_empty() => data_dir = Some(PathBuf::from(value)),
                _ => return die("publish", "--data needs a value", 2),
            },
            "--sha" => match take_value(args, &mut i, arg) {
                Ok(value) => sha = if value.is_empty() { None } else { Some(value) },
                Err(_) => sha = None,
            },
            _ => return die("publish", &format!("unexpected argument {arg}"), 2),
        }
        i += 1;
    }

    let (Some(record_path), Some(data_dir)) = (record_path, data_dir) else {
        return die("publish", "need --record and --data", 2);
    };

    match publish(&record_path, &data_dir, sha.as_deref()) {
        Ok(summary) => {
            let short = &summary.sha[..summary.sha.len().min(8)];
            eprintln!(
                "publish: {short} at {} ({} regressions, {} fixes, {} new timeouts, {} newly measured)",
                summary.date,
                summary.diff.regressions.len(),
                summary.diff.fixes.len(),
                summary.diff.new_timeouts.len(),
                summary.diff.newly_measured.len()
            );
            ExitCode::SUCCESS
        }
        Err(error) => die("publish", &error, 2),
    }
}

fn cmd_summary(args: &[String]) -> ExitCode {
    let mut data_dir: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        let name = arg.split('=').next().unwrap_or(arg);
        match name {
            "--data" => match take_value(args, &mut i, arg) {
                Ok(value) if !value.is_empty() => data_dir = Some(PathBuf::from(value)),
                _ => return die("summary", "--data needs a value", 2),
            },
            _ => return die("summary", &format!("unexpected argument {arg}"), 2),
        }
        i += 1;
    }

    let Some(data_dir) = data_dir else {
        return die("summary", "need --data", 2);
    };
    let path = data_dir.join("regressions.json");
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) => {
            return die(
                "summary",
                &format!("cannot read {}: {error}", path.display()),
                2,
            );
        }
    };
    let diff: Regression = match serde_json::from_str(&text) {
        Ok(diff) => diff,
        Err(error) => {
            return die(
                "summary",
                &format!("{} is not valid JSON ({error})", path.display()),
                2,
            );
        }
    };
    print!("{}", format_regression_summary(&diff));
    ExitCode::SUCCESS
}

fn cmd_emit_ts(args: &[String]) -> ExitCode {
    let mut out: Option<PathBuf> = None;
    let mut check: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        let name = arg.split('=').next().unwrap_or(arg);
        match name {
            "--out" => match take_value(args, &mut i, arg) {
                Ok(value) if !value.is_empty() => out = Some(PathBuf::from(value)),
                _ => return die("emit-ts", "--out needs a value", 2),
            },
            "--check" => match take_value(args, &mut i, arg) {
                Ok(value) if !value.is_empty() => check = Some(PathBuf::from(value)),
                _ => return die("emit-ts", "--check needs a value", 2),
            },
            _ => return die("emit-ts", &format!("unexpected argument {arg}"), 2),
        }
        i += 1;
    }

    let generated = emit_typescript();
    if let Some(path) = check {
        return check_ts_file(&path, &generated);
    }
    if let Some(path) = out {
        if let Err(error) = fs::write(&path, generated) {
            return die("emit-ts", &error.to_string(), 1);
        }
        return ExitCode::SUCCESS;
    }
    print!("{generated}");
    ExitCode::SUCCESS
}

fn check_ts_file(path: &Path, expected: &str) -> ExitCode {
    match fs::read_to_string(path) {
        Ok(actual) if actual == expected => ExitCode::SUCCESS,
        Ok(_) => die(
            "emit-ts",
            &format!(
                "{} is out of date; regenerate with: cargo run -p wpt-report -- emit-ts --out {}",
                path.display(),
                path.display()
            ),
            1,
        ),
        Err(error) => die(
            "emit-ts",
            &format!("cannot read {}: {error}", path.display()),
            1,
        ),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let Some((command, rest)) = args.split_first() else {
        eprintln!("{}", usage());
        return ExitCode::from(2);
    };
    match command.as_str() {
        "summarize" => cmd_summarize(rest),
        "merge" => cmd_merge(rest),
        "publish" => cmd_publish(rest),
        "summary" => cmd_summary(rest),
        "emit-ts" => cmd_emit_ts(rest),
        "-h" | "--help" => {
            println!("{}", usage());
            ExitCode::SUCCESS
        }
        other => die(
            "cli",
            &format!("unknown subcommand {other}\n{}", usage()),
            2,
        ),
    }
}
