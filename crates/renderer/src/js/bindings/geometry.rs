// https://drafts.fxtf.org/geometry/#DOMPoint
// https://drafts.fxtf.org/geometry/#dictdef-dompointinit

use rquickjs::class::Trace;
use rquickjs::prelude::Opt;
use rquickjs::{Coerced, Ctx, Result};

#[derive(Trace, rquickjs::JsLifetime)]
#[rquickjs::class(rename = "DOMPoint")]
pub(crate) struct JsDomPoint {
    x: f64,
    y: f64,
    z: f64,
    w: f64,
}

#[rquickjs::methods]
impl JsDomPoint {
    #[qjs(constructor)]
    fn new(
        x: Opt<Coerced<f64>>,
        y: Opt<Coerced<f64>>,
        z: Opt<Coerced<f64>>,
        w: Opt<Coerced<f64>>,
    ) -> Self {
        Self {
            x: x.0.map_or(0.0, |value| *value),
            y: y.0.map_or(0.0, |value| *value),
            z: z.0.map_or(0.0, |value| *value),
            w: w.0.map_or(1.0, |value| *value),
        }
    }

    #[qjs(get)]
    fn x(&self) -> f64 {
        self.x
    }

    #[qjs(set, rename = "x")]
    fn set_x(&mut self, value: Coerced<f64>) {
        self.x = *value;
    }

    #[qjs(get)]
    fn y(&self) -> f64 {
        self.y
    }

    #[qjs(set, rename = "y")]
    fn set_y(&mut self, value: Coerced<f64>) {
        self.y = *value;
    }

    #[qjs(get)]
    fn z(&self) -> f64 {
        self.z
    }

    #[qjs(set, rename = "z")]
    fn set_z(&mut self, value: Coerced<f64>) {
        self.z = *value;
    }

    #[qjs(get)]
    fn w(&self) -> f64 {
        self.w
    }

    #[qjs(set, rename = "w")]
    fn set_w(&mut self, value: Coerced<f64>) {
        self.w = *value;
    }
}

#[derive(Trace, rquickjs::JsLifetime)]
#[rquickjs::class(rename = "DOMPointReadOnly")]
pub(crate) struct JsDomPointReadOnly {
    x: f64,
    y: f64,
    z: f64,
    w: f64,
}

#[rquickjs::methods]
impl JsDomPointReadOnly {
    #[qjs(constructor)]
    fn new(
        x: Opt<Coerced<f64>>,
        y: Opt<Coerced<f64>>,
        z: Opt<Coerced<f64>>,
        w: Opt<Coerced<f64>>,
    ) -> Self {
        Self {
            x: x.0.map_or(0.0, |value| *value),
            y: y.0.map_or(0.0, |value| *value),
            z: z.0.map_or(0.0, |value| *value),
            w: w.0.map_or(1.0, |value| *value),
        }
    }

    #[qjs(get)]
    fn x(&self) -> f64 {
        self.x
    }

    #[qjs(get)]
    fn y(&self) -> f64 {
        self.y
    }

    #[qjs(get)]
    fn z(&self) -> f64 {
        self.z
    }

    #[qjs(get)]
    fn w(&self) -> f64 {
        self.w
    }
}

// https://drafts.fxtf.org/geometry/#domrect
#[derive(Trace, rquickjs::JsLifetime)]
#[rquickjs::class(rename = "DOMRect")]
pub(crate) struct JsDomRect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

#[rquickjs::methods]
impl JsDomRect {
    #[qjs(constructor)]
    fn new(
        x: Opt<Coerced<f64>>,
        y: Opt<Coerced<f64>>,
        width: Opt<Coerced<f64>>,
        height: Opt<Coerced<f64>>,
    ) -> Self {
        Self {
            x: x.0.map_or(0.0, |value| *value),
            y: y.0.map_or(0.0, |value| *value),
            width: width.0.map_or(0.0, |value| *value),
            height: height.0.map_or(0.0, |value| *value),
        }
    }

    #[qjs(get)]
    fn x(&self) -> f64 {
        self.x
    }

    #[qjs(get)]
    fn y(&self) -> f64 {
        self.y
    }

    #[qjs(get)]
    fn width(&self) -> f64 {
        self.width
    }

    #[qjs(get)]
    fn height(&self) -> f64 {
        self.height
    }

    #[qjs(get, rename = "top")]
    fn top(&self) -> f64 {
        self.y.min(self.y + self.height)
    }

    #[qjs(get, rename = "right")]
    fn right(&self) -> f64 {
        self.x.max(self.x + self.width)
    }

    #[qjs(get, rename = "bottom")]
    fn bottom(&self) -> f64 {
        self.y.max(self.y + self.height)
    }

    #[qjs(get, rename = "left")]
    fn left(&self) -> f64 {
        self.x.min(self.x + self.width)
    }

    #[qjs(rename = "toJSON")]
    fn to_json<'js>(&self, ctx: Ctx<'js>) -> Result<rquickjs::Object<'js>> {
        let object = rquickjs::Object::new(ctx)?;
        object.set("x", self.x)?;
        object.set("y", self.y)?;
        object.set("width", self.width)?;
        object.set("height", self.height)?;
        object.set("top", self.top())?;
        object.set("right", self.right())?;
        object.set("bottom", self.bottom())?;
        object.set("left", self.left())?;
        Ok(object)
    }
}

pub(crate) fn dom_rect<'js>(
    ctx: &Ctx<'js>,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) -> Result<rquickjs::Value<'js>> {
    Ok(rquickjs::Class::<JsDomRect>::instance(
        ctx.clone(),
        JsDomRect {
            x,
            y,
            width,
            height,
        },
    )?
    .into_value())
}

pub(crate) fn install(ctx: &Ctx<'_>) -> Result<()> {
    let globals = ctx.globals();
    rquickjs::Class::<JsDomPoint>::define(&globals)?;
    rquickjs::Class::<JsDomPointReadOnly>::define(&globals)?;
    rquickjs::Class::<JsDomRect>::define(&globals)?;
    Ok(())
}
