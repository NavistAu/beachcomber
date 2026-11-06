"""beachcomber — Python client SDK for the comb shell-state daemon.

Quick start::

    from libbeachcomber import Client

    client = Client()
    # What `comb get git.branch` prints there; None on a miss.
    branch = client.get("git.branch", cwd="/path/to/repo")

    # Full provider object
    git = client.get("git", cwd="/path/to/repo")
    print(git["branch"])

    # The template itself, unrendered: a facet, read like any key
    client.get("git:template.branch")

    # Force recomputation
    client.refresh("git", path="/path/to/repo")

    # Persistent connection for multiple queries
    with client.session() as session:
        session.set_context("/path/to/repo")
        branch = session.get("git.branch")
        host = session.get("hostname")

    # Daemon status
    status = client.status()
"""

from .client import Client, Session, WatchStream
from .exceptions import (
    BusyError,
    CombError,
    ConnectionFailedError,
    DaemonNotRunningError,
    IoError,
    LibraryNotFoundError,
    LibrarySymbolError,
    PanicError,
    ParseError,
    ServerError,
    TimeoutError,
    UnsupportedError,
)
from .types import (
    CacheRow,
    DaemonHealth,
    HelloInfo,
    IntrospectResponse,
    IntrospectSubject,
    ReaperStatus,
    Verdict,
    WatchEvent,
)

__all__ = [
    "Client",
    "Session",
    "WatchStream",
    "CombError",
    "DaemonNotRunningError",
    "ConnectionFailedError",
    "IoError",
    "ParseError",
    "ServerError",
    "TimeoutError",
    "BusyError",
    "PanicError",
    "LibraryNotFoundError",
    "LibrarySymbolError",
    "UnsupportedError",
    "CacheRow",
    "DaemonHealth",
    "HelloInfo",
    "IntrospectResponse",
    "IntrospectSubject",
    "ReaperStatus",
    "Verdict",
    "WatchEvent",
]

__version__ = "0.1.0"
