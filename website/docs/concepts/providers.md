---
sidebar_position: 1
title: Providers, Sources, and Fields
---

# Providers, Sources, and Fields

beachcomber organizes cached data in three layers: **Providers**, **Sources**, and **Fields**.

## The three-layer model

```
Provider                       (namespace)
  └── Source                   (one invalidation strategy + lifecycle)
        └── Field              (a single typed value)
```

**Provider** — a named namespace that groups related data. The name is the key used in `comb get <name>.<field>`. Examples: `git`, `mise`, `battery`, `hostname`.

**Source** — a single logical unit of execution within a provider. Every source has exactly one invalidation strategy (filesystem events, polling, or both), its own lifecycle entry in the scheduler, and its own set of fields it produces. A provider declares one or more sources.

**Field** — a typed value produced by a source. Fields from different sources within the same provider share the provider namespace but are produced independently.

## Why multiple sources per provider?

Different fields within a provider may need different refresh rates or different triggers. For example, `git` has:

| Source | Strategy | Fields produced |
|---|---|---|
| `head` | fsevent on `.git/` | `branch`, `detached` |
| `refs` | fsevent on `.git/` | `commit`, `tag`, `ahead`, `behind`, `upstream`, `state`, `state_step`, `state_total`, `stash`, `last_commit_age_secs`, `commit_summary`, `push_ahead`, `push_behind` |
| `diff` | poll 30s | `lines_added`, `lines_removed`, `lines_staged_added`, `lines_staged_removed` |
| `lineage` | poll 60s | `branch_parent`, `lines_added_parent`, `lines_removed_parent`, `branch_default`, `lines_added_default`, `lines_removed_default` |
| `status` | fsevent on `.git/index` + poll 60s | `staged`, `unstaged`, `untracked`, `conflicted`, `dirty` |

Branch information changes when `.git/HEAD` or `.git/refs/` is written — a filesystem event is the right trigger. Diff line counts need a subprocess call that is slower, so polling once every 30 seconds is a better fit.

Splitting into sources means a branch lookup only demands the `head` source lifecycle, not the slower `diff` source. The cache delivers `git.branch` from a filesystem-event-driven entry with millisecond freshness while `git.lines_added` waits for its poll cycle.

## Addressing

A source name is metadata on a field — where it came from, how it's invalidated — never a level in the address. Consumers address only fields and providers:

| Key form | What it returns | Demand effect |
|---|---|---|
| `git.branch` | The `branch` field value | Demand on the `head` source only |
| `git` | All fields from all sources (flattened) | Demand on all git sources |

The registry resolves which source owns each field, so `git.branch` demands only the `head` source's lifecycle, not the whole provider's. `git.refs` is not an address for the `refs` source — `refs` is a source name, not a field, so it misses like any other unknown field.

## Invalidation strategies

Each source declares one of three strategies:

| Strategy | TOML `type` | Trigger | Watch config keys |
|---|---|---|---|
| `Poll` | `"poll"` | Re-executes on a fixed timer | — |
| `Watch` | `"fsevent"` | Re-executes when watched paths change | `fsevent_patterns`, `fsevent_abs_paths` |
| `WatchAndPoll` | `"fsevent_poll"` | Re-executes on watched path changes AND on a timer backstop | `fsevent_patterns`, `fsevent_abs_paths` |

`fsevent_patterns` — relative path components matched within the source's scope path (path-scoped sources). `fsevent_abs_paths` — absolute filesystem roots watched directly; used by global sources and for cross-directory config files. Use `expand_abs_path()` when building metadata in Rust to expand `~`, `$HOME`, and XDG vars.

**Pure-watch global sources** are a special case of `Watch` with `scope = "global"` and no decay (`KeepAlive::Never`). These execute once on first demand and re-execute only when an fs event fires on their `fsevent_abs_paths`. Used for values that can only change when a specific config file is written: hostname, username, OS name, mise global config.

## Scope

Each source is either **global** or **path-scoped**:

- **Global** (`scope = "global"`) — one lifecycle instance per daemon. The cache key is just the provider name. Examples: `battery`, `hostname`, `load`, `mise.global`.
- **Path-scoped** (`scope = "path"`) — one lifecycle instance per project directory. The cache key is `provider\0path`. Examples: `git.refs`, `git.status`, `mise.project`.

Path-scoped sources walk the directory tree to find a project root (e.g., git looks for `.git/`, mise for `mise.toml` or `.mise.toml`). All subdirectories of the same project share one cache entry.

## comb status — source on the wire

`comb status` shows one row per field. Each row carries a `source` field in the wire protocol identifying which source produced it; SDKs expose it on the row struct. The default human-readable output is unchanged.
