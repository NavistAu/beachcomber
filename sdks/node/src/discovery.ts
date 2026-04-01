/**
 * Socket path discovery for the beachcomber daemon.
 *
 * Priority:
 *  1. $XDG_RUNTIME_DIR/beachcomber/sock
 *  2. $TMPDIR/beachcomber-<uid>/sock
 *  3. /tmp/beachcomber-<uid>/sock
 */

import * as os from 'os';
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
  const xdgRuntime = process.env['XDG_RUNTIME_DIR'];
  if (xdgRuntime) {
    return path.join(xdgRuntime, 'beachcomber', 'sock');
  }

  const uid = getUid();
  const tmpdir = process.env['TMPDIR'] || os.tmpdir() || '/tmp';
  return path.join(tmpdir, `beachcomber-${uid}`, 'sock');
}
