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

`sendBeacon()`'s `Origin` header was the next `beacon` gap. "Append a request
`Origin` header" always names the origin for a `cors`-tainted request and for
`GET`/`HEAD`, but for any other request the value can be `null`, and which
referrer policies do that is the policy's own decision: `no-referrer` always,
`same-origin` across an origin boundary, and the three downgrade-aware policies
when an `https` origin reaches a non-`https` URL
(`https://fetch.spec.whatwg.org/#append-a-request-origin-header`). The client
kept a hop's serialized origin and never consulted the policy, so
`beacon/headers/header-origin-same-origin.html` — whose document sets
`<meta name=referrer content=same-origin>` and posts cross-origin — saw
`Origin: http://web-platform.test:8000` where every engine sends `null`. The hop
now carries the request's referrer policy, and a redirect's
`Referrer-Policy` field updates it for the remaining hops.

Evidence: `beacon/headers/header-origin-same-origin.html` goes from 2 of 4 to
**4 of 4**, and the `beacon` directory from 76 to **78 of 85** at `--jobs 1` to
3 with no subtest lost (three `--jobs 1` and four `--jobs 3` runs agree).
`referrer-policy` is 8020 subtests before and after, and `fetch/api` (2254),
`xhr` (949), and `cookies` (336) are subtest-identical. The two `beacon-basic`
subtests that differ under a multi-file `--jobs 4+` batch run are a harness
flake: `beacon-basic.https.window.html` is 25 of 25 in five isolated runs and in
every `--jobs 3` run, and a six-beacon concurrency probe records each request's
own `Content-Type` consistently, so it is load, not header mix-up. Still open in
`beacon`: `headers/header-content-type-and-body.html` needs
`performance.getEntriesByName()` (Resource Timing), and `idlharness` reports
"Unexpected global object" for the `Navigator` interface.

Worker scripts had no Content Security Policy check at all. `new Worker()` and
`new SharedWorker()` handed the URL straight to the engine, so a policy that
blocked the script still let it run: `worker-src-none`'s classic and module
files all loaded the worker they said must never load. The constructor now
checks the URL against the creator's policy before anything is created. The
request carries the `worker` destination, so `worker-src` decides it, then
`child-src`, `script-src`, and `default-src`
(`https://w3c.github.io/webappsec-csp/#effective-directive-for-a-request`).
A blocked worker reports the violations at the creator's document and then
fires the plain `error` event at the `Worker`/`SharedWorker` object, which is
the shape a failed load always has
(`https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model`);
a `SharedWorker` still hands back a connected-looking port whose worker side is
gone.

The first version of the check broke `script-src 'strict-dynamic'`: it treated
every worker as a `worker-src` request, and `worker-src`'s pre-request check is
URL matching only. The directive the fallback lands on decides the check, so a
worker that falls back to `script-src` gets that list's pre-request step and a
script-created worker survives `'strict-dynamic'`, while `worker-src` and
`child-src` themselves stay URL-only. The violation still reports the canonical
directive name for the destination, not the fallback directive that decided it.

Evidence, subtest diffs against a saved baseline of the same tree with no
subtest lost: `content-security-policy` goes from 2254 to **2743 of 3999**,
and its `gen` directory from 1382 to **1872 of 2480**. Every `worker-src-none`
file passes except the four worklet files, which need a CSS worklet API this
engine lacks. `workers` (974), `html/webappapis` (1413), `fetch/api` (2254),
`cookies` (336), `cookiestore` (247) and `dom/events` (426) lose no subtest;
`beacon` is 78 of 85 in two of three isolated runs, with the third losing only
the known `beacon-basic` load flake.

Still open: a violation raised inside a worker — a static import or
`importScripts` the worker's own policy blocks — is queued in the worker realm
and dropped by its drain, so the generated `worker-import` and
`worker-import-data` files still fail their "one violation event" subtests.
The event belongs at the creator's document
(`https://w3c.github.io/webappsec-csp/#report-violation`), and the worker
already records its parent frame.

A worker's own policy was still the creator's policy container, and a
`securitypolicyviolation` raised inside a worker was queued and then dropped by
the worker drain, so the generated `worker-import` and `worker-import-data`
files reported zero events where WPT expects one.

A worker global scope's policy comes from the script response, not from its
creator; a `blob:` or `data:` script inherits the creator's container because
it has no response headers
(`https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model`).
The realm now builds its `PolicyContainer` from the response's
`Content-Security-Policy` and `-Report-Only` headers, and keeps the creator's
list separately for the script graph: the graph belongs to the outside settings
object, so the creator's policy decides it, while everything the worker body
starts later uses the response policy. That fixes the classic-worker cases in
`inside-worker/`, where a network worker previously inherited the document's
`connect-src` and now does not.

Violations raised in a worker are now forwarded instead of dropped. The worker
queues them without a node target, the engine routes them up the worker chain
(`WorkerCommand::CspViolation` for a nested worker), and the document that owns
the chain reports them, which is where every engine reports a worker's
violation even though the spec names the worker's global object
(`https://w3c.github.io/webappsec-csp/#report-violation`). A blocked
`new Worker()` still reports at the creator's document directly, as the previous
slice established.

Redirect policy was only enforced by the native browser's network service; the
in-process WPT host built its dials without the gate, so a redirect the policy
refused was followed and only the post-response check noticed. The gate now
lives in the `dial` adapter, `watch_redirects`/`refused_redirect`, and both the
browser service and the harness call it, so a refused hop yields the same
status-0 outcome everywhere.

The harness's sidecar runs wptserve on a plain loopback socket, so an `https`
test request reached a handler as `http` and a keep-scheme redirect was built
on the wrong scheme. The harness now sends the browser-visible scheme in
`X-WPT-Scheme` and the sidecar restores it before a handler reads
`request.url`.

Evidence, subtest diffs against a saved baseline of the committed tree with no
subtest lost: `content-security-policy` goes from 2743 to **2879 of 3999**.
`gen/top.http-rp/script-src-self/worker-import.http.html` and its eleven
siblings go to 12 of 12 (from 7 of 12); the `.https` twins to 11 or 12 of 12
where the mixed-content downgrade case still cannot load. `inside-worker/
dedicatedworker-connect-src.html` and `sharedworker-connect-src.sub.html` keep
their passing subtests and gain the redirect ones. `fetch/api` (2254), `xhr`
(949), `referrer-policy` (8020), `cookies` (336), `cookiestore` (247),
`html/webappapis` (1413), `html/browsers` (676), `dom/events` (426),
`css/selectors` (4528), `css/cssom` (3212), `css/cssom-view` (763) and
`workers` (974) lose no subtest. `beacon` is 78 of 85 in isolation and the
`workers/SharedWorker-extendedLifetime-*` files pass in isolation; their batch
failures are the known multi-file load flake. `html/semantics/forms` loses the
same `form-double-submit*` subtests the committed tree loses in three isolated
runs, and `html/semantics/scripting-1` gains 24 while its one lost
`execution-timing/105.html` subtest passes in three isolated runs.

`dom/events` was 28% because the event interfaces a page touches on every
keypress and animation were only partly exposed. The handler list was a hand
picked subset of `GlobalEventHandlers`: `onanimationend`,
`ontransitionend`, `onpointerdown`, `onscrollend`,
`onsecuritypolicyviolation`, `onslotchange` and thirty others read
`undefined`, and `KeyboardEvent.which`, `AnimationEvent` and `TransitionEvent`
did not exist. The list now comes from the interface mixins the IDL declares,
including the partials (CSS Animations, CSS Transitions, Pointer Events, Touch
Events, Selection, Scroll Snap), and `WindowEventHandlers` gained the four
names it was missing.

Three conversion rules came with it. An event handler IDL attribute converts
through a callback type marked `[LegacyTreatNonObjectAsNull]`, so assigning a
string clears the slot instead of storing a non-callable value; the tests
assert `element.onclick = ""` reads back null
(`https://webidl.spec.whatwg.org/#LegacyTreatNonObjectAsNull`). The four legacy
`webkit` animation and transition event types keep their camelCase spelling
for `addEventListener` but map to all-lowercase handler properties, so a
dispatched `webkitAnimationEnd` reaches `onwebkitanimationend` and not
`onwebkitAnimationEnd`. A `body` or `frameset` element forwards every
`WindowEventHandlers` attribute, plus the six `GlobalEventHandlers` in the
window-reflecting set, to its document's window; a body whose document has no
browsing context (one made by `DOMParser` or `new Document()`) reads null and
ignores writes (`https://html.spec.whatwg.org/multipage/webappapis.html#window-reflecting-body-element-event-handler-set`).

`AnimationEvent` and `TransitionEvent` now exist with the spec members
(`animationName`, `propertyName`, `elapsedTime`, `pseudoElement`), and
`KeyboardEvent.which` reports an explicitly constructed value, then `keyCode`,
then `charCode`
(`https://drafts.csswg.org/css-animations-1/#interface-animationevent`,
`https://drafts.csswg.org/css-transitions-1/#interface-transitionevent`,
`https://w3c.github.io/uievents/#dom-keyboardevent-which`).

Evidence, subtest diffs against saved baselines of the committed tree with no
subtest lost: `dom/events` goes from 426 to **488 of 582**, and
`html/webappapis` from 1413 to **1885 of 2254**. The four `webkit-*-event`
files pass their property and dispatch subtests; what still times out is every
subtest that waits for an engine-fired animation event, because the engine has
no animation engine (`docs/limitations.md`). `html/webappapis` gains the
`onpointerdown`/`onscrollend`/`onslotchange` handler reflection and the
windowless-body forwarding subtests. `webmessaging`, `html/browsers`,
`dom/nodes`, `domparsing`, `workers`, `css/selectors`, `css/cssom`,
`css/cssom-view`, `fetch/api`, `xhr`, `cookies`, `cookiestore` and
`referrer-policy` are subtest-identical. `html/dom` loses three render-blocking
subtests against an older saved report but the committed tree scores the same
9 of 38 on those files in two isolated runs, so the loss predates this slice.
`cargo fmt --check`, `tools/check clippy`, and the renderer (172), wpt-harness
(48), cookies (26), browser (69+4), net (52) and dial (29+3) unit tests pass.

`dom/events` also missed the DOM rule that ties listener invocation to the
target's global object. An `EventTarget` created in an `iframe` kept running
its listeners after the `iframe` was removed: removing the frame removes its
browsing context, so the document is no longer fully active, and inner invoke
must skip the listener
(`https://dom.spec.whatwg.org/#concept-event-listener-inner-invoke`). The
engine dispatched the event exactly as before, so a listener added inside a
removed frame still ran, a listener that removed the frame did not stop the
rest of the list, and abort listeners on an `AbortSignal` from the removed
frame still ran.

Invoke now asks whether the path item's target global is a document realm with
a browsing context before setting the phase and again before each listener, so
a listener that removes the frame skips the remainder, and handler attributes
run under the same check. The target global is re-derived per listener: a node
follows its current node document, so a listener that adopts the target into a
removed `iframe`'s document stops the remaining listeners, while the same
adoption into a live document does not. The engine's "fully active" proxy is
frame liveness, which is what removing an `iframe` changes. The dispatch itself
still happens: `event.target` is set, `dispatchEvent()` still returns true,
`defaultPrevented` stays false, and `currentTarget`/`eventPhase` end at
null/NONE. A worker global is not a `Window`, so worker events are unaffected.

Evidence: `dom/events/event-listener-not-fully-active-target.html` goes from
4 of 16 to **16 of 16**, `event-listener-not-fully-active-adopted-node.html`
from 4 of 5 to **5 of 5**, and `dom/events` from 426 to **475 of 582**.
`html/webappapis` is 1886 of 2254; `webmessaging` (189), `html/dom` (63975),
`html/browsers` (676), `dom/nodes` (14344), `domparsing` (543), `workers`
(974), `css/selectors` (4528), `css/cssom` (3212), `css/cssom-view` (763),
`fetch/api` (2254), `xhr` (949), `cookies` (336), `cookiestore` (247) and
`referrer-policy` (8020) lose no subtest. `cargo fmt --check`,
`tools/check clippy`, and the renderer (172), wpt-harness (48), cookies (26),
browser (69+4), net (52) and dial (29+3) unit tests pass.

`html/semantics/forms` had the text selection APIs but never fired their
`select` event. The setters stored a selection silently, so
`textfieldselection/selection-start-end.html` timed out waiting for a
`select` after `selectionStart` changed, and
`textfieldselection/select-event.html` never got past its first subtest:
`select()` changed the selection but dispatched nothing, a promise test waited
forever, and the harness reported all 270 subtests as a timeout.

The "set the selection range" algorithm is now one host operation
(`https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#set-the-selection-range`).
It clamps both edges to the relevant value, collapses `end` at or before
`start` to a caret at `end`, normalizes an unknown direction to `none`, and
queues the `select` event only when the extent or direction actually changed,
which is why a second `select()` on an already selected control fires nothing.
The event is queued through the realm outbox as a document task, so it runs
after the assignment returns; `isTrusted` is true, `bubbles` is true, and
`cancelable` is false. The JavaScript shim's `select()`, `setSelectionRange()`
and `setRangeText()` all call the same host operation, so a call cannot fire
one event per property write.

Evidence, subtest diffs against saved baselines of the committed tree with no
subtest lost: `html/semantics/forms` goes from 4198 to **4512 of 4768**.
`textfieldselection/select-event.html` goes from 0 to **270 of 270**,
`textfieldselection/textfieldselection-setRangeText.html` from 65 to
**88 of 88**, `textfieldselection/selection-start-end.html` from 27 to
**45 of 45**, and `textfieldselection/textfieldselection-setSelectionRange.html`
stays 49 of 49. `dom/events`, `html/webappapis`, `webmessaging`, `html/dom`,
`html/browsers`, `dom/nodes`, `domparsing`, `workers`, `css/selectors`,
`css/cssom`, `css/cssom-view`, `fetch/api`, `xhr`, `cookies`, `cookiestore` and
`referrer-policy` lose no subtest; `xhr/formdata/constructor-submitter.html`
times out in an isolated run of the committed tree too, so its drop from a
batch run is the existing hang. `cargo fmt --check`, `tools/check clippy`, and
the renderer (172), wpt-harness (48), cookies (26), browser (69+4), net (52)
and dial (29+3) unit tests pass.

`CSS.supports()` existed only as a stub that returned `false`, so every test
that gates an assertion on it skipped silently. Both forms now parse through
the style crate: the two-argument form parses the property with
`PropertyId::parse_enabled_for_all_content` and the value with
`parse_one_declaration_into`, and the one-argument form parses and evaluates a
supports condition with `parse_condition_or_declaration` plus `eval`
(<https://drafts.csswg.org/css-conditional-3/#dom-css-supports>). A custom
property accepts any non-empty token stream because the same declaration
parser does; the empty string is rejected explicitly, because the declaration
parser treats it as a valid empty value and the spec's change log requires
`false` for it (Chromium answers `false` for `CSS.supports("--x", "")` and
`true` for `" "`). A property name with surrounding whitespace is invalid
because the name is one ident, while the value parser skips its own outer
whitespace. Those answers, and `--x: red` as a name, match Chromium. The two
overloads share one shim function, so Web IDL's shortest-argument rule gives
`CSS.supports.length` 1.

Evidence, subtest diffs against baselines of the committed tree, none lost:
`css/cssom/CSS-supports.html` goes from 2 to **11 of 11**, `css/selectors` from
4528 to **4689 of 5784** (the `parse-*` files, one per non-type selector,
including `parse-is-where.html` 26, `parse-has.html` 23 and `parse-not.html`
19), `css/cssom` from 3212 to **3220** passing subtests, `css/css-variables`
from 419 to **437**, `css/css-cascade` from 195 to **208**, `css/css-align`
from 2682 to
**3236 of 5311** and `css/css-position` from 237 to **882 of 1501**. The
`css/selectors` gains are all in files the suite could not evaluate before.
`focus-visible-017-2.html`, `-017` and `-020` now pass the
`CSS.supports("selector(:focus-visible)")` validity subtest that
`parsing-testcommon.js` runs and fail only on the UA focus ring that is still
missing.

One test lost a pass because the assertion loop it was skipping now runs:
`css/cssom/font-variant-shorthand-serialization.html` "font: menu serialization"
is 6 of 7. It passed before only because `CSS.supports("font-variant-ligatures:
initial")` was false and `test_serialization_set` skipped every assertion. The
block stores the system font's `font-variant-*` longhands at `normal`, where a
browser keeps the `font` shorthand and reports `""`
(`docs/limitations.md`, `docs/upstream.md`).

The CSSOM's declaration block disagreed with `CSS.supports()`: `'color' in
element.style` asked whether the block *currently sets* the property, so an
empty block answered false for every supported property. CSSOM gives a
`CSSStyleDeclaration` an IDL attribute for each supported property, set or not
(<https://drafts.csswg.org/cssom/#css-property-to-idl-attribute>), so `in`, the
IDL getter, and `CSS.supports()` now ask one host predicate,
`css::supports::property_supported`. An unknown name reads `undefined`, as in a
browser, and `style.webkitAnimation` resolves to `-webkit-animation`, the
webkit-cased IDL name that drops the leading dash.

The parsing moved to `crates/renderer/src/css/supports.rs`, the concept owner
for CSS values shared with canvas and form controls, and `css::url_data` /
`css::parser_context` replaced the second and third copies of the standalone
parse context (`css/color.rs` was the first). The one-argument
`CSS.supports()` now runs both spec steps: the text as a
`<supports-condition>`, then the text in parentheses, which is how a bare
declaration is accepted and how a browser answers true for
`CSS.supports("color: red) or (color: blue")`. Arguments convert through
`__tbToDOMString`, so a symbol throws, and the `CSS` namespace is an ordinary
object with writable, configurable members and a `Symbol.toStringTag` of
`"CSS"`.

Evidence, subtest diffs against the previous commit's build with no subtest
lost: `css/css-conditional` 837 to **1647 of 1851**, with
`js/CSS-supports-CSSStyleDeclaration.html` 685 to **1495 of 1495**;
`css/css-align` 3236 to **3422 of 5311**; `css/css-position` 882 to
**945 of 1501**; `css/cssom` 3220 to **3226**; `css/cssom-view` 763 to
**767**. Every other suite is subtest-identical. The parsing fixes (wrap,
symbol, namespace attributes) have no WPT coverage; each answer was checked
against Chromium 157.

A rule's declaration block (`cssRules[i].style`) still has no property IDL
attributes, and `for...in` over a declaration block enumerates only its
indexed entries (`docs/limitations.md`).

The 24 `css/cssom/serialize-values.html` subtests that the inline-style rewrite
lost are settled as an accepted engine gap. The style crate has no
`page-break-*`, `orphans`, `widows`, `alignment-baseline`, or
`outline-color: invert` property, so a declaration naming one is a parse error
and the block serializes the empty string. The pre-`715bfd17` shim passed those
subtests by echoing the attribute text, which hid the gap rather than modelling
it; a browser that lacked the properties would report `""` too. Recorded in
`docs/limitations.md`; teaching the style crate the properties belongs
upstream.

`css/cssom-view` scrolling is wired to Blitz's scroll engine. The renderer now
answers `Document.scrollingElement` with the CSSOM View algorithm: in quirks
mode the body element is the answer unless the body is potentially scrollable
(its own overflow and its parent's overflow each create a scrolling box), and
otherwise the root element is (`crates/renderer/src/js/bindings/node/
scrolling.rs`). `Element.scrollTop`/`scrollLeft` read and write the element's
own scroll offset through `Node::scroll_offset`/`BaseDocument::scroll_to`
instead of pretending only the viewport scrolls, while the root element and the
quirks-mode body still address the viewport. `Element.scroll`/`scrollTo`/
`scrollBy` and their `Window` counterparts are shims over the same host calls;
the two IDL overloads share one dispatcher because the binder emits exactly one
implementation per operation name. The window gained live `scrollX`, `scrollY`,
`pageXOffset`, and `pageYOffset`. `scrollIntoView` now maps its
`ScrollIntoViewOptions` (or a boolean) onto Blitz's block/inline alignment
instead of ignoring the argument, so `scrollIntoView({block: "center"})` and
`scrollIntoView(false)` differ.

Evidence, subtest diffs against the commit before the slice, 239 gained and one
lost: `css/cssom-view` 767 to **1006 of 1758**. `scrollingElement.html` 0 to
**8 of 8**; `scrollintoview.html` 0 to **40 of 40**; `element-scroll-
arguments.html` 0 to **12 of 12**; `elementScroll.html` 0 to **8 of 8**;
`elementScroll-002.html` 0 to **4 of 4**; `dom-element-scroll.html` 0 to
**4 of 4**; `scroll-behavior-element.html` 0 to **13 of 39** (the instant and
auto cases); `scrolling-quirks-vs-nonquirks.html` 8 to **16 of 30**;
`scroll-behavior-main-frame-root.html` and `-window.html` each gain one subtest.
The single lost subtest is `scrolling-quirks-vs-nonquirks.html`'s
"scrollLeft/scrollTop on the root element in non-quirks mode": the previous
setter stored the requested offset without clamping, so it returned 70 on a
viewport Blitz considers unscrollable (a subframe laid out at the tab's 800px
width, and a body whose `overflow: hidden` does not propagate to the viewport);
the spec clamps instead. Both causes are recorded in `docs/limitations.md` and
`docs/upstream.md` rather than asserted by a special case.


The scrolling slice's review found four conversion and routing mistakes and
three structural ones, all folded in. The two-argument `scroll(x, y)` overloads
now convert each argument with ToNumber (a numeric string and `null` work, a
BigInt rejects), because Web IDL picks the overload by argument count and then
converts as `unrestricted double`; the shim had required `typeof number`.
`scrollBy` normalizes non-finite values before adding them to the current
position, so `scrollBy({top: NaN})` no longer jumps to zero. A receiver check
now rejects a misused method (`Element.prototype.scroll.call({})`,
`window.scroll.call(element)`) and, because these methods return promises, it
rejects instead of throwing; a cross-frame `contentWindow` proxy is accepted
through its self-referential `window`. The `[Replaceable]` viewport attributes
(`innerWidth`, `innerHeight`, `outerWidth`, `outerHeight`, `scrollX`,
`scrollY`, `pageXOffset`, `pageYOffset`) are enumerable and assigning one
replaces it with an own data property, as Web IDL requires.

The structural findings: the four host functions moved from the already large
`bindings/mod.rs` into `node/scrolling.rs`, where the algorithms live; the
element and window paths now share one active-document check that follows the
node's owning realm (so a same-origin iframe's `documentElement.scrollTop`
works when read from the parent, and a `DOMParser` document reports zero); and
the `clip`-on-the-parent-counts-as-`hidden` override now applies only to
`scrollingElement`, because the scroll getters and setters use the
`potentially scrollable` definition as written. `JsNode::is_document_element`
reuses the shared root-element helper.

Two spec readings changed. `Element.scroll()` on the root element is specified
to call `window.scroll(scrollX, y)`, discarding x, but Chrome applies both
coordinates and `scrolling-quirks-vs-nonquirks.html` asserts the browser
behavior, so tinybrowser follows the browser and records the disagreement in
`docs/upstream.md`. The `container` member of `ScrollIntoViewOptions` is parsed
and ignored, which the scrolling limitation now says.

A probe page confirms each fix: numeric-string and `null` arguments scroll,
BigInt and both brand failures reject with `TypeError`, `scrollBy({top: NaN})`
does nothing, `scrollBy({left: 10})` moves one axis, `scrollX` is enumerable
and replaceable, and a same-origin iframe's document is scrollable from the
parent realm. `css/cssom-view` stays at **1002 of 1758** against 767 before the
slice, with the same single loss (`scrolling-quirks-vs-nonquirks.html`, whose
scroll assertions depend on the frame-viewport and overflow-propagation gaps in
`docs/limitations.md`).

`Element.checkVisibility()` now answers the CSSOM View algorithm
(`crates/renderer/src/js/bindings/node/element.rs`). It returns false for an
element without a box (`display: none`, `display: contents`, a detached
element, or a document without a browsing context), walks the ancestors for a
`display: none` ancestor, and then applies the option gates:
`opacityProperty`/`checkOpacity` fail when the element or an ancestor has
computed `opacity: 0`, and `visibilityProperty`/`checkVisibilityCSS` fail when
the element is known to be invisible. The two option names in each pair are the
spec's aliases. `visibility` is inherited, so the element's own computed value
already reflects an ancestor unless a descendant re-declares
`visibility: visible`.

Evidence: `css/cssom-view/checkVisibility.html` 0 to **10 of 15**. The five
remaining failures all need `content-visibility`, which the pinned Servo style
crate does not generate (`engine = "gecko"`), so a hidden ancestor and a
skipped `content-visibility: auto` subtree cannot be detected; the gap is
recorded in `docs/limitations.md` and `docs/upstream.md`. The passing ten cover
`visibility: hidden`, `display: none` on the element and on a shadow host,
`display: contents`, `opacity: 0` on the element and on a shadow host, and the
aliased option names.

Review folded into the slice. The ancestor walk now uses the shared
`dom/tree.rs` `shadow_including_parent`, so a shadow-tree descendant sees a
`display: none` or `opacity: 0` host (a probe verified both against Chrome's
answers; without the host link both reported visible). The walk is
shadow-including rather than the flat tree, because Blitz has no slot
assignment; that and the unstyled shadow-scoped cascade are recorded in
`docs/limitations.md`. The check now requires the element's document to be the
active document of the realm that owns it (`active_world`, moved next to
`owning_world` for the scrolling and visibility callers), so a `DOMParser`
element reports false instead of inheriting a box from the parser's layout.
One walk now serves both the `display: none` and `opacity` gates, the option
locals are named for the request rather than the check, and the visibility gate
fails only when a computed value says invisible (an unstyled node is not
evidence). The binder learns the `clippy::struct_excessive_bools` shape of a
WebIDL dictionary: a dictionary with more than three plain `bool` fields gets a
reasoned `#[expect]`, and `dictionary_field_is_bool` is shared with the field
emitter so the count cannot drift from the type. The dictionary contract test
now asserts the attribute on a four-bool dictionary.

The CSSOM View element metrics are implemented: `HTMLElement.offsetParent`,
`offsetLeft`, and `offsetTop`, and `offsetWidth`/`offsetHeight` now share one
offset rectangle (`crates/renderer/src/js/bindings/node/element.rs`).
`offsetParent` walks the shadow-including ancestors for the nearest containing
block, the body rule, a static `td`/`th`/`table`, and the fixed-position
containing-block rule; `offsetLeft`/`offsetTop` read Blitz's
`BaseDocument::offset_rect`, which measures from the offset parent's padding
edge and handles the first fragment box of a non-atomic inline. `offsetWidth`
and `offsetHeight` no longer read the layout size directly, so an inline
element with several line boxes reports its first fragment's union rather than
zero.

Evidence: `css/cssom-view` 1012 to **1070 of 1758** against the checkVisibility
run, with **0 lost subtests** and no dropped files. `offsetTopLeft-border-box`
gains all 28, `offsetTopLeftInScrollableParent` 8, `offsetParent-body-and-html`
6, `table-offset-props` 4 (1 to 5), `detached-element-geometry.window` 3,
`subpixel-sizes-and-offsets.tentative` 3 (6 to 9), `offsetParent-fixed` 2,
`offsetTopLeft-trailing-space-inline` 1, `offsetParent-block-in-inline` 1, and
`offsetParent_element_test` 1 of its 2.

Three rules came from the spec and the tests disagreeing with the renderer.
Every `body` element answers null for `offsetParent` and 0 for
`offsetLeft`/`offsetTop`, not only `document.body`: `offsetParent-body-and-html.html`
requires it and Chrome agrees (probed with nested bodies), so the body rule
tests the element's tag, not document identity. `transform-style: preserve-3d`
establishes a containing block for all descendants even though the layout
crate's predicate omits it, so the offset walk adds the rule locally and
records that Taffy's hoisting does not, which leaves layout and `offsetParent`
disagreeing inside such an ancestor (`docs/upstream.md`). `scrollParent` stays
unimplemented: the draft declares an attribute and the pinned tests call a
method, and no browser ships either, so there is nothing to match yet.

Review folded into the slice. The box rule (`display: none` on the element or an
ancestor, `display: contents`) is one predicate shared with `checkVisibility`,
so a descendant of `display: none` answers 0 and null for the offset metrics
instead of reporting a box, matching Chrome (probed). The algorithms moved to
`js/bindings/node/offset.rs`, beside `scrolling.rs`, and
`shadow_including_ancestors` moved next to `shadow_including_parent` in
`dom/tree.rs`; the file also reuses `dom/tree.rs`'s `is_html_tag` instead of
carrying its own tag tests. `offsetLeft`/`offsetTop` run one layout pass
instead of two, and the empty-inline fallback sits beside the sibling scan it
uses. The fixed-position containing-block branch now applies only to a
`position: fixed` element whose containing block is an ancestor, as the spec's
wording ("the element is in a fixed position containing block") reads and as
Chrome answers for a static element inside a transform (probed); Chrome does
not apply it to a fixed element with a nearer positioned ancestor, and that
deviation is recorded in `docs/upstream.md`. A `display: contents` ancestor and
a non-atomic inline ancestor no longer count as containing blocks, matching
Chrome and the layout crate's `inline_containing_block_claims` (transforms and
containment do not apply to non-atomic inlines). Blitz's `offset_rect` now
reports a non-atomic inline's first fragment as the origin and the union of its
fragments as the size, so a multi-line inline's `offsetLeft`/`offsetTop` match
Chrome's first-line answer while `offsetWidth`/`offsetHeight` still span every
line; the fork change is recorded in `docs/upstream.md`.

The remaining gaps are recorded in `docs/limitations.md`: table captions are
not laid out, CSS `zoom` is not applied, `writing-mode` is disabled by its
Servo pref and the off `blitz-dom` feature, the empty-inline position is a
sibling guess, and the fork's `offset_top_left` fixed-position-containing-block
table does not match `offsetParent`. Three suites that read offset geometry
changed as a side effect of reporting real values: `css/css-align` gains
subtests that assert `content.offsetTop` (and loses the `display: ruby-text`
probe, a Gecko-only display), `css/css-position`'s sticky tests now expose that
sticky positioning has no layout offset, and `html/browsers`'
`open-features-tokenization-noopener.html` errored with "script failed" in the
battery run; a rerun of the file at the same build errors the same way, and the
file is in the known `window.open` tokenization flake family.

Vertical `writing-mode` now lays out. Blitz's support was off in two places: the
style crate gates the property behind the `layout.css.writing-mode.enabled`
Servo pref, and `blitz-dom` gates its per-box writing-mode layout behind the
`writing-mode` Cargo feature. The fork's `init_stylo_prefs` already set the
prefs when the feature is on, so the change is one line in
`crates/renderer/Cargo.toml`; the property now parses, computes
(`getComputedStyle`, `CSS.supports`, the inline style), and drives layout in
each box's writing-mode frame.

Evidence: `css/cssom-view` 1070 to **1475 of 1758** in an isolated run (**1465**
in the 21-suite battery, whose smooth-scroll cases lose subtests to the harness
budget). The battery against the offset run has no other lost subtests and no
dropped files; the flagged rows are the known flakes
(`scroll-behavior-subframe-window.html` times out under `--jobs 8` but runs
14 of 28 isolated, the same as the three earlier runs;
`broadcastchannel.window.html`'s bfcache probe oscillates between PASS, FAIL,
TIMEOUT, and SKIP across the pre-slice runs, and reported SKIP here).
`scrollWidthHeight-negative-margin-002.html` goes 200 to **600 of 600** (it
runs its margin matrix in every writing mode), `scrollLeftTop.html` gains its
three vertical cases (3 to 6), and `offsetTopLeft-trailing-space-inline.html`
goes 1 to **3 of 3**, because the empty-inline fallback's writing-mode side is
now observable. A probe confirms `vertical-lr` computes and a 100x200 box
reports `clientWidth` 100 and `clientHeight` 200.

The `scrollIntoView-*-writing-mode.html` family stays at 0 of 9: its boxes are
now vertical, but `scrollIntoView` still scrolls only the viewport, not the
nested scroll container the tests set up (`docs/limitations.md`). That is the
scrolling gap, not a writing-mode one.

`window.screen` and the remaining client metrics are implemented. The screen
follows the CSSOM View model: there is no physical display or browser chrome,
so the web-exposed screen and its available area are the viewport, and the
color depth is 24-bit (`crates/renderer/src/js/scripts/web/cssom.js`). The
`Screen` interface is installed with a throwing constructor, enumerable
prototype getters, and a `[SameObject, Replaceable]` `screen` attribute.
`Element.clientLeft`/`clientTop` return the used border on each axis, and zero
for an element without a box or a non-atomic inline
(`crates/renderer/src/js/bindings/node/element.rs`).

Evidence: `css/cssom-view` 1475 to **1501 of 1758** with no lost subtests, and
216 to **218 files** passing. The five screen files all pass fully
(`cssom-view-window-screen-interface` 6, the width and height range files 3
each, both immutability files 1 each), `subpixel-sizes-and-offsets.tentative`
goes 9 to **12 of 15** (the three `clientLeft`/`clientTop` rounding cases),
`table-client-props` 1 to **4 of 15** and `table-scroll-props` 1 to **4 of 13**
(their `clientLeft` cases; the rest await caption layout), and
`detached-element-geometry.window` and `outer-svg` gain their client-edge
cases. What still fails is the `input`/`textarea` inline-padding rule from
csswg-drafts#13502 and the left-hand scrollbar side, both recorded in
`docs/limitations.md`.

`scrollIntoView` now scrolls the scrolling boxes that can actually move the
target. Blitz's `scroll_into_view` aligned the target against the viewport, so
a target inside a nested scroller left that scroller alone, and it read its
block and inline arguments as vertical and horizontal. It now walks the
target's containing block chain innermost to outermost, scrolls each ancestor
that establishes a scrolling box, then the viewport, and maps each logical
position through that box's writing mode
(`third_party/blitz/packages/blitz-dom/src/scrolling.rs`).

Evidence: `css/cssom-view` 1501 to **1578 of 1758**. The nine
`scrollIntoView-*-writing-mode` files were all 0 of 9; seven now pass fully
(every mode except `sideways-lr`), `scroll-behavior-element` gains 4 (15 to
19), `scrollIntoView-container` 4, `scrollIntoView-fixed` 2, and
`scrollintoview-containingblock-chain`,
`scrollIntoView-scrolling-box-with-large-border`,
`scrollIntoView-nearest-oversized-element`,
`scrollIntoView-horizontal-partially-visible`, and
`scrollIntoView-align-scrollport-covering-child` 1 each, with
`scrollintoview.html` still 40 of 40.

The containing block chain, not every DOM ancestor, is the walk: a scroller
outside an absolutely positioned box's containing block does not move the box
(`scrollintoview-containingblock-chain.html`). Two boxes are skipped because
they cannot move the target: a `body` whose overflow propagates to the viewport
has no scrolling box of its own, and a `position: fixed` box hoisted to the
viewport stops the chain. Without the first, `scrollintoview.html`'s padded
body scrolled twice (40 of 40 fell to 32 of 40 while the walk was being
written); without the second, `scrollIntoView-fixed.html` scrolled the window
under a fixed box. The `container` option's `"nearest"` stops after the nearest
scroller, which is `ScrollIntoViewContainer` in the scroll engine.

Two failures are layout, not scrolling, and are recorded in
`docs/upstream.md`. `sideways-lr` places floats along the block axis instead
of the inline axis (probed against Chrome with the test's own markup), so the
two `sideways-lr` files keep their layout-dependent expectations; the scroll
mapping itself treats them like the other vertical modes. `position: sticky`
has no layout offset, so `scrollIntoView-stuck.tentative.html` reads the stuck
input at its static position and scrolls the container back to the top. That
is the one lost subtest against the previous run, and it is the documented
sticky gap rather than the scroll walk; its sibling
`position-sticky-right.html`/`-bottom.html` already moved for the same reason.
`scroll-margin`/`scroll-padding` are unavailable rather than unimplemented:
both are `engine = "gecko"` longhands in the pinned Stylo, so this build does
not generate them (probed: `CSS.supports` is false for both).

A child frame now lays out at its iframe's content box.
`Engine::sync_child_frame_viewports` reads the container's laid-out content box
after the parent resolves and calls the child document's `set_viewport` when
the size differs (`crates/renderer/src/engine.rs`). Before this, every child
frame used the tab viewport: `window.innerWidth`, media queries, and scroll
ranges inside an iframe were the tab's, so a test that sizes an iframe to
300px and scrolls it could not match.

Evidence: `css/cssom-view` 1578 to **1610 of 1758**, no lost subtests.
`MediaQueryList-addListener-removeListener` 1 to 8 of 8,
`MediaQueryList-extends-EventTarget-interop` 1 to 8,
`MediaQueryList-extends-EventTarget` 1 to 7,
`MediaQueryList-addListener-handleEvent` 0 to 3,
`MediaQueryList-change-event-matches-value` 0 to 1, `matchMedia` 6 to 7,
`matchMedia-display-none-iframe` 1 to 2, `scrollIntoView-iframes` 1 to 3,
`scrollIntoView-container` 4 to 5 (the iframe subtest now passes), and
`scrolling-quirks-vs-nonquirks` 16 to 19 (the non-quirks root and body
geometry now uses the 300x500 iframe box instead of the tab's 800x600).

The `MediaQueryList` shim also stopped registering `onchange` as an event
listener: the dispatcher already calls a target's `on<type>` property after the
listener list, so a handler assigned through the attribute fired twice
(`MediaQueryList-extends-EventTarget.html`, "onchange removes listener"). The
slot is now the only registration, and a non-callable value becomes null as
event handler IDL attributes do.

The CSSOM View shims had to stop routing a scroll through a `body` whose
overflow propagates to the viewport: `scroll_position` reads that body's used
overflow as `visible` and returns zero, and `scroll_to` refuses to move it. The
refusal is on the element's own used overflow only; CSSOM's parent-overflow
condition in "potentially scrollable" answers the `scrollingElement` and
quirks-body questions and must not gate `scroll()` on an ordinary scroller
inside a `body`, or every scroller in `scrollLeftTop.html` freezes.

`MediaQueryList` change events are now `MediaQueryListEvent`s. The shim
constructs the interface from the captured `Event`, stores `media` and
`matches` in a host slot, and dispatches it from `report_media_changes`
(`crates/renderer/src/js/scripts/web/cssom.js`); before this, the shim created
a plain `Event` and copied the two members onto it. Evidence:
`css/cssom-view` 1610 to **1616 of 1758**, no lost subtests;
`MediaQueryListEvent.html` 0 to 6 of 6, which also covers the event an
`addListener` callback and an `onchange` handler receive.

`IntersectionObserver` now reports real intersections. The shim was a stub:
every observation claimed `isIntersecting: true`, `intersectionRatio: 1`, the
target's full rect for both rects, and `rootBounds: null`, so any page that
used the result to defer work saw every target as visible.

The update now runs per animation frame
(`crates/renderer/src/js/scripts/web/observers.js`). It computes the target's
border box, clips it by each ancestor's content clip (per axis, so
`overflow-x: visible` with `overflow-y: clip` leaves the horizontal axis
unclipped), intersects it with the root intersection rectangle, and derives
`isIntersecting` and `intersectionRatio` with the spec's edge-adjacent rule.
The implicit root is the viewport, an element root uses its padding area when
it is a scroll container and its border box otherwise, and a `Document` root
uses that document's viewport. `rootMargin` parses one to four lengths or
percentages (percentages against the corresponding root axis), thresholds are
validated, sorted, and deduplicated, and `IntersectionObserverEntry` is a real
prototype with the rects, `isVisible`, `target`, and `time`.

Evidence: the `intersection-observer` suite goes from **115 to 185 of 232**
subtests, with no lost subtests. `multiple-thresholds` 1 to 10,
`observer-exceptions` 0 to 9, `containing-block` 1 to 5, `multiple-targets`
2 to 5, `observer-attributes` 6 to 8, and the root-margin, zero-size,
document-root, and `isIntersecting`-change files each gain. The one v2
occlusion case the stub passed by accident
(`position-absolute-overflow-visible-and-not-visible`) is restored by the
per-axis clip.

`ResizeObserver` now exists. It was missing entirely (`ResizeObserver is not
defined`), so a component that sized itself from its own box never received an
observation.

The shim measures the content box, border box, and device-pixel content box
once per frame and delivers only the boxes whose size changed, plus the initial
observation (`crates/renderer/src/js/scripts/web/observers.js`). An entry
carries `contentRect` (the content box relative to the padding box, so its
`x`/`y` are the padding offsets), the logical `contentBoxSize` and
`borderBoxSize`, and `target`. Observed boxes are content-box by default, a
second `observe()` with a different box replaces the first, and callbacks that
resize are measured again in the same frame up to five passes before the
loop-error event is dispatched. The frame loop stops after 20 idle frames so a
reftest settles.

Evidence: `resize-observer` goes from **5 to 50 subtests** and 14 to 33
passing files, with no lost subtests. `observe-001` through `observe-015` pass
(content, border, and device-pixel boxes, padding, box sizing, overriding),
plus `observe-017`, `observe-019`, `ordering`, `scrollbars-2`,
`calculate-depth-for-node`, `observer-in-cross-origin-frame`, and
`multiple-observers-with-mutation-crash`; `notify.html` passes 14 of 15.

The remaining failures are layout gaps, recorded in `docs/limitations.md` and
`docs/upstream.md`: Blitz keeps a block's size when its `display` becomes
`inline` (so no notification is due), layout rounds fractional sizes
(`notify.html`), there is no scrollbar gutter, CSS `zoom` and SVG boxes are
approximate, and the resize loop's depth-limited delivery order and
`window.onerror` lifecycle are not modelled.

`customElements.define()` now runs the definition algorithm's checks. The name
validation accepts only a lowercase hyphenated name outside the reserved SVG
set (`annotation-xml`, `color-profile`, `font-face*`, `missing-glyph`), and the
constructor is read in order: `prototype`, the five reaction callbacks
(including `connectedMoveCallback`, which this engine supports),
`observedAttributes` when `attributeChangedCallback` exists, `disabledFeatures`,
`formAssociated`, and the four form-associated callbacks when it converts
true. `observedAttributes` and `disabledFeatures` go through Web IDL
`sequence<DOMString>` conversion, so a non-iterable value or an iterator that
throws surfaces its own error (`crates/renderer/src/js/scripts/web/custom_elements.js`).

Evidence: `custom-elements` 2355 to **2383 subtests**, no lost subtests.
`CustomElementRegistry.html` 32 to 44 of 46, `registries/define.html` 56 to 64
of 70.

Seven enumerated ARIA state attributes now reflect their keyword sets:
`aria-atomic`, `aria-checked`, `aria-expanded`, `aria-haspopup`,
`aria-orientation`, `aria-pressed`, and `aria-selected`
(`crates/renderer/src/js/bindings/reflected-enumerated.txt`). The generator's
reflected-enumerated path learned the nullable shape: a `DOMString?` member
reports the Undefined state as `null` and its setter removes the content
attribute on `null`, while a plain `DOMString` keeps the empty-string answer.
The getter maps the content value through the keyword set, returns the
canonical keyword for an ASCII case-insensitive match, and the invalid-value
default otherwise (`false` for `aria-atomic` and `aria-haspopup`).

Evidence: `html/dom` 63984 to **64380 subtests**, no lost subtests.
`aria-attribute-reflection-enumerated.tentative.html` 601 to 997 of 1761;
`aria-attribute-reflection.html` stays at 41 of 41.

The other thirteen enumerated ARIA attributes keep transparent reflection:
their proposed missing-value default is a keyword (`false`, `none`, `off`),
which contradicts the published ARIA 1.3 algorithm and the stable
`aria-attribute-reflection.html` (a removed attribute reads `null`). The
tentative test and the stable test describe the same DOM state with different
expected answers, so the conflict is recorded in `docs/limitations.md` and the
published behaviour wins until the ARIA change ships.

`navigator.registerProtocolHandler()` and `unregisterProtocolHandler()` now
exist and run the spec's normalize protocol handler parameters algorithm
(`crates/renderer/src/js/scripts/web/navigator.js`). The scheme is
ASCII-lowercased (so the Kelvin sign does not become an ASCII `k`), only a
safelisted scheme or `web+` plus one or more ASCII lowercase letters is
accepted, the URL must contain `%s`, and the encoding-parsed result must be an
HTTP(S) same-origin URL. The error order is the spec's: an invalid scheme is a
`SecurityError` before the URL is inspected, a missing `%s` or unparseable URL
is a `SyntaxError`, and a cross-origin or non-HTTP(S) URL is a
`SecurityError`.

Evidence: `html/webappapis` 1885 to **2130 subtests**, no lost subtests.
`protocol.https.html` 0 to 244 of 244, and `historical.https.window.html`
gains the one historical-IDL subtest that looks for the method.

Handler invocation is not implemented: a navigation to a registered scheme
does not reach the handler yet, which `docs/limitations.md` records.

`document.body` found the first `body` element anywhere in the document
instead of the first `body` or `frameset` child of the document element
(<https://html.spec.whatwg.org/multipage/dom.html#the-body-element-2>). A
frameset document therefore read `null`, and the three event-handler
reflection files that pass `document.body` to their shared helper threw
"cannot read property 'onblur' of null" before asserting anything.

The getter now shares the setter's `body_child` lookup: the first HTML
`body` or `frameset` child of the root element. A document with a `frameset`
reports it as `document.body`, and the setter keeps replacing that element.

Evidence: `html/webappapis` 1885 to **2506 subtests**, no lost subtests:
`event-handler-attributes-frameset-window.html` 0 to 140 of 140,
`event-handler-attributes-window-frameset.html` and
`event-handler-attributes-frameset-alt.html` 0 to 118 of 118 each.
`dom/nodes` 14344 to 14395 and `html/dom` 63984 to 64390, both with no lost
subtests.

The CSSOM `StyleSheet` interface is now on `CSSStyleSheet`
(`crates/renderer/src/js/scripts/web/cssom.js`): `type`, `href`, `ownerNode`,
`parentStyleSheet`, `ownerRule`, `title`, `media`, and `disabled`, with
`disabled` wired to the engine's `set_stylesheet_disabled` so the sheet stops
applying while keeping its owner. `ownerNode` is null once the owner element is
removed, `href` is the resolved `<link>` URL and null for inline `<style>`, and
`media` is a live `MediaList` over the owner's `media` attribute. `CSSRuleList`
and `MediaList` are interface objects, so `sheet.cssRules` and `sheet.media`
are their instances.

Evidence: `css/cssom` 3266 to **3307 subtests**, no lost subtests.
`StyleSheet-properties.html` 1 to 26 of 35; `style-sheet-interfaces-001.html`
2 to 4, `MediaList2.xhtml` 0 to 4, `HTMLLinkElement-disabled-003.html` 0 to 1,
and the two `ttwf-cssom-doc-ext-*` load-count/tree-order files also improve.

The nine remaining `@import` subtests need `CSSImportRule.styleSheet`; the
imported rules live outside the owner sheet's rule-list address space, which
`docs/limitations.md` records.

`location.protocol` now runs the spec's setter algorithm instead of passing the
value straight to `url::quirks::set_protocol`. The setter parses `value`
followed by `:` with the scheme start state as a state override
(`crates/renderer/src/js/bindings/window.rs`): ASCII tab and newline are
removed, other leading and trailing C0 controls and spaces are not, and a
scheme is one ASCII alpha followed by ASCII alphanumerics, `+`, `-`, or `.`.
A parse failure throws `SyntaxError`; a change the URL parser refuses
(special to non-special, or `file` with credentials) terminates without
navigating, and a non-HTTP(S) result terminates without navigating. The unit
test `protocol_scheme_follows_the_scheme_start_state` names the mutations.

Evidence: `html/browsers` 679 to **721 subtests**, no lost subtests.
`location-protocol-setter.html` 0 to 48 of 48 and
`location-protocol-setter-non-broken.html` 0 to 10; the weird-scheme file
stays at 5 of 5. `open-features-tokenization-*` report "script failed" on the
unmodified checkout too, so they are not caused by this change.

The `Origin` interface is implemented
(`crates/renderer/src/js/scripts/web/origin.js`): `new Origin()` creates an
opaque origin, and `Origin.from()` extracts one from an absolute URL string, a
`URL`, an `Origin`, the current global, a dispatched `MessageEvent`, and an
HTML `<a>`/`<area>`. Tuple origins compare by ASCII serialization and opaque
origins by identity, with one opaque identity per realm; `isSameSite()` uses
the cookie rules' scheme + registrable-domain comparison through `net`.
`Origin.from(string)` and `Origin.from(URL)` run the URL parser in the host
(`crates/renderer/src/js/bindings/origin.rs`), so blob-URL delegation and the
opaque-vs-tuple split match the URL Standard.

Evidence: `html/browsers` 679 to **885 subtests**, no lost subtests.
`origin-from-origin.any.html` 25 of 25, `origin-from-string.any.html` 27,
`origin-from-url.any.html` 27, `origin-from.any.html` 9,
`origin-from-messageevent.window.html` 6, `origin-from-window.window.html` 5 of
7, `origin-from-hyperlinkelementutils.window.html` 54 of 129 (the HTML
`<a>`/`<area>` cases), and `origin-from-worker.window.html` 2 of 2.

SVG/MathML hyperlink origins, another frame's `WindowProxy`, and opaque-origin
identity across messages from one sandboxed document are recorded in
`docs/limitations.md`.

`Element.setHTMLUnsafe()` and `ShadowRoot.setHTMLUnsafe()` now exist. They
parse the markup as a fragment in the target's context, replace its children
(or a template's contents), and with `runScripts: true` run the fragment's
scripts in tree order (`crates/renderer/src/js/bindings/node/element.rs`).
Scripts run before the custom element reactions the method's `[CEReactions]`
flushes, which is the order `setHTMLUnsafe-runScripts.html` asserts. `innerHTML`
shares the parse and insertion path through `replace_with_markup` and still
never runs scripts, so an inline or external script inserted with `innerHTML`
stays inert.

The interface operations install in JS
(`crates/renderer/src/js/scripts/web/dom.js`) because their
`(TrustedHTML or DOMString)` argument and `SetHTMLUnsafeOptions` dictionary are
unions the WebIDL generator does not lower. The host owns the receiver check
and the tree work, so a forged object or a non-element receiver throws.

Evidence: `html/webappapis` 2506 to **2529 subtests**, no lost subtests.
`setHTMLUnsafe-runScripts.html` 0 to 19 of 28, `setHTMLUnsafe.html` 0 to 3 of
5, and `Element-setHTMLUnsafe-04.html` 0 to 1 of 1. `domparsing` stays at 543
and `html/dom` at 64390, both with no lost subtests.

Declarative shadow roots, the `Sanitizer` API, `setHTML()`, already-started
scripts from `parseHTMLUnsafe()`, and XML documents are recorded in
`docs/limitations.md`.

`CSSStyleSheet.rules` is the legacy alias of `cssRules` and now reads the same
live list (`crates/renderer/src/js/scripts/web/cssom.js`). Test harnesses reach
for `style.sheet.rules`, so its absence failed every `@scope` parsing subtest
before the stylesheet was even inspected. `@scope` itself still does not parse,
which `docs/limitations.md` and `docs/upstream.md` record.

Evidence: `css/css-cascade` 208 to **236 subtests**, no lost subtests.
`at-scope-parsing.html` 0 to 28 of 43; the other 15 need the `@scope` at-rule,
which the pinned style crate gates behind a pref that cannot resolve an
implicit scope root correctly.

`Document.parseHTMLUnsafe()` gave the parsed document the caller's URL.
The spec gives the new document a content type and the caller's origin but no
URL, so its URL, `documentURI`, and `baseURI` stay the initial `about:blank`
(`crates/renderer/src/js/bindings/parsing.rs`). Five URL test files failed
their first assertion on every case before this.

Evidence: `html/webappapis` 2529 to **2555 subtests**, no lost subtests.
`Document-parseHTMLUnsafe-url.html`, `...-url-base.html`,
`...-url-pushstate.html`, and `...-url-base-pushstate.html` 0 to 6 of 6 each,
and `...-url-moretests.html` 0 to 2 of 4.

The window's own members now exist
(`crates/renderer/src/js/scripts/web/window.js`): `BarProp` and the six
`locationbar`/`menubar`/`personalbar`/`scrollbars`/`statusbar`/`toolbar`
replaceable attributes, `devicePixelRatio`, `screenX`, `screenY`, `status`,
`closed`, `frameElement`, and the prompt/print/move/resize/stop/blur/focus
operations. `frameElement` resolves the container element through a new host
call and throws for a cross-origin parent; there is no window manager, so the
geometry answers the viewport and the prompts behave as dismissed ones.

`Origin.from(WindowProxy)` also gained its cross-origin rule: a same-origin
proxy extracts this realm's origin, and a cross-origin or sandboxed one throws
`TypeError`, because a `WindowProxy` is not a platform object in the caller's
realm.

Evidence: `html/browsers` 885 to **921 subtests**, no lost subtests
(`origin-from-window.window.html` 5 to 7, `frameElement.sub.html` 0 to 1,
`noopener-noreferrer-BarProp.window.html` 0 to 1).
`webmessaging` stays at 189 and `html/dom` at 64390, both with no lost
subtests.

Throwing `SecurityError` for cross-origin member reads is implemented and
reverted: it lifts `window-security.https.html` from 12 to 121 of 124 but
regresses the `document.domain` tests, which the engine cannot support yet.
`docs/limitations.md` records what closing it needs.
The partial-update insertion methods now exist:
`appendHTMLUnsafe`/`prependHTMLUnsafe` on `Element` and `ShadowRoot`, and
`beforeHTMLUnsafe`/`afterHTMLUnsafe`/`replaceWithHTMLUnsafe` on `Element` and
`CharacterData` (`crates/renderer/src/js/bindings/node/element.rs`, installed
in `crates/renderer/src/js/scripts/web/dom.js`). They resolve the spec's
"target for HTML insertion" (a template unwraps to its contents) and "parent
for HTML insertion" (a `template` element, a `Document`, or a
`DocumentFragment` without a host throws `HierarchyRequestError`), parse in the
resolved context, place the fragment, and with `runScripts: true` run its
scripts before the `[CEReactions]` flush.

Template contents live in their own inert document, where the contents
fragment cannot name its host element, so `BlitzDocument` gained a
`template_host` back link. Without it, `beforeHTMLUnsafe` on a
`template.content` child saw a host-less `DocumentFragment` and threw.

Evidence: `domparsing` 543 to **687 subtests**, no lost subtests;
`positional-methods.html` 6 to 150 of 378. `html/dom` stays at 64390,
`html/webappapis` 2555 to 2557, and `dom/nodes` at 14395, all with no lost
subtests. The remaining 228 failures in `positional-methods.html` are the safe
variants (`appendHTML` and friends) and the `Sanitizer` subtests, which
`docs/limitations.md` records.
Selector matching gained `:lang()`, `:root`, and `:empty`, and attribute names
now follow the document's mode. All four are fork fixes in
`third_party/blitz/packages/blitz-dom/src/stylo.rs`:

- `:lang()` reads the nearest `lang`/`xml:lang` ancestor value and applies
  basic language-range filtering. The matcher had `Lang(_) => false`, so
  every `:lang()` failed even with the hooks that read the attribute.
- `:root` matches the document element only. The old check treated the top of
  any detached tree as a root, so `:root` matched detached and fragment
  subtrees.
- `:empty` ignores comments, processing instructions, and zero-length text.
- Attribute-name case sensitivity reads the document mode. `ElementData`
  carries it, `DocumentMutator::create_element` stamps it from the document
  node, and `HtmlDocument::from_xml` sets XML mode before parsing, so XHTML
  matches case-sensitively while a detached element created by an HTML
  document stays case-insensitive.

Evidence: `dom/nodes` 14395 to **14510 subtests**, `css/selectors` 4689 to
**4723**, `html/dom` 64390 to **64402**, and `css/css-cascade` stays at 236;
no lost subtests in any of them. `dom/nodes/ParentNode-querySelector-All.html`
and `ParentNode-querySelector-All-xht.xht` are both 1975/1975.

The UA stylesheet's default focus ring is `outline-style: auto` again, and the
paint backend draws `auto` as a solid ring instead of skipping it
(`third_party/blitz/packages/blitz-dom/assets/default.css`,
`blitz-paint/src/render/border.rs`). `focus-visible-017-2.html` focuses every
`[tabindex]` element in turn and asserts `outline-style: auto`; the sheet said
`solid`, so every one of its 37 focus subtests failed.

Evidence: `css/selectors` 4723 to **4761 subtests** and `css/cssom` 3305 to
**3309**, no lost subtests. `html/dom` stays at 64402 and `css/cssom-view` at
1616. `focus-visible-017-2.html` 1 to 38 of 38 and `focus-visible-017.html` 1
to 2.
SVG and MathML hyperlinks now expose `href`
(`crates/renderer/src/js/scripts/web/svg.js`, with the extraction in
`crates/renderer/src/js/scripts/web/origin.js`). SVG's `href` is an
`[SameObject] SVGAnimatedString` whose `baseVal` reflects the attribute and
whose `animVal` equals it (no animation support); MathML's `<a>` reflects
`href` as a string. The engine has one `SVGElement` and one `MathMLElement`
prototype, so the members live there and answer only for the local names that
include `SVGURIReference`/`HyperlinkElementUtils`;
`docs/limitations.md` records that shape deviation.

Without a `href` on those elements, `Origin.from()` threw `TypeError` for
every SVG and MathML anchor.

Evidence: `html/browsers` 921 to **995 subtests**, with one apparent loss,
`broadcastchannel.window.html`, that reruns as SKIP (bfcache unsupported) on
this build and at HEAD, so it is the known flake and not a regression.
`origin-from-hyperlinkelementutils.window.html` 54 to 129 of 129.
`html/dom` stays at 64402 with no lost subtests.
`:disabled` and `:enabled` now match the HTML disabled model
(`third_party/blitz` `5387dd187`). The state bits used to follow only the
element's own `disabled` attribute and only for button/input/select/textarea,
so a control inside a disabled fieldset, an `optgroup` or `option` under a
disabled `select`, and a disabled `fieldset`/`optgroup`/`option` never matched.
`BaseDocument::actually_disabled` implements the spec's "actually disabled"
set: the fieldset rule with each fieldset's first-legend exemption, the
`concept-option-disabled` ancestor walk that stops at `select`/`hr`/`datalist`/
`option` and at a second `optgroup`, and inherited disabledness from a
disabled `select` (`docs/upstream.md` records the fork reason).

The bits resync for an inserted or removed subtree, on a `disabled` write, and
on a disabled fieldset's child-list change, because a new first `legend`
re-enables its contents. A write on fieldset/select/optgroup hints
`RESTYLE_DESCENDANTS`, since descendants' own bits moved without an attribute
change of their own. `inside_first_legend` now matches the legend itself, not
just its descendants.

Evidence: `dom/nodes` 14510 to **14520** and `css/selectors` 4761 to **4777**
subtests, no lost subtests. `html/dom` stays at 64402 and `css/cssom` at 3309,
no lost subtests. `Element-matches-disabled-pseudo-class.html` 1/11 to 11/11,
`option-disabled-when-ancestor-changes.html` 0/4 to 4/4, and
`has-with-pseudo-class.html` 17/41 to 29/41. All four `option-disabled-*`
files in `html/semantics/forms/the-select-element/customizable-select` and
`disabled-nested-legend.html` pass.
`Document.images` and `Document.forms` are generated `[SameObject]`
`HTMLCollection` members backed by the live-collection machinery
(`crates/renderer/src/js/bindings/collections.rs`,
`crates/renderer/src/js/bindings/node/document.rs`). `document.images` did not
exist at all, and `document.forms` was a JavaScript shim whose proxy exposed
`length` in `getOwnPropertyNames` and matched an empty name.

Evidence: `html/dom` 64402 to **64417** subtests, no lost subtests.
`document.images.html` 0/11 to 11/11 and `document.forms.html` 2/6 to 6/6;
`document.links.html` and `document.scripts.html` stay green.
Namespaced attributes no longer stand in for the no-namespace attribute with
the same local name (`third_party/blitz` `3ab9c29a3`). The mutator's HTML
behavior branches keyed on the local name alone, so
`setAttributeNS(ns, "style", ...)` replaced the inline style, a namespaced
`disabled` disabled the control, a namespaced `href` made a link, and
`ElementData::attr`/`has_attr` answered a no-namespace lookup with the
namespaced value. `attr`/`has_attr` now match only the null namespace, the
mutator returns after storing a namespaced attribute, and the renderer's
`dom::tree::attr` follows the same rule.

`Element.slot` is now implemented as a reflected `DOMString`, and the WebIDL
generator supports `[Unscopable]` on attributes (the name joins the
prototype's `@@unscopables` object), which the DOM IDL marks on `slot`. The
input `type` reflection reads the content attribute through
`getAttributeNS(null, "type")`, because `getAttribute` is the qualified-name
lookup and may name a namespaced attribute.

Evidence: `html/dom` stays at **64417** and `html/browsers` at 995 to 1000,
both with no lost subtests. `dom/nodes` stays at 14520 plus the
namespaced-attribute gain, with no lost subtests; `Node-childNodes.html` (6/6)
and `Node-cloneNode-XMLDocument.html` (1/1) produced no subtests in the batch
run and pass in isolation again, so that was the harness flake.
`namespaced-attribute-mutations.html` 15/24 to **23/24** (the remaining
subtest needs `Element.assignedSlot`, a missing shadow-slotting surface).
`Element-setAttribute.html`, `Element-removeAttribute.html`,
`Element-hasAttribute.html` and `attributes.html` stay green, and
`document.forms.html` stays 6/6.
`ParentNode.moveBefore` now invalidates selectors that depend on the child
lists at both ends of the move (`third_party/blitz` `5ea1ff869`).
`Mutator::move_node` spliced the lists and marked damage but skipped the
child-list restyle invalidation that an insertion or removal runs, so
`:empty`, `:first-child`, `:nth-child`, sibling combinators, and `:has(>)`
stayed stale on the old and new parents, and a same-parent move left the
moved node with its old style data.

Evidence: `dom/nodes` 14521 to **14538** subtests, no lost subtests.
`moveBefore-child-list-selector-invalidation.html` 0/9 to 9/9.
`MutationObserver` now keeps a removed subtree observed until mutation
observers are notified (`crates/renderer/src/js/observers.rs`,
`crates/renderer/src/js/world.rs`). `drain` copies every `subtree: true`
registration whose target is an ancestor of a child-list removal onto the
removed node as a transient registration, including from other transients, so
a subtree removed from a removed subtree stays observed. Transient
registrations are matched like their source (rooted at the removed node), are
dropped when `observe` re-registers the source target, cleared by
`disconnect`, and retired by the notify microtask for every observer, whether
or not a record was queued for it.

The notify microtask is now also queued for an unobserved mutation that lands
inside an observed region, because the microtask is what retires those
transients. A mutation outside every observed region cannot queue a record or
create a transient, so it does not start one; without that bound, building an
unrelated detached tree would start a notification that retires a live
registration.

Evidence: `dom/nodes` 14538 to **14550** subtests, no lost subtests.
`MutationObserver-transient-registered-observers.window.html` 5/15 to 15/15.
The ARIA enumerated state attributes now reflect through the shared
enumerated-attribute table instead of returning the raw `aria-*` value
(`crates/renderer/src/js/bindings/reflected-enumerated.txt`,
`crates/renderer/src/js/bindings/enumerated.rs`). `aria-autocomplete`,
`aria-current`, `aria-invalid`, `aria-live`, and `aria-sort` gained keyword
types, and all twenty state attributes (`aria-atomic` through `aria-sort`)
carry their ARIA missing-value and invalid-value defaults. The hand-written
`aria_get`/`aria_set` getters and setters for the thirteen attributes the table
now owns were removed, because the generated pair is the only definition.

A nullable enumerated setter that receives null removes the content attribute
and records an explicit-null state on the document
(`SideTables::enumerated_nulls`); the getter reads that back as null instead
of the attribute's missing-value default, and any direct content-attribute
write clears it. That is what `aria-attribute-reflection.html` asserts for
`element.ariaBusy = null` while the tentative enumerated table still expects
a never-written attribute to read its missing default.

Evidence: `html/dom` 64417 to **65181** subtests, no lost subtests.
`aria-attribute-reflection-enumerated.tentative.html` 997/1761 to 1761/1761,
`aria-attribute-reflection.html` 41/41 and
`aria-attribute-reflection.tentative.html` 3/3.
`document.title` now follows the title-element definition instead of the first
`title` in the tree (`crates/renderer/src/js/bindings/node/html_elements.rs`).
An SVG document — one whose document element is an SVG `svg` — uses an SVG
`title` that is a child of that element, and the setter creates one as the
document element's first child. Every other document uses the first `title` in
the HTML namespace in tree order, even when the document is XML, and the setter
appends a new HTML `title` to the document's `head`. A document whose element is
neither HTML nor an SVG `svg` has no title, so the setter does nothing there.
The getter applies the HTML strip-and-collapse rule to the title element's
child text content, which also normalizes the initial `<title>` text
(`crates/renderer/src/dom_string.rs`).

Evidence: `html/dom` 65181 to **65204** subtests, no lost subtests.
`dom-tree-accessors` 157 to 180. `document.title-01` 3/4 to 4/4,
`-02.xhtml` 3/4 to 4/4, `-03` 1/11 to 11/11, `-04.xhtml` 0/1 to 1/1, `-07`
4/9 to 9/9, `-09` 1/5 to 5/5, and `document.title-not-in-html-svg` 1/2 to 2/2;
`-05`, `-06`, and `-08` stay green.
ARIA element reflection now stores the element or element list an IDL setter
received, instead of reflecting the `aria-*` content attribute as a string
(`crates/renderer/src/js/bindings/element_reference.rs`,
`crates/renderer/src/js/bindings/node/element.rs`). `ariaActiveDescendantElement`
and the seven `FrozenArray<Element>?` list attributes (`aria-controls`,
`aria-describedby`, `aria-details`, `aria-errormessage`, `aria-flowto`,
`aria-labelledby`, `aria-owns`) write the empty string to the content attribute
and keep the reference on the element. The getters apply the spec's
attr-associated-element and attr-associated-elements algorithms: a stored node
is visible only while it is a descendant of one of the element's
shadow-including ancestors, and the content-attribute fallback resolves each
IDREF in the element's own tree. The list getter caches the frozen array by
contents, so `element.reflectedElements === element.reflectedElements` holds
until the contents change.

A direct content-attribute write clears the stored reference, which is the
spec's attribute change step. Cross-document adoption keeps the reference
naming the same element: adoption snapshots into a new tree id, so
`DocumentStore::retarget_element_references` follows the adopted copy and
re-files the entry under the document that now owns the element. The
`FrozenArray` IDL type is carried to the native side as an opaque platform
object, and the setter converts the argument through its `@@iterator` so a
non-iterable throws `TypeError`.

Evidence: `html/dom` 65204 to **65229** subtests, no lost subtests.
`aria-element-reflection.html` 13/27 to 27/27 and
`aria-element-reflection-disconnected.html` 1/2 to 2/2.
`document.body` is now exposed only while the root element is an HTML `html`
element (`crates/renderer/src/js/bindings/node/document.rs`). The getter
returned the first `body`/`frameset` child of whatever the document element
was, so a document created with a non-HTML `html` root, or with `body` or
`frameset` as the root, exposed a body the spec hides
(<https://html.spec.whatwg.org/multipage/dom.html#the-body-element>). The
setter is unchanged: it still replaces the first body/frameset child of the
document element even when the root is not an HTML `html` element.

Evidence: `html/dom` 65229 to **65234** subtests, no lost subtests.
`Document.body.html` 21/26 to 26/26.

`document.embeds` and `document.plugins` now exist as the same
`[SameObject]` live `HTMLCollection` of the document's HTML `embed`
elements (`crates/renderer/src/js/bindings/collections.rs`). Both names share
one `SameObject` slot, so `document.embeds === document.plugins` holds and
repeated reads return the same object
(<https://html.spec.whatwg.org/multipage/dom.html#dom-document-embeds>).

Evidence: `html/dom` 65234 to **65239** subtests, no lost subtests.
`document.embeds-document.plugins-01.html` 0/5 to 5/5.
`dataset` now behaves as the spec's `DOMStringMap`
(`crates/renderer/src/js/scripts/web/dom.js`). The interface object
`DOMStringMap` is installed in the realm, with a normal
`DOMStringMap.prototype` chained to `Object.prototype`, so
`element.dataset instanceof DOMStringMap` holds and pages can extend the
prototype; the interface itself is not constructible. The proxy over the
element's `data-*` attributes gained the `has` trap, so `"foo" in dataset`
tracks the attributes, and the getter, deleter, and own-property descriptor
now ignore names that no name-value pair can produce (a U+002D followed by an
ASCII lower alpha), which is why `dataset["foo-bar"]` stays undefined even when
`data-foo-bar` is present. Named properties resolve before the prototype chain
because `DOMStringMap` has `[LegacyOverrideBuiltIns]`, while a non-set name
still reaches `Object.prototype`, and the named setter runs for every string
property, including one a prototype accessor also names.

Evidence: `html/dom` 65239 to **65253** subtests, no lost subtests.
`dataset.html` 3/8 to 8/8, `dataset-binding.window.html` 2/4 to 4/4,
`dataset-delete.html` 8/9 to 9/9, `dataset-get-invalid-names.html` 24/28 to
28/28, `dataset-prototype.html` 0/2 to 2/2; `dataset-get`, `dataset-set`, and
`dataset-enumeration` stay green. `html/browsers/the-window-object` stays at
238 subtests with no lost subtests.
`Document` now supports named properties
(`crates/renderer/src/js/bindings/node/named.rs`). `JsNode` is an exotic
class, and its `[[GetOwnProperty]]`/`[[OwnPropertyKeys]]` hooks answer only
for `Document`, decided by a flag stored on the wrapper at construction. The
supported property names follow the HTML named-element definition: each
element in the document tree contributes its `id` first and its `name`
second, ignoring later duplicates; `embed`, `form`, and `iframe` are named by
`name`; `object` by `name` or `id`; `img` by `name`, or by `id` while it also
carries a non-empty `name`. Template contents and shadow trees are outside
the document tree and contribute nothing. A single match answers the element,
an `iframe` with a content navigable answers its `WindowProxy`, and several
matches answer one live `HTMLCollection` whose identity is cached per
`(document, name)` so it survives tree moves and name mutations.

Named properties do not shadow built-ins here: a name a prototype already
carries is not exposed. That is the direction of the open WHATWG change
<https://github.com/whatwg/html/pull/13161> and what Firefox Nightly ships;
the current IDL still marks `Document` `[LegacyOverrideBuiltIns]`, and the
matching `nameditem-no-shadowing.tentative.html` asserts the no-shadowing
behavior. The `[[Set]]` hook creates an ordinary own property that shadows a
supported name, and the delete hook refuses to delete one, matching Web IDL's
legacy platform object internal methods.

Evidence: `html/dom` 65253 to **65339** subtests, no lost subtests.
All `nameditem-*` files pass except the two `HTMLDocument` assertions in
`nameditem-no-shadowing.tentative.html`, which stay red because the engine
has no `HTMLDocument` interface object (that file goes 5/8 to 6/8);
`nameditem-01` 0/7 to 7/7, `-02` 2/12 to 12/12, `-04` 3/12 to 12/12,
`-05` 3/12 to 12/12, `-06` 2/9 to 9/9, `-07` 0/14 to 14/14, `-08` 0/2 to 2/2,
`-adoption` 0/1 to 1/1, the four `img-name-*` files 0/1 to 1/1 each,
`-names` 3/21 to 21/21, `-supported-property-names-order` 0/1 to 1/1,
`-tree-order` 0/1 to 1/1, and `-tree-transitions` 0/2 to 2/2.
`dom/nodes` stays at 14550, `html/browsers` at 1000, and `css/selectors` at
4777 subtests, with no lost subtests.
HTML documents now expose the `HTMLDocument` interface object, so
`document.constructor` and `Object.getPrototypeOf(document)` match what engines
report (`crates/renderer/src/js/scripts/brands.js`,
`crates/renderer/src/js/bindings/mod.rs`). `HTMLDocument` is a legacy interface
that neither the current HTML nor DOM specification declares, but every engine
defines it and WPT uses it. A document whose content type is `text/html` is an
`HTMLDocument`; an XML document stays `XMLDocument`; `new Document()` and
`DOMParser` XML results stay plain `Document`. `host::node_interface_matches`
answers the new brand from the same parsed state, so `require_interface` and
union conversion agree with the prototype.

Evidence: `html/dom` 65339 to **65341** subtests, no lost subtests.
`nameditem-no-shadowing.tentative.html` 6/8 to 8/8. `dom/nodes` stays at
14550 and `html/browsers` at 1000 (plus a known-flaky
`broadcastchannel.window.html` pass), with no lost subtests.
`window.event` is now the spec's per-realm current event
(`crates/renderer/src/js/events.rs`). It is a `[Replaceable]` accessor
installed by `scripts/web/window.js`; the value lives on the `World`
(`World::current_event`), not on the global object, so assigning
`window.event` replaces the accessor with an own data property and dispatch
keeps writing only the realm slot. The inner invoke algorithm sets the
current event around each listener individually, using the callback's own
realm rather than the dispatching one, and restores it before the next
listener — including the handler attribute path, and including the exception
report, which runs in the callback's realm while its event is still current.
An invocation target whose tree root is a shadow root leaves the previous
current event in place, so `window.event` stays undefined inside a shadow
tree, as the legacy attribute requires.

The event path now follows the spec's get-the-parent for a composed event: a
shadow root continues at its host, while a non-composed event still stops at
the shadow root. Nodes beyond the host are outside the shadow tree, so their
listeners see the current event again. `assignedSlot` is still not followed
because Blitz has no slot assignment; that gap is recorded in
`docs/limitations.md`.

Evidence: `dom/events` 479 to **486** subtests, no lost subtests.
`event-global.html` 6/8 to 8/8, `event-global-extra.window.html` 4/6 to 6/6,
`event-global-replaceable.html` 1/2 to 2/2,
`event-global-is-still-set-when-reporting-exception-onerror.html` 0/1 to 1/1,
and `Event-dispatch-listener-order.window.html` 0/1 to 1/1. The timeout count
is unchanged (17) and the only remaining `event-global*` failure is the
`Performance` target, which has no `addEventListener` in this engine.
`html/dom` stays at 65341, `dom/nodes` at 14550, `html/browsers` at 1000, and
`css/selectors` at 4777, with no lost subtests.
Window named access now includes the document-tree child navigables' target
names (`crates/renderer/src/js/bindings/collections.rs`). The supported name
set adds an `iframe`/`frame` whose navigable target name matches and whose
child is same-origin, and the named getter answers that child's active
`WindowProxy`; a name matched only by the element's `id` still answers the
element, as the spec's "if objects contains a navigable" step requires. The
target name is read from the frame tree, so both the `name` content attribute
and a later `window.name` assignment move the name: the child's rename stops
the old name and starts the new one.

The listener adapter also reports a thrown exception with the listener's realm
context, so a callback interface's own `onerror` sees it, and `call_listener`
throws the "not callable" `TypeError` in that realm.

Evidence: `html/browsers` 1001 to **1009** subtests. `nested-context.html`
1/2 to 2/2, `window-named-properties.html` 2/7 to 4/7,
`named-objects.html` 5/6 to 6/6, `window-null-names.html` 0/1 to 1/1,
`window-indexed-access-vs-named-access.html` 5/10 to 8/10, and
`length-attribute.window.html` 0/1 to 1/1. The remaining `window[index]`
failures are the global's own indexed accessors shadowing a name, and
`Window`-based assertions still hit the missing `Window` interface object
(both recorded in `docs/limitations.md`). `dom/events` stays at 486,
`html/dom` at 65341, `dom/nodes` at 14550, and `css/selectors` at 4777, with
no lost subtests.
`Event.timeStamp` is now measured from the realm's `performance.now()` time
origin (`crates/renderer/src/js/events.rs`, `World::event_clock`). The world
samples `performance.now()` and an `Instant` once at install; every event is
stamped from that pair, so a constructed event's timestamp is comparable with
`performance.now()` taken around its creation. Constructor-created events stamp
in the `Event` constructor, user-agent and `createEvent()` events stamp in
`realm_event`, and the timestamp is no longer tied to the process's first event.

Evidence: `dom/events` 486 to **490** subtests, no lost subtests.
`Event-timestamp-high-resolution.html` 0/4 to 4/4. The 5-microsecond
coarsening of `Event-timestamp-safe-resolution.html` is not implemented and
is recorded in `docs/limitations.md`. `html/dom` stays at 65341,
`dom/nodes` at 14550, and `html/browsers` at 1009 (the one batch timeout,
`grandparent_session_aboutsrcdoc.sub.window.html`, passes 4/4 in isolation).
The legacy `init*Event` operations and the checkbox/radio activation rules now
match their specs. `UIEvent.prototype.initUIEvent`,
`MouseEvent.prototype.initMouseEvent`, and
`KeyboardEvent.prototype.initKeyboardEvent` do nothing while their event is
being dispatched (the dispatch flag is read through
`host.__tbEventDispatching`), and otherwise re-initialize the event and their
interface's slots; `UIEvent`'s `view` member is now converted as `Window?`, so
`new UIEvent("x", { view: 7 })` throws
(`crates/renderer/src/js/scripts/web/ui_events.js`). The checkbox/radio input
activation behavior returns before firing `input` and `change` when the
control is not connected, while the checkedness still moves, so a detached
control that is `click()`ed or has a `click` dispatched at it stays silent
(`crates/renderer/src/js/bindings/focus.rs`).

Evidence: `dom/events` 490 to **498** subtests, no lost subtests.
`Event-init-while-dispatching.html` 2/5 to 5/5,
`Event-subclasses-constructors.html` 48/49 to 49/49, and
`Event-dispatch-detached-input-and-change.html` 8/12 to 12/12.
`html/semantics/forms` 4512 to **4521** with no regression: the two
`form-double-submit*` subtests that differ from that old baseline also fail
at `0b584af2`, so they are pre-existing navigation-timing failures, not this
change. `html/dom` stays at 65341, `dom/nodes` at 14550, and `html/browsers`
at 1009, with no lost subtests.
Event handler content attributes now follow HTML's event handler map instead
of a compiled slot invoked after the listener list. `World` keeps one
`EventHandlerSlot` per `(target, name)` with a value that is null, a
JavaScript value, or the raw attribute source; setting the attribute stores
the source and activates a real listener with no callback, and dispatch runs
the event handler processing algorithm when that listener comes up
(`crates/renderer/src/js/events.rs`, `world.rs`). The listener is registered
where the attribute was set, so `onclick` and `addEventListener` now interleave
in the order they were written instead of every handler running last
(`inline-event-handler-ordering.html`). The raw value compiles lazily in the
handler document's realm, which is what makes a `DOMParser` document's handler
run in the page realm while a document without a browsing context never
compiles at all (`event-handler-idl-attribute-realm`, `uncompiled_event_handler_with_scripting_disabled.html`);
the compiled function's `toString()` is the spec's `sourceText`
(`event-handler-sourcetext.html`). Returning `false` (or `true` for a window
`onerror`) sets the canceled flag, an `onbeforeunload` return coerces through
`DOMString?` and only cancels a real `BeforeUnloadEvent`, and a handler value
that is an object but not callable is ignored rather than thrown
(`event-handler-handleEvent-ignored.html`). Special error handling keys off an
`ErrorEvent` of type `error` at a global, not off the handler's name, so
`window.onerror` gets its five arguments for a real error and one argument for
`new Event("error")`; a worker global keeps its plain-property handlers by
being classified as one (`Worker_Self_Origin.html` 6/6). The adopted node's
map entries travel with it, so `el.onclick = fn` survives moving the element
to another document instead of being rederived from the attribute.

Attribute change steps are the only place the map changes, so every path that
installs attributes without the DOM setter runs them: the parser walk at realm
install, `innerHTML`/`outerHTML`/`insertAdjacentHTML`/`setHTMLUnsafe`, and now
`cloneNode`, `importNode`, and cross-document adoption
(`crates/renderer/src/js/bindings/attributes.rs`). The clone and adoption
hooks were not optional: cloning an element out of `<template>` content
copied the `onclick` attribute but not its handler, so `click()` ran the
default action and navigated the test page
(`html/semantics/forms/the-label-element/clicking-interactive-content.html`,
36/36 back).

Evidence: `html/webappapis` 2558 to **2607** subtests, `dom/events` 498 to
**638** (`Event-dispatch-single-activation-behavior.html` 0/132 to 132/132,
`Event-dispatch-click.html` 0/33 to 33/33), `html/browsers` 1009 to **1012**,
with `html/dom` at 65341, `dom/nodes` at 14550, `css/selectors` at 4777, and
`html/semantics/forms` 4521 to **4522**, all with no lost subtests. Batch runs report
harness timeouts for `nameditem-*`, `grandparent_session_aboutsrcdoc`, and
`form-double-submit`; each passes in isolation (the last only intermittently,
at `0b584af2` too), and `form-submit-and-window-stop` timed out there as well.
Still open: the handler scope chain has no `document`/element/form-owner
object environment, and the same-document traversal files now get past their
first ordering assertion and hang on `history.back()` firing `popstate` and
`hashchange` synchronously (`docs/limitations.md`).
`getComputedStyle` now answers an empty read-only declaration for an element
that has no computed style. CSSOM reports one only when the element is
connected, in the flat tree, and its shadow-including root's browsing context
is being rendered; a detached element, an unslotted light child of a shadow
host, or the document of a `display: none` iframe gets no declaration
(`css/cssom/getComputedStyle-detached-subtree.html`, 6/6). The check lives in
`crates/renderer/src/js/bindings/mod.rs` (`has_computed_style`) with the
flat-tree walk in `dom/tree.rs`; the JS wrapper returns a proxy that reports
`length` 0 and `""` for every property. Blitz has no slot assignment, so the
flat-tree test treats a light child as slotted when the host's shadow tree
contains a `<slot>` element, which keeps
`getComputedStyle-display-none-shadow-invalidation.html` green. A pending
same-origin child window now forwards `getComputedStyle` from the caller's
realm, because the call reads the element's own document and carries no window
state.

Evidence: `css/cssom` 3331 to **3337** subtests, no lost subtests.
`css/cssom-view` stays at 1616, `resize-observer` at 50,
`intersection-observer` at 185, `css/selectors` at 4777, and
`html/semantics/forms` at 4522 against freshly captured baselines at the
parent commit. The one form diff, `form-double-submit.html`, fails the same
way three times at the parent commit, so it is pre-existing.
CSS declaration blocks now carry `removeProperty` and the CSSOM stringifier.
`css/cssom/property-accessors.html` sets and removes properties on a computed
style, an inline style, and a rule's style; the rule style had no
`removeProperty`, so that call threw and the crash test's `catch` compared the
wrong declarations and failed all nine properties. `CSSStyleDeclaration`
objects also failed `String(decl)`: the inline-style proxy answered
`toString` with the empty string from its unknown-property path, so
`ToPrimitive` got a non-callable. The rule style delegates removal to the host
setter's empty-value path and returns the previous value, and every declaration
object (inline, rule, computed, and the empty computed declaration) now has a
`toString` stringifier
(`crates/renderer/src/js/scripts/web/cssom.js`, `dom.js`,
<https://drafts.csswg.org/cssom/#dom-cssstyledeclaration-removeproperty>,
<https://drafts.csswg.org/cssom/#the-cssstyledeclaration-interface>).

Evidence: `css/cssom` 3337 to **3347** subtests, no lost subtests;
`property-accessors.html` 0/9 to 9/9. Against the previous slice's runs,
`css/cssom-view` stays at 1616, `resize-observer` at 50,
`intersection-observer` at 185, `css/selectors` at 4777, and
`html/semantics/forms` 4522 to 4523 with no lost subtests.
`cargo fmt --check`, `tools/check clippy`, and the renderer (175),
webidl-bindgen (42), and wpt-harness (30) test binaries pass.
CSS rule objects now expose their IDL attributes on the interface prototypes
instead of as own properties, and a rule's `style` is a real
`CSSStyleDeclaration`. WebIDL puts attributes on the interface prototype
object, so `assert_idl_attribute` failed for every rule attribute
(`CSSStyleRule.html`, `cssimportrule.html`), and `rule.style.margin` was
undefined because the rule declaration had no CSS property IDL attributes.
`rule.style` is now `[SameObject, PutForwards=cssText]`: reading it answers
one cached declaration, assigning a string replaces that declaration's text.
The text setter goes through the engine's existing rule cssText path
(`stylesheet_rule_style_set_css_text`), and the declaration is wrapped in the
same property-name proxy the inline style uses
(`crates/renderer/src/js/scripts/web/cssom.js`, `bindings/cssom_host.rs`).
A style rule also owns a rule list (CSS Nesting), so `cssRules` moved to
`CSSStyleRule.prototype` as well as the grouping and keyframes prototypes.

Evidence: `css/cssom` 3347 to **3370** subtests, no lost subtests.
`CSSStyleRule.html` 4/10 to 10/10, `cssimportrule.html` 5/11 to 6/11 (what
remains needs a `MediaList` on rule `media`, the imported `styleSheet`, the
`@page` rule the engine does not list, and `supportsText`).
`css/cssom-view` stays at 1616, `resize-observer` at 50,
`intersection-observer` at 185, `css/selectors` 4777 to **4778**, and
`html/dom` at 65341 with no lost subtests; `html/semantics/forms` shows only
the pre-existing `form-double-submit.html` failure. `cargo fmt --check`,
`tools/check clippy`, and the renderer (175), webidl-bindgen (42), and
wpt-harness (30) test binaries pass. `CSSFontFeatureValuesRule.fontFamily` has
no setter in the style engine, so it is not exposed on the prototype; a JS
assignment keeps the old own-property behavior, recorded in
`docs/limitations.md`.
Media lists are now live objects on rules and stylesheets. A media rule's and
an import rule's `media` are `[SameObject]` `MediaList`s backed by the engine's
query list, and `StyleSheet.media` gained its `[PutForwards=mediaText]` setter;
`MediaList` gained `appendMedium`/`deleteMedium`, with the same normalized-form
comparison CSSOM requires
(`crates/renderer/src/js/scripts/web/cssom.js`). The Blitz CSSOM snapshot now
reports a media rule's `media` text and an import rule's `supportsText`, and a
new `cssom_rule_set_media` parses the setter's text with stylo's
`MediaList::parse` and replaces the list on the rule or the imported sheet
(`third_party/blitz` commit `6152e0e2d`, recorded in `docs/upstream.md`).

Evidence: `css/cssom` 3370 to **3394** subtests, no lost subtests.
`MediaList-parsing.html` 0/13 to 13/13 and `cssimportrule.html` 6/11 to 9/11;
`StyleSheet-properties.html` 26/35. The remaining `cssimportrule.html` failures
are `CSSImportRule.styleSheet` (the imported sheet is still not exposed) and the
`@page` rule the engine does not list. `css/cssom-view` stays at 1616,
`resize-observer` at 50, `intersection-observer` at 185, `css/selectors` at
4778, `html/dom` at 65341, and `html/semantics/forms` at 4522, with no lost
subtests. `cargo fmt --check` (both trees), `tools/check clippy`, and the
renderer (175), webidl-bindgen (42), and wpt-harness (30) test binaries pass.
`CSSKeyframeRule.keyText` is now settable. A keyframe's selectors parse as a
`<keyframe-selector>#` list, an invalid list throws `SyntaxError` and leaves
the rule unchanged, and a valid one replaces the selectors, which normalizes
`from`/`to` to `0%`/`100%`
(`crates/renderer/src/js/scripts/web/cssom.js`,
`crates/renderer/src/js/bindings/cssom_host.rs`). The Blitz fork's new
`cssom_rule_set_key_text` parses with stylo's `KeyframeSelectors::parse`,
writes through the keyframe lock, and invalidates the owning `@keyframes`
rule (`third_party/blitz` commit `a398a93d7`, recorded in
`docs/upstream.md`).

Evidence: `css/cssom` 3394 to **3412** subtests, no lost subtests.
`CSSKeyframeRule-keyText.html` 4/11 to 11/11 and
`CSSKeyframeRule-keyText-edge-cases.html` 3/14 to 14/14.
`css/cssom-view` stays at 1616, `resize-observer` at 50,
`intersection-observer` at 185, `css/selectors` at 4778, and `html/dom` at
65341, with no lost subtests. `html/semantics/forms` shows three
`form-double-submit*` failures in the batch; all three fail the same way three
times in isolation at the parent commit, so they are pre-existing.
`cargo fmt --check` (both trees), `tools/check clippy`, and the renderer (175),
webidl-bindgen (42), and wpt-harness (30) test binaries pass.
`CSSKeyframesRule` now has its full IDL surface: `name` is settable,
`cssRules` and `length` answer the keyframe list, the indexed getter reads it,
and `appendRule`/`deleteRule`/`findRule` address keyframes by selector
(`crates/renderer/src/js/scripts/web/cssom.js`,
`crates/renderer/src/js/bindings/cssom_host.rs`). The Blitz fork reports a
keyframes rule's raw name in the snapshot (the IDL attribute is unquoted while
`cssText` quotes a name that is not a valid `<custom-ident>`), and it gained
`cssom_rule_set_name` and `cssom_find_keyframe` (`third_party/blitz` commit
`bb70a63aa`, recorded in `docs/upstream.md`). The keyframes rule object is
wrapped in a proxy so the indexed getter (`keyframes[0]`) reads the same
`CSSRuleList` the `cssRules` attribute exposes.

Evidence: `css/cssom` 3412 to **3421** subtests, no lost subtests;
`CSSKeyframesRule.html` 0/2 to 2/2. `css/cssom-view` stays at 1616,
`resize-observer` at 50, `intersection-observer` at 185, `css/selectors` at
4778, `html/dom` at 65341, and `html/semantics/forms` at 4522, with no lost
subtests. `cargo fmt --check` (both trees), `tools/check clippy`, and the
renderer (175), webidl-bindgen (42), and wpt-harness (30) test binaries pass.
`getComputedStyle` now parses its pseudo-element argument. `pseudoElt` is
tokenized as a `<pseudo-element-selector>`: a cascaded pseudo-element
(`::before`, `::after`, `::selection`, `::first-line`, `::first-letter`, and the
legacy single-colon spellings) answers that pseudo-element's declaration, one
CSSOM recognises but the engine does not cascade still answers a declaration,
and an invalid selector (a trailing token, `::part()`, `::slotted()`, or a
`::picker()` argument other than `select`) returns the empty declaration
(`crates/renderer/src/js/scripts/web/cssom.js`,
`crates/renderer/src/js/bindings/mod.rs`). A cascaded `::before`/`::after`
resolves from its layout box when one exists and from its computed style
otherwise, which covers pseudos of `display: contents` elements, `display: none`
elements, and flex blockification. A computed style also reports its declaration
names now: every supported longhand lexicographically, then the element's custom
properties, backing `length`, `item()`, and the indexed getter. The engine work
is in `third_party/blitz` commit `ad4193380`, recorded in `docs/upstream.md`.

A computed style declaration is immutable: every write, `setProperty`, and
`removeProperty` throws `NoModificationAllowedError` instead of reaching the
inline style
(<https://drafts.csswg.org/cssom/#dom-window-getcomputedstyle>).

Evidence: `css/cssom` 3421 to **3449** subtests, no lost subtests.
`getComputedStyle-pseudo.html` 2/28 to 14/28,
`getComputedStyle-pseudo-with-argument.html` 5/22 to 17/22,
`getComputedStyle-pseudo-picker.html` 6/9 to 7/9, and
`cssstyledeclaration-custom-properties.html` 0/1 to 1/1. `css/cssom-view` stays
at 1616, `resize-observer` at 50, `intersection-observer` at 185,
`css/selectors` 4778 to **4791**, and `html/dom` at 65341, with no lost
subtests. `html/semantics/forms` shows one `form-double-submit` failure in the
batch; that file is intermittent in isolation both with this change (1/3 pass)
and at the parent commit (2/3 pass), so it is a pre-existing timing flake.
`cargo fmt --check` (both trees), `tools/check clippy`, and the renderer (175),
webidl-bindgen (42), and wpt-harness (30) test binaries pass.
`getComputedStyle` now resolves the inset properties of a `position: sticky`
box. CSSOM reports a sticky box's insets as used values, so percentages and
`calc()` absolutize against the box's nearest scrollport rather than staying
percentages, while `auto` stays `auto` because a sticky box's static position
is not an inset
(<https://drafts.csswg.org/css-position-3/#sticky-pos>). The engine resolves
`top`/`bottom` against the scrollport height and `left`/`right` against its
width, where the scrollport is the nearest ancestor that establishes a
scrolling box (or the viewport when none does)
(`third_party/blitz` commit `afe0d6d25`, recorded in `docs/upstream.md`).

Evidence: `css/cssom` 3449 to **3594** subtests, no lost subtests.
`getComputedStyle-insets-sticky.html` 180/252 to 252/252,
`getComputedStyle-insets-sticky-container-for-abspos.html` 180/252 to 252/252,
and `getComputedStyle-sticky-pos-percent.html` 0/1 to 1/1. `css/cssom-view`
stays at 1616, `resize-observer` at 50, `intersection-observer` at 185,
`css/selectors` at 4791, `html/dom` at 65341, and `html/semantics/forms` at
4521, with no lost subtests. `cargo fmt --check` (both trees), `tools/check
clippy`, and the renderer (175), webidl-bindgen (42), and wpt-harness (30) test
binaries pass.
`CSSStyleDeclaration` is now iterable. WebIDL's `iterable<CSSOMString>` gives
every declaration object `keys`, `values`, `entries`, `forEach`, and
`Symbol.iterator` (the same function as `values`), on the interface prototype
(`crates/renderer/src/js/scripts/web/cssom.js`). The inline style proxy and the
computed style proxy forward that surface and run it against their own
`length`/`item`, so `[...element.style]`, `for (const name of block)`, and
`[...getComputedStyle(elt)]` all enumerate declaration names
(`crates/renderer/src/js/scripts/web/dom.js`). An iterator over the values now
advances its index on every step (the first version did not, which made every
page-side loop run until the harness killed the script). A computed style's
`cssText` is also the empty string now
(<https://drafts.csswg.org/cssom/#dom-window-getcomputedstyle>).

Evidence: `css/cssom` 3594 to **3601** subtests, no lost subtests.
`CSSStyleDeclaration-iterator.html` 0/1 to 1/1,
`cssstyledeclaration-csstext.html` 10/11 to 11/11,
`cssstyledeclaration-csstext-all-shorthand.html` 5/6 to 6/6,
`getComputedStyle-basic.html` 3/4 to 4/4,
`getComputedStyle-logical-enumeration.html` 0/1 to 1/1, and
`serialize-all-longhands.html` 0/2 to 2/2. `css/cssom-view` stays at 1616,
`resize-observer` at 50, `css/selectors` at 4791, `intersection-observer` at
185, `html/dom` at 65341, and `html/semantics/forms` at 4521, with no lost
subtests; the forms batch loses one `form-double-submit-3` subtest that fails
3/3 in isolation at the parent commit, so it is pre-existing. `cargo fmt
--check` (both trees), `tools/check clippy`, and the renderer (175),
webidl-bindgen (42), and wpt-harness (30) test binaries pass.
A computed style now enumerates its standard properties before the
vendor-prefixed ones, each group in lexicographical order, which is the order
browsers report (the plain lexicographical sort put `-webkit-…` first because
`-` sorts before letters). Setting a declaration through the CSSOM also
replaces the old declaration's priority: `setProperty(name, value)` without a
priority now clears an existing `!important`, and a shorthand replaces all of
its longhands. Both are engine fixes in `third_party/blitz` commit `4f3a4d572`,
recorded in `docs/upstream.md`
(<https://drafts.csswg.org/cssom/#set-a-css-declaration>).

Evidence: `css/cssom` 3601 to **3607** subtests, no lost subtests.
`getComputedStyle-property-order.html` 0/1 to 1/1 and
`cssstyledeclaration-setter-declarations.html` 0/4 to 4/4. `css/cssom-view`
stays at 1616, `resize-observer` at 50, `css/selectors` at 4791,
`intersection-observer` at 185, `html/dom` at 65341, and
`html/semantics/forms` 4521 to **4522**, with no lost subtests. `cargo fmt
--check` (both trees), `tools/check clippy`, and the renderer (175),
webidl-bindgen (42), and wpt-harness (30) test binaries pass.
`CSSImportRule.styleSheet` now answers the imported sheet. The engine opens a
CSSOM sheet record for the sheet an `@import` rule loaded (a record with no
owner node), and the renderer wraps it as a `CSSStyleSheet` whose
`ownerNode` is null, `parentStyleSheet` is the importing sheet, `ownerRule` is
the `CSSImportRule`, `href` is the resolved import URL, and `media` is the same
`MediaList` object as `CSSImportRule.media`
(<https://drafts.csswg.org/cssom/#dom-cssimportrule-stylesheet>,
<https://drafts.csswg.org/cssom/#dom-stylesheet-parentstylesheet>). Deleting the
import rule unlinks the child's `parentStyleSheet`/`ownerRule`, and its
`cssRules` is a live list addressed by the imported sheet's id, so nested
imports work. The engine change is `third_party/blitz` commit `8c65aba97`,
recorded in `docs/upstream.md`.

Evidence: `css/cssom` 3607 to **3624** subtests, no lost subtests.
`StyleSheet-properties.html` 26/35 to 35/35,
`CSSImportRule-media-identity.html` 0/1 to 1/1, `cssimportrule-parent.html`
0/1 to 1/1, `cssimportrule-sheet-identity.html` 0/1 to 1/1, `cssimportrule.html`
9/11 to 10/11, `style-sheet-interfaces-001.html` 4/5 to 5/5, and
`CSSStyleSheet-constructable-replace-on-regular-sheet.html` 0/3 to 3/3.
`css/cssom-view` stays at 1616, `resize-observer` at 50, `css/selectors` at
4791, `intersection-observer` at 185, and `html/dom` at 65341, with no lost
subtests. `html/semantics/forms` loses one `form-double-submit.html` subtest
that fails 3/3 in isolation both with this change and at the parent commit, so
it is the pre-existing flake. `cargo fmt --check` (both trees), `tools/check
clippy`, and the renderer (175), webidl-bindgen (42), and wpt-harness (30) test
binaries pass.
`new CSSStyleSheet(options)` now builds a real sheet. The constructor accepts
`baseURL` (an invalid one throws `NotAllowedError`, a relative one resolves
against the document), `media`, and `disabled`; the engine parses the CSS into
an ownerless stylesheet record the CSSOM can address, so `cssRules`,
`insertRule`, `deleteRule`, `replaceSync`/`replace`, `style.cssText`, `media`,
and `disabled` all work without an element owner. `replace` re-parses in place
under the same CSSOM id, keeps the media list and disabled flag, and detaches
every rule object already handed out. A constructable sheet never loads an
`@import`: `replaceSync` drops one and `insertRule` throws `SyntaxError`. The
engine change is `third_party/blitz` commit `90800c660`, recorded in
`docs/upstream.md`
(<https://drafts.csswg.org/cssom/#dom-cssstylesheet-cssstylesheet>,
<https://wicg.github.io/construct-stylesheets/>).

Evidence: `css/cssom` 3624 to **3651** subtests, no lost subtests.
`cssstyledeclaration-all-shorthand.html` 21/27 to 27/27,
`CSSStyleSheet-replace-parse-errors.html` 2/8 to 8/8,
`CSSStyleSheet-replace-detach-old-rules.html` 0/4 to 4/4,
`CSSStyleSheet-constructable-baseURL.html` 2/3 to 3/3,
`CSSStyleSheet-constructable-disallow-import.tentative.html` 0/4 to 1/4,
`CSSStyleSheet-constructable-replace-thenable.html` 0/1 to 1/1,
`CSSStyleSheet-constructable.html` 1/13 to 3/13,
`CSSKeyframesRule-appendRule-multiple.html` 0/2 to 2/2,
`medialist-mutation-after-owner-gc.html` 0/9 to 3/9, and
`style-sheet-interfaces-001.html` 5/6 to 6/6. `css/cssom-view` stays at 1616,
`resize-observer` at 50, `css/selectors` at 4791, `intersection-observer` at
185, and `html/dom` at 65341, with no lost subtests; `html/semantics/forms`
4522 to **4523**. `cargo fmt --check` (both trees), `tools/check clippy`, and
the renderer (175), webidl-bindgen (42), and wpt-harness (30) test binaries
pass.
`document.adoptedStyleSheets` is now an observable array of constructable
sheets whose order is the cascade order, applied after every document sheet
(<https://drafts.csswg.org/cssom/#dom-document-adoptedstylesheets>,
<https://github.com/WICG/observable-array>). Every in-place mutation (`push`,
`pop`, `splice`, `reverse`, an index or `length` write) validates the whole
list — a non-object is a `TypeError`, a sheet the constructor did not build is
`NotAllowedError` — and hands the ids to the engine only when the list is
valid, restoring the previous contents on rejection. Assigning the attribute
replaces the array's contents in place, so a held array keeps tracking the
document. The engine removes the old list from the stylist and appends the new
one in order, keeps adopted sheets live for rule edits, and places a document
sheet inserted later before them
(`third_party/blitz` commit `2049b2b0d`, recorded in `docs/upstream.md`).
Shadow-root adoption remains out because the engine's
`implicit_scope_for_sheet_in_shadow_root` is unimplemented; that is recorded in
`docs/limitations.md`.

Evidence: `css/cssom` 3651 to **3673** subtests, no lost subtests.
`adoptedstylesheets-cascade-order.html` 0/13 to 10/13,
`adoptedstylesheets-modify-array-and-sheet.html` 0/3 to 3/3,
`CSSStyleSheet-constructable-replace-cssRules.html` 0/2 to 2/2,
`CSSStyleSheet-constructable.html` 3/13 to 6/13,
`CSSStyleSheet-constructable-duplicate.html` 0/4 to 2/4,
`CSSStyleSheet-constructable-invalidation.html` 0/3 to 1/3, and
`CSSStyleSheet-constructable-cssRules.html` 0/1 to 1/1. `css/cssom-view` stays
at 1616, `resize-observer` at 50, `css/selectors` at 4791,
`intersection-observer` at 185, and `html/dom` at 65341, with no lost subtests;
`html/semantics/forms` stays at 4522, losing one `form-double-submit-3`
subtest that fails 3/3 in isolation at the parent commit too. `cargo fmt
--check` (both trees), `tools/check clippy`, and the renderer (175),
webidl-bindgen (42), and wpt-harness (30) test binaries pass.
Click activation now follows DOM's dispatch rules
(<https://dom.spec.whatwg.org/#concept-event-dispatch>). Only a `MouseEvent`
named `click` is an activation event, ancestors are considered only when
`event.bubbles` is true, and the one activation target picked from the event
path runs the activation behavior: `input` submit/reset return when the control
is disabled, a checked `label` forwards a synthetic click to its labeled control
unless the click landed on interactive content, and `summary` toggles the
`open` attribute of its parent `details` and queues its `toggle` event. The
unconditional post-dispatch label call is gone, so a click on an `area` or
`a` inside a label no longer also activates the labeled control. `submit` and
`reset` stop propagating at an ancestor `form` during bubbling, matching all
three engines and whatwg/html#3599; capture still traverses the whole path.

Evidence: `dom/events` 638 to **663** passing subtests, no lost subtests.
`Event-dispatch-click.html` 28/33 to 33/33 and
`Event-dispatch-single-activation-behavior.html` 112/132 to 132/132.
`css/cssom` stays at 3673, `css/cssom-view` at 1616, `resize-observer` at 50,
`css/selectors` at 4791, `intersection-observer` at 185, and `html/dom` at
65341, with no lost subtests; `html/semantics/forms` 4522 to **4524**, with
`form-double-submit-3.html` failing 1/1 at the parent commit too. `cargo fmt
--check` (both trees), `tools/check clippy`, and the renderer (175),
webidl-bindgen (42), and wpt-harness (30) test binaries pass. The two browser
integration failures (`secure_context::a_popup_inherits_its_openers_secure_context`,
`window::a_blob_popup_compares_its_entry_with_the_top_level_origin`) reproduce
1/1 at the parent commit.
The Performance Timeline is now a real per-realm timeline. The native
`performance` object only offered `now`/`timeOrigin`, so `performance.js`
replaces it with `Performance`, `PerformanceEntry`, `PerformanceMark`,
`PerformanceMeasure`, `PerformanceResourceTiming`,
`PerformanceNavigationTiming`, `PerformanceObserver`, and the obsolete
`PerformanceTiming`/`PerformanceNavigation` views
(<https://w3c.github.io/hr-time/#the-performance-interface>,
<https://w3c.github.io/performance-timeline/>,
<https://w3c.github.io/user-timing/>). The buffer map keeps one tuple per
entry type with its maximum size, `availableFromTimeline` flag, and dropped
entries count; `getEntries*` filters the timeline and sorts by `startTime`.
`PerformanceObserver` keeps the per-global task coalescing, the
`droppedEntriesCount` callback option, and the one-shot dropped report that a
buffered registration owes. `measure()` converts a `PerformanceTiming`
attribute name through the same view the page reads, so
`navigationStart`/`responseEnd` resolve instead of throwing.

The navigation entry is created with the document and queued to observers when
`load` finishes, so an observer registered during parsing sees it without
`buffered`; its `navigationId` is the id every later entry on that document
carries (<https://w3c.github.io/navigation-timing/#marking-navigation-timing>).
Resource entries come from two places: the page `fetch`/XHR/beacon helper
records its own request in `fetch.js`, and the renderer reports Blitz
subresources (images, stylesheets, fonts) and classic scripts through
`bindings/performance.rs`. A dial now carries its start `Instant`, so the
entry's start is still placed on the timeline when the realm clock was
installed after the dial began. Resource buffer overflow keeps the secondary
buffer, fires `resourcetimingbufferfull`, and counts dropped entries
(<https://w3c.github.io/resource-timing/#sec-resource-timing>).

Evidence: `user-timing` 13 to **579** passing subtests and
`performance-timeline` 5 to **51**, with no lost subtests. `measure.html`
2/38 to 37/38, `measure_navigation_timing.html` 2/17 to 17/17,
`measures.html` 0/119 to 119/119, `po-observe.html` 0/1 to 1/1,
`po-resource.html` 0/1 to 1/1, `droppedentriescount.any.html` 0/5 to 4/5,
and `get-invalid-entries.html` 0/1 to 1/1. Nine user-timing files move from
ERROR to OK and five performance-timeline files do the same;
`navigation-id-initial-load.tentative.html` moves OK (0/1) to TIMEOUT because
the test waits for a largest-contentful-paint entry, which this engine does
not produce. `dom/events` stays at 665 (638 in the pre-click baseline, with
the two EventTarget tests now passing), `css/cssom` 3674,
`css/cssom-view` 1616, `resize-observer` 50, `css/selectors` 4791,
`intersection-observer` 185, `html/dom` 65341, and the beacon suite 84/85,
all with no lost subtests. `html/semantics/forms` loses 1 subtest
(`form-double-submit.html`), which fails 1/1 at the parent commit.
`cargo fmt --check` (both trees), `tools/check clippy`, and the renderer
(175), webidl-bindgen (42), and wpt-harness (30) test binaries pass; the two
browser integration failures are the environment failures that reproduce at
the parent commit.

CSSOM View geometry is now a full API surface. `cssom_geometry.js` installs
`DOMQuad` and `DOMRectList`, and `GeometryUtils` (`getBoxQuads`,
`convertPointFromNode`, `convertRectFromNode`, `convertQuadFromNode`) on
`Element`, `Text`, and `Document`
(<https://drafts.csswg.org/cssom-view/#the-geometryutils-interface>,
<https://drafts.fxtf.org/geometry/#DOMQuad>). Each method follows the spec's
box adjustments: a quad is built in the node's border-box space, shifted by
the source's viewport origin, and expressed in `relativeTo`'s border-box
space; `fromBox`/`toBox` add the margin, border, padding, or content offset
from the element's computed edges. `getBoundingClientRect()` now answers the
union of an element's box fragments and `getClientRects()` one rect per line
box, both through `BaseDocument::get_client_bounding_rect`/`node_client_rects`
instead of a linear scan of every laid-out box, so a non-atomic inline returns
its per-line fragments. `DOMRect`/`DOMRectList` now carry their
`Symbol.toStringTag`, and `getClientRects()` is iterable.

A geometry conversion reads layout once per node: the four-point conversions
share the two origins, which is what keeps
`cssom-geometryutils-connected-callback.html` (300 elements times six markers)
inside the harness budget.

Evidence: `css/cssom-view` 1616 to **1760** passing subtests, no lost
subtests; `getClientRects-inline.html` moves FAIL to PASS.
`cssom-geometryutils-convertPointFromNode` 0/20 to 20/20,
`convertQuadFromNode` 0/18 to 18/18, `convertRectFromNode` 0/18 to 18/18,
`element-types` 1/41 to 41/41, `getBoxQuads-options` 0/17 to 17/17,
`lifecycle` 0/4 to 4/4, `same-origin-iframe` 0/2 to 2/2, `shadow-dom` 1/2 to
2/2, `fragmentation` 0/4 to 3/4, `transforms` 0/10 to 7/10,
`cssom-getBoxQuads-001` 0/6 to 6/6 and `-002` 0/1 to 1/1. `dom/events` stays
at 665, `css/cssom` 3674, `css/selectors` 4791, `intersection-observer` 185,
`resize-observer` 50, `html/dom` 65341, and `html/semantics/forms` 4524, all
with no lost subtests. `cargo fmt --check` (both trees), `tools/check clippy`,
and the renderer (175), webidl-bindgen (42), and wpt-harness (30) test
binaries pass; the two browser integration failures are the environment
failures that reproduce at the parent commit.

The `Navigator` surface now reports what the engine has.
`navigator.plugins` and `navigator.mimeTypes` are the Window's one empty
`PluginArray` and `MimeTypeArray`, and `pdfViewerEnabled` is false, which is
HTML's answer for a user agent without a PDF viewer
(<https://html.spec.whatwg.org/multipage/system-state.html#navigatorplugins>).
Both interfaces are legacy platform objects with named properties, which the
binding generator's prototype does not synthesize, so they are written with
the other legacy collections; an empty list answers null from every `item` and
`namedItem`, and a worker's `Navigator` carries none of the three members.
`window.clientInformation` is the `[Replaceable]` alias of `navigator`, and a
same-origin frame whose realm has not started answers it from the child's
pending `Navigator`, like `window.navigator`.

Evidence: `html/webappapis` 2607 to **2613** passing subtests, with no lost
subtests and no file-status changes. `clientinformation.window.html` 1/2 to
2/2, `plugins-and-mimetypes.html` 0/3 to 3/3, and `per-global.window.html`
2/6 to 4/6; the two remaining per-global failures are the realm-per-document
identity gap recorded in `docs/limitations.md`.
`css/cssom-view` stays at 1760, `dom/events` 665, `css/cssom` 3674, `html/dom`
65341, and `html/semantics/forms` 4524, all with no lost subtests.
`html/browsers`, which exercises the pending-frame proxy, measures 1012/2689
subtests and 537/758 files. `cargo fmt --check`, `tools/check clippy`, and the
renderer (175), webidl-bindgen (42), and wpt-harness (30) test binaries pass.

Smooth scrolling now runs on the document's animation timeline and each
scrolling box animates on its own. Blitz timed a smooth scroll with
`SystemTime::now()` and kept one animation slot for the whole document, so a
`scrollIntoView` of a container was cancelled when its viewport scroll
started. `BaseDocument::resolve` now stores the embedder's animation time in
`animation_time_ms`, which `start_scroll_animation` and
`resolve_scroll_animation` read, and `ScrollAnimations` holds one smooth scroll
per `ScrollTarget` beside the single fling
(<https://drafts.csswg.org/cssom-view/#scroll-an-element>). A user-initiated
scroll aborts the smooth scroll of the box it moves, not every smooth scroll in
the document. The renderer passes `crate::animation_seconds()` — the
`tokio::time` clock the WPT harness advances virtually — to every
`BaseDocument::resolve` call, so an animation, a timer, and a frame observe the
same time.

The viewport's scrollable overflow also unions the boxes Taffy hoists to the
initial containing block, which its root positioning pass produces after the
root's own layout: absolutely positioned content past the root's height now
extends the scroll range, while a `position: fixed` box does not
(<https://drafts.csswg.org/css-overflow-3/#scrollable>).

Evidence: `css/cssom-view` 1760 to **1897** passing subtests, no lost
subtests, and 209 of 240 files OK (from 208; `background-change-during-smooth-scroll.html`
moves TIMEOUT to OK). `scroll-behavior-main-frame-root` 2/40 to 39/40,
`main-frame-window` 2/28 to 27/28, `subframe-root` 20/40 to 39/40,
`subframe-window` 14/28 to 27/28, `scroll-behavior-element` 19/39 to 38/39,
`scroll-behavior-smooth-positions` 0/23 to 22/23, and
`scroll-behavior-default-css` 1/3 to 2/3. Every remaining failure in those
files is `waitForCompositorReady()`, which needs Web Animations
(`document.body.animate(...).finished`). `scrollIntoView-smooth.html` and the
two `scrollIntoView-multiple*` files stay TIMEOUT at 0/4: they await
`scrollend`, which the engine does not dispatch (recorded in
`docs/limitations.md`).

Regressions are clean: `css/css-transitions` 945 and `css/css-animations` 812
unchanged, and `dom/events` 665, `css/cssom` 3674, `css/selectors` 4791,
`intersection-observer` 185, `resize-observer` 50, `html/dom` 65341, and
`html/semantics/forms` 4524 all with no lost subtests. The loaded
`dom/events/scrolling` files and `css/css-scroll-snap` (21/173) match the
parent commit's results exactly, and the double-submit pair keeps alternating
its one flaky subtest (`form-double-submit-3.html` passes 1 of 3 isolated
runs). `cargo fmt --check` (main tree; the fork's remaining diffs are
pre-existing), `tools/check clippy`, and the renderer (175), webidl-bindgen
(42), and wpt-harness (30) test binaries pass.

Programmatic scrolling now reports its `scroll` and `scrollend` events. Blitz
queues one `scroll` entry per scrolling box that moved and one `scrollend`
entry when the operation completes — at once for an instant scroll, on the
frame a smooth one reaches its target — and the renderer runs the scroll steps
in a task, after the assignment, with the `scroll` entries before the
`scrollend` entries
(<https://drafts.csswg.org/cssom-view/#scrolling-events>). A viewport event
targets the `Document` and bubbles; an element's targets the element and does
not. A host that sleeps until the next deadline now gets a 16 ms frame deadline
while a scroll animation runs, so a smooth scroll advances at rendering
opportunities instead of jumping straight to the next page timer; that is what
lets a page await `scrollend` without also polling the offset.

`scrollIntoView` also carries the distance its inner scrollers are about to
scroll into the outer alignment. The scrolls of one call start together, so an
outer box measured from the pre-scroll layout would put a nested target at the
wrong offset: `scrollIntoView-multiple-nested.html` expected 600 and got the
clamped 800. Only an animation still owes layout that distance; an instant
scroll is already in the offsets the next measurement reads.

Evidence: `css/cssom-view` 1897 to **1911** passing subtests, no lost
subtests, and 213 of 240 files OK (from 209).
`scrollIntoView-smooth.html` 0/4 to 4/4 (TIMEOUT to OK, likewise
`scrollIntoView-multiple.html` and `-nested.html`),
`scroll-behavior-scrollintoview-nested.html` 1/2 to 2/2, and
`smooth-scroll-in-load-event.html` 0/1 to 1/1 (TIMEOUT to OK).
`dom/events/scrolling` 6 to **14** passing subtests over its 9 loadable files
with 8 of 9 files OK (from 6), including both scroll-target files,
`scroll-events-fired-once-per-frame-across-display-none.html`,
`scroll-event-not-fired-on-ancestor-style-change.html`, and
`scrollend-event-fired-after-instant-scroll-in-microtask.html`.
`css/css-scroll-snap` 153 to **158** with 137 of 173 files OK (from 125), as
twelve re-snap and snap-event files stop timing out.

Regressions are clean: `css/css-transitions` 945 and `css/css-animations` 812
unchanged; the `dom/events`+`css/cssom`+`css/selectors`+observer batch is 9371
with 8 gains from the scrolling files and 2 flaky `insertRule-charset-no-index`
losses that pass 3/3 in isolation; `html/webappapis` 2613 (the rAF
same-dispatch-time file, which passes in isolation); `html/dom` and forms 69864
with the double-submit flake, which fails 3/3 at the parent commit; and
`html/browsers` identical at 1012/2689 subtests and 537/758 files.
`cargo fmt --check` (main tree), `tools/check clippy`, and the renderer (175),
webidl-bindgen (42), and wpt-harness (30) test binaries pass.

The Selection API now exists. `crates/renderer/src/js/scripts/web/selection.js`
installs the `Selection` interface plus `Document.getSelection` and
`Window.getSelection`, and holds one `Range` by strong reference, so
`getRangeAt(0)` returns the object `addRange` stored and a mutation that moves
the live range moves the selection with it
(<https://w3c.github.io/selection-api/#selection-interface>). Anchor and focus
derive from the stored direction, a boundary point outside the document tree
reads as `null` with offset `0` (a foreign, detached, or template document's
nodes do not belong to the selection's document tree), and `getSelection`
answers `null` on a document with no browsing context. The `Range` arguments
run WebIDL's interface conversion first, so `removeRange(null)` is a
`TypeError` rather than the algorithm's `NotFoundError`
(<https://webidl.spec.whatwg.org/#es-interface>). Direction is `forward` when
the focus is not before the anchor, including the equal case.

The webidl-bindgen JavaScript path also gained argument defaults for types
other than strings: boolean, integer (decimal, hexadecimal, and octal literals,
emitted as decimal), floating point (including `NaN` and the infinities), and
`null`. Each default is checked against the argument's type and converted like
a supplied value, and an integer literal is accepted for a `double`
(<https://webidl.spec.whatwg.org/#idl-default-values>).

Evidence, against a stash build of the parent commit: `selection` 48 to
**33,364** passing subtests with no lost subtests and 92 of 151 files OK (from
49). `dom/ranges` 40,269 to **41,694** (+1,425) with no lost subtests: the
1,415 `getSelection is not defined` failures from `Range-mutations.js` now run
their selection half, and `selection/getSelection.html` passes 15 of 18,
`setBaseAndExtent.html` 96 of 120, `isCollapsed.html`, `type.html`,
`removeAllRanges.html`, `collapseToStartEnd.html`, and every `addRange-*` file
pass. The three remaining `getSelection.html` failures are the pending-realm
gap (a child frame's realm starts after the task that appended it), and the
rest need a caret engine: `modify`, `deleteFromDocument`, `getComposedRanges`,
`selectionchange` from a gesture, and text-control selection, all recorded in
`docs/limitations.md`.

Regressions are clean: `css/cssom-view` stays at 1,911 with no lost subtests;
the `dom/events`+`css/cssom`+`css/selectors`+observer batch is 9,371
unchanged; `html/dom` and forms 69,865 (the known flaky double-submit pair
contributes the one difference); `html/webappapis` 2,613; and `html/browsers`
1,012/2,689 subtests and 537/758 files, all unchanged. `cargo fmt --check`,
`tools/check clippy`, and the renderer (175), webidl-bindgen (43, including a
new argument-default test), and wpt-harness (30) test binaries pass.

`Document.elementFromPoint()` and `elementsFromPoint()` now use Blitz's hit
testing instead of a box-containment scan over a flat tree-order list. The
renderer had its own approximation: every element's border box, the deepest
containing one wins, no transforms, no stacking order, no overflow clipping,
no `pointer-events`, no text runs. The fork's `Node::hit_inner` already walked
paint order (a Blitz feature the renderer was not using), so the renderer now
calls it through `BaseDocument::element_from_point` and a new
`elements_from_point` (blitz@fce5b5524). The walk fills a `HitCollector`: the
`first()` form keeps the topmost box for pointer targeting and scrollbar
thumbs, and `all()` keeps every painted box, which is what
`elementsFromPoint()` returns — the full paint-order stack, deduplicated per
element, rather than the hit element plus its ancestors.

Three corrections came with it. Hit testing resolves style and layout first
(`resolve_until_settled`), because a page may ask right after a mutation. A
document that is not a realm's main document (a `createHTMLDocument` result,
an `XMLDocument` from `DOMParser`) has no viewport, so both methods answer
`null`/`[]` instead of reading another document's layout. And CDP's
`DOM.getNodeForLocation` names a point in page coordinates, so the renderer
subtracts the viewport scroll before the CSSOM View hit test.

A box with no area is no longer its own hit target: a zero-height block's top
edge satisfies the inclusive edge test, so `elementFromPoint(0, 0)` used to
name `head`. `NodeBox.visible` lost its last reader and is gone.

Evidence, against `cv9` (css/cssom-view at 1,911 passing subtests):
**1,934** passing subtests (+23 net), files OK 213 → 212. Gained:
`elementFromPoint-002/003` "dynamic changes to visibility" (now OK),
`elementFromPoint-dynamic-anon-box`, `elementsFromPoint`'s
`pointer-events`, SVG, and empty-query cases (8 of 9, from 4), the four
`elementsFromPoint-inline-*` writing-mode files, `elementsFromPoint-simple`,
`elementsFromPoint-table`, `elementsFromPoint-invalid-cases`,
`negativeMargins`, `elementFromPoint`'s `pointer-events` and "no hit target"
cases (8 of 11, from 6), and `elementFromPosition` (16 of 16, from 11).

Three subtests are lost and accounted for. Two are
`cssom-geometryutils-connected-callback.html`, which runs at ~4.8 s against a
5 s file budget (~4.63 s at the parent) and flips to ERROR/TIMEOUT in a batch
run; it passes alone, repeatedly. The third is
`elementFromPoint-negative-z-index-inline.html` "Scrolling preserves hit
testing of negative z-index inline content": it passed before because the old
hit test agreed with an equally-wrong `getBoundingClientRect()`, and the
point it computes is now correctly outside the viewport. Its two root causes
are pre-existing and independent of this change: Parley starts each line's
`text_wrap_mode` at `Wrap`, so `white-space: nowrap` does not stop an atomic
inline from wrapping, and `physical_unrounded_geometry` subtracts an
element's own scroll offset. Both are recorded in `docs/upstream.md` and
`docs/limitations.md`.

Regressions are clean: the `dom/events`+`css/cssom`+`css/selectors`+observer
batch is 9,371 unchanged with no lost subtests; `html/dom` and forms 69,865 →
69,866 (the known alternating double-submit flake, one subtest the other way);
and `html/webappapis` 2,613 unchanged. `cargo fmt --check`, `tools/check
clippy`, and the renderer (175), webidl-bindgen (43), and wpt-harness (30)
test binaries pass.

`clientWidth`/`clientHeight` and `scrollWidth`/`scrollHeight` now implement
the CSSOM View steps for the root element and the quirks-mode body. Both
viewport branches were missing: `clientWidth`/`clientHeight` special-cased
only the root element (outside quirks mode), and `scrollWidth`/`scrollHeight`
had no viewport branch at all, so a quirks-mode document answered the body's
own box (240x90 instead of the viewport's 600) and a non-quirks document's
root answered its content height (108 instead of 600 for an empty page).

The steps are now shared per pair. `client_size` answers zero for a boxless
element, the viewport for the root outside quirks mode or the body in quirks
mode, and the element's padding edge otherwise. `scroll_size` answers the
viewport's scrolling area for the root outside quirks mode and for a
quirks-mode body that is not potentially scrollable on the queried axis, and
the element's own scrolling area otherwise; `Scrolling`'s existing
`potentially_scrollable_in`, `root_element`, and `body_element` are the same
helpers the scroll getters use, so the two families cannot disagree about
which element scrolls the document. The viewport's scrolling area is the
root element's scrolling area floored at the viewport extent, because the
viewport's scrolling area always covers the initial containing block
(<https://drafts.csswg.org/cssom-view/#dom-element-scrollheight>).

Evidence: `css/cssom-view` 1,934 to **1,939** passing subtests with no lost
subtests, files OK 212 to **213**. `HTMLBody-ScrollArea_quirksmode.html`
passes 10 of 10 (from 7), and `scrolling-quirks-vs-nonquirks.html` gains
"clientWidth/clientHeight on the root element in quirks mode" and on the body.
The rest of the suite is unchanged, including its 10 error and 4 timeout
files. Regressions are clean: the
`dom/events`+`css/cssom`+`css/selectors`+observer batch is 9,371 unchanged
with no lost subtests; `css/css-scroll-snap` 158 unchanged; `html/dom` and
forms 69,865 (the one difference is the known alternating double-submit
flake); `html/webappapis` 2,613. `cargo fmt --check`, `tools/check clippy`, and
the renderer (175), webidl-bindgen (43), and wpt-harness (30) test binaries
pass.

A table with captions now lays out as the wrapper box CSS Tables 3 requires
(blitz@095b629bc). Blitz had dropped captions from layout entirely ("Probably
a table caption: ignore"): the table's box was the grid, the captions were
never laid out or painted, and CSSOM View measured the grid alone. The wrapper
is an anonymous block whose children are the top captions, the grid, and the
bottom captions, and it carries the table's margin, position, insets, float,
and clear; the grid keeps the border, padding, and size. The grid's own
algorithm is untouched — its existing `TableTreeWrapper` is the wrapper's grid
child.

The wrapper sizes itself to the wider of the grid and the captions' margin
boxes, clamped to the containing block only when that block is wider than the
grid: a shrink-to-fit ancestor is sized from the table, so clamping there
would shrink a wider caption. Captions are then moved onto the grid's origin,
because their DOM parent is the table element whose box is the grid. The
element's stored layout is re-expressed as the grid box, so its border,
background, and hit-testing region cover the table rather than the captions,
while `Node::table_wrapper` reports the wrapper box to
`getBoundingClientRect()`/`offsetWidth`/`offsetHeight` and
`offset_top_left` measures from the wrapper's padding edge.

Two smaller corrections came with it. Border-spacing padding is skipped only
when the grid is empty: explicit `<col>`/`<tr>` tracks still generate
anonymous cells, which keep their spacing (`html5-table-formatting-2.html`).
And a caption whose style changes rebuilds the table box, because the wrapper
orders captions by `caption-side` and caches their margin boxes when the
table is built; before, a runtime `caption-side` flip left the caption where
it was.

Evidence, against the pre-change baseline: `css/css-tables` 481 → **492**
passing subtests with **no lost subtests**, gaining
`caption-relative-positioning.html`, `tentative/caption.html` (4 subtests),
`caption-side-1.html` (2), `caption-writing-mode-001.html` (2),
`tentative/table-minmax.html` (3), `tentative/tbody-height-redistribution.html`,
and `width-distribution/computing-table-width-0.html`;
`paint/table-border-paint-caption-change.html` (a reftest) passes once the
harness runs the page's frames, below. `css/cssom-view`
`offsetTopLeft-table-caption.html` passes and `table-offset-props.html` goes
from 5 of 13 to 12 of 13; the suite is 1,947 → **1,949** with no lost
subtests.

The reftest harness now runs a page's pending frame callbacks before it
captures (`SCREENSHOT_FRAMES` in `tools/wpt-harness/src/engine.rs`): a
screenshot is a rendering opportunity, so the callbacks queued with
`requestAnimationFrame` run first and a callback that queues another gets the
next frame, bounded to three frames so a self-requeueing callback cannot hold
the capture. That recovers `table-border-paint-caption-change.html` and
`html/webappapis/animation-frames/nested-raf-not-sync.html`, and it exposes one
pre-existing paint gap: the canvas background is propagated from the body for
`background-color` only, so `resize-observer/devicepixel2-ref.html` (a body
`background-image`) renders blank and its test now reports the difference.
Its sibling `devicepixel.html` already failed on the same gap. Recorded in
`docs/limitations.md`.

`devicePixelContentBoxSize` now snaps each edge to the nearest device pixel
instead of flooring the size, so a box that starts mid-pixel reports the
pixels it covers (<https://drafts.csswg.org/css-box-4/#snap>).

Regression batches: the `dom/events`+`css/cssom`+`css/selectors`+observer
batch is 9,371 unchanged with no lost subtests; `html/dom` and forms 69,865 →
69,866 (the known alternating double-submit flake, a different file this
time); `html/webappapis` 2,613 → 2,614 (the nested-rAF gain above);
`css/css-scroll-snap` 158 unchanged. `cargo fmt --check`, `tools/check
clippy`, and the renderer (175), webidl-bindgen (43), and wpt-harness (30)
test binaries pass.

The canvas background now propagates images, not just a colour (blitz@c83bacef7).
Blitz painted a background colour from the body onto the canvas and nothing
else, so a page whose canvas background was a body `background-image` or
gradient rendered blank — which is exactly what
`css-backgrounds/background-image-shared-stylesheet-ref.html` and
`resize-observer/devicepixel2-ref.html` paint. The paint pass now draws the
whole background (colour, images, gradients) of the root element, or of its
first `body` child when the root's `background-image` is `none` and its
`background-color` is transparent, over the canvas; an element that generates
no box leaves the canvas transparent. The painting area is always the canvas
(at least the viewport), while images are sized and positioned against the root
element's box, as the spec says; `background-clip` does not narrow a canvas
background, so `clip: text` paints the canvas rather than the glyphs, and the
source element stops painting its own background because the propagated values
replace its used values. The source's outset `box-shadow` is clipped to its
border box, since it no longer paints a background that could cover the shadow.

Tiling had to follow. `repeat` and `round` raster tiles now cover the union of
the positioning and painting boxes rather than stopping at the positioning box:
the image sampler repeats inside a single quad, so the quad is what has to
reach the painting area, and its phase stays anchored at the positioning box.
`space` keeps its count and stride from the positioning area and then repeats
that pattern over the painting area, except when only one image fits, where one
image is placed at `background-position`. Gradient layers use the same painting
area. That last rule is what keeps
`background-repeat-space-8.html` honest: its `space` box now fills the border
area like its `repeat` reference, while its single-tile gradient box still
matches the `no-repeat` reference.

Evidence against the pre-change run of the same 261-file `css/css-backgrounds`
slice: eight reftests flip to passing — `background-attachment-margin-root-002`,
`background-color-body-propagation-004`,
`background-color-root-propagation-001`,
`background-color-root-propagation-002`,
`background-image-shared-stylesheet`, `origin-content-box`,
`origin-content-box_with_radius`, and `gradient-repeat-spaced-with-borders` —
with **no lost tests** and `background-repeat-space-8` still passing.
`resize-observer/devicepixel2.html` passes again as well, recovering the
file-level regression the table-caption slice's frame-settling exposed.
`css/css-tables` stays at 492 passing subtests and `css/cssom-view` at 1,949,
both with no losses (its `cssom-geometryutils-connected-callback.html` flake
timed out in the batch and passes 3 of 3 in isolation). Regression batches lose
nothing: `dom/events`+`css/cssom`+`css/selectors`+observers 9,371 → 9,373,
`html/dom` and forms 69,866 → 69,867, `html/webappapis` 2,613 (the nested-rAF
test flips on `Date.now()` granularity in both directions), `css/css-scroll-snap`
158.

CSSOM View now applies its `zoom` rules (blitz@15cbe6725). CSS `zoom` was
already applied by layout, but every geometry member returned the zoomed
layout length, so a `zoom: 4` box reported a 256px `clientWidth` for a 64px
box and `offsetWidth` grew with the zoom. The spec splits the members
(<https://drafts.csswg.org/css-viewport/#zoom-dom-cssom>):
`getBoundingClientRect()`, `getClientRects()`, and `IntersectionObserver`
return scaled lengths (which the layout values already are), and every other
geometry API is unscaled. `offsetLeft`/`offsetTop`/`offsetWidth`/`offsetHeight`,
`clientTop`/`clientLeft`/`clientWidth`/`clientHeight`,
`scrollTop`/`scrollLeft`/`scrollWidth`/`scrollHeight`, and the coordinates
passed to `scrollTo`/`scrollBy` now divide or multiply by the element's
effective zoom (`crates/renderer/src/js/bindings/node/zoom.rs`), and
`Element.currentCSSZoom` returns that zoom, or 1 for an element that is not
rendered.

Rounding had to follow. CSSOM View rounds `offsetTop` once, at the end, so the
offset walk now sums the unrounded layout: a `zoom: 0.5` box at y = 5.5
reports 11, not 12. `unrounded_layout` is written in the placer's writing mode,
so nodes placed by a vertical box use the rounded physical layout instead.
Paint snaps its scroll offsets to whole CSS pixels, because Taffy's final
layout is integer-rounded and an unrounded offset left a strip of the
scrollport's own background at its leading edge.

Evidence against the pre-change `css/cssom-view` run: 1,949 → **1,953**
passing subtests. `scrollTo-zoom.html` flips FAIL → PASS, and the gains are
`Element-currentCSSZoom.html` (both subtests), `client-props-zoom.html` (1),
`offsetTop-offsetLeft-with-zoom.html` (the offset-width subtest), and
`scroll-zoom.html` (both scroll-size subtests). The only lost subtests are the
two in `cssom-geometryutils-connected-callback.html`, which exceeds the 5 s
budget at 5.20 s in the batch and does so identically on the stashed baseline
build, so it is a pre-existing speed work item, not a regression.
`css/css-tables` stays at 492 passing subtests, `css/css-backgrounds` at 4,616
with no losses, and `css/css-viewport/zoom` at 346 with no losses.

`HTMLImageElement.x`/`y` are implemented, with the zoomed replaced-element
size underneath them (blitz@bf3bb8346). `x`/`y` return the scaled coordinate of
the first box's left/top border edge relative to the initial containing block
origin, ignoring transforms
(<https://drafts.csswg.org/cssom-view/#dom-htmlimageelement-x>), which is the
document-relative box position: the viewport's scroll offset is added back to
`get_client_bounding_rect`'s viewport-relative rectangle. A boxless image
answers zero. The intrinsic-size bug that `image-x-y-zoom.html` exposed is
fixed too: an intrinsic size is a physical length, so a replaced element's
intrinsic width and height and its 300x150 default object size are now scaled by
its effective zoom. A `width: auto` 1x1 image inside a `zoom: 4` box lays out
4px tall instead of 1px, and the image whose own `zoom` is 2 inside that box is
8px tall, which puts the container's four images at y = 8, 9, 148, 112, and
120 as the test expects.

Evidence: `css/cssom-view/image-x-y-zoom.html` 1/2 -> 2/2,
`cssom-view-img-attributes-001.html` 4/4 (including the `display: none`
zeros), and `HTMLImageElement-x-and-y-ignore-transforms.html` 1/1. In the full
`css/cssom-view` run the file count goes 1,949 -> **1,959** passing subtests
with the same two `cssom-geometryutils-connected-callback.html` budget losses
that predate the zoom work; `css/css-viewport/zoom` stays at 346,
`css/css-tables` at 492, and `css/css-backgrounds` at 4,616, all with no
losses.

`document.domain` and the same-origin-domain relation are implemented, and the
WindowProxy now follows the cross-origin access rules instead of answering
`undefined`. Origins carry an optional domain set by the setter: a plain tuple
origin starts with a null domain, and an `about:blank` or `srcdoc` document
shares its creator's domain cell because it inherits the creator's *origin
object*, so an assignment on either side is visible to both. The setter runs
the spec's rejections — no browsing context, the sandboxed `document.domain`
flag, an opaque origin, and a value that is not a registrable domain suffix of
the effective domain — and the last rule uses the vendored public suffix list,
so `document.domain = "test"` throws. Same-origin-domain is the spec's
comparison: equal non-null domains with the same scheme, or same origin with
both domains null
(<https://html.spec.whatwg.org/multipage/browsers.html#same-origin-domain>).

Every WindowProxy decision now goes through that relation: reaching the target
window, `frameElement`, `EventTarget` forwarding, and `contentDocument`.
Cross-origin reads follow the WindowProxy algorithm: the cross-origin property
set answers, a named child navigable answers its `WindowProxy` (so
`parent[name]` works from a sibling), and every other member throws
`SecurityError`; `location` is a null-prototype `Location` whose members throw,
and `opener`, `focus`, and `blur` answer without throwing. `document.open()`
keeps its plain same-origin check, which `document.domain` does not widen.

Evidence against the pre-change run of the same directories:
`document_domain.html` 0/4 -> 4/4, `document_domain_access_details.sub.html`
2/12 -> 10/12, `document_domain_setter.html` 0/3 -> 2/3,
`document_domain_setter_srcdoc.html` 3/3 (unchanged),
`sandboxed-document_domain.html` 0/5 -> 5/5, and
`window-security.https.html` 13/124 -> 122/124 (the two remaining subtests
want `window.external` and `onmousewheel`, which are recorded missing
surfaces). `html/dom` and forms lose nothing and gain the two
`self-origin.sub.html` document.domain subtests, the events/CSSOM/selectors
batch loses nothing, and `html/webappapis` is unchanged. The remaining three
subtests are the Location-revocation and entry-realm gaps recorded in
`docs/limitations.md`.

`window.visualViewport` is implemented. The engine has no pinch zoom and no
visual chrome that scrolls apart from the layout viewport, so the visual
viewport always coincides with it: `offsetLeft`/`offsetTop` are 0, `scale` is
1, `width`/`height` are the layout viewport's, and `pageLeft`/`pageTop` are the
page's scroll offset
(<https://drafts.csswg.org/cssom-view/#the-visualviewport-interface>). The
object is a `[SameObject, Replaceable]` `EventTarget`; `resize` fires when the
viewport size changed since the last report (the spec's "run the resize
steps"), and `scroll`/`scrollend` follow the viewport's own scroll steps.

Two things underneath had to be fixed to get there:

- A bare `scrollTo(...)` (and `scroll`/`scrollBy`) in a strict-mode script
  passes `this` as `undefined`. WebIDL replaces a null or undefined `this` with
  the realm's global object before the interface check
  (<https://webidl.spec.whatwg.org/#es-operations>), so the shared scroll
  wrapper now substitutes `globalThis` first; without it every
  `scrollTo(1000, 1200)` in a page's own script silently did nothing.
- A same-origin child whose realm has not started answers `visualViewport`,
  `scroll`, `scrollTo`, and `scrollBy` through its `WindowProxy`, bound to the
  child frame's viewport metrics and scroll offset, instead of `undefined`.

Evidence: `visual-viewport` goes from 3/15 files (11 errors, 1 timeout) to
**15/15**, including the iframe variants that read `frames[0].window` in the
same task. Regression batches are clean: `dom/events`+`css/cssom`+
`css/selectors`+observers 9,374 passing subtests with no losses,
`html/dom`+forms 69,866 → 69,868 with no losses (the two gains are the
`form-double-submit-*` flake, which recovers), `html/webappapis` 2,613
unchanged, and `css/cssom-view`+`css/css-scroll-snap` 2,124 unchanged.

The engine now fires CSS animation and transition lifecycle events. Blitz
already advanced every animation on the document timeline, so the fork
(`third_party/blitz` 736a87b86) records the state changes the tick observes and
the renderer dispatches them as trusted `AnimationEvent` and `TransitionEvent`
at the target element:

- `animationstart` (an animation leaves its delay phase), `animationiteration`
  (a boundary is crossed), `animationend` (the last iteration finishes), and
  `animationcancel`;
- `transitionrun` (a transition is created), `transitionstart`,
  `transitionend`, and `transitioncancel`.

Each event carries the animation name or transitioned property, the
originating pseudo selector, and `elapsedTime`
(<https://drafts.csswg.org/css-animations-1/#events>,
<https://drafts.csswg.org/css-transitions-1/#transition-events>). The DOM
legacy rewrite is implemented for trusted events: a listener list with no
unprefixed listener is invoked a second time with the event type rewritten to
`webkitAnimationStart`/`webkitAnimationIteration`/`webkitAnimationEnd`/
`webkitTransitionEnd`, then the type is restored
(<https://dom.spec.whatwg.org/#concept-event-listener-invoke>). A document with
a resize or transition that can finish gets a rendering opportunity each frame,
which is what advances the tick; an infinite animation is excluded so a
crashtest with one still reaches quiescence.

Evidence, subtest diffs against saved baselines with no loss:
`dom/events` 674 -> **716** (the five `webkit-*-event` files and
`EventListener-invoke-legacy.html` go from timeouts to 58/58),
`css/css-transitions` 945 -> **976**, and `css/css-animations` 812 -> **813**.
Regression batches are clean: events/CSSOM/selectors/observers 9,374 -> 9,416
with no losses, `html/dom`+forms 69,868 -> 69,868 (one `form-double-submit-*`
flake, which recovers in isolation), `html/webappapis` 2,613 unchanged, and
`css/cssom-view`+`css/css-scroll-snap` 2,124 unchanged. `cargo fmt --check`,
`tools/check clippy`, and the renderer (176), wpt-harness (29), and one other
test pass.

Table `clientWidth`/`clientHeight`/`clientLeft`/`clientTop` and
`scrollWidth`/`scrollHeight` now follow CSSOM View's table rules. A table's
client size is its offset size — the table wrapper box when it has captions,
else the grid's border box — and not the generic padding edge, which dropped
the captions and subtracted the grid's own border; `clientLeft`/`clientTop` are
0 because the wrapper box has no border; and the scroll size is at least that
box. The renderer reuses Blitz's offset rectangle for the table, so the client
and offset members cannot disagree
(<https://drafts.csswg.org/cssom-view/#dom-element-clientwidth>,
<https://drafts.csswg.org/css-tables-3/#table-wrapper-box>).

Evidence: `table-client-props.html` 4/15 -> **13/15**, `table-scroll-props.html`
4/13 -> **12/13**, and the two spec tests
(`table-with-border-client-width-height.html`,
`table-border-separate-client-width-height.html`) 1/1. `css/cssom-view` goes
from 1,966 to **1,984** passing subtests with no losses. `css/css-tables` is
unchanged at 492, `css/cssom` at 3,674, and `html/dom` at 65,343, all with no
losses. The three remaining subtests are the collapsed-border half extents,
recorded in `docs/limitations.md`. `cargo fmt --check`, `tools/check clippy`,
and the renderer (176), wpt-harness (29), and one other test pass.

`HTMLElement.translate` now reports and sets the element's translation mode.
The getter walks ancestor-or-self: the nearest HTML element whose `translate`
attribute is in the yes state (including the empty value) answers true, one in
the no state answers false, and the inherit state, a non-HTML element, or a
missing parent falls through to translate-enabled. The setter writes `yes` or
`no` as a content attribute, so it runs CEReactions like any other reflection
(<https://html.spec.whatwg.org/multipage/dom.html#the-translate-attribute>).
`html/dom/elements/global-attributes`' translate files go from 0/16 to
**16/16** and `html/dom` from 65,343 to **65,358** passing subtests with no
losses. `cargo fmt --check`, `tools/check clippy`, and the renderer (176),
wpt-harness (29), and one other test pass.

`link`, `script`, and `style` now expose the `blocking` `DOMTokenList`, and
`DOMTokenList.supports()` knows its supported tokens. The list is
`[SameObject, PutForwards=value]`, so it reuses the same object cache that
`classList` and `sandbox` use (a shared `token_list` helper now builds all
three), and `supports("render")` answers true while any other token answers
false; a list without supported tokens still throws the spec's `TypeError`
(<https://html.spec.whatwg.org/multipage/urls-and-fetching.html#blocking-attributes>,
<https://dom.spec.whatwg.org/#dom-domtokenlist-supports>). The blocking engine
behavior itself is separate work. `blocking-idl-attr.html` goes from 0/6 to
**6/6**; `dom/nodes` + `dom/collections` + `html/dom` goes from 79,920 to
**79,926** passing subtests with no losses. `cargo fmt --check`,
`tools/check clippy`, and the renderer (176), wpt-harness (29), and one other
test pass.

`ToggleEvent` is exposed as a constructor with the `oldState`, `newState`, and
`source` members, the interface `toggle` events use on a `details` element and
on a popover. The `EventInit` conversions follow WebIDL's `DOMString` rules
(`undefined` is the default empty string, `null` is `"null"`, an array is
joined), and `source` is retargeted against the event's target on every read
with the DOM retarget algorithm, so a source inside the same shadow tree as
the target answers that tree's host
(<https://html.spec.whatwg.org/multipage/popover.html#the-toggleevent-interface>,
<https://dom.spec.whatwg.org/#retarget>). `toggleevent-interface.html` goes from
0/40 to **40/40** and `html/semantics/popovers` from 2 to **42** passing
subtests with no losses; the popover engine behavior itself is separate work.
Regression batches are clean: `dom/events` 716, `html/dom` 65,364,
`html/webappapis` 2,613, all unchanged or up with no losses. `cargo fmt
--check`, `tools/check clippy`, and the renderer (176), wpt-harness (29), and
one other test pass.

`details` elements now fire their `toggle` event on any `open` attribute change,
not only when a `summary` is activated: the IDL setter, `setAttribute`, and
`removeAttribute` all reach the same attribute-change hook. The event is a
trusted `ToggleEvent` that reports the state the burst started from and the
state it ends at, does not bubble, and is not cancelable; a second change
before the queued task runs updates only the end state, so `open` then
`removeAttribute("open")` fires once with `oldState: "closed"` and
`newState: "closed"`
(<https://html.spec.whatwg.org/multipage/interactive-elements.html#queue-a-details-toggle-event-task>).
`toggleEvent.html` goes from 1/11 to **9/11** and
`html/semantics/interactive-elements` from 43 to **52** passing subtests with
no losses. The two remaining subtests need the parser's `open` attribute set to
queue the task (the parser path has no attribute-change hook yet), recorded in
`docs/limitations.md`. Regression batches are clean: `html/dom` 65,364,
`dom/events` 716, and forms 4,525 with no losses (one known
`form-double-submit-*` flake recovers in isolation). `cargo fmt --check`,
`tools/check clippy`, and the renderer (176), wpt-harness (29), and one other
test pass.

`navigator.windowControlsOverlay` now exists as a `WindowControlsOverlay`
object. The UA has no window controls overlay, so `visible` is false, the
`[SameObject]` instance is not constructible, `getTitlebarAreaRect()` answers
an empty `DOMRect`, and `ongeometrychange` is a real event handler attribute
that never runs because no `geometrychange` can fire
(<https://wicg.github.io/window-controls-overlay/#windowcontrolsany>).
`navigator-window-controls-overlay.tentative.html` goes from 0/7 to **7/7**
and `html/webappapis` from 2,613 to **2,622** with no losses. `cargo fmt
--check`, `tools/check clippy`, and the renderer (176), wpt-harness (29), and
one other test pass.

`dialog` elements now implement the non-modal half of their API: `show()`,
`close(returnValue)`, `requestClose(returnValue)`, `returnValue`, and the
computed `closedBy` state. `show()` and `close()` fire a `beforetoggle`
`ToggleEvent` synchronously (cancelable for `show`), queue the coalesced
`toggle` task, and `close()` queues the `close` event on the user interaction
task source; `requestClose()` fires a cancelable `cancel` event first and
ignores `closedby`. The dialog focusing steps focus an autofocus descendant,
then the first focusable descendant, then the dialog, and `close()` restores
the previously focused element. A `method=dialog` form submission now closes
the form's nearest ancestor dialog instead of navigating, with the submitter's
optional value as the result
(<https://html.spec.whatwg.org/multipage/interactive-elements.html#the-dialog-element>,
<https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#form-submission-algorithm>).
`showModal()` stays absent because it needs a top layer, document blocking,
and inertness, none of which Blitz has; the gap is recorded in
`docs/limitations.md`. `html/semantics/interactive-elements` goes from 43 to
**88** passing subtests with no losses, and the `html/dom` (65,364),
`dom/events` (716), forms (4,525), and `html/webappapis` (2,622) batches are
unchanged. `cargo fmt --check`, `tools/check clippy`, and the renderer (176),
wpt-harness (29), and one other test pass.

`HTMLFormElement` now implements its legacy indexed and named properties.
`form[0]` returns the same elements as `form.elements`, `form[name]` answers a
control by `id`/`name` (listed elements first, then form-associated `img`), a
name that matches several controls answers a cached live `RadioNodeList`, and
the past names map keeps a name after the control stops carrying it, dropping
entries when the element changes form owner. Named properties override
prototype members and are non-enumerable
(`[LegacyOverrideBuiltIns]`, `[LegacyUnenumerableNamedProperties]`), writes and
deletes to a supported name or index fail, and `[[OwnPropertyKeys]]` lists the
indices then the supported names
(<https://html.spec.whatwg.org/multipage/forms.html#dom-form-nameditem>,
<https://webidl.spec.whatwg.org/#legacy-platform-object>). The form-owner
lookup now scopes a `form` attribute's id search to the element's own tree, so
a control inside a shadow root no longer resolves a form in the host's tree.
`form-nameditem.html` goes 6/17 to 17/17, `form-indexed-element.html` 0/2 to
2/2, `form-indexed-property-names.html` 0/1 to 1/1, and
`form-elements-matches.html` 1/2 to 2/2; `form-elements-filter.html` stays 1/2
because Blitz's file-input browse button is a light-DOM child (recorded in
`docs/limitations.md`). The `html/dom` + `dom/events` + forms batch goes from
70,605 to 70,626 passing with nothing lost, and `html/webappapis` stays at
2,622. `cargo fmt --check`, `tools/check clippy`, and the renderer (176),
wpt-harness (29), and one other test pass.

`HTMLDataListElement.options` now returns the live `HTMLCollection` of the
datalist's `option` descendants
(<https://html.spec.whatwg.org/multipage/form-elements.html#dom-datalist-options>),
and `input.list` resolves its id in the input's own tree, so a detached tree
still finds its datalist
(<https://html.spec.whatwg.org/multipage/input.html#dom-input-list>). The id
search used by the form-owner and image-owner lookups goes through one
`firstElementWithId` helper that walks an element root's `[id]` descendants,
because an element root has no `getElementById`. `html/semantics/forms/the-datalist-element`
goes 4/6 to 6/6 files, and the `html/dom` + `dom/events` + forms batch goes from
70,626 to 70,628 with nothing lost.

`labels` is now `[SameObject]`: one live `NodeList` per element, cached in a
per-realm `WeakMap`, so a retained reference answers the current label set and
goes empty once the element stops being labelable
(<https://html.spec.whatwg.org/multipage/forms.html#dom-labels>). The radio
button group rule now also runs on a type change: an input that becomes a
checked radio unchecks the other elements in its group, as "a type change is
signalled" requires
(<https://html.spec.whatwg.org/multipage/input.html#radio-button-group>).
`labelable-elements.html` goes 25/26 to 26/26 and `radio-morphed.html` 0/1 to
1/1; the `html/dom` + `dom/events` + forms batch gains both with nothing lost
(the `form-double-submit-3.html` subtest flipped again, a known
timing-dependent flake that fails in isolation).

`:required`, `:optional`, `:read-write`, and `:read-only` now match. Blitz
answered `false` for all four, so `input:required { … }` never applied even
though `ElementState` already had the bits; `ElementData::sync_form_selector_state`
derives them from the element's attributes and runs at construction and on
every `required`/`readonly`/`type`/`contenteditable`/`disabled` change
(<https://html.spec.whatwg.org/multipage/semantics-other.html#selector-required>,
<https://html.spec.whatwg.org/multipage/semantics-other.html#selector-read-write>).
`css/selectors/selector-required.html` passes and
`css/selectors/invalidation/input-pseudo-classes-in-has.html` goes 3/10 to 5/10;
`css/selectors` 4,791 to 4,793 and the `html/dom` + `dom/events` + forms batch
70,629 to 70,630, with nothing lost in either.

The `submit` event of a form submission is now fired by the engine with the
trust bit set: the shim builds the `SubmitEvent` (its prototype and
`submitter` stay shim-owned) and a host function dispatches it trusted, because
`HTMLElement.click()` starting a submission is a user-agent event
(<https://dom.spec.whatwg.org/#concept-event-dispatch>). `SubmitEvent`'s
`submitter` member now refuses a non-`HTMLElement` value with a `TypeError`, as
the nullable `HTMLElement?` conversion requires
(<https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#submitevent>).
`button-events.html` goes 8/9 to 9/9 and `SubmitEvent.window.html` 5/7 to 6/7;
the `html/dom` + `dom/events` + forms batch goes from 70,630 to 70,631 with
nothing lost (the `form-double-submit*.html` pair flipped, the known
timing-dependent flakes that also flip in isolation).

`FormDataEvent`'s `formData` member is `required FormData`, so the constructor
now throws `TypeError` for a value that is not a `FormData` instead of storing
it
(<https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#the-formdataevent-interface>).
`FormDataEvent.window.html` goes 1/2 to 2/2 and the `html/dom` + `dom/events` +
forms batch 70,631 to 70,633 with nothing lost.

`HTMLSelectElement` is a legacy platform object with an indexed getter, so
`select[0]` now answers the same options as `select.options`; a non-index key
falls through, and a write or delete of a supported index fails as an
indexed-only object requires
(<https://html.spec.whatwg.org/multipage/form-elements.html#dom-select-length>,
<https://webidl.spec.whatwg.org/#legacy-platform-object>). The wrapper's two
`Document`/`HTMLFormElement` booleans became one `LegacyShape`, so a wrapper
cannot claim two shapes. `common-HTMLOptionsCollection.html` goes 6/8 to 8/8
and `common-HTMLOptionsCollection-add.html` 1/3 to 3/3; the `html/dom` +
`dom/events` + forms batch goes from 70,633 to 70,637 with nothing lost.

`option.form` now answers the form owner of the option's nearest ancestor
`select`, found with the spec's algorithm: a `datalist`, `hr`, or `option`
ancestor, or a second `optgroup`, ends the search at null
(<https://html.spec.whatwg.org/multipage/form-elements.html#dom-option-form>).
`option_select_owner` uses that same algorithm, so an option nested inside one
of those elements is outside the select's list of options.
`option-form-ancestor-select.html` goes 0/2 to 2/2 and the `html/dom` +
`dom/events` + forms batch 70,637 to 70,640 with nothing lost.

`MouseEvent` now answers the CSSOM View coordinates: `x`/`y` (the client
coordinates under their CSSOM View names), `pageX`/`pageY` (client plus the
viewport's scroll offset), and `offsetX`/`offsetY` (client relative to the
target's padding edge, measured from its border box plus border width; a target
with no box has no padding edge, so the coordinate itself is the offset)
(<https://drafts.csswg.org/cssom-view/#dom-mouseevent-pagex>,
<https://w3c.github.io/uievents/#dom-mouseevent-offsetx>).
`dom/events/mouse-event-retarget.html` goes 0/1 to 1/1; the `html/dom` +
`dom/events` + forms batch gains it with nothing lost (the
`form-double-submit*.html` pair flipped, the known timing-dependent flakes that
also flip in isolation).

`innerText` collection now dispatches on the box an element generates, not on
its tag: a `div` with `display:table` (from a stylesheet rule or the `style`
attribute) collects like a `<table>`, `display:table-row` like a `<tr>`,
`display:table-cell` like a `<td>`, and `display:inline-table` joins the
surrounding text without boundary breaks while still separating its rows and
cells (<https://html.spec.whatwg.org/multipage/dom.html#the-innertext-idl-attribute>).
The resolved `display` comes from `BaseDocument::resolved_style_value`, so the
getter resolves layout first. Cells that are direct children of the table form
an anonymous row, tab-separated. `getter.html` goes 267/276 to 273/276; the
`html/dom` + `dom/events` + forms batch gains six subtests with nothing lost
(the `form-double-submit-to-different-origin-frame.html` subtest is the known
timing-dependent flake and fails in isolation too).

The two legacy Document collections exist. `document.applets` is the
`[SameObject]` empty `HTMLCollection` the spec requires, and `document.all` is
an `HTMLAllCollection`: every element indexed, ids and the names of the
"all"-named elements (a, button, form, iframe, img, input, select, …) as named
properties, `item`/`namedItem`/callable/tree order as the interface defines,
and `[[IsHTMLDDA]]` set through a new rquickjs wrapper, so `typeof document.all`
is "undefined", it is falsy, and `document.all == null`
(<https://html.spec.whatwg.org/multipage/common-dom-interfaces.html#htmlallcollection>).
`getComputedStyle(el).cssFloat` also reads `float` now instead of the nonexistent
`css-float`. `html/dom/historical.html` goes 10/13 to 13/13 and the
`html/dom` + `dom/events` + forms batch 70,644 to 70,647 with nothing lost.

`document.lastModified` now answers the local time as `MM/DD/YYYY hh:mm:ss`
(<https://html.spec.whatwg.org/multipage/dom.html#dom-document-lastmodified>).
The engine does not record a document's source modification time, so the time
is unavailable and the getter answers the current time, which the spec requires
for that case. `document-lastModified-01.html` goes 0/3 to 3/3; the `html/dom` +
`dom/events` + forms batch keeps every subtest (one harness-budget timeout on
`form-double-submit-requestsubmit.html` that passes 36/36 in isolation twice).

`location.origin` now serializes the document URL's origin, not the settings
object's origin. The two differ for `about:blank`, `about:srcdoc`, and
`javascript:` documents: `window.origin` reports the inherited origin, while
`Location.origin` serializes its own URL, which is opaque for those schemes
(<https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-location-origin>).
`html/dom/self-origin.sub.html` goes 9/11 to 11/11; the `html/dom` +
`dom/events` + forms batch gains all of them with nothing lost (the
`form-double-submit.html` flake flipped, and it fails in isolation too).

`label.focus()` now forwards focus to the label's labeled control when the
label itself is not focusable: the control named by `for` (the first element in
the label's tree with that id, when it is labelable), or the first labelable
descendant in tree order
(<https://html.spec.whatwg.org/multipage/forms.html#labeled-control>). Two
focusability bugs surfaced with it and are fixed: an empty or unparseable
`tabindex` no longer makes an element focusable, and a `display: none` subtree is
not a focusable area
(<https://html.spec.whatwg.org/multipage/interaction.html#focusable-area>).
`forward-focus-to-associated-element.html` goes 2/6 to 6/6; the `html/dom` +
`dom/events` + forms batch and `css/selectors` keep every subtest (a
`form-double-submit-to-different-origin-frame.html` flake flipped, and it
flips in isolation too).

The `selectedcontent` element now reflects the first selected option of its
`select`. The update collects the select's enabled descendant selectedcontent
elements (an `option` or another `selectedcontent` in between disables one, and
so does a second nested `select`), clones the option's children with HTML and
SVG `script`, `frame`, `iframe`, `embed`, and `object` removed, and replaces
each element's children with its own clone set
(<https://html.spec.whatwg.org/multipage/form-elements.html#the-selectedcontent-element>).
It runs after parsing and whenever selectedness changes: `option.selected`,
`select.value`, `select.selectedIndex`, the `selected`/`disabled`/`size`/
`multiple` attributes, option insertion, and a form reset. A `size` change that
does not change selectedness leaves the children alone, and removing `multiple`
always updates. `HTMLSelectedContentElement` is now the element's interface.
`selectedcontent-attribute-change.html` goes 0/7 to 7/7,
`selectedcontent-subtree-mutations.html` 0/2 to 2/2,
`selectedcontent-option-insertion.html` 0/2 to 2/2,
`selectedcontent-nested-select.html` 0/1 to 1/1, and the `html/dom` +
`dom/events` + forms batch gains 15 subtests with nothing lost (the
`form-double-submit-3.html` flake flipped, and it fails in isolation too);
`css/selectors` is unchanged at 4,793. The synchronous-vs-microtask update and
the missing insertion/moving steps are recorded in `docs/limitations.md`.

`document.getElementsByName` now returns only HTML-namespace elements with a
matching `name` attribute; a foreign element such as `<math name>` or
`<svg name>` no longer appears in the list
(<https://html.spec.whatwg.org/multipage/dom.html#dom-document-getelementsbyname>).
Both `document.getElementsByName-namespace.html` and its XHTML twin go 0/1 to
1/1, and the `html/dom` + `dom/events` + forms batch goes 70,671 to 70,673 with
nothing lost.

`document.cookie` is now an attribute of the `Document` interface rather than an
own property of the global document, so every `Document` object answers it. A
cookie-averse document (no browsing context, or a URL that is not HTTP(S))
returns the empty string and ignores writes, as the spec requires
(<https://html.spec.whatwg.org/multipage/dom.html#dom-document-cookie>). A
document made by `createHTMLDocument` now behaves like that instead of reading
`undefined`. `document-cookie.html` goes 4/5 to 5/5 and the `html/dom` +
`dom/events` + forms batch goes 70,673 to 70,674 with nothing lost (the
`form-double-submit-to-different-origin-frame.html` flake flipped, and it flips
in isolation too).

Disabling a `fieldset` now moves focus away from a control inside it. A
focused control that a newly disabled fieldset makes unfocusable is blurred
(firing `blur` and `focusout`) when the attribute changes, instead of waiting
for a rendering update this engine does not run
(<https://html.spec.whatwg.org/multipage/interaction.html#focusable-area>).
`disabled-003.html` goes 0/2 to 2/2 and the `html/dom` + `dom/events` + forms
batch goes 70,674 to 70,676 with nothing lost.

`document.head` now follows the spec's definition: the first `head` element
that is a child of the HTML `html` root element. It no longer falls back to a
document-wide CSS lookup, so a `head` in a foreign namespace inserted before
the real one is ignored and a nested `head` is never returned
(<https://html.spec.whatwg.org/multipage/dom.html#the-head-element-2>).
`document.head-02.html` goes 0/1 to 1/1 and the `html/dom` + `dom/events` +
forms batch keeps its count with nothing lost (two `form-double-submit` flakes
swapped; both flip in isolation, and `form-double-submit.html` passes there).

`HTMLElement.accessKeyLabel` now exists: it answers a modifier-plus-key label
(`Alt+Shift+B`) when the `accesskey` attribute holds exactly one key, and the
empty string for an absent, empty, or multi-key value
(<https://html.spec.whatwg.org/multipage/interaction.html#dom-accesskeylabel>).
The attribute was in the IDL but the generator skips members without an
implementation, so the getter unlocks it. `access-key-label.html` goes 1/2 to
2/2 and the `html/dom` + `dom/events` + forms batch goes 70,676 to 70,677 with
nothing lost.
