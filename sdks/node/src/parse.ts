/**
 * The typed shapes of docs/sdk-api.md, read from the JSON the ABI hands on:
 * `libbeachcomber::protocol`'s fields, in camelCase, and no others.
 */

import { ParseError } from './errors.js';
import type {
  CacheRow,
  DaemonHealth,
  HelloInfo,
  IntrospectResponse,
  IntrospectSubject,
  ReaperStatus,
  RowKind,
  Verdict,
  WatchEvent,
} from './types.js';

type Wire = Record<string, unknown>;

const optional = <T>(value: unknown, as: (v: unknown) => T): T | undefined =>
  value === undefined || value === null ? undefined : as(value);

export function parseHello(data: unknown): HelloInfo {
  const d = (data ?? {}) as Wire;
  return {
    protocolVersion: String(d['protocol_version'] ?? ''),
    daemonVersion: String(d['daemon_version'] ?? ''),
  };
}

/** `bc_status`'s rows. Anything but an array is the ABI broken: a `ParseError`. */
export function parseCacheRows(data: unknown, libraryVersion?: string): CacheRow[] {
  if (!Array.isArray(data)) {
    throw new ParseError('status data is not an array', libraryVersion);
  }
  return data.map((row: unknown) => {
    const r = row as Wire;
    return {
      provider: String(r['provider'] ?? ''),
      path: optional(r['path'], String) ?? null,
      source: String(r['source'] ?? ''),
      field: String(r['field'] ?? ''),
      value: r['value'],
      ageMs: optional(r['age_ms'], Number),
      stale: Boolean(r['stale']),
      kind: optional(r['kind'], (v) => v as RowKind),
      pollIntervalSecs: optional(r['poll_interval_secs'], Number),
      pollCount: optional(r['poll_count'], Number),
      pollsElapsed: optional(r['polls_elapsed'], Number),
      nextPollInSecs: optional(r['next_poll_in_secs'], Number),
      failure: optional(r['failure'], (v) => v as CacheRow['failure']),
    };
  });
}

function parseReaper(r: Wire): ReaperStatus {
  return {
    armed: Boolean(r['armed']),
    visibility: String(r['visibility'] ?? ''),
    sweeps: Number(r['sweeps'] ?? 0),
    reaped: Number(r['reaped'] ?? 0),
    killDenied: Number(r['kill_denied'] ?? 0),
  };
}

function parseVerdict(v: Wire): Verdict {
  return { level: String(v['level'] ?? ''), message: String(v['message'] ?? '') };
}

export function parseDaemonHealth(data: unknown): DaemonHealth {
  const d = (data ?? {}) as Wire;
  return {
    pid: Number(d['pid'] ?? 0),
    version: String(d['version'] ?? ''),
    uptimeSecs: Number(d['uptime_secs'] ?? 0),
    socketPath: String(d['socket_path'] ?? ''),
    configPath: optional(d['config_path'], String) ?? null,
    requestsTotal: Number(d['requests_total'] ?? 0),
    inFlight: Number(d['in_flight'] ?? 0),
    activeWatchers: Number(d['active_watchers'] ?? 0),
    cacheEntries: Number(d['cache_entries'] ?? 0),
    watchBackend: String(d['watch_backend'] ?? ''),
    reaper: optional(d['reaper'], (v) => parseReaper(v as Wire)) ?? null,
    verdicts: Array.isArray(d['verdicts']) ? (d['verdicts'] as Wire[]).map(parseVerdict) : [],
  };
}

/** The `daemon` subject is typed; every other subject is its JSON as the ABI hands it on. */
export function parseIntrospect(subject: IntrospectSubject, data: unknown): IntrospectResponse {
  if (subject === 'daemon') {
    return { subject, daemon: parseDaemonHealth(data), other: null };
  }
  return { subject, daemon: null, other: data ?? null };
}

/** A watch event: the value `get` answers at that moment, `null` a miss. */
export function parseWatchEvent(data: unknown): WatchEvent {
  return { data: data ?? null };
}
