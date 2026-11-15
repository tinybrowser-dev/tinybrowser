//! Screenshots through Blitz: resolve, paint, encode.

mod blitz;
mod png;
mod providers;

pub(crate) use blitz::{
    INVALID_BASE_URL, MAX_VIEWPORT_SIDE, blitz_base_url, paint, resolve_until_settled,
};
pub(crate) use png::encode_png;
pub(crate) use providers::{BlitzFetch, CountingHandler, TinyNetProvider, TinyShell};

/// Default object size for an `<img>` without intrinsic dimensions
/// (<https://html.spec.whatwg.org/multipage/rendering.html#images>).
const DEFAULT_OBJECT_WIDTH: u32 = 300;
const DEFAULT_OBJECT_HEIGHT: u32 = 150;

/// Natural size of a decoded SVG image: the declared absolute `width` and
/// `height` when both resolve to CSS pixels, otherwise the default object
/// size. Chromium reports the default size even for a `viewBox`-only SVG
/// (probed 2026-10-06: `viewBox="0 0 20 10"` with no width/height answers
/// 300x150), so the viewBox only sizes paint, never
/// `naturalWidth`/`naturalHeight`.
pub(crate) fn svg_natural_size(svg: &blitz_dom::node::SvgImageData) -> (u32, u32) {
    let dims = &svg.intrinsic_dimensions;
    if let (Some(width), Some(height)) = (dims.width, dims.height)
        && let (Some(width), Some(height)) = (absolute_px(width), absolute_px(height))
    {
        return (width, height);
    }
    (DEFAULT_OBJECT_WIDTH, DEFAULT_OBJECT_HEIGHT)
}

/// `length` in CSS pixels, if it is an absolute SVG length. Relative units
/// (`em`, `ex`, `%`) have no meaning without layout context.
fn absolute_px(length: svgtypes::Length) -> Option<u32> {
    use svgtypes::LengthUnit;
    let px = match length.unit {
        LengthUnit::None | LengthUnit::Px => length.number,
        LengthUnit::In => length.number * 96.0,
        LengthUnit::Cm => length.number * 96.0 / 2.54,
        LengthUnit::Mm => length.number * 96.0 / 25.4,
        LengthUnit::Pt => length.number * 96.0 / 72.0,
        LengthUnit::Pc => length.number * 16.0,
        LengthUnit::Em | LengthUnit::Ex | LengthUnit::Percent => return None,
    };
    if !px.is_finite() || px <= 0.0 {
        return None;
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "finite and positive; `as` saturates at u32::MAX instead of wrapping"
    )]
    Some(px.floor() as u32)
}

/// One rendered viewport, 8 bits per channel, straight (non-premultiplied)
/// alpha, row-major RGBA.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RgbaImage {
    /// Width in device pixels.
    pub width: u32,
    /// Height in device pixels.
    pub height: u32,
    /// `width * height * 4` bytes, RGBA order.
    pub data: Vec<u8>,
}

impl RgbaImage {
    /// Crops to the device-pixel rectangle `(x, y, width, height)`, clamped
    /// to the image bounds.
    ///
    /// # Errors
    ///
    /// [`RenderError::InvalidCrop`] when the rectangle has no area after
    /// clamping.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        reason = "coordinates are clamped to the capped image bounds, well below u32"
    )]
    pub(crate) fn crop(
        &self,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    ) -> Result<Self, RenderError> {
        let image_width = self.width as f32;
        let image_height = self.height as f32;
        let left = x.max(0.0).min(image_width).floor();
        let top = y.max(0.0).min(image_height).floor();
        let right = (x + width).max(left).min(image_width).ceil();
        let bottom = (y + height).max(top).min(image_height).ceil();
        let crop_width = (right - left) as u32;
        let crop_height = (bottom - top) as u32;
        if crop_width == 0 || crop_height == 0 {
            return Err(RenderError::InvalidCrop);
        }
        let left = left as u32;
        let top = top as u32;
        let mut data = Vec::with_capacity((crop_width * crop_height * 4) as usize);
        for row in 0..crop_height {
            let start = (((top + row) * self.width + left) * 4) as usize;
            let end = start + (crop_width * 4) as usize;
            data.extend_from_slice(&self.data[start..end]);
        }
        Ok(Self {
            width: crop_width,
            height: crop_height,
            data,
        })
    }
}

/// One laid-out box in CSS pixels, keyed by its DOM element.
#[derive(Clone, Copy, Debug)]
pub struct NodeBox {
    /// The element the box was generated for.
    pub node: crate::dom::tree::NodeId,
    /// Border box in CSS pixels.
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// A render failure that is the document's fault rather than the caller's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RenderError {
    /// A zero-sized screenshot viewport was requested.
    InvalidViewport,
    /// The output image would exceed the renderer's size cap.
    TooLarge,
    /// The PNG encoder rejected the rendered image.
    Encode,
    /// A crop rectangle had no area inside the image.
    InvalidCrop,
}

impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidViewport => f.write_str("invalid viewport"),
            Self::TooLarge => f.write_str("render output exceeds the size cap"),
            Self::Encode => f.write_str("png encoding failed"),
            Self::InvalidCrop => f.write_str("crop rectangle is empty"),
        }
    }
}

impl std::error::Error for RenderError {}
