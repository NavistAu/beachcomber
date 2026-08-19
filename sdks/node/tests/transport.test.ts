import './helpers.js'; // side effect: defaults BEACHCOMBER_LIB to a locally built dylib/so when present
import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { daemonAvailable } from './helpers.js';
import { koffiAvailable } from '../src/ffi_transport.js';
import { createSubprocessTransport } from '../src/subprocess_transport.js';
import { selectTransport } from '../src/transport_select.js';

describe('subprocess transport', () => {
  it('reports kind "subprocess"', () => {
    const t = createSubprocessTransport();
    assert.equal(t.kind, 'subprocess');
  });

  it('names the resolved comb binary in its library version identity', () => {
    const t = createSubprocessTransport();
    assert.match(t.libraryVersion, /^subprocess:/);
  });
});

describe('transport selection', () => {
  it('koffi (the optional peer dependency) is installed in this environment', () => {
    // Documents which path selectTransport() takes below — this test's own
    // outcome IS the "koffi availability" report.
    assert.equal(koffiAvailable(), true, 'expected koffi to be resolvable via require.resolve');
  });

  it('selects the FFI transport when koffi and the library are both available', { skip: !daemonAvailable() }, () => {
    const t = selectTransport();
    assert.equal(t.kind, 'ffi');
    assert.ok(t.libraryVersion.length > 0);
  });
});
