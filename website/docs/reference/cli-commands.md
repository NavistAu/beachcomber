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
| `.f` | template | interpolate provider fields via `{field}` placeholders |

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

## `comb g` (get) `<key> [path] [-f format]`

Query a cached value. Returns cached data immediately. On a cold cache (first query for a key), executes the provider inline and blocks briefly while it runs — subsequent queries return the cached value with no delay.

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
comb g.f '{branch} ({dirty})' git .      # → main (false)

# Shell-variable output (key=value lines, sourceable)
comb g.s git .
# ahead=0
# behind=0
# branch=main
# dirty=false

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

Show daemon health and statistics.

```sh
$ comb s
{
  "ok": true,
  "data": {
    "pid": 12345,
    "version": "0.5.1",
    "uptime_secs": 7234,
    "active_watchers": 4,
    "requests_total": 184291,
    "cache_entries": 14,
    "providers": 19
  }
}
```

## `comb p` (put) `<key> <json-data> [--ttl <duration>] [--path <path>]`

Write data into the cache as a virtual provider. External processes use this to expose state to prompt/statusline consumers without writing a script provider.

```sh
# Put application status into cache
comb p myapp '{"status":"healthy","version":"1.2.3"}'

# Put with TTL — consumers see staleness if writer stops updating
comb p myapp '{"status":"healthy"}' --ttl 30s

# Put with path scope
comb p myapp '{"status":"building"}' --path /home/user/project
```

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

Evaluate an expression against cached provider data. Expressions can reference provider fields and combine multiple values.

```sh
# Print a formatted prompt segment
comb e 'git.branch + " " + git.dirty'

# Evaluate with an explicit path
comb e 'git.ahead > 0' --path /home/user/project
```

**Exit codes:** `0` on success, `2` on error.

## `comb i` (init)

Detect installed tools (starship, powerlevel10k, tmux, neovim, polybar, waybar, sketchybar, oh-my-zsh, etc.) and print integration snippets for them.

```sh
comb i
```

Takes no subcommand. The output is appropriate shell/config fragments that you can source or paste into your shell RC, tmux config, neovim config, etc.

## `comb c` (check) `[subcommand]`

Run health checks. With no subcommand, checks daemon reachability.

```sh
comb c                # quick daemon-reachability check
comb c all            # run all checks
comb c daemon         # daemon connectivity
comb c config         # validate config syntax
comb c providers      # provider health and backoff state
comb c cache          # stale-entry report
comb c procs          # process-spawn snapshot (60s sample by default)
```

Subcommands: `all`, `daemon`, `config`, `providers`, `cache`, `procs`.

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

The command asks the daemon for its pid via the status socket, so it works even when the pid file is stale or missing.

**Exit codes:** `0` on success (including no-op), `1` if the daemon didn't exit within `--timeout`, `2` on error (pid could not be determined, signal failed).
