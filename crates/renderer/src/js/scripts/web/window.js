// The window's own members that no generated interface installs.
//
// The global object is the `Window` instance, but the engine does not build it
// from `Window.prototype`: these members install as own properties, and the
// interface-object gap is recorded in `docs/limitations.md`. A shim that
// rewired the global's prototype regressed far more than it fixed.
{
  // A `[Replaceable]` attribute is enumerable, and assigning it replaces the
  // accessor with an own data property
  // (<https://webidl.spec.whatwg.org/#dfn-replaceable>).
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

  // ── BarProp ──────────────────────────────────────────────────────────
  // There is no browser chrome, so every bar reports itself invisible... but
  // this engine has no bars at all and reports `true` for each, the value a
  // window with chrome reports for its own bars
  // (<https://html.spec.whatwg.org/multipage/window-object.html#barprop>).
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

  // ── attributes ───────────────────────────────────────────────────────
  // No device scale factor is applied, so one CSS pixel is one device pixel
  // (<https://drafts.csswg.org/cssom-view/#dom-window-devicepixelratio>).
  Object.defineProperty(globalThis, 'devicePixelRatio', {
    get() { return 1; },
    enumerable: true, configurable: true,
  });
  // No window chrome moves, so the screen coordinates stay at the origin
  // (<https://drafts.csswg.org/cssom-view/#dom-window-screenx>).
  replaceable('screenX', () => 0);
  replaceable('screenY', () => 0);
  // The status bar is gone from the platform, but the property stays writable
  // and its text never renders
  // (<https://html.spec.whatwg.org/multipage/window-object.html#dom-window-status>).
  replaceable('status', () => '');
  // A document whose browsing context is still alive is never closed
  // (<https://html.spec.whatwg.org/multipage/window-object.html#dom-window-closed>).
  replaceable('closed', () => false);
  // The container element of this frame; the host resolves it, and throws for
  // a cross-origin parent
  // (<https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-frameelement>).
  replaceable('frameElement', () => host.__tbFrameElement());
  // The realm window's current event, which dispatch fills while one of this
  // realm's listeners runs. `[Replaceable]` means an assignment installs an
  // own property, so `window.event = 1` hides this accessor from then on and
  // dispatch keeps writing only the world's slot
  // (<https://dom.spec.whatwg.org/#concept-event-listener-inner-invoke>).
  replaceable('event', () => host.__tbCurrentEvent());

  // ── operations ───────────────────────────────────────────────────────
  // User prompts are not implemented: the engine has no UI to ask. `alert` and
  // `print` do nothing, `confirm` answers false, and `prompt` answers null,
  // which are the answers a dismissed prompt gives
  // (<https://html.spec.whatwg.org/multipage/timers-and-user-prompts.html#dom-alert>).
  method('alert', function alert() {});
  method('confirm', function confirm() { return false; });
  method('prompt', function prompt() { return null; });
  method('print', function print() {});
  // There is no window chrome to move or resize
  // (<https://drafts.csswg.org/cssom-view/#dom-window-moveby>).
  for (const name of ['moveBy', 'moveTo', 'resizeBy', 'resizeTo']) {
    method(name, function() {});
  }
  // `stop()` aborts the document load. The engine's load is synchronous for
  // the document itself, so there is nothing left to abort by the time script
  // runs, and the host owns child navigations
  // (<https://html.spec.whatwg.org/multipage/window-object.html#dom-window-stop>).
  method('stop', function stop() {});
  // Focus moves between documents, and this engine has no window manager; a
  // frame's `focus()` and `blur()` are therefore no-ops for the window itself,
  // while element focus stays on `HTMLElement.focus()`
  // (<https://html.spec.whatwg.org/multipage/window-object.html#dom-window-focus>).
  method('blur', function blur() {});
  method('focus', function focus() {});
}
