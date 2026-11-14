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

## CSSOM

The CSS rule interfaces are exposed for the rule kinds the style engine reports,
but several rules never reach `cssRules`: `@page`, `@counter-style`,
`@container`, and `@scope` are absent from an author sheet's list, so
`CSSPageRule`, `CSSCounterStyleRule`, `CSSContainerRule`, and `CSSScopeRule`
objects are never produced and `CSSRule-type.html` keeps four subtests red.
That list comes from Blitz's CSSOM snapshot (`cssom_rule_info`), not from this
engine.

A media rule's `media` and an import rule's `media` are live `MediaList`
objects backed by the rule's query list, and `StyleSheet.media` is a live list
too; `mediaText`, `appendMedium`, and `deleteMedium` write through the engine.
`CSSImportRule.styleSheet` answers null because imported sheets live in their
own address space that the snapshot does not expose.
`CSSFontFeatureValuesRule` has no `CSSFontFeatureValuesMap`, and a constructable
`CSSStyleSheet` has no parsed rule list, so its `insertRule` and `deleteRule`
throw `NotSupportedError` and `adoptedStyleSheets` does not feed the cascade.

`getComputedStyle` returns an empty declaration when an element is detached, is
outside the flat tree, or lives in a document whose browsing context is not
rendered. Blitz has no slot assignment, so the flat-tree test approximates
"assigned to a slot" with "the host's shadow tree contains a `<slot>`": a light
child of a shadow host with no slot element is treated as unslotted, while any
light child of a host that has one is treated as slotted even when its `slot`
name or `slot` attribute would not actually match.

A `getComputedStyle` pseudo-element argument is parsed, but only the eager
pseudo-elements are cascaded. `::marker`, `::backdrop`, `::placeholder`,
`::file-selector-button`, and the functional pseudo-elements (`::highlight()`,
`::picker()`, `::view-transition-*()`) return a declaration without their
rules applied, because the style engine stores only the eager pseudo styles.
The same gap shows up as a missing `::file-selector-button` style on a file
input. A nonexistent `::before`/`::after` of a flex or grid container also
reports the initial `display: inline` instead of the blockified value the real
cascade would compute, because stylo drops a pseudo style that is definitely
not generated.

CSS rule attributes live on the interface prototypes, but
`CSSFontFeatureValuesRule.fontFamily` is still an own data property: the style
engine stores a font-feature-values rule's family list behind an immutable
`Arc`, so changing it needs the containing rule list rewritten. The property is
therefore not installed on the prototype; a page that assigns it creates an own
property on that one rule object, and the sheet keeps the original family.
`CSSImportRule.styleSheet` remains null (recorded below), and a media rule's
`media` is still a string rather than a live `MediaList`.

## CSS content-visibility

`content-visibility` is not in the style crate's property tables: the Servo
backend generates it only for the Gecko engine (`engine = "gecko"` in
`longhands.toml`), so a declaration is a parse error. As a consequence:

- `CSS.supports("content-visibility", "hidden")` is false and
  `getComputedStyle(el).contentVisibility` is undefined.
- Blitz lays out and paints content that `content-visibility: hidden` or a
  skipped `content-visibility: auto` subtree would skip; `contain-intrinsic-size`
  has no effect either.
- `Element.checkVisibility()` cannot run its unconditional "an ancestor has
  `content-visibility: hidden`" step, or honour the `contentVisibilityAuto`
  option, so an element the reference browser hides reports visible. Five of
  `css/cssom-view/checkVisibility.html`'s fifteen subtests fail for this
  reason. The other ten pass: the associated-box check (including
  `display: none` on the element and on ancestors, and `display: contents`),
  `visibility` on the element, and `opacity` on the element and on ancestors.

`checkVisibility`'s ancestor walk is shadow-including, not the flat tree. Blitz
has no slot assignment, so a slotted element's slot is not in its chain, and
shadow-tree content has no computed styles (the shadow-scoped cascade is
absent), so a `display: none` ancestor inside a shadow tree is not detected. A
`display: none` or `opacity: 0` host is detected, because a shadow root's node
records its host. An element in a document without a browsing context
(`DOMParser`, `createHTMLDocument`) reports false, and so does an element in
unattached content such as a template's content fragment: the renderer requires
the element's document to be the active document of its realm, and a node
outside the document tree has no box.

Teaching the Servo style backend the property belongs upstream
(`docs/upstream.md`).

## Scrolling

`document.scrollingElement`, the element and viewport scroll positions, and
`scroll`/`scrollTo`/`scrollBy`/`scrollIntoView` route through Blitz's scroll
engine. The engine's model has these gaps:

- The viewport's scrollable range is the root element's own scrollable
  overflow. A body element with `overflow: hidden`, `scroll`, or `auto` does not
  contribute its scrolling area when the root element's overflow is `visible`,
  although CSS overflow propagates the body's overflow to the viewport
  (<https://drafts.csswg.org/css-overflow-3/#overflow-propagation>). A page that
  hides body overflow and then scrolls the document clamps to zero.
- A child frame's viewport is its iframe's content box, forwarded by
  `Engine::sync_child_frame_viewports` after the parent lays out
  (`crates/renderer/src/engine.rs`). Blitz itself does not couple the two, so
  the engine re-lays out the child when the box size changes. The iframe's
  `width`/`height` attributes and CSS size reach the child; scrollbar gutters
  and `scrolling="no"` still do not reduce it.
- `scroll-behavior: smooth` starts Blitz's animation, but the in-process WPT
  harness does not drive the renderer's frame loop to completion, so a test that
  waits for the animation end times out. Instant and auto scrolling work.
- `scrollIntoView` scrolls every scrolling box in the target's containing
  block chain, innermost to outermost, then the viewport. The block and inline
  positions are mapped through each box's writing mode, and the `container`
  option's `"nearest"` stops after the nearest scroller.
  `scroll-margin`/`scroll-padding` are not applied because neither property
  exists in this build: both are `engine = "gecko"` longhands in the pinned
  Stylo's property tables, which the Servo build does not generate (the same
  reason `content-visibility` is missing). `CSS.supports("scroll-margin-top",
  "4px")` and `CSS.supports("scroll-padding-top", "4px")` are both false, so
  `scrollIntoView-scrollMargin.html`, `scrollIntoView-scrollPadding.html`, and
  `scrollIntoView-scrolling-container.html` cannot pass until the property
  tables include them.
- `sideways-lr` float placement is wrong in layout: floats on one line stack
  along the block axis instead of the inline axis, so a row's boxes drift in the
  block direction and the target lands at the wrong position. The two
  `scrollIntoView-sideways-lr-*` files compute their expectations from layout
  and stay red for that reason; the scroll algorithm itself maps `sideways-lr`
  like the other vertical modes (`docs/upstream.md`).
- `position: sticky` has no layout offset, so `scrollIntoView` reads a stuck
  element's unstuck position. `scrollIntoView-stuck.tentative.html` scrolls its
  container back to the top because the sticky input is recorded at its static
  position; the gap is the same one recorded under `## Element offset metrics`.

The `scroll`/`scrollTo`/`scrollBy`/`scrollIntoView` methods return
`Promise<undefined>`, which is what the pinned IDL says. The current CSSOM View
draft returns `Promise<ScrollResult>` with an `interrupted` member, so tests
written against that draft fail regardless of scrolling behavior.

## Visibility observation

`IntersectionObserver` runs a per-frame update in
`crates/renderer/src/js/scripts/web/observers.js`: it computes the target's
border box, clips it by ancestor scrollports and the root intersection
rectangle, and reports `isIntersecting`, `intersectionRatio`, and the three
rects. The implicit root is the viewport; an element root uses its padding area
when it is a scroll container and its border box otherwise, and a `Document`
root uses that document's viewport. `rootMargin`, thresholds, `unobserve`,
`disconnect`, `takeRecords`, and the `Element or Document` root type follow the
algorithm (<https://w3c.github.io/IntersectionObserver/#update-intersection-observations-algo>).

What remains:

- `scroll-margin` is not part of the clip rect: the property is a
  `engine = "gecko"` longhand in the pinned Stylo and does not parse, so every
  `scroll-margin-*` file reports the unadjusted scrollport
  (`docs/upstream.md`). The same gap removes `scroll-margin` from
  `scrollIntoView`.
- The v2 occlusion tests (`v2/*-occlusion*`, `v2/text-shadow`,
  `v2/box-shadow`) need paint-order occlusion, which this engine does not
  model. `isVisible` is `checkVisibility()`, so opacity and
  `content-visibility` are not part of it (`content-visibility` is missing from
  this Stylo build).
- Cross-origin and nested-iframe targets are measured in their own realm's
  viewport; the parent frame's offset is not folded in
  (`cross-origin-*`, `same-origin-grand-child-iframe`, `target-in-different-window`).
- Lazy-loaded images are not deferred by an observer
  (`img-lazy-loading-not-blocked-by-img-outside-viewport`), CSS animations do
  not run (`animating`, `clip-path-animation`, `transform-animation`), and
  `clip-path` clips by its bounding box rather than its shape.
- A `display: none` target still reports an edge-adjacent zero rect as
  intersecting (`display-none.html`), because the update has no rendered-box
  gate.

## Resize observation

`ResizeObserver` lives in `crates/renderer/src/js/scripts/web/observers.js`.
It measures the content, border, and device-pixel boxes per frame and delivers
only boxes whose size changed, including the initial observation, with
`contentRect` relative to the padding box and logical `inlineSize`/`blockSize`
for vertical writing modes. Callback-driven resizes are measured again in the
same frame up to five passes, and undelivered work after that dispatches the
`ResizeObserver loop completed with undelivered notifications.` error.

What remains:

- The loop stops after 20 idle frames so a reftest can settle. A size change
  that arrives later than that window, and that is not preceded by a new
  `observe()`, is missed; browsers run the observation at every rendering
  opportunity.
- The depth-limited delivery order of the resize loop is not modelled, so
  `eventloop.html` reports an extra entry per iteration, and the loop error is
  not raised in `change-layout-in-error.html`.
- An inline element keeps its block size in Blitz's layout, so changing
  `display` to `inline` produces no notification (`observe-016.html`); the
  browser reports a 0x0 box.
- `devicePixelContentBoxSize` uses a device-pixel ratio of 1 and the layout's
  box size, so the `zoom` files disagree about which members are scaled
  (`zoom.html`, `observe-020.html`).
- No scrollbar gutter exists in layout, so `scrollbars.html` cannot subtract
  one, and SVG boxes are measured from their CSS box only
  (`svg.html`, `svg-with-css-box-*`).
- A fragment-triggered notification (`fragments.html`) needs fragment-aware
  content-box sizing, and a callback exception is rethrown in the observer's
  realm rather than reported at the realm that created the callback
  (`callback-cross-realm-report-exception.html`).

## Custom elements

`customElements.define()` validates the name (including the reserved SVG
names) and reads the constructor and prototype in the spec's order:
`prototype`, the five reaction callbacks, `observedAttributes` when
`attributeChangedCallback` is present, `disabledFeatures`, `formAssociated`,
and the four form-associated callbacks when it is true. Exceptions from a
getter or from sequence conversion propagate, and `moveBefore` keeps
`connectedMoveCallback` alive.

What remains:

- Customized built-in elements (`{ extends }`) throw `NotSupportedError`;
  `builtin-coverage.html` and the `reactions/customized-builtins/*` files
  cannot pass until `is=` construction and its interface mapping exist.
- `new MyElement()` outside an upgrade throws `Illegal constructor`: the
  HTMLElement constructor does not turn a registered definition's constructor
  into a new element, so `CustomElementRegistry.html`'s instantiation case
  fails.
- Connection reactions do not follow a shadow host into its shadow tree, and
  the document/adoption variants (template, XHR, cloned, iframe, new document)
  do not produce their callbacks (`connected-callbacks.html`,
  `disconnected-callbacks.html`, `adopted-callback.html`).
- `ElementInternals` is a partial surface: form value, validation, and ARIA
  reflection are incomplete (`form-associated/*`,
  `ElementInternals-accessibility*.html`).
- Scoped registries are not implemented (`registries/*`).



## ARIA reflection

Seven enumerated ARIA state attributes use the reflected-enumerated table:
`aria-atomic`, `aria-checked`, `aria-expanded`, `aria-haspopup`,
`aria-orientation`, `aria-pressed`, and `aria-selected`
(`crates/renderer/src/js/bindings/reflected-enumerated.txt`). Their getters map
the content value through its keyword set, apply the invalid-value default
when the value names no keyword (`false` for `aria-atomic` and
`aria-haspopup`), and report the Undefined state as `null` because the IDL
member is `DOMString?`
(<https://w3c.github.io/aria/#enumerated-attribute-values-html>).

The other thirteen enumerated ARIA attributes (`aria-autocomplete`,
`aria-busy`, `aria-current`, `aria-disabled`, `aria-hidden`, `aria-invalid`,
`aria-live`, `aria-modal`, `aria-multiline`, `aria-multiselectable`,
`aria-readonly`, `aria-required`, `aria-sort`) keep transparent nullable-string
reflection. Their proposed missing-value default is a keyword (`false`,
`none`, `off`), and that conflicts with the published ARIA 1.3 algorithm:
`aria-attribute-reflection.html` (and the published text) require a removed
attribute to read `null`, while
`aria-attribute-reflection-enumerated.tentative.html` (tracking
<https://github.com/w3c/aria/pull/2484>) requires a never-set attribute to
read its missing-value default. Both tests describe the same DOM state, so no
implementation can satisfy both; the stable test follows the published spec and
stays green until the ARIA change ships. The tentative test's failing subtests
are therefore the two per attribute that read a removed attribute.

## Element offset metrics

`offsetParent`, `offsetLeft`, `offsetTop`, `offsetWidth`, and `offsetHeight`
follow the CSSOM View algorithms and share Blitz's offset rectangle, which
handles the first fragment box of a non-atomic inline. Remaining gaps:

- Table layout ignores captions ("Probably a table caption: ignore" in
  `third_party/blitz/packages/blitz-dom/src/layout/table.rs`), so a table's
  `offsetWidth`/`offsetHeight` exclude the caption and a cell's `offsetTop` does
  not account for it. `offsetTopLeft-table-caption.html` fails and
  `table-offset-props.html` passes 5 of 13. Caption layout belongs upstream
  (`docs/upstream.md`).
- CSS `zoom` is parsed but never applied to layout, so offsets and sizes are
  unscaled; `offsetTop-offsetLeft-with-zoom.html` cannot pass. The `zoom`
  property is not in the current CSSOM View algorithms, which apply `unscaled`
  coordinates to `offsetTop`/`offsetLeft`, so the property needs layout support
  before those tests can distinguish the rules. `offsetParent`'s "different
  effective zoom" clause is unimplemented for the same reason; Chrome does not
  fire it either (probed), so no test covers it.
- Vertical writing modes are laid out: `blitz-dom`'s `writing-mode` feature is
  on, and its `init_stylo_prefs` sets the style crate's
  `layout.css.writing-mode.enabled` and `...sideways.enabled` prefs, so the
  property parses, computes, and drives layout in each box's writing-mode frame.
  `scrollIntoView` still scrolls only the viewport (see `## Scrolling`), so the
  `scrollIntoView-*-writing-mode.html` family stays red even though the boxes
  are laid out vertically.
- An empty non-atomic inline has no fragment box: Blitz records fragments only
  for glyphs and atomic inline boxes. `offsetLeft`/`offsetTop` therefore
  approximate its position from the next boxed sibling's first fragment, or
  from the previous sibling's end along the inline axis. The approximation
  picks the sibling's border-box top rather than the line-box top, so it is off
  when an atomic inline is baseline-aligned in a taller line
  (`offsetTop-offsetLeft-nested-offsetParents.html` reads 17 where the line top
  is 10), and it is wrong across a line break.
- `offsetParent`'s walk is shadow-including, not the flat tree, for the reason
  recorded under `## CSS content-visibility`. The spec's "closed-shadow-hidden
  and fixed" early return cannot be evaluated either: the walk crosses a closed
  shadow root to its host, where the hidden relationship is not observable, and
  shadow-tree content has no computed styles. Blitz's own
  `Node::offset_top_left` also stops only at positioned, `body`, `td`, and `th`
  ancestors, so it does not see a `transform`/`filter`/`contain` fixed-position
  containing block or a `table`; inside such an ancestor the offset rectangle
  and `offsetParent` can disagree (`docs/upstream.md`).
- `position: sticky` has no offset in layout, so a stuck box's `offsetLeft`
  reports its unstuck position. `position-sticky-right.html` and
  `position-sticky-bottom.html` compare that value against the containing block
  and now fail the assertions that previously passed only because `offsetLeft`
  was always 0 (`docs/upstream.md`).
- `display: ruby-text` is a Gecko-only `DisplayInside` variant in the pinned
  Stylo, so the declaration is dropped and the previous `display` stays.
  `align-content-block-display-coverage.html`'s unsupported-value case reads
  that stale display through `offsetTop` and fails; the other seven cases pass
  now that `offsetTop` reports the aligned position.
- `clientLeft`/`clientTop` are the used border widths; Blitz renders
  scrollbars on the right and bottom only, so a scrollbar between the left
  padding edge and the left border edge (RTL, or a vertical writing mode) is
  never part of the value. `client-scroll-props-input.html` also expects an
  `input`'s inline padding to be included after the leading border
  (<https://github.com/w3c/csswg-drafts/issues/13502>), a draft change the
  engine does not model yet.
- `scrollParent` is not implemented. The current CSSOM View draft declares
  `readonly attribute Element? scrollParent`, but the pinned WPT tests call
  `element.scrollParent()` and no shipping browser implements either shape, so
  the API is deferred until the surface settles (`docs/priorities.md`).


## Custom scheme handlers

`navigator.registerProtocolHandler()` and `unregisterProtocolHandler()` run the
spec's normalize protocol handler parameters algorithm: ASCII-lowercase the
scheme, accept only a safelisted scheme or `web+` followed by one or more ASCII
lowercase letters, require a `%s` placeholder, encoding-parse the URL relative
to the document, and require an HTTP(S) same-origin URL
(<https://html.spec.whatwg.org/multipage/system-state.html#normalize-protocol-handler-parameters>).
Accepted registrations live in a per-realm map; there is no user-consent
prompt, which the spec allows ("user agents could also silently collect the
information", step 2).

The engine does not yet route a navigation to a registered scheme through its
handler. Invoking a handler replaces the first `%s` with the percent-encoded
input URL and navigates to the result
(<https://html.spec.whatwg.org/multipage/system-state.html#invoke-a-protocol-handler>),
which needs navigation interception for non-HTTP schemes. A page can register
and unregister handlers, and the argument checks match a browser, but a
registered handler has no observable effect on navigation yet.


## Scoped styles

`@scope` rules do not parse: the style crate gates the at-rule behind
`layout.css.at-scope.enabled`, which defaults off, and enabling it makes an
implicit `@scope { … }` scope to the document element instead of the parent of
the owning `<style>`, so the rule applies to the scope root. The style crate's
stylesheet type carries no owner node to fix that with. The 262 `@scope`
subtests in `css/css-cascade` mostly fail, and the pref stays off;
`docs/upstream.md` records the reason.

## Imported CSS style sheets

`StyleSheet` on an associated sheet reports `type`, `href`, `ownerNode`,
`parentStyleSheet`, `ownerRule`, `title`, `media`, and `disabled`
(`crates/renderer/src/js/scripts/web/cssom.js`). `disabled` is wired to the
engine's `set_stylesheet_disabled`, so a disabled sheet keeps its owner and
stops applying. `media` is a live `MediaList` over the owner's `media`
attribute, and `CSSRuleList`/`MediaList` are interface objects.

`CSSImportRule.styleSheet` answers the imported sheet: the engine opens a CSSOM
sheet record for it with no owner node, and the renderer wraps it with the
importing sheet as `parentStyleSheet` and the rule as `ownerRule`. Its `media`
is the same `MediaList` object as `CSSImportRule.media`, and its `cssRules` is
its own live list addressed by the imported sheet's id, so nested imports and
`insertRule` work on it
(`crates/renderer/src/js/scripts/web/cssom.js`,
`crates/renderer/src/js/bindings/cssom_host.rs`,
`third_party/blitz/packages/blitz-dom/src/cssom.rs`).

A pending import has no sheet object yet: stylo's `ImportSheet::Pending` is
replaced by the loaded sheet only when the fetch lands, so `styleSheet` reads
null until then (and reports null for a refused import forever). Browsers
create the imported sheet synchronously and fill it in place, which would need
the engine to parse into an existing sheet rather than swap it in
(`css/cssom/CSSImportRule-insertRule-styleSheet-parentStyleSheet.html` reads
`styleSheet` in the same task as `insertRule`).


## Origin API

`new Origin()`, `Origin.from()` for an absolute URL string, a `URL`, an
`Origin`, the current global, a dispatched `MessageEvent`, and an HTML `<a>` or
`<area>`, plus `opaque`, `isSameOrigin()`, and `isSameSite()`, are implemented
(`crates/renderer/src/js/scripts/web/origin.js`,
`crates/renderer/src/js/bindings/origin.rs`). A tuple origin compares by its
ASCII serialization; an opaque origin compares by identity, and the realm's own
opaque origin is one identity across `Origin.from(globalThis)` calls. Same-site
uses the cookie rules' scheme + registrable-domain comparison.

`Origin.from()` on an SVG or MathML `<a>` reads the element's `href`, falling
back to SVG's legacy `xlink:href`
(`crates/renderer/src/js/scripts/web/origin.js`). SVG's `href` is an
`SVGAnimatedString` and MathML's is a plain string
(`crates/renderer/src/js/scripts/web/svg.js`).

One shape deviation: the engine builds one `SVGElement` prototype for every SVG
element and one `MathMLElement` prototype for every MathML element, so `href`
sits on those prototypes and answers only for the elements that include
`SVGURIReference` or `HyperlinkElementUtils`. It is therefore `undefined` on,
say, `SVG <rect>` instead of being absent from it. Per-interface prototypes
would be needed to match the spec's shape exactly.

`Origin.from(Window)` works for the current global and for a data-URL iframe's
own `Origin.from(globalThis)`, but not for another frame's `WindowProxy`: the
proxy carries a frame id, not an origin, and there is no public extract hook
for it. `origin-from-window.window.html` therefore passes five of seven
subtests.

Opaque origins from sandboxed-frame messages compare by event, not by the
sender's document: each message's opaque origin is a new token, so two messages
from one sandboxed document do not compare same-origin. The third
`origin-from-messageevent-opaque.window.html` case also reloads a frame through
`navigation.reload()`, which the engine does not navigate.

## Window interface object

The global object is the `Window` instance, but it is not built that way: it
starts as the realm's own global prototype and the engine installs `window`,
`document`, the event-handler properties, and the CSSOM/session members on it
directly. There is no `Window` interface object, so
`html/browsers/the-window-object/window-properties.https.html` fails its
`Window method:` and `Window accessor:` groups with `ReferenceError: Window is
not defined` and fails the `EventTarget method:` group because the global's own
`addEventListener` is a host function rather than the interface prototype's.

Rewiring the prototype after the shims loaded — defining `Window` with
`Window.prototype` chaining to `EventTarget.prototype` and calling
`Object.setPrototypeOf(globalThis, Window.prototype)` at the end of the script
order — was tried and rejected. It raised that file from 36 to 125 passing
subtests but lost hundreds elsewhere, because several shims and the messaging
proxy treat "own property of the global" as the signal for a window member:
`css/selectors` 4689 to 2334, `css/css-cascade` 210 to 130,
`referrer-policy` 8020 to 7962, `css/cssom-view` 1594 to 1630 with 38 lost,
`html/browsers` 679 to 947 with 37 lost, and smaller losses in
`css/cssom`, `css/css-variables`, `dom/nodes`, and `css/css-conditional`. The
repair is structural, not a shim: build the global as a `Window` instance in
Rust (its prototype chain is `Window.prototype` → `EventTarget.prototype` →
`Object.prototype`), install the members on `Window.prototype`, and keep the
realm's own data on the global. Until then the property shape is the
documented gap, not a fake interface object.


## Unsafe HTML setters

`Element.setHTMLUnsafe()` and `ShadowRoot.setHTMLUnsafe()` parse a fragment in
the target's context and replace its children, and with `runScripts: true`
they run the fragment's scripts in tree order after the tree is updated
(`crates/renderer/src/js/bindings/node/element.rs`). `innerHTML` shares the
parse and insertion path and never runs scripts.

The partial-update insertion methods also exist:
`appendHTMLUnsafe`/`prependHTMLUnsafe` on `Element` and `ShadowRoot`, and
`beforeHTMLUnsafe`/`afterHTMLUnsafe`/`replaceWithHTMLUnsafe` on `Element` and
`CharacterData`. They follow the spec's "target for HTML insertion" (a
template unwraps to its contents) and "parent for HTML insertion" (a
`template` element, a `Document`, or a `DocumentFragment` without a host
throws `HierarchyRequestError`). Template contents live in their own inert
document, so `BlitzDocument::template_host` carries the host back link that the
fragment node cannot.

Five things the spec's algorithms ask for are absent:

- Declarative shadow roots. The parser has no `allowDeclarativeShadowRoots`
  mode, so a `<template shadowrootmode=open>` stays a template and the shadow
  tree is never attached. `setHTMLUnsafe.html`'s two `with shadowdom` subtests
  and the two `script inside declarative shadow DOM` subtests depend on it.
- The `Sanitizer` API. `SetHTMLUnsafeOptions.sanitizer` is not read, so a
  sanitizer that removes scripts does not stop `runScripts`, and
  `positional-methods.html`'s sanitizer subtests see the element it was asked
  to remove.
- The safe variants (`setHTML()`, `appendHTML()`, `prependHTML()`,
  `beforeHTML()`, `afterHTML()`, `replaceWithHTML()`) do not exist yet,
  because their default sanitizer configuration cannot be provided without the
  `Sanitizer` API. Half of `positional-methods.html` calls them.
- A script element created by `Document.parseHTMLUnsafe()` is not marked
  already started, so appending it into a document runs it. The spec parses
  into an inert document whose scripts never execute.
- `setHTMLUnsafe()` and the insertion methods on an XML document parse as
  HTML: the spec's fragment parsing dispatches on the document type, and the
  engine has one HTML fragment path.


## Window surface

The window's own members that no generated interface provides are installed by
`crates/renderer/src/js/scripts/web/window.js`: `BarProp` with the six
`[Replaceable]` bar attributes, `devicePixelRatio`, `screenX`, `screenY`,
`status`, `closed`, `frameElement`, and the `alert`, `confirm`, `prompt`,
`print`, `moveBy`, `moveTo`, `resizeBy`, `resizeTo`, `stop`, `blur`, and
`focus` operations. There is no window manager, so the geometry and bar
attributes report the viewport and `visible: true`, prompts do nothing,
`confirm` answers false, and `prompt` answers null.

Three members are absent: `window.external` (a legacy interface no current
specification defines), `onmousewheel` (a legacy alias of `onwheel`), and
`getSelection` (the `Selection` interface is not implemented).

Cross-origin window member access answers `undefined` instead of throwing
`SecurityError`. The spec throws for every property outside the cross-origin
list, and implementing that was tried: it raises
`window-security.https.html` from 12 to 121 of 124, but it regresses
`windowproxy-prototype-setting-same-origin-domain.sub.html` and
`location-prototype-setting-cross-origin-domain.sub.html`, whose subtests read
`frames[0].TypeError` and `frames[0].location` after setting `document.domain`.
`document.domain` is unimplemented and is the feature those tests exercise, so
closing this needs same-origin-domain origin comparison (an origin type that
carries an optional domain) and a cross-origin `Location` object with a null,
immutable prototype. Reverted until that lands; the throw is otherwise
correct.

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

Still open in `dom/events`: `FocusEvent`/`MouseEvent` `relatedTarget` is not
retargeted across a shadow boundary and dispatch does not reset `target` and
`relatedTarget` afterwards, so `relatedTarget.window.html` and
`shadow-relatedTarget.html` fail; the event path follows parent nodes and
crosses a shadow root to its host for a composed event, but it does not follow
`assignedSlot` because Blitz has no slot assignment. A listener that is a
plain object created in another realm cannot report its exception in that
realm, because `QuickJS` stores a realm only on functions
(`EventListener-handleEvent-cross-realm.html`, recorded in
`docs/upstream.md`). `Event.timeStamp` now shares `performance.now()`'s time
origin, but the engine does not coarsen the clock to HR-Time's 5-microsecond
resolution, so `Event-timestamp-safe-resolution.html` stays red.

Event handler attributes use HTML's event handler map: the content attribute
stores its raw source, activation registers a real listener, and the source
compiles lazily in the handler document's realm. The scope chain is still the
global one. HTML puts a `NewObjectEnvironment(document, true, …)` around it,
then the element and its form owner, so `compile-event-handler-lexical-scopes*`
and `compile-event-handler-symbol-unscopables` fail: a handler cannot see
`document` properties, element mixin members, or `form.elements` through the
bare `elements` name. `QuickJS`'s `Function` constructor and `eval` can only
reach the global environment, so this needs a custom environment record rather
than another wrapper. The same gap covers the element's own properties
(`HTMLTableCellElement.prototype.cellIndex` and friends) and the form owner.
`event-handler-idl-attribute-realm` also fails: compilation happens in the
realm `world_for_node` resolves for the target document, but after the test
moves a body between frames the getter still answers null instead of a function
from the new document's realm. Two more gaps are recorded rather than worked
around: a parser-installed handler runs its attribute change steps in the walk
at realm install, so the CSP inline check for a parsed handler fires before
page script rather than at parse time, and `compile-event-handler-settings-objects`
still times out.

The same-document traversal files
(`html/browsers/browsing-the-web/history-traversal/event-order/same-document-traverse-*.html`)
used to fail their first ordering assertion because handlers ran after the
listener list. With the map they now pass that assertion and hang on
`history.back()`: this engine does not fire `popstate` and `hashchange`
synchronously from the traversal inside a `hashchange` listener, so the test's
second listener never runs.
`Event-dispatch-single-activation-behavior.html`
exceeds the harness budget. `Event-dispatch-click.html` is flaky under the
in-process harness: it passes its 33 subtests in some batch runs and hits the
per-file budget in others, on both the old and the new tree.

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
