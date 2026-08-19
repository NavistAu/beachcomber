/**
 * Chooses the FFI transport (`koffi`) when available, otherwise the
 * subprocess fallback.
 *
 * The fallback tier is entered only when `koffi` is absent **by
 * configuration** — a peer dependency that was never installed. If `koffi`
 * resolves but the native library itself cannot be found or is missing a
 * required symbol, that is a broken install and must fail loudly rather
 * than silently degrade to the slower subprocess tier (Phase 4's common
 * contract, point 2).
 */

import { koffiAvailable, createFfiTransport } from './ffi_transport.js';
import { createSubprocessTransport } from './subprocess_transport.js';
import type { Transport } from './transport.js';

let cached: Transport | undefined;

export function selectTransport(): Transport {
  if (!cached) {
    cached = koffiAvailable() ? createFfiTransport() : createSubprocessTransport();
  }
  return cached;
}

/** Test-only: forget the cached transport so the next call re-selects. */
export function resetTransportForTests(): void {
  cached = undefined;
}
