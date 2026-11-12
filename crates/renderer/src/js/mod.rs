//! `QuickJS` host for one document: eval, timers, `fetch`, DOM host objects.
//!
//! Callbacks live in JS (`__tb_timeouts`, `__tb_fetchCbs`). Rust holds
//! integer ids so a `Function` never crosses the JS boundary. Invocation
//! uses `Function::call` on the renderer thread.

// The rquickjs native-function ABI hands every host function and method its
// `Ctx` and JS arguments by value, so the lint fires on the whole binding
// layer. Borrowing them is the fork-side fix, tracked for the rquickjs fork in
// `docs/upstream.md`; one scoped allowance stands in until then.
#![expect(
    clippy::needless_pass_by_value,
    reason = "the rquickjs native-function ABI passes Ctx and JS arguments by value"
)]

mod bindings;
mod blob;
mod bridge;
mod canvas;
mod encoding;
mod events;
mod fonts;
mod intl;
mod modules;
mod runtime;
mod url_parts;
pub(crate) mod world;

mod observers;
mod reactions;

pub(crate) use bindings::selector_matching_elements;
pub(crate) use bindings::{element_at_point, element_box, viewport_scroll};
pub(crate) use bridge::js_number;
pub use runtime::Stop;
pub(crate) use runtime::{FrameRuntime, ObjectUrlStore, SharedBufferTable, WorkerRequests};
pub(crate) use world::{DocumentStreamCommand, FrameNavigation, NavigationTarget, RealmRegistry};

use enumerated::Enumerated;
use std::cell::{Cell, OnceCell, RefCell};
use std::fmt;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use rquickjs::{
    Array, Coerced, Context, Ctx, Exception, FromJs, Function, Object, Persistent, Runtime,
    TypedArray, Value, context::EvalOptions, prelude::Func,
};
use url::Url;

use crate::protocol::{DialRequest, FrameId, ReferrerRequest, RequestPolicy, TargetName, WindowId};

pub(crate) use world::{CreatorInfo, NavigationReferrer, PolicyContainer, RealmRequest, World};

/// Shared `QuickJS` heap cap. Forty live subframe realms need more than
/// 32MB (measured: `Document-createElement-namespace.html`, 40 parallel
/// iframes, wedges past ~24 XML realms at 32MB with an allocation failure
/// surfacing as a generic `QuickJS` exception; green at 256MB). 256MB still
/// bounds a malicious page while fitting MAX_FRAMES-scale bursts. A
/// per-realm budget would isolate better; the shared heap is the current
/// `QuickJS` shape (see `SharedRuntime`).
const MAX_RUNTIME_MEMORY: usize = 256 * 1024 * 1024;
const MAX_RUNTIME_STACK: usize = 512 * 1024;
/// Wall-clock limit for one top-level entry into JS (a script, a timer or
/// event callback, a browser evaluation without its own timeout) and its
/// microtask checkpoint. Past it the interrupt handler aborts the task with
/// [`JsError::Interrupted`]. Serial top-level entries in one renderer turn
/// share [`TURN_SCRIPT_BUDGET`] so they cannot each take a full 20 s and
/// together hit the browser's 60 s renderer request timeout.
const DEFAULT_SCRIPT_BUDGET: Duration = Duration::from_secs(20);
/// Wall-clock cap for one renderer turn: a parser script walk, a batch of
/// init scripts, or one drain of ready tasks from the session's inbox wait
/// back to the next wait. Each JS entry still uses `DEFAULT_SCRIPT_BUDGET`,
/// then clamps to this deadline so N serial runaways stay well under the 60 s
/// request timeout in `crates/browser/src/link.rs`.
pub const TURN_SCRIPT_BUDGET: Duration = Duration::from_secs(45);

#[derive(Clone, Copy)]
enum MicrotaskCheckpoint {
    Perform,
    Defer,
}

/// Web-platform JS shims, one spec area per file, evaluated in order as a
/// single script so top-level bindings are shared across areas.
///
/// The whole concatenation runs inside one function scope: top-level
/// `const`/`function` bindings stay visible to every shim file but invisible
/// to page script, which shares the global lexical scope. Only explicit
/// `globalThis` assignments publish names outward.
///
/// The bundle is assembled and deflated by `build.rs` from the file order in
/// `scripts/web/order.txt`; it inflates once per process.
const INSTALL_WEB_APIS_DEFLATE: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/js_blobs/web_bundle.deflate"));

/// The web shim bundle, inflated once per process.
fn install_web_apis_js(ctx: &Ctx<'_>) -> rquickjs::Result<&'static str> {
    static CACHE: std::sync::OnceLock<Box<str>> = std::sync::OnceLock::new();
    blob::decompress(ctx, INSTALL_WEB_APIS_DEFLATE, &CACHE)
}

/// The worker-only shim bundle: the web APIs a `WorkerGlobalScope` exposes,
/// without the document and browsing-context shims.
///
/// Assembled by `build.rs` from `scripts/web/worker_order.txt`.
const INSTALL_WORKER_APIS_DEFLATE: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/js_blobs/worker_bundle.deflate"));

fn install_worker_apis_js(ctx: &Ctx<'_>) -> rquickjs::Result<&'static str> {
    static CACHE: std::sync::OnceLock<Box<str>> = std::sync::OnceLock::new();
    blob::decompress(ctx, INSTALL_WORKER_APIS_DEFLATE, &CACHE)
}

/// A value produced by script evaluation.
#[derive(Clone, Debug, PartialEq)]
pub enum ScriptValue {
    /// JS `undefined`.
    Undefined,
    /// JS `null`.
    Null,
    /// JS boolean.
    Bool(bool),
    /// JS number.
    Number(f64),
    /// JS string.
    String(String),
    /// A DOM node handle for `WebDriver` element encoding.
    Node(crate::dom::tree::NodeId),
    /// A JS array, for `WebDriver` JSON.
    List(Vec<ScriptValue>),
    /// A JS object, for `WebDriver` JSON.
    Map(Vec<(String, ScriptValue)>),
}

/// A `<script>` in document order.
///
/// [HTML prepare the script element](https://html.spec.whatwg.org/multipage/webappapis.html#prepare-the-script-element)
#[derive(Clone)]
pub(crate) enum Script {
    Classic(ScriptSource),
    Module(ScriptSource),
}

#[derive(Clone)]
pub(crate) enum ScriptSource {
    /// Inline text, with the 1-based document line its source starts on.
    Inline {
        source: String,
        line: u32,
    },
    Src(String),
}

#[derive(Debug)]
pub(crate) enum JsError {
    Engine(Box<str>),
    Interrupted,
    BadTimerId,
}

impl JsError {
    fn engine(err: impl fmt::Display) -> Self {
        Self::Engine(err.to_string().into_boxed_str())
    }
}

impl fmt::Display for JsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Engine(message) => f.write_str(message),
            Self::Interrupted => f.write_str("interrupted"),
            Self::BadTimerId => f.write_str("bad timer id"),
        }
    }
}

impl std::error::Error for JsError {}

/// Why a worker's top-level classic script did not complete.
///
/// The worker host acts on the cases differently: a parse failure discards the
/// environment and fires a plain `error` event at the `Worker` object, a
/// runtime exception was already reported at the worker global and leaves the
/// worker's event loop running, and an interruption stops the worker without a
/// script-visible event
/// (<https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model>).
pub(crate) enum WorkerScriptError {
    /// The script did not compile; no JavaScript in it ran.
    Parse,
    /// The script ran and threw; [`report_script_error`] reported the value.
    Runtime,
    /// The renderer turn's deadline or the engine's stop flag cut evaluation.
    Interrupted,
}

impl From<rquickjs::Error> for JsError {
    fn from(err: rquickjs::Error) -> Self {
        Self::Engine(err.to_string().into_boxed_str())
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct JsFetchId(i32);

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct PageTimerId(i32);

pub(crate) struct TimeoutRequest {
    pub delay: Duration,
    pub js_id: PageTimerId,
}

/// One page `fetch` / XHR dial, parsed from the shim's options object.
pub(crate) struct JsFetchRequest {
    pub url: String,
    pub js_id: JsFetchId,
    pub method: net::Method,
    pub body: Vec<u8>,
    pub content_type: Option<String>,
    pub headers: Vec<(String, String)>,
    pub options: DialOptions,
}

/// The options object `fetch.js` hands `__queueFetch`: one named field per
/// option the renderer's dial needs, parsed once at the boundary.
///
/// The shim writes every key; `null` is how a request with no value says so,
/// and each `null` has one meaning here: `referrer: null` is no referrer,
/// `integrity: null` and `referrerPolicy: null` are unset, and `cache: null`
/// is the IDL default mode. An unknown spelling is a `TypeError` that names
/// the key and value; it never becomes a different, valid-looking request.
pub(crate) struct DialOptions {
    pub(crate) referrer: crate::protocol::ReferrerRequest,
    pub(crate) cors: crate::protocol::CorsRequest,
    pub(crate) keepalive: bool,
    pub(crate) body_len: u64,
    pub(crate) integrity: Option<String>,
    pub(crate) cache: crate::protocol::RequestCache,
    pub(crate) delivery: crate::protocol::BodyDelivery,
    pub(crate) referrer_policy: Option<net::ReferrerPolicy>,
}

impl DialOptions {
    fn from_js(options: &Object<'_>, client_url: &url::Url) -> rquickjs::Result<Self> {
        fn enum_member<T>(
            ctx: &Ctx<'_>,
            options: &Object<'_>,
            key: &'static str,
            parse: impl Fn(&str) -> Option<T>,
        ) -> rquickjs::Result<T> {
            let value: String = options.get(key)?;
            parse(&value).ok_or_else(|| invalid_option(ctx, key, &value))
        }

        fn nullable_enum_member<T>(
            ctx: &Ctx<'_>,
            options: &Object<'_>,
            key: &'static str,
            parse: impl Fn(&str) -> Option<T>,
        ) -> rquickjs::Result<Option<T>> {
            match options.get::<_, Option<String>>(key)? {
                None => Ok(None),
                Some(value) => parse(&value)
                    .map(Some)
                    .ok_or_else(|| invalid_option(ctx, key, &value)),
            }
        }

        /// A value the IDL enum does not name is a `TypeError` that says which
        /// option and which value, at the boundary that reads it.
        fn invalid_option(ctx: &Ctx<'_>, key: &'static str, value: &str) -> rquickjs::Error {
            Exception::throw_type(ctx, &format!("{key}: {value:?} is not a request option"))
        }

        let ctx = options.ctx();
        let referrer = match options.get::<_, Option<String>>("referrer")? {
            Some(value) => crate::protocol::ReferrerRequest::from_idl(&value, client_url)
                .ok_or_else(|| {
                    Exception::throw_type(ctx, &format!("referrer: {value:?} is not a referrer"))
                })?,
            None => crate::protocol::ReferrerRequest::NoReferrer,
        };
        let cors = crate::protocol::CorsRequest {
            // `fetch` always passes the mode its `Request` normalized
            // (<https://fetch.spec.whatwg.org/#concept-request-mode>).
            mode: enum_member(ctx, options, "mode", crate::protocol::CorsMode::from_idl)?,
            credentials: enum_member(
                ctx,
                options,
                "credentials",
                net::cors::CredentialsMode::from_idl,
            )?,
            redirect: enum_member(ctx, options, "redirect", net::cors::RedirectMode::from_idl)?,
        };
        let cache = nullable_enum_member(
            ctx,
            options,
            "cache",
            crate::protocol::RequestCache::from_idl,
        )?
        .unwrap_or_default();
        let delivery = enum_member(
            ctx,
            options,
            "delivery",
            crate::protocol::BodyDelivery::from_idl,
        )?;
        let referrer_policy =
            nullable_enum_member(ctx, options, "referrerPolicy", net::ReferrerPolicy::parse)?;
        Ok(Self {
            referrer,
            cors,
            keepalive: options
                .get::<_, Option<bool>>("keepalive")?
                .unwrap_or(false),
            body_len: options.get::<_, Option<u64>>("bodySize")?.unwrap_or(0),
            integrity: options.get::<_, Option<String>>("integrity")?,
            cache,
            delivery,
            referrer_policy,
        })
    }
}

/// One renderer process's `QuickJS` heap, created on first use and shared by
/// every frame in the process.
///
/// Several `QuickJS` contexts may share one runtime and its objects, "similar to
/// frames of the same origin sharing JavaScript objects in a web browser"
/// (<https://bellard.org/quickjs/quickjs.html>, JSRuntime): the same-site-frame
/// model.
/// The creation result is cached so a heap that cannot start fails every realm
/// the same way instead of retrying.
#[derive(Clone, Default)]
pub(crate) struct SharedJsRuntime(Rc<OnceCell<Result<Runtime, Box<str>>>>);

#[derive(Clone)]
pub(crate) struct JsRuntimeHandle(Weak<OnceCell<Result<Runtime, Box<str>>>>);

impl JsRuntimeHandle {
    fn get(&self) -> Result<Runtime, JsError> {
        let owner = self
            .0
            .upgrade()
            .ok_or_else(|| JsError::Engine("renderer JS runtime has been released".into()))?;
        SharedJsRuntime(owner).get().cloned()
    }
}

impl SharedJsRuntime {
    pub(crate) fn handle(&self) -> JsRuntimeHandle {
        JsRuntimeHandle(Rc::downgrade(&self.0))
    }

    pub(crate) fn collect(&self) {
        if let Some(Ok(runtime)) = self.0.get() {
            runtime.run_gc();
        }
    }

    pub(crate) fn get(&self) -> Result<&Runtime, JsError> {
        let slot = self.0.get_or_init(|| {
            let runtime = Runtime::new().map_err(|err| err.to_string().into_boxed_str())?;
            // `SharedArrayBuffer` data must be reference-counted across
            // runtimes: a buffer a page posts to a worker thread is the same
            // block, and the receiving runtime wraps it with
            // `JS_NewArrayBuffer(..., is_shared = true)`
            // (<https://tc39.es/ecma262/#sec-abstract-operations-for-sharedarraybuffer-objects>).
            runtime.install_shared_array_buffer_hooks();
            runtime.set_loader(modules::WebModuleResolver, modules::WebModuleLoader);
            // QuickJS reports a rejected-without-handler promise and a late
            // handler for one here; the events fire at the microtask
            // checkpoint
            // (<https://html.spec.whatwg.org/multipage/webappapis.html#unhandled-promise-rejections>).
            runtime.set_host_promise_rejection_tracker(Some(Box::new(
                |ctx, promise, reason, handled| {
                    bindings::note_rejection(&ctx, promise, reason, handled);
                },
            )));
            // String compilation from JS is a CSP surface the engine must own.
            // A JS wrapper over `eval` or `Function` is observable and turns
            // every direct eval into an indirect one
            // (<https://tc39.es/ecma262/#sec-evaluatecall>), so the gate sits
            // below the JS surface, where the intrinsics keep their identity
            // (<https://w3c.github.io/webappsec-csp/#can-compile-strings>).
            runtime.set_can_compile_strings(Some(Box::new(|ctx, source| {
                let Some(world) = bindings::try_world(&ctx) else {
                    // No realm behind this context: no policy to enforce.
                    return true;
                };
                // The timer initialization steps already checked the string
                // when the timer was created; running the handler is not a
                // second evaluation. Consume the allowance so an `eval`
                // inside the handler is checked again.
                if world.borrow().timer_string.replace(false) {
                    return true;
                }
                // An event handler content attribute is compiled as a
                // function body by the engine, not by page script. CSP governs
                // it with the inline check (`script-src-attr`), which runs
                // when the handler is dispatched; subjecting it to `can
                // compile strings` would demand `'unsafe-eval'` for an
                // `onload` attribute
                // (<https://w3c.github.io/webappsec-csp/#can-compile-strings>).
                if world.borrow().compiling_handler.get() {
                    return true;
                }
                match csp_eval_violations(&ctx, &world, source) {
                    Ok(violations) if !crate::csp::blocks(&violations) => true,
                    Ok(violations) => {
                        throw_eval_error(&ctx, &eval_denied_message(&violations));
                        false
                    }
                    // Reporting raised; that exception is already pending.
                    Err(_) => false,
                }
            })));
            Ok(runtime)
        });
        slot.as_ref()
            .map_err(|message| JsError::Engine(message.clone()))
    }
}

pub(crate) struct JsRealm {
    runtime: Runtime,
    context: Context,
    world: Rc<RefCell<World>>,
    stop: Arc<Stop>,
    /// Set for a worker realm: `terminate()` interrupts its script even when
    /// the page thread is the one that called it.
    terminate: Option<Arc<AtomicBool>>,
    /// Nesting depth of [`JsRealm::with_budget`]; reentrant evaluations run
    /// under the outer frame instead of installing their own budget.
    budget_depth: Cell<usize>,
    /// Deadline for the current renderer turn, shared with [`ScriptTurn`]
    /// so nested walks do not reset it. `with_budget_outer` clamps each
    /// entry to this Instant when it is set.
    turn_deadline: Rc<Cell<Option<Instant>>>,
}

impl JsRealm {
    pub(crate) fn new(
        shared: &JsRuntimeHandle,
        world: Rc<RefCell<World>>,
        stop: Arc<Stop>,
    ) -> Result<Self, JsError> {
        Self::with_install(shared, world, stop, None, false)
    }

    /// Builds a dedicated worker's realm. It shares the engine's `QuickJS`
    /// runtime with its creator but has its own global, task queue, and
    /// event-target graph.
    pub(crate) fn new_worker(
        shared: &JsRuntimeHandle,
        world: Rc<RefCell<World>>,
        stop: Arc<Stop>,
        terminate: Arc<AtomicBool>,
    ) -> Result<Self, JsError> {
        Self::with_install(shared, world, stop, Some(terminate), true)
    }

    fn with_install(
        shared: &JsRuntimeHandle,
        world: Rc<RefCell<World>>,
        stop: Arc<Stop>,
        terminate: Option<Arc<AtomicBool>>,
        worker: bool,
    ) -> Result<Self, JsError> {
        let runtime = shared.get()?;
        runtime.set_memory_limit(MAX_RUNTIME_MEMORY);
        runtime.set_max_stack_size(MAX_RUNTIME_STACK);
        let context = Context::full(&runtime)?;
        // Every realm in the renderer process shares the engine's cell, so
        // one drain deadline covers tasks that run in frames created later.
        let turn_deadline = Rc::clone(&world.borrow().runtime.turn_deadline);
        let host = Self {
            runtime,
            context,
            world,
            stop,
            terminate,
            budget_depth: Cell::new(0),
            turn_deadline,
        };
        if worker {
            if let Err(error) = host.install_worker() {
                return Err(host.install_failure(error));
            }
        } else if let Err(error) = host.install() {
            return Err(host.install_failure(error));
        }
        // Publish the context so a cross-realm script insertion can enter this
        // realm synchronously instead of deferring to the next drain.
        host.world.borrow_mut().realm_context = Some(host.context.clone());
        Ok(host)
    }

    /// Turns a realm-install failure into a message that names the shim
    /// exception instead of the engine's generic `Exception generated` text.
    fn install_failure(&self, error: JsError) -> JsError {
        let message = self.context.with(|ctx| {
            let value = ctx.catch();
            value
                .as_string()
                .and_then(|text| text.to_string().ok())
                .or_else(|| {
                    value.as_object().and_then(|object| {
                        object
                            .get::<_, Option<String>>("stack")
                            .ok()
                            .flatten()
                            .or_else(|| object.get::<_, Option<String>>("message").ok().flatten())
                    })
                })
                .unwrap_or_else(|| format!("{value:?}"))
        });
        JsError::Engine(format!("realm install failed: {error}: {message}").into_boxed_str())
    }

    /// Starts a renderer turn on this realm, or joins one already open.
    ///
    /// Dropping the last owned guard clears the turn deadline.
    pub(crate) fn enter_turn(&self) -> ScriptTurn {
        ScriptTurn::enter(&self.turn_deadline)
    }

    pub(crate) fn eval(&self, source: &str) -> Result<String, JsError> {
        self.with_budget(None, MicrotaskCheckpoint::Perform, || {
            self.context.with(|ctx| {
                let value: Value = eval_classic(&ctx, source)?;
                render_eval_result(&ctx, value)
            })
        })
    }

    /// Evaluates one parser-driven classic script and, when it throws, reports
    /// the exception to the realm's window (`window.onerror` and the `error`
    /// event) before returning the failure.
    ///
    /// `base_line` is the 1-based document line the script's first source line
    /// occupies (0 when unknown), used to translate the inline stack's
    /// script-relative line into a document line
    /// (<https://html.spec.whatwg.org/multipage/webappapis.html#report-the-error>).
    /// Records the URL of the script about to run, so a dynamic `import()` in
    /// `eval`'d code can resolve a relative specifier. `QuickJS` compiles
    /// string eval under the fixed name `<input>`, which no URL can base on
    /// (<https://tc39.es/ecma262/#sec-hostresolveimportedmodule>).
    fn enter_script_url<T>(&self, url: &str, run: impl FnOnce() -> T) -> T {
        let previous = {
            let mut world = self.world.borrow_mut();
            let previous = world.state.current_script_url.take();
            world.state.current_script_url = url::Url::parse(url).ok();
            previous
        };
        let result = run();
        self.world.borrow_mut().state.current_script_url = previous;
        result
    }

    pub(crate) fn eval_classic_script(
        &self,
        source: &str,
        base_line: u32,
        filename: &str,
    ) -> Result<(), JsError> {
        self.enter_script_url(filename, || {
            self.with_budget(None, MicrotaskCheckpoint::Perform, || {
                self.context.with(|ctx| {
                    let mut options = EvalOptions::default();
                    options.strict = false;
                    // The URL names classic-script stacks: CSP `sourceFile` and
                    // `window.onerror` lines attribute to the script that ran,
                    // not to the engine's anonymous eval label.
                    options.filename = Some(filename.into());
                    match ctx.eval_with_options::<Value, _>(source, options) {
                        Ok(_) => Ok(()),
                        Err(error) => {
                            report_script_error(&ctx, &error, base_line, filename);
                            Err(JsError::from(error))
                        }
                    }
                })
            })
        })
    }

    /// Evaluates the top-level classic script of a dedicated worker.
    ///
    /// A parse failure is not a runtime exception: `QuickJS` compiles the whole
    /// script before running any of it, so nothing has executed in the worker's
    /// realm. The spec discards the environment and fires a plain `error`
    /// event at the `Worker` object for that case, while an exception from a
    /// script that did run is reported at the worker global and the worker
    /// keeps its event loop
    /// (<https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model>,
    /// step 14).
    pub(crate) fn eval_worker_script(
        &self,
        source: &str,
        filename: &str,
    ) -> Result<(), WorkerScriptError> {
        let outcome = self.enter_script_url(filename, || {
            self.with_budget(None, MicrotaskCheckpoint::Perform, || {
                self.context.with(|ctx| {
                    let mut options = EvalOptions::default();
                    options.strict = false;
                    options.filename = Some(filename.into());
                    match ctx.eval_with_options::<Value, _>(source, options) {
                        Ok(_) => Ok(None),
                        // A host failure (allocation) has no thrown JavaScript
                        // value; report its text like any uncaught exception and
                        // leave the realm usable.
                        Err(error) if !error.is_exception() => {
                            report_script_error(&ctx, &error, 0, filename);
                            Ok(Some(WorkerScriptError::Runtime))
                        }
                        Err(_) => {
                            let caught = ctx.catch();
                            if is_script_parse_error(&caught) {
                                Ok(Some(WorkerScriptError::Parse))
                            } else {
                                bindings::report_exception_value(&ctx, caught, 0, filename);
                                Ok(Some(WorkerScriptError::Runtime))
                            }
                        }
                    }
                })
            })
        });
        match outcome {
            Ok(None) => Ok(()),
            Ok(Some(error)) => Err(error),
            // The script's own error wins over a job or interrupt error only
            // when evaluation returned one; `with_budget` reports an interrupt
            // in place of the result
            // (<https://html.spec.whatwg.org/multipage/webappapis.html#clean-up-after-running-script>).
            Err(JsError::Interrupted) => Err(WorkerScriptError::Interrupted),
            Err(_) => Err(WorkerScriptError::Runtime),
        }
    }

    /// Runs one classic inline script that was inserted from another realm
    /// before this document had a realm.
    pub(crate) fn run_deferred_inline_script(
        &self,
        id: crate::dom::tree::NodeId,
    ) -> Result<(), JsError> {
        self.with_budget(None, MicrotaskCheckpoint::Perform, || {
            self.context.with(|ctx| {
                bindings::prepare_classic_script(&ctx, id)?;
                Ok(())
            })
        })
    }

    pub(crate) fn eval_inline_module(&self, name: &str, source: &str) -> Result<(), JsError> {
        self.enter_script_url(name, || {
            self.with_budget(None, MicrotaskCheckpoint::Perform, || {
                self.context.with(|ctx| {
                    let module = modules::declare_inline(&ctx, name, source)?;
                    let (_, promise) = module.eval()?;
                    promise.finish::<()>()?;
                    Ok(())
                })
            })
        })
    }

    pub(crate) fn eval_external_module(&self, url: &str) -> Result<(), JsError> {
        self.enter_script_url(url, || {
            self.with_budget(None, MicrotaskCheckpoint::Perform, || {
                self.context.with(|ctx| {
                    let module = modules::load_module(&ctx, url)?;
                    let (_, promise) = module.eval()?;
                    promise.finish::<()>()?;
                    Ok(())
                })
            })
        })
    }

    /// Evaluates the top-level module script of a dedicated worker.
    ///
    /// A module script that fails to fetch, parse, or link is a load failure:
    /// nothing in it ran, so the environment is discarded and the owner sees a
    /// plain `error` event. An exception from the module's own evaluation is a
    /// runtime error and is reported at the worker global
    /// (<https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model>,
    /// step 14).
    pub(crate) fn eval_worker_module(&self, url: &str) -> Result<(), WorkerScriptError> {
        let outcome = self.enter_script_url(url, || {
            self.with_budget(None, MicrotaskCheckpoint::Perform, || {
                self.context.with(|ctx| {
                    // The module graph's fetch or parse failed: no module script
                    // exists to evaluate.
                    let Ok(module) = modules::load_module(&ctx, url) else {
                        return Ok(Some(WorkerScriptError::Parse));
                    };
                    let Ok((_, promise)) = module.eval() else {
                        return Ok(Some(WorkerScriptError::Parse));
                    };
                    // The graph is linked and its static imports are fetched;
                    // only the module bodies and their dynamic `import()` jobs
                    // remain. Those run with the worker's own settings object,
                    // so the outside origin stops applying before the
                    // microtask checkpoint that runs them
                    // (<https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model>).
                    self.world.borrow_mut().state.module_client_origin = None;
                    match promise.finish::<()>() {
                        Ok(()) => Ok(None),
                        // The rejection is rethrown as a pending exception, so the
                        // realm's error plumbing reports it like any other
                        // uncaught evaluation error.
                        Err(error) => {
                            report_script_error(&ctx, &error, 0, url);
                            Ok(Some(WorkerScriptError::Runtime))
                        }
                    }
                })
            })
        });
        // The root graph has been fetched and linked. A later dynamic import()
        // runs with the worker's own settings object, so stop applying the
        // outside origin
        // (<https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model>).
        self.world.borrow_mut().state.module_client_origin = None;
        match outcome {
            Ok(None) => Ok(()),
            Ok(Some(error)) => Err(error),
            Err(JsError::Interrupted) => Err(WorkerScriptError::Interrupted),
            Err(_) => Err(WorkerScriptError::Runtime),
        }
    }

    pub(crate) fn eval_value_deadline(
        &self,
        source: crate::ScriptSource<&str>,
        deadline: Option<Instant>,
    ) -> Result<crate::js::ScriptValue, JsError> {
        self.with_budget(deadline, MicrotaskCheckpoint::Perform, || {
            self.context.with(|ctx| {
                let value: Value = match source {
                    crate::ScriptSource::Page(source) => eval_classic(&ctx, source)?,
                    crate::ScriptSource::Browser(source) => {
                        let evaluate: Function = ctx.eval(format!(
                            "(function(host) {{ 'use strict'; return (\n{source}\n); }})"
                        ))?;
                        match evaluate.call((bridge::object(&ctx)?,)) {
                            Ok(value) => value,
                            Err(error) => return Err(caught_error(&ctx, error)),
                        }
                    }
                };
                decode_value(&ctx, value)
            })
        })
    }

    /// The node a runtime handle wraps, or `None` when the handle names
    /// anything else.
    pub(crate) fn node_for_handle(
        &self,
        object_id: &str,
    ) -> Result<Option<crate::dom::tree::NodeId>, JsError> {
        self.context
            .with(
                |ctx| -> rquickjs::Result<Option<crate::dom::tree::NodeId>> {
                    let host = bridge::object(&ctx)?;
                    let handles: Object = host.get("__tb_handles")?;
                    let value: Value = handles.get(object_id)?;
                    Ok(bindings::host_node_id(&ctx, &value))
                },
            )
            .map_err(JsError::engine)
    }

    /// Interns `node` as a runtime handle, the `DOM.resolveNode` shape: the
    /// wrapper is minted in Rust and stored in the bridge's private handle
    /// table, so `Runtime.callFunctionOn` can use the returned id.
    pub(crate) fn intern_node(
        &self,
        node: crate::dom::tree::NodeId,
    ) -> Result<crate::DomRemoteNode, JsError> {
        self.context
            .with(|ctx| -> rquickjs::Result<crate::DomRemoteNode> {
                let host = bridge::object(&ctx)?;
                let value = bindings::wrap_node(&ctx, node)?;
                // A stable key per node: repeated `DOM.resolveNode` calls
                // overwrite one entry instead of growing the handle table
                // (<https://chromedevtools.github.io/devtools-protocol/tot/DOM/#method-resolveNode>).
                let object_id = {
                    let remote = self.world.borrow_mut().remote_id(node);
                    format!("dom-{remote}")
                };
                let handles: Object = host.get("__tb_handles")?;
                handles.set(object_id.as_str(), value)?;
                // Read the shape from the tree, not from wrapper properties a
                // page can redefine: `resolveNode` must not run page script.
                let description = {
                    let world = self.world.borrow();
                    world.document(node).map_or_else(String::new, |parsed| {
                        crate::dom::tree::node_name(
                            &parsed.document.base,
                            node.node,
                            parsed.content_type == "text/html",
                        )
                        .unwrap_or_default()
                    })
                };
                Ok(crate::DomRemoteNode {
                    object_id,
                    class_name: "Node".to_owned(),
                    description,
                })
            })
            .map_err(JsError::engine)
    }

    pub(crate) fn fire_timer(&self, js_id: PageTimerId) -> Result<(), JsError> {
        self.with_budget(None, MicrotaskCheckpoint::Perform, || {
            self.context.with(|ctx| {
                let timeouts: Array = bridge::object(&ctx)?.get("__tb_timeouts")?;
                let idx = usize::try_from(js_id.0).map_err(|_| JsError::BadTimerId)?;
                // A cancelled timer already left the queue: firing is a
                // no-op, not a `TypeError`
                // (<https://html.spec.whatwg.org/multipage/timers-and-user-prompts.html#timers>).
                let value: Value = timeouts.get(idx)?;
                timeouts.as_object().remove(js_id.0)?;
                if value.is_undefined() || value.is_null() {
                    return Ok(());
                }
                if let Some(func) = value.as_function() {
                    if let Err(error) = func.call::<_, ()>(()) {
                        bindings::host::report_callback_error(&ctx, &error);
                    }
                    return Ok(());
                }
                // A non-function argument is compiled and run as a classic
                // script when the timer fires; a compile or runtime error is
                // reported like any uncaught script error
                // (<https://html.spec.whatwg.org/multipage/timers-and-user-prompts.html#dom-settimeout>).
                let code = Coerced::<String>::from_js(&ctx, value)?.0;
                let mut options = EvalOptions::default();
                options.strict = false;
                if let Err(error) = ctx.eval_with_options::<Value, _>(code.as_str(), options) {
                    report_script_error(&ctx, &error, 0, "");
                }
                Ok(())
            })
        })
    }

    /// Dispatches one message a worker posted on its `Worker` object in the
    /// parent realm
    /// (<https://html.spec.whatwg.org/multipage/workers.html#dom-worker-postmessage>).
    pub(crate) fn deliver_worker_event(
        &self,
        worker: u64,
        payload: &str,
        ports: &[u64],
    ) -> Result<(), JsError> {
        let ports = ports.to_vec();
        self.with_budget(None, MicrotaskCheckpoint::Perform, || {
            self.context.with(|ctx| {
                let deliver: Function = ctx.globals().get("__tbWorkerEvent")?;
                deliver.call::<_, ()>((worker, payload, ports))?;
                Ok(())
            })
        })
    }

    /// Fires a worker's `error` event on the parent's `Worker` object.
    pub(crate) fn deliver_worker_error(
        &self,
        worker: u64,
        message: &str,
        filename: &str,
        lineno: u32,
        colno: u32,
    ) -> Result<(), JsError> {
        self.with_budget(None, MicrotaskCheckpoint::Perform, || {
            self.context.with(|ctx| {
                let deliver: Function = ctx.globals().get("__tbWorkerErrorEvent")?;
                deliver.call::<_, ()>((worker, message, filename, lineno, colno))?;
                Ok(())
            })
        })
    }

    /// Fires the plain `error` event a worker that never started reports at
    /// its `Worker` object
    /// (<https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model>).
    pub(crate) fn deliver_worker_load_error(&self, worker: u64) -> Result<(), JsError> {
        self.with_budget(None, MicrotaskCheckpoint::Perform, || {
            self.context.with(|ctx| {
                let deliver: Function = ctx.globals().get("__tbWorkerLoadErrorEvent")?;
                deliver.call::<_, ()>((worker,))?;
                Ok(())
            })
        })
    }

    /// Dispatches one message posted to a worker global.
    pub(crate) fn deliver_worker_message(
        &self,
        payload: &str,
        ports: &[u64],
    ) -> Result<(), JsError> {
        let ports = ports.to_vec();
        self.with_budget(None, MicrotaskCheckpoint::Perform, || {
            self.context.with(|ctx| {
                let deliver: Function = ctx.globals().get("__tbWorkerDeliverMessage")?;
                deliver.call::<_, ()>((payload, ports))?;
                Ok(())
            })
        })
    }

    /// Dispatches one `connect` event at a shared worker global, carrying the
    /// worker-side port of a new connection
    /// (<https://html.spec.whatwg.org/multipage/workers.html#dom-sharedworkerglobalscope-onconnect>).
    pub(crate) fn deliver_shared_connect(&self, endpoint: u64) -> Result<(), JsError> {
        self.with_budget(None, MicrotaskCheckpoint::Perform, || {
            self.context.with(|ctx| {
                let deliver: Function = ctx.globals().get("__tbWorkerConnect")?;
                deliver.call::<_, ()>((endpoint,))?;
                Ok(())
            })
        })
    }

    /// Tells the page shim to dispatch `change` on `MediaQueryList`s whose
    /// result flipped (<https://drafts.csswg.org/cssom-view/#evaluate-media-queries-and-report-changes>).
    pub(crate) fn report_media_changes(&self) -> Result<(), JsError> {
        self.with_budget(None, MicrotaskCheckpoint::Perform, || {
            self.context.with(|ctx| {
                let host = bridge::object(&ctx)?;
                let Some(func) = host.get::<_, Option<Function>>("__tbReportMediaChanges")? else {
                    return Ok(());
                };
                func.call::<_, ()>(())?;
                Ok(())
            })
        })
    }

    pub(crate) fn finish_js_fetch(
        &self,
        js_id: JsFetchId,
        outcome: Option<crate::protocol::DialOutcome>,
    ) -> Result<(), JsError> {
        self.with_budget(None, MicrotaskCheckpoint::Perform, || {
            self.context.with(|ctx| {
                let cbs: Object = bridge::object(&ctx)?.get("__tb_fetchCbs")?;
                let Some(func) = cbs.get::<_, Option<Function>>(js_id.0)? else {
                    // The page aborted between the dial and this callback.
                    // A streamed body's task would fill its channel and hold
                    // the dial permits for nobody, so cancel it here.
                    if let Some(crate::protocol::ResponseBody::Streamed(id)) =
                        outcome.as_ref().map(|outcome| &outcome.body)
                    {
                        self.world.borrow().runtime.services.cancel_stream(*id);
                    }
                    return Ok(());
                };
                // The header list stays as served: sniffing picks the
                // document parser, it never adds a field to a `fetch`
                // Response, and `blob().type` reads the header list
                // (<https://fetch.spec.whatwg.org/#concept-header-extract-mime-type>).
                let callback = Object::new(ctx.clone())?;
                if let Some(outcome) = outcome {
                    callback.set("status", i32::from(outcome.status))?;
                    callback.set("statusText", outcome.status_text)?;
                    // A filtered opaque response has an empty URL list, so
                    // `Response.url` is the empty string; the engine still knows
                    // the URL it fetched
                    // (<https://fetch.spec.whatwg.org/#concept-filtered-response-opaque>).
                    let url = if outcome.response_type.is_opaque() {
                        String::new()
                    } else {
                        outcome.final_url.to_string()
                    };
                    callback.set("url", url)?;
                    callback.set("contentType", outcome.content_type.unwrap_or_default())?;
                    callback.set(
                        "headers",
                        outcome
                            .headers
                            .into_iter()
                            .map(|(name, value)| vec![name, value])
                            .collect::<Vec<_>>(),
                    )?;
                    callback.set("responseType", outcome.response_type.as_str())?;
                    callback.set("redirectCount", outcome.redirect_count)?;
                    // One tagged value carries the body. Making the shim
                    // switch on it keeps "streamed with a decode error"
                    // out of reach, which the flat body/bodyError/stream
                    // triple allowed
                    // (<https://fetch.spec.whatwg.org/#concept-response-body>).
                    let body = Object::new(ctx.clone())?;
                    match outcome.body {
                        crate::protocol::ResponseBody::Bytes(bytes) => {
                            body.set("kind", "bytes")?;
                            body.set("bytes", TypedArray::new(ctx.clone(), bytes)?)?;
                        }
                        crate::protocol::ResponseBody::Undecodable => {
                            body.set("kind", "undecodable")?;
                        }
                        crate::protocol::ResponseBody::Streamed(id) => {
                            body.set("kind", "stream")?;
                            body.set("id", id.get())?;
                        }
                    }
                    callback.set("body", body)?;
                } else {
                    // A failed dial is a network error: the renderer rejects
                    // and `Response.type` never becomes observable
                    // (<https://fetch.spec.whatwg.org/#concept-network-error>).
                    callback.set("status", 0i32)?;
                    callback.set("statusText", "")?;
                    callback.set("url", "")?;
                    callback.set("contentType", "")?;
                    callback.set("headers", Vec::<Vec<String>>::new())?;
                    callback.set("responseType", "error")?;
                    callback.set("redirectCount", 0u32)?;
                    let body = Object::new(ctx.clone())?;
                    body.set("kind", "bytes")?;
                    body.set("bytes", TypedArray::new(ctx.clone(), Vec::<u8>::new())?)?;
                    callback.set("body", body)?;
                }
                if let Err(error) = func.call::<_, ()>((callback,)) {
                    bindings::host::report_callback_error(&ctx, &error);
                }
                Ok(())
            })
        })
    }

    pub(crate) fn fire_load(&self) -> Result<(), JsError> {
        self.with_budget(None, MicrotaskCheckpoint::Perform, || {
            self.context.with(|ctx| {
                bindings::fire_window_load(&ctx)?;
                Ok(())
            })
        })
    }

    /// Rebinds the realm's `document` global to the world's active document
    /// after a same-realm navigation install.
    pub(crate) fn refresh_document(&self) -> Result<(), JsError> {
        self.context.with(|ctx| {
            bindings::refresh_document_global(&ctx)?;
            Ok(())
        })
    }

    /// Restores state without a microtask checkpoint. The caller commits the
    /// traversal before firing `popstate` and performing its checkpoint.
    /// <https://html.spec.whatwg.org/multipage/browsing-the-web.html#update-document-for-history-step-application>
    pub(crate) fn restore_history(
        &self,
        state: Option<&str>,
        length: usize,
    ) -> Result<Persistent<Value<'static>>, JsError> {
        self.with_budget(None, MicrotaskCheckpoint::Defer, || {
            self.context.with(|ctx| {
                let restore: Function = bridge::object(&ctx)?.get("__tbHistoryRestore")?;
                let state: Value = restore.call((state, length))?;
                Ok(Persistent::save(&ctx, state))
            })
        })
    }

    /// Fires the trusted `hashchange` a same-document fragment navigation
    /// owes, after the URL and history entry already changed
    /// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#the-hashchangeevent-interface>).
    pub(crate) fn fire_hash_change(&self, old_url: &str, new_url: &str) -> Result<(), JsError> {
        self.with_budget(None, MicrotaskCheckpoint::Perform, || {
            self.context.with(|ctx| {
                events::fire_trusted_hashchange(&ctx, old_url, new_url)?;
                Ok(())
            })
        })
    }

    pub(crate) fn fire_popstate(&self, state: &Persistent<Value<'static>>) -> Result<(), JsError> {
        self.with_budget(None, MicrotaskCheckpoint::Perform, || {
            self.context.with(|ctx| {
                let state = state.clone().restore(&ctx)?;
                if let Err(error) = events::fire_trusted_popstate(&ctx, state) {
                    bindings::host::report_callback_error(&ctx, &error);
                }
                Ok(())
            })
        })
    }

    /// Fires `DOMContentLoaded` at the document
    /// (<https://html.spec.whatwg.org/multipage/parsing.html#the-end>).
    pub(crate) fn fire_dom_content_loaded(&self) -> Result<(), JsError> {
        self.with_budget(None, MicrotaskCheckpoint::Perform, || {
            self.context.with(|ctx| {
                bindings::fire_dom_content_loaded(&ctx)?;
                Ok(())
            })
        })
    }

    /// Fires `readystatechange` after a document readiness change.
    pub(crate) fn fire_ready_state_change(&self) -> Result<(), JsError> {
        self.with_budget(None, MicrotaskCheckpoint::Perform, || {
            self.context.with(|ctx| {
                bindings::fire_ready_state_change(&ctx)?;
                Ok(())
            })
        })
    }

    pub(crate) fn fire_node_load(&self, id: crate::dom::tree::NodeId) -> Result<(), JsError> {
        self.with_budget(None, MicrotaskCheckpoint::Perform, || {
            self.context.with(|ctx| {
                bindings::fire_node_load(&ctx, id)?;
                Ok(())
            })
        })
    }

    pub(crate) fn fire_node_error(&self, id: crate::dom::tree::NodeId) -> Result<(), JsError> {
        self.with_budget(None, MicrotaskCheckpoint::Perform, || {
            self.context.with(|ctx| {
                bindings::fire_node_error(&ctx, id)?;
                Ok(())
            })
        })
    }

    /// Dispatches one trusted `securitypolicyviolation` event per violation.
    pub(crate) fn fire_csp_violations(
        &self,
        target: crate::dom::tree::NodeId,
        violations: &[crate::csp::Violation],
    ) -> Result<(), JsError> {
        self.with_budget(None, MicrotaskCheckpoint::Perform, || {
            self.context.with(|ctx| {
                bindings::fire_csp_violations(&ctx, target, violations)?;
                Ok(())
            })
        })
    }

    /// Decodes and dispatches one posted window message in this realm.
    ///
    /// Returns `false` when the payload cannot be decoded, which the engine
    /// turns into a `messageerror`
    /// (<https://html.spec.whatwg.org/multipage/web-messaging.html#window-post-message-steps>).
    pub(crate) fn deliver_window_message(
        &self,
        source: u64,
        origin: &str,
        payload: &str,
        ports: &[u64],
    ) -> Result<bool, JsError> {
        self.with_budget(None, MicrotaskCheckpoint::Perform, || {
            let payload = payload.to_owned();
            let origin = origin.to_owned();
            let ports: Vec<f64> = ports.iter().map(|port| js_number(*port)).collect();
            let source = js_number(source);
            self.context.with(|ctx| {
                let deliver: Function = bridge::object(&ctx)?.get("__tbDeliverMessage")?;
                Ok(deliver.call((payload, source, origin, ports))?)
            })
        })
    }

    /// Dispatches `messageerror` for a payload this realm could not decode.
    pub(crate) fn deliver_window_message_error(
        &self,
        source: u64,
        origin: &str,
    ) -> Result<(), JsError> {
        self.with_budget(None, MicrotaskCheckpoint::Perform, || {
            let origin = origin.to_owned();
            let source = js_number(source);
            self.context.with(|ctx| {
                let deliver: Function = bridge::object(&ctx)?.get("__tbDeliverMessageError")?;
                deliver.call::<_, ()>((source, origin))?;
                Ok(())
            })
        })
    }

    /// Fires one `storage` event in this realm; `storageArea` is this realm's
    /// own area, per the spec's broadcast steps
    /// (<https://html.spec.whatwg.org/multipage/webstorage.html#concept-storage-broadcast>).
    pub(crate) fn fire_storage_event(
        &self,
        event: &crate::storage::PendingStorageEvent,
    ) -> Result<(), JsError> {
        let kind = event.kind.as_str();
        let key = event.key.clone();
        let old_value = event.old_value.clone();
        let new_value = event.new_value.clone();
        let url = event.url.clone();
        self.with_budget(None, MicrotaskCheckpoint::Perform, || {
            self.context.with(|ctx| {
                let fire: Function = bridge::object(&ctx)?.get("__tbFireStorageEvent")?;
                fire.call::<_, ()>((kind, key, old_value, new_value, url))?;
                Ok(())
            })
        })
    }

    /// Dispatches one cross-tab `message` event in this realm; the source is
    /// `null` because the posting window lives in another renderer, and
    /// `origin` is the sender's serialized origin at post time.
    /// (<https://html.spec.whatwg.org/multipage/web-messaging.html#window-post-message-steps>)
    pub(crate) fn deliver_remote_message(
        &self,
        origin: &str,
        payload: &str,
    ) -> Result<(), JsError> {
        let origin = origin.to_owned();
        let payload = payload.to_owned();
        self.with_budget(None, MicrotaskCheckpoint::Perform, || {
            self.context.with(|ctx| {
                let deliver: Function = bridge::object(&ctx)?.get("__tbDeliverRemoteMessage")?;
                deliver.call::<_, ()>((origin, payload))?;
                Ok(())
            })
        })
    }

    /// Dispatches one `BroadcastChannel` message in this realm.
    /// (<https://html.spec.whatwg.org/multipage/web-messaging.html#broadcasting-to-other-browsing-contexts>)
    pub(crate) fn deliver_broadcast_message(
        &self,
        origin: &str,
        name: &str,
        payload: &str,
        source: Option<u64>,
    ) -> Result<(), JsError> {
        let origin = origin.to_owned();
        let name = name.to_owned();
        let payload = payload.to_owned();
        let source = source.map(crate::js::js_number);
        self.with_budget(None, MicrotaskCheckpoint::Perform, || {
            self.context.with(|ctx| {
                let deliver: Function = bridge::object(&ctx)?.get("__tbDeliverBroadcast")?;
                deliver.call::<_, ()>((name, payload, origin, source))?;
                Ok(())
            })
        })
    }

    /// Decodes and dispatches one channel message in this realm.
    pub(crate) fn deliver_port_message(
        &self,
        endpoint: u64,
        payload: &str,
        ports: &[u64],
    ) -> Result<bool, JsError> {
        self.with_budget(None, MicrotaskCheckpoint::Perform, || {
            let payload = payload.to_owned();
            let ports: Vec<f64> = ports.iter().map(|port| js_number(*port)).collect();
            let endpoint = js_number(endpoint);
            self.context.with(|ctx| {
                let deliver: Function = bridge::object(&ctx)?.get("__tbDeliverPortMessage")?;
                Ok(deliver.call((endpoint, payload, ports))?)
            })
        })
    }

    /// Dispatches `messageerror` at a port whose payload failed to decode.
    pub(crate) fn deliver_port_message_error(&self, endpoint: u64) -> Result<(), JsError> {
        self.with_budget(None, MicrotaskCheckpoint::Perform, || {
            let endpoint = js_number(endpoint);
            self.context.with(|ctx| {
                let deliver: Function = bridge::object(&ctx)?.get("__tbDeliverPortMessageError")?;
                deliver.call::<_, ()>((endpoint,))?;
                Ok(())
            })
        })
    }

    /// Applies the proxy writes this realm stored for `frame` before its
    /// realm existed.
    pub(crate) fn flush_frame_sets(&self, frame: u64) -> Result<(), JsError> {
        self.with_budget(None, MicrotaskCheckpoint::Perform, || {
            let frame = js_number(frame);
            self.context.with(|ctx| {
                let flush: Function = bridge::object(&ctx)?.get("__tbFlushFrameSets")?;
                flush.call::<_, ()>((frame,))?;
                Ok(())
            })
        })
    }

    /// Fires `close` at one channel endpoint
    /// (<https://html.spec.whatwg.org/multipage/web-messaging.html#disentangle>).
    pub(crate) fn deliver_port_close(&self, endpoint: u64) -> Result<(), JsError> {
        self.with_budget(None, MicrotaskCheckpoint::Perform, || {
            let endpoint = js_number(endpoint);
            self.context.with(|ctx| {
                let deliver: Function = bridge::object(&ctx)?.get("__tbDeliverPortClose")?;
                deliver.call::<_, ()>((endpoint,))?;
                Ok(())
            })
        })
    }

    /// Microtask checkpoint for parser-driven mutations: schedules the
    /// delivery microtask when records are pending and runs the job queue.
    ///
    /// Parser insertions record mutations without entering a JS binding, so
    /// nothing else schedules delivery. Called between parser scripts, where
    /// the spec drains microtasks before the next script runs.
    pub(crate) fn deliver_mutations(&self) -> Result<(), JsError> {
        self.with_budget(None, MicrotaskCheckpoint::Perform, || {
            self.context.with(|ctx| {
                bindings::schedule_mutation_delivery(&ctx)?;
                Ok(())
            })
        })
    }

    /// Runs `operation` under the script deadline and interrupt handler, then
    /// drains the job queue and clears the interrupt handler.
    ///
    /// The job drain always runs so a scheduled microtask cannot outlive the
    /// operation that scheduled it; the operation's error wins over a job
    /// error, and an interrupt wins over both.
    ///
    /// Reentrant calls (script evaluated while an outer script runs) execute
    /// under the outer frame: the outer handler, deadline, and job drain own
    /// the nesting, so an inner frame cannot overwrite and clear the outer
    /// budget.
    fn with_budget<T>(
        &self,
        deadline: Option<Instant>,
        checkpoint: MicrotaskCheckpoint,
        operation: impl FnOnce() -> Result<T, JsError>,
    ) -> Result<T, JsError> {
        if self.budget_depth.get() > 0 {
            return operation();
        }
        self.budget_depth.set(1);
        let outcome = self.with_budget_outer(deadline, checkpoint, operation);
        self.budget_depth.set(0);
        outcome
    }

    fn with_budget_outer<T>(
        &self,
        deadline: Option<Instant>,
        checkpoint: MicrotaskCheckpoint,
        operation: impl FnOnce() -> Result<T, JsError>,
    ) -> Result<T, JsError> {
        let entry = deadline.unwrap_or_else(|| Instant::now() + DEFAULT_SCRIPT_BUDGET);
        let deadline = self
            .turn_deadline
            .get()
            .map_or(entry, |turn| entry.min(turn));
        let interrupted = Rc::new(Cell::new(false));
        let flag = Rc::clone(&interrupted);
        let stop = Arc::clone(&self.stop);
        let terminate = self.terminate.clone();
        self.runtime.set_interrupt_handler(Some(Box::new(move || {
            let should_interrupt = stop.is_set()
                || terminate
                    .as_ref()
                    .is_some_and(|terminate| terminate.load(Ordering::Relaxed))
                || Instant::now() >= deadline;
            flag.set(should_interrupt);
            should_interrupt
        })));
        let _clear = ClearInterrupt {
            runtime: &self.runtime,
        };
        let result = operation();
        // https://html.spec.whatwg.org/multipage/webappapis.html#clean-up-after-running-script
        let jobs = match checkpoint {
            MicrotaskCheckpoint::Perform => {
                let jobs = self.run_jobs();
                self.report_pending_rejections();
                jobs
            }
            MicrotaskCheckpoint::Defer => Ok(()),
        };
        if interrupted.get() {
            Err(JsError::Interrupted)
        } else {
            result.and_then(|value| jobs.map(|()| value))
        }
    }

    /// Fires queued `unhandledrejection` / `rejectionhandled` events for this
    /// realm after a microtask checkpoint.
    fn report_pending_rejections(&self) {
        self.context.with(|ctx| {
            bindings::drain_rejections(&ctx, &self.world);
        });
    }

    fn run_jobs(&self) -> Result<(), JsError> {
        loop {
            match self.runtime.execute_pending_job() {
                Ok(true) => {}
                Ok(false) => return Ok(()),
                Err(err) => return Err(JsError::engine(err)),
            }
        }
    }

    fn install(&self) -> Result<(), JsError> {
        let world = self.world.clone();
        bridge::prepare(&self.runtime, &world)?;
        self.context.with(|ctx| {
            bindings::install(&ctx, &world)?;
            fonts::install(&ctx)?;
            intl::install(&ctx)?;
            self.install_task_host_functions(&ctx, &world)?;
            Self::install_document_host_functions(&ctx, &world)?;
            Self::install_url_host_functions(&ctx, &world)?;
            Self::install_crypto_host_functions(&ctx)?;
            install_storage_host_functions(&ctx, &world)?;
            install_window_host_functions(&ctx, &world)?;
            install_window_messaging_functions(&ctx, &world)?;
            install_history_host_functions(&ctx, &world)?;
            url_parts::install(&ctx)?;
            encoding::install(&ctx)?;
            bindings::install_messaging(&ctx)?;
            reactions::install(&ctx)?;
            bridge::evaluate(&ctx, install_web_apis_js(&ctx)?)?;
            crate::js::canvas::install_serialization(&ctx)?;
            reactions::capture(&ctx)?;
            for name in ["PopStateEvent", "HashChangeEvent"] {
                let constructor: Object = ctx.globals().get(name)?;
                let prototype: Object = constructor.get("prototype")?;
                world
                    .borrow_mut()
                    .intern_brand(name, Persistent::save(&ctx, prototype));
            }
            // The `createEvent` interfaces, captured before page script can
            // replace the globals. Missing entries stay missing: the legacy
            // lookup below reports them as unexposed.
            for name in [
                "BeforeUnloadEvent",
                "CompositionEvent",
                "CustomEvent",
                "DeviceMotionEvent",
                "DeviceOrientationEvent",
                "DragEvent",
                "FocusEvent",
                "HashChangeEvent",
                "KeyboardEvent",
                "MessageEvent",
                "MouseEvent",
                "SecurityPolicyViolationEvent",
                "StorageEvent",
                "TextEvent",
                "TouchEvent",
                "UIEvent",
            ] {
                if let Ok(ctor) = ctx.globals().get::<_, Object>(name) {
                    world
                        .borrow_mut()
                        .event_ctors
                        .insert(name.to_owned(), Persistent::save(&ctx, ctor));
                }
            }
            Ok(())
        })
    }

    /// Installs a dedicated worker's global.
    ///
    /// The worker shares the engine's services and heap, so it keeps the
    /// timer, fetch, encoding, and messaging host functions. It has no
    /// document, history, storage, or Window, so those installers never run
    /// (<https://html.spec.whatwg.org/multipage/workers.html#the-workerglobalscope-common-interface>).
    fn install_worker(&self) -> Result<(), JsError> {
        let world = self.world.clone();
        bridge::prepare(&self.runtime, &world)?;
        self.context.with(|ctx| {
            bindings::install_worker(&ctx, &world)?;
            intl::install(&ctx)?;
            self.install_task_host_functions(&ctx, &world)?;
            Self::install_url_host_functions(&ctx, &world)?;
            Self::install_import_scripts_host(&ctx, &world)?;
            Self::install_crypto_host_functions(&ctx)?;
            install_storage_host_functions(&ctx, &world)?;
            url_parts::install(&ctx)?;
            encoding::install(&ctx)?;
            bindings::install_messaging(&ctx)?;
            // A worker global exposes the canvas interfaces that do not need a
            // document: `OffscreenCanvas`, `ImageData`, `ImageBitmap`, and
            // `createImageBitmap`
            // (<https://html.spec.whatwg.org/multipage/workers.html#apis-available-to-workers>).
            crate::js::canvas::install(&ctx)?;
            bridge::evaluate(&ctx, install_worker_apis_js(&ctx)?)?;
            Ok(())
        })
    }

    /// Fetches an `importScripts` resource as a classic script.
    ///
    /// `importScripts` shares the classic-script fetch, not a page `fetch`:
    /// the response is a JavaScript resource loaded with the worker's origin
    /// as initiator and no CORS negotiation
    /// (<https://html.spec.whatwg.org/multipage/workers.html#dom-workerglobalscope-importscripts>).
    fn install_import_scripts_host(
        ctx: &Ctx<'_>,
        world: &Rc<RefCell<World>>,
    ) -> Result<(), JsError> {
        let import_world = world.clone();
        bridge::object(ctx)?.set(
            "__tbImportScripts",
            Func::from(move |url: String| -> Option<String> {
                let parsed = url::Url::parse(&url).ok()?;
                let body = if parsed.scheme() == "data" {
                    let (content_type, body) = crate::engine::decode_data_url_bounded(&url)?;
                    let essence = content_type
                        .split(';')
                        .next()
                        .unwrap_or(&content_type)
                        .trim();
                    if !javascript_mime_essence(essence) {
                        return None;
                    }
                    body
                } else {
                    if parsed.scheme() != "http" && parsed.scheme() != "https" {
                        return None;
                    }
                    let world = import_world.borrow();
                    let mut dial = DialRequest::new(
                        RequestPolicy::ClassicScript { crossorigin: None },
                        &parsed,
                        &world.origin,
                    );
                    dial.partition_key = world.partition_key();
                    dial.referrer = ReferrerRequest::Client {
                        url: world.referrer_url.clone(),
                    };
                    dial.referrer_policy = world.policy.referrer_policy.unwrap_or_default();
                    dial.timeout_ms = 30_000;
                    let (send, receive) = std::sync::mpsc::sync_channel(1);
                    let cancel = world.runtime.services.start_dial(
                        dial,
                        Box::new(move |outcome| {
                            let _ = send.send(outcome);
                        }),
                    );
                    let Ok(Ok(outcome)) = receive.recv_timeout(Duration::from_secs(30)) else {
                        cancel();
                        return None;
                    };
                    if !(200..300).contains(&outcome.status) {
                        return None;
                    }
                    if let Some(content_type) = &outcome.content_type {
                        let essence = content_type
                            .split(';')
                            .next()
                            .unwrap_or(content_type)
                            .trim();
                        if !javascript_mime_essence(essence) {
                            return None;
                        }
                    }
                    match outcome.body {
                        crate::protocol::ResponseBody::Bytes(body) => body,
                        crate::protocol::ResponseBody::Streamed(_)
                        | crate::protocol::ResponseBody::Undecodable => return None,
                    }
                };
                // A classic worker-imported script is decoded with UTF-8
                // decode, whose replacement mode substitutes U+FFFD and whose
                // BOM removal drops one leading byte-order mark
                // (<https://infra.spec.whatwg.org/#utf-8-decode>).
                let source = String::from_utf8_lossy(&body);
                let source = source.strip_prefix('\u{feff}').unwrap_or(&source);
                Some(source.to_owned())
            }),
        )?;
        // `importScripts` runs the fetched source as a classic script whose
        // base URL and error locations are the imported resource's URL, and
        // rethrows its exception to the caller
        // (<https://html.spec.whatwg.org/multipage/webappapis.html#fetch-a-classic-worker-imported-script>).
        bridge::object(ctx)?.set(
            "__tbEvalImportedScript",
            Func::from(
                |ctx: Ctx<'_>, url: String, source: String| -> rquickjs::Result<()> {
                    let mut options = EvalOptions::default();
                    options.strict = false;
                    options.filename = Some(url);
                    match ctx.eval_with_options::<Value, _>(source.as_str(), options) {
                        Ok(_) => Ok(()),
                        Err(error) => {
                            let caught = if error.is_exception() {
                                ctx.catch()
                            } else {
                                rquickjs::String::from_str(ctx.clone(), &error.to_string())?
                                    .into_value()
                            };
                            Err(ctx.throw(caught))
                        }
                    }
                },
            ),
        )?;
        Ok(())
    }

    fn install_crypto_host_functions(ctx: &Ctx<'_>) -> Result<(), JsError> {
        bridge::object(ctx)?.set("__tbRandomBytes", Func::from(random_bytes))?;
        Ok(())
    }

    /// Timer scheduling and cancellation hooks the JS shim calls.
    fn install_timer_host_functions(
        ctx: &Ctx<'_>,
        outbox: &crate::js::world::Outbox,
    ) -> Result<(), JsError> {
        let schedule_outbox = Rc::clone(outbox);
        bridge::object(ctx)?.set(
            "__scheduleTimeout",
            Func::from(move |js_id: i32, delay: f64| {
                schedule_outbox
                    .borrow_mut()
                    .push_back(RealmRequest::Timer(TimeoutRequest {
                        delay: Duration::from_millis(u64::from(millis(delay))),
                        js_id: PageTimerId(js_id),
                    }));
            }),
        )?;

        let cancel_outbox = Rc::clone(outbox);
        bridge::object(ctx)?.set(
            "__cancelTimeout",
            Func::from(move |js_id: i32| {
                cancel_outbox
                    .borrow_mut()
                    .push_back(RealmRequest::ClearTimer(PageTimerId(js_id)));
            }),
        )?;

        Ok(())
    }

    /// Timer and fetch submission hooks the JS shim calls.
    fn install_task_host_functions(
        &self,
        ctx: &Ctx<'_>,
        world: &Rc<RefCell<World>>,
    ) -> Result<(), JsError> {
        let outbox = Rc::clone(&self.world.borrow().outbox);
        Self::install_timer_host_functions(ctx, &outbox)?;
        install_header_policy_host_functions(ctx)?;
        install_mime_host_functions(ctx)?;
        install_sync_fetch(ctx, world)?;
        install_stream_host_functions(ctx, world)?;
        Self::install_csp_host_functions(ctx, world)?;
        let fetch_outbox = Rc::clone(&outbox);
        let fetch_world = Rc::clone(world);
        bridge::object(ctx)?.set(
            "__queueFetch",
            Func::from(
                move |url: String,
                      js_id: i32,
                      method: String,
                      body: TypedArray<'_, u8>,
                      content_type: Option<String>,
                      headers: Vec<Vec<String>>,
                      options: Object<'_>| {
                    let headers = header_pairs(headers)?;
                    let method = net::Method::parse(&method)
                        .map_err(|_| rquickjs::Error::new_from_js("method", "HTTP method"))?;
                    let client_url = fetch_world.borrow().referrer_url.clone();
                    let options = DialOptions::from_js(&options, &client_url)?;
                    fetch_outbox
                        .borrow_mut()
                        .push_back(RealmRequest::Fetch(Box::new(JsFetchRequest {
                            url,
                            js_id: JsFetchId(js_id),
                            method,
                            body: body.to_vec(),
                            content_type,
                            headers,
                            options,
                        })));
                    Ok::<(), rquickjs::Error>(())
                },
            ),
        )?;
        let cancel_fetch_outbox = Rc::clone(&outbox);
        bridge::object(ctx)?.set(
            "__cancelFetch",
            Func::from(move |js_id: i32| {
                cancel_fetch_outbox
                    .borrow_mut()
                    .push_back(RealmRequest::AbortFetch(JsFetchId(js_id)));
            }),
        )?;
        Ok(())
    }

    /// The CSP checks the shim calls where the Rust choke points cannot run:
    /// string resources for timers, and a synchronous XHR that cannot reach
    /// the dial queue.
    fn install_csp_host_functions(
        ctx: &Ctx<'_>,
        world: &Rc<RefCell<World>>,
    ) -> Result<(), JsError> {
        let eval_world = world.clone();
        let csp_world = world.clone();
        bridge::object(ctx)?.set(
            "__tbCspAllowsEval",
            Func::from(
                move |ctx: Ctx<'_>, source: String| -> rquickjs::Result<bool> {
                    let violations = csp_eval_violations(&ctx, &eval_world, &source)?;
                    Ok(!crate::csp::blocks(&violations))
                },
            ),
        )?;
        let begin_timer = world.clone();
        bridge::object(ctx)?.set(
            "__tbTimerStringBegin",
            Func::from(move || {
                begin_timer.borrow().timer_string.set(true);
            }),
        )?;
        let end_timer = world.clone();
        bridge::object(ctx)?.set(
            "__tbTimerStringEnd",
            Func::from(move || {
                end_timer.borrow().timer_string.set(false);
            }),
        )?;
        // A synchronous XHR cannot reach the dial queue's connect check, so
        // the shim asks this hook instead and the drain dispatches the
        // queued violations
        // (<https://w3c.github.io/webappsec-csp/#should-request-be-blocked-by-content-security-policy>).
        bridge::object(ctx)?.set(
            "__tbCspAllowsConnect",
            Func::from(move |url: String| {
                let Ok(url) = url::Url::parse(&url) else {
                    return false;
                };
                let world = csp_world.borrow();
                let violations = world.policy.csp.check_request(
                    &world.origin,
                    &world.document_url,
                    &url,
                    crate::csp::Destination::Connect,
                    None,
                );
                let blocked = crate::csp::blocks(&violations);
                if blocked {
                    let target = world.with_main_document(|parsed| crate::dom::tree::NodeId {
                        document: parsed.id,
                        node: parsed.document.base.root_node().id,
                    });
                    if let Some(target) = target {
                        world
                            .outbox
                            .borrow_mut()
                            .push_back(RealmRequest::CspViolation(target, violations));
                    }
                }
                !blocked
            }),
        )?;
        Ok(())
    }

    /// Cookie, object URL, and URL resolution hooks the JS shim calls.
    fn install_document_host_functions(
        ctx: &Ctx<'_>,
        world: &Rc<RefCell<World>>,
    ) -> Result<(), JsError> {
        let cookie_get = world.clone();
        let cookie_set = world.clone();

        bridge::object(ctx)?.set(
            "__cookieGet",
            Func::from(move || {
                let (services, url, partition_key) = {
                    let world = cookie_get.borrow();
                    (
                        world.runtime.services.clone(),
                        world.document_url.clone(),
                        world.partition_key(),
                    )
                };
                services.cookies_for(&url, partition_key.as_deref())
            }),
        )?;

        bridge::object(ctx)?.set(
            "__cookieSet",
            Func::from(move |value: String| {
                let (services, url, partition_key) = {
                    let world = cookie_set.borrow();
                    (
                        world.runtime.services.clone(),
                        world.document_url.clone(),
                        world.partition_key(),
                    )
                };
                services.set_cookie(&value, &url, partition_key.as_deref());
            }),
        )?;

        Self::install_cookie_store_host_functions(ctx, world)
    }

    /// The `CookieStore` host functions: the cookie URL, the jar query, and
    /// the typed write the shim builds from the spec's algorithm
    /// (<https://cookiestore.spec.whatwg.org/#cookie-store-interface>).
    fn install_cookie_store_host_functions(
        ctx: &Ctx<'_>,
        world: &Rc<RefCell<World>>,
    ) -> Result<(), JsError> {
        // `cookieStore` resolves against the document's creation URL, so a
        // `pushState` to another path does not move the default path
        // (<https://cookiestore.spec.whatwg.org/#dom-cookiestore-get>).
        let creation_url = world.clone();
        bridge::object(ctx)?.set(
            "__tbCookieStoreCreationUrl",
            Func::from(move || cookie_store_url(&creation_url.borrow()).to_string()),
        )?;

        // The `Domain` attribute may not name a public suffix, which the
        // cookie jar's list decides
        // (<https://cookiestore.spec.whatwg.org/#set-a-cookie>).
        bridge::object(ctx)?.set(
            "__tbCookieStorePublicSuffix",
            Func::from(|domain: String| net::is_public_suffix(&domain)),
        )?;

        // The spec host-parses the `domain` option before comparing it, so an
        // IDN option compares as its punycoded host
        // (<https://cookiestore.spec.whatwg.org/#set-a-cookie> step 12).
        bridge::object(ctx)?.set(
            "__tbCookieStoreParseDomain",
            Func::from(|domain: String| {
                url::Host::parse(&domain).ok().map(|host| host.to_string())
            }),
        )?;

        // A host that is an IP address has no registrable domain suffix
        // (<https://html.spec.whatwg.org/multipage/urls-and-fetching.html#is-a-registrable-domain-suffix-of-or-is-equal-to>).
        bridge::object(ctx)?.set(
            "__tbCookieStoreIsIp",
            Func::from(|host: String| {
                let bare = host
                    .strip_prefix('[')
                    .and_then(|rest| rest.strip_suffix(']'))
                    .unwrap_or(&host);
                bare.parse::<std::net::IpAddr>().is_ok()
            }),
        )?;

        // The `url` option parses against the API base URL, which is the
        // document base URL
        // (<https://html.spec.whatwg.org/multipage/webappapis.html#api-base-url>).
        let api_base = world.clone();
        bridge::object(ctx)?.set(
            "__tbCookieStoreApiBaseUrl",
            Func::from(move || api_base.borrow().main_base_url().to_string()),
        )?;

        let store_get_all = world.clone();
        bridge::object(ctx)?.set(
            "__tbCookieStoreGetAll",
            Func::from(move || {
                let (services, url, partition_key) = {
                    let world = store_get_all.borrow();
                    (
                        world.runtime.services.clone(),
                        cookie_store_url(&world),
                        world.partition_key(),
                    )
                };
                // A cookie name and value cannot contain NUL, LF, `;`, or
                // whitespace, so the pair encoding survives every cookie the
                // jar can hold
                // (<https://datatracker.ietf.org/doc/html/rfc6265#section-4.1.1>).
                let mut encoded = String::new();
                for record in services.cookie_store_records(&url, partition_key.as_deref()) {
                    encoded.push_str(&record.slot);
                    encoded.push('\0');
                    encoded.push_str(&record.name);
                    encoded.push('\0');
                    encoded.push_str(&record.value);
                    encoded.push('\0');
                    encoded.push_str(&record.state);
                    encoded.push('\n');
                }
                encoded
            }),
        )?;

        let store_set = world.clone();
        bridge::object(ctx)?.set(
            "__tbCookieStoreSet",
            Func::from(move |options: Object<'_>| {
                let write = cookie_store_write(&options)?;
                let (services, url, partition_key) = {
                    let world = store_set.borrow();
                    (
                        world.runtime.services.clone(),
                        cookie_store_url(&world),
                        world.partition_key(),
                    )
                };
                services.cookie_store_store(&write, &url, partition_key.as_deref());
                Ok::<(), rquickjs::Error>(())
            }),
        )?;
        Ok(())
    }

    /// URL parsing, object URLs, and the realm's serialized origin.
    ///
    /// Every global has these: a `WorkerGlobalScope` exposes `URL` and
    /// `location`, and its fetch and `importScripts` resolve against the
    /// worker's script URL, so the worker needs the same host functions a
    /// document does
    /// (<https://html.spec.whatwg.org/multipage/workers.html#the-workerglobalscope-common-interface>).
    fn install_url_host_functions(
        ctx: &Ctx<'_>,
        world: &Rc<RefCell<World>>,
    ) -> Result<(), JsError> {
        let object_url_create = world.clone();
        let object_url_revoke = world.clone();
        let object_url_contents = world.clone();
        let object_url_type = world.clone();
        let url_resolve = world.clone();
        let realm_origin = world.clone();

        bridge::object(ctx)?.set(
            "__tbCreateObjectURL",
            Func::from(move |contents: String, content_type: String| {
                object_url_create
                    .borrow_mut()
                    .create_object_url(contents, content_type)
            }),
        )?;

        bridge::object(ctx)?.set(
            "__tbRevokeObjectURL",
            Func::from(move |url: String| {
                object_url_revoke.borrow_mut().revoke_object_url(&url);
            }),
        )?;

        bridge::object(ctx)?.set(
            "__tbObjectUrlContents",
            Func::from(move |url: String| {
                object_url_contents
                    .borrow()
                    .object_url_contents(&url)
                    .map(|contents| contents.to_string())
            }),
        )?;

        bridge::object(ctx)?.set(
            "__tbObjectUrlType",
            Func::from(move |url: String| {
                object_url_type
                    .borrow()
                    .object_url_type(&url)
                    .map(|content_type| content_type.to_string())
            }),
        )?;

        bridge::object(ctx)?.set(
            "__tbParseUrl",
            Func::from(|input: String| url::Url::parse(&input).ok().map(|url| url.to_string())),
        )?;

        bridge::object(ctx)?.set(
            "__tbResolveUrl",
            Func::from(move |input: String, base: Option<String>| {
                let resolved = match base {
                    Some(base) => url::Url::parse(&base)
                        .ok()
                        .and_then(|base| base.join(&input).ok()),
                    None => url_resolve.borrow().resolve_relative(&input),
                };
                resolved.map(|url| url.to_string())
            }),
        )?;

        // The serialized origin of this realm's settings object. A document's
        // URL cannot supply it: `about:blank`, `about:srcdoc`, and `data:`
        // documents carry an inherited tuple origin
        // (<https://html.spec.whatwg.org/multipage/browsers.html#concept-origin>).
        bridge::object(ctx)?.set(
            "__tbOrigin",
            Func::from(move || realm_origin.borrow().origin_string()),
        )?;
        Ok(())
    }
}

/// Validates and records the synchronous history API's URL update, then
/// forwards it to the tab coordinator after the current script finishes.
/// <https://html.spec.whatwg.org/multipage/nav-history-apis.html#shared-history-push/replace-state-steps>
fn install_history_host_functions(
    ctx: &Ctx<'_>,
    world: &Rc<RefCell<World>>,
) -> Result<(), JsError> {
    let snapshot = world.borrow().history.clone();
    bridge::object(ctx)?.set("__tbHistoryLength", snapshot.length.max(1))?;
    bridge::object(ctx)?.set("__tbHistoryState", snapshot.state)?;
    // One host function per history step, so no boolean crosses the JS
    // boundary to say which.
    let update = |handling: crate::protocol::HistoryHandling| {
        let update_world = world.clone();
        move |ctx: Ctx<'_>, url: String, state: String| {
            let next =
                Url::parse(&url).map_err(|_| Exception::throw_type(&ctx, "Invalid history URL"))?;
            let mut world = update_world.borrow_mut();
            if !world.url_matches_origin(&next) {
                return Err(Exception::throw_type(&ctx, "Cross-origin history URL"));
            }
            world.document_url = next;
            // https://html.spec.whatwg.org/multipage/browsing-the-web.html#url-and-history-update-steps
            world.history.apply(state.clone(), handling);
            world
                .outbox
                .borrow_mut()
                .push_back(RealmRequest::HistoryPush {
                    url,
                    state,
                    handling,
                });
            Ok::<usize, rquickjs::Error>(world.history.length)
        }
    };
    bridge::object(ctx)?.set(
        "__tbHistoryPush",
        Func::from(update(crate::protocol::HistoryHandling::Push)),
    )?;
    bridge::object(ctx)?.set(
        "__tbHistoryReplace",
        Func::from(update(crate::protocol::HistoryHandling::Replace)),
    )?;
    let traverse_world = world.clone();
    bridge::object(ctx)?.set(
        "__tbHistoryTraverse",
        Func::from(move |delta: i32| {
            traverse_world
                .borrow()
                .outbox
                .borrow_mut()
                .push_back(RealmRequest::HistoryTraverse { delta });
        }),
    )?;
    Ok(())
}

/// Produces the cryptographically strong bytes required by
/// [`Crypto.getRandomValues`](https://w3c.github.io/webcrypto/#Crypto-method-getRandomValues).
fn random_bytes(ctx: Ctx<'_>, length: usize) -> rquickjs::Result<TypedArray<'_, u8>> {
    if length > 65_536 {
        return Err(Exception::throw_range(&ctx, "random byte quota exceeded"));
    }
    let mut bytes = vec![0; length];
    getrandom::fill(&mut bytes)
        .map_err(|error| Exception::throw_internal(&ctx, &error.to_string()))?;
    TypedArray::new(ctx, bytes)
}

/// `window.open`/`window.close` hooks the JS shim calls.
fn install_window_host_functions(ctx: &Ctx<'_>, world: &Rc<RefCell<World>>) -> Result<(), JsError> {
    // The window open steps are one algorithm, so the engine runs them:
    // `__tbOpen` encodes the URL once, reads the features once, decides the
    // opener policy (including the blob rule, which needs the top-level
    // origin), and picks the context. The shim only turns the answer into a
    // `WindowProxy`
    // (<https://html.spec.whatwg.org/multipage/nav-history-apis.html#window-open-steps>).
    let window_open = world.clone();
    bridge::object(ctx)?.set(
        "__tbOpen",
        Func::from(move |url: String, name: String, features: String| {
            open_window(&window_open, &url, &name, &features)
        }),
    )?;

    let window_close = world.clone();
    bridge::object(ctx)?.set(
        "__tbWindowClose",
        Func::from(move |window: u64| {
            window_close
                .borrow()
                .runtime
                .services
                .window_close(crate::protocol::WindowId::new(window));
        }),
    )?;

    let opener = world.clone();
    bridge::object(ctx)?.set(
        "__tbWindowOpener",
        Func::from(move || {
            opener
                .borrow()
                .runtime
                .services
                .window_opener()
                .map(|window| crate::js::js_number(window.get()))
        }),
    )?;

    install_auxiliary_host_functions(ctx, world)?;
    Ok(())
}

/// The hooks a `WindowProxy` uses to exchange messages: a cross-tab
/// `postMessage`, a remote same-origin `sessionStorage` read, and a
/// `BroadcastChannel` post.
fn install_window_messaging_functions(
    ctx: &Ctx<'_>,
    world: &Rc<RefCell<World>>,
) -> Result<(), JsError> {
    let post_message = world.clone();
    bridge::object(ctx)?.set(
        "__tbWindowPostMessage",
        Func::from(
            move |ctx: Ctx<'_>, window: u64, target_origin: String, payload: String| {
                let world = post_message.borrow();
                // The sender resolves the target, because `/` names the
                // sender's own origin identity, which no serialized value can
                // carry
                // (<https://html.spec.whatwg.org/multipage/web-messaging.html#window-post-message-steps>).
                let Some(target) =
                    crate::protocol::TargetOrigin::parse(&target_origin, Some(&world.origin))
                else {
                    return Err(crate::js::bindings::throw_dom(
                        &ctx,
                        "SyntaxError",
                        "Invalid target origin",
                    ));
                };
                if matches!(
                    &target,
                    crate::protocol::TargetOrigin::Exact(url::Origin::Opaque(_))
                ) {
                    // An opaque exact target is a different origin object in
                    // every document, so a cross-tab post with one can never
                    // be delivered. It is still a valid argument, and the
                    // shim already serialized the message.
                    return Ok(());
                }
                world.runtime.services.window_post_message(
                    crate::protocol::WindowId::new(window),
                    crate::protocol::PostedMessage {
                        origin: crate::protocol::SerializedOrigin::from(&world.origin),
                        target_origin: target,
                        payload,
                    },
                );
                Ok(())
            },
        ),
    )?;

    let remote_session = world.clone();
    bridge::object(ctx)?.set(
        "__tbRemoteSessionGet",
        Func::from(move |window: u64, key: String| {
            let world = remote_session.borrow();
            world.storage_origin().and_then(|origin| {
                world.runtime.services.remote_session_get(
                    crate::protocol::WindowId::new(window),
                    &origin,
                    &key,
                )
            })
        }),
    )?;
    Ok(())
}

/// What one `window.open` call selected.
///
/// The context is a tagged union because a `FrameId` and a `WindowId` are
/// different kinds of identity: this renderer hosts the first and the browser
/// hosts the second.
///
/// `noopener` still selects and navigates a context; it only withholds the
/// `WindowProxy`, which is [`withholds`](Self::withholds)
/// (<https://html.spec.whatwg.org/multipage/nav-history-apis.html#window-open-steps>).
struct WindowOpenAnswer {
    context: OpenedContext,
    withholds: bool,
}

/// The context one `window.open` call selected.
enum OpenedContext {
    /// A context this renderer hosts: an auxiliary one it created, or the
    /// navigable a name selected.
    Frame(FrameId),
    /// A browser tab.
    Tab(WindowId),
}

impl<'js> rquickjs::IntoJs<'js> for WindowOpenAnswer {
    fn into_js(self, ctx: &Ctx<'js>) -> rquickjs::Result<Value<'js>> {
        let (kind, id) = match self.context {
            OpenedContext::Frame(frame) => ("frame", frame.get()),
            OpenedContext::Tab(window) => ("tab", window.get()),
        };
        let opened = Object::new(ctx.clone())?;
        opened.set("kind", kind)?;
        opened.set("id", js_number(id))?;
        opened.set("withholds", self.withholds)?;
        Ok(opened.into_value())
    }
}

/// One `window.open` call
/// (<https://html.spec.whatwg.org/multipage/nav-history-apis.html#window-open-steps>).
///
/// `None` when the call opens nothing.
fn open_window(
    world: &Rc<RefCell<World>>,
    url: &str,
    name: &str,
    features: &str,
) -> Option<WindowOpenAnswer> {
    let target = TargetName::for_window_open(name);
    let plan = world.borrow().plan_window_open(url, &target, features)?;
    let services = world.borrow().runtime.services.clone();
    // The rules for choosing a navigable search this tab's tree, then the
    // browsing context group's browser tabs
    // (<https://html.spec.whatwg.org/multipage/document-sequences.html#the-rules-for-choosing-a-navigable>).
    let selected = {
        let world = world.borrow();
        world
            .runtime
            .shared
            .borrow()
            .tree
            .choose_navigable(&target, world.frame())
    };
    if let Some(frame) = selected {
        World::navigate_selected(world, frame, plan.url.as_ref(), plan.opener);
        return Some(WindowOpenAnswer {
            context: OpenedContext::Frame(frame),
            withholds: plan.withholds,
        });
    }
    if let TargetName::Named(name) = &target
        && let Some(tab) =
            services.navigate_named(name, plan.url.as_ref(), plan.opener, &plan.source)
    {
        return Some(WindowOpenAnswer {
            context: OpenedContext::Tab(tab),
            withholds: plan.withholds,
        });
    }
    // Nothing selected a context, so the call creates one. A sandboxed
    // document without `allow-popups` cannot create an auxiliary context,
    // whether this renderer or the browser would host it; spelling the name
    // `_BLANK` or choosing an unknown `_x` must not change that
    // (<https://html.spec.whatwg.org/multipage/browsers.html#sandboxed-auxiliary-navigation-browsing-context-flag>).
    // The browser checks the same flag against its own copy of the tab's set;
    // this early return is the honest renderer behaving, not the check.
    if !world.borrow().allows_auxiliary_navigation() {
        return None;
    }
    // A blob URL names bytes in this renderer's object-URL store, which a
    // browser tab cannot resolve, so a blob popup stays in this renderer. Every
    // other URL is a real browser tab, so it gets its own target and webdriver
    // handle
    // (<https://html.spec.whatwg.org/multipage/window-object.html#dom-open>).
    if plan.url.as_ref().is_some_and(|url| url.scheme() == "blob")
        && let Some(frame) =
            World::create_auxiliary(world, plan.url.as_ref(), &target, plan.opener, false)
    {
        return Some(WindowOpenAnswer {
            context: OpenedContext::Frame(frame),
            withholds: plan.withholds,
        });
    }
    // A host with no tab carrier cannot open the window itself, so the
    // renderer hosts it. The document may be cross-origin; frames already
    // cross that boundary through `WindowProxy`.
    if !services.opens_window_as_tab()
        && let Some(frame) =
            World::create_auxiliary(world, plan.url.as_ref(), &target, plan.opener, true)
    {
        return Some(WindowOpenAnswer {
            context: OpenedContext::Frame(frame),
            withholds: plan.withholds,
        });
    }
    let tab = services.window_open(plan.url.as_ref(), &target, plan.opener, &plan.source)?;
    Some(WindowOpenAnswer {
        context: OpenedContext::Tab(tab),
        withholds: plan.withholds,
    })
}

/// `window.open`'s auxiliary-context hooks: reading and closing the opener.
fn install_auxiliary_host_functions(
    ctx: &Ctx<'_>,
    world: &Rc<RefCell<World>>,
) -> Result<(), JsError> {
    let opener = world.clone();
    bridge::object(ctx)?.set(
        "__tbWindowOpenerFrame",
        Func::from(move || {
            let world = opener.borrow();
            let frame = world.frame();
            let shared = world.runtime.shared.borrow();
            shared
                .tree
                .opener(frame)
                .filter(|opener| shared.tree.is_alive(*opener))
                .map(|opener| crate::js::js_number(opener.get()))
        }),
    )?;

    let close = world.clone();
    bridge::object(ctx)?.set(
        "__tbCloseWindow",
        Func::from(move || {
            let world = close.borrow();
            // A browser tab closes through the same `window_close` the opener's
            // `WindowProxy.close()` uses, so either side reaches one path
            // (<https://html.spec.whatwg.org/multipage/window-object.html#dom-window-close>).
            if world.frame() == FrameId::MAIN
                && let Some(window) = world.own_window
            {
                world.runtime.services.window_close(window);
                return;
            }
            // A renderer-hosted blob popup is not a tab, so its frame is the
            // context to close. An iframe has no opener and is left alone.
            world
                .runtime
                .shared
                .borrow_mut()
                .close_auxiliary(world.frame());
        }),
    )?;
    Ok(())
}

/// `localStorage`/`sessionStorage` hooks the JS shim calls. The session area
/// lives in the engine; the local area crosses the service seam.
fn install_storage_host_functions(
    ctx: &Ctx<'_>,
    world: &Rc<RefCell<World>>,
) -> Result<(), JsError> {
    let origin = world.clone();
    bridge::object(ctx)?.set(
        "__tbStorageOrigin",
        Func::from(move || origin.borrow().storage_origin().map(|key| key.to_string())),
    )?;

    let get = world.clone();
    bridge::object(ctx)?.set(
        "__tbStorageGet",
        Func::from(
            move |ctx: Ctx<'_>, kind: String, key: String| -> rquickjs::Result<Option<String>> {
                let kind = storage_area(&ctx, &kind)?;
                Ok(get.borrow().storage_get(kind, &key))
            },
        ),
    )?;

    let keys = world.clone();
    bridge::object(ctx)?.set(
        "__tbStorageKeys",
        Func::from(
            move |ctx: Ctx<'_>, kind: String| -> rquickjs::Result<Vec<String>> {
                let kind = storage_area(&ctx, &kind)?;
                Ok(keys.borrow().storage_keys(kind))
            },
        ),
    )?;

    let set = world.clone();
    bridge::object(ctx)?.set(
        "__tbStorageSet",
        Func::from(
            move |ctx: Ctx<'_>,
                  kind: String,
                  key: String,
                  value: String|
                  -> rquickjs::Result<bool> {
                let kind = storage_area(&ctx, &kind)?;
                Ok(set.borrow().storage_set(kind, &key, &value).is_ok())
            },
        ),
    )?;

    let remove = world.clone();
    bridge::object(ctx)?.set(
        "__tbStorageRemove",
        Func::from(
            move |ctx: Ctx<'_>, kind: String, key: String| -> rquickjs::Result<bool> {
                let kind = storage_area(&ctx, &kind)?;
                Ok(remove.borrow().storage_remove(kind, &key).is_some())
            },
        ),
    )?;

    let clear = world.clone();
    bridge::object(ctx)?.set(
        "__tbStorageClear",
        Func::from(
            move |ctx: Ctx<'_>, kind: String| -> rquickjs::Result<bool> {
                let kind = storage_area(&ctx, &kind)?;
                Ok(clear.borrow().storage_clear(kind).is_some())
            },
        ),
    )?;
    Ok(())
}

/// The storage area the shim names: `"local"` or `"session"`. The shim is the
/// only caller, so an unknown name is an engine bug: it throws instead of
/// silently selecting `localStorage`.
fn storage_area(ctx: &Ctx<'_>, kind: &str) -> rquickjs::Result<crate::protocol::StorageKind> {
    crate::protocol::StorageKind::parse(kind)
        .ok_or_else(|| rquickjs::Exception::throw_type(ctx, "unknown storage area"))
}

impl Drop for JsRealm {
    fn drop(&mut self) {
        bindings::forget_rejections(&self.world);
        bindings::forget_world(&self.context);
        self.context.with(|ctx| bridge::release(&ctx, &self.world));
        let world = self.world.clone();
        let mut world = world.borrow_mut();
        // Release this realm's cached wrappers and document associations
        // before its QuickJS context goes away; sibling realms keep theirs.
        world.forget_owned_documents();
        world.clear_listeners();
        world.release_host_primitives();
    }
}

pub(crate) fn script_at(world: &World, id: crate::dom::tree::NodeId) -> Option<Script> {
    let parsed = world.document(id)?;
    let base = &parsed.document.base;
    let node = base.get_node(id.node)?;
    let element = node.data.downcast_element()?;
    let name = &element.name;
    if name.ns != crate::dom::tree::html_namespace()
        || !name.local.as_str().eq_ignore_ascii_case("script")
    {
        return None;
    }
    let source = match crate::dom::tree::attr(base, id.node, "src") {
        Some(src) if !src.trim().is_empty() => ScriptSource::Src(src.to_owned()),
        _ => ScriptSource::Inline {
            source: crate::dom::text::child_text_content(base, id.node),
            // Known gap: Blitz stores no script line numbers.
            line: 0,
        },
    };
    // The script block's type string: the `type` attribute when present,
    // otherwise `text/` plus the `language` attribute, otherwise
    // `text/javascript`; an exactly empty `type` or `language` also means
    // `text/javascript`
    // (<https://html.spec.whatwg.org/multipage/scripting.html#prepare-the-script-element>).
    let typ = crate::dom::tree::attr(base, id.node, "type").map(str::trim_ascii);
    let language = crate::dom::tree::attr(base, id.node, "language");
    let type_string = match (typ, language) {
        (Some(""), _) | (None, Some("") | None) => "text/javascript".to_owned(),
        (Some(typ), _) => typ.to_owned(),
        (None, Some(language)) => format!("text/{language}"),
    };
    if type_string.eq_ignore_ascii_case("module") {
        Some(Script::Module(source))
    } else if javascript_mime_essence(&type_string) {
        Some(Script::Classic(source))
    } else {
        None
    }
}

pub(super) fn javascript_module_mime(typ: Option<&str>) -> bool {
    // A response header is a MIME type, so parameters and whitespace are
    // allowed and the first parsable item wins
    // (<https://fetch.spec.whatwg.org/#concept-header-extract-mime-type>).
    let Some(mime) = typ.and_then(crate::sniff::extract_mime_type) else {
        return false;
    };
    let essence = mime.split(';').next().unwrap_or(&mime);
    javascript_mime_essence(essence)
}

/// Classic scripts additionally accept `text/html` and `text/plain`
/// (<https://html.spec.whatwg.org/multipage/scripting.html#prepare-the-script-element>).
pub(crate) fn classic_script_mime(typ: Option<&str>) -> bool {
    let Some(typ) = typ.map(str::trim).filter(|typ| !typ.is_empty()) else {
        return true;
    };
    let essence = typ
        .split(';')
        .next()
        .unwrap_or(typ)
        .trim()
        .to_ascii_lowercase();
    essence == "text/html" || essence == "text/plain" || javascript_mime_essence(&essence)
}

/// Whether `typ` is exactly a JavaScript MIME type essence string, with no
/// parameters and no surrounding whitespace
/// (<https://html.spec.whatwg.org/multipage/scripting.html#javascript-mime-type-essence-match>).
pub(crate) fn javascript_mime_essence(typ: &str) -> bool {
    // https://mimesniff.spec.whatwg.org/#javascript-mime-type
    [
        "application/ecmascript",
        "application/javascript",
        "application/x-ecmascript",
        "application/x-javascript",
        "text/ecmascript",
        "text/javascript",
        "text/javascript1.0",
        "text/javascript1.1",
        "text/javascript1.2",
        "text/javascript1.3",
        "text/javascript1.4",
        "text/javascript1.5",
        "text/jscript",
        "text/livescript",
        "text/x-ecmascript",
        "text/x-javascript",
    ]
    .iter()
    .any(|essence| typ.eq_ignore_ascii_case(essence))
}

/// Owns [`JsRealm::turn_deadline`] for one renderer turn. Nested enters keep
/// the outer Instant; only the owner clears it.
#[must_use]
pub(crate) struct ScriptTurn {
    slot: Rc<Cell<Option<Instant>>>,
    owned: bool,
}

impl ScriptTurn {
    fn enter(slot: &Rc<Cell<Option<Instant>>>) -> Self {
        Self::enter_at(slot, Instant::now() + TURN_SCRIPT_BUDGET)
    }

    pub(crate) fn enter_at(slot: &Rc<Cell<Option<Instant>>>, deadline: Instant) -> Self {
        let owned = slot.get().is_none();
        if owned {
            slot.set(Some(deadline));
        }
        Self {
            slot: Rc::clone(slot),
            owned,
        }
    }
}

impl Drop for ScriptTurn {
    fn drop(&mut self) {
        if self.owned {
            self.slot.set(None);
        }
    }
}

struct ClearInterrupt<'a> {
    runtime: &'a Runtime,
}

impl Drop for ClearInterrupt<'_> {
    fn drop(&mut self) {
        self.runtime.set_interrupt_handler(None);
    }
}

/// Array `__tbSyncFetch` returns to the XHR shim: status, body, final URL,
/// and the response header list.
type SyncFetchList = rquickjs::prelude::List<(i32, Vec<u8>, String, Vec<Vec<String>>)>;

struct SyncPageFetch {
    url: String,
    method: net::Method,
    body: Vec<u8>,
    content_type: Option<String>,
    headers: Vec<(String, String)>,
    referrer: crate::protocol::ReferrerRequest,
    with_credentials: bool,
    timeout: f64,
}

/// `__tbSyncFetch`: one page request that finishes before `send` returns.
/// MIME host calls. One Rust parser serves serialization, multipart boundaries,
/// and `blob().type`, so JS never keeps a second MIME grammar.
fn install_mime_host_functions(ctx: &Ctx<'_>) -> Result<(), JsError> {
    // Parse a MIME type and serialize it back; `XMLHttpRequest`'s
    // `overrideMimeType` stores the result, `""` on failure
    // (<https://mimesniff.spec.whatwg.org/#parse-a-mime-type>).
    bridge::object(ctx)?.set(
        "__tbParseMimeType",
        Func::from(|value: String| -> String { crate::sniff::parse_serialized_mime_type(&value) }),
    )?;
    bridge::object(ctx)?.set(
        "__tbMimeTypeParts",
        Func::from(|value: String| -> Vec<String> {
            crate::sniff::mime_type_parts(&value)
                .map_or_else(Vec::new, |(essence, boundary)| vec![essence, boundary])
        }),
    )?;
    // "extract a MIME type" over one combined `Content-Type` value, serialized,
    // `""` on failure. The `blob()` methods and the XHR blob response read it
    // (<https://fetch.spec.whatwg.org/#concept-header-extract-mime-type>).
    bridge::object(ctx)?.set(
        "__tbExtractMimeType",
        Func::from(|value: String| -> String {
            crate::sniff::extract_mime_type(&value).unwrap_or_default()
        }),
    )?;
    Ok(())
}

/// Header and method policy predicates. `net::cors` is the only table, so the
/// preflight decision and the JS `Headers` guard cannot drift.
fn install_header_policy_host_functions(ctx: &Ctx<'_>) -> Result<(), JsError> {
    bridge::object(ctx)?.set(
        "__tbIsForbiddenRequestHeader",
        Func::from(|name: String, value: String| {
            net::isomorphic_encode(&value)
                .is_some_and(|value| net::cors::is_forbidden_request_header(&name, &value))
        }),
    )?;
    bridge::object(ctx)?.set(
        "__tbIsNoCorsSafelistedHeader",
        Func::from(|name: String, value: String| {
            net::isomorphic_encode(&value)
                .is_some_and(|value| net::cors::is_no_cors_safelisted_request_header(&name, &value))
        }),
    )?;
    bridge::object(ctx)?.set(
        "__tbIsCorsSafelistedMethod",
        Func::from(|method: String| {
            net::Method::parse(&method)
                .is_ok_and(|method| net::cors::is_cors_safelisted_method(&method))
        }),
    )?;
    Ok(())
}

fn install_stream_host_functions(ctx: &Ctx<'_>, world: &Rc<RefCell<World>>) -> Result<(), JsError> {
    let read_world = world.clone();
    bridge::object(ctx)?.set(
        "__tbFetchRead",
        Func::from(move |stream: u64| -> rquickjs::Result<Option<Vec<u8>>> {
            read_world
                .borrow()
                .runtime
                .services
                .dial_read(crate::protocol::StreamId::from_page(stream))
                .map_err(|_| rquickjs::Error::new_from_js("dial", "network error"))
        }),
    )?;
    let cancel_world = world.clone();
    bridge::object(ctx)?.set(
        "__tbFetchCancel",
        Func::from(move |stream: u64| {
            cancel_world
                .borrow()
                .runtime
                .services
                .cancel_stream(crate::protocol::StreamId::from_page(stream));
        }),
    )?;
    Ok(())
}

fn install_sync_fetch(ctx: &Ctx<'_>, world: &Rc<RefCell<World>>) -> Result<(), JsError> {
    let sync_world = world.clone();
    bridge::object(ctx)?.set(
        "__tbSyncFetch",
        Func::from(
            move |url: String,
                  method: String,
                  body: TypedArray<'_, u8>,
                  content_type: Option<String>,
                  headers: Vec<Vec<String>>,
                  options: Object<'_>,
                  timeout_ms: f64| {
                let headers = header_pairs(headers)?;
                let method = net::Method::parse(&method)
                    .map_err(|_| rquickjs::Error::new_from_js("method", "HTTP method"))?;
                // `{ referrer, withCredentials }`, named like `__queueFetch`'s
                // options
                // (<https://fetch.spec.whatwg.org/#concept-request-referrer>,
                // <https://xhr.spec.whatwg.org/#the-withcredentials-attribute>).
                let client_url = sync_world.borrow().referrer_url.clone();
                let referrer = match options.get::<_, Option<String>>("referrer")? {
                    Some(value) => crate::protocol::ReferrerRequest::from_idl(&value, &client_url)
                        .ok_or_else(|| {
                            Exception::throw_type(
                                options.ctx(),
                                &format!("referrer: {value:?} is not a referrer"),
                            )
                        })?,
                    None => crate::protocol::ReferrerRequest::default(),
                };
                let with_credentials = options
                    .get::<_, Option<bool>>("withCredentials")?
                    .unwrap_or(false);
                Ok::<_, rquickjs::Error>(sync_page_fetch(
                    &sync_world.borrow(),
                    SyncPageFetch {
                        url,
                        method,
                        body: body.to_vec(),
                        content_type,
                        headers,
                        referrer,
                        with_credentials,
                        timeout: timeout_ms,
                    },
                ))
            },
        ),
    )?;
    Ok(())
}

fn header_pairs(headers: Vec<Vec<String>>) -> rquickjs::Result<Vec<(String, String)>> {
    headers
        .into_iter()
        .map(|field| match field.as_slice() {
            [name, value] => Ok((name.clone(), value.clone())),
            _ => Err(rquickjs::Error::new_from_js("array", "header pair")),
        })
        .collect()
}

/// One synchronous page request. `None` is a network error, which includes
/// the carrier deadline: a synchronous request has no `timeout` attribute to
/// report a timeout event for
/// (<https://xhr.spec.whatwg.org/#the-timeout-attribute>). `data:` is
/// decoded here, the same way an async dial short-circuits before the
/// carrier.
fn sync_page_fetch(world: &World, request: SyncPageFetch) -> Option<SyncFetchList> {
    if request.url.starts_with("data:") {
        let (content_type, body) = crate::engine::decode_data_url_bounded(&request.url)?;
        // The data: URL processor yields a MIME type for every URL, and the
        // response header list carries it
        // (<https://fetch.spec.whatwg.org/#data-url-processor>).
        let headers = vec![vec!["content-type".to_owned(), content_type]];
        return Some(sync_fetch_value(200, body, request.url, headers));
    }
    let Ok(parsed) = url::Url::parse(&request.url) else {
        return None;
    };
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return None;
    }
    let mut dial = crate::protocol::DialRequest::new(
        crate::protocol::RequestPolicy::Fetch(crate::protocol::CorsRequest {
            mode: crate::protocol::CorsMode::Cors,
            // `withCredentials` selects `include`, else `same-origin`
            // (<https://xhr.spec.whatwg.org/#the-withcredentials-attribute>).
            credentials: if request.with_credentials {
                net::cors::CredentialsMode::Include
            } else {
                net::cors::CredentialsMode::SameOrigin
            },
            redirect: net::cors::RedirectMode::Follow,
        }),
        &parsed,
        &world.origin,
    );
    dial.partition_key = world.partition_key();
    dial.method = request.method;
    dial.body = request.body;
    dial.content_type = request.content_type;
    dial.headers = request.headers;
    dial.referrer = request.referrer;
    dial.timeout_ms = u64::from(millis(request.timeout));
    dial.referrer_policy = world.policy.referrer_policy.unwrap_or_default();
    // Synchronous XHR owns the renderer thread until the response arrives;
    // the blocking seam is the one service implementations provide for it.
    let outcome = world.runtime.services.dial_sync(dial);
    match outcome {
        // The header list stays as served; nothing is sniffed into it
        // (<https://fetch.spec.whatwg.org/#concept-response-header-list>).
        Ok(outcome) => {
            // Page script reads the filtered view: an opaque response's URL
            // list is empty
            // (<https://fetch.spec.whatwg.org/#concept-filtered-response-opaque>).
            let url = if outcome.response_type.is_opaque() {
                String::new()
            } else {
                outcome.final_url.to_string()
            };
            let crate::protocol::ResponseBody::Bytes(body) = outcome.body else {
                return None;
            };
            Some(sync_fetch_value(
                i32::from(outcome.status),
                body,
                url,
                outcome
                    .headers
                    .into_iter()
                    .map(|(name, value)| vec![name, value])
                    .collect(),
            ))
        }
        Err(_) => None,
    }
}

/// One synchronous fetch result as the array `__tbSyncFetch` returns.
fn sync_fetch_value(
    status: i32,
    body: Vec<u8>,
    url: String,
    headers: Vec<Vec<String>>,
) -> SyncFetchList {
    rquickjs::prelude::List((status, body, url, headers))
}

/// The violations compiling `source` produces against the document's policies,
/// with the calling script's location captured while the compile call is on
/// the stack and the `securitypolicyviolation` events fired
/// (<https://w3c.github.io/webappsec-csp/#can-compile-strings>,
/// <https://w3c.github.io/webappsec-csp/#create-violation-for-global>).
///
/// Shared by the engine's string-compilation hook and the `setTimeout(string)`
/// host surface.
fn csp_eval_violations(
    ctx: &Ctx<'_>,
    world: &Rc<RefCell<World>>,
    source: &str,
) -> rquickjs::Result<Vec<crate::csp::Violation>> {
    let caller = error_stack_top(ctx);
    let mut violations = {
        let world = world.borrow();
        world.policy.csp.check_inline(
            &world.document_url,
            &crate::csp::InlineScript {
                source,
                nonce: "",
                nonceable: false,
                parser_inserted: false,
                kind: crate::csp::InlineKind::Evaluation,
            },
        )
    };
    if let Some((url, line, column)) = caller {
        for violation in &mut violations {
            violation.source_file = Some(url.clone());
            violation.line_number = line;
            violation.column_number = column;
        }
    }
    if !violations.is_empty() {
        let target = world
            .borrow()
            .with_main_document(|parsed| crate::dom::tree::NodeId {
                document: parsed.id,
                node: parsed.document.base.root_node().id,
            });
        if let Some(target) = target {
            bindings::fire_csp_violations(ctx, target, &violations)?;
        }
    }
    Ok(violations)
}

/// The message the engine ends a refused string compilation with, naming the
/// directive that blocked it rather than a fixed policy.
fn eval_denied_message(violations: &[crate::csp::Violation]) -> String {
    let directive = violations
        .iter()
        .find(|violation| violation.blocks())
        .map_or("script-src", |violation| {
            violation.effective_directive.as_str()
        });
    format!(
        "Refused to evaluate a string as JavaScript because 'unsafe-eval' is not an allowed \
         source of script in the following Content Security Policy directive: \"{directive}\""
    )
}

/// Raises the `EvalError` the engine propagates instead of compiling a refused
/// string (<https://tc39.es/ecma262/#sec-eval-x>).
///
/// The constructor is the pristine one captured at install: a page that
/// replaces the `EvalError` global must neither run code inside the bindings
/// nor substitute another thrown value. With no captured constructor (outside
/// a realm), fall back to an internal error carrying the same message.
fn throw_eval_error(ctx: &Ctx<'_>, message: &str) -> rquickjs::Error {
    let constructor = bindings::try_world(ctx)
        .and_then(|world| world.borrow().pristine_eval_error.clone())
        .and_then(|saved| saved.restore(ctx).ok());
    let Some(constructor) = constructor else {
        return Exception::throw_internal(ctx, message);
    };
    match constructor.call::<_, Object>((message,)) {
        Ok(error) => ctx.throw(error.into_value()),
        Err(error) => error,
    }
}

/// The executing script's URL, line, and column: the innermost script frame of
/// the interpreter's stack. Subresource triggers (an element insertion, a `src`
/// set) capture this so a later CSP violation for the load can report
/// `sourceFile`, `lineNumber`, and `columnNumber`
/// (<https://w3c.github.io/webappsec-csp/#create-violation-for-global>).
///
/// The frame is read from the interpreter directly, so a page that rewrites
/// `Error.stackTraceLimit` or `Error.prepareStackTrace` cannot spoof it, no
/// page code runs during the capture, and nothing is allocated for a stack
/// string. Frames of the engine's own bundle are skipped, as are frames that
/// name no script; `None` means no page script is running (a parser insertion).
pub(crate) fn error_stack_top(ctx: &Ctx<'_>) -> Option<(String, u32, u32)> {
    let mut level = 0;
    loop {
        let frame = ctx.stack_frame(level)?;
        level += 1;
        if frame.native {
            continue;
        }
        let Some(url) = frame.filename.and_then(|atom| atom.to_string().ok()) else {
            continue;
        };
        if url == crate::js::bridge::ENGINE_FILENAME {
            continue;
        }
        return Some((url, frame.line, frame.column));
    }
}

fn eval_classic<'js, V: FromJs<'js>>(ctx: &rquickjs::Ctx<'js>, source: &str) -> Result<V, JsError> {
    let mut options = EvalOptions::default();
    options.strict = false;
    ctx.eval_with_options(source, options)
        .map_err(|error| match error {
            rquickjs::Error::Exception => JsError::engine(
                exception_text(ctx.catch()).unwrap_or_else(|| "uncaught exception".into()),
            ),
            other => JsError::engine(other),
        })
}

/// A caught exception's message and stack, the diagnostic a host script's
/// failure or a compile error reports.
fn exception_text(caught: Value<'_>) -> Option<String> {
    let exception = caught.into_exception()?;
    Some(format!(
        "{} | {}",
        exception.message().unwrap_or_default(),
        exception.stack().unwrap_or_default()
    ))
}

/// Converts a host script's engine error, keeping the thrown value's message
/// instead of `rquickjs`'s generic `Exception generated by QuickJS`.
fn caught_error(ctx: &rquickjs::Ctx<'_>, error: rquickjs::Error) -> JsError {
    if !error.is_exception() {
        return JsError::from(error);
    }
    JsError::engine(
        exception_text(ctx.catch()).unwrap_or_else(|| String::from("uncaught exception")),
    )
}

/// Reports one uncaught exception to the realm's window: the `error` event and
/// `window.onerror`. `base_line` is the 1-based document line the script's
/// first source line occupies (0 when unknown), so the inline stack's
/// script-relative line can be translated into a document line.
///
/// <https://html.spec.whatwg.org/multipage/webappapis.html#report-the-error>
fn report_script_error(
    ctx: &rquickjs::Ctx<'_>,
    error: &rquickjs::Error,
    base_line: u32,
    filename: &str,
) {
    let caught = if error.is_exception() {
        ctx.catch()
    } else {
        match rquickjs::String::from_str(ctx.clone(), &error.to_string()) {
            Ok(text) => text.into_value(),
            Err(_) => return,
        }
    };
    bindings::report_exception_value(ctx, caught, base_line, filename);
}

/// Whether a caught value is the exception a classic-script parse failure
/// raised.
///
/// ECMAScript checks a script's early errors before running any of its
/// statements, so a parse failure executes no JavaScript. `QuickJS` builds the
/// parser's `SyntaxError` in `js_parse_error`, which calls `build_backtrace`
/// with only the source location and no running frame: the whole `stack` is
/// one `    at <file>:<line>:<col>` line. An exception thrown at runtime,
/// including one from the script's first statement, always carries the
/// evaluated script's frame, written `    at <name> (<file>:<line>:<col>)`.
///
/// This is the shape the embedding API exposes: rquickjs has no compile-only
/// eval, and probing with the `Function` constructor would change the grammar
/// (a function body accepts `return`) and add a CSP `unsafe-eval` surface.
fn is_script_parse_error(caught: &Value<'_>) -> bool {
    let Some(object) = caught.as_object() else {
        return false;
    };
    if object
        .get::<_, Option<String>>("name")
        .ok()
        .flatten()
        .as_deref()
        != Some("SyntaxError")
    {
        return false;
    }
    let stack: Option<String> = object.get("stack").ok().flatten();
    let Some(stack) = stack else {
        return false;
    };
    let frames: Vec<&str> = stack
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    frames.len() == 1 && !frames[0].contains('(')
}

fn decode_value<'js>(ctx: &rquickjs::Ctx<'js>, value: Value<'js>) -> Result<ScriptValue, JsError> {
    decode_value_inner(ctx, value, 0)
}

fn decode_value_inner<'js>(
    ctx: &rquickjs::Ctx<'js>,
    value: Value<'js>,
    depth: u8,
) -> Result<ScriptValue, JsError> {
    if depth > 32 {
        return Ok(ScriptValue::Null);
    }
    if value.is_undefined() {
        return Ok(ScriptValue::Undefined);
    }
    if value.is_null() {
        return Ok(ScriptValue::Null);
    }
    if let Some(flag) = value.as_bool() {
        return Ok(ScriptValue::Bool(flag));
    }
    if let Some(number) = value.as_number() {
        return Ok(ScriptValue::Number(number));
    }
    if let Ok(node) = rquickjs::Class::<bindings::JsNode>::from_js(ctx, value.clone()) {
        return Ok(ScriptValue::Node(node.borrow().node_id()));
    }
    if let Some(id) = bindings::host_node_id(ctx, &value) {
        return Ok(ScriptValue::Node(id));
    }
    if let Some(array) = value.as_array() {
        let mut items = Vec::with_capacity(array.len());
        for index in 0..array.len() {
            let item: Value = array.get(index)?;
            items.push(decode_value_inner(ctx, item, depth.saturating_add(1))?);
        }
        return Ok(ScriptValue::List(items));
    }
    if value.as_function().is_some() {
        return Ok(ScriptValue::Null);
    }
    if let Some(object) = value.as_object() {
        let mut map = Vec::new();
        for key in object.keys::<String>() {
            let key = key?;
            let nested: Value = object.get(key.as_str())?;
            map.push((
                key,
                decode_value_inner(ctx, nested, depth.saturating_add(1))?,
            ));
        }
        return Ok(ScriptValue::Map(map));
    }
    Ok(ScriptValue::String(
        Coerced::<String>::from_js(ctx, value)?.0,
    ))
}

fn render_eval_result<'js>(ctx: &rquickjs::Ctx<'js>, value: Value<'js>) -> Result<String, JsError> {
    if value.is_undefined() || value.is_null() {
        return Ok(String::new());
    }
    Ok(Coerced::<String>::from_js(ctx, value)?.0)
}

/// The typed write `cookies.js` hands `__tbCookieStoreSet`: the API's
/// algorithm already validated and defaulted every field, so this only checks
/// the shape at the boundary
/// (<https://cookiestore.spec.whatwg.org/#set-a-cookie>).
fn cookie_store_write(options: &Object<'_>) -> rquickjs::Result<net::ScriptCookieWrite> {
    let same_site = match options.get::<_, String>("sameSite")?.as_str() {
        "strict" => net::CookieSameSite::Strict,
        "lax" => net::CookieSameSite::Lax,
        "none" => net::CookieSameSite::None,
        other => {
            return Err(Exception::throw_type(
                options.ctx(),
                &format!("sameSite: {other:?} is not a CookieSameSite"),
            ));
        }
    };
    Ok(net::ScriptCookieWrite {
        name: options.get("name")?,
        value: options.get("value")?,
        expires_millis: options.get("expires")?,
        max_age: options.get("maxAge")?,
        domain: options.get("domain")?,
        path: options.get("path")?,
        same_site,
        partitioned: options.get("partitioned")?,
    })
}

/// The URL a `CookieStore` operation uses as its resource. It is the
/// document's creation URL, which a `pushState` does not move
/// (<https://cookiestore.spec.whatwg.org/#dom-cookiestore-get>). A document
/// that inherits its origin (an initial `about:blank` or `about:srcdoc`) has
/// no URL of its own that cookies apply to, so it uses its creator's cookie
/// URL, captured when the document was installed
/// (<https://html.spec.whatwg.org/multipage/dom.html#dom-document-cookie>).
fn cookie_store_url(world: &World) -> Url {
    world.cookie_url()
}

fn millis(delay: f64) -> u32 {
    if !delay.is_finite() || delay <= 0.0 {
        return 0;
    }
    let duration = Duration::from_secs_f64((delay / 1000.0).clamp(0.0, 86_400.0));
    u32::try_from(duration.as_millis()).unwrap_or(u32::MAX)
}
