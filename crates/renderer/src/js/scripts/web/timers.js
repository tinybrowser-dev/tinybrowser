const __tbEventConstructor = globalThis.Event;
const __tbEventTargetConstructor = globalThis.EventTarget;
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
globalThis.requestAnimationFrame = function(fn) {
  if (typeof fn !== 'function') {
    throw new TypeError('requestAnimationFrame requires a callback');
  }
  pendingFrameCallbacks += 1;
  return __tbTimer(function() {
    pendingFrameCallbacks -= 1;
    fn(Date.now());
  }, 16, [], false) + 1;
};
globalThis.cancelAnimationFrame = function(id) {
  globalThis.clearTimeout(Number(id) - 1);
};
// https://html.spec.whatwg.org/multipage/imagebitmap-and-animations.html#run-the-animation-frame-callbacks
globalThis.__tbPendingFrameCallbacks = function() {
  return pendingFrameCallbacks;
};
if (typeof globalThis.console === 'undefined') {
  const noop = function() {};
  globalThis.console = {
    log: noop, info: noop, warn: noop, error: noop, debug: noop,
    trace: noop, dir: noop, group: noop, groupEnd: noop, table: noop,
    assert: noop, time: noop, timeEnd: noop, count: noop,
  };
}
// https://html.spec.whatwg.org/multipage/dom.html#dom-document-cookie
if (globalThis.document && globalThis.Document) {
  Object.defineProperty(globalThis.Document.prototype, 'cookie', {
    get() {
      if (host.__tbCookieAverse(this)) return '';
      return host.__cookieGet();
    },
    set(v) {
      if (host.__tbCookieAverse(this)) return;
      // https://cookiestore.spec.whatwg.org/#process-cookie-changes
      host.__cookieSet(String(v));
    },
    enumerable: true,
    configurable: true,
  });
}
