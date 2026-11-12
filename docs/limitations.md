# Limitations

Honest current gaps. Upstream engine gaps live in docs/upstream.md;
out-of-scope API families live in docs/priorities.md.

## JS engine

QuickJS bytecode interpreter, no JIT. DOM glue and agent scripts are
fine; JS-heavy pages run slower than V8 browsers. No benchmark yet.

## Rendering

One-shot PNG only: CDP Page.captureScreenshot is PNG-only
(crates/cdp/src/dispatch.rs:882), no continuous paint loop, no GPU,
no vsync, no video. requestAnimationFrame is a 16ms timer with
documented deviations: shared timer table, Date.now() timestamps,
no per-frame timestamp sharing (timers.js:63).

## Intl

NumberFormat and DateTimeFormat only. No Collator, PluralRules,
RelativeTimeFormat, ListFormat, or Segmenter. Real CLDR data for 7
locales (en-US es-ES de-DE ja-JP fr-FR zh-CN ko-KR,
tools/intl/generate-data); every other locale silently falls back.

## Strings

Lone surrogates round-trip through character data, titles, and
attributes. The layout tree still stores UTF-8 (docs/upstream.md).

## CSS colors

Canvas styles and `<input type=color>` use the CSS Color 4 parser in
`crates/renderer/src/css/color.rs`. It implements the predefined `color()`
spaces, but clips out-of-gamut colors per channel instead of running OKLCH
local-MINDE gamut mapping. Wide-gamut colors far outside sRGB can differ from
a browser that implements the mapping algorithm.

## Missing surfaces

Dedicated workers run on their own OS thread with their own `QuickJS`
runtime: construction from HTTP(S), `blob:`, and `data:` URLs; classic and
module scripts; nested workers; `postMessage` with the structured-clone
codec (ports, transferables, `ImageData`, `ImageBitmap`, `OffscreenCanvas`,
files, and `SharedArrayBuffer`); timers; `importScripts`; `import.meta`;
asynchronous `fetch`/XHR and synchronous XHR (including `blob:` URLs);
`BroadcastChannel`; `close()`, `terminate()`, and runtime error propagation
to the `Worker` object. A page that spins on `Atomics` does not block its
worker, and `terminate()` interrupts a worker stuck in synchronous script.
`docs/worker-threads.md` records the thread model, the engine-owned timer
mirror, and the shared-memory transport.

`SharedWorker` still runs in the page engine. One realm per
`(creator origin, script URL, name, sameSiteCookies)` receives a `connect`
event per constructor call, with the options (`type`, `credentials`,
`extendedLifetime`) required to match. `sameSiteCookies: "all"` is refused
with an `InvalidStateError` unless the creator is in a first-party site
context; the absent option defaults to `all` there and `none` otherwise, and
`none` filters every request and `Set-Cookie` line the worker's script,
module graph, and `fetch`/XHR make down to `SameSite=None`. The context is
first-party only when the creator's site matches every ancestor's, so a
same-site frame inside a cross-site one stays third-party. Shared-worker
runtime errors fire at the worker global only, and a script that fails to
load or parse fires a plain `error` event at every connected `SharedWorker`
object. A shared worker is shared inside one engine, not across renderer
processes, and `extendedLifetime` does not outlive the tab; moving the realm
to its own thread is milestone 6 in `docs/worker-threads.md`. Service
workers remain unimplemented. `indexedDB` exists on windows and workers and
implements the factory, database open and upgrade, one versionchange
transaction per upgrade, object-store create/delete, the record operations
`put`, `add`, `get`, `getKey`, `getAll`, `getAllKeys`, `getAllRecords`,
`count`, `delete`, and `clear` over `IDBKeyRange` bounds, and the cursor
surface `openCursor`/`openKeyCursor`, `continue`, `advance`,
`continuePrimaryKey`, `update`, and `delete`. Indexes are implemented:
`createIndex`, `index`, `deleteIndex`, `indexNames`, unique and multiEntry
constraints, and every `IDBIndex` query. Inline and generated keys, binary,
array, and `SharedArrayBuffer` keys, `databases()`, `deleteDatabase()`, and
object-store and index renames are implemented too. Records live in the profile
as opaque encoded rows behind `StorageKind::IndexedDb`, so every renderer of a
profile sees one store and the browser persists it like `localStorage`.
Upgrade blocking between open connections, transaction scheduling across
overlapping connections, a key generator shared across connections, and the
`AbortError` a listener's uncaught exception must produce are still absent;
metadata deleted during an aborted upgrade is only partly restored. The area
enforces the same 5 MiB per-origin cap as `localStorage` instead of a separate
IndexedDB quota. A `ReadableStream` is
transferable: the transfer list locks the sender and moves the stream over an
internal `MessageChannel`, one chunk per pull, so close, error, and cancel all
cross with it. `crossOriginIsolated` is exposed on windows and workers and is computed from a
secure origin, a `same-origin` `Cross-Origin-Opener-Policy` at the top level, a
compatible `Cross-Origin-Embedder-Policy` (`require-corp` or
`credentialless`) on every document in the chain, and the embedder's (or
worker creator's) capability. Serializing a `SharedArrayBuffer` throws a
`DataCloneError` unless the sender is cross-origin isolated, as the HTML
structured-clone algorithm requires, so the `Growable SharedArrayBuffer` WPT
case passes through its non-isolated branch.

`TreeWalker` and `NodeIterator` implement every traversal method, the
whatToShow bitmask, the filter callback with its re-entrancy guard, and
`createTreeWalker`/`createNodeIterator`. The NodeIterator pre-remove steps
are absent: removing a node does not move a live iterator's reference, so
`NodeIterator-removal.html` fails. The cross-realm traversal files also
depend on named-property access to an iframe and on `srcdoc`, not on the
traversal interfaces.

A `QuickJS` realm cannot start while another realm's script runs, so a
same-origin `iframe` inserted by a script has its document but not its realm
until the script returns. Its `WindowProxy` answers the inserting realm's
globals (`DOMException`, the ECMAScript intrinsics, and the interfaces whose
prototype carries a `Symbol.toStringTag`) and a `Location` bound to the child
frame, so `contentWindow.DOMException` and `contentWindow.location = url`
behave as the spec requires in that same task. It also hands out a `Navigator`
of its own so per-document state such as the beacon quota stays with the
child. Those objects still execute in the inserting realm, though: a relative
URL handed to the stand-in `Navigator.sendBeacon` resolves against the
inserting document's base URL and queues with its client settings, not the
child's, until the child realm starts. The constructors also come from the
inserting realm rather than the child's own realm, which is observably
different only before the child realm exists.
`WebAssembly.Memory` exists, including shared memories whose buffer is a real
`SharedArrayBuffer`, but the module compiler does not, so `WebAssembly.Module`,
`Instance`, `compile`, and `validate` are absent. A non-shared memory's buffer
is a fixed-length `ArrayBuffer`, matching the current JS API draft and Chrome:
`grow()` detaches it and installs a new one, and `toResizableBuffer()` and
`toFixedLengthBuffer()` are not provided. A shared memory's buffer is a
growable `SharedArrayBuffer` grown in place; Chrome 152 hands out a
fixed-length one and replaces it on growth, but JavaScript cannot alias a new
`SharedArrayBuffer` to an existing block, and the threads proposal requires
every agent to see the additional memory (`docs/upstream.md`). A relative
`eval(import())` resolves against the script that called `eval`, so it works
inside an imported HTTP module even when the worker's own URL is a `blob:` or
`data:` URL; the QuickJS fork walks out of the eval frames to find that script
(`docs/upstream.md`). The cookie
jar implements the `Partitioned` attribute (CHIPS) by storing a cookie under
the site of its top-level document and matching it only inside that
partition; `document.cookie`, every dial, and a worker's script and module
graph carry the key, and a `SharedWorker` is keyed by its creator's storage
key (origin plus top-level site). Chrome's newer cross-site ancestor bit is
not modelled, so a same-site frame that has a cross-site ancestor still sees
the top-level partition's cookie
(`cookies/partitioned-cookies/partitioned-cookies-a-b-a-embed.tentative.https.html`
fails only that assertion).
The in-process WPT harness serves files directly, applies wptserve's
`header(...)` and `sub` pipes, and stores a mounted document's `.headers`
`Set-Cookie` in the jar; it does not implement `trickle` timing. It has no
tab carrier, so the renderer hosts a popup in the same engine
(`BrowsingContextHost::opens_window_as_tab` answers false): `window.open`
creates an auxiliary context whose initial `about:blank` is replaced by a
queued navigation, and the opener's pre-realm `WindowProxy` writes
(`popup.onload`, a property assignment) are replayed into the popup's first
real document. That is what lets the `extendedLifetime` cases, the
partitioned-cookie files, and the module CSP/referrer files run. The harness
also answers `/resources/testdriver-vendor.js` with a cookie-backed
implementation of `get_all_cookies`/`delete_all_cookies`; every cookie those
tests set is a non-`HttpOnly`, path-`/` cookie, so the page's own
`document.cookie` is the set they ask about. Its standalone session history
keeps the joint steps itself: a child navigation adds a step, and a main-frame
or child `history.back()` moves every frame whose entry changes at the target
step, so `workers/semantics/navigation/001.html` observes the child document
being restored.

Canvas `getContext` returns a working 2D context (paths, text, gradients,
patterns, filters, layers); the `bitmaprenderer` context exists but
transfers nothing until `ImageBitmap` decoding lands; WebGL returns null.
Sync XHR is rejected on a `Window`, customized built-in elements are
rejected. Shadow-scoped styles and composed-event retargeting are upstream
gaps (docs/upstream.md).

`Request` exposes `blob()` only of the body-consumption methods; `text`,
`json`, `arrayBuffer`, and `bytes` are missing. A `text/plain` document
is still parsed as HTML, so markup served as plain text renders instead
of showing as text (WPT `mimesniff/sniffing/`).

Cross-origin `fetch`/XHR run the CORS protocol in the browser process
(`crates/net/src/cors.rs`): the CORS check, preflight, credentials mode,
response-header filtering, opaque `no-cors`, redirect-tainted origins,
and redirect mode. A `no-cors` request is held to CORS-safelisted methods
and headers at the network boundary, but the JS-visible `Request.headers`
and `Headers.set` do not yet apply the `request-no-cors` guard, so a page
sees headers in its own object that the wire drops. Still open: the
CORS-preflight cache (every non-simple request preflights again),
`requestInit.referrerPolicy`/`referrer` on a request, `keepalive`, request
origins for sandboxed and `data:` frames (sent as the URL origin, not
`null`), cookies on a credentialed cross-origin redirect, and deriving the
request origin from a per-frame browser record rather than the
site-authorized initiator (a renderer may name any origin within its own
site, which shares its process). A header value holding a control byte
other than TAB (Fetch permits e.g. `0x01`) is refused by the HTTP layer,
so WPT `fetch/api/cors/cors-preflight-not-cors-safelisted.any.js` still
fails on the `accept-language`/`content-language` `0x01` rows. `fetch`
enforces request `integrity` at the dial for the native browser and the
in-process host alike (`dial::integrity_ok`), re-checks a cached body
before answering, and treats an opaque or undecodable body as a failure.
A same-origin `cors` POST that survives a 307/308 redirect sends `Origin`,
which `fetch/api/redirect/redirect-origin.any.js` expects to be absent;
step 3 of the append algorithm does not exempt that case, and Chrome and
Firefox fail the same two subtests. WPT `fetch/api/cors/` covers the
surface.

## Cookie Store

`CookieStore`, `CookieChangeEvent`, and the `cookieStore` attribute are a
shim over the cookie jar. The shim owns the API's validation, default path,
and overloads; the jar owns storage, retrieval order, path and domain
matching, and the storage model's refusals. A write crosses to the jar as a
typed `ScriptCookieWrite`, not as a `Set-Cookie` line, so a `path` holding
U+003B (;) reaches the jar intact. A document that inherits its origin uses
its creator's cookie URL, captured when the document is installed; the
document's own `<base href>` never moves it.

Change events compare the jar's script-visible records before and after a
script-initiated write (`cookieStore.set`/`delete` and `document.cookie`).
Two gaps remain:

- A `Set-Cookie` response header, another tab, or WebDriver inserts or
  evicts a cookie without a script write, so no `change` event is fired for
  it. The next script write reports only its own diff.
- The spec queues a global task on the DOM manipulation task source
  (<https://cookiestore.spec.whatwg.org/#process-cookie-changes>). This realm
  has no task source the shim can queue on, so the event is a promise job:
  it runs before the write continuation's timers, not after them.

The spec's `delete(name)` step passes `partitioned` as true, but WPT
`cookieStore_get_delete_basic` sets the cookie with the positional overload
(unpartitioned) and expects this delete to remove it, and Chrome and Firefox
remove it. This implementation passes false.

The `cookiestore` slice passes **37 of 48 files and 221 of 244 subtests**
(from 0 of 48 files). The remaining files fail for infrastructure the engine
does not have:

- `cookieStoreManager_getSubscriptions_*`, `cookieStore_subscribe_arguments`,
  `cookieStore_subscriptions_empty`, `serviceworker_cookieStore_cross_origin`,
  and `serviceworker_cookieStore_subscriptions_reset` need service workers
  (12 subtests); `idlharness` needs the WebIDL2 script.
- `cookieStore_get_set_across_frames` needs a script-created iframe's realm
  before the creating script runs, so `iframe.contentWindow.cookieStore` is
  undefined (2 subtests).
- `cookieStore_in_detached_frame` reads `contentWindow` of a removed iframe,
  which the engine reports as a cross-origin access (1 subtest).
- `cookieStore_delete_domain` passes both subtests but
  `fetch_tests_from_window` never reports the child's completion, so the
  file times out.
- `change_eventhandler_for_http_cookie_and_set_cookie_headers` needs
  HTTP-initiated change events (7 subtests).

WPT's `get_named_cookie` runs from a harness route,
`/resources/testdriver-cookie?name=`, that answers from the jar's live
records. It is a test-harness surface, not engine code: the vendor script
fetches it, and the origin filters by the cookie's domain against the
request host. The jar publishes a snapshot after every operation that can
store a cookie, including a page fetch whose response carried
`Set-Cookie`.

## HTTP cache

`crates/dial/src/cache.rs` holds the one response cache, shared by the
native browser and the in-process WPT harness, so a verdict measures the
same freshness, `Vary`, revalidation, and invalidation rules the browser
runs. Entries are keyed by method, URL, credentials mode, initiator
origin, and the storage partition, and each base key keeps one stored
response per `Vary` representation.

Still open: the partition is the browser's renderer assignment, not the
top-level site Fetch's "HTTP cache partitions" defines, so a same-site
frame inside a cross-site top-level page reuses the opener's entries
(`fetch/http-cache/split-cache.html`, last subtest). There is no HTTP
authentication cache, so `basic-auth-cache-test.html` never finishes.
`post-patch.any.html` cannot pass as written: its second request omits
`response_body`, so the harness expects the default UUID while the
correct `Content-Location` reuse answers `"abc"`. `stale-while-revalidate`
background revalidation is not implemented; a stored
`206` is completed with a synthetic `Range: bytes=<end>-` request rather
than being assembled from several stored ranges.

`navigator.sendBeacon()` builds the same `POST` dial as `fetch` with the
keepalive flag: `BodyInit` extraction, the scheme check, the CORS mode
chosen from the content type, and the in-flight quota. The quota is
accounted per document in the shim, so a `fetch(..., {keepalive: true})`
in the same document does not consume it; the browser's process-wide
reservation is still the hard limit and a rejected dial surfaces as a
dropped beacon rather than a return value. The `BodyInit` union is not
lowered by the JS binding generator yet, so `sendBeacon` is installed on
the prototype by hand with the operation's shape.

CSP gates string compilation in the engine (`can_compile_strings`), so
direct `eval` keeps its scope and `Function` and indirect `eval` are
covered by the same check. Still open: there is no incremental HTML
parser, so scripts a page writes with `document.write` during parsing
never run (this blocks the strict-dynamic and about-blank iframe cases
that depend on it); `report-uri`/`report-to` endpoint delivery is
partial; `base-uri` is checked when the parser reaches a `<base>`, when
a base is inserted, and when its `href` changes, but during parsing the
live document base URL can still see a `<base>` the parser has not
reached yet; the CSP list does not carry the self-origin, so `'self'` in
an inherited policy resolves against the document origin; the tentative
`'eval-sha256-…'` source expression is not implemented (it is not in
CSP3, and a policy naming it still needs `'unsafe-eval'`); and
`content-security-policy/sandbox/sandbox-empty.sub.html` now delivers a
sandboxed child's `postMessage` to the parent's handler, which the
converted WebKit test expected to lose a parse-order race.

Associated stylesheets expose `cssRules`, per-interface rule objects, and
declaration accessors. Still open: constructable `insertRule` /
`deleteRule` throw `NotSupportedError`, and a cross-origin `<link>` is
never origin-clean because the fetch path does not record a CORS
approval. Conformance for this surface is WPT `css/cssom/`.

## Session history

A fragment navigation is a same-document navigation: a `Location` assignment
that changes only the fragment updates the document URL and appends (or
replaces, when the assignment names the current URL) one joint step with a
null classic state, fires `popstate` synchronously in the owner realm, and
delivers `hashchange` in the document's next turn. `location.hash` follows
its own setter steps, not the URL interface's: it strips one leading `#`,
sets the fragment to the empty string, and parses the rest into it, so
clearing a fragment leaves a bare `#` while writing an unchanged fragment
bails out before navigating
(`https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-location-hash`).
Two gaps remain: nothing scrolls to the fragment target, and
`location.assign`, `replace`, and `reload` are still unimplemented.

A session-history entry stores its URL, title, classic state, and the
document state's navigable target name. It does not store the document
state's origin, so the traversal rule that clears the target name when a
re-fetch lands on another origin
(`https://html.spec.whatwg.org/multipage/browsing-the-web.html#apply-the-history-step`,
"targetEntry's document's origin is not same origin with oldOrigin") is not
applied. Until it is, a stored entry that redirects across origins restores
the old name and another tab in the group can still select it by that name.
`history.state` is likewise kept instead of cleared. The browser knows the
origin of an ordinary HTTP commit, so the redirect case is fixable by storing
an origin per entry; the opaque cases (sandboxed, `data:`, inherited
`about:blank`) need the renderer's computed origin on the commit.

`window.name` is stored as `ContextName`, which cannot be a keyword, so
assigning `_self` or `_blank` is recorded as no name and that string does not
survive a commit or a traversal. The spec keeps any string in the document
state and treats keywords specially only when a target is resolved.

A carrier with no browser tab behind it keeps the joint steps in the engine:
each used step records the local entry every live frame presents, a child
navigation adds one, and a same-document push adds one. It records no
main-frame entry, so a step that would move the main frame is refused instead
of moving only part of the tab; `history.back()` from the main frame still
moves a child whose entry differs at the target step, which is what
`workers/semantics/navigation/001.html` needs. A browser-hosted engine's tab
actor owns the full list and records every main-frame entry.

A popup's renderer records "auxiliary with a live opener" when it mounts, and
nothing tells it when that opener closes, so a later cross-origin navigation
it drives itself keeps the target name. The browser's copy of the rule
(`Tab::keeps_target_name_at`) is checked live, but a renderer-driven commit
never consults it. The name survives the navigation and the group can still
select the tab by it until a browser-driven commit clears it.

A same-origin popup is a browser tab, and its `WindowProxy` in the opener is
the restricted cross-tab proxy: `opener`, `postMessage`, `location`, and
`close` work, but the full `Window` surface (`isSecureContext`, `document`,
and the rest) is absent. The spec leaves same-origin proxy allocation to the
UA but a conforming engine exposes the full Window. The popup's own document
is correct; tests that need one of its properties attach to its target
instead (`tests/browser/secure_context.rs`).
