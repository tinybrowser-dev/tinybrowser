//! One WPT file, mounted in this process and driven on a virtual clock.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use renderer::{EmbeddedRenderer, Mount, RendererEvent, ResponseHead, TabError};
use serde_json::Value;
use url::Url;

use crate::services::FsServices;
pub use wpt_report::Subtest;
use wpt_report::SubtestStatus;

use crate::suite::{Fuzzy, Kind, Relation, TestCase};

/// Wall-clock time a test may spend before it is called hung. The driver owns
/// the numbers: `cli.py` computes them from `tools/wpt/timeouts.py`, the same
/// policy the `WebDriver` lane uses, and passes them as `--normal-budget` and
/// `--long-budget`. Keeping a copy here would let the two lanes drift (#237).
///
/// One drain gets the whole budget. A shorter slice would interrupt a script
/// that is still making progress: `testharness.js` catches the engine's
/// `interrupted` error, reports the tests that already ran, and marks the rest
/// `NOTRUN` with a harness error — a truncated file that looks like a verdict.
#[derive(Clone, Copy, Debug)]
pub struct Budgets {
    /// Budget for a file without `<meta name=timeout content=long>`.
    pub normal: Duration,
    /// Budget for a file that asks for the long timeout.
    pub long: Duration,
}

impl Budgets {
    /// The wall-clock budget for one file.
    pub(crate) fn for_case(self, case: &TestCase) -> Duration {
        if case.long_timeout {
            self.long
        } else {
            self.normal
        }
    }
}

/// Reads the reporter's JSON out of the page.
const POLL: &str =
    "String(globalThis.__tinybrowser_result === undefined ? '' : globalThis.__tinybrowser_result)";

/// The verdict for one file, shared with the other fast lane so the report and
/// the baseline rule cannot drift (#234).
pub use wpt_report::Status;

/// The result of one file.
#[derive(Clone, Debug)]
pub struct Outcome {
    /// wptreport test identifier, such as `/dom/nodes/Node-nodeName.html`.
    pub id: String,
    /// Which executor produced the result.
    pub kind: Kind,
    /// File-level verdict used by the summary and the baseline diff.
    pub status: Status,
    /// Failure, skip, or timeout reason.
    pub message: Option<String>,
    /// testharness subtests, or one synthetic subtest for crashtests.
    pub subtests: Vec<Subtest>,
    /// Wall time spent in this file.
    pub duration: Duration,
}

/// One file's engine and the in-memory host that serves the checkout.
///
/// A file gets its own engine instead of a reused one: a page can leave a
/// `QuickJS` class cell borrowed, and the next mount then panics inside
/// rquickjs. A file's own engine cannot see that state. The price is one
/// runtime, document, and layout tree per file.
pub struct Session {
    root: std::path::PathBuf,
    services: Arc<FsServices>,
    renderer: EmbeddedRenderer,
    budgets: Budgets,
    /// Extension table used to type each reference document.
    content_types: HashMap<String, String>,
}

impl Session {
    /// Creates an engine for one worker, sharing `fonts` with the worker's
    /// other engines.
    #[must_use]
    pub fn new(
        root: &std::path::Path,
        content_types: HashMap<String, String>,
        budgets: Budgets,
        fonts: &renderer::Fonts,
        sidecar: Option<crate::sidecar::Sidecar>,
    ) -> Self {
        let services = Arc::new(FsServices::new(
            root.to_path_buf(),
            content_types.clone(),
            sidecar,
        ));
        let renderer = EmbeddedRenderer::with_fonts(
            Arc::clone(&services) as Arc<dyn renderer::BrowserServices>,
            net::ValidIdentity::default(),
            fonts,
        );
        Self {
            root: root.to_path_buf(),
            services,
            renderer,
            budgets,
            content_types,
        }
    }

    /// Runs one file.
    ///
    /// `virtual_time` is the runtime's clock mode; the caller owns the runtime
    /// and builds it paused or real.
    pub async fn run(&mut self, case: &TestCase, virtual_time: bool) -> Outcome {
        let started = Instant::now();
        let outcome = match case.kind {
            Kind::Testharness => run_testharness(self, case, virtual_time).await,
            Kind::Crashtest => run_crashtest(self, case, virtual_time).await,
            Kind::Reftest => run_reftest(self, case, virtual_time).await,
            Kind::Testdriver => Err(Failure::Skip(
                "testdriver requires a WebDriver session".into(),
            )),
        };
        let (status, message, subtests) = match outcome {
            Ok(result) => (result.status, result.message, result.subtests),
            Err(failure) => {
                let (status, message) = failure.into_parts();
                let subtests = vec![Subtest {
                    name: "Test".into(),
                    status: match status {
                        Status::Pass => SubtestStatus::Pass,
                        Status::Fail => SubtestStatus::Fail,
                        Status::Timeout => SubtestStatus::Timeout,
                        Status::Error => SubtestStatus::NotRun,
                        Status::Skip => SubtestStatus::Skip,
                    },
                    message: message.clone(),
                }];
                (status, message, subtests)
            }
        };
        Outcome {
            id: case.id(),
            kind: case.kind,
            status,
            message,
            subtests,
            duration: started.elapsed(),
        }
    }

    /// Mounts one file and settles its first turn, bounded by `deadline`.
    fn mount(&mut self, case: &TestCase, deadline: Instant) -> Result<(), Failure> {
        self.mount_source(
            &case.relative,
            &case.source,
            case.bytes.clone(),
            case.content_type.clone(),
            deadline,
        )
    }

    /// Mounts a URL from bytes, for the test file or one of its references.
    fn mount_source(
        &mut self,
        relative: &str,
        source: &str,
        body: Vec<u8>,
        fallback_content_type: String,
        deadline: Instant,
    ) -> Result<(), Failure> {
        // Events belong to the file that produced them. A crashtest is judged
        // by whether it survives, and a stale `ScriptFailed` from the previous
        // file must not be read as this one's.
        let _stale = self.renderer.take_events();
        self.services.clear();
        let url = Url::parse(&format!(
            "{}{relative}",
            crate::services::base_for(relative)
        ))
        .map_err(|error| Failure::Error(format!("bad url: {error}")))?;
        let path = self.root.join(source);
        let headers = crate::services::FsServices::headers_for(
            &path,
            &crate::services::RequestContext {
                url: &url,
                headers: &[],
            },
        );
        // The mount bypasses the transport, so the jar misses the response's
        // `Set-Cookie` fields; a navigation would store them
        // (<https://fetch.spec.whatwg.org/#http-network-or-cache-fetch>).
        self.services.store_set_cookies(&headers.list, &url);
        let content_type = headers.content_type.unwrap_or(fallback_content_type);
        // The main document is mounted from the bytes on disk, so the
        // transport's `.sub.` pipe never sees it. Apply the same expansion a
        // subresource gets; a page whose own body kept `{{host}}` would fail
        // every test that reads its own source.
        let body = if url.path().contains(".sub.") {
            let text = String::from_utf8_lossy(&body);
            let context = crate::template::Context {
                url: &url,
                headers: &[],
            };
            crate::template::substitute_escaped(
                &text,
                &context,
                crate::template::Escape::for_path(url.path()),
            )
            .map_err(|error| Failure::Error(format!("{relative}: {error}")))?
            .into_bytes()
        } else {
            body
        };
        let head = ResponseHead::http(200, url, Some(content_type), &headers.list);
        self.renderer
            .mount_bounded(
                &Mount {
                    head,
                    // No browser behind this carrier: no name, no opener, and
                    // no popup sandbox. `window.open` returns null too, so a
                    // file cannot open a context whose flags it would need.
                    tab: renderer::TabState::default(),
                    body,
                },
                deadline.saturating_duration_since(Instant::now()),
            )
            .map_err(Failure::Mount)
    }
}

/// A file that completed.
struct Completed {
    status: Status,
    message: Option<String>,
    subtests: Vec<Subtest>,
}

/// Why a file did not complete.
enum Failure {
    Mount(TabError),
    Timeout,
    Error(String),
    Skip(String),
}

impl Failure {
    fn into_parts(self) -> (Status, Option<String>) {
        match self {
            Self::Mount(error) => (Status::Error, Some(error.to_string())),
            Self::Timeout => (Status::Timeout, Some("harness budget elapsed".into())),
            Self::Error(message) => (Status::Error, Some(message)),
            Self::Skip(message) => (Status::Skip, Some(message)),
        }
    }
}

async fn run_testharness(
    session: &mut Session,
    case: &TestCase,
    virtual_time: bool,
) -> Result<Completed, Failure> {
    let started = Instant::now();
    let limit = session.budgets.for_case(case);
    let deadline = started + limit;
    session.mount(case, deadline)?;
    let renderer = &mut session.renderer;
    loop {
        let _yielded = renderer.drain_until(deadline);
        let result = renderer.eval(POLL).unwrap_or_default();
        if !result.is_empty() {
            return parse_result(&result);
        }
        if started.elapsed() > limit {
            return Err(timeout_failure(renderer));
        }
        // A dial completion arrives from the fetch thread through a std
        // channel. Wait for it before advancing the virtual clock: otherwise a
        // testharness timeout fires while its own response is still in flight.
        // The sleep is real time, not tokio time, so a paused clock cannot jump
        // past the work the page is waiting on.
        if renderer.has_pending_dials() {
            std::thread::sleep(Duration::from_micros(200));
            continue;
        }
        let Some(delay) = renderer.time_until_deadline() else {
            // Nothing is ready and nothing is scheduled. The in-memory host
            // has no pending I/O, so this page cannot make progress.
            return Err(timeout_failure(renderer));
        };
        if !delay.is_zero() {
            if virtual_time {
                session.services.advance_clock(delay);
                tokio::time::advance(delay).await;
            } else {
                tokio::time::sleep(delay.min(Duration::from_millis(50))).await;
            }
        }
    }
}

/// A timeout that names a script failure when the page reported one. A
/// testharness file whose script threw before `setup` leaves no timer and no
/// result, so the event is the only evidence of what happened.
fn timeout_failure(renderer: &mut EmbeddedRenderer) -> Failure {
    let script_failed = renderer
        .take_events()
        .unwrap_or_default()
        .iter()
        .any(|(_, event)| matches!(event, RendererEvent::ScriptFailed));
    if script_failed {
        Failure::Error("script failed".into())
    } else {
        Failure::Timeout
    }
}

async fn run_crashtest(
    session: &mut Session,
    case: &TestCase,
    virtual_time: bool,
) -> Result<Completed, Failure> {
    let started = Instant::now();
    let limit = session.budgets.for_case(case);
    let deadline = started + limit;
    session.mount(case, deadline)?;
    let renderer = &mut session.renderer;
    loop {
        let _yielded = renderer.drain_until(deadline);
        if started.elapsed() > limit {
            return Err(Failure::Timeout);
        }
        // A crashtest can also start a fetch. Wait for the in-process dial
        // before judging the file complete, or the continuation that contains
        // the crash never runs and a hung page is reported as PASS.
        if renderer.has_pending_dials() {
            std::thread::sleep(Duration::from_micros(200));
            continue;
        }
        match renderer.time_until_deadline() {
            Some(delay) if !delay.is_zero() => {
                if virtual_time {
                    session.services.advance_clock(delay);
                    tokio::time::advance(delay).await;
                } else {
                    tokio::time::sleep(delay.min(Duration::from_millis(50))).await;
                }
            }
            Some(_) => {}
            None => {
                // A crashtest's contract is that nothing kills the process.
                // Its script is allowed to throw; upstream wptrunner treats a
                // completed load as PASS regardless of script errors.
                return Ok(Completed {
                    status: Status::Pass,
                    message: None,
                    subtests: vec![Subtest {
                        name: "Crashtest".into(),
                        status: SubtestStatus::Pass,
                        message: None,
                    }],
                });
            }
        }
    }
}

/// Runs one reftest: render the test, then every reference, and compare.
async fn run_reftest(
    session: &mut Session,
    case: &TestCase,
    virtual_time: bool,
) -> Result<Completed, Failure> {
    let started = Instant::now();
    let limit = session.budgets.for_case(case);
    let deadline = started + limit;
    session.mount(case, deadline)?;
    settle_reftest(session, deadline, virtual_time, true).await?;
    let request = screenshot_request();
    let test = session
        .renderer
        .render(&request)
        .map_err(|error| render_failure(&error))?;
    let mut differing = Vec::new();
    for reference in &case.refs {
        let path = session.root.join(&reference.source);
        let bytes = std::fs::read(&path).map_err(|error| {
            Failure::Error(format!("cannot read {}: {error}", reference.source))
        })?;
        let content_type = crate::suite::content_type_for(
            std::path::Path::new(&reference.source),
            &session.content_types,
        );
        session.mount_source(
            &reference.relative,
            &reference.source,
            bytes,
            content_type,
            deadline,
        )?;
        settle_reftest(session, deadline, virtual_time, false).await?;
        let image = session
            .renderer
            .render(&request)
            .map_err(|error| render_failure(&error))?;
        let diff = compare_reftest(&test, &image, case.fuzzy.as_ref(), reference.relation);
        if !diff.matches {
            differing.push(format!(
                "{} (max channel difference {}, {} pixels)",
                reference.relative, diff.max_difference, diff.differing
            ));
        }
    }
    let subtest = |status: SubtestStatus, message: Option<String>| Subtest {
        name: "Reftest".into(),
        status,
        message,
    };
    if differing.is_empty() {
        Ok(Completed {
            status: Status::Pass,
            message: None,
            subtests: vec![subtest(SubtestStatus::Pass, None)],
        })
    } else {
        let message = format!("differs from {}", differing.join(", "));
        Ok(Completed {
            status: Status::Fail,
            message: Some(message.clone()),
            subtests: vec![subtest(SubtestStatus::Fail, Some(message))],
        })
    }
}

/// Waits until a reftest removes its `reftest-wait` class.
///
/// A document that leaves the class on and has no runnable task left cannot
/// make progress; that is a timeout here just as it is in `wptrunner`
/// (<https://web-platform-tests.org/writing-tests/reftests.html#reftest-wait>).
async fn settle_reftest(
    session: &mut Session,
    deadline: Instant,
    virtual_time: bool,
    fire_test_rendered: bool,
) -> Result<(), Failure> {
    let mut rendered = false;
    loop {
        let _yielded = session.renderer.drain_until(deadline);
        // An image, stylesheet, or font is still in flight. Its completion is
        // a dial, and a render taken now would miss it.
        if session.renderer.has_pending_dials() {
            if Instant::now() >= deadline {
                return Err(Failure::Timeout);
            }
            std::thread::sleep(Duration::from_micros(200));
            continue;
        }
        // `wptrunner` fires `TestRendered` after load and before it waits for
        // `reftest-wait`; tests such as `dir-auto-dynamic` do their final
        // mutation in that listener.
        if fire_test_rendered && !rendered {
            let _ = session
                .renderer
                .eval("document.dispatchEvent(new Event('TestRendered'))");
            rendered = true;
            continue;
        }
        let waiting = session
            .renderer
            .eval("String(document.documentElement.classList.contains('reftest-wait'))")
            .unwrap_or_default();
        if waiting != "true" {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(Failure::Timeout);
        }
        let Some(delay) = session.renderer.time_until_deadline() else {
            return Err(Failure::Timeout);
        };
        if !delay.is_zero() {
            if virtual_time {
                session.services.advance_clock(delay);
                tokio::time::advance(delay).await;
            } else {
                tokio::time::sleep(delay.min(Duration::from_millis(50))).await;
            }
        }
    }
}

/// The shared reftest viewport.
fn screenshot_request() -> renderer::ScreenshotRequest {
    let (width, height) = renderer::DEFAULT_VIEWPORT;
    renderer::ScreenshotRequest {
        viewport_width: width,
        viewport_height: height,
        clip: None,
    }
}

fn render_failure(error: &TabError) -> Failure {
    Failure::Error(format!("render: {error}"))
}

/// The pixel difference between two renders.
struct Diff {
    max_difference: u8,
    differing: u64,
    matches: bool,
}

/// Compares two renders under the test's relation and fuzzy tolerance.
fn compare_reftest(
    test: &renderer::RgbaImage,
    reference: &renderer::RgbaImage,
    fuzzy: Option<&Fuzzy>,
    relation: Relation,
) -> Diff {
    let (max_difference, differing) =
        if test.width != reference.width || test.height != reference.height {
            (u8::MAX, u64::from(test.width) * u64::from(test.height))
        } else {
            let mut max_difference = 0u8;
            let mut differing = 0u64;
            for (left, right) in test
                .data
                .as_chunks::<4>()
                .0
                .iter()
                .zip(reference.data.as_chunks::<4>().0.iter())
            {
                let mut pixel = 0u8;
                for channel in 0..4 {
                    pixel = pixel.max(left[channel].abs_diff(right[channel]));
                }
                if pixel > 0 {
                    differing += 1;
                    max_difference = max_difference.max(pixel);
                }
            }
            (max_difference, differing)
        };
    let matches = match fuzzy {
        Some(fuzzy) => max_difference <= fuzzy.max_difference && differing <= fuzzy.total_pixels,
        None => differing == 0,
    };
    Diff {
        max_difference,
        differing,
        matches: match relation {
            Relation::Match => matches,
            Relation::Mismatch => !matches,
        },
    }
}

/// Turns the reporter payload into a verdict.
fn parse_result(raw: &str) -> Result<Completed, Failure> {
    let value: Value = serde_json::from_str(raw)
        .map_err(|error| Failure::Error(format!("bad harness result: {error}")))?;
    // testharness.js always reports a status; a payload without one is a
    // broken reporter, not a skip, and guessing would hide it.
    let harness_status = value
        .get("status")
        .and_then(Value::as_u64)
        .ok_or_else(|| Failure::Error("harness result has no status".into()))?;
    let subtests: Vec<Subtest> = value
        .get("tests")
        .and_then(Value::as_array)
        .map(|tests| tests.iter().map(parse_subtest).collect::<Result<_, _>>())
        .transpose()?
        .unwrap_or_default();
    // The status is the *harness* verdict, not the test's: `OK` with a
    // failing subtest is still an `OK` file in a wptreport. `ERROR` is a
    // harness-level failure (a script threw during setup, so no subtest ever
    // ran), and wptrunner writes it as `ERROR` rather than as a failing
    // subtest. The codes are the ones `TestharnessResultConverter` maps
    // (`tools/wptrunner/wptrunner/executors/base.py`).
    let status = match harness_status {
        0 if subtests
            .iter()
            .all(|subtest| subtest.status == SubtestStatus::Pass) =>
        {
            Status::Pass
        }
        0 => Status::Fail,
        1 => Status::Error,
        2 => Status::Timeout,
        // `PRECONDITION_FAILED`: optional features the test needs are absent.
        // wptrunner reports it as a skip, and the score counts it as one.
        3 => Status::Skip,
        other => {
            return Err(Failure::Error(format!(
                "harness reported unknown status code {other}"
            )));
        }
    };
    let message = value
        .get("message")
        .and_then(Value::as_str)
        .filter(|message| !message.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            subtests
                .iter()
                .find(|subtest| subtest.status != SubtestStatus::Pass)
                .and_then(|subtest| subtest.message.clone())
        });
    Ok(Completed {
        status,
        message,
        subtests,
    })
}

/// Reads one reported subtest. A status code testharness.js does not define is
/// a harness error: guessing would turn a broken reporter into a verdict.
fn parse_subtest(value: &Value) -> Result<Subtest, Failure> {
    let code = value.get("status").and_then(Value::as_u64).unwrap_or(3);
    let status = SubtestStatus::from_harness_code(code)
        .ok_or_else(|| Failure::Error(format!("subtest reported unknown status code {code}")))?;
    Ok(Subtest {
        name: value
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("Test")
            .to_owned(),
        status,
        message: value
            .get("message")
            .and_then(Value::as_str)
            .filter(|message| !message.is_empty())
            .map(str::to_owned),
    })
}

#[cfg(test)]
mod reftest_tests {
    use super::{Fuzzy, Relation, compare_reftest};
    use renderer::RgbaImage;

    fn solid(pixel: [u8; 4]) -> RgbaImage {
        RgbaImage {
            width: 2,
            height: 2,
            data: pixel.iter().copied().cycle().take(16).collect(),
        }
    }

    #[test]
    fn exact_match_and_mismatch() {
        let same = compare_reftest(
            &solid([1, 2, 3, 4]),
            &solid([1, 2, 3, 4]),
            None,
            Relation::Match,
        );
        assert!(same.matches);
        assert_eq!((same.max_difference, same.differing), (0, 0));
        let inverted = compare_reftest(
            &solid([1, 2, 3, 4]),
            &solid([1, 2, 3, 4]),
            None,
            Relation::Mismatch,
        );
        assert!(!inverted.matches);
    }

    #[test]
    fn fuzzy_tolerance_is_two_bounds() {
        let fuzzy = Some(Fuzzy {
            max_difference: 1,
            total_pixels: 4,
        });
        let pass = compare_reftest(
            &solid([0, 0, 0, 0]),
            &solid([1, 0, 0, 0]),
            fuzzy.as_ref(),
            Relation::Match,
        );
        assert!(pass.matches);
        assert_eq!((pass.max_difference, pass.differing), (1, 4));
        // The pixel count bound is separate from the channel bound.
        let too_many = compare_reftest(
            &solid([0, 0, 0, 0]),
            &solid([1, 0, 0, 0]),
            Some(&Fuzzy {
                max_difference: 1,
                total_pixels: 3,
            }),
            Relation::Match,
        );
        assert!(!too_many.matches);
        // A larger channel difference exceeds the channel bound.
        let too_different = compare_reftest(
            &solid([0, 0, 0, 0]),
            &solid([2, 0, 0, 0]),
            fuzzy.as_ref(),
            Relation::Match,
        );
        assert!(!too_different.matches);
    }

    #[test]
    fn a_size_change_never_matches() {
        let small = RgbaImage {
            width: 1,
            height: 1,
            data: vec![0, 0, 0, 0],
        };
        let large = RgbaImage {
            width: 2,
            height: 1,
            data: vec![0; 8],
        };
        assert!(!compare_reftest(&small, &large, None, Relation::Match).matches);
        assert!(compare_reftest(&small, &large, None, Relation::Mismatch).matches);
    }
}
