host.__tb_getOwnPropertyNames = __tbGetOwnPropertyNames;
host.__tb_getOwnPropertySymbols = __tbGetOwnPropertySymbols;
host.__tb_getOwnPropertyDescriptor = __tbGetOwnPropertyDescriptor;
host.__tb_getPrototypeOf = __tbGetPrototypeOf;
host.__tb_isArray = __tbIsArray;
host.__tb_symbolToString = __tbSymbolToString;
host.__tb_set = __tbSetConstructor;
host.__tb_set_has = __tbSetHas;
host.__tb_set_add = __tbSetAdd;

host.__tb_handle_group = __tbObjectCreate(null);
host.__tb_groups = __tbObjectCreate(null);

const __tbRawStringify = host.stringify;
const __tbSafeStringify = (value, depth = 0) => {
  if (depth > 150) {
    throw new Error('Cannot serialize result: object reference chain is too long.');
  }
  if (value === null) return 'null';
  const kind = typeof value;
  if (kind === 'boolean') return value ? 'true' : 'false';
  if (kind === 'number') return __tbNumberIsFinite(value) ? __tbStringCtor(value) : 'null';
  if (kind === 'string') return host.apply(__tbRawStringify, null, [value]);
  if (kind !== 'object') return undefined;
  if (__tbIsArray(value)) {
    let out = '[';
    for (let i = 0; i < value.length; i++) {
      if (i > 0) out += ',';
      const item = __tbSafeStringify(value[i], depth + 1);
      out += item === undefined ? 'null' : item;
    }
    return out + ']';
  }
  const keys = __tbGetOwnPropertyNames(value);
  let out = '{';
  let first = true;
  for (let i = 0; i < keys.length; i++) {
    const key = keys[i];
    const desc = __tbGetOwnPropertyDescriptor(value, key);
    if (!desc || !desc.enumerable) continue;
    const encoded = __tbSafeStringify(value[key], depth + 1);
    if (encoded === undefined) continue;
    if (!first) out += ',';
    first = false;
    out += host.apply(__tbRawStringify, null, [key]) + ':' + encoded;
  }
  return out + '}';
};
host.stringify = value => __tbSafeStringify(value, 0);

const __tbIsNode = value => typeof value === 'object' && value !== null &&
  typeof value.nodeType === 'number' && typeof value.nodeName === 'string';

const __tbClassName = value => {
  const prototype = __tbGetPrototypeOf(value);
  if (prototype === null) return 'Object';
  const descriptor = __tbGetOwnPropertyDescriptor(prototype, 'constructor');
  const constructor = descriptor && descriptor.value;
  if (typeof constructor !== 'function') return 'Object';
  const name = __tbGetOwnPropertyDescriptor(constructor, 'name');
  if (!name || typeof name.value !== 'string' || name.value === '') return 'Object';
  return name.value;
};

const __tbPreview = (value, type, subtype) => {
  const properties = [];
  const names = __tbGetOwnPropertyNames(value);
  let overflow = false;
  for (let index = 0; index < names.length; index++) {
    if (index === 5) { overflow = true; break; }
    const descriptor = __tbGetOwnPropertyDescriptor(value, names[index]);
    if (!descriptor) continue;
    if ('value' in descriptor) {
      const kind = typeof descriptor.value;
      const entry = { __proto__: null, name: names[index], type: kind };
      if (descriptor.value === null) {
        entry.type = 'object';
        entry.subtype = 'null';
        entry.value = 'null';
      } else if (kind !== 'object' && kind !== 'function') {
        entry.value = __tbStringCtor(descriptor.value);
      }
      properties[properties.length] = entry;
    } else {
      properties[properties.length] = { __proto__: null, name: names[index], type: 'accessor' };
    }
  }
  const preview = { __proto__: null, type: type, overflow: overflow, properties: properties };
  if (subtype) preview.subtype = subtype;
  return preview;
};

host.remoteObject = (value, options) => {
  options = options || {};
  if (value === null) return { __proto__: null, type: 'object', subtype: 'null', value: null };
  const kind = typeof value;
  if (kind === 'undefined') return { __proto__: null, type: 'undefined' };
  if (kind === 'number') {
    if (__tbNumberIsFinite(value)) return { __proto__: null, type: 'number', value: value };
    return { __proto__: null, type: 'number', unserializableValue: __tbNumberIsNaN(value) ? 'NaN' : (value > 0 ? 'Infinity' : '-Infinity') };
  }
  if (kind === 'string' || kind === 'boolean') return { __proto__: null, type: kind, value: value };
  if (kind === 'bigint') {
    return { __proto__: null, type: 'bigint', unserializableValue: __tbStringCtor(value) };
  }
  if (options.returnByValue) {
    return { __proto__: null, type: kind === 'function' ? 'function' : 'object', value: value };
  }
  const objectId = host.__tb_mint();
  host.__tb_handles[objectId] = value;
  const group = typeof options.objectGroup === 'string' ? options.objectGroup : '';
  host.__tb_handle_group[objectId] = group;
  let members = host.__tb_groups[group];
  if (!members) {
    members = __tbObjectCreate(null);
    host.__tb_groups[group] = members;
  }
  members[objectId] = true;

  const type = kind === 'symbol' ? 'symbol' : (kind === 'function' ? 'function' : 'object');
  const object = { __proto__: null, type: type, objectId: objectId };
  let subtype = null;
  if (kind === 'symbol') {
    object.description = __tbApply(__tbSymbolToString, value, []);
  } else if (kind === 'function') {
    object.className = 'Function';
  } else if (__tbIsArray(value)) {
    subtype = 'array';
    object.className = 'Array';
  } else if (__tbIsNode(value)) {
    subtype = 'node';
    object.className = __tbClassName(value);
    object.description = value.nodeName;
  } else {
    object.className = __tbClassName(value);
  }
  if (subtype) object.subtype = subtype;
  if (options.generatePreview && kind === 'object') {
    object.preview = __tbPreview(value, type, subtype);
  }
  return object;
};

host.__tb_release_handle = objectId => {
  if (objectId in host.__tb_handles) delete host.__tb_handles[objectId];
  const group = host.__tb_handle_group[objectId];
  if (group !== undefined) {
    const members = host.__tb_groups[group];
    if (members && (objectId in members)) delete members[objectId];
    delete host.__tb_handle_group[objectId];
  }
};

host.__tb_release_group = objectGroup => {
  const members = host.__tb_groups[objectGroup];
  if (!members) return;
  const ids = __tbGetOwnPropertyNames(members);
  for (let index = 0; index < ids.length; index++) {
    delete host.__tb_handles[ids[index]];
    delete host.__tb_handle_group[ids[index]];
  }
  delete host.__tb_groups[objectGroup];
};

host.__tb_clear_handles = () => {
  const ids = __tbGetOwnPropertyNames(host.__tb_handles);
  for (let index = 0; index < ids.length; index++) delete host.__tb_handles[ids[index]];
  const groups = __tbGetOwnPropertyNames(host.__tb_groups);
  for (let index = 0; index < groups.length; index++) delete host.__tb_groups[groups[index]];
  const grouped = __tbGetOwnPropertyNames(host.__tb_handle_group);
  for (let index = 0; index < grouped.length; index++) delete host.__tb_handle_group[grouped[index]];
};
