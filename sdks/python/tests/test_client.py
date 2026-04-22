"""Integration tests for Client and Session using the mock daemon."""

from __future__ import annotations

import json
import time
from typing import Any, Dict

import pytest

from libbeachcomber.client import Client
from libbeachcomber.exceptions import DaemonNotRunning, ProtocolError, ServerError
from libbeachcomber.result import CombResult

from .conftest import MockDaemon


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def make_client(daemon: MockDaemon) -> Client:
    """Return a Client pointed at the mock daemon socket."""
    return Client(socket_path=daemon.socket_path, timeout=2.0)


# ---------------------------------------------------------------------------
# Client.get
# ---------------------------------------------------------------------------


class TestClientGet:
    def test_cache_hit_string(self, mock_daemon: MockDaemon) -> None:
        mock_daemon.respond("get", {"ok": True, "data": "main", "age_ms": 50, "stale": False})
        client = make_client(mock_daemon)
        result = client.get("git.branch", path="/repo")
        assert result.is_hit is True
        assert result.data == "main"
        assert result.age_ms == 50
        assert result.stale is False

    def test_cache_hit_dict(self, mock_daemon: MockDaemon) -> None:
        mock_daemon.respond(
            "get",
            {"ok": True, "data": {"branch": "main", "dirty": True}, "age_ms": 10, "stale": False},
        )
        client = make_client(mock_daemon)
        result = client.get("git")
        assert result.is_hit is True
        assert result["branch"] == "main"
        assert result["dirty"] is True

    def test_cache_miss_no_data_field(self, mock_daemon: MockDaemon) -> None:
        mock_daemon.respond("get", {"ok": True})
        client = make_client(mock_daemon)
        result = client.get("git.branch")
        assert result.is_hit is False
        assert result.data is None

    def test_cache_miss_null_data(self, mock_daemon: MockDaemon) -> None:
        mock_daemon.respond("get", {"ok": True, "data": None})
        client = make_client(mock_daemon)
        result = client.get("git.branch")
        assert result.is_hit is False

    def test_server_error_raises(self, mock_daemon: MockDaemon) -> None:
        mock_daemon.respond("get", {"ok": False, "error": "unknown provider: foo"})
        client = make_client(mock_daemon)
        with pytest.raises(ServerError, match="unknown provider"):
            client.get("foo.bar")

    def test_request_contains_key(self, mock_daemon: MockDaemon) -> None:
        mock_daemon.respond("get", {"ok": True})
        client = make_client(mock_daemon)
        client.get("git.branch", path="/repo")
        assert len(mock_daemon.received) == 1
        req = mock_daemon.received[0]
        assert req["op"] == "get"
        assert req["key"] == "git.branch"
        assert req["path"] == "/repo"

    def test_request_omits_path_when_none(self, mock_daemon: MockDaemon) -> None:
        mock_daemon.respond("get", {"ok": True})
        client = make_client(mock_daemon)
        client.get("hostname")
        req = mock_daemon.received[0]
        assert "path" not in req

    def test_stale_flag_propagated(self, mock_daemon: MockDaemon) -> None:
        mock_daemon.respond("get", {"ok": True, "data": "old-branch", "age_ms": 9000, "stale": True})
        client = make_client(mock_daemon)
        result = client.get("git.branch")
        assert result.stale is True

    def test_age_ms_zero_when_missing(self, mock_daemon: MockDaemon) -> None:
        mock_daemon.respond("get", {"ok": True, "data": "v1"})
        client = make_client(mock_daemon)
        result = client.get("version")
        assert result.age_ms == 0


# ---------------------------------------------------------------------------
# Client.refresh
# ---------------------------------------------------------------------------


class TestClientRefresh:
    def test_refresh_sends_correct_request(self, mock_daemon: MockDaemon) -> None:
        mock_daemon.respond("refresh", {"ok": True})
        client = make_client(mock_daemon)
        client.refresh("git", path="/repo")
        req = mock_daemon.received[0]
        assert req["op"] == "refresh"
        assert req["key"] == "git"
        assert req["path"] == "/repo"

    def test_refresh_no_path(self, mock_daemon: MockDaemon) -> None:
        mock_daemon.respond("refresh", {"ok": True})
        client = make_client(mock_daemon)
        client.refresh("hostname")
        assert "path" not in mock_daemon.received[0]

    def test_refresh_server_error_raises(self, mock_daemon: MockDaemon) -> None:
        mock_daemon.respond("refresh", {"ok": False, "error": "no such provider"})
        client = make_client(mock_daemon)
        with pytest.raises(ServerError):
            client.refresh("nonexistent")

    def test_refresh_returns_none(self, mock_daemon: MockDaemon) -> None:
        mock_daemon.respond("refresh", {"ok": True})
        client = make_client(mock_daemon)
        result = client.refresh("git")
        assert result is None


# ---------------------------------------------------------------------------
# Client.list
# ---------------------------------------------------------------------------


class TestClientList:
    def test_list_returns_provider_list(self, git_daemon: MockDaemon) -> None:
        client = make_client(git_daemon)
        providers = client.list()
        assert isinstance(providers, list)
        names = [p["name"] for p in providers]
        assert "git" in names
        assert "hostname" in names

    def test_list_sends_correct_op(self, git_daemon: MockDaemon) -> None:
        client = make_client(git_daemon)
        client.list()
        reqs = [r for r in git_daemon.received if r["op"] == "list"]
        assert len(reqs) == 1

    def test_list_empty_on_no_data(self, mock_daemon: MockDaemon) -> None:
        mock_daemon.respond("list", {"ok": True})
        client = make_client(mock_daemon)
        assert client.list() == []


# ---------------------------------------------------------------------------
# Client.status
# ---------------------------------------------------------------------------


class TestClientStatus:
    def test_status_returns_dict(self, git_daemon: MockDaemon) -> None:
        client = make_client(git_daemon)
        status = client.status()
        assert isinstance(status, dict)
        assert status["scheduler"] == "running"

    def test_status_empty_on_no_data(self, mock_daemon: MockDaemon) -> None:
        mock_daemon.respond("status", {"ok": True})
        client = make_client(mock_daemon)
        assert client.status() == {}


# ---------------------------------------------------------------------------
# Client.session (context manager)
# ---------------------------------------------------------------------------


class TestClientSession:
    def test_session_context_manager(self, mock_daemon: MockDaemon) -> None:
        mock_daemon.respond("context", {"ok": True})
        mock_daemon.respond("get", {"ok": True, "data": "main", "age_ms": 5, "stale": False})
        client = make_client(mock_daemon)
        with client.session() as session:
            session.set_context("/repo")
            result = session.get("git.branch")
        assert result.is_hit is True
        assert result.data == "main"

    def test_session_multiple_gets_on_one_connection(
        self, mock_daemon: MockDaemon
    ) -> None:
        responses = [
            {"ok": True, "data": "main", "age_ms": 1, "stale": False},
            {"ok": True, "data": False, "age_ms": 2, "stale": False},
        ]
        call_count = 0

        def handler(req: Dict[str, Any]) -> Dict[str, Any]:
            nonlocal call_count
            resp = responses[call_count]
            call_count += 1
            return resp

        mock_daemon.on("get", handler)
        client = make_client(mock_daemon)
        with client.session() as session:
            r1 = session.get("git.branch")
            r2 = session.get("git.dirty")
        assert r1.data == "main"
        assert r2.data is False

    def test_session_context_sets_path(self, mock_daemon: MockDaemon) -> None:
        mock_daemon.respond("context", {"ok": True})
        mock_daemon.respond("get", {"ok": True, "data": "feat"})
        client = make_client(mock_daemon)
        with client.session() as session:
            session.set_context("/workspace")
            session.get("git.branch")
        ctx_reqs = [r for r in mock_daemon.received if r["op"] == "context"]
        assert len(ctx_reqs) == 1
        assert ctx_reqs[0]["path"] == "/workspace"

    def test_session_refresh(self, mock_daemon: MockDaemon) -> None:
        mock_daemon.respond("refresh", {"ok": True})
        client = make_client(mock_daemon)
        with client.session() as session:
            session.refresh("git", "/repo")
        req = mock_daemon.received[0]
        assert req["op"] == "refresh"

    def test_session_list(self, git_daemon: MockDaemon) -> None:
        client = make_client(git_daemon)
        with client.session() as session:
            providers = session.list()
        assert any(p["name"] == "git" for p in providers)

    def test_session_status(self, git_daemon: MockDaemon) -> None:
        client = make_client(git_daemon)
        with client.session() as session:
            status = session.status()
        assert "scheduler" in status


# ---------------------------------------------------------------------------
# Error paths
# ---------------------------------------------------------------------------


class TestErrorPaths:
    def test_daemon_not_running_raises(self, tmp_path: pytest.TempPathFixture) -> None:
        client = Client(socket_path=str(tmp_path / "nosock"), timeout=0.5)
        with pytest.raises(DaemonNotRunning):
            client.get("git.branch")

    def test_daemon_not_running_message_contains_path(
        self, tmp_path: pytest.TempPathFixture
    ) -> None:
        sock_path = str(tmp_path / "nosock")
        client = Client(socket_path=sock_path)
        with pytest.raises(DaemonNotRunning) as exc_info:
            client.get("git.branch")
        assert sock_path in str(exc_info.value)

    def test_daemon_not_running_for_refresh(
        self, tmp_path: pytest.TempPathFixture
    ) -> None:
        client = Client(socket_path=str(tmp_path / "nosock"))
        with pytest.raises(DaemonNotRunning):
            client.refresh("git")

    def test_daemon_not_running_for_list(
        self, tmp_path: pytest.TempPathFixture
    ) -> None:
        client = Client(socket_path=str(tmp_path / "nosock"))
        with pytest.raises(DaemonNotRunning):
            client.list()

    def test_daemon_not_running_for_status(
        self, tmp_path: pytest.TempPathFixture
    ) -> None:
        client = Client(socket_path=str(tmp_path / "nosock"))
        with pytest.raises(DaemonNotRunning):
            client.status()

    def test_daemon_not_running_for_session(
        self, tmp_path: pytest.TempPathFixture
    ) -> None:
        client = Client(socket_path=str(tmp_path / "nosock"))
        with pytest.raises(DaemonNotRunning):
            with client.session():
                pass

    def test_malformed_response_raises_protocol_error(
        self, mock_daemon: MockDaemon
    ) -> None:
        # Inject a handler that sends raw non-JSON.
        def bad_handler(req: Dict[str, Any]) -> Dict[str, Any]:
            raise ValueError("override")

        import socket as _socket

        # We bypass the normal handler mechanism and patch the server directly.
        # Instead, register a handler that returns a valid dict but the server
        # will encode it — we need to corrupt it at the transport level.
        # Simulate this by connecting directly and sending garbage.
        client = make_client(mock_daemon)
        # Connect directly to the daemon and write garbage JSON so the daemon
        # echoes it back (the daemon itself validates, so we need the daemon
        # to send back non-JSON). Easier: use a second mock that sends garbage.
        garbage_daemon = MockDaemon()

        def garbage_conn(conn: _socket.socket) -> None:
            conn.sendall(b"this is not json\n")
            conn.close()

        import threading

        garbage_daemon._server_sock = _socket.socket(_socket.AF_UNIX, _socket.SOCK_STREAM)
        garbage_daemon._server_sock.bind(garbage_daemon.socket_path)
        garbage_daemon._server_sock.listen(1)
        garbage_daemon._server_sock.settimeout(2.0)

        def serve_once() -> None:
            try:
                conn, _ = garbage_daemon._server_sock.accept()
                garbage_conn(conn)
            except OSError:
                pass
            finally:
                garbage_daemon._server_sock.close()

        t = threading.Thread(target=serve_once, daemon=True)
        t.start()

        bad_client = Client(socket_path=garbage_daemon.socket_path, timeout=2.0)
        with pytest.raises(ProtocolError):
            bad_client.get("git.branch")

        t.join(timeout=3.0)

    def test_server_error_has_message(self, mock_daemon: MockDaemon) -> None:
        mock_daemon.respond("get", {"ok": False, "error": "provider panic"})
        client = make_client(mock_daemon)
        with pytest.raises(ServerError) as exc_info:
            client.get("git.branch")
        assert exc_info.value.message == "provider panic"
