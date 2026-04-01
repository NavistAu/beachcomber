package beachcomber

import (
	"fmt"
	"os"
	"path/filepath"
)

// DiscoverSocketPath returns the expected Unix socket path for the beachcomber
// daemon. Discovery order:
//  1. $XDG_RUNTIME_DIR/beachcomber/sock  (if the variable is set and the path exists)
//  2. $TMPDIR/beachcomber-<uid>/sock
//  3. /tmp/beachcomber-<uid>/sock
//
// The returned path is not guaranteed to be reachable; callers should handle
// connection errors as [ErrDaemonNotRunning].
func DiscoverSocketPath() string {
	if xdg := os.Getenv("XDG_RUNTIME_DIR"); xdg != "" {
		candidate := filepath.Join(xdg, "beachcomber", "sock")
		if _, err := os.Stat(candidate); err == nil {
			return candidate
		}
	}

	uid := os.Getuid()
	dir := fmt.Sprintf("beachcomber-%d", uid)

	if tmpdir := os.Getenv("TMPDIR"); tmpdir != "" {
		return filepath.Join(tmpdir, dir, "sock")
	}

	return fmt.Sprintf("/tmp/%s/sock", dir)
}
