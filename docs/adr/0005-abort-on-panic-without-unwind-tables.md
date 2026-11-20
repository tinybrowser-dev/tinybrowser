# Abort on panic without unwind tables

Release builds compile with `panic = "abort"`, `-C force-unwind-tables=no`, and in-tree `build-std` (`std`, `core`, `alloc`, `panic_abort` at `opt-level = "z"`), saving ~3.8 MiB over unwinding while keeping `panic = "abort"` over `immediate-abort` so a synchronous panic hook in `crates/logging/src/crash.rs` can flush the crash location before process exit. Production code never relies on `Drop` during a panic or `catch_unwind`, and child renderers exit on IPC channel EOF.
