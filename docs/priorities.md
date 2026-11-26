# Priorities

Ranked tiers for ongoing web-platform work. Open gaps and feature work live in [GitHub issues](https://github.com/tinybrowser-dev/tinybrowser/issues).

## P0

Ordered by current priority:

1. **CSS layout and CSSOM** — `css/cssom`, `css/cssom-view`, grid, flexbox, tables, display, overflow
2. **HTML forms and semantics** — controls, validation, submission, script and timer plumbing
3. **Selectors** — `:has()`, `:is()`, `:not()`, `:where()`, pseudo-classes and invalidation
4. **DOM events and parsing** — `dom/events`, `domparsing`, `dom/ranges`
5. **Fetch and CSP tail** — `fetch/metadata`, `fetch/h1-parsing`, remaining CSP directives
6. **Cross-cutting** — `webidl`, `infrastructure`, `mimesniff`, `encoding-detection` (fix through the feature that exposes the bug)

## P1

`navigation-api`, `custom-elements`, `shadow-dom`, `editing`, `selection`, `clipboard`, `uievents`, `focus`, `service-workers`, remaining `IndexedDB` (indexes, cursors, upgrade blocking), `streams`, `compression`, `FileAPI`, `trusted-types`, `locks`, `import-maps`, Chrome extensions (MV3, feature-gated, after `service-workers`).

## P2

Page-facing WebAssembly, WebMCP, AI APIs.

## Skip

Hardware APIs, human window/modal chrome, realtime media/GPU, OS/browser UI.
