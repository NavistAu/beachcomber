/**
 * Native library discovery for the FFI transport: the rule every SDK follows
 * (docs/sdk-api.md, "Library discovery"; sdks/discovery.json is its fixture).
 * The daemon's socket is resolved by libbeachcomber itself
 * (`bc_socket_path`), never here.
 */

import * as fs from 'fs';
import * as path from 'path';

/** The library's file name: `.dylib` on macOS, `.so` on every other platform. */
export function platformLibraryName(): string {
  return process.platform === 'darwin' ? 'libbeachcomber.dylib' : 'libbeachcomber.so';
}

/**
 * The first executable `comb` file on `$PATH`, the way a shell finds it, or
 * null when there is none.
 */
export function resolveCombOnPath(): string | null {
  const dirs = (process.env['PATH'] ?? '').split(path.delimiter).filter((d) => d.length > 0);
  for (const dir of dirs) {
    const candidate = path.join(dir, 'comb');
    try {
      if (!fs.statSync(candidate).isFile()) continue;
      fs.accessSync(candidate, fs.constants.X_OK);
      return candidate;
    } catch {
      // not here — keep looking
    }
  }
  return null;
}

/** One place the loader tries, and how it was derived (for the failure message). */
export interface LibraryCandidate {
  path: string;
  source: string;
}

/**
 * The library candidates, in the order the loader tries them:
 *
 * 1. `$BEACHCOMBER_LIB`, set and not empty, is the only candidate: a path set
 *    on purpose that does not load is an error, not a reason to load some
 *    other library.
 * 2. Otherwise `<dir>/../lib/<file>`, where `<dir>` holds the first executable
 *    `comb` on `$PATH` with its symlinks resolved (a Homebrew `comb` is a
 *    symlink into the Cellar, where the library sits beside it),
 * 3. then the bare file name, for the system loader to resolve.
 */
export function libraryCandidates(): LibraryCandidate[] {
  const envLib = process.env['BEACHCOMBER_LIB'];
  if (envLib) {
    return [{ path: envLib, source: '$BEACHCOMBER_LIB' }];
  }

  const candidates: LibraryCandidate[] = [];
  const libName = platformLibraryName();
  const comb = resolveCombOnPath();
  if (comb) {
    let resolved = comb;
    try {
      resolved = fs.realpathSync(comb);
    } catch {
      // a link that cannot be resolved is read where it stands
    }
    candidates.push({
      path: path.join(path.dirname(resolved), '..', 'lib', libName),
      source: `beside comb (${resolved})`,
    });
  }
  candidates.push({ path: libName, source: 'the system loader' });
  return candidates;
}
