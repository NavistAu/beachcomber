/**
 * The typed shapes of docs/sdk-api.md: `libbeachcomber::protocol`'s fields,
 * in camelCase, and no others (sdks/api.json lists them under `types`).
 */

export interface HelloInfo {
  protocolVersion: string;
  daemonVersion: string;
}

export type RowKind =
  | { kind: 'lifecycle'; decay: number; depth: number; watches_files: boolean }
  | { kind: 'once' }
  | { kind: 'static' }
  | { kind: 'transient' };

export interface CacheRow {
  provider: string;
  /** The directory the row is stored at; `null` for the global entry. */
  path: string | null;
  /** What produced the field, as the daemon presents it (`source(head)`, `put`, `config(<file>)`). */
  source: string;
  field: string;
  value: unknown;
  /** How old the value is; undefined when the row has no age. */
  ageMs?: number;
  kind?: RowKind;
  pollIntervalSecs?: number;
  pollCount?: number;
  pollsElapsed?: number;
  /** Seconds until the next scheduled poll; undefined when it has none. */
  nextPollInSecs?: number;
  failure?: { consecutive_failures: number; suppressed_until_unix_ms?: number; last_error?: string };
}

export interface Verdict {
  level: string;
  message: string;
}

export interface ReaperStatus {
  armed: boolean;
  visibility: string;
  sweeps: number;
  reaped: number;
  killDenied: number;
}

export interface DaemonHealth {
  pid: number;
  version: string;
  uptimeSecs: number;
  socketPath: string;
  configPath: string | null;
  requestsTotal: number;
  inFlight: number;
  activeWatchers: number;
  cacheEntries: number;
  /** `native`, `polling`, `disabled` or `unknown`. */
  watchBackend: string;
  /** The reaper's capability; `null` on a daemon that has none. */
  reaper: ReaperStatus | null;
  verdicts: Verdict[];
}

export type IntrospectSubject =
  | 'daemon'
  | 'providers'
  | 'config'
  | 'cache'
  | 'lifecycle'
  | 'watches'
  | 'timers'
  | 'demand'
  | 'procs';

/**
 * An introspect report: `daemon` is typed for the `daemon` subject (and
 * `null` otherwise); `other` is any other subject's JSON as the ABI hands it
 * on (and `null` for `daemon`).
 */
export interface IntrospectResponse {
  subject: IntrospectSubject;
  daemon: DaemonHealth | null;
  other: unknown;
}

/**
 * One emitted change on a watch. `data` is the value `get` answers for the
 * watched key at that moment, or `null` when nothing answers there — a miss,
 * not the end of the stream. A value read from several cache records has no
 * single age or staleness, so a watch reports neither.
 */
export interface WatchEvent {
  data: unknown;
}
