use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Instant;

use rquickjs::{
    ArrayBuffer, CaughtError, Context, Ctx, Function, Module, Object, Runtime, Value,
    context::EvalOptions,
};

use wpt_report::{Outcome, Status, Subtest, SubtestStatus};

use crate::meta::{Meta, Phase};

#[derive(Clone, Debug)]
pub struct TestCase {
    pub relative: String,
    pub strict: bool,
    pub source: String,
    pub meta: Meta,
}

impl TestCase {
    #[must_use]
    pub fn wpt_id(&self) -> String {
        let stem = self.relative.strip_suffix(".js").unwrap_or(&self.relative);
        let suffix = if self.strict {
            ".test262.strict.html"
        } else {
            ".test262.html"
        };
        format!("/third_party/test262/{stem}{suffix}")
    }
}

pub struct Harness {
    pub root: PathBuf,
    pub scripts: BTreeMap<String, String>,
}

impl Harness {
    pub fn load(root: &Path) -> std::io::Result<Self> {
        let harness_root = root.join("harness");
        let mut scripts = BTreeMap::new();
        collect_scripts(&harness_root, &harness_root, &mut scripts)?;
        Ok(Self {
            root: root.to_path_buf(),
            scripts,
        })
    }
}

fn collect_scripts(
    base: &Path,
    directory: &Path,
    scripts: &mut BTreeMap<String, String>,
) -> std::io::Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_scripts(base, &path, scripts)?;
        } else if path.extension().is_some_and(|extension| extension == "js") {
            let name = path
                .strip_prefix(base)
                .map_err(std::io::Error::other)?
                .to_string_lossy()
                .replace('\\', "/");
            scripts.insert(name, fs::read_to_string(&path)?);
        }
    }
    Ok(())
}

#[must_use]
pub fn run(runtime: &Runtime, harness: &Harness, case: &TestCase) -> Outcome {
    let started = Instant::now();
    let verdict = if case.meta.is_module() {
        run_module(runtime, harness, case)
    } else if case.meta.is_only_strict() && !case.strict || case.meta.is_no_strict() && case.strict
    {
        Verdict::Skip("variant excluded by strict flags".into())
    } else {
        run_file(runtime, harness, case)
    };
    let (status, message) = match verdict {
        Verdict::Pass => (Status::Pass, None),
        Verdict::Fail(message) => (Status::Fail, Some(message)),
        Verdict::Skip(message) => (Status::Skip, Some(message)),
    };
    Outcome {
        id: case.wpt_id(),
        status,
        subtests: vec![Subtest {
            name: "Test".to_owned(),
            status: match status {
                Status::Pass => SubtestStatus::Pass,
                Status::Fail => SubtestStatus::Fail,
                Status::Timeout => SubtestStatus::Timeout,
                Status::Error => SubtestStatus::NotRun,
                Status::Skip => SubtestStatus::Skip,
            },
            message: message.clone(),
        }],
        message,
        duration: started.elapsed(),
        testharness: true,
    }
}

enum Verdict {
    Pass,
    Fail(String),
    Skip(String),
}

enum Failure {
    Syntax(String),
    Resolution(String),
    Runtime {
        name: Option<String>,
        message: String,
    },
    Host(String),
}

impl Failure {
    fn message(&self) -> String {
        match self {
            Self::Syntax(message) | Self::Resolution(message) | Self::Host(message) => {
                message.clone()
            }
            Self::Runtime { name, message, .. } => match name {
                Some(name) => format!("{name}: {message}"),
                None => message.clone(),
            },
        }
    }

    fn matches(&self, expected: Option<&str>) -> bool {
        let Some(expected) = expected else {
            return true;
        };
        match self {
            Self::Runtime { name, .. } => name.as_deref() == Some(expected),
            Self::Syntax(_) => expected == "SyntaxError",
            Self::Resolution(_) | Self::Host(_) => false,
        }
    }
}

fn run_file(runtime: &Runtime, harness: &Harness, case: &TestCase) -> Verdict {
    let async_result: Rc<RefCell<Option<Result<(), String>>>> = Rc::new(RefCell::new(None));
    let context = match Context::full(runtime) {
        Ok(context) => context,
        Err(error) => return Verdict::Fail(format!("context: {error}")),
    };
    let mut failure = None;
    context.with(|ctx| {
        failure = prepare(&ctx, harness, case, Rc::clone(&async_result));
        if failure.is_some() {
            return;
        }
        let body = if case.strict {
            format!("\"use strict\";\n{}", case.source)
        } else {
            case.source.clone()
        };
        if let Err(error) = eval(&ctx, &body) {
            failure = Some(error);
        }
    });
    if let Some(failure) = failure {
        return classify_failure(&case.meta, &failure);
    }
    finish(runtime, &case.meta, &async_result)
}

fn run_module(runtime: &Runtime, harness: &Harness, case: &TestCase) -> Verdict {
    let async_result: Rc<RefCell<Option<Result<(), String>>>> = Rc::new(RefCell::new(None));
    let context = match Context::full(runtime) {
        Ok(context) => context,
        Err(error) => return Verdict::Fail(format!("context: {error}")),
    };
    let mut failure = None;
    context.with(|ctx| {
        failure = prepare(&ctx, harness, case, Rc::clone(&async_result));
        if failure.is_some() {
            return;
        }
        let promise =
            match Module::evaluate(ctx.clone(), case.relative.clone(), case.source.clone()) {
                Ok(promise) => promise,
                Err(error) => {
                    failure = Some(failure_from_error(&ctx, error));
                    return;
                }
            };
        let settled = promise.finish::<()>();
        while ctx.execute_pending_job() {}
        if let Err(error) = settled {
            failure = Some(failure_from_error(&ctx, error));
        }
    });
    if let Some(failure) = failure {
        return classify_failure(&case.meta, &failure);
    }
    finish(runtime, &case.meta, &async_result)
}

fn prepare(
    ctx: &Ctx<'_>,
    harness: &Harness,
    case: &TestCase,
    async_result: Rc<RefCell<Option<Result<(), String>>>>,
) -> Option<Failure> {
    if let Err(error) = install_host(ctx, async_result) {
        return Some(Failure::Host(error));
    }
    for name in ["assert.js", "sta.js"] {
        let Some(source) = harness.scripts.get(name) else {
            return Some(Failure::Host(format!("missing harness {name}")));
        };
        if let Err(error) = eval(ctx, source) {
            return Some(Failure::Host(format!(
                "harness {name}: {}",
                error.message()
            )));
        }
    }
    for include in &case.meta.includes {
        let Some(source) = harness.scripts.get(include) else {
            return Some(Failure::Host(format!("missing include {include}")));
        };
        if let Err(error) = eval(ctx, source) {
            return Some(Failure::Host(format!(
                "include {include}: {}",
                error.message()
            )));
        }
    }
    if case.meta.is_async()
        && let Some(source) = harness.scripts.get("doneprintHandle.js")
        && let Err(error) = eval(ctx, source)
    {
        return Some(Failure::Host(format!(
            "doneprintHandle.js: {}",
            error.message()
        )));
    }
    None
}

fn finish(
    runtime: &Runtime,
    meta: &Meta,
    async_result: &Rc<RefCell<Option<Result<(), String>>>>,
) -> Verdict {
    if !meta.is_async() {
        return completed(meta);
    }
    if let Err(error) = drain_jobs(runtime) {
        return Verdict::Fail(format!("pending job: {error}"));
    }
    match async_result.borrow_mut().take() {
        Some(Ok(())) => completed(meta),
        Some(Err(error)) => Verdict::Fail(error),
        None => Verdict::Fail("async test never called $DONE".into()),
    }
}

fn completed(meta: &Meta) -> Verdict {
    match &meta.negative {
        Some(negative) => Verdict::Fail(format!(
            "expected {} but the test completed",
            negative.error_type.as_deref().unwrap_or("an exception")
        )),
        None => Verdict::Pass,
    }
}

fn classify_failure(meta: &Meta, failure: &Failure) -> Verdict {
    let Some(negative) = &meta.negative else {
        return Verdict::Fail(failure.message());
    };
    let expected = negative.error_type.as_deref();
    if negative.phase != Phase::Runtime
        && matches!(failure, Failure::Syntax(_) | Failure::Resolution(_))
    {
        return Verdict::Pass;
    }
    if failure.matches(expected) {
        return Verdict::Pass;
    }
    let expected = expected.unwrap_or("an exception");
    Verdict::Fail(format!("expected {expected}, got {}", failure.message()))
}

fn install_host(
    ctx: &Ctx<'_>,
    async_result: Rc<RefCell<Option<Result<(), String>>>>,
) -> Result<(), String> {
    let print = Function::new(ctx.clone(), move |message: String| {
        if message == "Test262:AsyncTestComplete" {
            *async_result.borrow_mut() = Some(Ok(()));
        } else if let Some(reason) = message.strip_prefix("Test262:AsyncTestFailure:") {
            *async_result.borrow_mut() = Some(Err(reason.to_owned()));
        }
    })
    .map_err(|error| error.to_string())?;
    ctx.globals()
        .set("print", print)
        .map_err(|error| error.to_string())?;

    let host = Object::new(ctx.clone()).map_err(|error| error.to_string())?;
    host.set("global", ctx.globals())
        .map_err(|error| error.to_string())?;
    let eval_script = Function::new(ctx.clone(), |ctx: Ctx<'_>, source: String| {
        let mut options = EvalOptions::default();
        options.strict = false;
        ctx.eval_with_options::<(), _>(source, options)
    })
    .map_err(|error| error.to_string())?;
    host.set("evalScript", eval_script)
        .map_err(|error| error.to_string())?;
    let detach = Function::new(ctx.clone(), |mut buffer: ArrayBuffer<'_>| {
        buffer.detach();
    })
    .map_err(|error| error.to_string())?;
    host.set("detachArrayBuffer", detach)
        .map_err(|error| error.to_string())?;
    let gc = Function::new(ctx.clone(), |ctx: Ctx<'_>| ctx.run_gc())
        .map_err(|error| error.to_string())?;
    host.set("gc", gc).map_err(|error| error.to_string())?;
    ctx.globals()
        .set("$262", host)
        .map_err(|error| error.to_string())
}

fn eval(ctx: &Ctx<'_>, source: &str) -> Result<(), Failure> {
    let mut options = EvalOptions::default();
    options.strict = false;
    ctx.eval_with_options::<(), _>(source, options)
        .map_err(|error| failure_from_error(ctx, error))
}

fn failure_from_error(ctx: &Ctx<'_>, error: rquickjs::Error) -> Failure {
    match CaughtError::from_error(ctx, error) {
        CaughtError::Exception(exception) => exception_failure(&exception),
        CaughtError::Value(value) => value_failure(&value),
        CaughtError::Error(error) => match error {
            rquickjs::Error::Resolving { .. } | rquickjs::Error::Loading { .. } => {
                Failure::Resolution(error.to_string())
            }
            other => Failure::Host(other.to_string()),
        },
    }
}

fn exception_failure(exception: &rquickjs::Exception<'_>) -> Failure {
    let name = constructor_name(exception.as_object());
    let message = exception.message().unwrap_or_default();
    if name.as_deref() == Some("SyntaxError") {
        Failure::Syntax(message)
    } else {
        Failure::Runtime { name, message }
    }
}

fn constructor_name(object: &Object<'_>) -> Option<String> {
    object
        .get::<_, Function<'_>>("constructor")
        .and_then(|constructor| constructor.get::<_, String>("name"))
        .ok()
}

fn value_failure(value: &Value<'_>) -> Failure {
    let Some(object) = value.as_object() else {
        return Failure::Runtime {
            name: None,
            message: value
                .as_string()
                .and_then(|value| value.to_string().ok())
                .unwrap_or_else(|| format!("{value:?}")),
        };
    };
    let name = constructor_name(object);
    let message = object
        .get::<_, Option<String>>("message")
        .ok()
        .flatten()
        .unwrap_or_else(|| format!("{value:?}"));
    Failure::Runtime { name, message }
}

fn drain_jobs(runtime: &Runtime) -> Result<(), String> {
    loop {
        match runtime.execute_pending_job() {
            Ok(true) => {}
            Ok(false) => return Ok(()),
            Err(error) => return Err(error.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::meta::{Meta, Negative, Phase};

    use super::{TestCase, Verdict, completed};

    #[test]
    fn a_negative_test_that_completes_is_a_failure() {
        let meta = Meta {
            negative: Some(Negative {
                error_type: Some("TypeError".into()),
                phase: Phase::Early,
            }),
            ..Meta::default()
        };
        match completed(&meta) {
            Verdict::Fail(message) => {
                assert_eq!(message, "expected TypeError but the test completed");
            }
            Verdict::Pass | Verdict::Skip(_) => panic!("a negative test must not pass"),
        }
        assert!(matches!(completed(&Meta::default()), Verdict::Pass));
    }

    #[test]
    fn the_wpt_id_replaces_the_js_extension() {
        let case = TestCase {
            relative: "test/language/arguments-object/10.5-1-s.js".into(),
            strict: true,
            source: String::new(),
            meta: Meta::default(),
        };
        assert_eq!(
            case.wpt_id(),
            "/third_party/test262/test/language/arguments-object/10.5-1-s.test262.strict.html"
        );
        let case = TestCase {
            strict: false,
            ..case
        };
        assert_eq!(
            case.wpt_id(),
            "/third_party/test262/test/language/arguments-object/10.5-1-s.test262.html"
        );
    }
}
