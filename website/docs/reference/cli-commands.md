---
sidebar_position: 1
title: CLI Commands
---

# CLI Commands

All commands are subcommands of `comb`. The daemon is socket-activated — you never need to start it manually.

## `comb get <key> [path] [-f format]`

Query a cached value. Returns immediately with cached data; never waits for a fresh computation.

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
- `1` — cache miss (provider has no data yet)
- `2` — error (daemon unreachable, unknown provider, invalid key)

## `comb poke <key> [path]`

Trigger immediate recomputation of a provider. Returns immediately after acknowledging the request — does not wait for the result. The next `get` will return the fresh value.

```sh
# Force git status refresh after a branch switch
comb poke git .

# Force network info refresh after connecting to VPN
comb poke network

# After modifying kubeconfig manually
comb poke kubecontext
```

**Exit codes:** `0` on success, `2` on error.

## `comb status`

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

## `comb list`

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

## `comb daemon [--socket <path>]`

Run the daemon in the foreground. You almost never need this — the daemon is socket-activated automatically. Use it for debugging or for running under a process supervisor.

```sh
# Run with debug logging
BEACHCOMBER_LOG=debug comb daemon

# Override socket path
comb daemon --socket /tmp/beachcomber-debug.sock
```

The daemon exits on SIGINT (Ctrl+C) with a graceful shutdown sequence.
