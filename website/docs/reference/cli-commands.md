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

## `comb g` (get) `<key>... [path] [-f format]`

Query a cached value. Returns cached data immediately. On a cold cache (first query for a key), executes the provider inline and blocks briefly while it runs — subsequent queries return the cached value with no delay.

**Flags:** `--force` (trigger immediate recomputation before returning), `--wait` (block until a fresh value arrives). Multiple keys can be passed in a single connection (variadic).

```sh
# Query a specific field — text is the default
comb g git.branch .              # → main
comb get git.branch .            # equivalent long form

# Query a field from a global provider (no path needed)
comb g battery.percent           # → 87
comb g hostname.short            # → myhost

# Query the entire provider (all fields) — alternate formats
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
comb g git.branch:source         # → builtin (provider kind: builtin, script, or virtual)
```

**Metadata suffixes:**

| Suffix | Type | Description |
|--------|------|-------------|
| `:age` | int | Milliseconds since the value was last computed |
| `:stale` | bool | Whether the value is past its expected refresh time |
| `:fresh` | bool | Inverse of `:stale` — true when the value is within its refresh window |
| `:cache` | bool | Whether the value was served from cache (`true`) or freshly computed (`false`) |
| `:source` | string | Provider kind: `builtin`, `script`, or `virtual` |

**Exit codes:**
- `0` — success, data returned
- `1` — provider returned no data (e.g. `git` queried outside a git repository)
- `2` — error (daemon unreachable, unknown provider, invalid key)

## `comb s` (status)

Show all warm cache entries as a table — one row per provider field. The `TTL` column encodes each entry's lifecycle position (`★` active, `3`/`2`/`1`/`0` decay countdown), effective poll interval, keep-alive count, and whether filesystem events will reinstate the entry to Active (`◉`).

`watch -c comb status` is the recommended live view — it keeps colour and the human preset active even when stdout is a pipe, so you can watch entries pulse through their lifecycle in real time.

```sh
$ comb s
PROVIDER  PATH    FIELD    VALUE    AGE   TTL
git       /repo   branch   main     14s   ★     60s×12 ◉
git       /repo   dirty    true     14s   ★     60s×12 ◉
battery   -       percent  87       8s    ★     30s×04
hostname  -       short    artemis  3h    ---

# Filter to entries in a specific lifecycle state
comb s --filter=lifecycle=active
comb s --filter=lifecycle=decay1
comb s --filter=fsevents_reinstate=true

# Sort options
comb s --sort age             # oldest first
comb s --sort lifecycle       # most-decayed first
comb s --sort poll_interval   # slowest-pollers first

# Custom per-row template
comb s --format "{{ provider }}.{{ field }}={{ value }}"

# Script-friendly formats (bypass the human preset)
comb s -f tsv
comb s -f json
```

**Flags:** `--format <template>`, `--filter <provider>`, `--filter=lifecycle=active|decay1..4|once|virtual`, `--filter=fsevents_reinstate=true|false`, `--sort <field|age|stale|lifecycle|poll_interval>`, `--no-trunc`, `--max-width=auto|N` (default 120), `--color=auto|always|never`, `--ascii`.

**TTL column key:**

| Lead | Lifecycle | Meaning |
|------|-----------|---------|
| `★`  | Active    | alive, polling at base rate |
| `3`  | Decay1    | 3 decay steps before eviction |
| `2`  | Decay2    | 2 remaining |
| `1`  | Decay3    | 1 remaining |
| `0`  | Decay4    | 0 remaining — next tick evicts |
| `---` | Once / virtual | no lifecycle (hostname, put entries) |

A `◉` trailing indicator means `fsevents_reinstate=true` — a filesystem event will reinstate the entry to Active even during decay. A `⚠` lead replaces `★` when the provider is in failure-suppress (row renders red).

The default output preset is `human` (coloured table) regardless of whether stdout is a TTY. Scripts that want tab-separated data should pass `-f tsv` explicitly. The `WATCH_INTERVAL` environment variable (set automatically by `watch(1)`) also enables colour when stdout is not a TTY.

Use `comb check daemon` for daemon health (pid, uptime, version, active watchers, request counts).

## `comb p` (put) `<key> [<json-data>] [--ttl <duration>] [--path <path>] [--null]`

Write data into the cache as a virtual provider. External processes use this to expose state to prompt/statusline consumers without writing a script provider.

```sh
# Put application status into cache
comb p myapp '{"status":"healthy","version":"1.2.3"}'

# Put with TTL — consumers see staleness if writer stops updating
comb p myapp '{"status":"healthy"}' --ttl 30s

# Put with path scope
comb p myapp '{"status":"building"}' --path /home/user/project

# Clear a previously written entry
comb p myapp --null
```

`--null` clears a previously written virtual provider entry. Omitting `--null` requires a JSON object argument.

Read back with standard `comb g`:

```sh
comb g myapp.status        # healthy
```

Namespace hierarchy: builtin > script > virtual. Storing to a name used by a built-in or script provider is rejected.

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

Evaluate a minijinja template string against cached provider data. Resolves all referenced keys in a single connection.

```sh
# Formatted prompt segment
comb e "{{ git.branch }} | {{ battery.percent }}%" .

# With path
comb e "{{ git.branch }}" /home/user/project

# Conditionals and filters
comb e '{% if git.dirty %}*{{ git.branch }}{% else %}{{ git.branch }}{% endif %}' .
comb e '{{ git.branch | truncate(20) }}' .
```

Available filters: `truncate`, `default`, `upper`, `lower`, `length`.

**Exit codes:** `0` on success, `2` on error.

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

**Exit codes:** `0` if healthy, `1` if unhealthy, `2` on error.

## `comb d` (daemon) `[--socket <path>]`

Run the daemon in the foreground. You almost never need this — the daemon is socket-activated automatically. Use it for debugging or for running under a process supervisor.

```sh
# Run with debug logging
BEACHCOMBER_LOG=debug comb d

# Override socket path
comb d --socket /tmp/beachcomber-debug.sock
```

The daemon exits cleanly on SIGINT (Ctrl+C) or SIGTERM. Prefer `comb kill` from another shell rather than signalling manually.

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
