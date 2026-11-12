# Priorities

The current cut is the wasm demo (vault brief); these tiers rank
web-platform work behind it. Chrome extensions are a separate, later
track with their own corpus.

P0 is not flat. The tiers group it by measured reach; Next effort
ranks the work by reach and the current WPT gap together. Reach is the
share of page loads that use the API in Chrome UseCounter (2026-10-20
snapshot, all platforms, official channels).
WPT is the test-file pass rate from CI run 39980734258 (2026-10-23,
testharness + crashtest + reftest + test262, `--exclude=worker`).
UseCounter counts pages that touch an API, including feature probes,
and has no counter for the oldest DOM calls (`querySelector`,
`addEventListener`), so the HTML/DOM numbers are lower bounds.

Sources:
<https://chromestatus.com/data/featurepopularity>,
<https://chromestatus.com/data/csspopularity>,
<https://webstatus.dev> for features without a UseCounter,
<https://almanac.httparchive.org/en/2025/capabilities> for
`document.write`.

## P0

### Tier 0 — on every page

- **html** — 81.0% of pages have a form (`FormElement`), 55.2% a
  `<slot>`, 47.1% a `<canvas>`, 41.5% call `innerText`, 38.2% use
  `<template>`, 9.9% a `<dialog>`, 8.0% `popover`. WPT 46.7%;
  `html/semantics` 32.2%, `html/webappapis` 20.4%, `html/dom` 24.5%.
- **css** — `display` 93.2%, `width` 92.6%, `height` 92.1%,
  `position` 91.3%, `margin` 91.0%, `padding` 90.8%. WPT 44.1%
  across 34.5k files; flexbox 73.1%, grid 38.7%.
- **selectors** — `:not()` 84.2%, `:focus` 81.0%, `:focus-visible`
  59.5%, `:has()` 55.6%, `:where()` 38.1%, `:is()` 36.7%. WPT
  `css/selectors` 51.0%.
- **dom** — `getBoundingClientRect()` 71.0%,
  `IntersectionObserver` 55.4%, `ResizeObserver` 46.4%,
  `getRootNode()` 43.9%, `isConnected` 40.9%, `attachShadow()` 22.2%,
  `customElements.define()` 19.7%. WPT 59.1%; `dom/nodes` 80.5%,
  `dom/events` 28.0%, `dom/ranges` 50.0%, `dom/traversal` 97.9%.

### Tier 1 — high reach

- **fetch** — `fetch()` 72.7%, `sendBeacon()` 52.6%, `keepalive`
  39.0%, body streams 30.9%, response construction with a stream
  24.8%. WPT 34.8%; `fetch/api` 71.8%.
- **cookies** — `document.cookie` read 78.7%, write 68.3%,
  `SameSite=None` 38.6%, no `SameSite` 31.7%, partitioned 30.7%,
  `CookieStore` 11.9%. WPT 32.6%; `cookiestore` 39/48 files and
  230/244 subtests. What remains is service workers and a discarded
  frame's `contentWindow` global.
- **storage** — `localStorage`/`sessionStorage` read 80.0%, write
  75.8%. WPT `webstorage` 81.5%.
- **csp** — any CSP 68.9%, HTTP-delivered 50.0%, `unsafe-eval` 54.2%,
  `strict-dynamic` 43.1%. WPT 55.3% of subtests (703/884 files).
- **referrer-policy** — `SetReferrerPolicy` 66.6%, response header
  32.3%, meta 27.8%, iframe attribute 6.2%. WPT 16.5%.

### Tier 2 — common

- **xhr** — asynchronous 73.9%, `responseXML` 5.7%, synchronous 3.5%.
  WPT 54.7%.
- **messaging** — `MessageChannel` 58.7%, `postMessage` 52.3%,
  `MessagePort.postMessage` 48.4%, `BroadcastChannel` 17.5%. WPT
  82.1%; `broadcastchannel` 33.3%, `message-channels` 54.5%.
- **encoding** — `TextEncoder` 55.9%, `TextDecoder` 42.6%. WPT 68.0%
  for the modern suite (79.3% with legacy encodings).
- **url** — `createObjectURL(blob)` 20.9%, `history.replaceState`
  40.9%, `URL.canParse()` 6.5%, `URLPattern` 2.2%. WPT `url` 51.0%.
- **js** — strict mode 81.3%, modules 41.5%, `??` 50.1%, `replaceAll`
  34.1%, logical assignment 32.2%, `structuredClone` 22.0%, weak
  references 24.7%. WPT test262 80.4%.
- **domparsing** — `<template>` 38.2%, `document.write` 12–13% of
  pages (Web Almanac 2025). WPT `domparsing` 63.0%, tentative 2.8%.

### Tier 3 — cross-cutting, no separate counter

- **webidl** 22.2%, **infrastructure** 35.9%, **mimesniff** 0/6,
  **encoding-detection** 33.3%. Every tier above depends on these;
  fix them through the feature that exposes the bug, not as
  standalone projects.

## Next effort

Ordered by reach × current gap:

1. **css layout + cssom** — standing workstream, not one push:
   `display` 93.2%, `matchMedia()` 66.2%, `getBoundingClientRect()`
   71.0% against 44.1% for css; `css/cssom-view` 17.1%, `css/cssom`
   29.5%.
2. **cookies** — read on 78.7% of pages against 32.6% WPT. SameSite and
   partitioned cookies are in; `CookieStore` (11.9%) is 39/48 files. The
   rest needs service workers and a discarded frame's `contentWindow`
   global.
3. **csp** — 68.9% reach against 26.6%. A response-header path with a
   small surface.
4. **fetch** — 72.7% reach against 34.8%. The Body/Headers/stream
   rewrite took `fetch/api` from 26.2% to 71.8%; what remains is
   `sendBeacon()` (52.6% of pages, 13/16 files), `fetch/metadata` 1/87,
   `fetch/h1-parsing` 1/20, and `fetch/http-cache` 12/15.
5. **html forms and semantics** — 81.0% of pages have a form; html is
   46.7%, but `html/semantics` is 32.2% and `html/webappapis` 20.4%.
   Controls, submission, and script/timer plumbing.
6. **selectors** — 84.2% reach against `css/selectors` 51.0%.
7. **dom parsing and events** — `dom/events` 28.0% behind universal DOM
   reach, with `dom/traversal` now at 97.9%; parser work stays in
   `domparsing`.

`encoding` (68–79%) and `messaging` (82.1%) pass enough that they need
targeted fixes, not a push.

## P1

navigation-api, custom-elements, shadow-dom, editing, selection, clipboard, uievents, focus, shared/service workers, IndexedDB, streams, compression, FileAPI, trusted-types, locks, import-maps, chrome-extensions.

Dedicated workers are implemented: classic and module scripts, nested
workers, `MessagePort` transfer and delivery, the structured-clone codec,
worker `fetch`/XHR (including synchronous XHR), `import.meta`, `blob:` and
`data:` scripts, timers, and error propagation. SharedWorker runs in the
same engine with one realm per `(origin, URL, name)`, per-connection
ports, and the same classic/module and error semantics. The remaining
worker work is service workers and the remaining IndexedDB surface
(indexes, cursors, and upgrade blocking).

P1 is not "rare". IndexedDB is read on 24.7% of pages (write 23.2%),
compression streams on 25.1%, Cache Storage read on 19.6%, a worker
starts on 21.2%, a service worker controls 17.8%. They stay P1 because
pages can feature-detect and degrade, not because nobody uses them.
Promote IndexedDB and compression first if a P1 item is pulled forward.

### Chrome extensions (full MV3)

A Chrome vendor surface, not a web spec, so WPT cannot score it. It
needs its own corpus: a small set of real unpacked MV3 extensions plus
contract tests for each API. Full MV3, not a subset — a partial
`chrome.*` surface makes extensions fail in ways the corpus would hide.

Surface: the `chrome-extension://<id>/` origin, storage partitioning and
extension CSP; unpacked manifest parse/validate, ID derivation and the
permission model; content scripts (match patterns, `run_at`,
`all_frames`, isolated worlds) and CSS injection; `chrome.runtime`
(messaging, `getURL`), `chrome.storage.*`, `chrome.scripting`,
`chrome.tabs`, `chrome.alarms`, `chrome.i18n`, and
`chrome.declarativeNetRequest`; `web_accessible_resources`; extension
popup and options pages; the MV3 background service worker.

Dependency: the MV3 background is a service worker, so `workers` and
`service-workers` land first. Feature-gate the whole extension surface:
the default binary keeps its size budget, and an extension-enabled build
carries the `chrome.*` glue and its corpus harness.

Out: Chrome Web Store install/auth, native messaging, MV2.

## P2

wasm (the wasm demo is the current cut; this tier is about wasm as a
page technology), webmcp, ai.

## Skip

hardware, human window/modal, realtime media/GPU, OS/chrome UI.
