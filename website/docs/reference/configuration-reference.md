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
# These vars reach a Source's lines as `env.VAR` (an unset one renders empty) and
# the commands its exec() runs.
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
# [providers.<name>] switches a whole provider off (`enabled`, a boolean). A
# built-in Source's failback is tuned in [providers.<name>.<source>]; the
# source name matches the source field carried per row in `comb status`'s wire
# response. No other key is valid in either place.

# Disable a provider entirely (it will never execute or appear in results)
[providers.sudo]
enabled = false

# Back git's refs source off sooner after failures
[providers.git.refs]
failback_count = 5            # consecutive failures before suppression
failback_interval = "2s"      # suppression duration after failback_count hit


# ─── Config Sources ────────────────────────────────────────────────────────────
# Define your own providers as Sources: `data` runs a command, `result` shapes
# its output into fields, `return` says when it runs again.

# Minimal: a global provider that polls every 30 seconds
[daemon.providers.docker_context.sources.main]
data = "{{ exec('docker context show') | trim }}"
result.value = "{{ data }}"            # one field, `docker_context.value`
return = "{{ result | poll('30s') }}"

# A path-scoped provider that watches files and has a poll fallback
[daemon.providers.node_version.sources.main]
root = "{{ lup('package.json') | dirname }}"   # the directory an instance answers for
data = "{{ exec('node --version') | trim }}"
result.value = "{{ data }}"
return = "{{ result | watch('.node-version', '.nvmrc', 'package.json') | poll('60s') }}"

# A Source with structured JSON output: `result = "{{ data }}"` owns the whole node
[daemon.providers.cargo_meta.sources.main]
root = "{{ lup('Cargo.toml') | dirname }}"
data = "{{ exec('cargo metadata --format-version=1 --no-deps --quiet') | fromjson }}"
result = "{{ data }}"
return = "{{ result | watch('Cargo.toml', 'Cargo.lock') | poll('120s') }}"

# Explicitly disable a custom provider without removing its config
[providers.cargo_meta]
enabled = false


# ─── HTTP ─────────────────────────────────────────────────────────────────────
# Fetch data from REST APIs with url() in a Source's data line — no curl fork,
# no shell spawning. A non-2xx response fails the Source (its failback applies).

# Basic: poll a status API
[daemon.providers.service_status.sources.main]
data = "{{ url('https://status.anthropic.com/api/v2/summary.json') | fromjson }}"
result.indicator = "{{ data.status.indicator }}"   # response.status.indicator → field "indicator"
return = "{{ result | poll('60s') }}"

# With auth headers (the daemon's environment is `env`)
[daemon.providers.github_rate.sources.main]
data = "{{ url('https://api.github.com/rate_limit', headers={'Authorization': 'Bearer ' ~ env.GITHUB_TOKEN}) | fromjson }}"
result = "{{ data.rate }}"                          # { "limit": 5000, "remaining": 4999, ... }
return = "{{ result | poll('30s') }}"

# Infrequent poll (daily)
[daemon.providers.exchange_rate.sources.main]
data = "{{ url('https://api.exchangerate-api.com/v4/latest/USD') | fromjson }}"
result.value = "{{ data.rates.AUD }}"               # a single nested value
return = "{{ result | poll('86400s') }}"
```

### Template facets

A **facet** is a namespace holding data *about* a provider, written `provider:facet`. The `template` facet holds the value expressions a `comb get` on that provider returns alongside the data. Declare one under `[client.providers.<p>]` as `template` — the table maps 1:1 onto the tree, nested tables are groups, and string leaves are the expressions:

```toml
[client.providers.python]
template.venv_name = "{{ python.venv }}"

[client.providers.python.template.widgets]
version = "{{ python.version }}"
```

That is `python:template.venv_name` and `python:template.widgets.version`. A provider exists if either root names it: one named only under `[client.providers]` or `[daemon.providers]` is a provider with no Sources (unless `[providers.<p>] enabled = false` disables it). The rules are checked file by file, because seeding is, and enforced at daemon startup, on reload and by `comb check config`; each error names its file:

- `[client]` holds only `providers`, and `[client.providers.<p>]` holds only `template`; any other key is `unknown key '<k>' in [client]` (or `[client.providers.<p>]`).
- Nesting is tables only, so a quoted key containing a `.` (`"a.b" = "…"`) is an **error**, and so is an empty key. A provider name holds no `:` or `.` and is not `*`.
- A template leaf that is neither a string nor a table is not a value expression: an **error**.
- The colon-table form `[providers."<p>:template"]` is gone: it is `unknown table [providers."<p>:template"]: templates live in [client.providers.<p>] template.<leaf>, decay in [daemon.providers.<p>.cache._]`.

The decay depth is seeded the same way, from the daemon root: `[daemon.providers.git.cache._]` with `decay = 2` seeds `git:cache._.decay`, and `[daemon.providers._.cache._]` seeds `*:cache._.decay` for every provider (the shipped `defaults/00-comb.toml` sets 4). `cache` holds only `_`, `_` holds only `decay`, and `decay` is an integer from 1 to 16 (`comb put <p>:cache._.decay` takes the same range).

Every leaf is loaded into the cache at daemon start, labelled with the file it came from — `comb status` shows its source as `config:~/.config/beachcomber/conf.d/10-mine.toml`. Layers are loaded in composition order, so a leaf two files set carries the later file's label and value, while a leaf only the earlier file set keeps its own.

**Seeded tables reload without a restart.** A configuration change that touches only `[client.providers]` or `[daemon.providers.<p>.cache]` tables is applied in place: every `config:`-labelled record is dropped and all layers are loaded again, so an edited leaf takes effect on the next read, a leaf deleted from its file disappears, and a `watch` on the affected key wakes. Records written by `comb put` are untouched. A change that touches anything else still restarts the daemon.

The reload is all or nothing, twice over. The edited files are checked against the rules above before anything is touched, so an edit that breaks one of them changes nothing at all — the previous values keep being served and the error is logged (and shown by `comb check config`) until the file is fixed. And each namespace's old and new records are swapped together, so a `comb get` running concurrently with the reload never sees a value momentarily missing.

### Templates and value expressions

A **template** is a string leaf of a `template` table: a value expression over `env.*`, `comb.*` and other fields' resolved values. The daemon stores it and serves it; the client — `comb get`, `comb eval`, and every SDK's `resolve`/`eval`, all through `libbeachcomber::resolve` — renders it with the caller's environment and directory. A field need not have a cached value to carry a template:

```toml
[client.providers.scanline]
template.theme = "{{ env.SCANLINE_THEME or 'dark' }}"

[client.providers.scanline.template.widgets]
foo = "{{ env.SCANLINE_FOO }}"
bar = "{{ comb.git.branch | truncate(12) }}"
```

**Addressing.** Every node is addressable at any depth, templated or cached alike: `comb g scanline.widgets.foo` renders one leaf, `comb g scanline.widgets` renders the group as an object, and `comb g scanline` returns the whole namespace — its cached fields deep-merged with its rendered template tree. Where a template sits at the same path as a cached field, the template wins on read (`defaults/terraform.toml` does this to `terraform.workspace`, reading the cached value back through `comb.terraform.workspace`). The raw template text is read through the facet: `comb g scanline:template.widgets.foo` prints `{{ env.SCANLINE_FOO }}`. An address can run past a template into what it renders — with `stats = "{{ {'cpu': 40} }}"`, `comb g scanline.stats.cpu` is `40`.

**Expressions.** One rule, everywhere a template is evaluated:

| Written as | Evaluates to | Example |
|---|---|---|
| exactly one `{{ expr }}` tag | the expression's natural type | `"{{ env.OP_SERVICE_ACCOUNT_TOKEN != '' }}"` → a bool |
| literal text and/or more than one tag | a string | `"{{ git.branch }}{% if git.dirty %}*{% endif %}"` → `main*` |
| no tags at all | its own literal text | `"env.HOME"` → the string `env.HOME` |

Inside an expression, `comb.<provider>.<field>` is the raw cached value; `<provider>.<field>` is that field as `comb get` would resolve it (templates included, recursively, cycle-detected); `env.X` is the caller's environment. A missing ref is undefined and falsy. Filters: minijinja's built-ins plus `truncate`, `basename`, `fg`, `bg`, `bold`, `dim` and `fromjson`.

**Errors.** A template that does not compile, or a cycle, is a configuration error: a single-leaf query fails, and a namespace query fails naming the leaf (`comb g scanline` exits 2 with `scanline.widgets.foo: …`). A runtime failure — a failed or disabled `exec()` — fails a single-leaf query but is omitted from a namespace object.

**Writing a template at runtime.** `put` addresses the facet like any other node: `comb p scanline:template.widgets.foo '"{{ env.OTHER }}"'` takes effect on the next read and lasts until the daemon restarts; it may also remove it with `null`, and `--global` writes `$HOME`. A `put` layers over a Source or `config:` value at the same directory — it replaces the node there and hides what is beneath it — and removing the put shows them again. Flags and semantics are under `comb p` in [CLI Commands](/docs/reference/cli-commands).

**Managed defaults.** The shipped templates — `aws`, `conda`, `gcloud`, `op`, `python`, `terraform`, `kubecontext`, `talos` — are files the daemon writes to `~/.config/beachcomber/defaults/` before it loads configuration, each headed by `# Managed by comb <version>` and rewritten on a version change (a shipped name with this version's marker is left untouched even if edited). `defaults/` is the lowest composition layer — `defaults/*.toml`, then `config.toml`, then `conf.d/*.toml` — so override one field by setting the same key in your own file and leaving the rest to the default:

```toml
# ~/.config/beachcomber/conf.d/10-mine.toml
[client.providers.python]
template.version = "{{ env.MY_PY or comb.python.venv_version }}"   # venv_name still comes from defaults/python.toml
```

`comb check config` reports the directory, its file count and version, and warns about files in it that `comb` did not write. A directory the daemon cannot write is a refusal to start.

**The 0.9 form is an error.** `[providers.<name>.virtual]` — the 0.9 home of a provider's templates, as a table or as dotted keys under `[providers.<name>]` — is reported by daemon start and `comb check config` as `unrecognised block [providers.<name>.virtual]: templates live in [client.providers.<name>] template.<leaf>`.

**A `path` key directly under `[providers.<name>]` is also an error.** Path expressions are gone; a query addresses the caller's directory, `--path`, or `--global`. Daemon start and `comb check config` report it as `[providers.<name>]: path is not a provider setting: a query addresses the caller's directory, --path, or --global`.

### `exec()` in templates

A template can call `exec()` to run a command and use its output as the field's value — `defaults/kubecontext.toml` renders `kubecontext.context` this way:

```toml
[client.providers.myapp]
template.deploy_tag = "{{ exec('git describe --tags --always') }}"
```

| | |
|---|---|
| Signature | `exec(cmd, timeout_ms=10000, stdin=none, sh='sh')`, or the filter `cmd \| exec()` |
| Spawn | a **string** runs under `sh -c` (`sh=` names another shell); a **list** is argv, no shell — each element exactly one argument, `'$FOO'` stays literal. |
| Environment | the child's environment is exactly the template's `env` map (`comb`'s environment for the CLI, `env_json` for an SDK), so `$FOO` and `env.FOO` agree; its working directory is the render's. |
| List element coercion | string → itself; bool → `"true"`/`"false"`; number → its canonical string form; `undefined`/`none`, a sequence, or a map → error (structured data goes through `stdin=` instead) |
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
  an edit confined to the [seeded tables](#template-facets) themselves (`[client.providers]` and `[daemon.providers.<p>.cache]`), which is
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
| `providers.<p>.cache._.decay` | int, 1 to 16 | (4 from `defaults/00-comb.toml`) | Decay steps before eviction, seeded as `<p>:cache._.decay`; `<p> = _` is every provider ([Template facets](#template-facets)) |

**`[client]` section:**

| Field | Type | Default | Description |
|---|---|---|---|
| `providers.<p>.template.<leaf>` | string, or a table of them | none | A template (value expression), seeded as `<p>:template.<leaf>`; nest with tables ([Template facets](#template-facets)) |

**`[lifecycle]` section:**

| Field | Type | Default | Description |
|---|---|---|---|
| `idle_shutdown_secs` | int or null | `null` (disabled) | Seconds until idle daemon shuts down |

> A Source's schedule is its `return` line (`poll`, `watch`, `ttl`); `[lifecycle] poll_interval`, `poll_live_count` and `fsevents_reinstate` are config errors.

> Duration strings use whole-second values: `"30s"` (seconds), `"2m"` (minutes), `"1h"` (hours), `"2h30m"` (compound). Sub-second values (e.g. `"500ms"`) are not accepted.

**`[failback]` section:**

Global failure-suppression defaults. These apply to all providers and sources unless overridden at the per-source level with `failback_count`/`failback_interval`.

| Field | Type | Default | Description |
|---|---|---|---|
| `count` | int | `3` | Consecutive failures before suppression (provider stops executing until interval elapses) |
| `interval` | duration | `"1s"` | Suppression duration after `count` failures |

> `[lifecycle] failure_reattempts` and `[lifecycle] failure_backoff_interval` are deprecated aliases for `[failback] count` and `[failback] interval`. They still parse but emit a `WARN` log at startup.

**`[providers.<name>]` section (built-in overrides):**

Only `enabled` (a boolean) is valid directly under `[providers.<name>]` for built-in providers. A built-in Source's failback goes in a per-source sub-table: `[providers.<name>.<source>]`.

| Field | Type | Default | Description |
|---|---|---|---|
| `enabled` | bool | `true` | Set `false` to disable provider entirely |

**`[providers.<name>.<source>]` section (built-in source overrides):**

Only the failback settings apply to a built-in Source; any other key is an error.

| Field | Type | Default | Description |
|---|---|---|---|
| `failback_count` | int | the Source's own | Consecutive failures before suppression |
| `failback_interval` | duration | the Source's own | Suppression duration after failback_count hit |

**`[daemon.providers.<name>.sources.<source>]` section (config Sources):**

| Field | Type | Required | Description |
|---|---|---|---|
| `data` | string | yes | The body: a jinja line that runs on the daemon, usually `exec('command')`. Sees `env` (the daemon's own), `comb.<provider>` and `_`. |
| `result` | string, or table of strings | yes | `result = "{{ data }}"` owns the whole provider node; `result.<field> = "{{ … }}"` lines declare fields, each over `data`. |
| `root` | string | no | The directory an instance answers for, e.g. `{{ lup('.git') \| dirname }}`. Empty (the default) is the one instance at `~`. |
| `return` | string | no | What the Source hands out, decorated: `poll(interval, count)`, `watch(path, …)`, `ttl(interval)`, `failback(count, interval)`, `budget(interval)`. Absent, it takes the nearest `_` Source's (`defaults/00-comb.toml` ships `poll('60s', 12) \| failback(3, '1s')`). `\| super` extends that line instead of replacing it. |

A table named `_` (`[daemon.providers.<name>.sources._]`, `[daemon.providers._.sources._]`) is a prototype: the defaults for every Source below it, never a Source itself. Prototypes apply to config Sources only: the Rust Sources of built-in providers keep their own schedules until they ship as config.

**`[daemon.plugin]` section:**

| Field | Type | Default | Description |
|---|---|---|---|
| `path` | array of strings | `["<config dir>/plugins"]` | Directories `plugin(name, symbol, args...)` searches, in order, for `lib<name>.dylib` (macOS) or `lib<name>.so` (Linux). Each is absolute or starts `~/`. A later file's list replaces an earlier one's. |
