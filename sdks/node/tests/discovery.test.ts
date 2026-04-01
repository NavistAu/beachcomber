import { describe, it, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { discoverSocketPath, getUid } from '../src/discovery.js';

describe('discoverSocketPath', () => {
  let savedXdg: string | undefined;
  let savedTmpdir: string | undefined;

  before(() => {
    savedXdg = process.env['XDG_RUNTIME_DIR'];
    savedTmpdir = process.env['TMPDIR'];
  });

  after(() => {
    if (savedXdg === undefined) {
      delete process.env['XDG_RUNTIME_DIR'];
    } else {
      process.env['XDG_RUNTIME_DIR'] = savedXdg;
    }
    if (savedTmpdir === undefined) {
      delete process.env['TMPDIR'];
    } else {
      process.env['TMPDIR'] = savedTmpdir;
    }
  });

  it('uses XDG_RUNTIME_DIR when set', () => {
    process.env['XDG_RUNTIME_DIR'] = '/run/user/1000';
    delete process.env['TMPDIR'];

    const p = discoverSocketPath();
    assert.equal(p, '/run/user/1000/beachcomber/sock');
  });

  it('falls back to TMPDIR when XDG_RUNTIME_DIR is unset', () => {
    delete process.env['XDG_RUNTIME_DIR'];
    process.env['TMPDIR'] = '/tmp/custom';

    const p = discoverSocketPath();
    const uid = getUid();
    assert.equal(p, `/tmp/custom/beachcomber-${uid}/sock`);
  });

  it('falls back to os.tmpdir() when both XDG_RUNTIME_DIR and TMPDIR are unset', () => {
    delete process.env['XDG_RUNTIME_DIR'];
    delete process.env['TMPDIR'];

    const p = discoverSocketPath();
    // Just verify the path ends with the right suffix
    assert.ok(p.endsWith('/sock'), `expected path to end with /sock, got: ${p}`);
    assert.ok(p.includes('beachcomber-'), `expected path to include 'beachcomber-', got: ${p}`);
  });

  it('XDG_RUNTIME_DIR takes priority over TMPDIR', () => {
    process.env['XDG_RUNTIME_DIR'] = '/run/user/2000';
    process.env['TMPDIR'] = '/tmp/other';

    const p = discoverSocketPath();
    assert.equal(p, '/run/user/2000/beachcomber/sock');
  });
});

describe('getUid', () => {
  it('returns a string', () => {
    const uid = getUid();
    assert.equal(typeof uid, 'string');
  });

  it('returns a non-empty string', () => {
    const uid = getUid();
    assert.ok(uid.length > 0);
  });

  it('returns a numeric-looking string', () => {
    const uid = getUid();
    assert.ok(/^\d+$/.test(uid), `expected numeric string, got: ${uid}`);
  });
});
