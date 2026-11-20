(function() {
  const Native = globalThis.Event;
  const isTrustedGet = Object.getOwnPropertyDescriptor(Native.prototype, 'isTrusted').get;
  function Event() {
    if (new.target === undefined) {
      throw new TypeError('Class constructor Event cannot be invoked without new');
    }
    const event = __tbConstruct(Native, arguments, new.target);
    // https://dom.spec.whatwg.org/#dom-event-istrusted
    // https://webidl.spec.whatwg.org/#dfn-unforgeable
    __tbDefineProperty(event, 'isTrusted', {
      __proto__: null,
      get: isTrustedGet, enumerable: true, configurable: false,
    });
    return event;
  }
  // https://webidl.spec.whatwg.org/#define-the-constants
  function defineConstant(target, name, value) {
    Object.defineProperty(target, name, { value: value, writable: false, enumerable: true, configurable: false });
  }
  for (const [name, value] of [['NONE', 0], ['CAPTURING_PHASE', 1], ['AT_TARGET', 2], ['BUBBLING_PHASE', 3]]) {
    defineConstant(Event, name, value);
  }
  Object.defineProperty(Event, 'prototype', { value: Native.prototype, writable: false, configurable: false });
  Object.defineProperty(Native.prototype, 'constructor', { value: Event, writable: true, configurable: true });
  Object.defineProperty(globalThis, 'Event', { value: Event, writable: true, configurable: true });
})();
