"""beachcomber — Python client SDK for the comb shell-state daemon.

Quick start::

    from beachcomber import Client

    client = Client()
    result = client.get("git.branch", path="/path/to/repo")
    if result.is_hit:
        print(result.data)        # "main"
        print(result.age_ms)      # 234
        print(result.stale)       # False

    # Full provider object
    result = client.get("git", path="/path/to/repo")
    if result.is_hit:
        print(result["branch"])   # subscript into dict data

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

from .client import Client, Session
from .exceptions import CombError, DaemonNotRunning, ProtocolError, ServerError
from .result import CombResult

__all__ = [
    "Client",
    "Session",
    "CombResult",
    "CombError",
    "DaemonNotRunning",
    "ProtocolError",
    "ServerError",
]

__version__ = "0.1.0"
