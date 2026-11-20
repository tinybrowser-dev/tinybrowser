use super::super::{attribute_value, host, set_attribute_sync, world, world_for_node};

use rquickjs::{Ctx, Result, Value};

use crate::dom::tree::{NodeId, attr, html_namespace, is_connected};

use super::JsNode;
use super::element::{
    content_attr, reflect_cross_origin, reflect_double, reflect_limited_double, reflect_ulong,
    set_cross_origin,
};

include!(concat!(env!("OUT_DIR"), "/HTMLMediaElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLVideoElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLCanvasElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLImageElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLMeterElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLProgressElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLMarqueeElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLAudioElement.rs"));

impl<'js> html_media_element_generated::HTMLMediaElement<'js> for JsNode {
    // https://html.spec.whatwg.org/multipage/media.html#dom-media-networkstate
    fn get_network_state(&self, ctx: &Ctx<'js>) -> Result<u16> {
        // https://html.spec.whatwg.org/multipage/media.html#concept-media-load-algorithm
        // https://html.spec.whatwg.org/multipage/media.html#loading-the-media-resource
        // https://html.spec.whatwg.org/multipage/embedded-content.html#the-source-element:html-element-insertion-steps
        const NETWORK_NO_SOURCE: u16 = 3;
        const NETWORK_EMPTY: u16 = 0;
        let selected = world_for_node(ctx, self.handle.get().0)
            .ok()
            .and_then(|owner| {
                owner.borrow().with_document(self.handle.get().0, |parsed| {
                    let base = &parsed.document.base;
                    if is_connected(base, self.handle.get().0.node) {
                        return true;
                    }
                    if attr(base, self.handle.get().0.node, "src").is_some() {
                        return true;
                    }
                    base.get_node(self.handle.get().0.node).is_some_and(|node| {
                        node.children.iter().any(|kid| {
                            base.get_node(*kid).is_some_and(|candidate| {
                                candidate.data.downcast_element().is_some_and(|element| {
                                    element.name.ns == html_namespace()
                                        && element.name.local.as_str() == "source"
                                })
                            })
                        })
                    })
                })
            })
            .unwrap_or(false);
        Ok(if selected {
            NETWORK_NO_SOURCE
        } else {
            NETWORK_EMPTY
        })
    }

    // https://html.spec.whatwg.org/multipage/media.html#dom-media-readystate
    fn get_ready_state(&self, _ctx: &Ctx<'js>) -> Result<u16> {
        Ok(0)
    }

    // https://html.spec.whatwg.org/multipage/media.html#dom-media-crossorigin
    fn get_cross_origin(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        reflect_cross_origin(ctx, self.handle.get().0)
    }

    // https://html.spec.whatwg.org/multipage/media.html#dom-media-crossorigin
    fn set_cross_origin(&self, ctx: &Ctx<'js>, value: Option<rquickjs::String<'js>>) -> Result<()> {
        set_cross_origin(ctx, self.handle.get().0, value)
    }
}

impl<'js> html_video_element_generated::HTMLVideoElement<'js> for JsNode {
    // https://html.spec.whatwg.org/multipage/embedded-content.html#dom-video-width
    fn get_width(&self, ctx: &Ctx<'js>) -> Result<usize> {
        Ok(usize::try_from(reflect_ulong(
            content_attr(ctx, self.handle.get().0, "width").as_deref(),
            0,
        ))
        .unwrap_or(usize::MAX))
    }

    // https://html.spec.whatwg.org/multipage/embedded-content.html#dom-video-width
    fn set_width(&self, ctx: &Ctx<'js>, value: u32) -> Result<()> {
        host::reflect_set_ulong_defaulting(ctx, self.handle.get().0, "width", value, 0)
    }

    // https://html.spec.whatwg.org/multipage/embedded-content.html#dom-video-height
    fn get_height(&self, ctx: &Ctx<'js>) -> Result<usize> {
        Ok(usize::try_from(reflect_ulong(
            content_attr(ctx, self.handle.get().0, "height").as_deref(),
            0,
        ))
        .unwrap_or(usize::MAX))
    }

    // https://html.spec.whatwg.org/multipage/embedded-content.html#dom-video-height
    fn set_height(&self, ctx: &Ctx<'js>, value: u32) -> Result<()> {
        host::reflect_set_ulong_defaulting(ctx, self.handle.get().0, "height", value, 0)
    }
}

fn reset_canvas(ctx: &Ctx<'_>, id: NodeId) -> Result<()> {
    let width = crate::js::canvas::canvas_dimension(ctx, id, "width", 300);
    let height = crate::js::canvas::canvas_dimension(ctx, id, "height", 150);
    let world = world_for_node(ctx, id)?;
    world
        .borrow()
        .registry()
        .borrow_mut()
        .reset_canvas(id, width, height);
    Ok(())
}

impl<'js> html_canvas_element_generated::HTMLCanvasElement<'js> for JsNode {
    // https://html.spec.whatwg.org/multipage/canvas.html#dom-canvas-width
    fn get_width(&self, ctx: &Ctx<'js>) -> Result<usize> {
        Ok(
            usize::try_from(canvas_dimension(ctx, self.handle.get().0, "width", 300))
                .unwrap_or(usize::MAX),
        )
    }

    fn set_width(&self, ctx: &Ctx<'js>, value: u32) -> Result<()> {
        // https://html.spec.whatwg.org/multipage/canvas.html#dom-canvas-width
        let value = if value > 2_147_483_647 { 300 } else { value };
        set_attribute_sync(ctx, self.handle.get().0, "width", &value.to_string())?;
        reset_canvas(ctx, self.handle.get().0)
    }

    // https://html.spec.whatwg.org/multipage/canvas.html#dom-canvas-height
    fn get_height(&self, ctx: &Ctx<'js>) -> Result<usize> {
        Ok(
            usize::try_from(canvas_dimension(ctx, self.handle.get().0, "height", 150))
                .unwrap_or(usize::MAX),
        )
    }

    fn set_height(&self, ctx: &Ctx<'js>, value: u32) -> Result<()> {
        // https://html.spec.whatwg.org/multipage/canvas.html#dom-canvas-height
        let value = if value > 2_147_483_647 { 150 } else { value };
        set_attribute_sync(ctx, self.handle.get().0, "height", &value.to_string())?;
        reset_canvas(ctx, self.handle.get().0)
    }

    // https://html.spec.whatwg.org/multipage/canvas.html#dom-canvas-getcontext
    fn get_context(
        &self,
        ctx: Ctx<'js>,
        context_id: rquickjs::String<'js>,
        options: Value<'js>,
    ) -> Result<Option<RenderingContext<'js>>> {
        // https://html.spec.whatwg.org/multipage/canvas.html#dom-canvas-getcontext
        let requested = context_id.to_string()?;
        let member = match requested.as_str() {
            "2d" => crate::js::world::SameObject::Canvas2d,
            "bitmaprenderer" => crate::js::world::SameObject::CanvasBitmap,
            _ => return Ok(None),
        };
        let world_rc = world(&ctx)?;
        if let Some(saved) = world_rc.borrow().same_object(self.handle.get().0, member)
            && let Some(value) = super::super::deref_weak(&ctx, saved)?
        {
            return Ok(Some(context_variant(member, value)));
        }
        let other = match member {
            crate::js::world::SameObject::Canvas2d => crate::js::world::SameObject::CanvasBitmap,
            _ => crate::js::world::SameObject::Canvas2d,
        };
        if let Some(saved) = world_rc.borrow().same_object(self.handle.get().0, other)
            && super::super::deref_weak(&ctx, saved)?.is_some()
        {
            // https://html.spec.whatwg.org/multipage/canvas.html#concept-canvas-context-mode
            return Ok(None);
        }
        // https://html.spec.whatwg.org/multipage/canvas.html#concept-canvas-context-mode
        let requested_mode = if member == crate::js::world::SameObject::Canvas2d {
            crate::js::world::CanvasMode::TwoD
        } else {
            crate::js::world::CanvasMode::Bitmap
        };
        if let Some(mode) = world_rc
            .borrow()
            .registry()
            .borrow()
            .canvas_mode_for_node(self.handle.get().0)
            && mode != crate::js::world::CanvasMode::None
            && mode != requested_mode
        {
            return Ok(None);
        }
        crate::js::canvas::read_context_options(&ctx, Some(&options))?;
        let width = crate::js::canvas::canvas_dimension(&ctx, self.handle.get().0, "width", 300);
        let height = crate::js::canvas::canvas_dimension(&ctx, self.handle.get().0, "height", 150);
        let canvas_id = world_rc.borrow().registry().borrow_mut().element_canvas(
            self.handle.get().0,
            width,
            height,
        );
        let value = if member == crate::js::world::SameObject::Canvas2d {
            world_rc
                .borrow()
                .registry()
                .borrow_mut()
                .set_canvas_mode(canvas_id, crate::js::world::CanvasMode::TwoD);
            crate::js::canvas::context_object(&ctx, canvas_id)?
        } else {
            world_rc
                .borrow()
                .registry()
                .borrow_mut()
                .set_canvas_mode(canvas_id, crate::js::world::CanvasMode::Bitmap);
            bitmap_context_object(&ctx, self.handle.get().0)?
        };
        let weak = super::super::make_weak(&ctx, value.clone())?;
        world_rc.borrow_mut().intern_same_object(
            self.handle.get().0,
            member,
            rquickjs::Persistent::save(&ctx, weak),
        );
        Ok(Some(context_variant(member, value)))
    }
}

impl<'js> html_image_element_generated::HTMLImageElement<'js> for JsNode {
    // https://html.spec.whatwg.org/multipage/embedded-content.html#dom-img-naturalwidth
    fn get_natural_width(&self, ctx: &Ctx<'js>) -> Result<usize> {
        Ok(self.natural_width(ctx.clone())? as usize)
    }

    // https://html.spec.whatwg.org/multipage/embedded-content.html#dom-img-naturalheight
    fn get_natural_height(&self, ctx: &Ctx<'js>) -> Result<usize> {
        Ok(self.natural_height(ctx.clone())? as usize)
    }

    // https://html.spec.whatwg.org/multipage/embedded-content.html#dom-img-complete
    fn get_complete(&self, ctx: &Ctx<'js>) -> Result<bool> {
        self.complete(ctx.clone())
    }

    // https://drafts.csswg.org/cssom-view/#dom-htmlimageelement-x
    fn get_x(&self, ctx: &Ctx<'js>) -> Result<i32> {
        Ok(super::offset::image_edge(ctx, self.handle.get().0, false))
    }

    // https://drafts.csswg.org/cssom-view/#dom-htmlimageelement-y
    fn get_y(&self, ctx: &Ctx<'js>) -> Result<i32> {
        Ok(super::offset::image_edge(ctx, self.handle.get().0, true))
    }

    // https://html.spec.whatwg.org/multipage/embedded-content.html#dom-img-currentsrc
    fn get_current_src(&self, ctx: &Ctx<'js>) -> Result<crate::dom_string::DomString> {
        Ok(self.current_src(ctx.clone())?.into())
    }

    // https://html.spec.whatwg.org/multipage/embedded-content.html#dom-img-width
    fn get_width(&self, ctx: &Ctx<'js>) -> Result<u32> {
        Ok(reflect_ulong(
            content_attr(ctx, self.handle.get().0, "width").as_deref(),
            0,
        ))
    }

    // https://html.spec.whatwg.org/multipage/embedded-content.html#dom-img-height
    fn get_height(&self, ctx: &Ctx<'js>) -> Result<u32> {
        Ok(reflect_ulong(
            content_attr(ctx, self.handle.get().0, "height").as_deref(),
            0,
        ))
    }

    // https://html.spec.whatwg.org/multipage/embedded-content.html#dom-img-crossorigin
    fn get_cross_origin(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        reflect_cross_origin(ctx, self.handle.get().0)
    }

    // https://html.spec.whatwg.org/multipage/embedded-content.html#dom-img-crossorigin
    fn set_cross_origin(&self, ctx: &Ctx<'js>, value: Option<rquickjs::String<'js>>) -> Result<()> {
        set_cross_origin(ctx, self.handle.get().0, value)
    }

    // https://html.spec.whatwg.org/multipage/obsolete.html#dom-img-hspace
    fn get_hspace(&self, ctx: &Ctx<'js>) -> Result<usize> {
        Ok(usize::try_from(reflect_ulong(
            content_attr(ctx, self.handle.get().0, "hspace").as_deref(),
            0,
        ))
        .unwrap_or(usize::MAX))
    }

    // https://html.spec.whatwg.org/multipage/obsolete.html#dom-img-hspace
    fn set_hspace(&self, ctx: &Ctx<'js>, value: u32) -> Result<()> {
        host::reflect_set_ulong_defaulting(ctx, self.handle.get().0, "hspace", value, 0)
    }

    // https://html.spec.whatwg.org/multipage/obsolete.html#dom-img-vspace
    fn get_vspace(&self, ctx: &Ctx<'js>) -> Result<usize> {
        Ok(usize::try_from(reflect_ulong(
            content_attr(ctx, self.handle.get().0, "vspace").as_deref(),
            0,
        ))
        .unwrap_or(usize::MAX))
    }

    // https://html.spec.whatwg.org/multipage/obsolete.html#dom-img-vspace
    fn set_vspace(&self, ctx: &Ctx<'js>, value: u32) -> Result<()> {
        host::reflect_set_ulong_defaulting(ctx, self.handle.get().0, "vspace", value, 0)
    }
}

impl<'js> html_meter_element_generated::HTMLMeterElement<'js> for JsNode {
    // https://html.spec.whatwg.org/multipage/form-elements.html#the-meter-element
    fn get_value(&self, ctx: &Ctx<'js>) -> Result<f64> {
        Ok(meter_gauge(ctx, self.handle.get().0).value)
    }

    // https://html.spec.whatwg.org/multipage/form-elements.html#dom-meter-value
    fn get_min(&self, ctx: &Ctx<'js>) -> Result<f64> {
        Ok(meter_gauge(ctx, self.handle.get().0).min)
    }

    // https://html.spec.whatwg.org/multipage/form-elements.html#dom-meter-min
    fn get_max(&self, ctx: &Ctx<'js>) -> Result<f64> {
        Ok(meter_gauge(ctx, self.handle.get().0).max)
    }

    // https://html.spec.whatwg.org/multipage/form-elements.html#dom-meter-max
    fn get_low(&self, ctx: &Ctx<'js>) -> Result<f64> {
        Ok(meter_gauge(ctx, self.handle.get().0).low)
    }

    // https://html.spec.whatwg.org/multipage/form-elements.html#dom-meter-low
    fn get_high(&self, ctx: &Ctx<'js>) -> Result<f64> {
        Ok(meter_gauge(ctx, self.handle.get().0).high)
    }

    // https://html.spec.whatwg.org/multipage/form-elements.html#dom-meter-high
    fn get_optimum(&self, ctx: &Ctx<'js>) -> Result<f64> {
        Ok(meter_gauge(ctx, self.handle.get().0).optimum)
    }
}

impl<'js> html_progress_element_generated::HTMLProgressElement<'js> for JsNode {
    // https://html.spec.whatwg.org/multipage/form-elements.html#dom-progress-value
    fn get_value(&self, ctx: &Ctx<'js>) -> Result<f64> {
        let max = reflect_limited_double(
            content_attr(ctx, self.handle.get().0, "max").as_deref(),
            1.0,
        );
        Ok(
            match content_attr(ctx, self.handle.get().0, "value")
                .as_deref()
                .and_then(crate::dom::parse::parse_double)
            {
                Some(value) if value > 0.0 => value.min(max),
                _ => 0.0,
            },
        )
    }

    // https://html.spec.whatwg.org/multipage/form-elements.html#dom-progress-max
    fn get_max(&self, ctx: &Ctx<'js>) -> Result<f64> {
        Ok(reflect_limited_double(
            content_attr(ctx, self.handle.get().0, "max").as_deref(),
            1.0,
        ))
    }

    // https://html.spec.whatwg.org/multipage/form-elements.html#dom-progress-max
    fn set_max(&self, ctx: &Ctx<'js>, value: f64) -> Result<()> {
        // https://html.spec.whatwg.org/multipage/form-elements.html#dom-progress-max
        if value.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater) {
            return Ok(());
        }
        host::reflect_set_double(ctx, self.handle.get().0, "max", value)
    }
}

impl<'js> html_marquee_element_generated::HTMLMarqueeElement<'js> for JsNode {
    // https://html.spec.whatwg.org/multipage/obsolete.html#dom-marquee-scrollamount
    fn get_scroll_amount(&self, ctx: &Ctx<'js>) -> Result<usize> {
        Ok(usize::try_from(reflect_ulong(
            content_attr(ctx, self.handle.get().0, "scrollamount").as_deref(),
            6,
        ))
        .unwrap_or(usize::MAX))
    }

    // https://html.spec.whatwg.org/multipage/obsolete.html#dom-marquee-scrollamount
    fn set_scroll_amount(&self, ctx: &Ctx<'js>, value: u32) -> Result<()> {
        host::reflect_set_ulong_defaulting(ctx, self.handle.get().0, "scrollamount", value, 6)
    }

    // https://html.spec.whatwg.org/multipage/obsolete.html#dom-marquee-scrolldelay
    fn get_scroll_delay(&self, ctx: &Ctx<'js>) -> Result<usize> {
        Ok(usize::try_from(reflect_ulong(
            content_attr(ctx, self.handle.get().0, "scrolldelay").as_deref(),
            85,
        ))
        .unwrap_or(usize::MAX))
    }

    // https://html.spec.whatwg.org/multipage/obsolete.html#dom-marquee-scrolldelay
    fn set_scroll_delay(&self, ctx: &Ctx<'js>, value: u32) -> Result<()> {
        host::reflect_set_ulong_defaulting(ctx, self.handle.get().0, "scrolldelay", value, 85)
    }

    // https://html.spec.whatwg.org/multipage/obsolete.html#dom-marquee-loop
    fn get_loop(&self, ctx: &Ctx<'js>) -> Result<i32> {
        Ok(content_attr(ctx, self.handle.get().0, "loop")
            .as_deref()
            .and_then(crate::dom::parse::parse_integer)
            .unwrap_or(-1))
    }

    // https://html.spec.whatwg.org/multipage/obsolete.html#dom-marquee-loop
    fn set_loop(&self, ctx: &Ctx<'js>, value: i32) -> Result<()> {
        set_attribute_sync(ctx, self.handle.get().0, "loop", &value.to_string())
    }

    // https://html.spec.whatwg.org/multipage/obsolete.html#dom-marquee-hspace
    fn get_hspace(&self, ctx: &Ctx<'js>) -> Result<usize> {
        Ok(usize::try_from(reflect_ulong(
            content_attr(ctx, self.handle.get().0, "hspace").as_deref(),
            0,
        ))
        .unwrap_or(usize::MAX))
    }

    // https://html.spec.whatwg.org/multipage/obsolete.html#dom-marquee-hspace
    fn set_hspace(&self, ctx: &Ctx<'js>, value: u32) -> Result<()> {
        host::reflect_set_ulong_defaulting(ctx, self.handle.get().0, "hspace", value, 0)
    }

    // https://html.spec.whatwg.org/multipage/obsolete.html#dom-marquee-vspace
    fn get_vspace(&self, ctx: &Ctx<'js>) -> Result<usize> {
        Ok(usize::try_from(reflect_ulong(
            content_attr(ctx, self.handle.get().0, "vspace").as_deref(),
            0,
        ))
        .unwrap_or(usize::MAX))
    }

    // https://html.spec.whatwg.org/multipage/obsolete.html#dom-marquee-vspace
    fn set_vspace(&self, ctx: &Ctx<'js>, value: u32) -> Result<()> {
        host::reflect_set_ulong_defaulting(ctx, self.handle.get().0, "vspace", value, 0)
    }
}

impl JsNode {
    // https://html.spec.whatwg.org/multipage/embedded-content.html#dom-img-naturalwidth
    pub(super) fn natural_width(&self, ctx: Ctx<'_>) -> Result<u32> {
        Ok(img_size(&ctx, self.handle.get().0)?.map_or(0, |(width, _)| width))
    }

    // https://html.spec.whatwg.org/multipage/embedded-content.html#dom-img-naturalheight
    pub(super) fn natural_height(&self, ctx: Ctx<'_>) -> Result<u32> {
        Ok(img_size(&ctx, self.handle.get().0)?.map_or(0, |(_, height)| height))
    }

    // https://html.spec.whatwg.org/multipage/embedded-content.html#dom-img-complete
    pub(super) fn complete(&self, ctx: Ctx<'_>) -> Result<bool> {
        let owner = world_for_node(&ctx, self.handle.get().0)?;
        let is_img = owner
            .borrow()
            .document(self.handle.get().0)
            .is_some_and(|parsed| {
                crate::dom::tree::is_html_element(
                    &parsed.document.base,
                    self.handle.get().0.node,
                    "img",
                )
            });
        if !is_img {
            return Ok(false);
        }
        let world = world(&ctx)?;
        let world = world.borrow();
        // https://html.spec.whatwg.org/multipage/embedded-content.html#dom-img-complete
        let (src_empty, srcset_present) =
            world
                .document(self.handle.get().0)
                .map_or((true, false), |parsed| {
                    let base = &parsed.document.base;
                    (
                        attr(base, self.handle.get().0.node, "src").is_none_or(str::is_empty),
                        attr(base, self.handle.get().0.node, "srcset").is_some(),
                    )
                });
        if src_empty && !srcset_present {
            return Ok(true);
        }
        Ok(world
            .image_request(self.handle.get().0)
            .is_some_and(|request| request.status != blitz_dom::ImageRequestStatus::Pending))
    }

    // https://html.spec.whatwg.org/multipage/embedded-content.html#dom-img-currentsrc
    pub(super) fn current_src(&self, ctx: Ctx<'_>) -> Result<String> {
        let owner = world_for_node(&ctx, self.handle.get().0)?;
        let is_img = owner
            .borrow()
            .document(self.handle.get().0)
            .is_some_and(|parsed| {
                crate::dom::tree::is_html_element(
                    &parsed.document.base,
                    self.handle.get().0.node,
                    "img",
                )
            });
        if !is_img {
            return Ok(String::new());
        }
        let world = world(&ctx)?;
        Ok(world
            .borrow()
            .image_request(self.handle.get().0)
            .map_or_default(|request| {
                request
                    .final_url
                    .as_ref()
                    .or(request.key.as_ref().map(|key| &key.url))
                    .map_or_default(ToString::to_string)
            }))
    }
}
pub(super) fn img_size(ctx: &Ctx<'_>, id: NodeId) -> Result<Option<(u32, u32)>> {
    let owner = world_for_node(ctx, id)?;
    let is_img = owner.borrow().document(id).is_some_and(|parsed| {
        crate::dom::tree::is_html_element(&parsed.document.base, id.node, "img")
    });
    if !is_img {
        return Ok(None);
    }
    let world = world(ctx)?;
    Ok(world.borrow().image_size(id))
}

// https://html.spec.whatwg.org/multipage/input.html#image-button-state
pub(super) fn is_image_input(ctx: &Ctx<'_>, id: NodeId) -> bool {
    content_attr(ctx, id, "type")
        .as_deref()
        .is_some_and(|kind| kind.eq_ignore_ascii_case("image"))
}

// https://html.spec.whatwg.org/multipage/form-elements.html#the-meter-element
pub(super) struct MeterGauge {
    min: f64,
    max: f64,
    value: f64,
    low: f64,
    high: f64,
    optimum: f64,
}

pub(super) fn meter_gauge(ctx: &Ctx<'_>, id: NodeId) -> MeterGauge {
    let min = reflect_double(content_attr(ctx, id, "min").as_deref(), 0.0);
    let mut max = reflect_double(content_attr(ctx, id, "max").as_deref(), 1.0);
    if max < min {
        max = min;
    }
    let raw_value = reflect_double(content_attr(ctx, id, "value").as_deref(), 0.0);
    let value = raw_value.clamp(min, max);
    let raw_low = reflect_double(content_attr(ctx, id, "low").as_deref(), min);
    let low = raw_low.clamp(min, max);
    let raw_high = reflect_double(content_attr(ctx, id, "high").as_deref(), max);
    let mut high = raw_high.clamp(min, max);
    if high < low {
        high = low;
    }
    #[expect(
        clippy::manual_midpoint,
        reason = "f64::midpoint differs only when min + max overflows; a lint cleanup keeps the existing result"
    )]
    let raw_optimum = reflect_double(
        content_attr(ctx, id, "optimum").as_deref(),
        (min + max) / 2.0,
    );
    let optimum = raw_optimum.clamp(min, max);
    MeterGauge {
        min,
        max,
        value,
        low,
        high,
        optimum,
    }
}

// https://html.spec.whatwg.org/multipage/canvas.html#dom-canvas-width
pub(super) fn canvas_dimension(ctx: &Ctx<'_>, id: NodeId, attribute: &str, default: u32) -> u32 {
    let value = attribute_value(ctx, id, attribute).unwrap_or_default();
    crate::dom::parse::parse_non_negative_integer(&value).unwrap_or(default)
}

pub(super) fn install(ctx: &Ctx<'_>) -> Result<()> {
    html_media_element_generated::install(ctx)?;
    html_video_element_generated::install(ctx)?;
    html_canvas_element_generated::install(ctx)?;
    html_image_element_generated::install(ctx)?;
    html_meter_element_generated::install(ctx)?;
    html_progress_element_generated::install(ctx)?;
    html_marquee_element_generated::install(ctx)?;
    html_audio_element_generated::install(ctx)?;
    crate::js::canvas::install(ctx)?;
    Ok(())
}

type RenderingContext<'js> =
    html_canvas_element_generated::CanvasRenderingContext2DOrGPUCanvasContextOrImageBitmapRenderingContextOrWebGL2RenderingContextOrWebGLRenderingContext<'js>;

fn context_variant(member: crate::js::world::SameObject, value: Value<'_>) -> RenderingContext<'_> {
    if member == crate::js::world::SameObject::CanvasBitmap {
        RenderingContext::ImageBitmapRenderingContext(value)
    } else {
        RenderingContext::CanvasRenderingContext2D(value)
    }
}

fn bitmap_context_object<'js>(ctx: &Ctx<'js>, id: NodeId) -> Result<Value<'js>> {
    let globals = ctx.globals();
    let ctor =
        if let Ok(existing) = globals.get::<_, rquickjs::Function>("ImageBitmapRenderingContext") {
            existing
        } else {
            let ctor = rquickjs::Function::new(ctx.clone(), || {})?;
            let prototype = rquickjs::Object::new(ctx.clone())?;
            ctor.set("prototype", prototype)?;
            globals.set("ImageBitmapRenderingContext", ctor.clone())?;
            ctor
        };
    let prototype: rquickjs::Object = ctor.get("prototype")?;
    let object = rquickjs::Object::new(ctx.clone())?;
    object.set_prototype(Some(&prototype))?;
    object.set("canvas", super::super::wrap_node(ctx, id)?)?;
    Ok(object.into_value())
}
