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
}

pub(crate) fn queue_resource_timing(ctx: &Ctx<'_>, entry: ResourceTiming<'_>) -> Result<()> {
    let Some(function) =
        crate::js::bridge::object(ctx)?.get::<_, Option<Function>>("__tbQueueResourceTiming")?
    else {
        return Ok(());
    };
    function.call::<_, ()>((
        entry.name,
        entry.initiator_type,
        entry.start_time,
        entry.end_time,
        entry.response_status,
        entry.encoded_body_size,
        entry.decoded_body_size,
    ))
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
