// https://html.spec.whatwg.org/multipage/custom-elements.html#custom-elements-core-concepts
{
  const definitions = new __tbPrivateMap();
  const constructors = new __tbPrivateMap();
  const upgraded = host.slots();
  const connected = host.slots();
  // https://dom.spec.whatwg.org/#move
  const atomicMoves = host.slots();
  const pending = new __tbPrivateMap();
  let definitionRunning = false;
  const enqueueReaction = host.__tbEnqueueCustomReaction;
  const pushReactions = host.__tbPushCustomReactions;
  const popReactions = host.__tbPopCustomReactions;
  const noteDefined = host.__tbNoteCustomDefined;
  const nativeApply = Reflect.apply;
  const nativeConstruct = Reflect.construct;
  const MapConstructor = Map;
  const mapGet = Map.prototype.get;
  const mapSet = Map.prototype.set;
  const mapHas = Map.prototype.has;
  const arrayIncludes = Array.prototype.includes;
  const createObject = Object.create;
  const observe = MutationObserver.prototype.observe;
  const callbackNames = ['connectedCallback', 'disconnectedCallback', 'connectedMoveCallback', 'adoptedCallback', 'attributeChangedCallback'];
  const formCallbackNames = ['formAssociatedCallback', 'formResetCallback', 'formDisabledCallback', 'formStateRestoreCallback'];
  // https://html.spec.whatwg.org/multipage/custom-elements.html#valid-custom-element-name
  const reservedNames = ['annotation-xml', 'color-profile', 'font-face', 'font-face-src',
    'font-face-uri', 'font-face-format', 'font-face-name', 'missing-glyph'];
  const getAttribute = Element.prototype.getAttribute;
  const getAttributeNS = Element.prototype.getAttributeNS;
  const hasAttribute = Element.prototype.hasAttribute;
  const localNameGetter = __tbGetOwnPropertyDescriptor(Element.prototype, 'localName').get;
  const connectedGetter = __tbGetOwnPropertyDescriptor(Node.prototype, 'isConnected').get;
  const nodeTypeGetter = __tbGetOwnPropertyDescriptor(Node.prototype, 'nodeType').get;
  const firstChildGetter = __tbGetOwnPropertyDescriptor(Node.prototype, 'firstChild').get;
  const nextSiblingGetter = __tbGetOwnPropertyDescriptor(Node.prototype, 'nextSibling').get;
  const shadowRootGetter = __tbGetOwnPropertyDescriptor(Element.prototype, 'shadowRoot').get;
  const listLengthGetter = __tbGetOwnPropertyDescriptor(NodeList.prototype, 'length').get;
  const listItem = NodeList.prototype.item;
  const recordGetters = __tbObjectCreate(null);
  for (const name of ['type', 'target', 'removedNodes', 'addedNodes', 'attributeName', 'attributeNamespace', 'oldValue']) {
    recordGetters[name] = __tbGetOwnPropertyDescriptor(MutationRecord.prototype, name).get;
  }

  function localNameOf(element) {
    return nativeApply(localNameGetter, element, []);
  }

  function isConnected(element) {
    return nativeApply(connectedGetter, element, []);
  }

  function shadowRootOf(host) {
    return nativeApply(shadowRootGetter, host, []) || null;
  }

  // https://dom.spec.whatwg.org/#concept-shadow-including-tree-order
  function visitElements(root, callback) {
    if (!root) return;
    const type = nativeApply(nodeTypeGetter, root, []);
    if (type === 1) {
      callback(root);
      const shadow = shadowRootOf(root);
      if (shadow) visitElements(shadow, callback);
    } else if (type !== 9 && type !== 11) {
      return;
    }
    for (let child = nativeApply(firstChildGetter, root, []); child; child = nativeApply(nextSiblingGetter, child, [])) {
      visitElements(child, callback);
    }
  }

  function notifyConnected(root) {
    visitElements(root, function(element) {
      if (!upgraded.has(element) || connected.has(element) || !isConnected(element)) return;
      connected.add(element);
      invoke(element, 'connectedCallback', []);
    });
  }

  // https://dom.spec.whatwg.org/#move
  // https://html.spec.whatwg.org/multipage/custom-elements.html#enqueue-a-custom-element-callback-reaction
  function noteConnectedMove(node) {
    if (definitions.size === 0) return;
    atomicMoves.add(node);
    visitElements(node, function(element) {
      if (!upgraded.has(element) || !isConnected(element)) return;
      const callbacks = definitions.get(localNameOf(element))?.callbacks;
      if (!callbacks) return;
      if (typeof callbacks.connectedMoveCallback === 'function') {
        enqueueReaction(element, callbacks.connectedMoveCallback, []);
        return;
      }
      if (typeof callbacks.disconnectedCallback === 'function') {
        enqueueReaction(element, callbacks.disconnectedCallback, []);
      }
      if (typeof callbacks.connectedCallback === 'function') {
        enqueueReaction(element, callbacks.connectedCallback, []);
      }
    });
  }
  host.__tbNoteConnectedMove = noteConnectedMove;

  function notifyDisconnected(root) {
    visitElements(root, function(element) {
      if (!upgraded.has(element) || !connected.has(element)) return;
      connected.delete(element);
      invoke(element, 'disconnectedCallback', []);
    });
  }

  function validName(name) {
    if (typeof name !== 'string' || !__tbApply(__tbStringIncludes, name, ['-'])
        || name !== __tbApply(__tbStringToLowerCase, name, [])
        || !__tbApply(__tbRegExpTest, /^[a-z][.0-9_a-z-]*$/, [name])) {
      return false;
    }
    for (let index = 0; index < reservedNames.length; index++) {
      if (reservedNames[index] === name) return false;
    }
    return true;
  }

  // https://webidl.spec.whatwg.org/#es-sequence
  function toSequence(value) {
    if (value === undefined) return [];
    if (value === null || (typeof value !== 'object' && typeof value !== 'function')) {
      throw new TypeError('value is not iterable');
    }
    const iterator = value[Symbol.iterator];
    if (typeof iterator !== 'function') throw new TypeError('value is not iterable');
    const iteratorObject = nativeApply(iterator, value, []);
    const result = [];
    for (;;) {
      const step = iteratorObject.next();
      if (step.done) break;
      result.push(String(step.value));
    }
    return result;
  }

  function invoke(element, name, args) {
    const callback = definitions.get(localNameOf(element))?.callbacks[name];
    if (typeof callback === 'function') {
      enqueueReaction(element, callback, args);
    }
  }

  function deliverUpgrade(definition) {
    upgradeElement(this, definition);
  }

  function enqueueUpgrade(element, definition) {
    if (upgraded.has(element)) return;
    enqueueReaction(element, deliverUpgrade, [definition]);
  }

  function upgradeElement(element, definition) {
    if (upgraded.has(element) || localNameOf(element) !== definition.name) return;
    upgraded.add(element);
    nativeApply(observe, observer, [element, observerOptions]);
    try {
      host.__tbPushCustomConstruction(element);
      let constructed;
      try {
        constructed = nativeConstruct(definition.constructor, [], definition.constructor);
      } finally {
        host.__tbDiscardCustomConstruction(element);
      }
      if (constructed !== element) throw new TypeError('custom element constructor returned another object');
      // https://html.spec.whatwg.org/multipage/custom-elements.html#upgrades
      __tbSetPrototypeOf(element, definition.constructor.prototype);
      for (const name of definition.observed) {
        if (nativeApply(hasAttribute, element, [name])) {
          invoke(element, 'attributeChangedCallback', [name, null, nativeApply(getAttribute, element, [name]), null]);
        }
      }
      if (isConnected(element)) {
        connected.add(element);
        invoke(element, 'connectedCallback', []);
      }
    } catch (error) {
      console.error(error);
    }
  }

  function upgradeTree(root) {
    visitElements(root, function(element) {
      const definition = definitions.get(localNameOf(element));
      if (definition) enqueueUpgrade(element, definition);
    });
  }

  class CustomElementRegistry {
    define(name, constructor, options) {
      name = String(name);
      if (typeof constructor !== 'function') throw new TypeError('constructor must be callable');
      const extendsValue = options == null ? undefined : options.extends;
      const extendsName = extendsValue === undefined ? undefined : String(extendsValue);
      pushReactions();
      try {
        if (!validName(name)) throw new DOMException('invalid custom element name', 'SyntaxError');
        if (extendsName !== undefined) {
          throw new DOMException('customized built-in elements are not supported', 'NotSupportedError');
        }
        if (definitions.has(name) || constructors.has(constructor)) {
          throw new DOMException('custom element is already defined', 'NotSupportedError');
        }
        // https://html.spec.whatwg.org/multipage/custom-elements.html#dom-customelementregistry-define
        if (definitionRunning) throw new DOMException('custom element definition is running', 'NotSupportedError');
        let callbacks;
        let observed;
        definitionRunning = true;
        try {
          callbacks = createObject(null);
          observed = [];
          const prototype = constructor.prototype;
          if (prototype === null || (typeof prototype !== 'object' && typeof prototype !== 'function')) {
            throw new TypeError('custom element prototype must be an object');
          }
          for (let index = 0; index < callbackNames.length; ++index) {
            const key = callbackNames[index];
            const callback = prototype[key];
            if (callback !== undefined && typeof callback !== 'function') throw new TypeError('custom element callback must be callable');
            callbacks[key] = callback;
          }
          if (callbacks.attributeChangedCallback !== undefined) {
            observed = toSequence(constructor.observedAttributes);
          }
          toSequence(constructor.disabledFeatures);
          const formAssociated = Boolean(constructor.formAssociated);
          if (formAssociated) {
            for (let index = 0; index < formCallbackNames.length; ++index) {
              const key = formCallbackNames[index];
              const callback = prototype[key];
              if (callback !== undefined && typeof callback !== 'function') {
                throw new TypeError('custom element callback must be callable');
              }
              callbacks[key] = callback;
            }
          }
        } finally {
          definitionRunning = false;
        }
        const definition = { name, constructor, observed, callbacks };
        definitions.set(name, definition);
        constructors.set(constructor, name);
        noteDefined();
        ensureObserving();
        upgradeTree(document);
        const waiter = pending.get(name);
        if (waiter) {
          waiter.resolve(constructor);
          pending.delete(name);
        }
      } finally { popReactions(); }
    }
    get(name) { return definitions.get(String(name))?.constructor; }
    getName(constructor) { return constructors.get(constructor) ?? null; }
    whenDefined(name) {
      name = String(name);
      if (!validName(name)) return Promise.reject(new DOMException('invalid custom element name', 'SyntaxError'));
      const definition = definitions.get(name);
      if (definition) return Promise.resolve(definition.constructor);
      let waiter = pending.get(name);
      if (!waiter) {
        let resolve;
        const promise = new Promise(done => { resolve = done; });
        waiter = { promise, resolve };
        pending.set(name, waiter);
      }
      return waiter.promise;
    }
    upgrade(root) {
      pushReactions();
      try { upgradeTree(root); } finally { popReactions(); }
    }
  }

  const registry = new CustomElementRegistry();
  const __tbCustomElementsRegistry = registry;
  __tbDefineProperty(globalThis, 'CustomElementRegistry', {
    value: CustomElementRegistry, writable: true, configurable: true,
  });
  __tbDefineProperty(globalThis, 'customElements', {
    value: registry, writable: false, configurable: true,
  });

  const nativeCreateElement = Document.prototype.createElement;
  // https://html.spec.whatwg.org/multipage/dom.html#htmlconstructor
  host.__tbConstructCustomElement = function(newTarget) {
    if (host.__tbHasCustomConstruction()) {
      const value = host.__tb_construct('HTMLElement');
      if (newTarget && newTarget.prototype) {
        __tbSetPrototypeOf(value, newTarget.prototype);
      }
      return value;
    }
    const name = constructors.get(newTarget);
    if (name === undefined) {
      throw new TypeError('Illegal constructor');
    }
    ensureObserving();
    const element = nativeApply(nativeCreateElement, document, [name]);
    upgraded.add(element);
    nativeApply(observe, observer, [element, observerOptions]);
    const proto = newTarget.prototype;
    if (proto !== null && (typeof proto === 'object' || typeof proto === 'function')) {
      __tbSetPrototypeOf(element, proto);
    }
    return element;
  };
  Document.prototype.createElement = function(name, options) {
    const element = nativeCreateElement.call(this, name, options);
    const definition = definitions.get(localNameOf(element));
    if (definition) {
      pushReactions();
      try { enqueueUpgrade(element, definition); } finally { popReactions(); }
    }
    return element;
  };

  // https://dom.spec.whatwg.org/#concept-create-element
  // https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-element-innerhtml
  const ownerDocumentGetter = __tbGetOwnPropertyDescriptor(Node.prototype, 'ownerDocument').get;
  const defaultViewGetter = __tbGetOwnPropertyDescriptor(Document.prototype, 'defaultView').get;
  const innerHTML = __tbGetOwnPropertyDescriptor(Element.prototype, 'innerHTML');
  function upgradeFromNodeDocument(root) {
    const document = nativeApply(ownerDocumentGetter, root, []);
    if (!document) return;
    const view = nativeApply(defaultViewGetter, document, []);
    const foreign = view && view.customElements;
    if (!foreign || foreign === __tbCustomElementsRegistry) return;
    if (typeof foreign.upgrade === 'function') {
      foreign.upgrade(root);
    }
  }
  if (innerHTML && innerHTML.set) {
    __tbDefineProperty(Element.prototype, 'innerHTML', {
      configurable: true,
      enumerable: innerHTML.enumerable,
      get: innerHTML.get,
      set: function(value) {
        nativeApply(innerHTML.set, this, [value]);
        if (isConnected(this)) upgradeFromNodeDocument(this);
      },
    });
  }
  // https://dom.spec.whatwg.org/#concept-node-insert
  host.__tbUpgradeParsedTree = function(root) {
    if (!isConnected(root)) return;
    upgradeFromNodeDocument(root);
    if (definitions.size === 0) return;
    pushReactions();
    try { upgradeTree(root); } finally { popReactions(); }
  };

  // https://dom.spec.whatwg.org/#concept-node-adopt
  function insertedNodes(node) {
    if (nativeApply(nodeTypeGetter, node, []) !== 11) return [node];
    const children = [];
    for (let child = nativeApply(firstChildGetter, node, []); child; child = nativeApply(nextSiblingGetter, child, [])) {
      children.push(child);
    }
    return children;
  }
  for (const name of ['appendChild', 'insertBefore', 'replaceChild']) {
    const native = Node.prototype[name];
    const insertion = function() {
      const inserted = insertedNodes(arguments[0]);
      const result = nativeApply(native, this, arguments);
      pushReactions();
      try {
        for (const node of inserted) {
          if (isConnected(node)) upgradeTree(node);
        }
      } finally { popReactions(); }
      return result;
    };
    __tbDefineProperty(insertion, 'length', { value: native.length });
    __tbDefineProperty(insertion, 'name', { value: native.name });
    Node.prototype[name] = insertion;
  }

  // https://html.spec.whatwg.org/multipage/custom-elements.html#enqueue-a-custom-element-callback-reaction
  function collectRecords(records) {
    const snapshots = createObject(null);
    snapshots.length = records.length;
    for (let index = 0; index < records.length; ++index) {
      const record = records[index];
      snapshots[index] = {
        type: nativeApply(recordGetters.type, record, []),
        target: nativeApply(recordGetters.target, record, []),
        removedNodes: nativeApply(recordGetters.removedNodes, record, []),
        addedNodes: nativeApply(recordGetters.addedNodes, record, []),
        attributeName: nativeApply(recordGetters.attributeName, record, []),
        attributeNamespace: nativeApply(recordGetters.attributeNamespace, record, []),
        oldValue: nativeApply(recordGetters.oldValue, record, []),
      };
    }
    records = snapshots;
    const values = new MapConstructor();
    const newValues = new MapConstructor();
    for (let index = records.length - 1; index >= 0; --index) {
      const record = records[index];
      if (record.type !== 'attributes') continue;
      let namespaces = nativeApply(mapGet, values, [record.target]);
      if (!namespaces) nativeApply(mapSet, values, [record.target, namespaces = new MapConstructor()]);
      let attributes = nativeApply(mapGet, namespaces, [record.attributeNamespace]);
      if (!attributes) nativeApply(mapSet, namespaces, [record.attributeNamespace, attributes = new MapConstructor()]);
      const key = record.attributeName;
      const value = nativeApply(mapHas, attributes, [key]) ? nativeApply(mapGet, attributes, [key]) : nativeApply(getAttributeNS, record.target, [record.attributeNamespace, key]);
      nativeApply(mapSet, newValues, [record, value]);
      nativeApply(mapSet, attributes, [key, record.oldValue]);
    }
    for (let index = 0; index < records.length; ++index) {
      const record = records[index];
      if (record.type === 'childList') {
        const removed = record.removedNodes;
        for (let childIndex = 0, length = nativeApply(listLengthGetter, removed, []); childIndex < length; ++childIndex) {
          const node = nativeApply(listItem, removed, [childIndex]);
          if (atomicMoves.has(node)) continue;
          notifyDisconnected(node);
        }
        const added = record.addedNodes;
        for (let childIndex = 0, length = nativeApply(listLengthGetter, added, []); childIndex < length; ++childIndex) {
          const node = nativeApply(listItem, added, [childIndex]);
          if (atomicMoves.has(node)) {
            atomicMoves.delete(node);
            continue;
          }
          if (isConnected(node)) upgradeTree(node);
          notifyConnected(node);
        }
      } else if (record.type === 'attributes' && upgraded.has(record.target)) {
        const definition = definitions.get(localNameOf(record.target));
        if (definition && nativeApply(arrayIncludes, definition.observed, [record.attributeName])) {
          invoke(record.target, 'attributeChangedCallback', [
            record.attributeName, record.oldValue, nativeApply(mapGet, newValues, [record]), record.attributeNamespace,
          ]);
        }
      }
    }
  }
  const observer = new MutationObserver(collectRecords);
  const takeRecords = MutationObserver.prototype.takeRecords;
  host.__tbCollectCustomReactions = function() {
    collectRecords(nativeApply(takeRecords, observer, []));
  };
  const observerOptions = {
    subtree: true, childList: true, attributes: true, attributeOldValue: true,
  };
  let observing = false;
  function ensureObserving() {
    if (observing) return;
    observing = true;
    nativeApply(observe, observer, [document, observerOptions]);
  }
  const nativeAttachShadow = Element.prototype.attachShadow;
  if (typeof nativeAttachShadow === 'function') {
    Element.prototype.attachShadow = function attachShadow(init) {
      const shadow = nativeApply(nativeAttachShadow, this, [init]);
      if (shadow) {
        nativeApply(observe, observer, [shadow, observerOptions]);
      }
      return shadow;
    };
  }
}
