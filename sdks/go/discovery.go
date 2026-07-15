package beachcomber

import (
	"fmt"
	"os"
)

// DiscoverSocketPath returns the expected Unix socket path for the beachcomber
// daemon. It mirrors the daemon's bind-path resolution
// (Config::resolve_socket_path), minus the config-file step which is
// daemon-only. Discovery order:
//  1. $BEACHCOMBER_SOCKET  (if set and non-empty)
//  2. /tmp/beachcomber-<uid>/sock
//
// There is no existence probe and no session-scoped environment ($TMPDIR,
// $XDG_RUNTIME_DIR) is consulted: the result is the single stable per-user
// path the daemon binds for the same environment. Non-standard setups point
// clients at the daemon via BEACHCOMBER_SOCKET.
//
// The returned path is not guaranteed to be reachable; callers should handle
// connection errors as [ErrDaemonNotRunning].
func DiscoverSocketPath() string {
	if sock := os.Getenv("BEACHCOMBER_SOCKET"); sock != "" {
		return sock
	}

	uid := os.Getuid()
	return fmt.Sprintf("/tmp/beachcomber-%d/sock", uid)
}
