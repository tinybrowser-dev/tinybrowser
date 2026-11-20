// https://webassembly.github.io/spec/js-api/#memories
// https://webassembly.github.io/spec/js-api/#ref-for-refresh-the-memory-buffer
// https://github.com/WebAssembly/threads/blob/main/proposals/threads/Overview.md#shared-linear-memory — grows shared memory in place so all agents observe the new length, unlike Chrome 152's fixed-length replacement
(function() {
  // https://webassembly.github.io/spec/core/exec/runtime.html#memory-instances
  const PAGE_SIZE = 65536;
  // https://webassembly.github.io/spec/core/valid/types.html#valid-memtype
  const MAX_I32_PAGES = 65536;
  const MAX_I64_PAGES = 0x1000000000000n;

  const memories = new WeakMap();

  // https://webassembly.github.io/spec/js-api/#addressvaluetou64
  function to_page_count(value, name, address) {
    if (address === 'i64') {
      let pages;
      if (typeof value === 'bigint') {
        pages = value;
      } else if (typeof value === 'boolean') {
        pages = value ? 1n : 0n;
      } else if (typeof value === 'string') {
        pages = BigInt(value);
      } else {
        throw new TypeError(`WebAssembly.Memory(): Property '${name}' must be a BigInt for an i64 memory`);
      }
      if (pages < 0n) {
        throw new TypeError(`WebAssembly.Memory(): Property '${name}' must be non-negative`);
      }
      if (pages > MAX_I64_PAGES) {
        throw new RangeError(`WebAssembly.Memory(): Property '${name}': value ${pages} is above the upper bound ${MAX_I64_PAGES}`);
      }
      return pages;
    }
    const number = Number(value);
    if (!Number.isFinite(number)) {
      throw new TypeError(`WebAssembly.Memory(): Property '${name}' must be a finite number`);
    }
    const pages = Math.trunc(number);
    if (pages < 0 || pages > 0xFFFFFFFF) {
      throw new TypeError(`WebAssembly.Memory(): Property '${name}' must be a non-negative 32-bit integer`);
    }
    if (pages > MAX_I32_PAGES) {
      throw new RangeError(`WebAssembly.Memory(): Property '${name}': value ${pages} is above the upper bound ${MAX_I32_PAGES}`);
    }
    return pages;
  }

  function byte_length(pages) {
    const bytes = Number(pages) * PAGE_SIZE;
    if (!Number.isSafeInteger(bytes)) {
      throw new RangeError('WebAssembly.Memory is too large to allocate');
    }
    return bytes;
  }

  function page_limit(state) {
    return state.address === 'i64' ? MAX_I64_PAGES : MAX_I32_PAGES;
  }

  function allocate(state, pages) {
    const bytes = byte_length(pages);
    if (!state.shared) {
      return new ArrayBuffer(bytes);
    }
    if (state.maximum !== null && state.maximum > pages) {
      return new SharedArrayBuffer(bytes, { maxByteLength: byte_length(state.maximum) });
    }
    return new SharedArrayBuffer(bytes);
  }

  function refresh(state, pages) {
    const bytes = byte_length(pages);
    if (state.shared) {
      if (pages > state.pages) {
        state.buffer.grow(bytes);
      }
      return;
    }
    const previous = state.buffer;
    const kept = previous.detached ? null : new Uint8Array(previous).slice();
    const next = new ArrayBuffer(bytes);
    if (kept !== null) {
      new Uint8Array(next).set(kept.subarray(0, Math.min(kept.length, bytes)));
      previous.transfer(0);
    }
    state.buffer = next;
  }

  class Memory {
    constructor(descriptor) {
      if (descriptor === null || typeof descriptor !== 'object') {
        throw new TypeError('WebAssembly.Memory(): Argument 0 must be a memory descriptor');
      }
      const address = descriptor.address === undefined ? 'i32' : descriptor.address;
      if (address !== 'i32' && address !== 'i64') {
        throw new TypeError(`WebAssembly.Memory(): Unknown address type ${String(address)}`);
      }
      if (descriptor.initial === undefined) {
        throw new TypeError("WebAssembly.Memory(): Property 'initial' is required");
      }
      const initial = to_page_count(descriptor.initial, 'initial', address);
      const maximum = descriptor.maximum === undefined
        ? null
        : to_page_count(descriptor.maximum, 'maximum', address);
      if (maximum !== null && maximum < initial) {
        throw new RangeError(`WebAssembly.Memory(): Property 'maximum': value ${maximum} is below the lower bound ${initial}`);
      }
      const shared = Boolean(descriptor.shared);
      // https://github.com/WebAssembly/threads/blob/main/proposals/threads/Overview.md#shared-linear-memory — grows shared memory in place so all agents observe the new length, unlike Chrome 152's fixed-length replacement
      if (shared && maximum === null) {
        throw new TypeError('WebAssembly.Memory(): If shared is true, maximum property should be defined.');
      }
      const state = { pages: initial, maximum, shared, address };
      state.buffer = allocate(state, initial);
      memories.set(this, state);
    }

    get buffer() {
      const state = memories.get(this);
      if (state === undefined) {
        throw new TypeError('WebAssembly.Memory.prototype.buffer called on an incompatible receiver');
      }
      return state.buffer;
    }

    grow(delta) {
      const state = memories.get(this);
      if (state === undefined) {
        throw new TypeError('WebAssembly.Memory.prototype.grow called on an incompatible receiver');
      }
      const pages = to_page_count(delta, 'delta', state.address);
      const next = state.pages + pages;
      if (state.maximum !== null && next > state.maximum) {
        throw new RangeError('WebAssembly.Memory.grow(): Maximum memory size exceeded');
      }
      if (next > page_limit(state)) {
        throw new RangeError('WebAssembly.Memory.grow(): Maximum memory size exceeded');
      }
      refresh(state, next);
      const previous = state.pages;
      state.pages = next;
      return previous;
    }
  }

  Object.defineProperty(Memory.prototype, Symbol.toStringTag, {
    value: 'WebAssembly.Memory',
    configurable: true,
  });

  const namespace = globalThis.WebAssembly === undefined ? {} : globalThis.WebAssembly;
  Object.defineProperty(namespace, 'Memory', {
    value: Memory,
    writable: true,
    configurable: true,
  });
  if (globalThis.WebAssembly === undefined) {
    Object.defineProperty(globalThis, 'WebAssembly', {
      value: namespace,
      writable: true,
      configurable: true,
    });
  }
})();
