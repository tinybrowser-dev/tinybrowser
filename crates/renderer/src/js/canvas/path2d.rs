// https://html.spec.whatwg.org/multipage/canvas.html#path2d-objects

use std::cell::RefCell;

use rquickjs::class::Trace;
use rquickjs::function::Opt;
use rquickjs::prelude::Coerced;
use rquickjs::{Class, Ctx, Exception, FromJs, Result, Value};

use super::state::Canvas2D;
use super::{f32_of, round_rect_radii};
use crate::js::bindings::throw_dom;

#[derive(rquickjs::JsLifetime)]
#[rquickjs::class(rename = "Path2D")]
pub(crate) struct JsPath2D {
    pub(super) canvas: RefCell<Canvas2D>,
}

impl<'js> Trace<'js> for JsPath2D {
    fn trace<'a>(&self, _tracer: rquickjs::class::Tracer<'a, 'js>) {}
}

#[rquickjs::methods]
impl JsPath2D {
    #[qjs(constructor)]
    fn new(value: Opt<Value<'_>>) -> Self {
        let mut canvas = Canvas2D::new(1, 1);
        if let Some(value) = value.0
            && let Ok(source) = Class::<JsPath2D>::from_value(&value)
        {
            let source = source.borrow();
            canvas.copy_path(&source.canvas.borrow());
        }
        Self {
            canvas: RefCell::new(canvas),
        }
    }

    #[qjs(rename = "addPath")]
    fn add_path<'js>(
        &self,
        ctx: Ctx<'js>,
        path: Class<'js, JsPath2D>,
        transform: Opt<Value<'js>>,
    ) -> Result<()> {
        let matrix = add_path_transform(&ctx, transform.0.as_ref())?;
        let source_path = {
            let source = path.borrow();
            let source = source.canvas.borrow();
            source.user_path()
        };
        if let Some(mut source_path) = source_path {
            if let Some(matrix) = matrix {
                let Some(transformed) = source_path.transform(matrix) else {
                    return Ok(());
                };
                source_path = transformed;
            }
            let mut target = self.canvas.borrow_mut();
            target.append_path(&source_path);
            if let Some(current) = target.current_point() {
                target.move_to(current.x, current.y);
            }
        }
        Ok(())
    }

    #[qjs(rename = "closePath")]
    fn close_path(&self) {
        self.canvas.borrow_mut().close_path();
    }

    #[qjs(rename = "moveTo")]
    fn move_to(&self, x: Coerced<f64>, y: Coerced<f64>) {
        self.canvas.borrow_mut().move_to(f32_of(*x), f32_of(*y));
    }

    #[qjs(rename = "lineTo")]
    fn line_to(&self, x: Coerced<f64>, y: Coerced<f64>) {
        self.canvas.borrow_mut().line_to(f32_of(*x), f32_of(*y));
    }

    #[qjs(rename = "quadraticCurveTo")]
    fn quadratic_curve_to(
        &self,
        cpx: Coerced<f64>,
        cpy: Coerced<f64>,
        x: Coerced<f64>,
        y: Coerced<f64>,
    ) {
        self.canvas
            .borrow_mut()
            .quadratic_to(f32_of(*cpx), f32_of(*cpy), f32_of(*x), f32_of(*y));
    }

    #[qjs(rename = "bezierCurveTo")]
    #[expect(
        clippy::similar_names,
        reason = "control point and endpoint coordinates are named for their axis"
    )]
    fn bezier_curve_to(
        &self,
        cp1x: Coerced<f64>,
        cp1y: Coerced<f64>,
        cp2x: Coerced<f64>,
        cp2y: Coerced<f64>,
        x: Coerced<f64>,
        y: Coerced<f64>,
    ) {
        self.canvas.borrow_mut().cubic_to(
            f32_of(*cp1x),
            f32_of(*cp1y),
            f32_of(*cp2x),
            f32_of(*cp2y),
            f32_of(*x),
            f32_of(*y),
        );
    }

    #[qjs(rename = "arc")]
    #[expect(clippy::too_many_arguments, reason = "matches the IDL member")]
    fn arc(
        &self,
        ctx: Ctx<'_>,
        x: Coerced<f64>,
        y: Coerced<f64>,
        radius: Coerced<f64>,
        start: Coerced<f64>,
        end: Coerced<f64>,
        counterclockwise: Opt<Coerced<bool>>,
    ) -> Result<()> {
        if ![*x, *y, *radius, *start, *end]
            .iter()
            .all(|value| value.is_finite())
        {
            return Ok(());
        }
        if *radius < 0.0 {
            return Err(throw_dom(&ctx, "IndexSizeError", "radius is negative"));
        }
        self.canvas.borrow_mut().arc(
            f32_of(*x),
            f32_of(*y),
            f32_of(*radius),
            f32_of(*start),
            f32_of(*end),
            counterclockwise.0.is_some_and(|value| *value),
        );
        Ok(())
    }

    #[qjs(rename = "arcTo")]
    fn arc_to(
        &self,
        ctx: Ctx<'_>,
        x1: Coerced<f64>,
        y1: Coerced<f64>,
        x2: Coerced<f64>,
        y2: Coerced<f64>,
        radius: Coerced<f64>,
    ) -> Result<()> {
        if ![*x1, *y1, *x2, *y2, *radius]
            .iter()
            .all(|value| value.is_finite())
        {
            return Ok(());
        }
        if *radius < 0.0 {
            return Err(throw_dom(&ctx, "IndexSizeError", "radius is negative"));
        }
        self.canvas.borrow_mut().arc_to(
            f32_of(*x1),
            f32_of(*y1),
            f32_of(*x2),
            f32_of(*y2),
            f32_of(*radius),
        );
        Ok(())
    }

    #[qjs(rename = "ellipse")]
    #[expect(clippy::too_many_arguments, reason = "matches the IDL member")]
    fn ellipse(
        &self,
        ctx: Ctx<'_>,
        x: Coerced<f64>,
        y: Coerced<f64>,
        radius_x: Coerced<f64>,
        radius_y: Coerced<f64>,
        rotation: Coerced<f64>,
        start: Coerced<f64>,
        end: Coerced<f64>,
        counterclockwise: Opt<Coerced<bool>>,
    ) -> Result<()> {
        if ![*x, *y, *radius_x, *radius_y, *rotation, *start, *end]
            .iter()
            .all(|value| value.is_finite())
        {
            return Ok(());
        }
        if *radius_x < 0.0 || *radius_y < 0.0 {
            return Err(throw_dom(&ctx, "IndexSizeError", "radius is negative"));
        }
        self.canvas.borrow_mut().ellipse(
            f32_of(*x),
            f32_of(*y),
            f32_of(*radius_x),
            f32_of(*radius_y),
            f32_of(*rotation),
            f32_of(*start),
            f32_of(*end),
            counterclockwise.0.is_some_and(|value| *value),
        );
        Ok(())
    }

    #[qjs(rename = "rect")]
    fn rect(&self, x: Coerced<f64>, y: Coerced<f64>, width: Coerced<f64>, height: Coerced<f64>) {
        self.canvas
            .borrow_mut()
            .rect(f32_of(*x), f32_of(*y), f32_of(*width), f32_of(*height));
    }

    #[qjs(rename = "roundRect")]
    fn round_rect(
        &self,
        ctx: Ctx<'_>,
        x: Coerced<f64>,
        y: Coerced<f64>,
        width: Coerced<f64>,
        height: Coerced<f64>,
        radii: Opt<Value<'_>>,
    ) -> Result<()> {
        let radii = match radii.0 {
            Some(value) if !value.is_undefined() => round_rect_radii(&value)?,
            _ => None,
        };
        let radii = if let Some(radii) = radii {
            if radii.is_empty() || radii.len() > 4 {
                return Err(Exception::throw_range(
                    &ctx,
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
                return Err(Exception::throw_range(&ctx, "radius is negative"));
            }
            radii
        } else {
            self.canvas
                .borrow_mut()
                .rect(f32_of(*x), f32_of(*y), f32_of(*width), f32_of(*height));
            return Ok(());
        };
        self.canvas.borrow_mut().round_rect(
            f32_of(*x),
            f32_of(*y),
            f32_of(*width),
            f32_of(*height),
            &radii,
        );
        Ok(())
    }
}

// https://html.spec.whatwg.org/multipage/canvas.html#dom-path2d-addpath
fn add_path_transform<'js>(
    ctx: &Ctx<'js>,
    value: Option<&Value<'js>>,
) -> Result<Option<tiny_skia::Transform>> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_undefined() || value.is_null() {
        return Ok(None);
    }
    let Some(object) = value.as_object() else {
        return Err(Exception::throw_type(
            ctx,
            "addPath transform must be an object",
        ));
    };
    let read = |name: &str| -> Result<Option<f32>> {
        let member: Value = object.get(name)?;
        if member.is_undefined() || member.is_null() {
            return Ok(None);
        }
        let number = Coerced::<f64>::from_js(ctx, member)?;
        Ok(Some(f32_of(*number)))
    };
    let m11 = read("a")?.unwrap_or(1.0);
    let m12 = read("b")?.unwrap_or(0.0);
    let m21 = read("c")?.unwrap_or(0.0);
    let m22 = read("d")?.unwrap_or(1.0);
    let dx = read("e")?.unwrap_or(0.0);
    let dy = read("f")?.unwrap_or(0.0);
    if [m11, m12, m21, m22, dx, dy]
        .iter()
        .any(|value| !value.is_finite())
    {
        return Ok(None);
    }
    Ok(Some(tiny_skia::Transform::from_row(
        m11, m12, m21, m22, dx, dy,
    )))
}

pub(crate) fn install(ctx: &Ctx<'_>) -> Result<()> {
    Class::<JsPath2D>::define(&ctx.globals())?;
    Ok(())
}
