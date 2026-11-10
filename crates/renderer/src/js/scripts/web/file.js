// https://xhr.spec.whatwg.org/#interface-progressevent
globalThis.ProgressEvent = class ProgressEvent extends Event {
  constructor(type, init) {
    const eventInit = init === undefined ? {} : Object(init);
    super(String(type), eventInit);
    __tbProgressData.set(this, {
        lengthComputable: eventInit.lengthComputable === undefined ? false : Boolean(eventInit.lengthComputable),
        loaded: eventInit.loaded === undefined ? 0 : Number(eventInit.loaded),
        total: eventInit.total === undefined ? 0 : Number(eventInit.total),
    });
  }
  get lengthComputable() { return __tbBrand(this, __tbProgressData).lengthComputable; }
  get loaded() { return __tbBrand(this, __tbProgressData).loaded; }
  get total() { return __tbBrand(this, __tbProgressData).total; }
};
Object.defineProperty(globalThis.ProgressEvent.prototype, Symbol.toStringTag, { value: 'ProgressEvent', writable: false, enumerable: false, configurable: true });
// Web IDL sequence conversion for blob parts: the sequence must be an object,
// elements convert left to right, and platform objects with indexed
// properties convert through `length` and the index getters
// (<https://webidl.spec.whatwg.org/#es-sequence>).
const __tbConvertBlobParts = blobParts => {
  const parts = blobParts === undefined ? [] : blobParts;
  if (parts === null || (typeof parts !== 'object' && typeof parts !== 'function')) {
    throw new TypeError('The blobParts argument must be a sequence');
  }
  const iteratorMethod = parts[Symbol.iterator];
  const converted = [];
  const convertPart = part => {
    const fromBlob = part !== null && typeof part === 'object' ? __tbBlobData.get(part) : undefined;
    if (fromBlob !== undefined) {
      __tbArray.push(converted, fromBlob.bytes);
    } else if (part instanceof ArrayBuffer || (typeof SharedArrayBuffer !== 'undefined' && part instanceof SharedArrayBuffer)) {
      __tbArray.push(converted, __tbBytesCopy(new __tbUint8Array(part)));
    } else if (ArrayBuffer.isView(part)) {
      __tbArray.push(converted, __tbBytesCopy(new __tbUint8Array(part.buffer, part.byteOffset, part.byteLength)));
    } else {
      __tbArray.push(converted, String(part));
    }
  };
  if (iteratorMethod !== undefined && iteratorMethod !== null) {
    if (typeof iteratorMethod !== 'function') {
      throw new TypeError('The blobParts argument must be iterable');
    }
    const iterator = __tbApply(iteratorMethod, parts, []);
    while (true) {
      const step = iterator.next();
      if (step.done) break;
      convertPart(step.value);
    }
  } else if (typeof parts.item === 'function' && typeof parts.length === 'number') {
    const length = Number(parts.length);
    for (let index = 0; index < length; index++) convertPart(parts[index]);
  } else {
    throw new TypeError('The blobParts argument must be iterable');
  }
  return converted;
};
// Dictionary members evaluate in lexicographic order, so each member gets a
// helper the constructors call in IDL order
// (<https://webidl.spec.whatwg.org/#es-dictionary>).
const __tbEndings = value => {
  if (value === undefined) return 'transparent';
  value = String(value);
  if (value !== 'transparent' && value !== 'native') {
    throw new TypeError('The endings option must be transparent or native');
  }
  return value;
};
const __tbBlobType = value => {
  if (value === undefined) return '';
  // https://w3c.github.io/FileAPI/#dfn-type
  value = String(value);
  for (let index = 0; index < value.length; index++) {
    const code = value.charCodeAt(index);
    if (code < 0x20 || code > 0x7E) return '';
  }
  return value.toLowerCase();
};
const __tbBlobBytes = (converted, endings) => {
  const chunks = __tbArray.map(converted, part => typeof part === 'string'
    ? __tbUtf8Encode(endings === 'native' ? __tbNativeEndings(part) : part)
    : part);
  let length = 0;
  for (let index = 0; index < chunks.length; index++) length += chunks[index].length;
  const bytes = host.slots.bytes(length);
  let offset = 0;
  for (let index = 0; index < chunks.length; index++) {
    const chunk = chunks[index];
    for (let byte = 0; byte < chunk.length; byte++) bytes[offset + byte] = chunk[byte];
    offset += chunk.length;
  }
  return bytes;
};
globalThis.Blob = class Blob {
  constructor(blobParts, options) {
    const converted = __tbConvertBlobParts(blobParts);
    let endings = 'transparent';
    let type = '';
    if (options !== undefined && options !== null) {
      if (typeof options !== 'object' && typeof options !== 'function') {
        throw new TypeError('The options argument must be a property bag');
      }
      endings = __tbEndings(options.endings);
      type = __tbBlobType(options.type);
    }
    const bytes = __tbBlobBytes(converted, endings);
    __tbBlobData.set(this, { bytes, type });
  }
  get size() { return __tbBrand(this, __tbBlobData).bytes.length; }
  get type() { return __tbBrand(this, __tbBlobData).type; }
  // https://w3c.github.io/FileAPI/#dom-blob-text
  text() {
    const bytes = __tbBrand(this, __tbBlobData).bytes;
    return Promise.resolve(__tbDecodeBytes(bytes, 'utf-8', false, false));
  }
  // https://w3c.github.io/FileAPI/#dom-blob-arraybuffer
  arrayBuffer() {
    const bytes = __tbBrand(this, __tbBlobData).bytes;
    return Promise.resolve(__tbBytesCopy(bytes).buffer);
  }
  // https://w3c.github.io/FileAPI/#dom-blob-bytes
  bytes() {
    const bytes = __tbBrand(this, __tbBlobData).bytes;
    return Promise.resolve(__tbBytesCopy(bytes));
  }
  // https://w3c.github.io/FileAPI/#dom-blob-slice
  slice(start, end, contentType) {
    const data = __tbBrand(this, __tbBlobData);
    const size = data.bytes.length;
    const hasStart = arguments.length > 0 && start !== undefined;
    const hasEnd = arguments.length > 1 && end !== undefined;
    const hasContentType = arguments.length > 2 && contentType !== undefined;
    let relativeStart = 0;
    if (hasStart) {
      relativeStart = __tbClampRound(start);
      relativeStart = relativeStart < 0 ? Math.max(size + relativeStart, 0) : Math.min(relativeStart, size);
    }
    let relativeEnd = size;
    if (hasEnd) {
      relativeEnd = __tbClampRound(end);
      relativeEnd = relativeEnd < 0 ? Math.max(size + relativeEnd, 0) : Math.min(relativeEnd, size);
    }
    const span = Math.max(relativeEnd - relativeStart, 0);
    const type = hasContentType ? __tbBlobType(contentType) : '';
    return new Blob([__tbBytesCopy(data.bytes, relativeStart, relativeStart + span)], { type });
  }
  // https://w3c.github.io/FileAPI/#dom-blob-stream
  stream() {
    const bytes = __tbBytesCopy(__tbBrand(this, __tbBlobData).bytes);
    return new ReadableStream({
      start(controller) {
        if (bytes.length > 0) controller.enqueue(bytes);
        controller.close();
      },
    });
  }
  // https://w3c.github.io/FileAPI/#dom-blob-textstream
  textStream() {
    const text = __tbDecodeBytes(__tbBrand(this, __tbBlobData).bytes, 'utf-8', false, false);
    return new ReadableStream({
      start(controller) {
        if (text.length > 0) controller.enqueue(text);
        controller.close();
      },
    });
  }
};
Object.defineProperty(globalThis.Blob.prototype, Symbol.toStringTag, { value: 'Blob', writable: false, enumerable: false, configurable: true });
// A Blob whose type is set directly, without the constructor's ASCII filter
// and lowercase: `body.blob()` stores the MIME type extracted from the
// header list (<https://fetch.spec.whatwg.org/#dom-body-blob>). Captured at
// realm init so a page replacing `Blob` cannot reroute it.
const __tbBlobPrototype = globalThis.Blob.prototype;
const __tbMakeBlob = (bytes, type) => {
  const blob = Object.create(__tbBlobPrototype);
  __tbBlobData.set(blob, { bytes, type });
  return blob;
};
// Web IDL constructor objects: `length` is the number of required arguments
// (<https://webidl.spec.whatwg.org/#es-interface>).
Object.defineProperty(globalThis.Blob, 'length', { value: 0, writable: false, enumerable: false, configurable: true });
// https://w3c.github.io/FileAPI/#file-section
globalThis.File = class File extends Blob {
  constructor(fileBits, fileName, options) {
    if (arguments.length < 2) throw new TypeError('The File constructor requires 2 arguments');
    // fileBits converts before the options dictionary, and FilePropertyBag
    // members evaluate in lexicographic order: endings, lastModified, type.
    const converted = __tbConvertBlobParts(fileBits);
    const name = String(fileName);
    let endings = 'transparent';
    let type = '';
    let lastModified = Date.now();
    if (options !== undefined && options !== null) {
      if (typeof options !== 'object' && typeof options !== 'function') {
        throw new TypeError('The options argument must be a property bag');
      }
      endings = __tbEndings(options.endings);
      if (options.lastModified !== undefined) {
        // A long long: ToInt64 maps NaN and infinities to 0
        // (<https://webidl.spec.whatwg.org/#abstract-opdef-converttoint>).
        lastModified = Number(options.lastModified);
        lastModified = Number.isFinite(lastModified) ? Math.trunc(lastModified) : 0;
      }
      type = __tbBlobType(options.type);
    }
    super([__tbBlobBytes(converted, endings)], { endings, type });
    __tbFileData.set(this, { name, lastModified });
  }
  get name() { return __tbBrand(this, __tbFileData).name; }
  get lastModified() { return __tbBrand(this, __tbFileData).lastModified; }
  get webkitRelativePath() { __tbBrand(this, __tbFileData); return ''; }
};
Object.defineProperty(globalThis.File.prototype, Symbol.toStringTag, { value: 'File', writable: false, enumerable: false, configurable: true });
Object.defineProperty(globalThis.File, 'length', { value: 2, writable: false, enumerable: false, configurable: true });
// https://w3c.github.io/FileAPI/#filelist-section
globalThis.FileList = class FileList {
  constructor() { throw new TypeError('Illegal constructor'); }
  get length() { return __tbBrand(this, __tbFileListData).files.length; }
  item(index) {
    const files = __tbBrand(this, __tbFileListData).files;
    index = Number(index);
    return index >= 0 && index < files.length ? files[index] : null;
  }
  [Symbol.iterator]() {
    return __tbArray.iterator(__tbBrand(this, __tbFileListData).files);
  }
};
Object.defineProperty(globalThis.FileList.prototype, Symbol.toStringTag, { value: 'FileList', writable: false, enumerable: false, configurable: true });
// The prototype is captured once: structured deserialization still builds a
// FileList after a page deletes the global interface
// (<https://html.spec.whatwg.org/multipage/structured-data.html#structureddeserialize>).
const __tbFileListPrototype = globalThis.FileList.prototype;
host.__tbCreateFileList = files => {
  const list = __tbObjectCreate(__tbFileListPrototype);
  const items = __tbArray.map(files, file => file);
  __tbFileListData.set(list, { files: items });
  // The indexed getter (<https://webidl.spec.whatwg.org/#dfn-indexed-property-getter>):
  // a FileList is fixed-size, so each index is an own property.
  for (let index = 0; index < items.length; index++) {
    __tbDefineProperty(list, index, {
      __proto__: null,
      value: items[index], writable: false, enumerable: true, configurable: true,
    });
  }
  return list;
};
// https://w3c.github.io/FileAPI/#APIASynch
globalThis.FileReader = class FileReader extends EventTarget {
  constructor() {
    super();
    __tbReaderData.set(this, { state: 0, result: null, error: null, generation: 0, handlers: { __proto__: null } });
  }
  get readyState() { return __tbBrand(this, __tbReaderData).state; }
  get result() { return __tbBrand(this, __tbReaderData).result; }
  get error() { return __tbBrand(this, __tbReaderData).error; }
  // https://w3c.github.io/FileAPI/#dfn-abort
  abort() {
    const data = __tbBrand(this, __tbReaderData);
    if (data.state !== 1) return;
    data.state = 2;
    data.result = null;
    const generation = ++data.generation;
    this.dispatchEvent(new ProgressEvent('abort'));
    // A read started by the abort handler suppresses the old loadend
    // (<https://w3c.github.io/FileAPI/#fileReaderAbort>).
    if (data.generation === generation) {
      this.dispatchEvent(new ProgressEvent('loadend'));
    }
  }
  readAsText(blob, encoding) { __tbFileReaderRead(this, blob, 'text', encoding); }
  readAsArrayBuffer(blob) { __tbFileReaderRead(this, blob, 'arraybuffer', undefined); }
  readAsDataURL(blob) { __tbFileReaderRead(this, blob, 'dataurl', undefined); }
  readAsBinaryString(blob) { __tbFileReaderRead(this, blob, 'binarystring', undefined); }
};
for (const [name, value] of [['EMPTY', 0], ['LOADING', 1], ['DONE', 2]]) {
  for (const target of [globalThis.FileReader, globalThis.FileReader.prototype]) {
    Object.defineProperty(target, name, { value, writable: false, enumerable: true, configurable: false });
  }
}
for (const name of ['onloadstart', 'onprogress', 'onload', 'onabort', 'onerror', 'onloadend']) {
  Object.defineProperty(globalThis.FileReader.prototype, name, {
    get() {
      const handler = __tbBrand(this, __tbReaderData).handlers[name];
      return handler === undefined ? null : handler;
    },
    set(value) { __tbBrand(this, __tbReaderData).handlers[name] = value; },
    enumerable: true, configurable: true,
  });
}
const __tbSniffBOM = bytes => {
  if (bytes.length >= 3 && bytes[0] === 0xef && bytes[1] === 0xbb && bytes[2] === 0xbf) return 'utf-8';
  if (bytes.length >= 2 && bytes[0] === 0xfe && bytes[1] === 0xff) return 'utf-16be';
  if (bytes.length >= 2 && bytes[0] === 0xff && bytes[1] === 0xfe) return 'utf-16le';
  return null;
};
const __tbFileReaderRead = (reader, blob, kind, argument) => {
  const data = __tbBrand(reader, __tbReaderData);
  if (data.state === 1) {
    throw new DOMException('The FileReader is already reading.', 'InvalidStateError');
  }
  const source = __tbBrand(blob, __tbBlobData);
  data.state = 1;
  data.result = null;
  data.error = null;
  const generation = ++data.generation;
  const total = source.bytes.length;
  const fire = (type, loaded, lengthComputable) => reader.dispatchEvent(new ProgressEvent(type, {
    lengthComputable, loaded, total,
  }));
  const stale = () => data.generation !== generation;
  // Every event runs in its own task, so awaiting tests observe the order the
  // spec queues and abort() can land between steps
  // (<https://w3c.github.io/FileAPI/#readOperation>).
  host.setTimeout(function() {
    if (stale()) return;
    fire('loadstart', 0, true);
    host.setTimeout(function() {
      if (stale()) return;
      if (total > 0) fire('progress', total, true);
      host.setTimeout(function() {
        if (stale()) return;
        let result = null;
        let error = null;
        try {
          if (kind === 'text') {
            // Encoding: explicit argument, else the blob type's charset, else
            // UTF-8; a BOM overrides (<https://w3c.github.io/FileAPI/#readAsDataText>).
            let encoding;
            if (argument !== undefined) {
              const label = __tbEncoding(__tbIDLString(argument));
              if (label !== null) encoding = label;
            }
            if (encoding === undefined) {
              const charset = /charset=['\u0022]?([^;\u0022\s]+)/i.exec(source.type);
              if (charset) {
                const label = __tbEncoding(charset[1]);
                if (label !== null) encoding = label;
              }
            }
            if (encoding === undefined) encoding = 'utf-8';
            const bom = __tbSniffBOM(source.bytes);
            result = __tbDecodeBytes(source.bytes, bom === null ? encoding : bom, false, false);
          } else if (kind === 'dataurl') {
            const type = source.type === '' ? 'application/octet-stream' : source.type;
            result = 'data:' + type + ';base64,' + __tbBase64Encode(source.bytes);
          } else if (kind === 'arraybuffer') {
            result = __tbBytesCopy(source.bytes).buffer;
          } else {
            result = __tbToLatin1(source.bytes);
          }
        } catch (exception) {
          error = exception;
        }
        if (stale()) return;
        data.state = 2;
        if (error !== null) {
          data.error = error;
          fire('error', 0, false);
        } else {
          data.result = result;
          fire('load', total, true);
        }
        host.setTimeout(function() {
          if (stale()) return;
          fire('loadend', total, true);
        }, 0);
      }, 0);
    }, 0);
  }, 0);
};
Object.defineProperty(globalThis.FileReader.prototype, Symbol.toStringTag, { value: 'FileReader', writable: false, enumerable: false, configurable: true });
// https://w3c.github.io/FileAPI/#dfn-createObjectURL
// WebIDL USVString conversion: unpaired surrogates become U+FFFD
// (<https://webidl.spec.whatwg.org/#idl-USVString>). Values that cross into
// Rust have to be valid UTF-8, and a URL parser sees replacement characters
// anyway.

// `DataTransfer` only as far as tests need it: `items.add(file)` collects
// files and `files` exposes them as a FileList
// (<https://html.spec.whatwg.org/multipage/dnd.html#datatransfer>).
const __tbDataTransferFiles = host.slots('tb-data-transfer-files');
const __tbDataTransferItems = host.slots('tb-data-transfer-items');
const __tbDataTransferFileList = host.slots('tb-data-transfer-file-list');
globalThis.DataTransfer = class DataTransfer {
  constructor() {
    __tbDataTransferFiles.set(this, []);
  }
  get items() {
    let items = __tbDataTransferItems.get(this);
    if (items === undefined) {
      const files = __tbDataTransferFiles.get(this);
      items = {
        add(file) { __tbArray.push(files, file); return null; },
      };
      Object.defineProperty(items, 'length', {
        get() { return files.length; }, enumerable: true, configurable: true,
      });
      __tbDataTransferItems.set(this, items);
    }
    return items;
  }
  get files() {
    // `files` is a `[SameObject]` FileList over the DataTransfer's items
    // (<https://html.spec.whatwg.org/multipage/dnd.html#dom-datatransfer-files>).
    if (__tbDataTransferFileList.get(this) === undefined) {
      __tbDataTransferFileList.set(this, host.__tbCreateFileList(__tbDataTransferFiles.get(this)));
    }
    __tbBrand(__tbDataTransferFileList.get(this), __tbFileListData).files = __tbDataTransferFiles.get(this);
    return __tbDataTransferFileList.get(this);
  }
};
Object.defineProperty(globalThis.DataTransfer.prototype, Symbol.toStringTag, {
  value: 'DataTransfer', writable: false, enumerable: false, configurable: true,
});

// A `type=file` input with no files still contributes an entry, an empty File
// (<https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#constructing-form-data-set>).
host.__tbEmptyFile = () => new globalThis.File([], '');

// https://html.spec.whatwg.org/multipage/imagebitmap-and-animations.html#dom-createimagebitmap
globalThis.createImageBitmap = function createImageBitmap(source, options) {
  return new Promise((resolve, reject) => {
    try {
      if (source !== null && typeof source === 'object' && __tbBlobData.get(source) !== undefined) {
        source.arrayBuffer().then(
          buffer => {
            try {
              resolve(host.createImageBitmapDecode(new __tbUint8Array(buffer), options));
            } catch (error) {
              reject(error);
            }
          },
          reject);
        return;
      }
      resolve(host.createImageBitmapFromSource(source, options));
    } catch (error) {
      reject(error);
    }
  });
};
