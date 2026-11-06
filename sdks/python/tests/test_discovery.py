"""Library discovery, against the shared fixture ``sdks/discovery.json`` (the
rule in docs/sdk-api.md, "Library discovery"): every case builds its tree in
a fresh directory and checks the candidate paths, in order. No library is
loaded, so no build is required."""

from __future__ import annotations

import json
import os
import pathlib
import shutil
import sys
import tempfile
from typing import Any, Dict, Iterator

import pytest

from libbeachcomber.discovery import candidates, library_basename

_FIXTURE = pathlib.Path(__file__).resolve().parents[2] / "discovery.json"
_CASES = json.loads(_FIXTURE.read_text())["cases"]


def _build(root: str, tree: Dict[str, str], lib: str) -> None:
    """``tree`` under ``root``: "exe" an executable file, "file" one that is
    not, "-> target" a symlink to target."""
    for rel, kind in tree.items():
        path = os.path.join(root, rel.replace("{lib}", lib))
        os.makedirs(os.path.dirname(path), exist_ok=True)
        if kind.startswith("-> "):
            os.symlink(kind[3:], path)
            continue
        with open(path, "w") as f:
            f.write("#!/bin/sh\n")
        os.chmod(path, 0o755 if kind == "exe" else 0o644)


@pytest.fixture
def root() -> Iterator[str]:
    """A fresh directory under /tmp, its symlinks resolved."""
    made = tempfile.mkdtemp(prefix="bcdisc", dir="/tmp")
    try:
        yield os.path.realpath(made)
    finally:
        shutil.rmtree(made, ignore_errors=True)


@pytest.mark.parametrize("case", _CASES, ids=[c["name"] for c in _CASES])
def test_the_shared_fixture(case: Dict[str, Any], root: str, monkeypatch: pytest.MonkeyPatch) -> None:
    lib = library_basename()

    def sub(s: str) -> str:
        return s.replace("{root}", root).replace("{lib}", lib)

    _build(root, case["tree"], lib)
    monkeypatch.setenv("PATH", os.pathsep.join(sub(p) for p in case["path"]))
    if "lib_env" in case:
        monkeypatch.setenv("BEACHCOMBER_LIB", sub(case["lib_env"]))
    else:
        monkeypatch.delenv("BEACHCOMBER_LIB", raising=False)

    assert [c.path for c in candidates()] == [sub(p) for p in case["expect"]]


def test_nothing_loading_names_every_candidate_and_why(
    root: str, monkeypatch: pytest.MonkeyPatch
) -> None:
    from libbeachcomber import LibraryNotFoundError, _native

    _build(root, {"bin/comb": "exe"}, library_basename())
    monkeypatch.setenv("PATH", f"{root}/bin")
    monkeypatch.delenv("BEACHCOMBER_LIB", raising=False)
    monkeypatch.setattr(_native.discovery, "library_basename", lambda: "libbeachcomber-none.so")

    with pytest.raises(LibraryNotFoundError) as excinfo:
        _native._load()
    message = str(excinfo.value)
    assert f"beside comb ({root}/bin/comb) ({root}/lib/libbeachcomber-none.so): " in message
    assert "the system loader (libbeachcomber-none.so): " in message


def test_the_library_file_is_dylib_on_macos_and_so_everywhere_else(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    for platform, name in (
        ("darwin", "libbeachcomber.dylib"),
        ("linux", "libbeachcomber.so"),
        ("freebsd14", "libbeachcomber.so"),
    ):
        monkeypatch.setattr(sys, "platform", platform)
        assert library_basename() == name
