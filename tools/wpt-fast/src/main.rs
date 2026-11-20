mod meta;
mod modules;
mod runner;
mod suite;

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Instant;

use rquickjs::Runtime;

use runner::{Harness, TestCase};
use wpt_report::{Outcome, Status};

struct Options {
    root: PathBuf,
    filters: Vec<String>,
    jobs: usize,
    report: Option<PathBuf>,
    baseline: Option<PathBuf>,
    verbose: bool,
    list: bool,
    limit: Option<usize>,
}

fn main() {
    let options = match parse_args() {
        Ok(options) => options,
        Err(message) => {
            eprintln!("wpt-fast: {message}");
            eprintln!("{HELP}");
            std::process::exit(2);
        }
    };
    if !options.root.is_dir() {
        eprintln!(
            "wpt-fast: {} is not a test262 checkout; pass --test262-root",
            options.root.display()
        );
        std::process::exit(2);
    }
    let cases = match suite::select(&options.root, &options.filters) {
        Ok(cases) => cases,
        Err(error) => {
            eprintln!("wpt-fast: cannot read suite: {error}");
            std::process::exit(2);
        }
    };
    let cases = match options.limit {
        Some(limit) => &cases[..cases.len().min(limit)],
        None => &cases[..],
    };
    if cases.is_empty() {
        eprintln!("wpt-fast: no tests selected");
        std::process::exit(2);
    }
    if options.list {
        for case in cases {
            println!(
                "{}{}",
                case.relative,
                if case.strict { " (strict)" } else { "" }
            );
        }
        return;
    }
    let harness = match Harness::load(&options.root) {
        Ok(harness) => harness,
        Err(error) => {
            eprintln!("wpt-fast: cannot read harness: {error}");
            std::process::exit(2);
        }
    };
    let started = Instant::now();
    let outcomes = run_all(cases, &harness, options.jobs);
    let wall = started.elapsed();
    wpt_report::summarize(&outcomes, wall, options.verbose);
    if let Some(path) = &options.report
        && let Err(error) = wpt_report::write_report(path, &outcomes)
    {
        eprintln!("wpt-fast: cannot write {}: {error}", path.display());
        std::process::exit(2);
    }
    if let Some(baseline) = &options.baseline {
        match wpt_report::compare(baseline, &outcomes) {
            Ok(comparison) => {
                wpt_report::print_comparison(&comparison);
                if comparison.failed() {
                    std::process::exit(1);
                }
            }
            Err(error) => {
                eprintln!("wpt-fast: cannot compare {}: {error}", baseline.display());
                std::process::exit(2);
            }
        }
    }
}

fn run_all(cases: &[TestCase], harness: &Harness, jobs: usize) -> Vec<Outcome> {
    let next = AtomicUsize::new(0);
    let (sender, receiver) = mpsc::channel();
    let workers = jobs.clamp(1, cases.len());
    std::thread::scope(|scope| {
        for _ in 0..workers {
            let sender = sender.clone();
            let next = &next;
            scope.spawn(move || {
                let runtime = Runtime::new().unwrap_or_else(|error| {
                    eprintln!("wpt-fast: cannot create QuickJS runtime: {error}");
                    std::process::exit(2);
                });
                runtime.set_loader(
                    modules::SuiteResolver,
                    modules::SuiteLoader {
                        root: harness.root.clone(),
                    },
                );
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(case) = cases.get(index) else {
                        break;
                    };
                    let outcome = runner::run(&runtime, harness, case);
                    if sender.send((index, outcome)).is_err() {
                        break;
                    }
                }
            });
        }
        drop(sender);
        let mut slots: Vec<Option<Outcome>> = (0..cases.len()).map(|_| None).collect();
        for (index, outcome) in receiver {
            slots[index] = Some(outcome);
        }
        slots
            .into_iter()
            .enumerate()
            .map(|(index, outcome)| {
                outcome.unwrap_or_else(|| {
                    let message = Some("worker stopped before this test".into());
                    Outcome {
                        id: cases[index].wpt_id(),
                        status: Status::Skip,
                        subtests: vec![wpt_report::Subtest {
                            name: "Test".to_owned(),
                            status: wpt_report::SubtestStatus::Skip,
                            message: message.clone(),
                        }],
                        message,
                        duration: std::time::Duration::ZERO,
                        testharness: true,
                    }
                })
            })
            .collect()
    })
}

const HELP: &str = "\
usage: wpt-fast [OPTIONS] [FILTER...]

Runs test262 files in-process and emits a wptreport.

  --test262-root PATH  checkout root (default third_party/wpt/third_party/test262)
  --jobs N             worker threads (default: available CPUs)
  --report PATH        write a wptreport JSON
  --baseline PATH      compare with an earlier --report and exit 1 on regressions
  --verbose            print failing tests and the ten slowest files
  --list               print the selected files and exit
  --limit N            stop after N selected files
  -h, --help           show this help

FILTER matches a path under the checkout, such as test/language or harness.";

fn parse_args() -> Result<Options, String> {
    let mut options = Options {
        root: suite::default_root(),
        filters: Vec::new(),
        jobs: std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get),
        report: None,
        baseline: None,
        verbose: false,
        list: false,
        limit: None,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{HELP}");
                std::process::exit(0);
            }
            "--test262-root" => {
                options.root = args
                    .next()
                    .map(PathBuf::from)
                    .ok_or("--test262-root needs a path")?;
            }
            "--jobs" => {
                options.jobs = args
                    .next()
                    .ok_or("--jobs needs a number")?
                    .parse()
                    .map_err(|_| "--jobs needs a number")?;
            }
            "--report" => {
                options.report = Some(
                    args.next()
                        .map(PathBuf::from)
                        .ok_or("--report needs a path")?,
                );
            }
            "--baseline" => {
                options.baseline = Some(
                    args.next()
                        .map(PathBuf::from)
                        .ok_or("--baseline needs a path")?,
                );
            }
            "--verbose" => options.verbose = true,
            "--list" => options.list = true,
            "--limit" => {
                options.limit = Some(
                    args.next()
                        .ok_or("--limit needs a number")?
                        .parse()
                        .map_err(|_| "--limit needs a number")?,
                );
            }
            other if other.starts_with('-') => return Err(format!("unknown flag {other}")),
            other => options
                .filters
                .push(other.trim_start_matches('/').to_owned()),
        }
    }
    Ok(options)
}
