"""Typed response shapes for beachcomber SDK."""

from __future__ import annotations

from dataclasses import dataclass, field
from enum import Enum
from typing import Any, Optional


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
    data: Any
    age_ms: int
    stale: bool
