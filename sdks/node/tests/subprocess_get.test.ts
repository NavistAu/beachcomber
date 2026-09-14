import './helpers.js'; // side effect: defaults COMB_BIN/BEACHCOMBER_LIB to local builds when present
import { describe, it, before, after } from 'node:test';
import assert from 'node:assert/strict';
import * as fs from 'fs';
import * as path from 'path';
import { daemonAvailable, spawnDaemon, type TestDaemon } from './helpers.js';
import { makeCombResult, type GetResultShape } from '../src/client.js';
import { unwrap } from '../src/envelope.js';
import { ServerError } from '../src/errors.js';
import { createSubprocessTransport } from '../src/subprocess_transport.js';
import type { ClientHandle, Transport } from '../src/transport.js';

const skip = !daemonAvailable();

const BC_GET_FORCE = 0x1;
const BC_GET_WAIT = 0x2;

// The subprocess tier asks `comb get -f json <key> <key>:age <key>:stale`
// for the value, its age and its staleness in one spawn, so its answer is
// the resolver's — the directory chain, `$HOME` as the global entry,
// metadata suffixes, `--force`/`--wait`, the daemon's refusals — and a
// template-backed key renders. These drive the transport directly and
// unwrap its envelope exactly as `Client.get` does.
describe('subprocess transport get against a real daemon', { skip }, () => {
  let daemon: TestDaemon;
  let tp: Transport;
  let handle: ClientHandle;
  let argvLog: string;
  const savedEnv: Record<string, string | undefined> = {};

  async function put(key: string, data: unknown, path_?: string): Promise<void> {
    unwrap(await tp.put(handle, key, JSON.stringify(data), undefined, path_));
  }

  async function get(key: string, path_?: string, flags = 0) {
    return makeCombResult(unwrap(await tp.get(handle, key, path_, flags)) as GetResultShape);
  }

  function lastArgv(): string {
    const lines = fs.readFileSync(argvLog, 'utf8').trimEnd().split('\n');
    return lines[lines.length - 1] as string;
  }

  before(async () => {
    daemon = await spawnDaemon();
    // The `comb` children this transport spawns read the config under
    // $HOME/$XDG_CONFIG_HOME (a config-file `socket_path` beats
    // $BEACHCOMBER_SOCKET), so they get the daemon's dirs, not the
    // developer's. A `comb` stand-in records each call's argv first.
    for (const name of ['HOME', 'XDG_CONFIG_HOME', 'XDG_STATE_HOME', 'COMB_BIN']) {
      savedEnv[name] = process.env[name];
    }
    const realComb = process.env['COMB_BIN'] as string;
    argvLog = path.join(daemon.tmpDir, 'argv.log');
    const standIn = path.join(daemon.tmpDir, 'comb');
    fs.writeFileSync(standIn, `#!/bin/sh\nprintf '%s\\n' "$*" >> '${argvLog}'\nexec '${realComb}' "$@"\n`, { mode: 0o755 });
    process.env['HOME'] = daemon.tmpDir;
    process.env['XDG_CONFIG_HOME'] = daemon.tmpDir;
    process.env['XDG_STATE_HOME'] = path.join(daemon.tmpDir, '.local', 'state');
    process.env['COMB_BIN'] = standIn;

    tp = createSubprocessTransport();
    handle = tp.newClient({ socketPath: daemon.sockPath, timeoutMs: 5000 });
    fs.mkdirSync(path.join(daemon.tmpDir, 'proj', 'sub'), { recursive: true });
    await put('sp', { n: 1, s: 'str', o: { k: 'v' } }, '/tmp');
    await put('sp:template', { t: '{{ 1 + 1 }}' }, '/tmp');
    await put('spg', { g: 1 });
    await put('chain', { a: 1 });
    await put('bad', { n: 1 }, '/tmp');
    await put('bad:template', { bad: '{{ 1 + }}', cyc: '{{ bad.cyc }}' }, '/tmp');
  });

  after(() => {
    tp.freeClient(handle);
    daemon.stop();
    for (const [name, value] of Object.entries(savedEnv)) {
      if (value === undefined) delete process.env[name];
      else process.env[name] = value;
    }
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
    assert.deepEqual(r.data, { n: 1, s: 'str', o: { k: 'v' }, t: 2 });
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

  it("a daemon rejection is a ServerError in the daemon's words", async () => {
    await assert.rejects(
      () => get('a:b:c.x', '/tmp'),
      (err: unknown) => err instanceof ServerError && err.serverMessage.startsWith('invalid facet key: a:b:c'),
    );
  });

  it('a template-only node is a hit holding the rendered value, with no template half', async () => {
    const r = await get('sp.t', '/tmp');
    assert.equal(r.isHit, true);
    assert.equal(r.data, 2);
    assert.equal(r.template, undefined);
    assert.equal(r.rest, undefined);
  });

  it('no path reads the global entry', async () => {
    const r = await get('spg.g');
    assert.equal(r.isHit, true);
    assert.equal(r.data, 1);
  });

  it("an ancestor's put is read from a child path (the directory chain)", async () => {
    const r = await get('chain.a', path.join(daemon.tmpDir, 'proj', 'sub'));
    assert.equal(r.isHit, true);
    assert.equal(r.data, 1);
  });

  it("a path equal to the daemon's $HOME is the global entry", async () => {
    const r = await get('spg.g', daemon.tmpDir);
    assert.equal(r.isHit, true);
    assert.equal(r.data, 1);
  });

  it('a :age suffix is the age, a number', async () => {
    const r = await get('sp.n:age', '/tmp');
    assert.equal(r.isHit, true);
    assert.equal(typeof r.data, 'number');
    assert.equal(lastArgv(), 'get -f json sp.n:age --path /tmp');
  });

  it('wait reaches the wire as --wait, in one comb get with the two companions', async () => {
    const r = await get('sp.n', '/tmp', BC_GET_WAIT);
    assert.equal(r.isHit, true);
    assert.equal(lastArgv(), 'get -f json sp.n sp.n:age sp.n:stale --path /tmp --wait');
  });

  it("--force on a static namespace is the daemon's refusal", async () => {
    await assert.rejects(
      () => get('sp.n', '/tmp', BC_GET_FORCE),
      (err: unknown) => err instanceof ServerError && err.serverMessage.startsWith('cannot --force static namespace'),
    );
    assert.equal(lastArgv(), 'get -f json sp.n sp.n:age sp.n:stale --path /tmp --force');
  });

  it("a template that does not compile is the resolver's error, not a null hit", async () => {
    await assert.rejects(
      () => get('bad.bad', '/tmp'),
      (err: unknown) => err instanceof ServerError && /expression compile error/.test(err.message),
    );
  });

  it("a namespace read with one bad leaf is the resolver's error, not a null hit", async () => {
    await assert.rejects(
      () => get('bad', '/tmp'),
      (err: unknown) => err instanceof ServerError && /expression compile error/.test(err.message),
    );
  });

  it("a template cycle is the resolver's error", async () => {
    await assert.rejects(
      () => get('bad.cyc', '/tmp'),
      (err: unknown) => err instanceof ServerError && /cycle/.test(err.message),
    );
  });
});
