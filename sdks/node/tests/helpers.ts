/**
 * Mock Unix socket server for integration tests.
 */

import * as net from 'net';
import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';

export interface MockRequest {
  raw: string;
  parsed: Record<string, unknown>;
}

export type RequestHandler = (req: MockRequest) => Record<string, unknown>;

/**
 * A simple mock TCP/Unix server that handles newline-delimited JSON requests.
 *
 * Usage:
 *   const server = await MockServer.start();
 *   server.handle((req) => ({ ok: true, data: 'hello' }));
 *   // ... run tests ...
 *   await server.stop();
 */
export class MockServer {
  private readonly server: net.Server;
  public readonly socketPath: string;
  private handler: RequestHandler = () => ({ ok: true });

  private constructor(server: net.Server, socketPath: string) {
    this.server = server;
    this.socketPath = socketPath;
  }

  static async start(): Promise<MockServer> {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'beachcomber-test-'));
    const socketPath = path.join(dir, 'sock');

    const server = net.createServer((socket) => {
      let buffer = '';

      socket.on('data', (chunk: Buffer) => {
        buffer += chunk.toString('utf8');
        while (true) {
          const newline = buffer.indexOf('\n');
          if (newline === -1) break;
          const line = buffer.slice(0, newline);
          buffer = buffer.slice(newline + 1);
          if (line.trim() === '') continue;

          let parsed: Record<string, unknown>;
          try {
            parsed = JSON.parse(line) as Record<string, unknown>;
          } catch {
            socket.write(JSON.stringify({ ok: false, error: 'invalid json' }) + '\n');
            continue;
          }

          const mock = server as unknown as { _handler: RequestHandler };
          const response = mock._handler({ raw: line, parsed });
          socket.write(JSON.stringify(response) + '\n');
        }
      });

      socket.on('error', () => {
        // swallow errors in test server
      });
    });

    (server as unknown as { _handler: RequestHandler })._handler = () => ({ ok: true });

    await new Promise<void>((resolve, reject) => {
      server.listen(socketPath, resolve);
      server.on('error', reject);
    });

    return new MockServer(server, socketPath);
  }

  /**
   * Set the handler for incoming requests.  Subsequent requests will use the
   * last handler that was set.
   */
  handle(fn: RequestHandler): void {
    (this.server as unknown as { _handler: RequestHandler })._handler = fn;
  }

  async stop(): Promise<void> {
    await new Promise<void>((resolve) => {
      this.server.close(() => resolve());
    });
    try {
      fs.unlinkSync(this.socketPath);
      fs.rmdirSync(path.dirname(this.socketPath));
    } catch {
      // best-effort cleanup
    }
  }
}
