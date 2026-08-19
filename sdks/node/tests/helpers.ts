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
import * as net from 'net';
import * as os from 'os';
import * as path from 'path';

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

/** True when a `comb` binary was found and daemon-backed tests can run. */
export function daemonAvailable(): boolean {
  return COMB_BIN !== undefined;
}

function waitForSocket(sockPath: string, timeoutMs = 5000): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  return new Promise((resolve) => {
    function attempt(): void {
      if (Date.now() > deadline) {
        resolve(false);
        return;
      }
      const s = new net.Socket();
      s.setTimeout(100);
      s.connect(sockPath, () => {
        s.destroy();
        resolve(true);
      });
      s.on('error', () => setTimeout(attempt, 50));
      s.on('timeout', () => {
        s.destroy();
        setTimeout(attempt, 50);
      });
    }
    attempt();
  });
}

export interface TestDaemon {
  sockPath: string;
  stop(): void;
}

/** Spawn a fresh `comb daemon` on a private temp socket. */
export async function spawnDaemon(): Promise<TestDaemon> {
  if (!COMB_BIN) {
    throw new Error('comb binary not found — call daemonAvailable() first');
  }
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'beachcomber-node-test-'));
  const sockPath = path.join(tmpDir, 'comb.sock');

  const proc = spawn(COMB_BIN, ['daemon', '--socket', sockPath], {
    stdio: ['ignore', 'ignore', 'ignore'],
    env: { ...process.env },
  });

  const ready = await waitForSocket(sockPath, 5000);
  if (!ready) {
    proc.kill('SIGKILL');
    fs.rmSync(tmpDir, { recursive: true, force: true });
    throw new Error(`daemon did not start within 5s (COMB_BIN=${COMB_BIN})`);
  }

  return {
    sockPath,
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
