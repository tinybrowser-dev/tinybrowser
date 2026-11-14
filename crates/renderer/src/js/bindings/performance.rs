//! Resource Timing entry creation for the renderer's own subresource fetches.
//!
//! A page `fetch`/XHR queues its entry from the shim that owns the request.
//! Blitz subresources (images, stylesheets, fonts) and classic scripts are
//! dialed by the document itself, so the document hands the finished request
//! to the realm here
//! (<https://w3c.github.io/resource-timing/#dfn-add-a-performanceresourcetiming-entry>).

use rquickjs::{Ctx, Function, Result};

/// One completed resource request, in the timeline's millisecond timebase.
pub(crate) struct ResourceTiming<'a> {
    pub(crate) name: &'a str,
    pub(crate) initiator_type: &'a str,
    pub(crate) start_time: f64,
    pub(crate) end_time: f64,
    pub(crate) response_status: u16,
    pub(crate) encoded_body_size: u64,
    pub(crate) decoded_body_size: u64,
}

/// Appends one resource entry through the `performance.js` hook. A realm that
/// never installed the shim has no timeline, so the entry is dropped.
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

/// Queues the navigation entry to observers now that the document has
/// finished loading
/// (<https://w3c.github.io/navigation-timing/#marking-navigation-timing>).
pub(crate) fn queue_navigation_timing_entry(ctx: &Ctx<'_>) -> Result<()> {
    let Some(function) =
        crate::js::bridge::object(ctx)?.get::<_, Option<Function>>("__tbQueueNavigationTiming")?
    else {
        return Ok(());
    };
    function.call::<_, ()>(())
}
