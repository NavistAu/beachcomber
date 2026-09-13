"""Unit tests for CombResult dataclass."""

from __future__ import annotations

import pytest

from libbeachcomber.client import _result_from_get_payload
from libbeachcomber.result import CombResult


class TestCombResultIsHit:
    def test_hit_when_data_is_string(self) -> None:
        result = CombResult(ok=True, data="main")
        assert result.is_hit is True

    def test_hit_when_data_is_dict(self) -> None:
        result = CombResult(ok=True, data={"branch": "main"})
        assert result.is_hit is True

    def test_hit_when_data_is_zero(self) -> None:
        # 0 and False are valid data values — not a miss.
        result = CombResult(ok=True, data=0)
        assert result.is_hit is True

    def test_hit_when_data_is_false(self) -> None:
        result = CombResult(ok=True, data=False)
        assert result.is_hit is True

    def test_miss_when_data_is_none(self) -> None:
        result = CombResult(ok=True, data=None)
        assert result.is_hit is False

    def test_default_is_miss(self) -> None:
        result = CombResult()
        assert result.is_hit is False
        assert result.is_miss is True


class TestCombResultTemplateHalf:
    """The three shapes bc_get's payload takes (libbeachcomber-ffi
    combresult_to_json): a template-only node, a node with both halves, and
    a plain hit that carries no template keys at all."""

    def test_template_only_is_neither_hit_nor_miss(self) -> None:
        result = _result_from_get_payload(
            {"data": None, "age_ms": None, "stale": None, "template": "{{ 1 }}"}
        )
        assert result.template == "{{ 1 }}"
        assert result.data is None
        assert result.is_hit is False
        assert result.is_miss is False

    def test_both_halves_is_a_hit_carrying_the_template(self) -> None:
        result = _result_from_get_payload(
            {"data": 1, "age_ms": 12, "stale": False, "template": "{{ 2 }}", "rest": "x"}
        )
        assert result.is_hit is True
        assert result.is_miss is False
        assert result.data == 1
        assert result.template == "{{ 2 }}"
        assert result.rest == "x"

    def test_plain_hit_has_no_template_or_rest(self) -> None:
        result = _result_from_get_payload({"data": "main", "age_ms": 5, "stale": False})
        assert result.is_hit is True
        assert result.template is None
        assert result.rest is None

    def test_miss_has_neither_half(self) -> None:
        result = _result_from_get_payload({"data": None, "age_ms": None, "stale": None})
        assert result.is_miss is True
        assert result.is_hit is False
        assert result.template is None

    def test_template_may_be_a_group(self) -> None:
        result = _result_from_get_payload(
            {"data": None, "age_ms": None, "stale": None, "template": {"a": "{{ 1 }}"}}
        )
        assert result.template == {"a": "{{ 1 }}"}
        assert result.is_miss is False


class TestCombResultDefaults:
    def test_ok_default_true(self) -> None:
        assert CombResult().ok is True

    def test_age_ms_default_zero(self) -> None:
        assert CombResult().age_ms == 0

    def test_stale_default_false(self) -> None:
        assert CombResult().stale is False

    def test_error_default_none(self) -> None:
        assert CombResult().error is None


class TestCombResultSubscript:
    def test_subscript_on_dict_data(self) -> None:
        result = CombResult(ok=True, data={"branch": "main", "dirty": False})
        assert result["branch"] == "main"
        assert result["dirty"] is False

    def test_subscript_missing_key_raises_key_error(self) -> None:
        result = CombResult(ok=True, data={"branch": "main"})
        with pytest.raises(KeyError):
            _ = result["nonexistent"]

    def test_subscript_on_string_data_raises_type_error(self) -> None:
        result = CombResult(ok=True, data="main")
        with pytest.raises(TypeError, match="not dict"):
            _ = result["branch"]

    def test_subscript_on_none_data_raises_type_error(self) -> None:
        result = CombResult(ok=True, data=None)
        with pytest.raises(TypeError):
            _ = result["branch"]

    def test_subscript_on_int_data_raises_type_error(self) -> None:
        result = CombResult(ok=True, data=42)
        with pytest.raises(TypeError):
            _ = result["branch"]


class TestCombResultFields:
    def test_fields_are_accessible(self) -> None:
        result = CombResult(ok=True, data="main", age_ms=100, stale=True)
        assert result.data == "main"
        assert result.age_ms == 100
        assert result.stale is True

    def test_repr_contains_data(self) -> None:
        result = CombResult(ok=True, data="main")
        assert "main" in repr(result)
