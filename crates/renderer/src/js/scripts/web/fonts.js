// https://drafts.csswg.org/css-font-loading/#fontfaceset
const __tbFontFaceSlots = new WeakMap();
const __tbFontSetSlots = new WeakMap();
const __tbCssFontFaces = new WeakMap();
const __tbFontFaceSetToken = Symbol('FontFaceSet');
const __tbFontEvent = globalThis.Event;
const __tbFontEventTarget = globalThis.EventTarget;
const __tbFontDOMException = globalThis.DOMException;
const __tbFontDispatch = __tbFontEventTarget.prototype.dispatchEvent;
const __tbFontAddEventListener = __tbFontEventTarget.prototype.addEventListener;
const __tbFontRemoveEventListener = __tbFontEventTarget.prototype.removeEventListener;

const __tbFontCssWide = [
  'initial', 'inherit', 'unset', 'default', 'revert', 'revert-layer',
];
const __tbFontGenerics = [
  'serif', 'sans-serif', 'cursive', 'fantasy', 'monospace', 'system-ui',
  'ui-serif', 'ui-sans-serif', 'ui-monospace', 'ui-rounded', 'math', 'emoji',
  'fangsong',
];
const __tbFontStretches = [
  'ultra-condensed', 'extra-condensed', 'condensed', 'semi-condensed',
  'normal', 'semi-expanded', 'expanded', 'extra-expanded', 'ultra-expanded',
];

function __tbFontError(message, name) {
  return new __tbFontDOMException(message, name);
}

function __tbFontIsKeyword(value, list) {
  const lower = __tbApply(__tbStringToLowerCase, value, []);
  return __tbArray.indexOf(list, lower) !== -1;
}

// https://drafts.csswg.org/css-fonts-4/#family-name-syntax
function __tbFontIsFamilyName(name) {
  if (name.length === 0) return false;
  if (name !== __tbApply(__tbStringTrim, name, [])) return false;
  if (__tbApply(__tbStringSplit, name, ['  ']).length !== 1) return false;
  if (__tbFontIsKeyword(name, __tbFontCssWide)) return false;
  if (__tbFontIsKeyword(name, __tbFontGenerics)) return false;
  const parts = __tbApply(__tbStringSplit, name, [' ']);
  for (let index = 0; index < parts.length; index++) {
    if (!/^-?[A-Za-z_\u0080-\uFFFF][A-Za-z0-9_\-\u0080-\uFFFF]*$/.test(parts[index])) {
      return false;
    }
  }
  return true;
}

function __tbFontSerializeFamily(raw) {
  const text = __tbStringCtor(raw);
  let name = text;
  const quoted = /^"((?:[^"\\]|\\.)*)"$/.exec(text);
  if (quoted !== null) {
    name = quoted[1].replace(/\\(.)/g, '$1');
  }
  if (__tbFontIsFamilyName(name)) return name;
  return '"' + name.replace(/\\/g, '\\\\').replace(/"/g, '\\"') + '"';
}

function __tbFontUnquoteFamily(value) {
  const text = __tbApply(__tbStringTrim, value, []);
  if (text.length >= 2 && text[0] === '"' && text[text.length - 1] === '"') {
    return text.slice(1, -1).replace(/\\(.)/g, '$1');
  }
  if (text.length >= 2 && text[0] === "'" && text[text.length - 1] === "'") {
    return text.slice(1, -1).replace(/\\(.)/g, '$1');
  }
  return text;
}

function __tbFontHexStart(part) {
  return parseInt(part.replace(/\?/g, '0'), 16);
}

function __tbFontHexEnd(part) {
  return parseInt(part.replace(/\?/g, 'f'), 16);
}

function __tbFontHex(value) {
  return value.toString(16).toUpperCase();
}

function __tbFontNormalizeUnicodeRange(raw) {
  const text = __tbApply(__tbStringTrim, __tbStringCtor(raw), []);
  if (text.length === 0 || __tbFontIsKeyword(text, ['normal'])) {
    throw __tbFontError('invalid unicode-range', 'SyntaxError');
  }
  const parts = __tbApply(__tbStringSplit, text, [',']);
  const result = [];
  for (let index = 0; index < parts.length; index++) {
    const part = __tbApply(__tbStringTrim, parts[index], []);
    const match = /^[uU]\+([0-9a-fA-F?]{1,6})(?:-([0-9a-fA-F]{1,6}))?$/.exec(part);
    if (match === null) {
      throw __tbFontError('invalid unicode-range', 'SyntaxError');
    }
    let start;
    let end;
    if (match[2] !== undefined) {
      start = parseInt(match[1], 16);
      end = parseInt(match[2], 16);
    } else if (match[1].indexOf('?') !== -1) {
      start = __tbFontHexStart(match[1]);
      end = __tbFontHexEnd(match[1]);
    } else {
      start = parseInt(match[1], 16);
      end = start;
    }
    if (start > end || end > 0x10ffff) {
      throw __tbFontError('invalid unicode-range', 'SyntaxError');
    }
    result.push(
      start === end
        ? `U+${__tbFontHex(start)}`
        : `U+${__tbFontHex(start)}-${__tbFontHex(end)}`,
    );
  }
  return __tbArray.join(result, ', ');
}

function __tbFontNormalizeWeight(raw) {
  const text = __tbApply(__tbStringTrim, __tbStringCtor(raw), []);
  if (__tbFontIsKeyword(text, ['normal', 'bold'])) {
    return __tbApply(__tbStringToLowerCase, text, []);
  }
  if (!/^[1-9][0-9]{0,3}$/.test(text)) {
    throw __tbFontError('invalid font-weight', 'SyntaxError');
  }
  const value = __tbNumberCtor(text);
  if (value < 1 || value > 1000) {
    throw __tbFontError('invalid font-weight', 'SyntaxError');
  }
  return __tbStringCtor(value);
}

function __tbFontNormalizeStyle(raw) {
  const text = __tbApply(__tbStringTrim, __tbStringCtor(raw), []);
  const lower = __tbApply(__tbStringToLowerCase, text, []);
  if (lower === 'normal' || lower === 'italic') return lower;
  const oblique = /^oblique(?:\s+(-?(?:\d+|\d*\.\d+))(deg|grad|rad|turn))?$/i.exec(text);
  if (oblique !== null) {
    return oblique[1] === undefined
      ? 'oblique'
      : `oblique ${oblique[1]}${__tbApply(__tbStringToLowerCase, oblique[2], [])}`;
  }
  throw __tbFontError('invalid font-style', 'SyntaxError');
}

function __tbFontNormalizeStretch(raw) {
  const text = __tbApply(__tbStringTrim, __tbStringCtor(raw), []);
  const lower = __tbApply(__tbStringToLowerCase, text, []);
  if (__tbArray.indexOf(__tbFontStretches, lower) !== -1) return lower;
  const percentage = /^(\d+|\d*\.\d+)%$/.exec(text);
  if (percentage !== null && __tbNumberCtor(percentage[1]) >= 0) {
    return `${percentage[1]}%`;
  }
  throw __tbFontError('invalid font-stretch', 'SyntaxError');
}

function __tbFontNormalizeFeatureSettings(raw) {
  const text = __tbApply(__tbStringTrim, __tbStringCtor(raw), []);
  if (text.length === 0 || __tbFontIsKeyword(text, ['normal'])) return 'normal';
  const parts = __tbApply(__tbStringSplit, text, [',']);
  const result = [];
  for (let index = 0; index < parts.length; index++) {
    const match = /^\s*(?:"([^"]*)"|'([^']*)')\s*(.*?)\s*$/.exec(parts[index]);
    if (match === null) {
      throw __tbFontError('invalid font-feature-settings', 'SyntaxError');
    }
    const tag = match[1] !== undefined ? match[1] : match[2];
    const value = match[3];
    let suffix = '';
    if (value.length !== 0) {
      const lower = __tbApply(__tbStringToLowerCase, value, []);
      if (lower === 'off') {
        suffix = ' 0';
      } else if (lower !== 'on' && lower !== '1') {
        if (!/^[0-9]{1,4}$/.test(value)) {
          throw __tbFontError('invalid font-feature-settings', 'SyntaxError');
        }
        suffix = ` ${value}`;
      }
    }
    result.push(`"${tag}"${suffix}`);
  }
  return __tbArray.join(result, ', ');
}

function __tbFontNormalizeVariationSettings(raw) {
  const text = __tbApply(__tbStringTrim, __tbStringCtor(raw), []);
  if (text.length === 0 || __tbFontIsKeyword(text, ['normal'])) return 'normal';
  const parts = __tbApply(__tbStringSplit, text, [',']);
  const result = [];
  for (let index = 0; index < parts.length; index++) {
    const match = /^\s*(?:"([^"]*)"|'([^']*)')\s+(-?(?:\d+|\d*\.\d+))\s*$/.exec(parts[index]);
    if (match === null) {
      throw __tbFontError('invalid font-variation-settings', 'SyntaxError');
    }
    const tag = match[1] !== undefined ? match[1] : match[2];
    result.push(`"${tag}" ${match[3]}`);
  }
  return __tbArray.join(result, ', ');
}

function __tbFontNormalizeDisplay(raw) {
  const lower = __tbApply(__tbStringToLowerCase, __tbApply(__tbStringTrim, __tbStringCtor(raw), []), []);
  if (__tbArray.indexOf(['auto', 'block', 'swap', 'fallback', 'optional'], lower) === -1) {
    throw __tbFontError('invalid font-display', 'SyntaxError');
  }
  return lower;
}

function __tbFontNormalizeOverride(raw, name) {
  const text = __tbApply(__tbStringTrim, __tbStringCtor(raw), []);
  if (__tbFontIsKeyword(text, ['normal'])) return 'normal';
  const percentage = /^(\d+|\d*\.\d+)%$/.exec(text);
  if (percentage === null || __tbNumberCtor(percentage[1]) < 0) {
    throw __tbFontError(`invalid ${name}`, 'SyntaxError');
  }
  return `${percentage[1]}%`;
}

function __tbFontNormalizeSizeAdjust(raw) {
  const text = __tbApply(__tbStringTrim, __tbStringCtor(raw), []);
  const percentage = /^(\d+|\d*\.\d+)%$/.exec(text);
  if (percentage === null || __tbNumberCtor(percentage[1]) < 0) {
    throw __tbFontError('invalid size-adjust', 'SyntaxError');
  }
  return `${percentage[1]}%`;
}

function __tbFontNormalizeVariant(raw) {
  const text = __tbApply(__tbStringTrim, __tbStringCtor(raw), []);
  return text.length === 0 ? 'normal' : text;
}

function __tbFontDescriptor(descriptors, key, lower) {
  const value = descriptors[key];
  if (value === undefined || value === null || value === '') return null;
  return __tbApply(lower ? __tbStringToLowerCase : __tbStringTrim, __tbStringCtor(value), []);
}

// https://drafts.csswg.org/css-font-loading/#font-face-constructor
function __tbFontDescriptors(descriptors) {
  const state = {
    style: 'normal',
    weight: 'normal',
    stretch: 'normal',
    unicodeRange: 'U+0-10FFFF',
    variant: 'normal',
    featureSettings: 'normal',
    variationSettings: 'normal',
    display: 'auto',
    ascentOverride: 'normal',
    descentOverride: 'normal',
    lineGapOverride: 'normal',
    sizeAdjust: '100%',
    error: null,
  };
  const apply = (key, parse) => {
    const value = descriptors[key];
    if (value === undefined || value === null || value === '') return;
    try {
      state[key] = parse(value);
    } catch (error) {
      if (state.error === null) state.error = error;
    }
  };
  apply('style', __tbFontNormalizeStyle);
  apply('weight', __tbFontNormalizeWeight);
  // https://drafts.csswg.org/css-fonts-4/#font-width-prop
  try {
    const width = descriptors.width;
    const stretch = descriptors.stretch;
    if (width !== undefined && width !== null && width !== '') {
      state.stretch = __tbFontNormalizeStretch(width);
    } else if (stretch !== undefined && stretch !== null && stretch !== '') {
      state.stretch = __tbFontNormalizeStretch(stretch);
    }
  } catch (error) {
    if (state.error === null) state.error = error;
  }
  apply('unicodeRange', __tbFontNormalizeUnicodeRange);
  apply('variant', __tbFontNormalizeVariant);
  apply('featureSettings', __tbFontNormalizeFeatureSettings);
  apply('variationSettings', __tbFontNormalizeVariationSettings);
  apply('display', __tbFontNormalizeDisplay);
  apply('ascentOverride', value => __tbFontNormalizeOverride(value, 'ascent-override'));
  apply('descentOverride', value => __tbFontNormalizeOverride(value, 'descent-override'));
  apply('lineGapOverride', value => __tbFontNormalizeOverride(value, 'line-gap-override'));
  apply('sizeAdjust', __tbFontNormalizeSizeAdjust);
  return state;
}

function __tbFontParseSource(source) {
  if (
    typeof ArrayBuffer === 'function'
    && (source instanceof ArrayBuffer || ArrayBuffer.isView(source))
  ) {
    const view = source instanceof ArrayBuffer
      ? new Uint8Array(source)
      : new Uint8Array(source.buffer, source.byteOffset, source.byteLength);
    return { bytes: view.slice(), sources: [] };
  }
  const text = __tbStringCtor(source);
  const sources = [];
  const pattern = /(local|url)\(\s*(?:"([^"]*)"|'([^']*)'|([^)]*))\s*\)/gi;
  let match;
  while ((match = pattern.exec(text)) !== null) {
    const value = match[2] !== undefined ? match[2] : (match[3] !== undefined ? match[3] : match[4]);
    // https://drafts.csswg.org/css-fonts-4/#src-desc
    sources.push({
      local: match[1].toLowerCase() === 'local',
      value: __tbApply(__tbStringTrim, value, []),
    });
  }
  return { bytes: null, sources };
}

function __tbFontFaceState(face) {
  const state = __tbFontFaceSlots.get(face);
  if (state === undefined) throw new TypeError('Illegal invocation');
  return state;
}

function __tbFontFaceRefresh(face) {
  const state = __tbFontFaceState(face);
  if (!state.attached || state.token === 0) return;
  face._unregister();
  face._register();
}

// https://drafts.csswg.org/css-font-loading/#fontface-interface
class FontFace {
  constructor(family, source, descriptors) {
    if (arguments.length < 2) {
      throw new TypeError('FontFace needs a family and a source');
    }
    const parsed = __tbFontParseSource(source);
    const state = __tbFontDescriptors(descriptors == null ? {} : descriptors);
    state.family = __tbFontSerializeFamily(family);
    state.source = source;
    state.bytes = parsed.bytes;
    state.sources = parsed.sources;
    state.status = 'unloaded';
    state.loadPromise = null;
    state.token = 0;
    state.attached = false;
    state.cssConnected = false;
    state.loadedPromise = new Promise((resolve, reject) => {
      state.resolveLoaded = resolve;
      state.rejectLoaded = reject;
    });
    state.loadedPromise.catch(() => {});
    __tbFontFaceSlots.set(this, state);
  }

  get family() { return __tbFontFaceState(this).family; }
  set family(value) {
    const state = __tbFontFaceState(this);
    state.family = __tbFontSerializeFamily(value);
    __tbFontFaceRefresh(this);
  }

  get status() {
    return __tbFontFaceState(this).status;
  }

  get loaded() { return __tbFontFaceState(this).loadedPromise; }

  get weight() { return __tbFontFaceState(this).weight; }
  set weight(value) {
    const state = __tbFontFaceState(this);
    state.weight = __tbFontNormalizeWeight(value);
    __tbFontFaceRefresh(this);
  }

  get style() { return __tbFontFaceState(this).style; }
  set style(value) {
    const state = __tbFontFaceState(this);
    state.style = __tbFontNormalizeStyle(value);
    __tbFontFaceRefresh(this);
  }

  get stretch() { return __tbFontFaceState(this).stretch; }
  set stretch(value) {
    const state = __tbFontFaceState(this);
    state.stretch = __tbFontNormalizeStretch(value);
    __tbFontFaceRefresh(this);
  }

  get width() { return this.stretch; }
  set width(value) { this.stretch = value; }

  get unicodeRange() { return __tbFontFaceState(this).unicodeRange; }
  set unicodeRange(value) {
    const state = __tbFontFaceState(this);
    state.unicodeRange = __tbFontNormalizeUnicodeRange(value);
  }

  get variant() { return __tbFontFaceState(this).variant; }
  set variant(value) {
    const state = __tbFontFaceState(this);
    state.variant = __tbFontNormalizeVariant(value);
  }

  get featureSettings() { return __tbFontFaceState(this).featureSettings; }
  set featureSettings(value) {
    const state = __tbFontFaceState(this);
    state.featureSettings = __tbFontNormalizeFeatureSettings(value);
  }

  get variationSettings() { return __tbFontFaceState(this).variationSettings; }
  set variationSettings(value) {
    const state = __tbFontFaceState(this);
    state.variationSettings = __tbFontNormalizeVariationSettings(value);
  }

  get display() { return __tbFontFaceState(this).display; }
  set display(value) {
    const state = __tbFontFaceState(this);
    state.display = __tbFontNormalizeDisplay(value);
  }

  get ascentOverride() { return __tbFontFaceState(this).ascentOverride; }
  set ascentOverride(value) {
    const state = __tbFontFaceState(this);
    state.ascentOverride = __tbFontNormalizeOverride(value, 'ascent-override');
  }

  get descentOverride() { return __tbFontFaceState(this).descentOverride; }
  set descentOverride(value) {
    const state = __tbFontFaceState(this);
    state.descentOverride = __tbFontNormalizeOverride(value, 'descent-override');
  }

  get lineGapOverride() { return __tbFontFaceState(this).lineGapOverride; }
  set lineGapOverride(value) {
    const state = __tbFontFaceState(this);
    state.lineGapOverride = __tbFontNormalizeOverride(value, 'line-gap-override');
  }

  get sizeAdjust() { return __tbFontFaceState(this).sizeAdjust; }
  set sizeAdjust(value) {
    const state = __tbFontFaceState(this);
    state.sizeAdjust = __tbFontNormalizeSizeAdjust(value);
  }

  load() {
    const state = __tbFontFaceState(this);
    if (state.loadPromise !== null) return state.loadPromise;
    if (state.status === 'loaded' || state.status === 'error') {
      return state.loadedPromise;
    }
    state.status = 'loading';
    // https://drafts.csswg.org/css-font-loading/#dom-fontfaceset-onloading
    __tbFontSetTrack(__tbFontFaceSet);
    if (state.error !== null) {
      this._fail(state.error);
      state.loadPromise = state.loadedPromise;
      return state.loadPromise;
    }
    if (state.bytes !== null) {
      if (!host.__tbValidateFont(state.bytes)) {
        this._fail(__tbFontError('invalid font data', 'SyntaxError'));
      } else {
        this._complete();
      }
      state.loadPromise = state.loadedPromise;
      return state.loadPromise;
    }
    // https://drafts.csswg.org/css-fonts-4/#src-desc
    let chain = Promise.resolve(false);
    for (let index = 0; index < state.sources.length; index++) {
      const source = state.sources[index];
      chain = chain.then(done => {
        if (done) return true;
        if (source.local) {
          if (!host.__tbFontFamilyAvailable(source.value)) return false;
          this._complete();
          return state.status !== 'error';
        }
        return fetch(source.value)
          .then(response => {
            if (!response.ok) return false;
            return response.arrayBuffer();
          })
          .then(buffer => {
            if (buffer === false) return false;
            const bytes = new Uint8Array(buffer);
            if (!host.__tbValidateFont(bytes)) return false;
            state.bytes = bytes;
            this._complete();
            return state.status !== 'error';
          }, () => false);
      });
    }
    state.loadPromise = chain.then(done => {
      if (!done) {
        throw __tbFontError('font source failed to load', 'NetworkError');
      }
      return this;
    }).catch(error => {
      this._fail(error);
      throw error;
    });
    return state.loadPromise;
  }

  _complete() {
    const state = __tbFontFaceState(this);
    if (state.status === 'error') return;
    if (state.attached && state.bytes !== null && state.token === 0) {
      const token = host.__tbRegisterFont(
        state.family, state.bytes, state.weight, state.style, state.stretch,
      );
      if (token === 0) {
        this._fail(__tbFontError('font registration failed', 'NetworkError'));
        return;
      }
      state.token = token;
    }
    state.status = 'loaded';
    state.resolveLoaded(this);
    __tbFontSetTrack(__tbFontFaceSet);
  }

  _fail(error) {
    const state = __tbFontFaceState(this);
    if (state.status === 'error') return;
    state.status = 'error';
    state.rejectLoaded(error);
    __tbFontSetTrack(__tbFontFaceSet);
  }

  _register() {
    const state = __tbFontFaceState(this);
    if (!state.attached || state.token !== 0 || state.bytes === null) return;
    state.token = host.__tbRegisterFont(
      state.family, state.bytes, state.weight, state.style, state.stretch,
    );
  }

  _unregister() {
    const state = __tbFontFaceState(this);
    if (state.token !== 0) {
      host.__tbUnregisterFont(state.token);
      state.token = 0;
    }
  }

  _attach() {
    const state = __tbFontFaceState(this);
    state.attached = true;
    this.load().then(
      () => this._register(),
      () => {},
    );
  }

  _detach() {
    const state = __tbFontFaceState(this);
    state.attached = false;
    this._unregister();
  }
}

globalThis.FontFace = FontFace;

// https://drafts.csswg.org/css-font-loading/#fontfacesetloadevent
class FontFaceSetLoadEvent extends __tbFontEvent {
  constructor(type, init) {
    if (arguments.length < 1) {
      throw new TypeError("Failed to construct 'FontFaceSetLoadEvent': 1 argument required, but only 0 present.");
    }
    init = init || {};
    const event = __tbConstruct(__tbFontEvent, [type, init], new.target);
    const faces = init.fontfaces === undefined ? [] : __tbArrayFrom(init.fontfaces);
    __tbDefineProperty(event, 'fontfaces', {
      value: faces, writable: false, enumerable: true, configurable: true,
    });
    return event;
  }
}
__tbDefineProperty(FontFaceSetLoadEvent.prototype, Symbol.toStringTag, {
  value: 'FontFaceSetLoadEvent', writable: false, enumerable: false, configurable: true,
});
globalThis.FontFaceSetLoadEvent = FontFaceSetLoadEvent;

function __tbFontFire(set, type, faces) {
  if (set === undefined) return;
  __tbApply(__tbFontDispatch, set, [new FontFaceSetLoadEvent(type, { fontfaces: faces })]);
}

function __tbFontCollectRules(list, output) {
  if (list === undefined || list === null) return;
  const length = list.length;
  for (let index = 0; index < length; index++) {
    const rule = list.item(index);
    if (rule === null || rule === undefined) continue;
    if (rule.type === 5 || (rule.constructor && rule.constructor.name === 'CSSFontFaceRule')) {
      output.push(rule);
      continue;
    }
    try {
      __tbFontCollectRules(rule.cssRules, output);
    } catch (error) {
    }
  }
}

function __tbFontCssFaces() {
  const document = globalThis.document;
  if (document === undefined || document.styleSheets === undefined) return [];
  // https://drafts.csswg.org/css-font-loading/#fontfaceset-iteration
  const generation = host.__tb_sheet_generation();
  if (__tbFontFacesWalk.generation === generation) return __tbFontFacesWalk.faces;
  const output = [];
  const sheets = document.styleSheets;
  for (let index = 0; index < sheets.length; index++) {
    const sheet = sheets[index];
    if (sheet === null) continue;
    try {
      __tbFontCollectRules(sheet.cssRules, output);
    } catch (error) {
    }
  }
  __tbFontWalkDepth += 1;
  let faces;
  try {
    faces = [];
    for (let index = 0; index < output.length; index++) {
      const rule = output[index];
      let face = __tbCssFontFaces.get(rule);
      if (face === undefined) {
        face = __tbFontFromRule(rule);
      }
      faces.push(face);
    }
  } finally {
    __tbFontWalkDepth -= 1;
  }
  __tbFontFacesWalk = { generation, faces };
  return faces;
}

let __tbFontFacesWalk = { generation: -1, faces: [] };
let __tbFontWalkDepth = 0;

function __tbFontFromRule(rule) {
  const style = rule.style;
  const read = name => (style ? __tbStringCtor(style.getPropertyValue(name)) : '');
  const source = read('src');
  const descriptors = {
    style: read('font-style'),
    weight: read('font-weight'),
    width: read('font-width'),
    stretch: read('font-stretch'),
    unicodeRange: read('unicode-range'),
    variant: read('font-variant'),
    featureSettings: read('font-feature-settings'),
    variationSettings: read('font-variation-settings'),
    display: read('font-display'),
    ascentOverride: read('ascent-override'),
    descentOverride: read('descent-override'),
    lineGapOverride: read('line-gap-override'),
    sizeAdjust: read('size-adjust'),
  };
  const face = new FontFace(read('font-family'), source, descriptors);
  const state = __tbFontFaceState(face);
  state.cssConnected = true;
  __tbCssFontFaces.set(rule, face);
  // https://drafts.csswg.org/css-font-loading/#font-face-constructor
  // https://drafts.csswg.org/css-font-loading/#dom-fontface-status
  const address = __tbCssomRuleAddress(rule);
  if (address !== null) {
    const status = host.__tbFontFaceStatus(address.sheetId, address.path);
    if (status === 'loading') {
      state.status = 'loading';
    } else if (status === 'loaded') {
      face._complete();
    } else if (status === 'error') {
      face._fail(__tbFontError('font source failed to load', 'NetworkError'));
    }
  }
  return face;
}

// https://drafts.csswg.org/css-fonts-4/#src-desc
// https://drafts.csswg.org/css-font-loading/#dom-fontface-status
host.__tbFontFaceSettled = function (sheetId, path, loaded) {
  const rule = __tbCssomRuleAt(sheetId, path);
  if (rule === null) return;
  const face = __tbCssFontFaces.get(rule);
  if (face === undefined) return;
  const state = __tbFontFaceState(face);
  if (state.status === 'loaded' || state.status === 'error') return;
  if (loaded) {
    face._complete();
  } else {
    face._fail(__tbFontError('font source failed to load', 'NetworkError'));
  }
  __tbFontSetTrack(__tbFontFaceSet);
};

function __tbFontSetState(set) {
  const state = __tbFontSetSlots.get(set);
  if (state === undefined) throw new TypeError('Illegal invocation');
  return state;
}

function __tbFontSetAll(set) {
  const state = __tbFontSetState(set);
  const css = __tbFontCssFaces();
  const all = [];
  for (let index = 0; index < css.length; index++) all.push(css[index]);
  for (let index = 0; index < state.faces.length; index++) all.push(state.faces[index]);
  return all;
}

// https://drafts.csswg.org/css-font-loading/#font-face-set-ready
// https://drafts.csswg.org/css-font-loading/#dom-fontfaceset-onloading
function __tbFontSetTrack(set) {
  const state = __tbFontSetState(set);
  if (__tbFontWalkDepth > 0) {
    return;
  }
  const all = __tbFontSetAll(set);
  let loading = false;
  for (let index = 0; index < all.length; index++) {
    if (__tbFontFaceState(all[index]).status === 'loading') { loading = true; break; }
  }
  if (!loading) {
    if (!state.pendingReady) return;
    state.pendingReady = false;
    const failed = __tbArray.filter(
      state.loadingFaces, face => __tbFontFaceState(face).status === 'error');
    const done = __tbArray.filter(
      state.loadingFaces, face => __tbFontFaceState(face).status === 'loaded');
    if (failed.length !== 0) {
      __tbFontFire(set, 'loadingerror', failed);
    } else if (done.length !== 0) {
      __tbFontFire(set, 'loadingdone', done);
    }
    state.loadingFaces = [];
    state.resolveReady(set);
    return;
  }
  if (state.pendingReady) return;
  state.pendingReady = true;
  state.loadingFaces = [];
  for (let index = 0; index < all.length; index++) {
    if (__tbFontFaceState(all[index]).status === 'loading') {
      state.loadingFaces.push(all[index]);
    }
  }
  state.ready = new Promise(resolve => { state.resolveReady = resolve; });
  __tbFontFire(set, 'loading', state.loadingFaces.slice());
}

function __tbFontSplitCommas(text) {
  const parts = [];
  let current = '';
  let quote = null;
  for (let index = 0; index < text.length; index++) {
    const ch = text[index];
    if (quote !== null) {
      current += ch;
      if (ch === '\\' && index + 1 < text.length) {
        current += text[++index];
      } else if (ch === quote) {
        quote = null;
      }
    } else if (ch === '"' || ch === "'") {
      quote = ch;
      current += ch;
    } else if (ch === ',') {
      parts.push(current);
      current = '';
    } else {
      current += ch;
    }
  }
  parts.push(current);
  return parts;
}

function __tbFontParseFamilyPart(part) {
  const text = __tbApply(__tbStringTrim, part, []);
  if (text.length === 0) {
    throw __tbFontError('invalid font family', 'SyntaxError');
  }
  if (text[0] === '"' || text[0] === "'") {
    if (text.length < 2 || text[text.length - 1] !== text[0]) {
      throw __tbFontError('invalid font family', 'SyntaxError');
    }
    return {
      name: text.slice(1, -1).replace(/\\(.)/g, '$1'),
      quoted: true,
    };
  }
  const parts = __tbApply(__tbStringSplit, text, [' ']);
  for (let index = 0; index < parts.length; index++) {
    if (!/^-?[A-Za-z_\u0080-\uFFFF][A-Za-z0-9_\-\u0080-\uFFFF]*$/.test(parts[index])) {
      throw __tbFontError('invalid font family', 'SyntaxError');
    }
  }
  return { name: text, quoted: false };
}

// https://drafts.csswg.org/css-fonts-4/#font-prop
function __tbFontFamilies(value) {
  const text = __tbApply(__tbStringTrim, __tbStringCtor(value), []);
  if (text.length === 0) {
    throw __tbFontError('invalid font value', 'SyntaxError');
  }
  if (/var\s*\(/i.test(text)) {
    throw __tbFontError('var() is not allowed in a font value', 'SyntaxError');
  }
  const sizePattern = /^(?:-?\d*\.?\d+(?:px|pt|pc|in|cm|mm|q|em|rem|ex|ch|vw|vh|vmin|vmax|%)|xx-small|x-small|small|medium|large|x-large|xx-large|xxx-large|smaller|larger)(?:\/-?\d*\.?\d+(?:px|pt|pc|in|cm|mm|q|em|rem|ex|ch|vw|vh|vmin|vmax|%)?)?$/i;
  const tokens = [];
  const pattern = /"((?:[^"\\]|\\.)*)"|'((?:[^'\\]|\\.)*)'|\S+/g;
  let match;
  while ((match = pattern.exec(text)) !== null) {
    tokens.push({
      text: match[0],
      quoted: match[1] !== undefined || match[2] !== undefined,
    });
  }
  let firstFamily = 0;
  for (let index = 0; index < tokens.length; index++) {
    if (!tokens[index].quoted && sizePattern.test(tokens[index].text)) {
      firstFamily = index + 1;
      break;
    }
  }
  let familyText = '';
  for (let index = firstFamily; index < tokens.length; index++) {
    familyText += (index === firstFamily ? '' : ' ') + tokens[index].text;
  }
  if (firstFamily === 0) familyText = text;
  const parts = __tbFontSplitCommas(familyText);
  const families = [];
  for (let index = 0; index < parts.length; index++) {
    const family = __tbFontParseFamilyPart(parts[index]);
    if (!family.quoted && __tbFontIsKeyword(family.name, __tbFontCssWide)) {
      throw __tbFontError('CSS-wide keyword is not a font family', 'SyntaxError');
    }
    families.push(family.name);
  }
  return families;
}

function __tbFontMatches(face, families) {
  const name = __tbFontUnquoteFamily(face.family);
  const lower = __tbApply(__tbStringToLowerCase, name, []);
  for (let index = 0; index < families.length; index++) {
    if (__tbApply(__tbStringToLowerCase, families[index], []) === lower) return true;
  }
  return false;
}

// https://drafts.csswg.org/css-font-loading/#fontfaceset
class FontFaceSet extends __tbFontEventTarget {
  constructor(token) {
    if (token !== __tbFontFaceSetToken) {
      throw new TypeError('Illegal constructor');
    }
    super();
    __tbFontSetSlots.set(this, {
      faces: [],
      ready: Promise.resolve(this),
      pendingReady: false,
      resolveReady: null,
      loadingFaces: [],
    });
  }

  get size() {
    return __tbFontSetAll(this).length;
  }

  get status() {
    const all = __tbFontSetAll(this);
    for (let index = 0; index < all.length; index++) {
      if (all[index].status === 'loading') return 'loading';
    }
    return 'loaded';
  }

  get ready() {
    const state = __tbFontSetState(this);
    __tbFontSetTrack(this);
    return state.ready;
  }

  has(face) {
    __tbFontFaceState(face);
    const state = __tbFontSetState(this);
    if (__tbArray.indexOf(state.faces, face) !== -1) return true;
    return __tbArray.indexOf(__tbFontCssFaces(), face) !== -1;
  }

  add(face) {
    const faceState = __tbFontFaceState(face);
    const state = __tbFontSetState(this);
    if (faceState.cssConnected) return this;
    if (__tbArray.indexOf(state.faces, face) !== -1) return this;
    state.faces.push(face);
    face._attach();
    __tbFontSetTrack(this);
    return this;
  }

  delete(face) {
    __tbFontFaceState(face);
    const state = __tbFontSetState(this);
    const index = __tbArray.indexOf(state.faces, face);
    if (index === -1) return false;
    __tbArray.remove(state.faces, index);
    face._detach();
    __tbFontSetTrack(this);
    return true;
  }

  clear() {
    const state = __tbFontSetState(this);
    const faces = state.faces;
    state.faces = [];
    for (let index = 0; index < faces.length; index++) faces[index]._detach();
    __tbFontSetTrack(this);
  }

  forEach(callback, thisArg) {
    if (typeof callback !== 'function') throw new TypeError('callback is not a function');
    const all = __tbFontSetAll(this);
    for (let index = 0; index < all.length; index++) {
      __tbApply(callback, thisArg, [all[index], all[index], this]);
    }
  }

  keys() {
    return this.values();
  }

  values() {
    const all = __tbFontSetAll(this);
    let index = 0;
    return {
      next() {
        if (index >= all.length) return { value: undefined, done: true };
        return { value: all[index++], done: false };
      },
      [__tbIteratorSymbol]() { return this; },
    };
  }

  entries() {
    const all = __tbFontSetAll(this);
    let index = 0;
    return {
      next() {
        if (index >= all.length) return { value: undefined, done: true };
        const face = all[index++];
        return { value: [face, face], done: false };
      },
      [__tbIteratorSymbol]() { return this; },
    };
  }

  [__tbIteratorSymbol]() {
    return this.values();
  }

  check(font, text) {
    let families;
    try {
      families = __tbFontFamilies(font);
    } catch (error) {
      return false;
    }
    const all = __tbFontSetAll(this);
    for (let index = 0; index < families.length; index++) {
      const family = families[index];
      if (__tbFontIsKeyword(family, __tbFontGenerics)) continue;
      let found = false;
      let loaded = true;
      for (let faceIndex = 0; faceIndex < all.length; faceIndex++) {
        if (!__tbFontMatches(all[faceIndex], [family])) continue;
        found = true;
        if (all[faceIndex].status !== 'loaded') loaded = false;
      }
      if (found) {
        if (!loaded) return false;
        continue;
      }
      // https://drafts.csswg.org/css-font-loading/#font-face-set-check
    }
    return true;
  }

  load(font, text) {
    return new Promise((resolve, reject) => {
      let families;
      try {
        families = __tbFontFamilies(font);
      } catch (error) {
        reject(error);
        return;
      }
      const all = __tbFontSetAll(this);
      const matched = [];
      for (let index = 0; index < all.length; index++) {
        if (!__tbFontMatches(all[index], families)) continue;
        if (__tbArray.indexOf(matched, all[index]) === -1) matched.push(all[index]);
      }
      if (matched.length === 0) {
        resolve([]);
        return;
      }
      Promise.all(__tbArray.map(matched, face => face.load())).then(
        () => resolve(matched),
        error => reject(error),
      );
    });
  }
}
__tbDefineProperty(FontFaceSet.prototype, Symbol.toStringTag, {
  value: 'FontFaceSet', writable: false, enumerable: false, configurable: true,
});
globalThis.FontFaceSet = FontFaceSet;

const __tbFontFaceSet = new FontFaceSet(__tbFontFaceSetToken);

function __tbFontSetOnEvent(name) {
  __tbDefineProperty(FontFaceSet.prototype, name, {
    get() {
      const state = __tbFontSetState(this);
      return state[name] || null;
    },
    set(value) {
      const state = __tbFontSetState(this);
      if (value !== null && typeof value !== 'function') {
        throw new TypeError('event handler must be a function');
      }
      const type = name.slice(2);
      if (state[name]) {
        __tbApply(__tbFontRemoveEventListener, this, [type, state[name]]);
      }
      state[name] = value;
      if (value) {
        __tbApply(__tbFontAddEventListener, this, [type, value]);
      }
    },
    enumerable: true,
    configurable: true,
  });
}
__tbFontSetOnEvent('onloading');
__tbFontSetOnEvent('onloadingdone');
__tbFontSetOnEvent('onloadingerror');

if (globalThis.Document !== undefined) {
  Object.defineProperty(globalThis.Document.prototype, 'fonts', {
    configurable: true,
    enumerable: false,
    get() {
      return __tbFontFaceSet;
    },
  });
}
