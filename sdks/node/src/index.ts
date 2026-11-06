/**
 * beachcomber Node.js client SDK
 *
 * A binding over the beachcomber C ABI (`libbeachcomber.{so,dylib}`) that
 * follows the one SDK API table (docs/sdk-api.md), using `koffi` (an
 * optional peer dependency) for direct FFI when available, and
 * falling back to shelling out to `comb` otherwise. Check
 * `client.transport()` to see which one is active.
 *
 * @example
 * ```typescript
 * import { Client } from 'beachcomber';
 *
 * const client = new Client();
 *
 * // What `comb get git.branch` prints there; null on a miss.
 * const branch = await client.get('git.branch', { cwd: '/path/to/repo' });
 *
 * // The template itself, unrendered: a facet, read like any key.
 * const template = await client.get('git:template.branch');
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
  type GetOptions,
  type ReadOptions,
  type HelloInfo,
  type CacheRow,
  type DaemonHealth,
  type ReaperStatus,
  type IntrospectSubject,
  type IntrospectResponse,
  type Verdict,
  type WatchEvent,
} from './client.js';
export { libraryCandidates, platformLibraryName, resolveCombOnPath } from './discovery.js';
export {
  CombError,
  type CombErrorKind,
  DaemonNotRunningError,
  ConnectionFailedError,
  IoError,
  ParseError,
  ServerError,
  TimeoutError,
  BusyError,
  PanicError,
  LibraryNotFoundError,
  LibrarySymbolError,
  UnsupportedError,
} from './errors.js';
export type { Envelope, WatchNextResult, WatchOutcome } from './envelope.js';
