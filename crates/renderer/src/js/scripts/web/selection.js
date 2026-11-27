// https://w3c.github.io/selection-api/#selection-interface
// https://w3c.github.io/selection-api/#dom-selection-getrangeat
{
  const documentOf = () => globalThis.document;

  const lengthOf = (node) => {
    const type = node.nodeType;
    if (type === Node.TEXT_NODE || type === Node.CDATA_SECTION_NODE
        || type === Node.COMMENT_NODE
        || type === Node.PROCESSING_INSTRUCTION_NODE) {
      return node.length;
    }
    return node.childNodes.length;
  };

  // https://dom.spec.whatwg.org/#dom-node-issamenode
  const sameNode = (a, b) => a === b
    || (a !== null && b !== null && typeof a.isSameNode === 'function' && a.isSameNode(b));

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

  const currentRange = () => host.__tbSelectionRange(documentOf());
  const currentDirection = () => host.__tbSelectionDirection(documentOf());

  const anchorPoint = (range, direction) => (direction === 'backward'
    ? point(range.endContainer, range.endOffset)
    : point(range.startContainer, range.startOffset));

  const focusPoint = (range, direction) => (direction === 'backward'
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
    return chain.toReversed();
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

  const setRange = (start, end, direction) => {
    host.__tbSelectionSetRange(
      documentOf(),
      start.node, start.offset,
      end.node, end.offset,
      direction,
    );
  };

  const setBoundaries = (anchor, focus) => {
    const order = comparePoints(anchor, focus);
    const start = order <= 0 ? anchor : focus;
    const end = order <= 0 ? focus : anchor;
    const direction = order < 0 ? 'forward' : (order > 0 ? 'backward' : 'forward');
    setRange(start, end, direction);
  };

  const requireRange = () => {
    const range = currentRange();
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
  const exposed = (targetPoint) => (inDocumentTree(targetPoint.node)
    ? targetPoint
    : { node: null, offset: 0 });

  const [Selection, selection] = __tbInstallGlobal('Selection', class Selection {
    get anchorNode() {
      const range = currentRange();
      return range === null ? null : exposed(anchorPoint(range, currentDirection())).node;
    }
    get anchorOffset() {
      const range = currentRange();
      return range === null ? 0 : exposed(anchorPoint(range, currentDirection())).offset;
    }
    get focusNode() {
      const range = currentRange();
      return range === null ? null : exposed(focusPoint(range, currentDirection())).node;
    }
    get focusOffset() {
      const range = currentRange();
      return range === null ? 0 : exposed(focusPoint(range, currentDirection())).offset;
    }
    // https://w3c.github.io/selection-api/#dom-selection-iscollapsed
    get isCollapsed() {
      const anchor = this.anchorNode;
      const focus = this.focusNode;
      return anchor === focus
        && (anchor === null || this.anchorOffset === this.focusOffset);
    }
    get rangeCount() {
      const range = currentRange();
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
      if (this.rangeCount === 0) return 'none';
      return currentDirection();
    }
    // https://w3c.github.io/selection-api/#dom-selection-getrangeat
    getRangeAt(index) {
      const range = currentRange();
      if (index !== 0 || range === null || !inDocumentTree(range.startContainer)
          || !inDocumentTree(range.endContainer)) {
        throw new DOMException('no range at this index', 'IndexSizeError');
      }
      // https://w3c.github.io/selection-api/#dom-selection-addrange
      return range;
    }
    addRange(candidate) {
      requireRangeArgument(candidate);
      const document = documentOf();
      const rootStart = candidate.startContainer.getRootNode();
      const rootEnd = candidate.endContainer.getRootNode();
      if (!sameNode(rootStart, document) || !sameNode(rootEnd, document)) {
        return;
      }
      if (this.rangeCount !== 0) return;
      host.__tbSelectionAddRange(document, candidate);
    }
    removeRange(candidate) {
      // https://webidl.spec.whatwg.org/#es-interface
      requireRangeArgument(candidate);
      if (!host.__tbSelectionRemoveRange(documentOf(), candidate)) {
        throw new DOMException('the range is not in the selection', 'NotFoundError');
      }
    }
    removeAllRanges() {
      host.__tbSelectionClear(documentOf());
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
      const index = offset >>> 0;
      if (index > lengthOf(node)) {
        throw new DOMException('offset is past the node length', 'IndexSizeError');
      }
      if (!inDocumentTree(node)) return;
      const collapsed = point(node, index);
      setRange(collapsed, collapsed, currentDirection());
    }
    setPosition(node, offset) {
      this.collapse(node, offset);
    }
    collapseToStart() {
      const current = requireRange();
      const start = point(current.startContainer, current.startOffset);
      setRange(start, start, currentDirection());
    }
    collapseToEnd() {
      const current = requireRange();
      const end = point(current.endContainer, current.endOffset);
      setRange(end, end, currentDirection());
    }
    // https://w3c.github.io/selection-api/#dom-selection-extend
    extend(node, offset) {
      if (!inDocumentTree(node)) return;
      const range = requireRange();
      const anchor = anchorPoint(range, currentDirection());
      const focus = point(node, offset);
      // https://w3c.github.io/selection-api/#dom-selection-extend
      if (!sameNode(focus.node.getRootNode(), range.startContainer.getRootNode())) {
        setRange(focus, focus, 'forward');
        return;
      }
      setBoundaries(anchor, focus);
    }
    // https://w3c.github.io/selection-api/#dom-selection-setbaseandextent
    setBaseAndExtent(anchorNode, anchorOffset, focusNode, focusOffset) {
      const anchorIndex = anchorOffset >>> 0;
      const focusIndex = focusOffset >>> 0;
      if (anchorIndex > lengthOf(anchorNode) || focusIndex > lengthOf(focusNode)) {
        throw new DOMException('offset is past the node length', 'IndexSizeError');
      }
      if (!inDocumentTree(anchorNode) || !inDocumentTree(focusNode)) return;
      setBoundaries(point(anchorNode, anchorIndex), point(focusNode, focusIndex));
    }
    // https://w3c.github.io/selection-api/#dom-selection-selectallchildren
    selectAllChildren(node) {
      checkNode(node);
      if (!sameNode(node.getRootNode(), documentOf())) return;
      const end = lengthOf(node);
      setRange(point(node, 0), point(node, end), 'forward');
    }
    // https://w3c.github.io/selection-api/#dom-selection-containsnode
    containsNode(node, allowPartialContainment) {
      const range = currentRange();
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
      const range = currentRange();
      if (range !== null) {
        range.deleteContents();
      }
    },
    writable: true, enumerable: true, configurable: true,
  });

  // https://w3c.github.io/selection-api/#dom-selection-stringifier
  __tbDefineProperty(Selection.prototype, 'toString', {
    value: function toString() {
      const range = currentRange();
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
