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
    for (const name of ['window', 'localStorage', 'sessionStorage', 'frameElement', 'length', 'parent', 'top', 'open', 'opener']) {
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
      importScripts(...urls) {
        // A module worker's global has no classic-script import: the operation
        // throws, and the worker keeps running
        // (<https://html.spec.whatwg.org/multipage/workers.html#dom-workerglobalscope-importscripts>).
        if (host.__tbWorkerModule) {
          throw new TypeError("Failed to execute 'importScripts' on 'WorkerGlobalScope': Module scripts don't support importScripts().");
        }
        for (const value of urls) {
          const href = new URL(String(value), location.href).href;
          // The host reports a failed fetch as `undefined`; an empty script is
          // the empty string, which still runs
          // (<https://html.spec.whatwg.org/multipage/webappapis.html#fetch-a-classic-worker-imported-script>).
          const source = host.__tbImportScripts(href);
          if (source === undefined) {
            throw new DOMException(`Failed to load ${href}`, 'NetworkError');
          }
          host.__tbEvalImportedScript(href, source);
        }
      }
    }
    // `onerror` is a `WorkerGlobalScope` handler, and the realm's own global is
    // an `OnErrorEventHandler`: the five-argument call is decided by the
    // event's target, not by this slot
    // (<https://html.spec.whatwg.org/multipage/webappapis.html#event-handlers>).
    __tbDefineWorkerHandler(WorkerGlobalScope.prototype, 'onerror');
    __tbDefineWorkerHandler(WorkerGlobalScope.prototype, 'onlanguagechange');
    __tbDefineWorkerHandler(WorkerGlobalScope.prototype, 'onoffline');
    __tbDefineWorkerHandler(WorkerGlobalScope.prototype, 'ononline');
    __tbDefineWorkerHandler(WorkerGlobalScope.prototype, 'onrejectionhandled');
    __tbDefineWorkerHandler(WorkerGlobalScope.prototype, 'onunhandledrejection');
    Object.defineProperty(WorkerGlobalScope.prototype, Symbol.toStringTag, {
      value: 'WorkerGlobalScope', writable: false, enumerable: false, configurable: true,
    });

    class DedicatedWorkerGlobalScope extends WorkerGlobalScope {
      postMessage(message, transfer) {
        if (arguments.length < 1) {
          throw new TypeError("Failed to execute 'postMessage' on 'DedicatedWorkerGlobalScope': 1 argument required, but only 0 present.");
        }
        const encoded = __tbEncode(message, __tbPostMessageTransfer(transfer));
        host.__tbWorkerPostToParent(encoded.payload, encoded.ports);
      }
      // `close()` belongs to the dedicated global, not to the common
      // `WorkerGlobalScope`
      // (<https://html.spec.whatwg.org/multipage/workers.html#dom-dedicatedworkerglobalscope-close>).
      close() {
        host.__tbWorkerClose();
      }
    }
    __tbDefineWorkerHandler(DedicatedWorkerGlobalScope.prototype, 'onmessage');
    __tbDefineWorkerHandler(DedicatedWorkerGlobalScope.prototype, 'onmessageerror');
    Object.defineProperty(DedicatedWorkerGlobalScope.prototype, Symbol.toStringTag, {
      value: 'DedicatedWorkerGlobalScope', writable: false, enumerable: false, configurable: true,
    });

    // A `SharedWorker` global receives connections instead of window messages:
    // its only entry points are `onconnect` and the ports each event carries
    // (<https://html.spec.whatwg.org/multipage/workers.html#shared-workers-and-the-sharedworkerglobalscope-interface>).
    class SharedWorkerGlobalScope extends WorkerGlobalScope {
      close() {
        host.__tbWorkerClose();
      }
    }
    __tbDefineWorkerHandler(SharedWorkerGlobalScope.prototype, 'onconnect');
    Object.defineProperty(SharedWorkerGlobalScope.prototype, Symbol.toStringTag, {
      value: 'SharedWorkerGlobalScope', writable: false, enumerable: false, configurable: true,
    });

    Object.defineProperty(globalThis, 'WorkerGlobalScope', {
      value: WorkerGlobalScope, writable: true, configurable: true,
    });
    if (host.__tbIsSharedWorker === true) {
      Object.defineProperty(globalThis, 'SharedWorkerGlobalScope', {
        value: SharedWorkerGlobalScope, writable: true, configurable: true,
      });
      Object.setPrototypeOf(globalThis, SharedWorkerGlobalScope.prototype);
    } else {
      Object.defineProperty(globalThis, 'DedicatedWorkerGlobalScope', {
        value: DedicatedWorkerGlobalScope, writable: true, configurable: true,
      });
      Object.setPrototypeOf(globalThis, DedicatedWorkerGlobalScope.prototype);
    }
    globalThis.self = globalThis;

    // The Window shim in the messaging script installs these as own properties
    // of every global. A worker's are the `WorkerGlobalScope` operations on
    // the prototype chain, so drop the Window-shaped own ones.
    delete globalThis.close;
    delete globalThis.postMessage;

    if (host.__tbIsSharedWorker === true) {
      // The host calls this when the engine adopts a `connect`: the event's
      // port is the worker side of the new connection
      // (<https://html.spec.whatwg.org/multipage/workers.html#dom-sharedworkerglobalscope-onconnect>).
      globalThis.__tbWorkerConnect = function(endpoint) {
        const materialized = __tbMaterializePorts([endpoint]);
        const port = materialized.ports[endpoint];
        globalThis.dispatchEvent(new MessageEvent('connect', {
          data: '',
          source: port,
          ports: Object.freeze([port]),
        }));
        __tbFirePortCloses(materialized.closes);
      };
    }

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

    // The host calls this after a message from the creator arrived.
    globalThis.__tbWorkerDeliverMessage = function(payload, portIds) {
      const materialized = __tbMaterializePorts(portIds);
      let data;
      try {
        data = __tbDecode(payload, materialized.ports);
      } catch (error) {
        // The ports arrived with a message that cannot be decoded; they are
        // lost with it
        // (<https://html.spec.whatwg.org/multipage/web-messaging.html#transfer-receiving-steps>).
        __tbDiscardPorts(portIds);
        return;
      }
      const ports = __tbArray.map(portIds, id => materialized.ports[id]);
      globalThis.dispatchEvent(new MessageEvent('message', {
        data,
        ports: Object.freeze(ports),
      }));
      __tbFirePortCloses(materialized.closes);
    };
  }

  // `Worker` is exposed on both globals, so a worker can start a nested
  // worker; the base URL then resolves against the worker script's URL
  // (<https://html.spec.whatwg.org/multipage/workers.html#dom-worker>).
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
      // An off-origin script is not a constructor error: the fetch fails and
      // the `Worker` object reports a plain `error` event, the same shape as
      // any other script that cannot load
      // (<https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model>).
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
      if (arguments.length < 1) {
        throw new TypeError("Failed to execute 'postMessage' on 'Worker': 1 argument required, but only 0 present.");
      }
      const encoded = __tbEncode(message, __tbPostMessageTransfer(transfer));
      host.__tbWorkerPost(__tbWorkerIds.get(this), encoded.payload, encoded.ports);
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

  // `SharedWorker` is Window-only; a worker global has no shared worker
  // constructor
  // (<https://html.spec.whatwg.org/multipage/workers.html#shared-workers-and-the-sharedworkerglobalscope-interface>).
  if (!__tbWorkerIsGlobal) {
    const __tbSharedWorkerPorts = new WeakMap();
    class SharedWorker extends EventTarget {
      constructor(url, options) {
        if (arguments.length < 1) {
          throw new TypeError("Failed to construct 'SharedWorker': 1 argument required, but only 0 present.");
        }
        // Convert and parse before initializing the base class: a `toString`
        // that constructs another worker must recurse through JS alone, and a
        // conversion exception propagates unchanged
        // (<https://html.spec.whatwg.org/multipage/workers.html#dom-sharedworker>).
        const href = String(url);
        let parsed;
        try {
          parsed = new URL(href, __tbWorkerBase());
        } catch (error) {
          throw new DOMException(`Invalid shared worker script URL: ${href}`, 'SyntaxError');
        }
        super();
        // `options` is `(DOMString or WorkerOptions)`. `undefined` takes the
        // default empty dictionary and `null` converts to an empty dictionary;
        // every other primitive is the worker name
        // (<https://webidl.spec.whatwg.org/#idl-union>).
        let dictionary;
        let name;
        if (options === undefined || options === null || typeof options === 'object') {
          dictionary = options === undefined || options === null ? {} : Object(options);
          name = dictionary.name === undefined ? '' : String(dictionary.name);
        } else {
          dictionary = {};
          name = String(options);
        }
        const type = dictionary.type === undefined ? 'classic' : String(dictionary.type);
        if (type !== 'classic' && type !== 'module') {
          throw new TypeError(`SharedWorker type must be 'classic' or 'module': ${type}`);
        }
        const credentials = dictionary.credentials === undefined
          ? 'same-origin'
          : String(dictionary.credentials);
        if (credentials !== 'omit' && credentials !== 'same-origin' && credentials !== 'include') {
          throw new TypeError(`SharedWorker credentials must be 'omit', 'same-origin', or 'include': ${credentials}`);
        }
        const extendedLifetime = dictionary.extendedLifetime === undefined
          ? false
          : Boolean(dictionary.extendedLifetime);
        const created = host.__tbSharedWorkerCreate(
          parsed.href, name, type === 'module', credentials, extendedLifetime);
        const id = created[0];
        __tbWorkerIds.set(this, id);
        __tbSharedWorkerPorts.set(this, __tbMaterializePort(created[1]));
        __tbWorkers.set(id, this);
      }
      get port() { return __tbSharedWorkerPorts.get(this); }
    }
    __tbDefineWorkerHandler(SharedWorker.prototype, 'onerror');
    Object.defineProperty(SharedWorker.prototype, Symbol.toStringTag, {
      value: 'SharedWorker', writable: false, enumerable: false, configurable: true,
    });
    Object.defineProperty(globalThis, 'SharedWorker', {
      value: SharedWorker, writable: true, configurable: true,
    });
  }

  globalThis.__tbWorkerEvent = function(id, payload, portIds) {
    const worker = __tbWorkers.get(id);
    if (worker === undefined) {
      __tbDiscardPorts(portIds);
      return;
    }
    const materialized = __tbMaterializePorts(portIds);
    let data;
    try {
      data = __tbDecode(payload, materialized.ports);
    } catch (error) {
      __tbDiscardPorts(portIds);
      return;
    }
    const ports = __tbArray.map(portIds, endpoint => materialized.ports[endpoint]);
    worker.dispatchEvent(new MessageEvent('message', {
      data,
      ports: Object.freeze(ports),
    }));
    __tbFirePortCloses(materialized.closes);
  };
  globalThis.__tbWorkerErrorEvent = function(id, message, filename, lineno, colno) {
    const worker = __tbWorkers.get(id);
    if (worker === undefined) return;
    const event = new ErrorEvent('error', {
      message,
      filename,
      lineno,
      colno,
      cancelable: true,
    });
    // An uncanceled worker error is reported at the `Worker` object's own
    // realm — the document or the parent worker — exactly like an uncaught
    // exception there
    // (<https://html.spec.whatwg.org/multipage/webappapis.html#report-an-exception>).
    worker.dispatchEvent(event);
    if (!event.defaultPrevented && typeof host.__tbReportError === 'function') {
      host.__tbReportError(message, filename, lineno, colno);
    }
  };
  // A worker that never started (fetch, MIME, parse, or realm failure) fires a
  // plain `error` event at the `Worker` object, with no message
  // (<https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model>).
  globalThis.__tbWorkerLoadErrorEvent = function(id) {
    const worker = __tbWorkers.get(id);
    if (worker === undefined) return;
    worker.dispatchEvent(new Event('error'));
  };
})();
