//! Canvas 2D context: JS bindings over [`Canvas2D`].
//!
//! The context is a plain platform object, not a node wrapper, so the
//! generated IDL traits (which dispatch on `JsNode`) do not apply. Members
//! are installed on the `CanvasRenderingContext2D` prototype and reach the
//! bitmap through the opaque `__tbCanvasId` stamped on the instance.
//!
//! Spec: <https://html.spec.whatwg.org/multipage/canvas.html#2dcontext>

mod host;
mod path2d;
mod serialization;
mod state;
mod text;

pub(crate) use host::{CanvasHost, CanvasId, ElementCanvas, OffscreenCanvasHost};
pub(crate) use serialization::install as install_serialization;
pub(crate) use state::{
    Canvas2D, ColorMatrix, Gradient, JpegQuality, Pattern, Rect, SourceImage, StyleValue,
};

use rquickjs::atom::PredefinedAtom;
use rquickjs::function::This;
use rquickjs::object::{Accessor, Property};
use rquickjs::prelude::{Func, Opt, Rest};
use rquickjs::{
    Array, Class, Coerced, Ctx, Exception, FromJs, Function, Object, Result, TypedArray, Value,
};

use num_traits::ToPrimitive as _;
use std::sync::Arc;

use crate::dom::tree::NodeId;
use crate::js::bindings::{attribute_value, world, wrap_node};
use path2d::JsPath2D;
use state::serialize_color;

/// A `WebIDL` `DOMString` argument: coerces like `ToString`, then owns the
/// UTF-8 bytes for the duration of the call.
pub(crate) struct DomString(std::string::String);

impl std::ops::Deref for DomString {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// One measured glyph cluster. A native class, not a plain object, so
/// `fillTextCluster` can tell a cluster this engine produced from an object a
/// page made up; the brand is the class itself
/// (<https://github.com/whatwg/html/issues/10677>).
#[derive(rquickjs::JsLifetime)]
#[rquickjs::class(rename = "TextCluster")]
pub(crate) struct JsTextCluster {
    record: std::rc::Rc<text::TextMetricsRecord>,
    /// Index into `record.shaped.clusters`.
    index: usize,
    /// The alignment and baseline this cluster was positioned with. A
    /// `getTextClusters({align, baseline})` override applies to the returned
    /// clusters, so the object cannot read them back from the record.
    align: text::ResolvedAlign,
    baseline: state::TextBaseline,
}

impl<'js> rquickjs::class::Trace<'js> for JsTextCluster {
    fn trace<'a>(&self, _tracer: rquickjs::class::Tracer<'a, 'js>) {}
}

/// One `measureText()` result. The class owns the shaping through an `Rc`, so
/// the shaping is released when `QuickJS` collects the metrics object and every
/// cluster it returned instead of living in a context-wide map.
#[derive(rquickjs::JsLifetime)]
#[rquickjs::class(rename = "TextMetrics")]
pub(crate) struct JsTextMetrics {
    record: std::rc::Rc<text::TextMetricsRecord>,
}

impl<'js> rquickjs::class::Trace<'js> for JsTextMetrics {
    fn trace<'a>(&self, _tracer: rquickjs::class::Tracer<'a, 'js>) {}
}

impl<'js> FromJs<'js> for DomString {
    fn from_js(ctx: &Ctx<'js>, value: Value<'js>) -> Result<Self> {
        Ok(Self(
            Coerced::<rquickjs::String>::from_js(ctx, value)?.to_string()?,
        ))
    }
}

/// `OffscreenCanvas.transferToImageBitmap()`: snapshot the bitmap, clear the
/// canvas to transparent black, and wrap the pixels in an `ImageBitmap`
/// (<https://html.spec.whatwg.org/multipage/canvas.html#dom-offscreencanvas-transfertoimagebitmap>).
fn offscreen_transfer_to_image_bitmap(ctx: Ctx<'_>, raw: f64) -> Result<Value<'_>> {
    let id = canvas_id_argument(&ctx, raw)?;
    let world = world(&ctx)?;
    let image = {
        let registry = world.borrow().registry();
        let mut registry = registry.borrow_mut();
        if !registry.owns_canvas(id, &world.borrow()) {
            return Err(crate::js::bindings::throw_dom(
                &ctx,
                "InvalidStateError",
                "the canvas is not usable",
            ));
        }
        match registry.canvas_mode(id) {
            Some(crate::js::world::CanvasMode::Detached) => {
                return Err(crate::js::bindings::throw_dom(
                    &ctx,
                    "InvalidStateError",
                    "the OffscreenCanvas is detached",
                ));
            }
            Some(crate::js::world::CanvasMode::TwoD) => {}
            _ => {
                return Err(crate::js::bindings::throw_dom(
                    &ctx,
                    "InvalidStateError",
                    "the OffscreenCanvas has no 2D context",
                ));
            }
        }
        if registry.canvas_has_open_layer(id) {
            return Err(crate::js::bindings::throw_dom(
                &ctx,
                "InvalidStateError",
                "a layer is open",
            ));
        }
        // A context exists once `getContext` returned it, even before the
        // first draw; allocate the bitmap so the first transfer returns a
        // transparent image instead of "no 2D context".
        if registry.canvas_mut(id).is_none() {
            return Err(crate::js::bindings::throw_dom(
                &ctx,
                "InvalidStateError",
                "the OffscreenCanvas has no 2D context",
            ));
        }
        let Some(image) = registry.canvas_snapshot(id) else {
            return Err(crate::js::bindings::throw_dom(
                &ctx,
                "InvalidStateError",
                "the OffscreenCanvas has no 2D context",
            ));
        };
        registry.clear_canvas_bitmap(id);
        image
    };
    image_bitmap_object(
        &ctx,
        image.width,
        image.height,
        image.pixels,
        image.origin_clean,
    )
}

/// Reads a bitmap id that an internal host function received from the shim.
fn canvas_id_argument(ctx: &Ctx<'_>, raw: f64) -> Result<CanvasId> {
    crate::js::bridge::enforce_range_u64(raw)
        .map(CanvasId)
        .ok_or_else(|| Exception::throw_type(ctx, "invalid canvas id"))
}

/// The bitmap id of an `OffscreenCanvas`, read through the pristine brand the
/// shim keeps in a realm-private map. A plain object with the same shape is
/// not an `OffscreenCanvas`.
pub(crate) fn offscreen_canvas_id<'js>(
    ctx: &Ctx<'js>,
    value: &Value<'js>,
) -> Result<Option<CanvasId>> {
    if !value.is_object() {
        return Ok(None);
    }
    let host = crate::js::bridge::object(ctx)?;
    let brand: Option<Function> = host.get("__tbOffscreenCanvasId")?;
    let Some(brand) = brand else {
        return Ok(None);
    };
    let raw: f64 = brand.call((value.clone(),))?;
    if !raw.is_finite() || raw < 0.0 {
        return Ok(None);
    }
    Ok(crate::js::bridge::enforce_range_u64(raw).map(CanvasId))
}

/// Builds the `CanvasRenderingContext2D` constructor and its prototype.
pub(crate) fn install(ctx: &Ctx<'_>) -> Result<()> {
    let host = crate::js::bridge::object(ctx)?;
    host.set("__tbOffscreenCanvasNew", Func::from(offscreen_canvas_new))?;
    host.set(
        "__tbOffscreenCanvasWidth",
        Func::from(offscreen_canvas_width),
    )?;
    host.set(
        "__tbOffscreenCanvasHeight",
        Func::from(offscreen_canvas_height),
    )?;
    host.set(
        "__tbOffscreenCanvasSetWidth",
        Func::from(offscreen_canvas_set_width),
    )?;
    host.set(
        "__tbOffscreenCanvasSetHeight",
        Func::from(offscreen_canvas_set_height),
    )?;
    host.set(
        "__tbOffscreenCanvasContext",
        Func::from(offscreen_canvas_context),
    )?;
    host.set(
        "__tbOffscreenCanvasTransferToImageBitmap",
        Func::from(offscreen_transfer_to_image_bitmap),
    )?;
    host.set(
        "__tbOffscreenCanvasDetach",
        Func::from(offscreen_canvas_detach),
    )?;
    let ctor = Function::new(ctx.clone(), || {})?;
    let prototype = Object::new(ctx.clone())?;
    install_members(ctx, &prototype)?;
    ctor.prop("prototype", prototype)?;
    ctx.globals().set("CanvasRenderingContext2D", ctor)?;
    install_gradient_objects(ctx)?;
    install_text_metrics_objects(ctx)?;
    install_path2d(ctx)?;
    install_image_data(ctx)?;
    install_image_bitmap(ctx)?;
    let host = crate::js::bridge::object(ctx)?;
    host.set("__tbImageBitmapPixels", Func::from(image_bitmap_pixels))?;
    host.set("__tbImageBitmapFrom", Func::from(image_bitmap_from))?;
    Ok(())
}

/// Reads an `ImageBitmap`'s pixels for structured serialization
/// (<https://html.spec.whatwg.org/multipage/structured-data.html#serializable-objects>).
fn image_bitmap_pixels<'js>(ctx: Ctx<'js>, image: Value<'js>) -> Result<Value<'js>> {
    let null = || Value::new_null(ctx.clone());
    let Some(object) = image.as_object() else {
        return Ok(null());
    };
    if object.get::<_, bool>("__tbClosed").unwrap_or(false) {
        return Ok(null());
    }
    let Ok(id) = object.get::<_, u64>("__tbBitmap") else {
        return Ok(null());
    };
    let bitmap = world(&ctx)?.borrow().registry().borrow().bitmap(id);
    let Some(bitmap) = bitmap else {
        return Ok(null());
    };
    Ok(TypedArray::new(ctx.clone(), bitmap.pixels.to_vec())?.into_value())
}

/// Builds a fresh `ImageBitmap` from serialized pixels, so a clone does not
/// share the source's bitmap
/// (<https://html.spec.whatwg.org/multipage/structured-data.html#structureddeserialize>).
fn image_bitmap_from<'js>(
    ctx: Ctx<'js>,
    width: u32,
    height: u32,
    origin_clean: bool,
    data: Value<'js>,
) -> Result<Value<'js>> {
    let Some(bytes) = typed_array_bytes(&data)? else {
        return Err(Exception::throw_type(&ctx, "ImageBitmap needs pixel data"));
    };
    image_bitmap_object(&ctx, width, height, bytes.into(), origin_clean)
}

fn install_path2d(ctx: &Ctx<'_>) -> Result<()> {
    path2d::install(ctx)
}

/// `new OffscreenCanvas(width, height)`: allocate a bitmap in this realm with
/// no DOM node
/// (<https://html.spec.whatwg.org/multipage/canvas.html#dom-offscreencanvas-offscreencanvas>).
fn offscreen_canvas_new(ctx: Ctx<'_>, width: f64, height: f64) -> Result<f64> {
    let world = world(&ctx)?;
    let (realm, document) = {
        let world = world.borrow();
        (world.realm_id(), world.main_document_id())
    };
    let id = world.borrow().registry().borrow_mut().offscreen_canvas(
        realm,
        document,
        canvas_u32(width),
        canvas_u32(height),
    );
    Ok(crate::js::bridge::js_number(id.0))
}

/// The bitmap id's width or height
/// (<https://html.spec.whatwg.org/multipage/canvas.html#dom-offscreencanvas-width>).
fn offscreen_canvas_width(ctx: Ctx<'_>, raw: f64) -> Result<u32> {
    let id = canvas_id_argument(&ctx, raw)?;
    with_offscreen_registry(&ctx, id, |registry| {
        Ok(registry.canvas_dimensions(id).map_or(0, |(width, _)| width))
    })
}

fn offscreen_canvas_height(ctx: Ctx<'_>, raw: f64) -> Result<u32> {
    let id = canvas_id_argument(&ctx, raw)?;
    with_offscreen_registry(&ctx, id, |registry| {
        Ok(registry
            .canvas_dimensions(id)
            .map_or(0, |(_, height)| height))
    })
}

/// Replaces the bitmap with a new transparent one of the requested width
/// (<https://html.spec.whatwg.org/multipage/canvas.html#dom-offscreencanvas-width>).
fn offscreen_canvas_set_width(ctx: Ctx<'_>, raw: f64, value: f64) -> Result<()> {
    let id = canvas_id_argument(&ctx, raw)?;
    let width = canvas_u32(value);
    with_offscreen_registry(&ctx, id, |registry| {
        let height = registry
            .canvas_dimensions(id)
            .map_or(0, |(_, height)| height);
        registry.resize_canvas(id, width, height);
        Ok(())
    })
}

fn offscreen_canvas_set_height(ctx: Ctx<'_>, raw: f64, value: f64) -> Result<()> {
    let id = canvas_id_argument(&ctx, raw)?;
    let height = canvas_u32(value);
    with_offscreen_registry(&ctx, id, |registry| {
        let width = registry.canvas_dimensions(id).map_or(0, |(width, _)| width);
        registry.resize_canvas(id, width, height);
        Ok(())
    })
}

/// `OffscreenCanvas.getContext("2d")`: create the context object on the bitmap
/// (<https://html.spec.whatwg.org/multipage/canvas.html#dom-offscreencanvas-getcontext>).
fn offscreen_canvas_context<'js>(
    ctx: Ctx<'js>,
    raw: f64,
    options: Opt<Value<'js>>,
) -> Result<Value<'js>> {
    let id = canvas_id_argument(&ctx, raw)?;
    // The dictionary converts before the context mode changes, so a throwing
    // getter leaves the canvas untouched.
    read_context_options(&ctx, options.0.as_ref())?;
    let existing = with_offscreen_registry(&ctx, id, |registry| Ok(registry.canvas_mode(id)))?;
    match existing {
        Some(crate::js::world::CanvasMode::Detached) => {
            return Err(crate::js::bindings::throw_dom(
                &ctx,
                "InvalidStateError",
                "the OffscreenCanvas is detached",
            ));
        }
        Some(crate::js::world::CanvasMode::None) | None => {}
        Some(crate::js::world::CanvasMode::TwoD | crate::js::world::CanvasMode::Bitmap) => {
            // The shim returns the same context object it stored, so a second
            // request never reaches here.
            return Ok(Value::new_null(ctx.clone()));
        }
    }
    let context = context_object(&ctx, id)?;
    with_offscreen_registry(&ctx, id, |registry| {
        registry.set_canvas_mode(id, crate::js::world::CanvasMode::TwoD);
        Ok(())
    })?;
    Ok(context)
}

/// The transfer steps unset the sender's bitmap
/// (<https://html.spec.whatwg.org/multipage/structured-data.html#transferable-objects>).
fn offscreen_canvas_detach(ctx: Ctx<'_>, raw: f64) -> Result<()> {
    let id = canvas_id_argument(&ctx, raw)?;
    with_offscreen_registry(&ctx, id, |registry| {
        registry.detach_canvas(id);
        Ok(())
    })
}

/// Runs `run` against the registry after checking that this realm owns `id`.
fn with_offscreen_registry<R>(
    ctx: &Ctx<'_>,
    id: CanvasId,
    run: impl FnOnce(&mut crate::js::world::RealmRegistry) -> Result<R>,
) -> Result<R> {
    let world = world(ctx)?;
    let registry = world.borrow().registry();
    let mut registry = registry.borrow_mut();
    if !registry.owns_canvas(id, &world.borrow()) {
        return Err(crate::js::bindings::throw_dom(
            ctx,
            "InvalidStateError",
            "the canvas does not belong to this realm",
        ));
    }
    run(&mut registry)
}

/// `CanvasGradient` and `CanvasPattern` constructors: canvas returns branded
/// instances, and the globals make `instanceof` and prototype checks work
/// (<https://html.spec.whatwg.org/multipage/canvas.html#canvasgradient>).
fn install_gradient_objects(ctx: &Ctx<'_>) -> Result<()> {
    for name in ["CanvasGradient", "CanvasPattern"] {
        let prototype = Object::new(ctx.clone())?;
        if name == "CanvasGradient" {
            prototype.set("addColorStop", Func::from(add_color_stop))?;
        } else {
            prototype.set("setTransform", Func::from(set_pattern_transform))?;
        }
        let ctor = Function::new(ctx.clone(), || {})?;
        ctor.set("prototype", prototype.clone())?;
        prototype.set("constructor", ctor.clone())?;
        ctx.globals().set(name, ctor)?;
    }
    Ok(())
}

/// `TextMetrics` and `TextCluster` interface objects. Both are non-constructible
/// platform interfaces, so the global is a throwing constructor and the
/// prototypes carry the members. The prototypes live in the realm's brand map,
/// so a cluster from an iframe never resolves against another realm's
/// prototype (the rquickjs `Class::instance` cache is runtime-wide).
fn install_text_metrics_objects(ctx: &Ctx<'_>) -> Result<()> {
    let metrics = Object::new(ctx.clone())?;
    for (name, getter) in [
        (
            "width",
            metrics_width as fn(This<Object<'_>>) -> Result<f64>,
        ),
        ("actualBoundingBoxLeft", metrics_actual_bounding_box_left),
        ("actualBoundingBoxRight", metrics_actual_bounding_box_right),
        (
            "actualBoundingBoxAscent",
            metrics_actual_bounding_box_ascent,
        ),
        (
            "actualBoundingBoxDescent",
            metrics_actual_bounding_box_descent,
        ),
        ("fontBoundingBoxAscent", metrics_font_bounding_box_ascent),
        ("fontBoundingBoxDescent", metrics_font_bounding_box_descent),
        ("emHeightAscent", metrics_em_height_ascent),
        ("emHeightDescent", metrics_em_height_descent),
        ("hangingBaseline", metrics_hanging_baseline),
        ("alphabeticBaseline", metrics_alphabetic_baseline),
        ("ideographicBaseline", metrics_ideographic_baseline),
    ] {
        metrics.prop(name, Accessor::from(getter))?;
    }
    metrics.set("getTextClusters", Func::from(get_text_clusters))?;
    metrics.set("getActualBoundingBox", Func::from(get_actual_bounding_box))?;
    metrics.set("getSelectionRects", Func::from(get_selection_rects))?;
    metrics.set("getIndexFromOffset", Func::from(get_index_from_offset))?;

    let cluster = Object::new(ctx.clone())?;
    cluster.prop("begin", Accessor::from(cluster_begin))?;
    // `TextCluster.start` is the name the current WPT suite uses; the explainer
    // IDL calls it `begin`. Expose both rather than pick a side.
    cluster.prop("start", Accessor::from(cluster_begin))?;
    cluster.prop("end", Accessor::from(cluster_end))?;
    cluster.prop("x", Accessor::from(cluster_x))?;
    cluster.prop("y", Accessor::from(cluster_y))?;
    cluster.prop("text", Accessor::from(cluster_text))?;
    cluster.prop("align", Accessor::from(cluster_align))?;
    cluster.prop("baseline", Accessor::from(cluster_baseline))?;

    install_interface_object(ctx, "TextMetrics", metrics)?;
    install_interface_object(ctx, "TextCluster", cluster)?;
    Ok(())
}

/// Publishes a non-constructible platform interface: a throwing constructor
/// whose `prototype` is the member object, and a realm-local brand entry that
/// instance creation looks up.
fn install_interface_object<'js>(ctx: &Ctx<'js>, name: &str, prototype: Object<'js>) -> Result<()> {
    let ctor = Function::new(ctx.clone(), illegal_constructor)?;
    ctor.prop("name", Property::from(name).configurable())?;
    ctor.set("prototype", prototype.clone())?;
    prototype.set("constructor", ctor.clone())?;
    prototype.prop(
        PredefinedAtom::SymbolToStringTag,
        Property::from(name).configurable(),
    )?;
    ctx.globals().set(name, ctor)?;
    world(ctx)?
        .borrow_mut()
        .intern_brand(name, rquickjs::Persistent::save(ctx, prototype));
    Ok(())
}

fn illegal_constructor(ctx: Ctx<'_>) -> Result<Value<'_>> {
    Err(Exception::throw_type(&ctx, "illegal constructor"))
}

/// One 2D context object for `id`.
pub(crate) fn context_object<'js>(ctx: &Ctx<'js>, id: CanvasId) -> Result<Value<'js>> {
    let ctor: Function = ctx.globals().get("CanvasRenderingContext2D")?;
    let prototype: Object = ctor.get("prototype")?;
    let object = Object::new(ctx.clone())?;
    object.set_prototype(Some(&prototype))?;
    object.set("__tbCanvasId", id.0)?;
    let resources = Object::new(ctx.clone())?;
    object.set("__tbRes", resources)?;
    Ok(object.into_value())
}

/// Converts `getContext` options to `CanvasRenderingContext2DSettings` in
/// `WebIDL` dictionary member order (lexicographic by member name). The settings
/// are not stored yet, but conversion order is observable through getters and
/// must match
/// (<https://webidl.spec.whatwg.org/#es-dictionary>).
pub(crate) fn read_context_options<'js>(
    ctx: &Ctx<'js>,
    options: Option<&Value<'js>>,
) -> Result<()> {
    let Some(options) = options else {
        return Ok(());
    };
    if options.is_undefined() || options.is_null() {
        return Ok(());
    }
    // `getContext` takes `any` for options: primitives are ignored rather than
    // converted as a dictionary
    // (<https://html.spec.whatwg.org/multipage/canvas.html#dom-canvas-getcontext>).
    let Some(object) = options.as_object() else {
        return Ok(());
    };
    for name in [
        "alpha",
        "colorSpace",
        "colorType",
        "desynchronized",
        "willReadFrequently",
    ] {
        let value: Value = object.get(name)?;
        if value.is_undefined() {
            continue;
        }
        match name {
            "alpha" | "desynchronized" | "willReadFrequently" => {
                let _ = Coerced::<bool>::from_js(ctx, value)?;
            }
            "colorSpace" => {
                let text = Coerced::<rquickjs::String>::from_js(ctx, value)?.to_string()?;
                if !matches!(
                    text.as_str(),
                    "srgb" | "srgb-linear" | "display-p3" | "display-p3-linear"
                ) {
                    return Err(Exception::throw_type(
                        ctx,
                        "unknown PredefinedColorSpace value",
                    ));
                }
            }
            "colorType" => {
                let text = Coerced::<rquickjs::String>::from_js(ctx, value)?.to_string()?;
                if !matches!(text.as_str(), "unorm8" | "float16") {
                    return Err(Exception::throw_type(ctx, "unknown CanvasColorType value"));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn install_members<'js>(ctx: &Ctx<'js>, prototype: &Object<'js>) -> Result<()> {
    install_methods(ctx, prototype)?;
    install_accessors(prototype)
}

fn install_methods<'js>(ctx: &Ctx<'js>, prototype: &Object<'js>) -> Result<()> {
    macro_rules! method {
        ($name:literal, $length:expr, $function:expr) => {
            method(ctx, prototype, $name, $length, $function)?;
        };
    }

    method!("save", 0, save);
    method!("restore", 0, restore);
    method!("reset", 0, reset);
    method!("beginLayer", 0, begin_layer);
    method!("endLayer", 0, end_layer);
    method!("fillTextCluster", 2, fill_text_cluster);
    method!("strokeTextCluster", 2, stroke_text_cluster);
    method!("scale", 2, scale);
    method!("rotate", 1, rotate);
    method!("translate", 2, translate);
    method!("transform", 6, transform);
    method!("setTransform", 0, set_transform);
    method!("resetTransform", 0, reset_transform);
    method!("getTransform", 0, get_transform);

    method!("beginPath", 0, begin_path);
    method!("closePath", 0, close_path);
    method!("moveTo", 2, move_to);
    method!("lineTo", 2, line_to);
    method!("quadraticCurveTo", 4, quadratic_curve_to);
    method!("bezierCurveTo", 6, bezier_curve_to);
    method!("arc", 5, arc);
    method!("arcTo", 5, arc_to);
    method!("ellipse", 7, ellipse);
    method!("rect", 4, rect);
    method!("roundRect", 3, round_rect);

    method!("fill", 0, fill);
    method!("stroke", 0, stroke);
    method!("clip", 0, clip);
    method!("isPointInPath", 2, is_point_in_path);
    method!("isPointInStroke", 2, is_point_in_stroke);
    method!("fillRect", 4, fill_rect);
    method!("strokeRect", 4, stroke_rect);
    method!("clearRect", 4, clear_rect);

    method!("getLineDash", 0, get_line_dash);
    method!("setLineDash", 1, set_line_dash);

    method!("createLinearGradient", 4, create_linear_gradient);
    method!("createRadialGradient", 6, create_radial_gradient);
    method!("createConicGradient", 3, create_conic_gradient);
    method!("createPattern", 2, create_pattern);

    method!("getImageData", 4, get_image_data);
    method!("putImageData", 3, put_image_data);
    method!("createImageData", 2, create_image_data);

    method!("drawImage", 3, draw_image);

    method!("fillText", 3, fill_text);
    method!("strokeText", 3, stroke_text);
    method!("measureText", 1, measure_text);
    method!("drawFocusIfNeeded", 1, noop);

    Ok(())
}

macro_rules! number_prop {
    ($prototype:expr, $name:literal, $get:ident, $set:ident) => {
        $prototype.prop(
            $name,
            Accessor::new(
                |this: This<Object<'_>>| with_canvas(this.0.ctx(), &this.0, |canvas| canvas.$get()),
                |this: This<Object<'_>>, value: Coerced<f64>| {
                    with_canvas(this.0.ctx(), &this.0, |canvas| {
                        canvas.$set(f32_of(*value));
                    })
                },
            ),
        )?;
    };
}

macro_rules! string_prop {
    ($prototype:expr, $name:literal, $get:ident, $set:ident) => {
        $prototype.prop(
            $name,
            Accessor::new(
                |this: This<Object<'_>>| {
                    with_canvas(this.0.ctx(), &this.0, |canvas| canvas.$get().to_owned())
                },
                |this: This<Object<'_>>, value: DomString| {
                    with_canvas(this.0.ctx(), &this.0, |canvas| canvas.$set(&value))
                },
            ),
        )?;
    };
}
fn install_accessors(prototype: &Object<'_>) -> Result<()> {
    prototype.prop("canvas", Accessor::from(canvas_object))?;
    install_paint_accessors(prototype)?;
    install_text_accessors(prototype)?;
    prototype.prop(
        "imageSmoothingEnabled",
        Accessor::new(
            |this: This<Object<'_>>| {
                with_canvas(this.0.ctx(), &this.0, |canvas| {
                    canvas.image_smoothing_enabled()
                })
            },
            |this: This<Object<'_>>, value: Coerced<bool>| {
                with_canvas(this.0.ctx(), &this.0, |canvas| {
                    canvas.set_image_smoothing_enabled(*value);
                })
            },
        ),
    )?;
    string_prop!(
        prototype,
        "imageSmoothingQuality",
        image_smoothing_quality,
        set_image_smoothing_quality
    );
    prototype.prop("filter", Accessor::new(get_filter, set_filter_binding))?;
    Ok(())
}

fn install_paint_accessors(prototype: &Object<'_>) -> Result<()> {
    prototype.prop("fillStyle", Accessor::new(get_fill_style, set_fill_style))?;
    prototype.prop(
        "strokeStyle",
        Accessor::new(get_stroke_style, set_stroke_style),
    )?;
    number_prop!(prototype, "lineWidth", line_width, set_line_width);
    string_prop!(prototype, "lineCap", line_cap, set_line_cap);
    string_prop!(prototype, "lineJoin", line_join, set_line_join);
    number_prop!(prototype, "miterLimit", miter_limit, set_miter_limit);
    number_prop!(
        prototype,
        "lineDashOffset",
        line_dash_offset,
        set_line_dash_offset
    );
    number_prop!(prototype, "globalAlpha", global_alpha, set_global_alpha);
    string_prop!(
        prototype,
        "globalCompositeOperation",
        global_composite_operation,
        set_global_composite_operation
    );
    number_prop!(prototype, "shadowBlur", shadow_blur, set_shadow_blur);
    prototype.prop(
        "shadowColor",
        Accessor::new(
            |this: This<Object<'_>>| {
                with_canvas(this.0.ctx(), &this.0, |canvas| {
                    match canvas.shadow_serialized() {
                        Some(text) => text.to_owned(),
                        None => serialize_color(canvas.shadow_color()),
                    }
                })
            },
            |this: This<Object<'_>>, value: DomString| {
                let color = css_color_exact(this.0.ctx(), &this.0, &value)?;
                with_canvas(this.0.ctx(), &this.0, |canvas| {
                    if let Some((color, text)) = color {
                        canvas.set_shadow_color_exact(color, text);
                    }
                })
            },
        ),
    )?;
    number_prop!(
        prototype,
        "shadowOffsetX",
        shadow_offset_x,
        set_shadow_offset_x
    );
    number_prop!(
        prototype,
        "shadowOffsetY",
        shadow_offset_y,
        set_shadow_offset_y
    );
    Ok(())
}

fn install_text_accessors(prototype: &Object<'_>) -> Result<()> {
    prototype.prop("font", Accessor::new(get_font, set_font_binding))?;
    string_prop!(prototype, "textAlign", text_align, set_text_align);
    string_prop!(prototype, "textBaseline", text_baseline, set_text_baseline);
    string_prop!(prototype, "direction", direction, set_direction);
    string_prop!(
        prototype,
        "letterSpacing",
        letter_spacing,
        set_letter_spacing
    );
    string_prop!(prototype, "wordSpacing", word_spacing, set_word_spacing);
    string_prop!(prototype, "fontKerning", font_kerning, set_font_kerning);
    string_prop!(prototype, "fontStretch", font_stretch, set_font_stretch);
    string_prop!(prototype, "lang", lang, set_lang);
    string_prop!(
        prototype,
        "textRendering",
        text_rendering,
        set_text_rendering
    );
    string_prop!(
        prototype,
        "fontVariantCaps",
        font_variant_caps,
        set_font_variant_caps
    );
    Ok(())
}

fn method<'js, F, A>(
    ctx: &Ctx<'js>,
    prototype: &Object<'js>,
    name: &str,
    length: usize,
    function: F,
) -> Result<()>
where
    F: rquickjs::function::IntoJsFunc<'js, A> + 'js,
{
    let function = Function::new(ctx.clone(), function)?
        .with_name(name)?
        .with_length(length)?;
    prototype.set(name, function)?;
    Ok(())
}

#[expect(clippy::unnecessary_wraps, reason = "rquickjs callbacks return Result")]
fn noop(_: This<Object<'_>>) -> Result<()> {
    Ok(())
}

// ---- state access --------------------------------------------------------

fn context_canvas_id(ctx: &Ctx<'_>, object: &Object<'_>) -> Result<CanvasId> {
    let id = CanvasId(object.get("__tbCanvasId")?);
    // The id is a claim like any page-writable property, so the registry
    // decides whether this realm owns it. An element slot must still be a
    // `<canvas>` in a document this realm created; an offscreen slot must
    // belong to this realm, which is what lets a worker with no document use
    // one.
    let world = world(ctx)?;
    let registry = world.borrow().registry();
    if !registry.borrow().owns_canvas(id, &world.borrow()) {
        return Err(crate::js::bindings::throw_dom(
            ctx,
            "InvalidStateError",
            "canvas does not belong to this frame",
        ));
    }
    Ok(id)
}

/// The element behind a 2D context, or `None` for an `OffscreenCanvas`.
fn context_canvas_node(ctx: &Ctx<'_>, object: &Object<'_>) -> Result<Option<NodeId>> {
    let id = context_canvas_id(ctx, object)?;
    let world = world(ctx)?;
    let registry = world.borrow().registry();
    Ok(registry.borrow().canvas_node(id))
}

/// Rejects members that cannot run while a layer is open
/// (<https://html.spec.whatwg.org/multipage/canvas.html#dom-context-2d-beginlayer>).
fn reject_open_layer(ctx: &Ctx<'_>, object: &Object<'_>) -> Result<()> {
    if with_canvas(ctx, object, |canvas| canvas.has_open_layer())? {
        return Err(crate::js::bindings::throw_dom(
            ctx,
            "InvalidStateError",
            "a layer is open",
        ));
    }
    Ok(())
}

fn with_canvas<R>(
    ctx: &Ctx<'_>,
    object: &Object<'_>,
    run: impl FnOnce(&mut Canvas2D) -> R,
) -> Result<R> {
    let id = context_canvas_id(ctx, object)?;
    // A content attribute change resets the bitmap even when it bypassed the
    // IDL setter, so follow the element's current size before every use
    // (<https://html.spec.whatwg.org/multipage/canvas.html#the-canvas-element>).
    let world = world(ctx)?;
    let registry = world.borrow().registry();
    let mut registry = registry.borrow_mut();
    // A content attribute change resets the bitmap even when it bypassed the
    // IDL setter, so follow the element's current size before every use
    // (<https://html.spec.whatwg.org/multipage/canvas.html#the-canvas-element>).
    if let Some(node) = registry.canvas_node(id) {
        let dimensions = (
            canvas_dimension(ctx, node, "width", 300),
            canvas_dimension(ctx, node, "height", 150),
        );
        if registry.canvas_dimensions(id) != Some(dimensions) {
            registry.resize_canvas(id, dimensions.0, dimensions.1);
        }
    }
    let canvas = registry.canvas_mut(id).ok_or_else(|| {
        crate::js::bindings::throw_dom(ctx, "InvalidStateError", "the canvas is not usable")
    })?;
    Ok(run(canvas))
}

/// A canvas content attribute as a bitmap dimension
/// (<https://html.spec.whatwg.org/multipage/canvas.html#dom-canvas-width>).
pub(crate) fn canvas_dimension(ctx: &Ctx<'_>, id: NodeId, attribute: &str, default: u32) -> u32 {
    let value = world(ctx)
        .ok()
        .and_then(|owner| {
            owner.borrow().with_document(id, |parsed| {
                crate::dom::tree::attr(&parsed.document.base, id.node, attribute).map(str::to_owned)
            })
        })
        .flatten()
        .unwrap_or_default();
    crate::dom::parse::parse_non_negative_integer(&value).unwrap_or(default)
}

fn f32_of(value: f64) -> f32 {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "canvas rasterizes coordinates at f32 precision, like Skia"
    )]
    let value = value as f32;
    value
}

/// `WebIDL` `[EnforceRange] long`: reject non-finite values and values outside
/// the long range with `TypeError`, then truncate.
fn enforce_long(ctx: &Ctx<'_>, value: f64) -> Result<i32> {
    if !value.is_finite() {
        return Err(Exception::throw_type(ctx, "value must be finite"));
    }
    let value = value.trunc();
    if value < f64::from(i32::MIN) || value > f64::from(i32::MAX) {
        return Err(Exception::throw_type(
            ctx,
            "value is outside the long range",
        ));
    }
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the value is finite, integral, and inside the i32 range"
    )]
    Ok(value as i32)
}

fn i32_of(value: f64) -> i32 {
    if !value.is_finite() {
        return 0;
    }
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the value is finite and clamped by the IDL long conversion below"
    )]
    let value = value.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32;
    value
}

// ---- state methods -------------------------------------------------------

fn save(this: This<Object<'_>>) -> Result<()> {
    with_canvas(this.0.ctx(), &this.0, state::Canvas2D::save)
}

fn restore(this: This<Object<'_>>) -> Result<()> {
    let restored = with_canvas(this.0.ctx(), &this.0, state::Canvas2D::restore)?;
    if restored {
        Ok(())
    } else {
        Err(crate::js::bindings::throw_dom(
            this.0.ctx(),
            "InvalidStateError",
            "restore() cannot cross a layer boundary",
        ))
    }
}

fn begin_layer<'js>(this: This<Object<'js>>, options: Opt<Value<'js>>) -> Result<()> {
    let ctx = this.0.ctx();
    let mut matrix = None;
    if let Some(value) = options.0 {
        if value.is_null() || value.is_undefined() {
            return with_canvas(ctx, &this.0, state::Canvas2D::begin_layer);
        }
        if !value.is_object() {
            return Err(Exception::throw_type(
                ctx,
                "beginLayer options must be an object",
            ));
        }
        if let Some(object) = value.as_object()
            && let Ok(filter) = object.get::<_, Value>("filter")
            && filter.is_object()
            && !filter.is_null()
        {
            // Validate before reading `name`/`type`/`values`, so an invalid
            // filter throws before any page getter runs.
            if let Ok(validate) = ctx.globals().get::<_, Function>("__tbValidateFilter") {
                validate.call::<_, ()>((filter.clone(),))?;
            }
            matrix = color_matrix_from_filter(&filter);
        }
    }
    with_canvas(ctx, &this.0, |canvas| {
        canvas.begin_layer_filtered(matrix);
    })
}

fn end_layer(this: This<Object<'_>>) -> Result<()> {
    let closed = with_canvas(this.0.ctx(), &this.0, state::Canvas2D::end_layer)?;
    if closed {
        Ok(())
    } else {
        Err(crate::js::bindings::throw_dom(
            this.0.ctx(),
            "InvalidStateError",
            "endLayer() without a matching beginLayer()",
        ))
    }
}

fn reset(this: This<Object<'_>>) -> Result<()> {
    with_canvas(this.0.ctx(), &this.0, state::Canvas2D::reset)
}

fn scale(this: This<Object<'_>>, x: Coerced<f64>, y: Coerced<f64>) -> Result<()> {
    with_canvas(this.0.ctx(), &this.0, |canvas| {
        canvas.scale(f32_of(*x), f32_of(*y));
    })
}

fn rotate(this: This<Object<'_>>, angle: Coerced<f64>) -> Result<()> {
    with_canvas(this.0.ctx(), &this.0, |canvas| {
        canvas.rotate(f32_of(*angle));
    })
}

fn translate(this: This<Object<'_>>, x: Coerced<f64>, y: Coerced<f64>) -> Result<()> {
    with_canvas(this.0.ctx(), &this.0, |canvas| {
        canvas.translate(f32_of(*x), f32_of(*y));
    })
}

fn transform(
    this: This<Object<'_>>,
    m11: Coerced<f64>,
    m12: Coerced<f64>,
    m21: Coerced<f64>,
    m22: Coerced<f64>,
    dx: Coerced<f64>,
    dy: Coerced<f64>,
) -> Result<()> {
    with_canvas(this.0.ctx(), &this.0, |canvas| {
        canvas.transform(
            f32_of(*m11),
            f32_of(*m12),
            f32_of(*m21),
            f32_of(*m22),
            f32_of(*dx),
            f32_of(*dy),
        );
    })
}

/// Reads a `DOMMatrix2DInit` dictionary. Each member is read once, in
/// lexicographic order, `undefined` falls back to the default, and the
/// `m11`/`m12`/... aliases override `a`/`b`/...
/// (<https://drafts.fxtf.org/geometry/#dictdef-dommatrix2dinit>).
#[expect(
    clippy::many_single_char_names,
    reason = "the names are the DOMMatrix2DInit members a through f"
)]
fn matrix_2d_init<'js>(ctx: &Ctx<'js>, value: &Value<'js>) -> Result<Option<tiny_skia::Transform>> {
    if value.is_undefined() || value.is_null() {
        return Ok(Some(tiny_skia::Transform::identity()));
    }
    let Some(object) = value.as_object() else {
        return Err(Exception::throw_type(
            ctx,
            "DOMMatrix2DInit must be an object",
        ));
    };
    let read = |name: &str| -> Result<Option<f64>> {
        let value: Value<'js> = object.get(name)?;
        if value.is_undefined() {
            return Ok(None);
        }
        Ok(Some(Coerced::<f64>::from_js(ctx, value)?.0))
    };
    let a = read("a")?;
    let b = read("b")?;
    let c = read("c")?;
    let d = read("d")?;
    let e = read("e")?;
    let f = read("f")?;
    let m11 = read("m11")?;
    let m12 = read("m12")?;
    let m21 = read("m21")?;
    let m22 = read("m22")?;
    let m41 = read("m41")?;
    let m42 = read("m42")?;
    // The two spellings may both appear only when each alias agrees
    // (<https://drafts.csswg.org/geometry/#matrix-validate-and-fixup-2d>).
    // SameValueZero compares NaN equal and +0 equal to -0
    // (<https://tc39.es/ecma262/#sec-samevaluezero>).
    let same_value_zero = |left: f64, right: f64| {
        let canonical = |value: f64| {
            if value.is_nan() {
                f64::NAN.to_bits()
            } else if value.abs().to_bits() == 0 {
                0
            } else {
                value.to_bits()
            }
        };
        canonical(left) == canonical(right)
    };
    if [(a, m11), (b, m12), (c, m21), (d, m22), (e, m41), (f, m42)]
        .into_iter()
        .any(|(short, long)| match (short, long) {
            (Some(short), Some(long)) => !same_value_zero(short, long),
            _ => false,
        })
    {
        return Err(Exception::throw_type(
            ctx,
            "DOMMatrix2DInit a..f and m11..m42 disagree",
        ));
    }
    let values = [
        m11.or(a).unwrap_or(1.0),
        m12.or(b).unwrap_or(0.0),
        m21.or(c).unwrap_or(0.0),
        m22.or(d).unwrap_or(1.0),
        m41.or(e).unwrap_or(0.0),
        m42.or(f).unwrap_or(0.0),
    ];
    if values.iter().any(|value| !value.is_finite()) {
        return Ok(None);
    }
    Ok(Some(tiny_skia::Transform::from_row(
        f32_of(values[0]),
        f32_of(values[1]),
        f32_of(values[2]),
        f32_of(values[3]),
        f32_of(values[4]),
        f32_of(values[5]),
    )))
}

#[expect(
    clippy::many_single_char_names,
    reason = "the arguments are the six DOMMatrix2DInit components"
)]
fn set_transform<'js>(this: This<Object<'js>>, args: Rest<Value<'js>>) -> Result<()> {
    let ctx = this.0.ctx();
    let transform = match args.0.as_slice() {
        [] => Some(tiny_skia::Transform::identity()),
        [dictionary] => matrix_2d_init(ctx, dictionary)?,
        [a, b, c, d, e, f] => {
            let numbers = [
                Coerced::<f64>::from_js(ctx, a.clone())?.0,
                Coerced::<f64>::from_js(ctx, b.clone())?.0,
                Coerced::<f64>::from_js(ctx, c.clone())?.0,
                Coerced::<f64>::from_js(ctx, d.clone())?.0,
                Coerced::<f64>::from_js(ctx, e.clone())?.0,
                Coerced::<f64>::from_js(ctx, f.clone())?.0,
            ];
            if numbers.iter().any(|value| !value.is_finite()) {
                return Ok(());
            }
            Some(tiny_skia::Transform::from_row(
                f32_of(numbers[0]),
                f32_of(numbers[1]),
                f32_of(numbers[2]),
                f32_of(numbers[3]),
                f32_of(numbers[4]),
                f32_of(numbers[5]),
            ))
        }
        _ => {
            return Err(Exception::throw_type(
                ctx,
                "setTransform needs six numbers or one dictionary",
            ));
        }
    };
    let Some(transform) = transform else {
        return Ok(());
    };
    with_canvas(ctx, &this.0, |canvas| {
        canvas.set_transform(
            transform.sx,
            transform.ky,
            transform.kx,
            transform.sy,
            transform.tx,
            transform.ty,
        );
    })
}

fn reset_transform(this: This<Object<'_>>) -> Result<()> {
    with_canvas(this.0.ctx(), &this.0, state::Canvas2D::reset_transform)
}

fn get_transform(this: This<Object<'_>>) -> Result<Object<'_>> {
    let ctx = this.0.ctx();
    let transform = with_canvas(ctx, &this.0, |canvas| canvas.get_transform())?;
    if let Ok(ctor) = ctx.globals().get::<_, Function>("DOMMatrix")
        && let Ok(prototype) = ctor.get::<_, Object>("prototype")
    {
        let object = Object::new(ctx.clone())?;
        object.set_prototype(Some(&prototype))?;
        let init = Array::new(ctx.clone())?;
        for value in [
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        ] {
            init.set(init.len(), value)?;
        }
        init.set(0, transform.sx)?;
        init.set(1, transform.ky)?;
        init.set(4, transform.kx)?;
        init.set(5, transform.sy)?;
        init.set(12, transform.tx)?;
        init.set(13, transform.ty)?;
        object.set("_m", init)?;
        return Ok(object);
    }
    let object = Object::new(ctx.clone())?;
    object.set("a", transform.sx)?;
    object.set("b", transform.ky)?;
    object.set("c", transform.kx)?;
    object.set("d", transform.sy)?;
    object.set("e", transform.tx)?;
    object.set("f", transform.ty)?;
    object.set("is2D", true)?;
    object.set("isIdentity", transform.is_identity())?;
    Ok(object)
}

// ---- path methods --------------------------------------------------------

fn begin_path(this: This<Object<'_>>) -> Result<()> {
    with_canvas(this.0.ctx(), &this.0, state::Canvas2D::begin_path)
}

fn close_path(this: This<Object<'_>>) -> Result<()> {
    with_canvas(this.0.ctx(), &this.0, state::Canvas2D::close_path)
}

fn move_to(this: This<Object<'_>>, x: Coerced<f64>, y: Coerced<f64>) -> Result<()> {
    with_canvas(this.0.ctx(), &this.0, |canvas| {
        canvas.move_to(f32_of(*x), f32_of(*y));
    })
}

fn line_to(this: This<Object<'_>>, x: Coerced<f64>, y: Coerced<f64>) -> Result<()> {
    with_canvas(this.0.ctx(), &this.0, |canvas| {
        canvas.line_to(f32_of(*x), f32_of(*y));
    })
}

fn quadratic_curve_to(
    this: This<Object<'_>>,
    cx: Coerced<f64>,
    cy: Coerced<f64>,
    x: Coerced<f64>,
    y: Coerced<f64>,
) -> Result<()> {
    with_canvas(this.0.ctx(), &this.0, |canvas| {
        canvas.quadratic_to(f32_of(*cx), f32_of(*cy), f32_of(*x), f32_of(*y));
    })
}

fn bezier_curve_to(
    this: This<Object<'_>>,
    c1x: Coerced<f64>,
    c1y: Coerced<f64>,
    c2x: Coerced<f64>,
    c2y: Coerced<f64>,
    x: Coerced<f64>,
    y: Coerced<f64>,
) -> Result<()> {
    with_canvas(this.0.ctx(), &this.0, |canvas| {
        canvas.cubic_to(
            f32_of(*c1x),
            f32_of(*c1y),
            f32_of(*c2x),
            f32_of(*c2y),
            f32_of(*x),
            f32_of(*y),
        );
    })
}

fn arc(
    this: This<Object<'_>>,
    x: Coerced<f64>,
    y: Coerced<f64>,
    radius: Coerced<f64>,
    start: Coerced<f64>,
    end: Coerced<f64>,
    counterclockwise: Opt<Coerced<bool>>,
) -> Result<()> {
    let counterclockwise = counterclockwise.0.is_some_and(|value| *value);
    if !(*x).is_finite()
        || !(*y).is_finite()
        || !(*radius).is_finite()
        || !(*start).is_finite()
        || !(*end).is_finite()
    {
        return Ok(());
    }
    if *radius < 0.0 {
        return Err(crate::js::bindings::throw_dom(
            this.0.ctx(),
            "IndexSizeError",
            "radius is negative",
        ));
    }
    with_canvas(this.0.ctx(), &this.0, |canvas| {
        canvas.arc(
            f32_of(*x),
            f32_of(*y),
            f32_of(*radius),
            f32_of(*start),
            f32_of(*end),
            counterclockwise,
        );
    })
}

fn arc_to(
    this: This<Object<'_>>,
    x1: Coerced<f64>,
    y1: Coerced<f64>,
    x2: Coerced<f64>,
    y2: Coerced<f64>,
    radius: Coerced<f64>,
) -> Result<()> {
    if !(*x1).is_finite()
        || !(*y1).is_finite()
        || !(*x2).is_finite()
        || !(*y2).is_finite()
        || !(*radius).is_finite()
    {
        return Ok(());
    }
    if *radius < 0.0 {
        return Err(crate::js::bindings::throw_dom(
            this.0.ctx(),
            "IndexSizeError",
            "radius is negative",
        ));
    }
    with_canvas(this.0.ctx(), &this.0, |canvas| {
        canvas.arc_to(
            f32_of(*x1),
            f32_of(*y1),
            f32_of(*x2),
            f32_of(*y2),
            f32_of(*radius),
        );
    })
}

fn ellipse<'js>(this: This<Object<'js>>, args: rquickjs::function::Rest<Value<'js>>) -> Result<()> {
    let ctx = this.0.ctx();
    let number = |index: usize| -> Result<f64> {
        let value = args
            .0
            .get(index)
            .ok_or_else(|| Exception::throw_type(ctx, "missing argument"))?;
        Ok(*Coerced::<f64>::from_js(ctx, value.clone())?)
    };
    let (x, y, radius_x, radius_y) = (number(0)?, number(1)?, number(2)?, number(3)?);
    let (rotation, start, end) = (number(4)?, number(5)?, number(6)?);
    let counterclockwise = args
        .0
        .get(7)
        .map(|value| Coerced::<bool>::from_js(ctx, value.clone()))
        .transpose()?
        .is_some_and(|value| *value);
    if ![x, y, radius_x, radius_y, rotation, start, end]
        .iter()
        .all(|value| value.is_finite())
    {
        return Ok(());
    }
    if radius_x < 0.0 || radius_y < 0.0 {
        return Err(crate::js::bindings::throw_dom(
            ctx,
            "IndexSizeError",
            "radius is negative",
        ));
    }
    with_canvas(ctx, &this.0, |canvas| {
        canvas.ellipse(
            f32_of(x),
            f32_of(y),
            f32_of(radius_x),
            f32_of(radius_y),
            f32_of(rotation),
            f32_of(start),
            f32_of(end),
            counterclockwise,
        );
    })
}

fn rect(
    this: This<Object<'_>>,
    x: Coerced<f64>,
    y: Coerced<f64>,
    width: Coerced<f64>,
    height: Coerced<f64>,
) -> Result<()> {
    with_canvas(this.0.ctx(), &this.0, |canvas| {
        canvas.rect(f32_of(*x), f32_of(*y), f32_of(*width), f32_of(*height));
    })
}

fn round_rect<'js>(
    this: This<Object<'js>>,
    x: Coerced<f64>,
    y: Coerced<f64>,
    width: Coerced<f64>,
    height: Coerced<f64>,
    radii: Opt<Value<'js>>,
) -> Result<()> {
    // Step 1 returns before the radii list is validated, so a non-finite
    // rectangle never throws for its corners
    // (<https://html.spec.whatwg.org/multipage/canvas.html#dom-context-2d-roundrect>).
    if [*x, *y, *width, *height]
        .iter()
        .any(|value| !value.is_finite())
    {
        return Ok(());
    }
    let radii = match radii.0 {
        Some(value) if !value.is_undefined() => round_rect_radii(&value)?,
        _ => None,
    };
    let radii = if let Some(radii) = radii {
        if radii.is_empty() || radii.len() > 4 {
            return Err(Exception::throw_range(
                this.0.ctx(),
                "Between one and four radii are necessary",
            ));
        }
        if radii
            .iter()
            .any(|(rx, ry)| !rx.is_finite() || !ry.is_finite())
        {
            return Ok(());
        }
        if radii.iter().any(|(rx, ry)| *rx < 0.0 || *ry < 0.0) {
            return Err(Exception::throw_range(this.0.ctx(), "radius is negative"));
        }
        radii
    } else {
        // An omitted radii argument uses one zero radius. It still draws the
        // rectangle and leaves the step-14 subpath at (x, y).
        vec![(0.0, 0.0)]
    };
    with_canvas(this.0.ctx(), &this.0, |canvas| {
        canvas.round_rect(
            f32_of(*x),
            f32_of(*y),
            f32_of(*width),
            f32_of(*height),
            &radii,
        );
    })
}

/// Reads the `roundRect` radii argument: one number or `DOMPointInit`, or a
/// sequence of either
/// (<https://html.spec.whatwg.org/multipage/canvas.html#dom-context-2d-roundrect>).
fn round_rect_radii(value: &Value<'_>) -> Result<Option<Vec<(f32, f32)>>> {
    let Some(array) = value.as_array() else {
        let Some(pair) = round_rect_radius(value)? else {
            return Ok(None);
        };
        return Ok(Some(vec![pair]));
    };
    let mut radii = Vec::new();
    for item in array.iter::<Value>() {
        let Some(pair) = round_rect_radius(&item?)? else {
            return Ok(None);
        };
        radii.push(pair);
    }
    Ok(Some(radii))
}

/// One radius: a number for both axes, or a `DOMPointInit` dictionary whose
/// missing members read zero.
fn round_rect_radius(value: &Value<'_>) -> Result<Option<(f32, f32)>> {
    if let Some(object) = value.as_object() {
        // WebIDL dictionary conversion: a missing or undefined member is 0,
        // not NaN from ToNumber(undefined).
        let read = |name: &str| -> Result<f32> {
            let member: Value = object.get(name)?;
            if member.is_undefined() || member.is_null() {
                return Ok(0.0);
            }
            let number = Coerced::<f64>::from_js(value.ctx(), member)?;
            Ok(f32_of(*number))
        };
        return Ok(Some((read("x")?, read("y")?)));
    }
    let number = Coerced::<f64>::from_js(value.ctx(), value.clone())?;
    let number = f32_of(*number);
    Ok(Some((number, number)))
}

// ---- painting ------------------------------------------------------------

/// `CanvasFillRule`: an absent rule is nonzero, and any other string is a
/// `TypeError`
/// (<https://html.spec.whatwg.org/multipage/canvas.html#canvasfillrule>).
fn fill_rule(ctx: &Ctx<'_>, rule: Option<&DomString>) -> Result<bool> {
    match rule {
        None => Ok(false),
        Some(rule) => match &**rule {
            "nonzero" | "undefined" => Ok(false),
            "evenodd" => Ok(true),
            _ => Err(Exception::throw_type(ctx, "invalid fill rule")),
        },
    }
}

fn fill<'js>(
    this: This<Object<'js>>,
    first: Opt<Value<'js>>,
    second: Opt<DomString>,
) -> Result<()> {
    let ctx = this.0.ctx();
    if let Some(value) = first.0 {
        if value.is_undefined() {
            if second.0.is_some() {
                return Err(Exception::throw_type(ctx, "fill expects a Path2D"));
            }
            return with_canvas(ctx, &this.0, |canvas| canvas.fill(false));
        }
        if let Ok(path) = Class::<JsPath2D>::from_value(&value) {
            let even_odd = fill_rule(ctx, second.0.as_ref())?;
            let path = path.borrow();
            return with_canvas(ctx, &this.0, |canvas| {
                canvas.fill_borrowed_path(&path.canvas.borrow(), even_odd);
            });
        }
        // The two-argument overload only accepts a Path2D first.
        if second.0.is_some() {
            return Err(Exception::throw_type(ctx, "fill expects a Path2D"));
        }
        let rule = DomString::from_js(ctx, value)?;
        let even_odd = fill_rule(ctx, Some(&rule))?;
        return with_canvas(ctx, &this.0, |canvas| canvas.fill(even_odd));
    }
    with_canvas(ctx, &this.0, |canvas| canvas.fill(false))
}

fn stroke<'js>(this: This<Object<'js>>, path: Opt<Value<'js>>) -> Result<()> {
    let ctx = this.0.ctx();
    if let Some(value) = path.0 {
        if value.is_undefined() {
            return with_canvas(ctx, &this.0, state::Canvas2D::stroke);
        }
        let Ok(path) = Class::<JsPath2D>::from_value(&value) else {
            return Err(Exception::throw_type(ctx, "stroke expects a Path2D"));
        };
        let path = path.borrow();
        return with_canvas(ctx, &this.0, |canvas| {
            canvas.stroke_borrowed_path(&path.canvas.borrow());
        });
    }
    with_canvas(ctx, &this.0, state::Canvas2D::stroke)
}

fn clip<'js>(
    this: This<Object<'js>>,
    first: Opt<Value<'js>>,
    second: Opt<DomString>,
) -> Result<()> {
    let ctx = this.0.ctx();
    if let Some(value) = first.0 {
        if value.is_undefined() {
            if second.0.is_some() {
                return Err(Exception::throw_type(ctx, "clip expects a Path2D"));
            }
            return with_canvas(ctx, &this.0, |canvas| canvas.clip(false));
        }
        if let Ok(path) = Class::<JsPath2D>::from_value(&value) {
            let even_odd = fill_rule(ctx, second.0.as_ref())?;
            let path = path.borrow();
            return with_canvas(ctx, &this.0, |canvas| {
                canvas.clip_borrowed_path(&path.canvas.borrow(), even_odd);
            });
        }
        if second.0.is_some() {
            return Err(Exception::throw_type(ctx, "clip expects a Path2D"));
        }
        let rule = DomString::from_js(ctx, value)?;
        let even_odd = fill_rule(ctx, Some(&rule))?;
        return with_canvas(ctx, &this.0, |canvas| canvas.clip(even_odd));
    }
    with_canvas(ctx, &this.0, |canvas| canvas.clip(false))
}

fn is_point_in_path<'js>(
    this: This<Object<'js>>,
    args: rquickjs::function::Rest<Value<'js>>,
) -> Result<bool> {
    let ctx = this.0.ctx();
    let (path, offset) = match args.0.first() {
        Some(value) => match Class::<JsPath2D>::from_value(value) {
            Ok(path) => (Some(path), 1),
            Err(_) => (None, 0),
        },
        None => (None, 0),
    };
    let x = args
        .0
        .get(offset)
        .ok_or_else(|| Exception::throw_type(ctx, "isPointInPath needs x"))?;
    let y = args
        .0
        .get(offset + 1)
        .ok_or_else(|| Exception::throw_type(ctx, "isPointInPath needs y"))?;
    let x = f32_of(*Coerced::<f64>::from_js(ctx, x.clone())?);
    let y = f32_of(*Coerced::<f64>::from_js(ctx, y.clone())?);
    let rule = args
        .0
        .get(offset + 2)
        .map(|value| DomString::from_js(ctx, value.clone()))
        .transpose()?;
    let even_odd = fill_rule(ctx, rule.as_ref())?;
    let Some(path) = path else {
        return with_canvas(ctx, &this.0, |canvas| {
            canvas.is_point_in_path(x, y, even_odd)
        });
    };
    let path = path.borrow();
    with_canvas(ctx, &this.0, |canvas| {
        canvas.is_point_in_borrowed_path(&path.canvas.borrow(), x, y, even_odd)
    })
}

fn is_point_in_stroke<'js>(
    this: This<Object<'js>>,
    args: rquickjs::function::Rest<Value<'js>>,
) -> Result<bool> {
    let ctx = this.0.ctx();
    let (path, offset) = match args.0.first() {
        Some(value) => match Class::<JsPath2D>::from_value(value) {
            Ok(path) => (Some(path), 1),
            Err(_) => (None, 0),
        },
        None => (None, 0),
    };
    let x = args
        .0
        .get(offset)
        .ok_or_else(|| Exception::throw_type(ctx, "isPointInStroke needs x"))?;
    let y = args
        .0
        .get(offset + 1)
        .ok_or_else(|| Exception::throw_type(ctx, "isPointInStroke needs y"))?;
    let x = f32_of(*Coerced::<f64>::from_js(ctx, x.clone())?);
    let y = f32_of(*Coerced::<f64>::from_js(ctx, y.clone())?);
    let Some(path) = path else {
        return with_canvas(ctx, &this.0, |canvas| canvas.is_point_in_stroke(x, y));
    };
    let path = path.borrow();
    with_canvas(ctx, &this.0, |canvas| {
        canvas.is_point_in_borrowed_stroke(&path.canvas.borrow(), x, y)
    })
}

fn fill_rect(
    this: This<Object<'_>>,
    x: Coerced<f64>,
    y: Coerced<f64>,
    width: Coerced<f64>,
    height: Coerced<f64>,
) -> Result<()> {
    with_canvas(this.0.ctx(), &this.0, |canvas| {
        canvas.fill_rect(f32_of(*x), f32_of(*y), f32_of(*width), f32_of(*height));
    })
}

fn stroke_rect(
    this: This<Object<'_>>,
    x: Coerced<f64>,
    y: Coerced<f64>,
    width: Coerced<f64>,
    height: Coerced<f64>,
) -> Result<()> {
    with_canvas(this.0.ctx(), &this.0, |canvas| {
        canvas.stroke_rect(f32_of(*x), f32_of(*y), f32_of(*width), f32_of(*height));
    })
}

fn clear_rect(
    this: This<Object<'_>>,
    x: Coerced<f64>,
    y: Coerced<f64>,
    width: Coerced<f64>,
    height: Coerced<f64>,
) -> Result<()> {
    with_canvas(this.0.ctx(), &this.0, |canvas| {
        canvas.clear_rect(f32_of(*x), f32_of(*y), f32_of(*width), f32_of(*height));
    })
}

// ---- line dash -----------------------------------------------------------

fn get_line_dash(this: This<Object<'_>>) -> Result<rquickjs::Array<'_>> {
    let dash = with_canvas(this.0.ctx(), &this.0, |canvas| canvas.line_dash())?;
    let array = rquickjs::Array::new(this.0.ctx().clone())?;
    for (index, value) in dash.into_iter().enumerate() {
        array.set(index, value)?;
    }
    Ok(array)
}

fn set_line_dash<'js>(this: This<Object<'js>>, values: Value<'js>) -> Result<()> {
    let Some(array) = values.as_array() else {
        return Ok(());
    };
    let mut dash = Vec::new();
    for value in array.iter::<Coerced<f64>>() {
        let value = f32_of(*value?);
        // An invalid entry rejects the whole sequence and keeps the old one
        // (<https://html.spec.whatwg.org/multipage/canvas.html#dom-context-2d-setlinedash>).
        if !value.is_finite() || value < 0.0 {
            return Ok(());
        }
        dash.push(value);
    }
    with_canvas(this.0.ctx(), &this.0, |canvas| canvas.set_line_dash(dash))
}

// ---- resources -----------------------------------------------------------

fn create_linear_gradient(
    this: This<Object<'_>>,
    x0: Coerced<f64>,
    y0: Coerced<f64>,
    x1: Coerced<f64>,
    y1: Coerced<f64>,
) -> Result<Value<'_>> {
    if ![*x0, *y0, *x1, *y1].iter().all(|value| value.is_finite()) {
        return Err(Exception::throw_type(
            this.0.ctx(),
            "gradient coordinates must be finite",
        ));
    }
    let id = with_canvas(this.0.ctx(), &this.0, |canvas| {
        canvas.create_linear_gradient(f32_of(*x0), f32_of(*y0), f32_of(*x1), f32_of(*y1))
    })?;
    resource_object(&this.0, "Gradient", id)
}

fn create_radial_gradient(
    this: This<Object<'_>>,
    x0: Coerced<f64>,
    y0: Coerced<f64>,
    r0: Coerced<f64>,
    x1: Coerced<f64>,
    y1: Coerced<f64>,
    r1: Coerced<f64>,
) -> Result<Value<'_>> {
    if ![*x0, *y0, *r0, *x1, *y1, *r1]
        .iter()
        .all(|value| value.is_finite())
    {
        return Err(Exception::throw_type(
            this.0.ctx(),
            "gradient coordinates must be finite",
        ));
    }
    if *r0 < 0.0 || *r1 < 0.0 {
        return Err(crate::js::bindings::throw_dom(
            this.0.ctx(),
            "IndexSizeError",
            "radii must not be negative",
        ));
    }
    let id = with_canvas(this.0.ctx(), &this.0, |canvas| {
        canvas.create_radial_gradient(
            f32_of(*x0),
            f32_of(*y0),
            f32_of(*r0),
            f32_of(*x1),
            f32_of(*y1),
            f32_of(*r1),
        )
    })?;
    resource_object(&this.0, "Gradient", id)
}

fn create_conic_gradient(
    this: This<Object<'_>>,
    angle: Coerced<f64>,
    x: Coerced<f64>,
    y: Coerced<f64>,
) -> Result<Value<'_>> {
    if ![*x, *y, *angle].iter().all(|value| value.is_finite()) {
        return Err(Exception::throw_type(
            this.0.ctx(),
            "gradient coordinates must be finite",
        ));
    }
    let id = with_canvas(this.0.ctx(), &this.0, |canvas| {
        canvas.create_conic_gradient(f32_of(*x), f32_of(*y), f32_of(*angle))
    })?;
    resource_object(&this.0, "Gradient", id)
}

fn create_pattern<'js>(
    this: This<Object<'js>>,
    source: Value<'js>,
    repetition: Opt<Value<'js>>,
) -> Result<Value<'js>> {
    reject_open_layer(this.0.ctx(), &this.0)?;
    let Some(repetition) = repetition.0 else {
        return Err(Exception::throw_type(
            this.0.ctx(),
            "createPattern needs a repetition",
        ));
    };
    let repetition = if repetition.is_null() {
        "repeat".to_owned()
    } else {
        let ctx = repetition.ctx().clone();
        DomString::from_js(&ctx, repetition)?.to_string()
    };
    if !matches!(
        repetition.as_str(),
        "repeat" | "repeat-x" | "repeat-y" | "no-repeat" | ""
    ) {
        return Err(crate::js::bindings::throw_dom(
            this.0.ctx(),
            "SyntaxError",
            "invalid repetition value",
        ));
    }
    let repetition = if repetition.is_empty() {
        "repeat".to_owned()
    } else {
        repetition
    };
    let source = match source_image(this.0.ctx(), source)? {
        ImageLookup::Decoded(source) => source,
        ImageLookup::Vector(vector) => {
            // A pattern has no destination rect, so the natural size is the
            // only scale available.
            let Some(image) = vector.natural(this.0.ctx()) else {
                return Ok(Value::new_null(this.0.ctx().clone()));
            };
            image
        }
        ImageLookup::NotReady => return Ok(Value::new_null(this.0.ctx().clone())),
        ImageLookup::Unsupported => {
            return Err(Exception::throw_type(
                this.0.ctx(),
                "createPattern expects an image source",
            ));
        }
        ImageLookup::Invalid => {
            return Err(crate::js::bindings::throw_dom(
                this.0.ctx(),
                "InvalidStateError",
                "the image source is not valid",
            ));
        }
    };
    let smoothing = with_canvas(this.0.ctx(), &this.0, |canvas| {
        canvas.image_smoothing_enabled()
    })?;
    let id = with_canvas(this.0.ctx(), &this.0, |canvas| {
        canvas.create_pattern(source, &repetition, smoothing)
    })?;
    let Some(id) = id else {
        return Ok(Value::new_null(this.0.ctx().clone()));
    };
    resource_object(&this.0, "Pattern", id)
}

fn resource_object<'js>(this: &Object<'js>, kind: &str, id: u32) -> Result<Value<'js>> {
    let object = Object::new(this.ctx().clone())?;
    let ctor_name = if kind == "Gradient" {
        "CanvasGradient"
    } else {
        "CanvasPattern"
    };
    // Use the intrinsic prototype captured at install time. The global is
    // page-writable, the intrinsic is not.
    let prototype: Object = if let Some(prototype) = world(this.ctx())?.borrow().brand(ctor_name) {
        prototype.restore(this.ctx())?
    } else {
        let ctor: Function = this.ctx().globals().get(ctor_name)?;
        ctor.get("prototype")?
    };
    object.set_prototype(Some(&prototype))?;
    object.set("__tbCanvasId", this.get::<_, u64>("__tbCanvasId")?)?;
    object.set(format!("__tb{kind}"), id)?;
    let resources: Object = this.get("__tbRes")?;
    resources.set(id.to_string(), object.clone())?;
    Ok(object.into_value())
}

fn add_color_stop(this: This<Object<'_>>, offset: Coerced<f64>, color: DomString) -> Result<()> {
    let id: u32 = this.0.get("__tbGradient")?;
    if !offset.is_finite() {
        return Err(Exception::throw_type(
            this.0.ctx(),
            "color stop offset must be finite",
        ));
    }
    // The offset check precedes color parsing, so an out-of-range offset
    // reports IndexSizeError even when the color is also invalid.
    if !(0.0..=1.0).contains(&*offset) {
        return Err(crate::js::bindings::throw_dom(
            this.0.ctx(),
            "IndexSizeError",
            "offset out of range",
        ));
    }
    // `currentColor` in a gradient stop resolves to opaque black: canvas
    // color stops do not inherit the element's CSS color
    // (<https://html.spec.whatwg.org/multipage/canvas.html#dom-canvasgradient-addcolorstop>).
    let color = if color.trim().eq_ignore_ascii_case("currentColor") {
        [0, 0, 0, 255]
    } else if let Some((color, _)) = css_color_exact(this.0.ctx(), &this.0, &color)? {
        color
    } else {
        return Err(crate::js::bindings::throw_dom(
            this.0.ctx(),
            "SyntaxError",
            "invalid color for addColorStop",
        ));
    };
    let object = this.0.clone();
    let id_wrapper = Object::new(this.0.ctx().clone())?;
    id_wrapper.set("__tbCanvasId", object.get::<_, u64>("__tbCanvasId")?)?;
    with_canvas(this.0.ctx(), &id_wrapper, |canvas| {
        canvas.add_color_stop(id, f32_of(*offset), color);
    })
}

fn resource_id(object: &Object<'_>, kind: &str) -> Option<u32> {
    object.get::<_, u32>(format!("__tb{kind}")).ok()
}

fn set_pattern_transform<'js>(this: This<Object<'js>>, matrix: Value<'js>) -> Result<()> {
    let ctx = this.0.ctx();
    let Some(transform) = matrix_2d_init(ctx, &matrix)? else {
        return Ok(());
    };
    let Some(id) = resource_id(&this.0, "Pattern") else {
        return Ok(());
    };
    with_canvas(ctx, &this.0, |canvas| {
        canvas.set_pattern_transform(id, transform);
    })
}

// ---- image data ----------------------------------------------------------

fn install_image_data(ctx: &Ctx<'_>) -> Result<()> {
    let prototype = Object::new(ctx.clone())?;
    prototype.prop(
        "width",
        Accessor::new(image_data_width, image_data_read_only),
    )?;
    prototype.prop(
        "height",
        Accessor::new(image_data_height, image_data_read_only),
    )?;
    prototype.prop("data", Accessor::new(image_data_data, image_data_read_only))?;
    prototype.prop(
        "colorSpace",
        Accessor::new(image_data_color_space, image_data_read_only),
    )?;
    prototype.prop(
        "pixelFormat",
        Accessor::new(image_data_pixel_format, image_data_read_only),
    )?;
    // https://html.spec.whatwg.org/multipage/canvas.html#imagedata
    prototype.prop(
        PredefinedAtom::SymbolToStringTag,
        Property::from("ImageData").configurable(),
    )?;
    let ctor =
        rquickjs::function::Constructor::new_prototype(ctx, prototype, ImageDataConstructor)?;
    ctx.globals().set("ImageData", ctor)?;
    Ok(())
}

fn image_data_width(this: This<Object<'_>>) -> Result<u32> {
    this.0.get("__tbWidth")
}

fn image_data_height(this: This<Object<'_>>) -> Result<u32> {
    this.0.get("__tbHeight")
}

fn image_data_data(this: This<Object<'_>>) -> Result<Value<'_>> {
    this.0.get("__tbData")
}

fn image_data_color_space(this: This<Object<'_>>) -> Result<rquickjs::String<'_>> {
    this.0.get("__tbColorSpace")
}

fn image_data_pixel_format(this: This<Object<'_>>) -> Result<rquickjs::String<'_>> {
    this.0.get("__tbPixelFormat")
}

#[expect(clippy::unnecessary_wraps, reason = "rquickjs callbacks return Result")]
fn image_data_read_only(_this: This<Object<'_>>, _value: Value<'_>) -> Result<()> {
    Ok(())
}

/// Runs the `ImageData` constructor as a native callback so it can inspect
/// `new.target`; signature-derived callbacks cannot see `Params`.
struct ImageDataConstructor;

impl<'js> rquickjs::function::IntoJsFunc<'js, ()> for ImageDataConstructor {
    fn param_requirements() -> rquickjs::function::ParamRequirement {
        rquickjs::function::ParamRequirement::any()
    }

    fn call(&self, params: rquickjs::function::Params<'_, 'js>) -> Result<Value<'js>> {
        image_data_construct(&params)
    }
}

/// Both `ImageData` constructor overloads
/// (<https://html.spec.whatwg.org/multipage/canvas.html#dom-imagedata-imagedata>).
fn image_data_construct<'js>(params: &rquickjs::function::Params<'_, 'js>) -> Result<Value<'js>> {
    let ctx = params.ctx().clone();
    if !params.is_constructor() {
        return Err(Exception::throw_type(
            &ctx,
            "ImageData must be called with new",
        ));
    }
    let ctx = &ctx;
    let args: Vec<Value<'js>> = (0..params.len())
        .filter_map(|index| params.arg(index))
        .collect();
    let object = Object::new(ctx.clone())?;
    let Some(first) = args.first() else {
        return Err(Exception::throw_type(ctx, "ImageData needs arguments"));
    };
    if let Some(data) = first
        .as_object()
        .and_then(|object| object.as_typed_array::<rquickjs::U8Clamped>())
    {
        if image_data_buffer_is_shared(first) {
            return Err(Exception::throw_type(
                ctx,
                "ImageData does not accept shared buffers",
            ));
        }
        let Some(sw) = args.get(1) else {
            return Err(Exception::throw_type(ctx, "ImageData needs a width"));
        };
        let sw = *Coerced::<f64>::from_js(ctx, sw.clone())?;
        let sh = args
            .get(2)
            .map(|value| Coerced::<f64>::from_js(ctx, value.clone()).map(|value| *value))
            .transpose()?;
        let settings = args.get(3);
        validate_image_data_settings(ctx, settings)?;
        initialize_image_data_from_data(ctx, &object, data.len(), first, sw, sh, settings)?;
        return Ok(object.into_value());
    }
    // The other data-overload member is Float16Array, which this engine does
    // not implement. Any other value selects the numeric overload, where
    // `null`/`{}` go through ToNumber to 0 and fail with IndexSizeError.
    let width = *Coerced::<f64>::from_js(ctx, first.clone())?;
    let height = args
        .get(1)
        .map(|value| Coerced::<f64>::from_js(ctx, value.clone()).map(|value| *value))
        .transpose()?
        .ok_or_else(|| Exception::throw_type(ctx, "ImageData needs a height"))?;
    let settings = args.get(2);
    validate_image_data_settings(ctx, settings)?;
    new_blank_image_data(ctx, &object, width, height, settings)?;
    Ok(object.into_value())
}

/// A `Uint8ClampedArray`'s buffer is shared when the engine does not classify
/// it as an ordinary `ArrayBuffer`. Class checks survive cross-realm buffers;
/// a page cannot fake the internal class with a constructor name.
fn image_data_buffer_is_shared(value: &Value<'_>) -> bool {
    let Some(buffer) = value
        .as_object()
        .and_then(|object| object.get::<_, Value>("buffer").ok())
        .filter(Value::is_object)
    else {
        return false;
    };
    rquickjs::ArrayBuffer::from_value(buffer).is_none()
}

/// The data overload of the `ImageData` constructor
/// (<https://html.spec.whatwg.org/multipage/canvas.html#dom-imagedata-with-data>).
fn initialize_image_data_from_data<'js>(
    ctx: &Ctx<'js>,
    object: &Object<'js>,
    byte_length: usize,
    data: &Value<'js>,
    sw: f64,
    sh: Option<f64>,
    settings: Option<&Value<'js>>,
) -> Result<()> {
    if byte_length == 0 || !byte_length.is_multiple_of(4) {
        return Err(crate::js::bindings::throw_dom(
            ctx,
            "InvalidStateError",
            "ImageData data length must be a nonzero multiple of four",
        ));
    }
    let length = byte_length / 4;
    let width = canvas_u32(sw);
    let width_usize = usize::try_from(width).unwrap_or(usize::MAX);
    if width == 0 || !length.is_multiple_of(width_usize) {
        return Err(crate::js::bindings::throw_dom(
            ctx,
            "IndexSizeError",
            "ImageData data length does not match its width",
        ));
    }
    let height = length / width_usize;
    let height = u32::try_from(height).unwrap_or(u32::MAX);
    if let Some(sh) = sh
        && canvas_u32(sh) != height
    {
        return Err(crate::js::bindings::throw_dom(
            ctx,
            "IndexSizeError",
            "ImageData data length does not match its height",
        ));
    }
    let (color_space, pixel_format) = image_data_settings(ctx, settings)?;
    set_image_data(
        ctx,
        object,
        width,
        height,
        data.clone(),
        color_space,
        pixel_format,
    )
}

fn new_blank_image_data<'js>(
    ctx: &Ctx<'js>,
    object: &Object<'js>,
    width: f64,
    height: f64,
    settings: Option<&Value<'js>>,
) -> Result<()> {
    let width = canvas_u32(width);
    let height = canvas_u32(height);
    if width == 0 || height == 0 {
        return Err(crate::js::bindings::throw_dom(
            ctx,
            "IndexSizeError",
            "ImageData dimensions must be nonzero",
        ));
    }
    let Some(bytes) = crate::js::world::canvas_bytes(width, height) else {
        return Err(crate::js::bindings::throw_dom(
            ctx,
            "IndexSizeError",
            "ImageData area is too large",
        ));
    };
    let data = data_array(ctx, vec![0; bytes])?.into_value();
    let (color_space, pixel_format) = image_data_settings(ctx, settings)?;
    set_image_data(ctx, object, width, height, data, color_space, pixel_format)
}

/// A dictionary argument must be an object (or null/undefined). Other
/// primitives cannot convert to `ImageDataSettings` and throw `TypeError`
/// (<https://webidl.spec.whatwg.org/#es-dictionary>).
fn validate_image_data_settings(ctx: &Ctx<'_>, settings: Option<&Value<'_>>) -> Result<()> {
    if let Some(value) = settings
        && !value.is_null()
        && !value.is_undefined()
        && value.as_object().is_none()
    {
        return Err(Exception::throw_type(
            ctx,
            "ImageData settings must be an object",
        ));
    }
    Ok(())
}

fn image_data_settings<'js>(
    ctx: &Ctx<'js>,
    settings: Option<&Value<'js>>,
) -> Result<(String, String)> {
    let Some(object) = settings.and_then(Value::as_object) else {
        return Ok(("srgb".to_owned(), "rgba-unorm8".to_owned()));
    };
    let read = |name: &str| -> Result<Option<String>> {
        let value: Value = object.get(name)?;
        if value.is_undefined() {
            return Ok(None);
        }
        Ok(Some(
            Coerced::<rquickjs::String>::from_js(ctx, value)?.to_string()?,
        ))
    };
    let color_space = read("colorSpace")?.unwrap_or_else(|| "srgb".to_owned());
    if !matches!(
        color_space.as_str(),
        "srgb" | "srgb-linear" | "display-p3" | "display-p3-linear"
    ) {
        return Err(Exception::throw_type(
            ctx,
            "unknown PredefinedColorSpace value",
        ));
    }
    let pixel_format = read("pixelFormat")?.unwrap_or_else(|| "rgba-unorm8".to_owned());
    if !matches!(pixel_format.as_str(), "rgba-unorm8" | "rgba-float16") {
        return Err(Exception::throw_type(
            ctx,
            "unknown ImageDataPixelFormat value",
        ));
    }
    Ok((color_space, pixel_format))
}

fn set_image_data<'js>(
    ctx: &Ctx<'js>,
    object: &Object<'js>,
    width: u32,
    height: u32,
    data: Value<'js>,
    color_space: String,
    pixel_format: String,
) -> Result<()> {
    object.set("__tbWidth", width)?;
    object.set("__tbHeight", height)?;
    object.set("__tbData", data)?;
    object.set(
        "__tbColorSpace",
        rquickjs::String::from_str(ctx.clone(), &color_space)?,
    )?;
    object.set(
        "__tbPixelFormat",
        rquickjs::String::from_str(ctx.clone(), &pixel_format)?,
    )?;
    Ok(())
}

fn data_array<'js>(ctx: &Ctx<'js>, bytes: Vec<u8>) -> Result<TypedArray<'js, rquickjs::U8Clamped>> {
    let bytes = bytes
        .into_iter()
        .map(rquickjs::U8Clamped)
        .collect::<Vec<_>>();
    TypedArray::new(ctx.clone(), bytes)
}

fn get_image_data(
    this: This<Object<'_>>,
    sx: Coerced<f64>,
    sy: Coerced<f64>,
    sw: Coerced<f64>,
    sh: Coerced<f64>,
) -> Result<Value<'_>> {
    reject_open_layer(this.0.ctx(), &this.0)?;
    // The IDL marks every argument `[EnforceRange] long`.
    let (mut sx, mut sy, mut sw, mut sh) = (
        f64::from(enforce_long(this.0.ctx(), *sx)?),
        f64::from(enforce_long(this.0.ctx(), *sy)?),
        f64::from(enforce_long(this.0.ctx(), *sw)?),
        f64::from(enforce_long(this.0.ctx(), *sh)?),
    );
    if sw == 0.0 || sh == 0.0 {
        return Err(crate::js::bindings::throw_dom(
            this.0.ctx(),
            "IndexSizeError",
            "source width or height is zero",
        ));
    }
    // Negative extents move the origin and normalize, like Blink
    // (`base_rendering_context_2d.cc`, getImageDataInternal).
    if sw < 0.0 {
        sx += sw;
        sw = -sw;
    }
    if sh < 0.0 {
        sy += sh;
        sh = -sh;
    }
    let width = canvas_u32(sw);
    let height = canvas_u32(sh);
    if width == 0 || height == 0 {
        return Err(crate::js::bindings::throw_dom(
            this.0.ctx(),
            "IndexSizeError",
            "source width or height rounds to zero",
        ));
    }
    let x = i32_of(sx);
    let y = i32_of(sy);
    let (pixels, origin_clean) = with_canvas(this.0.ctx(), &this.0, |canvas| {
        (
            canvas.read_pixels(x, y, width, height),
            canvas.origin_clean(),
        )
    })?;
    if !origin_clean {
        return Err(crate::js::bindings::throw_dom(
            this.0.ctx(),
            "SecurityError",
            "the canvas is not origin-clean",
        ));
    }
    let bytes = pixels.ok_or_else(|| {
        crate::js::bindings::throw_dom(
            this.0.ctx(),
            "IndexSizeError",
            "getImageData area is too large",
        )
    })?;
    image_data_object(this.0.ctx(), width, height, bytes)
}

fn put_image_data<'js>(
    this: This<Object<'js>>,
    data: Value<'js>,
    args: rquickjs::function::Rest<Coerced<f64>>,
) -> Result<()> {
    let ctx = this.0.ctx();
    reject_open_layer(ctx, &this.0)?;
    let number = |index: usize| -> Result<f64> {
        let value = args
            .0
            .get(index)
            .ok_or_else(|| Exception::throw_type(ctx, "missing argument"))?;
        Ok(**value)
    };
    let (dx, dy) = (
        enforce_long(ctx, number(0)?)?,
        enforce_long(ctx, number(1)?)?,
    );
    let Some((width, height, pixels)) = read_image_data(&data)? else {
        return Err(Exception::throw_type(ctx, "putImageData expects ImageData"));
    };
    let dirty = match (number(2), number(3), number(4), number(5)) {
        (Ok(x), Ok(y), Ok(width), Ok(height)) => Some((
            enforce_long(ctx, x)?,
            enforce_long(ctx, y)?,
            enforce_long(ctx, width)?,
            enforce_long(ctx, height)?,
        )),
        _ => None,
    };
    with_canvas(ctx, &this.0, |canvas| {
        canvas.write_pixels(&pixels, width, height, dx, dy, dirty);
    })
}

fn create_image_data<'js>(
    this: This<Object<'js>>,
    first: Value<'js>,
    second: Opt<Coerced<f64>>,
) -> Result<Value<'js>> {
    let ctx = this.0.ctx();
    context_canvas_id(ctx, &this.0)?;
    if let Some(second) = second.0 {
        let width = *Coerced::<f64>::from_js(ctx, first)?;
        let height = *second;
        // Blink takes the absolute value of both dimensions
        // (`BaseRenderingContext2D::createImageData`), but both are
        // `[EnforceRange] long` first.
        let width = enforce_long(ctx, width)?.unsigned_abs();
        let height = enforce_long(ctx, height)?.unsigned_abs();
        if width == 0 || height == 0 {
            return Err(crate::js::bindings::throw_dom(
                ctx,
                "IndexSizeError",
                "ImageData dimensions must be nonzero",
            ));
        }
        let Some(bytes) = crate::js::world::canvas_bytes(width, height) else {
            return Err(crate::js::bindings::throw_dom(
                ctx,
                "IndexSizeError",
                "ImageData area is too large",
            ));
        };
        return image_data_object(ctx, width, height, vec![0; bytes]);
    }
    let Some((width, height, _)) = read_image_data(&first)? else {
        return Err(Exception::throw_type(
            ctx,
            "createImageData expects ImageData or a size",
        ));
    };
    let Some(bytes) = crate::js::world::canvas_bytes(width, height) else {
        return Err(crate::js::bindings::throw_dom(
            ctx,
            "IndexSizeError",
            "ImageData area is too large",
        ));
    };
    image_data_object(ctx, width, height, vec![0; bytes])
}

fn image_data_object<'js>(
    ctx: &Ctx<'js>,
    width: u32,
    height: u32,
    bytes: Vec<u8>,
) -> Result<Value<'js>> {
    let ctor: Function = ctx.globals().get("ImageData")?;
    let prototype: Object = ctor.get("prototype")?;
    let image = Object::new(ctx.clone())?;
    image.set_prototype(Some(&prototype))?;
    let data = data_array(ctx, bytes)?.into_value();
    set_image_data(
        ctx,
        &image,
        width,
        height,
        data,
        "srgb".to_owned(),
        "rgba-unorm8".to_owned(),
    )?;
    Ok(image.into_value())
}

fn read_image_data(value: &Value<'_>) -> Result<Option<(u32, u32, Vec<u8>)>> {
    let Some(object) = value.as_object() else {
        return Ok(None);
    };
    let Ok(width) = object.get::<_, u32>("width") else {
        return Ok(None);
    };
    let Ok(height) = object.get::<_, u32>("height") else {
        return Ok(None);
    };
    let usable = |value: Value<'_>| !value.is_undefined() && !value.is_null();
    let data = object
        .get::<_, Value>("data")
        .ok()
        .filter(|value| usable(value.clone()))
        .or_else(|| {
            object
                .get::<_, Value>("__tbData")
                .ok()
                .filter(|value| usable(value.clone()))
        });
    let Some(data) = data else {
        return Ok(None);
    };
    let Some(bytes) = typed_array_bytes(&data)? else {
        return Ok(None);
    };
    // A matching width and height do not validate the buffer: a page can hand
    // `putImageData` any object with a short array and huge sizes, and the
    // per-pixel loop would run for hours inside Rust, where the JS interrupt
    // handler never runs.
    let Some(expected) = crate::js::world::canvas_bytes(width, height) else {
        return Ok(None);
    };
    if bytes.len() != expected {
        return Ok(None);
    }
    Ok(Some((width, height, bytes)))
}

fn typed_array_bytes(value: &Value<'_>) -> Result<Option<Vec<u8>>> {
    let Some(object) = value.as_object() else {
        return Ok(None);
    };
    if let Some(array) = object.as_typed_array::<rquickjs::U8Clamped>() {
        let mut bytes = Vec::with_capacity(array.len());
        for index in 0..array.len() {
            let key = u32::try_from(index).unwrap_or(u32::MAX);
            bytes.push(array.get::<_, u8>(key)?);
        }
        return Ok(Some(bytes));
    }
    if let Some(array) = object.as_typed_array::<u8>() {
        let mut bytes = Vec::with_capacity(array.len());
        for index in 0..array.len() {
            let key = u32::try_from(index).unwrap_or(u32::MAX);
            bytes.push(array.get::<_, u8>(key)?);
        }
        return Ok(Some(bytes));
    }
    Ok(None)
}

fn canvas_u32(value: f64) -> u32 {
    if !value.is_finite() || value <= 0.0 {
        return 0;
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "value is finite, non-negative, and clamped to the u32 range"
    )]
    let value = value.min(f64::from(u32::MAX)) as u32;
    value
}

// ---- image bitmaps -------------------------------------------------------

/// `ImageBitmap` and the host half of `createImageBitmap`
/// (<https://html.spec.whatwg.org/multipage/imagebitmap-and-animations.html#imagebitmaps>).
fn install_image_bitmap(ctx: &Ctx<'_>) -> Result<()> {
    let prototype = Object::new(ctx.clone())?;
    prototype.prop(
        "width",
        Accessor::new(image_bitmap_width, image_data_read_only),
    )?;
    prototype.prop(
        "height",
        Accessor::new(image_bitmap_height, image_data_read_only),
    )?;
    prototype.prop("close", Func::from(image_bitmap_close))?;
    // https://html.spec.whatwg.org/multipage/canvas.html#imagebitmap
    prototype.prop(
        PredefinedAtom::SymbolToStringTag,
        Property::from("ImageBitmap").configurable(),
    )?;
    let ctor = Function::new(ctx.clone(), || {})?;
    ctor.set("prototype", prototype)?;
    ctx.globals().set("ImageBitmap", ctor)?;
    let host = crate::js::bridge::object(ctx)?;
    host.set(
        "createImageBitmapDecode",
        Func::from(create_image_bitmap_decode),
    )?;
    host.set(
        "createImageBitmapFromSource",
        Func::from(create_image_bitmap_from_source),
    )?;
    Ok(())
}

fn image_bitmap_width(this: This<Object<'_>>) -> Result<u32> {
    this.0.get("__tbWidth")
}

fn image_bitmap_height(this: This<Object<'_>>) -> Result<u32> {
    this.0.get("__tbHeight")
}

fn image_bitmap_close(this: This<Object<'_>>) -> Result<()> {
    if let Ok(id) = this.0.get::<_, u64>("__tbBitmap")
        && let Ok(world) = world(this.0.ctx())
    {
        world.borrow().registry().borrow_mut().remove_bitmap(id);
    }
    this.0.set("__tbWidth", 0)?;
    this.0.set("__tbHeight", 0)?;
    this.0.set("__tbClosed", true)?;
    Ok(())
}

fn create_image_bitmap_decode<'js>(
    ctx: Ctx<'js>,
    bytes: Value<'js>,
    options: Opt<Value<'js>>,
) -> Result<Value<'js>> {
    let Some(bytes) = typed_array_bytes(&bytes)? else {
        return Err(Exception::throw_type(
            &ctx,
            "createImageBitmap expects a BufferSource",
        ));
    };
    let options = bitmap_options(options.0.as_ref().and_then(|value| value.as_object()))?;
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| {
            crate::js::bindings::throw_dom(&ctx, "InvalidStateError", "image could not be read")
        })?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(crate::js::world::MAX_CANVAS_BYTES as u64);
    reader.limits(limits);
    let image = reader.decode().map_err(|_| {
        crate::js::bindings::throw_dom(&ctx, "InvalidStateError", "image could not be decoded")
    })?;
    if crate::js::world::canvas_bytes(image.width(), image.height()).is_none() {
        return Err(crate::js::bindings::throw_dom(
            &ctx,
            "InvalidStateError",
            "image is too large",
        ));
    }
    let image = image.to_rgba8();
    let (width, height) = (image.width(), image.height());
    let Some((width, height, pixels)) =
        resize_bitmap(width, height, image.into_raw().into(), &options)
    else {
        return Err(crate::js::bindings::throw_dom(
            &ctx,
            "InvalidStateError",
            "bitmap is too large",
        ));
    };
    image_bitmap_object(&ctx, width, height, pixels, true)
}

fn create_image_bitmap_from_source<'js>(
    ctx: Ctx<'js>,
    source: Value<'js>,
    options: Opt<Value<'js>>,
) -> Result<Value<'js>> {
    let options = bitmap_options(options.0.as_ref().and_then(|value| value.as_object()))?;
    let image = match source_image(&ctx, source)? {
        ImageLookup::Decoded(image) => image,
        ImageLookup::Vector(vector) => {
            let Some(image) = vector.natural(&ctx) else {
                return Err(crate::js::bindings::throw_dom(
                    &ctx,
                    "InvalidStateError",
                    "the image source is not usable",
                ));
            };
            image
        }
        ImageLookup::Unsupported => {
            return Err(Exception::throw_type(
                &ctx,
                "createImageBitmap cannot use this source",
            ));
        }
        ImageLookup::NotReady | ImageLookup::Invalid => {
            return Err(crate::js::bindings::throw_dom(
                &ctx,
                "InvalidStateError",
                "the image source is not usable",
            ));
        }
    };
    let origin_clean = image.origin_clean;
    let Some((width, height, pixels)) =
        resize_bitmap(image.width, image.height, image.pixels, &options)
    else {
        return Err(crate::js::bindings::throw_dom(
            &ctx,
            "InvalidStateError",
            "bitmap is too large",
        ));
    };
    image_bitmap_object(&ctx, width, height, pixels, origin_clean)
}

fn image_bitmap_object<'js>(
    ctx: &Ctx<'js>,
    width: u32,
    height: u32,
    pixels: Arc<[u8]>,
    origin_clean: bool,
) -> Result<Value<'js>> {
    let ctor: Function = ctx.globals().get("ImageBitmap")?;
    let prototype: Object = ctor.get("prototype")?;
    let image = Object::new(ctx.clone())?;
    image.set_prototype(Some(&prototype))?;
    let bitmap_id = world(ctx)?.borrow().registry().borrow_mut().insert_bitmap(
        width,
        height,
        pixels,
        origin_clean,
    );
    image.set("__tbBitmap", bitmap_id)?;
    image.set("__tbWidth", width)?;
    image.set("__tbHeight", height)?;
    image.set(
        "__tbColorSpace",
        rquickjs::String::from_str(ctx.clone(), "srgb")?,
    )?;
    image.set(
        "__tbPixelFormat",
        rquickjs::String::from_str(ctx.clone(), "rgba-unorm8")?,
    )?;
    image.set("__tbOriginClean", origin_clean)?;
    image.set("__tbClosed", false)?;
    Ok(image.into_value())
}

struct BitmapOptions {
    resize_width: Option<u32>,
    resize_height: Option<u32>,
    quality: image::imageops::FilterType,
    flip_y: bool,
}

fn bitmap_options(options: Option<&Object<'_>>) -> Result<BitmapOptions> {
    let read = |name: &str| {
        options
            .and_then(|object| object.get::<_, Value>(name).ok())
            .filter(|value| !value.is_undefined() && !value.is_null())
    };
    let dimension = |value: Option<Value<'_>>| -> Result<Option<u32>> {
        let Some(value) = value else {
            return Ok(None);
        };
        let ctx = value.ctx().clone();
        let number = *Coerced::<f64>::from_js(&ctx, value)?;
        if !number.is_finite() {
            return Err(Exception::throw_type(
                &ctx,
                "resize dimensions must be finite",
            ));
        }
        let number = canvas_u32(number);
        if number == 0 {
            return Err(Exception::throw_type(
                &ctx,
                "resize dimensions must be nonzero",
            ));
        }
        Ok(Some(number))
    };
    let resize_width = dimension(read("resizeWidth"))?;
    let resize_height = dimension(read("resizeHeight"))?;
    let quality = read("resizeQuality")
        .map(|value| {
            let ctx = value.ctx().clone();
            DomString::from_js(&ctx, value)
        })
        .transpose()?
        .map_or(
            image::imageops::FilterType::Triangle,
            |value| match &*value {
                "pixelated" => image::imageops::FilterType::Nearest,
                "medium" => image::imageops::FilterType::CatmullRom,
                "high" => image::imageops::FilterType::Lanczos3,
                _ => image::imageops::FilterType::Triangle,
            },
        );
    let flip_y = read("imageOrientation")
        .map(|value| {
            let ctx = value.ctx().clone();
            DomString::from_js(&ctx, value)
        })
        .transpose()?
        .is_some_and(|value| &*value == "flipY");
    Ok(BitmapOptions {
        resize_width,
        resize_height,
        quality,
        flip_y,
    })
}

fn resize_bitmap(
    width: u32,
    height: u32,
    pixels: Arc<[u8]>,
    options: &BitmapOptions,
) -> Option<(u32, u32, Arc<[u8]>)> {
    // `RgbaImage` owns its pixels, so a raster still shared with the source
    // cache is copied once here; `createImageBitmap` is not a hot path.
    let buffer = image::RgbaImage::from_raw(width, height, pixels.to_vec())?;
    let (source_width, source_height) = (buffer.width(), buffer.height());
    let (target_width, target_height) = match (options.resize_width, options.resize_height) {
        (None, None) => (source_width, source_height),
        (Some(width), None) => (width, scaled_side(source_height, width, source_width)),
        (None, Some(height)) => (scaled_side(source_width, height, source_height), height),
        (Some(width), Some(height)) => (width, height),
    };
    crate::js::world::canvas_bytes(target_width, target_height)?;
    let resized = if (target_width, target_height) == (source_width, source_height) {
        buffer
    } else {
        image::imageops::resize(&buffer, target_width, target_height, options.quality)
    };
    let resized = if options.flip_y {
        image::imageops::flip_vertical(&resized)
    } else {
        resized
    };
    Some((resized.width(), resized.height(), resized.into_raw().into()))
}

/// The unspecified resize axis keeps the source aspect ratio, rounded to the
/// nearest pixel
/// (<https://html.spec.whatwg.org/multipage/imagebitmap-and-animations.html#dom-createimagebitmap>).
fn scaled_side(side: u32, target: u32, source: u32) -> u32 {
    if source == 0 {
        return 1;
    }
    let ratio = f64::from(target) / f64::from(source);
    let scaled = (f64::from(side) * ratio).round();
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the scale is positive and bounded by the target dimension"
    )]
    let scaled = scaled.max(1.0) as u32;
    scaled
}

// ---- images --------------------------------------------------------------

fn draw_image<'js>(
    this: This<Object<'js>>,
    source: Value<'js>,
    args: rquickjs::function::Rest<Coerced<f64>>,
) -> Result<()> {
    reject_open_layer(this.0.ctx(), &this.0)?;
    let lookup = source_image(this.0.ctx(), source)?;
    let (natural_width, natural_height) = match &lookup {
        ImageLookup::Decoded(image) => (image.width, image.height),
        ImageLookup::Vector(vector) => (vector.natural_width, vector.natural_height),
        ImageLookup::NotReady => return Ok(()),
        ImageLookup::Unsupported => {
            return Err(Exception::throw_type(
                this.0.ctx(),
                "drawImage expects an image source",
            ));
        }
        ImageLookup::Invalid => {
            return Err(crate::js::bindings::throw_dom(
                this.0.ctx(),
                "InvalidStateError",
                "the image source is not usable",
            ));
        }
    };
    let values: Vec<f64> = args.0.iter().map(|value| **value).collect();
    let (sx, sy, sw, sh, dx, dy, dw, dh) = match values.as_slice() {
        [dx, dy] => (
            0.0,
            0.0,
            f64::from(natural_width),
            f64::from(natural_height),
            *dx,
            *dy,
            f64::from(natural_width),
            f64::from(natural_height),
        ),
        [dx, dy, dw, dh] => (
            0.0,
            0.0,
            f64::from(natural_width),
            f64::from(natural_height),
            *dx,
            *dy,
            *dw,
            *dh,
        ),
        [sx, sy, sw, sh, dx, dy, dw, dh] => (*sx, *sy, *sw, *sh, *dx, *dy, *dw, *dh),
        _ => {
            return Err(rquickjs::Exception::throw_type(
                this.0.ctx(),
                "drawImage expects 3, 5, or 9 arguments",
            ));
        }
    };
    let src = Rect::new(f32_of(sx), f32_of(sy), f32_of(sw), f32_of(sh));
    let dst = Rect::new(f32_of(dx), f32_of(dy), f32_of(dw), f32_of(dh));
    match lookup {
        ImageLookup::Decoded(image) => with_canvas(this.0.ctx(), &this.0, |canvas| {
            canvas.draw_image(&image, src, dst);
        }),
        ImageLookup::Vector(vector) => {
            let (scale_x, scale_y) =
                with_canvas(this.0.ctx(), &this.0, |canvas| canvas.device_scale())?;
            let Some((raster_width, raster_height)) =
                vector.raster_size(src, dst, scale_x, scale_y)
            else {
                return Ok(());
            };
            let Some(image) = vector.rasterize(this.0.ctx(), raster_width, raster_height) else {
                return Ok(());
            };
            // `src` selects a region in intrinsic image pixels; the raster is
            // that image at `raster_width x raster_height`, so the same region
            // is a proportionally larger rectangle.
            let factor_x = to_f32(raster_width) / to_f32(vector.natural_width);
            let factor_y = to_f32(raster_height) / to_f32(vector.natural_height);
            let src = Rect::new(
                src.x * factor_x,
                src.y * factor_y,
                src.width * factor_x,
                src.height * factor_y,
            );
            with_canvas(this.0.ctx(), &this.0, |canvas| {
                canvas.draw_image(&image, src, dst);
            })
        }
        // The natural-size lookup above returned for these.
        ImageLookup::NotReady | ImageLookup::Unsupported | ImageLookup::Invalid => Ok(()),
    }
}

/// A `drawImage`/`createPattern` source lookup.
enum ImageLookup {
    Decoded(SourceImage),
    /// A vector source (`<img>` whose data is SVG). Rasterization is deferred
    /// so `drawImage` can choose the destination's device resolution instead
    /// of scaling a fixed raster.
    Vector(VectorSource),
    /// A canvas or image that exists but has no pixels yet; drawing it is a
    /// no-op, not an error.
    NotReady,
    /// The object is not an image source at all.
    Unsupported,
    /// The source is invalid (zero-sized canvas, closed bitmap).
    Invalid,
}

/// Decodes a `drawImage`/`createPattern` source: a canvas element, an
/// `ImageData`/`ImageBitmap`, or an `<img>` with decoded pixels
/// (<https://html.spec.whatwg.org/multipage/canvas.html#check-the-usability-of-the-image-argument>).
fn source_image<'js>(ctx: &Ctx<'js>, value: Value<'js>) -> Result<ImageLookup> {
    // A canvas this realm owns is its bitmap, not a DOM lookup. `<canvas>` and
    // `OffscreenCanvas` share the snapshot and taint rules; only their
    // dimensions differ.
    if let Some(host) = ElementCanvas::resolve(ctx, &value) {
        return Ok(canvas_host_image(ctx, host));
    }
    if let Some(id) = offscreen_canvas_id(ctx, &value)? {
        return Ok(canvas_host_image(ctx, OffscreenCanvasHost { id }));
    }
    let Some(id) = crate::js::bindings::host_node_id(ctx, &value) else {
        if let Some(object) = value.as_object()
            && object.get::<_, bool>("__tbClosed").unwrap_or(false)
        {
            return Ok(ImageLookup::Invalid);
        }
        if let Some(object) = value.as_object()
            && let Ok(id) = object.get::<_, u64>("__tbBitmap")
        {
            let image = world(ctx)?.borrow().registry().borrow().bitmap(id);
            return Ok(image.map_or(ImageLookup::Invalid, ImageLookup::Decoded));
        }
        let Some((width, height, pixels)) = read_image_data(&value)? else {
            return Ok(ImageLookup::Unsupported);
        };
        let origin_clean = value
            .as_object()
            .and_then(|object| object.get::<_, bool>("__tbOriginClean").ok())
            .unwrap_or(true);
        return Ok(ImageLookup::Decoded(SourceImage {
            width,
            height,
            pixels: pixels.into(),
            origin_clean,
        }));
    };
    let world = world(ctx)?;
    let (request, size) = {
        let world = world.borrow();
        (world.image_request(id), world.image_size(id))
    };
    let loading = request
        .as_ref()
        .is_some_and(|request| request.status == blitz_dom::ImageRequestStatus::Pending);
    let broken = request
        .as_ref()
        .is_some_and(|request| request.status == blitz_dom::ImageRequestStatus::Failed);
    let zero_size = size.is_some_and(|(width, height)| width == 0 || height == 0);
    // A pending request is not fully decodable, so `drawImage` draws
    // nothing: reassigning `src` starts a new request whose state is
    // unavailable, and the previous request's pixels are not what the
    // element's image data names any more
    // (<https://html.spec.whatwg.org/multipage/canvas.html#check-the-usability-of-the-image-argument>,
    // <https://html.spec.whatwg.org/multipage/images.html#img-not-fully-decodable>).
    if loading {
        return Ok(ImageLookup::NotReady);
    }
    if broken {
        return Ok(ImageLookup::Invalid);
    }
    let tracked_origin_clean = request.as_ref().map(|request| request.origin_clean);
    let lookup = world
        .borrow()
        .with_document(id, |parsed| {
            source_element(ctx, id, parsed, zero_size, broken, tracked_origin_clean)
        })
        .flatten()
        .unwrap_or(ImageLookup::Unsupported);
    Ok(lookup)
}

/// The pixels of a canvas source, whether it has a context yet or not.
fn canvas_host_image(ctx: &Ctx<'_>, host: impl CanvasHost) -> ImageLookup {
    let (width, height) = host.dimensions(ctx);
    if width == 0 || height == 0 {
        return ImageLookup::Invalid;
    }
    if host.has_open_layer(ctx) {
        return ImageLookup::Invalid;
    }
    if let Some(snapshot) = host.snapshot(ctx) {
        return if snapshot.width == 0 || snapshot.height == 0 {
            ImageLookup::Invalid
        } else {
            ImageLookup::Decoded(snapshot)
        };
    }
    // A canvas without a context is still an empty bitmap. Its declared size
    // is page-controlled, so an over-budget one has no pixels to draw.
    let pixels =
        crate::js::world::canvas_bytes(width, height).map_or_else(Vec::new, |bytes| vec![0; bytes]);
    ImageLookup::Decoded(SourceImage {
        width,
        height,
        pixels: pixels.into(),
        origin_clean: true,
    })
}

/// Looks up a canvas, SVG image, or HTML image element by node id. An HTML
/// image's canvas taint is `origin_clean`, recorded when its response
/// settled; a canvas is its own bitmap and carries no taint of its own.
fn source_element(
    ctx: &Ctx<'_>,
    id: NodeId,
    parsed: &crate::Parsed,
    zero_size: bool,
    broken: bool,
    origin_clean: Option<bool>,
) -> Option<ImageLookup> {
    let element = parsed.document.base.get_node(id.node)?;
    let data = element.data.downcast_element()?;
    let zero_size = zero_size
        || matches!(
            data.image_data(),
            Some(blitz_dom::node::ImageData::Svg(svg))
                if svg.intrinsic_width() == Some(0.0) || svg.intrinsic_height() == Some(0.0)
        );

    if data.name.ns == crate::dom::tree::html_namespace() && data.name.local.as_str() == "canvas" {
        // A canvas without a context is still an empty bitmap.
        let width = canvas_dimension(ctx, id, "width", 300);
        let height = canvas_dimension(ctx, id, "height", 150);
        if width == 0 || height == 0 {
            return Some(ImageLookup::Invalid);
        }
        // A context-less canvas is transparent, but its declared size is
        // page-controlled: an over-budget one has no pixels to draw.
        let pixels = crate::js::world::canvas_bytes(width, height)
            .map_or_else(Vec::new, |bytes| vec![0; bytes]);
        return Some(ImageLookup::Decoded(SourceImage {
            width,
            height,
            pixels: pixels.into(),
            origin_clean: true,
        }));
    }
    if data.name.ns == crate::dom::tree::svg_namespace() && data.name.local.as_str() == "image" {
        // A rasterized SVG image is usable; zero-sized and detached sources are null.
        let has_source = data.attrs().iter().any(|attribute| {
            matches!(attribute.name.local.as_str(), "href" | "src")
                && !attribute.value.trim().is_empty()
        });
        let connected = crate::dom::tree::is_connected(&parsed.document.base, id.node);
        return Some(match data.raster_image_data() {
            Some(raster) => ImageLookup::Decoded(SourceImage {
                width: raster.width,
                height: raster.height,
                pixels: raster.data.as_ref().to_vec().into(),
                // Blitz does not decode SVG `<image>` resources, so this is
                // unreachable today; if it starts, fail closed rather than
                // claim the pixels are clean.
                origin_clean: false,
            }),
            None if broken => ImageLookup::Invalid,
            None if zero_size || !connected || !has_source => ImageLookup::NotReady,
            // A connected SVG image that Blitz did not rasterize and did not
            // mark broken has no usable pixels yet.
            None => ImageLookup::NotReady,
        });
    }
    if data.name.ns != crate::dom::tree::html_namespace() {
        return Some(ImageLookup::Unsupported);
    }
    match data.name.local.as_str() {
        "video" => {
            // Video decoding is not implemented, so a video element never has
            // a frame to draw. It is a valid source type, not an unsupported
            // one
            // (<https://html.spec.whatwg.org/multipage/canvas.html#canvasimagesource>).
            return Some(ImageLookup::NotReady);
        }
        "img" => {}
        _ => return Some(ImageLookup::Unsupported),
    }
    if let Some(blitz_dom::node::ImageData::Svg(svg)) = data.image_data() {
        if zero_size {
            return Some(ImageLookup::NotReady);
        }
        let (natural_width, natural_height) = crate::render::svg_natural_size(svg);
        if natural_width == 0 || natural_height == 0 {
            return Some(ImageLookup::NotReady);
        }
        return Some(ImageLookup::Vector(VectorSource {
            svg: svg.clone(),
            natural_width,
            natural_height,
            origin_clean: origin_clean.unwrap_or(false),
        }));
    }
    let has_source = data.attrs().iter().any(|attribute| {
        attribute.name.local.as_str() == "src" && !attribute.value.trim().is_empty()
    });
    let connected = crate::dom::tree::is_connected(&parsed.document.base, id.node);
    match data.raster_image_data() {
        Some(raster) => Some(ImageLookup::Decoded(SourceImage {
            width: raster.width,
            height: raster.height,
            pixels: raster.data.as_ref().to_vec().into(),
            // Only the settled request's taint record speaks for the pixels;
            // an unrecorded request is treated as cross-origin.
            origin_clean: origin_clean.unwrap_or(false),
        })),
        // A connected missing resource is invalid; a zero-sized image, a
        // detached image, or one with no source is not ready.
        None if has_source && connected && !zero_size => Some(ImageLookup::Invalid),
        None => Some(ImageLookup::NotReady),
    }
}

/// A parsed vector image source. Rasterization is deferred so `drawImage` can
/// choose the destination's device resolution instead of scaling a fixed
/// raster.
struct VectorSource {
    svg: blitz_dom::node::SvgImageData,
    natural_width: u32,
    natural_height: u32,
    origin_clean: bool,
}

impl VectorSource {
    /// Rasterizes at the natural size, for callers without a destination rect
    /// (`createPattern`, `createImageBitmap`).
    fn natural(&self, ctx: &Ctx<'_>) -> Option<SourceImage> {
        self.rasterize(ctx, self.natural_width, self.natural_height)
    }

    /// Rasterizes at `width x height` through the realm's bounded cache.
    fn rasterize(&self, ctx: &Ctx<'_>, width: u32, height: u32) -> Option<SourceImage> {
        Some(SourceImage {
            width,
            height,
            pixels: svg_raster(ctx, &self.svg, width, height)?,
            origin_clean: self.origin_clean,
        })
    }

    /// The raster size for a draw of `src` (in intrinsic image pixels) into
    /// `dst` (in user units) under a CTM that maps user units to device pixels
    /// by `(scale_x, scale_y)`.
    fn raster_size(&self, src: Rect, dst: Rect, scale_x: f32, scale_y: f32) -> Option<(u32, u32)> {
        vector_raster_size(
            self.natural_width,
            self.natural_height,
            src,
            dst,
            scale_x,
            scale_y,
        )
    }
}

/// The raster size for a vector draw of `src` (in intrinsic image pixels)
/// into `dst` (in user units), under a CTM that maps user units to device
/// pixels by `(scale_x, scale_y)`.
///
/// The raster never shrinks below the intrinsic size: upscaling a small SVG to
/// a large destination is the case that looks soft, while keeping the
/// intrinsic size for a downscale leaves room for the sampler.
fn vector_raster_size(
    natural_width: u32,
    natural_height: u32,
    src: Rect,
    dst: Rect,
    scale_x: f32,
    scale_y: f32,
) -> Option<(u32, u32)> {
    let natural_width = to_f32(natural_width);
    let natural_height = to_f32(natural_height);
    let src_width = src.width.abs();
    let src_height = src.height.abs();
    let pixels_per_source_x = if src_width.is_finite() && src_width > 0.0 {
        dst.width.abs() * scale_x / src_width
    } else {
        1.0
    };
    let pixels_per_source_y = if src_height.is_finite() && src_height > 0.0 {
        dst.height.abs() * scale_y / src_height
    } else {
        1.0
    };
    fit_raster(
        (natural_width * pixels_per_source_x).max(natural_width),
        (natural_height * pixels_per_source_y).max(natural_height),
    )
}

/// The most cached SVG rasters and the bytes they may hold together. A page
/// can request any size, so both bounds are needed.
const MAX_SVG_RASTER_ENTRIES: usize = 8;
const MAX_SVG_RASTER_BYTES: usize = 32 * 1024 * 1024;

/// Bounded cache of SVG rasters, keyed by the parsed tree and target size.
///
/// The entry keeps the tree alive, so the `Arc` identity cannot be reused by a
/// later parse while its raster is cached.
#[derive(Default)]
pub(crate) struct SvgRasterCache {
    entries: Vec<SvgRasterEntry>,
    bytes: usize,
}

struct SvgRasterEntry {
    svg: blitz_dom::node::SvgImageData,
    width: u32,
    height: u32,
    pixels: Arc<[u8]>,
}

impl SvgRasterCache {
    fn get(
        &mut self,
        svg: &blitz_dom::node::SvgImageData,
        width: u32,
        height: u32,
    ) -> Option<Arc<[u8]>> {
        let index = self.entries.iter().position(|entry| {
            entry.width == width
                && entry.height == height
                && Arc::ptr_eq(&entry.svg.tree, &svg.tree)
        })?;
        // A repeated draw at the same size is the common case, so the hit
        // moves to the front and eviction takes from the back.
        if index != 0 {
            let entry = self.entries.remove(index);
            self.entries.insert(0, entry);
        }
        Some(self.entries[0].pixels.clone())
    }

    fn insert(
        &mut self,
        svg: &blitz_dom::node::SvgImageData,
        width: u32,
        height: u32,
        pixels: Arc<[u8]>,
    ) {
        // An oversized raster is used once, not retained for the whole realm.
        if pixels.len() > MAX_SVG_RASTER_BYTES {
            return;
        }
        self.bytes += pixels.len();
        self.entries.insert(
            0,
            SvgRasterEntry {
                svg: svg.clone(),
                width,
                height,
                pixels,
            },
        );
        while self.entries.len() > MAX_SVG_RASTER_ENTRIES || self.bytes > MAX_SVG_RASTER_BYTES {
            let Some(entry) = self.entries.pop() else {
                break;
            };
            self.bytes -= entry.pixels.len();
        }
    }
}

/// Rasterizes `svg` at `width x height` into straight RGBA, through the
/// realm's cache.
fn svg_raster(
    ctx: &Ctx<'_>,
    svg: &blitz_dom::node::SvgImageData,
    width: u32,
    height: u32,
) -> Option<Arc<[u8]>> {
    if width == 0 || height == 0 {
        return None;
    }
    let registry = world(ctx).ok()?.borrow().registry();
    if let Some(pixels) = registry.borrow_mut().svg_rasters.get(svg, width, height) {
        return Some(pixels);
    }
    let pixels: Arc<[u8]> = render_svg_pixels(svg, width, height)?.into();
    registry
        .borrow_mut()
        .svg_rasters
        .insert(svg, width, height, pixels.clone());
    Some(pixels)
}

/// Renders `svg` at `width x height` into straight RGBA. This is the work the
/// cache exists to avoid repeating.
fn render_svg_pixels(
    svg: &blitz_dom::node::SvgImageData,
    width: u32,
    height: u32,
) -> Option<Vec<u8>> {
    // The declared viewport is page-controlled. Refuse anything above the
    // canvas pixmap budget instead of letting a huge SVG allocate gigabytes.
    let bytes = crate::js::world::canvas_bytes(width, height)?;
    let mut painter = anyrender_tiny_skia::TinySkiaScenePainter::new(width, height);
    #[expect(
        clippy::cast_precision_loss,
        reason = "SVG raster sizes are bounded by the pixmap budget"
    )]
    let viewport = svg.viewport_transform(width as f32, height as f32);
    anyrender_svg::render_svg_tree(
        &mut painter,
        &svg.tree,
        kurbo::Affine::new([
            f64::from(viewport.sx),
            f64::from(viewport.ky),
            f64::from(viewport.kx),
            f64::from(viewport.sy),
            f64::from(viewport.tx),
            f64::from(viewport.ty),
        ]),
    );
    let pixmap = painter.pixmap();
    let mut pixels = Vec::with_capacity(bytes);
    for pixel in pixmap.pixels() {
        let color = pixel.demultiply();
        pixels.extend_from_slice(&[color.red(), color.green(), color.blue(), color.alpha()]);
    }
    Some(pixels)
}

/// A raster size clamped to the pixmap budget by shrinking both sides
/// proportionally.
fn fit_raster(width: f32, height: f32) -> Option<(u32, u32)> {
    if !(width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0) {
        return None;
    }
    let mut width = f64::from(width).ceil().max(1.0);
    let mut height = f64::from(height).ceil().max(1.0);
    #[expect(
        clippy::cast_precision_loss,
        reason = "the budget is 64 MiB, exactly representable in f64"
    )]
    let budget = crate::js::world::MAX_CANVAS_BYTES as f64;
    let pixels = width * height * 4.0;
    if pixels > budget {
        let factor = (budget / pixels).sqrt();
        width = (width * factor).floor().max(1.0);
        height = (height * factor).floor().max(1.0);
    }
    Some((width.to_u32()?, height.to_u32()?))
}

/// A raster side in device pixels. Raster sizes are bounded by
/// [`crate::js::world::MAX_CANVAS_BYTES`], far inside the exact `f32` range.
fn to_f32(value: u32) -> f32 {
    #[expect(
        clippy::cast_precision_loss,
        reason = "raster sides are bounded well below 2^24"
    )]
    let value = value as f32;
    value
}

// ---- text ----------------------------------------------------------------

fn fill_text(
    this: This<Object<'_>>,
    text: DomString,
    x: Coerced<f64>,
    y: Coerced<f64>,
    max_width: Opt<Coerced<f64>>,
) -> Result<()> {
    draw_text(this, &text, *x, *y, max_width.0.as_deref().copied(), false)
}

fn stroke_text(
    this: This<Object<'_>>,
    text: DomString,
    x: Coerced<f64>,
    y: Coerced<f64>,
    max_width: Opt<Coerced<f64>>,
) -> Result<()> {
    draw_text(this, &text, *x, *y, max_width.0.as_deref().copied(), true)
}

fn fill_text_cluster<'js>(
    this: This<Object<'js>>,
    cluster: Class<'js, JsTextCluster>,
    x: Coerced<f64>,
    y: Coerced<f64>,
    options: Opt<Value<'js>>,
) -> Result<()> {
    draw_text_cluster(this, &cluster, *x, *y, options, false)
}

fn stroke_text_cluster<'js>(
    this: This<Object<'js>>,
    cluster: Class<'js, JsTextCluster>,
    x: Coerced<f64>,
    y: Coerced<f64>,
    options: Opt<Value<'js>>,
) -> Result<()> {
    draw_text_cluster(this, &cluster, *x, *y, options, true)
}

/// Paints one measured cluster from the shaping recorded by `measureText`. The
/// glyph outlines are the measured ones, so a font or spacing change after
/// measurement cannot reshape the cluster, and drawing every cluster at the
/// same anchor reproduces `fillText` exactly
/// (<https://github.com/whatwg/html/issues/10677>).
fn draw_text_cluster<'js>(
    this: This<Object<'js>>,
    cluster: &Class<'js, JsTextCluster>,
    x: f64,
    y: f64,
    options: Opt<Value<'js>>,
    stroke: bool,
) -> Result<()> {
    if !x.is_finite() || !y.is_finite() {
        return Ok(());
    }
    let (record, index, cluster_align, cluster_baseline) = {
        let cluster = cluster.borrow();
        (
            std::rc::Rc::clone(&cluster.record),
            cluster.index,
            cluster.align,
            cluster.baseline,
        )
    };
    let ctx = this.0.ctx();
    let options = read_text_options(ctx, options.0.as_ref())?;
    let cluster = &record.shaped.clusters[index];
    let Some(path) = cluster.path.as_ref() else {
        return Ok(());
    };
    let align = options
        .align
        .as_deref()
        .map_or(cluster_align, |value| resolve_alignment(value, record.rtl));
    let baseline = options
        .baseline
        .as_deref()
        .and_then(state::TextBaseline::parse)
        .unwrap_or(cluster_baseline);
    let scale_x = record.shaped.scale_x;
    let advance = cluster.advance * scale_x;
    // Blink replaces the cluster's recorded position with an explicit option and
    // otherwise uses it as-is. That position was computed from the *measured*
    // alignment, so drawing adds this call's alignment delta on top.
    let cluster_x = f64::from(
        cluster.offset * scale_x + align_delta(advance, cluster_align)
            - -alignment_offset(&record.shaped, record.align),
    );
    let cluster_y = f64::from(
        baseline_offset(&record.shaped, record.baseline)
            - baseline_offset(&record.shaped, cluster_baseline),
    );
    let offset_x = options.x.unwrap_or(cluster_x);
    let offset_y = options.y.unwrap_or(cluster_y);
    let tx = f32_of(x + offset_x) + alignment_offset_width(advance, align);
    let ty = f32_of(y + offset_y) + baseline_offset(&record.shaped, baseline);
    with_canvas(ctx, &this.0, |canvas| {
        if stroke {
            canvas.stroke_text_path(path, tx, ty, scale_x);
        } else {
            canvas.fill_text_path(path, tx, ty, scale_x);
        }
    })
}

fn draw_text(
    this: This<Object<'_>>,
    text: &str,
    x: f64,
    y: f64,
    max_width: Option<f64>,
    stroke: bool,
) -> Result<()> {
    draw_text_with(this, text, x, y, max_width, stroke)
}

/// Horizontal shift from the text anchor to the alignment point. Negative
/// shifts move the string left for `center` and `right`.
fn alignment_offset_width(width: f32, align: text::ResolvedAlign) -> f32 {
    match align {
        text::ResolvedAlign::Center => -width / 2.0,
        text::ResolvedAlign::Right => -width,
        text::ResolvedAlign::Left => 0.0,
    }
}

fn alignment_offset(shaped: &text::ShapedText, align: text::ResolvedAlign) -> f32 {
    alignment_offset_width(shaped.width * shaped.scale_x, align)
}

/// Blink's `getTextAlignDelta`: the distance from the anchor to the text
/// origin, positive in the inline direction. It is the negation of
/// [`alignment_offset_width`], which is the shift applied at draw time
/// (<https://source.chromium.org/chromium/chromium/src/+/main:third_party/blink/renderer/core/html/canvas/text_metrics.cc>).
fn align_delta(width: f32, align: text::ResolvedAlign) -> f32 {
    -alignment_offset_width(width, align)
}

/// Vertical shift from the top of the line box to the requested baseline.
/// All five text operations share this to keep the signs consistent.
fn baseline_offset(shaped: &text::ShapedText, baseline: state::TextBaseline) -> f32 {
    match baseline {
        state::TextBaseline::Top => 0.0,
        state::TextBaseline::Hanging => shaped.hanging_baseline - shaped.ascent,
        state::TextBaseline::Middle => -(shaped.ascent + shaped.descent) / 2.0,
        state::TextBaseline::Bottom => -(shaped.ascent + shaped.descent),
        state::TextBaseline::Ideographic => shaped.ideographic_baseline - shaped.ascent,
        state::TextBaseline::Alphabetic => -shaped.ascent,
    }
}

fn draw_text_with(
    this: This<Object<'_>>,
    text: &str,
    x: f64,
    y: f64,
    max_width: Option<f64>,
    stroke: bool,
) -> Result<()> {
    if !x.is_finite() || !y.is_finite() {
        return Ok(());
    }
    let ctx = this.0.ctx();
    let (font, align, baseline, letter_spacing, word_spacing, lang, kerning) =
        with_canvas(ctx, &this.0, |canvas| {
            (
                canvas.font().to_owned(),
                canvas.text_align().to_owned(),
                canvas.text_baseline().to_owned(),
                canvas.letter_spacing().to_owned(),
                canvas.word_spacing().to_owned(),
                canvas.lang().to_owned(),
                canvas.font_kerning().to_owned(),
            )
        })?;
    let language = resolved_language(ctx, &this.0, &lang);
    let font_size = text::font_size(&font);
    let letter_spacing = text::length_px(&letter_spacing, font_size);
    let word_spacing = text::length_px(&word_spacing, font_size);
    let max_width = match max_width {
        None => None,
        Some(value) if value.is_finite() && value > 0.0 => Some(f32_of(value)),
        // A zero, negative, or non-finite maxWidth paints nothing
        // (<https://html.spec.whatwg.org/multipage/canvas.html#dom-context-2d-filltext>).
        Some(_) => return Ok(()),
    };
    let mut font_ctx = document_font_context(ctx, &this.0)?;
    let shaped = text::shape(
        text,
        &font,
        max_width,
        letter_spacing,
        word_spacing,
        language.as_deref(),
        &kerning,
        &mut font_ctx,
        text::Shaping::Glyphs,
    );
    let Some(path) = shaped.path.as_ref() else {
        return Ok(());
    };
    let direction = with_canvas(ctx, &this.0, |canvas| canvas.direction().to_owned())?;
    let resolved = resolved_alignment(ctx, &this.0, &align, &direction);
    let dx = alignment_offset(&shaped, resolved);
    // The shaped layout's origin is the top of the line, so shift it up by
    // the distance from the top to the chosen baseline
    // (<https://html.spec.whatwg.org/multipage/canvas.html#text-baselines>).
    let baseline = state::TextBaseline::parse(&baseline).unwrap_or(state::TextBaseline::Alphabetic);
    let dy = baseline_offset(&shaped, baseline);
    let (x, y) = (f32_of(x) + dx, f32_of(y) + dy);
    with_canvas(ctx, &this.0, |canvas| {
        if stroke {
            canvas.stroke_text_path(path, x, y, shaped.scale_x);
        } else {
            canvas.fill_text_path(path, x, y, shaped.scale_x);
        }
    })
}

/// UTF-16 ranges come from `WebIDL` `unsigned long`; negatives are a `TypeError`
/// and a range outside the string or reversed is an `IndexSizeError`
/// (<https://html.spec.whatwg.org/multipage/canvas.html#dom-textmetrics-gettextclusters>).
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "indices were validated non-negative and within the UTF-16 length"
)]
fn text_range<'js>(
    ctx: &Ctx<'js>,
    text: &str,
    args: &[Value<'js>],
    default_full: bool,
    allow_reversed: bool,
) -> Result<(usize, usize)> {
    let length = text.encode_utf16().count();
    if args.is_empty() {
        if default_full {
            return Ok((0, length));
        }
        return Err(Exception::throw_type(
            ctx,
            "a start and end index are required",
        ));
    }
    let start = *Coerced::<f64>::from_js(ctx, args[0].clone())?;
    let end = match args.get(1) {
        Some(value) => Some(*Coerced::<f64>::from_js(ctx, value.clone())?),
        None if default_full => Some(length as f64),
        None => None,
    };
    if start < 0.0 || end.is_some_and(|end| end < 0.0) {
        return Err(Exception::throw_type(ctx, "indices must be non-negative"));
    }
    let Some(end) = end else {
        return Err(Exception::throw_type(ctx, "an end index is required"));
    };
    let (start, end) = (start as usize, end as usize);
    if start > length || end > length || (!allow_reversed && start > end) {
        return Err(crate::js::bindings::throw_dom(
            ctx,
            "IndexSizeError",
            &format!("index is outside the text (start {start}, end {end}, len {length})"),
        ));
    }
    Ok((start, end))
}

/// Reads the `TextClusterOptions` dictionary in `WebIDL` member order. A member
/// that is absent or `undefined` stays `None`, so "not passed" is distinct
/// from an explicit `0`.
#[derive(Default)]
struct TextOptions {
    align: Option<String>,
    baseline: Option<String>,
    x: Option<f64>,
    y: Option<f64>,
}

fn read_text_options<'js>(ctx: &Ctx<'js>, value: Option<&Value<'js>>) -> Result<TextOptions> {
    let mut options = TextOptions::default();
    let Some(object) = value.and_then(Value::as_object) else {
        return Ok(options);
    };
    options.align = object.get::<_, Option<String>>("align")?;
    options.baseline = object.get::<_, Option<String>>("baseline")?;
    options.x = object.get::<_, Option<f64>>("x")?;
    options.y = object.get::<_, Option<f64>>("y")?;
    // WebIDL enum conversion throws `TypeError` for a value outside the enum;
    // a silently ignored typo would measure at a different alignment than the
    // page asked for.
    if let Some(align) = &options.align
        && !matches!(
            align.as_str(),
            "start" | "end" | "left" | "right" | "center"
        )
    {
        return Err(Exception::throw_type(ctx, "invalid CanvasTextAlign value"));
    }
    if let Some(baseline) = &options.baseline
        && state::TextBaseline::parse(baseline).is_none()
    {
        return Err(Exception::throw_type(
            ctx,
            "invalid CanvasTextBaseline value",
        ));
    }
    Ok(options)
}

/// The shaping a `TextMetrics` method was called on. Extracting the class
/// clones the `Rc` and drops the borrow before the caller runs any page
/// script, so a getter or `valueOf` cannot re-enter a borrowed canvas.
fn metrics_record(this: &This<Object<'_>>) -> Result<std::rc::Rc<text::TextMetricsRecord>> {
    let metrics = Class::<JsTextMetrics>::from_object(&this.0)
        .ok_or_else(|| Exception::throw_type(this.0.ctx(), "illegal invocation"))?;
    Ok(std::rc::Rc::clone(&metrics.borrow().record))
}

fn cluster_parts(
    this: &This<Object<'_>>,
) -> Result<(
    std::rc::Rc<text::TextMetricsRecord>,
    usize,
    text::ResolvedAlign,
    state::TextBaseline,
)> {
    let cluster = Class::<JsTextCluster>::from_object(&this.0)
        .ok_or_else(|| Exception::throw_type(this.0.ctx(), "illegal invocation"))?;
    let borrow = cluster.borrow();
    Ok((
        std::rc::Rc::clone(&borrow.record),
        borrow.index,
        borrow.align,
        borrow.baseline,
    ))
}

/// Builds a `TextCluster` bound to this realm's prototype.
fn text_cluster_object<'js>(
    ctx: &Ctx<'js>,
    record: std::rc::Rc<text::TextMetricsRecord>,
    index: usize,
    align: text::ResolvedAlign,
    baseline: state::TextBaseline,
) -> Result<Value<'js>> {
    let prototype = world(ctx)?
        .borrow()
        .brand("TextCluster")
        .ok_or_else(|| Exception::throw_internal(ctx, "TextCluster has no prototype"))?
        .restore(ctx)?;
    Ok(crate::js::bindings::host::instance_proto(
        JsTextCluster {
            record,
            index,
            align,
            baseline,
        },
        prototype,
    )?
    .into_value())
}

/// The `actualBoundingBox*` distances for one ink box, in the record's
/// alignment and baseline. `(left, right, ascent, descent)`.
fn bounding_box(record: &text::TextMetricsRecord, ink: text::ClusterInk) -> (f32, f32, f32, f32) {
    // The alignment point sits at `-align_dx` in text space; distances are
    // measured from it, so `left` grows leftward and `right` rightward.
    let align_dx = alignment_offset(&record.shaped, record.align);
    let dy = baseline_offset(&record.shaped, record.baseline);
    let shift = -record.shaped.ascent - dy;
    (
        -align_dx - ink.min_x,
        ink.max_x + align_dx,
        ink.ascent + shift,
        ink.descent - shift,
    )
}

fn no_ink() -> text::ClusterInk {
    text::ClusterInk {
        min_x: 0.0,
        max_x: 0.0,
        ascent: 0.0,
        descent: 0.0,
    }
}

/// The union of the ink of every cluster whose start index falls in
/// `[start, end)`. Blink attributes each glyph to its cluster's start
/// character, so a range that excludes a ligature's start contributes no ink;
/// this follows that rule rather than intersecting the cluster's span. The
/// WPT reference shapes a space-padded copy of the range, which is a third
/// answer this engine does not reproduce (the suite only measures `ABCDE`).
fn range_ink(record: &text::TextMetricsRecord, start: usize, end: usize) -> text::ClusterInk {
    if start >= end {
        return no_ink();
    }
    record
        .shaped
        .clusters
        .iter()
        .filter(|cluster| {
            let start_of_cluster = cluster.utf16_start as usize;
            start_of_cluster >= start && start_of_cluster < end
        })
        .filter_map(|cluster| cluster.ink)
        .reduce(text::ClusterInk::union)
        .unwrap_or_else(no_ink)
}

fn get_text_clusters<'js>(
    this: This<Object<'js>>,
    args: rquickjs::function::Rest<Value<'js>>,
) -> Result<Array<'js>> {
    let ctx = this.0.ctx();
    let record = metrics_record(&this)?;
    // The options-only overload takes a dictionary first; the ranged overload
    // takes `start`, `end`, then an optional dictionary
    // (<https://github.com/whatwg/html/issues/10677>).
    let (range_args, options_value): (&[Value<'js>], Option<&Value<'js>>) =
        if args.0.first().is_some_and(Value::is_object) {
            (&[], args.0.first())
        } else {
            (&args.0[..], args.0.get(2))
        };
    let (start, end) = text_range(ctx, &record.text, range_args, true, false)?;
    let options = read_text_options(ctx, options_value)?;
    let align = options
        .align
        .as_deref()
        .map_or(record.align, |value| resolve_alignment(value, record.rtl));
    let baseline = options
        .baseline
        .as_deref()
        .and_then(state::TextBaseline::parse)
        .unwrap_or(record.baseline);
    let array = Array::new(ctx.clone())?;
    if start >= end {
        return Ok(array);
    }
    for (index, cluster) in record.shaped.clusters.iter().enumerate() {
        if (cluster.utf16_start as usize) >= end || (cluster.utf16_end as usize) <= start {
            continue;
        }
        array.set(
            array.len(),
            text_cluster_object(ctx, std::rc::Rc::clone(&record), index, align, baseline)?,
        )?;
    }
    Ok(array)
}

fn get_actual_bounding_box<'js>(
    this: This<Object<'js>>,
    args: rquickjs::function::Rest<Value<'js>>,
) -> Result<Value<'js>> {
    let ctx = this.0.ctx();
    let record = metrics_record(&this)?;
    let (start, end) = text_range(ctx, &record.text, &args.0, false, false)?;
    let (left, right, ascent, descent) = bounding_box(&record, range_ink(&record, start, end));
    crate::js::bindings::geometry::dom_rect(
        ctx,
        f64::from(-left),
        f64::from(-ascent),
        f64::from(left + right),
        f64::from(ascent + descent),
    )
}

fn get_selection_rects<'js>(
    this: This<Object<'js>>,
    args: rquickjs::function::Rest<Value<'js>>,
) -> Result<Array<'js>> {
    let ctx = this.0.ctx();
    let record = metrics_record(&this)?;
    let (start, end) = text_range(ctx, &record.text, &args.0, false, true)?;
    // A reversed or empty range has no selection.
    let array = Array::new(ctx.clone())?;
    if start >= end {
        return Ok(array);
    }
    let shaped = &record.shaped;
    let align_dx = alignment_offset(shaped, record.align);
    let baseline_dy = baseline_offset(shaped, record.baseline);
    let mut selected: Vec<(f32, f32)> = shaped
        .clusters
        .iter()
        .filter(|cluster| {
            (cluster.utf16_start as usize) < end && (cluster.utf16_end as usize) > start
        })
        .map(|cluster| {
            (
                cluster.offset * shaped.scale_x,
                (cluster.offset + cluster.advance) * shaped.scale_x,
            )
        })
        .collect();
    selected.sort_by(|left, right| left.0.total_cmp(&right.0));
    let mut runs: Vec<(f32, f32)> = Vec::new();
    for (left, right) in selected {
        if let Some(last) = runs.last_mut()
            && (left - last.1).abs() <= 0.01
        {
            last.1 = last.1.max(right);
        } else {
            runs.push((left, right));
        }
    }
    let shift = -shaped.ascent - baseline_dy;
    let top = -shaped.font_ascent - shift;
    let bottom = shaped.font_descent - shift;
    for (left, right) in runs {
        let x = align_dx + left;
        let width = right - left;
        let rect = crate::js::bindings::geometry::dom_rect(
            ctx,
            f64::from(x),
            f64::from(top),
            f64::from(width),
            f64::from(bottom - top),
        )?;
        array.set(array.len(), rect)?;
    }
    Ok(array)
}

/// Nearest cluster boundary to a horizontal offset.
fn get_index_from_offset(this: This<Object<'_>>, offset: Coerced<f64>) -> Result<u32> {
    let record = metrics_record(&this)?;
    let shaped = &record.shaped;
    let width = shaped.width * shaped.scale_x;
    if width <= 0.0 {
        return Ok(0);
    }
    // The offset is measured from the alignment point, so undo the alignment
    // shift before comparing against left-to-right cluster positions.
    let target = (f32_of(*offset) - alignment_offset(shaped, record.align)).clamp(0.0, width);
    let mut best = 0_u32;
    let mut best_distance = f32::MAX;
    for cluster in &shaped.clusters {
        let start = cluster.offset * shaped.scale_x;
        let end = start + cluster.advance * shaped.scale_x;
        for (boundary, position) in [(cluster.utf16_start, start), (cluster.utf16_end, end)] {
            let distance = (position - target).abs();
            if distance < best_distance {
                best_distance = distance;
                best = boundary;
            }
        }
    }
    Ok(best)
}

fn measure_text(this: This<Object<'_>>, text: DomString) -> Result<Value<'_>> {
    let ctx = this.0.ctx();
    let (font, align, baseline, direction, letter_spacing, word_spacing, lang, kerning) =
        with_canvas(ctx, &this.0, |canvas| {
            (
                canvas.font().to_owned(),
                canvas.text_align().to_owned(),
                canvas.text_baseline().to_owned(),
                canvas.direction().to_owned(),
                canvas.letter_spacing().to_owned(),
                canvas.word_spacing().to_owned(),
                canvas.lang().to_owned(),
                canvas.font_kerning().to_owned(),
            )
        })?;
    let language = resolved_language(ctx, &this.0, &lang);
    let font_size = text::font_size(&font);
    let letter_spacing = text::length_px(&letter_spacing, font_size);
    let word_spacing = text::length_px(&word_spacing, font_size);
    let mut font_ctx = document_font_context(ctx, &this.0)?;
    let shaped = text::shape(
        &text,
        &font,
        None,
        letter_spacing,
        word_spacing,
        language.as_deref(),
        &kerning,
        &mut font_ctx,
        text::Shaping::Clusters,
    );
    // Canvas `width` is a double, and `word-spacing` is a double of layout
    // units. Parley accumulates spacing in f32, which rounds the total before
    // the caller adds the same spacing in f64 (WPT's wordSpacing.change.font
    // compares exactly). Shape once without spacing and add it in f64.
    let width = if word_spacing == 0.0 {
        f64::from(shaped.width)
    } else {
        let unspaced = text::shape(
            &text,
            &font,
            None,
            letter_spacing,
            0.0,
            language.as_deref(),
            &kerning,
            &mut font_ctx,
            text::Shaping::Width,
        );
        let spaces = text.chars().filter(char::is_ascii_whitespace).count();
        #[expect(
            clippy::cast_precision_loss,
            reason = "a string with more than 2^53 spaces cannot fit in memory"
        )]
        let spacing = f64::from(word_spacing) * spaces as f64;
        f64::from(unspaced.width) + spacing
    };
    // Keep positive zero so `TextMetrics` does not expose `-0`.
    let width = if width == 0.0 { 0.0 } else { width };
    // Freeze the two anchor values at measure time, so a later context change
    // cannot alter what this snapshot reports.
    let rtl = resolved_direction(ctx, &this.0, &direction);
    let align = resolve_alignment(&align, rtl);
    let baseline = state::TextBaseline::parse(&baseline).unwrap_or(state::TextBaseline::Alphabetic);
    let record = std::rc::Rc::new(text::TextMetricsRecord {
        text: text.to_owned(),
        shaped,
        align,
        baseline,
        rtl,
        width,
    });
    let prototype = world(ctx)?
        .borrow()
        .brand("TextMetrics")
        .ok_or_else(|| Exception::throw_internal(ctx, "TextMetrics has no prototype"))?
        .restore(ctx)?;
    Ok(
        crate::js::bindings::host::instance_proto(JsTextMetrics { record }, prototype)?
            .into_value(),
    )
}

fn metrics_width(this: This<Object<'_>>) -> Result<f64> {
    Ok(metrics_record(&this)?.width)
}

fn metrics_actual_bounding_box_left(this: This<Object<'_>>) -> Result<f64> {
    let record = metrics_record(&this)?;
    let ink = record.shaped.ink.unwrap_or_else(no_ink);
    Ok(f64::from(bounding_box(&record, ink).0))
}

fn metrics_actual_bounding_box_right(this: This<Object<'_>>) -> Result<f64> {
    let record = metrics_record(&this)?;
    let ink = record.shaped.ink.unwrap_or_else(no_ink);
    Ok(f64::from(bounding_box(&record, ink).1))
}

fn metrics_actual_bounding_box_ascent(this: This<Object<'_>>) -> Result<f64> {
    let record = metrics_record(&this)?;
    let ink = record.shaped.ink.unwrap_or_else(no_ink);
    Ok(f64::from(bounding_box(&record, ink).2))
}

fn metrics_actual_bounding_box_descent(this: This<Object<'_>>) -> Result<f64> {
    let record = metrics_record(&this)?;
    let ink = record.shaped.ink.unwrap_or_else(no_ink);
    Ok(f64::from(bounding_box(&record, ink).3))
}

fn metrics_font_bounding_box_ascent(this: This<Object<'_>>) -> Result<f64> {
    let record = metrics_record(&this)?;
    let shift = -record.shaped.ascent - baseline_offset(&record.shaped, record.baseline);
    Ok(f64::from(record.shaped.font_ascent + shift))
}

fn metrics_font_bounding_box_descent(this: This<Object<'_>>) -> Result<f64> {
    let record = metrics_record(&this)?;
    let shift = -record.shaped.ascent - baseline_offset(&record.shaped, record.baseline);
    Ok(f64::from(record.shaped.font_descent - shift))
}

fn metrics_em_height_ascent(this: This<Object<'_>>) -> Result<f64> {
    let record = metrics_record(&this)?;
    let shift = -record.shaped.ascent - baseline_offset(&record.shaped, record.baseline);
    Ok(f64::from(record.shaped.em_ascent + shift))
}

fn metrics_em_height_descent(this: This<Object<'_>>) -> Result<f64> {
    let record = metrics_record(&this)?;
    let shift = -record.shaped.ascent - baseline_offset(&record.shaped, record.baseline);
    Ok(f64::from(record.shaped.em_descent - shift))
}

fn metrics_hanging_baseline(this: This<Object<'_>>) -> Result<f64> {
    let record = metrics_record(&this)?;
    let shift = -record.shaped.ascent - baseline_offset(&record.shaped, record.baseline);
    Ok(f64::from(record.shaped.hanging_baseline + shift))
}

fn metrics_alphabetic_baseline(this: This<Object<'_>>) -> Result<f64> {
    let record = metrics_record(&this)?;
    Ok(f64::from(
        -record.shaped.ascent - baseline_offset(&record.shaped, record.baseline),
    ))
}

fn metrics_ideographic_baseline(this: This<Object<'_>>) -> Result<f64> {
    let record = metrics_record(&this)?;
    let shift = -record.shaped.ascent - baseline_offset(&record.shaped, record.baseline);
    Ok(f64::from(record.shaped.ideographic_baseline + shift))
}

fn cluster_begin(this: This<Object<'_>>) -> Result<u32> {
    let (record, index, _, _) = cluster_parts(&this)?;
    Ok(record.shaped.clusters[index].utf16_start)
}

fn cluster_end(this: This<Object<'_>>) -> Result<u32> {
    let (record, index, _, _) = cluster_parts(&this)?;
    Ok(record.shaped.clusters[index].utf16_end)
}

fn cluster_x(this: This<Object<'_>>) -> Result<f64> {
    let (record, index, align, _) = cluster_parts(&this)?;
    let cluster = &record.shaped.clusters[index];
    let advance = cluster.advance * record.shaped.scale_x;
    // Blink: `cluster.x_position + getTextAlignDelta(cluster.width, option) -
    // text_align_dx`, where `text_align_dx` is the measured alignment's
    // distance from the anchor to the text origin.
    let text_align_dx = -alignment_offset(&record.shaped, record.align);
    Ok(f64::from(
        cluster.offset * record.shaped.scale_x + align_delta(advance, align) - text_align_dx,
    ))
}

fn cluster_y(this: This<Object<'_>>) -> Result<f64> {
    let (record, _, _, baseline) = cluster_parts(&this)?;
    // Blink: `GetFontBaseline(measured) - GetFontBaseline(option)`, which is
    // the difference of the two baseline offsets.
    Ok(f64::from(
        baseline_offset(&record.shaped, record.baseline)
            - baseline_offset(&record.shaped, baseline),
    ))
}

fn cluster_text(this: This<Object<'_>>) -> Result<std::string::String> {
    let (record, index, _, _) = cluster_parts(&this)?;
    Ok(record.shaped.clusters[index].text.clone())
}

fn cluster_align(this: This<Object<'_>>) -> Result<&'static str> {
    Ok(cluster_parts(&this)?.2.as_str())
}

fn cluster_baseline(this: This<Object<'_>>) -> Result<&'static str> {
    Ok(cluster_parts(&this)?.3.as_str())
}

fn get_filter(this: This<Object<'_>>) -> Result<Value<'_>> {
    let ctx = this.0.ctx().clone();
    // The filter object is drawing state. `save()` clones it with the rest of
    // `State`, so `restore()` brings back both the object and its matrix.
    if let Some(object) = with_canvas(&ctx, &this.0, |canvas| canvas.filter_object())? {
        return object.restore(&ctx);
    }
    let filter = with_canvas(&ctx, &this.0, |canvas| canvas.filter().to_owned())?;
    Ok(rquickjs::String::from_str(ctx, &filter)?.into_value())
}

fn set_filter_binding<'js>(this: This<Object<'js>>, value: Value<'js>) -> Result<()> {
    let ctx = this.0.ctx();
    let is_canvas_filter = value.as_object().is_some_and(|object| {
        ctx.globals()
            .get::<_, Function>("CanvasFilter")
            .is_ok_and(|ctor| object.is_instance_of(&ctor))
    });
    if is_canvas_filter {
        let matrix = color_matrix_from_filter(&value);
        let object = rquickjs::Persistent::save(ctx, value);
        return with_canvas(ctx, &this.0, move |canvas| {
            canvas.set_filter_object(Some(object));
            canvas.set_filter_matrix(matrix);
        });
    }
    let text = DomString::from_js(ctx, value)?.to_string();
    if !valid_filter(&text) {
        return Ok(());
    }
    with_canvas(ctx, &this.0, move |canvas| {
        canvas.set_filter_object(None);
        canvas.set_filter(&text);
        canvas.set_filter_matrix(None);
    })
}

/// Reads a `colorMatrix` filter init dictionary (or a list of them) into one
/// matrix. Returns `None` for filters this engine cannot represent as a color
/// matrix, so the drawing path stays unfiltered instead of wrong.
fn color_matrix_from_filter(value: &Value<'_>) -> Option<ColorMatrix> {
    let object = value.as_object()?;
    let init = object
        .get::<_, Value>("__tbInit")
        .ok()
        .filter(|value| !value.is_undefined())
        .unwrap_or_else(|| value.clone());
    if let Some(array) = init.as_array() {
        let mut matrix = ColorMatrix::IDENTITY;
        for index in 0..array.len() {
            let item = array.get::<Value>(index).ok()?;
            matrix = matrix.then(color_matrix_from_single(&item)?);
        }
        return Some(matrix);
    }
    color_matrix_from_single(&init)
}

fn color_matrix_from_single(value: &Value<'_>) -> Option<ColorMatrix> {
    let object = value.as_object()?;
    let name: String = object.get("name").ok()?;
    if name != "colorMatrix" {
        return None;
    }
    let kind: Option<String> = object.get("type").ok();
    match kind.as_deref() {
        // `feColorMatrix` defaults: hueRotate 0 degrees, saturate 1.
        Some("hueRotate") => {
            let degrees = object
                .get::<_, Coerced<f64>>("values")
                .ok()
                .map_or(0.0, |value| *value);
            Some(ColorMatrix::hue_rotate(f32_of(degrees)))
        }
        Some("saturate") => {
            let amount = object
                .get::<_, Coerced<f64>>("values")
                .ok()
                .map_or(1.0, |value| *value);
            Some(ColorMatrix::saturate(f32_of(amount)))
        }
        Some("luminanceToAlpha") => Some(ColorMatrix::luminance_to_alpha()),
        Some("matrix") | None => {
            let values = object.get::<_, Array>("values").ok()?;
            if values.len() != 20 {
                return None;
            }
            let mut matrix = [0.0; 20];
            for (index, slot) in matrix.iter_mut().enumerate() {
                let value: Coerced<f64> = values.get(index).ok()?;
                *slot = f32_of(*value);
            }
            Some(ColorMatrix::from_values(matrix))
        }
        // An unknown `type` is not a color matrix this engine can apply.
        Some(_) => None,
    }
}

/// Index of the `)` matching the opening `(` that was already consumed,
/// skipping quoted strings and nested function calls.
fn matching_paren(text: &str) -> Option<usize> {
    let mut depth = 0_u32;
    let mut quote = None;
    let mut escaped = false;
    for (index, ch) in text.char_indices() {
        if let Some(delimiter) = quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == delimiter {
                quote = None;
            }
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            '(' => depth += 1,
            ')' if depth == 0 => return Some(index),
            ')' => depth -= 1,
            _ => {}
        }
    }
    None
}

/// A CSS filter list with each function's argument shape checked. The canvas
/// stores the serialized string; invalid values leave the previous filter
/// (<https://drafts.fxtf.org/filter-effects/#filter-property>).
fn valid_filter(text: &str) -> bool {
    let mut rest = text.trim();
    if rest.eq_ignore_ascii_case("none") {
        return true;
    }
    if rest.is_empty() {
        return false;
    }
    while !rest.is_empty() {
        let name_length = rest
            .find(|ch: char| !(ch.is_ascii_alphabetic() || ch == '-'))
            .unwrap_or(rest.len());
        let (name, after_name) = rest.split_at(name_length);
        if name.is_empty() {
            return false;
        }
        let after_name = after_name.trim_start();
        let Some(after_open) = after_name.strip_prefix('(') else {
            return false;
        };
        let Some(close) = matching_paren(after_open) else {
            return false;
        };
        let argument = after_open[..close].trim();
        let name = name.to_ascii_lowercase();
        let valid = match name.as_str() {
            "blur" => argument
                .strip_suffix("px")
                .or_else(|| (argument == "0").then_some(""))
                .and_then(|value| value.trim().parse::<f32>().ok())
                .is_some_and(|value| value.is_finite() && value >= 0.0),
            "brightness" | "contrast" | "grayscale" | "invert" | "opacity" | "saturate"
            | "sepia" => percentage_or_number(argument),
            "hue-rotate" => angle(argument),
            "drop-shadow" | "url" => !argument.is_empty(),
            _ => false,
        };
        if !valid {
            return false;
        }
        rest = after_open[close + 1..].trim_start();
    }
    true
}

fn percentage_or_number(text: &str) -> bool {
    let text = text.strip_suffix('%').unwrap_or(text).trim();
    text.parse::<f32>()
        .is_ok_and(|value| value.is_finite() && value >= 0.0)
}

fn angle(text: &str) -> bool {
    let number = ["deg", "rad", "turn", "grad"]
        .iter()
        .find_map(|unit| text.strip_suffix(unit))
        .unwrap_or(text)
        .trim();
    number.parse::<f32>().is_ok_and(f32::is_finite)
}

/// The document's Parley font context. It includes `@font-face` fonts, which
/// the shared process context does not.
fn document_font_context(ctx: &Ctx<'_>, object: &Object<'_>) -> Result<parley::FontContext> {
    let world = world(ctx)?;
    let font_ctx = match context_canvas_node(ctx, object)? {
        Some(id) => world
            .borrow()
            .document(id)
            .map(|parsed| parsed.document.base.font_context()),
        // An `OffscreenCanvas` has no element, but a window realm still shapes
        // with the document's `@font-face` fonts.
        None => world
            .borrow()
            .main_document()
            .map(|parsed| parsed.document.base.font_context()),
    };
    font_ctx.map_or_else(
        || Ok(world.borrow().runtime.font_ctx.clone()),
        |font_ctx| Ok(font_ctx.lock().map_or_default(|font_ctx| font_ctx.clone())),
    )
}

// ---- style properties ----------------------------------------------------

fn get_fill_style(this: This<Object<'_>>) -> Result<Value<'_>> {
    style_value(this.0.ctx(), &this.0, state::Canvas2D::fill_style)
}

fn set_fill_style<'js>(this: This<Object<'js>>, value: Value<'js>) -> Result<()> {
    set_style_value(
        this.0.ctx(),
        &this.0,
        value,
        state::Canvas2D::set_fill_style,
    )
}

fn get_stroke_style(this: This<Object<'_>>) -> Result<Value<'_>> {
    style_value(this.0.ctx(), &this.0, state::Canvas2D::stroke_style)
}

fn set_stroke_style<'js>(this: This<Object<'js>>, value: Value<'js>) -> Result<()> {
    set_style_value(
        this.0.ctx(),
        &this.0,
        value,
        state::Canvas2D::set_stroke_style,
    )
}

fn style_value<'js>(
    ctx: &Ctx<'js>,
    object: &Object<'js>,
    read: impl FnOnce(&Canvas2D) -> StyleValue,
) -> Result<Value<'js>> {
    let style = with_canvas(ctx, object, |canvas| read(canvas))?;
    match style {
        StyleValue::Color(color) => {
            Ok(rquickjs::String::from_str(ctx.clone(), &serialize_color(color))?.into_value())
        }
        StyleValue::ColorExact(_, text) => {
            Ok(rquickjs::String::from_str(ctx.clone(), &text)?.into_value())
        }
        StyleValue::Gradient(id) | StyleValue::Pattern(id) => {
            let resources: Object = object.get("__tbRes")?;
            if let Ok(value) = resources.get::<_, Value>(id.to_string()) {
                Ok(value)
            } else {
                Ok(Value::new_undefined(ctx.clone()))
            }
        }
    }
}

/// Copies a gradient or pattern into the target canvas when the style comes
/// from another canvas
/// (<https://html.spec.whatwg.org/multipage/canvas.html#dom-context-2d-fillstyle>).
fn import_resource(
    ctx: &Ctx<'_>,
    target: &Object<'_>,
    resource: &Object<'_>,
    style: StyleValue,
) -> Result<StyleValue> {
    match style {
        StyleValue::Gradient(id) => {
            let source = context_canvas_id(ctx, resource)?;
            // Ids are per-bitmap; a target that happens to own the same id is
            // a different gradient. Only a same-canvas assignment is local.
            if source == context_canvas_id(ctx, target)? {
                return Ok(style);
            }
            let definition = {
                let world = world(ctx)?;
                let registry = world.borrow().registry();
                registry.borrow().canvas_gradient(source, id)
            };
            let Some(definition) = definition else {
                return Ok(style);
            };
            Ok(StyleValue::Gradient(with_canvas(ctx, target, |canvas| {
                canvas.insert_gradient(definition)
            })?))
        }
        StyleValue::Pattern(id) => {
            let source = context_canvas_id(ctx, resource)?;
            if source == context_canvas_id(ctx, target)? {
                return Ok(style);
            }
            let definition = {
                let world = world(ctx)?;
                let registry = world.borrow().registry();
                registry.borrow().canvas_pattern(source, id)
            };
            let Some(definition) = definition else {
                return Ok(style);
            };
            Ok(StyleValue::Pattern(with_canvas(ctx, target, |canvas| {
                canvas.insert_pattern(definition)
            })?))
        }
        StyleValue::Color(_) | StyleValue::ColorExact(..) => Ok(style),
    }
}

fn set_style_value<'js>(
    ctx: &Ctx<'js>,
    object: &Object<'js>,
    value: Value<'js>,
    set: impl FnOnce(&mut Canvas2D, StyleValue),
) -> Result<()> {
    let resource = value.as_object().cloned();
    let style = if let Some(string) = value.as_string() {
        let text = string.to_string()?;
        match css_color_exact(ctx, object, &text)? {
            Some((color, serialized)) => Some(match serialized {
                Some(serialized) => StyleValue::ColorExact(color, serialized),
                None => StyleValue::Color(color),
            }),
            None => return Ok(()),
        }
    } else if let Some(gradient) = value
        .as_object()
        .and_then(|object| resource_id(object, "Gradient"))
    {
        Some(StyleValue::Gradient(gradient))
    } else if let Some(pattern) = value
        .as_object()
        .and_then(|object| resource_id(object, "Pattern"))
    {
        Some(StyleValue::Pattern(pattern))
    } else if let Some(color) = value.as_object().and_then(color_object) {
        Some(StyleValue::Color(color))
    } else {
        let text = Coerced::<rquickjs::String<'_>>::from_js(ctx, value)?.to_string()?;
        match css_color_exact(ctx, object, &text)? {
            Some((color, serialized)) => Some(match serialized {
                Some(serialized) => StyleValue::ColorExact(color, serialized),
                None => StyleValue::Color(color),
            }),
            None => return Ok(()),
        }
    };
    let Some(style) = style else {
        return Ok(());
    };
    let style = match resource {
        Some(resource) => import_resource(ctx, object, &resource, style)?,
        None => style,
    };
    with_canvas(ctx, object, |canvas| set(canvas, style))
}

/// A color object is a dictionary with numeric `r`, `g`, and `b` in the
/// 0..1 range, plus an optional `a`/`alpha`
/// (<https://drafts.csswg.org/css-color-5/#color-conversion>).
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "channels are clamped to 0..1 before scaling to 0..255"
)]
fn color_object(object: &Object<'_>) -> Option<[u8; 4]> {
    let finite = |key: &str| {
        object
            .get::<_, Coerced<f64>>(key)
            .ok()
            .map(|value| *value)
            .filter(|value| value.is_finite())
    };
    let r = finite("r")?;
    let g = finite("g")?;
    let b = finite("b")?;
    let alpha = finite("a").or_else(|| finite("alpha")).unwrap_or(1.0);
    let channel = |value: f64| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    Some([channel(r), channel(g), channel(b), channel(alpha)])
}

fn get_font(this: This<Object<'_>>) -> Result<Value<'_>> {
    let font = with_canvas(this.0.ctx(), &this.0, |canvas| canvas.font().to_owned())?;
    Ok(rquickjs::String::from_str(this.0.ctx().clone(), &font)?.into_value())
}

fn set_font_binding(this: This<Object<'_>>, value: DomString) -> Result<()> {
    let ctx = this.0.ctx();
    let style = context_canvas_node(ctx, &this.0)?
        .and_then(|id| attribute_value(ctx, id, "style").ok())
        .unwrap_or_default();
    let (base_size, line_height) = canvas_style_metrics(&style);
    let Some(font) = text::canonical_font(&value, base_size, line_height) else {
        return Ok(());
    };
    with_canvas(ctx, &this.0, |canvas| canvas.set_font(&font))
}

/// The canvas element's font-size and line-height from its inline style. A
/// canvas with no font-size resolves relative lengths against the context's
/// 10px initial font
/// (<https://html.spec.whatwg.org/multipage/canvas.html#dom-context-2d-font>).
fn canvas_style_metrics(style: &str) -> (f32, f32) {
    let mut size = 10.0;
    let mut line_height = None;
    for declaration in style.split(';') {
        let Some((name, value)) = declaration.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match name.trim().to_ascii_lowercase().as_str() {
            "font-size" => {
                if let Some(px) = value
                    .strip_suffix("px")
                    .and_then(|value| value.trim().parse::<f32>().ok())
                {
                    size = px;
                }
            }
            "line-height" => line_height = Some(value),
            _ => {}
        }
    }
    let line_height = line_height.map_or(size * 1.2, |value| {
        value
            .strip_suffix("px")
            .and_then(|value| value.trim().parse::<f32>().ok())
            .or_else(|| value.trim().parse::<f32>().ok().map(|value| value * size))
            .unwrap_or(size * 1.2)
    });
    (size, line_height)
}

/// `ctx.lang` inherits the canvas element's `lang` attribute; parley uses it
/// for locale-sensitive shaping
/// (<https://html.spec.whatwg.org/multipage/canvas.html#dom-context-2d-lang>).
fn resolved_language(ctx: &Ctx<'_>, object: &Object<'_>, lang: &str) -> Option<String> {
    if lang != "inherit" && !lang.is_empty() {
        return Some(lang.to_owned());
    }
    if let Some(id) = context_canvas_node(ctx, object).ok().flatten() {
        let owner = crate::js::bindings::world_for_node(ctx, id).ok()?;
        let owner = owner.borrow();
        let parsed = owner.document(id)?;
        // `lang` inherits from the nearest ancestor that declares it, and an
        // empty declaration stops the search
        // (<https://html.spec.whatwg.org/multipage/dom.html#attr-lang>).
        for ancestor in
            crate::dom::tree::ancestor_chain(&parsed.document.base, id.document, id.node)
        {
            if let Some(value) =
                crate::dom::tree::attr(&parsed.document.base, ancestor.node, "lang")
            {
                return (!value.trim().is_empty()).then(|| value.to_owned());
            }
        }
        return None;
    }
    // An `OffscreenCanvas` has no element of its own; the constructor step
    // inherits the document element's language
    // (<https://html.spec.whatwg.org/multipage/canvas.html#offscreencanvas-inherited-lang>).
    let world = world(ctx).ok()?;
    let owner = world.borrow();
    let parsed = owner.main_document()?;
    // A document with no element child (an empty or still-parsing tree) has no
    // document element to inherit from.
    let root = parsed.document.base.try_root_element()?;
    crate::dom::tree::attr(&parsed.document.base, root.id, "lang")
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
}

/// Resolves `start`/`end` using the context direction, then the canvas
/// element's `dir` attribute
/// (<https://html.spec.whatwg.org/multipage/canvas.html#dom-context-2d-textalign>).
fn resolved_direction(ctx: &Ctx<'_>, object: &Object<'_>, direction: &str) -> bool {
    match direction {
        "rtl" => true,
        "ltr" => false,
        _ => context_canvas_node(ctx, object)
            .ok()
            .flatten()
            .and_then(|id| attribute_value(ctx, id, "dir").ok())
            .is_some_and(|dir| dir.trim().eq_ignore_ascii_case("rtl")),
    }
}

/// Resolves `start`/`end` against a direction. The metrics record stores the
/// resolved answer, so a `TextMetrics` read never re-resolves against the
/// canvas element's current `dir`.
fn resolve_alignment(align: &str, rtl: bool) -> text::ResolvedAlign {
    match align {
        "start" if rtl => text::ResolvedAlign::Right,
        "end" if rtl => text::ResolvedAlign::Left,
        "end" | "right" => text::ResolvedAlign::Right,
        "center" => text::ResolvedAlign::Center,
        _ => text::ResolvedAlign::Left,
    }
}

fn resolved_alignment(
    ctx: &Ctx<'_>,
    object: &Object<'_>,
    align: &str,
    direction: &str,
) -> text::ResolvedAlign {
    resolve_alignment(align, resolved_direction(ctx, object, direction))
}

fn canvas_object(this: This<Object<'_>>) -> Result<Value<'_>> {
    let ctx = this.0.ctx();
    // The inspection attributes `ctx.canvas` returns the element; an
    // `OffscreenCanvas` overrides this accessor with its owner object.
    match context_canvas_node(ctx, &this.0)? {
        Some(id) => wrap_node(ctx, id),
        None => Ok(Value::new_undefined(ctx.clone())),
    }
}

/// Parses a canvas color, preserving the `color(srgb …)` serialization that
/// `fillStyle`/`strokeStyle`/`shadowColor` must report for color functions and
/// `color-mix`. `currentColor` resolves against the canvas element's computed
/// `color` at assignment time, not at paint time
/// (<https://html.spec.whatwg.org/multipage/canvas.html#dom-context-2d-fillstyle>).
fn css_color_exact(
    ctx: &Ctx<'_>,
    object: &Object<'_>,
    text: &str,
) -> Result<Option<([u8; 4], Option<String>)>> {
    let current = current_color(ctx, object)?;
    Ok(parse_canvas_color(ctx, object, text.trim(), &current)?
        .map(|parsed| (parsed.bytes, parsed.serialized)))
}

/// Parses a canvas color against the canvas document's device, so a system
/// color resolves with the document's used color scheme.
fn parse_canvas_color(
    ctx: &Ctx<'_>,
    object: &Object<'_>,
    source: &str,
    current: &style::color::AbsoluteColor,
) -> Result<Option<crate::css::color::ParsedColor>> {
    let Some(parsed) = with_canvas_document(ctx, object, |_, parsed| {
        let device = parsed.document.base.stylist_device();
        crate::css::color::parse_exact(source, Some(device), current)
    })?
    else {
        return Ok(crate::css::color::parse_exact(
            source,
            None,
            &style::color::AbsoluteColor::BLACK,
        ));
    };
    Ok(parsed)
}

/// The canvas document and, for an element context, its node. `__tbCanvasId`
/// is a writable property, so the registry decides whether the id is this
/// realm's element. An `OffscreenCanvas` has no element style; it still parses
/// colors against its realm's document device so system colors resolve.
fn with_canvas_document<T>(
    ctx: &Ctx<'_>,
    object: &Object<'_>,
    f: impl FnOnce(Option<NodeId>, &mut crate::Parsed) -> T,
) -> Result<Option<T>> {
    let world = world(ctx)?;
    let world = world.borrow();
    let (node, mut parsed) = if let Some(id) = context_canvas_node(ctx, object)? {
        if !world.owns_node(id) {
            return Ok(None);
        }
        let Some(parsed) = world.document_mut(id) else {
            return Ok(None);
        };
        (Some(id), parsed)
    } else {
        let Some(document) = world.main_document_id() else {
            return Ok(None);
        };
        let Some(parsed) = world.document_mut_by_id(document) else {
            return Ok(None);
        };
        (None, parsed)
    };
    Ok(Some(f(node, &mut parsed)))
}

/// The canvas element's computed `color`, or black when it has no computed
/// style (detached elements and offscreen canvases have none)
/// (<https://drafts.csswg.org/css-color-4/#currentcolor-color>).
fn current_color(ctx: &Ctx<'_>, object: &Object<'_>) -> Result<style::color::AbsoluteColor> {
    let Some(current) = with_canvas_document(ctx, object, |node, parsed| {
        let node = node?;
        let base = &mut parsed.document.base;
        base.resolve(0.0);
        let styles = base
            .get_node(node.node)
            .and_then(|node| node.primary_styles())?;
        Some(
            styles
                .get_inherited_text()
                .color
                .to_color_space(style::color::ColorSpace::Srgb),
        )
    })?
    else {
        return Ok(style::color::AbsoluteColor::BLACK);
    };
    Ok(current.unwrap_or(style::color::AbsoluteColor::BLACK))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vector_raster_size_upscales_to_the_destination_scale() {
        // A 10x10 SVG drawn into 100x100 user units on a 2x display needs 200
        // device pixels per side.
        let size = vector_raster_size(
            10,
            10,
            Rect::new(0.0, 0.0, 10.0, 10.0),
            Rect::new(0.0, 0.0, 100.0, 100.0),
            2.0,
            2.0,
        );
        assert_eq!(size, Some((200, 200)));
    }

    #[test]
    fn vector_raster_size_keeps_the_intrinsic_size_for_a_downscale() {
        let size = vector_raster_size(
            100,
            100,
            Rect::new(0.0, 0.0, 100.0, 100.0),
            Rect::new(0.0, 0.0, 10.0, 10.0),
            1.0,
            1.0,
        );
        assert_eq!(size, Some((100, 100)));
    }

    #[test]
    fn vector_raster_size_scales_a_source_subrect_back_to_the_whole_image() {
        // Half the image fills the same destination, so the whole raster needs
        // twice the destination's device pixels.
        let size = vector_raster_size(
            100,
            100,
            Rect::new(0.0, 0.0, 50.0, 50.0),
            Rect::new(0.0, 0.0, 100.0, 100.0),
            2.0,
            2.0,
        );
        assert_eq!(size, Some((400, 400)));
    }

    #[test]
    fn fit_raster_stays_inside_the_pixmap_budget() {
        let (width, height) = fit_raster(100_000.0, 100_000.0).expect("raster");
        assert!(crate::js::world::canvas_bytes(width, height).is_some());
        assert!(width > 0 && height > 0);
    }

    fn test_svg(markup: &str, width: u32, height: u32) -> blitz_dom::node::SvgImageData {
        blitz_dom::node::SvgImageData::from_data(
            format!(
                "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\">{markup}</svg>"
            )
            .as_bytes(),
            &anyrender_svg::usvg::Options::default(),
        )
        .expect("test SVG")
    }

    #[test]
    fn svg_raster_cache_reuses_a_raster_for_the_same_tree_and_size() {
        let svg = test_svg("<rect width=\"4\" height=\"4\"/>", 4, 4);
        let mut cache = SvgRasterCache::default();
        let pixels: Arc<[u8]> = Arc::from(vec![7u8; 4 * 4 * 4].into_boxed_slice());
        cache.insert(&svg, 4, 4, pixels.clone());
        let hit = cache.get(&svg, 4, 4).expect("cached raster");
        assert!(
            Arc::ptr_eq(&pixels, &hit),
            "a repeated draw must reuse the cached raster, not copy or redraw it"
        );
        assert!(cache.get(&svg, 5, 5).is_none(), "a new size must miss");
    }

    #[test]
    fn svg_raster_cache_keys_on_the_parsed_tree() {
        let first = test_svg("<rect width=\"4\" height=\"4\"/>", 4, 4);
        let second = test_svg("<circle cx=\"2\" cy=\"2\" r=\"2\"/>", 4, 4);
        let mut cache = SvgRasterCache::default();
        cache.insert(
            &first,
            4,
            4,
            Arc::from(vec![0u8; 4 * 4 * 4].into_boxed_slice()),
        );
        assert!(
            cache.get(&second, 4, 4).is_none(),
            "a different SVG at the same size must not reuse the raster"
        );
    }

    #[test]
    fn svg_raster_cache_evicts_to_its_entry_bound() {
        let mut cache = SvgRasterCache::default();
        for index in 0..(MAX_SVG_RASTER_ENTRIES + 2) {
            let svg = test_svg(
                &format!("<rect x=\"{index}\" width=\"1\" height=\"1\"/>"),
                1,
                1,
            );
            cache.insert(&svg, 1, 1, Arc::from(vec![0u8; 4].into_boxed_slice()));
        }
        assert!(cache.entries.len() <= MAX_SVG_RASTER_ENTRIES);
    }
}
