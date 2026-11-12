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

Standalone joint session history followed. A carrier with no browser tab (the
in-process harness and the embedded renderer) now keeps the joint steps itself:
a child navigation or `pushState()` adds one, and a step moves every frame
whose entry differs at the target, so a main-frame `history.back()` restores a
child's entry. `workers/semantics/navigation/001.html` passes, taking
`workers/` to **964/974 passing subtests and 240 perfect files**, with 8
failing, 0 timeout, and 0 error files, no subtest lost. The two history
interface files that navigate an iframe gain their `history.length` subtests,
and `cookies/` and `webmessaging/` are unchanged. A standalone engine records
no main-frame entry, so a step that would move the main frame is refused rather
than applied halfway; a browser-hosted tab actor owns the full list.

`WebAssembly.Memory` followed, with non-shared memories backed by a fixed
`ArrayBuffer` that `grow()` detaches and replaces, and shared memories backed
by a real growable `SharedArrayBuffer` that `grow()` extends in place. The
module compiler is still absent, so `Module`, `Instance`, `compile`, and
`validate` are not exposed. The two `structured-clone` files now reach the
`Growable SharedArrayBuffer` case, and the engine refuses to serialize shared
memory outside a cross-origin isolated realm, which is the branch that case
expects in the harness; both subtests pass. Serialization is gated by the new
`crossOriginIsolated` capability, computed from `Cross-Origin-Opener-Policy`,
`Cross-Origin-Embedder-Policy`, the secure context, and the embedder's (or
worker creator's) capability, and the three worker files that transfer shared
memory under COOP/COEP still pass. `workers/` rises to **966/974 passing
subtests and 240 perfect files**, with 8 failing, 0 timeout, and 0 errors, no
subtest lost; the two `structured-clone` files still count as failing only
because `ReadableStream` transfer is a SKIP. `cookies/` and `webmessaging/`
are unchanged. `docs/upstream.md` records the fixture's remaining wrong
assertion for an isolated page.

`ReadableStream` transfer then landed: a transfer list entry locks the sender
and moves the stream over an internal `MessageChannel`, one chunk per pull, so
the receiver's reads pace the source and close/error/cancel cross the channel.
Both `structured-clone` files pass completely, taking `workers/` to **968/974
passing subtests and 242 perfect files** with no loss.

The last engine gap was `eval(import())` inside a blob- or data-URL module
worker. The eval'd code runs in an imported HTTP module, so the relative
specifier has a real base URL, but quickjs's `js_dynamic_import` captured the
fixed `<input>` name for eval frames. The fork now walks out of the eval frames
to the enclosing script or module (`docs/upstream.md`), and all four module
files pass: `workers/` reaches **972/974 passing subtests and 246 perfect
files**, with 0 timeouts, 0 errors, no subtest lost, and only the two
`workers/data-url*.html` files left, each short one `indexedDB is present`
assertion. `cookies/` and `webmessaging/` remain unchanged. IndexedDB was the
last feature-sized gap; it is a real implementation, so no `"indexedDB" in
self` shim was added.

`indexedDB` then landed as a shim over the browser-owned storage seam. The
factory, `open()` and its `upgradeneeded`/`success`/`error` events,
`deleteDatabase()`, `databases()`, one versionchange transaction per upgrade,
object-store create/delete, and the record operations `put`, `add`, `get`,
`getKey`, `getAll`, `getAllKeys`, `count`, `delete`, and `clear` are
implemented, with `IDBKeyRange`, inline and generated keys, binary and array
keys, and `DOMStringList`. Rows are opaque encoded values in a per-origin area
behind `StorageKind::IndexedDb`, owned and persisted by the browser process
(`tinybrowser-indexeddb-v1`), so every renderer of a profile sees one store.
An opaque origin has no storage key: `open()` and `deleteDatabase()` throw
`SecurityError` and `databases()` rejects, which is what the two
`workers/data-url*.html` files test. `workers/` reaches **974/974 passing
subtests and 248 perfect files**, with 0 failures, 0 timeouts, and 0 errors;
`cookies/` and `webmessaging/` are unchanged at 522/642 subtests and 154
perfect files. A 16-file IndexedDB slice (factory, keys, ranges, requests,
transactions, object stores, binary keys, delete) passes 142 of 150 subtests
and 12 of 16 files; the eight remaining subtests all need indexes or cursors,
which are the next milestone and are absent rather than stubbed.

The index and cursor milestone landed next. `createIndex`, `deleteIndex`,
`index`, `indexNames`, unique and multiEntry constraints, every `IDBIndex`
query, `IDBObjectStore`/`IDBIndex` `openCursor` and `openKeyCursor`, all four
cursor directions, `continue`, `advance`, `continuePrimaryKey`, `update`, and
`delete` are implemented, together with the new `getAllRecords()` and
`IDBGetAllOptions` forms of `getAll`/`getAllKeys`, object-store and index
renames, key-generator ordering, and `Infinity` keys. The whole IndexedDB
directory now passes **1258 of 1403 subtests and 165 of 234 files**, up from
439 subtests and 53 files. The WPT harness also stopped treating
`storage_remove`/`storage_clear` as no-ops, so an in-process transaction that
deletes a row no longer aborts with a false `QuotaExceededError`. What remains
is connection semantics: upgrade blocking, transaction scheduling across
overlapping connections, a shared key generator, `AbortError` on an uncaught
listener exception, and the last metadata-revert cases.

Traversal moved next, because `dom/traversal` was the cheapest large hole in
tier 0. `TreeWalker` now implements all six traversal algorithms, the
`currentNode` setter, the read-only attributes, and the filter re-entrancy
guard that throws `InvalidStateError`; `NodeIterator`, `createNodeIterator`,
`detach()`, and the reference/pointer traversal are new. `dom/traversal`
moves from 11.1% of files to **13 of 18 files and 1573 of 1608 subtests**:
`TreeWalker.html` passes 761 of 761, `NodeIterator.html` 766 of 766, and every
TreeWalker functional file passes. The five remaining files need the
NodeIterator pre-remove steps, named-property access to an iframe, or
`srcdoc`.

The WPT work also exposed an engine bug outside IndexedDB: reading an
undeclared global name returned `undefined` instead of throwing
`ReferenceError`, because the Window named-properties proxy on the global
prototype answers `[[Get]]` with `undefined` for a missing name. The quickjs
fork now resolves a global binding through `[[HasProperty]]` first, as the
object environment record does (`docs/upstream.md`); the engine carries a
regression test for bare, `eval`, `Function`, and strict-mode reads.

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

## URL failure consistency, 2026-11-11

`url/failure.html` ran every failure input through five APIs. Three of them
already threw; the last two were engine gaps. `URL.href` silently kept the old
value instead of throwing `TypeError`, and XHR/fetch compared a missing parse
with `=== null`, so an `undefined` answer slipped through. Both now throw.

`window.open(url)` also returned `null` for an unparsable URL because the
engine's window-open plan conflated "no URL" with "bad URL". The shim now
encodes and parses a non-empty URL first and throws `SyntaxError`, which is the
window-open step before any context is chosen.

The `Location` case exposed a structural gap: assigning
`frame.contentWindow.location` for an `iframe` created in the same task read
`undefined`, because a `QuickJS` realm cannot start while the inserting script
runs. The `WindowProxy` now sees a pending same-origin frame
(`__tbFrameSameOrigin`), answers its globals from the inserting realm, and
builds a `Location` bound to the child frame, so the assignment parses, throws
`SyntaxError` for a bad URL, and queues the child's navigation for a good one.
`dom/traversal/TreeWalker-realm.html` passes for the same reason.

`url/failure.html` moves from 795 to **1175 of 1175 subtests**, `url/` from
8505 to **8885 of 10020**, and `dom/traversal` from 1573 to **1575 of 1608**.

## Beacons, 2026-11-11

`navigator.sendBeacon()` was absent, so `beacon/` passed 2 of 73 subtests.
It now sends through the same dial as `fetch` with the keepalive flag:
`BodyInit` extraction (string, `Blob`, buffer source, `FormData`,
`URLSearchParams`), the `http`/`https` scheme check, `no-cors` for a
CORS-safelisted body type and `cors` otherwise, and the 64 KiB in-flight
quota. The quota is per document and released when a request finishes,
which the parallel WPT cases require.

`beacon/` moves to **64 of 73 subtests and 13 of 16 files**, with
`beacon-basic` and `beacon-cors` fully passing. The three remaining files
are outside the beacon surface: `idlharness.any` needs `WebIDL2`,
`header-content-type-and-body` needs `performance.getEntriesByName()`, and
`header-origin-same-origin` expects `Origin: null` on a cross-origin
`no-cors` request where this engine (like the spec's append algorithm)
sends the origin.

A same-origin `iframe` inserted by a script still has no realm until the
script returns, so its `WindowProxy` now hands out a `Navigator` instance of
its own. That keeps per-document state (the beacon quota) with the child
instead of borrowing the inserting document's.

## HTTP cache, 2026-11-12

The in-process harness had no HTTP cache: its `fetch()` called the fetch
core and the transport directly, so `fetch/http-cache` measured the
harness as much as the engine, and the browser's cache was a second copy
inside `crates/browser`. Both hosts now run `dial::HttpCache`
(`crates/dial/src/cache.rs`), one implementation of selection, freshness,
revalidation, and invalidation, with the browser and harness passing their
own partition and clock.

The cache implements the Fetch and RFC 9111 behaviors the suite checks:
`Vary` representations stored per base key, request directives
(`no-store`, `no-cache`, `max-age`, `min-fresh`, `max-stale`,
`only-if-cached` with its generated 504), a private cache that ignores
`s-maxage`, an unparsable `Expires` treated as already stale, response
`Pragma: no-cache`, heuristic freshness from `Last-Modified`, `Age`
correction, 304 header merges, unsafe-method invalidation of the request
URL and same-origin `Location`/`Content-Location` plus storage of a
`Content-Location` response under a GET key, byte ranges (slicing a stored
`200`/`206` and completing a stored `206` with `Range: bytes=<end>-`), and
`No-Vary-Search` lookup across query variants.

The harness also got a `HarnessClock` the engine advances with its
virtual-time jumps, so a `pause()` in a test really ages the cache and the
cookie jar instead of leaving them frozen.

`fetch/http-cache` moves from 73 of 142 subtests and 0 of 15 files to
**139 of 142 and 12 of 15**, with `304-update`, `cache-mode`,
`cc-request`, `credentials`, `freshness`, `heuristic`, `invalidate`,
`no-vary-search`, `partial`, `pragma-no-cache-with-cache-control`,
`status`, and `vary` fully passing. The three remaining files are
recorded in `docs/limitations.md`: cache partitioning by top-level site,
the absent HTTP authentication cache, and an upstream `post-patch` body
expectation.

A review of the slice folded in the Fetch and RFC 9111 behavior the first
pass missed. `only-if-cached` now separates the cache mode (any stored
response, stale included; a miss is a network error) from the request
directive (the generated 504). `Vary` compares the headers that actually go
on the wire, so a `Vary: Cookie` response is not reused across login
states. Revalidation requires a conditional dial and a matching strong
validator, the 304 merge keeps the stored status and refuses to rewrite
framing or coding headers, and a served hit reports its current `Age`.
Range handling serves a stored `206` that covers the whole representation
as a complete `200`, completes only a stored prefix, and honors `If-Range`
(ignoring a weak validator). A same-origin request is no longer answered
by a cached cross-origin redirect, invalidation stays inside its
partition, `force-cache` still validates a `no-cache` directive, and
`No-Vary-Search` decodes percent-encoded keys, ignores unknown dictionary
members, and compares username and password. Integrity metadata is checked
for every hit and for a buffered network body in the in-process host,
which previously checked it only when a stale entry happened to exist.
A streamed unsafe response now invalidates as soon as its head arrives and
stores its `Content-Location` entry when the body completes, so a browser
`POST` result page is cached with its bytes instead of an empty body: the
webdriver lane returns `"abc"` for the `post-patch` reuse.

A cache is a behavior change for every suite that fetches a URL twice, so
it was measured against a build with the harness cache disabled:
`fetch/api` gains six files (`conditional-get`, `integrity.sub` 18/18,
`request-cache-default`, `-force-cache`, `-only-if-cached` 14/14,
`-reload`) and `xhr/`, `fetch/range`, and `fetch/redirects` keep every
subtest they had. No file regresses.

## Content Security Policy, 2026-11-12

The whole `content-security-policy/` directory now runs instead of
erroring on its support scripts, and the checks it exercises landed in
the engine rather than in page shims. `wptserve` answers an absent
`{{GET[key]}}` with the empty string, and the CSP support scripts rely on
that (`tools/wpt-harness/src/template.rs`).

String compilation is gated where the engine compiles a string
(`Runtime::set_can_compile_strings`), so direct `eval` keeps direct-eval
scope and `Function` and indirect `eval` share one check. The check
follows the spec: only `'unsafe-eval'` allows a compilation; a matching
hash source names an inline script and does not, and `'wasm-unsafe-eval'`
covers WebAssembly rather than strings. An event handler content
attribute is compiled by HTML, not by page script, so its compilation is
not an evaluation: the inline check (`script-src-attr` and its fallback)
runs when the attribute is set and again at dispatch, and the handler
slot stays null when it blocks. `setTimeout`/`setInterval` compile a
string handler when the timer is created and return 0 when CSP blocks
it; the firing path runs the already-checked string without compiling it
again, so a meta policy added later neither skips a timer nor reports
twice.

`base-uri` is implemented: a `<base href>` is resolved against the
fallback base URL, checked against every policy that names the directive
(no `default-src` fallback), and a refused element leaves the fallback
base in place. The check runs from the parser walk, from the insertion
steps, and when `href` is set, changed, or removed, and each `href` value
is checked once. An unparseable or `data:`/`javascript:` href keeps the
fallback, as HTML requires, and report-only policies report without
blocking. `document.baseURI` and every relative URL resolve through the
same frozen base.

Violations are queued rather than dispatched inside `setAttribute`, so a
listener attached after the setter still sees the event. The handler
check reads the element's own Document's policy list, and a violation
sample is sent only when the deciding directive carries
`'report-sample'`.

The harness polls its result expression with `document.eval` while a page
is parsing. That evaluation used to adopt every pending iframe, starting
`src` loads before the parser-blocking scripts ahead of them ran; it now
leaves adoption to the parse walk. A parser-created frame's `about:blank`
document is additionally registered when it is materialized, so a classic
script that follows the `<iframe>` sees `contentDocument`, and a child
frame's about base URL is the creator's document base URL, not its
document URL.

`content-security-policy/` moves from **2176 to 2201 passing subtests**
and from **697 to 703 passing files** (884 files, 3977 reported
subtests). `base-uri` is 5 of 6 files and 10 of 11 subtests,
`unsafe-eval` is 12 of 15 files (15 of 23 subtests), `script-src` is 78
of 90 files (125 of 203 subtests), and `generic` is 23 of 29 files (84 of
99). The `securitypolicyviolation` sample and targeting files gained 9
subtests. The only regression is `sandbox/sandbox-empty.sub.html`,
recorded in `docs/limitations.md`: a child's `postMessage` now reaches
the parent handler that the test expected to miss it.

`CookieStore` landed as a shim over the cookie jar, taking `cookiestore/`
from **0 to 34 passing files** and from 0 to **208 of 244 passing
subtests**. The interface is secure-context only and absent in an opaque
origin's methods, which reject with `SecurityError`; the overloads, the
name/value validation, the default path, the `__Host-`/`__Secure-` prefix
rules, the UTF-8 size limits, the `Domain` suffix and public-suffix rules,
and the `sameSite`/`partitioned` conversions run in the shim, while the jar
keeps storage, retrieval order, and the storage model's refusals. A write
crosses as a typed `ScriptCookieWrite`, so a `path` holding `;` cannot inject
attributes; a read is the jar's script-visible records with their storage
slot and an observable-attribute state. `CookieChangeEvent` diffs the jar
before and after each script-initiated write, and `onchange` is an event
handler attribute read at dispatch, so it is called once.

The slice also fixed two latent bugs the review found: a document's cookie
URL used the current document base URL, so a `<base href>` in an
`about:blank` frame could move the cookie query out of the renderer's
authorized site (the browser treats that as a violation and kills the
renderer); the URL is now captured when the document is installed and never
follows `<base>`. `CookieJar::records_for` now returns the retrieval model's
order (longest path first, then oldest), which is what `get()` returns as its
first item.

`cookiestore/` is 34 of 48 files and 208 of 244 subtests. The remaining
files need testdriver `get_named_cookie` (13 subtests), service workers (12,
plus the IDL harness), the iframe realm before its creating script (2), a
detached frame's `contentWindow` (1), HTTP-initiated change events (7), and
`fetch_tests_from_window` completion (`cookieStore_delete_domain`, whose two
subtests pass). `docs/limitations.md` records each gap.

WPT's `get_named_cookie` now works in the harness, closing the last
testdriver gap in the cookie suite. The vendor script asks a
`/resources/testdriver-cookie?name=` route, and the origin answers from the
jar's live records filtered by the request host, so the result carries the
cookie's path, domain, `Secure`, `HttpOnly`, expiry, and `SameSite` rather
than the subset `document.cookie` exposes. The jar publishes that snapshot
after every operation that can store a cookie, including a page fetch whose
response carried `Set-Cookie`. `cookieListItem_attributes` and
`cookieStore_set_path` now pass all 14 subtests, and `cookiestore/` is
**37 of 48 files and 221 of 244 subtests**.

Same-document fragment navigation landed next. A `Location` assignment that
changes only the fragment (`window.location = '#x'`, `location.href`, and
`location.hash`) now updates the document URL and appends its history entry
in place, fires a trusted `hashchange` with `oldURL`/`newURL`, and queues no
load; a different path still navigates. A `Location` whose browsing context
is gone ignores every assignment, as the setter steps require. The
`html/browsers/history/the-location-interface` directory is **69 of 252
subtests** (from 61): `location_hash.html` gains four, and
`location_hashchange_infinite_loop`, `location-hash-about-blank`,
`location_hash_set_empty_string`, `location-hash-setter-empty-string`, and
`no-browsing-context` one each. `html/browsers/browsing-the-web/history-traversal`
goes from 19 to **24 of 87 subtests**, including
`hashchange-same-fragment-does-not-fire`, `hash-empty-string`, and the
`before-load-hash` pair. No subtest anywhere in either directory went from
passing to failing. `same-hash.html` started running instead of erroring; its
subtests fail because the engine exposes no `scrollY` for the scroll tests to
read. `cookieStore_getAll_arguments` now finishes in about 65 ms instead of
reloading and sometimes exceeding the budget.

`WindowEventHandlers` handlers (`onhashchange`, `onpopstate`, and the rest of
the set) are now installed from their own name list, window-only, so an
element with one of those attributes has no handler and `window.onhashchange`
is a real handler property instead of a plain data property.

`CookieStore` change events now cover HTTP responses. The jar reports
whether it stored a `Set-Cookie` through `DialOutcome::cookies_stored`
(the header itself is a forbidden response-header name and never reaches the
renderer), and every store event queues "process cookie changes" in each
live realm of the renderer. Each realm keeps its own snapshot of the jar's
script-visible records and diffs it, so `cookieStore.set`/`delete`,
`document.cookie`, and a response header all take one path. The realm that
made or received the change processes it before the operation returns; the
rest process it in their own next turn. A cache hit clears the flag, since
it exchanges no headers, while a `304` carries its own response's flag.

`change_eventhandler_for_http_cookie_and_set_cookie_headers` passes 7 of 7
subtests, and `cookiestore/` is **38 of 48 files and 228 of 244 subtests**.
`HTTP duplicate cookie should not be observed` passes as well: the diff runs
against the jar at process time, so a response that stores the same value
leaves nothing to report. `cookies` stays 336/415, `fetch/api` 2142/2418,
and `xhr` 941/1435 subtests.

`cookieStore_delete_domain` no longer times out. Its two subtests already
passed, but the child frame's `result` message reached the parent twice: once
live, when the frame's posted task ran after the `load` handler attached
testharness's listener, and once in the `getmessages` replay that recovers
the messages posted before it. testharness counts one result per remote test,
so the repeat drove its pending count negative and its own timeout fired with
both subtests passing. The harness reporter now drops the repeat at the
message boundary (keyed by source and test name), so `cookiestore/` is
**38 of 48 files and 228 of 244 subtests with no timeouts**, and
`cookies` stays 336/415, `fetch/api` 2142/2418, `xhr` 941/1435, and
`resource-timing` 3 subtests, with no subtest going from pass to fail.

`cookieStore_get_set_across_frames` passes both subtests. A same-origin
iframe has no realm until the engine adopts it, so `iframe.contentWindow`
is a pending `WindowProxy` while the inserting script runs.
`cookieStore_get_set_across_frames` read `.cookieStore` through that proxy
and got `undefined`. The pending proxy now answers `cookieStore` from the
creator's realm: an initial `about:blank` inherits the creator's cookie URL,
so both objects read and write the same cookies (only the creation URL used
to validate a `url` option differs). `cookiestore/` is **39 of 48 files and
230 of 244 subtests** with 39 perfect files, and `cookies` stays 336/415.
`html/browsers/the-windowproxy-exotic-object`, `html/browsers/windows`
(65 subtests each), and
`html/semantics/embedded-content/the-iframe-element` (109 subtests) are
unchanged with no subtest going from pass to fail.

`cookieStore_in_detached_frame` passes. The test holds a removed iframe's
`contentWindow` and reads `.cookieStore` in the same task. The destroy steps
remove the tree entry synchronously, and the proxy treated every
unregistered frame as cross-origin, so the read threw. While the frame's
world is still alive (the engine drops it at the end of the turn), a
same-origin proxy now answers from that world instead. `cookiestore/` is
**40 of 48 files and 231 of 244 subtests** with 40 perfect files; only the
12 service-worker subtests and `idlharness` remain. `cookies` stays 336/415.
`html/browsers/the-windowproxy-exotic-object`, `html/browsers/windows`
(65 subtests), and
`html/semantics/embedded-content/the-iframe-element` (109 subtests) are
unchanged with no subtest going from pass to fail.

The in-process harness now answers `/resources/WebIDLParser.js` with
`resources/webidl2/lib/webidl2.js`. The alias is wptserve's
(`tools/serve/serve.py` rewrites the path), and the sidecar built the server
without that rewrite table, so every `idlharness` test 404'd the parser and
failed with `WebIDL2 is not defined`. `cookiestore/idlharness` now runs its
IDL validation subtests (16 of 17 pass) and fails only in setup, where it
registers a service worker: `navigator.serviceWorker` is still absent. The
`cookiestore` slice is **40 of 48 files and 247 of 260 subtests** (the total
grows because the validation subtests run now). `cookies` stays 336/415.

Two `dom/events` bugs are fixed. Cloning a node lost the script
already-started flag on every descendant: `Event-dispatch-bubbles-{false,true}`
clone the document element into a new `Document`, and the clone's inline
`<script>` re-ran on connection, recursively executing the page 40 times and
reporting 200 duplicate subtests. The binding clone path now copies the flag
across the materialized subtree, so both files are **5 of 5 subtests and OK**
(from `ERROR` with four duplicated passes each). Adopting a node during
dispatch panicked with `AlreadyBorrowed`: a listener that moves the
dispatching target retargets a wrapper whose class rquickjs has borrowed for
the native call. `JsNode.handle` is a `Cell`, so retargeting needs no
`borrow_mut`, and `event-global-extra.window.html` runs **4 of 6** subtests
(from `ERROR: engine panicked`). Across `dom/events`, unique passing subtests
go from 449 to 454 with none lost and 96 OK files (from 93).

No regression in the checked slices: `dom/nodes`, `dom/ranges`,
`dom/traversal`, `html/dom`, `webmessaging`, and `html/semantics/forms`
(325 files) are identical before and after with zero lost subtests;
`css/cssom` and `domparsing` (374 files) have zero lost subtests;
`cookiestore` stays 247/260 and `cookies` 336/415; `workers` is 248/248.
`Event-dispatch-click.html` times out in some batch runs on both trees; two
`form-double-submit` subtests flip between pass and fail across runs on a
single build.

CSP `script-src` gained three files and four subtests with no file lost.
`HTMLScriptElement.text` is now the child text content (text node children
only; `textContent` is the descendant text), and its setter replaces all
children with one text node, which makes `html/semantics/scripting-1`'s
`script-text.html` perfect at 6 of 6. `document.writeln` exists and appends
U+000A, so `script-src-1_2_1.html` is 2 of 2 instead of an uncaught
`TypeError`. A connected script element's child-list change now runs the
children-changed steps and re-prepares it: an empty inline script returns
from prepare before the already-started flag is set, so setting its body
later still executes it
(<https://html.spec.whatwg.org/multipage/scripting.html#script-processing-model>).
That fixes `scripthash-unicode-normalization.sub.html` (2 of 2) and
`hash-always-converted-to-utf-8/utf-8-lone-surrogate.html` (1 of 1), whose
scripts are appended empty and populated afterwards.

The binding generator now reruns when a nested implementation file changes;
`rerun-if-changed` on a directory tracks the directory entry only, so a new
trait method was invisible to it before. Evidence: `content-security-policy/script-src`
goes from 125 of 203 subtests and 78 OK files to 129 of 203 and 81 OK, with
no subtest lost; `html/semantics/scripting-1` (538 files) gains
`script-text.html`'s six subtests and loses nothing. `dom/events`,
`domparsing`, `dom/nodes`, `dom/ranges`, `dom/traversal`, `html/dom`,
`webmessaging`, `html/semantics/forms`, `cookiestore` (247/260), and
`cookies` (336/415) are unchanged. `html/semantics/scripting-1`'s
`106-import.html` / `106-module-import.html` flake in the 538-file batch
(they pass 3 of 3 in their own directory) because they race a trickling
stylesheet. `document.write` during parsing is still dropped; the remaining
`strict-dynamic` parser-inserted files depend on it.

CSP violation delivery now matches the spec's two rules. Events are queued as
tasks instead of dispatched at report time, and a violation reported while the
parser is active waits for the parser, because a parser-inserted subresource
can be blocked before the inline script that listens for it has run: the
one-shot parser stops at the previous parser-blocking `<script src>`, so the
image request is drained while the listener does not exist yet. The queue
mirrors the existing deferred image `load`/`error` events. A request violation
fires at the document and an inline violation at its element while the element
stays connected to that document
(<https://w3c.github.io/webappsec-csp/#report-violation>), so `img-src` and
`icon-blocked` events target the document while inline script and handler
events keep their element.

A `javascript:` URL's violation belongs to the initiator's document and
names the navigable's container element, not the target frame: `script-sample`
installs its listener on the `<iframe>` it navigates, and the target frame has
no listener for it. The engine now checks the initiator's policy, fires there
with the container as the violation's element, and passes only the blocking
decision to the frame that runs the script
(<https://w3c.github.io/webappsec-csp/#should-block-navigation-request>,
<https://w3c.github.io/webappsec-csp/#report-violation>).

Evidence, all subtest-level diffs against saved baselines and zero lost
subtests: `content-security-policy/securitypolicyviolation` plus
`content-security-policy/reporting` go from 70 to 76 passing subtests (of 138)
and 44 to 51 OK files, gaining both `report-original-url` cases,
`report-strips-fragment`, `report-strips-username-password`, and both
`script-sample` files; `content-security-policy/img-src` goes from 9 to 12
passing files (of 15), with `img-src-targeting.html` 2 of 2,
`img-src-none-blocks-css-background-image.html` 1 of 1, and
`report-blocked-data-uri.sub.html` 1 of 1; `content-security-policy/style-src`
adds three violation-event subtests and three OK files (48 to 51 of 86, 27 to
30 of 42); `content-security-policy/script-src` goes from 129 to 130 of 203
(`javascript-window-open-blocked.html`). The
remaining reporting timeouts need `ReportingObserver` or service workers, or
wait out a server-side 20 second "no report expected" poll that the harness
budget cannot cover. `dom/events` plus `domparsing`, the dom/forms batch,
`cookiestore` (247/260), `cookies` (336/415), and `workers` (248/248) are
unchanged; the two `form-double-submit` subtests still flip between runs on a
single build.

Parse-time handler checks close the remaining CSP delivery gap for attributes
the markup carries. HTML checks a handler content attribute when it compiles
it; this engine compiles them lazily at dispatch, so a parsed `on*` attribute
was never checked until its element dispatched an event, and
`targeting-for-inline-handler-in-markup*` waited for a click that never came.
Once the parser stops, the document now walks its connected elements and runs
the attribute check for each `on*` value, after the document's scripts
installed their listeners and before any handler runs. A handler set through
`setAttribute` also carries the setting script's `sourceFile`, line, and
column, and a remembered value keeps the invocation check from reporting the
same blocked handler twice.

Evidence, subtest diffs against the saved baseline with no subtest lost:
`content-security-policy/securitypolicyviolation` + `.../reporting` go from 76
to **82 of 138** passing subtests and 51 to **53 OK files**;
`targeting-for-inline-handler-in-markup.html` (2 of 2) and
`-in-markup-then-disconnected.html` (2 of 2) are OK, and
`targeting-for-inline-handler-on-subframe-element.html` gains both column
assertions (7 of 9). The handler tests that still fail do so only on
`lineNumber`, which needs the parser's document positions that Blitz does not
keep; the rest of the `targeting-for-inline-handler-*` files now report the
event and pass their element, blocked URI, and column assertions where they
assert them. `script-src` (130/203), `style-src` (51/86), `img-src` (17/20),
`script-src-attr-elem` (8/8), and `style-src-attr-elem` (3/7) are unchanged
against the stashed pre-change build; `dom/events` + `domparsing`,
`html/semantics/forms` (503 files), `cookiestore` (247/260), `cookies`
(336/415), and `workers` (248/248) lose nothing.

`CSSStyleRule.selectorText` has a setter. The JS layer exposed only a getter,
so every assignment in `CSSStyleRule-set-selectorText.html` and its namespace
variant silently did nothing: 62 of 82 and most of the namespace variant
failed on a stale selector. The setter parses the new selector list with the
document's selector parser, leaves the rule untouched on a parse error, and on
success reports the rule changed so the stylist re-runs the cascade
(<https://drafts.csswg.org/cssom/#dom-cssstylerule-selectortext>). A detached
rule ignores the write, as it already did for every other mutation.

Evidence, subtest diff against `cssom-after.json` with no subtest lost:
`css/cssom` goes from 2846 to **2915 of 3928** passing subtests (+69), 105 to
108 passing files; `CSSStyleRule-set-selectorText.html` goes from 20 to 78 of
82 and `CSSStyleRule-selectorText-setter.html` passes. The four remaining
selector failures are `:lang()` matching and `:first-line`/`::first-line`
serialization, which are cascade and parser gaps, not the setter: the rule
keeps the new text but matching or serializing it is wrong.

The inline `style` declaration block is now the engine's parsed block instead
of a JS shim. The shim split the `style` attribute text on `;` and echoed each
value back, so `style="background-position: 5% .5%"` read back as `5% .5%`
instead of CSSOM's `5% 0.5%`, `url(http://host/)` never gained its quotes, and
shorthands were reported as the author wrote them instead of as the longhands
the indexed getter exposes. Every read (`cssText`, `length`, `item`,
`getPropertyValue`, `getPropertyPriority`) now serializes through
`PropertyDeclarationBlock::to_css` and `property_value_to_css`, and every
mutation (`setProperty` with its priority, `removeProperty`, the `cssText`
setter) goes through the same declaration block the cascade uses. A successful
mutation rewrites the `style` content attribute with the serialized block
("update style attribute"), so `getAttribute("style")` and attribute mutation
observers see CSSOM changes; an unchanged block leaves the attribute alone,
while the `cssText` setter always writes because the spec always updates.
`setProperty` maps `null` to the empty string (removing) and leaves
`undefined` to fail parsing, and a priority other than empty or `important`
makes the call a no-op. Indexed (`style[0]`) and named (`style.color`) access,
`in`, `delete`, and enumeration come from the same block.

The style engine also gained `set_style_property_with_importance` and the
inline-block readers/writers (`inline_style_css_text`,
`inline_style_property_names`, `inline_style_get_property`,
`set_inline_style_css_text`) on `BaseDocument`, so the host bridge stays thin.

Evidence, subtest diff against the saved baseline with no subtest lost except
where recorded: `css/cssom` goes from 2915 to **3212 of 3928** passing
subtests (+297). `serialize-values.html` **533 to 673 of 697**,
`cssstyledeclaration-all-shorthand.html` **3 to 21 of 27**,
`style-attr-stray-braces.tentative.html` 0 to 10 of 16, `shorthand-values.html`
**6 to 20 of 20**, `CSSStyleDeclaration-indexed-properties.html` 0 to 6,
`variable-names.html` 0 to 6, `cssstyledeclaration-csstext.html` 4 to 10 of 11,
`cssstyledeclaration-csstext-setter.window.html` 0 to 9,
`style-background-position.html` 0 to 7, `functional-values-comma.html` 1 to 7,
`font-family-serialization-001.html` 18 to 24 of 27, and
`MutationObserver-style.html` 1 to 2.

24 subtests in `serialize-values.html` move from pass to fail, all for
`page-break-before`, `page-break-after`, `page-break-inside`, `orphans`,
`widows`, `alignment-baseline`, and `outline-color: invert`, which the Servo
`style` crate does not implement. The old shim passed them only because it
echoed the attribute text without parsing; a declaration the engine drops
must serialize to the empty string, so the test now reports that gap instead
of hiding it (recorded in `docs/upstream.md`).

No regression elsewhere: `css/css-style-attr` (15 of 16) is unchanged;
`css/cssom-view` has identical file statuses and gains 3 subtests
(`scroll-behavior-invalid.html`) with no real loss (the subtest churn there is
NOTRUN reporting inside files that crash or time out in both runs);
`dom/events` + `domparsing` gain 3 (`style_attribute_html.html`) and lose
nothing; `html/semantics/forms` (503 files), `cookiestore` (247/260),
`cookies` (336/415), and `workers` (248/248) are unchanged. `tools/check
clippy` (both workspaces), `cargo fmt --check` for the renderer, and the
renderer, wpt-harness, cookies, and browser unit tests pass.

A followed link to a fragment reloaded the page. The `Location` setter knew
that a URL differing only in its fragment is a same-document navigation, but a
link click, a form action, and every other queued navigation went straight to
a frame load, so `<a href="#fragment">` replaced the document, re-ran its
scripts, and delivered no `hashchange`. The engine now makes that decision
where every navigation converges (`Engine::navigate_frame`): a URL that
differs from the target frame's document URL only in its fragment, with a
fragment present, becomes a `FragmentNavigation` request for the frame's
realm, which moves the URL and the history mirror, records the same-document
entry, and delivers `hashchange` in its next turn.
`html/browsers/history/the-location-interface` goes from 68 to **71 of 252**
subtests with nothing lost, and `history-traversal` (24 of 87) and
`scroll-to-fragid` (6 of 24) are unchanged subtest for subtest. A page whose
script clicks the same anchor twice now navigates twice instead of reloading
in a loop.

`:target` never matched: Blitz's selector matcher answered `false` for the
pseudo-class unconditionally. The document now applies HTML's "update the
target element" for the fragment (ID first, then `a[name]`, in tree order),
clears the bit from the element the previous fragment named, and snapshots
each element before its state changes so only the rules that read `:target`
are invalidated. A document loaded with a fragment sets it at the end of
parsing, when the tree is complete.

`:has()` and `:nth-child(An+B of S)` were parsed by nothing: the style crate
gates both behind prefs that default off in this build. Turning them on is
what makes 876 of the 5784 `css/selectors` subtests match. Matching alone was
not enough: invalidation stayed stale because a relative selector depends on
the subtree below its anchor and this engine's restyle hints only flow
downwards, so an attribute, child-list, or state change below an anchor now
hints the anchor's subtree. The anchor flags live only on element, anonymous
block, and document nodes, which the walk checks.

Evidence, subtest diffs against the saved baselines with no subtest lost
outside the recorded engine gap: `css/selectors` goes from 3647 to **4528 of
5784** (+881) with the same number of OK files (384), including
`invalidation/target-pseudo-in-has.html` (1 of 1),
`nth-child-of.html` and the other `nth-child-of*` files, `has-basic.html`,
`parse-has.html`, `has-style-sharing-*`, and `has-visited.html`. The 74
subtests that still go from pass to fail are all `:has()` arguments that look
outside the anchor's subtree (`invalidation/not-pseudo-containing-complex-in-has`,
`is-pseudo-containing-complex-in-has`, `has-in-sibling-position`,
`has-sibling-insertion-removal`, `has-in-adjacent-position`); they only passed
before because `:has()` parsed to nothing and the rules never matched, so those
subtests asserted the un-styled color. Their correct values need the
relative-selector invalidation maps Servo keeps and this engine does not
(recorded in `docs/upstream.md`).

No regression in the checked slices: `css/cssom` (3212 of 3928) and
`css/cssom-view` (763) are subtest-identical, `dom/events` + `domparsing` and
`html/semantics/forms` (503 files) lose nothing, `dom/nodes` + `html/dom` +
`webmessaging` lose nothing and gain 14 (`:target` in `querySelector`),
`cookiestore` + `cookies` + `workers` (1557) are identical, and
`html/semantics/links/following-hyperlinks` is byte-identical to a run of the
same build without this change. `tools/check clippy` (renderer and blitz-dom),
`cargo fmt --check` for the renderer, and the renderer, wpt-harness, cookies,
browser, and blitz-dom unit tests pass.

`keepalive` was the next `fetch` gap: `fetch(..., {keepalive: true})` is how a
page reports on its own teardown, so the request has to survive the document
that made it. It did not. `DialLedger::terminate` runs from `Document::release`
and from every realm reset, so the teardown of the very document that queued
the request cancelled it before the next drain step started it.
`fetch/api/basic/keepalive.any.html` was 9 of 14 (every `unload` and
`pagehide` case) and `fetch/api/cors/cors-keepalive.any.html` timed out at 15
of 55.

Now a `keepalive` request leaves the ledger's ownership when its environment
goes away. A document being dropped takes its queued `keepalive` fetches out of
the realm outbox and the dial queue and starts them itself
(`Document::flush_keepalive_fetches`, called by `Engine::drop_frame_document`),
a closing worker does the same for its queue, and `DialLedger::terminate`
leaves a started `keepalive` dial running instead of running its cancellation
handle. The dial's completion reaches a channel nothing reads, which is what
outliving the document means here, so the request also leaves the engine's
"pending dials" set; an in-flight `keepalive` count in the shared state stands
in for it, and `Engine::has_pending_dials` reports it, so the renderer stays
awake until the network answers instead of reporting itself idle.

Teardown events had to exist for any of that to be reachable. A document now
runs its unload steps as trusted `pagehide` then trusted `unload`
(`PageTransitionEvent` is new), and an `iframe` removal runs them in the
child's realm before destroying its navigable. HTML's iframe removing steps say
"without any unload events firing"
(`https://html.spec.whatwg.org/multipage/iframe-embed-object.html#the-iframe-element:html-element-removing-steps`),
but every shipped engine fires them, the tests depend on it
(`https://github.com/whatwg/html/issues/10194`), and
`fetch/api/basic/keepalive.any.js` asserts the fetch that a child's `unload`
handler issues; the contradiction is recorded in `docs/upstream.md`. Closing an
auxiliary context unloads its active document and its inclusive descendant
navigables before the tree drops them
(`https://html.spec.whatwg.org/multipage/document-sequences.html#definitely-close-a-top-level-traversable`).

`contentWindow.addEventListener` was broken for every frame, not just a
pending one: a `WindowProxy` is not a platform object in the calling realm, so
the target realm's `EventTarget.prototype` member failed the interface brand
check with "incompatible receiver". The three window `EventTarget` members now
name the target frame explicitly, the way `postMessage` does, and a call made
before the target's realm exists — or while a queued navigation is about to
replace its document — waits in the writer realm's pending call log and replays
when the replacement document's realm starts, next to the `WindowProxy` writes
that already worked that way. `fetch/api/redirect/redirect-keepalive.any.html`
(0 of 6, timed out) is the acceptance test: its popup registers `unload` on a
freshly created `iframe`'s window and closes itself from the opener.

Evidence, subtest diffs against the saved baselines with no subtest lost:
`fetch/api` goes from 2157 to **2254 of 2473** (+97). `basic/keepalive.any.html`
is 14 of 14 (from 9), `cors/cors-keepalive.any.html` 95 of 95 (from 15 of 55
with a timeout), and `redirect/redirect-keepalive.any.html` 12 of 12 (from 0 of
6 with a timeout). `html/browsers` goes from 641 to **676** (+35), including
`javascript-url-no-beforeunload.window.html` 8 of 8 (from 4 of 8) — its opened
window case was unreachable while `addEventListener` threw — and 27 more in
`javascript-url-referrer` and the `same-document-traversal` files.
`html/webappapis` 1410 to **1413**, `html/semantics/forms` 4200 to **4201**.
`beacon` (76), `cookiestore` (247), `cookies` (336), `workers` (974),
`css/selectors` (4528), `css/cssom` (3212), and `css/cssom-view` (763) are
subtest-identical, and `dom/nodes` (+17), `dom/events`, `domparsing` (+3), and
`webmessaging` lose nothing. `tools/check clippy`, `cargo fmt --check`, and the
renderer, wpt-harness, cookies, and browser unit tests pass.

Fetch Metadata headers were the next `fetch` gap, and the biggest one left in
`fetch/metadata` (3 of 78 files, 345 of 1542 subtests). Two separate bugs:

`Sec-Fetch-Mode`, `Sec-Fetch-Dest` and `Sec-Fetch-User` were built once, by
whoever knew the request's mode and destination, and then travelled with the
request unchanged. "Append the Fetch metadata headers" is a per-URL step: all
of them are one group gated on the hop's URL being potentially trustworthy, so
a destination served over plain `http` must learn nothing about its requester,
and a hop that redirects `http` to `https` must carry them again
(`https://w3c.github.io/webappsec-fetch-metadata/#fetch-integration`). The net
client now captures the three headers when the request arrives and applies the
group to every hop, next to the `Sec-Fetch-Site` it already wrote per hop.

The larger bug was the parser walk. `Document::run_scripts` walks every
`<script>` in the tree and runs it as a parser script, which is the stand-in
for the missing incremental parser. It treated a script inserted by a running
script as a parser script too, and marked it started before the dynamic
preparation path could: `scripts.map(s => ...)` inserting 36 `<script>` elements
had every one of them marked started and then dropped, so the element got no
`load`, its `src` was never fetched, and `element-script.sub.html` sat at 0 of
36. The walk now takes only parser-inserted scripts
(`https://html.spec.whatwg.org/multipage/scripting.html#parser-inserted`); a
script a script connected prepares itself on insertion, as it should.

`type=module` scripts inserted by script had no path at all: preparation queued
the element, the queue read its `src` and ran it as a classic script, and an
inline module never ran. The dynamic path now shares `run_module_body` with the
parser's deferred-module queue, so both fetch the graph as a module script
request and fire `load` or `error` at the element; CSP blocks either path
before the fetch and fires `error`.

Evidence, subtest diffs against a saved baseline of the same tree with no
subtest lost: `fetch/metadata` goes from 345 to **620 of 1542**, and the
fully-passing files from 3 to **43 of 78**. `generated/element-script.sub.html`
goes 0 of 36 to 36, `element-script.https.sub.html` 1 to 36,
`script-module-import-static.sub.html` 0 to 18 (and its `.https` twin),
`element-img`, `form-submission` and `window-location` each 16 to 36, and the
`element-a`/`area`/`frame`/`iframe`/`fetch`/`header-refresh`/dynamic-import
files 8 to 18. Six subtests in `generated/css-font-face*.tentative.html`
differ, and those files are nondeterministic in this harness: three isolated
runs give 0 to 3 of 19 and 0 to 3 of 18 passes on both the old and the new
tree, so the difference is font-load timing, not these changes.

`html/semantics/scripting-1` goes from 2015 to **2037**, including
`load-error-events.html` (inline and `src` 404 for a script-inserted element)
and the `execution-timing` scheduler files. Two `106-*-module-import.html`
subtests ("stylesheets blocking ... module scripts") fail under the batch run
and pass in three isolated runs of `execution-timing`, the same load-order
flake the CSS slices show. `fetch/api` (2254), `beacon` (76), `dom/nodes`,
`dom/events`, `domparsing`, `webmessaging`, `html/browsers` (676),
`html/webappapis` (1413), `html/semantics/forms`, `cookiestore` (247),
`cookies` (336), `workers` (974), `css/selectors` (4528), `css/cssom` (3212)
and `css/cssom-view` (763) lose no subtest; `beacon` is 76 of 76 in three
isolated runs (it timed out one subtest in the batch).

Still open in `fetch/metadata`: the HTTP `Link` response header is not
processed, so `header-link.sub.html`'s `rel=icon` and `rel=stylesheet` requests
never happen; `sec-fetch-storage-access` is never sent (this engine has no
Storage Access API); `img srcset` never finishes loading, which times out
`element-picture`; and the service-worker files need a service worker.
