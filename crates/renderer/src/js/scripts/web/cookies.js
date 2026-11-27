// https://cookiestore.spec.whatwg.org/#cookie-store-interface
(function() {
  'use strict';
  // https://cookiestore.spec.whatwg.org/#cookie-store-interface
  if (host.__tbSecureContext !== true) {
    return;
  }

  // https://cookiestore.spec.whatwg.org/#normalize-a-cookie-name-or-value
  const kNormalizeEdges = /^[\t ]+|[\t ]+$/g;
  // https://cookiestore.spec.whatwg.org/#set-a-cookie
  const kUnsafePair = /[;\u0000-\u0008\u000A-\u001F\u007F]/;
  const kMaxNameValuePair = 4096;
  const kMaxAttribute = 1024;
  const kLongMin = -(2 ** 63);
  const kLongExclusiveMax = 2 ** 63;

  const kCreationUrl = new URL(host.__tbCookieStoreCreationUrl());
  const kApiBaseUrl = new URL(host.__tbCookieStoreApiBaseUrl());
  const kOpaqueOrigin = host.__tbOpaqueOrigin === true;

  function normalize(value) {
    return value.replace(kNormalizeEdges, '');
  }

  // https://webidl.spec.whatwg.org/#es-USVString
  function usv(value) {
    return host.__tbUSVString(value);
  }

  // https://cookiestore.spec.whatwg.org/#cookie-maximum-name/value-pair-size
  function utf8Length(value) {
    return host.__tbUtf8Encode(value).length;
  }

  // https://datatracker.ietf.org/doc/html/rfc6265#section-5.1.4
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

  // https://cookiestore.spec.whatwg.org/#set-a-cookie
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

  // https://cookiestore.spec.whatwg.org/#dom-cookiestore-getall-options
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

  // https://datatracker.ietf.org/doc/html/rfc6265#section-4.1.1
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

  // https://cookiestore.spec.whatwg.org/#observable-changes
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
    // https://cookiestore.spec.whatwg.org/#cookie-list-item
    return list.map(cookie => ({name: cookie.name, value: cookie.value}));
  }

  // https://cookiestore.spec.whatwg.org/#set-a-cookie
  function buildCookie(url, name, value, expires, domain, path, sameSite,
                       partitioned, maxAge) {
    // https://webidl.spec.whatwg.org/#es-enumeration
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
      // https://webidl.spec.whatwg.org/#es-double
      const millis = Number(expires);
      if (!Number.isFinite(millis)) {
        throw new TypeError('expires is not a finite number');
      }
      expires = Math.trunc(millis);
    } else {
      expires = null;
    }
    if (maxAge !== null && maxAge !== undefined) {
      // https://webidl.spec.whatwg.org/#es-long-long
      const age = Number(maxAge);
      if (!Number.isFinite(age) || age < kLongMin || age >= kLongExclusiveMax) {
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
    // https://cookiestore.spec.whatwg.org/#set-a-cookie
    // https://cookiestore.spec.whatwg.org/#process-cookie-changes
    host.__tbCookieStoreSet(cookie);
  }

  // https://cookiestore.spec.whatwg.org/#observable-changes
  class CookieChangeEvent extends Event {
    constructor(type, init = {}) {
      super(usv(type), init);
      this.__changed = Object.freeze([]);
      this.__deleted = Object.freeze([]);
      if (init.changed !== undefined) {
        // https://cookiestore.spec.whatwg.org/#cookiechangeevent
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

  // https://cookiestore.spec.whatwg.org/#process-cookie-changes
  let observed = kOpaqueOrigin ? new Map() : snapshotCookies();

  // https://cookiestore.spec.whatwg.org/#prepare-lists
  function cookieChangeLists(before, after) {
    const changed = [];
    const deleted = [];
    for (const [key, cookie] of after) {
      const old = before.get(key);
      if (old === undefined || old.state !== cookie.state) {
        changed.push({name: cookie.name, value: cookie.value});
      }
    }
    for (const [key, cookie] of before) {
      if (!after.has(key)) {
        // https://cookiestore.spec.whatwg.org/#prepare-lists
        deleted.push({name: cookie.name, value: undefined});
      }
    }
    return {changed, deleted};
  }

  // https://cookiestore.spec.whatwg.org/#process-cookie-changes
  function processCookieChanges() {
    const after = snapshotCookies();
    const lists = cookieChangeLists(observed, after);
    observed = after;
    if (lists.changed.length === 0 && lists.deleted.length === 0) {
      return;
    }
    // https://cookiestore.spec.whatwg.org/#process-cookie-changes
    Promise.resolve().then(() => {
      host.__tbDispatchTargetTrusted(
          globalThis.cookieStore,
          new CookieChangeEvent('change', lists));
    });
  }

  function cookieStoreOperation(steps) {
    if (kOpaqueOrigin) {
      // https://cookiestore.spec.whatwg.org/#dom-cookiestore-get
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
    // https://cookiestore.spec.whatwg.org/#window-cookiestore
    // https://html.spec.whatwg.org/multipage/webappapis.html#event-handler-attributes
    get onchange() {
      return this.__onchange === undefined ? null : this.__onchange;
    }

    set onchange(handler) {
      this.__onchange = typeof handler === 'function' ? handler : null;
    }

    // https://webidl.spec.whatwg.org/#overload-resolution-algorithm
    get(nameOrOptions = undefined) {
      return cookieStoreOperation(() => {
        if (nameOrOptions === undefined || nameOrOptions === null ||
            isObject(nameOrOptions)) {
          const options = dictionary(nameOrOptions);
          if (options.name === undefined && options.url === undefined) {
            // https://cookiestore.spec.whatwg.org/#dom-cookiestore-get
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
        // https://cookiestore.spec.whatwg.org/#dom-cookiestore-delete — delete(name) does not force partitioned: true, matching Chrome and Firefox
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
        // https://cookiestore.spec.whatwg.org/#delete-a-cookie
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

  // https://cookiestore.spec.whatwg.org/#window-cookiestore
  const store = new CookieStore();
  Object.defineProperty(globalThis, 'cookieStore', {
    value: store,
    enumerable: true,
    configurable: true,
    writable: false,
  });

  if (!kOpaqueOrigin) {
    host.__tbCookieStoreProcessChanges = processCookieChanges;
  }
})();
