use super::{events, main_document, world};
use rquickjs::function::{Opt, This};
use rquickjs::object::Accessor;

use std::cell::RefCell;

use std::rc::Rc;

use crate::dom::tree::NodeId;

use rquickjs::{Class, Ctx, Exception, Function, Object, Persistent, Result, Value};

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
    let same_origin = target.borrow().same_origin_domain_with(&current.borrow());
    if same_origin { Ok(target) } else { Ok(current) }
}

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
            // https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-location-origin
            Self::Origin => url.origin().ascii_serialization(),
            Self::Protocol => format!("{}:", url.scheme()),
            Self::Host => match url.port() {
                Some(port) => format!("{}:{port}", url.host_str().unwrap_or_default()),
                None => url.host_str().unwrap_or_default().to_owned(),
            },
            Self::Hostname => url.host_str().unwrap_or_default().to_owned(),
            Self::Port => url.port().map_or_else(String::new, |port| port.to_string()),
        }
    }

    // https://url.spec.whatwg.org/#example-635b7a2a
    // https://html.spec.whatwg.org/multipage/nav-history-apis.html#the-location-interface
    fn replace(self, url: &mut url::Url, value: &str) -> std::result::Result<bool, ()> {
        let before = url.as_str().to_owned();
        match self {
            Self::Pathname => url::quirks::set_pathname(url, value),
            Self::Search => url::quirks::set_search(url, value),
            Self::Protocol => {
                // https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-location-protocol
                let Some(scheme) = protocol_scheme(value) else {
                    return Err(());
                };
                let _ = url.set_scheme(&scheme);
                if !matches!(url.scheme(), "http" | "https") {
                    return Ok(false);
                }
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
            // https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-location-hash
            Self::Hash => {
                let input = value.strip_prefix('#').unwrap_or(value);
                let previous = url.fragment().unwrap_or_default().to_owned();
                url.set_fragment(Some(input));
                if url.fragment().unwrap_or_default() == previous {
                    return Ok(false);
                }
            }
            Self::Href | Self::Origin => return Ok(false),
        }
        Ok(url.as_str() != before)
    }
}

// https://url.spec.whatwg.org/#scheme-start-state
// https://url.spec.whatwg.org/#scheme-state
fn protocol_scheme(value: &str) -> Option<String> {
    let mut scheme = String::with_capacity(value.len());
    for c in value.chars() {
        if matches!(c, '\t' | '\n' | '\r') {
            continue;
        }
        if c == ':' {
            break;
        }
        scheme.push(c);
    }
    let mut chars = scheme.chars();
    let first = chars.next()?;
    if !first.is_ascii_alphabetic() {
        return None;
    }
    let mut normalized = String::with_capacity(scheme.len());
    normalized.push(first.to_ascii_lowercase());
    for c in chars {
        if !(c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')) {
            return None;
        }
        normalized.push(c.to_ascii_lowercase());
    }
    Some(normalized)
}

// https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-location-href
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

// https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-location-href
fn parse_location(ctx: &Ctx<'_>, caller: &Rc<RefCell<World>>, value: &str) -> Result<url::Url> {
    let caller = caller.borrow();
    let base = caller
        .base_url
        .clone()
        .unwrap_or_else(|| caller.document_url.clone());
    base.join(value)
        .map_err(|_| super::throw_dom(ctx, "SyntaxError", "invalid URL"))
}

fn set_component(owner: &Rc<RefCell<World>>, component: Component, value: Value<'_>) -> Result<()> {
    let ctx = value.ctx().clone();
    // https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-location-href
    let value = super::webidl::webidl_to_string(&ctx, value)?;
    if !owner.borrow().has_browsing_context() {
        return Ok(());
    }
    // https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-location-href
    let caller = world(&ctx)?;
    let source = caller.borrow().frame();
    if matches!(component, Component::Href) {
        let url = parse_location(&ctx, &caller, &value)?;
        if only_fragment_changed(owner, &url) {
            navigate_to_fragment(&ctx, &caller, owner, &url, false);
        } else {
            navigate_location(owner, url.as_str().to_owned(), Some(source));
        }
        return Ok(());
    }
    let mut url = owner.borrow().document_url.clone();
    let changed = component
        .replace(&mut url, &value)
        .map_err(|()| super::throw_dom(&ctx, "SyntaxError", "invalid scheme"))?;
    if changed {
        if only_fragment_changed(owner, &url) {
            navigate_to_fragment(&ctx, &caller, owner, &url, false);
        } else {
            navigate_location(owner, url.as_str().to_owned(), Some(source));
        }
    }
    Ok(())
}

// https://html.spec.whatwg.org/multipage/browsing-the-web.html#beginning-navigation
fn only_fragment_changed(owner: &Rc<RefCell<World>>, url: &url::Url) -> bool {
    url.fragment().is_some() && {
        let world = owner.borrow();
        crate::js::world::same_document_url(&world.document_url, url)
    }
}

// https://html.spec.whatwg.org/multipage/browsing-the-web.html#navigate-to-a-fragment
fn navigate_to_fragment(
    ctx: &Ctx<'_>,
    caller: &Rc<RefCell<World>>,
    owner: &Rc<RefCell<World>>,
    url: &url::Url,
    force_replace: bool,
) {
    let (old_url, handling) = {
        let mut world = owner.borrow_mut();
        let old_url = world.document_url.as_str().to_owned();
        let handling = if force_replace || world.document_url == *url {
            crate::protocol::HistoryHandling::Replace
        } else {
            crate::protocol::HistoryHandling::Push
        };
        world.document_url = url.clone();
        world.history.apply("null".to_owned(), handling);
        (old_url, handling)
    };
    sync_fragment_state(ctx, caller, owner);
    owner.borrow_mut().outbox.borrow_mut().push_back(
        crate::js::world::RealmRequest::FragmentNavigation {
            old_url,
            url: url.clone(),
            handling,
        },
    );
}

// https://html.spec.whatwg.org/multipage/browsing-the-web.html#navigate-to-a-fragment
// https://html.spec.whatwg.org/multipage/browsing-the-web.html#update-document-for-history-step-application
fn sync_fragment_state(ctx: &Ctx<'_>, caller: &Rc<RefCell<World>>, owner: &Rc<RefCell<World>>) {
    let update = |ctx: &Ctx<'_>| -> Result<()> {
        let length = owner.borrow().history.length;
        let restore: rquickjs::Function =
            crate::js::bridge::object(ctx)?.get("__tbHistoryRestore")?;
        let _: Value = restore.call((Option::<String>::None, length))?;
        crate::js::events::fire_trusted_popstate(ctx, Value::new_null(ctx.clone()))
    };
    let result = if Rc::ptr_eq(caller, owner) {
        update(ctx)
    } else if let Some(owner_context) = owner.borrow().realm_context.clone() {
        owner_context.with(|owner_ctx| update(&owner_ctx))
    } else {
        return;
    };
    if let Err(error) = result {
        super::host::report_callback_error(ctx, &error);
    }
}

// https://html.spec.whatwg.org/multipage/nav-history-apis.html#the-location-interface
// https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-location-search
// https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-location-hash
// https://html.spec.whatwg.org/multipage/browsing-the-web.html#navigate-to-a-fragment
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

// https://html.spec.whatwg.org/multipage/nav-history-apis.html#the-location-interface
fn require_same_origin_location(ctx: &Ctx<'_>, owner: &Rc<RefCell<World>>) -> Result<()> {
    let caller = super::world(ctx)?;
    if !Rc::ptr_eq(&caller, owner) && !owner.borrow().same_origin_domain_with(&caller.borrow()) {
        return Err(super::throw_dom(
            ctx,
            "SecurityError",
            "blocked cross-origin Location access",
        ));
    }
    Ok(())
}

// https://html.spec.whatwg.org/multipage/nav-history-apis.html#the-location-interface
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
        let getter = Accessor::new_get(move |ctx: Ctx<'js>| -> Result<String> {
            require_same_origin_location(&ctx, &owner)?;
            Ok(component.read(&owner.borrow()))
        });
        if matches!(component, Component::Origin) {
            location.prop(name, getter.enumerable())?;
            continue;
        }
        let owner = Rc::clone(world);
        location.prop(
            name,
            getter
                .set(move |value: Value<'js>| {
                    if !matches!(component, Component::Href) {
                        require_same_origin_location(value.ctx(), &owner)?;
                    }
                    set_component(&owner, component, value)
                })
                .enumerable(),
        )?;
    }
    // https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-location-assign
    let assign_owner = Rc::clone(world);
    location.set(
        "assign",
        rquickjs::prelude::Func::from(move |ctx: Ctx<'js>, value: Value<'js>| -> Result<()> {
            require_same_origin_location(&ctx, &assign_owner)?;
            let value = super::webidl::webidl_to_string(&ctx, value)?;
            if !assign_owner.borrow().has_browsing_context() {
                return Ok(());
            }
            let caller = super::world(&ctx)?;
            let source = caller.borrow().frame();
            let url = parse_location(&ctx, &caller, &value)?;
            if only_fragment_changed(&assign_owner, &url) {
                navigate_to_fragment(&ctx, &caller, &assign_owner, &url, false);
            } else {
                navigate_location(&assign_owner, url.as_str().to_owned(), Some(source));
            }
            Ok(())
        }),
    )?;
    // https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-location-replace
    let replace_owner = Rc::clone(world);
    location.set(
        "replace",
        rquickjs::prelude::Func::from(move |ctx: Ctx<'js>, value: Value<'js>| -> Result<()> {
            let value = super::webidl::webidl_to_string(&ctx, value)?;
            if !replace_owner.borrow().has_browsing_context() {
                return Ok(());
            }
            let caller = super::world(&ctx)?;
            let source = caller.borrow().frame();
            let url = parse_location(&ctx, &caller, &value)?;
            if only_fragment_changed(&replace_owner, &url) {
                navigate_to_fragment(&ctx, &caller, &replace_owner, &url, true);
            } else {
                navigate_location(&replace_owner, url.as_str().to_owned(), Some(source));
            }
            Ok(())
        }),
    )?;
    // https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-location-reload
    let reload_owner = Rc::clone(world);
    location.set(
        "reload",
        rquickjs::prelude::Func::from(move |ctx: Ctx<'js>| -> Result<()> {
            require_same_origin_location(&ctx, &reload_owner)?;
            if !reload_owner.borrow().has_browsing_context() {
                return Ok(());
            }
            let caller = super::world(&ctx)?;
            let source = caller.borrow().frame();
            let current = reload_owner.borrow().document_url.as_str().to_owned();
            navigate_location(&reload_owner, current, Some(source));
            Ok(())
        }),
    )?;
    let tostring_owner = Rc::clone(world);
    location.set(
        "toString",
        rquickjs::prelude::Func::from(move |ctx: Ctx<'js>| -> Result<String> {
            require_same_origin_location(&ctx, &tostring_owner)?;
            Ok(Component::Href.read(&tostring_owner.borrow()))
        }),
    )?;
    Ok(location)
}

pub(crate) fn set_location_href(owner: &Rc<RefCell<World>>, value: Value<'_>) -> Result<()> {
    set_component(owner, Component::Href, value)
}

// https://webidl.spec.whatwg.org/#es-operations
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

// https://html.spec.whatwg.org/multipage/dom.html#current-document-readiness
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

// https://drafts.csswg.org/css-font-loading/#font-face-set-ready
pub(crate) fn font_face_settled(
    ctx: &Ctx<'_>,
    sheet: u64,
    path: Vec<usize>,
    loaded: bool,
) -> Result<()> {
    let hook: Option<Function> = crate::js::bridge::object(ctx)?.get("__tbFontFaceSettled")?;
    let Some(hook) = hook else {
        return Ok(());
    };
    #[expect(
        clippy::cast_precision_loss,
        reason = "sheet ids and rule indices stay inside the exact integer range of f64"
    )]
    {
        let path: Vec<_> = path.into_iter().map(|index| index as f64).collect();
        hook.call((sheet as f64, path, loaded))
    }
}

// https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#set-the-selection-range
pub(crate) fn fire_selection_changed(ctx: &Ctx<'_>, id: NodeId) -> Result<()> {
    events::fire_trusted(ctx, EventTargetKey::Node(id), "select", true, false)
}

// https://html.spec.whatwg.org/multipage/interactive-elements.html#queue-a-details-toggle-event-task
// https://html.spec.whatwg.org/multipage/interactive-elements.html#queue-a-dialog-toggle-event-task
pub(crate) fn fire_toggle_task(
    ctx: &Ctx<'_>,
    id: NodeId,
    old_state: &str,
    new_state: &str,
) -> Result<()> {
    events::fire_toggle(ctx, EventTargetKey::Node(id), old_state, new_state)
}

// https://html.spec.whatwg.org/multipage/interactive-elements.html#the-dialog-element:close-event
pub(crate) fn fire_dialog_close(ctx: &Ctx<'_>, id: NodeId) -> Result<()> {
    events::fire_trusted(ctx, EventTargetKey::Node(id), "close", false, false)
}

// https://drafts.csswg.org/cssom-view/#scrolling-events
pub(crate) fn fire_scroll_event(ctx: &Ctx<'_>, event: blitz_dom::PendingScrollEvent) -> Result<()> {
    let typ = match event.kind {
        blitz_dom::ScrollEventKind::Scroll => "scroll",
        blitz_dom::ScrollEventKind::ScrollEnd => "scrollend",
    };
    match event.target {
        blitz_dom::ScrollEventTarget::Viewport => {
            let document = main_document(ctx)?;
            events::fire_trusted(ctx, EventTargetKey::Node(document), typ, true, false)?;
            // https://drafts.csswg.org/cssom-view/#scrolling-events
            let host = crate::js::bridge::object(ctx)?;
            let Some(func) =
                host.get::<_, Option<rquickjs::Function>>("__tbVisualViewportScroll")?
            else {
                return Ok(());
            };
            func.call::<_, ()>((typ,))?;
            Ok(())
        }
        blitz_dom::ScrollEventTarget::Element(node) => {
            let document = world(ctx)?.borrow().main_document_id();
            let Some(document) = document else {
                return Ok(());
            };
            events::fire_trusted(
                ctx,
                EventTargetKey::Node(NodeId { document, node }),
                typ,
                false,
                false,
            )
        }
    }
}

// https://drafts.csswg.org/css-animations-1/#events
pub(crate) fn fire_animation_event(
    ctx: &Ctx<'_>,
    event: &blitz_dom::animation_events::PendingAnimationEvent,
) -> Result<()> {
    let document = world(ctx)?.borrow().main_document_id();
    let Some(document) = document else {
        return Ok(());
    };
    events::fire_animation_event(
        ctx,
        EventTargetKey::Node(NodeId {
            document,
            node: event.target,
        }),
        event,
    )
}

// https://w3c.github.io/webappsec-csp/#report-violation
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

#[cfg(test)]
mod tests {
    use super::protocol_scheme;

    // https://url.spec.whatwg.org/#scheme-start-state
    #[test]
    fn protocol_scheme_follows_the_scheme_start_state() {
        assert_eq!(protocol_scheme("https").as_deref(), Some("https"));
        assert_eq!(protocol_scheme("HTTPS").as_deref(), Some("https"));
        assert_eq!(protocol_scheme("http+x").as_deref(), Some("http+x"));
        assert_eq!(protocol_scheme("x:foo").as_deref(), Some("x"));
        assert_eq!(protocol_scheme("\tx").as_deref(), Some("x"));
        assert_eq!(protocol_scheme("x\t").as_deref(), Some("x"));
        assert_eq!(protocol_scheme(" x"), None);
        assert_eq!(protocol_scheme("\0x"), None);
        assert_eq!(protocol_scheme("x\0"), None);
        assert_eq!(protocol_scheme("x y"), None);
        assert_eq!(protocol_scheme(":"), None);
        assert_eq!(protocol_scheme(""), None);
    }
}
