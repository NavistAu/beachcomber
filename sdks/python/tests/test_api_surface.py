"""The SDK's public surface is the SDK API table's (docs/sdk-api.md), as
``sdks/api.json`` writes it down: the same methods, options, error kinds and
typed shapes as every other SDK. Pure: no native library or daemon needed."""

from __future__ import annotations

import dataclasses
import inspect
import json
import pathlib

import pytest

import libbeachcomber
from libbeachcomber import exceptions, types
from libbeachcomber.client import Client, Session, WatchStream

_TABLE = json.loads(
    (pathlib.Path(__file__).resolve().parents[2] / "api.json").read_text()
)


def _public_methods(cls: type) -> list[str]:
    return sorted(
        name
        for name, member in inspect.getmembers(cls)
        if not name.startswith("_") and callable(member)
    )


@pytest.mark.parametrize(
    "cls,entry",
    [(Client, "client"), (Session, "session"), (WatchStream, "watch_stream")],
)
def test_the_methods_are_the_tables(cls: type, entry: str) -> None:
    assert _public_methods(cls) == sorted(_TABLE[entry]["methods"])


def test_the_client_options_are_the_tables() -> None:
    params = list(inspect.signature(Client.__init__).parameters)[1:]
    assert params == _TABLE["client"]["options"]


def _class_name(kind: str) -> str:
    """``daemon_not_running`` is ``DaemonNotRunningError``; a slug that
    already ends in ``_error`` (``io_error``) is not doubled."""
    words = kind.removesuffix("_error").split("_")
    return "".join(w.capitalize() for w in words) + "Error"


def test_one_error_per_kind_named_as_the_table_says() -> None:
    subclasses = {
        cls
        for cls in vars(exceptions).values()
        if isinstance(cls, type) and issubclass(cls, exceptions.CombError) and cls is not exceptions.CombError
    }
    assert sorted(cls.__name__ for cls in subclasses) == sorted(
        _class_name(kind) for kind in _TABLE["error_kinds"]
    )
    for kind in _TABLE["error_kinds"]:
        cls = getattr(exceptions, _class_name(kind))
        assert cls.kind == kind
        assert getattr(libbeachcomber, cls.__name__) is cls


def test_every_error_carries_the_tables_fields() -> None:
    for kind in _TABLE["error_kinds"]:
        err = exceptions.exception_for_envelope_error(kind, "boom", "1.2.3")
        for name in _TABLE["error_fields"]:
            assert hasattr(err, name), (kind, name)


@pytest.mark.parametrize(
    "name,entry",
    [
        ("CacheRow", "cache_row"),
        ("HelloInfo", "hello_info"),
        ("DaemonHealth", "daemon_health"),
        ("ReaperStatus", "reaper_status"),
        ("Verdict", "verdict"),
        ("IntrospectResponse", "introspect_response"),
        ("WatchEvent", "watch_event"),
    ],
)
def test_the_typed_shapes_are_the_tables(name: str, entry: str) -> None:
    cls = getattr(types, name, None)
    assert cls is not None, f"no {name}"
    assert getattr(libbeachcomber, name) is cls
    assert sorted(f.name for f in dataclasses.fields(cls)) == sorted(_TABLE["types"][entry])
