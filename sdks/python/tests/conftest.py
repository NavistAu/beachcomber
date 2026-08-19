"""Shared pytest fixtures: a real ``comb`` daemon for integration tests.

The Python SDK is now a ``ctypes`` binding over the native
``libbeachcomber`` C ABI (see ``libbeachcomber/_native.py``) rather than a
hand-rolled NDJSON socket client, so there is no longer a meaningful
wire-level mock to test against: the wire protocol is entirely the native
library's concern. Integration tests here drive the real binding against a
real daemon, the same way ``conformance_runner.py`` does.

Requires ``COMB_BIN`` (or ``comb`` on ``$PATH``) and ``BEACHCOMBER_LIB`` (or
a discoverable ``libbeachcomber.{so,dylib}``); tests needing the daemon are
skipped if a binary can't be found, and the whole session errors loudly if
the daemon fails to start once a binary *is* found.
"""

from __future__ import annotations

import os
import pathlib
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time
from typing import Optional

import pytest

# Hard-kill the entire test process if the suite takes longer than 30 seconds.
# This mirrors the nextest `global-timeout` for the Rust suite.
_SUITE_TIMEOUT_S = 30
_watchdog = threading.Timer(
    _SUITE_TIMEOUT_S,
    lambda: (
        sys.stderr.write(f"\n[TIMEOUT] Test suite exceeded {_SUITE_TIMEOUT_S}s — aborting.\n"),
        os._exit(1),
    ),
)
_watchdog.daemon = True
_watchdog.start()

_SDK_DIR = pathlib.Path(__file__).parent.parent
_REPO_ROOT = _SDK_DIR.parent.parent

# Make a locally built dylib discoverable without requiring the caller to
# set BEACHCOMBER_LIB themselves — but never override an explicit setting.
if "BEACHCOMBER_LIB" not in os.environ:
    if sys.platform == "darwin":
        _default_lib = _REPO_ROOT / "target" / "debug" / "libbeachcomber.dylib"
    else:
        _default_lib = _REPO_ROOT / "target" / "debug" / "libbeachcomber.so"
    if _default_lib.is_file():
        os.environ["BEACHCOMBER_LIB"] = str(_default_lib)


def _find_comb_bin() -> Optional[str]:
    env = os.environ.get("COMB_BIN")
    if env and os.path.isfile(env) and os.access(env, os.X_OK):
        return env
    default = _REPO_ROOT / "target" / "debug" / "comb"
    if default.is_file() and os.access(default, os.X_OK):
        return str(default)
    return shutil.which("comb")


def _wait_for_socket(sock_path: str, timeout: float = 8.0) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if os.path.exists(sock_path):
            try:
                s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                s.settimeout(0.1)
                s.connect(sock_path)
                s.close()
                return True
            except OSError:
                pass
        time.sleep(0.05)
    return False


class DaemonProcess:
    """Manages a ``comb daemon`` subprocess for the test session."""

    def __init__(self, comb_bin: str) -> None:
        self._comb_bin = comb_bin
        self._tmpdir = tempfile.mkdtemp(prefix="bcpytest_")
        self.socket_path = os.path.join(self._tmpdir, "comb.sock")
        self._proc: Optional[subprocess.Popen] = None

    def start(self) -> None:
        env = os.environ.copy()
        env["XDG_CONFIG_HOME"] = self._tmpdir
        log_path = os.path.join(self._tmpdir, "daemon.log")
        with open(log_path, "wb") as log:
            self._proc = subprocess.Popen(
                [self._comb_bin, "daemon", "--socket", self.socket_path],
                stdout=log,
                stderr=log,
                env=env,
            )
        if not _wait_for_socket(self.socket_path):
            self.stop()
            tail = ""
            try:
                tail = pathlib.Path(log_path).read_text()[-2000:]
            except OSError:
                pass
            raise RuntimeError(f"daemon did not start within 8s\nLog tail:\n{tail}")

    def stop(self) -> None:
        if self._proc is not None:
            try:
                self._proc.send_signal(signal.SIGTERM)
                self._proc.wait(timeout=3.0)
            except (ProcessLookupError, subprocess.TimeoutExpired):
                try:
                    self._proc.kill()
                except ProcessLookupError:
                    pass
            self._proc = None
        shutil.rmtree(self._tmpdir, ignore_errors=True)


@pytest.fixture(scope="session")
def comb_bin() -> str:
    bin_path = _find_comb_bin()
    if not bin_path:
        pytest.skip("COMB_BIN not set and 'comb' not found on $PATH / target/debug")
    return bin_path


@pytest.fixture(scope="session")
def daemon(comb_bin: str):
    proc = DaemonProcess(comb_bin)
    proc.start()
    yield proc
    proc.stop()


@pytest.fixture()
def client(daemon: DaemonProcess):
    from libbeachcomber import Client

    c = Client(socket_path=daemon.socket_path, timeout=5.0)
    yield c
    c.close()
