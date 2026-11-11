import { describe, it, before, after } from 'node:test';
import assert from 'node:assert/strict';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { createSubprocessTransport } from '../src/subprocess_transport.js';

// A `comb` that refuses a daemon of another protocol major says so on
// stderr; the subprocess tier gives that the kind the FFI tier does.
describe('subprocess transport and a daemon of another protocol major', () => {
  const words = 'the daemon speaks protocol 3.0 and this client 2.0, which cannot talk: upgrade this client to one that speaks it';
  let dir: string;
  let saved: string | undefined;

  before(() => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'bc-proto-'));
    const comb = path.join(dir, 'comb');
    fs.writeFileSync(comb, `#!/bin/sh\necho 'error: daemon: ${words}' >&2\nexit 2\n`, { mode: 0o755 });
    saved = process.env['COMB_BIN'];
    process.env['COMB_BIN'] = comb;
  });

  after(() => {
    if (saved === undefined) delete process.env['COMB_BIN'];
    else process.env['COMB_BIN'] = saved;
    fs.rmSync(dir, { recursive: true, force: true });
  });

  it('every op is protocol_mismatch, in the CLI words', async () => {
    const tp = createSubprocessTransport();
    const handle = tp.newClient({ socketPath: path.join(dir, 'sock'), timeoutMs: 5000 });
    for (const env of [
      await tp.get(handle, 'git.branch', undefined, undefined, false, undefined),
      await tp.put(handle, 'x', '1', undefined, undefined, false),
      await tp.refresh(handle, 'x', undefined),
    ]) {
      assert.equal(env.ok, false);
      assert.deepEqual((env as { error: unknown }).error, { kind: 'protocol_mismatch', message: words });
    }
  });
});
