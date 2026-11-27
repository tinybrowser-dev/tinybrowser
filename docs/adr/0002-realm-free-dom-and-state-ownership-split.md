# Realm-free DOM layer and Rust/JS state ownership split

The realm-free DOM layer (`crates/dom`: `dom/`, `documents.rs`, `sniff.rs`, `css/`, `xml.rs`, `csp.rs`, `protocol.rs`) is a separate crate with no `rquickjs` dependency, enforcing the boundary at compile time. Rust owns shared browser state (DOM tree, parsing, layout, event dispatch, I/O, timers, frame lifetimes, origin checks), while embedded JavaScript shims own pure value algorithms behind a private host bridge that page-replaced methods cannot observe or mutate.
