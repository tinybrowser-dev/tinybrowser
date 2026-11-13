// Visibility observation against the viewport (implicit root) or an element
// root, with per-frame delivery
// <https://w3c.github.io/IntersectionObserver/#intersection-observer-interface>.
{
  const observers = host.slots('IntersectionObserver');
  const entrySlots = host.slots('IntersectionObserverEntry');
  // Captured at realm init so a page-patched builtin cannot retarget the
  // observer's own bookkeeping after it exists.
  const requestFrame = globalThis.requestAnimationFrame;
  const computedStyle = globalThis.getComputedStyle;
  const ElementCtor = globalThis.Element;
  const DOMExceptionCtor = globalThis.DOMException;
  const create = Object.create;
  const setPrototypeOf = __tbSetPrototypeOf;
  const freeze = __tbFreeze;
  const define = __tbDefineProperty;
  const apply = __tbApply;
  const PrivateSet = __tbPrivateSet;
  const min = Math.min;
  const max = Math.max;
  const parseFloatValue = globalThis.parseFloat;
  const toNumber = Number;
  const setTimer = globalThis.setTimeout;
  const toString = String;
  const toObject = Object;
  const arrayIsArray = Array.isArray;
  const clock = () => (globalThis.performance ? globalThis.performance.now() : Date.now());

  const liveObservers = PrivateSet();
  let frameHandle = null;

  // ── geometry ──────────────────────────────────────────────────────────
  const lengthPx = value => {
    const number = parseFloatValue(value);
    return number === number ? number : 0;
  };
  const borderBox = element => {
    const rect = element.getBoundingClientRect();
    return { left: rect.left, top: rect.top, right: rect.right, bottom: rect.bottom };
  };
  // The scrollport of a scroll container: its border box minus the borders,
  // which is the area its content is clipped to
  // (<https://drafts.csswg.org/css-overflow-3/#scrollport>).
  const paddingBox = element => {
    const style = computedStyle(element);
    const rect = element.getBoundingClientRect();
    return {
      left: rect.left + lengthPx(style.borderLeftWidth),
      top: rect.top + lengthPx(style.borderTopWidth),
      right: rect.right - lengthPx(style.borderRightWidth),
      bottom: rect.bottom - lengthPx(style.borderBottomWidth),
    };
  };
  // The clip rect of an ancestor: its padding box restricted to the axes whose
  // overflow clips. `overflow-x: visible` with `overflow-y: clip` leaves the
  // horizontal axis unclipped, so its bounds stay unbounded
  // (<https://drafts.csswg.org/css-overflow-3/#overflow-properties>).
  const clipRect = element => {
    const style = computedStyle(element);
    const clipX = style.overflowX !== 'visible';
    const clipY = style.overflowY !== 'visible';
    if (!clipX && !clipY && style.clipPath === 'none') return null;
    const box = paddingBox(element);
    if (!clipX) {
      box.left = -Infinity;
      box.right = Infinity;
    }
    if (!clipY) {
      box.top = -Infinity;
      box.bottom = Infinity;
    }
    return box;
  };
  const clipsContent = style =>
    style.overflowX !== 'visible' || style.overflowY !== 'visible' || style.clipPath !== 'none';
  const intersect = (a, b) => {
    const left = max(a.left, b.left);
    const top = max(a.top, b.top);
    const right = min(a.right, b.right);
    const bottom = min(a.bottom, b.bottom);
    if (left > right || top > bottom) {
      return { left: 0, top: 0, right: 0, bottom: 0 };
    }
    return { left, top, right, bottom };
  };
  const zeroRect = { left: 0, top: 0, right: 0, bottom: 0 };
  const makeRect = (rect, fallback) => {
    const box = rect ?? fallback ?? zeroRect;
    const value = create(rectPrototype);
    value.x = box.left;
    value.y = box.top;
    value.width = box.right - box.left;
    value.height = box.bottom - box.top;
    value.left = box.left;
    value.top = box.top;
    value.right = box.right;
    value.bottom = box.bottom;
    return value;
  };
  const rectPrototype = {
    toJSON() {
      return {
        x: this.x, y: this.y, width: this.width, height: this.height,
        top: this.top, right: this.right, bottom: this.bottom, left: this.left,
      };
    },
  };

  // ── root margin ───────────────────────────────────────────────────────
  const syntaxError = () => new DOMExceptionCtor(
    'The root margin must be one to four lengths or percentages', 'SyntaxError');
  const parseMargin = text => {
    const trimmed = text.trim();
    if (trimmed === '') return [{ value: 0, unit: 'px' }, { value: 0, unit: 'px' },
      { value: 0, unit: 'px' }, { value: 0, unit: 'px' }];
    const tokens = trimmed.split(/\s+/);
    if (tokens.length > 4) throw syntaxError();
    const sides = [];
    for (const token of tokens) {
      const match = /^(-?(?:\d+|\d*\.\d+))(px|%)$/.exec(token);
      if (!match) throw syntaxError();
      sides.push({ value: parseFloatValue(match[1]), unit: match[2] });
    }
    if (sides.length === 1) return [sides[0], sides[0], sides[0], sides[0]];
    if (sides.length === 2) return [sides[0], sides[1], sides[0], sides[1]];
    if (sides.length === 3) return [sides[0], sides[1], sides[2], sides[1]];
    return sides;
  };
  const serializeMargin = margin => margin
    .map(side => `${side.value}${side.unit}`)
    .join(' ');
  const resolveMargin = (side, basis) =>
    side.unit === '%' ? side.value * basis / 100 : side.value;

  // ── threshold ─────────────────────────────────────────────────────────
  const parseThresholds = value => {
    const items = [];
    if (value !== null && typeof value === 'object'
        && typeof value[Symbol.iterator] === 'function') {
      for (const item of value) items.push(item);
    } else {
      items.push(value);
    }
    const thresholds = [];
    for (const item of items) {
      const number = toNumber(item);
      // A non-numeric string converts to NaN, which Web IDL's `double`
      // conversion rejects with a TypeError.
      if (number !== number) throw new TypeError('threshold must be a number');
      if (number < 0 || number > 1) {
        throw new RangeError('threshold must be between 0 and 1');
      }
      thresholds.push(number);
    }
    thresholds.sort((a, b) => a - b);
    const unique = [];
    for (const threshold of thresholds) {
      if (unique.length === 0 || unique[unique.length - 1] !== threshold) {
        unique.push(threshold);
      }
    }
    return freeze(unique);
  };

  // ── intersection ──────────────────────────────────────────────────────
  // The root intersection rectangle: the viewport for an implicit root, the
  // padding area of a scrollable element root, and the border box otherwise,
  // expanded by the root margin
  // <https://w3c.github.io/IntersectionObserver/#root-intersection-rectangle>.
  const rootRectangle = data => {
    let rect;
    if (data.root === null || data.root.nodeType === 9) {
      const owner = data.root === null ? globalThis.document : data.root;
      const root = owner.documentElement;
      rect = { left: 0, top: 0, right: root.clientWidth, bottom: root.clientHeight };
    } else {
      const style = computedStyle(data.root);
      rect = clipsContent(style) ? paddingBox(data.root) : borderBox(data.root);
    }
    const width = rect.right - rect.left;
    const height = rect.bottom - rect.top;
    const [top, right, bottom, left] = data.margin;
    return {
      left: rect.left - resolveMargin(left, width),
      top: rect.top - resolveMargin(top, height),
      right: rect.right + resolveMargin(right, width),
      bottom: rect.bottom + resolveMargin(bottom, height),
    };
  };
  // Whether `root` is in `target`'s containing block chain. An absolutely
  // positioned target's chain starts at its offset parent, so a static element
  // root the target is merely nested in does not qualify
  // (<https://w3c.github.io/IntersectionObserver/#update-intersection-observations-algo>).
  const inContainingBlockChain = (target, root) => {
    // A Document root contains every target in its document.
    if (root.nodeType === 9) return target.ownerDocument === root;
    const position = computedStyle(target).position;
    let node;
    if (position === 'fixed') return false;
    node = position === 'absolute' ? target.offsetParent : target.parentElement;
    while (node) {
      if (node === root) return true;
      node = node.parentElement;
    }
    return false;
  };
  // Clip the target's border box by every ancestor that clips its content,
  // then by the root intersection rectangle
  // (<https://w3c.github.io/IntersectionObserver/#compute-the-intersection>).
  const computeIntersection = (target, root, rootBounds) => {
    let rect = borderBox(target);
    const stop = root !== null && root.nodeType === 1 ? root : null;
    let node = target.parentElement;
    while (node && node !== stop) {
      const clip = clipRect(node);
      if (clip !== null) rect = intersect(rect, clip);
      node = node.parentElement;
    }
    return intersect(rect, rootBounds);
  };

  // ── entries ───────────────────────────────────────────────────────────
  function IntersectionObserverEntry() {
    throw new TypeError('Illegal constructor');
  }
  const entryPrototype = {
    get boundingClientRect() { return entrySlots.get(this).boundingClientRect; },
    get intersectionRect() { return entrySlots.get(this).intersectionRect; },
    get rootBounds() { return entrySlots.get(this).rootBounds; },
    get isIntersecting() { return entrySlots.get(this).isIntersecting; },
    get isVisible() { return entrySlots.get(this).isVisible; },
    get intersectionRatio() { return entrySlots.get(this).intersectionRatio; },
    get target() { return entrySlots.get(this).target; },
    get time() { return entrySlots.get(this).time; },
    toJSON() {
      return {
        boundingClientRect: this.boundingClientRect.toJSON(),
        intersectionRect: this.intersectionRect.toJSON(),
        rootBounds: this.rootBounds === null ? null : this.rootBounds.toJSON(),
        isIntersecting: this.isIntersecting,
        isVisible: this.isVisible,
        intersectionRatio: this.intersectionRatio,
        target: this.target,
        time: this.time,
      };
    },
  };
  IntersectionObserverEntry.prototype = entryPrototype;
  define(entryPrototype, Symbol.toStringTag, {
    value: 'IntersectionObserverEntry', writable: false, enumerable: false, configurable: true,
  });
  define(globalThis, 'IntersectionObserverEntry', {
    value: IntersectionObserverEntry, writable: true, configurable: true,
  });

  const entry = (data, target, time, rootBounds, targetRect, intersectionRect,
                 isIntersecting, isVisible, ratio) => {
    const value = create(entryPrototype);
    entrySlots.set(value, {
      boundingClientRect: makeRect(targetRect),
      intersectionRect: makeRect(intersectionRect),
      rootBounds: rootBounds === null ? null : makeRect(rootBounds),
      isIntersecting,
      isVisible,
      intersectionRatio: ratio,
      target,
      time,
    });
    return value;
  };

  // ── observer ──────────────────────────────────────────────────────────
  // A wrapper from another realm fails `instanceof` here, so the node type is
  // the cross-realm brand (`nodeType` is 1 for an Element and 9 for a
  // Document).
  const isElement = value => value !== null && typeof value === 'object'
    && value.nodeType === 1;
  const isElementOrDocument = value => value !== null && typeof value === 'object'
    && (value.nodeType === 1 || value.nodeType === 9);

  const brand = observer => {
    const data = observers.get(observer);
    if (!data) throw new TypeError('Illegal invocation');
    return data;
  };

  const update = (observer, data, time) => {
    const rootBounds = rootRectangle(data);
    const entries = [];
    for (const registration of data.registrations) {
      const { target } = registration;
      if ((time - registration.lastUpdateTime) < data.delay) continue;
      registration.lastUpdateTime = time;
      // An explicit root's observations only reach targets it contains in the
      // containing block chain; other targets produce no entry at all
      // (`containing-block.html`).
      if (data.root !== null && !inContainingBlockChain(target, data.root)) continue;
      const targetRect = borderBox(target);
      // Edge-adjacent counts as intersecting even when the area is zero
      // (<https://w3c.github.io/IntersectionObserver/#update-intersection-observations-algo>).
      const isIntersecting = targetRect.left <= rootBounds.right
        && targetRect.right >= rootBounds.left
        && targetRect.top <= rootBounds.bottom
        && targetRect.bottom >= rootBounds.top;
      const intersectionRect = isIntersecting
        ? computeIntersection(target, data.root, rootBounds)
        : zeroRect;
      const targetArea = (targetRect.right - targetRect.left)
        * (targetRect.bottom - targetRect.top);
      const intersectionArea = max(0, intersectionRect.right - intersectionRect.left)
        * max(0, intersectionRect.bottom - intersectionRect.top);
      const ratio = targetArea > 0
        ? intersectionArea / targetArea
        : (isIntersecting ? 1 : 0);
      let thresholdIndex = 0;
      while (thresholdIndex < data.thresholds.length
          && data.thresholds[thresholdIndex] <= ratio) {
        thresholdIndex++;
      }
      const isVisible = typeof target.checkVisibility === 'function'
        ? target.checkVisibility()
        : target.getClientRects().length > 0;
      if (thresholdIndex === registration.previousThresholdIndex
          && isIntersecting === registration.previousIsIntersecting
          && isVisible === registration.previousIsVisible) {
        continue;
      }
      registration.previousThresholdIndex = thresholdIndex;
      registration.previousIsIntersecting = isIntersecting;
      registration.previousIsVisible = isVisible;
      entries.push(entry(data, target, time, rootBounds, targetRect, intersectionRect,
                         isIntersecting, isVisible, ratio));
    }
    return entries;
  };

  const tick = () => {
    frameHandle = null;
    const time = clock();
    let pending = false;
    for (const observer of liveObservers.values()) {
      const data = observers.get(observer);
      if (!data || data.registrations.length === 0) continue;
      pending = true;
      const entries = update(observer, data, time);
      if (entries.length === 0) continue;
      try {
        apply(data.callback, observer, [entries, observer]);
      } catch (error) {
        // A throwing callback is reported as an uncaught error instead of
        // breaking the observation loop
        // (<https://html.spec.whatwg.org/multipage/webappapis.html#report-the-exception>).
        setTimer(() => { throw error; }, 0);
      }
    }
    if (pending) frameHandle = requestFrame(tick);
  };
  const ensureFrame = () => {
    if (frameHandle === null) frameHandle = requestFrame(tick);
  };

  function IntersectionObserver(callback, options) {
    if (!(this instanceof IntersectionObserver)) {
      throw new TypeError('Illegal constructor');
    }
    if (typeof callback !== 'function') throw new TypeError('callback must be callable');
    const init = options === undefined || options === null ? {} : toObject(options);
    const root = init.root === undefined ? null : init.root;
    if (root !== null && !isElementOrDocument(root)) {
      throw new TypeError('root must be an Element or Document');
    }
    const margin = parseMargin(init.rootMargin === undefined ? '0px' : toString(init.rootMargin));
    const thresholds = parseThresholds(init.threshold === undefined ? 0 : init.threshold);
    const delay = init.delay === undefined ? 0 : Number(init.delay);
    observers.set(this, {
      callback,
      root,
      margin,
      thresholds,
      delay,
      rootMargin: serializeMargin(margin),
      registrations: [],
    });
    liveObservers.add(this);
  }
  const observerPrototype = IntersectionObserver.prototype;
  define(observerPrototype, 'root', {
    get() { return brand(this).root; },
    enumerable: true, configurable: true,
  });
  define(observerPrototype, 'rootMargin', {
    get() { return brand(this).rootMargin; },
    enumerable: true, configurable: true,
  });
  define(observerPrototype, 'thresholds', {
    get() { return brand(this).thresholds; },
    enumerable: true, configurable: true,
  });
  define(observerPrototype, 'observe', {
    value(target) {
      const data = brand(this);
      if (!isElement(target)) throw new TypeError('target must be an Element');
      for (const registration of data.registrations) {
        if (registration.target === target) return;
      }
      data.registrations.push({
        target,
        previousThresholdIndex: -1,
        previousIsIntersecting: false,
        previousIsVisible: false,
        lastUpdateTime: -Infinity,
      });
      ensureFrame();
    },
    writable: true, enumerable: true, configurable: true,
  });
  define(observerPrototype, 'unobserve', {
    value(target) {
      const data = brand(this);
      for (let index = 0; index < data.registrations.length; index++) {
        if (data.registrations[index].target === target) {
          data.registrations.splice(index, 1);
          return;
        }
      }
    },
    writable: true, enumerable: true, configurable: true,
  });
  define(observerPrototype, 'disconnect', {
    value() {
      brand(this).registrations.length = 0;
    },
    writable: true, enumerable: true, configurable: true,
  });
  define(observerPrototype, 'takeRecords', {
    value() {
      brand(this);
      // Entries are delivered in the frame that produced them, so nothing is
      // ever pending between frames.
      return [];
    },
    writable: true, enumerable: true, configurable: true,
  });
  define(observerPrototype, 'constructor', {
    value: IntersectionObserver, writable: true, configurable: true,
  });
  define(observerPrototype, Symbol.toStringTag, {
    value: 'IntersectionObserver', writable: false, enumerable: false, configurable: true,
  });

  define(globalThis, 'IntersectionObserver', {
    value: IntersectionObserver, writable: true, configurable: true,
  });
}
