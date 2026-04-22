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
            ``"list"``, or ``"status"``).
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


def build_get_request(key: str, path: Optional[str] = None) -> bytes:
    """Build a ``get`` request.

    Args:
        key: Provider key, e.g. ``"git.branch"`` or ``"git"``.
        path: Optional working-directory path for per-directory providers.

    Returns:
        Encoded request bytes.
    """
    kwargs: dict[str, Any] = {"key": key}
    if path is not None:
        kwargs["path"] = path
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


def build_list_request() -> bytes:
    """Build a ``list`` request."""
    return encode_request("list")


def build_status_request() -> bytes:
    """Build a ``status`` request."""
    return encode_request("status")
