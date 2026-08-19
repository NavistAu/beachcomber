"""Unit tests for envelope-error -> exception mapping.

Pure: no native library or daemon required.
"""

from __future__ import annotations

import pytest

from libbeachcomber.exceptions import (
    BadFlagsError,
    BusyError,
    CombError,
    ConnectionFailedError,
    DaemonNotRunning,
    PanicError,
    ProtocolError,
    ServerError,
    TimeoutError,
    VersionSkewError,
    exception_for_envelope_error,
)


@pytest.mark.parametrize(
    "kind,expected_cls",
    [
        ("daemon_not_running", DaemonNotRunning),
        ("connection_failed", ConnectionFailedError),
        ("server_error", ServerError),
        ("timeout", TimeoutError),
        ("busy", BusyError),
        ("bad_flags", BadFlagsError),
        ("panic", PanicError),
        ("version_skew", VersionSkewError),
        ("io_error", ProtocolError),
        ("parse_error", ProtocolError),
    ],
)
def test_known_kind_maps_to_expected_subclass(kind: str, expected_cls: type) -> None:
    exc = exception_for_envelope_error(kind, "boom", "1.2.3")
    assert isinstance(exc, expected_cls)
    assert exc.kind == kind


def test_message_includes_library_version() -> None:
    exc = exception_for_envelope_error("server_error", "bad request", "9.9.9")
    assert "boom" not in str(exc)
    assert "bad request" in str(exc)
    assert "9.9.9" in str(exc)


def test_unknown_kind_falls_back_to_base_combrror_with_kind_preserved() -> None:
    exc = exception_for_envelope_error("something_new", "boom", "1.0.0")
    assert type(exc) is CombError
    assert exc.kind == "something_new"


def test_server_error_exposes_message_attribute_for_conformance_runner() -> None:
    exc = exception_for_envelope_error("server_error", "no such key", "1.0.0")
    assert isinstance(exc, ServerError)
    assert "no such key" in exc.message


def test_all_combrror_subclasses_carry_a_kind_class_attribute() -> None:
    for cls in (
        DaemonNotRunning,
        ConnectionFailedError,
        ServerError,
        TimeoutError,
        BusyError,
        BadFlagsError,
        PanicError,
        VersionSkewError,
    ):
        assert cls.kind is not None
