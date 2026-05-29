/**
 * Socket path discovery for the beachcomber daemon.
 *
 * Mirrors the daemon's bind-path resolution (Config::resolve_socket_path),
 * minus the config-file step which is daemon-only. Priority:
 *  1. $BEACHCOMBER_SOCKET  (if set and non-empty)
 *  2. $XDG_RUNTIME_DIR/beachcomber/sock  (if XDG_RUNTIME_DIR is set)
 *  3. /tmp/beachcomber-<uid>/sock
 *
 * There is no existence probe and $TMPDIR is not consulted: the result is the
 * single path the daemon binds for the same environment. Non-standard setups
 * point clients at the daemon via BEACHCOMBER_SOCKET.
 */

import * as path from 'path';

/**
 * Return the process UID as a string.
 * On platforms that do not expose process.getuid (Windows), returns '0'.
 */
export function getUid(): string {
  if (typeof process.getuid === 'function') {
    return String(process.getuid());
  }
  return '0';
}

/**
 * Discover the beachcomber socket path using the standard priority order.
 *
 * Note: this function does NOT verify that the socket exists or that the
 * daemon is reachable — callers should attempt to connect and handle errors.
 */
export function discoverSocketPath(): string {
  const sock = process.env['BEACHCOMBER_SOCKET'];
  if (sock) {
    return sock;
  }

  const xdgRuntime = process.env['XDG_RUNTIME_DIR'];
  if (xdgRuntime) {
    return path.join(xdgRuntime, 'beachcomber', 'sock');
  }

  const uid = getUid();
  return path.join('/tmp', `beachcomber-${uid}`, 'sock');
}
