---
sidebar_position: 3
title: Debugging
---

# Debugging

## Log file

The daemon writes logs to `~/.local/state/beachcomber/daemon.log` (XDG state home). Both the foreground and background (socket-activated) daemon use this file. Logs are appended across restarts.

```sh
# Watch live daemon logs
tail -f ~/.local/state/beachcomber/daemon.log
```

## Changing the log level

The default log level is `info`. To enable debug logging, set `log_level` in your config:

```toml
# ~/.config/beachcomber/config.toml
[daemon]
log_level = "debug"
```

Valid levels: `trace`, `debug`, `info`, `warn`, `error`.

You can also override it at runtime using the `RUST_LOG` environment variable when running the daemon in the foreground (see below).

## Running the daemon in the foreground

The easiest way to watch what the daemon is doing is to run it interactively. Stop any running background instance first, then start it yourself:

```sh
# Stop the background daemon
comb kill

# Run in foreground with debug logging
RUST_LOG=debug comb d

# Or use a custom socket to avoid interfering with your running setup
comb d --socket /tmp/beachcomber-debug.sock
```

Logs print directly to your terminal. Press Ctrl+C to shut down.

## Checking active state with `comb s` and `comb check`

`comb s` shows warm cache entries as a table — one row per provider field. The `TTL` column shows each entry's lifecycle position and how close it is to eviction:

```sh
$ comb s
PROVIDER  PATH    FIELD    VALUE    AGE   TTL
git       /repo   branch   main     14s   ★     60s×12 ◉
git       /repo   dirty    true     14s   ★     60s×12 ◉
battery   -       percent  87       8s    ★     30s×04
hostname  -       short    artemis  3h    ---

# Filter to entries currently in decay
comb s --filter=lifecycle=decay1
comb s --filter=lifecycle=decay2

# Run as a live diagnostic view — keeps colour even under watch(1)
watch -c comb status
```

`★` means Active (healthy, polling at base rate). `3`/`2`/`1`/`0` are the decay countdown — `0` means the entry evicts on the next scheduler tick. `◉` means filesystem events will reinstate the entry. `⚠` replaces `★` when the provider is in failure-suppress.

For daemon internals, use `comb check` subjects:

```sh
comb check daemon     # pid, version, uptime, request counts, active watchers
comb check watches    # filesystem paths currently being watched for changes
comb check lifecycle  # keys in the decay/eviction sequence
comb check timers     # active poll timers and when they last ran
comb check demand     # demand-tracked keys and last-query times
comb check providers  # provider health and failure-suppress state
```

## Killing and restarting the daemon

The daemon will restart automatically the next time any client queries it (socket activation). To force a restart, use the built-in `kill` command:

```sh
comb kill                # graceful SIGTERM, waits up to 5s for shutdown
comb kill --timeout 30   # wait up to 30s instead

# The daemon restarts automatically on next query
# comb g returns plain text by default. g = get, no suffix needed.
comb g hostname.short
```

`comb kill` asks the daemon for its pid over the socket, so it works even if the pid file is stale or missing.

## Common issues

**Daemon never starts / connection refused**

The daemon socket lives at `/tmp/beachcomber-<uid>/sock` (unless overridden via config or `BEACHCOMBER_SOCKET`). Check that the socket exists:

```sh
ls -la /tmp/beachcomber-$(id -u)/
```

If the socket is missing, run `comb d` in the foreground to see why it failed to start.

**Watch-driven entries never refresh on file changes**

At startup the daemon self-tests whether kernel filesystem events (FSEvents / inotify) actually deliver. When they don't, provider file-watching falls back to a 1s polling scan — correct, just slower. Check which backend is live:

```sh
comb check daemon     # PASS "watch backend: native fs events", or WARN "polling"
```

`comb s` also prints a stderr warning when the backend is degraded. A polling fallback usually means the environment suppresses kernel events (some sandboxes and containers) or `fseventsd` is unhealthy on the host.

**Multiple `comb daemon` processes**

At most one daemon per user is intended. The daemon on the default socket reaps orphaned daemons (reparented to PID 1, on sockets no client resolves) at startup and hourly, so strays remove themselves within the hour. Persistent strays usually mean a long-lived old client — a `comb watch` in a tmux status bar or prompt from before an upgrade — is respawning a daemon on an old socket path; restart that client. Deliberate side daemons are exempt while attended (foreground under a shell) or when started with `--no-reap`.

**Provider always returns stale/empty data**

Check whether the provider is in failure-suppress (failure backoff):

```sh
comb check lifecycle
# Look for the provider in the decay/failure list; also check the daemon log for "suppressed due to failure backoff"
```

Run the provider directly to check for errors:

```sh
# For git, run from inside a repo
comb g git .
tail -20 ~/.local/state/beachcomber/daemon.log
```

**High CPU or unexpected provider executions**

Enable debug logging and watch the log file. Look for repeated `Executed provider` lines:

```sh
RUST_LOG=debug comb d 2>&1 | grep 'Executed provider'
```

If a provider is executing too frequently, check whether a filesystem watcher is triggering on a high-churn path (e.g., a build output directory). Run `comb check watches` to see which paths are being watched.

**Log file grows too large**

Logs are appended indefinitely. Rotate manually or add a logrotate rule:

```sh
# Truncate manually
: > ~/.local/state/beachcomber/daemon.log

# Or set a higher log level to reduce volume
# In ~/.config/beachcomber/config.toml:
# [daemon]
# log_level = "warn"
```
