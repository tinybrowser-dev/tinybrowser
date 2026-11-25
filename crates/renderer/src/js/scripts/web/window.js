{
  // https://webidl.spec.whatwg.org/#dfn-replaceable
  const replaceable = (name, get) => {
    Object.defineProperty(globalThis, name, {
      get,
      set(value) {
        Object.defineProperty(globalThis, name, {
          value, writable: true, enumerable: true, configurable: true,
        });
      },
      enumerable: true, configurable: true,
    });
  };
  const method = (name, value) => {
    Object.defineProperty(globalThis, name, {
      value, writable: true, enumerable: true, configurable: true,
    });
  };

  // https://html.spec.whatwg.org/multipage/window-object.html#barprop
  const barSlots = host.slots('BarProp');
  function BarProp() {
    throw new TypeError('Illegal constructor');
  }
  BarProp.prototype = Object.create(Object.prototype, {
    constructor: { value: BarProp, writable: true, configurable: true },
  });
  __tbDefineProperty(BarProp.prototype, 'visible', {
    get() {
      if (!barSlots.get(this)) throw new TypeError('Illegal invocation');
      return true;
    },
    enumerable: true, configurable: true,
  });
  __tbDefineProperty(BarProp.prototype, Symbol.toStringTag, {
    value: 'BarProp', writable: false, enumerable: false, configurable: true,
  });
  Object.defineProperty(globalThis, 'BarProp', {
    value: BarProp, writable: true, configurable: true,
  });
  const bar = () => {
    const instance = Object.create(BarProp.prototype);
    barSlots.set(instance, true);
    return instance;
  };
  for (const name of [
    'locationbar', 'menubar', 'personalbar', 'scrollbars', 'statusbar', 'toolbar',
  ]) {
    let current;
    replaceable(name, () => {
      if (current === undefined) current = bar();
      return current;
    });
  }

  // https://drafts.csswg.org/cssom-view/#dom-window-devicepixelratio
  Object.defineProperty(globalThis, 'devicePixelRatio', {
    get() { return 1; },
    enumerable: true, configurable: true,
  });
  // https://drafts.csswg.org/cssom-view/#dom-window-screenx
  replaceable('screenX', () => 0);
  replaceable('screenY', () => 0);
  // https://html.spec.whatwg.org/multipage/window-object.html#dom-window-status
  replaceable('status', () => '');
  // https://html.spec.whatwg.org/multipage/window-object.html#dom-window-clientinformation
  replaceable('clientInformation', () => navigator);
  // https://html.spec.whatwg.org/multipage/window-object.html#dom-window-closed
  replaceable('closed', () => false);
  // https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-frameelement
  replaceable('frameElement', () => host.__tbFrameElement());
  // https://dom.spec.whatwg.org/#concept-event-listener-inner-invoke
  replaceable('event', () => host.__tbCurrentEvent());

  // https://html.spec.whatwg.org/multipage/timers-and-user-prompts.html#dom-alert
  method('alert', function alert() {
    const message = arguments.length === 0 ? '' : String(arguments[0]);
    host.__tbUserPrompt('alert', message, null);
  });
  // https://html.spec.whatwg.org/multipage/timers-and-user-prompts.html#dom-confirm
  method('confirm', function confirm() {
    const message = arguments.length === 0 || arguments[0] === undefined ? '' : String(arguments[0]);
    return Boolean(host.__tbUserPrompt('confirm', message, null));
  });
  // https://html.spec.whatwg.org/multipage/timers-and-user-prompts.html#dom-prompt
  method('prompt', function prompt() {
    const message = arguments.length === 0 || arguments[0] === undefined ? '' : String(arguments[0]);
    const defaultValue = arguments.length < 2 || arguments[1] === undefined ? '' : String(arguments[1]);
    return host.__tbUserPrompt('prompt', message, defaultValue);
  });
  method('print', function print() {});
  // https://drafts.csswg.org/cssom-view/#dom-window-moveby
  for (const name of ['moveBy', 'moveTo', 'resizeBy', 'resizeTo']) {
    method(name, function() {});
  }
  // https://html.spec.whatwg.org/multipage/window-object.html#dom-window-stop
  method('stop', function stop() {});
  // https://html.spec.whatwg.org/multipage/window-object.html#dom-window-focus
  method('blur', function blur() {});
  method('focus', function focus() {});
  // https://w3c.github.io/selection-api/#extensions-to-the-window-interface
  method('getSelection', function getSelection() {
    return document.getSelection();
  });
}
