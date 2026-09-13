/**
 * The subprocess transport: shells out to `comb`.
 *
 * Sound only because `comb` links `libbeachcomber` directly — the same code
 * the cdylib exposes — so both transports run the same implementation. This
 * is the documented slow tier (~5ms per call against ~0.3ms over the
 * socket via FFI): a process spawn per call, entered only when `koffi` is
 * absent by configuration (see `transport_select.ts`).
 *
 * Coverage is deliberately narrower than the FFI transport. `comb`'s CLI
 * has direct equivalents for get/put/put_null/status/watch, and session
 * semantics are emulated client-side (remembering a context path between
 * calls — there is no real persistent connection to share, since each op
 * is its own process). `hello`, `introspect` and `refresh` have **no**
 * faithful CLI equivalent:
 *
 *   - `comb` exposes no JSON-emitting subcommand for `hello`/`introspect`.
 *   - `get --force` is not a stand-in for `refresh`: `refresh` on a key
 *     naming no Source is a documented error ("nothing to refresh: <key>
 *     has no sources"), and `comb get --force` on the same key errors too
 *     ("no source to re-execute from") — differently worded, but neither
 *     is a silent no-op.
 *
 * `resolve`/`eval` are simply not implemented over this tier. `comb get
 * --path <cwd> -f json` and `comb eval <template> <cwd>` resolve through
 * the same `libbeachcomber::resolve` the FFI does, so a faithful shell-out
 * exists; nobody has wired it yet.
 *
 * Rather than fake structured data from scraped human-readable text or
 * silently return the wrong answer, all five ops throw
 * `UnsupportedTransportError` naming the FFI transport as the
 * full-fidelity path.
 *
 * `get` does not read its value from `comb get` either: that command
 * *resolves* — it prints the rendered value, not the raw node `bc_get`
 * returns — so the raw value comes from the `comb status -f json` rows for
 * the key's provider instead, the way the Lua subprocess backend reads it
 * (`sdks/lua/beachcomber/subprocess_backend.lua`). Those rows are the
 * `<p>` provider's cached values, not its `<p>:template` tree, so this tier
 * never sees the template half: a result from here never carries
 * `template` or `rest`, and a template-only node reads as a miss. A caller
 * that needs the template half needs the FFI transport.
 *
 * `comb get` and `comb put` both default an omitted `--path` to the CLI
 * process's own working directory, while an absent `path` here means the
 * global entry — the same thing `path: NULL` means under FFI, and the same
 * thing a wire request with no `path` field means. So a pathless call asks
 * for the global entry by name, with `--global`.
 */

import { spawn, spawnSync } from 'child_process';
import { realpathSync } from 'fs';
import { ServerError, UnsupportedTransportError } from './errors.js';
import type { Envelope, WatchNextResult } from './envelope.js';
import type { NewClientOptions, Transport } from './transport.js';

interface SubprocessClientHandle {
  combBin: string;
  socketPath: string | undefined;
  timeoutMs: number;
}

interface SubprocessSessionHandle {
  client: SubprocessClientHandle;
  contextPath: string | undefined;
}

interface SubprocessWatchHandle {
  child: ReturnType<typeof spawn>;
  buffer: string;
  lines: string[];
  closed: boolean;
  cancelled: boolean;
}

function unsupported(op: string): never {
  throw new UnsupportedTransportError(
    `${op} is not supported over the subprocess transport (no koffi available) — ` +
      `comb's CLI has no faithful JSON equivalent for it. Install the optional ` +
      `'koffi' peer dependency for the full-fidelity FFI transport.`,
  );
}

function resolveCombBin(): string {
  return process.env['COMB_BIN'] || 'comb';
}

function runComb(
  combBin: string,
  args: string[],
  opts: { socketPath?: string; timeoutMs: number },
): { code: number | null; stdout: string; stderr: string } {
  const env = { ...process.env };
  if (opts.socketPath) {
    env['BEACHCOMBER_SOCKET'] = opts.socketPath;
  }
  const result = spawnSync(combBin, args, {
    env,
    timeout: opts.timeoutMs,
    encoding: 'utf8',
  });
  if (result.error) {
    throw new UnsupportedTransportError(`failed to spawn '${combBin}': ${result.error.message}`);
  }
  return { code: result.status, stdout: result.stdout ?? '', stderr: result.stderr ?? '' };
}

/** Best-effort error kind classification from a CLI failure's stderr text. */
function envelopeFromFailure(stderr: string): Envelope {
  const text = stderr.trim() || 'comb exited with a non-zero status';
  const kind = /connection refused|no such file or directory|daemon is not running/i.test(text)
    ? 'daemon_not_running'
    : 'server_error';
  return { ok: false, error: { kind, message: text } };
}

/** `--path P`, or `--global` when the call is not scoped to a directory. */
function pathArgs(path_: string | undefined): string[] {
  return path_ === undefined ? ['--global'] : ['--path', path_];
}

/** The fields of a `comb status -f json` row this transport reads. */
interface StatusRow {
  provider: string;
  path: string | null;
  field: string;
  value: unknown;
  age_ms: number;
  stale: boolean;
}

/** `p` canonicalized as the daemon does it, or `p` itself when that fails. */
function canonicalPath(p: string): string {
  try {
    return realpathSync.native(p);
  } catch {
    return p;
  }
}

/**
 * Walk `segments` (the dotted address below a top-level field) into
 * `value`, the daemon's own drilling: object keys, and array elements by
 * decimal-string index. `undefined` when the address is not there.
 */
function drill(value: unknown, segments: string[]): unknown {
  for (const seg of segments) {
    if (typeof value !== 'object' || value === null) return undefined;
    value = (value as Record<string, unknown>)[seg];
  }
  return value;
}

/**
 * The raw node at `key`, shaped as `bc_get`'s envelope
 * (`{"ok":true,"data":{"data":...,"age_ms":...,"stale":...}}`, the three
 * null on a miss).
 *
 * `comb get <key> [--path P | --global] [--force]` primes the key — a cold
 * Source is executed, `--force` re-executes — but its output is the
 * *resolved* value and is never read. The value comes from the `comb status
 * -f json` rows for the key's provider at the same path (a global entry's
 * row has a null `path`): a bare provider is every field merged, an
 * address below one is that field's row drilled to the rest, with the row's
 * own `age_ms`/`stale` — a whole namespace reports the oldest age and is
 * stale if any field is, as the daemon does. Nothing there is a miss,
 * unless the priming call was a daemon rejection (`Error querying <key>:
 * <the daemon's words>`), which is reported as the error instead.
 */
function cliGet(
  handle: SubprocessClientHandle,
  key: string,
  path_: string | undefined,
  force: boolean,
): Envelope {
  const args = ['get', key, ...pathArgs(path_)];
  if (force) args.push('--force');
  const prime = runComb(handle.combBin, args, handle);

  const status = cliStatus(handle);
  if (!status.ok) return status;
  const [provider, ...segments] = key.split('.');
  // The daemon addresses an entry by the canonical form of the path it was
  // given (`query::normalise_path`), so a row's `path` is canonical too —
  // `/private/tmp` for a `--path /tmp` on macOS.
  const rowPath = path_ === undefined ? null : canonicalPath(path_);
  const rows = (status.data as StatusRow[]).filter((row) => row.provider === provider && row.path === rowPath);

  if (segments.length === 0) {
    if (rows.length > 0) {
      const merged: Record<string, unknown> = {};
      let ageMs = 0;
      let stale = false;
      for (const row of rows) {
        merged[row.field] = row.value;
        ageMs = Math.max(ageMs, row.age_ms);
        stale ||= row.stale;
      }
      return { ok: true, data: { data: merged, age_ms: ageMs, stale } };
    }
  } else {
    const [field, ...below] = segments;
    const row = rows.find((r) => r.field === field);
    if (row) {
      const value = drill(row.value, below);
      if (value !== undefined) {
        return { ok: true, data: { data: value, age_ms: row.age_ms, stale: row.stale } };
      }
    }
  }

  if (prime.code !== 0 && prime.stderr.trim() !== '') return envelopeFromFailure(prime.stderr);
  return { ok: true, data: { data: null, age_ms: null, stale: null } };
}

function cliPut(
  handle: SubprocessClientHandle,
  key: string,
  jsonData: string | null,
  ttl: string | undefined,
  path_: string | undefined,
  nullify: boolean,
): Envelope {
  const args = ['put', key];
  if (nullify) {
    args.push('--null');
  } else {
    args.push(jsonData as string);
  }
  if (ttl !== undefined) args.push('--ttl', ttl);
  args.push(...pathArgs(path_));
  const { code, stderr } = runComb(handle.combBin, args, handle);
  if (code !== 0) return envelopeFromFailure(stderr);
  return { ok: true, data: null };
}

function cliStatus(handle: SubprocessClientHandle): Envelope {
  const { code, stdout, stderr } = runComb(handle.combBin, ['status', '-f', 'json'], handle);
  if (code !== 0) return envelopeFromFailure(stderr);
  const rows = stdout
    .split('\n')
    .map((l) => l.trim())
    .filter((l) => l.length > 0)
    .map((l) => JSON.parse(l));
  return { ok: true, data: rows };
}

export function createSubprocessTransport(): Transport {
  const combBin = resolveCombBin();

  const transport: Transport = {
    kind: 'subprocess',
    // No native library is loaded in this transport; report the resolved
    // `comb` binary path as the build identity in errors instead.
    libraryVersion: `subprocess:${combBin}`,

    newClient(options: NewClientOptions): SubprocessClientHandle {
      return {
        combBin,
        socketPath: options.socketPath,
        timeoutMs: options.timeoutMs ?? 5000,
      };
    },
    freeClient() {
      // No resources held.
    },

    async get(handle, key, path_, flags) {
      const h = handle as SubprocessClientHandle;
      const force = (flags & 0x1) !== 0;
      return cliGet(h, key, path_, force);
    },
    async put(handle, key, jsonData, ttl, path_) {
      return cliPut(handle as SubprocessClientHandle, key, jsonData, ttl, path_, false);
    },
    async putNull(handle, key, path_) {
      return cliPut(handle as SubprocessClientHandle, key, null, undefined, path_, true);
    },
    async refresh(_handle, key) {
      // `get --force` is not a faithful stand-in: `refresh` on a key naming
      // no Source is a documented error ("nothing to refresh: <key> has no
      // sources"), while `comb get --force` on the same key errors
      // differently ("cannot --force static namespace '<name>': no source
      // to re-execute from"). There is no other `comb` CLI equivalent for a
      // bare refresh.
      unsupported(`refresh(${key})`);
    },
    async status(handle) {
      return cliStatus(handle as SubprocessClientHandle);
    },
    async introspect(_handle, subject) {
      unsupported(`introspect{${subject}}`);
    },
    async hello() {
      unsupported('hello');
    },
    async resolve() {
      unsupported('resolve');
    },
    async evaluate() {
      unsupported('eval');
    },

    openSession(handle): SubprocessSessionHandle {
      return { client: handle as SubprocessClientHandle, contextPath: undefined };
    },
    closeSession() {
      // No resources held.
    },
    async sessionGet(session, key, path_, flags) {
      const s = session as SubprocessSessionHandle;
      const force = (flags & 0x1) !== 0;
      return cliGet(s.client, key, path_ ?? s.contextPath, force);
    },
    async sessionPut(session, key, jsonData, ttl, path_) {
      const s = session as SubprocessSessionHandle;
      // No contextPath fallback: the daemon's put op never consults
      // connection context (only get/refresh do), and the FFI tier passes
      // the caller's path through untouched. An omitted path means global.
      return cliPut(s.client, key, jsonData, ttl, path_, false);
    },
    async sessionSetContext(session, path_) {
      (session as SubprocessSessionHandle).contextPath = path_;
      return { ok: true, data: null };
    },

    openWatch(handle, key, path_): SubprocessWatchHandle {
      const h = handle as SubprocessClientHandle;
      const args = ['watch', key, '-f', 'json'];
      if (path_ !== undefined) args.push('--path', path_);
      const env = { ...process.env };
      if (h.socketPath) env['BEACHCOMBER_SOCKET'] = h.socketPath;
      const child = spawn(h.combBin, args, { env, stdio: ['ignore', 'pipe', 'pipe'] });
      const w: SubprocessWatchHandle = { child, buffer: '', lines: [], closed: false, cancelled: false };
      child.stdout.on('data', (chunk: Buffer) => {
        w.buffer += chunk.toString('utf8');
        let idx: number;
        while ((idx = w.buffer.indexOf('\n')) !== -1) {
          const line = w.buffer.slice(0, idx).trim();
          w.buffer = w.buffer.slice(idx + 1);
          if (line) w.lines.push(line);
        }
      });
      child.on('close', () => {
        w.closed = true;
      });
      return w;
    },
    async watchNext(watch, timeoutMs): Promise<WatchNextResult> {
      const w = watch as SubprocessWatchHandle;
      const deadline = timeoutMs > 0 ? Date.now() + timeoutMs : null;
      const pollTick = 20;
      for (;;) {
        if (w.cancelled) return { outcome: 'cancelled' };
        if (w.lines.length > 0) {
          const line = w.lines.shift() as string;
          const parsed = JSON.parse(line) as {
            ok: boolean;
            data?: unknown;
            age_ms?: number | null;
            stale?: boolean | null;
            error?: string;
          };
          if (!parsed.ok) {
            throw new ServerError(parsed.error ?? 'watch error');
          }
          return {
            outcome: 'event',
            data: parsed.data ?? null,
            ageMs: parsed.age_ms ?? null,
            stale: parsed.stale ?? null,
          };
        }
        if (w.closed) return { outcome: 'eof' };
        if (timeoutMs === 0) return { outcome: 'timeout' };
        if (deadline !== null && Date.now() >= deadline) return { outcome: 'timeout' };
        await new Promise((r) => setTimeout(r, pollTick));
      }
    },
    watchCancel(watch) {
      const w = watch as SubprocessWatchHandle;
      w.cancelled = true;
    },
    freeWatch(watch) {
      const w = watch as SubprocessWatchHandle;
      if (!w.closed) {
        try {
          w.child.kill('SIGTERM');
        } catch {
          // best-effort
        }
      }
    },
  };

  return transport;
}
