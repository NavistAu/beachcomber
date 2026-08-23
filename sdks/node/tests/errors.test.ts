import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import {
  errorFromEnvelope,
  BadFlagsError,
  BusyError,
  PanicError,
  VersionSkewError,
  DaemonNotRunning,
  ConnectionFailedError,
  IoErrorError,
  ParseError,
  ServerError,
  TimeoutError,
  CombError,
} from '../src/errors.js';

describe('errorFromEnvelope', () => {
  const cases: Array<[string, unknown]> = [
    ['bad_flags', BadFlagsError],
    ['busy', BusyError],
    ['panic', PanicError],
    ['version_skew', VersionSkewError],
    ['daemon_not_running', DaemonNotRunning],
    ['connection_failed', ConnectionFailedError],
    ['io_error', IoErrorError],
    ['parse_error', ParseError],
    ['server_error', ServerError],
    ['timeout', TimeoutError],
  ];

  for (const [kind, ctor] of cases) {
    it(`maps kind "${kind}" to ${(ctor as { name: string }).name}`, () => {
      const err = errorFromEnvelope({ kind, message: 'boom' });
      assert.ok(err instanceof (ctor as new (...a: never[]) => Error));
      assert.ok(err instanceof CombError);
      assert.equal((err as CombError).kind, kind);
    });
  }

  it('falls back to the generic CombError for an unrecognised kind, preserving it', () => {
    const err = errorFromEnvelope({ kind: 'something_new', message: 'boom' });
    assert.ok(err instanceof CombError);
    assert.equal(err.kind, 'something_new');
    assert.equal(err.message, 'boom');
  });

  it('every error is a CombError with a kind property, not just a message', () => {
    const err = errorFromEnvelope({ kind: 'server_error', message: 'unknown provider: foo' });
    assert.equal(err.kind, 'server_error');
    assert.match(err.message, /unknown provider: foo/);
  });
});
