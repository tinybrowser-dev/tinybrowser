// https://html.spec.whatwg.org/multipage/canvas.html#concept-canvas-bitmap

use std::collections::HashMap;
use std::f32::consts::FRAC_PI_4;
use std::sync::Arc;

use num_traits::ToPrimitive;

const MAX_SHADOW_SIDE: u32 = 4096;

const KAPPA: f32 = 0.552_284_8;

use tiny_skia::{
    BlendMode, Color, ColorU8, FillRule, FilterQuality, GradientStop, LineCap, LineJoin, Mask,
    Paint, Path, PathBuilder, Pixmap, PixmapPaint, Point, Shader, SpreadMode, Stroke, StrokeDash,
    Transform,
};

pub(crate) struct Canvas2D {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) pixmap: Option<Pixmap>,
    state: State,
    stack: Vec<State>,
    path: Vec<Segment>,
    current: Option<Point>,
    subpath_start: Point,
    gradients: HashMap<u32, Gradient>,
    patterns: HashMap<u32, Pattern>,
    next_resource: u32,
    // https://html.spec.whatwg.org/multipage/canvas.html#the-canvas-element
    origin_clean: bool,
    // https://html.spec.whatwg.org/multipage/canvas.html#dom-context-2d-beginlayer
    layers: Vec<Layer>,
}

// https://html.spec.whatwg.org/multipage/canvas.html#dom-context-2d-letterspacing
fn normalize_length(value: &str) -> Option<String> {
    let text = value.trim().to_ascii_lowercase();
    let Some(split) = text.find(|ch: char| !(ch.is_ascii_digit() || matches!(ch, '.' | '-' | '+')))
    else {
        return text
            .parse::<f32>()
            .ok()
            .filter(|number| *number == 0.0)
            .map(|_| "0px".to_owned());
    };
    let (number, unit) = text.split_at(split);
    if !matches!(
        unit,
        "px" | "em" | "ex" | "ch" | "ic" | "rem" | "in" | "cm" | "mm" | "pt" | "pc" | "q"
    ) {
        return None;
    }
    if !number.parse::<f32>().is_ok_and(f32::is_finite) {
        return None;
    }
    Some(format!("{number}{unit}"))
}

impl<'js> rquickjs::class::Trace<'js> for Canvas2D {
    fn trace<'a>(&self, _tracer: rquickjs::class::Tracer<'a, 'js>) {}
}

#[derive(Clone)]
struct State {
    transform: Transform,
    fill: Style,
    stroke: Style,
    shadow: CanvasColor,
    alpha: f32,
    blend: BlendMode,
    line_width: f32,
    line_cap: LineCap,
    line_join: LineJoin,
    miter_limit: f32,
    dash: Vec<f32>,
    dash_offset: f32,
    shadow_blur: f32,
    shadow_x: f32,
    shadow_y: f32,
    font: String,
    text_align: TextAlign,
    text_baseline: TextBaseline,
    direction: Direction,
    letter_spacing: String,
    word_spacing: String,
    font_kerning: String,
    font_stretch: String,
    font_variant_caps: String,
    text_rendering: String,
    lang: String,
    smoothing: bool,
    smoothing_quality: String,
    filter: String,
    filter_matrix: Option<ColorMatrix>,
    filter_object: Option<rquickjs::Persistent<rquickjs::Value<'static>>>,
    clip: Option<Arc<Mask>>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Rect {
    pub(crate) x: f32,
    pub(crate) y: f32,
    pub(crate) width: f32,
    pub(crate) height: f32,
}

impl Rect {
    pub(crate) const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    fn normalized(self) -> Self {
        let mut rect = self;
        if rect.width < 0.0 {
            rect.x += rect.width;
            rect.width = -rect.width;
        }
        if rect.height < 0.0 {
            rect.y += rect.height;
            rect.height = -rect.height;
        }
        rect
    }
}

// https://drafts.fxtf.org/filter-effects/#feColorMatrixElement
#[derive(Clone, Copy, Debug)]
pub(crate) struct ColorMatrix([f32; 20]);

impl ColorMatrix {
    pub(crate) const IDENTITY: Self = Self([
        1.0, 0.0, 0.0, 0.0, 0.0, //
        0.0, 1.0, 0.0, 0.0, 0.0, //
        0.0, 0.0, 1.0, 0.0, 0.0, //
        0.0, 0.0, 0.0, 1.0, 0.0,
    ]);

    pub(crate) fn hue_rotate(degrees: f32) -> Self {
        let (sin, cos) = degrees.to_radians().sin_cos();
        Self([
            0.213 + cos * 0.787 - sin * 0.213,
            0.715 - cos * 0.715 - sin * 0.715,
            0.072 - cos * 0.072 + sin * 0.928,
            0.0,
            0.0,
            0.213 - cos * 0.213 + sin * 0.143,
            0.715 + cos * 0.285 + sin * 0.140,
            0.072 - cos * 0.072 - sin * 0.283,
            0.0,
            0.0,
            0.213 - cos * 0.213 - sin * 0.787,
            0.715 - cos * 0.715 + sin * 0.715,
            0.072 + cos * 0.928 + sin * 0.072,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            1.0,
            0.0,
        ])
    }

    pub(crate) fn saturate(amount: f32) -> Self {
        Self([
            0.213 + 0.787 * amount,
            0.715 - 0.715 * amount,
            0.072 - 0.072 * amount,
            0.0,
            0.0,
            0.213 - 0.213 * amount,
            0.715 + 0.285 * amount,
            0.072 - 0.072 * amount,
            0.0,
            0.0,
            0.213 - 0.213 * amount,
            0.715 - 0.715 * amount,
            0.072 + 0.928 * amount,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            1.0,
            0.0,
        ])
    }

    pub(crate) fn luminance_to_alpha() -> Self {
        Self([
            0.0, 0.0, 0.0, 0.0, 0.0, //
            0.0, 0.0, 0.0, 0.0, 0.0, //
            0.0, 0.0, 0.0, 0.0, 0.0, //
            0.2125, 0.7154, 0.0721, 0.0, 0.0,
        ])
    }

    pub(crate) fn from_values(values: [f32; 20]) -> Self {
        Self(values)
    }

    pub(crate) fn then(self, next: Self) -> Self {
        let left = self.0;
        let right = next.0;
        let mut out = [0.0; 20];
        for row in 0..4 {
            for column in 0..5 {
                let mut sum = 0.0;
                for inner in 0..4 {
                    sum += right[row * 5 + inner] * left[inner * 5 + column];
                }
                if column == 4 {
                    sum += right[row * 5 + 4];
                }
                out[row * 5 + column] = sum;
            }
        }
        Self(out)
    }
}

// https://drafts.fxtf.org/filter-effects/#feColorMatrixElement
fn apply_color_matrix(pixmap: &mut Pixmap, matrix: &ColorMatrix) {
    let coefficients = &matrix.0;
    for pixel in pixmap.pixels_mut() {
        let color = pixel.demultiply();
        let alpha = f32::from(color.alpha()) / 255.0;
        let red = f32::from(color.red()) / 255.0;
        let green = f32::from(color.green()) / 255.0;
        let blue = f32::from(color.blue()) / 255.0;
        let channel = |row: usize| {
            let row = row * 5;
            (coefficients[row] * red
                + coefficients[row + 1] * green
                + coefficients[row + 2] * blue
                + coefficients[row + 3] * alpha
                + coefficients[row + 4])
                .clamp(0.0, 1.0)
        };
        let out_alpha = channel(3);
        *pixel = ColorU8::from_rgba(
            to_u8(channel(0)),
            to_u8(channel(1)),
            to_u8(channel(2)),
            to_u8(out_alpha),
        )
        .premultiply();
    }
}

#[derive(Clone, PartialEq)]
struct CanvasColor {
    rgba: [u8; 4],
    serialized: Option<String>,
}

impl CanvasColor {
    fn new(rgba: [u8; 4]) -> Self {
        Self {
            rgba,
            serialized: None,
        }
    }

    fn exact(rgba: [u8; 4], serialized: String) -> Self {
        Self {
            rgba,
            serialized: Some(serialized),
        }
    }
}

#[derive(Clone, PartialEq)]
enum Style {
    Color(CanvasColor),
    Gradient(u32),
    Pattern(u32),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum TextAlign {
    Start,
    End,
    Left,
    Right,
    Center,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum TextBaseline {
    Top,
    Hanging,
    Middle,
    Alphabetic,
    Ideographic,
    Bottom,
}

impl TextBaseline {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "top" => Self::Top,
            "hanging" => Self::Hanging,
            "middle" => Self::Middle,
            "alphabetic" => Self::Alphabetic,
            "ideographic" => Self::Ideographic,
            "bottom" => Self::Bottom,
            _ => return None,
        })
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Top => "top",
            Self::Hanging => "hanging",
            Self::Middle => "middle",
            Self::Alphabetic => "alphabetic",
            Self::Ideographic => "ideographic",
            Self::Bottom => "bottom",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Direction {
    Inherit,
    Ltr,
    Rtl,
}

#[derive(Clone, Copy)]
enum Segment {
    MoveTo(Point),
    LineTo(Point),
    Quad(Point, Point),
    Cubic(Point, Point, Point),
    Close,
}

#[derive(Clone)]
pub(crate) struct Gradient {
    kind: GradientKind,
    stops: Vec<(f32, [u8; 4])>,
    // https://html.spec.whatwg.org/multipage/canvas.html#dom-context-2d-createlineargradient
    degenerate: bool,
}

#[derive(Clone)]
enum GradientKind {
    Linear {
        start: Point,
        end: Point,
    },
    Radial {
        start: Point,
        start_radius: f32,
        end: Point,
        end_radius: f32,
    },
    Conic {
        center: Point,
        angle: f32,
    },
}

struct Layer {
    base: usize,
    parent: Option<Pixmap>,
    filter_matrix: Option<ColorMatrix>,
}

#[derive(Clone)]
pub(crate) struct Pattern {
    image: Arc<Pixmap>,
    spread: SpreadMode,
    repetition: PatternRepeat,
    quality: FilterQuality,
    origin_clean: bool,
    transform: Transform,
}

// https://html.spec.whatwg.org/multipage/canvas.html#dom-context-2d-createpattern
#[derive(Clone, Copy, PartialEq, Eq)]
enum PatternRepeat {
    Repeat,
    RepeatX,
    RepeatY,
    NoRepeat,
}

pub(crate) struct SourceImage {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) pixels: Arc<[u8]>,
    pub(crate) origin_clean: bool,
}

fn sample_stops(stops: &[(f32, [u8; 4])], offset: f32) -> [u8; 4] {
    let offset = offset.rem_euclid(1.0);
    let Some((first_offset, first)) = stops.first() else {
        return [0, 0, 0, 0];
    };
    if offset <= *first_offset {
        return *first;
    }
    let Some((last_offset, last)) = stops.last() else {
        return *first;
    };
    if offset >= *last_offset {
        return *last;
    }
    let index = stops.partition_point(|(stop, _)| *stop <= offset);
    let (start_offset, start) = stops[index - 1];
    let (end_offset, end) = stops[index];
    let span = end_offset - start_offset;
    let t = if span > 0.0 {
        (offset - start_offset) / span
    } else {
        0.0
    };
    let channel = |start: u8, end: u8| {
        let start = f32::from(start) / 255.0;
        let end = f32::from(end) / 255.0;
        to_u8(start + (end - start) * t)
    };
    [
        channel(start[0], end[0]),
        channel(start[1], end[1]),
        channel(start[2], end[2]),
        channel(start[3], end[3]),
    ]
}

fn conic_stops(stops: &[(f32, [u8; 4])], angle: f32, alpha: f32) -> Vec<GradientStop> {
    let shift = angle.rem_euclid(360.0) / 360.0;
    let boundary = sample_stops(stops, 1.0 - shift);
    let mut rotated: Vec<(f32, [u8; 4])> = Vec::with_capacity(stops.len() + 2);
    rotated.push((0.0, boundary));
    rotated.extend(
        stops
            .iter()
            .map(|(offset, color)| ((offset + shift).rem_euclid(1.0), *color)),
    );
    rotated.push((1.0, boundary));
    rotated.sort_by(|left, right| left.0.total_cmp(&right.0));
    rotated
        .into_iter()
        .map(|(offset, [r, g, b, a])| {
            GradientStop::new(
                offset,
                Color::from_rgba8(r, g, b, to_u8(f32::from(a) / 255.0 * alpha)),
            )
        })
        .collect()
}

impl Canvas2D {
    // https://html.spec.whatwg.org/multipage/canvas.html#dom-canvas-width
    pub(crate) fn reset_for_resize(&mut self, width: u32, height: u32) {
        let gradients = std::mem::take(&mut self.gradients);
        let patterns = std::mem::take(&mut self.patterns);
        let next_resource = self.next_resource;
        let origin_clean = self.origin_clean;
        *self = Canvas2D::new(width, height);
        self.gradients = gradients;
        self.patterns = patterns;
        self.next_resource = next_resource;
        self.origin_clean = origin_clean;
    }

    pub(crate) fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            pixmap: make_pixmap(width, height),
            state: State::default(),
            stack: Vec::new(),
            path: Vec::new(),
            current: None,
            subpath_start: Point::zero(),
            gradients: HashMap::new(),
            patterns: HashMap::new(),
            next_resource: 1,
            origin_clean: true,
            layers: Vec::new(),
        }
    }

    pub(crate) fn origin_clean(&self) -> bool {
        self.origin_clean
    }

    pub(crate) fn has_open_layer(&self) -> bool {
        !self.layers.is_empty()
    }

    fn taint(&mut self) {
        self.origin_clean = false;
    }

    // https://html.spec.whatwg.org/multipage/canvas.html#dom-context-2d-reset
    pub(crate) fn reset(&mut self) {
        while let Some(layer) = self.layers.pop() {
            self.pixmap = layer.parent;
        }
        if let Some(pixmap) = &mut self.pixmap {
            pixmap.fill(Color::TRANSPARENT);
        }
        self.reset_state();
    }

    // https://html.spec.whatwg.org/multipage/canvas.html#dom-offscreencanvas-transfertoimagebitmap
    pub(crate) fn clear_bitmap(&mut self) {
        if let Some(pixmap) = &mut self.pixmap {
            pixmap.fill(Color::TRANSPARENT);
        }
    }

    fn reset_state(&mut self) {
        self.state = State::default();
        self.stack.clear();
        self.path.clear();
        self.current = None;
        self.subpath_start = Point::zero();
        // https://html.spec.whatwg.org/multipage/canvas.html#dom-context-2d-reset
        self.layers.clear();
    }

    pub(crate) fn save(&mut self) {
        self.stack.push(self.state.clone());
    }

    // https://html.spec.whatwg.org/multipage/canvas.html#dom-context-2d-restore
    pub(crate) fn restore(&mut self) -> bool {
        if self
            .layers
            .last()
            .is_some_and(|layer| self.stack.len() <= layer.base + 1)
        {
            return false;
        }
        if let Some(state) = self.stack.pop() {
            self.state = state;
        }
        true
    }

    pub(crate) fn begin_layer(&mut self) {
        self.begin_layer_filtered(None);
    }

    // https://html.spec.whatwg.org/multipage/canvas.html#dom-context-2d-beginlayer
    pub(crate) fn begin_layer_filtered(&mut self, filter: Option<ColorMatrix>) {
        let base = self.stack.len();
        let captured = self.state.clone();
        let parent = self.pixmap.take();
        self.pixmap = make_pixmap(self.width, self.height);
        self.layers.push(Layer {
            base,
            parent,
            filter_matrix: filter,
        });
        self.stack.push(captured);
        self.state.alpha = 1.0;
        self.state.blend = BlendMode::SourceOver;
        self.state.shadow = CanvasColor::new([0, 0, 0, 0]);
        self.state.shadow_blur = 0.0;
        self.state.shadow_x = 0.0;
        self.state.shadow_y = 0.0;
        "none".clone_into(&mut self.state.filter);
        self.state.filter_matrix = None;
        self.state.filter_object = None;
    }

    pub(crate) fn end_layer(&mut self) -> bool {
        let Some(base) = self.layers.last().map(|layer| layer.base) else {
            return false;
        };
        if self.stack.len() != base + 1 {
            return false;
        }
        let Some(layer) = self.layers.pop() else {
            return false;
        };
        let Layer {
            parent,
            filter_matrix,
            ..
        } = layer;
        let Some(state) = self.stack.pop() else {
            return false;
        };
        let mut layer_pixmap = self.pixmap.take();
        if let (Some(pixmap), Some(matrix)) = (layer_pixmap.as_mut(), filter_matrix) {
            apply_color_matrix(pixmap, &matrix);
        }
        // https://html.spec.whatwg.org/multipage/canvas.html#drawing-model
        self.state = state;
        self.pixmap = parent;
        let Some(layer_pixmap) = layer_pixmap else {
            return true;
        };
        self.paint_layer_shadow(&layer_pixmap);
        if let Some(parent_pixmap) = self.pixmap.as_mut() {
            parent_pixmap.draw_pixmap(
                0,
                0,
                layer_pixmap.as_ref(),
                &PixmapPaint {
                    opacity: self.state.alpha.clamp(0.0, 1.0),
                    blend_mode: self.state.blend,
                    quality: FilterQuality::Nearest,
                },
                Transform::identity(),
                self.state.clip.as_deref(),
            );
        }
        true
    }

    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        clippy::cast_sign_loss,
        reason = "mask sides and offsets stay within the bounded canvas pixmap"
    )]
    fn paint_layer_shadow(&mut self, layer: &Pixmap) {
        let [_, _, _, alpha] = self.state.shadow.rgba;
        if alpha == 0
            || self.pixmap.is_none()
            || (self.state.shadow_blur == 0.0
                && self.state.shadow_x == 0.0
                && self.state.shadow_y == 0.0)
        {
            return;
        }
        let width = layer.width();
        let mut bounds: Option<(u32, u32, u32, u32)> = None;
        for (index, pixel) in layer.pixels().iter().enumerate() {
            if pixel.alpha() == 0 {
                continue;
            }
            let x = index as u32 % width;
            let y = index as u32 / width;
            bounds = Some(match bounds {
                Some((left, top, right, bottom)) => {
                    (left.min(x), top.min(y), right.max(x), bottom.max(y))
                }
                None => (x, y, x, y),
            });
        }
        let Some((min_x, min_y, max_x, max_y)) = bounds else {
            return;
        };
        let sigma = self.state.shadow_blur / 2.0;
        let padding = (sigma * 3.0).ceil() as u32 + 1;
        let margin = padding
            + self.state.shadow_x.abs().ceil() as u32
            + self.state.shadow_y.abs().ceil() as u32
            + 1;
        let width = max_x - min_x + 1 + margin * 2;
        let height = max_y - min_y + 1 + margin * 2;
        let Some(mut mask) = Pixmap::new(width, height) else {
            return;
        };
        let Ok(margin) = i32::try_from(margin) else {
            return;
        };
        mask.draw_pixmap(
            margin,
            margin,
            layer.as_ref(),
            &PixmapPaint::default(),
            Transform::identity(),
            None,
        );
        self.finish_shadow(
            mask,
            sigma,
            min_x as f32 - margin as f32,
            min_y as f32 - margin as f32,
            self.state.alpha,
        );
    }

    pub(crate) fn filter(&self) -> &str {
        &self.state.filter
    }

    pub(crate) fn set_filter(&mut self, value: &str) {
        value.clone_into(&mut self.state.filter);
    }

    pub(crate) fn set_filter_matrix(&mut self, matrix: Option<ColorMatrix>) {
        self.state.filter_matrix = matrix;
    }

    pub(crate) fn filter_object(&self) -> Option<rquickjs::Persistent<rquickjs::Value<'static>>> {
        self.state.filter_object.clone()
    }

    pub(crate) fn set_filter_object(
        &mut self,
        value: Option<rquickjs::Persistent<rquickjs::Value<'static>>>,
    ) {
        self.state.filter_object = value;
    }

    pub(crate) fn transform(&mut self, m11: f32, m12: f32, m21: f32, m22: f32, dx: f32, dy: f32) {
        if !(m11.is_finite()
            && m12.is_finite()
            && m21.is_finite()
            && m22.is_finite()
            && dx.is_finite()
            && dy.is_finite())
        {
            return;
        }
        let matrix = Transform::from_row(m11, m12, m21, m22, dx, dy);
        self.state.transform = self.state.transform.pre_concat(matrix);
    }

    pub(crate) fn set_transform(
        &mut self,
        m11: f32,
        m12: f32,
        m21: f32,
        m22: f32,
        dx: f32,
        dy: f32,
    ) {
        if !(m11.is_finite()
            && m12.is_finite()
            && m21.is_finite()
            && m22.is_finite()
            && dx.is_finite()
            && dy.is_finite())
        {
            return;
        }
        self.state.transform = Transform::from_row(m11, m12, m21, m22, dx, dy);
    }

    pub(crate) fn reset_transform(&mut self) {
        self.state.transform = Transform::identity();
    }

    pub(crate) fn get_transform(&self) -> Transform {
        self.state.transform
    }

    pub(crate) fn device_scale(&self) -> (f32, f32) {
        let transform = self.state.transform;
        (
            transform.sx.hypot(transform.ky),
            transform.kx.hypot(transform.sy),
        )
    }

    pub(crate) fn translate(&mut self, x: f32, y: f32) {
        if x.is_finite() && y.is_finite() {
            self.state.transform = self
                .state
                .transform
                .pre_concat(Transform::from_translate(x, y));
        }
    }

    pub(crate) fn scale(&mut self, x: f32, y: f32) {
        if x.is_finite() && y.is_finite() {
            self.state.transform = self.state.transform.pre_concat(Transform::from_scale(x, y));
        }
    }

    pub(crate) fn rotate(&mut self, radians: f32) {
        if radians.is_finite() {
            self.state.transform = self
                .state
                .transform
                .pre_concat(Transform::from_rotate(radians.to_degrees()));
        }
    }

    pub(crate) fn fill_style(&self) -> StyleValue {
        StyleValue::from_style(&self.state.fill)
    }

    pub(crate) fn set_fill_style(&mut self, value: StyleValue) {
        let style = match value {
            StyleValue::Color(color) => Style::Color(CanvasColor::new(color)),
            StyleValue::ColorExact(color, text) => Style::Color(CanvasColor::exact(color, text)),
            StyleValue::Gradient(id) => Style::Gradient(id),
            StyleValue::Pattern(id) => {
                if self
                    .patterns
                    .get(&id)
                    .is_some_and(|pattern| !pattern.origin_clean)
                {
                    self.taint();
                }
                Style::Pattern(id)
            }
        };
        self.state.fill = style;
    }

    pub(crate) fn stroke_style(&self) -> StyleValue {
        StyleValue::from_style(&self.state.stroke)
    }

    pub(crate) fn set_stroke_style(&mut self, value: StyleValue) {
        let style = match value {
            StyleValue::Color(color) => Style::Color(CanvasColor::new(color)),
            StyleValue::ColorExact(color, text) => Style::Color(CanvasColor::exact(color, text)),
            StyleValue::Gradient(id) => Style::Gradient(id),
            StyleValue::Pattern(id) => {
                if self
                    .patterns
                    .get(&id)
                    .is_some_and(|pattern| !pattern.origin_clean)
                {
                    self.taint();
                }
                Style::Pattern(id)
            }
        };
        self.state.stroke = style;
    }

    pub(crate) fn line_width(&self) -> f32 {
        self.state.line_width
    }

    pub(crate) fn set_line_width(&mut self, value: f32) {
        if value > 0.0 && value.is_finite() {
            self.state.line_width = value;
        }
    }

    pub(crate) fn line_cap(&self) -> &'static str {
        match self.state.line_cap {
            LineCap::Butt => "butt",
            LineCap::Round => "round",
            LineCap::Square => "square",
        }
    }

    pub(crate) fn set_line_cap(&mut self, value: &str) {
        self.state.line_cap = match value {
            "butt" => LineCap::Butt,
            "round" => LineCap::Round,
            "square" => LineCap::Square,
            _ => return,
        };
    }

    pub(crate) fn line_join(&self) -> &'static str {
        match self.state.line_join {
            LineJoin::Miter | LineJoin::MiterClip => "miter",
            LineJoin::Round => "round",
            LineJoin::Bevel => "bevel",
        }
    }

    pub(crate) fn set_line_join(&mut self, value: &str) {
        self.state.line_join = match value {
            "miter" => LineJoin::Miter,
            "round" => LineJoin::Round,
            "bevel" => LineJoin::Bevel,
            _ => return,
        };
    }

    pub(crate) fn miter_limit(&self) -> f32 {
        self.state.miter_limit
    }

    pub(crate) fn set_miter_limit(&mut self, value: f32) {
        if value > 0.0 && value.is_finite() {
            self.state.miter_limit = value;
        }
    }

    pub(crate) fn line_dash(&self) -> Vec<f32> {
        self.state.dash.clone()
    }

    pub(crate) fn set_line_dash(&mut self, mut dash: Vec<f32>) {
        if dash.iter().any(|value| !value.is_finite() || *value < 0.0) {
            return;
        }
        if dash.iter().all(|value| *value == 0.0) {
            dash.clear();
        }
        if dash.len() % 2 == 1 {
            let copy = dash.clone();
            dash.extend(copy);
        }
        self.state.dash = dash;
    }

    pub(crate) fn line_dash_offset(&self) -> f32 {
        self.state.dash_offset
    }

    pub(crate) fn set_line_dash_offset(&mut self, value: f32) {
        if value.is_finite() {
            self.state.dash_offset = value;
        }
    }

    pub(crate) fn global_alpha(&self) -> f32 {
        self.state.alpha
    }

    pub(crate) fn set_global_alpha(&mut self, value: f32) {
        if value.is_finite() && (0.0..=1.0).contains(&value) {
            self.state.alpha = value;
        }
    }

    pub(crate) fn global_composite_operation(&self) -> &'static str {
        blend_name(self.state.blend)
    }

    pub(crate) fn set_global_composite_operation(&mut self, value: &str) {
        if let Some(blend) = blend_from_name(value) {
            self.state.blend = blend;
        }
    }

    pub(crate) fn shadow_color(&self) -> [u8; 4] {
        self.state.shadow.rgba
    }

    pub(crate) fn shadow_serialized(&self) -> Option<&str> {
        self.state.shadow.serialized.as_deref()
    }

    pub(crate) fn set_shadow_color_exact(&mut self, color: [u8; 4], text: Option<String>) {
        self.state.shadow = text.map_or_else(
            || CanvasColor::new(color),
            |text| CanvasColor::exact(color, text),
        );
    }

    pub(crate) fn shadow_blur(&self) -> f32 {
        self.state.shadow_blur
    }

    pub(crate) fn set_shadow_blur(&mut self, value: f32) {
        if value >= 0.0 && value.is_finite() {
            self.state.shadow_blur = value;
        }
    }

    // https://html.spec.whatwg.org/multipage/canvas.html#dom-context-2d-filltext
    pub(crate) fn fill_text_path(&mut self, path: &Path, x: f32, y: f32, scale_x: f32) {
        let transform = self
            .state
            .transform
            .pre_concat(Transform::from_translate(x, y))
            .pre_concat(Transform::from_scale(scale_x, 1.0));
        let Some(path) = path.clone().transform(transform) else {
            return;
        };
        self.fill_path(&path, false);
    }

    pub(crate) fn stroke_text_path(&mut self, path: &Path, x: f32, y: f32, scale_x: f32) {
        let transform = self
            .state
            .transform
            .pre_concat(Transform::from_translate(x, y))
            .pre_concat(Transform::from_scale(scale_x, 1.0));
        let Some(path) = path.clone().transform(transform) else {
            return;
        };
        self.stroke_path(&path);
    }

    pub(crate) fn shadow_offset_x(&self) -> f32 {
        self.state.shadow_x
    }

    pub(crate) fn set_shadow_offset_x(&mut self, value: f32) {
        if value.is_finite() {
            self.state.shadow_x = value;
        }
    }

    pub(crate) fn shadow_offset_y(&self) -> f32 {
        self.state.shadow_y
    }

    pub(crate) fn set_shadow_offset_y(&mut self, value: f32) {
        if value.is_finite() {
            self.state.shadow_y = value;
        }
    }

    pub(crate) fn font(&self) -> &str {
        &self.state.font
    }

    pub(crate) fn set_font(&mut self, value: &str) {
        if !value.is_empty() {
            value.clone_into(&mut self.state.font);
        }
    }

    pub(crate) fn text_align(&self) -> &'static str {
        match self.state.text_align {
            TextAlign::Start => "start",
            TextAlign::End => "end",
            TextAlign::Left => "left",
            TextAlign::Right => "right",
            TextAlign::Center => "center",
        }
    }

    pub(crate) fn set_text_align(&mut self, value: &str) {
        self.state.text_align = match value {
            "start" => TextAlign::Start,
            "end" => TextAlign::End,
            "left" => TextAlign::Left,
            "right" => TextAlign::Right,
            "center" => TextAlign::Center,
            _ => return,
        };
    }

    pub(crate) fn text_baseline(&self) -> &'static str {
        self.state.text_baseline.as_str()
    }

    pub(crate) fn set_text_baseline(&mut self, value: &str) {
        if let Some(baseline) = TextBaseline::parse(value) {
            self.state.text_baseline = baseline;
        }
    }

    pub(crate) fn direction(&self) -> &'static str {
        match self.state.direction {
            Direction::Inherit => "inherit",
            Direction::Ltr => "ltr",
            Direction::Rtl => "rtl",
        }
    }

    pub(crate) fn set_direction(&mut self, value: &str) {
        self.state.direction = match value {
            "inherit" => Direction::Inherit,
            "ltr" => Direction::Ltr,
            "rtl" => Direction::Rtl,
            _ => return,
        };
    }

    pub(crate) fn letter_spacing(&self) -> &str {
        &self.state.letter_spacing
    }

    pub(crate) fn set_letter_spacing(&mut self, value: &str) {
        if let Some(value) = normalize_length(value) {
            self.state.letter_spacing = value;
        }
    }

    pub(crate) fn word_spacing(&self) -> &str {
        &self.state.word_spacing
    }

    pub(crate) fn set_word_spacing(&mut self, value: &str) {
        if let Some(value) = normalize_length(value) {
            self.state.word_spacing = value;
        }
    }

    pub(crate) fn font_kerning(&self) -> &str {
        &self.state.font_kerning
    }

    pub(crate) fn set_font_kerning(&mut self, value: &str) {
        if matches!(value, "auto" | "normal" | "none") {
            value.clone_into(&mut self.state.font_kerning);
        }
    }

    pub(crate) fn font_stretch(&self) -> &str {
        &self.state.font_stretch
    }

    pub(crate) fn set_font_stretch(&mut self, value: &str) {
        if matches!(
            value,
            "ultra-condensed"
                | "extra-condensed"
                | "condensed"
                | "semi-condensed"
                | "normal"
                | "semi-expanded"
                | "expanded"
                | "extra-expanded"
                | "ultra-expanded"
        ) {
            value.clone_into(&mut self.state.font_stretch);
        }
    }

    pub(crate) fn lang(&self) -> &str {
        &self.state.lang
    }

    pub(crate) fn set_lang(&mut self, value: &str) {
        value.clone_into(&mut self.state.lang);
    }

    pub(crate) fn text_rendering(&self) -> &str {
        &self.state.text_rendering
    }

    pub(crate) fn set_text_rendering(&mut self, value: &str) {
        if matches!(
            value,
            "auto" | "optimizeSpeed" | "optimizeLegibility" | "geometricPrecision"
        ) {
            value.clone_into(&mut self.state.text_rendering);
        }
    }

    pub(crate) fn font_variant_caps(&self) -> &str {
        &self.state.font_variant_caps
    }

    pub(crate) fn set_font_variant_caps(&mut self, value: &str) {
        if matches!(
            value,
            "normal"
                | "small-caps"
                | "all-small-caps"
                | "petite-caps"
                | "all-petite-caps"
                | "unicase"
                | "titling-caps"
        ) {
            value.clone_into(&mut self.state.font_variant_caps);
        }
    }

    pub(crate) fn image_smoothing_enabled(&self) -> bool {
        self.state.smoothing
    }

    pub(crate) fn set_image_smoothing_enabled(&mut self, value: bool) {
        self.state.smoothing = value;
    }

    pub(crate) fn image_smoothing_quality(&self) -> &str {
        &self.state.smoothing_quality
    }

    pub(crate) fn set_image_smoothing_quality(&mut self, value: &str) {
        if value == "low" || value == "medium" || value == "high" {
            value.clone_into(&mut self.state.smoothing_quality);
        }
    }

    pub(crate) fn begin_path(&mut self) {
        self.path.clear();
        self.current = None;
        self.subpath_start = Point::zero();
    }

    pub(crate) fn move_to(&mut self, x: f32, y: f32) {
        if !(x.is_finite() && y.is_finite()) {
            return;
        }
        let mut point = Point::from_xy(x, y);
        self.state.transform.map_point(&mut point);
        self.path.push(Segment::MoveTo(point));
        self.current = Some(point);
        self.subpath_start = point;
    }

    pub(crate) fn line_to(&mut self, x: f32, y: f32) {
        if !(x.is_finite() && y.is_finite()) {
            return;
        }
        if self.current.is_none() {
            self.move_to(x, y);
            return;
        }
        let mut point = Point::from_xy(x, y);
        self.state.transform.map_point(&mut point);
        self.path.push(Segment::LineTo(point));
        self.current = Some(point);
    }

    pub(crate) fn quadratic_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        if !(cx.is_finite() && cy.is_finite() && x.is_finite() && y.is_finite()) {
            return;
        }
        if self.current.is_none() {
            self.move_to(cx, cy);
            self.line_to(x, y);
            return;
        }
        let mut control = Point::from_xy(cx, cy);
        let mut end = Point::from_xy(x, y);
        self.state.transform.map_point(&mut control);
        self.state.transform.map_point(&mut end);
        self.path.push(Segment::Quad(control, end));
        self.current = Some(end);
    }

    pub(crate) fn cubic_to(&mut self, c1x: f32, c1y: f32, c2x: f32, c2y: f32, x: f32, y: f32) {
        if !(c1x.is_finite()
            && c1y.is_finite()
            && c2x.is_finite()
            && c2y.is_finite()
            && x.is_finite()
            && y.is_finite())
        {
            return;
        }
        if self.current.is_none() {
            self.move_to(c1x, c1y);
        }
        let mut first = Point::from_xy(c1x, c1y);
        let mut second = Point::from_xy(c2x, c2y);
        let mut end = Point::from_xy(x, y);
        self.state.transform.map_point(&mut first);
        self.state.transform.map_point(&mut second);
        self.state.transform.map_point(&mut end);
        self.path.push(Segment::Cubic(first, second, end));
        self.current = Some(end);
    }

    pub(crate) fn close_path(&mut self) {
        if self.current.is_none() {
            return;
        }
        self.path.push(Segment::Close);
        self.current = Some(self.subpath_start);
    }

    pub(crate) fn rect(&mut self, x: f32, y: f32, width: f32, height: f32) {
        if !(x.is_finite() && y.is_finite() && width.is_finite() && height.is_finite()) {
            return;
        }
        if width == 0.0 && height == 0.0 {
            self.move_to(x, y);
            return;
        }
        let corners = [
            (x, y),
            (x + width, y),
            (x + width, y + height),
            (x, y + height),
        ];
        let mut first = true;
        for (cx, cy) in corners {
            let mut point = Point::from_xy(cx, cy);
            self.state.transform.map_point(&mut point);
            if first {
                self.path.push(Segment::MoveTo(point));
                self.subpath_start = point;
                first = false;
            } else {
                self.path.push(Segment::LineTo(point));
            }
            self.current = Some(point);
        }
        self.path.push(Segment::Close);
        self.current = Some(self.subpath_start);
    }

    // https://html.spec.whatwg.org/multipage/canvas.html#dom-context-2d-arc
    pub(crate) fn arc(
        &mut self,
        cx: f32,
        cy: f32,
        radius: f32,
        start: f32,
        end: f32,
        counterclockwise: bool,
    ) {
        if !(cx.is_finite()
            && cy.is_finite()
            && radius.is_finite()
            && start.is_finite()
            && end.is_finite())
        {
            return;
        }
        if radius < 0.0 {
            return;
        }
        if radius == 0.0 || same_float(start, end) {
            self.line_to(cx + radius * start.cos(), cy + radius * start.sin());
            return;
        }
        self.ellipse_arc(cx, cy, radius, radius, 0.0, start, end, counterclockwise);
    }

    #[expect(clippy::too_many_arguments, reason = "matches the IDL member")]
    pub(crate) fn ellipse(
        &mut self,
        x: f32,
        y: f32,
        rx: f32,
        ry: f32,
        rotation: f32,
        start: f32,
        end: f32,
        counterclockwise: bool,
    ) {
        if !(x.is_finite()
            && y.is_finite()
            && rx.is_finite()
            && ry.is_finite()
            && rotation.is_finite()
            && start.is_finite()
            && end.is_finite())
        {
            return;
        }
        if rx < 0.0 || ry < 0.0 {
            return;
        }
        if rx == 0.0 || ry == 0.0 || same_float(start, end) {
            self.line_to(x + rx * start.cos(), y + ry * start.sin());
            return;
        }
        self.ellipse_arc(x, y, rx, ry, rotation, start, end, counterclockwise);
    }

    #[expect(clippy::too_many_arguments, reason = "shared by arc() and ellipse()")]
    fn ellipse_arc(
        &mut self,
        cx: f32,
        cy: f32,
        rx: f32,
        ry: f32,
        rotation: f32,
        start: f32,
        end: f32,
        counterclockwise: bool,
    ) {
        let raw = f64::from(if counterclockwise {
            start - end
        } else {
            end - start
        });
        let tau = std::f64::consts::TAU;
        let tolerance = 1e-6;
        let sweep = if raw >= tau {
            tau
        } else {
            let modulo = raw.rem_euclid(tau);
            if modulo <= tolerance {
                if raw < 0.0 { tau } else { 0.0 }
            } else {
                modulo
            }
        };
        let delta = if counterclockwise { -sweep } else { sweep };
        #[expect(clippy::cast_possible_truncation, reason = "the sweep is at most 2π")]
        let delta = delta as f32;
        let mut start_point = ellipse_point(cx, cy, rx, ry, rotation, start);
        self.state.transform.map_point(&mut start_point);
        match self.current {
            Some(current) if current.distance(start_point) > f32::EPSILON => {
                self.path.push(Segment::LineTo(start_point));
            }
            None => {
                self.path.push(Segment::MoveTo(start_point));
                self.subpath_start = start_point;
            }
            _ => {}
        }
        self.current = Some(start_point);
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "the count is a finite positive angle span, far below u32::MAX"
        )]
        let segments = ((delta.abs() / FRAC_PI_4).ceil() as u32).max(1);
        #[expect(
            clippy::cast_precision_loss,
            reason = "segment counts are far below 2^24"
        )]
        let step = delta / segments as f32;
        for index in 0..segments {
            #[expect(
                clippy::cast_precision_loss,
                reason = "segment counts are far below 2^24"
            )]
            let index = index as f32;
            let from = start + step * index;
            let to = from + step;
            let mut p0 = ellipse_point(cx, cy, rx, ry, rotation, from);
            let mut p1 = ellipse_point(cx, cy, rx, ry, rotation, to);
            let alpha = (4.0 / 3.0) * (step / 4.0).tan();
            let d0 = ellipse_derivative(cx, cy, rx, ry, rotation, from);
            let d1 = ellipse_derivative(cx, cy, rx, ry, rotation, to);
            let mut c1 = Point::from_xy(p0.x + alpha * d0.x, p0.y + alpha * d0.y);
            let mut c2 = Point::from_xy(p1.x - alpha * d1.x, p1.y - alpha * d1.y);
            for point in [&mut p0, &mut p1, &mut c1, &mut c2] {
                self.state.transform.map_point(point);
            }
            self.path.push(Segment::Cubic(c1, c2, p1));
        }
        if let Some(Segment::Cubic(_, _, end)) = self.path.last() {
            self.current = Some(*end);
        }
    }

    pub(crate) fn arc_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, radius: f32) {
        if radius < 0.0 {
            return;
        }
        if !(x1.is_finite() && y1.is_finite() && x2.is_finite() && y2.is_finite()) {
            return;
        }
        let Some(mut current) = self.current else {
            self.move_to(x1, y1);
            return;
        };
        let Some(inverse) = self.state.transform.invert() else {
            return;
        };
        inverse.map_point(&mut current);
        if radius == 0.0 || (same_float(current.x, x1) && same_float(current.y, y1)) {
            self.line_to(x1, y1);
            return;
        }
        let p0 = current;
        let p1 = Point::from_xy(x1, y1);
        let p2 = Point::from_xy(x2, y2);
        let v1 = Point::from_xy(p0.x - p1.x, p0.y - p1.y);
        let v2 = Point::from_xy(p2.x - p1.x, p2.y - p1.y);
        let l1 = (v1.x * v1.x + v1.y * v1.y).sqrt();
        let l2 = (v2.x * v2.x + v2.y * v2.y).sqrt();
        if l1 == 0.0 || l2 == 0.0 {
            self.line_to(x1, y1);
            return;
        }
        let u1 = Point::from_xy(v1.x / l1, v1.y / l1);
        let u2 = Point::from_xy(v2.x / l2, v2.y / l2);
        let cross = u1.x * u2.y - u1.y * u2.x;
        let dot = (u1.x * u2.x + u1.y * u2.y).clamp(-1.0, 1.0);
        if cross.abs() < 1e-6 {
            self.line_to(x1, y1);
            return;
        }
        let angle = dot.acos();
        let tangent = radius / (angle / 2.0).tan().abs();
        if !tangent.is_finite() {
            self.line_to(x1, y1);
            return;
        }
        let t1 = Point::from_xy(p1.x + u1.x * tangent, p1.y + u1.y * tangent);
        let t2 = Point::from_xy(p1.x + u2.x * tangent, p1.y + u2.y * tangent);
        let bisector = Point::from_xy(u1.x + u2.x, u1.y + u2.y);
        let length = (bisector.x * bisector.x + bisector.y * bisector.y).sqrt();
        if length == 0.0 {
            self.line_to(x1, y1);
            return;
        }
        let distance = radius / (angle / 2.0).sin().abs();
        let center = Point::from_xy(
            p1.x + bisector.x / length * distance,
            p1.y + bisector.y / length * distance,
        );
        self.line_to(t1.x, t1.y);
        let start = (t1.y - center.y).atan2(t1.x - center.x);
        let end = (t2.y - center.y).atan2(t2.x - center.x);
        self.ellipse_arc(
            center.x,
            center.y,
            radius,
            radius,
            0.0,
            start,
            end,
            cross > 0.0,
        );
    }

    #[expect(
        clippy::similar_names,
        clippy::too_many_lines,
        reason = "one corner radius per corner and axis is the spec's own model"
    )]
    pub(crate) fn round_rect(
        &mut self,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        radii: &[(f32, f32)],
    ) {
        if !(x.is_finite() && y.is_finite() && width.is_finite() && height.is_finite()) {
            return;
        }
        if width == 0.0 || height == 0.0 {
            self.rect(x, y, width, height);
            self.start_round_rect_tail(x, y);
            return;
        }
        if radii
            .iter()
            .any(|(rx, ry)| !rx.is_finite() || !ry.is_finite())
        {
            return;
        }
        let clockwise = (width < 0.0) == (height < 0.0);
        let flip_x = width < 0.0;
        let flip_y = height < 0.0;
        let (mut left, mut top) = (x, y);
        let mut width = width;
        let mut height = height;
        if width < 0.0 {
            left += width;
            width = -width;
        }
        if height < 0.0 {
            top += height;
            height = -height;
        }
        let [
            (mut tl_x, mut tl_y),
            (mut tr_x, mut tr_y),
            (mut br_x, mut br_y),
            (mut bl_x, mut bl_y),
        ] = round_rect_radii(radii, width, height);
        if flip_x {
            std::mem::swap(&mut tl_x, &mut tr_x);
            std::mem::swap(&mut tl_y, &mut tr_y);
            std::mem::swap(&mut bl_x, &mut br_x);
            std::mem::swap(&mut bl_y, &mut br_y);
        }
        if flip_y {
            std::mem::swap(&mut tl_x, &mut bl_x);
            std::mem::swap(&mut tl_y, &mut bl_y);
            std::mem::swap(&mut tr_x, &mut br_x);
            std::mem::swap(&mut tr_y, &mut br_y);
        }
        let start = {
            let mut point = Point::from_xy(left + tl_x, top);
            self.state.transform.map_point(&mut point);
            point
        };
        let right = left + width;
        let bottom = top + height;
        let transform = self.state.transform;
        let map = |x: f32, y: f32| {
            let mut point = Point::from_xy(x, y);
            transform.map_point(&mut point);
            point
        };
        // https://html.spec.whatwg.org/multipage/canvas.html#dom-context-2d-roundrect
        let cw = [
            Segment::MoveTo(start),
            Segment::LineTo(map(right - tr_x, top)),
            Segment::Cubic(
                map(right - tr_x * (1.0 - KAPPA), top),
                map(right, top + tr_y * (1.0 - KAPPA)),
                map(right, top + tr_y),
            ),
            Segment::LineTo(map(right, bottom - br_y)),
            Segment::Cubic(
                map(right, bottom - br_y * (1.0 - KAPPA)),
                map(right - br_x * (1.0 - KAPPA), bottom),
                map(right - br_x, bottom),
            ),
            Segment::LineTo(map(left + bl_x, bottom)),
            Segment::Cubic(
                map(left + bl_x * (1.0 - KAPPA), bottom),
                map(left, bottom - bl_y * (1.0 - KAPPA)),
                map(left, bottom - bl_y),
            ),
            Segment::LineTo(map(left, top + tl_y)),
            Segment::Cubic(
                map(left, top + tl_y * (1.0 - KAPPA)),
                map(left + tl_x * (1.0 - KAPPA), top),
                map(left + tl_x, top),
            ),
            Segment::Close,
        ];
        if clockwise {
            self.path.extend_from_slice(&cw);
        } else {
            let [
                Segment::MoveTo(start),
                Segment::LineTo(tr_start),
                Segment::Cubic(tr_1, tr_2, tr_end),
                Segment::LineTo(br_start),
                Segment::Cubic(br_1, br_2, br_end),
                Segment::LineTo(bl_start),
                Segment::Cubic(bl_1, bl_2, bl_end),
                Segment::LineTo(tl_start),
                Segment::Cubic(tl_1, tl_2, _),
                Segment::Close,
            ] = cw
            else {
                return;
            };
            self.path.extend_from_slice(&[
                Segment::MoveTo(start),
                Segment::Cubic(tl_2, tl_1, tl_start),
                Segment::LineTo(bl_end),
                Segment::Cubic(bl_2, bl_1, bl_start),
                Segment::LineTo(br_end),
                Segment::Cubic(br_2, br_1, br_start),
                Segment::LineTo(tr_end),
                Segment::Cubic(tr_2, tr_1, tr_start),
                Segment::LineTo(start),
                Segment::Close,
            ]);
        }
        self.start_round_rect_tail(x, y);
    }

    // https://html.spec.whatwg.org/multipage/canvas.html#dom-context-2d-roundrect
    fn start_round_rect_tail(&mut self, x: f32, y: f32) {
        let mut origin = Point::from_xy(x, y);
        self.state.transform.map_point(&mut origin);
        self.path.push(Segment::MoveTo(origin));
        self.subpath_start = origin;
        self.current = Some(origin);
    }

    fn build_path(&self) -> Option<Path> {
        let mut builder = PathBuilder::new();
        for segment in &self.path {
            match *segment {
                Segment::MoveTo(point) => builder.move_to(point.x, point.y),
                Segment::LineTo(point) => builder.line_to(point.x, point.y),
                Segment::Quad(control, end) => {
                    builder.quad_to(control.x, control.y, end.x, end.y);
                }
                Segment::Cubic(first, second, end) => {
                    builder.cubic_to(first.x, first.y, second.x, second.y, end.x, end.y);
                }
                Segment::Close => builder.close(),
            }
        }
        builder.finish()
    }

    pub(crate) fn is_point_in_path(&self, x: f32, y: f32, even_odd: bool) -> bool {
        let Some(path) = self.build_path() else {
            return false;
        };
        self.is_point_in_built_path(&path, x, y, even_odd)
    }

    fn is_point_in_built_path(&self, path: &Path, x: f32, y: f32, even_odd: bool) -> bool {
        if !x.is_finite() || !y.is_finite() || !transform_is_invertible(self.state.transform) {
            return false;
        }
        let rule = if even_odd {
            FillRule::EvenOdd
        } else {
            FillRule::Winding
        };
        // https://html.spec.whatwg.org/multipage/canvas.html#dom-context-2d-ispointinpath
        path_contains(path, rule, Point::from_xy(x, y))
    }

    pub(crate) fn is_point_in_stroke(&self, x: f32, y: f32) -> bool {
        let Some(path) = self.build_path() else {
            return false;
        };
        self.is_point_in_built_stroke(&path, x, y)
    }

    fn is_point_in_built_stroke(&self, path: &Path, x: f32, y: f32) -> bool {
        if !(x.is_finite() && y.is_finite()) {
            return false;
        }
        if !transform_is_invertible(self.state.transform) {
            return false;
        }
        let Some(inverse) = self.state.transform.invert() else {
            return false;
        };
        let Some(user_path) = path.clone().transform(inverse) else {
            return false;
        };
        let stroke = self.stroke_options();
        let Some(user_path) = prune_degenerate(&user_path) else {
            return false;
        };
        let Some(outline) = dash_and_stroke(&user_path, &stroke, self.state.transform) else {
            return false;
        };
        let Some(outline) = outline.transform(self.state.transform) else {
            return false;
        };
        path_contains(&outline, FillRule::Winding, Point::from_xy(x, y))
    }

    fn canvas_mask(&self) -> Option<Mask> {
        let width = self.width.max(1);
        let height = self.height.max(1);
        crate::js::world::canvas_bytes(width, height)?;
        Mask::new(width, height)
    }

    // https://html.spec.whatwg.org/multipage/canvas.html#dom-context-2d-fill
    pub(crate) fn fill_borrowed_path(&mut self, other: &Canvas2D, even_odd: bool) {
        let Some(user_path) = other.build_path() else {
            return;
        };
        let Some(device_path) = user_path.transform(self.state.transform) else {
            return;
        };
        self.fill_path(&device_path, even_odd);
    }

    pub(crate) fn stroke_borrowed_path(&mut self, other: &Canvas2D) {
        let Some(user_path) = other.build_path() else {
            return;
        };
        let Some(device_path) = user_path.transform(self.state.transform) else {
            return;
        };
        self.stroke_path(&device_path);
    }

    pub(crate) fn clip_borrowed_path(&mut self, other: &Canvas2D, even_odd: bool) {
        let Some(user_path) = other.build_path() else {
            self.state.clip = self.canvas_mask().map(Arc::new);
            return;
        };
        let Some(device_path) = user_path.transform(self.state.transform) else {
            return;
        };
        self.clip_built_path(&device_path, even_odd);
    }

    pub(crate) fn is_point_in_borrowed_path(
        &self,
        other: &Canvas2D,
        x: f32,
        y: f32,
        even_odd: bool,
    ) -> bool {
        let Some(user_path) = other.build_path() else {
            return false;
        };
        let Some(device_path) = user_path.transform(self.state.transform) else {
            return false;
        };
        self.is_point_in_built_path(&device_path, x, y, even_odd)
    }

    pub(crate) fn is_point_in_borrowed_stroke(&self, other: &Canvas2D, x: f32, y: f32) -> bool {
        let Some(user_path) = other.build_path() else {
            return false;
        };
        let Some(device_path) = user_path.transform(self.state.transform) else {
            return false;
        };
        self.is_point_in_built_stroke(&device_path, x, y)
    }

    pub(crate) fn user_path(&self) -> Option<Path> {
        self.build_path()
    }

    pub(crate) fn current_point(&self) -> Option<Point> {
        self.current
    }

    pub(crate) fn append_path(&mut self, path: &Path) {
        for segment in path.segments() {
            match segment {
                tiny_skia::PathSegment::MoveTo(point) => {
                    self.path.push(Segment::MoveTo(point));
                    self.current = Some(point);
                    self.subpath_start = point;
                }
                tiny_skia::PathSegment::LineTo(point) => {
                    self.path.push(Segment::LineTo(point));
                    self.current = Some(point);
                }
                tiny_skia::PathSegment::QuadTo(control, point) => {
                    self.path.push(Segment::Quad(control, point));
                    self.current = Some(point);
                }
                tiny_skia::PathSegment::CubicTo(first, second, point) => {
                    self.path.push(Segment::Cubic(first, second, point));
                    self.current = Some(point);
                }
                tiny_skia::PathSegment::Close => {
                    self.path.push(Segment::Close);
                    self.current = Some(self.subpath_start);
                }
            }
        }
    }

    pub(crate) fn copy_path(&mut self, other: &Canvas2D) {
        self.path.clone_from(&other.path);
        self.current = other.current;
        self.subpath_start = other.subpath_start;
    }

    pub(crate) fn fill(&mut self, even_odd: bool) {
        let Some(path) = self.build_path() else {
            return;
        };
        self.fill_path(&path, even_odd);
    }

    pub(crate) fn stroke(&mut self) {
        let Some(path) = self.build_path() else {
            return;
        };
        self.stroke_path(&path);
    }

    pub(crate) fn clip(&mut self, even_odd: bool) {
        let Some(path) = self.build_path() else {
            self.state.clip = self.canvas_mask().map(Arc::new);
            return;
        };
        self.clip_built_path(&path, even_odd);
    }

    fn clip_built_path(&mut self, path: &Path, even_odd: bool) {
        let rule = if even_odd {
            FillRule::EvenOdd
        } else {
            FillRule::Winding
        };
        if let Some(mask) = &mut self.state.clip {
            Arc::make_mut(mask).intersect_path(path, rule, true, Transform::identity());
        } else {
            let Some(mut mask) = self.canvas_mask() else {
                return;
            };
            mask.fill_path(path, rule, true, Transform::identity());
            self.state.clip = Some(Arc::new(mask));
        }
    }

    pub(crate) fn fill_rect(&mut self, x: f32, y: f32, width: f32, height: f32) {
        if !(x.is_finite() && y.is_finite() && width.is_finite() && height.is_finite()) {
            return;
        }
        if width == 0.0 && height == 0.0 {
            return;
        }
        let Some(path) = self.rect_path(x, y, width, height) else {
            return;
        };
        self.fill_path(&path, false);
    }

    pub(crate) fn stroke_rect(&mut self, x: f32, y: f32, width: f32, height: f32) {
        if !(x.is_finite() && y.is_finite() && width.is_finite() && height.is_finite()) {
            return;
        }
        if width == 0.0 && height == 0.0 {
            return;
        }
        let Some(path) = self.rect_path(x, y, width, height) else {
            return;
        };
        self.stroke_path(&path);
    }

    pub(crate) fn clear_rect(&mut self, x: f32, y: f32, width: f32, height: f32) {
        if !(x.is_finite() && y.is_finite() && width.is_finite() && height.is_finite()) {
            return;
        }
        if width == 0.0 && height == 0.0 {
            return;
        }
        let Some(path) = self.rect_path(x, y, width, height) else {
            return;
        };
        let Some(pixmap) = &mut self.pixmap else {
            return;
        };
        let paint = Paint {
            blend_mode: BlendMode::Clear,
            ..Paint::default()
        };
        pixmap.fill_path(
            &path,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            self.state.clip.as_deref(),
        );
    }

    fn rect_path(&self, x: f32, y: f32, width: f32, height: f32) -> Option<Path> {
        let corners = [
            (x, y),
            (x + width, y),
            (x + width, y + height),
            (x, y + height),
        ];
        let mut builder = PathBuilder::new();
        for (index, (cx, cy)) in corners.into_iter().enumerate() {
            let mut point = Point::from_xy(cx, cy);
            self.state.transform.map_point(&mut point);
            if index == 0 {
                builder.move_to(point.x, point.y);
            } else {
                builder.line_to(point.x, point.y);
            }
        }
        builder.close();
        builder.finish()
    }

    // https://html.spec.whatwg.org/multipage/canvas.html#drawing-model
    fn with_draw_filter(
        &mut self,
        bounds: tiny_skia::Rect,
        draw: impl FnOnce(&mut Self, Transform),
    ) {
        let Some(matrix) = self.state.filter_matrix else {
            draw(self, Transform::identity());
            return;
        };
        let Some((left, top, width, height)) = self.filter_bounds(bounds) else {
            return;
        };
        let Some(layer) = make_pixmap(width, height) else {
            return;
        };
        let parent = self.pixmap.replace(layer);
        let saved_transform = self.state.transform;
        let saved_blend = std::mem::replace(&mut self.state.blend, BlendMode::SourceOver);
        let saved_shadow = self.state.shadow.clone();
        let saved_shadow_blur = self.state.shadow_blur;
        let saved_clip = self.state.clip.take();
        self.state.clip = saved_clip
            .as_ref()
            .and_then(|clip| crop_clip(clip, left, top, width, height))
            .map(Arc::new);
        self.state.shadow = CanvasColor::new([0, 0, 0, 0]);
        self.state.shadow_blur = 0.0;
        let offset = Transform::from_translate(-to_f32(left), -to_f32(top));
        self.state.transform = saved_transform.pre_concat(offset);
        draw(self, offset);
        self.state.transform = saved_transform;
        self.state.blend = saved_blend;
        self.state.shadow = saved_shadow;
        self.state.shadow_blur = saved_shadow_blur;
        self.state.clip = saved_clip;
        let Some(mut layer) = self.pixmap.take() else {
            self.pixmap = parent;
            return;
        };
        self.pixmap = parent;
        apply_color_matrix(&mut layer, &matrix);

        if self.shadow_drawn() {
            self.finish_shadow(
                layer.clone(),
                self.state.shadow_blur / 2.0,
                to_f32(left),
                to_f32(top),
                1.0,
            );
        }

        if clears_uncovered(self.state.blend)
            && let Some(coverage) = self.coverage_mask(&layer, left, top)
        {
            self.clear_uncovered_mask(&coverage);
        }
        let Some(parent_pixmap) = &mut self.pixmap else {
            return;
        };
        parent_pixmap.draw_pixmap(
            left,
            top,
            layer.as_ref(),
            &PixmapPaint {
                opacity: 1.0,
                blend_mode: self.state.blend,
                quality: FilterQuality::Nearest,
            },
            Transform::identity(),
            self.state.clip.as_deref(),
        );
    }

    fn filter_bounds(&self, bounds: tiny_skia::Rect) -> Option<(i32, i32, u32, u32)> {
        if !(bounds.left().is_finite()
            && bounds.top().is_finite()
            && bounds.right().is_finite()
            && bounds.bottom().is_finite())
        {
            return None;
        }
        let canvas_width = to_f32(self.width);
        let canvas_height = to_f32(self.height);
        let left = (bounds.left() - 1.0).clamp(0.0, canvas_width).floor();
        let top = (bounds.top() - 1.0).clamp(0.0, canvas_height).floor();
        let right = (bounds.right() + 1.0).clamp(0.0, canvas_width).ceil();
        let bottom = (bounds.bottom() + 1.0).clamp(0.0, canvas_height).ceil();
        let width = (right - left).to_u32()?;
        let height = (bottom - top).to_u32()?;
        if width == 0 || height == 0 {
            return None;
        }
        Some((left.to_i32()?, top.to_i32()?, width, height))
    }

    fn coverage_mask(&self, layer: &Pixmap, left: i32, top: i32) -> Option<Mask> {
        let mut coverage = Mask::new(self.width, self.height)?;
        let width = layer.width() as usize;
        let height = layer.height() as usize;
        let stride = self.width as usize;
        let data = layer.data();
        for y in 0..height {
            let canvas_y = top + i32::try_from(y).ok()?;
            if canvas_y < 0 || canvas_y >= i32::try_from(self.height).ok()? {
                continue;
            }
            for x in 0..width {
                let canvas_x = left + i32::try_from(x).ok()?;
                if canvas_x < 0 || canvas_x >= i32::try_from(self.width).ok()? {
                    continue;
                }
                let alpha = data[(y * width + x) * 4 + 3];
                let index =
                    usize::try_from(canvas_y).ok()? * stride + usize::try_from(canvas_x).ok()?;
                coverage.data_mut()[index] = alpha;
            }
        }
        Some(coverage)
    }

    fn shadow_drawn(&self) -> bool {
        self.state.shadow.rgba[3] != 0
            && (self.state.shadow_blur != 0.0
                || self.state.shadow_x != 0.0
                || self.state.shadow_y != 0.0)
    }

    fn fill_path(&mut self, path: &Path, even_odd: bool) {
        let bounds = path.bounds();
        self.with_draw_filter(bounds, |canvas, offset| {
            let Some(path) = path.clone().transform(offset) else {
                return;
            };
            canvas.fill_path_inner(&path, even_odd);
        });
    }

    fn fill_path_inner(&mut self, path: &Path, even_odd: bool) {
        let rule = if even_odd {
            FillRule::EvenOdd
        } else {
            FillRule::Winding
        };
        self.clear_uncovered(path, rule);
        self.paint_shadow(path, rule, None);
        if let Style::Pattern(id) = &self.state.fill
            && self
                .patterns
                .get(id)
                .is_some_and(|entry| !entry.origin_clean)
        {
            self.taint();
        }
        let style = &self.state.fill;
        let alpha = self.state.alpha;
        let pattern = match style {
            Style::Pattern(id) => self
                .patterns
                .get(id)
                .map(|entry| (entry.image.clone(), entry.repetition)),
            _ => None,
        };
        let shader = match style {
            Style::Color(color) => {
                let [r, g, b, a] = color.rgba;
                let alpha = f32::from(a) / 255.0 * alpha;
                Shader::SolidColor(Color::from_rgba8(r, g, b, to_u8(alpha)))
            }
            Style::Gradient(id) => {
                let Some(shader) = self.gradient_shader(*id, alpha) else {
                    return;
                };
                shader
            }
            Style::Pattern(id) => {
                let Some((image, _)) = &pattern else {
                    return;
                };
                let Some(entry) = self.patterns.get(id) else {
                    return;
                };
                tiny_skia::Pattern::new(
                    image.as_ref().as_ref(),
                    entry.spread,
                    entry.quality,
                    alpha,
                    self.state.transform.pre_concat(entry.transform),
                )
            }
        };
        let paint = Paint {
            shader,
            blend_mode: self.state.blend,
            anti_alias: true,
            ..Paint::default()
        };
        let pattern_mask = pattern
            .as_ref()
            .and_then(|(image, repetition)| self.pattern_mask(image, *repetition));
        let mask = pattern_mask.as_ref().or(self.state.clip.as_deref());
        let Some(pixmap) = &mut self.pixmap else {
            return;
        };
        pixmap.fill_path(path, &paint, rule, Transform::identity(), mask);
    }

    // https://html.spec.whatwg.org/multipage/canvas.html#dom-context-2d-globalcompositeoperation
    fn clear_uncovered(&mut self, path: &Path, rule: FillRule) {
        if !clears_uncovered(self.state.blend) {
            return;
        }
        let Some(mut shape) = Mask::new(self.width, self.height) else {
            return;
        };
        shape.fill_path(path, rule, true, Transform::identity());
        self.clear_uncovered_mask(&shape);
    }

    fn clear_uncovered_mask(&mut self, shape: &Mask) {
        let clip = match &self.state.clip {
            None => None,
            Some(clip) => {
                let size = tiny_skia::IntSize::from_wh(clip.width(), clip.height());
                let Some(clip) = size.and_then(|size| Mask::from_vec(clip.data().to_vec(), size))
                else {
                    return;
                };
                Some(clip)
            }
        };
        let Some(mut mask) = Mask::new(self.width, self.height) else {
            return;
        };
        for (index, target) in mask.data_mut().iter_mut().enumerate() {
            let coverage = u16::from(*shape.data().get(index).unwrap_or(&0));
            let clip = clip
                .as_ref()
                .map_or(255, |clip| u16::from(*clip.data().get(index).unwrap_or(&0)));
            let cleared = clip * (255 - coverage) / 255;
            *target = u8::try_from(255 - cleared).unwrap_or(0);
        }
        let Some(pixmap) = &mut self.pixmap else {
            return;
        };
        pixmap.apply_mask(&mask);
    }

    // https://html.spec.whatwg.org/multipage/canvas.html#dom-context-2d-createpattern
    fn pattern_mask(&self, image: &Arc<Pixmap>, repetition: PatternRepeat) -> Option<Mask> {
        const LIMIT: f32 = 1_000_000.0;
        #[expect(
            clippy::cast_precision_loss,
            reason = "pattern images are far below 2^24 pixels per side"
        )]
        let (width, height) = (image.width() as f32, image.height() as f32);
        let rect = match repetition {
            PatternRepeat::Repeat => return None,
            PatternRepeat::RepeatX => tiny_skia::Rect::from_ltrb(-LIMIT, 0.0, LIMIT, height)?,
            PatternRepeat::RepeatY => tiny_skia::Rect::from_ltrb(0.0, -LIMIT, width, LIMIT)?,
            PatternRepeat::NoRepeat => tiny_skia::Rect::from_ltrb(0.0, 0.0, width, height)?,
        };
        let band = PathBuilder::from_rect(rect).transform(self.state.transform)?;
        let mut mask = if let Some(clip) = &self.state.clip {
            let size = tiny_skia::IntSize::from_wh(clip.width(), clip.height())?;
            Mask::from_vec(clip.data().to_vec(), size)?
        } else {
            let (pixmap_width, pixmap_height) = self.pixmap_size();
            let mut mask = Mask::new(pixmap_width, pixmap_height)?;
            #[expect(
                clippy::cast_precision_loss,
                reason = "canvas sides are far below 2^24"
            )]
            let full =
                tiny_skia::Rect::from_xywh(0.0, 0.0, pixmap_width as f32, pixmap_height as f32)?;
            mask.fill_path(
                &PathBuilder::from_rect(full),
                FillRule::Winding,
                false,
                Transform::identity(),
            );
            mask
        };
        mask.intersect_path(&band, FillRule::Winding, true, Transform::identity());
        Some(mask)
    }

    fn pixmap_size(&self) -> (u32, u32) {
        self.pixmap
            .as_ref()
            .map_or((self.width, self.height), |pixmap| {
                (pixmap.width(), pixmap.height())
            })
    }

    fn stroke_path(&mut self, path: &Path) {
        let scale = self
            .state
            .transform
            .sx
            .abs()
            .max(self.state.transform.sy.abs());
        let padding = self.state.line_width * scale * self.state.miter_limit.max(1.0) + 1.0;
        let bounds = path.bounds();
        let Some(bounds) = tiny_skia::Rect::from_ltrb(
            bounds.left() - padding,
            bounds.top() - padding,
            bounds.right() + padding,
            bounds.bottom() + padding,
        ) else {
            return;
        };
        self.with_draw_filter(bounds, |canvas, offset| {
            let Some(path) = path.clone().transform(offset) else {
                return;
            };
            canvas.stroke_path_inner(&path);
        });
    }

    fn stroke_path_inner(&mut self, path: &Path) {
        let stroke = self.stroke_options();
        self.paint_shadow(path, FillRule::Winding, Some(&stroke));
        if let Style::Pattern(id) = &self.state.stroke
            && self
                .patterns
                .get(id)
                .is_some_and(|entry| !entry.origin_clean)
        {
            self.taint();
        }
        let style = &self.state.stroke;
        let alpha = self.state.alpha;
        let pattern = match style {
            Style::Pattern(id) => self
                .patterns
                .get(id)
                .map(|entry| (entry.image.clone(), entry.repetition)),
            _ => None,
        };
        let shader = match style {
            Style::Color(color) => {
                let [r, g, b, a] = color.rgba;
                let alpha = f32::from(a) / 255.0 * alpha;
                Shader::SolidColor(Color::from_rgba8(r, g, b, to_u8(alpha)))
            }
            Style::Gradient(id) => {
                let Some(shader) = self.gradient_shader_at(*id, alpha, Transform::identity())
                else {
                    return;
                };
                shader
            }
            Style::Pattern(id) => {
                let Some((image, _)) = &pattern else {
                    return;
                };
                let Some(entry) = self.patterns.get(id) else {
                    return;
                };
                tiny_skia::Pattern::new(
                    image.as_ref().as_ref(),
                    entry.spread,
                    entry.quality,
                    alpha,
                    entry.transform,
                )
            }
        };
        let paint = Paint {
            shader,
            blend_mode: self.state.blend,
            anti_alias: true,
            ..Paint::default()
        };
        // https://html.spec.whatwg.org/multipage/canvas.html#dom-context-2d-stroke
        let Some(inverse) = self.state.transform.invert() else {
            return;
        };
        let Some(user_path) = path.clone().transform(inverse) else {
            return;
        };
        let Some(user_path) = prune_degenerate(&user_path) else {
            return;
        };
        if let Some(outline) = user_path.stroke(&stroke, 1.0)
            && let Some(outline) = outline.transform(self.state.transform)
        {
            self.clear_uncovered(&outline, FillRule::Winding);
        }
        let pattern_mask = pattern
            .as_ref()
            .and_then(|(image, repetition)| self.pattern_mask(image, *repetition));
        let mask = pattern_mask.as_ref().or(self.state.clip.as_deref());
        let Some(pixmap) = &mut self.pixmap else {
            return;
        };
        pixmap.stroke_path(&user_path, &paint, &stroke, self.state.transform, mask);
    }

    fn gradient_shader(&self, id: u32, alpha: f32) -> Option<Shader<'static>> {
        self.gradient_shader_at(id, alpha, self.state.transform)
    }

    fn gradient_shader_at(
        &self,
        id: u32,
        alpha: f32,
        transform: Transform,
    ) -> Option<Shader<'static>> {
        let gradient = self.gradients.get(&id)?;
        if gradient.stops.is_empty() || gradient.degenerate {
            return None;
        }
        let stops = match &gradient.kind {
            // https://html.spec.whatwg.org/multipage/canvas.html#dom-context-2d-createconicgradient
            GradientKind::Conic { angle, .. } => {
                conic_stops(&gradient.stops, angle.to_degrees(), alpha)
            }
            _ => gradient
                .stops
                .iter()
                .map(|(offset, [r, g, b, a])| {
                    GradientStop::new(
                        *offset,
                        Color::from_rgba8(*r, *g, *b, to_u8(f32::from(*a) / 255.0 * alpha)),
                    )
                })
                .collect(),
        };
        match gradient.kind {
            GradientKind::Linear { start, end } => {
                tiny_skia::LinearGradient::new(start, end, stops, SpreadMode::Pad, transform)
            }
            GradientKind::Radial {
                start,
                start_radius,
                end,
                end_radius,
            } => tiny_skia::RadialGradient::new(
                start,
                start_radius,
                end,
                end_radius,
                stops,
                SpreadMode::Pad,
                transform,
            ),
            GradientKind::Conic { center, .. } => {
                tiny_skia::SweepGradient::new(center, 0.0, 360.0, stops, SpreadMode::Pad, transform)
            }
        }
    }

    pub(crate) fn gradient_definition(&self, id: u32) -> Option<Gradient> {
        self.gradients.get(&id).cloned()
    }

    pub(crate) fn insert_gradient(&mut self, gradient: Gradient) -> u32 {
        let id = self.next_resource;
        self.next_resource += 1;
        self.gradients.insert(id, gradient);
        id
    }

    pub(crate) fn set_pattern_transform(&mut self, id: u32, transform: Transform) {
        if let Some(pattern) = self.patterns.get_mut(&id) {
            pattern.transform = transform;
        }
    }

    pub(crate) fn pattern_definition(&self, id: u32) -> Option<Pattern> {
        self.patterns.get(&id).cloned()
    }

    pub(crate) fn insert_pattern(&mut self, pattern: Pattern) -> u32 {
        let id = self.next_resource;
        self.next_resource += 1;
        self.patterns.insert(id, pattern);
        id
    }

    pub(crate) fn create_linear_gradient(&mut self, x0: f32, y0: f32, x1: f32, y1: f32) -> u32 {
        let id = self.next_resource;
        self.next_resource += 1;
        self.gradients.insert(
            id,
            Gradient {
                kind: GradientKind::Linear {
                    start: Point::from_xy(x0, y0),
                    end: Point::from_xy(x1, y1),
                },
                stops: Vec::new(),
                degenerate: (x0 - x1).abs() < f32::EPSILON && (y0 - y1).abs() < f32::EPSILON,
            },
        );
        id
    }

    pub(crate) fn create_radial_gradient(
        &mut self,
        x0: f32,
        y0: f32,
        r0: f32,
        x1: f32,
        y1: f32,
        r1: f32,
    ) -> u32 {
        let id = self.next_resource;
        self.next_resource += 1;
        self.gradients.insert(
            id,
            Gradient {
                kind: GradientKind::Radial {
                    start: Point::from_xy(x0, y0),
                    start_radius: r0,
                    end: Point::from_xy(x1, y1),
                    end_radius: r1,
                },
                stops: Vec::new(),
                // https://html.spec.whatwg.org/multipage/canvas.html#dom-context-2d-createradialgradient
                degenerate: (r0 - r1).abs() < f32::EPSILON
                    && (x0 - x1).abs() < f32::EPSILON
                    && (y0 - y1).abs() < f32::EPSILON,
            },
        );
        id
    }

    pub(crate) fn create_conic_gradient(&mut self, x: f32, y: f32, angle: f32) -> u32 {
        let id = self.next_resource;
        self.next_resource += 1;
        self.gradients.insert(
            id,
            Gradient {
                kind: GradientKind::Conic {
                    center: Point::from_xy(x, y),
                    angle,
                },
                stops: Vec::new(),
                degenerate: false,
            },
        );
        id
    }

    pub(crate) fn add_color_stop(&mut self, id: u32, offset: f32, color: [u8; 4]) -> bool {
        let Some(gradient) = self.gradients.get_mut(&id) else {
            return false;
        };
        if !(0.0..=1.0).contains(&offset) {
            return false;
        }
        let index = gradient
            .stops
            .iter()
            .position(|(existing, _)| *existing > offset)
            .unwrap_or(gradient.stops.len());
        gradient.stops.insert(index, (offset, color));
        true
    }

    pub(crate) fn create_pattern(
        &mut self,
        source: SourceImage,
        repetition: &str,
        smoothing: bool,
    ) -> Option<u32> {
        let pixmap = Pixmap::from_vec(
            to_premultiplied(&source.pixels),
            tiny_skia::IntSize::from_wh(source.width, source.height)?,
        )?;
        let origin_clean = source.origin_clean;
        let (spread, repetition) = match repetition {
            "repeat" => (SpreadMode::Repeat, PatternRepeat::Repeat),
            "repeat-x" => (SpreadMode::Repeat, PatternRepeat::RepeatX),
            "repeat-y" => (SpreadMode::Repeat, PatternRepeat::RepeatY),
            "no-repeat" => (SpreadMode::Pad, PatternRepeat::NoRepeat),
            _ => return None,
        };
        let quality = if smoothing {
            FilterQuality::Bilinear
        } else {
            FilterQuality::Nearest
        };
        let id = self.next_resource;
        self.next_resource += 1;
        self.patterns.insert(
            id,
            Pattern {
                image: Arc::new(pixmap),
                spread,
                repetition,
                quality,
                origin_clean,
                transform: Transform::identity(),
            },
        );
        Some(id)
    }

    pub(crate) fn to_png(&self) -> Option<Vec<u8>> {
        let pixels = self.read_pixels(0, 0, self.width, self.height)?;
        let mut png = Vec::new();
        image::ImageEncoder::write_image(
            image::codecs::png::PngEncoder::new(&mut png),
            &pixels,
            self.width,
            self.height,
            image::ExtendedColorType::Rgba8,
        )
        .ok()?;
        Some(png)
    }

    pub(crate) fn to_jpeg(&self, quality: JpegQuality) -> Option<Vec<u8>> {
        let pixels = self.read_pixels(0, 0, self.width, self.height)?;
        // https://html.spec.whatwg.org/multipage/canvas.html#a-serialisation-of-the-bitmap-as-a-file
        let mut rgb = Vec::with_capacity(pixels.len() / 4 * 3);
        for pixel in pixels.as_chunks::<4>().0 {
            let alpha = u16::from(pixel[3]);
            for channel in &pixel[..3] {
                let value = (u16::from(*channel) * alpha + 127) / 255;
                rgb.push(u8::try_from(value).unwrap_or(u8::MAX));
            }
        }
        encode_jpeg(
            &rgb,
            self.width,
            self.height,
            image::ExtendedColorType::Rgb8,
            quality,
        )
    }

    pub(crate) fn blank_png(width: u32, height: u32) -> Option<Vec<u8>> {
        crate::js::world::canvas_bytes(width, height)?;
        let row_len = usize::try_from(width).ok()?.checked_mul(4)?;
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, width, height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().ok()?;
            {
                let mut stream = writer.stream_writer().ok()?;
                let row = vec![0; row_len];
                for _ in 0..height {
                    std::io::Write::write_all(&mut stream, &row).ok()?;
                }
                stream.finish().ok()?;
            }
        }
        Some(out)
    }

    pub(crate) fn blank_jpeg(width: u32, height: u32, quality: JpegQuality) -> Option<Vec<u8>> {
        let pixels = crate::js::world::canvas_bytes(width, height)?;
        let pixels = pixels / 4 * 3;
        encode_jpeg(
            &vec![0; pixels],
            width,
            height,
            image::ExtendedColorType::Rgb8,
            quality,
        )
    }

    // https://html.spec.whatwg.org/multipage/canvas.html#dom-context-2d-getimagedata
    pub(crate) fn read_pixels(&self, x: i32, y: i32, width: u32, height: u32) -> Option<Vec<u8>> {
        let mut out = vec![0; crate::js::world::canvas_bytes(width, height)?];
        let Some(pixmap) = &self.pixmap else {
            return Some(out);
        };
        for row in 0..height {
            for column in 0..width {
                let sx = i64::from(x) + i64::from(column);
                let sy = i64::from(y) + i64::from(row);
                if sx < 0 || sy < 0 {
                    continue;
                }
                let (Ok(sx), Ok(sy)) = (u32::try_from(sx), u32::try_from(sy)) else {
                    continue;
                };
                if sx >= pixmap.width() || sy >= pixmap.height() {
                    continue;
                }
                let Some(pixel) = pixmap.pixel(sx, sy) else {
                    continue;
                };
                let color = pixel.demultiply();
                let offset = (row as usize * width as usize + column as usize) * 4;
                out[offset] = color.red();
                out[offset + 1] = color.green();
                out[offset + 2] = color.blue();
                out[offset + 3] = color.alpha();
            }
        }
        Some(out)
    }

    // https://html.spec.whatwg.org/multipage/canvas.html#dom-context-2d-putimagedata
    #[expect(
        clippy::similar_names,
        reason = "source and target corners are named for their axis"
    )]
    pub(crate) fn write_pixels(
        &mut self,
        source: &[u8],
        source_width: u32,
        source_height: u32,
        dx: i32,
        dy: i32,
        dirty: Option<(i32, i32, i32, i32)>,
    ) {
        let Some(pixmap) = &mut self.pixmap else {
            return;
        };
        let (mut dirty_x, mut dirty_y, mut dirty_width, mut dirty_height) = dirty.unwrap_or((
            0,
            0,
            i32::try_from(source_width).unwrap_or(i32::MAX),
            i32::try_from(source_height).unwrap_or(i32::MAX),
        ));
        if dirty_width < 0 {
            if dirty_x < 0 {
                dirty_x = 0;
                dirty_width = 0;
            } else {
                dirty_x += dirty_width;
                dirty_width = -dirty_width;
            }
        }
        if dirty_height < 0 {
            if dirty_y < 0 {
                dirty_y = 0;
                dirty_height = 0;
            } else {
                dirty_y += dirty_height;
                dirty_height = -dirty_height;
            }
        }
        let mut source_x0 = i64::from(dirty_x).max(0);
        let mut source_y0 = i64::from(dirty_y).max(0);
        let mut source_x1 =
            (i64::from(dirty_x) + i64::from(dirty_width)).min(i64::from(source_width));
        let mut source_y1 =
            (i64::from(dirty_y) + i64::from(dirty_height)).min(i64::from(source_height));
        if source_x1 <= source_x0 || source_y1 <= source_y0 {
            return;
        }
        // https://html.spec.whatwg.org/multipage/canvas.html#dom-context-2d-putimagedata
        let target_x0 = source_x0 + i64::from(dx);
        let target_y0 = source_y0 + i64::from(dy);
        let target_x1 = source_x1 + i64::from(dx);
        let target_y1 = source_y1 + i64::from(dy);
        let clip_x0 = target_x0.max(0);
        let clip_y0 = target_y0.max(0);
        let clip_x1 = target_x1.min(i64::from(pixmap.width()));
        let clip_y1 = target_y1.min(i64::from(pixmap.height()));
        if clip_x1 <= clip_x0 || clip_y1 <= clip_y0 {
            return;
        }
        source_x0 += clip_x0 - target_x0;
        source_y0 += clip_y0 - target_y0;
        source_x1 += clip_x1 - target_x1;
        source_y1 += clip_y1 - target_y1;
        for sy in source_y0..source_y1 {
            for sx in source_x0..source_x1 {
                let source_row = usize::try_from(sy).unwrap_or(0);
                let source_column = usize::try_from(sx).unwrap_or(0);
                let source_offset = (source_row * source_width as usize + source_column) * 4;
                let Some(pixel) = source.get(source_offset..source_offset + 4) else {
                    continue;
                };
                let target_x = u32::try_from(sx + i64::from(dx)).unwrap_or(u32::MAX);
                let target_y = u32::try_from(sy + i64::from(dy)).unwrap_or(u32::MAX);
                let color =
                    ColorU8::from_rgba(pixel[0], pixel[1], pixel[2], pixel[3]).premultiply();
                let index = (target_y as usize * pixmap.width() as usize + target_x as usize) * 4;
                if let Some(slot) = pixmap.data_mut().get_mut(index..index + 4) {
                    slot.copy_from_slice(&[
                        color.red(),
                        color.green(),
                        color.blue(),
                        color.alpha(),
                    ]);
                }
            }
        }
    }

    pub(crate) fn draw_image(&mut self, source: &SourceImage, src: Rect, dst: Rect) {
        let bounds = if let Some(path) = self.rect_path(dst.x, dst.y, dst.width, dst.height) {
            path.bounds()
        } else {
            let mut left = dst.x;
            let mut top = dst.y;
            let mut right = dst.x + dst.width;
            let mut bottom = dst.y + dst.height;
            if left > right {
                std::mem::swap(&mut left, &mut right);
            }
            if top > bottom {
                std::mem::swap(&mut top, &mut bottom);
            }
            let mut corners = [
                Point::from_xy(left, top),
                Point::from_xy(right, top),
                Point::from_xy(right, bottom),
                Point::from_xy(left, bottom),
            ];
            for corner in &mut corners {
                self.state.transform.map_point(corner);
            }
            let left = corners
                .iter()
                .map(|point| point.x)
                .fold(f32::INFINITY, f32::min);
            let top = corners
                .iter()
                .map(|point| point.y)
                .fold(f32::INFINITY, f32::min);
            let right = corners
                .iter()
                .map(|point| point.x)
                .fold(f32::NEG_INFINITY, f32::max);
            let bottom = corners
                .iter()
                .map(|point| point.y)
                .fold(f32::NEG_INFINITY, f32::max);
            let Some(bounds) = tiny_skia::Rect::from_ltrb(left, top, right, bottom) else {
                return;
            };
            bounds
        };
        self.with_draw_filter(bounds, |canvas, _| {
            canvas.draw_image_inner(source, src, dst);
        });
    }

    fn draw_image_inner(&mut self, source: &SourceImage, src: Rect, dst: Rect) {
        if !(src.x.is_finite()
            && src.y.is_finite()
            && src.width.is_finite()
            && src.height.is_finite()
            && dst.x.is_finite()
            && dst.y.is_finite()
            && dst.width.is_finite()
            && dst.height.is_finite())
        {
            return;
        }
        if !source.origin_clean {
            self.taint();
        }
        if src.width == 0.0 || src.height == 0.0 || dst.width == 0.0 || dst.height == 0.0 {
            return;
        }
        // https://html.spec.whatwg.org/multipage/canvas.html#dom-context-2d-drawimage
        let src = src.normalized();
        let dst = dst.normalized();
        let Rect {
            x: sx,
            y: sy,
            width: sw,
            height: sh,
        } = src;
        let Rect {
            x: dx,
            y: dy,
            width: dw,
            height: dh,
        } = dst;
        let Some(pixmap) = Pixmap::from_vec(
            to_premultiplied(&source.pixels),
            tiny_skia::IntSize::from_wh(source.width, source.height)
                .unwrap_or(tiny_skia::IntSize::from_wh(1, 1).expect("1x1 is a valid size")),
        ) else {
            return;
        };
        let pattern = Arc::new(pixmap);
        let quality = if self.state.smoothing {
            FilterQuality::Bilinear
        } else {
            FilterQuality::Nearest
        };
        let scale_x = dw / sw;
        let scale_y = dh / sh;
        let image_to_user = Transform::from_row(
            scale_x,
            0.0,
            0.0,
            scale_y,
            dx - sx * scale_x,
            dy - sy * scale_y,
        );
        let transform = self.state.transform.pre_concat(image_to_user);
        let paint = Paint {
            shader: tiny_skia::Pattern::new(
                pattern.as_ref().as_ref(),
                SpreadMode::Pad,
                quality,
                self.state.alpha,
                transform,
            ),
            blend_mode: self.state.blend,
            anti_alias: true,
            ..Paint::default()
        };
        let Some(path) = self.rect_path(dx, dy, dw, dh) else {
            return;
        };
        self.clear_uncovered(&path, FillRule::Winding);
        self.paint_image_shadow(&path, &pattern, quality, transform);
        let Some(pixmap) = &mut self.pixmap else {
            return;
        };
        pixmap.fill_path(
            &path,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            self.state.clip.as_deref(),
        );
        if !source.origin_clean {
            self.taint();
        }
    }

    fn paint_image_shadow(
        &mut self,
        path: &Path,
        pattern: &Arc<Pixmap>,
        quality: FilterQuality,
        image_transform: Transform,
    ) {
        let [_, _, _, alpha] = self.state.shadow.rgba;
        if alpha == 0
            || (self.state.shadow_blur == 0.0
                && self.state.shadow_x == 0.0
                && self.state.shadow_y == 0.0)
        {
            return;
        }
        let bounds = path.bounds();
        if !(bounds.width().is_finite() && bounds.height().is_finite()) {
            return;
        }
        let sigma = self.state.shadow_blur / 2.0;
        let padding = (sigma * 3.0).ceil() + 1.0;
        let left = bounds.left() - padding;
        let top = bounds.top() - padding;
        let width = bounds.width() + padding * 2.0;
        let height = bounds.height() + padding * 2.0;
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "the bitmap sides are checked against a fixed cap below"
        )]
        let (bitmap_width, bitmap_height) = (width.ceil() as u32, height.ceil() as u32);
        if bitmap_width == 0
            || bitmap_height == 0
            || bitmap_width > MAX_SHADOW_SIDE
            || bitmap_height > MAX_SHADOW_SIDE
        {
            return;
        }
        let Some(mut mask) = Pixmap::new(bitmap_width, bitmap_height) else {
            return;
        };
        let shift = Transform::from_translate(-left, -top);
        let paint = Paint {
            shader: tiny_skia::Pattern::new(
                pattern.as_ref().as_ref(),
                SpreadMode::Pad,
                quality,
                1.0,
                shift.pre_concat(image_transform),
            ),
            blend_mode: BlendMode::SourceOver,
            anti_alias: true,
            ..Paint::default()
        };
        let Some(shifted) = path.clone().transform(shift) else {
            return;
        };
        mask.fill_path(
            &shifted,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
        self.finish_shadow(mask, sigma, left, top, self.state.alpha);
    }

    // https://html.spec.whatwg.org/multipage/canvas.html#shadows
    fn paint_shadow(&mut self, path: &Path, rule: FillRule, stroke: Option<&Stroke>) {
        let [_, _, _, alpha] = self.state.shadow.rgba;
        if alpha == 0
            || (self.state.shadow_blur == 0.0
                && self.state.shadow_x == 0.0
                && self.state.shadow_y == 0.0)
        {
            return;
        }
        let bounds = path.bounds();
        if !(bounds.width().is_finite() && bounds.height().is_finite()) {
            return;
        }
        let sigma = self.state.shadow_blur / 2.0;
        let blur_padding = (sigma * 3.0).ceil() + 1.0;
        let scale = self
            .state
            .transform
            .sx
            .abs()
            .max(self.state.transform.sy.abs());
        let stroke_padding = stroke.map_or(0.0, |stroke| {
            stroke.width * scale * stroke.miter_limit.max(1.0) + 1.0
        });
        let padding = blur_padding + stroke_padding;
        let left = bounds.left() - padding;
        let top = bounds.top() - padding;
        let width = bounds.width() + padding * 2.0;
        let height = bounds.height() + padding * 2.0;
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "the bitmap sides are checked against a fixed cap below"
        )]
        let (bitmap_width, bitmap_height) = (width.ceil() as u32, height.ceil() as u32);
        if bitmap_width == 0
            || bitmap_height == 0
            || bitmap_width > MAX_SHADOW_SIDE
            || bitmap_height > MAX_SHADOW_SIDE
        {
            return;
        }
        let Some(mut mask) = Pixmap::new(bitmap_width, bitmap_height) else {
            return;
        };
        let shift = Transform::from_translate(-left, -top);
        let style = if stroke.is_some() {
            &self.state.stroke
        } else {
            &self.state.fill
        };
        let Some(paint) = self.shadow_mask_paint(style) else {
            return;
        };
        let Some(inverse) = self.state.transform.invert() else {
            return;
        };
        let Some(user_path) = path.clone().transform(inverse) else {
            return;
        };
        let target = shift.pre_concat(self.state.transform);
        match stroke {
            Some(stroke) => {
                let Some(user_path) = prune_degenerate(&user_path) else {
                    return;
                };
                mask.stroke_path(&user_path, &paint, stroke, target, None);
            }
            None => {
                mask.fill_path(&user_path, &paint, rule, target, None);
            }
        }
        self.finish_shadow(mask, sigma, left, top, self.state.alpha);
    }

    fn finish_shadow(&mut self, mut mask: Pixmap, sigma: f32, left: f32, top: f32, opacity: f32) {
        Self::tint_shadow(&mut mask, self.state.shadow.rgba);
        blur_pixmap(&mut mask, sigma);
        let offset =
            Transform::from_translate(self.state.shadow_x + left, self.state.shadow_y + top);
        let shadow_paint = Paint {
            shader: tiny_skia::Pattern::new(
                mask.as_ref(),
                SpreadMode::Pad,
                FilterQuality::Nearest,
                opacity,
                offset,
            ),
            blend_mode: self.state.blend,
            anti_alias: true,
            ..Paint::default()
        };
        #[expect(
            clippy::cast_precision_loss,
            reason = "canvas sides are capped well below 2^24"
        )]
        let (canvas_width, canvas_height) = (self.width as f32, self.height as f32);
        let Some(cover) = tiny_skia::Rect::from_xywh(0.0, 0.0, canvas_width, canvas_height) else {
            return;
        };
        let cover = PathBuilder::from_rect(cover);
        let Some(pixmap) = &mut self.pixmap else {
            return;
        };
        pixmap.fill_path(
            &cover,
            &shadow_paint,
            FillRule::Winding,
            Transform::identity(),
            self.state.clip.as_deref(),
        );
    }

    fn shadow_mask_paint(&self, style: &Style) -> Option<Paint<'_>> {
        let mut paint = Paint {
            blend_mode: BlendMode::SourceOver,
            anti_alias: true,
            ..Paint::default()
        };
        paint.shader = match style {
            Style::Color(color) => {
                Shader::SolidColor(Color::from_rgba8(255, 255, 255, color.rgba[3]))
            }
            Style::Gradient(id) => self.gradient_shader_at(*id, 1.0, Transform::identity())?,
            Style::Pattern(id) => {
                let entry = self.patterns.get(id)?;
                tiny_skia::Pattern::new(
                    entry.image.as_ref().as_ref(),
                    entry.spread,
                    entry.quality,
                    1.0,
                    Transform::identity(),
                )
            }
        };
        Some(paint)
    }

    fn tint_shadow(mask: &mut Pixmap, [red, green, blue, alpha]: [u8; 4]) {
        let shadow_alpha = u32::from(alpha);
        for pixel in mask.data_mut().as_chunks_mut::<4>().0 {
            let coverage = u32::from(pixel[3]);
            let tinted = (coverage * shadow_alpha + 127) / 255;
            #[expect(
                clippy::cast_possible_truncation,
                reason = "channel products divided by 255 stay below 256"
            )]
            let (red, green, blue) = (
                (u32::from(red) * tinted / 255) as u8,
                (u32::from(green) * tinted / 255) as u8,
                (u32::from(blue) * tinted / 255) as u8,
            );
            #[expect(
                clippy::cast_possible_truncation,
                reason = "channel products divided by 255 stay below 256"
            )]
            let tinted = tinted as u8;
            pixel.copy_from_slice(&[red, green, blue, tinted]);
        }
    }

    fn stroke_options(&self) -> Stroke {
        let dash = if self.state.dash.is_empty() {
            None
        } else {
            StrokeDash::new(self.state.dash.clone(), self.state.dash_offset)
        };
        Stroke {
            width: self.state.line_width,
            miter_limit: self.state.miter_limit,
            line_cap: self.state.line_cap,
            line_join: self.state.line_join,
            dash,
        }
    }
}

impl Default for State {
    fn default() -> Self {
        Self {
            transform: Transform::identity(),
            fill: Style::Color(CanvasColor::new([0, 0, 0, 255])),
            stroke: Style::Color(CanvasColor::new([0, 0, 0, 255])),
            shadow: CanvasColor::new([0, 0, 0, 0]),
            alpha: 1.0,
            blend: BlendMode::SourceOver,
            line_width: 1.0,
            line_cap: LineCap::Butt,
            line_join: LineJoin::Miter,
            miter_limit: 10.0,
            dash: Vec::new(),
            dash_offset: 0.0,
            shadow_blur: 0.0,
            shadow_x: 0.0,
            shadow_y: 0.0,
            font: "10px sans-serif".to_owned(),
            text_align: TextAlign::Start,
            text_baseline: TextBaseline::Alphabetic,
            direction: Direction::Inherit,
            letter_spacing: "0px".to_owned(),
            word_spacing: "0px".to_owned(),
            font_kerning: "auto".to_owned(),
            font_stretch: "normal".to_owned(),
            font_variant_caps: "normal".to_owned(),
            text_rendering: "auto".to_owned(),
            lang: "inherit".to_owned(),
            smoothing: true,
            smoothing_quality: "low".to_owned(),
            filter: "none".to_owned(),
            filter_matrix: None,
            filter_object: None,
            clip: None,
        }
    }
}

#[derive(Clone)]
pub(crate) enum StyleValue {
    Color([u8; 4]),
    ColorExact([u8; 4], String),
    Gradient(u32),
    Pattern(u32),
}

impl StyleValue {
    fn from_style(style: &Style) -> Self {
        match style {
            Style::Color(color) => match &color.serialized {
                Some(text) => Self::ColorExact(color.rgba, text.clone()),
                None => Self::Color(color.rgba),
            },
            Style::Gradient(id) => Self::Gradient(*id),
            Style::Pattern(id) => Self::Pattern(*id),
        }
    }
}

fn make_pixmap(width: u32, height: u32) -> Option<Pixmap> {
    let width = width.max(1);
    let height = height.max(1);
    crate::js::world::canvas_bytes(width, height)?;
    Pixmap::new(width, height)
}

fn to_u8(value: f32) -> u8 {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the value is clamped to 0..=255 before the cast"
    )]
    let channel = value.mul_add(255.0, 0.5).clamp(0.0, 255.0) as u8;
    channel
}

fn same_float(left: f32, right: f32) -> bool {
    left.partial_cmp(&right) == Some(std::cmp::Ordering::Equal)
}

fn ellipse_point(cx: f32, cy: f32, rx: f32, ry: f32, rotation: f32, angle: f32) -> Point {
    let (sin, cos) = rotation.sin_cos();
    let x = rx * angle.cos();
    let y = ry * angle.sin();
    Point::from_xy(cx + x * cos - y * sin, cy + x * sin + y * cos)
}

fn ellipse_derivative(cx: f32, cy: f32, rx: f32, ry: f32, rotation: f32, angle: f32) -> Point {
    let _ = (cx, cy);
    let (sin, cos) = rotation.sin_cos();
    let x = -rx * angle.sin();
    let y = ry * angle.cos();
    Point::from_xy(x * cos - y * sin, x * sin + y * cos)
}

// https://html.spec.whatwg.org/multipage/canvas.html#dom-context-2d-roundrect
fn round_rect_radii(radii: &[(f32, f32)], width: f32, height: f32) -> [(f32, f32); 4] {
    let values: [(f32, f32); 4] = match radii {
        [] => [(0.0, 0.0); 4],
        [one] => [*one; 4],
        [first, second] => [*first, *second, *first, *second],
        [first, second, third] => [*first, *second, *third, *second],
        [first, second, third, fourth, ..] => [*first, *second, *third, *fourth],
    };
    let mut scale = 1.0f32;
    let sides = [
        (values[0].0 + values[1].0, width),
        (values[1].1 + values[2].1, height),
        (values[2].0 + values[3].0, width),
        (values[3].1 + values[0].1, height),
    ];
    for (sum, side) in sides {
        if sum > side && sum > 0.0 {
            scale = scale.min(side / sum);
        }
    }
    values.map(|(rx, ry)| (rx * scale, ry * scale))
}

// https://html.spec.whatwg.org/multipage/canvas.html#dom-context-2d-ispointinpath
fn path_contains(path: &Path, rule: FillRule, point: Point) -> bool {
    if !(point.x.is_finite() && point.y.is_finite()) {
        return false;
    }
    let mut crossings = 0_u32;
    let mut winding = 0_i32;
    let mut current = Point::zero();
    let mut subpath_start = Point::zero();
    let mut subpath_open = false;
    let mut on_boundary = false;
    let mut edge = |a: Point, b: Point| {
        if point_on_segment(point, a, b) {
            on_boundary = true;
        }
        if (a.y <= point.y && b.y > point.y) || (b.y <= point.y && a.y > point.y) {
            let t = (point.y - a.y) / (b.y - a.y);
            if a.x + t * (b.x - a.x) > point.x {
                crossings += 1;
                winding += if b.y > a.y { 1 } else { -1 };
            }
        }
    };
    for segment in path.segments() {
        match segment {
            tiny_skia::PathSegment::MoveTo(p) => {
                if subpath_open {
                    edge(current, subpath_start);
                }
                current = p;
                subpath_start = p;
                subpath_open = true;
            }
            tiny_skia::PathSegment::LineTo(p) => {
                edge(current, p);
                current = p;
            }
            tiny_skia::PathSegment::QuadTo(control, end) => {
                let mut flattened = Vec::new();
                flatten_quad(current, control, end, &mut flattened);
                for p in flattened {
                    edge(current, p);
                    current = p;
                }
            }
            tiny_skia::PathSegment::CubicTo(first, second, end) => {
                let mut flattened = Vec::new();
                flatten_cubic(current, first, second, end, &mut flattened);
                for p in flattened {
                    edge(current, p);
                    current = p;
                }
            }
            tiny_skia::PathSegment::Close => {
                edge(current, subpath_start);
                current = subpath_start;
                subpath_open = false;
            }
        }
    }
    if subpath_open {
        edge(current, subpath_start);
    }
    if on_boundary {
        return true;
    }
    match rule {
        FillRule::EvenOdd => crossings % 2 == 1,
        FillRule::Winding => winding != 0,
    }
}

const BOUNDARY_EPSILON: f32 = 1e-3;

fn point_on_segment(point: Point, a: Point, b: Point) -> bool {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let length_squared = dx * dx + dy * dy;
    if length_squared == 0.0 {
        return (point.x - a.x).abs() <= BOUNDARY_EPSILON
            && (point.y - a.y).abs() <= BOUNDARY_EPSILON;
    }
    let t = (((point.x - a.x) * dx + (point.y - a.y) * dy) / length_squared).clamp(0.0, 1.0);
    let closest_x = a.x + t * dx;
    let closest_y = a.y + t * dy;
    (point.x - closest_x).hypot(point.y - closest_y) <= BOUNDARY_EPSILON
}

fn flatten_quad(start: Point, control: Point, end: Point, out: &mut Vec<Point>) {
    flatten_quad_at(start, control, end, out, 0);
}

fn flatten_quad_at(start: Point, control: Point, end: Point, out: &mut Vec<Point>, depth: u32) {
    if out.len() >= MAX_FLATTEN_POINTS {
        return;
    }
    if depth >= 16 || point_line_distance(control, start, end) <= FLATTEN_TOLERANCE {
        out.push(end);
        return;
    }
    let first = midpoint(start, control);
    let second = midpoint(control, end);
    let middle = midpoint(first, second);
    flatten_quad_at(start, first, middle, out, depth + 1);
    flatten_quad_at(middle, second, end, out, depth + 1);
}

fn flatten_cubic(start: Point, first: Point, second: Point, end: Point, out: &mut Vec<Point>) {
    flatten_cubic_at(start, first, second, end, out, 0);
}

#[expect(
    clippy::many_single_char_names,
    reason = "the five points are the de Casteljau subdivision inputs"
)]
fn flatten_cubic_at(
    start: Point,
    first: Point,
    second: Point,
    end: Point,
    out: &mut Vec<Point>,
    depth: u32,
) {
    if out.len() >= MAX_FLATTEN_POINTS {
        return;
    }
    if depth >= 16
        || (point_line_distance(first, start, end) <= FLATTEN_TOLERANCE
            && point_line_distance(second, start, end) <= FLATTEN_TOLERANCE)
    {
        out.push(end);
        return;
    }
    let a = midpoint(start, first);
    let b = midpoint(first, second);
    let c = midpoint(second, end);
    let d = midpoint(a, b);
    let e = midpoint(b, c);
    let middle = midpoint(d, e);
    flatten_cubic_at(start, a, d, middle, out, depth + 1);
    flatten_cubic_at(middle, e, c, end, out, depth + 1);
}

fn transform_is_invertible(transform: Transform) -> bool {
    let determinant = transform.sx * transform.sy - transform.kx * transform.ky;
    determinant != 0.0 && determinant.is_finite()
}

fn midpoint(a: Point, b: Point) -> Point {
    Point::from_xy(a.x.midpoint(b.x), a.y.midpoint(b.y))
}

fn point_line_distance(point: Point, a: Point, b: Point) -> f32 {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let length = dx.hypot(dy);
    if length == 0.0 {
        return (point.x - a.x).hypot(point.y - a.y);
    }
    ((point.x - a.x) * dy - (point.y - a.y) * dx).abs() / length
}

const FLATTEN_TOLERANCE: f32 = 0.1;

const MAX_FLATTEN_POINTS: usize = 4096;

// https://html.spec.whatwg.org/multipage/canvas.html#dom-context-2d-stroke
fn prune_degenerate(path: &Path) -> Option<Path> {
    let mut builder = PathBuilder::new();
    let mut commands: Vec<tiny_skia::PathSegment> = Vec::new();
    let mut bounds: Option<(Point, Point)> = None;
    for segment in path.segments() {
        match segment {
            tiny_skia::PathSegment::MoveTo(point) => {
                flush_subpath(&mut builder, &mut commands, &mut bounds);
                extend_bounds(point, &mut bounds);
            }
            tiny_skia::PathSegment::LineTo(point) => extend_bounds(point, &mut bounds),
            tiny_skia::PathSegment::QuadTo(control, end) => {
                extend_bounds(control, &mut bounds);
                extend_bounds(end, &mut bounds);
            }
            tiny_skia::PathSegment::CubicTo(first, second, end) => {
                extend_bounds(first, &mut bounds);
                extend_bounds(second, &mut bounds);
                extend_bounds(end, &mut bounds);
            }
            tiny_skia::PathSegment::Close => {}
        }
        commands.push(segment);
    }
    flush_subpath(&mut builder, &mut commands, &mut bounds);
    builder.finish()
}

fn extend_bounds(point: Point, bounds: &mut Option<(Point, Point)>) {
    *bounds = Some(match *bounds {
        None => (point, point),
        Some((min, max)) => (
            Point::from_xy(min.x.min(point.x), min.y.min(point.y)),
            Point::from_xy(max.x.max(point.x), max.y.max(point.y)),
        ),
    });
}

fn flush_subpath(
    builder: &mut PathBuilder,
    commands: &mut Vec<tiny_skia::PathSegment>,
    bounds: &mut Option<(Point, Point)>,
) {
    let keep = bounds
        .take()
        .is_some_and(|(min, max)| (max.x - min.x).max(max.y - min.y) > 0.0);
    if keep {
        for command in commands.drain(..) {
            match command {
                tiny_skia::PathSegment::MoveTo(p) => builder.move_to(p.x, p.y),
                tiny_skia::PathSegment::LineTo(p) => builder.line_to(p.x, p.y),
                tiny_skia::PathSegment::QuadTo(c, p) => builder.quad_to(c.x, c.y, p.x, p.y),
                tiny_skia::PathSegment::CubicTo(a, b, p) => {
                    builder.cubic_to(a.x, a.y, b.x, b.y, p.x, p.y);
                }
                tiny_skia::PathSegment::Close => builder.close(),
            }
        }
    }
    commands.clear();
}

// https://html.spec.whatwg.org/multipage/canvas.html#dom-context-2d-setlinedash
fn dash_and_stroke(path: &Path, stroke: &Stroke, transform: Transform) -> Option<Path> {
    let scale = tiny_skia::PathStroker::compute_resolution_scale(&transform);
    let dashed;
    let input = if let Some(dash) = &stroke.dash {
        dashed = path.dash(dash, scale)?;
        &dashed
    } else {
        path
    };
    input.stroke(stroke, scale)
}

// https://html.spec.whatwg.org/multipage/canvas.html#dom-context-2d-globalcompositeoperation
fn clears_uncovered(blend: BlendMode) -> bool {
    matches!(
        blend,
        BlendMode::Source
            | BlendMode::SourceIn
            | BlendMode::SourceOut
            | BlendMode::DestinationIn
            | BlendMode::DestinationAtop
    )
}

fn to_f32<T: ToPrimitive>(value: T) -> f32 {
    value.to_f32().unwrap_or(0.0)
}

fn crop_clip(clip: &Mask, left: i32, top: i32, width: u32, height: u32) -> Option<Mask> {
    let mut out = Mask::new(width, height)?;
    let target_width = usize::try_from(width).ok()?;
    let source_width = usize::try_from(clip.width()).ok()?;
    let source_height = usize::try_from(clip.height()).ok()?;
    for y in 0..usize::try_from(height).ok()? {
        let source_y = top.checked_add(i32::try_from(y).ok()?)?;
        if source_y < 0 {
            continue;
        }
        let source_y = usize::try_from(source_y).ok()?;
        if source_y >= source_height {
            continue;
        }
        for x in 0..target_width {
            let source_x = left.checked_add(i32::try_from(x).ok()?)?;
            if source_x < 0 {
                continue;
            }
            let source_x = usize::try_from(source_x).ok()?;
            if source_x >= source_width {
                continue;
            }
            out.data_mut()[y * target_width + x] = clip.data()[source_y * source_width + source_x];
        }
    }
    Some(out)
}

fn to_premultiplied(straight: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(straight.len());
    for pixel in straight.as_chunks::<4>().0 {
        let color = ColorU8::from_rgba(pixel[0], pixel[1], pixel[2], pixel[3]).premultiply();
        out.extend_from_slice(&[color.red(), color.green(), color.blue(), color.alpha()]);
    }
    out
}

fn blend_from_name(name: &str) -> Option<BlendMode> {
    Some(match name {
        "source-over" => BlendMode::SourceOver,
        "source-in" => BlendMode::SourceIn,
        "source-out" => BlendMode::SourceOut,
        "source-atop" => BlendMode::SourceAtop,
        "destination-over" => BlendMode::DestinationOver,
        "destination-in" => BlendMode::DestinationIn,
        "destination-out" => BlendMode::DestinationOut,
        "destination-atop" => BlendMode::DestinationAtop,
        "lighter" => BlendMode::Plus,
        "copy" => BlendMode::Source,
        "clear" => BlendMode::Clear,
        "xor" => BlendMode::Xor,
        "multiply" => BlendMode::Multiply,
        "screen" => BlendMode::Screen,
        "overlay" => BlendMode::Overlay,
        "darken" => BlendMode::Darken,
        "lighten" => BlendMode::Lighten,
        "color-dodge" => BlendMode::ColorDodge,
        "color-burn" => BlendMode::ColorBurn,
        "hard-light" => BlendMode::HardLight,
        "soft-light" => BlendMode::SoftLight,
        "difference" => BlendMode::Difference,
        "exclusion" => BlendMode::Exclusion,
        "hue" => BlendMode::Hue,
        "saturation" => BlendMode::Saturation,
        "color" => BlendMode::Color,
        "luminosity" => BlendMode::Luminosity,
        _ => return None,
    })
}

fn blend_name(blend: BlendMode) -> &'static str {
    match blend {
        BlendMode::SourceOver => "source-over",
        BlendMode::SourceIn => "source-in",
        BlendMode::SourceOut => "source-out",
        BlendMode::SourceAtop => "source-atop",
        BlendMode::DestinationOver => "destination-over",
        BlendMode::DestinationIn => "destination-in",
        BlendMode::DestinationOut => "destination-out",
        BlendMode::DestinationAtop => "destination-atop",
        BlendMode::Plus => "lighter",
        BlendMode::Source => "copy",
        BlendMode::Clear => "clear",
        BlendMode::Xor => "xor",
        BlendMode::Multiply => "multiply",
        BlendMode::Screen => "screen",
        BlendMode::Overlay => "overlay",
        BlendMode::Darken => "darken",
        BlendMode::Lighten => "lighten",
        BlendMode::ColorDodge => "color-dodge",
        BlendMode::ColorBurn => "color-burn",
        BlendMode::HardLight => "hard-light",
        BlendMode::SoftLight => "soft-light",
        BlendMode::Difference => "difference",
        BlendMode::Exclusion => "exclusion",
        BlendMode::Hue => "hue",
        BlendMode::Saturation => "saturation",
        BlendMode::Color => "color",
        BlendMode::Luminosity => "luminosity",
        BlendMode::Destination => "destination",
        BlendMode::Modulate => "modulate",
    }
}

pub(crate) fn serialize_color([r, g, b, a]: [u8; 4]) -> String {
    if a == 255 {
        format!("#{r:02x}{g:02x}{b:02x}")
    } else {
        let alpha = f32::from(a) / 255.0;
        let alpha = if a == 128 {
            0.5
        } else {
            (alpha * 1000.0).round() / 1000.0
        };
        format!("rgba({r}, {g}, {b}, {alpha})")
    }
}

// https://html.spec.whatwg.org/multipage/canvas.html#dom-context-2d-shadowblur
fn blur_pixmap(pixmap: &mut Pixmap, sigma: f32) {
    if sigma <= 0.0 {
        return;
    }
    for radius in box_radii(sigma, 3) {
        box_blur(pixmap, radius);
    }
}

fn box_radii(sigma: f32, passes: usize) -> Vec<usize> {
    let passes = passes.max(1);
    #[expect(
        clippy::cast_precision_loss,
        reason = "the pass count is a small constant"
    )]
    let count = passes as f32;
    let ideal = (12.0 * sigma * sigma / count + 1.0).sqrt();
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the ideal box width is small for canvas blur radii"
    )]
    let mut lower = ideal.floor() as i32;
    if lower % 2 == 0 {
        lower -= 1;
    }
    let upper = lower + 2;
    #[expect(
        clippy::cast_precision_loss,
        reason = "the pass count is a small constant"
    )]
    let lower_f = lower as f32;
    let m =
        ((12.0 * sigma * sigma - count * lower_f * lower_f - 4.0 * count * lower_f - 3.0 * count)
            / (-4.0 * lower_f - 4.0))
            .round();
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the mixed-box count is bounded by the pass count"
    )]
    let mixed = m as i32;
    (0..passes)
        .map(|index| {
            let index = i32::try_from(index).unwrap_or(i32::MAX);
            let width = if index < mixed { lower } else { upper };
            usize::try_from((width.max(1) - 1) / 2).unwrap_or(0)
        })
        .collect()
}

fn box_blur(pixmap: &mut Pixmap, radius: usize) {
    if radius == 0 {
        return;
    }
    let width = pixmap.width() as usize;
    let height = pixmap.height() as usize;
    let data = pixmap.data_mut();
    box_blur_horizontal(data, width, height, radius);
    box_blur_vertical(data, width, height, radius);
}

fn box_blur_horizontal(data: &mut [u8], width: usize, height: usize, radius: usize) {
    let mut out = vec![0u8; data.len()];
    let window = u32::try_from(radius * 2 + 1).unwrap_or(u32::MAX);
    for y in 0..height {
        let row = y * width;
        let mut sums = [0u32; 4];
        for offset in 0..=(radius * 2) {
            let x = offset.saturating_sub(radius).min(width - 1);
            for (channel, sum) in sums.iter_mut().enumerate() {
                *sum += u32::from(data[(row + x) * 4 + channel]);
            }
        }
        for x in 0..width {
            for (channel, sum) in sums.iter().enumerate() {
                out[(row + x) * 4 + channel] = u8::try_from(sum / window).unwrap_or(255);
            }
            let leaving = x.saturating_sub(radius).min(width - 1);
            let entering = (x + radius + 1).min(width - 1);
            for (channel, sum) in sums.iter_mut().enumerate() {
                *sum = *sum - u32::from(data[(row + leaving) * 4 + channel])
                    + u32::from(data[(row + entering) * 4 + channel]);
            }
        }
    }
    data.copy_from_slice(&out);
}

fn box_blur_vertical(data: &mut [u8], width: usize, height: usize, radius: usize) {
    let mut out = vec![0u8; data.len()];
    let window = u32::try_from(radius * 2 + 1).unwrap_or(u32::MAX);
    for x in 0..width {
        let mut sums = [0u32; 4];
        for offset in 0..=(radius * 2) {
            let y = offset.saturating_sub(radius).min(height - 1);
            for (channel, sum) in sums.iter_mut().enumerate() {
                *sum += u32::from(data[(y * width + x) * 4 + channel]);
            }
        }
        for y in 0..height {
            for (channel, sum) in sums.iter().enumerate() {
                out[(y * width + x) * 4 + channel] = u8::try_from(sum / window).unwrap_or(255);
            }
            let leaving = y.saturating_sub(radius).min(height - 1);
            let entering = (y + radius + 1).min(height - 1);
            for (channel, sum) in sums.iter_mut().enumerate() {
                *sum = *sum - u32::from(data[(leaving * width + x) * 4 + channel])
                    + u32::from(data[(entering * width + x) * 4 + channel]);
            }
        }
    }
    data.copy_from_slice(&out);
}

fn encode_jpeg(
    pixels: &[u8],
    width: u32,
    height: u32,
    color: image::ExtendedColorType,
    quality: JpegQuality,
) -> Option<Vec<u8>> {
    let mut jpeg = Vec::new();
    image::ImageEncoder::write_image(
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, quality.0),
        pixels,
        width,
        height,
        color,
    )
    .ok()?;
    Some(jpeg)
}

// https://html.spec.whatwg.org/multipage/canvas.html#dom-canvas-todataurl
#[derive(Clone, Copy)]
pub(crate) struct JpegQuality(u8);

impl JpegQuality {
    pub(crate) fn new(quality: f64) -> Self {
        let scaled = (quality * 100.0).round().clamp(1.0, 100.0);
        Self(scaled.to_u8().unwrap_or(100))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_rect_paints_pixels() {
        let mut canvas = Canvas2D::new(4, 4);
        canvas.set_fill_style(StyleValue::Color([255, 0, 0, 255]));
        canvas.fill_rect(0.0, 0.0, 2.0, 2.0);
        let pixels = canvas.read_pixels(0, 0, 4, 4).unwrap();
        assert_eq!(&pixels[..4], &[255, 0, 0, 255]);
        assert_eq!(&pixels[12..16], &[0, 0, 0, 0]);
    }

    #[test]
    fn transform_applies_to_fill_rect() {
        let mut canvas = Canvas2D::new(4, 4);
        canvas.set_fill_style(StyleValue::Color([0, 0, 255, 255]));
        canvas.translate(2.0, 0.0);
        canvas.fill_rect(0.0, 0.0, 2.0, 2.0);
        let pixels = canvas.read_pixels(0, 0, 4, 4).unwrap();
        assert_eq!(&pixels[8..12], &[0, 0, 255, 255]);
    }

    #[test]
    fn clip_limits_fill() {
        let mut canvas = Canvas2D::new(4, 4);
        canvas.set_fill_style(StyleValue::Color([0, 255, 0, 255]));
        canvas.begin_path();
        canvas.rect(0.0, 0.0, 2.0, 4.0);
        canvas.clip(false);
        canvas.fill_rect(0.0, 0.0, 4.0, 4.0);
        let pixels = canvas.read_pixels(0, 0, 4, 4).unwrap();
        assert_eq!(&pixels[0..4], &[0, 255, 0, 255]);
        assert_eq!(&pixels[8..12], &[0, 0, 0, 0]);
    }

    #[test]
    fn is_point_in_path_ignores_the_current_transform() {
        let mut canvas = Canvas2D::new(100, 50);
        canvas.translate(50.0, 0.0);
        canvas.rect(0.0, 0.0, 20.0, 20.0);
        assert!(canvas.is_point_in_path(51.0, 10.0, false));
        assert!(!canvas.is_point_in_path(10.0, 10.0, false));
        assert!(!canvas.is_point_in_path(f32::NAN, 10.0, false));
    }

    #[test]
    fn open_subpath_is_closed_for_containment() {
        let mut canvas = Canvas2D::new(30, 30);
        canvas.begin_path();
        canvas.move_to(20.0, 0.0);
        canvas.line_to(0.0, 0.0);
        canvas.line_to(0.0, 20.0);
        assert!(canvas.is_point_in_path(5.0, 10.0, false));
    }

    #[test]
    fn round_rect_leaves_a_subpath_at_its_origin() {
        let mut canvas = Canvas2D::new(100, 50);
        canvas.round_rect(5.0, 6.0, 20.0, 10.0, &[(2.0, 2.0)]);
        let Some(Segment::MoveTo(origin)) = canvas.path.last() else {
            panic!("roundRect must leave a trailing move");
        };
        assert_eq!((origin.x, origin.y), (5.0, 6.0));

        let mut default_radii = Canvas2D::new(100, 50);
        default_radii.round_rect(1.0, 2.0, 20.0, 10.0, &[]);
    }

    #[test]
    fn arc_points_follow_the_transform() {
        let mut canvas = Canvas2D::new(100, 50);
        canvas.scale(2.0, 0.5);
        canvas.arc(25.0, 50.0, 56.0, 0.0, std::f32::consts::TAU, false);
        let Some(Segment::MoveTo(start)) = canvas.path.first() else {
            panic!("arc must move to its start point");
        };
        assert!((start.x - 162.0).abs() < 0.01, "x = {}", start.x);
        assert!((start.y - 25.0).abs() < 0.01, "y = {}", start.y);
    }

    #[test]
    fn arc_spans_cover_the_requested_sweep() {
        let cubics = |canvas: &Canvas2D| {
            canvas
                .path
                .iter()
                .filter(|segment| matches!(segment, Segment::Cubic(..)))
                .count()
        };
        let mut full = Canvas2D::new(4, 4);
        full.arc(2.0, 2.0, 1.0, 0.0, std::f32::consts::TAU, false);
        assert_eq!(cubics(&full), 8);

        let mut reversed = Canvas2D::new(4, 4);
        reversed.arc(2.0, 2.0, 1.0, 0.0, std::f32::consts::TAU, true);
        assert_eq!(cubics(&reversed), 8);

        let mut wrapped = Canvas2D::new(4, 4);
        wrapped.arc(2.0, 2.0, 1.0, 0.0, 2.5 * std::f32::consts::PI, true);
        assert_eq!(cubics(&wrapped), 6);
    }

    #[test]
    fn conic_gradient_rotates_stops() {
        let mut canvas = Canvas2D::new(100, 50);
        let id = canvas.create_conic_gradient(50.0, 25.0, 1.5 * std::f32::consts::PI);
        assert!(canvas.add_color_stop(id, 0.0, [255, 0, 0, 255]));
        assert!(canvas.add_color_stop(id, 0.25, [0, 255, 0, 255]));
        assert!(canvas.add_color_stop(id, 0.5, [0, 255, 0, 255]));
        assert!(canvas.add_color_stop(id, 0.75, [255, 0, 0, 255]));
        canvas.set_fill_style(StyleValue::Gradient(id));
        canvas.fill_rect(0.0, 0.0, 100.0, 50.0);
        let pixels = canvas.read_pixels(0, 0, 100, 50).unwrap();
        let pixel = |x: usize, y: usize| &pixels[(y * 100 + x) * 4..(y * 100 + x) * 4 + 4];
        assert_eq!(pixel(25, 15), &[255, 0, 0, 255]);
        assert_eq!(pixel(75, 40), &[0, 255, 0, 255]);
    }

    #[test]
    fn shadow_offset_paints_below_the_shape() {
        let mut canvas = Canvas2D::new(4, 4);
        canvas.set_fill_style(StyleValue::Color([255, 0, 0, 255]));
        canvas.set_shadow_color_exact([0, 255, 0, 255], None);
        canvas.set_shadow_offset_y(2.0);
        canvas.fill_rect(0.0, 0.0, 4.0, 2.0);
        let pixels = canvas.read_pixels(0, 0, 4, 4).unwrap();
        assert_eq!(&pixels[..4], &[255, 0, 0, 255]);
        assert_eq!(&pixels[32..36], &[0, 255, 0, 255]);
    }

    #[test]
    fn cross_origin_image_taints_the_canvas() {
        let mut canvas = Canvas2D::new(1, 1);
        canvas.draw_image(
            &SourceImage {
                width: 1,
                height: 1,
                pixels: vec![255, 0, 0, 255].into(),
                origin_clean: false,
            },
            Rect::new(0.0, 0.0, 1.0, 1.0),
            Rect::new(0.0, 0.0, 1.0, 1.0),
        );
        assert!(!canvas.origin_clean());
    }

    #[test]
    fn full_turn_from_2pi_to_zero_is_a_full_ellipse() {
        let mut canvas = Canvas2D::new(100, 50);
        canvas.set_line_dash(vec![10.0, 21.4159]);
        canvas.scale(20.0, 20.0);
        canvas.ellipse(6.0, 10.0, 5.0, 5.0, 0.0, std::f32::consts::TAU, 0.0, false);
        canvas.stroke();
        assert!(canvas.is_point_in_path(120.0, 200.0, false));
        assert!(canvas.is_point_in_stroke(220.0, 200.0));
        assert!(canvas.is_point_in_stroke(174.0, 284.2));
        assert!(canvas.is_point_in_stroke(82.0, 292.6));
        assert!(!canvas.is_point_in_stroke(74.8, 289.2));
    }

    #[test]
    fn arc_to_takes_the_short_arc_between_tangent_points() {
        let mut canvas = Canvas2D::new(30, 30);
        canvas.set_stroke_style(StyleValue::Color([0, 255, 0, 255]));
        canvas.set_line_width(2.0);
        canvas.begin_path();
        canvas.move_to(10.0, 0.0);
        canvas.arc_to(20.0, 0.0, 20.0, 10.0, 10.0);
        canvas.stroke();
        let pixels = canvas.read_pixels(0, 0, 30, 30).unwrap();
        let alpha = |x: usize, y: usize| pixels[(y * 30 + x) * 4 + 3];
        assert!(alpha(17, 3) > 0, "short arc midpoint is empty");
        assert_eq!(alpha(3, 17), 0, "arc took the long way around");
    }

    #[test]
    fn blank_encoders_emit_their_containers() {
        let png = Canvas2D::blank_png(2, 2).expect("blank PNG");
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        let jpeg = Canvas2D::blank_jpeg(2, 2, JpegQuality::new(0.92)).expect("blank JPEG");
        assert_eq!(&jpeg[..2], &[0xff, 0xd8]);
    }

    #[test]
    fn jpeg_encodes_half_transparent_red_over_black() {
        let mut canvas = Canvas2D::new(2, 2);
        canvas.set_fill_style(StyleValue::Color([255, 0, 0, 128]));
        canvas.fill_rect(0.0, 0.0, 2.0, 2.0);
        let jpeg = canvas.to_jpeg(JpegQuality::new(0.92)).expect("JPEG");
        let decoded = image::load_from_memory_with_format(&jpeg, image::ImageFormat::Jpeg)
            .expect("decode the encoded JPEG")
            .to_rgb8();
        let [red, green, blue] = decoded.get_pixel(0, 0).0;
        assert!(
            red.abs_diff(128) <= 3,
            "red = {red}, a dropped alpha is 255"
        );
        assert!(green <= 3, "green = {green}, a composite over white is 127");
        assert!(blue <= 3, "blue = {blue}");
    }

    #[test]
    fn layer_composites_with_the_captured_alpha_and_shadow() {
        let mut canvas = Canvas2D::new(8, 8);
        canvas.set_global_alpha(0.5);
        canvas.set_shadow_color_exact([0, 255, 0, 255], None);
        canvas.set_shadow_offset_y(4.0);
        canvas.begin_layer();
        canvas.set_global_alpha(1.0);
        canvas.set_fill_style(StyleValue::Color([255, 0, 0, 255]));
        canvas.fill_rect(0.0, 0.0, 8.0, 2.0);
        assert!(canvas.end_layer());
        let pixels = canvas.read_pixels(0, 0, 8, 8).unwrap();
        let pixel = |x: usize, y: usize| &pixels[(y * 8 + x) * 4..(y * 8 + x) * 4 + 4];
        assert_eq!(pixel(0, 0), &[255, 0, 0, 128]);
        assert_eq!(pixel(0, 5), &[0, 255, 0, 128]);
    }

    #[test]
    fn a_filtered_draw_stays_inside_its_device_rect() {
        let mut canvas = Canvas2D::new(32, 32);
        canvas.set_fill_style(StyleValue::Color([255, 0, 0, 255]));
        canvas.set_filter_matrix(Some(ColorMatrix::IDENTITY));
        canvas.fill_rect(8.0, 8.0, 4.0, 4.0);
        let pixels = canvas.read_pixels(0, 0, 32, 32).unwrap();
        let alpha = |x: usize, y: usize| pixels[(y * 32 + x) * 4 + 3];
        assert_eq!(alpha(10, 10), 255, "the filtered shape moved");
        assert_eq!(alpha(0, 0), 0, "the filtered layer leaked into the canvas");
        assert_eq!(
            alpha(20, 20),
            0,
            "the filtered layer leaked into the canvas"
        );
    }
}
