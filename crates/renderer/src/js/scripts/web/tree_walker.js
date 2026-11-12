// https://dom.spec.whatwg.org/#traversal
//
// `TreeWalker` and `NodeIterator` are the traversal interfaces: both hold a
// root, a whatToShow bitmask, and a filter callback, and both guard against a
// filter that re-enters the traversal through an "is active" flag.
{
  const FILTER_ACCEPT = 1;
  const FILTER_REJECT = 2;
  const FILTER_SKIP = 3;

  const __tbTraversalTreeWalker = host.slots('tinybrowser.traversal.treewalker');
  const __tbTraversalNodeIterator = host.slots('tinybrowser.traversal.nodeiterator');
  // Passed as the first constructor argument so `new TreeWalker()` and
  // `new NodeIterator()` throw: neither interface has a constructor
  // (<https://webidl.spec.whatwg.org/#interface-object>).
  const __tbTraversalConstruct = { __proto__: null };

  /// Converts `whatToShow` to an unsigned long.
  const __tbTraversalMask = value => {
    if (value === undefined) return 0xFFFFFFFF;
    return Number(value) >>> 0;
  };

  /// The `NodeFilter?` callback-interface conversion: null and undefined are
  /// no filter, anything else is kept and called as a function or through
  /// `acceptNode` when filtering
  /// (<https://webidl.spec.whatwg.org/#es-callback-interface>).
  const __tbTraversalFilterValue = value => {
    if (value === null || value === undefined) return null;
    return value;
  };

  /// Filtering a node for a traverser, including the active flag that makes a
  /// re-entrant filter throw
  /// (<https://dom.spec.whatwg.org/#concept-node-filter>).
  const __tbTraversalFilter = (state, node) => {
    if (state.active) {
      throw new globalThis.DOMException(
        'The filter is already running for this traversal.',
        'InvalidStateError',
      );
    }
    const bit = 1 << (node.nodeType - 1);
    if ((state.whatToShow & bit) === 0) return FILTER_SKIP;
    if (state.filter === null) return FILTER_ACCEPT;
    state.active = true;
    let result;
    try {
      result = typeof state.filter === 'function'
        ? state.filter(node)
        : state.filter.acceptNode(node);
    } finally {
      state.active = false;
    }
    return Number(result);
  };

  /// The first node after `node` in tree order, staying inside `root`'s
  /// subtree.
  const __tbTraversalFollowing = (root, node) => {
    if (node.firstChild !== null) return node.firstChild;
    let current = node;
    while (current !== null && current !== root) {
      if (current.nextSibling !== null) return current.nextSibling;
      current = current.parentNode;
    }
    return null;
  };

  /// The last node before `node` in tree order, staying inside `root`'s
  /// subtree.
  const __tbTraversalPreceding = (root, node) => {
    if (node === root) return null;
    if (node.previousSibling !== null) {
      let current = node.previousSibling;
      while (current.lastChild !== null) current = current.lastChild;
      return current;
    }
    return node.parentNode;
  };

  // https://dom.spec.whatwg.org/#interface-treewalker
  const __tbTreeWalkerConstructor = globalThis.TreeWalker = class TreeWalker {
    constructor(token, root, whatToShow, filter) {
      if (token !== __tbTraversalConstruct) throw new TypeError('Illegal constructor');
      __tbTraversalTreeWalker.set(this, {
        __proto__: null,
        root,
        whatToShow: __tbTraversalMask(whatToShow),
        filter: __tbTraversalFilterValue(filter),
        current: root,
        active: false,
      });
    }
    get root() {
      const state = __tbTraversalTreeWalker.get(this);
      if (state === undefined) throw new TypeError('Illegal invocation');
      return state.root;
    }
    get whatToShow() {
      const state = __tbTraversalTreeWalker.get(this);
      if (state === undefined) throw new TypeError('Illegal invocation');
      return state.whatToShow;
    }
    get filter() {
      const state = __tbTraversalTreeWalker.get(this);
      if (state === undefined) throw new TypeError('Illegal invocation');
      return state.filter;
    }
    get currentNode() {
      const state = __tbTraversalTreeWalker.get(this);
      if (state === undefined) throw new TypeError('Illegal invocation');
      return state.current;
    }
    set currentNode(value) {
      const state = __tbTraversalTreeWalker.get(this);
      if (state === undefined) throw new TypeError('Illegal invocation');
      state.current = __tbTraversalRoot(value);
    }
    parentNode() {
      const state = __tbTraversalTreeWalker.get(this);
      if (state === undefined) throw new TypeError('Illegal invocation');
      let node = state.current;
      while (node !== null && node !== state.root) {
        node = node.parentNode;
        if (node !== null && __tbTraversalFilter(state, node) === FILTER_ACCEPT) {
          state.current = node;
          return node;
        }
      }
      return null;
    }
    firstChild() {
      return __tbTraversalChildren(this, 'first');
    }
    lastChild() {
      return __tbTraversalChildren(this, 'last');
    }
    previousSibling() {
      return __tbTraversalSiblings(this, 'previous');
    }
    nextSibling() {
      return __tbTraversalSiblings(this, 'next');
    }
    previousNode() {
      const state = __tbTraversalTreeWalker.get(this);
      if (state === undefined) throw new TypeError('Illegal invocation');
      let node = state.current;
      while (node !== state.root) {
        let sibling = node.previousSibling;
        while (sibling !== null) {
          node = sibling;
          let result = __tbTraversalFilter(state, node);
          while (result !== FILTER_REJECT && node.lastChild !== null) {
            node = node.lastChild;
            result = __tbTraversalFilter(state, node);
          }
          if (result === FILTER_ACCEPT) {
            state.current = node;
            return node;
          }
          sibling = node.previousSibling;
        }
        if (node === state.root || node.parentNode === null) return null;
        node = node.parentNode;
        if (__tbTraversalFilter(state, node) === FILTER_ACCEPT) {
          state.current = node;
          return node;
        }
      }
      return null;
    }
    nextNode() {
      const state = __tbTraversalTreeWalker.get(this);
      if (state === undefined) throw new TypeError('Illegal invocation');
      let node = state.current;
      let result = FILTER_ACCEPT;
      while (true) {
        while (result !== FILTER_REJECT && node.firstChild !== null) {
          node = node.firstChild;
          result = __tbTraversalFilter(state, node);
          if (result === FILTER_ACCEPT) {
            state.current = node;
            return node;
          }
        }
        let sibling = null;
        let temporary = node;
        while (temporary !== null) {
          if (temporary === state.root) return null;
          sibling = temporary.nextSibling;
          if (sibling !== null) break;
          temporary = temporary.parentNode;
        }
        if (sibling === null) return null;
        node = sibling;
        result = __tbTraversalFilter(state, node);
        if (result === FILTER_ACCEPT) {
          state.current = node;
          return node;
        }
      }
    }
  };
  Object.defineProperty(__tbTreeWalkerConstructor.prototype, Symbol.toStringTag, {
    value: 'TreeWalker', configurable: true,
  });

  /// `firstChild()` / `lastChild()`
  /// (<https://dom.spec.whatwg.org/#concept-traverse-children>).
  const __tbTraversalChildren = (walker, type) => {
    const state = __tbTraversalTreeWalker.get(walker);
    if (state === undefined) throw new TypeError('Illegal invocation');
    let node = type === 'first' ? state.current.firstChild : state.current.lastChild;
    while (node !== null) {
      const result = __tbTraversalFilter(state, node);
      if (result === FILTER_ACCEPT) {
        state.current = node;
        return node;
      }
      if (result === FILTER_SKIP) {
        const child = type === 'first' ? node.firstChild : node.lastChild;
        if (child !== null) {
          node = child;
          continue;
        }
      }
      while (node !== null) {
        const sibling = type === 'first' ? node.nextSibling : node.previousSibling;
        if (sibling !== null) {
          node = sibling;
          break;
        }
        const parent = node.parentNode;
        if (parent === null || parent === state.root || parent === state.current) return null;
        node = parent;
      }
    }
    return null;
  };

  /// `nextSibling()` / `previousSibling()`
  /// (<https://dom.spec.whatwg.org/#concept-traverse-siblings>).
  const __tbTraversalSiblings = (walker, type) => {
    const state = __tbTraversalTreeWalker.get(walker);
    if (state === undefined) throw new TypeError('Illegal invocation');
    let node = state.current;
    if (node === state.root) return null;
    while (true) {
      let sibling = type === 'next' ? node.nextSibling : node.previousSibling;
      while (sibling !== null) {
        node = sibling;
        const result = __tbTraversalFilter(state, node);
        if (result === FILTER_ACCEPT) {
          state.current = node;
          return node;
        }
        sibling = type === 'next' ? node.firstChild : node.lastChild;
        if (result === FILTER_REJECT || sibling === null) {
          sibling = type === 'next' ? node.nextSibling : node.previousSibling;
        }
      }
      node = node.parentNode;
      if (node === null || node === state.root) return null;
      if (__tbTraversalFilter(state, node) === FILTER_ACCEPT) return null;
    }
  };

  // https://dom.spec.whatwg.org/#interface-nodeiterator
  const __tbNodeIteratorConstructor = globalThis.NodeIterator = class NodeIterator {
    constructor(token, root, whatToShow, filter) {
      if (token !== __tbTraversalConstruct) throw new TypeError('Illegal constructor');
      __tbTraversalNodeIterator.set(this, {
        __proto__: null,
        root,
        whatToShow: __tbTraversalMask(whatToShow),
        filter: __tbTraversalFilterValue(filter),
        reference: root,
        pointerBefore: true,
        active: false,
      });
    }
    get root() {
      const state = __tbTraversalNodeIterator.get(this);
      if (state === undefined) throw new TypeError('Illegal invocation');
      return state.root;
    }
    get referenceNode() {
      const state = __tbTraversalNodeIterator.get(this);
      if (state === undefined) throw new TypeError('Illegal invocation');
      return state.reference;
    }
    get pointerBeforeReferenceNode() {
      const state = __tbTraversalNodeIterator.get(this);
      if (state === undefined) throw new TypeError('Illegal invocation');
      return state.pointerBefore;
    }
    get whatToShow() {
      const state = __tbTraversalNodeIterator.get(this);
      if (state === undefined) throw new TypeError('Illegal invocation');
      return state.whatToShow;
    }
    get filter() {
      const state = __tbTraversalNodeIterator.get(this);
      if (state === undefined) throw new TypeError('Illegal invocation');
      return state.filter;
    }
    nextNode() {
      return __tbTraversalIterate(this, 'next');
    }
    previousNode() {
      return __tbTraversalIterate(this, 'previous');
    }
    detach() {
      if (__tbTraversalNodeIterator.get(this) === undefined) {
        throw new TypeError('Illegal invocation');
      }
    }
  };
  Object.defineProperty(__tbNodeIteratorConstructor.prototype, Symbol.toStringTag, {
    value: 'NodeIterator', configurable: true,
  });

  /// `nextNode()` / `previousNode()` for a `NodeIterator`
  /// (<https://dom.spec.whatwg.org/#concept-nodeiterator-traverse>).
  const __tbTraversalIterate = (iterator, type) => {
    const state = __tbTraversalNodeIterator.get(iterator);
    if (state === undefined) throw new TypeError('Illegal invocation');
    let node = state.reference;
    let before = state.pointerBefore;
    while (true) {
      if (type === 'next') {
        if (!before) {
          const following = __tbTraversalFollowing(state.root, node);
          if (following === null) break;
          node = following;
        }
        before = false;
      } else {
        if (before) {
          const preceding = __tbTraversalPreceding(state.root, node);
          if (preceding === null) break;
          node = preceding;
        }
        before = true;
      }
      const result = __tbTraversalFilter(state, node);
      if (result === FILTER_ACCEPT) {
        state.reference = node;
        state.pointerBefore = before;
        return node;
      }
    }
    return null;
  };

  const NodeFilter = {
    FILTER_ACCEPT, FILTER_REJECT, FILTER_SKIP,
    SHOW_ALL: 0xFFFFFFFF,
    SHOW_ELEMENT: 0x1,
    SHOW_ATTRIBUTE: 0x2,
    SHOW_TEXT: 0x4,
    SHOW_CDATA_SECTION: 0x8,
    SHOW_ENTITY_REFERENCE: 0x10,
    SHOW_ENTITY: 0x20,
    SHOW_PROCESSING_INSTRUCTION: 0x40,
    SHOW_COMMENT: 0x80,
    SHOW_DOCUMENT: 0x100,
    SHOW_DOCUMENT_TYPE: 0x200,
    SHOW_DOCUMENT_FRAGMENT: 0x400,
    SHOW_NOTATION: 0x800,
  };
  Object.defineProperty(globalThis, 'NodeFilter', {
    value: Object.freeze(NodeFilter), writable: true, configurable: true,
  });

  /// The `Node` root argument shared by `createTreeWalker` and
  /// `createNodeIterator`. Nodes from another realm are still Nodes, so this
  /// checks the interface's shape instead of the local constructor
  /// (<https://webidl.spec.whatwg.org/#es-interface>).
  const __tbTraversalRoot = root => {
    if (root === null || typeof root !== 'object'
      || typeof root.nodeType !== 'number'
      || typeof root.nodeName !== 'string') {
      throw new TypeError('The root argument is not a Node.');
    }
    return root;
  };

  Object.defineProperty(Document.prototype, 'createTreeWalker', {
    value: function (root, whatToShow = 0xFFFFFFFF, filter = null) {
      if (arguments.length < 1) {
        throw new TypeError("Failed to execute 'createTreeWalker' on 'Document': 1 argument required, but only 0 present.");
      }
      return new __tbTreeWalkerConstructor(
        __tbTraversalConstruct, __tbTraversalRoot(root), whatToShow, filter);
    },
    writable: true, configurable: true, enumerable: true,
  });
  Object.defineProperty(Document.prototype, 'createNodeIterator', {
    value: function (root, whatToShow = 0xFFFFFFFF, filter = null) {
      if (arguments.length < 1) {
        throw new TypeError("Failed to execute 'createNodeIterator' on 'Document': 1 argument required, but only 0 present.");
      }
      return new __tbNodeIteratorConstructor(
        __tbTraversalConstruct, __tbTraversalRoot(root), whatToShow, filter);
    },
    writable: true, configurable: true, enumerable: true,
  });
}
