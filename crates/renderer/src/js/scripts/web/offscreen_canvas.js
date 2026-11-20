// https://html.spec.whatwg.org/multipage/canvas.html#the-offscreencanvas-interface
(function () {
  if (typeof globalThis.OffscreenCanvas === 'function') {
    return;
  }

  const ids = new __tbPrivateMap();
  const states = new __tbPrivateMap();
  const contexts = new __tbPrivateMap();

  function toUnsignedLongLong(value) {
    const number = Number(value);
    if (!Number.isFinite(number)) {
      throw new TypeError('value is not a finite number');
    }
    const integer = Math.trunc(number);
    if (integer < 0 || integer > Number.MAX_SAFE_INTEGER) {
      throw new TypeError('value is outside the unsigned long long range');
    }
    return integer;
  }

  function stateOf(canvas) {
    const state = states.get(canvas);
    if (!state) {
      throw new TypeError('Illegal invocation');
    }
    return state;
  }

  function idOf(canvas) {
    const id = ids.get(canvas);
    if (id === undefined) {
      throw new TypeError('Illegal invocation');
    }
    return id;
  }

  const base = globalThis.CanvasRenderingContext2D;
  let offscreenPrototype = base && base.prototype;
  if (base) {
    function OffscreenCanvasRenderingContext2D() {}
    const prototype = Object.create(Object.prototype);
    for (const key of Object.getOwnPropertyNames(base.prototype)) {
      if (key === 'constructor' || key === 'canvas') {
        continue;
      }
      Object.defineProperty(
        prototype,
        key,
        Object.getOwnPropertyDescriptor(base.prototype, key),
      );
    }
    Object.defineProperty(prototype, 'canvas', {
      configurable: true,
      get() {
        const state = contexts.get(this);
        if (!state) {
          throw new TypeError('Illegal invocation');
        }
        return state.owner;
      },
    });
    Object.defineProperty(prototype, 'constructor', {
      configurable: true,
      writable: true,
      value: OffscreenCanvasRenderingContext2D,
    });
    Object.defineProperty(OffscreenCanvasRenderingContext2D, 'prototype', {
      value: prototype,
      writable: false,
      configurable: false,
    });
    globalThis.OffscreenCanvasRenderingContext2D = OffscreenCanvasRenderingContext2D;
    offscreenPrototype = prototype;
  }

  const validContexts = new Set(['2d', 'bitmaprenderer', 'webgl', 'webgl2', 'webgpu']);

  class OffscreenCanvas {
    constructor(width, height) {
      if (arguments.length < 2) {
        throw new TypeError('OffscreenCanvas needs width and height');
      }
      const id = host.__tbOffscreenCanvasNew(
        toUnsignedLongLong(width),
        toUnsignedLongLong(height),
      );
      ids.set(this, id);
      states.set(this, {
        context: null,
        detached: false,
      });
    }

    get width() { return host.__tbOffscreenCanvasWidth(idOf(this)); }
    set width(value) {
      host.__tbOffscreenCanvasSetWidth(idOf(this), toUnsignedLongLong(value));
    }

    get height() { return host.__tbOffscreenCanvasHeight(idOf(this)); }
    set height(value) {
      host.__tbOffscreenCanvasSetHeight(idOf(this), toUnsignedLongLong(value));
    }

    getContext(type, options) {
      const state = stateOf(this);
      if (state.detached) {
        throw new DOMException('the OffscreenCanvas is detached', 'InvalidStateError');
      }
      if (arguments.length === 0) {
        throw new TypeError('getContext needs a context type');
      }
      const contextId = String(type);
      if (!validContexts.has(contextId)) {
        throw new TypeError('unknown context type');
      }
      if (contextId !== '2d') {
        return null;
      }
      if (state.context) {
        return state.context;
      }
      const context = host.__tbOffscreenCanvasContext(idOf(this), options);
      if (context) {
        if (offscreenPrototype) {
          Object.setPrototypeOf(context, offscreenPrototype);
        }
        contexts.set(context, { owner: this });
        state.context = context;
      }
      return context;
    }

    transferToImageBitmap() {
      const state = stateOf(this);
      if (state.detached) {
        throw new DOMException('the OffscreenCanvas is detached', 'InvalidStateError');
      }
      if (!state.context) {
        throw new DOMException('the OffscreenCanvas has no 2D context', 'InvalidStateError');
      }
      return host.__tbOffscreenCanvasTransferToImageBitmap(idOf(this));
    }
  }

  Object.defineProperty(OffscreenCanvas.prototype, Symbol.toStringTag, {
    configurable: true,
    value: 'OffscreenCanvas',
  });
  globalThis.OffscreenCanvas = OffscreenCanvas;

  host.__tbOffscreenCanvasId = function (value) {
    const id = value && typeof value === 'object' ? ids.get(value) : undefined;
    return id === undefined ? -1 : id;
  };
  host.__tbOffscreenDetached = function (canvas) {
    const state = canvas && typeof canvas === 'object' ? states.get(canvas) : undefined;
    return Boolean(state && state.detached);
  };
  host.__tbOffscreenHasContext = function (canvas) {
    const state = canvas && typeof canvas === 'object' ? states.get(canvas) : undefined;
    return Boolean(state && state.context);
  };
  host.__tbOffscreenDetach = function (canvas) {
    const state = stateOf(canvas);
    const id = idOf(canvas);
    state.detached = true;
    state.context = null;
    host.__tbOffscreenCanvasDetach(id);
  };
})();
