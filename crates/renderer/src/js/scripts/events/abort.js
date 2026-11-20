(function() {
  const STATE = host.slots('abort-state');
  const DEPENDENTS = new Map();
  const EventTargetConstructor = globalThis.EventTarget;
  const EventConstructor = globalThis.Event;
  const dispatchEvent = EventTargetConstructor.prototype.dispatchEvent;
  function createSignal() {
    const signal = __tbConstruct(EventTargetConstructor, [], AbortSignal);
    STATE.set(signal, { aborted: false, reason: undefined });
    return signal;
  }
  function signalAbort(signal, reason) {
    const state = STATE.get(signal);
    if (!state || state.aborted) {
      return;
    }
    state.aborted = true;
    state.reason = reason !== undefined
      ? reason
      : new DOMException('signal is aborted without reason', 'AbortError');
    __tbApply(dispatchEvent, signal, [new EventConstructor('abort')]);
    const dependents = DEPENDENTS.get(signal);
    if (dependents !== undefined) {
      DEPENDENTS.delete(signal);
      for (const dependent of dependents) signalAbort(dependent, state.reason);
    }
  }
  function AbortSignal() {
    throw new TypeError('Illegal constructor');
  }
  const signalProto = Object.create(globalThis.EventTarget.prototype);
  Object.defineProperty(signalProto, 'constructor', {
    value: AbortSignal, writable: true, configurable: true,
  });
  Object.defineProperty(signalProto, 'aborted', {
    get: function() { return !!(STATE.get(this) && STATE.get(this).aborted); },
    enumerable: true, configurable: true,
  });
  Object.defineProperty(signalProto, 'reason', {
    get: function() { return STATE.get(this) ? STATE.get(this).reason : undefined; },
    enumerable: true, configurable: true,
  });
  Object.defineProperty(signalProto, 'throwIfAborted', {
    value: function() {
      if (STATE.get(this) && STATE.get(this).aborted) {
        throw STATE.get(this).reason;
      }
    },
    writable: true, enumerable: true, configurable: true,
  });
  Object.defineProperty(AbortSignal, 'prototype', {
    value: signalProto, writable: false, configurable: false,
  });
  Object.defineProperty(AbortSignal, 'abort', {
    value: function(reason) {
      const signal = createSignal();
      signalAbort(signal, reason);
      return signal;
    },
    writable: true, enumerable: true, configurable: true,
  });
  Object.defineProperty(AbortSignal, 'timeout', {
    value: function(milliseconds) {
      const signal = createSignal();
      host.setTimeout(function() {
        signalAbort(signal, new DOMException('The operation timed out.', 'TimeoutError'));
      }, Number(milliseconds));
      return signal;
    },
    writable: true, enumerable: true, configurable: true,
  });
  Object.defineProperty(AbortSignal, 'any', {
    value: function(signals) {
      const controller = new AbortController();
      for (const signal of signals) {
        if (signal.aborted) {
          controller.abort(signal.reason);
          break;
        }
        let dependents = DEPENDENTS.get(signal);
        if (dependents === undefined) {
          dependents = new Set();
          DEPENDENTS.set(signal, dependents);
        }
        dependents.add(controller.signal);
      }
      return controller.signal;
    },
    writable: true, enumerable: true, configurable: true,
  });
  function AbortController() {
    if (new.target === undefined) {
      throw new TypeError('Class constructor AbortController cannot be invoked without new');
    }
    const signal = createSignal();
    __tbDefineProperty(this, 'signal', {
      __proto__: null,
      get: function() { return signal; },
      enumerable: true, configurable: true,
    });
  }
  Object.defineProperty(AbortController.prototype, 'abort', {
    value: function(reason) { signalAbort(this.signal, reason); },
    writable: true, enumerable: true, configurable: true,
  });
  Object.defineProperty(AbortController.prototype, 'constructor', {
    value: AbortController, writable: true, configurable: true,
  });
  Object.defineProperty(globalThis, 'AbortSignal', {
    value: AbortSignal, writable: true, configurable: true,
  });
  Object.defineProperty(globalThis, 'AbortController', {
    value: AbortController, writable: true, configurable: true,
  });
})();
