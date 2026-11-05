// The SDK API table (docs/sdk-api.md), checked against this SDK's surface:
// sdks/api.json is the table in machine-readable form, and every name in it
// is spelled here in camelCase (classes in PascalCase).
import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { Client, Session, WatchStream } from '../src/client.js';
import * as errors from '../src/errors.js';
import { clientOptionsJson } from '../src/ffi_transport.js';
import {
  parseCacheRows,
  parseDaemonHealth,
  parseHello,
  parseIntrospect,
  parseWatchEvent,
} from '../src/parse.js';

const SDK_DIR = path.dirname(path.dirname(new URL(import.meta.url).pathname));
const table = JSON.parse(fs.readFileSync(path.join(SDK_DIR, '..', 'api.json'), 'utf8')) as {
  client: { options: string[]; methods: string[] };
  session: { methods: string[] };
  watch_stream: { methods: string[] };
  tiered_client: { methods: string[] };
  types: Record<string, string[]>;
  error_fields: string[];
  error_kinds: string[];
};

const camel = (name: string) => name.replace(/_([a-z])/g, (_, c: string) => c.toUpperCase());
const pascal = (name: string) => camel(name).replace(/^[a-z]/, (c) => c.toUpperCase());
/** The table's class name for a kind: `io_error` is `IoError`, `busy` is `BusyError`. */
const errorClassOf = (kind: string) => (pascal(kind).endsWith('Error') ? pascal(kind) : `${pascal(kind)}Error`);
const sorted = (names: string[]) => [...names].sort();
const methodsOf = (proto: object) =>
  sorted(Object.getOwnPropertyNames(proto).filter((name) => name !== 'constructor'));
const keysOf = (value: unknown) => sorted(Object.keys(value as object));

describe('the SDK API table', () => {
  it('Client has exactly the table methods, and the tiered one', () => {
    assert.deepEqual(
      methodsOf(Client.prototype),
      sorted([...table.client.methods, ...table.tiered_client.methods].map(camel)),
    );
  });

  it('Session has exactly the table methods', () => {
    assert.deepEqual(methodsOf(Session.prototype), sorted(table.session.methods.map(camel)));
  });

  it('WatchStream has exactly the table methods, and iterates', () => {
    assert.deepEqual(methodsOf(WatchStream.prototype), sorted(table.watch_stream.methods.map(camel)));
    assert.equal(typeof WatchStream.prototype[Symbol.asyncIterator], 'function');
  });

  it('every client option reaches bc_client_new under the table name', () => {
    const given = Object.fromEntries(table.client.options.map((name, i) => [camel(name), i]));
    const sent = JSON.parse(clientOptionsJson(given as never) as string) as object;
    assert.deepEqual(keysOf(sent), sorted(table.client.options));
  });

  it('one error class per kind, each a CombError carrying kind, message and libraryVersion', () => {
    const exported = errors as unknown as Record<string, unknown>;
    for (const kind of table.error_kinds) {
      const name = errorClassOf(kind);
      const ctor = exported[name] as (new (message: string, libraryVersion?: string) => errors.CombError) | undefined;
      assert.equal(typeof ctor, 'function', `${name} is exported`);
      const err = new ctor!('the words', '9.9.9');
      assert.ok(err instanceof errors.CombError, `${name} extends CombError`);
      assert.equal(err.kind, kind);
      assert.equal(err.message, 'the words');
      for (const field of table.error_fields) {
        assert.ok(camel(field) in err, `${name} carries ${camel(field)}`);
      }
      assert.equal(err.libraryVersion, '9.9.9');
      const fromEnvelope = errors.errorFromEnvelope({ kind, message: 'the words' }, '9.9.9');
      assert.ok(fromEnvelope instanceof ctor!, `kind ${kind} becomes ${name}`);
      assert.equal(fromEnvelope.message, 'the words', 'the message is the ABI message, unchanged');
    }
  });

  it('no error class beyond the table', () => {
    const exported = errors as unknown as Record<string, unknown>;
    const classes = Object.keys(exported).filter(
      (name) => typeof exported[name] === 'function' && /^[A-Z]/.test(name),
    );
    assert.deepEqual(sorted(classes), sorted(['CombError', ...table.error_kinds.map(errorClassOf)]));
  });
});

describe('the typed shapes are exactly the table fields', () => {
  const fields = (name: string) => sorted(table.types[name].map(camel));

  it('a status row', () => {
    const [row] = parseCacheRows([
      {
        provider: 'git',
        path: '/repo',
        source: 'source(head)',
        field: 'branch',
        value: 'main',
        age_ms: 12,
        kind: { kind: 'lifecycle', decay: 0, depth: 4, watches_files: true },
        poll_interval_secs: 10,
        poll_count: 3,
        polls_elapsed: 1,
        next_poll_in_secs: 7,
        failure: { consecutive_failures: 2 },
      },
    ]);
    assert.deepEqual(keysOf(row), fields('cache_row'));
  });

  it('the hello versions', () => {
    assert.deepEqual(
      keysOf(parseHello({ protocol_version: '1', daemon_version: '0.9.1' })),
      fields('hello_info'),
    );
  });

  const health = {
    pid: 1,
    version: '0.9.1',
    uptime_secs: 2,
    socket_path: '/tmp/s',
    config_path: '/c',
    requests_total: 3,
    in_flight: 0,
    active_watchers: 1,
    cache_entries: 4,
    watch_backend: 'native',
    reaper: { armed: true, visibility: 'confined', sweeps: 1, reaped: 0, kill_denied: 0 },
    verdicts: [{ level: 'PASS', message: 'ok' }],
  };

  it('the daemon health, its reaper status and its verdicts', () => {
    const parsed = parseDaemonHealth(health);
    assert.deepEqual(keysOf(parsed), fields('daemon_health'));
    assert.deepEqual(keysOf(parsed.reaper), fields('reaper_status'));
    assert.deepEqual(keysOf(parsed.verdicts[0]), fields('verdict'));
  });

  it('an introspect response, for daemon and for any other subject', () => {
    assert.deepEqual(keysOf(parseIntrospect('daemon', health)), fields('introspect_response'));
    assert.deepEqual(keysOf(parseIntrospect('cache', { entries: 1 })), fields('introspect_response'));
  });

  it('a watch event', () => {
    assert.deepEqual(keysOf(parseWatchEvent('main')), fields('watch_event'));
  });
});
