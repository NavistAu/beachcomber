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
 * `get` is one spawn: `comb get -f json <key> [--path P | --global]
 * [--force] [--wait]` — the value `comb get` prints, which is the value
 * `bc_get` answers over the FFI tier: the same resolver, the directory
 * chain up to `$HOME`, `$HOME` itself as the global entry, `--force`/`--wait`
 * and the daemon's refusals. Exit 0 is an answer: the value on stdout, or
 * `null` for a miss. Exit 2 is an error, reported as `error: <who>: <why>`:
 * an `error: <key>: ` line is the daemon's refusal of `key` or the
 * resolver's own error (a template that does not compile, a cycle, a failed
 * `exec()`), and an `error: <key>.<leaf>: ` line is a leaf of a group
 * failing.
 * The same code drives both subprocess tiers
 * (`sdks/lua/beachcomber/subprocess_backend.lua` is the Lua half).
 *
 * Two things a `get` takes cannot cross the spawn. A spawned `comb` renders
 * with its own environment, so a caller-supplied `env` cannot be honoured:
 * a call that passes one throws `UnsupportedTransportError` naming the FFI
 * transport. With no `env`, the spawned `comb` inherits this process's
 * environment and renders with it, where the FFI tier renders with none.
 * And the CLI renders with `exec()` on, so the client's `exec` option has no
 * effect on this tier. A caller that needs either needs the FFI transport.
 *
 * `eval` is not implemented over this tier, for the `env` reason above:
 * `comb eval` reads its environment from its own process, and the `env` a
 * caller hands in could not be honoured faithfully here.
 *
 * `watch` went the same way in Plan 4 Task 3, when it became a *resolved*
 * watch: every event is what `get` answers, re-read with the caller's own
 * `cwd` and `env`. `comb watch <key> -f json` renders it through the same
 * library, but it reads its environment from its own process. The Lua
 * SDK's subprocess backend refuses `watch` for its own, unrelated reason
 * (`comb`'s stdout is block- rather than line-buffered through
 * `io.popen`, so a blocking line read never returns), and the two tiers
 * agree regardless.
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
 * same code drives both subprocess tiers.
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
 * The value at `key`, shaped as `bc_get`'s envelope (`{"ok":true,"data":<value>}`,
 * `null` on a miss), from one `comb get -f json` (the file header).
 */
function cliGet(
  handle: SubprocessClientHandle,
  key: string,
  path_: string | undefined,
  envJson: string | undefined,
  force: boolean,
  wait: boolean,
): Envelope {
  if (envJson !== undefined) unsupported('get with an env');
  const args = ['get', '-f', 'json', key, ...pathArgs(path_)];
  if (force) args.push('--force');
  if (wait) args.push('--wait');
  const { code, stdout, stderr } = runComb(handle.combBin, args, handle);

  if (code !== 0) {
    const lines = stderr.split('\n');
    const prefix = `error: ${key}: `;
    const own = lines.find((line) => line.startsWith(prefix))?.slice(prefix.length);
    if (own !== undefined) {
      // The daemon refused `key`, or the resolver failed on it: a server
      // error whatever the words say — never a transport kind, which
      // `envelopeFromFailure` would guess from them.
      return { ok: false, error: { kind: 'server_error', message: own } };
    }
    // A leaf of a group failed. The daemon answered, so this is a server
    // error whatever the words say — a failed `exec()` names a missing file,
    // which must not read as the daemon being unreachable.
    const leaf = lines.find(
      (line) => line.startsWith(`error: ${key}.`) || line.startsWith(`error: ${key}[`),
    );
    if (leaf !== undefined) {
      return { ok: false, error: { kind: 'server_error', message: leaf.slice('error: '.length).trim() } };
    }
    // The CLI failed before answering at all (no daemon, a bad argument).
    return envelopeFromFailure(stderr);
  }
  const trimmed = stdout.trim();
  return { ok: true, data: trimmed === '' ? null : JSON.parse(trimmed) };
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

    async get(handle, key, cwd, envJson, flags) {
      const h = handle as SubprocessClientHandle;
      return cliGet(h, key, cwd, envJson, (flags & 0x1) !== 0, (flags & 0x2) !== 0);
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
    async evaluate() {
      unsupported('eval');
    },

    openSession(handle): SubprocessSessionHandle {
      return { client: handle as SubprocessClientHandle, contextPath: undefined };
    },
    closeSession() {
      // No resources held.
    },
    async sessionGet(session, key, cwd, envJson, flags) {
      const s = session as SubprocessSessionHandle;
      return cliGet(s.client, key, cwd ?? s.contextPath, envJson, (flags & 0x1) !== 0, (flags & 0x2) !== 0);
    },
    async sessionPut(session, key, jsonData, ttl, path_) {
      const s = session as SubprocessSessionHandle;
      return cliPut(s.client, key, jsonData, ttl, path_ ?? s.contextPath);
    },
    async sessionRefresh(session, key, path_) {
      const s = session as SubprocessSessionHandle;
      return cliRefresh(s.client, key, path_ ?? s.contextPath);
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
