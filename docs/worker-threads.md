# Worker threads

Dedicated workers run as documentless realms on their own OS threads, each with
its own `QuickJS` runtime. That is what the HTML worker model needs in three
places that a same-thread realm cannot satisfy:

- **Parallel execution.** A worker starts and runs while the page keeps
  running. `Worker-creation-happens-in-parallel.https.html`,
  `Worker-postMessage-happens-in-parallel.https.html`, and
  `postMessage_block.https.html` spin or block on `Atomics` on the page thread
  and require the worker to make progress meanwhile.
- **Forcible termination.** `terminate()` must stop a worker that is inside an
  infinite top-level loop. `Worker-terminate-forever-during-evaluation.html`
  and `Worker_terminate_event_queue.htm` hang when the page cannot run its own
  script while the worker's loop owns the thread.
- **Shared memory.** The two parallelism files post a `SharedArrayBuffer`, and
  the clone codec must keep it shared across runtimes.

Shared workers still run in the page engine (milestone 6). This document
records the design, what landed, and what is left.

## Measured: two runtimes, one buffer, two threads

The transport for `SharedArrayBuffer` across runtimes is
`JS_NewArrayBuffer(ctx, bytes, len, max_len, null, null, is_shared=true)` plus a
shared allocator. A fork test
(`third_party/rquickjs/core/src/runtime/shared_buffer.rs`) creates a
`SharedArrayBuffer` in one runtime, takes a lease on its backing block, hands
the lease to a second runtime on a second OS thread, wraps the same bytes, and
stores through `Atomics`. The first thread's spin sees the byte. The bytes are
genuinely shared and the second runtime's `Atomics` and typed-array views
operate on the same memory.

This is required, not optional: `testharness` has no other way to observe
cross-thread progress, and WPT's `createBuffer` fixture hands the same buffer
to both sides.

## Thread model

One dedicated worker thread owns:

- its own `QuickJS` `Runtime`, created on that thread, with a fresh
  `SharedJsRuntime`;
- its own `Context`, `World`, `JsRealm`, task queue, and timers;
- a `Receiver<WorkerCommand>`.

The engine keeps a `WorkerHandle` per worker in `WorkerDirectory`:

- the command `Sender`;
- an `Arc<AtomicBool>` terminate flag the worker's interrupt handler reads;
- an `Arc<AtomicUsize>` `active` count: commands the thread has accepted but
  not finished, plus one while its top-level script runs;
- an `Arc<AtomicBool>` `closed` flag and an `Arc<AtomicUsize>` pending-fetch
  count;
- the timer mirror below.

Commands (engine → worker): `Post`, `PortMessage`, `PortClosed`, `Broadcast`,
`FireTimer`, `NestedMessage`, `NestedError`, `NestedLoadFailure`, `Terminate`.

Events (worker → engine) are the existing `WorkerRequest` variants
(`PostToParent`, `BroadcastPost`, `Error`, `LoadFailed`, `SharedLoadFailed`,
`Create` for a nested worker, plus `SetTimer`/`ClearTimer`/`Close`). They land
in a process-wide `Arc<Mutex<Vec<WorkerRequest>>>`, and a worker thread wakes
the engine through the existing `Notify` after pushing one.

Creation happens inside `new Worker()`, not at the engine's next reconcile: the
parallelism tests post and then spin in the same synchronous function, so a
deferred start can never win
(<https://html.spec.whatwg.org/multipage/workers.html#dom-worker>). Spawning a
thread while the page's own runtime is executing is safe because the worker has
its own `Runtime`; the old "QuickJS cannot create a second realm" restriction
was runtime-local.

Termination is a flag the worker's interrupt handler reads:

```text
should_interrupt = terminate_flag.load() || stop.is_set() || now >= deadline
```

The handler is installed once on the worker thread, so `terminate()` aborts a
top-level infinite loop without the page ever entering the worker's realm.
`#report-the-exception` is skipped for that interrupt, as `worker.rs` already
does for the page's turn deadline.

## Timers and the virtual clock

The in-process harness pauses `tokio` time and advances it when the engine has
nothing ready. `tokio::time::Instant` is per-runtime, so a worker thread cannot
schedule against the page engine's clock. Worker timers therefore live in the
engine:

- the worker realm forwards `setTimeout`/`clearTimeout` as
  `WorkerRequest::SetTimer`/`ClearTimer`;
- `WorkerHandle` keeps the timer, and `next_deadline()` includes it;
- when due, the engine sends `WorkerCommand::FireTimer` and the worker realm
  runs the callback at its next task.

The engine's `has_ready_work` includes "any dedicated worker's `active` count
is nonzero, any timer is due, or any worker event is queued", so the harness
does not declare the page idle while a worker thread is still working.

Fetch completions arrive on the worker's own channel, not the command channel.
While `pending_fetches > 0` the thread polls the command channel with a 2 ms
timeout and drains fetches between polls, and the engine keeps draining on
`has_pending_dials`.

## Milestones

1. **Thread-safe `Shared`.** Done. `Shared` (frame tree, `MessagePort` table,
   delivery queue) is `Arc<Mutex<Shared>>` behind the same `borrow()` shape.
   Gate: `workers/` unchanged at 886/966 subtests and 218 perfect files.
2. **Process-wide versus thread-local `FrameRuntime`.** Done as part of
   milestone 3: the process-wide handles (services, identity, wake, stop,
   shared state, worker directory, object-URL store, shared-buffer table, fonts)
   are cloned into each worker's `FrameRuntime`; documents, registry, pending
   storage, turn deadline, and the request queue are per thread.
3. **Dedicated worker thread.** Done. Command/event channels, terminate flag,
   engine integration, creation inside `new Worker()`, nested-worker routing,
   Blob URL Store moved to process-wide state so a page-created `blob:` URL is
   fetchable from a worker thread.
4. **Timers across threads.** Done. The engine owns worker timers and fires
   them as commands, because the paused harness clock is per-runtime.
5. **Shared memory.** Done. The fork installs reference-counted
   `JS_SetSharedArrayBufferFunctions` hooks on every runtime, and
   `SharedBufferLease` keeps a block alive across runtimes. `messaging.js`
   encodes a `SharedArrayBuffer` as a table id and the receiver wraps the same
   bytes instead of copying them.
6. **Shared workers.** Not started. A shared realm still runs in the page
   engine; its identity is shared by several creators and the engine owns that
   table.
7. **Nested workers.** The routing is in place (`NestedMessage`,
   `NestedError`, `NestedLoadFailure`), and a worker thread's `new Worker()`
   spawns a thread through the same directory.

Gate after milestones 3–5: `workers/` is **972 subtests, 897 passing, 223
perfect files**, with 15 failing, 9 timeout, and 1 error file. That is +6
subtests, +11 passing subtests, +5 perfect files, and zero lost subtests
against the 886/966 baseline. The gains are
`Worker-creation-happens-in-parallel`, `Worker-postMessage-happens-in-parallel`,
`postMessage_block`, `Worker-terminate-forever-during-evaluation`,
`Worker_terminate_event_queue`, and `workers/semantics/xhr/001.html` (the fetch
poll above).

Gate after popups: `workers/` is **974 subtests, 933 passing, 227 perfect
files**, with 15 failing, 0 timeout, and 6 error files, and zero lost
subtests. The popup work gained 36 subtests: the four `extendedLifetime`
files, `data-url-shared`, the four module CSP/referrer files (now running
instead of timing out), and two `shared-worker-partitioned-cookies`
assertions. `FileAPI/url/url-charset.window.html` also gained two subtests
through the same pending-write replay. `webmessaging/` stayed at 187/232
passing subtests.

Gate after same-site cookies: `workers/` is **974 subtests, 939 passing, 233
perfect files**, with 14 failing, 0 timeout, and 1 error file, still with
zero lost subtests. The harness answers `/resources/testdriver-vendor.js`
with a `document.cookie`-backed `get_all_cookies`, and the `SharedWorker`
`sameSiteCookies` option now resolves `all`/`none`/default against the
creator's first-party site context (its site must match every ancestor's)
and filters the worker's script, module graph, `fetch`, XHR, and `Set-Cookie`
storage through the jar. All six `workers/same-site-cookies/` files pass.
`cookies/` and `webmessaging/` are unchanged.

Gate after module CSP and referrer: `workers/` is **974 subtests, 961 passing,
237 perfect files**, with 10 failing, 0 timeout, and 1 error file, still no
subtest lost. The harness applies `header(...)` pipes on `.sub.` paths as well
as their body expansion, which is what makes the cross-origin referrer cases
observe a policy at all. A module fetch now names the declaring module (or the
worker's own script for a dynamic import) as its referrer, a worker's static
imports are `worker-src` requests (`worker-src` → `child-src` → `script-src`
→ `default-src`, never `script-src-elem`), and a worker's own script response
supplies the referrer policy and CSP list for the dynamic imports its body
runs while static imports keep what it inherited. Both module referrer files
are 15/15 and both module CSP files are perfect.

Gate after CHIPS: `workers/` is **974 subtests, 963 passing, 239 perfect
files**, with 9 failing, 0 timeout, and 0 error files, still no subtest lost,
and `cookies/` moved from 331 to 335 passing subtests and 40 to 41 perfect
files. The cookie jar stores a `Partitioned` cookie under the site of its
top-level document, requires `Secure`, and includes the key in the cookie
identity, so two partitions hold the same name independently. `document.cookie`,
every dial, and a worker's script and module graph carry the key: a realm takes
it from its top-level origin, a top-level navigation uses the destination site,
a nested frame uses the initiator's, and a worker inherits its creator's. A
`SharedWorker` is now keyed by its constructor storage key (origin plus
top-level site) and its name, so each top-level site gets its own realm.
`shared-worker-partitioned.tentative.html` and
`shared-worker-partitioned-cookies.tentative.https.html` both pass.

Two event-dispatch bugs surfaced on the way. A realm's `first_party` verdict
was recomputed from the frame tree only on the engine's own commit paths, so a
document committed by the embedded renderer kept its `about:blank` verdict; the
verdict now folds the parent's stored verdict and site at creation and is
recomputed whenever the realm's origin changes. A `body`/`frameset` element
mirrors the Window-reflecting handler set (`onload` included), so
`document.body.onload = …` now runs on load. Window handler dispatch reads the
registration slot rather than the property, so a global `function onload() {}`
declaration no longer runs on every load, which is what Chrome does.

## What the remaining failures need

## What the remaining failures need

Popups exist now: a host with no tab carrier (the in-process harness) has the
renderer host an auxiliary context, so the `extendedLifetime`,
partitioned-cookie, and module CSP/referrer files run. The remaining worker
failures are implementation gaps, not the thread model:

- Joint session history is no longer a worker gap: a standalone engine keeps
  the steps itself (a child navigation or `pushState()` adds one, and a step
  moves every frame whose entry differs), so
  `workers/semantics/navigation/001.html` passes. It records no main-frame
  entry, so a step that would move the main frame is refused; a browser-hosted
  tab actor owns the full list.
- Partitioned cookies: the cookie jar keys on the top-level site only, not
  Chrome's cross-site ancestor bit, so `cookies/partitioned-cookies/`
  `partitioned-cookies-a-b-a-embed` sees the top-level partition's cookie in a
  same-site embed that has a cross-site ancestor; `partitioned-cookies-top-level-redirect`
  still fails its redirect assertion.
- `data-url`/`data-url-shared`: `IndexedDB` is absent.
- Growable `SharedArrayBuffer`: construction and structured clone work, and
  the receiver keeps the sender's `maxByteLength`. `WebAssembly.Memory` now
  exists, so the WPT fixture can obtain its constructor, but the fixture then
  fails its own `assert_true(sab instanceof ArrayBuffer)`; a
  `SharedArrayBuffer` is not an `ArrayBuffer`, and Chrome fails the same
  assertion. `WebAssembly.Module`/`Instance` are absent, and quickjs-ng stores
  `byteLength` per object, so a `grow()` in one runtime is not visible through
  another runtime's object (`docs/upstream.md`).
- `ReadableStream` transfer is a codec gap, not concurrency. `eval(import())`
  in a blob- or data-URL module worker cannot resolve a relative specifier,
  because the URL standard gives those URLs no hierarchical path; Chrome
  rejects it the same way, and `docs/upstream.md` records the probe.
- `SharedWorker` concurrency: milestone 6.

## Registry lifetime

`SharedBufferTable` keeps one entry per distinct `SharedArrayBuffer` block for
the engine's lifetime, keyed by the block's address. A bounded strategy needs
to know when the last message naming an entry was delivered, which a broadcast
does not expose. One entry per block, not per post, keeps the growth bounded by
the number of distinct shared buffers a page creates.
