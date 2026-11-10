// https://xhr.spec.whatwg.org/#the-xmlhttprequest-interface
const __tbXhrData = host.slots('tinybrowser.xhr');
// One helper for the `responseDocument` sentinel discipline: `undefined`
// means uncached (not yet computed), `null` means cached-empty.
const __tbXhrResetDocument = (data, value) => { data.responseDocument = value; };
const __tbXhrCancel = data => {
  data.generation++;
  __tbCancelPageRequest(data.request);
  data.request = null;
  if (data.timer !== null) clearTimeout(data.timer);
  data.timer = null;
};
// State after a failed request: done, not sent, and a network error as the
// response (<https://xhr.spec.whatwg.org/#request-error-steps>).
const __tbXhrResetResponse = data => {
  __tbXhrCancel(data);
  data.sent = false;
  data.state = 4;
  data.status = 0;
  data.responseURL = '';
  data.responseHeaders = new __tbHeadersConstructor();
  data.responseBytes = host.slots.bytes(0);
  __tbXhrResetDocument(data, undefined);
};
// https://xhr.spec.whatwg.org/#request-error-steps
const __tbXhrError = (xhr, data, type) => {
  __tbXhrResetResponse(data);
  xhr.dispatchEvent(new Event('readystatechange'));
  xhr.dispatchEvent(new ProgressEvent(type));
  xhr.dispatchEvent(new ProgressEvent('loadend'));
};
// A synchronous request reports the same failure by throwing: its whole
// lifecycle is inside `send`, so there is no later event to listen for
// (<https://xhr.spec.whatwg.org/#request-error-steps>).
const __tbXhrSyncError = (data, message) => {
  __tbXhrResetResponse(data);
  throw new DOMException(message, 'NetworkError');
};
// https://xhr.spec.whatwg.org/#the-timeout-attribute
const __tbXhrTimeout = (xhr, data) => {
  if (data.timer !== null) clearTimeout(data.timer);
  data.timer = null;
  if (!data.sent || data.timeout === 0) return;
  const generation = data.generation;
  data.timer = host.__tb_timeouts.length;
  host.__tb_timeouts[data.timer] = () => {
    if (data.sent && data.generation === generation) __tbXhrError(xhr, data, 'timeout');
  };
  host.__scheduleTimeout(data.timer, Math.max(0, data.started + data.timeout - Date.now()));
};
// Synchronous `send` cannot await body extraction. Blob bytes are already in
// the slot; other body types match the async extractor
// (<https://fetch.spec.whatwg.org/#bodyinit-safely-extract>).
const __tbSyncRequestBody = body => {
  if (body === null || body === undefined) return { bytes: new __tbUint8Array(0), type: null };
  if (typeof Blob === 'function' && body instanceof Blob) {
    const stored = __tbBlobData.get(body);
    if (stored === undefined) return null;
    return { bytes: stored.bytes, type: body.type || null };
  }
  if (body instanceof ArrayBuffer) return { bytes: new __tbUint8Array(body), type: null };
  if (ArrayBuffer.isView(body)) {
    return {
      bytes: new __tbUint8Array(body.buffer, body.byteOffset, body.byteLength),
      type: null,
    };
  }
  if (body instanceof URLSearchParams) {
    return {
      bytes: __tbUtf8Encode(String(body)),
      type: 'application/x-www-form-urlencoded;charset=UTF-8',
    };
  }
  return {
    bytes: __tbUtf8Encode(String(body)),
    type: 'text/plain;charset=UTF-8',
  };
};
// "get a final MIME type": the override MIME type when one is set, otherwise
// the response MIME type, whose extraction failure is `text/xml`
// (<https://xhr.spec.whatwg.org/#final-mime-type>,
// <https://xhr.spec.whatwg.org/#response-mime-type>).
const __tbXhrFinalMime = data => {
  if (data.overrideMimeType !== null) return data.overrideMimeType;
  const header = __tbApply(__tbHeadersGet, data.responseHeaders, ['content-type']);
  const mime = header === null ? '' : host.__tbExtractMimeType(header);
  return mime === '' ? 'text/xml' : mime;
};
// https://xhr.spec.whatwg.org/#handle-response-end-of-body
const __tbXhrFinish = (xhr, data, generation, status, bytes, url, fields, sync) => {
  if (data.generation !== generation || !data.sent) return;
  data.request = null;
  if (status === 0) {
    __tbXhrError(xhr, data, 'error');
    return;
  }
  data.status = status;
  data.responseURL = url;
  // The response header list stays as served: nothing is sniffed into it
  // (<https://fetch.spec.whatwg.org/#concept-response-header-list>).
  data.responseHeaders = new __tbHeadersConstructor(fields);
  // A synchronous send pauses inside `send` and then runs end-of-body.
  // It does not visit headers-received or loading, and it does not fire
  // `progress` (<https://xhr.spec.whatwg.org/#handle-response-end-of-body>).
  if (sync) {
    data.responseBytes = host.slots.bytes(bytes);
  } else {
    data.state = 2;
    xhr.dispatchEvent(new Event('readystatechange'));
    if (data.generation !== generation || data.state !== 2 || !data.sent) return;
    data.responseBytes = host.slots.bytes(bytes);
    if (bytes.length) {
      data.state = 3;
      xhr.dispatchEvent(new Event('readystatechange'));
      if (data.generation !== generation || data.state !== 3 || !data.sent) return;
      xhr.dispatchEvent(new ProgressEvent('progress', { loaded: bytes.length }));
      if (data.generation !== generation || data.state !== 3 || !data.sent) return;
    }
  }
  if (data.timer !== null) clearTimeout(data.timer);
  data.timer = null;
  data.state = 4;
  data.sent = false;
  xhr.dispatchEvent(new Event('readystatechange'));
  if (data.generation !== generation || data.state !== 4) return;
  xhr.dispatchEvent(new ProgressEvent('load', { loaded: bytes.length }));
  xhr.dispatchEvent(new ProgressEvent('loadend', { loaded: bytes.length }));
};
globalThis.XMLHttpRequestUpload = class XMLHttpRequestUpload extends EventTarget {};
globalThis.XMLHttpRequest = class XMLHttpRequest extends EventTarget {
  constructor() {
    super();
    __tbXhrData.set(this, {
        __proto__: null,
        state: 0, sent: false, method: '', url: '', headers: new __tbHeadersConstructor(),
        responseHeaders: new __tbHeadersConstructor(), responseURL: '', status: 0, responseBytes: host.slots.bytes(0),
        responseType: '', overrideMimeType: null, timeout: 0, withCredentials: false, sync: false, generation: 0,
        request: null, timer: null, started: 0, responseDocument: undefined,
        upload: new XMLHttpRequestUpload(),
    });
  }
  get readyState() { return __tbBrand(this, __tbXhrData).state; }
  get status() { return __tbBrand(this, __tbXhrData).status; }
  get statusText() { return ''; }
  get responseURL() { return __tbBrand(this, __tbXhrData).responseURL; }
  get upload() { return __tbBrand(this, __tbXhrData).upload; }
  get timeout() { return __tbBrand(this, __tbXhrData).timeout; }
  set timeout(value) {
    const data = __tbBrand(this, __tbXhrData);
    // A Window synchronous request cannot carry a timeout
    // (<https://xhr.spec.whatwg.org/#the-timeout-attribute>).
    if (data.sync) throw new DOMException('Synchronous requests do not support timeout', 'InvalidAccessError');
    data.timeout = Number(value) >>> 0;
    __tbXhrTimeout(this, data);
  }
  get withCredentials() { return __tbBrand(this, __tbXhrData).withCredentials; }
  set withCredentials(value) {
    const data = __tbBrand(this, __tbXhrData);
    if (data.state > 1 || data.sent) throw new DOMException('Request already sent', 'InvalidStateError');
    data.withCredentials = Boolean(value);
  }
  get responseType() { return __tbBrand(this, __tbXhrData).responseType; }
  set responseType(value) {
    const data = __tbBrand(this, __tbXhrData);
    // <https://xhr.spec.whatwg.org/#the-responsetype-attribute>
    if (data.state >= 3) throw new DOMException('Response loading', 'InvalidStateError');
    const next = String(value);
    // The spec declares this attribute as the `XMLHttpRequestResponseType`
    // enum, so WebIDL conversion would throw a TypeError for an unknown value.
    // Chrome and Firefox silently ignore it instead, and
    // `xhr/response-invalid-responsetype.htm` pins that behavior.
    if (!['', 'text', 'json', 'arraybuffer', 'blob', 'document'].includes(next)) return;
    if (data.sync) throw new DOMException('Synchronous requests do not support responseType', 'InvalidAccessError');
    data.responseType = next;
  }
  get responseText() {
    const data = __tbBrand(this, __tbXhrData);
    if (data.responseType !== '' && data.responseType !== 'text') throw new DOMException('Not a text response', 'InvalidStateError');
    return data.state < 3 ? '' : __tbDecodeBytes(data.responseBytes, 'utf-8', false, false);
  }
  get response() {
    // https://xhr.spec.whatwg.org/#dom-xmlhttprequest-response
    const data = __tbBrand(this, __tbXhrData);
    if (data.responseType === '' || data.responseType === 'text') return this.responseText;
    if (data.state !== 4) return null;
    if (data.responseType === 'arraybuffer') return __tbBytesCopy(data.responseBytes).buffer;
    if (data.responseType === 'blob') {
      // The Blob type is the final MIME type for this
      // (<https://xhr.spec.whatwg.org/#the-response-attribute>).
      return __tbMakeBlob(__tbBytesCopy(data.responseBytes), __tbXhrFinalMime(data));
    }
    if (data.responseType === 'json') {
      try { return JSON.parse(__tbDecodeBytes(data.responseBytes, 'utf-8', false, false)); }
      catch (_) { return null; }
    }
    if (data.responseType === 'document') return this.responseXML;
    return null;
  }
  get responseXML() {
    // https://xhr.spec.whatwg.org/#response-xml
    // A worker global has no document response: the attribute is null even
    // for an XML body
    // (<https://xhr.spec.whatwg.org/#response-xml>).
    if (globalThis.document === undefined) return null;
    const data = __tbBrand(this, __tbXhrData);
    if (data.responseType !== '' && data.responseType !== 'document') {
      throw new DOMException('Not a document response', 'InvalidStateError');
    }
    if (data.state !== 4) return null;
    if (data.responseDocument !== undefined) return data.responseDocument;
    const header = data.overrideMimeType || __tbApply(__tbHeadersGet, data.responseHeaders, ['content-type']) || '';
    const essence = __tbApply(__tbStringTrim, __tbApply(__tbStringSplit, __tbIDLString(header), [';'])[0], []);
    const mime = __tbApply(__tbStringToLowerCase, essence, []);
    const xml = mime === 'text/xml' || mime === 'application/xml' || mime === 'application/xhtml+xml'
      || mime === 'image/svg+xml' || __tbApply(__tbStringEndsWith, mime, ['+xml']);
    const supported = ['text/html', 'text/xml', 'application/xml', 'application/xhtml+xml', 'image/svg+xml'];
    let type = null;
    if (xml) type = __tbArray.indexOf(supported, mime) !== -1 ? mime : 'application/xml';
    else if (mime === 'text/html' && data.responseType === 'document') type = 'text/html';
    if (type === null) {
      __tbXhrResetDocument(data, null);
      return null;
    }
    let source;
    try {
      // Decoded the way navigations decode: BOM, then the transport charset
      // or the XML declaration, then UTF-8. Fatal: undecodable bytes mean no
      // document, not replacement text.
      source = host.__tbDecodeXmlResponse(data.responseBytes, header);
    } catch (_) {
      __tbXhrResetDocument(data, null);
      return null;
    }
    let parsed;
    try {
      // The document takes the response's URL, not the realm's, so the XHR
      // path uses its own parse entry point rather than `DOMParser`
      // (<https://xhr.spec.whatwg.org/#document-response>).
      parsed = host.__tbXhrParseDocument(source, type, data.responseURL);
    } catch (_) {
      parsed = null;
    }
    // An empty body parses to no document element; a `parsererror`
    // document element in the parsererror namespace means the body was not
    // well-formed, so there is no document either
    // (<https://xhr.spec.whatwg.org/#document-response>). The namespace
    // check matters: a well-formed `<parsererror/>` response is a valid
    // document, not a failure. Anything else the parser accepts is the
    // document.
    if (parsed !== null && parsed.documentElement !== null
      && parsed.documentElement.localName === 'parsererror'
      && parsed.documentElement.namespaceURI === 'http://www.mozilla.org/newlayout/xml/parsererror.xml') parsed = null;
    data.responseDocument = parsed !== null && parsed.documentElement !== null ? parsed : null;
    return data.responseDocument;
  }
  // https://xhr.spec.whatwg.org/#the-open()-method
  open(method, url, async) {
    const data = __tbBrand(this, __tbXhrData);
    const verb = String(method);
    if (!/^[!#$%&'*+.^_`|~0-9A-Za-z-]+$/.test(verb)) throw new DOMException('Invalid HTTP method', 'SyntaxError');
    if (/^(CONNECT|TRACE|TRACK)$/i.test(verb)) throw new DOMException('Forbidden HTTP method', 'SecurityError');
    const parsed = host.__tbResolveUrl(String(url), undefined);
    if (parsed === null) throw new DOMException('Invalid URL', 'SyntaxError');
    // Omitted stays true. An explicit `undefined` is the boolean false, which
    // legacy content depends on
    // (<https://xhr.spec.whatwg.org/#the-open()-method>).
    const asynchronous = arguments.length < 3 ? true : Boolean(async);
    if (!asynchronous && (data.timeout !== 0 || data.responseType !== '')) {
      throw new DOMException('Synchronous requests do not support timeout or responseType', 'InvalidAccessError');
    }
    __tbXhrCancel(data);
    data.sync = !asynchronous;
    data.method = /^(GET|HEAD|POST|PUT|DELETE|OPTIONS)$/i.test(verb) ? verb.toUpperCase() : verb;
    data.url = parsed;
    data.sent = false;
    data.headers = new __tbHeadersConstructor();
    data.responseHeaders = new __tbHeadersConstructor();
    data.responseURL = '';
    data.responseBytes = host.slots.bytes(0);
    __tbXhrResetDocument(data, undefined);
    data.status = 0;
    if (data.state !== 1) {
      data.state = 1;
      this.dispatchEvent(new Event('readystatechange'));
    }
  }
  // https://xhr.spec.whatwg.org/#the-setrequestheader()-method
  setRequestHeader(name, value) {
    const data = __tbBrand(this, __tbXhrData);
    if (data.state !== 1 || data.sent) throw new DOMException('Request not open', 'InvalidStateError');
    // ByteString conversion comes first, so a code unit above 0xFF is a
    // TypeError; an invalid header name or value is a SyntaxError
    // (<https://xhr.spec.whatwg.org/#dom-xmlhttprequest-setrequestheader>).
    const headerName = __tbByteString(name);
    const headerValue = __tbByteString(value).replace(/^[\t\n\r ]+|[\t\n\r ]+$/g, '');
    if (!__tbIsHeaderName(headerName) || !__tbIsHeaderValue(headerValue)) {
      throw new DOMException('Invalid header', 'SyntaxError');
    }
    __tbHeadersAppendRaw(data.headers, headerName, headerValue);
  }
  // https://xhr.spec.whatwg.org/#the-send()-method
  send(body = null) {
    const data = __tbBrand(this, __tbXhrData);
    if (data.state !== 1 || data.sent) throw new DOMException('Request not open', 'InvalidStateError');
    if (data.method === 'GET' || data.method === 'HEAD') body = null;
    data.sent = true;
    data.started = Date.now();
    const generation = data.generation;
    this.dispatchEvent(new ProgressEvent('loadstart'));
    if (data.state !== 1 || !data.sent || data.generation !== generation) return;
    if (data.sync) {
      // The async path checks connect policy in the dial queue; a synchronous
      // send cannot reach the queue before it returns, so it checks the
      // reinstated hook here
      // (<https://w3c.github.io/webappsec-csp/#should-request-be-blocked-by-content-security-policy>).
      // Every failure throws a NetworkError DOMException instead of firing an
      // event: the request's whole lifecycle is inside `send`
      // (<https://xhr.spec.whatwg.org/#request-error-steps>).
      if (host.__tbCspAllowsConnect(data.url) === false) {
        __tbXhrSyncError(data, 'The request was blocked by Content Security Policy.');
      }
      // `blob:` is resolved in this realm. The carrier only speaks http(s)
      // and `data:` (<https://fetch.spec.whatwg.org/#scheme-fetch>).
      if (data.url.indexOf('blob:') === 0) {
        if (data.method !== 'GET') {
          __tbXhrSyncError(data, 'The request failed due to a network error.');
        }
        const entry = __tbLookupObjectUrl(data.url);
        if (entry === null) {
          __tbXhrSyncError(data, 'The request failed due to a network error.');
        }
        const bytes = __tbUtf8Encode(String(entry.contents));
        // The blob URL response's header list carries the blob's type
        // (<https://fetch.spec.whatwg.org/#scheme-fetch>).
        __tbXhrFinish(this, data, generation, 200, bytes, data.url, [['content-type', entry.type]], true);
        return;
      }
      const payload = __tbSyncRequestBody(body);
      if (payload === null) {
        __tbXhrSyncError(data, 'The request body could not be extracted.');
      }
      const type = __tbApply(__tbHeadersGet, data.headers, ['content-type']) === null ? payload.type : null;
      const fields = __tbArray.map(__tbHeadersList(data.headers), entry => [entry[0], entry[1]]);
      // XHR's referrer is the client document; the host resolves it against
      // the authorized origin
      // (<https://xhr.spec.whatwg.org/#the-send()-method>).
      const result = host.__tbSyncFetch(data.url, data.method, payload.bytes, type, fields, { referrer: 'about:client', withCredentials: data.withCredentials === true }, data.timeout);
      if (result === null || result === undefined) {
        __tbXhrSyncError(data, 'The request failed due to a network error.');
      }
      __tbXhrFinish(this, data, generation, result[0], result[1], result[2], result[3], true);
      return;
    }
    // An async XHR of a `blob:` URL resolves in this realm, like `fetch`: the
    // carrier only speaks http(s) and `data:`
    // (<https://fetch.spec.whatwg.org/#scheme-fetch>). The completion is
    // queued as a task so the events never fire inside `send()`.
    if (data.url.indexOf('blob:') === 0) {
      const generation = data.generation;
      const entry = data.method === 'GET' ? __tbLookupObjectUrl(data.url) : null;
      setTimeout(() => {
        if (entry === null) {
          __tbXhrError(this, data, 'error');
          return;
        }
        const bytes = typeof entry.contents === 'string'
          ? __tbUtf8Encode(entry.contents)
          : entry.contents;
        __tbXhrFinish(this, data, generation, 200, bytes, data.url,
          [['content-type', entry.type]], false);
      }, 0);
      __tbXhrTimeout(this, data);
      return;
    }
    data.request = __tbQueuePageRequest(data.url, data.method, body, data.headers, {
      mode: 'cors',
      credentials: data.withCredentials ? 'include' : 'same-origin',
      redirect: 'follow',
      keepalive: false,
      bodySize: 0,
      delivery: 'buffer',
    }, 'about:client', outcome => {
      if (outcome.body.kind === 'undecodable') __tbXhrError(this, data, 'error');
      else __tbXhrFinish(this, data, generation, outcome.status, outcome.body.bytes, outcome.url, outcome.headers, false);
    });
    __tbXhrTimeout(this, data);
  }
  // https://xhr.spec.whatwg.org/#the-abort()-method
  abort() {
    const data = __tbBrand(this, __tbXhrData);
    if (data.sent || data.state === 2 || data.state === 3) {
      __tbXhrError(this, data, 'abort');
    } else {
      __tbXhrCancel(data);
      data.status = 0;
      data.responseURL = '';
      data.responseHeaders = new __tbHeadersConstructor();
      data.responseBytes = host.slots.bytes(0);
      __tbXhrResetDocument(data, undefined);
    }
    if (data.state === 4) data.state = 0;
  }
  getResponseHeader(name) {
    const data = __tbBrand(this, __tbXhrData);
    const headerName = __tbByteString(name);
    if (data.state < 2 || !__tbIsHeaderName(headerName)) return null;
    return __tbHeadersLookupRaw(data.responseHeaders, headerName.toLowerCase());
  }
  getAllResponseHeaders() {
    const data = __tbBrand(this, __tbXhrData);
    if (data.state < 2) return '';
    return __tbArray.map(__tbHeadersList(data.responseHeaders), entry => [entry[0], entry[1]])
      .sort(([left], [right]) => left.localeCompare(right))
      .map(([name, value]) => name + ': ' + value + '\r\n').join('');
  }
  // https://xhr.spec.whatwg.org/#the-overridemimetype()-method
  overrideMimeType(value) {
    const data = __tbBrand(this, __tbXhrData);
    if (data.state >= 3) throw new DOMException('Response loading', 'InvalidStateError');
    // A MIME type that does not parse becomes `application/octet-stream`
    // (<https://xhr.spec.whatwg.org/#the-overridemimetype()-method>).
    const parsed = host.__tbParseMimeType(String(value));
    data.overrideMimeType = parsed === '' ? 'application/octet-stream' : parsed;
  }
};
for (const [name, value] of [['UNSENT', 0], ['OPENED', 1], ['HEADERS_RECEIVED', 2], ['LOADING', 3], ['DONE', 4]]) {
  for (const target of [globalThis.XMLHttpRequest, globalThis.XMLHttpRequest.prototype]) {
    Object.defineProperty(target, name, { value, writable: false, enumerable: true, configurable: false });
  }
}
for (const name of ['onreadystatechange', 'onloadstart', 'onprogress', 'onabort', 'onerror', 'onload', 'ontimeout', 'onloadend']) {
  Object.defineProperty(globalThis.XMLHttpRequest.prototype, name, { value: null, writable: true, enumerable: true, configurable: true });
  Object.defineProperty(globalThis.XMLHttpRequestUpload.prototype, name, { value: null, writable: true, enumerable: true, configurable: true });
}
Object.defineProperty(globalThis.XMLHttpRequest.prototype, Symbol.toStringTag, { value: 'XMLHttpRequest', configurable: true });
Object.defineProperty(globalThis.XMLHttpRequestUpload.prototype, Symbol.toStringTag, { value: 'XMLHttpRequestUpload', configurable: true });
