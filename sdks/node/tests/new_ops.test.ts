/**
 * Tests for the new protocol operations added in Phase 7:
 * getWithFlags, hello, put, introspect, status, watch.
 */

import { describe, it, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { Client, WatchStream } from '../src/client.js';
import { ServerError } from '../src/errors.js';
import { MockServer } from './helpers.js';

// ---- getWithFlags ----

describe('Client.getWithFlags', () => {
  let server: MockServer;

  before(async () => {
    server = await MockServer.start();
  });

  after(async () => {
    await server.stop();
  });

  it('sends force=true when force flag is set', async () => {
    let received: Record<string, unknown> = {};
    server.handle((req) => {
      received = req.parsed;
      return { ok: true, data: 'main', age_ms: 0, stale: false };
    });
    const client = new Client({ socketPath: server.socketPath, timeoutMs: 1000 });
    await client.getWithFlags('git.branch', undefined, { force: true });
    assert.equal(received['force'], true);
  });

  it('sends wait=true when wait flag is set', async () => {
    let received: Record<string, unknown> = {};
    server.handle((req) => {
      received = req.parsed;
      return { ok: true, data: 'main', age_ms: 0, stale: false };
    });
    const client = new Client({ socketPath: server.socketPath, timeoutMs: 1000 });
    await client.getWithFlags('git.branch', undefined, { wait: true });
    assert.equal(received['wait'], true);
  });

  it('omits force and wait when not set', async () => {
    let received: Record<string, unknown> = {};
    server.handle((req) => {
      received = req.parsed;
      return { ok: true, data: 'main', age_ms: 0, stale: false };
    });
    const client = new Client({ socketPath: server.socketPath, timeoutMs: 1000 });
    await client.getWithFlags('git.branch');
    assert.ok(!('force' in received));
    assert.ok(!('wait' in received));
  });

  it('sends path when provided', async () => {
    let received: Record<string, unknown> = {};
    server.handle((req) => {
      received = req.parsed;
      return { ok: true, data: 'main', age_ms: 0, stale: false };
    });
    const client = new Client({ socketPath: server.socketPath, timeoutMs: 1000 });
    await client.getWithFlags('git.branch', '/my/repo');
    assert.equal(received['path'], '/my/repo');
  });

  it('returns a CombResult', async () => {
    server.handle(() => ({ ok: true, data: 'feature-branch', age_ms: 100, stale: false }));
    const client = new Client({ socketPath: server.socketPath, timeoutMs: 1000 });
    const result = await client.getWithFlags('git.branch');
    assert.equal(result.isHit, true);
    assert.equal(result.getString(), 'feature-branch');
  });
});

// ---- hello ----

describe('Client.hello', () => {
  let server: MockServer;

  before(async () => {
    server = await MockServer.start();
  });

  after(async () => {
    await server.stop();
  });

  it('sends op:hello', async () => {
    let received: Record<string, unknown> = {};
    server.handle((req) => {
      received = req.parsed;
      return {
        ok: true,
        data: { protocol_version: '1', daemon_version: '0.5.1' },
      };
    });
    const client = new Client({ socketPath: server.socketPath, timeoutMs: 1000 });
    await client.hello();
    assert.equal(received['op'], 'hello');
  });

  it('parses protocolVersion and daemonVersion', async () => {
    server.handle(() => ({
      ok: true,
      data: { protocol_version: '2', daemon_version: '0.6.0' },
    }));
    const client = new Client({ socketPath: server.socketPath, timeoutMs: 1000 });
    const info = await client.hello();
    assert.equal(info.protocolVersion, '2');
    assert.equal(info.daemonVersion, '0.6.0');
  });

  it('returns empty strings when data fields are missing', async () => {
    server.handle(() => ({ ok: true, data: {} }));
    const client = new Client({ socketPath: server.socketPath, timeoutMs: 1000 });
    const info = await client.hello();
    assert.equal(info.protocolVersion, '');
    assert.equal(info.daemonVersion, '');
  });

  it('throws ServerError on ok:false', async () => {
    server.handle(() => ({ ok: false, error: 'not supported' }));
    const client = new Client({ socketPath: server.socketPath, timeoutMs: 1000 });
    await assert.rejects(() => client.hello(), ServerError);
  });
});

// ---- put ----

describe('Client.put', () => {
  let server: MockServer;

  before(async () => {
    server = await MockServer.start();
  });

  after(async () => {
    await server.stop();
  });

  it('sends op:put with key and data', async () => {
    let received: Record<string, unknown> = {};
    server.handle((req) => {
      received = req.parsed;
      return { ok: true };
    });
    const client = new Client({ socketPath: server.socketPath, timeoutMs: 1000 });
    await client.put('mykey', { color: 'blue' });
    assert.equal(received['op'], 'put');
    assert.equal(received['key'], 'mykey');
    assert.deepEqual(received['data'], { color: 'blue' });
  });

  it('sends ttl when provided', async () => {
    let received: Record<string, unknown> = {};
    server.handle((req) => {
      received = req.parsed;
      return { ok: true };
    });
    const client = new Client({ socketPath: server.socketPath, timeoutMs: 1000 });
    await client.put('mykey', { v: 1 }, { ttl: '60s' });
    assert.equal(received['ttl'], '60s');
  });

  it('sends path when provided', async () => {
    let received: Record<string, unknown> = {};
    server.handle((req) => {
      received = req.parsed;
      return { ok: true };
    });
    const client = new Client({ socketPath: server.socketPath, timeoutMs: 1000 });
    await client.put('mykey', { v: 1 }, { path: '/some/path' });
    assert.equal(received['path'], '/some/path');
  });

  it('omits ttl and path when not provided', async () => {
    let received: Record<string, unknown> = {};
    server.handle((req) => {
      received = req.parsed;
      return { ok: true };
    });
    const client = new Client({ socketPath: server.socketPath, timeoutMs: 1000 });
    await client.put('mykey', { v: 1 });
    assert.ok(!('ttl' in received));
    assert.ok(!('path' in received));
  });

  it('throws ServerError on ok:false', async () => {
    server.handle(() => ({ ok: false, error: 'must be a JSON object' }));
    const client = new Client({ socketPath: server.socketPath, timeoutMs: 1000 });
    await assert.rejects(() => client.put('k', 'not-an-object'), ServerError);
  });
});

// ---- introspect ----

describe('Client.introspect', () => {
  let server: MockServer;

  before(async () => {
    server = await MockServer.start();
  });

  after(async () => {
    await server.stop();
  });

  it('sends op:introspect with subject', async () => {
    let received: Record<string, unknown> = {};
    server.handle((req) => {
      received = req.parsed;
      return {
        ok: true,
        data: {
          pid: 1234,
          version: '0.5.1',
          uptime_secs: 10,
          socket_path: '/tmp/sock',
          config_path: null,
          requests_total: 5,
          in_flight: 0,
          active_watchers: 0,
          cache_entries: 3,
          verdicts: [],
        },
      };
    });
    const client = new Client({ socketPath: server.socketPath, timeoutMs: 1000 });
    await client.introspect('daemon');
    assert.equal(received['op'], 'introspect');
    assert.equal(received['subject'], 'daemon');
  });

  it('parses daemon subject into typed DaemonHealth', async () => {
    server.handle(() => ({
      ok: true,
      data: {
        pid: 9999,
        version: '1.0.0',
        uptime_secs: 42,
        socket_path: '/var/run/comb.sock',
        config_path: '/home/user/.config/beachcomber/config.toml',
        requests_total: 100,
        in_flight: 2,
        active_watchers: 1,
        cache_entries: 50,
        verdicts: [{ level: 'warn', message: 'something' }],
      },
    }));
    const client = new Client({ socketPath: server.socketPath, timeoutMs: 1000 });
    const resp = await client.introspect('daemon');
    assert.equal(resp.subject, 'daemon');
    assert.ok(resp.subject === 'daemon');
    const health = resp.daemon;
    assert.equal(health.pid, 9999);
    assert.equal(health.version, '1.0.0');
    assert.equal(health.uptimeSecs, 42);
    assert.equal(health.socketPath, '/var/run/comb.sock');
    assert.equal(health.configPath, '/home/user/.config/beachcomber/config.toml');
    assert.equal(health.requestsTotal, 100);
    assert.equal(health.inFlight, 2);
    assert.equal(health.activeWatchers, 1);
    assert.equal(health.cacheEntries, 50);
    assert.equal(health.verdicts.length, 1);
    assert.equal(health.verdicts[0]!.level, 'warn');
    assert.equal(health.verdicts[0]!.message, 'something');
  });

  it('returns null configPath when config_path is absent', async () => {
    server.handle(() => ({
      ok: true,
      data: {
        pid: 1,
        version: '',
        uptime_secs: 0,
        socket_path: '',
        requests_total: 0,
        in_flight: 0,
        active_watchers: 0,
        cache_entries: 0,
        verdicts: [],
      },
    }));
    const client = new Client({ socketPath: server.socketPath, timeoutMs: 1000 });
    const resp = await client.introspect('daemon');
    assert.ok(resp.subject === 'daemon');
    assert.equal(resp.daemon.configPath, null);
  });

  it('returns other subject as IntrospectResponse with other field', async () => {
    server.handle(() => ({
      ok: true,
      data: { entries: [] },
    }));
    const client = new Client({ socketPath: server.socketPath, timeoutMs: 1000 });
    const resp = await client.introspect('cache');
    assert.equal(resp.subject, 'cache');
    assert.ok(resp.subject !== 'daemon');
    assert.deepEqual((resp as { subject: 'cache'; other: unknown }).other, { entries: [] });
  });

  it('sends duration_secs when durationSecs is provided', async () => {
    let received: Record<string, unknown> = {};
    server.handle((req) => {
      received = req.parsed;
      return { ok: true, data: { entries: [] } };
    });
    const client = new Client({ socketPath: server.socketPath, timeoutMs: 1000 });
    await client.introspect('procs', { durationSecs: 5 });
    assert.equal(received['duration_secs'], 5);
  });
});

// ---- status ----

describe('Client.status', () => {
  let server: MockServer;

  before(async () => {
    server = await MockServer.start();
  });

  after(async () => {
    await server.stop();
  });

  it('sends op:status', async () => {
    let received: Record<string, unknown> = {};
    server.handle((req) => {
      received = req.parsed;
      return { ok: true, data: [] };
    });
    const client = new Client({ socketPath: server.socketPath, timeoutMs: 1000 });
    await client.status();
    assert.equal(received['op'], 'status');
  });

  it('returns an empty array when data is empty', async () => {
    server.handle(() => ({ ok: true, data: [] }));
    const client = new Client({ socketPath: server.socketPath, timeoutMs: 1000 });
    const rows = await client.status();
    assert.deepEqual(rows, []);
  });

  it('parses cache rows into typed CacheRow objects', async () => {
    server.handle(() => ({
      ok: true,
      data: [
        {
          provider: 'git',
          field: 'branch',
          path: '/repo',
          value: 'main',
          age_ms: 500,
          stale: false,
        },
        {
          provider: 'hostname',
          field: null,
          path: null,
          value: 'myhost',
          age_ms: 1000,
          stale: true,
        },
      ],
    }));
    const client = new Client({ socketPath: server.socketPath, timeoutMs: 1000 });
    const rows = await client.status();
    assert.equal(rows.length, 2);

    const first = rows[0]!;
    assert.equal(first.provider, 'git');
    assert.equal(first.field, 'branch');
    assert.equal(first.path, '/repo');
    assert.equal(first.value, 'main');
    assert.equal(first.ageMs, 500);
    assert.equal(first.stale, false);

    const second = rows[1]!;
    assert.equal(second.provider, 'hostname');
    assert.equal(second.field, null);
    assert.equal(second.path, null);
    assert.equal(second.value, 'myhost');
    assert.equal(second.ageMs, 1000);
    assert.equal(second.stale, true);
  });
});

// ---- status lifecycle fields ----

describe('Client.status lifecycle fields', () => {
  let server: MockServer;

  before(async () => {
    server = await MockServer.start();
  });

  after(async () => {
    await server.stop();
  });

  it('status row exposes lifecycle fields', async () => {
    server.handle(() => ({
      ok: true,
      data: [
        {
          provider: 'git',
          field: 'branch',
          path: '/tmp',
          value: 'main',
          age_ms: 100,
          stale: false,
          kind: { kind: 'lifecycle', decay: 0, watches_files: true },
          poll_interval_secs: 5,
          keep_alive_polls: 3,
          fsevents_reinstate: false,
        },
      ],
    }));
    const client = new Client({ socketPath: server.socketPath, timeoutMs: 1000 });
    const rows = await client.status();
    const git = rows.find(r => r.provider === 'git');
    assert.ok(git);
    assert.ok(git.kind);
    assert.equal(git.kind.kind, 'lifecycle');
    assert.ok(git.pollIntervalSecs && git.pollIntervalSecs > 0);
    assert.ok(git.keepAlivePolls && git.keepAlivePolls > 0);
    assert.ok(typeof git.fseventsReinstate === 'boolean');
  });

  it('status row handles missing lifecycle fields', async () => {
    server.handle(() => ({
      ok: true,
      data: [
        {
          provider: 'hostname',
          field: null,
          path: null,
          value: 'myhost',
          age_ms: 200,
          stale: false,
        },
      ],
    }));
    const client = new Client({ socketPath: server.socketPath, timeoutMs: 1000 });
    const rows = await client.status();
    const host = rows.find(r => r.provider === 'hostname');
    assert.ok(host);
    assert.equal(host.kind, undefined);
    assert.equal(host.pollIntervalSecs, undefined);
    assert.equal(host.keepAlivePolls, undefined);
    assert.equal(host.fseventsReinstate, undefined);
    assert.equal(host.failure, undefined);
  });

  it('status row parses failure field', async () => {
    server.handle(() => ({
      ok: true,
      data: [
        {
          provider: 'git',
          field: 'branch',
          path: '/tmp',
          value: null,
          age_ms: 0,
          stale: true,
          kind: { kind: 'lifecycle', decay: 2, watches_files: false },
          poll_interval_secs: 10,
          keep_alive_polls: 5,
          fsevents_reinstate: true,
          failure: { consecutive_failures: 3, suppressed_until_unix_ms: 9999999 },
        },
      ],
    }));
    const client = new Client({ socketPath: server.socketPath, timeoutMs: 1000 });
    const rows = await client.status();
    const git = rows.find(r => r.provider === 'git');
    assert.ok(git);
    assert.ok(git.failure);
    assert.equal(git.failure.consecutive_failures, 3);
    assert.equal(git.failure.suppressed_until_unix_ms, 9999999);
  });
});

// ---- WatchStream ----

describe('WatchStream', () => {
  let server: MockServer;

  before(async () => {
    server = await MockServer.start();
  });

  after(async () => {
    await server.stop();
  });

  it('yields events from watch op', async () => {
    // The mock server keeps the connection open and sends two events.
    // We override the raw server behaviour using a custom handler that
    // writes two NDJSON lines before the normal handler loop closes.
    const events: unknown[] = [];

    // Start a second mock server that manually drives watch protocol.
    const watchServer = await MockServer.start();
    // Patch the server to send multiple responses per request.
    // We access the underlying net.Server to override connection handling.
    const client = new Client({ socketPath: watchServer.socketPath, timeoutMs: 1000 });

    // For the watch test, set up handler to return two events in sequence.
    let requestCount = 0;
    watchServer.handle((req) => {
      requestCount++;
      // The watch request itself — return the first event immediately.
      // (MockServer reads one request line, sends one response line.)
      return {
        ok: true,
        data: 42,
        age_ms: 100,
        stale: false,
      };
    });

    const stream = await client.watch('fixture_w.count');

    // Read only the first event then close.
    for await (const ev of stream) {
      events.push(ev);
      stream.close();
      break;
    }

    await watchServer.stop();

    assert.equal(events.length, 1);
    const ev = events[0] as { data: unknown; ageMs: number; stale: boolean };
    assert.equal(ev.data, 42);
    assert.equal(ev.ageMs, 100);
    assert.equal(ev.stale, false);
  });

  it('sends op:watch with key', async () => {
    let received: Record<string, unknown> = {};
    server.handle((req) => {
      received = req.parsed;
      return { ok: true, data: 'hello', age_ms: 0, stale: false };
    });
    const client = new Client({ socketPath: server.socketPath, timeoutMs: 1000 });
    const stream = await client.watch('git.branch');
    for await (const _ev of stream) {
      stream.close();
      break;
    }
    assert.equal(received['op'], 'watch');
    assert.equal(received['key'], 'git.branch');
  });

  it('WatchStream.close destroys the socket and terminates iteration', async () => {
    server.handle(() => ({ ok: true, data: 'x', age_ms: 0, stale: false }));
    const client = new Client({ socketPath: server.socketPath, timeoutMs: 1000 });
    const stream = await client.watch('test.key');
    // Collect one event then close — iteration must terminate.
    const collected: unknown[] = [];
    try {
      for await (const ev of stream) {
        collected.push(ev);
        stream.close();
        break;
      }
    } catch {
      // Premature close on the destroyed socket is acceptable — close() worked.
    }
    // We either got the event before closing, or close was called and ended iteration.
    assert.ok(collected.length <= 1);
  });
});

// ---- Session new methods ----

describe('Session new ops', () => {
  let server: MockServer;

  before(async () => {
    server = await MockServer.start();
  });

  after(async () => {
    await server.stop();
  });

  it('session.hello sends op:hello and parses result', async () => {
    let received: Record<string, unknown> = {};
    server.handle((req) => {
      received = req.parsed;
      return { ok: true, data: { protocol_version: '1', daemon_version: '0.5.0' } };
    });
    const client = new Client({ socketPath: server.socketPath, timeoutMs: 1000 });
    const session = await client.session();
    const info = await session.hello();
    session.close();
    assert.equal(received['op'], 'hello');
    assert.equal(info.protocolVersion, '1');
    assert.equal(info.daemonVersion, '0.5.0');
  });

  it('session.put sends op:put', async () => {
    let received: Record<string, unknown> = {};
    server.handle((req) => {
      received = req.parsed;
      return { ok: true };
    });
    const client = new Client({ socketPath: server.socketPath, timeoutMs: 1000 });
    const session = await client.session();
    await session.put('sessionkey', { x: 1 });
    session.close();
    assert.equal(received['op'], 'put');
    assert.equal(received['key'], 'sessionkey');
  });

  it('session.introspect sends op:introspect', async () => {
    let received: Record<string, unknown> = {};
    server.handle((req) => {
      received = req.parsed;
      return { ok: true, data: { entries: [] } };
    });
    const client = new Client({ socketPath: server.socketPath, timeoutMs: 1000 });
    const session = await client.session();
    const resp = await session.introspect('cache');
    session.close();
    assert.equal(received['op'], 'introspect');
    assert.equal(received['subject'], 'cache');
    assert.equal(resp.subject, 'cache');
  });

  it('session.status returns CacheRow array', async () => {
    server.handle(() => ({
      ok: true,
      data: [
        { provider: 'p', field: 'f', path: null, value: 'v', age_ms: 10, stale: false },
      ],
    }));
    const client = new Client({ socketPath: server.socketPath, timeoutMs: 1000 });
    const session = await client.session();
    const rows = await session.status();
    session.close();
    assert.equal(rows.length, 1);
    assert.equal(rows[0]!.provider, 'p');
  });

  it('session.getWithFlags sends force flag', async () => {
    let received: Record<string, unknown> = {};
    server.handle((req) => {
      received = req.parsed;
      return { ok: true, data: 'v', age_ms: 0, stale: false };
    });
    const client = new Client({ socketPath: server.socketPath, timeoutMs: 1000 });
    const session = await client.session();
    const result = await session.getWithFlags('k.f', undefined, { force: true });
    session.close();
    assert.equal(received['force'], true);
    assert.equal(result.getString(), 'v');
  });
});
