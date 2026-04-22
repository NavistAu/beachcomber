/**
 * Wire-level request and response types for the beachcomber protocol.
 * All communication is newline-delimited JSON over a Unix domain socket.
 */

// ---- Requests ----

export interface GetRequest {
  op: 'get';
  key: string;
  path?: string;
}

export interface RefreshRequest {
  op: 'refresh';
  key: string;
  path?: string;
}

export interface ContextRequest {
  op: 'context';
  path: string;
}

export interface StatusRequest {
  op: 'status';
}

export type Request =
  | GetRequest
  | RefreshRequest
  | ContextRequest
  | StatusRequest;

// ---- Responses ----

export interface ErrorResponse {
  ok: false;
  error: string;
}

export interface GetHitResponse {
  ok: true;
  data: unknown;
  age_ms: number;
  stale: boolean;
}

export interface GetMissResponse {
  ok: true;
  data?: null;
}

export type GetResponse = ErrorResponse | GetHitResponse | GetMissResponse;

export interface OkResponse {
  ok: true;
}

export type SimpleResponse = ErrorResponse | OkResponse;

export interface StatusResponse {
  ok: true;
  data: Record<string, unknown>;
}

export type StatusResponseFull = ErrorResponse | StatusResponse;

// ---- Serialisation helpers ----

/**
 * Serialise a request object to a newline-terminated JSON string ready for
 * writing to the socket.
 */
export function serialiseRequest(req: Request): string {
  return JSON.stringify(req) + '\n';
}

/**
 * Parse a raw response line into a typed object.
 * Throws SyntaxError if the line is not valid JSON.
 */
export function parseResponseLine(line: string): Record<string, unknown> {
  const trimmed = line.trim();
  const parsed: unknown = JSON.parse(trimmed);
  if (typeof parsed !== 'object' || parsed === null || Array.isArray(parsed)) {
    throw new SyntaxError(`expected JSON object, got: ${trimmed}`);
  }
  return parsed as Record<string, unknown>;
}
