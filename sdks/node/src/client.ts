/**
 * Client and Session implementations for the beachcomber daemon.
 */

import * as net from 'net';
import { discoverSocketPath } from './discovery.js';
import { DaemonNotRunning, ParseError, ServerError } from './errors.js';
import {
  type ProviderInfo,
  parseResponseLine,
  serialiseRequest,
} from './protocol.js';

// ---- CombResult ----

/**
 * The result of a cache query.
 *
 * Check `isHit` before accessing `data`, `ageMs`, and `stale`.
 * Convenience accessors (`getString`, `getNumber`, `getBool`) safely coerce
 * the data value and return `undefined` when the cast is not possible.
 */
export interface CombResult {
  ok: boolean;
  /** Present on a cache hit, undefined on a miss. */
  data: unknown;
  /** Age of the cached value in milliseconds (0 on miss). */
  ageMs: number;
  /** True when the cached value is considered stale (0 on miss). */
  stale: boolean;
  /** Error message when ok is false (should not normally be seen — errors throw). */
  error?: string;
  /** True when the cache had a value for this key. */
  isHit: boolean;
  /** True when the cache had no value for this key. */
  isMiss: boolean;
  /**
   * Return the data (or a named field within an object result) as a string.
   * @param field  Optional field name for object results (e.g. "branch" from "git").
   */
  getString(field?: string): string | undefined;
  /**
   * Return the data (or a named field within an object result) as a number.
   */
  getNumber(field?: string): number | undefined;
  /**
   * Return the data (or a named field within an object result) as a boolean.
   */
  getBool(field?: string): boolean | undefined;
}

function pickField(data: unknown, field?: string): unknown {
  if (field !== undefined && typeof data === 'object' && data !== null && !Array.isArray(data)) {
    return (data as Record<string, unknown>)[field];
  }
  return data;
}

function makeCombResult(raw: Record<string, unknown>): CombResult {
  const ok = raw['ok'] === true;
  const hasData = 'data' in raw && raw['data'] !== null && raw['data'] !== undefined;
  const data = hasData ? raw['data'] : undefined;
  const ageMs = typeof raw['age_ms'] === 'number' ? raw['age_ms'] : 0;
  const stale = raw['stale'] === true;
  const isHit = ok && hasData;

  return {
    ok,
    data,
    ageMs,
    stale,
    isHit,
    isMiss: !isHit,
    getString(field?: string): string | undefined {
      const v = pickField(data, field);
      if (v === undefined || v === null) return undefined;
      if (typeof v === 'string') return v;
      if (typeof v === 'number' || typeof v === 'boolean') return String(v);
      return JSON.stringify(v);
    },
    getNumber(field?: string): number | undefined {
      const v = pickField(data, field);
      if (typeof v === 'number') return v;
      if (typeof v === 'string') {
        const n = Number(v);
        return isNaN(n) ? undefined : n;
      }
      return undefined;
    },
    getBool(field?: string): boolean | undefined {
      const v = pickField(data, field);
      if (typeof v === 'boolean') return v;
      return undefined;
    },
  };
}

// ---- Low-level socket helpers ----

interface ClientOptions {
  /** Override the auto-discovered socket path. */
  socketPath?: string;
  /** Connection + read timeout in milliseconds. Default: 5000. */
  timeoutMs?: number;
}

/**
 * Open a TCP/Unix connection, send one newline-delimited JSON request, and
 * resolve with the trimmed response line.  The socket is destroyed after
 * the response is received.
 */
function sendOneShot(socketPath: string, request: string, timeoutMs: number): Promise<string> {
  return new Promise((resolve, reject) => {
    const socket = net.createConnection(socketPath);
    let responded = false;
    let buffer = '';

    const timer = setTimeout(() => {
      if (!responded) {
        responded = true;
        socket.destroy();
        reject(new DaemonNotRunning(socketPath));
      }
    }, timeoutMs);

    socket.on('connect', () => {
      socket.write(request);
    });

    socket.on('data', (chunk: Buffer) => {
      buffer += chunk.toString('utf8');
      const newline = buffer.indexOf('\n');
      if (newline !== -1) {
        if (!responded) {
          responded = true;
          clearTimeout(timer);
          const line = buffer.slice(0, newline);
          socket.destroy();
          resolve(line);
        }
      }
    });

    socket.on('error', (err: NodeJS.ErrnoException) => {
      if (!responded) {
        responded = true;
        clearTimeout(timer);
        if (err.code === 'ENOENT' || err.code === 'ECONNREFUSED') {
          reject(new DaemonNotRunning(socketPath));
        } else {
          reject(err);
        }
      }
    });

    socket.on('close', () => {
      if (!responded) {
        responded = true;
        clearTimeout(timer);
        reject(new DaemonNotRunning(socketPath));
      }
    });
  });
}

function parseAndCheck(line: string): Record<string, unknown> {
  let parsed: Record<string, unknown>;
  try {
    parsed = parseResponseLine(line);
  } catch (e) {
    throw new ParseError(line, e instanceof Error ? e.message : String(e));
  }
  if (parsed['ok'] === false) {
    const msg = typeof parsed['error'] === 'string' ? parsed['error'] : 'unknown error';
    throw new ServerError(msg);
  }
  return parsed;
}

// ---- Session ----

/**
 * A persistent connection to the daemon.
 *
 * More efficient than individual `Client` method calls when querying
 * multiple values in sequence (one connection vs. N connections).
 *
 * Obtain a Session via `client.session()`.  Call `session.close()` when done.
 */
export class Session {
  private readonly socket: net.Socket;
  private buffer: string = '';
  private readonly pending: Array<{
    resolve: (line: string) => void;
    reject: (err: Error) => void;
  }> = [];
  private closed = false;

  constructor(socket: net.Socket) {
    this.socket = socket;

    socket.on('data', (chunk: Buffer) => {
      this.buffer += chunk.toString('utf8');
      while (true) {
        const newline = this.buffer.indexOf('\n');
        if (newline === -1) break;
        const line = this.buffer.slice(0, newline);
        this.buffer = this.buffer.slice(newline + 1);
        const waiter = this.pending.shift();
        if (waiter) {
          waiter.resolve(line);
        }
      }
    });

    socket.on('error', (err: Error) => {
      this.closed = true;
      for (const waiter of this.pending) {
        waiter.reject(err);
      }
      this.pending.length = 0;
    });

    socket.on('close', () => {
      this.closed = true;
      for (const waiter of this.pending) {
        waiter.reject(new Error('socket closed unexpectedly'));
      }
      this.pending.length = 0;
    });
  }

  private sendAndReceive(request: string): Promise<string> {
    return new Promise((resolve, reject) => {
      if (this.closed) {
        reject(new Error('session is closed'));
        return;
      }
      this.pending.push({ resolve, reject });
      this.socket.write(request);
    });
  }

  /**
   * Set the default path for subsequent queries on this connection.
   * After calling this, `get` and `poke` calls can omit the path.
   */
  async setContext(repoPath: string): Promise<void> {
    const req = serialiseRequest({ op: 'context', path: repoPath });
    const line = await this.sendAndReceive(req);
    parseAndCheck(line);
  }

  /**
   * Query a key.  If `setContext` has been called, `path` can be omitted.
   */
  async get(key: string, path?: string): Promise<CombResult> {
    const req = serialiseRequest(
      path !== undefined ? { op: 'get', key, path } : { op: 'get', key },
    );
    const line = await this.sendAndReceive(req);
    const parsed = parseAndCheck(line);
    return makeCombResult(parsed);
  }

  /**
   * Trigger recomputation of a provider.
   */
  async poke(key: string, path?: string): Promise<void> {
    const req = serialiseRequest(
      path !== undefined ? { op: 'poke', key, path } : { op: 'poke', key },
    );
    const line = await this.sendAndReceive(req);
    parseAndCheck(line);
  }

  /** Close the underlying socket. */
  close(): void {
    this.closed = true;
    this.socket.destroy();
  }
}

// ---- Client ----

/**
 * Client for the beachcomber daemon.
 *
 * Each method call (except `session()`) opens a new socket connection,
 * sends one request, reads one response, and closes the connection.
 *
 * For multiple sequential queries, use `session()` to reuse one connection.
 */
export class Client {
  private readonly socketPath: string;
  private readonly timeoutMs: number;

  constructor(opts: ClientOptions = {}) {
    this.socketPath = opts.socketPath ?? discoverSocketPath();
    this.timeoutMs = opts.timeoutMs ?? 5000;
  }

  private async doRequest(request: string): Promise<Record<string, unknown>> {
    const line = await sendOneShot(this.socketPath, request, this.timeoutMs);
    return parseAndCheck(line);
  }

  /**
   * Read a cached value.
   *
   * @param key   Provider key, e.g. `"git.branch"` or `"git"`.
   * @param path  Optional repository/working-directory path.
   */
  async get(key: string, path?: string): Promise<CombResult> {
    const req = serialiseRequest(
      path !== undefined ? { op: 'get', key, path } : { op: 'get', key },
    );
    const parsed = await this.doRequest(req);
    return makeCombResult(parsed);
  }

  /**
   * Force recomputation of a provider.
   *
   * @param key   Provider key, e.g. `"git"`.
   * @param path  Optional repository path.
   */
  async poke(key: string, path?: string): Promise<void> {
    const req = serialiseRequest(
      path !== undefined ? { op: 'poke', key, path } : { op: 'poke', key },
    );
    await this.doRequest(req);
  }

  /**
   * List all available providers.
   */
  async list(): Promise<ProviderInfo[]> {
    const req = serialiseRequest({ op: 'list' });
    const parsed = await this.doRequest(req);
    if (!Array.isArray(parsed['data'])) {
      throw new ParseError(JSON.stringify(parsed), 'expected data array in list response');
    }
    return parsed['data'] as ProviderInfo[];
  }

  /**
   * Return daemon status information.
   */
  async status(): Promise<Record<string, unknown>> {
    const req = serialiseRequest({ op: 'status' });
    const parsed = await this.doRequest(req);
    if (typeof parsed['data'] !== 'object' || parsed['data'] === null) {
      throw new ParseError(JSON.stringify(parsed), 'expected data object in status response');
    }
    return parsed['data'] as Record<string, unknown>;
  }

  /**
   * Open a persistent session.  Remember to call `session.close()` when done.
   */
  async session(): Promise<Session> {
    return new Promise((resolve, reject) => {
      const socket = net.createConnection(this.socketPath);
      const timer = setTimeout(() => {
        socket.destroy();
        reject(new DaemonNotRunning(this.socketPath));
      }, this.timeoutMs);

      socket.on('connect', () => {
        clearTimeout(timer);
        resolve(new Session(socket));
      });

      socket.on('error', (err: NodeJS.ErrnoException) => {
        clearTimeout(timer);
        if (err.code === 'ENOENT' || err.code === 'ECONNREFUSED') {
          reject(new DaemonNotRunning(this.socketPath));
        } else {
          reject(err);
        }
      });
    });
  }
}
