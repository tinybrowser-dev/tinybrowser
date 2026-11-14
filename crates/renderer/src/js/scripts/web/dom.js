// `dataset` returns a `DOMStringMap`, whose supported property names come from
// the element's `data-*` attributes. The interface object exists so
// `dataset instanceof DOMStringMap` works and pages can extend
// `DOMStringMap.prototype`; it is not constructible and every instance is one
// of these proxies
// (<https://html.spec.whatwg.org/multipage/dom.html#domstringmap>).
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

// The content attribute a dataset property name names, or null when the name
// is not one the element's name-value pairs can produce: a U+002D followed by
// an ASCII lower alpha never appears in a pair name
// (<https://html.spec.whatwg.org/multipage/dom.html#dom-domstringmap-nameditem>).
const __tbDatasetAttribute = property => {
  if (/-[a-z]/.test(property)) return null;
  return 'data-' + property.replace(/[A-Z]/g, letter => '-' + letter.toLowerCase());
};

host.__tbMakeDataset = element => new Proxy(Object.create(__tbDOMStringMapPrototype), {
  // Named properties come from the attributes, before the prototype chain,
  // because `DOMStringMap` has `[LegacyOverrideBuiltIns]`
  // (<https://webidl.spec.whatwg.org/#LegacyOverrideBuiltIns>).
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
    // The named setter runs for every string property, whether or not the name
    // is a supported property name yet
    // (<https://webidl.spec.whatwg.org/#legacy-platform-object-set>).
    if (/-[a-z]/.test(property)) throw new DOMException('invalid dataset property', 'SyntaxError');
    const name = 'data-' + property.replace(/[A-Z]/g, letter => '-' + letter.toLowerCase());
    element.setAttribute(name, String(value));
    return true;
  },
  // Only a name that a pair can produce reaches the deleter; any other name
  // deletes an ordinary property instead
  // (<https://webidl.spec.whatwg.org/#legacy-platform-object-delete>).
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

// The inline declaration block behind `ElementCSSInlineStyle.style`
// (<https://drafts.csswg.org/cssom/#the-cssstyledeclaration-interface>). Every
// read and write goes through the style engine's parsed block, so values
// serialize the way CSSOM requires instead of echoing the attribute text, and
// shorthands expand to the longhands the indexed getter reports.
const __tbSupportedProperties = new Map();
host.__tbMakeStyle = element => {
  const names = () => host.__tb_inline_style_property_names(element) || [];
  // The supported-property set cannot change once the realm starts, so one
  // answer per name is enough
  // (<https://drafts.csswg.org/cssom/#css-property-to-idl-attribute>).
  const supported = name => {
    let answer = __tbSupportedProperties.get(name);
    if (answer === undefined) {
      answer = host.__tb_style_property_supported(name);
      __tbSupportedProperties.set(name, answer);
    }
    return answer;
  };
  const serialized = () => host.__tb_inline_style_css_text(element) ?? '';
  // "Update style attribute for the CSS declaration block": the attribute's
  // value becomes the serialized block
  // (<https://drafts.csswg.org/cssom/#update-style-attribute-for>). Skipping an
  // unchanged value keeps a failed `setProperty` from rewriting the attribute.
  const sync = () => {
    const text = serialized();
    const current = element.getAttribute('style');
    if (current === text) return;
    if (current === null && text === '') return;
    element.setAttribute('style', text);
  };
  // A non-custom property name is ASCII lowercase; a custom property name is
  // case-sensitive
  // (<https://drafts.csswg.org/cssom/#dom-cssstyledeclaration-getpropertyvalue>).
  const propertyName = name => {
    const text = String(name);
    return text.startsWith('--') ? text : text.toLowerCase();
  };
  // The IDL attribute for a CSS property: camel case becomes dashed, except
  // `cssFloat`, the one property whose IDL name is not `float`
  // (<https://drafts.csswg.org/cssom/#dom-cssstyleproperties-cssfloat>).
  // A `-webkit-` property also has a webkit-cased IDL name whose leading dash
  // is dropped, so `-webkit-animation` is `webkitAnimation`. Browsers also
  // accept the dashed spelling without the leading dash; that is wider than
  // the spec's IDL names, and follows the browsers
  // (<https://drafts.csswg.org/cssom/#css-property-to-idl-attribute>).
  const idlProperty = name => {
    if (name === 'cssFloat') return 'float';
    if (name === 'cssText' || name === 'length' || name === 'parentRule') return null;
    const dashed = name.replace(/[A-Z]/g, letter => '-' + letter.toLowerCase());
    return dashed.startsWith('webkit-') ? `-${dashed}` : dashed;
  };
  const declaration = {
    get cssText() { return serialized(); },
    set cssText(value) {
      // The setter empties the block, parses the value, and always updates the
      // attribute, even when nothing parses
      // (<https://drafts.csswg.org/cssom/#dom-cssstyledeclaration-csstext>).
      host.__tb_inline_style_set_css_text(element, value == null ? '' : String(value));
      // The setter always updates the attribute, even when the serialized
      // value is unchanged, so an attribute observer sees the mutation
      // (<https://drafts.csswg.org/cssom/#dom-cssstyledeclaration-csstext>).
      element.setAttribute('style', serialized());
    },
    // `stringifier attribute CSSOMString cssText` gives every declaration
    // object a `toString` that answers its serialization
    // (<https://drafts.csswg.org/cssom/#the-cssstyledeclaration-interface>).
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
      // `null` is the empty string; `undefined` becomes "undefined", which no
      // property accepts, so setting a property to undefined has no effect
      // (<https://webidl.spec.whatwg.org/#LegacyNullToEmptyString>).
      const text = value === null ? '' : String(value);
      if (text === '') {
        this.removeProperty(property);
        return;
      }
      // Any priority other than the empty string or "important" makes the call
      // a no-op (<https://drafts.csswg.org/cssom/#dom-cssstyledeclaration-setproperty>).
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
  return new Proxy(declaration, {
    get(target, property, receiver) {
      if (typeof property !== 'string') return Reflect.get(target, property, receiver);
      if (own(property)) {
        const value = Reflect.get(target, property, receiver);
        return typeof value === 'function' ? value.bind(target) : value;
      }
      // The indexed getter yields a property name, and `undefined` past the
      // last declaration (<https://drafts.csswg.org/cssom/#the-cssstyledeclaration-interface>).
      if (indexProperty(property)) return names()[Number(property)];
      // Only a supported property has an IDL attribute; an unknown name is
      // absent rather than an empty value
      // (<https://drafts.csswg.org/cssom/#css-property-to-idl-attribute>).
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
      if (typeof property !== 'string') return Reflect.has(target, property);
      if (own(property)) return true;
      // The indexed getter is present up to `length - 1`
      // (<https://drafts.csswg.org/cssom/#the-cssstyledeclaration-interface>).
      if (indexProperty(property)) return Number(property) < names().length;
      // `in` asks whether the UA supports the property, not whether the block
      // currently sets it; an unset property still has its IDL attribute
      // (<https://drafts.csswg.org/cssom/#css-property-to-idl-attribute>).
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

// `Document.parseHTMLUnsafe` is a static operation; the WebIDL generator does
// not emit those yet, so it installs here. It parses a new document with the
// caller's environment settings object's URL and scripting disabled, exactly
// like `DOMParser.parseFromString`
// (<https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-document-parsehtmlunsafe>).
// `Document.parseHTML` stays absent until a `Sanitizer` can back its safe
// default configuration: exposing it as an alias of the unsafe parser would
// hand unsanitized markup to code that feature-detects it
// (<https://wicg.github.io/sanitizer-api/#dom-document-parsehtml>).
globalThis.Document.parseHTMLUnsafe = function parseHTMLUnsafe(html) {
  return host.__tbParseHTMLDocument(html);
};

// `setHTMLUnsafe` is a partial-interface operation on `Element` and
// `ShadowRoot` whose `(TrustedHTML or DOMString)` argument and
// `SetHTMLUnsafeOptions` dictionary are unions the WebIDL generator does not
// lower yet, so it installs here. The host owns the receiver check and the
// insertion; this only runs the conversions
// (<https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-element-sethtmlunsafe>).
{
  const setHTMLUnsafe = function setHTMLUnsafe(html, options) {
    const markup = __tbIDLString(html);
    // A dictionary argument accepts `undefined` and `null` as the empty
    // dictionary; anything else that is not an object is a TypeError
    // (<https://webidl.spec.whatwg.org/#es-dictionary>).
    let runScripts = false;
    if (options !== undefined && options !== null) {
      if (typeof options !== 'object' && typeof options !== 'function') {
        throw new TypeError('setHTMLUnsafe options must be an object');
      }
      // `runScripts` defaults to false and converts as a boolean; a sanitizer
      // is not supported yet, so it is not read (docs/limitations.md)
      // (<https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#sethtmlunsafeoptions>).
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

// The partial-update insertion methods are `ParentNode`'s
// `appendHTMLUnsafe`/`prependHTMLUnsafe` and `ChildNode`'s
// `beforeHTMLUnsafe`/`afterHTMLUnsafe`/`replaceWithHTMLUnsafe`. Their
// `(TrustedHTML or DOMString)` argument and `SetHTMLUnsafeOptions` dictionary
// are unions the WebIDL generator does not lower yet, so they install here;
// the host owns the receiver and parent rules and the tree work
// (<https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#html-insertion-methods>).
{
  const define = (prototype, name, position) => {
    Object.defineProperty(prototype, name, {
      value: function(html, options) {
        const markup = __tbIDLString(html);
        // A dictionary argument accepts `undefined` and `null` as the empty
        // dictionary; anything else that is not an object is a TypeError
        // (<https://webidl.spec.whatwg.org/#es-dictionary>).
        let runScripts = false;
        if (options !== undefined && options !== null) {
          if (typeof options !== 'object' && typeof options !== 'function') {
            throw new TypeError(name + ' options must be an object');
          }
          // `runScripts` defaults to false and converts as a boolean; a
          // sanitizer is not supported yet, so it is not read
          // (docs/limitations.md)
          // (<https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#sethtmlunsafeoptions>).
          runScripts = options.runScripts === undefined ? false : Boolean(options.runScripts);
        }
        return host.__tbInsertHTMLUnsafe(this, markup, position, runScripts);
      },
      writable: true,
      enumerable: true,
      configurable: true,
    });
  };

  // `ParentNode`'s methods are on `Element` and `ShadowRoot`; `ChildNode`'s
  // are on `Element` and `CharacterData`
  // (<https://dom.spec.whatwg.org/#interface-parentnode>,
  // <https://dom.spec.whatwg.org/#interface-childnode>).
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
