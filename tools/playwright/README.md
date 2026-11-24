# Upstream Playwright gate

Runs Playwright's own `tests/page/` specs against tinybrowser over CDP and
compares every result with `pw-expectations.json`. A test missing from the
expectations must pass; a listed test must keep the recorded status.

## Setup

    node tools/playwright/upstream-setup.mjs

Clones `microsoft/playwright` at the tag pinned in the script into
`.playwright/`, applies `patches/`, and installs the screenshot baselines from
`snapshots/`. `--force` redoes the clone.

## Run

    cargo build --profile wpt -p tinybrowser
    node tools/playwright/upstream-run.mjs

The runner uses the `wpt` profile binary (or `TINYBROWSER_BINARY` when set).
The patched config budgets tests at 3 s with a 2 s navigation timeout, and only
an optimized build finishes inside them; a debug build reports correct tests as
timeouts.

Use `--spec <file>`, `--jobs <n>`, and `--timeout <ms>` to narrow a run.
`--update-expectations` rewrites `pw-expectations.json` from the run: passing
tests are removed, failing and skipped tests are recorded with their reason.

A test recorded as `SKIP` is excluded from the run with `--grep-invert` and
counted as expected-non-pass, never as a pass. Reserve it for a result that
cannot be gated deterministically (for example a timing race), and name the
issue that tracks the real fix in `reason`.

## Screenshot baselines

`snapshots/` mirrors the `tests/` tree and is copied into `.playwright/tests/`
by setup, which is where Playwright looks for `*-tinybrowser-page-linux.*`
goldens. Regenerate inside `.playwright/` with

    npx playwright test --config=tests/tinybrowser/playwright.config.ts --update-snapshots

then copy the changed `*-tinybrowser-page-linux.*` files back into
`snapshots/`.

A golden for a test listed as expected `FAIL` is the intended output (for a
missing feature, copy the matching Chromium golden), not a dump of the current
result. Playwright records a missing snapshot as passing under
`updateSnapshots: "missing"`, so generating one from broken output would turn
the known failure into a silent pass.
