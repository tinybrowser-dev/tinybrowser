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
    const found = document.elementFromPoint ? document.elementFromPoint(x, y) : null;
    return found || document.documentElement || document.body;
  };
  const centerOf = origin => {
    if (!origin || typeof origin !== 'object' || typeof origin.__tbRemote !== 'number') return null;
    const element = host.__tb_webdriver_element(origin.__tbRemote);
    if (!element || typeof element.getBoundingClientRect !== 'function') return null;
    const rect = element.getBoundingClientRect();
    return { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 };
  };
  const mouseInit = (x, y, buttons, extra) => Object.assign({
    bubbles: true, cancelable: true, composed: true, view: globalThis,
    clientX: x, clientY: y, screenX: x, screenY: y, buttons: buttons || 0,
  }, extra || {});
  const pointerInit = (state, x, y, buttonsOrExtra, maybeExtra) => {
    const extra = maybeExtra === undefined ? null : maybeExtra;
    return Object.assign(mouseInit(x, y, state.buttons, extra), {
      pointerId: 1, isPrimary: true, pointerType: state.pointerType,
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
      fire(next, new PointerEvent('pointermove', pointerInit(state, x, y, state.buttons)));
      fire(next, new MouseEvent('mousemove', mouseInit(x, y, state.buttons)));
    } else if (item.type === 'pointerDown') {
      const button = item.button || 0;
      state.buttons |= 1 << button;
      const node = at(state.x, state.y);
      state.target = node;
      if (node && typeof node.focus === 'function') { try { node.focus(); } catch (error) {} }
      setActive(node);
      fire(node, new PointerEvent('pointerdown', pointerInit(state, state.x, state.y, state.buttons, { button: button })));
      fire(node, new MouseEvent('mousedown', mouseInit(state.x, state.y, state.buttons, { button: button })));
    } else if (item.type === 'pointerUp') {
      const button = item.button || 0;
      // https://w3c.github.io/uievents/#dom-mouseevent-buttons
      state.buttons &= ~(1 << button);
      clearActive();
      const node = at(state.x, state.y);
      fire(node, new PointerEvent('pointerup', pointerInit(state, state.x, state.y, state.buttons, { button: button })));
      fire(node, new MouseEvent('mouseup', mouseInit(state.x, state.y, state.buttons, { button: button })));
      if (button === 0) {
        // https://html.spec.whatwg.org/multipage/interaction.html#activation-behavior
        fire(node, new MouseEvent('click', mouseInit(state.x, state.y, 0, { button: 0 })));
      }
    } else if (item.type === 'pointerCancel') {
      fire(at(state.x, state.y), new PointerEvent('pointercancel', pointerInit(state, state.x, state.y, state.buttons)));
      state.buttons = 0;
    } else throw new TypeError('unknown pointer action type: ' + item.type);
  };
  const specialKeys = {
    '\uE000': ['Unidentified', ''], '\uE003': ['Backspace', 'Backspace'], '\uE004': ['Tab', 'Tab'],
    '\uE006': ['Enter', 'Enter'], '\uE007': ['Enter', 'Enter'],
    '\uE008': ['Shift', 'ShiftLeft'], '\uE009': ['Control', 'ControlLeft'],
    '\uE00A': ['Alt', 'AltLeft'], '\uE00C': ['Escape', 'Escape'],
    '\uE00D': [' ', 'Space'], '\uE00E': ['PageUp', 'PageUp'], '\uE00F': ['PageDown', 'PageDown'],
    '\uE010': ['End', 'End'], '\uE011': ['Home', 'Home'],
    '\uE012': ['ArrowLeft', 'ArrowLeft'], '\uE013': ['ArrowUp', 'ArrowUp'],
    '\uE014': ['ArrowRight', 'ArrowRight'], '\uE015': ['ArrowDown', 'ArrowDown'],
    '\uE017': ['Delete', 'Delete'],
  };
  const keyOf = value => {
    if (specialKeys[value]) return { key: specialKeys[value][0], code: specialKeys[value][1] };
    if (value.length === 1) {
      const code = value >= 'a' && value <= 'z' ? 'Key' + value.toUpperCase()
        : value >= 'A' && value <= 'Z' ? 'Key' + value
        : value >= '0' && value <= '9' ? 'Digit' + value
        : '';
      return { key: value, code: code };
    }
    return { key: value, code: value };
  };
  // https://html.spec.whatwg.org/multipage/input.html#common-input-element-attributes
  const EDITABLE_TYPES = new Set([
    'text', 'search', 'tel', 'url', 'email', 'password',
    'date', 'month', 'week', 'time', 'datetime-local', 'number',
  ]);
  const textControl = () => {
    const element = document.activeElement;
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
  const insertText = text => {
    const element = textControl();
    if (!element) return;
    const start = caretStart(element);
    const end = Math.max(start, caretEnd(element));
    replaceRange(element, text, start, end, 'insertText', text);
  };
  const deleteText = backwards => {
    const element = textControl();
    if (!element) return;
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
  };
  const moveCaret = to => {
    const element = textControl();
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
  let shiftDown = false;
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
  const keyItem = (item, down) => {
    const value = item.value === undefined ? '' : String(item.value);
    const info = keyOf(value);
    const node = document.activeElement || document.body || document.documentElement;
    if (down) {
      const accepted = fire(node, new KeyboardEvent('keydown', {
        bubbles: true, cancelable: true, key: info.key, code: info.code,
      }));
      if (accepted) {
        if (value === '\uE008') shiftDown = true;
        else if (value === '\uE00D') {
          const focused = document.activeElement;
          if (focused && focused.localName === 'input'
              && (focused.type === 'checkbox' || focused.type === 'radio')
              && !focused.disabled) {
            setActive(focused);
          }
          insertText(' ');
        } else if (value === '\uE004') {
          clearActive();
          host.__tbFocusNext(shiftDown);
        } else if (value === '\uE015') {
          radioArrow(1);
        } else if (value === '\uE013') {
          radioArrow(-1);
        } else if (value.length === 1) insertText(value);
        else if (value === '\uE003') deleteText(true);
        else if (value === '\uE017') deleteText(false);
        else if (value === '\uE011') moveCaret('home');
        else if (value === '\uE010') moveCaret('end');
        else if (value === '\uE012') moveCaret('left');
        else if (value === '\uE014') moveCaret('right');
      }
      if (value.length === 1 && value.charCodeAt(0) < 0xE000) {
        fire(node, new KeyboardEvent('keypress', { bubbles: true, cancelable: true, key: info.key, code: info.code }));
      }
    } else {
      if (value === '\uE00D') clearActive();
      if (value === '\uE008') shiftDown = false;
      fire(node, new KeyboardEvent('keyup', { bubbles: true, cancelable: true, key: info.key, code: info.code }));
    }
  };
  const wheelItem = item => {
    const x = item.x || 0;
    const y = item.y || 0;
    const node = at(x, y);
    if (!node) return;
    // https://w3c.github.io/uievents/#events-wheel
    const accepted = fire(node, new WheelEvent('wheel', mouseInit(x, y, 0, {
      deltaX: item.deltaX || 0, deltaY: item.deltaY || 0,
    })));
    if (accepted && typeof node.scrollBy === 'function') node.scrollBy(item.deltaX || 0, item.deltaY || 0);
  };
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
    if (!element || typeof element.focus !== 'function') return false;
    try { element.focus(); } catch (error) {}
    hookChangeOnBlur(element);
    changeState.get(element).baseline = element.value;
    for (const character of String(text)) {
      const code = character.charCodeAt(0);
      if (character.length === 1 && code >= 0xE000 && code <= 0xE05D) {
        if (character === '\uE003') deleteText(true);
        else if (character === '\uE017') deleteText(false);
        else if (character === '\uE011') moveCaret('home');
        else if (character === '\uE010') moveCaret('end');
        else if (character === '\uE012') moveCaret('left');
        else if (character === '\uE014') moveCaret('right');
        else if (character === '\uE00D') insertText(' ');
        else if (character === '\uE004') {
          clearActive();
          host.__tbFocusNext(false);
        }
      } else {
        insertText(character);
      }
    }
    return true;
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
