//! Frame-tree and cross-realm messaging host functions.

use super::{handler_target, throw_dom, world, world_for_node, wrap_node};

use std::cell::RefCell;

use std::rc::Rc;

use crate::dom::tree::NodeId;

use rquickjs::{Ctx, Exception, Object, Persistent, Result, Value};

use crate::js::world::World;

use crate::messaging::{Delivery, MAX_FRAMES, PAYLOAD_VERSION, Shared};
use crate::protocol::TargetOrigin;

use crate::protocol::FrameId;

/// Installs the frame-tree and cross-realm messaging host functions the shim
/// calls: posting window messages and channel messages, reading the frame
/// tree, and moving channel endpoints between realms.
///
/// The calling realm's world comes from the realm registration, so every
/// function is a plain item and its borrow of the world ends with the call.
pub(crate) fn install_messaging(ctx: &Ctx<'_>) -> Result<()> {
    let globals = crate::js::bridge::object(ctx)?;
    let frame = world(ctx)?.borrow().frame();
    globals.set("__tb_frameId", crate::js::js_number(frame.get()))?;
    globals.set(
        "__tb_maxFrames",
        crate::js::js_number(u64::try_from(MAX_FRAMES).unwrap_or(u64::MAX)),
    )?;

    globals.set(
        "__tbPostWindowMessage",
        rquickjs::prelude::Func::from(post_window_message),
    )?;
    globals.set(
        "__tbValidateTargetOrigin",
        rquickjs::prelude::Func::from(validate_target_origin),
    )?;
    globals.set("__tbPortNew", rquickjs::prelude::Func::from(port_new))?;
    globals.set("__tbPortPost", rquickjs::prelude::Func::from(port_post))?;
    globals.set("__tbPortStart", rquickjs::prelude::Func::from(port_start))?;
    globals.set("__tbPortClose", rquickjs::prelude::Func::from(port_close))?;
    globals.set("__tbPortDetach", rquickjs::prelude::Func::from(port_detach))?;
    globals.set("__tbPortAdopt", rquickjs::prelude::Func::from(port_adopt))?;
    globals.set("__tbPortPeer", rquickjs::prelude::Func::from(port_peer))?;
    globals.set(
        "__tbFrameParent",
        rquickjs::prelude::Func::from(frame_parent),
    )?;
    globals.set("__tbFrameTop", rquickjs::prelude::Func::from(frame_top))?;
    globals.set(
        "__tbFrameChildCount",
        rquickjs::prelude::Func::from(frame_child_count),
    )?;
    globals.set("__tbFrameChild", rquickjs::prelude::Func::from(frame_child))?;
    globals.set(
        "__tbFrameGlobal",
        rquickjs::prelude::Func::from(frame_global),
    )?;
    globals.set(
        "__tbFrameDocument",
        rquickjs::prelude::Func::from(frame_document),
    )?;
    globals.set(
        "__tbFrameRegistered",
        rquickjs::prelude::Func::from(frame_registered),
    )?;
    globals.set("__tbFrameName", rquickjs::prelude::Func::from(frame_name))?;
    globals.set(
        "__tbFrameSameOrigin",
        rquickjs::prelude::Func::from(frame_same_origin),
    )?;
    globals.set(
        "__tbFrameLocation",
        rquickjs::prelude::Func::from(frame_location),
    )?;
    globals.set(
        "__tbFrameSetLocation",
        rquickjs::prelude::Func::from(frame_set_location),
    )?;
    globals.set(
        "__tbSetFrameName",
        rquickjs::prelude::Func::from(set_frame_name),
    )?;
    globals.set(
        "__tbBroadcastChannelId",
        rquickjs::prelude::Func::from(broadcast_channel_id),
    )?;
    globals.set(
        "__tbBroadcastPost",
        rquickjs::prelude::Func::from(broadcast_post),
    )?;
    globals.set(
        "__tbIsOutOfBoundsView",
        rquickjs::prelude::Func::from(is_out_of_bounds_view),
    )?;
    globals.set("__tbCloseFrame", rquickjs::prelude::Func::from(close_frame))?;
    globals.set(
        "__tbFrameAwaitingReplay",
        rquickjs::prelude::Func::from(frame_awaiting_replay),
    )?;
    globals.set(
        "__tbFrameAddEventListener",
        rquickjs::prelude::Func::from(frame_add_event_listener),
    )?;
    globals.set(
        "__tbFrameRemoveEventListener",
        rquickjs::prelude::Func::from(frame_remove_event_listener),
    )?;
    globals.set(
        "__tbFrameDispatchEvent",
        rquickjs::prelude::Func::from(frame_dispatch_event),
    )?;
    globals.set(
        "__tbRegisterSharedBuffer",
        rquickjs::prelude::Func::from(register_shared_buffer),
    )?;
    globals.set(
        "__tbCreateSharedBuffer",
        rquickjs::prelude::Func::from(create_shared_buffer),
    )?;
    Ok(())
}

/// Registers one `SharedArrayBuffer` block and returns the id its serialized
/// form names.
///
/// The bytes are not copied: the message names the block, and a receiving
/// realm, on this thread or a worker's, wraps the same memory
/// (<https://html.spec.whatwg.org/multipage/structured-data.html#structuredserializeinternal>).
fn register_shared_buffer(ctx: Ctx<'_>, value: Value<'_>) -> Result<Option<u64>> {
    let Some(lease) = rquickjs::SharedBufferLease::new(&ctx, &value) else {
        return Ok(None);
    };
    let world = world(&ctx)?;
    let world = world.borrow();
    Ok(Some(world.runtime.shared_buffers.register(lease)))
}

/// Wraps a registered `SharedArrayBuffer` block in this realm's runtime.
///
/// `byte_length` and `max_byte_length` come from the serialized payload, which
/// another realm's script can shape, so both are checked against the block's
/// allocation before `QuickJS` sees them.
/// `max_byte_length == 0` means the sender's buffer was fixed-length.
fn create_shared_buffer(
    ctx: Ctx<'_>,
    id: u64,
    byte_length: u64,
    max_byte_length: u64,
) -> Result<Option<rquickjs::ArrayBuffer<'_>>> {
    let world = world(&ctx)?;
    let table = std::sync::Arc::clone(&world.borrow().runtime.shared_buffers);
    let Some(lease) = table.get(id) else {
        return Ok(None);
    };
    let invalid = || Exception::throw_range(&ctx, "invalid SharedArrayBuffer length");
    let byte_length = usize::try_from(byte_length).map_err(|_| invalid())?;
    let max_byte_length = if max_byte_length == 0 {
        None
    } else {
        Some(usize::try_from(max_byte_length).map_err(|_| invalid())?)
    };
    if byte_length > lease.len()
        || max_byte_length.is_some_and(|max| max < byte_length || max > lease.len())
    {
        return Err(invalid());
    }
    lease
        .to_array_buffer_with(&ctx, byte_length, max_byte_length)
        .map(Some)
}

/// Whether one structured-clone candidate is a typed array whose buffer is
/// detached or out of bounds.
///
/// The structured-clone algorithm throws a `DataCloneError` for such a view,
/// but the JavaScript getters report `0` for both `byteOffset` and
/// `byteLength`, so the serializer cannot see it from script
/// (<https://html.spec.whatwg.org/multipage/structured-data.html#structuredserializeinternal>).
fn is_out_of_bounds_view(value: rquickjs::Value<'_>) -> bool {
    rquickjs::is_out_of_bounds_view(&value)
}

/// Closes an auxiliary context from its opener, across origins.
///
/// `close` is a cross-origin-accessible `WindowProxy` member, but only the
/// context's opener may use it on another window
/// (<https://html.spec.whatwg.org/multipage/browsers.html#crossoriginproperties-(-o-)>,
/// <https://html.spec.whatwg.org/multipage/window-object.html#dom-window-close>).
fn close_frame(ctx: Ctx<'_>, frame: f64) {
    let Some(frame) = protocol_id(frame) else {
        return;
    };
    let Ok(world) = world(&ctx) else {
        return;
    };
    let caller = world.borrow().frame();
    let frame = FrameId::new(frame);
    {
        let shared = world.borrow().shared();
        if shared.borrow().tree.opener(frame) != Some(caller) {
            return;
        }
    }
    // The closing window and every navigable it owns run their unload steps
    // first, then the context is destroyed
    // (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#unload>).
    world.borrow().close_auxiliary(frame);
}

/// The world an `EventTarget` call on `frame`'s window targets.
///
/// A `WindowProxy` is not a platform object in the calling realm, so
/// `addEventListener` reached through it cannot pass the interface's receiver
/// check. The target frame is named explicitly instead, as `postMessage` does
/// (<https://html.spec.whatwg.org/multipage/window-object.html#the-windowproxy-exotic-object>,
/// <https://dom.spec.whatwg.org/#interface-eventtarget>).
fn frame_event_target(ctx: &Ctx<'_>, frame: u64) -> Result<Rc<RefCell<World>>> {
    let current = world(ctx)?;
    let Some(target) = current.borrow().frame_world(FrameId::new(frame)) else {
        return Err(Exception::throw_type(ctx, "incompatible receiver"));
    };
    let same_origin = target.borrow().origin == current.borrow().origin;
    if !same_origin {
        return Err(Exception::throw_type(ctx, "incompatible receiver"));
    }
    Ok(target)
}

/// Whether `frame`'s document still owes a replay of the `WindowProxy` state
/// other realms recorded for it. A call made now would land on a document the
/// frame is about to replace, so the caller holds it instead
/// (<https://html.spec.whatwg.org/multipage/window-object.html#windowproxy-set>).
fn frame_awaiting_replay(ctx: Ctx<'_>, frame: f64) -> Result<bool> {
    let Some(frame) = protocol_id(frame) else {
        return Ok(false);
    };
    let current = world(&ctx)?;
    Ok(current
        .borrow()
        .frame_world(FrameId::new(frame))
        .is_some_and(|target| target.borrow().needs_proxy_sets_replay()))
}

/// Window `addEventListener` addressed to `frame`
/// (<https://dom.spec.whatwg.org/#dom-eventtarget-addeventlistener>).
fn frame_add_event_listener<'js>(
    ctx: Ctx<'js>,
    frame: f64,
    typ: Value<'js>,
    callback: Value<'js>,
    options: rquickjs::function::Opt<Value<'js>>,
) -> Result<()> {
    let Some(frame) = protocol_id(frame) else {
        return Err(Exception::throw_type(&ctx, "incompatible receiver"));
    };
    let target = frame_event_target(&ctx, frame)?;
    crate::js::events::add_listener_in(
        &ctx,
        &target,
        crate::js::world::EventTargetKey::Window,
        typ,
        callback,
        options.0,
    )
}

/// Window `removeEventListener` addressed to `frame`
/// (<https://dom.spec.whatwg.org/#dom-eventtarget-removeeventlistener>).
fn frame_remove_event_listener<'js>(
    ctx: Ctx<'js>,
    frame: f64,
    typ: Value<'js>,
    callback: Value<'js>,
    options: rquickjs::function::Opt<Value<'js>>,
) -> Result<()> {
    let Some(frame) = protocol_id(frame) else {
        return Err(Exception::throw_type(&ctx, "incompatible receiver"));
    };
    let target = frame_event_target(&ctx, frame)?;
    crate::js::events::remove_listener_in(
        &ctx,
        &target,
        crate::js::world::EventTargetKey::Window,
        typ,
        callback,
        options.0,
    )
}

/// Window `dispatchEvent` addressed to `frame`
/// (<https://dom.spec.whatwg.org/#dom-eventtarget-dispatchevent>).
fn frame_dispatch_event<'js>(
    ctx: Ctx<'js>,
    frame: f64,
    event: rquickjs::Class<'js, crate::js::events::JsEvent>,
) -> Result<bool> {
    let Some(frame) = protocol_id(frame) else {
        return Err(Exception::throw_type(&ctx, "incompatible receiver"));
    };
    let target = frame_event_target(&ctx, frame)?;
    crate::js::events::dispatch_event_for_window(&ctx, &target, &event)
}

/// Mints one `BroadcastChannel` id from the engine-wide counter. A post names
/// its sender's channel so that channel does not receive its own message, and
/// the comparison spans realms.
fn broadcast_channel_id(ctx: Ctx<'_>) -> Result<f64> {
    let world = world(&ctx)?;
    let world = world.borrow();
    let next = world.runtime.next_broadcast_id();
    Ok(crate::js::js_number(next))
}

/// Posts one `BroadcastChannel` message
/// (<https://html.spec.whatwg.org/multipage/web-messaging.html#dom-broadcastchannel-postmessage>).
///
/// A tuple origin broadcasts through the browser, so every renderer process
/// with the same storage key hears it. An opaque origin has no storage key:
/// only realms in this engine that share the same opaque origin can hear it,
/// which is exactly the agent cluster sharing rule, so the engine routes it
/// itself.
fn broadcast_post(ctx: Ctx<'_>, name: String, payload: String, channel: u64) {
    let Ok(world) = world(&ctx) else {
        return;
    };
    let world = world.borrow();
    // A channel in a detached iframe must not reach live contexts. A worker
    // has no frame-tree entry but is live while its realm is.
    if world.worker_id().is_none() && !world.is_attached() {
        return;
    }
    match world.storage_origin() {
        Some(origin) => world
            .runtime
            .services
            .broadcast_post(&origin, &name, &payload, channel),
        None => {
            world
                .runtime
                .push_worker_request_waking(crate::worker::WorkerRequest::BroadcastPost {
                    origin: world.origin.clone(),
                    name,
                    payload,
                    channel,
                });
        }
    }
}

/// Resolves one `targetOrigin` argument without posting. The shim calls this
/// before `__tbEncode`, because the spec resolves the target and throws
/// `SyntaxError` before it structured-serializes the message; doing the parse
/// only in [`post_window_message`] would detach the transferables first
/// (<https://html.spec.whatwg.org/multipage/web-messaging.html#window-post-message-steps>).
fn validate_target_origin(ctx: Ctx<'_>, target_origin: String) -> Result<()> {
    let origin = world(&ctx)?.borrow().origin.clone();
    TargetOrigin::parse(&target_origin, Some(&origin))
        .map(|_| ())
        .ok_or_else(|| throw_dom(&ctx, "SyntaxError", "Invalid target origin"))
}

/// One window message: the target origin is resolved here, where `/` still
/// names the sender's origin with its identity intact, and re-checked at
/// delivery
/// (<https://html.spec.whatwg.org/multipage/web-messaging.html#window-post-message-steps>).
fn post_window_message(
    ctx: Ctx<'_>,
    target: f64,
    target_origin: String,
    payload: String,
    ports: Vec<f64>,
) -> Result<()> {
    let Some(target) = protocol_id(target) else {
        return Ok(());
    };
    let Some(ports) = protocol_ids(ports) else {
        return Ok(());
    };
    if !payload.starts_with(PAYLOAD_VERSION) {
        return Ok(());
    }
    let world = world(&ctx)?;
    let (source, origin, origin_identity, shared) = {
        let world = world.borrow();
        (
            world.frame(),
            world.origin_string(),
            world.origin.clone(),
            world.shared(),
        )
    };
    // A page can call this host function directly, so every transferred
    // endpoint must be one this frame detached; otherwise a hostile frame
    // could hand another frame's in-transit port to a target of its choosing
    // (<https://html.spec.whatwg.org/multipage/web-messaging.html#transfer-receiving-steps>).
    if !shared.borrow().ports.all_in_transit_from(&ports, source) {
        return Ok(());
    }
    // `/` keeps the sender's origin identity, which a serialized `"null"`
    // cannot carry, so the target is resolved here
    // (<https://html.spec.whatwg.org/multipage/web-messaging.html#window-post-message-steps>).
    let Some(target_origin) = TargetOrigin::parse(&target_origin, Some(&origin_identity)) else {
        // The shim validated this argument before serializing the message, so
        // a failure here is an internal mismatch, not a page error.
        shared.borrow_mut().forget_endpoints(&ports);
        return Ok(());
    };
    // The task is bound to the target window's document as of post time
    // (<https://html.spec.whatwg.org/multipage/webappapis.html#queue-a-global-task>).
    let Some(document) = shared.borrow().document_id(FrameId::new(target)) else {
        shared.borrow_mut().forget_endpoints(&ports);
        return Ok(());
    };
    shared
        .borrow_mut()
        .deliveries
        .push(Delivery::WindowMessage {
            target: FrameId::new(target),
            document,
            source,
            origin,
            target_origin,
            payload,
            ports,
        });
    Ok(())
}

/// Validates a worker `postMessage` transfer list the way a window message is
/// validated: the payload must carry the shim's version marker and every
/// endpoint must be one the calling realm detached. A message that fails is
/// dropped, and `None` says so
/// (<https://html.spec.whatwg.org/multipage/web-messaging.html#transfer-receiving-steps>).
pub(super) fn accept_worker_transfer(
    ctx: &Ctx<'_>,
    payload: &str,
    ports: Vec<f64>,
) -> Option<Vec<u64>> {
    if !payload.starts_with(PAYLOAD_VERSION) {
        return None;
    }
    let ports = protocol_ids(ports)?;
    let world = world(ctx).ok()?;
    let (frame, shared) = {
        let world = world.borrow();
        (world.frame(), world.shared())
    };
    shared
        .borrow()
        .ports
        .all_in_transit_from(&ports, frame)
        .then_some(ports)
}

fn port_new(ctx: Ctx<'_>) -> Result<Vec<f64>> {
    let world = world(&ctx)?;
    let (frame, shared) = {
        let world = world.borrow();
        (world.frame(), world.shared())
    };
    let (first, second) = shared.borrow_mut().ports.new_pair(frame);
    Ok(vec![
        crate::js::js_number(first),
        crate::js::js_number(second),
    ])
}

fn port_post(ctx: Ctx<'_>, endpoint: f64, payload: String, ports: Vec<f64>) -> Result<()> {
    let Some(endpoint) = protocol_id(endpoint) else {
        return Ok(());
    };
    let Some(ports) = protocol_ids(ports) else {
        return Ok(());
    };
    if !payload.starts_with(PAYLOAD_VERSION) {
        return Ok(());
    }
    let world = world(&ctx)?;
    if !endpoint_owned(&world, endpoint) {
        return Ok(());
    }
    let (frame, shared) = {
        let world = world.borrow();
        (world.frame(), world.shared())
    };
    {
        // Every transferred endpoint must be one this frame detached
        // (<https://html.spec.whatwg.org/multipage/web-messaging.html#message-port-post-message-steps>).
        let shared = shared.borrow();
        if !shared.ports.all_in_transit_from(&ports, frame) {
            return Ok(());
        }
    }
    let mut shared = shared.borrow_mut();
    let Shared {
        ports: table,
        deliveries,
        ..
    } = &mut *shared;
    table.post(endpoint, payload, ports, deliveries);
    Ok(())
}

fn port_start(ctx: Ctx<'_>, endpoint: f64) -> Result<()> {
    let Some(endpoint) = protocol_id(endpoint) else {
        return Ok(());
    };
    let world = world(&ctx)?;
    if !endpoint_owned(&world, endpoint) {
        return Ok(());
    }
    let shared = world.borrow().shared();
    let mut shared = shared.borrow_mut();
    let Shared {
        ports: table,
        deliveries,
        ..
    } = &mut *shared;
    table.start(endpoint, deliveries);
    Ok(())
}

fn port_close(ctx: Ctx<'_>, endpoint: f64) -> Result<()> {
    let Some(endpoint) = protocol_id(endpoint) else {
        return Ok(());
    };
    let world = world(&ctx)?;
    if !endpoint_owned(&world, endpoint) {
        return Ok(());
    }
    let shared = world.borrow().shared();
    let mut shared = shared.borrow_mut();
    let Shared {
        ports: table,
        deliveries,
        ..
    } = &mut *shared;
    table.close(endpoint, deliveries);
    Ok(())
}

fn port_detach(ctx: Ctx<'_>, endpoint: f64) -> Result<bool> {
    let Some(endpoint) = protocol_id(endpoint) else {
        return Ok(false);
    };
    let world = world(&ctx)?;
    let (frame, shared) = {
        let world = world.borrow();
        (world.frame(), world.shared())
    };
    Ok(shared.borrow_mut().ports.detach(endpoint, frame))
}

fn port_adopt(ctx: Ctx<'_>, endpoint: f64) -> Result<Option<bool>> {
    let Some(endpoint) = protocol_id(endpoint) else {
        return Ok(None);
    };
    let world = world(&ctx)?;
    let (frame, shared) = {
        let world = world.borrow();
        (world.frame(), world.shared())
    };
    Ok(shared.borrow_mut().ports.adopt(endpoint, frame))
}

fn port_peer(ctx: Ctx<'_>, endpoint: f64) -> Result<Option<f64>> {
    let Some(endpoint) = protocol_id(endpoint) else {
        return Ok(None);
    };
    let world = world(&ctx)?;
    if !endpoint_owned(&world, endpoint) {
        return Ok(None);
    }
    let shared = world.borrow().shared();
    let Some(peer) = shared.borrow().ports.peer(endpoint) else {
        return Ok(None);
    };
    Ok(Some(crate::js::js_number(peer)))
}

fn frame_parent(ctx: Ctx<'_>, frame: f64) -> Result<Option<f64>> {
    let Some(frame) = protocol_id(frame) else {
        return Ok(None);
    };
    let world = world(&ctx)?;
    let shared = world.borrow().shared();
    let Some(parent) = shared.borrow().tree.parent(FrameId::new(frame)) else {
        return Ok(None);
    };
    Ok(Some(crate::js::js_number(parent.get())))
}

fn frame_top(ctx: Ctx<'_>, frame: f64) -> Result<Option<f64>> {
    let Some(frame) = protocol_id(frame) else {
        return Ok(None);
    };
    let mut frame = FrameId::new(frame);
    let world = world(&ctx)?;
    let shared = world.borrow().shared();
    let shared = shared.borrow();
    while let Some(parent) = shared.tree.parent(frame) {
        frame = parent;
    }
    Ok(Some(crate::js::js_number(frame.get())))
}

fn frame_child_count(ctx: Ctx<'_>, frame: f64) -> Result<f64> {
    let Some(frame) = protocol_id(frame) else {
        return Ok(0.0);
    };
    let world = world(&ctx)?;
    let shared = world.borrow().shared();
    let count = shared.borrow().tree.children(FrameId::new(frame)).len();
    Ok(crate::js::js_number(
        u64::try_from(count).unwrap_or(u64::MAX),
    ))
}

fn frame_child(ctx: Ctx<'_>, frame: f64, index: f64) -> Result<Option<f64>> {
    let Some(frame) = protocol_id(frame) else {
        return Ok(None);
    };
    let Some(index) = protocol_id(index).and_then(|index| usize::try_from(index).ok()) else {
        return Ok(None);
    };
    let world = world(&ctx)?;
    let shared = world.borrow().shared();
    let Some(child) = shared
        .borrow()
        .tree
        .children(FrameId::new(frame))
        .get(index)
        .copied()
    else {
        return Ok(None);
    };
    Ok(Some(crate::js::js_number(child.get())))
}

/// The target frame's world and whether it shares this realm's origin. `None`
/// when the frame is unknown or has no world. Every same-origin decision the
/// `WindowProxy` makes goes through this one predicate
/// (<https://html.spec.whatwg.org/multipage/window-object.html#windowproxy-get>).
fn frame_world_with_origin(
    ctx: &Ctx<'_>,
    frame: u64,
) -> Result<Option<(Rc<RefCell<World>>, bool)>> {
    let current = world(ctx)?;
    let Some(target) = current.borrow().frame_world(FrameId::new(frame)) else {
        return Ok(None);
    };
    let same_origin = target.borrow().origin == current.borrow().origin;
    Ok(Some((target, same_origin)))
}

/// Same-origin members forward through the target realm's window; a null
/// answer means the member is blocked cross-origin
/// (<https://html.spec.whatwg.org/multipage/window-object.html#windowproxy-get>).
fn frame_global(ctx: Ctx<'_>, frame: f64) -> Result<Option<Object<'_>>> {
    let Some(frame) = protocol_id(frame) else {
        return Ok(None);
    };
    let frame = FrameId::new(frame);
    let current = world(&ctx)?;
    if current.borrow().frame() == frame {
        return Ok(Some(ctx.globals()));
    }
    let Some((target, same_origin)) = frame_world_with_origin(&ctx, frame.get())? else {
        return Ok(None);
    };
    if !same_origin {
        return Ok(None);
    }
    let Some(window) = target.borrow().window_object() else {
        return Ok(None);
    };
    Ok(Some(window.restore(&ctx)?))
}

/// `document` on a same-origin proxy wraps the target frame's active document
/// in this realm; cross-origin access throws
/// (<https://html.spec.whatwg.org/multipage/iframe-embed-object.html#dom-iframe-contentdocument>).
pub(crate) fn frame_document(ctx: Ctx<'_>, frame: f64) -> Result<Value<'_>> {
    let Some(frame) = protocol_id(frame) else {
        return Ok(Value::new_null(ctx));
    };
    let Some((target, same_origin)) = frame_world_with_origin(&ctx, frame)? else {
        return Ok(Value::new_null(ctx));
    };
    let root = target.borrow().main_document_root();
    if !same_origin {
        return Err(throw_dom(
            &ctx,
            "SecurityError",
            "Blocked a frame from accessing a cross-origin frame.",
        ));
    }
    match root {
        Some(root) => wrap_node(&ctx, root),
        None => Ok(Value::new_null(ctx)),
    }
}

/// The browsing context name of `frame` for `window.name`
/// (<https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-name>).
fn frame_name(ctx: Ctx<'_>, frame: f64) -> Result<String> {
    let Some(frame) = protocol_id(frame) else {
        return Ok(String::new());
    };
    let world = world(&ctx)?;
    let shared = world.borrow().shared();
    Ok(shared.borrow().tree.name(FrameId::new(frame)).to_owned())
}

/// Sets the browsing context name of `frame` for `window.name`
/// (<https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-name>).
///
/// The tab's main frame name also goes to the browser: it is what a
/// `window.open` target lookup searches across tabs, and it has to survive the
/// renderer that reported it
/// (<https://html.spec.whatwg.org/multipage/document-sequences.html#the-rules-for-choosing-a-navigable>).
fn set_frame_name(ctx: Ctx<'_>, frame: f64, name: String) -> Result<()> {
    let Some(frame) = protocol_id(frame) else {
        return Ok(());
    };
    let world = world(&ctx)?;
    let frame = FrameId::new(frame);
    let (services, changed) = {
        let world = world.borrow();
        let services = world.runtime.services.clone();
        let shared_world = world.shared();
        let mut shared = shared_world.borrow_mut();
        let changed = shared.tree.name(frame) != name;
        shared.tree.set_name(frame, name.clone());
        (services, changed)
    };
    // The call is synchronous, so the world must not stay borrowed while it
    // runs: a host that answered by re-entering this realm would panic on a
    // second borrow. An unchanged name is not worth the round trip.
    if changed && frame == FrameId::MAIN {
        services.set_window_name(crate::protocol::NameChange::Assigned(
            crate::protocol::ContextName::parse(&name),
        ));
    }
    Ok(())
}

/// Whether `frame` has a browsing context, even when its realm is not
/// materialized yet. A script that sets a member on a just-inserted iframe's
/// `contentWindow` gets a stored write instead of a cross-origin error
/// (<https://html.spec.whatwg.org/multipage/window-object.html#windowproxy-set>).
fn frame_registered(ctx: Ctx<'_>, frame: f64) -> Result<bool> {
    let Some(frame) = protocol_id(frame) else {
        return Ok(false);
    };
    let world = world(&ctx)?;
    let shared = world.borrow().shared();
    Ok(shared.borrow().tree.contains(FrameId::new(frame)))
}

/// A same-origin frame's world, whether or not its realm has started. `None`
/// for a cross-origin, missing, or unknown frame
/// (<https://html.spec.whatwg.org/multipage/window-object.html#windowproxy-get>).
fn same_origin_frame(ctx: &Ctx<'_>, frame: f64) -> Result<Option<Rc<RefCell<World>>>> {
    let Some(frame) = protocol_id(frame) else {
        return Ok(None);
    };
    Ok(frame_world_with_origin(ctx, frame)?
        .and_then(|(target, same_origin)| if same_origin { Some(target) } else { None }))
}

/// Whether a not-yet-materialized frame is same-origin: an `iframe` whose
/// document exists but whose `QuickJS` realm cannot start until the inserting
/// script returns. The `WindowProxy` uses it to answer the target window's
/// globals and `location` instead of returning `undefined`
/// (<https://html.spec.whatwg.org/multipage/window-object.html#windowproxy-get>).
fn frame_same_origin(ctx: Ctx<'_>, frame: f64) -> Result<bool> {
    Ok(same_origin_frame(&ctx, frame)?.is_some())
}

/// The target window's `Location` for a frame whose realm has not started,
/// built in the caller's realm but bound to the target frame's world
/// (<https://html.spec.whatwg.org/multipage/nav-history-apis.html#the-location-interface>).
fn frame_location(ctx: Ctx<'_>, frame: f64) -> Result<Option<Object<'_>>> {
    let Some(target) = same_origin_frame(&ctx, frame)? else {
        return Ok(None);
    };
    Ok(Some(super::window::build_location(&ctx, &target)?))
}

/// The `Window.location` setter for a frame whose realm has not started: it
/// parses the URL against the entry settings object and queues the target's
/// navigation, throwing the same `SyntaxError` as the installed accessor
/// (<https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-location-href>).
fn frame_set_location(ctx: Ctx<'_>, frame: f64, value: Value<'_>) -> Result<()> {
    let Some(target) = same_origin_frame(&ctx, frame)? else {
        return Err(throw_dom(
            &ctx,
            "SecurityError",
            "Blocked a frame from accessing a cross-origin frame.",
        ));
    };
    super::window::set_location_href(&target, value)
}

/// Event handler property names whose values live in the world, so a wrapper
/// can be collected without losing `element.onload`
/// (<https://html.spec.whatwg.org/multipage/webappapis.html#event-handler-idl-attributes>).
pub(crate) const HANDLER_ATTRIBUTES: &[&str] = &[
    "onabort",
    // CSS Animations and Transitions: the unprefixed handlers sit beside the
    // `webkit`-prefixed legacy aliases, which stay separate properties
    // (<https://drafts.csswg.org/css-animations-1/#extensions-to-the-globaleventhandlers-mixin>,
    // <https://drafts.csswg.org/css-transitions-1/#extensions-to-the-globaleventhandlers-mixin>).
    "onanimationcancel",
    "onanimationend",
    "onanimationiteration",
    "onanimationstart",
    "onauxclick",
    "onbeforeinput",
    "onbeforematch",
    "onbeforetoggle",
    "onblur",
    "oncancel",
    "oncanplay",
    "oncanplaythrough",
    "onchange",
    "onclick",
    "onclose",
    "oncommand",
    "oncontextlost",
    "oncontextmenu",
    "oncontextrestored",
    "oncopy",
    "oncuechange",
    "oncut",
    "ondblclick",
    "ondrag",
    "ondragend",
    "ondragenter",
    "ondragleave",
    "ondragover",
    "ondragstart",
    "ondrop",
    "ondurationchange",
    "onemptied",
    "onended",
    "onerror",
    "onfocus",
    "onformdata",
    "oninput",
    "oninvalid",
    "onkeydown",
    "onkeypress",
    "onkeyup",
    "onload",
    "onloadeddata",
    "onloadedmetadata",
    "onloadstart",
    "onmousedown",
    "onmouseenter",
    "onmouseleave",
    "onmousemove",
    "onmouseout",
    "onmouseover",
    "onmouseup",
    "onpaste",
    "onpause",
    "onplay",
    "onplaying",
    // Pointer Events extends GlobalEventHandlers
    // (<https://w3c.github.io/pointerevents/#extensions-to-the-globaleventhandlers-mixin>).
    "onpointercancel",
    "onpointerdown",
    "onpointerenter",
    "onpointerleave",
    "onpointermove",
    "onpointerout",
    "onpointerover",
    "onpointerrawupdate",
    "onpointerup",
    "ongotpointercapture",
    "onlostpointercapture",
    "onprogress",
    "onratechange",
    "onreset",
    "onresize",
    "onscroll",
    "onscrollend",
    "onsecuritypolicyviolation",
    "onseeked",
    "onseeking",
    "onselect",
    "onselectionchange",
    "onselectstart",
    "onslotchange",
    "onsnapchanged",
    "onsnapchanging",
    "onstalled",
    "onsubmit",
    "onsuspend",
    "ontimeupdate",
    "ontoggle",
    // Touch Events extends GlobalEventHandlers
    // (<https://w3c.github.io/touch-events/#extensions-to-the-globaleventhandlers-mixin>).
    "ontouchcancel",
    "ontouchend",
    "ontouchmove",
    "ontouchstart",
    "ontransitioncancel",
    "ontransitionend",
    "ontransitionrun",
    "ontransitionstart",
    "onvolumechange",
    "onwaiting",
    "onwebkitanimationend",
    "onwebkitanimationiteration",
    "onwebkitanimationstart",
    "onwebkittransitionend",
    "onwheel",
];

/// `WindowEventHandlers` names. A `div` has no such handler; `body` and
/// `frameset` expose them by forwarding to their document's window
/// (<https://html.spec.whatwg.org/multipage/webappapis.html#windoweventhandlers>).
pub(crate) const WINDOW_HANDLER_NAMES: &[&str] = &[
    "onafterprint",
    "onbeforeprint",
    "onbeforeunload",
    "onhashchange",
    "onlanguagechange",
    "onmessage",
    "onmessageerror",
    "onoffline",
    "ononline",
    "onpagehide",
    "onpagereveal",
    "onpageshow",
    "onpageswap",
    "onpopstate",
    "onrejectionhandled",
    "onstorage",
    "onunhandledrejection",
    "onunload",
];

/// The six `GlobalEventHandlers` names a `body` or `frameset` element replaces
/// with its window's handler, in addition to every `WindowEventHandlers` name
/// (<https://html.spec.whatwg.org/multipage/webappapis.html#window-reflecting-body-element-event-handler-set>).
pub(crate) const BODY_WINDOW_HANDLERS: &[&str] = &[
    "onblur", "onerror", "onfocus", "onload", "onresize", "onscroll",
];

/// Whether a `body` or `frameset` element forwards `name` to its document's
/// window instead of keeping a slot of its own.
pub(crate) fn is_window_reflecting_handler(name: &str) -> bool {
    BODY_WINDOW_HANDLERS.contains(&name) || WINDOW_HANDLER_NAMES.contains(&name)
}

/// Event handler attributes `Document` declares itself, outside
/// `GlobalEventHandlers` and `WindowEventHandlers`. They are installed on
/// `Document.prototype` only, so an element has no such property
/// (<https://html.spec.whatwg.org/multipage/dom.html#the-document-object>,
/// <https://w3c.github.io/page-lifecycle/#sec-api>,
/// <https://fullscreen.spec.whatwg.org/#api>,
/// <https://w3c.github.io/pointerlock/#extensions-to-the-document-interface>).
pub(crate) const DOCUMENT_HANDLER_NAMES: &[&str] = &[
    "onfullscreenchange",
    "onfullscreenerror",
    "onfreeze",
    "onpointerlockchange",
    "onpointerlockerror",
    "onreadystatechange",
    "onresume",
    "onvisibilitychange",
];

pub(crate) fn get_node_handler<'js>(
    ctx: Ctx<'js>,
    node: Value<'js>,
    name: String,
) -> Result<Value<'js>> {
    let id = handler_target(&ctx, &node)?;
    get_handler(&ctx, Some(id), &name)
}

pub(crate) fn set_node_handler<'js>(
    ctx: Ctx<'js>,
    node: Value<'js>,
    name: String,
    value: Value<'js>,
) -> Result<()> {
    let id = handler_target(&ctx, &node)?;
    set_handler(&ctx, Some(id), &name, value)
}

/// The stored value of a handler a `body` or `frameset` element mirrors from
/// its document's window.
///
/// The `HTMLBodyElement`/`HTMLFrameSetElement` prototypes route every
/// `WindowEventHandlers` attribute and the six `GlobalEventHandlers` in the
/// window-reflecting set here, so the element has no slot of its own. A
/// document with no browsing context answers null, and a set is ignored
/// (<https://html.spec.whatwg.org/multipage/webappapis.html#window-reflecting-body-element-event-handler-set>,
/// <https://html.spec.whatwg.org/multipage/dom.html#body-element-event-handlers>).
pub(crate) fn get_window_reflecting_handler<'js>(
    ctx: Ctx<'js>,
    node: Value<'js>,
    name: String,
) -> Result<Value<'js>> {
    let id = handler_target(&ctx, &node)?;
    match super::owning_world(&ctx, id) {
        // Only the realm's active document has a window to forward to. A
        // document created by `DOMParser` or `new Document()` belongs to the
        // realm for wrapper bookkeeping but has no browsing context, so its
        // body reads null.
        Some(world) if world.borrow().main_document_id() == Some(id.document_id()) => {
            get_handler_in(&world, None, &name, &ctx)
        }
        _ => Ok(Value::new_null(ctx.clone())),
    }
}

/// Stores a handler a `body` or `frameset` element mirrors from its window; a
/// document with no browsing context ignores the write.
pub(crate) fn set_window_reflecting_handler<'js>(
    ctx: Ctx<'js>,
    node: Value<'js>,
    name: String,
    value: Value<'js>,
) -> Result<()> {
    let id = handler_target(&ctx, &node)?;
    if let Some(world) = super::owning_world(&ctx, id)
        && world.borrow().main_document_id() == Some(id.document_id())
    {
        set_handler_in(&world, None, &name, value, &ctx);
    }
    Ok(())
}

pub(crate) fn get_window_handler(ctx: Ctx<'_>, name: String) -> Result<Value<'_>> {
    let world = world(&ctx)?;
    get_handler_in(&world, None, &name, &ctx)
}

pub(crate) fn set_window_handler<'js>(
    ctx: Ctx<'js>,
    name: String,
    value: Value<'js>,
) -> Result<()> {
    let world = world(&ctx)?;
    set_handler_in(&world, None, &name, value, &ctx);
    Ok(())
}

/// The stored value of the handler property `name` on the window (`None`
/// target) or on `target`.
fn get_handler<'js>(ctx: &Ctx<'js>, target: Option<NodeId>, name: &str) -> Result<Value<'js>> {
    let world = match target {
        Some(id) => world_for_node(ctx, id)?,
        None => world(ctx)?,
    };
    get_handler_in(&world, target, name, ctx)
}

fn get_handler_in<'js>(
    world: &Rc<RefCell<World>>,
    target: Option<NodeId>,
    name: &str,
    ctx: &Ctx<'js>,
) -> Result<Value<'js>> {
    match world.borrow().handler_attribute(target, name) {
        Some(saved) => saved.restore(ctx),
        None => Ok(Value::new_null(ctx.clone())),
    }
}

/// Stores the handler property `name` on the window (`None` target) or on
/// `target`. A non-object clears it, including `null` and `undefined`
/// (<https://webidl.spec.whatwg.org/#LegacyTreatNonObjectAsNull>).
fn set_handler<'js>(
    ctx: &Ctx<'js>,
    target: Option<NodeId>,
    name: &str,
    value: Value<'js>,
) -> Result<()> {
    let world = match target {
        Some(id) => world_for_node(ctx, id)?,
        None => world(ctx)?,
    };
    set_handler_in(&world, target, name, value, ctx);
    Ok(())
}

fn set_handler_in<'js>(
    world: &Rc<RefCell<World>>,
    target: Option<NodeId>,
    name: &str,
    value: Value<'js>,
    ctx: &Ctx<'js>,
) {
    // An event handler IDL attribute converts through a callback type marked
    // `[LegacyTreatNonObjectAsNull]`: any value that is not an object clears
    // the handler instead of being stored or throwing. Without this, assigning
    // a string leaves a non-callable value in the slot and a later read
    // returns it
    // (<https://webidl.spec.whatwg.org/#LegacyTreatNonObjectAsNull>,
    // <https://html.spec.whatwg.org/multipage/webappapis.html#event-handler-idl-attributes>).
    let saved = if value.is_object() {
        Some(Persistent::save(ctx, value))
    } else {
        None
    };
    world
        .borrow_mut()
        .set_handler_attribute(target, name, saved);
}

/// Whether the calling realm's frame owns `endpoint`.
///
/// The `__tb*` host functions are reachable from page script, so every
/// endpoint operation is checked against the caller's frame; otherwise a page
/// could close or steal another frame's port by guessing its id
/// (<https://html.spec.whatwg.org/multipage/web-messaging.html#transfer-receiving-steps>).
fn endpoint_owned(world: &Rc<RefCell<World>>, endpoint: u64) -> bool {
    let (frame, shared) = {
        let world = world.borrow();
        (world.frame(), world.shared())
    };
    shared.borrow().ports.owner(endpoint) == Some(frame)
}

/// A JS number naming a protocol id, read as an `unsigned long long`.
fn protocol_id(value: f64) -> Option<u64> {
    crate::js::bridge::enforce_range_u64(value)
}

fn protocol_ids(values: Vec<f64>) -> Option<Vec<u64>> {
    values.into_iter().map(protocol_id).collect()
}
