# Tinybrowser

We are building the smallest, lightest, and fastest headless browser for AI agents.
Maximize web spec conformance per byte.

- WHATWG specs are ground truth. Start every implementation and review from the spec. Follow exact algorithm order and cite the spec URL at the implementation site.
- Heavily reference mature engines. Read Chromium/Firefox source code (see `docs/engines.md`). Never clone the repositories. When the spec and a shipped browser disagree, state the disagreement in a code comment and follow the browser that matches the spec algorithm.
- Breaking changes, churn, and rewrites are always welcome. When code fights you, zoom out and fix the architectural design. Do not work or patch around it.
- Simplicity is not brevity. A naive short version that skips edge cases is simplistic, not simple. Write the obvious version, even when it takes more lines, and never drop a spec branch or an error path to keep a change small.
- No lint silencing. Fix the underlying design flaw instead of suppressing compiler or lint diagnostics.
- Handwritten `unsafe` is prohibited unless runtime safety is proven (`// SAFETY:` invariant, Miri/valgrind checks via `tools/check`) and no safe alternative is as simple, clear, or fast.
- Let upstream bugs fail honestly. Never build tinybrowser workarounds for upstream engine bugs; record gaps in `docs/upstream.md`.
- Keep feedback loop fast. Cargo tests cover tinybrowser internals only (`cargo test --workspace`); never test web-platform spec conformance in cargo tests. Run the smallest slice that answers the question (single files or directories, never full suites). Never run long or whole-suite test runs without explicit approval.
- A WPT timeout is a failure. The harness budget (`tools/wpt/timeouts.py`) is the speed standard, so a test that cannot finish inside it is too slow and is a work item. Profile and fix the slow path; never raise a budget to hide one.
- Commit messages should also record decisions.
- Throwaway experiments belong in `/tmp/`.

## Disk

- Build outputs live in mbx's managed target directory: `target` is a symlink
  into `/mnt/ssd/mbx/targets` (`flake.nix` puts mbx's cargo wrapper first).
  mbx keeps the store and the managed targets under an 80 GiB budget and
  collects the least recently used.
- Never point builds at `$HOME`, `/tmp`, or any path on the root filesystem:
  builds are tens of GB and home is not scratch space. Do not set
  `CARGO_TARGET_DIR` either; that turns managed targets off and puts the
  outputs back in a directory nothing collects.
- If `/mnt/ssd` is full, or any build says `No space left on device`, let mbx
  reclaim before anything else:

      mbx gc --dry-run   # preview
      mbx gc             # collect to the budget, and past it when the disk is low

  `cargo clean` only removes this checkout's target. A cold rebuild is cheaper
  than a full disk. Do not "fix" it by moving the target directory to home.
- Check `df -h /mnt/ssd` before a long build, test, or WPT run. Below ~10G
  free, run `mbx gc` first.

## Size and release

- Size is an important constraint, but never try to gain smaller size by removing feature(s).
- No binary packing. No compressor, stub, or self-extractor step (nothing UPX-like).
- The toolchain is pinned nightly (`rust-toolchain.toml`) for `[profile.release] rustflags` and in-tree std. Release invariants live in `Cargo.toml`/`build.rs` with their size reason: `panic = "abort"`, `force-unwind-tables=no`, `[unstable] build-std`. Changing one means re-measuring; see `docs/size.md`.
- Release never unwinds. Design for it: no cleanup may depend on `Drop` after a panic, never use `catch_unwind` on a production path, and a child process must not outlive its parent (`crates/browser/src/child/mod.rs` exits on channel EOF).
- A panic must leave a record. The panic hook writes synchronously to the log file before the abort (`crates/logging/src/crash.rs`); the async log queue is lost on abort. Keep crash-path code panic-free.

## Method

- Measure the ceiling before designing. Prove what a change can win (stub the code out, read `.text`, count call sites) before writing it.
- Record flat experiments in `docs/size.md`. Negative results are results, so nobody retries them.
- Data over code. Generated glue becomes tables plus shared funnels: monomorphize per IDL type, never per member.
- Parity gates every refactor. WPT before/after must match exactly (same pass/unexp/timeout/crash counts), and any release-binary change gets a WPT smoke run via `--binary`.
- Delete stale config when its reason expires. Workarounds are removed with the reason recorded in the commit, not left to rot.
- Forks are for correctness, not micro-wins. A fork change needs a spec or crash reason recorded in `docs/upstream.md`; compile or size tricks stay upstream.

## Types and boundaries

- The browser is the trust boundary. It decides from state it owns; a
  renderer message is a claim to check, never an answer.
- Parse once into a type. Never stringify a typed value to parse it again, and
  a type with a parser deserializes through that parser at the wire.
- Design types so invalid states cannot exist: `Option<ContextName>` is the
  only "no name", never `""`.
- No `_ =>` on a closed enum. A missing answer is an error, not a default.
- A change that makes "one X" deletes every other X in the same change, fork
  included, and adds a guard to `tools/check` when a grep can see it.
- Prefer the upstream crate or spec table over a hand copy; when a copy stays,
  record the reason in `docs/upstream.md`.
- A test names the mutation that makes it fail, and the commit says it was
  checked.

## Pointers

- **Concept owners**: Read `docs/owners.md` before adding a parser, table, or
  second copy of a concept; it names the one module each belongs to.
- **The gate**: `tools/check clippy` runs on every push
  (`.github/workflows/check.yml`) and is what a local change must pass.
  `tools/check` uses `nix develop` when available and the installed toolchain
  otherwise, so the same script runs in CI.
- **Web API bindings & IDL**: Read `docs/bindings.md` for JS/Rust ownership, WebIDL contracts, and platform objects.
- **Workers**: Read `docs/worker-threads.md` before touching worker realms; it records the thread model, what is shared across threads, and the milestone order.
- **Engine behavior**: Read `docs/engines.md` to search and fetch Chromium/Firefox sources without cloning.
- **Engine forks & submodules**: Read `third_party/README.md` for submodule maintenance, fork branches, and update workflows.
- **Linked worktrees**: Run `tools/worktree-init` after `git worktree add`. It shares submodule objects with the canonical checkout and leaves WPT to the harness's canonical-checkout fallback; see `third_party/README.md`. Do not initialize a linked worktree with a bare `git submodule update --init --recursive`; it clones object stores and unused upstream test fixtures again.
- **WPT & conformance harness**: Read `tools/wpt/README.md` for test runners, scoring, and failure diffing. Check `.github/workflows/wpt.yml` runs (`gh run list --workflow=wpt.yml`) against `docs/progress.md` for regressions.
- **Upstream gaps, priorities, snapshot**: See `docs/upstream.md`, `docs/priorities.md`, `docs/progress.md`.
