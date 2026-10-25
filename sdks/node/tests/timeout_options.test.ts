import './helpers.js'; // side effect: defaults BEACHCOMBER_LIB to a locally built dylib/so when present
import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { Client } from '../src/client.js';
import { ConnectionFailedError, UnsupportedError } from '../src/errors.js';
import { clientOptionsJson, koffiAvailable } from '../src/ffi_transport.js';
import { createSubprocessTransport } from '../src/subprocess_transport.js';

// The timeouts are the ABI's options, passed through under its spelling with
// no default of the SDK's own: a key the caller did not give is not sent.
describe('client options reach bc_client_new as given', () => {
  it('no options is no options_json at all', () => {
    assert.equal(clientOptionsJson({}), null);
    assert.equal(clientOptionsJson({ timeoutMs: undefined, socketPath: undefined }), null);
  });

  it('carries exactly the keys given, under the ABI spelling', () => {
    assert.deepEqual(JSON.parse(clientOptionsJson({ timeoutMs: 250, connectTimeoutMs: 0 }) as string), {
      timeout_ms: 250,
      connect_timeout_ms: 0,
    });
  });
});

const lib = process.env['BEACHCOMBER_LIB'];
const skipFfi = !koffiAvailable() || !lib || !fs.existsSync(lib);

describe('connectTimeoutMs (FFI transport, no daemon)', { skip: skipFfi }, () => {
  it('0 is one attempt: a socket nobody serves fails at once', async () => {
    const dir = fs.mkdtempSync(path.join('/tmp', 'bcnode'));
    const client = new Client({ socketPath: path.join(dir, 'sock'), autostart: false, connectTimeoutMs: 0 });
    try {
      const started = Date.now();
      await assert.rejects(() => client.hello(), ConnectionFailedError);
      const took = Date.now() - started;
      assert.ok(took < 250, `one attempt took ${took}ms`);
    } finally {
      client.close();
      fs.rmSync(dir, { recursive: true, force: true });
    }
  });
});

describe('timeouts over the subprocess transport', () => {
  it('has no default timeout of its own', () => {
    const handle = createSubprocessTransport().newClient({}) as { timeoutMs?: number };
    assert.equal(handle.timeoutMs, undefined);
  });

  it('timeoutMs bounds each spawned comb', () => {
    const handle = createSubprocessTransport().newClient({ timeoutMs: 1234 }) as { timeoutMs?: number };
    assert.equal(handle.timeoutMs, 1234);
  });

  it('refuses connectTimeoutMs, which comb has no flag for', () => {
    assert.throws(
      () => createSubprocessTransport().newClient({ connectTimeoutMs: 0 }),
      (err: unknown) => err instanceof UnsupportedError && /FFI transport/.test(err.message),
    );
  });
});
