// CSSOM View's GeometryUtils mixin and the DOMQuad/DOMRectList types
// (<https://drafts.csswg.org/cssom-view/#the-geometryutils-interface>,
// <https://drafts.fxtf.org/geometry/#DOMQuad>).
//
// The renderer exposes each element's border box through
// `getBoundingClientRect()` and `getClientRects()`. This shim derives the
// margin, padding, and content boxes from those and the element's computed
// edges, so every GeometryUtils result agrees with the CSSOM View boxes the
// page already reads.
{
  const __tbDOMRect = globalThis.DOMRect;
  const __tbDOMPoint = globalThis.DOMPoint;
  // `Object.prototype.toString` reads the `Symbol.toStringTag` of the native
  // geometry objects, which `assert_class_string` checks
  // (<https://tc39.es/ecma262/#sec-object.prototype.tostring>).
  for (const [prototype, tag] of [
    [__tbDOMRect && __tbDOMRect.prototype, 'DOMRect'],
    [__tbDOMPoint && __tbDOMPoint.prototype, 'DOMPoint'],
  ]) {
    if (prototype && prototype[Symbol.toStringTag] === undefined) {
      Object.defineProperty(prototype, Symbol.toStringTag, {
        configurable: true, value: tag,
      });
    }
  }
  // WebIDL dictionary member conversion to `unrestricted double`: a present
  // member keeps every value, including `NaN` and the infinities; an absent
  // one takes the dictionary's default
  // (<https://webidl.spec.whatwg.org/#es-dictionary>).
  const __tbDouble = (dictionary, name, fallback) =>
    dictionary[name] === undefined ? fallback : Number(dictionary[name]);
  // WebIDL dictionary conversion: only `undefined` and `null` read as the
  // empty dictionary; any other primitive is a `TypeError`
  // (<https://webidl.spec.whatwg.org/#es-dictionary>).
  const __tbDictionary = (value, what) => {
    if (value === undefined || value === null) return {};
    if (typeof value !== 'object' && typeof value !== 'function') {
      throw new TypeError(what + ' is not a dictionary');
    }
    return value;
  };
  const __tbPointInit = value => {
    const dictionary = __tbDictionary(value, 'DOMPointInit');
    return new __tbDOMPoint(
      __tbDouble(dictionary, 'x', 0),
      __tbDouble(dictionary, 'y', 0),
      __tbDouble(dictionary, 'z', 0),
      __tbDouble(dictionary, 'w', 1));
  };

  // https://drafts.fxtf.org/geometry/#domquad
  class DOMQuad {
    constructor(p1, p2, p3, p4) {
      const points = [
        __tbPointInit(p1), __tbPointInit(p2),
        __tbPointInit(p3), __tbPointInit(p4),
      ];
      Object.defineProperty(this, '_points', {
        configurable: true, enumerable: false, writable: true, value: points,
      });
    }
    get p1() { return this._points[0]; }
    get p2() { return this._points[1]; }
    get p3() { return this._points[2]; }
    get p4() { return this._points[3]; }
    getBounds() {
      let left = Infinity;
      let top = Infinity;
      let right = -Infinity;
      let bottom = -Infinity;
      for (let index = 0; index < 4; index += 1) {
        const point = this._points[index];
        left = Math.min(left, point.x);
        top = Math.min(top, point.y);
        right = Math.max(right, point.x);
        bottom = Math.max(bottom, point.y);
      }
      return new __tbDOMRect(left, top, right - left, bottom - top);
    }
    toJSON() {
      const point = value => ({
        x: value.x, y: value.y, z: value.z, w: value.w,
      });
      return {
        p1: point(this._points[0]),
        p2: point(this._points[1]),
        p3: point(this._points[2]),
        p4: point(this._points[3]),
      };
    }
    static fromRect(rect) {
      const dictionary = __tbDictionary(rect, 'DOMRectInit');
      const x = __tbDouble(dictionary, 'x', 0);
      const y = __tbDouble(dictionary, 'y', 0);
      const width = __tbDouble(dictionary, 'width', 0);
      const height = __tbDouble(dictionary, 'height', 0);
      return new DOMQuad(
        { x, y },
        { x: x + width, y },
        { x: x + width, y: y + height },
        { x, y: y + height });
    }
    static fromQuad(quad) {
      const dictionary = __tbDictionary(quad, 'DOMQuadInit');
      return new DOMQuad(
        dictionary.p1, dictionary.p2, dictionary.p3, dictionary.p4);
    }
  }
  Object.defineProperty(DOMQuad.prototype, Symbol.toStringTag, {
    configurable: true, value: 'DOMQuad',
  });
  Object.defineProperty(DOMQuad, 'length', { configurable: true, value: 0 });

  // https://drafts.fxtf.org/geometry/#domrectlist
  class DOMRectList {
    constructor(rects) {
      Object.defineProperty(this, '_rects', {
        configurable: true, enumerable: false, writable: true, value: rects,
      });
      for (let index = 0; index < rects.length; index += 1) {
        Object.defineProperty(this, index, {
          configurable: true, enumerable: true, value: rects[index],
        });
      }
    }
    get length() { return this._rects.length; }
    item(index) {
      const position = Number(index) >>> 0;
      return position < this._rects.length ? this._rects[position] : null;
    }
    [Symbol.iterator]() { return this._rects[Symbol.iterator](); }
  }
  Object.defineProperty(DOMRectList.prototype, Symbol.toStringTag, {
    configurable: true, value: 'DOMRectList',
  });
  Object.defineProperty(DOMRectList, 'length', { configurable: true, value: 1 });

  // Element and Range already answer with plain rect objects; the interfaces
  // return the geometry classes those objects describe
  // (<https://drafts.csswg.org/cssom-view/#dom-element-getboundingclientrect>,
  // <https://drafts.csswg.org/cssom-view/#dom-element-getclientrects>).
  const __tbRectsOf = list => {
    const rects = [];
    for (let index = 0; index < list.length; index += 1) {
      const rect = list[index];
      rects.push(rect instanceof __tbDOMRect
        ? rect
        : new __tbDOMRect(rect.x, rect.y, rect.width, rect.height));
    }
    return rects;
  };
  const __tbWrapRectMethod = (prototype, name, list) => {
    const native = prototype[name];
    if (typeof native !== 'function') return;
    const wrapped = list
      ? function (...args) { return new DOMRectList(__tbRectsOf(native.apply(this, args))); }
      : function (...args) {
        const rect = native.apply(this, args);
        return new __tbDOMRect(rect.x, rect.y, rect.width, rect.height);
      };
    Object.defineProperty(wrapped, 'length', {
      configurable: true, value: native.length,
    });
    Object.defineProperty(prototype, name, {
      configurable: true, enumerable: true, writable: true, value: wrapped,
    });
  };
  if (globalThis.Element) {
    __tbWrapRectMethod(Element.prototype, 'getBoundingClientRect', false);
    __tbWrapRectMethod(Element.prototype, 'getClientRects', true);
  }
  if (globalThis.Range) {
    __tbWrapRectMethod(Range.prototype, 'getBoundingClientRect', false);
    __tbWrapRectMethod(Range.prototype, 'getClientRects', true);
  }

  const __tbPixels = value => {
    const number = Number.parseFloat(value);
    return Number.isFinite(number) ? number : 0;
  };
  // The used margin, border, and padding edges of one element, in CSS pixels.
  const __tbEdges = node => {
    const style = getComputedStyle(node);
    return {
      marginTop: __tbPixels(style.marginTop),
      marginRight: __tbPixels(style.marginRight),
      marginBottom: __tbPixels(style.marginBottom),
      marginLeft: __tbPixels(style.marginLeft),
      borderTop: __tbPixels(style.borderTopWidth),
      borderRight: __tbPixels(style.borderRightWidth),
      borderBottom: __tbPixels(style.borderBottomWidth),
      borderLeft: __tbPixels(style.borderLeftWidth),
      paddingTop: __tbPixels(style.paddingTop),
      paddingRight: __tbPixels(style.paddingRight),
      paddingBottom: __tbPixels(style.paddingBottom),
      paddingLeft: __tbPixels(style.paddingLeft),
    };
  };
  const __tbBoxType = (value, what) => {
    const box = value === undefined ? 'border' : String(value);
    if (box !== 'margin' && box !== 'border' && box !== 'padding' && box !== 'content') {
      throw new TypeError(what + ' is not a box type');
    }
    return box;
  };
  // The offset from the border-box top-left corner to the requested box's
  // top-left corner (`adjust a point to/from the border box`,
  // <https://drafts.csswg.org/cssom-view/#the-geometryutils-interface>).
  const __tbBoxOffset = (node, box) => {
    if (!node || node.nodeType !== 1 || box === 'border') return { x: 0, y: 0 };
    const edges = __tbEdges(node);
    if (box === 'margin') return { x: -edges.marginLeft, y: -edges.marginTop };
    if (box === 'padding') return { x: edges.borderLeft, y: edges.borderTop };
    return {
      x: edges.borderLeft + edges.paddingLeft,
      y: edges.borderTop + edges.paddingTop,
    };
  };
  const __tbDocumentOf = node => {
    if (!node) throw new TypeError('not a GeometryNode');
    return node.nodeType === 9 ? node : node.ownerDocument;
  };
  const __tbDisplayed = node => {
    if (!node) return false;
    if (node.nodeType === 9) return true;
    if (node.nodeType === 1) return getComputedStyle(node).display !== 'none';
    if (node.nodeType === 3) {
      return node.parentElement ? __tbDisplayed(node.parentElement) : false;
    }
    return false;
  };
  // The rects this node generates in its document's viewport: one per box
  // fragment for an element, one per line box for a text node, and the layout
  // viewport for a document
  // (<https://drafts.csswg.org/cssom-view/#the-geometryutils-interface>).
  const __tbRectsFor = node => {
    if (node.nodeType === 9) {
      const root = node.documentElement;
      const width = root ? root.clientWidth : node.defaultView.innerWidth;
      const height = root ? root.clientHeight : node.defaultView.innerHeight;
      return [new __tbDOMRect(0, 0, width, height)];
    }
    if (!__tbDisplayed(node)) return [];
    if (node.nodeType === 3) {
      // A `Range` over the text node has one rect per line box. The renderer
      // has no range layout yet, so its parent's box is the fallback and the
      // limitation is recorded in `docs/limitations.md`.
      if (typeof Range.prototype.getClientRects !== 'function') {
        return node.parentElement ? __tbRectsFor(node.parentElement) : [];
      }
      const range = node.ownerDocument.createRange();
      range.selectNodeContents(node);
      return __tbRectsOf(range.getClientRects());
    }
    return __tbRectsOf(node.getClientRects());
  };
  // The offset of `document`'s viewport inside the top-level viewport. A
  // cross-origin frame's element is not readable, which is the point at which
  // the coordinate conversion is refused
  // (<https://drafts.csswg.org/cssom-view/#check-the-documents-for-coordinate-conversion>).
  const __tbViewportOffset = document => {
    const view = document.defaultView;
    if (!view || view.top === view) return { x: 0, y: 0 };
    const frame = view.frameElement;
    if (!frame) {
      throw new DOMException('The documents are not same origin', 'SecurityError');
    }
    const rect = frame.getBoundingClientRect();
    const edges = __tbEdges(frame);
    const parent = __tbViewportOffset(frame.ownerDocument);
    return {
      x: parent.x + rect.left + edges.borderLeft + edges.paddingLeft,
      y: parent.y + rect.top + edges.borderTop + edges.paddingTop,
    };
  };
  // The border-box top-left corner of `node` in the top-level viewport.
  const __tbOrigin = node => {
    const document = __tbDocumentOf(node);
    const offset = __tbViewportOffset(document);
    if (node.nodeType === 9) return offset;
    const rects = __tbRectsFor(node);
    if (rects.length === 0) return null;
    return { x: offset.x + rects[0].left, y: offset.y + rects[0].top };
  };
  // The rect of one box fragment, in that fragment's local coordinate space
  // with its border edge at (0, 0)
  // (<https://drafts.csswg.org/cssom-view/#the-geometryutils-interface>).
  const __tbLocalBox = (node, box, rect, index, count) => {
    if (node.nodeType !== 1 || box === 'border') {
      return { x: 0, y: 0, width: rect.width, height: rect.height };
    }
    const edges = __tbEdges(node);
    const first = index === 0;
    const last = index === count - 1;
    if (box === 'margin') {
      const left = first ? edges.marginLeft : 0;
      const right = last ? edges.marginRight : 0;
      return {
        x: -left,
        y: -edges.marginTop,
        width: rect.width + left + right,
        height: rect.height + edges.marginTop + edges.marginBottom,
      };
    }
    if (box === 'padding') {
      const left = first ? edges.borderLeft : 0;
      const right = last ? edges.borderRight : 0;
      return {
        x: left,
        y: edges.borderTop,
        width: rect.width - left - right,
        height: rect.height - edges.borderTop - edges.borderBottom,
      };
    }
    const left = first ? edges.borderLeft + edges.paddingLeft : 0;
    const right = last ? edges.borderRight + edges.paddingRight : 0;
    return {
      x: left,
      y: edges.borderTop + edges.paddingTop,
      width: rect.width - left - right,
      height: rect.height - edges.borderTop - edges.borderBottom
        - edges.paddingTop - edges.paddingBottom,
    };
  };

  // https://drafts.csswg.org/cssom-view/#dom-geometryutils-convertpointfromnode
  //
  // Each call reads layout once for the source and once for the target, so a
  // four-point conversion shares the two origins instead of re-reading layout
  // per point.
  const __tbOrigins = (target, from, fromRects) => {
    const thisDocument = __tbDocumentOf(target);
    const fromDocument = __tbDocumentOf(from);
    if (thisDocument !== fromDocument) {
      const thisOrigin = thisDocument.location && thisDocument.location.origin;
      const fromOrigin = fromDocument.location && fromDocument.location.origin;
      if (thisOrigin === undefined || fromOrigin === undefined || thisOrigin !== fromOrigin) {
        throw new DOMException('The documents are not same origin', 'SecurityError');
      }
    }
    const thisOrigin = __tbOrigin(target);
    let fromOrigin;
    if (fromRects !== undefined && fromRects.length !== 0) {
      const offset = __tbViewportOffset(fromDocument);
      fromOrigin = { x: offset.x + fromRects[0].left, y: offset.y + fromRects[0].top };
    } else {
      fromOrigin = __tbOrigin(from);
    }
    if (thisOrigin === null || fromOrigin === null) {
      throw new DOMException('The node generates no box', 'NotFoundError');
    }
    return { thisOrigin, fromOrigin };
  };
  const __tbPointIn = (origins, point, target, from, fromBox, toBox) => {
    const fromOffset = __tbBoxOffset(from, fromBox);
    const toOffset = __tbBoxOffset(target, toBox);
    return new __tbDOMPoint(
      origins.fromOrigin.x + point.x + fromOffset.x - origins.thisOrigin.x - toOffset.x,
      origins.fromOrigin.y + point.y + fromOffset.y - origins.thisOrigin.y - toOffset.y);
  };
  const __tbOptions = (options, what) => __tbDictionary(options, what);
  // https://drafts.csswg.org/cssom-view/#dom-geometryutils-convertquadfromnode
  const __tbConvertQuad = (target, quad, from, options) => {
    const dictionary = __tbOptions(options, 'ConvertCoordinateOptions');
    const fromBox = __tbBoxType(dictionary.fromBox, 'fromBox');
    const toBox = __tbBoxType(dictionary.toBox, 'toBox');
    const origins = __tbOrigins(target, from);
    const source = DOMQuad.fromQuad(quad);
    return new DOMQuad(
      __tbPointIn(origins, source.p1, target, from, fromBox, toBox),
      __tbPointIn(origins, source.p2, target, from, fromBox, toBox),
      __tbPointIn(origins, source.p3, target, from, fromBox, toBox),
      __tbPointIn(origins, source.p4, target, from, fromBox, toBox));
  };

  // https://drafts.csswg.org/cssom-view/#dom-geometryutils-getboxquads
  const __tbGetBoxQuads = (node, options) => {
    const dictionary = __tbOptions(options, 'BoxQuadOptions');
    const document = __tbDocumentOf(node);
    const relativeTo = dictionary.relativeTo === undefined ? document : dictionary.relativeTo;
    const box = __tbBoxType(dictionary.box, 'box');
    const rects = __tbRectsFor(node);
    if (rects.length === 0) return [];
    const origins = __tbOrigins(relativeTo, node, rects);
    // Every fragment is expressed in the node's font-box coordinate space,
    // whose origin is the first fragment's border edge; the conversion below
    // moves the whole sequence into `relativeTo`'s space.
    const base = { x: rects[0].left, y: rects[0].top };
    const quads = [];
    for (let index = 0; index < rects.length; index += 1) {
      const rect = rects[index];
      const local = __tbLocalBox(node, box, rect, index, rects.length);
      const left = origins.fromOrigin.x + rect.left - base.x + local.x
        - origins.thisOrigin.x;
      const top = origins.fromOrigin.y + rect.top - base.y + local.y
        - origins.thisOrigin.y;
      quads.push(new DOMQuad(
        { x: left, y: top },
        { x: left + local.width, y: top },
        { x: left + local.width, y: top + local.height },
        { x: left, y: top + local.height }));
    }
    return quads;
  };

  // https://drafts.csswg.org/cssom-view/#dom-geometryutils-convertquadfromnode
  const __tbInstallGeometry = prototype => {
    if (!prototype) return;
    Object.defineProperty(prototype, 'getBoxQuads', {
      configurable: true, enumerable: true, writable: true,
      value: function getBoxQuads(options) { return __tbGetBoxQuads(this, options); },
    });
    Object.defineProperty(prototype, 'convertQuadFromNode', {
      configurable: true, enumerable: true, writable: true,
      value: function convertQuadFromNode(quad, from, options) {
        return __tbConvertQuad(this, quad, from, options);
      },
    });
    Object.defineProperty(prototype, 'convertRectFromNode', {
      configurable: true, enumerable: true, writable: true,
      value: function convertRectFromNode(rect, from, options) {
        return __tbConvertQuad(this, DOMQuad.fromRect(rect), from, options);
      },
    });
    Object.defineProperty(prototype, 'convertPointFromNode', {
      configurable: true, enumerable: true, writable: true,
      value: function convertPointFromNode(point, from, options) {
        const dictionary = __tbOptions(options, 'ConvertCoordinateOptions');
        const fromBox = __tbBoxType(dictionary.fromBox, 'fromBox');
        const toBox = __tbBoxType(dictionary.toBox, 'toBox');
        const origins = __tbOrigins(this, from);
        return __tbPointIn(
          origins, __tbPointInit(point), this, from, fromBox, toBox);
      },
    });
  };
  __tbInstallGeometry(globalThis.Element && Element.prototype);
  __tbInstallGeometry(globalThis.Text && Text.prototype);
  __tbInstallGeometry(globalThis.Document && Document.prototype);

  for (const [name, value] of [['DOMQuad', DOMQuad], ['DOMRectList', DOMRectList]]) {
    Object.defineProperty(globalThis, name, {
      configurable: true, enumerable: false, writable: true, value,
    });
  }
}
