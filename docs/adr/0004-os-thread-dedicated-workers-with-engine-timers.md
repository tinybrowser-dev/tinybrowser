# OS-thread dedicated workers with engine-owned timers

Dedicated workers run on their own OS threads with independent `QuickJS` runtimes, reference-counted `SharedArrayBuffer` leases (`SharedBufferTable`), and an atomic interrupt flag so `Atomics.wait` parallelism and `Worker.prototype.terminate()` on infinite loops work per spec. Because the WPT harness uses a paused per-runtime `tokio` clock, worker `setTimeout`/`clearTimeout` calls forward to the page engine and fire via `WorkerCommand::FireTimer`.
