"""Newline-delimited JSON protocol helpers for the beachcomber daemon.

All requests are JSON objects with an ``"op"`` field. All responses are
JSON objects with an ``"ok"`` field that is ``true`` or ``false``.
"""

from __future__ import annotations

import json
from typing import Any, Optional

from .exceptions import ProtocolError, ServerError


def encode_request(op: str, **fields: Any) -> bytes:
    """Serialise a request object to a newline-terminated JSON bytes value.

    Args:
        op: The operation name (``"get"``, ``"refresh"``, ``"context"``,
            or ``"status"``).
        **fields: Additional key/value pairs to include in the request.

    Returns:
        UTF-8 encoded JSON followed by a newline byte.
    """
    payload: dict[str, Any] = {"op": op}
    payload.update(fields)
    return (json.dumps(payload, separators=(",", ":")) + "\n").encode()


def decode_response(line: str) -> dict[str, Any]:
    """Parse a single newline-delimited JSON response line.

    Args:
        line: A single text line received from the daemon.

    Returns:
        Parsed response dict.

    Raises:
        ProtocolError: If the line is not valid JSON or is missing the
            ``"ok"`` field.
        ServerError: If the response contains ``"ok": false``.
    """
    line = line.strip()
    if not line:
        raise ProtocolError("received empty response from daemon")

    try:
        data: dict[str, Any] = json.loads(line)
    except json.JSONDecodeError as exc:
        raise ProtocolError(f"invalid JSON from daemon: {exc}") from exc

    if not isinstance(data, dict):
        raise ProtocolError(f"expected JSON object, got {type(data).__name__}")

    ok = data.get("ok")
    if ok is None:
        raise ProtocolError("response missing 'ok' field")

    if ok is False:
        error_msg = data.get("error", "unknown error")
        raise ServerError(str(error_msg))

    return data


def build_get_request(
    key: str,
    path: Optional[str] = None,
    force: bool = False,
    wait: bool = False,
) -> bytes:
    """Build a ``get`` request with optional flags.

    Args:
        key: Provider key, e.g. ``"git.branch"`` or ``"git"``.
        path: Optional working-directory path for per-directory providers.
        force: If ``True``, bypass cache and force recomputation.
        wait: If ``True``, block until a fresh value is available.

    Returns:
        Encoded request bytes.
    """
    kwargs: dict[str, Any] = {"key": key}
    if path is not None:
        kwargs["path"] = path
    if force:
        kwargs["force"] = True
    if wait:
        kwargs["wait"] = True
    return encode_request("get", **kwargs)


def build_refresh_request(key: str, path: Optional[str] = None) -> bytes:
    """Build a ``refresh`` request.

    Args:
        key: Provider key to recompute.
        path: Optional working-directory path.

    Returns:
        Encoded request bytes.
    """
    kwargs: dict[str, Any] = {"key": key}
    if path is not None:
        kwargs["path"] = path
    return encode_request("refresh", **kwargs)


def build_context_request(path: str) -> bytes:
    """Build a ``context`` request to set the default path for a session.

    Args:
        path: Directory path to set as the connection context.

    Returns:
        Encoded request bytes.
    """
    return encode_request("context", path=path)


def build_status_request() -> bytes:
    """Build a ``status`` request."""
    return encode_request("status")


def build_hello_request() -> bytes:
    """Build a ``hello`` request."""
    return encode_request("hello")


def build_put_request(
    key: str,
    data: Optional[Any] = None,
    ttl: Optional[str] = None,
    path: Optional[str] = None,
) -> bytes:
    """Build a ``put`` request.

    Args:
        key: Provider key for the virtual entry.
        data: Data payload. ``None`` clears the entry.
        ttl: Optional time-to-live string (e.g. ``"60s"``).
        path: Optional path for per-directory virtual entries.

    Returns:
        Encoded request bytes.
    """
    kwargs: dict[str, Any] = {"key": key}
    if data is not None:
        kwargs["data"] = data
    if ttl is not None:
        kwargs["ttl"] = ttl
    if path is not None:
        kwargs["path"] = path
    return encode_request("put", **kwargs)


def build_watch_request(key: str, path: Optional[str] = None) -> bytes:
    """Build a ``watch`` request.

    Args:
        key: Provider key to watch.
        path: Optional working-directory path.

    Returns:
        Encoded request bytes.
    """
    kwargs: dict[str, Any] = {"key": key}
    if path is not None:
        kwargs["path"] = path
    return encode_request("watch", **kwargs)


def build_introspect_request(
    subject: str, duration_secs: Optional[int] = None
) -> bytes:
    """Build an ``introspect`` request.

    Args:
        subject: Subject string (see :class:`~beachcomber.types.IntrospectSubject`).
        duration_secs: Optional sampling duration for metrics-style subjects.

    Returns:
        Encoded request bytes.
    """
    kwargs: dict[str, Any] = {"subject": subject}
    if duration_secs is not None:
        kwargs["duration_secs"] = duration_secs
    return encode_request("introspect", **kwargs)
