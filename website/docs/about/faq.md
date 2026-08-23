---
sidebar_position: 2
title: FAQ
---

# FAQ

### Does beachcomber replace starship / powerlevel10k / oh-my-posh?

No. beachcomber is infrastructure — a data cache that prompt frameworks can consume. It does not render prompts, apply themes, or manage shell hooks. Think of it as a fast, shared data source that your existing prompt setup can optionally use instead of computing everything from scratch.

With beachcomber, starship reads git state from a cache instead of invoking gitoxide. With beachcomber, powerlevel10k (if it gains socket support) could share one gitstatusd-equivalent across all shells. The prompt frameworks stay; they just get faster.

### Why not just use Watchman?

Watchman tells you *which files changed*. beachcomber tells you *what the git status is*, *what the battery percentage is*, *which kubernetes context is active*. Watchman is a lower-level primitive — it produces events, not interpreted state.

Building on Watchman would mean beachcomber is also responsible for maintaining a Watchman installation, handling its failure modes, and adding 88MB+ to your system. The `notify` crate beachcomber uses talks to FSEvents/inotify directly, achieving the same result without the dependency.

### How much memory does the daemon use?

Light. The cache holds one result object per `(provider, path)` combination. A typical developer session with 10 active providers across 3 directories is around 30 cache entries. Provider results are small — the git state object is a few dozen bytes.

Unlike Watchman, beachcomber does not maintain an in-memory database of every file's metadata. It knows that `.git/HEAD` changed; it does not index every file in your repository.

On a system with 20 shells and typical usage, expect the daemon to use 10-30MB of RSS. The tokio thread pool is fixed-size; provider executions happen on `spawn_blocking` threads that are bounded by tokio's defaults.

### What happens when the daemon crashes?

The socket file is cleaned up on graceful exit. If the daemon crashes unexpectedly, the stale socket file may remain. The next client connection will attempt to connect, fail, detect the stale socket, remove it, start a fresh daemon instance, and retry. This is handled transparently — `comb g` will succeed with a slight delay on the restart.

You can verify the daemon is responsive at any time with `comb s`. If the daemon is unhealthy, run `comb kill` to stop it — it will socket-activate automatically on the next query.

### Can I use this on Linux?

Yes. macOS and Linux are both first-class supported platforms. The filesystem watcher uses FSEvents on macOS and inotify on Linux; the battery and network providers have native implementations for both.

A few fields are macOS-specific or depend on optional Linux components — for example, `battery.time_remaining` needs UPower on Linux and reads `"unknown"` if it isn't installed. See the [built-in providers reference](../reference/built-in-providers.md) for per-field platform notes.

Pre-built Linux binaries (glibc, musl, arm64) and packages (`.deb`, `.rpm`, AUR) are published on every release — see [Installation](../getting-started/installation.md).

### Can I run multiple daemons simultaneously?

The daemon is designed for one instance per user. Multiple daemon instances would each have independent caches and independent filesystem watchers, defeating the purpose of centralization. The socket activation logic prevents this by design: if a socket already exists and is responsive, the client uses it.

If you need per-project isolation (e.g., different config for work vs personal projects), use `daemon.socket_path` in a per-project config to run daemons on separate sockets.

### How do I add a provider for a tool beachcomber doesn't know about?

Write a script provider. See the Custom Providers Guide. If the provider would be useful to everyone (not just your specific setup), consider contributing it as a built-in — see [Contributing](./contributing).

### How do I check what version is running?

Run `comb --version` to see the version of the installed binary. To see what the running daemon reports (useful if you have multiple binaries on `PATH` or just updated), check `comb s` — the status output includes the running daemon's version.

### How do I update beachcomber?

Update using the same package manager you installed with: `brew upgrade beachcomber`, `npm install -g beachcomber@latest`, `pip install -U beachcomber`, `cargo install beachcomber --force`, `yay -Syu beachcomber`, etc. The pre-built `.deb` and `.rpm` releases can be upgraded by installing the newer package.

The daemon supervises its own binary — an fs-event watch plus a 5s mtime poll — and gracefully shuts down when the file on disk changes. The next `comb` query respawns the new build automatically. If your package manager installs each version at a *new path* (versioned cellars with a symlink flip can do this), the old file never changes and the old daemon keeps running; cut over manually:

```sh
comb kill                # stop the running daemon
comb s                   # triggers socket activation of the new binary
```

### What is the `stale` flag in responses?

Each provider has an expected refresh interval. If the cached value is older than that interval plus some tolerance, `stale: true` is set in the response. The value is still returned — beachcomber never blocks a read waiting for fresh data.

Consumers can use `stale` to decide whether to show a loading indicator or use a different visual style. For prompt use, ignoring `stale` is usually the right choice — showing a slightly old branch name is better than blocking the prompt.
