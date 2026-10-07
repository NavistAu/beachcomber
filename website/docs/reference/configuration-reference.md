---
sidebar_position: 3
title: Configuration Reference
---

# Configuration Reference

beachcomber runs with sensible defaults and requires no configuration. The optional config file lives at `~/.config/beachcomber/config.toml`.

```toml
# ~/.config/beachcomber/config.toml

# ─── Daemon ────────────────────────────────────────────────────────────────────

[daemon]

# Override the Unix socket path.
# Default resolution order: BEACHCOMBER_SOCKET env var →
#          /tmp/beachcomber-<uid>/sock
# socket_path = "/path/to/socket.sock"

# Log level for daemon output.
# Options: "error", "warn", "info", "debug", "trace"
# Default: "info"
# Logs go to: $XDG_STATE_HOME/beachcomber/daemon.log
log_level = "info"

# Maximum time (in seconds) to wait for any provider to complete.
# Providers that exceed this are cancelled; the last good cached value is retained.
# Default: 10
provider_timeout_secs = 10

# Path to an environment file loaded at daemon startup.
# Each line is KEY=VALUE (or KEY="VALUE"). Blank lines and #comments are ignored.
# These vars are available to ${VAR} expansion in HTTP headers, script commands, etc.
# Default: ~/.config/beachcomber/env (loaded automatically if present)
# env_file = "~/.config/beachcomber/env"

# How often the watchdog checks the scheduler heartbeat.
# If the heartbeat hasn't advanced within the threshold, the daemon shuts down
# for the process supervisor (launchd, systemd) to restart.
# Default: disabled (no watchdog)
# watchdog_interval = "30s"

# How long the heartbeat can be stale before the watchdog triggers shutdown.
# Default: 3x watchdog_interval
# watchdog_threshold = "90s"


# ─── Lifecycle ─────────────────────────────────────────────────────────────────

[lifecycle]

# Base polling interval used while an entry is in Active state (P in the
# cache lifecycle model). See docs/cache-lifecycle.md.
# Default: "60s"
poll_interval = "60s"

# Keep-alive count in polls (K in the cache lifecycle model). An Active
# entry stays warm for K × poll_interval seconds after the last demand
# signal, then enters the exponential decay ladder (Decay1..4, each step
# doubling interval and duration) until eviction.
# Default: 12
poll_live_count = 12

# Whether an fsevent during decay reinstates the entry to Active.
# When set, overrides all per-source defaults. When unset (default),
# each source uses its own declared default (true for Watch/WatchAndPoll
# sources that benefit from it, false for Poll-only sources).
# Default: unset (each source uses its own default)
# fsevents_reinstate = true

# How long (in seconds) the daemon waits with no active connections before
# shutting itself down. The next client connection will socket-activate a
# fresh instance.
# Set to null to disable idle shutdown (daemon stays resident permanently).
# Default: null (disabled — daemon stays resident)
# idle_shutdown_secs = 300


# ─── Failback (failure suppression) ────────────────────────────────────────────

[failback]

# Consecutive failures before suppression (provider stops executing).
# Default: 3
# count = 3

# Duration of suppression after count failures.
# Default: "1s"
# interval = "1s"


# ─── Built-in Provider Overrides ───────────────────────────────────────────────
# Provider config uses per-source nesting: [providers.<name>.<source>]
# The source name matches the source field carried per row in `comb status`'s
# wire response. Use [providers.<name>] only for whole-provider switches (e.g. enabled).

# Key reference for per-source blocks:
#
# type              string    Strategy: "poll", "fsevent", "fsevent_poll"
# scope             string    "path" or "global" (informational for built-ins)
# poll_interval     duration  Poll interval (poll, fsevent_poll)
# poll_count        integer   Keep-alive polls before decay (poll, fsevent_poll)
# fsevent_patterns  [string]  Relative path components to watch (fsevent, fsevent_poll)
# fsevent_abs_paths [string]  Absolute roots to watch (fsevent, fsevent_poll)
# fsevent_lifespan  duration  Keep-alive duration before decay (fsevent, path-scoped)
# fsevent_reinstates bool     Whether watches survive decay. Default true.
# failback_count    integer   Consecutive failures before suppression
# failback_interval duration  Suppression duration after failback_count hit
# enabled           bool      When false, source never executes. Default true.

# Disable a provider entirely (it will never execute or appear in results)
[providers.sudo]
enabled = false

# Override the git.diff source to poll more frequently
[providers.git.diff]
poll_interval = "15s"         # default: "30s"
poll_count = 24               # keep-alive = 24 × 15s = 6 min

# Make git.refs reinstate faster from decay
[providers.git.refs]
fsevent_reinstates = true     # .git fsevent during decay reinstates to Active
fsevent_lifespan = "600s"     # stay warm for 10 min after last query

# Slow down battery polling (less important on desktop)
[providers.battery.state]
poll_interval = "60s"         # default: "30s"
poll_count = 4

# Override network polling interval
[providers.network.interfaces]
poll_interval = "30s"         # default: "10s"


# ─── Custom Script Providers ───────────────────────────────────────────────────
# Define your own providers backed by any executable.

# Minimal: a global provider that polls every 30 seconds
[providers.docker_context]
command = "docker context show"
output = "text"         # single-line output becomes { "value": "<output>" }
# or use output = "json" for structured output: { "key": value, ... }
# or use output = "kv" for key=value line format

[providers.docker_context.invalidation]
poll = "30s"

# A path-scoped provider that watches a file and has a poll fallback
[providers.node_version]
command = "node --version"
output = "text"
scope = "path"          # scoped to a directory; path argument required

[providers.node_version.invalidation]
watch = [".node-version", ".nvmrc", "package.json"]
poll = "60s"            # safety-net poll in case filesystem events are missed

# A provider with structured JSON output
[providers.cargo_meta]
command = "cargo metadata --format-version=1 --no-deps --quiet"
output = "json"         # parse stdout as JSON object; top-level keys become fields
scope = "path"

[providers.cargo_meta.invalidation]
watch = ["Cargo.toml", "Cargo.lock"]
poll = "120s"

# Explicitly disable a custom provider without removing its config
[providers.my_slow_thing]
command = "my-slow-script"
enabled = false


# ─── HTTP Providers ──────────────────────────────────────────────────────────
# Fetch data directly from REST APIs — no curl fork, no shell spawning.
# Uses in-process HTTP client with connection reuse.

# Basic: poll a status API
[providers.service_status]
type = "http"
url = "https://status.anthropic.com/api/v2/summary.json"
extract = "status"              # dot-path into the JSON response
                                # e.g., response.status.indicator → provider field "indicator"

[providers.service_status.invalidation]
poll = "60s"

# With auth headers (env vars expanded at runtime)
[providers.github_rate]
type = "http"
url = "https://api.github.com/rate_limit"
headers = { Authorization = "Bearer ${GITHUB_TOKEN}" }
extract = "rate"                # extracts { "limit": 5000, "remaining": 4999, ... }

[providers.github_rate.invalidation]
poll = "30s"

# Infrequent poll (daily)
[providers.exchange_rate]
type = "http"
url = "https://api.exchangerate-api.com/v4/latest/USD"
extract = "rates.AUD"           # extracts a single nested value

[providers.exchange_rate.invalidation]
poll = "86400s"
```

### Per-field scope

Script and library providers can declare scope per field. The provider-level
`scope` acts as the default; individual fields can override.

```toml
[providers.mixed]
type = "script"
command = "/usr/bin/myscript --json"
scope = "path"

[providers.mixed.fields.branch]
type = "string"
# inherits provider-level "path"

[providers.mixed.fields.status]
type = "string"
scope = "global"   # override — always cached globally
```

Back-compat: `fields = { branch = "string" }` continues to parse. All fields
inherit the provider-level `scope`.

Library providers (.so / .dylib) use the same per-field scope model in their
JSON metadata:

```json
{
  "fields": {
    "branch": {"type": "string", "scope": "path"},
    "system": {"type": "string", "scope": "global"}
  }
}
```

### Template facets

A **facet** is a namespace holding data *about* a provider, written `provider:facet`. The `template` facet holds the value expressions a `comb get` on that provider returns alongside the data. Declare one as a quoted provider key — the table maps 1:1 onto the tree, nested tables are groups, and string leaves are the expressions:

```toml
[providers."python:template"]
venv_name = "{{ python.venv }}"

[providers."python:template".widgets]
version = "{{ python.version }}"
```

That is `python:template.venv_name` and `python:template.widgets.version`. A facet table holds data only, and four rules are enforced at daemon startup (and by `comb check config`):

- The key must be valid facet syntax — exactly one colon, a facet name of letters, digits, `_` or `-`, and not one of the metadata suffix names (`age`, `stale`, `fresh`, `cache`, `source`). `[providers."a:b:c"]` and `[providers."x:age"]` are **errors**.
- No settings keys. `enabled`, `backend`, `type`, `command`, `library_path` and `url` are **errors** at the top level of a facet table, and so is any per-source knob — `poll_interval`, `poll_count`, `poll_secs`, `fsevent_patterns`, `fsevent_abs_paths`, `fsevent_lifespan`, `fsevent_reinstates`, `failback_count`, `failback_interval`, and the legacy spellings `poll_interval_secs`, `poll_live_count`, `fsevents_reinstate`, `failure_reattempts` and `failure_backoff_interval`. They belong in `[providers.<name>]`.
- Nesting is tables only, in **every** facet, so a quoted key containing a `.` (`"a.b" = "…"`) is an **error**.
- Under `:template` a leaf that is neither a string nor a table is not a value expression: a **warning**, and the leaf is dropped from what gets loaded. Any other facet is plain data, so its leaves may be of any type.

Every leaf is loaded into the cache at daemon start, labelled with the file it came from — `comb status` shows its source as `config:~/.config/beachcomber/conf.d/10-mine.toml`. Layers are loaded in composition order, so a leaf two files set carries the later file's label and value, while a leaf only the earlier file set keeps its own.

**Facet tables reload without a restart.** A configuration change that touches only facet tables is applied in place: every `config:`-labelled record is dropped and all layers are loaded again, so an edited leaf takes effect on the next read, a leaf deleted from its file disappears, and a `watch` on the affected key wakes. Records written by `comb put` are untouched. A change that touches anything else still restarts the daemon.

The reload is all or nothing, twice over. The edited files are checked against the four rules above before anything is touched, so an edit that breaks one of them changes nothing at all — the previous values keep being served and the error is logged (and shown by `comb check config`) until the file is fixed. And each namespace's old and new records are swapped together, so a `comb get` running concurrently with the reload never sees a value momentarily missing.

### Templates and value expressions

A **template** is a string leaf of a `:template` facet: a value expression over `env.*`, `cache.*` and other fields' resolved values. The daemon stores it and serves it; the client — `comb get`, `comb eval`, and every SDK's `resolve`/`eval`, all through `libbeachcomber::resolve` — renders it with the caller's environment and directory. A field need not have a cached value to carry a template:

```toml
[providers."scanline:template"]
theme = "{{ env.SCANLINE_THEME or 'dark' }}"

[providers."scanline:template".widgets]
foo = "{{ env.SCANLINE_FOO }}"
bar = "{{ cache.git.branch | truncate(12) }}"
```

**Addressing.** Every node is addressable at any depth, templated or cached alike: `comb g scanline.widgets.foo` renders one leaf, `comb g scanline.widgets` renders the group as an object, and `comb g scanline` returns the whole namespace — its cached fields deep-merged with its rendered template tree. Where a template sits at the same path as a cached field, the template wins on read (`defaults/terraform.toml` does this to `terraform.workspace`, reading the cached value back through `cache.terraform.workspace`). The raw template text is read through the facet: `comb g scanline:template.widgets.foo` prints `{{ env.SCANLINE_FOO }}`. An address can run past a template into what it renders — with `stats = "{{ {'cpu': 40} }}"`, `comb g scanline.stats.cpu` is `40`.

**Expressions.** One rule, everywhere a template is evaluated:

| Written as | Evaluates to | Example |
|---|---|---|
| exactly one `{{ expr }}` tag | the expression's natural type | `"{{ env.OP_SERVICE_ACCOUNT_TOKEN != '' }}"` → a bool |
| literal text and/or more than one tag | a string | `"{{ git.branch }}{% if git.dirty %}*{% endif %}"` → `main*` |
| no tags at all | its own literal text | `"env.HOME"` → the string `env.HOME` |

Inside an expression, `cache.<provider>.<field>` is the raw cached value; `<provider>.<field>` is that field as `comb get` would resolve it (templates included, recursively, cycle-detected); `env.X` is the caller's environment. A missing ref is undefined and falsy. Filters: minijinja's built-ins plus `truncate`, `basename`, `fg`, `bg`, `bold`, `dim` and `fromjson`.

**Errors.** A template that does not compile, or a cycle, is a configuration error: a single-leaf query fails, and a namespace query fails naming the leaf (`comb g scanline` exits 2 with `scanline.widgets.foo: …`). A runtime failure — a failed or disabled `exec()` — fails a single-leaf query but is omitted from a namespace object.

**Writing a template at runtime.** `put` addresses the facet like any other node: `comb p scanline:template.widgets.foo '"{{ env.OTHER }}"'` takes effect on the next read and lasts until the daemon restarts; it may also `--null` it, and `--global` writes `$HOME`. A `put` layers over a Source or `config:` value at the same directory — it replaces the node there and hides what is beneath it — and removing the put shows them again. Flags and semantics are under `comb p` in [CLI Commands](/docs/reference/cli-commands).

**Managed defaults.** The shipped templates — `aws`, `conda`, `gcloud`, `op`, `python`, `terraform`, `kubecontext`, `talos` — are files the daemon writes to `~/.config/beachcomber/defaults/` before it loads configuration, each headed by `# Managed by comb <version>` and rewritten on a version change (a shipped name with this version's marker is left untouched even if edited). `defaults/` is the lowest composition layer — `defaults/*.toml`, then `config.toml`, then `conf.d/*.toml` — so override one field by setting the same key in your own file and leaving the rest to the default:

```toml
# ~/.config/beachcomber/conf.d/10-mine.toml
[providers."python:template"]
version = "{{ env.MY_PY or cache.python.venv_version }}"   # venv_name still comes from defaults/python.toml
```

`comb check config` reports the directory, its file count and version, and warns about files in it that `comb` did not write. A directory the daemon cannot write is a refusal to start.

**The 0.9 form is an error.** `[providers.<name>.virtual]` — the 0.9 home of a provider's templates, as a table or as dotted keys under `[providers.<name>]` — is reported by daemon start and `comb check config` as `unrecognised block [providers.<name>.virtual]: templates live in [providers."<name>:template"]`.

**A `path` key directly under `[providers.<name>]` is also an error.** Path expressions are gone; a query addresses the caller's directory, `--path`, or `--global`. Daemon start and `comb check config` report it as `[providers.<name>]: path is not a provider setting: a query addresses the caller's directory, --path, or --global`.

### `exec()` in templates

A template can call `exec()` to run a command and use its output as the field's value — `defaults/kubecontext.toml` renders `kubecontext.context` this way:

```toml
[providers."myapp:template"]
deploy_tag = "{{ exec('git', 'describe', '--tags', '--always') }}"
```

| | |
|---|---|
| Signature | `exec(cmd, arg1, arg2, ..., timeout_ms=10000, stdin=none)` |
| Spawn | argv only — no shell. `cmd` and each positional become one argv entry; no interpolation, no `\|`/`&&`/redirection. |
| Argument coercion | string → itself; bool → `"true"`/`"false"`; number → its canonical string form; `undefined`/`none`, a sequence, or a map → error (structured data goes through `stdin=` instead) |
| `timeout_ms` | keyword, default `10000` |
| `stdin` | keyword, default none; must be a string — `\| tojson` first for structured data |
| Output | stdout, lossy-UTF-8, at most one trailing newline stripped; capped at 1 MiB (an oversized stdout is a failure, not a truncation) |
| Failure | non-zero exit / timeout / oversized stdout / spawn failure → a runtime error naming the command, with truncated stderr attached |
| Availability | `comb get`, `comb eval`, and an SDK client constructed with `exec` on (`Client(exec=True)`, `new Client({ exec: true })`, `beachcomber.WithExec(true)`, `Beachcomber::Client.new(exec: true)`, `comb.connect{ exec = true }`, `bc_client_new("{\"exec\":true}")`) — off by default. Never the daemon, and never a formatting surface (`-f fmt`, `watch -f fmt`, the status formatter). A call anywhere it is off fails with `exec() is disabled in this context`. |

A whole-namespace `comb get <provider>` renders every template in that namespace in sequence, so several `exec()`-backed leaves under one namespace can serialise up to N × `timeout_ms` in the worst case — pass an explicit, low `timeout_ms` when the consumer is latency-sensitive (a shell prompt or statusline).

See [CLI Commands](/docs/reference/cli-commands) for the expression-level reference and more examples.

### Composition / conf.d

Config can be split across multiple files using the `conf.d` convention —
no `include =` directive needed. Any `*.toml` file placed in
`~/.config/beachcomber/conf.d/` is composed onto `config.toml` automatically:

```
~/.config/beachcomber/
├── defaults/            # written by the daemon — the shipped templates
│   ├── aws.toml
│   └── …
├── config.toml
└── conf.d/
    ├── 10-scanline.toml
    └── 20-overrides.toml
```

Composition rules:

- The managed `defaults/*.toml` files (see [Templates](#templates-and-value-expressions))
  are the lowest layer, in lexical order; `config.toml` and then `conf.d/`
  compose on top, so a key you set anywhere else overrides a default.
- Files in `conf.d/` are merged in **lexical filename order** (a numeric
  prefix like `10-`, `20-` is a common way to control that order).
- Each file is **deep-merged** onto the accumulated config: tables merge key
  by key (a drop-in overriding one key of `[providers.x]` doesn't clobber the
  rest of that block), while scalars and arrays are **last-wins** (the last
  file to set a given key or array wins outright — arrays are replaced, not
  concatenated).
- `config.toml` itself is optional; `conf.d/*.toml` files compose fine with
  no main file present.
- A `conf.d` file that fails to parse is **skipped** (with a warning naming
  the file) rather than blocking daemon startup — the rest of the composed
  set still loads.
- Config changes take effect via **automatic daemon restart** — no manual
  restart. Changes to `config.toml`, any `conf.d/*.toml` or any
  `defaults/*.toml` file (including files added or removed) trigger the
  daemon's config self-watch, which acts
  only once the *whole* composed set parses cleanly; an edit that doesn't parse
  is ignored with a logged warning. The one change that does **not** restart is
  an edit confined to the [facet tables](#template-facets) themselves, which is
  applied in place.

## Config field summary

**`[daemon]` section:**

| Field | Type | Default | Description |
|---|---|---|---|
| `socket_path` | string | (see resolution order) | Unix socket path. Resolution order: this key → `BEACHCOMBER_SOCKET` env var → `/tmp/beachcomber-<uid>/sock` |
| `log_level` | string | `"info"` | Tracing log level |
| `provider_timeout_secs` | int | `10` | Max seconds for any provider to run |
| `env_file` | string | `~/.config/beachcomber/env` | Path to env file loaded at startup |
| `watchdog_interval` | duration or null | `null` (disabled) | How often the watchdog checks scheduler liveness |
| `watchdog_threshold` | duration or null | 3x `watchdog_interval` | Stale heartbeat duration before triggering shutdown |

**`[lifecycle]` section:**

| Field | Type | Default | Description |
|---|---|---|---|
| `poll_interval` | duration | `"60s"` | Base polling interval `P` used in Active state |
| `poll_live_count` | int | `12` | Keep-alive count `K` in polls (see `docs/cache-lifecycle.md`) |
| `fsevents_reinstate` | bool or null | `null` (unset — per-source defaults apply) | Global override for fsevent-during-decay policy; unset means each source uses its own default |
| `idle_shutdown_secs` | int or null | `null` (disabled) | Seconds until idle daemon shuts down |

> `cache_lifespan`, `poll_idle_interval`, and `poll_live_interval` have been removed. Cache warmth is now `poll_interval × poll_live_count`; the decay ladder plus `fsevents_reinstate` replaces the single idle rate. Legacy configs parse cleanly — the daemon emits a `WARN` log at startup for each deprecated key.

> Duration strings use whole-second values: `"30s"` (seconds), `"2m"` (minutes), `"1h"` (hours), `"2h30m"` (compound). Sub-second values (e.g. `"500ms"`) are not accepted.

**`[failback]` section:**

Global failure-suppression defaults. These apply to all providers and sources unless overridden at the per-source level with `failback_count`/`failback_interval`.

| Field | Type | Default | Description |
|---|---|---|---|
| `count` | int | `3` | Consecutive failures before suppression (provider stops executing until interval elapses) |
| `interval` | duration | `"1s"` | Suppression duration after `count` failures |

> `[lifecycle] failure_reattempts` and `[lifecycle] failure_backoff_interval` are deprecated aliases for `[failback] count` and `[failback] interval`. They still parse but emit a `WARN` log at startup.

**`[providers.<name>]` section (built-in overrides):**

Only `enabled` is valid directly under `[providers.<name>]` for built-in providers. Source knobs belong in a per-source sub-table: `[providers.<name>.<source>]`.

| Field | Type | Default | Description |
|---|---|---|---|
| `enabled` | bool | `true` | Set `false` to disable provider entirely |

**`[providers.<name>.<source>]` section (built-in source overrides):**

| Field | Type | Default | Description |
|---|---|---|---|
| `poll_interval` | duration | inherited from `[lifecycle]` | Override base poll rate `P` |
| `poll_count` | int | inherited from `[lifecycle]` | Override keep-alive count `K` |
| `fsevent_reinstates` | bool | per-source default | Override fsevent-during-decay policy |
| `fsevent_patterns` | array of strings | per-source default | Relative path components to watch |
| `fsevent_abs_paths` | array of strings | per-source default | Absolute roots to watch |
| `fsevent_lifespan` | duration | per-source default | Keep-alive duration before decay (path-scoped fsevent sources) |
| `failback_count` | int | inherited from `[failback]` | Consecutive failures before suppression |
| `failback_interval` | duration | inherited from `[failback]` | Suppression duration after failback_count hit |

**`[providers.<name>]` section (custom script providers):**

| Field | Type | Required | Description |
|---|---|---|---|
| `command` | string | yes | Shell command to execute |
| `output` | string | no | `"json"` (default), `"kv"`, or `"text"` |
| `scope` | string | no | `"global"` (default) or `"path"` |
| `enabled` | bool | no | `false` to disable |
| `invalidation.poll` | string | no | Poll interval as duration string (`"30s"`, `"2m"`) |
| `invalidation.watch` | array of strings | no | File/directory patterns to watch |

**`[providers.<name>]` section (HTTP providers):**

| Field | Type | Required | Description |
|---|---|---|---|
| `type` | string | yes | Must be `"http"` |
| `url` | string | yes | URL to fetch. Supports `${ENV_VAR}` expansion. |
| `method` | string | no | HTTP method: `"GET"` (default), `"POST"`, `"PUT"` |
| `headers` | table | no | HTTP headers. Values support `${ENV_VAR}` expansion. |
| `body` | string | no | Request body (for POST/PUT) |
| `extract` | string | no | Dot-separated path into the JSON response (e.g., `"status.indicator"`, `"rates.AUD"`) |
| `enabled` | bool | no | `false` to disable |
| `invalidation.poll` | string | no | Poll interval (default `"60s"`) |

**`[providers.<name>]` section (shared library providers):**

| Field | Type | Required | Description |
|---|---|---|---|
| `type` | string | yes | Must be `"library"` |
| `library_path` | string | yes | Path to `.so`/`.dylib` file. Supports `~/` expansion. |
| `scope` | string | no | `"global"` (default) or `"path"` — overrides library metadata |
| `fields` | table | no | Field name to type mapping — overrides library metadata |
| `enabled` | bool | no | `false` to disable |
| `invalidation.poll` | string | no | Poll interval — overrides library metadata |
| `invalidation.watch` | array of strings | no | Watch patterns — overrides library metadata |
