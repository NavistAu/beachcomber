---
sidebar_position: 1
title: CLI Commands
---

# CLI Commands

All commands are subcommands of `comb`. The daemon is socket-activated — you never need to start it manually.

## `comb get <key> [path] [-f format]` (alias: `g`)

Query a cached value. Returns cached data immediately. On a cold cache (first query for a key), executes the provider inline and blocks briefly while it runs — subsequent queries return the cached value with no delay.

```sh
# Query a specific field from a path-scoped provider
comb get git.branch /path/to/repo
comb get git.branch .          # relative path resolved to absolute

# Query a field from a global provider (no path needed)
comb get battery.percent
comb get hostname.short

# Query the entire provider (all fields)
comb get git .
comb get battery

# Output formats
comb get git.branch . -f text   # prints: main
comb get git.branch . -f json   # prints: full JSON response (default)

# Multi-field text output (key=value lines)
comb get git . -f text
# prints:
# ahead=0
# behind=0
# branch=main
# dirty=false
# staged=0
# stash=0
# state=clean
# untracked=2
# unstaged=0
```

**Exit codes:**
- `0` — success, data returned
- `1` — provider returned no data (e.g. `git` queried outside a git repository)
- `2` — error (daemon unreachable, unknown provider, invalid key)

## `comb refresh <key> [path]` (alias: `r`)

Trigger immediate recomputation of a provider. Returns immediately after acknowledging the request — does not wait for the result. The next `get` will return the fresh value.

```sh
# Force git status refresh after a branch switch
comb refresh git .

# Force network info refresh after connecting to VPN
comb refresh network

# After modifying kubeconfig manually
comb refresh kubecontext
```

**Exit codes:** `0` on success, `2` on error.

## `comb status` (alias: `s`)

Show daemon health and statistics.

```sh
$ comb status
{
  "uptime_secs": 7234,
  "cache_entries": 14,
  "active_watchers": 4,
  "providers": 16,
  "requests_total": 184291
}
```

## `comb list` (alias: `l`)

Show all active providers and their cached state age.

```sh
$ comb list
{
  "entries": [
    { "key": "git", "path": "/Users/me/project", "age_ms": 1240 },
    { "key": "battery", "path": null, "age_ms": 8900 },
    { "key": "kubecontext", "path": null, "age_ms": 22100 }
  ]
}
```

## `comb put <key> <json-data> [--ttl <duration>] [--path <path>]` (alias: `p`)

Write data into the cache as a virtual provider. External processes use this to expose state to prompt/statusline consumers without writing a script provider.

```sh
# Store application status
comb put myapp '{"status":"healthy","version":"1.2.3"}'

# Store with TTL — consumers see staleness if writer stops updating
comb put myapp '{"status":"healthy"}' --ttl 30s

# Store with path scope
comb put myapp '{"status":"building"}' --path /home/user/project
```

Read back with standard `comb get`:

```sh
comb get myapp.status        # "healthy"
```

Namespace hierarchy: builtin > script > virtual. Storing to a name used by a built-in or script provider is rejected.

**Exit codes:** `0` on success, `2` on error.

## `comb watch <key> [--path <path>] [-f format]` (alias: `w`)

Stream cache changes to stdout. Opens a long-lived connection and emits an NDJSON line on each cache update for the watched key.

```sh
comb watch git.branch --path /home/user/project
comb watch git --path /home/user/project
comb watch git.branch -f text
```

First line: current value (or cache miss). Subsequent lines: emitted on each change. Ctrl-C to stop.

Field-level filtering: `comb watch git.branch` only emits when the branch value changes, not on every git provider update.

**Exit codes:** `0` on success (clean disconnect), `2` on error.

## `comb daemon [--socket <path>]` (alias: `d`)

Run the daemon in the foreground. You almost never need this — the daemon is socket-activated automatically. Use it for debugging or for running under a process supervisor.

```sh
# Run with debug logging
BEACHCOMBER_LOG=debug comb daemon

# Override socket path
comb daemon --socket /tmp/beachcomber-debug.sock
```

The daemon exits on SIGINT (Ctrl+C) with a graceful shutdown sequence.
