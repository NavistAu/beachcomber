import './helpers.js'; // side effect: defaults COMB_BIN/BEACHCOMBER_LIB to local builds when present
import { describe, it, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { daemonAvailable, spawnDaemon, type TestDaemon } from './helpers.js';
import { makeCombResult, type GetResultShape } from '../src/client.js';
import { unwrap } from '../src/envelope.js';
import { ServerError } from '../src/errors.js';
import { createSubprocessTransport } from '../src/subprocess_transport.js';
import type { ClientHandle, Transport } from '../src/transport.js';

const skip = !daemonAvailable();

// `comb get` stopped printing the daemon envelope when it started resolving
// (it prints the rendered value), so the subprocess tier reads the raw node
// from `comb status -f json` rows. These drive the transport directly and
// unwrap its envelope exactly as `Client.get` does.
describe('subprocess transport get against a real daemon', { skip }, () => {
  let daemon: TestDaemon;
  let tp: Transport;
  let handle: ClientHandle;

  async function put(key: string, data: unknown, path?: string): Promise<void> {
    unwrap(await tp.put(handle, key, JSON.stringify(data), undefined, path));
  }

  async function get(key: string, path?: string) {
    return makeCombResult(unwrap(await tp.get(handle, key, path, 0)) as GetResultShape);
  }

  before(async () => {
    daemon = await spawnDaemon();
    tp = createSubprocessTransport();
    handle = tp.newClient({ socketPath: daemon.sockPath, timeoutMs: 5000 });
    await put('sp', { n: 1, s: 'str', o: { k: 'v' } }, '/tmp');
    await put('sp:template', { t: '{{ 1 }}' }, '/tmp');
    await put('spg', { g: 1 });
  });

  after(() => {
    tp.freeClient(handle);
    daemon.stop();
  });

  it('a scalar field is a hit with the raw value', async () => {
    const r = await get('sp.n', '/tmp');
    assert.equal(r.isHit, true);
    assert.equal(r.data, 1);
    assert.equal(typeof r.ageMs, 'number');
  });

  it('a string field is a hit with the raw string', async () => {
    const r = await get('sp.s', '/tmp');
    assert.equal(r.isHit, true);
    assert.equal(r.data, 'str');
  });

  it('a bare provider is every field merged into one object', async () => {
    const r = await get('sp', '/tmp');
    assert.equal(r.isHit, true);
    assert.deepEqual(r.data, { n: 1, s: 'str', o: { k: 'v' } });
  });

  it('a dotted address drills into the field value', async () => {
    const r = await get('sp.o.k', '/tmp');
    assert.equal(r.isHit, true);
    assert.equal(r.data, 'v');
  });

  it('an absent field is a miss, not a throw', async () => {
    const r = await get('sp.nope', '/tmp');
    assert.equal(r.isMiss, true);
    assert.equal(r.isHit, false);
  });

  it('a daemon rejection is a ServerError in the daemon\'s words', async () => {
    await assert.rejects(
      () => get('a:b:c.x', '/tmp'),
      (err: unknown) => err instanceof ServerError && /invalid facet key/.test(err.message),
    );
  });

  it('a template-only node reads as a miss: this tier cannot see the template half', async () => {
    const r = await get('sp.t', '/tmp');
    assert.equal(r.isMiss, true);
    assert.equal(r.template, undefined);
    assert.equal(r.rest, undefined);
  });

  it('no path reads the global entry', async () => {
    const r = await get('spg.g');
    assert.equal(r.isHit, true);
    assert.equal(r.data, 1);
  });
});
