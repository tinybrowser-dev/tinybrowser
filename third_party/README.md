# third_party

Submodules for living forks we rebase; vendored copies for small frozen
deltas; snapshots for test input. Only the forks and vendored crates
ship in the binary.

## Linked worktrees

A linked worktree needs the build submodules but not a second copy of
their objects, and not the 1.2 GB WPT tree. After `git worktree add`,
run:

    tools/worktree-init

It initializes blitz, html5ever, rquickjs, and rquickjs's nested
sys/quickjs with the canonical checkout's clones as git alternates: the
worktree keeps its own checkouts and refs, while immutable objects are
read from the canonical store. It also sets
`submodule.third_party/wpt.update=none` in that worktree's config, so a
later `git submodule update --init` skips WPT (`--recursive` also pulls
upstream test fixtures the build does not use). The harness resolves the
canonical WPT checkout through `git worktree list` and still runs the
worktree's binary against the full suite. A branch that bumps the WPT pin
needs its own checkout plus `--wpt-root`.

Keep the canonical checkout: the alternates point into it. Initialize
worktrees with this script, not a bare `git submodule update --init`,
which clones each submodule's object store again.

## Forks (submodules, ship in binary)

- rquickjs + QuickJS-NG at third_party/rquickjs (nested sys/quickjs).
  [patch.crates-io] paths; own workspace, excluded from ours.
- blitz at third_party/blitz, branch master (main tracks upstream).
  [patch.crates-io] blitz-dom, blitz-html, blitz-paint, blitz-traits.
- html5ever at third_party/html5ever, branch master.
  [patch.crates-io] xml5ever only; builds against registry markup5ever.
- Update: pull upstream in the submodule, push the fork branch, bump
  the pointer here. Check pins with git submodule status. Fresh clones
  (canonical checkout): git submodule update --init --recursive.
- Patch scope: only make fork changes small and self-contained enough that
  pulling from upstream stays easy. Large architectural changes to forks stay
  upstream and belong in docs/upstream.md.
- Conflicts: upstream wins — take upstream's shape, re-apply our patch
  on top. The rule and its reason live in docs/upstream.md.

## Vendored crates (ship in binary)

- weedle4: sagudev/weedle4 at 7dd96ea plus our async_sequence grammar
  patch (WebIDL async-iterable). Build-only dep via
  crates/webidl-bindgen/Cargo.toml. Update: copy new tree over,
  re-apply the grammar patch, rebuild over all pinned extracts.
  Upstream tip is our pin (2026-10-15); recent commits are bot data syncs.
- fontconfig-parser 0.5.8 with roxmltree moved 0.20.0 -> 0.21.1
  (usvg/fontique already pull 0.21; one parser linked).
  [patch.crates-io] path. Update: copy new release over, re-apply the
  one-line bump, run crate tests + workspace build. Upstream inactive
  since 2025-05-12.

## Test data (never ships)

- WPT submodule at third_party/wpt. IDL extracts snapshotted to
  crates/webidl-bindgen/idl/ via tools/webidl/import (manifest pins
  revision + sha256; tools/check enforces). Never edit extracts to
  match code. Runner: tools/wpt/cli.py; parser gate:
  html/syntax/parsing/html5lib_*.html.
- Blink inspector-protocol snapshot at third_party/blink-cdp.
  Revision in REVISION file. Update: set REVISION, run
  tools/cdp-tests/update. Runner: tools/cdp-tests/run.
