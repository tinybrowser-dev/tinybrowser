const __tbIDLString = value => {
  // https://webidl.spec.whatwg.org/#es-DOMString
  if (typeof value === 'symbol') throw new TypeError('cannot convert a Symbol to DOMString');
  return String(value);
};
const __tbIDLCharCodeAt = String.prototype.charCodeAt;
const __tbIDLFromCharCode = String.fromCharCode;
const __tbIDLFromCodePoint = String.fromCodePoint;
const __tbIDLUSVString = value => {
  // https://webidl.spec.whatwg.org/#es-USVString
  const string = __tbIDLString(value);
  let result = '';
  for (let index = 0; index < string.length; index++) {
    const code = __tbApply(__tbIDLCharCodeAt, string, [index]);
    if (code >= 0xd800 && code <= 0xdbff) {
      const next = __tbApply(__tbIDLCharCodeAt, string, [index + 1]);
      if (next >= 0xdc00 && next <= 0xdfff) {
        result += string[index] + string[++index];
      } else {
        result += '\ufffd';
      }
    } else {
      result += code >= 0xdc00 && code <= 0xdfff ? '\ufffd' : string[index];
    }
  }
  return result;
};
const __tbIDLDescriptors = Object.getOwnPropertyDescriptors;
const __tbIDLDescriptor = Object.getOwnPropertyDescriptor;
const __tbIDLGet = Reflect.get;
const __tbIDLSetPrototypeOf = Object.setPrototypeOf;
const __tbIDLInstanceOf = Function.prototype[Symbol.hasInstance];
const __tbIDLTypedArray = Object.getPrototypeOf(Uint8Array.prototype);
const __tbIDLTypedArrayTag = __tbIDLDescriptor(__tbIDLTypedArray, Symbol.toStringTag).get;
const __tbIDLTypedArrayLength = __tbIDLDescriptor(__tbIDLTypedArray, 'length').get;
const __tbIDLTypedArrayBuffer = __tbIDLDescriptor(__tbIDLTypedArray, 'buffer').get;
const __tbIDLTypedArrayByteOffset = __tbIDLDescriptor(__tbIDLTypedArray, 'byteOffset').get;
const __tbIDLTypedArrayByteLength = __tbIDLDescriptor(__tbIDLTypedArray, 'byteLength').get;
const __tbIDLTypedArraySet = __tbIDLTypedArray.set;
const __tbIDLDataViewBuffer = __tbIDLDescriptor(DataView.prototype, 'buffer').get;
const __tbIDLBufferLength = __tbIDLDescriptor(ArrayBuffer.prototype, 'byteLength').get;
const __tbIDLResizable = __tbIDLDescriptor(ArrayBuffer.prototype, 'resizable')?.get;
const __tbIDLSharedPrototype = globalThis.SharedArrayBuffer?.prototype;
const __tbIDLSharedLength = __tbIDLSharedPrototype && __tbIDLDescriptor(__tbIDLSharedPrototype, 'byteLength').get;
const __tbIDLGrowable = __tbIDLSharedPrototype && __tbIDLDescriptor(__tbIDLSharedPrototype, 'growable')?.get;
const __tbIDLBufferSource = (value, allowShared) => {
  // https://webidl.spec.whatwg.org/#es-buffer-source-types
  let buffer;
  try {
    __tbApply(__tbIDLTypedArrayTag, value, []);
    buffer = __tbApply(__tbIDLTypedArrayBuffer, value, []);
  } catch {
    buffer = undefined;
  }
  if (buffer === undefined) {
    try {
      buffer = __tbApply(__tbIDLDataViewBuffer, value, []);
    } catch {
      throw new TypeError('value is not an ArrayBufferView');
    }
  }
  let shared = false;
  try { __tbApply(__tbIDLBufferLength, buffer, []); }
  catch (error) {
    if (!allowShared || !__tbIDLSharedLength) throw error;
    __tbApply(__tbIDLSharedLength, buffer, []);
    shared = true;
  }
  if (shared) {
    if (__tbIDLGrowable && __tbApply(__tbIDLGrowable, buffer, [])) {
      throw new TypeError('growable buffers are not allowed');
    }
  } else if (__tbIDLResizable && __tbApply(__tbIDLResizable, buffer, [])) {
    throw new TypeError('resizable buffers are not allowed');
  }
  return value;
};
const __tbIDLUint8Array = (value, allowShared) => {
  if (__tbApply(__tbIDLTypedArrayTag, value, []) !== 'Uint8Array') {
    throw new TypeError('value must be a Uint8Array');
  }
  return __tbIDLBufferSource(value, allowShared);
};
const __tbIDLRealmUint8Array = (value, allowShared, realm) => {
  // https://webidl.spec.whatwg.org/#es-buffer-source-types
  value = __tbIDLUint8Array(value, allowShared);
  if (__tbApply(__tbIDLInstanceOf, realm.Uint8Array, [value])) return value;
  const length = __tbApply(__tbIDLTypedArrayLength, value, []);
  const copy = new realm.Uint8Array(length);
  for (let index = 0; index < length; index++) copy[index] = value[index];
  return copy;
};
const __tbIDLResult = (value, type) => {
  if (typeof value !== type) throw new TypeError('algorithm result does not match IDL');
  return value;
};
const __tbIDLInteger = Number.isInteger;
const __tbIDLBoolean = value => Boolean(value);
const __tbIDLUnsignedLong = value => value >>> 0;
const __tbIDLUnsignedLongLong = value => {
  // https://webidl.spec.whatwg.org/#es-unsigned-long-long
  const number = Number(value);
  if (!__tbIDLIsFinite(number) || number === 0) return 0;
  const truncated = Math.trunc(number);
  if (truncated < 0 || truncated >= 2 ** 64) {
    throw new TypeError('value is out of range for unsigned long long');
  }
  return truncated;
};
const __tbIDLLong = value => value | 0;
const __tbIDLDouble = value => Number(value);
const __tbIDLIsFinite = Number.isFinite;
const __tbIDLRestrictedDouble = value => {
  const number = Number(value);
  if (!__tbIDLIsFinite(number)) throw new TypeError('value is not a finite double');
  return number;
};
const __tbIDLEnum = (value, keywords) => {
  const string = __tbIDLString(value);
  for (let index = 0; index < keywords.length; index++) {
    if (string === keywords[index]) return string;
  }
  throw new TypeError('value is not a valid enumeration keyword');
};
const __tbIDLIteratorNext = Object.getPrototypeOf([][Symbol.iterator]()).next;
const __tbIDLSequence = (value, convert) => {
  // https://webidl.spec.whatwg.org/#es-sequence
  if (value === null || value === undefined) return [];
  if (typeof value !== 'object' && typeof value !== 'function') {
    throw new TypeError('value is not iterable');
  }
  const iteratorMethod = value[Symbol.iterator];
  if (typeof iteratorMethod !== 'function') throw new TypeError('value is not iterable');
  const iterator = __tbApply(iteratorMethod, value, []);
  const result = [];
  while (true) {
    const step = __tbApply(__tbIDLIteratorNext, iterator, []);
    if (step.done) return result;
    result[result.length] = convert(step.value);
  }
};
const __tbIDLSequenceResult = (value, convert) => {
  const result = [];
  for (let index = 0; index < value.length; index++) {
    result[result.length] = convert(value[index]);
  }
  return result;
};
const __tbIDLFrozenArrayResult = (value, convert) =>
  Object.freeze(__tbIDLSequenceResult(value, convert));
const __tbIDLPromise = (value, convert) => Promise.resolve(value).then(convert);
const __tbIDLResultNumber = value => {
  if (typeof value !== 'number') throw new TypeError('algorithm result does not match IDL');
  return value;
};
const __tbIDLResultEnum = (value, keywords) => {
  if (typeof value !== 'string') throw new TypeError('algorithm result does not match IDL');
  for (let index = 0; index < keywords.length; index++) {
    if (value === keywords[index]) return value;
  }
  throw new TypeError('algorithm result is not an enumeration keyword');
};
const __tbIDLResultDictionary = (value, fields) => {
  // https://webidl.spec.whatwg.org/#es-to-js-dictionary
  const result = {};
  for (let index = 0; index < fields.length; index++) {
    const name = fields[index][0];
    const member = value[name];
    if (member === undefined) continue;
    __tbDefineProperty(result, name, {
      __proto__: null,
      value: fields[index][1](member), writable: true, enumerable: true, configurable: true,
    });
  }
  return result;
};
const __tbIDLDictionary = (value, fields) => {
  // https://webidl.spec.whatwg.org/#es-dictionary
  if (value === null || value === undefined) value = {};
  if (typeof value !== 'object' && typeof value !== 'function') {
    throw new TypeError('value is not a dictionary');
  }
  const result = {};
  for (let index = 0; index < fields.length; index++) {
    const name = fields[index][0];
    const member = value[name];
    if (member === undefined) {
      if (fields[index][2]) throw new TypeError(name + ' is required');
      continue;
    }
    result[name] = fields[index][1](member);
  }
  return result;
};
const __tbInstallInterface = (implementation, name, construction, members) => {
  // https://webidl.spec.whatwg.org/#es-interface
  const brand = host.slots('webidl.' + name);
  const realm = { Uint8Array: __tbUint8Array };
  const secureContext = host.__tbSecureContext === true;
  const algorithms = __tbIDLDescriptors(implementation.prototype);
  // https://webidl.spec.whatwg.org/#interface-prototype-object
  const prototype = {};
  for (const member of members) {
    if (member.exposure.secureContext && !secureContext) continue;
    const descriptor = algorithms[member.name];
    const algorithm = member.kind === 'method' ? descriptor.value : descriptor[member.kind];
    const operation = {
      invoke(...args) {
        // https://webidl.spec.whatwg.org/#es-operations
        // https://webidl.spec.whatwg.org/#es-attributes
        if (!brand.has(this)) throw new TypeError('Illegal invocation');
        if (args.length < member.count) {
          throw new TypeError(name + ': insufficient arguments');
        }
        return member.result(__tbApply(algorithm, this, member.convert(args)), brand.get(this));
      },
    }.invoke;
    __tbDefineProperty(operation, 'name', { __proto__: null, value: member.kind === 'method' ? member.name : member.kind + ' ' + member.name, configurable: true });
    __tbDefineProperty(operation, 'length', { __proto__: null, value: member.count, configurable: true });
    const property = { __proto__: null, enumerable: true, configurable: true };
    if (member.kind === 'method') {
      property.value = operation;
      property.writable = true;
    } else {
      const prior = __tbIDLDescriptor(prototype, member.name);
      property.get = member.kind === 'get' ? operation : prior?.get;
      property.set = member.kind === 'set' ? operation : prior?.set;
    }
    __tbDefineProperty(prototype, member.name, property);
  }
  const constructor = function(...args) {
    // https://webidl.spec.whatwg.org/#es-interface-call
    if (new.target === undefined || construction === null) throw new TypeError('Illegal constructor');
    if (args.length < construction[0]) throw new TypeError(name + ': insufficient arguments');
    const converted = construction[1](args);
    // https://webidl.spec.whatwg.org/#internally-create-a-new-object-implementing-the-interface
    const target = __tbIDLGet(new.target, 'prototype');
    const value = __tbConstruct(implementation, [converted], implementation);
    const chosen = target !== null && (typeof target === 'object' || typeof target === 'function')
      ? target
      : prototype;
    __tbIDLSetPrototypeOf(value, chosen);
    brand.set(value, realm);
    return value;
  };
  __tbDefineProperty(constructor, 'name', { __proto__: null, value: name, configurable: true });
  __tbDefineProperty(constructor, 'length', { __proto__: null, value: construction === null ? 0 : construction[0], configurable: true });
  __tbDefineProperty(constructor, 'prototype', { __proto__: null, value: prototype, writable: false });
  __tbDefineProperty(prototype, 'constructor', { __proto__: null, value: constructor, writable: true, configurable: true });
  __tbDefineProperty(prototype, Symbol.toStringTag, { __proto__: null, value: name, configurable: true });
  if (construction === null) {
    const instance = __tbIDLSetPrototypeOf({}, prototype);
    brand.set(instance, realm);
    return [constructor, instance];
  }
  return constructor;
};
const __tbInstallGlobal = (implementation, name, construction, members, secureOnly) => {
  // https://webidl.spec.whatwg.org/#es-interfaces
  const value = __tbInstallInterface(implementation, name, construction, members);
  if (!secureOnly || host.__tbSecureContext === true) {
    const global = construction === null ? value[0] : value;
    __tbDefineProperty(globalThis, name, {
      __proto__: null, value: global, writable: true, configurable: true,
    });
  }
  return value;
};

const __tbEventConstructor = globalThis.Event;
const __tbEventTargetConstructor = globalThis.EventTarget;
const __tbBrand = (value, slots, message) => {
  const data = slots.get(value);
  if (data === undefined) throw new TypeError(message === undefined ? 'Illegal invocation' : message);
  return data;
};
const __tbWindowData = host.slots('tinybrowser.window.data');
const __tbIsWindow = value =>
  value === globalThis ||
  (value !== null && typeof value === 'object' && __tbWindowData.get(value) !== undefined);
