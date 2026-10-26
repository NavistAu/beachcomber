/**
 * Real `comb` daemon helper for integration tests.
 *
 * The Node SDK is a binding over the native C ABI (FFI via `koffi`, or the
 * `comb` subprocess fallback) rather than a hand-rolled NDJSON socket
 * client, so there is no longer a meaningful wire-level mock to test
 * against — the wire protocol is entirely the native library's concern.
 * Tests here drive the real binding against a real daemon, the same way
 * `conformance_runner.js` does. Mirrors `sdks/python/tests/conftest.py`.
 *
 * Requires `COMB_BIN` (or `comb` on `$PATH`, or a `target/debug/comb`
 * built at the repo root) and `BEACHCOMBER_LIB` (or a discoverable
 * `libbeachcomber.{so,dylib}`, including `target/debug/` at the repo
 * root). Tests needing the daemon should skip via `daemonAvailable()`
 * when neither is found.
 */

import { spawn } from 'child_process';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import type { Readable } from 'stream';

const SDK_DIR = path.dirname(new URL(import.meta.url).pathname);
const REPO_ROOT = path.resolve(SDK_DIR, '..', '..', '..');

function defaultLibPath(): string {
  const name = process.platform === 'darwin' ? 'libbeachcomber.dylib' : 'libbeachcomber.so';
  return path.join(REPO_ROOT, 'target', 'debug', name);
}

// Make a locally built dylib/comb discoverable without requiring the caller
// to set BEACHCOMBER_LIB/COMB_BIN themselves — but never override an
// explicit setting.
if (!process.env['BEACHCOMBER_LIB'] && fs.existsSync(defaultLibPath())) {
  process.env['BEACHCOMBER_LIB'] = defaultLibPath();
}

function findCombBin(): string | undefined {
  if (process.env['COMB_BIN']) return process.env['COMB_BIN'];
  const local = path.join(REPO_ROOT, 'target', 'debug', 'comb');
  if (fs.existsSync(local)) return local;
  const pathEnv = process.env['PATH'] ?? '';
  for (const dir of pathEnv.split(path.delimiter)) {
    const candidate = path.join(dir, 'comb');
    if (fs.existsSync(candidate)) return candidate;
  }
  return undefined;
}

const COMB_BIN = findCombBin();
if (!process.env['COMB_BIN'] && COMB_BIN) {
  process.env['COMB_BIN'] = COMB_BIN;
}

/** True when a `comb` binary was found and daemon-backed tests can run. */
export function daemonAvailable(): boolean {
  return COMB_BIN !== undefined;
}

// The daemon writes a newline on its `--ready-fd` pipe once it serves its
// socket, and closes it unwritten if it exits first.
function waitForReady(ready: Readable, timeoutMs: number): Promise<boolean> {
  return new Promise((resolve) => {
    const timer = setTimeout(() => done(false), timeoutMs);
    function done(served: boolean): void {
      clearTimeout(timer);
      ready.removeAllListeners();
      ready.destroy();
      resolve(served);
    }
    ready.on('data', (chunk: Buffer) => done(chunk.includes(10)));
    ready.on('end', () => done(false));
    ready.on('error', () => done(false));
  });
}

export interface TestDaemon {
  sockPath: string;
  /** The daemon's `$HOME` and `$XDG_CONFIG_HOME`; its state dir is `.local/state` below it. */
  tmpDir: string;
  stop(): void;
}

/** Spawn a fresh `comb daemon` on a private temp socket. */
export async function spawnDaemon(): Promise<TestDaemon> {
  if (!COMB_BIN) {
    throw new Error('comb binary not found — call daemonAvailable() first');
  }
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'beachcomber-node-test-'));
  const sockPath = path.join(tmpDir, 'comb.sock');

  // The daemon writes its managed defaults under $XDG_CONFIG_HOME on start
  // (falling back to $HOME/.config) and its log under $XDG_STATE_HOME: all
  // three point into the test's tempdir.
  // Its fd 3 is a pipe to this process: `--ready-fd 3`.
  const proc = spawn(COMB_BIN, ['daemon', '--socket', sockPath, '--ready-fd', '3'], {
    stdio: ['ignore', 'ignore', 'ignore', 'pipe'],
    env: {
      ...process.env,
      XDG_CONFIG_HOME: tmpDir,
      HOME: tmpDir,
      XDG_STATE_HOME: path.join(tmpDir, '.local', 'state'),
    },
  });

  const ready = await waitForReady(proc.stdio[3] as Readable, 5000);
  if (!ready) {
    proc.kill('SIGKILL');
    fs.rmSync(tmpDir, { recursive: true, force: true });
    throw new Error(`daemon did not serve within 5s (COMB_BIN=${COMB_BIN})`);
  }

  return {
    sockPath,
    tmpDir,
    stop(): void {
      try {
        proc.kill('SIGTERM');
      } catch {
        // best-effort
      }
      try {
        fs.rmSync(tmpDir, { recursive: true, force: true });
      } catch {
        // best-effort
      }
    },
  };
}
