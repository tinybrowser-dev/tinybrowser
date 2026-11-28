"""WPT results orchestration: delegates verdict, merge, record, publish, and summary to `wpt-report`."""

from __future__ import annotations

import argparse
import gzip
import json
import os
import subprocess
import sys
import urllib.request
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
CORPUS_DIR = ROOT / "tools" / "wpt" / "corpus"

HARD_STATUSES = frozenset({"TIMEOUT", "CRASH", "ERROR"})
TIMEOUT_STATUSES = frozenset({"TIMEOUT", "EXTERNAL-TIMEOUT"})
WPT_FYI_RUNS = "https://wpt.fyi/api/runs?sha={revision}&max-count=100"
USER_AGENT = "tinybrowser-wpt/1"

CORPUS_USAGE = """corpus (--revision REV | --summary FILE) [--out FILE]

Write the per-test subtest counts of a Chrome run at a WPT revision as a
compact {test: subtests} map. `merge` adds the subtests a run never
reported from the artifact pinned for the run's WPT revision, so the score's
denominator is that revision's corpus rather than only the part that reached
the harness.

--revision REV asks wpt.fyi for the newest Chrome run at that revision and
downloads its summary_v2. --summary FILE reads that summary from disk, which
is what the selftest uses. --out defaults to tools/wpt/corpus/REV.json.gz
when --revision is used, which is the file CI reads for the pinned submodule;
the output is JSON, gzipped when FILE ends in .gz.
"""

PUBLISH_USAGE = """publish --record FILE --data DIR [--sha SHA]

Add one compact run record (from `merge --summary-out`) to the data directory
that the results repo publishes: latest.json, regressions.json, index.json, and
failures.jsonl.gz."""

SUMMARY_USAGE = """summary --data DIR

Print the regression summary from regressions.json written by `publish`."""


class _ArgParser(argparse.ArgumentParser):
    """ArgumentParser that raises ValueError instead of exiting the process."""

    def error(self, message: str) -> Any:
        raise ValueError(message)


def eprint(*args: object) -> None:
    print(*args, file=sys.stderr, flush=True)


def die(scope: str, message: str, code: int = 2) -> int:
    eprint(f"wpt: {scope}: {message}")
    return code


def pinned_corpus(revision: str) -> Path:
    return CORPUS_DIR / f"{revision}.json.gz"


def wpt_report_binary() -> Path:
    """Locate or build the single-owner `wpt-report` Rust binary."""
    override = os.environ.get("WPT_REPORT_BIN")
    if override:
        return Path(override)
    target_dir = Path(os.environ.get("CARGO_TARGET_DIR", str(ROOT / "target")))
    for profile in ("wpt", "debug", "release"):
        candidate = target_dir / profile / "wpt-report"
        if candidate.is_file() and os.access(candidate, os.X_OK):
            return candidate
    subprocess.run(
        ["cargo", "build", "-p", "wpt-report"],
        cwd=str(ROOT),
        check=True,
    )
    return target_dir / "debug" / "wpt-report"


def run_wpt_report(args: list[str]) -> int:
    binary = wpt_report_binary()
    proc = subprocess.run(
        [str(binary), *args],
        cwd=str(ROOT),
        capture_output=True,
        text=True,
    )
    if proc.stdout:
        sys.stdout.write(proc.stdout)
    if proc.stderr:
        sys.stderr.write(proc.stderr)
    return proc.returncode


def summarize(report: Path) -> int:
    return run_wpt_report(["summarize", str(report)])


def is_expected(entry: dict) -> bool:
    status = entry.get("status")
    if status in (entry.get("known_intermittent") or []):
        return True
    expected = entry.get("expected")
    if expected is None:
        return True
    return status == expected


def unexpected_subs(subtests: list) -> list[dict]:
    return [s for s in subtests if s.get("status") != "SKIP" and not is_expected(s)]


def needs_retest(result: dict, include_timeout: bool) -> bool:
    status = result.get("status", "ERROR")
    if status == "SKIP":
        return False
    if status in TIMEOUT_STATUSES:
        return include_timeout
    if status in HARD_STATUSES:
        return True
    if not is_expected(result):
        return True
    return bool(unexpected_subs(result.get("subtests") or []))


def failing_tests(report: Path, include_timeout: bool) -> list[str]:
    data = json.loads(report.read_text())
    return [
        r["test"]
        for r in data.get("results", [])
        if r.get("test") and needs_retest(r, include_timeout)
    ]


def rerun_status(remaining: int, runner_exit: int) -> tuple[int, bool]:
    if runner_exit != 0:
        return runner_exit, True
    if remaining:
        return 1, True
    return 0, False


def merge_main(argv: list[str]) -> int:
    parser = _ArgParser(prog="wpt merge", add_help=False)
    parser.add_argument("--out")
    parser.add_argument("--summary-out")
    parser.add_argument("--product-revision")
    parser.add_argument("--corpus")
    parser.add_argument("--strict-skips", action="store_true")
    parser.add_argument("reports", nargs="*")
    try:
        ns = parser.parse_args(argv)
    except ValueError as error:
        return die("merge", str(error), 2)
    if not ns.reports:
        return die("merge", "merge needs at least one report file", 2)
    cmd = ["merge", "--corpus-dir", str(CORPUS_DIR)]
    if ns.out:
        cmd.extend(["--out", ns.out])
    if ns.summary_out:
        cmd.extend(["--summary-out", ns.summary_out])
    if ns.product_revision:
        cmd.extend(["--product-revision", ns.product_revision])
    if ns.corpus:
        cmd.extend(["--corpus", ns.corpus])
    if ns.strict_skips:
        cmd.append("--strict-skips")
    cmd.extend(ns.reports)
    return run_wpt_report(cmd)


def publish_main(argv: list[str]) -> int:
    if any(arg in ("-h", "--help") for arg in argv):
        print(PUBLISH_USAGE)
        return 0
    parser = _ArgParser(prog="wpt publish", add_help=False)
    parser.add_argument("--record", required=True)
    parser.add_argument("--data", required=True)
    parser.add_argument("--sha", default=None)
    try:
        ns = parser.parse_args(argv)
    except ValueError as error:
        return die("publish", f"{error}\n{PUBLISH_USAGE}", 2)
    cmd = ["publish", "--record", ns.record, "--data", ns.data]
    if ns.sha:
        cmd.extend(["--sha", ns.sha])
    return run_wpt_report(cmd)


def summary_main(argv: list[str]) -> int:
    if any(arg in ("-h", "--help") for arg in argv):
        print(SUMMARY_USAGE)
        return 0
    parser = _ArgParser(prog="wpt summary", add_help=False)
    parser.add_argument("--data", required=True)
    try:
        ns = parser.parse_args(argv)
    except ValueError as error:
        return die("summary", f"{error}\n{SUMMARY_USAGE}", 2)
    return run_wpt_report(["summary", "--data", ns.data])


def corpus_counts(summary: dict) -> dict[str, int]:
    """Per-test subtest totals from a wpt.fyi `summary_v2` payload."""
    counts: dict[str, int] = {}
    for test, entry in summary.items():
        if not isinstance(test, str) or not test.startswith("/"):
            continue
        if not isinstance(entry, dict):
            continue
        pair = entry.get("c")
        if not isinstance(pair, list) or len(pair) != 2:
            continue
        total = pair[1]
        if isinstance(total, bool) or not isinstance(total, int) or total <= 0:
            continue
        counts[test] = total
    return counts


def write_corpus(path: Path, counts: dict[str, int]) -> None:
    body = json.dumps(counts, separators=(",", ":"), sort_keys=True).encode()
    if path.suffix == ".gz":
        with gzip.open(path, "wb") as handle:
            handle.write(body)
    else:
        path.write_bytes(body)


def download(url: str, timeout: float) -> bytes:
    request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    with urllib.request.urlopen(request, timeout=timeout) as response:
        return response.read()


def summary_from_wpt_fyi(revision: str) -> tuple[dict | None, str | None]:
    """The newest Chrome summary at `revision`, or a reason it is unavailable."""
    try:
        runs = json.loads(download(WPT_FYI_RUNS.format(revision=revision), 60))
    except (OSError, ValueError) as error:
        return None, f"wpt.fyi run list: {error}"
    if not isinstance(runs, list):
        return None, "wpt.fyi returned no run list"
    candidates = [
        run
        for run in runs
        if isinstance(run, dict)
        and run.get("browser_name") == "chrome"
        and run.get("results_url")
    ]
    candidates.sort(key=lambda run: run.get("created_at") or "", reverse=True)
    if not candidates:
        return None, f"no Chrome run at {revision}"
    problems: list[str] = []
    for run in candidates:
        try:
            raw = download(run["results_url"], 120)
            if raw[:2] == b"\x1f\x8b":
                raw = gzip.decompress(raw)
            summary = json.loads(raw)
        except (OSError, ValueError, EOFError) as error:
            problems.append(str(error))
            continue
        if isinstance(summary, dict) and corpus_counts(summary):
            return summary, None
        problems.append("summary has no subtest counts")
    return None, "; ".join(problems)


def corpus_main(argv: list[str]) -> int:
    if any(arg in ("-h", "--help") for arg in argv):
        print(CORPUS_USAGE)
        return 0
    parser = _ArgParser(prog="wpt corpus", add_help=False)
    parser.add_argument("--revision", default=None)
    parser.add_argument("--summary", default=None)
    parser.add_argument("--out", default=None)
    try:
        ns = parser.parse_args(argv)
    except ValueError as error:
        return die("corpus", f"{error}\n{CORPUS_USAGE}", 2)
    revision: str | None = ns.revision
    summary_path: Path | None = Path(ns.summary) if ns.summary else None
    out: Path | None = Path(ns.out) if ns.out else None
    if (revision is None) == (summary_path is None):
        return die(
            "corpus",
            f"need exactly one of --revision or --summary\n{CORPUS_USAGE}",
            2,
        )
    if out is None:
        if revision is None:
            return die("corpus", f"--out is required with --summary\n{CORPUS_USAGE}", 2)
        out = pinned_corpus(revision)
    if summary_path is not None:
        try:
            raw = summary_path.read_bytes()
            if raw[:2] == b"\x1f\x8b":
                raw = gzip.decompress(raw)
            summary = json.loads(raw)
        except (OSError, EOFError, ValueError) as error:
            return die("corpus", f"cannot read {summary_path}: {error}", 2)
        if not isinstance(summary, dict):
            return die("corpus", f"{summary_path} is not a JSON object", 2)
    else:
        assert revision is not None
        summary, problem = summary_from_wpt_fyi(revision)
        if summary is None:
            return die("corpus", problem or "no summary", 1)
    counts = corpus_counts(summary)
    if not counts:
        return die("corpus", "the summary has no subtest counts", 1)
    out.parent.mkdir(parents=True, exist_ok=True)
    write_corpus(out, counts)
    source = revision if revision is not None else str(summary_path)
    eprint(
        f"corpus: {len(counts)} tests, {sum(counts.values())} subtests from {source}"
    )
    return 0
