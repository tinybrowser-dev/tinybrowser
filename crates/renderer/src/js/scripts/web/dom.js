// https://html.spec.whatwg.org/multipage/dom.html#domstringmap
const __tbDOMStringMapPrototype = Object.create(Object.prototype);
const __tbDOMStringMap = function () {
  throw new TypeError('Illegal constructor');
};
Object.defineProperty(__tbDOMStringMap, 'name', {
  __proto__: null, value: 'DOMStringMap', configurable: true,
});
Object.defineProperty(__tbDOMStringMap, 'length', {
  __proto__: null, value: 0, configurable: true,
});
Object.defineProperty(__tbDOMStringMap, 'prototype', {
  __proto__: null, value: __tbDOMStringMapPrototype, writable: false,
});
Object.defineProperty(__tbDOMStringMapPrototype, 'constructor', {
  __proto__: null, value: __tbDOMStringMap, writable: true, configurable: true,
});
Object.defineProperty(__tbDOMStringMapPrototype, Symbol.toStringTag, {
  __proto__: null, value: 'DOMStringMap', configurable: true,
});
Object.defineProperty(globalThis, 'DOMStringMap', {
  __proto__: null, value: __tbDOMStringMap, writable: true, configurable: true,
});

// https://html.spec.whatwg.org/multipage/dom.html#dom-domstringmap-nameditem
const __tbDatasetAttribute = property => {
  if (/-[a-z]/.test(property)) return null;
  return 'data-' + property.replace(/[A-Z]/g, letter => '-' + letter.toLowerCase());
};

host.__tbMakeDataset = element => new Proxy(Object.create(__tbDOMStringMapPrototype), {
  // https://webidl.spec.whatwg.org/#LegacyOverrideBuiltIns
  get(target, property, receiver) {
    if (typeof property === 'string') {
      const name = __tbDatasetAttribute(property);
      if (name !== null) {
        const value = element.getAttribute(name);
        if (value !== null) return value;
      }
    }
    return Reflect.get(target, property, receiver);
  },
  set(target, property, value, receiver) {
    if (typeof property !== 'string') return Reflect.set(target, property, value, receiver);
    // https://webidl.spec.whatwg.org/#legacy-platform-object-set
    if (/-[a-z]/.test(property)) throw new DOMException('invalid dataset property', 'SyntaxError');
    const name = 'data-' + property.replace(/[A-Z]/g, letter => '-' + letter.toLowerCase());
    // https://html.spec.whatwg.org/multipage/dom.html#dom-domstringmap-setitem
    element.setAttributeNS(null, name, String(value));
    return true;
  },
  // https://webidl.spec.whatwg.org/#legacy-platform-object-delete
  deleteProperty(target, property) {
    if (typeof property === 'string') {
      const name = __tbDatasetAttribute(property);
      if (name !== null) {
        element.removeAttribute(name);
        return true;
      }
    }
    return Reflect.deleteProperty(target, property);
  },
  has(target, property) {
    if (typeof property === 'string') {
      const name = __tbDatasetAttribute(property);
      if (name !== null && element.hasAttribute(name)) return true;
    }
    return Reflect.has(target, property);
  },
  ownKeys() {
    return element.getAttributeNames().filter(name =>
      name.startsWith('data-') && !/[A-Z]/.test(name.slice(5))
    ).map(name => name.slice(5).replace(/-([a-z])/g, (_match, letter) => letter.toUpperCase()));
  },
  getOwnPropertyDescriptor(target, property) {
    if (typeof property === 'string') {
      const name = __tbDatasetAttribute(property);
      if (name !== null) {
        const value = element.getAttribute(name);
        if (value !== null) {
          return { configurable: true, enumerable: true, writable: true, value };
        }
      }
    }
    return Reflect.getOwnPropertyDescriptor(target, property);
  }
});

// https://drafts.csswg.org/cssom/#the-cssstyledeclaration-interface
const __tbSupportedProperties = new Map();
host.__tbMakeStyle = element => {
  const names = () => host.__tb_inline_style_property_names(element) || [];
  // https://drafts.csswg.org/cssom/#css-property-to-idl-attribute — accepts webkit-prefixed dashed names without the leading dash, matching shipped browsers
  const supported = name => {
    let answer = __tbSupportedProperties.get(name);
    if (answer === undefined) {
      answer = host.__tb_style_property_supported(name);
      __tbSupportedProperties.set(name, answer);
    }
    return answer;
  };
  const serialized = () => host.__tb_inline_style_css_text(element) ?? '';
  // https://drafts.csswg.org/cssom/#update-style-attribute-for
  const sync = () => {
    const text = serialized();
    const current = element.getAttribute('style');
    if (current === text) return;
    if (current === null && text === '') return;
    element.setAttribute('style', text);
  };
  // https://drafts.csswg.org/cssom/#dom-cssstyledeclaration-getpropertyvalue
  const propertyName = name => {
    const text = String(name);
    return text.startsWith('--') ? text : text.toLowerCase();
  };
  // https://drafts.csswg.org/cssom/#dom-cssstyleproperties-cssfloat
  // https://drafts.csswg.org/cssom/#css-property-to-idl-attribute — accepts webkit-prefixed dashed names without the leading dash, matching shipped browsers
  const idlProperty = name => {
    if (name === 'cssFloat') return 'float';
    if (name === 'cssText' || name === 'length' || name === 'parentRule') return null;
    const dashed = name.replace(/[A-Z]/g, letter => '-' + letter.toLowerCase());
    return dashed.startsWith('webkit-') ? `-${dashed}` : dashed;
  };
  const declaration = {
    get cssText() { return serialized(); },
    set cssText(value) {
      // https://drafts.csswg.org/cssom/#dom-cssstyledeclaration-csstext
      host.__tb_inline_style_set_css_text(element, value == null ? '' : String(value));
      // https://drafts.csswg.org/cssom/#dom-cssstyledeclaration-csstext
      element.setAttribute('style', serialized());
    },
    // https://drafts.csswg.org/cssom/#the-cssstyledeclaration-interface
    toString() { return serialized(); },
    get length() { return names().length; },
    item(index) {
      if (arguments.length < 1) throw new TypeError('item requires 1 argument');
      return names()[Number(index) >>> 0] ?? '';
    },
    getPropertyValue(name) {
      const found = host.__tb_inline_style_get_property(element, propertyName(name));
      return found ? found.value : '';
    },
    getPropertyPriority(name) {
      const found = host.__tb_inline_style_get_property(element, propertyName(name));
      return found && found.important ? 'important' : '';
    },
    setProperty(name, value, priority) {
      const property = propertyName(name);
      // https://webidl.spec.whatwg.org/#LegacyNullToEmptyString
      const text = value === null ? '' : String(value);
      if (text === '') {
        this.removeProperty(property);
        return;
      }
      // https://drafts.csswg.org/cssom/#dom-cssstyledeclaration-setproperty
      const raw = priority == null ? '' : String(priority);
      const important = raw.toLowerCase() === 'important';
      if (raw !== '' && !important) return;
      if (host.__tb_inline_style_set_property(element, property, text, important)) sync();
    },
    removeProperty(name) {
      const property = propertyName(name);
      const old = this.getPropertyValue(property);
      if (host.__tb_inline_style_remove_property(element, property)) sync();
      return old;
    },
    get parentRule() { return null; },
  };
  const own = property => Object.prototype.hasOwnProperty.call(declaration, property);
  const indexProperty = property => /^(0|[1-9][0-9]*)$/.test(property);
  // https://webidl.spec.whatwg.org/#idl-iterable
  const declarationMember = property => {
    if (property !== Symbol.iterator && property !== 'keys' && property !== 'values'
        && property !== 'entries' && property !== 'forEach') {
      return undefined;
    }
    const prototype = globalThis.CSSStyleDeclaration?.prototype;
    return prototype === undefined ? undefined : Reflect.get(prototype, property);
  };
  return new Proxy(declaration, {
    get(target, property, receiver) {
      if (typeof property !== 'string') {
        const member = declarationMember(property);
        if (member !== undefined) return member.bind(target);
        return Reflect.get(target, property, receiver);
      }
      if (own(property)) {
        const value = Reflect.get(target, property, receiver);
        return typeof value === 'function' ? value.bind(target) : value;
      }
      // https://drafts.csswg.org/cssom/#the-cssstyledeclaration-interface
      if (indexProperty(property)) return names()[Number(property)];
      const member = declarationMember(property);
      if (member !== undefined) return member.bind(target);
      // https://drafts.csswg.org/cssom/#css-property-to-idl-attribute — accepts webkit-prefixed dashed names without the leading dash, matching shipped browsers
      const idl = idlProperty(property);
      if (idl === null || !supported(idl)) return undefined;
      return target.getPropertyValue(idl);
    },
    set(target, property, value, receiver) {
      if (typeof property !== 'string' || own(property)) {
        return Reflect.set(target, property, value, receiver);
      }
      const idl = idlProperty(property);
      if (idl !== null) target.setProperty(idl, value);
      return true;
    },
    has(target, property) {
      if (typeof property !== 'string') {
        return declarationMember(property) !== undefined || Reflect.has(target, property);
      }
      if (own(property)) return true;
      // https://drafts.csswg.org/cssom/#the-cssstyledeclaration-interface
      if (indexProperty(property)) return Number(property) < names().length;
      if (declarationMember(property) !== undefined) return true;
      // https://drafts.csswg.org/cssom/#css-property-to-idl-attribute — accepts webkit-prefixed dashed names without the leading dash, matching shipped browsers
      const idl = idlProperty(property);
      return idl !== null && supported(idl);
    },
    ownKeys(target) {
      const indices = names().map((_name, index) => String(index));
      return indices.concat(Reflect.ownKeys(target));
    },
    getOwnPropertyDescriptor(target, property) {
      if (typeof property === 'string' && indexProperty(property)) {
        const index = Number(property);
        if (index < names().length) {
          return {
            value: names()[index], writable: false, enumerable: true, configurable: true,
          };
        }
        return undefined;
      }
      return Reflect.getOwnPropertyDescriptor(target, property);
    },
    deleteProperty(target, property) {
      if (typeof property === 'string' && !own(property)) {
        const idl = idlProperty(property);
        if (idl !== null) target.removeProperty(idl);
        return true;
      }
      return Reflect.deleteProperty(target, property);
    },
  });
};

// https://html.spec.whatwg.org/multipage/embedded-content.html#dom-image
globalThis.Image = function Image(width, height) {
  const image = globalThis.document.createElement('img');
  if (width !== undefined) {
    image.width = width;
  }
  if (height !== undefined) {
    image.height = height;
  }
  return image;
};

// https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-document-parsehtmlunsafe
// https://wicg.github.io/sanitizer-api/#dom-document-parsehtml
globalThis.Document.parseHTMLUnsafe = function parseHTMLUnsafe(html) {
  return host.__tbParseHTMLDocument(html);
};

// https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-element-sethtmlunsafe
{
  const setHTMLUnsafe = function setHTMLUnsafe(html, options) {
    const markup = __tbIDLString(html);
    // https://webidl.spec.whatwg.org/#es-dictionary
    let runScripts = false;
    if (options !== undefined && options !== null) {
      if (typeof options !== 'object' && typeof options !== 'function') {
        throw new TypeError('setHTMLUnsafe options must be an object');
      }
      // https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#sethtmlunsafeoptions
      runScripts = options.runScripts === undefined ? false : Boolean(options.runScripts);
    }
    return host.__tbSetHTMLUnsafe(this, markup, runScripts);
  };
  for (const prototype of [globalThis.Element.prototype, globalThis.ShadowRoot.prototype]) {
    Object.defineProperty(prototype, 'setHTMLUnsafe', {
      value: setHTMLUnsafe,
      writable: true,
      enumerable: true,
      configurable: true,
    });
  }
}

// https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#html-insertion-methods
{
  const define = (prototype, name, position) => {
    Object.defineProperty(prototype, name, {
      value: function(html, options) {
        const markup = __tbIDLString(html);
        // https://webidl.spec.whatwg.org/#es-dictionary
        let runScripts = false;
        if (options !== undefined && options !== null) {
          if (typeof options !== 'object' && typeof options !== 'function') {
            throw new TypeError(name + ' options must be an object');
          }
          // https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#sethtmlunsafeoptions
          runScripts = options.runScripts === undefined ? false : Boolean(options.runScripts);
        }
        return host.__tbInsertHTMLUnsafe(this, markup, position, runScripts);
      },
      writable: true,
      enumerable: true,
      configurable: true,
    });
  };

  // https://dom.spec.whatwg.org/#interface-parentnode
  // https://dom.spec.whatwg.org/#interface-childnode
  const parents = [globalThis.Element.prototype, globalThis.ShadowRoot.prototype];
  for (const prototype of parents) {
    define(prototype, 'appendHTMLUnsafe', 'append');
    define(prototype, 'prependHTMLUnsafe', 'prepend');
  }
  const children = parents.concat([globalThis.CharacterData.prototype]);
  for (const prototype of children) {
    define(prototype, 'beforeHTMLUnsafe', 'before');
    define(prototype, 'afterHTMLUnsafe', 'after');
    define(prototype, 'replaceWithHTMLUnsafe', 'replaceWith');
  }
}

// https://html.spec.whatwg.org/multipage/obsolete.html#dom-document-applets
// https://html.spec.whatwg.org/multipage/common-dom-interfaces.html#htmlallcollection
{
  const HTMLCollectionPrototype = globalThis.HTMLCollection.prototype;
  const canonicalIndex = property => {
    if (!/^(?:0|[1-9][0-9]*)$/.test(property)) return false;
    const index = Number(property);
    return Number.isSafeInteger(index) && String(index) === property && index < 4294967295;
  };

  const appletsCollections = new WeakMap();
  const appletsOf = document => {
    let collection = appletsCollections.get(document);
    if (collection === undefined) {
      collection = Object.create(HTMLCollectionPrototype);
      Object.defineProperties(collection, {
        length: { value: 0, enumerable: false, configurable: true },
        item: {
          value: function item() { return null; },
          writable: true, enumerable: true, configurable: true,
        },
        namedItem: {
          value: function namedItem() { return null; },
          writable: true, enumerable: true, configurable: true,
        },
        [Symbol.iterator]: {
          value: function* () {},
          writable: true, enumerable: false, configurable: true,
        },
      });
      appletsCollections.set(document, collection);
    }
    return collection;
  };
  Object.defineProperty(globalThis.Document.prototype, 'applets', {
    get() { return appletsOf(this); },
    enumerable: true, configurable: true,
  });

  // https://html.spec.whatwg.org/multipage/common-dom-interfaces.html#htmlcollection
  const liveCollection = getList => new Proxy(Object.create(HTMLCollectionPrototype), {
    get(target, property) {
      const list = getList();
      if (property === 'length') return list.length;
      if (property === 'item') return index => list[index >>> 0] ?? null;
      if (property === 'namedItem') return name => {
        const key = String(name);
        return list.find(element => element.getAttribute('id') === key
          || element.getAttribute('name') === key) ?? null;
      };
      if (property === Symbol.iterator) return function* () { yield* list; };
      if (typeof property === 'string' && canonicalIndex(property)) {
        return list[Number(property)];
      }
      return Reflect.get(HTMLCollectionPrototype, property, target);
    },
    getOwnPropertyDescriptor(target, property) {
      const list = getList();
      if (property === 'length') {
        return { value: list.length, writable: false, enumerable: false, configurable: true };
      }
      if (typeof property === 'string' && canonicalIndex(property)) {
        const index = Number(property);
        if (index < list.length) {
          return { value: list[index], writable: false, enumerable: true, configurable: true };
        }
      }
      return undefined;
    },
    getPrototypeOf() { return HTMLCollectionPrototype; },
    has(target, property) {
      const list = getList();
      if (typeof property === 'string' && canonicalIndex(property)) {
        return Number(property) < list.length;
      }
      return property in HTMLCollectionPrototype;
    },
  });

  // https://html.spec.whatwg.org/multipage/common-dom-interfaces.html#htmlallcollection
  const allNamed = new Set([
    'a', 'button', 'embed', 'form', 'frame', 'frameset', 'iframe', 'img',
    'input', 'map', 'meta', 'object', 'select', 'textarea',
  ]);
  const allElements = document => Array.from(document.getElementsByTagName('*'));
  const namedMatches = (document, name) => allElements(document).filter(element => {
    if (element.getAttribute('id') === name) return true;
    return allNamed.has(element.localName) && element.getAttribute('name') === name;
  });

  const interfaceObject = function HTMLAllCollection() {
    throw new TypeError('Illegal constructor');
  };
  Object.defineProperty(interfaceObject.prototype, Symbol.toStringTag, {
    value: 'HTMLAllCollection', writable: false, enumerable: false, configurable: true,
  });

  const allCollections = new WeakMap();
  const allOf = document => {
    let collection = allCollections.get(document);
    if (collection !== undefined) return collection;
    const elements = () => allElements(document);
    const named = name => {
      const matches = namedMatches(document, name);
      if (matches.length === 0) return null;
      if (matches.length === 1) return matches[0];
      return liveCollection(() => namedMatches(document, name));
    };
    // https://html.spec.whatwg.org/multipage/common-dom-interfaces.html#concept-get-all-indexed-or-named
    const indexedOrNamed = nameOrIndex => {
      if (canonicalIndex(nameOrIndex)) {
        const list = elements();
        return Number(nameOrIndex) < list.length ? list[Number(nameOrIndex)] : null;
      }
      return named(nameOrIndex);
    };
    // https://html.spec.whatwg.org/multipage/common-dom-interfaces.html#htmlallcollection-call
    const target = function () {
      if (arguments.length === 0 || arguments[0] === undefined) return null;
      return indexedOrNamed(String(arguments[0]));
    };
    const prototype = interfaceObject.prototype;
    collection = new Proxy(target, {
      get(_target, property) {
        if (property === 'length') return elements().length;
        if (property === 'item') return nameOrIndex => {
          if (nameOrIndex === undefined) return null;
          return indexedOrNamed(String(nameOrIndex));
        };
        if (property === 'namedItem') return name => named(String(name));
        if (property === Symbol.iterator) return function* () { yield* elements(); };
        if (typeof property === 'string') {
          if (canonicalIndex(property)) {
            const list = elements();
            return Number(property) < list.length ? list[Number(property)] : undefined;
          }
          const value = named(property);
          if (value !== null) return value;
        }
        return Reflect.get(prototype, property, _target);
      },
      getOwnPropertyDescriptor(_target, property) {
        if (property === 'length') {
          return {
            value: elements().length, writable: false, enumerable: false, configurable: true,
          };
        }
        if (typeof property === 'string' && canonicalIndex(property)) {
          const list = elements();
          const index = Number(property);
          if (index < list.length) {
            return { value: list[index], writable: false, enumerable: true, configurable: true };
          }
        }
        return undefined;
      },
      getPrototypeOf() { return prototype; },
      has(_target, property) {
        if (typeof property === 'string') {
          if (canonicalIndex(property)) return Number(property) < elements().length;
          return named(property) !== null;
        }
        return property in prototype;
      },
    });
    // https://html.spec.whatwg.org/multipage/common-dom-interfaces.html#htmlallcollection
    host.__tbMarkHTMLDDA(collection);
    allCollections.set(document, collection);
    return collection;
  };
  Object.defineProperty(globalThis.Document.prototype, 'all', {
    get() { return allOf(this); },
    enumerable: true, configurable: true,
  });
}

// https://html.spec.whatwg.org/multipage/dom.html#dom-document-lastmodified
{
  const two = value => String(value).padStart(2, '0');
  Object.defineProperty(globalThis.Document.prototype, 'lastModified', {
    get() {
      const now = new Date();
      return `${two(now.getMonth() + 1)}/${two(now.getDate())}/`
        + `${String(now.getFullYear()).padStart(4, '0')} `
        + `${two(now.getHours())}:${two(now.getMinutes())}:${two(now.getSeconds())}`;
    },
    enumerable: true, configurable: true,
  });
}

// https://html.spec.whatwg.org/multipage/scripting.html#dom-script-supports
if (globalThis.HTMLScriptElement) {
  Object.defineProperty(globalThis.HTMLScriptElement, 'supports', {
    value: function supports(type) {
      if (arguments.length < 1) {
        throw new TypeError("Failed to execute 'supports' on 'HTMLScriptElement': 1 argument required, but only 0 present.");
      }
      const value = String(type);
      return value === 'classic' || value === 'module' || value === 'importmap';
    },
    writable: true,
    enumerable: true,
    configurable: true,
  });
}
