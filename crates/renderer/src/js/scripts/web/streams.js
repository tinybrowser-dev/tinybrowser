// A ReadableStream with a synchronous chunk queue: enough for Blob.stream()
// and other producers that push all their chunks up front.
const __tbStreamBrand = value => __tbBrand(value, __tbStreamData);

/// Wraps a transferred endpoint in a stream that pulls one chunk per message
/// from its peer.
///
/// The sender keeps the original stream locked and reads it on demand, so a
/// transfer moves the stream rather than draining it. Pulling is a message,
/// not an eager pump, which is what gives the receiving stream backpressure
/// (<https://streams.spec.whatwg.org/#readable-stream-transfer>).
const __tbStreamFromPort = port => {
  const pulls = [];
  let controller = null;
  let finished = false;
  const wake = () => {
    while (pulls.length > 0) {
      const resolve = __tbArray.shift(pulls);
      resolve();
    }
  };
  const stream = new globalThis.ReadableStream({
    start(value) { controller = value; },
    pull() {
      if (finished) return undefined;
      return new Promise(resolve => {
        __tbArray.push(pulls, resolve);
        port.postMessage('pull');
      });
    },
    cancel(reason) {
      if (finished) return;
      finished = true;
      wake();
      try { port.postMessage(['cancel', reason]); } catch (error) { /* the peer is gone */ }
      port.close();
    },
  });
  port.onmessage = event => {
    const message = event.data;
    if (!__tbIsArray(message)) return;
    if (message[0] === 'chunk') {
      controller.enqueue(message[1]);
      wake();
    } else if (message[0] === 'close') {
      finished = true;
      controller.close();
      wake();
      port.close();
    } else if (message[0] === 'error') {
      finished = true;
      controller.error(message[1]);
      wake();
      port.close();
    }
  };
  return stream;
};

// Reader results use a null prototype so a page-installed
// `Object.prototype.then` cannot turn them into thenables.
const __tbStreamResult = (value, done) => {
  const result = __tbObjectCreate(null);
  result.value = value;
  result.done = done;
  return result;
};
const __tbStreamWake = data => {
  const waiters = data.readWaiters;
  data.readWaiters = [];
  for (let index = 0; index < waiters.length; index++) {
    if (data.errored !== null) waiters[index].reject(data.errored);
    else waiters[index].resolve();
  }
};
const __tbWritableData = host.slots('tinybrowser.writable.data');
const __tbWritableBrand = value => __tbBrand(value, __tbWritableData);
globalThis.ReadableStream = class ReadableStream {
  constructor(source, strategy) {
    const sourceObject = source === undefined ? {} : Object(source);
    // Read source fields only when they are own properties, so a throwing
    // `Object.prototype.start` (or `type`) cannot affect stream creation
    // (<https://streams.spec.whatwg.org/#readable-stream-initialize>).
    const own = key => __tbGetOwnPropertyDescriptor(sourceObject, key) !== undefined;
    const type = own('type') && sourceObject.type === 'bytes' ? 'bytes' : 'default';
    let resolveClosed;
    let rejectClosed;
    const closedPromise = new Promise((resolve, reject) => {
      resolveClosed = resolve;
      rejectClosed = reject;
    });
    // A stream may error before anyone observes `reader.closed`; mark the
    // internal rejection handled while leaving the promise rejected for later
    // observers.
    closedPromise.catch(() => {});
    const data = {
      chunks: [], closed: false, closing: false, errored: null, canceled: false, reader: null,
      disturbed: false, cancelWaiters: [], readWaiters: [],
      closedPromise, resolveClosed, rejectClosed,
      type,
      pull: own('pull') && typeof sourceObject.pull === 'function' ? sourceObject.pull : null,
      cancel: own('cancel') && typeof sourceObject.cancel === 'function' ? sourceObject.cancel : null,
      controller: null,
      source: sourceObject,
    };
    __tbStreamData.set(this, data);
    data.controller = {
      enqueue(chunk) {
        if (data.closed || data.errored !== null || data.canceled) return;
        __tbArray.push(data.chunks, chunk);
        __tbStreamWake(data);
      },
      close() {
        if (data.closed || data.errored !== null) return;
        data.closed = true;
        data.resolveClosed();
        __tbStreamWake(data);
      },
      error(error) {
        if (data.closed || data.errored !== null) return;
        data.errored = error;
        data.rejectClosed(error);
        __tbStreamWake(data);
      },
      get desiredSize() { return data.closed ? null : 1; },
    };
    if (own('start') && typeof sourceObject.start === 'function') {
      try {
        sourceObject.start(data.controller);
      } catch (error) {
        data.controller.error(error);
      }
    }
  }
  get locked() { return __tbStreamBrand(this).reader !== null; }
  getReader(options) {
    const data = __tbStreamBrand(this);
    if (data.disturbed) throw new TypeError('The stream is disturbed');
    if (data.reader !== null) throw new TypeError('The stream is locked to a reader');
    if (options !== undefined && options !== null && Object(options).mode !== undefined) {
      const mode = Object(options).mode;
      if (mode !== 'byob') throw new TypeError('Invalid reader mode');
      if (data.type !== 'bytes') throw new TypeError('BYOB requires a byte stream');
      const reader = {
        read: view => {
          if (!ArrayBuffer.isView(view) || !(view instanceof Uint8Array)) {
            return Promise.reject(new TypeError('BYOB read needs a Uint8Array view'));
          }
          const step = () => {
            data.disturbed = true;
            if (data.errored !== null) return Promise.reject(data.errored);
            if (data.chunks.length === 0) {
              if (data.closed || data.canceled) return Promise.resolve(__tbStreamResult(view.subarray(0, 0), true));
              if (data.pull !== null) {
                const pulled = Promise.resolve(__tbApply(data.pull, data.source, [data.controller]))
                  .then(step, error => { data.controller.error(error); throw error; });
                const canceled = new Promise(resolve => __tbArray.push(data.cancelWaiters, resolve));
                return Promise.race([pulled, canceled]).then(value => {
                  if (data.canceled) return __tbStreamResult(view.subarray(0, 0), true);
                  return value;
                });
              }
              return new Promise((resolve, reject) => {
                __tbArray.push(data.readWaiters, { resolve, reject });
              }).then(step);
            }
            const chunk = __tbArray.shift(data.chunks);
            const bytes = chunk instanceof Uint8Array
              ? chunk
              : ArrayBuffer.isView(chunk)
                ? new Uint8Array(chunk.buffer, chunk.byteOffset, chunk.byteLength)
                : chunk instanceof ArrayBuffer
                  ? new Uint8Array(chunk)
                  : null;
            if (bytes === null) return Promise.reject(new TypeError('A body stream chunk must be a BufferSource'));
            const length = Math.min(bytes.length, view.byteLength);
            view.set(bytes.subarray(0, length));
            if (length < bytes.length) __tbArray.push(data.chunks, bytes.subarray(length));
            return Promise.resolve(__tbStreamResult(view.subarray(0, length), false));
          };
          return step();
        },
        cancel: reason => this.cancel(reason),
        releaseLock: () => { data.reader = null; },
        closed: data.closedPromise,
      };
      data.reader = reader;
      return reader;
    }
    const reader = {
      read: () => {
        const step = () => {
          data.disturbed = true;
          if (data.errored !== null) return Promise.reject(data.errored);
          if (data.chunks.length > 0) return Promise.resolve(__tbStreamResult(__tbArray.shift(data.chunks), false));
          if (data.closed || data.canceled) return Promise.resolve(__tbStreamResult(undefined, true));
          if (data.pull !== null) {
            const pulled = Promise.resolve(__tbApply(data.pull, data.source, [data.controller]))
              .then(step, error => { data.controller.error(error); throw error; });
            const canceled = new Promise(resolve => __tbArray.push(data.cancelWaiters, resolve));
            return Promise.race([pulled, canceled]).then(value => {
              if (data.canceled) return __tbStreamResult(undefined, true);
              return value;
            });
          }
          return new Promise((resolve, reject) => {
            __tbArray.push(data.readWaiters, { resolve, reject });
          }).then(step);
        };
        return step();
      },
      cancel: reason => this.cancel(reason),
      releaseLock: () => { data.reader = null; },
      closed: data.closedPromise,
    };
    data.reader = reader;
    return reader;
  }
  cancel(reason) {
    const data = __tbStreamBrand(this);
    data.disturbed = true;
    // Canceling resets the queue: a later `read()` resolves done, it does
    // not drain what was queued
    // (<https://streams.spec.whatwg.org/#readable-stream-cancel>).
    data.canceled = true;
    data.chunks.length = 0;
    data.resolveClosed();
    while (data.cancelWaiters.length > 0) {
      const waiter = __tbArray.shift(data.cancelWaiters);
      waiter(__tbStreamResult(undefined, true));
    }
    __tbStreamWake(data);
    if (data.cancel !== null) return Promise.resolve(__tbApply(data.cancel, data.source, [reason]));
    return Promise.resolve();
  }
  pipeTo(destination, options) {
    const data = __tbStreamBrand(this);
    let reader;
    try { reader = this.getReader(); } catch (error) { return Promise.reject(error); }
    let writer;
    try {
      writer = destination.getWriter();
    } catch (error) {
      reader.releaseLock();
      return Promise.reject(error);
    }
    data.disturbed = true;
    const signal = options !== undefined && options !== null ? Object(options).signal : undefined;
    if (signal !== undefined && signal.aborted) {
      reader.releaseLock();
      writer.releaseLock();
      return Promise.reject(signal.reason);
    }
    const pump = () => reader.read().then(step => {
      if (step.done) return writer.close();
      return Promise.resolve(writer.write(step.value)).then(pump);
    });
    return pump().then(() => {
      reader.releaseLock();
      writer.releaseLock();
    }, error => Promise.resolve(writer.abort(error)).catch(() => {}).then(() => {
      reader.releaseLock();
      writer.releaseLock();
      throw error;
    }));
  }
  pipeThrough(transform, options) {
    const promise = this.pipeTo(transform.writable, options);
    // The pipe promise is observed so a failure is not an unhandled rejection;
    // the caller observes it through the readable branch.
    promise.catch(() => {});
    return transform.readable;
  }
  tee() {
    const data = __tbStreamBrand(this);
    if (data.reader !== null) throw new TypeError('The stream is locked to a reader');
    const reader = this.getReader();
    const queues = [[], []];
    const canceled = [false, false];
    let done = false;
    let error = null;
    let pumping = null;
    const pump = () => {
      if (pumping !== null) return pumping;
      pumping = reader.read().then(step => {
        pumping = null;
        if (step.done) {
          done = true;
          return;
        }
        if (!canceled[0]) __tbArray.push(queues[0], step.value);
        // `Response.clone()` and `Request.clone()` expose the second branch as
        // a structured clone, as WPT requires for script-visible chunks
        // (<https://streams.spec.whatwg.org/#readable-stream-tee>).
        if (!canceled[1]) __tbArray.push(queues[1], structuredClone(step.value));
      }, reason => {
        pumping = null;
        error = reason;
        done = true;
      });
      return pumping;
    };
    const cancelBranch = (index, reason) => {
      if (canceled[index]) return Promise.resolve();
      canceled[index] = true;
      queues[index].length = 0;
      if (canceled[0] && canceled[1] && !done) return reader.cancel(reason);
      return Promise.resolve();
    };
    const branch = index => new globalThis.ReadableStream({
      pull(controller) {
        const step = () => {
          if (error !== null) {
            controller.error(error);
            return;
          }
          if (queues[index].length > 0) {
            controller.enqueue(__tbArray.shift(queues[index]));
            return;
          }
          if (done || canceled[index]) {
            controller.close();
            return;
          }
          return pump().then(step);
        };
        return step();
      },
      cancel(reason) { return cancelBranch(index, reason); },
    });
    return [branch(0), branch(1)];
  }
};
Object.defineProperty(globalThis.ReadableStream.prototype, Symbol.toStringTag, { value: 'ReadableStream', writable: false, enumerable: false, configurable: true });
const __tbStreamDisturbed = stream => __tbStreamBrand(stream).disturbed;
// A closed stream already holds every chunk; draining it avoids one promise
// turn per empty chunk.
const __tbDrainClosedStream = stream => {
  const data = __tbStreamBrand(stream);
  if (!data.closed || data.errored !== null) return null;
  data.disturbed = true;
  const chunks = data.chunks;
  data.chunks = [];
  return chunks;
};
globalThis.WritableStream = class WritableStream {
  constructor(sink, strategy) {
    const sinkObject = sink === undefined ? {} : Object(sink);
    let resolveClosed;
    let rejectClosed;
    const closedPromise = new Promise((resolve, reject) => {
      resolveClosed = resolve;
      rejectClosed = reject;
    });
    closedPromise.catch(() => {});
    const data = {
      sink: sinkObject, writer: null, closed: false, closing: false, errored: null,
      queue: Promise.resolve(), closedPromise, resolveClosed, rejectClosed, controller: null,
    };
    data.controller = {
      error(error) {
        if (data.closed) return;
        data.closed = true;
        data.errored = error;
        data.rejectClosed(error);
      },
      get desiredSize() { return data.closed ? 0 : 1; },
    };
    __tbWritableData.set(this, data);
    if (__tbGetOwnPropertyDescriptor(sinkObject, 'start') !== undefined
        && typeof sinkObject.start === 'function') {
      try {
        const result = sinkObject.start(data.controller);
        if (result !== undefined && result !== null && typeof result.then === 'function') {
          result.catch(error => data.controller.error(error));
        }
      } catch (error) {
        data.controller.error(error);
      }
    }
  }
  get locked() { return __tbWritableBrand(this).writer !== null; }
  getWriter() {
    const data = __tbWritableBrand(this);
    if (data.writer !== null) throw new TypeError('The stream is locked to a writer');
    const writer = {
      write: chunk => {
        if (data.closed || data.closing) return Promise.reject(data.errored === null ? new TypeError('The stream is closed') : data.errored);
        if (typeof data.sink.write !== 'function') return data.queue;
        data.queue = data.queue.then(() => data.sink.write(chunk));
        return data.queue;
      },
      close: () => {
        if (data.closed || data.closing) return Promise.reject(data.errored === null ? new TypeError('The stream is closed') : data.errored);
        data.closing = true;
        data.queue = data.queue.then(() => typeof data.sink.close === 'function' ? data.sink.close() : undefined);
        data.queue.then(() => { data.closed = true; data.resolveClosed(); }, data.rejectClosed);
        return data.queue;
      },
      abort: reason => __tbWritableAbort(data, reason),
      releaseLock: () => { data.writer = null; },
      closed: data.closedPromise,
      get ready() { return data.queue.then(() => undefined); },
      get desiredSize() { return data.closed ? 0 : 1; },
    };
    data.writer = writer;
    return writer;
  }
  abort(reason) {
    return __tbWritableAbort(__tbWritableBrand(this), reason);
  }
};
const __tbWritableAbort = (data, reason) => {
  if (data.closed) return data.queue;
  data.closed = true;
  data.closing = true;
  data.errored = reason;
  data.queue = data.queue.then(
    () => typeof data.sink.abort === 'function' ? data.sink.abort(reason) : undefined,
    () => typeof data.sink.abort === 'function' ? data.sink.abort(reason) : undefined,
  );
  data.queue.then(() => data.rejectClosed(reason), () => data.rejectClosed(reason));
  return data.queue;
};
Object.defineProperty(globalThis.WritableStream.prototype, Symbol.toStringTag, { value: 'WritableStream', writable: false, enumerable: false, configurable: true });
