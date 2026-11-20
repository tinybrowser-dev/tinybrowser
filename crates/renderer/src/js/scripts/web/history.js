// https://html.spec.whatwg.org/multipage/nav-history-apis.html#the-history-interface
(function() {
  const initialState = host.__tbHistoryState;
  let currentState = initialState == null ? null : __tbDecode(initialState, Object.create(null));
  let length = host.__tbHistoryLength;
  globalThis.PopStateEvent = class PopStateEvent extends Event {
    constructor(type, init) {
      super(type, init);
      this.state = init == null || init.state === undefined ? null : init.state;
    }
  };
  host.__tbHistoryRestore = function(payload, count) {
    currentState = payload == null ? null : __tbDecode(payload, Object.create(null));
    length = count;
    return currentState;
  };
  globalThis.history = {
    get length() { return length; },
    get state() { return currentState; },
    scrollRestoration: 'auto',
    pushState(data, unused, url) { update(data, url, false); },
    replaceState(data, unused, url) { update(data, url, true); },
    go(delta = 0) { host.__tbHistoryTraverse(Number(delta) | 0); },
    back() { this.go(-1); },
    forward() { this.go(1); },
  };
  function update(data, url, replace) {
    // https://html.spec.whatwg.org/multipage/nav-history-apis.html#shared-history-push/replace-state-steps
    const serialized = __tbEncode(data, undefined, null).payload;
    const next = url === undefined || url === null || String(url) === ''
      ? String(globalThis.location.href)
      : new URL(String(url), globalThis.document.baseURI).href;
    const current = new URL(globalThis.location.href);
    const target = new URL(next);
    if (target.origin !== current.origin) {
      throw new DOMException('History URL must be same-origin', 'SecurityError');
    }
    length = replace
      ? host.__tbHistoryReplace(next, serialized)
      : host.__tbHistoryPush(next, serialized);
    currentState = __tbDecode(serialized, Object.create(null));
  }
  Object.defineProperty(globalThis.history, Symbol.toStringTag, { value: 'History' });
})();
