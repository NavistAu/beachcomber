"""Typed response shapes: ``libbeachcomber::protocol``'s, field for field
(``sdks/api.json`` ``types``), as ``bc_status``, ``bc_hello`` and
``bc_introspect`` hand them on. A field the ABI leaves out is ``None``."""

from __future__ import annotations

from dataclasses import dataclass, field
from enum import Enum
from typing import Any, Dict, List, Optional


@dataclass(frozen=True)
class HelloInfo:
    protocol_version: str
    daemon_version: str


@dataclass(frozen=True)
class CacheRow:
    """One row of ``status``: one per (provider, path, source, field).
    ``path`` is ``None`` for the global entry; ``kind`` and ``failure`` are
    the JSON objects the ABI hands on."""

    provider: str
    path: Optional[str]
    source: str
    field: str
    value: Any
    age_ms: Optional[int] = None
    kind: Optional[Dict[str, Any]] = None
    poll_interval_secs: Optional[int] = None
    poll_count: Optional[int] = None
    polls_elapsed: Optional[int] = None
    next_poll_in_secs: Optional[int] = None
    failure: Optional[Dict[str, Any]] = None


@dataclass(frozen=True)
class Verdict:
    level: str
    message: str


@dataclass(frozen=True)
class ReaperStatus:
    """The reaper's capability, in :class:`DaemonHealth`."""

    armed: bool
    visibility: str  # "system-wide" or "confined"
    sweeps: int
    reaped: int
    kill_denied: int


@dataclass(frozen=True)
class DaemonHealth:
    """The ``introspect`` ``daemon`` report."""

    pid: int
    version: str
    uptime_secs: int
    socket_path: str
    config_path: Optional[str]
    requests_total: int
    in_flight: int
    active_watchers: int
    cache_entries: int
    watch_backend: str  # "native", "polling", "disabled" or "unknown"
    reaper: Optional[ReaperStatus]
    verdicts: List[Verdict] = field(default_factory=list)


class IntrospectSubject(str, Enum):
    DAEMON = "daemon"
    PROVIDERS = "providers"
    CONFIG = "config"
    CACHE = "cache"
    LIFECYCLE = "lifecycle"
    WATCHES = "watches"
    TIMERS = "timers"
    DEMAND = "demand"
    PROCS = "procs"


@dataclass(frozen=True)
class IntrospectResponse:
    subject: IntrospectSubject
    daemon: Optional[DaemonHealth]
    other: Any


@dataclass(frozen=True)
class WatchEvent:
    """One emitted change on a watch.

    ``data`` is the resolved value — what ``comb get`` would print for the
    watched key at that moment — or ``None`` when nothing is stored there.
    A value resolved from several cache records has no single age or
    staleness, so a watch reports neither.
    """

    data: Any
