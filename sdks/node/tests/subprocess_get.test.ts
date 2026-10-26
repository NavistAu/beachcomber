import './helpers.js'; // side effect: defaults COMB_BIN/BEACHCOMBER_LIB to local builds when present
import { describe, it, before, after } from 'node:test';
import assert from 'node:assert/strict';
import * as fs from 'fs';
import * as path from 'path';
import { daemonAvailable, spawnDaemon, type TestDaemon } from './helpers.js';
import { unwrap } from '../src/envelope.js';
import { ParseError, ServerError } from '../src/errors.js';
import { createSubprocessTransport } from '../src/subprocess_transport.js';
import type { ClientHandle, Transport } from '../src/transport.js';

const skip = !daemonAvailable();

// The subprocess tier's `get` is one `comb get -f json <key> [--path P |
// --global] [--force] [--wait=<wait>]`, so its answer is the CLI's — the
// directory chain, `$HOME` as the global entry, `--force`, the wait, the
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

  async function get(key: string, path_?: string, force = false, wait?: string): Promise<unknown> {
    return unwrap(await tp.get(handle, key, path_, undefined, force, wait));
  }

  function lastCall(): string {
    const lines = fs.readFileSync(argvLog, 'utf8').trimEnd().split('\n');
    return lines[lines.length - 1] as string;
  }

  /** The last call's argv from its subcommand on, without the global flags. */
  function lastArgv(): string {
    return lastCall().replace(/^.*? (?=get |put |refresh |status)/, '');
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

  it('the wait reaches the CLI as --wait, as given', async () => {
    assert.equal(await get('sp.n', '/tmp', false, '50ms'), 1);
    assert.equal(lastArgv(), 'get -f json sp.n --path /tmp --wait=50ms');
    assert.equal(await get('sp.n', '/tmp', false, 'forever'), 1);
    assert.equal(lastArgv(), 'get -f json sp.n --path /tmp --wait=forever');
  });

  it("a wait comb cannot read is the library's parse_error, in its words", async () => {
    for (const wait of ['soon', '-1s']) {
      await assert.rejects(
        () => get('sp.n', '/tmp', false, wait),
        (err: unknown) => err instanceof ParseError && err.message.startsWith(`invalid wait \`${wait}\`: `),
      );
    }
    await assert.rejects(
      () => get('sp.n', '/tmp', false, 0 as unknown as string),
      (err: unknown) => err instanceof ParseError && err.message.includes('invalid type'),
    );
  });

  it("--force on a static namespace is the daemon's refusal", async () => {
    await assert.rejects(
      () => get('sp.n', '/tmp', true),
      (err: unknown) => err instanceof ServerError && err.message.startsWith('cannot --force static namespace'),
    );
    assert.equal(lastArgv(), 'get -f json sp.n --path /tmp --force');
  });

  it('the global flags a get spawns with are --socket, --color and --no-exec', async () => {
    assert.equal(await get('sp.n', '/tmp'), 1);
    assert.equal(lastCall(), `--socket ${daemon.sockPath} --color always --no-exec get -f json sp.n --path /tmp`);
  });

  it('the map is the template env: a map-only variable hits, a host-only one misses', async () => {
    await put('mapenv:template', { m: '{{ env.ONLYINMAP }}', h: '{{ env.ONLYINHOST }}' }, '/tmp');
    process.env['ONLYINHOST'] = 'host';
    try {
      const map = JSON.stringify({ ONLYINMAP: 'map' });
      assert.equal(unwrap(await tp.get(handle, 'mapenv.m', '/tmp', map, 0)), 'map');
      assert.equal(unwrap(await tp.get(handle, 'mapenv.h', '/tmp', map, 0)), null);
      // With no map the spawn renders with this process's environment.
      assert.equal(unwrap(await tp.get(handle, 'mapenv.h', '/tmp', undefined, 0)), 'host');
    } finally {
      delete process.env['ONLYINHOST'];
    }
  });

  it("an exec child sees the map's variables", async () => {
    await put('mapexec:template', { v: '{{ exec(\'echo "$MARK"\') | trim }}' }, '/tmp');
    const execHandle = tp.newClient({ socketPath: daemon.sockPath, timeoutMs: 5000, exec: true });
    try {
      const map = JSON.stringify({ MARK: 'from-map', PATH: '/usr/bin:/bin' });
      assert.equal(unwrap(await tp.get(execHandle, 'mapexec.v', '/tmp', map, 0)), 'from-map');
      assert.equal(lastCall().includes('--no-exec'), false);
    } finally {
      tp.freeClient(execHandle);
    }
  });

  it('with exec off the call fails with exec() is disabled', async () => {
    await put('noexec:template', { v: "{{ exec('echo hi') }}" }, '/tmp');
    await assert.rejects(
      () => get('noexec.v', '/tmp'),
      (err: unknown) => err instanceof ServerError && /exec\(\) is disabled/.test(err.message),
    );
  });

  it('NO_COLOR in the map stops fg drawing escapes', async () => {
    await put('hue:template', { v: "{{ 'x' | fg('#ff0000') }}" }, '/tmp');
    const drawn = unwrap(await tp.get(handle, 'hue.v', '/tmp', JSON.stringify({}), 0));
    assert.match(String(drawn), /\u001b\[/);
    assert.equal(lastCall().includes('--color always'), true);
    const plain = unwrap(await tp.get(handle, 'hue.v', '/tmp', JSON.stringify({ NO_COLOR: '1' }), 0));
    assert.equal(plain, 'x');
    assert.equal(lastCall().includes('--color never'), true);
  });

  it("socketPath beats a socket_path in the config under the map's HOME", async () => {
    const home = fs.mkdtempSync(path.join('/tmp', 'bc-cfg-'));
    try {
      fs.mkdirSync(path.join(home, '.config', 'beachcomber'), { recursive: true });
      fs.writeFileSync(
        path.join(home, '.config', 'beachcomber', 'config.toml'),
        '[daemon]\nsocket_path = "/nonexistent/other.sock"\n',
      );
      const map = JSON.stringify({ HOME: home, XDG_CONFIG_HOME: path.join(home, '.config') });
      assert.equal(unwrap(await tp.get(handle, 'spg.g', undefined, map, 0)), 1);
    } finally {
      fs.rmSync(home, { recursive: true, force: true });
    }
  });

  // Target T3.6: a get with an env costs about what one without does. The
  // numbers are reported, never asserted (a spawn's time is the machine's).
  it('timing: a get with an env against one without (reported)', async (t) => {
    const runs = 30;
    const median = async (envJson: string | undefined): Promise<number> => {
      const times: number[] = [];
      for (let i = 0; i < runs; i++) {
        const start = process.hrtime.bigint();
        unwrap(await tp.get(handle, 'sp.n', '/tmp', envJson, 0));
        times.push(Number(process.hrtime.bigint() - start) / 1e6);
      }
      times.sort((a, b) => a - b);
      return times[Math.floor(runs / 2)] as number;
    };
    await median(undefined); // warm the daemon and the file cache
    const without = await median(undefined);
    const withEnv = await median(JSON.stringify({ A: 'x', PATH: '/usr/bin:/bin' }));
    t.diagnostic(
      `get p50: ${without.toFixed(2)} ms without an env, ${withEnv.toFixed(2)} ms with one ` +
        `(${(withEnv - without).toFixed(2)} ms more)`,
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
    const execHandle = tp.newClient({ socketPath: daemon.sockPath, timeoutMs: 5000, exec: true });
    try {
      await assert.rejects(
        async () => unwrap(await tp.get(execHandle, 'bad.ex', '/tmp', undefined, 0)),
        (err: unknown) => err instanceof ServerError && /definitely-missing-cmd-4f2a/.test(err.message),
      );
    } finally {
      tp.freeClient(execHandle);
    }
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
