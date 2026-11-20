// https://html.spec.whatwg.org/multipage/web-messaging.html#messageevent
const __tbMessageEventData = host.slots('tinybrowser.messageevent.data');
const __tbFreshMessageEvent = () => ({
  data: null, origin: '', lastEventId: '', source: null, ports: __tbFreeze([]),
});
const __tbMessageEventConstructor = globalThis.MessageEvent = class MessageEvent extends Event {
  constructor(type, init) {
    const eventInit = init === undefined ? {} : Object(init);
    const event = __tbConstruct(__tbEventConstructor, [String(type), eventInit], new.target);
    __tbMessageEventData.set(event, {
        data: __tbApply(__tbHasOwnProperty, eventInit, ['data']) ? eventInit.data : null,
        origin: eventInit.origin === undefined ? '' : String(eventInit.origin),
        lastEventId: eventInit.lastEventId === undefined ? '' : String(eventInit.lastEventId),
        source: eventInit.source === undefined ? null : eventInit.source,
        ports: eventInit.ports === undefined ? __tbFreeze([]) : __tbFreeze(__tbArrayFrom(eventInit.ports)),
    });
    return event;
  }
  get data() { return __tbEventEntry(this, __tbMessageEventData, __tbFreshMessageEvent).data; }
  get origin() { return __tbEventEntry(this, __tbMessageEventData, __tbFreshMessageEvent).origin; }
  get lastEventId() { return __tbEventEntry(this, __tbMessageEventData, __tbFreshMessageEvent).lastEventId; }
  get source() { return __tbEventEntry(this, __tbMessageEventData, __tbFreshMessageEvent).source; }
  get ports() { return __tbEventEntry(this, __tbMessageEventData, __tbFreshMessageEvent).ports; }
};
Object.defineProperty(globalThis.MessageEvent.prototype, Symbol.toStringTag, { value: 'MessageEvent', writable: false, enumerable: false, configurable: true });
// https://html.spec.whatwg.org/multipage/structured-data.html#structuredserializeinternal
const __tbPayloadVersion = 'tb1:';
const __tbPlatform = host.slots('tinybrowser.platform');
const __tbWindowData = host.slots('tinybrowser.window.data');
const __tbPortData = host.slots('tinybrowser.messageport.data');
const __tbFrameId = host.__tb_frameId;
__tbWindowData.set(globalThis, { frame: __tbFrameId });
// https://html.spec.whatwg.org/multipage/nav-history-apis.html#window
const __tbIsWindow = value =>
  value === globalThis ||
  (value !== null && typeof value === 'object' && __tbWindowData.get(value) !== undefined);
const __tbFrameProxies = Object.create(null);
// https://html.spec.whatwg.org/multipage/window-object.html#windowproxy-set
const __tbFramePendingSets = Object.create(null);
const __tbFramePending = frame => {
  let pending = __tbFramePendingSets[frame];
  if (pending === undefined) {
    pending = __tbObjectCreate(null);
    __tbFramePendingSets[frame] = pending;
  }
  return pending;
};
// https://html.spec.whatwg.org/multipage/window-object.html#windowproxy-get
// https://dom.spec.whatwg.org/#interface-eventtarget
const __tbFramePendingCalls = Object.create(null);
// https://html.spec.whatwg.org/multipage/browsers.html#the-origin-interface
host.__tbOriginOfMessageEvent = event => {
  const entry = __tbMessageEventData.get(event);
  if (entry === undefined) return undefined;
  if (entry.source === null) return null;
  return entry.origin;
};

// https://html.spec.whatwg.org/multipage/browsers.html#the-origin-interface
host.__tbWindowProxyAccess = value => {
  if (value === null || (typeof value !== 'object' && typeof value !== 'function')) {
    return undefined;
  }
  // https://html.spec.whatwg.org/multipage/browsers.html#the-origin-interface
  if (value === globalThis) return undefined;
  const entry = __tbWindowData.get(value);
  if (entry === undefined) return undefined;
  return host.__tbFrameSameOrigin(entry.frame) ? 'same' : 'cross';
};

// https://html.spec.whatwg.org/multipage/window-object.html#windowproxy-get
const __tbFrameForwardedNamespaces = {
  __proto__: null,
  Math: true, JSON: true, Reflect: true, Atomics: true, Intl: true,
  console: true,
};
const __tbFrameIntrinsics = {
  __proto__: null,
  Object: true, Function: true, Array: true, String: true, Number: true,
  Boolean: true, Symbol: true, BigInt: true, Date: true, RegExp: true,
  Error: true, TypeError: true, RangeError: true, SyntaxError: true,
  ReferenceError: true, EvalError: true, URIError: true, AggregateError: true,
  Promise: true, Map: true, Set: true, WeakMap: true, WeakSet: true,
  WeakRef: true, FinalizationRegistry: true, Proxy: true,
  ArrayBuffer: true, SharedArrayBuffer: true, DataView: true,
  Int8Array: true, Uint8Array: true, Uint8ClampedArray: true, Int16Array: true,
  Uint16Array: true, Int32Array: true, Uint32Array: true, Float32Array: true,
  Float64Array: true, BigInt64Array: true, BigUint64Array: true,
};
// https://cookiestore.spec.whatwg.org/#cookie-store
const __tbFrameForwardedState = {
  __proto__: null,
  cookieStore: true,
};
const __tbFrameOwnMembers = {
  __proto__: null,
  location: true, name: true, document: true, parent: true, top: true,
  window: true, self: true, frames: true, length: true, close: true,
  closed: true, postMessage: true, opener: true,
  addEventListener: true, removeEventListener: true, dispatchEvent: true,
};
const __tbFrameForwarded = (property, value) => {
  if (typeof property !== 'string' || __tbFrameOwnMembers[property] === true) return false;
  if (value === undefined) return false;
  if (typeof value !== 'function' || value.prototype === undefined) {
    return __tbFrameForwardedNamespaces[property] === true ||
        __tbFrameForwardedState[property] === true;
  }
  if (__tbFrameIntrinsics[property] === true) return true;
  // https://webidl.spec.whatwg.org/#es-interfaces
  return value.prototype[__tbSymbolToStringTag] !== undefined;
};
// https://html.spec.whatwg.org/multipage/system-state.html#the-navigator-object
const __tbNavigatorBrand = host.slots('webidl.Navigator');
const __tbFrameNavigators = new __tbPrivateMap();
const __tbSymbolToStringTag = Symbol.toStringTag;
const __tbFrameNavigator = frame => {
  const existing = __tbFrameNavigators.get(frame);
  if (existing !== undefined) return existing;
  const navigator = __tbObjectCreate(globalThis.Navigator.prototype);
  __tbNavigatorBrand.set(navigator, { __proto__: null, Uint8Array: __tbUint8Array });
  __tbFrameNavigators.set(frame, navigator);
  return navigator;
};
host.__tbFlushFrameSets = function(frame) {
  const pending = __tbFramePendingSets[frame];
  const calls = __tbFramePendingCalls[frame];
  if (pending === undefined && calls === undefined) return false;
  const target = host.__tbFrameGlobal(frame);
  // https://html.spec.whatwg.org/multipage/window-object.html#windowproxy-set
  if (target == null) return false;
  delete __tbFramePendingCalls[frame];
  for (const key of pending === undefined ? [] : Reflect.ownKeys(pending)) {
    if (key === '__proto__' || typeof key === 'symbol') {
      Object.defineProperty(target, key, {
        value: pending[key], writable: true, enumerable: true, configurable: true,
      });
    } else {
      target[key] = pending[key];
    }
  }
  if (calls !== undefined) {
    for (const [hostName, args] of calls) {
      __tbApply(host[hostName], undefined, [frame, ...args]);
    }
  }
  return true;
};

const __tbCloneFailure = () => new globalThis.DOMException('The object could not be cloned.', 'DataCloneError');
const __tbBase64Bytes = text => {
  const binary = globalThis.atob(text);
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index++) bytes[index] = binary.charCodeAt(index);
  return bytes;
};
const __tbProbe = (read, value) => {
  try { read(value); return true; } catch (error) { return false; }
};
const __tbIsDate = value => __tbProbe(candidate => Date.prototype.getTime.call(candidate), value);
const __tbIsRegExp = value => __tbProbe(candidate => Object.getOwnPropertyDescriptor(RegExp.prototype, 'source').get.call(candidate), value);
const __tbIsMap = value => __tbProbe(candidate => Object.getOwnPropertyDescriptor(Map.prototype, 'size').get.call(candidate), value);
const __tbIsSet = value => __tbProbe(candidate => Object.getOwnPropertyDescriptor(Set.prototype, 'size').get.call(candidate), value);
const __tbIsArrayBuffer = value => __tbProbe(candidate => Object.getOwnPropertyDescriptor(ArrayBuffer.prototype, 'byteLength').get.call(candidate), value);
const __tbIsDataView = value => __tbProbe(candidate => Object.getOwnPropertyDescriptor(DataView.prototype, 'byteLength').get.call(candidate), value);
const __tbTypedArrayPrototype = Object.getPrototypeOf(Uint8Array.prototype);
const __tbIsTypedArray = value => __tbProbe(candidate => Object.getOwnPropertyDescriptor(__tbTypedArrayPrototype, 'length').get.call(candidate), value);
const __tbIsBoxedBoolean = value => __tbProbe(candidate => Boolean.prototype.valueOf.call(candidate), value);
const __tbIsBoxedNumber = value => __tbProbe(candidate => Number.prototype.valueOf.call(candidate), value);
const __tbIsBoxedString = value => __tbProbe(candidate => String.prototype.valueOf.call(candidate), value);
const __tbIsBoxedBigInt = value => typeof BigInt === 'function'
  && __tbProbe(candidate => BigInt.prototype.valueOf.call(candidate), value);
// https://html.spec.whatwg.org/multipage/structured-data.html#serializable-objects
const __tbImageDataPrototype = typeof globalThis.ImageData === 'function'
  ? globalThis.ImageData.prototype
  : null;
// https://html.spec.whatwg.org/multipage/structured-data.html#structureddeserialize
const __tbHasPrototype = (value, prototype) => {
  if (value === null || (typeof value !== 'object' && typeof value !== 'function')) {
    return false;
  }
  for (let object = Object.getPrototypeOf(value); object !== null; object = Object.getPrototypeOf(object)) {
    if (object === prototype) return true;
  }
  return false;
};
const __tbIsImageData = value => __tbImageDataPrototype !== null
  && __tbHasPrototype(value, __tbImageDataPrototype);
const __tbImageBitmapPrototype = typeof globalThis.ImageBitmap === 'function'
  ? globalThis.ImageBitmap.prototype
  : null;
const __tbIsImageBitmap = value => __tbImageBitmapPrototype !== null
  && __tbHasPrototype(value, __tbImageBitmapPrototype)
  && typeof value.__tbBitmap === 'number';
// https://html.spec.whatwg.org/multipage/structured-data.html#structureddeserialize
const __tbCloneIntrinsic = name =>
  typeof globalThis[name] === 'function' ? globalThis[name] : undefined;
const __tbBlobCtor = __tbCloneIntrinsic('Blob');
const __tbFileCtor = __tbCloneIntrinsic('File');
const __tbImageDataCtor = __tbCloneIntrinsic('ImageData');
const __tbOffscreenCanvasCtor = __tbCloneIntrinsic('OffscreenCanvas');
const __tbDomExceptionCtor = __tbCloneIntrinsic('DOMException');
const __tbUint8ClampedCtor = globalThis.Uint8ClampedArray;
const __tbViewCtors = Object.create(null);
for (const name of [
  'Int8Array', 'Uint8Array', 'Uint8ClampedArray', 'Int16Array', 'Uint16Array',
  'Int32Array', 'Uint32Array', 'Float32Array', 'Float64Array',
  'BigInt64Array', 'BigUint64Array',
]) {
  if (typeof globalThis[name] === 'function') __tbViewCtors[name] = globalThis[name];
}
if (typeof globalThis.Float16Array === 'function') {
  __tbViewCtors.Float16Array = globalThis.Float16Array;
}
const __tbErrorCtors = Object.create(null);
for (const name of [
  'Error', 'EvalError', 'RangeError', 'ReferenceError', 'SyntaxError',
  'TypeError', 'URIError', 'AggregateError',
]) {
  if (typeof globalThis[name] === 'function') __tbErrorCtors[name] = globalThis[name];
}
const __tbIsArrayIndex = key => {
  const number = Number(key);
  return Number.isInteger(number) && number >= 0 && number < 4294967295
    && String(number) === key;
};
// https://html.spec.whatwg.org/multipage/structured-data.html#structuredserializeinternal
const __tbIsError = value => Object.prototype.toString.call(value) === '[object Error]'
  && value[Symbol.toStringTag] === undefined;
const __tbIsWeakMap = value => __tbProbe(candidate => WeakMap.prototype.has.call(candidate, candidate), value);
const __tbIsWeakSet = value => __tbProbe(candidate => WeakSet.prototype.has.call(candidate, candidate), value);
const __tbIsPromise = value => typeof Promise === 'function' && Object.prototype.toString.call(value) === '[object Promise]';
const __tbIsWeakRef = value => typeof WeakRef === 'function' && Object.prototype.toString.call(value) === '[object WeakRef]';
const __tbHasInternalSlots = value => __tbIsPromise(value) || __tbIsWeakMap(value)
  || __tbIsWeakSet(value) || __tbIsWeakRef(value);

const __tbNumberWire = value => {
  if (value !== value) return 'NaN';
  if (value === Infinity) return 'Infinity';
  if (value === -Infinity) return '-Infinity';
  if (value === 0 && 1 / value === -Infinity) return '-0';
  return value;
};
const __tbNumberValue = wire => wire === 'NaN' ? NaN
  : wire === 'Infinity' ? Infinity
  : wire === '-Infinity' ? -Infinity
  : wire === '-0' ? -0
  : wire;

// https://webidl.spec.whatwg.org/#es-sequence
const __tbTransferList = transfer => {
  if (transfer === undefined) return [];
  if (transfer === null || typeof transfer !== 'object' || typeof transfer[Symbol.iterator] !== 'function') {
    throw new TypeError('The transfer list must be an iterable object');
  }
  const items = Array.from(transfer);
  // https://webidl.spec.whatwg.org/#es-object
  for (const item of items) {
    if (item === null || (typeof item !== 'object' && typeof item !== 'function')) {
      throw new TypeError('The transfer list must contain only objects');
    }
  }
  return items;
};

// https://webidl.spec.whatwg.org/#dfn-overload-resolution-algorithm
const __tbPostMessageTransfer = transfer => {
  if (transfer !== null && typeof transfer === 'object'
      && typeof transfer[Symbol.iterator] === 'function') {
    return transfer;
  }
  if (transfer === null || transfer === undefined) return undefined;
  return transfer.transfer;
};

const __tbStartStreamTransfer = (stream, port) => {
  let reader;
  try {
    reader = stream.getReader();
  } catch (error) {
    throw __tbCloneFailure();
  }
  let busy = false;
  let closed = false;
  const finish = (kind, value) => {
    if (closed) return;
    closed = true;
    try { port.postMessage([kind, value]); } catch (error) { /* the peer is gone */ }
    port.close();
  };
  port.onmessage = event => {
    const message = event.data;
    if (message === 'pull') {
      if (busy || closed) return;
      busy = true;
      reader.read().then(step => {
        busy = false;
        if (step.done) {
          finish('close');
        } else {
          try {
            port.postMessage(['chunk', step.value]);
          } catch (error) {
            finish('error', error);
          }
        }
      }, error => {
        busy = false;
        finish('error', error);
      });
    } else if (__tbIsArray(message) && message[0] === 'cancel') {
      if (closed) return;
      closed = true;
      reader.cancel(message[1]).catch(() => {});
      port.close();
    }
  };
};

// https://html.spec.whatwg.org/multipage/structured-data.html#structuredserializewithtransfer
const __tbEncode = (value, transfer, sourcePort) => {
  // https://html.spec.whatwg.org/multipage/structured-data.html#structuredserializewithtransfer
  const buffers = new __tbPrivateMap();
  const transferred = new __tbPrivateMap();
  const streams = new __tbPrivateMap();
  const pendingStreams = __tbPrivateArray();
  const canvases = new __tbPrivateMap();
  const transferredBitmaps = new __tbPrivateSet();
  const bitmapDescriptors = new __tbPrivateMap();
  for (const item of __tbTransferList(transfer)) {
    if (item instanceof ArrayBuffer) {
      if (buffers.has(item)) throw new globalThis.DOMException('Transfer list contains duplicate buffers', 'DataCloneError');
      if (typeof item.transfer !== 'function') throw new globalThis.DOMException('The buffer is not transferable', 'DataCloneError');
      buffers.set(item, -1);
    } else if (item instanceof __tbMessagePortConstructor) {
      if (item === sourcePort) throw new globalThis.DOMException('Cannot transfer the source port', 'DataCloneError');
      const itemData = __tbBrand(item, __tbPortData);
      if (itemData.closed) throw new globalThis.DOMException('Cannot transfer a detached MessagePort', 'DataCloneError');
      if (transferred.has(item)) throw new globalThis.DOMException('Transfer list contains duplicate ports', 'DataCloneError');
      transferred.set(item, itemData.id);
    } else if (typeof globalThis.ReadableStream === 'function'
               && item instanceof globalThis.ReadableStream) {
      // https://streams.spec.whatwg.org/#readable-stream-transfer
      if (item.locked) {
        throw new globalThis.DOMException('Cannot transfer a locked ReadableStream', 'DataCloneError');
      }
      if (streams.has(item)) {
        throw new globalThis.DOMException('Transfer list contains duplicate streams', 'DataCloneError');
      }
      streams.set(item, -1);
    } else if (typeof globalThis.OffscreenCanvas === 'function'
               && item instanceof globalThis.OffscreenCanvas) {
      if (canvases.has(item)) {
        throw new globalThis.DOMException('Transfer list contains duplicate canvases', 'DataCloneError');
      }
      if (host.__tbOffscreenDetached(item)) {
        throw new globalThis.DOMException('Cannot transfer a detached OffscreenCanvas', 'DataCloneError');
      }
      // https://html.spec.whatwg.org/multipage/structured-data.html#structuredserializeinternal
      if (host.__tbOffscreenHasContext(item)) {
        throw new globalThis.DOMException('Cannot transfer an OffscreenCanvas with a context', 'InvalidStateError');
      }
      canvases.set(item, -1);
    } else if (__tbIsImageBitmap(item)) {
      if (transferredBitmaps.has(item)) {
        throw new globalThis.DOMException('Transfer list contains duplicate bitmaps', 'DataCloneError');
      }
      if (host.__tbImageBitmapPixels(item) == null) {
        throw new globalThis.DOMException('Cannot transfer a detached ImageBitmap', 'DataCloneError');
      }
      transferredBitmaps.add(item);
    } else {
      throw new globalThis.DOMException('Value not transferable', 'DataCloneError');
    }
  }
  const nodes = __tbPrivateArray();
  const seen = new __tbPrivateMap();
  const slot = descriptor => {
    if (descriptor !== null) __tbSetPrototypeOf(descriptor, null);
    const index = nodes.length;
    __tbArray.push(nodes, descriptor);
    return index;
  };
  const encode = input => {
    if (input === null) return slot(['null']);
    const kind = typeof input;
    if (kind === 'undefined') return slot(['undefined']);
    if (kind === 'boolean') return slot(['boolean', input]);
    if (kind === 'number') return slot(['number', __tbNumberWire(input)]);
    if (kind === 'string') return slot(['string', input]);
    if (kind === 'bigint') return slot(['bigint', String(input)]);
    if (kind === 'function' || kind === 'symbol') throw __tbCloneFailure();
    if (seen.has(input)) return seen.get(input);
    // https://html.spec.whatwg.org/multipage/structured-data.html#structuredserializeinternal
    if (__tbWindowData.get(input) !== undefined) throw __tbCloneFailure();
    if (transferred.has(input)) {
      const index = slot(['port', transferred.get(input)]);
      seen.set(input, index);
      return index;
    }
    if (typeof globalThis.ReadableStream === 'function'
        && input instanceof globalThis.ReadableStream) {
      // https://streams.spec.whatwg.org/#readable-stream-transfer
      if (!streams.has(input)) throw __tbCloneFailure();
      const channel = new globalThis.MessageChannel();
      const id = __tbBrand(channel.port2, __tbPortData).id;
      const index = slot(['stream', id]);
      transferred.set(channel.port2, id);
      __tbArray.push(pendingStreams, { __proto__: null, stream: input, port: channel.port1 });
      seen.set(input, index);
      return index;
    }
    if (buffers.has(input)) {
      const index = slot(null);
      seen.set(input, index);
      buffers.set(input, index);
      return index;
    }
    if (canvases.has(input)) {
      const index = slot(['offscreen', input.width, input.height]);
      seen.set(input, index);
      canvases.set(input, index);
      return index;
    }
    if (typeof globalThis.OffscreenCanvas === 'function'
        && input instanceof globalThis.OffscreenCanvas) {
      // https://html.spec.whatwg.org/multipage/structured-data.html#structuredserializeinternal
      throw __tbCloneFailure();
    }
    if (__tbBlobData.get(input) !== undefined) {
      const blob = __tbBlobData.get(input);
       const bytes = __tbBase64Encode(blob.bytes);
      let descriptor;
      if (__tbFileData.get(input) !== undefined) {
        const file = __tbFileData.get(input);
        descriptor = ['file', file.name, blob.type, file.lastModified, bytes];
      } else {
        descriptor = ['blob', blob.type, bytes];
      }
      const index = slot(descriptor);
      seen.set(input, index);
      return index;
    }
    // https://w3c.github.io/FileAPI/#filelist-section
    if (__tbFileListData.get(input) !== undefined) {
      const files = __tbFileListData.get(input).files;
      const index = slot(null);
      seen.set(input, index);
      const entries = __tbPrivateArray();
      for (let item = 0; item < files.length; item++) {
        __tbArray.push(entries, encode(files[item]));
      }
      nodes[index] = __tbSetPrototypeOf(['filelist', entries], null);
      return index;
    }
    // https://html.spec.whatwg.org/multipage/structured-data.html#serializable-objects
    if (__tbIsImageData(input)) {
      const index = slot([
        'imagedata', input.width, input.height,
        __tbBase64Encode(new __tbUint8Array(input.data.buffer, input.data.byteOffset, input.data.byteLength)),
      ]);
      seen.set(input, index);
      return index;
    }
    // https://html.spec.whatwg.org/multipage/structured-data.html#serializable-objects
    if (__tbIsImageBitmap(input)) {
      let descriptor = bitmapDescriptors.get(input);
      if (descriptor === undefined) {
        const pixels = host.__tbImageBitmapPixels(input);
        if (pixels === null || pixels === undefined) throw __tbCloneFailure();
        descriptor = [
          'bitmap', input.width, input.height, input.__tbOriginClean === true,
          __tbBase64Encode(new __tbUint8Array(pixels.buffer, pixels.byteOffset, pixels.byteLength)),
        ];
        bitmapDescriptors.set(input, descriptor);
      }
      const index = slot(descriptor);
      seen.set(input, index);
      return index;
    }
    if (__tbIsArrayBuffer(input)) {
      let view;
      try {
        view = new Uint8Array(input);
      } catch (error) {
        throw __tbCloneFailure();
      }
      const index = slot([
        'buffer', __tbBase64Encode(view), input.maxByteLength, input.resizable === true,
      ]);
      seen.set(input, index);
      return index;
    }
    if (typeof SharedArrayBuffer === 'function' && input instanceof SharedArrayBuffer) {
      // https://html.spec.whatwg.org/multipage/structured-data.html#structuredserializeinternal
      if (host.__tbCrossOriginIsolated !== true) throw __tbCloneFailure();
      // https://html.spec.whatwg.org/multipage/structured-data.html#structuredserializeinternal
      const id = host.__tbRegisterSharedBuffer(input);
      if (id === undefined || id === null) throw __tbCloneFailure();
      const growable = input.growable === true;
      const index = slot([
        'shared', id, input.byteLength, growable ? input.maxByteLength : 0,
      ]);
      seen.set(input, index);
      return index;
    }
    // https://tc39.es/ecma262/#sec-arraybuffer.isview
    if (__tbIsArrayBufferView(input)) {
      // https://html.spec.whatwg.org/multipage/structured-data.html#structuredserializeinternal
      if (host.__tbIsOutOfBoundsView(input)) throw __tbCloneFailure();
      const tag = Object.prototype.toString.call(input);
      const kind = tag.slice(8, -1);
      let buffer;
      let byteOffset;
      let byteLength;
      try {
        buffer = input.buffer;
        byteOffset = input.byteOffset;
        byteLength = input.byteLength;
      } catch (error) {
        throw __tbCloneFailure();
      }
      // https://html.spec.whatwg.org/multipage/structured-data.html#structuredserializeinternal
      if (byteOffset + byteLength > buffer.byteLength) throw __tbCloneFailure();
      // https://tc39.es/ecma262/#sec-arraybuffer.prototype.resize
      const tracked = buffer.resizable === true
        && byteOffset === 0 && byteLength === buffer.byteLength;
      const encoded = encode(buffer);
      const descriptor = kind === 'DataView'
        ? ['view', encoded, 'DataView', byteOffset, byteLength, tracked]
        : ['view', encoded, kind, byteOffset, input.length, tracked];
      const index = slot(descriptor);
      seen.set(input, index);
      return index;
    }
    // https://html.spec.whatwg.org/multipage/structured-data.html#serializable-objects
    if (Object.prototype.toString.call(input) === '[object DOMException]') {
      return slot(['domexception', String(input.name), String(input.message)]);
    }
    if (__tbPortData.has(input)) throw __tbCloneFailure();
    for (let object = input; object !== null; object = Object.getPrototypeOf(object)) {
      if (__tbPlatform.has(object)) throw __tbCloneFailure();
    }
    // https://html.spec.whatwg.org/multipage/structured-data.html#structuredserializeinternal
    if (__tbHasInternalSlots(input)) throw __tbCloneFailure();
    if (__tbIsDate(input)) return slot(['date', __tbNumberWire(input.getTime())]);
    if (__tbIsRegExp(input)) return slot(['regexp', input.source, input.flags]);
    if (__tbIsError(input)) {
      const index = slot(null);
      seen.set(input, index);
      // https://html.spec.whatwg.org/multipage/structured-data.html#structuredserializeinternal
      const hasMessage = __tbHasOwnProperty.call(input, 'message');
      const cause = __tbHasOwnProperty.call(input, 'cause') ? encode(input.cause) : null;
      nodes[index] = __tbSetPrototypeOf([
        'error',
        String(input.name),
        hasMessage ? String(input.message) : '',
        hasMessage,
        cause,
      ], null);
      return index;
    }
    if (__tbIsBoxedBoolean(input)) return slot(['boxed', 'Boolean', input.valueOf()]);
    if (__tbIsBoxedNumber(input)) return slot(['boxed', 'Number', __tbNumberWire(input.valueOf())]);
    if (__tbIsBoxedString(input)) return slot(['boxed', 'String', input.valueOf()]);
    if (__tbIsBoxedBigInt(input)) return slot(['boxedbigint', String(input.valueOf())]);
    if (__tbIsMap(input)) {
      const index = slot(null);
      seen.set(input, index);
      const entries = __tbPrivateArray();
      for (const [key, entry] of input) __tbArray.push(entries, __tbSetPrototypeOf([encode(key), encode(entry)], null));
      nodes[index] = __tbSetPrototypeOf(['map', entries], null);
      return index;
    }
    if (__tbIsSet(input)) {
      const index = slot(null);
      seen.set(input, index);
      const entries = __tbPrivateArray();
      for (const entry of input) __tbArray.push(entries, encode(entry));
      nodes[index] = __tbSetPrototypeOf(['set', entries], null);
      return index;
    }
    if (Array.isArray(input)) {
      const index = slot(null);
      seen.set(input, index);
      const items = __tbPrivateArray();
      for (let item = 0; item < input.length; item++) __tbArray.push(items, encode(input[item]));
      // https://html.spec.whatwg.org/multipage/structured-data.html#structuredserializeinternal
      const properties = __tbPrivateArray();
      for (const key of Object.keys(input)) {
        if (__tbIsArrayIndex(key)) continue;
        __tbArray.push(properties, __tbSetPrototypeOf([key, encode(input[key])], null));
      }
      nodes[index] = __tbSetPrototypeOf(['array', items, properties], null);
      return index;
    }
    // https://html.spec.whatwg.org/multipage/structured-data.html#structuredserializeinternal
    const index = slot(null);
    seen.set(input, index);
    const entries = __tbPrivateArray();
    for (const key of Object.keys(input)) __tbArray.push(entries, __tbSetPrototypeOf([key, encode(input[key])], null));
    nodes[index] = __tbSetPrototypeOf(['object', entries], null);
    return index;
  };
  const root = encode(value);
  // https://html.spec.whatwg.org/multipage/structured-data.html#structuredserializewithtransfer
  for (const [buffer, index] of buffers) {
    let moved;
    try {
      moved = buffer.transfer();
    } catch (error) {
      throw new globalThis.DOMException('The buffer is already detached', 'DataCloneError');
    }
    if (index !== -1) {
      nodes[index] = __tbSetPrototypeOf([
        'buffer',
        __tbBase64Encode(new __tbUint8Array(moved)),
        moved.maxByteLength,
        moved.resizable === true,
      ], null);
    }
  }
  for (const [canvas] of canvases) {
    host.__tbOffscreenDetach(canvas);
  }
  // https://html.spec.whatwg.org/multipage/structured-data.html#structuredserializewithtransfer
  for (const bitmap of transferredBitmaps) {
    bitmap.close();
  }
  const ports = __tbPrivateArray();
  for (const [port, id] of transferred) {
    if (!host.__tbPortDetach(id)) throw new globalThis.DOMException('Cannot transfer a detached MessagePort', 'DataCloneError');
    __tbBrand(port, __tbPortData).closed = true;
    delete __tbPorts[id];
    __tbArray.push(ports, id);
  }
  for (let index = 0; index < pendingStreams.length; index++) {
    const pending = pendingStreams[index];
    __tbStartStreamTransfer(pending.stream, pending.port);
  }
  return { __proto__: null, payload: __tbPayloadVersion + __tbJsonStringify({ __proto__: null, root, nodes }), ports };
};

const __tbDecode = (payload, ports) => {
  const text = String(payload);
  if (text.indexOf(__tbPayloadVersion) !== 0) throw new Error('structured clone payload mismatch');
  const wire = __tbJsonParse(text.slice(__tbPayloadVersion.length));
  const nodes = wire.nodes;
  const values = __tbPrivateArray();
  values.length = nodes.length;
  for (let index = 0; index < nodes.length; index++) {
    switch (nodes[index][0]) {
      case 'buffer': {
        const bytes = __tbBase64Bytes(nodes[index][1]);
        const resizable = nodes[index][3] === true;
        const max = typeof nodes[index][2] === 'number' ? nodes[index][2] : bytes.length;
        // https://tc39.es/ecma262/#sec-arraybuffer-constructor
        const buffer = resizable && max > bytes.length
          ? new ArrayBuffer(bytes.length, { maxByteLength: max })
          : resizable
            ? new ArrayBuffer(bytes.length, { maxByteLength: bytes.length })
            : new ArrayBuffer(bytes.length);
        new Uint8Array(buffer).set(bytes);
        values[index] = buffer;
        break;
      }
      case 'shared': {
        // https://html.spec.whatwg.org/multipage/structured-data.html#structureddeserialize
        const buffer = host.__tbCreateSharedBuffer(
          nodes[index][1], nodes[index][2], nodes[index][3]);
        if (buffer === undefined || buffer === null) throw __tbCloneFailure();
        if (buffer.byteLength !== nodes[index][2]) throw __tbCloneFailure();
        if (nodes[index][3] !== 0 && buffer.maxByteLength !== nodes[index][3]) {
          throw __tbCloneFailure();
        }
        values[index] = buffer;
        break;
      }
      case 'object': values[index] = {}; break;
      case 'array': values[index] = new Array(nodes[index][1].length); break;
      case 'map': values[index] = new Map(); break;
      case 'set': values[index] = new Set(); break;
      default: values[index] = undefined; break;
    }
  }
  for (let index = 0; index < nodes.length; index++) {
    const descriptor = nodes[index];
    switch (descriptor[0]) {
      case 'view': {
        const buffer = values[descriptor[1]];
        // https://tc39.es/ecma262/#sec-arraybuffer.prototype.resize
        const tracked = descriptor[5] === true;
        if (descriptor[2] === 'DataView') {
          values[index] = tracked
            ? new DataView(buffer, descriptor[3])
            : new DataView(buffer, descriptor[3], descriptor[4]);
        } else {
          const View = __tbViewCtors[descriptor[2]];
          if (View === undefined) throw new Error(`unknown typed array ${descriptor[2]}`);
          values[index] = tracked
            ? new View(buffer, descriptor[3])
            : new View(buffer, descriptor[3], descriptor[4]);
        }
        break;
      }
      case 'undefined': values[index] = undefined; break;
      case 'null': values[index] = null; break;
      case 'boolean': values[index] = descriptor[1]; break;
      case 'number': values[index] = __tbNumberValue(descriptor[1]); break;
      case 'string': values[index] = descriptor[1]; break;
      case 'bigint': values[index] = BigInt(descriptor[1]); break;
      case 'offscreen':
        values[index] = new __tbOffscreenCanvasCtor(descriptor[1], descriptor[2]);
        break;
      case 'date': values[index] = new Date(__tbNumberValue(descriptor[1])); break;
      case 'regexp': values[index] = new RegExp(descriptor[1], descriptor[2]); break;
      case 'domexception':
        values[index] = new __tbDomExceptionCtor(descriptor[2], descriptor[1]);
        break;
      case 'error': {
        const name = descriptor[1];
        const ctor = __tbErrorCtors[name] ?? __tbErrorCtors.Error;
        values[index] = descriptor[3] ? new ctor(descriptor[2]) : new ctor();
        values[index].name = name;
        break;
      }
      case 'boxed': {
        const ctor = __tbCloneIntrinsic(descriptor[1]);
        values[index] = new ctor(__tbNumberValue(descriptor[2]));
        break;
      }
      case 'boxedbigint': values[index] = Object(BigInt(descriptor[1])); break;
      case 'blob':
        values[index] = new __tbBlobCtor([__tbBase64Bytes(descriptor[2])], { type: descriptor[1] });
        break;
      case 'file':
        values[index] = new __tbFileCtor([__tbBase64Bytes(descriptor[4])], descriptor[1], {
          type: descriptor[2], lastModified: descriptor[3],
        });
        break;
      case 'filelist': {
        break;
      }
      case 'imagedata': {
        const bytes = __tbBase64Bytes(descriptor[3]);
        const data = new __tbUint8ClampedCtor(bytes.length);
        for (let byte = 0; byte < bytes.length; byte++) data[byte] = bytes[byte];
        values[index] = new __tbImageDataCtor(data, descriptor[1], descriptor[2]);
        break;
      }
      case 'bitmap': {
        const bytes = __tbBase64Bytes(descriptor[4]);
        const data = new __tbUint8Array(bytes.length);
        for (let byte = 0; byte < bytes.length; byte++) data[byte] = bytes[byte];
        values[index] = host.__tbImageBitmapFrom(
          descriptor[1], descriptor[2], descriptor[3], data);
        break;
      }
      case 'port': {
        const port = ports[descriptor[1]];
        if (port === undefined) throw new Error('missing transferred MessagePort');
        values[index] = port;
        break;
      }
      case 'stream': {
        const port = ports[descriptor[1]];
        if (port === undefined) throw new Error('missing transferred ReadableStream');
        values[index] = __tbStreamFromPort(port);
        break;
      }
    }
  }
  // https://w3c.github.io/FileAPI/#filelist-section
  for (let index = 0; index < nodes.length; index++) {
    const descriptor = nodes[index];
    if (descriptor[0] !== 'filelist') continue;
    const files = __tbArray.map(descriptor[1], slot => values[slot]);
    values[index] = host.__tbCreateFileList(files);
  }
  for (let index = 0; index < nodes.length; index++) {
    const descriptor = nodes[index];
    switch (descriptor[0]) {
      case 'object': {
        const target = values[index];
        const entries = descriptor[1];
        for (let entry = 0; entry < entries.length; entry++) {
          const key = entries[entry][0];
          const slot = entries[entry][1];
          // https://html.spec.whatwg.org/multipage/structured-data.html#structureddeserialize
          __tbDefineProperty(target, key, {
            __proto__: null,
            value: values[slot], writable: true, enumerable: true, configurable: true,
          });
        }
        break;
      }
      case 'array': {
        const target = values[index];
        for (let item = 0; item < descriptor[1].length; item++) target[item] = values[descriptor[1][item]];
        const properties = descriptor[2] === undefined ? [] : descriptor[2];
        for (let entry = 0; entry < properties.length; entry++) {
          __tbDefineProperty(target, properties[entry][0], {
            __proto__: null,
            value: values[properties[entry][1]],
            writable: true,
            enumerable: true,
            configurable: true,
          });
        }
        break;
      }
      case 'error': {
        if (descriptor[4] !== null && descriptor[4] !== undefined) {
          values[index].cause = values[descriptor[4]];
        }
        break;
      }
      case 'map': {
        const target = values[index];
        const entries = descriptor[1];
        for (let entry = 0; entry < entries.length; entry++) target.set(values[entries[entry][0]], values[entries[entry][1]]);
        break;
      }
      case 'set': {
        const target = values[index];
        for (let entry = 0; entry < descriptor[1].length; entry++) target.add(values[descriptor[1][entry]]);
        break;
      }
    }
  }
  return values[wire.root];
};
// https://html.spec.whatwg.org/multipage/structured-data.html#dom-structuredclone
globalThis.structuredClone = function(value, options) {
  if (options != null && typeof options !== 'object') {
    throw new TypeError('The provided value is not of type StructuredSerializeOptions');
  }
  const transfer = options == null ? undefined : options.transfer;
  const encoded = __tbEncode(value, transfer, null);
  const materialized = __tbMaterializePorts(encoded.ports);
  const clone = __tbDecode(encoded.payload, materialized.ports);
  __tbFirePortCloses(materialized.closes);
  return clone;
};

const __tbPorts = Object.create(null);

// https://html.spec.whatwg.org/multipage/web-messaging.html#disentangle
const __tbMaterializePorts = ids => {
  const ports = { __proto__: null };
  const closes = __tbPrivateArray();
  for (let index = 0; index < ids.length; index++) {
    const id = ids[index];
    const pendingClose = host.__tbPortAdopt(id);
    if (pendingClose == null) throw new Error('missing transferred MessagePort');
    ports[id] = __tbMaterializePort(id);
    if (pendingClose) __tbArray.push(closes, id);
  }
   return { __proto__: null, ports, closes };
};
const __tbFirePortCloses = ids => {
  for (let index = 0; index < ids.length; index++) {
    const id = ids[index];
    const port = __tbPortLookup(id);
    if (port === null) continue;
    const data = __tbBrand(port, __tbPortData);
    data.closed = true;
    host.__tbPortClose(id);
    host.__tbDispatchTargetTrusted(port, new __tbEventConstructor('close'));
  }
};
const __tbDiscardPorts = ids => {
  for (let index = 0; index < ids.length; index++) {
    const id = ids[index];
    const port = __tbPortLookup(id);
    if (port === null) continue;
    __tbBrand(port, __tbPortData).closed = true;
    host.__tbPortClose(id);
  }
};
const __tbMaterializePort = id => {
  if (__tbPorts[id] !== undefined) return __tbPorts[id];
  const port = __tbConstruct(__tbEventTargetConstructor, [], __tbMessagePortConstructor);
  __tbPortData.set(port, { id: id, closed: false, onmessage: null, onclose: null });
  __tbPorts[id] = port;
  return port;
};
// https://webidl.spec.whatwg.org/#idl-event-handler
const __tbEventHandler = value => {
  if (value === null || value === undefined) return null;
  if (typeof value === 'function') return value;
  if (typeof value === 'object' && typeof value.handleEvent === 'function') return value;
  return null;
};
const __tbPortLookup = id => {
  const port = __tbPorts[id];
  return port === undefined ? null : port;
};
// https://html.spec.whatwg.org/multipage/web-messaging.html#messageport
const __tbMessagePortConstructor = globalThis.MessagePort = class MessagePort extends EventTarget {
  constructor() { throw new TypeError('Illegal constructor'); }
  postMessage(message, transfer) {
    const data = __tbBrand(this, __tbPortData);
    // https://webidl.spec.whatwg.org/#overload-resolution
    if (arguments.length < 1) {
      throw new TypeError("Failed to execute 'postMessage' on 'MessagePort': 1 argument required, but only 0 present.");
    }
    // https://html.spec.whatwg.org/multipage/web-messaging.html#message-port-post-message-steps
    const encoded = __tbEncode(message, __tbPostMessageTransfer(transfer), this);
    if (data.closed) return;
    // https://html.spec.whatwg.org/multipage/web-messaging.html#message-port-post-message-steps
    const peer = host.__tbPortPeer(data.id);
    if (peer != null && __tbArray.indexOf(encoded.ports, peer) !== -1) return;
    host.__tbPortPost(data.id, encoded.payload, encoded.ports);
  }
  start() {
    const data = __tbBrand(this, __tbPortData);
    if (data.closed) return;
    host.__tbPortStart(data.id);
  }
  close() {
    const data = __tbBrand(this, __tbPortData);
    if (data.closed) return;
    data.closed = true;
    host.__tbPortClose(data.id);
  }
  get onmessage() { return __tbBrand(this, __tbPortData).onmessage; }
  set onmessage(value) {
    const data = __tbBrand(this, __tbPortData);
    data.onmessage = __tbEventHandler(value);
    // https://html.spec.whatwg.org/multipage/web-messaging.html#message-ports
    if (!data.closed) host.__tbPortStart(data.id);
  }
  get onmessageerror() { return __tbBrand(this, __tbPortData).onmessageerror; }
  set onmessageerror(value) { __tbBrand(this, __tbPortData).onmessageerror = __tbEventHandler(value); }
  get onclose() { return __tbBrand(this, __tbPortData).onclose; }
  set onclose(value) { __tbBrand(this, __tbPortData).onclose = __tbEventHandler(value); }
};
Object.defineProperty(globalThis.MessagePort.prototype, Symbol.toStringTag, { value: 'MessagePort', writable: false, enumerable: false, configurable: true });
// https://html.spec.whatwg.org/multipage/web-messaging.html#messagechannel
globalThis.MessageChannel = class MessageChannel {
  constructor() {
    const pair = host.__tbPortNew();
    const port1 = __tbMaterializePort(pair[0]);
    const port2 = __tbMaterializePort(pair[1]);
    Object.defineProperty(this, 'port1', { value: port1, writable: false, enumerable: true, configurable: true });
    Object.defineProperty(this, 'port2', { value: port2, writable: false, enumerable: true, configurable: true });
  }
};
Object.defineProperty(globalThis.MessageChannel.prototype, Symbol.toStringTag, { value: 'MessageChannel', writable: false, enumerable: false, configurable: true });

// https://html.spec.whatwg.org/multipage/window-object.html#the-windowproxy-exotic-object
// https://html.spec.whatwg.org/multipage/window-object.html#windowproxy-getownproperty
const __tbFrameNamedChild = (frame, name) => {
  const count = host.__tbFrameChildCount(frame);
  for (let index = 0; index < count; index += 1) {
    const child = host.__tbFrameChild(frame, index);
    if (child != null && host.__tbFrameName(child) === name) return child;
  }
  return null;
};
const __tbFrameProxy = frame => {
  if (frame == null) return undefined;
  if (frame === __tbFrameId) return globalThis;
  if (__tbFrameProxies[frame] !== undefined) return __tbFrameProxies[frame];
  let proxy;
  let pendingLocation;
  const crossOrigin = () => {
    throw new globalThis.DOMException('Blocked a frame from accessing a cross-origin frame.', 'SecurityError');
  };
  const crossOriginNoop = function() {};
  // https://html.spec.whatwg.org/multipage/nav-history-apis.html#the-location-interface
  const crossOriginLocation = () => cachedMember('crossOriginLocation', 'frame', () => {
    const location = __tbObjectCreate(null);
    for (const name of ['href', 'protocol', 'host', 'hostname', 'port', 'pathname',
                        'search', 'hash', 'origin', 'toString']) {
      __tbDefineProperty(location, name, {
        get: crossOrigin,
        set: crossOriginNoop,
        configurable: false,
        enumerable: true,
      });
    }
    return location;
  });
  const postMessage = function(message, targetOrigin, transfer) {
    try {
      return __tbPostMessage(frame, arguments.length, message, targetOrigin, transfer);
    } catch (error) {
      if (error instanceof globalThis.DOMException) {
        const global = host.__tbFrameGlobal(frame);
        const constructor = global == null ? undefined : global.DOMException;
        if (typeof constructor === 'function' && error.constructor !== constructor) {
          throw new constructor(error.message, error.name);
        }
      }
      throw error;
    }
  };
  const proxyCache = __tbObjectCreate(null);
  const cachedMember = (key, targetGlobal, make) => {
    const entry = proxyCache[key];
    if (entry !== undefined && entry.global === targetGlobal) return entry.fn;
    const fn = make();
    proxyCache[key] = { global: targetGlobal, fn };
    return fn;
  };
  // https://dom.spec.whatwg.org/#interface-eventtarget
  // https://html.spec.whatwg.org/multipage/window-object.html#the-windowproxy-exotic-object
  const eventTargetMethod = (key, hostName) => cachedMember(key, 'frame', () =>
    function() {
      if (host.__tbFrameGlobal(frame) != null && !host.__tbFrameAwaitingReplay(frame)) {
        return __tbApply(host[hostName], undefined, [frame, ...arguments]);
      }
      // https://html.spec.whatwg.org/multipage/window-object.html#windowproxy-get
      if (key === 'dispatchEvent') {
        return true;
      }
      let calls = __tbFramePendingCalls[frame];
      if (calls === undefined) {
        calls = [];
        __tbFramePendingCalls[frame] = calls;
      }
      calls.push([hostName, __tbArrayFrom(arguments)]);
      return undefined;
    });
  const handler = {
    get(target, property) {
      // https://html.spec.whatwg.org/multipage/browsers.html#crossoriginproperties-(-o-
      switch (property) {
        case 'postMessage':
          return postMessage;
        case 'parent': {
          const parent = host.__tbFrameParent(frame);
          return parent == null ? proxy : __tbFrameProxy(parent);
        }
        case 'top': {
          const top = host.__tbFrameTop(frame);
          return top == null || top === frame ? proxy : __tbFrameProxy(top);
        }
        case 'window': case 'self': case 'frames': return proxy;
        case 'length': return host.__tbFrameChildCount(frame);
        case 'close': return () => host.__tbCloseFrame(frame);
        case 'closed': return !host.__tbFrameRegistered(frame);
        case Symbol.toStringTag: return 'Window';
      }
      const sameOrigin = host.__tbFrameGlobal(frame);
      if (sameOrigin == null) {
        if (host.__tbFrameSameOrigin(frame)) {
          if (property === 'document') return host.__tbFrameDocument(frame);
          const pending = __tbFramePendingSets[frame];
          if (pending !== undefined && __tbApply(__tbHasOwnProperty, pending, [property])) {
            return pending[property];
          }
          // https://html.spec.whatwg.org/multipage/window-object.html#windowproxy-get
          if (property === 'name') return host.__tbFrameName(frame);
          if (property === 'navigator') return __tbFrameNavigator(frame);
          // https://html.spec.whatwg.org/multipage/window-object.html#dom-window-clientinformation
          if (property === 'clientInformation') return __tbFrameNavigator(frame);
          // https://drafts.csswg.org/cssom/#dom-window-getcomputedstyle
          if (property === 'getComputedStyle') return globalThis.getComputedStyle;
          // https://drafts.csswg.org/cssom-view/#the-visualviewport-interface
          if (property === 'visualViewport') return host.__tbFrameVisualViewport(frame);
          if (property === 'scroll' || property === 'scrollTo' || property === 'scrollBy') {
            return host.__tbFrameScrollMethod(frame, property === 'scrollBy');
          }
          if (property === 'addEventListener') {
            return eventTargetMethod(property, '__tbFrameAddEventListener');
          }
          if (property === 'removeEventListener') {
            return eventTargetMethod(property, '__tbFrameRemoveEventListener');
          }
          if (property === 'dispatchEvent') {
            return eventTargetMethod(property, '__tbFrameDispatchEvent');
          }
          if (property === 'location') {
            if (pendingLocation === undefined) {
              pendingLocation = host.__tbFrameLocation(frame);
              if (pendingLocation !== null && pendingLocation !== undefined) {
                __tbDefineProperty(pendingLocation, 'toString', {
                  value: function() { return String(this.href); },
                  writable: true, enumerable: false, configurable: true,
                });
              }
            }
            return pendingLocation;
          }
          const value = globalThis[property];
          if (__tbFrameForwarded(property, value)) return value;
          return undefined;
        }
        // https://html.spec.whatwg.org/multipage/browsers.html#crossoriginproperties-(-o-
        // https://html.spec.whatwg.org/multipage/window-object.html#dom-window-focus
        // https://html.spec.whatwg.org/multipage/window-object.html#dom-opener
        switch (property) {
          case 'location': return crossOriginLocation();
          case 'focus': case 'blur': return crossOriginNoop;
          case 'opener': return null;
        }
        // https://html.spec.whatwg.org/multipage/window-object.html#windowproxy-getownproperty
        if (typeof property === 'string') {
          const child = __tbFrameNamedChild(frame, property);
          if (child != null) return __tbFrameProxy(child);
        }
        // https://html.spec.whatwg.org/multipage/document-sequences.html#destroy-a-child-navigable
        if (!host.__tbFrameRegistered(frame)) return undefined;
        crossOrigin();
      }
      switch (property) {
        case 'name': return host.__tbFrameName(frame);
        case 'document': return host.__tbFrameDocument(frame);
        // https://dom.spec.whatwg.org/#dom-comment-comment
        case 'Comment': case 'Text': return cachedMember(property, sameOrigin, () => {
          const ctor = function() {
            return __tbApply(host.__tb_construct_in_frame, globalThis, [frame, property, ...arguments]);
          };
          const original = sameOrigin[property];
          const proto = original && original.prototype;
          if (proto) ctor.prototype = proto;
          __tbDefineProperty(ctor, 'name', { value: property, configurable: true });
          return ctor;
        });
        // https://dom.spec.whatwg.org/#interface-eventtarget
        // https://html.spec.whatwg.org/multipage/window-object.html#the-windowproxy-exotic-object
        case 'addEventListener': return eventTargetMethod(property, '__tbFrameAddEventListener');
        case 'removeEventListener': return eventTargetMethod(property, '__tbFrameRemoveEventListener');
        case 'dispatchEvent': return eventTargetMethod(property, '__tbFrameDispatchEvent');
      }
      return sameOrigin[property];
    },
    set(target, property, value) {
      const sameOrigin = host.__tbFrameGlobal(frame);
      if (sameOrigin == null) {
        if (host.__tbFrameRegistered(frame)) {
          // https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-location-href
          if (property === 'location' && host.__tbFrameSameOrigin(frame)) {
            host.__tbFrameSetLocation(frame, value);
            return true;
          }
          __tbFramePending(frame)[property] = value;
          return true;
        }
        crossOrigin();
      }
      if (property === 'name') {
        host.__tbSetFrameName(frame, __tbStringCtor(value));
        return true;
      }
      if (property === '__proto__') {
        __tbDefineProperty(sameOrigin, property, {
          __proto__: null, value, writable: true, enumerable: true, configurable: true,
        });
        return true;
      }
      sameOrigin[property] = value;
      return true;
    },
    has(target, property) {
      switch (property) {
        case 'postMessage': case 'parent': case 'top': case 'window': case 'self':
        case 'frames': case 'length': case 'close': case 'closed':
          return true;
      }
      const sameOrigin = host.__tbFrameGlobal(frame);
      if (sameOrigin != null) {
        switch (property) {
          case 'name': case 'document': case 'Comment': case 'Text':
          case 'addEventListener': case 'removeEventListener': case 'dispatchEvent':
            return true;
          default:
            return property in sameOrigin;
        }
      }
      const pending = __tbFramePendingSets[frame];
      if (pending !== undefined && property in pending) return true;
      if (!host.__tbFrameSameOrigin(frame)) {
        switch (property) {
          case 'location': case 'opener': case 'focus': case 'blur':
            return true;
        }
        return typeof property === 'string' && __tbFrameNamedChild(frame, property) != null;
      }
      if (property === 'location' || property === 'name' || property === 'document') return true;
      if (property === 'visualViewport' || property === 'scroll' ||
          property === 'scrollTo' || property === 'scrollBy') return true;
      if (property === 'addEventListener' || property === 'removeEventListener'
          || property === 'dispatchEvent') return true;
      return __tbFrameForwarded(property, globalThis[property]);
    },
    getPrototypeOf() {
      // https://html.spec.whatwg.org/multipage/window-object.html#windowproxy-getprototypeof
      const sameOrigin = host.__tbFrameGlobal(frame);
      return sameOrigin == null
        ? __tbObjectPrototype
        : __tbApply(__tbGetPrototypeOf, null, [sameOrigin]);
    },
    ownKeys() {
      const sameOrigin = host.__tbFrameGlobal(frame);
      return sameOrigin == null ? [] : __tbApply(__tbOwnKeys, null, [sameOrigin]);
    },
    getOwnPropertyDescriptor() { return undefined; },
  };
  proxy = new Proxy({}, handler);
  __tbWindowData.set(proxy, { frame });
  __tbFrameProxies[frame] = proxy;
  return proxy;
};
host.__tbFrameProxy = __tbFrameProxy;

// https://html.spec.whatwg.org/multipage/web-messaging.html#dom-window-postmessage
function __tbNormalizeTargetOrigin(targetOrigin) {
  return targetOrigin === undefined ? '/' : String(targetOrigin);
}
function __tbPostMessage(targetFrame, argumentCount, message, targetOrigin, transfer) {
  if (argumentCount === 0) {
    throw new TypeError(`Failed to execute 'postMessage' on 'Window': 1 argument required, but only 0 present.`);
  }
  if (targetFrame == null) {
    throw new TypeError(`Failed to execute 'postMessage' on 'Window': the target window is missing.`);
  }
  // https://html.spec.whatwg.org/multipage/web-messaging.html#dom-window-postmessage
  if (targetOrigin !== null && typeof targetOrigin === 'object') {
    transfer = targetOrigin.transfer;
    targetOrigin = targetOrigin.targetOrigin;
  }
  // https://html.spec.whatwg.org/multipage/web-messaging.html#window-post-message-steps
  const checkedOrigin = __tbNormalizeTargetOrigin(targetOrigin);
  host.__tbValidateTargetOrigin(checkedOrigin);
  const encoded = __tbEncode(message, transfer, null);
  host.__tbPostWindowMessage(targetFrame, checkedOrigin, encoded.payload, encoded.ports);
}
globalThis.postMessage = function(message, targetOrigin, transfer) {
  return __tbPostMessage(__tbFrameId, arguments.length, message, targetOrigin, transfer);
};
// https://html.spec.whatwg.org/multipage/web-messaging.html#window-post-message-steps
host.__tbDeliverMessage = function(payload, sourceFrame, origin, portIds) {
  if (String(payload).indexOf(__tbPayloadVersion) !== 0) return false;
  const materialized = __tbMaterializePorts(portIds);
  let data;
  try {
    data = __tbDecode(payload, materialized.ports);
  } catch (error) {
    __tbDiscardPorts(portIds);
    return false;
  }
  const portArray = __tbArray.map(portIds, id => materialized.ports[id]);
  host.__tbDispatchTrusted(new __tbMessageEventConstructor('message', {
    data: data, origin: origin, source: __tbFrameProxy(sourceFrame), ports: Object.freeze(portArray),
  }));
  __tbFirePortCloses(materialized.closes);
  return true;
};
host.__tbDeliverMessageError = function(sourceFrame, origin) {
  host.__tbDispatchTrusted(new __tbMessageEventConstructor('messageerror', {
    data: null, origin: origin, source: __tbFrameProxy(sourceFrame),
  }));
};
// https://html.spec.whatwg.org/multipage/web-messaging.html#window-post-message-steps
host.__tbDeliverRemoteMessage = function(origin, payload) {
  if (String(payload).indexOf(__tbPayloadVersion) !== 0) return;
  let data;
  try {
    data = __tbDecode(payload, []);
  } catch (error) {
    return;
  }
  host.__tbDispatchTrusted(new __tbMessageEventConstructor('message', {
    data: data, origin: origin, source: null, ports: Object.freeze([]),
  }));
};

// https://html.spec.whatwg.org/multipage/web-messaging.html#broadcastchannel
const __tbBroadcastData = host.slots('tinybrowser.broadcastchannel.data');
const __tbBroadcastChannels = new __tbPrivateSet();

globalThis.BroadcastChannel = class BroadcastChannel extends EventTarget {
  constructor(name) {
    if (arguments.length < 1) {
      throw new TypeError("Failed to construct 'BroadcastChannel': 1 argument required, but only 0 present.");
    }
    super();
    __tbBroadcastData.set(this, {
        id: host.__tbBroadcastChannelId(), name: String(name), closed: false,
        onmessage: null, onmessageerror: null,
    });
    __tbBroadcastChannels.add(this);
  }
  get name() { return __tbBrand(this, __tbBroadcastData).name; }
  postMessage(message) {
    if (arguments.length < 1) {
      throw new TypeError("Failed to execute 'postMessage' on 'BroadcastChannel': 1 argument required, but only 0 present.");
    }
    const data = __tbBrand(this, __tbBroadcastData);
    if (data.closed) {
      throw new globalThis.DOMException('The channel is closed.', 'InvalidStateError');
    }
    const encoded = __tbEncode(message, [], null);
    host.__tbBroadcastPost(data.name, encoded.payload, data.id);
  }
  close() {
    const data = __tbBrand(this, __tbBroadcastData);
    if (data.closed) return;
    data.closed = true;
    __tbBroadcastChannels.delete(this);
  }
  get onmessage() { return __tbBrand(this, __tbBroadcastData).onmessage; }
  set onmessage(value) { __tbBrand(this, __tbBroadcastData).onmessage = value; }
  get onmessageerror() { return __tbBrand(this, __tbBroadcastData).onmessageerror; }
  set onmessageerror(value) { __tbBrand(this, __tbBroadcastData).onmessageerror = value; }
};
Object.defineProperty(globalThis.BroadcastChannel.prototype, Symbol.toStringTag, {
  value: 'BroadcastChannel', writable: false, enumerable: false, configurable: true,
});

host.__tbDeliverBroadcast = function(name, payload, origin, sourceChannel) {
  for (const channel of __tbBroadcastChannels) {
    const data = __tbBroadcastData.get(channel);
    if (data === undefined || data.closed || data.name !== name) continue;
    if (sourceChannel !== null && sourceChannel !== undefined && data.id === sourceChannel) continue;
    let event;
    try {
      event = new __tbMessageEventConstructor('message', {
        data: __tbDecode(payload, []), origin: origin, source: null, ports: Object.freeze([]),
      });
    } catch (error) {
      event = new __tbMessageEventConstructor('messageerror', {
        data: null, origin: origin, source: null, ports: Object.freeze([]),
      });
    }
    host.__tbDispatchTargetTrusted(channel, event);
  }
};
host.__tbDeliverPortMessage = function(endpoint, payload, portIds) {
  const port = __tbPortLookup(endpoint);
  if (port === null) return true;
  const data = __tbBrand(port, __tbPortData);
  if (data.closed) return true;
  if (String(payload).indexOf(__tbPayloadVersion) !== 0) return false;
  const materialized = __tbMaterializePorts(portIds);
  let value;
  try {
    value = __tbDecode(payload, materialized.ports);
  } catch (error) {
    __tbDiscardPorts(portIds);
    return false;
  }
  const portArray = __tbArray.map(portIds, id => materialized.ports[id]);
  host.__tbDispatchTargetTrusted(port, new __tbMessageEventConstructor('message', {
    data: value, ports: Object.freeze(portArray),
  }));
  __tbFirePortCloses(materialized.closes);
  return true;
};
host.__tbDeliverPortMessageError = function(endpoint) {
  const port = __tbPortLookup(endpoint);
  if (port === null) return;
  host.__tbDispatchTargetTrusted(port, new __tbMessageEventConstructor('messageerror'));
};
host.__tbDeliverPortClose = function(endpoint) {
  const port = __tbPortLookup(endpoint);
  if (port === null) return;
  const data = __tbBrand(port, __tbPortData);
  if (data.closed) return;
  host.__tbDispatchTargetTrusted(port, new __tbEventConstructor('close'));
};

// https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-length
Object.defineProperty(globalThis, 'length', {
  get() { return host.__tbFrameChildCount(__tbFrameId); },
  configurable: true, enumerable: false,
});
// https://html.spec.whatwg.org/multipage/window-object.html#dom-origin
Object.defineProperty(globalThis, 'origin', {
  get() {
    const value = host.__tbStorageOrigin();
    return value === null || value === undefined ? 'null' : value;
  },
  configurable: true, enumerable: true,
});
// https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-window-item
for (let index = 0; index < host.__tb_maxFrames; index++) {
  Object.defineProperty(globalThis, String(index), {
    get() {
      const child = host.__tbFrameChild(__tbFrameId, index);
      return child == null ? undefined : __tbFrameProxy(child);
    },
    configurable: true, enumerable: false,
  });
}
// https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-parent
Object.defineProperty(globalThis, 'parent', {
  get() {
    const parent = host.__tbFrameParent(__tbFrameId);
    return parent == null ? globalThis : __tbFrameProxy(parent);
  },
  configurable: true, enumerable: true,
});
// https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-top
Object.defineProperty(globalThis, 'top', {
  get() {
    const top = host.__tbFrameTop(__tbFrameId);
    return top === __tbFrameId ? globalThis : __tbFrameProxy(top);
  },
  configurable: true, enumerable: true,
});
// https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-name
Object.defineProperty(globalThis, 'name', {
  get() { return host.__tbFrameName(__tbFrameId); },
  set(value) { host.__tbSetFrameName(__tbFrameId, __tbStringCtor(value)); },
  configurable: true, enumerable: true,
});

// https://html.spec.whatwg.org/multipage/webappapis.html#event-handlers
(function() {
  const names = host.__tb_handlerNames;
  if (names === undefined) return;
  // https://html.spec.whatwg.org/multipage/dom.html#documents
  // https://html.spec.whatwg.org/multipage/webappapis.html#event-handlers
  // https://w3c.github.io/mathml-core/#dom-mathmlelement
  const elementProtos = [
    globalThis.Document.prototype,
    globalThis.HTMLElement.prototype,
    globalThis.SVGElement.prototype,
  ];
  if (globalThis.MathMLElement) elementProtos.push(globalThis.MathMLElement.prototype);
  for (const name of names) {
    // https://w3c.github.io/pointerevents/#extensions-to-the-globaleventhandlers-mixin
    if (name === 'onpointerrawupdate' && !globalThis.isSecureContext) continue;
    // https://webidl.spec.whatwg.org/#LegacyLenientThis
    const lenient = name === 'onmouseenter' || name === 'onmouseleave';
    Object.defineProperty(globalThis, name, {
      get() { return lenient && this !== globalThis ? undefined : host.__tbGetWindowHandler(name); },
      set(value) {
        if (lenient && this !== globalThis) return;
        host.__tbSetWindowHandler(name, value);
      },
      enumerable: true, configurable: true,
    });
    for (const proto of elementProtos) {
      Object.defineProperty(proto, name, {
        get() {
          if (lenient && !(this instanceof Node)) return undefined;
          return host.__tbGetNodeHandler(this, name);
        },
        set(value) {
          if (lenient && !(this instanceof Node)) return;
          host.__tbSetNodeHandler(this, name, value);
        },
        enumerable: true, configurable: true,
      });
    }
  }
  // https://html.spec.whatwg.org/multipage/webappapis.html#windoweventhandlers
  for (const name of host.__tb_windowHandlerNames || []) {
    Object.defineProperty(globalThis, name, {
      get() { return host.__tbGetWindowHandler(name); },
      set(value) { host.__tbSetWindowHandler(name, value); },
      enumerable: true, configurable: true,
    });
  }
  // https://html.spec.whatwg.org/multipage/dom.html#the-document-object
  for (const name of host.__tb_documentHandlerNames || []) {
    // https://html.spec.whatwg.org/multipage/dom.html#the-document-object
    const lenient = name === 'onreadystatechange';
    Object.defineProperty(globalThis.Document.prototype, name, {
      get() {
        if (lenient && !(this instanceof Document)) return undefined;
        return host.__tbGetNodeHandler(this, name);
      },
      set(value) {
        if (lenient && !(this instanceof Document)) return;
        host.__tbSetNodeHandler(this, name, value);
      },
      enumerable: true, configurable: true,
    });
  }
  // https://html.spec.whatwg.org/multipage/webappapis.html#window-reflecting-body-element-event-handler-set
  // https://html.spec.whatwg.org/multipage/dom.html#body-element-event-handlers
  const windowReflecting = [
    ...(host.__tb_windowHandlerNames || []),
    ...(host.__tb_bodyWindowHandlers || []),
  ];
  for (const proto of [globalThis.HTMLBodyElement.prototype, globalThis.HTMLFrameSetElement.prototype]) {
    for (const name of windowReflecting) {
      Object.defineProperty(proto, name, {
        get() { return host.__tbGetWindowReflectingHandler(this, name); },
        set(value) { host.__tbSetWindowReflectingHandler(this, name, value); },
        enumerable: true, configurable: true,
      });
    }
  }
})();

// https://html.spec.whatwg.org/multipage/window-object.html#dom-open
(function() {
  const remoteWindows = new __tbPrivateMap();
  const remoteSessions = new __tbPrivateMap();
  const closedTabs = new __tbPrivateSet();

  function remoteWindow(tab) {
    const existing = remoteWindows.get(tab);
    if (existing !== undefined) return existing;
    const handler = {
      get(target, property) {
        switch (property) {
          case 'close': return () => {
            host.__tbWindowClose(tab);
            closedTabs.add(tab);
          };
          case 'closed': return closedTabs.has(tab);
          case 'postMessage': return function(message, targetOrigin, transfer) {
            // https://html.spec.whatwg.org/multipage/web-messaging.html#dom-window-postmessage
            if (arguments.length === 0) {
              throw new TypeError(`Failed to execute 'postMessage' on 'Window': 1 argument required, but only 0 present.`);
            }
            if (targetOrigin !== null && typeof targetOrigin === 'object') {
              transfer = targetOrigin.transfer;
              targetOrigin = targetOrigin.targetOrigin;
            }
            const checkedOrigin = __tbNormalizeTargetOrigin(targetOrigin);
            host.__tbValidateTargetOrigin(checkedOrigin);
            const encoded = __tbEncode(message, transfer, null);
            host.__tbWindowPostMessage(tab, checkedOrigin, encoded.payload);
          };
          case 'localStorage': return host.__tbStorageArea('local');
          case 'sessionStorage': return remoteSession(tab);
          case Symbol.toStringTag: return 'Window';
          default: return undefined;
        }
      },
      has(target, property) {
        switch (property) {
          case 'close': case 'closed': case 'postMessage':
          case 'localStorage': case 'sessionStorage':
            return true;
        }
        return false;
      },
      getOwnPropertyDescriptor() { return undefined; },
      ownKeys() { return []; },
    };
    const proxy = new Proxy({}, handler);
    remoteWindows.set(tab, proxy);
    return proxy;
  }

  function remoteSession(tab) {
    const existing = remoteSessions.get(tab);
    if (existing !== undefined) return existing;
    const session = {
      getItem(key) {
        if (arguments.length < 1) {
          throw new TypeError("Failed to execute 'getItem' on 'Storage': 1 argument required, but only 0 present.");
        }
        const value = host.__tbRemoteSessionGet(tab, host.__tbStorageEncode(String(key)));
        return value === undefined ? null : host.__tbStorageDecode(value);
      },
    };
    remoteSessions.set(tab, session);
    return session;
  }

  globalThis.open = function(url, target, features) {
    if (arguments.length < 1 || url === undefined || url === null) url = '';
    // https://html.spec.whatwg.org/multipage/nav-history-apis.html#window-open-steps
    const href = host.__tbUSVString(url);
    if (href !== '' && host.__tbResolveUrl(href, undefined) == null) {
      throw new globalThis.DOMException('Invalid URL', 'SyntaxError');
    }
    // https://html.spec.whatwg.org/multipage/nav-history-apis.html#window-open-steps
    const name = target === undefined || target === null ? '' : String(target);
    const featureString = features === undefined || features === null ? '' : String(features);
    const answer = host.__tbOpen(href, name, featureString);
    if (answer === null || answer === undefined) return null;
    let proxy;
    switch (answer.kind) {
      case 'frame': proxy = __tbFrameProxy(answer.id); break;
      case 'tab': proxy = remoteWindow(answer.id); break;
      default: throw new TypeError(`unknown window open answer: ${answer.kind}`);
    }
    // https://html.spec.whatwg.org/multipage/nav-history-apis.html#window-open-steps
    return answer.withholds ? null : proxy;
  };

  // https://html.spec.whatwg.org/multipage/window-object.html#dom-window-close
  globalThis.close = function close() { host.__tbCloseWindow(); };

  // https://html.spec.whatwg.org/multipage/window-object.html#dom-opener
  Object.defineProperty(globalThis, 'opener', {
    get() {
      const frame = host.__tbWindowOpenerFrame();
      if (frame !== null && frame !== undefined) return __tbFrameProxy(frame);
      const tab = host.__tbWindowOpener();
      return tab === null || tab === undefined ? null : remoteWindow(tab);
    },
    configurable: true,
  });
})();

// https://html.spec.whatwg.org/multipage/webstorage.html#the-storage-interface
// https://webidl.spec.whatwg.org/#legacy-platform-object
(function() {
  const kindSlot = host.slots('storageKind');
  const holders = new __tbPrivateMap();

  function kindOf(storage) {
    const kind = storage == null ? undefined : kindSlot.get(storage);
    if (kind !== 'local' && kind !== 'session') {
      throw new TypeError('Illegal invocation');
    }
    return kind;
  }

  const quotaError = key => new globalThis.QuotaExceededError(
    "Failed to execute 'setItem' on 'Storage': Setting the value of '" + key +
    "' exceeded the quota.");

  // https://webidl.spec.whatwg.org/#idl-DOMString
  const encode = value => JSON.stringify(value);
  const decode = value => (value === null || value === undefined ? null : JSON.parse(value));

  // https://storage.spec.whatwg.org/#quotaexceedederror
  globalThis.QuotaExceededError = class QuotaExceededError extends globalThis.DOMException {
    constructor(message = '') {
      super(message, 'QuotaExceededError');
    }
    get requested() { return null; }
    get quota() { return null; }
  };
  Object.defineProperty(globalThis.QuotaExceededError.prototype, Symbol.toStringTag, {
    value: 'QuotaExceededError', writable: false, enumerable: false, configurable: true,
  });

  class Storage {
    constructor(kind) {
      kindSlot.set(this, kind);
    }
    get length() {
      return host.__tbStorageKeys(kindOf(this)).length;
    }
    key(index) {
      if (arguments.length < 1) {
        throw new TypeError("Failed to execute 'key' on 'Storage': 1 argument required, but only 0 present.");
      }
      const keys = host.__tbStorageKeys(kindOf(this));
      const n = Number(index) >>> 0;
      return n < keys.length ? decode(keys[n]) : null;
    }
    getItem(key) {
      if (arguments.length < 1) {
        throw new TypeError("Failed to execute 'getItem' on 'Storage': 1 argument required, but only 0 present.");
      }
      const item = host.__tbStorageGet(kindOf(this), encode(String(key)));
      return item === undefined ? null : decode(item);
    }
    setItem(key, value) {
      if (arguments.length < 2) {
        throw new TypeError("Failed to execute 'setItem' on 'Storage': 2 arguments required.");
      }
      key = String(key);
      if (!host.__tbStorageSet(kindOf(this), encode(key), encode(String(value)))) {
        throw quotaError(key);
      }
    }
    removeItem(key) {
      if (arguments.length < 1) {
        throw new TypeError("Failed to execute 'removeItem' on 'Storage': 1 argument required, but only 0 present.");
      }
      host.__tbStorageRemove(kindOf(this), encode(String(key)));
    }
    clear() {
      host.__tbStorageClear(kindOf(this));
    }
  }
  globalThis.Storage = Storage;
  Object.defineProperty(Storage.prototype, Symbol.toStringTag, {
    value: 'Storage', writable: false, enumerable: false, configurable: true,
  });

  function area(kind) {
    const existing = holders.get(kind);
    if (existing !== undefined) return existing;
    const origin = host.__tbStorageOrigin();
    if (origin === null || origin === undefined) {
      throw new globalThis.DOMException(
        "Failed to read the '" + (kind === 'session' ? 'sessionStorage' : 'localStorage') +
        "' property from 'Window': Storage is unavailable for opaque origins.", 'SecurityError');
    }
    const target = new Storage(kind);
    const handler = {
      get(t, property, receiver) {
        if (typeof property === 'symbol' || property in t) {
          return Reflect.get(t, property, receiver);
        }
        const item = host.__tbStorageGet(kind, encode(property));
        return item === null || item === undefined ? undefined : decode(item);
      },
      set(t, property, value) {
        if (typeof property === 'symbol') return Reflect.set(t, property, value);
        if (!host.__tbStorageSet(kind, encode(property), encode(String(value)))) {
          throw quotaError(property);
        }
        return true;
      },
      has(t, property) {
        if (typeof property === 'symbol' || property in t) return true;
        const item = host.__tbStorageGet(kind, encode(property));
        return item !== null && item !== undefined;
      },
      deleteProperty(t, property) {
        if (typeof property === 'symbol') return Reflect.deleteProperty(t, property);
        host.__tbStorageRemove(kind, encode(property));
        return true;
      },
      defineProperty(t, property, descriptor) {
        if (typeof property === 'symbol') return Reflect.defineProperty(t, property, descriptor);
        const value = 'value' in descriptor ? String(descriptor.value) : 'undefined';
        if (!host.__tbStorageSet(kind, encode(property), encode(value))) throw quotaError(property);
        return true;
      },
      getOwnPropertyDescriptor(t, property) {
        if (typeof property === 'symbol') return Reflect.getOwnPropertyDescriptor(t, property);
        if (property in t) return undefined;
        const item = host.__tbStorageGet(kind, encode(property));
        if (item === null || item === undefined) return undefined;
        return { value: decode(item), writable: true, enumerable: true, configurable: true };
      },
      ownKeys(t) {
        const names = host.__tbStorageKeys(kind).map(decode).filter(key => !(key in t));
        return Reflect.ownKeys(t).concat(names);
      },
    };
    const proxy = new Proxy(target, handler);
    kindSlot.set(proxy, kind);
    holders.set(kind, proxy);
    return proxy;
  }

  // https://html.spec.whatwg.org/multipage/webstorage.html#the-storageevent-interface
  const eventData = host.slots('tinybrowser.storageevent.data');
  const freshStorageEvent = () => ({ key: null, oldValue: null, newValue: null, url: '', storageArea: null });
  function dataOf(event) {
    return __tbEventEntry(event, eventData, freshStorageEvent);
  }
  function nullableString(value) {
    return value === undefined || value === null ? null : String(value);
  }
  const StorageEvent = globalThis.StorageEvent = class StorageEvent extends Event {
    constructor(type, init = undefined) {
      if (arguments.length < 1) {
        throw new TypeError("Failed to construct 'StorageEvent': 1 argument required, but only 0 present.");
      }
      const eventInit = init === undefined ? {} : Object(init);
      const event = __tbConstruct(__tbEventConstructor, [String(type), eventInit], new.target);
      eventData.set(event, {
          key: nullableString(eventInit.key),
          oldValue: nullableString(eventInit.oldValue),
          newValue: nullableString(eventInit.newValue),
          url: eventInit.url === undefined ? '' : String(eventInit.url),
          storageArea: eventInit.storageArea === undefined ? null : eventInit.storageArea,
      });
      return event;
    }
    get key() { return dataOf(this).key; }
    get oldValue() { return dataOf(this).oldValue; }
    get newValue() { return dataOf(this).newValue; }
    get url() { return dataOf(this).url; }
    get storageArea() { return dataOf(this).storageArea; }
    initStorageEvent(type, bubbles = false, cancelable = false, key = null, oldValue = null, newValue = null, url = '', storageArea = null) {
      if (arguments.length < 1) {
        throw new TypeError("Failed to execute 'initStorageEvent' on 'StorageEvent': 1 argument required, but only 0 present.");
      }
      Event.prototype.initEvent.call(this, String(type), Boolean(bubbles), Boolean(cancelable));
      const data = dataOf(this);
      data.key = key === null ? null : String(key);
      data.oldValue = oldValue === null ? null : String(oldValue);
      data.newValue = newValue === null ? null : String(newValue);
      data.url = String(url);
      data.storageArea = storageArea;
    }
  };
  Object.defineProperty(globalThis.StorageEvent.prototype, Symbol.toStringTag, {
    value: 'StorageEvent', writable: false, enumerable: false, configurable: true,
  });

  // https://html.spec.whatwg.org/multipage/webstorage.html#concept-storage-broadcast
  host.__tbFireStorageEvent = function(kind, key, oldValue, newValue, url) {
    host.__tbDispatchTrusted(new StorageEvent('storage', {
      key: decode(key), oldValue: decode(oldValue), newValue: decode(newValue),
      url: url, storageArea: area(kind),
    }));
  };

  Object.defineProperty(globalThis, 'localStorage', {
    get() { return area('local'); }, configurable: true,
  });
  Object.defineProperty(globalThis, 'sessionStorage', {
    get() { return area('session'); }, configurable: true,
  });
  host.__tbStorageArea = area;
  host.__tbStorageEncode = encode;
  host.__tbStorageDecode = decode;
})();

// https://html.spec.whatwg.org/multipage/structured-data.html#serializable-objects
(function() {
  const names = [
    'Event', 'EventTarget', 'Node', 'DOMException', 'Attr', 'NamedNodeMap',
    'TokenList', 'Implementation', 'DOMParser', 'XMLSerializer', 'MutationObserver',
    'MutationRecord', 'AbstractRange', 'Range', 'MessageEvent', 'MessagePort', 'MessageChannel', 'Headers',
    'Request', 'Response', 'Blob', 'File', 'FileList', 'FileReader', 'ProgressEvent',
    'XMLHttpRequest', 'XMLHttpRequestUpload',
    'ReadableStream', 'TextDecoder', 'TextEncoder', 'URL', 'URLSearchParams',
    'AbortController', 'AbortSignal', 'CustomEvent', 'Document',
    'Storage', 'StorageEvent', 'QuotaExceededError', 'Navigator', 'WorkerNavigator',
  ];
  for (const name of names) {
    const ctor = globalThis[name];
    if (typeof ctor === 'function' && ctor.prototype !== undefined) {
      try {
        __tbPlatform.set(ctor.prototype, true);
      } catch (error) {}
    }
  }
  __tbPlatform.set(globalThis, true);
})();
