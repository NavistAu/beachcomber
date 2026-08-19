/**
 * Error types for the beachcomber client.
 *
 * Every error the ABI can raise carries a stable, machine-readable `kind`
 * slug (see `libbeachcomber-ffi/src/envelope.rs::ErrorKind`) alongside the
 * human-readable message. `CombError` preserves it as a `kind` property
 * rather than flattening it into the message string.
 */

/** Stable machine-readable error kind slugs, mirroring the ABI's `ErrorKind`. */
export type CombErrorKind =
  | 'bad_flags'
  | 'busy'
  | 'panic'
  | 'version_skew'
  | 'daemon_not_running'
  | 'connection_failed'
  | 'io_error'
  | 'parse_error'
  | 'server_error'
  | 'timeout'
  | 'library_discovery_failed'
  | 'missing_symbol'
  | 'unsupported_transport';

/**
 * Base error class for beachcomber client errors. `kind` is a stable,
 * machine-readable slug — check it instead of matching on `message`.
 */
export class CombError extends Error {
  constructor(
    message: string,
    public readonly kind: CombErrorKind,
  ) {
    super(message);
    this.name = 'CombError';
    Object.setPrototypeOf(this, new.target.prototype);
  }
}

/** The daemon is not running and could not be reached. */
export class DaemonNotRunning extends CombError {
  constructor(detail?: string) {
    super(
      detail ? `comb daemon is not running (${detail})` : 'comb daemon is not running',
      'daemon_not_running',
    );
    this.name = 'DaemonNotRunning';
  }
}

/** The server returned an error response (`ok: false`). */
export class ServerError extends CombError {
  constructor(public readonly serverMessage: string) {
    super(`server error: ${serverMessage}`, 'server_error');
    this.name = 'ServerError';
  }
}

/** The response from the server (or the ABI envelope) could not be parsed. */
export class ParseError extends CombError {
  constructor(public readonly raw: string, cause?: string) {
    super(`parse error${cause ? ': ' + cause : ''} (raw: ${JSON.stringify(raw)})`, 'parse_error');
    this.name = 'ParseError';
  }
}

/** A `bc_get`/`bc_session_get` call set a reserved flag bit. */
export class BadFlagsError extends CombError {
  constructor(message: string) {
    super(message, 'bad_flags');
    this.name = 'BadFlagsError';
  }
}

/** A session or watch handle's connection is in use by another caller. */
export class BusyError extends CombError {
  constructor(message: string) {
    super(message, 'busy');
    this.name = 'BusyError';
  }
}

/** The native library panicked; caught at the FFI boundary. */
export class PanicError extends CombError {
  constructor(message: string) {
    super(message, 'panic');
    this.name = 'PanicError';
  }
}

/** The daemon's reported version does not match the loaded library's. */
export class VersionSkewError extends CombError {
  constructor(message: string) {
    super(message, 'version_skew');
    this.name = 'VersionSkewError';
  }
}

/** The client failed to connect to the daemon's socket. */
export class ConnectionFailedError extends CombError {
  constructor(message: string) {
    super(message, 'connection_failed');
    this.name = 'ConnectionFailedError';
  }
}

/** A lower-level I/O failure talking to the daemon. */
export class IoErrorError extends CombError {
  constructor(message: string) {
    super(message, 'io_error');
    this.name = 'IoErrorError';
  }
}

/** A call did not complete within its timeout. */
export class TimeoutError extends CombError {
  constructor(message: string) {
    super(message, 'timeout');
    this.name = 'TimeoutError';
  }
}

/** The native library could not be located. Every candidate path tried is named. */
export class LibraryDiscoveryError extends CombError {
  constructor(message: string) {
    super(message, 'library_discovery_failed');
    this.name = 'LibraryDiscoveryError';
  }
}

/** A required `bc_*` symbol was missing from the loaded library. */
export class MissingSymbolError extends CombError {
  constructor(message: string) {
    super(message, 'missing_symbol');
    this.name = 'MissingSymbolError';
  }
}

/** The current transport (subprocess) does not implement this operation. */
export class UnsupportedTransportError extends CombError {
  constructor(message: string) {
    super(message, 'unsupported_transport');
    this.name = 'UnsupportedTransportError';
  }
}

/**
 * Build the idiomatic `CombError` subclass for an ABI envelope's
 * `error: {kind, message}` object. Unrecognised kinds (forward
 * compatibility with a newer library) fall back to the generic
 * `CombError` with the kind preserved verbatim.
 */
export function errorFromEnvelope(err: { kind: string; message: string }): CombError {
  switch (err.kind) {
    case 'bad_flags':
      return new BadFlagsError(err.message);
    case 'busy':
      return new BusyError(err.message);
    case 'panic':
      return new PanicError(err.message);
    case 'version_skew':
      return new VersionSkewError(err.message);
    case 'daemon_not_running':
      return new DaemonNotRunning(err.message);
    case 'connection_failed':
      return new ConnectionFailedError(err.message);
    case 'io_error':
      return new IoErrorError(err.message);
    case 'parse_error':
      return new ParseError(err.message);
    case 'server_error':
      return new ServerError(err.message);
    case 'timeout':
      return new TimeoutError(err.message);
    default:
      return new CombError(err.message, err.kind as CombErrorKind);
  }
}
