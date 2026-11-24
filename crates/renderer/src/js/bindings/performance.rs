// https://w3c.github.io/resource-timing/#dfn-add-a-performanceresourcetiming-entry

use rquickjs::{Ctx, Function, Result};

pub(crate) struct ResourceTiming<'a> {
    pub(crate) name: &'a str,
    pub(crate) initiator_type: &'a str,
    pub(crate) start_time: f64,
    pub(crate) end_time: f64,
    pub(crate) response_status: u16,
    pub(crate) encoded_body_size: u64,
    pub(crate) decoded_body_size: u64,
    pub(crate) tao_passed: bool,
}

pub(crate) fn queue_resource_timing(ctx: &Ctx<'_>, entry: ResourceTiming<'_>) -> Result<()> {
    let Some(function) =
        crate::js::bridge::object(ctx)?.get::<_, Option<Function>>("__tbQueueResourceTiming")?
    else {
        return Ok(());
    };
    let mut args = rquickjs::function::Args::new(ctx.clone(), 8);
    args.push_arg(entry.name)?;
    args.push_arg(entry.initiator_type)?;
    args.push_arg(entry.start_time)?;
    args.push_arg(entry.end_time)?;
    args.push_arg(entry.response_status)?;
    args.push_arg(entry.encoded_body_size)?;
    args.push_arg(entry.decoded_body_size)?;
    args.push_arg(entry.tao_passed)?;
    function.call_arg::<()>(args)
}

// https://w3c.github.io/navigation-timing/#marking-navigation-timing
pub(crate) fn queue_navigation_timing_entry(ctx: &Ctx<'_>) -> Result<()> {
    let Some(function) =
        crate::js::bridge::object(ctx)?.get::<_, Option<Function>>("__tbQueueNavigationTiming")?
    else {
        return Ok(());
    };
    function.call::<_, ()>(())
}
