"""Unit tests for native-library discovery (candidate ordering only).

These test the pure candidate-list builder in :mod:`libbeachcomber.discovery`
— no actual library loading, so no built ``libbeachcomber.{so,dylib}`` is
required. Loading itself is exercised by the conformance runner and the
integration tests, both of which require a real build.
"""

from __future__ import annotations

import os

import pytest

from libbeachcomber.discovery import candidates, library_basename


@pytest.fixture(autouse=True)
def _clean_env(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("BEACHCOMBER_LIB", raising=False)


def test_order_is_env_then_comb_relative_then_platform_default(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("BEACHCOMBER_LIB", "/explicit/libbeachcomber.dylib")
    monkeypatch.setattr("shutil.which", lambda name: "/opt/comb/bin/comb")

    cands = candidates()

    assert [c.description.split(" ")[0] for c in cands][0] == "$BEACHCOMBER_LIB"
    assert cands[0].path == "/explicit/libbeachcomber.dylib"
    assert cands[1].path == os.path.join("/opt/comb/lib", library_basename())
    assert cands[2].path == library_basename()
    assert cands[2].reason is None


def test_env_unset_is_reported_not_silently_skipped(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("BEACHCOMBER_LIB", raising=False)

    cands = candidates()

    assert cands[0].path is None
    assert cands[0].reason == "not set"


def test_comb_not_on_path_is_reported(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr("shutil.which", lambda name: None)

    cands = candidates()

    comb_candidate = cands[1]
    assert comb_candidate.path is None
    assert comb_candidate.reason == "comb not found on $PATH"


def test_comb_relative_uses_parent_of_bin_dir(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr("shutil.which", lambda name: "/usr/local/bin/comb")

    cands = candidates()

    assert cands[1].path == os.path.join("/usr/local/lib", library_basename())


def test_platform_default_is_always_present() -> None:
    cands = candidates()

    assert cands[-1].description == "platform default search path"
    assert cands[-1].path == library_basename()


def test_library_basename_matches_platform(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr("sys.platform", "darwin")
    assert library_basename() == "libbeachcomber.dylib"

    monkeypatch.setattr("sys.platform", "linux")
    assert library_basename() == "libbeachcomber.so"
