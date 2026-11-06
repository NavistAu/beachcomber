"""A closed client or session is an ``IoError`` on any later call, raised
before the library is reached (docs/sdk-api.md: a closed handle)."""

from __future__ import annotations

import pytest

from libbeachcomber import Client, IoError


def test_a_closed_client_is_an_io_error() -> None:
    client = Client(socket_path="/tmp/bc-closed.sock", autostart=False)
    client.close()
    with pytest.raises(IoError, match="the client is closed"):
        client.hello()
    with pytest.raises(IoError, match="the client is closed"):
        client.get("p.f")


def test_a_closed_session_is_an_io_error() -> None:
    client = Client(socket_path="/tmp/bc-closed.sock", autostart=False)
    try:
        with client.session() as session:
            pass
        with pytest.raises(IoError, match="the session is closed"):
            session.get("p.f")
    finally:
        client.close()
