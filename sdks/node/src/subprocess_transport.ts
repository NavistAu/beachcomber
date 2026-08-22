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
 * is its own process). `hello`, `introspect`, `refresh`, and the
 * client-side `resolve`/`eval` resolution ops have **no** faithful CLI
 * equivalent:
 *
 *   - `comb` exposes no JSON-emitting subcommand for `hello`/`introspect`.
 *   - `get --force` is not a stand-in for `refresh`: `bc_refresh` on a
 *     virtual provider is a documented no-op success, while `comb get
 *     --force` on the same key errors ("no source to re-execute from").
 *   - `resolve`'s path-expression case pins `cwd` to the CLI process's
 *     actual working directory rather than the caller-supplied one, which
 *     fixtures often set to a nonexistent path.
 *
 * Rather than fake structured data from scraped human-readable text or
 * silently return the wrong answer, those ops throw
 * `UnsupportedTransportError` naming the FFI transport as the
 * full-fidelity path.
 *
 * One more asymmetry worth knowing: `comb get`'s CLI defaults an omitted
 * `--path` to the *CLI process's own working directory* ("When no path is
 * given, the CLI's current working directory is used automatically"),
 * whereas `comb put` without `--path` means "global" (no path at all).
 * Round-tripping a pathless (global) provider through `get`/`put` without
 * an explicit path can therefore miss under this transport even though it
 * hits under FFI, where `path: NULL` means the same thing (absent) on both
 * ends. Pass an explicit `path` for global providers when running under
 * the subprocess transport.
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

/**
 * Run `comb get <key> [--path P] [--force] -f json` and parse its JSON
 * envelope, which matches the ABI's `bc_get` shape exactly
 * (`{"ok":true,"data":...,"age_ms":...,"stale":...}` with fields omitted
 * when absent, or a plain-text error on stderr with a non-zero exit).
 */
function cliGet(
  handle: SubprocessClientHandle,
  key: string,
  path_: string | undefined,
  force: boolean,
): Envelope {
  const args = ['get', key, '-f', 'json'];
  if (path_ !== undefined) args.push('--path', path_);
  if (force) args.push('--force');
  const { code, stdout, stderr } = runComb(handle.combBin, args, handle);
  if (code !== 0) return envelopeFromFailure(stderr);
  const trimmed = stdout.trim();
  if (trimmed === '') return { ok: true, data: null };
  return JSON.parse(trimmed) as Envelope;
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
  if (path_ !== undefined) args.push('--path', path_);
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
      // `get --force` is not a faithful stand-in: bc_refresh is documented
      // as a no-op success on a virtual provider (nothing to re-execute),
      // while `comb get --force` on the same key actively errors
      // ("cannot --force virtual provider: no source to re-execute from").
      // There is no other `comb` CLI equivalent for a bare refresh.
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
