/**
 * The transport abstraction over the beachcomber C ABI.
 *
 * Two implementations exist: `ffi_transport.ts` (koffi over the cdylib) and
 * `subprocess_transport.ts` (shelling out to `comb`). `Client`/`Session`/
 * `WatchStream` in `client.ts` are transport-agnostic — they call these
 * methods and interpret the returned `Envelope` uniformly.
 *
 * Handle types (`ClientHandle`, `SessionHandle`, `WatchHandle`) are opaque
 * to callers; each transport defines what it actually holds.
 */

import type { Envelope, WatchNextResult } from './envelope.js';

export type ClientHandle = unknown;
export type SessionHandle = unknown;
export type WatchHandle = unknown;

export interface NewClientOptions {
  socketPath?: string;
  timeoutMs?: number;
  autostart?: boolean;
}

export interface Transport {
  /** Which transport this is — surfaced to callers via `Client.transport()`. */
  readonly kind: 'ffi' | 'subprocess';
  /** The build identity of the code actually driving this transport (`bc_version()` for FFI). */
  readonly libraryVersion: string;

  newClient(options: NewClientOptions): ClientHandle;
  freeClient(handle: ClientHandle): void;

  get(handle: ClientHandle, key: string, path: string | undefined, flags: number): Promise<Envelope>;
  put(
    handle: ClientHandle,
    key: string,
    jsonData: string,
    ttl: string | undefined,
    path: string | undefined,
  ): Promise<Envelope>;
  putNull(handle: ClientHandle, key: string, path: string | undefined): Promise<Envelope>;
  refresh(handle: ClientHandle, key: string, path: string | undefined): Promise<Envelope>;
  status(handle: ClientHandle): Promise<Envelope>;
  introspect(handle: ClientHandle, subject: string, optionsJson: string | undefined): Promise<Envelope>;
  hello(handle: ClientHandle): Promise<Envelope>;
  resolve(
    handle: ClientHandle,
    key: string,
    cwd: string,
    envJson: string | undefined,
    overridesJson: string | undefined,
  ): Promise<Envelope>;
  evaluate(
    handle: ClientHandle,
    templateStr: string,
    cwd: string,
    envJson: string | undefined,
    overridesJson: string | undefined,
  ): Promise<Envelope>;

  openSession(handle: ClientHandle): SessionHandle;
  closeSession(session: SessionHandle): void;
  sessionGet(
    session: SessionHandle,
    key: string,
    path: string | undefined,
    flags: number,
  ): Promise<Envelope>;
  sessionPut(
    session: SessionHandle,
    key: string,
    jsonData: string,
    ttl: string | undefined,
    path: string | undefined,
  ): Promise<Envelope>;
  sessionSetContext(session: SessionHandle, path: string): Promise<Envelope>;

  openWatch(handle: ClientHandle, key: string, path: string | undefined): WatchHandle;
  watchNext(watch: WatchHandle, timeoutMs: number): Promise<WatchNextResult>;
  watchCancel(watch: WatchHandle): void;
  freeWatch(watch: WatchHandle): void;
}
