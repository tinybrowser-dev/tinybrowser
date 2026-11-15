// The Selection API for this realm's document
// (<https://w3c.github.io/selection-api/#selection-interface>).
//
// A selection holds at most one range, by reference: `addRange` stores the
// Range object a script passed, and `getRangeAt(0)` returns that same object,
// so a DOM mutation that moves the live range moves the selection with it
// (<https://w3c.github.io/selection-api/#dom-selection-getrangeat>).
{
  // The document the selection belongs to. A realm keeps its document object
  // across `document.open()`, and a navigation replaces the whole realm, so
  // reading it lazily is enough to notice a replaced tree.
  let selectionDocument = null;
  let range = null;
  // null is the spec's "directionless" state, which `getRangeAt` answers as
  // "none" until `extend`, `setBaseAndExtent`, or `selectAllChildren` sets a
  // direction (<https://w3c.github.io/selection-api/#dfn-directionless>).
  let direction = null;

  const documentOf = () => {
    const document = globalThis.document;
    if (document !== selectionDocument) {
      selectionDocument = document;
      range = null;
      direction = null;
    }
    return document;
  };

  // A node's length: CharacterData's character count, otherwise the child
  // count. A document type has none.
  const lengthOf = (node) => {
    const type = node.nodeType;
    if (type === Node.TEXT_NODE || type === Node.CDATA_SECTION_NODE
        || type === Node.COMMENT_NODE
        || type === Node.PROCESSING_INSTRUCTION_NODE) {
      return node.length;
    }
    return node.childNodes.length;
  };

  // Whether the node is in the document tree the selection belongs to, in
  // the shadow-including sense every Selection algorithm asks about: the
  // document must be a shadow-including inclusive ancestor of the node
  // (<https://w3c.github.io/selection-api/#terminology>). A node in a
  // detached tree, a template's contents, or another document (one a parser
  // or `createHTMLDocument` produced, whose `isConnected` is true against its
  // own root) is not in this document tree.
  const inDocumentTree = (node) => {
    const document = documentOf();
    let current = node;
    while (current !== null) {
      if (current === document) return true;
      current = current.parentNode !== null ? current.parentNode : (current.host ?? null);
    }
    return false;
  };

  const point = (node, offset) => ({ node, offset });

  const anchorPoint = () => (direction === 'backward'
    ? point(range.endContainer, range.endOffset)
    : point(range.startContainer, range.startOffset));

  const focusPoint = () => (direction === 'backward'
    ? point(range.startContainer, range.startOffset)
    : point(range.endContainer, range.endOffset));

  // The shadow-including ancestor chain, root first.
  const ancestry = (node) => {
    const chain = [];
    let current = node;
    while (current !== null) {
      chain.push(current);
      if (current.parentNode !== null) {
        current = current.parentNode;
      } else if (current.host !== undefined) {
        // A shadow root hangs below its host: its contents come before the
        // host's light children in shadow-including tree order.
        current = current.host;
      } else {
        current = null;
      }
    }
    return chain.reverse();
  };

  const compareTreeOrder = (a, b) => {
    const pathA = ancestry(a);
    const pathB = ancestry(b);
    const shared = Math.min(pathA.length, pathB.length);
    let index = 0;
    while (index < shared && pathA[index] === pathB[index]) index += 1;
    if (index < shared) {
      const parent = pathA[index - 1];
      const aShadow = pathA[index].host !== undefined;
      const bShadow = pathB[index].host !== undefined;
      // A shadow root is not in `childNodes`; it precedes the light children
      // of its host
      if (aShadow !== bShadow) return aShadow ? -1 : 1;
      const children = parent.childNodes;
      let aIndex = children.length;
      let bIndex = children.length;
      for (let i = 0; i < children.length; i += 1) {
        if (children[i] === pathA[index]) aIndex = i;
        if (children[i] === pathB[index]) bIndex = i;
      }
      return aIndex < bIndex ? -1 : 1;
    }
    // One chain is a prefix of the other: the shorter is an ancestor
    return pathA.length === pathB.length ? 0 : (pathA.length < pathB.length ? -1 : 1);
  };

  // Where boundary point `a` lies relative to `b`: -1 before, 0 equal, 1
  // after. Same-tree points go through `Range.comparePoint`; across shadow
  // trees the answer comes from shadow-including tree order.
  const comparePoints = (a, b) => {
    if (a.node === b.node) {
      if (a.offset === b.offset) return 0;
      return a.offset < b.offset ? -1 : 1;
    }
    if (a.node.getRootNode() === b.node.getRootNode()) {
      const scratch = documentOf().createRange();
      scratch.setStart(b.node, b.offset);
      scratch.setEnd(b.node, b.offset);
      return scratch.comparePoint(a.node, a.offset);
    }
    return compareTreeOrder(a.node, b.node);
  };

  const newRange = (start, end) => {
    const created = documentOf().createRange();
    created.setStart(start.node, start.offset);
    created.setEnd(end.node, end.offset);
    return created;
  };

  const setBoundaries = (anchor, focus) => {
    const order = comparePoints(anchor, focus);
    range = order <= 0 ? newRange(anchor, focus) : newRange(focus, anchor);
    direction = order < 0 ? 'forward' : (order > 0 ? 'backward' : 'forward');
  };

  const requireRange = () => {
    if (range === null) {
      throw new DOMException('the selection is empty', 'InvalidStateError');
    }
    return range;
  };

  // WebIDL's `Range` argument conversion: only an object implementing the
  // interface reaches the algorithm. `Range` is a Rust-backed class and
  // `startContainer` is `AbstractRange`'s accessor, so invoking it on the
  // value is the brand check: it throws `TypeError` for anything that is not
  // an `AbstractRange` (<https://webidl.spec.whatwg.org/#es-interface>).
  const rangeStartContainer = __tbIDLDescriptor(AbstractRange.prototype, 'startContainer').get;
  const requireRangeArgument = (value) => {
    if (value === null || (typeof value !== 'object' && typeof value !== 'function')) {
      throw new TypeError('value is not a Range');
    }
    __tbApply(rangeStartContainer, value, []);
  };

  const checkNode = (node) => {
    if (node.nodeType === Node.DOCUMENT_TYPE_NODE) {
      throw new DOMException('a doctype cannot be selected', 'InvalidNodeTypeError');
    }
  };

  // The anchor and focus points as the IDL attributes expose them: a point
  // outside the document tree reads as `null`, offset `0`
  // (<https://w3c.github.io/selection-api/#dom-selection-anchornode>).
  const exposed = (point) => (inDocumentTree(point.node)
    ? point
    : { node: null, offset: 0 });

  const [Selection, selection] = __tbInstallGlobal('Selection', class Selection {
    get anchorNode() {
      return range === null ? null : exposed(anchorPoint()).node;
    }
    get anchorOffset() {
      return range === null ? 0 : exposed(anchorPoint()).offset;
    }
    get focusNode() {
      return range === null ? null : exposed(focusPoint()).node;
    }
    get focusOffset() {
      return range === null ? 0 : exposed(focusPoint()).offset;
    }
    // https://w3c.github.io/selection-api/#dom-selection-iscollapsed
    get isCollapsed() {
      const anchor = this.anchorNode;
      const focus = this.focusNode;
      return anchor === focus
        && (anchor === null || this.anchorOffset === this.focusOffset);
    }
    get rangeCount() {
      if (range === null || !inDocumentTree(range.startContainer)
          || !inDocumentTree(range.endContainer)) {
        return 0;
      }
      return 1;
    }
    get type() {
      if (this.rangeCount === 0) return 'None';
      return this.isCollapsed ? 'Caret' : 'Range';
    }
    get direction() {
      if (this.rangeCount === 0 || direction === null) return 'none';
      return direction;
    }
    // https://w3c.github.io/selection-api/#dom-selection-getrangeat
    getRangeAt(index) {
      if (index !== 0 || this.rangeCount === 0) {
        throw new DOMException('no range at this index', 'IndexSizeError');
      }
      // The selection holds the range by reference, so a script that keeps it
      // sees the mutations the document makes to it
      // (<https://w3c.github.io/selection-api/#dom-selection-addrange>).
      return range;
    }
    addRange(candidate) {
      requireRangeArgument(candidate);
      const document = documentOf();
      if (candidate.startContainer.getRootNode() !== document
          || candidate.endContainer.getRootNode() !== document) {
        return;
      }
      if (this.rangeCount !== 0) return;
      range = candidate;
      direction = null;
    }
    removeRange(candidate) {
      // The IDL conversion runs before the algorithm, so `null` is a
      // `TypeError` and only a real Range can reach the identity test
      // (<https://webidl.spec.whatwg.org/#es-interface>).
      requireRangeArgument(candidate);
      if (range !== candidate) {
        throw new DOMException('the range is not in the selection', 'NotFoundError');
      }
      range = null;
      direction = null;
    }
    removeAllRanges() {
      range = null;
      direction = null;
    }
    empty() {
      this.removeAllRanges();
    }
    // https://w3c.github.io/selection-api/#dom-selection-collapse
    collapse(node, offset) {
      if (node === null) {
        this.removeAllRanges();
        return;
      }
      checkNode(node);
      if (offset > lengthOf(node)) {
        throw new DOMException('offset is past the node length', 'IndexSizeError');
      }
      if (!inDocumentTree(node)) return;
      range = newRange(point(node, offset), point(node, offset));
    }
    setPosition(node, offset) {
      this.collapse(node, offset);
    }
    collapseToStart() {
      const current = requireRange();
      const start = point(current.startContainer, current.startOffset);
      range = newRange(start, start);
    }
    collapseToEnd() {
      const current = requireRange();
      const end = point(current.endContainer, current.endOffset);
      range = newRange(end, end);
    }
    // https://w3c.github.io/selection-api/#dom-selection-extend
    extend(node, offset) {
      if (!inDocumentTree(node)) return;
      if (range === null) {
        throw new DOMException('the selection is empty', 'InvalidStateError');
      }
      const anchor = anchorPoint();
      const focus = point(node, offset);
      // A focus in another root cannot share a range with the anchor, so the
      // selection collapses onto it
      // (<https://w3c.github.io/selection-api/#dom-selection-extend>).
      if (focus.node.getRootNode() !== range.startContainer.getRootNode()) {
        range = newRange(focus, focus);
        direction = 'forward';
        return;
      }
      const order = comparePoints(anchor, focus);
      range = order <= 0 ? newRange(anchor, focus) : newRange(focus, anchor);
      // The direction is backwards only when the focus ends up before the
      // anchor; an equal focus is forwards
      // (<https://w3c.github.io/selection-api/#dom-selection-extend>).
      direction = order < 0 ? 'forward' : (order > 0 ? 'backward' : 'forward');
    }
    // https://w3c.github.io/selection-api/#dom-selection-setbaseandextent
    setBaseAndExtent(anchorNode, anchorOffset, focusNode, focusOffset) {
      if (anchorOffset > lengthOf(anchorNode) || focusOffset > lengthOf(focusNode)) {
        throw new DOMException('offset is past the node length', 'IndexSizeError');
      }
      if (!inDocumentTree(anchorNode) || !inDocumentTree(focusNode)) return;
      setBoundaries(point(anchorNode, anchorOffset), point(focusNode, focusOffset));
    }
    // https://w3c.github.io/selection-api/#dom-selection-selectallchildren
    selectAllChildren(node) {
      checkNode(node);
      if (node.getRootNode() !== documentOf()) return;
      const end = lengthOf(node);
      range = newRange(point(node, 0), point(node, end));
      direction = 'forward';
    }
    // https://w3c.github.io/selection-api/#dom-selection-containsnode
    containsNode(node, allowPartialContainment) {
      if (range === null || !inDocumentTree(node)) return false;
      if (allowPartialContainment) return range.intersectsNode(node);
      const start = point(range.startContainer, range.startOffset);
      const end = point(range.endContainer, range.endOffset);
      return comparePoints(start, point(node, 0)) <= 0
        && comparePoints(point(node, lengthOf(node)), end) <= 0;
    }
  }, ['deleteFromDocument', 'getComposedRanges', 'modify']);

  // The anonymous `stringifier` is the selection's text: the range's text if
  // it has one, the empty string otherwise
  // (<https://w3c.github.io/selection-api/#dom-selection-stringifier>).
  __tbDefineProperty(Selection.prototype, 'toString', {
    value: function toString() {
      return range === null ? '' : range.toString();
    },
    writable: true, enumerable: true, configurable: true,
  });

  // Every read of the document's selection returns that Window's one object
  // (<https://w3c.github.io/selection-api/#extensions-to-the-document-interface>).
  // A document with no browsing context has no Window, and therefore no
  // selection.
  const getSelection = function getSelection() {
    return this.defaultView === null ? null : selection;
  };
  __tbDefineProperty(Document.prototype, 'getSelection', {
    __proto__: null, value: getSelection, writable: true, enumerable: true, configurable: true,
  });
}
