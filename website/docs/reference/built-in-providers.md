---
sidebar_position: 2
title: Built-in Providers
---

# Built-in Providers

beachcomber ships 19 built-in providers organized by category.

Each provider declares one or more **sources** — independent units of execution with their own invalidation strategy and lifecycle. The tables below show each provider's source breakdown. Consumers use `comb get provider.field` as before; the source is selected automatically by the registry's `field → source` map.

## Source breakdown by provider

| Provider | Source | Strategy | Scope | Fields |
|---|---|---|---|---|
| `asdf` | `tools` | fsevent | path | tool versions (one field per tool) |
| `aws` | `profile` | poll 60s | global | `profile`, `region`, `source`, `expiration` |
| `battery` | `state` | poll 30s | global | `percent`, `charging`, `status`, `time_remaining_secs` (macOS) |
| `battery` | `level` | poll 30s | global | `percent`, `charging`, `status_raw` (Linux) |
| `battery` | `upower` | poll 60s | global | `time_remaining_secs`, `status` (Linux, requires UPower) |
| `conda` | `env` | poll 30s | global | `env` |
| `direnv` | `state` | fsevent | path | `status`, `allowed` |
| `gcloud` | `config` | poll 60s | global | `project`, `account` |
| `git` | `refs` | fsevent | path | `branch`, `commit`, `tag`, `ahead`, `behind`, `upstream`, `detached`, `state`, `stash` |
| `git` | `diff` | poll 30s | path | `lines_added`, `lines_removed`, `lines_staged_added`, `lines_staged_removed` |
| `git` | `status` | fsevent_poll | path | `staged`, `unstaged`, `untracked`, `conflicted`, `dirty` |
| `hostname` | `host` | fsevent (pure-watch global) | global | `name`, `short` |
| `kubecontext` | `context` | poll 30s | global | `context`, `namespace` |
| `load` | `loadavg` | poll 10s | global | `one`, `five`, `fifteen` |
| `mise` | `global` | fsevent (pure-watch global) | global | global tool versions (one field per tool) |
| `mise` | `project` | fsevent | path | project tool versions (one field per tool) |
| `network` | `interfaces` | poll 10s | global | `interface`, `ip`, `vpn_active`, `vpn_name`, `ssid`, `online` |
| `op` | `vault` | poll 60s | global | `signed_in`, `account` |
| `python` | `venv` | fsevent | path | `venv`, `venv_name`, `version` |
| `sudo` | `state` | poll 30s | global | `active` |
| `terraform` | `state` | fsevent | path | `workspace` |
| `uname` | `system` | fsevent (pure-watch global) | global | `sysname`, `release`, `version`, `machine` |
| `uptime` | `time` | poll 60s | global | `seconds`, `days`, `hours`, `minutes` |
| `user` | `name` | fsevent (pure-watch global) | global | `name`, `uid` |

**Pure-watch global** sources execute once on first demand and re-execute only when a registered filesystem path changes. They never decay.

**fsevent_poll** sources watch files for quick invalidation and also poll on a timer as a backstop.

## System

| Provider | Scope | Fields | Invalidation | Typical Latency |
|---|---|---|---|---|
| `hostname` | global | `name` (string), `short` (string) | once at startup | 400 ns |
| `user` | global | `name` (string), `uid` (int) | once at startup | 395 ns |
| `load` | global | `one` (float), `five` (float), `fifteen` (float) | poll 10s / floor 5s | 550 ns |
| `uptime` | global | `seconds` (int), `days` (int), `hours` (int), `minutes` (int) | poll 60s | 660 ns |
| `battery` | global | `percent` (int), `charging` (bool), `time_remaining_secs` (int), `status` (string) | poll 30s / floor 5s | 6 ms |
| `network` | global | `interface` (string), `ip` (string), `vpn_active` (bool), `vpn_name` (string), `ssid` (string), `online` (bool) | poll 10s / floor 5s | 2 ms |
| `sudo` | global | `active` (bool) | poll 30s | < 1 µs |
| `op` | global | `signed_in` (bool), `account` (string) | poll 60s | varies |

**Platform notes:**

- **battery:** `time_remaining_secs` is `0` when the platform can't compute an estimate (battery charged, or state unknown). `status` takes one of `charging` / `discharging` / `charged` / `calculating` / `unknown`. On Linux, the numeric estimate requires UPower; without it, `status` is `calculating`.
- **network:** SSID detection uses `nmcli`/`iw` on Linux (vs `airport` on macOS).
- **uptime:** reads `/proc/uptime` on Linux (vs `sysctl` on macOS) — transparent to consumers, same fields.

**Example output:**

```json
// comb g battery
{
  "ok": true,
  "data": { "percent": 78, "charging": false, "time_remaining_secs": 7200, "status": "discharging" },
  "age_ms": 4200
}

// comb g network
{
  "ok": true,
  "data": {
    "interface": "en0",
    "ip": "192.168.1.42",
    "vpn_active": true,
    "vpn_name": "utun2",
    "ssid": "OfficeNet",
    "online": true
  },
  "age_ms": 3100
}

// comb g load
{
  "ok": true,
  "data": { "one": 2.34, "five": 1.87, "fifteen": 1.42 },
  "age_ms": 8900
}
```

## Git

| Provider | Scope | Fields | Invalidation | Typical Latency |
|---|---|---|---|---|
| `git` | path | 24 fields (see table below) | watch `.git` + fallback poll | 5.6 ms |

**Fields:**

| Field | Type | Description |
|---|---|---|
| `branch` | string | Current branch name |
| `commit` | string | Short SHA of HEAD |
| `detached` | bool | Whether HEAD is detached |
| `upstream` | string | Upstream tracking branch (e.g., "origin/main") |
| `tag` | string | Nearest tag (empty if none) |
| `dirty` | bool | Whether working tree has changes |
| `staged` | int | Number of staged files |
| `unstaged` | int | Number of unstaged modified files |
| `untracked` | int | Number of untracked files |
| `conflicted` | int | Number of conflicted files |
| `ahead` | int | Commits ahead of upstream |
| `behind` | int | Commits behind upstream |
| `stash` | int | Number of stash entries |
| `lines_added` | int | Lines added in working tree (unstaged) |
| `lines_removed` | int | Lines removed in working tree (unstaged) |
| `lines_staged_added` | int | Lines added in index (staged) |
| `lines_staged_removed` | int | Lines removed in index (staged) |
| `state` | string | Repo state: "clean", "merge", "rebase", "cherry-pick", "bisect", "revert" |
| `state_step` | int | Current step in rebase/cherry-pick (0 if not in progress) |
| `state_total` | int | Total steps in rebase/cherry-pick (0 if not in progress) |
| `last_commit_age_secs` | int | Seconds since last commit |
| `commit_summary` | string | First line of the HEAD commit message |
| `push_ahead` | int | Commits ahead of the push remote |
| `push_behind` | int | Commits behind the push remote |

**Example output:**

```json
// comb g git .
{
  "ok": true,
  "data": {
    "branch": "feature/fast-cache",
    "commit": "a1b2c3d",
    "detached": false,
    "upstream": "origin/main",
    "tag": "v0.3.1",
    "dirty": true,
    "staged": 3,
    "unstaged": 1,
    "untracked": 0,
    "conflicted": 0,
    "ahead": 2,
    "behind": 0,
    "stash": 1,
    "lines_added": 47,
    "lines_removed": 12,
    "lines_staged_added": 23,
    "lines_staged_removed": 5,
    "state": "clean",
    "state_step": 0,
    "state_total": 0,
    "last_commit_age_secs": 3420,
    "commit_summary": "feat: add synchronous cache miss",
    "push_ahead": 2,
    "push_behind": 0
  },
  "age_ms": 234
}

// comb g git.branch .    (g = get, text is the default format)
feature/fast-cache
```

## Cloud and DevOps

| Provider | Scope | Fields | Invalidation | Typical Latency |
|---|---|---|---|---|
| `kubecontext` | global | `context` (string), `namespace` (string) | poll 30s | 749 ns |
| `gcloud` | global | `project` (string), `account` (string) | poll 60s | 1.08 µs |
| `aws` | global | `profile` (string), `region` (string), `source` (string), `expiration` (string) | poll 60s | < 1 µs |
| `terraform` | path | `workspace` (string) | watch `.terraform/` | < 1 µs |

`kubecontext` reads `~/.kube/config` directly (respecting `$KUBECONFIG`) — no `kubectl` subprocess. `gcloud` reads `~/.config/gcloud/properties` directly — no Python CLI subprocess.

**Example output:**

```json
// comb g kubecontext
{
  "ok": true,
  "data": { "context": "prod-cluster", "namespace": "default" },
  "age_ms": 15200
}

// comb g aws
{
  "ok": true,
  "data": { "profile": "work-prod", "region": "us-east-1", "source": "profile", "expiration": "2026-05-30T12:00:00Z" },
  "age_ms": 42100
}
```

## Development Tools

| Provider | Scope | Fields | Invalidation | Typical Latency |
|---|---|---|---|---|
| `python` | path | `venv` (bool), `venv_name` (string), `version` (string) | watch `.venv/`, `pyproject.toml` | < 1 µs |
| `conda` | global | `env` (string) | poll 30s | < 1 µs |
| `mise` | path | one string field per tool name (e.g. `node`, `rust`) | watch `.mise.toml`, `mise.toml` | varies |
| `asdf` | path | `tools` (object) | watch `.tool-versions` | < 1 µs |
| `direnv` | path | `status` (string), `allowed` (bool) | watch `.envrc` | varies |

**Example output:**

```json
// comb g mise .              (project tools for the current directory)
{
  "ok": true,
  "data": { "node": "20.11.0", "python": "3.12.1" },
  "age_ms": 890
}

// comb g mise               (global tools — no path context)
{
  "ok": true,
  "data": { "rust": "1.75.0" },
  "age_ms": 890
}

// comb g python .
{
  "ok": true,
  "data": { "venv": true, "venv_name": ".venv", "version": "3.12.1" },
  "age_ms": 120
}
```
