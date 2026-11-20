(function() {
  const windowNamedValue = host.__tbWindowNamedValue;
  const windowNamedHas = host.__tbWindowNamedHas;
  delete host.__tbWindowNamedValue;
  delete host.__tbWindowNamedHas;
  const native = globalThis.NodeList.prototype;
  // https://dom.spec.whatwg.org/#interface-nodelist — NodeList uses Array.prototype iterator methods, matching Chromium, WebKit, and WPT
  // https://webidl.spec.whatwg.org/#legacy-platform-object-getownproperty
  {
    const lengthDesc = Object.getOwnPropertyDescriptor(native, 'length');
    const nativeLength = lengthDesc.get;
    const nativeItem = native.item;
    class StaticNodeList extends function(list) { return list; } {
      #length;
      constructor(list, length) {
        super(list);
        this.#length = length;
      }
      static {
        Object.defineProperty(native, 'length', {
          get: function() {
            if (#length in this) return this.#length;
            return __tbApply(nativeLength, this, []);
          },
          enumerable: lengthDesc.enumerable,
          configurable: lengthDesc.configurable,
        });
        Object.defineProperty(native, 'item', {
          value: function item(index) {
            if (#length in this) {
              const value = this[index >>> 0];
              return value === undefined ? null : value;
            }
            return __tbApply(nativeItem, this, arguments);
          },
          writable: true,
          enumerable: true,
          configurable: true,
        });
      }
    }
    host.__tbFinishStaticNodeList = function(list, length) {
      new StaticNodeList(list, length);
    };
  }
  // https://webidl.spec.whatwg.org/#es-iterable
  function installValueIterable(ctor) {
    const proto = ctor.prototype;
    const lengthGet = Object.getOwnPropertyDescriptor(proto, 'length').get;
    const item = proto.item;
    const iteratorPrototype = Object.create(Object.prototype);
    Object.defineProperty(iteratorPrototype, Symbol.toStringTag, {
      value: ctor.name + ' Iterator', writable: false, enumerable: false, configurable: true,
    });
    iteratorPrototype[Symbol.iterator] = function() { return this; };
    function brand(collection) {
      if (!(collection instanceof ctor)) throw new TypeError('Illegal invocation');
      return collection;
    }
    function lengthOf(collection) {
      return __tbApply(lengthGet, collection, []);
    }
    function valueAt(collection, index) {
      return __tbApply(item, collection, [index]);
    }
    function iterator(next) {
      const object = Object.create(iteratorPrototype);
      object.next = next;
      return object;
    }
    function values() {
      const collection = brand(this);
      let index = 0;
      return iterator(function() {
        if (index >= lengthOf(collection)) return { value: undefined, done: true };
        return { value: valueAt(collection, index++), done: false };
      });
    }
    function keys() {
      const collection = brand(this);
      let index = 0;
      return iterator(function() {
        if (index >= lengthOf(collection)) return { value: undefined, done: true };
        return { value: index++, done: false };
      });
    }
    function entries() {
      const collection = brand(this);
      let index = 0;
      return iterator(function() {
        if (index >= lengthOf(collection)) return { value: undefined, done: true };
        const current = index++;
        return { value: [current, valueAt(collection, current)], done: false };
      });
    }
    function forEach(callback, thisArg) {
      const collection = brand(this);
      if (typeof callback !== 'function') throw new TypeError('not a function');
      for (let index = 0; index < lengthOf(collection); index++) {
        __tbApply(callback, thisArg, [valueAt(collection, index), index, collection]);
      }
    }
    for (const [name, value] of [['values', values], ['keys', keys], ['entries', entries], ['forEach', forEach]]) {
      Object.defineProperty(proto, name, {
        value, writable: true, enumerable: true, configurable: true,
      });
    }
    Object.defineProperty(proto, Symbol.iterator, {
      value: values,
      writable: true,
      configurable: true,
    });
  }
  // https://webidl.spec.whatwg.org/#es-iterable
  // https://dom.spec.whatwg.org/#interface-nodelist — NodeList uses Array.prototype iterator methods, matching Chromium, WebKit, and WPT
  {
    const proto = globalThis.NodeList.prototype;
    for (const name of ['values', 'keys', 'entries', 'forEach']) {
      Object.defineProperty(proto, name, {
        value: Array.prototype[name],
        writable: true,
        enumerable: true,
        configurable: true,
      });
    }
    Object.defineProperty(proto, Symbol.iterator, {
      value: Array.prototype[Symbol.iterator],
      writable: true,
      configurable: true,
    });
  }
  installValueIterable(globalThis.DOMTokenList);
  function values() {
    let index = 0;
    const collection = this;
    return {
      next: function() {
        if (index >= collection.length) return { value: undefined, done: true };
        return { value: collection.item(index++), done: false };
      },
      [Symbol.iterator]: function() { return this; }
    };
  }
  Object.defineProperty(globalThis.NamedNodeMap.prototype, Symbol.iterator, {
    value: values,
    writable: true,
    configurable: true,
  });
  for (const [collectionName, collectionProto] of [
    ['NodeList', native],
    ['NamedNodeMap', globalThis.NamedNodeMap.prototype],
    ['DOMTokenList', globalThis.DOMTokenList.prototype],
  ]) {
    Object.defineProperty(collectionProto, Symbol.toStringTag, {
      value: collectionName, writable: false, enumerable: false, configurable: true,
    });
  }
  const ctor = globalThis.HTMLCollection;
  const proto = ctor.prototype;
  Object.defineProperty(proto, Symbol.iterator, {
    value: values,
    writable: true,
    configurable: true,
  });
  Object.defineProperty(proto, Symbol.toStringTag, {
    value: 'HTMLCollection', writable: false, enumerable: false, configurable: true,
  });
  for (const name of ['options', 'selectedOptions']) {
    const nativeGetter = Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, name).get;
    const cache = host.slots();
    Object.defineProperty(HTMLSelectElement.prototype, name, {
      get: function() {
        let collection = cache.get(this);
        if (collection === undefined) {
          collection = nativeGetter.call(this);
          cache.set(this, collection);
        }
        return collection;
      },
      enumerable: true, configurable: true,
    });
  }
  // https://html.spec.whatwg.org/multipage/nav-history-apis.html#named-access-on-the-window-object
  {
    const originalProto = Object.getPrototypeOf(globalThis);
    const target = Object.create(originalProto);
    Object.setPrototypeOf(globalThis, new Proxy(target, {
      get: function(inner, property, receiver) {
        if (typeof property === 'string' && !Reflect.has(inner, property)
            && windowNamedHas(property)) {
          const value = windowNamedValue(property);
          if (value !== undefined) return value;
        }
        return Reflect.get(inner, property, receiver);
      },
      has: function(inner, property) {
        if (typeof property === 'string' && !Reflect.has(inner, property)
            && windowNamedHas(property)) {
          return windowNamedValue(property) !== undefined;
        }
        return Reflect.has(inner, property);
      },
      getOwnPropertyDescriptor: function(inner, property) {
        if (typeof property === 'string' && !Reflect.has(inner, property)
            && windowNamedHas(property)) {
          const value = windowNamedValue(property);
          if (value !== undefined) {
            return { value, writable: true, enumerable: false, configurable: true };
          }
        }
        return Reflect.getOwnPropertyDescriptor(inner, property);
      },
      // https://webidl.spec.whatwg.org/#named-properties-object
      defineProperty: function() { return false; },
      deleteProperty: function() { return false; },
      preventExtensions: function() { return false; },
      setPrototypeOf: function(inner, proto) {
        return proto === Reflect.getPrototypeOf(inner);
      },
    }));
  }
})();
