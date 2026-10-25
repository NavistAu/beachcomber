/**
 * Client, Session and WatchStream for the beachcomber daemon, built over the
 * C ABI (`libbeachcomber.{so,dylib}` via `koffi`, or the `comb` subprocess
 * fallback — see `transport_select.ts`). The surface is the one SDK API
 * table, docs/sdk-api.md, which every SDK follows.
 */

import { selectTransport } from './transport_select.js';
import { unwrap } from './envelope.js';
import { IoError, TimeoutError } from './errors.js';
import { parseCacheRows, parseHello, parseIntrospect, parseWatchEvent } from './parse.js';
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
 * A watch on a key's value: each event is what `get` answers for the key,
 * read again whenever any key the read depends on changes, including the
 * provider's template tree.
 *
 * Read it with `nextEvent()`, or iterate it with
 * `for await (const event of stream)`, which ends where `nextEvent` answers
 * `null`. `cancel()` ends the stream; `close()` ends it and releases it.
 */
export class WatchStream implements AsyncIterable<WatchEvent> {
  private cancelled = false;
  private ended = false;
  private freed = false;
  /** The `bc_watch_next` in flight, so `close` frees the handle only once it returns. */
  private pending: Promise<unknown> | undefined;

  constructor(
    private readonly tp: Transport,
    private readonly handle: WatchHandle,
  ) {}

  /**
   * The next event, or `null` once the stream has ended (the daemon closed
   * it, or it was cancelled). An event whose `data` is `null` is a miss, not
   * the end. `timeoutMs` omitted waits indefinitely; one that runs out with
   * no event rejects with `TimeoutError`.
   */
  async nextEvent(timeoutMs?: number): Promise<WatchEvent | null> {
    if (this.ended || this.freed) return null;
    const next = this.tp.watchNext(this.handle, timeoutMs ?? -1);
    this.pending = next;
    let result;
    try {
      result = await next;
    } finally {
      this.pending = undefined;
    }
    if (result.outcome === 'event') return parseWatchEvent(result.data);
    if (result.outcome === 'timeout') {
      throw new TimeoutError(`no event within ${timeoutMs}ms`, this.tp.libraryVersion);
    }
    this.ended = true;
    return null;
  }

  async *[Symbol.asyncIterator](): AsyncIterator<WatchEvent> {
    try {
      for (;;) {
        const event = await this.nextEvent();
        if (event === null) return;
        yield event;
      }
    } finally {
      this.close();
    }
  }

  /** End the stream: a `nextEvent` waiting now, or any later one, answers `null`. */
  cancel(): void {
    if (this.cancelled || this.freed) return;
    this.cancelled = true;
    this.tp.watchCancel(this.handle);
  }

  /** End the stream and release it, once any `nextEvent` in flight has returned. */
  close(): void {
    this.cancel();
    if (this.freed) return;
    const free = () => {
      if (this.freed) return;
      this.freed = true;
      this.tp.freeWatch(this.handle);
    };
    if (this.pending) {
      this.pending.then(free, free);
    } else {
      free();
    }
  }
}

// ---- Session ----

/**
 * A persistent connection to the daemon (FFI transport) or a lightweight
 * context-remembering wrapper (subprocess transport — see
 * `subprocess_transport.ts` for why there is no real shared connection to
 * reuse there), with a directory of its own.
 *
 * Obtain a Session via `client.session()`. Call `session.close()` when done.
 * `status`, `hello` and `introspect` address no directory and need no
 * connection of their own, so they are the client's alone.
 */
export class Session {
  private closed = false;

  constructor(
    private readonly tp: Transport,
    private readonly handle: SessionHandle,
  ) {}

  /**
   * Set this session's directory: every later `get` that names no `cwd`, and
   * `put` and `refresh` that name no `path`, address it.
   */
  async setContext(repoPath: string): Promise<void> {
    unwrap(await this.tp.sessionSetContext(this.#open(), repoPath), this.tp.libraryVersion);
  }

  /**
   * `Client.get` on this session's connection. An omitted `cwd` is this
   * session's directory (`setContext`) when it has one, else the global
   * entry.
   */
  async get(key: string, opts: GetOptions = {}): Promise<unknown> {
    return unwrap(
      await this.tp.sessionGet(this.#open(), key, opts.cwd, envJsonOf(opts.env), flagsOf(opts)),
      this.tp.libraryVersion,
    );
  }

  /**
   * Trigger recomputation of a provider at `path`, or at the session's
   * directory (`setContext`) when `path` is omitted.
   */
  async refresh(key: string, path?: string): Promise<void> {
    unwrap(await this.tp.sessionRefresh(this.#open(), key, path), this.tp.libraryVersion);
  }

  /** Store data in the cache under the given key, at `path` or the session's directory. */
  async put(key: string, data: unknown, opts?: { ttl?: string; path?: string }): Promise<void> {
    const jsonData = JSON.stringify(data);
    unwrap(await this.tp.sessionPut(this.#open(), key, jsonData, opts?.ttl, opts?.path), this.tp.libraryVersion);
  }

  /** Close the session handle. */
  close(): void {
    if (this.closed) return;
    this.closed = true;
    this.tp.closeSession(this.handle);
  }

  /**
   * The handle every call passes on. A closed session's is freed, and the
   * library must never see a freed handle, so the call fails here.
   */
  #open(): SessionHandle {
    if (this.closed) throw new IoError('the session is closed', this.tp.libraryVersion);
    return this.handle;
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
   * `exec()`. Unset, the library's default (off) — a template that calls it
   * then fails with `exec() is disabled in this context`.
   */
  exec?: boolean;
}

/** The inputs of an `eval` or a `watch`: the directory and the caller's environment. */
export interface ReadOptions {
  /**
   * The directory the read addresses (`~` is home, a relative path is
   * against this process's working directory). Omitted, the global entry.
   */
  cwd?: string;
  /** The caller's environment for `env.*` references. Omitted, the calling process's environment. */
  env?: Record<string, string>;
}

/** The inputs of a `get`: a read's, and its flags. */
export interface GetOptions extends ReadOptions {
  /** Re-execute every Source the read demands before answering. */
  force?: boolean;
  /** Re-execute a stale entry inline before answering. */
  wait?: boolean;
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
  private closed = false;

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
    return unwrap(
      await this.tp.get(this.#open(), key, opts.cwd, envJsonOf(opts.env), flagsOf(opts)),
      this.tp.libraryVersion,
    );
  }

  /**
   * Force recomputation of a provider.
   *
   * @param key   Provider key, e.g. `"git"`.
   * @param path  Optional repository path.
   */
  async refresh(key: string, path?: string): Promise<void> {
    unwrap(await this.tp.refresh(this.#open(), key, path), this.tp.libraryVersion);
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
    unwrap(await this.tp.put(this.#open(), key, jsonData, opts?.ttl, opts?.path), this.tp.libraryVersion);
  }

  /** Query daemon protocol and version information. */
  async hello(): Promise<HelloInfo> {
    return parseHello(unwrap(await this.tp.hello(this.#open()), this.tp.libraryVersion));
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
    const data = unwrap(await this.tp.introspect(this.#open(), subject, optionsJson), this.tp.libraryVersion);
    return parseIntrospect(subject, data);
  }

  /** Return cache rows from the daemon. */
  async status(): Promise<CacheRow[]> {
    return parseCacheRows(unwrap(await this.tp.status(this.#open()), this.tp.libraryVersion), this.tp.libraryVersion);
  }

  /**
   * Render a template that is stored nowhere — the same value `comb eval`
   * prints. Every ref resolves exactly as it would inside a stored
   * template, so `eval("{{ p.f }}")` and `get("p.f")` agree. `cwd` and
   * `env` mean what they mean for `get`.
   *
   * `templateStr` accepts a single `{{ expr }}` tag, or literal
   * text/several tags — the former keeps the expression's natural type,
   * the latter is always a string (a source with no tags at all is its
   * literal text).
   */
  async eval(templateStr: string, opts: ReadOptions = {}): Promise<unknown> {
    return unwrap(
      await this.tp.evaluate(this.#open(), templateStr, opts.cwd, envJsonOf(opts.env)),
      this.tp.libraryVersion,
    );
  }

  /**
   * Watch a key's *value* — what `get` answers for it — and receive it
   * again whenever it changes, so a template edited under the watch fires it
   * just as a changed cache record does. `cwd` and `env` mean what they mean
   * for `get`.
   *
   * @param key   Provider key, e.g. `"git.branch"`.
   * @param opts  `cwd` (omitted: the global entry) and `env`.
   */
  async watch(key: string, opts: ReadOptions = {}): Promise<WatchStream> {
    const handle = this.tp.openWatch(this.#open(), key, opts.cwd, envJsonOf(opts.env));
    return new WatchStream(this.tp, handle);
  }

  /**
   * Open a persistent session. Remember to call `session.close()` when done.
   */
  async session(): Promise<Session> {
    return new Session(this.tp, this.tp.openSession(this.#open()));
  }

  /** Release the underlying client handle. Idempotent. */
  close(): void {
    if (this.closed) return;
    this.closed = true;
    this.tp.freeClient(this.handle);
  }

  /**
   * The handle every call passes on. A closed client's is freed, and the
   * library must never see a freed handle, so the call fails here.
   */
  #open(): ClientHandle {
    if (this.closed) throw new IoError('the client is closed', this.tp.libraryVersion);
    return this.handle;
  }
}
