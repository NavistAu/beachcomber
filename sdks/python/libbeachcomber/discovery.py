"""Socket path discovery for the beachcomber daemon.

Discovery order:
1. ``$XDG_RUNTIME_DIR/beachcomber/sock``
2. ``$TMPDIR/beachcomber-<uid>/sock``
3. ``/tmp/beachcomber-<uid>/sock``
"""

import os


def get_uid() -> int:
    """Return the effective user ID of the current process."""
    return os.geteuid()


def discover_socket_path() -> str:
    """Return the expected socket path for the running daemon.

    Checks the standard locations in order. Returns the first path that
    *should* exist according to the discovery rules — callers are
    responsible for verifying the socket is reachable.

    Returns:
        Absolute path string for the Unix domain socket.
    """
    xdg_runtime = os.environ.get("XDG_RUNTIME_DIR", "")
    if xdg_runtime:
        candidate = os.path.join(xdg_runtime, "beachcomber", "sock")
        if os.path.exists(candidate):
            return candidate

    uid = get_uid()
    tmpdir = os.environ.get("TMPDIR", "/tmp").rstrip("/")
    return os.path.join(tmpdir, f"beachcomber-{uid}", "sock")
