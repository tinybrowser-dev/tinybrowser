use std::time::Duration;

use serde_json::json;

use crate::fixture::{PIXEL, TestServer, evaluate, ok, open_page, redirect, server};

const PROBE: &str = r"(async () => {
  const settle = (id) => new Promise((resolve, reject) => {
    const image = document.getElementById(id);
    if (image.complete) {
      resolve();
      return;
    }
    const timer = setTimeout(() => reject(new Error(`${id} did not settle`)), 5000);
    image.addEventListener('load', () => { clearTimeout(timer); resolve(); }, { once: true });
    image.addEventListener('error', () => { clearTimeout(timer); reject(new Error(`${id} failed`)); }, { once: true });
  });
  await Promise.all(['redirected', 'roundtrip', 'same'].map(settle));
  const probe = (id) => {
    const canvas = document.createElement('canvas');
    canvas.width = 1;
    canvas.height = 1;
    const context = canvas.getContext('2d');
    context.drawImage(document.getElementById(id), 0, 0);
    try {
      context.getImageData(0, 0, 1, 1);
      return 'clean';
    } catch (error) {
      return error.name;
    }
  };
  return { redirected: probe('redirected'), roundtrip: probe('roundtrip'), same: probe('same') };
})()";

#[test]
fn image_origin_follows_the_response_taint() {
    let (cross_listener, cross) = server();
    let (page_listener, page) = server();
    TestServer::serve_on(
        cross_listener,
        vec![
            ("/pixel.png", ok("image/png", PIXEL)),
            ("/bounce.png", redirect(&format!("http://{page}/pixel.png"))),
        ],
    );
    let body = format!(
        "<!doctype html>\
         <img id=redirected src=\"http://{page}/redirect.png\">\
         <img id=roundtrip src=\"http://{page}/bounce.png\">\
         <img id=same src=\"http://{page}/pixel.png\">"
    );
    TestServer::serve_on(
        page_listener,
        vec![
            ("/", ok("text/html", body.as_bytes())),
            ("/pixel.png", ok("image/png", PIXEL)),
            (
                "/redirect.png",
                redirect(&format!("http://{cross}/pixel.png")),
            ),
            (
                "/bounce.png",
                redirect(&format!("http://{cross}/bounce.png")),
            ),
        ],
    );

    let (_fixture, mut client, session) =
        open_page("tinybrowser-canvas-taint", &format!("http://{page}/"));
    let result = evaluate(&mut client, &session, PROBE);
    assert_eq!(result["redirected"], json!("SecurityError"));
    assert_eq!(result["roundtrip"], json!("SecurityError"));
    assert_eq!(result["same"], json!("clean"));
}

#[test]
fn src_change_draws_nothing_until_the_new_request_settles() {
    let (slow_listener, slow) = server();
    let (page_listener, page) = server();
    let pixel = PIXEL.to_vec();
    TestServer::serve_on_after(
        slow_listener,
        vec![("/slow.png", ok("image/png", &pixel))],
        Duration::from_millis(300),
    );
    let body = format!(
        "<!doctype html>\
         <img id=swap data-slow=\"http://{slow}/slow.png\" src=\"http://{page}/pixel.png\">"
    );
    TestServer::serve_on(
        page_listener,
        vec![
            ("/", ok("text/html", body.as_bytes())),
            ("/pixel.png", ok("image/png", PIXEL)),
        ],
    );

    let (_fixture, mut client, session) =
        open_page("tinybrowser-canvas-swap", &format!("http://{page}/"));
    let result = evaluate(
        &mut client,
        &session,
        r"(async () => {
          try {
            const image = document.getElementById('swap');
            const probe = () => {
              const canvas = document.createElement('canvas');
              canvas.width = 1;
              canvas.height = 1;
              const context = canvas.getContext('2d');
              context.fillStyle = '#0f0';
              context.fillRect(0, 0, 1, 1);
              context.drawImage(image, 0, 0);
              try {
                return { error: null, red: context.getImageData(0, 0, 1, 1).data[0] };
              } catch (error) {
                return { error: error.name };
              }
            };
            const before = probe();
            image.src = image.dataset.slow;
            await new Promise((resolve) => setTimeout(resolve, 100));
            const during = probe();
            await new Promise((resolve, reject) => {
              const timer = setTimeout(() => reject(new Error('swap did not settle')), 5000);
              image.addEventListener('load', () => { clearTimeout(timer); resolve(); }, { once: true });
              image.addEventListener('error', () => { clearTimeout(timer); reject(new Error('swap failed')); }, { once: true });
            });
            return { before, during, after: probe() };
          } catch (error) {
            return { error: String((error && error.message) || error) };
          }
        })()",
    );
    // https://html.spec.whatwg.org/multipage/canvas.html#check-the-usability-of-the-image-argument
    assert_eq!(result["before"]["error"], json!(null), "{result}");
    assert!(
        result["before"]["red"].as_u64().unwrap_or(0) > 0,
        "{result}"
    );
    assert_eq!(result["during"]["error"], json!(null), "{result}");
    assert_eq!(result["during"]["red"], json!(0), "{result}");
    assert_eq!(result["after"]["error"], json!("SecurityError"), "{result}");
}

#[test]
fn blob_encoders_name_and_encode_the_settled_type() {
    let (page_listener, page) = server();
    TestServer::serve_on(
        page_listener,
        vec![(
            "/",
            ok(
                "text/html",
                b"<!doctype html><canvas id=canvas width=4 height=4></canvas>",
            ),
        )],
    );

    let (_fixture, mut client, session) =
        open_page("tinybrowser-canvas-blob", &format!("http://{page}/"));
    let result = evaluate(
        &mut client,
        &session,
        r"(async () => {
          const canvas = document.getElementById('canvas');
          const context = canvas.getContext('2d');
          context.fillStyle = '#f00';
          context.fillRect(0, 0, 4, 4);
          let conversions = 0;
          const type = { toString() { conversions += 1; return 'IMAGE/JPEG'; } };
          const blob = await new Promise((resolve) => canvas.toBlob(resolve, type));

          const offscreen = new OffscreenCanvas(2, 2);
          offscreen.getContext('2d').fillRect(0, 0, 2, 2);
          let offscreenConversions = 0;
          const offscreenBlob = await offscreen.convertToBlob({
            type: { toString() { offscreenConversions += 1; return 'IMAGE/JPEG'; } },
          });

          return {
            conversions,
            offscreenConversions,
            type: blob === null ? null : blob.type,
            size: blob === null ? 0 : blob.size,
            offscreenType: offscreenBlob.type,
            dataUrl: canvas.toDataURL('image/jpeg').slice(0, 23),
          };
        })()",
    );
    assert_eq!(result["conversions"], json!(1));
    assert_eq!(result["offscreenConversions"], json!(1));
    assert_eq!(result["type"], json!("image/jpeg"));
    assert!(result["size"].as_u64().unwrap_or(0) > 0);
    assert_eq!(result["offscreenType"], json!("image/jpeg"));
    assert_eq!(result["dataUrl"], json!("data:image/jpeg;base64,"));
}
