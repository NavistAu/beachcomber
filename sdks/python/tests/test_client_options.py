"""The options ``Client.__init__`` hands ``bc_client_new``: exactly the ones
the caller gave, under the ABI's names, and no defaults of the SDK's own."""

from __future__ import annotations

import json
import shutil
import tempfile
import time

import pytest

from libbeachcomber import Client, ConnectionFailedError
from libbeachcomber.client import _build_options_json


def test_an_option_not_given_is_not_sent() -> None:
    assert json.loads(_build_options_json()) == {}


def test_the_options_reach_the_options_json_as_given() -> None:
    options = json.loads(
        _build_options_json(
            socket_path="/tmp/s",
            timeout_ms=250,
            connect_timeout_ms=0,
            autostart=False,
            exec=True,
        )
    )
    assert options == {
        "socket_path": "/tmp/s",
        "timeout_ms": 250,
        "connect_timeout_ms": 0,
        "autostart": False,
        "exec": True,
    }


def test_the_seconds_timeout_is_gone() -> None:
    with pytest.raises(TypeError):
        Client(socket_path="/tmp/s", timeout=0.5)  # type: ignore[call-arg]


def test_connect_timeout_ms_bounds_the_connect() -> None:
    # A short path: a socket path past the platform limit fails before any
    # retry, which would pass this test for the wrong reason.
    root = tempfile.mkdtemp(prefix="bcct", dir="/tmp")
    try:
        client = Client(socket_path=f"{root}/sock", autostart=False, connect_timeout_ms=0)
        started = time.monotonic()
        with pytest.raises(ConnectionFailedError):
            client.hello()
        assert time.monotonic() - started < 0.25
        client.close()
    finally:
        shutil.rmtree(root, ignore_errors=True)
