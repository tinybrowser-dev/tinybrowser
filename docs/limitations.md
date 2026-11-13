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

## CSS declaration blocks

The property IDL attributes exist on `element.style` and, through it, on the
object `getComputedStyle()` returns. A rule's declaration block
(`document.styleSheets[0].cssRules[0].style`) builds only the methods and the
indexed getter, so `rules[0].style.color` is `undefined` and assigning it adds
a JavaScript property instead of a declaration. `in` and the IDL getter are
`Proxy` traps rather than prototype accessors, so `for...in` over an inline
declaration enumerates only its indices, where a browser also enumerates every
supported property name.

## CSS system fonts

`style.font = "menu"` expands the system-font shorthand into every longhand,
including `font-variant-*: normal`, because that is what the style crate's
`font` parser produces. A browser keeps the `font` shorthand in its
declaration block and reports `""` for those longhands, so
`css/cssom/font-variant-shorthand-serialization.html`'s "font: menu
serialization" fails once the element is asked for a `font-variant-*` value
(`docs/upstream.md`).

## CSS legacy page and alignment properties

`page-break-before`, `page-break-after`, `page-break-inside`, `orphans`,
`widows`, `alignment-baseline`, and the deprecated `outline-color: invert`
keyword are not in the style crate's property tables. A declaration naming one
is a parse error, so `element.style.setProperty("orphans", "2")` leaves the
block empty and the read serializes as `""`; a browser without the property
would do the same. The previous inline-style shim echoed the attribute text and
reported these values, which hid the gap. With the declaration block as the
source of truth, 24 subtests of `css/cssom/serialize-values.html` read `""`
where the reference browser reads the value. The modern
`break-before`/`break-after`/`break-inside` longhands are supported and
`CSS.supports("orphans", "2")` correctly answers false. Accepted engine gap:
teach the style crate those properties or leave them out (`docs/upstream.md`).

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

A worker script URL is checked against its creator's policy before the worker
is created: the `worker` destination resolves `worker-src`, then `child-src`,
`script-src`, and `default-src`, and a blocked URL fires
`securitypolicyviolation` at the creator's document and the plain `error` event
at the `Worker`/`SharedWorker` object. When the fallback lands on `script-src`
the list's own pre-request check runs, so a worker created from a
`'strict-dynamic'` script loads. What is still missing is the other direction:
a violation raised inside a worker is reported at the document that owns the
worker chain. A worker's own requests run under the policy its script response
carried; its script graph (the top-level module and static imports) runs under
the creator's policy, because the outside settings object fetches that graph.
A `blob:` or `data:` worker has no response headers, so it inherits the
creator's container.

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

## Text selection

The `select` event is queued correctly, but two selection behaviours still
differ from a browser. A non-dirty `textarea` whose child text changes keeps
its stored selection instead of resetting it, so
`textfieldselection/selection-start-end-extra.html` fails "Removing child nodes
in non-dirty textarea should make selection{Start,End} 0". And per-element
scroll offsets do not exist yet (Blitz exposes only the viewport scroll), so
`selection.html` fails its `scrollLeft` preservation subtest for an `input`.

## DOM events

`AnimationEvent` and `TransitionEvent` are constructible and every animation
and transition handler attribute exists, but the engine has no animation
engine: CSS `animation` and `transition` never start a timeline, so it never
fires `animationstart`, `animationend`, `animationiteration`,
`transitionrun`, `transitionstart` or `transitionend`. WPT's
`dom/events/webkit-*-event.html` files therefore pass their property and
`dispatchEvent` subtests and time out on every subtest that triggers an
animation. The legacy prefixed-event rewrite (a trusted `webkitAnimationEnd`
is delivered as `animationend` when an unprefixed listener exists) is not
implemented for the same reason; it is only observable on an engine-fired,
trusted event.


A node can be adopted into another document while its own native method is
still on the stack: an event listener that moves the dispatching target.
Adoption retargets the wrapper the page already holds, so the node identity
is interior-mutable (`JsNode.handle` is a `Cell`) and the retarget happens
under the receiver's immutable borrow
(<https://dom.spec.whatwg.org/#concept-node-adopt>). Cloning copies the
already-started flag onto the whole subtree, so a cloned `<script>` does not
run when the clone is connected
(<https://html.spec.whatwg.org/multipage/scripting.html#script-processing-model:cloning-steps>).

A `keepalive` request outlives its environment: the document or worker that
made it hands the request to the network before its realm goes away, and the
completion is discarded because nothing is left to read it. The request never
starts in the same turn: a document that is dropped in the turn its script ran
still sends it from teardown, but a script that queues one and never yields is
the same as any other pending task. `pagehide` reports `persisted: false`
always — this engine keeps no back/forward cache — and an `iframe` removal runs
the child's unload steps even though HTML's removing steps say no unload events
fire; that deliberate divergence is in `docs/upstream.md`.

Still open in `dom/events`: `window.event` inside a shadow tree and for a
listener installed from another global (`event-global-extra.window.html`, two
subtests), and `Event-dispatch-single-activation-behavior.html` exceeds the
harness budget. `Event-dispatch-click.html` is flaky under the in-process
harness: it passes its 33 subtests in some batch runs and hits the per-file
budget in others, on both the old and the new tree.

## Cookie Store

`CookieStore`, `CookieChangeEvent`, and the `cookieStore` attribute are a
shim over the cookie jar. The shim owns the API's validation, default path,
and overloads; the jar owns storage, retrieval order, path and domain
matching, and the storage model's refusals. A write crosses to the jar as a
typed `ScriptCookieWrite`, not as a `Set-Cookie` line, so a `path` holding
U+003B (;) reaches the jar intact. A document that inherits its origin uses
its creator's cookie URL, captured when the document is installed; the
document's own `<base href>` never moves it.

Change events work from a per-realm snapshot of the jar's script-visible
records. Anything that stores a cookie — `cookieStore.set`/`delete`,
`document.cookie`, or a `Set-Cookie` on a response this renderer dialled —
queues "process cookie changes" in every live realm of the renderer, and
each realm diffs its snapshot against the jar. The realm that made the
change processes it before the operation returns, so its own observers see
the change in order. A response that stored a cookie says so through
`DialOutcome::cookies_stored`, because `Set-Cookie` is a forbidden
response-header name and never reaches the renderer in the header list
(<https://fetch.spec.whatwg.org/#forbidden-response-header-name>); a failed
dial processes too, since a hop stores its `Set-Cookie` before the check
that can refuse the exchange. Three gaps remain:

- Another renderer (a second tab), a service worker, or WebDriver can change
  the jar with no message to this one, so no `change` event fires for it.
  The realms here process only changes this renderer made or received. A
  top-level navigation is browser-dialled for the same reason: its response
  carries no `cookies_stored` into the renderer, and the Window it would have
  notified is replaced by the commit.
- A cookie that only expires is evicted lazily by the next jar operation, so
  nothing fires a `deleted` event at the expiry instant
  (<https://cookiestore.spec.whatwg.org/#cookie-store> condition 2). A jar
  write after the expiry does report it.
- The spec queues a global task on the DOM manipulation task source
  (<https://cookiestore.spec.whatwg.org/#process-cookie-changes>). This realm
  has no task source the shim can queue on, so the event is a promise job:
  it runs before the write continuation's timers, not after them.

The spec's `delete(name)` step passes `partitioned` as true, but WPT
`cookieStore_get_delete_basic` sets the cookie with the positional overload
(unpartitioned) and expects this delete to remove it, and Chrome and Firefox
remove it. This implementation passes false.

The `cookiestore` slice passes **40 of 48 files and 247 of 260 subtests**
(from 0 of 48 files). The remaining files fail for infrastructure the engine
does not have:

- `cookieStoreManager_getSubscriptions_*`, `cookieStore_subscribe_arguments`,
  `cookieStore_subscriptions_empty`, `serviceworker_cookieStore_cross_origin`,
  and `serviceworker_cookieStore_subscriptions_reset` need service workers
  (12 subtests), and `idlharness` registers one in its setup, so it needs
  them too. Its IDL validation subtests run and pass now that the harness
  applies wptserve's `/resources/WebIDLParser.js` rewrite, which is why the
  subtest total is larger than the 244 reported before the parser resolved.

A same-origin child whose realm has not started yet answers `cookieStore`
from the creator's realm. An initial `about:blank` inherits the creator's
cookie URL, so both objects read and write the same cookies; the stand-in
differs only in the creation URL used to validate a `url` option, which the
Cookie Store tests in this slice do not pass. Once the child's realm starts,
`contentWindow.cookieStore` is the child's own object.

A frame removed from the tree keeps that Window readable through an already
held `WindowProxy` until the engine adopts the removal at the end of the
turn; the destroy steps remove the tree entry first, so the proxy falls back
to the frame's still-live world instead of throwing a cross-origin error.
The spec keeps the Window alive as long as script references it, which would
need the discarded realm retained past the turn; this engine drops it, so a
read from a later task throws again.

`cookieStore_delete_domain` passes its two subtests and the file reports
`OK`: the harness reporter drops a duplicate remote result. A child frame's
messages can reach the parent twice in this engine, live and again through
the `getmessages` replay; testharness counts one result per remote test, so
the repeat used to drive its pending count negative and time the file out
with every subtest passing.

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

The Fetch metadata group (`Sec-Fetch-Site`, `-Mode`, `-Dest`, `-User`) is
applied per hop in the net client: a hop whose URL is not potentially
trustworthy carries none of them, and a redirect back onto `https` carries them
again. `Sec-Fetch-Storage-Access` is never sent, because this engine has no
Storage Access API to report on, and the HTTP `Link` response header is not
processed, so a `Link: <...>; rel=stylesheet` or `rel=icon` never turns into a
request (the `fetch/metadata` `header-link` files).

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

`securitypolicyviolation` events are queued as tasks, as CSP requires. A
violation reported while the one-shot parser is still waiting on a blocking
script waits for the parser to finish, because that is when the document's
own scripts install their listeners; there is no incremental parser, so a
request the parser issues before an inline script has not reached that
script yet. A request violation fires at the document and an inline
violation at its element while the element stays connected to the document,
per the report-a-violation algorithm. Inline handler content attributes are
checked when the attribute is set, when the element is connected, and by a
walk of the parsed tree once the parser ends, so a parsed handler reports
before it is first invoked; the value is remembered so the invocation check
does not report the same blocked handler a second time. A handler's report
names the script that set the attribute, but its line and column are relative
to that script because Blitz keeps no source positions; a browser reports the
document line, which is why the `targeting-for-inline-handler-*` files that
assert `lineNumber` still fail while their element, blocked URI, and column
assertions pass. Inline `<style>` and `style` attributes still have no CSP
check, and `WebSocket` does not exist, so `blockeduri-ws-wss-scheme.html`
cannot run.

`HTMLScriptElement.text` returns the child text content (the text node
children only) and its setter replaces every child with one text node;
`document.writeln` appends U+000A. A child-list change on a connected script
element runs the children-changed steps, which re-run "prepare the script
element", so an empty script that did not start at insertion executes when
its body arrives later
(<https://html.spec.whatwg.org/multipage/scripting.html#script-processing-model>).
`async`, `blocking`, `fetchPriority`, and `HTMLScriptElement.supports` are
still absent because the generated contract has no lowering for them.

Associated stylesheets expose `cssRules`, per-interface rule objects, and
declaration accessors. The inline `ElementCSSInlineStyle.style` declaration
block is the style engine's parsed `style` attribute: reads serialize through
Stylo, mutations go through the same declaration block the cascade uses, and
every successful mutation or `cssText` write rewrites the content attribute
with the serialized block, as "update style attribute" requires. Still open:
constructable `insertRule` / `deleteRule` throw `NotSupportedError`, a
cross-origin `<link>` is never origin-clean because the fetch path does not
record a CORS approval, and the style engine does not implement a handful of
properties (see `docs/upstream.md`), so an inline declaration of one of them
parses to nothing and reads back empty. Conformance for this surface is WPT
`css/cssom/`.

## Session history

A fragment navigation is a same-document navigation: any navigation whose URL
differs from the target frame's document URL only in its fragment — a
`Location` assignment, a followed `<a href="#...">`, or a form whose action
names a fragment — updates the document URL and appends (or replaces, when the
navigation names the current URL) one joint step with a null classic state,
fires `popstate` synchronously in the owner realm, and delivers `hashchange` in
the document's next turn. The element the fragment names becomes the
document's target element, so `:target` matches it and its ancestors can key
`div:has(:target)` on it. `location.hash` follows
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
