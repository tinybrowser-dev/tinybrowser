  // `window` viewport metrics track the live document viewport, so CDP
  // emulation changes are visible without re-installing globals
  // (<https://drafts.csswg.org/cssom-view/#dom-window-innerwidth>).
  const __tbViewportSize = () => host.__tb_viewport_size();
  Object.defineProperty(globalThis, 'innerWidth', {
    get: () => __tbViewportSize()[0], configurable: true,
  });
  Object.defineProperty(globalThis, 'innerHeight', {
    get: () => __tbViewportSize()[1], configurable: true,
  });
  // No browser chrome exists, so the outer window equals the inner viewport
  // (<https://drafts.csswg.org/cssom-view/#dom-window-outerwidth>).
  Object.defineProperty(globalThis, 'outerWidth', {
    get: () => __tbViewportSize()[0], configurable: true,
  });
  Object.defineProperty(globalThis, 'outerHeight', {
    get: () => __tbViewportSize()[1], configurable: true,
  });

// Constructable stylesheet surface used by component libraries, plus the
// associated CSS style sheet for `<style>` and `<link rel=stylesheet>`.
// https://drafts.csswg.org/cssom/#the-cssstylesheet-interface
{
  // Captured at realm init so a page-patched builtin cannot retarget a rule
  // path or swallow an exception.
  const __tbReflectGet = Reflect.get;
  const __tbBind = Function.prototype.bind;
  const __tbDomException = globalThis.DOMException;
  const __tbCharCodeAt = String.prototype.charCodeAt;
  const __tbSheetsById = new Map();
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
  function CSSStyleDeclaration() { __tbIllegal(); }
  CSSStyleRule.prototype = Object.create(CSSRule.prototype);
  CSSGroupingRule.prototype = Object.create(CSSRule.prototype);
  CSSConditionRule.prototype = Object.create(CSSGroupingRule.prototype);
  CSSMediaRule.prototype = Object.create(CSSConditionRule.prototype);
  CSSSupportsRule.prototype = Object.create(CSSConditionRule.prototype);
  CSSImportRule.prototype = Object.create(CSSRule.prototype);
  CSSFontFaceRule.prototype = Object.create(CSSRule.prototype);
  CSSKeyframesRule.prototype = Object.create(CSSRule.prototype);
  CSSKeyframeRule.prototype = Object.create(CSSRule.prototype);
  // Each interface owns its `constructor`, so `rule.constructor.name` names
  // the rule's real interface instead of inheriting `CSSRule`
  // (<https://webidl.spec.whatwg.org/#interface-object>).
  for (const ctor of [
    CSSStyleRule, CSSGroupingRule, CSSConditionRule, CSSMediaRule, CSSSupportsRule,
    CSSImportRule, CSSFontFaceRule, CSSKeyframesRule, CSSKeyframeRule,
  ]) {
    __tbDefineProperty(ctor.prototype, 'constructor', {
      value: ctor, writable: true, configurable: true,
    });
  }
  const __tbRulePrototype = {
    CSSStyleRule, CSSGroupingRule, CSSConditionRule, CSSMediaRule, CSSSupportsRule,
    CSSImportRule, CSSFontFaceRule, CSSKeyframesRule, CSSKeyframeRule, CSSRule,
  };
  for (const name of Object.keys(__tbRulePrototype)) {
    __tbDefineProperty(globalThis, name, {
      value: __tbRulePrototype[name], writable: true, configurable: true,
    });
    __tbDefineProperty(globalThis, 'CSSStyleDeclaration', {
      value: CSSStyleDeclaration, writable: true, configurable: true,
    });
  }
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
    const state = host.__tb_sheet_origin_state(sheet.__tbOwner, sheet.__tbId);
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
        return host.__tb_sheet_style_css_text(sheet.__tbOwner, sheet.__tbId, pathKey());
      },
      enumerable: true, configurable: true,
    });
    __tbDefineProperty(style, 'length', {
      get() {
        if (entry.detached) return 0;
        const names = host.__tb_sheet_style_names(sheet.__tbOwner, sheet.__tbId, pathKey());
        return names ? names.length : 0;
      },
      enumerable: true, configurable: true,
    });
    style.getPropertyValue = function(name) {
      if (entry.detached) return '';
      const found = host.__tb_sheet_style_get_property(sheet.__tbOwner, sheet.__tbId, pathKey(), String(name));
      return found ? found.value : '';
    };
    style.getPropertyPriority = function(name) {
      if (entry.detached) return '';
      const found = host.__tb_sheet_style_get_property(sheet.__tbOwner, sheet.__tbId, pathKey(), String(name));
      return found && found.important ? 'important' : '';
    };
    style.setProperty = function(name, value, priority) {
      if (entry.detached) return;
      host.__tb_sheet_style_set_property(
        sheet.__tbOwner, sheet.__tbId, pathKey(), String(name),
        value == null ? '' : String(value), String(priority) === 'important',
      );
    };
    style.item = function(index) {
      if (arguments.length < 1) throw new TypeError('item requires 1 argument');
      if (entry.detached) return '';
      const names = host.__tb_sheet_style_names(sheet.__tbOwner, sheet.__tbId, pathKey()) || [];
      index = index >>> 0;
      return index < names.length ? names[index] : '';
    };
    return style;
  }
  function __tbCachedRule(sheet, path, parentRule) {
    for (const entry of sheet.__tbRules) {
      if (!entry.detached && __tbSamePath(entry.path, path)) return entry.rule;
    }
    const entry = { path: path.slice(), detached: false, frozenText: '', rule: null, list: null, style: null };
    const rule = Object.create(CSSRule.prototype);
    const read = () => {
      if (entry.detached) return null;
      return host.__tb_sheet_rule_info(sheet.__tbOwner, sheet.__tbId, __tbRulePath(entry.path));
    };
    const info = read() || { interface: 'CSSRule', type: 0, cssText: '', hasChildRules: false, hasStyle: false };
    const ctor = __tbRulePrototype[info.interface] || CSSRule;
    __tbSetPrototypeOf(rule, ctor.prototype);
    __tbDefineProperty(rule, 'type', { get() { const next = read(); return next ? next.type : info.type; }, enumerable: true, configurable: true });
    __tbDefineProperty(rule, 'cssText', {
      get() { return entry.detached ? entry.frozenText : (read() || {}).cssText || ''; },
      enumerable: true, configurable: true,
    });
    __tbDefineProperty(rule, 'parentRule', { get() { return parentRule; }, enumerable: true, configurable: true });
    __tbDefineProperty(rule, 'parentStyleSheet', {
      get() { return entry.detached ? null : sheet; },
      enumerable: true, configurable: true,
    });
    if (info.selectorText !== undefined) {
      __tbDefineProperty(rule, 'selectorText', {
        get() { const next = read(); return next && next.selectorText ? next.selectorText : ''; },
        // A valid selector list replaces the rule's selectors and re-enters the
        // cascade; an invalid one leaves the rule unchanged without throwing
        // (<https://drafts.csswg.org/cssom/#dom-cssstylerule-selectortext>).
        set(value) {
          if (entry.detached) return;
          host.__tb_sheet_rule_set_selector_text(
            sheet.__tbOwner, sheet.__tbId, __tbRulePath(entry.path), String(value),
          );
        },
        enumerable: true, configurable: true,
      });
    }
    if (info.conditionText !== undefined) {
      __tbDefineProperty(rule, 'conditionText', {
        get() { const next = read(); return next && next.conditionText ? next.conditionText : ''; },
        enumerable: true, configurable: true,
      });
    }
    for (const key of ['keyText', 'name', 'href', 'media']) {
      if (info[key] === undefined) continue;
      const attribute = key;
      __tbDefineProperty(rule, attribute, {
        get() { const next = read(); return next && next[attribute] ? next[attribute] : ''; },
        enumerable: true, configurable: true,
      });
    }
    if (info.hasChildRules) {
      __tbDefineProperty(rule, 'cssRules', {
        get() {
          if (entry.list === null) entry.list = __tbLiveRuleList(sheet, entry, rule);
          return entry.list;
        },
        enumerable: true, configurable: true,
      });
      rule.insertRule = function(text, index) {
        return __tbInsert(sheet, entry.path, text, index, arguments.length);
      };
      rule.deleteRule = function(index) {
        return __tbDelete(sheet, entry.path, index, arguments.length);
      };
    }
    if (info.hasStyle) {
      __tbDefineProperty(rule, 'style', {
        get() {
          if (entry.style === null) entry.style = __tbStyle(sheet, entry);
          return entry.style;
        },
        enumerable: true, configurable: true,
      });
    }
    entry.rule = rule;
    sheet.__tbRules.push(entry);
    return rule;
  }
  // Live `CSSRuleList`. `item` returns the same CSSRule for as long as that
  // rule exists (<https://drafts.csswg.org/cssom/#the-cssrulelist-interface>).
  function __tbLiveRuleList(sheet, parentEntry, parentRule) {
    const listPath = () => parentEntry ? parentEntry.path : [];
    const list = {
      get length() {
        if (parentEntry && parentEntry.detached) return 0;
        __tbRequireClean(sheet);
        const count = host.__tb_sheet_rule_count(sheet.__tbOwner, sheet.__tbId, __tbRulePath(listPath()));
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
    const inserted = host.__tb_sheet_insert_rule(sheet.__tbOwner, sheet.__tbId, __tbRulePath(listPath), String(text), index);
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
      const info = host.__tb_sheet_rule_info(sheet.__tbOwner, sheet.__tbId, __tbRulePath(entry.path));
      entry.frozenText = info ? info.cssText : '';
      entry.detached = true;
    }
    host.__tb_sheet_delete_rule(sheet.__tbOwner, sheet.__tbId, __tbRulePath(listPath), index);
    for (const entry of sheet.__tbRules) {
      if (entry.detached || !__tbUnder(entry, listPath)) continue;
      if (entry.path[listPath.length] > index) entry.path[listPath.length] -= 1;
    }
  }
  class CSSStyleSheet {
    constructor() {
      this.cssRules = [];
      this.__tbRules = [];
    }
    replaceSync(text) {
      if (this.__tbId !== undefined) {
        throw new __tbDomException('associated sheet is not constructable', 'NotAllowedError');
      }
      const cssText = String(text);
      this.cssRules = cssText ? [{ cssText }] : [];
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
      if (this.__tbId === undefined) {
        throw new __tbDomException('constructable sheet has no parsed rule list', 'NotSupportedError');
      }
      return __tbInsert(this, [], rule, index, arguments.length);
    }
    deleteRule(index) {
      if (this.__tbId === undefined) {
        throw new __tbDomException('constructable sheet has no parsed rule list', 'NotSupportedError');
      }
      __tbDelete(this, [], index, arguments.length);
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
    sheet = new CSSStyleSheet();
    sheet.__tbId = id;
    sheet.__tbOwner = element;
    // The constructor's own array is the constructable-sheet list. An
    // associated sheet's `cssRules` is the engine's list instead
    // (<https://drafts.csswg.org/cssom/#dom-cssstylesheet-cssrules>).
    delete sheet.cssRules;
    __tbDefineProperty(sheet, 'cssRules', {
      get() {
        if (sheet.__tbList === undefined) sheet.__tbList = __tbLiveRuleList(sheet, null, null);
        return sheet.__tbList;
      },
      enumerable: true,
      configurable: true,
    });
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
  Object.defineProperty(globalThis, 'CSS', {
    value: Object.freeze({
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
      supports() { return false; },
    }),
    writable: true, configurable: true,
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
  Object.defineProperty(globalThis, 'getComputedStyle', {
    value: function(element) {
      const inline = element?.style ?? {};
      if (typeof inline.getPropertyValue !== 'function') {
        inline.getPropertyValue = function(name) { return this[name] ?? ''; };
      }
      const resolved = name => {
        if (!element || typeof element.nodeType !== 'number') return '';
        return host.__tb_resolved_style(element, name);
      };
      const read = name => {
        // The resolved value comes first: it carries the cascade, `!important`
        // declarations, and layout-dependent used values. The inline
        // declaration is only a fallback for elements the engine has no style
        // for (a detached element, or a property outside its tables).
        // https://drafts.csswg.org/cssom/#dom-window-getcomputedstyle
        const engine = resolved(name);
        if (engine !== '') return engine;
        const value = inline.getPropertyValue(name);
        if (value !== '') return value;
        return __tbInitialValues[String(name).toLowerCase()] ?? '';
      };
      return new Proxy(inline, {
        get(target, property, receiver) {
          if (typeof property !== 'string') return Reflect.get(target, property, receiver);
          if (property === 'getPropertyValue') return read;
          const kebab = property.replace(/[A-Z]/g, letter => '-' + letter.toLowerCase());
          const engine = resolved(kebab);
          if (engine !== '') return engine;
          const value = Reflect.get(target, property, receiver);
          if (value !== undefined && value !== null && value !== '') return value;
          if (kebab in __tbInitialValues) return __tbInitialValues[kebab];
          return value;
        },
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
  Object.defineProperty(MediaQueryList.prototype, 'onchange', {
    get() { return __tbMediaBrand(this).onchange; },
    set(value) {
      const slot = __tbMediaBrand(this);
      if (value != null && typeof value !== 'function') throw new TypeError('not a function');
      const next = typeof value === 'function' ? value : null;
      if (slot.onchange) __tbApply(__tbRemoveEventListener, this, ['change', slot.onchange]);
      slot.onchange = next;
      if (next) __tbApply(__tbAddEventListener, this, ['change', next]);
    },
    enumerable: true, configurable: true,
  });
  function __tbReportMediaChanges() {
    for (const list of __tbMediaLists) {
      const slot = __tbMediaSlots.get(list);
      if (!slot) continue;
      const matches = __tbMediaQueryList(slot.media);
      if (matches === slot.matches) continue;
      slot.matches = matches;
      const event = new __tbEvent('change');
      Object.defineProperty(event, 'media', { value: slot.media });
      Object.defineProperty(event, 'matches', { value: matches });
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
}
