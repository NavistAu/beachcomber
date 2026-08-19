/**
 * Socket path discovery for the beachcomber daemon.
 *
 * Mirrors the daemon's bind-path resolution (Config::resolve_socket_path),
 * minus the config-file step which is daemon-only. Priority:
 *  1. $BEACHCOMBER_SOCKET  (if set and non-empty)
 *  2. /tmp/beachcomber-<uid>/sock
 *
 * There is no existence probe and no session-scoped environment (such as
 * $TMPDIR or $XDG_RUNTIME_DIR) is consulted: the result is the single stable
 * per-user path the daemon binds for the same environment, so singleton
 * enforcement works per-user rather than per-session. Non-standard setups
 * point clients at the daemon via BEACHCOMBER_SOCKET.
 */

import * as fs from 'fs';
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

  const uid = getUid();
  return path.join('/tmp', `beachcomber-${uid}`, 'sock');
}

/** The platform-conventional native library filename. */
export function platformLibraryName(): string {
  switch (process.platform) {
    case 'darwin':
      return 'libbeachcomber.dylib';
    case 'win32':
      return 'beachcomber.dll';
    default:
      return 'libbeachcomber.so';
  }
}

/**
 * Resolve `comb` on `$PATH`, the way a shell would. Returns null if not found.
 */
export function resolveCombOnPath(): string | null {
  const pathEnv = process.env['PATH'] ?? '';
  const dirs = pathEnv.split(path.delimiter).filter((d) => d.length > 0);
  const names = process.platform === 'win32' ? ['comb.exe', 'comb'] : ['comb'];
  for (const dir of dirs) {
    for (const name of names) {
      const candidate = path.join(dir, name);
      try {
        fs.accessSync(candidate, fs.constants.X_OK);
        return candidate;
      } catch {
        // not here — keep looking
      }
    }
  }
  return null;
}

/**
 * The native library candidate paths, in the seven-point contract's fixed
 * search order:
 *
 * 1. `$BEACHCOMBER_LIB`, if set.
 * 2. `../lib/<platform library name>` relative to `comb` resolved on `$PATH`.
 * 3. The platform default dynamic-linker search path (a bare library name,
 *    left for `dlopen`/`LoadLibrary` to resolve).
 *
 * Each entry names how it was derived, for use in a loud discovery-failure
 * message naming every location tried.
 */
export interface LibraryCandidate {
  path: string;
  source: string;
}

export function libraryCandidates(): LibraryCandidate[] {
  const candidates: LibraryCandidate[] = [];
  const libName = platformLibraryName();

  const envLib = process.env['BEACHCOMBER_LIB'];
  if (envLib) {
    candidates.push({ path: envLib, source: '$BEACHCOMBER_LIB' });
  }

  const comb = resolveCombOnPath();
  if (comb) {
    const relative = path.join(path.dirname(comb), '..', 'lib', libName);
    candidates.push({ path: relative, source: `../lib/ next to comb (${comb})` });
  }

  candidates.push({ path: libName, source: 'platform default search path' });

  return candidates;
}
