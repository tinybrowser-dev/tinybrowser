// SVG and MathML hyperlink surfaces.
//
// The engine exposes one `SVGElement` prototype for every SVG element and one
// `MathMLElement` prototype for every MathML element, so the URI-reference
// members live on those prototypes and answer only for the elements that
// include the mixin. `docs/limitations.md` records that shape: `href` is
// present on every SVG/Math element instead of only on the interfaces that
// include `SVGURIReference`/`HyperlinkElementUtils`.
{
  const SVG_NS = 'http://www.w3.org/2000/svg';

  // The SVG elements that include `SVGURIReference`, by local name
  // (<https://svgwg.org/svg2-draft/linking.html#InterfaceSVGURIReference>).
  const uri_reference_elements = new Set([
    'a', 'color-profile', 'cursor', 'feImage', 'filter', 'font-face-uri',
    'glyphRef', 'image', 'linearGradient', 'mpath', 'pattern',
    'radialGradient', 'script', 'textPath', 'tref', 'use',
  ]);

  // ── SVGAnimatedString ────────────────────────────────────────────────
  // A value that reflects one attribute. There is no animation support, so
  // `animVal` equals `baseVal`
  // (<https://svgwg.org/svg2-draft/types.html#InterfaceSVGAnimatedString>).
  const animated_slots = host.slots('SVGAnimatedString');
  function SVGAnimatedString() {
    throw new TypeError('Illegal constructor');
  }
  SVGAnimatedString.prototype = Object.create(Object.prototype, {
    constructor: { value: SVGAnimatedString, writable: true, configurable: true },
  });
  const animated = value => {
    const entry = animated_slots.get(value);
    if (entry === undefined) throw new TypeError('Illegal invocation');
    return entry;
  };
  Object.defineProperty(SVGAnimatedString.prototype, 'baseVal', {
    get() {
      const { element, attribute } = animated(this);
      const value = element.getAttribute(attribute);
      return value === null ? '' : value;
    },
    set(value) {
      const { element, attribute } = animated(this);
      element.setAttribute(attribute, __tbIDLString(value));
    },
    enumerable: true,
    configurable: true,
  });
  Object.defineProperty(SVGAnimatedString.prototype, 'animVal', {
    get() {
      const { element, attribute } = animated(this);
      const value = element.getAttribute(attribute);
      return value === null ? '' : value;
    },
    enumerable: true,
    configurable: true,
  });
  Object.defineProperty(SVGAnimatedString.prototype, Symbol.toStringTag, {
    value: 'SVGAnimatedString', writable: false, enumerable: false, configurable: true,
  });
  Object.defineProperty(globalThis, 'SVGAnimatedString', {
    value: SVGAnimatedString, writable: true, configurable: true,
  });

  // One `SVGAnimatedString` per element and attribute, because the IDL
  // attribute is `[SameObject]`
  // (<https://svgwg.org/svg2-draft/linking.html#__svg__SVGURIReference__href>).
  const animated_cache = new WeakMap();
  const animated_attribute = (element, attribute) => {
    let by_attribute = animated_cache.get(element);
    if (by_attribute === undefined) {
      by_attribute = new Map();
      animated_cache.set(element, by_attribute);
    }
    let value = by_attribute.get(attribute);
    if (value === undefined) {
      value = Object.create(SVGAnimatedString.prototype);
      animated_slots.set(value, { element, attribute });
      by_attribute.set(attribute, value);
    }
    return value;
  };

  if (globalThis.SVGElement !== undefined) {
    Object.defineProperty(globalThis.SVGElement.prototype, 'href', {
      get() {
        if (this.namespaceURI !== SVG_NS || !uri_reference_elements.has(this.localName)) {
          return undefined;
        }
        return animated_attribute(this, 'href');
      },
      enumerable: true,
      configurable: true,
    });
  }

  // ── MathML <a> ──────────────────────────────────────────────────────
  // `MathMLAnchorElement` reflects `href` as a USVString and includes
  // `HyperlinkElementUtils`
  // (<https://w3c.github.io/mathml-core/#the-mathml-anchor-element>).
  if (globalThis.MathMLElement !== undefined) {
    Object.defineProperty(globalThis.MathMLElement.prototype, 'href', {
      get() {
        if (this.namespaceURI !== 'http://www.w3.org/1998/Math/MathML'
            || this.localName !== 'a') {
          return undefined;
        }
        const value = this.getAttribute('href');
        return value === null ? '' : value;
      },
      set(value) {
        if (this.namespaceURI !== 'http://www.w3.org/1998/Math/MathML'
            || this.localName !== 'a') {
          throw new TypeError('Illegal invocation');
        }
        this.setAttribute('href', __tbIDLUSVString(value));
      },
      enumerable: true,
      configurable: true,
    });
  }
}
