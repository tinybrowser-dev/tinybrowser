// https://xhr.spec.whatwg.org/#interface-formdata
(function() {
  const lists = host.slots();

  // https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#constructing-the-form-data-set
  const constructingEntryList = new WeakSet();

  function FormData(form, submitter) {
    if (!(this instanceof FormData)) {
      throw new TypeError('Class constructor FormData cannot be invoked without new');
    }
    const list = [];
    lists.set(this, list);
    if (form !== undefined && form !== null) {
      if (constructingEntryList.has(form)) return;
      constructingEntryList.add(form);
      try {
        const flat = host.__tbFormEntries(
          form, submitter === undefined || submitter === null ? null : submitter);
        for (let index = 0; index + 1 < flat.length; index += 2) {
          __tbArray.push(list, [String(flat[index]), flat[index + 1]]);
        }
        // https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#constructing-the-form-data-set
        if (typeof globalThis.FormDataEvent === 'function') {
          form.dispatchEvent(new globalThis.FormDataEvent('formdata', {
            formData: this, bubbles: true, cancelable: false,
          }));
        }
      } finally {
        constructingEntryList.delete(form);
      }
    }
  }

  const entryList = receiver => {
    const list = lists.get(receiver);
    if (list === undefined) {
      throw new TypeError('Illegal invocation');
    }
    return list;
  };

  // https://xhr.spec.whatwg.org/#create-an-entry
  const createEntry = (value, filename) => {
    if (globalThis.Blob && value instanceof globalThis.Blob) {
      if (globalThis.File && value instanceof globalThis.File) return value;
      return new globalThis.File(
        [value],
        filename === undefined ? 'blob' : String(filename),
        { type: value.type },
      );
    }
    return String(value);
  };

  Object.defineProperty(FormData.prototype, 'append', {
    value: function(name, value, filename) {
      __tbArray.push(entryList(this), [String(name), createEntry(value, filename)]);
    },
    writable: true, enumerable: true, configurable: true,
  });

  Object.defineProperty(FormData.prototype, 'delete', {
    value: function(name) {
      const key = String(name);
      const list = entryList(this);
      for (let index = list.length - 1; index >= 0; index--) {
        if (list[index][0] === key) __tbArray.remove(list, index);
      }
    },
    writable: true, enumerable: true, configurable: true,
  });

  Object.defineProperty(FormData.prototype, 'get', {
    value: function(name) {
      const key = String(name);
      const list = entryList(this);
      for (let index = 0; index < list.length; index++) {
        const entry = list[index];
        if (entry[0] === key) return entry[1];
      }
      return null;
    },
    writable: true, enumerable: true, configurable: true,
  });

  Object.defineProperty(FormData.prototype, 'getAll', {
    value: function(name) {
      const key = String(name);
      const values = [];
      const list = entryList(this);
      for (let index = 0; index < list.length; index++) {
        const entry = list[index];
        if (entry[0] === key) __tbArray.push(values, entry[1]);
      }
      return values;
    },
    writable: true, enumerable: true, configurable: true,
  });

  Object.defineProperty(FormData.prototype, 'has', {
    value: function(name) {
      const key = String(name);
      return __tbArray.some(entryList(this), entry => entry[0] === key);
    },
    writable: true, enumerable: true, configurable: true,
  });

  Object.defineProperty(FormData.prototype, 'set', {
    value: function(name, value, filename) {
      const key = String(name);
      const entry = [key, createEntry(value, filename)];
      const list = entryList(this);
      // https://xhr.spec.whatwg.org/#dom-formdata-set
      let first = -1;
      for (let index = 0; index < list.length; index++) {
        if (list[index][0] === key) { first = index; break; }
      }
      if (first === -1) {
        __tbArray.push(list, entry);
        return;
      }
      list[first] = entry;
      for (let index = list.length - 1; index > first; index--) {
        if (list[index][0] === key) __tbArray.remove(list, index);
      }
    },
    writable: true, enumerable: true, configurable: true,
  });

  Object.defineProperty(FormData.prototype, 'forEach', {
    value: function(callback, thisArg) {
      if (typeof callback !== 'function') {
        throw new TypeError('callback is not a function');
      }
      const list = __tbArray.map(entryList(this), entry => entry);
      for (let index = 0; index < list.length; index++) {
        const entry = list[index];
        __tbApply(callback, thisArg, [entry[1], entry[0], this]);
      }
    },
    writable: true, enumerable: true, configurable: true,
  });

  const iterator = (receiver, kind) => {
    const list = __tbArray.map(entryList(receiver), entry => entry);
    let index = 0;
    const result = {
      next() {
        if (index >= list.length) return { value: undefined, done: true };
        const entry = list[index++];
        let value;
        if (kind === 'keys') value = entry[0];
        else if (kind === 'values') value = entry[1];
        else value = [entry[0], entry[1]];
        return { value: value, done: false };
      },
    };
    result[Symbol.iterator] = function() { return this; };
    return result;
  };

  for (const [name, kind] of [['entries', 'entries'], ['keys', 'keys'], ['values', 'values']]) {
    Object.defineProperty(FormData.prototype, name, {
      value: function() { return iterator(this, kind); },
      writable: true, enumerable: true, configurable: true,
    });
  }
  Object.defineProperty(FormData.prototype, Symbol.iterator, {
    value: FormData.prototype.entries,
    writable: true, enumerable: false, configurable: true,
  });
  Object.defineProperty(FormData.prototype, Symbol.toStringTag, {
    value: 'FormData', writable: false, enumerable: false, configurable: true,
  });
  Object.defineProperty(FormData, 'prototype', { value: FormData.prototype, writable: false });
  Object.defineProperty(globalThis, 'FormData', {
    value: FormData, writable: true, enumerable: false, configurable: true,
  });
})();
