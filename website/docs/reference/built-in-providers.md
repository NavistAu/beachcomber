---
sidebar_position: 2
title: Built-in Providers
---

# Built-in Providers

beachcomber ships 16 built-in providers organized by category, plus [template-backed namespaces](#template-backed-namespaces) defined in the managed `defaults/` files.

Each provider declares one or more **sources** — independent units of execution with their own invalidation strategy and lifecycle. The tables below show each provider's source breakdown. Consumers use `comb get provider.field` as before; the source is selected automatically by the registry's `field → source` map.

## Source breakdown by provider

| Provider | Source | Strategy | Scope | Fields |
|---|---|---|---|---|
| `asdf` | `tools` | fsevent | path | `tools` (object keyed by tool name) |
| `aws_profiles` | `config_file` | fsevent_poll 60s (`~/.aws/config`) | global | one object per profile name (`default`, `staging`, …), each `{ region }` |
| `battery` | `state` | poll 30s | global | `percent`, `charging`, `status`, `time_remaining_secs` (macOS) |
| `battery` | `level` | poll 30s | global | `percent`, `charging`, `status_raw` (Linux) |
| `battery` | `upower` | poll 60s | global | `time_remaining_secs`, `status` (Linux, requires UPower) |
| `direnv` | `state` | fsevent | path | `status`, `allowed` |
| `gcloud_configs` | `config_dir` | poll 60s | global | `active_config`, plus one object per configuration name, each `{ project, account }` |
| `git` | `head` | fsevent | path | `branch`, `detached` |
| `git` | `refs` | fsevent | path | `commit`, `tag`, `ahead`, `behind`, `upstream`, `state`, `state_step`, `state_total`, `stash`, `last_commit_age_secs`, `commit_summary`, `push_ahead`, `push_behind` |
| `git` | `diff` | poll 30s | path | `lines_added`, `lines_removed`, `lines_staged_added`, `lines_staged_removed` |
| `git` | `lineage` | poll 60s | path | `branch_parent`, `lines_added_parent`, `lines_removed_parent`, `branch_default`, `lines_added_default`, `lines_removed_default` |
| `git` | `status` | fsevent_poll | path | `staged`, `unstaged`, `untracked`, `conflicted`, `dirty` |
| `hostname` | `host` | fsevent (pure-watch global) | global | `name`, `short` |
| `load` | `loadavg` | poll 10s | global | `one`, `five`, `fifteen` |
| `mise` | `global` | fsevent (pure-watch global) | global | global tool versions (one field per tool) |
| `mise` | `project` | fsevent | path | project tool versions (one field per tool) |
| `network` | `interfaces` | poll 10s | global | `interface`, `ip`, `vpn_active`, `vpn_name`, `ssid`, `online` |
| `python` | `venv` | fsevent | path | `venv`, `local_venv_name`, `venv_version` (`venv_name` and `version` are [templates](#template-backed-namespaces) over them) |
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
| `load` | global | `one` (float), `five` (float), `fifteen` (float) | poll 10s | 550 ns |
| `uptime` | global | `seconds` (int), `days` (int), `hours` (int), `minutes` (int) | poll 60s | 660 ns |
| `battery` | global | `percent` (int), `charging` (bool), `time_remaining_secs` (int), `status` (string) | poll 30s | 6 ms |
| `network` | global | `interface` (string), `ip` (string), `vpn_active` (bool), `vpn_name` (string), `ssid` (string), `online` (bool) | poll 10s | 2 ms |
| `sudo` | global | `active` (bool) | poll 30s | < 1 µs |
| `uname` | global | `sysname` (string), `release` (string), `version` (string), `machine` (string) | once at startup | < 1 µs |

**Platform notes:**

- **battery:** `time_remaining_secs` is `0` when the platform can't compute an estimate (battery charged, or state unknown). `status` takes one of `charging` / `discharging` / `charged` / `calculating` / `unknown`. On Linux, the numeric estimate requires UPower; without it, `status` is `calculating`.
- **network:** SSID detection uses `nmcli`/`iw` on Linux (vs `airport` on macOS).
- **uptime:** reads `/proc/uptime` on Linux (vs `sysctl` on macOS) — transparent to consumers, same fields.

**Example output:**

```json
// comb g.j battery       (-f json prints the bare value; ask for age with battery:age)
{ "percent": 78, "charging": false, "time_remaining_secs": 7200, "status": "discharging" }

// comb g.j network
{
  "interface": "en0",
  "ip": "192.168.1.42",
  "vpn_active": true,
  "vpn_name": "utun2",
  "ssid": "OfficeNet",
  "online": true
}

// comb g.j load
{ "one": 2.34, "five": 1.87, "fifteen": 1.42 }
```

## Git

| Provider | Scope | Fields | Invalidation | Typical Latency |
|---|---|---|---|---|
| `git` | path | 30 fields (see table below) | watch `.git` + fallback poll | 5.6 ms |

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
| `branch_parent` | string | Branch this branch was forked from, per its reflog; the default branch when the reflog names none |
| `lines_added_parent` | int | Lines added on this branch since it forked from `branch_parent` |
| `lines_removed_parent` | int | Lines removed on this branch since it forked from `branch_parent` |
| `branch_default` | string | Default branch, from `origin/HEAD` or a local `main`/`master` |
| `lines_added_default` | int | Lines added on this branch since it diverged from `branch_default` |
| `lines_removed_default` | int | Lines removed on this branch since it diverged from `branch_default` |

The `*_parent` / `*_default` counts come from `git diff --numstat <merge-base>..HEAD`, so they
describe committed divergence in the commit graph — not the working tree, which
`lines_added` / `lines_removed` cover. The merge base is `git merge-base --fork-point`,
falling back to a plain `git merge-base` when the reference branch's reflog has been
pruned. When `branch_default` is discovered through `origin/HEAD` the comparison is against
`origin/<name>`, so a checkout sitting on the default branch still reports the work it
has not pushed. All six fields are absent on a detached HEAD, and the `*_default` fields are
absent when no default branch can be detected.

**Example output:**

```json
// comb g.j git .
{
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
}

// comb g git.branch .    (g = get, text is the default format)
feature/fast-cache
```

## Cloud and DevOps

| Provider | Scope | Fields | Invalidation | Typical Latency |
|---|---|---|---|---|
| `gcloud_configs` | global | `active_config` (string), plus one object per gcloud configuration: `{ project, account }` | poll 60s | 1.08 µs |
| `aws_profiles` | global | one object per profile in `~/.aws/config`: `{ region }` | watch `~/.aws/config` + poll 60s | < 1 µs |
| `terraform` | path | `workspace` (string) | watch `.terraform/environment` | < 1 µs |

`gcloud_configs` reads `~/.config/gcloud/configurations/` directly — no Python CLI subprocess. Both are **data providers**: they cache every profile or configuration, blind to the shell's selector, and the user-facing `aws.*` and `gcloud.*` fields are [templates](#template-backed-namespaces) that index them by `$AWS_PROFILE` / `$CLOUDSDK_ACTIVE_CONFIG_NAME`.

**Example output:**

```json
// comb g.j aws_profiles
{ "default": { "region": "us-east-1" }, "work-prod": { "region": "eu-west-1" } }

// AWS_PROFILE=work-prod comb g aws.region       (a template over aws_profiles)
eu-west-1
```

## Template-backed namespaces

Not every `comb g` namespace is a daemon provider. The managed `defaults/` files the daemon writes to `~/.config/beachcomber/defaults/` declare templates under `[providers."<namespace>:template"]`; the daemon serves them as data and the client renders them with the caller's environment (see [Configuration Reference — Templates](/docs/reference/configuration-reference#templates-and-value-expressions)). Override one field by setting the same key in `config.toml` or `conf.d/`.

| Namespace | Default file | Fields | Renders from |
|---|---|---|---|
| `aws` | `defaults/aws.toml` | `profile`, `region`, `expiration` | `$AWS_PROFILE`/`$AWS_VAULT`/`$AWS_DEFAULT_PROFILE`, `$AWS_REGION`, `$AWS_CREDENTIAL_EXPIRATION`, falling back to `comb.aws_profiles[<profile>].region` |
| `gcloud` | `defaults/gcloud.toml` | `project`, `account` | `$CLOUDSDK_CORE_PROJECT`, `$CLOUDSDK_ACTIVE_CONFIG_NAME`, falling back to `comb.gcloud_configs[<active>]` |
| `python` | `defaults/python.toml` | `version`, `venv_name` | `$PYENV_VERSION`/`$MISE_PYTHON_VERSION`, `comb.mise.python`, `comb.asdf.python`, `comb.python.venv_version`; `comb.python.local_venv_name`, `$VIRTUAL_ENV` |
| `terraform` | `defaults/terraform.toml` | `workspace` | `$TF_WORKSPACE`, falling back to the cached `comb.terraform.workspace` — a template at a cached field's path wins on read |
| `conda` | `defaults/conda.toml` | `env` | `$CONDA_DEFAULT_ENV` |
| `op` | `defaults/op.toml` | `signed_in` (bool) | whether `$OP_SERVICE_ACCOUNT_TOKEN` is set — never the token itself |
| `kubecontext` | `defaults/kubecontext.toml` | `context`, `namespace` | `exec('kubectl', 'config', …)` |
| `talos` | `defaults/talos.toml` | `context` | `exec('talosctl', 'config', 'info', …)` |

Only `python` and `terraform` have a daemon provider of the same name, so only there does `comb g <namespace>` return cached fields deep-merged with rendered ones. `aws` and `gcloud` are templates only: their data lives in the differently named `aws_profiles` and `gcloud_configs` namespaces, which the templates reach through `cache.*` refs, and `comb g aws` is just the rendered template tree. `conda`, `op`, `kubecontext` and `talos` are templates only too, over the environment or a command. `kubecontext` and `talos` call the tool the shell would run, so they honour the shell's own `$KUBECONFIG`/`$TALOSCONFIG`, and the field names are unchanged — `comb g kubecontext.context` still prints the active context. Because they spawn a process, they render only where `exec()` is enabled: `comb get`/`comb eval`, or an SDK client constructed with `exec` on. Anywhere else the field is the error `exec() is disabled in this context`, and when `kubectl` itself fails (no current context) `comb g kubecontext.context` exits `2` with its error rather than printing nothing.

## Development Tools

| Provider | Scope | Fields | Invalidation | Typical Latency |
|---|---|---|---|---|
| `python` | path | `venv` (bool), `local_venv_name` (string), `venv_version` (string) — plus the `venv_name` and `version` [templates](#template-backed-namespaces) | watch `.venv`, `pyproject.toml` | < 1 µs |
| `mise` | global/path | one string field per tool name (e.g. `node`, `rust`) — `global` source (Global scope) and `project` source (PathScoped) | watch `.mise.toml`, `mise.toml` | varies |
| `asdf` | path | `tools` (object keyed by tool name) | watch `.tool-versions` | < 1 µs |
| `direnv` | path | `status` (string), `allowed` (bool) | watch `.envrc` | varies |

**Example output:**

```json
// comb g.j mise .            (project tools for the current directory, merged over the global set)
{ "node": "20.11.0", "python": "3.12.1", "rust": "1.75.0" }

// comb g.j --global mise     (global tools only)
{ "rust": "1.75.0" }

// comb g.j python .          (cached fields plus the two rendered templates)
{ "venv": true, "local_venv_name": ".venv", "venv_version": "3.12.1", "venv_name": ".venv", "version": "3.12.1" }
```
