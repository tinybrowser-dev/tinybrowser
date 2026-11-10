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
    let shared = world.borrow().shared();
    let mut shared = shared.borrow_mut();
    let frame = FrameId::new(frame);
    if shared.tree.opener(frame) != Some(caller) {
        return;
    }
    shared.close_auxiliary(frame);
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
    let Some(target) = current.borrow().frame_world(frame) else {
        return Ok(None);
    };
    let same_origin = target.borrow().origin == current.borrow().origin;
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
    let current = world(&ctx)?;
    let Some(target) = current.borrow().frame_world(FrameId::new(frame)) else {
        return Ok(Value::new_null(ctx));
    };
    let root = target.borrow().main_document_root();
    let same_origin = target.borrow().origin == current.borrow().origin;
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

/// Event handler property names whose values live in the world, so a wrapper
/// can be collected without losing `element.onload`
/// (<https://html.spec.whatwg.org/multipage/webappapis.html#event-handler-idl-attributes>).
pub(crate) const HANDLER_ATTRIBUTES: &[&str] = &[
    "onabort",
    "onblur",
    "oncancel",
    "onchange",
    "onclick",
    "onclose",
    "oncontextmenu",
    "oncopy",
    "oncut",
    "ondblclick",
    "ondrag",
    "ondragend",
    "ondragenter",
    "ondragleave",
    "ondragover",
    "ondragstart",
    "ondrop",
    "onerror",
    "onfocus",
    "oninput",
    "oninvalid",
    "onkeydown",
    "onkeypress",
    "onkeyup",
    "onload",
    "onloadeddata",
    "onloadedmetadata",
    "onloadstart",
    "onmessage",
    "onmessageerror",
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
    "onprogress",
    "onratechange",
    "onreadystatechange",
    "onreset",
    "onresize",
    "onscroll",
    "onseeked",
    "onseeking",
    "onselect",
    "onsubmit",
    "onsuspend",
    "ontimeupdate",
    "ontoggle",
    "onwheel",
];

/// The "Window-reflecting body element event handler set": handlers a `body`
/// or `frameset` element mirrors from its `Window`, replacing the generic
/// element handler of the same name
/// (<https://html.spec.whatwg.org/multipage/webappapis.html#window-reflecting-body-element-event-handler-set>).
const BODY_WINDOW_HANDLERS: &[&str] = &[
    "onblur", "onerror", "onfocus", "onload", "onresize", "onscroll",
];

pub(crate) fn get_node_handler<'js>(
    ctx: Ctx<'js>,
    node: Value<'js>,
    name: String,
) -> Result<Value<'js>> {
    let id = handler_target(&ctx, &node)?;
    if let Some(world) = mirrored_window_handler(&ctx, id, &name) {
        return get_handler_in(&world, None, &name, &ctx);
    }
    get_handler(&ctx, Some(id), &name)
}

pub(crate) fn set_node_handler<'js>(
    ctx: Ctx<'js>,
    node: Value<'js>,
    name: String,
    value: Value<'js>,
) -> Result<()> {
    let id = handler_target(&ctx, &node)?;
    if let Some(world) = mirrored_window_handler(&ctx, id, &name) {
        set_handler_in(&world, None, &name, value, &ctx);
        return Ok(());
    }
    set_handler(&ctx, Some(id), &name, value)
}

/// The world whose `Window` holds `name` for `id`, when `id` is a `body` or
/// `frameset` element reflecting that handler.
fn mirrored_window_handler(ctx: &Ctx<'_>, id: NodeId, name: &str) -> Option<Rc<RefCell<World>>> {
    if !BODY_WINDOW_HANDLERS.contains(&name) {
        return None;
    }
    let world = world_for_node(ctx, id).ok()?;
    let reflected = world.borrow().document(id).is_some_and(|parsed| {
        crate::dom::tree::is_html_element(&parsed.document.base, id.node, "body")
            || crate::dom::tree::is_html_element(&parsed.document.base, id.node, "frameset")
    });
    reflected.then_some(world)
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
/// `target`; `null` and `undefined` clear it.
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
    let saved = if value.is_null() || value.is_undefined() {
        None
    } else {
        Some(Persistent::save(ctx, value))
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
