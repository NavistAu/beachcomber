---
sidebar_position: 1
title: CLI Commands
---

# CLI Commands

All commands are subcommands of `comb`. The daemon is socket-activated — you never need to start it manually.

Every command has a single-letter shorthand. Long forms (`get`, `status`, etc.) also work.

**Default output is plain text** — `comb g git.branch .` prints just the branch name. Format suffixes on the command replace the `-f` flag when you need something else:

| Suffix | Format | Use when |
|--------|--------|----------|
| _(none)_ | **text** (default) | the common case — single values, shell substitution |
| `.p` | plain text (explicit) | when text isn't the default for the command |
| `.j` | json | programmatic consumers, want the `age_ms`/`stale` envelope |
| `.s` | sh (key=value lines) | sourceable output for `eval`/`source` |
| `.c` / `.C` | csv / csv+header | spreadsheet-style field dumps |
| `.t` / `.T` | tsv / tsv+header | tab-separated for `awk`/`cut` |
| `.f` | template | minijinja template — `{{ field }}` placeholders |

| Short | Long | Accepts format suffixes? |
|-------|------|--------------------------|
| `comb g` | `comb get` | all of them |
| `comb s` | `comb status` | — |
| `comb p` | `comb put` | — |
| `comb w` | `comb watch` | `.p` `.j` `.s` |
| `comb e` | `comb eval` | — |
| `comb i` | `comb init` | — |
| `comb c` | `comb check` | — |
| `comb d` | `comb daemon` | — |
| `comb k` | `comb kill` | — |

## `comb g` (get) `<key>... [path] [--path <path>] [--global] [-f format]`

Query a cached value. Returns cached data immediately. On a cold cache (first query for a key), executes the provider inline and blocks briefly while it runs — subsequent queries return the cached value with no delay.

**Flags:** `--force` (trigger immediate recomputation before returning), `--wait` (block until a fresh value arrives), `--path <dir>` (query a directory other than the current one), `--global` (query the global entry, `$HOME`). Multiple keys can be passed in a single connection (variadic).

**Path:** with no `--path` and no trailing path positional, `get` queries the current working directory. A read at a directory resolves data stored by `put` (and by config) up the directory chain to `$HOME`, the nearest write winning; a provider's own data stays where its source scope puts it. `--global` reads `$HOME` alone and cannot be combined with a path.

**Key addressing forms:**

| Form | What it selects |
|---|---|
| `provider.field` | Single field — most common form |
| `provider` | All fields from all sources (flattened) |

The `provider.field` form automatically routes to the source that owns the field — a source name is metadata on the record, never a key you address directly. Unlike a field, a source has no address at all.

```sh
# Query a specific field — text is the default
comb g git.branch .              # → main
comb get git.branch .            # equivalent long form

# Query a field from a global provider (no path needed)
comb g battery.percent           # → 87
comb g hostname.short            # → myhost

# Query the entire provider (all fields from all sources) — alternate formats
comb g git .                     # → values only, one per line (text default)
comb g.j git .                   # → full JSON envelope with age_ms, stale
comb g.s git .                   # → key=value pairs, sourceable
comb g.c git .                   # → comma-separated values
comb g.C git .                   # → CSV with header row
comb g.t git .                   # → tab-separated values
comb g.T git .                   # → TSV with header row

# Template formatting
comb g.f '{{ branch }} ({{ dirty }})' git .      # → main (false)

# Shell-variable output (key=value lines, sourceable)
comb g.s git .
# ahead=0
# behind=0
# branch=main
# dirty=false

# Multiple keys in one connection (variadic)
comb g git.branch git.dirty battery.percent .

# Force immediate recomputation before returning
comb g --force git .

# Block until a fresh value arrives (useful after a trigger)
comb g --wait git.branch .

# Field metadata — append a colon suffix to retrieve metadata instead of the value
comb g git.branch:age            # → 1523 (age in ms)
comb g git.branch:stale          # → false
comb g git.branch:fresh          # → true (inverse of :stale)
comb g git.branch:cache          # → true (served from cache vs freshly computed)
comb g git.branch:source         # → head (the Source that wrote the record)
```

**Metadata suffixes:**

| Suffix | Type | Description |
|--------|------|-------------|
| `:age` | int | Milliseconds since the value was last computed |
| `:stale` | bool | Whether the value is past its expected refresh time |
| `:fresh` | bool | Inverse of `:stale` — true when the value is within its refresh window |
| `:cache` | bool | Whether the value was served from cache (`true`) or freshly computed (`false`) |
| `:source` | string | The record's writer: a Source of the provider (`head`, `refs`, …) or a static label (`put`, `config:<file>`). `null` for an object — a whole namespace, or a group of several records — and for a miss |

**Exit codes:**
- `0` — success, data returned
- `1` — provider returned no data (e.g. `git` queried outside a git repository)
- `2` — error (daemon unreachable, unknown provider, invalid key)

## `comb s` (status)

Show all warm cache entries as a table — one row per provider field. The `TTL` column encodes each entry's lifecycle position (`★` active, `3`/`2`/`1`/`0` decay countdown), effective poll interval, keep-alive count, and whether filesystem events will reinstate the entry to Active (`◉`).

For a live view, ask for colour explicitly — under `watch` and `viddy` stdout is a pipe, so `--color=auto` (the default) resolves to no colour:

```sh
watch -c 'comb status --color=always'
viddy 'comb status --color=always'
```

`watch` needs its own `-c` to interpret the colour it is handed; without it the escape sequences render as literal text.

```sh
$ comb s
PROVIDER  FIELD    VALUE    PATH    AGE   POLL  TTL
git       branch   main     /repo   14s    46s  ★     60s×12 ◉
git       dirty    true     /repo   14s    46s  ★     60s×12 ◉
battery   percent  87       -        8s    22s  ★     30s×04
hostname  short    artemis  -        3h      -  ---

# Filter to entries in a specific lifecycle state
comb s --filter=lifecycle=active
comb s --filter=lifecycle=decay1
comb s --filter=fsevents_reinstate=true

# Sort options
comb s --sort age             # oldest first
comb s --sort stale           # stale entries first

# Script-friendly formats (bypass the human preset)
comb s -f tsv
comb s -f json
```

**Flags:** `--format <preset>` (`human` (default), `tsv`, `json`, `csv`, `table`, `sh`), `--filter <key=value>` (keys: `provider`, `path`, `field`, `stale`, `lifecycle`, `fsevents_reinstate`; e.g. `--filter=lifecycle=active`, `--filter=provider=git`, `--filter=fsevents_reinstate=true`), `--sort <default|provider|path|field|value|age|stale>`, `--no-trunc`, `--max-width=auto|N` (default 120), `--color=auto|always|never`, `--ascii`, `--depth N`.

**Destructuring (`--depth`):** object values expand into one row per key, recursively, with the parent's path, age and lifecycle metadata carried down. `--depth 0` is unlimited, `1` shows top-level fields only, `2` goes one level down, and so on.

| Preset | Default depth |
|--------|---------------|
| `human`, `table` | `2` — one level down |
| `json`, `tsv`, `csv`, `sh`, custom templates | `1` — no destructuring |

The human default stops at 2 deliberately: a provider caching a deep API response expands to thousands of rows at unlimited depth, and the long dotted field names then crowd out the VALUE column. Pass `--depth 0` when you do want the whole tree. The machine presets stay at `1` so scripts reading `-f json` keep the output they have today unless they ask for more. Arrays are never expanded.

```sh
$ comb p agent '{"session":{"name":"claude","ctx_pct":42}}'
$ comb s --filter=provider=agent
PROVIDER  FIELD            VALUE   PATH  AGE  POLL  TTL
agent     session.ctx_pct  42      -     0s      -  ---
agent     session.name     claude  -     0s      -  ---

$ comb s --filter=provider=agent --depth 1
PROVIDER  FIELD    VALUE                           PATH  AGE  POLL  TTL
agent     session  {"ctx_pct":42,"name":"claude"}  -     0s      -  ---
```

`--filter` and `--sort` see the destructured field names, so `--filter=field=session.name` works on the expanded rows.

**TTL column key:**

| Lead | Lifecycle | Meaning |
|------|-----------|---------|
| `★`  | Active    | alive, polling at base rate |
| `3`  | Decay1    | 3 decay steps before eviction |
| `2`  | Decay2    | 2 remaining |
| `1`  | Decay3    | 1 remaining |
| `0`  | Decay4    | 0 remaining — next tick evicts |
| `---` | Once / Static | no lifecycle (hostname, put entries) |

A `◉` trailing indicator means `fsevents_reinstate=true` — a filesystem event will reinstate the entry to Active even during decay. A `⚠` lead replaces `★` when the provider is in failure-suppress (row renders red).

The default output preset is `human` (coloured table) regardless of whether stdout is a TTY. Scripts that want tab-separated data should pass `-f tsv` explicitly. Colour follows `--color`: `auto` (the default) colours only a TTY, so piping into a pager or a live viewer needs `--color=always`.

Use `comb check daemon` for daemon health (pid, uptime, version, active watchers, request counts, watch backend). The watch backend line reports `native fs events` when the startup self-test confirmed kernel event delivery, or a WARN with `polling` when it didn't — provider file-watching still works, invalidating on a 1s scan instead of kernel events.

## `comb p` (put) `<key> [<json-data>] [--ttl <duration>] [--path <path>] [--global] [--replace] [--null]`

Write static data into the cache. External processes use this to expose state to prompt/statusline consumers without writing a script provider.

```sh
# Put application status into cache — scoped to the current directory
comb p myapp '{"status":"healthy","version":"1.2.3"}'

# Put with TTL — consumers see staleness if writer stops updating
comb p myapp '{"status":"healthy"}' --ttl 30s

# Put with path scope
comb p myapp '{"status":"building"}' --path /home/user/project

# Put once for every directory
comb p myapp '{"status":"healthy"}' --global

# Replace the subtree at the node instead of merging into it
comb p myapp '{"status":"healthy"}' --replace

# Clear a previously written entry
comb p myapp --null
```

**Merge or replace:** a write deep-merges into whatever is already stored at the node, so `comb p myapp '{"version":"1.2.4"}'` leaves a `status` written earlier in place. `--replace` drops the subtree at the node first, so only what this write carries remains there. Both act on static records only — a `put` never removes a record a provider's Source produces. `--null` removes the subtree and writes nothing in its place, and cannot be combined with `--replace`.

`data` at a namespace root must be a JSON object (`put data at the root must be an object`); below the root any JSON is legal, so `comb p myapp.deploy.count 3` and `comb p myapp.status '"healthy"'` are both fine. `--null` is the one case that takes no data argument at all.

**Path:** `put` defaults to the current working directory, exactly as `get` does, so a plain `comb p` followed by a plain `comb g` in the same directory round-trips. `--global` writes `$HOME`, the entry every directory's chain ends at, which is how you publish one value for the whole machine; it cannot be combined with `--path`.

### Session-scoped entries

The `--path` value is a cache coordinate, not a filesystem lookup — a path that doesn't exist on disk is stored
verbatim. Appending a session discriminator to the directory gives each writer its own cache entry, so several agents
working in the same directory can publish independent state:

```sh
# Two agents in one project, each with its own path
comb p agent '{"context_pct":42}' --path "/home/user/project#session-a"
comb p agent '{"context_pct":97}' --path "/home/user/project#session-b"

comb g agent.context_pct "/home/user/project#session-a"   # 42
```

Reads must pass the exact path the writer used: `project#session-a` is a sibling name, not a child of `project`,
so a session-scoped path inherits from the parent directory (`/home/user`) and up — never from the plain project
directory. Use absolute paths: a relative `--path` is resolved against the daemon's working directory, not the
caller's.

Read back with standard `comb g`:

```sh
comb g myapp.status        # healthy
```

Ownership is per record, not per namespace: storing to a field name a Source already produces is rejected (`cannot write <namespace>.<field>: owned by source <source>`), but a static record can sit beside that Source's own records in the same namespace.

**Exit codes:** `0` on success, `2` on error.

## `comb w` (watch) `<key> [--path <path>] [-f format]`

Stream cache changes to stdout. Opens a long-lived connection and emits an NDJSON line on each cache update for the watched key.

```sh
comb w git.branch --path /home/user/project   # default text output, one value per update
comb w git --path /home/user/project          # whole-provider text output
comb w.j git.branch                           # JSON envelope per update
```

First line: current value (or cache miss). Subsequent lines: emitted on each change. Ctrl-C to stop.

Field-level filtering: `comb w git.branch` only emits when the branch value changes, not on every git provider update.

**Exit codes:** `0` on success (clean disconnect), `2` on error.

## `comb e` (eval) `<template> [path]`

Evaluate a value expression against cached provider data, resolving every referenced key in a single connection.

A value expression is written in one of two forms:

- **Single tag** — exactly one `{{ expr }}` spanning the whole argument apart from surrounding whitespace (`'{{ git.dirty }} '` is still a single tag): keeps the expression's natural type (bool, number, object, …).
- **Template** — literal text and/or more than one tag: always string-valued. A source with no tags at all is a template too — it renders to its own literal text.

```sh
# Template (two tags + literal text) — a formatted prompt segment, string-valued
comb e "{{ git.branch }} | {{ battery.percent }}%" .

# Single tag keeping its natural type
comb e '{{ git.dirty }}' .

# No tags at all — literal text, rendered as written
comb e hello   # → hello

# With path
comb e "{{ git.branch }}" /home/user/project

# Template: conditionals and filters
comb e '{% if git.dirty %}*{{ git.branch }}{% else %}{{ git.branch }}{% endif %}' .
comb e '{{ git.branch | truncate(20) }}' .
```

Available filters: `truncate`, `default`, `upper`, `lower`, `length`.

The natural type a single-tag expression keeps is mostly *not* visible
in `comb e`'s own output, which is text: a bool prints `true`, and so does the
string `"true"`. Where it does show is in the shapes text cannot flatten — an
object prints as sorted `key=value` lines and an array as JSON — and in every
programmatic consumer: `bc_eval`, and so every SDK's `eval`, hands back a typed
value (bool, number, object, array). A template is always a string in all of
them.

**Exit codes:** `0` on success, `2` on error.

### `exec()` — running a command from an expression

A value expression evaluated by `comb e` can call `exec()` to run a command and use its output:

```sh
comb e '{{ exec("git", "rev-parse", "--short", "HEAD") }}' .
```

| | |
|---|---|
| Signature | `exec(cmd, arg1, arg2, ..., timeout_ms=10000, stdin=none)` |
| Spawn | argv only — `cmd` and each positional argument become one argv entry each. No shell: no interpolation, no globbing, no `\|`/`&&`/redirection. |
| Argument coercion | string → itself; bool → `"true"`/`"false"` (not minijinja's `Display`, which prints `True`/`False`); number → its canonical string form; `undefined`/`none` → error (typically a missed daemon ref reaching `exec()` un-guarded); a sequence or map → error, pointing at `stdin=` instead |
| `timeout_ms` | keyword, default `10000`. A command still running at the deadline is killed (whole process group) and the call fails. |
| `stdin` | keyword, default none. Must be a string — pass structured data through `\| tojson` first (e.g. `stdin=(some_map \| tojson)`). |
| Output | stdout, lossy-UTF-8 decoded, with at most one trailing `\n` (and a preceding `\r`) stripped |
| Output cap | stdout past 1 MiB is a failure, not a truncation |
| Failure | a non-zero exit, a timeout, an oversized stdout, or a spawn error is an eval error naming the command, with stderr (truncated) attached |
| Availability | `comb eval` and `comb get`'s virtual-field evaluation only — the CLI user already has a shell. Disabled everywhere else a value expression is evaluated: path expressions, every other formatting surface (`-f fmt`, `watch -f fmt`, the status formatter), and the FFI/SDK surface (`bc_eval`/`bc_resolve`). A call on a disabled surface fails immediately with `exec() is disabled in this context`. |

`exec()` always runs client-side — the daemon never executes it, the same as every other value expression. See the [Configuration Reference](/docs/reference/configuration-reference) for a virtual-field example that wraps a script call.

## `comb i` (init)

Detect installed tools (starship, powerlevel10k, tmux, neovim, polybar, waybar, sketchybar, oh-my-zsh, etc.) and print integration snippets for them.

```sh
comb i
```

Takes no subcommand. The output is appropriate shell/config fragments that you can source or paste into your shell RC, tmux config, neovim config, etc.

## `comb c` (check) `[subcommand]`

Run health checks and introspect daemon internals. Without a subcommand, runs top-level aggregation across all subjects.

```sh
comb c                   # aggregate across all subjects
comb c daemon            # daemon health: pid, version, uptime, request counts, watchers
comb c providers         # provider health and failure-suppress state
comb c config            # validate config file syntax
comb c cache             # cache entries, staleness, hit/miss summary
comb c lifecycle         # keys in the decay/drain sequence
comb c watches           # active filesystem watch registrations
comb c timers            # poll timers and last-run times
comb c demand            # demand-tracked keys and last-query times
comb c procs             # 1-minute process snapshot, categorize against providers
```

Each check prints `[PASS]`, `[WARN]`, or `[FAIL]` with a short explanation.

`--connect-timeout <DURATION>` bounds how long the check keeps trying to reach the daemon before reporting it unreachable (default `1s`; `0` for a single attempt, or e.g. `250ms`, `2s`). Retries start 5ms apart and double, so a daemon that is mid-restart is reached as soon as it binds. The option is accepted before or after the subcommand: `comb c --connect-timeout 0 daemon`.

**Exit codes:** `0` if healthy, `1` if unhealthy, `2` on error.

## `comb d` (daemon) `[--socket <path>] [--no-reap]`

Run the daemon in the foreground. You almost never need this — the daemon is socket-activated automatically. Use it for debugging or for running under a process supervisor.

Socket path resolution order: `daemon.socket_path` in config → `BEACHCOMBER_SOCKET` env var → `/tmp/beachcomber-<uid>/sock`.

```sh
# Run with debug logging
BEACHCOMBER_LOG=debug comb d

# Override socket path
comb d --socket /tmp/beachcomber-debug.sock
```

The daemon exits cleanly on SIGINT (Ctrl+C) or SIGTERM. Prefer `comb kill` from another shell rather than signalling manually.

**One daemon per user.** The daemon whose socket matches its own resolution above is the *canonical* daemon. It removes orphaned `comb daemon` processes (reparented to PID 1, on sockets nothing resolves) at startup and hourly. A foreground debug run under your shell is never touched; a daemon you run under a supervisor on a *non-default* socket is parented to PID 1, so mark it exempt with `--no-reap`. Full rules in the repo's `docs/canon/singleton.md`.

## `comb k` (kill) `[--timeout <secs>] [--socket <path>]`

Stop the running daemon. Sends `SIGTERM` and waits for the daemon to exit; the next `comb` query will socket-activate a fresh daemon. No-op if the daemon is not running.

```sh
comb kill                 # default 5s timeout
comb k                    # short form
comb kill --timeout 30    # wait longer on slow shutdowns
comb kill --socket /tmp/beachcomber-debug.sock   # target a custom socket
```

The command queries the daemon for its PID via `introspect{subject:"daemon"}`, so it works even when the PID file is stale or missing.

**Exit codes:** `0` on success (including no-op), `1` if the daemon didn't exit within `--timeout`, `2` on error (pid could not be determined, signal failed).
