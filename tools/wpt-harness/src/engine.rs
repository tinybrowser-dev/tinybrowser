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

#[derive(Clone, Copy, Debug)]
pub struct Budgets {
    pub normal: Duration,
    pub long: Duration,
}

impl Budgets {
    pub(crate) fn for_case(self, case: &TestCase) -> Duration {
        if case.long_timeout {
            self.long
        } else {
            self.normal
        }
    }
}

const POLL: &str =
    "String(globalThis.__tinybrowser_result === undefined ? '' : globalThis.__tinybrowser_result)";

pub use wpt_report::Status;

#[derive(Clone, Debug)]
pub struct Outcome {
    pub id: String,
    pub kind: Kind,
    pub status: Status,
    pub message: Option<String>,
    pub subtests: Vec<Subtest>,
    pub duration: Duration,
}

pub struct Session {
    root: std::path::PathBuf,
    services: Arc<FsServices>,
    renderer: EmbeddedRenderer,
    budgets: Budgets,
    content_types: HashMap<String, String>,
}

impl Session {
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

    fn mount(&mut self, case: &TestCase, deadline: Instant) -> Result<(), Failure> {
        self.mount_source(
            &case.relative,
            &case.source,
            case.bytes.clone(),
            case.content_type.clone(),
            deadline,
        )
    }

    fn mount_source(
        &mut self,
        relative: &str,
        source: &str,
        body: Vec<u8>,
        fallback_content_type: String,
        deadline: Instant,
    ) -> Result<(), Failure> {
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
        // https://fetch.spec.whatwg.org/#http-network-or-cache-fetch
        self.services.store_set_cookies(&headers.list, &url);
        let content_type = headers.content_type.unwrap_or(fallback_content_type);
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
                    tab: renderer::TabState::default(),
                    body,
                },
                deadline.saturating_duration_since(Instant::now()),
            )
            .map_err(Failure::Mount)
    }
}

struct Completed {
    status: Status,
    message: Option<String>,
    subtests: Vec<Subtest>,
}

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
        if renderer.has_pending_dials() {
            std::thread::sleep(Duration::from_micros(200));
            continue;
        }
        let Some(delay) = renderer.time_until_deadline() else {
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

const SCREENSHOT_FRAMES: u32 = 3;

fn has_pending_frame_callbacks(session: &mut Session) -> bool {
    session
        .renderer
        .eval("String(globalThis.__tbPendingFrameCallbacks())")
        .is_ok_and(|count| count != "0")
}

async fn advance(session: &mut Session, virtual_time: bool, delay: Duration) {
    if virtual_time {
        session.services.advance_clock(delay);
        tokio::time::advance(delay).await;
    } else {
        tokio::time::sleep(delay.min(Duration::from_millis(50))).await;
    }
}

// https://web-platform-tests.org/writing-tests/reftests.html#reftest-wait
async fn settle_reftest(
    session: &mut Session,
    deadline: Instant,
    virtual_time: bool,
    fire_test_rendered: bool,
) -> Result<(), Failure> {
    let mut rendered = false;
    // https://html.spec.whatwg.org/multipage/imagebitmap-and-animations.html#run-the-animation-frame-callbacks
    let mut frames_left = SCREENSHOT_FRAMES;
    loop {
        let _yielded = session.renderer.drain_until(deadline);
        if session.renderer.has_pending_dials() {
            if Instant::now() >= deadline {
                return Err(Failure::Timeout);
            }
            std::thread::sleep(Duration::from_micros(200));
            continue;
        }
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
            if frames_left > 0 && has_pending_frame_callbacks(session) {
                frames_left -= 1;
                if let Some(delay) = session.renderer.time_until_deadline() {
                    advance(session, virtual_time, delay).await;
                    continue;
                }
            }
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(Failure::Timeout);
        }
        let Some(delay) = session.renderer.time_until_deadline() else {
            return Err(Failure::Timeout);
        };
        if !delay.is_zero() {
            advance(session, virtual_time, delay).await;
        }
    }
}

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

struct Diff {
    max_difference: u8,
    differing: u64,
    matches: bool,
}

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

fn parse_result(raw: &str) -> Result<Completed, Failure> {
    let value: Value = serde_json::from_str(raw)
        .map_err(|error| Failure::Error(format!("bad harness result: {error}")))?;
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
