"""Unit tests for envelope-error -> exception mapping, and for the payloads
that are not the shape their op answers. Pure: no native library or daemon
required.
"""

from __future__ import annotations

import pytest

from libbeachcomber.client import _parse_cache_rows
from libbeachcomber.exceptions import (
    BadFlagsError,
    BusyError,
    CombError,
    ConnectionFailedError,
    DaemonNotRunningError,
    IoError,
    PanicError,
    ParseError,
    ServerError,
    TimeoutError,
    VersionSkewError,
    exception_for_envelope_error,
)


@pytest.mark.parametrize(
    "kind,expected_cls",
    [
        ("daemon_not_running", DaemonNotRunningError),
        ("connection_failed", ConnectionFailedError),
        ("io_error", IoError),
        ("parse_error", ParseError),
        ("server_error", ServerError),
        ("timeout", TimeoutError),
        ("busy", BusyError),
        ("bad_flags", BadFlagsError),
        ("panic", PanicError),
        ("version_skew", VersionSkewError),
    ],
)
def test_known_kind_maps_to_expected_subclass(kind: str, expected_cls: type) -> None:
    exc = exception_for_envelope_error(kind, "boom", "1.2.3")
    assert type(exc) is expected_cls
    assert exc.kind == kind


def test_the_message_is_the_abis_and_the_version_a_field_of_its_own() -> None:
    exc = exception_for_envelope_error("server_error", "bad request", "9.9.9")
    assert str(exc) == "bad request"
    assert exc.message == "bad request"
    assert exc.library_version == "9.9.9"


def test_unknown_kind_falls_back_to_base_comberror_with_kind_preserved() -> None:
    exc = exception_for_envelope_error("something_new", "boom", "1.0.0")
    assert type(exc) is CombError
    assert exc.kind == "something_new"
    assert exc.library_version == "1.0.0"


def test_a_status_payload_that_is_not_an_array_is_a_parse_error() -> None:
    with pytest.raises(ParseError):
        _parse_cache_rows({"not": "rows"})
