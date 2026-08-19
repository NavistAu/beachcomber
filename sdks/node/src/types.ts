/**
 * Typed response shapes for the beachcomber protocol.
 */

export interface HelloInfo {
  protocolVersion: string;
  daemonVersion: string;
}

export type RowKind =
  | { kind: 'lifecycle'; decay: number; watches_files: boolean }
  | { kind: 'once' }
  | { kind: 'virtual' }
  | { kind: 'transient' };

export interface CacheRow {
  provider: string;
  field: string | null;
  path: string | null;
  value: unknown;
  ageMs: number;
  stale: boolean;
  kind?: RowKind;
  pollIntervalSecs?: number;
  keepAlivePolls?: number;
  pollsElapsed?: number;
  fseventsReinstate?: boolean;
  failure?: { consecutive_failures: number; suppressed_until_unix_ms?: number };
  /** Owning source name within the provider; undefined when not present on the wire. */
  source?: string;
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
  watchBackend?: string;
  reaper?: ReaperStatus;
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

export type IntrospectResponse =
  | { subject: 'daemon'; daemon: DaemonHealth }
  | { subject: Exclude<IntrospectSubject, 'daemon'>; other: unknown };

export interface WatchEvent {
  data: unknown;
  ageMs: number;
  stale: boolean;
}
