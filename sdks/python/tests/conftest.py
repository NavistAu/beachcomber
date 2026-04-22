"""Shared pytest fixtures including a mock beachcomber daemon server."""

from __future__ import annotations

import json
import os
import socket
import tempfile
import threading
from typing import Any, Callable, Dict, List, Optional

import pytest


class MockDaemon:
    """In-process Unix socket server that mimics the beachcomber daemon.

    Accepts newline-delimited JSON requests and dispatches them to
    per-op handlers registered via :meth:`on`.  A default handler for
    each op can be set, or individual responses can be queued.

    The server runs in a background thread and is stopped when
    :meth:`stop` is called (or when used as a context manager).

    Attributes:
        socket_path: Path to the Unix domain socket.
        received: List of all parsed request dicts received so far.
    """

    def __init__(self) -> None:
        self._tmpdir = tempfile.mkdtemp()
        self.socket_path = os.path.join(self._tmpdir, "test.sock")
        self.received: List[Dict[str, Any]] = []
        self._handlers: Dict[str, Callable[[Dict[str, Any]], Dict[str, Any]]] = {}
        self._default_handler: Callable[[Dict[str, Any]], Dict[str, Any]] = (
            lambda req: {"ok": False, "error": f"no handler for op '{req.get('op')}'"}
        )
        self._server_sock: Optional[socket.socket] = None
        self._thread: Optional[threading.Thread] = None
        self._stop_event = threading.Event()

    # --- Handler registration ---

    def on(
        self, op: str, handler: Callable[[Dict[str, Any]], Dict[str, Any]]
    ) -> "MockDaemon":
        """Register a response handler for a given op.

        Args:
            op: Operation name (``"get"``, ``"refresh"``, etc.).
            handler: Callable that receives the parsed request dict and
                returns a response dict.

        Returns:
            ``self`` for chaining.
        """
        self._handlers[op] = handler
        return self

    def respond(self, op: str, response: Dict[str, Any]) -> "MockDaemon":
        """Register a static response for a given op.

        Args:
            op: Operation name.
            response: Response dict to always return for this op.

        Returns:
            ``self`` for chaining.
        """
        self._handlers[op] = lambda _req: response
        return self

    # --- Lifecycle ---

    def start(self) -> "MockDaemon":
        """Bind the socket and start the server thread."""
        self._server_sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self._server_sock.bind(self.socket_path)
        self._server_sock.listen(5)
        self._server_sock.settimeout(0.1)
        self._thread = threading.Thread(target=self._serve, daemon=True)
        self._thread.start()
        return self

    def stop(self) -> None:
        """Signal the server thread to stop and wait for it."""
        self._stop_event.set()
        if self._server_sock:
            try:
                self._server_sock.close()
            except OSError:
                pass
        if self._thread:
            self._thread.join(timeout=2.0)

    def __enter__(self) -> "MockDaemon":
        return self.start()

    def __exit__(self, *_: Any) -> None:
        self.stop()

    # --- Internal server loop ---

    def _serve(self) -> None:
        assert self._server_sock is not None
        while not self._stop_event.is_set():
            try:
                conn, _ = self._server_sock.accept()
            except OSError:
                break
            t = threading.Thread(target=self._handle_conn, args=(conn,), daemon=True)
            t.start()

    def _handle_conn(self, conn: socket.socket) -> None:
        reader = conn.makefile("r", encoding="utf-8")
        writer = conn.makefile("w", encoding="utf-8")
        try:
            for line in reader:
                line = line.strip()
                if not line:
                    continue
                try:
                    req = json.loads(line)
                except json.JSONDecodeError:
                    resp = {"ok": False, "error": "invalid JSON"}
                    writer.write(json.dumps(resp) + "\n")
                    writer.flush()
                    continue

                self.received.append(req)
                op = req.get("op", "")
                handler = self._handlers.get(op, self._default_handler)
                try:
                    resp = handler(req)
                except Exception as exc:  # pragma: no cover
                    resp = {"ok": False, "error": str(exc)}

                writer.write(json.dumps(resp) + "\n")
                writer.flush()
        except OSError:
            pass
        finally:
            try:
                reader.close()
            except OSError:
                pass
            try:
                writer.close()
            except OSError:
                pass
            conn.close()


@pytest.fixture()
def mock_daemon() -> Any:
    """Yield a started :class:`MockDaemon` and stop it after the test."""
    with MockDaemon() as daemon:
        yield daemon


@pytest.fixture()
def git_daemon(mock_daemon: MockDaemon) -> MockDaemon:
    """MockDaemon pre-configured with a git provider."""
    mock_daemon.respond(
        "get",
        {
            "ok": True,
            "data": {"branch": "main", "dirty": False},
            "age_ms": 42,
            "stale": False,
        },
    )
    mock_daemon.respond("refresh", {"ok": True})
    mock_daemon.respond("context", {"ok": True})
    mock_daemon.respond(
        "list",
        {
            "ok": True,
            "data": [
                {"name": "git", "global": False, "fields": ["branch", "dirty"]},
                {"name": "hostname", "global": True, "fields": ["short", "full"]},
            ],
        },
    )
    mock_daemon.respond(
        "status",
        {"ok": True, "data": {"cache_entries": 3, "scheduler": "running"}},
    )
    return mock_daemon
