"""Typed response shapes for beachcomber SDK."""

from __future__ import annotations

from dataclasses import dataclass, field
from enum import Enum
from typing import Any, Dict, Optional


@dataclass(frozen=True)
class HelloInfo:
    protocol_version: str
    daemon_version: str


@dataclass(frozen=True)
class CacheRow:
    provider: str
    field: Optional[str]
    path: Optional[str]
    value: Any
    age_ms: int
    stale: bool
    kind: Optional[Dict[str, Any]] = None
    poll_interval_secs: Optional[int] = None
    poll_count: Optional[int] = None
    fsevents_reinstate: Optional[bool] = None
    polls_elapsed: Optional[int] = None
    next_poll_in_secs: Optional[int] = None
    failure: Optional[Dict[str, Any]] = None
    source: Optional[str] = None


@dataclass(frozen=True)
class Verdict:
    level: str
    message: str


@dataclass(frozen=True)
class DaemonHealth:
    pid: int
    version: str
    uptime_secs: int
    socket_path: str
    config_path: Optional[str]
    requests_total: int
    in_flight: int
    active_watchers: int
    cache_entries: int
    verdicts: list = field(default_factory=list)  # list[Verdict]


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
