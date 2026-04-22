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

`comb s` shows warm cache entries as a table — one row per provider field:

```sh
$ comb s
provider   field      value      age_ms  stale
git        branch     main          234  false
git        dirty      false         234  false
battery    percent    85           4200  false

# Filter to a specific provider
comb s --filter git
```

For daemon internals, use `comb check` subjects:

```sh
comb check daemon     # pid, version, uptime, request counts, active watchers
comb check watches    # filesystem paths currently being watched for changes
comb check backoff    # keys in the drain/eviction sequence after demand expired
comb check timers     # active poll timers and when they last ran
comb check demand     # demand-tracked keys and last-query times
comb check providers  # provider health and backoff state
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

The daemon socket path depends on `$XDG_RUNTIME_DIR` (Linux) or `$TMPDIR` (macOS). Check that the socket exists:

```sh
ls -la /run/user/$(id -u)/beachcomber/   # Linux
ls -la $TMPDIR/beachcomber-$(id -u)/     # macOS fallback
```

If the socket is missing, run `comb d` in the foreground to see why it failed to start.

**Provider always returns stale/empty data**

Check whether the provider is in a failure backoff loop:

```sh
comb check backoff
# Look for the provider in the backoff list; also check the daemon log for "suppressed due to failure backoff"
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
