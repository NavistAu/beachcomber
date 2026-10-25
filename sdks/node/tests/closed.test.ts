import './helpers.js'; // side effect: defaults BEACHCOMBER_LIB to a locally built dylib/so when present
import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { Client } from '../src/client.js';
import { IoError } from '../src/errors.js';

// A closed client or session is an IoError on any later call, raised before
// the library is reached; a closed watch stream has ended (docs/sdk-api.md:
// a closed handle). Handing the library a freed handle would crash the process.
describe('a closed handle', () => {
  const options = { socketPath: '/tmp/bc-closed.sock', autostart: false };

  it('a closed client is an IoError, and closing twice frees once', async () => {
    const client = new Client(options);
    client.close();
    client.close();
    await assert.rejects(client.hello(), (e: unknown) => e instanceof IoError && e.message === 'the client is closed');
    await assert.rejects(client.get('p.f'), IoError);
  });

  it('a closed session is an IoError', async () => {
    const client = new Client(options);
    const session = await client.session();
    session.close();
    await assert.rejects(session.get('p.f'), (e: unknown) => e instanceof IoError && e.message === 'the session is closed');
    client.close();
  });

  it('a closed watch stream has ended', async () => {
    const client = new Client(options);
    const stream = await client.watch('p.f');
    stream.close();
    assert.equal(await stream.nextEvent(), null);
    stream.cancel();
    client.close();
  });
});
