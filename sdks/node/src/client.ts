/**
 * Client and Session implementations for the beachcomber daemon.
 */

import * as net from 'net';
import { discoverSocketPath } from './discovery.js';
import { DaemonNotRunning, ParseError, ServerError } from './errors.js';
import {
  parseResponseLine,
  serialiseRequest,
} from './protocol.js';
import type {
  HelloInfo,
  CacheRow,
  DaemonHealth,
  IntrospectSubject,
  IntrospectResponse,
  RowKind,
  Verdict,
  WatchEvent,
} from './types.js';

export type { HelloInfo, CacheRow, DaemonHealth, IntrospectSubject, IntrospectResponse, RowKind, Verdict, WatchEvent };

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

const RETRY_BACKOFFS_MS = [250, 500, 1000];

/**
 * Connect to a Unix socket with 3 retries (250ms/500ms/1s exponential backoff).
 * Retries on ECONNREFUSED and ENOENT only — other errors surface immediately.
 * Covers the brief restart window when the daemon is restarting.
 */
export function connectWithRetry(path: string): Promise<net.Socket> {
  return new Promise((resolve, reject) => {
    let attempt = 0;
    const tryConnect = () => {
      const sock = net.createConnection(path);
      sock.once('connect', () => resolve(sock));
      sock.once('error', (err: NodeJS.ErrnoException) => {
        if (err.code !== 'ECONNREFUSED' && err.code !== 'ENOENT') {
          reject(err);
          return;
        }
        if (attempt >= RETRY_BACKOFFS_MS.length) {
          reject(err);
          return;
        }
        const backoff = RETRY_BACKOFFS_MS[attempt];
        attempt++;
        setTimeout(tryConnect, backoff);
      });
    };
    tryConnect();
  });
}

interface ClientOptions {
  /** Override the auto-discovered socket path. */
  socketPath?: string;
  /** Connection + read timeout in milliseconds. Default: 5000. */
  timeoutMs?: number;
}

/**
 * Open a TCP/Unix connection, send one newline-delimited JSON request, and
 * resolve with the trimmed response line.  The socket is destroyed after
 * the response is received.  Uses connectWithRetry to tolerate the brief
 * restart window when the daemon is restarting.
 */
async function sendOneShot(socketPath: string, request: string, timeoutMs: number): Promise<string> {
  let socket: net.Socket;
  try {
    socket = await connectWithRetry(socketPath);
  } catch (err: unknown) {
    throw new DaemonNotRunning(socketPath);
  }

  return new Promise((resolve, reject) => {
    let responded = false;
    let buffer = '';

    const timer = setTimeout(() => {
      if (!responded) {
        responded = true;
        socket.destroy();
        reject(new DaemonNotRunning(socketPath));
      }
    }, timeoutMs);

    socket.write(request);

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

// ---- Parse helpers ----

function parseHello(resp: Record<string, unknown>): HelloInfo {
  const data = (resp['data'] ?? {}) as Record<string, unknown>;
  return {
    protocolVersion: String(data['protocol_version'] ?? ''),
    daemonVersion: String(data['daemon_version'] ?? ''),
  };
}

function parseCacheRows(resp: Record<string, unknown>): CacheRow[] {
  if (!Array.isArray(resp['data'])) {
    throw new ParseError(JSON.stringify(resp), 'status data is not an array');
  }
  return (resp['data'] as unknown[]).map((row: unknown) => {
    const r = row as Record<string, unknown>;
    return {
      provider: String(r['provider'] ?? ''),
      field: r['field'] != null ? String(r['field']) : null,
      path: r['path'] != null ? String(r['path']) : null,
      value: r['value'],
      ageMs: Number(r['age_ms'] ?? 0),
      stale: Boolean(r['stale']),
      kind: r['kind'] != null ? (r['kind'] as RowKind) : undefined,
      pollIntervalSecs: r['poll_interval_secs'] != null ? Number(r['poll_interval_secs']) : undefined,
      keepAlivePolls: r['keep_alive_polls'] != null ? Number(r['keep_alive_polls']) : undefined,
      fseventsReinstate: r['fsevents_reinstate'] != null ? Boolean(r['fsevents_reinstate']) : undefined,
      failure: r['failure'] != null ? (r['failure'] as CacheRow['failure']) : undefined,
    };
  });
}

function parseDaemonHealth(data: unknown): DaemonHealth {
  const d = (data ?? {}) as Record<string, unknown>;
  const verdicts: Verdict[] = Array.isArray(d['verdicts'])
    ? (d['verdicts'] as unknown[]).map((v: unknown) => {
        const vr = v as Record<string, unknown>;
        return {
          level: String(vr['level'] ?? ''),
          message: String(vr['message'] ?? ''),
        };
      })
    : [];
  return {
    pid: Number(d['pid'] ?? 0),
    version: String(d['version'] ?? ''),
    uptimeSecs: Number(d['uptime_secs'] ?? 0),
    socketPath: String(d['socket_path'] ?? ''),
    configPath: d['config_path'] != null ? String(d['config_path']) : null,
    requestsTotal: Number(d['requests_total'] ?? 0),
    inFlight: Number(d['in_flight'] ?? 0),
    activeWatchers: Number(d['active_watchers'] ?? 0),
    cacheEntries: Number(d['cache_entries'] ?? 0),
    verdicts,
  };
}

function parseIntrospect(subject: IntrospectSubject, resp: Record<string, unknown>): IntrospectResponse {
  if (subject === 'daemon') {
    return { subject: 'daemon', daemon: parseDaemonHealth(resp['data']) };
  }
  return { subject, other: resp['data'] ?? null } as IntrospectResponse;
}

// ---- WatchStream ----

/**
 * An AsyncIterable that yields WatchEvent values from a persistent socket
 * connection opened for an 'op:watch' request.
 *
 * Iterate with `for await (const event of stream)`.  Call `stream.close()`
 * to stop watching.
 */
export class WatchStream implements AsyncIterable<WatchEvent> {
  constructor(private readonly socket: net.Socket) {}

  async *[Symbol.asyncIterator](): AsyncIterator<WatchEvent> {
    let buffer = '';
    for await (const chunk of this.socket) {
      buffer += (chunk as Buffer).toString('utf8');
      let idx: number;
      while ((idx = buffer.indexOf('\n')) !== -1) {
        const line = buffer.slice(0, idx).trim();
        buffer = buffer.slice(idx + 1);
        if (!line) continue;
        let resp: Record<string, unknown>;
        try {
          resp = parseResponseLine(line);
        } catch (e) {
          throw new ParseError(line, e instanceof Error ? e.message : String(e));
        }
        if (resp['ok'] === false) {
          const msg = typeof resp['error'] === 'string' ? resp['error'] : 'watch error';
          throw new ServerError(msg);
        }
        yield {
          data: resp['data'] ?? null,
          ageMs: Number(resp['age_ms'] ?? 0),
          stale: Boolean(resp['stale']),
        };
      }
    }
  }

  close(): void {
    this.socket.destroy();
  }
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
   * After calling this, `get` and `refresh` calls can omit the path.
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
   * Query a key with optional force/wait flags.
   *
   * @param key   Provider key.
   * @param path  Optional path.
   * @param opts  Optional flags: force recomputation, wait for fresh value.
   */
  async getWithFlags(
    key: string,
    path?: string,
    opts?: { force?: boolean; wait?: boolean },
  ): Promise<CombResult> {
    const baseReq: Record<string, unknown> = { op: 'get', key };
    if (path !== undefined) baseReq['path'] = path;
    if (opts?.force) baseReq['force'] = true;
    if (opts?.wait) baseReq['wait'] = true;
    const req = serialiseRequest(baseReq as unknown as Parameters<typeof serialiseRequest>[0]);
    const line = await this.sendAndReceive(req);
    const parsed = parseAndCheck(line);
    return makeCombResult(parsed);
  }

  /**
   * Trigger recomputation of a provider.
   */
  async refresh(key: string, path?: string): Promise<void> {
    const req = serialiseRequest(
      path !== undefined ? { op: 'refresh', key, path } : { op: 'refresh', key },
    );
    const line = await this.sendAndReceive(req);
    parseAndCheck(line);
  }

  /**
   * Store data in the cache under the given key.
   *
   * @param key   Provider key (e.g. "myprovider").
   * @param data  Object payload to store.
   * @param opts  Optional ttl string and path.
   */
  async put(
    key: string,
    data?: unknown,
    opts?: { ttl?: string; path?: string },
  ): Promise<void> {
    const baseReq: Record<string, unknown> = { op: 'put', key };
    if (data !== undefined) baseReq['data'] = data;
    if (opts?.ttl !== undefined) baseReq['ttl'] = opts.ttl;
    if (opts?.path !== undefined) baseReq['path'] = opts.path;
    const req = serialiseRequest(baseReq as unknown as Parameters<typeof serialiseRequest>[0]);
    const line = await this.sendAndReceive(req);
    parseAndCheck(line);
  }

  /**
   * Query daemon health/hello information.
   */
  async hello(): Promise<HelloInfo> {
    const req = serialiseRequest({ op: 'hello' });
    const line = await this.sendAndReceive(req);
    const parsed = parseAndCheck(line);
    return parseHello(parsed);
  }

  /**
   * Introspect an internal daemon subject.
   */
  async introspect(
    subject: IntrospectSubject,
    opts?: { durationSecs?: number },
  ): Promise<IntrospectResponse> {
    const baseReq: Record<string, unknown> = { op: 'introspect', subject };
    if (opts?.durationSecs !== undefined) baseReq['duration_secs'] = opts.durationSecs;
    const req = serialiseRequest(baseReq as unknown as Parameters<typeof serialiseRequest>[0]);
    const line = await this.sendAndReceive(req);
    const parsed = parseAndCheck(line);
    return parseIntrospect(subject, parsed);
  }

  /**
   * Return cache rows from the daemon.
   */
  async status(): Promise<CacheRow[]> {
    const req = serialiseRequest({ op: 'status' });
    const line = await this.sendAndReceive(req);
    const parsed = parseAndCheck(line);
    return parseCacheRows(parsed);
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
  async refresh(key: string, path?: string): Promise<void> {
    const req = serialiseRequest(
      path !== undefined ? { op: 'refresh', key, path } : { op: 'refresh', key },
    );
    await this.doRequest(req);
  }

  /**
   * Read a cached value with optional force/wait flags.
   *
   * @param key   Provider key.
   * @param path  Optional path.
   * @param opts  Optional flags: force recomputation, wait for fresh value.
   */
  async getWithFlags(
    key: string,
    path?: string,
    opts?: { force?: boolean; wait?: boolean },
  ): Promise<CombResult> {
    const baseReq: Record<string, unknown> = { op: 'get', key };
    if (path !== undefined) baseReq['path'] = path;
    if (opts?.force) baseReq['force'] = true;
    if (opts?.wait) baseReq['wait'] = true;
    const req = serialiseRequest(baseReq as unknown as Parameters<typeof serialiseRequest>[0]);
    const parsed = await this.doRequest(req);
    return makeCombResult(parsed);
  }

  /**
   * Query daemon protocol and version information.
   */
  async hello(): Promise<HelloInfo> {
    const req = serialiseRequest({ op: 'hello' });
    const parsed = await this.doRequest(req);
    return parseHello(parsed);
  }

  /**
   * Store data in the cache under the given key.
   *
   * @param key   Provider key (e.g. "myprovider").
   * @param data  Object payload to store.
   * @param opts  Optional ttl string and path.
   */
  async put(
    key: string,
    data?: unknown,
    opts?: { ttl?: string; path?: string },
  ): Promise<void> {
    const baseReq: Record<string, unknown> = { op: 'put', key };
    if (data !== undefined) baseReq['data'] = data;
    if (opts?.ttl !== undefined) baseReq['ttl'] = opts.ttl;
    if (opts?.path !== undefined) baseReq['path'] = opts.path;
    const req = serialiseRequest(baseReq as unknown as Parameters<typeof serialiseRequest>[0]);
    await this.doRequest(req);
  }

  /**
   * Introspect an internal daemon subject.
   *
   * @param subject  The subsystem to inspect.
   * @param opts     Optional durationSecs for profiling subjects.
   */
  async introspect(
    subject: IntrospectSubject,
    opts?: { durationSecs?: number },
  ): Promise<IntrospectResponse> {
    const baseReq: Record<string, unknown> = { op: 'introspect', subject };
    if (opts?.durationSecs !== undefined) baseReq['duration_secs'] = opts.durationSecs;
    const req = serialiseRequest(baseReq as unknown as Parameters<typeof serialiseRequest>[0]);
    const parsed = await this.doRequest(req);
    return parseIntrospect(subject, parsed);
  }

  /**
   * Return cache rows from the daemon.
   */
  async status(): Promise<CacheRow[]> {
    const req = serialiseRequest({ op: 'status' });
    const parsed = await this.doRequest(req);
    return parseCacheRows(parsed);
  }

  /**
   * Open a watch stream for a key.  The stream is an AsyncIterable<WatchEvent>.
   * Call `stream.close()` to stop watching.
   *
   * @param key   Provider key, e.g. `"git.branch"`.
   * @param path  Optional repository path.
   */
  async watch(key: string, path?: string): Promise<WatchStream> {
    let socket: net.Socket;
    try {
      socket = await connectWithRetry(this.socketPath);
    } catch {
      throw new DaemonNotRunning(this.socketPath);
    }
    const baseReq: Record<string, unknown> = { op: 'watch', key };
    if (path !== undefined) baseReq['path'] = path;
    socket.write(serialiseRequest(baseReq as unknown as Parameters<typeof serialiseRequest>[0]));
    return new WatchStream(socket);
  }

  /**
   * Open a persistent session.  Remember to call `session.close()` when done.
   */
  async session(): Promise<Session> {
    let socket: net.Socket;
    try {
      socket = await connectWithRetry(this.socketPath);
    } catch {
      throw new DaemonNotRunning(this.socketPath);
    }
    return new Session(socket);
  }
}
