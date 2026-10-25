"""Client and Session classes for the beachcomber daemon.

Transport: ``ctypes`` calls into the ``libbeachcomber`` C ABI (see
:mod:`._native`). Socket connection, retry, auto-start and wire-protocol
framing are all the native library's job now — this module only builds
request arguments, decodes typed results, and manages native handle
lifetimes.

Typical usage::

    from beachcomber import Client

    client = Client()
    branch = client.get("git.branch", cwd="/path/to/repo")  # None on a miss

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


def _read_args(
    cwd: Optional[str], env: Optional[dict], force: bool, wait: bool
) -> tuple[Optional[bytes], Optional[bytes], int]:
    """The ``cwd``, ``env_json`` and ``flags`` arguments ``bc_get`` and
    ``bc_session_get`` take."""
    env_json = _opt_json(env)
    flags = (_native.BC_GET_FORCE if force else 0) | (_native.BC_GET_WAIT if wait else 0)
    return (
        cwd.encode() if cwd is not None else None,
        env_json.encode() if env_json is not None else None,
        flags,
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
                poll_count=row.get("poll_count"),
                fsevents_reinstate=row.get("fsevents_reinstate"),
                polls_elapsed=row.get("polls_elapsed"),
                next_poll_in_secs=row.get("next_poll_in_secs"),
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
    socket_path: Optional[str], timeout: float, autostart: Optional[bool], exec: bool
) -> Optional[str]:
    options: dict = {}
    if socket_path is not None:
        options["socket_path"] = socket_path
    options["timeout_ms"] = int(round(timeout * 1000))
    if autostart is not None:
        options["autostart"] = autostart
    if exec:
        options["exec"] = True
    return json.dumps(options)


class WatchStream:
    """Iterator over watch events. Yields :class:`~beachcomber.types.WatchEvent` instances.

    Each event is the *resolved value* of the watched key — what
    ``comb get`` would print for it — re-resolved whenever any key the
    resolution reads changes, including the provider's template tree.

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
            :class:`~beachcomber.types.WatchEvent` carrying the resolved
            value on the ``event`` outcome; ``None`` on ``timeout``,
            ``eof``, or ``cancelled``. An event whose ``data`` is ``None``
            is a miss, not the end of the stream.
        """
        if self._ptr is None:
            return None
        env = self._lib.call_watch_next(self._ptr, timeout_ms)
        if env.get("outcome") != "event":
            return None
        return WatchEvent(data=env.get("data"))

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
        exec: Whether a template this client renders (:meth:`get`,
            :meth:`eval`) may call ``exec()``. Off by default: a template
            that calls it then fails with ``exec() is disabled in this
            context``.
    """

    def __init__(
        self,
        socket_path: Optional[str] = None,
        timeout: float = _DEFAULT_TIMEOUT,
        autostart: Optional[bool] = None,
        exec: bool = False,
    ) -> None:
        self._ptr = None
        self._lib = _native.get_lib()
        options_json = _build_options_json(socket_path, timeout, autostart, exec)
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

    def get(
        self,
        key: str,
        cwd: Optional[str] = None,
        env: Optional[dict] = None,
        force: bool = False,
        wait: bool = False,
    ) -> Any:
        """The value ``comb get`` prints for this key, cwd and env.

        The daemon holds the templates (``<provider>:template``, seeded from
        configuration or :meth:`put`); this client renders them. A plain
        field is its cached value, a templated one is rendered, and a bare
        provider name is the whole merged namespace. The unrendered halves
        are facets, read like any key: ``"p:template.x"`` is the template as
        stored, ``"p:put.x"`` and ``"p:source.x"`` those layers' values.

        Args:
            key: ``"provider.field"``, or a bare provider name.
            cwd: The directory the read addresses (``~`` is home, a relative
                path is against this process's working directory). ``None``
                is the global entry.
            env: The caller's environment for ``env.*`` refs. ``None`` means
                none supplied (every ``env.*`` is absent).
            force: Re-execute every Source the read demands first.
            wait: Re-execute a stale entry inline before answering.

        Returns:
            The value, or ``None`` on a miss.

        Raises:
            CombError: subclass matching the failure kind (see
                :mod:`beachcomber.exceptions`).
        """
        return self._lib.call(
            "bc_get",
            self._ptr,
            key.encode(),
            *_read_args(cwd, env, force, wait),
        )

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
        data: Any,
        ttl: Optional[str] = None,
        path: Optional[str] = None,
    ) -> None:
        """Store data into the cache as static data. ``data=None`` clears the entry.

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

    def eval(
        self,
        template_str: str,
        cwd: str,
        env: Optional[dict] = None,
    ) -> Any:
        """Render a template that is stored nowhere — the same value
        ``comb eval`` prints. Every ref resolves exactly as it would inside
        a stored template, so ``eval("{{ p.f }}")`` and ``get("p.f")``
        agree.

        ``template_str`` accepts a single ``{{ expr }}`` tag, or literal
        text/several tags — the former keeps the expression's natural
        type, the latter is always a string (a source with no tags at all
        is its literal text).

        Args:
            template_str: The template to render.
            cwd: Required, matching :meth:`get`'s ``cwd`` semantics.
            env: The caller's environment for ``env.*`` refs.

        Raises:
            CombError: subclass matching the failure kind.
        """
        env_json = _opt_json(env)
        return self._lib.call(
            "bc_eval",
            self._ptr,
            template_str.encode(),
            cwd.encode(),
            env_json.encode() if env_json is not None else None,
        )

    def watch(self, key: str, cwd: str, env: Optional[dict] = None) -> WatchStream:
        """Subscribe to a key's *value* and receive it again whenever it changes.

        Each event is the resolved value — what ``comb get`` would print for
        this key, ``cwd`` and ``env`` — so a template edited under the watch
        fires it just as a changed cache record does.

        Returns a :class:`WatchStream` iterator that yields
        :class:`~beachcomber.types.WatchEvent` instances. Release it via
        :meth:`WatchStream.close` or by using the stream as a context
        manager.

        Args:
            key: ``"provider.field"`` or a bare provider name.
            cwd: Required, matching :meth:`get`'s ``cwd`` semantics —
                the directory every subscription addresses and every
                re-resolution runs at.
            env: The caller's environment for ``env.*`` refs.

        Raises:
            CombError: subclass matching the failure kind.
        """
        env_json = _opt_json(env)
        ptr = self._lib.call_ptr(
            "bc_watch_open",
            self._ptr,
            key.encode(),
            cwd.encode(),
            env_json.encode() if env_json is not None else None,
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
                branch = session.get("git.branch")

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

    Wraps a native ``BcSession`` handle for ``get``/``put``/``refresh``
    and ``set_context``, which reuse its socket and its directory.
    ``status``, ``hello`` and ``introspect`` take no path and have no
    session-level ABI equivalent, so they delegate to the owning
    :class:`Client` on a fresh one-shot native connection.

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
        """Set this session's directory: every later ``get`` that names no
        ``cwd``, and ``put`` and ``refresh`` that name no path, address it. The session keeps it
        and sends it with each request.

        Raises:
            CombError: subclass matching the failure kind (e.g.
                :class:`~beachcomber.exceptions.BusyError` on concurrent use).
        """
        self._lib.call("bc_session_set_context", self._ptr, path.encode())

    def get(
        self,
        key: str,
        cwd: Optional[str] = None,
        env: Optional[dict] = None,
        force: bool = False,
        wait: bool = False,
    ) -> Any:
        """:meth:`Client.get` on the persistent connection. ``cwd=None`` is
        this session's directory (:meth:`set_context`) when it has one, else
        the global entry.

        Raises:
            CombError: subclass matching the failure kind.
        """
        return self._lib.call(
            "bc_session_get",
            self._ptr,
            key.encode(),
            *_read_args(cwd, env, force, wait),
        )

    def put(
        self,
        key: str,
        data: Any,
        ttl: Optional[str] = None,
        path: Optional[str] = None,
    ) -> None:
        """Store data into the cache as static data via the persistent connection.

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
        """Trigger recomputation at ``path``, else at this session's directory.

        Raises:
            CombError: subclass matching the failure kind.
        """
        self._lib.call(
            "bc_session_refresh",
            self._ptr,
            key.encode(),
            path.encode() if path is not None else None,
        )

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
