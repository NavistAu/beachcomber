/**
 * Typed response shapes for the beachcomber protocol.
 */

export interface HelloInfo {
  protocolVersion: string;
  daemonVersion: string;
}

export interface CacheRow {
  provider: string;
  field: string | null;
  path: string | null;
  value: unknown;
  ageMs: number;
  stale: boolean;
}

export interface Verdict {
  level: string;
  message: string;
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
