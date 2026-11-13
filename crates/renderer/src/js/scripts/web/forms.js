// Form-control infrastructure that is pure Web IDL sugar: length reflection
// and the constraint validation API. Host state that touches the tree stays a
// Rust binding (docs/engines.md), so this file only reads
// attributes and the `value` IDL attribute.
(function() {
  // Rules for parsing non-negative integers live in the engine's one
  // microsyntax implementation
  // (<https://infra.spec.whatwg.org/#rules-for-parsing-non-negative-integers>).
  const parseNonNegative = value => host.__tbParseNonNegativeInteger(String(value));

  // ASCII whitespace stripping only: Unicode `trim()` would strip NBSP and
  // friends that the infra grammars must reject
  // (<https://infra.spec.whatwg.org/#ascii-whitespace>).
  const asciiTrim = value => String(value).replace(/^[\t\n\f\r ]+|[\t\n\f\r ]+$/g, '');

  // Web IDL `long` conversion (<https://webidl.spec.whatwg.org/#es-long>).
  const toLong = value => {
    // Explicit `Number()` converts BigInts; WebIDL's `ToNumber` throws.
    if (typeof value === 'bigint') {
      throw new TypeError('Cannot convert a BigInt value to a number');
    }
    let number = Number(value);
    if (!Number.isFinite(number)) number = 0;
    return Math.trunc(number) | 0;
  };

  // The color state's `alpha` and `colorSpace` IDL attributes reflect their
  // content attributes. The comparison for `colorSpace` is ASCII
  // case-insensitive, and the setter stores the string as authored
  // (<https://html.spec.whatwg.org/multipage/input.html#dom-input-colorspace>).
  Object.defineProperties(globalThis.HTMLInputElement.prototype, {
    alpha: {
      configurable: true,
      get() {
        return this.hasAttribute('alpha');
      },
      set(value) {
        if (value) this.setAttribute('alpha', '');
        else this.removeAttribute('alpha');
      },
    },
    colorSpace: {
      configurable: true,
      get() {
        const value = this.getAttribute('colorspace');
        return value !== null && value.toLowerCase() === 'display-p3'
          ? 'display-p3'
          : 'limited-srgb';
      },
      set(value) {
        this.setAttribute('colorspace', String(value));
      },
    },
  });

  // Content attributes whose absence is `-1` and whose setter rejects a
  // negative value (<https://html.spec.whatwg.org/multipage/input.html#dom-input-maxlength>).
  const lengthProperty = name => ({
    get: function() {
      const raw = this.getAttribute(name);
      if (raw === null) return -1;
      const parsed = parseNonNegative(raw);
      return parsed === null ? -1 : parsed;
    },
    set: function(value) {
      const number = toLong(value);
      if (number < 0) {
        throw new DOMException(
          'The length is negative: ' + number, 'IndexSizeError');
      }
      this.setAttribute(name, String(number));
    },
    enumerable: true,
    configurable: true,
  });

  const validityStates = host.slots();

  const VALUE_MODE_TYPES = new Set([
    'text', 'search', 'tel', 'url', 'email', 'password', 'date', 'month',
    'week', 'time', 'datetime-local', 'number',
  ]);
  // The text-selection APIs apply only to these input types (plus textarea);
  // notably email and number are excluded
  // (<https://html.spec.whatwg.org/multipage/input.html#do-not-apply>).
  const SELECTION_TYPES = new Set(['text', 'search', 'tel', 'url', 'password']);
  const BARRED_TYPES = new Set(['hidden', 'button', 'reset', 'image']);

  const isInput = element => element.tagName === 'INPUT';
  const isTextControl = element =>
    element.tagName === 'TEXTAREA' || (isInput(element) && VALUE_MODE_TYPES.has(element.type));

  // <https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#barred-from-constraint-validation>
  // A control is disabled by a disabled fieldset ancestor, unless it is inside
  // the fieldset's first legend
  // (<https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#concept-fe-disabled>).
  const fieldsetDisabled = element => {
    for (let ancestor = element.parentElement; ancestor; ancestor = ancestor.parentElement) {
      if (ancestor.tagName === 'FIELDSET' && ancestor.hasAttribute('disabled')) {
        const legend = Array.from(ancestor.children).find(child => child.tagName === 'LEGEND');
        if (legend === undefined || !legend.contains(element)) return true;
        return false;
      }
    }
    return false;
  };

  const barredFromValidation = element => {
    if (element.disabled || fieldsetDisabled(element)) return true;
    // A control inside a `datalist` is barred
    // (<https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#barred-from-constraint-validation>).
    for (let ancestor = element.parentElement; ancestor; ancestor = ancestor.parentElement) {
      if (ancestor.tagName === 'DATALIST') return true;
    }
    switch (element.tagName) {
      case 'INPUT':
        return BARRED_TYPES.has(element.type) || element.readOnly;
      case 'TEXTAREA':
        return element.readOnly;
      case 'SELECT':
        return false;
      case 'BUTTON':
        return element.type !== 'submit';
      default:
        return true;
    }
  };

  // The scope a radio button group lives in: its form owner, or the root of
  // its tree (which may be detached)
  // (<https://html.spec.whatwg.org/multipage/input.html#radio-button-group>).
  const radioRoot = element => {
    if (element.form) return element.form;
    if (typeof element.getRootNode === 'function') return element.getRootNode();
    return element.ownerDocument;
  };
  const radiosIn = root => {
    if (!root) return [];
    if (typeof root.querySelectorAll === 'function') return Array.from(root.querySelectorAll('input'));
    if (typeof root.getElementsByTagName === 'function') return Array.from(root.getElementsByTagName('input'));
    return [];
  };

  const anyRadioChecked = element => {
    if (element.checked) return true;
    const name = element.name;
    if (!name) return false;
    for (const radio of radiosIn(radioRoot(element))) {
      if (radio !== element && radio.type === 'radio' && radio.name === name
          && radio.form === element.form && radio.checked) {
        return true;
      }
    }
    return false;
  };

  // A radio button group is required when any member has the `required`
  // attribute; every member then suffers from being missing while none is
  // checked
  // (<https://html.spec.whatwg.org/multipage/input.html#radio-button-state-(type=radio):suffering-from-being-missing>).
  const radioGroupRequired = element => {
    if (element.required) return true;
    const name = element.name;
    if (!name) return false;
    for (const radio of radiosIn(radioRoot(element))) {
      if (radio.type === 'radio' && radio.name === name
          && radio.form === element.form && radio.required) return true;
    }
    return false;
  };

  // A control is mutable when it is neither disabled nor readonly
  // (<https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#mutability>).
  const isMutable = element =>
    !element.disabled && !fieldsetDisabled(element) && !element.readOnly;

  // <https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#suffering-from-being-missing>
  const valueMissing = element => {
    // A radio group is missing while no member is checked, for every member
    // of a group that has any required member
    // (<https://html.spec.whatwg.org/multipage/input.html#radio-button-state-(type=radio):suffering-from-being-missing>).
    if (element.tagName === 'INPUT' && element.type === 'radio') {
      // A radio with an empty name is not part of a radio button group, so
      // the radio-group clause does not apply
      // (<https://html.spec.whatwg.org/multipage/input.html#radio-button-state-(type=radio)>).
      if (element.name === '') return false;
      return radioGroupRequired(element) && !anyRadioChecked(element);
    }
    if (!element.required) return false;
    switch (element.tagName) {
      case 'INPUT':
        switch (element.type) {
          // The checkedness and filename clauses do not require a mutable
          // control; only the value-mode clause does.
          case 'checkbox': return !element.checked;
          case 'file': return element.files.length === 0;
          case 'hidden': return false;
          default: return isMutable(element) && isTextControl(element) && element.value === '';
        }
      case 'TEXTAREA':
        return isMutable(element) && element.value === '';
      case 'SELECT': {
        // A required select is missing when no option is selected, or when
        // the first option is its placeholder label option: the first option
        // element child of the select with an empty value. A listbox
        // (`multiple` or `size` greater than one) has no placeholder
        // (<https://html.spec.whatwg.org/multipage/form-elements.html#the-select-element:attr-select-required-4>).
        const options = Array.from(element.options);
        if (element.multiple || element.size > 1) {
          return !options.some(option => option.selected);
        }
        if (options.length === 0) return true;
        const first = options[0];
        // A wrapper element does not stop the first option from being the
        // placeholder; an `optgroup` does
        // (<https://html.spec.whatwg.org/multipage/form-elements.html#placeholder-label-option>).
        const grouped = first.closest('optgroup') !== null;
        return first.selected && !grouped && first.value === '';
      }
      default:
        return false;
    }
  };

  const tooLong = element => {
    if (!isTextControl(element) || !host.__tbUserEdited(element)) return false;
    const maximum = element.maxLength;
    return maximum >= 0 && element.value.length > maximum;
  };

  const tooShort = element => {
    if (!isTextControl(element) || !host.__tbUserEdited(element)) return false;
    const minimum = element.minLength;
    return minimum > 0 && element.value.length !== 0 && element.value.length < minimum;
  };

  const inputType = element => (isInput(element) ? element.type : '');

  // An email/url control whose value does not match its type
  // (<https://html.spec.whatwg.org/multipage/input.html#the-email-state-(type=email)>).
  const typeMismatch = element => {
    const value = element.value;
    if (value === '') return false;
    const type = inputType(element);
    if (type === 'email') {
      const address = /^[a-zA-Z0-9.!#$%&'*+/=?^_`{|}~-]+@[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?(?:\.[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?)*$/;
      // A comma separates addresses only with the `multiple` attribute
      // (<https://html.spec.whatwg.org/multipage/input.html#attr-input-multiple>).
      if (element.hasAttribute('multiple')) {
        return value.split(',').some(part => !address.test(asciiTrim(part)));
      }
      return !address.test(value);
    }
    if (type === 'url') {
      try { new globalThis.URL(value); return false; } catch (error) { return true; }
    }
    return false;
  };

  // QuickJS's `v` implementation rejects a few characters that the spec allows
  // as literals (`/`, and `-` outside a character class); translate them to
  // equivalent escapes so the pattern still compiles
  // (<https://html.spec.whatwg.org/multipage/input.html#the-pattern-attribute>).
  const translatePattern = pattern => {
    let translated = '';
    let depth = 0;
    for (let index = 0; index < pattern.length; index += 1) {
      const character = pattern[index];
      if (character === '\\' && index + 1 < pattern.length) {
        translated += character + pattern[index + 1];
        index += 1;
        continue;
      }
      if (character === '[') depth += 1;
      else if (character === ']' && depth > 0) depth -= 1;
      else if (character === '/') { translated += '\\/'; continue; }
      else if (character === '-' && depth === 0) { translated += '\\u002D'; continue; }
      translated += character;
    }
    return translated;
  };

  // The `pattern` attribute compiled with the `v` flag; a pattern that does
  // not compile is ignored
  // (<https://html.spec.whatwg.org/multipage/input.html#the-pattern-attribute>).
  const compiledPattern = element => {
    const pattern = element.getAttribute('pattern');
    if (pattern === null || pattern === '') return null;
    const translated = translatePattern(pattern);
    try {
      new RegExp(translated, 'v');
    } catch (error) {
      return null;
    }
    return new RegExp(`^(?:${translated})$`, 'v');
  };

  const patternMismatch = element => {
    if (element.value === '') return false;
    const expression = compiledPattern(element);
    if (expression === null) return false;
    // For a multiple email control the pattern applies to each address
    // (<https://html.spec.whatwg.org/multipage/input.html#attr-input-multiple>).
    if (inputType(element) === 'email' && element.hasAttribute('multiple')) {
      return element.value.split(',').some(part => !expression.test(asciiTrim(part)));
    }
    return !expression.test(element.value);
  };

  // The value interpreted in its state's step unit (days, months, weeks, or
  // seconds), or `null` when the string is not valid in that state. The engine
  // owns the one copy of the microsyntax grammar
  // (<https://html.spec.whatwg.org/multipage/input.html#dom-input-valueasnumber>).
  const parseValue = (type, value) =>
    value === null ? null : host.__tbValueInUnits(type, value);
  const valueAsNumber = element => parseValue(inputType(element), element.value);
  const rangeUnderflow = element => {
    const value = valueAsNumber(element);
    const type = inputType(element);
    const min = parseValue(type, element.getAttribute('min'));
    const max = parseValue(type, element.getAttribute('max'));
    if (value === null || min === null) return false;
    // A reversed range (min greater than max) accepts everything outside the
    // open interval (max, min).
    if (max !== null && min > max) return value > max && value < min;
    return value < min;
  };
  const rangeOverflow = element => {
    const value = valueAsNumber(element);
    const type = inputType(element);
    const min = parseValue(type, element.getAttribute('min'));
    const max = parseValue(type, element.getAttribute('max'));
    if (value === null || max === null) return false;
    if (min !== null && min > max) return value > max && value < min;
    return value > max;
  };
  // A `step` attribute by the engine's floating-point grammar: anything
  // unparseable or non-positive falls back to the default step
  // (<https://html.spec.whatwg.org/multipage/input.html#the-step-attribute>).
  const parseStep = (raw, fallback) => {
    if (raw === null) return fallback;
    const parsed = host.__tbParseFloat(asciiTrim(raw));
    return parsed !== null && parsed > 0 ? parsed : fallback;
  };

  const stepMismatch = element => host.__tbStepMismatch(element);

  const FLAGS = {
    valueMissing,
    typeMismatch,
    badInput: element => host.__tbBadInput(element),
    patternMismatch,
    tooLong,
    tooShort,
    rangeUnderflow,
    rangeOverflow,
    stepMismatch,
    customError: element => host.__tbCustomValidity(element) !== '',
  };

  const isValid = element => !Object.values(FLAGS).some(check => check(element));

  class ValidityState {
    constructor() {
      throw new TypeError('Illegal constructor');
    }
  }
  Object.defineProperty(ValidityState.prototype, Symbol.toStringTag, {
    value: 'ValidityState', writable: false, enumerable: false, configurable: true,
  });
  Object.defineProperty(globalThis, 'ValidityState', {
    value: ValidityState, writable: true, enumerable: false, configurable: true,
  });

  const validityOf = element => {
    let state = validityStates.get(element);
    if (state !== undefined) return state;
    const descriptors = {};
    const names = [
      'valueMissing', 'typeMismatch', 'patternMismatch', 'tooLong', 'tooShort',
      'rangeUnderflow', 'rangeOverflow', 'stepMismatch', 'badInput',
      'customError', 'valid',
    ];
    for (const name of names) {
      descriptors[name] = {
        get() {
          if (name === 'valid') return isValid(element);
          const check = FLAGS[name];
          return check === undefined ? false : check(element);
        },
        enumerable: true,
        configurable: true,
      };
    }
    state = Object.create(ValidityState.prototype, descriptors);
    validityStates.set(element, state);
    return state;
  };

  const validationMessageOf = element => {
    // A barred control has no validation message.
    if (barredFromValidation(element)) return '';
    const custom = host.__tbCustomValidity(element);
    if (custom !== '') return custom;
    return isValid(element) ? '' : 'Constraints not satisfied';
  };

  const fireInvalid = element => {
    element.dispatchEvent(new Event('invalid', { bubbles: false, cancelable: true }));
  };

  const validationMember = {
    willValidate: {
      get() { return !barredFromValidation(this); },
      enumerable: true,
      configurable: true,
    },
    validity: {
      get() { return validityOf(this); },
      enumerable: true,
      configurable: true,
    },
    validationMessage: {
      get() { return validationMessageOf(this); },
      enumerable: true,
      configurable: true,
    },
    checkValidity: {
      value: function() {
        if (barredFromValidation(this) || isValid(this)) return true;
        fireInvalid(this);
        return false;
      },
      writable: true,
      enumerable: true,
      configurable: true,
    },
    reportValidity: {
      value: function() {
        if (barredFromValidation(this) || isValid(this)) return true;
        fireInvalid(this);
        return false;
      },
      writable: true,
      enumerable: true,
      configurable: true,
    },
    setCustomValidity: {
      value: function(message) {
        // The message is normalized to LF line breaks by the binding
        // (<https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#dom-cva-setcustomvalidity>).
        host.__tbSetCustomValidity(this, String(message));
      },
      writable: true,
      enumerable: true,
      configurable: true,
    },
  };

  for (const name of [
    'HTMLInputElement', 'HTMLTextAreaElement', 'HTMLSelectElement',
    'HTMLButtonElement', 'HTMLFieldSetElement', 'HTMLOutputElement',
    'HTMLObjectElement',
  ]) {
    const constructor = globalThis[name];
    if (!constructor || !constructor.prototype) continue;
    Object.defineProperties(constructor.prototype, validationMember);
  }

  for (const name of ['HTMLInputElement', 'HTMLTextAreaElement']) {
    const constructor = globalThis[name];
    if (!constructor || !constructor.prototype) continue;
    Object.defineProperties(constructor.prototype, {
      maxLength: lengthProperty('maxlength'),
      minLength: lengthProperty('minlength'),
    });
  }

  // ── Text control selection ─────────────────────────────────────────────

  // Web IDL `unsigned long` conversion.
  const toUnsignedLong = value => {
    // Explicit `Number()` converts BigInts; WebIDL's `ToNumber` throws
    // (and `Number(Symbol)` throws already).
    if (typeof value === 'bigint') {
      throw new TypeError('Cannot convert a BigInt value to a number');
    }
    let number = Number(value);
    if (!Number.isFinite(number)) number = 0;
    number = Math.trunc(number);
    return ((number % 4294967296) + 4294967296) % 4294967296;
  };

  const DIRECTION = value =>
    value === 'forward' || value === 'backward' ? value : 'none';

  const selectionApplies = element =>
    element.tagName === 'TEXTAREA' ||
    (isInput(element) && SELECTION_TYPES.has(element.type));

  // "Set the selection range" algorithm
  // (<https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#set-the-selection-range>).
  // The host owns the stored selection, its clamping, and the `select` task a
  // change queues, so the shim only converts the arguments. `direction` is
  // optional; a missing or unknown value means "none".
  const applySelection = (element, start, end, direction) => {
    host.__tbSetSelectionRange(
      element,
      toUnsignedLong(start),
      toUnsignedLong(end),
      direction === undefined ? undefined : DIRECTION(direction));
  };

  const requireSelection = element => {
    if (!selectionApplies(element)) {
      throw new DOMException(
        'The text selection APIs do not apply to this control', 'InvalidStateError');
    }
  };

  const selectionMethods = {
    setSelectionRange: {
      value: function(start, end, direction) {
        requireSelection(this);
        applySelection(this, start, end, direction);
      },
      writable: true, enumerable: true, configurable: true,
    },
    select: {
      value: function() {
        // A control with no selectable text is a no-op
        // (<https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#dom-textarea/input-select>).
        if (!selectionApplies(this)) return;
        applySelection(this, 0, this.value.length, 'none');
      },
      writable: true, enumerable: true, configurable: true,
    },
    setRangeText: {
      value: function(replacement, start, end, selectionMode) {
        // No overload takes zero arguments, so the Web IDL layer throws before
        // the method body runs
        // (<https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#dom-textarea/input-setrangetext>).
        if (arguments.length === 0) {
          throw new TypeError('setRangeText: at least 1 argument required');
        }
        requireSelection(this);
        replacement = String(replacement);
        let rangeStart;
        let rangeEnd;
        if (arguments.length === 1) {
          rangeStart = this.selectionStart;
          rangeEnd = this.selectionEnd;
        } else {
          rangeStart = toUnsignedLong(start);
          rangeEnd = toUnsignedLong(end);
        }
        if (rangeStart > rangeEnd) {
          throw new DOMException('start is greater than end', 'IndexSizeError');
        }
        const length = this.value.length;
        rangeStart = Math.min(rangeStart, length);
        rangeEnd = Math.min(rangeEnd, length);
        const selectionStart = this.selectionStart;
        const selectionEnd = this.selectionEnd;
        const mode = selectionMode === undefined ? 'preserve' : String(selectionMode);
        this.value =
          this.value.slice(0, rangeStart) + replacement + this.value.slice(rangeEnd);
        const newEnd = rangeStart + replacement.length;
        if (mode === 'select') {
          applySelection(this, rangeStart, newEnd, this.selectionDirection);
        } else if (mode === 'start') {
          applySelection(this, rangeStart, rangeStart, this.selectionDirection);
        } else if (mode === 'end') {
          applySelection(this, newEnd, newEnd, this.selectionDirection);
        } else {
          // "preserve": shift the old selection by the replacement's delta.
          const delta = replacement.length - (rangeEnd - rangeStart);
          let nextStart = selectionStart;
          let nextEnd = selectionEnd;
          if (nextStart > rangeEnd) nextStart += delta;
          else if (nextStart > rangeStart) nextStart = rangeStart;
          if (nextEnd > rangeEnd) nextEnd += delta;
          else if (nextEnd > rangeStart) nextEnd = newEnd;
          applySelection(this, nextStart, nextEnd, this.selectionDirection);
        }
      },
      writable: true, enumerable: true, configurable: true,
    },
  };

  for (const name of ['HTMLInputElement', 'HTMLTextAreaElement']) {
    const constructor = globalThis[name];
    if (!constructor || !constructor.prototype) continue;
    Object.defineProperties(constructor.prototype, selectionMethods);
  }

  // ── textarea sizing reflection ─────────────────────────────────────────

  // `[ReflectPositiveWithFallback]` unsigned long: an absent, invalid, or zero
  // attribute reports the fallback
  // (<https://html.spec.whatwg.org/multipage/form-elements.html#dom-textarea-rows>).
  const positiveFallback = (name, fallback) => ({
    get: function() {
      const raw = this.getAttribute(name);
      if (raw === null) return fallback;
      const parsed = parseNonNegative(raw);
      return parsed === null || parsed === 0 ? fallback : parsed;
    },
    set: function(value) {
      const unsigned = toUnsignedLong(value);
      this.setAttribute(name, String(unsigned === 0 || unsigned > 2147483647 ? fallback : unsigned));
    },
    enumerable: true,
    configurable: true,
  });

  if (globalThis.HTMLTextAreaElement && globalThis.HTMLTextAreaElement.prototype) {
    // `rows`/`cols` are numeric `[Reflect]` the generator does not
    // auto-generate, so this shim owns them until it does. (`wrap` is plain
    // string `[Reflect]` and stays generated in Rust: no shim here.)
    Object.defineProperties(globalThis.HTMLTextAreaElement.prototype, {
      rows: positiveFallback('rows', 2),
      cols: positiveFallback('cols', 20),
    });
  }

  // ── submit events and submission ───────────────────────────────────────

  const SUBMITTER = host.slots('tb-submit-submitter');
  const FORMDATA = host.slots('tb-formdata-event-data');

  // <https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#submitevent>
  function SubmitEvent(type) {
    if (new.target === undefined) {
      throw new TypeError('Class constructor SubmitEvent cannot be invoked without new');
    }
    const init = arguments[1];
    const event = Reflect.construct(globalThis.Event, arguments, new.target);
    SUBMITTER.set(event, (init === undefined || init === null || init.submitter === undefined) ? null : init.submitter);
    return event;
  }
  const submitProto = Object.create(globalThis.Event.prototype);
  Object.defineProperty(submitProto, 'constructor', {
    value: SubmitEvent, writable: true, configurable: true,
  });
  Object.defineProperty(submitProto, 'submitter', {
    get() { return SUBMITTER.get(this); }, enumerable: true, configurable: true,
  });
  Object.defineProperty(submitProto, Symbol.toStringTag, {
    value: 'SubmitEvent', writable: false, enumerable: false, configurable: true,
  });
  Object.defineProperty(SubmitEvent, 'prototype', { value: submitProto, writable: false });
  Object.defineProperty(globalThis, 'SubmitEvent', {
    value: SubmitEvent, writable: true, configurable: true,
  });

  // <https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#formdataevent>
  function FormDataEvent(type) {
    if (new.target === undefined) {
      throw new TypeError('Class constructor FormDataEvent cannot be invoked without new');
    }
    const init = arguments[1];
    if (init === undefined || init === null || init.formData === undefined || init.formData === null) {
      throw new TypeError('FormDataEventInit requires formData');
    }
    const event = Reflect.construct(globalThis.Event, arguments, new.target);
    FORMDATA.set(event, init.formData);
    return event;
  }
  const formDataProto = Object.create(globalThis.Event.prototype);
  Object.defineProperty(formDataProto, 'constructor', {
    value: FormDataEvent, writable: true, configurable: true,
  });
  Object.defineProperty(formDataProto, 'formData', {
    get() { return FORMDATA.get(this); }, enumerable: true, configurable: true,
  });
  Object.defineProperty(formDataProto, Symbol.toStringTag, {
    value: 'FormDataEvent', writable: false, enumerable: false, configurable: true,
  });
  Object.defineProperty(FormDataEvent, 'prototype', { value: formDataProto, writable: false });
  Object.defineProperty(globalThis, 'FormDataEvent', {
    value: FormDataEvent, writable: true, configurable: true,
  });

  // application/x-www-form-urlencoded: LF is normalized to CRLF, a space
  // becomes `+`, and the `!'()~` set is percent-encoded
  // (<https://url.spec.whatwg.org/#concept-urlencoded-serializer>).
  // The submission encoding: the first label in `accept-charset` that resolves
  // to an encoding, else UTF-8
  // (<https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#attr-fs-accept-charset>).
  const charsetLabel = form => {
    for (const label of (form.acceptCharset || '').split(/[\t\n\f\r ]+/)) {
      if (label === '') continue;
      const name = host.__tbEncodingName(label);
      if (name !== null && name !== undefined) return name;
    }
    return 'UTF-8';
  };
  // Text crosses to the engine's one Encoding Standard implementation, which
  // also turns an unrepresentable character into a numeric character reference
  // (<https://encoding.spec.whatwg.org/#encode>).
  const charsetBytes = (text, label) =>
    host.__tbEncodeFormBytes(toWellFormed(String(text)), label);
  // Percent-encode already encoded bytes for application/x-www-form-urlencoded
  // (<https://url.spec.whatwg.org/#concept-urlencoded-byte-serializer>).
  const urlEncodeByte = (text, label) =>
    host.__tbUrlEncodeForm(toWellFormed(String(text)), label);
  // A JS string with a lone surrogate cannot cross to Rust as UTF-8; replace
  // lone surrogates with U+FFFD, the same replacement the Encoding Standard's
  // `encode` applies
  // (<https://encoding.spec.whatwg.org/#encode>).
  const toWellFormed = text => {
    let out = '';
    for (let index = 0; index < text.length; index++) {
      const code = text.charCodeAt(index);
      if (code >= 0xD800 && code <= 0xDBFF) {
        const next = text.charCodeAt(index + 1);
        if (next >= 0xDC00 && next <= 0xDFFF) {
          out += text[index] + text[index + 1];
          index++;
        } else {
          out += '\uFFFD';
        }
      } else if (code >= 0xDC00 && code <= 0xDFFF) {
        out += '\uFFFD';
      } else {
        out += text[index];
      }
    }
    return out;
  };
  // The DOM side stores UTF-8, so a lone surrogate must become U+FFFD
  // before it crosses the JS boundary; the form data set conversion and the
  // attribute setters both apply the same replacement
  // (<https://infra.spec.whatwg.org/#javascript-string-convert>).
  const wellFormedSetter = (prototype, property, nullToEmpty = false) => {
    if (!prototype) return;
    const descriptor = Object.getOwnPropertyDescriptor(prototype, property);
    if (descriptor === undefined || descriptor.set === undefined) return;
    Object.defineProperty(prototype, property, {
      get: descriptor.get,
      set(value) {
        const text = nullToEmpty && value === null ? '' : String(value);
        // The value binding clears the edit state after it stores the string
        // (<https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#suffering-from-being-too-long>).
        descriptor.set.call(this, toWellFormed(text));
      },
      enumerable: descriptor.enumerable,
      configurable: true,
    });
  };
  wellFormedSetter(globalThis.HTMLInputElement && globalThis.HTMLInputElement.prototype, 'name');
  wellFormedSetter(globalThis.HTMLInputElement && globalThis.HTMLInputElement.prototype, 'value', true);
  wellFormedSetter(globalThis.HTMLSelectElement && globalThis.HTMLSelectElement.prototype, 'name');
  wellFormedSetter(globalThis.HTMLSelectElement && globalThis.HTMLSelectElement.prototype, 'value', true);
  wellFormedSetter(globalThis.HTMLTextAreaElement && globalThis.HTMLTextAreaElement.prototype, 'value', true);
  wellFormedSetter(globalThis.HTMLOptionElement && globalThis.HTMLOptionElement.prototype, 'value');
  // The tentative `switch` attribute on a checkbox is a reflected boolean;
  // the stable IDL has no member for it yet
  // (<https://html.spec.whatwg.org/multipage/input.html#attr-input-switch>).
  if (globalThis.HTMLInputElement && globalThis.HTMLInputElement.prototype) {
    Object.defineProperty(globalThis.HTMLInputElement.prototype, 'switch', {
      get() { return this.hasAttribute('switch'); },
      set(value) {
        if (value) this.setAttribute('switch', '');
        else this.removeAttribute('switch');
      },
      enumerable: true,
      configurable: true,
    });
  }
  // A `File`/`Blob` entry serializes as its file name
  // (<https://url.spec.whatwg.org/#concept-urlencoded-serializer>).
  const entryValueString = value =>
    (value !== null && typeof value === 'object' && typeof value.name === 'string')
      ? value.name
      : String(value);
  const urlEncode = (formData, label) => {
    const parts = [];
    for (const entry of formData) {
      const name = String(entry[0]).replace(/\r\n|\r|\n/g, '\r\n');
      const value = entryValueString(entry[1]).replace(/\r\n|\r|\n/g, '\r\n');
      parts.push(`${urlEncodeByte(name, label)}=${urlEncodeByte(value, label)}`);
    }
    return parts.join('&');
  };

  // multipart/form-data with a generated boundary
  // (<https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#multipart-form-data>).
  // The multipart name/filename escape: CR, LF, and `"` become percent
  // escapes (<https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#multipart-form-data>).
  const escapeMultipartName = value =>
    String(value)
      .replace(/\r\n|\r|\n/g, '\r\n')
      .replace(/\r/g, '%0D')
      .replace(/\n/g, '%0A')
      .replace(/"/g, '%22');
  // A filename's newlines are not normalized first, only escaped.
  const escapeMultipartFilename = value =>
    String(value)
      .replace(/\r/g, '%0D')
      .replace(/\n/g, '%0A')
      .replace(/"/g, '%22');
  // Text is encoded once; the navigation body crosses as a `Uint8Array`.
  const encoder = new TextEncoder();
  const concatBytes = chunks => {
    let total = 0;
    for (let index = 0; index < chunks.length; index++) total += chunks[index].length;
    const out = new __tbUint8Array(total);
    let offset = 0;
    for (let index = 0; index < chunks.length; index++) {
      const chunk = chunks[index];
      for (let byte = 0; byte < chunk.length; byte++) out[offset + byte] = chunk[byte];
      offset += chunk.length;
    }
    return out;
  };

  const encodeMultipart = (formData, label) => {
    const boundary = `----tinybrowser${Math.random().toString(16).slice(2)}`;
    const chunks = __tbPrivateArray();
    const push = text => __tbArray.push(chunks, encoder.encode(text));
    for (const entry of formData) {
      const name = escapeMultipartName(entry[0]);
      const value = entry[1];
      push(`--${boundary}\r\n`);
      if (value !== null && typeof value === 'object' && typeof value.name === 'string') {
        push(`Content-Disposition: form-data; name="`);
        __tbArray.push(chunks, charsetBytes(name, label));
        push(`"; filename="`);
        __tbArray.push(chunks, charsetBytes(escapeMultipartFilename(value.name), label));
        push(`"\r\nContent-Type: ${value.type || 'application/octet-stream'}\r\n\r\n`);
        const data = __tbBlobData.get(value);
        __tbArray.push(chunks, data === undefined ? encoder.encode(String(value)) : data.bytes);
        push('\r\n');
      } else {
        push(`Content-Disposition: form-data; name="`);
        __tbArray.push(chunks, charsetBytes(name, label));
        push(`"\r\n\r\n`);
        __tbArray.push(
          chunks,
          charsetBytes(String(value).replace(/\r\n|\r|\n/g, '\r\n'), label),
        );
        push('\r\n');
      }
    }
    push(`--${boundary}--\r\n`);
    return {
      body: concatBytes(chunks),
      contentType: `multipart/form-data; boundary=${boundary}`,
    };
  };

  const encodeTextPlain = (formData, label) => {
    const chunks = __tbPrivateArray();
    for (const entry of formData) {
      const name = charsetBytes(String(entry[0]).replace(/\r\n|\r|\n/g, '\r\n'), label);
      const value = charsetBytes(
        entryValueString(entry[1]).replace(/\r\n|\r|\n/g, '\r\n'),
        label,
      );
      __tbArray.push(chunks, name);
      __tbArray.push(chunks, encoder.encode('='));
      __tbArray.push(chunks, value);
      __tbArray.push(chunks, encoder.encode('\r\n'));
    }
    return concatBytes(chunks);
  };

  // A submit button overrides its form's action/method/enctype/target through
  // its `form*` attributes; the engine resolves all four, including the
  // submitter's state, so the form and the button cannot disagree
  // (<https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#form-submission-algorithm>).
  const submitForm = (form, formData, submitter) => {
    const plan = host.__tbFormPlan(form, submitter === undefined ? null : submitter);
    if (plan === null) return;
    const action = plan[0];
    const method = plan[1];
    const enctype = plan[2];
    const target = plan[3];
    const label = charsetLabel(form);
    // A `_charset_` control carries the submission encoding's name
    // (<https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#attr-fe-name-charset>).
    if (formData.has('_charset_')) formData.set('_charset_', label);
    if (method === 'get') {
      const encoded = urlEncode(formData, label);
      // Mutate the action URL: replace the query component, keeping the
      // fragment; an empty entry list still produces an empty query
      // (<https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#submit-mutate-action>).
      const hash = action.indexOf('#');
      const fragment = hash === -1 ? '' : action.slice(hash);
      const withoutFragment = hash === -1 ? action : action.slice(0, hash);
      const query = withoutFragment.indexOf('?');
      const base = query === -1 ? withoutFragment : withoutFragment.slice(0, query);
      host.__tbFormNavigate(base + '?' + encoded + fragment, target, 'GET', new __tbUint8Array(0), null, form);
      return;
    }
    // POST: the entries become the request body, encoded per `enctype`
    // (<https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#submit-mutate-action>).
    let body;
    let contentType;
    if (enctype === 'multipart/form-data') {
      ({ body, contentType } = encodeMultipart(formData, label));
    } else if (enctype === 'text/plain') {
      body = encodeTextPlain(formData, label);
      contentType = 'text/plain';
    } else {
      body = encoder.encode(urlEncode(formData, label));
      contentType = 'application/x-www-form-urlencoded';
    }
    host.__tbFormNavigate(action, target, 'POST', body, contentType, form);
  };

  // The submission algorithm's entry-list step: the FormData constructor
  // fires `formdata`, whose handler may extend the list, then we submit it.
  // `form.submit()` runs this without the `submit` event; `requestSubmit()`
  // fires that first
  // (<https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#constructing-form-data-set>).
  const runSubmission = (form, submitter) => {
    // The entry list includes the submitter before `formdata` fires
    // (<https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#constructing-form-data-set>).
    const formData = new globalThis.FormData(form, submitter ?? null);
    submitForm(form, formData, submitter);
  };

  // <https://html.spec.whatwg.org/multipage/forms.html#dom-fs-action>
  Object.defineProperty(globalThis.HTMLFormElement.prototype, 'action', {
    get() {
      const attribute = this.getAttribute('action');
      if (attribute === null || attribute === '') return this.ownerDocument.URL;
      return new globalThis.URL(attribute, this.ownerDocument.baseURI).href;
    },
    set(value) { this.setAttribute('action', __tbToDOMString(value)); },
    enumerable: true, configurable: true,
  });

  // <https://html.spec.whatwg.org/multipage/forms.html#dom-form-submit>
  Object.defineProperty(globalThis.HTMLFormElement.prototype, 'submit', {
    value: function() {
      // A form that is not connected (or whose document navigated away) has
      // no browsing context to submit to
      // (<https://html.spec.whatwg.org/multipage/forms.html#dom-form-submit>).
      if (!host.__tbFormCanSubmit(this)) return;
      runSubmission(this);
    },
    writable: true,
    enumerable: true,
    configurable: true,
  });

  // <https://html.spec.whatwg.org/multipage/forms.html#dom-form-checkvalidity>
  const formValidation = form => {
    let valid = true;
    for (const control of listedElements(form)) {
      if (typeof control.checkValidity !== 'function') continue;
      if (!control.checkValidity()) valid = false;
    }
    return valid;
  };
  Object.defineProperties(globalThis.HTMLFormElement.prototype, {
    checkValidity: {
      value: function() { return formValidation(this); },
      writable: true, enumerable: true, configurable: true,
    },
    reportValidity: {
      value: function() { return formValidation(this); },
      writable: true, enumerable: true, configurable: true,
    },
  });

  // The form's "firing submission events" flag: a nested submission attempt
  // while validation or the `submit` event runs returns without firing again
  // (<https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#form-submission-algorithm>).
  const formsFiringSubmission = host.slots('tb-form-firing-submission');

  // The internal submit algorithm. Native activation calls this through
  // `host.__tbRequestSubmit`, so a page that replaces
  // `HTMLFormElement.prototype.requestSubmit` cannot intercept a button click
  // (<https://html.spec.whatwg.org/multipage/input.html#submit-button-state-(type=submit)>).
  const requestSubmitInternal = (form, submitter) => {
    if (!host.__tbFormCanSubmit(form)) return;
    if (formsFiringSubmission.has(form)) return;
    if (submitter !== undefined && submitter !== null) {
      const tag = submitter.tagName;
      const isSubmit = tag === 'BUTTON'
        ? submitter.type === 'submit'
        : tag === 'INPUT' && (submitter.type === 'submit' || submitter.type === 'image');
      if (!isSubmit) {
        throw new TypeError('submitter must be a submit button');
      }
      if (submitter.form !== form) {
        throw new DOMException('submitter must belong to this form', 'NotFoundError');
      }
    }
    // Interactive validation runs before the submit event and can stop the
    // submission; a submitter's `formnovalidate` skips it
    // (<https://html.spec.whatwg.org/multipage/forms.html#interactively-validate-the-constraints>).
    const noValidate = form.noValidate
      || (submitter !== undefined && submitter !== null && submitter.formNoValidate);
    formsFiringSubmission.add(form);
    try {
      if (!noValidate && !form.checkValidity()) return;
      const event = new SubmitEvent('submit', {
        submitter: submitter === undefined ? null : submitter,
        bubbles: true,
        cancelable: true,
      });
      if (!form.dispatchEvent(event)) return;
      runSubmission(form, submitter);
    } finally {
      formsFiringSubmission.delete(form);
    }
  };
  Object.defineProperty(host, '__tbRequestSubmit', {
    value: requestSubmitInternal,
    writable: false, enumerable: false, configurable: false,
  });

  // <https://html.spec.whatwg.org/multipage/forms.html#dom-form-requestsubmit>
  Object.defineProperty(globalThis.HTMLFormElement.prototype, 'requestSubmit', {
    value: function(submitter) { return requestSubmitInternal(this, submitter); },
    writable: true,
    enumerable: true,
    configurable: true,
  });

  // ── select helpers ─────────────────────────────────────────────────────

  // <https://html.spec.whatwg.org/multipage/form-elements.html#dom-option>
  function Option(text, value, defaultSelected, selected) {
    if (new.target === undefined) {
      throw new TypeError('Class constructor Option cannot be invoked without new');
    }
    const option = globalThis.document.createElement('option');
    if (text !== undefined) option.text = String(text);
    if (value !== undefined) option.value = String(value);
    if (defaultSelected !== undefined) option.defaultSelected = Boolean(defaultSelected);
    // The `selected` argument sets selectedness without the dirty flag, so a
    // later `selected` attribute change still updates it
    // (<https://html.spec.whatwg.org/multipage/form-elements.html#dom-option-option>).
    host.__tbSetOptionSelectedness(option, selected !== undefined && Boolean(selected));
    return option;
  }
  Object.defineProperty(Option, 'prototype', {
    value: globalThis.HTMLOptionElement.prototype, writable: false,
  });
  Object.defineProperty(globalThis, 'Option', {
    value: Option, writable: true, configurable: true,
  });

  Object.defineProperties(globalThis.HTMLSelectElement.prototype, {
    showPicker: {
      value: function() { showPicker(this); },
      writable: true, enumerable: true, configurable: true,
    },
    // `[Reflect, ReflectDefault=0] attribute unsigned long size`: numeric
    // `[Reflect]` is not auto-generated, so this shim owns the property
    // until the generator lowers it (like `rows`/`cols` below). The getter
    // parses non-negative integers, defaulting to 0
    // (<https://html.spec.whatwg.org/multipage/form-elements.html#dom-select-size>).
    size: {
      get: function() {
        const raw = this.getAttribute('size');
        if (raw === null) return 0;
        const parsed = parseNonNegative(raw);
        return parsed === null ? 0 : parsed;
      },
      set: function(value) {
        const unsigned = toUnsignedLong(value);
        this.setAttribute('size', String(unsigned > 2147483647 ? 0 : unsigned));
      },
      enumerable: true, configurable: true,
    },
    length: {
      get: function() { return this.options.length; },
      set: function(value) { this.options.length = value; },
      enumerable: true, configurable: true,
    },
    // Both delegate to the options collection
    // (<https://html.spec.whatwg.org/multipage/form-elements.html#dom-select-item>,
    // <https://html.spec.whatwg.org/multipage/form-elements.html#dom-select-nameditem>).
    // `item` takes `unsigned long`, so a negative index wraps out of range and
    // returns null instead of reaching the host's `usize` conversion.
    item: {
      value: function(index) { return this.options.item((+index) >>> 0); },
      writable: true, enumerable: true, configurable: true,
    },
    namedItem: {
      value: function(name) { return this.options.namedItem(name); },
      writable: true, enumerable: true, configurable: true,
    },
    // <https://html.spec.whatwg.org/multipage/form-elements.html#dom-select-add>
    add: {
      value: function(element, before) {
        if (element === undefined) throw new TypeError('add requires an element');
        // Adding a node before itself is a no-op
        // (<https://html.spec.whatwg.org/multipage/form-elements.html#dom-select-add>).
        if (element === before) return;
        if (before === undefined || before === null) {
          this.appendChild(element);
          return;
        }
        if (!before || typeof before !== 'object' || before.nodeType !== 1) {
          const options = this.options;
          const beforeIndex = toLong(before);
          const index = beforeIndex < 0 ? options.length : beforeIndex;
          const reference = options[index];
          if (reference === undefined) this.appendChild(element);
          else reference.parentNode.insertBefore(element, reference);
          return;
        }
        if (before === this || !this.contains(before)) {
          throw new DOMException('reference is not a child of this select', 'NotFoundError');
        }
        before.parentNode.insertBefore(element, before);
      },
      writable: true, enumerable: true, configurable: true,
    },
    remove: {
      value: function(index) {
        if (arguments.length === 0) {
          globalThis.Element.prototype.remove.call(this);
          return;
        }
        const option = this.options[toLong(index)];
        if (option !== undefined) option.remove();
      },
      writable: true, enumerable: true, configurable: true,
    },
  });

  // ── input valueAsNumber / valueAsDate / stepUp / stepDown ──────────────

  const DATE_LIKE = new Set(['date', 'month', 'week', 'time', 'datetime-local']);

  const civilFromDays = days => {
    const z = days + 719468;
    const era = Math.floor((z >= 0 ? z : z - 146096) / 146097);
    const dayOfEra = z - era * 146097;
    const yearOfEra = Math.floor(
      (dayOfEra - Math.floor(dayOfEra / 1460) + Math.floor(dayOfEra / 36524)
        - Math.floor(dayOfEra / 146096)) / 365,
    );
    const year = yearOfEra + era * 400;
    const dayOfYear = dayOfEra - (365 * yearOfEra + Math.floor(yearOfEra / 4) - Math.floor(yearOfEra / 100));
    const mp = Math.floor((5 * dayOfYear + 2) / 153);
    const day = dayOfYear - Math.floor((153 * mp + 2) / 5) + 1;
    const month = mp + (mp < 10 ? 3 : -9);
    return { year: year + (month <= 2 ? 1 : 0), month, day };
  };
  const pad = (number, width) => String(number).padStart(width, '0');
  // Days since 1970-01-01 for a civil date, the inverse of `civilFromDays`
  // (<https://howardhinnant.github.io/date_algorithms.html#days_from_civil>).
  const daysFromCivil = (year, month, day) => {
    const y = year - (month <= 2 ? 1 : 0);
    const era = Math.floor((y >= 0 ? y : y - 399) / 400);
    const yearOfEra = y - era * 400;
    const dayOfYear = Math.floor((153 * (month + (month > 2 ? -3 : 9)) + 2) / 5) + day - 1;
    const dayOfEra = yearOfEra * 365
      + Math.floor(yearOfEra / 4) - Math.floor(yearOfEra / 100) + dayOfYear;
    return era * 146097 + dayOfEra - 719468;
  };
  // Monday is 0 ... Sunday is 6; 1970-01-01 was a Thursday.
  const weekdayFromDays = days => (((days % 7) + 10) % 7);
  // The Monday of the week containing 1970-01-01, three days before day zero,
  // is the week-numbering origin
  // (<https://html.spec.whatwg.org/multipage/common-microsyntaxes.html#week-number-of-the-last-day>).
  const epochMonday = -3;
  // Date objects stop at 275760-09-13; a valueAsNumber outside that range
  // serializes to the empty string
  // (<https://html.spec.whatwg.org/multipage/input.html#dom-input-valueasnumber>).
  const representableYear = year => year >= 1 && year <= 275760;
  const formatDate = days => {
    const { year, month, day } = civilFromDays(days);
    return representableYear(year)
      ? `${pad(year, 4)}-${pad(month, 2)}-${pad(day, 2)}`
      : '';
  };
  const formatMonth = index => {
    const months = index + 1970 * 12;
    const year = Math.floor(months / 12);
    return representableYear(year)
      ? `${pad(year, 4)}-${pad((months % 12) + 1, 2)}`
      : '';
  };
  const formatWeek = weeks => {
    const days = epochMonday + weeks * 7;
    const { year } = civilFromDays(days + 3);
    if (!representableYear(year)) return '';
    const januaryFourth = daysFromCivil(year, 1, 4);
    const mondayOfWeekOne = januaryFourth - weekdayFromDays(januaryFourth);
    return `${pad(year, 4)}-W${pad(Math.round((days - mondayOfWeekOne) / 7) + 1, 2)}`;
  };
  const formatTime = seconds => {
    const whole = Math.floor(seconds);
    const millis = Math.round((seconds - whole) * 1000);
    const base = `${pad(Math.floor(whole / 3600), 2)}:${pad(Math.floor((whole % 3600) / 60), 2)}`;
    if (millis === 0 && whole % 60 === 0) return base;
    const withSeconds = `${base}:${pad(whole % 60, 2)}`;
    return millis === 0 ? withSeconds : `${withSeconds}.${pad(millis, 3).replace(/0+$/, '')}`;
  };

  // The input's value as a number in each state's scale, or NaN
  // (<https://html.spec.whatwg.org/multipage/input.html#dom-input-valueasnumber>).
  // The engine owns the single grammar and the per-state unit.
  const valueAsNumberOf = element => {
    const value = host.__tbValueAsNumber(inputType(element), element.value);
    return value === null ? NaN : value;
  };

  const whole = value => Math.abs(value - Math.round(value)) < 1e-9;
  const formatValueAsNumber = (type, number) => {
    switch (type) {
      case 'number': case 'range': return String(number);
      case 'date': return whole(number / 86400000) ? formatDate(Math.round(number / 86400000)) : '';
      case 'month': return whole(number) ? formatMonth(number) : '';
      case 'week': {
        if (!whole(number / 86400000)) return '';
        return formatWeek((Math.round(number / 86400000) - epochMonday) / 7);
      }
      case 'time': {
        const normalized = ((number % 86400000) + 86400000) % 86400000;
        return formatTime(normalized / 1000);
      }
      case 'datetime-local': {
        const days = Math.floor(number / 86400000);
        return `${formatDate(days)}T${formatTime((number - days * 86400000) / 1000)}`;
      }
      default: return '';
    }
  };

  const setValueAsNumber = (element, value) => {
    const number = Number(value);
    // An infinite value throws before the applicability check
    // (<https://html.spec.whatwg.org/multipage/input.html#dom-input-valueasnumber>).
    if (number === Infinity || number === -Infinity) {
      throw new TypeError('valueAsNumber must be finite');
    }
    const type = inputType(element);
    if (type !== 'number' && type !== 'range' && !DATE_LIKE.has(type)) {
      throw new DOMException('valueAsNumber is not applicable', 'InvalidStateError');
    }
    element.value = Number.isNaN(number) ? '' : formatValueAsNumber(type, number);
  };

  // `valueAsDate` applies to date, month, week, and time
  // (<https://html.spec.whatwg.org/multipage/input.html#dom-input-valueasdate>).
  const DATE_VALUE_TYPES = new Set(['date', 'month', 'week', 'time']);
  const valueAsDateOf = element => {
    const type = inputType(element);
    if (!DATE_VALUE_TYPES.has(type) || element.value === '') return null;
    const units = host.__tbValueInUnits(type, element.value);
    if (units === null) return null;
    let milliseconds = NaN;
    if (type === 'date') {
      milliseconds = units * 86400000;
    } else if (type === 'month') {
      const months = units + 1970 * 12;
      milliseconds = daysFromCivil(Math.floor(months / 12), (months % 12) + 1, 1) * 86400000;
    } else if (type === 'week') {
      milliseconds = (epochMonday + units * 7) * 86400000;
    } else {
      milliseconds = units * 1000;
    }
    return Number.isNaN(milliseconds) ? null : new Date(milliseconds);
  };
  const setValueAsDate = (element, value) => {
    const type = inputType(element);
    if (!DATE_VALUE_TYPES.has(type)) {
      throw new DOMException('valueAsDate is not applicable', 'InvalidStateError');
    }
    if (value === null) { element.value = ''; return; }
    if (!(value instanceof globalThis.Date)) {
      throw new TypeError('valueAsDate requires a Date');
    }
    const time = value.getTime();
    if (Number.isNaN(time)) { element.value = ''; return; }
    if (type === 'date') {
      element.value = whole(time / 86400000) ? formatDate(Math.round(time / 86400000)) : '';
    } else if (type === 'month') {
      const { year, month } = civilFromDays(Math.floor(time / 86400000));
      element.value = formatMonth((year - 1970) * 12 + month - 1);
    } else if (type === 'week') {
      element.value = formatWeek((Math.floor(time / 86400000) - epochMonday) / 7);
    } else {
      const normalized = ((time % 86400000) + 86400000) % 86400000;
      element.value = formatTime(normalized / 1000);
    }
  };

  // The value serialized in each state's `parseValue` unit, for `stepUp`.
  const formatValueInUnit = (type, value) => {
    switch (type) {
      case 'number': case 'range': return String(value);
      case 'date': return formatDate(value);
      case 'month': return formatMonth(value);
      case 'week': return formatWeek(value);
      case 'time': return formatTime(value);
      case 'datetime-local': {
        const days = Math.floor(value / 86400);
        return `${formatDate(days)}T${formatTime(value - days * 86400)}`;
      }
      default: return '';
    }
  };

  // Moves the value by `count` steps
  // (<https://html.spec.whatwg.org/multipage/input.html#dom-input-stepup>).
  const stepBy = (element, count, direction) => {
    const type = inputType(element);
    // The engine owns the per-state default-step table, so `stepUp` and the
    // validity flag cannot disagree about which states step or by how much
    // (<https://html.spec.whatwg.org/multipage/input.html#the-step-attribute>).
    const fallback = host.__tbDefaultStep(type);
    if (fallback === null) {
      throw new DOMException('stepUp is not applicable', 'InvalidStateError');
    }
    const raw = element.getAttribute('step');
    // The step attribute's `any` keyword is an ASCII case-insensitive match on
    // the attribute value, not a trimmed token
    // (<https://html.spec.whatwg.org/multipage/input.html#the-step-attribute>).
    if (raw !== null && __tbAsciiLower(raw) === 'any') {
      throw new DOMException('step is any', 'InvalidStateError');
    }
    const step = parseStep(raw, fallback);
    const min = parseValue(type, element.getAttribute('min'));
    const max = parseValue(type, element.getAttribute('max'));
    if (min !== null && max !== null && min > max) return;
    // The step base is the min attribute, else the value attribute, else 0.
    let base = min;
    if (base === null) base = parseValue(type, element.getAttribute('value'));
    if (base === null) base = 0;
    const alignUp = value => base + Math.ceil((value - base) / step - 1e-9) * step;
    const alignDown = value => base + Math.floor((value - base) / step + 1e-9) * step;
    // Nothing in [min, max] is representable: nothing to do.
    if (min !== null && max !== null && alignUp(min) > max + 1e-9) return;
    const parsed = parseValue(type, element.value);
    const wasEmpty = parsed === null;
    let value = parsed === null ? 0 : parsed;
    const valueBefore = value;
    const quotient = (value - base) / step;
    if (Math.abs(quotient - Math.round(quotient)) > 1e-9) {
      // Snap to the nearest representable value in the stepping direction.
      value = direction > 0 ? alignUp(value) : alignDown(value);
    } else {
      value += direction * step * count;
    }
    if (min !== null && value < min) value = alignUp(min);
    if (max !== null && value > max) value = alignDown(max);
    if (!wasEmpty) {
      if (direction < 0 && value > valueBefore) return;
      if (direction > 0 && value < valueBefore) return;
    }
    if (type === 'number' || type === 'range') {
      // Round binary floating-point noise back to the step's decimal scale,
      // so 1.2 - 0.3 serializes as 0.9
      // (<https://html.spec.whatwg.org/multipage/input.html#dom-input-stepdown>).
      const text = raw === null ? String(fallback) : asciiTrim(raw);
      const scale = host.__tbDecimalScale(text);
      if (scale === null || scale <= 20) value = Number(value.toFixed(scale ?? 0));
    }
    element.value = formatValueInUnit(type, value);
  };

  // Autofill field names
  // (<https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#autofill-field>).
  const autofillFields = new Set([
    'name', 'honorific-prefix', 'given-name', 'additional-name', 'family-name',
    'honorific-suffix', 'nickname', 'username', 'new-password',
    'current-password', 'one-time-code', 'organization-title', 'organization',
    'street-address', 'address-line1', 'address-line2', 'address-line3',
    'address-level4', 'address-level3', 'address-level2', 'address-level1',
    'country', 'country-name', 'postal-code', 'cc-name', 'cc-given-name',
    'cc-additional-name', 'cc-family-name', 'cc-number', 'cc-exp',
    'cc-exp-month', 'cc-exp-year', 'cc-csc', 'cc-type',
    'transaction-currency', 'transaction-amount', 'language', 'bday',
    'bday-day', 'bday-month', 'bday-year', 'sex', 'url', 'photo', 'tel',
    'tel-country-code', 'tel-national', 'tel-area-code', 'tel-local',
    'tel-local-prefix', 'tel-local-suffix', 'tel-extension', 'email', 'impp',
    'webauthn',
  ]);
  const contactTokens = new Set(['home', 'work', 'mobile', 'fax', 'pager']);
  const autocompleteValue = element => {
    const raw = element.getAttribute('autocomplete');
    if (element.localName === 'input' && element.type === 'hidden') return '';
    if (element.localName === 'form') {
      if (raw === null) return 'on';
      const token = raw.trim().toLowerCase();
      return token === 'off' ? 'off' : 'on';
    }
    if (raw === null) return '';
    const tokens = raw.toLowerCase().split(/[\t\n\f\r ]+/).filter(Boolean);
    if (tokens.length === 0) return '';
    if (tokens.length === 1) {
      return tokens[0] === 'on' || tokens[0] === 'off' || autofillFields.has(tokens[0])
        ? tokens[0]
        : '';
    }
    let index = 0;
    const result = [];
    if (tokens[index]?.startsWith('section-')) {
      if (tokens[index].length === 'section-'.length) return '';
      result.push(tokens[index]);
      index += 1;
    }
    if (tokens[index] === 'shipping' || tokens[index] === 'billing') {
      result.push(tokens[index]);
      index += 1;
    }
    if (contactTokens.has(tokens[index])) {
      result.push(tokens[index]);
      index += 1;
    }
    if (autofillFields.has(tokens[index])) {
      result.push(tokens[index]);
      index += 1;
    }
    if (tokens[index] === 'webauthn') {
      result.push(tokens[index]);
      index += 1;
    }
    return index === tokens.length && result.length > 0 ? result.join(' ') : '';
  };
  for (const name of ['HTMLFormElement', 'HTMLInputElement', 'HTMLSelectElement', 'HTMLTextAreaElement']) {
    const constructor = globalThis[name];
    if (!constructor || !constructor.prototype) continue;
    Object.defineProperty(constructor.prototype, 'autocomplete', {
      get() { return autocompleteValue(this); },
      set(value) { this.setAttribute('autocomplete', __tbToDOMString(value)); },
      enumerable: true, configurable: true,
    });
  }

  // A control is mutable unless disabled; an input with `readonly` is also
  // immutable in the states where the attribute applies
  // (<https://html.spec.whatwg.org/multipage/input.html#the-readonly-attribute>).
  const readonlyApplies = new Set([
    'text', 'search', 'tel', 'url', 'email', 'password', 'date', 'month',
    'week', 'time', 'datetime-local', 'number',
  ]);
  const controlIsMutable = element => {
    if (element.disabled) return false;
    if (element.localName === 'input' && readonlyApplies.has(element.type)) {
      return !element.readOnly;
    }
    return true;
  };
  // The rendered check only guards `select.showPicker()`. A `display: none`
  // box or a `content-visibility: hidden` ancestor means no box is rendered
  // (<https://drafts.csswg.org/css-display-4/#becoming-visible>).
  const isBeingRendered = element => {
    if (!element.isConnected) return false;
    for (let node = element; node !== null; node = node.parentElement) {
      const style = globalThis.getComputedStyle(node);
      if (style.display === 'none' || style.contentVisibility === 'hidden') {
        return false;
      }
    }
    return true;
  };
  // <https://html.spec.whatwg.org/multipage/form-elements.html#dom-select-showpicker>
  const showPicker = element => {
    if (!host.__tbSameOriginWithTop()) {
      throw new DOMException(
        'showPicker() requires same-origin with the top-level window', 'SecurityError');
    }
    if (!controlIsMutable(element)) {
      throw new DOMException('The element is not mutable', 'InvalidStateError');
    }
    if (!host.__tbUserActivationActive()) {
      throw new DOMException('A user gesture is required', 'NotAllowedError');
    }
    if (element.localName === 'select' && !isBeingRendered(element)) {
      throw new DOMException('The element is not being rendered', 'NotSupportedError');
    }
    // Showing the picker is optional, but the activation is consumed whether
    // or not this engine draws one
    // (<https://html.spec.whatwg.org/multipage/form-elements.html#show-the-picker,-if-applicable>).
    host.__tbConsumeUserActivation();
  };
  Object.defineProperties(globalThis.HTMLInputElement.prototype, {
    showPicker: {
      value: function() { showPicker(this); },
      writable: true, enumerable: true, configurable: true,
    },
    valueAsNumber: {
      get() { return valueAsNumberOf(this); },
      set(value) { setValueAsNumber(this, value); },
      configurable: true,
    },
    valueAsDate: {
      get() { return valueAsDateOf(this); },
      set(value) { setValueAsDate(this, value); },
      configurable: true,
    },
    stepUp: {
      value: function(count) { stepBy(this, count === undefined ? 1 : toLong(count), 1); },
      writable: true, enumerable: true, configurable: true,
    },
    stepDown: {
      value: function(count) { stepBy(this, count === undefined ? 1 : toLong(count), -1); },
      writable: true, enumerable: true, configurable: true,
    },
  });

  // The `list` attribute's datalist, or null
  // (<https://html.spec.whatwg.org/multipage/input.html#dom-input-list>).
  Object.defineProperty(globalThis.HTMLInputElement.prototype, 'list', {
    get() {
      const id = this.getAttribute('list');
      if (id === null) return null;
      const element = this.ownerDocument.getElementById(id);
      return element !== null && element.tagName === 'DATALIST' ? element : null;
    },
    configurable: true,
  });

  // `output.value` follows its text content, with a value-mode flag that
  // freezes `defaultValue` once the value is set directly; resetting restores
  // the frozen default. The flag and the frozen default live in the engine's
  // control state
  // (<https://html.spec.whatwg.org/multipage/form-elements.html#dom-output-defaultvalue>).
  if (globalThis.HTMLOutputElement) {
    Object.defineProperties(globalThis.HTMLOutputElement.prototype, {
      type: {
        get() { return 'output'; },
        configurable: true,
      },
      defaultValue: {
        get() {
          if (!host.__tbOutputValueMode(this)) return this.textContent;
          const frozen = host.__tbOutputDefaultValue(this);
          return frozen === null ? this.textContent : frozen;
        },
        set(value) {
          const text = __tbToDOMString(value);
          // While the value mode flag is clear, the default is the text, so
          // the setter writes the text; a frozen override exists only in value
          // mode
          // (<https://html.spec.whatwg.org/multipage/form-elements.html#dom-output-defaultvalue>).
          if (host.__tbOutputValueMode(this)) host.__tbSetOutputDefaultValue(this, text);
          else this.textContent = text;
        },
        configurable: true,
      },
      value: {
        get() { return this.textContent; },
        set(value) {
          const text = __tbToDOMString(value);
          if (!host.__tbOutputValueMode(this)) {
            host.__tbSetOutputDefaultValue(this, this.textContent);
            host.__tbSetOutputValueMode(this, true);
          }
          this.textContent = text;
        },
        configurable: true,
      },
    });
  }
  const nativeFormReset = globalThis.HTMLFormElement.prototype.reset;
  // The engine's reset algorithm, captured before page scripts can replace
  // the prototype method. Native activation calls it through
  // `host.__tbResetForm`, so a page that replaces
  // `HTMLFormElement.prototype.reset` cannot intercept a reset button
  // (<https://html.spec.whatwg.org/multipage/input.html#reset-button-state-(type=reset)>).
  Object.defineProperty(host, '__tbResetForm', {
    value: form => nativeFormReset.call(form),
    writable: false, enumerable: false, configurable: false,
  });

  // <https://html.spec.whatwg.org/multipage/form-elements.html#dom-progress-position>
  if (globalThis.HTMLProgressElement) {
    Object.defineProperty(globalThis.HTMLProgressElement.prototype, 'position', {
      get() {
        if (!this.hasAttribute('value')) return -1;
        const value = host.__tbParseFloat(asciiTrim(this.getAttribute('value')));
        const maximum = host.__tbParseFloat(asciiTrim(this.getAttribute('max')));
        if (value === null) return 0;
        const max = maximum === null || maximum <= 0 ? 1 : maximum;
        return Math.min(Math.max(value / max, 0), 1);
      },
      configurable: true,
    });
  }

  // <https://html.spec.whatwg.org/multipage/form-elements.html#dom-legend-form>
  if (globalThis.HTMLLegendElement) {
    Object.defineProperty(globalThis.HTMLLegendElement.prototype, 'form', {
      get() {
        const parent = this.parentElement;
        return parent !== null && parent.tagName === 'FIELDSET' ? parent.form : null;
      },
      configurable: true,
    });
  }

  // <https://html.spec.whatwg.org/multipage/form-elements.html#dom-fieldset-type>
  const fieldSetElements = new WeakMap();
  if (globalThis.HTMLFieldSetElement) {
    Object.defineProperties(globalThis.HTMLFieldSetElement.prototype, {
      type: {
        get() { return 'fieldset'; },
        configurable: true,
      },
      elements: {
        get() {
          let collection = fieldSetElements.get(this);
          if (collection === undefined) {
            collection = collectionOf(
              () => Array.from(this.querySelectorAll(LISTED))
                .filter(element => !(element.tagName === 'INPUT' && element.type === 'image')),
              HTMLCollectionInterface.prototype, false,
            );
            fieldSetElements.set(this, collection);
          }
          return collection;
        },
        configurable: true,
      },
    });
  }

  // ── labels and labelable elements ──────────────────────────────────────

  const LABELABLE = new Set(['BUTTON', 'METER', 'OUTPUT', 'PROGRESS', 'SELECT', 'TEXTAREA']);
  const isLabelable = element => {
    const tag = element.tagName;
    if (tag === 'INPUT') return element.type !== 'hidden';
    return LABELABLE.has(tag);
  };

  // `label.htmlFor` reflects the `for` attribute
  // (<https://html.spec.whatwg.org/multipage/forms.html#dom-label-htmlfor>).
  Object.defineProperty(globalThis.HTMLLabelElement.prototype, 'htmlFor', {
    get() { return this.getAttribute('for') ?? ''; },
    set(value) { this.setAttribute('for', __tbToDOMString(value)); },
    enumerable: true, configurable: true,
  });

  // `label.control`: the element named by `for`, or the first labelable
  // descendant
  // (<https://html.spec.whatwg.org/multipage/forms.html#dom-label-control>).
  Object.defineProperty(globalThis.HTMLLabelElement.prototype, 'control', {
    get() {
      if (this.hasAttribute('for')) {
        const reference = this.htmlFor;
        if (reference === '') return null;
        const root = this.getRootNode ? this.getRootNode() : this.ownerDocument;
        let element = root.getElementById ? root.getElementById(reference) : null;
        if (element === null && typeof root.querySelectorAll === 'function') {
          element = Array.from(root.querySelectorAll('[id]'))
            .find(candidate => candidate.id === reference) ?? null;
        }
        return element !== null && isLabelable(element) ? element : null;
      }
      for (const descendant of this.querySelectorAll('button, input, meter, output, progress, select, textarea')) {
        if (isLabelable(descendant)) return descendant;
      }
      return null;
    },
    configurable: true,
  });

  // A label's `form` is its labeled control's form owner
  // (<https://html.spec.whatwg.org/multipage/forms.html#dom-label-form>).
  Object.defineProperty(globalThis.HTMLLabelElement.prototype, 'form', {
    get() {
      const control = this.control;
      return control ? control.form : null;
    },
    configurable: true,
  });

  const labelsOf = element => {
    if (!isLabelable(element)) return null;
    return collectionOf(() => {
      const root = element.getRootNode ? element.getRootNode() : element.ownerDocument;
      const labels = Array.from(root.querySelectorAll('label'));
      if (root.nodeType === 1 && root.tagName === 'LABEL') labels.unshift(root);
      return labels.filter(label => label.control === element);
    }, collectionInterface('NodeList').prototype, false);
  };
  for (const name of [
    'HTMLButtonElement', 'HTMLInputElement', 'HTMLMeterElement', 'HTMLOutputElement',
    'HTMLProgressElement', 'HTMLSelectElement', 'HTMLTextAreaElement',
  ]) {
    const constructor = globalThis[name];
    if (!constructor || !constructor.prototype) continue;
    Object.defineProperty(constructor.prototype, 'labels', {
      get() { return labelsOf(this); },
      configurable: true,
    });
  }

  // ── form controls collection ───────────────────────────────────────────

  const LISTED = 'button, fieldset, input, object, output, select, textarea';

  // The form's listed elements, in tree order: every matching control whose
  // form owner is the form
  // (<https://html.spec.whatwg.org/multipage/forms.html#category-listed>).
  const listedElements = form =>
    Array.from(form.ownerDocument.querySelectorAll(LISTED))
      .filter(element => element.form === form)
      // An image button is excluded from the controls collection for historical
      // reasons (<https://html.spec.whatwg.org/multipage/forms.html#dom-form-elements>).
      .filter(element => !(element.tagName === 'INPUT' && element.type === 'image'));

  // Collection interfaces. The engine may already publish `HTMLCollection`;
  // reuse it so both brands share one prototype chain
  // (<https://html.spec.whatwg.org/multipage/forms.html#htmlformcontrolscollection>).
  const collectionInterface = name => {
    const existing = globalThis[name];
    if (existing !== undefined && existing !== null) return existing;
    const constructor = function() { throw new TypeError('Illegal constructor'); };
    Object.defineProperty(constructor.prototype, Symbol.toStringTag, {
      value: name, configurable: true,
    });
    Object.defineProperty(globalThis, name, {
      value: constructor, writable: true, configurable: true,
    });
    return constructor;
  };
  const HTMLCollectionInterface = collectionInterface('HTMLCollection');
  const FormControlsInterface = collectionInterface('HTMLFormControlsCollection');
  const RadioNodeListInterface = collectionInterface('RadioNodeList');
  Object.setPrototypeOf(FormControlsInterface.prototype, HTMLCollectionInterface.prototype);
  // `RadioNodeList` extends `NodeList`, not `HTMLCollection`
  // (<https://html.spec.whatwg.org/multipage/common-dom-interfaces.html#radionodelist>).
  Object.setPrototypeOf(
    RadioNodeListInterface.prototype,
    globalThis.NodeList.prototype,
  );

  // A live, named, indexed collection. `getList` recomputes on each access so
  // the collection tracks DOM changes; the proxy's prototype reports the right
  // interface for `instanceof`.
  const collectionOf = (getList, prototype, radioList) =>
    new Proxy({}, {
      get(target, property, receiver) {
        const list = getList();
        if (property === 'length') return list.length;
        if (property === 'item') return index => list[Number(index)] ?? null;
        if (property === 'namedItem') return name => namedItemIn(getList(), String(name), getList);
        if (property === 'value' && radioList) {
          const checked = list.find(element => element.type === 'radio' && element.checked);
          return checked ? checked.value : '';
        }
        if (property === Symbol.iterator) return function* () { yield* list; };
        if (typeof property === 'string') {
          if (/^\d+$/.test(property)) return list[Number(property)];
          const named = namedItemIn(list, property, getList);
          return named === null ? Reflect.get(prototype, property, receiver) : named;
        }
        return undefined;
      },
      getOwnPropertyDescriptor(target, property) {
        const list = getList();
        if (property === 'length') {
          return {
            value: list.length, writable: true, enumerable: false, configurable: true,
          };
        }
        if (typeof property === 'string' && /^\d+$/.test(property)) {
          const index = Number(property);
          if (index < list.length) {
            return {
              value: list[index], writable: false, enumerable: true, configurable: true,
            };
          }
        }
        return Reflect.getOwnPropertyDescriptor(target, property);
      },
      getPrototypeOf() { return prototype; },
      has(_target, property) {
        const list = getList();
        if (property === 'length' || property === 'item' || property === 'namedItem') return true;
        if (typeof property === 'string' && /^\d+$/.test(property)) return list[Number(property)] !== undefined;
        return namedItemIn(list, String(property), getList) !== null;
      },
    });

  const namedItemIn = (list, name, liveList) => {
    const matches = () => (liveList ? liveList() : list).filter(
      element => element.id === name || element.getAttribute('name') === name,
    );
    const first = matches();
    if (first.length === 0) return null;
    if (first.length === 1) return first[0];
    return collectionOf(matches, RadioNodeListInterface.prototype, true);
  };

  // `document.forms`: every form in tree order, with named access by `name` or
  // `id` (<https://html.spec.whatwg.org/multipage/obsolete.html#dom-document-forms>).
  const formsCache = host.slots();
  Object.defineProperty(globalThis.Document.prototype, 'forms', {
    get() {
      let forms = formsCache.get(this);
      if (forms === undefined) {
        forms = collectionOf(
          () => Array.from(this.querySelectorAll('form')),
          HTMLCollectionInterface.prototype,
          false,
        );
        formsCache.set(this, forms);
      }
      return forms;
    },
    configurable: true,
  });

  const elementsCache = host.slots();
  Object.defineProperties(globalThis.HTMLFormElement.prototype, {
    elements: {
      get() {
        let collection = elementsCache.get(this);
        if (collection === undefined) {
          collection = collectionOf(
            () => listedElements(this), FormControlsInterface.prototype, false,
          );
          elementsCache.set(this, collection);
        }
        return collection;
      },
      configurable: true,
    },
    length: {
      get() { return listedElements(this).length; },
      configurable: true,
    },
  });

})();
