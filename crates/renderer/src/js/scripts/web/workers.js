// https://html.spec.whatwg.org/multipage/workers.html#dedicated-workers-and-the-dedicatedworkerglobalscope-interface
(function() {
  const __tbWorkerIsGlobal = host.__tbIsWorker === true;

  // https://html.spec.whatwg.org/multipage/webappapis.html#event-handlers
  const __tbDefineWorkerHandler = (prototype, name) => {
    Object.defineProperty(prototype, name, {
      value: null,
      writable: true,
      enumerable: true,
      configurable: true,
    });
  };

  if (__tbWorkerIsGlobal) {
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
        // https://html.spec.whatwg.org/multipage/workers.html#dom-workerglobalscope-importscripts
        if (host.__tbWorkerModule) {
          throw new TypeError("Failed to execute 'importScripts' on 'WorkerGlobalScope': Module scripts don't support importScripts().");
        }
        for (const value of urls) {
          const href = new URL(String(value), location.href).href;
          // https://html.spec.whatwg.org/multipage/webappapis.html#fetch-a-classic-worker-imported-script
          const source = host.__tbImportScripts(href);
          if (source === undefined) {
            throw new DOMException(`Failed to load ${href}`, 'NetworkError');
          }
          host.__tbEvalImportedScript(href, source);
        }
      }
    }
    // https://html.spec.whatwg.org/multipage/webappapis.html#event-handlers
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
      // https://html.spec.whatwg.org/multipage/workers.html#dom-dedicatedworkerglobalscope-close
      close() {
        host.__tbWorkerClose();
      }
    }
    __tbDefineWorkerHandler(DedicatedWorkerGlobalScope.prototype, 'onmessage');
    __tbDefineWorkerHandler(DedicatedWorkerGlobalScope.prototype, 'onmessageerror');
    Object.defineProperty(DedicatedWorkerGlobalScope.prototype, Symbol.toStringTag, {
      value: 'DedicatedWorkerGlobalScope', writable: false, enumerable: false, configurable: true,
    });

    // https://html.spec.whatwg.org/multipage/workers.html#shared-workers-and-the-sharedworkerglobalscope-interface
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

    delete globalThis.close;
    delete globalThis.postMessage;

    if (host.__tbIsSharedWorker === true) {
      // https://html.spec.whatwg.org/multipage/workers.html#dom-sharedworkerglobalscope-onconnect
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

    // https://webidl.spec.whatwg.org/#idl-attributes
    // https://html.spec.whatwg.org/multipage/workers.html#dom-dedicatedworkerglobalscope-name
    let workerName = host.__tbWorkerName;
    Object.defineProperty(globalThis, 'name', {
      get() { return workerName; },
      set(value) {
        workerName = value;
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

    // https://html.spec.whatwg.org/multipage/webappapis.html#dom-origin
    // https://webidl.spec.whatwg.org/#idl-attributes
    let workerOrigin = location.origin;
    Object.defineProperty(globalThis, 'origin', {
      get() { return workerOrigin; },
      set(value) {
        workerOrigin = value;
        Object.defineProperty(globalThis, 'origin', {
          value: workerOrigin,
          writable: true,
          enumerable: true,
          configurable: true,
        });
      },
      enumerable: true,
      configurable: true,
    });

    // https://w3c.github.io/hr-time/#the-performance-interface
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

    globalThis.__tbWorkerDeliverMessage = function(payload, portIds) {
      const materialized = __tbMaterializePorts(portIds);
      let data;
      try {
        data = __tbDecode(payload, materialized.ports);
      } catch (error) {
        // https://html.spec.whatwg.org/multipage/web-messaging.html#transfer-receiving-steps
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

  // https://html.spec.whatwg.org/multipage/workers.html#dom-worker
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
      // https://html.spec.whatwg.org/multipage/workers.html#dom-worker
      const href = String(url);
      const parsed = host.__tbParseWorkerURL(href, __tbWorkerBase());
      if (parsed === undefined) {
        throw new DOMException(`Invalid worker script URL: ${href}`, 'SyntaxError');
      }
      super();
      // https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model
      const dictionary = options === undefined || options === null ? {} : Object(options);
      const type = dictionary.type === undefined ? 'classic' : String(dictionary.type);
      if (type !== 'classic' && type !== 'module') {
        throw new TypeError(`Worker type must be 'classic' or 'module': ${type}`);
      }
      // https://html.spec.whatwg.org/multipage/webappapis.html#fetch-a-single-module-script
      const credentials = dictionary.credentials === undefined
        ? 'same-origin'
        : String(dictionary.credentials);
      if (credentials !== 'omit' && credentials !== 'same-origin' && credentials !== 'include') {
        throw new TypeError(`Worker credentials must be 'omit', 'same-origin', or 'include': ${credentials}`);
      }
      const name = dictionary.name === undefined ? '' : String(dictionary.name);
      const id = host.__tbWorkerCreate(parsed, name, type === 'module', credentials);
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

  // https://html.spec.whatwg.org/multipage/workers.html#shared-workers-and-the-sharedworkerglobalscope-interface
  if (!__tbWorkerIsGlobal) {
    const __tbSharedWorkerPorts = new WeakMap();
    class SharedWorker extends EventTarget {
      constructor(url, options) {
        if (arguments.length < 1) {
          throw new TypeError("Failed to construct 'SharedWorker': 1 argument required, but only 0 present.");
        }
        // https://html.spec.whatwg.org/multipage/workers.html#dom-sharedworker
        const href = String(url);
        const parsed = host.__tbParseWorkerURL(href, __tbWorkerBase());
        if (parsed === undefined) {
          throw new DOMException(`Invalid shared worker script URL: ${href}`, 'SyntaxError');
        }
        super();
        // https://webidl.spec.whatwg.org/#idl-union
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
        const sameSiteCookies = dictionary.sameSiteCookies === undefined
          ? undefined
          : String(dictionary.sameSiteCookies);
        if (sameSiteCookies !== undefined
            && sameSiteCookies !== 'all' && sameSiteCookies !== 'none') {
          throw new TypeError(
            'SharedWorker sameSiteCookies must be \'all\' or \'none\': ' + sameSiteCookies);
        }
        const created = host.__tbSharedWorkerCreate(
          parsed, name, type === 'module', credentials, extendedLifetime, sameSiteCookies);
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
    // https://html.spec.whatwg.org/multipage/webappapis.html#report-an-exception
    worker.dispatchEvent(event);
    Promise.resolve().then(() => {
      if (!event.defaultPrevented && typeof host.__tbReportError === 'function') {
        host.__tbReportError(message, filename, lineno, colno);
      }
    });
  };
  // https://html.spec.whatwg.org/multipage/workers.html#worker-processing-model
  globalThis.__tbWorkerLoadErrorEvent = function(id) {
    const worker = __tbWorkers.get(id);
    if (worker === undefined) return;
    worker.dispatchEvent(new Event('error'));
  };
})();
