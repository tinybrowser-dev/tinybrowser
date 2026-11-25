// https://w3c.github.io/selection-api/#selection-interface
// https://w3c.github.io/selection-api/#dom-selection-getrangeat
{
  let selectionDocument = null;
  let range = null;
  // https://w3c.github.io/selection-api/#dfn-directionless
  let direction = null;
  let version = -1;

  const documentOf = () => {
    const document = globalThis.document;
    if (document !== selectionDocument) {
      selectionDocument = document;
      range = null;
      direction = null;
      version = -1;
    }
    return document;
  };

  const lengthOf = (node) => {
    const type = node.nodeType;
    if (type === Node.TEXT_NODE || type === Node.CDATA_SECTION_NODE
        || type === Node.COMMENT_NODE
        || type === Node.PROCESSING_INSTRUCTION_NODE) {
      return node.length;
    }
    return node.childNodes.length;
  };

  // https://w3c.github.io/selection-api/#terminology
  const inDocumentTree = (node) => {
    const document = documentOf();
    let current = node;
    while (current !== null) {
      if (sameNode(current, document)) return true;
      current = current.parentNode !== null ? current.parentNode : (current.host ?? null);
    }
    return false;
  };

  const point = (node, offset) => ({ node, offset });

  // https://dom.spec.whatwg.org/#dom-node-issamenode
  const sameNode = (a, b) => a === b
    || (a !== null && b !== null && typeof a.isSameNode === 'function' && a.isSameNode(b));

  const anchorPoint = () => (direction === 'backward'
    ? point(range.endContainer, range.endOffset)
    : point(range.startContainer, range.startOffset));

  const focusPoint = () => (direction === 'backward'
    ? point(range.startContainer, range.startOffset)
    : point(range.endContainer, range.endOffset));

  const ancestry = (node) => {
    const chain = [];
    let current = node;
    while (current !== null) {
      chain.push(current);
      if (current.parentNode !== null) {
        current = current.parentNode;
      } else if (current.host !== undefined) {
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
    return pathA.length === pathB.length ? 0 : (pathA.length < pathB.length ? -1 : 1);
  };

  const comparePoints = (a, b) => {
    if (a.node === b.node) {
      if (a.offset === b.offset) return 0;
      return a.offset < b.offset ? -1 : 1;
    }
    if (sameNode(a.node.getRootNode(), b.node.getRootNode())) {
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
    return {
      range: order <= 0 ? newRange(anchor, focus) : newRange(focus, anchor),
      direction: order < 0 ? 'forward' : (order > 0 ? 'backward' : 'forward'),
    };
  };

  const directionCode = () => (direction === 'forward' ? 1 : (direction === 'backward' ? 2 : 0));
  const directionFrom = (code) => (code === 1 ? 'forward' : (code === 2 ? 'backward' : null));

  // https://w3c.github.io/selection-api/#selection-interface
  const sync = () => {
    const document = documentOf();
    const state = host.__tbSelectionState(document);
    if (state === null || state[0] === version) return;
    version = state[0];
    const [, startNode, startOffset, endNode, endOffset, code] = state;
    direction = directionFrom(code);
    if (startNode === null || endNode === null || !sameNode(startNode.getRootNode(), endNode.getRootNode())) {
      range = null;
      return;
    }
    const clampedStart = Math.min(startOffset, lengthOf(startNode));
    const clampedEnd = Math.min(endOffset, lengthOf(endNode));
    range = newRange(point(startNode, clampedStart), point(endNode, clampedEnd));
  };

  const commit = (nextRange, nextDirection) => {
    const document = documentOf();
    range = nextRange;
    direction = nextDirection;
    if (nextRange === null) {
      version = host.__tbSelectionClear(document);
      return;
    }
    version = host.__tbSelectionCommit(
      document,
      nextRange.startContainer, nextRange.startOffset,
      nextRange.endContainer, nextRange.endOffset,
      directionCode(),
    );
  };

  const requireRange = () => {
    if (range === null) {
      throw new DOMException('the selection is empty', 'InvalidStateError');
    }
    return range;
  };

  // https://webidl.spec.whatwg.org/#es-interface
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

  // https://w3c.github.io/selection-api/#dom-selection-anchornode
  const exposed = (point) => (inDocumentTree(point.node)
    ? point
    : { node: null, offset: 0 });

  const [Selection, selection] = __tbInstallGlobal('Selection', class Selection {
    get anchorNode() {
      sync();
      return range === null ? null : exposed(anchorPoint()).node;
    }
    get anchorOffset() {
      sync();
      return range === null ? 0 : exposed(anchorPoint()).offset;
    }
    get focusNode() {
      sync();
      return range === null ? null : exposed(focusPoint()).node;
    }
    get focusOffset() {
      sync();
      return range === null ? 0 : exposed(focusPoint()).offset;
    }
    // https://w3c.github.io/selection-api/#dom-selection-iscollapsed
    get isCollapsed() {
      sync();
      const anchor = this.anchorNode;
      const focus = this.focusNode;
      return anchor === focus
        && (anchor === null || this.anchorOffset === this.focusOffset);
    }
    get rangeCount() {
      sync();
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
      sync();
      if (index !== 0 || this.rangeCount === 0) {
        throw new DOMException('no range at this index', 'IndexSizeError');
      }
      // https://w3c.github.io/selection-api/#dom-selection-addrange
      return range;
    }
    addRange(candidate) {
      requireRangeArgument(candidate);
      sync();
      const document = documentOf();
      const rootStart = candidate.startContainer.getRootNode();
      const rootEnd = candidate.endContainer.getRootNode();
      if (!sameNode(rootStart, document) || !sameNode(rootEnd, document)) {
        return;
      }
      if (this.rangeCount !== 0) return;
      commit(candidate, null);
    }
    removeRange(candidate) {
      // https://webidl.spec.whatwg.org/#es-interface
      requireRangeArgument(candidate);
      sync();
      if (range !== candidate) {
        throw new DOMException('the range is not in the selection', 'NotFoundError');
      }
      commit(null, null);
    }
    removeAllRanges() {
      sync();
      commit(null, null);
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
      sync();
      checkNode(node);
      const index = offset >>> 0;
      if (index > lengthOf(node)) {
        throw new DOMException('offset is past the node length', 'IndexSizeError');
      }
      if (!inDocumentTree(node)) return;
      commit(newRange(point(node, index), point(node, index)), direction);
    }
    setPosition(node, offset) {
      this.collapse(node, offset);
    }
    collapseToStart() {
      sync();
      const current = requireRange();
      const start = point(current.startContainer, current.startOffset);
      commit(newRange(start, start), direction);
    }
    collapseToEnd() {
      sync();
      const current = requireRange();
      const end = point(current.endContainer, current.endOffset);
      commit(newRange(end, end), direction);
    }
    // https://w3c.github.io/selection-api/#dom-selection-extend
    extend(node, offset) {
      sync();
      if (!inDocumentTree(node)) return;
      if (range === null) {
        throw new DOMException('the selection is empty', 'InvalidStateError');
      }
      const anchor = anchorPoint();
      const focus = point(node, offset);
      // https://w3c.github.io/selection-api/#dom-selection-extend
      if (!sameNode(focus.node.getRootNode(), range.startContainer.getRootNode())) {
        commit(newRange(focus, focus), 'forward');
        return;
      }
      const order = comparePoints(anchor, focus);
      commit(
        order <= 0 ? newRange(anchor, focus) : newRange(focus, anchor),
        // https://w3c.github.io/selection-api/#dom-selection-extend
        order < 0 ? 'forward' : (order > 0 ? 'backward' : 'forward'),
      );
    }
    // https://w3c.github.io/selection-api/#dom-selection-setbaseandextent
    setBaseAndExtent(anchorNode, anchorOffset, focusNode, focusOffset) {
      sync();
      const anchorIndex = anchorOffset >>> 0;
      const focusIndex = focusOffset >>> 0;
      if (anchorIndex > lengthOf(anchorNode) || focusIndex > lengthOf(focusNode)) {
        throw new DOMException('offset is past the node length', 'IndexSizeError');
      }
      if (!inDocumentTree(anchorNode) || !inDocumentTree(focusNode)) return;
      const boundaries = setBoundaries(
        point(anchorNode, anchorIndex), point(focusNode, focusIndex));
      commit(boundaries.range, boundaries.direction);
    }
    // https://w3c.github.io/selection-api/#dom-selection-selectallchildren
    selectAllChildren(node) {
      sync();
      checkNode(node);
      if (!sameNode(node.getRootNode(), documentOf())) return;
      const end = lengthOf(node);
      commit(newRange(point(node, 0), point(node, end)), 'forward');
    }
    // https://w3c.github.io/selection-api/#dom-selection-containsnode
    containsNode(node, allowPartialContainment) {
      sync();
      if (range === null || !inDocumentTree(node)) return false;
      if (allowPartialContainment) return range.intersectsNode(node);
      const start = point(range.startContainer, range.startOffset);
      const end = point(range.endContainer, range.endOffset);
      return comparePoints(start, point(node, 0)) <= 0
        && comparePoints(point(node, lengthOf(node)), end) <= 0;
    }
  }, ['deleteFromDocument', 'getComposedRanges', 'modify']);

  // https://w3c.github.io/selection-api/#dom-selection-deletefromdocument
  __tbDefineProperty(Selection.prototype, 'deleteFromDocument', {
    value: function deleteFromDocument() {
      sync();
      if (range !== null) {
        range.deleteContents();
        commit(range, direction);
      }
    },
    writable: true, enumerable: true, configurable: true,
  });

  // https://w3c.github.io/selection-api/#dom-selection-stringifier
  __tbDefineProperty(Selection.prototype, 'toString', {
    value: function toString() {
      sync();
      return range === null ? '' : range.toString();
    },
    writable: true, enumerable: true, configurable: true,
  });

  // https://w3c.github.io/selection-api/#extensions-to-the-document-interface
  const getSelection = function getSelection() {
    return this.defaultView === null ? null : selection;
  };
  __tbDefineProperty(Document.prototype, 'getSelection', {
    __proto__: null, value: getSelection, writable: true, enumerable: true, configurable: true,
  });
}
