"""ctypes binding over the ``libbeachcomber`` C ABI.

Implements the Phase-4 binding contract:

1. Library discovery (see :mod:`.discovery`).
2. Loud discovery failure naming every location tried.
3. Required symbols checked at load, not on first use.
4. ``bc_version()`` read on load and included in every error raised.
5. ``ok: false`` envelopes become idiomatic exceptions with ``kind``
   preserved (see :mod:`.exceptions`).
6. Memory discipline: every ``char *`` this module returns to a caller is
   freed via ``bc_string_free``; ``bc_version()``'s return value never is.

This module is the sole owner of the loaded ``CDLL`` and of every ``ctypes``
signature; :mod:`.client` never touches ``ctypes`` directly.
"""

from __future__ import annotations

import ctypes
import json
import threading
from typing import Any, Optional

from . import discovery
from .exceptions import LibraryDiscoveryError, LibrarySymbolError, ProtocolError, exception_for_envelope_error

# name -> (argtypes, restype). Pointer-returning functions use c_void_p
# (not c_char_p) so the raw address survives for bc_string_free — ctypes'
# c_char_p restype copies the bytes and discards the pointer.
_SYMBOLS: dict = {
    "bc_version": ([], ctypes.c_char_p),
    "bc_client_new": ([ctypes.c_char_p], ctypes.c_void_p),
    "bc_client_free": ([ctypes.c_void_p], None),
    "bc_get": (
        [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_char_p, ctypes.c_uint32],
        ctypes.c_void_p,
    ),
    "bc_put": (
        [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_char_p, ctypes.c_char_p, ctypes.c_char_p],
        ctypes.c_void_p,
    ),
    "bc_put_null": ([ctypes.c_void_p, ctypes.c_char_p, ctypes.c_char_p], ctypes.c_void_p),
    "bc_refresh": ([ctypes.c_void_p, ctypes.c_char_p, ctypes.c_char_p], ctypes.c_void_p),
    "bc_status": ([ctypes.c_void_p], ctypes.c_void_p),
    "bc_introspect": (
        [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_char_p],
        ctypes.c_void_p,
    ),
    "bc_hello": ([ctypes.c_void_p], ctypes.c_void_p),
    "bc_resolve": (
        [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_char_p, ctypes.c_char_p, ctypes.c_char_p],
        ctypes.c_void_p,
    ),
    "bc_eval": (
        [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_char_p, ctypes.c_char_p, ctypes.c_char_p],
        ctypes.c_void_p,
    ),
    "bc_session_open": ([ctypes.c_void_p], ctypes.c_void_p),
    "bc_session_close": ([ctypes.c_void_p], None),
    "bc_session_get": (
        [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_char_p, ctypes.c_uint32],
        ctypes.c_void_p,
    ),
    "bc_session_put": (
        [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_char_p, ctypes.c_char_p, ctypes.c_char_p],
        ctypes.c_void_p,
    ),
    "bc_session_set_context": ([ctypes.c_void_p, ctypes.c_char_p], ctypes.c_void_p),
    "bc_watch_open": (
        [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_char_p],
        ctypes.c_void_p,
    ),
    "bc_watch_next": ([ctypes.c_void_p, ctypes.c_int32], ctypes.c_void_p),
    "bc_watch_cancel": ([ctypes.c_void_p], None),
    "bc_watch_free": ([ctypes.c_void_p], None),
    "bc_string_free": ([ctypes.c_void_p], None),
}

# bc_get / bc_session_get flags (mirrors BC_GET_FORCE / BC_GET_WAIT in
# libbeachcomber-ffi/include/beachcomber.h).
BC_GET_FORCE = 1 << 0
BC_GET_WAIT = 1 << 1


class NativeLib:
    """A loaded ``libbeachcomber`` with verified symbols and cached version.

    Args:
        cdll: An already-`ctypes.CDLL`-loaded handle.
        path: The path/spec it was loaded from (for error messages).
    """

    def __init__(self, cdll: ctypes.CDLL, path: str) -> None:
        self._cdll = cdll
        self.path = path
        self._check_and_bind_symbols()
        self.version = self._read_version()

    def _read_version(self) -> str:
        v = self._cdll.bc_version()
        return v.decode("utf-8", errors="replace") if v else "unknown"

    def _check_and_bind_symbols(self) -> None:
        for name in _SYMBOLS:
            if not hasattr(self._cdll, name):
                version = None
                if hasattr(self._cdll, "bc_version"):
                    try:
                        fn = self._cdll.bc_version
                        fn.restype = ctypes.c_char_p
                        raw = fn()
                        version = raw.decode("utf-8", errors="replace") if raw else None
                    except Exception:
                        version = None
                raise LibrarySymbolError(
                    f"{self.path} is missing required symbol {name!r} "
                    f"(loaded library version: {version if version is not None else 'unknown'})"
                )
        for name, (argtypes, restype) in _SYMBOLS.items():
            fn = getattr(self._cdll, name)
            fn.argtypes = argtypes
            fn.restype = restype

    # -- raw pointer-returning constructors (BcClient*/BcSession*/BcWatch*) --

    def call_ptr(self, name: str, *args: Any) -> Optional[int]:
        """Call a handle-constructor function; returns the raw pointer or None."""
        fn = getattr(self._cdll, name)
        return fn(*args)

    def call_void(self, name: str, *args: Any) -> None:
        """Call a void-returning function (frees/cancels a handle)."""
        fn = getattr(self._cdll, name)
        fn(*args)

    # -- envelope-returning functions --

    def _decode_envelope(self, ptr: Optional[int]) -> dict:
        if not ptr:
            raise ProtocolError(
                f"native call returned NULL (beachcomber lib version: {self.version})"
            )
        raw = ctypes.cast(ptr, ctypes.c_char_p).value
        self._cdll.bc_string_free(ptr)
        if raw is None:
            raise ProtocolError(
                f"native call returned an empty envelope (beachcomber lib version: {self.version})"
            )
        try:
            text = raw.decode("utf-8")
        except UnicodeDecodeError as exc:
            raise ProtocolError(
                f"envelope is not valid UTF-8: {exc} (beachcomber lib version: {self.version})"
            ) from exc
        try:
            return json.loads(text)
        except json.JSONDecodeError as exc:
            raise ProtocolError(
                f"malformed envelope JSON: {exc} (beachcomber lib version: {self.version})"
            ) from exc

    def call(self, name: str, *args: Any) -> Any:
        """Call an ordinary ``{"ok":..., "data"/"error":...}`` envelope
        function and return the unwrapped ``data``, raising on ``ok: false``.
        """
        fn = getattr(self._cdll, name)
        env = self._decode_envelope(fn(*args))
        if not env.get("ok"):
            err = env.get("error") or {}
            raise exception_for_envelope_error(
                err.get("kind"), err.get("message", "unknown error"), self.version
            )
        return env.get("data")

    def call_watch_next(self, ptr: int, timeout_ms: int) -> dict:
        """Call ``bc_watch_next``, returning its decoded envelope as-is
        (callers distinguish the ``outcome`` field themselves) or raising on
        ``ok: false``.
        """
        env = self._decode_envelope(self._cdll.bc_watch_next(ptr, timeout_ms))
        if not env.get("ok"):
            err = env.get("error") or {}
            raise exception_for_envelope_error(
                err.get("kind"), err.get("message", "unknown error"), self.version
            )
        return env


_lock = threading.Lock()
_lib: Optional[NativeLib] = None


def get_lib() -> NativeLib:
    """Return the process-wide loaded :class:`NativeLib`, loading it on first use."""
    global _lib
    if _lib is not None:
        return _lib
    with _lock:
        if _lib is not None:
            return _lib
        _lib = _load()
        return _lib


def _load() -> NativeLib:
    tried: list = []
    for candidate in discovery.candidates():
        if candidate.path is None:
            tried.append(f"  - {candidate.description}: skipped ({candidate.reason})")
            continue
        try:
            cdll = ctypes.CDLL(candidate.path)
        except OSError as exc:
            tried.append(f"  - {candidate.description} ({candidate.path}): {exc}")
            continue
        return NativeLib(cdll, candidate.path)

    raise LibraryDiscoveryError(
        "could not load libbeachcomber; tried, in order:\n" + "\n".join(tried)
    )
