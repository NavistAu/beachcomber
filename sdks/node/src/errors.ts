/**
 * Base error class for beachcomber client errors.
 */
export class CombError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'CombError';
    Object.setPrototypeOf(this, new.target.prototype);
  }
}

/**
 * The daemon is not running and could not be reached.
 */
export class DaemonNotRunning extends CombError {
  constructor(socketPath?: string) {
    super(
      socketPath
        ? `comb daemon is not running (tried: ${socketPath})`
        : 'comb daemon is not running',
    );
    this.name = 'DaemonNotRunning';
  }
}

/**
 * The server returned an error response (ok: false).
 */
export class ServerError extends CombError {
  constructor(public readonly serverMessage: string) {
    super(`server error: ${serverMessage}`);
    this.name = 'ServerError';
  }
}

/**
 * The response from the server could not be parsed.
 */
export class ParseError extends CombError {
  constructor(public readonly raw: string, cause?: string) {
    super(`parse error${cause ? ': ' + cause : ''} (raw: ${JSON.stringify(raw)})`);
    this.name = 'ParseError';
  }
}
