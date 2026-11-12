// https://cookiestore.spec.whatwg.org/#cookie-store-interface
//
// `CookieStore` is a thin, promise-returning front end over the cookie jar.
// The jar owns storage, retrieval order, path and domain matching, and the
// storage model's refusals. This shim owns the spec's validation, its default
// path computation, and the option overloads, because those refuse an
// operation with a `TypeError` before the jar ever sees it.
(function() {
  'use strict';
  // `[SecureContext]`: the whole interface is absent otherwise
  // (<https://cookiestore.spec.whatwg.org/#cookie-store-interface>).
  if (host.__tbSecureContext !== true) {
    return;
  }

  // To normalize a cookie name or value: remove the U+0009 TAB and U+0020
  // SPACE at either end, and nothing else
  // (<https://cookiestore.spec.whatwg.org/#normalize-a-cookie-name-or-value>).
  const kNormalizeEdges = /^[\t ]+|[\t ]+$/g;
  // U+003B, any C0 control except U+0009 TAB, and U+007F DELETE
  // (<https://cookiestore.spec.whatwg.org/#set-a-cookie> step 3).
  const kUnsafePair = /[;\u0000-\u0008\u000A-\u001F\u007F]/;
  const kMaxNameValuePair = 4096;
  const kMaxAttribute = 1024;
  const kLongMin = -9223372036854775808;
  const kLongMax = 9223372036854775807;

  const kCreationUrl = new URL(host.__tbCookieStoreCreationUrl());
  const kApiBaseUrl = new URL(host.__tbCookieStoreApiBaseUrl());
  const kOpaqueOrigin = host.__tbOpaqueOrigin === true;

  function normalize(value) {
    return value.replace(kNormalizeEdges, '');
  }

  // USVString conversion: a Symbol is a TypeError and a lone surrogate
  // becomes U+FFFD, which `String()` alone would get wrong
  // (<https://webidl.spec.whatwg.org/#es-USVString>).
  function usv(value) {
    return host.__tbUSVString(value);
  }

  // The size limits count UTF-8 bytes, not UTF-16 units
  // (<https://cookiestore.spec.whatwg.org/#cookie-maximum-name/value-pair-size>).
  function utf8Length(value) {
    return host.__tbUtf8Encode(value).length;
  }

  // RFC 6265 §5.1.4's serialized cookie default path
  // (<https://datatracker.ietf.org/doc/html/rfc6265#section-5.1.4>).
  function defaultPath(url) {
    const path = url.pathname;
    if (path === '' || path[0] !== '/') {
      return '/';
    }
    const last = path.lastIndexOf('/');
    if (last <= 0) {
      return '/';
    }
    return path.slice(0, last);
  }

  // Whether `domain` is a registrable domain suffix of, or is equal to,
  // `hostname` (<https://cookiestore.spec.whatwg.org/#set-a-cookie> step 12).
  // Host parsing lowercases and punycodes the option, an IP address has no
  // registrable suffix, and a public suffix is neither.
  function domainMatchesHost(domain, hostname) {
    if (domain === hostname) {
      return true;
    }
    if (host.__tbCookieStoreIsIp(hostname)) {
      return false;
    }
    return hostname.endsWith('.' + domain) &&
        !host.__tbCookieStorePublicSuffix(domain);
  }

  function dictionary(value) {
    if (value === null || value === undefined) {
      return {};
    }
    if (typeof value !== 'object' && typeof value !== 'function') {
      throw new TypeError('dictionary argument expected');
    }
    return value;
  }

  function isObject(value) {
    return value !== null &&
        (typeof value === 'object' || typeof value === 'function');
  }

  // The URL a query runs against. `options.url` may only repeat the document's
  // creation URL, fragment aside
  // (<https://cookiestore.spec.whatwg.org/#dom-cookiestore-getall-options>).
  function validateQueryUrl(options) {
    if (options.url === undefined) {
      return;
    }
    let parsed;
    try {
      parsed = new URL(usv(options.url), kApiBaseUrl);
    } catch (error) {
      throw new TypeError('invalid url');
    }
    const creation = new URL(kCreationUrl.href);
    creation.hash = '';
    const candidate = new URL(parsed.href);
    candidate.hash = '';
    if (creation.href !== candidate.href) {
      throw new TypeError('url does not match the creation URL');
    }
    if (parsed.origin !== kCreationUrl.origin) {
      throw new TypeError('url is not same origin with the creation URL');
    }
  }

  // The host encodes one cookie per line as `slot\0name\0value\0state\n`.
  // None of those fields can hold NUL or LF once the jar accepted the cookie
  // (<https://datatracker.ietf.org/doc/html/rfc6265#section-4.1.1>).
  function decode(records) {
    const list = [];
    if (records === '') {
      return list;
    }
    for (const line of records.split('\n')) {
      if (line === '') {
        continue;
      }
      const fields = line.split('\0');
      list.push({
        slot: fields[0],
        name: fields[1],
        value: fields[2],
        state: fields[3],
      });
    }
    return list;
  }

  // The jar's records keyed by storage slot and name. A cookie with the same
  // name can live in several slots at once, so the slot is part of the key
  // (<https://cookiestore.spec.whatwg.org/#observable-changes>).
  function snapshotCookies() {
    const snapshot = new Map();
    for (const cookie of decode(host.__tbCookieStoreGetAll())) {
      snapshot.set(cookie.slot + '\0' + cookie.name, cookie);
    }
    return snapshot;
  }

  function query(name) {
    const cookies = decode(host.__tbCookieStoreGetAll());
    const list = name === null
        ? cookies
        : cookies.filter(cookie => cookie.name === name);
    // `CookieListItem` exposes exactly the name and the value
    // (<https://cookiestore.spec.whatwg.org/#cookie-list-item>).
    return list.map(cookie => ({name: cookie.name, value: cookie.value}));
  }

  // <https://cookiestore.spec.whatwg.org/#set-a-cookie>
  function buildCookie(url, name, value, expires, domain, path, sameSite,
                       partitioned, maxAge) {
    // The IDL enum conversion runs before the algorithm, so an unknown
    // spelling throws even when the cookie would fail validation later
    // (<https://webidl.spec.whatwg.org/#es-enumeration>).
    sameSite = cookieSameSite(sameSite);
    name = normalize(usv(name));
    value = normalize(usv(value));
    const lowerName = name.toLowerCase();
    if (kUnsafePair.test(name) || kUnsafePair.test(value)) {
      return null;
    }
    if (name.includes('=')) {
      return null;
    }
    if (name.length === 0) {
      const lowerValue = value.toLowerCase();
      if (value.includes('=') || value.length === 0) {
        return null;
      }
      if (lowerValue.startsWith('__host-') ||
          lowerValue.startsWith('__host-http-') ||
          lowerValue.startsWith('__http-') ||
          lowerValue.startsWith('__secure-')) {
        return null;
      }
    }
    if (lowerName.startsWith('__host-http-') ||
        lowerName.startsWith('__http-')) {
      return null;
    }
    if (utf8Length(name) + utf8Length(value) > kMaxNameValuePair) {
      return null;
    }

    const hostname = url.hostname;
    if (domain !== null && domain !== undefined) {
      domain = usv(domain);
      if (domain.startsWith('.')) {
        return null;
      }
      if (lowerName.startsWith('__host-')) {
        return null;
      }
      const parsedDomain = host.__tbCookieStoreParseDomain(domain);
      if (parsedDomain === null ||
          !domainMatchesHost(parsedDomain, hostname)) {
        return null;
      }
      domain = parsedDomain;
      if (utf8Length(domain) > kMaxAttribute) {
        return null;
      }
    } else {
      domain = null;
    }

    if (expires !== null && expires !== undefined) {
      if (maxAge !== null && maxAge !== undefined) {
        return null;
      }
      // `DOMHighResTimeStamp` is an IDL `double`, so a non-finite value is a
      // `TypeError` before the algorithm runs
      // (<https://webidl.spec.whatwg.org/#es-double>).
      const millis = Number(expires);
      if (!Number.isFinite(millis)) {
        throw new TypeError('expires is not a finite number');
      }
      expires = Math.trunc(millis);
    } else {
      expires = null;
    }
    if (maxAge !== null && maxAge !== undefined) {
      // `long long?`: finite, truncated toward zero, and inside the range
      // (<https://webidl.spec.whatwg.org/#es-long-long>).
      const age = Number(maxAge);
      if (!Number.isFinite(age) || age < kLongMin || age > kLongMax) {
        throw new TypeError('maxAge is not a long long');
      }
      maxAge = Math.trunc(age);
    } else {
      maxAge = null;
    }

    if (path === '') {
      path = defaultPath(url);
    }
    if (!path.startsWith('/')) {
      return null;
    }
    if (path !== '/' && lowerName.startsWith('__host-')) {
      return null;
    }
    if (utf8Length(path) > kMaxAttribute) {
      return null;
    }

    return {
      name,
      value,
      expires,
      domain,
      path,
      sameSite,
      partitioned: Boolean(partitioned),
      maxAge,
    };
  }

  function cookieSameSite(value) {
    switch (value) {
      case 'strict':
      case 'lax':
      case 'none':
        return value;
      default:
        throw new TypeError('sameSite is not a CookieSameSite');
    }
  }

  function storeCookie(cookie) {
    if (cookie === null) {
      throw new TypeError('cookie rejected');
    }
    const before = snapshotCookies();
    // The write is typed, not a `Set-Cookie` line: the API's `path` is
    // stored verbatim, and a line would split a `path` holding U+003B (;)
    // into extra attributes
    // (<https://cookiestore.spec.whatwg.org/#set-a-cookie>).
    host.__tbCookieStoreSet(cookie);
    queueCookieChanges(before);
  }

  // `CookieChangeEvent` and the script-side change notification. A cookie
  // change is observed when the cookie is visible to this document's cookie
  // URL, so the snapshot is the jar's script-visible view for that URL
  // (<https://cookiestore.spec.whatwg.org/#observable-changes>).
  class CookieChangeEvent extends Event {
    constructor(type, init = {}) {
      super(usv(type), init);
      this.__changed = Object.freeze([]);
      this.__deleted = Object.freeze([]);
      if (init.changed !== undefined) {
        // `changed` is a `FrozenArray<CookieListItem>`
        // (<https://cookiestore.spec.whatwg.org/#cookiechangeevent>).
        this.__changed = Object.freeze(Array.from(
            init.changed,
            item => ({name: usv(item.name), value: usv(item.value)})));
      }
      if (init.deleted !== undefined) {
        this.__deleted = Object.freeze(Array.from(
            init.deleted,
            item => ({
              name: usv(item.name),
              value: item.value === undefined ? undefined : usv(item.value),
            })));
      }
    }

    get changed() {
      return this.__changed;
    }

    get deleted() {
      return this.__deleted;
    }
  }

  /// Fires one `change` event for the cookies that changed between `before`
  /// and `after`, both captured at the write. A replaced cookie keeps its
  /// slot, so an attribute-only change is seen; a cookie that disappeared
  /// from the slot is a deletion
  /// (<https://cookiestore.spec.whatwg.org/#process-cookie-changes>).
  function fireCookieChanges(before, after) {
    const next = after;
    const changed = [];
    const deleted = [];
    for (const [key, cookie] of next) {
      const old = before.get(key);
      if (old === undefined || old.state !== cookie.state) {
        changed.push({name: cookie.name, value: cookie.value});
      }
    }
    for (const [key, cookie] of before) {
      if (!next.has(key)) {
        // A deleted item's value is undefined
        // (<https://cookiestore.spec.whatwg.org/#prepare-lists>).
        deleted.push({name: cookie.name, value: undefined});
      }
    }
    if (changed.length === 0 && deleted.length === 0) {
      return;
    }
    globalThis.cookieStore.dispatchEvent(
        new CookieChangeEvent('change', {changed, deleted}));
  }

  function queueCookieChanges(before) {
    // The change set is the jar at the write, not at delivery: a later write
    // from a response header must not join this event
    // (<https://cookiestore.spec.whatwg.org/#observable-changes>).
    const after = snapshotCookies();
    // The spec queues a global task on the DOM manipulation task source
    // (<https://cookiestore.spec.whatwg.org/#process-cookie-changes>); this
    // realm has no task source the shim can queue on, so the event is a
    // promise job instead. See docs/limitations.md.
    Promise.resolve().then(() => fireCookieChanges(before, after));
  }

  function cookieStoreOperation(steps) {
    if (kOpaqueOrigin) {
      // <https://cookiestore.spec.whatwg.org/#dom-cookiestore-get>
      return Promise.reject(
          new DOMException('opaque origin', 'SecurityError'));
    }
    try {
      return Promise.resolve(steps());
    } catch (error) {
      return Promise.reject(error);
    }
  }

  class CookieStore extends EventTarget {
    // `[Exposed=Window] attribute EventHandler onchange`
    // (<https://cookiestore.spec.whatwg.org/#window-cookiestore>). This is an
    // event handler attribute, not a listener: the engine reads it at
    // dispatch time through the handler-attribute path and calls it once,
    // after the listener list
    // (<https://html.spec.whatwg.org/multipage/webappapis.html#event-handler-attributes>).
    get onchange() {
      return this.__onchange === undefined ? null : this.__onchange;
    }

    set onchange(handler) {
      this.__onchange = typeof handler === 'function' ? handler : null;
    }

    // `get.length` is 0: the dictionary overload needs no required argument
    // (<https://webidl.spec.whatwg.org/#overload-resolution-algorithm>).
    get(nameOrOptions = undefined) {
      return cookieStoreOperation(() => {
        if (nameOrOptions === undefined || nameOrOptions === null ||
            isObject(nameOrOptions)) {
          const options = dictionary(nameOrOptions);
          if (options.name === undefined && options.url === undefined) {
            // An empty `options` dictionary is a `TypeError`
            // (<https://cookiestore.spec.whatwg.org/#dom-cookiestore-get>).
            throw new TypeError('get() needs a name or url');
          }
          validateQueryUrl(options);
          const list = query(
              options.name === undefined
                  ? null
                  : normalize(usv(options.name)));
          return list.length === 0 ? null : list[0];
        }
        const list = query(normalize(usv(nameOrOptions)));
        return list.length === 0 ? null : list[0];
      });
    }

    getAll(nameOrOptions = undefined) {
      return cookieStoreOperation(() => {
        if (nameOrOptions === undefined || nameOrOptions === null ||
            isObject(nameOrOptions)) {
          const options = dictionary(nameOrOptions);
          validateQueryUrl(options);
          return query(
              options.name === undefined ? null : normalize(usv(options.name)));
        }
        return query(normalize(usv(nameOrOptions)));
      });
    }

    // `set.length` is 1: the dictionary overload needs one required argument.
    set(nameOrOptions, value = undefined) {
      return cookieStoreOperation(() => {
        if (arguments.length >= 2) {
          storeCookie(buildCookie(kCreationUrl, nameOrOptions, value, null,
                                  null, '/', 'strict', false, null));
          return;
        }
        const options = dictionary(nameOrOptions);
        if (options.name === undefined || options.value === undefined) {
          throw new TypeError('name and value are required');
        }
        const path = options.path === undefined ? '/' : usv(options.path);
        storeCookie(buildCookie(
            kCreationUrl, options.name, options.value, options.expires,
            options.domain, path,
            options.sameSite === undefined ? 'strict' : usv(options.sameSite),
            options.partitioned === undefined ? false : options.partitioned,
            options.maxAge));
      });
    }

    delete(nameOrOptions) {
      return cookieStoreOperation(() => {
        let name;
        let domain = null;
        let path = '/';
        // The spec's `delete(name)` step passes `partitioned` as true, but
        // WPT `cookieStore_get_delete_basic` sets the cookie with the
        // positional overload (unpartitioned) and expects this delete to
        // remove it, and both Chrome and Firefox do remove it. Follow them.
        let partitioned = false;
        if (nameOrOptions === undefined || nameOrOptions === null ||
            isObject(nameOrOptions)) {
          const options = dictionary(nameOrOptions);
          if (options.name === undefined) {
            throw new TypeError('name is required');
          }
          name = options.name;
          domain = options.domain === undefined ? null : options.domain;
          path = options.path === undefined ? '/' : usv(options.path);
          partitioned = options.partitioned === undefined
              ? false
              : options.partitioned;
        } else {
          name = nameOrOptions;
        }
        // An empty name needs a non-empty value to reach the storage model
        // (<https://cookiestore.spec.whatwg.org/#delete-a-cookie>).
        const value = normalize(usv(name)).length === 0 ? 'deleted' : '';
        storeCookie(buildCookie(kCreationUrl, name, value, null, domain, path,
                                'strict', partitioned, 0));
      });
    }
  }

  Object.defineProperty(CookieStore.prototype, Symbol.toStringTag, {
    value: 'CookieStore',
    configurable: true,
  });
  Object.defineProperty(CookieChangeEvent.prototype, Symbol.toStringTag, {
    value: 'CookieChangeEvent',
    configurable: true,
  });

  globalThis.CookieStore = CookieStore;
  globalThis.CookieChangeEvent = CookieChangeEvent;

  // `[SameObject] readonly attribute CookieStore cookieStore`
  // (<https://cookiestore.spec.whatwg.org/#window-cookiestore>).
  const store = new CookieStore();
  Object.defineProperty(globalThis, 'cookieStore', {
    value: store,
    enumerable: true,
    configurable: true,
    writable: false,
  });

  // The `document.cookie` setter runs through the earlier cookie shim, which
  // snapshots before the write and calls this after it, so a script-visible
  // change is observed with the same diff a `CookieStore` write uses. An
  // opaque origin has no cookie access at all, so it gets no hook to run.
  if (!kOpaqueOrigin) {
    globalThis.__tbCookieStoreSnapshot = snapshotCookies;
    globalThis.__tbCookieStoreDidWrite = queueCookieChanges;
  }
})();
