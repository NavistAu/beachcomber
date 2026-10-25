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
 * has direct equivalents for get/put/refresh/status, and session
 * semantics are emulated client-side (remembering a context path between
 * calls — there is no real persistent connection to share, since each op is
 * its own process). `hello` and `introspect` have **no** faithful CLI
 * equivalent: `comb` exposes no JSON-emitting subcommand for either.
 *
 * `resolve`/`eval` are not implemented over this tier. Both take an
 * explicit `env` map the caller controls, but a spawned `comb get`/`comb
 * eval` process reads its own environment — and a spawn cannot be given
 * only the caller's `env` without also losing the `HOME`/`XDG_*` that
 * decide where the daemon's config and the global entry are. So the env a
 * caller hands in could not be honoured faithfully here.
 *
 * `watch` went the same way in Plan 4 Task 3, when it became a *resolved*
 * watch: every event is what `resolve` answers, re-resolved with the
 * caller's own `cwd` and `env`. `comb watch <key> -f json` renders it
 * through the same library, but it reads its environment from its own
 * process, which is exactly why `resolve` is unsupported too. The Lua
 * SDK's subprocess backend refuses `watch` for its own, unrelated reason
 * (`comb`'s stdout is block- rather than line-buffered through
 * `io.popen`, so a blocking line read never returns), and the two tiers
 * now agree regardless.
 *
 * Rather than fake structured data from scraped human-readable text or
 * silently return the wrong answer, all of these ops throw
 * `UnsupportedTransportError` naming the FFI transport as the
 * full-fidelity path.
 *
 * `refresh` is one spawn of `comb refresh <key> [--path P | --global]`: the
 * daemon's `refresh` op itself, which re-executes only the Sources the key
 * demands and refuses a key with none behind it. `comb refresh` prints the
 * daemon's refusal alone on stderr, so that line is the error message here
 * — the same text a `server_error` envelope carries over the FFI tier. The
 * same code drives both subprocess tiers
 * (`sdks/lua/beachcomber/subprocess_backend.lua` is the Lua half).
 *
 * `get` is one spawn: `comb get -f json <key> <p>:cache.<rest>
 * [--path P | --global] [--force] [--wait]` (`<p>:cache` for a bare
 * provider). Multi-key `-f json` prints a JSON array in key order, `null` in
 * a missed key's slot, so the two slots are the value and its `:cache`
 * answer (`age_ms`, `stale`; an object node is a tree of such leaves, read
 * as the largest age and any stale) — the resolver's own answer, with
 * everything the daemon does for a `get`: the directory chain up to `$HOME`,
 * `$HOME` itself as the global entry, `--force`/`--wait` and the daemon's
 * refusals. A key carrying a facet (a `:`) is asked for alone and answers
 * with age 0 and not stale. The same code drives both subprocess tiers
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
 * transport. A rendered value has no cached freshness of its own: its
 * `:cache` slot is a leaf with age 0 and `source` `template`, so it reports
 * age 0 and not stale.
 *
 * `comb get` and `comb put` both default an omitted `--path` to the CLI
 * process's own working directory, while an absent `path` here means the
 * global entry — the same thing `path: NULL` means under FFI, and the same
 * thing a wire request with no `path` field means. So a pathless call asks
 * for the global entry by name, with `--global`.
 */

import { spawnSync } from 'child_process';
import { UnsupportedTransportError } from './errors.js';
import type { Envelope } from './envelope.js';
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

function unsupported(op: string): never {
  throw new UnsupportedTransportError(
    `${op} is not supported over the subprocess transport (no koffi available). ` +
      `Install the optional 'koffi' peer dependency for the full-fidelity FFI transport.`,
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

/**
 * The `:cache` key that answers for `key`'s freshness: `git.branch` →
 * `git:cache.branch`, `mise['a.b']` → `mise:cache['a.b']`, `git` →
 * `git:cache`. The head runs to the first `.` or `[`. A facet key (a `:` in
 * the head) has no such companion and is asked alone; a `:` below the head
 * is part of a segment.
 */
function cacheKeyOf(key: string): string | undefined {
  const end = key.search(/[.[]/);
  const head = end < 0 ? key : key.slice(0, end);
  if (head.includes(':')) return undefined;
  return `${head}:cache${key.slice(head.length)}`;
}

/**
 * Age and staleness from a `:cache` node: a leaf answer is read as it is; an
 * object node is a tree of them, so the largest `age_ms` and any `stale`
 * across its leaves — what the response envelope reports. A miss is `0`/`false`.
 */
function freshness(node: unknown): { ageMs: number; stale: boolean } {
  if (typeof node !== 'object' || node === null) return { ageMs: 0, stale: false };
  const obj = node as Record<string, unknown>;
  if (typeof obj['age_ms'] === 'number') return { ageMs: obj['age_ms'], stale: obj['stale'] === true };
  let ageMs = 0;
  let stale = false;
  for (const child of Object.values(obj)) {
    const f = freshness(child);
    ageMs = Math.max(ageMs, f.ageMs);
    stale = stale || f.stale;
  }
  return { ageMs, stale };
}

/**
 * The node at `key`, shaped as `bc_get`'s envelope
 * (`{"ok":true,"data":{"data":...,"age_ms":...,"stale":...}}`, the three
 * null on a miss), from one `comb get -f json` for the value and its `:cache`
 * answer (the file header). Exit 0 is an answer: a null value slot is a miss,
 * anything else a hit, aged by its `:cache` slot (a rendered value has none,
 * so age 0). Exit 2 is an error, reported as `error: <who>: <why>`: an
 * `error: <key>: ` line is the daemon's refusal of `key` or the resolver's
 * own error (a template that does not compile, a cycle, a failed `exec()`);
 * with a null value slot, an `error: <key>.<leaf>` line is a leaf of a group
 * failing. Exit 2 with the value slot filled is a `:cache` slot that failed
 * on its own — still a hit, with age 0.
 */
function cliGet(
  handle: SubprocessClientHandle,
  key: string,
  path_: string | undefined,
  force: boolean,
  wait: boolean,
): Envelope {
  const cacheKey = cacheKeyOf(key);
  const meta = cacheKey === undefined;
  const args = ['get', '-f', 'json', key];
  if (cacheKey !== undefined) args.push(cacheKey);
  args.push(...pathArgs(path_));
  if (force) args.push('--force');
  if (wait) args.push('--wait');
  const { code, stdout, stderr } = runComb(handle.combBin, args, handle);

  const trimmed = stdout.trim();
  const decoded: unknown = trimmed === '' ? undefined : JSON.parse(trimmed);
  const value = meta ? decoded : (decoded as unknown[] | undefined)?.[0];
  if (code !== 0) {
    const lines = stderr.split('\n');
    // The CLI prints one `error: <k>: <msg>` line per key that failed; the
    // `<cacheKey>` line starts `error: <cacheKey>: `, which never matches
    // `<key>`'s prefix.
    const prefix = `error: ${key}: `;
    const own = lines.find((line) => line.startsWith(prefix))?.slice(prefix.length);
    if (own !== undefined) {
      // The daemon refused `key`, or the resolver failed on it: a server error
      // whatever the words say — never a transport kind, which
      // `envelopeFromFailure` would guess from them.
      return { ok: false, error: { kind: 'server_error', message: own } };
    }
    if (value === null) {
      // A leaf of a group failed: an `error: <key>.<leaf>: <msg>` line. The
      // daemon answered, so this is a server error whatever the words say.
      const companion = cacheKey === undefined ? undefined : `error: ${cacheKey}`;
      const leaf = lines.find(
        (line) => line.startsWith(`error: ${key}.`) || line.startsWith(`error: ${key}[`),
      );
      const ownError =
        leaf?.slice('error: '.length) ??
        (lines
          .filter((line) => companion === undefined || !line.startsWith(companion))
          .join('\n') || stderr);
      return { ok: false, error: { kind: 'server_error', message: ownError.trim() } };
    }
    if (decoded === undefined) {
      // The CLI failed before answering at all (no daemon, a bad argument).
      return envelopeFromFailure(stderr);
    }
    // Only the `:cache` slot failed: `key` itself answered.
  }
  if (value === null || value === undefined) {
    return { ok: true, data: { data: null, age_ms: null, stale: null } };
  }
  if (meta) return { ok: true, data: { data: value, age_ms: 0, stale: false } };
  const { ageMs, stale } = freshness((decoded as unknown[])[1]);
  return { ok: true, data: { data: value, age_ms: ageMs, stale } };
}

function cliPut(
  handle: SubprocessClientHandle,
  key: string,
  jsonData: string,
  ttl: string | undefined,
  path_: string | undefined,
): Envelope {
  const args = ['put', key, jsonData];
  if (ttl !== undefined) args.push('--ttl', ttl);
  args.push(...pathArgs(path_));
  const { code, stderr } = runComb(handle.combBin, args, handle);
  if (code !== 0) return envelopeFromFailure(stderr);
  return { ok: true, data: null };
}

/**
 * One `comb refresh <key> [--path P | --global]`: exit 0 is the daemon's ok;
 * exit 2 is its refusal, printed alone on stderr as `error: <key>: <words>`
 * — `nothing to refresh: <key> has no sources` for a key with no Source
 * behind it — or the CLI's own `error: daemon: …` when it could not reach the
 * daemon at all.
 */
function cliRefresh(handle: SubprocessClientHandle, key: string, path_: string | undefined): Envelope {
  const args = ['refresh', key, ...pathArgs(path_)];
  const { code, stderr } = runComb(handle.combBin, args, handle);
  if (code !== 0) {
    const trimmed = stderr.trim();
    // The daemon's own refusal is the CLI's only stderr line. Anything else
    // — nothing printed, or `error: daemon: …` — means the CLI never reached
    // the daemon at all.
    const refusal = `error: ${key}: `;
    if (!trimmed.startsWith(refusal)) {
      return envelopeFromFailure(stderr);
    }
    return { ok: false, error: { kind: 'server_error', message: trimmed.slice(refusal.length) } };
  }
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
      return cliPut(handle as SubprocessClientHandle, key, jsonData, ttl, path_);
    },
    async refresh(handle, key, path_) {
      return cliRefresh(handle as SubprocessClientHandle, key, path_);
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
      return cliPut(s.client, key, jsonData, ttl, path_);
    },
    async sessionSetContext(session, path_) {
      (session as SubprocessSessionHandle).contextPath = path_;
      return { ok: true, data: null };
    },

    openWatch() {
      unsupported('watch');
    },
    async watchNext() {
      unsupported('watch');
    },
    watchCancel() {
      unsupported('watch');
    },
    freeWatch() {
      unsupported('watch');
    },
  };

  return transport;
}
