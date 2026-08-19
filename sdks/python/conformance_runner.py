#!/usr/bin/env python3
"""Protocol conformance runner for the beachcomber Python SDK.

Loads fixtures from tests/conformance/ relative to the repo root, spawns
a fresh daemon for each fixture group, drives the ops through the Python
client, and asserts the expected outcomes.

Usage::

    COMB_BIN=/path/to/comb python sdks/python/conformance_runner.py

The runner exits with code 0 if all fixtures pass and code 1 if any fail
or if COMB_BIN is unset / the binary cannot be found.
"""

from __future__ import annotations

import json
import os
import pathlib
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
from typing import Any, Optional

# ---------------------------------------------------------------------------
# Path setup — make the SDK importable when run as a script.
# ---------------------------------------------------------------------------

_SDK_DIR = pathlib.Path(__file__).parent
sys.path.insert(0, str(_SDK_DIR))

from libbeachcomber.client import Client  # noqa: E402
from libbeachcomber.exceptions import CombError, ServerError  # noqa: E402
from libbeachcomber.types import IntrospectSubject  # noqa: E402

# ---------------------------------------------------------------------------
# Fixture discovery
# ---------------------------------------------------------------------------

_REPO_ROOT = _SDK_DIR.parent.parent
_CONFORMANCE_DIR = _REPO_ROOT / "tests" / "conformance"


def discover_fixtures() -> list[pathlib.Path]:
    """Return all *.json fixture files sorted by path."""
    if not _CONFORMANCE_DIR.exists():
        print(f"ERROR: conformance directory not found: {_CONFORMANCE_DIR}", file=sys.stderr)
        sys.exit(1)
    fixtures = sorted(_CONFORMANCE_DIR.rglob("*.json"))
    return fixtures


# ---------------------------------------------------------------------------
# Daemon lifecycle
# ---------------------------------------------------------------------------


def _wait_for_socket(sock_path: str, timeout: float = 5.0) -> bool:
    """Poll until the Unix socket exists and accepts connections."""
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
    """Manages a comb daemon subprocess for one conformance run."""

    def __init__(self, comb_bin: str) -> None:
        self._comb_bin = comb_bin
        self._tmpdir = tempfile.mkdtemp(prefix="bcconf_")
        self.socket_path = os.path.join(self._tmpdir, "comb.sock")
        self._config_path = os.path.join(self._tmpdir, "config.toml")
        self._proc: Optional[subprocess.Popen[bytes]] = None
        self._log_path = os.path.join(self._tmpdir, "daemon.log")

        # Write a minimal config pointing at our temp socket.
        with open(self._config_path, "w") as f:
            f.write(f'[daemon]\nsocket_path = "{self.socket_path}"\n')

    def start(self) -> None:
        """Start the daemon process."""
        env = os.environ.copy()
        env["XDG_CONFIG_HOME"] = self._tmpdir  # isolate from user config

        with open(self._log_path, "wb") as log:
            self._proc = subprocess.Popen(
                [self._comb_bin, "daemon", "--socket", self.socket_path],
                stdout=log,
                stderr=log,
                env=env,
            )

        if not _wait_for_socket(self.socket_path, timeout=8.0):
            self.stop()
            log_tail = ""
            try:
                with open(self._log_path) as f:
                    log_tail = f.read()[-2000:]
            except OSError:
                pass
            raise RuntimeError(
                f"daemon did not start within 8s (socket: {self.socket_path})\n"
                f"Log tail:\n{log_tail}"
            )

    def stop(self) -> None:
        """Terminate the daemon process and clean up."""
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

    def make_client(self) -> Client:
        """Return a Client connected to this daemon's socket."""
        return Client(socket_path=self.socket_path, timeout=5.0)


# ---------------------------------------------------------------------------
# Op dispatch helpers
# ---------------------------------------------------------------------------


def _run_op(
    client: Client,
    op: str,
    args: dict[str, Any],
    resolve_ctx: Optional[dict[str, Any]] = None,
) -> Any:
    """Run a single op against the client.

    Returns the raw response data (or None for ops that return nothing),
    or raises CombError / ServerError on failure.

    `resolve_ctx` (only consulted for the `resolve` op) carries the
    fixture's `virtual`/`env`/`cwd` blocks — see
    tests/conformance/README.md.
    """
    if op == "resolve":
        assert resolve_ctx is not None
        key = args.get("key", "")
        return client.resolve(
            key,
            cwd=resolve_ctx["cwd"],
            env=resolve_ctx["env"] or None,
            overrides=resolve_ctx["virtual"] or None,
        )
    if op == "get":
        return client.get(**args)
    if op == "refresh":
        client.refresh(**args)
        return None
    if op == "put":
        client.put(**args)
        return None
    if op == "status":
        return client.status()
    if op == "hello":
        return client.hello()
    if op == "context":
        # Context is session-level; run through a session to avoid issues.
        with client.session() as session:
            session.set_context(args.get("path", ""))
        return None
    if op == "watch":
        key = args.get("key", "")
        path = args.get("path")
        with client.watch(key, path=path) as stream:
            event = stream.next_event()
        return event
    if op == "introspect":
        subject_str = args.get("subject", "daemon")
        try:
            subject = IntrospectSubject(subject_str)
        except ValueError:
            raise ValueError(f"unknown introspect subject: {subject_str!r}")
        duration_secs = args.get("duration_secs")
        return client.introspect(subject, duration_secs=duration_secs)
    raise ValueError(f"unknown op: {op!r}")


# Ops this runner's binding can execute. A fixture using any op outside this
# set must be skipped, not failed — the binding doesn't implement it yet.
_SUPPORTED_OPS = {
    "hello",
    "get",
    "refresh",
    "put",
    "status",
    "context",
    "watch",
    "introspect",
    "resolve",
}


def _unsupported_op(fixture: dict[str, Any]) -> Optional[str]:
    """Return the first op in the fixture that this runner can't execute, or None."""
    ops = [s.get("op") for s in fixture.get("setup", [])]
    ops.append(fixture["test"]["op"])
    for op in ops:
        if op not in _SUPPORTED_OPS:
            return op
    return None


# ---------------------------------------------------------------------------
# Assertion helpers
# ---------------------------------------------------------------------------


def _check_expect(
    expect: dict[str, Any],
    result: Any,
    error: Optional[str],
    fixture_name: str,
) -> list[str]:
    """Check the expectation dict against the result/error.

    Returns a list of failure messages (empty on success).
    """
    failures: list[str] = []
    status = expect.get("status", "ok")

    if status == "ok" or status == "hit":
        if error is not None:
            failures.append(f"expected ok/hit but got error: {error!r}")
            return failures

        # For get results (CombResult).
        from libbeachcomber.result import CombResult
        from libbeachcomber.types import HelloInfo, IntrospectResponse, WatchEvent

        if status == "hit":
            if isinstance(result, CombResult):
                if not result.is_hit:
                    failures.append("expected cache hit but got miss (data is None)")
            elif isinstance(result, WatchEvent):
                if result.data is None:
                    failures.append("expected watch hit but data is None")
            else:
                failures.append(f"expected CombResult or WatchEvent for hit, got {type(result).__name__}")

        # data_type check.
        data_type = expect.get("data_type")
        if data_type is not None:
            actual_data = _extract_data(result)
            if not _check_data_type(actual_data, data_type):
                failures.append(
                    f"expected data_type={data_type!r} but data was {actual_data!r} "
                    f"(type {type(actual_data).__name__})"
                )

        # data_equals check.
        if "data_equals" in expect:
            expected_val = expect["data_equals"]
            actual_data = _extract_data(result)
            if actual_data != expected_val:
                failures.append(
                    f"expected data == {expected_val!r} but got {actual_data!r}"
                )

        # data_as_text check.
        if "data_as_text" in expect:
            expected_text = str(expect["data_as_text"])
            actual_data = _extract_data(result)
            if str(actual_data) != expected_text:
                failures.append(
                    f"expected data as text {expected_text!r} but got {str(actual_data)!r}"
                )

        # data_contains_field check.
        if "data_contains_field" in expect:
            field = expect["data_contains_field"]
            actual_data = _extract_data(result)
            if not isinstance(actual_data, dict) or field not in actual_data:
                failures.append(
                    f"expected data to contain field {field!r} but data was {actual_data!r}"
                )

        # age_ms_present check.
        if expect.get("age_ms_present"):
            age = _extract_age_ms(result)
            if age is None:
                failures.append("expected age_ms to be present but it was None/missing")

    elif status == "error":
        if error is None:
            failures.append(f"expected an error but op succeeded with result: {result!r}")
            return failures
        error_contains = expect.get("error_contains")
        if error_contains and error_contains not in error:
            failures.append(
                f"expected error to contain {error_contains!r} but got: {error!r}"
            )

    elif status == "miss":
        if error is not None:
            failures.append(f"expected miss but got error: {error!r}")
        else:
            from libbeachcomber.result import CombResult
            if isinstance(result, CombResult) and result.is_hit:
                failures.append(f"expected cache miss but got hit with data: {result.data!r}")

    else:
        failures.append(f"unknown expect status: {status!r}")

    return failures


def _extract_data(result: Any) -> Any:
    """Pull the data value out of typed result objects."""
    from libbeachcomber.result import CombResult
    from libbeachcomber.types import HelloInfo, IntrospectResponse, WatchEvent

    if isinstance(result, CombResult):
        return result.data
    if isinstance(result, WatchEvent):
        return result.data
    if isinstance(result, HelloInfo):
        return {"protocol_version": result.protocol_version, "daemon_version": result.daemon_version}
    if isinstance(result, IntrospectResponse):
        if result.daemon is not None:
            import dataclasses
            return dataclasses.asdict(result.daemon)
        return result.other
    return result


def _extract_age_ms(result: Any) -> Optional[int]:
    from libbeachcomber.result import CombResult
    from libbeachcomber.types import WatchEvent

    if isinstance(result, CombResult):
        return result.age_ms
    if isinstance(result, WatchEvent):
        return result.age_ms
    return None


def _check_data_type(data: Any, expected_type: str) -> bool:
    if expected_type == "object":
        return isinstance(data, dict)
    if expected_type == "array":
        return isinstance(data, list)
    if expected_type == "string":
        return isinstance(data, str)
    if expected_type == "number":
        return isinstance(data, (int, float)) and not isinstance(data, bool)
    if expected_type == "boolean":
        return isinstance(data, bool)
    if expected_type == "null":
        return data is None
    return False


# ---------------------------------------------------------------------------
# Run a single fixture
# ---------------------------------------------------------------------------


def run_fixture(fixture_path: pathlib.Path, client: Client) -> tuple[str, str]:
    """Run a single fixture against the given client.

    Returns (status, message) where status is "pass", "fail", or "skip".
    """
    with open(fixture_path) as f:
        fixture = json.load(f)

    name = fixture.get("name", fixture_path.stem)

    unsupported = _unsupported_op(fixture)
    if unsupported is not None:
        return "skip", f"unsupported op {unsupported!r}"

    with tempfile.TemporaryDirectory(prefix="bcconf_cwd_") as default_cwd:
        resolve_ctx = {
            "virtual": fixture.get("virtual", {}),
            "env": fixture.get("env", {}),
            "cwd": fixture.get("cwd", default_cwd),
        }

        # Run setup ops (ignore their results; failures are fatal for the fixture).
        for setup_op in fixture.get("setup", []):
            op = setup_op["op"]
            args = setup_op.get("args", {})
            try:
                _run_op(client, op, args, resolve_ctx)
            except Exception as exc:
                return "fail", f"setup op {op!r} failed: {exc}"

        # Run the test op.
        test = fixture["test"]
        op = test["op"]
        args = test.get("args", {})
        expect = fixture.get("expect", {})

        result = None
        error: Optional[str] = None

        try:
            result = _run_op(client, op, args, resolve_ctx)
        except ServerError as exc:
            error = exc.message
        except CombError as exc:
            error = str(exc)
        except Exception as exc:
            return "fail", f"unexpected exception running {op!r}: {type(exc).__name__}: {exc}"

        failures = _check_expect(expect, result, error, name)
        if failures:
            msg = "; ".join(failures)
            return "fail", f"{name}: FAIL — {msg}"

        return "pass", f"{name}: ok"


# ---------------------------------------------------------------------------
# Main entry point
# ---------------------------------------------------------------------------


def main() -> int:
    comb_bin = os.environ.get("COMB_BIN", shutil.which("comb") or "")
    if not comb_bin:
        print(
            "ERROR: COMB_BIN environment variable is not set and 'comb' is not in PATH.\n"
            "Set COMB_BIN=/path/to/comb to run the conformance suite.",
            file=sys.stderr,
        )
        return 1

    if not os.path.isfile(comb_bin) or not os.access(comb_bin, os.X_OK):
        print(f"ERROR: COMB_BIN={comb_bin!r} is not an executable file.", file=sys.stderr)
        return 1

    fixtures = discover_fixtures()
    if not fixtures:
        print("WARNING: No conformance fixtures found.", file=sys.stderr)
        return 0

    print(f"Found {len(fixtures)} fixture(s) in {_CONFORMANCE_DIR}")
    print(f"Using daemon: {comb_bin}\n")

    passed = 0
    failed = 0
    skipped = 0
    errors: list[str] = []

    # A fresh daemon per fixture, per tests/conformance/README.md's
    # isolation guarantee ("Each fixture runs against a fresh daemon
    # instance") — a shared daemon would leak cache state (e.g. `put`s from
    # one fixture's `setup` visible to the next fixture's `resolve` cache
    # refs) across fixtures that happen to reuse a key.
    for fixture_path in fixtures:
        rel = fixture_path.relative_to(_CONFORMANCE_DIR)

        daemon = DaemonProcess(comb_bin)
        try:
            daemon.start()
        except RuntimeError as exc:
            print(f"ERROR: Failed to start daemon for {rel}: {exc}", file=sys.stderr)
            return 1

        try:
            client = daemon.make_client()
            status, msg = run_fixture(fixture_path, client)
        finally:
            daemon.stop()

        if status == "skip":
            print(f"  SKIP {rel}: {msg}")
            skipped += 1
            continue
        status_label = "PASS" if status == "pass" else "FAIL"
        print(f"  [{status_label}] {rel}: {msg.split(': ', 1)[-1]}")
        if status == "pass":
            passed += 1
        else:
            failed += 1
            errors.append(str(rel) + ": " + msg)

    print(f"\n{passed + failed + skipped} fixtures: {passed} passed, {failed} failed, {skipped} skipped")

    if errors:
        print("\nFailures:")
        for e in errors:
            print(f"  - {e}")
        return 1

    return 0


if __name__ == "__main__":
    sys.exit(main())
