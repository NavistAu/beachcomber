import { describe, it, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { discoverSocketPath, getUid } from '../src/discovery.js';

describe('discoverSocketPath', () => {
  let savedSock: string | undefined;
  let savedXdg: string | undefined;
  let savedTmpdir: string | undefined;

  const restore = (key: string, saved: string | undefined): void => {
    if (saved === undefined) {
      delete process.env[key];
    } else {
      process.env[key] = saved;
    }
  };

  before(() => {
    savedSock = process.env['BEACHCOMBER_SOCKET'];
    savedXdg = process.env['XDG_RUNTIME_DIR'];
    savedTmpdir = process.env['TMPDIR'];
  });

  after(() => {
    restore('BEACHCOMBER_SOCKET', savedSock);
    restore('XDG_RUNTIME_DIR', savedXdg);
    restore('TMPDIR', savedTmpdir);
  });

  it('BEACHCOMBER_SOCKET takes precedence over everything', () => {
    process.env['BEACHCOMBER_SOCKET'] = '/custom/path/comb.sock';
    process.env['XDG_RUNTIME_DIR'] = '/run/user/1000';
    process.env['TMPDIR'] = '/should-not-be-used';

    assert.equal(discoverSocketPath(), '/custom/path/comb.sock');
  });

  it('ignores XDG_RUNTIME_DIR entirely', () => {
    delete process.env['BEACHCOMBER_SOCKET'];
    process.env['XDG_RUNTIME_DIR'] = '/run/user/1000';
    process.env['TMPDIR'] = '/should-not-be-used';

    const uid = getUid();
    assert.equal(discoverSocketPath(), `/tmp/beachcomber-${uid}/sock`);
  });

  it('falls back to /tmp when XDG_RUNTIME_DIR is unset', () => {
    delete process.env['BEACHCOMBER_SOCKET'];
    delete process.env['XDG_RUNTIME_DIR'];
    process.env['TMPDIR'] = '/should-not-be-used';

    const uid = getUid();
    assert.equal(discoverSocketPath(), `/tmp/beachcomber-${uid}/sock`);
  });

  it('ignores TMPDIR entirely', () => {
    delete process.env['BEACHCOMBER_SOCKET'];
    delete process.env['XDG_RUNTIME_DIR'];
    process.env['TMPDIR'] = '/var/folders/xyz';

    const uid = getUid();
    assert.equal(discoverSocketPath(), `/tmp/beachcomber-${uid}/sock`);
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
