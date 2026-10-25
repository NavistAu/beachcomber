import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { errorFromEnvelope, CombError, ParseError, ServerError } from '../src/errors.js';
import { parseCacheRows } from '../src/parse.js';

// One class per kind is the API surface test's (tests/api_surface.test.ts);
// these are the rest of docs/sdk-api.md's error rules.
describe('errors', () => {
  it('an unrecognised kind is a bare CombError, the kind kept as it came', () => {
    const err = errorFromEnvelope({ kind: 'something_new', message: 'boom' }, '1.0.0');
    assert.ok(err instanceof CombError);
    assert.equal(err.constructor, CombError);
    assert.equal(err.kind, 'something_new');
    assert.equal(err.message, 'boom');
    assert.equal(err.libraryVersion, '1.0.0');
  });

  it("the message is the library's words alone, and the version a field of its own", () => {
    const err = errorFromEnvelope({ kind: 'server_error', message: 'unknown provider: foo' }, '0.9.1');
    assert.ok(err instanceof ServerError);
    assert.equal(err.message, 'unknown provider: foo');
    assert.equal(err.libraryVersion, '0.9.1');
    assert.equal(err.name, 'ServerError');
  });

  it('status data that is not an array is a ParseError, not an empty list', () => {
    assert.throws(
      () => parseCacheRows({ not: 'rows' }, '0.9.1'),
      (err: unknown) => err instanceof ParseError && err.kind === 'parse_error' && err.libraryVersion === '0.9.1',
    );
  });
});
