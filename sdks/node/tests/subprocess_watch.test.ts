import './helpers.js'; // side effect: defaults COMB_BIN/BEACHCOMBER_LIB to local builds when present
import { describe, it, before, after } from 'node:test';
import assert from 'node:assert/strict';
import type { ChildProcess } from 'child_process';
import * as net from 'net';
import * as path from 'path';
import { daemonAvailable, spawnDaemon, type TestDaemon } from './helpers.js';
import { WatchStream } from '../src/client.js';
import { unwrap } from '../src/envelope.js';
import { ServerError, TimeoutError } from '../src/errors.js';
import { createSubprocessTransport } from '../src/subprocess_transport.js';
import type { ClientHandle, Transport, WatchHandle } from '../src/transport.js';

const skip = !daemonAvailable();

// The subprocess tier's `watch` is one long-lived `comb watch -f json <key>
// [--path P | --global]`: every frame is the value `get` answers, a miss is
// `null`, and the stream is read through the same `WatchStream` the FFI tier
// is. These drive the transport directly, as `Client.watch` does.
describe('subprocess transport watch against a real daemon', { skip }, () => {
  let daemon: TestDaemon;
  let tp: Transport;
  let handle: ClientHandle;
  const savedEnv: Record<string, string | undefined> = {};

  async function put(key: string, data: unknown): Promise<void> {
    unwrap(await tp.put(handle, key, JSON.stringify(data), undefined, undefined));
  }

  function watch(key: string, env?: Record<string, string>): { stream: WatchStream; raw: WatchHandle } {
    const raw = tp.openWatch(handle, key, undefined, env === undefined ? undefined : JSON.stringify(env));
    return { stream: new WatchStream(tp, raw), raw };
  }

  /** The `comb watch` child behind a subprocess watch handle. */
  function childOf(raw: WatchHandle): ChildProcess {
    return (raw as { child: ChildProcess }).child;
  }

  before(async () => {
    daemon = await spawnDaemon();
    // The `comb` children read the config under $HOME/$XDG_CONFIG_HOME, so
    // they get the daemon's dirs, not the developer's.
    for (const name of ['HOME', 'XDG_CONFIG_HOME', 'XDG_STATE_HOME']) {
      savedEnv[name] = process.env[name];
    }
    process.env['HOME'] = daemon.tmpDir;
    process.env['XDG_CONFIG_HOME'] = daemon.tmpDir;
    process.env['XDG_STATE_HOME'] = path.join(daemon.tmpDir, '.local', 'state');
    tp = createSubprocessTransport();
    handle = tp.newClient({ socketPath: daemon.sockPath, timeoutMs: 5000 });
  });

  after(() => {
    tp.freeClient(handle);
    daemon.stop();
    for (const [name, value] of Object.entries(savedEnv)) {
      if (value === undefined) delete process.env[name];
      else process.env[name] = value;
    }
  });

  it('the first event is the current value', async () => {
    await put('w1', { v: 'one' });
    const { stream } = watch('w1.v');
    try {
      assert.deepEqual(await stream.nextEvent(5000), { data: 'one' });
    } finally {
      stream.close();
    }
  });

  it('a put gives an event', async () => {
    await put('w2', { v: 'one' });
    const { stream } = watch('w2.v');
    try {
      assert.deepEqual(await stream.nextEvent(5000), { data: 'one' });
      await put('w2', { v: 'two' });
      assert.deepEqual(await stream.nextEvent(5000), { data: 'two' });
    } finally {
      stream.close();
    }
  });

  it('a missing key gives a null event, and the stream stays open', async () => {
    const { stream } = watch('w3.v');
    try {
      assert.deepEqual(await stream.nextEvent(5000), { data: null });
      await put('w3', { v: 'here' });
      assert.deepEqual(await stream.nextEvent(5000), { data: 'here' });
    } finally {
      stream.close();
    }
  });

  it('nextEvent(50) with nothing new is a TimeoutError', async () => {
    await put('w4', { v: 'one' });
    const { stream } = watch('w4.v');
    try {
      await stream.nextEvent(5000);
      await assert.rejects(() => stream.nextEvent(50), TimeoutError);
    } finally {
      stream.close();
    }
  });

  it('a cancel during a pending nextEvent answers null', async () => {
    await put('w5', { v: 'one' });
    const { stream } = watch('w5.v');
    try {
      await stream.nextEvent(5000);
      const pending = stream.nextEvent();
      stream.cancel();
      assert.equal(await pending, null);
      assert.equal(await stream.nextEvent(), null, 'a cancelled stream has ended');
    } finally {
      stream.close();
    }
  });

  it('close leaves no child behind', async () => {
    await put('w6', { v: 'one' });
    const { stream, raw } = watch('w6.v');
    await stream.nextEvent(5000);
    const child = childOf(raw);
    const exited = new Promise((resolve) => child.once('exit', resolve));
    stream.close();
    stream.close();
    await exited;
    assert.throws(() => process.kill(child.pid as number, 0), { code: 'ESRCH' });
  });

  it("a refused key is a ServerError in the daemon's words", async () => {
    const { stream } = watch('a:b:c.x');
    try {
      await assert.rejects(
        () => stream.nextEvent(5000),
        (err: unknown) => err instanceof ServerError && /^invalid facet key: a:b:c/.test(err.message),
      );
    } finally {
      stream.close();
    }
  });

  it('the env map is the template env', async () => {
    await put('wenv:template', { v: '{{ env.ONLYINMAP or "none" }}' });
    const mapped = watch('wenv.v', { ONLYINMAP: 'm' });
    const empty = watch('wenv.v', {});
    try {
      assert.deepEqual(await mapped.stream.nextEvent(5000), { data: 'm' });
      assert.deepEqual(await empty.stream.nextEvent(5000), { data: 'none' });
    } finally {
      mapped.stream.close();
      empty.stream.close();
    }
  });

  // T3.6, reported and not asserted: from the daemon acknowledging a `put` —
  // sent on a socket of its own, so no spawn is timed — to `nextEvent`
  // returning the new value.
  it('reports how soon an event follows the write', async (t) => {
    await put('wlat', { v: 0 });
    const { stream } = watch('wlat.v');
    const socket = net.connect(daemon.sockPath);
    await new Promise((resolve) => socket.once('connect', resolve));
    let buffered = '';
    const acked = () =>
      new Promise<void>((resolve) => {
        const onData = (chunk: Buffer) => {
          buffered += chunk.toString();
          if (buffered.includes('\n')) {
            buffered = '';
            socket.off('data', onData);
            resolve();
          }
        };
        socket.on('data', onData);
      });
    try {
      await stream.nextEvent(5000);
      const after: number[] = [];
      for (let round = 1; round <= 50; round++) {
        const ack = acked();
        socket.write(`${JSON.stringify({ op: 'put', key: 'wlat.v', data: round })}\n`);
        await ack;
        const written = performance.now();
        assert.deepEqual(await stream.nextEvent(5000), { data: round });
        after.push(performance.now() - written);
      }
      after.sort((a, b) => a - b);
      const at = (q: number) => (after[Math.round((after.length - 1) * q)] as number).toFixed(2);
      t.diagnostic(
        `subprocess watch, write acknowledged to event: min ${at(0)}ms p50 ${at(0.5)}ms p99 ${at(0.99)}ms (n=${after.length})`,
      );
    } finally {
      socket.destroy();
      stream.close();
    }
  });
});
