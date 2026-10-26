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
from .exceptions import IoError, ParseError, TimeoutError
from .types import (
    CacheRow,
    DaemonHealth,
    HelloInfo,
    IntrospectResponse,
    IntrospectSubject,
    ReaperStatus,
    Verdict,
    WatchEvent,
)


def _opt_json(value: Optional[Any]) -> Optional[str]:
    """Serialise a nullable dict/value argument to JSON, or leave it None."""
    if value is None:
        return None
    return json.dumps(value)


def _read_args(
    cwd: Optional[str], env: Optional[dict], force: bool, wait: Optional[str]
) -> tuple[Optional[bytes], Optional[bytes], Optional[bytes]]:
    """The ``cwd``, ``env_json`` and ``options_json`` arguments ``bc_get`` and
    ``bc_session_get`` take. The options are the ones the caller gave:
    ``force`` when it is set, ``wait`` as it was given; none at all is
    NULL."""
    options: dict = {}
    if force:
        options["force"] = True
    if wait is not None:
        options["wait"] = wait
    env_json = _opt_json(env)
    options_json = _opt_json(options or None)
    return (
        cwd.encode() if cwd is not None else None,
        env_json.encode() if env_json is not None else None,
        options_json.encode() if options_json is not None else None,
    )


def _parse_hello(data: Optional[dict]) -> HelloInfo:
    if not isinstance(data, dict):
        data = {}
    return HelloInfo(
        protocol_version=str(data.get("protocol_version", "")),
        daemon_version=str(data.get("daemon_version", "")),
    )


def _parse_cache_rows(data: Any) -> list[CacheRow]:
    if not isinstance(data, list) or not all(isinstance(row, dict) for row in data):
        raise ParseError(f"status payload is not an array of rows: {data!r}")
    return [
        CacheRow(
            provider=str(row.get("provider", "")),
            path=row.get("path"),
            source=str(row.get("source", "")),
            field=str(row.get("field", "")),
            value=row.get("value"),
            stale=bool(row.get("stale", False)),
            age_ms=row.get("age_ms"),
            kind=row.get("kind"),
            poll_interval_secs=row.get("poll_interval_secs"),
            poll_count=row.get("poll_count"),
            polls_elapsed=row.get("polls_elapsed"),
            next_poll_in_secs=row.get("next_poll_in_secs"),
            failure=row.get("failure"),
        )
        for row in data
    ]


def _parse_daemon_health(data: dict) -> DaemonHealth:
    verdicts = []
    for v in data.get("verdicts", []) or []:
        if isinstance(v, dict):
            verdicts.append(Verdict(level=str(v.get("level", "")), message=str(v.get("message", ""))))
    reaper = data.get("reaper")
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
        watch_backend=str(data.get("watch_backend", "")),
        reaper=ReaperStatus(
            armed=bool(reaper.get("armed", False)),
            visibility=str(reaper.get("visibility", "")),
            sweeps=int(reaper.get("sweeps", 0) or 0),
            reaped=int(reaper.get("reaped", 0) or 0),
            kill_denied=int(reaper.get("kill_denied", 0) or 0),
        )
        if isinstance(reaper, dict)
        else None,
        verdicts=verdicts,
    )


def _parse_introspect(subject: IntrospectSubject, data: Any) -> IntrospectResponse:
    if subject == IntrospectSubject.DAEMON and isinstance(data, dict):
        return IntrospectResponse(subject=subject, daemon=_parse_daemon_health(data), other=None)
    return IntrospectResponse(subject=subject, daemon=None, other=data)


def _build_options_json(**options: Any) -> str:
    """The ``options_json`` ``bc_client_new`` takes: exactly the options the
    caller gave (``None`` is not given), under the ABI's own names. The SDK
    adds no defaults; an option left out is the library's."""
    return json.dumps({name: value for name, value in options.items() if value is not None})


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

    def next_event(self, timeout_ms: Optional[int] = None) -> Optional[WatchEvent]:
        """Read the next event from the stream.

        Args:
            timeout_ms: How long to wait for one, in milliseconds; ``0``
                polls. ``None`` waits indefinitely.

        Returns:
            The next :class:`~beachcomber.types.WatchEvent`, or ``None``
            once the stream has ended (the daemon closed it, or it was
            cancelled or closed). An event whose ``data`` is ``None`` is a
            miss, not the end of the stream.

        Raises:
            TimeoutError: ``timeout_ms`` ran out with no event (kind
                ``timeout``); the stream is still open.
            CombError: subclass matching any other failure kind.
        """
        if self._ptr is None:
            return None
        wait = -1 if timeout_ms is None else timeout_ms
        env = self._lib.call_watch_next(self._ptr, wait)
        outcome = env.get("outcome")
        if outcome == "event":
            return WatchEvent(data=env.get("data"))
        if outcome == "timeout":
            raise TimeoutError(f"no watch event within {wait}ms", self._lib.version)
        return None

    def cancel(self) -> None:
        """End the stream: a :meth:`next_event` waiting on another thread,
        and every later one, answers ``None``."""
        if self._ptr is not None:
            self._lib.call_void("bc_watch_cancel", self._ptr)

    def close(self) -> None:
        """End the stream and free the underlying native watch handle."""
        if self._ptr is not None:
            self._lib.call_void("bc_watch_cancel", self._ptr)
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
        timeout_ms: How long one read or write on the socket may take, in
            milliseconds; at least 1. ``None`` is the library's default
            (100).
        connect_timeout_ms: How long connecting keeps retrying before it
            gives up, in milliseconds; ``0`` is one attempt. ``None`` is the
            library's default (1500).
        autostart: Whether to attempt starting the daemon if it isn't
            running. ``None`` is the library's default (on).
        exec: Whether a template this client renders (:meth:`get`,
            :meth:`eval`) may call ``exec()``. ``None`` is the library's
            default, off: a template that calls it then fails with
            ``exec() is disabled in this context``.

    Each option given is passed to ``bc_client_new`` as it is; this SDK has
    no defaults of its own.
    """

    # Set before ``__init__`` runs, so ``__del__`` is safe on a client whose
    # construction was refused.
    _ptr: Optional[int] = None

    def __init__(
        self,
        socket_path: Optional[str] = None,
        timeout_ms: Optional[int] = None,
        connect_timeout_ms: Optional[int] = None,
        autostart: Optional[bool] = None,
        exec: Optional[bool] = None,
    ) -> None:
        self._ptr = None
        self._lib = _native.get_lib()
        options_json = _build_options_json(
            socket_path=socket_path,
            timeout_ms=timeout_ms,
            connect_timeout_ms=connect_timeout_ms,
            autostart=autostart,
            exec=exec,
        )
        ptr = self._lib.call_ptr("bc_client_new", options_json.encode())
        if not ptr:
            raise IoError("bc_client_new returned NULL (allocation failure)", self._lib.version)
        self._ptr = ptr

    def _handle(self) -> int:
        """The native handle every call passes on. A closed client has none,
        and handing the library a freed or NULL handle would crash the
        process, so the call fails here instead."""
        if self._ptr is None:
            raise IoError("the client is closed")
        return self._ptr

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
        wait: Optional[str] = None,
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
                the calling process's environment, as ``comb`` renders.
            force: Re-execute every Source the read demands first.
            wait: How long to wait for a run the read needs before
                answering what is stored, as ``comb get --wait``: a duration
                (``"0"``, ``"50ms"``, ``"2s"``) or ``"forever"``. ``None`` is
                the read's budget. The read may take ``timeout_ms`` plus the
                wait; ``"forever"`` has no deadline.

        Returns:
            The value, or ``None`` on a miss.

        Raises:
            CombError: subclass matching the failure kind (see
                :mod:`beachcomber.exceptions`).
        """
        return self._lib.call(
            "bc_get",
            self._handle(),
            key.encode(),
            *_read_args(cwd, env, force, wait),
        )

    def refresh(self, key: str, path: Optional[str] = None) -> None:
        """Trigger recomputation of a provider.

        Raises:
            CombError: subclass matching the failure kind.
        """
        self._lib.call("bc_refresh", self._handle(), key.encode(), path.encode() if path is not None else None)

    def status(self) -> list[CacheRow]:
        """Return cache rows from the daemon.

        Raises:
            CombError: subclass matching the failure kind.
        """
        data = self._lib.call("bc_status", self._handle())
        return _parse_cache_rows(data)

    def hello(self) -> HelloInfo:
        """Ask the daemon for protocol and build versions.

        Raises:
            CombError: subclass matching the failure kind.
        """
        data = self._lib.call("bc_hello", self._handle())
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
            self._handle(),
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
            self._handle(),
            subject.value.encode(),
            options_json.encode() if options_json else None,
        )
        return _parse_introspect(subject, data)

    def eval(
        self,
        template_str: str,
        cwd: Optional[str] = None,
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
            cwd: Read as :meth:`get` reads it; ``None`` is the global entry.
            env: The caller's environment for ``env.*`` refs.

        Returns:
            The value, or ``None`` when the template renders nothing.

        Raises:
            CombError: subclass matching the failure kind.
        """
        env_json = _opt_json(env)
        return self._lib.call(
            "bc_eval",
            self._handle(),
            template_str.encode(),
            cwd.encode() if cwd is not None else None,
            env_json.encode() if env_json is not None else None,
        )

    def watch(self, key: str, cwd: Optional[str] = None, env: Optional[dict] = None) -> WatchStream:
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
            cwd: Read as :meth:`get` reads it — the directory every
                subscription addresses and every re-resolution runs at;
                ``None`` is the global entry.
            env: The caller's environment for ``env.*`` refs.

        Raises:
            CombError: subclass matching the failure kind.
        """
        env_json = _opt_json(env)
        ptr = self._lib.call_ptr(
            "bc_watch_open",
            self._handle(),
            key.encode(),
            cwd.encode() if cwd is not None else None,
            env_json.encode() if env_json is not None else None,
        )
        if not ptr:
            raise IoError("bc_watch_open returned NULL (allocation failure)", self._lib.version)
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
    ``status``, ``hello`` and ``introspect`` address no directory and need
    no connection of their own, so they are the :class:`Client`'s alone.

    Create via :meth:`Client.session` rather than directly.
    """

    def __init__(self, client: Client) -> None:
        self._ptr = None
        # Held so the client is not freed while its session is open.
        self._client = client
        self._lib = client._lib
        ptr = self._lib.call_ptr("bc_session_open", client._ptr)
        if not ptr:
            raise IoError("bc_session_open returned NULL (allocation failure)", self._lib.version)
        self._ptr = ptr

    def _handle(self) -> int:
        """The native handle every call passes on. A closed session has none,
        and handing the library a freed or NULL handle would crash the
        process, so the call fails here instead."""
        if self._ptr is None:
            raise IoError("the session is closed")
        return self._ptr

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
        self._lib.call("bc_session_set_context", self._handle(), path.encode())

    def get(
        self,
        key: str,
        cwd: Optional[str] = None,
        env: Optional[dict] = None,
        force: bool = False,
        wait: Optional[str] = None,
    ) -> Any:
        """:meth:`Client.get` on the persistent connection. ``cwd=None`` is
        this session's directory (:meth:`set_context`) when it has one, else
        the global entry.

        Raises:
            CombError: subclass matching the failure kind.
        """
        return self._lib.call(
            "bc_session_get",
            self._handle(),
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
            self._handle(),
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
            self._handle(),
            key.encode(),
            path.encode() if path is not None else None,
        )
