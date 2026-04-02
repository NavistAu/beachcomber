---
sidebar_position: 2
title: Built-in Providers
---

# Built-in Providers

beachcomber ships 16 built-in providers organized by category.

## System

| Provider | Scope | Fields | Invalidation | Typical Latency |
|---|---|---|---|---|
| `hostname` | global | `name` (string), `short` (string) | once at startup | 400 ns |
| `user` | global | `name` (string), `uid` (int) | once at startup | 395 ns |
| `load` | global | `one` (float), `five` (float), `fifteen` (float) | poll 10s / floor 5s | 550 ns |
| `uptime` | global | `seconds` (int), `days` (int), `hours` (int), `minutes` (int) | poll 60s | 660 ns |
| `battery` | global | `percent` (int), `charging` (bool), `time_remaining` (int, seconds) | poll 30s / floor 5s | 6 ms |
| `network` | global | `interface` (string), `ip` (string), `vpn_active` (bool), `vpn_name` (string), `ssid` (string), `online` (bool) | poll 10s / floor 5s | 2 ms |

**Example output:**

```json
// comb get battery
{
  "ok": true,
  "data": { "percent": 78, "charging": false, "time_remaining": 7200 },
  "age_ms": 4200
}

// comb get network
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

// comb get load
{
  "ok": true,
  "data": { "one": 2.34, "five": 1.87, "fifteen": 1.42 },
  "age_ms": 8900
}
```

## Git

| Provider | Scope | Fields | Invalidation | Typical Latency |
|---|---|---|---|---|
| `git` | path | 21 fields (see table below) | watch `.git` + fallback poll | 5.6 ms |

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

**Example output:**

```json
// comb get git .
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
    "last_commit_age_secs": 3420
  },
  "age_ms": 234
}

// comb get git.branch . -f text
feature/fast-cache
```

## Cloud and DevOps

| Provider | Scope | Fields | Invalidation | Typical Latency |
|---|---|---|---|---|
| `kubecontext` | global | `context` (string), `namespace` (string) | poll 30s | 749 ns |
| `gcloud` | global | `project` (string), `account` (string) | poll 60s | 1.08 µs |
| `aws` | global | `profile` (string), `region` (string) | poll 60s | < 1 µs |
| `terraform` | path | `workspace` (string) | watch `.terraform/` | < 1 µs |

`kubecontext` reads `~/.kube/config` directly (respecting `$KUBECONFIG`) — no `kubectl` subprocess. `gcloud` reads `~/.config/gcloud/properties` directly — no Python CLI subprocess.

**Example output:**

```json
// comb get kubecontext
{
  "ok": true,
  "data": { "context": "prod-cluster", "namespace": "default" },
  "age_ms": 15200
}

// comb get aws
{
  "ok": true,
  "data": { "profile": "work-prod", "region": "us-east-1" },
  "age_ms": 42100
}
```

## Development Tools

| Provider | Scope | Fields | Invalidation | Typical Latency |
|---|---|---|---|---|
| `python` | path | `venv` (bool), `venv_name` (string), `version` (string) | watch `.venv/`, `pyproject.toml` | < 1 µs |
| `conda` | global | `env` (string), `version` (string) | poll 30s | < 1 µs |
| `mise` | path | `tools` (object: tool-name → version) | watch `.mise.toml`, `mise.toml` | varies |
| `asdf` | path | `tools` (object: tool-name → version) | watch `.tool-versions` | < 1 µs |
| `direnv` | path | `status` (string), `allowed` (bool) | watch `.envrc` | varies |

**Example output:**

```json
// comb get mise .
{
  "ok": true,
  "data": {
    "tools": {
      "node": "20.11.0",
      "python": "3.12.1",
      "rust": "1.75.0"
    }
  },
  "age_ms": 890
}

// comb get python .
{
  "ok": true,
  "data": { "venv": true, "venv_name": ".venv", "version": "3.12.1" },
  "age_ms": 120
}
```
