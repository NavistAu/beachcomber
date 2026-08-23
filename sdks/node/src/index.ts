/**
 * beachcomber Node.js client SDK
 *
 * A binding over the beachcomber C ABI (`libbeachcomber.{so,dylib}`), using
 * `koffi` (an optional peer dependency) for direct FFI when available, and
 * falling back to shelling out to `comb` otherwise. Check
 * `client.transport()` to see which one is active.
 *
 * @example
 * ```typescript
 * import { Client } from 'beachcomber';
 *
 * const client = new Client();
 *
 * const result = await client.get('git.branch', '/path/to/repo');
 * if (result.isHit) {
 *   console.log(result.getString()); // e.g. "main"
 * }
 *
 * const session = await client.session();
 * await session.setContext('/path/to/repo');
 * const branch = await session.get('git.branch');
 * const dirty = await session.get('git.dirty');
 * session.close();
 * ```
 */

export {
  Client,
  Session,
  WatchStream,
  type ClientOptions,
  type ResolveOptions,
  type CombResult,
  type HelloInfo,
  type CacheRow,
  type DaemonHealth,
  type ReaperStatus,
  type IntrospectSubject,
  type IntrospectResponse,
  type Verdict,
  type WatchEvent,
} from './client.js';
export { discoverSocketPath, getUid, libraryCandidates, platformLibraryName, resolveCombOnPath } from './discovery.js';
export {
  CombError,
  type CombErrorKind,
  DaemonNotRunning,
  ServerError,
  ParseError,
  BadFlagsError,
  BusyError,
  PanicError,
  VersionSkewError,
  ConnectionFailedError,
  IoErrorError,
  TimeoutError,
  LibraryDiscoveryError,
  MissingSymbolError,
  UnsupportedTransportError,
} from './errors.js';
export type { Envelope, WatchNextResult, WatchOutcome } from './envelope.js';
