/**
 * The FFI transport: `koffi` bindings over `libbeachcomber.{so,dylib}`.
 *
 * Implements the seven-point common contract from the plan's Phase 4 header:
 * ordered library discovery, loud discovery failure, symbols checked at
 * load (not first use), `bc_version()` read on load and carried by every
 * error as `libraryVersion`, `ok:false` envelopes turned into idiomatic errors, and every
 * `char *` freed via `bc_string_free` (via koffi's disposable-type
 * mechanism) except `bc_version()`'s static string.
 */

import { createRequire } from 'module';
import { libraryCandidates } from './discovery.js';
import { errorFromEnvelope, LibraryNotFoundError, LibrarySymbolError } from './errors.js';
import type { Envelope, WatchNextResult, WatchOutcome } from './envelope.js';
import type { NewClientOptions, Transport } from './transport.js';

const require = createRequire(import.meta.url);

/**
 * The `options_json` `bc_client_new` takes for `options`: each option the
 * caller gave, under the ABI's spelling, and nothing else — no key for an
 * option left unset, so the library's own default applies. `null` (no
 * options at all) when none was given.
 */
export function clientOptionsJson(options: NewClientOptions): string | null {
  const given: Record<string, unknown> = {};
  const put = (key: string, value: unknown) => {
    if (value !== undefined) given[key] = value;
  };
  put('socket_path', options.socketPath);
  put('timeout_ms', options.timeoutMs);
  put('connect_timeout_ms', options.connectTimeoutMs);
  put('autostart', options.autostart);
  put('exec', options.exec);
  return Object.keys(given).length === 0 ? null : JSON.stringify(given);
}

/** True iff the optional `koffi` peer dependency resolves. Does not load it. */
export function koffiAvailable(): boolean {
  try {
    require.resolve('koffi');
    return true;
  } catch {
    return false;
  }
}

// The bc_* symbols this binding calls (see libbeachcomber-ffi/include/beachcomber.h).
// bc_version and bc_string_free are bound separately since they need special handling
// (bc_version's result must never be freed; bc_string_free is itself the free function).
const OP_SYMBOLS = [
  'bc_client_new',
  'bc_client_free',
  'bc_get',
  'bc_put',
  'bc_refresh',
  'bc_status',
  'bc_introspect',
  'bc_hello',
  'bc_eval',
  'bc_session_open',
  'bc_session_close',
  'bc_session_get',
  'bc_session_put',
  'bc_session_refresh',
  'bc_session_set_context',
  'bc_watch_open',
  'bc_watch_next',
  'bc_watch_cancel',
  'bc_watch_free',
] as const;

interface Bound {
  lib: unknown;
  version: string;
  fns: Record<string, (...args: unknown[]) => unknown>;
}

function loadLibrary(): { lib: import('koffi').LibraryHandle; version: string } {
  // eslint-disable-next-line @typescript-eslint/no-var-requires
  const koffi = require('koffi');
  const candidates = libraryCandidates();
  const tried: string[] = [];
  let lib: import('koffi').LibraryHandle | undefined;

  for (const candidate of candidates) {
    try {
      lib = koffi.load(candidate.path);
      break;
    } catch (e) {
      tried.push(`  - ${candidate.path} (${candidate.source}): ${e instanceof Error ? e.message : String(e)}`);
    }
  }

  if (!lib) {
    throw new LibraryNotFoundError(
      `could not locate libbeachcomber; tried, in order:\n${tried.join('\n')}`,
    );
  }

  let bcVersion: (...args: unknown[]) => unknown;
  try {
    bcVersion = lib.func('bc_version', 'str', []);
  } catch (e) {
    throw new LibrarySymbolError(
      `loaded library is missing required symbol 'bc_version': ${e instanceof Error ? e.message : String(e)}`,
    );
  }
  const version = String(bcVersion());
  return { lib, version };
}

function bindSymbols(lib: import('koffi').LibraryHandle, koffi: typeof import('koffi'), version: string): Bound['fns'] {
  const BcClient = koffi.pointer('BcClient', koffi.opaque());
  const BcSession = koffi.pointer('BcSession', koffi.opaque());
  const BcWatch = koffi.pointer('BcWatch', koffi.opaque());

  let bcStringFreeRaw: (...args: unknown[]) => unknown;
  try {
    bcStringFreeRaw = lib.func('bc_string_free', 'void', ['void *']);
  } catch (e) {
    throw new LibrarySymbolError(
      `loaded library is missing required symbol 'bc_string_free': ${e instanceof Error ? e.message : String(e)}`,
      version,
    );
  }
  const BcString = koffi.disposable('BcString', 'str', bcStringFreeRaw);

  type Spec = string | import('koffi').TypeObject;
  const fns: Record<string, (...args: unknown[]) => unknown> = {};
  const sig: Record<(typeof OP_SYMBOLS)[number], [Spec, Spec[]]> = {
    bc_client_new: [BcClient, ['str']],
    bc_client_free: ['void', [BcClient]],
    bc_get: [BcString, [BcClient, 'str', 'str', 'str', 'uint32']],
    bc_put: [BcString, [BcClient, 'str', 'str', 'str', 'str']],
    bc_refresh: [BcString, [BcClient, 'str', 'str']],
    bc_status: [BcString, [BcClient]],
    bc_introspect: [BcString, [BcClient, 'str', 'str']],
    bc_hello: [BcString, [BcClient]],
    bc_eval: [BcString, [BcClient, 'str', 'str', 'str']],
    bc_session_open: [BcSession, [BcClient]],
    bc_session_close: ['void', [BcSession]],
    bc_session_get: [BcString, [BcSession, 'str', 'str', 'str', 'uint32']],
    bc_session_put: [BcString, [BcSession, 'str', 'str', 'str', 'str']],
    bc_session_refresh: [BcString, [BcSession, 'str', 'str']],
    bc_session_set_context: [BcString, [BcSession, 'str']],
    bc_watch_open: [BcWatch, [BcClient, 'str', 'str', 'str']],
    bc_watch_next: [BcString, [BcWatch, 'int32']],
    bc_watch_cancel: ['void', [BcWatch]],
    bc_watch_free: ['void', [BcWatch]],
  };

  for (const name of OP_SYMBOLS) {
    const [ret, args] = sig[name];
    try {
      fns[name] = lib.func(name, ret, args) as (...a: unknown[]) => unknown;
    } catch (e) {
      throw new LibrarySymbolError(
        `loaded library is missing required symbol '${name}': ${e instanceof Error ? e.message : String(e)}`,
        version,
      );
    }
  }
  return fns;
}

/** Promisified call through koffi's async (worker-thread) calling convention. */
function callAsync(fn: unknown, ...args: unknown[]): Promise<unknown> {
  const f = fn as { async: (...a: unknown[]) => void };
  return new Promise((resolve, reject) => {
    f.async(...args, (err: Error | null, res: unknown) => {
      if (err) reject(err);
      else resolve(res);
    });
  });
}

function parseEnvelope(raw: unknown): Envelope {
  const text = String(raw);
  return JSON.parse(text) as Envelope;
}

function parseWatchNext(raw: unknown, version: string): WatchNextResult {
  const text = String(raw);
  const v = JSON.parse(text) as
    | { ok: true; outcome: WatchOutcome; data?: unknown }
    | { ok: false; error: { kind: string; message: string } };
  if (!v.ok) {
    throw errorFromEnvelope(v.error, version);
  }
  if (v.outcome === 'event') {
    // The payload is the resolved value itself (null for a miss).
    return { outcome: 'event', data: v.data ?? null };
  }
  return { outcome: v.outcome };
}

export function createFfiTransport(): Transport {
  // eslint-disable-next-line @typescript-eslint/no-var-requires
  const koffi = require('koffi') as typeof import('koffi');
  const { lib, version } = loadLibrary();
  const fns = bindSymbols(lib, koffi, version);

  const nullIfUndefined = (v: string | undefined): string | null => (v === undefined ? null : v);

  const transport: Transport = {
    kind: 'ffi',
    libraryVersion: version,

    newClient(options: NewClientOptions) {
      return fns.bc_client_new(clientOptionsJson(options));
    },
    freeClient(handle) {
      fns.bc_client_free(handle);
    },

    async get(handle, key, cwd, envJson, flags) {
      return parseEnvelope(
        await callAsync(fns.bc_get, handle, key, nullIfUndefined(cwd), nullIfUndefined(envJson), flags),
      );
    },
    async put(handle, key, jsonData, ttl, path) {
      return parseEnvelope(
        await callAsync(fns.bc_put, handle, key, jsonData, nullIfUndefined(ttl), nullIfUndefined(path)),
      );
    },
    async refresh(handle, key, path) {
      return parseEnvelope(await callAsync(fns.bc_refresh, handle, key, nullIfUndefined(path)));
    },
    async status(handle) {
      return parseEnvelope(await callAsync(fns.bc_status, handle));
    },
    async introspect(handle, subject, optionsJson) {
      return parseEnvelope(
        await callAsync(fns.bc_introspect, handle, subject, nullIfUndefined(optionsJson)),
      );
    },
    async hello(handle) {
      return parseEnvelope(await callAsync(fns.bc_hello, handle));
    },
    async evaluate(handle, templateStr, cwd, envJson) {
      return parseEnvelope(await callAsync(fns.bc_eval, handle, templateStr, nullIfUndefined(cwd), nullIfUndefined(envJson)));
    },

    openSession(handle) {
      return fns.bc_session_open(handle);
    },
    closeSession(session) {
      fns.bc_session_close(session);
    },
    async sessionGet(session, key, cwd, envJson, flags) {
      return parseEnvelope(
        await callAsync(fns.bc_session_get, session, key, nullIfUndefined(cwd), nullIfUndefined(envJson), flags),
      );
    },
    async sessionPut(session, key, jsonData, ttl, path) {
      return parseEnvelope(
        await callAsync(
          fns.bc_session_put,
          session,
          key,
          jsonData,
          nullIfUndefined(ttl),
          nullIfUndefined(path),
        ),
      );
    },
    async sessionRefresh(session, key, path) {
      return parseEnvelope(
        await callAsync(fns.bc_session_refresh, session, key, nullIfUndefined(path)),
      );
    },
    async sessionSetContext(session, path) {
      return parseEnvelope(await callAsync(fns.bc_session_set_context, session, path));
    },

    openWatch(handle, key, cwd, envJson) {
      return fns.bc_watch_open(handle, key, nullIfUndefined(cwd), nullIfUndefined(envJson));
    },
    async watchNext(watch, timeoutMs) {
      const raw = await callAsync(fns.bc_watch_next, watch, timeoutMs);
      return parseWatchNext(raw, version);
    },
    watchCancel(watch) {
      fns.bc_watch_cancel(watch);
    },
    freeWatch(watch) {
      fns.bc_watch_free(watch);
    },
  };

  return transport;
}
