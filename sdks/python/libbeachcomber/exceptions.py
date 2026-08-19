"""Exceptions raised by the beachcomber client.

Every envelope the C ABI returns with ``ok: false`` carries a stable
``error.kind`` slug (see ``libbeachcomber-ffi/src/envelope.rs``). This module
maps each slug to an idiomatic Python exception subclass with that slug
preserved as a machine-readable ``.kind`` attribute — callers should not
need to string-match ``str(exc)``.
"""

from __future__ import annotations

from typing import Optional


class CombError(Exception):
    """Base exception for all beachcomber client errors.

    Attributes:
        kind: Machine-readable error kind slug. Matches the C ABI envelope's
            ``error.kind`` field for errors raised by the daemon or the
            native library; ``"library_discovery"`` / ``"library_symbol"``
            for binding-side failures that never reach the ABI.
    """

    kind: Optional[str] = None


class LibraryDiscoveryError(CombError):
    """No candidate location yielded a loadable ``libbeachcomber``."""

    kind = "library_discovery"


class LibrarySymbolError(CombError):
    """The loaded library is missing a required ``bc_*`` symbol."""

    kind = "library_symbol"


class DaemonNotRunning(CombError):
    """The daemon socket could not be reached (and autostart failed/disabled)."""

    kind = "daemon_not_running"


class ConnectionFailedError(CombError):
    """The connection to the daemon failed for a reason short of "not running"."""

    kind = "connection_failed"


class ProtocolError(CombError):
    """I/O failure, or a response that could not be parsed / was malformed.

    Covers both the ABI's ``io_error`` and ``parse_error`` kinds, plus
    binding-side envelope decoding failures (``kind`` left ``None`` for the
    latter).
    """

    def __init__(self, message: str, kind: Optional[str] = None) -> None:
        self.kind = kind
        super().__init__(message)


class ServerError(CombError):
    """The daemon returned ``ok: false`` for reasons of its own (bad request, etc.).

    Attributes:
        message: The error string from the daemon.
    """

    kind = "server_error"

    def __init__(self, message: str) -> None:
        self.message = message
        super().__init__(message)


class TimeoutError(CombError):  # noqa: A001 - deliberately mirrors the ABI's "timeout" kind
    """The operation timed out."""

    kind = "timeout"


class BusyError(CombError):
    """A session/watch handle is already in use by another caller."""

    kind = "busy"


class BadFlagsError(CombError):
    """An unrecognised bit was set in a ``get`` flags argument."""

    kind = "bad_flags"


class PanicError(CombError):
    """The native library panicked; caught at the FFI boundary."""

    kind = "panic"


class VersionSkewError(CombError):
    """The daemon's reported version does not match the loaded library's."""

    kind = "version_skew"


_KIND_TO_EXCEPTION = {
    "daemon_not_running": DaemonNotRunning,
    "connection_failed": ConnectionFailedError,
    "server_error": ServerError,
    "timeout": TimeoutError,
    "busy": BusyError,
    "bad_flags": BadFlagsError,
    "panic": PanicError,
    "version_skew": VersionSkewError,
}


def exception_for_envelope_error(
    kind: Optional[str], message: str, lib_version: str
) -> CombError:
    """Build the idiomatic exception for an ``ok: false`` envelope.

    Every raised error includes the loaded library's ``bc_version()`` so a
    version-skew diagnosis is possible from the message alone, per the
    Phase-4 binding contract.

    Args:
        kind: The envelope's ``error.kind`` slug (may be unrecognised, e.g.
            from a newer library than this binding knows about).
        message: The envelope's ``error.message``.
        lib_version: The loaded library's ``bc_version()`` string.

    Returns:
        A :class:`CombError` subclass instance with ``.kind`` set.
    """
    full_message = f"{message} (beachcomber lib version: {lib_version})"
    if kind in ("io_error", "parse_error"):
        return ProtocolError(full_message, kind=kind)
    cls = _KIND_TO_EXCEPTION.get(kind)  # type: ignore[arg-type]
    if cls is not None:
        if cls is ServerError:
            return ServerError(full_message)
        return cls(full_message)
    err = CombError(full_message)
    err.kind = kind
    return err
