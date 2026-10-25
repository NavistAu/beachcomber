"""Integration tests for the ctypes binding against a real comb daemon.

Exercises the public API end to end through the native library — the wire
protocol itself is the Rust client's concern and is covered by
``libbeachcomber/tests/suite/conformance.rs`` and this SDK's own
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


def test_get_template_only_node_is_neither_hit_nor_miss(client: Client) -> None:
    client.put("tpl1:template", {"t": "{{ 1 }}"})
    result = client.get("tpl1.t")
    assert result.template == "{{ 1 }}"
    assert result.data is None
    assert not result.is_hit
    assert not result.is_miss


def test_get_carries_both_halves(client: Client) -> None:
    client.put("tpl2", {"v": 1})
    client.put("tpl2:template", {"v": "{{ 2 }}"})
    result = client.get("tpl2.v")
    assert result.is_hit
    assert result.data == 1
    assert result.template == "{{ 2 }}"
    assert result.rest is None


def test_get_past_a_template_string_sets_rest(client: Client) -> None:
    client.put("tpl3:template", {"t": "{{ 1 }}"})
    result = client.get("tpl3.t.x")
    assert result.template == "{{ 1 }}"
    assert result.rest == "x"


def test_get_full_provider_object_subscriptable(client: Client) -> None:
    client.put("widget2", {"a": 1, "b": "two"})
    result = client.get("widget2")
    assert result["a"] == 1
    assert result["b"] == "two"


def test_put_none_clears_entry(client: Client) -> None:
    client.put("widget3", {"v": 1})
    assert client.get("widget3.v").is_hit
    client.put("widget3", None)
    assert not client.get("widget3.v").is_hit


def test_refresh_of_a_provider_does_not_raise(client: Client) -> None:
    client.refresh("hostname")


def test_refresh_of_a_put_namespace_raises(client: Client) -> None:
    # A put-created namespace has no Source to re-execute, so the daemon
    # refuses rather than answering ok and doing nothing.
    client.put("widget4", {"v": 1})
    with pytest.raises(ServerError) as excinfo:
        client.refresh("widget4")
    # The daemon's words alone, as `comb refresh` prints them — no
    # `server error: ` prefix from the envelope.
    assert excinfo.value.message.startswith("nothing to refresh: widget4 has no sources")


def test_status_returns_cache_rows(client: Client) -> None:
    client.put("widget5", {"v": 1})
    client.get("widget5.v")
    rows = client.status()
    assert any(r.provider == "widget5" for r in rows)


def test_a_status_row_says_when_it_polls_next(client: Client) -> None:
    client.put("widget5ttl", {"v": 1}, ttl="1m")
    row = next(r for r in client.status() if r.provider == "widget5ttl")
    assert isinstance(row.next_poll_in_secs, int)


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
        client.get("a:b:c.x")
    assert excinfo.value.kind == "server_error"
    assert "invalid facet key" in excinfo.value.message


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


def test_resolve_renders_a_put_template(client: Client) -> None:
    # The daemon holds the template; the client renders it with the env the
    # caller passes — the same value `comb get widget.label` prints.
    client.put("widget:template", {"label": "{{ 'resolved-' ~ env.MYVAR }}"})
    result = client.resolve("widget.label", cwd="/tmp", env={"MYVAR": "x"})
    assert result.is_hit
    assert result.data == "resolved-x"


def test_resolve_bare_provider_is_the_merged_namespace(client: Client) -> None:
    client.put("widget6", {"lit": 1})
    client.put("widget6:template", {"t": "{{ env.W6 }}"})
    result = client.resolve("widget6", cwd="/tmp", env={"W6": "hi"})
    assert result.data == {"lit": 1, "t": "hi"}


def test_resolve_miss_is_none(client: Client) -> None:
    client.put("widget7", {"v": 1})
    assert client.resolve("widget7.nope", cwd="/tmp").data is None


def test_eval_raw_expression(client: Client) -> None:
    value = client.eval("{{ 'x-' ~ env.FOO }}", cwd="/tmp", env={"FOO": "bar"})
    assert value == "x-bar"


def test_exec_is_disabled_without_the_option(client: Client) -> None:
    client.put("execwidget:template", {"out": "{{ exec(['printf', 'x']) }}"})
    with pytest.raises(ServerError) as excinfo:
        client.resolve("execwidget.out", cwd="/tmp")
    assert "exec() is disabled in this context" in excinfo.value.message


def test_exec_runs_on_a_client_with_the_option(client: Client, daemon) -> None:
    client.put("execwidget2:template", {"out": "{{ exec(['printf', 'x']) }}"})
    exec_client = Client(socket_path=daemon.socket_path, timeout=5.0, exec=True)
    try:
        assert exec_client.resolve("execwidget2.out", cwd="/tmp").data == "x"
    finally:
        exec_client.close()


def test_session_get_put_set_context(client: Client) -> None:
    with client.session() as session:
        session.put("sesswidget", {"v": 42})
        result = session.get("sesswidget.v")
        assert result.data == 42
        session.set_context("/tmp")


def test_watch_yields_initial_value(client: Client) -> None:
    client.put("watchwidget", {"v": 1})
    with client.watch("watchwidget.v", cwd="/tmp") as stream:
        event = stream.next_event()
        assert event is not None
        assert event.data == 1


def test_watch_timeout_zero_returns_none_when_nothing_new(client: Client) -> None:
    client.put("watchwidget2", {"v": 1})
    with client.watch("watchwidget2.v", cwd="/tmp") as stream:
        first = stream.next_event()
        assert first is not None
        second = stream.next_event(timeout_ms=0)
        assert second is None


def test_watch_yields_the_resolved_value_and_refires_on_a_template_edit(
    client: Client,
) -> None:
    """The event is what ``comb get`` would print, and the template the value
    is rendered from is watched alongside the addressed key."""
    client.put("watchtpl:template", {"f": "{{ env.WATCH_TPL }}"})
    env = {"WATCH_TPL": "v1"}
    with client.watch("watchtpl.f", cwd="/tmp", env=env) as stream:
        first = stream.next_event()
        assert first is not None
        assert first.data == "v1"
        assert first.data == client.resolve("watchtpl.f", cwd="/tmp", env=env).data

        client.put("watchtpl:template", {"f": "{{ env.WATCH_TPL }}!"})
        second = stream.next_event(timeout_ms=5000)
        assert second is not None
        assert second.data == "v1!"
