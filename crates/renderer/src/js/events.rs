//! DOM events: the `Event` and `EventTarget` platform objects and the
//! dispatch algorithm (<https://dom.spec.whatwg.org/#events>).
//!
//! One event listener list is keyed per target in the [`World`] that owns the
//! target, so nodes, the window, and constructible `EventTarget`s all share
//! the same add/remove/invoke code.

use std::cell::{Cell, Ref, RefCell, RefMut};
use std::rc::Rc;

use rquickjs::{
    Class, Ctx, Exception, FromJs, Function, Object, Persistent, Result, Value,
    class::{Trace, Tracer},
    function::{Rest, This},
};

use super::bindings;
use super::bindings::JsNode;
use super::world::{EventTargetKey, GlobalKind, HandlerValue, Listener, World};
use crate::dom::tree::NodeId;

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

/// `DOMHighResTimeStamp` for `Event.timeStamp`, measured from the realm's
/// `performance` time origin
/// (<https://dom.spec.whatwg.org/#dom-event-timestamp>).
fn stamp_event(ctx: &Ctx<'_>, event: &JsEvent) {
    let Ok(world) = bindings::world(ctx) else {
        return;
    };
    let (now, resolution) = {
        let world = world.borrow();
        let (sampled, origin) = world.event_clock;
        let now = origin + sampled.elapsed().as_secs_f64() * 1000.0;
        (now, world.time_resolution_micros())
    };
    event.state_mut().time_stamp = super::world::clamp_time(now, resolution);
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

    pub(crate) fn trusted_with(typ: &str, bubbles: bool, cancelable: bool, composed: bool) -> Self {
        Self {
            state: EventStateCell(RefCell::new(EventState {
                typ: typ.to_owned(),
                bubbles,
                cancelable,
                composed,
                is_trusted: true,
                initialized: true,
                ..EventState::default()
            })),
        }
    }

    /// The event `Document.createEvent()` returns: type empty and
    /// `initialized` unset until `initEvent()`
    /// (<https://dom.spec.whatwg.org/#dom-document-createevent>).
    pub(crate) fn uninitialized() -> Self {
        Self {
            state: EventStateCell(RefCell::new(EventState::default())),
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
        ctx: &Ctx<'js>,
        typ: rquickjs::String<'js>,
        init: event_generated::EventInit,
    ) -> Result<Self> {
        let event = Self::from_init(typ.to_string()?, &init);
        stamp_event(ctx, &event);
        Ok(event)
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
    // `window.event` reads the realm's current event, which the inner invoke
    // algorithm updates around each listener
    // (<https://dom.spec.whatwg.org/#concept-event-listener-inner-invoke>).
    super::bridge::object(ctx)?.set(
        "__tbCurrentEvent",
        rquickjs::prelude::Func::from(current_event),
    )?;
    super::bridge::object(ctx)?.set(
        "__tbEventDispatching",
        rquickjs::prelude::Func::from(event_dispatching_bridge),
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

/// The `window.event` getter: this realm's current event, or undefined while
/// no listener of this realm runs
/// (<https://dom.spec.whatwg.org/#window-current-event>).
fn current_event(ctx: Ctx<'_>) -> Result<Value<'_>> {
    let world = bindings::world(&ctx)?;
    let saved = world.borrow().current_event.clone();
    match saved {
        Some(saved) => Ok(saved.restore(&ctx)?),
        None => Ok(Value::new_undefined(ctx)),
    }
}

/// Whether an event's dispatch flag is set, for the legacy `init*Event`
/// operations, which must do nothing while dispatched
/// (<https://w3c.github.io/uievents/#dom-uievent-inituievent>).
fn event_dispatching_bridge<'js>(_ctx: Ctx<'js>, event: Class<'js, JsEvent>) -> bool {
    event.borrow().state().dispatching
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
pub(crate) fn realm_event<'js>(ctx: &Ctx<'js>, event: JsEvent) -> Result<Class<'js, JsEvent>> {
    stamp_event(ctx, &event);
    bindings::host::instance(ctx, event)
}

/// The `click` event `HTMLElement.click()` dispatches: a `MouseEvent`, which
/// is what makes dispatch treat it as an activation event
/// (<https://html.spec.whatwg.org/multipage/interaction.html#dom-click>).
pub(crate) fn synthetic_click_event<'js>(ctx: &Ctx<'js>) -> Result<Class<'js, JsEvent>> {
    let event = realm_event(ctx, JsEvent::uninitialized())?;
    event.borrow().initialize("click".to_owned(), true, true);
    set_exposed_event_prototype(ctx, &event, "MouseEvent")?;
    Ok(event)
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
            handler_name: None,
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
    // A null callback removes nothing
    // (<https://dom.spec.whatwg.org/#dom-eventtarget-removeeventlistener> step 2).
    let Some(callback) = callback else {
        return Ok(());
    };
    let mut world = world.borrow_mut();
    let mut removed = Vec::new();
    for existing in world.listener_snapshot(target) {
        if existing.typ == typ
            && existing.capture == capture
            && same_callback(ctx, existing.callback.as_ref(), Some(callback))?
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

/// Fires one CSS animation or transition lifecycle event at its element. The
/// events bubble and are not cancelable
/// (<https://drafts.csswg.org/css-animations-1/#events>,
/// <https://drafts.csswg.org/css-transitions-1/#transition-events>).
pub(crate) fn fire_animation_event(
    ctx: &Ctx<'_>,
    target: EventTargetKey,
    event: &blitz_dom::animation_events::PendingAnimationEvent,
) -> Result<()> {
    let interface = if event.kind.is_transition() {
        "TransitionEvent"
    } else {
        "AnimationEvent"
    };
    let class = realm_event(ctx, JsEvent::trusted(event.kind.event_type(), true, false))?;
    // A trusted event is not built by the shim's constructor, so its slot has
    // no entry. Expose the interface prototype when the realm has it and set
    // the readonly members as own data properties, which shadow the shim's
    // slot-reading getters
    // (<https://webidl.spec.whatwg.org/#idl-read-only-attributes>).
    if bindings::world(ctx)?
        .borrow()
        .event_ctors
        .contains_key(interface)
    {
        set_exposed_event_prototype(ctx, &class, interface)?;
    }
    let name_member = if event.kind.is_transition() {
        "propertyName"
    } else {
        "animationName"
    };
    class.prop(name_member, event.name.clone())?;
    class.prop("elapsedTime", event.elapsed_time)?;
    class.prop("pseudoElement", event.pseudo_element.clone())?;
    dispatch(ctx, target, &class, None)?;
    Ok(())
}

/// Fires a trusted `toggle` event carrying the states the element moved
/// between. The event uses the `ToggleEvent` interface when the realm exposes
/// it and does not bubble or cancel
/// (<https://html.spec.whatwg.org/multipage/popover.html#the-toggleevent-interface>).
pub(crate) fn fire_toggle(
    ctx: &Ctx<'_>,
    target: EventTargetKey,
    old_state: &str,
    new_state: &str,
) -> Result<()> {
    fire_toggle_event(
        ctx, target, "toggle", old_state, new_state, None, false, false,
    )?;
    Ok(())
}

/// Fires one trusted `ToggleEvent` and reports whether it was not canceled, so
/// `show()` can stop when a `beforetoggle` listener prevents the change
/// (<https://html.spec.whatwg.org/multipage/popover.html#the-toggleevent-interface>,
/// <https://html.spec.whatwg.org/multipage/interactive-elements.html#the-dialog-element:dom-dialog-show>).
#[expect(
    clippy::too_many_arguments,
    reason = "one parameter per ToggleEvent member plus its dispatch flags"
)]
pub(crate) fn fire_toggle_event(
    ctx: &Ctx<'_>,
    target: EventTargetKey,
    typ: &str,
    old_state: &str,
    new_state: &str,
    source: Option<crate::dom::tree::NodeId>,
    bubbles: bool,
    cancelable: bool,
) -> Result<bool> {
    let class = realm_event(ctx, JsEvent::trusted(typ, bubbles, cancelable))?;
    if bindings::world(ctx)?
        .borrow()
        .event_ctors
        .contains_key("ToggleEvent")
    {
        set_exposed_event_prototype(ctx, &class, "ToggleEvent")?;
    }
    class.prop("oldState", old_state)?;
    class.prop("newState", new_state)?;
    let source = match source {
        Some(id) => bindings::wrap_node(ctx, id)?,
        None => Value::new_null(ctx.clone()),
    };
    class.prop("source", source)?;
    dispatch(ctx, target, &class, None)
}

/// Dispatches an event the shim built at `target` with the trust bit set and
/// reports whether it was not canceled.
///
/// A submission an activation behavior starts is a user-agent event, so its
/// `submit` listener has to see `isTrusted` true, and only the engine can set
/// that bit. Everything else about the event (the `SubmitEvent` prototype and
/// its `submitter`) stays with the shim, which is why it builds the object and
/// the engine only owns the dispatch
/// (<https://dom.spec.whatwg.org/#concept-event-dispatch>,
/// <https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#form-submission-algorithm>).
pub(crate) fn dispatch_shim_event_trusted<'js>(
    ctx: &Ctx<'js>,
    target: EventTargetKey,
    event: Value<'js>,
) -> Result<bool> {
    let event = Class::<JsEvent>::from_js(ctx, event)?;
    dispatch_checked(ctx, target, &event, true, None)
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

/// Fires the trusted `hashchange` of a same-document fragment navigation.
/// `oldURL` and `newURL` live in the interface's JavaScript slot map, so the
/// shim writes them while this function owns the trust flag and dispatch
/// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#the-hashchangeevent-interface>).
pub(crate) fn fire_trusted_hashchange(ctx: &Ctx<'_>, old_url: &str, new_url: &str) -> Result<()> {
    let event = realm_event(ctx, JsEvent::trusted("hashchange", false, false))?;
    if let Some(prototype) = bindings::world(ctx)?.borrow().brand("HashChangeEvent") {
        event.set_prototype(Some(&prototype.restore(ctx)?))?;
    }
    let setter: Function = crate::js::bridge::object(ctx)?.get("__tbSetHashChange")?;
    let _: Value = setter.call((Class::into_value(event.clone()), old_url, new_url))?;
    dispatch(ctx, EventTargetKey::Window, &event, None)?;
    Ok(())
}

/// Fires the trusted `pagehide` of a document unload step, with `persisted`
/// false because this engine keeps no page in a back/forward cache
/// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#the-pagetransitionevent-interface>).
pub(crate) fn fire_trusted_pagehide(ctx: &Ctx<'_>) -> Result<()> {
    let event = realm_event(ctx, JsEvent::trusted("pagehide", false, false))?;
    if let Some(prototype) = bindings::world(ctx)?.borrow().brand("PageTransitionEvent") {
        event.set_prototype(Some(&prototype.restore(ctx)?))?;
    }
    dispatch(ctx, EventTargetKey::Window, &event, None)?;
    Ok(())
}

/// Runs a document's unload steps in `ctx`'s realm: trusted `pagehide` (this
/// engine keeps no page in a back/forward cache, so `persisted` is false) then
/// `unload`, both on the window
/// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#the-pagetransitionevent-interface>,
/// <https://html.spec.whatwg.org/multipage/browsing-the-web.html#unload>).
pub(crate) fn fire_document_unload(ctx: &Ctx<'_>) -> Result<()> {
    fire_trusted_pagehide(ctx)?;
    fire_trusted(ctx, EventTargetKey::Window, "unload", false, false)
}

/// Fires a trusted `FocusEvent` that carries the related target. Dispatch
/// retargets it for every path struct, so a listener outside a shadow tree sees
/// the host instead of the inner node
/// (<https://w3c.github.io/uievents/#events-focusevent>).
pub(crate) fn fire_trusted_focus(
    ctx: &Ctx<'_>,
    target: EventTargetKey,
    typ: &str,
    bubbles: bool,
    related: Option<NodeId>,
) -> Result<()> {
    let event = realm_event(ctx, JsEvent::trusted(typ, bubbles, false))?;
    set_exposed_event_prototype(ctx, &event, "FocusEvent")?;
    if let Some(related) = related {
        let value = bindings::wrap_node(ctx, related)?;
        let init: Function = crate::js::bridge::object(ctx)?.get("__tbInitRelatedTarget")?;
        init.call::<_, ()>((Class::into_value(event.clone()), value))?;
    }
    dispatch(ctx, target, &event, None)?;
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
    // A user-agent click is a `MouseEvent` (browsers dispatch a `PointerEvent`,
    // a `MouseEvent` subclass), which is what makes dispatch treat it as an
    // activation event (<https://dom.spec.whatwg.org/#concept-event-dispatch>).
    set_exposed_event_prototype(ctx, &event, "MouseEvent")?;
    dispatch(ctx, target, &event, None)
}

/// The early return when the retargeted related target is the target itself: no
/// listener runs, and the targets are reset. The reset is what the WPT test for
/// this branch asserts
/// (<https://dom.spec.whatwg.org/#concept-event-dispatch>).
fn skip_dispatch<'js>(
    ctx: &Ctx<'js>,
    event: &Class<'js, JsEvent>,
    event_value: &Value<'js>,
    has_related: bool,
) -> Result<bool> {
    let canceled = {
        let class = event.borrow();
        let mut state = class.state_mut();
        state.dispatching = false;
        state.target = None;
        state.current_target = None;
        state.phase = NONE;
        state.path.clear();
        state.stop_propagation = false;
        state.stop_immediate = false;
        state.canceled
    };
    if has_related {
        store_related_target(ctx, event_value, None)?;
    }
    Ok(!canceled)
}

/// Whether the event reports no targets after dispatch: the last struct with
/// its own shadow-adjusted target, or that struct's related target, is a node
/// whose root is a shadow root. It is decided before any listener runs, so a
/// listener that moves the target out of the tree cannot change it
/// (<https://dom.spec.whatwg.org/#concept-event-dispatch>).
fn clears_targets<'js>(ctx: &Ctx<'js>, path: &[PathItem<'js>]) -> bool {
    match path
        .iter()
        .rev()
        .find(|item| item.shadow_adjusted_target.is_some())
    {
        Some(item) => {
            item.shadow_adjusted_target
                .as_ref()
                .is_some_and(reference_is_in_shadow_tree)
                || item
                    .related_target
                    .as_ref()
                    .is_some_and(|value| value_is_in_shadow_tree(ctx, value))
        }
        None => false,
    }
}

/// The end-of-dispatch bookkeeping: clears the phase, current target, path, and
/// stop flags, drops the targets when `clear_targets` is set, and returns the
/// canceled flag (<https://dom.spec.whatwg.org/#concept-event-dispatch>).
fn finish_dispatch<'js>(
    ctx: &Ctx<'js>,
    event: &Class<'js, JsEvent>,
    event_value: &Value<'js>,
    clear_targets: bool,
    has_related: bool,
) -> Result<bool> {
    let canceled = {
        let class = event.borrow();
        let mut state = class.state_mut();
        state.phase = NONE;
        state.current_target = None;
        state.path.clear();
        state.dispatching = false;
        state.stop_propagation = false;
        state.stop_immediate = false;
        if clear_targets {
            state.target = None;
        }
        state.canceled
    };
    if clear_targets && has_related {
        store_related_target(ctx, event_value, None)?;
    }
    Ok(canceled)
}

/// [Dispatch](https://dom.spec.whatwg.org/#concept-event-dispatch) an event.
fn dispatch<'js>(
    ctx: &Ctx<'js>,
    target: EventTargetKey,
    event: &Class<'js, JsEvent>,
    window: Option<&Rc<RefCell<World>>>,
) -> Result<bool> {
    let composed = event.borrow().state().composed;
    let event_value = Class::into_value(event.clone());
    let related = related_target_value(ctx, &event_value)?;
    let path = build_path(ctx, target, window, composed, related.as_ref())?;
    if path.is_empty() {
        return skip_dispatch(ctx, event, &event_value, related.is_some());
    }
    {
        let class = event.borrow();
        let mut state = class.state_mut();
        state.dispatching = true;
        state.target = Some(path[0].event_target.clone());
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
    // A click event runs the element's legacy-pre-activation behavior before
    // its listeners and completes or rolls it back after dispatch. Only a
    // `MouseEvent` named `click` is an activation event; the page can
    // dispatch a plain `Event("click")`, which has no activation behavior.
    // `click()` owns that sequence itself, so it marks the dispatch to skip
    // this path
    // (<https://dom.spec.whatwg.org/#concept-event-dispatch>,
    // <https://html.spec.whatwg.org/multipage/input.html#the-input-element:legacy-pre-activation-behavior>).
    let is_activation_event =
        typ == "click" && is_event_of(ctx, &Class::into_value(event.clone()), "MouseEvent");
    let click_activation = if is_activation_event && let EventTargetKey::Node(node) = target {
        let activation = super::bindings::focus::activation_target(ctx, node, bubbles)?;
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
    // `clearTargets` is decided before any listener runs, so a listener that
    // moves the target out of the tree cannot change it
    // (<https://dom.spec.whatwg.org/#concept-event-dispatch>).
    let clear_targets = clears_targets(ctx, &path);
    let result = run_invocations(ctx, event, &path, target, bubbles, &typ);
    // Handlers on elements, documents, and a document realm's window are
    // listeners, and their listener already ran with the list. Only the
    // targets this engine keeps as plain properties run here, after the
    // list. A stopped event never reaches them.
    let stopped = {
        let class = event.borrow();
        let state = class.state();
        state.stop_propagation || state.stop_immediate
    };
    let handler = if result.is_ok() && !stopped {
        run_plain_property_handlers(ctx, event, &path, &typ, bubbles)
    } else {
        Ok(())
    };
    // Clear the dispatch state on both paths, so a failed invocation cannot
    // leave the event permanently undispatchable.
    let canceled = finish_dispatch(ctx, event, &event_value, clear_targets, related.is_some())?;
    let outcome = result.and(handler);
    // The activation target runs its activation behavior after dispatch:
    // roll back for a canceled event, otherwise complete it. A label or
    // summary is chosen here like any other activation target, so only one
    // element activates per click
    // (<https://dom.spec.whatwg.org/#concept-event-dispatch>).
    if let Some((node, previous)) = click_activation {
        if canceled {
            super::bindings::focus::legacy_canceled_activation(ctx, node, &previous)?;
        } else if let EventTargetKey::Node(event_target) = target {
            super::bindings::focus::complete_activation(ctx, node, &previous, event_target)?;
        }
    }
    // The per-listener current event already restored itself even when a
    // listener threw, so dispatch only has to surface the first error.
    outcome.map(|()| !canceled)
}

fn run_invocations<'js>(
    ctx: &Ctx<'js>,
    event: &Class<'js, JsEvent>,
    path: &[PathItem<'js>],
    target: EventTargetKey,
    bubbles: bool,
    typ: &str,
) -> Result<()> {
    // Capture phase: root to target.
    for item in path.iter().rev() {
        let phase = if item.reference.key == target {
            AT_TARGET
        } else {
            CAPTURING_PHASE
        };
        let found = invoke(ctx, event, item, phase, true)?;
        if !found {
            invoke_legacy(ctx, event, item, phase, true)?;
        }
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
        // A `submit` or `reset` event stops propagating at an ancestor form
        // element during the bubble phase; the capture phase still traverses
        // the whole path. All engines share the quirk, and the HTML Standard
        // records it as a known deviation from the dispatch algorithm
        // (<https://github.com/whatwg/html/issues/3599>,
        // <https://github.com/whatwg/dom/issues/756>).
        if phase == BUBBLING_PHASE
            && matches!(typ, "submit" | "reset")
            && is_html_form(&item.reference)
        {
            break;
        }
        let found = invoke(ctx, event, item, phase, false)?;
        if !found {
            invoke_legacy(ctx, event, item, phase, false)?;
        }
    }
    Ok(())
}

/// The legacy event type a trusted CSS animation or transition event is
/// rewritten to for a listener list that had no unprefixed listener
/// (<https://dom.spec.whatwg.org/#concept-event-listener-invoke> step 2).
fn legacy_event_type(typ: &str) -> Option<&'static str> {
    match typ {
        "animationend" => Some("webkitAnimationEnd"),
        "animationiteration" => Some("webkitAnimationIteration"),
        "animationstart" => Some("webkitAnimationStart"),
        "transitionend" => Some("webkitTransitionEnd"),
        _ => None,
    }
}

/// Runs the legacy-event rewrite for one path item: when the first inner
/// invoke found no listener and the event is trusted, the event's type becomes
/// the prefixed legacy name for a second pass, then is restored
/// (<https://dom.spec.whatwg.org/#concept-event-listener-invoke> step 2).
fn invoke_legacy<'js>(
    ctx: &Ctx<'js>,
    event: &Class<'js, JsEvent>,
    item: &PathItem<'js>,
    phase: u16,
    capturing: bool,
) -> Result<()> {
    {
        let class = event.borrow();
        if !class.state().is_trusted {
            return Ok(());
        }
    }
    let original = event.borrow().state().typ.clone();
    let Some(legacy) = legacy_event_type(&original) else {
        return Ok(());
    };
    {
        let class = event.borrow();
        legacy.clone_into(&mut class.state_mut().typ);
    }
    let outcome = invoke(ctx, event, item, phase, capturing);
    event.borrow().state_mut().typ = original;
    outcome.map(|_| ())
}

/// Whether an event path item is an HTML `form` element.
fn is_html_form(item: &EventTargetRef) -> bool {
    let EventTargetKey::Node(id) = item.key else {
        return false;
    };
    let world = item.world.borrow();
    let Some(parsed) = world.document(id) else {
        return false;
    };
    crate::dom::tree::is_html_tag(
        parsed
            .document
            .base
            .get_node(id.node)
            .map(|tree| &tree.data),
        "form",
    )
}

/// One event path item (<https://dom.spec.whatwg.org/#event-path-item>).
struct PathItem<'js> {
    reference: EventTargetRef,
    target: Value<'js>,
    /// This struct's shadow-adjusted target, when it has its own. The dispatch
    /// target and, for a composed event that leaves a shadow tree, the host
    /// carry one; every other struct reports the nearest preceding one
    /// (<https://dom.spec.whatwg.org/#concept-event-dispatch>).
    shadow_adjusted_target: Option<EventTargetRef>,
    /// `event.target` while this struct's listeners run: this struct's
    /// shadow-adjusted target or the nearest preceding non-null one
    /// (<https://dom.spec.whatwg.org/#concept-event-listener-invoke>).
    event_target: EventTargetRef,
    /// `event.relatedTarget` while this struct's listeners run, retargeted
    /// against the invocation target
    /// (<https://dom.spec.whatwg.org/#concept-event-dispatch>).
    related_target: Option<Value<'js>>,
    /// Whether the invocation target's root is a shadow root, which suppresses
    /// the legacy `window.event` for this item's listeners
    /// (<https://dom.spec.whatwg.org/#concept-event-path-append>).
    invocation_target_in_shadow_tree: bool,
}

/// The event's `relatedTarget`, as the interface stores it. `None` when the
/// value is null or the interface has no `relatedTarget` attribute
/// (<https://dom.spec.whatwg.org/#concept-event-relatedtarget>).
fn related_target_value<'js>(ctx: &Ctx<'js>, event: &Value<'js>) -> Result<Option<Value<'js>>> {
    let bridge = crate::js::bridge::object(ctx)?;
    let getter: Function = bridge.get("__tbRelatedTargetGet")?;
    let value: Value = getter.call((event.clone(),))?;
    Ok((!value.is_null() && !value.is_undefined()).then_some(value))
}

/// Stores `relatedTarget` back into the interface's own slot, so a listener
/// reads the value the current path struct defines
/// (<https://dom.spec.whatwg.org/#concept-event-listener-invoke>).
fn store_related_target<'js>(
    ctx: &Ctx<'js>,
    event: &Value<'js>,
    value: Option<&Value<'js>>,
) -> Result<()> {
    let bridge = crate::js::bridge::object(ctx)?;
    let setter: Function = bridge.get("__tbRelatedTargetSet")?;
    let value = match value {
        Some(value) => value.clone(),
        None => Value::new_null(ctx.clone()),
    };
    setter.call::<_, ()>((event.clone(), value))
}

/// Whether `ancestor` is a shadow-including inclusive ancestor of `node`
/// (<https://dom.spec.whatwg.org/#concept-shadow-including-inclusive-ancestor>).
fn is_shadow_including_inclusive_ancestor(
    world: &Rc<RefCell<World>>,
    ancestor: NodeId,
    node: NodeId,
) -> bool {
    let mut current = node;
    loop {
        if current == ancestor {
            return true;
        }
        let (parent, host) = {
            let world = world.borrow();
            (world.node_parent(current), world.shadow_root_host(current))
        };
        match parent.or(host) {
            Some(next) => current = next,
            None => return false,
        }
    }
}

/// Retargets a related target against the object it is reported to
/// (<https://dom.spec.whatwg.org/#retarget>).
///
/// Only a node can be retargeted: a related target that is not a node, or a
/// node whose root is not a shadow root, is returned unchanged.
fn retarget_related<'js>(
    ctx: &Ctx<'js>,
    value: &Value<'js>,
    against: &EventTargetRef,
) -> Result<Value<'js>> {
    let Some(mut id) = bindings::host_node_id(ctx, value) else {
        return Ok(value.clone());
    };
    // A related target against a non-node target still retargets: the target
    // being a node only decides whether a root already contains it
    // (<https://dom.spec.whatwg.org/#retarget>).
    let against_node = match against.key {
        EventTargetKey::Node(id) => {
            Some(bindings::realm_registry(ctx)?.borrow_mut().live_node_id(id))
        }
        _ => None,
    };
    let original = id;
    loop {
        let Some(world) = bindings::owning_world(ctx, id) else {
            break;
        };
        let root = plain_root(&world, id);
        // A's root not being a shadow root ends the loop at the current node
        let Some(host) = world.borrow().shadow_root_host(root) else {
            break;
        };
        if against_node
            .is_some_and(|against| is_shadow_including_inclusive_ancestor(&world, root, against))
        {
            break;
        }
        id = host;
    }
    if id == original {
        return Ok(value.clone());
    }
    let world =
        bindings::owning_world(ctx, id).ok_or_else(|| Exception::throw_type(ctx, "stale node"))?;
    resolve_target(
        ctx,
        &EventTargetRef {
            key: EventTargetKey::Node(id),
            world,
        },
    )
}

/// Whether a related target value is a node whose root is a shadow root
/// (<https://dom.spec.whatwg.org/#concept-tree-root>).
fn value_is_in_shadow_tree<'js>(ctx: &Ctx<'js>, value: &Value<'js>) -> bool {
    let Some(id) = bindings::host_node_id(ctx, value) else {
        return false;
    };
    let Some(world) = bindings::owning_world(ctx, id) else {
        return false;
    };
    node_root_is_shadow(&world, id)
}

/// Whether a target reference is a node whose root is a shadow root
/// (<https://dom.spec.whatwg.org/#concept-tree-root>).
fn reference_is_in_shadow_tree(reference: &EventTargetRef) -> bool {
    let EventTargetKey::Node(id) = reference.key else {
        return false;
    };
    node_root_is_shadow(&reference.world, id)
}

/// Whether `id`'s tree root is a shadow root
/// (<https://dom.spec.whatwg.org/#concept-tree-root>).
fn node_root_is_shadow(world: &Rc<RefCell<World>>, mut id: NodeId) -> bool {
    let world = world.borrow();
    while let Some(parent) = world.node_parent(id) {
        id = parent;
    }
    let Some(parsed) = world.document(id) else {
        return false;
    };
    parsed
        .document
        .base
        .get_node(id.node)
        .is_some_and(|node| matches!(node.data, blitz_dom::NodeData::ShadowRoot { .. }))
}

/// The event target reference for a dispatch target
/// (<https://dom.spec.whatwg.org/#concept-event-target>).
fn target_reference(
    ctx: &Ctx<'_>,
    target: EventTargetKey,
    window: Option<&Rc<RefCell<World>>>,
) -> Result<EventTargetRef> {
    let world = match target {
        EventTargetKey::Window => match window {
            Some(world) => Rc::clone(world),
            None => bindings::world(ctx)?,
        },
        _ => target_world(ctx, target)?,
    };
    Ok(EventTargetRef { key: target, world })
}

/// The root of `id` in its tree
/// (<https://dom.spec.whatwg.org/#concept-tree-root>).
fn plain_root(world: &Rc<RefCell<World>>, mut id: NodeId) -> NodeId {
    while let Some(parent) = world.borrow().node_parent(id) {
        id = parent;
    }
    id
}

/// The one-item path of a target that is not a node: an `Attr`, a `Window`, or
/// a constructible `EventTarget`
/// (<https://dom.spec.whatwg.org/#concept-event-path-append>).
fn single_target_path<'js>(
    ctx: &Ctx<'js>,
    reference: EventTargetRef,
    related: Option<Value<'js>>,
) -> Result<Vec<PathItem<'js>>> {
    Ok(vec![PathItem {
        target: resolve_target(ctx, &reference)?,
        shadow_adjusted_target: Some(reference.clone()),
        event_target: reference.clone(),
        reference,
        related_target: related,
        invocation_target_in_shadow_tree: false,
    }])
}

/// One ancestor's path item. `Ok(None)` means the ancestor is the related
/// target, so the path stops before it
/// (<https://dom.spec.whatwg.org/#concept-event-dispatch>).
///
/// `current_root` and `event_target` follow the algorithm's `target` variable:
/// when the path leaves a shadow tree at its host, the host becomes the target
/// for the structs above it.
fn ancestor_path_item<'js>(
    ctx: &Ctx<'js>,
    world: &Rc<RefCell<World>>,
    node: NodeId,
    current_root: &mut NodeId,
    event_target: &mut EventTargetRef,
    related: Option<&Value<'js>>,
) -> Result<Option<PathItem<'js>>> {
    let reference = EventTargetRef {
        key: EventTargetKey::Node(node),
        world: Rc::clone(world),
    };
    let value = resolve_target(ctx, &reference)?;
    let related_here = match related {
        Some(original) => Some(retarget_related(ctx, original, &reference)?),
        None => None,
    };
    if is_shadow_including_inclusive_ancestor(world, *current_root, node) {
        let invocation_target_in_shadow_tree = bindings::node_is_shadow_root(ctx, *current_root)?;
        return Ok(Some(PathItem {
            reference,
            target: value,
            shadow_adjusted_target: None,
            event_target: event_target.clone(),
            related_target: related_here,
            invocation_target_in_shadow_tree,
        }));
    }
    if related_here
        .as_ref()
        .is_some_and(|related| related.strict_eq(&value))
    {
        return Ok(None);
    }
    *current_root = plain_root(world, node);
    *event_target = reference.clone();
    let invocation_target_in_shadow_tree = bindings::node_is_shadow_root(ctx, *current_root)?;
    Ok(Some(PathItem {
        reference,
        target: value,
        shadow_adjusted_target: Some(event_target.clone()),
        event_target: event_target.clone(),
        related_target: related_here,
        invocation_target_in_shadow_tree,
    }))
}

/// Builds the event path: the target, its ancestors, and the window
/// (<https://dom.spec.whatwg.org/#concept-event-path-append>).
///
/// An empty path means the dispatch is skipped: the retargeted related target
/// is the target itself, so the event never propagates
/// (<https://dom.spec.whatwg.org/#concept-event-dispatch>).
fn build_path<'js>(
    ctx: &Ctx<'js>,
    target: EventTargetKey,
    window: Option<&Rc<RefCell<World>>>,
    composed: bool,
    related: Option<&Value<'js>>,
) -> Result<Vec<PathItem<'js>>> {
    let reference = target_reference(ctx, target, window)?;
    let related_against_target = match related {
        Some(value) => Some(retarget_related(ctx, value, &reference)?),
        None => None,
    };
    // If the target is the retargeted related target, the event is not
    // dispatched at all; the exception is an event whose own relatedTarget is
    // the target, which still runs
    // (<https://dom.spec.whatwg.org/#concept-event-dispatch>).
    if let (Some(value), Some(retargeted)) = (related, &related_against_target) {
        let target_value = resolve_target(ctx, &reference)?;
        if target_value.strict_eq(retargeted) && !target_value.strict_eq(value) {
            return Ok(Vec::new());
        }
    }
    match target {
        EventTargetKey::Window => single_target_path(ctx, reference, related_against_target),
        EventTargetKey::Attribute { .. } | EventTargetKey::Standalone(_) => {
            // https://dom.spec.whatwg.org/#get-the-parent
            // https://dom.spec.whatwg.org/#interface-attr
            // Attr's owner element is not its parent; Chromium's
            // EventPath::CalculatePath likewise follows parentNode().
            if let EventTargetKey::Standalone(id) = target
                && reference.world.borrow().standalone_target(id).is_none()
            {
                return Err(Exception::throw_internal(ctx, "missing EventTarget"));
            }
            single_target_path(ctx, reference, related_against_target)
        }
        EventTargetKey::Node(id) => {
            let world = Rc::clone(&reference.world);
            // The dispatch algorithm's `target` root, which decides whether an
            // ancestor already contains the target, and which changes when the
            // path leaves a shadow tree for its host
            // (<https://dom.spec.whatwg.org/#concept-event-dispatch>).
            let mut current_root = plain_root(&world, id);
            let mut event_target = reference.clone();
            let origin_in_shadow_tree = bindings::node_is_shadow_root(ctx, current_root)?;
            let mut path = vec![PathItem {
                reference: reference.clone(),
                target: resolve_target(ctx, &reference)?,
                shadow_adjusted_target: Some(reference),
                event_target: event_target.clone(),
                related_target: related_against_target,
                invocation_target_in_shadow_tree: origin_in_shadow_tree,
            }];
            let mut cursor = world.borrow().node_parent(id);
            // The dispatch target is the document itself when it has no
            // parent: its window is still the last path item
            // (<https://dom.spec.whatwg.org/#get-the-parent>).
            let mut reached_document = world.borrow().node_is_document(id);
            while let Some(node) = cursor {
                let Some(item) = ancestor_path_item(
                    ctx,
                    &world,
                    node,
                    &mut current_root,
                    &mut event_target,
                    related,
                )?
                else {
                    // The parent is the related target, so the path stops
                    // before it (<https://dom.spec.whatwg.org/#concept-event-dispatch>).
                    break;
                };
                let invocation_target_in_shadow_tree = item.invocation_target_in_shadow_tree;
                path.push(item);
                let parent = world.borrow().node_parent(node);
                if let Some(parent) = parent {
                    cursor = Some(parent);
                } else {
                    reached_document = world.borrow().node_is_document(node);
                    // A composed event continues past a shadow root at its
                    // host (<https://dom.spec.whatwg.org/#get-the-parent>).
                    cursor = if composed && invocation_target_in_shadow_tree {
                        world.borrow().shadow_root_host(node)
                    } else {
                        None
                    };
                }
            }
            if reached_document {
                // A document's parent is its own window, not the dispatching
                // realm's (<https://dom.spec.whatwg.org/#get-the-parent>).
                let window_reference = EventTargetRef {
                    key: EventTargetKey::Window,
                    world,
                };
                let related_here = match related {
                    Some(original) => Some(retarget_related(ctx, original, &window_reference)?),
                    None => None,
                };
                path.push(PathItem {
                    reference: window_reference.clone(),
                    target: resolve_target(ctx, &window_reference)?,
                    shadow_adjusted_target: None,
                    event_target,
                    related_target: related_here,
                    invocation_target_in_shadow_tree: false,
                });
            }
            Ok(path)
        }
    }
}

/// The world whose global object is an event target's relevant global object
/// right now.
///
/// The inner invoke algorithm re-reads `targetGlobal` from the current
/// `event.currentTarget` for every listener, so a node adopted into another
/// document partway through a dispatch changes which document decides whether
/// its remaining listeners run. A node key therefore follows the node's
/// current document; a `Window`, `Attr`, or standalone `EventTarget` belongs to
/// the realm that created it
/// (<https://dom.spec.whatwg.org/#concept-event-listener-inner-invoke>).
fn target_global_world(ctx: &Ctx<'_>, reference: &EventTargetRef) -> Option<Rc<RefCell<World>>> {
    match reference.key {
        EventTargetKey::Node(id) => {
            let id = bindings::realm_registry(ctx)
                .ok()?
                .borrow_mut()
                .live_node_id(id);
            bindings::owning_world(ctx, id)
        }
        EventTargetKey::Window
        | EventTargetKey::Attribute { .. }
        | EventTargetKey::Standalone(_) => Some(Rc::clone(&reference.world)),
    }
}

/// Whether an event target's relevant global object can still receive
/// listeners.
///
/// The inner invoke algorithm skips a listener when that global object is a
/// `Window` whose associated `Document` is not fully active. The engine's
/// proxy for "fully active" is the realm's frame still being in the browsing
/// context tree: removing an `iframe` is what makes its document inactive in
/// practice, and the richer definition (`is_main_document` plus an active
/// document) is not modelled. A worker global is not a `Window`, so its events
/// keep running
/// (<https://dom.spec.whatwg.org/#concept-event-listener-inner-invoke>).
fn target_is_fully_active(ctx: &Ctx<'_>, reference: &EventTargetRef) -> bool {
    let Some(world) = target_global_world(ctx, reference) else {
        return false;
    };
    let world = world.borrow();
    world.kind != GlobalKind::Document || world.has_browsing_context()
}

/// [Invoke](https://dom.spec.whatwg.org/#concept-event-listener-invoke) the
/// listeners of one path item.
///
/// Returns the inner invoke's `found`: whether the listener list held a
/// listener whose type matched the event, before the capture-phase filter
/// skipped it. The caller uses it for the legacy-event rewrite
/// (<https://dom.spec.whatwg.org/#concept-event-listener-invoke> step 2).
fn invoke<'js>(
    ctx: &Ctx<'js>,
    event: &Class<'js, JsEvent>,
    item: &PathItem<'js>,
    phase: u16,
    capturing: bool,
) -> Result<bool> {
    if !target_is_fully_active(ctx, &item.reference) {
        return Ok(false);
    }
    {
        let class = event.borrow();
        if class.state().stop_propagation {
            return Ok(false);
        }
        let mut state = class.state_mut();
        state.phase = phase;
        state.current_target = Some(item.reference.clone());
        // The target is this struct's shadow-adjusted target or, when it has
        // none, the nearest preceding one
        // (<https://dom.spec.whatwg.org/#concept-event-listener-invoke>).
        state.target = Some(item.event_target.clone());
    }
    let listeners = item
        .reference
        .world
        .borrow()
        .listener_snapshot(item.reference.key);
    let event_value = Class::into_value(event.clone());
    if let Some(related) = &item.related_target {
        store_related_target(ctx, &event_value, Some(related))?;
    }
    let mut found = false;
    for listener in listeners {
        // The check is per listener, not per path item: a listener can remove
        // the target's frame or adopt the node into an inactive document, and
        // the remaining listeners are skipped one by one
        // (<https://dom.spec.whatwg.org/#concept-event-listener-inner-invoke>).
        if !target_is_fully_active(ctx, &item.reference) {
            continue;
        }
        if listener.removed.get() {
            continue;
        }
        let typ = event.borrow().state().typ.clone();
        if typ != listener.typ {
            continue;
        }
        // A type match marks the list as found even when the capture-phase
        // filter skips this listener, which is what suppresses the legacy
        // rewrite for that path item
        // (<https://dom.spec.whatwg.org/#concept-event-listener-inner-invoke>).
        found = true;
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
        if let Some(handler_name) = &listener.handler_name {
            // An event handler's listener has no callback: it runs the HTML
            // event handler processing algorithm against the handler's
            // current value
            // (<https://html.spec.whatwg.org/multipage/webappapis.html#the-event-handler-processing-algorithm>).
            run_handler_listener(ctx, event, item, handler_name);
        } else if let Some(callback) = &listener.callback {
            let value: Value<'js> = callback.clone().restore(ctx)?;
            call_with_current_event(
                &value,
                &event_value,
                item.invocation_target_in_shadow_tree,
                |realm| call_listener(realm, &value, &item.target, &event_value),
            );
        }
        event.borrow().state_mut().in_passive = false;
        if event.borrow().state().stop_immediate {
            break;
        }
    }
    Ok(found)
}

/// Runs one listener call with the listener's realm current event set to
/// `event`, and restores the previous value after reporting any exception in
/// that same realm
/// (<https://dom.spec.whatwg.org/#concept-event-listener-inner-invoke> steps
/// 8–13).
///
/// `listenerGlobal` is the callback's own realm, not the dispatching one: a
/// listener from another frame updates that frame's `window.event`, and a
/// thrown exception is reported there. A listener whose invocation target sits
/// in a shadow tree keeps the previous current event, because the legacy
/// attribute is intentionally inaccurate there.
fn call_with_current_event<'js>(
    callback: &Value<'js>,
    event: &Value<'js>,
    invocation_target_in_shadow_tree: bool,
    call: impl FnOnce(&Ctx<'js>) -> Result<()>,
) {
    let realm = listener_realm(callback);
    let Some(world) = bindings::try_world(&realm) else {
        if let Err(error) = call(&realm) {
            report_exception(&realm, &error);
        }
        return;
    };
    let previous = world.borrow().current_event.clone();
    if !invocation_target_in_shadow_tree {
        world.borrow_mut().current_event = Some(Persistent::save(&realm, event.clone()));
    }
    let outcome = call(&realm);
    // The report runs before the restore, so the listener's own realm still
    // sees the event as its current event while `onerror` handles the throw.
    if let Err(error) = outcome {
        report_exception(&realm, &error);
    }
    world.borrow_mut().current_event = previous;
}

/// The realm a listener callback belongs to, which decides whose
/// `window.event` dispatch updates. A function carries its own realm through
/// `QuickJS`; a value that crossed a realm boundary keeps only the caller's
/// context, which is the fallback for callback objects.
fn listener_realm<'js>(callback: &Value<'js>) -> Ctx<'js> {
    callback
        .as_function()
        .and_then(|function| function.realm().ok())
        .unwrap_or_else(|| callback.ctx().clone())
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

/// The violations an event handler content attribute produces under its
/// element's document policy. The element is the event target while it stays
/// connected to that document
/// (<https://w3c.github.io/webappsec-csp/#should-block-inline>,
/// <https://w3c.github.io/webappsec-csp/#report-violation>).
pub(crate) fn handler_attribute_violations(
    world: &crate::js::world::World,
    id: crate::dom::tree::NodeId,
    source: &str,
) -> Vec<crate::csp::Violation> {
    let document_url =
        url::Url::parse(&world.document_url_of(id)).unwrap_or_else(|_| world.document_url.clone());
    let mut violations = world.policy.csp.check_inline(
        &document_url,
        &crate::csp::InlineScript {
            source,
            nonce: "",
            nonceable: false,
            parser_inserted: false,
            kind: crate::csp::InlineKind::ScriptAttribute,
        },
    );
    for violation in &mut violations {
        violation.element = Some(id);
    }
    violations
}

/// Whether an event handler content attribute may run. `position` is the call
/// site that set the attribute, which the report carries as `sourceFile`,
/// `lineNumber`, and `columnNumber`; a handler the parser inserted has none
/// (<https://w3c.github.io/webappsec-csp/#should-block-inline>,
/// <https://w3c.github.io/webappsec-csp/#create-violation-for-global>).
pub(crate) fn handler_attribute_allowed(
    ctx: &Ctx<'_>,
    id: NodeId,
    source: &str,
    position: Option<(&str, u32, u32)>,
) -> bool {
    // The inline check reads the element's own Document's policy list, while
    // the violation is reported on the current settings object's global
    // (<https://w3c.github.io/webappsec-csp/#should-block-inline>).
    let Some(owner) = bindings::owning_world(ctx, id) else {
        return true;
    };
    let mut violations = handler_attribute_violations(&owner.borrow(), id, source);
    if let Some((url, line, column)) = position {
        for violation in &mut violations {
            violation.source_file = Some(url.to_owned());
            violation.line_number = line;
            violation.column_number = column;
        }
    }
    if !violations.is_empty() {
        // The attribute is checked again when it is invoked, and the parser
        // walk checks attributes the parser inserted: report one event per
        // distinct value, not one per check
        // (<https://w3c.github.io/webappsec-csp/#report-violation>).
        let repeated = owner
            .borrow_mut()
            .state
            .reported_handlers
            .insert((id, source.to_owned()), source.to_owned())
            .is_some_and(|previous| previous == source);
        if !repeated {
            // Reporting is a task, so a listener attached later in the same
            // script still sees the event
            // (<https://w3c.github.io/webappsec-csp/#report-violation>).
            owner
                .borrow_mut()
                .queue_csp_violations(Some(id), violations.clone());
        }
    }
    !crate::csp::blocks(&violations)
}

/// The four legacy prefixed animation and transition handlers: their content
/// attribute names are all lowercase while their event types are camelCase
/// (<https://drafts.csswg.org/css-animations-1/#extensions-to-the-globaleventhandlers-mixin>,
/// <https://drafts.csswg.org/css-transitions-1/#extensions-to-the-globaleventhandlers-mixin>).
const PREFIXED_HANDLERS: [(&str, &str); 4] = [
    ("onwebkitanimationend", "webkitAnimationEnd"),
    ("onwebkitanimationiteration", "webkitAnimationIteration"),
    ("onwebkitanimationstart", "webkitAnimationStart"),
    ("onwebkittransitionend", "webkitTransitionEnd"),
];

/// The handler property name for an event type.
///
/// The four legacy `webkit`-prefixed animation and transition event types keep
/// their camelCase spelling in `addEventListener`, while their handler
/// attributes are all lowercase, so deriving the property name is not just
/// `on` plus the type
/// (<https://html.spec.whatwg.org/multipage/webappapis.html#event-handlers-on-elements,-document-objects,-and-window-objects>).
fn handler_attribute_name(typ: &str) -> String {
    PREFIXED_HANDLERS
        .iter()
        .find(|(_, event_type)| *event_type == typ)
        .map_or_else(|| format!("on{typ}"), |(name, _)| (*name).to_owned())
}

/// Runs the handlers this engine keeps as plain properties after the
/// listener list. Event handlers on elements, documents, and a document
/// realm's window are ordinary listeners registered by activation, so their
/// order among `addEventListener` calls is the spec's; only targets this
/// engine does not give an event handler map — constructible targets such as
/// `MessagePort` and `XMLHttpRequest`, `Attr`, and a worker global — still run
/// their property here
/// (<https://html.spec.whatwg.org/multipage/webappapis.html#event-handler-content-attributes>).
fn run_plain_property_handlers<'js>(
    ctx: &Ctx<'js>,
    event: &Class<'js, JsEvent>,
    path: &[PathItem<'js>],
    typ: &str,
    bubbles: bool,
) -> Result<()> {
    let event_value = Class::into_value(event.clone());
    for (index, item) in path.iter().enumerate() {
        if index > 0 && !bubbles {
            break;
        }
        if index > 0 {
            let class = event.borrow();
            let state = class.state();
            if state.stop_propagation || state.stop_immediate {
                break;
            }
        }
        // Handler attributes are listeners too, so a target whose global lost
        // its browsing context runs none of them
        // (<https://dom.spec.whatwg.org/#concept-event-listener-inner-invoke>).
        if !target_is_fully_active(ctx, &item.reference) {
            continue;
        }
        match handler_storage(item.reference.key, &item.reference.world) {
            HandlerStorage::PlainProperty => {}
            HandlerStorage::Window | HandlerStorage::Node(_) => continue,
        }
        {
            let class = event.borrow();
            let mut state = class.state_mut();
            state.current_target = Some(item.reference.clone());
            state.target = Some(item.event_target.clone());
        }
        if let Some(related) = &item.related_target {
            store_related_target(ctx, &event_value, Some(related))?;
        }
        call_handler_attribute(
            ctx,
            Some(&item.target),
            typ,
            &event_value,
            item.invocation_target_in_shadow_tree,
        )?;
    }
    Ok(())
}

/// Calls a target's plain-property `on<type>` handler, for the targets this
/// engine does not give an event handler map: constructible `EventTarget`s
/// (`MessagePort`, `XMLHttpRequest`), `Attr`, and a worker global
/// (<https://html.spec.whatwg.org/multipage/webappapis.html#event-handlers>).
fn call_handler_attribute<'js>(
    ctx: &Ctx<'js>,
    target: Option<&Value<'js>>,
    typ: &str,
    event: &Value<'js>,
    invocation_target_in_shadow_tree: bool,
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
    let name = handler_attribute_name(typ);
    let handler: Value = object.get(name.as_str())?;
    if let Some(function) = handler.as_function() {
        // A realm's own global is an `OnErrorEventHandler`: both `Window` and
        // `WorkerGlobalScope` call `onerror` with
        // `(message, filename, lineno, colno, error)` when the event is an
        // `ErrorEvent` of type `error`. A `Worker` object is also a non-node
        // event target, but its `onerror` receives the `ErrorEvent` like any
        // other handler
        // (<https://html.spec.whatwg.org/multipage/webappapis.html#the-event-handler-processing-algorithm>).
        let is_global = {
            let world = bindings::world(ctx)?;
            let world = world.borrow();
            object.as_value() == ctx.globals().as_value()
                || world
                    .window_object()
                    .and_then(|window| window.restore(ctx).ok())
                    .is_some_and(|window| window.as_value() == object.as_value())
        };
        let special_error = typ == "error" && is_global && is_event_of(ctx, event, "ErrorEvent");
        call_with_current_event(
            function.as_value(),
            event,
            invocation_target_in_shadow_tree,
            |_| {
                if special_error {
                    call_error_handler(ctx, function, object, event).map(|_| ())
                } else {
                    function.call::<_, ()>((This(object.clone()), event.clone()))
                }
            },
        );
    }
    Ok(())
}

/// Where one event target keeps its handlers
/// (<https://html.spec.whatwg.org/multipage/webappapis.html#event-handler-map>).
#[derive(Clone, Copy)]
enum HandlerStorage {
    /// The window's entry, keyed `(None, name)`.
    Window,
    /// An element's or document's entry, keyed `(Some(node), name)`.
    Node(NodeId),
    /// A target this engine has not given an event handler map: `Attr`,
    /// constructible `EventTarget`s, and a worker global.
    PlainProperty,
}

/// The one place that decides how a dispatch path item stores its handlers,
/// so the map's listener path and the plain-property fallback cannot drift.
/// A worker realm installs its handlers as ordinary properties on the global,
/// so its `Window` key is not a map.
fn handler_storage(key: EventTargetKey, world: &Rc<RefCell<World>>) -> HandlerStorage {
    match key {
        EventTargetKey::Window if world.borrow().kind == GlobalKind::Document => {
            HandlerStorage::Window
        }
        EventTargetKey::Window
        | EventTargetKey::Attribute { .. }
        | EventTargetKey::Standalone(_) => HandlerStorage::PlainProperty,
        EventTargetKey::Node(id) => HandlerStorage::Node(id),
    }
}

impl HandlerStorage {
    /// The world's map key for a map-backed target.
    fn map_target(self) -> Option<NodeId> {
        match self {
            Self::Node(id) => Some(id),
            Self::Window | Self::PlainProperty => None,
        }
    }
}

/// Gets the current value of one event handler, compiling a raw content
/// attribute in the handler target document's realm
/// (<https://html.spec.whatwg.org/multipage/webappapis.html#getting-the-current-value-of-the-event-handler>).
pub(crate) fn current_handler_value<'js>(
    ctx: &Ctx<'js>,
    world: &Rc<RefCell<World>>,
    target: Option<NodeId>,
    name: &str,
) -> Result<Value<'js>> {
    let Some(slot) = world.borrow().handler_slot(target, name) else {
        return Ok(Value::new_null(ctx.clone()));
    };
    match slot.value {
        HandlerValue::Null => Ok(Value::new_null(ctx.clone())),
        HandlerValue::Value(saved) => saved.restore(ctx),
        HandlerValue::Raw(body) => {
            // A document with no browsing context, or with scripting
            // disabled, never compiles its event handler content attributes.
            // The raw value stays in the map: only the answer is null, so the
            // handler still works if the document is adopted somewhere
            // scripting is enabled
            // (<https://html.spec.whatwg.org/multipage/webappapis.html#getting-the-current-value-of-the-event-handler>,
            // <https://html.spec.whatwg.org/multipage/browsers.html#sandboxed-scripts-browsing-context-flag>).
            let has_context = {
                let world = world.borrow();
                let document =
                    target.map_or_else(|| world.main_document_id(), |id| Some(id.document_id()));
                document.is_some_and(|document| world.main_document_id() == Some(document))
                    && !world.sandbox.scripts_disabled()
            };
            if !has_context {
                return Ok(Value::new_null(ctx.clone()));
            }
            let Some(realm) = world.borrow().realm_context.clone() else {
                return Ok(Value::new_null(ctx.clone()));
            };
            let compiled: Result<Persistent<Value<'static>>> = realm.with(|realm_ctx| {
                compile_handler_text(&realm_ctx, name, target.is_none(), &body)
                    .map(|function| Persistent::save(&realm_ctx, function.as_value().clone()))
            });
            match compiled {
                Ok(saved) => {
                    world.borrow_mut().set_handler_value(
                        target,
                        name,
                        HandlerValue::Value(saved.clone()),
                    );
                    saved.restore(ctx)
                }
                Err(error) => {
                    // A parse error clears the value but leaves the handler
                    // active, and reports in the handler's realm
                    // (<https://html.spec.whatwg.org/multipage/webappapis.html#getting-the-current-value-of-the-event-handler>).
                    world
                        .borrow_mut()
                        .set_handler_value(target, name, HandlerValue::Null);
                    realm.with(|realm_ctx| report_exception(&realm_ctx, &error));
                    Ok(Value::new_null(ctx.clone()))
                }
            }
        }
    }
}

/// The source text of an event handler's function, which is what
/// `Function.prototype.toString` reports
/// (<https://html.spec.whatwg.org/multipage/webappapis.html#getting-the-current-value-of-the-event-handler>).
fn handler_function_source(name: &str, is_window: bool, body: &str) -> String {
    if name == "onerror" && is_window {
        format!("function {name}(event, source, lineno, colno, error) {{\n{body}\n}}")
    } else {
        format!("function {name}(event) {{\n{body}\n}}")
    }
}

/// Parses `body` as a `FunctionBody`, the requirement on an event handler
/// content attribute. The parse also keeps a body whose braces would close the
/// `sourceText` wrapper out of the function object that is actually returned
/// (<https://html.spec.whatwg.org/multipage/webappapis.html#event-handler-content-attributes>).
fn validate_handler_body(ctx: &Ctx<'_>, body: &str) -> Result<()> {
    let bridge = super::bridge::object(ctx)?;
    let constructor: Object = bridge.get("__tb_function")?;
    let Some(constructor) = constructor.as_constructor() else {
        return Err(Exception::throw_internal(
            ctx,
            "Function constructor is not constructible",
        ));
    };
    // No parameter list: the body may declare anything, including `event`.
    let _: Function = constructor.construct((body.to_owned(),))?;
    Ok(())
}

/// Compiles one handler source text in `ctx`'s realm, returning a function
/// whose `toString()` is the spec's `sourceText`
/// (<https://html.spec.whatwg.org/multipage/webappapis.html#getting-the-current-value-of-the-event-handler>).
fn compile_handler_text<'js>(
    ctx: &Ctx<'js>,
    name: &str,
    is_window: bool,
    body: &str,
) -> Result<Function<'js>> {
    let world = bindings::world(ctx)?;
    // CSP's inline check owns the handler, so the string-compilation hook
    // must not apply the `'unsafe-eval'` rule to it
    // (<https://w3c.github.io/webappsec-csp/#can-compile-strings>).
    world.borrow().compiling_handler.set(true);
    let compiled = (|| {
        validate_handler_body(ctx, body)?;
        let source = handler_function_source(name, is_window, body);
        let mut options = rquickjs::context::EvalOptions::default();
        options.strict = false;
        options.filename = Some(crate::js::bridge::ENGINE_FILENAME.into());
        let value: Value = ctx.eval_with_options(format!("({source})"), options)?;
        value
            .into_function()
            .ok_or_else(|| Exception::throw_type(ctx, "event handler source is not a function"))
    })();
    world.borrow().compiling_handler.set(false);
    compiled
}

/// The event type an event handler listens for, from the same table.
fn handler_event_type(name: &str) -> String {
    PREFIXED_HANDLERS
        .iter()
        .find(|(handler, _)| *handler == name)
        .map_or_else(
            || name.strip_prefix("on").unwrap_or(name).to_owned(),
            |(_, event_type)| (*event_type).to_owned(),
        )
}

/// One handler's listener object. It carries no callback: dispatch sees the
/// name and runs the processing algorithm against the current value
/// (<https://html.spec.whatwg.org/multipage/webappapis.html#the-event-handler-processing-algorithm>).
fn handler_listener(name: &str) -> Rc<Listener> {
    Rc::new(Listener {
        typ: handler_event_type(name),
        callback: None,
        capture: false,
        once: false,
        passive: false,
        signal: None,
        removed: Cell::new(false),
        handler_name: Some(name.to_owned()),
    })
}

/// Registers the listener an event handler uses while it is active
/// (<https://html.spec.whatwg.org/multipage/webappapis.html#activate-an-event-handler>).
pub(crate) fn activate_handler(world: &mut World, target: Option<NodeId>, name: &str) {
    let Some(slot) = world.handler_slot(target, name) else {
        return;
    };
    if slot.listener.is_some() {
        return;
    }
    let listener = handler_listener(name);
    world.add_listener(EventTargetKey::from(target), Rc::clone(&listener));
    world.set_handler_listener(target, name, Some(listener));
}

/// Clears one event handler and removes its listener. The map entry stays
/// with a null value, so a later install pass on the same element does not
/// resurrect the content attribute
/// (<https://html.spec.whatwg.org/multipage/webappapis.html#deactivate-an-event-handler>).
pub(crate) fn deactivate_handler(world: &Rc<RefCell<World>>, target: Option<NodeId>, name: &str) {
    let listener = world
        .borrow()
        .handler_slot(target, name)
        .and_then(|slot| slot.listener);
    if let Some(listener) = listener {
        listener.removed.set(true);
        world
            .borrow_mut()
            .remove_listener(EventTargetKey::from(target), &listener);
    }
    world.borrow_mut().deactivate_handler_slot(target, name);
}

/// Moves one adopted node's handler map entries to the copy that replaces
/// it. Adoption materializes a new node id, and the handler map is node
/// state: a script-assigned handler must travel with the node instead of
/// being rederived from the content attribute
/// (<https://dom.spec.whatwg.org/#concept-node-adopt>).
pub(crate) fn transfer_node_handlers(
    from_world: &Rc<RefCell<World>>,
    from: NodeId,
    to_world: &Rc<RefCell<World>>,
    to: NodeId,
) {
    // The iterator expression's borrow would live for the whole loop, and
    // the body takes the same world mutably. Same-world adoption is the
    // common case.
    let names = from_world.borrow().handler_names(Some(from));
    for name in names {
        let Some(slot) = from_world.borrow_mut().take_handler(Some(from), &name) else {
            continue;
        };
        if let Some(listener) = &slot.listener {
            listener.removed.set(true);
            from_world
                .borrow_mut()
                .remove_listener(EventTargetKey::Node(from), listener);
        }
        to_world
            .borrow_mut()
            .set_handler_value(Some(to), &name, slot.value);
        if slot.listener.is_some() {
            let listener = handler_listener(&name);
            to_world
                .borrow_mut()
                .add_listener(EventTargetKey::Node(to), Rc::clone(&listener));
            to_world
                .borrow_mut()
                .set_handler_listener(Some(to), &name, Some(listener));
        }
    }
}

/// Re-registers the window's handler listeners after a navigation cleared
/// the listener list while keeping the same `Window` object. The handler map
/// entries stay; only their listeners are rebuilt
/// (<https://html.spec.whatwg.org/multipage/webappapis.html#event-handlers>).
pub(crate) fn reactivate_window_handlers(world: &mut World) {
    for name in world.handler_names(None) {
        let active = world
            .handler_slot(None, &name)
            .is_some_and(|slot| !matches!(slot.value, HandlerValue::Null));
        if active {
            activate_handler(world, None, &name);
        }
    }
}

/// Runs one event handler's processing algorithm for a dispatch path item
/// (<https://html.spec.whatwg.org/multipage/webappapis.html#the-event-handler-processing-algorithm>).
fn run_handler_listener<'js>(
    ctx: &Ctx<'js>,
    event: &Class<'js, JsEvent>,
    item: &PathItem<'js>,
    name: &str,
) {
    let storage = handler_storage(item.reference.key, &item.reference.world);
    if matches!(storage, HandlerStorage::PlainProperty) {
        return;
    }
    let world = Rc::clone(&item.reference.world);
    let callback = match current_handler_value(ctx, &world, storage.map_target(), name) {
        Ok(callback) => callback,
        Err(error) => {
            report_exception(ctx, &error);
            return;
        }
    };
    // A handler value that is not callable is ignored at invoke time. The
    // attribute accepts any object (`[LegacyTreatNonObjectAsNull]` clears
    // only non-objects), and a browser does not throw for a plain object or
    // a `Function.prototype` object assigned as the handler
    // (<https://webidl.spec.whatwg.org/#LegacyTreatNonObjectAsNull>,
    // <https://html.spec.whatwg.org/multipage/webappapis.html#the-event-handler-processing-algorithm>).
    let Some(function) = callback.as_function() else {
        return;
    };
    let typ = event.borrow().state().typ.clone();
    let event_value = Class::into_value(event.clone());
    // "Special error event handling" is a property of the event and its
    // target, not of the handler's name: an `ErrorEvent` of type `error`
    // invoked at a global gets the five-argument call and cancels on `true`;
    // every other handler cancels on `false`
    // (<https://html.spec.whatwg.org/multipage/webappapis.html#the-event-handler-processing-algorithm>).
    let special_error = typ == "error"
        && matches!(storage, HandlerStorage::Window)
        && is_event_of(ctx, &event_value, "ErrorEvent");
    let this = item.target.clone();
    call_with_current_event(
        &callback,
        &event_value,
        item.invocation_target_in_shadow_tree,
        |realm| {
            let return_value: Value = if special_error {
                call_error_handler(realm, function, &this, &event_value)?
            } else {
                function.call((This(this.clone()), event_value.clone()))?
            };
            process_handler_return(
                realm,
                event,
                &event_value,
                name,
                &typ,
                special_error,
                &return_value,
            )
        },
    );
}

/// Whether `event` is a `BeforeUnloadEvent` object, which decides the branch
/// the processing algorithm takes for a `beforeunload` event
/// (<https://html.spec.whatwg.org/multipage/webappapis.html#the-event-handler-processing-algorithm>).
fn is_before_unload_event<'js>(ctx: &Ctx<'js>, event: &Value<'js>) -> bool {
    is_event_of(ctx, event, "BeforeUnloadEvent")
}

/// Whether `event` is an instance of this realm's pinned event interface.
/// A page that replaces the global constructor must not change the answer,
/// so the prototype chain is compared against the constructor captured at
/// install (<https://html.spec.whatwg.org/multipage/webappapis.html#dom-createevent>).
fn is_event_of<'js>(ctx: &Ctx<'js>, event: &Value<'js>, interface: &str) -> bool {
    let Some(object) = event.as_object() else {
        return false;
    };
    let Some(pinned) = bindings::world(ctx)
        .ok()
        .and_then(|world| world.borrow().event_ctors.get(interface).cloned())
    else {
        return false;
    };
    let Ok(ctor) = pinned.restore(ctx) else {
        return false;
    };
    let Ok(prototype) = ctor.get::<_, Value>("prototype") else {
        return false;
    };
    let Some(prototype) = prototype.as_object() else {
        return false;
    };
    let mut current = object.get_prototype();
    while let Some(proto) = current {
        if proto.as_value() == prototype.as_value() {
            return true;
        }
        current = proto.get_prototype();
    }
    false
}

/// Processes an event handler's return value
/// (<https://html.spec.whatwg.org/multipage/webappapis.html#the-event-handler-processing-algorithm>).
fn process_handler_return<'js>(
    ctx: &Ctx<'js>,
    event: &Class<'js, JsEvent>,
    event_value: &Value<'js>,
    name: &str,
    typ: &str,
    special_error: bool,
    return_value: &Value<'js>,
) -> Result<()> {
    // `onbeforeunload` is an `OnBeforeUnloadEventHandler`, whose return type
    // is `DOMString?`: undefined and null become null, and every other value
    // is coerced to a string
    // (<https://html.spec.whatwg.org/multipage/webappapis.html#onbeforeunloadeventhandler>).
    let before_unload_return = if name == "onbeforeunload" {
        if return_value.is_null() || return_value.is_undefined() {
            None
        } else {
            Some(bindings::webidl_to_string(ctx, return_value.clone())?)
        }
    } else {
        None
    };
    if typ == "beforeunload" && is_before_unload_event(ctx, event_value) {
        // A non-null return cancels and fills `returnValue` while it is empty.
        let text = match (name == "onbeforeunload", &before_unload_return) {
            (true, coerced) => coerced.clone(),
            (false, _) if return_value.is_null() || return_value.is_undefined() => None,
            (false, _) => Some(bindings::webidl_to_string(ctx, return_value.clone())?),
        };
        if let Some(text) = text {
            set_event_canceled(event);
            if let Some(object) = event_value.as_object() {
                let current: String = object.get("returnValue").unwrap_or_default();
                if current.is_empty() {
                    object.set("returnValue", text)?;
                }
            }
        }
        return Ok(());
    }
    let canceled = if special_error {
        return_value.as_bool() == Some(true)
    } else if name == "onbeforeunload" {
        // A coerced `onbeforeunload` return is a string or null, never the
        // boolean false, so it only cancels a BeforeUnloadEvent
        // (<https://html.spec.whatwg.org/multipage/webappapis.html#the-event-handler-processing-algorithm>).
        false
    } else {
        return_value.as_bool() == Some(false)
    };
    if canceled {
        set_event_canceled(event);
    }
    Ok(())
}

/// Sets an event's canceled flag when it is cancelable and no passive
/// listener is running (<https://dom.spec.whatwg.org/#set-the-canceled-flag>).
fn set_event_canceled(event: &Class<'_, JsEvent>) {
    let class = event.borrow();
    let mut state = class.state_mut();
    if state.cancelable && !state.in_passive {
        state.canceled = true;
    }
}

/// The special `window.onerror` signature: `(message, filename, lineno, colno,
/// error)`
/// (<https://html.spec.whatwg.org/multipage/webappapis.html#the-event-handler-processing-algorithm>).
fn call_error_handler<'js>(
    ctx: &Ctx<'js>,
    function: &Function<'js>,
    this: &Value<'js>,
    event: &Value<'js>,
) -> Result<Value<'js>> {
    let Some(event_object) = event.as_object() else {
        return function.call((This(this.clone()), event.clone()));
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
    function.call((This(this.clone()), message, filename, lineno, colno, error))
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
