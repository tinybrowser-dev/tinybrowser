const __tbUIEventData = host.slots('tinybrowser.uievent.data');
const __tbFreshUIEvent = () => ({ view: null, detail: 0, which: 0 });
// https://webidl.spec.whatwg.org/#es-unsigned-long
const __tbToUnsignedLong = value =>
  value === undefined ? 0 : (__tbNumberCtor(value) >>> 0);
// https://webidl.spec.whatwg.org/#es-long
const __tbToLong = value => {
  const n = __tbNumberCtor(value);
  return Number.isNaN(n) ? 0 : Math.trunc(n);
};
// https://webidl.spec.whatwg.org/#es-interface
const __tbEventView = value => {
  if (value === undefined) return null;
  if (value === null || __tbIsWindow(value)) return value;
  throw new TypeError("member view is not of type 'Window'.");
};
const __tbUIEventConstructor = globalThis.UIEvent = class UIEvent extends Event {
  constructor(type, init) {
    if (arguments.length < 1) {
      throw new TypeError("Failed to construct 'UIEvent': 1 argument required, but only 0 present.");
    }
    init = init || {};
    const view = __tbEventView(init.view);
    const event = __tbConstruct(__tbEventConstructor, [type, init], new.target);
    __tbUIEventData.set(event, {
      view,
      detail: init.detail || 0,
      // https://w3c.github.io/uievents/#dom-uievent-which
      which: __tbToUnsignedLong(init.which),
    });
    return event;
  }
  // https://w3c.github.io/uievents/#dom-uievent-inituievent
  initUIEvent(type, bubbles, cancelable, view, detail) {
    if (host.__tbEventDispatching(this)) return;
    __tbEventConstructor.prototype.initEvent.call(this, type, bubbles, cancelable);
    __tbUIEventData.set(this, {
      view: __tbEventView(view),
      detail: __tbToLong(detail),
      which: 0,
    });
  }
  get view() { return __tbEventEntry(this, __tbUIEventData, __tbFreshUIEvent).view; }
  get detail() { return __tbEventEntry(this, __tbUIEventData, __tbFreshUIEvent).detail; }
  get which() { return __tbEventEntry(this, __tbUIEventData, __tbFreshUIEvent).which; }
  // https://github.com/w3c/uievents/pull/413
  get pseudoTarget() { return null; }
};
Object.defineProperty(globalThis.UIEvent.prototype, Symbol.toStringTag, { value: 'UIEvent', writable: false, enumerable: false, configurable: true });

const __tbMouseEventData = host.slots('tinybrowser.mouseevent.data');
// https://w3c.github.io/uievents/#dictdef-eventmodifierinit
const __tbModifierKeys = [
  ['Alt', 'altKey'],
  ['AltGraph', 'modifierAltGraph'],
  ['CapsLock', 'modifierCapsLock'],
  ['Control', 'ctrlKey'],
  ['Fn', 'modifierFn'],
  ['FnLock', 'modifierFnLock'],
  ['Hyper', 'modifierHyper'],
  ['Meta', 'metaKey'],
  ['NumLock', 'modifierNumLock'],
  ['ScrollLock', 'modifierScrollLock'],
  ['Shift', 'shiftKey'],
  ['Super', 'modifierSuper'],
  ['Symbol', 'modifierSymbol'],
  ['SymbolLock', 'modifierSymbolLock'],
];
const __tbModifierDefaults = () =>
  globalThis.Object.fromEntries(__tbModifierKeys.map(([, field]) => [field, false]));
const __tbModifiersFromInit = init =>
  globalThis.Object.fromEntries(__tbModifierKeys.map(([, field]) => [field, !!init[field]]));
const __tbModifiersFromList = list => {
  const names = list.split(/[,\s]+/);
  return globalThis.Object.fromEntries(
    __tbModifierKeys.map(([key, field]) => [field, names.includes(key)]));
};
// https://w3c.github.io/uievents/#dom-keyboardevent-getmodifierstate
const __tbModifierState = (entry, key) => {
  const name = __tbStringCtor(key);
  const row = __tbModifierKeys.find(([known]) => known === name);
  return row === undefined ? false : !!entry[row[1]];
};
const __tbFreshMouseEvent = () => ({
  screenX: 0, screenY: 0, clientX: 0, clientY: 0,
  ...__tbModifierDefaults(),
  button: 0, buttons: 0, relatedTarget: null,
});
// https://w3c.github.io/uievents/#dom-mouseevent-offsetx
const __tbPaddingEdge = event => {
  const target = event.target;
  if (target === null || target === undefined || target.nodeType !== 1) return null;
  const rect = target.getBoundingClientRect();
  const style = globalThis.getComputedStyle(target);
  const pixels = value => {
    const number = globalThis.parseFloat(value);
    return globalThis.Number.isNaN(number) ? 0 : number;
  };
  return [rect.left + pixels(style.borderLeftWidth), rect.top + pixels(style.borderTopWidth)];
};
const __tbMouseEventConstructor = globalThis.MouseEvent = class MouseEvent extends UIEvent {
  constructor(type, init) {
    if (arguments.length < 1) {
      throw new TypeError("Failed to construct 'MouseEvent': 1 argument required, but only 0 present.");
    }
    init = init || {};
    const event = __tbConstruct(__tbUIEventConstructor, [type, init], new.target);
    // https://w3c.github.io/uievents/#dom-mouseevent-clientx
    const button = init.button === undefined ? 0 : __tbToLong(init.button);
    __tbMouseEventData.set(event, {
        screenX: init.screenX === undefined ? 0 : __tbToLong(init.screenX),
        screenY: init.screenY === undefined ? 0 : __tbToLong(init.screenY),
        clientX: init.clientX === undefined ? 0 : __tbToLong(init.clientX),
        clientY: init.clientY === undefined ? 0 : __tbToLong(init.clientY),
        ...__tbModifiersFromInit(init),
        button: button, buttons: init.buttons === undefined ? 0 : __tbToLong(init.buttons),
        relatedTarget: init.relatedTarget || null,
    });
    return event;
  }
  // https://w3c.github.io/uievents/#dom-mouseevent-initmouseevent
  initMouseEvent(type, bubbles, cancelable, view, detail, screenX, screenY,
      clientX, clientY, ctrlKey, altKey, shiftKey, metaKey, button,
      relatedTarget) {
    if (host.__tbEventDispatching(this)) return;
    __tbEventConstructor.prototype.initEvent.call(this, type, bubbles, cancelable);
    const buttonValue = __tbToLong(button);
    __tbUIEventData.set(this, {
      view: __tbEventView(view),
      detail: __tbToLong(detail),
      which: buttonValue,
    });
    __tbMouseEventData.set(this, {
      screenX: __tbToLong(screenX), screenY: __tbToLong(screenY),
      clientX: __tbToLong(clientX), clientY: __tbToLong(clientY),
      ...__tbModifiersFromInit({ ctrlKey, altKey, shiftKey, metaKey }),
      button: buttonValue, buttons: 0,
      relatedTarget: relatedTarget === undefined ? null : relatedTarget,
    });
  }
  get screenX() { return __tbEventEntry(this, __tbMouseEventData, __tbFreshMouseEvent).screenX; }
  get screenY() { return __tbEventEntry(this, __tbMouseEventData, __tbFreshMouseEvent).screenY; }
  get clientX() { return __tbEventEntry(this, __tbMouseEventData, __tbFreshMouseEvent).clientX; }
  get clientY() { return __tbEventEntry(this, __tbMouseEventData, __tbFreshMouseEvent).clientY; }
  // https://w3c.github.io/uievents/#dom-mouseevent-x
  get x() { return __tbEventEntry(this, __tbMouseEventData, __tbFreshMouseEvent).clientX; }
  get y() { return __tbEventEntry(this, __tbMouseEventData, __tbFreshMouseEvent).clientY; }
  // https://drafts.csswg.org/cssom-view/#dom-mouseevent-pagex
  get pageX() {
    return __tbEventEntry(this, __tbMouseEventData, __tbFreshMouseEvent).clientX + globalThis.scrollX;
  }
  get pageY() {
    return __tbEventEntry(this, __tbMouseEventData, __tbFreshMouseEvent).clientY + globalThis.scrollY;
  }
  // https://w3c.github.io/uievents/#dom-mouseevent-offsetx
  get offsetX() {
    const edge = __tbPaddingEdge(this);
    const clientX = __tbEventEntry(this, __tbMouseEventData, __tbFreshMouseEvent).clientX;
    return edge === null ? clientX : clientX - edge[0];
  }
  get offsetY() {
    const edge = __tbPaddingEdge(this);
    const clientY = __tbEventEntry(this, __tbMouseEventData, __tbFreshMouseEvent).clientY;
    return edge === null ? clientY : clientY - edge[1];
  }
  get ctrlKey() { return __tbEventEntry(this, __tbMouseEventData, __tbFreshMouseEvent).ctrlKey; }
  get shiftKey() { return __tbEventEntry(this, __tbMouseEventData, __tbFreshMouseEvent).shiftKey; }
  get altKey() { return __tbEventEntry(this, __tbMouseEventData, __tbFreshMouseEvent).altKey; }
  get metaKey() { return __tbEventEntry(this, __tbMouseEventData, __tbFreshMouseEvent).metaKey; }
  get button() { return __tbEventEntry(this, __tbMouseEventData, __tbFreshMouseEvent).button; }
  get buttons() { return __tbEventEntry(this, __tbMouseEventData, __tbFreshMouseEvent).buttons; }
  get relatedTarget() { return __tbEventEntry(this, __tbMouseEventData, __tbFreshMouseEvent).relatedTarget; }
  getModifierState(key) {
    return __tbModifierState(__tbEventEntry(this, __tbMouseEventData, __tbFreshMouseEvent), key);
  }
};
Object.defineProperty(globalThis.MouseEvent.prototype, Symbol.toStringTag, { value: 'MouseEvent', writable: false, enumerable: false, configurable: true });

const __tbPointerEventData = host.slots('tinybrowser.pointerevent.data');
globalThis.PointerEvent = class PointerEvent extends MouseEvent {
  constructor(type, init) {
    if (arguments.length < 1) {
      throw new TypeError("Failed to construct 'PointerEvent': 1 argument required, but only 0 present.");
    }
    init = init || {};
    const event = __tbConstruct(__tbMouseEventConstructor, [type, init], new.target);
    __tbPointerEventData.set(event, {
        pointerId: init.pointerId === undefined ? 0 : init.pointerId,
        // https://w3c.github.io/pointerevents/#dom-pointerevent-width
        width: init.width === undefined ? 1 : __tbNumberCtor(init.width),
        height: init.height === undefined ? 1 : __tbNumberCtor(init.height),
        pressure: init.pressure === undefined ? 0 : init.pressure,
        tangentialPressure: init.tangentialPressure || 0,
        tiltX: init.tiltX || 0, tiltY: init.tiltY || 0, twist: init.twist || 0,
        pointerType: init.pointerType === undefined ? '' : init.pointerType,
        isPrimary: init.isPrimary === undefined ? false : !!init.isPrimary,
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

// https://html.spec.whatwg.org/multipage/interaction.html#dom-click
// https://w3c.github.io/pointerevents/#dom-pointerevent-initpointerevent
host.__tbInitClickPointer = event => {
  __tbPointerEventData.set(event, {
    pointerId: -1, width: 1, height: 1, pressure: 0, tangentialPressure: 0,
    tiltX: 0, tiltY: 0, twist: 0, pointerType: '', isPrimary: false,
  });
};

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
  ...__tbModifierDefaults(),
  repeat: false, isComposing: false, keyCode: 0, charCode: 0,
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
        ...__tbModifiersFromInit(init),
        repeat: !!init.repeat, isComposing: !!init.isComposing,
        keyCode: init.keyCode === undefined ? 0 : __tbToLong(init.keyCode),
        charCode: init.charCode === undefined ? 0 : __tbToLong(init.charCode),
    });
    return event;
  }
  // https://www.w3.org/TR/DOM-Level-3-Events/#dom-keyboardevent-initkeyboardevent
  initKeyboardEvent(type, bubbles, cancelable, view, keyIdentifier, location,
      modifiersList, repeat, _locale) {
    if (host.__tbEventDispatching(this)) return;
    __tbEventConstructor.prototype.initEvent.call(this, type, bubbles, cancelable);
    const modifiers = modifiersList === undefined || modifiersList === null
      ? '' : __tbStringCtor(modifiersList);
    __tbUIEventData.set(this, { view: __tbEventView(view), detail: 0, which: 0 });
    __tbKeyboardEventData.set(this, {
      key: keyIdentifier === undefined ? '' : __tbStringCtor(keyIdentifier),
      code: '',
      location: __tbToLong(location),
      ...__tbModifiersFromList(modifiers),
      repeat: !!repeat, isComposing: false,
      keyCode: 0, charCode: 0,
    });
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
  getModifierState(key) {
    return __tbModifierState(__tbEventEntry(this, __tbKeyboardEventData, __tbFreshKeyboardEvent), key);
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

// https://dom.spec.whatwg.org/#dom-document-createevent
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
  // https://webidl.spec.whatwg.org/#interface-object
  __tbDefineProperty(globalThis, name, {
    __proto__: null,
    value: ctor, writable: true, enumerable: false, configurable: true,
  });
}
// https://w3c.github.io/uievents/
// https://w3c.github.io/touch-events/
const __tbBeforeUnloadEventData = host.slots('tinybrowser.beforeunloadevent.data');
__tbExposeEvent('BeforeUnloadEvent', __tbEventConstructor, __tbBeforeUnloadEventData,
  () => ({ returnValue: '' }),
  init => ({ returnValue: __tbEventString(init.returnValue) }),
  ['returnValue']);
// https://html.spec.whatwg.org/multipage/browsing-the-web.html#the-beforeunloadevent-interface
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
// https://html.spec.whatwg.org/multipage/popover.html#the-toggleevent-interface
const __tbToggleEventData = host.slots('tinybrowser.toggleevent.data');
__tbExposeEvent('ToggleEvent', __tbEventConstructor, __tbToggleEventData,
  () => ({ oldState: '', newState: '', source: null }),
  init => ({
    oldState: __tbEventString(init.oldState),
    newState: __tbEventString(init.newState),
    source: init.source === undefined ? null : init.source,
  }),
  ['oldState', 'newState']);
// https://html.spec.whatwg.org/multipage/popover.html#dom-toggleevent-source
// https://dom.spec.whatwg.org/#retarget
const __tbGetRootNode = Node.prototype.getRootNode;
function __tbRetarget(a, b) {
  while (a !== null && typeof a === 'object' && typeof a.nodeType === 'number') {
    const root = __tbGetRootNode.call(a);
    const host = root === null ? undefined : root.host;
    if (host === undefined || host === null) return a;
    if (b === null || typeof b !== 'object' || typeof b.nodeType !== 'number') return a;
    const bRoot = __tbGetRootNode.call(b);
    const bHost = bRoot === null ? undefined : bRoot.host;
    if (bHost === undefined || bHost === null) return a;
    a = host;
  }
  return a;
}
__tbDefineProperty(globalThis.ToggleEvent.prototype, 'source', {
  __proto__: null,
  get() {
    const entry = __tbEventEntry(this, __tbToggleEventData,
      () => ({ oldState: '', newState: '', source: null }));
    return __tbRetarget(entry.source, this.target);
  },
  enumerable: true, configurable: true,
});
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
const __tbFocusEventData = host.slots('tinybrowser.focusevent.data');
const __tbFreshFocusEvent = () => ({ relatedTarget: null });
__tbExposeEvent('FocusEvent', __tbUIEventConstructor, __tbFocusEventData,
  __tbFreshFocusEvent,
  init => ({ relatedTarget: init.relatedTarget || null }),
  ['relatedTarget']);
const __tbFocusEventPrototype = globalThis.FocusEvent.prototype;
__tbExposeEvent('TextEvent', __tbUIEventConstructor, host.slots('tinybrowser.textevent.data'),
  () => ({ data: '' }),
  init => ({ data: __tbEventString(init.data) }),
  ['data']);
const __tbTouchEventData = host.slots('tinybrowser.touchevent.data');
const __tbFreshTouchEvent = () => ({
  touches: [], targetTouches: [], changedTouches: [], ...__tbModifierDefaults(),
});
__tbExposeEvent('TouchEvent', __tbUIEventConstructor, __tbTouchEventData,
  __tbFreshTouchEvent,
  init => ({
    touches: init.touches || [],
    targetTouches: init.targetTouches || [],
    changedTouches: init.changedTouches || [],
    ...__tbModifiersFromInit(init),
  }),
  ['touches', 'targetTouches', 'changedTouches', 'altKey', 'metaKey', 'ctrlKey', 'shiftKey']);
// https://w3c.github.io/uievents/#dom-touchevent-getmodifierstate
globalThis.TouchEvent.prototype.getModifierState = function (key) {
  return __tbModifierState(__tbEventEntry(this, __tbTouchEventData, __tbFreshTouchEvent), key);
};
__tbExposeEvent('DragEvent', __tbMouseEventConstructor, host.slots('tinybrowser.dragevent.data'),
  () => ({ dataTransfer: null }),
  init => ({ dataTransfer: init.dataTransfer || null }),
  ['dataTransfer']);

// https://dom.spec.whatwg.org/#concept-event-dispatch
const __tbRelatedTargetSlots = [
  [__tbMouseEventData, __tbFreshMouseEvent, __tbMouseEventConstructor.prototype],
  [__tbFocusEventData, __tbFreshFocusEvent, __tbFocusEventPrototype],
];
host.__tbRelatedTargetGet = event => {
  for (const [data] of __tbRelatedTargetSlots) {
    const entry = data.get(event);
    if (entry !== undefined) return entry.relatedTarget;
  }
  return null;
};
host.__tbRelatedTargetSet = (event, value) => {
  for (const [data] of __tbRelatedTargetSlots) {
    const entry = data.get(event);
    if (entry !== undefined) {
      entry.relatedTarget = value;
      return;
    }
  }
};
host.__tbInitRelatedTarget = (event, value) => {
  for (const [data, fresh, prototype] of __tbRelatedTargetSlots) {
    if (prototype.isPrototypeOf(event)) {
      __tbEventEntry(event, data, fresh).relatedTarget = value;
      return;
    }
  }
};
// https://drafts.csswg.org/css-animations-1/#interface-animationevent
// https://drafts.csswg.org/css-transitions-1/#interface-transitionevent
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

// https://w3c.github.io/touch-events/#extensions-to-the-globaleventhandlers-mixin
if (typeof Document !== 'undefined') {
  for (const name of ['ontouchstart', 'ontouchend', 'ontouchmove', 'ontouchcancel']) {
    Object.defineProperty(Document.prototype, name, {
      get() { return host.__tbGetNodeHandler(this, name); },
      set(value) { host.__tbSetNodeHandler(this, name, value); },
      enumerable: true, configurable: true,
    });
  }
}
