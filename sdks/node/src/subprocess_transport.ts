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
 * `get` is one spawn: `comb get -f json <key> <key>:age <key>:stale
 * [--path P | --global] [--force] [--wait]`. Multi-key `-f json` prints a
 * JSON array in key order, `null` in a missed key's slot, so the three
 * slots are the value, its age and its staleness — the resolver's own
 * answer, with everything the daemon does for a `get`: the directory chain
 * up to `$HOME`, `$HOME` itself as the global entry, metadata suffixes,
 * `--force`/`--wait` and the daemon's refusals. A key that already carries
 * a metadata suffix (`:age`, `:stale`, `:fresh`, `:cache`, `:source`) is
 * asked for alone and answers with age 0 and not stale, as the daemon
 * answers one over the wire. The same code drives both subprocess tiers
 * (`sdks/lua/beachcomber/subprocess_backend.lua` is the Lua half).
 *
 * The one divergence from the FFI tier: `comb get` *resolves*, so a
 * template-backed key returns its **rendered** value here where `bc_get`
 * returns the raw template half. A result from this tier never carries
 * `template` or `rest`, and a template-only node is a hit holding the
 * rendered value rather than the "neither hit nor miss" the FFI tier
 * reports. The CLI renders with `exec()` on, so the client's `exec` option
 * has no effect on this tier; a caller that must not spawn processes from
 * a template, or that needs the raw template half, needs the FFI
 * transport. A rendered value has no cached freshness of its own (its
 * `:age`/`:stale` are misses), so it reports age 0 and not stale, as
 * libbeachcomber does for a hit whose freshness is absent.
 *
 * `comb get` and `comb put` both default an omitted `--path` to the CLI
 * process's own working directory, while an absent `path` here means the
 * global entry — the same thing `path: NULL` means under FFI, and the same
 * thing a wire request with no `path` field means. So a pathless call asks
 * for the global entry by name, with `--global`.
 */

import { spawn, spawnSync } from 'child_process';
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

const META_SUFFIXES = new Set(['age', 'stale', 'fresh', 'cache', 'source']);

/** True when `key` ends in one of the daemon's metadata suffixes. */
function hasMetaSuffix(key: string): boolean {
  const m = /:([\w-]+)$/.exec(key);
  return m !== null && META_SUFFIXES.has(m[1] as string);
}

/**
 * The node at `key`, shaped as `bc_get`'s envelope
 * (`{"ok":true,"data":{"data":...,"age_ms":...,"stale":...}}`, the three
 * null on a miss), from one `comb get -f json` for the value, its age and
 * its staleness (the file header). Exit 0 is a hit. Exit 2 with the CLI's
 * miss line for `key` (`Error querying <key>: no data (cache miss)`) is a
 * miss; with another line for `key` it is that refusal, in the daemon's
 * words. A rendered value's `:age`/`:stale` companions miss on their own —
 * the value slot is filled and only they fail — and that is a hit with age 0.
 */
function cliGet(
  handle: SubprocessClientHandle,
  key: string,
  path_: string | undefined,
  force: boolean,
  wait: boolean,
): Envelope {
  const meta = hasMetaSuffix(key);
  const args = ['get', '-f', 'json', key];
  if (!meta) args.push(`${key}:age`, `${key}:stale`);
  args.push(...pathArgs(path_));
  if (force) args.push('--force');
  if (wait) args.push('--wait');
  const { code, stdout, stderr } = runComb(handle.combBin, args, handle);

  const trimmed = stdout.trim();
  const decoded: unknown = trimmed === '' ? undefined : JSON.parse(trimmed);
  if (code !== 0) {
    // The CLI prints one `Error querying <k>: <msg>` line per failed key; the
    // `<key>:age` line starts `Error querying <key>:age: `, so the trailing
    // `: ` keeps it from matching `<key>`.
    const prefix = `Error querying ${key}: `;
    const own = stderr.split('\n').find((line) => line.startsWith(prefix));
    if (own === `${prefix}no data (cache miss)`) {
      return { ok: true, data: { data: null, age_ms: null, stale: null } };
    }
    // The daemon refused `key`, or the CLI failed before answering at all
    // (no daemon, a bad argument) — either way the message is the CLI's.
    if (own !== undefined || decoded === undefined) return envelopeFromFailure(own ?? stderr);
    // Only the `:age`/`:stale` companions missed: `key` itself rendered.
  }
  if (meta) return { ok: true, data: { data: decoded, age_ms: 0, stale: false } };
  const [data, ageMs, stale] = decoded as [unknown, number | null, boolean | null];
  return { ok: true, data: { data, age_ms: ageMs ?? 0, stale: stale ?? false } };
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
      return cliGet(h, key, path_, (flags & 0x1) !== 0, (flags & 0x2) !== 0);
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
      return cliGet(s.client, key, path_ ?? s.contextPath, (flags & 0x1) !== 0, (flags & 0x2) !== 0);
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
