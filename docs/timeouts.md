# WPT timeouts

A WPT timeout is a failure and a work item (`AGENTS.md`). This is the work list
for the ones that are *unexpected* — the tests that hang instead of reporting a
verdict. Measured on the 2026-10-18 run (`39089218986`, 20 chunks,
`--exclude=worker`, testharness + crashtest + reftest + test262).

## What a timeout costs

119 002 results, 63 362 s of test time, **30 935 s (49%) inside 5 415 unexpected
TIMEOUTs**. Every one is `expected: OK`/`PASS`, so none is a gap metadata already
admits. At 4 processes × 20 chunks that is ~6.5 min of every chunk's ~14 min, and
the chunk phase is 23m33s of the 31m21s CI run.

The budgets are already the floor: `DEFAULT_TIMEOUT = 3` / `LONG_TIMEOUT = 5`
plus `EXTRA_TIMEOUT = 2` (`tools/wpt/timeouts.py`) against
upstream's 10/60/5. The kill lands at 5 s (3 968 files) or 7 s (1 260 files),
which is exactly the two clusters in the reports. Nothing can be shaved there: a
test has to stop hanging, or the feature it waits on has to fail fast.

A timeout also hides work: the runner kills the browser, so the report says
"TIMEOUT" where a real failure would have said what is wrong.

## How to diagnose one

    nix develop --command python3 tools/wpt/cli.py profile <test-path> [-- --test-types reftest]

`profile` reruns the file at trace and prints the browser's navigation/request
timeline plus wall time. Read it as: the last record before the silence is what
the test finished; everything it awaits after that never happened. Records that
all end in milliseconds while the test still times out mean the page is waiting,
not the network. `--test-types` matters: reftests and `.svg` tests are invisible
to the default testharness+crashtest selection.

Faster than reading the spec: write a probe next to the test that asserts *which
half* of the contract is missing, with a `step_timeout` so a missing event is a
fast FAIL instead of another 5 s hang. The probe that cracked the biggest class:

```html
<script src="/resources/testharness.js"></script>
<script src="/resources/testharnessreport.js"></script>
<script>
const t = async_test("srcdoc document exposes its markup and its script element");
let checked = false;
const iframe = document.createElement("iframe");
iframe.srcdoc = "<!doctype html><p id=probe>markup</p><script>0<\/script>";
iframe.addEventListener("load", () => {
  checked = true;
  t.step(() => assert_equals(iframe.contentDocument.body.textContent.trim(), "markup"));
  t.step(() => assert_equals(iframe.contentDocument.scripts.length, 1));
  t.done();
});
document.body.appendChild(iframe);
t.step_timeout(() => { if (!checked) assert_true(false, "load never fired"); t.done(); }, 1500);
</script>
```

## The classes

Grouped by the API the test waits on, by grepping the resolved source of every
timed-out test (generated variants map back to their `.js` source; the first
match wins, so a test can appear under a broader class than its own subject).

| waiting on | tests | timeouts | timeout-s | hang rate |
|---|---:|---:|---:|---:|
| no marker (long tail, see below) | 50 576 | 2 451 | 13 876 | 5% |
| `srcdoc` iframe | 826 | 622 | 4 280 | 75% |
| Perform Actions (`testdriver-actions`) | 2 112 | 370 | 2 012 | 18% |
| iframe / child browsing context | 2 339 | 332 | 1 901 | 14% |
| WebVTT `<track>` | 466 | 352 | 1 804 | 76% |
| testdriver, other endpoints | 1 578 | 295 | 1 723 | 19% |
| Web Animations / CSS animations | 743 | 246 | 1 251 | 33% |
| workers | 912 | 193 | 1 054 | 21% |
| canvas / drawImage | 1 824 | 130 | 661 | 7% |
| media element (`video`/`audio`) | 504 | 93 | 479 | 18% |
| BiDi / test-only API (bfcache, pageswap) | 136 | 67 | 475 | 49% |
| font loading | 866 | 86 | 439 | 10% |
| View Transitions | 385 | 63 | 332 | 16% |
| popover / dialog | 280 | 41 | 211 | 15% |
| wasm | 488 | 23 | 118 | 5% |
| Bluetooth (testdriver) | 174 | 10 | 104 | 6% |
| Navigation API | 289 | 15 | 72 | 5% |
| WebDriver permissions | 96 | 6 | 34 | 6% |
| media capture (gUM/WebRTC) | 382 | 6 | 33 | 2% |
| IndexedDB | 58 | 5 | 28 | 9% |
| test262 suite | 53 640 | 5 | 27 | 0% |
| PerformanceObserver / timeline | 283 | 2 | 10 | 1% |
| unresolved id / other | 37 | 2 | 10 | — |

## Verified root causes

### 1. `srcdoc` markup never reaches the frame document — 622 timeouts, 4 280 s (resolved)

In the 2026-10-18 baseline, the probe above showed `load` firing and
`contentDocument`/`contentWindow` existing while
`contentDocument.body.textContent` stayed `""`: the frame remained on
`about:blank` and never parsed the `srcdoc` markup.

That caused the security-features harness (behind `/referrer-policy/gen/*`,
`/mixed-content/gen/*`, `/upgrade-insecure-requests/gen/*`, `/fetch/metadata/*`)
to hang waiting for the frame's script to fetch and `postMessage`
(`common/security-features/resources/common.sub.js:1316-1323`).

Implemented in `crates/renderer/src/engine/mod.rs` (`FrameNavigation::srcdoc` /
`load_frame_srcdoc`, <https://html.spec.whatwg.org/multipage/iframe-embed-object.html#attr-iframe-srcdoc>).

### 2. `document.write` into a blank child frame — same family (resolved)

`iframe-blank` source contexts fetch the frame document and inject it with
`iframe.contentDocument.write(content)` + `close()` (`common.sub.js:1327-1338`).
`Document.write()` / `writeln()` / `close()` now route into the target document's
own world (`crates/renderer/src/js/bindings/node/document.rs`).

### 3. Media elements never finish resource selection — 445 timeouts, 2 283 s

`<video>`/`<track>` tests wait for `loadeddata`, an active cue, or a
`resource-selection-*` event. The timeline for
`/webvtt/rendering/cues-with-video/processing-model/2_tracks.html` shows the
media never fetched at all: no `/media/white.webm`, no `support/test.vtt` record.
`crates/renderer/src/js/bindings/node/media.rs:28` is honest about the state
(`networkState` stays `NETWORK_NO_SOURCE`), but the spec's *dedicated media
source failure steps* never run, so the element never fires `error` and the
test's wait never settles
(<https://html.spec.whatwg.org/multipage/media.html#dedicated-media-source-failure-steps>).

Fix: run those failure steps when no source can be selected — `error` event,
`MediaError`, queued task. No decoder is needed: a test blocked on the missing
pipeline then fails in milliseconds instead of burning its whole budget. This is
fail-fast, not a new feature.

### 4. Web Animations — 246 timeouts, 1 251 s

Reftests under `/css/css-masking/clip-path/animations/*` and
`/css/css-transitions/*` wait on `document.getAnimations()[0].ready`.
`getAnimations` is declared in the IDL
(`crates/webidl-bindgen/idl/web-animations.idl:129`) but returns nothing, so
`[0]` throws before the test can call `takeScreenshot`, and the page never lifts
`reftest-wait`. The reftest then times out with no comparison at all.
Fix: implement `Animatable.getAnimations()` over the CSS animations/transitions
the engine already tracks.

### 5. WebDriver and test-only endpoints — 742 timeouts, 4 314 s

- **Perform Actions** (370, 2 012 s): `/html/editing/dnd/*` and every test that
  drives input through `test_driver.Actions()`. The timeline ends at
  `testdriver-actions.js`; the awaited drag/drop/pointer event never arrives.
  `perform_actions` exists (`crates/webdriver/src/classic.rs:2368` and
  `crates/webdriver/src/bidi/input.rs:225`) and does answer —
  so the gap is the page-side event the test waits on, not the endpoint.
- **bfcache / pageswap / BiDi** (67, 475 s, 49% hang rate in the 2026-10-18 run):
  WebDriver BiDi is now implemented (`crates/webdriver/src/bidi/`); the remaining
  hangs in this bucket are test-only navigation hooks (bfcache, pageswap).
- **Bluetooth** (10, 104 s): all "TestRunner hit external timeout".
- **Permissions** (6, 34 s) and other testdriver endpoints (295, 1 723 s).

Fix per endpoint: deliver the command and its event, or answer with an error the
page can observe. A silent no-answer is what costs the budget.

## The long tail — 2 451 timeouts, 13 876 s, no marker

Largest directory groups: `/html/semantics` 1 695 s, `/html/browsers` 721 s,
`/html/cross-origin-opener-policy` 641 s, `/referrer-policy/gen` 533 s,
`/mixed-content/gen` 484 s, `/upgrade-insecure-requests/gen` 333 s,
`/svg/painting` 327 s, `/svg/geometry` 252 s, `/svg/animations` 246 s,
`/fetch/metadata` 232 s, `/css/css-view-transitions` 210 s, `/webaudio` 125 s,
`/svg/interact` 113 s, `/svg/text` 113 s, `/content-security-policy/unsafe-hashes` 108 s.

Several of the `gen/` trees and `/fetch/metadata` belong to the same
child-frame-injection family as #1/#2 (they are the same harness with `iframe`,
`iframe-blank`, `iframe-data`, and worker source contexts). The `/svg/*`,
cross-origin, and child-browsing-context groups still need their own probe; the
`iframe / child browsing context` class (1 901 s) is the natural place to start,
since it is 14% of its population and shares the frame-document machinery.

## Not fixes

Raising `DEFAULT_TIMEOUT`/`LONG_TIMEOUT`/`EXTRA_TIMEOUT`, `--skip-timeout`, or
excluding a directory changes the score instead of the engine, and `score` /
`overnight` refuse `--skip-timeout` on purpose. A hang that is merely reported
faster is still a hang.
