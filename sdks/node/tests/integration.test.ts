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

  it('get on an invalid facet key throws ServerError with kind server_error', async () => {
    await assert.rejects(
      () => client.get('a:b:c.x', '/tmp'),
      (err: unknown) => {
        if (!(err instanceof ServerError)) return false;
        assert.equal(err.kind, 'server_error');
        assert.match(err.message, /invalid facet key/);
        return true;
      },
    );
  });

  it('putNull clears a cached value', async () => {
    await client.put('itest_null', { x: 1 }, { path: '/tmp' });
    await client.putNull('itest_null', '/tmp');
    const result = await client.get('itest_null.x', '/tmp');
    assert.equal(result.isMiss, true);
    assert.equal(result.template, undefined);
  });

  it('put(key, null) removes the node', async () => {
    await client.put('itest_putnull', { x: 1 }, { path: '/tmp' });
    await client.put('itest_putnull', null, { path: '/tmp' });
    const result = await client.get('itest_putnull.x', '/tmp');
    assert.equal(result.isMiss, true);
  });

  // The template half (design §3.4): get returns both halves, unrendered.
  it('get on a template-only node is neither a hit nor a miss', async () => {
    await client.put('itest_tpl1:template', { t: '{{ 1 }}' });
    const result = await client.get('itest_tpl1.t');
    assert.equal(result.template, '{{ 1 }}');
    assert.equal(result.data, undefined);
    assert.equal(result.isHit, false);
    assert.equal(result.isMiss, false);
  });

  it('get carries both halves when a node has data and a template', async () => {
    await client.put('itest_tpl2', { v: 1 });
    await client.put('itest_tpl2:template', { v: '{{ 2 }}' });
    const result = await client.get('itest_tpl2.v');
    assert.equal(result.isHit, true);
    assert.equal(result.isMiss, false);
    assert.equal(result.data, 1);
    assert.equal(result.template, '{{ 2 }}');
    assert.equal(result.rest, undefined);
  });

  it('get past a template string carries the leftover address as rest', async () => {
    await client.put('itest_tpl3:template', { t: '{{ 1 }}' });
    const result = await client.get('itest_tpl3.t.x');
    assert.equal(result.template, '{{ 1 }}');
    assert.equal(result.rest, 'x');
  });

  it('a plain hit carries no template half', async () => {
    await client.put('itest_tpl4', { v: 'plain' });
    const result = await client.get('itest_tpl4.v');
    assert.equal(result.isHit, true);
    assert.equal(result.template, undefined);
    assert.equal(result.rest, undefined);
  });

  it("refresh of a static namespace is the daemon's refusal, in its own words", async () => {
    await client.put('itest_static', { v: 1 }, { path: '/tmp' });
    // `serverMessage` is the daemon's words alone — the same text `comb
    // refresh` prints and the subprocess tier reports — with no `server
    // error: ` prefix from the envelope.
    await assert.rejects(
      () => client.refresh('itest_static', '/tmp'),
      (err: unknown) =>
        err instanceof ServerError &&
        err.serverMessage.startsWith('nothing to refresh: itest_static has no sources'),
    );
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

  it('resolve renders a template the daemon holds', async () => {
    // The daemon holds the template; the client renders it with the env the
    // caller passes — the same value `comb get filters.itest_based` prints.
    await client.put('filters:template', { itest_based: '{{ env.ITEST_PYVAR | basename }}' });
    const value = await client.resolve('filters.itest_based', {
      cwd: '/tmp',
      env: { ITEST_PYVAR: '/foo/bar/baz' },
    });
    assert.equal(value, 'baz');
  });

  it('resolve on a bare provider is the whole merged namespace', async () => {
    await client.put('itest_ns', { lit: 1 });
    await client.put('itest_ns:template', { t: '{{ env.ITEST_NS }}' });
    const value = await client.resolve('itest_ns', { cwd: '/tmp', env: { ITEST_NS: 'hi' } });
    assert.deepEqual(value, { lit: 1, t: 'hi' });
  });

  it('eval renders a template stored nowhere', async () => {
    const value = await client.eval("{{ 'x-' ~ env.FOO }}", { cwd: '/tmp', env: { FOO: 'bar' } });
    assert.equal(value, 'x-bar');
  });

  it('exec() is disabled on a client without the option', async () => {
    await client.put('itest_exec:template', { out: "{{ exec('printf', 'x') }}" });
    await assert.rejects(
      () => client.resolve('itest_exec.out', { cwd: '/tmp' }),
      (err: unknown) => {
        if (!(err instanceof ServerError)) return false;
        assert.match(err.message, /exec\(\) is disabled in this context/);
        return true;
      },
    );
  });

  it('exec() runs on a client with the option', async () => {
    await client.put('itest_exec2:template', { out: "{{ exec('printf', 'x') }}" });
    const execClient = new Client({ socketPath: daemon.sockPath, timeoutMs: 5000, exec: true });
    try {
      assert.equal(await execClient.resolve('itest_exec2.out', { cwd: '/tmp' }), 'x');
    } finally {
      execClient.close();
    }
  });

  it('watch yields the current value as its first event', async () => {
    await client.put('itest_watch', { count: 7 }, { path: '/tmp' });
    const stream = await client.watch('itest_watch.count', { cwd: '/tmp' });
    const iterator = stream[Symbol.asyncIterator]();
    const { value, done } = await iterator.next();
    assert.equal(done, false);
    assert.equal(value?.data, 7);
    stream.close();
  });

  it('watch yields the resolved value and refires on a template edit', async () => {
    // The event is what `resolve` answers for the same key, cwd and env,
    // and the template the value is rendered from is watched alongside the
    // addressed key.
    const env = { NODE_WATCH_TPL: 'v1' };
    await client.put('nodewatchtpl:template', { f: '{{ env.NODE_WATCH_TPL }}' });
    const resolved = await client.resolve('nodewatchtpl.f', { cwd: '/tmp', env });

    const stream = await client.watch('nodewatchtpl.f', { cwd: '/tmp', env });
    const iterator = stream[Symbol.asyncIterator]();
    const first = await iterator.next();
    assert.equal(first.value?.data, resolved);

    await client.put('nodewatchtpl:template', { f: '{{ env.NODE_WATCH_TPL }}!' });
    const second = await iterator.next();
    assert.equal(second.value?.data, 'v1!');
    stream.close();
  });
});
