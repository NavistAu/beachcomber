"""CombResult dataclass returned by client get() calls."""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, Optional


@dataclass
class CombResult:
    """Result of a ``get`` query to the beachcomber daemon.

    Attributes:
        ok: Whether the daemon responded without error (always ``True``
            when returned from the client — errors raise exceptions).
        data: The cached value. ``None`` on a cache miss. May be a string,
            number, bool, or dict depending on the provider and key.
        age_ms: How many milliseconds old the cached value is. ``0`` on a
            miss.
        stale: ``True`` when the value is past its TTL but no fresh value
            is available yet.
        error: Error message string when ``ok`` is ``False``. The client
            raises :class:`~beachcomber.exceptions.ServerError` for these,
            so callers typically do not see this field set.
    """

    ok: bool = True
    data: Any = None
    age_ms: int = 0
    stale: bool = False
    error: Optional[str] = None

    @property
    def is_hit(self) -> bool:
        """``True`` when the cache contains a value for this key."""
        return self.data is not None

    def __getitem__(self, key: str) -> Any:
        """Access a field from dict data by subscript.

        Useful when querying a full provider that returns an object,
        e.g. ``result["branch"]`` after ``client.get("git")``.

        Args:
            key: Field name to look up in the data dict.

        Returns:
            The field value.

        Raises:
            TypeError: If ``data`` is not a dict.
            KeyError: If ``key`` is not present in the dict.
        """
        if not isinstance(self.data, dict):
            raise TypeError(
                f"CombResult.data is {type(self.data).__name__}, not dict; "
                "subscript access is only valid for object responses"
            )
        return self.data[key]
