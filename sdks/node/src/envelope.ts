/**
 * The JSON envelope every `bc_*` ABI call (and its subprocess-transport
 * equivalent) returns:
 *
 *   {"ok": true,  "data": <op result>}
 *   {"ok": false, "error": {"kind": "...", "message": "..."}}
 *
 * See `libbeachcomber-ffi/src/envelope.rs`.
 */

import { errorFromEnvelope } from './errors.js';

export interface OkEnvelope {
  ok: true;
  data?: unknown;
}

export interface ErrEnvelope {
  ok: false;
  error: { kind: string; message: string };
}

export type Envelope = OkEnvelope | ErrEnvelope;

/** Unwrap an envelope's `data`, throwing the idiomatic `CombError` on `ok: false`. */
export function unwrap(env: Envelope): unknown {
  if (!env.ok) {
    throw errorFromEnvelope(env.error);
  }
  return env.data;
}

/** The five machine-readable outcomes of a `bc_watch_next` poll. */
export type WatchOutcome = 'event' | 'timeout' | 'eof' | 'cancelled';

export interface WatchNextResult {
  outcome: WatchOutcome;
  data?: unknown;
  ageMs?: number | null;
  stale?: boolean | null;
}
