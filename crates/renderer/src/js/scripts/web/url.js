const __tbUrlSlots = host.slots('URL');
const __tbParamsSlots = host.slots('URLSearchParams');
const __tbUrlState = value => __tbBrand(value, __tbUrlSlots);
const __tbParamsState = value => __tbBrand(value, __tbParamsSlots);
// Single USVString conversion, shared with the IDL bindings: lone
// surrogates become U+FFFD and Symbols throw, per WebIDL
// (<https://webidl.spec.whatwg.org/#es-USVString>).
host.__tbUSVString = __tbIDLUSVString;

// URL decomposition components; indexes match `js/url_parts.rs`
// (<https://html.spec.whatwg.org/multipage/links.html#url-decomposition-idl-attributes>).
const __tbUrlPart = (href, index) => {
  const values = host.__tbUrlParts(href, href);
  return values === null || values === undefined ? null : values[index];
};
const __tbRefreshUrlParams = url => {
  if (__tbUrlState(url).searchParams !== null) {
    const fresh = __tbParamsState(new URLSearchParams(__tbUrlPart(__tbUrlState(url).href, 7) || '')).pairs;
    const pairs = __tbParamsState(__tbUrlState(url).searchParams).pairs;
    // Replace the contents without replacing the array, so live iterators
    // keep observing the same list
    // (<https://url.spec.whatwg.org/#urlsearchparams-iterate>).
    pairs.length = 0;
    for (const pair of fresh) pairs.push(pair);
  }
};
const __tbSetUrlPart = (url, index, value) => {
  const result = host.__tbUrlSetPart(__tbUrlState(url).href, __tbUrlState(url).href, index, value);
  if (result !== null && result !== undefined) {
    __tbUrlState(url).href = result;
    __tbRefreshUrlParams(url);
  }
};

globalThis.URL = class URL {
  constructor(input, base) {
    // No base means no document fallback: the input must parse absolutely
    // (<https://url.spec.whatwg.org/#concept-url-parser>).
    const href = base === undefined
      ? host.__tbParseUrl(host.__tbUSVString(input))
      : host.__tbResolveUrl(host.__tbUSVString(input), host.__tbUSVString(base));
    if (href == null) throw new TypeError('Invalid URL');
    __tbUrlSlots.set(this, { href, searchParams: null });
  }
  get href() { return __tbUrlState(this).href; }
  set href(value) {
    // A value that fails to parse throws a TypeError and leaves the URL
    // unchanged (<https://url.spec.whatwg.org/#dom-url-href>).
    const parsed = host.__tbParseUrl(host.__tbUSVString(value));
    if (parsed == null) throw new TypeError('Invalid URL');
    __tbUrlState(this).href = parsed;
    __tbRefreshUrlParams(this);
  }
  toString() { return __tbUrlState(this).href; }
  toJSON() { return __tbUrlState(this).href; }
  get protocol() { return __tbUrlPart(__tbUrlState(this).href, 0); }
  set protocol(value) { __tbSetUrlPart(this, 0, host.__tbUSVString(value)); }
  get username() { return __tbUrlPart(__tbUrlState(this).href, 1); }
  set username(value) { __tbSetUrlPart(this, 1, host.__tbUSVString(value)); }
  get password() { return __tbUrlPart(__tbUrlState(this).href, 2); }
  set password(value) { __tbSetUrlPart(this, 2, host.__tbUSVString(value)); }
  get host() { return __tbUrlPart(__tbUrlState(this).href, 3); }
  set host(value) { __tbSetUrlPart(this, 3, host.__tbUSVString(value)); }
  get hostname() { return __tbUrlPart(__tbUrlState(this).href, 4); }
  set hostname(value) { __tbSetUrlPart(this, 4, host.__tbUSVString(value)); }
  get port() { return __tbUrlPart(__tbUrlState(this).href, 5); }
  set port(value) { __tbSetUrlPart(this, 5, host.__tbUSVString(value)); }
  get pathname() { return __tbUrlPart(__tbUrlState(this).href, 6); }
  set pathname(value) { __tbSetUrlPart(this, 6, host.__tbUSVString(value)); }
  get search() { return __tbUrlPart(__tbUrlState(this).href, 7); }
  set search(value) { __tbSetUrlPart(this, 7, host.__tbUSVString(value)); }
  get hash() { return __tbUrlPart(__tbUrlState(this).href, 8); }
  set hash(value) { __tbSetUrlPart(this, 8, host.__tbUSVString(value)); }
  get origin() { return __tbUrlPart(__tbUrlState(this).href, 9); }
  get searchParams() {
    if (__tbUrlState(this).searchParams === null) {
      const params = new URLSearchParams(this.search);
      const url = this;
      __tbParamsState(params).sync = value => {
        const result = host.__tbUrlSetPart(__tbUrlState(url).href, __tbUrlState(url).href, 7, String(value));
        if (result !== null && result !== undefined) __tbUrlState(url).href = result;
      };
      __tbUrlState(this).searchParams = params;
    }
    return __tbUrlState(this).searchParams;
  }
};
globalThis.URL.createObjectURL = function(blob) {
  const data = __tbBrand(blob, __tbBlobData, 'value is not a Blob');
  const text = __tbUtf8Decode(data.bytes);
  const url = host.__tbCreateObjectURL(text, data.type);
  if (url == null) throw new RangeError('object URL budget exceeded');
  return url;
};
// https://w3c.github.io/FileAPI/#dfn-revokeObjectURL
globalThis.URL.revokeObjectURL = function(url) {
  host.__tbRevokeObjectURL(String(url));
};
Object.defineProperty(globalThis.URL.prototype, Symbol.toStringTag, { value: 'URL', writable: false, enumerable: false, configurable: true });
// `location` stringifies to its URL, which is what `new URL(input, location)`
// and other base-taking APIs expect
// (<https://html.spec.whatwg.org/multipage/nav-history-apis.html#dom-location-href>).
if (globalThis.location !== undefined && globalThis.location !== null) {
  Object.defineProperty(globalThis.location, 'toString', {
    value: function() { return String(this.href); },
    writable: true, enumerable: false, configurable: true,
  });
}
// https://url.spec.whatwg.org/#dom-url-parse
globalThis.URL.parse = function(input, base) {
  try { return new globalThis.URL(input, base); }
  catch (error) { return null; }
};
// https://url.spec.whatwg.org/#dom-url-canparse
globalThis.URL.canParse = function(input, base) {
  return globalThis.URL.parse(input, base) !== null;
};
// https://url.spec.whatwg.org/#interface-urlsearchparams
globalThis.URLSearchParams = class URLSearchParams {
  constructor(init) {
    __tbParamsSlots.set(this, { pairs: [], sync: null });
    if (init instanceof URLSearchParams) {
      __tbParamsState(this).pairs = __tbArray.map(__tbParamsState(init).pairs, pair => [pair[0], pair[1]]);
      return;
    }
    if (init !== null && typeof init === 'object') {
      if (typeof init[Symbol.iterator] === 'function') {
        for (const pair of init) {
          const values = __tbArrayFrom(pair);
          if (values.length !== 2) throw new TypeError('parameter pair must contain two values');
          __tbArray.push(__tbParamsState(this).pairs, [host.__tbUSVString(values[0]), host.__tbUSVString(values[1])]);
        }
      } else {
        for (const name of __tbObjectKeys(init)) {
          __tbArray.push(__tbParamsState(this).pairs, [host.__tbUSVString(name), host.__tbUSVString(init[name])]);
        }
      }
      return;
    }
    var input = host.__tbUSVString(init === undefined ? '' : init);
    if (input[0] === '?') input = __tbApply(__tbStringSlice, input, [1]);
    if (!input) return;
    // The `application/x-www-form-urlencoded` parser drops empty items
    // (<https://url.spec.whatwg.org/#urlencoded-parsing>).
    for (const pair of host.__tbParseParams(input)) {
      __tbArray.push(__tbParamsState(this).pairs, [pair[0], pair[1]]);
    }
  }
  get size() { return __tbParamsState(this).pairs.length; }
  get(name) {
    name = host.__tbUSVString(name);
    const pairs = __tbParamsState(this).pairs;
    for (let index = 0; index < pairs.length; index++) {
      const pair = pairs[index];
      if (pair[0] === name) return pair[1];
    }
    return null;
  }
  getAll(name) {
    name = host.__tbUSVString(name);
    return __tbArray.map(__tbArray.filter(__tbParamsState(this).pairs, pair => pair[0] === name), pair => pair[1]);
  }
  has(name, value) {
    name = host.__tbUSVString(name);
    if (arguments.length < 2 || value === undefined) return __tbArray.some(__tbParamsState(this).pairs, pair => pair[0] === name);
    value = host.__tbUSVString(value);
    return __tbArray.some(__tbParamsState(this).pairs, pair => pair[0] === name && pair[1] === value);
  }
  append(name, value) {
    __tbArray.push(__tbParamsState(this).pairs, [host.__tbUSVString(name), host.__tbUSVString(value)]);
    if (__tbParamsState(this).sync) __tbParamsState(this).sync(this.toString());
  }
  set(name, value) {
    name = host.__tbUSVString(name);
    value = host.__tbUSVString(value);
    let first = -1;
    for (let index = 0; index < __tbParamsState(this).pairs.length; index++) {
      if (__tbParamsState(this).pairs[index][0] !== name) continue;
      if (first < 0) {
        first = index;
        __tbParamsState(this).pairs[index][1] = value;
      } else {
        __tbArray.remove(__tbParamsState(this).pairs, index);
        index--;
      }
    }
    if (first < 0) __tbArray.push(__tbParamsState(this).pairs, [name, value]);
    if (__tbParamsState(this).sync) __tbParamsState(this).sync(this.toString());
  }
  delete(name, value) {
    name = host.__tbUSVString(name);
    const removeValue = !(arguments.length < 2 || value === undefined);
    if (removeValue) value = host.__tbUSVString(value);
    // Splice in place: an iterator over this object must observe mutations
    // (<https://url.spec.whatwg.org/#urlsearchparams-iterate>).
    for (let index = __tbParamsState(this).pairs.length - 1; index >= 0; index--) {
      const pair = __tbParamsState(this).pairs[index];
      if (pair[0] === name && (!removeValue || pair[1] === value)) __tbArray.remove(__tbParamsState(this).pairs, index);
    }
    if (__tbParamsState(this).sync) __tbParamsState(this).sync(this.toString());
  }
  sort() {
    // Array.prototype.sort is stable, matching the spec's sort
    // (<https://url.spec.whatwg.org/#dom-urlsearchparams-sort>).
    __tbArray.sort(__tbParamsState(this).pairs, (a, b) => a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0);
    if (__tbParamsState(this).sync) __tbParamsState(this).sync(this.toString());
  }
  // https://webidl.spec.whatwg.org/#dfn-iterator-object-next
  entries() {
    const pairs = __tbParamsState(this).pairs;
    let index = 0;
    return {
      next() {
        if (index >= pairs.length) return { value: undefined, done: true };
        const pair = pairs[index++];
        return { value: [pair[0], pair[1]], done: false };
      },
      [Symbol.iterator]() { return this; },
    };
  }
  keys() { return __tbArray.iterator(__tbArray.map(__tbParamsState(this).pairs, pair => pair[0])); }
  values() { return __tbArray.iterator(__tbArray.map(__tbParamsState(this).pairs, pair => pair[1])); }
  forEach(callback, thisArg) {
    const pairs = __tbParamsState(this).pairs;
    for (let index = 0; index < pairs.length; index++) {
      const pair = pairs[index];
      __tbApply(callback, thisArg, [pair[1], pair[0], this]);
    }
  }
  toString() {
    // The `application/x-www-form-urlencoded` serializer
    // (<https://url.spec.whatwg.org/#concept-urlencoded-serializer>).
    return host.__tbSerializeParams(__tbParamsState(this).pairs);
  }
  [Symbol.iterator]() { return this.entries(); }
};
Object.defineProperty(globalThis.URLSearchParams.prototype, Symbol.toStringTag, { value: 'URLSearchParams', writable: false, enumerable: false, configurable: true });
