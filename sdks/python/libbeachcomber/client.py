"""Client and Session classes for the beachcomber daemon.

Typical usage::

    from beachcomber import Client

    client = Client()
    result = client.get("git.branch", path="/path/to/repo")
    if result.is_hit:
        print(result.data)

For multiple queries on one connection use a session::

    with client.session() as session:
        session.set_context("/path/to/repo")
        branch = session.get("git.branch")
        status = session.get("git")
"""

from __future__ import annotations

import io
import socket
from contextlib import contextmanager
from typing import Any, Generator, List, Optional

from .discovery import discover_socket_path
from .exceptions import DaemonNotRunning, ProtocolError
from .protocol import (
    build_context_request,
    build_get_request,
    build_list_request,
    build_refresh_request,
    build_status_request,
    decode_response,
)
from .result import CombResult

# Default socket timeout in seconds (matches Rust client: 100 ms).
_DEFAULT_TIMEOUT: float = 0.1


def _connect(socket_path: str, timeout: float) -> socket.socket:
    """Open a Unix domain socket connection to the daemon.

    Args:
        socket_path: Absolute path to the Unix domain socket.
        timeout: Read/write timeout in seconds.

    Returns:
        Connected :class:`socket.socket`.

    Raises:
        DaemonNotRunning: If the connection is refused or the socket does
            not exist.
    """
    try:
        sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        sock.settimeout(timeout)
        sock.connect(socket_path)
        return sock
    except (ConnectionRefusedError, FileNotFoundError, OSError) as exc:
        raise DaemonNotRunning(socket_path) from exc


def _send_recv(sock: socket.socket, request: bytes) -> dict[str, Any]:
    """Send a request and read one response line.

    Args:
        sock: Connected socket.
        request: Encoded request bytes (must include trailing newline).

    Returns:
        Parsed response dict (``ok`` has already been verified to be
        ``True``).

    Raises:
        ProtocolError: On I/O or parse failure.
        ServerError: If the daemon returns ``ok: false``.
    """
    try:
        sock.sendall(request)
    except OSError as exc:
        raise ProtocolError(f"failed to send request: {exc}") from exc

    # Read until newline using a file-like wrapper.
    reader = sock.makefile("r", encoding="utf-8")
    try:
        line = reader.readline()
    except OSError as exc:
        raise ProtocolError(f"failed to read response: {exc}") from exc
    finally:
        reader.detach()  # Do not close the underlying socket.

    return decode_response(line)


def _result_from_response(resp: dict[str, Any]) -> CombResult:
    """Build a :class:`CombResult` from a parsed ``get`` response dict."""
    data = resp.get("data")
    age_ms = int(resp.get("age_ms", 0) or 0)
    stale = bool(resp.get("stale", False))
    return CombResult(ok=True, data=data, age_ms=age_ms, stale=stale)


class Client:
    """One-shot client for the beachcomber daemon.

    Each method opens a new socket connection, sends one request, reads
    the response, then closes the connection. This is simple and safe
    for occasional queries.

    For repeated queries (e.g. populating a shell prompt) prefer
    :meth:`session` which reuses the connection.

    Args:
        socket_path: Explicit path to the daemon socket. If ``None`` the
            path is auto-discovered via
            :func:`~beachcomber.discovery.discover_socket_path`.
        timeout: Socket read/write timeout in seconds. Default ``0.1``.
    """

    def __init__(
        self,
        socket_path: Optional[str] = None,
        timeout: float = _DEFAULT_TIMEOUT,
    ) -> None:
        self._socket_path = socket_path
        self._timeout = timeout

    def _resolve_path(self) -> str:
        return self._socket_path or discover_socket_path()

    def get(self, key: str, path: Optional[str] = None) -> CombResult:
        """Read a cached value from the daemon.

        Args:
            key: Provider key. Use ``"provider.field"`` for a single
                scalar (e.g. ``"git.branch"``) or ``"provider"`` for the
                full provider object (e.g. ``"git"``).
            path: Working-directory path for per-directory providers.
                Global providers (e.g. ``"hostname"``) ignore this.

        Returns:
            :class:`~beachcomber.result.CombResult` with ``is_hit``
            ``True`` when a cached value exists.

        Raises:
            DaemonNotRunning: If the socket cannot be reached.
            ServerError: If the daemon returns an error response.
            ProtocolError: On I/O or JSON parse failure.
        """
        sock = _connect(self._resolve_path(), self._timeout)
        try:
            resp = _send_recv(sock, build_get_request(key, path))
        finally:
            sock.close()
        return _result_from_response(resp)

    def refresh(self, key: str, path: Optional[str] = None) -> None:
        """Trigger recomputation of a provider.

        The daemon will recompute the value in the background. This is
        fire-and-forget — the method returns once the daemon acknowledges
        the refresh.

        Args:
            key: Provider key to recompute.
            path: Working-directory path for per-directory providers.

        Raises:
            DaemonNotRunning: If the socket cannot be reached.
            ServerError: If the daemon returns an error response.
            ProtocolError: On I/O or JSON parse failure.
        """
        sock = _connect(self._resolve_path(), self._timeout)
        try:
            _send_recv(sock, build_refresh_request(key, path))
        finally:
            sock.close()

    def list(self) -> List[dict[str, Any]]:
        """Return available providers from the daemon.

        Returns:
            List of provider dicts. Each dict has at least ``"name"``,
            ``"global"``, and ``"fields"`` keys.

        Raises:
            DaemonNotRunning: If the socket cannot be reached.
            ServerError: If the daemon returns an error response.
            ProtocolError: On I/O or JSON parse failure.
        """
        sock = _connect(self._resolve_path(), self._timeout)
        try:
            resp = _send_recv(sock, build_list_request())
        finally:
            sock.close()
        return resp.get("data", [])

    def status(self) -> dict[str, Any]:
        """Return daemon scheduler and cache status.

        Returns:
            Status dict as returned by the daemon.

        Raises:
            DaemonNotRunning: If the socket cannot be reached.
            ServerError: If the daemon returns an error response.
            ProtocolError: On I/O or JSON parse failure.
        """
        sock = _connect(self._resolve_path(), self._timeout)
        try:
            resp = _send_recv(sock, build_status_request())
        finally:
            sock.close()
        return resp.get("data", {})

    @contextmanager
    def session(self) -> Generator[Session, None, None]:
        """Open a persistent connection as a context manager.

        Yields a :class:`Session` that reuses a single socket for all
        operations within the ``with`` block.

        Example::

            with client.session() as session:
                session.set_context("/my/repo")
                result = session.get("git.branch")

        Raises:
            DaemonNotRunning: If the socket cannot be reached.
        """
        sock = _connect(self._resolve_path(), self._timeout)
        session = Session(sock)
        try:
            yield session
        finally:
            sock.close()


class Session:
    """Persistent connection to the beachcomber daemon.

    Reuses a single Unix domain socket across multiple operations.
    Create via :meth:`Client.session` rather than directly.

    Args:
        sock: Already-connected :class:`socket.socket`.
    """

    def __init__(self, sock: socket.socket) -> None:
        self._sock = sock

    def set_context(self, path: str) -> None:
        """Set the default working-directory path for this connection.

        After calling this, :meth:`get` and :meth:`refresh` calls do not
        need an explicit ``path`` argument.

        Args:
            path: Absolute path to set as the session context.

        Raises:
            ServerError: If the daemon returns an error response.
            ProtocolError: On I/O or JSON parse failure.
        """
        _send_recv(self._sock, build_context_request(path))

    def get(self, key: str, path: Optional[str] = None) -> CombResult:
        """Read a cached value using the persistent connection.

        Args:
            key: Provider key (``"git.branch"``, ``"git"``, etc.).
            path: Optional path override. If omitted and
                :meth:`set_context` has been called, the session context
                is used by the daemon.

        Returns:
            :class:`~beachcomber.result.CombResult`.

        Raises:
            ServerError: If the daemon returns an error response.
            ProtocolError: On I/O or JSON parse failure.
        """
        resp = _send_recv(self._sock, build_get_request(key, path))
        return _result_from_response(resp)

    def refresh(self, key: str, path: Optional[str] = None) -> None:
        """Trigger recomputation via the persistent connection.

        Args:
            key: Provider key to recompute.
            path: Optional path override.

        Raises:
            ServerError: If the daemon returns an error response.
            ProtocolError: On I/O or JSON parse failure.
        """
        _send_recv(self._sock, build_refresh_request(key, path))

    def list(self) -> List[dict[str, Any]]:
        """Return available providers via the persistent connection.

        Returns:
            List of provider dicts.
        """
        resp = _send_recv(self._sock, build_list_request())
        return resp.get("data", [])

    def status(self) -> dict[str, Any]:
        """Return daemon status via the persistent connection.

        Returns:
            Status dict as returned by the daemon.
        """
        resp = _send_recv(self._sock, build_status_request())
        return resp.get("data", {})
