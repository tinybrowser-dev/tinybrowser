// https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-domparser-parsefromstring
(function() {
  const Native = globalThis.DOMParser;
  const parsers = host.slots('DOMParser');
  class DOMParser {
    constructor() {
      parsers.set(this, new Native());
    }
    parseFromString(source, type) {
      const parser = parsers.get(this);
      if (parser === undefined) throw new TypeError('Illegal invocation');
      return parser.parseFromString(source, type);
    }
  }
  Object.defineProperty(globalThis, 'DOMParser', {
    value: DOMParser, writable: true, configurable: true,
  });
})();
