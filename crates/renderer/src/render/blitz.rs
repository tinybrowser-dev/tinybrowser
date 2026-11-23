#[cfg(test)]
use crate::render::providers::TinyNetProvider;
use crate::render::{RenderError, RgbaImage};

pub(crate) const MAX_VIEWPORT_SIDE: u32 = 4096;

pub(crate) const INVALID_BASE_URL: &str = "http://invalid/";

pub(crate) fn blitz_base_url(url: &url::Url) -> String {
    if url.cannot_be_a_base() {
        INVALID_BASE_URL.to_owned()
    } else {
        url.as_str().to_owned()
    }
}

pub(crate) fn resolve_until_settled(base: &mut blitz_dom::BaseDocument) -> u32 {
    let mut passes = 0;
    for _ in 0..8 {
        base.handle_messages();
        if base.has_pending_critical_resources() {
            break;
        }
        let first_animation_sample = passes == 0 && base.is_animating();
        if !base.has_changes() && !first_animation_sample {
            break;
        }
        base.resolve(crate::animation_seconds());
        passes += 1;
        if !base.has_changes() {
            break;
        }
    }
    passes
}

#[cfg(test)]
pub(crate) fn render_html(html: &str, width: u32, height: u32) -> Result<RgbaImage, RenderError> {
    let viewport = blitz_traits::shell::Viewport::new(
        width,
        height,
        1.0,
        blitz_traits::shell::ColorScheme::Light,
    );
    let config = blitz_dom::DocumentConfig {
        viewport: Some(viewport),
        ..blitz_dom::DocumentConfig::default()
    };
    let document = blitz_html::HtmlDocument::from_html(html, config);
    let mut base: blitz_dom::BaseDocument = document.into();
    base.resolve(crate::animation_seconds());
    paint(&mut base, width, height, false)
}

#[cfg(test)]
pub(crate) fn resolve_frame(base: &mut blitz_dom::BaseDocument, net: &TinyNetProvider) -> bool {
    resolve_until_settled(base);
    !base.has_pending_critical_resources() && net.in_flight() == 0
}

pub(crate) fn paint(
    base: &mut blitz_dom::BaseDocument,
    width: u32,
    height: u32,
    omit_background: bool,
) -> Result<RgbaImage, RenderError> {
    check_dims(width, height)?;
    let mut painter = anyrender_tiny_skia::TinySkiaScenePainter::new(width, height);
    blitz_paint::paint_scene(&mut painter, base, 1.0, width, height, 0, 0);
    let pixmap = painter.pixmap();
    let pixels = pixmap.data();
    let mut data = Vec::with_capacity(pixels.len());
    for pixel in pixels.as_chunks::<4>().0 {
        let [r, g, b, a] = [pixel[0], pixel[1], pixel[2], pixel[3]];
        let rgba = if omit_background {
            unpremultiply(r, g, b, a)
        } else {
            over_white(r, g, b, a)
        };
        data.extend_from_slice(&rgba);
    }
    Ok(RgbaImage {
        width,
        height,
        data,
    })
}

fn check_dims(width: u32, height: u32) -> Result<(), RenderError> {
    if width == 0 || height == 0 {
        return Err(RenderError::InvalidViewport);
    }
    if width > MAX_VIEWPORT_SIDE || height > MAX_VIEWPORT_SIDE {
        return Err(RenderError::TooLarge);
    }
    Ok(())
}

fn over_white(red: u8, green: u8, blue: u8, alpha: u8) -> [u8; 4] {
    let blend = |component: u8| component.saturating_add(u8::MAX - alpha);
    [blend(red), blend(green), blend(blue), u8::MAX]
}

fn unpremultiply(red: u8, green: u8, blue: u8, alpha: u8) -> [u8; 4] {
    if alpha == 0 || alpha == u8::MAX {
        return [red, green, blue, alpha];
    }
    let scale = |component: u8| -> u8 {
        let v = (u16::from(component) * 255 + u16::from(alpha) / 2) / u16::from(alpha);
        u8::try_from(v).unwrap_or(255)
    };
    [scale(red), scale(green), scale(blue), alpha]
}

#[cfg(test)]
mod tests {
    use super::{paint, render_html, resolve_frame};
    use crate::render::providers::{TinyNetProvider, TinyShell};
    use blitz_traits::net::NetHandler;
    use std::io::{Read, Write};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    #[test]
    fn linear_gradient_paints_endpoints() {
        let image = render_html(
            "<!doctype html><html><body style=\"margin:0\"><div style=\"width:200px;height:120px;background:linear-gradient(red,blue)\"></div></body></html>",
            200,
            120,
        )
        .expect("blitz render");
        assert_eq!((image.width, image.height), (200, 120));
        let top = &image.data[0..4];
        let bottom_start = (119 * 200 * 4) as usize;
        let bottom = &image.data[bottom_start..bottom_start + 4];
        assert!(
            top[0] > 200 && top[3] == 255,
            "gradient starts red, got {top:?}"
        );
        assert!(
            bottom[2] > 200 && bottom[3] == 255,
            "gradient ends blue, got {bottom:?}"
        );
    }

    #[test]
    fn external_stylesheet_applies_before_paint() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let body = "div{background:rgb(0,0,255)}";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/css\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
        });
        let (tx, rx) = std::sync::mpsc::channel();
        let (settle_tx, _settle_rx) = std::sync::mpsc::channel();
        let net = Arc::new(TinyNetProvider::new(tx, settle_tx));
        let viewport = blitz_traits::shell::Viewport::new(
            200,
            120,
            1.0,
            blitz_traits::shell::ColorScheme::Light,
        );
        let config = blitz_dom::DocumentConfig {
            viewport: Some(viewport),
            net_provider: Some(net.clone()),
            shell_provider: Some(Arc::new(TinyShell::new())),
            ..blitz_dom::DocumentConfig::default()
        };
        let html = format!(
            "<!doctype html><html><head><link rel=stylesheet href=\"http://127.0.0.1:{port}/a.css\"></head><body style=\"margin:0\"><div style=\"width:200px;height:120px\"></div></body></html>"
        );
        let document = blitz_html::HtmlDocument::from_html(&html, config);
        let mut base: blitz_dom::BaseDocument = document.into();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            base.resolve(crate::animation_seconds());
            base.handle_messages();
            while let Ok((fetch, handler)) = rx.try_recv() {
                let mut stream =
                    std::net::TcpStream::connect(("127.0.0.1", port)).expect("css server");
                let request = format!(
                    "GET {} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
                    fetch.request.url.path()
                );
                stream.write_all(request.as_bytes()).expect("css request");
                let mut response = Vec::new();
                stream.read_to_end(&mut response).expect("css body");
                let body = response
                    .windows(4)
                    .position(|window| window == b"\r\n\r\n")
                    .map_or_default(|index| response[index + 4..].to_vec());
                Box::new(handler).bytes(
                    fetch.request.url.as_str().to_owned(),
                    blitz_traits::net::Bytes::from(body),
                );
            }
            base.handle_messages();
            if resolve_frame(&mut base, &net) || Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let image = paint(&mut base, 200, 120, false).expect("blitz render");
        let middle = &image.data[((60 * 200 + 100) * 4) as usize..][..4];
        assert!(
            middle[2] > 200 && middle[3] == 255,
            "linked stylesheet paints blue, got {middle:?}"
        );
    }
}
