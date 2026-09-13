"""Unit tests for the options JSON ``Client.__init__`` hands ``bc_client_new``."""

from __future__ import annotations

import json

from libbeachcomber.client import _build_options_json


def test_exec_is_omitted_by_default() -> None:
    options = json.loads(_build_options_json(None, 0.1, None, False))
    assert "exec" not in options


def test_exec_true_reaches_the_options_json() -> None:
    options = json.loads(_build_options_json("/tmp/s", 0.1, False, True))
    assert options["exec"] is True
    assert options["socket_path"] == "/tmp/s"
    assert options["autostart"] is False
