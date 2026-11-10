# Concept owners

Where each domain concept is parsed, validated, and decided. A second copy is a
bug: the fix deletes it and adds a guard to `tools/check` when a grep can see
it (#239).

| Concept | Owner | Other layers take |
| --- | --- | --- |
| Browsing-context names | `renderer::protocol::ContextName` | `Option<ContextName>`; `""` is not a name |
| Target names and keywords | `renderer::protocol::TargetName` | the type; never a `_self`/`_blank` spelling |
| Serialized origins | `renderer::protocol::SerializedOrigin` | `Origin`/`Site` after parsing |
| Origin to URL | `net::origin_url` (`crates/net/src/lib.rs`) | the `Url`, not a string |
| Site for an origin | `net::site_for_origin`, `cookies::site_for_origin` | the site string |
| Referrer policy | `net::referrer` | the enum |
| Header grammar and request methods | `net::protocol`, `net::cors` | `http`/`net` types |
| Sandbox flags | `renderer::protocol::SandboxFlags` | the bit set |
| Enumerated attributes | `enumerated!` (`crates/enumerated`) | generated `parse`/`Deserialize` |
| Secure context | `net::secure_context` | the computed bit |
| Wall/monotonic time | `net::Clock`/`ClockSource` | an injected clock |
| Fetch policy | `net::Agent` with `net::Transport` | the transport only feeds bytes (#233) |
| HTTP cache | `crates/browser/src/network.rs` | the browser's response store |
| Per-origin key/value rows | `crates/browser/src/storage.rs` (`StorageAreas`) | `StorageKind` plus opaque encoded values |
| IndexedDB databases and records | `crates/renderer/src/js/scripts/web/indexeddb.js` | the `indexeddb` area through the storage seam |
| Carrier dial adapter | `crates/dial` | one conversion from `DialRequest` to `net::Request` |
| WPT timeout policy | `tools/wpt/timeouts.py` | flags from `cli.py` |
| WPT test selection | `tools/wpt-harness/src/suite.rs` (manifest) | — |
| WPT response metadata | `tools/wpt-harness/src/{services,wrapper,template}.rs` | — |
| WPT report shape | `tools/wpt-report` | — |
