# Worker threads

Dedicated and shared workers currently run as documentless realms in the page
engine: same `QuickJS` runtime, same thread, same event loop. That design
cannot match the HTML worker model in three places, and all three are WPT
failures:

- **Parallel execution.** A worker starts and runs while the page keeps
  running. `Worker-creation-happens-in-parallel.https.html`,
  `Worker-postMessage-happens-in-parallel.https.html`, and
  `postMessage_block.https.html` spin or block on `Atomics` on the page thread
  and require the worker to make progress meanwhile.
- **Forcible termination.** `terminate()` must stop a worker that is inside an
  infinite top-level loop. `Worker-terminate-forever-during-evaluation.html`
  and `Worker_terminate_event_queue.htm` hang today because the page cannot run
  its own script while the worker's loop owns the thread.
- **Shared memory.** The two parallelism files post a `SharedArrayBuffer`, and
  the clone codec refuses one because there is no shared-memory transport.

The fix is one worker realm per OS thread with its own `QuickJS` runtime. This
document records the design, what is already in place, and the milestones. It
is a plan, not a claim: each milestone lands only when the `workers/` slice
keeps its pass, timeout, and error counts.

## Measured: two runtimes, one buffer, two threads

The transport for `SharedArrayBuffer` across runtimes is
`JS_NewArrayBuffer(ctx, bytes, len, max_len, null, null, is_shared=true)`. A
throwaway test (reverted) created a `SharedArrayBuffer` in one runtime, took
its backing pointer with `JS_GetArrayBuffer`, handed the pointer to a second
runtime on a second thread, wrapped it with `JS_NewArrayBuffer(..., true)`, and
spun in JavaScript until the writer thread stored a byte. The reader saw the
byte. So the bytes are genuinely shared and the second runtime's `Atomics` and
typed-array views operate on the same memory.

This is required, not optional: `testharness` has no other way to observe
cross-thread progress, and WPT's `createBuffer` fixture hands the same buffer
to both sides.

## What is already in place

- `SharedHandle` is `Arc<Mutex<Shared>>` behind `borrow()`/`borrow_mut()`
  (`crates/renderer/src/messaging.rs`). `Shared` is plain data — the frame
  tree, the `MessagePort` table, and the delivery queue — so it is the only
  page state a worker thread touches. The lock is held for one operation and
  no site nests it.
- `Stop` and `wake` are already `Arc` (an atomic flag and a `Notify`), and
  `services` is an `Arc<dyn BrowserServices>`.
- Fetches already run on dial threads and return over a `Sender`; a worker
  thread changes nothing about that path.
- Worker creation is already deferred to the engine's reconcile, and messages
  already cross as serialized payload strings with endpoint ids.

## Thread model

One worker thread owns:

- its own `QuickJS` `Runtime`, created on that thread, with a fresh
  `SharedJsRuntime`;
- its own `Context`, `World`, `JsRealm`, task queue, and timers;
- a `WorkerCommand` receiver and a `WorkerEvent` sender.

The engine keeps a `WorkerHandle` per worker: the command sender, an
`Arc<AtomicBool>` terminate flag, and a `next_deadline` published by the
worker for the virtual clock.

Channels, not a shared task queue:

- **Engine → worker:** `Post`, `PortMessage`, `PortClosed`, `Connect`,
  `Broadcast`, `RunTimer`, `Terminate`, `Close`. Each command corresponds to
  the existing `WorkerTask` variant.
- **Worker → engine:** the existing `WorkerRequest` variants
  (`PostToParent`, `BroadcastPost`, `Error`, `LoadFailed`, `SharedLoadFailed`,
  `Create` for a nested worker). They land in a `Sender<WorkerRequest>` the
  engine drains in `adopt_worker_requests`, replacing the `Rc<RefCell<Vec<_>>>`.

Creation must happen inside `new Worker()`, not after the constructor's script
returns. The parallelism tests call `worker.postMessage(sab)` and then spin in
the same synchronous function, so a deferred start can never win. A thread can
be spawned while the page's own runtime is executing, because it has its own
`Runtime`; only the *realm* creation had to be deferred, and that restriction
is QuickJS-runtime-local.

Termination is a flag the worker's interrupt handler reads:

```text
should_interrupt = terminate_flag.load() || stop.is_set() || now >= deadline
```

The handler is installed once on the worker thread, so `terminate()` aborts a
top-level infinite loop without the page ever entering the worker's realm.
`#report-the-exception` must be skipped for that interrupt, as
`worker.rs` already does for the page's turn deadline.

## Timers and the virtual clock

The in-process harness pauses `tokio` time and advances it when the engine has
nothing ready. A worker thread's timers must therefore be visible to the
engine:

- the worker publishes its earliest timer deadline in an `AtomicU64` of
  nanoseconds since the runtime's start;
- a fired timer is a normal `WorkerEvent` that wakes the engine through the
  existing `Notify`;
- the engine's `has_ready_work` includes "any worker has a ready command,
  event, or due timer".

If `tokio`'s paused clock is not visible from a non-runtime thread, the
fallback is a clock owned by the engine and passed to workers (a shared
`Arc<Clock>` that `tokio::time::Instant` reads), so both sides advance
together. Measure that before choosing.

## Milestones

1. **Thread-safe `Shared`.** Done. Gate: `workers/` unchanged at 886/966
   subtests and 218 perfect files, no loss.
2. **Split `FrameRuntime`.** `FrameRuntime` currently mixes process-wide
   handles (`services`, `identity`, `wake`, `stop`, `shared`, `font_ctx`) with
   thread-local ones (`js_runtime`, `documents`, `registry`, `pending_storage`,
   `turn_deadline`, `worker_requests`, `next_worker`, `next_broadcast`). Split
   it into a process-wide part the engine and every worker thread clone, and a
   per-thread part each thread builds for itself (a fresh `RealmRegistry`,
   `DocumentStore`, and ids). Gate: parity.
3. **Dedicated worker thread.** Command/event channels, terminate flag, engine
   integration, and creation inside `new Worker()`. Gate: parity, then the two
   `happens-in-parallel` files and `postMessage_block`, which also need
   milestone 5.
4. **Timers across threads.** The virtual clock above. Gate: parity, then
   `Worker_terminate_forever-during-evaluation` and
   `Worker_terminate_event_queue`.
5. **Shared memory.** Replace the clone codec's `DataCloneError` for a
   `SharedArrayBuffer` with a transport that wraps the bytes in a new
   `JS_NewArrayBuffer(..., is_shared=true)` on the receiving runtime. The
   buffer table needs an `Arc`-owned allocation so both runtimes free it only
   when the last one drops it.
6. **Shared workers.** A shared realm lives on its own thread and receives a
   `Connect` command per connection; the realm key stays in the engine.
7. **Nested workers.** A worker thread's `new Worker()` forwards a `Create`
   request; the engine spawns the nested thread and routes messages.

## What threads will not fix

- `extendedLifetime`, same-site cookies, partitioned shared workers, module
  CSP/referrer: these need popups, which the in-process harness does not have.
- `data-url`/`data-url-shared`: `IndexedDB` is absent.
- Growable `SharedArrayBuffer`: QuickJS requires
  `JS_SetSharedArrayBufferFunctions` allocator hooks, independent of threads.
- `ReadableStream` transfer and `eval(import())` in a module worker: codec and
  QuickJS gaps, not concurrency.
