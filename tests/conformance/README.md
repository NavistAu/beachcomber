# Protocol Conformance Fixtures

Language-agnostic tests that every beachcomber SDK must pass. Each fixture
is a JSON file describing a protocol op request and structural expectations
on the daemon's response.

## Purpose

SDKs drift. When the wire protocol's `Status` response shape changed from a
daemon-health object to a cache-row array, six SDKs silently started
returning the wrong data. This suite prevents that: every SDK ships a
runner that loads these fixtures, drives its public API, and asserts
expectations.

A wire-level change that breaks a response shape fails every SDK's
conformance run in the same commit.

## Fixture shape

```json
{
  "name": "unique_test_id",
  "description": "Human-readable explanation.",
  "setup": [
    { "op": "put", "args": { "key": "seed", "data": {"v": 1} } }
  ],
  "test": { "op": "get", "args": {"key": "seed.v"} },
  "expect": {
    "status": "hit",
    "data_type": "number",
    "data_equals": 1
  }
}
```

### Top-level fields

| Field | Required | Shape |
|---|---|---|
| `name` | yes | unique string identifier |
| `description` | yes | one-line prose |
| `setup` | no | array of op descriptors to run first (ignored for expectations) |
| `test` | yes | the op descriptor whose response is asserted |
| `expect` | yes | assertions — see below |
| `env` | no | object mapping env var name to string value. The caller's environment for `get`, `eval` and `watch` fixtures — the only source of `env.*`. Defaults to empty. |
| `cwd` | no | string. The directory a `get`, `eval` or `watch` fixture reads at: every daemon query the read makes addresses it. Defaults to the runner's per-fixture temp directory. |

There is no `virtual` block. A fixture declares a template the way any
caller does — by a `setup` `put` to `<provider>:template`:

```json
{ "op": "put", "args": { "key": "widget:template", "data": { "label": "{{ comb.widgetdata.label }}" } } }
```

A nested field (`widget.group.label`) is a nested object in `data`. The
daemon is the only source of templates; the resolver takes no overrides.

### Op descriptor

```json
{ "op": "<hello|get|refresh|context|put|status|watch|introspect|eval>", "args": {...} }
```

`args` matches the wire request body for that op (minus `op` itself). See
`docs/protocol-spec.md` for each op's request schema.

### The `get` op

`get` is what `comb get` prints, the same in every SDK: the client-side
resolver (`libbeachcomber::resolve`, which every SDK's `get` binds through
the C ABI's `bc_get`) asks the daemon for each node — the cached value and
the template in effect at the address — and renders the template with the
fixture's `env` and `cwd`. `args.key` resolves exactly as `comb get <key>`
does; `args.force` (a boolean) and `args.wait` (a string: a duration or
`"forever"`) are the SDK's `force` and `wait`, handed on as given, so a
`wait` the library cannot read is refused in its words on every SDK:

- `"provider.field"` — the cached value, or, where a template is in effect
  at that address, the rendered template. Refs of the form `comb.P.F` /
  `comb.P` inside it are fetched from the daemon; a plain `P.F` ref is
  resolved the same way recursively, so a template may reference another
  provider's templated field. `env.X` comes from the fixture's `env` map,
  never the ambient process environment.
- `"provider"` (no `.`) — the whole merged namespace: every string leaf of
  the provider's template tree rendered, shape kept, merged over the cached
  data (rendered wins).

A miss — nothing stored or templated at the address — is `ok=true`, data
absent. An all-empty cascade is `""`, a hit.

The unrendered halves are facets, read like any key: `p:template.x` is the
template as stored, `p:put.x` and `p:source.x` those layers' values. No SDK
exposes the raw wire read.

A `get` fixture typically seeds cache values and templates via `setup`
`put` ops, supplies `env`/`cwd` as needed, and asserts the value and its
`data_type`.

Daemon queries a `get` makes are scoped to the fixture's `cwd`. A
pathless (global) `setup` `put` is still visible to them: a read at a path
resolves static records up the directory chain, and the global entry is the
last link of every chain (canon `field_resolution.md` §"Path resolution").
Seed with `args.path` only when the fixture is about path-keyed data.

### The `eval` op

`eval` is not a wire op either. It renders one **value expression** that is
stored nowhere, every ref resolved as it would be inside a stored template —
so `{ "op": "eval", "args": { "template": "{{ p.f }}" } }` and
`{ "op": "get", "args": { "key": "p.f" } }` agree by construction.
`args.template` is the only argument:

```json
{ "op": "eval", "args": { "template": "{{ env.A or comb.p.f }}" } }
```

The type rule the fixtures pin down:

- **exactly one `{{ expr }}`** spanning the whole source — evaluated as an
  expression, keeping its natural type. `{{ env.A | bool }}` yields a bool.
  Surrounding whitespace is not literal text.
- **anything else** — literal text, no tags at all, more than one tag, a
  `{% %}` statement tag, an unterminated marker, or an empty source — is a
  template and yields a **string**. `{{ 1 + 1 }} apples` is `"2 apples"`, not
  a number, and a source with no tags at all is its own literal text.

A missing or unknown ref is falsy at any depth, never an error: an absent
`env.*`, a cache miss, and a daemon-rejected key (invalid facet syntax)
all evaluate falsy, so `{{ nope.field or "x" }}` is `"x"`.

`env` and `cwd` apply exactly as they do for `get`, with the same chain
resolution. `exec()` is disabled: the runners build their client without the
`exec` option, as an SDK user does by default.

**Not every runner supports `eval`.** A runner whose binding doesn't
implement it must **skip** those fixtures and report them as skipped, never
as passed — a silent skip that reads as a pass is worse than no fixture.

### Expectation kinds

All expectations are **structural**. No byte-level matches on pid, age_ms,
uptime — those vary per run.

| Key | Meaning |
|---|---|
| `status` | "hit" (ok=true, data present), "miss" (ok=true, data absent), "ok" (ok=true, data not asserted), "error" (ok=false) |
| `data_type` | "string", "number", "bool", "object", "array", "null" |
| `data_equals` | exact deep-equality match on data |
| `data_as_text` | data interpreted as text (scalar stringification, or field-less object with a single value) equals this |
| `data_contains_field` | data is an object containing this field |
| `data_field_equals` | `{ "field": "<name>", "value": <json> }` — data.field deep-equals |
| `error_contains` | ok=false and response.error contains this substring |

Multiple expectations in the same `expect` block are AND-combined. A
fixture using a key outside this table must **fail** in every runner, never
be ignored — otherwise it can pass vacuously; the same goes for a `status`
value outside the four above.

## Isolation

Each fixture runs against a fresh daemon instance. Setup ops happen on the
same connection as the test op.

`watch` is special: the runner reads one event (the initial value) from the
stream and asserts against that; additional events are out of scope for
this fixture format.

## Directory layout

```
tests/conformance/
├── README.md
├── hello/
│   └── *.json
├── get/
│   └── *.json
├── refresh/
│   └── *.json
├── context/
│   └── *.json
├── put/
│   └── *.json
├── status/
│   └── *.json
├── watch/
│   └── *.json
├── introspect/
│   └── *.json
├── mapping/
│   └── *.json
└── eval/
    └── *.json
```

## Per-SDK runners

Each SDK ships a conformance runner that:

1. Spawns a fresh daemon on a temp socket (per fixture).
2. Runs each setup op via the SDK's public API.
3. Runs the test op via the SDK's public API.
4. Validates the SDK's typed result against the fixture's `expect`.
5. Reports pass/fail per fixture.

**The runner validates typed SDK results, not raw JSON.** A fixture asserting
`data_contains_field: "pid"` on `introspect{daemon}` must be checked via the
SDK's typed `DaemonHealth.pid` accessor, not by reparsing the raw JSON.
This is what catches typed-shape drift.

The Rust runner at `libbeachcomber/tests/suite/conformance.rs` is the reference
implementation; other SDK runners mirror its structure.
