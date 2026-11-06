import './helpers.js'; // side effect: defaults BEACHCOMBER_LIB to a locally built dylib/so when present
import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import { Client } from '../src/client.js';
import { koffiAvailable } from '../src/ffi_transport.js';
import { ServerError } from '../src/errors.js';

// An exec() call with no daemon-backed ref never touches the socket, so
// whether `exec: true` reached bc_client_new is observable against a dead
// socket with autostart off — no daemon needed, only koffi and the library.
const lib = process.env['BEACHCOMBER_LIB'];
const skip = !koffiAvailable() || !lib || !fs.existsSync(lib);

describe('Client({ exec }) reaches the library (FFI transport, no daemon)', { skip }, () => {
  const dead = { socketPath: '/nonexistent/beachcomber-test.sock', autostart: false, timeoutMs: 500 };

  it('exec() is disabled by default', async () => {
    const client = new Client(dead);
    try {
      await assert.rejects(
        () => client.eval("{{ exec(['printf', 'x']) }}", { cwd: '/tmp' }),
        (err: unknown) => {
          if (!(err instanceof ServerError)) return false;
          assert.match(err.message, /exec\(\) is disabled in this context/);
          return true;
        },
      );
    } finally {
      client.close();
    }
  });

  it('exec: true lets the template run it', async () => {
    const client = new Client({ ...dead, exec: true });
    try {
      assert.equal(await client.eval("{{ exec(['printf', 'x']) }}", { cwd: '/tmp' }), 'x');
    } finally {
      client.close();
    }
  });
});
