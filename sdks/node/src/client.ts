/**
 * Client, Session and WatchStream implementations for the beachcomber
 * daemon, built over the C ABI (`libbeachcomber.{so,dylib}` via `koffi`,
 * or the `comb` subprocess fallback — see `transport_select.ts`).
 */

import { selectTransport } from './transport_select.js';
import { unwrap } from './envelope.js';
import type { ClientHandle, SessionHandle, Transport, WatchHandle } from './transport.js';
import type {
  HelloInfo,
  CacheRow,
  DaemonHealth,
  ReaperStatus,
  IntrospectSubject,
  IntrospectResponse,
  RowKind,
  Verdict,
  WatchEvent,
} from './types.js';

export type {
  HelloInfo,
  CacheRow,
  DaemonHealth,
  ReaperStatus,
  IntrospectSubject,
  IntrospectResponse,
  RowKind,
  Verdict,
  WatchEvent,
};

const BC_GET_FORCE = 1 << 0;
const BC_GET_WAIT = 1 << 1;

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

interface GetResultShape {
  data: unknown;
  age_ms: number | null;
  stale: boolean | null;
}

function makeCombResult(raw: GetResultShape): CombResult {
  // The ABI's Miss variant sets age_ms/stale to null alongside data; a Hit
  // always carries an age. That is the reliable hit/miss signal, since the
  // data value itself may legitimately be JSON null on a hit.
  const isHit = raw.age_ms !== null;
  const data = isHit ? raw.data : undefined;
  const ageMs = raw.age_ms ?? 0;
  const stale = raw.stale ?? false;

  return {
    ok: true,
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

// ---- Parse helpers ----

function parseHello(data: unknown): HelloInfo {
  const d = (data ?? {}) as Record<string, unknown>;
  return {
    protocolVersion: String(d['protocol_version'] ?? ''),
    daemonVersion: String(d['daemon_version'] ?? ''),
  };
}

function parseCacheRows(data: unknown): CacheRow[] {
  if (!Array.isArray(data)) {
    throw new TypeError('status data is not an array');
  }
  return data.map((row: unknown) => {
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
      pollsElapsed: r['polls_elapsed'] != null ? Number(r['polls_elapsed']) : undefined,
      fseventsReinstate: r['fsevents_reinstate'] != null ? Boolean(r['fsevents_reinstate']) : undefined,
      failure: r['failure'] != null ? (r['failure'] as CacheRow['failure']) : undefined,
      source: r['source'] != null ? String(r['source']) : undefined,
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
  const reaperRaw = d['reaper'] as Record<string, unknown> | null | undefined;
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
    watchBackend: d['watch_backend'] != null ? String(d['watch_backend']) : undefined,
    reaper: reaperRaw
      ? {
          armed: Boolean(reaperRaw['armed']),
          visibility: String(reaperRaw['visibility'] ?? ''),
          sweeps: Number(reaperRaw['sweeps'] ?? 0),
          reaped: Number(reaperRaw['reaped'] ?? 0),
          killDenied: Number(reaperRaw['kill_denied'] ?? 0),
        }
      : undefined,
    verdicts,
  };
}

function parseIntrospect(subject: IntrospectSubject, data: unknown): IntrospectResponse {
  if (subject === 'daemon') {
    return { subject: 'daemon', daemon: parseDaemonHealth(data) };
  }
  return { subject, other: data ?? null } as IntrospectResponse;
}

function flagsOf(opts?: { force?: boolean; wait?: boolean }): number {
  let flags = 0;
  if (opts?.force) flags |= BC_GET_FORCE;
  if (opts?.wait) flags |= BC_GET_WAIT;
  return flags;
}

// ---- WatchStream ----

/**
 * An AsyncIterable that yields WatchEvent values from an open watch handle.
 *
 * Iterate with `for await (const event of stream)`. Call `stream.close()`
 * to stop watching.
 */
export class WatchStream implements AsyncIterable<WatchEvent> {
  private closed = false;

  constructor(
    private readonly tp: Transport,
    private readonly handle: WatchHandle,
  ) {}

  async *[Symbol.asyncIterator](): AsyncIterator<WatchEvent> {
    try {
      while (!this.closed) {
        const result = await this.tp.watchNext(this.handle, -1);
        if (result.outcome === 'event') {
          yield { data: result.data ?? null, ageMs: result.ageMs ?? 0, stale: result.stale ?? false };
        } else if (result.outcome === 'eof' || result.outcome === 'cancelled') {
          return;
        }
        // 'timeout' does not occur with an indefinite (-1) wait.
      }
    } finally {
      this.tp.freeWatch(this.handle);
    }
  }

  close(): void {
    if (this.closed) return;
    this.closed = true;
    this.tp.watchCancel(this.handle);
  }
}

// ---- Session ----

/**
 * A persistent connection to the daemon (FFI transport) or a lightweight
 * context-remembering wrapper (subprocess transport — see
 * `subprocess_transport.ts` for why there is no real shared connection to
 * reuse there).
 *
 * Obtain a Session via `client.session()`. Call `session.close()` when done.
 */
export class Session {
  private closed = false;

  constructor(
    private readonly tp: Transport,
    private readonly clientHandle: ClientHandle,
    private readonly handle: SessionHandle,
  ) {}

  /** Set the default path for subsequent queries on this connection. */
  async setContext(repoPath: string): Promise<void> {
    unwrap(await this.tp.sessionSetContext(this.handle, repoPath));
  }

  /** Query a key. If `setContext` has been called, `path` can be omitted. */
  async get(key: string, path?: string): Promise<CombResult> {
    const result = unwrap(await this.tp.sessionGet(this.handle, key, path, 0)) as GetResultShape;
    return makeCombResult(result);
  }

  /** Query a key with optional force/wait flags. */
  async getWithFlags(
    key: string,
    path?: string,
    opts?: { force?: boolean; wait?: boolean },
  ): Promise<CombResult> {
    const result = unwrap(
      await this.tp.sessionGet(this.handle, key, path, flagsOf(opts)),
    ) as GetResultShape;
    return makeCombResult(result);
  }

  /**
   * Trigger recomputation of a provider. There is no session-scoped
   * `bc_session_refresh` in the ABI; this issues a one-shot client-level
   * refresh against the same daemon connection this client was constructed
   * with.
   */
  async refresh(key: string, path?: string): Promise<void> {
    unwrap(await this.tp.refresh(this.clientHandle, key, path));
  }

  /** Store data in the cache under the given key. */
  async put(key: string, data?: unknown, opts?: { ttl?: string; path?: string }): Promise<void> {
    const jsonData = JSON.stringify(data ?? {});
    unwrap(await this.tp.sessionPut(this.handle, key, jsonData, opts?.ttl, opts?.path));
  }

  /** Query daemon health/hello information (one-shot, not session-scoped in the ABI). */
  async hello(): Promise<HelloInfo> {
    return parseHello(unwrap(await this.tp.hello(this.clientHandle)));
  }

  /** Introspect an internal daemon subject (one-shot, not session-scoped in the ABI). */
  async introspect(
    subject: IntrospectSubject,
    opts?: { durationSecs?: number },
  ): Promise<IntrospectResponse> {
    const optionsJson =
      opts?.durationSecs !== undefined ? JSON.stringify({ duration_secs: opts.durationSecs }) : undefined;
    const data = unwrap(await this.tp.introspect(this.clientHandle, subject, optionsJson));
    return parseIntrospect(subject, data);
  }

  /** Return cache rows from the daemon (one-shot, not session-scoped in the ABI). */
  async status(): Promise<CacheRow[]> {
    return parseCacheRows(unwrap(await this.tp.status(this.clientHandle)));
  }

  /** Close the session handle. */
  close(): void {
    if (this.closed) return;
    this.closed = true;
    this.tp.closeSession(this.handle);
  }
}

// ---- Client ----

export interface ClientOptions {
  /** Override the auto-discovered socket path. */
  socketPath?: string;
  /** Connection + read timeout in milliseconds. Default: 5000. */
  timeoutMs?: number;
  /** Whether the underlying client may auto-start the daemon. Default: true. */
  autostart?: boolean;
}

export interface ResolveOptions {
  /** Required: path expressions resolve over `cwd`. Never defaults to the process's own cwd. */
  cwd: string;
  /** `env.*` references in the expression. Absent references resolve to `""`. */
  env?: Record<string, string>;
  /** Field-expression overrides (`"provider.field"`) or path-expression overrides (a bare provider name). */
  overrides?: Record<string, string>;
}

/**
 * Client for the beachcomber daemon, over the C ABI.
 *
 * Uses `koffi` FFI over `libbeachcomber.{so,dylib}` when available, or the
 * `comb` subprocess fallback otherwise — see `transport()`.
 */
export class Client {
  private readonly tp: Transport;
  private readonly handle: ClientHandle;

  constructor(opts: ClientOptions = {}) {
    this.tp = selectTransport();
    this.handle = this.tp.newClient({
      socketPath: opts.socketPath,
      timeoutMs: opts.timeoutMs,
      autostart: opts.autostart,
    });
  }

  /** Which transport this client is using: `"ffi"` (koffi) or `"subprocess"` (comb CLI). */
  transport(): 'ffi' | 'subprocess' {
    return this.tp.kind;
  }

  /**
   * Read a cached value.
   *
   * @param key   Provider key, e.g. `"git.branch"` or `"git"`.
   * @param path  Optional repository/working-directory path.
   */
  async get(key: string, path?: string): Promise<CombResult> {
    const result = unwrap(await this.tp.get(this.handle, key, path, 0)) as GetResultShape;
    return makeCombResult(result);
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
    const result = unwrap(await this.tp.get(this.handle, key, path, flagsOf(opts))) as GetResultShape;
    return makeCombResult(result);
  }

  /**
   * Force recomputation of a provider.
   *
   * @param key   Provider key, e.g. `"git"`.
   * @param path  Optional repository path.
   */
  async refresh(key: string, path?: string): Promise<void> {
    unwrap(await this.tp.refresh(this.handle, key, path));
  }

  /**
   * Store data in the cache under the given key.
   *
   * @param key   Provider key (e.g. "myapp").
   * @param data  Object payload to store.
   * @param opts  Optional ttl string and path.
   */
  async put(key: string, data?: unknown, opts?: { ttl?: string; path?: string }): Promise<void> {
    const jsonData = JSON.stringify(data ?? {});
    unwrap(await this.tp.put(this.handle, key, jsonData, opts?.ttl, opts?.path));
  }

  /**
   * Clear the cached entry for a virtual provider key without dropping the
   * registry entry.
   */
  async putNull(key: string, path?: string): Promise<void> {
    unwrap(await this.tp.putNull(this.handle, key, path));
  }

  /** Query daemon protocol and version information. */
  async hello(): Promise<HelloInfo> {
    return parseHello(unwrap(await this.tp.hello(this.handle)));
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
    const optionsJson =
      opts?.durationSecs !== undefined ? JSON.stringify({ duration_secs: opts.durationSecs }) : undefined;
    const data = unwrap(await this.tp.introspect(this.handle, subject, optionsJson));
    return parseIntrospect(subject, data);
  }

  /** Return cache rows from the daemon. */
  async status(): Promise<CacheRow[]> {
    return parseCacheRows(unwrap(await this.tp.status(this.handle)));
  }

  /**
   * Resolve a virtual field (`key = "provider.field"`) or a path expression
   * (`key` = a bare provider name) client-side — exactly as `comb get`'s
   * resolution layer does. `cwd` is required: this library never falls back
   * to the process's own working directory.
   */
  async resolve(key: string, opts: ResolveOptions): Promise<unknown> {
    const envJson = opts.env !== undefined ? JSON.stringify(opts.env) : undefined;
    const overridesJson = opts.overrides !== undefined ? JSON.stringify(opts.overrides) : undefined;
    return unwrap(await this.tp.resolve(this.handle, key, opts.cwd, envJson, overridesJson));
  }

  /**
   * Evaluate an arbitrary expression string — the same evaluator `resolve`
   * uses for a declared virtual field, but for a raw expression that need
   * not be registered anywhere.
   */
  async eval(templateStr: string, opts: ResolveOptions): Promise<unknown> {
    const envJson = opts.env !== undefined ? JSON.stringify(opts.env) : undefined;
    const overridesJson = opts.overrides !== undefined ? JSON.stringify(opts.overrides) : undefined;
    return unwrap(await this.tp.evaluate(this.handle, templateStr, opts.cwd, envJson, overridesJson));
  }

  /**
   * Open a watch stream for a key. The stream is an AsyncIterable<WatchEvent>.
   * Call `stream.close()` to stop watching.
   *
   * @param key   Provider key, e.g. `"git.branch"`.
   * @param path  Optional repository path.
   */
  async watch(key: string, path?: string): Promise<WatchStream> {
    const handle = this.tp.openWatch(this.handle, key, path);
    return new WatchStream(this.tp, handle);
  }

  /**
   * Open a persistent session. Remember to call `session.close()` when done.
   */
  async session(): Promise<Session> {
    const handle = this.tp.openSession(this.handle);
    return new Session(this.tp, this.handle, handle);
  }

  /** Release the underlying client handle. */
  close(): void {
    this.tp.freeClient(this.handle);
  }
}
