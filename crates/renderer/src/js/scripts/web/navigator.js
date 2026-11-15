// Navigator identity exposed by the browsing-context global.
// https://html.spec.whatwg.org/multipage/system-state.html#the-navigator-object
{
  const identity = host.__tbIdentity();
  const uaBrands = Object.freeze(identity.brands.map((entry) => Object.freeze(entry)));
  const uaFullVersionList = Object.freeze(identity.fullVersionList.map((entry) => Object.freeze(entry)));
  const uaMobile = identity.mobile;
  const uaPlatform = identity.platform;
  // `navigator.languages` is a FrozenArray of the identity's language list, in
  // preference order, and `language` is its first tag
  // https://html.spec.whatwg.org/multipage/system-state.html#dom-navigator-languages
  const languages = Object.freeze(identity.languages.slice());
  // `uaFullVersion` reports the engine version, so the Chromium brand wins; an
  // identity without one falls back to its last brand rather than to nothing
  // https://wicg.github.io/ua-client-hints/#getHighEntropyValues
  const chromium = uaFullVersionList.find((entry) => entry.brand === 'Chromium');
  const lastFullVersion = uaFullVersionList[uaFullVersionList.length - 1];
  const uaFullVersion = chromium !== undefined
    ? chromium.version
    : (lastFullVersion === undefined ? '' : lastFullVersion.version);

  // `NavigatorUAData` is generated from its IDL shape. Its global stays
  // conditional because tinybrowser exposes it only where `userAgentData`
  // itself is available; the installer's `__tbInstallGlobal` form is what a
  // `[SecureContext]` interface needs, and `NavigatorUAData` is not one.
  const [NavigatorUAData, userAgentData] = __tbInstallInterface(class NavigatorUAData {
    get brands() {
      return uaBrands;
    }
    get mobile() {
      return uaMobile;
    }
    get platform() {
      return uaPlatform;
    }
    // https://wicg.github.io/ua-client-hints/#getHighEntropyValues
    getHighEntropyValues(hints) {
      const values = { brands: uaBrands, mobile: uaMobile, platform: uaPlatform };
      if (hints.includes('architecture')) values.architecture = identity.architecture;
      if (hints.includes('bitness')) values.bitness = identity.bitness;
      if (hints.includes('formFactors')) values.formFactors = identity.formFactors.slice();
      if (hints.includes('fullVersionList')) values.fullVersionList = uaFullVersionList;
      if (hints.includes('model')) values.model = identity.model;
      if (hints.includes('platformVersion')) values.platformVersion = identity.platformVersion;
      if (hints.includes('uaFullVersion')) values.uaFullVersion = uaFullVersion;
      if (hints.includes('wow64')) values.wow64 = identity.wow64;
      return Promise.resolve(values);
    }
    // https://wicg.github.io/ua-client-hints/#h-tojson
    toJSON() {
      return { brands: uaBrands, mobile: uaMobile, platform: uaPlatform };
    }
  });

  // https://html.spec.whatwg.org/multipage/interaction.html#dom-useractivation
  const userActivationKey = host.slots('UserActivation');
  class UserActivation {
    constructor() {
      throw new TypeError('Illegal constructor');
    }
    get isActive() {
      __tbBrand(this, userActivationKey);
      return host.__tbUserActivationActive();
    }
    get hasBeenActive() {
      __tbBrand(this, userActivationKey);
      return host.__tbUserActivationSticky();
    }
  }
  Object.defineProperty(UserActivation.prototype, Symbol.toStringTag, {
    value: 'UserActivation', writable: false, enumerable: false, configurable: true,
  });
  const userActivation = Object.create(UserActivation.prototype);
  userActivationKey.set(userActivation, Object.freeze({}));

  // https://html.spec.whatwg.org/multipage/system-state.html#safelisted-scheme
  const protocolHandlerSafelist = new Set([
    'bitcoin', 'ftp', 'ftps', 'geo', 'im', 'irc', 'ircs', 'magnet', 'mailto',
    'matrix', 'mms', 'news', 'nntp', 'openpgp4fpr', 'sftp', 'sip', 'sms',
    'smsto', 'ssh', 'tel', 'urn', 'webcal', 'wtai', 'xmpp',
  ]);
  // The spec says ASCII lowercase, not `toLowerCase`: the Kelvin sign and
  // other non-ASCII characters must not become an ASCII letter
  // (<https://infra.spec.whatwg.org/#ascii-lowercase>).
  const asciiLowercase = value => {
    let result = '';
    for (let index = 0; index < value.length; index++) {
      const code = value.charCodeAt(index);
      result += code >= 0x41 && code <= 0x5a ? String.fromCharCode(code + 0x20) : value[index];
    }
    return result;
  };
  // https://html.spec.whatwg.org/multipage/system-state.html#normalize-protocol-handler-parameters
  const normalizeProtocolHandler = (scheme, url) => {
    const normalizedScheme = asciiLowercase(scheme);
    if (!protocolHandlerSafelist.has(normalizedScheme)
        && !/^web\+[a-z]+$/.test(normalizedScheme)) {
      throw new DOMException('The scheme is not allowed.', 'SecurityError');
    }
    if (url.indexOf('%s') === -1) {
      throw new DOMException('The URL does not contain a %s placeholder.', 'SyntaxError');
    }
    // Encoding-parsing the URL relative to the entry settings object; a
    // placeholder in the host or port makes the parse fail
    // (<https://html.spec.whatwg.org/multipage/system-state.html#normalize-protocol-handler-parameters>).
    const resolved = host.__tbResolveUrl(url, undefined);
    if (resolved == null) {
      throw new DOMException('The URL is invalid.', 'SyntaxError');
    }
    const record = new URL(resolved);
    if ((record.protocol !== 'http:' && record.protocol !== 'https:')
        || record.origin !== host.__tbOrigin()) {
      throw new DOMException('The URL must be same-origin.', 'SecurityError');
    }
    return { key: normalizedScheme + ' ' + resolved, scheme: normalizedScheme, url: resolved };
  };
  // The user agent's registered handlers for this realm.
  const protocolHandlers = new Map();

  // The UA renders no PDF content, so `pdfViewerEnabled` is false and both
  // plugin lists are empty. A Window has one `PluginArray` and one
  // `MimeTypeArray`, and every `Navigator` of that Window returns the same
  // object
  // (<https://html.spec.whatwg.org/multipage/system-state.html#navigatorplugins>,
  // <https://html.spec.whatwg.org/multipage/system-state.html#pdf-viewer-plugin-objects-list>).
  //
  // Both interfaces are legacy platform objects with named properties, which
  // the binding generator's prototype does not synthesize, so they are written
  // here like the other legacy collections
  // (<https://webidl.spec.whatwg.org/#es-legacy-platform-objects>). An empty
  // list has no named or indexed property to find, so every `item` and
  // `namedItem` answer is null.
  const pluginArrayBrand = host.slots('tinybrowser.PluginArray');
  const mimeTypeArrayBrand = host.slots('tinybrowser.MimeTypeArray');
  const checkBrand = (slot, receiver) => {
    if (!slot.has(receiver)) throw new TypeError('Illegal invocation');
  };
  function PluginArray() {
    throw new TypeError('Illegal constructor');
  }
  PluginArray.prototype = Object.create(Object.prototype, {
    constructor: { value: PluginArray, writable: true, configurable: true },
    length: {
      get() { checkBrand(pluginArrayBrand, this); return 0; },
      enumerable: true, configurable: true,
    },
    item: {
      // https://html.spec.whatwg.org/multipage/system-state.html#dom-pluginarray-item
      value: function item(index) {
        checkBrand(pluginArrayBrand, this);
        return null;
      },
      writable: true, enumerable: true, configurable: true,
    },
    namedItem: {
      // https://html.spec.whatwg.org/multipage/system-state.html#dom-pluginarray-nameditem
      value: function namedItem(name) {
        checkBrand(pluginArrayBrand, this);
        return null;
      },
      writable: true, enumerable: true, configurable: true,
    },
    refresh: {
      // https://html.spec.whatwg.org/multipage/system-state.html#dom-pluginarray-refresh
      value: function refresh() {
        checkBrand(pluginArrayBrand, this);
      },
      writable: true, enumerable: true, configurable: true,
    },
  });
  __tbDefineProperty(PluginArray.prototype, Symbol.toStringTag, {
    value: 'PluginArray', writable: false, enumerable: false, configurable: true,
  });
  __tbDefineProperty(PluginArray, 'length', { value: 0, configurable: true });
  const pluginArray = Object.create(PluginArray.prototype);
  pluginArrayBrand.set(pluginArray, true);

  function MimeTypeArray() {
    throw new TypeError('Illegal constructor');
  }
  MimeTypeArray.prototype = Object.create(Object.prototype, {
    constructor: { value: MimeTypeArray, writable: true, configurable: true },
    length: {
      get() { checkBrand(mimeTypeArrayBrand, this); return 0; },
      enumerable: true, configurable: true,
    },
    item: {
      // https://html.spec.whatwg.org/multipage/system-state.html#dom-mimetypearray-item
      value: function item(index) {
        checkBrand(mimeTypeArrayBrand, this);
        return null;
      },
      writable: true, enumerable: true, configurable: true,
    },
    namedItem: {
      // https://html.spec.whatwg.org/multipage/system-state.html#dom-mimetypearray-nameditem
      value: function namedItem(name) {
        checkBrand(mimeTypeArrayBrand, this);
        return null;
      },
      writable: true, enumerable: true, configurable: true,
    },
  });
  __tbDefineProperty(MimeTypeArray.prototype, Symbol.toStringTag, {
    value: 'MimeTypeArray', writable: false, enumerable: false, configurable: true,
  });
  __tbDefineProperty(MimeTypeArray, 'length', { value: 0, configurable: true });
  const mimeTypeArray = Object.create(MimeTypeArray.prototype);
  mimeTypeArrayBrand.set(mimeTypeArray, true);

  // `Navigator` includes seven NavigatorID/Language/etc. mixins, so the
  // generator resolves `userAgentData` from `Navigator includes NavigatorUA`
  // and applies its `[SecureContext]` exposure per member.
  const [Navigator, navigator] = __tbInstallGlobal('Navigator', class Navigator {
    get userAgent() {
      return identity.userAgent;
    }
    get appVersion() { return this.userAgent.replace(/^Mozilla\//, ''); }
    get appCodeName() { return 'Mozilla'; }
    get appName() { return 'Netscape'; }
    get product() { return 'Gecko'; }
    get productSub() { return identity.productSub; }
    get vendor() { return identity.vendor; }
    get vendorSub() { return ''; }
    get platform() { return identity.navigatorPlatform; }
    get language() { return languages[0]; }
    get languages() { return languages; }
    get onLine() { return true; }
    get hardwareConcurrency() { return 1; }
    get maxTouchPoints() { return 0; }
    get cookieEnabled() { return true; }
    get webdriver() { return false; }
    get userActivation() {
      return userActivation;
    }
    get userAgentData() {
      return userAgentData;
    }
    // https://html.spec.whatwg.org/multipage/system-state.html#dom-navigator-registerprotocolhandler
    registerProtocolHandler(scheme, url) {
      const handler = normalizeProtocolHandler(scheme, url);
      protocolHandlers.set(handler.key, handler);
    }
    // https://html.spec.whatwg.org/multipage/system-state.html#dom-navigator-unregisterprotocolhandler
    unregisterProtocolHandler(scheme, url) {
      const handler = normalizeProtocolHandler(scheme, url);
      protocolHandlers.delete(handler.key);
    }
  }, [
    'adAuctionComponents', 'attribution', 'audioSession', 'bluetooth',
    'canShare', 'clearAppBadge', 'clipboard', 'connection', 'contacts',
    'cpuPerformance', 'createHandwritingRecognizer', 'credentials',
    'crossOriginStorage', 'deprecatedReplaceInURN', 'deprecatedURNtoURL',
    'deviceMemory', 'devicePosture', 'geolocation', 'getAutoplayPolicy',
    'getBattery', 'getGamepads', 'getInstalledRelatedApps',
    'globalPrivacyControl', 'gpu', 'hid', 'ink', 'install',
    'javaEnabled', 'keyboard', 'locks', 'login', 'managed',
    'mediaCapabilities', 'mediaDevices', 'mediaSession', 'mimeTypes', 'ml',
    'oscpu', 'pdfViewerEnabled', 'permissions', 'plugins', 'preferences',
    'presentation', 'queryHandwritingRecognizer',
    'requestMediaKeySystemAccess',
    'requestMIDIAccess', 'scheduling', 'sendBeacon', 'serial',
    'serviceWorker', 'setAppBadge', 'share', 'storage', 'storageBuckets',
    'taintEnabled', 'usb', 'vibrate',
    'virtualKeyboard', 'wakeLock', 'windowControlsOverlay', 'xr',
  ]);

  // `sendBeacon`'s `BodyInit` union is not lowered by the binding generator
  // yet, so the operation is installed here with the prototype shape the
  // generator gives an operation: a brand check and the IDL string conversion
  // (<https://w3c.github.io/beacon/#sec-sendBeacon-method>).
  if (host.__tbIsWorker !== true) {
    const navigatorBrand = host.slots('webidl.Navigator');
    // The quota covers the requests this navigator's document has in flight,
    // and a finished request returns its bytes
    // (<https://w3c.github.io/beacon/#sec-sendBeacon-method>, "the amount of
    // data that can be queued ... in-flight quota restrictions").
    const beaconQuota = host.slots('tinybrowser.beacon.quota');
    const quotaFor = navigator => {
      let quota = beaconQuota.get(navigator);
      if (quota === undefined) {
        quota = { bytes: 0 };
        beaconQuota.set(navigator, quota);
      }
      return quota;
    };
    const sendBeacon = function sendBeacon(url, data) {
      if (arguments.length < 1) {
        throw new TypeError("Failed to execute 'sendBeacon' on 'Navigator': 1 argument required, but only 0 present.");
      }
      if (!navigatorBrand.has(this)) throw new TypeError('Illegal invocation');
      const quota = quotaFor(this);
      // WebIDL converts both arguments before the algorithm runs. The
      // `BodyInit` union is (ReadableStream or XMLHttpRequestBodyInit), so any
      // other object fails the union conversion here; a `ReadableStream`
      // reaches the keepalive extraction step, which throws because a
      // keepalive request cannot have a stream body
      // (<https://webidl.spec.whatwg.org/#es-union>,
      // <https://fetch.spec.whatwg.org/#concept-bodyinit-extract>).
      const isObject = data !== null && (typeof data === 'object' || typeof data === 'function');
      if (isObject && !(data instanceof ReadableStream) && !(data instanceof Blob)
          && !(data instanceof FormData) && !(data instanceof URLSearchParams)
          && !(data instanceof ArrayBuffer) && !ArrayBuffer.isView(data)) {
        throw new TypeError('The provided value is not a valid BodyInit.');
      }
      // `url` is a USVString resolved against the entry settings object, and
      // only `http` and `https` are beacon schemes
      // (<https://w3c.github.io/beacon/#sec-sendBeacon-method>, step 3).
      const resolved = host.__tbResolveUrl(__tbIDLUSVString(url), undefined);
      if (resolved == null) throw new TypeError('Invalid URL');
      if (!/^https?:/i.test(resolved)) {
        throw new TypeError('The URL scheme is not http or https.');
      }
      // A keepalive request cannot have a stream body, so a `ReadableStream`
      // in the union throws at extraction
      // (<https://fetch.spec.whatwg.org/#concept-bodyinit-extract>).
      if (data instanceof ReadableStream) {
        throw new TypeError('The provided body is a ReadableStream, which keepalive cannot send.');
      }
      const extracted = __tbExtractBody(data === undefined ? null : data);
      const bytes = extracted.bytes === null ? new __tbUint8Array(0) : extracted.bytes;
      // The quota covers the keepalive requests this document has in flight,
      // and a finished request returns its bytes
      // (<https://w3c.github.io/beacon/#sec-sendBeacon-method>, step 5; the
      // limit itself is Fetch's keepalive quota).
      if (quota.bytes + bytes.length > 65536) return false;
      quota.bytes += bytes.length;
      // A CORS-safelisted content type keeps the request in `no-cors` mode;
      // any other content type needs a CORS request
      // (<https://w3c.github.io/beacon/#sec-sendBeacon-method>, step 6.3).
      let mode = 'no-cors';
      if (extracted.type !== null
          && host.__tbIsNoCorsSafelistedHeader('content-type', extracted.type) !== true) {
        mode = 'cors';
      }
      const hash = resolved.indexOf('#');
      const headers = new Headers();
      if (extracted.type !== null) headers.set('content-type', extracted.type);
      __tbQueuePageRequest(
        hash === -1 ? resolved : resolved.slice(0, hash),
        'POST', bytes, headers, {
          mode,
          credentials: 'include',
          redirect: 'follow',
          keepalive: true,
          bodySize: bytes.length,
          integrity: '',
          cache: '',
          delivery: 'buffer',
          referrerPolicy: '',
        }, 'about:client',
        () => { quota.bytes -= Math.min(quota.bytes, bytes.length); }, 'beacon');
      return true;
    };
    __tbDefineProperty(sendBeacon, 'length', {
      __proto__: null, value: 1, configurable: true,
    });
    __tbDefineProperty(Navigator.prototype, 'sendBeacon', {
      __proto__: null, value: sendBeacon, writable: true, enumerable: true, configurable: true,
    });
  }

  // `NavigatorPlugins` is Window-only and all three members read state this
  // realm already owns, so they install on the generated prototype here. A
  // worker's `Navigator` has none of them
  // (<https://html.spec.whatwg.org/multipage/system-state.html#navigatorplugins>).
  if (host.__tbIsWorker !== true) {
    const navigatorBrand = host.slots('webidl.Navigator');
    const attribute = (name, read) => {
      const getter = function() {
        if (!navigatorBrand.has(this)) throw new TypeError('Illegal invocation');
        return read();
      };
      __tbDefineProperty(getter, 'name', {
        __proto__: null, value: 'get ' + name, configurable: true,
      });
      __tbDefineProperty(Navigator.prototype, name, {
        __proto__: null, get: getter, enumerable: true, configurable: true,
      });
    };
    // `[SameObject]`: the Window's one array answers every read
    // (<https://html.spec.whatwg.org/multipage/system-state.html#dom-navigator-plugins>).
    attribute('plugins', () => pluginArray);
    attribute('mimeTypes', () => mimeTypeArray);
    // The UA has no PDF viewer, so the flag is false and both arrays are empty
    // (<https://html.spec.whatwg.org/multipage/system-state.html#dom-navigator-pdfviewerenabled>).
    attribute('pdfViewerEnabled', () => false);
    for (const [name, value] of [['PluginArray', PluginArray], ['MimeTypeArray', MimeTypeArray]]) {
      __tbDefineProperty(globalThis, name, {
        __proto__: null, value, writable: true, configurable: true,
      });
    }
  }

  if (host.__tbSecureContext === true) {
    Object.defineProperty(globalThis, 'NavigatorUAData', {
      value: NavigatorUAData, writable: true, configurable: true,
    });
  }
  Object.defineProperty(globalThis, 'UserActivation', {
    value: UserActivation, writable: true, configurable: true,
  });
  Object.defineProperty(globalThis, 'navigator', {
    value: navigator, writable: false, configurable: true,
  });
}
