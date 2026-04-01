"""Unit tests for protocol encoding and decoding."""

from __future__ import annotations

import json

import pytest

from beachcomber.exceptions import ProtocolError, ServerError
from beachcomber.protocol import (
    build_context_request,
    build_get_request,
    build_list_request,
    build_poke_request,
    build_status_request,
    decode_response,
    encode_request,
)


class TestEncodeRequest:
    def test_includes_op(self) -> None:
        raw = encode_request("get", key="git.branch")
        obj = json.loads(raw)
        assert obj["op"] == "get"

    def test_includes_extra_fields(self) -> None:
        raw = encode_request("get", key="git.branch", path="/repo")
        obj = json.loads(raw)
        assert obj["key"] == "git.branch"
        assert obj["path"] == "/repo"

    def test_ends_with_newline(self) -> None:
        raw = encode_request("list")
        assert raw.endswith(b"\n")

    def test_is_bytes(self) -> None:
        assert isinstance(encode_request("status"), bytes)

    def test_compact_json_no_extra_spaces(self) -> None:
        raw = encode_request("get", key="k")
        # separators=(",", ":") means no spaces around colon or comma
        assert b": " not in raw
        assert b", " not in raw


class TestDecodeResponse:
    def test_ok_true_returns_dict(self) -> None:
        resp = decode_response('{"ok":true,"data":"main","age_ms":10,"stale":false}')
        assert resp["data"] == "main"
        assert resp["age_ms"] == 10

    def test_ok_false_raises_server_error(self) -> None:
        with pytest.raises(ServerError) as exc_info:
            decode_response('{"ok":false,"error":"unknown provider"}')
        assert "unknown provider" in str(exc_info.value)

    def test_ok_false_no_error_field(self) -> None:
        with pytest.raises(ServerError) as exc_info:
            decode_response('{"ok":false}')
        assert "unknown error" in str(exc_info.value)

    def test_invalid_json_raises_protocol_error(self) -> None:
        with pytest.raises(ProtocolError):
            decode_response("not json at all")

    def test_empty_line_raises_protocol_error(self) -> None:
        with pytest.raises(ProtocolError):
            decode_response("")

    def test_whitespace_only_raises_protocol_error(self) -> None:
        with pytest.raises(ProtocolError):
            decode_response("   \n")

    def test_json_array_raises_protocol_error(self) -> None:
        with pytest.raises(ProtocolError):
            decode_response("[1,2,3]")

    def test_missing_ok_field_raises_protocol_error(self) -> None:
        with pytest.raises(ProtocolError, match="missing 'ok'"):
            decode_response('{"data":"foo"}')

    def test_strips_trailing_newline(self) -> None:
        resp = decode_response('{"ok":true}\n')
        assert resp["ok"] is True

    def test_strips_whitespace(self) -> None:
        resp = decode_response('  {"ok":true}  ')
        assert resp["ok"] is True


class TestBuildRequests:
    def test_get_request_key_only(self) -> None:
        obj = json.loads(build_get_request("git.branch"))
        assert obj == {"op": "get", "key": "git.branch"}

    def test_get_request_with_path(self) -> None:
        obj = json.loads(build_get_request("git.branch", "/repo"))
        assert obj == {"op": "get", "key": "git.branch", "path": "/repo"}

    def test_get_request_path_none_omitted(self) -> None:
        obj = json.loads(build_get_request("hostname", None))
        assert "path" not in obj

    def test_poke_request_key_only(self) -> None:
        obj = json.loads(build_poke_request("git"))
        assert obj == {"op": "poke", "key": "git"}

    def test_poke_request_with_path(self) -> None:
        obj = json.loads(build_poke_request("git", "/repo"))
        assert obj["path"] == "/repo"

    def test_context_request(self) -> None:
        obj = json.loads(build_context_request("/my/dir"))
        assert obj == {"op": "context", "path": "/my/dir"}

    def test_list_request(self) -> None:
        obj = json.loads(build_list_request())
        assert obj == {"op": "list"}

    def test_status_request(self) -> None:
        obj = json.loads(build_status_request())
        assert obj == {"op": "status"}
