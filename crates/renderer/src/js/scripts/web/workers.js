// Dedicated workers: the `Worker` constructor in a document realm, and the
// `DedicatedWorkerGlobalScope` a worker script runs in
// (<https://html.spec.whatwg.org/multipage/workers.html#dedicated-workers-and-the-dedicatedworkerglobalscope-interface>).
(function() {
  const __tbWorkerIsGlobal = host.__tbIsWorker === true;

  // Handler properties are plain writable values; the event dispatch reads
  // `on<type>` after the listener list, the same shape `XMLHttpRequest` uses
  // (<https://html.spec.whatwg.org/multipage/webappapis.html#event-handlers>).
  const __tbDefineWorkerHandler = (prototype, name) => {
    Object.defineProperty(prototype, name, {
      value: null,
      writable: true,
      enumerable: true,
      configurable: true,
    });
  };

  if (__tbWorkerIsGlobal) {
    // A worker global has no browsing context: remove the Window-shaped names
    // the messaging shim installs for documents.
    for (const name of ['window', 'localStorage', 'sessionStorage', 'frameElement', 'length', 'parent', 'top']) {
      try {
        delete globalThis[name];
      } catch (error) {}
    }

    const __tbWorkerLocationURLs = new WeakMap();
    class WorkerLocation {
      constructor(href) {
        __tbWorkerLocationURLs.set(this, new URL(href));
      }
      get href() { return __tbWorkerLocationURLs.get(this).href; }
      get origin() { return __tbWorkerLocationURLs.get(this).origin; }
      get protocol() { return __tbWorkerLocationURLs.get(this).protocol; }
      get host() { return __tbWorkerLocationURLs.get(this).host; }
      get hostname() { return __tbWorkerLocationURLs.get(this).hostname; }
      get port() { return __tbWorkerLocationURLs.get(this).port; }
      get pathname() { return __tbWorkerLocationURLs.get(this).pathname; }
      get search() { return __tbWorkerLocationURLs.get(this).search; }
      get hash() { return __tbWorkerLocationURLs.get(this).hash; }
      toString() { return __tbWorkerLocationURLs.get(this).href; }
    }
    Object.defineProperty(WorkerLocation.prototype, Symbol.toStringTag, {
      value: 'WorkerLocation', writable: false, enumerable: false, configurable: true,
    });

    class WorkerGlobalScope extends EventTarget {
      constructor() {
        throw new TypeError('Illegal constructor');
      }
      get self() { return globalThis; }
      get location() { return globalThis.location; }
      get navigator() { return globalThis.navigator; }
      close() { host.__tbWorkerClose(); }
    }

    class DedicatedWorkerGlobalScope extends WorkerGlobalScope {
      postMessage(message, transfer) {
        host.__tbWorkerPostToParent(__tbEncode(message, transfer).payload);
      }
    }
    __tbDefineWorkerHandler(DedicatedWorkerGlobalScope.prototype, 'onmessage');
    __tbDefineWorkerHandler(DedicatedWorkerGlobalScope.prototype, 'onmessageerror');
    __tbDefineWorkerHandler(DedicatedWorkerGlobalScope.prototype, 'onerror');
    Object.defineProperty(DedicatedWorkerGlobalScope.prototype, Symbol.toStringTag, {
      value: 'DedicatedWorkerGlobalScope', writable: false, enumerable: false, configurable: true,
    });

    Object.defineProperty(globalThis, 'WorkerGlobalScope', {
      value: WorkerGlobalScope, writable: true, configurable: true,
    });
    Object.defineProperty(globalThis, 'DedicatedWorkerGlobalScope', {
      value: DedicatedWorkerGlobalScope, writable: true, configurable: true,
    });
    Object.setPrototypeOf(globalThis, DedicatedWorkerGlobalScope.prototype);
    globalThis.self = globalThis;

    // `name` is a `[Replaceable]` attribute: an own accessor on the global
    // that, once assigned, becomes a writable data property
    // (<https://webidl.spec.whatwg.org/#idl-attributes>,
    // <https://html.spec.whatwg.org/multipage/workers.html#dom-dedicatedworkerglobalscope-name>).
    let workerName = host.__tbWorkerName;
    Object.defineProperty(globalThis, 'name', {
      get() { return workerName; },
      set(value) {
        workerName = String(value);
        Object.defineProperty(globalThis, 'name', {
          value: workerName,
          writable: true,
          enumerable: true,
          configurable: true,
        });
      },
      enumerable: true,
      configurable: true,
    });

    const location = new WorkerLocation(host.__tbWorkerURL);
    Object.defineProperty(globalThis, 'location', {
      value: location, writable: false, enumerable: true, configurable: false,
    });

    // `performance` is not a Window-only API; a worker gets the same clock
    // surface for its own realm
    // (<https://w3c.github.io/hr-time/#the-performance-interface>).
    if (globalThis.performance === undefined) {
      const origin = Date.now();
      Object.defineProperty(globalThis, 'performance', {
        value: {
          now() { return Date.now() - origin; },
          timeOrigin: origin,
        },
        writable: false,
        configurable: true,
      });
    }

    // The messaging shim installs a Window `close()`; a worker closes its own
    // global instead
    // (<https://html.spec.whatwg.org/multipage/workers.html#dom-dedicatedworkerglobalscope-close>).
    globalThis.close = function close() {
      host.__tbWorkerClose();
    };
    globalThis.postMessage = function(message, transfer) {
      host.__tbWorkerPostToParent(__tbEncode(message, transfer).payload);
    };
    globalThis.importScripts = function(...urls) {
      for (const value of urls) {
        const href = new URL(String(value), location.href).href;
        const body = host.__tbImportScripts(href);
        if (body === null) {
          throw new DOMException(`Failed to load ${href}`, 'NetworkError');
        }
        const source = new TextDecoder('utf-8').decode(new Uint8Array(body));
        (0, eval)(source);
      }
    };

    // The host calls this after a message from the creator arrived.
    globalThis.__tbWorkerDeliverMessage = function(payload) {
      globalThis.dispatchEvent(new MessageEvent('message', {
        data: __tbDecode(payload),
      }));
    };
    return;
  }

  const __tbWorkers = new Map();
  const __tbWorkerIds = new WeakMap();
  const __tbWorkerBase = () => {
    const document = globalThis.document;
    return document !== undefined && typeof document.baseURI === 'string'
      ? document.baseURI
      : globalThis.location.href;
  };

  class Worker extends EventTarget {
    constructor(url, options) {
      if (arguments.length < 1) {
        throw new TypeError("Failed to construct 'Worker': 1 argument required, but only 0 present.");
      }
      // Convert and parse before initializing the base class: a `toString`
      // that constructs another worker must recurse through JS alone, and a
      // conversion exception propagates unchanged
      // (<https://html.spec.whatwg.org/multipage/workers.html#dom-worker>).
      const href = String(url);
      let parsed;
      try {
        parsed = new URL(href, __tbWorkerBase());
      } catch (error) {
        throw new DOMException(`Invalid worker script URL: ${href}`, 'SyntaxError');
      }
      super();
      if ((parsed.protocol === 'http:' || parsed.protocol === 'https:')
          && parsed.origin !== globalThis.location.origin) {
        throw new DOMException(`Worker script must be same-origin: ${parsed.href}`, 'SecurityError');
      }
      const dictionary = options === undefined || options === null ? {} : Object(options);
      const type = dictionary.type === undefined ? 'classic' : String(dictionary.type);
      if (type !== 'classic' && type !== 'module') {
        throw new TypeError(`Worker type must be 'classic' or 'module': ${type}`);
      }
      const name = dictionary.name === undefined ? '' : String(dictionary.name);
      const id = host.__tbWorkerCreate(parsed.href, name, type === 'module');
      __tbWorkerIds.set(this, id);
      __tbWorkers.set(id, this);
    }
    postMessage(message, transfer) {
      host.__tbWorkerPost(__tbWorkerIds.get(this), __tbEncode(message, transfer).payload);
    }
    terminate() {
      const id = __tbWorkerIds.get(this);
      host.__tbWorkerTerminate(id);
      __tbWorkers.delete(id);
    }
  }
  __tbDefineWorkerHandler(Worker.prototype, 'onmessage');
  __tbDefineWorkerHandler(Worker.prototype, 'onmessageerror');
  __tbDefineWorkerHandler(Worker.prototype, 'onerror');
  Object.defineProperty(Worker.prototype, Symbol.toStringTag, {
    value: 'Worker', writable: false, enumerable: false, configurable: true,
  });
  Object.defineProperty(globalThis, 'Worker', {
    value: Worker, writable: true, configurable: true,
  });

  globalThis.__tbWorkerEvent = function(id, payload) {
    const worker = __tbWorkers.get(id);
    if (worker === undefined) return;
    worker.dispatchEvent(new MessageEvent('message', {
      data: __tbDecode(payload),
    }));
  };
  globalThis.__tbWorkerErrorEvent = function(id, message, filename, lineno, colno) {
    const worker = __tbWorkers.get(id);
    if (worker === undefined) return;
    worker.dispatchEvent(new ErrorEvent('error', {
      message,
      filename,
      lineno,
      colno,
      cancelable: true,
    }));
  };
})();
