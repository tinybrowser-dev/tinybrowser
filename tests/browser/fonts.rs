//! `document.fonts.ready` against a CSS-connected `@font-face` whose fetch the
//! engine owns: the promise must not resolve while the face's response is
//! still in flight, a face whose source fails must settle, and a sheet added
//! after every notification must still settle its faces
//! (<https://drafts.csswg.org/css-font-loading/#dom-fontfaceset-ready>).

use crate::fixture::{TestServer, evaluate, ok, open_page, poll_until};

const PAGE: &str = "<!doctype html><span id=probe>XXXX</span>";
const INJECT: &str = "window.__ready = null;\
const s = document.createElement('style');\
s.textContent = \"@font-face { font-family: held; src: url('/ahem.ttf'); }\
@font-face { font-family: gone; src: url('/missing.ttf'); } #probe { font: 50px held; }\";\
document.head.appendChild(s);\
document.body.offsetWidth;\
document.fonts.ready.then(() => { window.__ready = JSON.stringify({\
  width: document.getElementById('probe').getBoundingClientRect().width,\
  faces: [...document.fonts].map(f => f.family + ':' + f.status).sort() }); });\
1";

#[test]
fn fonts_ready_waits_for_the_engine_fetch_of_a_css_face() {
    let ahem = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/third_party/wpt/fonts/Ahem.ttf"
    ))
    .expect("ahem");
    let ahem: &'static [u8] = Box::leak(ahem.into_boxed_slice());
    let server = TestServer::start(
        vec![
            ("/start", ok("text/html", PAGE.as_bytes())),
            ("/ahem.ttf", ok("font/ttf", ahem)),
        ],
        Some("/ahem.ttf"),
    );
    let (_fixture, mut client, session) = open_page(
        "tinybrowser-fonts-ready",
        &format!("http://{}/start", server.address()),
    );
    evaluate(&mut client, &session, INJECT);
    server.wait("/ahem.ttf");
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert_eq!(
        evaluate(
            &mut client,
            &session,
            "[window.__ready, document.fonts.status]"
        ),
        serde_json::json!([null, "loading"]),
        "ready must not resolve while the CSS face's fetch is in flight"
    );
    server.release();
    let answer = poll_until("document.fonts.ready", || {
        evaluate(&mut client, &session, "window.__ready")
            .as_str()
            .map(str::to_owned)
    });
    assert_eq!(
        answer, r#"{"width":200,"faces":["gone:error","held:loaded"]}"#,
        "ready resolves after the face is registered, and a failed source settles too"
    );

    // A sheet added after the fetch settled has no notification left to wait
    // for, so its face must still settle.
    let late = evaluate(
        &mut client,
        &session,
        "new Promise(done => { const s = document.createElement('style');\
         s.textContent = \"@font-face { font-family: late; src: url('/ahem.ttf'); }\";\
         document.head.appendChild(s);\
         document.fonts.ready.then(() => done([...document.fonts].filter(f => f.family === 'late').map(f => f.status))); })",
    );
    assert_eq!(late, serde_json::json!(["loaded"]));
}
