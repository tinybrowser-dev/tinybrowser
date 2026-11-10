use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use rquickjs::{
    Context, Ctx, Exception, Function, Object, Persistent, Result, Runtime, Value,
    context::EvalOptions, object::Accessor, prelude::Func,
};

use super::World;
use super::bindings::world;

pub(crate) fn prepare(runtime: &Runtime, world: &Rc<RefCell<World>>) -> Result<()> {
    let registry = world.borrow().registry();
    if registry.borrow().private_slots.is_none() {
        let context = Context::full(runtime)?;
        let factory = context.with(|ctx| {
            let factory: Function = ctx.eval(include_str!("scripts/private_state.js"))?;
            Ok::<_, rquickjs::Error>(Persistent::save(&ctx, factory))
        })?;
        registry.borrow_mut().private_slots = Some(super::world::PrivateSlots {
            factory,
            realms: std::collections::HashSet::new(),
        });
    }
    Ok(())
}

pub(crate) fn install(ctx: &Ctx<'_>, world: &Rc<RefCell<World>>) -> Result<()> {
    let bridge = Object::new(ctx.clone())?;
    bridge.set_prototype(None)?;
    let registry = world.borrow().registry();
    let saved_slots = registry
        .borrow()
        .private_slots
        .as_ref()
        .map(|slots| slots.factory.clone());
    let slots = saved_slots
        .ok_or_else(|| Exception::throw_internal(ctx, "private slot service is not initialized"))?
        .restore(ctx)?;
    bridge.set("slots", slots)?;
    let reflect: Object = ctx.globals().get("Reflect")?;
    bridge.set("apply", reflect.get::<_, Function>("apply")?)?;
    let json: Object = ctx.globals().get("JSON")?;
    bridge.set("stringify", json.get::<_, Function>("stringify")?)?;
    // Pristine `Function` constructor for event-handler compilation: pages
    // may replace the global afterwards, and handlers compile on demand.
    let function: Object = ctx.globals().get("Function")?;
    bridge.set("__tb_function", function)?;
    let table = Object::new(ctx.clone())?;
    table.set_prototype(None)?;
    bridge.set("__tb_handles", table)?;
    for name in ["__tb_async", "__tb_async_handles"] {
        let table = Object::new(ctx.clone())?;
        table.set_prototype(None)?;
        bridge.set(name, table)?;
    }
    bridge.set("evaluatePage", Func::from(evaluate_page))?;
    bridge.set("__tb_mint", Func::from(mint_handle))?;
    let secure_context = world.borrow().is_secure_context();
    bridge.set("__tbSecureContext", secure_context)?;
    let cross_origin_isolated = world.borrow().is_cross_origin_isolated();
    bridge.set("__tbCrossOriginIsolated", cross_origin_isolated)?;
    let install: Function = ctx.eval(format!(
        "(function(host) {{ 'use strict';\n{}\n{}\n}})",
        include_str!("scripts/primordials.js"),
        include_str!("scripts/remote_object.js")
    ))?;
    install.call::<_, ()>((bridge.clone(),))?;
    if let Some(slots) = &mut registry.borrow_mut().private_slots {
        slots.realms.insert(ctx.as_raw().as_ptr() as usize);
    }
    world.borrow_mut().bridge = Some(Persistent::save(ctx, bridge));
    // `isSecureContext` is a Web IDL readonly attribute: an enumerable,
    // configurable accessor with a getter and no setter
    // (<https://html.spec.whatwg.org/multipage/webappapis.html#dom-issecurecontext>).
    ctx.globals().prop(
        "isSecureContext",
        Accessor::new_get(move || secure_context)
            .enumerable()
            .configurable(),
    )?;
    // `crossOriginIsolated` is a `WindowOrWorkerGlobalScope` mixin attribute
    // with the same shape: an enumerable, configurable accessor and no setter
    // (<https://html.spec.whatwg.org/multipage/webappapis.html#dom-crossoriginisolated>).
    ctx.globals().prop(
        "crossOriginIsolated",
        Accessor::new_get(move || cross_origin_isolated)
            .enumerable()
            .configurable(),
    )?;
    Ok(())
}

/// Next handle id handed to one page-side object. Rust owns the counter; the
/// realm only stores the value under the id it receives.
static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

pub(crate) fn mint_handle() -> String {
    let id = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed);
    format!("h{id}")
}

pub(crate) fn release(ctx: &Ctx<'_>, world: &Rc<RefCell<World>>) {
    let registry = world.borrow().registry();
    let mut registry = registry.borrow_mut();
    if let Some(slots) = &mut registry.private_slots {
        slots.realms.remove(&(ctx.as_raw().as_ptr() as usize));
        if slots.realms.is_empty() {
            registry.private_slots = None;
        }
    }
}

pub(crate) fn object<'js>(ctx: &Ctx<'js>) -> Result<Object<'js>> {
    world(ctx)?
        .borrow()
        .bridge
        .clone()
        .ok_or_else(|| Exception::throw_internal(ctx, "realm bridge is not initialized"))?
        .restore(ctx)
}

/// Web IDL's `[EnforceRange] unsigned long` conversion
/// (<https://webidl.spec.whatwg.org/#abstract-opdef-converttoint>).
///
/// Reads one number a binding received from script as the integer it names in
/// `0..=4294967295`. The value is truncated toward zero, as the algorithm
/// says, so `1.9` names 1; `None` covers every value `unsigned long` cannot
/// hold.
#[must_use]
pub(crate) fn enforce_range_u32(value: f64) -> Option<u32> {
    let value = enforce_range(value, 4_294_967_295.0)?;
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the guard admits exactly the integers 0..=u32::MAX"
    )]
    Some(value as u32)
}

/// Web IDL's `[EnforceRange] unsigned long long` conversion
/// (<https://webidl.spec.whatwg.org/#abstract-opdef-converttoint>).
///
/// Reads one number a binding received from script as the integer it names in
/// `0..=18446744073709551615`, truncated toward zero like its `unsigned long`
/// twin. Every id this engine hands out is a counter far below the range a JS
/// number carries exactly, and a number that names no live id simply finds no
/// object.
#[must_use]
pub(crate) fn enforce_range_u64(value: f64) -> Option<u64> {
    let value = enforce_range(value, U64_MAX_EXACT)?;
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the guard admits exactly the integers 0..=u64::MAX"
    )]
    Some(value as u64)
}

/// The largest integer below `2^64` an `f64` holds, `2^64 - 2048`. `2^64` itself
/// is the first value an `unsigned long long` cannot hold, and rounding puts it
/// one step above this, so this is the conversion's inclusive upper bound.
const U64_MAX_EXACT: f64 = 1.844_674_407_370_955e19;

/// The half both conversions share: truncate toward zero, then admit only
/// `0..=max`. Both bounds are exact `f64` values, so every accepted number is
/// an integer the integer cast can hold.
fn enforce_range(value: f64, max: f64) -> Option<f64> {
    if !value.is_finite() {
        return None;
    }
    let value = value.trunc();
    (value >= 0.0 && value <= max).then_some(value)
}

/// An id as the JS number a binding hands to script.
///
/// A JS number holds an integer exactly up to `2^53 - 1`; an id past that has
/// no exact number form, so it becomes `NaN` and the shim's lookup answers no
/// such object instead of addressing a neighbour
/// (<https://tc39.es/ecma262/#sec-number.max_safe_integer>).
#[must_use]
pub(crate) fn js_number(id: u64) -> f64 {
    /// The largest id a JS number carries exactly.
    const MAX_EXACT: u64 = (1 << 53) - 1;

    if id > MAX_EXACT {
        return f64::NAN;
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "the guard admits only the integers a f64 holds exactly"
    )]
    let number = id as f64;
    number
}

/// A JS number naming a frame, or `None` when it names no frame id.
#[must_use]
pub(crate) fn frame_id(value: f64) -> Option<crate::protocol::FrameId> {
    enforce_range_u64(value).map(crate::protocol::FrameId::new)
}

pub(crate) fn evaluate(ctx: &Ctx<'_>, source: &str) -> Result<()> {
    let primordials = include_str!("scripts/primordials.js");
    let bindings = include_str!("scripts/bindings.js");
    // Engine code carries its own filename so violation source attribution
    // can tell the shim bundle apart from page script: the bundle's frames
    // never name a page location
    // (<https://w3c.github.io/webappsec-csp/#create-violation-for-global>).
    let mut options = EvalOptions::default();
    options.filename = Some(ENGINE_FILENAME.into());
    let initializer: Function = ctx.eval_with_options(
        format!("(function(host) {{ 'use strict';\n{primordials}\n{bindings}\n{source}\n}})"),
        options,
    )?;
    initializer.call((object(ctx)?,))
}

/// Filename for engine shim evaluations, so stacks distinguish engine frames
/// from page script. It parses as no URL scheme, so a leaked frame strips to
/// the empty string rather than a fake location.
pub(crate) const ENGINE_FILENAME: &str = "tinybrowser-shim";

pub(crate) fn evaluate_page(ctx: Ctx<'_>, source: String) -> Result<Value<'_>> {
    let mut options = EvalOptions::default();
    options.strict = false;
    ctx.eval_with_options(source, options)
}

#[cfg(test)]
mod tests {
    use super::{enforce_range_u32, enforce_range_u64, js_number};

    #[test]
    fn an_id_number_follows_the_webidl_enforce_range_algorithm() {
        assert_eq!(enforce_range_u32(0.0), Some(0));
        assert_eq!(enforce_range_u32(7.9), Some(7), "truncated toward zero");
        assert_eq!(enforce_range_u32(4_294_967_295.0), Some(u32::MAX));
        assert_eq!(
            enforce_range_u32(4_294_967_296.0),
            None,
            "2^32 is not a `unsigned long`"
        );
        assert_eq!(enforce_range_u32(-0.5), Some(0), "-0 is the integer zero");
        for rejected in [-1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(enforce_range_u32(rejected), None, "{rejected}");
        }

        assert_eq!(enforce_range_u64(0.0), Some(0));
        assert_eq!(
            enforce_range_u64(9_007_199_254_740_991.0),
            Some(9_007_199_254_740_991)
        );
        assert_eq!(
            enforce_range_u64(super::U64_MAX_EXACT * 2.0),
            None,
            "2^64 is not an `unsigned long long`"
        );
        assert_eq!(enforce_range_u64(-1.0), None);
        assert_eq!(enforce_range_u64(f64::NAN), None);
    }

    #[test]
    fn a_js_number_holds_an_id_only_while_it_is_exact() {
        assert_eq!(js_number(0), 0.0);
        assert_eq!(js_number(7), 7.0);
        assert_eq!(js_number((1 << 53) - 1), 9_007_199_254_740_991.0);
        assert!(
            js_number(1 << 53).is_nan(),
            "an id past 2^53 - 1 has no exact number form"
        );
    }
}
