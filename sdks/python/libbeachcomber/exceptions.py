"""Exceptions raised by the beachcomber client: one per error kind of the SDK
API table (``docs/sdk-api.md``).

Every envelope the C ABI returns with ``ok: false`` carries a stable
``error.kind`` slug. Each slug is one :class:`CombError` subclass here, named
for it, with the slug as its ``.kind`` — callers should not need to
string-match ``str(exc)``. Three kinds are this SDK's own:
``library_not_found``, ``library_symbol`` and ``unsupported``.
"""

from __future__ import annotations

from typing import Optional


class CombError(Exception):
    """Base exception for all beachcomber client errors.

    Attributes:
        kind: The error kind slug: the C ABI envelope's ``error.kind`` for a
            failure the library reports, or one of this SDK's own.
        message: The ABI's message, unchanged (``str(exc)`` is the same), or
            this SDK's own for a failure it raises itself.
        library_version: The loaded library's ``bc_version()``, when one is
            loaded; ``None`` otherwise.
    """

    kind: Optional[str] = None

    def __init__(self, message: str, library_version: Optional[str] = None) -> None:
        super().__init__(message)
        self.message = message
        self.library_version = library_version


class DaemonNotRunningError(CombError):
    """No daemon answers, and none was started."""

    kind = "daemon_not_running"


class ConnectionFailedError(CombError):
    """An explicit ``socket_path`` could not be connected to."""

    kind = "connection_failed"


class IoError(CombError):
    """A read or write on the socket failed."""

    kind = "io_error"


class ParseError(CombError):
    """A request, a response or a template could not be read."""

    kind = "parse_error"


class ServerError(CombError):
    """The daemon refused the request, or a template failed to render."""

    kind = "server_error"


class TimeoutError(CombError):  # noqa: A001 - deliberately mirrors the ABI's "timeout" kind
    """A read or write ran out ``timeout_ms``, or ``next_event`` ran out its
    ``timeout_ms``."""

    kind = "timeout"


class BadFlagsError(CombError):
    """A flag bit the ABI does not define was set."""

    kind = "bad_flags"


class BusyError(CombError):
    """A session or watch is in use by another caller."""

    kind = "busy"


class PanicError(CombError):
    """The library panicked; the panic was caught at the boundary."""

    kind = "panic"


class VersionSkewError(CombError):
    """The daemon is a different version from the library."""

    kind = "version_skew"


class LibraryNotFoundError(CombError):
    """No ``libbeachcomber`` was found where discovery looks."""

    kind = "library_not_found"


class LibrarySymbolError(CombError):
    """The library found lacks a function this SDK calls."""

    kind = "library_symbol"


class UnsupportedError(CombError):
    """A transport cannot do this operation. This SDK has only the one
    (native) transport, so it never raises this; it exists so every SDK has
    the same error kinds."""

    kind = "unsupported"


_KIND_TO_EXCEPTION = {
    cls.kind: cls
    for cls in (
        DaemonNotRunningError,
        ConnectionFailedError,
        IoError,
        ParseError,
        ServerError,
        TimeoutError,
        BadFlagsError,
        BusyError,
        PanicError,
        VersionSkewError,
        LibraryNotFoundError,
        LibrarySymbolError,
        UnsupportedError,
    )
}


def exception_for_envelope_error(
    kind: Optional[str], message: str, lib_version: str
) -> CombError:
    """Build the exception for an ``ok: false`` envelope.

    Args:
        kind: The envelope's ``error.kind`` slug (may be unrecognised, e.g.
            from a newer library than this binding knows about).
        message: The envelope's ``error.message``.
        lib_version: The loaded library's ``bc_version()`` string.

    Returns:
        The kind's :class:`CombError` subclass, or :class:`CombError` itself
        with ``.kind`` set for a kind this SDK does not know.
    """
    cls = _KIND_TO_EXCEPTION.get(kind)  # type: ignore[arg-type]
    if cls is not None:
        return cls(message, lib_version)
    err = CombError(message, lib_version)
    err.kind = kind
    return err
