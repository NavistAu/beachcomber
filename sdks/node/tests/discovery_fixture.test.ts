import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { libraryCandidates } from '../src/discovery.js';
import { LibraryNotFoundError } from '../src/errors.js';
import { createFfiTransport, koffiAvailable } from '../src/ffi_transport.js';

// sdks/discovery.json: the library-discovery rule (docs/sdk-api.md, "Library
// discovery") as cases every SDK runs. Each builds its tree under a fresh
// directory and checks the candidate paths, in the order they are tried.
interface DiscoveryCase {
  name: string;
  tree: Record<string, string>;
  path: string[];
  lib_env?: string;
  expect: string[];
}

const SDK_DIR = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const fixture = JSON.parse(fs.readFileSync(path.join(SDK_DIR, '..', 'discovery.json'), 'utf8')) as {
  cases: DiscoveryCase[];
};
const LIB = process.platform === 'darwin' ? 'libbeachcomber.dylib' : 'libbeachcomber.so';

/** Run `fn` with PATH and BEACHCOMBER_LIB set as given (unset when undefined), restoring both. */
function withEnv<T>(pathEnv: string, libEnv: string | undefined, fn: () => T): T {
  const saved = { PATH: process.env['PATH'], BEACHCOMBER_LIB: process.env['BEACHCOMBER_LIB'] };
  const set = (key: 'PATH' | 'BEACHCOMBER_LIB', value: string | undefined) => {
    if (value === undefined) delete process.env[key];
    else process.env[key] = value;
  };
  set('PATH', pathEnv);
  set('BEACHCOMBER_LIB', libEnv);
  try {
    return fn();
  } finally {
    set('PATH', saved.PATH);
    set('BEACHCOMBER_LIB', saved.BEACHCOMBER_LIB);
  }
}

describe('a $BEACHCOMBER_LIB that does not load', () => {
  it('is the only location tried, and the failure is LibraryNotFoundError naming it', { skip: !koffiAvailable() }, () => {
    const bogus = `/tmp/bc-no-such-lib/${LIB}`;
    withEnv(process.env['PATH'] ?? '', bogus, () =>
      assert.throws(
        () => createFfiTransport(),
        (e: unknown) =>
          e instanceof LibraryNotFoundError &&
          e.message.includes(bogus) &&
          !e.message.includes('the system loader') &&
          !e.message.includes('beside comb'),
      ),
    );
  });
});

describe('library discovery follows the shared fixture', () => {
  assert.ok(fixture.cases.length > 0, 'the shared fixture has no cases');

  for (const c of fixture.cases) {
    it(c.name, () => {
      const dir = fs.mkdtempSync('/tmp/bcdisc');
      try {
        const root = fs.realpathSync(dir);
        const sub = (s: string) => s.replaceAll('{root}', root).replaceAll('{lib}', LIB);
        for (const [rel, kind] of Object.entries(c.tree)) {
          const p = path.join(root, sub(rel));
          fs.mkdirSync(path.dirname(p), { recursive: true });
          if (kind === 'exe') fs.writeFileSync(p, '#!/bin/sh\n', { mode: 0o755 });
          else if (kind === 'file') fs.writeFileSync(p, '', { mode: 0o644 });
          else if (kind.startsWith('-> ')) fs.symlinkSync(kind.slice(3), p);
          else assert.fail(`unknown tree entry ${rel}: ${kind}`);
        }
        const got = withEnv(
          c.path.map(sub).join(path.delimiter),
          c.lib_env === undefined ? undefined : sub(c.lib_env),
          () => libraryCandidates().map((candidate) => candidate.path),
        );
        assert.deepEqual(got, c.expect.map(sub));
      } finally {
        fs.rmSync(dir, { recursive: true, force: true });
      }
    });
  }
});
