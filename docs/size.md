# Binary size

Method: `nix develop -c cargo build --release` (dynamically linked glibc Linux
build; there is no post-link step — the shipped static musl release artifact
with `--features vendored-tls` adds vendored OpenSSL and lands around ~17 MiB),
then `size`, `nm -C --print-size --size-sort`, and `cargo bloat`. Any codegen or
profile change is gated on a WPT parity run
(`tools/wpt/cli.py run webdriver --score dom/nodes/`) that must match the previous
pass/unexp/timeout/crash counts, and a release smoke via
`tools/wpt/cli.py run webdriver --binary <release binary>`.

## Ledger

| step | `cargo build --release` |
| --- | --- |
| start: stable, `panic = "abort"`, no build-std, `.eh_frame` stripped post-link | 12.19 MiB |
| funnel codegen (shared conversion funnels, one `with_reactions` core) | 12.07 MiB |
| nightly + `[unstable] build-std` (std at `opt-level = "z"`) | 11.86 MiB |
| `panic = "immediate-abort"` (interim) | 10.94 MiB |
| `force-unwind-tables=no`, `tools/release` deleted (interim, immediate-abort) | 9.79 MiB |
| panic = "abort" + synchronous panic hook for diagnostics | 10.61 MiB |
| postcard renderer IPC control payloads | 10.47 MiB |
| shared interface installer funnel | 10.43 MiB |
| `publicsuffix` replaces the hand-rolled matcher, list refreshed (12,138,648 B) | 11.58 MiB |
| fork sync (blitz, html5ever, rquickjs upstream) at a6eab97b | 11.84 MiB |
| current: nightly 2026-11-06 + cargo update; `sha2` 0.11 dedupes `digest` | **11.76 MiB** |

The generated bindings emitted the same six-line installer for all 90
interfaces — look up the constructor the web shims created, take its prototype,
add members, add constants to the interface object as well as the prototype,
mark the unscopables — inlined into every caller at roughly 590 B apiece. One
`#[inline(never)]` funnel (`host::install_interface`) holds that body now: the
install family fell from 183 KiB to 136 KiB and `.text` by 47 KiB. Generated
modules also stopped importing `Object`: the three places that name it spell
`rquickjs::Object`, so the import list is fixed rather than conditional on which
pieces a given interface emits.

The renderer channel is the only wire we own, and both ends are the same
binary, so JSON's field names and string escaping bought nothing: the derived
`serde_json` impls for `Frame`, `ServiceCall`, `Command`, `StorageCall`,
`ServiceReply`, and friends were 100 KiB of `.text` (one enum's deserializer
alone was 19.5 KiB). Postcard encodes the same derives as varint tags and
positional fields, so `.text` fell 126 KiB (8,556,052 → 8,427,016 B) and every
command and renderer event stopped paying a JSON parse. The frame header, body-chunk lane, and
control-size cap are unchanged; `RemoteValue::Number` lost its JSON-only
`serde(with)` shim with it, since postcard carries `f64` natively. One
validation had to be rebuilt rather than inherited: `postcard::from_bytes`
discards whatever follows the message, where `serde_json::from_slice` rejected
trailing characters, and a frame's length is the payload's exact extent, so
`decode_control` now decodes through `take_from_bytes` and rejects a remainder
(1.4 KiB of `.text` for the check and its message).

`panic = "abort"` costs 0.82 MiB over `immediate-abort` (10.61 vs 9.79 MiB
measured): the panic formatting machinery from every dependency. It buys the
panic record's message and location, which `immediate-abort` cannot produce at
all (std aborts before the hook). `panic = "unwind"` costs a further 2.98 MiB
(13.59 MiB) and is rejected; it is the only way rquickjs could catch a panic
in a JS callback (see `docs/upstream.md`).

## The wasm component

The browser component is a different artifact with a different profile, so it
gets its own number: `cargo build -p tinybrowser-wasm --target wasm32-wasip2
--release` produces a component of 12,388,542 B (11.81 MiB). It carries the
renderer, QuickJS, the font stack, and the fetch core, but no hyper, no TLS, and
no DNS: the host moves the bytes. `jco transpile` then emits that component plus
a 415 KiB JS driver, which is what a page loads.

## Where the bytes are (`.text`, ~8.4 MiB at 12.07 MiB total)

| area | size | note |
| --- | --- | --- |
| `style` (stylo) | 1.3 MiB | upstream structure |
| `blitz_dom` (layout) | 892 KiB | includes taffy grid/flex algorithms |
| `std` | 590 KiB | built in-tree at the profile's flags |
| `browser` (our async plumbing) | 576 KiB | `webdriver::dispatch`, `coordinator_loop`, `cdp::serve_upgraded` |
| `renderer::js` | ~840 KiB | bindings glue (~200 B/member after funnels) plus handwritten DOM logic inlined into dispatch |
| `usvg` / `harfrust` / `skrifa` / `tiny_skia` | 295 / 258 / 227 / 180 KiB | format and raster dependencies |

## Experiments measured flat (do not retry)

- `psl` 2.1.246 (compiled-in suffix trie) instead of `publicsuffix` over the
  text list: 12,131,240 → 12,652,664 B, **+521,424 B** for the same answers.
  The text snapshot plus the crate's matcher costs 7,408 B (12,131,240 →
  12,138,648 B), so the crate form wins by 0.50 MiB. `psl` would refresh with
  `cargo update`, but the list is data and the matcher is the only code worth
  sharing; `tools/update-public-suffix-list` owns the refresh instead.
- Replacing `serde_json` in the protocol layer (`cdp`, `webdriver`, `server`).
  Measured in a standalone probe with the same flags (`opt-level = "z"`, fat
  LTO, `codegen-units = 1`, `panic = "abort"`), same workload, byte-identical
  output: `serde_json` costs 29.5 KiB over a no-JSON baseline, a hand-rolled
  DOM 37.3 KiB, a direct writer (no response tree) 35.7 KiB, and `miniserde`
  23.4 KiB. miniserde is the only winner, at ~6 KiB of library plus ~100 B per
  `json!` site, so ~15-25 KiB (0.2%) for rewriting 259 `Value` references and
  149 `json!` sites across the three crates — not worth it. The hand-rolled
  variants lose on float tables (`core`'s `POWER_OF_FIVE_128` 10.4 KiB plus
  `ryu`'s `DOUBLE_POW5_*` 10.7 KiB) that `serde_json` avoids with its own 2.5
  KiB `POW10` table and `zmij`. The per-type codegen that JSON used to cost is
  already gone: the binary holds 13.8 KiB of derived `serde` impls in total.
- `#[inline(never)]` on generated dispatch groups: flat (outlining relocates code).
- `#[inline(never)]` on conversion helpers alone: flat (arms still inlined their own copies).
- Link-time `--icf=all`: flat (no identical function bodies to fold).
- Dictionary deduplication: 12 structs, 10 unique (`FocusOptions` ×3); negligible.
- Stub probe (all generated arm bodies replaced with `unreachable!()`): 0.87 MiB of `.text` per the earlier 12.19 MiB base, most of it handwritten DOM logic inlined into dispatch monoliths, not glue. That is the ceiling for codegen work, not a target.
