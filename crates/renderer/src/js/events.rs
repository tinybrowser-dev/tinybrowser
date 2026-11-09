//! DOM events: the `Event` and `EventTarget` platform objects and the
//! dispatch algorithm (<https://dom.spec.whatwg.org/#events>).
//!
//! One event listener list is keyed per target in the [`World`] that owns the
//! target, so nodes, the window, and constructible `EventTarget`s all share
//! the same add/remove/invoke code.

use std::cell::{Cell, Ref, RefCell, RefMut};
use std::rc::Rc;
use std::sync::OnceLock;
use std::time::Instant;

use rquickjs::{
    Class, Ctx, Exception, FromJs, Function, Object, Persistent, Result, Value,
    class::{Trace, Tracer},
    function::{Rest, This},
};

use super::bindings;
use super::bindings::JsNode;
use super::world::{EventTargetKey, Listener, World};

include!(concat!(env!("OUT_DIR"), "/Event.rs"));
include!(concat!(env!("OUT_DIR"), "/EventTarget.rs"));

/// `Event.NONE` (<https://dom.spec.whatwg.org/#dom-event-none>).
pub(crate) const NONE: u16 = 0;
/// `Event.CAPTURING_PHASE` (<https://dom.spec.whatwg.org/#dom-event-capturing_phase>).
pub(crate) const CAPTURING_PHASE: u16 = 1;
/// `Event.AT_TARGET` (<https://dom.spec.whatwg.org/#dom-event-at_target>).
pub(crate) const AT_TARGET: u16 = 2;
/// `Event.BUBBLING_PHASE` (<https://dom.spec.whatwg.org/#dom-event-bubbling_phase>).
pub(crate) const BUBBLING_PHASE: u16 = 3;

/// `DOMHighResTimeStamp` for `Event.timeStamp`, relative to this process's
/// first event (<https://dom.spec.whatwg.org/#dom-event-timestamp>).
fn now_millis() -> f64 {
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    let origin = ORIGIN.get_or_init(Instant::now);
    origin.elapsed().as_secs_f64() * 1000.0
}

/// One event target together with the world that owns it
/// (<https://dom.spec.whatwg.org/#concept-event-target>).
///
/// The world is needed to resolve a `Window` target from a realm that is not
/// the target's own: `ctx.globals()` is the caller's window.
#[derive(Clone)]
pub(crate) struct EventTargetRef {
    pub(crate) key: EventTargetKey,
    pub(crate) world: Rc<RefCell<World>>,
}

/// The mutable state of one `Event` object
/// (<https://dom.spec.whatwg.org/#concept-event>).
///
/// Targets and path entries are [`EventTargetRef`]s, not JS values: a
/// `Persistent` stored inside a class instance pins the `QuickJS` context
/// across realm teardown, which aborts the runtime at exit. References resolve
/// to wrappers on demand and hold no JS reference.
///
/// The booleans mirror the specification's event flags one-for-one; a bitmask
/// would obscure that mapping.
#[expect(
    clippy::struct_excessive_bools,
    reason = "the DOM event flag set is specified as individual flags"
)]
#[derive(Default)]
pub(crate) struct EventState {
    pub(crate) typ: String,
    pub(crate) bubbles: bool,
    pub(crate) cancelable: bool,
    pub(crate) composed: bool,
    pub(crate) is_trusted: bool,
    pub(crate) initialized: bool,
    pub(crate) time_stamp: f64,
    pub(crate) target: Option<EventTargetRef>,
    pub(crate) current_target: Option<EventTargetRef>,
    pub(crate) phase: u16,
    pub(crate) stop_propagation: bool,
    pub(crate) stop_immediate: bool,
    pub(crate) canceled: bool,
    pub(crate) in_passive: bool,
    pub(crate) dispatching: bool,
    pub(crate) path: Vec<EventTargetRef>,
}

pub(crate) struct EventStateCell(RefCell<EventState>);

impl<'js> Trace<'js> for EventStateCell {
    fn trace<'a>(&self, _tracer: Tracer<'a, 'js>) {}
}

/// The `CustomEvent` platform object.
///
/// `detail` is the one `any` attribute of the interface, so it lives as a
/// symbol-keyed own property on the object (`install_custom_event_js`) rather
/// than in this struct; every Rust-held JS value would pin the context.
#[derive(Trace, rquickjs::JsLifetime)]
pub struct JsEvent {
    pub(crate) state: EventStateCell,
}

impl JsEvent {
    /// A user-agent-created event: `isTrusted` true and `initialized` set
    /// (<https://dom.spec.whatwg.org/#concept-event-create>).
    pub(crate) fn trusted(typ: &str, bubbles: bool, cancelable: bool) -> Self {
        Self::trusted_with(typ, bubbles, cancelable, false)
    }

    /// A trusted event whose `composed` flag is set, for the input events that
    /// cross shadow boundaries.
    pub(crate) fn trusted_composed(typ: &str, bubbles: bool, cancelable: bool) -> Self {
        Self::trusted_with(typ, bubbles, cancelable, true)
    }

    fn trusted_with(typ: &str, bubbles: bool, cancelable: bool, composed: bool) -> Self {
        Self {
            state: EventStateCell(RefCell::new(EventState {
                typ: typ.to_owned(),
                bubbles,
                cancelable,
                composed,
                is_trusted: true,
                initialized: true,
                time_stamp: now_millis(),
                ..EventState::default()
            })),
        }
    }

    /// The event `Document.createEvent()` returns: type empty and
    /// `initialized` unset until `initEvent()`
    /// (<https://dom.spec.whatwg.org/#dom-document-createevent>).
    pub(crate) fn uninitialized() -> Self {
        Self {
            state: EventStateCell(RefCell::new(EventState {
                time_stamp: now_millis(),
                ..EventState::default()
            })),
        }
    }

    pub(crate) fn state(&self) -> Ref<'_, EventState> {
        self.state.0.borrow()
    }

    pub(crate) fn state_mut(&self) -> RefMut<'_, EventState> {
        self.state.0.borrow_mut()
    }

    /// [Initialize](https://dom.spec.whatwg.org/#concept-event-initialize) the
    /// event. `initEvent()` and `initCustomEvent()` both land here.
    pub(crate) fn initialize(&self, typ: String, bubbles: bool, cancelable: bool) {
        let mut state = self.state_mut();
        state.initialized = true;
        state.stop_propagation = false;
        state.stop_immediate = false;
        state.canceled = false;
        state.is_trusted = false;
        state.target = None;
        state.typ = typ;
        state.bubbles = bubbles;
        state.cancelable = cancelable;
    }

    /// Build an initialized event from an already-converted constructor
    /// argument set (<https://dom.spec.whatwg.org/#dom-event-event>).
    fn from_init(typ: String, init: &event_generated::EventInit) -> Self {
        Self {
            state: EventStateCell(RefCell::new(EventState {
                typ,
                bubbles: init.bubbles,
                cancelable: init.cancelable,
                composed: init.composed,
                initialized: true,
                time_stamp: now_millis(),
                ..EventState::default()
            })),
        }
    }

    /// [Set the canceled flag](https://dom.spec.whatwg.org/#set-the-canceled-flag).
    fn set_canceled_flag(&self) {
        let mut state = self.state_mut();
        if state.canceled || state.in_passive || !state.cancelable {
            return;
        }
        state.canceled = true;
    }
}

impl<'js> event_generated::Event<'js> for JsEvent {
    // https://dom.spec.whatwg.org/#dom-event-event
    fn constructor(
        _ctx: &Ctx<'js>,
        typ: rquickjs::String<'js>,
        init: event_generated::EventInit,
    ) -> Result<Self> {
        Ok(Self::from_init(typ.to_string()?, &init))
    }

    // https://dom.spec.whatwg.org/#dom-event-type
    fn get_type(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        rquickjs::String::from_str(ctx.clone(), &self.state().typ)
    }

    // https://dom.spec.whatwg.org/#dom-event-target
    fn get_target(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        stored_target(ctx, self.state().target.as_ref())
    }

    // https://dom.spec.whatwg.org/#dom-event-srcelement
    fn get_src_element(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        stored_target(ctx, self.state().target.as_ref())
    }

    // https://dom.spec.whatwg.org/#dom-event-currenttarget
    fn get_current_target(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        stored_target(ctx, self.state().current_target.as_ref())
    }

    // https://dom.spec.whatwg.org/#dom-event-eventphase
    fn get_event_phase(&self, _ctx: &Ctx<'_>) -> Result<u16> {
        Ok(self.state().phase)
    }

    // https://dom.spec.whatwg.org/#dom-event-bubbles
    fn get_bubbles(&self, _ctx: &Ctx<'_>) -> Result<bool> {
        Ok(self.state().bubbles)
    }

    // https://dom.spec.whatwg.org/#dom-event-cancelable
    fn get_cancelable(&self, _ctx: &Ctx<'_>) -> Result<bool> {
        Ok(self.state().cancelable)
    }

    // https://dom.spec.whatwg.org/#dom-event-composed
    fn get_composed(&self, _ctx: &Ctx<'_>) -> Result<bool> {
        Ok(self.state().composed)
    }

    // https://dom.spec.whatwg.org/#dom-event-defaultprevented
    fn get_default_prevented(&self, _ctx: &Ctx<'_>) -> Result<bool> {
        Ok(self.state().canceled)
    }

    // https://dom.spec.whatwg.org/#dom-event-istrusted
    fn get_is_trusted(&self, _ctx: &Ctx<'_>) -> Result<bool> {
        Ok(self.state().is_trusted)
    }

    // https://dom.spec.whatwg.org/#dom-event-timestamp
    fn get_time_stamp(&self, _ctx: &Ctx<'_>) -> Result<f64> {
        Ok(self.state().time_stamp)
    }

    // https://dom.spec.whatwg.org/#dom-event-cancelbubble
    fn get_cancel_bubble(&self, _ctx: &Ctx<'_>) -> Result<bool> {
        Ok(self.state().stop_propagation)
    }

    // https://dom.spec.whatwg.org/#dom-event-cancelbubble
    fn set_cancel_bubble(&self, _ctx: &Ctx<'_>, value: bool) -> Result<()> {
        if value {
            self.state_mut().stop_propagation = true;
        }
        Ok(())
    }

    // https://dom.spec.whatwg.org/#dom-event-returnvalue
    fn get_return_value(&self, _ctx: &Ctx<'_>) -> Result<bool> {
        Ok(!self.state().canceled)
    }

    // https://dom.spec.whatwg.org/#dom-event-returnvalue
    fn set_return_value(&self, _ctx: &Ctx<'_>, value: bool) -> Result<()> {
        if !value {
            self.set_canceled_flag();
        }
        Ok(())
    }

    // https://dom.spec.whatwg.org/#dom-event-stoppropagation
    fn stop_propagation(&self, _ctx: Ctx<'_>) -> Result<()> {
        self.state_mut().stop_propagation = true;
        Ok(())
    }

    // https://dom.spec.whatwg.org/#dom-event-stopimmediatepropagation
    fn stop_immediate_propagation(&self, _ctx: Ctx<'_>) -> Result<()> {
        let mut state = self.state_mut();
        state.stop_propagation = true;
        state.stop_immediate = true;
        Ok(())
    }

    // https://dom.spec.whatwg.org/#dom-event-preventdefault
    fn prevent_default(&self, _ctx: Ctx<'_>) -> Result<()> {
        self.set_canceled_flag();
        Ok(())
    }

    // https://dom.spec.whatwg.org/#dom-event-composedpath
    fn composed_path(&self, ctx: Ctx<'js>) -> Result<Vec<Value<'js>>> {
        let state = self.state();
        state
            .path
            .iter()
            .map(|reference| resolve_target(&ctx, reference))
            .collect()
    }

    // https://dom.spec.whatwg.org/#dom-event-initevent
    fn init_event(
        &self,
        _ctx: Ctx<'js>,
        typ: rquickjs::String<'js>,
        bubbles: bool,
        cancelable: bool,
    ) -> Result<()> {
        // Web IDL converts the arguments before the algorithm runs; the
        // generated dispatch already did, so a dispatch flag returns without
        // re-initializing.
        if self.state().dispatching {
            return Ok(());
        }
        self.initialize(typ.to_string()?, bubbles, cancelable);
        Ok(())
    }
}

/// The constructible `EventTarget` interface
/// (<https://dom.spec.whatwg.org/#interface-eventtarget>).
///
/// Nodes get their own copies of the three methods on the JavaScript `Node`
/// prototype (the generated `EventTarget` installer wires them from the
/// `JsNode` implementation), so these methods only ever see `new EventTarget()`
/// receivers. That matters because rquickjs methods brand-check their receiver
/// as the defining class.
#[derive(Trace, rquickjs::JsLifetime)]
pub struct JsEventTarget {
    id: u64,
}

impl<'js> event_target_generated::EventTarget<'js> for JsEventTarget {
    // https://dom.spec.whatwg.org/#dom-eventtarget-eventtarget
    fn constructor(ctx: &Ctx<'js>) -> Result<Self> {
        let id = bindings::world(ctx)?.borrow_mut().next_standalone_target();
        Ok(Self { id })
    }

    // https://dom.spec.whatwg.org/#dom-eventtarget-addeventlistener
    fn add_event_listener(
        &self,
        ctx: Ctx<'js>,
        this: Object<'js>,
        typ: rquickjs::String<'js>,
        callback: Value<'js>,
        options: event_target_generated::AddEventListenerOptionsOrBoolean,
    ) -> Result<()> {
        register_standalone(&ctx, self.id, &this)?;
        let callback = listener_callback(&ctx, callback)?;
        let options = listener_options(options);
        add_listener_parsed(
            &ctx,
            EventTargetKey::Standalone(self.id),
            typ.to_string()?,
            callback,
            options,
        )
    }

    // https://dom.spec.whatwg.org/#dom-eventtarget-removeeventlistener
    fn remove_event_listener(
        &self,
        ctx: Ctx<'js>,
        this: Object<'js>,
        typ: rquickjs::String<'js>,
        callback: Value<'js>,
        options: event_target_generated::BooleanOrEventListenerOptions,
    ) -> Result<()> {
        register_standalone(&ctx, self.id, &this)?;
        let callback = listener_callback(&ctx, callback)?;
        let capture = remove_capture(options);
        remove_listener_parsed(
            &ctx,
            EventTargetKey::Standalone(self.id),
            &typ.to_string()?,
            callback.as_ref(),
            capture,
        )
    }

    // https://dom.spec.whatwg.org/#dom-eventtarget-dispatchevent
    fn dispatch_event(&self, ctx: Ctx<'js>, this: Object<'js>, event: Value<'js>) -> Result<bool> {
        register_standalone(&ctx, self.id, &this)?;
        let event = Class::<JsEvent>::from_js(&ctx, event)?;
        dispatch_event(&ctx, EventTargetKey::Standalone(self.id), &event)
    }
}

/// Every node interface is also an `EventTarget`, so the one contract
/// dispatches node receivers to the node key.
impl<'js> event_target_generated::EventTarget<'js> for JsNode {
    fn constructor(ctx: &Ctx<'js>) -> Result<Self> {
        Err(rquickjs::Exception::throw_type(ctx, "Illegal constructor"))
    }

    fn add_event_listener(
        &self,
        ctx: Ctx<'js>,
        _this: Object<'js>,
        typ: rquickjs::String<'js>,
        callback: Value<'js>,
        options: event_target_generated::AddEventListenerOptionsOrBoolean,
    ) -> Result<()> {
        let callback = listener_callback(&ctx, callback)?;
        let options = listener_options(options);
        add_listener_parsed(
            &ctx,
            EventTargetKey::Node(self.node_id()),
            typ.to_string()?,
            callback,
            options,
        )
    }

    fn remove_event_listener(
        &self,
        ctx: Ctx<'js>,
        _this: Object<'js>,
        typ: rquickjs::String<'js>,
        callback: Value<'js>,
        options: event_target_generated::BooleanOrEventListenerOptions,
    ) -> Result<()> {
        let callback = listener_callback(&ctx, callback)?;
        let capture = remove_capture(options);
        remove_listener_parsed(
            &ctx,
            EventTargetKey::Node(self.node_id()),
            &typ.to_string()?,
            callback.as_ref(),
            capture,
        )
    }

    fn dispatch_event(&self, ctx: Ctx<'js>, _this: Object<'js>, event: Value<'js>) -> Result<bool> {
        let event = Class::<JsEvent>::from_js(&ctx, event)?;
        dispatch_event(&ctx, EventTargetKey::Node(self.node_id()), &event)
    }
}

/// The `addEventListener` options union as parsed listener options.
pub(crate) fn listener_options(
    options: event_target_generated::AddEventListenerOptionsOrBoolean,
) -> ListenerOptions {
    match options {
        event_target_generated::AddEventListenerOptionsOrBoolean::Boolean(capture) => {
            ListenerOptions {
                capture,
                once: false,
                passive: None,
                signal: None,
            }
        }
        event_target_generated::AddEventListenerOptionsOrBoolean::AddEventListenerOptions(
            options,
        ) => ListenerOptions {
            capture: options.capture,
            once: options.once,
            passive: options.passive,
            signal: options.signal,
        },
    }
}

/// `removeEventListener` reads only `capture`
/// (<https://dom.spec.whatwg.org/#dom-eventtarget-removeeventlistener>).
pub(crate) fn remove_capture(
    options: event_target_generated::BooleanOrEventListenerOptions,
) -> bool {
    match options {
        event_target_generated::BooleanOrEventListenerOptions::Boolean(capture) => capture,
        event_target_generated::BooleanOrEventListenerOptions::EventListenerOptions(options) => {
            options.capture
        }
    }
}

pub(crate) fn install_event_target_bridge(ctx: &Ctx<'_>) -> Result<()> {
    super::bridge::object(ctx)?.set(
        "__tbDispatchTargetTrusted",
        rquickjs::prelude::Func::from(dispatch_trusted_bridge),
    )?;
    // Forge-proof event brand check for the script getters: prototype-chain
    // walks (`instanceof`) pass for `Object.create(MouseEvent.prototype)`,
    // but only genuine events (native or constructed through the shared
    // native prototype) convert to `Class<JsEvent>`.
    super::bridge::object(ctx)?.set(
        "__tbIsEvent",
        rquickjs::prelude::Func::from(is_event_bridge),
    )?;
    Ok(())
}

fn is_event_bridge<'js>(ctx: Ctx<'js>, value: Value<'js>) -> bool {
    Class::<JsEvent>::from_js(&ctx, value).is_ok()
}

fn dispatch_trusted_bridge<'js>(
    ctx: Ctx<'js>,
    target: Object<'js>,
    event: Class<'js, JsEvent>,
) -> Result<bool> {
    // https://dom.spec.whatwg.org/#concept-event-fire
    if let Some(node) = super::bindings::host_node_id(&ctx, target.as_value()) {
        return dispatch_trusted_event(&ctx, EventTargetKey::Node(node), &event);
    }
    let class = Class::<JsEventTarget>::from_js(&ctx, target.clone().into_value())?;
    let id = class.borrow().id;
    register_standalone(&ctx, id, &target)?;
    dispatch_trusted_event(&ctx, EventTargetKey::Standalone(id), &event)
}

/// Wraps the native `EventTarget` constructor so a call without `new` throws
/// (<https://webidl.spec.whatwg.org/#interface-object>); the wrapper shares the
/// native prototype so `Class::<JsEventTarget>` conversions keep working.
/// Deflated by `build.rs`, inflated once per process.
pub(crate) const INSTALL_EVENT_TARGET_CTOR_DEFLATE: &[u8] = include_bytes!(concat!(
    env!("OUT_DIR"),
    "/js_blobs/event_target_ctor.deflate"
));

/// The `EventTarget` constructor shim, inflated once per process.
pub(crate) fn install_event_target_ctor_js(ctx: &Ctx<'_>) -> Result<&'static str> {
    static CACHE: std::sync::OnceLock<Box<str>> = std::sync::OnceLock::new();
    super::blob::decompress(ctx, INSTALL_EVENT_TARGET_CTOR_DEFLATE, &CACHE)
}

/// `AbortController` and `AbortSignal`
/// (<https://dom.spec.whatwg.org/#interface-abortcontroller>,
/// <https://dom.spec.whatwg.org/#abortsignal>).
/// Deflated by `build.rs`, inflated once per process.
pub(crate) const INSTALL_ABORT_DEFLATE: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/js_blobs/abort.deflate"));

/// The abort shim, inflated once per process.
pub(crate) fn install_abort_js(ctx: &Ctx<'_>) -> Result<&'static str> {
    static CACHE: std::sync::OnceLock<Box<str>> = std::sync::OnceLock::new();
    super::blob::decompress(ctx, INSTALL_ABORT_DEFLATE, &CACHE)
}

/// Wraps the native `Event` constructor so a call without `new` throws
/// (<https://webidl.spec.whatwg.org/#interface-object>); the wrapper shares the
/// native prototype so `Class::<JsEvent>` conversions keep working.
/// Deflated by `build.rs`, inflated once per process.
pub(crate) const INSTALL_EVENT_CTOR_DEFLATE: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/js_blobs/event_ctor.deflate"));

/// The `Event` constructor shim, inflated once per process.
pub(crate) fn install_event_ctor_js(ctx: &Ctx<'_>) -> Result<&'static str> {
    static CACHE: std::sync::OnceLock<Box<str>> = std::sync::OnceLock::new();
    super::blob::decompress(ctx, INSTALL_EVENT_CTOR_DEFLATE, &CACHE)
}

/// Keeps a constructible target's object reachable from its world, so dispatch
/// can use it as `target`/`currentTarget`.
fn register_standalone<'js>(ctx: &Ctx<'js>, id: u64, target: &Object<'js>) -> Result<()> {
    bindings::world(ctx)?
        .borrow_mut()
        .intern_standalone_target(id, Persistent::save(ctx, target.clone()));
    Ok(())
}

/// An event whose prototype is this realm's, not the runtime's cached one.
fn realm_event<'js>(ctx: &Ctx<'js>, event: JsEvent) -> Result<Class<'js, JsEvent>> {
    bindings::host::instance(ctx, event)
}

/// `document.createEvent(interface)`
/// (<https://dom.spec.whatwg.org/#dom-document-createevent>).
///
/// The interface name matches ASCII case-insensitively. An interface that is
/// not in the table, or that is not exposed on this realm, throws
/// `NotSupportedError`. The event is created uninitialized; its prototype is
/// the exposed interface's, not the result of calling that constructor.
pub(crate) fn create_event<'js>(ctx: &Ctx<'js>, interface: &str) -> Result<Value<'js>> {
    let Some(interface_name) = legacy_event_interface(&interface.to_ascii_lowercase()) else {
        return Err(bindings::throw_dom(
            ctx,
            "NotSupportedError",
            "the requested event interface is not supported",
        ));
    };
    let class = realm_event(ctx, JsEvent::uninitialized())?;
    if interface_name != "Event" {
        set_exposed_event_prototype(ctx, &class, interface_name)?;
    }
    Ok(Class::into_value(class))
}

/// The `createEvent` interface table
/// (<https://dom.spec.whatwg.org/#dom-document-createevent>).
fn legacy_event_interface(interface: &str) -> Option<&'static str> {
    Some(match interface {
        "beforeunloadevent" => "BeforeUnloadEvent",
        "compositionevent" => "CompositionEvent",
        "customevent" => "CustomEvent",
        "devicemotionevent" => "DeviceMotionEvent",
        "deviceorientationevent" => "DeviceOrientationEvent",
        "dragevent" => "DragEvent",
        "event" | "events" | "htmlevents" | "svgevents" => "Event",
        "focusevent" => "FocusEvent",
        "hashchangeevent" => "HashChangeEvent",
        "keyboardevent" => "KeyboardEvent",
        "messageevent" => "MessageEvent",
        "mouseevent" | "mouseevents" => "MouseEvent",
        "storageevent" => "StorageEvent",
        "textevent" => "TextEvent",
        "touchevent" => "TouchEvent",
        "uievent" | "uievents" => "UIEvent",
        _ => return None,
    })
}

/// Sets the event's prototype to the realm's own interface prototype.
///
/// The pinned constructor captured at install decides: a page that replaced
/// `window[interface]` must not stamp its own prototype onto the event. A
/// missing pin (or a missing `prototype` on it) means the interface is not
/// exposed on the relevant global
/// (<https://dom.spec.whatwg.org/#dom-document-createevent>).
fn set_exposed_event_prototype<'js>(
    ctx: &Ctx<'js>,
    class: &Class<'js, JsEvent>,
    interface: &str,
) -> Result<()> {
    let pinned = bindings::world(ctx)?
        .borrow()
        .event_ctors
        .get(interface)
        .cloned();
    let Some(pinned) = pinned else {
        return Err(bindings::throw_dom(
            ctx,
            "NotSupportedError",
            "the requested event interface is not exposed",
        ));
    };
    let ctor = pinned.restore(ctx)?;
    let proto: Value = ctor.get("prototype")?;
    let Some(proto) = proto.as_object() else {
        return Err(bindings::throw_dom(
            ctx,
            "NotSupportedError",
            "the requested event interface is not exposed",
        ));
    };
    class.set_prototype(Some(proto))
}

fn set_custom_event_prototype<'js>(ctx: &Ctx<'js>, class: &Class<'js, JsEvent>) -> Result<()> {
    set_exposed_event_prototype(ctx, class, "CustomEvent")
}

/// The `new CustomEvent(type, eventInitDict)` constructor, called from the
/// JavaScript wrapper so instances can carry `CustomEvent.prototype`
/// (<https://dom.spec.whatwg.org/#dom-customevent-customevent>).
///
/// The wrapper owns arity; this native owns the Web IDL conversions.
pub(crate) fn construct_custom_event<'js>(
    ctx: Ctx<'js>,
    args: Rest<Value<'js>>,
) -> Result<Value<'js>> {
    let mut args = args.0.into_iter();
    let Some(typ) = args.next() else {
        return Err(Exception::throw_type(
            &ctx,
            "1 argument required, but only 0 present",
        ));
    };
    let typ = bindings::webidl_to_string(&ctx, typ)?;
    let init = args
        .next()
        .unwrap_or_else(|| Value::new_undefined(ctx.clone()));
    let init = event_generated::EventInit::from_object(&ctx, &init)?;
    let class = realm_event(&ctx, JsEvent::from_init(typ, &init))?;
    set_custom_event_prototype(&ctx, &class)?;
    Ok(Class::into_value(class))
}

/// The initialization steps of `initCustomEvent`, without `detail`
/// (<https://dom.spec.whatwg.org/#dom-customevent-initcustomevent>).
///
/// Returns false when the event is being dispatched, in which case the caller
/// must not set `detail` either.
pub(crate) fn init_custom_event<'js>(ctx: Ctx<'js>, args: Rest<Value<'js>>) -> Result<bool> {
    // Convert the arguments before the algorithm's dispatch-flag check.
    let mut args = args.0.into_iter();
    let Some(event) = args.next() else {
        return Err(Exception::throw_type(&ctx, "event is required"));
    };
    let Some(typ) = args.next() else {
        return Err(Exception::throw_type(&ctx, "type is required"));
    };
    let class = Class::<JsEvent>::from_js(&ctx, event)?;
    let typ = bindings::webidl_to_string(&ctx, typ)?;
    let bubbles = boolean_argument(&ctx, args.next())?;
    let cancelable = boolean_argument(&ctx, args.next())?;
    if class.borrow().state().dispatching {
        return Ok(false);
    }
    class.borrow().initialize(typ, bubbles, cancelable);
    Ok(true)
}

/// Registers the `CustomEvent` constructor and its prototype chain.
/// Deflated by `build.rs`, inflated once per process.
pub(crate) const INSTALL_CUSTOM_EVENT_DEFLATE: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/js_blobs/custom_event.deflate"));

/// The `CustomEvent` shim, inflated once per process.
pub(crate) fn install_custom_event_js(ctx: &Ctx<'_>) -> Result<&'static str> {
    static CACHE: std::sync::OnceLock<Box<str>> = std::sync::OnceLock::new();
    super::blob::decompress(ctx, INSTALL_CUSTOM_EVENT_DEFLATE, &CACHE)
}

pub(crate) const INSTALL_SECURITY_POLICY_VIOLATION_EVENT_DEFLATE: &[u8] = include_bytes!(concat!(
    env!("OUT_DIR"),
    "/js_blobs/security_policy_violation_event.deflate"
));

/// The `SecurityPolicyViolationEvent` shim, inflated once per process.
pub(crate) fn install_security_policy_violation_event_js(ctx: &Ctx<'_>) -> Result<&'static str> {
    static CACHE: std::sync::OnceLock<Box<str>> = std::sync::OnceLock::new();
    super::blob::decompress(ctx, INSTALL_SECURITY_POLICY_VIOLATION_EVENT_DEFLATE, &CACHE)
}

/// The `new SecurityPolicyViolationEvent(type, eventInitDict)` constructor,
/// called from the JavaScript wrapper so instances can carry
/// `SecurityPolicyViolationEvent.prototype`
/// (<https://w3c.github.io/webappsec-csp/#securitypolicyviolationevent>).
///
/// The wrapper owns the readonly attributes and arity; this native owns the
/// `EventInit` conversions.
pub(crate) fn construct_security_policy_violation_event<'js>(
    ctx: Ctx<'js>,
    args: Rest<Value<'js>>,
) -> Result<Value<'js>> {
    let mut args = args.0.into_iter();
    let Some(typ) = args.next() else {
        return Err(Exception::throw_type(
            &ctx,
            "1 argument required, but only 0 present",
        ));
    };
    let typ = bindings::webidl_to_string(&ctx, typ)?;
    let init = args
        .next()
        .unwrap_or_else(|| Value::new_undefined(ctx.clone()));
    let init = event_generated::EventInit::from_object(&ctx, &init)?;
    let class = realm_event(&ctx, JsEvent::from_init(typ, &init))?;
    if bindings::world(&ctx)?
        .borrow()
        .event_ctors
        .contains_key("SecurityPolicyViolationEvent")
    {
        set_exposed_event_prototype(&ctx, &class, "SecurityPolicyViolationEvent")?;
    }
    Ok(Class::into_value(class))
}

/// Fires one trusted `securitypolicyviolation` event at `target` with the
/// readonly attributes set as own properties, so both constructor-created
/// (slot-backed getters) and user-agent events read the same values
/// (<https://w3c.github.io/webappsec-csp/#report-violation>).
pub(crate) fn fire_security_policy_violation(
    ctx: &Ctx<'_>,
    target: EventTargetKey,
    violation: &crate::csp::Violation,
) -> Result<()> {
    let event = realm_event(
        ctx,
        JsEvent::trusted("securitypolicyviolation", true, false),
    )?;
    if bindings::world(ctx)?
        .borrow()
        .event_ctors
        .contains_key("SecurityPolicyViolationEvent")
    {
        set_exposed_event_prototype(ctx, &event, "SecurityPolicyViolationEvent")?;
    }
    // Own data properties, because the shim's prototype accessors are
    // readonly and an assigning set would throw
    // (<https://webidl.spec.whatwg.org/#idl-read-only-attributes>).
    // `documentURI` is the stripped document URL from the violation, not
    // the live document URL: fragment and credentials never report
    // (<https://w3c.github.io/webappsec-csp/#strip-url-for-use-in-reports>).
    event.prop("documentURI", violation.document_uri.clone())?;
    event.prop("referrer", String::new())?;
    event.prop("blockedURI", violation.blocked_uri.clone())?;
    event.prop("violatedDirective", violation.effective_directive.clone())?;
    event.prop("effectiveDirective", violation.effective_directive.clone())?;
    event.prop("originalPolicy", violation.original_policy.clone())?;
    // `sourceFile` strips like the reports: a `data:` or `blob:` trigger
    // reports just its scheme. Line and column are zero when unknown.
    event.prop(
        "sourceFile",
        violation
            .source_file
            .as_deref()
            .map_or_else(String::new, crate::csp::strip_url_for_reports),
    )?;
    event.prop("sample", violation.sample.clone().unwrap_or_default())?;
    event.prop(
        "disposition",
        if violation.blocks() {
            "enforce"
        } else {
            "report"
        },
    )?;
    event.prop("statusCode", violation.status_code)?;
    event.prop("lineNumber", violation.line_number)?;
    event.prop("columnNumber", violation.column_number)?;
    dispatch(ctx, target, &event, None)?;
    Ok(())
}

/// Appends one listener to a target's list in a chosen realm, so
/// `contentWindow.addEventListener` runs in the caller's realm and still
/// registers on the frame's window
/// (<https://dom.spec.whatwg.org/#dom-eventtarget-addeventlistener>,
/// <https://html.spec.whatwg.org/multipage/window-object.html#windowproxy-get>).
pub(crate) fn add_listener_in<'js>(
    ctx: &Ctx<'js>,
    world: &Rc<RefCell<World>>,
    target: EventTargetKey,
    typ: Value<'js>,
    callback: Value<'js>,
    options: Option<Value<'js>>,
) -> Result<()> {
    let typ = bindings::webidl_to_string(ctx, typ)?;
    let callback = listener_callback(ctx, callback)?;
    let options = ListenerOptions::read(ctx, options)?;
    add_listener_parsed_in(ctx, world, target, typ, callback, options)
}

/// [`add_listener_in`] from an already-converted `EventListener?` and
/// options, used by the generated `EventTarget` contract.
pub(crate) fn add_listener_parsed(
    ctx: &Ctx<'_>,
    target: EventTargetKey,
    typ: String,
    callback: Option<Persistent<Value<'static>>>,
    options: ListenerOptions,
) -> Result<()> {
    add_listener_parsed_in(
        ctx,
        &target_world(ctx, target)?,
        target,
        typ,
        callback,
        options,
    )
}

fn add_listener_parsed_in(
    ctx: &Ctx<'_>,
    world: &Rc<RefCell<World>>,
    target: EventTargetKey,
    typ: String,
    callback: Option<Persistent<Value<'static>>>,
    options: ListenerOptions,
) -> Result<()> {
    if options
        .signal
        .as_ref()
        .is_some_and(|signal| signal_aborted(ctx, signal))
    {
        return Ok(());
    }
    // A null callback still flattens the options but is never added
    // (<https://dom.spec.whatwg.org/#add-an-event-listener> step 3).
    let Some(callback) = callback else {
        return Ok(());
    };
    let passive = match options.passive {
        Some(passive) => passive,
        None => default_passive(ctx, &typ, target)?,
    };
    // Abort steps run when the list is touched or a listener is about to be
    // invoked (<https://dom.spec.whatwg.org/#add-an-event-listener>).
    let existing_listeners = world.borrow().listener_snapshot(target);
    for existing in &existing_listeners {
        if let Some(signal) = &existing.signal
            && signal_aborted(ctx, signal)
        {
            existing.removed.set(true);
            world.borrow_mut().remove_listener(target, existing);
        }
    }
    for existing in &existing_listeners {
        if existing.typ == typ
            && existing.capture == options.capture
            && same_callback(ctx, existing.callback.as_ref(), Some(&callback))?
        {
            return Ok(());
        }
    }
    world.borrow_mut().add_listener(
        target,
        Rc::new(Listener {
            typ,
            callback: Some(callback),
            capture: options.capture,
            once: options.once,
            passive,
            signal: options.signal,
            removed: Cell::new(false),
        }),
    );
    Ok(())
}

/// Removes one listener from a target's list
/// (<https://dom.spec.whatwg.org/#dom-eventtarget-removeeventlistener>).
pub(crate) fn remove_listener_in<'js>(
    ctx: &Ctx<'js>,
    world: &Rc<RefCell<World>>,
    target: EventTargetKey,
    typ: Value<'js>,
    callback: Value<'js>,
    options: Option<Value<'js>>,
) -> Result<()> {
    let typ = bindings::webidl_to_string(ctx, typ)?;
    let callback = listener_callback(ctx, callback)?;
    let capture = ListenerOptions::read_capture(ctx, options)?;
    remove_listener_parsed_in(ctx, world, target, &typ, callback.as_ref(), capture)
}

/// [`remove_listener_in`] from an already-converted `EventListener?` and
/// capture, used by the generated `EventTarget` contract.
pub(crate) fn remove_listener_parsed(
    ctx: &Ctx<'_>,
    target: EventTargetKey,
    typ: &str,
    callback: Option<&Persistent<Value<'static>>>,
    capture: bool,
) -> Result<()> {
    remove_listener_parsed_in(
        ctx,
        &target_world(ctx, target)?,
        target,
        typ,
        callback,
        capture,
    )
}

fn remove_listener_parsed_in(
    ctx: &Ctx<'_>,
    world: &Rc<RefCell<World>>,
    target: EventTargetKey,
    typ: &str,
    callback: Option<&Persistent<Value<'static>>>,
    capture: bool,
) -> Result<()> {
    let mut world = world.borrow_mut();
    let mut removed = Vec::new();
    for existing in world.listener_snapshot(target) {
        if existing.typ == typ
            && existing.capture == capture
            && same_callback(ctx, existing.callback.as_ref(), callback)?
        {
            existing.removed.set(true);
            removed.push(existing);
        }
    }
    for listener in removed {
        world.remove_listener(target, &listener);
    }
    Ok(())
}

/// `dispatchEvent()`
/// (<https://dom.spec.whatwg.org/#dom-eventtarget-dispatchevent>).
pub(crate) fn dispatch_event<'js>(
    ctx: &Ctx<'js>,
    target: EventTargetKey,
    event: &Class<'js, JsEvent>,
) -> Result<bool> {
    dispatch_checked(ctx, target, event, false, None)
}

/// `dispatchEvent()` on a chosen realm's window, so a call through another
/// realm's `WindowProxy` dispatches on the frame's window
/// (<https://html.spec.whatwg.org/multipage/window-object.html#windowproxy-get>).
pub(crate) fn dispatch_event_for_window<'js>(
    ctx: &Ctx<'js>,
    world: &Rc<RefCell<World>>,
    event: &Class<'js, JsEvent>,
) -> Result<bool> {
    dispatch_checked(ctx, EventTargetKey::Window, event, false, Some(world))
}

/// Dispatches a user-agent event with the trust bit set, without the
/// `dispatchEvent()` step that clears `isTrusted`
/// (<https://dom.spec.whatwg.org/#concept-event-dispatch>).
pub(crate) fn dispatch_trusted_event<'js>(
    ctx: &Ctx<'js>,
    target: EventTargetKey,
    event: &Class<'js, JsEvent>,
) -> Result<bool> {
    dispatch_checked(ctx, target, event, true, None)
}

/// Checks the event's dispatch flags and sets `isTrusted` before dispatch:
/// script dispatch clears the trust bit, user-agent delivery keeps it.
fn dispatch_checked<'js>(
    ctx: &Ctx<'js>,
    target: EventTargetKey,
    event: &Class<'js, JsEvent>,
    trusted: bool,
    window: Option<&Rc<RefCell<World>>>,
) -> Result<bool> {
    {
        let class = event.borrow();
        let state = class.state();
        if state.dispatching || !state.initialized {
            return Err(bindings::throw_dom(
                ctx,
                "InvalidStateError",
                "the event is already being dispatched or was never initialized",
            ));
        }
    }
    event.borrow().state_mut().is_trusted = trusted;
    dispatch(ctx, target, event, window)
}

/// Creates and dispatches a user-agent event.
pub(crate) fn fire_trusted(
    ctx: &Ctx<'_>,
    target: EventTargetKey,
    typ: &str,
    bubbles: bool,
    cancelable: bool,
) -> Result<()> {
    let event = realm_event(ctx, JsEvent::trusted(typ, bubbles, cancelable))?;
    dispatch(ctx, target, &event, None)?;
    Ok(())
}

/// Fires a trusted event that crosses shadow boundaries. `input` and
/// `beforeinput` are composed, while `change` is not
/// (<https://w3c.github.io/uievents/#events-inputevents>).
pub(crate) fn fire_trusted_composed(
    ctx: &Ctx<'_>,
    target: EventTargetKey,
    typ: &str,
    bubbles: bool,
    cancelable: bool,
) -> Result<()> {
    let event = realm_event(ctx, JsEvent::trusted_composed(typ, bubbles, cancelable))?;
    dispatch(ctx, target, &event, None)?;
    Ok(())
}

/// Fires `popstate` with the deserialized state for the activated entry.
/// <https://html.spec.whatwg.org/multipage/nav-history-apis.html#the-popstateevent-interface>
pub(crate) fn fire_trusted_popstate<'js>(ctx: &Ctx<'js>, state: Value<'js>) -> Result<()> {
    let event = realm_event(ctx, JsEvent::trusted("popstate", false, false))?;
    if let Some(prototype) = bindings::world(ctx)?.borrow().brand("PopStateEvent") {
        event.set_prototype(Some(&prototype.restore(ctx)?))?;
    }
    event.set("state", state)?;
    dispatch(ctx, EventTargetKey::Window, &event, None)?;
    Ok(())
}

/// Fires a trusted event and reports whether it was not canceled, for
/// cancelable events that gate a following action such as `reset`.
pub(crate) fn fire_trusted_cancelable(
    ctx: &Ctx<'_>,
    target: EventTargetKey,
    typ: &str,
    bubbles: bool,
    cancelable: bool,
) -> Result<bool> {
    let event = realm_event(ctx, JsEvent::trusted(typ, bubbles, cancelable))?;
    dispatch(ctx, target, &event, None)
}

/// Fires a trusted `click` and reports whether it was not canceled, so the
/// caller can run the activation behavior
/// (<https://dom.spec.whatwg.org/#concept-event-dispatch>).
pub(crate) fn fire_trusted_click(ctx: &Ctx<'_>, target: EventTargetKey) -> Result<bool> {
    let event = realm_event(ctx, JsEvent::trusted("click", true, true))?;
    dispatch(ctx, target, &event, None)
}

/// [Dispatch](https://dom.spec.whatwg.org/#concept-event-dispatch) an event.
fn dispatch<'js>(
    ctx: &Ctx<'js>,
    target: EventTargetKey,
    event: &Class<'js, JsEvent>,
    window: Option<&Rc<RefCell<World>>>,
) -> Result<bool> {
    let path = build_path(ctx, target, window)?;
    {
        let class = event.borrow();
        let mut state = class.state_mut();
        state.dispatching = true;
        state.target = Some(EventTargetRef {
            key: target,
            world: Rc::clone(&path[0].reference.world),
        });
        state.current_target = None;
        state.phase = NONE;
        // The stop flags are intentionally not cleared here: an event that was
        // stopped before dispatch must not reach any listener, and the end of
        // the algorithm unsets the flags
        // (<https://dom.spec.whatwg.org/#concept-event-dispatch>).
        state.path = path.iter().map(|item| item.reference.clone()).collect();
    }
    let bubbles = event.borrow().state().bubbles;
    let typ = event.borrow().state().typ.clone();
    let window = current_event_window(ctx, &path)?;
    // https://html.spec.whatwg.org/multipage/webappapis.html#set-the-current-event
    let previous: Value<'js> = window.get("event")?;
    window.set("event", Class::into_value(event.clone()))?;
    // A click event runs the element's legacy-pre-activation behavior before
    // its listeners and completes or rolls it back after dispatch. `click()`
    // owns that sequence itself, so it marks the dispatch to skip this path
    // (<https://html.spec.whatwg.org/multipage/input.html#the-input-element:legacy-pre-activation-behavior>).
    let click_activation = if typ == "click"
        && let EventTargetKey::Node(node) = target
    {
        let activation = super::bindings::focus::activation_target(ctx, node)?;
        match activation {
            Some(activation)
                if !super::bindings::world_for_node(ctx, activation)?
                    .borrow()
                    .click_in_progress(activation) =>
            {
                Some((
                    activation,
                    super::bindings::focus::legacy_pre_activation(ctx, activation)?,
                ))
            }
            _ => None,
        }
    } else {
        None
    };
    let result = run_invocations(ctx, event, &path, target, bubbles);
    // Handler attributes (`onreadystatechange`, `onload`, …) act as listeners;
    // the engine runs them after the listener list until it models the
    // handler registration slot. A stopped event never reaches them.
    let stopped = {
        let class = event.borrow();
        let state = class.state();
        state.stop_propagation || state.stop_immediate
    };
    let handler = if result.is_ok() && !stopped {
        let event_value = Class::into_value(event.clone());
        // Event handler content attributes are bubble-phase listeners on
        // their element. The engine does not interleave them with the listener
        // list yet, but they still run at every node the event reaches, so an
        // ancestor's `onclick` can cancel the event before activation
        // (<https://html.spec.whatwg.org/multipage/webappapis.html#event-handler-content-attributes>).
        let mut outcome = Ok(());
        for (index, item) in path.iter().enumerate() {
            if index > 0 && !bubbles {
                break;
            }
            if index > 0 {
                let state = event.borrow();
                let state = state.state();
                if state.stop_propagation || state.stop_immediate {
                    break;
                }
            }
            outcome = call_handler_attribute(ctx, Some(&item.target), &typ, &event_value);
            if outcome.is_err() {
                break;
            }
        }
        outcome
    } else {
        Ok(())
    };
    // Clear the dispatch state on both paths, so a failed invocation cannot
    // leave the event permanently undispatchable.
    let canceled = {
        let class = event.borrow();
        let mut state = class.state_mut();
        state.phase = NONE;
        state.current_target = None;
        state.path.clear();
        state.dispatching = false;
        state.stop_propagation = false;
        state.stop_immediate = false;
        state.canceled
    };
    let restored = window.set("event", previous);
    let outcome = result.and(handler);
    // A click event runs the activation behavior of a label ancestor after
    // dispatch, including for events the page dispatched itself
    // (<https://html.spec.whatwg.org/multipage/forms.html#the-label-element:activation-behavior>).
    if let Some((node, previous)) = click_activation {
        if canceled {
            super::bindings::focus::legacy_canceled_activation(ctx, node, &previous)?;
        } else {
            super::bindings::focus::complete_activation(ctx, node, &previous)?;
        }
    }
    if typ == "click"
        && !canceled
        && let EventTargetKey::Node(node) = target
    {
        super::bindings::focus::run_label_activation(ctx, node)?;
    }
    // The current event restores even when dispatch threw; the dispatch
    // error wins over a restore error, so a failed invocation cannot leak a
    // stale current event into the next dispatch.
    match (outcome, restored) {
        (Ok(()), Ok(())) => Ok(!canceled),
        (Err(error), _) | (Ok(()), Err(error)) => Err(error),
    }
}

/// The `Window` whose [current event](https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-window-event)
/// this dispatch updates: the target's realm window, else the caller's.
fn current_event_window<'js>(ctx: &Ctx<'js>, path: &[PathItem<'js>]) -> Result<Object<'js>> {
    if let Some(item) = path.first()
        && let Some(window) = item.reference.world.borrow().window_object()
    {
        return window.restore(ctx);
    }
    Ok(ctx.globals())
}

fn run_invocations<'js>(
    ctx: &Ctx<'js>,
    event: &Class<'js, JsEvent>,
    path: &[PathItem<'js>],
    target: EventTargetKey,
    bubbles: bool,
) -> Result<()> {
    // Capture phase: root to target.
    for item in path.iter().rev() {
        let phase = if item.reference.key == target {
            AT_TARGET
        } else {
            CAPTURING_PHASE
        };
        invoke(ctx, event, item, phase, true)?;
    }
    // Bubble phase: target to root.
    for item in path {
        let phase = if item.reference.key == target {
            AT_TARGET
        } else if bubbles {
            BUBBLING_PHASE
        } else {
            continue;
        };
        invoke(ctx, event, item, phase, false)?;
    }
    Ok(())
}

/// One event path item (<https://dom.spec.whatwg.org/#event-path-item>).
struct PathItem<'js> {
    reference: EventTargetRef,
    target: Value<'js>,
}

/// Builds the event path: the target, its ancestors, and the window
/// (<https://dom.spec.whatwg.org/#concept-event-path-append>).
fn build_path<'js>(
    ctx: &Ctx<'js>,
    target: EventTargetKey,
    window: Option<&Rc<RefCell<World>>>,
) -> Result<Vec<PathItem<'js>>> {
    match target {
        EventTargetKey::Window => {
            let world = match window {
                Some(world) => Rc::clone(world),
                None => bindings::world(ctx)?,
            };
            let reference = EventTargetRef { key: target, world };
            let value = resolve_target(ctx, &reference)?;
            Ok(vec![PathItem {
                reference,
                target: value,
            }])
        }
        EventTargetKey::Attribute { .. } | EventTargetKey::Standalone(_) => {
            // https://dom.spec.whatwg.org/#get-the-parent
            // https://dom.spec.whatwg.org/#interface-attr
            // Attr's owner element is not its parent; Chromium's
            // EventPath::CalculatePath likewise follows parentNode().
            let reference = EventTargetRef {
                key: target,
                world: target_world(ctx, target)?,
            };
            if let EventTargetKey::Standalone(id) = target
                && reference.world.borrow().standalone_target(id).is_none()
            {
                return Err(Exception::throw_internal(ctx, "missing EventTarget"));
            }
            let value = resolve_target(ctx, &reference)?;
            Ok(vec![PathItem {
                reference,
                target: value,
            }])
        }
        EventTargetKey::Node(id) => {
            let world = bindings::world_for_node(ctx, id)?;
            let mut path = Vec::new();
            let mut cursor = Some(id);
            let mut reached_document = false;
            while let Some(node) = cursor {
                let reference = EventTargetRef {
                    key: EventTargetKey::Node(node),
                    world: Rc::clone(&world),
                };
                let value = resolve_target(ctx, &reference)?;
                path.push(PathItem {
                    reference,
                    target: value,
                });
                let parent = world.borrow().node_parent(node);
                if let Some(parent) = parent {
                    cursor = Some(parent);
                } else {
                    reached_document = world.borrow().node_is_document(node);
                    cursor = None;
                }
            }
            if reached_document {
                // A document's parent is its own window, not the dispatching
                // realm's (<https://dom.spec.whatwg.org/#get-the-parent>).
                let reference = EventTargetRef {
                    key: EventTargetKey::Window,
                    world,
                };
                let value = resolve_target(ctx, &reference)?;
                path.push(PathItem {
                    reference,
                    target: value,
                });
            }
            Ok(path)
        }
    }
}

/// [Invoke](https://dom.spec.whatwg.org/#concept-event-listener-invoke) the
/// listeners of one path item.
fn invoke<'js>(
    ctx: &Ctx<'js>,
    event: &Class<'js, JsEvent>,
    item: &PathItem<'js>,
    phase: u16,
    capturing: bool,
) -> Result<()> {
    {
        let class = event.borrow();
        if class.state().stop_propagation {
            return Ok(());
        }
        let mut state = class.state_mut();
        state.phase = phase;
        state.current_target = Some(item.reference.clone());
    }
    let listeners = item
        .reference
        .world
        .borrow()
        .listener_snapshot(item.reference.key);
    let event_value = Class::into_value(event.clone());
    for listener in listeners {
        if listener.removed.get() {
            continue;
        }
        let typ = event.borrow().state().typ.clone();
        if typ != listener.typ {
            continue;
        }
        if capturing != listener.capture {
            continue;
        }
        if let Some(signal) = &listener.signal
            && signal_aborted(ctx, signal)
        {
            listener.removed.set(true);
            item.reference
                .world
                .borrow_mut()
                .remove_listener(item.reference.key, &listener);
            continue;
        }
        if listener.once {
            listener.removed.set(true);
            item.reference
                .world
                .borrow_mut()
                .remove_listener(item.reference.key, &listener);
        }
        if listener.passive {
            event.borrow().state_mut().in_passive = true;
        }
        if let Some(callback) = &listener.callback {
            let value: Value<'js> = callback.clone().restore(ctx)?;
            if let Err(error) = call_listener(ctx, &value, &item.target, &event_value) {
                report_exception(ctx, &error);
            }
        }
        event.borrow().state_mut().in_passive = false;
        if event.borrow().state().stop_immediate {
            break;
        }
    }
    Ok(())
}

/// The Web IDL "call a user object's operation" step for an event listener:
/// a callable callback is called directly, any other object through its
/// `handleEvent` operation (<https://webidl.spec.whatwg.org/#call-a-user-objects-operation>).
fn call_listener<'js>(
    ctx: &Ctx<'js>,
    callback: &Value<'js>,
    this: &Value<'js>,
    event: &Value<'js>,
) -> Result<()> {
    if let Some(function) = callback.as_function() {
        function.call::<_, ()>((This(this.clone()), event.clone()))?;
        return Ok(());
    }
    if let Some(object) = callback.as_object() {
        let handler: Value = object.get("handleEvent")?;
        if let Some(function) = handler.as_function() {
            function.call::<_, ()>((This(object.clone()), event.clone()))?;
            return Ok(());
        }
    }
    Err(Exception::throw_type(ctx, "callback is not callable"))
}

/// [Report the exception](https://html.spec.whatwg.org/multipage/webappapis.html#report-an-exception)
/// step of the dispatch algorithm.
///
/// Fires the window's `error` event (`window.onerror`) for a throwing listener
/// or handler, then drops the exception so it cannot abort the rest of the
/// dispatch or leak into a later JavaScript operation. The location is taken
/// from the exception's own stack; no document line offset is applied.
pub(super) fn report_exception(ctx: &Ctx<'_>, error: &rquickjs::Error) {
    if error.is_exception() {
        let caught = ctx.catch();
        super::bindings::report_exception_value(ctx, caught, 0, "");
    }
}

/// `EventListener?` conversion: null and undefined become a null callback and
/// any object is a callback interface value, whose `handleEvent` is looked up
/// when invoked (<https://webidl.spec.whatwg.org/#es-callback-interface>).
pub(crate) fn listener_callback<'js>(
    ctx: &Ctx<'js>,
    callback: Value<'js>,
) -> Result<Option<Persistent<Value<'static>>>> {
    if callback.is_null() || callback.is_undefined() {
        return Ok(None);
    }
    if !callback.is_object() {
        return Err(Exception::throw_type(
            ctx,
            "callback must be an object or null",
        ));
    }
    Ok(Some(Persistent::save(ctx, callback)))
}

/// Parsed `AddEventListenerOptions` / `EventListenerOptions`
/// (<https://dom.spec.whatwg.org/#dictdef-addeventlisteneroptions>).
pub(crate) struct ListenerOptions {
    capture: bool,
    once: bool,
    /// `None` means the member was omitted; the default passive value is
    /// resolved when the listener is added
    /// (<https://dom.spec.whatwg.org/#default-passive-value>).
    passive: Option<bool>,
    signal: Option<Persistent<Object<'static>>>,
}

impl ListenerOptions {
    fn read<'js>(ctx: &Ctx<'js>, options: Option<Value<'js>>) -> Result<Self> {
        let mut parsed = Self {
            capture: false,
            once: false,
            passive: None,
            signal: None,
        };
        let object = match listener_options_argument(ctx, options)? {
            ListenerOptionsArgument::Absent => return Ok(parsed),
            ListenerOptionsArgument::Boolean(capture) => {
                // A non-object is the boolean form.
                parsed.capture = capture;
                return Ok(parsed);
            }
            ListenerOptionsArgument::Object(object) => object,
        };
        parsed.capture = bindings::option_truthy(ctx, &object, "capture")?;
        parsed.once = bindings::option_truthy(ctx, &object, "once")?;
        let passive: Value = object.get("passive")?;
        if !passive.is_undefined() {
            parsed.passive = Some(bindings::to_boolean(ctx, &passive)?);
        }
        let signal: Value = object.get("signal")?;
        if !signal.is_undefined() {
            // `AbortSignal signal` is not nullable
            // (<https://dom.spec.whatwg.org/#dictdef-addeventlisteneroptions>,
            // <https://webidl.spec.whatwg.org/#es-interface>).
            let Some(signal) = signal.as_object() else {
                return Err(Exception::throw_type(
                    ctx,
                    "Failed to convert 'signal' to AbortSignal",
                ));
            };
            parsed.signal = Some(Persistent::save(ctx, signal.clone()));
        }
        Ok(parsed)
    }

    fn read_capture<'js>(ctx: &Ctx<'js>, options: Option<Value<'js>>) -> Result<bool> {
        // `removeEventListener` reads only `capture`; reading the other
        // members would run their getters
        // (<https://dom.spec.whatwg.org/#dom-eventtarget-removeeventlistener>).
        match listener_options_argument(ctx, options)? {
            ListenerOptionsArgument::Absent => Ok(false),
            ListenerOptionsArgument::Boolean(capture) => Ok(capture),
            ListenerOptionsArgument::Object(object) => {
                bindings::option_truthy(ctx, &object, "capture")
            }
        }
    }
}

/// The options argument of `addEventListener` / `removeEventListener`: a
/// dictionary, the boolean shorthand, or nothing
/// (<https://dom.spec.whatwg.org/#dictdef-eventlisteneroptions>).
enum ListenerOptionsArgument<'js> {
    /// The argument was absent, `undefined`, or `null`.
    Absent,
    /// The boolean form, with its converted value.
    Boolean(bool),
    Object(Object<'js>),
}

fn listener_options_argument<'js>(
    ctx: &Ctx<'js>,
    options: Option<Value<'js>>,
) -> Result<ListenerOptionsArgument<'js>> {
    let Some(value) = options else {
        return Ok(ListenerOptionsArgument::Absent);
    };
    if value.is_undefined() || value.is_null() {
        return Ok(ListenerOptionsArgument::Absent);
    }
    match value.as_object() {
        Some(object) => Ok(ListenerOptionsArgument::Object(object.clone())),
        None => Ok(ListenerOptionsArgument::Boolean(bindings::to_boolean(
            ctx, &value,
        )?)),
    }
}

/// The default passive value for an event type on a target
/// (<https://dom.spec.whatwg.org/#default-passive-value>): wheel and
/// touch-start listeners are passive on the window, the document, its
/// document element, and its body element.
fn default_passive(ctx: &Ctx<'_>, typ: &str, target: EventTargetKey) -> Result<bool> {
    if !matches!(typ, "touchstart" | "touchmove" | "wheel" | "mousewheel") {
        return Ok(false);
    }
    match target {
        EventTargetKey::Window => Ok(true),
        EventTargetKey::Attribute { .. } | EventTargetKey::Standalone(_) => Ok(false),
        EventTargetKey::Node(id) => {
            let world = bindings::world_for_node(ctx, id)?;
            let world = world.borrow();
            let Some(parsed) = world.document(id) else {
                return Ok(false);
            };
            let base = &parsed.document.base;
            if base.root_node().id == id.node {
                return Ok(true);
            }
            let html = base
                .query_selector_in(base.root_node().id, "html")
                .ok()
                .flatten()
                .map(|node| crate::dom::tree::NodeId {
                    document: id.document,
                    node,
                });
            if html == Some(id) {
                return Ok(true);
            }
            let body = base
                .query_selector_in(base.root_node().id, "body")
                .ok()
                .flatten()
                .map(|node| crate::dom::tree::NodeId {
                    document: id.document,
                    node,
                });
            Ok(body == Some(id))
        }
    }
}

/// Whether an `AbortSignal` has its aborted flag set
/// (<https://dom.spec.whatwg.org/#abortsignal-aborted>).
fn signal_aborted(ctx: &Ctx<'_>, signal: &Persistent<Object<'static>>) -> bool {
    signal
        .clone()
        .restore(ctx)
        .ok()
        .and_then(|object| object.get::<_, Value>("aborted").ok())
        // The pristine conversion cannot throw; a broken realm reads as
        // not-aborted rather than failing the dispatch.
        .is_some_and(|value| bindings::to_boolean(ctx, &value).unwrap_or(false))
}

fn target_world(ctx: &Ctx<'_>, target: EventTargetKey) -> Result<Rc<RefCell<World>>> {
    match target {
        EventTargetKey::Node(id) => bindings::world_for_node(ctx, id),
        EventTargetKey::Attribute { scope, id } => {
            let state = bindings::attr_state(ctx, scope, id)?;
            bindings::world_for_node(ctx, state.document)
        }
        EventTargetKey::Window | EventTargetKey::Standalone(_) => bindings::world(ctx),
    }
}

fn stored_target<'js>(ctx: &Ctx<'js>, reference: Option<&EventTargetRef>) -> Result<Value<'js>> {
    match reference {
        Some(reference) => resolve_target(ctx, reference),
        None => Ok(Value::new_null(ctx.clone())),
    }
}

/// Resolves an event target back to its platform object, using the world that
/// owns it rather than the caller's
/// (<https://dom.spec.whatwg.org/#concept-event-target>).
fn resolve_target<'js>(ctx: &Ctx<'js>, reference: &EventTargetRef) -> Result<Value<'js>> {
    match reference.key {
        EventTargetKey::Window => match reference.world.borrow().window_object() {
            Some(window) => Ok(window.restore(ctx)?.into_value()),
            None => Ok(ctx.globals().into_value()),
        },
        EventTargetKey::Node(id) => bindings::wrap_node(ctx, id),
        EventTargetKey::Attribute { id, .. } => bindings::attr_wrapper(ctx, id),
        EventTargetKey::Standalone(id) => match reference.world.borrow().standalone_target(id) {
            Some(saved) => Ok(saved.restore(ctx)?.into_value()),
            None => Ok(Value::new_null(ctx.clone())),
        },
    }
}

/// Compiles one event handler content attribute as a sloppy-mode function
/// in the window's global script environment, matching classic scripts
/// (<https://html.spec.whatwg.org/multipage/webappapis.html#getting-the-current-value-of-the-event-handler>).
///
/// Compiled through the pinned `Function` constructor, the way `new
/// Function(params, body)` does: params and body stay separate inputs, so a
/// `}` in the attribute value is a syntax error (null handler), not an
/// escape from a string-interpolated wrapper. The constructor is captured
/// at realm init because handlers compile on demand, after page code may
/// have replaced the global. Like classic scripts in this engine (which
/// enforces no CSP yet -- tracked gap, not an intentional bypass),
/// compilation is unconditional.
pub(crate) fn compile_handler_function<'js>(
    ctx: &Ctx<'js>,
    params: &[&str],
    body: &str,
) -> Result<Function<'js>> {
    let bridge = super::bridge::object(ctx)?;
    let constructor: Object = bridge.get("__tb_function")?;
    let Some(constructor) = constructor.as_constructor() else {
        return Err(Exception::throw_internal(
            ctx,
            "Function constructor is not constructible",
        ));
    };
    // One params argument plus the body: `new Function("event", body)`.
    // The constructor parses the params list itself, so no identifier
    // validation is needed here (every caller passes literals regardless).
    constructor.construct((params.join(","), body.to_owned()))
}

/// Whether an event handler content attribute may run
/// (<https://w3c.github.io/webappsec-csp/#should-block-inline>). The event
/// targets the element that carries the attribute.
fn handler_attribute_allowed<'js>(
    ctx: &Ctx<'js>,
    target: &Value<'js>,
    source: &str,
) -> Result<bool> {
    let Some(id) = bindings::host_node_id(ctx, target) else {
        return Ok(true);
    };
    let violations = {
        let world = bindings::world(ctx)?;
        let world = world.borrow();
        world.policy.csp.check_inline(
            &world.document_url,
            &crate::csp::InlineScript {
                source,
                nonce: "",
                nonceable: false,
                parser_inserted: false,
                kind: crate::csp::InlineKind::ScriptAttribute,
            },
        )
    };
    if !violations.is_empty() {
        bindings::fire_csp_violations(ctx, id, &violations)?;
    }
    Ok(!crate::csp::blocks(&violations))
}

/// Calls a target's `on<type>` handler attribute, if one is assigned
/// (<https://html.spec.whatwg.org/multipage/webappapis.html#event-handlers>).
///
/// The engine does not model the handler registration slot yet, so this runs
/// after the listener list instead of at the handler's registration position.
fn call_handler_attribute<'js>(
    ctx: &Ctx<'js>,
    target: Option<&Value<'js>>,
    typ: &str,
    event: &Value<'js>,
) -> Result<()> {
    let Some(object) = target.and_then(|target| target.as_object()) else {
        return Ok(());
    };
    // Scripting is disabled in a sandboxed frame without `allow-scripts`, and
    // an event handler content attribute is script
    // (<https://html.spec.whatwg.org/multipage/browsers.html#sandboxed-scripts-browsing-context-flag>).
    if bindings::world(ctx)?.borrow().sandbox.scripts_disabled() {
        return Ok(());
    }
    let name = format!("on{typ}");
    let mut handler: Value = object.get(name.as_str())?;
    if handler.as_function().is_none()
        && let Some(source) = handler_attribute_source(ctx, object, &name)?
    {
        if !handler_attribute_allowed(ctx, object.as_value(), &source)? {
            object.set(name.as_str(), Value::new_null(ctx.clone()))?;
            return Ok(());
        }
        // A handler content attribute compiles to a function whose body is
        // the attribute value and whose `this` is the object. `onerror` takes
        // the spec's five arguments, not the usual single event argument
        // (<https://html.spec.whatwg.org/multipage/webappapis.html#event-handler-content-attributes>).
        let params = if typ == "error" {
            &["event", "source", "lineno", "colno", "error"] as &[&str]
        } else {
            &["event"] as &[&str]
        };
        match compile_handler_function(ctx, params, &source) {
            Ok(compiled) => {
                object.set(name.as_str(), compiled.clone())?;
                handler = compiled.into_value();
            }
            Err(error) => {
                // A failed compilation reports the error and clears the
                // handler; it must not abort the dispatch (or the attribute
                // set) that triggered it.
                report_exception(ctx, &error);
                object.set(name.as_str(), Value::new_null(ctx.clone()))?;
                return Ok(());
            }
        }
    }
    if let Some(function) = handler.as_function() {
        // A realm's own global is an `OnErrorEventHandler`: both `Window` and
        // `WorkerGlobalScope` call `onerror` with
        // `(message, filename, lineno, colno, error)`. A `Worker` object is
        // also a non-node event target, but its `onerror` receives the
        // `ErrorEvent` like any other handler
        // (<https://html.spec.whatwg.org/multipage/webappapis.html#the-event-handler-processing-algorithm>).
        let is_window = {
            let world = bindings::world(ctx)?;
            let world = world.borrow();
            world
                .window_object()
                .and_then(|window| window.restore(ctx).ok())
                .is_some_and(|window| window.as_value() == object.as_value())
        };
        let result = if typ == "error" && is_window {
            call_error_handler(ctx, function, object, event)
        } else {
            function.call::<_, ()>((This(object.clone()), event.clone()))
        };
        if let Err(error) = result {
            report_exception(ctx, &error);
        }
    }
    Ok(())
}

/// The special `window.onerror` signature: `(message, filename, lineno, colno,
/// error)`, where returning `true` cancels the event
/// (<https://html.spec.whatwg.org/multipage/webappapis.html#the-event-handler-processing-algorithm>).
fn call_error_handler<'js>(
    ctx: &Ctx<'js>,
    function: &Function<'js>,
    object: &Object<'js>,
    event: &Value<'js>,
) -> Result<()> {
    let Some(event_object) = event.as_object() else {
        return function.call::<_, ()>((This(object.clone()), event.clone()));
    };
    let field = |name: &str| {
        event_object
            .get::<_, Value>(name)
            .unwrap_or_else(|_| Value::new_undefined(ctx.clone()))
    };
    let message = field("message");
    let filename = field("filename");
    let lineno = field("lineno");
    let colno = field("colno");
    let error = field("error");
    let result: Value = function.call((
        This(object.clone()),
        message,
        filename,
        lineno,
        colno,
        error,
    ))?;
    if result.as_bool() == Some(true)
        && let Ok(prevent) = event_object.get::<_, Function>("preventDefault")
        && prevent
            .call::<_, ()>((This(event_object.clone()),))
            .is_err()
    {
        // Clear the exception a page-clobbered `preventDefault` threw.
        let _ = ctx.catch();
    }
    Ok(())
}

/// The content-attribute source for one handler: the target node's own
/// attribute, or for a window target the active document body's attribute,
/// which forwards window handlers to the window
/// (<https://html.spec.whatwg.org/multipage/dom.html#body-element-event-handlers>).
fn handler_attribute_source<'js>(
    ctx: &Ctx<'js>,
    target: &Object<'js>,
    name: &str,
) -> Result<Option<String>> {
    if let Some(id) = bindings::host_node_id(ctx, &target.clone().into_value()) {
        if bindings::handler_cleared(ctx, id, name)? {
            return Ok(None);
        }
        let body = bindings::handler_attribute(ctx, id, name)?;
        return Ok(body.filter(|source| !source.trim().is_empty()));
    }
    let Some(body) = active_body(ctx) else {
        return Ok(None);
    };
    if bindings::handler_cleared(ctx, body, name)?
        || bindings::window_handler_cleared(ctx, body, name)?
    {
        return Ok(None);
    }
    Ok(bindings::handler_attribute(ctx, body, name)?.filter(|source| !source.trim().is_empty()))
}

/// The body element of the current realm's active document, when it has one.
fn active_body(ctx: &Ctx<'_>) -> Option<crate::dom::tree::NodeId> {
    let world = bindings::world(ctx).ok()?;
    let world = world.borrow();
    let parsed = world.main_document()?;
    let document = parsed.id;
    let base = &parsed.document.base;
    base.query_selector_in(base.root_node().id, "body")
        .ok()
        .flatten()
        .map(|node| crate::dom::tree::NodeId { document, node })
}

/// `ToBoolean` for an optional argument; a missing or undefined argument is
/// false (<https://webidl.spec.whatwg.org/#es-boolean>).
fn boolean_argument<'js>(ctx: &Ctx<'js>, value: Option<Value<'js>>) -> Result<bool> {
    match value {
        Some(value) if !value.is_undefined() => bindings::to_boolean(ctx, &value),
        _ => Ok(false),
    }
}

fn same_callback(
    ctx: &Ctx<'_>,
    stored: Option<&Persistent<Value<'static>>>,
    live: Option<&Persistent<Value<'static>>>,
) -> Result<bool> {
    match (stored, live) {
        (None, None) => Ok(true),
        (Some(stored), Some(live)) => {
            let stored = stored.clone().restore(ctx)?;
            let live = live.clone().restore(ctx)?;
            Ok(stored == live)
        }
        _ => Ok(false),
    }
}
