host.array = __tbPrivateArray;
host.push = __tbArray.push;
// https://html.spec.whatwg.org/multipage/timers-and-user-prompts.html#dom-settimeout — slot 0 stays unused so the first setTimeout handle is 1, matching Chromium and WPT
host.__tb_timeouts = [null];
__tbSetPrototypeOf(host.__tb_timeouts, null);
host.__tb_intervals = Object.create(null);
host.__tb_fetchCbs = Object.create(null);
host.__tb_fetchSeq = 0;
let __tbTimerNesting = 0;
globalThis.clearTimeout = function(id) {
  host.__tb_timeouts[id] = function() {};
  delete host.__tb_intervals[id];
  host.__cancelTimeout(Number(id));
};
// https://html.spec.whatwg.org/multipage/timers-and-user-prompts.html#timer-initialisation-steps
const __tbTimer = (handler, ms, args, repeat) => {
  var delay = Math.max(0, Number(ms) | 0);
  var callback;
  if (typeof handler === 'function') {
    callback = handler;
  } else {
    callback = String(handler);
    // https://html.spec.whatwg.org/multipage/timers-and-user-prompts.html#timer-initialisation-steps
    if (!host.__tbCspAllowsEval(callback)) {
      return 0;
    }
  }
  var id = host.__tb_timeouts.length;
  var nesting;
  function schedule() {
    // https://crbug.com/1108877 — HTML clamps nesting level before incrementing; Chromium increments first
    var timeout = __tbTimerNesting > 5 ? Math.max(4, delay) : delay;
    nesting = __tbTimerNesting + 1;
    host.__scheduleTimeout(id, timeout);
  }
  function tick() {
    var previous = __tbTimerNesting;
    __tbTimerNesting = nesting;
    try {
      if (typeof callback === 'function') {
        __tbApply(callback, globalThis, args);
      } else {
        // https://html.spec.whatwg.org/multipage/timers-and-user-prompts.html#timer-initialisation-steps
        host.__tbTimerStringBegin();
        try {
          (0, eval)(callback);
        } finally {
          host.__tbTimerStringEnd();
        }
      }
    } finally {
      try {
        if (repeat && host.__tb_intervals[id]) {
          host.__tb_timeouts[id] = tick;
          schedule();
        }
      } finally {
        __tbTimerNesting = previous;
      }
    }
  }
  if (repeat) host.__tb_intervals[id] = true;
  host.__tb_timeouts[id] = tick;
  schedule();
  return id;
};
globalThis.setTimeout = function(handler, ms, ...args) {
  return __tbTimer(handler, ms, args, false);
};
host.setTimeout = globalThis.setTimeout;
globalThis.setInterval = function(handler, ms, ...args) {
  return __tbTimer(handler, ms, args, true);
};
globalThis.clearInterval = globalThis.clearTimeout;
// https://html.spec.whatwg.org/multipage/imagebitmap-and-animations.html#dom-animationframeprovider-requestanimationframe
let pendingFrameCallbacks = 0;
const activeFrameCallbacks = Object.create(null);
globalThis.requestAnimationFrame = function(fn) {
  if (typeof fn !== 'function') {
    throw new TypeError('requestAnimationFrame requires a callback');
  }
  pendingFrameCallbacks += 1;
  let handle = 0;
  handle = __tbTimer(function() {
    if (!activeFrameCallbacks[handle]) return;
    delete activeFrameCallbacks[handle];
    pendingFrameCallbacks -= 1;
    // https://html.spec.whatwg.org/multipage/imagebitmap-and-animations.html#run-the-animation-frame-callbacks
    const now = globalThis.performance ? globalThis.performance.now() : Date.now();
    fn(now);
  }, 16, [], false) + 1;
  activeFrameCallbacks[handle] = true;
  return handle;
};
globalThis.cancelAnimationFrame = function(id) {
  const handle = Number(id);
  if (activeFrameCallbacks[handle]) {
    delete activeFrameCallbacks[handle];
    pendingFrameCallbacks -= 1;
  }
  globalThis.clearTimeout(handle - 1);
};
// https://html.spec.whatwg.org/multipage/imagebitmap-and-animations.html#run-the-animation-frame-callbacks
host.__tbPendingFrameCallbacks = function() {
  return pendingFrameCallbacks;
};
if (typeof globalThis.console === 'undefined') {
  // https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#event-consoleAPICalled
  const call = (type) => (...parts) => {
    const args = [];
    for (let index = 0; index < parts.length; index++)
      args[index] = host.remoteObject(parts[index]);
    host.__tbConsoleCall(type, host.apply(host.stringify, null, [args]));
  };
  const noop = function() {};
  globalThis.console = {
    log: call('log'), info: call('info'), warn: call('warning'),
    error: call('error'), debug: call('debug'), trace: call('trace'),
    dir: call('dir'), table: call('table'),
    group: call('startGroup'), groupCollapsed: call('startGroupCollapsed'),
    groupEnd: call('endGroup'),
    assert: (condition, ...parts) => {
      if (condition) return;
      const args = parts.length === 0
        ? [host.remoteObject('Assertion failed')]
        : parts.map((part) => host.remoteObject(part));
      host.__tbConsoleCall('assert', host.apply(host.stringify, null, [args]));
    },
    time: noop, timeEnd: noop, count: noop,
  };
}
// https://html.spec.whatwg.org/multipage/dom.html#dom-document-cookie
if (globalThis.document && globalThis.Document) {
  Object.defineProperty(globalThis.Document.prototype, 'cookie', {
    get() {
      if (host.__tbCookieAverse(this)) return '';
      if (host.__tbOpaqueOrigin === true) {
        throw new DOMException(
            'Access to cookie is denied for opaque origin.', 'SecurityError');
      }
      return host.__cookieGet();
    },
    set(v) {
      if (host.__tbCookieAverse(this)) return;
      if (host.__tbOpaqueOrigin === true) {
        throw new DOMException(
            'Access to cookie is denied for opaque origin.', 'SecurityError');
      }
      // https://cookiestore.spec.whatwg.org/#process-cookie-changes
      host.__cookieSet(String(v));
    },
    enumerable: true,
    configurable: true,
  });
}
