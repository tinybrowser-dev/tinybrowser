const __tbEventConstructor = globalThis.Event;
const __tbEventTargetConstructor = globalThis.EventTarget;
host.array = __tbPrivateArray;
host.push = __tbArray.push;
// Timer handles are positive integers and the table index is the handle, so
// slot 0 stays unused: Chromium's first `setTimeout` returns 1 and WPT reads
// that value
// (<https://html.spec.whatwg.org/multipage/timers-and-user-prompts.html#dom-settimeout>).
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
// Both clear methods remove IDs from the same timer map, and an interval
// reschedules with its original ID after invoking its handler.
// <https://html.spec.whatwg.org/multipage/timers-and-user-prompts.html#timer-initialisation-steps>
const __tbTimer = (handler, ms, args, repeat) => {
  var delay = Math.max(0, Number(ms) | 0);
  var callback;
  if (typeof handler === 'function') {
    callback = handler;
  } else {
    callback = String(handler);
    // The timer initialization steps compile a string handler when the timer
    // is created and return 0 when compilation fails, which is what CSP
    // blocking an evaluation does
    // (<https://html.spec.whatwg.org/multipage/timers-and-user-prompts.html#timer-initialisation-steps>).
    if (!host.__tbCspAllowsEval(callback)) {
      return 0;
    }
  }
  var id = host.__tb_timeouts.length;
  var nesting;
  function schedule() {
    // Chromium increments before clamping (crbug.com/1108877); HTML clamps first.
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
        // The string was checked when the timer was created; running it is
        // not a second compilation
        // (<https://html.spec.whatwg.org/multipage/timers-and-user-prompts.html#timer-initialisation-steps>).
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
// The engine has no rendering pipeline; a frame callback is a 16ms timer
// (<https://html.spec.whatwg.org/multipage/imagebitmap-and-animations.html#dom-animationframeprovider-requestanimationframe>).
// Deviations: callbacks queued in the same frame do not share a
// DOMHighResTimeStamp (each gets `Date.now()`), handles share the timer
// table with the +1 offset, and `cancelAnimationFrame` of a plain
// `setTimeout` handle cancels that timer.
globalThis.requestAnimationFrame = function(fn) {
  if (typeof fn !== 'function') {
    throw new TypeError('requestAnimationFrame requires a callback');
  }
  return __tbTimer(function() {
    fn(Date.now());
  }, 16, [], false) + 1;
};
globalThis.cancelAnimationFrame = function(id) {
  globalThis.clearTimeout(Number(id) - 1);
};
// A no-op console: enough for scripts that only log, without a logging pipe.
if (typeof globalThis.console === 'undefined') {
  const noop = function() {};
  globalThis.console = {
    log: noop, info: noop, warn: noop, error: noop, debug: noop,
    trace: noop, dir: noop, group: noop, groupEnd: noop, table: noop,
    assert: noop, time: noop, timeEnd: noop, count: noop,
  };
}
// A worker global has no document and no cookie store.
if (globalThis.document) {
  Object.defineProperty(document, 'cookie', {
    get() { return host.__cookieGet(); },
    set(v) {
      // The CookieStore change event covers `document.cookie` writes too, and
      // the diff is against the jar as it stood before this write
      // (<https://cookiestore.spec.whatwg.org/#process-cookie-changes>).
      const snapshot = globalThis.__tbCookieStoreSnapshot;
      const before = typeof snapshot === 'function' ? snapshot() : null;
      host.__cookieSet(String(v));
      const changed = globalThis.__tbCookieStoreDidWrite;
      if (before !== null && typeof changed === 'function') {
        changed(before);
      }
    }
  });
}
