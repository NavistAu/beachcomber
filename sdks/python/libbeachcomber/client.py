"""Client and Session classes for the beachcomber daemon.

Transport: ``ctypes`` calls into the ``libbeachcomber`` C ABI (see
:mod:`._native`). Socket connection, retry, auto-start and wire-protocol
framing are all the native library's job now — this module only builds
request arguments, decodes typed results, and manages native handle
lifetimes.

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

import json
from contextlib import contextmanager
from typing import Any, Generator, Optional

from . import _native
from .exceptions import CombError
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

# Default socket timeout in seconds (matches the native client's own default).
_DEFAULT_TIMEOUT: float = 0.1


def _opt_json(value: Optional[Any]) -> Optional[str]:
    """Serialise a nullable dict/value argument to JSON, or leave it None."""
    if value is None:
        return None
    return json.dumps(value)


def _result_from_get_payload(payload: Optional[dict]) -> CombResult:
    """Build a :class:`CombResult` from a decoded ``bc_get``/``bc_session_get`` payload."""
    if not isinstance(payload, dict):
        return CombResult(ok=True, data=None, age_ms=0, stale=False)
    return CombResult(
        ok=True,
        data=payload.get("data"),
        age_ms=int(payload.get("age_ms") or 0),
        stale=bool(payload.get("stale") or False),
    )


def _parse_hello(data: Optional[dict]) -> HelloInfo:
    if not isinstance(data, dict):
        data = {}
    return HelloInfo(
        protocol_version=str(data.get("protocol_version", "")),
        daemon_version=str(data.get("daemon_version", "")),
    )


def _parse_cache_rows(data: Any) -> list[CacheRow]:
    if not isinstance(data, list):
        return []
    out = []
    for row in data:
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
                source=row.get("source"),
            )
        )
    return out


def _parse_daemon_health(data: dict) -> DaemonHealth:
    verdicts = []
    for v in data.get("verdicts", []) or []:
        if isinstance(v, dict):
            verdicts.append(Verdict(level=str(v.get("level", "")), message=str(v.get("message", ""))))
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


def _parse_introspect(subject: IntrospectSubject, data: Any) -> IntrospectResponse:
    if subject == IntrospectSubject.DAEMON and isinstance(data, dict):
        return IntrospectResponse(subject=subject, daemon=_parse_daemon_health(data), other=None)
    return IntrospectResponse(subject=subject, daemon=None, other=data)


def _build_options_json(
    socket_path: Optional[str], timeout: float, autostart: Optional[bool]
) -> Optional[str]:
    options: dict = {}
    if socket_path is not None:
        options["socket_path"] = socket_path
    options["timeout_ms"] = int(round(timeout * 1000))
    if autostart is not None:
        options["autostart"] = autostart
    return json.dumps(options)


class WatchStream:
    """Iterator over watch events. Yields :class:`~beachcomber.types.WatchEvent` instances.

    Create via :meth:`Client.watch` rather than directly. Wraps a native
    ``BcWatch`` handle; release it via :meth:`close` or by using the stream
    as a context manager.
    """

    def __init__(self, ptr: int) -> None:
        self._ptr = None
        self._lib = _native.get_lib()
        self._ptr = ptr

    def __iter__(self) -> "WatchStream":
        return self

    def __next__(self) -> WatchEvent:
        event = self.next_event()
        if event is None:
            raise StopIteration
        return event

    def next_event(self, timeout_ms: int = -1) -> Optional[WatchEvent]:
        """Read the next event from the stream.

        Args:
            timeout_ms: ``-1`` blocks indefinitely (the default, matching
                this method's historical no-argument behaviour), ``0``
                polls without blocking, ``>0`` waits that long.

        Returns:
            :class:`~beachcomber.types.WatchEvent` on the ``event``
            outcome; ``None`` on ``timeout``, ``eof``, or ``cancelled``.
        """
        if self._ptr is None:
            return None
        env = self._lib.call_watch_next(self._ptr, timeout_ms)
        if env.get("outcome") != "event":
            return None
        data = env.get("data") or {}
        return WatchEvent(
            data=data.get("data"),
            age_ms=int(data.get("age_ms") or 0),
            stale=bool(data.get("stale") or False),
        )

    def cancel(self) -> None:
        """Unblock a pending or future :meth:`next_event` call from another thread."""
        if self._ptr is not None:
            self._lib.call_void("bc_watch_cancel", self._ptr)

    def close(self) -> None:
        """Free the underlying native watch handle."""
        if self._ptr is not None:
            self._lib.call_void("bc_watch_free", self._ptr)
            self._ptr = None

    def __enter__(self) -> "WatchStream":
        return self

    def __exit__(self, *exc: Any) -> None:
        self.close()

    def __del__(self) -> None:
        self.close()


class Client:
    """Client for the beachcomber daemon, backed by the native C ABI.

    Args:
        socket_path: Explicit path to the daemon socket. If ``None`` the
            native library auto-discovers it (``$BEACHCOMBER_SOCKET``, then
            its per-user default).
        timeout: Socket read/write timeout in seconds. Default ``0.1``.
        autostart: Whether to attempt starting the daemon if it isn't
            running. ``None`` leaves the native library's own default.
    """

    def __init__(
        self,
        socket_path: Optional[str] = None,
        timeout: float = _DEFAULT_TIMEOUT,
        autostart: Optional[bool] = None,
    ) -> None:
        self._ptr = None
        self._lib = _native.get_lib()
        options_json = _build_options_json(socket_path, timeout, autostart)
        ptr = self._lib.call_ptr("bc_client_new", options_json.encode())
        if not ptr:
            raise CombError("bc_client_new returned NULL")
        self._ptr = ptr

    def close(self) -> None:
        """Free the underlying native client handle."""
        if self._ptr is not None:
            self._lib.call_void("bc_client_free", self._ptr)
            self._ptr = None

    def __del__(self) -> None:
        self.close()

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
            CombError: subclass matching the failure kind (see
                :mod:`beachcomber.exceptions`).
        """
        return self.get_with_flags(key, path)

    def get_with_flags(
        self,
        key: str,
        path: Optional[str] = None,
        force: bool = False,
        wait: bool = False,
    ) -> CombResult:
        """Read a cached value with optional force/wait flags.

        Raises:
            CombError: subclass matching the failure kind.
        """
        flags = (_native.BC_GET_FORCE if force else 0) | (_native.BC_GET_WAIT if wait else 0)
        payload = self._lib.call(
            "bc_get", self._ptr, key.encode(), path.encode() if path is not None else None, flags
        )
        return _result_from_get_payload(payload)

    def refresh(self, key: str, path: Optional[str] = None) -> None:
        """Trigger recomputation of a provider.

        Raises:
            CombError: subclass matching the failure kind.
        """
        self._lib.call("bc_refresh", self._ptr, key.encode(), path.encode() if path is not None else None)

    def status(self) -> list[CacheRow]:
        """Return cache rows from the daemon.

        Raises:
            CombError: subclass matching the failure kind.
        """
        data = self._lib.call("bc_status", self._ptr)
        return _parse_cache_rows(data)

    def hello(self) -> HelloInfo:
        """Ask the daemon for protocol and build versions.

        Raises:
            CombError: subclass matching the failure kind.
        """
        data = self._lib.call("bc_hello", self._ptr)
        return _parse_hello(data)

    def put(
        self,
        key: str,
        data: Optional[Any] = None,
        ttl: Optional[str] = None,
        path: Optional[str] = None,
    ) -> None:
        """Store data into a virtual provider. ``data=None`` clears the entry.

        Raises:
            CombError: subclass matching the failure kind.
        """
        self._lib.call(
            "bc_put",
            self._ptr,
            key.encode(),
            json.dumps(data).encode(),
            ttl.encode() if ttl is not None else None,
            path.encode() if path is not None else None,
        )

    def introspect(
        self,
        subject: IntrospectSubject,
        duration_secs: Optional[int] = None,
    ) -> IntrospectResponse:
        """Run a diagnostic introspect query.

        Raises:
            CombError: subclass matching the failure kind.
        """
        options_json = (
            json.dumps({"duration_secs": duration_secs}) if duration_secs is not None else None
        )
        data = self._lib.call(
            "bc_introspect",
            self._ptr,
            subject.value.encode(),
            options_json.encode() if options_json else None,
        )
        return _parse_introspect(subject, data)

    def resolve(
        self,
        key: str,
        cwd: str,
        env: Optional[dict] = None,
        overrides: Optional[dict] = None,
    ) -> CombResult:
        """Resolve a virtual field (``"provider.field"``) or a path expression
        (a bare provider name) client-side, exactly as ``comb get``'s
        resolution layer does.

        Args:
            key: ``"provider.field"`` for a field expression, or a bare
                provider name for a path expression.
            cwd: Working directory the resolver evaluates against. Required
                — this library never reads the process's own working
                directory on the caller's behalf.
            env: Overrides for ``env.*`` refs in the expression. ``None``
                means none supplied (every ``env.*`` ref misses).
            overrides: Maps a field key (``"provider.field"``) or bare
                provider name to an expression string, overriding the
                built-in default for that key.

        Returns:
            :class:`~beachcomber.result.CombResult` — ``age_ms``/``stale``
            are not meaningful for resolution and are always ``0``/``False``.

        Raises:
            CombError: subclass matching the failure kind.
        """
        env_json = _opt_json(env)
        overrides_json = _opt_json(overrides)
        value = self._lib.call(
            "bc_resolve",
            self._ptr,
            key.encode(),
            cwd.encode(),
            env_json.encode() if env_json is not None else None,
            overrides_json.encode() if overrides_json is not None else None,
        )
        return CombResult(ok=True, data=value, age_ms=0, stale=False)

    def eval(
        self,
        template_str: str,
        cwd: str,
        env: Optional[dict] = None,
        overrides: Optional[dict] = None,
    ) -> Any:
        """Evaluate an arbitrary expression string — the same evaluator
        :meth:`resolve` uses for a declared virtual field, but for a raw
        expression that need not be registered anywhere.

        Args:
            template_str: The expression to evaluate.
            cwd: Required, matching :meth:`resolve`'s ``cwd`` semantics.
            env: Overrides for ``env.*`` refs.
            overrides: Field-expression overrides, same shape as
                :meth:`resolve`'s.

        Raises:
            CombError: subclass matching the failure kind.
        """
        env_json = _opt_json(env)
        overrides_json = _opt_json(overrides)
        return self._lib.call(
            "bc_eval",
            self._ptr,
            template_str.encode(),
            cwd.encode(),
            env_json.encode() if env_json is not None else None,
            overrides_json.encode() if overrides_json is not None else None,
        )

    def watch(self, key: str, path: Optional[str] = None) -> WatchStream:
        """Subscribe to a key and receive live updates.

        Returns a :class:`WatchStream` iterator that yields
        :class:`~beachcomber.types.WatchEvent` instances. Release it via
        :meth:`WatchStream.close` or by using the stream as a context
        manager.

        Raises:
            CombError: subclass matching the failure kind.
        """
        ptr = self._lib.call_ptr(
            "bc_watch_open", self._ptr, key.encode(), path.encode() if path is not None else None
        )
        if not ptr:
            raise CombError("bc_watch_open returned NULL")
        return WatchStream(ptr)

    @contextmanager
    def session(self) -> Generator["Session", None, None]:
        """Open a persistent connection as a context manager.

        Example::

            with client.session() as session:
                session.set_context("/my/repo")
                result = session.get("git.branch")

        Raises:
            CombError: subclass matching the failure kind.
        """
        session = Session(self)
        try:
            yield session
        finally:
            session.close()


class Session:
    """Persistent connection to the beachcomber daemon.

    Wraps a native ``BcSession`` handle for ``get``/``put``/``set_context``
    — the only ops the ABI exposes at session granularity. ``refresh``,
    ``status``, ``hello`` and ``introspect`` have no session-level ABI
    equivalent, so they delegate to the owning :class:`Client` on a fresh
    one-shot native connection; they are kept here for API compatibility,
    not because they reuse this session's socket.

    Create via :meth:`Client.session` rather than directly.
    """

    def __init__(self, client: Client) -> None:
        self._ptr = None
        self._client = client
        self._lib = client._lib
        ptr = self._lib.call_ptr("bc_session_open", client._ptr)
        if not ptr:
            raise CombError("bc_session_open returned NULL")
        self._ptr = ptr

    def close(self) -> None:
        """Close and free the underlying native session handle."""
        if self._ptr is not None:
            self._lib.call_void("bc_session_close", self._ptr)
            self._ptr = None

    def __del__(self) -> None:
        self.close()

    def set_context(self, path: str) -> None:
        """Set the default working-directory path for this connection.

        Raises:
            CombError: subclass matching the failure kind (e.g.
                :class:`~beachcomber.exceptions.BusyError` on concurrent use).
        """
        self._lib.call("bc_session_set_context", self._ptr, path.encode())

    def get(self, key: str, path: Optional[str] = None) -> CombResult:
        """Read a cached value using the persistent connection.

        Raises:
            CombError: subclass matching the failure kind.
        """
        return self.get_with_flags(key, path)

    def get_with_flags(
        self,
        key: str,
        path: Optional[str] = None,
        force: bool = False,
        wait: bool = False,
    ) -> CombResult:
        """Read a cached value with optional force/wait flags via the persistent connection.

        Raises:
            CombError: subclass matching the failure kind.
        """
        flags = (_native.BC_GET_FORCE if force else 0) | (_native.BC_GET_WAIT if wait else 0)
        payload = self._lib.call(
            "bc_session_get",
            self._ptr,
            key.encode(),
            path.encode() if path is not None else None,
            flags,
        )
        return _result_from_get_payload(payload)

    def put(
        self,
        key: str,
        data: Optional[Any] = None,
        ttl: Optional[str] = None,
        path: Optional[str] = None,
    ) -> None:
        """Store data into a virtual provider via the persistent connection.

        Raises:
            CombError: subclass matching the failure kind.
        """
        self._lib.call(
            "bc_session_put",
            self._ptr,
            key.encode(),
            json.dumps(data).encode(),
            ttl.encode() if ttl is not None else None,
            path.encode() if path is not None else None,
        )

    def refresh(self, key: str, path: Optional[str] = None) -> None:
        """Trigger recomputation. No session-level ABI equivalent exists;
        runs on a fresh one-shot native connection via the owning client.
        """
        self._client.refresh(key, path)

    def status(self) -> list[CacheRow]:
        """Return cache rows. No session-level ABI equivalent exists; runs
        on a fresh one-shot native connection via the owning client.
        """
        return self._client.status()

    def hello(self) -> HelloInfo:
        """Ask the daemon for versions. No session-level ABI equivalent
        exists; runs on a fresh one-shot native connection via the owning
        client.
        """
        return self._client.hello()

    def introspect(
        self,
        subject: IntrospectSubject,
        duration_secs: Optional[int] = None,
    ) -> IntrospectResponse:
        """Run a diagnostic introspect query. No session-level ABI
        equivalent exists; runs on a fresh one-shot native connection via
        the owning client.
        """
        return self._client.introspect(subject, duration_secs=duration_secs)
