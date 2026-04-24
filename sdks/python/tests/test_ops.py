"""Tests for the new protocol operations: hello, put, introspect, watch, status, get_with_flags."""

from __future__ import annotations

import threading
from typing import Any, Dict

import pytest

from libbeachcomber.client import Client, WatchStream
from libbeachcomber.exceptions import ProtocolError, ServerError
from libbeachcomber.result import CombResult
from libbeachcomber.types import (
    CacheRow,
    DaemonHealth,
    HelloInfo,
    IntrospectResponse,
    IntrospectSubject,
    WatchEvent,
)

from .conftest import MockDaemon


def make_client(daemon: MockDaemon) -> Client:
    """Return a Client pointed at the mock daemon socket."""
    return Client(socket_path=daemon.socket_path, timeout=2.0)


# ---------------------------------------------------------------------------
# Client.hello
# ---------------------------------------------------------------------------


class TestClientHello:
    def test_hello_returns_hello_info(self, mock_daemon: MockDaemon) -> None:
        mock_daemon.respond(
            "hello",
            {
                "ok": True,
                "data": {"protocol_version": "1", "daemon_version": "0.5.0"},
            },
        )
        client = make_client(mock_daemon)
        info = client.hello()
        assert isinstance(info, HelloInfo)
        assert info.protocol_version == "1"
        assert info.daemon_version == "0.5.0"

    def test_hello_sends_correct_op(self, mock_daemon: MockDaemon) -> None:
        mock_daemon.respond(
            "hello",
            {"ok": True, "data": {"protocol_version": "1", "daemon_version": "0.1.0"}},
        )
        client = make_client(mock_daemon)
        client.hello()
        assert len(mock_daemon.received) == 1
        assert mock_daemon.received[0]["op"] == "hello"

    def test_hello_empty_data_defaults_to_empty_strings(
        self, mock_daemon: MockDaemon
    ) -> None:
        mock_daemon.respond("hello", {"ok": True})
        client = make_client(mock_daemon)
        info = client.hello()
        assert info.protocol_version == ""
        assert info.daemon_version == ""

    def test_hello_server_error_raises(self, mock_daemon: MockDaemon) -> None:
        mock_daemon.respond("hello", {"ok": False, "error": "not supported"})
        client = make_client(mock_daemon)
        with pytest.raises(ServerError, match="not supported"):
            client.hello()

    def test_session_hello(self, mock_daemon: MockDaemon) -> None:
        mock_daemon.respond(
            "hello",
            {"ok": True, "data": {"protocol_version": "2", "daemon_version": "1.0.0"}},
        )
        client = make_client(mock_daemon)
        with client.session() as session:
            info = session.hello()
        assert info.protocol_version == "2"
        assert info.daemon_version == "1.0.0"


# ---------------------------------------------------------------------------
# Client.put
# ---------------------------------------------------------------------------


class TestClientPut:
    def test_put_sends_correct_op(self, mock_daemon: MockDaemon) -> None:
        mock_daemon.respond("put", {"ok": True})
        client = make_client(mock_daemon)
        client.put("mykey", data={"x": 1})
        req = mock_daemon.received[0]
        assert req["op"] == "put"
        assert req["key"] == "mykey"
        assert req["data"] == {"x": 1}

    def test_put_returns_none(self, mock_daemon: MockDaemon) -> None:
        mock_daemon.respond("put", {"ok": True})
        client = make_client(mock_daemon)
        result = client.put("mykey", data="hello")
        assert result is None

    def test_put_omits_data_when_none(self, mock_daemon: MockDaemon) -> None:
        mock_daemon.respond("put", {"ok": True})
        client = make_client(mock_daemon)
        client.put("mykey")
        req = mock_daemon.received[0]
        assert "data" not in req

    def test_put_includes_ttl(self, mock_daemon: MockDaemon) -> None:
        mock_daemon.respond("put", {"ok": True})
        client = make_client(mock_daemon)
        client.put("mykey", data="val", ttl="30s")
        req = mock_daemon.received[0]
        assert req["ttl"] == "30s"

    def test_put_includes_path(self, mock_daemon: MockDaemon) -> None:
        mock_daemon.respond("put", {"ok": True})
        client = make_client(mock_daemon)
        client.put("mykey", data="val", path="/some/dir")
        req = mock_daemon.received[0]
        assert req["path"] == "/some/dir"

    def test_put_server_error_raises(self, mock_daemon: MockDaemon) -> None:
        mock_daemon.respond("put", {"ok": False, "error": "invalid key"})
        client = make_client(mock_daemon)
        with pytest.raises(ServerError, match="invalid key"):
            client.put("mykey", data=42)

    def test_session_put(self, mock_daemon: MockDaemon) -> None:
        mock_daemon.respond("put", {"ok": True})
        client = make_client(mock_daemon)
        with client.session() as session:
            session.put("k", data={"a": "b"})
        req = mock_daemon.received[0]
        assert req["op"] == "put"
        assert req["key"] == "k"


# ---------------------------------------------------------------------------
# Client.introspect
# ---------------------------------------------------------------------------


class TestClientIntrospect:
    def test_introspect_daemon_returns_daemon_health(
        self, mock_daemon: MockDaemon
    ) -> None:
        mock_daemon.respond(
            "introspect",
            {
                "ok": True,
                "data": {
                    "pid": 12345,
                    "version": "0.5.0",
                    "uptime_secs": 100,
                    "socket_path": "/tmp/comb.sock",
                    "config_path": None,
                    "requests_total": 42,
                    "in_flight": 0,
                    "active_watchers": 1,
                    "cache_entries": 10,
                    "verdicts": [],
                },
            },
        )
        client = make_client(mock_daemon)
        resp = client.introspect(IntrospectSubject.DAEMON)
        assert isinstance(resp, IntrospectResponse)
        assert resp.subject == IntrospectSubject.DAEMON
        assert isinstance(resp.daemon, DaemonHealth)
        assert resp.daemon.pid == 12345
        assert resp.daemon.version == "0.5.0"
        assert resp.daemon.uptime_secs == 100
        assert resp.daemon.socket_path == "/tmp/comb.sock"
        assert resp.daemon.requests_total == 42
        assert resp.daemon.in_flight == 0
        assert resp.daemon.active_watchers == 1
        assert resp.daemon.cache_entries == 10
        assert resp.other is None

    def test_introspect_non_daemon_returns_other(
        self, mock_daemon: MockDaemon
    ) -> None:
        mock_daemon.respond(
            "introspect",
            {"ok": True, "data": [{"name": "git", "ttl": "5s"}]},
        )
        client = make_client(mock_daemon)
        resp = client.introspect(IntrospectSubject.PROVIDERS)
        assert resp.subject == IntrospectSubject.PROVIDERS
        assert resp.daemon is None
        assert isinstance(resp.other, list)

    def test_introspect_sends_subject(self, mock_daemon: MockDaemon) -> None:
        mock_daemon.respond("introspect", {"ok": True, "data": {}})
        client = make_client(mock_daemon)
        client.introspect(IntrospectSubject.CACHE)
        req = mock_daemon.received[0]
        assert req["op"] == "introspect"
        assert req["subject"] == "cache"

    def test_introspect_sends_duration_secs(self, mock_daemon: MockDaemon) -> None:
        mock_daemon.respond("introspect", {"ok": True, "data": {}})
        client = make_client(mock_daemon)
        client.introspect(IntrospectSubject.PROCS, duration_secs=5)
        req = mock_daemon.received[0]
        assert req["duration_secs"] == 5

    def test_introspect_omits_duration_when_none(
        self, mock_daemon: MockDaemon
    ) -> None:
        mock_daemon.respond("introspect", {"ok": True, "data": {}})
        client = make_client(mock_daemon)
        client.introspect(IntrospectSubject.LIFECYCLE)
        req = mock_daemon.received[0]
        assert "duration_secs" not in req

    def test_introspect_daemon_with_verdicts(self, mock_daemon: MockDaemon) -> None:
        mock_daemon.respond(
            "introspect",
            {
                "ok": True,
                "data": {
                    "pid": 1,
                    "version": "0.1.0",
                    "uptime_secs": 0,
                    "socket_path": "",
                    "config_path": None,
                    "requests_total": 0,
                    "in_flight": 0,
                    "active_watchers": 0,
                    "cache_entries": 0,
                    "verdicts": [
                        {"level": "warn", "message": "provider slow"},
                        {"level": "ok", "message": "all good"},
                    ],
                },
            },
        )
        client = make_client(mock_daemon)
        resp = client.introspect(IntrospectSubject.DAEMON)
        assert resp.daemon is not None
        assert len(resp.daemon.verdicts) == 2
        assert resp.daemon.verdicts[0].level == "warn"
        assert resp.daemon.verdicts[0].message == "provider slow"

    def test_session_introspect(self, mock_daemon: MockDaemon) -> None:
        mock_daemon.respond(
            "introspect",
            {"ok": True, "data": {"items": []}},
        )
        client = make_client(mock_daemon)
        with client.session() as session:
            resp = session.introspect(IntrospectSubject.WATCHES)
        assert resp.subject == IntrospectSubject.WATCHES
        assert resp.other == {"items": []}


# ---------------------------------------------------------------------------
# Client.status
# ---------------------------------------------------------------------------


class TestClientStatus:
    def test_status_returns_cache_rows(self, mock_daemon: MockDaemon) -> None:
        mock_daemon.respond(
            "status",
            {
                "ok": True,
                "data": [
                    {
                        "provider": "git",
                        "field": "branch",
                        "path": "/repo",
                        "value": "main",
                        "age_ms": 100,
                        "stale": False,
                    }
                ],
            },
        )
        client = make_client(mock_daemon)
        rows = client.status()
        assert isinstance(rows, list)
        assert len(rows) == 1
        row = rows[0]
        assert isinstance(row, CacheRow)
        assert row.provider == "git"
        assert row.field == "branch"
        assert row.path == "/repo"
        assert row.value == "main"
        assert row.age_ms == 100
        assert row.stale is False

    def test_status_empty_list(self, mock_daemon: MockDaemon) -> None:
        mock_daemon.respond("status", {"ok": True, "data": []})
        client = make_client(mock_daemon)
        rows = client.status()
        assert rows == []

    def test_status_non_array_raises_protocol_error(
        self, mock_daemon: MockDaemon
    ) -> None:
        mock_daemon.respond("status", {"ok": True, "data": {"not": "an array"}})
        client = make_client(mock_daemon)
        with pytest.raises(ProtocolError, match="not an array"):
            client.status()

    def test_status_skips_non_dict_entries(
        self, mock_daemon: MockDaemon
    ) -> None:
        mock_daemon.respond(
            "status",
            {
                "ok": True,
                "data": [
                    "not a dict",
                    {"provider": "hostname", "field": None, "path": None, "value": "myhost", "age_ms": 5, "stale": False},
                ],
            },
        )
        client = make_client(mock_daemon)
        rows = client.status()
        assert len(rows) == 1
        assert rows[0].provider == "hostname"

    def test_session_status(self, mock_daemon: MockDaemon) -> None:
        mock_daemon.respond(
            "status",
            {
                "ok": True,
                "data": [
                    {
                        "provider": "uptime",
                        "field": None,
                        "path": None,
                        "value": 99,
                        "age_ms": 50,
                        "stale": False,
                    }
                ],
            },
        )
        client = make_client(mock_daemon)
        with client.session() as session:
            rows = session.status()
        assert len(rows) == 1
        assert rows[0].provider == "uptime"

    def test_status_row_exposes_lifecycle_fields(self, mock_daemon: MockDaemon) -> None:
        """Status rows for active providers carry RowKind=Lifecycle plus poll_interval/keep_alive/fsevents fields."""
        mock_daemon.respond(
            "status",
            {
                "ok": True,
                "data": [
                    {
                        "provider": "git",
                        "field": "branch",
                        "path": "/repo",
                        "value": "main",
                        "age_ms": 100,
                        "stale": False,
                        "kind": {"kind": "lifecycle", "decay": 0, "watches_files": True},
                        "poll_interval_secs": 30,
                        "keep_alive_polls": 5,
                        "fsevents_reinstate": False,
                        "failure": None,
                    }
                ],
            },
        )
        client = make_client(mock_daemon)
        rows = client.status()
        git = next(r for r in rows if r.provider == "git")
        assert git.kind is not None, "git row should have a kind"
        assert git.kind["kind"] == "lifecycle", f"expected lifecycle kind, got {git.kind!r}"
        assert isinstance(git.kind["decay"], int)
        assert git.poll_interval_secs is not None and git.poll_interval_secs > 0
        assert git.keep_alive_polls is not None and git.keep_alive_polls > 0
        assert git.fsevents_reinstate is not None

    def test_status_row_lifecycle_failure_field(self, mock_daemon: MockDaemon) -> None:
        """Status rows surface the failure dict when present."""
        mock_daemon.respond(
            "status",
            {
                "ok": True,
                "data": [
                    {
                        "provider": "git",
                        "field": "branch",
                        "path": "/repo",
                        "value": None,
                        "age_ms": 0,
                        "stale": True,
                        "kind": {"kind": "lifecycle", "decay": 2, "watches_files": False},
                        "poll_interval_secs": 30,
                        "keep_alive_polls": 5,
                        "fsevents_reinstate": True,
                        "failure": {"consecutive_failures": 3, "suppressed_until_unix_ms": 99999},
                    }
                ],
            },
        )
        client = make_client(mock_daemon)
        rows = client.status()
        git = next(r for r in rows if r.provider == "git")
        assert git.failure is not None
        assert git.failure["consecutive_failures"] == 3
        assert git.failure["suppressed_until_unix_ms"] == 99999

    def test_status_row_lifecycle_fields_absent_when_not_sent(
        self, mock_daemon: MockDaemon
    ) -> None:
        """Rows without lifecycle fields default all new fields to None."""
        mock_daemon.respond(
            "status",
            {
                "ok": True,
                "data": [
                    {
                        "provider": "hostname",
                        "field": None,
                        "path": None,
                        "value": "myhost",
                        "age_ms": 5,
                        "stale": False,
                    }
                ],
            },
        )
        client = make_client(mock_daemon)
        rows = client.status()
        row = rows[0]
        assert row.kind is None
        assert row.poll_interval_secs is None
        assert row.keep_alive_polls is None
        assert row.fsevents_reinstate is None
        assert row.failure is None


# ---------------------------------------------------------------------------
# Client.get_with_flags
# ---------------------------------------------------------------------------


class TestClientGetWithFlags:
    def test_get_with_flags_no_flags(self, mock_daemon: MockDaemon) -> None:
        mock_daemon.respond(
            "get", {"ok": True, "data": "main", "age_ms": 10, "stale": False}
        )
        client = make_client(mock_daemon)
        result = client.get_with_flags("git.branch")
        assert isinstance(result, CombResult)
        assert result.data == "main"
        req = mock_daemon.received[0]
        assert "force" not in req
        assert "wait" not in req

    def test_get_with_flags_force_true(self, mock_daemon: MockDaemon) -> None:
        mock_daemon.respond("get", {"ok": True, "data": "feat", "age_ms": 0, "stale": False})
        client = make_client(mock_daemon)
        client.get_with_flags("git.branch", force=True)
        req = mock_daemon.received[0]
        assert req["force"] is True
        assert "wait" not in req

    def test_get_with_flags_wait_true(self, mock_daemon: MockDaemon) -> None:
        mock_daemon.respond("get", {"ok": True, "data": "v2", "age_ms": 0, "stale": False})
        client = make_client(mock_daemon)
        client.get_with_flags("git.branch", wait=True)
        req = mock_daemon.received[0]
        assert req["wait"] is True
        assert "force" not in req

    def test_get_with_flags_force_and_wait(self, mock_daemon: MockDaemon) -> None:
        mock_daemon.respond("get", {"ok": True, "data": "v3", "age_ms": 0, "stale": False})
        client = make_client(mock_daemon)
        client.get_with_flags("git.branch", force=True, wait=True)
        req = mock_daemon.received[0]
        assert req["force"] is True
        assert req["wait"] is True

    def test_get_with_flags_includes_path(self, mock_daemon: MockDaemon) -> None:
        mock_daemon.respond("get", {"ok": True, "data": "dev"})
        client = make_client(mock_daemon)
        client.get_with_flags("git.branch", path="/myrepo", force=True)
        req = mock_daemon.received[0]
        assert req["path"] == "/myrepo"
        assert req["force"] is True

    def test_session_get_with_flags(self, mock_daemon: MockDaemon) -> None:
        mock_daemon.respond("get", {"ok": True, "data": "x"})
        client = make_client(mock_daemon)
        with client.session() as session:
            result = session.get_with_flags("k", force=True)
        assert result.data == "x"
        req = mock_daemon.received[0]
        assert req["force"] is True


# ---------------------------------------------------------------------------
# WatchStream
# ---------------------------------------------------------------------------


class TestWatchStream:
    def test_watch_sends_correct_request(self, mock_daemon: MockDaemon) -> None:
        """Watch sends correct op and key; stream is then iterable."""
        events_sent = []

        def watch_handler(conn: Any) -> None:
            import json
            import socket as _socket

            # Read the watch request.
            reader = conn.makefile("r", encoding="utf-8")
            line = reader.readline()
            try:
                req = json.loads(line.strip())
                events_sent.append(req)
            except Exception:
                pass
            reader.detach()

            # Send one event then close.
            event = json.dumps({"ok": True, "data": "hello", "age_ms": 5, "stale": False}) + "\n"
            conn.sendall(event.encode())
            conn.close()

        # Override _handle_conn to not loop — just handle once for watch.
        mock_daemon.respond(
            "watch",
            {"ok": True, "data": "hello", "age_ms": 5, "stale": False},
        )

        client = make_client(mock_daemon)
        stream = client.watch("git.branch", path="/repo")
        try:
            event = stream.next_event()
        finally:
            stream.close()

        req = mock_daemon.received[0]
        assert req["op"] == "watch"
        assert req["key"] == "git.branch"
        assert req["path"] == "/repo"

    def test_watch_yields_watch_event(self, mock_daemon: MockDaemon) -> None:
        mock_daemon.respond(
            "watch",
            {"ok": True, "data": 42, "age_ms": 100, "stale": True},
        )
        client = make_client(mock_daemon)
        stream = client.watch("mykey")
        try:
            event = stream.next_event()
        finally:
            stream.close()
        assert isinstance(event, WatchEvent)
        assert event.data == 42
        assert event.age_ms == 100
        assert event.stale is True

    def test_watch_context_manager(self, mock_daemon: MockDaemon) -> None:
        mock_daemon.respond(
            "watch",
            {"ok": True, "data": "value", "age_ms": 10, "stale": False},
        )
        client = make_client(mock_daemon)
        with client.watch("mykey") as stream:
            event = stream.next_event()
        assert event is not None
        assert event.data == "value"

    def test_watch_omits_path_when_none(self, mock_daemon: MockDaemon) -> None:
        mock_daemon.respond(
            "watch",
            {"ok": True, "data": "x", "age_ms": 0, "stale": False},
        )
        client = make_client(mock_daemon)
        with client.watch("hostname") as stream:
            stream.next_event()
        req = mock_daemon.received[0]
        assert "path" not in req

    def test_watch_stream_is_iterable(self, mock_daemon: MockDaemon) -> None:
        """WatchStream is a valid Python iterator."""
        stream = client = None
        call_count = 0

        def handler(req: Dict[str, Any]) -> Dict[str, Any]:
            nonlocal call_count
            call_count += 1
            if call_count == 1:
                return {"ok": True, "data": call_count, "age_ms": 0, "stale": False}
            # After first response, closing the mock connection from the
            # daemon side is complex; instead just verify __iter__ protocol.
            return {"ok": True, "data": call_count, "age_ms": 0, "stale": False}

        mock_daemon.on("watch", handler)
        client = make_client(mock_daemon)
        stream = client.watch("k")
        try:
            assert iter(stream) is stream
            event = next(stream)
            assert isinstance(event, WatchEvent)
            assert event.data == 1
        finally:
            stream.close()


# ---------------------------------------------------------------------------
# Protocol: build_get_request with flags
# ---------------------------------------------------------------------------


class TestBuildGetRequestFlags:
    def test_force_flag_included_in_request(self, mock_daemon: MockDaemon) -> None:
        from libbeachcomber.protocol import build_get_request
        import json

        req_bytes = build_get_request("git.branch", force=True)
        req = json.loads(req_bytes.decode().strip())
        assert req["force"] is True
        assert "wait" not in req

    def test_wait_flag_included_in_request(self) -> None:
        from libbeachcomber.protocol import build_get_request
        import json

        req_bytes = build_get_request("git.branch", wait=True)
        req = json.loads(req_bytes.decode().strip())
        assert req["wait"] is True
        assert "force" not in req

    def test_both_flags_false_omitted(self) -> None:
        from libbeachcomber.protocol import build_get_request
        import json

        req_bytes = build_get_request("git.branch")
        req = json.loads(req_bytes.decode().strip())
        assert "force" not in req
        assert "wait" not in req

    def test_backwards_compatible_positional_args(self) -> None:
        """Existing callers passing (key, path) positionally still work."""
        from libbeachcomber.protocol import build_get_request
        import json

        req_bytes = build_get_request("git.branch", "/myrepo")
        req = json.loads(req_bytes.decode().strip())
        assert req["key"] == "git.branch"
        assert req["path"] == "/myrepo"
        assert "force" not in req
        assert "wait" not in req
