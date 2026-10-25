import './helpers.js'; // side effect: defaults COMB_BIN/BEACHCOMBER_LIB to local builds when present
import { describe, it, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { daemonAvailable, spawnDaemon, type TestDaemon } from './helpers.js';
import { unwrap } from '../src/envelope.js';
import { ServerError } from '../src/errors.js';
import { createSubprocessTransport } from '../src/subprocess_transport.js';
import type { ClientHandle, Transport } from '../src/transport.js';

const skip = !daemonAvailable();

// The subprocess tier's `eval` is one `comb eval -f json`, so it answers what
// `comb eval` prints, with a number kept a number, and the call's env map is
// the template's environment as for `get`.
describe('subprocess transport eval against a real daemon', { skip }, () => {
  let daemon: TestDaemon;
  let tp: Transport;
  let handle: ClientHandle;

  async function evaluate(template: string, env?: Record<string, string>, h = handle): Promise<unknown> {
    return unwrap(await tp.evaluate(h, template, '/tmp', env === undefined ? undefined : JSON.stringify(env)));
  }

  before(async () => {
    daemon = await spawnDaemon();
    tp = createSubprocessTransport();
    handle = tp.newClient({ socketPath: daemon.sockPath, timeoutMs: 5000 });
    unwrap(await tp.put(handle, 'ev', JSON.stringify({ n: 7, s: 'str' }), undefined, undefined));
  });

  after(() => {
    tp.freeClient(handle);
    daemon.stop();
  });

  it('a single tag keeps its type', async () => {
    assert.equal(await evaluate('{{ 1 + 1 }}'), 2);
    assert.equal(await evaluate('{{ ev.n }}'), 7);
    assert.equal(await evaluate('{{ ev.s }}'), 'str');
  });

  it('literal text and several tags are a string', async () => {
    assert.equal(await evaluate('n={{ ev.n }}'), 'n=7');
  });

  it('an object stays an object', async () => {
    assert.deepEqual(await evaluate('{{ {"a": 1} }}'), { a: 1 });
  });

  it('a template that renders nothing is absent', async () => {
    assert.equal(await evaluate('{{ none }}'), null);
  });

  it('the map is the template env', async () => {
    assert.equal(await evaluate('{{ env.ONLYINMAP }}', { ONLYINMAP: 'm' }), 'm');
    assert.equal(await evaluate('{{ env.HOME or "none" }}', {}), 'none');
  });

  it('a template that cannot render is a ServerError', async () => {
    await assert.rejects(
      () => evaluate('{{ exec("echo hi") }}'),
      (err: unknown) => err instanceof ServerError && /exec\(\) is disabled/.test(err.message),
    );
  });

  it('exec runs on a client made with exec: true', async () => {
    const execHandle = tp.newClient({ socketPath: daemon.sockPath, timeoutMs: 5000, exec: true });
    try {
      assert.equal(await evaluate('{{ exec("echo hi") | trim }}', undefined, execHandle), 'hi');
    } finally {
      tp.freeClient(execHandle);
    }
  });
});
