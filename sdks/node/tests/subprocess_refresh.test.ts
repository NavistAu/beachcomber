import './helpers.js'; // side effect: defaults COMB_BIN/BEACHCOMBER_LIB to local builds when present
import { describe, it, before, after } from 'node:test';
import assert from 'node:assert/strict';
import * as fs from 'fs';
import * as path from 'path';
import { daemonAvailable, spawnDaemon, type TestDaemon } from './helpers.js';
import { unwrap } from '../src/envelope.js';
import { ServerError } from '../src/errors.js';
import { createSubprocessTransport } from '../src/subprocess_transport.js';
import type { ClientHandle, Transport } from '../src/transport.js';

const skip = !daemonAvailable();

// The subprocess tier's `refresh` is one `comb refresh <key> [--path P |
// --global]` — the daemon's op itself, so a key with no Source behind it
// (a `put`-created namespace) refuses in the daemon's own words: the same
// `serverMessage` the ffi tier reports (integration.test.ts asserts that
// half). These drive the transport directly and unwrap its envelope
// exactly as `Client.refresh` does.
describe('subprocess transport refresh against a real daemon', { skip }, () => {
  let daemon: TestDaemon;
  let tp: Transport;
  let handle: ClientHandle;
  let argvLog: string;
  const savedEnv: Record<string, string | undefined> = {};

  async function put(key: string, data: unknown, path_?: string): Promise<void> {
    unwrap(await tp.put(handle, key, JSON.stringify(data), undefined, path_));
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
    await put('sp', { n: 1 }, '/tmp');
  });

  after(() => {
    tp.freeClient(handle);
    daemon.stop();
    for (const [name, value] of Object.entries(savedEnv)) {
      if (value === undefined) delete process.env[name];
      else process.env[name] = value;
    }
  });

  it('a provider with sources refreshes and resolves', async () => {
    unwrap(await tp.refresh(handle, 'hostname', undefined));
    assert.equal(lastArgv(), 'refresh hostname --global');
  });

  it('a static namespace is the daemon\'s refusal, in its own words', async () => {
    await assert.rejects(
      async () => unwrap(await tp.refresh(handle, 'sp', '/tmp')),
      (err: unknown) =>
        err instanceof ServerError && err.serverMessage.startsWith('nothing to refresh: sp has no sources'),
    );
    assert.equal(lastArgv(), 'refresh sp --path /tmp');
  });
});
