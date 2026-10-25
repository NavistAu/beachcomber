import './helpers.js'; // side effect: defaults COMB_BIN/BEACHCOMBER_LIB to local builds when present
import { describe, it, before, after } from 'node:test';
import assert from 'node:assert/strict';
import * as fs from 'fs';
import * as path from 'path';
import { daemonAvailable, spawnDaemon, type TestDaemon } from './helpers.js';
import { unwrap } from '../src/envelope.js';
import { ServerError, UnsupportedError } from '../src/errors.js';
import { createSubprocessTransport } from '../src/subprocess_transport.js';
import type { ClientHandle, Transport } from '../src/transport.js';

const skip = !daemonAvailable();

const BC_GET_FORCE = 0x1;
const BC_GET_WAIT = 0x2;

// The subprocess tier's `get` is one `comb get -f json <key> [--path P |
// --global] [--force] [--wait]`, so its answer is the CLI's — the
// directory chain, `$HOME` as the global entry, `--force`/`--wait`, the
// daemon's refusals — and a template-backed key renders. These drive the
// transport directly and unwrap its envelope exactly as `Client.get` does.
describe('subprocess transport get against a real daemon', { skip }, () => {
  let daemon: TestDaemon;
  let tp: Transport;
  let handle: ClientHandle;
  let argvLog: string;
  const savedEnv: Record<string, string | undefined> = {};

  async function put(key: string, data: unknown, path_?: string): Promise<void> {
    unwrap(await tp.put(handle, key, JSON.stringify(data), undefined, path_));
  }

  async function get(key: string, path_?: string, flags = 0): Promise<unknown> {
    return unwrap(await tp.get(handle, key, path_, undefined, flags));
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
    await put(
      'bad:template',
      {
        bad: '{{ bad.bad }}',
        cyc: '{{ bad.cyc }}',
        ex: '{{ exec("definitely-missing-cmd-4f2a") }}',
        grp: { leaf: '{{ bad.grp.leaf }}' },
      },
      '/tmp',
    );
  });

  after(() => {
    tp.freeClient(handle);
    daemon.stop();
    for (const [name, value] of Object.entries(savedEnv)) {
      if (value === undefined) delete process.env[name];
      else process.env[name] = value;
    }
  });

  it('a scalar field is its value', async () => {
    assert.equal(await get('sp.n', '/tmp'), 1);
  });

  it('put(key, null) removes the node, over the CLI', async () => {
    await put('spnull', { x: 1 }, '/tmp');
    assert.equal(await get('spnull.x', '/tmp'), 1);
    await put('spnull', null, '/tmp');
    assert.equal(await get('spnull.x', '/tmp'), null);
  });

  it('a string field is its string', async () => {
    assert.equal(await get('sp.s', '/tmp'), 'str');
  });

  it('a bare provider is every field merged into one object, templates rendered', async () => {
    assert.deepEqual(await get('sp', '/tmp'), { n: 1, s: 'str', o: { k: 'v' }, t: 2 });
  });

  it('a dotted address drills into the field value', async () => {
    assert.equal(await get('sp.o.k', '/tmp'), 'v');
  });

  it('an absent field is null, not a throw', async () => {
    assert.equal(await get('sp.nope', '/tmp'), null);
  });

  it("a daemon rejection is a ServerError in the daemon's words", async () => {
    await assert.rejects(
      () => get('a:b:c.x', '/tmp'),
      (err: unknown) => err instanceof ServerError && err.message.startsWith('invalid facet key: a:b:c'),
    );
  });

  it('a template-only node is its rendered value', async () => {
    assert.equal(await get('sp.t', '/tmp'), 2);
  });

  it('no path reads the global entry', async () => {
    assert.equal(await get('spg.g'), 1);
    assert.equal(lastArgv(), 'get -f json spg.g --global');
  });

  it("an ancestor's put is read from a child path (the directory chain)", async () => {
    assert.equal(await get('chain.a', path.join(daemon.tmpDir, 'proj', 'sub')), 1);
  });

  it("a path equal to the daemon's $HOME is the global entry", async () => {
    assert.equal(await get('spg.g', daemon.tmpDir), 1);
  });

  it('a key is asked alone, in one spawn', async () => {
    assert.equal(await get('sp.n', '/tmp'), 1);
    assert.equal(lastArgv(), 'get -f json sp.n --path /tmp');
  });

  it('a facet key reads the layer as stored', async () => {
    assert.equal(await get('sp:put.n', '/tmp'), 1);
    assert.equal(lastArgv(), 'get -f json sp:put.n --path /tmp');
    assert.equal(await get('sp:template.t', '/tmp'), '{{ 1 + 1 }}');
  });

  it('a colon below the provider segment is part of the field, and misses', async () => {
    assert.equal(await get('sp.n:age', '/tmp'), null);
    assert.equal(lastArgv(), 'get -f json sp.n:age --path /tmp');
  });

  it('a bracketed key keeps its brackets', async () => {
    assert.equal(await get("sp['o'].k", '/tmp'), 'v');
    assert.equal(lastArgv(), "get -f json sp['o'].k --path /tmp");
  });

  it('wait reaches the CLI as --wait', async () => {
    assert.equal(await get('sp.n', '/tmp', BC_GET_WAIT), 1);
    assert.equal(lastArgv(), 'get -f json sp.n --path /tmp --wait');
  });

  it("--force on a static namespace is the daemon's refusal", async () => {
    await assert.rejects(
      () => get('sp.n', '/tmp', BC_GET_FORCE),
      (err: unknown) => err instanceof ServerError && err.message.startsWith('cannot --force static namespace'),
    );
    assert.equal(lastArgv(), 'get -f json sp.n --path /tmp --force');
  });

  it("a caller's env cannot be honoured and is refused, naming the FFI transport", async () => {
    await assert.rejects(
      () => tp.get(handle, 'sp.t', '/tmp', JSON.stringify({ A: 'x' }), 0),
      (err: unknown) => err instanceof UnsupportedError && /FFI transport/.test(err.message),
    );
  });

  it("a template that cannot resolve is the resolver's error, not a null hit", async () => {
    await assert.rejects(
      () => get('bad.bad', '/tmp'),
      (err: unknown) => err instanceof ServerError && /cycle/.test(err.message),
    );
  });

  it("a namespace read with one bad leaf is the resolver's error, not a null hit", async () => {
    await assert.rejects(
      () => get('bad', '/tmp'),
      (err: unknown) => err instanceof ServerError && /cycle/.test(err.message),
    );
  });

  it("a template cycle is the resolver's error", async () => {
    await assert.rejects(
      () => get('bad.cyc', '/tmp'),
      (err: unknown) => err instanceof ServerError && /cycle/.test(err.message),
    );
  });

  it("a failed exec() is a ServerError, never a transport kind", async () => {
    // The CLI's message names a missing file; that must not read as the
    // daemon being unreachable — the daemon answered.
    await assert.rejects(
      () => get('bad.ex', '/tmp'),
      (err: unknown) => err instanceof ServerError && /definitely-missing-cmd-4f2a/.test(err.message),
    );
  });

  it("a group whose leaf fails reports the leaf's line", async () => {
    await assert.rejects(
      () => get('bad.grp', '/tmp'),
      (err: unknown) =>
        err instanceof ServerError &&
        /^bad\.grp\.leaf: /.test(err.message) &&
        !/^error: /m.test(err.message),
    );
  });
});

// A refusal the daemon answered `key` with is a server error whatever its
// words — the transport must not guess a transport kind from them. Driven
// by a `comb` stand-in that prints such a line, since no real refusal
// happens to contain one of the phrases the transport-failure classifier
// looks for.
describe('subprocess transport classifies a daemon-answered refusal', () => {
  const savedCombBin = process.env['COMB_BIN'];
  let tmpDir: string;

  before(() => {
    tmpDir = fs.mkdtempSync(path.join('/tmp', 'bc-fake-comb-'));
    const standIn = path.join(tmpDir, 'comb');
    fs.writeFileSync(
      standIn,
      `#!/bin/sh\necho 'null'\necho 'error: k.x: cannot read: no such file or directory' >&2\nexit 2\n`,
      { mode: 0o755 },
    );
    process.env['COMB_BIN'] = standIn;
  });

  after(() => {
    if (savedCombBin === undefined) delete process.env['COMB_BIN'];
    else process.env['COMB_BIN'] = savedCombBin;
    fs.rmSync(tmpDir, { recursive: true, force: true });
  });

  it('as server_error, never daemon_not_running', async () => {
    const tp = createSubprocessTransport();
    const handle = tp.newClient({ timeoutMs: 5000 });
    const env = await tp.get(handle, 'k.x', '/tmp', undefined, 0);
    assert.equal(env.ok, false);
    if (env.ok) return;
    assert.deepEqual(env.error, {
      kind: 'server_error',
      message: 'cannot read: no such file or directory',
    });
  });
});
