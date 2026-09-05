# Field Resolution

**Status:** canonical. Describes how a consumer query for `provider.field` is resolved into a typed value from the daemon's cache, the calling shell's environment, and its working directory — and where each step happens (daemon vs client). Tests must match this document; disagreements mean the code is wrong.

**Scope:** the client-side resolution layer — the field-type taxonomy, the `cache.*` and `env.*` namespaces, path resolution (which cache entry a query reads), value resolution (the per-field expression, cascades, selection), the value tree, and typed output. The daemon's Source refresh model — how cached values are produced and invalidated — is owned by [`provider_source.md`](./provider_source.md); the cache state machine by [`cache-lifecycle.md`](./cache-lifecycle.md). Out-of-scope items are listed at the end.

## Glossary

| Term | Meaning |
|---|---|
| **Consumer query** | A request for `provider.field` (e.g. `aws.region`). |
| **Key** | `namespace.field…`, the dotted address of a node in the value tree. A field may be nested, so a key names any node. |
| **Namespace** | The first segment of a key: a provider (`git`) or a facet of one (`git:template`). |
| **Record** | One stored value at a (key, path). It carries exactly one source: a Source of its provider, `put`, or `config:<file>`. A record's identity is (key, path); its source is an attribute. |
| **Path** | The directory in the cache tuple, and only that. `$HOME` is the global path. |
| **Resolution** | Producing a typed value for a consumer query, client-side, from the cache + `env.*` + `cwd`. |
| **Cache** | The daemon's store of values, keyed by `(provider, path, name)`. |
| **Cached value** | A value held in the cache. Addressed in expressions as `cache.<provider>.<field>`, read at the resolved path. |
| **Facet** | `provider:facet`, a namespace holding data about a provider; its records are static and resolve up the directory chain like any static record; `template` is the facet that holds value expressions. |
| **Value expression** | The per-field expression that computes a field's resolved value. A cached field's default is the identity (its own cached value). |
| **Virtual field** | A field whose value expression is non-default — selection/computation over `cache.*` and `env.*`. It may have no cached value of its own. |
| **`env.*`** | The calling shell's environment, a value-only namespace; a miss yields `""`. |
| **`cwd`** | The calling shell's working directory; the cache-key path when no explicit path is given. |
| **Cascade** | A first-non-empty expression, `a or b or c`; the idiomatic shape for overrides and fallbacks. |
| **Selector** | A shell environment variable whose value designates which source a field's value comes from. |
| **Data provider** | A provider whose cached fields enumerate the variants of an env-selected source (e.g. `aws_profiles`). |
| **Consumer namespace** | A provider name composed of virtual fields presenting a user-facing surface (e.g. `aws`). |
| **Value tree** | The tree of resolved values. Addressable at any node: a leaf is a scalar, an interior node a subtree returned as an object. |

## Core model

The daemon is a **cache** of values keyed by `(provider, path, name)`. It produces and invalidates those values (see `provider_source.md`) but does not decide what a *consumer* sees. Resolution — the client's job — answers a query in two phases:

1. **Path resolution (identification)** — *which cache entry*: the cache-key path is the caller's directory, an explicit path, or the global path, `$HOME`. With the provider and field name from the query, this fixes the cache coordinate `(provider, path, name)`.
2. **Value resolution** — *what the value is*: evaluate the field's value expression, where `cache.*` reads cached values at the resolved path and `env.*` reads the shell.

```mermaid
graph TD
    Q[query: provider.field] --> PR[path resolution<br/>cwd, --path, or $HOME]
    PR --> COORD[cache coordinate<br/>provider, path, name]
    Q --> VR[value resolution<br/>evaluate value expression]
    COORD --> VR
    VR -->|cache.*| C[(daemon cache)]
    VR -->|env.*| SH[shell env]
    VR --> O[typed output]
```

**Locus rule.** The daemon's environment is frozen at launch and wrong for the caller, so the daemon never reads the shell's `env` or `cwd`. The client supplies every per-shell input; the daemon receives only a concrete cache coordinate.

### Field types

Every field is one of five types. The first three are **cached values** (they differ only in *how* the cache is filled — a `provider_source.md` concern); the last two are computed client-side:

| Type | Value comes from | Where resolved | Cached? | Env-aware? |
|---|---|---|---|---|
| **native** | Rust `execute()` over files/syscalls | daemon | yes | no |
| **external** | a script / library / HTTP backend run by the daemon | daemon | yes | no |
| **literal** | literal data stored via `comb put` | daemon | yes (opt TTL) | no |
| **virtual** | a value expression over `cache.*` + `env.*` | client | no | **yes** |
| **env** | the caller's shell environment variable | client | no | by definition |

From the resolution layer the three cached types are identical — all read via `cache.*`. The type distinction that matters here is **cached vs virtual vs env**. `literal` and `virtual` are the two **user-defined** types — both declare a field without a Source: a literal is stored data (daemon-side, env-blind); a virtual is an expression (client-side, env-aware).

### The `cache.*` namespace

A cached value is addressed as `cache.<provider>.<field>` and read at the path fixed in phase 1. `cache.*` returns the stored value **directly**, bypassing any field's value expression — it is the raw material, not a resolved field.

A query's referenced fields fall into two kinds, and the distinction is load-bearing:

- `cache.<provider>.<field>` — the **raw cached value**.
- `<provider>.<field>` — another field's **resolved value** (its own value expression evaluated; recursive, cycle-detected).

For a cached field with the default identity expression the two coincide; they diverge only when a field is virtual.

### `env.*` namespace

`env.X` is the caller's `$X`, exposed as a value-only namespace.

- A **miss yields `""`**, never an error.
- **Directly queryable**: `comb get env.X` returns the value (valid, if rarely useful alone — its purpose is to be a term in a larger expression).
- **Never contacts the daemon**; carries no `age`/`ttl`/`stale` metadata (live by definition).
- Available wherever value expressions are evaluated, including inside `{{ }}` tags (`{{ env.X }}`).
- `--format json` emits the bare value (no metadata wrapper); `--format sh` shell-escapes it.

### Path resolution

The cache-key path of a query is the caller's directory, an explicit path, or `$HOME`, the global path. It is never computed by an expression.

A read at a path collects the **static records** for the key — those whose source is `put` or `config:<file>` — along the chain from the path up to `$HOME`, the nearest winning. **Provider-produced records** keep their source's declared scope: a path-scoped source's records are read at the addressed path and a global-scoped source's at `$HOME`; neither walks the chain. A path outside `$HOME` walks up to `/` and then `$HOME`. `--global` reads `$HOME` alone. This is layering, not a fallback: a fallback picks one whole path when another is empty; layering always merges the chain.

### Value resolution

Every field has a **value expression**. A **template** — literal text around a tag, or more than one tag, e.g. `{{ git.branch }}{% if git.dirty %}*{% endif %}` — is a string-valued field. A value expression written as exactly one `{{ expr }}` evaluates to the expression's natural type; one with literal text or more than one tag evaluates to a string; one with no tags of any kind is its literal text.

- A **cached** field's value expression defaults to the **identity** — its own cached value, `cache.<provider>.<field>`.
- A **virtual** field's value expression is custom: selection or computation over `cache.*`, `env.*`, and other fields' resolved values. A virtual field **need not have a cached value of its own** — its value is entirely the expression.

Properties:

- **Cascade idiom:** `a or b or c` — the first non-empty term wins; all-empty ⇒ `""`.
- **Filters:** an expression may apply inline filter/transforms — e.g. `cache.python.local_venv_name or (env.VIRTUAL_ENV | basename)`.
- **Typed:** the result keeps its natural type (string/bool/number); a missing reference is falsy.
- **Ref discovery:** every reference in an expression is enumerated (nested undeclared-variable analysis, not byte scanning) — not only the first.
- **Cycle detection:** a `<provider>.<field>` reference that is itself virtual is evaluated recursively; a reference cycle is a config error, not a panic.

**Overriding a cached value under its own name.** When a consumer wants "env override on top of a single cached value," the expression references the **cached** value, not the resolved field:

```
terraform.workspace = {{ env.TF_WORKSPACE or cache.terraform.workspace }}
```

This is not a cycle — `cache.terraform.workspace` is the stored value, distinct from the resolved field `terraform.workspace`. Because of this, a cached value **keeps its own name**; no rename is needed to let a same-named virtual field build on it.

**Selection over distinct cached values.** Where several *distinct* values feed one consumer field, each keeps its own name and the virtual field selects among them:

```
python.version = {{ env.PYENV_VERSION or env.MISE_PYTHON_VERSION
                 or cache.mise.python or cache.asdf.python or cache.python.venv_version }}
```

`cache.python.venv_version` (the venv's version), `cache.mise.python`, `cache.asdf.python` are genuinely different things with their own names; `python.version` is the selection logic, with no cached value of its own.

### Env-driven selection

A selector is per-shell, so the daemon cannot apply it.

**A selector picks one value out of a cached set.** All variants live where the daemon already looks (one file or directory), so the daemon publishes the whole set as a **data provider**: a provider whose cached fields are the variants, keyed by variant name (e.g. `aws_profiles.default`, `aws_profiles.staging`, each an object of per-variant values). The consumer field is a virtual field that **indexes** the set by the selector's value:

```
<namespace>.<field> = {{ env.<DIRECT> or cache.<data_provider>[ env.<SELECTOR> or <default_key> ].<field> }}
```

The daemon caches every variant regardless of the shell; the client does the indexing. `comb get <data_provider>` returns the whole set; `comb get <namespace>` returns the computed consumer fields.

A selector designates a value within a cache entry and is applied in the value phase; the daemon never reads the selector.

### The value tree

Resolution produces a **tree of values**. A query addresses any node by its path:

- a **leaf** resolves to a scalar value — `aws_profiles.staging.region` → `"us-east-1"`;
- an **interior node** resolves to its subtree, returned as an object — `aws_profiles.staging` → `{ region: … }`, `aws_profiles` → every profile, `aws` → its computed fields.

Whether a node is **cached** (stored by the daemon) or **computed** (a virtual field's expression) does not change how it is addressed. `provider` is the root of that provider's subtree; `provider.field.subkey…` walks down it; an expression walks the cached tree the same way (`cache.aws_profiles[key].region`).

A template tree may itself have interior nodes; each is addressable like any other node and resolves to the object of its rendered records. A template at the same path as a cached value wins on read.

### Typed output

A resolved value keeps its natural type. `--format json` emits bool / number / string (or object, for an interior node) as-is; `--format text`/`sh` stringify (and `sh` shell-escapes). The type is the expression's natural result type — no bespoke type tags.

## Invariants

1. Resolution is client-side. The daemon never reads the calling shell's `env` or `cwd`; it receives only a concrete cache coordinate `(provider, path, name)`.
2. A query's cache-key path is the caller's directory, an explicit path, or `$HOME`, the global path; it is never computed. A read collects static records up the directory chain to `$HOME`, nearest winning; provider-produced records are read at their source's declared scope.
3. The daemon is a cache of values keyed by `(provider, path, name)`, exposed to expressions as `cache.<provider>.<field>` and read at the resolved path.
4. Every field has a value expression. A cached field's default is the identity (its own cached value); a virtual field's is a custom expression and may have no cached value of its own.
5. `cache.<provider>.<field>` returns the stored value, bypassing field expressions; `<provider>.<field>` evaluates that field's value expression (recursive, cycle-detected). A field may override its own cached value via `cache.<self>` without self-reference; cached values keep their own names.
6. `env.*` is a value-only namespace: a miss yields `""`, never an error; it never contacts the daemon, carries no metadata, and is directly queryable.
7. A virtual field's *value* is rendered client-side and never cached; its *template* is data the daemon holds at `provider:template` and never evaluates.
8. A resolved value keeps its natural type into `--format json`; `text`/`sh` stringify it. A missing reference is falsy.
9. A cascade resolves to its first non-empty term; all-empty resolves to `""`. A reference cycle is a config error, not a panic.
10. Value-phase env selection — the selector designates a value within a cache entry — publishes the whole set as a data provider keyed by variant; the client indexes it by the selector value; the daemon never reads the selector.
11. *(deleted — path-phase env selection no longer exists.)*
12. A query may address any node of the value tree: a leaf resolves to a scalar, an interior node (a field whose value is an object, or a bare provider/namespace) to its subtree as an object. A consumer namespace's subtree is its evaluated virtual fields; a cached provider's, its cached fields. Addressing is independent of whether a node is cached or computed.
13. Value expressions are configuration. Built-in ones ship as managed files under `<config-dir>/defaults/`; user configuration overrides them per provider and field; the daemon seeds them into the cache; a client holds none.
14. A value expression written as exactly one `{{ expr }}` evaluates to the expression's natural type; one with literal text or more than one tag evaluates to a string; one with no tags is its literal text.
15. `exec()` capability is surface-scoped: a value expression may spawn a command only where the evaluating surface opted in — `comb get`, `comb eval`, `comb status` and `comb watch`, and an SDK client constructed with `exec` on. It is evaluated client-side, never by the daemon, and is unavailable in formatting surfaces.

## Parameters

This document defines no tunable runtime parameters. Per-field value expressions are **configuration**, governed by the user-facing config documentation; their built-in defaults are managed configuration files written by the daemon (invariant 13).

## Worked examples

### Example 1 — fixed cascade over distinct cached values: `python.version`

```
python.version = {{ env.PYENV_VERSION or env.MISE_PYTHON_VERSION
                 or cache.mise.python or cache.asdf.python or cache.python.venv_version }}
```

There is no cached `python.version`; it is pure selection logic. `cache.python.venv_version` (the venv's version), `cache.mise.python`, and `cache.asdf.python` are distinct cached values with their own names, read at the resolved (cwd) path. First non-empty wins; all-empty ⇒ `""`.

### Example 2 — single-value override: `terraform.workspace`

```
terraform.workspace = {{ env.TF_WORKSPACE or cache.terraform.workspace }}
```

One underlying cached value (the workspace from `.terraform/environment`) with an env override on top. The cached value keeps the name `workspace`; the expression references `cache.terraform.workspace`, so there is no self-reference and no rename.

### Example 3 — enumerable selection: `aws.region`

`aws_profiles` is the data provider (a cached field per profile, each an object); `aws` is the consumer namespace.

```
aws.profile = {{ env.AWS_PROFILE or env.AWS_VAULT or env.AWS_DEFAULT_PROFILE or "default" }}
aws.region  = {{ env.AWS_REGION or env.AWS_DEFAULT_REGION
              or cache.aws_profiles[ env.AWS_PROFILE or env.AWS_VAULT or env.AWS_DEFAULT_PROFILE or "default" ].region }}
```

With `$AWS_PROFILE=staging` the client indexes the cached `aws_profiles` object at `["staging"].region`; unset → `["default"]`. `$AWS_REGION` short-circuits before the index. The daemon caches every profile regardless of the shell's selector.

### Example 4 — addressing an interior node: `comb get aws` vs `comb get aws_profiles`

Both address a provider root — an interior node — and return its subtree as an object. `comb get aws` evaluates the `aws` namespace's virtual fields (`region`, `profile`, `expiration`) — the computed surface. `comb get aws_profiles` returns the raw cached enumeration (every profile object). They differ only in whether the subtree is computed or cached.

## Behaviour assertions

```gherkin
Feature: Field resolution

  Scenario: a single-tag expression keeps the expression's natural type
    Given a value expression "{{ cache.provider.flag }}" where cache.provider.flag is a boolean
    When the field is resolved
    Then the value is a boolean, not a string

  Scenario: a template expression resolves to a string
    Given a value expression "{{ git.branch }}{% if git.dirty %}*{% endif %}"
    When the field is resolved
    Then the value is a string

  Scenario: whitespace around a single tag is not literal text
    Given a value expression " {{ cache.provider.flag }}\n" where cache.provider.flag is a boolean
    When the field is resolved
    Then the value is a boolean, not a string
    And the surrounding whitespace does not appear in the value

  Scenario: a cached field resolves to its cached value by default
    Given a cached field provider.f with no custom value expression
    When provider.f is resolved
    Then the value is the cached cache.provider.f at the resolved path

  Scenario: every reference in an expression is fetched, not only the first
    Given a value expression "cache.a.x or cache.b.y or cache.c.z"
    When it is resolved
    Then all of cache.a.x, cache.b.y, cache.c.z are fetched as needed

  Scenario: overriding a cached value under its own name is not a cycle
    Given the value expression "env.X or cache.provider.f" for field provider.f
    And $X is unset
    When provider.f is resolved
    Then the value is the cached cache.provider.f
    And resolution does not recurse into provider.f

  Scenario: a virtual field selects across distinct cached values
    Given python.version = "cache.mise.python or cache.python.venv_version"
    And cache.mise.python is empty and cache.python.venv_version is "3.12"
    When python.version is resolved
    Then the value is "3.12"

  Scenario: env.* miss yields empty string with no daemon round-trip
    Given the calling shell has no $NOPE
    When env.NOPE is resolved
    Then the value is ""
    And no request is sent to the daemon

  Scenario: env.* is directly queryable
    Given $FOO is "bar"
    When env.FOO is resolved
    Then the value is "bar"

  Scenario: value-phase selection indexes the published set
    Given a data provider caches { default:{v:1}, chosen:{v:2} }
    And the shell selector designates "chosen"
    When the consumer field is resolved
    Then it indexes the cached set by "chosen" and yields 2
    When the selector is unset
    Then it indexes by the default key and yields 1

  Scenario: a direct env value wins over the indexed value
    Given a direct env override is set
    When the consumer field is resolved
    Then the override wins and the data provider is not indexed

  Scenario: a bare consumer namespace evaluates all its virtual fields
    Given a namespace of virtual fields with no cached provider
    When the bare namespace is queried
    Then each virtual field is evaluated and returned as one object

  Scenario: a bare cached provider returns its cached fields
    When a bare cached/data provider is queried
    Then its cached fields are returned as one object

  Scenario: typed output is preserved
    Given a value expression that yields a boolean
    When resolved with --format json
    Then the output is a JSON boolean
    When resolved with --format text or sh
    Then the output is the stringified value

  Scenario: a reference cycle is a config error
    Given two virtual fields whose expressions reference each other as resolved fields
    When either is resolved
    Then resolution returns a config error
    And the process does not panic

  Scenario: a source with no tags is literal text
    Given a value expression "env.CONDA_DEFAULT_ENV" with no tags
    When the field is resolved
    Then the value is the string "env.CONDA_DEFAULT_ENV"

  Scenario: a nested template is addressable at every node
    Given provider scanline has templates "widgets.foo" and "widgets.clock.format"
    When "scanline.widgets.foo" is resolved
    Then the value is that template's rendered result
    When "scanline.widgets" is resolved
    Then the value is an object with keys foo and clock, and clock is an object with key format
    When "scanline" is resolved
    Then the value is an object whose widgets member is that same object

  Scenario: a template at a cached field's path wins on read
    Given the cache holds git.branch = "feature/x"
    And a template exists at git:template.branch
    When "git.branch" is resolved
    Then the value is the template's rendered result
```

## Out of scope

- **Daemon Source refresh model** — how cached values are produced and invalidated (Source structure, invalidation, scope, lifecycle, failure, cache-entry composition): see [`provider_source.md`](./provider_source.md).
- **Cache state machine** — Active / Decay / Evicted: see [`cache-lifecycle.md`](./cache-lifecycle.md).
- **Wire protocol / NDJSON encoding** — request/response shapes are protocol documentation, not this model.
- **User-facing config file syntax** — how `[providers.<name>]` and value-expression keys are written is governed by the user-facing config documentation.
- **SDK behaviour** — SDK API shapes and surfaces are SDK documentation, not this model. The resolution model itself binds every consumer, CLI and SDKs alike.

## See also

- [`provider_source.md`](./provider_source.md) — the daemon Source refresh model that produces cached values and path-scoped cache entries.
- [`cache-lifecycle.md`](./cache-lifecycle.md) — the Active/Decay/Evicted state machine of the cache entries this layer reads.
