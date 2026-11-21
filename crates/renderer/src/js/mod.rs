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

pub(crate) use events::handler_attribute_violations;
pub(crate) mod fonts;
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

const MAX_RUNTIME_MEMORY: usize = 256 * 1024 * 1024;
const MAX_RUNTIME_STACK: usize = 512 * 1024;
const DEFAULT_SCRIPT_BUDGET: Duration = Duration::from_secs(20);
pub const TURN_SCRIPT_BUDGET: Duration = Duration::from_secs(45);

#[derive(Clone, Copy)]
enum MicrotaskCheckpoint {
    Perform,
    Defer,
}

const INSTALL_WEB_APIS_DEFLATE: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/js_blobs/web_bundle.deflate"));

fn install_web_apis_js(ctx: &Ctx<'_>) -> rquickjs::Result<&'static str> {
    static CACHE: std::sync::OnceLock<Box<str>> = std::sync::OnceLock::new();
    blob::decompress(ctx, INSTALL_WEB_APIS_DEFLATE, &CACHE)
}

const INSTALL_WORKER_APIS_DEFLATE: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/js_blobs/worker_bundle.deflate"));

fn install_worker_apis_js(ctx: &Ctx<'_>) -> rquickjs::Result<&'static str> {
    static CACHE: std::sync::OnceLock<Box<str>> = std::sync::OnceLock::new();
    blob::decompress(ctx, INSTALL_WORKER_APIS_DEFLATE, &CACHE)
}

#[derive(Clone, Debug, PartialEq)]
pub enum ScriptValue {
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Node(crate::dom::tree::NodeId),
    List(Vec<ScriptValue>),
    Map(Vec<(String, ScriptValue)>),
}

// https://html.spec.whatwg.org/multipage/webappapis.html#prepare-the-script-element
#[derive(Clone)]
pub(crate) enum Script {
    Classic(ScriptElementSource),
    Module(ScriptElementSource),
}

#[derive(Clone)]
pub(crate) enum ScriptElementSource {
    Inline { source: String, line: u32 },
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

// https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model
pub(crate) enum WorkerScriptError {
    Parse,
    Runtime,
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

pub(crate) struct JsFetchRequest {
    pub url: String,
    pub js_id: JsFetchId,
    pub method: net::Method,
    pub body: Vec<u8>,
    pub content_type: Option<String>,
    pub headers: Vec<(String, String)>,
    pub options: DialOptions,
}

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
            // https://fetch.spec.whatwg.org/#concept-request-mode
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

// https://bellard.org/quickjs/quickjs.html
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
            // https://tc39.es/ecma262/#sec-abstract-operations-for-sharedarraybuffer-objects
            runtime.install_shared_array_buffer_hooks();
            runtime.set_loader(modules::WebModuleResolver, modules::WebModuleLoader);
            // https://html.spec.whatwg.org/multipage/webappapis.html#unhandled-promise-rejections
            runtime.set_host_promise_rejection_tracker(Some(Box::new(
                |ctx, promise, reason, handled| {
                    bindings::note_rejection(&ctx, promise, reason, handled);
                },
            )));
            // https://tc39.es/ecma262/#sec-evaluatecall
            // https://w3c.github.io/webappsec-csp/#can-compile-strings
            runtime.set_can_compile_strings(Some(Box::new(|ctx, source| {
                let Some(world) = bindings::try_world(&ctx) else {
                    return true;
                };
                if world.borrow().timer_string.replace(false) {
                    return true;
                }
                // https://w3c.github.io/webappsec-csp/#can-compile-strings
                if world.borrow().compiling_handler.get() {
                    return true;
                }
                match csp_eval_violations(&ctx, &world, source) {
                    Ok(violations) if !crate::csp::blocks(&violations) => true,
                    Ok(violations) => {
                        throw_eval_error(&ctx, &eval_denied_message(&violations));
                        false
                    }
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
    terminate: Option<Arc<AtomicBool>>,
    budget_depth: Cell<usize>,
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
        host.world.borrow_mut().realm_context = Some(host.context.clone());
        Ok(host)
    }

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

    // https://html.spec.whatwg.org/multipage/webappapis.html#report-the-error
    // https://tc39.es/ecma262/#sec-hostresolveimportedmodule
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

    // https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model
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
            // https://html.spec.whatwg.org/multipage/webappapis.html#clean-up-after-running-script
            Err(JsError::Interrupted) => Err(WorkerScriptError::Interrupted),
            Err(_) => Err(WorkerScriptError::Runtime),
        }
    }

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

    // https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model
    pub(crate) fn eval_worker_module(&self, url: &str) -> Result<(), WorkerScriptError> {
        let outcome = self.enter_script_url(url, || {
            self.with_budget(None, MicrotaskCheckpoint::Perform, || {
                self.context.with(|ctx| {
                    let Ok(module) = modules::load_module(&ctx, url) else {
                        return Ok(Some(WorkerScriptError::Parse));
                    };
                    let Ok((_, promise)) = module.eval() else {
                        return Ok(Some(WorkerScriptError::Parse));
                    };
                    // https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model
                    self.world.borrow_mut().state.module_client_origin = None;
                    match promise.finish::<()>() {
                        Ok(()) => Ok(None),
                        Err(error) => {
                            report_script_error(&ctx, &error, 0, url);
                            Ok(Some(WorkerScriptError::Runtime))
                        }
                    }
                })
            })
        });
        // https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model
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

    pub(crate) fn intern_node(
        &self,
        node: crate::dom::tree::NodeId,
    ) -> Result<crate::DomRemoteNode, JsError> {
        self.context
            .with(|ctx| -> rquickjs::Result<crate::DomRemoteNode> {
                let host = bridge::object(&ctx)?;
                let value = bindings::wrap_node(&ctx, node)?;
                // https://chromedevtools.github.io/devtools-protocol/tot/DOM/#method-resolveNode
                let object_id = {
                    let remote = self.world.borrow_mut().remote_id(node);
                    format!("dom-{remote}")
                };
                let handles: Object = host.get("__tb_handles")?;
                handles.set(object_id.as_str(), value)?;
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
                // https://html.spec.whatwg.org/multipage/timers-and-user-prompts.html#timers
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
                // https://html.spec.whatwg.org/multipage/timers-and-user-prompts.html#dom-settimeout
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

    // https://html.spec.whatwg.org/multipage/workers.html#dom-worker-postmessage
    pub(crate) fn deliver_worker_event(
        &self,
        worker: u64,
        payload: &str,
        ports: &[u64],
    ) -> Result<(), JsError> {
        let ports = ports.to_vec();
        self.with_budget(None, MicrotaskCheckpoint::Perform, || {
            self.context.with(|ctx| {
                let deliver: Function = bridge::object(&ctx)?.get("__tbWorkerEvent")?;
                deliver.call::<_, ()>((worker, payload, ports))?;
                Ok(())
            })
        })
    }

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
                let deliver: Function = bridge::object(&ctx)?.get("__tbWorkerErrorEvent")?;
                deliver.call::<_, ()>((worker, message, filename, lineno, colno))?;
                Ok(())
            })
        })
    }

    // https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model
    pub(crate) fn deliver_worker_load_error(&self, worker: u64) -> Result<(), JsError> {
        self.with_budget(None, MicrotaskCheckpoint::Perform, || {
            self.context.with(|ctx| {
                let deliver: Function = bridge::object(&ctx)?.get("__tbWorkerLoadErrorEvent")?;
                deliver.call::<_, ()>((worker,))?;
                Ok(())
            })
        })
    }

    pub(crate) fn deliver_worker_message(
        &self,
        payload: &str,
        ports: &[u64],
    ) -> Result<(), JsError> {
        let ports = ports.to_vec();
        self.with_budget(None, MicrotaskCheckpoint::Perform, || {
            self.context.with(|ctx| {
                let deliver: Function = bridge::object(&ctx)?.get("__tbWorkerDeliverMessage")?;
                deliver.call::<_, ()>((payload, ports))?;
                Ok(())
            })
        })
    }

    // https://html.spec.whatwg.org/multipage/workers.html#dom-sharedworkerglobalscope-onconnect
    pub(crate) fn deliver_shared_connect(&self, endpoint: u64) -> Result<(), JsError> {
        self.with_budget(None, MicrotaskCheckpoint::Perform, || {
            self.context.with(|ctx| {
                let deliver: Function = bridge::object(&ctx)?.get("__tbWorkerConnect")?;
                deliver.call::<_, ()>((endpoint,))?;
                Ok(())
            })
        })
    }

    // https://drafts.csswg.org/cssom-view/#run-the-resize-steps
    pub(crate) fn report_visual_viewport_changes(&self) -> Result<(), JsError> {
        self.with_budget(None, MicrotaskCheckpoint::Perform, || {
            self.context.with(|ctx| {
                let host = bridge::object(&ctx)?;
                let Some(func) =
                    host.get::<_, Option<Function>>("__tbReportVisualViewportChanges")?
                else {
                    return Ok(());
                };
                func.call::<_, ()>(())?;
                Ok(())
            })
        })
    }

    // https://drafts.csswg.org/cssom-view/#evaluate-media-queries-and-report-changes
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
                    if let Some(crate::protocol::ResponseBody::Streamed(id)) =
                        outcome.as_ref().map(|outcome| &outcome.body)
                    {
                        self.world.borrow().runtime.services.cancel_stream(*id);
                    }
                    return Ok(());
                };
                // https://fetch.spec.whatwg.org/#concept-header-extract-mime-type
                let callback = Object::new(ctx.clone())?;
                if let Some(outcome) = outcome {
                    callback.set("status", i32::from(outcome.status))?;
                    callback.set("statusText", outcome.status_text)?;
                    // https://fetch.spec.whatwg.org/#concept-filtered-response-opaque
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
                    // https://fetch.spec.whatwg.org/#concept-response-body
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
                    // https://fetch.spec.whatwg.org/#concept-network-error
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

    // https://w3c.github.io/navigation-timing/#marking-navigation-timing
    pub(crate) fn fire_navigation_timing_entry(&self) -> Result<(), JsError> {
        self.with_budget(None, MicrotaskCheckpoint::Perform, || {
            self.context.with(|ctx| {
                bindings::queue_navigation_timing_entry(&ctx)?;
                Ok(())
            })
        })
    }

    pub(crate) fn refresh_document(&self) -> Result<(), JsError> {
        self.context.with(|ctx| {
            bindings::refresh_document_global(&ctx)?;
            Ok(())
        })
    }

    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#update-document-for-history-step-application
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

    // https://cookiestore.spec.whatwg.org/#process-cookie-changes
    pub(crate) fn process_cookie_changes(&self) -> Result<(), JsError> {
        self.with_budget(None, MicrotaskCheckpoint::Perform, || {
            self.context.with(|ctx| {
                process_cookie_changes_now(&ctx)?;
                Ok(())
            })
        })
    }

    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#the-hashchangeevent-interface
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

    // https://html.spec.whatwg.org/multipage/parsing.html#the-end
    pub(crate) fn fire_dom_content_loaded(&self) -> Result<(), JsError> {
        self.with_budget(None, MicrotaskCheckpoint::Perform, || {
            self.context.with(|ctx| {
                bindings::fire_dom_content_loaded(&ctx)?;
                Ok(())
            })
        })
    }

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

    pub(crate) fn font_face_settled(
        &self,
        sheet: u64,
        path: Vec<usize>,
        loaded: bool,
    ) -> Result<(), JsError> {
        self.with_budget(None, MicrotaskCheckpoint::Perform, || {
            self.context.with(|ctx| {
                bindings::font_face_settled(&ctx, sheet, path, loaded)?;
                Ok(())
            })
        })
    }

    // https://w3c.github.io/resource-timing/#dfn-add-a-performanceresourcetiming-entry
    pub(crate) fn queue_resource_timing(
        &self,
        name: String,
        initiator_type: &str,
        start_time: f64,
        end_time: f64,
        response_status: u16,
        encoded_body_size: u64,
    ) -> Result<(), JsError> {
        self.with_budget(None, MicrotaskCheckpoint::Perform, || {
            self.context.with(|ctx| {
                bindings::queue_resource_timing(
                    &ctx,
                    bindings::ResourceTiming {
                        name: &name,
                        initiator_type,
                        start_time,
                        end_time,
                        response_status,
                        encoded_body_size,
                        decoded_body_size: encoded_body_size,
                    },
                )?;
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

    // https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#set-the-selection-range
    pub(crate) fn fire_selection_changed(
        &self,
        id: crate::dom::tree::NodeId,
    ) -> Result<(), JsError> {
        self.with_budget(None, MicrotaskCheckpoint::Perform, || {
            self.context.with(|ctx| {
                bindings::fire_selection_changed(&ctx, id)?;
                Ok(())
            })
        })
    }

    // https://html.spec.whatwg.org/multipage/interactive-elements.html#queue-a-details-toggle-event-task
    // https://html.spec.whatwg.org/multipage/interactive-elements.html#queue-a-dialog-toggle-event-task
    pub(crate) fn fire_toggle_task(
        &self,
        id: crate::dom::tree::NodeId,
        old_state: &str,
        new_state: &str,
    ) -> Result<(), JsError> {
        self.with_budget(None, MicrotaskCheckpoint::Perform, || {
            self.context.with(|ctx| {
                bindings::fire_toggle_task(&ctx, id, old_state, new_state)?;
                Ok(())
            })
        })
    }

    // https://html.spec.whatwg.org/multipage/interactive-elements.html#the-dialog-element:close-event
    pub(crate) fn fire_dialog_close(&self, id: crate::dom::tree::NodeId) -> Result<(), JsError> {
        self.with_budget(None, MicrotaskCheckpoint::Perform, || {
            self.context.with(|ctx| {
                bindings::fire_dialog_close(&ctx, id)?;
                Ok(())
            })
        })
    }

    // https://drafts.csswg.org/cssom-view/#run-the-scroll-steps
    pub(crate) fn fire_scroll_events(
        &self,
        events: &[blitz_dom::PendingScrollEvent],
    ) -> Result<(), JsError> {
        self.with_budget(None, MicrotaskCheckpoint::Perform, || {
            self.context.with(|ctx| {
                for event in events {
                    bindings::fire_scroll_event(&ctx, *event)?;
                }
                Ok(())
            })
        })
    }

    // https://drafts.csswg.org/css-animations-1/#events
    pub(crate) fn fire_animation_events(
        &self,
        events: &[blitz_dom::animation_events::PendingAnimationEvent],
    ) -> Result<(), JsError> {
        self.with_budget(None, MicrotaskCheckpoint::Perform, || {
            self.context.with(|ctx| {
                for event in events {
                    bindings::fire_animation_event(&ctx, event)?;
                }
                Ok(())
            })
        })
    }

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

    // https://html.spec.whatwg.org/multipage/web-messaging.html#window-post-message-steps
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

    // https://html.spec.whatwg.org/multipage/webstorage.html#concept-storage-broadcast
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

    // https://html.spec.whatwg.org/multipage/web-messaging.html#window-post-message-steps
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

    // https://html.spec.whatwg.org/multipage/web-messaging.html#broadcasting-to-other-browsing-contexts
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

    // https://html.spec.whatwg.org/multipage/web-messaging.html#disentangle
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

    pub(crate) fn deliver_mutations(&self) -> Result<(), JsError> {
        self.with_budget(None, MicrotaskCheckpoint::Perform, || {
            self.context.with(|ctx| {
                bindings::schedule_mutation_delivery(&ctx)?;
                Ok(())
            })
        })
    }

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

    // https://html.spec.whatwg.org/multipage/webappapis.html#notify-about-rejected-promises
    fn report_pending_rejections(&self) {
        self.context.with(|ctx| {
            bindings::notify_rejected_promises(&ctx, &self.world);
        });
    }

    pub(crate) fn fire_rejection_tasks(&self) {
        self.context.with(|ctx| {
            bindings::fire_rejection_tasks(&ctx, &self.world);
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
            for name in [
                "BeforeUnloadEvent",
                "CompositionEvent",
                "CustomEvent",
                "DeviceMotionEvent",
                "DeviceOrientationEvent",
                "DragEvent",
                "ErrorEvent",
                "FocusEvent",
                "HashChangeEvent",
                "KeyboardEvent",
                "MessageEvent",
                "MouseEvent",
                "PointerEvent",
                "SecurityPolicyViolationEvent",
                "StorageEvent",
                "TextEvent",
                "ToggleEvent",
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

    // https://html.spec.whatwg.org/multipage/workers.html#the-workerglobalscope-common-interface
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
            // https://html.spec.whatwg.org/multipage/workers.html#apis-available-to-workers
            crate::js::canvas::install(&ctx)?;
            bridge::evaluate(&ctx, install_worker_apis_js(&ctx)?)?;
            Ok(())
        })
    }

    // https://html.spec.whatwg.org/multipage/workers.html#dom-workerglobalscope-importscripts
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
                // https://infra.spec.whatwg.org/#utf-8-decode
                let source = String::from_utf8_lossy(&body);
                let source = source.strip_prefix('\u{feff}').unwrap_or(&source);
                Some(source.to_owned())
            }),
        )?;
        // https://html.spec.whatwg.org/multipage/webappapis.html#fetch-a-classic-worker-imported-script
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
        // https://w3c.github.io/webappsec-csp/#should-request-be-blocked-by-content-security-policy
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
                    world
                        .outbox
                        .borrow_mut()
                        .push_back(RealmRequest::CspViolation(target, violations));
                }
                !blocked
            }),
        )?;
        Ok(())
    }

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

        bridge::object(ctx)?.set("__tbCookieAverse", Func::from(cookie_averse))?;

        bridge::object(ctx)?.set(
            "__cookieSet",
            Func::from(move |ctx: Ctx<'_>, value: String| -> rquickjs::Result<()> {
                let (services, url, partition_key) = {
                    let world = cookie_set.borrow();
                    (
                        world.runtime.services.clone(),
                        world.document_url.clone(),
                        world.partition_key(),
                    )
                };
                services.set_cookie(&value, &url, partition_key.as_deref());
                // https://cookiestore.spec.whatwg.org/#process-cookie-changes
                cookie_set.borrow().notify_cookie_changes();
                report_cookie_change_failure(&ctx, process_cookie_changes_now(&ctx));
                Ok(())
            }),
        )?;

        Self::install_cookie_store_host_functions(ctx, world)
    }

    // https://cookiestore.spec.whatwg.org/#cookie-store-interface
    fn install_cookie_store_host_functions(
        ctx: &Ctx<'_>,
        world: &Rc<RefCell<World>>,
    ) -> Result<(), JsError> {
        // https://cookiestore.spec.whatwg.org/#dom-cookiestore-get
        let creation_url = world.clone();
        bridge::object(ctx)?.set(
            "__tbCookieStoreCreationUrl",
            Func::from(move || cookie_store_url(&creation_url.borrow()).to_string()),
        )?;

        // https://cookiestore.spec.whatwg.org/#set-a-cookie
        bridge::object(ctx)?.set(
            "__tbCookieStorePublicSuffix",
            Func::from(|domain: String| net::is_public_suffix(&domain)),
        )?;

        // https://cookiestore.spec.whatwg.org/#set-a-cookie
        bridge::object(ctx)?.set(
            "__tbCookieStoreParseDomain",
            Func::from(|domain: String| {
                url::Host::parse(&domain).ok().map(|host| host.to_string())
            }),
        )?;

        // https://html.spec.whatwg.org/multipage/urls-and-fetching.html#is-a-registrable-domain-suffix-of-or-is-equal-to
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

        // https://html.spec.whatwg.org/multipage/webappapis.html#api-base-url
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
                // https://datatracker.ietf.org/doc/html/rfc6265#section-4.1.1
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
            Func::from(
                move |ctx: Ctx<'_>, options: Object<'_>| -> rquickjs::Result<()> {
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
                    store_set.borrow().notify_cookie_changes();
                    report_cookie_change_failure(&ctx, process_cookie_changes_now(&ctx));
                    Ok(())
                },
            ),
        )?;
        Ok(())
    }

    // https://html.spec.whatwg.org/multipage/workers.html#the-workerglobalscope-common-interface
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

        // https://html.spec.whatwg.org/multipage/browsers.html#concept-origin
        bridge::object(ctx)?.set(
            "__tbOrigin",
            Func::from(move || realm_origin.borrow().origin_string()),
        )?;
        Ok(())
    }
}

// https://cookiestore.spec.whatwg.org/#process-cookie-changes
fn process_cookie_changes_now(ctx: &Ctx<'_>) -> rquickjs::Result<()> {
    let process: Option<rquickjs::Function> =
        bridge::object(ctx)?.get("__tbCookieStoreProcessChanges")?;
    let Some(process) = process else {
        return Ok(());
    };
    let _: Value = process.call(())?;
    Ok(())
}

// https://html.spec.whatwg.org/multipage/webappapis.html#report-the-error
fn report_cookie_change_failure(ctx: &Ctx<'_>, result: rquickjs::Result<()>) {
    if let Err(error) = result {
        bindings::host::report_callback_error(ctx, &error);
    }
}

// https://html.spec.whatwg.org/multipage/nav-history-apis.html#shared-history-push/replace-state-steps
fn install_history_host_functions(
    ctx: &Ctx<'_>,
    world: &Rc<RefCell<World>>,
) -> Result<(), JsError> {
    let snapshot = world.borrow().history.clone();
    bridge::object(ctx)?.set("__tbHistoryLength", snapshot.length.max(1))?;
    bridge::object(ctx)?.set("__tbHistoryState", snapshot.state)?;
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

// https://w3c.github.io/webcrypto/#Crypto-method-getRandomValues
fn random_bytes(ctx: Ctx<'_>, length: usize) -> rquickjs::Result<TypedArray<'_, u8>> {
    if length > 65_536 {
        return Err(Exception::throw_range(&ctx, "random byte quota exceeded"));
    }
    let mut bytes = vec![0; length];
    getrandom::fill(&mut bytes)
        .map_err(|error| Exception::throw_internal(&ctx, &error.to_string()))?;
    TypedArray::new(ctx, bytes)
}

fn install_window_host_functions(ctx: &Ctx<'_>, world: &Rc<RefCell<World>>) -> Result<(), JsError> {
    // https://html.spec.whatwg.org/multipage/nav-history-apis.html#window-open-steps
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
                // https://html.spec.whatwg.org/multipage/web-messaging.html#window-post-message-steps
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

// https://html.spec.whatwg.org/multipage/nav-history-apis.html#window-open-steps
struct WindowOpenAnswer {
    context: OpenedContext,
    withholds: bool,
}

enum OpenedContext {
    Frame(FrameId),
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

// https://html.spec.whatwg.org/multipage/nav-history-apis.html#window-open-steps
fn open_window(
    world: &Rc<RefCell<World>>,
    url: &str,
    name: &str,
    features: &str,
) -> Option<WindowOpenAnswer> {
    let target = TargetName::for_window_open(name);
    let plan = world.borrow().plan_window_open(url, &target, features)?;
    let services = world.borrow().runtime.services.clone();
    // https://html.spec.whatwg.org/multipage/document-sequences.html#the-rules-for-choosing-a-navigable
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
    // https://html.spec.whatwg.org/multipage/browsers.html#sandboxed-auxiliary-navigation-browsing-context-flag
    if !world.borrow().allows_auxiliary_navigation() {
        return None;
    }
    // https://html.spec.whatwg.org/multipage/window-object.html#dom-open
    if plan.url.as_ref().is_some_and(|url| url.scheme() == "blob")
        && let Some(frame) =
            World::create_auxiliary(world, plan.url.as_ref(), &target, plan.opener, false)
    {
        return Some(WindowOpenAnswer {
            context: OpenedContext::Frame(frame),
            withholds: plan.withholds,
        });
    }
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
        Func::from(move |ctx: Ctx<'_>| {
            // https://html.spec.whatwg.org/multipage/browsing-the-web.html#unload
            if let Err(error) = crate::js::events::fire_document_unload(&ctx) {
                crate::js::bindings::host::report_callback_error(&ctx, &error);
            }
            let world = close.borrow();
            // https://html.spec.whatwg.org/multipage/window-object.html#dom-window-close
            if world.frame() == FrameId::MAIN
                && let Some(window) = world.own_window
            {
                world.runtime.services.window_close(window);
                return;
            }
            world.close_auxiliary(world.frame());
        }),
    )?;
    Ok(())
}

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
        Some(src) if !src.trim().is_empty() => ScriptElementSource::Src(src.to_owned()),
        _ => ScriptElementSource::Inline {
            source: crate::dom::text::child_text_content(base, id.node),
            line: 0,
        },
    };
    // https://html.spec.whatwg.org/multipage/scripting.html#prepare-the-script-element
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
    // https://fetch.spec.whatwg.org/#concept-header-extract-mime-type
    let Some(mime) = typ.and_then(crate::sniff::extract_mime_type) else {
        return false;
    };
    let essence = mime.split(';').next().unwrap_or(&mime);
    javascript_mime_essence(essence)
}

// https://html.spec.whatwg.org/multipage/scripting.html#prepare-the-script-element
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

// https://html.spec.whatwg.org/multipage/scripting.html#javascript-mime-type-essence-match
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

fn install_mime_host_functions(ctx: &Ctx<'_>) -> Result<(), JsError> {
    // https://mimesniff.spec.whatwg.org/#parse-a-mime-type
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
    // https://fetch.spec.whatwg.org/#concept-header-extract-mime-type
    bridge::object(ctx)?.set(
        "__tbExtractMimeType",
        Func::from(|value: String| -> String {
            crate::sniff::extract_mime_type(&value).unwrap_or_default()
        }),
    )?;
    Ok(())
}

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
                // https://fetch.spec.whatwg.org/#concept-request-referrer
                // https://xhr.spec.whatwg.org/#the-withcredentials-attribute
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

// https://xhr.spec.whatwg.org/#the-timeout-attribute
fn sync_page_fetch(world: &World, request: SyncPageFetch) -> Option<SyncFetchList> {
    if request.url.starts_with("data:") {
        let (content_type, body) = crate::engine::decode_data_url_bounded(&request.url)?;
        // https://fetch.spec.whatwg.org/#data-url-processor
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
            // https://xhr.spec.whatwg.org/#the-withcredentials-attribute
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
    let outcome = world.runtime.services.dial_sync(dial);
    match outcome {
        // https://fetch.spec.whatwg.org/#concept-response-header-list
        Ok(outcome) => {
            // https://fetch.spec.whatwg.org/#concept-filtered-response-opaque
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

fn sync_fetch_value(
    status: i32,
    body: Vec<u8>,
    url: String,
    headers: Vec<Vec<String>>,
) -> SyncFetchList {
    rquickjs::prelude::List((status, body, url, headers))
}

// https://w3c.github.io/webappsec-csp/#can-compile-strings
// https://w3c.github.io/webappsec-csp/#create-violation-for-global
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

// https://tc39.es/ecma262/#sec-eval-x
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

// https://w3c.github.io/webappsec-csp/#create-violation-for-global
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

fn exception_text(caught: Value<'_>) -> Option<String> {
    let exception = caught.into_exception()?;
    Some(format!(
        "{} | {}",
        exception.message().unwrap_or_default(),
        exception.stack().unwrap_or_default()
    ))
}

fn caught_error(ctx: &rquickjs::Ctx<'_>, error: rquickjs::Error) -> JsError {
    if !error.is_exception() {
        return JsError::from(error);
    }
    JsError::engine(
        exception_text(ctx.catch()).unwrap_or_else(|| String::from("uncaught exception")),
    )
}

// https://html.spec.whatwg.org/multipage/webappapis.html#report-the-error
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

// https://cookiestore.spec.whatwg.org/#set-a-cookie
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

// https://cookiestore.spec.whatwg.org/#dom-cookiestore-get
// https://html.spec.whatwg.org/multipage/dom.html#dom-document-cookie
fn cookie_store_url(world: &World) -> Url {
    world.cookie_url()
}

// https://html.spec.whatwg.org/multipage/dom.html#cookie-averse-document-object
fn cookie_averse<'js>(ctx: Ctx<'js>, document: Value<'js>) -> bool {
    let Some(id) = bindings::host_node_id(&ctx, &document) else {
        return true;
    };
    let Ok(world) = bindings::world(&ctx) else {
        return true;
    };
    let world = world.borrow();
    if !world.is_main_document(id) {
        return true;
    }
    !world
        .document_url_of(id)
        .parse::<Url>()
        .is_ok_and(|url| matches!(url.scheme(), "http" | "https"))
}

fn millis(delay: f64) -> u32 {
    if !delay.is_finite() || delay <= 0.0 {
        return 0;
    }
    let duration = Duration::from_secs_f64((delay / 1000.0).clamp(0.0, 86_400.0));
    u32::try_from(duration.as_millis()).unwrap_or(u32::MAX)
}
