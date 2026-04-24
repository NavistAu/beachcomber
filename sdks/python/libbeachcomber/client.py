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

import socket
import time
from contextlib import contextmanager
from typing import Any, Generator, Optional

from .discovery import discover_socket_path
from .exceptions import DaemonNotRunning, ProtocolError
from .protocol import (
    build_context_request,
    build_get_request,
    build_hello_request,
    build_introspect_request,
    build_put_request,
    build_refresh_request,
    build_status_request,
    build_watch_request,
    decode_response,
)
from .result import CombResult
from .types import (
    CacheRow,
    DaemonHealth,
    HelloInfo,
    IntrospectResponse,
    IntrospectSubject,
    Verdict,
    WatchEvent,
)

# Default socket timeout in seconds (matches Rust client: 100 ms).
_DEFAULT_TIMEOUT: float = 0.1


_RETRY_BACKOFFS: list[float] = [0.250, 0.500, 1.000]


def _connect_with_retry(socket_path: str) -> socket.socket:
    """Connect to a Unix socket with 3 retries (250ms/500ms/1s exponential).

    Retries on ConnectionRefusedError and FileNotFoundError only — other
    errors surface immediately.  Intended to cover the brief restart window
    when the old daemon has shut down and the new one hasn't bound yet.

    Returns:
        Connected (blocking, no timeout set) :class:`socket.socket`.

    Raises:
        ConnectionRefusedError or FileNotFoundError: After all retries
            are exhausted.
    """
    last_exc: Exception | None = None
    for backoff in _RETRY_BACKOFFS:
        try:
            s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            s.connect(socket_path)
            return s
        except (ConnectionRefusedError, FileNotFoundError) as e:
            last_exc = e
            time.sleep(backoff)
    # Final attempt after all backoffs.
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.connect(socket_path)  # raises if still failing
    return s


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
        sock = _connect_with_retry(socket_path)
        sock.settimeout(timeout)
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


def _parse_hello(resp: dict[str, Any]) -> HelloInfo:
    """Build a :class:`HelloInfo` from a parsed ``hello`` response dict."""
    data = resp.get("data", {})
    if not isinstance(data, dict):
        data = {}
    return HelloInfo(
        protocol_version=str(data.get("protocol_version", "")),
        daemon_version=str(data.get("daemon_version", "")),
    )


def _parse_cache_rows(resp: dict[str, Any]) -> list[CacheRow]:
    """Build a list of :class:`CacheRow` from a parsed ``status`` response dict."""
    arr = resp.get("data", [])
    if not isinstance(arr, list):
        raise ProtocolError("status data is not an array")
    out = []
    for row in arr:
        if not isinstance(row, dict):
            continue
        out.append(
            CacheRow(
                provider=str(row.get("provider", "")),
                field=row.get("field"),
                path=row.get("path"),
                value=row.get("value"),
                age_ms=int(row.get("age_ms", 0) or 0),
                stale=bool(row.get("stale", False)),
                kind=row.get("kind"),
                poll_interval_secs=row.get("poll_interval_secs"),
                keep_alive_polls=row.get("keep_alive_polls"),
                fsevents_reinstate=row.get("fsevents_reinstate"),
                failure=row.get("failure"),
            )
        )
    return out


def _parse_daemon_health(data: dict[str, Any]) -> DaemonHealth:
    """Build a :class:`DaemonHealth` from a daemon data dict."""
    verdicts = []
    for v in data.get("verdicts", []) or []:
        if isinstance(v, dict):
            verdicts.append(
                Verdict(
                    level=str(v.get("level", "")),
                    message=str(v.get("message", "")),
                )
            )
    return DaemonHealth(
        pid=int(data.get("pid", 0) or 0),
        version=str(data.get("version", "")),
        uptime_secs=int(data.get("uptime_secs", 0) or 0),
        socket_path=str(data.get("socket_path", "")),
        config_path=data.get("config_path"),
        requests_total=int(data.get("requests_total", 0) or 0),
        in_flight=int(data.get("in_flight", 0) or 0),
        active_watchers=int(data.get("active_watchers", 0) or 0),
        cache_entries=int(data.get("cache_entries", 0) or 0),
        verdicts=verdicts,
    )


def _parse_introspect(
    subject: IntrospectSubject, resp: dict[str, Any]
) -> IntrospectResponse:
    """Build an :class:`IntrospectResponse` from a parsed ``introspect`` response dict."""
    data = resp.get("data", {})
    if subject == IntrospectSubject.DAEMON and isinstance(data, dict):
        return IntrospectResponse(
            subject=subject, daemon=_parse_daemon_health(data), other=None
        )
    return IntrospectResponse(subject=subject, daemon=None, other=data)


class WatchStream:
    """Iterator over watch events. Yields :class:`~beachcomber.types.WatchEvent` instances.

    Create via :meth:`Client.watch` rather than directly. The underlying
    connection is held open for the lifetime of the stream; release it via
    :meth:`close` or by using the stream as a context manager.

    Args:
        sock: Already-connected :class:`socket.socket` with a ``watch``
            request already sent.
    """

    def __init__(self, sock: socket.socket) -> None:
        self._sock = sock
        self._reader = sock.makefile("r", encoding="utf-8")

    def __iter__(self) -> "WatchStream":
        return self

    def __next__(self) -> WatchEvent:
        event = self.next_event()
        if event is None:
            raise StopIteration
        return event

    def next_event(self) -> Optional[WatchEvent]:
        """Read the next event from the stream.

        Returns:
            :class:`~beachcomber.types.WatchEvent` or ``None`` if the
            connection was closed by the daemon.
        """
        # Ensure the read blocks even if a timeout was set on the socket.
        self._sock.settimeout(None)
        line = self._reader.readline()
        if not line:
            return None
        resp = decode_response(line)
        data = resp.get("data")
        return WatchEvent(
            data=data,
            age_ms=int(resp.get("age_ms", 0) or 0),
            stale=bool(resp.get("stale", False)),
        )

    def close(self) -> None:
        """Close the underlying connection."""
        self._reader.detach()
        self._sock.close()

    def __enter__(self) -> "WatchStream":
        return self

    def __exit__(self, *exc: Any) -> None:
        self.close()


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

    def status(self) -> list[CacheRow]:
        """Return cache rows from the daemon.

        Returns:
            List of :class:`~beachcomber.types.CacheRow` dataclasses.

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
        return _parse_cache_rows(resp)

    def hello(self) -> HelloInfo:
        """Ask the daemon for protocol and build versions.

        Returns:
            :class:`~beachcomber.types.HelloInfo` with protocol and daemon
            version strings.

        Raises:
            DaemonNotRunning: If the socket cannot be reached.
            ServerError: If the daemon returns an error response.
            ProtocolError: On I/O or JSON parse failure.
        """
        sock = _connect(self._resolve_path(), self._timeout)
        try:
            resp = _send_recv(sock, build_hello_request())
        finally:
            sock.close()
        return _parse_hello(resp)

    def put(
        self,
        key: str,
        data: Optional[Any] = None,
        ttl: Optional[str] = None,
        path: Optional[str] = None,
    ) -> None:
        """Store data into a virtual provider. ``data=None`` clears the entry.

        Args:
            key: Virtual provider key to set.
            data: Data payload. ``None`` clears the entry.
            ttl: Optional time-to-live string (e.g. ``"60s"``).
            path: Optional path for per-directory virtual entries.

        Raises:
            DaemonNotRunning: If the socket cannot be reached.
            ServerError: If the daemon returns an error response.
            ProtocolError: On I/O or JSON parse failure.
        """
        sock = _connect(self._resolve_path(), self._timeout)
        try:
            _send_recv(sock, build_put_request(key, data, ttl, path))
        finally:
            sock.close()

    def introspect(
        self,
        subject: IntrospectSubject,
        duration_secs: Optional[int] = None,
    ) -> IntrospectResponse:
        """Run a diagnostic introspect query.

        Args:
            subject: Which subsystem to inspect.
            duration_secs: Optional sampling duration for metrics-style subjects.

        Returns:
            :class:`~beachcomber.types.IntrospectResponse`. When
            ``subject`` is :attr:`~beachcomber.types.IntrospectSubject.DAEMON`
            the ``daemon`` field is a typed :class:`~beachcomber.types.DaemonHealth`;
            for all other subjects ``other`` holds the raw data.

        Raises:
            DaemonNotRunning: If the socket cannot be reached.
            ServerError: If the daemon returns an error response.
            ProtocolError: On I/O or JSON parse failure.
        """
        sock = _connect(self._resolve_path(), self._timeout)
        try:
            resp = _send_recv(
                sock, build_introspect_request(subject.value, duration_secs)
            )
        finally:
            sock.close()
        return _parse_introspect(subject, resp)

    def watch(self, key: str, path: Optional[str] = None) -> WatchStream:
        """Subscribe to a key and receive live updates.

        Returns a :class:`WatchStream` iterator that yields
        :class:`~beachcomber.types.WatchEvent` instances. The underlying
        connection is held open for the lifetime of the stream; release it
        via :meth:`WatchStream.close` or by using the stream as a context
        manager.

        Args:
            key: Provider key to watch.
            path: Optional working-directory path.

        Returns:
            :class:`WatchStream` iterator.

        Raises:
            DaemonNotRunning: If the socket cannot be reached.
        """
        sock = _connect(self._resolve_path(), self._timeout)
        sock.sendall(build_watch_request(key, path))
        return WatchStream(sock)

    def get_with_flags(
        self,
        key: str,
        path: Optional[str] = None,
        force: bool = False,
        wait: bool = False,
    ) -> CombResult:
        """Read a cached value with optional force/wait flags.

        Args:
            key: Provider key.
            path: Optional working-directory path.
            force: Bypass cache and force recomputation.
            wait: Block until a fresh value is available.

        Returns:
            :class:`~beachcomber.result.CombResult`.

        Raises:
            DaemonNotRunning: If the socket cannot be reached.
            ServerError: If the daemon returns an error response.
            ProtocolError: On I/O or JSON parse failure.
        """
        sock = _connect(self._resolve_path(), self._timeout)
        try:
            resp = _send_recv(
                sock, build_get_request(key, path, force=force, wait=wait)
            )
        finally:
            sock.close()
        return _result_from_response(resp)

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

    def status(self) -> list[CacheRow]:
        """Return cache rows via the persistent connection.

        Returns:
            List of :class:`~beachcomber.types.CacheRow` dataclasses.
        """
        resp = _send_recv(self._sock, build_status_request())
        return _parse_cache_rows(resp)

    def hello(self) -> HelloInfo:
        """Ask the daemon for protocol and build versions via the persistent connection.

        Returns:
            :class:`~beachcomber.types.HelloInfo`.

        Raises:
            ServerError: If the daemon returns an error response.
            ProtocolError: On I/O or JSON parse failure.
        """
        resp = _send_recv(self._sock, build_hello_request())
        return _parse_hello(resp)

    def put(
        self,
        key: str,
        data: Optional[Any] = None,
        ttl: Optional[str] = None,
        path: Optional[str] = None,
    ) -> None:
        """Store data into a virtual provider via the persistent connection.

        Args:
            key: Virtual provider key to set.
            data: Data payload. ``None`` clears the entry.
            ttl: Optional time-to-live string (e.g. ``"60s"``).
            path: Optional path for per-directory virtual entries.

        Raises:
            ServerError: If the daemon returns an error response.
            ProtocolError: On I/O or JSON parse failure.
        """
        _send_recv(self._sock, build_put_request(key, data, ttl, path))

    def introspect(
        self,
        subject: IntrospectSubject,
        duration_secs: Optional[int] = None,
    ) -> IntrospectResponse:
        """Run a diagnostic introspect query via the persistent connection.

        Args:
            subject: Which subsystem to inspect.
            duration_secs: Optional sampling duration for metrics-style subjects.

        Returns:
            :class:`~beachcomber.types.IntrospectResponse`.

        Raises:
            ServerError: If the daemon returns an error response.
            ProtocolError: On I/O or JSON parse failure.
        """
        resp = _send_recv(
            self._sock, build_introspect_request(subject.value, duration_secs)
        )
        return _parse_introspect(subject, resp)

    def get_with_flags(
        self,
        key: str,
        path: Optional[str] = None,
        force: bool = False,
        wait: bool = False,
    ) -> CombResult:
        """Read a cached value with optional force/wait flags via the persistent connection.

        Args:
            key: Provider key.
            path: Optional working-directory path.
            force: Bypass cache and force recomputation.
            wait: Block until a fresh value is available.

        Returns:
            :class:`~beachcomber.result.CombResult`.

        Raises:
            ServerError: If the daemon returns an error response.
            ProtocolError: On I/O or JSON parse failure.
        """
        resp = _send_recv(
            self._sock, build_get_request(key, path, force=force, wait=wait)
        )
        return _result_from_response(resp)
