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
    "data_equals": 1,
    "stale": false
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
| `virtual` | no | object mapping a field key (`"provider.field"`) or a bare provider name to an expression string. Feeds the client-side resolver for `resolve` and `eval` fixtures — see below. Defaults to empty. |
| `env` | no | object mapping env var name to string value. Feeds the client-side resolver's `env.*` refs for `resolve` and `eval` fixtures. Defaults to empty. |
| `cwd` | no | string. Feeds the client-side resolver's `cwd`: the path-expression variable for `resolve` fixtures, and the path daemon queries are scoped to for both `resolve` and `eval`. Defaults to the runner's per-fixture temp directory. |

### Op descriptor

```json
{ "op": "<hello|get|refresh|context|put|status|watch|introspect|resolve|eval>", "args": {...} }
```

`args` matches the wire request body for that op (minus `op` itself). See
`docs/protocol-spec.md` for each op's request schema.

### The `resolve` op

`resolve` is not a wire op — it never reaches the daemon. It exercises
client-side field resolution: virtual field expressions and path expressions,
both evaluated in-process against the fixture's `virtual`, `env`, and `cwd`.
`args.key` is the only argument:

- `"provider.field"` (contains a `.`) — evaluate that virtual field. The
  fixture's `virtual` entry for the same `"provider.field"` key (if any)
  overrides the built-in expression; otherwise the built-in default (if one
  exists) is used. Refs of the form `cache.P.F` / `cache.P` inside the
  expression are fetched live from the daemon — seed them with `setup` `put`
  ops first. `env.X` refs come from the fixture's `env` map, not the
  ambient process environment.
- `"provider"` (no `.`) — evaluate that provider's path expression. The
  fixture's `virtual` entry keyed by the bare provider name (if any)
  overrides the built-in path expression; `cwd` and `env` come from the
  fixture. A falsy/undefined result is a `miss` (`ok=true`, data absent),
  matching the "no per-path variant" contract.

A `resolve` fixture typically seeds cache values via `setup` `put` ops,
declares `virtual`, supplies `env`/`cwd` as needed, and asserts the resolved
value and its `data_type`.

Resolution is client-side (see `docs/canon/` for the field-resolution model),
so this is deliberately **not** wired through a daemon config file — the
fixture's `virtual`/`env`/`cwd` feed the resolver call directly (for the C
ABI this is `overrides_json` / `env_json` / `cwd`).

Daemon queries a `resolve` makes are scoped to the fixture's `cwd`. A
pathless (global) `setup` `put` is still visible to them: a virtual provider
declares no path expression, so a path-scoped `get` falls back to the global
slot (canon `field_resolution.md` §"Path resolution" — the prose on virtual
providers, not invariant 2, which is only about an empty/falsy path
expression). Seed with `args.path` only when the fixture is about
path-keyed data.

### The `eval` op

`eval` is not a wire op either. It evaluates one **value expression** in any
of the three forms canon `field_resolution.md` (invariant 14) defines, against
the same client-side context `resolve` uses. `args.template` is the only
argument:

```json
{ "op": "eval", "args": { "template": "{{ env.A or cache.p.f }}" } }
```

The type rule the fixtures pin down:

- **bare expression** (no tag markers) — evaluated as an expression, keeping
  its natural type. `env.A != ""` yields a bool.
- **exactly one `{{ expr }}`** spanning the whole source — identical to the
  bare form, natural type preserved. Surrounding whitespace is not literal
  text.
- **anything else** — literal text, more than one tag, a `{% %}` statement
  tag, an unterminated marker, or an empty source — is a template and yields
  a **string**. `{{ 1 + 1 }} apples` is `"2 apples"`, not a number.

A missing or unknown ref is falsy at any depth, never an error: an absent
`env.*`, a cache miss, and a daemon-rejected key (an unregistered provider)
all evaluate falsy, so `{{ nope.field or "x" }}` is `"x"`.

`virtual`, `env` and `cwd` apply exactly as they do for `resolve`: `virtual`
entries keyed `"provider.field"` define the field expressions a `provider.field`
reference resolves through (transitively — a virtual field may reference
another), `env` supplies `env.*`, and `cwd` scopes the daemon queries — with
the same global-slot fallback `resolve` gets.

**Not every runner supports `resolve`/`eval`.** A runner whose binding
doesn't implement them must **skip** those fixtures and report them as
skipped, never as passed — a silent skip that reads as a pass is worse than
no fixture. The Lua runner skips both when it falls back to its subprocess
transport, which has no client-side resolver.

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
| `age_ms_present` | boolean — age_ms is (not) null |
| `stale` | boolean — stale equals this |
| `error_contains` | ok=false and response.error contains this substring |

Multiple expectations in the same `expect` block are AND-combined.

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
├── resolve/
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

The Rust runner at `libbeachcomber/tests/conformance.rs` is the reference
implementation; other SDK runners mirror its structure.
