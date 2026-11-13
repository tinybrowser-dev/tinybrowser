
// ── uievents ───────────────────────────────────────────────────────────────
// Event interfaces for input dispatch. The engine fires its own input events
// from Rust; these classes exist so pages can construct events with the
// spec's properties.

const __tbUIEventData = host.slots('tinybrowser.uievent.data');
const __tbFreshUIEvent = () => ({ view: null, detail: 0 });
const __tbUIEventConstructor = globalThis.UIEvent = class UIEvent extends Event {
  constructor(type, init) {
    if (arguments.length < 1) {
      throw new TypeError("Failed to construct 'UIEvent': 1 argument required, but only 0 present.");
    }
    init = init || {};
    const event = __tbConstruct(__tbEventConstructor, [type, init], new.target);
    __tbUIEventData.set(event, { view: init.view || null, detail: init.detail || 0 });
    return event;
  }
  get view() { return __tbEventEntry(this, __tbUIEventData, __tbFreshUIEvent).view; }
  get detail() { return __tbEventEntry(this, __tbUIEventData, __tbFreshUIEvent).detail; }
};
Object.defineProperty(globalThis.UIEvent.prototype, Symbol.toStringTag, { value: 'UIEvent', writable: false, enumerable: false, configurable: true });

const __tbMouseEventData = host.slots('tinybrowser.mouseevent.data');
const __tbFreshMouseEvent = () => ({
  screenX: 0, screenY: 0, clientX: 0, clientY: 0,
  ctrlKey: false, shiftKey: false, altKey: false, metaKey: false,
  button: 0, buttons: 0, relatedTarget: null,
});
const __tbMouseEventConstructor = globalThis.MouseEvent = class MouseEvent extends UIEvent {
  constructor(type, init) {
    if (arguments.length < 1) {
      throw new TypeError("Failed to construct 'MouseEvent': 1 argument required, but only 0 present.");
    }
    init = init || {};
    const event = __tbConstruct(__tbUIEventConstructor, [type, init], new.target);
    // `long` members convert with `ToNumber`, not truthiness: `"5"` is 5
    // (<https://w3c.github.io/uievents/#dom-mouseevent-clientx>).
    const toLong = value => { const n = __tbNumberCtor(value); return Number.isNaN(n) ? 0 : Math.trunc(n); };
    const button = init.button === undefined ? 0 : toLong(init.button);
    __tbMouseEventData.set(event, {
        screenX: init.screenX === undefined ? 0 : toLong(init.screenX),
        screenY: init.screenY === undefined ? 0 : toLong(init.screenY),
        clientX: init.clientX === undefined ? 0 : toLong(init.clientX),
        clientY: init.clientY === undefined ? 0 : toLong(init.clientY),
        ctrlKey: !!init.ctrlKey, shiftKey: !!init.shiftKey,
        altKey: !!init.altKey, metaKey: !!init.metaKey,
        button: button, buttons: init.buttons === undefined ? 0 : toLong(init.buttons),
        relatedTarget: init.relatedTarget || null,
    });
    return event;
  }
  get screenX() { return __tbEventEntry(this, __tbMouseEventData, __tbFreshMouseEvent).screenX; }
  get screenY() { return __tbEventEntry(this, __tbMouseEventData, __tbFreshMouseEvent).screenY; }
  get clientX() { return __tbEventEntry(this, __tbMouseEventData, __tbFreshMouseEvent).clientX; }
  get clientY() { return __tbEventEntry(this, __tbMouseEventData, __tbFreshMouseEvent).clientY; }
  get ctrlKey() { return __tbEventEntry(this, __tbMouseEventData, __tbFreshMouseEvent).ctrlKey; }
  get shiftKey() { return __tbEventEntry(this, __tbMouseEventData, __tbFreshMouseEvent).shiftKey; }
  get altKey() { return __tbEventEntry(this, __tbMouseEventData, __tbFreshMouseEvent).altKey; }
  get metaKey() { return __tbEventEntry(this, __tbMouseEventData, __tbFreshMouseEvent).metaKey; }
  get button() { return __tbEventEntry(this, __tbMouseEventData, __tbFreshMouseEvent).button; }
  get buttons() { return __tbEventEntry(this, __tbMouseEventData, __tbFreshMouseEvent).buttons; }
  get relatedTarget() { return __tbEventEntry(this, __tbMouseEventData, __tbFreshMouseEvent).relatedTarget; }
  getModifierState(key) {
    return { Alt: !!this.altKey, Control: !!this.ctrlKey, Meta: !!this.metaKey, Shift: !!this.shiftKey }[__tbStringCtor(key)] || false;
  }
};
Object.defineProperty(globalThis.MouseEvent.prototype, Symbol.toStringTag, { value: 'MouseEvent', writable: false, enumerable: false, configurable: true });

const __tbPointerEventData = host.slots('tinybrowser.pointerevent.data');
// Strict `__tbBrand` (not lazy `__tbEventEntry` like the `__tbExposeEvent`
// siblings): `createEvent` never produces Pointer/Wheel/Input events, so no
// uninitialized-entry path needs the lazy default. Keep strict on any
// consistency refactor.
globalThis.PointerEvent = class PointerEvent extends MouseEvent {
  constructor(type, init) {
    if (arguments.length < 1) {
      throw new TypeError("Failed to construct 'PointerEvent': 1 argument required, but only 0 present.");
    }
    init = init || {};
    const event = __tbConstruct(__tbMouseEventConstructor, [type, init], new.target);
    __tbPointerEventData.set(event, {
        pointerId: init.pointerId === undefined ? 1 : init.pointerId,
        // An explicit 0 is a valid width, not a missing one
        // (<https://w3c.github.io/pointerevents/#dom-pointerevent-width>).
        width: init.width === undefined ? 1 : __tbNumberCtor(init.width),
        height: init.height === undefined ? 1 : __tbNumberCtor(init.height),
        pressure: init.pressure === undefined ? 0 : init.pressure,
        tangentialPressure: init.tangentialPressure || 0,
        tiltX: init.tiltX || 0, tiltY: init.tiltY || 0, twist: init.twist || 0,
        pointerType: init.pointerType || 'mouse',
        isPrimary: init.isPrimary === undefined ? true : !!init.isPrimary,
    });
    return event;
  }
  get pointerId() { return __tbBrand(this, __tbPointerEventData).pointerId; }
  get width() { return __tbBrand(this, __tbPointerEventData).width; }
  get height() { return __tbBrand(this, __tbPointerEventData).height; }
  get pressure() { return __tbBrand(this, __tbPointerEventData).pressure; }
  get tangentialPressure() { return __tbBrand(this, __tbPointerEventData).tangentialPressure; }
  get tiltX() { return __tbBrand(this, __tbPointerEventData).tiltX; }
  get tiltY() { return __tbBrand(this, __tbPointerEventData).tiltY; }
  get twist() { return __tbBrand(this, __tbPointerEventData).twist; }
  get pointerType() { return __tbBrand(this, __tbPointerEventData).pointerType; }
  get isPrimary() { return __tbBrand(this, __tbPointerEventData).isPrimary; }
};
Object.defineProperty(globalThis.PointerEvent.prototype, Symbol.toStringTag, { value: 'PointerEvent', writable: false, enumerable: false, configurable: true });

const __tbWheelEventData = host.slots('tinybrowser.wheelevent.data');
globalThis.WheelEvent = class WheelEvent extends MouseEvent {
  constructor(type, init) {
    if (arguments.length < 1) {
      throw new TypeError("Failed to construct 'WheelEvent': 1 argument required, but only 0 present.");
    }
    init = init || {};
    const event = __tbConstruct(__tbMouseEventConstructor, [type, init], new.target);
    __tbWheelEventData.set(event, {
        deltaX: init.deltaX || 0, deltaY: init.deltaY || 0, deltaZ: init.deltaZ || 0,
        deltaMode: init.deltaMode || 0,
    });
    return event;
  }
  get deltaX() { return __tbBrand(this, __tbWheelEventData).deltaX; }
  get deltaY() { return __tbBrand(this, __tbWheelEventData).deltaY; }
  get deltaZ() { return __tbBrand(this, __tbWheelEventData).deltaZ; }
  get deltaMode() { return __tbBrand(this, __tbWheelEventData).deltaMode; }
};
Object.defineProperty(globalThis.WheelEvent.prototype, Symbol.toStringTag, { value: 'WheelEvent', writable: false, enumerable: false, configurable: true });

const __tbKeyboardEventData = host.slots('tinybrowser.keyboardevent.data');
const __tbFreshKeyboardEvent = () => ({
  key: '', code: '', location: 0,
  ctrlKey: false, shiftKey: false, altKey: false, metaKey: false,
  repeat: false, isComposing: false, keyCode: 0, charCode: 0, which: 0,
});
globalThis.KeyboardEvent = class KeyboardEvent extends UIEvent {
  constructor(type, init) {
    if (arguments.length < 1) {
      throw new TypeError("Failed to construct 'KeyboardEvent': 1 argument required, but only 0 present.");
    }
    init = init || {};
    const event = __tbConstruct(__tbUIEventConstructor, [type, init], new.target);
    const key = init.key === undefined ? '' : __tbStringCtor(init.key);
    __tbKeyboardEventData.set(event, {
        key: key,
        code: init.code === undefined ? '' : __tbStringCtor(init.code),
        location: init.location || 0,
        ctrlKey: !!init.ctrlKey, shiftKey: !!init.shiftKey,
        altKey: !!init.altKey, metaKey: !!init.metaKey,
        repeat: !!init.repeat, isComposing: !!init.isComposing,
        keyCode: init.keyCode === undefined ? (key.length === 1 ? key.charCodeAt(0) : 0) : init.keyCode,
        charCode: init.charCode || 0,
        // `which` is a legacy member the init dictionary carries, so a page
        // can construct one directly; the getter falls back to `keyCode`
        // (<https://w3c.github.io/uievents/#dom-keyboardeventinit-which>,
        // <https://w3c.github.io/uievents/#dom-keyboardevent-which>).
        which: init.which === undefined ? 0 : init.which,
    });
    return event;
  }
  get key() { return __tbEventEntry(this, __tbKeyboardEventData, __tbFreshKeyboardEvent).key; }
  get code() { return __tbEventEntry(this, __tbKeyboardEventData, __tbFreshKeyboardEvent).code; }
  get location() { return __tbEventEntry(this, __tbKeyboardEventData, __tbFreshKeyboardEvent).location; }
  get ctrlKey() { return __tbEventEntry(this, __tbKeyboardEventData, __tbFreshKeyboardEvent).ctrlKey; }
  get shiftKey() { return __tbEventEntry(this, __tbKeyboardEventData, __tbFreshKeyboardEvent).shiftKey; }
  get altKey() { return __tbEventEntry(this, __tbKeyboardEventData, __tbFreshKeyboardEvent).altKey; }
  get metaKey() { return __tbEventEntry(this, __tbKeyboardEventData, __tbFreshKeyboardEvent).metaKey; }
  get repeat() { return __tbEventEntry(this, __tbKeyboardEventData, __tbFreshKeyboardEvent).repeat; }
  get isComposing() { return __tbEventEntry(this, __tbKeyboardEventData, __tbFreshKeyboardEvent).isComposing; }
  get keyCode() { return __tbEventEntry(this, __tbKeyboardEventData, __tbFreshKeyboardEvent).keyCode; }
  get charCode() { return __tbEventEntry(this, __tbKeyboardEventData, __tbFreshKeyboardEvent).charCode; }
  // The legacy alias reports an explicitly constructed value first, then
  // `keyCode`, then `charCode` for a keypress event
  // (<https://w3c.github.io/uievents/#dom-keyboardevent-which>).
  get which() {
    const stored = __tbEventEntry(this, __tbKeyboardEventData, __tbFreshKeyboardEvent);
    return stored.which || stored.keyCode || stored.charCode;
  }
  getModifierState(key) {
    return { Alt: !!this.altKey, Control: !!this.ctrlKey, Meta: !!this.metaKey, Shift: !!this.shiftKey }[__tbStringCtor(key)] || false;
  }
};
Object.defineProperty(globalThis.KeyboardEvent.prototype, Symbol.toStringTag, { value: 'KeyboardEvent', writable: false, enumerable: false, configurable: true });

const __tbInputEventData = host.slots('tinybrowser.inputevent.data');
globalThis.InputEvent = class InputEvent extends UIEvent {
  constructor(type, init) {
    if (arguments.length < 1) {
      throw new TypeError("Failed to construct 'InputEvent': 1 argument required, but only 0 present.");
    }
    init = init || {};
    const event = __tbConstruct(__tbUIEventConstructor, [type, init], new.target);
    __tbInputEventData.set(event, {
        data: init.data === undefined ? null : init.data,
        inputType: init.inputType === undefined ? '' : __tbStringCtor(init.inputType),
        isComposing: !!init.isComposing,
        dataTransfer: init.dataTransfer || null,
    });
    return event;
  }
  get data() { return __tbBrand(this, __tbInputEventData).data; }
  get inputType() { return __tbBrand(this, __tbInputEventData).inputType; }
  get isComposing() { return __tbBrand(this, __tbInputEventData).isComposing; }
  get dataTransfer() { return __tbBrand(this, __tbInputEventData).dataTransfer; }
};
Object.defineProperty(globalThis.InputEvent.prototype, Symbol.toStringTag, { value: 'InputEvent', writable: false, enumerable: false, configurable: true });

// Interfaces named by document.createEvent
// (https://dom.spec.whatwg.org/#dom-document-createevent).
// createEvent builds an uninitialized Event and sets this prototype; it does
// not call the constructor, which requires a type. Getters below therefore
// initialize uninitialized events with their spec defaults on first read
// (real events constructed with `new` already carry entries). Non-events
// still throw: the brand check is the native `Class<JsEvent>` conversion
// (`host.__tbIsEvent`), not a prototype walk, so
// `Object.create(MouseEvent.prototype)` throws instead of allocating state.
const __tbEventString = value => (value === undefined ? '' : __tbStringCtor(value));
function __tbExposeEvent(name, parent, data, fresh, read, members) {
  const ctor = {
    [name]: class extends parent {
      constructor(type) {
        if (arguments.length < 1) {
          throw new TypeError("Failed to construct '" + name + "': 1 argument required, but only 0 present.");
        }
        const init = arguments.length < 2 || arguments[1] == null ? {} : arguments[1];
        const event = __tbConstruct(parent, [type, init], new.target);
        data.set(event, read(init));
        return event;
      }
    },
  }[name];
  for (const member of members) {
    const key = member;
    __tbDefineProperty(ctor.prototype, key, {
      __proto__: null,
      get() { return __tbEventEntry(this, data, fresh)[key]; },
      enumerable: true, configurable: true,
    });
  }
  __tbDefineProperty(ctor.prototype, Symbol.toStringTag, {
    value: name, writable: false, enumerable: false, configurable: true,
  });
  // WebIDL interface objects are non-enumerable on the global
  // (<https://webidl.spec.whatwg.org/#interface-object>).
  __tbDefineProperty(globalThis, name, {
    __proto__: null,
    value: ctor, writable: true, enumerable: false, configurable: true,
  });
}
// Each exposed interface: its slot map, a fresh-defaults factory (fresh
// objects per event, so TouchEvent lists never alias), an init-dictionary
// reader, and the member names exposed as getters
// (<https://w3c.github.io/uievents/>, <https://w3c.github.io/touch-events/>).
const __tbBeforeUnloadEventData = host.slots('tinybrowser.beforeunloadevent.data');
__tbExposeEvent('BeforeUnloadEvent', __tbEventConstructor, __tbBeforeUnloadEventData,
  () => ({ returnValue: '' }),
  init => ({ returnValue: __tbEventString(init.returnValue) }),
  ['returnValue']);
// `returnValue` is read-write per spec (all other exposed members are
// read-only): a setter on the prototype writing the slot entry
// (<https://html.spec.whatwg.org/multipage/browsing-the-web.html#the-beforeunloadevent-interface>).
__tbDefineProperty(globalThis.BeforeUnloadEvent.prototype, 'returnValue', {
  __proto__: null,
  get() { return __tbEventEntry(this, __tbBeforeUnloadEventData, () => ({ returnValue: '' })).returnValue; },
  set(value) { __tbEventEntry(this, __tbBeforeUnloadEventData, () => ({ returnValue: '' })).returnValue = __tbEventString(value); },
  enumerable: true, configurable: true,
});
const __tbPageTransitionEventData = host.slots('tinybrowser.pagetransitionevent.data');
__tbExposeEvent('PageTransitionEvent', __tbEventConstructor, __tbPageTransitionEventData,
  () => ({ persisted: false }),
  init => ({ persisted: Boolean(init.persisted) }),
  ['persisted']);
const __tbHashChangeEventData = host.slots('tinybrowser.hashchangeevent.data');
__tbExposeEvent('HashChangeEvent', __tbEventConstructor, __tbHashChangeEventData,
  () => ({ oldURL: '', newURL: '' }),
  init => ({ oldURL: __tbEventString(init.oldURL), newURL: __tbEventString(init.newURL) }),
  ['oldURL', 'newURL']);
// The engine fires its own trusted `hashchange`; it owns the event object and
// trust flag, and this writes the interface's slot map for it.
host.__tbSetHashChange = (event, oldURL, newURL) => {
  __tbHashChangeEventData.set(event, {
    oldURL: __tbEventString(oldURL),
    newURL: __tbEventString(newURL),
  });
};
__tbExposeEvent('DeviceMotionEvent', __tbEventConstructor, host.slots('tinybrowser.devicemotionevent.data'),
  () => ({ acceleration: null, accelerationIncludingGravity: null, rotationRate: null, interval: 0 }),
  init => ({
    acceleration: init.acceleration || null,
    accelerationIncludingGravity: init.accelerationIncludingGravity || null,
    rotationRate: init.rotationRate || null,
    interval: init.interval === undefined ? 0 : __tbNumberCtor(init.interval),
  }),
  ['acceleration', 'accelerationIncludingGravity', 'rotationRate', 'interval']);
__tbExposeEvent('DeviceOrientationEvent', __tbEventConstructor, host.slots('tinybrowser.deviceorientationevent.data'),
  () => ({ alpha: null, beta: null, gamma: null, absolute: false }),
  init => ({
    alpha: init.alpha === undefined ? null : __tbNumberCtor(init.alpha),
    beta: init.beta === undefined ? null : __tbNumberCtor(init.beta),
    gamma: init.gamma === undefined ? null : __tbNumberCtor(init.gamma),
    absolute: !!init.absolute,
  }),
  ['alpha', 'beta', 'gamma', 'absolute']);
__tbExposeEvent('CompositionEvent', __tbUIEventConstructor, host.slots('tinybrowser.compositionevent.data'),
  () => ({ data: '' }),
  init => ({ data: __tbEventString(init.data) }),
  ['data']);
__tbExposeEvent('FocusEvent', __tbUIEventConstructor, host.slots('tinybrowser.focusevent.data'),
  () => ({ relatedTarget: null }),
  init => ({ relatedTarget: init.relatedTarget || null }),
  ['relatedTarget']);
__tbExposeEvent('TextEvent', __tbUIEventConstructor, host.slots('tinybrowser.textevent.data'),
  () => ({ data: '' }),
  init => ({ data: __tbEventString(init.data) }),
  ['data']);
__tbExposeEvent('TouchEvent', __tbUIEventConstructor, host.slots('tinybrowser.touchevent.data'),
  () => ({ touches: [], targetTouches: [], changedTouches: [], altKey: false, metaKey: false, ctrlKey: false, shiftKey: false }),
  init => ({
    touches: init.touches || [],
    targetTouches: init.targetTouches || [],
    changedTouches: init.changedTouches || [],
    altKey: !!init.altKey, metaKey: !!init.metaKey, ctrlKey: !!init.ctrlKey, shiftKey: !!init.shiftKey,
  }),
  ['touches', 'targetTouches', 'changedTouches', 'altKey', 'metaKey', 'ctrlKey', 'shiftKey']);
__tbExposeEvent('DragEvent', __tbMouseEventConstructor, host.slots('tinybrowser.dragevent.data'),
  () => ({ dataTransfer: null }),
  init => ({ dataTransfer: init.dataTransfer || null }),
  ['dataTransfer']);
// CSS Animations and Transitions own the event interfaces whose handlers the
// legacy `webkit` names alias
// (<https://drafts.csswg.org/css-animations-1/#interface-animationevent>,
// <https://drafts.csswg.org/css-transitions-1/#interface-transitionevent>).
__tbExposeEvent('AnimationEvent', __tbEventConstructor, host.slots('tinybrowser.animationevent.data'),
  () => ({ animationName: '', elapsedTime: 0, pseudoElement: '' }),
  init => ({
    animationName: __tbEventString(init.animationName),
    elapsedTime: init.elapsedTime === undefined ? 0 : __tbNumberCtor(init.elapsedTime),
    pseudoElement: __tbEventString(init.pseudoElement),
  }),
  ['animationName', 'elapsedTime', 'pseudoElement']);
__tbExposeEvent('TransitionEvent', __tbEventConstructor, host.slots('tinybrowser.transitionevent.data'),
  () => ({ propertyName: '', elapsedTime: 0, pseudoElement: '' }),
  init => ({
    propertyName: __tbEventString(init.propertyName),
    elapsedTime: init.elapsedTime === undefined ? 0 : __tbNumberCtor(init.elapsedTime),
    pseudoElement: __tbEventString(init.pseudoElement),
  }),
  ['propertyName', 'elapsedTime', 'pseudoElement']);

// Touch Events extends GlobalEventHandlers with these handlers. createEvent's
// touch rows run only when the document exposes one
// (https://w3c.github.io/touch-events/#extensions-to-the-globaleventhandlers-mixin).
if (typeof Document !== 'undefined') {
  for (const name of ['ontouchstart', 'ontouchend', 'ontouchmove', 'ontouchcancel']) {
    Object.defineProperty(Document.prototype, name, {
      get() { return host.__tbGetNodeHandler(this, name); },
      set(value) { host.__tbSetNodeHandler(this, name, value); },
      enumerable: true, configurable: true,
    });
  }
}
