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
workers, `IndexedDB`, and `ReadableStream` transfer remain unimplemented;
growable `SharedArrayBuffer` needs `WebAssembly.Memory` for the WPT fixture,
though the allocator hooks themselves are in place, and the QuickJS gaps
behind the remaining clone failures are recorded in `docs/upstream.md`. A
relative `eval(import())` in a blob- or data-URL module worker cannot resolve
because those URLs have no hierarchical path in the URL standard; Chrome
rejects it the same way, and `docs/upstream.md` records the probe. The cookie jar does not implement the `Partitioned`
attribute (CHIPS), so a partitioned cookie is stored as an ordinary one;
`shared-worker-partitioned-cookies.tentative.https.html` now runs through its
popup and fails only the assertion that reads a partitioned cookie.
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
`document.cookie` is the set they ask about. Its standalone session
history also does not apply a main-frame `history.back()` to a child frame's
joint entry, so `workers/semantics/navigation/001.html` cannot observe the
child document being restored; that is a harness gap, not a worker lifetime
rule.

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
also ignores request `integrity` metadata
(SRI is enforced for scripts only, `crates/renderer/src/csp.rs`); the old
cross-origin block hid both that and the sandbox-origin gap, so
`fetch/api/basic/integrity.sub.any.html` and
`xhr/access-control-sandboxed-iframe-*.htm` now fail honestly. A
same-origin `cors` POST that survives a 307/308 redirect sends `Origin`,
which `fetch/api/redirect/redirect-origin.any.js` expects to be absent;
step 3 of the append algorithm does not exempt that case, and Chrome and
Firefox fail the same two subtests. WPT `fetch/api/cors/` covers the
surface.

CSP `eval`/`Function` gating shadows the globals in JS because the
engine has no compilation hook; direct `eval` therefore evaluates
indirectly (global scope) even when allowed. Blocking and report
delivery (`report-uri`/`report-to`), `frame-ancestors`, and per-element
violation source positions are not implemented yet.

Associated stylesheets expose `cssRules`, per-interface rule objects, and
declaration accessors. Still open: constructable `insertRule` /
`deleteRule` throw `NotSupportedError`, and a cross-origin `<link>` is
never origin-clean because the fetch path does not record a CORS
approval. Conformance for this surface is WPT `css/cssom/`.

## Session history

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
