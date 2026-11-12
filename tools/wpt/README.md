# WPT harness

tools/wpt/cli.py is the whole harness: `python3 tools/wpt/cli.py --help`
lists its subcommands (`run in-process`, `run webdriver`, `run test262`,
`score`, `merge`, and the rest).
It needs a dev-shell Python, so call it through `nix develop --command`.
Builds the wpt-profile binary via cargo each run; --binary PATH uses a
prebuilt binary as-is.

    nix develop --command python3 tools/wpt/cli.py run in-process dom/nodes --jobs 8
    nix develop --command python3 tools/wpt/cli.py run webdriver dom/events/ --exclude=worker
    nix develop --command python3 tools/wpt/cli.py run webdriver --score dom/nodes/ -- --processes 4
    nix develop --command python3 tools/wpt/cli.py run test262 test/language --jobs 12

Paths and our flags first; literal -- starts upstream flags.
--exclude=worker skips Worker variants too. Prove with `... selftest`.

A linked worktree may leave third_party/wpt uninitialized: the harness
falls back to the canonical checkout (the first `git worktree list`
entry), and `check_pin` still compares that checkout's HEAD with the
worktree's gitlink. Use `--wpt-root` for a different pin;
`--allow-pin-mismatch` only when the difference is known to be harmless.

Tight loop: --score prints per-directory buckets; --save-report keeps
the wptreport; rerun feeds attention-tests back (TIMEOUTs excluded
unless --include-timeout); --dry-run lists the selection. profile
ranks tests by wall time with --report, or runs a path and prints the
browser's navigation/request timeline.

`run test262` is the native ECMAScript lane: tools/wpt-fast runs each `.js`
file in a fresh QuickJS context with the test262 harness, no HTML,
testharness.js, wptserve, or WebDriver. The suite is read-only; strict
variants are a `"use strict";` prefix added in memory. It emits a wptreport with
`--report FILE`; `--baseline FILE` diffs the next run against that report
and exits 1 on a regression. The report has no WPT metadata, so every
failure in it reads as unexpected to `cli.py`; it is a native baseline, not
a replacement for the full score. Module tests load through QuickJS's module
loader; import attributes (JSON/text) and `$262.agent` are not implemented
yet. On this machine the 53,640 test262 files take ~27 s wall across 12 jobs
(~2m25s CPU) versus 20,587 s under WebDriver.

`run in-process` is the testharness/crashtest/reftest lane: each file is
mounted in the real engine in the harness process, with no WebDriver
or renderer child. Requests are served from the checkout, the only replaced
resource is `/resources/testharnessreport.js` (a small reporter the host
polls; it also drops a repeated remote result, because a child frame's
messages can reach the parent both live and through testharness's
`getmessages` replay, and a second result would break its pending count),
and a paused Tokio clock jumps over timer waits, so a hung file costs
its harness budget instead of a browser round trip. Budgets match the
WebDriver lane's: 5 s, or 7 s for `<meta name=timeout content=long>`; both
lanes read the policy in `tools/wpt/timeouts.py`, and cli.py passes it to the
runner as `--normal-budget`/`--long-budget`.

Two request kinds are not files: a `.py` path is a wptserve handler, and an
absent path is a 404. `cli.py run in-process` starts the real wptserve on a
loopback port (`tools/wpt/sidecar.py`) and the transport forwards those
requests to it, so the handler runs with its request object, templates, and
stash. The sidecar binds an ephemeral port, prints it on stdout, and exits
when the harness closes its stdin. Without it a handler is a network error.

The selection comes from the checkout's `MANIFEST.json`, the same manifest
the WebDriver lane uses, so "a test" means what upstream says it means:
testharness, crashtest, and reftest items, with the URL and `timeout` the
manifest records. Multi-global tests run through wptserve's `.any.html` and
`.window.html` wrappers, rendered from the source's `// META:` lines.
Testdriver items belong to the WebDriver lane (`testdriver.js` needs a
session), so this lane always excludes them. `cli.py run webdriver
--exclude-in-process` runs exactly the complement: the testdriver files plus
the generated wrappers this lane cannot render (service worker, shared worker,
module, shadow realm).

`--total-chunks N --this-chunk K` splits by a hash of the test's source path,
which keeps every variant of one source together and spreads the work evenly
(the CI chunk counts are balanced by test count).

A reftest mounts the test, fires the harness's `TestRendered` event after
load, waits for `reftest-wait` to clear, and renders at 800x600. It then
mounts each reference the manifest lists and compares raw RGBA buffers
(`==`/`!=`, with the manifest's `fuzzy` bounds); PNG encoding and decoding
would be pure overhead when both sides are rendered here. This matches the
WebDriver lane's reftest executor on the same build: a 600-test random
sample agreed on 599, and the one difference was an APNG animation test that
the WebDriver lane times out and this lane fails.

The origin comes from the test's filename flags, as wptrunner's
`server_protocol` reads them: `.https.` and `.serviceworker.` tests mount at
`https://web-platform.test:8443` and `.h2.` at port 9000, so secure-context
tests see the context they were written for. A `.sub.` document (main page or
subresource) is expanded with the same template language and HTML escaping
wptserve uses, and `{{ports[...]}}` follows wptrunner's port table.

Against the WebDriver lane on the same build, a 2,195-test random sample
agreed on 94.9% of verdicts and passed 1,148 versus 1,150 tests; most
disagreements were one lane timing out where the other finished, plus files
that need a real browser session (popups, service workers), which are the
WebDriver lane's half of the split.

    nix develop --command python3 tools/wpt/cli.py run in-process dom/nodes --jobs 8

The lane has no result cache: every run executes the selection. It is fast
because the engine is in-process and the clock is paused, not because an
earlier verdict is reused. On dom/nodes/ (422 manifest tests) a run is ~11 s
at 12 jobs. A truncated file is never mistaken for a verdict: testharness
reports `interrupted` when the engine cuts a script short, and the runner
gives each file one drain with its whole budget instead of a short slice.

A file can end the process instead of returning a verdict: layout can recurse
until the stack overflows, a `QuickJS` assertion aborts, and a page can spin in
a drain that never yields. The harness writes a journal line before each file
and another with its verdict (`--journal PATH`), so `cli.py run in-process` runs
it again after such an abort, reports the finished files from the journal,
records the files that were in flight as ERROR (TIMEOUT when the watchdog cut a
file that stopped returning) and runs only the rest. Each attempt must finish
at least one more file, so a harness that dies before its first verdict fails
the run instead of looping. The in-process watchdog allows every file its
budget plus 30 s before it ends the attempt, which is far above the slowest
real file and below a stack overflow on the deepest page.

One process does not run a whole chunk. The engine keeps ~5 MB per file alive
(`docs/upstream.md`), so `cli.py` splits the selection into windows of 600 files
and starts a fresh harness process for each; the journal carries the verdicts
across windows, so a window boundary re-runs nothing and loses nothing. The
window is a memory bound, not a result cache: every file still runs exactly
once per invocation.

Known gap: an async listener that leaves a host object borrowed makes
rquickjs panic with `AlreadyBorrowed` in `Class::borrow_mut`; rquickjs
converts that panic into a script exception, so the run still finishes and
writes its report. It is an engine re-entrancy bug to fix, not a harness one.

The host serves files the way wptserve does: the content-type table comes from
`wptserve.constants` (cli.py hands it over), headers are `wptserve`'s
`load_headers` order (`__dir__` then the file, `.sub.headers` before
`.headers`, templates expanded), and `.any.html`/`.window.html` documents are
rendered from their sources. `document.cookie` goes through the browser's own
jar (`crates/cookies`), and a document mounted from disk stores the
`Set-Cookie` fields of its `.headers` file the way a fetched navigation does.
The query-param `pipe` pipeline implements `header(Name, Value)` and `sub`;
`trickle` timing is ignored. A `.py` path is forwarded to a real wptserve
process (`tools/wpt/sidecar.py`). `domains[...]` accepts the configured
Unicode subdomain spellings and expands them to their IDNA form, as
`wptserve` does. Popups and WebDriver are out of scope: `window.open` returns
null and `testdriver.js` files belong to the WebDriver lane.

**The lane runs the real fetch core.** `FsServices` is a `NetworkHost` over
`net::Agent::with_transport`: response tainting, CORS and preflight, the
no-CORS request guards, redirect policy, `Sec-Fetch-*`, referrer selection, and
cookies all run in this process, and only the one HTTP exchange is an in-memory
transport that maps `web-platform.test`, `www1.web-platform.test`, `:8443`
and the other wptserve hosts to the checkout. A file that asserts a
cross-origin `fetch` fails with `TypeError`, or that a `no-cors` response is
`opaque`, now measures the same policy the native browser does, so those files
belong in the parity numbers. Opaque Response Blocking and the browser-side
HTTP cache still live in `crates/browser` and are not exercised here.

A conformance change is a failure-list diff: failures prints the
canonical list of a --score log, baseline runs both sides in a detached
worktree (exit 1 on new failures). Metadata-expected TIMEOUTs:
--skip-timeout in dev only; score/overnight refuse it. overnight needs
explicit approval. Update the docs/progress.md total after scoring.
Timeout policy (DEFAULT/LONG/EXTRA seconds) lives in tools/wpt/timeouts.py.

`merge --summary-out run.json` writes the compact record of a CI run: totals,
per-area counters, per-subtest outcome tallies, and the failure list with the
diagnostic message dropped. It comes from the same classifier as the printed
table, so the two cannot disagree. The 335 MB merged wptreport is a debug
artifact; the record is the hand-off, and `tools/wpt/verdict-cases.json`
pins its verdict rule against `tools/wpt-report`.

`corpus --revision REV --out corpus.json.gz` writes Chrome's per-test subtest
totals from wpt.fyi. `merge --corpus corpus.json.gz` fills the gap between
those totals and what the run reported with `subtest_unreported`, so the
denominator is the pinned revision's corpus and a test that never reports
counts as not passing. Without the map the denominator is only what reported.
The CI score job fetches the map and warns instead of failing when wpt.fyi is
unavailable.

`publish --record run.json --data DIR` turns that record into the files the
public dashboard reads: `latest.json` (the record without its failure list),
`index.json` (one entry per run, which is the history chart),
`regressions.json` (failure-list set difference against the previous run), and
`failures.jsonl.gz` (the list itself, since a full run has ~100k rows and a
JSON member that size would make every commit huge). It also gives every
index point at the current WPT revision the same corpus denominator, so an
older point does not show a fall that only changed the denominator. A WPT pin
bump makes the diff incomparable and it says so instead of reporting phantom
fixes. The `WPT` workflow runs this after a push to `main` and commits the
result to `tinybrowser-dev/tinybrowser-wpt-results`.

A test file whose own timers outlast DEFAULT_TIMEOUT cannot be sped up
from the engine: `referrer-policy/css-integration/svg/` sleeps 800 ms
per subtest (nine serially) before it checks anything. SLOW_TEST_PREFIXES
in tinybrowser_wpt.py gives those directories SLOW_TEST_TIMEOUT, still
below testharness.js's 10 s clock so the runner kill lands first. That
list is only for waits written into test code, never for a slow engine
path; an entry whose reason expired falls back to the default budget and
its files fail visibly.

Enabled: testharness (multi-window, same-site frames), crashtest,
reftest (screenshot compare, 800x600), test262 (generated wrappers),
HTTPS (--ssl-type=openssl; WSS unrun), testdriver (click/keys/cookies/
rect). wdspec/aamtest are red foundations (pytest not vendored).

Blocked on engine, not harness: Perform Actions (~660 files), user
activation bless (~340), Permissions/BiDi/Web Bluetooth (~350).
print-reftest is unregistered.

Conventions: metadata in tools/wpt/metadata; expected FAIL only when
understood. Cargo tests cover internals only.
