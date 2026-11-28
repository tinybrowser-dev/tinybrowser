#!/usr/bin/env python3
"""Single entry point for the tinybrowser WPT harness.

Upstream WPT (`third_party/wpt`, pristine submodule) owns serving,
scheduling, driving, and reporting. This file owns the phases around it:
* `run webdriver`  — build the binary, ready the shared venv + manifest, exec wpt.
* `run in-process` — mount testharness/crashtest/reftest in the engine process.
* `run test262`    — the ECMAScript suite natively in QuickJS.
* `score`  — run a set, then print one row per directory + attention list.
* `rerun`  — re-run only the attention set from a saved wptreport.
* `merge`  — fold the chunk reports of a CI run into one score.
* `corpus` — Chrome's per-test subtest counts for the pinned WPT revision.
* `publish` — one record into the history repo's data directory.
* `summary` — the regression summary of a published data directory.
* `profile` — run tests (or read one report) and rank them by wall time.
* `failures` — print the canonical failure list from a `--score` log.
* `baseline` — run a slice at two revisions, diff the failure lists.
* `manifest` — one manifest walk for a chunked CI run (build job).
* `overnight` — full-suite run + score (needs explicit approval per AGENTS.md).

A lane names the executor, and the executor decides what a result means: the
same file can differ between the WebDriver and in-process lanes, so every
report records the lane it came from. `run` requires the lane; there is no
default, because the slowest executor must never be the accidental one.

`tinybrowser_wpt.py` stays separate: upstream imports it as the
`wptrunner.products` entry-point, so it must remain a tiny importable module.
The run orchestration, in-venv patching, scoring, and rerun selection that
used to be split across shell and Python files live here, sharing one
argument grammar and one stamp/lock helper set.

Grammar (all subcommands): test paths and our flags come first; a literal
`--` starts verbatim upstream flags. For convenience an upstream flag before
`--` implicitly starts the verbatim region, so
`run dom/nodes/ --exclude=worker` works as well as
`run dom/nodes/ -- --exclude=worker`. The harness flags listed by `--help`
are recognized in any subcommand before `--`, and a command re-passes them
to whatever process it spawns, so a run has one configuration and no state
in the environment.
"""

from __future__ import annotations

import contextlib
import fcntl
import gzip
import hashlib
import io
import json
import os
import re
import shlex
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
from collections.abc import Iterable
from dataclasses import dataclass, replace
from pathlib import Path
from typing import Any

from results import (
    corpus_counts,
    corpus_main,
    failing_tests,
    merge_main,
    needs_retest,
    publish_main,
    rerun_status,
    summarize,
    summary_main,
    write_corpus,
)
from timeouts import DEFAULT_TIMEOUT, EXTRA_TIMEOUT, LONG_TIMEOUT
from wpt_profile import print_timeline, profile_main

ROOT = Path(__file__).resolve().parents[2]
CLI = Path(__file__).resolve()
CORPUS_DIR = ROOT / "tools" / "wpt" / "corpus"


def pinned_corpus(revision: str) -> Path:
    return CORPUS_DIR / f"{revision}.json.gz"


KNOWN_TEST_TYPES = "aamtest crashtest print-reftest reftest test262 testharness wdspec"
KNOWN_TEST_TYPE_SET = set(KNOWN_TEST_TYPES.split())

TLS_SCHEMES = ("https", "https-local", "https-public", "wss", "h2")
HTTP_ONLY = ("http", "ws")

WORKER_VARIANT = re.compile(
    r"(?:^|[/.])(any\.)?(sharedworker|serviceworker|worker)(-(classic|module))?"
    r"(\.[a-z0-9]+)*\.html$"
    r"|^/html/canvas/.+\.w\.html$"
)

HARD_STATUSES = ("TIMEOUT", "CRASH", "ERROR")
TIMEOUT_STATUSES = ("TIMEOUT", "EXTERNAL-TIMEOUT")
UNMEASURED_STATUSES = HARD_STATUSES + ("EXTERNAL-TIMEOUT",)
MAX_LISTED = 40

# https://github.com/tinybrowser-dev/tinybrowser/issues/241 — the in-process host lacks SRI/ORB/CSP/cookies/window.open; those directories run in WebDriver until the browser host lands
BROWSER_POLICY_PREFIXES = (
    "fetch/http-cache",
    "fetch/orb",
    "html/browsers/history",
    "html/browsers/the-window-object",
    "html/browsers/windows",
    "subresource-integrity",
    "webmessaging/broadcastchannel",
)


def browser_policy_path(url: str) -> bool:
    path = url.split("?", 1)[0].lstrip("/")
    return any(
        path == prefix or path.startswith(prefix + "/")
        for prefix in BROWSER_POLICY_PREFIXES
    )


def in_process_excludes(paths: list[str]) -> list[str]:
    """Policy directories the in-process lane must leave to WebDriver.

    A path that names the policy directory itself (or a child) is an explicit
    request and keeps it; `fetch/` or `/` does not, because those also collect
    files the in-process lane is meant to run.
    """
    requested = [path.split("?", 1)[0].strip("/") for path in paths]
    return [
        prefix
        for prefix in BROWSER_POLICY_PREFIXES
        if not any(
            name == prefix or name.startswith(prefix + "/") for name in requested
        )
    ]


HARNESS_VALUE_FLAGS = {
    "--wpt-root",
    "--jobs",
    "--report",
    "--baseline",
    "--limit",
    "--offset",
    "--total-chunks",
    "--this-chunk",
    "--normal-budget",
    "--long-budget",
    "--content-types",
    "--exclude",
    "--sidecar",
    "--journal",
    "--stale",
}


WPT_RUN_REQUIREMENTS = (
    "tools/manifest/requirements.txt",
    "tools/wptrunner/requirements.txt",
    "tools/serve/requirements.txt",
)
SETUPTOOLS = "setuptools<71"


def eprint(*parts: object) -> None:
    print(*parts, file=sys.stderr)


def die(prefix: str, message: str, code: int = 1) -> int:
    eprint(f"{prefix}: {message}")
    return code


def sha16(text: str) -> str:
    return hashlib.sha256(text.encode()).hexdigest()[:16]


def sha_file(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def cache_dir() -> Path:
    base = Path(os.environ.get("XDG_CACHE_HOME") or Path.home() / ".cache")
    out = base / "tinybrowser"
    out.mkdir(parents=True, exist_ok=True)
    return out


def git(args: list[str], cwd: Path | None = None) -> str | None:
    try:
        out = subprocess.run(
            ["git", *args],
            cwd=str(cwd or ROOT),
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            check=False,
        )
    except OSError:
        return None
    if out.returncode != 0:
        return None
    return out.stdout.decode(errors="replace").strip()


def exclude_skips_worker_variants(exclude: Iterable[str] | None) -> bool:
    return bool(exclude and any(item.strip("/") == "worker" for item in exclude))


def is_worker_variant_url(url: str) -> bool:
    return bool(WORKER_VARIANT.search(url.split("?", 1)[0]))


def has_option(option: str, args: list[str]) -> bool:
    for arg in args:
        if arg == option or arg.startswith(option + "="):
            return True
        if (
            option.startswith("--")
            and arg.startswith("--")
            and "=" not in arg
            and len(arg) >= 4
            and option.startswith(arg)
        ):
            return True
    return False


def has_option_exact(option: str, args: list[str]) -> bool:
    return any(arg == option or arg.startswith(option + "=") for arg in args)


def opt_value(args: list[str], option: str) -> str | None:
    for i, arg in enumerate(args):
        if arg.startswith(option + "="):
            return arg.split("=", 1)[1]
        if arg == option and i + 1 < len(args):
            return args[i + 1]
    return None


@dataclass(frozen=True)
class Options:
    """Harness configuration, resolved from argv once at the entry point.

    A subcommand and any process it spawns agree by construction: the parent
    re-creates the configuration as flags on the child's argv, so no value has
    to travel through the environment.
    """

    binary: str | None = None
    fast_binary: str | None = None
    wpt_root: str | None = None
    venv: str | None = None
    log_level: str | None = None
    log_file: str | None = None
    allow_pin_mismatch: bool = False
    no_lock: bool = False
    lock_timeout: int | None = None

    def flags(self) -> list[str]:
        """This configuration as the flags that reproduce it."""
        out: list[str] = []
        for name, value in (
            ("--binary", self.binary),
            ("--fast-binary", self.fast_binary),
            ("--wpt-root", self.wpt_root),
            ("--venv", self.venv),
            ("--log-level", self.log_level),
            ("--log-file", self.log_file),
        ):
            if value is not None:
                out += [name, value]
        if self.allow_pin_mismatch:
            out.append("--allow-pin-mismatch")
        if self.no_lock:
            out.append("--no-lock")
        if self.lock_timeout is not None:
            out += ["--lock-timeout", str(self.lock_timeout)]
        return out


HARNESS_FLAGS: dict[str, tuple[str, str]] = {
    "--binary": ("binary", "value"),
    "--fast-binary": ("fast_binary", "value"),
    "--wpt-root": ("wpt_root", "value"),
    "--venv": ("venv", "value"),
    "--log-level": ("log_level", "value"),
    "--log-file": ("log_file", "value"),
    "--lock-timeout": ("lock_timeout", "int"),
    "--allow-pin-mismatch": ("allow_pin_mismatch", "switch"),
    "--no-lock": ("no_lock", "switch"),
}


def extract_options(argv: list[str]) -> tuple[Options, list[str], str | None]:
    """Splits the harness flags out of argv, leaving the subcommand grammar.

    Stops at a literal `--` so upstream flags (`run -- --binary x`) are never
    mistaken for ours.
    """
    values: dict[str, Any] = {}
    rest: list[str] = []
    index = 0
    while index < len(argv):
        arg = argv[index]
        if arg == "--":
            rest.extend(argv[index:])
            break
        name = arg.split("=", 1)[0]
        entry = HARNESS_FLAGS.get(name)
        if entry is None:
            rest.append(arg)
            index += 1
            continue
        field, kind = entry
        if "=" in arg:
            value = arg.split("=", 1)[1]
        elif kind == "switch":
            value = ""
        else:
            if index + 1 >= len(argv) or argv[index + 1].startswith("-"):
                return Options(), [], f"{name} needs a value"
            value = argv[index + 1]
            index += 1
        if kind == "switch":
            if value:
                return Options(), [], f"{name} takes no value"
            values[field] = True
        elif kind == "int":
            try:
                number = int(value)
            except ValueError:
                return Options(), [], f"{name} needs a whole number"
            if number < 0:
                return Options(), [], f"{name} needs a non-negative number"
            values[field] = number
        elif kind == "value" and not value:
            return Options(), [], f"{name} needs a value"
        else:
            values[field] = value
        index += 1
    return Options(**values), rest, None


def resolve_wpt_root(options: Options) -> Path:
    override = options.wpt_root
    if override:
        return Path(override)
    local = ROOT / "third_party" / "wpt"
    if (local / "wpt").is_file():
        return local
    worktrees = git(["worktree", "list", "--porcelain"]) or ""
    primary = None
    for line in worktrees.splitlines():
        if line.startswith("worktree "):
            primary = line.split(" ", 1)[1]
            break
    base = Path(primary) if primary else ROOT
    return base / "third_party" / "wpt"


def check_pin(wpt: Path, options: Options) -> int:
    pinned = git(["rev-parse", "HEAD:third_party/wpt"])
    actual = git(["rev-parse", "HEAD"], cwd=wpt)
    allow = options.allow_pin_mismatch
    if pinned and not allow:
        if not actual:
            eprint(f"cannot read the WPT revision at {wpt}; it is not a git checkout")
            eprint("Pass --allow-pin-mismatch to run it anyway.")
            return 1
        if pinned != actual:
            eprint(f"WPT checkout at {wpt} is {actual}; this worktree pins {pinned}")
            eprint("Update the checkout, or pass --allow-pin-mismatch.")
            return 1
    elif pinned and pinned != (actual or ""):
        eprint(
            f"WPT pin mismatch ignored: running {actual or 'an unknown revision'}, worktree pins {pinned}"
        )
    return 0


LOCK_TIMEOUT_S = 1800


def acquire_lock(name: str, options: Options, what: str | None = None):
    if options.no_lock:
        return None
    path = cache_dir() / name
    handle = open(path, "a+b")
    try:
        fcntl.flock(handle.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        return handle
    except BlockingIOError:
        pass
    timeout_s = options.lock_timeout
    if timeout_s is None:
        timeout_s = LOCK_TIMEOUT_S
    eprint(f"another WPT run holds {path}; waiting")
    deadline = time.monotonic() + timeout_s
    while True:
        try:
            fcntl.flock(handle.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
            return handle
        except BlockingIOError:
            if time.monotonic() >= deadline:
                eprint(f"timed out waiting for {what or path}")
                handle.close()
                raise SystemExit(1)
            time.sleep(0.2)


def release_lock(handle) -> None:
    if handle is not None:
        try:
            handle.close()
        except OSError:
            pass


def venv_python(venv: Path) -> Path:
    return venv / "bin" / "python"


def venv_stamp_payload(wpt: Path, venv: Path) -> str:
    lines = [f"root {ROOT}"]
    exe = venv_python(venv)
    if exe.is_file() and os.access(exe, os.X_OK):
        try:
            out = subprocess.run(
                [str(exe), "-c", "import sys; print(sys.version)"],
                stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL,
                check=False,
            )
            lines.append(out.stdout.decode(errors="replace").strip() or "no-python")
        except OSError:
            lines.append("no-python")
    else:
        lines.append("no-python")
    for rel in WPT_RUN_REQUIREMENTS:
        lines.append(sha_file(wpt / rel))
    lines.append(SETUPTOOLS)
    lines.append(sha_file(ROOT / "tools" / "wpt" / "pyproject.toml"))
    return hashlib.sha256("\n".join(lines).encode()).hexdigest()


def venv_is_current(wpt: Path, venv: Path) -> bool:
    stamp = venv / ".tinybrowser-wpt-stamp"
    exe = venv_python(venv)
    if not (exe.is_file() and os.access(exe, os.X_OK) and stamp.is_file()):
        return False
    try:
        return venv_stamp_payload(wpt, venv) == stamp.read_text().strip()
    except OSError:
        return False


def install_venv(wpt: Path, venv: Path, options: Options) -> None:
    if shutil.which("uv") is None:
        eprint("uv is required to bootstrap the WPT virtualenv")
        raise SystemExit(1)
    exe = venv_python(venv)
    if not (exe.is_file() and os.access(exe, os.X_OK)):
        if options.venv:
            eprint(f"WPT virtualenv not found at {venv}; --venv must name one")
            raise SystemExit(1)
        subprocess.run(
            ["uv", "venv", "--seed", str(venv), "--python", "3.12"], check=True
        )
    requirements = [
        arg for rel in WPT_RUN_REQUIREMENTS for arg in ("-r", str(wpt / rel))
    ]
    subprocess.run(
        ["uv", "pip", "install", "--python", str(exe), "-q", SETUPTOOLS, *requirements],
        check=True,
    )
    subprocess.run(
        [
            "uv",
            "pip",
            "install",
            "--python",
            str(exe),
            "-q",
            "-e",
            str(ROOT / "tools" / "wpt"),
        ],
        check=True,
    )
    (venv / ".tinybrowser-wpt-stamp").write_text(venv_stamp_payload(wpt, venv) + "\n")


def ensure_venv(wpt: Path, venv: Path, options: Options) -> None:
    if venv_is_current(wpt, venv):
        eprint("wpt: venv is current, skipping install")
        return
    lock = acquire_lock(f"wpt-{sha16(str(venv.resolve()))}.lock", options)
    try:
        if venv_is_current(wpt, venv):
            eprint("wpt: venv is current, skipping install")
        else:
            eprint("wpt: installing WPT virtualenv")
            install_venv(wpt, venv, options)
    finally:
        release_lock(lock)


def manifest_parses(exe: Path, manifest: Path) -> bool:
    try:
        out = subprocess.run(
            [
                str(exe),
                "-c",
                "import json,sys; json.load(open(sys.argv[1]))",
                str(manifest),
            ],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            check=False,
        )
        return out.returncode == 0
    except OSError:
        return False


def split_run_argv(
    argv: list[str],
) -> tuple[list[str], bool, bool, list[str], str | None]:
    """paths + our flags vs verbatim upstream extras.

    Our flags: --score, --dry-run. Anything else starting with `-` (or the
    literal `--`) starts the verbatim region so upstream argparse sees it
    untouched. `--test-types` values stay with it; a following positional
    stays a path, so the nargs='*' swallow can never trigger.
    """
    paths: list[str] = []
    extras: list[str] = []
    score = False
    dry_run = False
    i = 0
    while i < len(argv):
        arg = argv[i]
        if arg == "--":
            extras.extend(argv[i + 1 :])
            break
        if arg == "--score":
            score = True
        elif arg == "--dry-run":
            dry_run = True
        elif arg == "--test-types" or arg.startswith("--test-types="):
            values = [arg]
            if arg == "--test-types":
                j = i + 1
                while j < len(argv) and argv[j] in KNOWN_TEST_TYPE_SET:
                    values.append(argv[j])
                    j += 1
                if (
                    j < len(argv)
                    and not argv[j].startswith("-")
                    and "/" not in argv[j]
                    and "." not in argv[j]
                ):
                    return (
                        [],
                        False,
                        False,
                        [],
                        (
                            f"unknown --test-types value '{argv[j]}'; expected one of: {KNOWN_TEST_TYPES}"
                        ),
                    )
                i = j - 1
            extras.extend(values)
        elif arg.startswith("-") and arg != "-":
            extras.extend(argv[i:])
            break
        else:
            paths.append(arg)
        i += 1
    return paths, score, dry_run, extras, None


def hoist_paths_after_test_types(
    paths: list[str], extras: list[str]
) -> tuple[list[str], list[str], str | None]:
    """Rescue positionals `--test-types` (nargs='*') would swallow.

    Upstream argparse greedily consumes every positional following
    `--test-types` as another type, so `run --processes 4 --test-types
    reftest dom/nodes/` would silently cover nothing. Anything bare that
    directly trails the type values and looks like a path (or `-`) moves
    back to paths; a typeless bare word is a typo and fails loudly.
    Option values elsewhere are untouched: only the tail of
    `--test-types` is hazardous, since argparse assigns free positionals
    correctly everywhere else.
    """
    out: list[str] = []
    i = 0
    while i < len(extras):
        arg = extras[i]
        out.append(arg)
        i += 1
        if arg != "--test-types" and not arg.startswith("--test-types="):
            continue
        while i < len(extras) and extras[i] in KNOWN_TEST_TYPE_SET:
            out.append(extras[i])
            i += 1
        while i < len(extras) and not extras[i].startswith("-"):
            token = extras[i]
            i += 1
            if token == "-" or "/" in token or "." in token:
                paths.append(token)
            else:
                return (
                    paths,
                    [],
                    (
                        f"unknown --test-types value '{token}'; expected one of: {KNOWN_TEST_TYPES}"
                    ),
                )
    return paths, out, None


def split_score_argv(
    argv: list[str],
) -> tuple[Path | None, Path | None, list[str], list[str], bool, str | None]:
    report: Path | None = None
    save_report: Path | None = None
    paths: list[str] = []
    extra: list[str] = []
    dry_run = False
    i = 0
    while i < len(argv):
        arg = argv[i]
        if arg == "--":
            extra = list(argv[i + 1 :])
            break
        if arg in ("--report", "--save-report") or arg.startswith(
            ("--report=", "--save-report=")
        ):
            option, value = arg.split("=", 1) if "=" in arg else (arg, None)
            if value is None:
                if i + 1 >= len(argv):
                    return (
                        report,
                        save_report,
                        paths,
                        extra,
                        dry_run,
                        f"{option} needs a file",
                    )
                value = argv[i + 1]
                i += 1
            if not value:
                return (
                    report,
                    save_report,
                    paths,
                    extra,
                    dry_run,
                    f"{option} needs a file",
                )
            if option == "--report":
                report = Path(value)
            else:
                save_report = Path(value)
        elif arg == "--dry-run":
            dry_run = True
        elif arg.startswith("-"):
            extra.extend(argv[i:])
            break
        else:
            paths.append(arg)
        i += 1
    return report, save_report, paths, extra, dry_run, None


def parse_rerun_argv(
    argv: list[str],
) -> tuple[Path | None, bool, bool, Path | None, list[str], str | None]:
    usage = "rerun REPORT [--include-timeout] [--dry-run] [--save-report FILE] [-- runner args...]"
    report: Path | None = None
    include_timeout = False
    dry_run = False
    save_report: Path | None = None
    i = 0
    while i < len(argv):
        arg = argv[i]
        if arg == "--":
            if report is None:
                return (
                    None,
                    include_timeout,
                    dry_run,
                    save_report,
                    list(argv[i + 1 :]),
                    usage,
                )
            return (
                report,
                include_timeout,
                dry_run,
                save_report,
                list(argv[i + 1 :]),
                None,
            )
        if arg == "--include-timeout":
            include_timeout = True
        elif arg == "--dry-run":
            dry_run = True
        elif arg == "--save-report" or arg.startswith("--save-report="):
            if arg == "--save-report":
                if i + 1 >= len(argv):
                    return (
                        report,
                        include_timeout,
                        dry_run,
                        None,
                        [],
                        "--save-report needs a file",
                    )
                save_report = Path(argv[i + 1])
                i += 1
            else:
                value = arg.split("=", 1)[1]
                if not value:
                    return (
                        report,
                        include_timeout,
                        dry_run,
                        None,
                        [],
                        "--save-report needs a file",
                    )
                save_report = Path(value)
        elif arg.startswith("-"):
            return (
                report,
                include_timeout,
                dry_run,
                save_report,
                list(argv[i:]),
                "runner options must follow a literal `--`",
            )
        elif report is None:
            report = Path(arg)
        else:
            return (
                report,
                include_timeout,
                dry_run,
                save_report,
                [],
                f"unexpected argument {arg}",
            )
        i += 1
    if report is None:
        return None, include_timeout, dry_run, save_report, [], usage
    return report, include_timeout, dry_run, save_report, [], None


def patch_wpt(wpt_root: Path, lane: str | None = None) -> None:
    sys.path.insert(0, str(wpt_root))
    from tools import localpaths  # noqa: F401
    from tools.wpt import run as wpt_run
    from wptserve import sslutils
    from wptserve.config import ConfigBuilder

    check_environ = wpt_run.check_environ

    def skip_tinybrowser_hosts(product: str) -> None:
        if product == "tinybrowser":
            return
        check_environ(product)

    wpt_run.check_environ = skip_tinybrowser_hosts  # ty: ignore[invalid-assignment] -- intentional narrowing patch; upstream is untyped, product is always str, verified by every run

    get_ports = ConfigBuilder._get_ports

    def skip_tls_ports_without_ssl(self, data) -> dict:
        ports = get_ports(self, data)
        ssl_type = data.get("ssl", {}).get("type", "none")
        if not sslutils.get_cls(ssl_type).ssl_enabled:
            for scheme in TLS_SCHEMES:
                ports.pop(scheme, None)
            for scheme in list(ports):
                if scheme not in HTTP_ONLY:
                    ports.pop(scheme, None)
        return ports

    ConfigBuilder._get_ports = skip_tls_ports_without_ssl

    from wptrunner import testloader

    original_filter = testloader.TestFilter

    class TinybrowserTestFilter(original_filter):
        def __init__(self, *args, **kwargs):
            exclude = kwargs.get("exclude")
            if exclude is None and len(args) >= 3:
                exclude = args[2]
            self._skip_worker_variants = exclude_skips_worker_variants(exclude)
            super().__init__(*args, **kwargs)

        def __call__(self, manifest_iter):
            for test_type, test_path, tests in super().__call__(manifest_iter):
                if self._skip_worker_variants:
                    tests = {t for t in tests if not is_worker_variant_url(t.url)}
                    if not tests:
                        continue
                if lane == "testdriver":
                    tests = {t for t in tests if getattr(t, "testdriver", None) is True}
                    if not tests:
                        continue
                elif lane == "in-process-complement":
                    selected = set()
                    for test in tests:
                        if (
                            getattr(test, "testdriver", None) is True
                            and test_type != "crashtest"
                        ):
                            selected.add(test)
                            continue
                        url = test.url.split("?", 1)[0]
                        if browser_policy_path(url):
                            selected.add(test)
                            continue
                        if url.endswith((".any.html", ".window.html")):
                            continue
                        if os.path.isfile(os.path.join(str(wpt_root), url.lstrip("/"))):
                            continue
                        selected.add(test)
                    tests = selected
                    if not tests:
                        continue
                yield test_type, test_path, tests

    testloader.TestFilter = TinybrowserTestFilter  # ty: ignore[invalid-assignment]

    from wptrunner.browsers import base as browser_base

    def wait_for_webdriver(
        logger: Any,
        host: str,
        port: int,
        timeout: int | float = 60,
        server_process: Any = None,
    ) -> bool:
        """`wait_for_service`, polling every 10 ms instead of 500 ms.

        The tinybrowser WebDriver listens ~75 ms after spawn, so upstream's
        interval turned every browser start into a 0.5 s wait.
        """
        end = time.time() + timeout
        while time.time() < end:
            if server_process is not None and server_process.poll() is not None:
                break
            with socket.socket() as probe:
                try:
                    probe.connect((host, port))
                except ConnectionRefusedError:
                    time.sleep(0.01)
                    continue
            logger.debug(f"Connected to {host}:{port}")
            return True
        raise OSError(f"Service is unavailable: {host}:{port}")

    browser_base.wait_for_service = wait_for_webdriver  # ty: ignore[invalid-assignment] -- intentional runtime patch; polling interval is the whole point, verified by every run


def inner_main(options: Options, argv: list[str]) -> int:
    """Runs under the venv python: patch upstream, then `wpt run`."""
    selection = selection_lane(argv)
    if selection is not None:
        argv = [
            arg
            for arg in argv
            if arg not in ("--testdriver-only", "--exclude-in-process")
        ]
    if argv[:1] == ["--"]:
        argv = argv[1:]
    wpt_root = resolve_wpt_root(options)
    patch_wpt(wpt_root, lane=selection)
    from tools.wpt import wpt

    code = wpt.main(prog=str(wpt_root / "wpt"), argv=["run", *argv])
    return code if isinstance(code, int) else 0


def build_binary(options: Options) -> Path:
    override = options.binary
    if override:
        eprint(
            f"wpt: using the --binary override without a freshness check: {override}"
        )
        return Path(override)
    return build_profile_binary("tinybrowser")


def fast_binary(options: Options, name: str) -> Path:
    """The in-process runner, prebuilt when the caller passed `--fast-binary`."""
    override = options.fast_binary
    if override:
        binary = Path(override)
        if not (binary.is_file() and os.access(binary, os.X_OK)):
            raise SystemExit(die("fast", f"{name} binary not found at {binary}"))
        eprint(f"wpt: using the --fast-binary override: {binary}")
        return Path(os.path.realpath(binary))
    return build_profile_binary(name)


def build_profile_binary(name: str) -> Path:
    out = subprocess.run(
        [str(ROOT / "tools" / "binary-path"), "--profile", "wpt", name],
        cwd=str(ROOT),
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        check=False,
    )
    binary = Path(out.stdout.decode(errors="replace").strip())
    eprint(f"wpt: building {name} (cargo no-ops when fresh)")
    build = ["cargo", "build", "-q"]
    if name != "tinybrowser":
        build.extend(["-p", name])
    ret = subprocess.run(
        [*build, "--profile", "wpt", "--offline"], cwd=str(ROOT)
    ).returncode
    if ret != 0:
        ret = subprocess.run([*build, "--profile", "wpt"], cwd=str(ROOT)).returncode
    if ret != 0:
        eprint(f"wpt: cargo build failed; refusing to run against a stale {name}")
        raise SystemExit(1)
    return binary


def finalize_run_args(wpt: Path, extras: list[str]) -> list[str]:
    args = list(extras)
    if not has_option("--metadata", args):
        args = ["--metadata", str(ROOT / "tools" / "wpt" / "metadata"), *args]
    if not has_option("--manifest", args):
        args = ["--manifest", str(wpt / "MANIFEST.json"), *args]
    if not has_option("--pause-after-test", args) and not has_option(
        "--no-pause-after-test", args
    ):
        args = ["--no-pause-after-test", *args]
    if not has_option("--no-restart-on-unexpected", args) and not has_option(
        "--restart-on-unexpected", args
    ):
        args = ["--no-restart-on-unexpected", *args]
    if not has_option("--ssl-type", args):
        args = ["--ssl-type=openssl", *args]
    if (
        not has_option("--processes", args)
        and not has_option("--fully-parallel", args)
        and not has_option("-f", args)
    ):
        procs = os.cpu_count() or 1
        args.extend(["--processes", str(procs)])
    if not has_option("--test-types", args):
        args.extend(["--test-types", "testharness", "crashtest"])
    return args


def check_type_drift(exe: Path) -> None:
    try:
        out = subprocess.run(
            [
                str(exe),
                "-c",
                "import sys; sys.path.insert(0, 'tools/wptrunner'); "
                "from wptrunner import wpttest; print(' '.join(sorted(wpttest.enabled_tests)))",
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            check=False,
        )
        actual = out.stdout.decode(errors="replace").strip()
    except OSError:
        return
    if actual and actual != KNOWN_TEST_TYPES:
        eprint(
            f"wpt: KNOWN_TEST_TYPES drifts from wpttest.enabled_tests ({actual}); update tools/wpt/cli.py"
        )


def inner_flags(wpt: Path, options: Options) -> list[str]:
    """Flags for the `_inner` child: only what it cannot derive itself.

    The checkout is always passed: the parent did the worktree lookup, and the
    child must see the same tree.
    """
    return ["--wpt-root", str(wpt)]


def run_wpt(
    wpt: Path,
    venv: Path,
    exe: Path,
    binary: Path,
    final_args: list[str],
    manifest: Path,
    record: int,
    options: Options,
    selection: str | None = None,
) -> int:
    wpt_args = list(final_args)
    for flag, value in (
        ("log-level", options.log_level),
        ("log-file", options.log_file),
    ):
        if value:
            wpt_args.append(f"--binary-arg=--{flag}={value}")
    inner_argv = [*inner_flags(wpt, options)]
    if selection is not None:
        inner_argv.append(selection_flag(selection))
    inner_argv.extend(
        [
            "--",
            "--binary",
            str(binary),
            "tinybrowser",
            *wpt_args,
        ]
    )
    manifest_lock = None
    if not has_option_exact("--no-manifest-update", final_args):
        lock_name = f"wpt-manifest-{sha16(str(manifest.resolve()))}.lock"
        manifest_lock = acquire_lock(lock_name, options, what=f"manifest {manifest}")
    manifest_locked = manifest_lock is not None
    venv_lock = None
    if (
        has_option("--install-browser", final_args)
        or has_option("--install-webdriver", final_args)
        or has_option_exact("--manifest-download", final_args)
        or has_option("--install-fonts", final_args)
    ):
        venv_lock = acquire_lock(
            f"wpt-{sha16(str(venv.resolve()))}.lock",
            options,
            what=f"venv {venv}",
        )

    for i, arg in enumerate(final_args):
        if arg == "--processes" and i + 1 < len(final_args):
            eprint(f"wpt: processes {final_args[i + 1]}")
            break
        if arg.startswith("--processes="):
            eprint(f"wpt: processes {arg.split('=', 1)[1]}")
            break

    if record == 0 and not manifest_locked and venv_lock is None:
        release_lock(manifest_lock)
        try:
            os.execv(str(exe), [str(exe), str(CLI), "_inner", *inner_argv])
        except OSError as error:
            return die("run webdriver", f"cannot exec {exe}: {error}")
        raise AssertionError("unreachable")

    try:
        child = subprocess.Popen(
            [str(exe), str(CLI), "_inner", *inner_argv],
            cwd=str(wpt),
            close_fds=True,
        )
    except OSError as error:
        release_lock(venv_lock)
        release_lock(manifest_lock)
        return die("run webdriver", f"could not start the runner: {error}")

    def forward(signum, _frame):
        try:
            child.send_signal(signum)
        except (OSError, ValueError):
            pass

    old = {s: signal.signal(s, forward) for s in (signal.SIGTERM, signal.SIGHUP)}
    old_int = signal.signal(signal.SIGINT, forward)
    try:
        status = child.wait()
    finally:
        for s, handler in old.items():
            signal.signal(s, handler)
        signal.signal(signal.SIGINT, old_int)
    release_lock(venv_lock)
    release_lock(manifest_lock)
    return status


LANES = ("in-process", "webdriver", "test262")


def run_main(options: Options, argv: list[str]) -> int:
    """`run <lane>` — pick the executor, then hand it the rest of argv.

    The lane is required. The executors disagree on some verdicts (a file can
    pass in-process and time out under WebDriver), so every result must name
    the executor that produced it, and no lane may be the accidental default.
    """
    if not argv:
        return die("run", "give a lane: in-process | webdriver | test262", 2)
    lane, rest = argv[0], argv[1:]
    if lane == "in-process":
        return in_process_main(options, rest)
    if lane == "webdriver":
        return webdriver_main(options, rest)
    if lane == "test262":
        return test262_main(options, rest)
    return die(
        "run",
        f"unknown lane {lane}; expected in-process | webdriver | test262",
        2,
    )


def webdriver_main(options: Options, argv: list[str]) -> int:
    selection = selection_lane(argv)
    if selection is not None:
        argv = [
            arg
            for arg in argv
            if arg not in ("--testdriver-only", "--exclude-in-process")
        ]
    paths, score, dry_run, extras, error = split_run_argv(argv)
    if error:
        return die("run webdriver", error, 2)
    if score:
        if dry_run:
            extras = ["--dry-run", *extras]
        if selection is not None:
            extras = [selection_flag(selection), *extras]
        return score_main(options, [*paths, *extras])
    if dry_run:
        wpt = resolve_wpt_root(options)
        final = finalize_run_args(wpt, extras)
        paths, final, error = hoist_paths_after_test_types(paths, final)
        if error:
            return die("run webdriver", error, 2)
        print(
            f"$ {wpt}/wpt run --binary <tinybrowser> tinybrowser {shlex.join([*paths, *final])}"
        )
        return 0

    wpt = resolve_wpt_root(options)
    if not (wpt / "wpt").is_file():
        eprint(f"WPT checkout not found at {wpt}")
        eprint("Initialize it with: git submodule update --init third_party/wpt")
        eprint("or pass --wpt-root with an initialized checkout.")
        return 1
    wpt = Path(os.path.realpath(wpt))
    if check_pin(wpt, options) != 0:
        return 1

    os.chdir(ROOT)
    binary = build_binary(options)
    if not (binary.is_file() and os.access(binary, os.X_OK)):
        return die("run webdriver", f"tinybrowser binary not found at {binary}")
    binary = Path(os.path.realpath(binary))
    os.chdir(wpt)

    venv = Path(options.venv) if options.venv else wpt / "_venv3"
    eprint(f"wpt: {wpt}")
    eprint(f"venv: {venv}")
    eprint(f"binary: {binary}")
    ensure_venv(wpt, venv, options)
    exe = venv_python(venv)

    final = finalize_run_args(wpt, extras)
    check_type_drift(exe)

    manifest = Path(opt_value(final, "--manifest") or wpt / "MANIFEST.json")
    record = 0 if has_option_exact("--no-manifest-update", final) else 1
    paths, final, error = hoist_paths_after_test_types(paths, final)
    if error:
        return die("run webdriver", error, 2)
    return run_wpt(
        wpt,
        venv,
        exe,
        binary,
        [*paths, *final],
        manifest,
        record,
        options,
        selection,
    )


def selection_lane(argv: list[str]) -> str | None:
    """The selection a `run webdriver` asked for, or `None` for every file."""
    if "--testdriver-only" in argv:
        return "testdriver"
    if "--exclude-in-process" in argv:
        return "in-process-complement"
    return None


def selection_flag(selection: str) -> str:
    """The flag that selects `selection`, for a child `run webdriver` command."""
    return "--testdriver-only" if selection == "testdriver" else "--exclude-in-process"


def test262_main(options: Options, argv: list[str]) -> int:
    """`run test262` — run the ECMAScript suite natively, no HTML or wptserve.

    The runner reads `third_party/wpt/third_party/test262` and never writes
    into it. It emits a wptreport with `--report`, and `--baseline` diffs the
    next run against that report. Strict variants are a prefix added in
    memory, so the checkout stays exactly at the pinned submodule revision.
    """
    if "--score" in argv:
        return die(
            "run test262",
            "--score summarizes a webdriver run; use `score --report FILE`",
            2,
        )
    wpt = resolve_wpt_root(options)
    if not (wpt / "wpt").is_file():
        eprint(f"WPT checkout not found at {wpt}")
        eprint("Initialize it with: git submodule update --init third_party/wpt")
        eprint("or pass --wpt-root with an initialized checkout.")
        return 1
    wpt = Path(os.path.realpath(wpt))
    if check_pin(wpt, options) != 0:
        return 1
    test262 = wpt / "third_party" / "test262"
    if not test262.is_dir():
        return die("run test262", f"no test262 suite at {test262}", 1)
    binary = fast_binary(options, "wpt-fast")
    command = [str(binary), "--test262-root", str(test262), *argv]
    eprint("wpt: " + " ".join(command))
    try:
        return subprocess.run(command, cwd=str(ROOT), check=False).returncode
    except OSError as error:
        return die("run test262", f"cannot run {binary}: {error}")


def in_process_main(options: Options, argv: list[str]) -> int:
    """`run in-process` — testharness, crashtest, and reftest in this process.

    No WebDriver, wptserve request path, or renderer child. The WPT checkout is
    read-only; requests are served from these files, and the paused clock jumps
    over timer waits. Use `--report` and `--baseline` like `run test262`.

    Testdriver files are never selected: driving a WebDriver session is the
    `run webdriver` lane, which owns exactly the files this one cannot run.

    Each window runs in one process, and that process keeps one file in flight
    at a time, so a page that aborts the engine costs exactly its own verdict.
    The harness journals every file it starts and finishes, and this restarts
    it until the selection is complete: a finished file is reported from the
    journal, an unfinished one becomes its own ERROR or TIMEOUT.
    """
    argv = [arg for arg in argv if arg != "--exclude-testdriver"]
    if "--score" in argv:
        return die(
            "run in-process",
            "--score summarizes a webdriver run; use `score --report FILE`",
            2,
        )
    wpt = resolve_wpt_root(options)
    if not (wpt / "wpt").is_file():
        eprint(f"WPT checkout not found at {wpt}")
        eprint("Initialize it with: git submodule update --init third_party/wpt")
        eprint("or pass --wpt-root with an initialized checkout.")
        return 1
    wpt = Path(os.path.realpath(wpt))
    if check_pin(wpt, options) != 0:
        return 1
    binary = fast_binary(options, "wpt-harness")
    if ensure_manifest(wpt, options) != 0:
        return 1
    venv = Path(options.venv) if options.venv else wpt / "_venv3"
    ensure_venv(wpt, venv, options)
    content_types = wptserve_content_types(wpt, venv)
    if content_types is None:
        return 1
    sidecar = start_sidecar(wpt, venv)
    if sidecar is None:
        return 1
    try:
        policy = [
            *harness_budget_flags(),
            "--exclude-testdriver",
            "--content-types",
            content_types,
            "--sidecar",
            sidecar.address,
        ]
        for prefix in in_process_excludes(harness_paths(argv)):
            policy.extend(["--exclude", prefix])
        with tempfile.TemporaryDirectory(prefix="wpt-harness-journal-") as scratch:
            journal = Path(scratch) / "journal.jsonl"
            return run_harness_windows(binary, wpt, policy, argv, journal)
    finally:
        sidecar.stop()


class SidecarProcess:
    """A live wptserve process serving handler routes for the harness."""

    def __init__(self, process: subprocess.Popen, address: str) -> None:
        self.process = process
        self.address = address

    def stop(self) -> None:
        """Closes the sidecar's stdin, which is its exit condition."""
        if self.process.poll() is not None:
            return
        if self.process.stdin is not None:
            try:
                self.process.stdin.close()
            except OSError:
                pass
        try:
            self.process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait(timeout=10)


def start_sidecar(wpt: Path, venv: Path) -> SidecarProcess | None:
    """Starts wptserve for the harness and returns its loopback address.

    The harness runs the engine in-process but a `.py` path is a program, not a
    file. This process runs the real wptserve handler for those requests, with
    its stash and its templates, instead of porting that server here.
    """
    command = [
        str(venv_python(venv)),
        str(ROOT / "tools" / "wpt" / "sidecar.py"),
        "--wpt-root",
        str(wpt),
    ]
    eprint("wpt: " + " ".join(command))
    try:
        process = subprocess.Popen(
            command,
            cwd=str(ROOT),
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            text=True,
        )
    except OSError as error:
        die("run in-process", f"cannot start wptserve: {error}")
        return None
    line = process.stdout.readline().strip() if process.stdout is not None else ""
    if not line.startswith("PORT "):
        process.kill()
        process.wait(timeout=10)
        die("run in-process", "wptserve did not report its port")
        return None
    return SidecarProcess(process, f"127.0.0.1:{line.removeprefix('PORT ').strip()}")


def harness_budget_flags() -> list[str]:
    """The runner's wall-clock budgets, from the one timeout policy.

    The in-process lane must abandon a file where the WebDriver lane does, so
    the numbers come from `tools/wpt/timeouts.py` and travel on the command
    line instead of being a second copy in the Rust runner.
    """
    normal = DEFAULT_TIMEOUT + EXTRA_TIMEOUT
    long = LONG_TIMEOUT + EXTRA_TIMEOUT
    return ["--normal-budget", str(normal), "--long-budget", str(long)]


def ensure_manifest(wpt: Path, options: Options) -> int:
    """Builds `MANIFEST.json` when the checkout has none.

    The fast lanes select tests from the manifest. It is a generated file, not
    a cache: once it exists it is used as-is, so the lanes never spend a tree
    walk per run. `cli.py manifest` is the explicit rebuild after editing the
    WPT checkout. Building it here keeps the walk in the venv python, where
    upstream's manifest package and its requirements live.
    """
    manifest = wpt / "MANIFEST.json"
    if manifest.is_file():
        return 0
    venv = Path(options.venv) if options.venv else wpt / "_venv3"
    ensure_venv(wpt, venv, options)
    exe = venv_python(venv)
    child = [str(exe), str(CLI), "_manifest", *inner_flags(wpt, options), "--"]
    try:
        return subprocess.run(child, cwd=str(wpt), check=False).returncode
    except OSError as error:
        eprint(f"wpt: cannot run {exe}: {error}")
        return 1


def wptserve_content_types(wpt: Path, venv: Path) -> str | None:
    """wptserve's extension-to-content-type map, as compact JSON.

    The runner serves files, so it must guess content types exactly as the
    WebDriver lane's wptserve does. Reading the table from the source keeps one
    copy; `None` means the table could not be read, and the caller must refuse
    to run rather than serve everything as binary.
    """
    code = (
        "import json,sys;"
        "sys.path.insert(0, '.');"
        "import tools.localpaths;"
        "from wptserve.constants import content_types;"
        "print(json.dumps(content_types, separators=(',', ':')))"
    )
    exe = venv_python(venv)
    try:
        out = subprocess.run(
            [str(exe), "-c", code],
            cwd=str(wpt),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
    except OSError as error:
        eprint(f"wpt: cannot read wptserve content types: {error}")
        return None
    if out.returncode != 0:
        eprint(out.stderr.decode(errors="replace").rstrip())
        eprint("wpt: cannot read wptserve content types")
        return None
    table = out.stdout.decode(errors="replace").strip()
    return table or None


def harness_paths(argv: list[str]) -> list[str]:
    """The positional paths in a harness argv, skipping option values."""
    paths: list[str] = []
    index = 0
    while index < len(argv):
        arg = argv[index]
        if arg == "--":
            paths.extend(item for item in argv[index + 1 :] if not item.startswith("-"))
            break
        name = arg.split("=", 1)[0]
        if name in HARNESS_VALUE_FLAGS and "=" not in arg:
            index += 2
            continue
        if not arg.startswith("-"):
            paths.append(arg)
        index += 1
    return paths


WINDOW_TESTS = 600


def run_harness_windows(
    binary: Path, wpt: Path, policy: list[str], argv: list[str], journal: Path
) -> int:
    """Runs the selection in windows, each in a fresh process.

    The caller's own `--offset`/`--limit` window is kept: each process is
    handed a slice inside it, so a driver-side window never runs a file the
    caller excluded.
    """
    base_offset, base_limit, forward = harness_argv_window(argv)
    total = harness_selection_count(
        binary, wpt, policy, forward, base_offset, base_limit
    )
    if total is None:
        return die("run in-process", "cannot list the selection")
    if total == 0:
        return die("run in-process", "no tests selected", 2)
    end = total if base_limit is None else min(total, base_limit)
    position = 0
    while position < end:
        size = min(WINDOW_TESTS, end - position)
        code = run_harness_supervised(
            binary,
            wpt,
            policy,
            [
                *forward,
                "--offset",
                str(base_offset + position),
                "--limit",
                str(size),
            ],
            journal,
        )
        if code != 0:
            return code
        position += size
    return 0


def harness_argv_window(argv: list[str]) -> tuple[int, int | None, list[str]]:
    """Takes the caller's `--offset`/`--limit` out of the runner flags."""
    offset = 0
    limit: int | None = None
    rest: list[str] = []
    index = 0
    while index < len(argv):
        arg = argv[index]
        value = argv[index + 1] if index + 1 < len(argv) else None
        if arg in ("--offset", "--limit") and value is not None and value.isdigit():
            if arg == "--offset":
                offset = int(value)
            else:
                limit = int(value)
            index += 2
            continue
        rest.append(arg)
        index += 1
    return offset, limit, rest


def harness_selection_count(
    binary: Path,
    wpt: Path,
    policy: list[str],
    forward: list[str],
    offset: int,
    limit: int | None,
) -> int | None:
    """How many files the same flags select, without running one."""
    command = [
        str(binary),
        "--wpt-root",
        str(wpt),
        *policy,
        *forward,
        "--offset",
        str(offset),
        "--list",
    ]
    if limit is not None:
        command.extend(["--limit", str(limit)])
    try:
        result = subprocess.run(
            command, cwd=str(ROOT), check=False, capture_output=True, text=True
        )
    except OSError as error:
        die("run in-process", f"cannot run {binary}: {error}")
        return None
    if result.returncode != 0:
        eprint(result.stderr.rstrip())
        return None
    return len([line for line in result.stdout.splitlines() if "\t" in line])


def run_harness_supervised(
    binary: Path, wpt: Path, policy: list[str], argv: list[str], journal: Path
) -> int:
    """Runs the harness until every selected file has a verdict.

    The engine runs in-process, so a hostile page can end the process without
    unwinding: a layout recursion overflows the stack, a `QuickJS` assertion
    aborts, a drain that never yields is cut by the harness watchdog. The
    harness journals each file before it starts and again with its verdict, so
    a restart reports the finished files from the journal, records the files
    that were in flight as ERROR (or TIMEOUT after a watchdog cut), and runs
    only the rest. That is what keeps one bad page from costing a chunk.

    An attempt must finish at least one more file, or the loop stops: a harness
    that dies before its first verdict is broken, not a bad page.
    """
    attempt = 0
    stale = "error"
    completed = 0
    while True:
        attempt += 1
        command = [
            str(binary),
            "--wpt-root",
            str(wpt),
            *policy,
            *argv,
            "--journal",
            str(journal),
            "--stale",
            stale,
        ]
        eprint("wpt: " + " ".join(command))
        try:
            code = subprocess.run(command, cwd=str(ROOT), check=False).returncode
        except OSError as error:
            return die("run in-process", f"cannot run {binary}: {error}")
        if code == 0:
            return 0
        if code > 0 and code != 3:
            return code
        finished, unfinished = read_journal(journal)
        if unfinished:
            shown = ", ".join(unfinished[:8])
            more = f" and {len(unfinished) - 8} more" if len(unfinished) > 8 else ""
            eprint(f"wpt: run in-process: interrupted while running {shown}{more}")
        if finished <= completed:
            eprint(
                f"wpt: run in-process: attempt {attempt} stopped after {finished} "
                f"files with no progress (exit {code})"
            )
            return code
        stale = "timeout" if code == 3 else "error"
        completed = finished
        eprint(f"wpt: run in-process: attempt {attempt} stopped; resuming")


def read_journal(path: Path) -> tuple[int, list[str]]:
    """The finished count and the files an attempt left in flight.

    A line that does not parse is a torn write from the abort that ended the
    attempt, so it names nothing and its file runs again.
    """
    if not path.exists():
        return 0, []
    started: dict[str, None] = {}
    done: set[str] = set()
    for line in path.read_text(errors="replace").splitlines():
        try:
            record = json.loads(line)
        except json.JSONDecodeError:
            continue
        if not isinstance(record, dict):
            continue
        if record.get("event") == "start":
            started[record["id"]] = None
        elif record.get("event") == "done":
            done.add(record["outcome"]["id"])
    return len(done), [test for test in started if test not in done]


def manifest_main(options: Options, argv: list[str]) -> int:
    """`manifest` — build MANIFEST.json once per CI run.

    A chunked CI run has one plan job and N chunk jobs; without this, every
    chunk walks the whole test tree to rebuild the manifest. The plan job
    runs this and uploads the file; each chunk drops it into place and passes
    --no-manifest-update, so the walk happens once. Locally, run this after
    editing the WPT checkout; the fast lanes use an existing manifest as-is.
    """
    if argv:
        return die("manifest", "manifest takes no arguments", 2)
    wpt = resolve_wpt_root(options)
    if not (wpt / "wpt").is_file():
        eprint(f"WPT checkout not found at {wpt}")
        eprint("Initialize it with: git submodule update --init third_party/wpt")
        eprint("or pass --wpt-root with an initialized checkout.")
        return 1
    wpt = Path(os.path.realpath(wpt))
    if check_pin(wpt, options) != 0:
        return 1
    venv = Path(options.venv) if options.venv else wpt / "_venv3"
    eprint(f"wpt: {wpt}")
    eprint(f"venv: {venv}")
    ensure_venv(wpt, venv, options)
    exe = venv_python(venv)
    os.chdir(wpt)
    child = [*inner_flags(wpt, options), "--"]
    try:
        os.execv(str(exe), [str(exe), str(CLI), "_manifest", *child])
    except OSError as error:
        return die("manifest", f"cannot exec {exe}: {error}")
    raise AssertionError("unreachable")


def manifest_inner_main(options: Options, argv: list[str]) -> int:
    """The `manifest` body, under the venv python (manifest has its own
    requirements)."""
    if argv[:1] == ["--"]:
        argv = argv[1:]
    if argv:
        return die("manifest", "manifest takes no arguments", 2)
    wpt = resolve_wpt_root(options)
    manifest = wpt / "MANIFEST.json"
    try:
        sys.path.insert(0, str(wpt))
        from tools import localpaths  # noqa: F401
        from tools.manifest import manifest as manifest_pkg
    except ImportError as error:
        return die("manifest", f"cannot import the manifest package: {error}", 1)

    manifest_pkg.load_and_update(str(wpt), str(manifest), "/")
    if not manifest_parses(Path(sys.executable), manifest):
        return die("manifest", f"{manifest} does not parse", 1)
    eprint(f"wpt: manifest {manifest} ({manifest.stat().st_size} bytes)")
    return 0


def score_run(
    paths: list[str], extra: list[str], save_report: Path | None, options: Options
) -> tuple[Path, int]:
    if save_report is not None:
        report = save_report
        report.parent.mkdir(parents=True, exist_ok=True)
    else:
        handle, name = tempfile.mkstemp(prefix="wpt-report-", suffix=".json")
        os.close(handle)
        report = Path(name)
    command = [
        sys.executable,
        str(CLI),
        "run",
        "webdriver",
        *paths,
        *options.flags(),
        "--log-wptreport",
        str(report),
        "--no-fail-on-unexpected",
        *extra,
    ]
    eprint(f"$ {shlex.join(command)}")
    exit_code = subprocess.run(command, cwd=str(ROOT), stdout=sys.stderr).returncode
    if exit_code != 0:
        eprint(f"runner exited {exit_code}")
    return report, exit_code


def finish_scored_run(
    report_path: Path, runner_exit: int, save_report: Path | None, started: float
) -> int:
    """Shared score/overnight tail: keep-or-delete, summarize, wall time."""
    keep_report = runner_exit != 0 or save_report is not None
    try:
        if runner_exit != 0:
            eprint(
                f"runner failed with exit {runner_exit}; report kept at {report_path}"
            )
            summarize(report_path)
            return runner_exit
        if keep_report:
            eprint(f"report kept at {report_path}")
        return summarize(report_path)
    finally:
        if not keep_report:
            try:
                os.unlink(report_path)
            except OSError:
                pass
        eprint(f"wall time: {time.monotonic() - started:.1f}s")


def score_main(options: Options, argv: list[str]) -> int:
    report, save_report, paths, extra, dry_run, error = split_score_argv(argv)
    if error:
        return die("run webdriver --score", error, 2)
    if report is not None and save_report is not None:
        return die(
            "run webdriver --score",
            "--report summarizes an existing report; do not combine it with --save-report",
            2,
        )
    if has_option("--skip-timeout", extra):
        return die(
            "run webdriver --score",
            "--skip-timeout is a dev-loop flag for `run`; a score must run expected TIMEOUTs",
            2,
        )
    if report is not None:
        if paths or extra or dry_run:
            return die(
                "run webdriver --score",
                "--report summarizes an existing report; give no test paths",
                2,
            )
        return summarize(report)
    if dry_run:
        eprint(
            f"$ {CLI} run webdriver {shlex.join([*paths, '--log-wptreport', '<tmp>', '--no-fail-on-unexpected', *extra])}"
        )
        return 0
    if not paths:
        return die("run webdriver --score", "give test paths or --report FILE", 2)
    started = time.monotonic()
    try:
        report_path, runner_exit = score_run(paths, extra, save_report, options)
    except OSError as error:
        return die("run webdriver --score", f"could not start the runner: {error}")
    return finish_scored_run(report_path, runner_exit, save_report, started)


def rerun_spawn(
    tests: list[str], extra: list[str], save_report: Path | None, options: Options
) -> tuple[Path, int]:
    handle, name = tempfile.mkstemp(prefix="wpt-rerun-", suffix=".txt")
    os.close(handle)
    include_file = Path(name)
    include_file.write_text("\n".join(tests) + "\n")
    if save_report is not None:
        report = save_report
        report.parent.mkdir(parents=True, exist_ok=True)
    else:
        handle, out_name = tempfile.mkstemp(prefix="wpt-rerun-report-", suffix=".json")
        os.close(handle)
        report = Path(out_name)
    command = [
        sys.executable,
        str(CLI),
        "run",
        "webdriver",
        "--include-file",
        str(include_file),
        *options.flags(),
        "--log-wptreport",
        str(report),
        "--no-fail-on-unexpected",
        *extra,
    ]
    eprint(f"$ {shlex.join(command)}")
    try:
        exit_code = subprocess.run(command, cwd=str(ROOT), stdout=sys.stderr).returncode
    finally:
        try:
            os.unlink(include_file)
        except OSError:
            pass
    if exit_code != 0:
        eprint(f"runner exited {exit_code}")
    return report, exit_code


def rerun_main(options: Options, argv: list[str]) -> int:
    report, include_timeout, dry_run, save_report, extra, error = parse_rerun_argv(argv)
    if error:
        return die("rerun", error, 2)
    assert report is not None
    try:
        tests = failing_tests(report, include_timeout)
    except OSError as error:
        return die("rerun", f"cannot read report {report}: {error}", 2)
    except json.JSONDecodeError as error:
        return die("rerun", f"report {report} is not valid JSON ({error})", 2)
    if not tests:
        eprint(f"nothing to rerun in {report}")
        return 0
    eprint(f"{len(tests)} tests to rerun")
    if dry_run:
        for test in tests:
            print(test)
        return 0
    started = time.monotonic()
    try:
        report_path, runner_exit = rerun_spawn(tests, extra, save_report, options)
    except OSError as error:
        return die("rerun", f"could not start the runner: {error}", 2)
    summarize(report_path)
    try:
        remaining = failing_tests(report_path, include_timeout=True)
    except OSError as error:
        eprint(f"rerun: no usable report at {report_path}: {error}")
        return runner_exit or 2
    except json.JSONDecodeError as error:
        eprint(f"rerun: report {report_path} is not valid JSON ({error})")
        return runner_exit or 2
    status, keep = rerun_status(len(remaining), runner_exit)
    eprint(f"wall time: {time.monotonic() - started:.1f}s")
    if runner_exit != 0:
        eprint(f"runner exited {runner_exit}; report kept at {report_path}")
    elif remaining:
        eprint(f"{len(remaining)} tests still need rerun; report at {report_path}")
    else:
        eprint("all selected tests pass")
    if not keep:
        try:
            os.unlink(report_path)
        except OSError:
            pass
    return status


def overnight_main(options: Options, argv: list[str]) -> int:
    save_report: Path | None = None
    extra: list[str] = []
    dry_run = False
    i = 0
    rest = list(argv)
    while i < len(rest):
        arg = rest[i]
        if arg == "--":
            extra = rest[i + 1 :]
            break
        if arg == "--save-report" or arg.startswith("--save-report="):
            option, value = arg.split("=", 1) if "=" in arg else (arg, None)
            if value is None:
                if i + 1 >= len(rest):
                    return die("overnight", "--save-report needs a file", 2)
                value = rest[i + 1]
                i += 1
            if not value:
                return die("overnight", "--save-report needs a file", 2)
            save_report = Path(value)
        elif arg == "--dry-run":
            dry_run = True
        elif arg.startswith("-"):
            extra.extend(rest[i:])
            break
        else:
            return die("overnight", f"unexpected argument {arg}", 2)
        i += 1
    if has_option("--skip-timeout", extra):
        return die(
            "overnight",
            "--skip-timeout is a dev-loop flag for `run`; a score must run expected TIMEOUTs",
            2,
        )
    if dry_run:
        eprint(
            f"$ {CLI} run --log-wptreport <report> --no-fail-on-unexpected {shlex.join(extra)}"
        )
        return 0
    started = time.monotonic()
    try:
        report_path, runner_exit = score_run([], extra, save_report, options)
    except OSError as error:
        return die("overnight", f"could not start the runner: {error}")
    return finish_scored_run(report_path, runner_exit, save_report, started)


_FAILURE_BLOCK = "  ▶ "
_FAILURE_UNEXPECTED = re.compile(r"^Unexpected (subtest )?result in ")
_FAILURE_EXPECTED = re.compile(r"^[A-Z][A-Z_-]* \[expected[^\]]*\] ")
_FAILURE_STATUS = re.compile(r"^[A-Z][A-Z_-]* ")
_FAILURE_FIRST_FIELD = re.compile(r" .*")
_FAILURE_SUBTEST = re.compile(r"^  [│└] [A-Z][A-Z_-]*( |$)")
_FAILURE_SUBTEST_LEAD = re.compile(r"^  [│└] +")
_FAILURE_TRAILING_COLON = re.compile(r":$")

FAILURES_USAGE = "failures [LOG...]   (or pipe one log on stdin)"

BASELINE_USAGE = """baseline REV PATH... [-- upstream-flags...]

Run the same WPT slice against REV and the working tree, then diff the two
canonical failure lists. Both runs use this checkout's WPT, venv, and
manifest; only the browser binary differs, so a fresh failure is the working
tree's. REV must pin the same third_party build submodules as this checkout.
The baseline binary is built in a detached worktree with its own target dir,
and that worktree is removed on exit; run logs stay in the temp dir printed.

Exit: 0 no new failures, 1 new failures, 2 usage, setup, or runner error."""


def failure_lines(lines: Iterable[str]) -> list[str]:
    """One canonical line per failing result: `STATUS PATH :: SUBTEST`.

    Top-level results (an ERROR or TIMEOUT with no failing subtests) print as
    `STATUS PATH :: [top-level]`; subtests print their own line. Sorted and
    deduplicated, so two such lists compare with set operations.
    """
    out: list[str] = []
    path = ""
    for line in lines:
        if line.startswith(_FAILURE_BLOCK):
            header = line[len(_FAILURE_BLOCK) :]
            if _FAILURE_UNEXPECTED.match(header):
                path = _FAILURE_UNEXPECTED.sub("", header)
            else:
                top = _FAILURE_FIRST_FIELD.sub("", header)
                path = header
                if _FAILURE_EXPECTED.match(path):
                    path = _FAILURE_EXPECTED.sub("", path)
                else:
                    path = _FAILURE_STATUS.sub("", path)
                out.append(f"{top} {path} :: [top-level]")
            path = _FAILURE_TRAILING_COLON.sub("", path)
        elif _FAILURE_SUBTEST.match(line):
            body = _FAILURE_SUBTEST_LEAD.sub("", line)
            status = _FAILURE_FIRST_FIELD.sub("", body)
            if _FAILURE_EXPECTED.match(body):
                body = _FAILURE_EXPECTED.sub("", body)
            else:
                body = _FAILURE_STATUS.sub("", body)
            if path and body:
                out.append(f"{status} {path} :: {body}")
    return sorted(set(out))


def read_log(path: Path) -> str | None:
    try:
        return path.read_text(errors="replace")
    except OSError as error:
        eprint(f"cannot read {path}: {error}")
        return None


def failures_main(argv: list[str]) -> int:
    if argv and argv[0] in ("-h", "--help"):
        print(FAILURES_USAGE)
        return 0
    if not argv:
        text = sys.stdin.read()
    else:
        parts: list[str] = []
        for name in argv:
            part = read_log(Path(name))
            if part is None:
                return 1
            parts.append(part)
        text = "\n".join(parts)
    for line in failure_lines(text.splitlines()):
        print(line)
    return 0


def _remove_worktree(worktree: Path) -> None:
    subprocess.run(
        ["git", "worktree", "remove", "--force", str(worktree)],
        cwd=str(ROOT),
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        check=False,
    )
    if worktree.exists():
        shutil.rmtree(worktree, ignore_errors=True)


def build_baseline_binary(worktree: Path, tmp: Path) -> Path | None:
    """Builds REV's browser in its worktree, returning the copied binary.

    The worktree build gets its own target dir: the machine shares one target
    dir across checkouts (~/.cargo/config.toml), and same-version path
    dependencies from another revision would otherwise poison this checkout's
    fingerprints (e.g. an old webidl-bindgen reused by the new build script).
    """
    env = {**os.environ, "CARGO_TARGET_DIR": str(tmp / "target")}
    cargo_toml = (worktree / "Cargo.toml").read_text(errors="replace")
    if any(line.startswith("[profile.wpt]") for line in cargo_toml.splitlines()):
        profile = ["--profile", "wpt"]
        build = ["cargo", "build", "-q", "--profile", "wpt"]
    else:
        profile = []
        env["CARGO_PROFILE_DEV_OPT_LEVEL"] = "2"
        build = ["cargo", "build", "-q"]
    if subprocess.run(build, cwd=str(worktree), env=env).returncode != 0:
        return None
    located = subprocess.run(
        [str(ROOT / "tools" / "binary-path"), *profile],
        cwd=str(worktree),
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        check=False,
    )
    source = Path(located.stdout.decode(errors="replace").strip())
    if located.returncode != 0 or not source.is_file():
        return None
    out = tmp / "tinybrowser-baseline"
    shutil.copy2(source, out)
    return out


def baseline_score(
    paths: list[str],
    runner: list[str],
    flags: list[str],
    binary: Path | None,
    log: Path,
) -> int:
    """Runs one side of the diff, streaming everything it prints into `log`."""
    command = [sys.executable, str(CLI), "run", "webdriver", *paths, *flags, "--score"]
    if binary is not None:
        command += ["--binary", str(binary)]
    command += runner
    eprint(f"$ {shlex.join(command)} > {log}")
    with open(log, "w") as handle:
        return subprocess.run(
            command, cwd=str(ROOT), stdout=handle, stderr=subprocess.STDOUT
        ).returncode


def baseline_main(options: Options, argv: list[str]) -> int:
    if argv and argv[0] in ("-h", "--help"):
        print(BASELINE_USAGE)
        return 0
    if len(argv) < 2:
        return die("baseline", BASELINE_USAGE, 2)
    rev, rest = argv[0], argv[1:]
    cut = rest.index("--") if "--" in rest else len(rest)
    paths, runner = rest[:cut], rest[cut:]
    if not paths:
        return die("baseline", "no test paths given", 2)
    if git(["rev-parse", "--verify", "--quiet", f"{rev}^{{commit}}"]) is None:
        return die("baseline", f"{rev} is not a commit", 2)

    tmp = Path(
        tempfile.mkdtemp(
            prefix="tinybrowser-baseline.", dir=os.environ.get("TMPDIR") or "/tmp"
        )
    )
    worktree = tmp / "tree"
    keep_logs = False
    try:
        if git(["worktree", "add", "--detach", str(worktree), rev]) is None:
            return die("baseline", f"cannot create a worktree at {worktree}", 2)
        eprint(f"baseline: {rev} -> {worktree}")
        for sub in (
            "third_party/rquickjs",
            "third_party/blitz",
            "third_party/html5ever",
        ):
            pinned = git(["rev-parse", f"{rev}:{sub}"])
            actual = git(["rev-parse", "HEAD"], cwd=ROOT / sub)
            if pinned is None or pinned != actual:
                eprint(
                    f"baseline: {rev} pins {sub} at {(pinned or 'nothing')[:12]}, "
                    f"this checkout has {(actual or 'nothing')[:12]}"
                )
                eprint("baseline: update that submodule to the pinned commit and retry")
                return 2
            dest = worktree / sub
            shutil.rmtree(dest, ignore_errors=True)
            shutil.copytree(ROOT / sub, dest, symlinks=True)
        eprint(f"baseline: building {rev}")
        baseline_bin = build_baseline_binary(worktree, tmp)
        if baseline_bin is None:
            return die("baseline", f"could not build {rev}", 2)

        flags = replace(options, binary=None).flags()
        baseline_log = tmp / "baseline.log"
        current_log = tmp / "current.log"
        eprint(f"baseline: running the slice at {rev}")
        base_status = baseline_score(paths, runner, flags, baseline_bin, baseline_log)
        eprint("baseline: running the slice at the working tree")
        current_status = baseline_score(paths, runner, flags, None, current_log)
        keep_logs = True
        if base_status != 0 or current_status != 0:
            eprint(
                f"baseline: runner failed (baseline {base_status}, "
                f"working tree {current_status}); logs: {tmp}"
            )
            return 2

        base_failures = failure_lines((read_log(baseline_log) or "").splitlines())
        current_failures = failure_lines((read_log(current_log) or "").splitlines())
        new = sorted(set(current_failures) - set(base_failures))
        fixed = sorted(set(base_failures) - set(current_failures))
        print(f"baseline {rev}: {len(base_failures)} failing results")
        print(f"working tree:   {len(current_failures)} failing results")
        if fixed:
            print()
            print("only in baseline (fixed):")
            for line in fixed:
                print(f"  {line}")
        if new:
            print()
            print("only in the working tree (new failures):")
            for line in new:
                print(f"  {line}")
            print()
            print(f"logs: {tmp}")
            return 1
        print()
        print(f"no new failures; logs: {tmp}")
        return 0
    finally:
        _remove_worktree(worktree)
        if not keep_logs:
            shutil.rmtree(tmp, ignore_errors=True)


def _selftest() -> None:
    assert exclude_skips_worker_variants(["worker"])
    assert exclude_skips_worker_variants(["/worker/"])
    assert not exclude_skips_worker_variants(["workers"])
    assert not exclude_skips_worker_variants(None)
    assert is_worker_variant_url("/dom/events/Event-isTrusted.any.worker.html")
    assert is_worker_variant_url("/dom/events/Event-isTrusted.any.worker.html?foo=1")
    assert is_worker_variant_url("/x.worker.html")
    assert is_worker_variant_url("/x.any.sharedworker.html")
    assert is_worker_variant_url("/x.any.serviceworker.html")
    assert is_worker_variant_url("/x.any.worker-module.html")
    assert is_worker_variant_url(
        "/html/canvas/offscreen/layers/2d.layer.clearRect.full.w.html"
    )
    assert is_worker_variant_url(
        "/html/canvas/offscreen/layers/2d.layer.clearRect.full.w.html?foo=1"
    )
    assert not is_worker_variant_url(
        "/html/canvas/offscreen/layers/2d.layer.clearRect.full.html"
    )
    assert not is_worker_variant_url("/dom/misc/file.w.html")
    assert is_worker_variant_url(
        "/referrer-policy/gen/top.http-rp/unset/worker-classic.http.html"
    )
    assert is_worker_variant_url(
        "/referrer-policy/gen/top.http-rp/unset/sharedworker-module.https.sub.html"
    )
    assert not is_worker_variant_url("/workers/Worker-basic.html")
    assert not is_worker_variant_url("/dom/events/Event-isTrusted.any.html")
    assert not is_worker_variant_url("/dom/events/Event-isTrusted.any.html?foo=1")
    assert not is_worker_variant_url("/dom/events/Event-dispatch-click.tentative.html")

    assert has_option("--processes", ["--processes", "4"])
    assert has_option("--processes", ["--processes=4"])
    assert has_option("--processes", ["--proc", "4"])
    assert not has_option("--processes", ["--pause-after-test"])
    assert has_option_exact("--manifest", ["--manifest", "x"])
    assert not has_option_exact("--manifest", ["--manif", "x"])

    opts, rest, err = extract_options(
        [
            "run",
            "dom/",
            "--binary",
            "/x",
            "--no-lock",
            "--",
            "--binary",
            "wpt-value",
        ]
    )
    assert err is None, err
    assert rest == ["run", "dom/", "--", "--binary", "wpt-value"]
    assert opts.binary == "/x" and opts.no_lock
    assert extract_options(["--binary"])[2] is not None
    assert extract_options(["--binary="])[2] is not None
    assert extract_options(["--binary", "--log-level=trace"])[2] is not None
    assert extract_options(["--log-level="])[2] is not None
    assert extract_options(["--no-lock=1"])[2] is not None
    assert extract_options(["--lock-timeout", "soon"])[2] is not None
    assert extract_options(["--lock-timeout", "-1"])[2] is not None
    assert extract_options([])[0] == Options()
    chosen = Options(
        binary="/x",
        wpt_root="/w",
        venv="/v",
        log_level="trace",
        log_file="/log",
        allow_pin_mismatch=True,
        no_lock=True,
        lock_timeout=7,
    )
    assert extract_options(chosen.flags())[0] == chosen

    with contextlib.redirect_stderr(io.StringIO()):
        assert run_main(Options(), []) == 2
        assert run_main(Options(), ["bogus", "dom/"]) == 2
    assert LANES == ("in-process", "webdriver", "test262")

    paths, score, dry, extras, err = split_run_argv(["dom/nodes/", "--exclude=worker"])
    assert (
        err is None
        and paths == ["dom/nodes/"]
        and extras == ["--exclude=worker"]
        and not score
        and not dry
    )
    assert browser_policy_path("/fetch/orb/x.html")
    assert browser_policy_path("/html/browsers/windows/opener.html")
    assert not browser_policy_path("/fetch/api/x.html")
    assert in_process_excludes(["/"]) == list(BROWSER_POLICY_PREFIXES)
    assert "fetch/orb" not in in_process_excludes(["fetch/orb/x.html"])
    assert "fetch/http-cache" in in_process_excludes(["fetch/orb"])
    assert harness_paths(
        ["fetch/orb/", "--report", "out.json", "--jobs", "2", "--exclude", "worker"]
    ) == ["fetch/orb/"]
    paths, score, dry, extras, err = split_run_argv(
        ["--score", "dom/", "--", "--processes", "4"]
    )
    assert (
        err is None and score and paths == ["dom/"] and extras == ["--processes", "4"]
    )
    paths, score, dry, extras, err = split_run_argv(
        ["--test-types", "reftest", "css/", "--", "--processes", "2"]
    )
    assert (
        err is None
        and paths == ["css/"]
        and extras == ["--test-types", "reftest", "--processes", "2"]
    )
    _, _, _, _, err = split_run_argv(["--test-types", "testharnes", "dom/"])
    assert err is not None

    paths, extras, err = hoist_paths_after_test_types(
        [], ["--processes", "4", "--test-types", "reftest", "dom/nodes/"]
    )
    assert (
        err is None
        and paths == ["dom/nodes/"]
        and extras == ["--processes", "4", "--test-types", "reftest"]
    )
    _, _, err = hoist_paths_after_test_types([], ["--test-types", "testharnes", "dom/"])
    assert err is not None  # typo fails loudly instead of running the wrong set
    _, _, err = hoist_paths_after_test_types([], ["--test-types", "testharnes"])
    assert err is not None

    assert corpus_counts(
        {
            "/a.any.html": {"s": "P", "c": [2, 3]},
            "/a.any.worker.html": {"s": "P", "c": [0, 3]},
            "/reftest.html": {"s": "P", "c": [0, 0]},
            "/broken": {"c": "nope"},
            "not-a-path": {"c": [1, 1]},
        }
    ) == {"/a.any.html": 3, "/a.any.worker.html": 3}
    ok = {"test": "/dom/ok.html", "status": "PASS"}
    skip = {
        "test": "/wasm/x.any.js",
        "status": "SKIP",
        "message": "does not support jsshell",
    }
    timeout = {"test": "/WebCryptoAPI/x.html", "status": "TIMEOUT"}
    error = {"test": "/html/y.html", "status": "ERROR", "expected": "OK"}
    failed = {"test": "/FileAPI/z.html", "status": "FAIL", "expected": "PASS"}
    baselined = {"test": "/dom/baselined.html", "status": "FAIL"}
    unpinned = {"test": "/dom/fixed.html", "status": "PASS", "expected": "FAIL"}
    sub = {
        "test": "/dom/s.html",
        "status": "OK",
        "subtests": [{"name": "a", "status": "FAIL", "expected": "PASS"}],
    }
    sub_baselined = {
        "test": "/dom/sb.html",
        "status": "OK",
        "subtests": [{"name": "a", "status": "FAIL"}],
    }
    expected = {
        "test": "/dom/e.html",
        "status": "OK",
        "subtests": [{"name": "a", "status": "PASS", "expected": "FAIL"}],
    }
    assert not needs_retest(ok, include_timeout=False)
    assert not needs_retest(skip, include_timeout=False)
    assert not needs_retest(timeout, include_timeout=False)
    assert needs_retest(timeout, include_timeout=True)
    assert needs_retest(error, include_timeout=False)
    assert needs_retest(failed, include_timeout=False)
    assert not needs_retest(baselined, include_timeout=False)
    assert needs_retest(unpinned, include_timeout=False)
    assert needs_retest(sub, include_timeout=False)
    assert not needs_retest(sub_baselined, include_timeout=False)
    assert needs_retest(expected, include_timeout=False)
    assert rerun_status(0, 0) == (0, False)
    assert rerun_status(0, 64) == (64, True)
    assert rerun_status(2, 0) == (1, True)

    report, save, paths, extra, dry, err = split_score_argv(
        ["FileAPI/", "--save-report", "/tmp/x.json", "--", "--exclude=worker"]
    )
    assert (
        report is None
        and save == Path("/tmp/x.json")
        and paths == ["FileAPI/"]
        and extra == ["--exclude=worker"]
        and not dry
        and err is None
    )
    report, save, paths, extra, dry, err = split_score_argv(["--dry-run", "dom/"])
    assert dry and paths == ["dom/"] and err is None
    r, _, _, _, _, err = parse_rerun_argv(
        ["r.json", "--include-timeout", "--", "--processes", "8"]
    )
    assert r == Path("r.json") and err is None
    assert parse_rerun_argv([])[5] is not None

    with tempfile.TemporaryDirectory() as tmp:
        journal = Path(tmp) / "journal.jsonl"
        assert read_journal(journal) == (0, [])
        journal.write_text(
            '{"event":"start","id":"/a.html"}\n'
            '{"event":"done","outcome":{"id":"/a.html","status":"PASS"}}\n'
            "3\n"
            '{"event":"start","id":"/b.html"}\n'
            '{"event":"done","outcome":{"id":"/b.html","sta'
        )
        assert read_journal(journal) == (1, ["/b.html"])

    _selftest_profile()
    _selftest_failures()
    _selftest_baseline()
    _selftest_corpus()
    _selftest_merge()
    _selftest_publish()
    _selftest_summary()


def _selftest_profile() -> None:
    """profile: ranks a report by wall time, and reads the browser log it asked for."""
    with tempfile.TemporaryDirectory() as tmp:
        report = Path(tmp) / "r.json"
        report.write_text(
            json.dumps(
                {
                    "results": [
                        {"test": "/a.html", "status": "PASS", "duration": 100},
                        {"test": "/b.html", "status": "TIMEOUT", "duration": 7192},
                        {"test": "/c.html", "status": "PASS", "duration": 2600},
                    ]
                }
            )
        )
        stdout = io.StringIO()
        with contextlib.redirect_stdout(stdout):
            assert profile_main(Options(), ["--report", str(report), "--top=2"]) == 0
        text = stdout.getvalue()
        assert text.index("/b.html") < text.index("/c.html")  # longest first
        assert "/a.html" not in text  # --top=2 drops it
        assert "1 killed" in text and "1 passing within 2500ms" in text

        log = Path(tmp) / "default.log"
        log.write_text(
            "2026-10-17T08:36:48.751Z TRACE browser::net nav start http://x/\n"
            "2026-10-17T08:36:48.752Z INFO renderer ready\n"
            "2026-10-17T08:36:48.772Z TRACE browser::net nav head http://x/ -> 200\n"
        )
        stdout = io.StringIO()
        with contextlib.redirect_stdout(stdout):
            print_timeline(log)
        text = stdout.getvalue()
        assert "nav start" in text and "nav head" in text
        assert "renderer ready" not in text

        for bad in (
            ["--top", "x", "--report", str(report)],
            ["--top=0", "--report", str(report)],
            ["--report"],
            [],
            ["--report", str(report), "dom/"],
            ["--bogus"],
        ):
            with (
                contextlib.redirect_stderr(io.StringIO()),
                contextlib.redirect_stdout(io.StringIO()),
            ):
                assert profile_main(Options(), bad) == 2, bad


def _selftest_failures() -> None:
    """failures: the canonical list is a pure fold over the runner's blocks."""
    log = (
        "  ▶ ERROR [expected OK] /a/top.html\n"
        "  ▶ TIMEOUT [expected OK] /b/timeout.html\n"
        "  ▶ NOTRUN /c/notrun.html\n"
        "  ▶ Unexpected subtest result in /d/sub.html:\n"
        "  │ FAIL [expected PASS] first subtest\n"
        "  │   → assert_equals: expected 1 but got 2\n"
        "  │ \n"
        "  │     at foo (eval:1:1)\n"
        "  └     at bar (eval:2:2)\n"
        "  ▶ Unexpected subtest result in /e/multi.html:\n"
        "  │ TIMEOUT subtest with status only\n"
        "  ▶ Unexpected subtest result in /d/sub.html:\n"
        "  │ FAIL [expected PASS] first subtest\n"
    )
    assert failure_lines(log.splitlines()) == [
        "ERROR /a/top.html :: [top-level]",
        "FAIL /d/sub.html :: first subtest",
        "NOTRUN /c/notrun.html :: [top-level]",
        "TIMEOUT /b/timeout.html :: [top-level]",
        "TIMEOUT /e/multi.html :: subtest with status only",
    ]
    assert failure_lines(["noise", "  │ FAIL [expected PASS] x"]) == []
    with tempfile.TemporaryDirectory() as tmp:
        path = Path(tmp) / "score.log"
        path.write_text(log)
        stdout = io.StringIO()
        with contextlib.redirect_stdout(stdout):
            assert failures_main([str(path)]) == 0
        assert "ERROR /a/top.html :: [top-level]" in stdout.getvalue()
        with contextlib.redirect_stdout(io.StringIO()):
            assert failures_main(["--help"]) == 0
        with (
            contextlib.redirect_stderr(io.StringIO()),
            contextlib.redirect_stdout(io.StringIO()),
        ):
            assert failures_main([str(Path(tmp) / "missing.log")]) == 1


def _selftest_baseline() -> None:
    """baseline: the argument grammar rejects before it touches the tree."""
    with contextlib.redirect_stdout(io.StringIO()):
        assert baseline_main(Options(), ["--help"]) == 0
    with (
        contextlib.redirect_stderr(io.StringIO()),
        contextlib.redirect_stdout(io.StringIO()),
    ):
        assert baseline_main(Options(), []) == 2
        assert baseline_main(Options(), ["d78b30e"]) == 2
        assert baseline_main(Options(), ["d78b30e", "--"]) == 2
        assert baseline_main(Options(), ["not-a-commit", "dom/"]) == 2


def _selftest_corpus() -> None:
    """corpus: extraction, the gzipped round trip, and the argument grammar."""
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        summary_path = tmp / "summary.json"
        summary_path.write_text(
            json.dumps(
                {
                    "/a.any.html": {"s": "P", "c": [1, 2]},
                    "/a.any.worker.html": {"s": "P", "c": [0, 2]},
                    "/reftest.html": {"s": "P", "c": [0, 0]},
                }
            )
        )
        out = tmp / "corpus.json.gz"
        with contextlib.redirect_stderr(io.StringIO()):
            assert corpus_main(["--summary", str(summary_path), "--out", str(out)]) == 0
        assert json.loads(gzip.decompress(out.read_bytes())) == {
            "/a.any.html": 2,
            "/a.any.worker.html": 2,
        }
        assert out.read_bytes()[:2] == b"\x1f\x8b"
        assert pinned_corpus("abc").name == "abc.json.gz"
        assert pinned_corpus("abc").parent.name == "corpus"

        with (
            contextlib.redirect_stderr(io.StringIO()),
            contextlib.redirect_stdout(io.StringIO()),
        ):
            assert corpus_main([]) == 2
            assert corpus_main(["--summary", str(summary_path)]) == 2
            assert (
                corpus_main(
                    [
                        "--summary",
                        str(summary_path),
                        "--revision",
                        "abc",
                        "--out",
                        str(out),
                    ]
                )
                == 2
            )
            assert (
                corpus_main(["--summary", str(tmp / "missing"), "--out", str(out)]) == 2
            )


def _selftest_merge() -> None:
    """Fabricated chunk reports: merge survives the failure modes CI has."""
    info = {"product": "tinybrowser", "os": "linux"}

    def chunk(results, **top):
        data = {"results": results}
        data.update(top)
        return json.dumps(data)

    def result(test, subsuite=None):
        out = {"test": test, "status": "PASS", "subtests": []}
        if subsuite is not None:
            out["subsuite"] = subsuite
        return out

    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        a, b = tmp / "a.json", tmp / "b.json"
        corrupt, empty = tmp / "corrupt.json", tmp / "empty.json"
        a.write_text(
            chunk(
                [result("/b.html"), result("/a.html")],
                run_info=info,
                time_start=10,
                time_end=20,
            )
        )
        b.write_text(
            chunk([result("/c.html")], run_info=info, time_start=5, time_end=30)
        )
        corrupt.write_text('{"results": [')
        empty.write_text(chunk([]))

        merged_path = tmp / "merged.json"
        record_path = tmp / "run.json"
        stdout, stderr = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            assert (
                merge_main(
                    [
                        str(a),
                        str(b),
                        "--out",
                        str(merged_path),
                        "--summary-out",
                        str(record_path),
                        "--product-revision",
                        "abc123",
                    ]
                )
                == 0
            )
        merged = json.loads(merged_path.read_text())
        assert [r["test"] for r in merged["results"]] == [
            "/a.html",
            "/b.html",
            "/c.html",
        ]
        assert merged["run_info"] == info
        assert merged["time_start"] == 5 and merged["time_end"] == 30

        record = json.loads(record_path.read_text())
        assert record["schema"] == 2
        assert record["product_revision"] == "abc123"
        assert record["wpt_revision"] is None
        assert record["totals"]["tests"] == 3
        assert record["totals"]["pass"] == 3
        assert record["areas"]["(root)"]["tests"] == 3
        assert record["failures"] == []

        corpus = tmp / "corpus.json.gz"
        write_corpus(corpus, {"/a.html": 4, "/b.html": 2, "/a.worker.html": 3})
        corpus_record = tmp / "run-corpus.json"
        with (
            contextlib.redirect_stdout(io.StringIO()),
            contextlib.redirect_stderr(io.StringIO()),
        ):
            assert (
                merge_main(
                    [
                        str(a),
                        str(b),
                        "--summary-out",
                        str(corpus_record),
                        "--corpus",
                        str(corpus),
                    ]
                )
                == 0
            )
        enrolled = json.loads(corpus_record.read_text())
        assert enrolled["totals"]["subtests"] == 9
        assert enrolled["totals"]["subtest_unreported"] == 9
        assert enrolled["totals"]["pass"] == 3
        assert enrolled["areas"]["(root)"]["subtests"] == 9
        assert enrolled["areas"]["(root)"]["subtest_unreported"] == 9
        with (
            contextlib.redirect_stdout(io.StringIO()),
            contextlib.redirect_stderr(io.StringIO()),
        ):
            assert (
                merge_main(
                    [
                        str(a),
                        "--summary-out",
                        str(corpus_record),
                        "--corpus",
                        str(tmp / "missing"),
                    ]
                )
                == 2
            )

        pinned = tmp / "pinned.json"
        unknown = "0" * 40
        pinned.write_text(
            chunk([result("/a.html")], run_info={**info, "revision": unknown})
        )
        pass
        with (
            contextlib.redirect_stdout(io.StringIO()),
            contextlib.redirect_stderr(io.StringIO()),
        ):
            assert (
                merge_main(
                    [str(pinned), "--summary-out", str(tmp / "pinned-record.json")]
                )
                == 2
            )

        failing = tmp / "failing.json"
        failing.write_text(
            chunk(
                [
                    {
                        "test": "/dom/x.html",
                        "status": "OK",
                        "subtests": [
                            {"name": "s", "status": "FAIL", "expected": "PASS"}
                        ],
                    }
                ]
            )
        )
        record_path = tmp / "run-failing.json"
        with (
            contextlib.redirect_stdout(io.StringIO()),
            contextlib.redirect_stderr(io.StringIO()),
        ):
            assert merge_main([str(failing), "--summary-out", str(record_path)]) == 0
        record = json.loads(record_path.read_text())
        assert record["totals"]["unexpected"] == 1
        assert record["totals"]["subtest_fail"] == 1
        assert record["areas"]["dom"]["unexpected"] == 1
        assert record["failures"] == [
            {"test": "/dom/x.html", "subtest": "s", "status": "FAIL"}
        ]

        stdout, stderr = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            assert merge_main([str(corrupt), str(empty), str(a)]) == 0
        assert "skipping" in stderr.getvalue()
        assert "no results in" in stderr.getvalue()
        assert "TOTAL" in stdout.getvalue()

        stdout, stderr = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            assert merge_main(["--strict-skips", str(corrupt), str(a)]) == 2
        assert "TOTAL" in stdout.getvalue()
        with (
            contextlib.redirect_stdout(io.StringIO()),
            contextlib.redirect_stderr(io.StringIO()),
        ):
            assert merge_main(["--strict-skips", str(a)]) == 0

        dup = tmp / "dup.json"
        dup.write_text(chunk([result("/a.html")]))
        stderr = io.StringIO()
        with (
            contextlib.redirect_stderr(stderr),
            contextlib.redirect_stdout(io.StringIO()),
        ):
            assert merge_main([str(a), str(dup)]) == 2
        assert f"/a.html appears in {a} and {dup}" in stderr.getvalue()
        twice = tmp / "twice.json"
        twice.write_text(chunk([result("/a.html"), result("/a.html")]))
        stderr = io.StringIO()
        with (
            contextlib.redirect_stderr(stderr),
            contextlib.redirect_stdout(io.StringIO()),
        ):
            assert merge_main([str(twice)]) == 2
        assert f"/a.html appears twice in {twice}" in stderr.getvalue()
        one, two = tmp / "one.json", tmp / "two.json"
        one.write_text(chunk([result("/a.html", subsuite="one")]))
        two.write_text(chunk([result("/a.html", subsuite="two")]))
        with contextlib.redirect_stdout(io.StringIO()):
            assert merge_main([str(one), str(two)]) == 0

        with (
            contextlib.redirect_stderr(io.StringIO()),
            contextlib.redirect_stdout(io.StringIO()),
        ):
            assert merge_main([str(corrupt)]) == 2
            assert merge_main([str(empty)]) == 1


def _read_failures_gz(path: Path) -> list[dict]:
    if not path.is_file():
        return []
    try:
        with gzip.open(path, "rt", encoding="utf-8") as handle:
            return [
                json.loads(line) for line in handle.read().splitlines() if line.strip()
            ]
    except (OSError, EOFError):
        return []


def _selftest_publish() -> None:
    """Two records: one point, then a diff, then a pin change kills the diff.

    The failure list lives in a gzipped sidecar, so the assertions read it
    back through `read_failures` — the same path the next publish uses.
    """
    record = {
        "schema": 2,
        "product_revision": "aaa",
        "wpt_revision": "w1",
        "time_end": 1_800_000_000,
        "totals": {"tests": 3, "subtests": 10},
        "areas": {},
        "failures": [
            {"test": "/dom/a.html", "subtest": "", "status": "TIMEOUT"},
            {"test": "/dom/b.html", "subtest": "", "status": "FAIL"},
            {"test": "/dom/d.html", "subtest": "", "status": "TIMEOUT"},
        ],
    }
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        data = tmp / "data"
        first = tmp / "first.json"
        first.write_text(json.dumps(record))
        with (
            contextlib.redirect_stdout(io.StringIO()),
            contextlib.redirect_stderr(io.StringIO()),
        ):
            assert publish_main(["--record", str(first), "--data", str(data)]) == 0

        latest = json.loads((data / "latest.json").read_text())
        assert latest["product_revision"] == "aaa"
        assert "failures" not in latest
        assert _read_failures_gz(data / "failures.jsonl.gz") == [
            {"status": "TIMEOUT", "test": "/dom/a.html", "subtest": ""},
            {"status": "FAIL", "test": "/dom/b.html", "subtest": ""},
            {"status": "TIMEOUT", "test": "/dom/d.html", "subtest": ""},
        ]
        assert not (data / "previous.json").exists()
        index = json.loads((data / "index.json").read_text())
        assert index[0]["sha"] == "aaa" and index[0]["date"].endswith("Z")
        diff = json.loads((data / "regressions.json").read_text())
        assert diff["previous_sha"] is None and not diff["comparable"]
        assert diff["regressions"] == [] and diff["fixes"] == []

        second = tmp / "second.json"
        updated = json.loads(json.dumps(record))
        updated.update(product_revision="bbb", time_end=1_800_000_100)
        updated["totals"]["subtests"] = 20
        updated["failures"] = [
            {"test": "/dom/b.html", "subtest": "", "status": "TIMEOUT"},
            {"test": "/dom/c.html", "subtest": "s", "status": "FAIL"},
            {"test": "/dom/d.html", "subtest": "x", "status": "FAIL"},
            {"test": "/dom/d.html", "subtest": "y", "status": "FAIL"},
        ]
        second.write_text(json.dumps(updated))
        with (
            contextlib.redirect_stdout(io.StringIO()),
            contextlib.redirect_stderr(io.StringIO()),
        ):
            assert publish_main(["--record", str(second), "--data", str(data)]) == 0
        diff = json.loads((data / "regressions.json").read_text())
        assert diff["previous_sha"] == "aaa" and diff["comparable"]
        assert [(e["test"], e["status"]) for e in diff["regressions"]] == [
            ("/dom/c.html", "FAIL")
        ]
        assert [e["test"] for e in diff["new_timeouts"]] == []
        assert [(e["test"], e.get("subtest")) for e in diff["newly_measured"]] == [
            ("/dom/d.html", "x"),
            ("/dom/d.html", "y"),
        ]
        assert [e["test"] for e in diff["fixes"]] == ["/dom/a.html"]
        assert not (data / "previous.json").exists()
        assert [
            item["sha"] for item in json.loads((data / "index.json").read_text())
        ] == ["bbb", "aaa"]
        index = json.loads((data / "index.json").read_text())
        latest = json.loads((data / "latest.json").read_text())
        assert index[1]["totals"]["subtests"] == 10
        assert index[0]["totals"] == latest["totals"]

        rerun = json.loads(json.dumps(updated))
        rerun["time_end"] = 1_800_000_200
        rerun["totals"]["subtests"] = 15
        path = tmp / "rerun.json"
        path.write_text(json.dumps(rerun))
        with (
            contextlib.redirect_stdout(io.StringIO()),
            contextlib.redirect_stderr(io.StringIO()),
        ):
            assert publish_main(["--record", str(path), "--data", str(data)]) == 0
        assert [
            item["sha"] for item in json.loads((data / "index.json").read_text())
        ] == ["bbb", "aaa"]
        index = json.loads((data / "index.json").read_text())
        assert index[0]["totals"]["subtests"] == 15
        assert [item["date"] for item in json.loads((data / "index.json").read_text())][
            0
        ].startswith("2027-01-15")

        third = json.loads(json.dumps(updated))
        third.update(product_revision="ccc", wpt_revision="w2")
        path = tmp / "third.json"
        path.write_text(json.dumps(third))
        with (
            contextlib.redirect_stdout(io.StringIO()),
            contextlib.redirect_stderr(io.StringIO()),
        ):
            assert publish_main(["--record", str(path), "--data", str(data)]) == 0
        diff = json.loads((data / "regressions.json").read_text())
        assert not diff["comparable"]
        assert diff["regressions"] == [] and diff["fixes"] == []

        (data / "failures.jsonl.gz").write_bytes(b"\x1f\x8b\x08truncated")
        with contextlib.redirect_stderr(io.StringIO()):
            assert _read_failures_gz(data / "failures.jsonl.gz") == []


def _selftest_summary() -> None:
    """summary_main delegates to wpt-report and formats regressions.json."""
    with tempfile.TemporaryDirectory() as tmp:
        data = Path(tmp)
        (data / "regressions.json").write_text(
            json.dumps(
                {
                    "sha": "bbbbbbbb",
                    "previous_sha": "aaaaaaaa",
                    "wpt_revision": "rev1",
                    "comparable": True,
                    "regressions": [
                        {"test": "/a.html", "subtest": "", "status": "FAIL"},
                        {"test": "/b.html", "subtest": "sub", "status": "FAIL"},
                    ],
                    "new_timeouts": [],
                    "newly_measured": [],
                    "fixes": [{"test": "/c.html", "subtest": "", "status": "FAIL"}],
                }
            )
        )
        assert summary_main(["--data", str(data)]) == 0
        assert summary_main([]) == 2


TOP_HELP = """tinybrowser WPT harness: run | score | rerun | merge | corpus | publish | summary | profile | failures | baseline | manifest | overnight | selftest

Lanes — `run <lane>`, required, one executor each:
  run in-process PATH... [--jobs N] [--report FILE] [--baseline FILE]
    Mount testharness, crashtest, and reftest files in the engine process,
    with a paused clock. No WebDriver or renderer child. `.py` handler routes
    are forwarded to a wptserve sidecar this command starts; reftests compare
    rendered pixels. Testdriver files are never selected: `run webdriver`
    owns exactly the files this lane cannot run.
    --total-chunks N --this-chunk K splits by source directory.
    A file that aborts the engine is one ERROR or TIMEOUT: the runner journals
    each file and this command restarts it until the selection is complete.
    The journal keeps one file in flight, so --jobs is accepted but ignored.
    Directories that exercise browser-side policy (SRI, ORB, CSP redirects,
    cookies, window.open) are left to `run webdriver`; name one explicitly to
    run it here anyway.
  run webdriver PATH... [--score] [--dry-run] [--testdriver-only | --exclude-in-process] [-- upstream-flags...]
    Drive the real browser through upstream wptrunner and WebDriver. Upstream
    flags follow `--` (or start implicitly at the first `-flag`). --score runs
    then prints the per-directory table. --testdriver-only keeps only files
    that drive a WebDriver session; --exclude-in-process keeps what
    `run in-process` cannot run, so the two lanes partition the suite. That
    includes the browser-policy directories (#241): the in-process lane has no
    browser host, so SRI, ORB, CSP redirects, cookies, and window.open are
    scored only here.
    --dry-run prints the wpt invocation without building or running.
  run test262 [FILTER...] [--jobs N] [--report FILE] [--baseline FILE]
    Run the ECMAScript suite natively in QuickJS, without HTML, testharness.js,
    wptserve, or WebDriver. The suite is read-only. --report writes a
    wptreport the other readers understand; --baseline compares with an
    earlier --report and exits 1 on a regression.

A lane names the executor, and the executor decides what a result means: the
same file can time out under WebDriver and pass in-process. Every report
records the lane it came from, and no lane is a default.

Reports and diffs:
  score PATH... [--report FILE | --save-report FILE] [-- upstream-flags...]
    Summarize an existing report (--report) or run then summarize.
  rerun REPORT [--include-timeout] [--dry-run] [--save-report FILE] [-- runner...]
    Re-run only the attention set from a wptreport.
  merge REPORT... [--out FILE] [--summary-out FILE] [--product-revision SHA] [--corpus FILE] [--strict-skips]
    Fold the chunk reports of one CI run into a single score. Reports that
    are missing, corrupt, or empty are skipped with a warning; overlapping
    tests are refused. --out writes the merged wptreport, keeping run_info,
    subsuites, and the time bounds. --summary-out writes the compact run
    record (totals, per-area counters, subtest outcomes, failure list) that
    the history and the site read; --product-revision stamps it with the
    commit under test. --corpus adds the subtests a test never reported, from
    the counts `corpus` writes, so the denominator is the pinned revision's
    corpus instead of only what reached the harness. --strict-skips exits 2
    after printing the table when any report was unusable, so a dead chunk
    still fails CI.
  corpus (--revision REV | --summary FILE) --out FILE
    Write Chrome's per-test subtest counts at a WPT revision. With
    --revision it asks wpt.fyi for the newest Chrome run and downloads its
    summary; with --summary it reads that summary locally. `merge --corpus`
    consumes the result.
  publish --record FILE --data DIR [--sha SHA]
    Add one record to the data directory the results repo publishes:
    latest.json, regressions.json, index.json, and the gzipped failure
    sidecar the next run diffs against. The diff is advisory and empty when
    the WPT revision changed.
  summary --data DIR
    Print the regression summary the review pass reads from a published
    data directory: file-level regressions first, then a subtest sample,
    then the timeout/newly-measured/fix counts.
  failures [LOG...]
    Print the canonical failure list of a `--score` log, one
    `STATUS PATH :: SUBTEST` line per failing result, sorted and deduplicated.
    Reads stdin when given no logs.
  baseline REV PATH... [-- upstream-flags...]
    Run the slice at REV and in the working tree and diff the failure lists;
    exit 1 on a failure the working tree adds. The baseline browser is built
    in a detached worktree that is removed on exit.

Utilities:
  profile PATH... [--top N]
  profile --report FILE [--top N]
    Rank tests by wall time. With paths it runs them at trace with the
    browser log at a path it picks, then prints the navigation/request
    timeline. With --report it ranks one existing wptreport instead.
    The timeline separates wait-bound from compute-bound: requests that all
    end in milliseconds while the test still times out mean the renderer is
    spinning, not the network. Records sit at the browser's seams, never in
    the renderer crate (that crate is shared with wasm and carries no logger).
  manifest
    Build MANIFEST.json once per chunked CI run, so chunk jobs can skip the
    tree walk. Locally, rerun it after editing the WPT checkout.
  overnight [--save-report FILE] [--dry-run] [-- runner...]
    Full-suite run + score. Long; needs explicit approval per AGENTS.md.
  selftest
    Pure-function checks (worker filter, classify, arg grammars).

Harness flags (any subcommand, before `--`):
  --binary PATH          use a prebuilt browser binary as-is
  --fast-binary PATH     use a prebuilt wpt-fast/wpt-harness runner
  --wpt-root PATH        WPT checkout to use
  --venv PATH            WPT virtualenv to use
  --log-level LEVEL      browser log level (e.g. trace)
  --log-file PATH        write the browser log here, not under the data home
  --allow-pin-mismatch   run even when the checkout drifts from the pin
  --no-lock              skip the shared venv/manifest locks
  --lock-timeout SECONDS how long to wait for a held lock

Examples:
  cli.py run in-process dom/events/ --jobs 8
  cli.py run webdriver dom/nodes/ --exclude=worker
  cli.py run webdriver --score dom/nodes/ -- --processes 4
  cli.py run test262 test/language --jobs 12
  cli.py rerun /tmp/fileapi.json -- --processes 8 --fully-parallel
  cli.py merge reports/*.json --out merged.json
  cli.py profile referrer-policy/gen/req.rel-noref/no-referrer/a-tag.http.html
  cli.py baseline d78b30e dom/nodes/Document-adoptNode.html
"""


def main(argv: list[str] | None = None) -> int:
    argv = list(sys.argv[1:] if argv is None else argv)
    if argv == ["--selftest"]:
        argv = ["selftest"]
    if not argv or argv[0] in ("-h", "--help", "help"):
        print(TOP_HELP)
        return 0
    options, rest, error = extract_options(argv)
    if error:
        return die("wpt", error, 2)
    if not rest or rest[0] in ("-h", "--help", "help"):
        print(TOP_HELP)
        return 0
    command, args = rest[0], rest[1:]
    if command == "run":
        return run_main(options, args)
    if command == "score":
        return score_main(options, args)
    if command in ("rerun", "retest"):
        return rerun_main(options, args)
    if command == "merge":
        return merge_main(args)
    if command == "publish":
        return publish_main(args)
    if command == "summary":
        return summary_main(args)
    if command == "corpus":
        return corpus_main(args)
    if command == "profile":
        return profile_main(options, args)
    if command == "failures":
        return failures_main(args)
    if command == "baseline":
        return baseline_main(options, args)
    if command == "manifest":
        return manifest_main(options, args)
    if command == "overnight":
        return overnight_main(options, args)
    if command == "_inner":
        return inner_main(options, args)
    if command == "_manifest":
        return manifest_inner_main(options, args)
    if command == "selftest":
        _selftest()
        eprint("selftest: ok")
        return 0
    return die(
        "wpt",
        f"unknown command {command}; expected run | score | rerun | merge | publish | summary | profile | failures | baseline | manifest | overnight | selftest",
        2,
    )


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except BrokenPipeError:
        devnull = os.open(os.devnull, os.O_WRONLY)
        os.dup2(devnull, sys.stdout.fileno())
        raise SystemExit(0)
