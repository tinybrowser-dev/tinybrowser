mod blitz;
mod png;
mod providers;

pub(crate) use blitz::{
    INVALID_BASE_URL, MAX_VIEWPORT_SIDE, blitz_base_url, paint, resolve_until_settled,
};
pub(crate) use png::encode_screenshot;
pub(crate) use providers::{BlitzFetch, CountingHandler, TinyNetProvider, TinyShell};

// https://html.spec.whatwg.org/multipage/rendering.html#images
const DEFAULT_OBJECT_WIDTH: u32 = 300;
const DEFAULT_OBJECT_HEIGHT: u32 = 150;

// https://svgwg.org/svg2-draft/coords.html#SizingSVGInCSS — viewBox-only SVG without width/height reports the 300x150 default natural size, matching Chromium
pub(crate) fn svg_natural_size(svg: &blitz_dom::node::SvgImageData) -> (u32, u32) {
    let dims = &svg.intrinsic_dimensions;
    if let (Some(width), Some(height)) = (dims.width, dims.height)
        && let (Some(width), Some(height)) = (absolute_px(width), absolute_px(height))
    {
        return (width, height);
    }
    (DEFAULT_OBJECT_WIDTH, DEFAULT_OBJECT_HEIGHT)
}

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RgbaImage {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl RgbaImage {
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RenderError {
    InvalidViewport,
    TooLarge,
    Encode,
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
