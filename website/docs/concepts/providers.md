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

A source name is metadata on the record — where it came from, how it's invalidated — never a level in the address. Consumers address only fields and providers:

| Key form | What it returns | Demand effect |
|---|---|---|
| `git.branch` | The `branch` field value | Demand on the `head` source only |
| `git` | All fields from all sources (flattened) | Demand on all git sources |

The registry resolves which source owns each field, so `git.branch` demands only the `head` source's lifecycle, not the whole provider's. `git.refs` is not an address for the `refs` source — `refs` is a source name, not a field, so it misses like any other unknown field.

## When a source runs again

A source's schedule is its `return` line, built from decorators on `result`:

| Decorator | Effect |
|---|---|
| `poll(interval, count)` | Re-executes on a timer while the record is queried; `count` intervals before it starts to decay |
| `watch(path, ...)` | Re-executes when a watched path changes. A relative path is matched within the directory the instance answers for; an absolute one (or `~/...`) is watched directly, for values that live in a config file elsewhere |
| `ttl(interval)` | `poll(interval, 1)` with a decay depth of 1: valid for one interval, then gone |
| `failback(count, interval)` | After `count` failures, suppress execution for `interval`, doubling per level. Always on: the engine's `failback(3, '1s')` applies where a line is silent |
| `budget(duration)` | The execute budget recorded for the source; `[daemon] budget.execute` (default `50ms`) where a line is silent. Recorded, not yet enforced |

`poll` and `watch` together make a watch with a timer backstop. A source with neither is watch-only with nothing to watch: it executes on first demand and is served from the cache after that. Like every source it decays once nothing reads it (the default Active window is 60 s × 12).

```toml
# A config Source: watched files with a poll fallback
[daemon.providers.node_version.sources.main]
data = "{{ exec('node --version') | trim }}"
result.value = "{{ data }}"
return = "{{ result | watch('.node-version', 'package.json') | poll('60s') }}"

# A built-in Source retuned: git's diff polls every 15 seconds
[daemon.providers.git.sources.diff]
return = "{{ result | poll('15s', 8) }}"
```

A built-in Source takes `return` and nothing else; the line replaces its schedule whole, and a built-in with no `return` keeps its own (the `_` Source lines reach it only through `| super`). See the [configuration reference](/docs/reference/configuration-reference).

## Where an instance lives

A config Source's `root` line says which directory an instance answers for: `root = "{{ lup('.git') | dirname }}"` gives one instance per project, shared by every subdirectory of it; no `root` is the single instance at `~`. Built-in sources do the same in Rust: git walks up to `.git/`, mise looks for `mise.toml` or `.mise.toml`, and `battery`, `hostname` and `load` are one instance for the daemon. The cache key of a path-scoped instance is `provider\0path`.

The Rust-side metadata that describes a built-in (its invalidation strategy, `Global`/`PathScoped` scope, watch patterns) is internal to the daemon: it is what `return` replaces, not something a config file names. `comb get <provider>:cache.<field>` shows the `poll`, `watch` and `decay` a record registered with.

## comb status — source on the wire

`comb status` shows one row per field. Each row carries a `source` field in the wire protocol identifying which source produced it; SDKs expose it on the row struct. The default human-readable output is unchanged.
