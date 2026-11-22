use image::ImageEncoder;
use png::{BitDepth, ColorType, Encoder};

use crate::protocol::ScreenshotFormat;
use crate::render::{RenderError, RgbaImage};

pub fn encode_screenshot(
    image: &RgbaImage,
    format: ScreenshotFormat,
) -> Result<Vec<u8>, RenderError> {
    match format {
        ScreenshotFormat::Png => encode_png(image),
        ScreenshotFormat::Jpeg { quality } => encode_jpeg(image, quality),
        ScreenshotFormat::Webp => encode_webp(image),
    }
}

pub fn encode_png(image: &RgbaImage) -> Result<Vec<u8>, RenderError> {
    let mut out = Vec::new();
    {
        let mut encoder = Encoder::new(&mut out, image.width, image.height);
        encoder.set_color(ColorType::Rgba);
        encoder.set_depth(BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|_| RenderError::Encode)?;
        writer
            .write_image_data(&image.data)
            .map_err(|_| RenderError::Encode)?;
    }
    Ok(out)
}

fn encode_jpeg(image: &RgbaImage, quality: u8) -> Result<Vec<u8>, RenderError> {
    let mut rgb = Vec::with_capacity((image.width * image.height * 3) as usize);
    for px in image.data.as_chunks::<4>().0 {
        let a = u16::from(px[3]);
        let inv = 255 - a;
        let blend = |c: u8| -> u8 {
            let v = (u16::from(c) * a + 255 * inv + 127) / 255;
            u8::try_from(v).unwrap_or(255)
        };
        rgb.push(blend(px[0]));
        rgb.push(blend(px[1]));
        rgb.push(blend(px[2]));
    }
    let mut out = Vec::new();
    let encoder =
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality.clamp(1, 100));
    encoder
        .write_image(
            &rgb,
            image.width,
            image.height,
            image::ExtendedColorType::Rgb8,
        )
        .map_err(|_| RenderError::Encode)?;
    Ok(out)
}

fn encode_webp(image: &RgbaImage) -> Result<Vec<u8>, RenderError> {
    let mut out = Vec::new();
    let encoder = image::codecs::webp::WebPEncoder::new_lossless(&mut out);
    encoder
        .write_image(
            &image.data,
            image.width,
            image.height,
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|_| RenderError::Encode)?;
    Ok(out)
}
