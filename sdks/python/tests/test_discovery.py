"""Unit tests for socket path discovery."""

from __future__ import annotations

import os
import tempfile

import pytest

from beachcomber.discovery import discover_socket_path, get_uid


class TestGetUid:
    def test_returns_int(self) -> None:
        uid = get_uid()
        assert isinstance(uid, int)
        assert uid >= 0

    def test_matches_os_geteuid(self) -> None:
        assert get_uid() == os.geteuid()


class TestDiscoverSocketPath:
    def test_falls_back_to_tmpdir(self, monkeypatch: pytest.MonkeyPatch) -> None:
        monkeypatch.delenv("XDG_RUNTIME_DIR", raising=False)
        monkeypatch.setenv("TMPDIR", "/tmp")
        uid = get_uid()
        path = discover_socket_path()
        assert path == f"/tmp/beachcomber-{uid}/sock"

    def test_uses_custom_tmpdir(self, monkeypatch: pytest.MonkeyPatch) -> None:
        monkeypatch.delenv("XDG_RUNTIME_DIR", raising=False)
        monkeypatch.setenv("TMPDIR", "/var/run/user/tmp")
        uid = get_uid()
        path = discover_socket_path()
        assert path == f"/var/run/user/tmp/beachcomber-{uid}/sock"

    def test_strips_trailing_slash_from_tmpdir(
        self, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        monkeypatch.delenv("XDG_RUNTIME_DIR", raising=False)
        monkeypatch.setenv("TMPDIR", "/tmp/")
        uid = get_uid()
        path = discover_socket_path()
        assert path == f"/tmp/beachcomber-{uid}/sock"

    def test_uses_xdg_runtime_dir_when_socket_exists(
        self, monkeypatch: pytest.MonkeyPatch, tmp_path: pytest.TempPathFixture
    ) -> None:
        xdg = tmp_path / "run"
        sock_dir = xdg / "beachcomber"
        sock_dir.mkdir(parents=True)
        (sock_dir / "sock").touch()

        monkeypatch.setenv("XDG_RUNTIME_DIR", str(xdg))
        path = discover_socket_path()
        assert path == str(sock_dir / "sock")

    def test_ignores_xdg_runtime_dir_when_socket_missing(
        self, monkeypatch: pytest.MonkeyPatch, tmp_path: pytest.TempPathFixture
    ) -> None:
        xdg = tmp_path / "run"
        xdg.mkdir()
        # No sock file created.

        monkeypatch.setenv("XDG_RUNTIME_DIR", str(xdg))
        monkeypatch.setenv("TMPDIR", "/tmp")
        uid = get_uid()
        path = discover_socket_path()
        assert path == f"/tmp/beachcomber-{uid}/sock"

    def test_no_tmpdir_env_uses_slash_tmp(
        self, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        monkeypatch.delenv("XDG_RUNTIME_DIR", raising=False)
        monkeypatch.delenv("TMPDIR", raising=False)
        uid = get_uid()
        path = discover_socket_path()
        assert path == f"/tmp/beachcomber-{uid}/sock"
