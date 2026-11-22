# Contributing to tinybrowser

Thanks for your interest in `tinybrowser`!

## Guiding principles

- **Specs are ground truth.** Start from WHATWG / W3C specs, follow exact algorithm order, and cite the spec URL (`// https://...`) at the implementation site.
- **No prose comments in source code.** Only `// SAFETY:` invariants and spec/RFC URL citations are permitted in Rust/JS/Python code (enforced by `tools/check`).
- **Keep the binary small and fast.** Avoid adding heavy dependencies or workarounds for upstream engine bugs.

See [AGENTS.md](AGENTS.md) for the full set of engineering invariants and [docs/owners.md](docs/owners.md) for module ownership.

## Development setup

1. Clone with the required submodules:
   ```sh
   git submodule update --init --depth 1 third_party/blitz third_party/rquickjs third_party/html5ever
   git -C third_party/rquickjs submodule update --init --depth 1 sys/quickjs
   ```
2. Use the pinned nightly toolchain in [rust-toolchain.toml](rust-toolchain.toml) or enter the Nix shell with `nix develop`.
3. Run the workspace gate before opening a pull request:
   ```sh
   tools/check clippy
   cargo test --workspace
   ```
4. For CDP/Playwright changes, run the Playwright compatibility gate:
   ```sh
   tools/playwright/run internal-bridge.spec.js
   node tools/playwright/upstream-setup.mjs
   node tools/playwright/upstream-run.mjs
   ```

## Engine forks

Fixes for `third_party/blitz`, `third_party/html5ever`, and `third_party/rquickjs` live on the `tinybrowser-dev/*` forks and are documented in [docs/upstream.md](docs/upstream.md).
