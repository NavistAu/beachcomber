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
 * The spawned `comb` is made to render as the FFI tier does:
 *
 * - **Environment.** A call's `env` map is the spawned `comb`'s whole
 *   environment, so `env.*` and an `exec()` child see exactly the map. With
 *   no map, the spawn inherits this process's environment, which is what the
 *   FFI tier renders with too.
 * - **Socket.** `--socket <p>` whenever the client has a `socketPath` or this
 *   process has `$BEACHCOMBER_SOCKET`: the order `bc_socket_path` picks in.
 *   The map can therefore never point the tier at another daemon.
 * - **Colour.** `--color never` when the render's environment sets a
 *   non-empty `NO_COLOR`, else `--color always` (the template filters would
 *   otherwise draw nothing into a pipe): the rule `EngineCtx::colour` has.
 * - **`exec()`.** `--no-exec` unless the client was made with `exec: true`.
 *
 * `eval` is not implemented over this tier yet.
 *
 * `watch` went the same way in Plan 4 Task 3, when it became a *resolved*
 * watch: every event is what `get` answers, re-read with the caller's own
 * `cwd` and `env`. The Lua SDK's subprocess backend refuses `watch` because
 * `io.popen` has no timed read and no cancel, which `next_event(timeout_ms)`
 * and `cancel` need.
 *
 * Until it is built, these ops throw `UnsupportedError` naming the FFI
 * transport as the full-fidelity path.
 *
 * `refresh` is one spawn of `comb refresh <key> [--path P | --global]`: the
 * daemon's `refresh` op itself, which re-executes only the Sources the key
 * demands and refuses a key with none behind it. `comb refresh` prints the
 * daemon's refusal alone on stderr, so that line is the error message here
 * — the same text a `server_error` envelope carries over the FFI tier. The
 * same code drives both subprocess tiers.
 *
 * Timeouts: this tier has no default of its own. `timeoutMs`, when given,
 * bounds each spawned `comb` (it is killed past it), the nearest a process
 * spawn has to the FFI tier's per-read timeout; unset, a spawn runs until
 * `comb` returns. `connectTimeoutMs` has no `comb` flag to carry it, so a
 * client constructed with it is refused with `UnsupportedError`
 * naming the FFI transport, rather than silently ignored.
 *
 * `comb get` and `comb put` both default an omitted `--path` to the CLI
 * process's own working directory, while an absent `path` here means the
 * global entry — the same thing `path: NULL` means under FFI, and the same
 * thing a wire request with no `path` field means. So a pathless call asks
 * for the global entry by name, with `--global`.
 */

import { spawnSync } from 'child_process';
import * as fs from 'fs';
import * as path from 'path';
import { IoError, LibraryNotFoundError, TimeoutError, UnsupportedError } from './errors.js';
import type { Envelope } from './envelope.js';
import type { NewClientOptions, Transport } from './transport.js';

interface SubprocessClientHandle {
  combBin: string;
  socketPath: string | undefined;
  timeoutMs: number | undefined;
  exec: boolean;
}

interface SubprocessSessionHandle {
  client: SubprocessClientHandle;
  contextPath: string | undefined;
}

function unsupported(op: string): never {
  throw new UnsupportedError(
    `${op} is not supported over the subprocess transport (no koffi available). ` +
      `Install the optional 'koffi' peer dependency for the full-fidelity FFI transport.`,
  );
}

/**
 * The `comb` to run, as an absolute path where this process's `$PATH` finds
 * it. A spawn with a caller's map as its whole environment would otherwise
 * search the map's `$PATH`, not ours.
 */
function resolveCombBin(): string {
  const bin = process.env['COMB_BIN'] || 'comb';
  if (bin.includes('/')) return bin;
  for (const dir of (process.env['PATH'] ?? '').split(':')) {
    if (dir === '') continue;
    const candidate = path.join(dir, bin);
    try {
      fs.accessSync(candidate, fs.constants.X_OK);
      if (fs.statSync(candidate).isFile()) return candidate;
    } catch {
      // not here
    }
  }
  return bin;
}

/** The variables a call renders with: its map, else this process's. */
function renderVars(envJson: string | undefined): NodeJS.ProcessEnv {
  return envJson === undefined ? process.env : (JSON.parse(envJson) as Record<string, string>);
}

/** `--socket`: the client's option, else this process's `$BEACHCOMBER_SOCKET`. */
function socketArgs(handle: SubprocessClientHandle): string[] {
  const socket = handle.socketPath || process.env['BEACHCOMBER_SOCKET'];
  return socket ? ['--socket', socket] : [];
}

/** `--no-exec` unless the client was made with `exec: true`. */
function execArgs(handle: SubprocessClientHandle): string[] {
  return handle.exec ? [] : ['--no-exec'];
}

/** `--color never` when `NO_COLOR` is set non-empty among the render's variables, else `always`. */
function colourArgs(vars: NodeJS.ProcessEnv): string[] {
  return ['--color', vars['NO_COLOR'] ? 'never' : 'always'];
}

function runComb(
  handle: SubprocessClientHandle,
  args: string[],
  vars: NodeJS.ProcessEnv = process.env,
): { code: number | null; stdout: string; stderr: string } {
  const combBin = handle.combBin;
  const result = spawnSync(combBin, [...socketArgs(handle), ...args], {
    env: vars,
    timeout: handle.timeoutMs,
    encoding: 'utf8',
  });
  if (result.error) {
    // The spawn itself failed: no `comb` to run, a `timeoutMs` that ran out,
    // or the spawn's own I/O.
    const code = (result.error as NodeJS.ErrnoException).code;
    const message = `failed to spawn '${combBin}': ${result.error.message}`;
    const identity = `subprocess:${combBin}`;
    if (code === 'ENOENT') throw new LibraryNotFoundError(message, identity);
    if (code === 'ETIMEDOUT') throw new TimeoutError(message, identity);
    throw new IoError(message, identity);
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
  const vars = renderVars(envJson);
  const args = [...colourArgs(vars), ...execArgs(handle), 'get', '-f', 'json', key, ...pathArgs(path_)];
  if (force) args.push('--force');
  if (wait) args.push('--wait');
  const { code, stdout, stderr } = runComb(handle, args, vars);

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
  const { code, stderr } = runComb(handle, args);
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
  const { code, stderr } = runComb(handle, args);
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
  const { code, stdout, stderr } = runComb(handle, [...execArgs(handle), 'status', '-f', 'json']);
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
      if (options.connectTimeoutMs !== undefined) {
        throw new UnsupportedError(
          'connectTimeoutMs is not supported over the subprocess transport: comb has no flag ' +
            "to carry it. Install the optional 'koffi' peer dependency for the FFI transport, which honours it.",
        );
      }
      return {
        combBin,
        socketPath: options.socketPath,
        timeoutMs: options.timeoutMs,
        exec: options.exec === true,
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
