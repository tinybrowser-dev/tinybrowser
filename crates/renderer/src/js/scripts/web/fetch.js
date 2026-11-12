// https://fetch.spec.whatwg.org/#headers-class
// Page requests share the browser process's HTTP client with navigation.
const __tbHeadersData = host.slots('tinybrowser.headers.data');
const __tbHeadersIteratorState = host.slots('tinybrowser.headers.iterator');
const __tbResponseData = host.slots('tinybrowser.response.data');
const __tbRequestData = host.slots('tinybrowser.request.data');
// WebIDL ByteString conversion: one code point per byte, and anything above
// 0xFF is a TypeError before any header algorithm runs
// (<https://webidl.spec.whatwg.org/#es-ByteString>).
const __tbByteString = value => {
  const text = __tbToDOMString(value);
  for (let index = 0; index < text.length; index++) {
    if (text.charCodeAt(index) > 0xFF) throw new TypeError('Value is not a byte string');
  }
  return text;
};
// WebIDL USVString conversion: lone surrogates become U+FFFD before the value
// reaches Rust, whose `String` cannot carry them
// (<https://webidl.spec.whatwg.org/#es-USVString>).
const __tbToUSVString = value => {
  const text = __tbToDOMString(value);
  let result = '';
  for (let index = 0; index < text.length; index++) {
    const code = text.charCodeAt(index);
    if (code >= 0xD800 && code <= 0xDBFF) {
      const next = index + 1 < text.length ? text.charCodeAt(index + 1) : 0;
      if (next >= 0xDC00 && next <= 0xDFFF) {
        result += text[index] + text[index + 1];
        index += 1;
      } else {
        result += '\uFFFD';
      }
    } else if (code >= 0xDC00 && code <= 0xDFFF) {
      result += '\uFFFD';
    } else {
      result += text[index];
    }
  }
  return result;
};
const __tbIsHeaderName = value => /^[!#$%&'*+.^_`|~0-9A-Za-z-]+$/.test(value);
const __tbHeaderName = value => {
  const name = __tbByteString(value);
  if (!__tbIsHeaderName(name)) throw new TypeError('Invalid header name');
  return name.toLowerCase();
};
const __tbIsHeaderValue = value => {
  for (let index = 0; index < value.length; index++) {
    const code = value.charCodeAt(index);
    if (code === 0x00 || code === 0x0A || code === 0x0D || code > 0xFF) return false;
  }
  return true;
};
const __tbHeaderValue = value => {
  const text = __tbByteString(value).replace(/^[\t\n\r ]+|[\t\n\r ]+$/g, '');
  if (!__tbIsHeaderValue(text)) throw new TypeError('Invalid header value');
  return text;
};
// HTTP whitespace is SP and HTAB; JS `trim()` also strips NBSP and BOM.
const __tbHttpTrim = value => value.replace(/^[\t ]+|[\t ]+$/g, '');
// The policy tables live in `net::cors`; the renderer only applies the host
// predicates, so JS and the preflight decision cannot drift.
const __tbHeadersRecord = headers => __tbBrand(headers, __tbHeadersData);
const __tbHeadersList = headers => __tbHeadersRecord(headers).list;
const __tbHeadersGuardOf = headers => __tbHeadersRecord(headers).guard;
const __tbHeadersTouch = record => {
  record.version += 1;
  record.sorted = null;
};
const __tbHeadersLookupRaw = (headers, name) => {
  const entries = __tbHeadersList(headers);
  const values = [];
  for (let index = 0; index < entries.length; index++) {
    if (entries[index][0] === name) __tbArray.push(values, entries[index][1]);
  }
  return values.length === 0 ? null : __tbArray.join(values, ', ');
};
// Sorted-and-combined view the iterator and `forEach` walk: names sort
// alphabetically, duplicate values combine with ", ", and every set-cookie
// entry stays separate in insertion order
// (<https://fetch.spec.whatwg.org/#concept-header-list-sort-and-combine>).
const __tbSortedHeaders = headers => {
  const record = __tbHeadersRecord(headers);
  if (record.sorted !== null) return record.sorted;
  const entries = record.list;
  const list = [];
  const indexByName = Object.create(null);
  for (let index = 0; index < entries.length; index++) {
    const name = entries[index][0];
    const value = entries[index][1];
    if (name === 'set-cookie') {
      __tbArray.push(list, [name, value]);
      continue;
    }
    const at = indexByName[name];
    if (at === undefined) {
      indexByName[name] = list.length;
      __tbArray.push(list, [name, value]);
    } else {
      list[at][1] = list[at][1] + ', ' + value;
    }
  }
  __tbArray.sort(list, (a, b) => a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0);
  record.sorted = list;
  return list;
};
const __tbHeadersIteratorProto = Object.create(Object.getPrototypeOf(Object.getPrototypeOf([][Symbol.iterator]())));
Object.defineProperty(__tbHeadersIteratorProto, Symbol.toStringTag, {
  value: 'Headers Iterator', writable: false, enumerable: false, configurable: true,
});
Object.defineProperty(__tbHeadersIteratorProto, 'next', {
  value: function() {
    const state = __tbHeadersIteratorState.get(this);
    if (state === undefined) throw new TypeError('Illegal invocation');
    const list = __tbSortedHeaders(state.headers);
    if (state.index >= list.length) return { value: undefined, done: true };
    const entry = list[state.index++];
    return { value: state.kind === 'entries' ? [entry[0], entry[1]] : state.kind === 'keys' ? entry[0] : entry[1], done: false };
  },
  writable: true, enumerable: true, configurable: true,
});
const __tbHeadersIterator = (headers, kind) => {
  const iterator = Object.create(__tbHeadersIteratorProto);
  __tbHeadersIteratorState.set(iterator, { headers, kind, index: 0 });
  return iterator;
};
const __tbHeadersAllowed = (record, name, value, existing) => {
  switch (record.guard) {
    case 'none':
      return true;
    case 'immutable':
      throw new TypeError('Headers are immutable');
    case 'response':
      return name !== 'set-cookie' && name !== 'set-cookie2';
    case 'request':
    case 'request-no-cors':
      if (host.__tbIsForbiddenRequestHeader(name, value)) return false;
      if (record.guard === 'request') return true;
      return host.__tbIsNoCorsSafelistedHeader(
        name,
        existing === null ? value : existing + ', ' + value,
      );
    default:
      throw new TypeError('Invalid headers guard');
  }
};
const __tbHeadersAppendRaw = (headers, name, value) => {
  const record = __tbHeadersRecord(headers);
  const header = [__tbHeaderName(name), __tbHeaderValue(value)];
  if (!__tbHeadersAllowed(record, header[0], header[1], __tbHeadersLookupRaw(headers, header[0]))) return;
  __tbArray.push(record.list, header);
  __tbHeadersTouch(record);
};
const __tbHeadersDeleteRaw = (headers, name) => {
  const record = __tbHeadersRecord(headers);
  if (record.guard === 'immutable') throw new TypeError('Headers are immutable');
  const normalized = __tbHeaderName(name);
  if ((record.guard === 'request' || record.guard === 'request-no-cors')
      && host.__tbIsForbiddenRequestHeader(normalized, '')) {
    return;
  }
  for (let index = record.list.length - 1; index >= 0; index--) {
    if (record.list[index][0] === normalized) __tbArray.remove(record.list, index);
  }
  __tbHeadersTouch(record);
};
const __tbHeadersSetRaw = (headers, name, value) => {
  const record = __tbHeadersRecord(headers);
  if (record.guard === 'immutable') throw new TypeError('Headers are immutable');
  const header = [__tbHeaderName(name), __tbHeaderValue(value)];
  if (!__tbHeadersAllowed(record, header[0], header[1], null)) return;
  for (let index = record.list.length - 1; index >= 0; index--) {
    if (record.list[index][0] === header[0]) __tbArray.remove(record.list, index);
  }
  __tbArray.push(record.list, header);
  __tbHeadersTouch(record);
};
const __tbHeadersGetRaw = (headers, name) => {
  const normalized = __tbHeaderName(name);
  return __tbHeadersLookupRaw(headers, normalized);
};
const __tbFillHeaders = (headers, init) => {
  if (init === undefined) return;
  if (init === null || (typeof init !== 'object' && typeof init !== 'function')) {
    throw new TypeError('The Headers init must be a sequence or a record');
  }
  const iteratorMethod = init[Symbol.iterator];
  if (iteratorMethod !== undefined && iteratorMethod !== null) {
    if (typeof iteratorMethod !== 'function') throw new TypeError('The Headers init must be iterable');
    const iterator = __tbApply(iteratorMethod, init, []);
    while (true) {
      const step = iterator.next();
      if (step.done) break;
      if (step.value === null || typeof step.value !== 'object') throw new TypeError('Header pairs must be objects');
      const values = __tbArrayFrom(step.value);
      if (values.length !== 2) throw new TypeError('Header pairs must contain two values');
      __tbHeadersAppendRaw(headers, values[0], values[1]);
    }
  } else {
    // WebIDL record conversion: ownKeys, then per-key
    // getOwnPropertyDescriptor and (for enumerable keys) get, with the name
    // converted to a ByteString before the value is read
    // (<https://webidl.spec.whatwg.org/#es-to-record>).
    const keys = Reflect.ownKeys(init);
    for (let index = 0; index < keys.length; index++) {
      const key = keys[index];
      const descriptor = __tbGetOwnPropertyDescriptor(init, key);
      if (descriptor === undefined || !descriptor.enumerable) continue;
      const name = __tbHeaderName(key);
      __tbHeadersAppendRaw(headers, name, init[key]);
    }
  }
};
// https://fetch.spec.whatwg.org/#concept-method-normalize
const __tbRequestMethod = value => {
  const method = String(value);
  if (!/^[!#$%&'*+.^_`|~0-9A-Za-z-]+$/.test(method) || /^(CONNECT|TRACE|TRACK)$/i.test(method)) {
    throw new TypeError('Invalid HTTP method');
  }
  return /^(DELETE|GET|HEAD|OPTIONS|POST|PUT)$/i.test(method) ? method.toUpperCase() : method;
};
// Request mode, credentials mode, and redirect mode
// (<https://fetch.spec.whatwg.org/#request-class>).
const __tbRequestMode = value => {
  const mode = String(value);
  if (mode === 'same-origin' || mode === 'cors' || mode === 'no-cors') return mode;
  throw new TypeError('Invalid request mode');
};
const __tbCredentialsMode = value => {
  const mode = String(value);
  if (mode === 'omit' || mode === 'same-origin' || mode === 'include') return mode;
  throw new TypeError('Invalid credentials mode');
};
const __tbRedirectMode = value => {
  const mode = String(value);
  if (mode === 'follow' || mode === 'error' || mode === 'manual') return mode;
  throw new TypeError('Invalid redirect mode');
};
// A `no-cors` request may use only a CORS-safelisted method
// (<https://fetch.spec.whatwg.org/#cors-safelisted-method>). `net` owns the
// method table.
const __tbCorsSafelistedMethod = method => host.__tbIsCorsSafelistedMethod(method);
const __tbBrand = (value, slots, message) => {
  const data = slots.get(value);
  if (data === undefined) throw new TypeError(message === undefined ? 'Illegal invocation' : message);
  return data;
};
globalThis.Headers = class Headers {
  constructor(init) {
    __tbHeadersData.set(this, { list: [], guard: 'none', version: 0, sorted: null });
    __tbFillHeaders(this, init);
  }
  append(name, value) { __tbHeadersAppendRaw(this, name, value); }
  delete(name) { __tbHeadersDeleteRaw(this, name); }
  get(name) { return __tbHeadersGetRaw(this, name); }
  getSetCookie() {
    const entries = __tbHeadersList(this);
    const values = [];
    for (let index = 0; index < entries.length; index++) {
      if (entries[index][0] === 'set-cookie') __tbArray.push(values, entries[index][1]);
    }
    return values;
  }
  has(name) { return __tbHeadersGetRaw(this, name) !== null; }
  set(name, value) { __tbHeadersSetRaw(this, name, value); }
  forEach(callback, thisArg) {
    if (typeof callback !== 'function') throw new TypeError('callback is not a function');
    const list = __tbSortedHeaders(this);
    for (let index = 0; index < list.length; index++) {
      __tbApply(callback, thisArg, [list[index][1], list[index][0], this]);
    }
  }
  entries() { return __tbHeadersIterator(this, 'entries'); }
  keys() { return __tbHeadersIterator(this, 'keys'); }
  values() { return __tbHeadersIterator(this, 'values'); }
  [Symbol.iterator]() { return this.entries(); }
};
Object.defineProperty(globalThis.Headers.prototype, Symbol.toStringTag, { value: 'Headers', writable: false, enumerable: false, configurable: true });
const __tbHeadersConstructor = globalThis.Headers;
const __tbHeadersGet = __tbHeadersConstructor.prototype.get;
const __tbHeadersAppend = __tbHeadersConstructor.prototype.append;
// Internal construction with a guard; page code only sees the default guard
// (<https://fetch.spec.whatwg.org/#concept-headers-guard>).
const __tbMakeHeaders = (entries, guard) => {
  const headers = Object.create(__tbHeadersConstructor.prototype);
  __tbHeadersData.set(headers, { list: entries, guard, version: 0, sorted: null });
  return headers;
};
const __tbNewHeaders = (init, guard) => {
  const headers = Object.create(__tbHeadersConstructor.prototype);
  __tbHeadersData.set(headers, { list: [], guard, version: 0, sorted: null });
  __tbFillHeaders(headers, init);
  return headers;
};
// A BodyInit becomes bytes plus a type, or a ReadableStream when the caller
// supplied one (<https://fetch.spec.whatwg.org/#bodyinit>). The bytes stay in
// memory: every consumer in this engine reads the whole body anyway.
const __tbConcatBytes = chunks => {
  let length = 0;
  for (let index = 0; index < chunks.length; index++) length += chunks[index].length;
  const bytes = new __tbUint8Array(length);
  let offset = 0;
  for (let index = 0; index < chunks.length; index++) {
    const chunk = chunks[index];
    for (let byte = 0; byte < chunk.length; byte++) bytes[offset + byte] = chunk[byte];
    offset += chunk.length;
  }
  return bytes;
};
const __tbNormalizeLineEndings = value => String(value).replace(/\r\n|\r|\n/g, '\r\n');
const __tbMultipartEscape = value =>
  __tbNormalizeLineEndings(value).replace(/\r/g, '%0D').replace(/\n/g, '%0A').replace(/"/g, '%22');
const __tbMultipartBoundary = () => {
  const bytes = new Uint8Array(16);
  if (typeof crypto !== 'undefined' && typeof crypto.getRandomValues === 'function') {
    crypto.getRandomValues(bytes);
  } else {
    for (let index = 0; index < bytes.length; index++) bytes[index] = Math.floor(Math.random() * 256);
  }
  let boundary = '----tinybrowser';
  for (let index = 0; index < bytes.length; index++) boundary += bytes[index].toString(16).padStart(2, '0');
  return boundary;
};
// <https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#multipart-form-data>
const __tbFormDataBody = formData => {
  const boundary = __tbMultipartBoundary();
  const type = 'multipart/form-data; boundary=' + boundary;
  const chunks = [];
  for (const entry of formData) {
    const name = entry[0];
    const value = entry[1];
    let head = '--' + boundary + '\r\nContent-Disposition: form-data; name="' + __tbMultipartEscape(name) + '"';
    let fileBytes = null;
    if (value instanceof Blob) {
      head += '; filename="' + __tbMultipartEscape(value.name === undefined ? 'blob' : value.name) + '"\r\n';
      head += 'Content-Type: ' + (value.type === '' ? 'application/octet-stream' : value.type) + '\r\n';
      fileBytes = __tbBytesCopy(__tbBlobData.get(value).bytes);
    } else {
      head += '\r\n';
    }
    head += '\r\n';
    __tbArray.push(chunks, __tbUtf8Encode(head));
    __tbArray.push(chunks, fileBytes === null ? __tbUtf8Encode(__tbNormalizeLineEndings(String(value))) : fileBytes);
    __tbArray.push(chunks, __tbUtf8Encode('\r\n'));
  }
  __tbArray.push(chunks, __tbUtf8Encode('--' + boundary + '--\r\n'));
  return { bytes: __tbConcatBytes(chunks), type };
};
const __tbExtractBody = body => {
  if (body === null || body === undefined) return { bytes: null, type: null, stream: null };
  if (body instanceof ReadableStream) {
    if (body.locked || __tbStreamDisturbed(body)) throw new TypeError('Stream is locked or disturbed');
    return { bytes: null, type: null, stream: body };
  }
  if (body instanceof Blob) {
    return { bytes: __tbBytesCopy(__tbBlobData.get(body).bytes), type: body.type === '' ? null : body.type, stream: null };
  }
  if (body instanceof URLSearchParams) {
    return { bytes: __tbUtf8Encode(String(body)), type: 'application/x-www-form-urlencoded;charset=UTF-8', stream: null };
  }
  if (body instanceof FormData) {
    const encoded = __tbFormDataBody(body);
    return { bytes: encoded.bytes, type: encoded.type, stream: null };
  }
  if (body instanceof ArrayBuffer) return { bytes: __tbBytesCopy(new __tbUint8Array(body)), type: null, stream: null };
  if (ArrayBuffer.isView(body)) {
    return { bytes: __tbBytesCopy(new __tbUint8Array(body.buffer, body.byteOffset, body.byteLength)), type: null, stream: null };
  }
  return { bytes: __tbUtf8Encode(__tbToDOMString(body)), type: 'text/plain;charset=UTF-8', stream: null };
};
// Blob types are ASCII-lowercased MIME type strings
// (<https://w3c.github.io/FileAPI/#dfn-type>).
const __tbBlobTypeFromHeader = header =>
  header === null ? '' : header.toLowerCase();
const __tbBodyStream = extracted => {
  if (extracted.stream !== null) return extracted.stream;
  if (extracted.bytes === null) return null;
  return new ReadableStream({
    type: 'bytes',
    start(controller) {
      controller.enqueue(extracted.bytes);
      controller.close();
    },
  });
};
const __tbBodyUsed = data =>
  data.bodyUsed || (data.stream !== null && __tbStreamDisturbed(data.stream));
// A null-prototype result keeps `Object.prototype.then` from turning the
// bytes into a thenable while the promise chain is still internal.
const __tbReadResult = bytes => {
  const result = __tbObjectCreate(null);
  result.bytes = bytes;
  return result;
};
const __tbConsumeBody = data => {
  if (data.bodySignal !== undefined && data.bodySignal !== null && data.bodySignal.aborted) {
    return Promise.reject(data.bodySignal.reason);
  }
  if (data.stream === null) return Promise.resolve(__tbReadResult(new __tbUint8Array(0)));
  if (data.stream.locked || __tbBodyUsed(data)) return Promise.reject(new TypeError('Body already used'));
  data.bodyUsed = true;
  const stream = data.stream;
  const markDisturbed = () => { __tbStreamBrand(stream).disturbed = true; };
  // The fast path drains a closed stream without taking a reader; mark the
  // stream disturbed so a later `getReader()` still throws
  // (<https://streams.spec.whatwg.org/#is-readable-stream-disturbed>).
  return __tbReadAllBytes(stream).then(
    result => { markDisturbed(); return result; },
    error => { markDisturbed(); throw error; },
  );
};
const __tbBodyData = (receiver, slots) => __tbBrand(receiver, slots);
const __tbInstallBody = (proto, slots) => {
  Object.defineProperties(proto, {
    body: {
      get() { return __tbBodyData(this, slots).stream; },
      configurable: true,
    },
    bodyUsed: {
      get() { return __tbBodyUsed(__tbBodyData(this, slots)); },
      configurable: true,
    },
    textStream: {
      value() {
        const data = __tbBodyData(this, slots);
        if (__tbBodyUsed(data)) throw new TypeError('Body already used');
        const source = data.stream;
        if (source === null) {
          return new ReadableStream({ start(controller) { controller.close(); } });
        }
        data.bodyUsed = true;
        const reader = source.getReader();
        const decoder = new TextDecoder();
        return new ReadableStream({
          async pull(controller) {
            const step = await reader.read();
            if (step.done) {
              const tail = decoder.decode();
              if (tail !== '') controller.enqueue(tail);
              controller.close();
              return;
            }
            const text = decoder.decode(step.value, { stream: true });
            if (text !== '') controller.enqueue(text);
          },
          cancel(reason) { return reader.cancel(reason); },
        });
      },
      writable: true, configurable: true,
    },
    text: {
      value() {
        const data = __tbBodyData(this, slots);
        if (data.bodySignal !== undefined && data.bodySignal !== null && data.bodySignal.aborted) {
          return Promise.reject(data.bodySignal.reason);
        }
        return __tbConsumeBody(data)
          .then(result => __tbDecodeBytes(result.bytes, 'utf-8', false, false));
      },
      writable: true, configurable: true,
    },
    json: {
      value() {
        const data = __tbBodyData(this, slots);
        if (data.bodySignal !== undefined && data.bodySignal !== null && data.bodySignal.aborted) {
          return Promise.reject(data.bodySignal.reason);
        }
        return __tbConsumeBody(data)
          .then(result => JSON.parse(__tbDecodeBytes(result.bytes, 'utf-8', false, false)));
      },
      writable: true, configurable: true,
    },
    arrayBuffer: {
      value() {
        const data = __tbBodyData(this, slots);
        if (data.bodySignal !== undefined && data.bodySignal !== null && data.bodySignal.aborted) {
          return Promise.reject(data.bodySignal.reason);
        }
        return __tbConsumeBody(data)
          .then(result => __tbBytesCopy(result.bytes).buffer);
      },
      writable: true, configurable: true,
    },
    bytes: {
      value() {
        const data = __tbBodyData(this, slots);
        if (data.bodySignal !== undefined && data.bodySignal !== null && data.bodySignal.aborted) {
          return Promise.reject(data.bodySignal.reason);
        }
        return __tbConsumeBody(data)
          .then(result => __tbBytesCopy(result.bytes));
      },
      writable: true, configurable: true,
    },
    blob: {
      value() {
        const data = __tbBodyData(this, slots);
        if (data.bodySignal !== undefined && data.bodySignal !== null && data.bodySignal.aborted) {
          return Promise.reject(data.bodySignal.reason);
        }
        const type = __tbBlobTypeFromHeader(__tbHeadersLookupRaw(data.headers, 'content-type'));
        return __tbConsumeBody(data).then(result => __tbMakeBlob(result.bytes, type));
      },
      writable: true, configurable: true,
    },
    formData: {
      value() {
        const data = __tbBodyData(this, slots);
        if (data.bodySignal !== undefined && data.bodySignal !== null && data.bodySignal.aborted) {
          return Promise.reject(data.bodySignal.reason);
        }
        const contentType = __tbContentType(data.headers);
        if (data.stream === null) {
          if (contentType !== null && contentType.essence === 'application/x-www-form-urlencoded') {
            return Promise.resolve(new FormData());
          }
          return Promise.reject(new TypeError('Body is not a form'));
        }
        return __tbConsumeBody(data)
          .then(result => __tbFormDataFromBytes(result.bytes, contentType));
      },
      writable: true, configurable: true,
    },
  });
};
// One host MIME parse supplies the essence and the boundary, so a boundary
// containing `=` or `;` and quoted parameters follow the MIME algorithm
// (<https://mimesniff.spec.whatwg.org/#parse-a-mime-type>).
const __tbContentType = headers => {
  const value = __tbHeadersLookupRaw(headers, 'content-type');
  if (value === null) return null;
  const parts = host.__tbMimeTypeParts(value);
  if (parts.length === 0) return null;
  return { essence: parts[0], boundary: parts[1] === '' ? null : parts[1] };
};
const __tbBytesIndexOf = (bytes, needle, from) => {
  for (let index = from; index + needle.length <= bytes.length; index++) {
    let match = true;
    for (let offset = 0; offset < needle.length; offset++) {
      if (bytes[index + offset] !== needle[offset]) { match = false; break; }
    }
    if (match) return index;
  }
  return -1;
};
const __tbAsciiBytes = bytes => {
  let text = '';
  for (let index = 0; index < bytes.length; index++) text += String.fromCharCode(bytes[index]);
  return text;
};
const __tbDispositionParam = (head, key) => {
  const quoted = new RegExp('(?:^|;)\\s*' + key + '="([^"]*)"', 'i').exec(head);
  if (quoted !== null) return quoted[1];
  const bare = new RegExp('(?:^|;)\\s*' + key + '=([^;\\r\\n]*)', 'i').exec(head);
  return bare === null ? null : __tbHttpTrim(bare[1]);
};
const __tbParseMultipart = (bytes, boundary) => {
  const formData = new FormData();
  const delimiter = __tbUtf8Encode('--' + boundary);
  const crlfDelimiter = __tbUtf8Encode('\r\n--' + boundary);
  const separator = __tbUtf8Encode('\r\n\r\n');
  let position = __tbBytesIndexOf(bytes, delimiter, 0);
  if (position === -1) throw new TypeError('Missing multipart boundary');
  position += delimiter.length;
  while (true) {
    // Transport padding between the boundary and the line break is legal
    // (<https://www.rfc-editor.org/rfc/rfc2046#section-5.1.1>).
    while (position < bytes.length && (bytes[position] === 0x20 || bytes[position] === 0x09)) position += 1;
    if (position + 1 < bytes.length && bytes[position] === 0x2D && bytes[position + 1] === 0x2D) {
      position += 2;
      while (position < bytes.length && (bytes[position] === 0x20 || bytes[position] === 0x09)) position += 1;
      if (position < bytes.length && !(bytes[position] === 0x0D && bytes[position + 1] === 0x0A)) {
        throw new TypeError('Malformed multipart terminator');
      }
      break;
    }
    if (position + 1 >= bytes.length || bytes[position] !== 0x0D || bytes[position + 1] !== 0x0A) {
      throw new TypeError('Malformed multipart body');
    }
    position += 2;
    const headerEnd = __tbBytesIndexOf(bytes, separator, position);
    if (headerEnd === -1) throw new TypeError('Malformed multipart headers');
    const head = __tbAsciiBytes(bytes.subarray(position, headerEnd));
    const valueStart = headerEnd + separator.length;
    const next = __tbBytesIndexOf(bytes, crlfDelimiter, valueStart);
    if (next === -1) throw new TypeError('Missing multipart terminator');
    const value = bytes.subarray(valueStart, next);
    const name = __tbDispositionParam(head, 'name');
    if (name !== null) {
      const filename = __tbDispositionParam(head, 'filename');
      if (filename !== null) {
        const typeMatch = /content-type:\s*([^\r\n]+)/i.exec(head);
        const type = typeMatch === null ? 'text/plain' : __tbHttpTrim(typeMatch[1]);
        formData.append(name, new File([value], filename, { type }));
      } else {
        formData.append(name, __tbDecodeBytes(value, 'utf-8', false, true));
      }
    }
    position = next + crlfDelimiter.length;
  }
  return formData;
};
const __tbFormDataFromBytes = (bytes, contentType) => {
  if (bytes.length === 0) return new FormData();
  if (contentType === null) throw new TypeError('Body is not a form');
  if (contentType.essence === 'multipart/form-data') {
    if (contentType.boundary === null) throw new TypeError('Missing multipart boundary');
    return __tbParseMultipart(bytes, contentType.boundary);
  }
  if (contentType.essence === 'application/x-www-form-urlencoded') {
    const formData = new FormData();
    const parameters = new URLSearchParams(__tbDecodeBytes(bytes, 'utf-8', false, false));
    for (const entry of parameters) formData.append(entry[0], entry[1]);
    return formData;
  }
  throw new TypeError('Body is not a form');
};
const __tbToUint16 = value => {
  let number = Number(value);
  if (!Number.isFinite(number)) number = 0;
  number = Math.trunc(number) % 65536;
  return number < 0 ? number + 65536 : number;
};
const __tbStatusText = value => {
  const text = __tbByteString(value === undefined ? '' : value);
  for (let index = 0; index < text.length; index++) {
    const code = text.charCodeAt(index);
    if (code <= 0x08 || code === 0x0A || code === 0x0B || code === 0x0C ||
        (code >= 0x0E && code <= 0x1F) || code === 0x7F) {
      throw new TypeError('Invalid status text');
    }
  }
  return text;
};
const __tbErroredStream = () => new ReadableStream({
  type: 'bytes',
  start(controller) { controller.error(new TypeError('Body decoding failed')); },
});
const __tbResponseRecord = (body, status, statusText, headers) => {
  const extracted = __tbExtractBody(body);
  // The body's type becomes the Content-Type when the header list holds none
  // (<https://fetch.spec.whatwg.org/#dom-response>).
  if (extracted.type !== null && __tbHeadersLookupRaw(headers, 'content-type') === null) {
    __tbHeadersAppendRaw(headers, 'content-type', extracted.type);
  }
  return {
    stream: __tbBodyStream(extracted),
    bodyUsed: false,
    status,
    statusText,
    url: '',
    type: 'default',
    redirected: false,
    headers,
  };
};
const __tbCreateResponse = record => {
  const response = Object.create(globalThis.Response.prototype);
  __tbResponseData.set(response, record);
  return response;
};
const __tbWithSignal = (response, signal) => {
  __tbBrand(response, __tbResponseData).bodySignal = signal;
  return response;
};
// https://fetch.spec.whatwg.org/#response-class
globalThis.Response = class Response {
  constructor(body, init) {
    const options = init === undefined ? {} : Object(init);
    const status = __tbToUint16(options.status === undefined ? 200 : options.status);
    if (status < 200 || status > 599) throw new RangeError('Invalid response status');
    const statusText = __tbStatusText(options.statusText);
    const storedBody = body === undefined || body === null ? null : body;
    // A null body status has no body (<https://fetch.spec.whatwg.org/#null-body-status>).
    if (storedBody !== null && (status === 204 || status === 205 || status === 304)) {
      throw new TypeError('A null body status cannot have a body');
    }
    const headers = __tbNewHeaders(options.headers, 'response');
    __tbResponseData.set(this, __tbResponseRecord(storedBody, status, statusText, headers));
  }
  get status() { return __tbBrand(this, __tbResponseData).status; }
  get statusText() { return __tbBrand(this, __tbResponseData).statusText; }
  get url() { return __tbBrand(this, __tbResponseData).url; }
  get type() { return __tbBrand(this, __tbResponseData).type; }
  get redirected() { return __tbBrand(this, __tbResponseData).redirected; }
  get headers() { return __tbBrand(this, __tbResponseData).headers; }
  get ok() { const status = __tbBrand(this, __tbResponseData).status; return status >= 200 && status <= 299; }
  clone() {
    const data = __tbBrand(this, __tbResponseData);
    if (__tbBodyUsed(data)) throw new TypeError('body already used');
    let stream = null;
    if (data.stream !== null) {
      if (data.stream.locked) throw new TypeError('body is locked');
      const branches = data.stream.tee();
      data.stream = branches[0];
      stream = branches[1];
    }
    const copy = Object.create(globalThis.Response.prototype);
    const headers = __tbMakeHeaders(
      __tbArray.map(__tbHeadersList(data.headers), entry => [entry[0], entry[1]]),
      __tbHeadersGuardOf(data.headers),
    );
    __tbResponseData.set(copy, { ...data, stream, bodyUsed: false, headers });
    return copy;
  }
  // <https://fetch.spec.whatwg.org/#dom-response-json>
  static json(data, init) {
    const options = init === undefined ? {} : Object(init);
    const body = JSON.stringify(data);
    if (body === undefined) throw new TypeError('The data is not JSON serializable');
    const status = __tbToUint16(options.status === undefined ? 200 : options.status);
    if (status < 200 || status > 599) throw new RangeError('Invalid response status');
    const statusText = __tbStatusText(options.statusText);
    if (status === 204 || status === 205 || status === 304) {
      throw new TypeError('A null body status cannot have a body');
    }
    const headers = __tbNewHeaders(options.headers, 'response');
    if (__tbHeadersLookupRaw(headers, 'content-type') === null) {
      __tbHeadersAppendRaw(headers, 'content-type', 'application/json');
    }
    return __tbCreateResponse(__tbResponseRecord(body, status, statusText, headers));
  }
  // <https://fetch.spec.whatwg.org/#dom-response-redirect>
  static redirect(url, status) {
    let resolved;
    try {
      const base = host.__tbResolveUrl('', undefined);
      resolved = new URL(__tbToUSVString(url), base === null ? undefined : base).href;
    } catch (error) {
      throw new TypeError('Invalid URL');
    }
    const code = __tbToUint16(status === undefined ? 302 : status);
    if (code !== 301 && code !== 302 && code !== 303 && code !== 307 && code !== 308) {
      throw new RangeError('Invalid redirect status');
    }
    return __tbCreateResponse({
      stream: null,
      bodyUsed: false,
      status: code,
      statusText: '',
      url: '',
      type: 'default',
      redirected: false,
      headers: __tbMakeHeaders([['location', resolved]], 'immutable'),
    });
  }
  // <https://fetch.spec.whatwg.org/#dom-response-error>
  static error() {
    return __tbCreateResponse({
      stream: null,
      bodyUsed: false,
      status: 0,
      statusText: '',
      url: '',
      type: 'error',
      redirected: false,
      headers: __tbMakeHeaders([], 'immutable'),
    });
  }
};
const __tbChunkBytes = chunk => {
  if (chunk instanceof Uint8Array) return chunk;
  if (ArrayBuffer.isView(chunk)) return new Uint8Array(chunk.buffer, chunk.byteOffset, chunk.byteLength);
  if (chunk instanceof ArrayBuffer) return new Uint8Array(chunk);
  throw new TypeError('A body stream chunk must be a BufferSource');
};
const __tbReadAllBytes = async stream => {
  const drained = __tbDrainClosedStream(stream);
  if (drained !== null) {
    const chunks = [];
    let length = 0;
    for (let index = 0; index < drained.length; index++) {
      const chunk = __tbChunkBytes(drained[index]);
      __tbArray.push(chunks, chunk);
      length += chunk.length;
    }
    const bytes = new Uint8Array(length);
    let offset = 0;
    for (let index = 0; index < chunks.length; index++) {
      bytes.set(chunks[index], offset);
      offset += chunks[index].length;
    }
    return __tbReadResult(bytes);
  }
  const chunks = [];
  let length = 0;
  const reader = stream.getReader();
  while (true) {
    const step = await reader.read();
    if (step.done) break;
    const chunk = __tbChunkBytes(step.value);
    __tbArray.push(chunks, chunk);
    length += chunk.length;
  }
  const bytes = new Uint8Array(length);
  let offset = 0;
  for (let index = 0; index < chunks.length; index++) {
    bytes.set(chunks[index], offset);
    offset += chunks[index].length;
  }
  return __tbReadResult(bytes);
};
Object.defineProperty(globalThis.Response.prototype, Symbol.toStringTag, { value: 'Response', writable: false, enumerable: false, configurable: true });
const __tbLookupObjectUrl = url => {
  const key = url.split('#')[0];
  const contents = host.__tbObjectUrlContents(key);
  if (contents === null || contents === undefined) return null;
  const content_type = host.__tbObjectUrlType(key);
  return { contents, type: content_type === null || content_type === undefined ? '' : content_type };
};
const __tbReferrerPolicy = value => {
  const policy = String(value);
  const allowed = ['', 'no-referrer', 'no-referrer-when-downgrade', 'origin', 'origin-when-cross-origin', 'unsafe-url', 'same-origin', 'strict-origin', 'strict-origin-when-cross-origin'];
  if (__tbArray.indexOf(allowed, policy) === -1) throw new TypeError('Invalid referrer policy');
  return policy;
};
const __tbRequestCache = value => {
  const cache = String(value);
  const allowed = ['default', 'no-store', 'reload', 'no-cache', 'force-cache', 'only-if-cached'];
  if (__tbArray.indexOf(allowed, cache) === -1) throw new TypeError('Invalid cache mode');
  return cache;
};
const __tbRequestDuplex = value => {
  if (String(value) !== 'half') throw new TypeError('Invalid duplex mode');
  return 'half';
};
globalThis.Request = class Request {
  constructor(input, init) {
    const options = init === undefined ? {} : Object(init);
    if (options.window !== undefined && options.window !== null) {
      throw new TypeError('RequestInit.window must be null');
    }
    const source = input instanceof globalThis.Request ? __tbBrand(input, __tbRequestData) : null;
    let url;
    if (source === null) {
      const resolved = host.__tbResolveUrl(__tbToUSVString(input), undefined);
      if (resolved == null) throw new TypeError('Invalid URL');
      const parsed = new URL(resolved);
      if (parsed.username !== '' || parsed.password !== '') throw new TypeError('URL with credentials');
      url = resolved;
    } else {
      url = source.url;
    }
    const method = options.method !== undefined ? __tbRequestMethod(options.method) : source === null ? 'GET' : source.method;
    const mode = options.mode !== undefined ? __tbRequestMode(options.mode) : source === null ? 'cors' : source.mode;
    const initHeaders = options.headers !== undefined ? options.headers : source === null ? undefined : source.headers;
    // The guard is installed before the list is filled so construction cannot
    // bypass it (<https://fetch.spec.whatwg.org/#concept-headers-guard>).
    const headers = __tbNewHeaders(initHeaders, mode === 'no-cors' ? 'request-no-cors' : 'request');
    const credentials = options.credentials !== undefined ? __tbCredentialsMode(options.credentials) : source === null ? 'same-origin' : source.credentials;
    const redirect = options.redirect !== undefined ? __tbRedirectMode(options.redirect) : source === null ? 'follow' : source.redirect;
    const cache = options.cache !== undefined ? __tbRequestCache(options.cache) : source === null ? 'default' : source.cache;
    if (cache === 'only-if-cached' && mode !== 'same-origin') {
      throw new TypeError('only-if-cached requires same-origin mode');
    }
    if (mode === 'no-cors' && redirect !== 'follow') {
      throw new TypeError('no-cors requests must follow redirects');
    }
    const integrity = options.integrity !== undefined ? String(options.integrity) : source === null ? '' : source.integrity;
    if (options.priority !== undefined
        && options.priority !== 'high'
        && options.priority !== 'low'
        && options.priority !== 'auto') {
      throw new TypeError('Invalid request priority');
    }
    const keepalive = options.keepalive !== undefined ? Boolean(options.keepalive) : source === null ? false : source.keepalive;
    const duplex = options.duplex !== undefined ? __tbRequestDuplex(options.duplex) : source === null ? 'half' : source.duplex;
    const referrerPolicy = options.referrerPolicy !== undefined ? __tbReferrerPolicy(options.referrerPolicy) : source === null ? '' : source.referrerPolicy;
    let referrer;
    if (options.referrer !== undefined) {
      const value = String(options.referrer);
      if (value === '' || value === 'about:client') {
        referrer = value;
      } else {
        const resolved = host.__tbResolveUrl(__tbToUSVString(value), undefined);
        if (resolved == null) throw new TypeError('Invalid referrer');
        // A referrer cross-origin to the settings object is not exposed; it
        // becomes "client". An opaque origin is never same origin, and it
        // serializes as "null", so it needs its own check
        // (<https://fetch.spec.whatwg.org/#dom-request-referrer>).
        const origin = host.__tbOrigin();
        referrer = origin !== 'null' && new URL(resolved).origin === origin
          ? resolved
          : 'about:client';
      }
    } else {
      referrer = source === null ? 'about:client' : source.referrer;
    }
    let signal = options.signal !== undefined
      ? options.signal === null ? new AbortController().signal : options.signal
      : source === null ? new AbortController().signal : source.signal;
    if (typeof AbortSignal.any === 'function') {
      signal = AbortSignal.any([signal]);
    } else {
      const controller = new AbortController();
      if (signal.aborted) controller.abort(signal.reason);
      else signal.addEventListener('abort', () => controller.abort(signal.reason));
      signal = controller.signal;
    }
    // A body inherited from a Request is transferred through a proxy reader;
    // the source becomes disturbed and the new request gets its own stream
    // (<https://fetch.spec.whatwg.org/#dom-request>).
    const inheritedBody = options.body === undefined && source !== null;
    if (inheritedBody && source.stream !== null && (__tbBodyUsed(source) || source.stream.locked)) {
      throw new TypeError('Body already used');
    }
    if (options.body !== undefined && options.body !== null && options.body instanceof ReadableStream && options.duplex === undefined) {
      throw new TypeError('A stream body requires duplex');
    }
    if ((method === 'GET' || method === 'HEAD') && (inheritedBody ? source.stream !== null : options.body !== undefined && options.body !== null)) {
      throw new TypeError('GET and HEAD requests cannot have a body');
    }
    // A `no-cors` Request rejects a method that is not CORS-safelisted
    // (<https://fetch.spec.whatwg.org/#dom-request>).
    if (mode === 'no-cors' && !__tbCorsSafelistedMethod(method)) {
      throw new TypeError('Invalid request method for no-cors mode');
    }
    let stream;
    let bodySize = 0;
    let bodySourceIsStream;
    if (inheritedBody) {
      bodySourceIsStream = source.bodySourceIsStream === true;
      if (source.stream === null) {
        stream = null;
      } else {
        const reader = source.stream.getReader();
        source.bodyUsed = true;
        stream = new ReadableStream({
          pull(controller) {
            return reader.read().then(step => {
              if (step.done) controller.close();
              else controller.enqueue(step.value);
            });
          },
        });
        bodySize = source.bodySize === undefined ? 0 : source.bodySize;
      }
    } else {
      bodySourceIsStream = options.body instanceof ReadableStream;
      if (source !== null && source.stream !== null && !__tbBodyUsed(source) && !source.stream.locked) {
        // Replacing the body still consumes the source request in the
        // vendored WPT revision this tree pins.
        source.stream.getReader();
        source.bodyUsed = true;
      }
      const extracted = __tbExtractBody(options.body === undefined ? null : options.body);
      if (options.body instanceof FormData) {
        let empty = true;
        for (const entry of options.body) {
          empty = false;
          break;
        }
        // An empty FormData request body is zero bytes; the Response
        // constructor keeps the multipart terminator instead
        // (<https://fetch.spec.whatwg.org/#bodyinit-safely-extract>).
        if (empty) extracted.bytes = new __tbUint8Array(0);
      }
      if (extracted.type !== null && __tbHeadersLookupRaw(headers, 'content-type') === null) {
        __tbHeadersAppendRaw(headers, 'content-type', extracted.type);
      }
      stream = __tbBodyStream(extracted);
      bodySize = extracted.bytes === null ? 0 : extracted.bytes.length;
    }
    if (keepalive && bodySourceIsStream) {
      throw new TypeError('A keepalive request cannot have a stream body');
    }
    if (bodySourceIsStream && mode !== 'same-origin' && mode !== 'cors') {
      throw new TypeError('A stream body requires same-origin or cors mode');
    }
    // A Request keeps a blob URL's data alive, so fetching it still works
    // after revokeObjectURL (<https://w3c.github.io/FileAPI/#lifeTime>).
    const blob = source !== null ? source.blob : url.indexOf('blob:') === 0 ? __tbLookupObjectUrl(url) : null;
    __tbRequestData.set(this, {
      url, method, blob, headers, stream, bodySourceIsStream, bodyUsed: false, bodySize, mode, credentials, redirect,
      referrer, referrerPolicy, cache, integrity, keepalive, duplex, signal,
      destination: source === null ? '' : source.destination,
      isReloadNavigation: false,
      isHistoryNavigation: false,
    });
  }
  get url() { return __tbBrand(this, __tbRequestData).url; }
  get method() { return __tbBrand(this, __tbRequestData).method; }
  get headers() { return __tbBrand(this, __tbRequestData).headers; }
  get mode() { return __tbBrand(this, __tbRequestData).mode; }
  get credentials() { return __tbBrand(this, __tbRequestData).credentials; }
  get redirect() { return __tbBrand(this, __tbRequestData).redirect; }
  get referrer() { return __tbBrand(this, __tbRequestData).referrer; }
  get referrerPolicy() { return __tbBrand(this, __tbRequestData).referrerPolicy; }
  get cache() { return __tbBrand(this, __tbRequestData).cache; }
  get integrity() { return __tbBrand(this, __tbRequestData).integrity; }
  get keepalive() { return __tbBrand(this, __tbRequestData).keepalive; }
  get duplex() { return __tbBrand(this, __tbRequestData).duplex; }
  get destination() { return __tbBrand(this, __tbRequestData).destination; }
  get isReloadNavigation() { return __tbBrand(this, __tbRequestData).isReloadNavigation; }
  get isHistoryNavigation() { return __tbBrand(this, __tbRequestData).isHistoryNavigation; }
  get signal() { return __tbBrand(this, __tbRequestData).signal; }
  clone() {
    const data = __tbBrand(this, __tbRequestData);
    if (__tbBodyUsed(data)) throw new TypeError('body already used');
    let stream = null;
    if (data.stream !== null) {
      if (data.stream.locked) throw new TypeError('body is locked');
      const branches = data.stream.tee();
      data.stream = branches[0];
      stream = branches[1];
    }
    const copy = Object.create(globalThis.Request.prototype);
    const headers = __tbMakeHeaders(
      __tbArray.map(__tbHeadersList(data.headers), entry => [entry[0], entry[1]]),
      __tbHeadersGuardOf(data.headers),
    );
    const signal = AbortSignal.any([data.signal]);
    __tbRequestData.set(copy, { ...data, stream, signal, bodyUsed: false, headers });
    return copy;
  }
};
Object.defineProperty(globalThis.Request.prototype, Symbol.toStringTag, { value: 'Request', writable: false, enumerable: false, configurable: true });
__tbInstallBody(globalThis.Response.prototype, __tbResponseData);
__tbInstallBody(globalThis.Request.prototype, __tbRequestData);
const __tbRequestConstructor = globalThis.Request;
// The body of a response the engine already holds, in the same tagged shape
// the host hands the fetch callback
// (<https://fetch.spec.whatwg.org/#concept-response-body>).
const __tbBodyBytes = bytes => ({ kind: 'bytes', bytes });

const __tbMakeResponse = (body, status, statusText, url, type, fields, responseType, redirected) => {
  // An opaque filtered response exposes no status, headers, or body
  // (<https://fetch.spec.whatwg.org/#concept-filtered-response-opaque>).
  const opaque = responseType === 'opaque';
  const headers = __tbMakeHeaders(opaque ? [] : fields, 'response');
  if (!opaque && type && __tbHeadersLookupRaw(headers, 'content-type') === null) {
    __tbHeadersAppendRaw(headers, 'content-type', type);
  }
  __tbHeadersRecord(headers).guard = 'immutable';
  const nullBody = opaque || status === 204 || status === 205 || status === 304;
  const undecodable = body.kind === 'undecodable';
  const extracted = __tbExtractBody(undecodable || nullBody ? null : body.bytes);
  const stream = undecodable ? __tbErroredStream() : nullBody ? null : __tbBodyStream(extracted);
  return __tbCreateResponse({
    stream,
    bodyUsed: false,
    status: opaque ? 0 : status,
    statusText: opaque ? '' : statusText === undefined ? '' : String(statusText),
    url: url === undefined ? '' : String(url),
    type: responseType,
    redirected: redirected === true,
    headers,
  });
};

const __tbMakeStreamingResponse = (outcome, signal) => {
  const headers = __tbMakeHeaders(outcome.headers, 'response');
  __tbHeadersRecord(headers).guard = 'immutable';
  const id = outcome.body.id;
  const stream = new ReadableStream({
    type: 'bytes',
    start(controller) {
      if (signal !== undefined && signal !== null) {
        const abort = () => {
          host.__tbFetchCancel(id);
          controller.error(signal.reason);
        };
        if (signal.aborted) {
          abort();
          return;
        }
        signal.addEventListener('abort', abort, { once: true });
      }
    },
    pull(controller) {
      if (signal !== undefined && signal !== null && signal.aborted) {
        controller.error(signal.reason);
        return;
      }
      let chunk;
      try {
        chunk = host.__tbFetchRead(id);
      } catch (error) {
        controller.error(error);
        return;
      }
      if (chunk === null || chunk === undefined) controller.close();
      else controller.enqueue(new Uint8Array(chunk));
    },
    cancel() { host.__tbFetchCancel(id); },
  });
  return __tbCreateResponse({
    stream,
    bodyUsed: false,
    status: outcome.status,
    statusText: outcome.statusText === undefined ? '' : String(outcome.statusText),
    url: outcome.url === undefined ? '' : String(outcome.url),
    type: outcome.responseType,
    redirected: outcome.redirectCount > 0,
    headers,
    bodySignal: signal,
  });
};

// <https://fetch.spec.whatwg.org/#bodyinit-safely-extract>
const __tbPageRequestBody = async body => {
  if (body === null || body === undefined) return { bytes: new __tbUint8Array(0), type: null };
  if (body instanceof ReadableStream) {
    const result = await __tbReadAllBytes(body);
    return { bytes: result.bytes, type: null };
  }
  const extracted = __tbExtractBody(body);
  return { bytes: extracted.bytes === null ? new __tbUint8Array(0) : extracted.bytes, type: extracted.type };
};
const __tbQueuePageRequest = (url, method, body, headers, cors, referrer, callback) => {
  const id = ++host.__tb_fetchSeq;
  host.__tb_fetchCbs[id] = function(outcome) {
    delete host.__tb_fetchCbs[id];
    callback(outcome);
  };
  __tbPageRequestBody(body).then(payload => {
    if (!host.__tb_fetchCbs[id]) return;
    const type = __tbApply(__tbHeadersGet, headers, ['content-type']) === null ? payload.type : null;
    const fields = __tbArray.map(__tbSortedHeaders(headers), entry => [entry[0], entry[1]]);
    host.__queueFetch(url, id, method, payload.bytes, type, fields, {
      referrer: referrer === null || referrer === undefined ? null : String(referrer),
      mode: cors.mode,
      credentials: cors.credentials,
      redirect: cors.redirect,
      keepalive: cors.keepalive,
      bodySize: cors.bodySize,
      integrity: cors.integrity === undefined || cors.integrity === '' ? null : String(cors.integrity),
      cache: cors.cache === undefined || cors.cache === '' ? null : String(cors.cache),
      delivery: cors.delivery,
      referrerPolicy: cors.referrerPolicy === undefined || cors.referrerPolicy === '' ? null : String(cors.referrerPolicy),
    });
  }).catch(() => {
    const done = host.__tb_fetchCbs[id];
    if (done) done({ status: 0, statusText: '', body: __tbBodyBytes(new __tbUint8Array(0)), url: '', contentType: '', headers: [], responseType: 'error' });
  });
  return id;
};
// https://fetch.spec.whatwg.org/#fetch-controller
const __tbCancelPageRequest = id => {
  if (id === null) return;
  delete host.__tb_fetchCbs[id];
  host.__cancelFetch(id);
};
// <https://fetch.spec.whatwg.org/#concept-range-header>
const __tbParseRange = (value, length) => {
  const match = /^bytes[ \t]*=[ \t]*([0-9]*)[ \t]*-[ \t]*([0-9]*)$/.exec(value);
  if (match === null) return null;
  const startText = match[1];
  const endText = match[2];
  if (startText === '' && endText === '') return null;
  if (startText === '') {
    const suffix = Number(endText);
    if (suffix === 0) return null;
    return { start: Math.max(length - suffix, 0), end: length - 1 };
  }
  const start = Number(startText);
  if (start >= length) return null;
  let end = endText === '' ? length - 1 : Number(endText);
  if (end < start) return null;
  if (end >= length) end = length - 1;
  return { start, end };
};
globalThis.fetch = function(input, init) {
  let request;
  try {
    request = new __tbRequestConstructor(input, init);
  } catch (error) {
    return Promise.reject(error);
  }
  const data = __tbBrand(request, __tbRequestData);
  const url = data.url;
  const method = data.method;
  const headers = data.headers;
  const body = data.stream;
  const mode = data.mode;
  const credentials = data.credentials;
  const redirect = data.redirect;
  // The host resolves "about:client" against the authorized document and runs
  // "Determine request's Referrer" per hop; JS only carries the request's
  // referrer value
  // (<https://fetch.spec.whatwg.org/#concept-request-referrer>).
  const referrer = data.referrer;
  const signal = data.signal;
  const keepalive = data.keepalive;
  return new Promise(function(resolve, reject) {
    if (signal.aborted) {
      if (body !== null) body.cancel(signal.reason);
      reject(signal.reason);
      return;
    }
    let requestID = null;
    let streamID = null;
    let settled = false;
    const onAbort = () => {
      if (settled) return;
      settled = true;
      if (body !== null) body.cancel(signal.reason);
      if (streamID !== null) host.__tbFetchCancel(streamID);
      if (requestID !== null) __tbCancelPageRequest(requestID);
      reject(signal.reason);
    };
    signal.addEventListener('abort', onAbort);
    const finish = (callback, value) => {
      if (settled) return;
      settled = true;
      signal.removeEventListener('abort', onAbort);
      callback(value);
    };
    if (url.indexOf('blob:') === 0) {
      if (method !== 'GET' || body !== null) {
        finish(reject, new TypeError('Failed to fetch'));
        return;
      }
      const entry = data.blob === null ? __tbLookupObjectUrl(url) : data.blob;
      if (entry === null) {
        finish(reject, new TypeError('Failed to fetch'));
        return;
      }
      const contents = typeof entry.contents === 'string' ? __tbUtf8Encode(entry.contents) : entry.contents;
      const range = __tbHeadersLookupRaw(headers, 'range');
      if (range === null) {
        const blobType = entry.type === null || entry.type === undefined ? '' : entry.type;
        const fields = [
          ['content-length', String(contents.length)],
          ['content-type', host.__tbExtractMimeType(blobType) === '' ? '' : blobType],
        ];
        finish(resolve, __tbMakeResponse(__tbBodyBytes(contents), 200, 'OK', url, null, fields, 'basic', false));
        return;
      }
      const parsed = __tbParseRange(range, contents.length);
      if (parsed === null) {
        finish(reject, new TypeError('Failed to fetch'));
        return;
      }
      const fields = [
        ['content-range', 'bytes ' + parsed.start + '-' + parsed.end + '/' + contents.length],
        ['content-length', String(parsed.end - parsed.start + 1)],
        ['content-type', entry.type === null || entry.type === undefined ? '' : entry.type],
      ];
      finish(resolve, __tbMakeResponse(
        __tbBodyBytes(__tbBytesCopy(contents, parsed.start, parsed.end + 1)),
        206, 'Partial Content', url, entry.type, fields, 'basic', false,
      ));
      return;
    }
    // A fragment is not part of the HTTP request or the response URL
    // (<https://fetch.spec.whatwg.org/#concept-request-url>).
    const hash = url.indexOf('#');
    const networkUrl = hash === -1 ? url : url.slice(0, hash);
    requestID = __tbQueuePageRequest(networkUrl, method, body, headers, {
      mode,
      credentials,
      redirect,
      keepalive,
      bodySize: data.bodySize,
      integrity: data.integrity,
      cache: data.cache,
      // HEAD has no body, and SRI has to see the whole body before exposure.
      delivery: method !== 'HEAD' && data.integrity === '' ? 'stream' : 'buffer',
      referrerPolicy: data.referrerPolicy,
    }, referrer, outcome => {
      const status = outcome.status;
      const statusText = outcome.statusText;
      const body = outcome.body;
      const finalUrl = outcome.url;
      const type = outcome.contentType;
      const fields = outcome.headers;
      const responseType = outcome.responseType;
      if (responseType !== 'error') {
        // A redirect in manual mode is an opaque-redirect filtered response
        // (<https://fetch.spec.whatwg.org/#concept-filtered-response-opaque-redirect>).
        if (responseType === 'opaqueredirect'
            || (redirect === 'manual' && (status === 301 || status === 302 || status === 303 || status === 307 || status === 308))) {
          finish(resolve, __tbMakeResponse(__tbBodyBytes(new __tbUint8Array(0)), 0, '', url, '', [], 'opaqueredirect', false));
          return;
        }
        if (status === 204 || status === 205 || status === 304) {
          if (body.kind === 'stream') host.__tbFetchCancel(body.id);
          finish(resolve, __tbWithSignal(__tbMakeResponse(body, status, statusText, finalUrl, type, fields, responseType, outcome.redirectCount > 0), signal));
          return;
        }
        if (body.kind === 'stream') {
          streamID = body.id;
          signal.addEventListener('abort', () => host.__tbFetchCancel(streamID), { once: true });
          finish(resolve, __tbMakeStreamingResponse(outcome, signal));
          return;
        }
        const response = __tbWithSignal(__tbMakeResponse(body, status, statusText, finalUrl, type, fields, responseType, outcome.redirectCount > 0), signal);
        // A HEAD response has no body regardless of what the wire carried
        // (<https://fetch.spec.whatwg.org/#concept-head>).
        if (method === 'HEAD') __tbBrand(response, __tbResponseData).stream = null;
        finish(resolve, response);
      } else {
        finish(reject, new TypeError('Failed to fetch'));
      }
    });
  });
};
// https://w3c.github.io/FileAPI/#blob
