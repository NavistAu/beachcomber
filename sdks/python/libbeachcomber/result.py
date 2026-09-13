"""CombResult dataclass returned by client get() calls."""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, Optional


@dataclass
class CombResult:
    """Result of a ``get`` query to the beachcomber daemon.

    ``get`` returns the node as the daemon holds it — both halves, nothing
    rendered. A node may hold a cached value (``data``), a template
    (``template``), or both. A template-only result is an address whose
    value :meth:`~beachcomber.client.Client.resolve` renders: this is the
    raw half — ``resolve(key, cwd, env)`` is how you get the value. Such a
    result is neither a hit (there is no value until it is rendered) nor a
    miss (something is stored there).

    Attributes:
        ok: Whether the daemon responded without error (always ``True``
            when returned from the client — errors raise exceptions).
        data: The cached value. ``None`` when nothing is cached at the
            address. May be a string, number, bool, or dict depending on
            the provider and key.
        age_ms: How many milliseconds old the cached value is. ``0`` when
            there is no cached value.
        stale: ``True`` when the value is past its TTL but no fresh value
            is available yet.
        error: Error message string when ``ok`` is ``False``. The client
            raises :class:`~beachcomber.exceptions.ServerError` for these,
            so callers typically do not see this field set.
        template: The template in effect at the address, unrendered: a
            string for a leaf, a dict for a group of templates. ``None``
            when no template is in effect.
        rest: The address left over when the template walk stopped at a
            string — ``get("p.t.x")`` where ``p.t`` is a template string
            gives ``template`` of ``p.t`` and ``rest`` of ``"x"``. ``None``
            otherwise.
    """

    ok: bool = True
    data: Any = None
    age_ms: int = 0
    stale: bool = False
    error: Optional[str] = None
    template: Any = None
    rest: Optional[str] = None

    @property
    def is_hit(self) -> bool:
        """``True`` when the cache contains a value for this key."""
        return self.data is not None

    @property
    def is_miss(self) -> bool:
        """``True`` when nothing is stored at the address: no cached value
        and no template. A template-only result is not a miss."""
        return self.data is None and self.template is None

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
