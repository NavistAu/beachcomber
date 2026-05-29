"""Socket path discovery for the beachcomber daemon.

Mirrors the daemon's bind-path resolution (``Config::resolve_socket_path``),
minus the config-file step which is daemon-only. Discovery order:

1. ``$BEACHCOMBER_SOCKET``  (if set and non-empty)
2. ``$XDG_RUNTIME_DIR/beachcomber/sock``  (if ``XDG_RUNTIME_DIR`` is set)
3. ``/tmp/beachcomber-<uid>/sock``

There is no existence probe and ``$TMPDIR`` is not consulted: the result is the
single path the daemon binds for the same environment. Non-standard setups
point clients at the daemon via ``BEACHCOMBER_SOCKET``.
"""

import os


def get_uid() -> int:
    """Return the effective user ID of the current process."""
    return os.geteuid()


def discover_socket_path() -> str:
    """Return the expected socket path for the running daemon.

    Resolves to the single path the daemon binds for the current environment.
    Callers are responsible for verifying the socket is reachable.

    Returns:
        Absolute path string for the Unix domain socket.
    """
    sock = os.environ.get("BEACHCOMBER_SOCKET", "")
    if sock:
        return sock

    xdg_runtime = os.environ.get("XDG_RUNTIME_DIR", "")
    if xdg_runtime:
        return os.path.join(xdg_runtime, "beachcomber", "sock")

    uid = get_uid()
    return os.path.join("/tmp", f"beachcomber-{uid}", "sock")
