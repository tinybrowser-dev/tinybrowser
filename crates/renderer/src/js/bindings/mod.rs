use rquickjs::function::Constructor;

mod attributes;
mod clone;
mod collections;
mod cssom_host;
mod document;
mod element_reference;
pub(crate) mod enumerated;
mod exceptions;
pub(crate) mod focus;
pub(crate) mod forms;
pub(crate) mod geometry;
pub(crate) mod host;
mod inner_text;
mod messaging;
mod mutation;
mod node;
mod origin;
mod performance;
pub(crate) use node::document::prepare_classic_script;
mod parsing;
mod range;
mod selection;
mod webidl;
mod window;

pub(super) use attributes::*;
pub(super) use clone::*;
pub(super) use collections::*;
pub(super) use document::*;
pub(super) use element_reference::*;
pub(super) use exceptions::*;
pub(super) use focus::*;
pub(super) use messaging::*;
pub(super) use mutation::*;
pub(super) use node::*;
pub(super) use parsing::*;
pub(crate) use performance::{
    ResourceTiming, queue_navigation_timing_entry, queue_resource_timing,
};
pub(super) use range::*;
pub(crate) use webidl::selector_matching_elements;
pub(super) use webidl::*;
pub(super) use window::*;

use std::cell::RefCell;

use std::collections::HashMap;

use std::rc::{Rc, Weak};

use blitz_dom::NodeData;
use markup5ever::{LocalName, Namespace, Prefix, QualName};

use crate::dom::TreeError;
use crate::dom::tree::{NodeId, is_element, sibling};

use rquickjs::{
    Array, Class, Ctx, Exception, FromJs, Function, Object, Persistent, Result, Value,
    prelude::This,
};

use super::events::{self, JsEvent, JsEventTarget};

use super::world::{Handle, RealmRegistry, WeakReferences, World};

thread_local! {
    static REALM_WORLDS: RefCell<HashMap<usize, Weak<RefCell<World>>>> =
        RefCell::new(HashMap::new());
    static REALM_REGISTRIES: RefCell<HashMap<usize, Weak<RefCell<RealmRegistry>>>> =
        RefCell::new(HashMap::new());
}

fn register_world(ctx: &Ctx<'_>, world: &Rc<RefCell<World>>) {
    let registry = world.borrow().registry();
    let context = ctx.as_raw().as_ptr() as usize;
    REALM_REGISTRIES.with(|registries| {
        registries
            .borrow_mut()
            .insert(context, Rc::downgrade(&registry));
    });
    registry.borrow_mut().realm_contexts.push(context);
    REALM_WORLDS.with(|worlds| {
        worlds
            .borrow_mut()
            .insert(ctx.as_raw().as_ptr() as usize, Rc::downgrade(world));
    });
}

pub(crate) fn forget_world(context: &rquickjs::Context) {
    REALM_WORLDS.with(|worlds| {
        worlds
            .borrow_mut()
            .remove(&(context.as_raw().as_ptr() as usize));
    });
}

pub(crate) fn forget_registry_contexts(contexts: &[usize]) {
    REALM_REGISTRIES.with(|registries| {
        let mut registries = registries.borrow_mut();
        for context in contexts {
            registries.remove(context);
        }
    });
}

pub(crate) fn realm_registry(ctx: &Ctx<'_>) -> Result<Rc<RefCell<RealmRegistry>>> {
    REALM_REGISTRIES
        .with(|registries| {
            registries
                .borrow()
                .get(&(ctx.as_raw().as_ptr() as usize))
                .and_then(Weak::upgrade)
        })
        .ok_or_else(|| Exception::throw_internal(ctx, "missing JS agent"))
}

pub(crate) fn main_document(ctx: &Ctx<'_>) -> Result<NodeId> {
    world(ctx)?
        .borrow()
        .with_main_document(|parsed| NodeId {
            document: parsed.id,
            node: parsed.document.base.root_node().id,
        })
        .ok_or_else(|| Exception::throw_type(ctx, "no document"))
}

pub(crate) fn dom_exception_value<'js>(
    ctx: &Ctx<'js>,
    name: &str,
    message: &str,
) -> rquickjs::Result<Value<'js>> {
    let name = rquickjs::String::from_str(ctx.clone(), name)?;
    let message = rquickjs::String::from_str(ctx.clone(), message)?;
    Ok(host::instance(ctx, JsDomException { name, message })?.into_value())
}

pub(crate) fn throw_dom(ctx: &Ctx<'_>, name: &str, message: &str) -> rquickjs::Error {
    match dom_exception_value(ctx, name, message) {
        Ok(exception) => ctx.throw(exception),
        Err(err) => err,
    }
}

// https://dom.spec.whatwg.org/#dom-domerror
pub(crate) fn throw_dom_error(ctx: &Ctx<'_>, err: TreeError) -> rquickjs::Error {
    match err {
        TreeError::Hierarchy => throw_dom(ctx, "HierarchyRequestError", &err.to_string()),
        TreeError::NotFound => throw_dom(ctx, "NotFoundError", &err.to_string()),
        TreeError::Stale => Exception::throw_type(ctx, &err.to_string()),
    }
}

// https://html.spec.whatwg.org/multipage/webappapis.html#report-the-error
pub(super) fn report_exception_value<'js>(
    ctx: &Ctx<'js>,
    caught: Value<'js>,
    base_line: u32,
    filename: &str,
) {
    let Ok(report) =
        crate::js::bridge::object(ctx).and_then(|host| host.get::<_, Value>("__tbReportException"))
    else {
        return;
    };
    let Some(report) = report.as_function() else {
        return;
    };
    let Ok(meta) = Object::new(ctx.clone()) else {
        return;
    };
    if meta.set("baseLine", base_line).is_err() || meta.set("filename", filename).is_err() {
        return;
    }
    if report.call::<_, ()>((caught, meta)).is_err() {
        let _ = ctx.catch();
    }
}

// https://html.spec.whatwg.org/multipage/webappapis.html#unhandled-promise-rejections
struct TrackedRejection {
    world: Weak<RefCell<World>>,
    promise: Persistent<Value<'static>>,
    reason: Persistent<Value<'static>>,
}

thread_local! {
    static ABOUT_TO_NOTIFY: RefCell<Vec<TrackedRejection>> =
        const { RefCell::new(Vec::new()) };
    // https://html.spec.whatwg.org/multipage/webappapis.html#notify-about-rejected-promises
    static NOTIFICATION_TASKS: RefCell<Vec<TrackedRejection>> =
        const { RefCell::new(Vec::new()) };
    static OUTSTANDING: RefCell<Vec<TrackedRejection>> =
        const { RefCell::new(Vec::new()) };
    static HANDLED_TASKS: RefCell<Vec<TrackedRejection>> =
        const { RefCell::new(Vec::new()) };
}

fn holds_promise<'js>(ctx: &Ctx<'js>, entry: &TrackedRejection, promise: &Value<'js>) -> bool {
    entry
        .promise
        .clone()
        .restore(ctx)
        .is_ok_and(|stored| stored == *promise)
}

fn remove_promise<'js>(
    ctx: &Ctx<'js>,
    queue: &RefCell<Vec<TrackedRejection>>,
    promise: &Value<'js>,
) -> Option<TrackedRejection> {
    let mut queue = queue.borrow_mut();
    let index = queue
        .iter()
        .position(|entry| holds_promise(ctx, entry, promise))?;
    Some(queue.remove(index))
}

fn contains_promise<'js>(
    ctx: &Ctx<'js>,
    queue: &RefCell<Vec<TrackedRejection>>,
    promise: &Value<'js>,
) -> bool {
    queue
        .borrow()
        .iter()
        .any(|entry| holds_promise(ctx, entry, promise))
}

fn queue_rejection_task(ctx: &Ctx<'_>) {
    let Ok(world) = world(ctx) else {
        return;
    };
    world
        .borrow()
        .outbox
        .borrow_mut()
        .push_back(crate::js::world::RealmRequest::RejectionTasks);
}

// https://html.spec.whatwg.org/multipage/webappapis.html#hostpromiserejectiontracker
pub(super) fn note_rejection<'js>(
    ctx: &Ctx<'js>,
    promise: Value<'js>,
    reason: Value<'js>,
    handled: bool,
) {
    if handled {
        if ABOUT_TO_NOTIFY
            .with(|queue| remove_promise(ctx, queue, &promise))
            .is_some()
        {
            return;
        }
        if NOTIFICATION_TASKS
            .with(|queue| remove_promise(ctx, queue, &promise))
            .is_some()
        {
            return;
        }
        let Some(entry) = OUTSTANDING.with(|queue| remove_promise(ctx, queue, &promise)) else {
            return;
        };
        HANDLED_TASKS.with(|queue| queue.borrow_mut().push(entry));
        queue_rejection_task(ctx);
        return;
    }
    if ABOUT_TO_NOTIFY.with(|queue| contains_promise(ctx, queue, &promise))
        || NOTIFICATION_TASKS.with(|queue| contains_promise(ctx, queue, &promise))
        || OUTSTANDING.with(|queue| contains_promise(ctx, queue, &promise))
    {
        return;
    }
    let Ok(world) = world(ctx) else {
        return;
    };
    ABOUT_TO_NOTIFY.with(|queue| {
        queue.borrow_mut().push(TrackedRejection {
            world: Rc::downgrade(&world),
            promise: Persistent::save(ctx, promise),
            reason: Persistent::save(ctx, reason),
        });
    });
}

pub(super) fn forget_rejections(current: &Rc<RefCell<World>>) {
    for queue in [
        &ABOUT_TO_NOTIFY,
        &NOTIFICATION_TASKS,
        &OUTSTANDING,
        &HANDLED_TASKS,
    ] {
        queue.with(|queue| {
            queue.borrow_mut().retain(|entry| {
                entry
                    .world
                    .upgrade()
                    .is_some_and(|world| !Rc::ptr_eq(&world, current))
            });
        });
    }
}

fn take_for_realm(
    queue: &'static std::thread::LocalKey<RefCell<Vec<TrackedRejection>>>,
    current: &Rc<RefCell<World>>,
) -> Vec<TrackedRejection> {
    queue.with(|queue| {
        let mut queue = queue.borrow_mut();
        let mut mine = Vec::new();
        let mut rest = Vec::new();
        for entry in queue.drain(..) {
            let Some(world) = entry.world.upgrade() else {
                continue;
            };
            if Rc::ptr_eq(&world, current) {
                mine.push(entry);
            } else {
                rest.push(entry);
            }
        }
        *queue = rest;
        mine
    })
}

// https://html.spec.whatwg.org/multipage/webappapis.html#notify-about-rejected-promises
pub(super) fn notify_rejected_promises(ctx: &Ctx<'_>, current: &Rc<RefCell<World>>) {
    let rejected = take_for_realm(&ABOUT_TO_NOTIFY, current);
    if rejected.is_empty() {
        return;
    }
    NOTIFICATION_TASKS.with(|queue| {
        let mut queue = queue.borrow_mut();
        queue.extend(rejected);
    });
    queue_rejection_task(ctx);
}

// https://html.spec.whatwg.org/multipage/webappapis.html#notify-about-rejected-promises
pub(super) fn fire_rejection_tasks(ctx: &Ctx<'_>, current: &Rc<RefCell<World>>) {
    for entry in take_for_realm(&NOTIFICATION_TASKS, current) {
        let (Ok(promise), Ok(reason)) = (
            entry.promise.clone().restore(ctx),
            entry.reason.clone().restore(ctx),
        ) else {
            continue;
        };
        fire_rejection(ctx, false, promise, reason);
        OUTSTANDING.with(|queue| queue.borrow_mut().push(entry));
    }
    for entry in take_for_realm(&HANDLED_TASKS, current) {
        if let (Ok(promise), Ok(reason)) = (entry.promise.restore(ctx), entry.reason.restore(ctx)) {
            fire_rejection(ctx, true, promise, reason);
        }
    }
}

fn fire_rejection<'js>(ctx: &Ctx<'js>, handled: bool, promise: Value<'js>, reason: Value<'js>) {
    let Ok(report) = crate::js::bridge::object(ctx)
        .and_then(|host| host.get::<_, Value>("__tbPromiseRejection"))
    else {
        return;
    };
    let Some(report) = report.as_function() else {
        return;
    };
    if report.call::<_, Value>((handled, promise, reason)).is_err() {
        let _ = ctx.catch();
    }
}

impl<'js> rquickjs::FromJs<'js> for OptString {
    fn from_js(ctx: &Ctx<'js>, value: Value<'js>) -> Result<Self> {
        if value.is_null() || value.is_undefined() {
            return Ok(Self(None));
        }
        Ok(Self(Some(webidl_to_string(ctx, value)?)))
    }
}

impl<'js> rquickjs::FromJs<'js> for WebIdlString {
    fn from_js(ctx: &Ctx<'js>, value: Value<'js>) -> Result<Self> {
        Ok(Self(webidl_to_string(ctx, value)?))
    }
}

impl<'js> rquickjs::FromJs<'js> for LegacyNullString {
    fn from_js(ctx: &Ctx<'js>, value: Value<'js>) -> Result<Self> {
        if value.is_null() {
            return Ok(Self(String::new()));
        }
        Ok(Self(webidl_to_string(ctx, value)?))
    }
}

impl<'js> rquickjs::FromJs<'js> for WebIdlCodeUnits {
    fn from_js(ctx: &Ctx<'js>, value: Value<'js>) -> Result<Self> {
        Ok(Self(crate::dom_string::DomString::from_utf16(
            webidl_to_units(ctx, value)?,
        )))
    }
}

impl<'js> rquickjs::FromJs<'js> for WebIdlUnsignedLong {
    fn from_js(ctx: &Ctx<'js>, value: Value<'js>) -> Result<Self> {
        let pristine = world(ctx)?.borrow().pristine_number.clone();
        if let Some(pristine) = pristine {
            let to_number: Function = pristine.restore(ctx)?;
            let number: f64 = to_number.call((value.clone(),))?;
            return Ok(Self(webidl_unsigned_long(number)));
        }
        let to_number: Function = ctx.globals().get("Number")?;
        let number: f64 = to_number.call((value,))?;
        Ok(Self(webidl_unsigned_long(number)))
    }
}

pub(super) fn handler_target<'js>(ctx: &Ctx<'js>, node: &Value<'js>) -> Result<NodeId> {
    host_node_id(ctx, node)
        .ok_or_else(|| Exception::throw_type(ctx, "event handler target is not a node"))
}

pub(crate) fn install(ctx: &Ctx<'_>, world: &Rc<RefCell<World>>) -> Result<()> {
    install_common(ctx, world)?;
    install_document(ctx, world)
}

pub(crate) fn install_worker(ctx: &Ctx<'_>, world: &Rc<RefCell<World>>) -> Result<()> {
    install_common(ctx, world)?;
    install_worker_global_hosts(ctx, world)?;
    let globals = ctx.globals();
    globals.set("self", globals.clone())?;
    Ok(())
}

// https://w3c.github.io/webappsec-csp/#effective-directive-for-a-request
// https://w3c.github.io/webappsec-csp/#script-pre-request
// https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model
fn worker_script_blocked(creator: &World, worker: crate::worker::WorkerId, url: &url::Url) -> bool {
    let doc = creator.doc();
    let violations = creator.effective_csp().check_request(
        &doc.origin,
        &doc.document_url,
        url,
        crate::csp::Destination::Worker,
        // https://w3c.github.io/webappsec-csp/#script-src-strict-dynamic
        Some(crate::csp::ScriptRequest {
            nonce: "",
            parser_inserted: false,
            integrity: "",
        }),
    );
    drop(doc);
    if violations.is_empty() {
        return false;
    }
    let blocked = crate::csp::blocks(&violations);
    creator.queue_worker_blocked(worker, violations);
    blocked
}

// https://html.spec.whatwg.org/multipage/workers.html#dom-worker
fn install_worker_constructor_hosts(ctx: &Ctx<'_>, world: &Rc<RefCell<World>>) -> Result<()> {
    let host = crate::js::bridge::object(ctx)?;
    let url_world = Rc::clone(world);
    host.set(
        "__tbParseWorkerURL",
        rquickjs::prelude::Func::from(move |href: String, base: String| -> Option<String> {
            let world = url_world.borrow();
            worker_script_url(&world, &href, &base)
        }),
    )?;
    let create_world = Rc::clone(world);
    host.set(
        "__tbWorkerCreate",
        rquickjs::prelude::Func::from(
            move |ctx: Ctx<'_>,
                  url: String,
                  name: String,
                  module: bool,
                  credentials: String|
                  -> Result<u64> {
                let parsed = url::Url::parse(&url)
                    .map_err(|_| Exception::throw_type(&ctx, "Invalid worker script URL"))?;
                let credentials = net::cors::CredentialsMode::from_idl(&credentials)
                    .ok_or_else(|| Exception::throw_type(&ctx, "Invalid worker credentials"))?;
                let current = create_world.borrow();
                let id = current.runtime.next_worker_id();
                if worker_script_blocked(&current, id, &parsed) {
                    return Ok(id.get());
                }
                let created = crate::worker::CreatedWorker::from_creator(
                    &current,
                    id,
                    parsed,
                    name,
                    crate::worker::CreatedWorkerOptions::dedicated(module, credentials),
                );
                // https://html.spec.whatwg.org/multipage/workers.html#dom-worker
                current.runtime.worker_dir.spawn_worker(created);
                Ok(id.get())
            },
        ),
    )?;
    // https://html.spec.whatwg.org/multipage/workers.html#dom-sharedworker
    install_shared_worker_constructor_host(ctx, world)?;
    let post_world = Rc::clone(world);
    host.set(
        "__tbWorkerPost",
        rquickjs::prelude::Func::from(
            move |ctx: Ctx<'_>, id: u64, payload: String, ports: Vec<f64>| {
                let Some(ports) = messaging::accept_worker_transfer(&ctx, &payload, ports) else {
                    return;
                };
                let world = post_world.borrow();
                // https://html.spec.whatwg.org/multipage/web-messaging.html#transfer-receiving-steps
                {
                    let shared_handle = world.shared();
                    let mut shared = shared_handle.borrow_mut();
                    for endpoint in &ports {
                        shared
                            .ports
                            .route(*endpoint, crate::protocol::FrameId::worker(id));
                    }
                }
                // https://html.spec.whatwg.org/multipage/web-messaging.html#transfer-receiving-steps
                if !world.runtime.worker_dir.send(
                    crate::worker::WorkerId::new(id),
                    crate::worker::WorkerCommand::Post {
                        payload,
                        ports: ports.clone(),
                    },
                ) {
                    world.shared().borrow_mut().forget_endpoints(&ports);
                }
            },
        ),
    )?;
    let terminate_world = Rc::clone(world);
    host.set(
        "__tbWorkerTerminate",
        rquickjs::prelude::Func::from(move |id: u64| {
            terminate_world
                .borrow()
                .runtime
                .worker_dir
                .terminate(crate::worker::WorkerId::new(id));
        }),
    )?;
    Ok(())
}

// https://html.spec.whatwg.org/multipage/workers.html#dom-sharedworker
fn install_shared_worker_constructor_host(ctx: &Ctx<'_>, world: &Rc<RefCell<World>>) -> Result<()> {
    let host = crate::js::bridge::object(ctx)?;
    let shared_world = Rc::clone(world);
    host.set(
        "__tbSharedWorkerCreate",
        rquickjs::prelude::Func::from(
            move |ctx: Ctx<'_>,
                  url: String,
                  name: String,
                  module: bool,
                  credentials: String,
                  extended_lifetime: bool,
                  same_site_cookies: Option<String>|
                  -> Result<Vec<f64>> {
                let parsed = url::Url::parse(&url)
                    .map_err(|_| Exception::throw_type(&ctx, "Invalid worker script URL"))?;
                let credentials = net::cors::CredentialsMode::from_idl(&credentials)
                    .ok_or_else(|| Exception::throw_type(&ctx, "Invalid worker credentials"))?;
                let requested = match same_site_cookies.as_deref() {
                    None => None,
                    Some(value) => Some(
                        crate::worker::SharedWorkerSameSiteCookies::from_idl(value)
                            .ok_or_else(|| {
                                Exception::throw_type(&ctx, "Invalid sameSiteCookies value")
                            })?,
                    ),
                };
                let current = shared_world.borrow();
                let first_party = current.doc().first_party;
                // https://privacycg.github.io/saa-non-cookie-storage/#shared-worker
                let same_site_cookies = match requested {
                    Some(crate::worker::SharedWorkerSameSiteCookies::All) if !first_party => {
                        return Err(throw_dom(
                            &ctx,
                            "InvalidStateError",
                            "SharedWorker sameSiteCookies 'all' requires a first-party site context",
                        ));
                    }
                    Some(value) => value,
                    None if first_party => crate::worker::SharedWorkerSameSiteCookies::All,
                    None => crate::worker::SharedWorkerSameSiteCookies::None,
                };
                let id = current.runtime.next_worker_id();
                let (frame, shared) = (current.frame(), current.shared());
                if worker_script_blocked(&current, id, &parsed) {
                    let (page_endpoint, worker_endpoint) =
                        shared.borrow_mut().ports.new_pair(frame);
                    shared.borrow_mut().forget_endpoints(&[worker_endpoint]);
                    return Ok(vec![
                        crate::js::js_number(id.get()),
                        crate::js::js_number(page_endpoint),
                    ]);
                }
                let created = crate::worker::CreatedWorker::from_creator(
                    &current,
                    id,
                    parsed,
                    name,
                    crate::worker::CreatedWorkerOptions {
                        module,
                        shared: true,
                        credentials,
                        extended_lifetime,
                        same_site_cookies,
                    },
                );
                let (page_endpoint, worker_endpoint) = shared.borrow_mut().ports.new_pair(frame);
                // https://html.spec.whatwg.org/multipage/web-messaging.html#transfer-receiving-steps
                if !shared.borrow_mut().ports.detach(worker_endpoint, frame) {
                    return Err(Exception::throw_type(&ctx, "Cannot transfer MessagePort"));
                }
                current.runtime.push_worker_request_waking(
                    crate::worker::WorkerRequest::CreateShared {
                        created: Box::new(created),
                        endpoint: worker_endpoint,
                    },
                );
                Ok(vec![
                    crate::js::js_number(id.get()),
                    crate::js::js_number(page_endpoint),
                ])
            },
        ),
    )
}

// https://html.spec.whatwg.org/multipage/workers.html#dom-worker
fn worker_script_url(world: &World, href: &str, base: &str) -> Option<String> {
    let base = url::Url::parse(base).ok()?;
    let encoding = world.main_document().map_or(encoding_rs::UTF_8, |parsed| {
        encoding_rs::Encoding::for_label(parsed.character_set.as_bytes())
            .unwrap_or(encoding_rs::UTF_8)
    });
    crate::js::url_parts::encoding_parse_url(href, &base, encoding)
        .map(|url| url.as_str().to_owned())
}

fn install_worker_global_hosts(ctx: &Ctx<'_>, world: &Rc<RefCell<World>>) -> Result<()> {
    let host = crate::js::bridge::object(ctx)?;
    let name = world.borrow().worker_name().unwrap_or_default().to_owned();
    let url = world.borrow().doc().document_url.to_string();
    host.set("__tbIsWorker", true)?;
    host.set("__tbIsSharedWorker", world.borrow().is_shared_worker())?;
    host.set("__tbWorkerName", name)?;
    host.set("__tbWorkerURL", url)?;
    host.set("__tbWorkerModule", world.borrow().worker_is_module())?;
    // https://html.spec.whatwg.org/multipage/workers.html#dom-worker
    install_worker_constructor_hosts(ctx, world)?;
    let unhandled_world = Rc::clone(world);
    host.set(
        "__tbWorkerUnhandled",
        rquickjs::prelude::Func::from(
            move |message: String, filename: String, lineno: u32, colno: u32| {
                let world = unhandled_world.borrow();
                let Some(worker) = world.worker_id() else {
                    return;
                };
                let Some(parent) = world.worker_parent() else {
                    return;
                };
                world
                    .runtime
                    .push_worker_request_waking(crate::worker::WorkerRequest::Error(Box::new(
                        crate::worker::WorkerError {
                            worker,
                            parent,
                            message,
                            filename,
                            lineno,
                            colno,
                        },
                    )));
            },
        ),
    )?;
    let parent_world = Rc::clone(world);
    host.set(
        "__tbWorkerPostToParent",
        rquickjs::prelude::Func::from(move |ctx: Ctx<'_>, payload: String, ports: Vec<f64>| {
            let Some(ports) = messaging::accept_worker_transfer(&ctx, &payload, ports) else {
                return;
            };
            let world = parent_world.borrow();
            let Some(worker) = world.worker_id() else {
                return;
            };
            let Some(parent) = world.worker_parent() else {
                return;
            };
            world
                .runtime
                .push_worker_request_waking(crate::worker::WorkerRequest::PostToParent {
                    parent,
                    worker,
                    payload,
                    ports,
                });
        }),
    )?;
    let close_world = Rc::clone(world);
    host.set(
        "__tbWorkerClose",
        rquickjs::prelude::Func::from(move || {
            let world = close_world.borrow();
            if world.worker_id().is_some() {
                // https://html.spec.whatwg.org/multipage/workers.html#close-a-worker
                world.worker_close_requested.set(true);
            }
        }),
    )?;
    Ok(())
}

fn install_common(ctx: &Ctx<'_>, world: &Rc<RefCell<World>>) -> Result<()> {
    register_world(ctx, world);
    crate::js::bridge::install(ctx, world)?;
    let globals = ctx.globals();
    world
        .borrow_mut()
        .set_window(Persistent::save(ctx, globals.clone()));
    // https://dom.spec.whatwg.org/#dom-event-timestamp
    let performance_now: f64 = globals
        .get::<_, Object>("performance")
        .and_then(|performance| performance.get::<_, Function>("now"))
        .and_then(|now| now.call::<_, f64>(()))
        .unwrap_or(0.0);
    world.borrow().win_mut().event_clock = (std::time::Instant::now(), performance_now);
    Class::<JsEvent>::define(&globals)?;
    crate::js::bridge::evaluate(ctx, events::install_event_ctor_js(ctx)?)?;
    crate::js::bridge::object(ctx)?.set(
        "__tb_new_custom_event",
        rquickjs::prelude::Func::from(events::construct_custom_event),
    )?;
    crate::js::bridge::object(ctx)?.set(
        "__tb_init_custom_event",
        rquickjs::prelude::Func::from(events::init_custom_event),
    )?;
    crate::js::bridge::evaluate(ctx, events::install_custom_event_js(ctx)?)?;
    crate::js::bridge::object(ctx)?.set(
        "__tb_new_security_policy_violation_event",
        rquickjs::prelude::Func::from(events::construct_security_policy_violation_event),
    )?;
    crate::js::bridge::evaluate(
        ctx,
        events::install_security_policy_violation_event_js(ctx)?,
    )?;
    Class::<JsEventTarget>::define(&globals)?;
    events::install_event_target_bridge(ctx)?;
    crate::js::bridge::evaluate(ctx, events::install_event_target_ctor_js(ctx)?)?;
    Class::<JsDomException>::define(&globals)?;
    crate::js::bridge::evaluate(ctx, events::install_abort_js(ctx)?)?;
    install_host_functions(ctx)?;
    capture_host_primitives(ctx, &globals, world)?;
    globals.set(
        "addEventListener",
        rquickjs::prelude::Func::from(window_add_event_listener),
    )?;
    globals.set(
        "removeEventListener",
        rquickjs::prelude::Func::from(window_remove_event_listener),
    )?;
    globals.set(
        "dispatchEvent",
        rquickjs::prelude::Func::from(window_dispatch_event),
    )?;
    crate::js::bridge::object(ctx)?.set(
        "__tbDispatchTrusted",
        rquickjs::prelude::Func::from(window_dispatch_trusted_event),
    )?;
    Ok(())
}

fn install_document(ctx: &Ctx<'_>, world: &Rc<RefCell<World>>) -> Result<()> {
    let globals = ctx.globals();
    install_webdriver_bridge(ctx)?;
    // https://drafts.csswg.org/cssom-view/#dom-window-innerwidth
    Class::<JsNode>::define(&globals)?;
    Class::<JsNodeList>::define(&globals)?;
    Class::<JsHtmlCollection>::define(&globals)?;
    Class::<JsOptionsCollection>::define(&globals)?;
    Class::<JsImplementation>::define(&globals)?;
    Class::<JsTokenList>::define(&globals)?;
    Class::<JsNamedNodeMap>::define(&globals)?;
    forms::install(ctx, &crate::js::bridge::object(ctx)?)?;
    Class::<JsDomParser>::define(&globals)?;
    crate::js::bridge::evaluate(ctx, parsing::install_domparser_ctor_js(ctx)?)?;
    Class::<JsXmlSerializer>::define(&globals)?;
    Class::<JsMutationObserver>::define(&globals)?;
    Class::<JsRange>::define(&globals)?;
    Class::<JsStaticRange>::define(&globals)?;
    Class::<JsMutationRecord>::define(&globals)?;
    node::install_custom_construction(ctx)?;
    crate::js::bridge::object(ctx)?.set("__tb_handlerNames", HANDLER_ATTRIBUTES.to_vec())?;
    crate::js::bridge::object(ctx)?
        .set("__tb_windowHandlerNames", WINDOW_HANDLER_NAMES.to_vec())?;
    crate::js::bridge::object(ctx)?
        .set("__tb_documentHandlerNames", DOCUMENT_HANDLER_NAMES.to_vec())?;
    crate::js::bridge::object(ctx)?
        .set("__tb_bodyWindowHandlers", BODY_WINDOW_HANDLERS.to_vec())?;
    crate::js::bridge::object(ctx)?.set(
        "__tbGetNodeHandler",
        rquickjs::prelude::Func::from(get_node_handler),
    )?;
    crate::js::bridge::object(ctx)?.set(
        "__tbSetNodeHandler",
        rquickjs::prelude::Func::from(set_node_handler),
    )?;
    crate::js::bridge::object(ctx)?.set(
        "__tbGetWindowReflectingHandler",
        rquickjs::prelude::Func::from(get_window_reflecting_handler),
    )?;
    crate::js::bridge::object(ctx)?.set(
        "__tbSetWindowReflectingHandler",
        rquickjs::prelude::Func::from(set_window_reflecting_handler),
    )?;
    crate::js::bridge::object(ctx)?.set(
        "__tbGetWindowHandler",
        rquickjs::prelude::Func::from(get_window_handler),
    )?;
    crate::js::bridge::object(ctx)?.set(
        "__tbSetWindowHandler",
        rquickjs::prelude::Func::from(set_window_handler),
    )?;
    install_brands(ctx)?;
    node::install(ctx)?;
    range::install(ctx)?;
    Class::<JsAttr>::define(&globals)?;
    install_collection_brand(ctx)?;

    let document_id = world.borrow().with_main_document(|parsed| NodeId {
        document: parsed.id,
        node: parsed.document.base.root_node().id,
    });
    if let Some(id) = document_id {
        globals.set("document", wrap_node(ctx, id)?)?;
        // https://html.spec.whatwg.org/multipage/webappapis.html#event-handler-content-attributes
        attributes::install_parsed_handlers(ctx, id)?;
    }

    install_location(ctx, &globals, world)?;
    globals.set("window", globals.clone())?;
    globals.set("self", globals.clone())?;
    // https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-frames
    globals.set("frames", globals.clone())?;
    globals.set("opener", Value::new_null(ctx.clone()))?;
    crate::js::bridge::object(ctx)?.set("__tbIsWorker", false)?;
    install_worker_constructor_hosts(ctx, world)?;
    Ok(())
}

fn install_host_functions(ctx: &Ctx<'_>) -> Result<()> {
    crate::js::bridge::object(ctx)?.set("__tbIdentity", rquickjs::prelude::Func::from(identity))?;
    crate::js::bridge::object(ctx)?.set(
        "__tb_deliver_mutations",
        rquickjs::prelude::Func::from(deliver_mutations),
    )?;
    crate::js::bridge::object(ctx)?.set(
        "__tb_viewport_size",
        rquickjs::prelude::Func::from(viewport_size),
    )?;
    crate::js::bridge::object(ctx)?.set(
        "__tb_construct",
        rquickjs::prelude::Func::from(construct_node),
    )?;
    crate::js::bridge::object(ctx)?.set(
        "__tb_construct_in_frame",
        rquickjs::prelude::Func::from(construct_node_in_frame),
    )?;
    crate::js::bridge::object(ctx)?.set(
        "__tb_resolved_style",
        rquickjs::prelude::Func::from(resolved_style),
    )?;
    crate::js::bridge::object(ctx)?.set(
        "__tb_has_computed_style",
        rquickjs::prelude::Func::from(has_computed_style),
    )?;
    crate::js::bridge::object(ctx)?.set(
        "__tb_parse_pseudo_element",
        rquickjs::prelude::Func::from(parse_pseudo_element),
    )?;
    crate::js::bridge::object(ctx)?.set(
        "__tb_computed_style_names",
        rquickjs::prelude::Func::from(computed_style_names),
    )?;
    crate::js::bridge::object(ctx)?.set(
        "__tbMarkHTMLDDA",
        rquickjs::prelude::Func::from(mark_html_dda),
    )?;
    node::scrolling::install(ctx)?;
    crate::js::bridge::object(ctx)?.set(
        "__tbSetHTMLUnsafe",
        rquickjs::prelude::Func::from(node::element::set_html_unsafe_host),
    )?;
    crate::js::bridge::object(ctx)?.set(
        "__tbInsertHTMLUnsafe",
        rquickjs::prelude::Func::from(node::element::insert_html_unsafe_host),
    )?;
    parsing::install_xhr_parse(ctx)?;
    parsing::install_document_parse(ctx)?;
    cssom_host::install(ctx)?;
    origin::install(ctx)?;
    selection::install(ctx)?;
    Ok(())
}

// https://drafts.csswg.org/cssom/#resolved-values
fn resolved_style<'js>(
    ctx: Ctx<'js>,
    element: Value<'js>,
    property: rquickjs::String<'js>,
    pseudo: i32,
) -> String {
    let Ok(world) = world(&ctx) else {
        return String::new();
    };
    let Some(id) = host_node_id(&ctx, &element) else {
        return String::new();
    };
    let property = property.to_string().unwrap_or_default();
    let world = world.borrow();
    let Some(mut parsed) = world.document_mut(id) else {
        return String::new();
    };
    let base = &mut parsed.document.base;
    // https://drafts.csswg.org/cssom/#dom-window-getcomputedstyle
    base.resolve_if_needed(world.runtime.animation_seconds());
    match computed_pseudo_element(pseudo) {
        Some(pseudo) => base.resolved_pseudo_style_value(id.node, pseudo, &property),
        None if pseudo != 0 => String::new(),
        None => base.resolved_style_value(id.node, &property),
    }
}

fn computed_pseudo_element(code: i32) -> Option<style::selector_parser::PseudoElement> {
    use style::selector_parser::PseudoElement;
    match code {
        1 => Some(PseudoElement::Before),
        2 => Some(PseudoElement::After),
        3 => Some(PseudoElement::Selection),
        4 => Some(PseudoElement::FirstLine),
        5 => Some(PseudoElement::FirstLetter),
        _ => None,
    }
}

// https://drafts.csswg.org/selectors-4/#typedef-pseudo-element-selector
fn parse_pseudo_element(text: String) -> i32 {
    use blitz_dom::ComputedPseudoElement;
    use style::selector_parser::PseudoElement;
    match blitz_dom::parse_computed_pseudo_element(&text) {
        ComputedPseudoElement::Cascaded(PseudoElement::Before) => 1,
        ComputedPseudoElement::Cascaded(PseudoElement::After) => 2,
        ComputedPseudoElement::Cascaded(PseudoElement::Selection) => 3,
        ComputedPseudoElement::Cascaded(PseudoElement::FirstLine) => 4,
        ComputedPseudoElement::Cascaded(PseudoElement::FirstLetter) => 5,
        ComputedPseudoElement::Unsupported => 6,
        ComputedPseudoElement::Cascaded(_) | ComputedPseudoElement::Invalid => -1,
    }
}

// https://drafts.csswg.org/cssom/#dom-cssstyledeclaration-length
fn computed_style_names<'js>(
    ctx: Ctx<'js>,
    element: Value<'js>,
    pseudo: i32,
) -> Result<Array<'js>> {
    let array = Array::new(ctx.clone())?;
    let Ok(world) = world(&ctx) else {
        return Ok(array);
    };
    let Some(id) = host_node_id(&ctx, &element) else {
        return Ok(array);
    };
    let world = world.borrow();
    let Some(parsed) = world.document(id) else {
        return Ok(array);
    };
    let names = parsed
        .document
        .base
        .computed_style_property_names(id.node, computed_pseudo_element(pseudo));
    for (index, name) in names.into_iter().enumerate() {
        array.set(index, name)?;
    }
    Ok(array)
}

// https://html.spec.whatwg.org/multipage/common-dom-interfaces.html#htmlallcollection
fn mark_html_dda(object: rquickjs::Object<'_>) {
    object.set_is_html_dda();
}

// https://drafts.csswg.org/cssom/#dom-window-getcomputedstyle
fn has_computed_style<'js>(ctx: Ctx<'js>, element: Value<'js>) -> bool {
    let Ok(world) = world(&ctx) else {
        return false;
    };
    let Some(id) = host_node_id(&ctx, &element) else {
        return false;
    };
    {
        let world = world.borrow();
        let Some(mut parsed) = world.document_mut(id) else {
            return false;
        };
        let base = &mut parsed.document.base;
        // https://drafts.csswg.org/cssom/#dom-window-getcomputedstyle
        base.resolve_if_needed(world.runtime.animation_seconds());
        if !crate::dom::tree::is_connected(base, id.node)
            || !crate::dom::tree::is_in_flat_tree(base, id.node)
        {
            return false;
        }
    }
    // https://dom.spec.whatwg.org/#concept-document-bc
    let Some(owner) = owning_world(&ctx, id) else {
        return false;
    };
    let has_context = owner.borrow().main_document_id() == Some(id.document_id());
    if !has_context {
        return false;
    }
    // https://html.spec.whatwg.org/multipage/rendering.html#being-rendered
    let Some(container) = owner.borrow().container() else {
        return true;
    };
    let world = world.borrow();
    let Some(mut parsed) = world.document_mut(container) else {
        return false;
    };
    let base = &mut parsed.document.base;
    base.resolve_if_needed(world.runtime.animation_seconds());
    base.get_node(container.node).is_some_and(|node| {
        node.try_stylo_element_data()
            .as_ref()
            .and_then(|data| data.get())
            .and_then(|data| {
                data.styles
                    .get_primary()
                    .map(|styles| styles.get_box().get_display().is_none())
            })
            .is_some_and(|display_none| !display_none)
    })
}

// https://drafts.csswg.org/cssom-view/#dom-window-innerwidth
fn viewport_size(ctx: Ctx<'_>) -> Vec<f64> {
    let (width, height) = world(&ctx)
        .ok()
        .map_or(crate::engine::DEFAULT_VIEWPORT, |world| {
            world.borrow().win().viewport_size.get()
        });
    vec![f64::from(width), f64::from(height)]
}

// https://wicg.github.io/ua-client-hints/#http-ua-hints
fn identity(ctx: Ctx<'_>) -> Result<Object<'_>> {
    let identity = world(&ctx)?.borrow().runtime.identity.borrow().clone();
    let object = Object::new(ctx.clone())?;
    object.set("userAgent", identity.user_agent.as_str().to_owned())?;
    object.set("platform", identity.platform.clone())?;
    object.set("platformVersion", identity.platform_version.clone())?;
    object.set("architecture", identity.architecture.clone())?;
    object.set("model", identity.model.clone())?;
    object.set("mobile", identity.mobile)?;
    object.set("bitness", identity.bitness.clone())?;
    object.set("wow64", identity.wow64)?;
    object.set("formFactors", identity.form_factors.clone())?;
    object.set("navigatorPlatform", identity.navigator_platform.clone())?;
    object.set("languages", identity.languages.clone())?;
    object.set("vendor", identity.vendor.clone())?;
    object.set("productSub", identity.product_sub.clone())?;
    object.set("geckoCompat", identity.is_gecko_compatibility_mode())?;
    object.set("brands", brand_array(&ctx, &identity.brands)?)?;
    object.set(
        "fullVersionList",
        brand_array(&ctx, &identity.full_version_list)?,
    )?;
    Ok(object)
}

fn brand_array<'js>(ctx: &Ctx<'js>, brands: &[net::Brand]) -> Result<Array<'js>> {
    let array = Array::new(ctx.clone())?;
    for (index, brand) in brands.iter().enumerate() {
        let entry = Object::new(ctx.clone())?;
        entry.set("brand", brand.brand.clone())?;
        entry.set("version", brand.version.clone())?;
        array.set(index, entry)?;
    }
    Ok(array)
}

fn capture_host_primitives<'js>(
    ctx: &Ctx<'js>,
    globals: &Object<'js>,
    world: &Rc<RefCell<World>>,
) -> Result<()> {
    let number: Function = globals.get("Number")?;
    let boolean: Function = globals.get("Boolean")?;
    let eval_error: Function = globals.get("EvalError")?;
    let reflect: Object = globals.get("Reflect")?;
    let reflect_set: Function = reflect.get("set")?;
    let deliver: Function = crate::js::bridge::object(ctx)?.get("__tb_deliver_mutations")?;
    let weak_ref: Constructor = globals.get("WeakRef")?;
    let weak_ref_prototype: Object = weak_ref.get("prototype")?;
    let weak_ref_deref: Function = weak_ref_prototype.get("deref")?;
    let mut world = world.borrow_mut();
    world.weak_references = Some(WeakReferences {
        constructor: Persistent::save(ctx, weak_ref),
        deref: Persistent::save(ctx, weak_ref_deref),
    });
    world.pristine_number = Some(Persistent::save(ctx, number));
    world.pristine_boolean = Some(Persistent::save(ctx, boolean));
    world.pristine_eval_error = Some(Persistent::save(ctx, eval_error));
    world.pristine_reflect_set = Some(Persistent::save(ctx, reflect_set));
    world.pristine_queue_microtask = globals
        .get::<_, Function>("queueMicrotask")
        .ok()
        .map(|queue| Persistent::save(ctx, queue));
    world.deliver_mutations_fn = Some(Persistent::save(ctx, deliver));
    Ok(())
}

pub(crate) fn host_node_id<'js>(ctx: &Ctx<'js>, value: &Value<'js>) -> Option<NodeId> {
    Class::<JsNode>::from_js(ctx, value.clone())
        .ok()
        .map(|node| node.borrow().node_id())
}

// https://w3c.github.io/webdriver/#elements
pub(super) fn webdriver_element(ctx: Ctx<'_>, remote_id: f64) -> Result<Value<'_>> {
    let Some(remote) = crate::js::bridge::enforce_range_u64(remote_id) else {
        return Ok(Value::new_null(ctx));
    };
    let world = world(&ctx)?;
    let Some(node) = world.borrow().node_for_remote(remote) else {
        return Ok(Value::new_null(ctx));
    };
    let valid = world.borrow().document(node).is_some_and(|parsed| {
        let base = &parsed.document.base;
        crate::dom::tree::is_connected(base, node.node)
            && base.get_node(node.node).is_some_and(|node| {
                matches!(
                    node.data,
                    NodeData::Element(_) | NodeData::ShadowRoot { .. }
                )
            })
    });
    if !valid {
        return Ok(Value::new_null(ctx));
    }
    wrap_node(&ctx, node)
}

pub(crate) use node::element::{bounding_rect, element_client_rects};

// https://drafts.csswg.org/cssom-view/#dom-document-elementfrompoint
#[expect(
    clippy::cast_possible_truncation,
    reason = "Blitz layout is f32; a point inside the viewport is exactly representable"
)]
pub(crate) fn element_at_point(world: &World, document: NodeId, x: f64, y: f64) -> Option<NodeId> {
    let mut parsed = world.document_mut(document)?;
    crate::render::resolve_until_settled(
        &mut parsed.document.base,
        world.runtime.animation_seconds(),
    );
    parsed
        .document
        .base
        .element_from_point(x as f32, y as f32)
        .map(|node| NodeId {
            document: document.document,
            node,
        })
}

// https://drafts.csswg.org/cssom-view/#dom-document-elementsfrompoint
#[expect(
    clippy::cast_possible_truncation,
    reason = "Blitz layout is f32; a point inside the viewport is exactly representable"
)]
pub(crate) fn elements_at_point(world: &World, document: NodeId, x: f64, y: f64) -> Vec<NodeId> {
    let Some(mut parsed) = world.document_mut(document) else {
        return Vec::new();
    };
    crate::render::resolve_until_settled(
        &mut parsed.document.base,
        world.runtime.animation_seconds(),
    );
    parsed
        .document
        .base
        .elements_from_point(x as f32, y as f32)
        .into_iter()
        .map(|node| NodeId {
            document: document.document,
            node,
        })
        .collect()
}

pub(super) fn element_at_point_ctx(
    ctx: &Ctx<'_>,
    document: NodeId,
    x: f64,
    y: f64,
) -> Result<Option<NodeId>> {
    let world = world_for_node(ctx, document)?;
    Ok(element_at_point(&world.borrow(), document, x, y))
}

pub(super) fn rect_object<'js>(
    ctx: &Ctx<'js>,
    left: f64,
    top: f64,
    width: f64,
    height: f64,
) -> Result<Object<'js>> {
    let object = Object::new(ctx.clone())?;
    object.set("x", left)?;
    object.set("y", top)?;
    object.set("width", width)?;
    object.set("height", height)?;
    object.set("top", top)?;
    object.set("left", left)?;
    object.set("right", left + width)?;
    object.set("bottom", top + height)?;
    Ok(object)
}

// https://dom.spec.whatwg.org/#concept-node-adopt
pub(super) fn retarget_wrapper(ctx: &Ctx<'_>, from: NodeId, to: NodeId) -> Result<()> {
    let world_rc = world(ctx)?;
    if let Some(source) = world_rc.borrow().owner_world(from) {
        world_rc
            .borrow()
            .registry()
            .borrow_mut()
            .stamp_creation_realm(from, &source);
    }
    world_rc
        .borrow()
        .registry()
        .borrow_mut()
        .rekey_wrapper(from, to);
    world_rc.borrow_mut().rekey_input_files(from, to);
    let Some(saved) = world(ctx)?.borrow().shared_wrapper(to) else {
        return Ok(());
    };
    let Ok(Some(value)) = deref_weak(ctx, saved) else {
        return Ok(());
    };
    let Ok(class) = Class::<JsNode>::from_js(ctx, value) else {
        return Ok(());
    };
    class.borrow().handle.set(Handle(to));
    Ok(())
}

pub(crate) fn wrap_node<'js>(ctx: &Ctx<'js>, id: NodeId) -> Result<Value<'js>> {
    let world_rc = world(ctx)?;
    if let Some(saved) = world_rc.borrow().shared_wrapper(id)
        && let Some(value) = deref_weak(ctx, saved)?
    {
        return Ok(value);
    }
    let value = instantiate_node(ctx, id)?;
    let weak = make_weak(ctx, value.clone())?;
    world_rc
        .borrow()
        .intern_shared_wrapper(id, Persistent::save(ctx, weak));
    Ok(value)
}

pub(super) fn wrap_new_document<'js>(ctx: &Ctx<'js>, parsed: crate::Parsed) -> Result<Value<'js>> {
    wrap_new_document_in_world(ctx, parsed, &world(ctx)?)
}

// https://dom.spec.whatwg.org/#create-a-document
pub(super) fn wrap_new_document_in_world<'js>(
    ctx: &Ctx<'js>,
    parsed: crate::Parsed,
    world_rc: &Rc<RefCell<World>>,
) -> Result<Value<'js>> {
    let root = world_rc.borrow_mut().add_document(parsed);
    let registry = world_rc.borrow().registry();
    registry
        .borrow_mut()
        .insert_document(root.document_id(), world_rc);
    wrap_node(ctx, root)
}

fn instantiate_node<'js>(ctx: &Ctx<'js>, id: NodeId) -> Result<Value<'js>> {
    let brand = with_node_data(ctx, id, |data| match data {
        Some(NodeData::Document(_)) => Some(if document_is_xml_document(ctx, id) {
            "XMLDocument"
        } else if document_is_html_content(ctx, id) {
            "HTMLDocument"
        } else {
            "Document"
        }),
        Some(NodeData::Fragment { .. }) => Some("DocumentFragment"),
        Some(NodeData::ShadowRoot { .. }) => Some("ShadowRoot"),
        Some(NodeData::Element(element)) => Some(element_interface(&element.name)),
        Some(NodeData::Text(_)) => Some("Text"),
        Some(NodeData::CDataSection { .. }) => Some("CDATASection"),
        Some(NodeData::Comment { .. }) => Some("Comment"),
        Some(NodeData::ProcessingInstruction { .. }) => Some("ProcessingInstruction"),
        Some(NodeData::Doctype { .. }) => Some("DocumentType"),
        _ => None,
    })?;
    let Some(brand) = brand else {
        return Err(Exception::throw_type(ctx, "stale node"));
    };
    wrap_with_brand(ctx, id, brand)
}

fn wrap_with_brand<'js>(ctx: &Ctx<'js>, id: NodeId, brand: &str) -> Result<Value<'js>> {
    let value = JsNode {
        handle: std::cell::Cell::new(Handle(id)),
        legacy: std::cell::Cell::new(node::LegacyShape::of_brand(brand)),
    };
    let class = if let Some(prototype) = node_wrapper_prototype(ctx, id, brand)? {
        host::instance_proto(value, prototype)?
    } else {
        let prototype = Class::<JsNode>::prototype(ctx)?
            .ok_or_else(|| Exception::throw_internal(ctx, "native Node class has no prototype"))?;
        host::instance_proto(value, prototype)?
    };
    Ok(Class::into_value(class))
}

// https://dom.spec.whatwg.org/#concept-node-adopt
// https://github.com/whatwg/dom/issues/977
// https://html.spec.whatwg.org/multipage/iframe-embed-object.html#the-iframe-element:html-element-insertion-steps
fn node_wrapper_prototype<'js>(
    ctx: &Ctx<'js>,
    id: NodeId,
    brand: &str,
) -> Result<Option<Object<'js>>> {
    let world_rc = world(ctx)?;
    if world_rc.borrow().is_isolated() {
        return world_rc
            .borrow()
            .brand(brand)
            .map(|proto| proto.restore(ctx))
            .transpose();
    }
    let owner = world_rc.borrow().owner_world(id);
    if let Some(owner) = owner.as_ref() {
        world_rc
            .borrow()
            .registry()
            .borrow_mut()
            .stamp_creation_realm(id, owner);
    }
    let creator = world_rc.borrow().registry().borrow().creation_realm(id);
    let proto = creator
        .as_ref()
        .and_then(|creator| creator.borrow().brand(brand))
        .or_else(|| owner.as_ref().and_then(|owner| owner.borrow().brand(brand)))
        .or_else(|| world_rc.borrow().brand(brand));
    proto.map(|proto| proto.restore(ctx)).transpose()
}

// https://html.spec.whatwg.org/multipage/dom.html#elements-in-the-dom:html-element
// https://w3c.github.io/mathml-core/#dom-mathmlelement
fn element_interface(name: &QualName) -> &'static str {
    if name.ns == crate::dom::tree::html_namespace() {
        html_element_interface(name.local.as_str())
    } else if name.ns == crate::dom::tree::svg_namespace() {
        "SVGElement"
    } else if name.ns == crate::dom::tree::mathml_namespace() {
        "MathMLElement"
    } else {
        "Element"
    }
}

// https://html.spec.whatwg.org/multipage/dom.html#elements-in-the-dom:html-element
const ELEMENT_INTERFACES: &[(&str, &str)] = &[
    ("a", "HTMLAnchorElement"),
    ("abbr", "HTMLElement"),
    ("acronym", "HTMLElement"),
    ("address", "HTMLElement"),
    ("area", "HTMLAreaElement"),
    ("article", "HTMLElement"),
    ("aside", "HTMLElement"),
    ("audio", "HTMLAudioElement"),
    ("b", "HTMLElement"),
    ("base", "HTMLBaseElement"),
    ("bdi", "HTMLElement"),
    ("bdo", "HTMLElement"),
    ("bgsound", "HTMLElement"),
    ("big", "HTMLElement"),
    ("blockquote", "HTMLQuoteElement"),
    ("body", "HTMLBodyElement"),
    ("br", "HTMLBRElement"),
    ("button", "HTMLButtonElement"),
    ("canvas", "HTMLCanvasElement"),
    ("caption", "HTMLTableCaptionElement"),
    ("center", "HTMLElement"),
    ("cite", "HTMLElement"),
    ("code", "HTMLElement"),
    ("col", "HTMLTableColElement"),
    ("colgroup", "HTMLTableColElement"),
    ("data", "HTMLDataElement"),
    ("datalist", "HTMLDataListElement"),
    ("dd", "HTMLElement"),
    ("del", "HTMLModElement"),
    ("details", "HTMLDetailsElement"),
    ("dfn", "HTMLElement"),
    ("dialog", "HTMLDialogElement"),
    ("dir", "HTMLDirectoryElement"),
    ("div", "HTMLDivElement"),
    ("dl", "HTMLDListElement"),
    ("dt", "HTMLElement"),
    ("embed", "HTMLEmbedElement"),
    ("fieldset", "HTMLFieldSetElement"),
    ("figcaption", "HTMLElement"),
    ("figure", "HTMLElement"),
    ("font", "HTMLFontElement"),
    ("footer", "HTMLElement"),
    ("form", "HTMLFormElement"),
    ("frame", "HTMLFrameElement"),
    ("frameset", "HTMLFrameSetElement"),
    ("h1", "HTMLHeadingElement"),
    ("h2", "HTMLHeadingElement"),
    ("h3", "HTMLHeadingElement"),
    ("h4", "HTMLHeadingElement"),
    ("h5", "HTMLHeadingElement"),
    ("h6", "HTMLHeadingElement"),
    ("head", "HTMLHeadElement"),
    ("header", "HTMLElement"),
    ("hgroup", "HTMLElement"),
    ("hr", "HTMLHRElement"),
    ("html", "HTMLHtmlElement"),
    ("i", "HTMLElement"),
    ("iframe", "HTMLIFrameElement"),
    ("img", "HTMLImageElement"),
    ("input", "HTMLInputElement"),
    ("ins", "HTMLModElement"),
    ("isindex", "HTMLElement"),
    ("kbd", "HTMLElement"),
    ("label", "HTMLLabelElement"),
    ("legend", "HTMLLegendElement"),
    ("li", "HTMLLIElement"),
    ("link", "HTMLLinkElement"),
    ("listing", "HTMLPreElement"),
    ("main", "HTMLElement"),
    ("map", "HTMLMapElement"),
    ("mark", "HTMLElement"),
    ("marquee", "HTMLMarqueeElement"),
    ("menu", "HTMLMenuElement"),
    ("meta", "HTMLMetaElement"),
    ("meter", "HTMLMeterElement"),
    ("nav", "HTMLElement"),
    ("nobr", "HTMLElement"),
    ("noframes", "HTMLElement"),
    ("noscript", "HTMLElement"),
    ("object", "HTMLObjectElement"),
    ("ol", "HTMLOListElement"),
    ("optgroup", "HTMLOptGroupElement"),
    ("option", "HTMLOptionElement"),
    ("output", "HTMLOutputElement"),
    ("p", "HTMLParagraphElement"),
    ("param", "HTMLParamElement"),
    ("picture", "HTMLPictureElement"),
    ("pre", "HTMLPreElement"),
    ("progress", "HTMLProgressElement"),
    ("q", "HTMLQuoteElement"),
    ("rp", "HTMLElement"),
    ("rt", "HTMLElement"),
    ("ruby", "HTMLElement"),
    ("s", "HTMLElement"),
    ("samp", "HTMLElement"),
    ("script", "HTMLScriptElement"),
    ("section", "HTMLElement"),
    ("select", "HTMLSelectElement"),
    ("selectedcontent", "HTMLSelectedContentElement"),
    ("small", "HTMLElement"),
    ("source", "HTMLSourceElement"),
    ("slot", "HTMLSlotElement"),
    ("spacer", "HTMLElement"),
    ("span", "HTMLSpanElement"),
    ("strike", "HTMLElement"),
    ("style", "HTMLStyleElement"),
    ("sub", "HTMLElement"),
    ("summary", "HTMLElement"),
    ("sup", "HTMLElement"),
    ("table", "HTMLTableElement"),
    ("tbody", "HTMLTableSectionElement"),
    ("td", "HTMLTableCellElement"),
    ("template", "HTMLTemplateElement"),
    ("textarea", "HTMLTextAreaElement"),
    ("tfoot", "HTMLTableSectionElement"),
    ("th", "HTMLTableCellElement"),
    ("thead", "HTMLTableSectionElement"),
    ("time", "HTMLTimeElement"),
    ("title", "HTMLTitleElement"),
    ("tr", "HTMLTableRowElement"),
    ("track", "HTMLTrackElement"),
    ("tt", "HTMLElement"),
    ("u", "HTMLElement"),
    ("ul", "HTMLUListElement"),
    ("unknown", "HTMLUnknownElement"),
    ("var", "HTMLElement"),
    ("video", "HTMLVideoElement"),
    ("wbr", "HTMLElement"),
    ("xmp", "HTMLPreElement"),
];

// https://html.spec.whatwg.org/multipage/dom.html#htmlmediaelement
pub(super) const ELEMENT_INTERFACE_PARENTS: &[(&str, &str)] = &[
    ("HTMLAudioElement", "HTMLMediaElement"),
    ("HTMLVideoElement", "HTMLMediaElement"),
];

pub(super) fn html_element_interface(local: &str) -> &'static str {
    if let Some((_, interface)) = ELEMENT_INTERFACES.iter().find(|(name, _)| *name == local) {
        return interface;
    }
    if local.contains('-') {
        return "HTMLElement";
    }
    "HTMLUnknownElement"
}

pub(super) fn make_weak<'js>(ctx: &Ctx<'js>, target: Value<'js>) -> Result<Value<'js>> {
    let ctor = world(ctx)?
        .borrow()
        .weak_references
        .as_ref()
        .ok_or_else(|| Exception::throw_internal(ctx, "weak references are not initialized"))?
        .constructor
        .clone()
        .restore(ctx)?;
    ctor.construct((target,))
}

pub(super) fn deref_weak<'js>(
    ctx: &Ctx<'js>,
    saved: Persistent<Value<'static>>,
) -> Result<Option<Value<'js>>> {
    let weak = saved.restore(ctx)?;
    let object = weak
        .as_object()
        .ok_or_else(|| Exception::throw_type(ctx, "weak wrapper"))?;
    let deref = world(ctx)?
        .borrow()
        .weak_references
        .as_ref()
        .ok_or_else(|| Exception::throw_internal(ctx, "weak references are not initialized"))?
        .deref
        .clone()
        .restore(ctx)?;
    let value: Value = deref.call((This(object.clone()),))?;
    if value.is_undefined() {
        Ok(None)
    } else {
        Ok(Some(value))
    }
}

const INSTALL_BRANDS_DEFLATE: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/js_blobs/brands.deflate"));

fn install_brands_js(ctx: &Ctx<'_>) -> Result<&'static str> {
    static CACHE: std::sync::OnceLock<Box<str>> = std::sync::OnceLock::new();
    super::blob::decompress(ctx, INSTALL_BRANDS_DEFLATE, &CACHE)
}

fn install_brands(ctx: &Ctx<'_>) -> Result<()> {
    crate::js::bridge::evaluate(ctx, install_brands_js(ctx)?)?;
    let table: Object = crate::js::bridge::object(ctx)?.get("__tb_brandTable")?;
    let entries = table
        .props::<String, Object>()
        .collect::<Result<Vec<(String, Object)>>>()?;
    // https://html.spec.whatwg.org/multipage/dom.html#elements-in-the-dom
    let defined: std::collections::HashSet<&str> =
        entries.iter().map(|(name, _)| name.as_str()).collect();
    let missing: std::collections::BTreeSet<&str> = ELEMENT_INTERFACES
        .iter()
        .map(|(_, interface)| *interface)
        .filter(|interface| !defined.contains(interface))
        .collect();
    if !missing.is_empty() {
        let message = format!("brands: missing element prototypes: {missing:?}");
        return Err(Exception::throw_internal(ctx, message.as_str()));
    }
    for (name, proto) in entries {
        world(ctx)?
            .borrow_mut()
            .intern_brand(name, Persistent::save(ctx, proto));
    }
    crate::js::bridge::object(ctx)?.remove("__tb_brandTable")?;
    Ok(())
}

pub(super) const INSTALL_COLLECTIONS_DEFLATE: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/js_blobs/collections.deflate"));

pub(super) fn install_collections_js(ctx: &Ctx<'_>) -> Result<&'static str> {
    static CACHE: std::sync::OnceLock<Box<str>> = std::sync::OnceLock::new();
    super::blob::decompress(ctx, INSTALL_COLLECTIONS_DEFLATE, &CACHE)
}

pub(crate) fn try_world(ctx: &Ctx<'_>) -> Option<Rc<RefCell<World>>> {
    REALM_WORLDS.with(|worlds| {
        worlds
            .borrow()
            .get(&(ctx.as_raw().as_ptr() as usize))
            .and_then(Weak::upgrade)
    })
}

pub(crate) fn world(ctx: &Ctx<'_>) -> Result<Rc<RefCell<World>>> {
    try_world(ctx).ok_or_else(|| Exception::throw_internal(ctx, "missing JS world"))
}

pub(crate) fn world_for_node(ctx: &Ctx<'_>, id: NodeId) -> Result<Rc<RefCell<World>>> {
    owning_world(ctx, id).ok_or_else(|| Exception::throw_type(ctx, "stale node"))
}

pub(crate) fn owning_world(ctx: &Ctx<'_>, id: NodeId) -> Option<Rc<RefCell<World>>> {
    realm_registry(ctx).ok()?.borrow().owner_world(id)
}

// https://drafts.csswg.org/cssom-view/#dom-element-scrolltop
pub(crate) fn active_world(ctx: &Ctx<'_>, id: NodeId) -> Option<Rc<RefCell<World>>> {
    let world = owning_world(ctx, id)?;
    if world.borrow().main_document_id() != Some(id.document) {
        return None;
    }
    Some(world)
}

pub(super) fn node_is_shadow_root(ctx: &Ctx<'_>, id: NodeId) -> Result<bool> {
    with_node_data(ctx, id, |data| {
        matches!(data, Some(NodeData::ShadowRoot { .. }))
    })
}

pub(super) fn with_node_data<T>(
    ctx: &Ctx<'_>,
    id: NodeId,
    read: impl FnOnce(Option<&NodeData>) -> T,
) -> Result<T> {
    let owner = world_for_node(ctx, id)?;
    let parsed = owner.borrow();
    let Some(parsed) = parsed.document(id) else {
        return Err(Exception::throw_type(ctx, "no document"));
    };
    Ok(read(
        parsed
            .document
            .base
            .get_node(id.node)
            .map(|node| &node.data),
    ))
}

pub(crate) fn character_data(ctx: &Ctx<'_>, id: NodeId) -> Result<crate::dom_string::DomString> {
    let owner = world_for_node(ctx, id)?;
    let parsed = owner.borrow();
    let Some(parsed) = parsed.document(id) else {
        return Err(Exception::throw_type(ctx, "no document"));
    };
    Ok(parsed.document.character_data(id.node))
}

pub(super) fn attribute_value(ctx: &Ctx<'_>, id: NodeId, local: &str) -> Result<String> {
    let owner = world_for_node(ctx, id)?;
    Ok(owner
        .borrow()
        .document(id)
        .and_then(|parsed| {
            crate::dom::tree::attr(&parsed.document.base, id.node, local).map(str::to_owned)
        })
        .unwrap_or_default())
}

// https://dom.spec.whatwg.org/#concept-cd-replace
pub(super) fn replace_character_data(
    ctx: &Ctx<'_>,
    id: NodeId,
    offset: u32,
    count: u32,
    data: &crate::dom_string::DomString,
) -> Result<()> {
    replace_character_data_inner(ctx, id, offset, count, data)
}

pub(super) fn set_character_data(
    ctx: &Ctx<'_>,
    id: NodeId,
    data: &crate::dom_string::DomString,
) -> Result<()> {
    replace_character_data_inner(ctx, id, 0, u32::MAX, data)
}

// https://dom.spec.whatwg.org/#update-data-from-attributes
pub(super) fn set_pi_attributes(
    ctx: &Ctx<'_>,
    id: NodeId,
    attributes: Vec<(String, String)>,
) -> Result<()> {
    let owner = world_for_node(ctx, id)?;
    let owner = owner.borrow();
    let Some(mut parsed) = owner.document_mut(id) else {
        return Ok(());
    };
    if !parsed
        .document
        .base
        .get_node(id.node)
        .is_some_and(|node| matches!(node.data, NodeData::ProcessingInstruction { .. }))
    {
        return Ok(());
    }
    let old_value = parsed.document.character_data(id.node);
    let old_length = old_value.units().len();
    let data = crate::dom_string::DomString::from(
        parsed
            .document
            .base
            .mutate()
            .set_processing_instruction_attributes(id.node, attributes),
    );
    parsed.document.remember_character_data(id.node, &data);
    crate::dom::range::adjust_for_replace_data(
        &mut parsed.document.side.ranges,
        id,
        0,
        u32::try_from(old_length).unwrap_or(u32::MAX),
        u32::try_from(data.units().len()).unwrap_or(u32::MAX),
    );
    parsed
        .document
        .record(crate::documents::JournalEntry::CharacterData {
            target: id,
            old_value,
        });
    drop(parsed);
    drop(owner);
    schedule_mutation_delivery(ctx)
}

fn write_character_data(base: &mut blitz_dom::BaseDocument, id: NodeId, utf8: &str) -> bool {
    let Some(node) = base.get_node(id.node) else {
        return false;
    };
    match &node.data {
        NodeData::Text(_) => {
            base.mutate().set_node_text(id.node, utf8);
            true
        }
        NodeData::Comment { .. } => {
            base.snapshot_node_state_only(id.node);
            base.get_node_mut(id.node)
                .and_then(|node| match &mut node.data {
                    NodeData::Comment { contents } => Some(contents),
                    _ => None,
                })
                .is_some_and(|contents| {
                    contents.clear();
                    contents.push_str(utf8);
                    true
                })
        }
        NodeData::ProcessingInstruction { .. } => {
            base.snapshot_node_state_only(id.node);
            // https://www.w3.org/TR/xml-stylesheet/#dt-parsing
            base.mutate().set_processing_instruction_data(id.node, utf8);
            true
        }
        NodeData::CDataSection { .. } => {
            base.snapshot_node_state_only(id.node);
            base.get_node_mut(id.node)
                .and_then(|node| match &mut node.data {
                    NodeData::CDataSection { contents } => Some(contents),
                    _ => None,
                })
                .is_some_and(|contents| {
                    contents.clear();
                    contents.push_str(utf8);
                    true
                })
        }
        _ => false,
    }
}

fn replace_character_data_inner(
    ctx: &Ctx<'_>,
    id: NodeId,
    offset: u32,
    count: u32,
    data: &crate::dom_string::DomString,
) -> Result<()> {
    let owner = world_for_node(ctx, id)?;
    let owner = owner.borrow();
    let Some(mut parsed) = owner.document_mut(id) else {
        return Ok(());
    };
    let old_value = {
        let is_character_data = parsed.document.base.get_node(id.node).is_some_and(|node| {
            matches!(
                node.data,
                NodeData::Text(_)
                    | NodeData::Comment { .. }
                    | NodeData::ProcessingInstruction { .. }
                    | NodeData::CDataSection { .. }
            )
        });
        if !is_character_data {
            return Ok(());
        }
        parsed.document.character_data(id.node)
    };
    // https://dom.spec.whatwg.org/#concept-cd-replace
    let old_units = old_value.units().into_owned();
    let offset = (offset as usize).min(old_units.len());
    let count = (count as usize).min(old_units.len() - offset);
    let replacement = data.units().into_owned();
    let mut units = old_units;
    units.splice(offset..offset + count, replacement.iter().copied());
    let exact = crate::dom_string::DomString::from_utf16(units);
    let utf8 = exact.to_string_lossy().into_owned();
    if !write_character_data(&mut parsed.document.base, id, &utf8) {
        return Ok(());
    }
    // https://infra.spec.whatwg.org/#javascript-string-convert
    parsed.document.remember_character_data(id.node, &exact);
    crate::dom::range::adjust_for_replace_data(
        &mut parsed.document.side.ranges,
        id,
        u32::try_from(offset).unwrap_or(u32::MAX),
        u32::try_from(count).unwrap_or(u32::MAX),
        u32::try_from(replacement.len()).unwrap_or(u32::MAX),
    );
    parsed
        .document
        .record(crate::documents::JournalEntry::CharacterData {
            target: id,
            old_value,
        });
    drop(parsed);
    drop(owner);
    schedule_mutation_delivery(ctx)
}

// https://dom.spec.whatwg.org/#concept-cd-substring
pub(crate) fn character_data_offset(ctx: &Ctx<'_>, offset: u32, length: usize) -> Result<usize> {
    let offset = usize::try_from(offset).unwrap_or(usize::MAX);
    if offset > length {
        return Err(throw_dom(
            ctx,
            "IndexSizeError",
            "offset is outside the data",
        ));
    }
    Ok(offset)
}

pub(super) fn required_node<'js>(ctx: &Ctx<'js>, value: &Value<'js>) -> Result<NodeId> {
    host_node_id(ctx, value).ok_or_else(|| Exception::throw_type(ctx, "argument is not a Node"))
}

pub(super) fn child_value<'js>(ctx: &Ctx<'js>, id: Option<NodeId>) -> Result<Value<'js>> {
    match id {
        Some(id) => wrap_node(ctx, id),
        None => Ok(Value::new_null(ctx.clone())),
    }
}

pub(super) fn sibling_value<'js>(ctx: &Ctx<'js>, id: NodeId, forward: bool) -> Result<Value<'js>> {
    let world = world(ctx)?;
    let sibling = world.borrow().document(id).and_then(|parsed| {
        sibling(&parsed.document.base, id.node, forward).map(|node| NodeId {
            document: id.document,
            node,
        })
    });
    child_value(ctx, sibling)
}

// https://dom.spec.whatwg.org/#dom-nondocumenttypechildnode-nextelementsibling
pub(super) fn element_sibling_value<'js>(
    ctx: &Ctx<'js>,
    id: NodeId,
    forward: bool,
) -> Result<Value<'js>> {
    let world = world(ctx)?;
    let found = {
        let parsed = world.borrow();
        let Some(parsed) = parsed.document(id) else {
            return Ok(Value::new_null(ctx.clone()));
        };
        let base = &parsed.document.base;
        let mut cursor = sibling(base, id.node, forward);
        while let Some(next) = cursor {
            if is_element(&parsed.document.base, next) {
                break;
            }
            cursor = sibling(base, next, forward);
        }
        cursor.map(|node| NodeId {
            document: id.document,
            node,
        })
    };
    child_value(ctx, found)
}

pub(super) fn string_value<'js>(ctx: &Ctx<'js>, text: &str) -> Result<Value<'js>> {
    Ok(rquickjs::String::from_str(ctx.clone(), text)?.into_value())
}

pub(crate) fn refresh_document_global(ctx: &Ctx<'_>) -> Result<()> {
    let world = world(ctx)?;
    let document_id = world.borrow().with_main_document(|parsed| NodeId {
        document: parsed.id,
        node: parsed.document.base.root_node().id,
    });
    if let Some(id) = document_id {
        ctx.globals().set("document", wrap_node(ctx, id)?)?;
        // https://html.spec.whatwg.org/multipage/webappapis.html#event-handler-content-attributes
        attributes::install_parsed_handlers(ctx, id)?;
    }
    Ok(())
}

pub(crate) fn dom_string<'js>(
    ctx: &Ctx<'js>,
    value: &crate::dom_string::DomString,
) -> Result<rquickjs::String<'js>> {
    rquickjs::String::from_utf16(ctx.clone(), &value.units())
}

// https://dom.spec.whatwg.org/#validate-and-extract
#[derive(Clone, Copy)]
pub(super) enum NodeContext {
    Attribute,
    Element,
}

// https://dom.spec.whatwg.org/#validate-and-extract
pub(super) fn validate_and_extract(
    ctx: &Ctx<'_>,
    namespace: Option<&str>,
    qualified: &str,
    context: NodeContext,
) -> Result<QualName> {
    let namespace = namespace.filter(|namespace| !namespace.is_empty());
    let (prefix, local) = match qualified.split_once(':') {
        Some((prefix, local)) => {
            if !valid_namespace_prefix(prefix) {
                return Err(throw_dom(
                    ctx,
                    "InvalidCharacterError",
                    "invalid namespace prefix",
                ));
            }
            (Some(prefix), local)
        }
        None => (None, qualified),
    };
    let local_valid = match context {
        NodeContext::Attribute => valid_attribute_local_name(local),
        NodeContext::Element => valid_element_local_name(local),
    };
    if !local_valid {
        return Err(throw_dom(
            ctx,
            "InvalidCharacterError",
            "invalid local name",
        ));
    }
    let xml = "http://www.w3.org/XML/1998/namespace";
    let xmlns = "http://www.w3.org/2000/xmlns/";
    if prefix.is_some() && namespace.is_none() {
        return Err(throw_dom(ctx, "NamespaceError", "prefix without namespace"));
    }
    if prefix == Some("xml") && namespace != Some(xml) {
        return Err(throw_dom(
            ctx,
            "NamespaceError",
            "xml prefix with wrong namespace",
        ));
    }
    if (qualified == "xmlns" || prefix == Some("xmlns")) && namespace != Some(xmlns) {
        return Err(throw_dom(
            ctx,
            "NamespaceError",
            "xmlns name with wrong namespace",
        ));
    }
    if namespace == Some(xmlns) && qualified != "xmlns" && prefix != Some("xmlns") {
        return Err(throw_dom(
            ctx,
            "NamespaceError",
            "xmlns namespace without xmlns name",
        ));
    }
    Ok(QualName::new(
        prefix.map(Prefix::from),
        Namespace::from(namespace.unwrap_or("")),
        LocalName::from(local),
    ))
}

// https://dom.spec.whatwg.org/#valid-namespace-prefix
fn valid_namespace_prefix(prefix: &str) -> bool {
    !prefix.is_empty()
        && !prefix
            .chars()
            .any(|c| is_infra_whitespace(c) || matches!(c, '\0' | '/' | '>'))
}

// https://dom.spec.whatwg.org/#valid-attribute-local-name
pub(super) fn valid_attribute_local_name(local: &str) -> bool {
    !local.is_empty()
        && !local
            .chars()
            .any(|c| is_infra_whitespace(c) || matches!(c, '\0' | '/' | '=' | '>'))
}

// https://dom.spec.whatwg.org/#valid-element-local-name
pub(crate) fn valid_element_local_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if first.is_ascii_alphabetic() {
        return !name
            .chars()
            .any(|c| is_infra_whitespace(c) || matches!(c, '\0' | '/' | '>'));
    }
    if first != ':' && first != '_' && u32::from(first) < 0x80 {
        return false;
    }
    name.chars().all(|c| {
        c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | ':' | '_') || u32::from(c) >= 0x80
    })
}

fn is_infra_whitespace(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\u{c}' | '\r' | ' ')
}

pub(super) fn elements_by_tag<'js>(
    ctx: &Ctx<'js>,
    scope: NodeId,
    name: &str,
) -> Result<Value<'js>> {
    live_collection(
        ctx,
        scope,
        CollectionKind::ElementsByTag {
            name: name.to_owned(),
            html_document: document_is_html_content(ctx, scope),
        },
        Some("HTMLCollection"),
    )
}

pub(super) fn live_collection<'js>(
    ctx: &Ctx<'js>,
    scope: NodeId,
    kind: CollectionKind,
    brand: Option<&str>,
) -> Result<Value<'js>> {
    let query = CollectionQuery {
        scope: Handle(scope),
        kind,
    };
    match brand {
        None => match &query.kind {
            CollectionKind::Static(handles) => static_node_list(ctx, handles),
            _ => Ok(Class::into_value(host::instance_in_world(
                ctx,
                JsNodeList { query },
            )?)),
        },
        Some("HTMLCollection") => Ok(Class::into_value(host::instance_in_world(
            ctx,
            JsHtmlCollection { query },
        )?)),
        Some("HTMLOptionsCollection") => Ok(Class::into_value(host::instance_in_world(
            ctx,
            JsOptionsCollection { query },
        )?)),
        Some(_) => Err(Exception::throw_type(
            ctx,
            "unsupported collection interface",
        )),
    }
}

pub(super) fn find_element_by_id(
    base: &blitz_dom::BaseDocument,
    document: u32,
    scope: crate::dom::tree::BlitzId,
    id: &str,
) -> Option<NodeId> {
    // https://dom.spec.whatwg.org/#concept-id
    if id.is_empty() {
        return None;
    }
    let mut stack = vec![scope];
    while let Some(current) = stack.pop() {
        let Some(node) = base.get_node(current) else {
            continue;
        };
        if node.data.downcast_element().is_some_and(|element| {
            element
                .attr(markup5ever::LocalName::from("id"))
                .is_some_and(|value| value == id)
        }) {
            return Some(NodeId {
                document,
                node: current,
            });
        }
        stack.extend(node.children.iter().rev().copied());
    }
    None
}

#[cfg(test)]
mod realm_tests {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::Arc;

    use rquickjs::{Persistent, Value};
    use url::Url;

    use super::{world, wrap_node};
    use crate::dom::tree::NodeId;
    use crate::js::Stop;
    use crate::js::{JsRealm, SharedJsRuntime, World};
    use crate::protocol::{BrowserServices, FrameId};
    use crate::test_services::NullServices;

    fn world_with_document(
        js_runtime: &SharedJsRuntime,
        services: &Arc<dyn BrowserServices>,
        documents: &Rc<RefCell<crate::documents::DocumentStore>>,
        registry: &Rc<RefCell<crate::js::RealmRegistry>>,
        url: &str,
        html: &str,
    ) -> Rc<RefCell<World>> {
        let identity = net::ValidIdentity::default();
        let common = Arc::new(crate::js::SharedRuntime::new(
            Arc::clone(services),
            identity.clone(),
            Arc::new(tokio::sync::Notify::new()),
            Arc::new(Stop::new()),
            crate::Fonts::default().0,
        ));
        let worker_dir = Arc::new(crate::worker::WorkerDirectory::new(Arc::clone(&common)));
        let runtime = crate::js::FrameRuntime {
            common,
            identity: Rc::new(RefCell::new(identity)),
            js_runtime: js_runtime.handle(),
            documents: Rc::clone(documents),
            registry: Rc::clone(registry),
            pending_storage: Rc::new(RefCell::new(Vec::new())),
            turn_deadline: Rc::new(std::cell::Cell::new(None)),
            worker_dir,
            font_ctx: crate::Fonts::default().0,
        };
        let mut world = World::new(
            Url::parse(url).expect("test url"),
            FrameId::MAIN,
            &runtime,
            Rc::new(RefCell::new(Vec::new())),
            Rc::new(RefCell::new(Vec::new())),
        );
        let id = world.replace_document(crate::parse_html(
            html,
            blitz_dom::DocumentConfig {
                font_ctx: Some(crate::Fonts::default().0),
                ..blitz_dom::DocumentConfig::default()
            },
        ));
        let world = Rc::new(RefCell::new(world));
        registry.borrow_mut().insert_document(id, &world);
        registry.borrow_mut().insert_frame(FrameId::MAIN, &world);
        world
    }

    #[test]
    fn canvas_measure_text_freezes_alignment_and_rejects_foreign_clusters() {
        let services: Arc<dyn BrowserServices> = Arc::new(NullServices::default());
        let shared = SharedJsRuntime::default();
        let stop = Arc::new(Stop::new());
        let documents = Rc::new(RefCell::new(crate::documents::DocumentStore::default()));
        let registry = Rc::new(RefCell::new(crate::js::RealmRegistry::default()));
        let world = world_with_document(
            &shared,
            &services,
            &documents,
            &registry,
            "https://a.test/",
            "<!doctype html><canvas id=c width=100 height=100></canvas>",
        );
        let realm = JsRealm::new(&shared.handle(), world, stop).expect("realm");
        let outcome = realm
            .eval(
                r"
                var ctx = document.getElementById('c').getContext('2d');
                ctx.font = '40px serif';
                ctx.textAlign = 'center';
                var tm = ctx.measureText('E');
                var width = tm.width;
                var frozen = tm.getTextClusters()[0].x;
                var right = tm.getTextClusters({align: 'right'})[0].x;
                ctx.textAlign = 'left';
                var later = tm.getTextClusters()[0].x;

                var reentered = true;
                try {
                  tm.getTextClusters({ get align() { ctx.fillRect(0, 0, 1, 1); return 'left'; } });
                } catch (e) { reentered = false; }

                var rejected = false;
                try { ctx.fillTextCluster({start: 0, end: 1, x: 0, y: 0}, 0, 0); }
                catch (e) { rejected = e instanceof TypeError; }

                [
                  Math.abs(frozen) < 0.5,
                  Math.abs(right - width / 2) < 0.5,
                  frozen === later,
                  reentered,
                  typeof ctx.fillTextCluster === 'function' && rejected,
                  typeof TextCluster === 'function',
                  typeof TextMetrics === 'function',
                  tm instanceof TextMetrics,
                  tm.getTextClusters()[0] instanceof TextCluster,
                  TextCluster.prototype.hasOwnProperty('begin'),
                  TextCluster.prototype.hasOwnProperty('align'),
                  Object.prototype.toString.call(tm) === '[object TextMetrics]',
                  TextMetrics.name === 'TextMetrics',
                ].join(',');
                ",
            )
            .expect("canvas metrics");
        assert_eq!(
            outcome,
            "true,true,true,true,true,true,true,true,true,true,true,true,true"
        );
    }

    #[test]
    fn canvas_serialization_converts_options_in_rust() {
        let services: Arc<dyn BrowserServices> = Arc::new(NullServices::default());
        let shared = SharedJsRuntime::default();
        let stop = Arc::new(Stop::new());
        let documents = Rc::new(RefCell::new(crate::documents::DocumentStore::default()));
        let registry = Rc::new(RefCell::new(crate::js::RealmRegistry::default()));
        let world = world_with_document(
            &shared,
            &services,
            &documents,
            &registry,
            "https://a.test/",
            "<!doctype html><canvas id=c width=4 height=4></canvas>",
        );
        let realm = JsRealm::new(&shared.handle(), world, stop).expect("realm");
        realm
            .eval(
                r"
                var canvas = document.getElementById('c');
                canvas.getContext('2d').fillRect(0, 0, 4, 4);

                var valueOfRan = false;
                canvas.toDataURL('image/jpeg', { valueOf() { valueOfRan = true; return 0.5; } });

                var off = new OffscreenCanvas(4, 4);
                off.getContext('2d').fillRect(0, 0, 4, 4);
                var order = [];
                var qualityConversions = 0;
                var typeConversions = 0;
                var options = {};
                Object.defineProperty(options, 'quality', {
                  get() { order.push('quality'); return { valueOf() { qualityConversions++; return 0.5; } }; },
                });
                Object.defineProperty(options, 'type', {
                  get() { order.push('type'); return { toString() { typeConversions++; return 'image/png'; } }; },
                });
                var first = off.convertToBlob(options);
                globalThis.__order = order.join(',');
                globalThis.__conversions = qualityConversions + ',' + typeConversions;
                globalThis.__valueOfRan = valueOfRan;
                globalThis.__first = first;

                globalThis.Blob = function () { throw new Error('page Blob'); };
                globalThis.setTimeout = function () { throw new Error('page setTimeout'); };
                canvas.toBlob(function () {}, 'image/png');
                off.convertToBlob({ type: 'image/png' }).then(function (blob) {
                  globalThis.__blob = { size: blob.size, tag: Object.prototype.toString.call(blob) };
                }, function (error) {
                  globalThis.__blob = { error: String(error) };
                });
                'ok'
                ",
            )
            .expect("canvas serialization");
        let outcome = realm
            .eval(
                r"
                [
                  __valueOfRan,
                  __order,
                  __conversions,
                  __first instanceof Promise,
                  __blob && __blob.size > 0,
                  __blob && __blob.tag,
                ].join('|')
                ",
            )
            .expect("canvas serialization result");
        assert_eq!(outcome, "false|quality,type|1,1|true|true|[object Blob]");
    }

    #[test]
    fn offscreen_canvas_needs_no_dom_node_and_exposes_no_bitmap_handle() {
        let services: Arc<dyn BrowserServices> = Arc::new(NullServices::default());
        let shared = SharedJsRuntime::default();
        let stop = Arc::new(Stop::new());
        let documents = Rc::new(RefCell::new(crate::documents::DocumentStore::default()));
        let registry = Rc::new(RefCell::new(crate::js::RealmRegistry::default()));
        let world = world_with_document(
            &shared,
            &services,
            &documents,
            &registry,
            "https://a.test/",
            "<!doctype html><p>offscreen</p>",
        );
        let realm = JsRealm::new(&shared.handle(), world, stop).expect("realm");
        realm
            .eval(
                r"
                var before = document.getElementsByTagName('*').length;
                var off = new OffscreenCanvas(4, 4);
                var after = document.getElementsByTagName('*').length;
                var context = off.getContext('2d');
                context.fillStyle = '#f00';
                context.fillRect(0, 0, 4, 4);
                var bitmap = off.transferToImageBitmap();
                var reader = document.createElement('canvas');
                reader.width = 4;
                reader.height = 4;
                var read = reader.getContext('2d');
                read.drawImage(bitmap, 0, 0);
                var pixel = read.getImageData(0, 0, 1, 1).data;
                var pending = new OffscreenCanvas(4, 4).convertToBlob();
                pending.then(function (blob) { globalThis.__contextless = blob.size > 0; },
                              function () { globalThis.__contextless = false; });
                var forged = document.createElement('canvas');
                forged.width = 4;
                forged.height = 4;
                var forgedContext = forged.getContext('2d');
                forgedContext.__tbCanvasId = 999999;
                var rejected = false;
                try { forgedContext.canvas; } catch (error) {
                  rejected = error.name === 'InvalidStateError';
                }
                globalThis.__probe = [
                  after - before,
                  '__tbCanvas' in off,
                  String(off.__tbCanvasId),
                  off.width,
                  pixel[0],
                  pixel[3],
                  rejected,
                ].join(',');
                'ok'
                ",
            )
            .expect("offscreen canvas");
        let outcome = realm
            .eval("[__probe, String(globalThis.__contextless)].join('|')")
            .expect("offscreen probe");
        assert_eq!(outcome, "0,false,undefined,4,255,255,true|true");
    }

    #[test]
    fn structured_clone_transfer_detaches_the_source_buffer() {
        let services: Arc<dyn BrowserServices> = Arc::new(NullServices::default());
        let shared = SharedJsRuntime::default();
        let stop = Arc::new(Stop::new());
        let documents = Rc::new(RefCell::new(crate::documents::DocumentStore::default()));
        let registry = Rc::new(RefCell::new(crate::js::RealmRegistry::default()));
        let world = world_with_document(
            &shared,
            &services,
            &documents,
            &registry,
            "https://a.test/",
            "<!doctype html><p>clone</p>",
        );
        let realm = JsRealm::new(&shared.handle(), world, stop).expect("realm");
        let outcome = realm
            .eval(
                r"
                var buffer = new ArrayBuffer(8);
                new Uint8Array(buffer).set([1, 2, 3, 4, 5, 6, 7, 8]);
                var copy = structuredClone(buffer, { transfer: [buffer] });
                var view = new Uint8Array(copy);
                [buffer.byteLength, copy.byteLength, view[3]].join('|')
                ",
            )
            .expect("structured clone");
        assert_eq!(
            outcome, "0|8|4",
            "a transferred buffer must be detached on the sender side"
        );
    }

    #[test]
    fn replacing_with_a_foreign_fragment_adopts_each_child() {
        let services: Arc<dyn BrowserServices> = Arc::new(NullServices::default());
        let shared = SharedJsRuntime::default();
        let stop = Arc::new(Stop::new());
        let documents = Rc::new(RefCell::new(crate::documents::DocumentStore::default()));
        let registry = Rc::new(RefCell::new(crate::js::RealmRegistry::default()));
        let world = world_with_document(
            &shared,
            &services,
            &documents,
            &registry,
            "https://a.test/",
            "<!doctype html><div id=target><b id=old></b></div>",
        );
        let realm = JsRealm::new(&shared.handle(), world, stop).expect("realm");
        let outcome = realm
            .eval(
                r#"
                var target = document.getElementById("target");
                var old = document.getElementById("old");
                var other = document.implementation.createHTMLDocument("");
                var fragment = other.createDocumentFragment();
                var span = other.createElement("span");
                span.textContent = "moved";
                fragment.appendChild(span);
                var replaced = target.replaceChild(fragment, old);
                [
                  replaced === old,
                  target.childNodes.length,
                  target.firstChild.nodeName,
                  target.firstChild.ownerDocument === document,
                  target.firstChild.textContent,
                  fragment.childNodes.length
                ].join(",");
                "#,
            )
            .expect("cross-document replace");
        assert_eq!(outcome, "true,1,SPAN,true,moved,0");
    }

    #[test]
    fn inserting_a_fragment_splices_its_children_in_tree_order() {
        let services: Arc<dyn BrowserServices> = Arc::new(NullServices::default());
        let shared = SharedJsRuntime::default();
        let stop = Arc::new(Stop::new());
        let documents = Rc::new(RefCell::new(crate::documents::DocumentStore::default()));
        let registry = Rc::new(RefCell::new(crate::js::RealmRegistry::default()));
        let world = world_with_document(
            &shared,
            &services,
            &documents,
            &registry,
            "https://a.test/",
            "<!doctype html><div id=target></div>",
        );
        let realm = JsRealm::new(&shared.handle(), world, stop).expect("realm");
        let outcome = realm
            .eval(
                r#"
                var target = document.getElementById("target");
                var fragment = document.createDocumentFragment();
                var first = document.createElement("i");
                var text = document.createTextNode("gap");
                var second = document.createElement("u");
                fragment.append(first, text, second);
                target.append(fragment);
                [
                  target.childNodes.length,
                  target.childNodes[0].nodeName,
                  target.childNodes[1].nodeValue,
                  target.childNodes[2].nodeName,
                  fragment.childNodes.length,
                  first.parentNode === target
                ].join(",");
                "#,
            )
            .expect("fragment insert");
        assert_eq!(outcome, "3,I,gap,U,0,true");
    }

    // https://html.spec.whatwg.org/multipage/webappapis.html#secure-contexts
    #[test]
    fn a_secure_context_gates_the_secure_context_only_surfaces() {
        let services: Arc<dyn BrowserServices> = Arc::new(NullServices::default());
        let shared = SharedJsRuntime::default();
        for (url, expected) in [
            ("https://a.test/", "true,function,true,true,function"),
            ("http://a.test/", "false,undefined,false,false,undefined"),
        ] {
            let stop = Arc::new(Stop::new());
            let documents = Rc::new(RefCell::new(crate::documents::DocumentStore::default()));
            let registry = Rc::new(RefCell::new(crate::js::RealmRegistry::default()));
            let world = world_with_document(
                &shared,
                &services,
                &documents,
                &registry,
                url,
                "<!doctype html><p></p>",
            );
            let realm = JsRealm::new(&shared.handle(), world, stop).expect("realm");
            let outcome = realm
                .eval(
                    r#"
                    [
                      String(isSecureContext),
                      typeof crypto.randomUUID,
                      String('randomUUID' in Crypto.prototype),
                      String('userAgentData' in navigator),
                      typeof NavigatorUAData
                    ].join(",");
                    "#,
                )
                .expect("secure context surfaces");
            assert_eq!(outcome, expected, "{url}");
        }
    }

    // https://w3c.github.io/webcrypto/#Crypto-interface
    #[test]
    fn crypto_is_one_interface_with_one_instance() {
        let services: Arc<dyn BrowserServices> = Arc::new(NullServices::default());
        let shared = SharedJsRuntime::default();
        let stop = Arc::new(Stop::new());
        let documents = Rc::new(RefCell::new(crate::documents::DocumentStore::default()));
        let registry = Rc::new(RefCell::new(crate::js::RealmRegistry::default()));
        let world = world_with_document(
            &shared,
            &services,
            &documents,
            &registry,
            "https://a.test/",
            "<!doctype html><p></p>",
        );
        let realm = JsRealm::new(&shared.handle(), world, stop).expect("realm");
        let outcome = realm
            .eval(
                "
                const results = [];
                const proto = Object.getPrototypeOf(crypto);
                const attempt = run => {
                  try { run(); results.push('no throw'); }
                  catch (error) { results.push(error.name + ': ' + error.message); }
                };
                results.push(globalThis.crypto === crypto);
                results.push(proto === Crypto.prototype);
                results.push(typeof proto.getRandomValues);
                results.push(String('randomUUID' in proto));
                const descriptor = Object.getOwnPropertyDescriptor(globalThis, 'crypto');
                results.push(String(typeof descriptor.get) + String(descriptor.set === undefined));
                attempt(() => new Crypto());
                attempt(() => Crypto.prototype.getRandomValues.call({}, new Uint8Array(1)));
                attempt(() => crypto.getRandomValues(new Float32Array(1)));
                attempt(() => crypto.getRandomValues(new DataView(new ArrayBuffer(1))));
                const oversized = new Uint8Array(70000);
                Object.defineProperty(oversized, 'byteLength', { value: 0 });
                attempt(() => crypto.getRandomValues(oversized));
                const bytes = new Uint8Array(4);
                results.push(String(crypto.getRandomValues(bytes) === bytes));
                const original = Crypto.prototype.getRandomValues;
                Crypto.prototype.getRandomValues = () => new Uint8Array(16);
                const uuid = crypto.randomUUID();
                Crypto.prototype.getRandomValues = original;
                results.push(String(/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(uuid)));
                results.push(String(uuid !== '00000000-0000-4000-8000-000000000000'));
                results.join(',');
                ",
            )
            .expect("crypto interface");
        assert_eq!(
            outcome,
            concat!(
                "true,true,function,true,functiontrue,",
                "TypeError: Illegal constructor,",
                "TypeError: Illegal invocation,",
                "TypeMismatchError: destination must be an integer TypedArray,",
                "TypeMismatchError: destination must be an integer TypedArray,",
                "QuotaExceededError: random byte quota exceeded,",
                "true,true,true"
            )
        );
    }

    // https://webidl.spec.whatwg.org/#internally-create-a-new-object-implementing-the-interface
    #[test]
    fn a_constructed_js_interface_gets_the_generated_prototype() {
        let services: Arc<dyn BrowserServices> = Arc::new(NullServices::default());
        let shared = SharedJsRuntime::default();
        let stop = Arc::new(Stop::new());
        let documents = Rc::new(RefCell::new(crate::documents::DocumentStore::default()));
        let registry = Rc::new(RefCell::new(crate::js::RealmRegistry::default()));
        let world = world_with_document(
            &shared,
            &services,
            &documents,
            &registry,
            "https://a.test/",
            "<!doctype html><p></p>",
        );
        let realm = JsRealm::new(&shared.handle(), world, stop).expect("realm");
        let outcome = realm
            .eval(
                "
                const encoder = new TextEncoder();
                const Subclass = class extends TextEncoder {};
                const sub = new Subclass();
                [
                  Object.getPrototypeOf(encoder) === TextEncoder.prototype,
                  encoder instanceof TextEncoder,
                  Object.getPrototypeOf(sub) === Subclass.prototype,
                  sub instanceof TextEncoder,
                  typeof encoder.encode
                ].join(',');
                ",
            )
            .expect("generated prototype");
        assert_eq!(outcome, "true,true,true,true,function");
    }

    // https://tc39.es/ecma262/#sec-evaluatecall
    // https://tc39.es/ecma262/#sec-eval-x
    #[test]
    fn evaluation_intrinsics_keep_spec_identity() {
        let services: Arc<dyn BrowserServices> = Arc::new(NullServices::default());
        let shared = SharedJsRuntime::default();
        let stop = Arc::new(Stop::new());
        let documents = Rc::new(RefCell::new(crate::documents::DocumentStore::default()));
        let registry = Rc::new(RefCell::new(crate::js::RealmRegistry::default()));
        let world = world_with_document(
            &shared,
            &services,
            &documents,
            &registry,
            "https://a.test/",
            "<!doctype html><p></p>",
        );
        let realm = JsRealm::new(&shared.handle(), world, stop).expect("realm");
        realm
            .eval(
                r#"
                if (eval.name !== "eval") throw new Error("eval.name=" + eval.name);
                if (eval.length !== 1) throw new Error("eval.length=" + eval.length);
                if ("prototype" in eval) throw new Error("eval has a prototype");
                var constructible = true;
                try {
                    new eval();
                } catch (error) {
                    if (error instanceof TypeError) constructible = false;
                    else throw error;
                }
                if (constructible) throw new Error("eval is a constructor");
                if (Function.prototype.constructor !== Function)
                    throw new Error("Function.prototype.constructor is not Function");
                var scope_probe = (function () {
                    var local = 1;
                    return eval("local");
                })();
                if (scope_probe !== 1) throw new Error("direct eval lost the local scope");
                "#,
            )
            .expect("evaluation intrinsics");
    }

    // https://w3c.github.io/webappsec-csp/#can-compile-strings
    #[test]
    fn evaluation_respects_content_security_policy() {
        let services: Arc<dyn BrowserServices> = Arc::new(NullServices::default());
        let shared = SharedJsRuntime::default();
        let stop = Arc::new(Stop::new());
        let documents = Rc::new(RefCell::new(crate::documents::DocumentStore::default()));
        let registry = Rc::new(RefCell::new(crate::js::RealmRegistry::default()));
        let world = world_with_document(
            &shared,
            &services,
            &documents,
            &registry,
            "https://a.test/",
            "<!doctype html><p></p>",
        );
        let policy = Rc::clone(&world);
        let realm = JsRealm::new(&shared.handle(), world, stop).expect("realm");

        policy.borrow().doc_mut().policy.csp =
            crate::csp::List::from_headers(Some("script-src 'self'"), None);
        let denied = realm
            .eval(
                r#"
                var outcome = [];
                try { eval("1"); outcome.push("eval ran"); }
                catch (error) { outcome.push(error.name); }
                try { Function("return 1")(); outcome.push("Function ran"); }
                catch (error) { outcome.push(error.name); }
                outcome.join(",");
                "#,
            )
            .expect("eval");
        assert_eq!(denied, "EvalError,EvalError");

        let spoofed = realm
            .eval(
                r#"
                var spoof_ran = false;
                globalThis.EvalError = function () { spoof_ran = true; return {}; };
                var outcome = [];
                try { eval("1"); outcome.push("ran"); }
                catch (error) { outcome.push(error.name); }
                outcome.push(spoof_ran);
                outcome.join(",");
                "#,
            )
            .expect("eval");
        assert_eq!(spoofed, "EvalError,false");

        policy.borrow().doc_mut().policy.csp =
            crate::csp::List::from_headers(Some("script-src 'self' 'unsafe-eval'"), None);
        let allowed = realm
            .eval(r#"[eval("1 + 1"), Function("return 2")()].join(",");"#)
            .expect("eval");
        assert_eq!(allowed, "2,2");
    }

    #[test]
    fn teardown_with_retained_custom_event() {
        let services: Arc<dyn BrowserServices> = Arc::new(NullServices::default());
        let shared = SharedJsRuntime::default();
        let stop = Arc::new(Stop::new());
        let documents = Rc::new(RefCell::new(crate::documents::DocumentStore::default()));
        let registry = Rc::new(RefCell::new(crate::js::RealmRegistry::default()));
        let world = world_with_document(
            &shared,
            &services,
            &documents,
            &registry,
            "https://a.test/",
            "<!doctype html><p></p>",
        );
        let realm = JsRealm::new(&shared.handle(), world, stop).expect("realm");
        realm
            .eval("window.ev = new CustomEvent('x', {detail: 1})")
            .expect("eval");
        drop(realm);
        drop(shared);
    }

    #[test]
    fn teardown_after_document_lifecycle() {
        let services: Arc<dyn BrowserServices> = Arc::new(NullServices::default());
        let shared = SharedJsRuntime::default();
        let stop = Arc::new(Stop::new());
        let documents = Rc::new(RefCell::new(crate::documents::DocumentStore::default()));
        let registry = Rc::new(RefCell::new(crate::js::RealmRegistry::default()));
        let world = world_with_document(
            &shared,
            &services,
            &documents,
            &registry,
            "https://a.test/",
            "<!doctype html><p>hello</p>",
        );
        let realm = JsRealm::new(&shared.handle(), world, stop).expect("realm");
        realm
            .eval("window.onload = function(){}; document.onreadystatechange = function(){};")
            .expect("eval");
        realm.fire_ready_state_change().expect("readystatechange");
        realm.fire_dom_content_loaded().expect("DOMContentLoaded");
        realm.fire_ready_state_change().expect("readystatechange 2");
        realm.fire_load().expect("load");
        drop(realm);
        drop(shared);
    }

    // https://webidl.spec.whatwg.org/#LegacyTreatNonObjectAsNull
    // https://w3c.github.io/uievents/#dom-keyboardevent-which
    #[test]
    fn event_handler_attributes_follow_their_conversions() {
        let services: Arc<dyn BrowserServices> = Arc::new(NullServices::default());
        let shared = SharedJsRuntime::default();
        let stop = Arc::new(Stop::new());
        let documents = Rc::new(RefCell::new(crate::documents::DocumentStore::default()));
        let registry = Rc::new(RefCell::new(crate::js::RealmRegistry::default()));
        let world = world_with_document(
            &shared,
            &services,
            &documents,
            &registry,
            "https://a.test/",
            "<!doctype html><p></p>",
        );
        let realm = JsRealm::new(&shared.handle(), world, stop).expect("realm");
        let eval = |source: &str| realm.eval(source).expect("eval");
        eval("window.div = document.createElement('div')");
        assert_eq!(
            eval("div.onanimationend === null && div.onwebkitanimationend === null"),
            "true",
            "a fresh element's handlers read null"
        );
        eval("div.onclick = 'a string clears the handler'");
        assert_eq!(
            eval("div.onclick === null"),
            "true",
            "a non-object assignment clears the handler"
        );
        eval("div.onanimationend = () => fired.push('plain'); window.fired = []");
        eval("div.onwebkitanimationend = () => fired.push('prefixed')");
        assert_eq!(
            eval(
                "div.onanimationend !== null && div.onwebkitanimationend !== null && div.onanimationend !== div.onwebkitanimationend"
            ),
            "true",
            "the unprefixed and prefixed handlers are separate slots"
        );
        eval("div.dispatchEvent(new AnimationEvent('webkitAnimationEnd'))");
        assert_eq!(
            eval("fired.join('+')"),
            "prefixed",
            "a prefixed event reaches only the prefixed handler"
        );
        assert_eq!(
            eval("new KeyboardEvent('x', { which: 7 }).which"),
            "7",
            "KeyboardEvent inherits the UIEvent which member"
        );
        assert_eq!(
            eval("new UIEvent('x', { which: '9' }).which"),
            "9",
            "which converts through unsigned long"
        );
        assert_eq!(
            eval("typeof AnimationEvent === 'function' && typeof TransitionEvent === 'function'"),
            "true",
            "the animation and transition event interfaces exist"
        );
        assert_eq!(
            eval("'onmessage' in window && !('onmessage' in div)"),
            "true",
            "WindowEventHandlers stays off elements"
        );
        assert_eq!(
            eval("'onreadystatechange' in document"),
            "true",
            "Document declares its own handlers"
        );
        drop(realm);
        drop(shared);
    }

    #[test]
    fn the_cookie_url_ignores_the_document_base() {
        let services: Arc<dyn BrowserServices> = Arc::new(NullServices::default());
        let shared = SharedJsRuntime::default();
        let documents = Rc::new(RefCell::new(crate::documents::DocumentStore::default()));
        let registry = Rc::new(RefCell::new(crate::js::RealmRegistry::default()));
        let world = world_with_document(
            &shared,
            &services,
            &documents,
            &registry,
            "https://a.test/dir/page",
            "<!doctype html><base href='https://other.example/'>",
        );
        {
            let world = world.borrow();
            world.doc_mut().base_url =
                Some(Url::parse("https://other.example/").expect("base URL"));
            assert_eq!(world.cookie_url().as_str(), "https://a.test/dir/page");
            world.doc_mut().inherited_cookie_url =
                Some(Url::parse("https://creator.test/dir/page").expect("creator URL"));
            assert_eq!(world.cookie_url().as_str(), "https://creator.test/dir/page");
        }
    }

    // https://cookiestore.spec.whatwg.org/#process-cookie-changes
    #[test]
    fn a_cookie_change_queues_every_other_realm() {
        let services: Arc<dyn BrowserServices> = Arc::new(NullServices::default());
        let shared = SharedJsRuntime::default();
        let stop = Arc::new(Stop::new());
        let documents = Rc::new(RefCell::new(crate::documents::DocumentStore::default()));
        let registry = Rc::new(RefCell::new(crate::js::RealmRegistry::default()));
        let main = world_with_document(
            &shared,
            &services,
            &documents,
            &registry,
            "https://a.test/",
            "<!doctype html><p>page</p>",
        );
        let child_frame = FrameId::new(1);
        let child = Rc::new(RefCell::new(World::new(
            Url::parse("https://a.test/frame").expect("child URL"),
            child_frame,
            &main.borrow().runtime,
            Rc::new(RefCell::new(Vec::new())),
            Rc::new(RefCell::new(Vec::new())),
        )));
        registry.borrow_mut().insert_frame(child_frame, &child);
        let _realm =
            JsRealm::new(&shared.handle(), main.clone(), Arc::clone(&stop)).expect("realm");

        main.borrow().notify_cookie_changes();
        assert!(
            !main.borrow().has_outbox_work(),
            "the realm that made the change processes it itself"
        );
        assert!(matches!(
            child.borrow().pop_outbox(),
            Some(crate::js::RealmRequest::CookieChanges)
        ));
        assert!(
            !child.borrow().has_outbox_work(),
            "one request per realm per change"
        );
    }

    // https://html.spec.whatwg.org/multipage/browsing-the-web.html#navigate-to-a-fragment
    #[test]
    fn a_fragment_assignment_does_not_queue_a_load() {
        let services: Arc<dyn BrowserServices> = Arc::new(NullServices::default());
        let shared = SharedJsRuntime::default();
        let stop = Arc::new(Stop::new());
        let documents = Rc::new(RefCell::new(crate::documents::DocumentStore::default()));
        let registry = Rc::new(RefCell::new(crate::js::RealmRegistry::default()));
        let world = world_with_document(
            &shared,
            &services,
            &documents,
            &registry,
            "https://a.test/dir/page",
            "<!doctype html><p>hello</p>",
        );
        world.borrow().doc_mut().history = crate::protocol::HistorySnapshot {
            length: 1,
            local_length: 1,
            ..crate::protocol::HistorySnapshot::default()
        };
        let realm = JsRealm::new(&shared.handle(), world.clone(), stop).expect("realm");
        let before = world.borrow().doc().history.length;

        let observed = realm
            .eval(
                "window.__seen = [];\
                 window.onpopstate = event => __seen.push([event.type, event.state]);\
                 location = '#frag';\
                 JSON.stringify(__seen) + '|' + location.href + '|' + history.length + '|' + (history.state === null)",
            )
            .expect("fragment assignment");
        assert_eq!(
            observed, r#"[["popstate",null]]|https://a.test/dir/page#frag|2|true"#,
            "the setter fires popstate and updates history before it returns"
        );
        {
            let world = world.borrow();
            assert_eq!(
                world.doc().document_url.as_str(),
                "https://a.test/dir/page#frag"
            );
            assert_eq!(world.doc().history.length, before.saturating_add(1));
            assert!(
                !world.has_frame_navigations(),
                "a fragment assignment must not load a document"
            );
        }

        let href = realm
            .eval("location.hash = 'other'; location.href")
            .expect("hash assignment");
        assert_eq!(href, "https://a.test/dir/page#other");
        assert!(!world.borrow().has_frame_navigations());

        realm
            .eval("location = '/elsewhere'")
            .expect("path assignment");
        assert!(
            world.borrow().has_frame_navigations(),
            "a different path is a document load"
        );
    }

    #[test]
    fn realms_share_a_heap_and_resolve_their_own_world() {
        let services: Arc<dyn BrowserServices> = Arc::new(NullServices::default());
        let shared = SharedJsRuntime::default();
        let stop = Arc::new(Stop::new());
        let documents = Rc::new(RefCell::new(crate::documents::DocumentStore::default()));
        let registry = Rc::new(RefCell::new(crate::js::RealmRegistry::default()));
        let world_a = world_with_document(
            &shared,
            &services,
            &documents,
            &registry,
            "https://a.test/",
            "<!doctype html><p id=a></p>",
        );
        let world_b = world_with_document(
            &shared,
            &services,
            &documents,
            &registry,
            "https://b.test/",
            "<!doctype html><p id=b></p>",
        );
        let realm_a =
            JsRealm::new(&shared.handle(), world_a.clone(), Arc::clone(&stop)).expect("realm a");
        let realm_b =
            JsRealm::new(&shared.handle(), world_b.clone(), Arc::clone(&stop)).expect("realm b");

        for (realm, expected) in [(&realm_a, &world_a), (&realm_b, &world_b)] {
            realm.context.with(|ctx| {
                let resolved = world(&ctx).expect("world");
                assert!(
                    Rc::ptr_eq(&resolved, expected),
                    "realm resolved a foreign world"
                );
            });
        }

        let add_one = realm_a.context.with(|ctx| {
            let value: Value = super::super::eval_classic(&ctx, "(x) => x + 1").expect("function");
            Persistent::save(&ctx, value)
        });
        realm_b.context.with(|ctx| {
            let value = add_one.restore(&ctx).expect("restore");
            let function = value.into_function().expect("function value");
            let result: i32 = function.call((41,)).expect("call");
            assert_eq!(result, 42);
        });

        realm_a.context.with(|ctx| {
            let mine: bool =
                super::super::eval_classic(&ctx, "document.getElementById('a') !== null")
                    .expect("own tree");
            let theirs: bool =
                super::super::eval_classic(&ctx, "document.getElementById('b') === null")
                    .expect("foreign tree");
            assert!(mine && theirs);
        });
    }

    #[test]
    fn wrappers_are_shared_with_the_owner_realms_prototypes() {
        let services: Arc<dyn BrowserServices> = Arc::new(NullServices::default());
        let shared = SharedJsRuntime::default();
        let stop = Arc::new(Stop::new());
        let documents = Rc::new(RefCell::new(crate::documents::DocumentStore::default()));
        let registry = Rc::new(RefCell::new(crate::js::RealmRegistry::default()));
        let world_a = world_with_document(
            &shared,
            &services,
            &documents,
            &registry,
            "https://a.test/",
            "<!doctype html><p id=a></p>",
        );
        let world_b = world_with_document(
            &shared,
            &services,
            &documents,
            &registry,
            "https://b.test/",
            "<!doctype html><p id=b></p>",
        );
        let realm_a = JsRealm::new(&shared.handle(), world_a, Arc::clone(&stop)).expect("realm a");
        let realm_b =
            JsRealm::new(&shared.handle(), world_b.clone(), Arc::clone(&stop)).expect("realm b");
        let b_root = world_b
            .borrow()
            .with_main_document(|parsed| NodeId {
                document: parsed.id,
                node: parsed.document.base.root_node().id,
            })
            .expect("b document");

        let from_a = realm_a.context.with(|ctx| {
            let value = match wrap_node(&ctx, b_root) {
                Ok(value) => value,
                Err(err) => {
                    let caught = ctx.catch();
                    let message: String = caught
                        .as_object()
                        .and_then(|object| object.get("message").ok())
                        .unwrap_or_default();
                    panic!("wrap failed: {err:?}: {message}");
                }
            };
            Persistent::save(&ctx, value)
        });
        realm_b.context.with(|ctx| {
            let own: Value = super::super::eval_classic(&ctx, "document").expect("b document");
            let wrapped = from_a.restore(&ctx).expect("restore");
            assert_eq!(own, wrapped, "one wrapper is shared across realms");
            ctx.globals().set("__w", wrapped).expect("test global");
            let is_document: bool = super::super::eval_classic(
                &ctx,
                "Object.getPrototypeOf(__w) === HTMLDocument.prototype && __w instanceof Document",
            )
            .expect("prototype");
            assert!(is_document, "wrapper keeps the owner realm's prototype");
        });

        realm_a.context.with(|ctx| {
            let value = wrap_node(&ctx, b_root).expect("wrap b document in a");
            let object = value.into_object().expect("object");
            ctx.globals().set("__b", object).expect("test global");
            let _: () = super::super::eval_classic(&ctx, "__b.body.setAttribute('x', '1')")
                .expect("mutate");
        });
        realm_b.context.with(|ctx| {
            let seen: bool =
                super::super::eval_classic(&ctx, "document.body.getAttribute('x') === '1'")
                    .expect("read");
            assert!(seen, "mutations from realm A reach realm B's tree");
            let element: Value =
                super::super::eval_classic(&ctx, "document.documentElement").expect("element");
            ctx.globals().set("__el", element).expect("test global");
        });
        let element_from_a = realm_a.context.with(|ctx| {
            let value = wrap_node(&ctx, b_root).expect("wrap b document in a");
            let object = value.into_object().expect("object");
            let element: Value = object.get("documentElement").expect("documentElement");
            Persistent::save(&ctx, element)
        });
        realm_b.context.with(|ctx| {
            let restored = element_from_a.restore(&ctx).expect("restore");
            ctx.globals()
                .set("__el_from_a", restored)
                .expect("test global");
            let same: bool =
                super::super::eval_classic(&ctx, "__el_from_a === __el").expect("identity");
            assert!(same, "cross-realm property reads return the shared wrapper");
        });
    }

    #[test]
    fn one_drain_stops_four_runaway_timers() {
        let services: Arc<dyn BrowserServices> = Arc::new(NullServices::default());
        let stop = Arc::new(Stop::new());
        let wake = Arc::new(tokio::sync::Notify::new());
        let mut engine =
            crate::engine::Engine::new(services, stop, wake, net::ValidIdentity::default());
        let html = "<script>globalThis.__n = 0; for (let i = 0; i < 4; i++) setTimeout(() => { globalThis.__n++; for (;;) {} }, 0);</script>";
        engine
            .mount_frame(
                FrameId::MAIN,
                &crate::protocol::Mount {
                    head: crate::protocol::ResponseHead::local(
                        "https://example.test/".parse().expect("URL"),
                        Some("text/html".into()),
                        None,
                    ),
                    tab: crate::protocol::TabState::default(),
                    body: html.as_bytes().to_vec(),
                },
            )
            .expect("mount");
        let started = std::time::Instant::now();
        let yielded = engine.drain_ready_until(started + std::time::Duration::from_millis(400));
        let elapsed = started.elapsed();
        assert!(yielded, "ready timers stay queued after the deadline");
        assert!(
            engine.next_deadline().is_none(),
            "fired timers are queued tasks, not a deadline"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(10),
            "one drain took {elapsed:?}"
        );
        let count = engine
            .eval_in(FrameId::MAIN, "String(globalThis.__n)")
            .expect("counter");
        assert_eq!(count, "1");
        let again = engine
            .drain_ready_until(std::time::Instant::now() + std::time::Duration::from_millis(400));
        assert!(again, "the queued timer is still ready");
        let count = engine
            .eval_in(FrameId::MAIN, "String(globalThis.__n)")
            .expect("counter");
        assert_eq!(count, "2");
    }

    #[test]
    fn embedded_yield_is_an_immediate_step() {
        let services: Arc<dyn BrowserServices> = Arc::new(NullServices::default());
        let mut renderer = crate::EmbeddedRenderer::new(services, net::ValidIdentity::default());
        let html = "<script>globalThis.__n = 0; for (let i = 0; i < 4; i++) setTimeout(() => { globalThis.__n++; for (;;) {} }, 0);</script>";
        renderer
            .mount_for_test(&crate::protocol::Mount {
                head: crate::protocol::ResponseHead::local(
                    "https://example.test/".parse().expect("URL"),
                    Some("text/html".into()),
                    None,
                ),
                tab: crate::protocol::TabState::default(),
                body: html.as_bytes().to_vec(),
            })
            .expect("mount");
        let yielded = renderer.drain_for(std::time::Duration::from_millis(400));
        assert!(yielded, "ready work stays queued");
        let delay = renderer.time_until_deadline();
        assert_eq!(delay, Some(std::time::Duration::ZERO));
        let count = renderer.eval("String(globalThis.__n)").expect("counter");
        assert_eq!(count, "1");
    }

    #[test]
    fn revoke_object_url_rejects_a_cross_origin_caller() {
        let services: Arc<dyn BrowserServices> = Arc::new(NullServices::default());
        let shared = SharedJsRuntime::default();
        let documents = Rc::new(RefCell::new(crate::documents::DocumentStore::default()));
        let registry = Rc::new(RefCell::new(crate::js::RealmRegistry::default()));
        let first = world_with_document(
            &shared,
            &services,
            &documents,
            &registry,
            "https://a.test/",
            "<html></html>",
        );
        let second = world_with_document(
            &shared,
            &services,
            &documents,
            &registry,
            "https://b.test/",
            "<html></html>",
        );
        let url = first
            .borrow_mut()
            .create_object_url("x".into(), "text/plain".into())
            .expect("object url");
        second.borrow_mut().revoke_object_url(&url);
        assert!(first.borrow().object_url_contents(&url).is_some());
        first.borrow_mut().revoke_object_url(&url);
        assert!(first.borrow().object_url_contents(&url).is_none());
    }

    fn mount_html(engine: &mut crate::engine::Engine, html: &str) {
        engine
            .mount_frame(
                FrameId::MAIN,
                &crate::protocol::Mount {
                    head: crate::protocol::ResponseHead::local(
                        "https://example.test/".parse().expect("URL"),
                        Some("text/html".into()),
                        None,
                    ),
                    tab: crate::protocol::TabState::default(),
                    body: html.as_bytes().to_vec(),
                },
            )
            .expect("mount");
        let _ =
            engine.drain_ready_until(std::time::Instant::now() + std::time::Duration::from_secs(2));
    }

    fn load_state(engine: &mut crate::engine::Engine) -> (String, String) {
        (
            engine
                .eval_in(FrameId::MAIN, "document.readyState")
                .expect("readyState"),
            engine
                .eval_in(FrameId::MAIN, "String(globalThis.__loaded)")
                .expect("load flag"),
        )
    }

    #[test]
    fn timeout_cancelled_in_the_same_turn_does_not_fire() {
        let services: Arc<dyn BrowserServices> = Arc::new(NullServices::default());
        let mut engine = crate::engine::Engine::new(
            services,
            Arc::new(Stop::new()),
            Arc::new(tokio::sync::Notify::new()),
            net::ValidIdentity::default(),
        );
        mount_html(
            &mut engine,
            "<script>globalThis.__fired = 0; \
             const id = setTimeout(() => { globalThis.__fired = 1; }, 0); \
             clearTimeout(id);</script>",
        );
        assert_eq!(
            engine
                .eval_in(FrameId::MAIN, "String(__fired)")
                .expect("fired"),
            "0"
        );
    }

    #[test]
    fn script_error_handler_may_insert_another_bad_script() {
        let services: Arc<dyn BrowserServices> = Arc::new(NullServices::default());
        let mut engine = crate::engine::Engine::new(
            services,
            Arc::new(Stop::new()),
            Arc::new(tokio::sync::Notify::new()),
            net::ValidIdentity::default(),
        );
        mount_html(
            &mut engine,
            "<script>globalThis.__errors = 0; \
             function insertBad() { \
                 const script = document.createElement('script'); \
                 script.src = 'http://['; \
                 script.onerror = () => { \
                     globalThis.__errors += 1; \
                     if (globalThis.__errors < 3) insertBad(); \
                 }; \
                 document.documentElement.appendChild(script); \
             } \
             insertBad();</script>",
        );
        assert_eq!(
            engine
                .eval_in(FrameId::MAIN, "String(__errors)")
                .expect("errors"),
            "3"
        );
    }

    #[test]
    fn document_write_after_open_keeps_queue_order() {
        let services: Arc<dyn BrowserServices> = Arc::new(NullServices::default());
        let mut engine = crate::engine::Engine::new(
            services,
            Arc::new(Stop::new()),
            Arc::new(tokio::sync::Notify::new()),
            net::ValidIdentity::default(),
        );
        mount_html(
            &mut engine,
            "<script>document.open(); \
             document.write('<p id=\"written\">hello</p>'); \
             document.close();</script>",
        );
        assert_eq!(
            engine
                .eval_in(
                    FrameId::MAIN,
                    "document.getElementById('written').textContent",
                )
                .expect("written element"),
            "hello"
        );
    }

    #[test]
    fn replace_document_releases_writes_and_keeps_cancellations() {
        let services: Arc<dyn BrowserServices> = Arc::new(NullServices::default());
        let shared = SharedJsRuntime::default();
        let documents = Rc::new(RefCell::new(crate::documents::DocumentStore::default()));
        let registry = Rc::new(RefCell::new(crate::js::RealmRegistry::default()));
        let world = world_with_document(
            &shared,
            &services,
            &documents,
            &registry,
            "https://a.test/",
            "<!doctype html><p></p>",
        );
        world
            .borrow_mut()
            .outbox
            .borrow_mut()
            .push_back(crate::js::RealmRequest::ClearTimer(
                crate::js::ClearTimeoutRequest {
                    js_id: crate::js::PageTimerId(7),
                    realm: 1,
                },
            ));
        world
            .borrow_mut()
            .queue_write("x".repeat(7 * 1024 * 1024))
            .expect("reserve write");
        world.borrow_mut().replace_document(crate::parse_html(
            "<!doctype html><p>new</p>",
            blitz_dom::DocumentConfig::default(),
        ));
        assert!(matches!(
            world.borrow().pop_outbox(),
            Some(crate::js::RealmRequest::ClearTimer(
                crate::js::ClearTimeoutRequest {
                    js_id: crate::js::PageTimerId(7),
                    realm: 1,
                }
            ))
        ));
        assert!(
            world
                .borrow_mut()
                .queue_write("y".repeat(7 * 1024 * 1024))
                .is_ok(),
            "the replaced document's write must release its budget"
        );
    }

    #[test]
    fn iframe_finishing_last_fires_parent_load() {
        let services: Arc<dyn BrowserServices> = Arc::new(NullServices::default());
        let mut engine = crate::engine::Engine::new(
            services,
            Arc::new(Stop::new()),
            Arc::new(tokio::sync::Notify::new()),
            net::ValidIdentity::default(),
        );
        mount_html(
            &mut engine,
            "<script>globalThis.__loaded = false; \
             addEventListener('load', () => { globalThis.__loaded = true; });</script>\
             <script src=\"data:text/javascript,globalThis.__parentScript = 1\"></script>\
             <iframe src=\"data:text/html,<p>child</p>\"></iframe>",
        );
        assert_eq!(load_state(&mut engine), ("complete".into(), "true".into()));
    }

    #[test]
    fn failed_data_script_finishing_last_fires_load() {
        let services: Arc<dyn BrowserServices> = Arc::new(NullServices::default());
        let mut engine = crate::engine::Engine::new(
            services,
            Arc::new(Stop::new()),
            Arc::new(tokio::sync::Notify::new()),
            net::ValidIdentity::default(),
        );
        mount_html(
            &mut engine,
            "<script>globalThis.__loaded = false; \
             addEventListener('load', () => { globalThis.__loaded = true; });</script>\
             <script src=\"data:text/javascript;base64,!!!!\"></script>",
        );
        assert_eq!(load_state(&mut engine), ("complete".into(), "true".into()));
    }

    #[test]
    fn font_requested_by_a_stylesheet_delays_load() {
        let services: Arc<dyn BrowserServices> = Arc::new(NullServices::default());
        let mut engine = crate::engine::Engine::new(
            services,
            Arc::new(Stop::new()),
            Arc::new(tokio::sync::Notify::new()),
            net::ValidIdentity::default(),
        );
        mount_html(
            &mut engine,
            "<style>@font-face { font-family: X; \
             src: url(data:font/woff2;base64,AAAA) format('woff2'); }</style>\
             <p style=\"font-family: X\">x</p>\
             <script>globalThis.__loaded = false; \
             addEventListener('load', () => { globalThis.__loaded = true; });</script>",
        );
        assert_eq!(load_state(&mut engine), ("complete".into(), "true".into()));
    }

    #[test]
    fn no_engine_hooks_leak_onto_page_global() {
        let services: Arc<dyn BrowserServices> = Arc::new(NullServices::default());
        let mut engine = crate::engine::Engine::new(
            services,
            Arc::new(Stop::new()),
            Arc::new(tokio::sync::Notify::new()),
            net::ValidIdentity::default(),
        );
        mount_html(&mut engine, "<p>ok</p>");
        let leaked = engine
            .eval_in(
                FrameId::MAIN,
                "Object.getOwnPropertyNames(globalThis).filter(n => n.startsWith('__tb')).join(',')",
            )
            .expect("eval");
        assert_eq!(leaked, "");
    }

    #[test]
    fn embedded_browser_script_dispatches_trusted_input() {
        let services: Arc<dyn BrowserServices> = Arc::new(NullServices::default());
        let mut embedded = crate::EmbeddedRenderer::new(services, net::ValidIdentity::default());
        let mount = crate::Mount {
            head: crate::ResponseHead::local(
                url::Url::parse("https://example.test/").expect("url"),
                Some("text/html; charset=utf-8".into()),
                None,
            ),
            tab: crate::TabState::default(),
            body: b"<!doctype html><body style=\"margin:0\">\
                    <button id=\"b\" style=\"width:120px;height:40px\">go</button>\
                    <script>\
                    window.__log = [];\
                    b.addEventListener('pointerdown', e => window.__log.push('pd:' + e.isTrusted));\
                    b.addEventListener('click', e => window.__log.push('ck:' + e.isTrusted));\
                    </script></body>"
                .to_vec(),
        };
        embedded.mount(&mount).expect("mount");
        let _frame = embedded
            .render(&crate::ScreenshotRequest {
                viewport_width: 300,
                viewport_height: 200,
                clip: None,
                format: crate::ScreenshotFormat::Png,
                omit_background: false,
            })
            .expect("render");
        embedded
            .execute_browser(
                r#"(function(){return host.__tbWebDriverActions([{"type":"pointer","id":"mouse","parameters":{"pointerType":"mouse"},"actions":[{"type":"pointerMove","origin":"viewport","x":20,"y":20},{"type":"pointerDown","button":0},{"type":"pointerUp","button":0}]}]);})()"#,
                None,
            )
            .expect("input");
        let log = embedded.eval("window.__log.join(',')").expect("eval");
        assert_eq!(log, "pd:true,ck:true");
    }

    // https://chromedevtools.github.io/devtools-protocol/tot/Page/#method-createIsolatedWorld
    #[test]
    fn isolated_realm_shares_live_document_and_bypasses_page_csp() {
        let services: Arc<dyn BrowserServices> = Arc::new(NullServices::default());
        let shared = SharedJsRuntime::default();
        let stop = Arc::new(Stop::new());
        let documents = Rc::new(RefCell::new(crate::documents::DocumentStore::default()));
        let registry = Rc::new(RefCell::new(crate::js::RealmRegistry::default()));
        let main = world_with_document(
            &shared,
            &services,
            &documents,
            &registry,
            "https://a.test/start",
            "<!doctype html><div id='target'></div>",
        );
        let main_realm =
            JsRealm::new(&shared.handle(), Rc::clone(&main), Arc::clone(&stop)).expect("main");
        let isolated = Rc::new(RefCell::new(World::new_isolated(
            &main.borrow(),
            "iso".to_owned(),
        )));
        let isolated_realm =
            JsRealm::new(&shared.handle(), Rc::clone(&isolated), stop).expect("isolated");

        main.borrow().doc_mut().policy.csp =
            crate::csp::List::from_headers(Some("script-src 'self'"), None);
        main.borrow().doc_mut().document_url =
            Url::parse("https://a.test/updated#hash").expect("url");
        assert_eq!(
            isolated_realm.eval("location.href").expect("isolated url"),
            "https://a.test/updated#hash",
            "isolated realm reads live DocumentData updates without sync"
        );
        assert_eq!(
            main_realm
                .eval("try { eval('1+1'); 'ok' } catch (e) { e.name }")
                .expect("main csp"),
            "EvalError",
            "main realm enforces page CSP"
        );
        assert_eq!(
            isolated_realm.eval("eval('1 + 1')").expect("isolated csp"),
            "2",
            "isolated realm is not blocked by page CSP by default"
        );

        drop(isolated_realm);
        drop(isolated);
        drop(main_realm);
        drop(main);
        drop(shared);
    }

    // https://chromedevtools.github.io/devtools-protocol/tot/Page/#method-createIsolatedWorld
    #[test]
    fn isolated_realm_drop_preserves_main_listeners_and_document() {
        let services: Arc<dyn BrowserServices> = Arc::new(NullServices::default());
        let shared = SharedJsRuntime::default();
        let stop = Arc::new(Stop::new());
        let documents = Rc::new(RefCell::new(crate::documents::DocumentStore::default()));
        let registry = Rc::new(RefCell::new(crate::js::RealmRegistry::default()));
        let main = world_with_document(
            &shared,
            &services,
            &documents,
            &registry,
            "https://a.test/start",
            "<!doctype html><div id='target'></div>",
        );
        let main_realm =
            JsRealm::new(&shared.handle(), Rc::clone(&main), Arc::clone(&stop)).expect("main");
        let isolated = Rc::new(RefCell::new(World::new_isolated(
            &main.borrow(),
            "iso".to_owned(),
        )));
        let isolated_realm =
            JsRealm::new(&shared.handle(), Rc::clone(&isolated), stop).expect("isolated");

        main_realm
            .eval(
                "window.__hits = [];\
                 window.addEventListener('ping', () => window.__hits.push('main'));\
                 document.getElementById('target').addEventListener('ping', () => window.__hits.push('main-el'));",
            )
            .expect("main listeners");
        isolated_realm
            .eval(
                "window.__isoHits = 0;\
                 window.addEventListener('ping', () => { window.__isoHits++; });\
                 document.getElementById('target').addEventListener('ping', () => { window.__isoHits += 10; });",
            )
            .expect("isolated listeners");
        main_realm
            .eval(
                "window.dispatchEvent(new Event('ping'));\
                 document.getElementById('target').dispatchEvent(new Event('ping'));",
            )
            .expect("dispatch before drop");
        assert_eq!(
            main_realm
                .eval("window.__hits.join(',')")
                .expect("main hits"),
            "main,main-el"
        );
        assert_eq!(
            isolated_realm.eval("window.__isoHits").expect("iso hits"),
            "11",
            "isolated realm shares WindowData event listeners"
        );

        let object_url = main_realm
            .eval("URL.createObjectURL(new Blob(['ok']))")
            .expect("blob url");
        main.borrow()
            .outbox
            .borrow_mut()
            .push_back(crate::js::RealmRequest::CookieChanges);

        drop(isolated_realm);
        drop(isolated);

        assert!(
            main.borrow().has_outbox_work(),
            "dropping an isolated realm must not clear the main outbox"
        );
        assert!(
            main.borrow().main_document().is_some(),
            "dropping an isolated realm must not remove the frame's documents"
        );
        assert!(
            object_url.starts_with("blob:"),
            "object URL must be a blob URL: {object_url}"
        );
        main_realm
            .eval(
                "window.__hits = [];\
                 window.dispatchEvent(new Event('ping'));\
                 document.getElementById('target').dispatchEvent(new Event('ping'));",
            )
            .expect("dispatch after drop");
        assert_eq!(
            main_realm
                .eval("window.__hits.join(',')")
                .expect("main hits after drop"),
            "main,main-el",
            "dropping an isolated realm removes only its listeners, keeping main listeners intact"
        );

        drop(main_realm);
        drop(main);
        drop(shared);
    }
}
