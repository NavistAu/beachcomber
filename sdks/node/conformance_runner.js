#!/usr/bin/env node
/**
 * Protocol conformance runner for the beachcomber Node.js SDK.
 *
 * Loads fixtures from tests/conformance/ relative to the repo root, spawns
 * a fresh daemon per fixture, drives the ops through the Node SDK, and
 * asserts the expected outcomes.
 *
 * Usage:
 *   COMB_BIN=/path/to/comb node sdks/node/conformance_runner.js
 *
 * Exits 0 if all fixtures pass, 1 otherwise.
 *
 * The runner validates typed SDK results, not raw JSON. For example, an
 * introspect{daemon} assertion on data_contains_field:"pid" is checked via
 * DaemonHealth.pid, not by reparsing the raw JSON wire response.
 */

'use strict';

import fs from 'fs';
import net from 'net';
import path from 'path';
import os from 'os';
import { spawnSync, spawn } from 'child_process';

// ---------------------------------------------------------------------------
// Path setup — import the SDK from compiled dist/ or via tsx if available.
// ---------------------------------------------------------------------------

const SDK_DIR = path.dirname(new URL(import.meta.url).pathname);
const REPO_ROOT = path.resolve(SDK_DIR, '..', '..');
const CONFORMANCE_DIR = path.join(REPO_ROOT, 'tests', 'conformance');
const DIST_CLIENT = path.join(SDK_DIR, 'dist', 'client.js');
const DIST_ERRORS = path.join(SDK_DIR, 'dist', 'errors.js');

if (!fs.existsSync(DIST_CLIENT)) {
  console.error(`ERROR: dist/client.js not found. Run 'npm run build' first in sdks/node/`);
  process.exit(1);
}

// Dynamic import is async; use top-level async IIFE.
(async () => {
  const { Client, WatchStream } = await import(DIST_CLIENT);
  const { ServerError } = await import(DIST_ERRORS);

  // ---------------------------------------------------------------------------
  // Fixture discovery
  // ---------------------------------------------------------------------------

  function discoverFixtures() {
    if (!fs.existsSync(CONFORMANCE_DIR)) {
      console.error(`ERROR: conformance directory not found: ${CONFORMANCE_DIR}`);
      process.exit(1);
    }
    const fixtures = [];
    for (const opDir of fs.readdirSync(CONFORMANCE_DIR)) {
      const opPath = path.join(CONFORMANCE_DIR, opDir);
      if (!fs.statSync(opPath).isDirectory()) continue;
      for (const file of fs.readdirSync(opPath)) {
        if (!file.endsWith('.json')) continue;
        const filePath = path.join(opPath, file);
        const text = fs.readFileSync(filePath, 'utf8');
        const v = JSON.parse(text);
        fixtures.push({
          name: v.name,
          description: v.description,
          setup: Array.isArray(v.setup) ? v.setup : [],
          test: v.test,
          expect: v.expect,
          sourcePath: filePath,
        });
      }
    }
    return fixtures;
  }

  // ---------------------------------------------------------------------------
  // Daemon lifecycle
  // ---------------------------------------------------------------------------

  const COMB_BIN = process.env['COMB_BIN'] || 'comb';

  function waitForSocket(sockPath, timeoutMs = 5000) {
    const deadline = Date.now() + timeoutMs;
    return new Promise((resolve) => {
      function attempt() {
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
        s.on('error', () => {
          setTimeout(attempt, 50);
        });
        s.on('timeout', () => {
          s.destroy();
          setTimeout(attempt, 50);
        });
      }
      attempt();
    });
  }

  async function spawnDaemon() {
    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'beachcomber-conform-'));
    const sockPath = path.join(tmpDir, 'comb.sock');

    const proc = spawn(COMB_BIN, ['daemon', '--socket', sockPath], {
      stdio: ['ignore', 'pipe', 'pipe'],
      env: { ...process.env, COMB_SOCKET: sockPath },
    });

    proc.stdout.on('data', () => {});
    proc.stderr.on('data', () => {});

    const ready = await waitForSocket(sockPath, 5000);
    if (!ready) {
      proc.kill('SIGKILL');
      fs.rmSync(tmpDir, { recursive: true, force: true });
      throw new Error(`daemon did not start within 5s (COMB_BIN=${COMB_BIN})`);
    }

    return {
      proc,
      sockPath,
      tmpDir,
      stop() {
        try { proc.kill('SIGTERM'); } catch {}
        try { fs.rmSync(tmpDir, { recursive: true, force: true }); } catch {}
      },
    };
  }

  // ---------------------------------------------------------------------------
  // Op dispatch
  // ---------------------------------------------------------------------------

  /**
   * Canonical response shape used by expectation checking.
   * Mirrors the Rust conformance runner's CanonicalResponse.
   */
  function makeOk({ data = null, dataAsText = null, ageMs = null, stale = null } = {}) {
    return { ok: true, data, dataAsText, ageMs, stale, error: null };
  }

  function makeError(message) {
    return { ok: false, data: null, dataAsText: null, ageMs: null, stale: null, error: message };
  }

  function valueAsText(v) {
    if (v === null || v === undefined) return null;
    if (typeof v === 'string') return v;
    if (typeof v === 'number' || typeof v === 'boolean') return String(v);
    // Objects: not directly as text.
    return null;
  }

  async function runOp(client, descriptor) {
    const { op, args } = descriptor;

    try {
      switch (op) {
        case 'hello': {
          const info = await client.hello();
          const data = {
            protocol_version: info.protocolVersion,
            daemon_version: info.daemonVersion,
          };
          return makeOk({ data });
        }

        case 'get': {
          const key = args.key || '';
          const pathArg = args.path;
          const result = await client.get(key, pathArg);
          if (result.isHit) {
            return makeOk({
              data: result.data,
              dataAsText: result.getString(),
              ageMs: result.ageMs,
              stale: result.stale,
            });
          } else {
            return makeOk();
          }
        }

        case 'refresh': {
          const key = args.key || '';
          const pathArg = args.path;
          await client.refresh(key, pathArg);
          return makeOk();
        }

        case 'context': {
          // Context is session-based; use a fresh session.
          const session = await client.session();
          try {
            await session.setContext(args.path || '/tmp');
            return makeOk();
          } finally {
            session.close();
          }
        }

        case 'put': {
          const key = args.key || '';
          const data = args.data;
          const ttl = args.ttl;
          const pathArg = args.path;
          await client.put(key, data, { ttl, path: pathArg });
          return makeOk();
        }

        case 'status': {
          const rows = await client.status();
          const arr = rows.map((r) => ({
            provider: r.provider,
            field: r.field,
            path: r.path,
            value: r.value,
            age_ms: r.ageMs,
            stale: r.stale,
          }));
          return makeOk({ data: arr });
        }

        case 'watch': {
          const key = args.key || '';
          const pathArg = args.path;
          const stream = await client.watch(key, pathArg);
          let event = null;
          try {
            for await (const ev of stream) {
              event = ev;
              stream.close();
              break;
            }
          } catch (err) {
            if (!event) {
              return makeError(err instanceof Error ? err.message : String(err));
            }
            // Had an event before close threw — treat as success.
          }
          if (event) {
            return makeOk({
              data: event.data,
              dataAsText: valueAsText(event.data),
              ageMs: event.ageMs,
              stale: event.stale,
            });
          }
          return makeOk();
        }

        case 'introspect': {
          const subject = args.subject || 'daemon';
          const durationSecs = args.duration_secs;
          const resp = await client.introspect(subject, { durationSecs });
          if (resp.subject === 'daemon') {
            const h = resp.daemon;
            const data = {
              pid: h.pid,
              version: h.version,
              uptime_secs: h.uptimeSecs,
              socket_path: h.socketPath,
              config_path: h.configPath,
              requests_total: h.requestsTotal,
              in_flight: h.inFlight,
              active_watchers: h.activeWatchers,
              cache_entries: h.cacheEntries,
            };
            return makeOk({ data });
          } else {
            return makeOk({ data: resp.other });
          }
        }

        default:
          throw new Error(`unknown op in fixture: ${op}`);
      }
    } catch (err) {
      return makeError(err instanceof Error ? err.message : String(err));
    }
  }

  // ---------------------------------------------------------------------------
  // Expectation checking
  // ---------------------------------------------------------------------------

  function jsonTypeOf(v) {
    if (v === null) return 'null';
    if (Array.isArray(v)) return 'array';
    if (typeof v === 'object') return 'object';
    if (typeof v === 'string') return 'string';
    if (typeof v === 'number') return 'number';
    if (typeof v === 'boolean') return 'bool';
    return 'unknown';
  }

  function deepEqual(a, b) {
    return JSON.stringify(a) === JSON.stringify(b);
  }

  function checkExpect(fixture, resp) {
    const expect = fixture.expect;
    const failures = [];

    // status
    if (expect.status) {
      switch (expect.status) {
        case 'ok':
          if (!resp.ok) failures.push(`status=ok expected but got error: ${resp.error}`);
          break;
        case 'hit':
          if (!resp.ok) failures.push(`status=hit expected but got error: ${resp.error}`);
          else if (resp.data === null) failures.push('status=hit expected but data was absent');
          break;
        case 'miss':
          if (!resp.ok) failures.push(`status=miss expected but got error: ${resp.error}`);
          else if (resp.data !== null) failures.push('status=miss expected but data was present');
          break;
        case 'error':
          if (resp.ok) failures.push('status=error expected but response was ok');
          break;
        default:
          failures.push(`unknown status: ${expect.status}`);
      }
    }

    // data_type
    if (expect.data_type !== undefined) {
      const actual = jsonTypeOf(resp.data);
      if (actual !== expect.data_type) {
        failures.push(`data_type=${expect.data_type} expected but got ${actual}: data=${JSON.stringify(resp.data)}`);
      }
    }

    // data_equals
    if (expect.data_equals !== undefined) {
      if (!deepEqual(resp.data, expect.data_equals)) {
        failures.push(`data_equals failed: expected ${JSON.stringify(expect.data_equals)}, got ${JSON.stringify(resp.data)}`);
      }
    }

    // data_as_text
    if (expect.data_as_text !== undefined) {
      const actual = resp.dataAsText ?? '';
      if (actual !== expect.data_as_text) {
        failures.push(`data_as_text=${JSON.stringify(expect.data_as_text)} expected but got ${JSON.stringify(actual)}`);
      }
    }

    // data_contains_field
    if (expect.data_contains_field !== undefined) {
      if (
        resp.data === null ||
        typeof resp.data !== 'object' ||
        Array.isArray(resp.data) ||
        !(expect.data_contains_field in resp.data)
      ) {
        failures.push(`data_contains_field=${expect.data_contains_field} failed: data=${JSON.stringify(resp.data)}`);
      }
    }

    // data_field_equals
    if (expect.data_field_equals !== undefined) {
      const { field, value } = expect.data_field_equals;
      if (
        resp.data === null ||
        typeof resp.data !== 'object' ||
        Array.isArray(resp.data)
      ) {
        failures.push(`data_field_equals: data is not an object: ${JSON.stringify(resp.data)}`);
      } else if (!deepEqual(resp.data[field], value)) {
        failures.push(`data_field_equals failed for ${field}: expected ${JSON.stringify(value)}, got ${JSON.stringify(resp.data[field])}`);
      }
    }

    // age_ms_present
    if (expect.age_ms_present !== undefined) {
      const actual = resp.ageMs !== null;
      if (actual !== expect.age_ms_present) {
        failures.push(`age_ms_present=${expect.age_ms_present} expected but got ${actual}`);
      }
    }

    // stale
    if (expect.stale !== undefined) {
      if (resp.stale !== expect.stale) {
        failures.push(`stale=${expect.stale} expected but got ${resp.stale}`);
      }
    }

    // error_contains
    if (expect.error_contains !== undefined) {
      const actual = resp.error ?? '';
      if (!actual.includes(expect.error_contains)) {
        failures.push(`error_contains=${JSON.stringify(expect.error_contains)} expected but error was ${JSON.stringify(actual)}`);
      }
    }

    return failures;
  }

  // ---------------------------------------------------------------------------
  // Main runner
  // ---------------------------------------------------------------------------

  async function main() {
    const fixtures = discoverFixtures();
    if (fixtures.length === 0) {
      console.error(`ERROR: no fixtures found under ${CONFORMANCE_DIR}`);
      process.exit(1);
    }

    console.log(`Found ${fixtures.length} conformance fixture(s).`);
    console.log(`Using COMB_BIN: ${COMB_BIN}\n`);

    const failures = [];
    let passed = 0;

    for (const fixture of fixtures) {
      let daemon = null;
      try {
        daemon = await spawnDaemon();
        const client = new Client({ socketPath: daemon.sockPath, timeoutMs: 5000 });

        // Run setup ops, ignore their responses.
        for (const setupOp of fixture.setup) {
          await runOp(client, setupOp);
        }

        // Run the test op and check expectations.
        const resp = await runOp(client, fixture.test);
        const expectFailures = checkExpect(fixture, resp);

        if (expectFailures.length > 0) {
          failures.push({
            name: fixture.name,
            description: fixture.description,
            sourcePath: fixture.sourcePath,
            reasons: expectFailures,
          });
          console.log(`  FAIL [${fixture.name}]`);
          for (const reason of expectFailures) {
            console.log(`       ${reason}`);
          }
        } else {
          passed++;
          console.log(`  pass [${fixture.name}]`);
        }
      } catch (err) {
        failures.push({
          name: fixture.name,
          description: fixture.description,
          sourcePath: fixture.sourcePath,
          reasons: [`unexpected exception: ${err instanceof Error ? err.message : String(err)}`],
        });
        console.log(`  FAIL [${fixture.name}] unexpected exception: ${err instanceof Error ? err.message : String(err)}`);
      } finally {
        if (daemon) daemon.stop();
      }
    }

    console.log(`\nResults: ${passed}/${fixtures.length} passed.`);

    if (failures.length > 0) {
      console.error(`\n${failures.length} conformance failure(s):`);
      for (const f of failures) {
        console.error(`\n  [${f.name}] ${f.description}`);
        console.error(`  path: ${f.sourcePath}`);
        for (const reason of f.reasons) {
          console.error(`  - ${reason}`);
        }
      }
      process.exit(1);
    }

    console.log('All conformance fixtures passed.');
    process.exit(0);
  }

  main().catch((err) => {
    console.error(`FATAL: ${err instanceof Error ? err.stack : String(err)}`);
    process.exit(1);
  });
})();
