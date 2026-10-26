"""Integration tests for the ctypes binding against a real comb daemon.

Exercises the public API end to end through the native library — the wire
protocol itself is the Rust client's concern and is covered by
``libbeachcomber/tests/suite/conformance.rs`` and this SDK's own
``conformance_runner.py``. These tests focus on the binding layer: typed
result construction, exception/kind mapping, sessions, and watch.
"""

from __future__ import annotations

import pytest

from libbeachcomber import (
    Client,
    ConnectionFailedError,
    DaemonNotRunningError,
    ParseError,
    ServerError,
)
from libbeachcomber import TimeoutError as CombTimeoutError
from libbeachcomber import _native
from libbeachcomber.types import IntrospectSubject


def test_put_then_get_is_the_value(client: Client) -> None:
    client.put("widget", {"label": "hello"})
    assert client.get("widget.label") == "hello"


def test_get_miss_after_put_none(client: Client) -> None:
    client.put("widget1b", {"v": 1})
    client.put("widget1b", None)  # clears via put(data=None)
    assert client.get("widget1b.v") is None


def test_get_renders_a_template_only_node(client: Client) -> None:
    client.put("tpl1:template", {"t": "{{ 1 }}"})
    assert client.get("tpl1.t") == 1


def test_get_a_template_wins_and_the_facets_read_what_is_stored(client: Client) -> None:
    client.put("tpl2", {"v": 1})
    client.put("tpl2:template", {"v": "{{ 2 }}"})
    assert client.get("tpl2.v") == 2
    assert client.get("tpl2:template.v") == "{{ 2 }}"
    assert client.get("tpl2:put.v") == 1


def test_get_past_a_template_string_walks_into_its_value(client: Client) -> None:
    client.put("tpl3:template", {"t": "{{ {'x': 5} }}"})
    assert client.get("tpl3.t.x") == 5


def test_get_full_provider_object(client: Client) -> None:
    client.put("widget2", {"a": 1, "b": "two"})
    assert client.get("widget2") == {"a": 1, "b": "two"}


def test_put_none_clears_entry(client: Client) -> None:
    client.put("widget3", {"v": 1})
    assert client.get("widget3.v") is not None
    client.put("widget3", None)
    assert client.get("widget3.v") is None


def test_get_takes_force_and_a_wait_duration(client: Client) -> None:
    for wait in ("forever", "0", "50ms"):
        assert client.get("hostname.short", force=True, wait=wait) is not None
        with client.session() as session:
            assert session.get("hostname.short", wait=wait) is not None


def test_a_wait_that_is_no_duration_is_a_parse_error(client: Client) -> None:
    # The library reads the wait, so its refusal is the same on every SDK.
    with pytest.raises(ParseError) as excinfo:
        client.get("hostname.short", wait="soon")
    assert "invalid wait `soon`" in excinfo.value.message
    with client.session() as session, pytest.raises(ParseError):
        session.get("hostname.short", wait=True)  # type: ignore[arg-type]


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
    bad_client = Client(socket_path=bad_socket, timeout_ms=200, autostart=False)
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
    bad_client = Client(timeout_ms=200, autostart=False)
    try:
        with pytest.raises(DaemonNotRunningError) as excinfo:
            bad_client.get("git.branch")
        assert excinfo.value.kind == "daemon_not_running"
    finally:
        bad_client.close()


def test_get_renders_with_the_callers_environment(client: Client) -> None:
    # The daemon holds the template; the client renders it with the env the
    # caller passes — the same value `comb get widget.label` prints.
    client.put("widget:template", {"label": "{{ 'resolved-' ~ env.MYVAR }}"})
    assert client.get("widget.label", cwd="/tmp", env={"MYVAR": "x"}) == "resolved-x"


def test_get_bare_provider_is_the_merged_namespace(client: Client) -> None:
    client.put("widget6", {"lit": 1})
    client.put("widget6:template", {"t": "{{ env.W6 }}"})
    assert client.get("widget6", cwd="/tmp", env={"W6": "hi"}) == {"lit": 1, "t": "hi"}


def test_get_reads_at_cwd(client: Client) -> None:
    client.put("widget7", {"v": "global"})
    client.put("widget7", {"v": "tmp"}, path="/tmp")
    assert client.get("widget7.v") == "global"
    assert client.get("widget7.v", cwd="/tmp") == "tmp"


def test_eval_raw_expression(client: Client) -> None:
    value = client.eval("{{ 'x-' ~ env.FOO }}", cwd="/tmp", env={"FOO": "bar"})
    assert value == "x-bar"


def test_exec_is_disabled_without_the_option(client: Client) -> None:
    client.put("execwidget:template", {"out": "{{ exec(['printf', 'x']) }}"})
    with pytest.raises(ServerError) as excinfo:
        client.get("execwidget.out", cwd="/tmp")
    assert "exec() is disabled in this context" in excinfo.value.message


def test_exec_runs_on_a_client_with_the_option(client: Client, daemon) -> None:
    client.put("execwidget2:template", {"out": "{{ exec(['printf', 'x']) }}"})
    exec_client = Client(socket_path=daemon.socket_path, timeout_ms=5000, exec=True)
    try:
        assert exec_client.get("execwidget2.out", cwd="/tmp") == "x"
    finally:
        exec_client.close()


def test_session_get_put_set_context(client: Client) -> None:
    with client.session() as session:
        session.put("sesswidget", {"v": 42})
        assert session.get("sesswidget.v") == 42
        session.put("sesswidget", {"v": 43}, path="/tmp")
        session.set_context("/tmp")
        assert session.get("sesswidget.v") == 43


def test_session_get_renders_with_the_callers_environment(client: Client) -> None:
    client.put("sesstpl:template", {"t": "{{ env.WHO }}"})
    with client.session() as session:
        assert session.get("sesstpl.t", env={"WHO": "me"}) == "me"


def test_watch_yields_initial_value(client: Client) -> None:
    client.put("watchwidget", {"v": 1})
    with client.watch("watchwidget.v", cwd="/tmp") as stream:
        event = stream.next_event()
        assert event is not None
        assert event.data == 1


def test_a_watch_timeout_is_the_timeout_error(client: Client) -> None:
    client.put("watchwidget2", {"v": 1})
    with client.watch("watchwidget2.v", cwd="/tmp") as stream:
        first = stream.next_event()
        assert first is not None
        with pytest.raises(CombTimeoutError) as excinfo:
            stream.next_event(timeout_ms=0)
        assert excinfo.value.kind == "timeout"


def test_a_cancelled_watch_has_ended(client: Client) -> None:
    client.put("watchwidget3", {"v": 1})
    with client.watch("watchwidget3.v", cwd="/tmp") as stream:
        assert stream.next_event() is not None
        stream.cancel()
        assert stream.next_event() is None
        assert list(stream) == []


def test_watch_without_a_cwd_watches_the_global_entry(client: Client) -> None:
    client.put("watchglobal", {"v": "global"})
    client.put("watchglobal", {"v": "tmp"}, path="/tmp")
    with client.watch("watchglobal.v") as stream:
        event = stream.next_event()
        assert event is not None
        assert event.data == "global"


def test_eval_without_a_cwd_reads_the_global_entry(client: Client) -> None:
    client.put("evalglobal", {"v": "global"})
    client.put("evalglobal", {"v": "tmp"}, path="/tmp")
    assert client.eval("{{ evalglobal.v }}") == "global"
    assert client.eval("{{ evalglobal.v }}", cwd="/tmp") == "tmp"


def test_an_errors_message_is_the_abis_alone(client: Client) -> None:
    with pytest.raises(ServerError) as excinfo:
        client.get("a:b:c.x")
    err = excinfo.value
    assert str(err) == err.message
    assert "version" not in err.message
    assert err.library_version == _native.get_lib().version


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
        assert first.data == client.get("watchtpl.f", cwd="/tmp", env=env)

        client.put("watchtpl:template", {"f": "{{ env.WATCH_TPL }}!"})
        second = stream.next_event(timeout_ms=5000)
        assert second is not None
        assert second.data == "v1!"
