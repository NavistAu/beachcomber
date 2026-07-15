"""Unit tests for socket path discovery."""

from __future__ import annotations

import os

import pytest

from libbeachcomber.discovery import discover_socket_path, get_uid


class TestGetUid:
    def test_returns_int(self) -> None:
        uid = get_uid()
        assert isinstance(uid, int)
        assert uid >= 0

    def test_matches_os_geteuid(self) -> None:
        assert get_uid() == os.geteuid()


class TestDiscoverSocketPath:
    def test_beachcomber_socket_takes_precedence(
        self, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        monkeypatch.setenv("BEACHCOMBER_SOCKET", "/custom/path/comb.sock")
        monkeypatch.setenv("XDG_RUNTIME_DIR", "/run/user/1000")
        monkeypatch.setenv("TMPDIR", "/should-not-be-used")
        assert discover_socket_path() == "/custom/path/comb.sock"

    def test_xdg_runtime_dir_is_ignored(
        self, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        # Session-scoped environment must never influence resolution.
        monkeypatch.delenv("BEACHCOMBER_SOCKET", raising=False)
        monkeypatch.setenv("XDG_RUNTIME_DIR", "/run/user/1000")
        monkeypatch.setenv("TMPDIR", "/should-not-be-used")
        uid = get_uid()
        assert discover_socket_path() == f"/tmp/beachcomber-{uid}/sock"

    def test_falls_back_to_slash_tmp(self, monkeypatch: pytest.MonkeyPatch) -> None:
        monkeypatch.delenv("BEACHCOMBER_SOCKET", raising=False)
        monkeypatch.delenv("XDG_RUNTIME_DIR", raising=False)
        monkeypatch.setenv("TMPDIR", "/should-not-be-used")
        uid = get_uid()
        assert discover_socket_path() == f"/tmp/beachcomber-{uid}/sock"

    def test_tmpdir_is_ignored(self, monkeypatch: pytest.MonkeyPatch) -> None:
        # TMPDIR must never influence resolution.
        monkeypatch.delenv("BEACHCOMBER_SOCKET", raising=False)
        monkeypatch.delenv("XDG_RUNTIME_DIR", raising=False)
        monkeypatch.setenv("TMPDIR", "/var/folders/xyz")
        uid = get_uid()
        assert discover_socket_path() == f"/tmp/beachcomber-{uid}/sock"

    def test_no_env_uses_slash_tmp(self, monkeypatch: pytest.MonkeyPatch) -> None:
        monkeypatch.delenv("BEACHCOMBER_SOCKET", raising=False)
        monkeypatch.delenv("XDG_RUNTIME_DIR", raising=False)
        monkeypatch.delenv("TMPDIR", raising=False)
        uid = get_uid()
        assert discover_socket_path() == f"/tmp/beachcomber-{uid}/sock"
