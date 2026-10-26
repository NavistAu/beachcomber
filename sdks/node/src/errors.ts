/**
 * The error kinds of docs/sdk-api.md: one class per kind, each a `CombError`
 * carrying `kind` (the slug), `message` and `libraryVersion`.
 *
 * `message` is the library's words unchanged — the same text the C ABI's
 * envelope carries — and `libraryVersion` the `bc_version()` of the library
 * that raised it, never folded into the message. An error this SDK raises
 * itself (`library_not_found`, `library_symbol`, `unsupported`) carries its
 * own message.
 */

/** The error kinds, as the ABI and this SDK spell them. */
export type CombErrorKind =
  | 'daemon_not_running'
  | 'connection_failed'
  | 'io_error'
  | 'parse_error'
  | 'server_error'
  | 'timeout'
  | 'busy'
  | 'panic'
  | 'version_skew'
  | 'library_not_found'
  | 'library_symbol'
  | 'unsupported';

/**
 * The base of every error this SDK raises. `kind` is a stable,
 * machine-readable slug — check it, or the subclass, instead of matching on
 * `message`. An unrecognised kind from a newer library is a bare `CombError`
 * with the kind kept as it came.
 */
export class CombError extends Error {
  constructor(
    message: string,
    public readonly kind: CombErrorKind | string,
    public readonly libraryVersion?: string,
  ) {
    super(message);
    this.name = new.target.name;
    Object.setPrototypeOf(this, new.target.prototype);
  }
}

/** A subclass for one kind: `new XError(message, libraryVersion?)`. */
function kindError(kind: CombErrorKind) {
  return class extends CombError {
    constructor(message: string, libraryVersion?: string) {
      super(message, kind, libraryVersion);
    }
  };
}

/** No daemon answers, and none was started. */
export class DaemonNotRunningError extends kindError('daemon_not_running') {}
/** An explicit `socketPath` could not be connected to. */
export class ConnectionFailedError extends kindError('connection_failed') {}
/** A read or write on the socket failed. */
export class IoError extends kindError('io_error') {}
/** A request, a response or a template could not be read. */
export class ParseError extends kindError('parse_error') {}
/** The daemon refused the request, or a template failed to render. */
export class ServerError extends kindError('server_error') {}
/** A read or write ran out `timeoutMs`, or `nextEvent` ran out its timeout. */
export class TimeoutError extends kindError('timeout') {}
/** A flag bit the ABI does not define was set. */
/** A session or watch is in use by another caller. */
export class BusyError extends kindError('busy') {}
/** The library panicked; the panic was caught at the boundary. */
export class PanicError extends kindError('panic') {}
/** The daemon is a different version from the library. */
export class VersionSkewError extends kindError('version_skew') {}
/** No `libbeachcomber` was found where discovery looks. Every candidate tried is named. */
export class LibraryNotFoundError extends kindError('library_not_found') {}
/** The library found lacks a `bc_*` function this SDK calls. */
export class LibrarySymbolError extends kindError('library_symbol') {}
/** The subprocess transport cannot do this operation. */
export class UnsupportedError extends kindError('unsupported') {}

const BY_KIND: Record<CombErrorKind, new (message: string, libraryVersion?: string) => CombError> = {
  daemon_not_running: DaemonNotRunningError,
  connection_failed: ConnectionFailedError,
  io_error: IoError,
  parse_error: ParseError,
  server_error: ServerError,
  timeout: TimeoutError,
  busy: BusyError,
  panic: PanicError,
  version_skew: VersionSkewError,
  library_not_found: LibraryNotFoundError,
  library_symbol: LibrarySymbolError,
  unsupported: UnsupportedError,
};

/** The error for an envelope's `error: {kind, message}`, from the library at `libraryVersion`. */
export function errorFromEnvelope(
  err: { kind: string; message: string },
  libraryVersion?: string,
): CombError {
  const ctor = BY_KIND[err.kind as CombErrorKind];
  return ctor ? new ctor(err.message, libraryVersion) : new CombError(err.message, err.kind, libraryVersion);
}
