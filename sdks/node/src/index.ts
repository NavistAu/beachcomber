/**
 * beachcomber Node.js client SDK
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

export { Client, Session, type CombResult } from './client.js';
export { discoverSocketPath, getUid } from './discovery.js';
export { CombError, DaemonNotRunning, ParseError, ServerError } from './errors.js';
export {
  type GetRequest,
  type RefreshRequest,
  type ContextRequest,
  type ListRequest,
  type StatusRequest,
  type Request,
  type ProviderInfo,
  parseResponseLine,
  serialiseRequest,
} from './protocol.js';
