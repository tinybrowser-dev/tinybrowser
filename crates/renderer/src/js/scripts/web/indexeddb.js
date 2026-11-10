// https://w3c.github.io/IndexedDB/
//
// The first IndexedDB slice: the factory, database open/upgrade, and object
// store records. The host seam stores opaque encoded rows in the profile's
// IndexedDB area, so the browser owns the data and every renderer of a
// profile sees one store, like `localStorage`.
//
// Not implemented yet: indexes, cursors, upgrade blocking between open
// connections, and a live key generator shared across connections. A missing
// member is absent, so a page gets a TypeError instead of a result.
//
// Rows:
//   m\0<database>                     metadata, JSON
//   r\0<database>\0<store>\0<key>      one record, JSON { key, value }
//
// The key is a JSON-encoded key object. The shim sorts and ranges in
// JavaScript, so the encoded form only has to be unique and reversible.

const __tbIdbRequestState = host.slots('tinybrowser.indexeddb.request');
const __tbIdbFactoryState = host.slots('tinybrowser.indexeddb.factory');
const __tbIdbDatabaseState = host.slots('tinybrowser.indexeddb.database');
const __tbIdbTransactionState = host.slots('tinybrowser.indexeddb.transaction');
const __tbIdbObjectStoreState = host.slots('tinybrowser.indexeddb.objectstore');
const __tbIdbKeyRangeState = host.slots('tinybrowser.indexeddb.keyrange');

const __tbIdbSetTimeout = globalThis.setTimeout;
const __tbIdbTrunc = Math.trunc;
const __tbIdbFloor = Math.floor;
const __tbIdbMaxSafeInteger = Number.MAX_SAFE_INTEGER;
const __tbIdbMetaPrefix = 'm\u0000';
const __tbIdbRecordPrefix = 'r\u0000';
const __tbIdbArea = 'indexeddb';

const __tbIdbStartsWith = (text, prefix) => __tbApply(__tbStringStartsWith, text, [prefix]);
const __tbIdbSlice = (text, start) => __tbApply(__tbStringSlice, text, [start]);
const __tbIdbSplit = (text, separator) => __tbApply(__tbStringSplit, text, [separator]);

/// Runs `callback` as a task, never as a microtask: IndexedDB results and
/// events are tasks
/// (<https://w3c.github.io/IndexedDB/#tasks>).
const __tbIdbTask = callback => {
  __tbIdbSetTimeout(callback, 0);
};

/// Constructs an instance of `constructor` without running its illegal-
/// constructor body, the way the shim builds every host-created object.
const __tbIdbConstruct = constructor => {
  const parent = constructor.prototype instanceof __tbEventTargetConstructor
    ? __tbEventTargetConstructor
    : Object;
  return __tbConstruct(parent, [], constructor);
};

const __tbIdbSecurityError = () => new globalThis.DOMException(
  'access to the Indexed Database API is denied in this context.',
  'SecurityError',
);
const __tbIdbDataError = () => new globalThis.DOMException(
  'The parameter is not a valid key.',
  'DataError',
);
const __tbIdbStateError = () => new globalThis.DOMException(
  'The database is not running a version change transaction.',
  'InvalidStateError',
);

/// Whether this realm has an origin a database can be filed under. An opaque
/// origin has no storage key, so `open()` and `deleteDatabase()` throw and
/// `databases()` rejects
/// (<https://w3c.github.io/IndexedDB/#dom-idbfactory-open>).
const __tbIdbOpaque = () => {
  if (typeof host.__tbStorageOrigin !== 'function') return true;
  const origin = host.__tbStorageOrigin();
  return origin === null || origin === undefined;
};

/// An ECMAScript `IdentifierName`
/// (<https://tc39.es/ecma262/#prod-IdentifierName>).
const __tbIdbIdentifierName = /^[$_\p{ID_Start}][$\u200c\u200d\p{ID_Continue}]*$/u;

/// A valid key path string: empty, or one or more identifiers separated by
/// periods (<https://w3c.github.io/IndexedDB/#valid-key-path>).
const __tbIdbValidKeyPathString = text => {
  if (text === '') return true;
  const parts = __tbIdbSplit(text, '.');
  for (let index = 0; index < parts.length; index++) {
    if (!__tbApply(__tbRegExpTest, __tbIdbIdentifierName, [parts[index]])) return false;
  }
  return true;
};

/// A valid key path: a valid string, or a non-empty sequence of valid strings
/// (<https://w3c.github.io/IndexedDB/#valid-key-path>).
const __tbIdbValidKeyPath = keyPath => {
  if (__tbIsArray(keyPath)) {
    if (keyPath.length === 0) return false;
    for (let index = 0; index < keyPath.length; index++) {
      const part = keyPath[index];
      if (typeof part !== 'string' || !__tbIdbValidKeyPathString(part)) return false;
    }
    return true;
  }
  return typeof keyPath === 'string' && __tbIdbValidKeyPathString(keyPath);
};

/// The WebIDL union `(DOMString or sequence<DOMString>)` conversion used by
/// `keyPath`: an iterable object becomes a sequence of strings, every other
/// value becomes a string
/// (<https://webidl.spec.whatwg.org/#es-union>).
const __tbIdbConvertKeyPath = value => {
  if (value === undefined) return null;
  if (
    value !== null
    && typeof value === 'object'
    && typeof value[__tbIteratorSymbol] === 'function'
  ) {
    const list = [];
    for (const entry of value) __tbArray.push(list, String(entry));
    return list;
  }
  return String(value);
};

/// Evaluates a key path against a value, returning the value that becomes the
/// key; `undefined` means the path names no value
/// (<https://w3c.github.io/IndexedDB/#evaluate-a-key-path>).
const __tbIdbEvaluate = (value, keyPath) => {
  if (__tbIsArray(keyPath)) {
    const parts = [];
    for (let index = 0; index < keyPath.length; index++) {
      const part = __tbIdbEvaluate(value, keyPath[index]);
      if (part === undefined) return undefined;
      __tbArray.push(parts, part);
    }
    return parts;
  }
  if (keyPath === '') return value;
  let current = value;
  const parts = __tbIdbSplit(keyPath, '.');
  for (let index = 0; index < parts.length; index++) {
    if (current === null || current === undefined) return undefined;
    if (typeof current !== 'object' && typeof current !== 'function') return undefined;
    current = current[parts[index]];
    if (current === undefined) return undefined;
  }
  return current;
};

/// Converts a JavaScript value to a key object, or returns `undefined` when
/// the value is not a valid key
/// (<https://w3c.github.io/IndexedDB/#convert-value-to-key>).
const __tbIdbToKey = (value, seen) => {
  const type = typeof value;
  if (type === 'number') {
    if (__tbNumberIsNaN(value)) return undefined;
    return { __proto__: null, t: 'number', v: value === 0 ? 0 : value };
  }
  if (type === 'string') return { __proto__: null, t: 'string', v: value };
  if (type !== 'object' || value === null) return undefined;
  if (__tbIsDate(value)) {
    const ms = value.getTime();
    if (__tbNumberIsNaN(ms)) return undefined;
    return { __proto__: null, t: 'date', v: ms };
  }
  if (__tbIsArray(value)) {
    if (seen.has(value)) return undefined;
    seen.add(value);
    const items = [];
    for (let index = 0; index < value.length; index++) {
      const item = __tbIdbToKey(value[index], seen);
      if (item === undefined) {
        seen.delete(value);
        return undefined;
      }
      __tbArray.push(items, item);
    }
    seen.delete(value);
    return { __proto__: null, t: 'array', v: items };
  }
  let bytes;
  try {
    if (__tbIsArrayBuffer(value)) {
      bytes = new __tbUint8Array(value);
    } else if (__tbIsTypedArray(value) || __tbIsDataView(value)) {
      bytes = new __tbUint8Array(value.buffer, value.byteOffset, value.byteLength);
    } else {
      return undefined;
    }
  } catch (error) {
    return undefined;
  }
  return { __proto__: null, t: 'binary', v: __tbBase64Encode(bytes) };
};

/// The type rank that orders numbers before dates before strings before
/// binary keys before arrays
/// (<https://w3c.github.io/IndexedDB/#compare-two-keys>).
const __tbIdbRank = key => {
  switch (key.t) {
    case 'number': return 0;
    case 'date': return 1;
    case 'string': return 2;
    case 'binary': return 3;
    case 'array': return 4;
  }
};

const __tbIdbCompare = (first, second) => {
  const firstRank = __tbIdbRank(first);
  const secondRank = __tbIdbRank(second);
  if (firstRank !== secondRank) return firstRank < secondRank ? -1 : 1;
  switch (first.t) {
    case 'number':
    case 'date':
    case 'string':
      if (first.v === second.v) return 0;
      return first.v < second.v ? -1 : 1;
    case 'binary': {
      const left = __tbBase64Bytes(first.v);
      const right = __tbBase64Bytes(second.v);
      const length = left.length < right.length ? left.length : right.length;
      for (let index = 0; index < length; index++) {
        if (left[index] !== right[index]) return left[index] < right[index] ? -1 : 1;
      }
      if (left.length === right.length) return 0;
      return left.length < right.length ? -1 : 1;
    }
    case 'array': {
      const length = first.v.length < second.v.length ? first.v.length : second.v.length;
      for (let index = 0; index < length; index++) {
        const order = __tbIdbCompare(first.v[index], second.v[index]);
        if (order !== 0) return order;
      }
      if (first.v.length === second.v.length) return 0;
      return first.v.length < second.v.length ? -1 : 1;
    }
  }
  return 0;
};

const __tbIdbEncodeKey = key => __tbJsonStringify(key);
const __tbIdbDecodeKey = text => __tbJsonParse(text);

/// The JavaScript value of a key. Keys live as tagged objects so they compare
/// and encode stably, but every result and range bound exposes the ordinary
/// value (<https://w3c.github.io/IndexedDB/#key-construct>).
const __tbIdbKeyValue = key => {
  switch (key.t) {
    case 'number':
      return key.v;
    case 'date':
      return new Date(key.v);
    case 'string':
      return key.v;
    case 'binary':
      return __tbBase64Bytes(key.v).buffer;
    case 'array': {
      const items = [];
      for (let index = 0; index < key.v.length; index++) {
        __tbArray.push(items, __tbIdbKeyValue(key.v[index]));
      }
      return items;
    }
  }
};

const __tbIdbRangeIncludes = (range, key) => {
  if (range.lower !== null) {
    const order = __tbIdbCompare(key, range.lower);
    if (order < 0 || (order === 0 && range.lowerOpen)) return false;
  }
  if (range.upper !== null) {
    const order = __tbIdbCompare(key, range.upper);
    if (order > 0 || (order === 0 && range.upperOpen)) return false;
  }
  return true;
};

const __tbIdbReadMeta = name => {
  const text = host.__tbStorageGet(__tbIdbArea, __tbIdbMetaPrefix + name);
  if (text === null || text === undefined) return null;
  return __tbJsonParse(text);
};

const __tbIdbWriteMeta = (name, meta) => {
  if (!host.__tbStorageSet(__tbIdbArea, __tbIdbMetaPrefix + name, __tbJsonStringify(meta))) {
    throw new globalThis.QuotaExceededError('The database could not be written.');
  }
};

const __tbIdbRemoveMeta = name => {
  host.__tbStorageRemove(__tbIdbArea, __tbIdbMetaPrefix + name);
};

const __tbIdbRecordPrefixFor = (name, store) =>
  __tbIdbRecordPrefix + name + '\u0000' + store + '\u0000';

const __tbIdbAllKeys = () => host.__tbStorageKeys(__tbIdbArea);

const __tbIdbAddRecordKeys = (keys, prefix, rows) => {
  for (let index = 0; index < keys.length; index++) {
    const rowKey = keys[index];
    if (!__tbIdbStartsWith(rowKey, prefix)) continue;
    const text = host.__tbStorageGet(__tbIdbArea, rowKey);
    if (text === null || text === undefined) continue;
    const row = __tbJsonParse(text);
    rows.set(__tbIdbEncodeKey(row.key), row);
  }
};

const __tbIdbNewStoreState = (transaction, meta, initial) => {
  const rows = new __tbPrivateMap();
  for (const entry of initial.entries()) rows.set(entry[0], entry[1]);
  return { __proto__: null, transaction, meta, rows, initial, deleted: false };
};

/// Builds the store records for one transaction scope. A transaction reads one
/// snapshot of its stores at creation; every later operation works on that
/// snapshot and commit writes the difference
/// (<https://w3c.github.io/IndexedDB/#transaction-construct>).
const __tbIdbNewTransaction = (db, names, mode) => {
  const transaction = __tbIdbConstruct(__tbIdbTransactionConstructor);
  const data = {
    __proto__: null,
    transaction,
    db,
    mode,
    scope: [],
    stores: __tbPrivateMap(),
    handles: __tbPrivateMap(),
    queue: [],
    running: false,
    scheduled: false,
    committing: false,
    finished: false,
    aborted: false,
    error: null,
    versionChange: mode === 'versionchange',
    removedStores: [],
    oldMeta: null,
    onComplete: null,
    onAbort: null,
  };
  __tbIdbTransactionState.set(transaction, data);
  // A normal transaction auto-commits when control returns to the event loop
  // with no queued requests, so a transaction created and never used still
  // finishes (<https://w3c.github.io/IndexedDB/#cleanup-indexed-database-transactions>).
  // A versionchange transaction is driven by the upgrade algorithm instead.
  if (mode !== 'versionchange') __tbIdbSchedule(data);
  const keys = __tbIdbAllKeys();
  const stores = data.stores;
  const scope = data.scope;
  const dbState = __tbIdbDatabaseState.get(db);
  const allMeta = dbState.meta;
  const dbName = dbState.name;
  for (let index = 0; index < names.length; index++) {
    const name = names[index];
    const storeMeta = __tbArray.find(allMeta.stores, store => store.name === name);
    if (storeMeta === undefined) {
      throw new globalThis.DOMException(`The object store '${name}' does not exist.`, 'NotFoundError');
    }
    if (__tbArray.indexOf(scope, name) !== -1) continue;
    __tbArray.push(scope, name);
    const initial = new __tbPrivateMap();
    __tbIdbAddRecordKeys(keys, __tbIdbRecordPrefixFor(dbName, name), initial);
    const state = __tbIdbNewStoreState(data, __tbJsonParse(__tbJsonStringify(storeMeta)), initial);
    stores.set(name, state);
  }
  // A versionchange transaction starts with every existing store in scope
  // (<https://w3c.github.io/IndexedDB/#upgrade-a-database>).
  if (mode === 'versionchange') {
    for (let index = 0; index < allMeta.stores.length; index++) {
      const storeMeta = allMeta.stores[index];
      if (__tbArray.indexOf(scope, storeMeta.name) !== -1) continue;
      __tbArray.push(scope, storeMeta.name);
      const initial = new __tbPrivateMap();
      __tbIdbAddRecordKeys(keys, __tbIdbRecordPrefixFor(dbName, storeMeta.name), initial);
      stores.set(storeMeta.name, __tbIdbNewStoreState(
        data, __tbJsonParse(__tbJsonStringify(storeMeta)), initial));
    }
  }
  return transaction;
};

const __tbIdbQueue = (data, request) => {
  __tbArray.push(data.queue, request);
  if (!data.running && !data.finished) __tbIdbSchedule(data);
};

const __tbIdbSchedule = data => {
  if (data.scheduled || data.finished) return;
  data.scheduled = true;
  __tbIdbTask(() => {
    data.scheduled = false;
    __tbIdbRun(data);
  });
};

const __tbIdbRun = data => {
  if (data.finished || data.running) return;
  if (data.queue.length === 0) {
    __tbIdbCommit(data);
    return;
  }
  data.running = true;
  const request = __tbArray.shift(data.queue);
  __tbIdbTask(() => __tbIdbPerform(data, request));
};

const __tbIdbAbort = (data, error) => {
  if (data.finished) return;
  data.finished = true;
  data.aborted = true;
  data.error = error === undefined ? null : error;
  data.queue.length = 0;
  if (data.versionChange) {
    const dbState = __tbIdbDatabaseState.get(data.db);
    if (dbState !== undefined) dbState.transaction = null;
  }
  __tbIdbTask(() => {
    __tbDispatch(data.transaction, new __tbEventConstructor('abort'));
    if (data.onAbort !== null) data.onAbort();
  });
};

const __tbIdbComplete = (data, request, error, result) => {
  const state = __tbIdbRequestState.get(request);
  state.done = true;
  if (error === null) {
    state.result = result;
    state.error = null;
    __tbDispatch(request, new __tbEventConstructor('success'));
  } else {
    state.error = error;
    const event = new __tbEventConstructor('error', { cancelable: true });
    const canceled = __tbDispatch(request, event);
    if (!canceled) {
      __tbIdbAbort(data, error);
      return;
    }
  }
  // With no request left, the transaction is now committing: `abort()` from a
  // later task must throw, and a new request is no longer allowed
  // (<https://w3c.github.io/IndexedDB/#cleanup-indexed-database-transactions>).
  if (!data.finished && data.queue.length === 0) data.committing = true;
  data.running = false;
  if (!data.finished) __tbIdbSchedule(data);
};

const __tbDispatch = (target, event) => {
  try {
    return target.dispatchEvent(event) === false;
  } catch (error) {
    return false;
  }
};

const __tbIdbPerform = (data, request) => {
  let result;
  try {
    result = __tbIdbRequestState.get(request).operation();
  } catch (error) {
    __tbIdbComplete(data, request, error, undefined);
    return;
  }
  __tbIdbComplete(data, request, null, result);
};

/// Writes the transaction's rows and metadata. A refused row rolls back every
/// row this commit already wrote, so another connection never sees half a
/// transaction
/// (<https://w3c.github.io/IndexedDB/#commit-a-transaction>).
const __tbIdbCommit = data => {
  if (data.finished) return;
  data.committing = true;
  data.running = true;
  const dbState = __tbIdbDatabaseState.get(data.db);
  const meta = dbState.meta;
  const applied = [];
  let failure = null;
  try {
    for (const entry of data.stores.entries()) {
      const storeName = entry[0];
      const store = entry[1];
      const prefix = __tbIdbRecordPrefixFor(dbState.name, storeName);
      for (const rowEntry of store.rows.entries()) {
        const key = rowEntry[0];
        const old = store.initial.get(key);
        if (old !== undefined && old.value === rowEntry[1].value) continue;
        __tbArray.push(applied, { __proto__: null, kind: 'record', key: prefix + key, old });
        if (!host.__tbStorageSet(__tbIdbArea, prefix + key, __tbJsonStringify(rowEntry[1]))) {
          failure = new globalThis.QuotaExceededError('The transaction could not be written.');
          break;
        }
      }
      if (failure !== null) break;
      for (const key of store.initial.keys()) {
        if (store.rows.has(key)) continue;
        __tbArray.push(applied, { __proto__: null, kind: 'record', key: prefix + key, old: store.initial.get(key) });
        if (!host.__tbStorageRemove(__tbIdbArea, prefix + key)) {
          failure = new globalThis.QuotaExceededError('The transaction could not be written.');
          break;
        }
      }
      if (failure !== null) break;
      const storeMeta = __tbArray.find(meta.stores, candidate => candidate.name === storeName);
      if (storeMeta !== undefined) storeMeta.nextKey = store.meta.nextKey;
    }
    if (failure === null) {
      for (let index = 0; index < data.removedStores.length; index++) {
        const prefix = __tbIdbRecordPrefixFor(dbState.name, data.removedStores[index]);
        const keys = __tbIdbAllKeys();
        for (let keyIndex = 0; keyIndex < keys.length; keyIndex++) {
          if (!__tbIdbStartsWith(keys[keyIndex], prefix)) continue;
          __tbArray.push(applied, { __proto__: null, kind: 'record', key: keys[keyIndex], old: undefined });
          if (!host.__tbStorageRemove(__tbIdbArea, keys[keyIndex])) {
            failure = new globalThis.QuotaExceededError('The transaction could not be written.');
            break;
          }
        }
      }
    }
    if (failure === null && (data.versionChange || data.mode === 'readwrite')) {
      __tbArray.push(applied, { __proto__: null, kind: 'meta', old: data.versionChange ? data.oldMeta : meta });
      __tbIdbWriteMeta(dbState.name, meta);
    }
  } catch (error) {
    failure = error;
  }
  if (failure !== null) {
    for (let index = applied.length - 1; index >= 0; index--) {
      const entry = applied[index];
      if (entry.kind === 'meta') {
        if (entry.old === null) __tbIdbRemoveMeta(dbState.name);
        else host.__tbStorageSet(__tbIdbArea, __tbIdbMetaPrefix + dbState.name, __tbJsonStringify(entry.old));
      } else if (entry.old === undefined) {
        host.__tbStorageRemove(__tbIdbArea, entry.key);
      } else {
        host.__tbStorageSet(__tbIdbArea, entry.key, __tbJsonStringify(entry.old));
      }
    }
    data.running = false;
    __tbIdbAbort(data, failure);
    return;
  }
  data.finished = true;
  data.running = false;
  if (data.versionChange) {
    const dbState2 = __tbIdbDatabaseState.get(data.db);
    if (dbState2 !== undefined) dbState2.transaction = null;
  }
  const completion = data.onComplete;
  data.onComplete = null;
  data.onAbort = null;
  __tbIdbTask(() => {
    __tbDispatch(data.transaction, new __tbEventConstructor('complete'));
    if (completion !== null) completion();
  });
};

const __tbIdbRequest = (data, source, operation) => {
  const request = __tbIdbConstruct(__tbIdbRequestConstructor);
  __tbIdbRequestState.set(request, {
    __proto__: null,
    done: false,
    result: undefined,
    error: null,
    source,
    transaction: data,
    operation,
  });
  if (data !== null) __tbIdbQueue(data, request);
  return request;
};

/// Checks the order every object-store operation shares: the transaction must
/// still be active, a write needs a `readwrite` transaction, and the store must
/// not have been deleted by `deleteObjectStore()`
/// (<https://w3c.github.io/IndexedDB/#add-or-put-operation>,
/// <https://w3c.github.io/IndexedDB/#delete-records-operation>).
const __tbIdbCheckStore = (store, what, write) => {
  const state = __tbIdbObjectStoreState.get(store);
  if (state === undefined) throw new TypeError('Illegal invocation');
  const data = state.transaction;
  if (data.finished || data.aborted || data.committing) {
    throw new globalThis.DOMException(
      `The ${what} request was not allowed on this transaction.`,
      'TransactionInactiveError',
    );
  }
  if (write && data.mode === 'readonly') {
    throw new globalThis.DOMException(
      `The ${what} request was not allowed on a read-only transaction.`,
      'ReadOnlyError',
    );
  }
  if (state.deleted) {
    throw new globalThis.DOMException(
      `The ${what} request was not allowed on a deleted object store.`,
      'InvalidStateError',
    );
  }
  return state;
};

const __tbIdbGenerateKey = state => {
  const key = { __proto__: null, t: 'number', v: state.meta.nextKey };
  state.meta.nextKey += 1;
  return key;
};

/// Injects a generated key into a record at `keyPath`, creating the
/// intermediate objects the path names
/// (<https://w3c.github.io/IndexedDB/#inject-key-into-value-using-a-key-path>).
/// Only a string key path reaches here: a key generator cannot be combined
/// with a sequence key path.
const __tbIdbInjectKey = (value, keyPath, key) => {
  const parts = __tbIdbSplit(keyPath, '.');
  let current = value;
  for (let index = 0; index < parts.length; index++) {
    if (current === null || typeof current !== 'object') throw __tbIdbDataError();
    const part = parts[index];
    if (index === parts.length - 1) {
      current[part] = key;
      return;
    }
    let next = current[part];
    if (next === null || typeof next !== 'object') {
      next = { __proto__: null };
      current[part] = next;
    }
    current = next;
  }
};

const __tbIdbStoreKey = (state, value, provided) => {
  const keyPath = state.meta.keyPath;
  let candidate;
  if (keyPath !== null) {
    // A store with an inline key path takes its key from the value, so an
    // explicit key argument is a caller error
    // (<https://w3c.github.io/IndexedDB/#add-or-put-operation>, step 5).
    if (provided !== undefined) throw __tbIdbDataError();
    candidate = __tbIdbEvaluate(value, keyPath);
    if (candidate === undefined) {
      if (!state.meta.autoIncrement) throw __tbIdbDataError();
      // The generated key becomes part of the stored record, so the record
      // reads back with the key at its key path.
      const generated = __tbIdbGenerateKey(state);
      __tbIdbInjectKey(value, keyPath, __tbIdbKeyValue(generated));
      return generated;
    }
  } else {
    if (provided === undefined) {
      if (state.meta.autoIncrement) return __tbIdbGenerateKey(state);
      throw __tbIdbDataError();
    }
    candidate = provided;
  }
  const key = __tbIdbToKey(candidate, __tbPrivateSet());
  if (key === undefined) throw __tbIdbDataError();
  if (state.meta.autoIncrement && key.t === 'number' && key.v >= state.meta.nextKey) {
    state.meta.nextKey = __tbIdbFloor(key.v) + 1;
  }
  return key;
};

const __tbIdbQuery = (value, optional) => {
  if (optional && (value === undefined)) {
    return { __proto__: null, lower: null, upper: null, lowerOpen: false, upperOpen: false };
  }
  const range = __tbIdbKeyRangeState.get(value);
  if (range !== undefined) return range;
  const key = __tbIdbToKey(value, __tbPrivateSet());
  if (key === undefined) throw __tbIdbDataError();
  return { __proto__: null, lower: key, upper: key, lowerOpen: false, upperOpen: false };
};

const __tbIdbSortedRows = state => {
  const rows = [];
  for (const entry of state.rows.entries()) __tbArray.push(rows, entry);
  return __tbArray.sort(rows, (first, second) => __tbIdbCompare(
    __tbIdbDecodeKey(first[0]), __tbIdbDecodeKey(second[0])));
};

const __tbIdbStoreValue = value => __tbEncode(value, undefined, null).payload;

const __tbIdbReadValue = payload => __tbDecode(payload, { __proto__: null });

const __tbIdbPut = (store, value, key, add) => {
  const state = __tbIdbCheckStore(store, add ? 'add' : 'put', true);
  // Clone before extracting the key, so a generated key lands in the stored
  // record and never mutates the caller's object
  // (<https://w3c.github.io/IndexedDB/#add-or-put-operation>, step 8).
  const clone = __tbIdbReadValue(__tbIdbStoreValue(value));
  const recordKey = __tbIdbStoreKey(state, clone, key);
  const payload = __tbIdbStoreValue(clone);
  return __tbIdbRequest(state.transaction, store, () => {
    const encoded = __tbIdbEncodeKey(recordKey);
    if (add && state.rows.has(encoded)) {
      throw new globalThis.DOMException('The key already exists in the object store.', 'ConstraintError');
    }
    state.rows.set(encoded, { __proto__: null, key: recordKey, value: payload });
    return __tbIdbKeyValue(recordKey);
  });
};

const __tbIdbGet = (store, query, wantKey, all, count) => {
  const state = __tbIdbCheckStore(store, all ? 'getAll' : 'get');
  const range = __tbIdbQuery(query, all);
  let limit = Infinity;
  if (count !== undefined) {
    const number = Number(count);
    if (!__tbNumberIsFinite(number) || number < 0 || number > 4294967295) {
      throw new TypeError("The count provided is outside the range of a valid count.");
    }
    limit = __tbIdbTrunc(number);
  }
  return __tbIdbRequest(state.transaction, store, () => {
    const matching = [];
    const rows = __tbIdbSortedRows(state);
    for (let index = 0; index < rows.length && matching.length < limit; index++) {
      const entry = rows[index];
      if (!__tbIdbRangeIncludes(range, __tbIdbDecodeKey(entry[0]))) continue;
      __tbArray.push(matching, entry[1]);
    }
    if (!all) {
      if (matching.length === 0) return undefined;
      return wantKey ? __tbIdbKeyValue(matching[0].key) : __tbIdbReadValue(matching[0].value);
    }
    const values = [];
    for (let index = 0; index < matching.length; index++) {
      __tbArray.push(values, wantKey
        ? __tbIdbKeyValue(matching[index].key)
        : __tbIdbReadValue(matching[index].value));
    }
    return values;
  });
};

const __tbIdbCount = (store, query) => {
  const state = __tbIdbCheckStore(store, 'count');
  const range = __tbIdbQuery(query, true);
  return __tbIdbRequest(state.transaction, store, () => {
    const rows = __tbIdbSortedRows(state);
    let count = 0;
    for (let index = 0; index < rows.length; index++) {
      if (__tbIdbRangeIncludes(range, __tbIdbDecodeKey(rows[index][0]))) count++;
    }
    return count;
  });
};

const __tbIdbDelete = (store, query) => {
  const state = __tbIdbCheckStore(store, 'delete', true);
  const range = __tbIdbQuery(query, false);
  return __tbIdbRequest(state.transaction, store, () => {
    const rows = __tbIdbSortedRows(state);
    for (let index = 0; index < rows.length; index++) {
      if (__tbIdbRangeIncludes(range, __tbIdbDecodeKey(rows[index][0]))) {
        state.rows.delete(rows[index][0]);
      }
    }
    return undefined;
  });
};

const __tbIdbClear = store => {
  const state = __tbIdbCheckStore(store, 'clear', true);
  return __tbIdbRequest(state.transaction, store, () => {
    state.rows.clear();
    return undefined;
  });
};

/// One `DOMStringList`, the interface `objectStoreNames`, `indexNames`, and
/// `IDBTransaction.objectStoreNames` return
/// (<https://webidl.spec.whatwg.org/#idl-DOMStringList>).
const __tbIdbStringListState = host.slots('tinybrowser.indexeddb.stringlist');
const __tbDOMStringListConstructor = globalThis.DOMStringList = class DOMStringList {
  constructor() { throw new TypeError('Illegal constructor'); }
  get length() {
    const state = __tbIdbStringListState.get(this);
    if (state === undefined) throw new TypeError('Illegal invocation');
    return state.length;
  }
  item(index) {
    const state = __tbIdbStringListState.get(this);
    if (state === undefined) throw new TypeError('Illegal invocation');
    const at = __tbIdbTrunc(Number(index));
    return at >= 0 && at < state.length ? state[at] : null;
  }
  contains(value) {
    const state = __tbIdbStringListState.get(this);
    if (state === undefined) throw new TypeError('Illegal invocation');
    return __tbArray.indexOf(state, String(value)) !== -1;
  }
  [__tbIteratorSymbol]() {
    const state = __tbIdbStringListState.get(this);
    if (state === undefined) throw new TypeError('Illegal invocation');
    return __tbArray.iterator(state);
  }
};
Object.defineProperty(
  globalThis.DOMStringList.prototype,
  Symbol.toStringTag,
  { value: 'DOMStringList', configurable: true },
);

const __tbIdbNames = names => {
  const sorted = __tbArray.sort(__tbArray.slice(names), (first, second) =>
    first < second ? -1 : first > second ? 1 : 0);
  const list = __tbIdbConstruct(__tbDOMStringListConstructor);
  __tbIdbStringListState.set(list, sorted);
  for (let index = 0; index < sorted.length; index++) list[index] = sorted[index];
  return list;
};

const __tbIdbDatabase = (name, version, meta) => {
  const db = __tbIdbConstruct(__tbIdbDatabaseConstructor);
  __tbIdbDatabaseState.set(db, {
    __proto__: null,
    name,
    version,
    meta,
    transaction: null,
    closed: false,
  });
  return db;
};

const __tbIdbRequestTransaction = (request, transaction) => {
  const state = __tbIdbRequestState.get(request);
  if (state !== undefined) state.transaction = transaction;
};

const __tbIdbOpenFailure = (request, error) => {
  const state = __tbIdbRequestState.get(request);
  state.done = true;
  state.error = error;
  state.transaction = null;
  __tbIdbTask(() => {
    __tbDispatch(request, new __tbEventConstructor('error', { cancelable: true }));
  });
};

const __tbIdbOpenSuccess = (request, db) => {
  const state = __tbIdbRequestState.get(request);
  state.done = true;
  state.result = db;
  __tbIdbTask(() => {
    __tbDispatch(request, new __tbEventConstructor('success'));
  });
};

const __tbIdbOpen = (request, name, version) => {
  try {
    const meta = __tbIdbReadMeta(name);
    if (meta === null) {
      __tbIdbUpgrade(request, name, null, version === undefined ? 1 : version);
      return;
    }
    if (version !== undefined && version < meta.version) {
      __tbIdbOpenFailure(request, new globalThis.DOMException(
        'The requested version is lower than the existing database version.',
        'VersionError',
      ));
      return;
    }
    if (version === undefined || version === meta.version) {
      __tbIdbOpenSuccess(request, __tbIdbDatabase(name, meta.version, meta));
      return;
    }
    __tbIdbUpgrade(request, name, meta, version);
  } catch (error) {
    __tbIdbOpenFailure(request, error);
  }
};

const __tbIdbUpgrade = (request, name, oldMeta, version) => {
  const meta = oldMeta === null
    ? { __proto__: null, version, stores: [] }
    : __tbJsonParse(__tbJsonStringify(oldMeta));
  meta.version = version;
  const db = __tbIdbDatabase(name, version, meta);
  const transaction = __tbIdbNewTransaction(db, [], 'versionchange');
  const data = __tbIdbTransactionState.get(transaction);
  data.oldMeta = oldMeta;
  __tbIdbDatabaseState.get(db).transaction = data;
  __tbIdbRequestTransaction(request, data);
  const requestState = __tbIdbRequestState.get(request);
  requestState.done = true;
  requestState.result = db;
  data.onComplete = () => {
    __tbIdbRequestTransaction(request, null);
    __tbIdbOpenSuccess(request, db);
  };
  data.onAbort = () => {
    __tbIdbRequestTransaction(request, null);
    __tbIdbOpenFailure(request, data.error === null
      ? new globalThis.DOMException('The version change transaction was aborted.', 'AbortError')
      : data.error);
  };
  __tbIdbTask(() => {
    // The result and versionchange transaction are visible from this event
    // (<https://w3c.github.io/IndexedDB/#upgrade-a-database>).
    __tbDispatch(request, new __tbIdbVersionChangeEventConstructor('upgradeneeded', {
      oldVersion: oldMeta === null ? 0 : oldMeta.version,
      newVersion: version,
    }));
    __tbIdbSchedule(data);
  });
};

const __tbIdbDeleteDatabase = (request, name) => {
  try {
    const meta = __tbIdbReadMeta(name);
    const oldVersion = meta === null ? 0 : meta.version;
    const prefix = __tbIdbRecordPrefix + name + '\u0000';
    const keys = __tbIdbAllKeys();
    for (let index = 0; index < keys.length; index++) {
      if (__tbIdbStartsWith(keys[index], prefix)) host.__tbStorageRemove(__tbIdbArea, keys[index]);
    }
    __tbIdbRemoveMeta(name);
    const state = __tbIdbRequestState.get(request);
    state.done = true;
    state.result = undefined;
    __tbIdbTask(() => {
      __tbDispatch(request, new __tbIdbVersionChangeEventConstructor('success', {
        oldVersion,
        newVersion: null,
      }));
    });
  } catch (error) {
    __tbIdbOpenFailure(request, error);
  }
};

// ── interfaces

// https://w3c.github.io/IndexedDB/#idbrequest
const __tbIdbRequestConstructor = globalThis.IDBRequest = class IDBRequest extends EventTarget {
  constructor() { throw new TypeError('Illegal constructor'); }
  get result() {
    const data = __tbIdbRequestState.get(this);
    if (data === undefined) throw new TypeError('Illegal invocation');
    if (!data.done) throw new globalThis.DOMException('The request has not finished.', 'InvalidStateError');
    return data.result;
  }
  get error() {
    const data = __tbIdbRequestState.get(this);
    if (data === undefined) throw new TypeError('Illegal invocation');
    if (!data.done) throw new globalThis.DOMException('The request has not finished.', 'InvalidStateError');
    return data.error;
  }
  get source() {
    const data = __tbIdbRequestState.get(this);
    if (data === undefined) throw new TypeError('Illegal invocation');
    return data.source;
  }
  get transaction() {
    const data = __tbIdbRequestState.get(this);
    if (data === undefined) throw new TypeError('Illegal invocation');
    return data.transaction === null ? null : data.transaction.transaction;
  }
  get readyState() {
    const data = __tbIdbRequestState.get(this);
    if (data === undefined) throw new TypeError('Illegal invocation');
    return data.done ? 'done' : 'pending';
  }
  get onsuccess() { return __tbIdbRequestState.get(this)?.onsuccess ?? null; }
  set onsuccess(value) {
    const data = __tbIdbRequestState.get(this);
    if (data !== undefined) data.onsuccess = __tbEventHandler(value);
  }
  get onerror() { return __tbIdbRequestState.get(this)?.onerror ?? null; }
  set onerror(value) {
    const data = __tbIdbRequestState.get(this);
    if (data !== undefined) data.onerror = __tbEventHandler(value);
  }
};
Object.defineProperty(globalThis.IDBRequest.prototype, Symbol.toStringTag, { value: 'IDBRequest', configurable: true });

// https://w3c.github.io/IndexedDB/#idbopendbrequest
globalThis.IDBOpenDBRequest = class IDBOpenDBRequest extends globalThis.IDBRequest {
  constructor() { throw new TypeError('Illegal constructor'); }
  get onblocked() { return __tbIdbRequestState.get(this)?.onblocked ?? null; }
  set onblocked(value) {
    const data = __tbIdbRequestState.get(this);
    if (data !== undefined) data.onblocked = __tbEventHandler(value);
  }
  get onupgradeneeded() { return __tbIdbRequestState.get(this)?.onupgradeneeded ?? null; }
  set onupgradeneeded(value) {
    const data = __tbIdbRequestState.get(this);
    if (data !== undefined) data.onupgradeneeded = __tbEventHandler(value);
  }
};
Object.defineProperty(globalThis.IDBOpenDBRequest.prototype, Symbol.toStringTag, { value: 'IDBOpenDBRequest', configurable: true });

// https://w3c.github.io/IndexedDB/#idbversionchangeevent
const __tbIdbVersionChangeEventConstructor = globalThis.IDBVersionChangeEvent = class IDBVersionChangeEvent extends Event {
  constructor(type, init) {
    super(type, init);
    this._oldVersion = init !== undefined && init.oldVersion !== undefined ? init.oldVersion : 0;
    this._newVersion = init !== undefined && init.newVersion !== undefined ? init.newVersion : null;
  }
  get oldVersion() { return this._oldVersion; }
  get newVersion() { return this._newVersion; }
};
Object.defineProperty(globalThis.IDBVersionChangeEvent.prototype, Symbol.toStringTag, { value: 'IDBVersionChangeEvent', configurable: true });

// https://w3c.github.io/IndexedDB/#idbkeyrange
const __tbIdbKeyRangeConstructor = globalThis.IDBKeyRange = class IDBKeyRange {
  constructor() { throw new TypeError('Illegal constructor'); }
  static only(value) {
    if (arguments.length < 1) {
      throw new TypeError(
        "Failed to execute 'only' on 'IDBKeyRange': 1 argument required, but only 0 present.",
      );
    }
    const key = __tbIdbToKey(value, __tbPrivateSet());
    if (key === undefined) throw __tbIdbDataError();
    const range = __tbIdbConstruct(__tbIdbKeyRangeConstructor);
    __tbIdbKeyRangeState.set(range, { __proto__: null, lower: key, upper: key, lowerOpen: false, upperOpen: false });
    return range;
  }
  static lowerBound(value, open) {
    if (arguments.length < 1) {
      throw new TypeError(
        "Failed to execute 'lowerBound' on 'IDBKeyRange': 1 argument required, but only 0 present.",
      );
    }
    const key = __tbIdbToKey(value, __tbPrivateSet());
    if (key === undefined) throw __tbIdbDataError();
    const range = __tbIdbConstruct(__tbIdbKeyRangeConstructor);
    __tbIdbKeyRangeState.set(range, { __proto__: null, lower: key, upper: null, lowerOpen: Boolean(open), upperOpen: true });
    return range;
  }
  static upperBound(value, open) {
    if (arguments.length < 1) {
      throw new TypeError(
        "Failed to execute 'upperBound' on 'IDBKeyRange': 1 argument required, but only 0 present.",
      );
    }
    const key = __tbIdbToKey(value, __tbPrivateSet());
    if (key === undefined) throw __tbIdbDataError();
    const range = __tbIdbConstruct(__tbIdbKeyRangeConstructor);
    __tbIdbKeyRangeState.set(range, { __proto__: null, lower: null, upper: key, lowerOpen: true, upperOpen: Boolean(open) });
    return range;
  }
  static bound(lower, upper, lowerOpen, upperOpen) {
    if (arguments.length < 2) {
      throw new TypeError(
        `Failed to execute 'bound' on 'IDBKeyRange': 2 arguments required, but only ${arguments.length} present.`,
      );
    }
    const first = __tbIdbToKey(lower, __tbPrivateSet());
    const second = __tbIdbToKey(upper, __tbPrivateSet());
    if (first === undefined || second === undefined) throw __tbIdbDataError();
    if (__tbIdbCompare(first, second) > 0) throw __tbIdbDataError();
    const range = __tbIdbConstruct(__tbIdbKeyRangeConstructor);
    __tbIdbKeyRangeState.set(range, {
      __proto__: null,
      lower: first,
      upper: second,
      lowerOpen: Boolean(lowerOpen),
      upperOpen: Boolean(upperOpen),
    });
    return range;
  }
  get lower() {
    const range = __tbIdbKeyRangeState.get(this);
    if (range === undefined) throw new TypeError('Illegal invocation');
    return range.lower === null ? undefined : __tbIdbKeyValue(range.lower);
  }
  get upper() {
    const range = __tbIdbKeyRangeState.get(this);
    if (range === undefined) throw new TypeError('Illegal invocation');
    return range.upper === null ? undefined : __tbIdbKeyValue(range.upper);
  }
  get lowerOpen() {
    const range = __tbIdbKeyRangeState.get(this);
    if (range === undefined) throw new TypeError('Illegal invocation');
    return range.lowerOpen;
  }
  get upperOpen() {
    const range = __tbIdbKeyRangeState.get(this);
    if (range === undefined) throw new TypeError('Illegal invocation');
    return range.upperOpen;
  }
  includes(key) {
    if (arguments.length < 1) {
      throw new TypeError(
        "Failed to execute 'includes' on 'IDBKeyRange': 1 argument required, but only 0 present.",
      );
    }
    const range = __tbIdbKeyRangeState.get(this);
    if (range === undefined) throw new TypeError('Illegal invocation');
    const candidate = __tbIdbToKey(key, __tbPrivateSet());
    if (candidate === undefined) throw __tbIdbDataError();
    return __tbIdbRangeIncludes(range, candidate);
  }
};
Object.defineProperty(globalThis.IDBKeyRange.prototype, Symbol.toStringTag, { value: 'IDBKeyRange', configurable: true });

// https://w3c.github.io/IndexedDB/#idbobjectstore
const __tbIdbObjectStoreConstructor = globalThis.IDBObjectStore = class IDBObjectStore {
  constructor() { throw new TypeError('Illegal constructor'); }
  get name() {
    const state = __tbIdbObjectStoreState.get(this);
    if (state === undefined) throw new TypeError('Illegal invocation');
    return state.meta.name;
  }
  get keyPath() {
    const state = __tbIdbObjectStoreState.get(this);
    if (state === undefined) throw new TypeError('Illegal invocation');
    return state.meta.keyPath;
  }
  get autoIncrement() {
    const state = __tbIdbObjectStoreState.get(this);
    if (state === undefined) throw new TypeError('Illegal invocation');
    return state.meta.autoIncrement;
  }
  get transaction() {
    const state = __tbIdbObjectStoreState.get(this);
    if (state === undefined) throw new TypeError('Illegal invocation');
    return state.transaction.transaction;
  }
  get indexNames() {
    const state = __tbIdbObjectStoreState.get(this);
    if (state === undefined) throw new TypeError('Illegal invocation');
    return __tbIdbNames([]);
  }
  add(value, key) {
    if (arguments.length < 1) throw new TypeError("Failed to execute 'add' on 'IDBObjectStore': 1 argument required, but only 0 present.");
    return __tbIdbPut(this, value, key, true);
  }
  put(value, key) {
    if (arguments.length < 1) throw new TypeError("Failed to execute 'put' on 'IDBObjectStore': 1 argument required, but only 0 present.");
    return __tbIdbPut(this, value, key, false);
  }
  get(query) {
    if (arguments.length < 1) throw new TypeError("Failed to execute 'get' on 'IDBObjectStore': 1 argument required, but only 0 present.");
    return __tbIdbGet(this, query, false, false);
  }
  getKey(query) {
    if (arguments.length < 1) throw new TypeError("Failed to execute 'getKey' on 'IDBObjectStore': 1 argument required, but only 0 present.");
    return __tbIdbGet(this, query, true, false);
  }
  getAll(query, count) { return __tbIdbGet(this, query, false, true, count); }
  getAllKeys(query, count) { return __tbIdbGet(this, query, true, true, count); }
  count(query) { return __tbIdbCount(this, query); }
  delete(query) {
    if (arguments.length < 1) throw new TypeError("Failed to execute 'delete' on 'IDBObjectStore': 1 argument required, but only 0 present.");
    return __tbIdbDelete(this, query);
  }
  clear() { return __tbIdbClear(this); }
};
Object.defineProperty(globalThis.IDBObjectStore.prototype, Symbol.toStringTag, { value: 'IDBObjectStore', configurable: true });

// https://w3c.github.io/IndexedDB/#idbtransaction
const __tbIdbTransactionConstructor = globalThis.IDBTransaction = class IDBTransaction extends EventTarget {
  constructor() { throw new TypeError('Illegal constructor'); }
  get db() {
    const data = __tbIdbTransactionState.get(this);
    if (data === undefined) throw new TypeError('Illegal invocation');
    return data.db;
  }
  get mode() {
    const data = __tbIdbTransactionState.get(this);
    if (data === undefined) throw new TypeError('Illegal invocation');
    return data.mode;
  }
  get durability() {
    const data = __tbIdbTransactionState.get(this);
    if (data === undefined) throw new TypeError('Illegal invocation');
    return 'default';
  }
  get objectStoreNames() {
    const data = __tbIdbTransactionState.get(this);
    if (data === undefined) throw new TypeError('Illegal invocation');
    return __tbIdbNames(data.scope);
  }
  objectStore(name) {
    const data = __tbIdbTransactionState.get(this);
    if (data === undefined) throw new TypeError('Illegal invocation');
    const storeName = String(name);
    const state = data.stores.get(storeName);
    if (state === undefined) {
      throw new globalThis.DOMException(`The object store '${storeName}' is not in this transaction's scope.`, 'NotFoundError');
    }
    let store = data.handles.get(storeName);
    if (store === undefined || store === null) {
      store = __tbIdbConstruct(__tbIdbObjectStoreConstructor);
      __tbIdbObjectStoreState.set(store, state);
      data.handles.set(storeName, store);
    }
    return store;
  }
  abort() {
    const data = __tbIdbTransactionState.get(this);
    if (data === undefined) throw new TypeError('Illegal invocation');
    if (data.finished || data.committing) {
      throw new globalThis.DOMException(
        'The transaction is committing or has finished.',
        'InvalidStateError',
      );
    }
    __tbIdbAbort(data, null);
  }
  commit() {
    const data = __tbIdbTransactionState.get(this);
    if (data === undefined) throw new TypeError('Illegal invocation');
    if (data.finished || data.committing) {
      throw new globalThis.DOMException(
        'The transaction is committing or has finished.',
        'InvalidStateError',
      );
    }
    data.committing = true;
    if (!data.running && data.queue.length === 0) __tbIdbCommit(data);
    else __tbIdbSchedule(data);
  }
  get oncomplete() { return __tbIdbTransactionState.get(this)?.oncomplete ?? null; }
  set oncomplete(value) {
    const data = __tbIdbTransactionState.get(this);
    if (data !== undefined) data.oncomplete = __tbEventHandler(value);
  }
  get onabort() { return __tbIdbTransactionState.get(this)?.onabort ?? null; }
  set onabort(value) {
    const data = __tbIdbTransactionState.get(this);
    if (data !== undefined) data.onabort = __tbEventHandler(value);
  }
  get onerror() { return __tbIdbTransactionState.get(this)?.onerror ?? null; }
  set onerror(value) {
    const data = __tbIdbTransactionState.get(this);
    if (data !== undefined) data.onerror = __tbEventHandler(value);
  }
};
Object.defineProperty(globalThis.IDBTransaction.prototype, Symbol.toStringTag, { value: 'IDBTransaction', configurable: true });

// https://w3c.github.io/IndexedDB/#idbdatabase
const __tbIdbDatabaseConstructor = globalThis.IDBDatabase = class IDBDatabase extends EventTarget {
  constructor() { throw new TypeError('Illegal constructor'); }
  get name() {
    const data = __tbIdbDatabaseState.get(this);
    if (data === undefined) throw new TypeError('Illegal invocation');
    return data.name;
  }
  get version() {
    const data = __tbIdbDatabaseState.get(this);
    if (data === undefined) throw new TypeError('Illegal invocation');
    return data.version;
  }
  get objectStoreNames() {
    const data = __tbIdbDatabaseState.get(this);
    if (data === undefined) throw new TypeError('Illegal invocation');
    return __tbIdbNames(__tbArray.map(data.meta.stores, store => store.name));
  }
  createObjectStore(name, options) {
    const data = __tbIdbDatabaseState.get(this);
    if (data === undefined) throw new TypeError('Illegal invocation');
    const transaction = data.transaction;
    if (transaction === null || transaction.finished || transaction.aborted) throw __tbIdbStateError();
    const storeName = String(name);
    const keyPath = __tbIdbConvertKeyPath(
      options !== undefined && options.keyPath !== undefined ? options.keyPath : undefined,
    );
    const autoIncrement = options !== undefined && options.autoIncrement !== undefined
      ? Boolean(options.autoIncrement)
      : false;
    if (__tbArray.find(data.meta.stores, store => store.name === storeName) !== undefined) {
      throw new globalThis.DOMException(`An object store named '${storeName}' already exists.`, 'ConstraintError');
    }
    if (keyPath !== null && !__tbIdbValidKeyPath(keyPath)) {
      throw new globalThis.DOMException(`The key path '${keyPath}' is not a valid key path.`, 'SyntaxError');
    }
    // An absent key path means out-of-line keys, which a key generator
    // supports. Only an empty key path and a sequence key path are invalid
    // (<https://w3c.github.io/IndexedDB/#dom-idbdatabase-createobjectstore>,
    // step 5).
    if (autoIncrement && (keyPath === '' || __tbIsArray(keyPath))) {
      throw new globalThis.DOMException(
        'A key generator cannot be used with an empty key path or a sequence key path.',
        'InvalidAccessError',
      );
    }
    const storeMeta = {
      __proto__: null,
      name: storeName,
      keyPath,
      autoIncrement,
      nextKey: 1,
      indexes: [],
    };
    __tbArray.push(data.meta.stores, storeMeta);
    const state = __tbIdbNewStoreState(transaction, storeMeta, new __tbPrivateMap());
    transaction.stores.set(storeName, state);
    __tbArray.push(transaction.scope, storeName);
    const store = __tbIdbConstruct(__tbIdbObjectStoreConstructor);
    __tbIdbObjectStoreState.set(store, state);
    transaction.handles.set(storeName, store);
    return store;
  }
  deleteObjectStore(name) {
    const data = __tbIdbDatabaseState.get(this);
    if (data === undefined) throw new TypeError('Illegal invocation');
    const transaction = data.transaction;
    if (transaction === null || transaction.finished || transaction.aborted) throw __tbIdbStateError();
    const storeName = String(name);
    const index = __tbArray.findIndex(data.meta.stores, store => store.name === storeName);
    if (index === -1) throw new globalThis.DOMException(`The object store '${storeName}' does not exist.`, 'NotFoundError');
    __tbArray.splice(data.meta.stores, index, 1);
    const removed = transaction.stores.get(storeName);
    if (removed !== undefined) removed.deleted = true;
    transaction.stores.delete(storeName);
    __tbArray.push(transaction.removedStores, storeName);
    transaction.scope = __tbArray.filter(transaction.scope, entry => entry !== storeName);
  }
  transaction(names, mode) {
    const data = __tbIdbDatabaseState.get(this);
    if (data === undefined) throw new TypeError('Illegal invocation');
    if (data.closed) throw new globalThis.DOMException('The database connection is closing.', 'InvalidStateError');
    if (data.transaction !== null) throw __tbIdbStateError();
    if (names === undefined) {
      throw new TypeError("Failed to execute 'transaction' on 'IDBDatabase': 1 argument required, but only 0 present.");
    }
    const list = typeof names === 'string' ? [names] : names;
    const copies = [];
    for (let index = 0; index < list.length; index++) __tbArray.push(copies, String(list[index]));
    if (copies.length === 0) throw new globalThis.DOMException('The scope cannot be empty.', 'InvalidAccessError');
    const requestedMode = mode === undefined ? 'readonly' : String(mode);
    if (requestedMode !== 'readonly' && requestedMode !== 'readwrite') {
      throw new TypeError(`The provided value '${requestedMode}' is not a valid enum value of type IDBTransactionMode.`);
    }
    return __tbIdbNewTransaction(this, copies, requestedMode);
  }
  close() {
    const data = __tbIdbDatabaseState.get(this);
    if (data === undefined) throw new TypeError('Illegal invocation');
    data.closed = true;
    data.transaction = null;
  }
  get onabort() { return __tbIdbDatabaseState.get(this)?.onabort ?? null; }
  set onabort(value) {
    const data = __tbIdbDatabaseState.get(this);
    if (data !== undefined) data.onabort = __tbEventHandler(value);
  }
  get onclose() { return __tbIdbDatabaseState.get(this)?.onclose ?? null; }
  set onclose(value) {
    const data = __tbIdbDatabaseState.get(this);
    if (data !== undefined) data.onclose = __tbEventHandler(value);
  }
  get onerror() { return __tbIdbDatabaseState.get(this)?.onerror ?? null; }
  set onerror(value) {
    const data = __tbIdbDatabaseState.get(this);
    if (data !== undefined) data.onerror = __tbEventHandler(value);
  }
  get onversionchange() { return __tbIdbDatabaseState.get(this)?.onversionchange ?? null; }
  set onversionchange(value) {
    const data = __tbIdbDatabaseState.get(this);
    if (data !== undefined) data.onversionchange = __tbEventHandler(value);
  }
};
Object.defineProperty(globalThis.IDBDatabase.prototype, Symbol.toStringTag, { value: 'IDBDatabase', configurable: true });

// https://w3c.github.io/IndexedDB/#idbfactory
const __tbIdbFactoryConstructor = globalThis.IDBFactory = class IDBFactory {
  constructor() { throw new TypeError('Illegal constructor'); }
  open(name, version) {
    if (__tbIdbFactoryState.get(this) === undefined) throw new TypeError('Illegal invocation');
    if (__tbIdbOpaque()) throw __tbIdbSecurityError();
    const requested = version === undefined ? undefined : __tbIdbVersion(version);
    const request = __tbIdbConstruct(globalThis.IDBOpenDBRequest);
    __tbIdbRequestState.set(request, {
      __proto__: null,
      done: false,
      result: undefined,
      error: null,
      source: null,
      transaction: null,
    });
    __tbIdbTask(() => __tbIdbOpen(request, String(name), requested));
    return request;
  }
  deleteDatabase(name) {
    if (__tbIdbFactoryState.get(this) === undefined) throw new TypeError('Illegal invocation');
    if (__tbIdbOpaque()) throw __tbIdbSecurityError();
    const request = __tbIdbConstruct(globalThis.IDBOpenDBRequest);
    __tbIdbRequestState.set(request, {
      __proto__: null,
      done: false,
      result: undefined,
      error: null,
      source: null,
      transaction: null,
    });
    __tbIdbTask(() => __tbIdbDeleteDatabase(request, String(name)));
    return request;
  }
  databases() {
    if (__tbIdbFactoryState.get(this) === undefined) throw new TypeError('Illegal invocation');
    if (__tbIdbOpaque()) {
      return Promise.reject(__tbIdbSecurityError());
    }
    // The method returns a snapshot of the databases and their versions as
    // they are when it is called. A versionchange transaction running now has
    // not committed, so its database and version are invisible
    // (<https://w3c.github.io/IndexedDB/#dom-idbfactory-databases>, steps 3
    // and 5).
    const names = [];
    const keys = __tbIdbAllKeys();
    for (let index = 0; index < keys.length; index++) {
      const key = keys[index];
      if (!__tbIdbStartsWith(key, __tbIdbMetaPrefix)) continue;
      const text = host.__tbStorageGet(__tbIdbArea, key);
      if (text === null || text === undefined) continue;
      const meta = __tbJsonParse(text);
      __tbArray.push(names, {
        __proto__: null,
        name: __tbIdbSlice(key, __tbIdbMetaPrefix.length),
        version: meta.version,
      });
    }
    __tbArray.sort(names, (first, second) =>
      first.name < second.name ? -1 : first.name > second.name ? 1 : 0);
    return new Promise(resolve => {
      __tbIdbTask(() => resolve(names));
    });
  }
  cmp(first, second) {
    if (__tbIdbFactoryState.get(this) === undefined) throw new TypeError('Illegal invocation');
    // Both keys are required members of the overload, so a short argument
    // list is a TypeError before either key is converted
    // (<https://webidl.spec.whatwg.org/#dfn-argument-count>).
    if (arguments.length < 2) {
      throw new TypeError(
        `Failed to execute 'cmp' on 'IDBFactory': 2 arguments required, but only ${arguments.length} present.`,
      );
    }
    const left = __tbIdbToKey(first, __tbPrivateSet());
    if (left === undefined) throw __tbIdbDataError();
    const right = __tbIdbToKey(second, __tbPrivateSet());
    if (right === undefined) throw __tbIdbDataError();
    return __tbIdbCompare(left, right);
  }
};
Object.defineProperty(globalThis.IDBFactory.prototype, Symbol.toStringTag, { value: 'IDBFactory', configurable: true });

/// [EnforceRange] unsigned long long conversion
/// (<https://webidl.spec.whatwg.org/#es-unsigned-long-long>).
const __tbIdbVersion = value => {
  const number = Number(value);
  if (!__tbNumberIsFinite(number) || number < 0 || number > __tbIdbMaxSafeInteger) {
    throw new TypeError('The version provided is outside the range of a valid version.');
  }
  const version = __tbIdbTrunc(number);
  if (version === 0) throw new TypeError('The version provided must not be 0.');
  return version;
};

const __tbIdbFactory = __tbIdbConstruct(__tbIdbFactoryConstructor);
__tbIdbFactoryState.set(__tbIdbFactory, true);

Object.defineProperty(globalThis, 'indexedDB', {
  get() { return __tbIdbFactory; },
  enumerable: true,
  configurable: true,
});
