const {
  Object, Function, String, Number, Boolean, Array, ArrayBuffer, Uint8Array,
  Date, JSON, Promise, RegExp, Proxy, Reflect, Symbol, Map, Set, WeakMap, WeakSet,
  Error, TypeError, RangeError,
} = globalThis;
const __tbApply = Reflect.apply;
const __tbConstruct = Reflect.construct;
const __tbDefineProperty = Object.defineProperty;
const __tbGetOwnPropertyDescriptor = Object.getOwnPropertyDescriptor;
const __tbGetOwnPropertyNames = Object.getOwnPropertyNames;
const __tbGetOwnPropertySymbols = Object.getOwnPropertySymbols;
const __tbGetPrototypeOf = Object.getPrototypeOf;
const __tbIsArray = Array.isArray;
const __tbSymbolToString = Symbol.prototype.toString;
const __tbNumberIsFinite = Number.isFinite;
const __tbNumberIsNaN = Number.isNaN;
const __tbSetConstructor = Set;
const __tbSetHas = Set.prototype.has;
const __tbSetAdd = Set.prototype.add;
const __tbOwnKeys = Reflect.ownKeys;
const __tbSetPrototypeOf = Object.setPrototypeOf;
const __tbObjectCreate = Object.create;
const __tbObjectPrototype = Object.prototype;
const __tbStringCtor = String;
const __tbNumberCtor = Number;
const __tbRegExpTest = RegExp.prototype.test;
const __tbHasOwnProperty = Object.prototype.hasOwnProperty;
const __tbJsonParse = JSON.parse;
const __tbJsonStringify = JSON.stringify;
const __tbIteratorSymbol = Symbol.iterator;
const __tbPrivateArray = () => {
  const array = [];
  __tbSetPrototypeOf(array, null);
  return array;
};
// Lazily initializes per-event state for interfaces named by
// `document.createEvent`: an uninitialized event gets its spec defaults on
// first getter read. Lives here because `messaging.js` and `ui_events.js`
// both consume it and any bundle order puts one of them first.
const __tbEventEntry = (event, data, fresh) => {
  const entry = data.get(event);
  if (entry !== undefined) return entry;
  if (!host.__tbIsEvent(event)) throw new TypeError('Illegal invocation');
  const initialized = fresh();
  data.set(event, initialized);
  return initialized;
};
const __tbArray = Object.create(null);
for (const name of ['some', 'sort', 'join', 'indexOf', 'concat']) {
  const method = Array.prototype[name];
  __tbArray[name] = (array, ...args) => __tbApply(method, array, args);
}
__tbArray.push = (array, ...values) => {
  for (let index = 0; index < values.length; index++) {
    __tbDefineProperty(array, array.length, {
      __proto__: null,
      value: values[index], writable: true, enumerable: true, configurable: true,
    });
  }
  return array.length;
};
__tbArray.remove = (array, index) => {
  for (; index + 1 < array.length; index++) array[index] = array[index + 1];
  array.length--;
};
__tbArray.shift = array => {
  if (array.length === 0) return undefined;
  const value = array[0];
  __tbArray.remove(array, 0);
  return value;
};
__tbArray.iterator = array => {
  let index = 0;
  return {
    next() {
      // https://tc39.es/ecma262/#sec-%arrayiteratorprototype%.next
      if (array === null || index >= array.length) {
        array = null;
        return { value: undefined, done: true };
      }
      return { value: array[index++], done: false };
    },
    [__tbIteratorSymbol]() { return this; },
  };
};
const __tbUint8Array = Uint8Array;
// Pinned statics and methods the shims call with attacker-controlled input:
// the constructors stay reachable for pages to replace.
const __tbArrayFrom = Array.from;
const __tbObjectKeys = Object.keys;
const __tbIsArrayBufferView = ArrayBuffer.isView;
const __tbStringSlice = String.prototype.slice;
const __tbStringSplit = String.prototype.split;
const __tbStringTrim = String.prototype.trim;
const __tbStringIncludes = String.prototype.includes;
const __tbStringStartsWith = String.prototype.startsWith;
const __tbStringEndsWith = String.prototype.endsWith;
const __tbStringToLowerCase = String.prototype.toLowerCase;
// ASCII lowercase: only A-Z fold. Unicode folding would wrongly match
// Kelvin K to `k` or Turkish İ to `i` in enumerated keywords
// (<https://infra.spec.whatwg.org/#ascii-lowercase>).
const __tbAsciiLower = value => __tbStringCtor(value).replace(/[A-Z]/g, c => c.toLowerCase());
// Web IDL `DOMString` conversion: `ToString`, which throws on Symbols
// (plain `String()` would stringify them instead)
// (<https://webidl.spec.whatwg.org/#es-DOMString>).
const __tbToDOMString = value => {
  if (typeof value === 'symbol') {
    throw new TypeError('Cannot convert a Symbol value to a string');
  }
  return __tbStringCtor(value);
};
const __tbWeakMapGet = WeakMap.prototype.get;
const __tbWeakMapSet = WeakMap.prototype.set;
const __tbMapConstructor = Map;
const __tbMapGet = Map.prototype.get;
const __tbMapSet = Map.prototype.set;
const __tbMapHas = Map.prototype.has;
const __tbMapDelete = Map.prototype.delete;
const __tbMapClear = Map.prototype.clear;
const __tbMapKeys = Map.prototype.keys;
const __tbMapValues = Map.prototype.values;
const __tbMapEntries = Map.prototype.entries;
const __tbMapSize = Object.getOwnPropertyDescriptor(Map.prototype, 'size').get;
const __tbMapNext = Object.getPrototypeOf(new Map().entries()).next;
const __tbFreeze = Object.freeze;
const __tbPrivateMap = function() {
  const map = new __tbMapConstructor();
  const iterator = method => {
    const iterator = __tbApply(method, map, []);
    return {
      next() {
        const result = __tbApply(__tbMapNext, iterator, []);
        if (!result.done && method === __tbMapEntries) {
          const entry = result.value;
          __tbSetPrototypeOf(entry, null);
          __tbDefineProperty(entry, __tbIteratorSymbol, { __proto__: null, value: () => __tbArray.iterator(entry) });
        }
        return result;
      },
      [__tbIteratorSymbol]() { return this; },
    };
  };
  const facade = {
    __proto__: null,
    get size() { return __tbApply(__tbMapSize, map, []); },
    get(key) { return __tbApply(__tbMapGet, map, [key]); },
    set(key, value) { __tbApply(__tbMapSet, map, [key, value]); return facade; },
    has(key) { return __tbApply(__tbMapHas, map, [key]); },
    delete(key) { return __tbApply(__tbMapDelete, map, [key]); },
    clear() { __tbApply(__tbMapClear, map, []); },
    keys() { return iterator(__tbMapKeys); },
    values() { return iterator(__tbMapValues); },
    entries() { return iterator(__tbMapEntries); },
    [__tbIteratorSymbol]() { return iterator(__tbMapEntries); },
  };
  return __tbFreeze(facade);
};
const __tbPrivateSet = function() {
  const map = new __tbPrivateMap();
  const facade = {
    __proto__: null,
    get size() { return map.size; },
    add(value) { map.set(value, true); return facade; },
    has(value) { return map.has(value); },
    delete(value) { return map.delete(value); },
    clear() { map.clear(); },
    values() { return map.keys(); },
    [__tbIteratorSymbol]() { return map.keys(); },
  };
  return __tbFreeze(facade);
};
const __tbBytesCopy = (bytes, start = 0, end = bytes.length) => {
  // Clamp: an over-long end must not zero-pad (a chunked bridge once turned
  // a 1-byte tail into 4096 bytes of NULs).
  if (end > bytes.length) end = bytes.length;
  if (start > end) start = end;
  const result = new __tbUint8Array(end - start);
  for (let index = start; index < end; index++) result[index - start] = bytes[index];
  return result;
};
__tbArray.map = (array, callback) => {
  const result = [];
  for (let index = 0; index < array.length; index++) {
    __tbDefineProperty(result, index, {
      __proto__: null,
      value: callback(array[index], index), writable: true, enumerable: true, configurable: true,
    });
  }
  return result;
};
__tbArray.slice = (array, start, end) => {
  const length = array.length;
  let first = start === undefined ? 0 : start < 0 ? Math.max(length + start, 0) : Math.min(start, length);
  let last = end === undefined ? length : end < 0 ? Math.max(length + end, 0) : Math.min(end, length);
  const result = [];
  for (; first < last; first++) {
    __tbDefineProperty(result, result.length, {
      __proto__: null, value: array[first], writable: true, enumerable: true, configurable: true,
    });
  }
  return result;
};
__tbArray.splice = (array, start, deleteCount, ...items) => {
  const length = array.length;
  let index = start < 0 ? Math.max(length + start, 0) : Math.min(start, length);
  const count = deleteCount === undefined ? length - index : Math.min(Math.max(deleteCount, 0), length - index);
  const removed = [];
  for (let offset = 0; offset < count; offset++) {
    __tbDefineProperty(removed, offset, {
      __proto__: null, value: array[index + offset], writable: true, enumerable: true, configurable: true,
    });
  }
  for (let offset = index; offset + count < length; offset++) array[offset] = array[offset + count];
  array.length = length - count;
  for (let offset = 0; offset < items.length; offset++) {
    for (let move = array.length; move > index + offset; move--) array[move] = array[move - 1];
    array[index + offset] = items[offset];
  }
  return removed;
};
__tbArray.find = (array, callback) => {
  for (let index = 0; index < array.length; index++) {
    if (callback(array[index], index)) return array[index];
  }
  return undefined;
};
__tbArray.findIndex = (array, callback) => {
  for (let index = 0; index < array.length; index++) {
    if (callback(array[index], index)) return index;
  }
  return -1;
};
__tbArray.filter = (array, callback) => {
  const result = [];
  for (let index = 0; index < array.length; index++) {
    if (callback(array[index], index)) {
      __tbDefineProperty(result, result.length, {
        __proto__: null,
        value: array[index], writable: true, enumerable: true, configurable: true,
      });
    }
  }
  return result;
};
