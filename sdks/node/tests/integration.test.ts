import './helpers.js'; // side effect: defaults BEACHCOMBER_LIB to a locally built dylib/so when present
import { describe, it, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { daemonAvailable, spawnDaemon, type TestDaemon } from './helpers.js';
import { Client } from '../src/client.js';
import { ServerError } from '../src/errors.js';

const skip = !daemonAvailable();

describe('Client against a real daemon (FFI transport)', { skip }, () => {
  let daemon: TestDaemon;
  let client: Client;

  before(async () => {
    daemon = await spawnDaemon();
    client = new Client({ socketPath: daemon.sockPath, timeoutMs: 5000 });
  });

  after(() => {
    client.close();
    daemon.stop();
  });

  it('reports the ffi transport', () => {
    assert.equal(client.transport(), 'ffi');
  });

  it('put then get round-trips a value as a hit', async () => {
    await client.put('itest_v', { greeting: 'hi' }, { path: '/tmp' });
    const result = await client.get('itest_v.greeting', '/tmp');
    assert.equal(result.isHit, true);
    assert.equal(result.data, 'hi');
    assert.equal(typeof result.ageMs, 'number');
  });

  it('get on an unknown provider throws ServerError with kind server_error', async () => {
    await assert.rejects(
      () => client.get('definitely_nonexistent_xyz.field', '/tmp'),
      (err: unknown) => {
        if (!(err instanceof ServerError)) return false;
        assert.equal(err.kind, 'server_error');
        assert.match(err.message, /unknown provider/);
        return true;
      },
    );
  });

  it('putNull clears a cached value', async () => {
    await client.put('itest_null', { x: 1 }, { path: '/tmp' });
    await client.putNull('itest_null', '/tmp');
    const result = await client.get('itest_null.x', '/tmp');
    assert.equal(result.isMiss, true);
  });

  it('status returns an array of cache rows', async () => {
    await client.put('itest_status', { k: 'v' }, { path: '/tmp' });
    const rows = await client.status();
    assert.ok(Array.isArray(rows));
    assert.ok(rows.some((r) => r.provider === 'itest_status'));
  });

  it('hello returns protocol and daemon version', async () => {
    const info = await client.hello();
    assert.ok(info.protocolVersion.length > 0);
    assert.ok(info.daemonVersion.length > 0);
  });

  it('introspect{daemon} returns pid and version', async () => {
    const resp = await client.introspect('daemon');
    assert.equal(resp.subject, 'daemon');
    if (resp.subject === 'daemon') {
      assert.ok(resp.daemon.pid > 0);
      assert.ok(resp.daemon.version.length > 0);
    }
  });

  it('session get/put/setContext works over one connection', async () => {
    const session = await client.session();
    try {
      await session.setContext('/tmp');
      // The daemon's `put` op does not consult connection context (only
      // `get`/`refresh` do) — an explicit path is required here regardless.
      await session.put('itest_session', { n: 42 }, { path: '/tmp' });
      // `get` omits `path` and picks up '/tmp' from setContext above.
      const result = await session.get('itest_session.n');
      assert.equal(result.isHit, true);
      assert.equal(result.data, 42);
    } finally {
      session.close();
    }
  });

  it('resolve evaluates a virtual field with the basename filter', async () => {
    const value = await client.resolve('filters.itest_based', {
      cwd: '/tmp',
      env: { ITEST_PYVAR: '/foo/bar/baz' },
      overrides: { 'filters.itest_based': 'env.ITEST_PYVAR | basename' },
    });
    assert.equal(value, 'baz');
  });

  it('watch yields the current value as its first event', async () => {
    await client.put('itest_watch', { count: 7 }, { path: '/tmp' });
    const stream = await client.watch('itest_watch.count', '/tmp');
    const iterator = stream[Symbol.asyncIterator]();
    const { value, done } = await iterator.next();
    assert.equal(done, false);
    assert.equal(value?.data, 7);
    stream.close();
  });
});
