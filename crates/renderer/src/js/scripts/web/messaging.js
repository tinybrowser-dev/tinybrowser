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
// ── cross-realm structured serialization ───────────────────────────────
// Values cannot cross realms, so the sender's realm encodes a message into a
// versioned payload string and the receiver's realm decodes it
// (<https://html.spec.whatwg.org/multipage/structured-data.html#structuredserializeinternal>).
// Rust carries payloads between frames, owns the frame tree, and owns the
// channel endpoints, so a port survives being transferred to another realm.
const __tbPayloadVersion = 'tb1:';
const __tbPlatform = host.slots('tinybrowser.platform');
const __tbWindowData = host.slots('tinybrowser.window.data');
const __tbPortData = host.slots('tinybrowser.messageport.data');
const __tbFrameId = host.__tb_frameId;
__tbWindowData.set(globalThis, { frame: __tbFrameId });
const __tbFrameProxies = Object.create(null);
// Writes made through a proxy whose target realm does not exist yet: a
// browsing context registered inside a script gets its realm at the next
// non-JS turn, and the engine then flushes these through
// `__tbFlushFrameSets` (<https://html.spec.whatwg.org/multipage/window-object.html#windowproxy-set>).
const __tbFramePendingSets = Object.create(null);
// EventTarget methods captured at realm init from our own prototype (like
// `cssom.js`): the WindowProxy below binds these, never the target realm's
// lazily-read (and page-clobberable) own properties.
const __tbProxyEventTarget = globalThis.EventTarget.prototype;
const __tbProxyAddEventListener = __tbProxyEventTarget.addEventListener;
const __tbProxyRemoveEventListener = __tbProxyEventTarget.removeEventListener;
const __tbProxyDispatchEvent = __tbProxyEventTarget.dispatchEvent;
const __tbFramePending = frame => {
  let pending = __tbFramePendingSets[frame];
  if (pending === undefined) {
    pending = __tbObjectCreate(null);
    __tbFramePendingSets[frame] = pending;
  }
  return pending;
};
host.__tbFlushFrameSets = function(frame) {
  const pending = __tbFramePendingSets[frame];
  if (pending === undefined) return;
  delete __tbFramePendingSets[frame];
  const target = host.__tbFrameGlobal(frame);
  if (target == null) return;
  for (const key of Reflect.ownKeys(pending)) {
    if (key === '__proto__' || typeof key === 'symbol') {
      // `CreateDataProperty`: a `__proto__` write must not walk the prototype
      // setter, and symbol keys need `defineProperty` to become own data
      // properties.
      Object.defineProperty(target, key, {
        value: pending[key], writable: true, enumerable: true, configurable: true,
      });
    } else {
      target[key] = pending[key];
    }
  }
};

const __tbCloneFailure = () => new globalThis.DOMException('The object could not be cloned.', 'DataCloneError');
const __tbBase64Bytes = text => {
  const binary = globalThis.atob(text);
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index++) bytes[index] = binary.charCodeAt(index);
  return bytes;
};
// Cross-realm brand probes: an intrinsic from this realm accepts an object
// from any realm, and throws when the internal slot is missing.
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
// `ImageData` is serializable but has no brand slot; its `width` accessor is
// an internal-slot check that works across realms
// (<https://html.spec.whatwg.org/multipage/structured-data.html#serializable-objects>).
const __tbImageDataPrototype = typeof globalThis.ImageData === 'function'
  ? globalThis.ImageData.prototype
  : null;
// A prototype-chain check, not a member probe: the `width`/`data` accessors
// do not validate their receiver, so probing one would also match an
// `ImageBitmap` or a plain object. A subclass still matches, and deserializes
// as its closest serializable superclass
// (<https://html.spec.whatwg.org/multipage/structured-data.html#structureddeserialize>).
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
// Every `ImageBitmap` carries the registry id the Rust canvas layer assigned.
const __tbIsImageBitmap = value => __tbImageBitmapPrototype !== null
  && __tbHasPrototype(value, __tbImageBitmapPrototype)
  && typeof value.__tbBitmap === 'number';
// Interface constructors the decoder instantiates are captured here, so a
// page that deletes or replaces a global cannot change what a clone becomes
// (<https://html.spec.whatwg.org/multipage/structured-data.html#structureddeserialize>).
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
// An array index is a canonical non-negative integer string below 2^32-1.
const __tbIsArrayIndex = key => {
  const number = Number(key);
  return Number.isInteger(number) && number >= 0 && number < 4294967295
    && String(number) === key;
};
// A real error (from any realm) answers `[object Error]` through its internal
// slot; a plain object can only do so by defining `Symbol.toStringTag`, which
// real errors never do
// (<https://html.spec.whatwg.org/multipage/structured-data.html#structuredserializeinternal>).
const __tbIsError = value => Object.prototype.toString.call(value) === '[object Error]'
  && value[Symbol.toStringTag] === undefined;
// Types with internal slots the serializer cannot reproduce. Weak maps and
// sets answer their own brand probes; promises and weak refs expose a tag and
// have no non-destructive brand probe.
const __tbIsWeakMap = value => __tbProbe(candidate => WeakMap.prototype.has.call(candidate, candidate), value);
const __tbIsWeakSet = value => __tbProbe(candidate => WeakSet.prototype.has.call(candidate, candidate), value);
const __tbIsPromise = value => typeof Promise === 'function' && Object.prototype.toString.call(value) === '[object Promise]';
const __tbIsWeakRef = value => typeof WeakRef === 'function' && Object.prototype.toString.call(value) === '[object WeakRef]';
const __tbHasInternalSlots = value => __tbIsPromise(value) || __tbIsWeakMap(value)
  || __tbIsWeakSet(value) || __tbIsWeakRef(value);

// `[[NumberData]]` specials have no JSON spelling.
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
  // Every element converts to the WebIDL `object` type: `null` and
  // `undefined` have no object conversion and throw a TypeError, before any
  // de/re-serialization or transfer side effect
  // (<https://webidl.spec.whatwg.org/#es-object>).
  for (const item of items) {
    if (item === null || (typeof item !== 'object' && typeof item !== 'function')) {
      throw new TypeError('The transfer list must contain only objects');
    }
  }
  return items;
};

// `postMessage(message, sequence<object> transfer)` and
// `postMessage(message, optional StructuredSerializeOptions options = {})`
// are overloads on `Worker`, `DedicatedWorkerGlobalScope`, and `MessagePort`.
// WebIDL picks the sequence only for an object with an `@@iterator` method;
// `null`, `undefined`, a primitive, and a non-iterable object all pick the
// dictionary, whose `transfer` member is the transfer list
// (<https://webidl.spec.whatwg.org/#dfn-overload-resolution-algorithm>).
const __tbPostMessageTransfer = transfer => {
  if (transfer !== null && typeof transfer === 'object'
      && typeof transfer[Symbol.iterator] === 'function') {
    return transfer;
  }
  if (transfer === null || transfer === undefined) return undefined;
  return transfer.transfer;
};

// Serializes `value` into { payload, ports }, detaching every transferred
// buffer and endpoint as it does so
// (<https://html.spec.whatwg.org/multipage/structured-data.html#structuredserializewithtransfer>).
const __tbEncode = (value, transfer, sourcePort) => {
  // Validate the transfer list first: nothing is transferred until the value
  // graph serializes, because transferring has side effects and
  // StructuredSerializeInternal must be able to throw first
  // (<https://html.spec.whatwg.org/multipage/structured-data.html#structuredserializewithtransfer>).
  const buffers = new __tbPrivateMap();     // ArrayBuffer -> node slot, or -1 once listed
  const transferred = new __tbPrivateMap(); // MessagePort -> endpoint id
  const canvases = new __tbPrivateMap();    // OffscreenCanvas -> node slot, or -1 once listed
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
    } else if (typeof globalThis.OffscreenCanvas === 'function'
               && item instanceof globalThis.OffscreenCanvas) {
      if (canvases.has(item)) {
        throw new globalThis.DOMException('Transfer list contains duplicate canvases', 'DataCloneError');
      }
      if (host.__tbOffscreenDetached(item)) {
        throw new globalThis.DOMException('Cannot transfer a detached OffscreenCanvas', 'DataCloneError');
      }
      // A canvas with a context cannot move its bitmap yet
      // (<https://html.spec.whatwg.org/multipage/structured-data.html#structuredserializeinternal>).
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
    // Window proxies are never serializable
    // (<https://html.spec.whatwg.org/multipage/structured-data.html#structuredserializeinternal>).
    if (__tbWindowData.get(input) !== undefined) throw __tbCloneFailure();
    if (transferred.has(input)) {
      const index = slot(['port', transferred.get(input)]);
      seen.set(input, index);
      return index;
    }
    if (buffers.has(input)) {
      // The bytes are captured by the transfer step, after the graph is done.
      const index = slot(null);
      seen.set(input, index);
      buffers.set(input, index);
      return index;
    }
    if (canvases.has(input)) {
      // The receiver gets a fresh OffscreenCanvas; the sender is detached in
      // the transfer step below.
      const index = slot(['offscreen', input.width, input.height]);
      seen.set(input, index);
      canvases.set(input, index);
      return index;
    }
    if (typeof globalThis.OffscreenCanvas === 'function'
        && input instanceof globalThis.OffscreenCanvas) {
      // OffscreenCanvas is transferable but not serializable, so a clone
      // without a transfer entry must fail instead of becoming `{}`
      // (<https://html.spec.whatwg.org/multipage/structured-data.html#structuredserializeinternal>).
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
    // A `FileList` clones its files; the interface object is recreated in the
    // target realm
    // (<https://w3c.github.io/FileAPI/#filelist-section>).
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
    // `ImageData` clones its pixels as a fresh `Uint8ClampedArray`
    // (<https://html.spec.whatwg.org/multipage/structured-data.html#serializable-objects>).
    if (__tbIsImageData(input)) {
      const index = slot([
        'imagedata', input.width, input.height,
        __tbBase64Encode(new __tbUint8Array(input.data.buffer, input.data.byteOffset, input.data.byteLength)),
      ]);
      seen.set(input, index);
      return index;
    }
    // `ImageBitmap` is serializable and transferable; the receiver gets a
    // fresh bitmap with the same pixels
    // (<https://html.spec.whatwg.org/multipage/structured-data.html#serializable-objects>).
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
        // A detached buffer cannot be copied; the spec reports it as a clone
        // failure, not a raw TypeError.
        throw __tbCloneFailure();
      }
      const index = slot([
        'buffer', __tbBase64Encode(view), input.maxByteLength, input.resizable === true,
      ]);
      seen.set(input, index);
      return index;
    }
    if (typeof SharedArrayBuffer === 'function' && input instanceof SharedArrayBuffer) {
      // Shared memory would need to stay shared across realms; the engine
      // has no shared-memory transport, so refuse instead of corrupting.
      throw __tbCloneFailure();
    }
    // `ArrayBuffer.isView` is the brand check: the typed-array `length`
    // accessor throws for a view left out of bounds by `resize()`, so probing
    // it would misclassify the view as a plain object
    // (<https://tc39.es/ecma262/#sec-arraybuffer.isview>).
    if (__tbIsArrayBufferView(input)) {
      // A view left out of bounds by `resize` reports 0 for both byte fields,
      // so only the engine can see that its backing store is gone. Such a view
      // cannot be serialized
      // (<https://html.spec.whatwg.org/multipage/structured-data.html#structuredserializeinternal>).
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
        // A view over a resized-away buffer has no byte range to read.
        throw __tbCloneFailure();
      }
      // A view that falls outside its (resized) buffer cannot be serialized
      // (<https://html.spec.whatwg.org/multipage/structured-data.html#structuredserializeinternal>).
      if (byteOffset + byteLength > buffer.byteLength) throw __tbCloneFailure();
      // A length-tracking view is created without an explicit length and grows
      // with its resizable buffer; the receiver rebuilds it the same way
      // (<https://tc39.es/ecma262/#sec-arraybuffer.prototype.resize>).
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
    // DOMException is serializable (name and message survive)
    // (<https://html.spec.whatwg.org/multipage/structured-data.html#serializable-objects>).
    if (Object.prototype.toString.call(input) === '[object DOMException]') {
      return slot(['domexception', String(input.name), String(input.message)]);
    }
    // Platform objects (nodes, events, ports, ...) are not serializable.
    if (__tbPortData.has(input)) throw __tbCloneFailure();
    for (let object = input; object !== null; object = Object.getPrototypeOf(object)) {
      if (__tbPlatform.has(object)) throw __tbCloneFailure();
    }
    // Objects with internal slots the serializer cannot reproduce must fail,
    // not silently clone as plain objects
    // (<https://html.spec.whatwg.org/multipage/structured-data.html#structuredserializeinternal>).
    if (__tbHasInternalSlots(input)) throw __tbCloneFailure();
    if (__tbIsDate(input)) return slot(['date', __tbNumberWire(input.getTime())]);
    if (__tbIsRegExp(input)) return slot(['regexp', input.source, input.flags]);
    if (__tbIsError(input)) {
      const index = slot(null);
      seen.set(input, index);
      // An error's `name`, its own `message` (a missing message is not the
      // same as an empty one), and its `cause` survive the clone; custom
      // properties do not
      // (<https://html.spec.whatwg.org/multipage/structured-data.html#structuredserializeinternal>).
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
      // Array serialization also copies own enumerable properties that are not
      // indices, like any other object graph node
      // (<https://html.spec.whatwg.org/multipage/structured-data.html#structuredserializeinternal>).
      const properties = __tbPrivateArray();
      for (const key of Object.keys(input)) {
        if (__tbIsArrayIndex(key)) continue;
        __tbArray.push(properties, __tbSetPrototypeOf([key, encode(input[key])], null));
      }
      nodes[index] = __tbSetPrototypeOf(['array', items, properties], null);
      return index;
    }
    // Anything else clones as an object with its own enumerable properties,
    // whatever its prototype chain says
    // (<https://html.spec.whatwg.org/multipage/structured-data.html#structuredserializeinternal>).
    const index = slot(null);
    seen.set(input, index);
    const entries = __tbPrivateArray();
    for (const key of Object.keys(input)) __tbArray.push(entries, __tbSetPrototypeOf([key, encode(input[key])], null));
    nodes[index] = __tbSetPrototypeOf(['object', entries], null);
    return index;
  };
  const root = encode(value);
  // Serialization succeeded, so the transfer steps run now
  // (<https://html.spec.whatwg.org/multipage/structured-data.html#structuredserializewithtransfer>).
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
  // Transferring an ImageBitmap closes the source bitmap
  // (<https://html.spec.whatwg.org/multipage/structured-data.html#structuredserializewithtransfer>).
  for (const bitmap of transferredBitmaps) {
    bitmap.close();
  }
  const ports = __tbPrivateArray();
  for (const [port, id] of transferred) {
    if (!host.__tbPortDetach(id)) throw new globalThis.DOMException('Cannot transfer a detached MessagePort', 'DataCloneError');
    __tbBrand(port, __tbPortData).closed = true;
    // The received port is a new object; the sender's is detached.
    delete __tbPorts[id];
    __tbArray.push(ports, id);
  }
  return { __proto__: null, payload: __tbPayloadVersion + __tbJsonStringify({ __proto__: null, root, nodes }), ports };
};

// Deserializes a payload in this realm; `ports` maps transferred endpoint ids
// to the port objects that were already materialized for them.
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
        // A resizable buffer keeps its maximum so a later `resize()` reaches
        // the same ceiling the sender had
        // (<https://tc39.es/ecma262/#sec-arraybuffer-constructor>).
        const buffer = resizable && max > bytes.length
          ? new ArrayBuffer(bytes.length, { maxByteLength: max })
          : resizable
            ? new ArrayBuffer(bytes.length, { maxByteLength: bytes.length })
            : new ArrayBuffer(bytes.length);
        new Uint8Array(buffer).set(bytes);
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
        // A length-tracking view takes no explicit length and follows the
        // buffer's size when it is resized
        // (<https://tc39.es/ecma262/#sec-arraybuffer.prototype.resize>).
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
        // Built in the prepass below: a FileList's files have higher slot
        // indices than the list itself, so they are not ready yet here.
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
    }
  }
  // A FileList is built before the other containers so a reference to it
  // resolves: its `File` elements are leaves and already exist
  // (<https://w3c.github.io/FileAPI/#filelist-section>).
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
          // `CreateDataProperty`, not `Set`: a `__proto__` key must become an
          // own property instead of walking the prototype setter
          // (<https://html.spec.whatwg.org/multipage/structured-data.html#structureddeserialize>).
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

// ── message ports ──────────────────────────────────────────────────────
// The endpoint (id, queue, entanglement) lives in Rust; the JS object is the
// realm's handle on it, registered in `__tbPorts` so deliveries can find it.
const __tbPorts = Object.create(null);

// Materializes every transferred endpoint. Closes are deferred: a port whose
// peer disentangled while it was in transit fires `close` once the carrying
// message has been dispatched, so the message handler can install
// `onclose` first
// (<https://html.spec.whatwg.org/multipage/web-messaging.html#disentangle>).
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
// Drops ports a delivery could not decode; the spec loses them with the
// failed message.
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
// An `EventHandler` accepts a function, or an object whose `handleEvent` is
// callable; every other value, `null`, and `undefined` become `null`
// (<https://webidl.spec.whatwg.org/#idl-event-handler>).
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
    // The message is required; the overload set has no zero-argument member
    // (<https://webidl.spec.whatwg.org/#overload-resolution>).
    if (arguments.length < 1) {
      throw new TypeError("Failed to execute 'postMessage' on 'MessagePort': 1 argument required, but only 0 present.");
    }
    // The transfer list is consumed even when the port is detached or the
    // message is doomed
    // (<https://html.spec.whatwg.org/multipage/web-messaging.html#message-port-post-message-steps>).
    const encoded = __tbEncode(message, __tbPostMessageTransfer(transfer), this);
    if (data.closed) return;
    // A port posted to its own entangled port loses the channel
    // (<https://html.spec.whatwg.org/multipage/web-messaging.html#message-port-post-message-steps>).
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
    // The first set enables the queue, whatever the value. The internal start
    // step runs even when script replaced the `start` property on this port
    // (<https://html.spec.whatwg.org/multipage/web-messaging.html#message-ports>).
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

// ── window proxies ─────────────────────────────────────────────────────
// One proxy per frame per realm, stable across the frame's navigations. It
// carries the cross-origin whitelist; same-origin members forward through the
// target realm's window
// (<https://html.spec.whatwg.org/multipage/window-object.html#the-windowproxy-exotic-object>).
// The cross-origin member set follows Blink's `[CrossOrigin]` IDL attributes
// and Firefox's `sCrossOriginProperties`: postMessage, window, self, frames,
// length, top, parent, closed, and the indexed getter.
const __tbFrameProxy = frame => {
  if (frame == null) return undefined;
  if (frame === __tbFrameId) return globalThis;
  if (__tbFrameProxies[frame] !== undefined) return __tbFrameProxies[frame];
  let proxy;
  const crossOrigin = () => {
    throw new globalThis.DOMException('Blocked a frame from accessing a cross-origin frame.', 'SecurityError');
  };
  // One function per proxy: a shipped engine keeps
  // `contentWindow.postMessage` identity stable, and so do we.
  const postMessage = function(message, targetOrigin, transfer) {
    try {
      return __tbPostMessage(frame, arguments.length, message, targetOrigin, transfer);
    } catch (error) {
      // Exceptions from a proxy's postMessage come from the target window's
      // realm, the way a shipped engine throws them.
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
  // Same-origin-only members are built once per target realm: proxy gets
  // keep identity (`contentWindow.addEventListener ===
  // contentWindow.addEventListener`), and rebuilding when the target global
  // changes keeps prototypes and methods from going stale across navigations.
  const proxyCache = __tbObjectCreate(null);
  const cachedMember = (key, targetGlobal, make) => {
    const entry = proxyCache[key];
    if (entry !== undefined && entry.global === targetGlobal) return entry.fn;
    const fn = make();
    proxyCache[key] = { global: targetGlobal, fn };
    return fn;
  };
  // One helper for the three EventTarget bindings below (was three
  // near-identical `cachedMember` blocks).
  const bindTargetMethod = (key, targetGlobal, method) => cachedMember(key, targetGlobal, () =>
    function() { return __tbApply(method, proxy, arguments); });
  const handler = {
    get(target, property) {
      // Cross-origin-safe window properties only; everything else needs the
      // target realm below. In particular `name`, `document`, the node
      // constructors, and the EventTarget methods are not cross-origin
      // accessible, so a navigated-away child no longer leaks them
      // (<https://html.spec.whatwg.org/multipage/browsers.html#crossoriginproperties-(-o-)>).
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
        case 'closed': return !host.__tbFrameRegistered(frame);
        case Symbol.toStringTag: return 'Window';
      }
      const sameOrigin = host.__tbFrameGlobal(frame);
      if (sameOrigin == null) {
        if (host.__tbFrameRegistered(frame)) {
          // A document exists before its realm does: `window.open`'s initial
          // about:blank and an `iframe` inserted in a running script. Its
          // wrapper belongs to this realm until the target realm starts.
          if (property === 'document') return host.__tbFrameDocument(frame);
          // Loading same-origin (realm pending) and navigated-away
          // cross-origin both yield `undefined` here; conflation is
          // intentional for the polling pattern (`child.document` becomes
          // defined at realm init), not a spec gate.
          const pending = __tbFramePendingSets[frame];
          if (pending !== undefined && __tbApply(__tbHasOwnProperty, pending, [property])) {
            return pending[property];
          }
          return undefined;
        }
        crossOrigin();
      }
      switch (property) {
        case 'name': return host.__tbFrameName(frame);
        case 'document': return host.__tbFrameDocument(frame);
        // Cross-realm constructors are not callable from this realm. Hand
        // back a local function that creates the node in the target frame's
        // document; its prototype is the target realm's so `instanceof
        // contentWindow.Comment` holds
        // (<https://dom.spec.whatwg.org/#dom-comment-comment>).
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
        // Host EventTarget methods live on each realm's window object. A
        // WindowProxy get that forwards to another realm's global does not
        // yield a callable from this realm, so `contentWindow.addEventListener`
        // was not a function. Bind the realm-init-captured method (never the
        // target's clobberable own property) with the proxy as `this`;
        // `window_world_for_call` resolves the target frame
        // (<https://dom.spec.whatwg.org/#interface-eventtarget>,
        // <https://html.spec.whatwg.org/multipage/window-object.html#the-windowproxy-exotic-object>).
        case 'addEventListener': return bindTargetMethod(property, sameOrigin, __tbProxyAddEventListener);
        case 'removeEventListener': return bindTargetMethod(property, sameOrigin, __tbProxyRemoveEventListener);
        case 'dispatchEvent': return bindTargetMethod(property, sameOrigin, __tbProxyDispatchEvent);
      }
      return sameOrigin[property];
    },
    set(target, property, value) {
      // Writes need the target realm too: a cross-origin `name` set must not
      // reach the stored name.
      const sameOrigin = host.__tbFrameGlobal(frame);
      if (sameOrigin == null) {
        if (host.__tbFrameRegistered(frame)) {
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
        // `CreateDataProperty`, not prototype mutation (cf.
        // `__tbFlushFrameSets`): a `__proto__` set must not re-prototype the
        // target realm's window.
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
        case 'frames': case 'length': case 'closed':
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
      return pending !== undefined && property in pending;
    },
    getPrototypeOf() {
      // The spec forwards to the target; cross-origin callers cannot reach
      // it, so the plain object prototype stands in
      // (<https://html.spec.whatwg.org/multipage/window-object.html#windowproxy-getprototypeof>).
      // Pinned at realm init: a page replacing `globalThis.Object` must not
      // control what cross-origin callers receive.
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

// ── posting and delivery ───────────────────────────────────────────────
// https://html.spec.whatwg.org/multipage/web-messaging.html#dom-window-postmessage
// `postMessage` step 5: `/` names the incumbent origin and is passed through
// for the host to resolve with origin identity intact; anything else but `*`
// must parse as a URL. The host parses it before the message is serialized.
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
  // `postMessage(message, options)` dictionary overload
  // (<https://html.spec.whatwg.org/multipage/web-messaging.html#dom-window-postmessage>).
  if (targetOrigin !== null && typeof targetOrigin === 'object') {
    transfer = targetOrigin.transfer;
    targetOrigin = targetOrigin.targetOrigin;
  }
  // The target origin resolves before the message is serialized, so an
  // invalid value throws `SyntaxError` without detaching buffers, ports, or
  // canvases
  // (<https://html.spec.whatwg.org/multipage/web-messaging.html#window-post-message-steps>).
  const checkedOrigin = __tbNormalizeTargetOrigin(targetOrigin);
  host.__tbValidateTargetOrigin(checkedOrigin);
  const encoded = __tbEncode(message, transfer, null);
  host.__tbPostWindowMessage(targetFrame, checkedOrigin, encoded.payload, encoded.ports);
}
globalThis.postMessage = function(message, targetOrigin, transfer) {
  return __tbPostMessage(__tbFrameId, arguments.length, message, targetOrigin, transfer);
};
// Runs in the target realm: decode, then dispatch a trusted message event, or
// report the failure so the engine dispatches `messageerror`
// (<https://html.spec.whatwg.org/multipage/web-messaging.html#window-post-message-steps>).
host.__tbDeliverMessage = function(payload, sourceFrame, origin, portIds) {
  if (String(payload).indexOf(__tbPayloadVersion) !== 0) return false;
  const materialized = __tbMaterializePorts(portIds);
  let data;
  try {
    data = __tbDecode(payload, materialized.ports);
  } catch (error) {
    // The ports arrived with a message that cannot be decoded; they are lost
    // with it.
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
// A message from another tab cannot name a frame in this renderer, so its
// source is `null`, and its `origin` is the sender's serialized origin at
// post time
// (<https://html.spec.whatwg.org/multipage/web-messaging.html#window-post-message-steps>).
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

// ── broadcast channels ─────────────────────────────────────────────────
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

// ── the window's own indexed and browsing-context members ──────────────
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

// Event handler properties live in the world rather than on the wrapper, so
// a collected wrapper cannot lose `element.onload`
// (<https://html.spec.whatwg.org/multipage/webappapis.html#event-handlers>).
(function() {
  const names = host.__tb_handlerNames;
  if (names === undefined) return;
  for (const name of names) {
    Object.defineProperty(globalThis, name, {
      get() { return host.__tbGetWindowHandler(name); },
      set(value) { host.__tbSetWindowHandler(name, value); },
      enumerable: true, configurable: true,
    });
    for (const proto of [globalThis.HTMLElement.prototype, globalThis.SVGElement.prototype]) {
      Object.defineProperty(proto, name, {
        get() { return host.__tbGetNodeHandler(this, name); },
        set(value) { host.__tbSetNodeHandler(this, name, value); },
        enumerable: true, configurable: true,
      });
    }
  }
})();

// ── auxiliary windows ──────────────────────────────────────────────────
// `window.open` may create a browser tab or a same-origin frame; either way
// the engine selects the context. This block keeps one proxy per remote tab,
// exposing `close`, `postMessage`, and the same-origin storage areas, and
// forgets the tab's name when `close()` runs so a later open creates a fresh
// one
// (<https://html.spec.whatwg.org/multipage/window-object.html#dom-open>).
(function() {
  const remoteWindows = new __tbPrivateMap();
  const remoteSessions = new __tbPrivateMap();
  // Tabs closed through `close()`, so `closed` reports it. The host forgets
  // the tab's name, so a later `open(url, name)` opens a fresh tab.
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
            // Same argument handling as a same-page `postMessage`
            // (<https://html.spec.whatwg.org/multipage/web-messaging.html#dom-window-postmessage>).
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

  // A live read of another window's session area, for same-origin openers and
  // opened windows. The response carries the storage seam's encoded strings.
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
    // The engine runs the window open steps: it encodes the URL, tokenizes the
    // features, applies the blob-URL opener rule, and picks the context. The
    // shim only turns the selected context into a `WindowProxy`
    // (<https://html.spec.whatwg.org/multipage/nav-history-apis.html#window-open-steps>).
    //
    // `target` defaults to `_blank` per the IDL, and the engine also treats an
    // empty or absent target as `_blank`, so passing `undefined` through is
    // enough.
    const name = target === undefined || target === null ? '' : String(target);
    const featureString = features === undefined || features === null ? '' : String(features);
    const answer = host.__tbOpen(String(url), name, featureString);
    if (answer === null || answer === undefined) return null;
    let proxy;
    switch (answer.kind) {
      case 'frame': proxy = __tbFrameProxy(answer.id); break;
      case 'tab': proxy = remoteWindow(answer.id); break;
      default: throw new TypeError(`unknown window open answer: ${answer.kind}`);
    }
    // `noopener` still selects and navigates the context, and never hands back
    // its `WindowProxy`
    // (<https://html.spec.whatwg.org/multipage/nav-history-apis.html#window-open-steps>).
    return answer.withholds ? null : proxy;
  };

  // https://html.spec.whatwg.org/multipage/window-object.html#dom-window-close
  // A script may only close a context it opened. Closing a child iframe or the
  // tab's own top-level frame is a no-op, which the host checks.
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

// ── web storage ─────────────────────────────────────────────────────────
// `localStorage` and `sessionStorage` are one interface over two areas: the
// browser process owns the local area (shared by every tab, persisted with
// the profile), the engine owns the session area (one per top-level browsing
// context). Items are named properties per the legacy platform-object rules:
// a stored item is hidden by a member of the prototype chain, and every
// string-keyed write stores
// (<https://html.spec.whatwg.org/multipage/webstorage.html#the-storage-interface>,
// <https://webidl.spec.whatwg.org/#legacy-platform-object>).
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

  // JavaScript strings may hold lone surrogates; DOMString preserves them but
  // the UTF-8 host seam cannot. JSON escaping round-trips them exactly
  // (<https://webidl.spec.whatwg.org/#idl-DOMString>).
  const encode = value => JSON.stringify(value);
  const decode = value => (value === null || value === undefined ? null : JSON.parse(value));

  // https://storage.spec.whatwg.org/#quotaexceedederror
  // This user agent names no requested size, so both members are `null`;
  // they live on the subclass so a plain `DOMException` named
  // `QuotaExceededError` does not grow them.
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

  /// Fires one storage event in this realm, with this realm's area as
  /// `storageArea`
  /// (<https://html.spec.whatwg.org/multipage/webstorage.html#concept-storage-broadcast>).
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
  // Remote-window proxies share these areas for same-origin openers.
  host.__tbStorageArea = area;
  host.__tbStorageEncode = encode;
  host.__tbStorageDecode = decode;
})();

// Platform objects and globals are not serializable; the marker travels with
// the prototype, so it identifies an object from another realm too
// (<https://html.spec.whatwg.org/multipage/structured-data.html#serializable-objects>).
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
