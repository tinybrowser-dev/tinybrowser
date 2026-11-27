mod engine;
mod journal;
mod services;
mod sidecar;
mod suite;
mod template;
mod watchdog;
mod wrapper;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use engine::Outcome;
use suite::{Kind, TestCase};

struct Options {
    root: PathBuf,
    paths: Vec<String>,
    jobs: usize,
    report: Option<PathBuf>,
    baseline: Option<PathBuf>,
    verbose: bool,
    list: bool,
    limit: usize,
    offset: usize,
    shard: Option<(usize, usize)>,
    exclude_testdriver: bool,
    clock: Clock,
    budgets: engine::Budgets,
    content_types: HashMap<String, String>,
    sidecar: Option<sidecar::Sidecar>,
    journal: Option<PathBuf>,
    stale: journal::Stale,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Clock {
    Paused,
    Real,
}

impl Options {
    fn defaults() -> Self {
        Self {
            root: suite::default_root(),
            paths: Vec::new(),
            jobs: std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get),
            report: None,
            baseline: None,
            verbose: false,
            list: false,
            limit: usize::MAX,
            offset: 0,
            shard: None,
            exclude_testdriver: false,
            clock: Clock::Paused,
            budgets: engine::Budgets {
                normal: Duration::ZERO,
                long: Duration::ZERO,
            },
            content_types: HashMap::new(),
            sidecar: None,
            journal: None,
            stale: journal::Stale::Died,
        }
    }

    fn validate(mut self) -> Result<Self, String> {
        if self.paths.is_empty() {
            return Err("pass at least one test path".into());
        }
        if self.budgets.normal.is_zero() || self.budgets.long.is_zero() {
            return Err("pass --normal-budget and --long-budget in seconds \
                 (cli.py computes them from tools/wpt/timeouts.py)"
                .into());
        }
        if self.content_types.is_empty() {
            return Err("pass --content-types with wptserve's extension map \
                 (cli.py reads it from wptserve.constants)"
                .into());
        }
        if let Some((index, count)) = self.shard {
            if count < 1 {
                return Err("pass --total-chunks with a count of at least 1".into());
            }
            if index < 1 || index > count {
                return Err(format!(
                    "--this-chunk must be in 1..={count} with --total-chunks {count}, got {index}"
                ));
            }
            self.shard = Some((index - 1, count));
        }
        Ok(self)
    }
}

fn main() {
    let options = match parse_args() {
        Ok(options) => options,
        Err(message) => {
            eprintln!("wpt-harness: {message}");
            eprintln!("{HELP}");
            std::process::exit(2);
        }
    };
    if !options.root.is_dir() {
        eprintln!(
            "wpt-harness: {} is not a WPT checkout; pass --wpt-root",
            options.root.display()
        );
        std::process::exit(2);
    }
    let cases = match suite::select(
        &options.root,
        &options.paths,
        &options.content_types,
        &suite::Selection {
            offset: options.offset,
            limit: options.limit,
            shard: options.shard,
            exclude_testdriver: options.exclude_testdriver,
        },
    ) {
        Ok(cases) => cases,
        Err(error) => {
            eprintln!("wpt-harness: cannot read suite: {error}");
            std::process::exit(2);
        }
    };
    if cases.is_empty() {
        eprintln!("wpt-harness: no tests selected");
        std::process::exit(2);
    }
    if options.list {
        for case in &cases {
            println!("{:?}\t{}", case.kind, case.relative);
        }
        return;
    }
    let started = Instant::now();
    let (journal, resumed) = match options.journal.as_deref() {
        Some(path) => match journal::Journal::open(path) {
            Ok((journal, resumed)) => (Some(journal), resumed),
            Err(error) => {
                eprintln!("wpt-harness: cannot open {}: {error}", path.display());
                std::process::exit(2);
            }
        },
        None => (None, journal::Resumed::default()),
    };
    let (attempt, outcomes) = run_selected(&options, &cases, journal.as_ref(), &resumed);
    let wall = started.elapsed();
    wpt_report::summarize(&attempt, wall, options.verbose);
    if let Some(path) = &options.report
        && let Err(error) = wpt_report::write_report(path, &outcomes)
    {
        eprintln!("wpt-harness: cannot write {}: {error}", path.display());
        std::process::exit(2);
    }
    if let Some(baseline) = &options.baseline {
        match wpt_report::compare(baseline, &outcomes) {
            Ok(comparison) => {
                wpt_report::print_comparison(&comparison);
                if comparison.failed() {
                    exit(1);
                }
            }
            Err(error) => {
                eprintln!(
                    "wpt-harness: cannot compare {}: {error}",
                    baseline.display()
                );
                std::process::exit(2);
            }
        }
    }
    exit(0);
}

fn exit(code: i32) -> ! {
    use std::io::Write as _;
    let _ = std::io::stdout().flush();
    std::process::exit(code)
}

fn run_selected(
    options: &Options,
    cases: &[TestCase],
    journal: Option<&journal::Journal>,
    resumed: &journal::Resumed,
) -> (Vec<wpt_report::Outcome>, Vec<wpt_report::Outcome>) {
    let mut finished = resumed.done.clone();
    if let Some(journal) = journal {
        for case in cases {
            if !resumed.stale.contains(&case.id()) {
                continue;
            }
            let outcome = interrupted(case, options.stale);
            journal.finish(&outcome);
            finished.insert(case.id(), outcome);
        }
    }
    let attempt = run_all(options, cases, journal, &finished);
    let mut outcomes = attempt.clone();
    if journal.is_some() {
        let selected: std::collections::HashSet<String> = cases.iter().map(TestCase::id).collect();
        for (id, outcome) in &finished {
            if !selected.contains(id) {
                outcomes.push(outcome.clone());
            }
        }
        outcomes.sort_by(|left, right| left.id.cmp(&right.id));
    }
    (attempt, outcomes)
}

fn interrupted(case: &TestCase, stale: journal::Stale) -> wpt_report::Outcome {
    let status = match stale {
        journal::Stale::Died => engine::Status::Error,
        journal::Stale::TimedOut => engine::Status::Timeout,
    };
    wpt_report::Outcome {
        id: case.id(),
        status,
        message: Some(journal::stale_message(stale).into()),
        duration: Duration::ZERO,
        subtests: Vec::new(),
        testharness: matches!(case.kind, Kind::Testharness),
    }
}

fn run_all(
    options: &Options,
    cases: &[TestCase],
    journal: Option<&journal::Journal>,
    finished: &HashMap<String, wpt_report::Outcome>,
) -> Vec<wpt_report::Outcome> {
    let mut slots: Vec<Option<wpt_report::Outcome>> = cases
        .iter()
        .map(|case| finished.get(&case.id()).cloned())
        .collect();
    let pending: Vec<usize> = (0..cases.len())
        .filter(|&index| slots[index].is_none())
        .collect();
    if !pending.is_empty() {
        run_pending(options, cases, &pending, journal, &mut slots);
    }
    slots
        .into_iter()
        .zip(cases)
        .map(|(slot, case)| {
            slot.unwrap_or_else(|| wpt_report::Outcome {
                id: case.id(),
                status: engine::Status::Skip,
                message: Some("worker stopped before this test".into()),
                duration: Duration::ZERO,
                subtests: Vec::new(),
                testharness: matches!(case.kind, Kind::Testharness),
            })
        })
        .collect()
}

fn run_pending(
    options: &Options,
    cases: &[TestCase],
    pending: &[usize],
    journal: Option<&journal::Journal>,
    slots: &mut [Option<wpt_report::Outcome>],
) {
    let next = AtomicUsize::new(0);
    let (sender, receiver) = mpsc::channel();
    let workers = options.jobs.clamp(1, pending.len());
    let watchdog = watchdog::Watchdog::new(workers);
    let done = AtomicBool::new(false);
    let done = &done;
    let ahem: Arc<Vec<u8>> = match std::fs::read(options.root.join("fonts/Ahem.ttf")) {
        Ok(data) => Arc::new(data),
        Err(error) => {
            eprintln!("wpt-harness: cannot read fonts/Ahem.ttf: {error}");
            std::process::exit(2);
        }
    };
    if renderer::Fonts::default().register_font(ahem.clone()) == 0 {
        eprintln!("wpt-harness: fonts/Ahem.ttf contains no font family");
        std::process::exit(2);
    }
    let ahem = &ahem;
    std::thread::scope(|scope| {
        let monitor = &watchdog;
        scope.spawn(move || {
            while !done.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(200));
                let Some(index) = monitor.expired() else {
                    continue;
                };
                let case = &cases[index];
                let outcome = interrupted(case, journal::Stale::TimedOut);
                if let Some(journal) = journal {
                    journal.finish(&outcome);
                }
                eprintln!(
                    "wpt-harness: {} stopped returning; exiting for the driver to resume",
                    outcome.id
                );
                std::process::exit(3);
            }
        });
        for worker in 0..workers {
            let sender = sender.clone();
            let next = &next;
            let watchdog = &watchdog;
            scope.spawn(move || {
                let mut fonts = renderer::Fonts::default();
                fonts.register_font(ahem.clone());
                let virtual_time = options.clock == Clock::Paused;
                while let Some(&index) = pending.get(next.fetch_add(1, Ordering::Relaxed)) {
                    let case = &cases[index];
                    if let Some(journal) = journal {
                        journal.start(&case.id());
                    }
                    watchdog.enter(worker, index, options.budgets.for_case(case));
                    let started = Instant::now();
                    let mut session = engine::Session::new(
                        &options.root,
                        options.content_types.clone(),
                        options.budgets,
                        &fonts,
                        options.sidecar,
                        virtual_time,
                    );
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        session.run(case, virtual_time)
                    }));
                    let outcome = result.unwrap_or_else(|_| {
                        Box::leak(Box::new(session));
                        Outcome {
                            id: case.id(),
                            kind: case.kind,
                            status: engine::Status::Error,
                            message: Some("engine panicked".into()),
                            subtests: Vec::new(),
                            duration: started.elapsed(),
                        }
                    });
                    watchdog.leave(worker);
                    let outcome = reported(&outcome);
                    if let Some(journal) = journal {
                        journal.finish(&outcome);
                    }
                    if sender.send((index, outcome)).is_err() {
                        break;
                    }
                }
            });
        }
        drop(sender);
        for (index, outcome) in receiver {
            slots[index] = Some(outcome);
        }
        done.store(true, Ordering::Relaxed);
    });
}

fn reported(outcome: &Outcome) -> wpt_report::Outcome {
    wpt_report::Outcome {
        id: outcome.id.clone(),
        status: outcome.status,
        message: outcome.message.clone(),
        duration: outcome.duration,
        subtests: outcome
            .subtests
            .iter()
            .map(|subtest| wpt_report::Subtest {
                name: subtest.name.clone(),
                status: subtest.status,
                message: subtest.message.clone(),
            })
            .collect(),
        testharness: matches!(outcome.kind, Kind::Testharness),
    }
}

const HELP: &str = "\
usage: wpt-harness [OPTIONS] PATH...

Runs WPT testharness and crashtest files in-process.

  --wpt-root PATH   WPT checkout (default third_party/wpt)
  --jobs N          worker threads (default: available CPUs)
  --report PATH     write a wptreport JSON
  --baseline PATH   compare with an earlier --report and exit 1 on regressions
  --limit N         stop after N selected files
  --offset N        skip the first N selected files (shards)
  --total-chunks N  split the selection into N directory-hashed chunks
  --this-chunk K    run chunk K of N (1-based, with --total-chunks)
  --exclude-testdriver  leave testdriver files to the WebDriver lane
  --normal-budget S wall-clock seconds before a normal file is TIMEOUT
  --long-budget S  wall-clock seconds before a long-timeout file is TIMEOUT
  --content-types JSON  wptserve's extension-to-content-type map
  --sidecar HOST:PORT  wptserve process for `.py` handler routes
  --journal PATH    log per-file progress; a restart resumes after it
  --stale MODE      how a file the previous attempt left running is recorded:
                    `error` (the process died) or `timeout` (it stopped
                    returning and the attempt was cut)
  --real-time       use the wall clock instead of the paused clock
  --verbose         print failing tests and the ten slowest files
  --list            print the selected files and exit
  -h, --help        show this help

PATH is relative to the WPT root, such as dom/nodes or dom/events/foo.html.
Tests come from MANIFEST.json, which `cli.py manifest` builds and stamps;
reftests and generated worker wrappers are not selected.";

fn path_arg(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<PathBuf, String> {
    args.next()
        .map(PathBuf::from)
        .ok_or_else(|| format!("{flag} needs a path"))
}

fn parse_args() -> Result<Options, String> {
    let mut options = Options::defaults();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        parse_flag(&mut options, &arg, &mut args)?;
    }
    options.validate()
}

fn parse_flag(
    options: &mut Options,
    arg: &str,
    args: &mut impl Iterator<Item = String>,
) -> Result<(), String> {
    match arg {
        "-h" | "--help" => {
            print!("{HELP}");
            std::process::exit(0);
        }
        "--wpt-root" => options.root = path_arg(args, "--wpt-root")?,
        "--jobs" => {
            options.jobs = args
                .next()
                .ok_or("--jobs needs a number")?
                .parse()
                .map_err(|_| "--jobs needs a number")?;
        }
        "--report" => options.report = Some(path_arg(args, "--report")?),
        "--baseline" => options.baseline = Some(path_arg(args, "--baseline")?),
        "--limit" => {
            options.limit = args
                .next()
                .ok_or("--limit needs a number")?
                .parse()
                .map_err(|_| "--limit needs a number")?;
        }
        "--offset" => {
            options.offset = args
                .next()
                .ok_or("--offset needs a number")?
                .parse()
                .map_err(|_| "--offset needs a number")?;
        }
        "--total-chunks" => {
            options.shard = Some((
                options.shard.map_or(0, |(index, _)| index),
                args.next()
                    .ok_or("--total-chunks needs a number")?
                    .parse()
                    .map_err(|_| "--total-chunks needs a number")?,
            ));
        }
        "--this-chunk" => {
            options.shard = Some((
                args.next()
                    .ok_or("--this-chunk needs a number")?
                    .parse()
                    .map_err(|_| "--this-chunk needs a number")?,
                options.shard.map_or(0, |(_, count)| count),
            ));
        }
        "--exclude-testdriver" => options.exclude_testdriver = true,
        "--normal-budget" => {
            options.budgets.normal = seconds_arg(args, "--normal-budget")?;
        }
        "--long-budget" => options.budgets.long = seconds_arg(args, "--long-budget")?,
        "--content-types" => {
            let raw = args.next().ok_or("--content-types needs a JSON map")?;
            options.content_types = serde_json::from_str(&raw)
                .map_err(|error| format!("--content-types is not a JSON map: {error}"))?;
        }
        "--sidecar" => {
            let raw = args.next().ok_or("--sidecar needs an address")?;
            let address = raw
                .parse()
                .map_err(|_| format!("--sidecar needs HOST:PORT, got {raw}"))?;
            options.sidecar = Some(sidecar::Sidecar::new(address));
        }
        "--journal" => options.journal = Some(path_arg(args, "--journal")?),
        "--stale" => {
            options.stale = match args.next().as_deref() {
                Some("error") => journal::Stale::Died,
                Some("timeout") => journal::Stale::TimedOut,
                _ => return Err("--stale needs `error` or `timeout`".into()),
            };
        }
        "--real-time" => options.clock = Clock::Real,
        "--verbose" => options.verbose = true,
        "--list" => options.list = true,
        other if other.starts_with('-') => return Err(format!("unknown flag {other}")),
        other => options.paths.push(other.trim_start_matches('/').to_owned()),
    }
    Ok(())
}

fn seconds_arg(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<Duration, String> {
    let raw = args.next().ok_or_else(|| format!("{flag} needs seconds"))?;
    let seconds: f64 = raw
        .parse()
        .map_err(|_| format!("{flag} needs seconds, got {raw}"))?;
    Duration::try_from_secs_f64(seconds)
        .ok()
        .filter(|duration| !duration.is_zero())
        .ok_or_else(|| format!("{flag} needs a positive number of seconds, got {raw}"))
}
