/**
 * Native library discovery for the FFI transport. The daemon's socket is
 * resolved by libbeachcomber itself (`bc_socket_path`), never here.
 */

import * as fs from 'fs';
import * as path from 'path';

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
