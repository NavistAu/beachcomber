import { test } from 'node:test';
import assert from 'node:assert';
import { createServer } from 'node:net';
import { mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import { connectWithRetry } from '../src/client.js';

test('connect retries succeed after brief outage', async () => {
    const dir = mkdtempSync(join(tmpdir(), 'comb-retry-'));
    const sockPath = join(dir, 'sock');

    let srv: ReturnType<typeof createServer> | undefined;
    const timer = setTimeout(() => {
        srv = createServer();
        srv.listen(sockPath);
    }, 400);

    try {
        const start = Date.now();
        const sock = await connectWithRetry(sockPath);
        const elapsed = Date.now() - start;

        assert.ok(sock);
        assert.ok(elapsed >= 250, `expected retry; elapsed=${elapsed}`);
        sock.end();
    } finally {
        // Close the listening server and clear the timer so no open handle
        // keeps the test runner's event loop alive (otherwise the file-level
        // subtest can hang to the 30s timeout under CI's loader).
        clearTimeout(timer);
        await new Promise<void>((resolve) => {
            if (srv) srv.close(() => resolve());
            else resolve();
        });
    }
});

test('connect retries exhaust', async () => {
    const dir = mkdtempSync(join(tmpdir(), 'comb-retry-'));
    const sockPath = join(dir, 'nosock');

    const start = Date.now();
    await assert.rejects(connectWithRetry(sockPath));
    const elapsed = Date.now() - start;
    assert.ok(elapsed >= 1700, `expected full retry wait; elapsed=${elapsed}`);
});
