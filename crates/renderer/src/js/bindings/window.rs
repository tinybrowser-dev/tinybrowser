//! Window-level listeners, location, and load events.

use super::{events, main_document, world};
use rquickjs::function::{Opt, This};
use rquickjs::object::Accessor;

use std::cell::RefCell;

use std::rc::Rc;

use crate::dom::tree::NodeId;

use rquickjs::{Class, Ctx, Exception, Object, Persistent, Result, Value};

use crate::js::events::JsEvent;
use crate::js::world::{EventTargetKey, World};
use crate::protocol::FrameId;

fn world_for_window_this<'js>(ctx: &Ctx<'js>, this: &Object<'js>) -> Result<Rc<RefCell<World>>> {
    let current = world(ctx)?;
    if this.as_value() == ctx.globals().as_value() {
        return Ok(current);
    }
    let slots: rquickjs::Function = crate::js::bridge::object(ctx)?.get("slots")?;
    let data: Object = slots.call(("tinybrowser.window.data",))?;
    let get: rquickjs::Function = data.get("get")?;
    let value: Value = get.call((this.clone(),))?;
    let Some(data) = value.as_object() else {
        return Ok(current);
    };
    let Ok(frame) = data.get::<_, f64>("frame") else {
        return Ok(current);
    };
    let Some(target) =
        crate::js::bridge::frame_id(frame).and_then(|frame| current.borrow().frame_world(frame))
    else {
        return Ok(current);
    };
    // https://html.spec.whatwg.org/multipage/window-object.html#windowproxy-get
    let same_origin = target.borrow().origin == current.borrow().origin;
    if same_origin { Ok(target) } else { Ok(current) }
}

/// The URL components `Location` exposes.
#[derive(Clone, Copy)]
enum Component {
    Pathname,
    Href,
    Search,
    Hash,
    Origin,
    Protocol,
    Host,
    Hostname,
    Port,
}

impl Component {
    /// The component's value with the shape the IDL getters return.
    fn read(self, world: &World) -> String {
        let url = &world.document_url;
        match self {
            Self::Pathname => url.path().to_owned(),
            Self::Href => url.as_str().to_owned(),
            Self::Search => url
                .query()
                .filter(|query| !query.is_empty())
                .map_or_else(String::new, |query| format!("?{query}")),
            Self::Hash => url
                .fragment()
                .filter(|fragment| !fragment.is_empty())
                .map_or_else(String::new, |fragment| format!("#{fragment}")),
            // The settings object's origin, not the URL's: `about:blank`,
            // `about:srcdoc`, and `data:` documents carry a captured origin
            // (<https://html.spec.whatwg.org/multipage/browsers.html#concept-origin>).
            Self::Origin => world.origin.ascii_serialization(),
            Self::Protocol => format!("{}:", url.scheme()),
            Self::Host => match url.port() {
                Some(port) => format!("{}:{port}", url.host_str().unwrap_or_default()),
                None => url.host_str().unwrap_or_default().to_owned(),
            },
            Self::Hostname => url.host_str().unwrap_or_default().to_owned(),
            Self::Port => url.port().map_or_else(String::new, |port| port.to_string()),
        }
    }

    /// Replaces this component of `url` with `value`, using the URL Standard's
    /// quirks setters, which are the algorithms the IDL setters name
    /// (<https://url.spec.whatwg.org/#example-635b7a2a>). Returns whether the
    /// URL changed, so assigning the value the document already has does not
    /// reload it
    /// (<https://html.spec.whatwg.org/multipage/nav-history-apis.html#the-location-interface>).
    fn replace(self, url: &mut url::Url, value: &str) -> bool {
        let before = url.as_str().to_owned();
        match self {
            Self::Pathname => url::quirks::set_pathname(url, value),
            Self::Search => url::quirks::set_search(url, value),
            Self::Protocol => {
                let _ = url::quirks::set_protocol(url, value);
            }
            Self::Host => {
                let _ = url::quirks::set_host(url, value);
            }
            Self::Hostname => {
                let _ = url::quirks::set_hostname(url, value);
            }
            Self::Port => {
                let _ = url::quirks::set_port(url, value);
            }
            // `href` names a whole URL; `origin` is read-only. `hash` needs
            // "navigate to a fragment", which this engine has no algorithm for,
            // and a full reload would be the wrong answer.
            Self::Href | Self::Hash | Self::Origin => return false,
        }
        url.as_str() != before
    }
}

/// Queues a navigation of `owner`'s frame to `spec`, which is what assigning
/// to `Location.href`, to any other navigating `Location` component, and to a
/// window's `location` does
/// (<https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-location-href>).
fn navigate_location(owner: &Rc<RefCell<World>>, spec: String, source: Option<FrameId>) {
    let mut navigation = crate::js::world::FrameNavigation::get(
        crate::js::world::NavigationTarget::Target(crate::protocol::TargetName::Current),
        spec,
    );
    if let Some(source) = source {
        navigation = navigation.with_source(source);
    }
    owner.borrow_mut().queue_frame_navigation(navigation);
}

/// Parses `value` the way the `href` setter does, relative to the entry
/// settings object's API base URL, and throws the `SyntaxError` the setter
/// requires when it is not a valid URL
/// (<https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-location-href>).
///
/// The parsed absolute URL is what the navigation uses, so a `<base>` in the
/// target document cannot re-resolve a relative assignment against a different
/// base than the one that was checked.
fn parse_location(ctx: &Ctx<'_>, caller: &Rc<RefCell<World>>, value: &str) -> Result<url::Url> {
    let caller = caller.borrow();
    let base = caller
        .base_url
        .clone()
        .unwrap_or_else(|| caller.document_url.clone());
    base.join(value)
        .map_err(|_| super::throw_dom(ctx, "SyntaxError", "invalid URL"))
}

/// The `Location` setter shared by every component that navigates.
///
/// `href` names a whole URL; every other component replaces itself in the
/// current URL and navigates only when that changed something.
fn set_component(owner: &Rc<RefCell<World>>, component: Component, value: Value<'_>) -> Result<()> {
    let ctx = value.ctx().clone();
    let value = super::webidl::webidl_to_string(&ctx, value)?;
    // The entry settings object supplies the resolution base; the incumbent
    // document supplies the referrer, policy, and sandbox rules when a script
    // in one frame assigns another frame's `location`
    // (<https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-location-href>).
    let caller = world(&ctx)?;
    let source = caller.borrow().frame();
    if matches!(component, Component::Href) {
        let url = parse_location(&ctx, &caller, &value)?;
        navigate_location(owner, url.as_str().to_owned(), Some(source));
        return Ok(());
    }
    let mut url = owner.borrow().document_url.clone();
    if component.replace(&mut url, &value) {
        navigate_location(owner, url.as_str().to_owned(), Some(source));
    }
    Ok(())
}

/// Installs the `Location` object
/// (<https://html.spec.whatwg.org/multipage/nav-history-apis.html#the-location-interface>).
///
/// Empty components return empty strings, as specified by
/// <https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-location-search>
/// and <https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-location-hash>.
///
/// Assigning to a window's `location` navigates, like `Location.href`. The
/// `hash` setter stays read-only: its same-document branch needs "navigate to
/// a fragment", which this engine has no algorithm for, and a full reload
/// would be the wrong answer.
pub(crate) fn install_location<'js>(
    ctx: &Ctx<'js>,
    globals: &Object<'js>,
    world: &Rc<RefCell<World>>,
) -> Result<()> {
    let location = build_location(ctx, world)?;
    world.borrow_mut().location_object = Some(Persistent::save(ctx, location));
    let getter_owner = Rc::clone(world);
    let setter_owner = Rc::clone(world);
    globals.prop(
        "location",
        Accessor::new(
            move |ctx: Ctx<'js>| -> Result<Value<'js>> {
                let saved = getter_owner.borrow().location_object.clone();
                let Some(saved) = saved else {
                    return Err(Exception::throw_internal(&ctx, "location is gone"));
                };
                Ok(saved.restore(&ctx)?.into_value())
            },
            move |value: Value<'js>| set_component(&setter_owner, Component::Href, value),
        )
        .enumerable(),
    )?;
    Ok(())
}

/// The `Location` object for `world`, built in the calling realm's context but
/// bound to `world`, so its setters navigate that world's frame. `install_location`
/// stores it on the world; a child window whose realm has not started yet gets
/// one through its `WindowProxy` instead
/// (<https://html.spec.whatwg.org/multipage/nav-history-apis.html#the-location-interface>).
pub(crate) fn build_location<'js>(
    ctx: &Ctx<'js>,
    world: &Rc<RefCell<World>>,
) -> Result<Object<'js>> {
    let components = [
        ("pathname", Component::Pathname),
        ("href", Component::Href),
        ("search", Component::Search),
        ("hash", Component::Hash),
        ("origin", Component::Origin),
        ("protocol", Component::Protocol),
        ("host", Component::Host),
        ("hostname", Component::Hostname),
        ("port", Component::Port),
    ];
    let location = Object::new(ctx.clone())?;
    for (name, component) in components {
        let owner = Rc::clone(world);
        let getter = Accessor::new_get(move || component.read(&owner.borrow()));
        if matches!(component, Component::Hash | Component::Origin) {
            location.prop(name, getter.enumerable())?;
            continue;
        }
        let owner = Rc::clone(world);
        location.prop(
            name,
            getter
                .set(move |value: Value<'js>| set_component(&owner, component, value))
                .enumerable(),
        )?;
    }
    Ok(location)
}

/// The `Window.location` setter for a frame whose realm is not running: it
/// throws the same `SyntaxError` and queues the same navigation as the
/// accessor `install_location` installs.
pub(crate) fn set_location_href(owner: &Rc<RefCell<World>>, value: Value<'_>) -> Result<()> {
    set_component(owner, Component::Href, value)
}

/// The window a `Window` method call targets, resolving a sloppy bare call.
///
/// A host function invoked as a bare global (`addEventListener(type, fn)`)
/// receives `undefined` for `this`: host functions do not get the sloppy-mode
/// global substitution a page function does. A missing receiver means the
/// current realm's window; any other non-object receiver is an illegal
/// invocation
/// (<https://webidl.spec.whatwg.org/#es-operations>).
fn window_world_for_call<'js>(ctx: &Ctx<'js>, this: &Value<'js>) -> Result<Rc<RefCell<World>>> {
    if this.is_undefined() || this.is_null() {
        return world(ctx);
    }
    let Some(object) = this.as_object() else {
        return Err(Exception::throw_type(ctx, "Illegal invocation"));
    };
    world_for_window_this(ctx, object)
}

pub(crate) fn window_add_event_listener<'js>(
    ctx: Ctx<'js>,
    this: This<Value<'js>>,
    typ: Value<'js>,
    callback: Value<'js>,
    options: Opt<Value<'js>>,
) -> Result<()> {
    let world = window_world_for_call(&ctx, &this.0)?;
    events::add_listener_in(
        &ctx,
        &world,
        EventTargetKey::Window,
        typ,
        callback,
        options.0,
    )
}

pub(crate) fn window_remove_event_listener<'js>(
    ctx: Ctx<'js>,
    this: This<Value<'js>>,
    typ: Value<'js>,
    callback: Value<'js>,
    options: Opt<Value<'js>>,
) -> Result<()> {
    let world = window_world_for_call(&ctx, &this.0)?;
    events::remove_listener_in(
        &ctx,
        &world,
        EventTargetKey::Window,
        typ,
        callback,
        options.0,
    )
}

pub(crate) fn window_dispatch_event<'js>(
    ctx: Ctx<'js>,
    this: This<Value<'js>>,
    event: Class<'js, JsEvent>,
) -> Result<bool> {
    let world = window_world_for_call(&ctx, &this.0)?;
    events::dispatch_event_for_window(&ctx, &world, &event)
}

pub(crate) fn window_dispatch_trusted_event<'js>(
    ctx: Ctx<'js>,
    event: Class<'js, JsEvent>,
) -> Result<bool> {
    events::dispatch_trusted_event(&ctx, EventTargetKey::Window, &event)
}

pub(crate) fn fire_dom_content_loaded(ctx: &Ctx<'_>) -> Result<()> {
    let document = main_document(ctx)?;
    events::fire_trusted(
        ctx,
        EventTargetKey::Node(document),
        "DOMContentLoaded",
        true,
        false,
    )?;
    Ok(())
}

/// Fires `readystatechange` at the document after its readiness changed
/// (<https://html.spec.whatwg.org/multipage/dom.html#current-document-readiness>).
pub(crate) fn fire_ready_state_change(ctx: &Ctx<'_>) -> Result<()> {
    let document = main_document(ctx)?;
    events::fire_trusted(
        ctx,
        EventTargetKey::Node(document),
        "readystatechange",
        false,
        false,
    )?;
    Ok(())
}

pub(crate) fn fire_window_load(ctx: &Ctx<'_>) -> Result<()> {
    events::fire_trusted(ctx, EventTargetKey::Window, "load", false, false)
}

pub(crate) fn fire_node_load(ctx: &Ctx<'_>, id: NodeId) -> Result<()> {
    events::fire_trusted(ctx, EventTargetKey::Node(id), "load", false, false)
}

pub(crate) fn fire_node_error(ctx: &Ctx<'_>, id: NodeId) -> Result<()> {
    events::fire_trusted(ctx, EventTargetKey::Node(id), "error", false, false)
}

/// Dispatches one trusted `securitypolicyviolation` event per blocked policy
/// at `target` (<https://w3c.github.io/webappsec-csp/#report-violation>).
pub(crate) fn fire_csp_violations(
    ctx: &Ctx<'_>,
    target: NodeId,
    violations: &[crate::csp::Violation],
) -> Result<()> {
    for violation in violations {
        events::fire_security_policy_violation(ctx, EventTargetKey::Node(target), violation)?;
    }
    Ok(())
}
