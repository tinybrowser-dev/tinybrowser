## Binary size

11,492,008 bytes (2026-10-18)

## WPT

Since 2026-11-05 the push gate in `wpt.yml` runs in-process (`run in-process`,
plus WebDriver only for testdriver and worker/module wrappers, plus native
test262), and it is the only suite run: the nightly WebDriver duplicate was
removed on 2026-11-07. It fires on pushes to `main` that touch code, the
harness, the WPT pin, or the build environment; a docs-only push does not
spend the 20-job matrix. A 2,195-test same-build comparison of the two
executors agreed on 94.9% of verdicts and passed 1,148 vs 1,150.

The gate first passed in run 41972597140 at the 11/9 split: 15m03 wall for
the 119k-test suite, of which 1m34 was queue, 1m42 plan, 3m17 build (one job
per binary, three links in parallel), 9m47 test and 22s score. That run
measured the harness lane at ~74 chunk-minutes at 4 jobs and the WebDriver
lane at ~32, so the 20-job limit splits 14/6 rather than 11/9. Run 41974678252
(14/6, 4s queue) took 11m19: 1m47 plan, 3m57 build, 6m34 test, 24s score,
with harness chunks summing 70.6 chunk-minutes (218-393s) and WebDriver 27.8
(202-332s). The slowest harness chunk alone set the test phase. One harness
process carries 600 files (the engine keeps ~5 MB per file alive, see
`docs/upstream.md`) and journals every file, so an engine abort costs one
ERROR instead of a chunk, and a page that stops returning is cut by a
watchdog instead of the job timeout.

Run 42013267456 took 11m09: 1m50 plan, 4m29 build, 6m11 test, 22s score, with
harness chunks summing 79.1 chunk-minutes and WebDriver 29.5 (the build is
longer than 41974678252's 3m57 only because the blitz submodule changed and
more of it recompiled). 108.6 chunk-minutes over 20 four-core jobs puts the
test phase's floor at 5m26; the spread inside the harness lane (275-382s) is
hash-partition noise, so re-splitting the lanes cannot beat it. All eight
engine aborts the harness found are now fixed: the first six in the fork, and
the last two were dependency panics with a blitz-side cause (see
`docs/upstream.md`), so they moved five files from ERROR to PASS or OK without
changing any other verdict.

Run 42015620914 (11m27: 31s queue, 1m41 plan, 4m08 build, 6m48 test, 28s
score) is the first scored with the corrected `is_expected` in
`tools/wpt/cli.py` (`db0985ff`): 118,532 tests, 68,809 pass, 13,528 expfail,
28,190 unexpected, 4,130 timeout, 1 crash, 3,874 error. Before that fix the
same reports were mis-scored because a status was compared with itself.

Run 42027403312 (11m25: 3s queue, 1m49 plan, 4m16 build, 6m44 test, 20s score)
is the first with every known engine abort fixed:
`css/css-borders/border-radius-greater-than-width.html` and
`css/css-viewport/zoom/massive-zoom-viewbox-crash.html` move from ERROR to
PASS, and the run has 0 crashes: 118,532 tests, 68,810 pass, 13,527 expfail,
28,192 unexpected, 4,132 timeout, 0 crash, 3,871 error. The rest of the
verdict movement against the previous run is noise, not regressions:
timeouts flip both ways with runner load, and the `font-stretch`/`css-text`
reftests flip with font rasterisation (repeated local runs of one binary flip
the same files).

Dedicated workers landed on 2026-11-09. The `workers/` in-process slice
(248 files) moved from 7 passing files / 34 passing subtests to 99 / 226,
with no previously passing file or subtest lost. The remaining timeouts are
the missing SharedWorker, service-worker, worker `fetch`/XHR, MessagePort
transfer, Blob-URL, and IndexedDB surfaces. A 2,131-file regression slice
(`dom/events`, `html/webappapis`, `encoding`, `xhr`, `FileAPI`) improved by
78 passing subtests against the same build without workers; three
worker-aware files that used to fail immediately with `Worker is not a
function` now spend their budget and report TIMEOUT instead.

The dedicated-worker surface then grew to 222 passing files / 481 passing
subtests of 794 on 2026-11-10, again with no subtest lost. The batch adds
`MessagePort` delivery inside worker realms and port transfer on
`postMessage`, the full structured-clone codec in worker realms
(`ImageData`, `ImageBitmap`, `OffscreenCanvas`, `FileList`, boxed BigInt,
error `cause`, resizable buffers, array non-index properties), module
workers (`import.meta`, response-URL bases after redirects, `blob:`/`data:`
graphs, `type: module` option semantics), worker `fetch`/XHR through real
dials (including synchronous XHR and `FormData`), `importScripts` refusal
in a module worker, and timer handles that start at 1 as Chromium's do.
The remaining `workers/` failures are the unimplemented SharedWorker
(~250 subtests), service-worker fixtures, module CSP/referrer/credentials
cases that need the in-process harness to implement wptserve pipes, and
the upstream QuickJS gaps in `docs/upstream.md`.

SharedWorker landed on 2026-11-10. The `workers/` slice (248 files) moved
from 222 passing files / 488 passing subtests of 794 to 218 perfect files
/ 886 passing subtests of 966, with no subtest lost. The realm key is
`(creator origin, script URL, name)`; a connection repeats the key and
must agree on `type`/`credentials`/`extendedLifetime`, and a mismatch
fires a plain `error` event on the new `SharedWorker` object without
joining. Each connection gets a fresh `MessagePort` pair, and the worker
side arrives as a `connect` event whose `data` is `''` and whose `source`
is `ports[0]`. Runtime errors stay at the shared global; load and parse
failures fan out to every connected `SharedWorker`. Worker script URLs
now resolve through the document's encoding override (a windows-1252
document encodes `?ß` as `?%DF`), `name` stores the assigned value as a
`[Replaceable]` attribute, and the worker construction path exposes
`document.links`, which the worker navigation tests use.

Module workers then got two fixes: the top-level graph fetches with the
outside settings object's origin, so a `data:` worker's static imports
are same-origin to its creator and its dynamic imports still use the
worker's opaque origin; and the worker's root script stays same-origin
even when the server sends `Access-Control-Allow-Origin`. Async XHR now
resolves `blob:` URLs in its realm, and a worker realm installs
`BroadcastChannel` with engine-wide channel ids, so an opaque-origin
worker can exchange messages with its owner. The in-process harness now
applies wptserve's `header(...)` and `sub` pipes, which let the module
CSP/referrer/data-URL and opaque-origin files run.

`WorkerOptions.credentials` now reaches the wire: a module worker's whole
graph (root script, static imports, dynamic imports) is fetched under the
mode the option selected, while a classic worker keeps `same-origin`.
Worker globals also expose `origin` from the
`WindowOrWorkerGlobalScope` mixin as a `[Replaceable]` attribute, which
worker code uses to build same-origin URLs. Two harness gaps had been
masking these: a document mounted from disk never stored its `.headers`
`Set-Cookie` in the jar, and `domains[...]` rejected the configured
Unicode subdomain spellings instead of expanding them to IDNA. With both
fixed, the dedicated and shared `WorkerOptions.credentials` files pass
33/33 each (51 subtests gained) and the WorkerLocation origin file passes.
The structured-clone battery then stopped misclassifying a typed array
that a resized buffer left out of bounds: both byte getters report 0 for
it, so the serializer could not tell it from a genuine empty view. The
rquickjs fork gained `is_out_of_bounds_view`, which asks
`JS_GetTypedArrayBuffer` and consumes the exception that reports the
state, and the serializer throws the required `DataCloneError`; the four
`Serializing`/`Transferring OOB TypedArray throws` subtests in the
dedicated and shared batteries now pass. The remaining failures are
popup-based `extendedLifetime` and same-site cookie files, `IndexedDB`
absence, CHIPS partitioning, joint-history traversal in the in-process
harness, `WebAssembly`'s absence in the growable-SAB fixture, and the
QuickJS `eval(import())` gap.

Dedicated workers now run on their own OS threads, one `QuickJS` runtime per
realm, so a page that spins on `Atomics` keeps its worker running and
`terminate()` interrupts a worker stuck in synchronous script. The engine
sends commands and fires engine-owned timers over channels; the worker
publishes an `active` count, a terminate flag, and a pending-fetch count.
The Blob URL Store became process-wide so a page-created `blob:` URL is
fetchable from a worker thread, and `SharedArrayBuffer` now crosses realms
by reference: the rquickjs fork installs reference-counted
`JS_SetSharedArrayBufferFunctions` hooks and exposes `SharedBufferLease`, so
the receiver wraps the sender's bytes instead of copying them.

The `workers/` slice went from 886/966 passing subtests and 218 perfect files
to **897/972 and 223 perfect files**, with five fewer failing files and no
lost subtest. The gains are the two `happens-in-parallel` files,
`postMessage_block`, `Worker-terminate-forever-during-evaluation`,
`Worker_terminate_event_queue`, and `workers/semantics/xhr/001.html` (the
worker thread polls its fetch channel while a dial is in flight).
`docs/worker-threads.md` records the thread model, the timer mirror, the
measured shared-memory transport, and the remaining milestones (shared and
nested worker threads).

The in-process harness can host popups now. A carrier with no tabs answers
`BrowsingContextHost::opens_window_as_tab` false, and the renderer creates an
auxiliary context in its own engine instead of asking the browser for a tab;
the popup navigates from its initial `about:blank` and the opener's
`WindowProxy` writes made before the popup's realm existed (an `onload`
handler, a property assignment) are replayed into the first real document.
A frame's `load` waits for that replay, so `popup.onload` fires.
`workers/` went from 897/972 passing subtests and 223 perfect files to
**933/974 and 227 perfect files**, gaining 36 subtests with no loss: the
four `extendedLifetime` files, `data-url-shared`, the four module
CSP/referrer files (they run now and fail only on real CSP/referrer gaps),
and two partitioned-cookie assertions. Timeouts fell from 9 files to zero.

The same-site-cookie files then went from six errors to six passes. The
harness serves a `testdriver-vendor.js` backed by the page's
`document.cookie`, and the `SharedWorker` `sameSiteCookies` option is
implemented: `all` is refused outside a first-party site context, the absent
option defaults to `all` there and `none` otherwise, and `none` restricts the
worker's script, module graph, `fetch`, XHR, and `Set-Cookie` storage to
`SameSite=None`. The context check walks the creator's ancestor chain, so a
same-site frame inside a cross-site one is still third-party.

Module CSP and referrer followed. The harness now applies `header(...)` pipes
to `.sub.` paths instead of only expanding their bodies, so a cross-origin
referrer test actually sees the policy. A module fetch's referrer is the
declaring module (or the worker's own script for a dynamic `import()`), a
worker's static imports settle under `worker-src` with its full fallback list
and never `script-src-elem`, and a worker's script response contributes the
referrer policy and CSP list its dynamic imports run under while static
imports keep the creator's. `workers/` is now **961/974 passing subtests and
237 perfect files**, with 10 failing, 0 timeout, and 1 error file, no subtest
lost; both module referrer files are 15/15 and both module CSP files are
perfect. `cookies/` and `referrer-policy/` are unchanged.

CHIPS followed. The cookie jar stores a `Partitioned` cookie under the site of
its top-level document, requires `Secure`, and includes the key in the cookie
identity; every dial and `document.cookie` call carries the key, a nested frame
uses the initiator's top-level site while a top-level navigation uses the
destination's, and a worker inherits its creator's. A `SharedWorker` is keyed by
its constructor storage key (origin plus top-level site) plus name, so each
top-level site gets its own realm. The realm-level first-party verdict now folds
its parent's stored verdict at creation and recomputes on an origin change, and
`body`/`frameset` mirror the Window-reflecting handler set. `workers/` is now
**963/974 passing subtests and 239 perfect files**, with 9 failing, 0 timeout,
and 0 error files, no subtest lost; `cookies/` gains 4 subtests (331 to 335) and
one perfect file (40 to 41), and `webmessaging/` is unchanged.

Every push to `main` now publishes the record to
`tinybrowser-dev/tinybrowser-wpt-results` and the site at
`tinybrowser.ericc.ch/wpt` rebuilds from it: subtests passing, files passing,
skipped, the history chart, the diff against the previous run, and the
per-folder table. `latest.json` has no failure list; the ~100k rows live in
`failures.jsonl.gz` and are what the next run diffs against. There is no
backfill: the history starts with the first push after the publish step
landed.

CI 2026-10-23 run 39980734258 (wpt.yml, 20 chunks), --exclude=worker
testharness+crashtest+reftest+test262

pass 66803
expfail 29
unexp 43362
timeout 5325
crash 1
error 3404
skip 78
tests 119002

per type (pass / tests)
testharness 8344 / 34819 = 24.0%
crashtest 1885 / 2031 = 92.8%
reftest 13448 / 28492 = 47.2%
test262 43126 / 53660 = 80.4%

Total 56.2% = pass / (tests - skip)

### referrer-policy/ (non-worker), 2026-10-25

Chrome 157 (WPT 9f868fd7) passes 892 of the 899 files that run here once
worker variants and their generated directories are set aside; tinybrowser
passes 890. The two-file gap is the style engine:

- 2 SVG (`css-integration/svg/{external-stylesheet,processing-instruction}`):
  paint servers (`fill: url()`, `stroke: url()`) are not fetched, and this
  Servo build does not generate `filter: url()`, `marker-*`, or a computed
  `clip-path: url()`, so 7 of the 9 paint properties in each file cannot reach
  a request. The other four `css-integration/svg/` files fail the same seven
  subtests and are Chrome failures too. This is the ceiling: 890 of the 899
  files is the most this engine can pass without a forked style engine.

The files that used to be blocked on popups and child history now pass:

- `generic/inheritance/popup-inheritance-about-blank.html`: `window.open()`
  now creates an auxiliary top-level context in the same renderer for a blank
  or same-origin URL, so the opener, `document`, `opener`, and messaging are
  live synchronously. Cross-origin popups keep the browser-tab proxy.
- `generic/inheritance/{iframe-inheritance-history-about-blank,
  iframe-inheritance-history-about-srcdoc}.html`: the engine keeps each child
  navigable's own session history, and a traversal installs the saved
  `about:blank`/`about:srcdoc` document state (origin, policy container,
  referrer) instead of re-fetching. The srcdoc file is now ahead of Chrome,
  which fails one of its subtests.

The remaining Chrome failures (`popup-inheritance-form-submission.html`,
`workers.html`, the four SVG files above) fail here too, so they cost nothing
against the Chrome score. `iframe-upgrade-request-to-cross-origin` passes here
and fails in Chrome.

The 30 generated `script-tag-dynamic-import` files used to time out at the
7.2 s budget, each with 12 subtests unreported. Nothing was slow: the renderer
drain reported idle while a module evaluation sat in a realm's outbox, so the
next test case only started when the harness timeout arrived. The session now
asks whether ready work remains after publishing, and the drain reconciles once
more before declaring itself idle. See the `renderer: stop parking the drain`
commit.

`css-integration/svg/` is the one harness-policy exception: each file sleeps
800 ms per subtest (7.2 s total) before checking, above the 3 s default, so
those files get a 6 s budget (8 s with grace, still under testharness.js's
10 s clock). See `tools/wpt/README.md`.
