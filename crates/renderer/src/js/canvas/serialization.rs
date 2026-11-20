// https://html.spec.whatwg.org/multipage/canvas.html#a-serialisation-of-the-bitmap-as-a-file

use rquickjs::function::This;
use rquickjs::object::Property;
use rquickjs::prelude::Opt;
use rquickjs::promise::Promise;
use rquickjs::{
    Array, Ctx, Exception, FromJs, Function, Object, Persistent, Result, TypedArray, Value,
};

use super::state::{Canvas2D, JpegQuality};
use super::{
    CanvasHost, CanvasString, ElementCanvas, OffscreenCanvasHost, offscreen_canvas_id, world,
};
use crate::js::bindings::{dom_exception_value, throw_dom};

// https://html.spec.whatwg.org/multipage/canvas.html#dom-canvas-todataurl
#[derive(Clone, Copy)]
enum CanvasImageFormat {
    Png,
    Jpeg,
}

impl CanvasImageFormat {
    fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
        }
    }
}

pub(crate) fn install(ctx: &Ctx<'_>) -> Result<()> {
    let globals = ctx.globals();
    let blob: Function = globals.get("Blob")?;
    let blob = blob
        .into_value()
        .into_constructor()
        .ok_or_else(|| Exception::throw_internal(ctx, "Blob is not a constructor"))?;
    let set_timeout: Function = globals.get("setTimeout")?;
    {
        let world = world(ctx)?;
        let mut world = world.borrow_mut();
        world.pristine_blob = Some(Persistent::save(ctx, blob));
        world.pristine_set_timeout = Some(Persistent::save(ctx, set_timeout));
    }

    if let Ok(canvas) = globals.get::<_, Object>("HTMLCanvasElement") {
        let canvas = canvas.get::<_, Object>("prototype")?;
        define_method(&canvas, ctx, "toDataURL", 0, to_data_url)?;
        define_method(&canvas, ctx, "toBlob", 1, to_blob)?;
    }

    if let Ok(offscreen) = globals.get::<_, Object>("OffscreenCanvas") {
        let offscreen = offscreen.get::<_, Object>("prototype")?;
        define_method(&offscreen, ctx, "convertToBlob", 0, convert_to_blob)?;
    }
    Ok(())
}

// https://webidl.spec.whatwg.org/#interface-prototype-object
fn define_method<'js, F, A>(
    object: &Object<'js>,
    ctx: &Ctx<'js>,
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
    object.prop(name, Property::from(function).writable().configurable())
}

// https://html.spec.whatwg.org/multipage/canvas.html#dom-canvas-todataurl
fn format<'js>(ctx: &Ctx<'js>, mime: Option<&Value<'js>>) -> Result<CanvasImageFormat> {
    let Some(mime) = mime.filter(|mime| !mime.is_undefined()) else {
        return Ok(CanvasImageFormat::Png);
    };
    let mime = CanvasString::from_js(ctx, mime.clone())?;
    Ok(if mime.0.eq_ignore_ascii_case("image/jpeg") {
        CanvasImageFormat::Jpeg
    } else {
        CanvasImageFormat::Png
    })
}

fn quality(value: Option<f64>) -> f64 {
    value
        .filter(|value| value.is_finite() && (0.0..=1.0).contains(value))
        .unwrap_or(0.92)
}

// https://webidl.spec.whatwg.org/#idl-any
fn any_quality(value: Option<&Value<'_>>) -> f64 {
    value
        .and_then(Value::as_number)
        .filter(|value| value.is_finite() && (0.0..=1.0).contains(value))
        .unwrap_or(0.92)
}

// https://webidl.spec.whatwg.org/#es-dictionary
struct ImageEncodeOptions {
    format: CanvasImageFormat,
    quality: f64,
}

fn encode_options<'js>(ctx: &Ctx<'js>, options: Option<&Value<'js>>) -> Result<ImageEncodeOptions> {
    // https://webidl.spec.whatwg.org/#es-dictionary
    let object =
        match options {
            None => None,
            Some(value) if value.is_undefined() || value.is_null() => None,
            Some(value) => Some(value.as_object().ok_or_else(|| {
                Exception::throw_type(ctx, "options must be an object or undefined")
            })?),
        };
    let Some(object) = object else {
        return Ok(ImageEncodeOptions {
            format: CanvasImageFormat::Png,
            quality: 0.92,
        });
    };
    let quality_value: Value<'js> = object.get("quality")?;
    let quality = if quality_value.is_undefined() {
        quality(None)
    } else {
        quality(Some(
            rquickjs::Coerced::<f64>::from_js(ctx, quality_value)?.0,
        ))
    };
    let type_value: Value<'js> = object.get("type")?;
    let format = if type_value.is_undefined() {
        CanvasImageFormat::Png
    } else {
        let mime = CanvasString::from_js(ctx, type_value)?;
        if mime.0.eq_ignore_ascii_case("image/jpeg") {
            CanvasImageFormat::Jpeg
        } else {
            CanvasImageFormat::Png
        }
    };
    Ok(ImageEncodeOptions { format, quality })
}

// https://html.spec.whatwg.org/multipage/canvas.html#dom-canvas-todataurl
fn encode(
    ctx: &Ctx<'_>,
    host: &impl CanvasHost,
    format: CanvasImageFormat,
    quality: f64,
) -> Result<Option<Vec<u8>>> {
    let (width, height) = host.dimensions(ctx);
    if width == 0 || height == 0 {
        return Ok(None);
    }
    if host.has_open_layer(ctx) {
        return Err(throw_dom(ctx, "InvalidStateError", "a layer is open"));
    }
    let origin_clean = host.origin_clean(ctx);
    if origin_clean == Some(false) {
        return Err(throw_dom(
            ctx,
            "SecurityError",
            "the canvas is not origin-clean",
        ));
    }
    let quality = JpegQuality::new(quality);
    let bytes = match (format, origin_clean, host.canvas_id(ctx)) {
        (CanvasImageFormat::Png, Some(_), Some(id)) => {
            let world = world(ctx)?;
            let registry = world.borrow().registry();
            registry.borrow().canvas_png(id)
        }
        (CanvasImageFormat::Jpeg, Some(_), Some(id)) => {
            let world = world(ctx)?;
            let registry = world.borrow().registry();
            registry.borrow().canvas_jpeg(id, quality)
        }
        (CanvasImageFormat::Png, _, _) => Canvas2D::blank_png(width, height),
        (CanvasImageFormat::Jpeg, _, _) => Canvas2D::blank_jpeg(width, height, quality),
    };
    Ok(bytes)
}

// https://html.spec.whatwg.org/multipage/canvas.html#dom-canvas-todataurl
fn to_data_url<'js>(
    ctx: Ctx<'js>,
    this: This<Object<'js>>,
    mime: Opt<Value<'js>>,
    quality_value: Opt<Value<'js>>,
) -> Result<String> {
    let format = format(&ctx, mime.0.as_ref())?;
    let quality = any_quality(quality_value.0.as_ref());
    let Some(host) = ElementCanvas::resolve(&ctx, &this.0.into_value()) else {
        return Ok("data:,".to_owned());
    };
    let Some(bytes) = encode(&ctx, &host, format, quality)? else {
        return Ok("data:,".to_owned());
    };
    Ok(data_url(format, &bytes))
}

fn data_url(format: CanvasImageFormat, bytes: &[u8]) -> String {
    use base64::Engine as _;
    format!(
        "data:{};base64,{}",
        format.mime(),
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

// https://html.spec.whatwg.org/multipage/canvas.html#dom-canvas-toblob
fn to_blob<'js>(
    ctx: Ctx<'js>,
    this: This<Object<'js>>,
    callback: Function<'js>,
    mime: Opt<Value<'js>>,
    quality_value: Opt<Value<'js>>,
) -> Result<()> {
    let format = format(&ctx, mime.0.as_ref())?;
    let quality = any_quality(quality_value.0.as_ref());
    let host = ElementCanvas::resolve(&ctx, &this.0.into_value())
        .ok_or_else(|| throw_dom(&ctx, "InvalidStateError", "the canvas is not usable"))?;
    let bytes = encode(&ctx, &host, format, quality)?;
    let blob = match bytes {
        None => Value::new_null(ctx.clone()),
        Some(bytes) => blob_value(&ctx, format.mime(), bytes)?,
    };
    let callback = Persistent::save(&ctx, callback);
    let blob = Persistent::save(&ctx, blob);
    let task = Function::new(ctx.clone(), move |ctx: Ctx<'_>| -> Result<()> {
        let callback = callback.clone().restore(&ctx)?;
        let blob = blob.clone().restore(&ctx)?;
        callback.call::<_, Value>((blob,))?;
        Ok(())
    })?;
    let schedule = world(&ctx)?
        .borrow()
        .pristine_set_timeout
        .clone()
        .ok_or_else(|| Exception::throw_internal(&ctx, "setTimeout is missing"))?;
    schedule.restore(&ctx)?.call::<_, Value>((task, 0))?;
    Ok(())
}

// https://html.spec.whatwg.org/multipage/canvas.html#dom-offscreencanvas-converttoblob
fn convert_to_blob<'js>(
    ctx: Ctx<'js>,
    this: This<Object<'js>>,
    options: Opt<Value<'js>>,
) -> Result<Value<'js>> {
    let options = encode_options(&ctx, options.0.as_ref())?;
    let (promise, resolve, reject) = Promise::new(&ctx)?;
    let Some(id) = offscreen_canvas_id(&ctx, &this.0.clone().into_value())? else {
        return Err(throw_dom(
            &ctx,
            "InvalidStateError",
            "the canvas is not usable",
        ));
    };
    let mode = {
        let world = world(&ctx)?;
        let registry = world.borrow().registry();
        registry.borrow().canvas_mode(id)
    };
    if matches!(mode, Some(crate::js::world::CanvasMode::Detached)) {
        reject_dom(&ctx, &reject, "InvalidStateError", "the canvas is detached")?;
        return Ok(promise.into_value());
    }
    let host = OffscreenCanvasHost { id };
    let (width, height) = host.dimensions(&ctx);
    if width == 0 || height == 0 {
        reject_dom(&ctx, &reject, "IndexSizeError", "the canvas has no pixels")?;
        return Ok(promise.into_value());
    }
    match encode(&ctx, &host, options.format, options.quality) {
        Ok(Some(bytes)) => {
            let blob = blob_value(&ctx, options.format.mime(), bytes)?;
            resolve.call::<_, Value>((blob,))?;
        }
        Ok(None) => {
            reject_dom(
                &ctx,
                &reject,
                "EncodingError",
                "the canvas could not be encoded",
            )?;
        }
        Err(rquickjs::Error::Exception) => {
            let error = ctx.catch();
            reject.call::<_, Value>((error,))?;
        }
        Err(error) => return Err(error),
    }
    Ok(promise.into_value())
}

fn reject_dom<'js>(
    ctx: &Ctx<'js>,
    reject: &Function<'js>,
    name: &str,
    message: &str,
) -> Result<()> {
    let error = dom_exception_value(ctx, name, message)?;
    reject.call::<_, Value>((error,))?;
    Ok(())
}

fn blob_value<'js>(ctx: &Ctx<'js>, mime: &str, bytes: Vec<u8>) -> Result<Value<'js>> {
    let constructor = world(ctx)?
        .borrow()
        .pristine_blob
        .clone()
        .ok_or_else(|| Exception::throw_internal(ctx, "Blob is missing"))?
        .restore(ctx)?;
    let part = TypedArray::new(ctx.clone(), bytes)?;
    let parts = Array::new(ctx.clone())?;
    parts.set(0, part)?;
    let init = Object::new(ctx.clone())?;
    init.set("type", mime)?;
    let blob: Object = constructor.construct((parts, init))?;
    Ok(blob.into_value())
}
