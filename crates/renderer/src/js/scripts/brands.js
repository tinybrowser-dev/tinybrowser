(function() {
  const inputFilesSymbol = host.slots('tinybrowser.input.files');
  function illegal() { throw new TypeError('Illegal constructor'); }
  function define(name, parent, constructible) {
    const ctor = constructible
      ? function() {
          const value = __tbApply(host.__tb_construct,
            globalThis,
            [name, ...arguments]
          );
          if (name === 'HTMLElement' && new.target && new.target.prototype) {
            __tbSetPrototypeOf(value, new.target.prototype);
          }
          return value;
        }
      : function() { return illegal(); };
    const proto = Object.create(parent ? parent.prototype : Object.prototype);
    Object.defineProperty(ctor, 'name', { value: name, configurable: true });
    Object.defineProperty(proto, 'constructor', { value: ctor, writable: true, configurable: true });
    Object.defineProperty(ctor, 'prototype', { value: proto, writable: false });
    // Interface prototypes carry @@toStringTag
    // (<https://webidl.spec.whatwg.org/#es-interface>), so
    // Object.prototype.toString says `[object HTMLDivElement]`.
    Object.defineProperty(proto, Symbol.toStringTag, {
      value: name, writable: false, enumerable: false, configurable: true,
    });
    Object.defineProperty(globalThis, name, { value: ctor, writable: true, configurable: true });
    return ctor;
  }
  const EventTargetInterface = globalThis.EventTarget;
  const NodeInterface = define('Node', EventTargetInterface);
  for (const [name, value] of [
    ['ELEMENT_NODE', 1],
    ['ATTRIBUTE_NODE', 2],
    ['TEXT_NODE', 3],
    ['CDATA_SECTION_NODE', 4],
    ['ENTITY_REFERENCE_NODE', 5],
    ['ENTITY_NODE', 6],
    ['PROCESSING_INSTRUCTION_NODE', 7],
    ['COMMENT_NODE', 8],
    ['DOCUMENT_NODE', 9],
    ['DOCUMENT_TYPE_NODE', 10],
    ['DOCUMENT_FRAGMENT_NODE', 11],
    ['NOTATION_NODE', 12],
    ['DOCUMENT_POSITION_DISCONNECTED', 1],
    ['DOCUMENT_POSITION_PRECEDING', 2],
    ['DOCUMENT_POSITION_FOLLOWING', 4],
    ['DOCUMENT_POSITION_CONTAINS', 8],
    ['DOCUMENT_POSITION_CONTAINED_BY', 16],
    ['DOCUMENT_POSITION_IMPLEMENTATION_SPECIFIC', 32],
  ]) {
    Object.defineProperty(NodeInterface, name, { value, writable: false, configurable: true });
    Object.defineProperty(NodeInterface.prototype, name, { value, writable: false, configurable: true });
  }
  const DocumentInterface = define('Document', NodeInterface, true);
  const HTMLDocumentInterface = define('HTMLDocument', DocumentInterface);
  const ElementInterface = define('Element', NodeInterface);
  const XMLDocumentInterface = define('XMLDocument', DocumentInterface);
  const CharacterDataInterface = define('CharacterData', NodeInterface);
  const TextInterface = define('Text', CharacterDataInterface, true);
  const CDATASectionInterface = define('CDATASection', TextInterface);
  const ProcessingInstructionInterface = define('ProcessingInstruction', CharacterDataInterface);
  const CommentInterface = define('Comment', CharacterDataInterface, true);
  const DocumentTypeInterface = define('DocumentType', NodeInterface);
  const DocumentFragmentInterface = define('DocumentFragment', NodeInterface, true);
  const ShadowRootInterface = define('ShadowRoot', DocumentFragmentInterface);
  const AbstractRangeInterface = define('AbstractRange');
  const RangeInterface = define('Range', AbstractRangeInterface, true);
  const HTMLElementInterface = define('HTMLElement', ElementInterface, true);
  const HTMLUnknownElementInterface = define('HTMLUnknownElement', HTMLElementInterface);
  const HTMLMediaElementInterface = define('HTMLMediaElement', HTMLElementInterface);
  const SVGElementInterface = define('SVGElement', ElementInterface);
  const MathMLElementInterface = define('MathMLElement', ElementInterface);
  const table = {
    Document: DocumentInterface.prototype,
    HTMLDocument: HTMLDocumentInterface.prototype,
    XMLDocument: XMLDocumentInterface.prototype,
    Element: ElementInterface.prototype,
    CharacterData: CharacterDataInterface.prototype,
    Text: TextInterface.prototype,
    CDATASection: CDATASectionInterface.prototype,
    ProcessingInstruction: ProcessingInstructionInterface.prototype,
    Comment: CommentInterface.prototype,
    DocumentType: DocumentTypeInterface.prototype,
    DocumentFragment: DocumentFragmentInterface.prototype,
    ShadowRoot: ShadowRootInterface.prototype,
    AbstractRange: AbstractRangeInterface.prototype,
    Range: RangeInterface.prototype,
    HTMLElement: HTMLElementInterface.prototype,
    HTMLUnknownElement: HTMLUnknownElementInterface.prototype,
    HTMLMediaElement: HTMLMediaElementInterface.prototype,
    SVGElement: SVGElementInterface.prototype,
    MathMLElement: MathMLElementInterface.prototype,
  };
  // Every element interface chains to HTMLElement except the media pair,
  // which chains through HTMLMediaElement, and SVG, which chains to Element.
  // Members arrive from the generated contract installers, which run after
  // this script creates the constructors and prototypes.
  // `type` is defined per interface below; the form-control states live
  // here so each interface exposes exactly the reflecting attributes the
  // spec gives it (<https://html.spec.whatwg.org/#the-disabled-attribute>).
  for (const [name, parent] of [
    ['HTMLAnchorElement', HTMLElementInterface],
    ['HTMLAreaElement', HTMLElementInterface],
    ['HTMLAudioElement', HTMLMediaElementInterface],
    ['HTMLBaseElement', HTMLElementInterface],
    ['HTMLBodyElement', HTMLElementInterface],
    ['HTMLBRElement', HTMLElementInterface],
    ['HTMLButtonElement', HTMLElementInterface],
    ['HTMLCanvasElement', HTMLElementInterface],
    ['HTMLDataElement', HTMLElementInterface],
    ['HTMLDataListElement', HTMLElementInterface],
    ['HTMLDetailsElement', HTMLElementInterface],
    ['HTMLDialogElement', HTMLElementInterface],
    ['HTMLDirectoryElement', HTMLElementInterface],
    ['HTMLDivElement', HTMLElementInterface],
    ['HTMLDListElement', HTMLElementInterface],
    ['HTMLEmbedElement', HTMLElementInterface],
    ['HTMLFieldSetElement', HTMLElementInterface],
    ['HTMLFontElement', HTMLElementInterface],
    ['HTMLFormElement', HTMLElementInterface],
    ['HTMLFrameElement', HTMLElementInterface],
    ['HTMLFrameSetElement', HTMLElementInterface],
    ['HTMLHeadingElement', HTMLElementInterface],
    ['HTMLHeadElement', HTMLElementInterface],
    ['HTMLHRElement', HTMLElementInterface],
    ['HTMLHtmlElement', HTMLElementInterface],
    ['HTMLIFrameElement', HTMLElementInterface],
    ['HTMLImageElement', HTMLElementInterface],
    ['HTMLInputElement', HTMLElementInterface],
    ['HTMLLabelElement', HTMLElementInterface],
    ['HTMLLIElement', HTMLElementInterface],
    ['HTMLLegendElement', HTMLElementInterface],
    ['HTMLLinkElement', HTMLElementInterface],
    ['HTMLMapElement', HTMLElementInterface],
    ['HTMLMarqueeElement', HTMLElementInterface],
    ['HTMLMenuElement', HTMLElementInterface],
    ['HTMLMetaElement', HTMLElementInterface],
    ['HTMLMeterElement', HTMLElementInterface],
    ['HTMLModElement', HTMLElementInterface],
    ['HTMLObjectElement', HTMLElementInterface],
    ['HTMLOListElement', HTMLElementInterface],
    ['HTMLOptGroupElement', HTMLElementInterface],
    ['HTMLOptionElement', HTMLElementInterface],
    ['HTMLOutputElement', HTMLElementInterface],
    ['HTMLParagraphElement', HTMLElementInterface],
    ['HTMLParamElement', HTMLElementInterface],
    ['HTMLPictureElement', HTMLElementInterface],
    ['HTMLPreElement', HTMLElementInterface],
    ['HTMLProgressElement', HTMLElementInterface],
    ['HTMLQuoteElement', HTMLElementInterface],
    ['HTMLScriptElement', HTMLElementInterface],
    ['HTMLSelectElement', HTMLElementInterface],
    ['HTMLSelectedContentElement', HTMLElementInterface],
    ['HTMLSourceElement', HTMLElementInterface],
    ['HTMLSlotElement', HTMLElementInterface],
    ['HTMLSpanElement', HTMLElementInterface],
    ['HTMLStyleElement', HTMLElementInterface],
    ['HTMLTableCaptionElement', HTMLElementInterface],
    ['HTMLTableCellElement', HTMLElementInterface],
    ['HTMLTableColElement', HTMLElementInterface],
    ['HTMLTableElement', HTMLElementInterface],
    ['HTMLTableRowElement', HTMLElementInterface],
    ['HTMLTableSectionElement', HTMLElementInterface],
    ['HTMLTemplateElement', HTMLElementInterface],
    ['HTMLTextAreaElement', HTMLElementInterface],
    ['HTMLTimeElement', HTMLElementInterface],
    ['HTMLTitleElement', HTMLElementInterface],
    ['HTMLTrackElement', HTMLElementInterface],
    ['HTMLUListElement', HTMLElementInterface],
    ['HTMLVideoElement', HTMLMediaElementInterface],
  ]) {
    table[name] = define(name, parent).prototype;
  }
  // URL decomposition IDL attributes
  // (<https://html.spec.whatwg.org/multipage/links.html#url-decomposition-idl-attributes>).
  // A href that fails to parse makes `protocol` ":" and every other getter
  // empty; setting a component that the parse or the component rejects
  // leaves the attribute untouched.
  {
    const parts = {
      protocol: 0, username: 1, password: 2, host: 3, hostname: 4,
      port: 5, pathname: 6, search: 7, hash: 8, origin: 9,
    };
    const hrefValue = function() {
      const value = this.getAttribute('href');
      return value === null ? null : host.__tbUSVString(value);
    };
    const base = function() { return host.__tbUSVString(document.baseURI); };
    for (const proto of [table.HTMLAnchorElement, table.HTMLAreaElement]) {
      for (const name of Object.keys(parts)) {
        const index = parts[name];
        const descriptor = {
          get: function() {
            // An absent `href` reports the URL-decomposition defaults, not
            // the document URL.
            const href = hrefValue.call(this);
            if (href === null) return name === 'protocol' ? ':' : '';
            const values = host.__tbUrlParts(href, base.call(this));
            if (values === null || values === undefined) {
              return name === 'protocol' ? ':' : '';
            }
            return values[index];
          },
          enumerable: true,
          configurable: true,
        };
        if (name !== 'origin') {
          descriptor.set = function(value) {
            // Setting a component with no `href` attribute is a no-op.
            const href = hrefValue.call(this);
            if (href === null) return;
            const result = host.__tbUrlSetPart(
              href, base.call(this), index, host.__tbUSVString(value));
            if (result !== null && result !== undefined) this.setAttribute('href', result);
          };
        }
        Object.defineProperty(proto, name, descriptor);
      }
    }
  }
  // `type` reflects the content attribute, limited to only known values with
  // ASCII case-insensitive matching (not Unicode folding: Kelvin K must not
  // match `k`)
  // (<https://html.spec.whatwg.org/multipage/common-dom-interfaces.html#limited-to-only-known-values>,
  // <https://html.spec.whatwg.org/multipage/input.html#dom-input-type>,
  // <https://html.spec.whatwg.org/multipage/form-elements.html#dom-button-type>).
  function reflectType(keywords, fallback) {
    return {
      get: function() {
        const value = this.getAttributeNS(null, 'type');
        if (value === null) return fallback;
        const lowered = __tbAsciiLower(value);
        return keywords.has(lowered) ? lowered : fallback;
      },
      // The setter reflects verbatim: no lowercasing, only conversion.
      set: function(value) { this.setAttribute('type', __tbToDOMString(value)); },
      enumerable: true,
      configurable: true,
    };
  }
  const INPUT_TYPE_KEYWORDS = new Set([
    'hidden', 'text', 'search', 'tel', 'url', 'email', 'password',
    'date', 'month', 'week', 'time', 'datetime-local', 'number', 'range',
    'color', 'checkbox', 'radio', 'file', 'submit', 'image', 'reset', 'button',
  ]);
  Object.defineProperty(table.HTMLInputElement, 'type', {
    get: function() {
      const value = this.getAttributeNS(null, 'type');
      if (value === null) return 'text';
      const lowered = __tbAsciiLower(value);
      return INPUT_TYPE_KEYWORDS.has(lowered) ? lowered : 'text';
    },
    set: function(value) {
      // The `type` attribute change runs the input type-change steps in the
      // DOM, whatever the mutation path
      // (<https://html.spec.whatwg.org/multipage/input.html#the-input-element:type-change-state>).
      this.setAttribute('type', __tbToDOMString(value));
    },
    enumerable: true,
    configurable: true,
  });
  // `<input type=file>` exposes a (possibly empty) FileList
  // (<https://html.spec.whatwg.org/multipage/input.html#dom-input-files>).
  Object.defineProperty(table.HTMLInputElement, 'files', {
    get: function() {
      // `files` applies only to the File Upload state
      // (<https://html.spec.whatwg.org/multipage/input.html#dom-input-files>).
      if (this.type !== 'file') return null;
      let list = inputFilesSymbol.get(this);
      if (list === undefined) {
        list = host.__tbCreateFileList([]);
        inputFilesSymbol.set(this, list);
      }
      return list;
    },
    set: function(value) {
      if (this.type !== 'file') return;
      if (value === null) return;
      if (!(value instanceof globalThis.FileList)) {
        throw new TypeError('files must be a FileList');
      }
      // `input.files = fileList` replaces the list a script set; the same list
      // can be shared across inputs
      // (<https://html.spec.whatwg.org/multipage/input.html#dom-input-files>).
      inputFilesSymbol.set(this, value);
      host.__tbSetInputFiles(this, value);
    },
    enumerable: true,
    configurable: true,
  });
  Object.defineProperty(table.HTMLButtonElement, 'type', reflectType(new Set([
    'submit', 'reset', 'button',
  ]), 'submit'));
  // <https://html.spec.whatwg.org/multipage/form-elements.html#dom-textarea-type>
  Object.defineProperty(table.HTMLTextAreaElement, 'type', {
    get: function() { return 'textarea'; },
    enumerable: true,
    configurable: true,
  });
  // The `hidden` IDL is a nullable `(boolean or unrestricted double or
  // DOMString)` union the generator does not lower yet, so the getter and
  // setter steps live here until it does
  // (<https://html.spec.whatwg.org/multipage/interaction.html#dom-hidden>).
  Object.defineProperty(table.HTMLElement, 'hidden', {
    get: function() {
      const value = this.getAttribute('hidden');
      if (value === null) return false;
      if (__tbAsciiLower(value) === 'until-found') return 'until-found';
      return true;
    },
    set: function(value) {
      // Nullable union conversion: null and undefined empty to null,
      // booleans and numbers keep their type, everything else stringifies.
      let converted;
      if (value === null || value === undefined) converted = null;
      else if (typeof value === 'boolean' || typeof value === 'number') converted = value;
      else converted = __tbToDOMString(value);
      if (typeof converted === 'string' && __tbAsciiLower(converted) === 'until-found') {
        this.setAttribute('hidden', 'until-found');
      } else if (
        converted === false || converted === '' || converted === null ||
        converted === 0 || Number.isNaN(converted)
      ) {
        this.removeAttribute('hidden');
      } else {
        this.setAttribute('hidden', '');
      }
    },
    enumerable: true,
    configurable: true,
  });
  // Slot assignment is not implemented. `assignedNodes` filters the host's
  // light-DOM children by the `slot` attribute.
  Object.defineProperties(table.HTMLSlotElement, {
    assignedNodes: {
      value: function() {
        const host = this.getRootNode()?.host;
        if (!host) return [];
        const name = this.getAttribute('name') || '';
        return Array.from(host.childNodes).filter(node =>
          node.nodeType !== 1 ? name === '' : (node.getAttribute('slot') || '') === name);
      },
      writable: true, configurable: true, enumerable: true,
    },
    assignedElements: {
      value: function() { return this.assignedNodes().filter(node => node.nodeType === 1); },
      writable: true, configurable: true, enumerable: true,
    },
  });
  Object.defineProperty(host, '__tb_brandTable', {
    enumerable: false,
    configurable: true,
    value: table,
  });
})();
