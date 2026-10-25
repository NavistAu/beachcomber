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
      pollCount: r['poll_count'] != null ? Number(r['poll_count']) : undefined,
      pollsElapsed: r['polls_elapsed'] != null ? Number(r['polls_elapsed']) : undefined,
      nextPollInSecs: r['next_poll_in_secs'] != null ? Number(r['next_poll_in_secs']) : undefined,
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

function flagsOf(opts: GetOptions): number {
  let flags = 0;
  if (opts.force) flags |= BC_GET_FORCE;
  if (opts.wait) flags |= BC_GET_WAIT;
  return flags;
}

function envJsonOf(env: Record<string, string> | undefined): string | undefined {
  return env !== undefined ? JSON.stringify(env) : undefined;
}

// ---- WatchStream ----

/**
 * An AsyncIterable that yields WatchEvent values from an open watch handle.
 *
 * Each event is the value of the watched key — what `comb get` would print
 * for it, and what `Client.get` answers — read again whenever any key the
 * read depends on changes, including the provider's template tree.
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
          yield { data: result.data ?? null };
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

  /**
   * Set this session's directory: every later `get` that names no `cwd`, and
   * `put` and `refresh` that name no `path`, address it.
   */
  async setContext(repoPath: string): Promise<void> {
    unwrap(await this.tp.sessionSetContext(this.handle, repoPath));
  }

  /**
   * `Client.get` on this session's connection. An omitted `cwd` is this
   * session's directory (`setContext`) when it has one, else the global
   * entry.
   */
  async get(key: string, opts: GetOptions = {}): Promise<unknown> {
    return unwrap(
      await this.tp.sessionGet(this.handle, key, opts.cwd, envJsonOf(opts.env), flagsOf(opts)),
    );
  }

  /**
   * Trigger recomputation of a provider at `path`, or at the session's
   * directory (`setContext`) when `path` is omitted.
   */
  async refresh(key: string, path?: string): Promise<void> {
    unwrap(await this.tp.sessionRefresh(this.handle, key, path));
  }

  /** Store data in the cache under the given key. */
  async put(key: string, data: unknown, opts?: { ttl?: string; path?: string }): Promise<void> {
    const jsonData = JSON.stringify(data);
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
  /**
   * How long one read or write on the daemon's socket may take, in
   * milliseconds (`bc_client_new`'s `timeout_ms`). Unset, the library's
   * default applies (100); this SDK has none of its own. 0 is refused.
   * Over the subprocess transport it bounds each spawned `comb`.
   */
  timeoutMs?: number;
  /**
   * How long connecting keeps retrying a socket nobody answers yet, in
   * milliseconds (`bc_client_new`'s `connect_timeout_ms`); 0 is a single
   * attempt. Unset, the library's default applies (1500). The subprocess
   * transport refuses it: `comb` has no such flag.
   */
  connectTimeoutMs?: number;
  /** Whether the underlying client may auto-start the daemon. Unset, the library's default (on). */
  autostart?: boolean;
  /**
   * Whether a template this client renders (`get`, `eval`) may call
   * `exec()`. Default: false — a template that calls it then fails with
   * `exec() is disabled in this context`.
   */
  exec?: boolean;
}

/** The inputs of a `get`: the directory, the caller's environment and the read's flags. */
export interface GetOptions {
  /**
   * The directory the read addresses (`~` is home, a relative path is
   * against this process's working directory). Omitted, the global entry.
   */
  cwd?: string;
  /** The caller's environment for `env.*` references. Omitted, none: every `env.*` is absent. */
  env?: Record<string, string>;
  /** Re-execute every Source the read demands before answering. */
  force?: boolean;
  /** Re-execute a stale entry inline before answering. */
  wait?: boolean;
}

/** The inputs of an `eval` or a `watch`. */
export interface ResolveOptions {
  /** Required: every fetch the read makes addresses `cwd`, read as `get` reads it. */
  cwd: string;
  /** The caller's environment for `env.*` references. Omitted, none: every `env.*` is absent. */
  env?: Record<string, string>;
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
      connectTimeoutMs: opts.connectTimeoutMs,
      autostart: opts.autostart,
      exec: opts.exec,
    });
  }

  /** Which transport this client is using: `"ffi"` (koffi) or `"subprocess"` (comb CLI). */
  transport(): 'ffi' | 'subprocess' {
    return this.tp.kind;
  }

  /**
   * The value `comb get` prints for this key, cwd and env.
   *
   * The daemon holds the templates (`<provider>:template`, seeded from
   * configuration or `put`); this client renders them. A plain field is
   * its cached value, a templated one is rendered, and a bare provider
   * name is the whole merged namespace. The unrendered halves are facets,
   * read like any key: `"p:template.x"` is the template as stored,
   * `"p:put.x"` and `"p:source.x"` those layers' values.
   *
   * @param key   `"provider.field"`, or a bare provider name.
   * @param opts  `cwd` (omitted: the global entry), `env` (omitted: no
   *              environment), and the `force`/`wait` flags.
   * @returns     The value, or `null` on a miss.
   */
  async get(key: string, opts: GetOptions = {}): Promise<unknown> {
    return unwrap(await this.tp.get(this.handle, key, opts.cwd, envJsonOf(opts.env), flagsOf(opts)));
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
   * @param data  JSON payload to store; `null` removes the node.
   * @param opts  Optional ttl string and path.
   */
  async put(key: string, data: unknown, opts?: { ttl?: string; path?: string }): Promise<void> {
    const jsonData = JSON.stringify(data);
    unwrap(await this.tp.put(this.handle, key, jsonData, opts?.ttl, opts?.path));
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
   * Render a template that is stored nowhere — the same value `comb eval`
   * prints. Every ref resolves exactly as it would inside a stored
   * template, so `eval("{{ p.f }}")` and `get("p.f")` agree.
   *
   * `templateStr` accepts a single `{{ expr }}` tag, or literal
   * text/several tags — the former keeps the expression's natural type,
   * the latter is always a string (a source with no tags at all is its
   * literal text).
   */
  async eval(templateStr: string, opts: ResolveOptions): Promise<unknown> {
    return unwrap(await this.tp.evaluate(this.handle, templateStr, opts.cwd, envJsonOf(opts.env)));
  }

  /**
   * Watch a key's *value* — what `get` answers for it — and receive it
   * again whenever it changes, so a template edited under the watch fires it
   * just as a changed cache record does. The stream is an
   * AsyncIterable<WatchEvent>; call `stream.close()` to stop watching.
   *
   * `cwd` and `env` mean what they mean for `get`, except that `cwd` is
   * required here.
   *
   * @param key   Provider key, e.g. `"git.branch"`.
   * @param opts  `cwd` (required) and `env`, as `get` reads them.
   */
  async watch(key: string, opts: ResolveOptions): Promise<WatchStream> {
    const handle = this.tp.openWatch(this.handle, key, opts.cwd, envJsonOf(opts.env));
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
