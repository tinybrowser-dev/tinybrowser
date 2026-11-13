// The `Origin` interface
// (<https://html.spec.whatwg.org/multipage/browsers.html#the-origin-interface>).
//
// `Origin` wraps an origin value so scripts can compare origins without
// serialization. A tuple origin is its ASCII serialization; an opaque origin
// has no serialization, so it compares by identity: `new Origin()` is a new
// opaque origin, and `Origin.from(existingOrigin)` shares the existing one's
// identity.
{
  const slots = host.slots('Origin');
  const HTML_NS = 'http://www.w3.org/1999/xhtml';
  const SVG_NS = 'http://www.w3.org/2000/svg';
  const MATHML_NS = 'http://www.w3.org/1998/Math/MathML';

  const brand = value => {
    const slot = slots.get(value);
    if (!slot) throw new TypeError('Illegal invocation');
    return slot;
  };
  const newOpaque = () => ({ opaque: true, token: Object.freeze({}) });
  // The realm's own opaque origin is one origin, so repeated
  // `Origin.from(globalThis)` calls share its identity; a new opaque origin
  // (the constructor, or a fresh URL parse) gets a new token.
  let globalOpaqueToken = null;
  const fromGlobal = () => {
    if (host.__tbOpaqueOrigin !== true) return { opaque: false, origin: host.__tbOrigin() };
    if (globalOpaqueToken === null) globalOpaqueToken = Object.freeze({});
    return { opaque: true, token: globalOpaqueToken };
  };
  const fromString = value => {
    const parsed = host.__tbOriginFromString(value);
    if (parsed.ok !== true) return null;
    if (parsed.opaque === true) return newOpaque();
    return { opaque: false, origin: parsed.origin };
  };
  const extracted = value => {
    const existing = slots.get(value);
    if (existing !== undefined) return existing;
    // A `WindowProxy` answers only when it is same-origin, and then its origin
    // is this realm's; a cross-origin one has no extract-an-origin operation
    // (<https://html.spec.whatwg.org/multipage/browsers.html#the-origin-interface>).
    const proxy = host.__tbWindowProxyAccess(value);
    if (proxy === 'same') return fromGlobal();
    if (proxy === 'cross') return null;
    if (value instanceof globalThis.URL) {
      const parsed = fromString(value.href);
      if (parsed !== null) return parsed;
    }
    // An HTML `<a>` or `<area>` is a HyperlinkElementUtils: the extracted
    // origin is its `href`'s, and an element with no href has none
    // (<https://html.spec.whatwg.org/multipage/links.html#hyperlinkelementutils>).
    if (value.nodeType === 1 && value.namespaceURI === HTML_NS
        && (value.localName === 'a' || value.localName === 'area')) {
      const parsed = fromString(value.href);
      if (parsed !== null) return parsed;
    }
    // SVG's `<a>` extracts from `href`, falling back to the legacy
    // `xlink:href` attribute; MathML's `<a>` reflects `href` as a string
    // (<https://svgwg.org/svg2-draft/linking.html#InterfaceSVGURIReference>,
    // <https://w3c.github.io/mathml-core/#the-mathml-anchor-element>).
    if (value.nodeType === 1 && value.localName === 'a'
        && value.namespaceURI === SVG_NS) {
      const href = value.getAttribute('href');
      const parsed = fromString(href === null ? value.getAttribute('xlink:href') : href);
      if (parsed !== null) return parsed;
    }
    if (value.nodeType === 1 && value.localName === 'a'
        && value.namespaceURI === MATHML_NS) {
      const parsed = fromString(value.href);
      if (parsed !== null) return parsed;
    }
    if (value === globalThis || value === globalThis.window) return fromGlobal();
    // A dispatched `MessageEvent` carries its sender's origin; a constructed
    // one has no real origin and does not extract one
    // (<https://html.spec.whatwg.org/multipage/browsers.html#the-origin-interface>).
    if (typeof host.__tbOriginOfMessageEvent === 'function') {
      const origin = host.__tbOriginOfMessageEvent(value);
      if (origin === null) return null;
      if (typeof origin === 'string') {
        return origin === 'null' ? newOpaque() : fromString(origin);
      }
    }
    return null;
  };

  class Origin {
    // https://html.spec.whatwg.org/multipage/browsers.html#dom-origin-origin
    constructor() {
      slots.set(this, newOpaque());
    }

    // https://html.spec.whatwg.org/multipage/browsers.html#dom-origin-from
    static from(value) {
      const instance = new Origin();
      if (value !== null && (typeof value === 'object' || typeof value === 'function')) {
        const origin = extracted(value);
        if (origin !== null) {
          slots.set(instance, origin);
          return instance;
        }
      }
      if (typeof value === 'string') {
        const origin = fromString(value);
        if (origin !== null) {
          slots.set(instance, origin);
          return instance;
        }
      }
      throw new TypeError('The value is not an origin and does not name one.');
    }

    // https://html.spec.whatwg.org/multipage/browsers.html#dom-origin-opaque
    get opaque() {
      return brand(this).opaque;
    }

    // https://html.spec.whatwg.org/multipage/browsers.html#dom-origin-issameorigin
    isSameOrigin(other) {
      const left = brand(this);
      const right = brand(other);
      if (left.opaque || right.opaque) {
        return left.opaque && right.opaque && left.token === right.token;
      }
      return left.origin === right.origin;
    }

    // https://html.spec.whatwg.org/multipage/browsers.html#dom-origin-issamesite
    isSameSite(other) {
      const left = brand(this);
      const right = brand(other);
      if (left.opaque || right.opaque) {
        return left.opaque && right.opaque && left.token === right.token;
      }
      return host.__tbOriginSameSite(left.origin, right.origin) === true;
    }
  }
  Object.defineProperty(Origin.prototype, Symbol.toStringTag, {
    value: 'Origin', writable: false, enumerable: false, configurable: true,
  });
  Object.defineProperty(globalThis, 'Origin', {
    value: Origin, writable: true, configurable: true,
  });
}
