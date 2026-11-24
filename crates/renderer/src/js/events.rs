// https://dom.spec.whatwg.org/#events

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

// https://dom.spec.whatwg.org/#dom-event-none
pub(crate) const NONE: u16 = 0;
// https://dom.spec.whatwg.org/#dom-event-capturing_phase
pub(crate) const CAPTURING_PHASE: u16 = 1;
// https://dom.spec.whatwg.org/#dom-event-at_target
pub(crate) const AT_TARGET: u16 = 2;
// https://dom.spec.whatwg.org/#dom-event-bubbling_phase
pub(crate) const BUBBLING_PHASE: u16 = 3;

// https://dom.spec.whatwg.org/#dom-event-timestamp
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

// https://dom.spec.whatwg.org/#concept-event-target
#[derive(Clone)]
pub(crate) struct EventTargetRef {
    pub(crate) key: EventTargetKey,
    pub(crate) world: Rc<RefCell<World>>,
}

// https://dom.spec.whatwg.org/#concept-event
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

#[derive(Trace, rquickjs::JsLifetime)]
pub struct JsEvent {
    pub(crate) state: EventStateCell,
}

impl JsEvent {
    // https://dom.spec.whatwg.org/#concept-event-create
    pub(crate) fn trusted(typ: &str, bubbles: bool, cancelable: bool) -> Self {
        Self::trusted_with(typ, bubbles, cancelable, false)
    }

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

    // https://dom.spec.whatwg.org/#dom-document-createevent
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

    // https://dom.spec.whatwg.org/#concept-event-initialize
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

    // https://dom.spec.whatwg.org/#dom-event-event
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

    // https://dom.spec.whatwg.org/#set-the-canceled-flag
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
        if self.state().dispatching {
            return Ok(());
        }
        self.initialize(typ.to_string()?, bubbles, cancelable);
        Ok(())
    }
}

// https://dom.spec.whatwg.org/#interface-eventtarget
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

// https://dom.spec.whatwg.org/#dom-eventtarget-removeeventlistener
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
    // https://dom.spec.whatwg.org/#concept-event-listener-inner-invoke
    super::bridge::object(ctx)?.set(
        "__tbCurrentEvent",
        rquickjs::prelude::Func::from(current_event),
    )?;
    super::bridge::object(ctx)?.set(
        "__tbEventDispatching",
        rquickjs::prelude::Func::from(event_dispatching_bridge),
    )?;
    super::bridge::object(ctx)?.set(
        "__tbIsEvent",
        rquickjs::prelude::Func::from(is_event_bridge),
    )?;
    Ok(())
}

// https://dom.spec.whatwg.org/#window-current-event
fn current_event(ctx: Ctx<'_>) -> Result<Value<'_>> {
    let world = bindings::world(&ctx)?;
    let saved = world.borrow().current_event.clone();
    match saved {
        Some(saved) => Ok(saved.restore(&ctx)?),
        None => Ok(Value::new_undefined(ctx)),
    }
}

// https://w3c.github.io/uievents/#dom-uievent-inituievent
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

// https://webidl.spec.whatwg.org/#interface-object
pub(crate) const INSTALL_EVENT_TARGET_CTOR_DEFLATE: &[u8] = include_bytes!(concat!(
    env!("OUT_DIR"),
    "/js_blobs/event_target_ctor.deflate"
));

pub(crate) fn install_event_target_ctor_js(ctx: &Ctx<'_>) -> Result<&'static str> {
    static CACHE: std::sync::OnceLock<Box<str>> = std::sync::OnceLock::new();
    super::blob::decompress(ctx, INSTALL_EVENT_TARGET_CTOR_DEFLATE, &CACHE)
}

// https://dom.spec.whatwg.org/#interface-abortcontroller
// https://dom.spec.whatwg.org/#abortsignal
pub(crate) const INSTALL_ABORT_DEFLATE: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/js_blobs/abort.deflate"));

pub(crate) fn install_abort_js(ctx: &Ctx<'_>) -> Result<&'static str> {
    static CACHE: std::sync::OnceLock<Box<str>> = std::sync::OnceLock::new();
    super::blob::decompress(ctx, INSTALL_ABORT_DEFLATE, &CACHE)
}

// https://webidl.spec.whatwg.org/#interface-object
pub(crate) const INSTALL_EVENT_CTOR_DEFLATE: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/js_blobs/event_ctor.deflate"));

pub(crate) fn install_event_ctor_js(ctx: &Ctx<'_>) -> Result<&'static str> {
    static CACHE: std::sync::OnceLock<Box<str>> = std::sync::OnceLock::new();
    super::blob::decompress(ctx, INSTALL_EVENT_CTOR_DEFLATE, &CACHE)
}

fn register_standalone<'js>(ctx: &Ctx<'js>, id: u64, target: &Object<'js>) -> Result<()> {
    bindings::world(ctx)?
        .borrow_mut()
        .intern_standalone_target(id, Persistent::save(ctx, target.clone()));
    Ok(())
}

pub(crate) fn realm_event<'js>(ctx: &Ctx<'js>, event: JsEvent) -> Result<Class<'js, JsEvent>> {
    stamp_event(ctx, &event);
    bindings::host::instance(ctx, event)
}

// https://html.spec.whatwg.org/multipage/interaction.html#dom-click
pub(crate) fn synthetic_click_event<'js>(ctx: &Ctx<'js>) -> Result<Class<'js, JsEvent>> {
    let event = realm_event(ctx, JsEvent::uninitialized())?;
    event.borrow().initialize("click".to_owned(), true, true);
    set_synthetic_click_interface(ctx, &event)?;
    Ok(event)
}

// https://html.spec.whatwg.org/multipage/interaction.html#dom-click
fn set_synthetic_click_interface<'js>(ctx: &Ctx<'js>, event: &Class<'js, JsEvent>) -> Result<()> {
    set_exposed_event_prototype(ctx, event, "PointerEvent")?;
    let init: Function = crate::js::bridge::object(ctx)?.get("__tbInitClickPointer")?;
    init.call::<_, ()>((Class::into_value(event.clone()),))?;
    Ok(())
}

// https://dom.spec.whatwg.org/#dom-document-createevent
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

// https://dom.spec.whatwg.org/#dom-document-createevent
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

// https://dom.spec.whatwg.org/#dom-document-createevent
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

// https://dom.spec.whatwg.org/#dom-customevent-customevent
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

// https://dom.spec.whatwg.org/#dom-customevent-initcustomevent
pub(crate) fn init_custom_event<'js>(ctx: Ctx<'js>, args: Rest<Value<'js>>) -> Result<bool> {
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

pub(crate) const INSTALL_CUSTOM_EVENT_DEFLATE: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/js_blobs/custom_event.deflate"));

pub(crate) fn install_custom_event_js(ctx: &Ctx<'_>) -> Result<&'static str> {
    static CACHE: std::sync::OnceLock<Box<str>> = std::sync::OnceLock::new();
    super::blob::decompress(ctx, INSTALL_CUSTOM_EVENT_DEFLATE, &CACHE)
}

pub(crate) const INSTALL_SECURITY_POLICY_VIOLATION_EVENT_DEFLATE: &[u8] = include_bytes!(concat!(
    env!("OUT_DIR"),
    "/js_blobs/security_policy_violation_event.deflate"
));

pub(crate) fn install_security_policy_violation_event_js(ctx: &Ctx<'_>) -> Result<&'static str> {
    static CACHE: std::sync::OnceLock<Box<str>> = std::sync::OnceLock::new();
    super::blob::decompress(ctx, INSTALL_SECURITY_POLICY_VIOLATION_EVENT_DEFLATE, &CACHE)
}

// https://w3c.github.io/webappsec-csp/#securitypolicyviolationevent
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

// https://w3c.github.io/webappsec-csp/#report-violation
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
    // https://webidl.spec.whatwg.org/#idl-read-only-attributes
    // https://w3c.github.io/webappsec-csp/#strip-url-for-use-in-reports
    event.prop("documentURI", violation.document_uri.clone())?;
    event.prop("referrer", String::new())?;
    event.prop("blockedURI", violation.blocked_uri.clone())?;
    event.prop("violatedDirective", violation.effective_directive.clone())?;
    event.prop("effectiveDirective", violation.effective_directive.clone())?;
    event.prop("originalPolicy", violation.original_policy.clone())?;
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

// https://dom.spec.whatwg.org/#dom-eventtarget-addeventlistener
// https://html.spec.whatwg.org/multipage/window-object.html#windowproxy-get
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
    let world = listener_world(ctx, world, target)?;
    add_listener_parsed_in(ctx, &world, target, typ, callback, options)
}

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
    // https://dom.spec.whatwg.org/#add-an-event-listener
    let Some(callback) = callback else {
        return Ok(());
    };
    let passive = match options.passive {
        Some(passive) => passive,
        None => default_passive(ctx, &typ, target)?,
    };
    // https://dom.spec.whatwg.org/#add-an-event-listener
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

// https://dom.spec.whatwg.org/#dom-eventtarget-removeeventlistener
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
    let world = listener_world(ctx, world, target)?;
    remove_listener_parsed_in(ctx, &world, target, &typ, callback.as_ref(), capture)
}

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
    // https://dom.spec.whatwg.org/#dom-eventtarget-removeeventlistener
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

// https://dom.spec.whatwg.org/#dom-eventtarget-dispatchevent
pub(crate) fn dispatch_event<'js>(
    ctx: &Ctx<'js>,
    target: EventTargetKey,
    event: &Class<'js, JsEvent>,
) -> Result<bool> {
    dispatch_checked(ctx, target, event, false, None)
}

// https://html.spec.whatwg.org/multipage/window-object.html#windowproxy-get
pub(crate) fn dispatch_event_for_window<'js>(
    ctx: &Ctx<'js>,
    world: &Rc<RefCell<World>>,
    event: &Class<'js, JsEvent>,
) -> Result<bool> {
    dispatch_checked(ctx, EventTargetKey::Window, event, false, Some(world))
}

// https://dom.spec.whatwg.org/#concept-event-dispatch
pub(crate) fn dispatch_trusted_event<'js>(
    ctx: &Ctx<'js>,
    target: EventTargetKey,
    event: &Class<'js, JsEvent>,
) -> Result<bool> {
    dispatch_checked(ctx, target, event, true, None)
}

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

// https://drafts.csswg.org/css-animations-1/#events
// https://drafts.csswg.org/css-transitions-1/#transition-events
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
    // https://webidl.spec.whatwg.org/#idl-read-only-attributes
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

// https://html.spec.whatwg.org/multipage/popover.html#the-toggleevent-interface
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

// https://html.spec.whatwg.org/multipage/popover.html#the-toggleevent-interface
// https://html.spec.whatwg.org/multipage/interactive-elements.html#the-dialog-element:dom-dialog-show
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

// https://dom.spec.whatwg.org/#concept-event-dispatch
// https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#form-submission-algorithm
pub(crate) fn dispatch_shim_event_trusted<'js>(
    ctx: &Ctx<'js>,
    target: EventTargetKey,
    event: Value<'js>,
) -> Result<bool> {
    let event = Class::<JsEvent>::from_js(ctx, event)?;
    dispatch_checked(ctx, target, &event, true, None)
}

// https://w3c.github.io/uievents/#events-inputevents
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

// https://html.spec.whatwg.org/multipage/nav-history-apis.html#the-popstateevent-interface
pub(crate) fn fire_trusted_popstate<'js>(ctx: &Ctx<'js>, state: Value<'js>) -> Result<()> {
    let event = realm_event(ctx, JsEvent::trusted("popstate", false, false))?;
    if let Some(prototype) = bindings::world(ctx)?.borrow().brand("PopStateEvent") {
        event.set_prototype(Some(&prototype.restore(ctx)?))?;
    }
    event.set("state", state)?;
    dispatch(ctx, EventTargetKey::Window, &event, None)?;
    Ok(())
}

// https://html.spec.whatwg.org/multipage/browsing-the-web.html#the-hashchangeevent-interface
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

// https://html.spec.whatwg.org/multipage/browsing-the-web.html#the-pagetransitionevent-interface
pub(crate) fn fire_trusted_pagehide(ctx: &Ctx<'_>) -> Result<()> {
    let event = realm_event(ctx, JsEvent::trusted("pagehide", false, false))?;
    if let Some(prototype) = bindings::world(ctx)?.borrow().brand("PageTransitionEvent") {
        event.set_prototype(Some(&prototype.restore(ctx)?))?;
    }
    dispatch(ctx, EventTargetKey::Window, &event, None)?;
    Ok(())
}

// https://html.spec.whatwg.org/multipage/browsing-the-web.html#the-pagetransitionevent-interface
// https://html.spec.whatwg.org/multipage/browsing-the-web.html#unload
pub(crate) fn fire_document_unload(ctx: &Ctx<'_>) -> Result<()> {
    fire_trusted_pagehide(ctx)?;
    fire_trusted(ctx, EventTargetKey::Window, "unload", false, false)
}

// https://w3c.github.io/uievents/#events-focusevent
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

// https://dom.spec.whatwg.org/#concept-event-dispatch
pub(crate) fn fire_trusted_click(ctx: &Ctx<'_>, target: EventTargetKey) -> Result<bool> {
    let event = realm_event(ctx, JsEvent::trusted("click", true, true))?;
    // https://dom.spec.whatwg.org/#concept-event-dispatch
    // https://html.spec.whatwg.org/multipage/interaction.html#dom-click
    set_synthetic_click_interface(ctx, &event)?;
    dispatch(ctx, target, &event, None)
}

// https://dom.spec.whatwg.org/#concept-event-dispatch
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

// https://dom.spec.whatwg.org/#concept-event-dispatch
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

// https://dom.spec.whatwg.org/#concept-event-dispatch
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

// https://dom.spec.whatwg.org/#concept-event-dispatch
fn dispatch<'js>(
    ctx: &Ctx<'js>,
    target: EventTargetKey,
    event: &Class<'js, JsEvent>,
    window: Option<&Rc<RefCell<World>>>,
) -> Result<bool> {
    let composed = event.borrow().state().composed;
    let typ = event.borrow().state().typ.clone();
    let event_value = Class::into_value(event.clone());
    let related = related_target_value(ctx, &event_value)?;
    let path = build_path(ctx, target, window, composed, &typ, related.as_ref())?;
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
        // https://dom.spec.whatwg.org/#concept-event-dispatch
        state.path = path.iter().map(|item| item.reference.clone()).collect();
    }
    let bubbles = event.borrow().state().bubbles;
    // https://dom.spec.whatwg.org/#concept-event-dispatch
    // https://html.spec.whatwg.org/multipage/input.html#the-input-element:legacy-pre-activation-behavior
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
    // https://dom.spec.whatwg.org/#concept-event-dispatch
    let clear_targets = clears_targets(ctx, &path);
    let result = run_invocations(ctx, event, &path, target, bubbles, &typ);
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
    let canceled = finish_dispatch(ctx, event, &event_value, clear_targets, related.is_some())?;
    let outcome = result.and(handler);
    // https://dom.spec.whatwg.org/#concept-event-dispatch
    if let Some((node, previous)) = click_activation {
        if canceled {
            super::bindings::focus::legacy_canceled_activation(ctx, node, &previous)?;
        } else if let EventTargetKey::Node(event_target) = target {
            super::bindings::focus::complete_activation(ctx, node, &previous, event_target)?;
        }
    }
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
    for item in path {
        let phase = if item.reference.key == target {
            AT_TARGET
        } else if bubbles {
            BUBBLING_PHASE
        } else {
            continue;
        };
        // https://github.com/whatwg/html/issues/3599
        // https://github.com/whatwg/dom/issues/756
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

// https://dom.spec.whatwg.org/#concept-event-listener-invoke
fn legacy_event_type(typ: &str) -> Option<&'static str> {
    match typ {
        "animationend" => Some("webkitAnimationEnd"),
        "animationiteration" => Some("webkitAnimationIteration"),
        "animationstart" => Some("webkitAnimationStart"),
        "transitionend" => Some("webkitTransitionEnd"),
        _ => None,
    }
}

// https://dom.spec.whatwg.org/#concept-event-listener-invoke
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

// https://dom.spec.whatwg.org/#event-path-item
struct PathItem<'js> {
    reference: EventTargetRef,
    target: Value<'js>,
    // https://dom.spec.whatwg.org/#concept-event-dispatch
    shadow_adjusted_target: Option<EventTargetRef>,
    // https://dom.spec.whatwg.org/#concept-event-listener-invoke
    event_target: EventTargetRef,
    // https://dom.spec.whatwg.org/#concept-event-dispatch
    related_target: Option<Value<'js>>,
    // https://dom.spec.whatwg.org/#concept-event-path-append
    invocation_target_in_shadow_tree: bool,
}

// https://dom.spec.whatwg.org/#concept-event-relatedtarget
fn related_target_value<'js>(ctx: &Ctx<'js>, event: &Value<'js>) -> Result<Option<Value<'js>>> {
    let bridge = crate::js::bridge::object(ctx)?;
    let getter: Function = bridge.get("__tbRelatedTargetGet")?;
    let value: Value = getter.call((event.clone(),))?;
    Ok((!value.is_null() && !value.is_undefined()).then_some(value))
}

// https://dom.spec.whatwg.org/#concept-event-listener-invoke
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

// https://dom.spec.whatwg.org/#concept-shadow-including-inclusive-ancestor
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

// https://dom.spec.whatwg.org/#retarget
fn retarget_related<'js>(
    ctx: &Ctx<'js>,
    value: &Value<'js>,
    against: &EventTargetRef,
) -> Result<Value<'js>> {
    let Some(mut id) = bindings::host_node_id(ctx, value) else {
        return Ok(value.clone());
    };
    // https://dom.spec.whatwg.org/#retarget
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

// https://dom.spec.whatwg.org/#concept-tree-root
fn value_is_in_shadow_tree<'js>(ctx: &Ctx<'js>, value: &Value<'js>) -> bool {
    let Some(id) = bindings::host_node_id(ctx, value) else {
        return false;
    };
    let Some(world) = bindings::owning_world(ctx, id) else {
        return false;
    };
    node_root_is_shadow(&world, id)
}

// https://dom.spec.whatwg.org/#concept-tree-root
fn reference_is_in_shadow_tree(reference: &EventTargetRef) -> bool {
    let EventTargetKey::Node(id) = reference.key else {
        return false;
    };
    node_root_is_shadow(&reference.world, id)
}

// https://dom.spec.whatwg.org/#concept-tree-root
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

// https://dom.spec.whatwg.org/#concept-event-target
fn target_reference(
    ctx: &Ctx<'_>,
    target: EventTargetKey,
    window: Option<&Rc<RefCell<World>>>,
) -> Result<EventTargetRef> {
    let world = match target {
        EventTargetKey::Window => match window {
            Some(world) => frame_main_world(ctx, Rc::clone(world))?,
            None => frame_main_world(ctx, bindings::world(ctx)?)?,
        },
        _ => target_world(ctx, target)?,
    };
    Ok(EventTargetRef { key: target, world })
}

// https://dom.spec.whatwg.org/#concept-tree-root
fn plain_root(world: &Rc<RefCell<World>>, mut id: NodeId) -> NodeId {
    while let Some(parent) = world.borrow().node_parent(id) {
        id = parent;
    }
    id
}

// https://dom.spec.whatwg.org/#concept-event-path-append
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

// https://dom.spec.whatwg.org/#concept-event-dispatch
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

// https://dom.spec.whatwg.org/#concept-event-path-append
// https://dom.spec.whatwg.org/#concept-event-dispatch
fn build_path<'js>(
    ctx: &Ctx<'js>,
    target: EventTargetKey,
    window: Option<&Rc<RefCell<World>>>,
    composed: bool,
    typ: &str,
    related: Option<&Value<'js>>,
) -> Result<Vec<PathItem<'js>>> {
    let reference = target_reference(ctx, target, window)?;
    let related_against_target = match related {
        Some(value) => Some(retarget_related(ctx, value, &reference)?),
        None => None,
    };
    // https://dom.spec.whatwg.org/#concept-event-dispatch
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
            if let EventTargetKey::Standalone(id) = target
                && reference.world.borrow().standalone_target(id).is_none()
            {
                return Err(Exception::throw_internal(ctx, "missing EventTarget"));
            }
            single_target_path(ctx, reference, related_against_target)
        }
        EventTargetKey::Node(id) => {
            let world = Rc::clone(&reference.world);
            // https://dom.spec.whatwg.org/#concept-event-dispatch
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
            // https://dom.spec.whatwg.org/#get-the-parent
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
                    // https://dom.spec.whatwg.org/#concept-event-dispatch
                    break;
                };
                let invocation_target_in_shadow_tree = item.invocation_target_in_shadow_tree;
                path.push(item);
                let parent = world.borrow().node_parent(node);
                if let Some(parent) = parent {
                    cursor = Some(parent);
                } else {
                    reached_document = world.borrow().node_is_document(node);
                    // https://dom.spec.whatwg.org/#get-the-parent
                    cursor = if composed && invocation_target_in_shadow_tree {
                        world.borrow().shadow_root_host(node)
                    } else {
                        None
                    };
                }
            }
            // https://dom.spec.whatwg.org/#get-the-parent
            if reached_document && typ != "load" && world.borrow().has_browsing_context() {
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

// https://dom.spec.whatwg.org/#concept-event-listener-inner-invoke
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

// https://dom.spec.whatwg.org/#concept-event-listener-inner-invoke
fn target_is_fully_active(ctx: &Ctx<'_>, reference: &EventTargetRef) -> bool {
    let Some(world) = target_global_world(ctx, reference) else {
        return false;
    };
    let world = world.borrow();
    world.kind != GlobalKind::Document || world.has_browsing_context()
}

// https://dom.spec.whatwg.org/#concept-event-listener-invoke
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
        // https://dom.spec.whatwg.org/#concept-event-listener-invoke
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
        // https://dom.spec.whatwg.org/#concept-event-listener-inner-invoke
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
        // https://dom.spec.whatwg.org/#concept-event-listener-inner-invoke
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
            // https://html.spec.whatwg.org/multipage/webappapis.html#the-event-handler-processing-algorithm
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

// https://dom.spec.whatwg.org/#concept-event-listener-inner-invoke
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
    if let Err(error) = outcome {
        report_exception(&realm, &error);
    }
    world.borrow_mut().current_event = previous;
}

fn listener_realm<'js>(callback: &Value<'js>) -> Ctx<'js> {
    callback
        .as_function()
        .and_then(|function| function.realm().ok())
        .unwrap_or_else(|| callback.ctx().clone())
}

// https://webidl.spec.whatwg.org/#call-a-user-objects-operation
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

// https://html.spec.whatwg.org/multipage/webappapis.html#report-an-exception
pub(super) fn report_exception(ctx: &Ctx<'_>, error: &rquickjs::Error) {
    if error.is_exception() {
        let caught = ctx.catch();
        super::bindings::report_exception_value(ctx, caught, 0, "");
    }
}

// https://webidl.spec.whatwg.org/#es-callback-interface
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

// https://dom.spec.whatwg.org/#dictdef-addeventlisteneroptions
pub(crate) struct ListenerOptions {
    capture: bool,
    once: bool,
    // https://dom.spec.whatwg.org/#default-passive-value
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
            // https://dom.spec.whatwg.org/#dictdef-addeventlisteneroptions
            // https://webidl.spec.whatwg.org/#es-interface
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
        // https://dom.spec.whatwg.org/#dom-eventtarget-removeeventlistener
        match listener_options_argument(ctx, options)? {
            ListenerOptionsArgument::Absent => Ok(false),
            ListenerOptionsArgument::Boolean(capture) => Ok(capture),
            ListenerOptionsArgument::Object(object) => {
                bindings::option_truthy(ctx, &object, "capture")
            }
        }
    }
}

// https://dom.spec.whatwg.org/#dictdef-eventlisteneroptions
enum ListenerOptionsArgument<'js> {
    Absent,
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

// https://dom.spec.whatwg.org/#default-passive-value
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

// https://dom.spec.whatwg.org/#abortsignal-aborted
fn signal_aborted(ctx: &Ctx<'_>, signal: &Persistent<Object<'static>>) -> bool {
    signal
        .clone()
        .restore(ctx)
        .ok()
        .and_then(|object| object.get::<_, Value>("aborted").ok())
        .is_some_and(|value| bindings::to_boolean(ctx, &value).unwrap_or(false))
}

fn target_world(ctx: &Ctx<'_>, target: EventTargetKey) -> Result<Rc<RefCell<World>>> {
    match target {
        EventTargetKey::Node(id) => bindings::world_for_node(ctx, id),
        EventTargetKey::Attribute { scope, id } => {
            let state = bindings::attr_state(ctx, scope, id)?;
            bindings::world_for_node(ctx, state.document)
        }
        EventTargetKey::Window => frame_main_world(ctx, bindings::world(ctx)?),
        EventTargetKey::Standalone(_) => bindings::world(ctx),
    }
}

// https://dom.spec.whatwg.org/#concept-event-target
// https://html.spec.whatwg.org/multipage/window-object.html#window
fn frame_main_world(ctx: &Ctx<'_>, world: Rc<RefCell<World>>) -> Result<Rc<RefCell<World>>> {
    let frame = world.borrow().frame();
    let main = bindings::realm_registry(ctx)?.borrow().frame_world(frame);
    Ok(main.unwrap_or(world))
}

// https://dom.spec.whatwg.org/#concept-event-target
// https://html.spec.whatwg.org/multipage/window-object.html#window
fn listener_world(
    ctx: &Ctx<'_>,
    world: &Rc<RefCell<World>>,
    target: EventTargetKey,
) -> Result<Rc<RefCell<World>>> {
    match target {
        EventTargetKey::Window => frame_main_world(ctx, Rc::clone(world)),
        _ => Ok(Rc::clone(world)),
    }
}

fn stored_target<'js>(ctx: &Ctx<'js>, reference: Option<&EventTargetRef>) -> Result<Value<'js>> {
    match reference {
        Some(reference) => resolve_target(ctx, reference),
        None => Ok(Value::new_null(ctx.clone())),
    }
}

// https://dom.spec.whatwg.org/#concept-event-target
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

// https://w3c.github.io/webappsec-csp/#should-block-inline
// https://w3c.github.io/webappsec-csp/#report-violation
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

// https://w3c.github.io/webappsec-csp/#should-block-inline
// https://w3c.github.io/webappsec-csp/#create-violation-for-global
pub(crate) fn handler_attribute_allowed(
    ctx: &Ctx<'_>,
    id: NodeId,
    source: &str,
    position: Option<(&str, u32, u32)>,
) -> bool {
    // https://w3c.github.io/webappsec-csp/#should-block-inline
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
        // https://w3c.github.io/webappsec-csp/#report-violation
        let repeated = owner
            .borrow_mut()
            .state
            .reported_handlers
            .insert((id, source.to_owned()), source.to_owned())
            .is_some_and(|previous| previous == source);
        if !repeated {
            // https://w3c.github.io/webappsec-csp/#report-violation
            owner
                .borrow_mut()
                .queue_csp_violations(Some(id), violations.clone());
        }
    }
    !crate::csp::blocks(&violations)
}

// https://drafts.csswg.org/css-animations-1/#extensions-to-the-globaleventhandlers-mixin
// https://drafts.csswg.org/css-transitions-1/#extensions-to-the-globaleventhandlers-mixin
const PREFIXED_HANDLERS: [(&str, &str); 4] = [
    ("onwebkitanimationend", "webkitAnimationEnd"),
    ("onwebkitanimationiteration", "webkitAnimationIteration"),
    ("onwebkitanimationstart", "webkitAnimationStart"),
    ("onwebkittransitionend", "webkitTransitionEnd"),
];

// https://html.spec.whatwg.org/multipage/webappapis.html#event-handlers-on-elements
fn handler_attribute_name(typ: &str) -> String {
    PREFIXED_HANDLERS
        .iter()
        .find(|(_, event_type)| *event_type == typ)
        .map_or_else(|| format!("on{typ}"), |(name, _)| (*name).to_owned())
}

// https://html.spec.whatwg.org/multipage/webappapis.html#event-handler-content-attributes
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
        // https://dom.spec.whatwg.org/#concept-event-listener-inner-invoke
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

// https://html.spec.whatwg.org/multipage/webappapis.html#event-handlers
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
    // https://html.spec.whatwg.org/multipage/browsers.html#sandboxed-scripts-browsing-context-flag
    if bindings::world(ctx)?.borrow().sandbox.scripts_disabled() {
        return Ok(());
    }
    let name = handler_attribute_name(typ);
    let handler: Value = object.get(name.as_str())?;
    if let Some(function) = handler.as_function() {
        // https://html.spec.whatwg.org/multipage/webappapis.html#the-event-handler-processing-algorithm
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

// https://html.spec.whatwg.org/multipage/webappapis.html#event-handler-map
#[derive(Clone, Copy)]
enum HandlerStorage {
    Window,
    Node(NodeId),
    PlainProperty,
}

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
    fn map_target(self) -> Option<NodeId> {
        match self {
            Self::Node(id) => Some(id),
            Self::Window | Self::PlainProperty => None,
        }
    }
}

// https://html.spec.whatwg.org/multipage/webappapis.html#getting-the-current-value-of-the-event-handler
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
            // https://html.spec.whatwg.org/multipage/webappapis.html#getting-the-current-value-of-the-event-handler
            // https://html.spec.whatwg.org/multipage/browsers.html#sandboxed-scripts-browsing-context-flag
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
                    // https://html.spec.whatwg.org/multipage/webappapis.html#getting-the-current-value-of-the-event-handler
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

// https://html.spec.whatwg.org/multipage/webappapis.html#getting-the-current-value-of-the-event-handler
fn handler_function_source(name: &str, is_window: bool, body: &str) -> String {
    if name == "onerror" && is_window {
        format!("function {name}(event, source, lineno, colno, error) {{\n{body}\n}}")
    } else {
        format!("function {name}(event) {{\n{body}\n}}")
    }
}

// https://html.spec.whatwg.org/multipage/webappapis.html#event-handler-content-attributes
fn validate_handler_body(ctx: &Ctx<'_>, body: &str) -> Result<()> {
    let bridge = super::bridge::object(ctx)?;
    let constructor: Object = bridge.get("__tb_function")?;
    let Some(constructor) = constructor.as_constructor() else {
        return Err(Exception::throw_internal(
            ctx,
            "Function constructor is not constructible",
        ));
    };
    let _: Function = constructor.construct((body.to_owned(),))?;
    Ok(())
}

// https://html.spec.whatwg.org/multipage/webappapis.html#getting-the-current-value-of-the-event-handler
fn compile_handler_text<'js>(
    ctx: &Ctx<'js>,
    name: &str,
    is_window: bool,
    body: &str,
) -> Result<Function<'js>> {
    let world = bindings::world(ctx)?;
    // https://w3c.github.io/webappsec-csp/#can-compile-strings
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

fn handler_event_type(name: &str) -> String {
    PREFIXED_HANDLERS
        .iter()
        .find(|(handler, _)| *handler == name)
        .map_or_else(
            || name.strip_prefix("on").unwrap_or(name).to_owned(),
            |(_, event_type)| (*event_type).to_owned(),
        )
}

// https://html.spec.whatwg.org/multipage/webappapis.html#the-event-handler-processing-algorithm
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

// https://html.spec.whatwg.org/multipage/webappapis.html#activate-an-event-handler
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

// https://html.spec.whatwg.org/multipage/webappapis.html#deactivate-an-event-handler
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

// https://dom.spec.whatwg.org/#concept-node-adopt
pub(crate) fn transfer_node_handlers(
    from_world: &Rc<RefCell<World>>,
    from: NodeId,
    to_world: &Rc<RefCell<World>>,
    to: NodeId,
) {
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

// https://html.spec.whatwg.org/multipage/webappapis.html#event-handlers
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

// https://html.spec.whatwg.org/multipage/webappapis.html#the-event-handler-processing-algorithm
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
    // https://webidl.spec.whatwg.org/#LegacyTreatNonObjectAsNull
    // https://html.spec.whatwg.org/multipage/webappapis.html#the-event-handler-processing-algorithm
    let Some(function) = callback.as_function() else {
        return;
    };
    let typ = event.borrow().state().typ.clone();
    let event_value = Class::into_value(event.clone());
    // https://html.spec.whatwg.org/multipage/webappapis.html#the-event-handler-processing-algorithm
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

// https://html.spec.whatwg.org/multipage/webappapis.html#the-event-handler-processing-algorithm
fn is_before_unload_event<'js>(ctx: &Ctx<'js>, event: &Value<'js>) -> bool {
    is_event_of(ctx, event, "BeforeUnloadEvent")
}

// https://html.spec.whatwg.org/multipage/webappapis.html#dom-createevent
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

// https://html.spec.whatwg.org/multipage/webappapis.html#the-event-handler-processing-algorithm
fn process_handler_return<'js>(
    ctx: &Ctx<'js>,
    event: &Class<'js, JsEvent>,
    event_value: &Value<'js>,
    name: &str,
    typ: &str,
    special_error: bool,
    return_value: &Value<'js>,
) -> Result<()> {
    // https://html.spec.whatwg.org/multipage/webappapis.html#onbeforeunloadeventhandler
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
        // https://html.spec.whatwg.org/multipage/webappapis.html#the-event-handler-processing-algorithm
        false
    } else {
        return_value.as_bool() == Some(false)
    };
    if canceled {
        set_event_canceled(event);
    }
    Ok(())
}

// https://dom.spec.whatwg.org/#set-the-canceled-flag
fn set_event_canceled(event: &Class<'_, JsEvent>) {
    let class = event.borrow();
    let mut state = class.state_mut();
    if state.cancelable && !state.in_passive {
        state.canceled = true;
    }
}

// https://html.spec.whatwg.org/multipage/webappapis.html#the-event-handler-processing-algorithm
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

// https://webidl.spec.whatwg.org/#es-boolean
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
