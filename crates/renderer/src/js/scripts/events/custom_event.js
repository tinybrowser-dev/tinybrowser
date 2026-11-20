(function() {
  const TB_DETAIL = host.slots('tb-custom-event-detail');
  function CustomEvent(type) {
    if (new.target === undefined) {
      throw new TypeError('Class constructor CustomEvent cannot be invoked without new');
    }
    const event = __tbApply(host.__tb_new_custom_event, globalThis, arguments);
    // https://webidl.spec.whatwg.org/#interface-object
    if (new.target !== CustomEvent) {
      __tbSetPrototypeOf(event, new.target.prototype);
    }
    // https://dom.spec.whatwg.org/#dictdef-customeventinit
    const init = arguments[1];
    const detail = (init === undefined || init === null) ? null : init.detail;
    TB_DETAIL.set(event, detail === undefined ? null : detail);
    return event;
  }
  const proto = Object.create(globalThis.Event.prototype);
  Object.defineProperty(proto, 'constructor', { value: CustomEvent, writable: true, configurable: true });
  Object.defineProperty(proto, 'detail', {
    get: function() {
      const detail = TB_DETAIL.get(this);
      return detail === undefined ? null : detail;
    },
    enumerable: true,
    configurable: true,
  });
  Object.defineProperty(proto, 'initCustomEvent', {
    value: function(type, bubbles, cancelable, detail) {
      if (arguments.length < 1) {
        throw new TypeError('initCustomEvent: at least 1 argument required');
      }
      if (host.__tb_init_custom_event(this, type, bubbles, cancelable)) {
        TB_DETAIL.set(this, (arguments.length < 4 || arguments[3] === undefined) ? null : detail);
      }
    },
    writable: true,
    configurable: true,
  });
  Object.defineProperty(CustomEvent, 'prototype', { value: proto, writable: false });
  Object.defineProperty(globalThis, 'CustomEvent', { value: CustomEvent, writable: true, configurable: true });
})();
