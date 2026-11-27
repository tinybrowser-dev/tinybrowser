// https://w3c.github.io/IntersectionObserver/#intersection-observer-interface
{
  const observers = host.slots('IntersectionObserver');
  const entrySlots = host.slots('IntersectionObserverEntry');
  const requestFrame = globalThis.requestAnimationFrame;
  const computedStyle = globalThis.getComputedStyle;
  const DOMExceptionCtor = globalThis.DOMException;
  const create = Object.create;
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
  const clock = () => (globalThis.performance ? globalThis.performance.now() : Date.now());

  const liveObservers = PrivateSet();
  let frameHandle = null;

  const lengthPx = value => {
    const number = parseFloatValue(value);
    return number === number ? number : 0;
  };
  const borderBox = element => {
    const rect = element.getBoundingClientRect();
    return { left: rect.left, top: rect.top, right: rect.right, bottom: rect.bottom };
  };
  // https://drafts.csswg.org/css-overflow-3/#scrollport
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
  // https://drafts.csswg.org/css-overflow-3/#overflow-properties
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

  // https://w3c.github.io/IntersectionObserver/#root-intersection-rectangle
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
  // https://w3c.github.io/IntersectionObserver/#update-intersection-observations-algo
  const inContainingBlockChain = (target, root) => {
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
  // https://w3c.github.io/IntersectionObserver/#compute-the-intersection
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
      if (data.root !== null && !inContainingBlockChain(target, data.root)) continue;
      const hasBox = typeof target.getClientRects === 'function'
        ? target.getClientRects().length > 0
        : true;
      const targetRect = borderBox(target);
      // https://w3c.github.io/IntersectionObserver/#update-intersection-observations-algo
      const isIntersecting = hasBox
        && targetRect.left <= rootBounds.right
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
        // https://html.spec.whatwg.org/multipage/webappapis.html#report-the-exception
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

// https://drafts.csswg.org/resize-observer/#resize-observer-interface
{
  const observers = host.slots('ResizeObserver');
  const entrySlots = host.slots('ResizeObserverEntry');
  const sizeSlots = host.slots('ResizeObserverSize');
  const requestFrame = globalThis.requestAnimationFrame;
  const computedStyle = globalThis.getComputedStyle;
  const ErrorEventCtor = globalThis.ErrorEvent;
  const dispatchEvent = globalThis.EventTarget.prototype.dispatchEvent;
  const create = Object.create;
  const freeze = __tbFreeze;
  const define = __tbDefineProperty;
  const apply = __tbApply;
  const PrivateSet = __tbPrivateSet;
  const parseFloatValue = globalThis.parseFloat;
  const max = Math.max;

  const live = PrivateSet();
  let frameHandle = null;
  const IDLE_FRAMES = 20;
  let idleFrames = 0;
  // https://drafts.csswg.org/resize-observer/#calculate-depth-for-node
  const MAX_DEPTH = 5;

  const lengthPx = value => {
    const number = parseFloatValue(value);
    return number === number ? number : 0;
  };
  const isVertical = style =>
    style.writingMode === 'vertical-rl' || style.writingMode === 'vertical-lr'
    || style.writingMode === 'sideways-rl' || style.writingMode === 'sideways-lr';
  // https://drafts.csswg.org/resize-observer/#resize-observer-entry-interface
  const measure = element => {
    const rect = element.getBoundingClientRect();
    const style = computedStyle(element);
    const borderWidth = rect.right - rect.left;
    const borderHeight = rect.bottom - rect.top;
    const left = lengthPx(style.paddingLeft);
    const top = lengthPx(style.paddingTop);
    const contentWidth = max(0, borderWidth
      - lengthPx(style.borderLeftWidth) - lengthPx(style.borderRightWidth)
      - lengthPx(style.paddingLeft) - lengthPx(style.paddingRight));
    const contentHeight = max(0, borderHeight
      - lengthPx(style.borderTopWidth) - lengthPx(style.borderBottomWidth)
      - lengthPx(style.paddingTop) - lengthPx(style.paddingBottom));
    const vertical = isVertical(style);
    return {
      borderWidth,
      borderHeight,
      contentWidth,
      contentHeight,
      contentX: rect.left + lengthPx(style.borderLeftWidth) + lengthPx(style.paddingLeft),
      contentY: rect.top + lengthPx(style.borderTopWidth) + lengthPx(style.paddingTop),
      contentLeft: left,
      contentTop: top,
      vertical,
    };
  };
  // https://drafts.csswg.org/css-box-4/#snap
  const snappedSize = (start, size, ratio) =>
    Math.round((start + size) * ratio) - Math.round(start * ratio);
  const boxSize = (measured, box) => {
    if (box === 'border-box') {
      return {
        inline: measured.vertical ? measured.borderHeight : measured.borderWidth,
        block: measured.vertical ? measured.borderWidth : measured.borderHeight,
      };
    }
    if (box === 'device-pixel-content-box') {
      const ratio = globalThis.devicePixelRatio || 1;
      const inline = snappedSize(
        measured.vertical ? measured.contentY : measured.contentX,
        measured.vertical ? measured.contentHeight : measured.contentWidth,
        ratio,
      );
      const block = snappedSize(
        measured.vertical ? measured.contentX : measured.contentY,
        measured.vertical ? measured.contentWidth : measured.contentHeight,
        ratio,
      );
      return { inline, block };
    }
    return {
      inline: measured.vertical ? measured.contentHeight : measured.contentWidth,
      block: measured.vertical ? measured.contentWidth : measured.contentHeight,
    };
  };

  const rectPrototype = {
    toJSON() {
      return {
        x: this.x, y: this.y, width: this.width, height: this.height,
        top: this.top, right: this.right, bottom: this.bottom, left: this.left,
      };
    },
  };
  const makeRect = (x, y, width, height) => {
    const value = create(rectPrototype);
    value.x = x;
    value.y = y;
    value.width = width;
    value.height = height;
    value.left = x;
    value.top = y;
    value.right = x + width;
    value.bottom = y + height;
    return value;
  };

  function ResizeObserverSize() {
    throw new TypeError('Illegal constructor');
  }
  const sizePrototype = {
    get inlineSize() { return sizeSlots.get(this).inlineSize; },
    get blockSize() { return sizeSlots.get(this).blockSize; },
  };
  ResizeObserverSize.prototype = sizePrototype;
  define(sizePrototype, Symbol.toStringTag, {
    value: 'ResizeObserverSize', writable: false, enumerable: false, configurable: true,
  });
  define(globalThis, 'ResizeObserverSize', {
    value: ResizeObserverSize, writable: true, configurable: true,
  });
  const makeSize = (inline, block) => {
    const value = create(sizePrototype);
    sizeSlots.set(value, { inlineSize: inline, blockSize: block });
    return value;
  };

  function ResizeObserverEntry() {
    throw new TypeError('Illegal constructor');
  }
  const entryPrototype = {
    get target() { return entrySlots.get(this).target; },
    get contentRect() { return entrySlots.get(this).contentRect; },
    get contentBoxSize() { return entrySlots.get(this).contentBoxSize; },
    get borderBoxSize() { return entrySlots.get(this).borderBoxSize; },
    get devicePixelContentBoxSize() { return entrySlots.get(this).devicePixelContentBoxSize; },
  };
  ResizeObserverEntry.prototype = entryPrototype;
  define(entryPrototype, Symbol.toStringTag, {
    value: 'ResizeObserverEntry', writable: false, enumerable: false, configurable: true,
  });
  define(globalThis, 'ResizeObserverEntry', {
    value: ResizeObserverEntry, writable: true, configurable: true,
  });
  const makeEntry = (target, measured) => {
    const content = boxSize(measured, 'content-box');
    const border = boxSize(measured, 'border-box');
    const device = boxSize(measured, 'device-pixel-content-box');
    const value = create(entryPrototype);
    entrySlots.set(value, {
      target,
      contentRect: makeRect(measured.contentLeft, measured.contentTop,
                            measured.contentWidth, measured.contentHeight),
      contentBoxSize: freeze([makeSize(content.inline, content.block)]),
      borderBoxSize: freeze([makeSize(border.inline, border.block)]),
      devicePixelContentBoxSize: freeze([makeSize(device.inline, device.block)]),
    });
    return value;
  };

  const brand = observer => {
    const data = observers.get(observer);
    if (!data) throw new TypeError('Illegal invocation');
    return data;
  };
  const isElement = value => value !== null && typeof value === 'object'
    && value.nodeType === 1;

  const deliverOnce = () => {
    let delivered = false;
    for (const observer of live.values()) {
      const data = observers.get(observer);
      if (!data || data.registrations.length === 0) continue;
      const entries = [];
      for (const registration of data.registrations) {
        const measured = measure(registration.target);
        const size = boxSize(measured, registration.box);
        if (registration.delivered
            && size.inline === registration.inline
            && size.block === registration.block) {
          continue;
        }
        registration.delivered = true;
        registration.inline = size.inline;
        registration.block = size.block;
        entries.push(makeEntry(registration.target, measured));
      }
      if (entries.length === 0) continue;
      delivered = true;
      try {
        apply(data.callback, observer, [entries, observer]);
      } catch (error) {
        // https://html.spec.whatwg.org/multipage/webappapis.html#report-the-exception
        globalThis.setTimeout(() => { throw error; }, 0);
      }
    }
    return delivered;
  };
  const hasPending = () => {
    for (const observer of live.values()) {
      const data = observers.get(observer);
      if (!data) continue;
      for (const registration of data.registrations) {
        if (!registration.delivered) return true;
        const size = boxSize(measure(registration.target), registration.box);
        if (size.inline !== registration.inline || size.block !== registration.block) {
          return true;
        }
      }
    }
    return false;
  };
  const tick = () => {
    frameHandle = null;
    let any = false;
    for (let depth = 0; depth < MAX_DEPTH; depth++) {
      if (!deliverOnce()) break;
      any = true;
    }
    idleFrames = any ? 0 : idleFrames + 1;
    if (any && hasPending()) {
      // https://drafts.csswg.org/resize-observer/#deliver-resize-loop-error
      if (typeof ErrorEventCtor === 'function') {
        const event = new ErrorEventCtor('error', {
          message: 'ResizeObserver loop completed with undelivered notifications.',
        });
        apply(dispatchEvent, globalThis, [event]);
      }
    }
    if (idleFrames < IDLE_FRAMES) {
      for (const observer of live.values()) {
        const data = observers.get(observer);
        if (data && data.registrations.length > 0) {
          frameHandle = requestFrame(tick);
          return;
        }
      }
    }
  };
  const ensureFrame = () => {
    if (frameHandle === null) frameHandle = requestFrame(tick);
  };

  function ResizeObserver(callback) {
    if (!(this instanceof ResizeObserver)) throw new TypeError('Illegal constructor');
    if (typeof callback !== 'function') throw new TypeError('callback must be callable');
    observers.set(this, { callback, registrations: [] });
    live.add(this);
  }
  const prototype = ResizeObserver.prototype;
  define(prototype, 'observe', {
    value(target, options) {
      const data = brand(this);
      if (!isElement(target)) throw new TypeError('target must be an Element');
      let box = 'content-box';
      if (options !== undefined && options !== null) {
        const init = Object(options);
        if (init.box !== undefined) box = String(init.box);
      }
      if (box !== 'content-box' && box !== 'border-box'
          && box !== 'device-pixel-content-box') {
        throw new TypeError('box must be "content-box", "border-box", or "device-pixel-content-box"');
      }
      for (const registration of data.registrations) {
        if (registration.target === target) {
          registration.box = box;
          return;
        }
      }
      data.registrations.push({
        target, box, delivered: false, inline: 0, block: 0,
      });
      idleFrames = 0;
      ensureFrame();
    },
    writable: true, enumerable: true, configurable: true,
  });
  define(prototype, 'unobserve', {
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
  define(prototype, 'disconnect', {
    value() {
      brand(this).registrations.length = 0;
    },
    writable: true, enumerable: true, configurable: true,
  });
  define(prototype, 'constructor', {
    value: ResizeObserver, writable: true, configurable: true,
  });
  define(prototype, Symbol.toStringTag, {
    value: 'ResizeObserver', writable: false, enumerable: false, configurable: true,
  });
  define(globalThis, 'ResizeObserver', {
    value: ResizeObserver, writable: true, configurable: true,
  });
}
