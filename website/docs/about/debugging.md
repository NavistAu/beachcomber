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
# Kill the background daemon
pkill -f 'comb daemon'

# Run in foreground with debug logging
RUST_LOG=debug comb daemon

# Or use a custom socket to avoid interfering with your running setup
comb daemon --socket /tmp/beachcomber-debug.sock
```

Logs print directly to your terminal. Press Ctrl+C to shut down.

## Checking active state with `comb status`

`comb status` returns a JSON snapshot of the daemon's internal state:

```sh
comb status
```

```json
{
  "cache_entries": 3,
  "providers": 12,
  "watched_paths": ["/Users/you/project"],
  "in_flight": [],
  "backoff": [],
  "poll_timers": [
    {
      "provider": "battery",
      "path": null,
      "interval_secs": 30,
      "last_run_secs_ago": 12
    }
  ],
  "demand": [
    {
      "provider": "git",
      "path": "/Users/you/project",
      "last_query_secs_ago": 5
    }
  ]
}
```

Key fields:

- `watched_paths` — filesystem paths currently being watched for changes
- `in_flight` — providers currently executing (non-empty means a computation is running right now)
- `backoff` — keys in the drain/eviction sequence after demand expired
- `poll_timers` — active poll timers and when they last ran
- `demand` — providers kept warm by recent queries and when they were last queried

## Killing and restarting the daemon

The daemon will restart automatically the next time any client queries it (socket activation). To force a restart:

```sh
# Kill by PID file (socket path depends on your platform)
kill $(cat ~/.local/state/beachcomber/daemon.pid 2>/dev/null || \
       cat /run/user/$(id -u)/beachcomber/daemon.pid 2>/dev/null)

# Or by process name
pkill -f 'comb daemon'

# The daemon restarts automatically on next query
comb get hostname.short -f text
```

## Common issues

**Daemon never starts / connection refused**

The daemon socket path depends on `$XDG_RUNTIME_DIR` (Linux) or `$TMPDIR` (macOS). Check that the socket exists:

```sh
ls -la /run/user/$(id -u)/beachcomber/   # Linux
ls -la $TMPDIR/beachcomber-$(id -u)/     # macOS fallback
```

If the socket is missing, run `comb daemon` in the foreground to see why it failed to start.

**Provider always returns stale/empty data**

Check whether the provider is in a failure backoff loop:

```sh
comb status
# Look at the "backoff" field and the daemon log for "suppressed due to failure backoff"
```

Run the provider directly to check for errors:

```sh
# For git, run from inside a repo
comb get git .
tail -20 ~/.local/state/beachcomber/daemon.log
```

**High CPU or unexpected provider executions**

Enable debug logging and watch the log file. Look for repeated `Executed provider` lines:

```sh
RUST_LOG=debug comb daemon 2>&1 | grep 'Executed provider'
```

If a provider is executing too frequently, check whether a filesystem watcher is triggering on a high-churn path (e.g., a build output directory). Check `watched_paths` in `comb status`.

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
