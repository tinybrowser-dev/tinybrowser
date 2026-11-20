// https://html.spec.whatwg.org/multipage/webappapis.html#report-the-error
(function() {
  const errorData = host.slots('ErrorEvent');
  const rejectionData = host.slots('PromiseRejectionEvent');
  const EventConstructor = globalThis.Event;
  const construct = Reflect.construct;

  // https://html.spec.whatwg.org/multipage/webappapis.html#erroreventinit
  function ErrorEvent(type) {
    if (new.target === undefined) {
      throw new TypeError('Class constructor ErrorEvent cannot be invoked without new');
    }
    const event = construct(EventConstructor, arguments, new.target);
    const init = arguments[1];
    const dictionary = (init === undefined || init === null) ? {} : init;
    errorData.set(event, {
      message: dictionary.message === undefined ? '' : String(dictionary.message),
      filename: dictionary.filename === undefined ? '' : String(dictionary.filename),
      lineno: dictionary.lineno === undefined ? 0 : (dictionary.lineno >>> 0),
      colno: dictionary.colno === undefined ? 0 : (dictionary.colno >>> 0),
      error: dictionary.error === undefined ? null : dictionary.error,
    });
    return event;
  }

  const proto = Object.create(globalThis.Event.prototype);
  Object.defineProperty(proto, 'constructor', {
    value: ErrorEvent, writable: true, configurable: true,
  });
  const member = (name, fallback) => {
    Object.defineProperty(proto, name, {
      get() {
        const data = errorData.get(this);
        const value = data === undefined ? undefined : data[name];
        return value === undefined ? fallback : value;
      },
      enumerable: true,
      configurable: true,
    });
  };
  member('message', '');
  member('filename', '');
  member('lineno', 0);
  member('colno', 0);
  member('error', null);
  Object.defineProperty(proto, Symbol.toStringTag, {
    value: 'ErrorEvent', writable: false, enumerable: false, configurable: true,
  });
  Object.defineProperty(ErrorEvent, 'prototype', { value: proto, writable: false });
  Object.defineProperty(globalThis, 'ErrorEvent', {
    value: ErrorEvent, writable: true, configurable: true,
  });

  // https://html.spec.whatwg.org/multipage/webappapis.html#promiserejectioneventinit
  function PromiseRejectionEvent(type) {
    if (new.target === undefined) {
      throw new TypeError('Class constructor PromiseRejectionEvent cannot be invoked without new');
    }
    const init = arguments[1];
    if (init === undefined || init === null || init.promise === undefined) {
      throw new TypeError('PromiseRejectionEventInit requires a promise');
    }
    const event = construct(EventConstructor, arguments, new.target);
    rejectionData.set(event, { promise: init.promise, reason: init.reason });
    return event;
  }
  const rejectionProto = Object.create(globalThis.Event.prototype);
  Object.defineProperty(rejectionProto, 'constructor', {
    value: PromiseRejectionEvent, writable: true, configurable: true,
  });
  Object.defineProperty(rejectionProto, 'promise', {
    get() { const data = rejectionData.get(this); return data === undefined ? undefined : data.promise; }, enumerable: true, configurable: true,
  });
  Object.defineProperty(rejectionProto, 'reason', {
    get() { const data = rejectionData.get(this); return data === undefined ? undefined : data.reason; }, enumerable: true, configurable: true,
  });
  Object.defineProperty(rejectionProto, Symbol.toStringTag, {
    value: 'PromiseRejectionEvent', writable: false, enumerable: false, configurable: true,
  });
  Object.defineProperty(PromiseRejectionEvent, 'prototype', {
    value: rejectionProto, writable: false,
  });
  Object.defineProperty(globalThis, 'PromiseRejectionEvent', {
    value: PromiseRejectionEvent, writable: true, configurable: true,
  });

  const locationOf = stack => {
    if (typeof stack !== 'string') return null;
    for (const frame of stack.split('\n')) {
      const match = /(?:\(|at )([^()\s]+):(\d+):(\d+)\)?\s*$/.exec(frame);
      if (match !== null) {
        return { file: match[1], line: Number(match[2]), column: Number(match[3]) };
      }
    }
    return null;
  };

  // https://html.spec.whatwg.org/multipage/webappapis.html#report-an-exception
  const fileOf = (location, filename) => {
    if (location === null) return filename;
    return /^[a-zA-Z][a-zA-Z0-9+.-]*:/.test(location.file) ? location.file : filename;
  };

  // https://html.spec.whatwg.org/multipage/webappapis.html#report-an-exception
  const describe = caught => {
    const isObject =
      caught !== null && (typeof caught === 'object' || typeof caught === 'function');
    let text;
    let name = '';
    let stack = '';
    if (isObject) {
      text = caught.message === undefined ? String(caught) : String(caught.message);
      if (typeof caught.name === 'string') name = caught.name;
      if (typeof caught.stack === 'string') stack = caught.stack;
    } else {
      text = String(caught);
    }
    return {
      message: name === '' ? `Uncaught ${text}` : `Uncaught ${name}: ${text}`,
      stack,
    };
  };

  const lineOf = (location, baseLine) => {
    if (location === null) return baseLine;
    return baseLine === 0 ? location.line : baseLine + location.line - 1;
  };

  let reporting = false;

  host.__tbReportException = function(caught, meta) {
    const info = describe(caught);
    const location = locationOf(info.stack);
    const details = () => {
      const baseLine = meta !== null && typeof meta === 'object' &&
        typeof meta.baseLine === 'number' ? meta.baseLine : 0;
      const filename = meta !== null && typeof meta === 'object' &&
        typeof meta.filename === 'string' && meta.filename !== ''
        ? meta.filename
        : String(globalThis.location === undefined ? '' : globalThis.location.href);
      return {
        filename: fileOf(location, filename),
        lineno: lineOf(location, baseLine),
        colno: location === null ? 0 : location.column,
      };
    };
    if (reporting) {
      // https://html.spec.whatwg.org/multipage/webappapis.html#report-an-exception
      if (typeof host.__tbWorkerUnhandled === 'function') {
        const { filename, lineno, colno } = details();
        host.__tbWorkerUnhandled(info.message, filename, lineno, colno);
      }
      return false;
    }
    reporting = true;
    try {
      const { filename, lineno, colno } = details();
      const event = new ErrorEvent('error', {
        message: info.message,
        filename,
        lineno,
        colno,
        // https://html.spec.whatwg.org/multipage/webappapis.html#report-an-exception
        error: caught,
        cancelable: true,
      });
      host.__tbDispatchTrusted(event);
      // https://html.spec.whatwg.org/multipage/workers.html#runtime-script-errors-in-workers
      if (!event.defaultPrevented && typeof host.__tbWorkerUnhandled === 'function') {
        host.__tbWorkerUnhandled(info.message, filename, event.lineno, event.colno);
      }
      return event.defaultPrevented;
    } finally {
      reporting = false;
    }
  };
  Object.defineProperty(host, '__tbReportException', {
    writable: false, configurable: false, enumerable: false,
  });

  // https://html.spec.whatwg.org/multipage/webappapis.html#report-an-exception
  host.__tbReportError = function(message, filename, lineno, colno) {
    const event = new ErrorEvent('error', {
      message,
      filename,
      lineno,
      colno,
      error: null,
      cancelable: true,
    });
    host.__tbDispatchTrusted(event);
    if (!event.defaultPrevented && typeof host.__tbWorkerUnhandled === 'function') {
      host.__tbWorkerUnhandled(message, filename, lineno, colno);
    }
  };
  Object.defineProperty(host, '__tbReportError', {
    writable: false, configurable: false, enumerable: false,
  });

  // https://html.spec.whatwg.org/multipage/webappapis.html#unhandled-promise-rejections
  host.__tbPromiseRejection = function(handled, promise, reason) {
    const event = new PromiseRejectionEvent(
      handled ? 'rejectionhandled' : 'unhandledrejection',
      { promise: promise, reason: reason, cancelable: !handled },
    );
    host.__tbDispatchTrusted(event);
    return event.defaultPrevented;
  };
  Object.defineProperty(host, '__tbPromiseRejection', {
    writable: false, configurable: false, enumerable: false,
  });
})();
