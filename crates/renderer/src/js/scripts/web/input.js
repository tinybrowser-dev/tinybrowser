// https://w3c.github.io/webdriver/#perform-actions
(function() {
  const { Event, MouseEvent, PointerEvent, KeyboardEvent, InputEvent, WheelEvent } = globalThis;
  const addEventListener = Node.prototype.addEventListener;
  const changeState = host.slots('input change state');
  const pointerStates = new __tbPrivateMap();
  const pointerState = (id, pointerType) => {
    if (!pointerStates.has(id)) {
      pointerStates.set(id, { x: 0, y: 0, buttons: 0, target: null, pointerType: pointerType || 'mouse' });
    }
    return pointerStates.get(id);
  };
  const at = (x, y) => {
    let found = document.elementFromPoint ? document.elementFromPoint(x, y) : null;
    while (found && found.shadowRoot
        && typeof found.shadowRoot.elementFromPoint === 'function') {
      const inner = found.shadowRoot.elementFromPoint(x, y);
      if (!inner || inner === found) break;
      found = inner;
    }
    return found || document.documentElement || document.body;
  };
  const centerOf = origin => {
    if (!origin || typeof origin !== 'object') return null;
    let element = origin;
    if (typeof origin.__tbRemote === 'number') {
      element = host.__tb_webdriver_element(origin.__tbRemote);
    }
    if (!element || typeof element.getBoundingClientRect !== 'function') return null;
    const rect = element.getBoundingClientRect();
    return { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 };
  };
  let shiftDown = false;
  let ctrlDown = false;
  let altDown = false;
  let metaDown = false;
  const modifierInit = mask => {
    const m = mask || 0;
    return {
      altKey: altDown || !!(m & 1),
      ctrlKey: ctrlDown || !!(m & 2),
      metaKey: metaDown || !!(m & 4),
      shiftKey: shiftDown || !!(m & 8),
    };
  };
  const mouseInit = (x, y, buttons, extra) => Object.assign({
    bubbles: true, cancelable: true, composed: true, view: globalThis,
    clientX: x, clientY: y, screenX: x, screenY: y, buttons: buttons || 0,
  }, modifierInit(extra && extra.modifiers), extra || {});
  const pointerInit = (state, x, y, buttonsOrExtra, maybeExtra) => {
    const extra = maybeExtra === undefined ? null : maybeExtra;
    return Object.assign(mouseInit(x, y, state.buttons, extra), {
      pointerId: 1, isPrimary: true, pointerType: state.pointerType,
      pressure: state.buttons ? 0.5 : 0,
    });
  };
  const fire = (node, event) => node ? host.__tbDispatchTargetTrusted(node, event) : false;
  let activeNode = null;
  const clearActive = () => {
    if (activeNode !== null) {
      try { host.__tbSetActive(activeNode, false); } catch (error) {}
      activeNode = null;
    }
  };
  const setActive = node => {
    clearActive();
    if (!node) return;
    let target = node;
    if (node.localName === 'label') target = node.control || node;
    try {
      host.__tbSetActive(target, true);
      activeNode = target;
    } catch (error) {}
  };
  const pointerItem = (state, item) => {
    if (item.type === 'pointerMove') {
      const center = centerOf(item.origin);
      let x, y;
      if (center) { x = center.x + (item.x || 0); y = center.y + (item.y || 0); }
      else if (item.origin === 'pointer') { x = state.x + (item.x || 0); y = state.y + (item.y || 0); }
      else if (item.origin === undefined || item.origin === 'viewport') { x = item.x || 0; y = item.y || 0; }
      // https://w3c.github.io/webdriver/#dfn-pointer-move
      else throw new Error('stale element reference: pointer origin did not resolve');
      const next = at(x, y);
      if (state.target && state.target !== next) {
        fire(state.target, new PointerEvent('pointerout', pointerInit(state, state.x, state.y, state.buttons)));
        fire(state.target, new PointerEvent('pointerleave', pointerInit(state, state.x, state.y, state.buttons)));
        fire(state.target, new MouseEvent('mouseout', mouseInit(state.x, state.y, state.buttons)));
        fire(state.target, new MouseEvent('mouseleave', mouseInit(state.x, state.y, state.buttons)));
        fire(next, new PointerEvent('pointerover', pointerInit(state, x, y, state.buttons)));
        fire(next, new PointerEvent('pointerenter', pointerInit(state, x, y, state.buttons)));
        fire(next, new MouseEvent('mouseover', mouseInit(x, y, state.buttons)));
        fire(next, new MouseEvent('mouseenter', mouseInit(x, y, state.buttons)));
      }
      state.x = x; state.y = y; state.target = next;
      const moveExtra = item.modifiers ? { modifiers: item.modifiers } : null;
      fire(next, new PointerEvent('pointermove', pointerInit(state, x, y, state.buttons, moveExtra)));
      fire(next, new MouseEvent('mousemove', mouseInit(x, y, state.buttons, moveExtra)));
    } else if (item.type === 'pointerDown') {
      if (item.x !== undefined) state.x = item.x;
      if (item.y !== undefined) state.y = item.y;
      const button = item.button || 0;
      const detail = item.clickCount || 1;
      const extra = { button: button, detail: detail, modifiers: item.modifiers || 0 };
      state.buttons |= 1 << button;
      const node = at(state.x, state.y);
      state.target = node;
      if (node && typeof node.focus === 'function') { try { node.focus(); } catch (error) {} }
      setActive(node);
      fire(node, new PointerEvent('pointerdown', pointerInit(state, state.x, state.y, state.buttons, extra)));
      fire(node, new MouseEvent('mousedown', mouseInit(state.x, state.y, state.buttons, extra)));
      if (button === 2) {
        fire(node, new MouseEvent('contextmenu', mouseInit(state.x, state.y, state.buttons, extra)));
      }
    } else if (item.type === 'pointerUp') {
      if (item.x !== undefined) state.x = item.x;
      if (item.y !== undefined) state.y = item.y;
      const button = item.button || 0;
      const detail = item.clickCount || 1;
      const extra = { button: button, detail: detail, modifiers: item.modifiers || 0 };
      // https://w3c.github.io/uievents/#dom-mouseevent-buttons
      state.buttons &= ~(1 << button);
      clearActive();
      const node = at(state.x, state.y);
      fire(node, new PointerEvent('pointerup', pointerInit(state, state.x, state.y, state.buttons, extra)));
      fire(node, new MouseEvent('mouseup', mouseInit(state.x, state.y, state.buttons, extra)));
      if (button === 0) {
        // https://html.spec.whatwg.org/multipage/interaction.html#activation-behavior
        fire(node, new MouseEvent('click', mouseInit(state.x, state.y, 0, extra)));
        if (detail === 2) {
          fire(node, new MouseEvent('dblclick', mouseInit(state.x, state.y, 0, extra)));
        }
      }
    } else if (item.type === 'pointerCancel') {
      fire(at(state.x, state.y), new PointerEvent('pointercancel', pointerInit(state, state.x, state.y, state.buttons)));
      state.buttons = 0;
    } else throw new TypeError('unknown pointer action type: ' + item.type);
  };
  // https://w3c.github.io/webdriver/#keyboard-actions
  const specialKeys = {
    '\uE000': ['Unidentified', '', 0], '\uE001': ['Cancel', '', 0],
    '\uE002': ['Help', 'Help', 0], '\uE003': ['Backspace', 'Backspace', 0],
    '\uE004': ['Tab', 'Tab', 0], '\uE005': ['Clear', '', 0],
    '\uE006': ['Enter', 'Enter', 0], '\uE007': ['Enter', 'NumpadEnter', 3],
    '\uE008': ['Shift', 'ShiftLeft', 1], '\uE009': ['Control', 'ControlLeft', 1],
    '\uE00A': ['Alt', 'AltLeft', 1], '\uE00B': ['Pause', 'Pause', 0],
    '\uE00C': ['Escape', 'Escape', 0], '\uE00D': [' ', 'Space', 0],
    '\uE00E': ['PageUp', 'PageUp', 0], '\uE00F': ['PageDown', 'PageDown', 0],
    '\uE010': ['End', 'End', 0], '\uE011': ['Home', 'Home', 0],
    '\uE012': ['ArrowLeft', 'ArrowLeft', 0], '\uE013': ['ArrowUp', 'ArrowUp', 0],
    '\uE014': ['ArrowRight', 'ArrowRight', 0], '\uE015': ['ArrowDown', 'ArrowDown', 0],
    '\uE016': ['Insert', 'Insert', 0], '\uE017': ['Delete', 'Delete', 0],
    '\uE018': [';', 'Semicolon', 0], '\uE019': ['=', 'Equal', 0],
    '\uE01A': ['0', 'Numpad0', 3], '\uE01B': ['1', 'Numpad1', 3],
    '\uE01C': ['2', 'Numpad2', 3], '\uE01D': ['3', 'Numpad3', 3],
    '\uE01E': ['4', 'Numpad4', 3], '\uE01F': ['5', 'Numpad5', 3],
    '\uE020': ['6', 'Numpad6', 3], '\uE021': ['7', 'Numpad7', 3],
    '\uE022': ['8', 'Numpad8', 3], '\uE023': ['9', 'Numpad9', 3],
    '\uE024': ['*', 'NumpadMultiply', 3], '\uE025': ['+', 'NumpadAdd', 3],
    '\uE026': [',', 'NumpadComma', 3], '\uE027': ['-', 'NumpadSubtract', 3],
    '\uE028': ['.', 'NumpadDecimal', 3], '\uE029': ['/', 'NumpadDivide', 3],
    '\uE031': ['F1', 'F1', 0], '\uE032': ['F2', 'F2', 0], '\uE033': ['F3', 'F3', 0],
    '\uE034': ['F4', 'F4', 0], '\uE035': ['F5', 'F5', 0], '\uE036': ['F6', 'F6', 0],
    '\uE037': ['F7', 'F7', 0], '\uE038': ['F8', 'F8', 0], '\uE039': ['F9', 'F9', 0],
    '\uE03A': ['F10', 'F10', 0], '\uE03B': ['F11', 'F11', 0], '\uE03C': ['F12', 'F12', 0],
    '\uE03D': ['Meta', 'MetaLeft', 1], '\uE040': ['ZenkakuHankaku', '', 0],
    '\uE050': ['Shift', 'ShiftRight', 2], '\uE051': ['Control', 'ControlRight', 2],
    '\uE052': ['Alt', 'AltRight', 2], '\uE053': ['Meta', 'MetaRight', 2],
    '\uE054': ['PageUp', 'Numpad9', 3], '\uE055': ['PageDown', 'Numpad3', 3],
    '\uE056': ['End', 'Numpad1', 3], '\uE057': ['Home', 'Numpad7', 3],
    '\uE058': ['ArrowLeft', 'Numpad4', 3], '\uE059': ['ArrowUp', 'Numpad8', 3],
    '\uE05A': ['ArrowRight', 'Numpad6', 3], '\uE05B': ['ArrowDown', 'Numpad2', 3],
    '\uE05C': ['Insert', 'Numpad0', 3], '\uE05D': ['Delete', 'NumpadDecimal', 3],
  };
  const punctCodes = {
    '`': 'Backquote', '~': 'Backquote', '!': 'Digit1', '@': 'Digit2', '#': 'Digit3',
    '$': 'Digit4', '%': 'Digit5', '^': 'Digit6', '&': 'Digit7', '*': 'Digit8',
    '(': 'Digit9', ')': 'Digit0', '-': 'Minus', '_': 'Minus', '=': 'Equal', '+': 'Equal',
    '[': 'BracketLeft', '{': 'BracketLeft', ']': 'BracketRight', '}': 'BracketRight',
    '\\': 'Backslash', '|': 'Backslash', ';': 'Semicolon', ':': 'Semicolon',
    '\'': 'Quote', '"': 'Quote', ',': 'Comma', '<': 'Comma', '.': 'Period', '>': 'Period',
    '/': 'Slash', '?': 'Slash', ' ': 'Space', '\n': 'Enter',
  };
  const keyOf = value => {
    if (specialKeys[value]) {
      return { key: specialKeys[value][0], code: specialKeys[value][1], location: specialKeys[value][2] };
    }
    if (value.length === 1) {
      const code = value >= 'a' && value <= 'z' ? 'Key' + value.toUpperCase()
        : value >= 'A' && value <= 'Z' ? 'Key' + value
        : value >= '0' && value <= '9' ? 'Digit' + value
        : (punctCodes[value] || '');
      return { key: value === '\n' ? 'Enter' : value, code: code, location: 0 };
    }
    return { key: value, code: '', location: 0 };
  };
  // https://html.spec.whatwg.org/multipage/input.html#common-input-element-attributes
  const EDITABLE_TYPES = new Set([
    'text', 'search', 'tel', 'url', 'email', 'password',
    'date', 'month', 'week', 'time', 'datetime-local', 'number',
  ]);
  const textControl = (doc = document) => {
    const element = activeDocument(doc).activeElement;
    if (!element) return null;
    const editable = element.tagName === 'TEXTAREA'
      || (element.tagName === 'INPUT' && EDITABLE_TYPES.has(element.type));
    if (!editable || element.disabled || element.readOnly) return null;
    return element;
  };
  // https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#do-not-apply
  const supportsSelection = element =>
    element.tagName === 'TEXTAREA' || element.selectionStart !== null;
  // https://html.spec.whatwg.org/multipage/input.html#number-state-(type=number
  const editText = element =>
    element.tagName === 'INPUT' && element.type === 'number'
      ? (host.__tbNumberBuffer(element) ?? element.value)
      : element.value;
  const caretStart = element => {
    const text = editText(element);
    if (supportsSelection(element)) {
      return element.selectionStart === null || element.selectionStart === undefined
        ? text.length : element.selectionStart;
    }
    const position = host.__tbCaret(element);
    return position === null ? text.length : Math.min(position, text.length);
  };
  const caretEnd = element => {
    const text = editText(element);
    if (supportsSelection(element)) {
      return element.selectionEnd === null || element.selectionEnd === undefined
        ? text.length : element.selectionEnd;
    }
    const position = host.__tbCaret(element);
    return position === null ? text.length : Math.min(position, text.length);
  };
  const setCaret = (element, position) => {
    if (supportsSelection(element)) element.setSelectionRange(position, position);
    else host.__tbSetCaret(element, position);
  };
  // https://html.spec.whatwg.org/multipage/input.html#number-state-(type=number
  const NUMBER_INVALID = 0;
  const NUMBER_EMPTY = 1;
  const NUMBER_COMPLETE = 3;
  const NUMBER_TRAILING_DOT = 4;
  const fireInput = (element, inputType, data) => {
    fire(element, new InputEvent('input', {
      bubbles: true, composed: true, inputType: inputType, data: data,
    }));
  };
  // https://w3c.github.io/uievents/#events-inputevents
  const replaceRange = (element, text, start, end, inputType, data) => {
    const before = new InputEvent('beforeinput', {
      bubbles: true, cancelable: true, composed: true, inputType: inputType, data: data,
    });
    if (!fire(element, before)) return;
    host.__tbMarkUserEdited(element);
    if (element.tagName === 'INPUT' && element.type === 'number') {
      const buffer = editText(element);
      const value = buffer.slice(0, start) + text + buffer.slice(end);
      const state = host.__tbNumberBufferState(value);
      if (state === NUMBER_INVALID) return;
      host.__tbSetNumberBuffer(element, value);
      // https://html.spec.whatwg.org/multipage/input.html#number-state-(type=number
      let exposed = '';
      if (state === NUMBER_COMPLETE) {
        exposed = value.startsWith('+') ? value.slice(1) : value;
        host.__tbSetBadInput(element, false);
      } else if (state === NUMBER_TRAILING_DOT) {
        exposed = value.startsWith('+') ? value.slice(1, -1) : value.slice(0, -1);
        host.__tbSetBadInput(element, false);
      } else if (state === NUMBER_EMPTY) {
        host.__tbSetBadInput(element, false);
      } else {
        host.__tbSetBadInput(element, true);
      }
      host.__tbSetNativeValue(element, exposed);
      setCaret(element, start + text.length);
      fireInput(element, inputType, data);
      return;
    }
    const value = element.value.slice(0, start) + text + element.value.slice(end);
    // https://html.spec.whatwg.org/multipage/interaction.html#input-events
    host.__tbSetNativeValue(element, value);
    setCaret(element, start + text.length);
    fireInput(element, inputType, data);
  };
  const contentEditableHost = (doc = document) => {
    const element = activeDocument(doc).activeElement;
    if (!element || !element.isContentEditable) return null;
    return element;
  };
  const placeContentEditableCaret = (element, atStart) => {
    const owner = element.ownerDocument || document;
    const view = owner.defaultView;
    const selection = view && typeof view.getSelection === 'function' ? view.getSelection() : null;
    if (!selection) return;
    const range = owner.createRange();
    range.selectNodeContents(element);
    range.collapse(atStart);
    selection.removeAllRanges();
    selection.addRange(range);
  };
  const replaceContentEditable = (element, text, inputType, data) => {
    const owner = element.ownerDocument || document;
    const before = new InputEvent('beforeinput', {
      bubbles: true, cancelable: true, composed: true, inputType: inputType, data: data,
    });
    if (!fire(element, before)) return;
    const insertFragment = (range, value) => {
      const parts = value.split('\n');
      let last = null;
      for (let i = 0; i < parts.length; i++) {
        if (i > 0) {
          const br = owner.createElement('br');
          range.insertNode(br);
          range.setStartAfter(br);
          range.collapse(true);
          last = br;
        }
        if (parts[i]) {
          const textNode = owner.createTextNode(parts[i]);
          range.insertNode(textNode);
          range.setStartAfter(textNode);
          range.collapse(true);
          last = textNode;
        }
      }
      return last;
    };
    const view = owner.defaultView;
    const sel = view && typeof view.getSelection === 'function' ? view.getSelection() : null;
    if (sel && sel.rangeCount > 0) {
      const range = sel.getRangeAt(0);
      if (element.contains(range.commonAncestorContainer)) {
        if (range.collapsed && !text && range.startContainer.nodeType === 3) {
          const textLen = range.startContainer.nodeValue ? range.startContainer.nodeValue.length : 0;
          if (inputType === 'deleteContentBackward' && range.startOffset > 0) {
            range.setStart(range.startContainer, range.startOffset - 1);
          } else if (inputType === 'deleteContentForward' && range.endOffset < textLen) {
            range.setEnd(range.endContainer, range.endOffset + 1);
          }
        }
        range.deleteContents();
        if (text) insertFragment(range, text);
        fireInput(element, inputType, data);
        return;
      }
    }
    if (text) {
      const range = owner.createRange();
      range.selectNodeContents(element);
      range.collapse(false);
      insertFragment(range, text);
    }
    fireInput(element, inputType, data);
  };
  const insertText = (text, doc = document) => {
    const element = textControl(doc);
    if (element) {
      const start = caretStart(element);
      const end = Math.max(start, caretEnd(element));
      replaceRange(element, text, start, end, 'insertText', text);
      return;
    }
    const editable = contentEditableHost(doc);
    if (editable) replaceContentEditable(editable, text, 'insertText', text);
  };
  const deleteText = (backwards, doc = document) => {
    const element = textControl(doc);
    if (element) {
      const length = editText(element).length;
      const start = caretStart(element);
      const end = Math.max(start, caretEnd(element));
      if (start === end) {
        if (backwards && start === 0) return;
        if (!backwards && end === length) return;
        const from = backwards ? start - 1 : start;
        const to = backwards ? end : end + 1;
        replaceRange(element, '', from, to, backwards ? 'deleteContentBackward' : 'deleteContentForward', null);
      } else {
        replaceRange(element, '', start, end, 'deleteContentBackward', null);
      }
      return;
    }
    const editable = contentEditableHost(doc);
    if (editable) {
      replaceContentEditable(editable, '', backwards ? 'deleteContentBackward' : 'deleteContentForward', null);
    }
  };
  const moveCaret = (to, doc = document) => {
    const element = textControl(doc);
    if (!element) return;
    const length = editText(element).length;
    const start = caretStart(element);
    const end = Math.max(start, caretEnd(element));
    let position;
    if (to === 'home') position = 0;
    else if (to === 'end') position = length;
    else if (to === 'left') position = start === end ? Math.max(0, start - 1) : start;
    else position = start === end ? Math.min(length, end + 1) : end;
    setCaret(element, position);
  };
  // https://html.spec.whatwg.org/multipage/form-elements.html#the-input-element:radio-button-group
  const radioArrow = direction => {
    const current = document.activeElement;
    if (!current || current.localName !== 'input' || current.type !== 'radio') return false;
    if (current.name === '') return true;
    const radios = Array.from(document.querySelectorAll('input[type=radio]')).filter(radio =>
      radio.name === current.name && radio.form === current.form && !radio.disabled);
    const index = radios.indexOf(current);
    if (index < 0) return true;
    const next = radios[(index + direction + radios.length) % radios.length];
    next.focus();
    next.click();
    return true;
  };
  const keyItem = (item, down, target) => {
    const value = item.value === undefined ? '' : String(item.value);
    const info = keyOf(value);
    const doc = target && target.doc ? target.doc : document;
    const node = target && target.node ? target.node
      : (activeDocument(doc).activeElement
         || activeDocument(doc).body || activeDocument(doc).documentElement);
    const mods = modifierInit(item.modifiers || 0);
    const keyEvent = type => new KeyboardEvent(type, Object.assign({
      bubbles: true, cancelable: true, key: info.key, code: info.code, location: info.location,
    }, modifierInit(item.modifiers || 0)));
    if (down) {
      if (value === '\uE008' || value === '\uE050') shiftDown = true;
      else if (value === '\uE009' || value === '\uE051') ctrlDown = true;
      else if (value === '\uE00A' || value === '\uE052') altDown = true;
      else if (value === '\uE03D' || value === '\uE053') metaDown = true;
      if (!fire(node, keyEvent('keydown'))) return;
      if (value === '\uE00D') {
        const focused = activeDocument(doc).activeElement;
        if (focused && focused.localName === 'input'
            && (focused.type === 'checkbox' || focused.type === 'radio')
            && !focused.disabled) {
          setActive(focused);
        }
      } else if (value === '\uE004') {
        clearActive();
        host.__tbFocusNext(shiftDown);
        return;
      } else if (value === '\uE015') {
        radioArrow(1);
        return;
      } else if (value === '\uE013') {
        radioArrow(-1);
        return;
      } else if (value === '\uE003') {
        deleteText(true, doc);
        return;
      } else if (value === '\uE017' || value === '\uE05D') {
        deleteText(false, doc);
        return;
      } else if (value === '\uE011' || value === '\uE057') {
        moveCaret('home', doc);
        return;
      } else if (value === '\uE010' || value === '\uE056') {
        moveCaret('end', doc);
        return;
      } else if (value === '\uE012' || value === '\uE058') {
        moveCaret('left', doc);
        return;
      } else if (value === '\uE014' || value === '\uE05A') {
        moveCaret('right', doc);
        return;
      }
      const text = value === '\uE00D' ? ' '
        : (value === '\uE006' || value === '\uE007') ? '\n'
        : specialKeys[value] ? null : value;
      if (text === null) return;
      if (!fire(node, keyEvent('keypress'))) return;
      if (mods.ctrlKey || mods.altKey || mods.metaKey) return;
      if (text === '\n') {
        const element = textControl(doc);
        if (element && element.tagName === 'TEXTAREA') insertText('\n', doc);
        else if (!element && contentEditableHost(doc)) insertText('\n', doc);
      } else {
        insertText(text, doc);
      }
    } else {
      if (value === '\uE00D') clearActive();
      if (value === '\uE008' || value === '\uE050') shiftDown = false;
      else if (value === '\uE009' || value === '\uE051') ctrlDown = false;
      else if (value === '\uE00A' || value === '\uE052') altDown = false;
      else if (value === '\uE03D' || value === '\uE053') metaDown = false;
      fire(node, new KeyboardEvent('keyup', Object.assign({
        bubbles: true, cancelable: true, key: info.key, code: info.code, location: info.location,
      }, mods)));
    }
  };
  const wheelItem = item => {
    const x = item.x || 0;
    const y = item.y || 0;
    const dx = item.deltaX || 0;
    const dy = item.deltaY || 0;
    const node = at(x, y);
    if (!node) return;
    // https://w3c.github.io/uievents/#events-wheel
    const accepted = fire(node, new WheelEvent('wheel', mouseInit(x, y, 0, {
      deltaX: dx, deltaY: dy,
    })));
    if (!accepted) return;
    for (let current = node; current && current.nodeType === 1; current = current.parentElement) {
      if (current === document.documentElement || current === document.body) break;
      const canScrollY = dy !== 0 && current.scrollHeight > current.clientHeight;
      const canScrollX = dx !== 0 && current.scrollWidth > current.clientWidth;
      if ((canScrollY || canScrollX) && typeof current.scrollBy === 'function') {
        const beforeX = current.scrollLeft;
        const beforeY = current.scrollTop;
        current.scrollBy(dx, dy);
        if (current.scrollLeft !== beforeX || current.scrollTop !== beforeY) return;
      }
    }
    if (typeof globalThis.scrollBy === 'function') globalThis.scrollBy(dx, dy);
  };
  // https://w3c.github.io/webdriver/#element-click
  host.__tbWebDriverElementClick = function(element) {
    if (!element || typeof element.getBoundingClientRect !== 'function') return 'stale';
    if (typeof element.checkVisibility === 'function' && !element.checkVisibility()) {
      return 'not_interactable';
    }
    const style = typeof getComputedStyle === 'function' ? getComputedStyle(element) : null;
    if (style && style.pointerEvents === 'none') return 'not_interactable';
    const viewport = () => ({
      width: globalThis.innerWidth || 800,
      height: globalThis.innerHeight || 600,
    });
    let size = viewport();
    let rect = element.getBoundingClientRect();
    if (!(rect.width > 0) || !(rect.height > 0)) return 'not_interactable';
    const visible = () => rect.right > 0 && rect.bottom > 0
      && rect.left < size.width && rect.top < size.height;
    if (!visible() && typeof element.scrollIntoView === 'function') {
      element.scrollIntoView({ block: 'center', inline: 'center', behavior: 'instant' });
      size = viewport();
      rect = element.getBoundingClientRect();
    }
    if (!visible()) return 'not_interactable';
    const left = Math.max(0, rect.left);
    const top = Math.max(0, rect.top);
    const right = Math.min(size.width, rect.right);
    const bottom = Math.min(size.height, rect.bottom);
    if (!(right > left) || !(bottom > top)) return 'not_interactable';
    const x = (left + right) / 2;
    const y = (top + bottom) / 2;
    const owner = element.ownerDocument || document;
    let hit = typeof owner.elementFromPoint === 'function' ? owner.elementFromPoint(x, y) : null;
    while (hit && hit.shadowRoot && typeof hit.shadowRoot.elementFromPoint === 'function') {
      const inner = hit.shadowRoot.elementFromPoint(x, y);
      if (!inner || inner === hit) break;
      hit = inner;
    }
    if (!hit) return 'not_interactable';
    const shadowIncludingContains = (root, target) => {
      for (let node = target; node;) {
        if (node === root) return true;
        node = node.nodeType === 11 && node.host ? node.host : node.parentNode;
      }
      return false;
    };
    if (!shadowIncludingContains(element, hit)) {
      if (shadowIncludingContains(hit, element)) return 'not_interactable';
      return 'intercepted';
    }
    const state = pointerState('__tb_element_click', 'mouse');
    pointerItem(state, { type: 'pointerMove', x: x, y: y });
    pointerItem(state, { type: 'pointerDown', button: 0, clickCount: 1 });
    pointerItem(state, { type: 'pointerUp', button: 0, clickCount: 1 });
    return 'ok';
  };
  Object.defineProperty(host, '__tbWebDriverElementClick', {
    writable: false, configurable: false, enumerable: false,
  });
  const activeDocument = (doc = document) => {
    const element = doc.activeElement;
    if (element && (element.localName === 'iframe' || element.localName === 'frame')
        && element.contentDocument) {
      return element.contentDocument;
    }
    return doc;
  };
  // https://w3c.github.io/webdriver/#element-send-keys
  host.__tbWebDriverUpload = function(element, files, append, strict) {
    if (!element || element.tagName !== 'INPUT' || element.type !== 'file') return 'stale';
    if (strict) {
      if (typeof element.checkVisibility === 'function' && !element.checkVisibility()) {
        return 'not_interactable';
      }
      try { element.focus(); } catch (error) {}
      if (document.activeElement !== element) return 'not_interactable';
    }
    const decode = text => {
      const binary = globalThis.atob(text);
      const bytes = new Uint8Array(binary.length);
      for (let index = 0; index < binary.length; index++) bytes[index] = binary.charCodeAt(index);
      return bytes;
    };
    const selected = [];
    if (append) {
      const current = element.files;
      for (let index = 0; index < current.length; index++) selected.push(current[index]);
    }
    for (const file of files) {
      selected.push(new File([decode(file.data)], file.name, {
        type: file.type, lastModified: file.lastModified,
      }));
    }
    element.files = host.__tbCreateFileList(selected);
    fire(element, new Event('input', { bubbles: true }));
    fire(element, new Event('change', { bubbles: true }));
    return 'ok';
  };
  Object.defineProperty(host, '__tbWebDriverUpload', {
    writable: false, configurable: false, enumerable: false,
  });
  // https://w3c.github.io/webdriver/#element-send-keys
  const hookChangeOnBlur = element => {
    if (changeState.has(element)) return;
    changeState.set(element, { baseline: element.value });
    __tbApply(addEventListener, element, ['blur', () => {
      if (element.value !== changeState.get(element).baseline) {
        fire(element, new Event('change', { bubbles: true }));
      }
    }]);
  };
  host.__tbWebDriverSendKeys = function(element, text) {
    if (!element || typeof element.focus !== 'function') return 'not_interactable';
    const doc = element.ownerDocument || document;
    const style = typeof getComputedStyle === 'function' ? getComputedStyle(element) : null;
    if (style && (style.visibility === 'hidden' || style.visibility === 'collapse')) {
      return 'not_interactable';
    }
    if (typeof element.checkVisibility === 'function' && !element.checkVisibility()) {
      return 'not_interactable';
    }
    const rect = element.getBoundingClientRect();
    if (!(rect.width > 0) || !(rect.height > 0)) return 'not_interactable';
    const view = doc.defaultView || globalThis;
    if (rect.bottom <= 0 || rect.right <= 0
        || rect.top >= (view.innerHeight || 0) || rect.left >= (view.innerWidth || 0)) {
      if (typeof element.scrollIntoView === 'function') {
        element.scrollIntoView({ block: 'center', inline: 'center', behavior: 'instant' });
      }
    }
    const localName = element.localName;
    let eventDoc = doc;
    let node = element;
    if (localName === 'iframe' || localName === 'frame') {
      host.__tbWebDriverSetActiveElement(element);
      eventDoc = element.contentDocument || doc;
      node = eventDoc.activeElement || eventDoc.body || eventDoc.documentElement;
    } else if (element === doc.body || element === doc.documentElement) {
      host.__tbWebDriverSetActiveElement(element);
    } else {
      const alreadyFocused = doc.activeElement === element;
      if (!alreadyFocused) {
        try { element.focus(); } catch (error) {}
        if (doc.activeElement !== element) return 'not_interactable';
        const editable = (element.tagName === 'INPUT' || element.tagName === 'TEXTAREA')
          ? !element.readOnly && !element.disabled
          : element.isContentEditable;
        if (editable) {
          if (element.tagName === 'INPUT' || element.tagName === 'TEXTAREA') {
            try { element.setSelectionRange(element.value.length, element.value.length); } catch (error) {}
          } else {
            placeContentEditableCaret(element, false);
          }
        }
      }
      if (element.isContentEditable) {
        const docView = doc.defaultView;
        const selection = docView && typeof docView.getSelection === 'function' ? docView.getSelection() : null;
        const range = selection && selection.rangeCount > 0 ? selection.getRangeAt(0) : null;
        const inside = range
          && element.contains(range.commonAncestorContainer);
        if (!inside) placeContentEditableCaret(element, true);
      }
    }
    if (element.tagName === 'INPUT' || element.tagName === 'TEXTAREA') {
      hookChangeOnBlur(element);
      changeState.get(element).baseline = element.value;
    }
    const target = { node: node, doc: eventDoc };
    for (const character of String(text)) {
      keyItem({ type: 'keyDown', value: character }, true, target);
      keyItem({ type: 'keyUp', value: character }, false, target);
    }
    return 'ok';
  };
  Object.defineProperty(host, '__tbWebDriverSendKeys', {
    writable: false, configurable: false, enumerable: false,
  });
  host.__tbWebDriverActions = function(actions) {
    let ticks = 0;
    for (const source of actions) {
      const count = source.actions ? source.actions.length : 0;
      if (count > ticks) ticks = count;
    }
    for (let tick = 0; tick < ticks; tick++) {
      for (const source of actions) {
        const item = source.actions ? source.actions[tick] : null;
        if (!item || item.type === 'pause') continue;
        if (source.type === 'pointer') {
          const parameters = source.parameters || {};
          pointerItem(pointerState(source.id, parameters.pointerType), item);
        } else if (source.type === 'key') {
          if (item.type === 'keyDown') keyItem(item, true);
          else if (item.type === 'keyUp') keyItem(item, false);
          else if (item.type === 'insertText') insertText(String(item.value === undefined ? '' : item.value));
          else throw new TypeError('unknown key action type: ' + item.type);
        } else if (source.type === 'wheel') {
          if (item.type !== 'scroll') throw new TypeError('unknown wheel action type: ' + item.type);
          wheelItem(item);
        } else throw new TypeError('unknown action source type: ' + source.type);
      }
    }
    return true;
  };
  Object.defineProperty(host, '__tbWebDriverActions', {
    writable: false, configurable: false, enumerable: false,
  });
})();
