"""WPT wall-time profiling and browser timeline inspection."""

from __future__ import annotations

import json
import os
import shlex
import subprocess
import sys
import tempfile
from dataclasses import replace
from pathlib import Path
from typing import Any

from results import HARD_STATUSES, ROOT, die, eprint

NEAR_BUDGET_MS = 2500
TIMELINE_LINES = 40
MAX_LISTED = 40
CLI = Path(__file__).resolve().parent / "cli.py"


def profile_report(report: Path, top: int) -> int:
    """Ranks one wptreport by wall time: killed tests first, then the near."""
    try:
        data = json.loads(report.read_text())
    except OSError as error:
        eprint(f"cannot read report {report}: {error}")
        return 1
    except json.JSONDecodeError as error:
        eprint(
            f"report {report} is not valid JSON ({error}); the runner probably failed before writing results"
        )
        return 1
    results = [r for r in data.get("results", []) if r.get("test")]
    if not results:
        eprint(f"no results in report {report}")
        return 1
    ranked = sorted(results, key=lambda r: -(r.get("duration") or 0))
    killed = [r for r in results if r.get("status") in HARD_STATUSES]
    near = [
        r
        for r in results
        if r.get("status") not in HARD_STATUSES
        and (r.get("duration") or 0) >= NEAR_BUDGET_MS
    ]
    total_s = sum(r.get("duration") or 0 for r in results) / 1000
    print(
        f"{len(results)} results, {total_s:.1f}s total, {len(killed)} killed, "
        f"{len(near)} passing within {NEAR_BUDGET_MS}ms of the budget"
    )
    print()
    test_width = max((len(r["test"]) for r in ranked[:top]), default=0)
    print(f"{'wall'.rjust(9)}  {'status'.ljust(8)}  test")
    print("-" * (11 + 8 + test_width))
    for result in ranked[:top]:
        duration = result.get("duration") or 0
        status = str(result.get("status", "?"))
        print(f"{duration:7.0f}ms  {status[:8].ljust(8)}  {result['test']}")
    return 0


def print_timeline(log: Path) -> None:
    """Prints the browser's navigation and request records from one kept log."""
    try:
        lines = log.read_text(errors="replace").splitlines()
    except OSError as error:
        eprint(f"cannot read browser log {log}: {error}")
        return
    records = [line for line in lines if "browser::net" in line]
    print()
    print(f"timeline ({log}), last {TIMELINE_LINES} records:")
    for line in records[-TIMELINE_LINES:]:
        print(f"  {line}")


def profile_run(options: Any, paths: list[str], extra: list[str], top: int) -> int:
    """Runs tests at trace with the browser log at a path it picks, then ranks them."""
    handle, name = tempfile.mkstemp(prefix="wpt-profile-", suffix=".json")
    os.close(handle)
    report = Path(name)
    handle, name = tempfile.mkstemp(prefix="wpt-profile-", suffix=".log")
    os.close(handle)
    log = Path(name)
    profiled = replace(options, log_level="trace", log_file=str(log))
    command = [
        sys.executable,
        str(CLI),
        "run",
        "webdriver",
        *paths,
        *profiled.flags(),
        "--log-wptreport",
        str(report),
        "--no-fail-on-unexpected",
        *extra,
    ]
    eprint(f"$ {shlex.join(command)}")
    proc = subprocess.run(
        command,
        cwd=str(ROOT),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    out = proc.stdout.decode(errors="replace")
    stderr = proc.stderr.decode(errors="replace")
    sys.stderr.write(out)
    sys.stderr.write(stderr)
    if proc.returncode != 0:
        eprint(f"runner exited {proc.returncode}")
    eprint(f"profile: browser log {log}")
    if log.is_file():
        print_timeline(log)
    code = profile_report(report, top)
    try:
        os.unlink(report)
    except OSError:
        pass
    return code


def profile_main(options: Any, argv: list[str]) -> int:
    usage = "profile PATH... [--top N] | profile --report FILE [--top N]"
    paths: list[str] = []
    extra: list[str] = []
    report: Path | None = None
    top = MAX_LISTED
    index = 0
    while index < len(argv):
        arg = argv[index]
        if arg == "--":
            extra = argv[index + 1 :]
            break
        if arg.startswith("--report=") or arg.startswith("--top="):
            flag, value = arg.split("=", 1)
        elif arg in ("--report", "--top"):
            if index + 1 >= len(argv):
                return die("profile", f"{arg} needs a value", 2)
            flag, value = arg, argv[index + 1]
            index += 1
        elif arg.startswith("-"):
            extra = argv[index:]
            break
        else:
            paths.append(arg)
            index += 1
            continue
        if flag == "--top":
            try:
                top = int(value)
            except ValueError:
                return die("profile", f"--top needs an integer, got {value!r}", 2)
            if top < 1:
                return die("profile", f"--top needs a positive integer, got {top}", 2)
        else:
            report = Path(value)
        index += 1
    if report is not None and paths:
        return die("profile", f"give paths or --report, not both; usage: {usage}", 2)
    if report is not None:
        return profile_report(report, top)
    if not paths:
        return die("profile", usage, 2)
    return profile_run(options, paths, extra, top)
