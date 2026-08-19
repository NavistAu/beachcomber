"""Integration tests for the ctypes binding against a real comb daemon.

Exercises the public API end to end through the native library — the wire
protocol itself is the Rust client's concern and is covered by
``libbeachcomber/tests/conformance.rs`` and this SDK's own
``conformance_runner.py``. These tests focus on the binding layer: typed
result construction, exception/kind mapping, sessions, and watch.
"""

from __future__ import annotations

import pytest

from libbeachcomber import Client, ConnectionFailedError, DaemonNotRunning, ServerError
from libbeachcomber.types import IntrospectSubject


def test_put_then_get_hit(client: Client) -> None:
    client.put("widget", {"label": "hello"})
    result = client.get("widget.label")
    assert result.is_hit
    assert result.data == "hello"
    assert result.age_ms >= 0
    assert result.stale is False


def test_get_miss_after_put_none(client: Client) -> None:
    client.put("widget1b", {"v": 1})
    client.put("widget1b", None)  # clears via put(data=None)
    result = client.get("widget1b.v")
    assert not result.is_hit
    assert result.data is None


def test_get_full_provider_object_subscriptable(client: Client) -> None:
    client.put("widget2", {"a": 1, "b": "two"})
    result = client.get("widget2")
    assert result["a"] == 1
    assert result["b"] == "two"


def test_put_null_clears_entry(client: Client) -> None:
    client.put("widget3", {"v": 1})
    assert client.get("widget3.v").is_hit
    client.put("widget3", None)
    assert not client.get("widget3.v").is_hit


def test_refresh_does_not_raise(client: Client) -> None:
    client.put("widget4", {"v": 1})
    client.refresh("widget4")


def test_status_returns_cache_rows(client: Client) -> None:
    client.put("widget5", {"v": 1})
    client.get("widget5.v")
    rows = client.status()
    assert any(r.provider == "widget5" for r in rows)


def test_hello_reports_versions(client: Client) -> None:
    info = client.hello()
    assert info.protocol_version
    assert info.daemon_version


def test_introspect_daemon_subject(client: Client) -> None:
    resp = client.introspect(IntrospectSubject.DAEMON)
    assert resp.daemon is not None
    assert resp.daemon.pid > 0


def test_server_error_carries_kind_attribute(client: Client) -> None:
    with pytest.raises(ServerError) as excinfo:
        client.get("nosuchprovider")
    assert excinfo.value.kind == "server_error"
    assert "unknown provider" in excinfo.value.message


def test_bad_explicit_socket_path_raises_connection_failed(tmp_path) -> None:
    # An explicit socket_path override bypasses discovery/autostart entirely
    # (it's an unconditional address to dial), so a missing socket surfaces
    # as a connection failure, not "daemon not running".
    bad_socket = str(tmp_path / "no-such-socket")
    bad_client = Client(socket_path=bad_socket, timeout=0.2, autostart=False)
    try:
        with pytest.raises(ConnectionFailedError) as excinfo:
            bad_client.get("git.branch")
        assert excinfo.value.kind == "connection_failed"
    finally:
        bad_client.close()


def test_daemon_not_running_raises_with_kind(tmp_path, monkeypatch: pytest.MonkeyPatch) -> None:
    # No explicit socket_path override: the native library's own default
    # discovery runs, finds nothing at $BEACHCOMBER_SOCKET, and with
    # autostart disabled reports daemon_not_running rather than trying to
    # dial (and failing to connect to) a path that was never expected to
    # exist yet.
    monkeypatch.setenv("BEACHCOMBER_SOCKET", str(tmp_path / "no-such-socket"))
    bad_client = Client(timeout=0.2, autostart=False)
    try:
        with pytest.raises(DaemonNotRunning) as excinfo:
            bad_client.get("git.branch")
        assert excinfo.value.kind == "daemon_not_running"
    finally:
        bad_client.close()


def test_resolve_field_expression_override(client: Client) -> None:
    result = client.resolve(
        "widget.label",
        cwd="/tmp",
        overrides={"widget.label": "'resolved-' ~ env.MYVAR"},
        env={"MYVAR": "x"},
    )
    assert result.is_hit
    assert result.data == "resolved-x"


def test_resolve_path_expression(client: Client) -> None:
    result = client.resolve(
        "myproject",
        cwd="/repo-a",
        overrides={"myproject": "'a' if cwd == '/repo-a' else 'b'"},
    )
    assert result.data == "a"


def test_eval_raw_expression(client: Client) -> None:
    value = client.eval("'x-' ~ env.FOO", cwd="/tmp", env={"FOO": "bar"})
    assert value == "x-bar"


def test_session_get_put_set_context(client: Client) -> None:
    with client.session() as session:
        session.put("sesswidget", {"v": 42})
        result = session.get("sesswidget.v")
        assert result.data == 42
        session.set_context("/tmp")


def test_watch_yields_initial_value(client: Client) -> None:
    client.put("watchwidget", {"v": 1})
    with client.watch("watchwidget.v") as stream:
        event = stream.next_event()
        assert event is not None
        assert event.data == 1


def test_watch_timeout_zero_returns_none_when_nothing_new(client: Client) -> None:
    client.put("watchwidget2", {"v": 1})
    with client.watch("watchwidget2.v") as stream:
        first = stream.next_event()
        assert first is not None
        second = stream.next_event(timeout_ms=0)
        assert second is None
