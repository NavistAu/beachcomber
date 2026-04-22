import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { serialiseRequest, parseResponseLine, type Request } from '../src/protocol.js';

describe('serialiseRequest', () => {
  it('serialises a get request without path', () => {
    const req: Request = { op: 'get', key: 'git.branch' };
    const line = serialiseRequest(req);
    assert.ok(line.endsWith('\n'), 'should end with newline');
    const parsed = JSON.parse(line.trim());
    assert.deepEqual(parsed, { op: 'get', key: 'git.branch' });
  });

  it('serialises a get request with path', () => {
    const req: Request = { op: 'get', key: 'git.branch', path: '/some/repo' };
    const line = serialiseRequest(req);
    const parsed = JSON.parse(line.trim());
    assert.deepEqual(parsed, { op: 'get', key: 'git.branch', path: '/some/repo' });
  });

  it('serialises a refresh request', () => {
    const req: Request = { op: 'refresh', key: 'git', path: '/repo' };
    const line = serialiseRequest(req);
    const parsed = JSON.parse(line.trim());
    assert.deepEqual(parsed, { op: 'refresh', key: 'git', path: '/repo' });
  });

  it('serialises a refresh request without path', () => {
    const req: Request = { op: 'refresh', key: 'hostname' };
    const line = serialiseRequest(req);
    const parsed = JSON.parse(line.trim());
    assert.deepEqual(parsed, { op: 'refresh', key: 'hostname' });
  });

  it('serialises a context request', () => {
    const req: Request = { op: 'context', path: '/my/project' };
    const line = serialiseRequest(req);
    const parsed = JSON.parse(line.trim());
    assert.deepEqual(parsed, { op: 'context', path: '/my/project' });
  });

  it('serialises a status request', () => {
    const req: Request = { op: 'status' };
    const line = serialiseRequest(req);
    const parsed = JSON.parse(line.trim());
    assert.deepEqual(parsed, { op: 'status' });
  });

  it('produces exactly one newline at the end', () => {
    const req: Request = { op: 'status' };
    const line = serialiseRequest(req);
    assert.equal(line.slice(-1), '\n');
    assert.notEqual(line.slice(-2, -1), '\n');
  });
});

describe('parseResponseLine', () => {
  it('parses a valid JSON object', () => {
    const line = '{"ok":true,"data":"main","age_ms":100,"stale":false}';
    const parsed = parseResponseLine(line);
    assert.equal(parsed['ok'], true);
    assert.equal(parsed['data'], 'main');
    assert.equal(parsed['age_ms'], 100);
    assert.equal(parsed['stale'], false);
  });

  it('trims leading/trailing whitespace', () => {
    const line = '  {"ok":true}  ';
    const parsed = parseResponseLine(line);
    assert.equal(parsed['ok'], true);
  });

  it('throws SyntaxError on invalid JSON', () => {
    assert.throws(() => parseResponseLine('not json'), SyntaxError);
  });

  it('throws SyntaxError on JSON array', () => {
    assert.throws(() => parseResponseLine('[1,2,3]'), SyntaxError);
  });

  it('throws SyntaxError on JSON null', () => {
    assert.throws(() => parseResponseLine('null'), SyntaxError);
  });

  it('throws SyntaxError on JSON string', () => {
    assert.throws(() => parseResponseLine('"hello"'), SyntaxError);
  });

  it('parses a miss response (no data field)', () => {
    const line = '{"ok":true}';
    const parsed = parseResponseLine(line);
    assert.equal(parsed['ok'], true);
    assert.ok(!('data' in parsed));
  });

  it('parses an error response', () => {
    const line = '{"ok":false,"error":"unknown provider: foo"}';
    const parsed = parseResponseLine(line);
    assert.equal(parsed['ok'], false);
    assert.equal(parsed['error'], 'unknown provider: foo');
  });
});
