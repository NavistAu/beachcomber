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
| `head` | watch on `.git/` | `branch`, `detached` |
| `refs` | watch on `.git/` | `commit`, `tag`, `ahead`, `behind`, `upstream`, `state`, `state_step`, `state_total`, `stash`, `last_commit_age_secs`, `commit_summary`, `push_ahead`, `push_behind` |
| `diff` | poll 30s | `lines_added`, `lines_removed`, `lines_staged_added`, `lines_staged_removed` |
| `lineage` | poll 60s | `branch_parent`, `lines_added_parent`, `lines_removed_parent`, `branch_default`, `lines_added_default`, `lines_removed_default` |
| `status` | watch on `.git/index` + poll 60s | `staged`, `unstaged`, `untracked`, `conflicted`, `dirty` |

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
| `watch(path, ...)` | Re-executes when a watched path changes. A relative path is matched within the directory the instance answers for; an absolute one (or `~/...`) is watched directly, for values that live in a config file elsewhere. A read also checks the paths itself (one `stat` each) before it answers from the record, so the read straight after a change has the new value without waiting for the file event. A watched directory is checked by its own mtime: a file created, removed or renamed in it counts, a change deeper in does not |
| `ttl(interval)` | `poll(interval, 1)` with a decay depth of 1: valid for one interval, then gone |
| `failback(interval, count)` | After `count` failures, suppress execution for `interval`, doubling per level. Always on: the engine's `failback('1s', 3)` applies where a line is silent |
| `nearest` | The nearest instance answers alone, instead of cascading up to `~`: see [Where an instance lives](#where-an-instance-lives). Takes no arguments; not saying it is cascade |
| `budget(duration)` | How long a read waits for the Source before answering with what it has; `[daemon] budget` (default `50ms`) where a line is silent. A read of a record past its lifetime, or one whose watched paths changed, runs the Source and waits this long, then answers what is stored |
| `timeout(duration)` | How long one run of the Source may take before it is cut off and fails; `[daemon] timeout` (default `10s`) where a line is silent |

`poll` and `watch` together make a watch with a timer backstop. A source with neither is watch-only with nothing to watch: it executes on first demand and is served from the cache after that. Like every source it decays once nothing reads it (the default Active window is 60 s × 12).

```toml
# A config Source: watched files with a poll fallback
[daemon.providers.node_version.sources.main]
data = "{{ exec('node --version') | trim }}"
result.value = "{{ data }}"
return = "{{ result | watch('.node-version', 'package.json') | poll('60s') }}"

# A shipped Source retuned: git's diff polls every 15 seconds. The line replaces
# everything the Source declares, `nearest` included, so git's says it again
[daemon.providers.git.sources.diff]
return = "{{ result | poll('15s', 8) | nearest }}"
```

Any line of a shipped Source can be set in a later layer. `return` replaces its schedule whole, and its `nearest` setting with it (a retuned git Source that leaves `| nearest` off cascades), and a built-in with no `return` keeps its own (the `_` Source lines reach it only through `| super`). See the [configuration reference](/docs/reference/configuration-reference).

## What a provider is

A provider is its Sources plus three lines of its own, which the shipped `defaults/00-comb.toml` writes once for every provider as the abstract provider `[daemon.providers._]`:

```toml
[daemon.providers._]
template = "{{ client.providers[_.name].template }}"     # this provider's client templates, seeded as the :template facet
result   = "{{ merge(sources) }}"                         # call each Source, merge the objects
return   = "{{ result | facet('template', template) }}"   # the data, with the :template facet beside it
```

`result` is what the provider reads as: each of its Sources is called (`sources.head()` is that Source's stored record where the read is standing) and the records are merged, so a provider with two Sources is one tree. `return` hands that tree out, as it is or reshaped, and attaches the `:template` facet beside it. A provider may write any of the three; a `return` can pipe into the abstract one with `| super` (a `template` or `result` line is the whole value, and `| super` there is a load error). See the [configuration reference](/docs/reference/configuration-reference).

A provider can be switched off: `[daemon.providers.git] enabled = false` sets its `result` to `"{{ {} }}"` and its `return` to `"{{ result }}"`, and none of its Sources is selected, called or run. A read of it is a miss, with no template half either (its `:template` facet is still readable by name), and a later layer can set `enabled = true` again. See [Switching a provider off](/docs/reference/configuration-reference#switching-a-provider-off--enabled--false).

## Where an instance lives

A config Source's `root` line says which directory an instance answers for: `root = "{{ lup('.git') | dirname }}"` gives one instance per project, shared by every subdirectory of it; no `root` is the single instance at `~`. Built-in sources do the same in Rust: git walks up to `.git/`, mise looks for `mise.toml` or `.mise.toml`, and `battery`, `hostname` and `load` are one instance for the daemon.

A Source's instances combine in one of two ways.

**Cascade** is the default. A read asks the nearest instance first. If the key is missing there it goes to the parent of that instance's root, and so on up to the instance at `~`, where it stops: `mise` is a single Source, so from a project `mise.node` is the project's while `mise.go`, which only the global config sets, is found at `~`. `comb get mise` merges the instances it walks, the nearer winning per leaf. A directory with no root is served by the instance at `~`. Use it when the instances are layers of one setting.

**Nearest** is `| nearest` on the `return` line. The nearest instance answers alone: a key it lacks is absent, no outer instance fills in, and a directory with no root has no instance at all, so no value and nothing runs; `~` is not a fallback. `git` is nearest: inside a nested repository `comb get git.branch_parent` is that repository's or absent, never the superproject's, and outside any repository there are no git values. (A repository at `$HOME` is found like any other, so every directory under `$HOME` without a repository of its own reads it.) `direnv` (the nearest `.envrc`), `python` (the venv) and `terraform` are nearest too; `mise` and `asdf` cascade. Use it when the instance is the one you are in.

```toml
# A config Source over the nearest package.json only
[daemon.providers.pkg.sources.main]
root = "{{ lup('package.json') | dirname }}"
data = "{{ exec('cat package.json') | fromjson }}"
result.name = "{{ data.name }}"
return = "{{ result | watch('package.json') | nearest }}"

# Every git config Source nearest: one with no return of its own, or whose return calls `| super`, takes it
[daemon.providers.git.sources._]
return = "{{ result | nearest | super }}"
```

Records written by `put`, and the `config:` seeds, merge up the directory chain whichever a Source does. The cache key of a rooted instance is `provider\0root`.

A Source may call another: `sources.<source>()` for a sibling, `daemon.providers.<provider>.sources.<source>()` for any provider's, and a provider's `result` and `return` lines may call the latter: `return = "{{ result | merge(daemon.providers.gitstats.sources.repo()) | facet('template', template) }}"`. A call reads the called Source's stored record for the same directory, and a Source with nothing stored yet is run inline, once, by the read or refresh that needs it. A called Source is kept warm while it is called. Its own failure is the caller's miss. Sources that call each other in a ring do not load; were one run anyway, it fails the read with `sources: <p>.<s> calls itself`.

A shipped provider is a file, `defaults/<name>.toml`: its invalidation, how it finds its root and what it watches are lines you can read there and override.

## comb status — source on the wire

`comb status` shows one row per field. Each row carries a `source` field in the wire protocol identifying which source produced it; SDKs expose it on the row struct. The default human-readable output is unchanged.
