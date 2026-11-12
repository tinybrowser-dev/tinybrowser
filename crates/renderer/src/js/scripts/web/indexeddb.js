// https://w3c.github.io/IndexedDB/
//
// The factory, database open/upgrade, object stores and their records,
// indexes, and cursors. The host seam stores opaque encoded rows in the
// profile's IndexedDB area, so the browser owns the data and every renderer of
// a profile sees one store, like `localStorage`.
//
// Not implemented yet: upgrade blocking between open connections, transaction
// scheduling across overlapping connections, and a key generator shared across
// connections. A missing member is absent, so a page gets a TypeError instead
// of a result.
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
const __tbIdbIndexState = host.slots('tinybrowser.indexeddb.index');
const __tbIdbCursorState = host.slots('tinybrowser.indexeddb.cursor');
const __tbIdbRecordState = host.slots('tinybrowser.indexeddb.record');
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

/// Deactivates a transaction at the end of the task that dispatched an event
/// for it. The task is queued before the event, so it runs after the event's
/// microtasks but before any timer the listeners queued
/// (<https://w3c.github.io/IndexedDB/#cleanup-indexed-database-transactions>).
const __tbIdbCleanup = data => {
  __tbIdbTask(() => {
    if (data.finished || data.explicitCommit) return;
    data.committing = true;
    __tbIdbSchedule(data);
  });
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
    if (typeof current === 'string') {
      if (parts[index] !== 'length') return undefined;
      current = current.length;
      continue;
    }
    // A File and a Blob expose these identifiers outside their own
    // properties (<https://w3c.github.io/IndexedDB/#evaluate-a-key-path>).
    if (globalThis.File !== undefined && current instanceof globalThis.File
      && (parts[index] === 'name' || parts[index] === 'lastModified')) {
      current = current[parts[index]];
      continue;
    }
    if (globalThis.Blob !== undefined && current instanceof globalThis.Blob
      && (parts[index] === 'size' || parts[index] === 'type')) {
      current = current[parts[index]];
      continue;
    }
    if (typeof current !== 'object' && typeof current !== 'function') return undefined;
    if (!__tbApply(__tbHasOwnProperty, current, [parts[index]])) return undefined;
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
    } else if (globalThis.SharedArrayBuffer !== undefined
      && value instanceof globalThis.SharedArrayBuffer) {
      // A shared buffer is a valid binary key once the page is allowed to
      // create one; WPT's sharedarraybuffer-key.https.html requires it even
      // though the older key-construct text called it an invalid type
      // (<https://w3c.github.io/IndexedDB/#convert-value-to-key>).
      bytes = new __tbUint8Array(value);
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

const __tbIdbInfinity = '\u0000infinity';
/// JSON for IndexedDB data. Plain JSON turns Infinity and -Infinity into
/// null, but both are valid keys, so the encoder tags them and the decoder
/// restores them.
const __tbIdbJson = value => __tbJsonStringify(value, (name, item) => {
  if (typeof item === 'number' && !__tbNumberIsFinite(item)) {
    return { __proto__: null, [__tbIdbInfinity]: item > 0 ? 1 : -1 };
  }
  return item;
});
const __tbIdbUnjson = text => __tbJsonParse(text, (name, item) => {
  if (item !== null && typeof item === 'object'
    && item[__tbIdbInfinity] !== undefined) {
    return item[__tbIdbInfinity] > 0 ? Infinity : -Infinity;
  }
  return item;
});
const __tbIdbEncodeKey = key => __tbIdbJson(key);
const __tbIdbDecodeKey = text => __tbIdbUnjson(text);

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

/// Converts a value to a multiEntry key: an array keeps the valid, distinct
/// members as subkeys and drops the rest; any other value becomes an ordinary
/// key (<https://w3c.github.io/IndexedDB/#convert-value-to-a-multientry-key>).
const __tbIdbToMultiEntryKey = value => {
  if (!__tbIsArray(value)) return __tbIdbToKey(value, __tbPrivateSet());
  const items = [];
  const seen = __tbPrivateSet();
  seen.add(value);
  for (let index = 0; index < value.length; index++) {
    let key;
    try {
      key = __tbIdbToKey(value[index], seen);
    } catch (error) {
      key = undefined;
    }
    if (key === undefined) continue;
    let duplicate = false;
    for (let itemIndex = 0; itemIndex < items.length; itemIndex++) {
      if (__tbIdbCompare(items[itemIndex], key) !== 0) continue;
      duplicate = true;
      break;
    }
    if (!duplicate) __tbArray.push(items, key);
  }
  return { __proto__: null, t: 'array', v: items };
};

/// The key an index extracts from a stored value, or `undefined` when the
/// value has no valid key at the index's key path
/// (<https://w3c.github.io/IndexedDB/#extract-a-key-from-a-value-using-a-key-path>).
const __tbIdbExtractKey = (value, keyPath, multiEntry) => {
  const candidate = __tbIdbEvaluate(value, keyPath);
  if (candidate === undefined) return undefined;
  try {
    return multiEntry
      ? __tbIdbToMultiEntryKey(candidate)
      : __tbIdbToKey(candidate, __tbPrivateSet());
  } catch (error) {
    return undefined;
  }
};

/// The stored records sorted by key, built once per mutation
/// (<https://w3c.github.io/IndexedDB/#object-store-construct>).
const __tbIdbRowList = state => {
  if (state.rowList !== null) return state.rowList;
  const rows = [];
  for (const entry of state.rows.entries()) __tbArray.push(rows, entry);
  __tbArray.sort(rows, (first, second) => __tbIdbCompare(
    __tbIdbDecodeKey(first[0]), __tbIdbDecodeKey(second[0])));
  state.rowList = rows;
  return rows;
};

/// The records of one index, sorted by index key and then by the primary key
/// of the referenced record
/// (<https://w3c.github.io/IndexedDB/#index-construct>).
const __tbIdbIndexEntries = (state, indexMeta) => {
  const cached = state.indexEntries.get(indexMeta);
  if (cached !== undefined) return cached;
  const entries = [];
  const rows = __tbIdbRowList(state);
  for (let index = 0; index < rows.length; index++) {
    const row = rows[index][1];
    const value = __tbIdbReadValue(row.value);
    const indexKey = __tbIdbExtractKey(value, indexMeta.keyPath, indexMeta.multiEntry);
    if (indexKey === undefined) continue;
    if (indexMeta.multiEntry && indexKey.t === 'array') {
      for (let subIndex = 0; subIndex < indexKey.v.length; subIndex++) {
        __tbArray.push(entries, {
          __proto__: null,
          key: indexKey.v[subIndex],
          primaryKey: row.key,
          row,
        });
      }
      continue;
    }
    __tbArray.push(entries, { __proto__: null, key: indexKey, primaryKey: row.key, row });
  }
  __tbArray.sort(entries, (first, second) => {
    const order = __tbIdbCompare(first.key, second.key);
    return order !== 0 ? order : __tbIdbCompare(first.primaryKey, second.primaryKey);
  });
  state.indexEntries.set(indexMeta, entries);
  return entries;
};

/// Throws when a record with `primaryKey` cannot be written because one of the
/// store's unique indexes already maps that index key to another record
/// (<https://w3c.github.io/IndexedDB/#store-a-record-into-an-object-store>,
/// step 9).
const __tbIdbCheckUnique = (state, value, primaryKey) => {
  const indexes = state.meta.indexes;
  for (let index = 0; index < indexes.length; index++) {
    const indexMeta = indexes[index];
    if (!indexMeta.unique || indexMeta.pending) continue;
    const indexKey = __tbIdbExtractKey(value, indexMeta.keyPath, indexMeta.multiEntry);
    if (indexKey === undefined) continue;
    const candidates = indexMeta.multiEntry && indexKey.t === 'array'
      ? indexKey.v
      : [indexKey];
    const entries = __tbIdbIndexEntries(state, indexMeta);
    for (let candidateIndex = 0; candidateIndex < candidates.length; candidateIndex++) {
      const candidate = candidates[candidateIndex];
      for (let entryIndex = 0; entryIndex < entries.length; entryIndex++) {
        const entry = entries[entryIndex];
        if (__tbIdbCompare(entry.primaryKey, primaryKey) === 0) continue;
        if (__tbIdbCompare(entry.key, candidate) !== 0) continue;
        throw new globalThis.DOMException(
          'The index already contains a record with that key.',
          'ConstraintError',
        );
      }
    }
  }
};


const __tbIdbReadMeta = name => {
  const text = host.__tbStorageGet(__tbIdbArea, __tbIdbMetaPrefix + name);
  if (text === null || text === undefined) return null;
  return __tbIdbUnjson(text);
};

const __tbIdbWriteMeta = (name, meta) => {
  if (!host.__tbStorageSet(__tbIdbArea, __tbIdbMetaPrefix + name, __tbIdbJson(meta))) {
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
    const row = __tbIdbUnjson(text);
    rows.set(__tbIdbEncodeKey(row.key), row);
  }
};

const __tbIdbNewStoreState = (transaction, meta, initial) => {
  const rows = new __tbPrivateMap();
  for (const entry of initial.entries()) rows.set(entry[0], entry[1]);
  return {
    __proto__: null,
    transaction,
    meta,
    rows,
    initial,
    deleted: false,
    renamed: false,
    created: false,
    rowList: null,
    indexEntries: __tbPrivateMap(),
    indexHandles: __tbPrivateMap(),
  };
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
    explicitCommit: false,
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
    const state = __tbIdbNewStoreState(data, __tbIdbUnjson(__tbIdbJson(storeMeta)), initial);
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
        data, __tbIdbUnjson(__tbIdbJson(storeMeta)), initial));
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
  // A request's event dispatch runs with the transaction active, even if the
  // previous task's cleanup already marked it inactive. An explicit commit()
  // keeps it inactive for every later call
  // (<https://w3c.github.io/IndexedDB/#fire-a-success-event>).
  data.committing = data.explicitCommit;
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
    if (dbState !== undefined) {
      // The connection reverts to the version and object store set it had
      // before the upgrade, and handles created during the transaction are
      // marked deleted (<https://w3c.github.io/IndexedDB/#abort-a-transaction>).
      dbState.meta = data.oldMeta === null
        ? { __proto__: null, version: 0, stores: [] }
        : __tbIdbUnjson(__tbIdbJson(data.oldMeta));
      dbState.version = data.oldMeta === null ? 0 : data.oldMeta.version;
      dbState.transaction = null;
    }
    for (const entry of data.stores.entries()) {
      const storeState = entry[1];
      if (storeState.created) {
        storeState.deleted = true;
        continue;
      }
      if (storeState.originalName !== undefined) storeState.meta.name = storeState.originalName;
      const survivingIndexes = [];
      for (let index = 0; index < storeState.meta.indexes.length; index++) {
        const indexMeta = storeState.meta.indexes[index];
        if (indexMeta.created === true) continue;
        __tbArray.push(survivingIndexes, indexMeta);
      }
      storeState.meta.indexes = survivingIndexes;
      for (const indexEntry of storeState.indexHandles.entries()) {
        const indexState = __tbIdbIndexState.get(indexEntry[1]);
        if (indexState === undefined) continue;
        if (indexState.meta.created === true) {
          indexState.deleted = true;
        } else if (indexState.originalName !== undefined) {
          indexState.meta.name = indexState.originalName;
        }
      }
    }
    const scope = [];
    const storeCount = dbState === undefined ? 0 : dbState.meta.stores.length;
    for (let index = 0; index < storeCount; index++) {
      __tbArray.push(scope, dbState.meta.stores[index].name);
    }
    data.scope = scope;
  }
  __tbIdbTask(() => {
    __tbDispatch(data.transaction, new __tbEventConstructor('abort'));
    if (data.onAbort !== null) data.onAbort();
  });
};

const __tbIdbComplete = (data, request, error, result) => {
  const state = __tbIdbRequestState.get(request);
  state.done = true;
  __tbIdbCleanup(data);
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
  // With no request left, the cleanup task commits the transaction, not this
  // one: listeners and their promise continuations still run here and the
  // transaction stays active for them
  // (<https://w3c.github.io/IndexedDB/#fire-a-success-event>).
  data.running = false;
  if (!data.finished && data.committing) __tbIdbSchedule(data);
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
    // Remove the rows of deleted and renamed-away stores before writing, so a
    // store that takes over a freed name writes into an empty prefix and a
    // swap of two names never deletes rows written earlier in this commit
    // (<https://w3c.github.io/IndexedDB/#commit-a-transaction>).
    for (let index = 0; index < data.removedStores.length; index++) {
      const prefix = __tbIdbRecordPrefixFor(dbState.name, data.removedStores[index]);
      const keys = __tbIdbAllKeys();
      for (let keyIndex = 0; keyIndex < keys.length; keyIndex++) {
        if (!__tbIdbStartsWith(keys[keyIndex], prefix)) continue;
        const old = host.__tbStorageGet(__tbIdbArea, keys[keyIndex]);
        __tbArray.push(applied, { __proto__: null, kind: 'raw', key: keys[keyIndex], old });
        if (!host.__tbStorageRemove(__tbIdbArea, keys[keyIndex])) {
          failure = new globalThis.QuotaExceededError('The transaction could not be written.');
          break;
        }
      }
      if (failure !== null) break;
    }
    if (failure === null) {
      for (const entry of data.stores.entries()) {
        const storeName = entry[0];
        const store = entry[1];
        const prefix = __tbIdbRecordPrefixFor(dbState.name, storeName);
        for (const rowEntry of store.rows.entries()) {
          const key = rowEntry[0];
          const old = store.renamed ? undefined : store.initial.get(key);
          if (old !== undefined && old.value === rowEntry[1].value) continue;
          __tbArray.push(applied, { __proto__: null, kind: 'record', key: prefix + key, old });
          if (!host.__tbStorageSet(__tbIdbArea, prefix + key, __tbIdbJson(rowEntry[1]))) {
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
        if (storeMeta !== undefined) {
          storeMeta.nextKey = store.meta.nextKey;
          const indexes = [];
          for (let index = 0; index < store.meta.indexes.length; index++) {
            const indexMeta = store.meta.indexes[index];
            __tbArray.push(indexes, {
              __proto__: null,
              name: indexMeta.name,
              keyPath: indexMeta.keyPath,
              unique: indexMeta.unique,
              multiEntry: indexMeta.multiEntry,
            });
          }
          storeMeta.indexes = indexes;
          storeMeta.keyPath = store.meta.keyPath;
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
        else host.__tbStorageSet(__tbIdbArea, __tbIdbMetaPrefix + dbState.name, __tbIdbJson(entry.old));
      } else if (entry.kind === 'raw') {
        if (entry.old === null || entry.old === undefined) host.__tbStorageRemove(__tbIdbArea, entry.key);
        else host.__tbStorageSet(__tbIdbArea, entry.key, entry.old);
      } else if (entry.old === undefined) {
        host.__tbStorageRemove(__tbIdbArea, entry.key);
      } else {
        host.__tbStorageSet(__tbIdbArea, entry.key, __tbIdbJson(entry.old));
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

/// Checks the order every object-store operation shares: the store must not
/// have been deleted, the transaction must still be active, and a write needs a
/// `readwrite` transaction
/// (<https://w3c.github.io/IndexedDB/#add-or-put-operation>,
/// <https://w3c.github.io/IndexedDB/#delete-records-operation>).
const __tbIdbCheckStore = (store, what, write) => {
  const state = __tbIdbObjectStoreState.get(store);
  if (state === undefined) throw new TypeError('Illegal invocation');
  const data = state.transaction;
  if (state.deleted) {
    throw new globalThis.DOMException(
      `The ${what} request was not allowed on a deleted object store.`,
      'InvalidStateError',
    );
  }
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
  return state;
};

/// Checks the same order for an index: a deleted index or a deleted referenced
/// object store precedes an inactive transaction
/// (<https://w3c.github.io/IndexedDB/#dom-idbindex-get>).
const __tbIdbCheckIndex = (index, what) => {
  const state = __tbIdbIndexState.get(index);
  if (state === undefined) throw new TypeError('Illegal invocation');
  const data = state.transaction;
  if (state.deleted || state.store.deleted) {
    throw new globalThis.DOMException(
      `The ${what} request was not allowed on a deleted index.`,
      'InvalidStateError',
    );
  }
  if (data.finished || data.aborted || data.committing) {
    throw new globalThis.DOMException(
      `The ${what} request was not allowed on this transaction.`,
      'TransactionInactiveError',
    );
  }
  return state;
};

const __tbIdbGeneratorLimit = 9007199254740992;

/// The next generated key, or `null` when the generator is exhausted
/// (<https://w3c.github.io/IndexedDB/#generate-a-key>).
const __tbIdbGenerateKey = state => {
  const value = state.meta.nextKey;
  if (value > __tbIdbGeneratorLimit) return null;
  state.meta.nextKey = value + 1;
  return { __proto__: null, t: 'number', v: value };
};

/// Advances the generator past an explicit numeric key, capped at 2^53
/// (<https://w3c.github.io/IndexedDB/#possibly-update-the-key-generator>).
const __tbIdbPossiblyUpdateKey = (state, key) => {
  if (!state.meta.autoIncrement || key.t !== 'number') return;
  let value = key.v;
  if (value > __tbIdbGeneratorLimit) value = __tbIdbGeneratorLimit;
  value = __tbIdbFloor(value);
  if (value >= state.meta.nextKey) state.meta.nextKey = value + 1;
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

/// Whether a generated key can be injected at `keyPath` into `value` without
/// hitting a non-object intermediate value
/// (<https://w3c.github.io/IndexedDB/#check-that-a-key-could-be-injected-into-a-value>).
const __tbIdbCanInject = (value, keyPath) => {
  const parts = __tbIdbSplit(keyPath, '.');
  let current = value;
  for (let index = 0; index + 1 < parts.length; index++) {
    if (current === null || typeof current !== 'object') return false;
    if (!__tbApply(__tbHasOwnProperty, current, [parts[index]])) return true;
    current = current[parts[index]];
  }
  return current !== null && typeof current === 'object';
};

/// The explicit key a new record gets, or `undefined` when the request will
/// generate one. The key is converted before the value is cloned
/// (<https://w3c.github.io/IndexedDB/#add-or-put-operation>, steps 4-6).
const __tbIdbExplicitKey = (state, provided) => {
  const keyPath = state.meta.keyPath;
  if (keyPath !== null) {
    // A store with an inline key path takes its key from the value, so an
    // explicit key argument is a caller error (step 4).
    if (provided !== undefined) throw __tbIdbDataError();
    return undefined;
  }
  if (provided === undefined) {
    if (state.meta.autoIncrement) return undefined;
    throw __tbIdbDataError();
  }
  const key = __tbIdbToKey(provided, __tbPrivateSet());
  if (key === undefined) throw __tbIdbDataError();
  return key;
};

/// The key at an inline key path in an already-cloned value, `null` when the
/// generator supplies one, or an error when the path cannot be used
/// (<https://w3c.github.io/IndexedDB/#add-or-put-operation>, step 8).
const __tbIdbInlineKey = (state, value) => {
  const keyPath = state.meta.keyPath;
  const candidate = __tbIdbEvaluate(value, keyPath);
  if (candidate === undefined) {
    if (!state.meta.autoIncrement) throw __tbIdbDataError();
    if (!__tbIdbCanInject(value, keyPath)) throw __tbIdbDataError();
    return null;
  }
  const key = __tbIdbToKey(candidate, __tbPrivateSet());
  if (key === undefined) throw __tbIdbDataError();
  return key;
};

const __tbIdbQuery = (value, disallowNull) => {
  if (value === undefined || value === null) {
    if (disallowNull && value === null) throw __tbIdbDataError();
    return { __proto__: null, lower: null, upper: null, lowerOpen: false, upperOpen: false };
  }
  const range = __tbIdbKeyRangeState.get(value);
  if (range !== undefined) return range;
  const key = __tbIdbToKey(value, __tbPrivateSet());
  if (key === undefined) throw __tbIdbDataError();
  return { __proto__: null, lower: key, upper: key, lowerOpen: false, upperOpen: false };
};

const __tbIdbSortedRows = state => __tbIdbRowList(state);

/// Drops the derived record lists after a write, so the next query rebuilds
/// them from the transaction's rows.
const __tbIdbInvalidate = state => {
  state.rowList = null;
  state.indexEntries.clear();
};

const __tbIdbStoreValue = value => __tbEncode(value, undefined, null).payload;

const __tbIdbReadValue = payload => __tbDecode(payload, { __proto__: null });

const __tbIdbPut = (store, value, key, add) => {
  const state = __tbIdbCheckStore(store, add ? 'add' : 'put', true);
  const explicit = __tbIdbExplicitKey(state, key);
  // Clone before extracting an inline key, so a generated key lands in the
  // stored record and never mutates the caller's object
  // (<https://w3c.github.io/IndexedDB/#add-or-put-operation>, step 7).
  const clone = __tbIdbReadValue(__tbIdbStoreValue(value));
  const prepared = state.meta.keyPath !== null
    ? __tbIdbInlineKey(state, clone)
    : explicit === undefined ? null : explicit;
  return __tbIdbRequest(state.transaction, store, () => {
    const previousNextKey = state.meta.nextKey;
    try {
      // Key generation happens when the request runs, so requests queued before
      // this one take the lower keys
      // (<https://w3c.github.io/IndexedDB/#store-a-record-into-an-object-store>).
      let recordKey = prepared;
      if (recordKey === null) {
        recordKey = __tbIdbGenerateKey(state);
        if (recordKey === null) {
          throw new globalThis.DOMException(
            'The key generator has no keys left.',
            'ConstraintError',
          );
        }
        if (state.meta.keyPath !== null) {
          __tbIdbInjectKey(clone, state.meta.keyPath, __tbIdbKeyValue(recordKey));
        }
      } else {
        __tbIdbPossiblyUpdateKey(state, recordKey);
      }
      const payload = __tbIdbStoreValue(clone);
      const encoded = __tbIdbEncodeKey(recordKey);
      if (add && state.rows.has(encoded)) {
        throw new globalThis.DOMException('The key already exists in the object store.', 'ConstraintError');
      }
      __tbIdbCheckUnique(state, clone, recordKey);
      state.rows.set(encoded, { __proto__: null, key: recordKey, value: payload });
      __tbIdbInvalidate(state);
      return __tbIdbKeyValue(recordKey);
    } catch (error) {
      // A failed operation leaves no trace, including a key the generator
      // handed out before the constraint check failed.
      state.meta.nextKey = previousNextKey;
      throw error;
    }
  });
};

/// [EnforceRange] unsigned long conversion
/// (<https://webidl.spec.whatwg.org/#es-unsigned-long>).
const __tbIdbCountValue = value => {
  const number = Number(value);
  if (!__tbNumberIsFinite(number) || number < 0 || number > 4294967295) {
    throw new TypeError('The count provided is outside the range of a valid count.');
  }
  return __tbIdbTrunc(number);
};

/// Whether the first argument of getAll()/getAllKeys() is a key or key range
/// rather than an options dictionary: any value convertible to a key, even one
/// that fails conversion later, counts
/// (<https://w3c.github.io/IndexedDB/#potentially-valid-key-range>).
const __tbIdbPotentiallyValidRange = value => {
  if (__tbIdbKeyRangeState.get(value) !== undefined) return true;
  if (typeof value === 'number' || typeof value === 'string') return true;
  if (typeof value !== 'object' || value === null) return false;
  return __tbIsDate(value)
    || __tbIsArray(value)
    || __tbIsArrayBuffer(value)
    || __tbIsTypedArray(value)
    || __tbIsDataView(value);
};

/// `IDBGetAllOptions` dictionary conversion
/// (<https://w3c.github.io/IndexedDB/#dictdef-idbgetalloptions>).
const __tbIdbGetAllOptions = value => {
  if (value === undefined || value === null) {
    return { __proto__: null, query: undefined, count: undefined, direction: 'next' };
  }
  if (typeof value !== 'object' && typeof value !== 'function') {
    throw new TypeError('The options argument is not an object.');
  }
  return {
    __proto__: null,
    query: value.query,
    count: value.count,
    direction: __tbIdbDirection(value.direction),
  };
};

/// Retrieves values, keys, or record snapshots from an object store or index
/// for getAll(), getAllKeys(), and getAllRecords()
/// (<https://w3c.github.io/IndexedDB/#retrieve-multiple-items-from-an-index>).
const __tbIdbRetrieve = (handle, state, indexMeta, kind, index, queryOrOptions, count) => {
  let range;
  let direction;
  let limit;
  if (index && (__tbIdbPotentiallyValidRange(queryOrOptions)
    || queryOrOptions === undefined || queryOrOptions === null)) {
    range = __tbIdbQuery(queryOrOptions, false);
    direction = 'next';
    limit = count === undefined ? Infinity : __tbIdbCountValue(count);
  } else {
    const options = __tbIdbGetAllOptions(queryOrOptions);
    range = __tbIdbQuery(options.query, false);
    direction = options.direction;
    limit = options.count === undefined ? Infinity : __tbIdbCountValue(options.count);
  }
  if (limit === 0) limit = Infinity;
  const unique = direction === 'nextunique' || direction === 'prevunique';
  const reverse = direction === 'prev' || direction === 'prevunique';
  const storeState = indexMeta === null ? state : state.store;
  return __tbIdbRequest(state.transaction, handle, () => {
    const source = __tbIdbSourceRecords(storeState, indexMeta);
    const records = [];
    for (let recordIndex = 0; recordIndex < source.length; recordIndex++) {
      const record = source[recordIndex];
      if (!__tbIdbRangeIncludes(range, record.key)) continue;
      if (unique && records.length > 0
        && __tbIdbCompare(records[records.length - 1].key, record.key) === 0) continue;
      __tbArray.push(records, record);
    }
    if (reverse) {
      const reversed = [];
      for (let recordIndex = records.length - 1; recordIndex >= 0; recordIndex--) {
        __tbArray.push(reversed, records[recordIndex]);
      }
      records.length = 0;
      for (let recordIndex = 0; recordIndex < reversed.length; recordIndex++) {
        __tbArray.push(records, reversed[recordIndex]);
      }
    }
    if (records.length > limit) records.length = limit;
    if (kind === 'value') {
      const values = [];
      for (let recordIndex = 0; recordIndex < records.length; recordIndex++) {
        __tbArray.push(values, __tbIdbReadValue(records[recordIndex].row.value));
      }
      return values;
    }
    if (kind === 'key') {
      const keys = [];
      for (let recordIndex = 0; recordIndex < records.length; recordIndex++) {
        __tbArray.push(keys, __tbIdbKeyValue(records[recordIndex].primaryKey));
      }
      return keys;
    }
    const snapshots = [];
    for (let recordIndex = 0; recordIndex < records.length; recordIndex++) {
      __tbArray.push(snapshots, __tbIdbRecordSnapshot(records[recordIndex]));
    }
    return snapshots;
  });
};

const __tbIdbGetAll = (handle, state, indexMeta, kind, index, queryOrOptions, count) => {
  if (kind === 'record') return __tbIdbRetrieve(handle, state, indexMeta, kind, false, queryOrOptions, undefined);
  return __tbIdbRetrieve(handle, state, indexMeta, kind, index, queryOrOptions, count);
};

const __tbIdbGet = (store, query, wantKey, all, count) => {
  const state = __tbIdbCheckStore(store, all ? 'getAll' : 'get');
  if (all) return __tbIdbGetAll(store, state, null, wantKey ? 'key' : 'value', true, query, count);
  const range = __tbIdbQuery(query, true);
  return __tbIdbRequest(state.transaction, store, () => {
    const rows = __tbIdbSortedRows(state);
    for (let index = 0; index < rows.length; index++) {
      const entry = rows[index];
      if (!__tbIdbRangeIncludes(range, entry[1].key)) continue;
      return wantKey ? __tbIdbKeyValue(entry[1].key) : __tbIdbReadValue(entry[1].value);
    }
    return undefined;
  });
};

const __tbIdbCount = (store, query) => {
  const state = __tbIdbCheckStore(store, 'count');
  const range = __tbIdbQuery(query, false);
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
  const range = __tbIdbQuery(query, true);
  return __tbIdbRequest(state.transaction, store, () => {
    const rows = __tbIdbSortedRows(state);
    for (let index = 0; index < rows.length; index++) {
      if (__tbIdbRangeIncludes(range, __tbIdbDecodeKey(rows[index][0]))) {
        state.rows.delete(rows[index][0]);
      }
    }
    __tbIdbInvalidate(state);
    return undefined;
  });
};

const __tbIdbClear = store => {
  const state = __tbIdbCheckStore(store, 'clear', true);
  return __tbIdbRequest(state.transaction, store, () => {
    state.rows.clear();
    __tbIdbInvalidate(state);
    return undefined;
  });
};

// ── indexes

const __tbIdbIndexHandle = (storeState, storeHandle, meta) => {
  const cached = storeState.indexHandles.get(meta.name);
  if (cached !== undefined && !__tbIdbIndexState.get(cached).deleted) return cached;
  const handle = __tbIdbConstruct(__tbIdbIndexConstructor);
  __tbIdbIndexState.set(handle, {
    __proto__: null,
    transaction: storeState.transaction,
    store: storeState,
    storeHandle,
    meta,
    deleted: false,
    keyPathValue: null,
  });
  storeState.indexHandles.set(meta.name, handle);
  return handle;
};

/// Validation runs as a database task after the requests queued before the
/// index existed, so an upgrade that already wrote conflicting records aborts
/// when the index is created rather than when the conflicting record is stored
/// (<https://w3c.github.io/IndexedDB/#dom-idbobjectstore-createindex>).
const __tbIdbValidateIndex = (state, indexMeta) => {
  if (!indexMeta.unique) return;
  const entries = __tbIdbIndexEntries(state, indexMeta);
  for (let index = 1; index < entries.length; index++) {
    if (__tbIdbCompare(entries[index - 1].key, entries[index].key) !== 0) continue;
    throw new globalThis.DOMException(
      `The index '${indexMeta.name}' already contains duplicate keys.`,
      'ConstraintError',
    );
  }
};

/// `IDBIndexParameters` dictionary conversion: `null` is an empty dictionary
/// and any other non-object throws a `TypeError`
/// (<https://webidl.spec.whatwg.org/#es-dictionary>).
const __tbIdbIndexParameters = value => {
  if (value === undefined || value === null) {
    return { __proto__: null, unique: false, multiEntry: false };
  }
  if (typeof value !== 'object' && typeof value !== 'function') {
    throw new TypeError('The options argument is not an object.');
  }
  return {
    __proto__: null,
    unique: Boolean(value.unique),
    multiEntry: Boolean(value.multiEntry),
  };
};

const __tbIdbCreateIndex = (store, name, keyPath, options) => {
  const indexName = String(name);
  const indexKeyPath = __tbIdbConvertKeyPath(keyPath);
  const parameters = __tbIdbIndexParameters(options);
  const state = __tbIdbObjectStoreState.get(store);
  if (state === undefined) throw new TypeError('Illegal invocation');
  const data = state.transaction;
  if (!data.versionChange) {
    throw new globalThis.DOMException(
      'An index can only be created inside a version change transaction.',
      'InvalidStateError',
    );
  }
  if (state.deleted) {
    throw new globalThis.DOMException(
      'The index cannot be created on a deleted object store.',
      'InvalidStateError',
    );
  }
  if (data.finished || data.aborted || data.committing) {
    throw new globalThis.DOMException(
      'The index cannot be created on an inactive transaction.',
      'TransactionInactiveError',
    );
  }
  if (__tbArray.find(state.meta.indexes, index => index.name === indexName) !== undefined) {
    throw new globalThis.DOMException(
      `An index named '${indexName}' already exists.`,
      'ConstraintError',
    );
  }
  if (indexKeyPath === null || !__tbIdbValidKeyPath(indexKeyPath)) {
    throw new globalThis.DOMException(
      `The key path '${indexKeyPath}' is not a valid key path.`,
      'SyntaxError',
    );
  }
  if (parameters.multiEntry && __tbIsArray(indexKeyPath)) {
    throw new globalThis.DOMException(
      'A multiEntry index cannot use a sequence key path.',
      'InvalidAccessError',
    );
  }
  const meta = {
    __proto__: null,
    name: indexName,
    keyPath: indexKeyPath,
    unique: parameters.unique,
    multiEntry: parameters.multiEntry,
    // The index's uniqueness applies to records stored after its creation
    // request runs, not to requests already queued when createIndex() was
    // called (<https://w3c.github.io/IndexedDB/#dom-idbobjectstore-createindex>).
    pending: true,
    created: true,
  };
  __tbArray.push(state.meta.indexes, meta);
  const index = __tbIdbIndexHandle(state, store, meta);
  __tbIdbRequest(data, index, () => {
    __tbIdbValidateIndex(state, meta);
    meta.pending = false;
  });
  return index;
};

const __tbIdbStoreIndex = (store, name) => {
  const state = __tbIdbObjectStoreState.get(store);
  if (state === undefined) throw new TypeError('Illegal invocation');
  if (state.deleted) {
    throw new globalThis.DOMException('The object store has been deleted.', 'InvalidStateError');
  }
  if (state.transaction.finished) {
    throw new globalThis.DOMException('The transaction has finished.', 'InvalidStateError');
  }
  const indexName = String(name);
  const meta = __tbArray.find(state.meta.indexes, index => index.name === indexName);
  if (meta === undefined) {
    throw new globalThis.DOMException(`The index '${indexName}' does not exist.`, 'NotFoundError');
  }
  return __tbIdbIndexHandle(state, store, meta);
};

const __tbIdbDeleteIndex = (store, name) => {
  const state = __tbIdbObjectStoreState.get(store);
  if (state === undefined) throw new TypeError('Illegal invocation');
  const data = state.transaction;
  if (!data.versionChange) {
    throw new globalThis.DOMException(
      'An index can only be deleted inside a version change transaction.',
      'InvalidStateError',
    );
  }
  if (state.deleted) {
    throw new globalThis.DOMException('The object store has been deleted.', 'InvalidStateError');
  }
  if (data.finished || data.aborted || data.committing) {
    throw new globalThis.DOMException(
      'The index cannot be deleted on an inactive transaction.',
      'TransactionInactiveError',
    );
  }
  const indexName = String(name);
  const index = __tbArray.findIndex(state.meta.indexes, entry => entry.name === indexName);
  if (index === -1) {
    throw new globalThis.DOMException(`The index '${indexName}' does not exist.`, 'NotFoundError');
  }
  __tbArray.splice(state.meta.indexes, index, 1);
  const handle = state.indexHandles.get(indexName);
  if (handle !== undefined) __tbIdbIndexState.get(handle).deleted = true;
  state.indexHandles.delete(indexName);
  __tbIdbInvalidate(state);
};

const __tbIdbIndexQuery = (index, query, wantKey, all, count) => {
  const state = __tbIdbCheckIndex(index, all ? 'getAll' : 'get');
  if (all) return __tbIdbGetAll(index, state, state.meta, wantKey ? 'key' : 'value', true, query, count);
  const range = __tbIdbQuery(query, true);
  return __tbIdbRequest(state.transaction, index, () => {
    const entries = __tbIdbIndexEntries(state.store, state.meta);
    for (let entryIndex = 0; entryIndex < entries.length; entryIndex++) {
      const entry = entries[entryIndex];
      if (!__tbIdbRangeIncludes(range, entry.key)) continue;
      return wantKey
        ? __tbIdbKeyValue(entry.primaryKey)
        : __tbIdbReadValue(entry.row.value);
    }
    return undefined;
  });
};

const __tbIdbIndexCount = (index, query) => {
  const state = __tbIdbCheckIndex(index, 'count');
  const range = __tbIdbQuery(query, false);
  return __tbIdbRequest(state.transaction, index, () => {
    const entries = __tbIdbIndexEntries(state.store, state.meta);
    let count = 0;
    for (let entryIndex = 0; entryIndex < entries.length; entryIndex++) {
      if (__tbIdbRangeIncludes(range, entries[entryIndex].key)) count++;
    }
    return count;
  });
};

// ── cursors

/// The records of an object store or index in ascending order: an index's
/// records are ordered by index key and then by primary key
/// (<https://w3c.github.io/IndexedDB/#cursor-construct>).
const __tbIdbSourceRecords = (store, indexMeta) => {
  if (indexMeta !== null) return __tbIdbIndexEntries(store, indexMeta);
  const rows = __tbIdbRowList(store);
  const records = [];
  for (let index = 0; index < rows.length; index++) {
    const row = rows[index][1];
    __tbArray.push(records, {
      __proto__: null,
      key: row.key,
      primaryKey: row.key,
      row,
    });
  }
  return records;
};

const __tbIdbCursorRecords = state => __tbIdbSourceRecords(state.store, state.indexMeta);

/// The first record a "next" or "nextunique" cursor moves to
/// (<https://w3c.github.io/IndexedDB/#cursor-iteration-operation>).
const __tbIdbCursorNext = (state, records, key, primaryKey, unique) => {
  for (let index = 0; index < records.length; index++) {
    const record = records[index];
    if (!__tbIdbRangeIncludes(state.range, record.key)) continue;
    if (key !== undefined) {
      const order = __tbIdbCompare(record.key, key);
      if (order < 0) continue;
      if (order === 0 && primaryKey !== undefined
        && __tbIdbCompare(record.primaryKey, primaryKey) < 0) continue;
    }
    if (state.position !== undefined) {
      const order = __tbIdbCompare(record.key, state.position);
      if (order < 0) continue;
      if (order === 0) {
        if (unique || state.indexMeta === null) continue;
        if (__tbIdbCompare(record.primaryKey, state.objectStorePosition) <= 0) continue;
      }
    }
    return record;
  }
  return null;
};

/// The last record a "prev" cursor moves to, or the last record a
/// "prevunique" cursor considers before it snaps to that key's first record
/// (<https://w3c.github.io/IndexedDB/#cursor-iteration-operation>).
const __tbIdbCursorPrev = (state, records, key, primaryKey, unique) => {
  for (let index = records.length - 1; index >= 0; index--) {
    const record = records[index];
    if (!__tbIdbRangeIncludes(state.range, record.key)) continue;
    if (key !== undefined) {
      const order = __tbIdbCompare(record.key, key);
      if (order > 0) continue;
      if (order === 0 && primaryKey !== undefined && !unique
        && __tbIdbCompare(record.primaryKey, primaryKey) > 0) continue;
    }
    if (state.position !== undefined) {
      const order = __tbIdbCompare(record.key, state.position);
      if (order > 0) continue;
      if (order === 0) {
        if (unique || state.indexMeta === null) continue;
        if (__tbIdbCompare(record.primaryKey, state.objectStorePosition) >= 0) continue;
      }
    }
    return record;
  }
  return null;
};

const __tbIdbCursorForward = direction => direction === 'next' || direction === 'nextunique';

/// Moves a cursor `count` records, or to `key`/`primaryKey`, and returns the
/// cursor or null when the iteration is exhausted
/// (<https://w3c.github.io/IndexedDB/#cursor-iteration-operation>).
const __tbIdbIterateCursor = (state, key, primaryKey, count) => {
  const records = __tbIdbCursorRecords(state);
  const unique = state.direction === 'nextunique' || state.direction === 'prevunique';
  const forward = __tbIdbCursorForward(state.direction);
  let remaining = count === undefined ? 1 : count;
  let found = null;
  while (remaining > 0) {
    found = forward
      ? __tbIdbCursorNext(state, records, key, primaryKey, unique)
      : __tbIdbCursorPrev(state, records, key, primaryKey, unique);
    if (found === null) break;
    if (!forward && unique) {
      // "prevunique" visits the same records as "nextunique", in reverse
      // order, so the record it settles on is the first record with the key.
      for (let index = 0; index < records.length; index++) {
        if (__tbIdbCompare(records[index].key, found.key) !== 0) continue;
        found = records[index];
        break;
      }
    }
    state.position = found.key;
    state.objectStorePosition = found.primaryKey;
    remaining--;
  }
  if (found === null) {
    state.key = undefined;
    state.primaryKey = undefined;
    state.keyValue = undefined;
    state.primaryKeyValue = undefined;
    state.value = undefined;
    state.gotValue = false;
    if (state.indexMeta !== null) state.objectStorePosition = undefined;
    return null;
  }
  state.key = found.key;
  state.primaryKey = found.primaryKey;
  state.keyValue = __tbIdbKeyValue(found.key);
  state.primaryKeyValue = __tbIdbKeyValue(found.primaryKey);
  state.value = state.keyOnly ? undefined : __tbIdbReadValue(found.row.value);
  state.gotValue = true;
  return state.handle;
};

const __tbIdbDirection = value => {
  const direction = value === undefined ? 'next' : String(value);
  switch (direction) {
    case 'next':
    case 'nextunique':
    case 'prev':
    case 'prevunique':
      return direction;
  }
  throw new TypeError(
    `The provided value '${direction}' is not a valid enum value of type IDBCursorDirection.`,
  );
};

/// Creates a cursor and queues its first iteration as a request
/// (<https://w3c.github.io/IndexedDB/#dom-idbobjectstore-opencursor>).
const __tbIdbOpenCursor = (handle, storeState, indexMeta, query, direction, keyOnly) => {
  const range = __tbIdbQuery(query, false);
  const cursor = __tbIdbConstruct(
    keyOnly ? __tbIdbCursorConstructor : __tbIdbCursorWithValueConstructor);
  const state = {
    __proto__: null,
    handle: cursor,
    source: handle,
    transaction: storeState.transaction,
    store: storeState,
    indexMeta,
    range,
    direction,
    keyOnly,
    position: undefined,
    objectStorePosition: undefined,
    key: undefined,
    primaryKey: undefined,
    keyValue: undefined,
    primaryKeyValue: undefined,
    value: undefined,
    gotValue: false,
    request: null,
  };
  __tbIdbCursorState.set(cursor, state);
  const request = __tbIdbRequest(storeState.transaction, handle, () => {
    state.gotValue = false;
    return __tbIdbIterateCursor(state);
  });
  state.request = request;
  return request;
};

/// Queues the cursor's existing request again, so every step fires `success`
/// at the request that opened the cursor
/// (<https://w3c.github.io/IndexedDB/#dom-idbcursor-continue>).
const __tbIdbCursorQueue = (state, operation) => {
  const requestState = __tbIdbRequestState.get(state.request);
  requestState.done = false;
  requestState.result = undefined;
  requestState.error = null;
  requestState.operation = operation;
  state.gotValue = false;
  __tbIdbQueue(state.transaction, state.request);
};

const __tbIdbCursorStateOf = cursor => {
  const state = __tbIdbCursorState.get(cursor);
  if (state === undefined) throw new TypeError('Illegal invocation');
  return state;
};

const __tbIdbCursorCheck = (state, what, readOnly) => {
  const data = state.transaction;
  if (data.finished || data.aborted || data.committing) {
    throw new globalThis.DOMException(
      `The ${what} request was not allowed on this transaction.`,
      'TransactionInactiveError',
    );
  }
  if (readOnly && data.mode === 'readonly') {
    throw new globalThis.DOMException(
      `The ${what} request was not allowed on a read-only transaction.`,
      'ReadOnlyError',
    );
  }
  if (state.store.deleted || state.indexMeta !== null
    && __tbArray.indexOf(state.store.meta.indexes, state.indexMeta) === -1) {
    throw new globalThis.DOMException('The cursor source has been deleted.', 'InvalidStateError');
  }
};

const __tbIdbCursorAdvance = (cursor, count) => {
  const state = __tbIdbCursorStateOf(cursor);
  const number = Number(count);
  if (!__tbNumberIsFinite(number) || number < 0 || number > 4294967295) {
    throw new TypeError('The count provided is outside the range of a valid count.');
  }
  if (__tbIdbTrunc(number) === 0) {
    throw new TypeError("Failed to execute 'advance' on 'IDBCursor': count must be greater than 0.");
  }
  __tbIdbCursorCheck(state, 'advance');
  if (!state.gotValue) {
    throw new globalThis.DOMException('The cursor is not positioned on a record.', 'InvalidStateError');
  }
  const steps = __tbIdbTrunc(number);
  __tbIdbCursorQueue(state, () => __tbIdbIterateCursor(state, undefined, undefined, steps));
};

const __tbIdbCursorContinue = (cursor, key) => {
  const state = __tbIdbCursorStateOf(cursor);
  __tbIdbCursorCheck(state, 'continue');
  if (!state.gotValue) {
    throw new globalThis.DOMException('The cursor is not positioned on a record.', 'InvalidStateError');
  }
  let target;
  if (key !== undefined) {
    target = __tbIdbToKey(key, __tbPrivateSet());
    if (target === undefined) throw __tbIdbDataError();
    const order = __tbIdbCompare(target, state.position);
    if (__tbIdbCursorForward(state.direction) ? order <= 0 : order >= 0) {
      throw new globalThis.DOMException(
        'The key is not beyond the cursor position.',
        'DataError',
      );
    }
  }
  __tbIdbCursorQueue(state, () => __tbIdbIterateCursor(state, target, undefined, 1));
};

const __tbIdbCursorContinuePrimaryKey = (cursor, key, primaryKey) => {
  const state = __tbIdbCursorStateOf(cursor);
  __tbIdbCursorCheck(state, 'continuePrimaryKey');
  if (state.indexMeta === null) {
    throw new globalThis.DOMException(
      'continuePrimaryKey is only allowed on an index cursor.',
      'InvalidAccessError',
    );
  }
  if (state.direction !== 'next' && state.direction !== 'prev') {
    throw new globalThis.DOMException(
      'continuePrimaryKey is not allowed on a unique cursor.',
      'InvalidAccessError',
    );
  }
  if (!state.gotValue) {
    throw new globalThis.DOMException('The cursor is not positioned on a record.', 'InvalidStateError');
  }
  const target = __tbIdbToKey(key, __tbPrivateSet());
  if (target === undefined) throw __tbIdbDataError();
  const targetPrimary = __tbIdbToKey(primaryKey, __tbPrivateSet());
  if (targetPrimary === undefined) throw __tbIdbDataError();
  const forward = state.direction === 'next';
  const order = __tbIdbCompare(target, state.position);
  if (forward ? order < 0 : order > 0) {
    throw new globalThis.DOMException('The key is not beyond the cursor position.', 'DataError');
  }
  if (order === 0) {
    const primaryOrder = __tbIdbCompare(targetPrimary, state.objectStorePosition);
    if (forward ? primaryOrder <= 0 : primaryOrder >= 0) {
      throw new globalThis.DOMException(
        'The primary key is not beyond the cursor position.',
        'DataError',
      );
    }
  }
  __tbIdbCursorQueue(state, () => __tbIdbIterateCursor(state, target, targetPrimary, 1));
};

const __tbIdbCursorUpdate = (cursor, value) => {
  const state = __tbIdbCursorStateOf(cursor);
  const data = state.transaction;
  __tbIdbCursorCheck(state, 'update', true);
  if (!state.gotValue) {
    throw new globalThis.DOMException('The cursor is not positioned on a record.', 'InvalidStateError');
  }
  if (state.keyOnly) {
    throw new globalThis.DOMException(
      'A key-only cursor cannot update a record.',
      'InvalidStateError',
    );
  }
  const clone = __tbIdbReadValue(__tbIdbStoreValue(value));
  const keyPath = state.store.meta.keyPath;
  if (keyPath !== null) {
    const key = __tbIdbExtractKey(clone, keyPath, false);
    if (key === undefined || __tbIdbCompare(key, state.primaryKey) !== 0) throw __tbIdbDataError();
  }
  const payload = __tbIdbStoreValue(clone);
  return __tbIdbRequest(data, cursor, () => {
    __tbIdbCheckUnique(state.store, clone, state.primaryKey);
    state.store.rows.set(__tbIdbEncodeKey(state.primaryKey), {
      __proto__: null,
      key: state.primaryKey,
      value: payload,
    });
    __tbIdbInvalidate(state.store);
    return __tbIdbKeyValue(state.primaryKey);
  });
};

const __tbIdbCursorDelete = cursor => {
  const state = __tbIdbCursorStateOf(cursor);
  const data = state.transaction;
  __tbIdbCursorCheck(state, 'delete', true);
  if (!state.gotValue) {
    throw new globalThis.DOMException('The cursor is not positioned on a record.', 'InvalidStateError');
  }
  if (state.keyOnly) {
    throw new globalThis.DOMException(
      'A key-only cursor cannot delete a record.',
      'InvalidStateError',
    );
  }
  return __tbIdbRequest(data, cursor, () => {
    state.store.rows.delete(__tbIdbEncodeKey(state.primaryKey));
    __tbIdbInvalidate(state.store);
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
    : __tbIdbUnjson(__tbIdbJson(oldMeta));
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
    if (first === undefined) throw __tbIdbDataError();
    const second = __tbIdbToKey(upper, __tbPrivateSet());
    if (second === undefined) throw __tbIdbDataError();
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

// https://w3c.github.io/IndexedDB/#idbrecord
const __tbIdbRecordConstructor = globalThis.IDBRecord = class IDBRecord {
  constructor() { throw new TypeError('Illegal constructor'); }
  get key() {
    const state = __tbIdbRecordState.get(this);
    if (state === undefined) throw new TypeError('Illegal invocation');
    return state.keyValue;
  }
  get primaryKey() {
    const state = __tbIdbRecordState.get(this);
    if (state === undefined) throw new TypeError('Illegal invocation');
    return state.primaryKeyValue;
  }
  get value() {
    const state = __tbIdbRecordState.get(this);
    if (state === undefined) throw new TypeError('Illegal invocation');
    return state.value;
  }
};
Object.defineProperty(globalThis.IDBRecord.prototype, Symbol.toStringTag, { value: 'IDBRecord', configurable: true });

const __tbIdbRecordSnapshot = record => {
  const snapshot = __tbIdbConstruct(__tbIdbRecordConstructor);
  __tbIdbRecordState.set(snapshot, {
    __proto__: null,
    keyValue: __tbIdbKeyValue(record.key),
    primaryKeyValue: __tbIdbKeyValue(record.primaryKey),
    value: __tbIdbReadValue(record.row.value),
  });
  return snapshot;
};

// https://w3c.github.io/IndexedDB/#idbindex
const __tbIdbIndexConstructor = globalThis.IDBIndex = class IDBIndex {
  constructor() { throw new TypeError('Illegal constructor'); }
  get name() {
    const state = __tbIdbIndexState.get(this);
    if (state === undefined) throw new TypeError('Illegal invocation');
    return state.meta.name;
  }
  set name(value) {
    const state = __tbIdbIndexState.get(this);
    if (state === undefined) throw new TypeError('Illegal invocation');
    const data = state.transaction;
    if (!data.versionChange) {
      throw new globalThis.DOMException(
        'An index can only be renamed inside a version change transaction.',
        'InvalidStateError',
      );
    }
    if (data.finished || data.aborted || data.committing) {
      throw new globalThis.DOMException(
        'The index cannot be renamed on an inactive transaction.',
        'TransactionInactiveError',
      );
    }
    if (state.deleted || state.store.deleted) {
      throw new globalThis.DOMException('The index has been deleted.', 'InvalidStateError');
    }
    const newName = String(value);
    const oldName = state.meta.name;
    if (newName === oldName) return;
    if (__tbArray.find(state.store.meta.indexes, index => index.name === newName) !== undefined) {
      throw new globalThis.DOMException(
        `An index named '${newName}' already exists.`,
        'ConstraintError',
      );
    }
    state.meta.name = newName;
    if (state.originalName === undefined) state.originalName = oldName;
    const handle = state.store.indexHandles.get(oldName);
    if (handle !== undefined) {
      state.store.indexHandles.delete(oldName);
      state.store.indexHandles.set(newName, handle);
    }
  }
  get objectStore() {
    const state = __tbIdbIndexState.get(this);
    if (state === undefined) throw new TypeError('Illegal invocation');
    return state.storeHandle;
  }
  get keyPath() {
    const state = __tbIdbIndexState.get(this);
    if (state === undefined) throw new TypeError('Illegal invocation');
    if (state.keyPathValue === null) {
      state.keyPathValue = typeof state.meta.keyPath === 'string'
        ? state.meta.keyPath
        : __tbArray.slice(state.meta.keyPath);
    }
    return state.keyPathValue;
  }
  get multiEntry() {
    const state = __tbIdbIndexState.get(this);
    if (state === undefined) throw new TypeError('Illegal invocation');
    return state.meta.multiEntry;
  }
  get unique() {
    const state = __tbIdbIndexState.get(this);
    if (state === undefined) throw new TypeError('Illegal invocation');
    return state.meta.unique;
  }
  get(query) {
    if (arguments.length < 1) throw new TypeError("Failed to execute 'get' on 'IDBIndex': 1 argument required, but only 0 present.");
    return __tbIdbIndexQuery(this, query, false, false);
  }
  getKey(query) {
    if (arguments.length < 1) throw new TypeError("Failed to execute 'getKey' on 'IDBIndex': 1 argument required, but only 0 present.");
    return __tbIdbIndexQuery(this, query, true, false);
  }
  getAll(query, count) { return __tbIdbIndexQuery(this, query, false, true, count); }
  getAllKeys(query, count) { return __tbIdbIndexQuery(this, query, true, true, count); }
  getAllRecords(options) {
    const state = __tbIdbCheckIndex(this, 'getAllRecords');
    return __tbIdbGetAll(this, state, state.meta, 'record', false, options, undefined);
  }
  count(query) { return __tbIdbIndexCount(this, query); }
  openCursor(query, direction) {
    const state = __tbIdbIndexState.get(this);
    if (state === undefined) throw new TypeError('Illegal invocation');
    const dir = __tbIdbDirection(direction);
    __tbIdbCheckIndex(this, 'openCursor');
    return __tbIdbOpenCursor(this, state.store, state.meta, query, dir, false);
  }
  openKeyCursor(query, direction) {
    const state = __tbIdbIndexState.get(this);
    if (state === undefined) throw new TypeError('Illegal invocation');
    const dir = __tbIdbDirection(direction);
    __tbIdbCheckIndex(this, 'openKeyCursor');
    return __tbIdbOpenCursor(this, state.store, state.meta, query, dir, true);
  }
};
Object.defineProperty(globalThis.IDBIndex.prototype, Symbol.toStringTag, { value: 'IDBIndex', configurable: true });

// https://w3c.github.io/IndexedDB/#idbcursor
const __tbIdbCursorConstructor = globalThis.IDBCursor = class IDBCursor {
  constructor() { throw new TypeError('Illegal constructor'); }
  get source() {
    return __tbIdbCursorStateOf(this).source;
  }
  get direction() {
    return __tbIdbCursorStateOf(this).direction;
  }
  get key() {
    return __tbIdbCursorStateOf(this).keyValue;
  }
  get primaryKey() {
    return __tbIdbCursorStateOf(this).primaryKeyValue;
  }
  get request() {
    return __tbIdbCursorStateOf(this).request;
  }
  advance(count) {
    if (arguments.length < 1) throw new TypeError("Failed to execute 'advance' on 'IDBCursor': 1 argument required, but only 0 present.");
    __tbIdbCursorAdvance(this, count);
  }
  continue(key) {
    __tbIdbCursorContinue(this, key);
  }
  continuePrimaryKey(key, primaryKey) {
    if (arguments.length < 2) throw new TypeError("Failed to execute 'continuePrimaryKey' on 'IDBCursor': 2 arguments required.");
    __tbIdbCursorContinuePrimaryKey(this, key, primaryKey);
  }
  update(value) {
    if (arguments.length < 1) throw new TypeError("Failed to execute 'update' on 'IDBCursor': 1 argument required, but only 0 present.");
    return __tbIdbCursorUpdate(this, value);
  }
  delete() {
    return __tbIdbCursorDelete(this);
  }
};
Object.defineProperty(globalThis.IDBCursor.prototype, Symbol.toStringTag, { value: 'IDBCursor', configurable: true });

// https://w3c.github.io/IndexedDB/#idbcursorwithvalue
const __tbIdbCursorWithValueConstructor = globalThis.IDBCursorWithValue = class IDBCursorWithValue extends globalThis.IDBCursor {
  constructor() { throw new TypeError('Illegal constructor'); }
  get value() {
    return __tbIdbCursorStateOf(this).value;
  }
};
Object.defineProperty(globalThis.IDBCursorWithValue.prototype, Symbol.toStringTag, { value: 'IDBCursorWithValue', configurable: true });

// https://w3c.github.io/IndexedDB/#idbobjectstore
const __tbIdbObjectStoreConstructor = globalThis.IDBObjectStore = class IDBObjectStore {
  constructor() { throw new TypeError('Illegal constructor'); }
  get name() {
    const state = __tbIdbObjectStoreState.get(this);
    if (state === undefined) throw new TypeError('Illegal invocation');
    return state.meta.name;
  }
  set name(value) {
    const state = __tbIdbObjectStoreState.get(this);
    if (state === undefined) throw new TypeError('Illegal invocation');
    const data = state.transaction;
    if (!data.versionChange) {
      throw new globalThis.DOMException(
        'An object store can only be renamed inside a version change transaction.',
        'InvalidStateError',
      );
    }
    if (data.finished || data.aborted || data.committing) {
      throw new globalThis.DOMException(
        'The object store cannot be renamed on an inactive transaction.',
        'TransactionInactiveError',
      );
    }
    if (state.deleted) {
      throw new globalThis.DOMException('The object store has been deleted.', 'InvalidStateError');
    }
    const newName = String(value);
    const oldName = state.meta.name;
    if (newName === oldName) return;
    const dbState = __tbIdbDatabaseState.get(data.db);
    if (__tbArray.find(dbState.meta.stores, store => store.name === newName) !== undefined) {
      throw new globalThis.DOMException(
        `An object store named '${newName}' already exists.`,
        'ConstraintError',
      );
    }
    state.meta.name = newName;
    state.renamed = true;
    if (state.originalName === undefined) state.originalName = oldName;
    const scopeIndex = __tbArray.indexOf(data.scope, oldName);
    if (scopeIndex !== -1) data.scope[scopeIndex] = newName;
    const storeState = data.stores.get(oldName);
    if (storeState !== undefined) {
      data.stores.delete(oldName);
      data.stores.set(newName, storeState);
    }
    const handle = data.handles.get(oldName);
    if (handle !== undefined) {
      data.handles.delete(oldName);
      data.handles.set(newName, handle);
    }
    const dbStore = __tbArray.find(dbState.meta.stores, store => store.name === oldName);
    if (dbStore !== undefined) dbStore.name = newName;
    __tbArray.push(data.removedStores, oldName);
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
    return __tbIdbNames(__tbArray.map(state.meta.indexes, index => index.name));
  }
  createIndex(name, keyPath, options) {
    if (arguments.length < 2) throw new TypeError("Failed to execute 'createIndex' on 'IDBObjectStore': 2 arguments required.");
    return __tbIdbCreateIndex(this, name, keyPath, options);
  }
  index(name) {
    if (arguments.length < 1) throw new TypeError("Failed to execute 'index' on 'IDBObjectStore': 1 argument required, but only 0 present.");
    return __tbIdbStoreIndex(this, name);
  }
  deleteIndex(name) {
    if (arguments.length < 1) throw new TypeError("Failed to execute 'deleteIndex' on 'IDBObjectStore': 1 argument required, but only 0 present.");
    __tbIdbDeleteIndex(this, name);
  }
  openCursor(query, direction) {
    const state = __tbIdbObjectStoreState.get(this);
    if (state === undefined) throw new TypeError('Illegal invocation');
    const dir = __tbIdbDirection(direction);
    __tbIdbCheckStore(this, 'openCursor', false);
    return __tbIdbOpenCursor(this, state, null, query, dir, false);
  }
  openKeyCursor(query, direction) {
    const state = __tbIdbObjectStoreState.get(this);
    if (state === undefined) throw new TypeError('Illegal invocation');
    const dir = __tbIdbDirection(direction);
    __tbIdbCheckStore(this, 'openKeyCursor', false);
    return __tbIdbOpenCursor(this, state, null, query, dir, true);
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
  getAllRecords(options) {
    const state = __tbIdbCheckStore(this, 'getAllRecords');
    return __tbIdbGetAll(this, state, null, 'record', false, options, undefined);
  }
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
  get error() {
    const data = __tbIdbTransactionState.get(this);
    if (data === undefined) throw new TypeError('Illegal invocation');
    return data.error;
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
    data.explicitCommit = true;
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
    state.created = true;
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
    // The connection is close-pending rather than gone: a running version
    // change transaction still finishes and its requests still work
    // (<https://w3c.github.io/IndexedDB/#dom-idbdatabase-close>).
    data.closed = true;
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
      const meta = __tbIdbUnjson(text);
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
