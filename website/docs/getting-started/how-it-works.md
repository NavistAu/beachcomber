---
sidebar_position: 4
---

# How It Works

beachcomber is a single async daemon that:

1. Serves queries from consumers (prompts, status bars, editors) via a Unix socket
2. Watches filesystem directories using native FSEvents (macOS) or inotify (Linux)
3. Executes providers when files change or poll timers fire — not on every query
4. Caches results in a shared in-memory map (157ns reads)
5. Returns cached data instantly to any number of concurrent readers

The daemon is socket-activated: it starts automatically when any client connects, and shuts down after an idle period when all connections drop.

```mermaid
graph TB
    FS["Filesystem changes"] -->|"FSEvents / inotify"| Sched

    subgraph daemon["beachcomber daemon"]
        Sched["Scheduler"] --> Prov["Providers<br/>git · battery · network<br/>hostname · scripts · (your own)"]
        Prov --> Cache["Cache · 157ns reads"]
        Cache --> USS["Unix Socket Server"]
    end

    USS --> Prompts["zsh / bash / fish prompt<br/>starship"]
    USS --> Status["tmux status<br/>polybar/waybar · sketchybar<br/>oh-my-posh"]
    USS --> Editors["neovim · lualine<br/>scripts · CI/automation"]
```

**Providers are never re-executed on every query.** A git status is computed once when `.git` changes, then served from cache to every reader — whether that's one prompt or a hundred tmux panes. The filesystem watcher is registered once for all concurrent readers.

**Connection context** means consumers can set a working directory once on connect. `comb g git.branch` without an explicit path uses the connection's context directory, making prompt integration natural.

**Provider/Source/Field model:** each provider (e.g., `git`) declares one or more sources (e.g., `refs`, `diff`, `status`), each with its own invalidation strategy and lifecycle. A field belongs to exactly one source. Lifecycle state is tracked per source instance at `(provider, path, source)`, so sibling sources decay independently.

**Demand-driven lifecycle:** the daemon watches nothing until queried. Each `get` request signals demand on the source that owns the queried field, keeping that source warm automatically. Resource usage scales with actual query patterns. When queries stop, source instances transition through an exponential-backoff decay sequence — Active → Decay1 → Decay2 → Decay3 → Decay4 → Evicted — with each stage doubling the interval. A new query at any decay stage reinstates the source to Active immediately. There is no fixed grace period; keep-alive is configured per source (`KeepAlive::Polls(K)`, `KeepAlive::Duration(secs)`, or `KeepAlive::Never` for pure-watch globals).
