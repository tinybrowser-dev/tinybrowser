from __future__ import annotations

import os
import shutil
import sys
import tempfile

from wptrunner.browsers.base import (
    WebDriverBrowser,
    get_timeout_multiplier,  # noqa: F401 -- used via the __wptrunner__ name below
    require_arg,
)
from wptrunner.executors import executor_kwargs as base_executor_kwargs
from wptrunner.executors.base import PytestExecutor
from wptrunner.executors.executorwebdriver import (
    WebDriverCrashtestExecutor,
    WebDriverProtocol,
    WebDriverRefTestExecutor,
    WebDriverTestharnessExecutor,
)
from wptrunner.products import Product
from wptrunner import wpttest

from timeouts import DEFAULT_TIMEOUT, EXTRA_TIMEOUT, LONG_TIMEOUT

wpttest.Test.default_timeout = DEFAULT_TIMEOUT
wpttest.Test.long_timeout = LONG_TIMEOUT
wpttest.WdspecTest.default_timeout = DEFAULT_TIMEOUT
wpttest.WdspecTest.long_timeout = LONG_TIMEOUT
wpttest.AamSpecTest.default_timeout = DEFAULT_TIMEOUT
wpttest.AamSpecTest.long_timeout = LONG_TIMEOUT
PytestExecutor.extra_timeout = EXTRA_TIMEOUT

SLOW_TEST_PREFIXES = ("referrer-policy/css-integration/svg/",)
SLOW_TEST_TIMEOUT = 6  # seconds; budget + EXTRA_TIMEOUT = 8 < testharness.js 10


def _with_slow_test_budget(original):
    """Wraps one `from_manifest` classmethod with the slow-test override."""

    def from_manifest(cls, manifest_file, *args, **kwargs):
        test = original(cls, manifest_file, *args, **kwargs)
        relative = os.path.relpath(test.path, manifest_file.tests_root)
        if relative.startswith(SLOW_TEST_PREFIXES):
            test.timeout = SLOW_TEST_TIMEOUT
        return test

    return classmethod(from_manifest)


wpttest.Test.from_manifest = _with_slow_test_budget(wpttest.Test.from_manifest.__func__)
wpttest.TestharnessTest.from_manifest = _with_slow_test_budget(
    wpttest.TestharnessTest.from_manifest.__func__
)

__wptrunner__ = {
    "product": "tinybrowser",
    "check_args": "check_args",
    "browser": "TinyBrowser",
    "executor": {
        "testharness": "TinyBrowserTestharnessExecutor",
        "crashtest": "TinyBrowserCrashtestExecutor",
        "reftest": "TinyBrowserRefTestExecutor",
        "test262": "TinyBrowserTestharnessExecutor",
        "wdspec": "PytestExecutor",
        "aamtest": "PytestExecutor",
    },
    "browser_kwargs": "browser_kwargs",
    "executor_kwargs": "executor_kwargs",
    "env_extras": "env_extras",
    "env_options": "env_options",
    "timeout_multiplier": "get_timeout_multiplier",
}


def load():
    return Product._from_dunder_wptrunner(sys.modules[__name__])


def check_args(**kwargs):
    require_arg(kwargs, "binary")


def browser_kwargs(logger, test_type, run_info_data, config, subsuite, **kwargs):
    ssl_config = getattr(config, "ssl_config", None) or {}
    return {
        "binary": kwargs["binary"],
        "binary_args": kwargs.get("binary_args") or [],
        "webdriver_host": "127.0.0.1",
        "ca_cert_path": ssl_config.get("ca_cert_path"),
    }


def executor_kwargs(logger, test_type, test_environment, run_info_data, **kwargs):
    rv = base_executor_kwargs(test_type, test_environment, run_info_data, **kwargs)
    rv["capabilities"] = {}
    return rv


def env_extras(**kwargs):
    return []


def env_options():
    return {"server_host": "127.0.0.1", "bind_address": True}


class TinyBrowser(WebDriverBrowser):
    def __init__(
        self,
        logger,
        binary,
        webdriver_host="127.0.0.1",
        binary_args=None,
        ca_cert_path=None,
        **kwargs,
    ):
        args = list(binary_args or [])
        self._profile_root = None
        self._ca_cert_path = ca_cert_path
        super().__init__(
            logger,
            binary=binary,
            webdriver_binary=binary,
            webdriver_args=args,
            host=webdriver_host,
            supports_pac=False,
            **kwargs,
        )

    def make_command(self):
        self._ensure_profile()
        command = [
            self.webdriver_binary,
            f"--port={self.port}",
            "--resolve=nonexistent.*.test=fail",
            "--resolve=*.test=127.0.0.1",
            "--resolve=*.test.=127.0.0.1",
        ]
        if self._ca_cert_path:
            command.append(f"--tls-ca={self._ca_cert_path}")
        return command + self.webdriver_args

    def stop(self, force=False):
        try:
            return super().stop(force=force)
        finally:
            self._remove_profile()

    def cleanup(self):
        try:
            super().cleanup()
        finally:
            self._remove_profile()

    def _ensure_profile(self):
        if self._profile_root is not None:
            return
        self._profile_root = tempfile.mkdtemp(prefix="tinybrowser-wpt-")
        runtime = os.path.join(self._profile_root, "run")
        data = os.path.join(self._profile_root, "data")
        os.makedirs(runtime)
        os.makedirs(data)
        self.env["XDG_RUNTIME_DIR"] = runtime
        self.env["XDG_DATA_HOME"] = data

    def _remove_profile(self):
        """Drops the throwaway profile.

        The run's log is wherever `--log-file` put it, which the harness picks,
        so nothing here has to survive teardown.
        """
        root = self._profile_root
        self._profile_root = None
        if root is not None:
            shutil.rmtree(root, ignore_errors=True)


class TinyBrowserProtocol(WebDriverProtocol):
    enable_bidi = False


class TinyBrowserTestharnessExecutor(WebDriverTestharnessExecutor):
    supports_testdriver = True
    protocol_cls = TinyBrowserProtocol
    extra_timeout = EXTRA_TIMEOUT


class TinyBrowserCrashtestExecutor(WebDriverCrashtestExecutor):
    """Crashtests only need the page to load and settle without dying."""

    protocol_cls = TinyBrowserProtocol
    extra_timeout = EXTRA_TIMEOUT


class TinyBrowserRefTestExecutor(WebDriverRefTestExecutor):
    """Reftests compare the screenshot against the test's reference image.

    The engine has no window chrome, so the outer window equals the inner
    viewport; the virtual window rectangle is accepted but the render size
    stays the shared 800x600 viewport.
    """

    protocol_cls = TinyBrowserProtocol
    extra_timeout = EXTRA_TIMEOUT
