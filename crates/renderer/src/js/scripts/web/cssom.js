  // `window` viewport metrics track the live document viewport, so CDP
  // emulation changes are visible without re-installing globals
  // (<https://drafts.csswg.org/cssom-view/#dom-window-innerwidth>).
  const __tbViewportSize = () => host.__tb_viewport_size();
  // A `[Replaceable]` attribute is enumerable, and assigning it replaces the
  // accessor with an own data property
  // (<https://webidl.spec.whatwg.org/#dfn-replaceable>).
  const __tbReplaceable = (name, get) => {
    Object.defineProperty(globalThis, name, {
      get,
      set(value) {
        Object.defineProperty(globalThis, name, {
          value, writable: true, enumerable: true, configurable: true,
        });
      },
      enumerable: true, configurable: true,
    });
  };
  __tbReplaceable('innerWidth', () => __tbViewportSize()[0]);
  __tbReplaceable('innerHeight', () => __tbViewportSize()[1]);
  // No browser chrome exists, so the outer window equals the inner viewport
  // (<https://drafts.csswg.org/cssom-view/#dom-window-outerwidth>).
  __tbReplaceable('outerWidth', () => __tbViewportSize()[0]);
  __tbReplaceable('outerHeight', () => __tbViewportSize()[1]);

  // ── screen ───────────────────────────────────────────────────────────
  // There is no physical display or browser chrome, so the web-exposed screen
  // is the viewport and the available area equals it
  // (<https://drafts.csswg.org/cssom-view/#the-screen-interface>).
  const __tbScreen = (() => {
    function Screen() {
      throw new TypeError('Illegal constructor');
    }
    Screen.prototype = Object.create(Object.prototype, {
      constructor: { value: Screen, writable: true, configurable: true },
    });
    __tbDefineProperty(Screen.prototype, Symbol.toStringTag, {
      value: 'Screen', writable: false, enumerable: false, configurable: true,
    });
    const size = () => __tbViewportSize();
    for (const [name, get] of [
      ['availWidth', () => size()[0]],
      ['availHeight', () => size()[1]],
      ['width', () => size()[0]],
      ['height', () => size()[1]],
      // 24-bit sRGB is the headless compositor's color depth.
      ['colorDepth', () => 24],
      ['pixelDepth', () => 24],
    ]) {
      __tbDefineProperty(Screen.prototype, name, {
        get, enumerable: true, configurable: true,
      });
    }
    __tbDefineProperty(globalThis, 'Screen', {
      value: Screen, writable: true, configurable: true,
    });
    return Object.create(Screen.prototype);
  })();
  // `screen` is `[SameObject, Replaceable]`: the getter returns one object, and
  // assigning replaces the accessor with an own property
  // (<https://webidl.spec.whatwg.org/#dfn-replaceable>).
  __tbReplaceable('screen', () => __tbScreen);

  // ── viewport scrolling ───────────────────────────────────────────────
  // `scrollX`/`scrollY` and their `page*Offset` aliases report the viewport's
  // scroll offset, which in quirks mode is the body element's
  // (<https://drafts.csswg.org/cssom-view/#dom-window-scrollx>).
  const __tbViewportScroll = () => host.__tb_viewport_scroll();
  __tbReplaceable('scrollX', () => __tbViewportScroll()[0]);
  __tbReplaceable('pageXOffset', () => __tbViewportScroll()[0]);
  __tbReplaceable('scrollY', () => __tbViewportScroll()[1]);
  __tbReplaceable('pageYOffset', () => __tbViewportScroll()[1]);

  // An IDL `ScrollBehavior`; an unknown spelling is a conversion failure
  // (<https://drafts.csswg.org/cssom-view/#enumdef-scrollbehavior>).
  const __tbScrollBehavior = value => {
    if (value === undefined) return 'auto';
    const behavior = __tbToDOMString(value);
    if (behavior !== 'auto' && behavior !== 'instant' && behavior !== 'smooth') {
      throw new TypeError(`'${behavior}' is not a valid value for enumeration ScrollBehavior.`);
    }
    return behavior;
  };

  // `unrestricted double` conversion: ToNumber, and "normalize non-finite
  // values" replaces NaN and the infinities with zero
  // (<https://webidl.spec.whatwg.org/#es-unrestricted-double>,
  // <https://drafts.csswg.org/cssom-view/#normalize-non-finite-values>).
  const __tbUnrestrictedDouble = value => {
    if (typeof value === 'bigint') {
      throw new TypeError('Cannot convert a BigInt value to a number');
    }
    const number = Number(value);
    return Number.isFinite(number) ? number : 0;
  };

  // Convert one argument as `ScrollToOptions`. A non-object is a conversion
  // failure, which the scroll methods report by rejecting their promise
  // (<https://webidl.spec.whatwg.org/#es-dictionary>).
  const __tbScrollOptions = value => {
    if (value === undefined || value === null) return { behavior: 'auto' };
    if (typeof value !== 'object' && typeof value !== 'function') {
      throw new TypeError('ScrollToOptions must be an object');
    }
    const options = { behavior: __tbScrollBehavior(value.behavior) };
    if (value.left !== undefined) options.left = __tbUnrestrictedDouble(value.left);
    if (value.top !== undefined) options.top = __tbUnrestrictedDouble(value.top);
    return options;
  };

  // The shared body of `scroll`/`scrollTo`/`scrollBy` for both the element and
  // the window: an overload set of one optional dictionary and one
  // `(unrestricted double, unrestricted double)` pair
  // (<https://drafts.csswg.org/cssom-view/#dom-element-scroll>).
  const __tbScroll = (target, receiver, args, relative, receiverCheck) => {
    try {
      // A null or undefined `this` is replaced with the realm's global object
      // before the interface check, which is why a bare `scrollTo(...)` in a
      // sloppy-mode script (where `this` is the global object) and in a strict
      // one (where it is undefined) both work
      // (<https://webidl.spec.whatwg.org/#es-operations>).
      if (receiver === null || receiver === undefined) receiver = globalThis;
      // The receiver check comes first, and a failure rejects like every other
      // error in a promise-returning operation
      // (<https://webidl.spec.whatwg.org/#es-operations>).
      if (!receiverCheck(receiver)) throw new TypeError('Illegal invocation');
      let x;
      let y;
      let behavior;
      if (args.length >= 2) {
        // The two-argument overload is chosen by argument count, then each
        // argument converts as `unrestricted double`, so a numeric string or
        // `null` is accepted and a BigInt rejects.
        x = __tbUnrestrictedDouble(args[0]);
        y = __tbUnrestrictedDouble(args[1]);
        behavior = 'auto';
      } else {
        const options = __tbScrollOptions(args[0]);
        behavior = options.behavior;
        x = options.left;
        y = options.top;
      }
      // `scrollBy` adds each member that was given to the current position; an
      // absent member leaves its axis where it is. `scroll` treats an absent
      // member the same way, by resolving it to the current position.
      if (relative) {
        x = target.current(receiver, false) + (x === undefined ? 0 : x);
        y = target.current(receiver, true) + (y === undefined ? 0 : y);
      } else {
        if (x === undefined) x = target.current(receiver, false);
        if (y === undefined) y = target.current(receiver, true);
      }
      target.scrollTo(receiver, x, y, behavior);
      return Promise.resolve(undefined);
    } catch (error) {
      // A promise-returning operation rejects on a conversion failure instead
      // of throwing (<https://webidl.spec.whatwg.org/#es-operations>).
      return Promise.reject(error);
    }
  };

  const __tbWindowScrollTarget = {
    current: (_receiver, vertical) => __tbViewportScroll()[vertical ? 1 : 0],
    scrollTo: (_receiver, x, y, behavior) => host.__tb_scroll_viewport(x, y, behavior),
  };

  // The Web IDL receiver check happens before argument conversion, so a
  // misused method throws instead of returning a rejected promise
  // (<https://webidl.spec.whatwg.org/#es-operations>).
  const __tbDefineScrollMethods = (object, target, receiverCheck) => {
    const method = (name, relative) => ({
      [name](...args) {
        return __tbScroll(target, this, args, relative, receiverCheck);
      },
    })[name];
    for (const name of ['scroll', 'scrollTo']) {
      Object.defineProperty(object, name, {
        value: method(name, false),
        writable: true, enumerable: true, configurable: true,
      });
    }
    Object.defineProperty(object, 'scrollBy', {
      value: method('scrollBy', true),
      writable: true, enumerable: true, configurable: true,
    });
  };

  // Another frame's `contentWindow` is a proxy whose `window` points at
  // itself, so accept it as a Window too; its members forward to the target
  // realm (<https://html.spec.whatwg.org/multipage/window-object.html#the-windowproxy-exotic-object>).
  __tbDefineScrollMethods(
    globalThis, __tbWindowScrollTarget,
    receiver => receiver === globalThis || (receiver !== null && receiver.window === receiver));
  // Declare the element operations here rather than through the IDL contract:
  // the binder emits one implementation per operation name, and these have
  // two overloads that the shared `__tbScroll` dispatches
  // (<https://drafts.csswg.org/cssom-view/#extension-to-the-element-interface>).
  __tbDefineScrollMethods(globalThis.Element.prototype, {
    current: (element, vertical) => host.__tb_element_scroll_position(element, vertical),
    scrollTo: (element, x, y, behavior) => host.__tb_scroll_element(element, x, y, behavior),
  }, receiver => globalThis.Element.prototype.isPrototypeOf(receiver));

// Constructable stylesheet surface used by component libraries, plus the
// associated CSS style sheet for `<style>` and `<link rel=stylesheet>`.
// https://drafts.csswg.org/cssom/#the-cssstylesheet-interface
// Addressing helpers the fonts shim reads: a CSS rule's own address, and the
// rule object an address names. Assigned inside the block below, which owns
// the sheet and rule registries they read.
let __tbCssomRuleAddress;
let __tbCssomRuleAt;
{
  // Captured at realm init so a page-patched builtin cannot retarget a rule
  // path or swallow an exception.
  const __tbReflectGet = Reflect.get;
  const __tbReflectHas = Reflect.has;
  const __tbReflectSet = Reflect.set;
  const __tbReflectDelete = Reflect.deleteProperty;
  const __tbBind = Function.prototype.bind;
  const __tbDomException = globalThis.DOMException;
  const __tbCharCodeAt = String.prototype.charCodeAt;
  const __tbSheetsById = new Map();
  // The node a host call addresses the document through. An associated sheet
  // uses its owner element; a sheet the `CSSStyleSheet` constructor built, or
  // one reached through `@import`, has no owner and keeps the document wrapper
  // as its address
  // (<https://drafts.csswg.org/cssom/#dom-cssstylesheet-cssstylesheet>).
  const __tbSheetAddress = sheet => sheet.__tbAddress ?? sheet.__tbOwner;
  const __tbRulePath = path => __tbArray.join(path, ',');
  function __tbIllegal() {
    throw new TypeError('Illegal constructor');
  }
  function CSSRule() { __tbIllegal(); }
  function CSSStyleRule() { __tbIllegal(); }
  function CSSGroupingRule() { __tbIllegal(); }
  function CSSConditionRule() { __tbIllegal(); }
  function CSSMediaRule() { __tbIllegal(); }
  function CSSSupportsRule() { __tbIllegal(); }
  function CSSImportRule() { __tbIllegal(); }
  function CSSFontFaceRule() { __tbIllegal(); }
  function CSSKeyframesRule() { __tbIllegal(); }
  function CSSKeyframeRule() { __tbIllegal(); }
  // Rule interfaces the style engine can report but that had no global. Each
  // inherits per its own specification; the rule's `type` still comes from
  // the engine's snapshot
  // (<https://drafts.csswg.org/cssom/#the-cssrule-interface>,
  // <https://drafts.csswg.org/css-page-3/#omitted>,
  // <https://drafts.csswg.org/css-cascade-5/#layer-rule>).
  function CSSNamespaceRule() { __tbIllegal(); }
  function CSSPageRule() { __tbIllegal(); }
  function CSSCounterStyleRule() { __tbIllegal(); }
  function CSSLayerBlockRule() { __tbIllegal(); }
  function CSSLayerStatementRule() { __tbIllegal(); }
  function CSSContainerRule() { __tbIllegal(); }
  function CSSScopeRule() { __tbIllegal(); }
  function CSSCustomMediaRule() { __tbIllegal(); }
  function CSSFontFeatureValuesRule() { __tbIllegal(); }
  function CSSFontPaletteValuesRule() { __tbIllegal(); }
  function CSSMarginRule() { __tbIllegal(); }
  function CSSPropertyRule() { __tbIllegal(); }
  function CSSStartingStyleRule() { __tbIllegal(); }
  function CSSPositionTryRule() { __tbIllegal(); }
  function CSSViewTransitionRule() { __tbIllegal(); }
  function CSSNestedDeclarations() { __tbIllegal(); }
  function CSSMozDocumentRule() { __tbIllegal(); }
  function CSSAppearanceBaseRule() { __tbIllegal(); }
  function CSSStyleDeclaration() { __tbIllegal(); }
  CSSStyleRule.prototype = Object.create(CSSRule.prototype);
  CSSGroupingRule.prototype = Object.create(CSSRule.prototype);
  CSSConditionRule.prototype = Object.create(CSSGroupingRule.prototype);
  CSSMediaRule.prototype = Object.create(CSSConditionRule.prototype);
  CSSSupportsRule.prototype = Object.create(CSSConditionRule.prototype);
  CSSContainerRule.prototype = Object.create(CSSConditionRule.prototype);
  CSSImportRule.prototype = Object.create(CSSRule.prototype);
  CSSFontFaceRule.prototype = Object.create(CSSRule.prototype);
  CSSKeyframesRule.prototype = Object.create(CSSRule.prototype);
  CSSKeyframeRule.prototype = Object.create(CSSRule.prototype);
  CSSPageRule.prototype = Object.create(CSSGroupingRule.prototype);
  CSSLayerBlockRule.prototype = Object.create(CSSGroupingRule.prototype);
  CSSScopeRule.prototype = Object.create(CSSGroupingRule.prototype);
  CSSStartingStyleRule.prototype = Object.create(CSSGroupingRule.prototype);
  CSSMozDocumentRule.prototype = Object.create(CSSConditionRule.prototype);
  for (const ctor of [
    CSSNamespaceRule, CSSCounterStyleRule, CSSLayerStatementRule,
    CSSCustomMediaRule, CSSFontFeatureValuesRule, CSSFontPaletteValuesRule,
    CSSMarginRule, CSSPropertyRule, CSSPositionTryRule, CSSViewTransitionRule,
    CSSNestedDeclarations, CSSAppearanceBaseRule,
  ]) {
    ctor.prototype = Object.create(CSSRule.prototype);
  }
  // `CSSRule`'s rule-type constants. WebIDL exposes a constant on both the
  // interface object and the interface prototype object
  // (<https://webidl.spec.whatwg.org/#idl-constants>,
  // <https://drafts.csswg.org/cssom/#the-cssrule-interface>).
  for (const [name, value] of [
    ['STYLE_RULE', 1],
    ['CHARSET_RULE', 2],
    ['IMPORT_RULE', 3],
    ['MEDIA_RULE', 4],
    ['FONT_FACE_RULE', 5],
    ['PAGE_RULE', 6],
    ['KEYFRAMES_RULE', 7],
    ['KEYFRAME_RULE', 8],
    ['NAMESPACE_RULE', 10],
    ['COUNTER_STYLE_RULE', 11],
    ['SUPPORTS_RULE', 12],
    ['FONT_FEATURE_VALUES_RULE', 14],
  ]) {
    for (const target of [CSSRule, CSSRule.prototype]) {
      __tbDefineProperty(target, name, {
        value, writable: false, enumerable: true, configurable: false,
      });
    }
  }
  // `CSSRuleList` is an interface object with no constructor; a live list is
  // its instance (<https://drafts.csswg.org/cssom/#the-cssrulelist-interface>).
  function CSSRuleList() { __tbIllegal(); }
  Object.defineProperty(globalThis, 'CSSRuleList', {
    value: CSSRuleList, writable: true, configurable: true,
  });
  // Each interface owns its `constructor`, so `rule.constructor.name` names
  // the rule's real interface instead of inheriting `CSSRule`
  // (<https://webidl.spec.whatwg.org/#interface-object>).
  for (const ctor of [
    CSSStyleRule, CSSGroupingRule, CSSConditionRule, CSSMediaRule, CSSSupportsRule,
    CSSImportRule, CSSFontFaceRule, CSSKeyframesRule, CSSKeyframeRule,
    CSSNamespaceRule, CSSPageRule, CSSCounterStyleRule, CSSLayerBlockRule,
    CSSLayerStatementRule, CSSContainerRule, CSSScopeRule, CSSCustomMediaRule,
    CSSFontFeatureValuesRule, CSSFontPaletteValuesRule, CSSMarginRule,
    CSSPropertyRule, CSSStartingStyleRule, CSSPositionTryRule,
    CSSViewTransitionRule, CSSNestedDeclarations, CSSMozDocumentRule,
    CSSAppearanceBaseRule,
  ]) {
    __tbDefineProperty(ctor.prototype, 'constructor', {
      value: ctor, writable: true, configurable: true,
    });
  }
  // The sheet an `@import` rule loaded. The engine answers a CSSOM sheet record
  // for the imported sheet, which has no owner element: host calls address the
  // document through the importing sheet's owner, while `ownerNode` stays null
  // (<https://drafts.csswg.org/cssom/#dom-cssimportrule-stylesheet>).
  const __tbImportedSheet = entry => {
    const id = host.__tb_sheet_import_sheet(
      __tbSheetAddress(entry.sheet), entry.sheet.__tbId, __tbRulePath(entry.path),
    );
    if (!(id >= 1)) return null;
    const sheet = Object.create(CSSStyleSheet.prototype);
    sheet.__tbRules = [];
    sheet.__tbId = id;
    // Host calls need a node to reach the document; the imported sheet is not
    // owned by one, so `ownerNode` reports null while the importing sheet's
    // owner is only an address
    sheet.__tbAddress = __tbSheetAddress(entry.sheet);
    sheet.__tbParent = entry.sheet;
    sheet.__tbOwnerRule = entry.rule;
    sheet.__tbOwnerRuleEntry = entry;
    const info = entry.read();
    sheet.__tbHref = info && info.href ? info.href : null;
    // `CSSImportRule.media` and the imported sheet's `media` are the same
    // object (<https://drafts.csswg.org/cssom/#dom-cssimportrule-media>).
    sheet.__tbMedia = entry.rule.media;
    return sheet;
  };
  __tbDefineProperty(CSSImportRule.prototype, 'styleSheet', {
    get() {
      const entry = __tbRuleState(this);
      if (entry.detached) return null;
      // An import that has not loaded yet answers null, and is retried next
      // time rather than cached
      // (<https://drafts.csswg.org/cssom/#dom-cssimportrule-stylesheet>).
      if (entry.importedSheet == null) entry.importedSheet = __tbImportedSheet(entry) ?? undefined;
      return entry.importedSheet ?? null;
    },
    enumerable: true, configurable: true,
  });
  // CSSRule's own attributes are accessors on the interface prototype
  // (<https://drafts.csswg.org/cssom/#the-cssrule-interface>).
  __tbDefineProperty(CSSRule.prototype, 'type', {
    get() {
      const entry = __tbRuleState(this);
      const next = entry.read();
      return next ? next.type : entry.info.type;
    },
    enumerable: true, configurable: true,
  });
  __tbDefineProperty(CSSRule.prototype, 'cssText', {
    get() {
      const entry = __tbRuleState(this);
      return entry.detached ? entry.frozenText : (entry.read() || {}).cssText || '';
    },
    enumerable: true, configurable: true,
  });
  __tbDefineProperty(CSSRule.prototype, 'parentRule', {
    get() { return __tbRuleState(this).parentRule; },
    enumerable: true, configurable: true,
  });
  __tbDefineProperty(CSSRule.prototype, 'parentStyleSheet', {
    get() {
      const entry = __tbRuleState(this);
      return entry.detached ? null : entry.sheet;
    },
    enumerable: true, configurable: true,
  });
  // One accessor per string attribute an interface exposes. The property
  // exists even when the current snapshot omits the value, as WebIDL requires
  // (<https://webidl.spec.whatwg.org/#idl-attributes>).
  const __tbRuleStringAttribute = (ctor, names) => {
    for (const name of names) {
      __tbDefineProperty(ctor.prototype, name, {
        get() {
          const next = __tbRuleState(this).read();
          const value = next ? next[name] : '';
          return value === undefined || value === null ? '' : value;
        },
        enumerable: true, configurable: true,
      });
    }
  };
  __tbRuleStringAttribute(CSSConditionRule, ['conditionText']);
  __tbRuleStringAttribute(CSSScopeRule, ['start', 'end']);
  __tbRuleStringAttribute(CSSContainerRule, ['containerName', 'containerQuery']);
  __tbRuleStringAttribute(CSSImportRule, ['href', 'layerName', 'supportsText']);
  __tbRuleStringAttribute(CSSNamespaceRule, ['namespaceURI', 'prefix']);
  __tbRuleStringAttribute(CSSLayerBlockRule, ['name']);
  __tbRuleStringAttribute(CSSLayerStatementRule, ['nameList']);
  __tbRuleStringAttribute(CSSCounterStyleRule, [
    'name', 'system', 'symbols', 'additiveSymbols', 'negative', 'prefix',
    'suffix', 'range', 'pad', 'fallback', 'speakAs',
  ]);
  __tbRuleStringAttribute(CSSPropertyRule, ['name', 'syntax', 'initialValue']);
  __tbRuleStringAttribute(CSSFontPaletteValuesRule, ['name']);
  __tbRuleStringAttribute(CSSMarginRule, ['name']);
  __tbRuleStringAttribute(CSSPositionTryRule, ['name']);
  __tbRuleStringAttribute(CSSCustomMediaRule, ['name']);
  __tbRuleStringAttribute(CSSViewTransitionRule, ['types']);
  // The keyframes name is settable and serialization quotes it when it is
  // not a valid `<custom-ident>`
  // (<https://drafts.csswg.org/css-animations-1/#dom-csskeyframesrule-name>).
  __tbDefineProperty(CSSKeyframesRule.prototype, 'name', {
    get() {
      const next = __tbRuleState(this).read();
      return next && next.name ? next.name : '';
    },
    set(value) {
      const entry = __tbRuleState(this);
      if (entry.detached) return;
      host.__tb_sheet_rule_set_name(
        __tbSheetAddress(entry.sheet), entry.sheet.__tbId, __tbRulePath(entry.path), String(value),
      );
    },
    enumerable: true, configurable: true,
  });
  // A keyframe's selectors are settable; an invalid list is a `SyntaxError`
  // and leaves the rule unchanged
  // (<https://drafts.csswg.org/css-animations-1/#dom-csskeyframerule-keytext>).
  __tbDefineProperty(CSSKeyframeRule.prototype, 'keyText', {
    get() {
      const next = __tbRuleState(this).read();
      return next && next.keyText ? next.keyText : '';
    },
    set(value) {
      const entry = __tbRuleState(this);
      if (entry.detached) return;
      host.__tb_sheet_rule_set_key_text(
        __tbSheetAddress(entry.sheet), entry.sheet.__tbId, __tbRulePath(entry.path), String(value),
      );
    },
    enumerable: true, configurable: true,
  });
  // A valid selector list replaces the rule's selectors and re-enters the
  // cascade; an invalid one leaves the rule unchanged without throwing
  // (<https://drafts.csswg.org/cssom/#dom-cssstylerule-selectortext>).
  for (const ctor of [CSSStyleRule, CSSPageRule]) {
    __tbDefineProperty(ctor.prototype, 'selectorText', {
      get() {
        const next = __tbRuleState(this).read();
        return next && next.selectorText ? next.selectorText : '';
      },
      set(value) {
        const entry = __tbRuleState(this);
        if (entry.detached) return;
        host.__tb_sheet_rule_set_selector_text(
          __tbSheetAddress(entry.sheet), entry.sheet.__tbId, __tbRulePath(entry.path), String(value),
        );
      },
      enumerable: true, configurable: true,
    });
  }
  // `style` is `[SameObject, PutForwards=cssText]`: the getter answers one
  // declaration object, and assignment replaces that object's text
  // (<https://webidl.spec.whatwg.org/#dfn-put-forwards>).
  for (const ctor of [
    CSSStyleRule, CSSPageRule, CSSFontFaceRule, CSSKeyframeRule, CSSMarginRule,
    CSSPositionTryRule, CSSNestedDeclarations,
  ]) {
    __tbDefineProperty(ctor.prototype, 'style', {
      get() {
        const entry = __tbRuleState(this);
        if (entry.style === null) entry.style = __tbStyle(entry.sheet, entry);
        return entry.style;
      },
      set(value) { this.style.cssText = value; },
      enumerable: true, configurable: true,
    });
  }
  // `media` is `[SameObject]` on a media rule and `[SameObject,
  // PutForwards=mediaText]` on an import rule: both answer one `MediaList`
  // backed by the rule's query list
  // (<https://drafts.csswg.org/cssom/#dom-cssmediarule-media>).
  const __tbRuleMedia = (ctor, putForwards) => {
    __tbDefineProperty(ctor.prototype, 'media', {
      get() {
        const entry = __tbRuleState(this);
        if (entry.media === null) {
          const slot = {
            text: '',
            get: () => {
              const next = entry.read();
              return next && next.media ? next.media : '';
            },
            set: value => {
              if (entry.detached) return;
              host.__tb_sheet_rule_set_media(
                __tbSheetAddress(entry.sheet), entry.sheet.__tbId, __tbRulePath(entry.path), value,
              );
            },
          };
          const list = Object.create(MediaList.prototype);
          __tbMediaListSlots.set(list, slot);
          entry.media = list;
        }
        return entry.media;
      },
      set: putForwards ? function(value) { this.media.mediaText = value; } : undefined,
      enumerable: true, configurable: true,
    });
  };
  __tbRuleMedia(CSSMediaRule, false);
  __tbRuleMedia(CSSImportRule, true);
  const __tbRuleChildList = ctor => {
    __tbDefineProperty(ctor.prototype, 'cssRules', {
      get() {
        const entry = __tbRuleState(this);
        if (entry.list === null) entry.list = __tbLiveRuleList(entry.sheet, entry, this);
        return entry.list;
      },
      enumerable: true, configurable: true,
    });
    ctor.prototype.insertRule = function(text, index) {
      const entry = __tbRuleState(this);
      return __tbInsert(entry.sheet, entry.path, text, index, arguments.length);
    };
    ctor.prototype.deleteRule = function(index) {
      const entry = __tbRuleState(this);
      __tbDelete(entry.sheet, entry.path, index, arguments.length);
    };
  };
  // A style rule in a nesting context and a grouping rule both answer
  // `cssRules`, `insertRule`, and `deleteRule` by index
  // (<https://drafts.csswg.org/css-nesting-1/#cssom>).
  for (const ctor of [CSSGroupingRule, CSSStyleRule]) __tbRuleChildList(ctor);
  // A keyframes rule's `cssRules` is its keyframe list, reached by index
  // through `length` and the indexed getter, and edited by selector through
  // `appendRule`, `deleteRule`, and `findRule`
  // (<https://drafts.csswg.org/css-animations-1/#interface-csskeyframesrule>).
  __tbDefineProperty(CSSKeyframesRule.prototype, 'cssRules', {
    get() {
      const entry = __tbRuleState(this);
      if (entry.list === null) entry.list = __tbLiveRuleList(entry.sheet, entry, this);
      return entry.list;
    },
    enumerable: true, configurable: true,
  });
  __tbDefineProperty(CSSKeyframesRule.prototype, 'length', {
    get() { return this.cssRules.length; },
    enumerable: true, configurable: true,
  });
  CSSKeyframesRule.prototype.appendRule = function appendRule(rule) {
    if (arguments.length < 1) throw new TypeError('appendRule requires 1 argument');
    const entry = __tbRuleState(this);
    if (entry.detached) return;
    // An invalid rule appends nothing and throws nothing (the IDL lists no
    // exceptions)
    // (<https://drafts.csswg.org/css-animations-1/#dom-csskeyframesrule-appendrule>).
    try {
      __tbInsert(entry.sheet, entry.path, String(rule), this.cssRules.length, 2);
    } catch (error) {
      if (!(error instanceof __tbDomException) || error.name !== 'SyntaxError') throw error;
    }
  };
  CSSKeyframesRule.prototype.findRule = function findRule(select) {
    if (arguments.length < 1) throw new TypeError('findRule requires 1 argument');
    const entry = __tbRuleState(this);
    if (entry.detached) return null;
    const index = host.__tb_sheet_find_keyframe(
      __tbSheetAddress(entry.sheet), entry.sheet.__tbId, __tbRulePath(entry.path), String(select),
    );
    return index < 0 ? null : this.cssRules[index];
  };
  CSSKeyframesRule.prototype.deleteRule = function deleteRule(select) {
    if (arguments.length < 1) throw new TypeError('deleteRule requires 1 argument');
    const entry = __tbRuleState(this);
    if (entry.detached) return;
    const index = host.__tb_sheet_find_keyframe(
      __tbSheetAddress(entry.sheet), entry.sheet.__tbId, __tbRulePath(entry.path), String(select),
    );
    if (index >= 0) __tbDelete(entry.sheet, entry.path, index, 1);
  };
  const __tbRulePrototype = {
    CSSStyleRule, CSSGroupingRule, CSSConditionRule, CSSMediaRule, CSSSupportsRule,
    CSSImportRule, CSSFontFaceRule, CSSKeyframesRule, CSSKeyframeRule, CSSRule,
    CSSNamespaceRule, CSSPageRule, CSSCounterStyleRule, CSSLayerBlockRule,
    CSSLayerStatementRule, CSSContainerRule, CSSScopeRule, CSSCustomMediaRule,
    CSSFontFeatureValuesRule, CSSFontPaletteValuesRule, CSSMarginRule,
    CSSPropertyRule, CSSStartingStyleRule, CSSPositionTryRule,
    CSSViewTransitionRule, CSSNestedDeclarations, CSSMozDocumentRule,
    CSSAppearanceBaseRule,
  };
  for (const name of Object.keys(__tbRulePrototype)) {
    __tbDefineProperty(globalThis, name, {
      value: __tbRulePrototype[name], writable: true, configurable: true,
    });
    __tbDefineProperty(globalThis, 'CSSStyleDeclaration', {
      value: CSSStyleDeclaration, writable: true, configurable: true,
    });
  }
  // Every declaration object is iterable over its property names: WebIDL's
  // `iterable<CSSOMString>` gives `keys`, `values`, `entries`, `forEach`, and
  // `Symbol.iterator` (the same function as `values`)
  // (<https://webidl.spec.whatwg.org/#idl-iterable>).
  const __tbDeclarationIterator = (style, kind) => {
    let index = 0;
    const iterator = {
      next() {
        if (index >= style.length) return { value: undefined, done: true };
        const current = index++;
        if (kind === 'keys') return { value: current, done: false };
        const value = style.item(current);
        return kind === 'entries'
          ? { value: [current, value], done: false }
          : { value, done: false };
      },
    };
    iterator[Symbol.iterator] = function () { return iterator; };
    return iterator;
  };
  for (const [name, kind] of [['keys', 'keys'], ['values', 'values'], ['entries', 'entries']]) {
    __tbDefineProperty(CSSStyleDeclaration.prototype, name, {
      value: function () { return __tbDeclarationIterator(this, kind); },
      writable: true, enumerable: true, configurable: true,
    });
  }
  __tbDefineProperty(CSSStyleDeclaration.prototype, 'forEach', {
    value: function forEach(callback) {
      const thisArg = arguments.length > 1 ? arguments[1] : undefined;
      for (let index = 0; index < this.length; index++) {
        __tbApply(callback, thisArg, [this.item(index), index, this]);
      }
    },
    writable: true, enumerable: true, configurable: true,
  });
  __tbDefineProperty(CSSStyleDeclaration.prototype, Symbol.iterator, {
    value: CSSStyleDeclaration.prototype.values,
    writable: true, enumerable: false, configurable: true,
  });
  function __tbSamePath(left, right) {
    if (left.length !== right.length) return false;
    for (let index = 0; index < left.length; index++) {
      if (left[index] !== right[index]) return false;
    }
    return true;
  }
  function __tbUnder(entry, listPath) {
    if (entry.path.length <= listPath.length) return false;
    for (let index = 0; index < listPath.length; index++) {
      if (entry.path[index] !== listPath[index]) return false;
    }
    return true;
  }
  function __tbRequireClean(sheet) {
    const state = host.__tb_sheet_origin_state(__tbSheetAddress(sheet), sheet.__tbId);
    if (state === 0) throw new __tbDomException('cross-origin stylesheet', 'SecurityError');
    if (state < 0) throw new __tbDomException('stylesheet is gone', 'InvalidStateError');
  }
  function __tbCanonicalIndex(property) {
    if (typeof property !== 'string' || property.length === 0) return false;
    if (property.length > 1 && __tbApply(__tbCharCodeAt, property, [0]) === 0x30) return false;
    let value = 0;
    for (let index = 0; index < property.length; index++) {
      const code = __tbApply(__tbCharCodeAt, property, [index]);
      if (code < 0x30 || code > 0x39) return false;
      value = value * 10 + (code - 0x30);
      if (value >= 4294967295) return false;
    }
    return true;
  }
  function __tbStyle(sheet, entry) {
    const style = Object.create(CSSStyleDeclaration.prototype);
    const pathKey = () => __tbRulePath(entry.path);
    __tbDefineProperty(style, 'cssText', {
      get() {
        if (entry.detached) return '';
        return host.__tb_sheet_style_css_text(__tbSheetAddress(sheet), sheet.__tbId, pathKey());
      },
      // The setter parses the value as a declaration block and replaces the
      // block whole, keeping the properties that parse
      // (<https://drafts.csswg.org/cssom/#dom-cssstyledeclaration-csstext>).
      set(value) {
        if (entry.detached) return;
        host.__tb_sheet_style_set_css_text(
          __tbSheetAddress(sheet), sheet.__tbId, pathKey(), value == null ? '' : String(value),
        );
      },
      enumerable: true, configurable: true,
    });
    __tbDefineProperty(style, 'length', {
      get() {
        if (entry.detached) return 0;
        const names = host.__tb_sheet_style_names(__tbSheetAddress(sheet), sheet.__tbId, pathKey());
        return names ? names.length : 0;
      },
      enumerable: true, configurable: true,
    });
    style.getPropertyValue = function(name) {
      if (entry.detached) return '';
      const found = host.__tb_sheet_style_get_property(__tbSheetAddress(sheet), sheet.__tbId, pathKey(), String(name));
      return found ? found.value : '';
    };
    style.getPropertyPriority = function(name) {
      if (entry.detached) return '';
      const found = host.__tb_sheet_style_get_property(__tbSheetAddress(sheet), sheet.__tbId, pathKey(), String(name));
      return found && found.important ? 'important' : '';
    };
    style.setProperty = function(name, value, priority) {
      if (entry.detached) return;
      host.__tb_sheet_style_set_property(
        __tbSheetAddress(sheet), sheet.__tbId, pathKey(), String(name),
        value == null ? '' : String(value), String(priority) === 'important',
      );
    };
    // Removing a property through the same host setter: an empty value is the
    // engine's remove path, and the call answers the previous serialized value
    // (<https://drafts.csswg.org/cssom/#dom-cssstyledeclaration-removeproperty>).
    style.removeProperty = function(name) {
      if (entry.detached) return '';
      const property = String(name);
      const previous = style.getPropertyValue(property);
      host.__tb_sheet_style_set_property(
        __tbSheetAddress(sheet), sheet.__tbId, pathKey(), property, '', false,
      );
      return previous;
    };
    style.item = function(index) {
      if (arguments.length < 1) throw new TypeError('item requires 1 argument');
      if (entry.detached) return '';
      const names = host.__tb_sheet_style_names(__tbSheetAddress(sheet), sheet.__tbId, pathKey()) || [];
      index = index >>> 0;
      return index < names.length ? names[index] : '';
    };
    // `stringifier attribute CSSOMString cssText` gives every declaration
    // object a `toString` that answers its serialization
    // (<https://drafts.csswg.org/cssom/#the-cssstyledeclaration-interface>).
    style.toString = function() { return this.cssText; };
    // The CSS property IDL attributes (`margin`, `borderTopWidth`, …) sit on
    // top of the operations, exactly as on an inline style
    // (<https://drafts.csswg.org/cssom/#css-property-to-idl-attribute>).
    const idlName = property => property === 'cssFloat'
      ? 'float'
      : property.replace(/[A-Z]/g, letter => '-' + letter.toLowerCase());
    return new Proxy(style, {
      get(target, property, receiver) {
        if (typeof property !== 'string') return Reflect.get(target, property, receiver);
        if (property in target) {
          const value = Reflect.get(target, property, receiver);
          return typeof value === 'function' ? value.bind(target) : value;
        }
        return target.getPropertyValue(idlName(property));
      },
      set(target, property, value) {
        if (typeof property !== 'string' || property in target) {
          return Reflect.set(target, property, value);
        }
        target.setProperty(idlName(property), value);
        return true;
      },
    });
  }
  // Per-instance state for a rule, kept under a symbol: WebIDL puts the
  // interface's accessors on its prototype, so the rule object itself owns no
  // IDL attribute
  // (<https://webidl.spec.whatwg.org/#interface-prototype-object>).
  const __tbRuleEntry = Symbol('CSSRule entry');
  __tbCssomRuleAddress = rule => {
    const entry = rule[__tbRuleEntry];
    if (entry === undefined || entry.detached) return null;
    return { sheetId: entry.sheet.__tbId, path: entry.path.slice() };
  };
  __tbCssomRuleAt = (sheetId, path) => {
    const sheet = __tbSheetsById.get(sheetId);
    if (sheet === undefined) return null;
    for (const entry of sheet.__tbRules) {
      if (!entry.detached && __tbSamePath(entry.path, path)) return entry.rule;
    }
    return null;
  };
  function __tbRuleState(receiver) {
    const entry = receiver[__tbRuleEntry];
    if (entry === undefined) throw new TypeError('Illegal invocation');
    return entry;
  }
  function __tbCachedRule(sheet, path, parentRule) {
    for (const entry of sheet.__tbRules) {
      if (!entry.detached && __tbSamePath(entry.path, path)) return entry.rule;
    }
    const entry = {
      path: path.slice(), detached: false, frozenText: '', rule: null,
      list: null, style: null, media: null, sheet, parentRule, info: null, read: null,
    };
    const rule = Object.create(CSSRule.prototype);
    entry.read = () => {
      if (entry.detached) return null;
      return host.__tb_sheet_rule_info(__tbSheetAddress(sheet), sheet.__tbId, __tbRulePath(entry.path));
    };
    const info = entry.read() || { interface: 'CSSRule', type: 0, cssText: '', hasChildRules: false, hasStyle: false };
    entry.info = info;
    const ctor = __tbRulePrototype[info.interface] || CSSRule;
    __tbSetPrototypeOf(rule, ctor.prototype);
    __tbDefineProperty(rule, __tbRuleEntry, { value: entry });
    // A keyframes rule also answers its keyframes through the indexed getter
    // (<https://drafts.csswg.org/css-animations-1/#dom-csskeyframesrule-index>).
    entry.rule = ctor === CSSKeyframesRule ? new Proxy(rule, __tbKeyframeIndexHandler) : rule;
    sheet.__tbRules.push(entry);
    return entry.rule;
  }
  // The indexed getter of a keyframes rule reads its `cssRules` list; every
  // other member is the rule object's own
  // (<https://webidl.spec.whatwg.org/#idl-indexed-properties>).
  const __tbKeyframeIndexHandler = {
    get(target, property, receiver) {
      if (__tbCanonicalIndex(property)) {
        const entry = __tbRuleState(target);
        const list = entry.list === null
          ? (entry.list = __tbLiveRuleList(entry.sheet, entry, receiver))
          : entry.list;
        return list[property >>> 0];
      }
      return __tbApply(__tbReflectGet, Reflect, [target, property, receiver]);
    },
    has(target, property) {
      if (__tbCanonicalIndex(property)) return (property >>> 0) < target.cssRules.length;
      return __tbApply(__tbReflectHas, Reflect, [target, property]);
    },
  };
  // Live `CSSRuleList`. `item` returns the same CSSRule for as long as that
  // rule exists (<https://drafts.csswg.org/cssom/#the-cssrulelist-interface>).
  function __tbLiveRuleList(sheet, parentEntry, parentRule) {
    const listPath = () => parentEntry ? parentEntry.path : [];
    const list = {
      get length() {
        if (parentEntry && parentEntry.detached) return 0;
        __tbRequireClean(sheet);
        const count = host.__tb_sheet_rule_count(__tbSheetAddress(sheet), sheet.__tbId, __tbRulePath(listPath()));
        return count < 0 ? 0 : count;
      },
      item(index) {
        if (arguments.length < 1) throw new TypeError('item requires 1 argument');
        if (parentEntry && parentEntry.detached) return null;
        __tbRequireClean(sheet);
        index = index >>> 0;
        if (index >= this.length) return null;
        const path = __tbArray.concat(listPath(), index);
        return __tbCachedRule(sheet, path, parentRule);
      },
    };
    list[Symbol.iterator] = function* () {
      for (let index = 0; index < this.length; index++) yield this.item(index);
    };
    Object.setPrototypeOf(list, CSSRuleList.prototype);
    return new Proxy(list, {
      get(target, property, receiver) {
        if (__tbCanonicalIndex(property)) {
          const item = target.item(property >>> 0);
          return item === null ? undefined : item;
        }
        const value = __tbApply(__tbReflectGet, Reflect, [target, property, receiver]);
        return typeof value === 'function' ? __tbApply(__tbBind, value, [target]) : value;
      },
    });
  }
  function __tbInsert(sheet, listPath, text, index, arity) {
    if (arity < 1) throw new TypeError('insertRule requires 1 argument');
    __tbRequireClean(sheet);
    index = arity < 2 ? 0 : (index >>> 0);
    const inserted = host.__tb_sheet_insert_rule(__tbSheetAddress(sheet), sheet.__tbId, __tbRulePath(listPath), String(text), index);
    for (const entry of sheet.__tbRules) {
      if (entry.detached || !__tbUnder(entry, listPath)) continue;
      if (entry.path[listPath.length] >= index) entry.path[listPath.length] += 1;
    }
    return inserted;
  }
  function __tbDelete(sheet, listPath, index, arity) {
    if (arity < 1) throw new TypeError('deleteRule requires 1 argument');
    __tbRequireClean(sheet);
    index = index >>> 0;
    for (const entry of sheet.__tbRules) {
      if (entry.detached || !__tbUnder(entry, listPath)) continue;
      if (entry.path[listPath.length] !== index) continue;
      const info = host.__tb_sheet_rule_info(__tbSheetAddress(sheet), sheet.__tbId, __tbRulePath(entry.path));
      entry.frozenText = info ? info.cssText : '';
      entry.detached = true;
    }
    host.__tb_sheet_delete_rule(__tbSheetAddress(sheet), sheet.__tbId, __tbRulePath(listPath), index);
    for (const entry of sheet.__tbRules) {
      if (entry.detached || !__tbUnder(entry, listPath)) continue;
      if (entry.path[listPath.length] > index) entry.path[listPath.length] -= 1;
    }
  }
  // `MediaList` behind `StyleSheet.media`. An associated sheet's list reads
  // and writes its owner element's `media` content attribute; a constructable
  // sheet keeps its own text
  // (<https://drafts.csswg.org/cssom/#the-medialist-interface>).
  const __tbMediaListSlots = host.slots('MediaList');
  function MediaList() { __tbIllegal(); }
  Object.defineProperty(globalThis, 'MediaList', {
    value: MediaList, writable: true, configurable: true,
  });
  __tbDefineProperty(MediaList.prototype, 'mediaText', {
    get() {
      const slot = __tbMediaListSlots.get(this);
      if (!slot) throw new TypeError('Illegal invocation');
      return slot.get();
    },
    set(value) {
      const slot = __tbMediaListSlots.get(this);
      if (!slot) throw new TypeError('Illegal invocation');
      slot.set(String(value));
    },
    enumerable: true, configurable: true,
  });
  __tbDefineProperty(MediaList.prototype, 'length', {
    get() {
      const slot = __tbMediaListSlots.get(this);
      if (!slot) throw new TypeError('Illegal invocation');
      const text = slot.get();
      let count = 0;
      for (const part of __tbSplitMedia(text, ',')) {
        if (part.trim() !== '') count += 1;
      }
      return count;
    },
    enumerable: true, configurable: true,
  });
  __tbDefineProperty(MediaList.prototype, 'item', {
    value: function item(index) {
      if (arguments.length < 1) throw new TypeError('item requires 1 argument');
      const slot = __tbMediaListSlots.get(this);
      if (!slot) throw new TypeError('Illegal invocation');
      const trimmed = [];
      for (const part of __tbSplitMedia(slot.get(), ',')) {
        if (part.trim() !== '') trimmed.push(part.trim());
      }
      return trimmed[index >>> 0] === undefined ? null : trimmed[index >>> 0];
    },
    writable: true, enumerable: true, configurable: true,
  });
  // A medium is compared in its normalized form: the engine lowercases media
  // types and collapses whitespace, and CSSOM compares the parsed queries, so
  // matching ignores case and runs of whitespace
  // (<https://drafts.csswg.org/cssom/#dom-medialist-appendmedium>).
  const __tbNormalizedMedium = value =>
    String(value).trim().toLowerCase().replace(/\s+/g, ' ');
  const __tbMediumEntries = text => {
    const entries = [];
    for (const part of __tbSplitMedia(text, ',')) {
      if (part.trim() !== '') entries.push(part.trim());
    }
    return entries;
  };
  __tbDefineProperty(MediaList.prototype, 'appendMedium', {
    value: function appendMedium(medium) {
      if (arguments.length < 1) throw new TypeError('appendMedium requires 1 argument');
      const slot = __tbMediaListSlots.get(this);
      if (!slot) throw new TypeError('Illegal invocation');
      const text = String(medium);
      if (text.trim() === '') return;
      const normalized = __tbNormalizedMedium(text);
      const entries = __tbMediumEntries(slot.get());
      if (entries.some(entry => __tbNormalizedMedium(entry) === normalized)) return;
      const current = entries.join(', ');
      slot.set(current === '' ? text : current + ', ' + text);
    },
    writable: true, enumerable: true, configurable: true,
  });
  __tbDefineProperty(MediaList.prototype, 'deleteMedium', {
    value: function deleteMedium(medium) {
      if (arguments.length < 1) throw new TypeError('deleteMedium requires 1 argument');
      const slot = __tbMediaListSlots.get(this);
      if (!slot) throw new TypeError('Illegal invocation');
      const normalized = __tbNormalizedMedium(medium);
      const entries = __tbMediumEntries(slot.get());
      const index = entries.findIndex(entry => __tbNormalizedMedium(entry) === normalized);
      if (index < 0) {
        throw new __tbDomException('The medium is not in the list', 'NotFoundError');
      }
      entries.splice(index, 1);
      slot.set(entries.join(', '));
    },
    writable: true, enumerable: true, configurable: true,
  });
  __tbDefineProperty(MediaList.prototype, Symbol.toStringTag, {
    value: 'MediaList', writable: false, enumerable: false, configurable: true,
  });
  function __tbSheetMediaList(sheet) {
    const slot = { text: '', get: null, set: null };
    const owner = sheet.__tbOwner;
    if (sheet.__tbConstructable === true) {
      // A constructable sheet owns its media query list, which the engine
      // parsed from the constructor's `media` member; the cascade consults it
      // when the sheet is adopted
      // (<https://drafts.csswg.org/cssom/#dom-stylesheet-media>).
      const address = __tbSheetAddress(sheet);
      slot.get = () => host.__tb_sheet_media_text(address, sheet.__tbId);
      slot.set = value => { host.__tb_set_sheet_media_text(address, sheet.__tbId, value); };
    } else if (owner === undefined) {
      slot.get = () => slot.text;
      slot.set = value => { slot.text = value; };
    } else {
      // The owner's `media` attribute is the live source, so a script that
      // changes it is visible through the list
      // (<https://drafts.csswg.org/cssom/#dom-medialist-mediatext>).
      slot.get = () => owner.getAttribute('media') ?? '';
      slot.set = value => {
        if (value === '') owner.removeAttribute('media');
        else owner.setAttribute('media', value);
      };
    }
    const list = Object.create(MediaList.prototype);
    __tbMediaListSlots.set(list, slot);
    return list;
  }
  class CSSStyleSheet {
    constructor(options) {
      // `CSSStyleSheetInit`'s members: `baseURL` resolves relative references
      // and an invalid one is a `NotAllowedError`, `media` seeds the media
      // query list, and `disabled` seeds the disabled flag. `title` and
      // `alternate` are ignored
      // (<https://drafts.csswg.org/cssom/#dom-cssstylesheet-cssstylesheet>).
      const init = options === undefined || options === null ? {} : Object(options);
      let baseURL = null;
      if (init.baseURL !== undefined) {
        try {
          baseURL = new URL(String(init.baseURL), document.baseURI).href;
        } catch (error) {
          throw new __tbDomException('baseURL is not a valid URL', 'NotAllowedError');
        }
      }
      const media = init.media === undefined ? '' : String(init.media);
      this.__tbRules = [];
      const id = host.__tb_create_sheet(
        document, '', media, baseURL, Boolean(init.disabled),
      );
      if (!(id >= 1)) {
        throw new __tbDomException('cannot create stylesheet', 'InvalidStateError');
      }
      this.__tbId = id;
      this.__tbBaseURL = baseURL;
      // A constructable sheet has no owner node; host calls address the
      // document that created it
      // (<https://drafts.csswg.org/cssom/#dom-cssstylesheet-cssstylesheet>).
      this.__tbAddress = document;
      this.__tbConstructable = true;
    }
    // `rules` is the legacy alias of `cssRules`, and both read the same live
    // list
    // (<https://drafts.csswg.org/cssom/#dom-cssstylesheet-rules>).
    get rules() {
      return this.cssRules;
    }
    get cssRules() {
      // One list object for the sheet's lifetime: a `replace` swaps the rules
      // behind it rather than replacing the list
      // (<https://drafts.csswg.org/cssom/#dom-cssstylesheet-cssrules>).
      if (this.__tbList === undefined) this.__tbList = __tbLiveRuleList(this, null, null);
      return this.__tbList;
    }
    // The StyleSheet interface properties. An associated sheet reads its
    // owner `<style>`/`<link>`; a constructable sheet has no owner and no URL
    // (<https://drafts.csswg.org/cssom/#the-stylesheet-interface>).
    get type() {
      return 'text/css';
    }
    get href() {
      // An imported sheet's URL is the one its `@import` rule names
      // (<https://drafts.csswg.org/cssom/#dom-stylesheet-href>).
      if (this.__tbHref !== undefined) return this.__tbHref;
      const owner = this.__tbOwner;
      if (owner === undefined) return null;
      // Only a `<link rel=stylesheet>` sheet has a URL; an inline `<style>`'s
      // sheet has none
      // (<https://drafts.csswg.org/cssom/#dom-stylesheet-href>).
      if (owner.localName !== 'link') return null;
      return owner.href;
    }
    get ownerNode() {
      const owner = this.__tbOwner;
      if (owner === undefined) return null;
      // Removing the element removes its sheet; the sheet object stays
      // readable but loses its owner
      // (<https://drafts.csswg.org/cssom/#remove-a-css-style-sheet>).
      return host.__tb_node_sheet_id(owner) === this.__tbId ? owner : null;
    }
    get parentStyleSheet() {
      // An imported sheet's parent is the sheet whose `@import` rule loaded
      // it; deleting that rule unlinks the child
      // (<https://drafts.csswg.org/cssom/#dom-stylesheet-parentstylesheet>).
      if (this.__tbOwnerRuleEntry !== undefined && this.__tbOwnerRuleEntry.detached) return null;
      return this.__tbParent ?? null;
    }
    get ownerRule() {
      // An imported sheet is owned by its `CSSImportRule`, not an element, and
      // deleting the rule releases it
      // (<https://drafts.csswg.org/cssom/#dom-stylesheet-ownerrule>).
      if (this.__tbOwnerRuleEntry !== undefined && this.__tbOwnerRuleEntry.detached) return null;
      return this.__tbOwnerRule ?? null;
    }
    get title() {
      // An imported or constructable sheet has no title of its own
      // (<https://drafts.csswg.org/cssom/#dom-stylesheet-title>).
      const owner = this.__tbOwner;
      if (owner === undefined) return null;
      return owner.getAttribute('title');
    }
    get disabled() {
      // `disabled` applies to an owner sheet; an imported sheet is never
      // disabled and assigning to the attribute does nothing
      // (<https://drafts.csswg.org/cssom/#dom-stylesheet-disabled>).
      if (this.__tbOwnerRule !== undefined) return false;
      if (this.__tbConstructable === true) {
        return host.__tb_cssom_sheet_disabled(__tbSheetAddress(this), this.__tbId);
      }
      if (this.__tbOwner === undefined) return this.__tbDisabled === true;
      return host.__tb_sheet_disabled(this.__tbOwner, this.__tbId);
    }
    set disabled(value) {
      if (this.__tbOwnerRule !== undefined) return;
      const disabled = Boolean(value);
      if (this.__tbConstructable === true) {
        host.__tb_set_cssom_sheet_disabled(__tbSheetAddress(this), this.__tbId, disabled);
        return;
      }
      if (this.__tbOwner === undefined) {
        this.__tbDisabled = disabled;
        return;
      }
      host.__tb_set_sheet_disabled(this.__tbOwner, this.__tbId, disabled);
    }
    get media() {
      if (this.__tbMedia === undefined) this.__tbMedia = __tbSheetMediaList(this);
      return this.__tbMedia;
    }
    // `[PutForwards=mediaText]`: assigning the attribute replaces the list's
    // text (<https://drafts.csswg.org/cssom/#dom-stylesheet-media>).
    set media(value) { this.media.mediaText = value; }
    replaceSync(text) {
      // Only a sheet the constructor built has a rule list a script may
      // replace; an associated sheet keeps its parsed sheet
      // (<https://drafts.csswg.org/cssom/#dom-cssstylesheet-replacesync>).
      if (this.__tbConstructable !== true) {
        throw new __tbDomException('associated sheet is not constructable', 'NotAllowedError');
      }
      const replaced = host.__tb_replace_sheet(
        __tbSheetAddress(this), this.__tbId, String(text), this.__tbBaseURL ?? null,
      );
      if (!replaced) {
        throw new __tbDomException('cannot replace stylesheet', 'InvalidStateError');
      }
      // `replace` detaches every rule object already handed out, so a stale
      // rule's `parentStyleSheet` and `parentRule` become null
      // (<https://drafts.csswg.org/cssom/#dom-cssstylesheet-replacesync>).
      for (const entry of this.__tbRules) {
        if (entry.detached) continue;
        const info = host.__tb_sheet_rule_info(
          __tbSheetAddress(this), this.__tbId, __tbRulePath(entry.path),
        );
        entry.frozenText = info ? info.cssText : '';
        entry.detached = true;
      }
    }
    replace(text) {
      try {
        this.replaceSync(text);
        return Promise.resolve(this);
      } catch (error) {
        return Promise.reject(error);
      }
    }
    insertRule(rule, index) {
      return __tbInsert(this, [], rule, index, arguments.length);
    }
    deleteRule(index) {
      __tbDelete(this, [], index, arguments.length);
    }
    // Legacy members: `addRule` builds `<selector> { <block> }` and inserts
    // it at `index` (or appends), returning -1; `removeRule` is `deleteRule`
    // (<https://drafts.csswg.org/cssom/#legacy-css-style-sheet-members>).
    addRule(selector, block, index) {
      let rule = selector === undefined ? 'undefined' : String(selector);
      rule += ' { ';
      block = block === undefined ? 'undefined' : String(block);
      if (block !== '') rule += block + ' ';
      rule += '}';
      this.insertRule(rule, index === undefined ? this.cssRules.length : index);
      return -1;
    }
    removeRule(index) {
      this.deleteRule(index === undefined ? 0 : index);
    }
  }
  Object.defineProperty(globalThis, 'CSSStyleSheet', {
    value: CSSStyleSheet, writable: true, configurable: true,
  });
  // Associated CSS style sheet for `<style>` and `<link rel=stylesheet>`
  // (<https://drafts.csswg.org/cssom/#associated-css-style-sheet>,
  // <https://html.spec.whatwg.org/multipage/semantics.html#the-style-element:create-a-css-style-sheet>,
  // <https://html.spec.whatwg.org/multipage/semantics.html#the-link-element:create-a-css-style-sheet>).
  // Created while the element is connected so an earlier script in the same
  // insert can observe `sheet` after the style/link node's insertion steps
  // and before that script's post-connection steps. `cssRules` reads the
  // style engine's parsed list for that owner node.
  // The brand check uses the node kind. Pages may replace the global
  // afterwards.
  const __tbCssomBrand = value => {
    // Realm-agnostic: cross-realm elements fail `instanceof` against this
    // realm's `Element`, so check the node kind instead. A forged
    // `{nodeType: 1}` passes here but the Rust `require_interface("Element")`
    // rejects it authoritatively (page prototypes never grant brand).
    if ((value === null || (typeof value !== 'object' && typeof value !== 'function'))
      || value.nodeType !== 1) throw new TypeError('Illegal invocation');
    return value;
  };
  function __tbAssociatedSheet(element) {
    __tbCssomBrand(element);
    const id = host.__tb_node_sheet_id(element);
    if (!(id >= 1)) return null;
    let sheet = __tbSheetsById.get(id);
    if (sheet !== undefined) return sheet;
    sheet = Object.create(CSSStyleSheet.prototype);
    sheet.__tbRules = [];
    sheet.__tbId = id;
    sheet.__tbOwner = element;
    __tbSheetsById.set(id, sheet);
    return sheet;
  }
  Object.defineProperty(globalThis.HTMLStyleElement.prototype, 'sheet', {
    get() {
      __tbCssomBrand(this);
      if (!this.isConnected) return null;
      return __tbAssociatedSheet(this);
    },
    enumerable: true,
    configurable: true,
  });
  Object.defineProperty(globalThis.HTMLLinkElement.prototype, 'sheet', {
    get() {
      __tbCssomBrand(this);
      if (!this.isConnected) return null;
      const rel = __tbApply(__tbStringToLowerCase, __tbIDLString(this.rel ?? this.getAttribute('rel') ?? ''), []);
      const tokens = __tbApply(__tbStringSplit, rel, [/\s+/]);
      if (__tbArray.indexOf(tokens, 'stylesheet') === -1) return null;
      return __tbAssociatedSheet(this);
    },
    enumerable: true,
    configurable: true,
  });
  // `document.styleSheets`: every author sheet whose owner node is in the
  // document, in tree order
  // (<https://drafts.csswg.org/cssom/#dom-document-stylesheets>). The list
  // reads the owner set on every access, so a sheet inserted or removed after
  // the list was first observed is reflected without rebuilding it.
  function StyleSheetList() { __tbIllegal(); }
  Object.defineProperty(globalThis, 'StyleSheetList', {
    value: StyleSheetList, writable: true, configurable: true,
  });
  __tbDefineProperty(StyleSheetList.prototype, 'item', {
    value: function item(index) {
      if (arguments.length < 1) throw new TypeError('item requires 1 argument');
      index = index >>> 0;
      const all = host.__tb_sheet_owner_nodes(this.__tbDocument);
      if (index >= all.length) return null;
      return __tbAssociatedSheet(all[index]);
    },
    writable: true, configurable: true,
  });
  __tbDefineProperty(StyleSheetList.prototype, 'length', {
    get() { return host.__tb_sheet_owner_nodes(this.__tbDocument).length; },
    enumerable: true, configurable: true,
  });
  __tbDefineProperty(StyleSheetList.prototype, Symbol.iterator, {
    value: function* () {
      for (let index = 0; index < this.length; index++) yield this.item(index);
    },
    writable: true, configurable: true,
  });
  __tbDefineProperty(StyleSheetList.prototype, Symbol.toStringTag, {
    value: 'StyleSheetList', writable: false, enumerable: false, configurable: true,
  });
  const __tbStyleSheetLists = new WeakMap();
  function __tbCssomDocumentBrand(value) {
    // A `Document` is the only node with nodeType 9; a forged object still
    // fails the Rust `host_node_id` check authoritatively.
    if (value === null || typeof value !== 'object' || value.nodeType !== 9) {
      throw new TypeError('Illegal invocation');
    }
    return value;
  }
  function __tbStyleSheetList(document) {
    __tbCssomDocumentBrand(document);
    let list = __tbStyleSheetLists.get(document);
    if (list !== undefined) return list;
    const target = Object.create(StyleSheetList.prototype, {
      __tbDocument: { value: document, writable: false, enumerable: false, configurable: false },
    });
    list = new Proxy(target, {
      get(list, property, receiver) {
        if (__tbCanonicalIndex(property)) {
          const item = list.item(property >>> 0);
          return item === null ? undefined : item;
        }
        const value = __tbApply(__tbReflectGet, Reflect, [list, property, receiver]);
        return typeof value === 'function' ? __tbApply(__tbBind, value, [list]) : value;
      },
    });
    __tbStyleSheetLists.set(document, list);
    return list;
  }
  Object.defineProperty(globalThis.Document.prototype, 'styleSheets', {
    get() { return __tbStyleSheetList(this); },
    enumerable: true,
    configurable: true,
  });
  // `adoptedStyleSheets` is an observable array of constructable sheets that
  // apply after every document sheet, in list order. Every in-place mutation
  // (`push`, `splice`, `reverse`, an index or `length` write) validates the
  // list and hands it to the engine, so cascade order and application follow
  // the array (<https://drafts.csswg.org/cssom/#dom-document-adoptedstylesheets>,
  // <https://github.com/WICG/observable-array>).
  const __tbAdoptedMutators = new Set([
    'push', 'pop', 'shift', 'unshift', 'splice', 'reverse', 'sort', 'fill', 'copyWithin',
  ]);
  const __tbAdoptedSheet = value => {
    if (value === null || (typeof value !== 'object' && typeof value !== 'function')) {
      throw new TypeError('adopted style sheet must be a CSSStyleSheet');
    }
    if (value.__tbConstructable !== true) {
      throw new __tbDomException('only constructable sheets may be adopted', 'NotAllowedError');
    }
    return value;
  };
  const __tbObservableSheetArray = apply => {
    const target = [];
    const restore = snapshot => {
      target.length = 0;
      for (const sheet of snapshot) __tbArray.push(target, sheet);
    };
    // Validate the whole list before the engine sees it, so a rejected
    // mutation leaves the observable array unchanged
    const commit = () => {
      const sheets = [];
      for (const sheet of target) sheets.push(__tbAdoptedSheet(sheet));
      apply(sheets);
    };
    // A mutator like `push` is several index writes plus a `length` write; one
    // engine update at the end covers the whole call
    let running = false;
    return new Proxy(target, {
      get(list, property) {
        const value = __tbApply(__tbReflectGet, Reflect, [list, property, list]);
        if (typeof value !== 'function' || !__tbAdoptedMutators.has(property)) return value;
        return function(...args) {
          const snapshot = list.slice();
          running = true;
          try {
            return __tbApply(value, list, args);
          } catch (error) {
            restore(snapshot);
            throw error;
          } finally {
            running = false;
            try {
              commit();
            } catch (error) {
              restore(snapshot);
              throw error;
            }
          }
        };
      },
      set(list, property, value) {
        const indexed = property === 'length' || __tbCanonicalIndex(property);
        const snapshot = indexed ? list.slice() : null;
        const ok = __tbApply(__tbReflectSet, Reflect, [list, property, value, list]);
        if (!indexed || running) return ok;
        try {
          commit();
        } catch (error) {
          restore(snapshot);
          return false;
        }
        return ok;
      },
      deleteProperty(list, property) {
        const indexed = __tbCanonicalIndex(property);
        const snapshot = indexed ? list.slice() : null;
        const ok = __tbApply(__tbReflectDelete, Reflect, [list, property]);
        if (!indexed || running) return ok;
        try {
          commit();
        } catch (error) {
          restore(snapshot);
          return false;
        }
        return ok;
      },
    });
  };
  const __tbAdoptedSheetArrays = new WeakMap();
  const __tbAdoptedStyleSheets = document => {
    __tbCssomDocumentBrand(document);
    let list = __tbAdoptedSheetArrays.get(document);
    if (list !== undefined) return list;
    list = __tbObservableSheetArray(sheets => host.__tb_set_adopted_sheets(
      document, sheets.map(sheet => sheet.__tbId),
    ));
    __tbAdoptedSheetArrays.set(document, list);
    return list;
  };
  Object.defineProperty(globalThis.Document.prototype, 'adoptedStyleSheets', {
    get() { return __tbAdoptedStyleSheets(this); },
    set(value) {
      // The setter replaces the observable array's contents in place, so the
      // array object a page already holds keeps tracking the document
      // (<https://drafts.csswg.org/cssom/#dom-document-adoptedstylesheets>).
      const list = __tbAdoptedStyleSheets(this);
      const sheets = [];
      if (value !== null && value !== undefined) {
        for (const sheet of __tbArrayFrom(value)) sheets.push(__tbAdoptedSheet(sheet));
      }
      list.length = 0;
      if (sheets.length !== 0) list.push(...sheets);
    },
    enumerable: true,
    configurable: true,
  });
  // Namespace members are writable and configurable, and the namespace
  // object is ordinary rather than frozen
  // (<https://webidl.spec.whatwg.org/#namespace-object>).
  const css = {
    // https://drafts.csswg.org/cssom/#dom-css-escape
    escape(value) {
      if (arguments.length < 1) throw new TypeError('CSS.escape requires 1 argument');
      const string = String(value);
      let result = '';
      for (let index = 0; index < string.length; index++) {
        const code = string.charCodeAt(index);
        if (code === 0) {
          result += '\uFFFD';
          continue;
        }
        if (
          (code >= 1 && code <= 0x1F) || code === 0x7F ||
          (index === 0 && code >= 0x30 && code <= 0x39) ||
          (index === 1 && code >= 0x30 && code <= 0x39 && string.charCodeAt(0) === 0x2D)
        ) {
          result += `\\${code.toString(16)} `;
          continue;
        }
        if (
          code >= 0x80 || code === 0x2D || code === 0x5F ||
          (code >= 0x30 && code <= 0x39) ||
          (code >= 0x41 && code <= 0x5A) ||
          (code >= 0x61 && code <= 0x7A)
        ) {
          result += string[index];
          continue;
        }
        result += `\\${string[index]}`;
      }
      return result;
    },
    // `supports(property, value)` tests one declaration; the single
    // argument form parses and evaluates a supports condition, including a
    // bare declaration
    // (<https://drafts.csswg.org/css-conditional-3/#the-css-namespace>).
    // Two overloads share one function, so its `length` is the shortest
    // required argument list: 1. Both arguments are `CSSOMString`, whose
    // conversion throws on a symbol
    // (<https://webidl.spec.whatwg.org/#overload-set>,
    // <https://webidl.spec.whatwg.org/#es-DOMString>).
    supports(property, ...rest) {
      if (arguments.length < 1) {
        throw new TypeError("Failed to execute 'supports' on 'CSS': 1 argument required, but only 0 present.");
      }
      if (rest.length === 0) {
        return host.__tb_css_supports_condition(__tbToDOMString(property));
      }
      return host.__tb_css_supports_declaration(
        __tbToDOMString(property), __tbToDOMString(rest[0]));
    },
  };
  // A namespace object's class string is its identifier, and its members are
  // writable and configurable
  // (<https://webidl.spec.whatwg.org/#namespace-object>).
  __tbDefineProperty(css, Symbol.toStringTag, { value: 'CSS', configurable: true });
  Object.defineProperty(globalThis, 'CSS', {
    value: css,
    writable: true, configurable: true, enumerable: false,
  });
  // A computed style always resolves: an unset property reports its initial
  // value, not the empty string
  // (<https://drafts.csswg.org/cssom/#resolved-values>). The style engine's
  // resolved value is authoritative when it has one; the inline style and then
  // the initial-value table below cover what it does not resolve (custom
  // properties, properties outside the supported set).
  const __tbInitialValues = {
    visibility: 'visible',
    display: 'inline',
    cursor: 'auto',
    transform: 'none',
    'transform-origin': '50% 50%',
    'border-left-width': '0px',
    'border-top-width': '0px',
    'border-right-width': '0px',
    'border-bottom-width': '0px',
    'border-left-style': 'none',
    'border-top-style': 'none',
    'border-right-style': 'none',
    'border-bottom-style': 'none',
    'padding-left': '0px',
    'padding-top': '0px',
    'padding-right': '0px',
    'padding-bottom': '0px',
    'margin-left': '0px',
    'margin-top': '0px',
    'margin-right': '0px',
    'margin-bottom': '0px',
    color: 'rgb(0, 0, 0)',
    'background-color': 'rgba(0, 0, 0, 0)',
    'font-size': '16px',
    'font-weight': '400',
    'font-family': 'serif',
    'line-height': 'normal',
    width: 'auto',
    height: 'auto',
    'box-sizing': 'content-box',
    position: 'static',
    overflow: 'visible',
    opacity: '1',
    'z-index': 'auto',
    'text-align': 'start',
    'white-space': 'normal',
  };
  // A read-only declaration is the answer for an element that has no
  // computed style and for a pseudo-element argument that names none
  // (<https://drafts.csswg.org/cssom/#dom-window-getcomputedstyle>).
  const __tbReadOnlyDeclaration = () => {
    throw new __tbDomException('The declaration is read-only', 'NoModificationAllowedError');
  };
  const __tbEmptyComputedStyle = () => new Proxy({}, {
    get(target, property) {
      switch (property) {
        case 'length': return 0;
        case 'parentRule': return null;
        case 'cssText': return '';
        case 'getPropertyValue':
        case 'getPropertyPriority':
        case 'item':
        case 'toString':
          return () => '';
        case 'setProperty':
        case 'removeProperty':
          return __tbReadOnlyDeclaration;
        default:
          return typeof property === 'string' ? '' : undefined;
      }
    },
    set() { __tbReadOnlyDeclaration(); },
    defineProperty() { __tbReadOnlyDeclaration(); },
    deleteProperty() { __tbReadOnlyDeclaration(); },
  });
  Object.defineProperty(globalThis, 'getComputedStyle', {
    value: function(element, pseudoElt) {
      // A pseudo-element argument is parsed as a `<pseudo-element-selector>`
      // only when it starts with a colon. Anything else names the element
      // itself, and one that names a pseudo-element the style engine does not
      // cascade gets an empty declaration
      // (<https://drafts.csswg.org/cssom/#dom-window-getcomputedstyle>).
      let pseudo = 0;
      if (arguments.length > 1 && pseudoElt !== undefined && pseudoElt !== null) {
        const text = String(pseudoElt);
        if (text !== '' && text.charCodeAt(0) === 0x3A) {
          pseudo = host.__tb_parse_pseudo_element(text);
          if (pseudo < 0) return __tbEmptyComputedStyle();
        }
      }
      // An element with no computed style gets an empty read-only
      // declaration: not in the flat tree, not connected, or in a document
      // whose browsing context is not rendered
      // (<https://drafts.csswg.org/cssom/#dom-window-getcomputedstyle>).
      if (!element || typeof element.nodeType !== 'number'
          || !host.__tb_has_computed_style(element)) {
        return __tbEmptyComputedStyle();
      }
      const inline = element?.style ?? {};
      if (typeof inline.getPropertyValue !== 'function') {
        inline.getPropertyValue = function(name) { return this[name] ?? ''; };
      }
      const resolved = name => {
        if (!element || typeof element.nodeType !== 'number') return '';
        return host.__tb_resolved_style(element, name, pseudo);
      };
      // Every longhand the engine supports, then the element's custom
      // properties, in the order CSSOM gives for a computed style
      // (<https://drafts.csswg.org/cssom/#dom-window-getcomputedstyle>).
      const names = () => host.__tb_computed_style_names(element, pseudo);
      const read = name => {
        // The resolved value comes first: it carries the cascade, `!important`
        // declarations, and layout-dependent used values. The inline
        // declaration is only a fallback for elements the engine has no style
        // for (a property outside its tables), and it never applies to a
        // pseudo-element.
        // https://drafts.csswg.org/cssom/#dom-window-getcomputedstyle
        const engine = resolved(name);
        if (engine !== '') return engine;
        if (pseudo === 0) {
          const value = inline.getPropertyValue(name);
          if (value !== '') return value;
        }
        return __tbInitialValues[String(name).toLowerCase()] ?? '';
      };
      return new Proxy(inline, {
        get(target, property, receiver) {
          if (typeof property !== 'string') {
            // The declaration iterable surface must read this declaration's
            // own `length` and `item`, not the inline style it wraps
            // (<https://webidl.spec.whatwg.org/#idl-iterable>).
            const member = Reflect.get(CSSStyleDeclaration.prototype, property);
            return typeof member === 'function' ? member.bind(receiver) : member;
          }
          if (property === 'getPropertyValue') return read;
          if (property === 'length') return names().length;
          if (property === 'item') {
            return index => names()[index >>> 0] ?? '';
          }
          if (property === 'values' || property === 'keys' || property === 'entries'
              || property === 'forEach') {
            return Reflect.get(CSSStyleDeclaration.prototype, property).bind(receiver);
          }
          if (property === 'cssText') return '';
          if (__tbCanonicalIndex(property)) {
            const list = names();
            const index = property >>> 0;
            return index < list.length ? list[index] : undefined;
          }
          if (property === 'toString') return () => '';
          // A computed style carries no priority flags, so no declaration it
          // reports is `!important`
          // (<https://drafts.csswg.org/cssom/#dom-cssstyledeclaration-getpropertypriority>).
          if (property === 'getPropertyPriority') return () => '';
          if (property === 'setProperty' || property === 'removeProperty') {
            return __tbReadOnlyDeclaration;
          }
          // `cssFloat` is the one IDL name that does not map to its dashed
          // spelling: it reads the `float` property
          // (<https://drafts.csswg.org/cssom/#dom-cssstyleproperties-cssfloat>).
          const kebab = property === 'cssFloat'
            ? 'float'
            : property.replace(/[A-Z]/g, letter => '-' + letter.toLowerCase());
          const engine = resolved(kebab);
          if (engine !== '') return engine;
          if (pseudo !== 0) return kebab in __tbInitialValues ? __tbInitialValues[kebab] : undefined;
          const value = Reflect.get(target, property, receiver);
          if (value !== undefined && value !== null && value !== '') return value;
          if (kebab in __tbInitialValues) return __tbInitialValues[kebab];
          return value;
        },
        // A computed style declaration is immutable: CSSOM throws
        // `NoModificationAllowedError` for every write
        // (<https://drafts.csswg.org/cssom/#dom-window-getcomputedstyle>).
        set() { __tbReadOnlyDeclaration(); },
        defineProperty() { __tbReadOnlyDeclaration(); },
        deleteProperty() { __tbReadOnlyDeclaration(); },
      });
    },
    writable: true, configurable: true,
  });
  // Viewport and color-scheme queries used by pages and by CSSOM tests.
  // Color scheme must match Stylo's Device `PrefersColorScheme::Dark`.
  // https://drafts.csswg.org/cssom-view/#dom-window-matchmedia
  // https://drafts.csswg.org/mediaqueries-5/#mq-syntax
  const __tbColorScheme = 'dark';
  const __tbSplitMedia = (text, separator) => {
    const parts = [];
    let current = '';
    let depth = 0;
    for (let index = 0; index < text.length; index++) {
      const ch = text[index];
      if (ch === '(') depth++;
      if (ch === ')') depth = Math.max(0, depth - 1);
      if (depth === 0 && text.slice(index, index + separator.length).toLowerCase() === separator) {
        parts.push(current);
        current = '';
        index += separator.length - 1;
        continue;
      }
      current += ch;
    }
    parts.push(current);
    return parts;
  };
  const __tbPx = (raw) => {
    const value = String(raw).trim().toLowerCase();
    if (value.endsWith('px')) return Number(value.slice(0, -2));
    if (value.endsWith('em')) return Number(value.slice(0, -2)) * 16;
    return Number(value);
  };
  const __tbFeature = (raw) => {
    const inner = raw.trim().replace(/^\(/, '').replace(/\)$/, '').trim();
    if (!inner) return 'unknown';
    const colon = inner.indexOf(':');
    const name = (colon < 0 ? inner : inner.slice(0, colon)).trim().toLowerCase();
    const value = colon < 0 ? '' : inner.slice(colon + 1).trim().toLowerCase();
    if (name === 'prefers-color-scheme') {
      if (value === '') return true;
      if (value === 'dark' || value === 'light') return value === __tbColorScheme;
      if (value === 'no-preference') return false;
      return 'unknown';
    }
    const width = Number(globalThis.innerWidth);
    const height = Number(globalThis.innerHeight);
    if (name === 'width' || name === 'min-width' || name === 'max-width') {
      const px = __tbPx(value);
      if (!Number.isFinite(px)) return 'unknown';
      if (name === 'min-width') return width >= px;
      if (name === 'max-width') return width <= px;
      return width === px;
    }
    if (name === 'height' || name === 'min-height' || name === 'max-height') {
      const px = __tbPx(value);
      if (!Number.isFinite(px)) return 'unknown';
      if (name === 'min-height') return height >= px;
      if (name === 'max-height') return height <= px;
      return height === px;
    }
    if (name === 'hover') {
      if (value === '' || value === 'hover') return true;
      if (value === 'none') return false;
      return 'unknown';
    }
    if (name === 'pointer') {
      if (value === '' || value === 'fine') return true;
      if (value === 'coarse' || value === 'none') return false;
      return 'unknown';
    }
    return 'unknown';
  };
  const __tbMediaQuery = (text) => {
    text = String(text).trim().toLowerCase();
    if (!text) return true;
    let negated = false;
    if (text.startsWith('only ')) text = text.slice(5).trim();
    if (text.startsWith('not ')) {
      negated = true;
      text = text.slice(4).trim();
    }
    const parts = __tbSplitMedia(text, ' and ').map(part => part.trim()).filter(part => part);
    let known = true;
    let matches = true;
    for (const part of parts) {
      if (part.startsWith('(')) {
        const feature = __tbFeature(part);
        if (feature === 'unknown') {
          known = false;
          matches = false;
        } else if (!feature) {
          matches = false;
        }
      } else if (part !== 'all' && part !== 'screen') {
        matches = false;
      }
    }
    // Unknown features stay false under `not`
    // (<https://drafts.csswg.org/mediaqueries-5/#error-handling>).
    if (!known) return false;
    return negated ? !matches : matches;
  };
  const __tbMediaQueryList = (text) => __tbSplitMedia(String(text), ',').some(__tbMediaQuery);
  // `MediaQueryList` is an `EventTarget` with the legacy listener pair
  // (<https://drafts.csswg.org/cssom-view/#mediaquerylist>). The constructor
  // is captured here so a page-replaced `EventTarget` cannot build the list.
  const __tbMediaSlots = host.slots('MediaQueryList');
  const __tbMediaLists = [];
  const __tbEventTarget = globalThis.EventTarget;
  const __tbEvent = globalThis.Event;
  const __tbAddEventListener = __tbEventTarget.prototype.addEventListener;
  const __tbRemoveEventListener = __tbEventTarget.prototype.removeEventListener;
  const __tbDispatchEvent = __tbEventTarget.prototype.dispatchEvent;
  function MediaQueryList() {
    throw new TypeError('Illegal constructor');
  }
  MediaQueryList.prototype = Object.create(__tbEventTarget.prototype, {
    constructor: { value: MediaQueryList, writable: true, configurable: true },
  });
  Object.defineProperty(MediaQueryList.prototype, Symbol.toStringTag, {
    value: 'MediaQueryList', writable: false, enumerable: false, configurable: true,
  });
  function __tbMediaBrand(list) {
    const slot = __tbMediaSlots.get(list);
    if (!slot) throw new TypeError('Illegal invocation');
    return slot;
  }
  Object.defineProperty(MediaQueryList.prototype, 'media', {
    get() { return __tbMediaBrand(this).media; },
    enumerable: true, configurable: true,
  });
  Object.defineProperty(MediaQueryList.prototype, 'matches', {
    get() { return __tbMediaQueryList(__tbMediaBrand(this).media); },
    enumerable: true, configurable: true,
  });
  function __tbMediaAddListener(callback) {
    const list = this;
    __tbMediaBrand(list);
    if (callback == null) return;
    __tbApply(__tbAddEventListener, list, ['change', callback]);
  }
  function __tbMediaRemoveListener(callback) {
    const list = this;
    __tbMediaBrand(list);
    if (callback == null) return;
    __tbApply(__tbRemoveEventListener, list, ['change', callback]);
  }
  Object.defineProperty(MediaQueryList.prototype, 'addListener', {
    value: __tbMediaAddListener, writable: true, enumerable: true, configurable: true,
  });
  Object.defineProperty(MediaQueryList.prototype, 'removeListener', {
    value: __tbMediaRemoveListener, writable: true, enumerable: true, configurable: true,
  });
  // The dispatcher already calls a target's `on<type>` property after the
  // listener list (`crates/renderer/src/js/events.rs`), so the slot is the only
  // registration: adding the handler as a listener too would fire it twice
  // (`MediaQueryList-extends-EventTarget.html`, "onchange removes listener").
  // A non-callable value becomes null, as event handler IDL attributes do.
  Object.defineProperty(MediaQueryList.prototype, 'onchange', {
    get() { return __tbMediaBrand(this).onchange; },
    set(value) {
      __tbMediaBrand(this).onchange = typeof value === 'function' ? value : null;
    },
    enumerable: true, configurable: true,
  });
  // `MediaQueryListEvent` carries the query text and its result
  // (<https://drafts.csswg.org/cssom-view/#mediaquerylistevent>). It extends
  // the captured `Event`, so a page-replaced global cannot change the shape of
  // a change event.
  const __tbMediaEventSlots = host.slots('MediaQueryListEvent');
  function __tbMediaEventBrand(event) {
    const slot = __tbMediaEventSlots.get(event);
    if (!slot) throw new TypeError('Illegal invocation');
    return slot;
  }
  globalThis.MediaQueryListEvent = class MediaQueryListEvent extends __tbEvent {
    constructor(type, init) {
      const eventInit = init === undefined ? {} : Object(init);
      super(String(type), eventInit);
      __tbMediaEventSlots.set(this, {
        media: eventInit.media === undefined ? '' : String(eventInit.media),
        matches: eventInit.matches === undefined ? false : Boolean(eventInit.matches),
      });
    }
    get media() { return __tbMediaEventBrand(this).media; }
    get matches() { return __tbMediaEventBrand(this).matches; }
  };
  __tbDefineProperty(globalThis.MediaQueryListEvent.prototype, Symbol.toStringTag, {
    value: 'MediaQueryListEvent', writable: false, enumerable: false, configurable: true,
  });
  const __tbMediaQueryListEvent = globalThis.MediaQueryListEvent;
  function __tbReportMediaChanges() {
    for (const list of __tbMediaLists) {
      const slot = __tbMediaSlots.get(list);
      if (!slot) continue;
      const matches = __tbMediaQueryList(slot.media);
      if (matches === slot.matches) continue;
      slot.matches = matches;
      const event = new __tbMediaQueryListEvent('change', {
        media: slot.media,
        matches,
      });
      __tbApply(__tbDispatchEvent, list, [event]);
    }
  }
  host.__tbReportMediaChanges = __tbReportMediaChanges;
  Object.defineProperty(globalThis, 'MediaQueryList', {
    value: MediaQueryList, writable: true, configurable: true,
  });
  Object.defineProperty(globalThis, 'matchMedia', {
    value: function(media) {
      const query = String(media);
      const list = __tbConstruct(__tbEventTarget, [], MediaQueryList);
      __tbMediaSlots.set(list, {
        media: query,
        matches: __tbMediaQueryList(query),
        onchange: null,
      });
      __tbMediaLists.push(list);
      return list;
    },
    writable: true, configurable: true,
  });

  // ── visualViewport ──────────────────────────────────────────────────
  // This engine has no pinch zoom and no visual chrome that scrolls apart from
  // the layout viewport, so the visual viewport always coincides with it: its
  // offset is zero and its scale is 1. `width`/`height` are the layout
  // viewport's, and `pageLeft`/`pageTop` are the page's scroll offset
  // (<https://drafts.csswg.org/cssom-view/#the-visualviewport-interface>).
  const __tbVisualViewportSlots = host.slots('VisualViewport');
  function VisualViewport() {
    throw new TypeError('Illegal constructor');
  }
  VisualViewport.prototype = Object.create(__tbEventTarget.prototype, {
    constructor: { value: VisualViewport, writable: true, configurable: true },
  });
  __tbDefineProperty(VisualViewport.prototype, Symbol.toStringTag, {
    value: 'VisualViewport', writable: false, enumerable: false, configurable: true,
  });
  function __tbVisualViewportBrand(viewport) {
    const slot = __tbVisualViewportSlots.get(viewport);
    if (!slot) throw new TypeError('Illegal invocation');
    return slot;
  }
  // A viewport bound to another frame reads that frame's metrics through
  // `frame`; this realm's own viewport reads them from the current world
  const __tbVisualViewportSize = (slot) => slot.frame === undefined
    ? __tbViewportSize()
    : host.__tbFrameViewportSize(slot.frame);
  const __tbVisualViewportScroll = (slot) => slot.frame === undefined
    ? __tbViewportScroll()
    : host.__tbFrameViewportScroll(slot.frame);
  for (const [name, read] of [
    ['offsetLeft', () => 0],
    ['offsetTop', () => 0],
    ['pageLeft', slot => __tbVisualViewportScroll(slot)[0]],
    ['pageTop', slot => __tbVisualViewportScroll(slot)[1]],
    ['width', slot => __tbVisualViewportSize(slot)[0]],
    ['height', slot => __tbVisualViewportSize(slot)[1]],
    ['scale', () => 1],
  ]) {
    __tbDefineProperty(VisualViewport.prototype, name, {
      get() { return read(__tbVisualViewportBrand(this)); },
      enumerable: true, configurable: true,
    });
  }
  // The handler attributes are plain properties on the target, which the
  // dispatcher reads after the listener list, exactly like `MediaQueryList`'s
  // `onchange`.
  for (const name of ['onresize', 'onscroll', 'onscrollend']) {
    __tbDefineProperty(VisualViewport.prototype, name, {
      get() { return __tbVisualViewportBrand(this)[name]; },
      set(value) {
        __tbVisualViewportBrand(this)[name] = typeof value === 'function' ? value : null;
      },
      enumerable: true, configurable: true,
    });
  }
  const __tbVisualViewport = (() => {
    const viewport = __tbConstruct(__tbEventTarget, [], VisualViewport);
    const size = __tbViewportSize();
    __tbVisualViewportSlots.set(viewport, {
      // The size the last `resize` reported, so a layout that does not move
      // the viewport fires nothing.
      width: size[0],
      height: size[1],
      onresize: null,
      onscroll: null,
      onscrollend: null,
    });
    return viewport;
  })();
  __tbReplaceable('visualViewport', () => __tbVisualViewport);
  // `[SameObject]`: a frame answers the same `VisualViewport` every read, and
  // its getters read the target frame's live metrics
  // (<https://drafts.csswg.org/cssom-view/#dom-window-visualviewport>).
  const __tbFrameVisualViewports = new __tbPrivateMap();
  host.__tbFrameVisualViewport = function __tbFrameVisualViewport(frame) {
    const cached = __tbFrameVisualViewports.get(frame);
    if (cached !== undefined) return cached;
    const viewport = __tbConstruct(__tbEventTarget, [], VisualViewport);
    __tbVisualViewportSlots.set(viewport, {
      frame,
      width: 0,
      height: 0,
      onresize: null,
      onscroll: null,
      onscrollend: null,
    });
    __tbFrameVisualViewports.set(frame, viewport);
    return viewport;
  };
  // The `scroll`/`scrollTo`/`scrollBy` of a frame whose realm has not started:
  // they scroll the target frame's viewport, which is what the WindowProxy's
  // own method would do
  // (<https://drafts.csswg.org/cssom-view/#dom-window-scrollto>).
  host.__tbFrameScrollMethod = (frame, relative) => function(...args) {
    return __tbScroll({
      current: (_receiver, vertical) =>
        host.__tbFrameViewportScroll(frame)[vertical ? 1 : 0],
      scrollTo: (_receiver, x, y, behavior) =>
        host.__tbFrameScrollViewport(frame, x, y, behavior),
    }, this, args, relative, () => true);
  };
  // Dispatches `resize` when the viewport size changed since the last report,
  // which is what the spec's "run the resize steps" does for the visual
  // viewport (<https://drafts.csswg.org/cssom-view/#run-the-resize-steps>).
  function __tbReportVisualViewportChanges() {
    const slot = __tbVisualViewportSlots.get(__tbVisualViewport);
    if (!slot) return;
    const size = __tbViewportSize();
    if (size[0] === slot.width && size[1] === slot.height) return;
    slot.width = size[0];
    slot.height = size[1];
    __tbApply(__tbDispatchEvent, __tbVisualViewport, [new __tbEvent('resize')]);
  }
  host.__tbReportVisualViewportChanges = __tbReportVisualViewportChanges;
  // The visual viewport scrolls with the page, so it reports the same
  // `scroll`/`scrollend` the viewport does, at its own event target
  // (<https://drafts.csswg.org/cssom-view/#scrolling-events>).
  host.__tbVisualViewportScroll = function __tbVisualViewportScroll(type) {
    __tbApply(__tbDispatchEvent, __tbVisualViewport, [new __tbEvent(String(type))]);
  };
  Object.defineProperty(globalThis, 'VisualViewport', {
    value: VisualViewport, writable: true, configurable: true,
  });
}
