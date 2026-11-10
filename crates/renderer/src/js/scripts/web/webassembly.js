// https://webassembly.github.io/spec/js-api/#memories
//
// `WebAssembly.Memory` is the only part of the WebAssembly JS API this engine
// provides: there is no module compiler, so a page can create and grow a
// memory's backing buffer but cannot instantiate a module over it.
//
// A non-shared memory's `buffer` is a fixed-length `ArrayBuffer`, as the
// current JS API draft defines it; `grow()` detaches that buffer and installs
// a new one, so `resizable` is always false and `maxByteLength` always equals
// `byteLength`
// (<https://webassembly.github.io/spec/js-api/#ref-for-refresh-the-memory-buffer>).
//
// A shared memory's buffer is a growable `SharedArrayBuffer`, and `grow()`
// extends it in place so every view in every realm sees the new length, which
// is what the threads proposal requires: all agents in the cluster have access
// to the additional linear memory
// (<https://github.com/WebAssembly/threads/blob/main/proposals/threads/Overview.md#shared-linear-memory>).
// Chrome 152 instead hands out a fixed-length `SharedArrayBuffer` and replaces
// it on growth; JavaScript cannot alias a new `SharedArrayBuffer` to an
// existing block, so in-place growth is the only way to keep the memory
// genuinely shared.
//
// The draft's `toResizableBuffer()`/`toFixedLengthBuffer()` are not provided:
// they would have to change the buffer object of a shared memory, which the
// same aliasing limit makes impossible.
(function() {
  /// Bytes per WebAssembly page
  /// (<https://webassembly.github.io/spec/core/exec/runtime.html#memory-instances>).
  const PAGE_SIZE = 65536;
  /// A 32-bit memory cannot address more than 2^16 pages
  /// (<https://webassembly.github.io/spec/core/valid/types.html#valid-memtype>).
  const MAX_I32_PAGES = 65536;
  /// A 64-bit memory cannot address more than 2^48 pages.
  const MAX_I64_PAGES = 0x1000000000000n;

  const memories = new WeakMap();

  /// Reads one address value as a page count
  /// (<https://webassembly.github.io/spec/js-api/#addressvaluetou64>).
  ///
  /// `i32` follows `[EnforceRange] unsigned long`: a fraction truncates toward
  /// zero, a negative number or a non-finite one is a type error, and a value
  /// past 2^32 - 1 is also a type error. The page limit is the memory type's
  /// own bound, which is a range error. `i64` requires a BigInt.
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

  /// The byte length of `pages` pages, rejecting a size a JavaScript buffer
  /// cannot represent.
  function byte_length(pages) {
    const bytes = Number(pages) * PAGE_SIZE;
    if (!Number.isSafeInteger(bytes)) {
      throw new RangeError('WebAssembly.Memory is too large to allocate');
    }
    return bytes;
  }

  /// Whether `pages` is past the memory type's bound.
  function page_limit(state) {
    return state.address === 'i64' ? MAX_I64_PAGES : MAX_I32_PAGES;
  }

  /// Allocates `state`'s initial buffer.
  ///
  /// A shared memory with room to grow reserves its maximum up front, so a
  /// growth extends the same block instead of copying. A memory whose maximum
  /// equals its initial size is fixed, and a non-shared memory is always
  /// fixed.
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

  /// Installs the buffer for the new page count, preserving its bytes.
  ///
  /// A non-shared buffer is detached, exactly as refreshing a fixed-length
  /// memory buffer does, and replaced by a fresh fixed one. A shared buffer
  /// grows in place. Growing by zero still replaces a non-shared buffer,
  /// because the specification refreshes the buffer on every `grow()`.
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
      // Detaching makes every old view report a zero length instead of
      // silently keeping a stale copy.
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
      // A shared memory without a maximum could never be grown, and the
      // allocator has to reserve the block once
      // (<https://github.com/WebAssembly/threads/blob/main/proposals/threads/Overview.md#shared-linear-memory>).
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
