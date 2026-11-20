# Realm-free DOM layer and Rust/JS state ownership split

Code below `crates/renderer/src/js/` (`dom/`, `documents.rs`, `sniff.rs`) is forbidden from referencing `rquickjs`, `Ctx`, `Value`, `Persistent`, or `JsNode`. Rust owns shared browser state (DOM tree, parsing, layout, event dispatch, I/O, timers, frame lifetimes, origin checks), while embedded JavaScript shims own pure value algorithms behind a private host bridge that page-replaced methods cannot observe or mutate.
