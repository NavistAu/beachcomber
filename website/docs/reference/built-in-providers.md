---
sidebar_position: 2
title: Shipped Providers
---

# Shipped Providers

beachcomber ships 16 providers organized by category, plus [template-backed namespaces](#template-backed-namespaces). Each is a file in the managed `defaults/` directory, written the way a user writes a Source; override any line of one in `config.toml` or `conf.d/`.

Each provider declares one or more **sources** — independent units of execution with their own schedule and lifecycle. The tables below show each provider's source breakdown. Consumers use `comb get provider.field` as before; the source is selected automatically by the registry's `field → source` map.

## Source breakdown by provider

| Provider | Source | Strategy | Root | Fields |
|---|---|---|---|---|
| `asdf` | `tools` | watch (`defaults/asdf.toml`) | project | one field per tool name (e.g. `nodejs`, `ruby`): the nearest `.tool-versions`' first version, then the global file's at `~` |
| `aws_profiles` | `config_file` | watch `~/.aws/config` + poll 60s (`defaults/aws.toml`) | none | one object per profile name (`default`, `staging`, …), each `{ region }` |
| `battery` | `state` | poll 30s (`defaults/battery.toml`) | none | `percent`, `charging`, `status`, `time_remaining_secs`, `status_raw` (Linux) |
| `direnv` | `state` | watch (`defaults/direnv.toml`) | project | `status`, `allowed` |
| `gcloud_configs` | `config_dir` | poll 60s (`defaults/gcloud.toml`) | none | `active_config`, plus one object per configuration name, each `{ project, account }` |
| `git` | `head` | watch (`defaults/git.toml`) | project | `branch`, `detached` |
| `git` | `refs` | watch (`defaults/git.toml`) | project | `commit`, `tag`, `ahead`, `behind`, `upstream`, `state`, `state_step`, `state_total`, `stash`, `last_commit_timestamp`, `commit_summary`, `push_ahead`, `push_behind` |
| `git` | `diff` | poll 30s (`defaults/git.toml`) | project | `lines_added`, `lines_removed`, `lines_staged_added`, `lines_staged_removed` |
| `git` | `branches` | watch on the refs (`defaults/git.toml`) | project | `branch_parent`, `branch_default` |
| `git` | `lineage` | watch on the refs (`defaults/git.toml`) | project | `lines_added_parent`, `lines_removed_parent`, `lines_added_default`, `lines_removed_default` |
| `git` | `status` | watch + poll (`defaults/git.toml`) | project | `staged`, `unstaged`, `untracked`, `conflicted`, `dirty` |
| `hostname` | `host` | poll 10m (`defaults/hostname.toml`) | none | `name`, `short` |
| `load` | `loadavg` | poll 10s (`defaults/load.toml`) | none | `one`, `five`, `fifteen` |
| `mise` | `tools` | watch (`defaults/mise.toml`) | project | tool versions, one field per tool: what `mise ls --current` resolves at the nearest `mise.toml`/`.mise.toml` directory, else at `~` |
| `network` | `interfaces` | poll 10s (`defaults/network.toml`) | none | `interface`, `ip`, `ipv6`, `vpn_active`, `vpn_name`, `ssid`, `online` |
| `python` | `venv` | watch (`defaults/python.toml`) | project | `venv`, `local_venv_name`, `venv_version` (`venv_name` and `version` are [templates](#template-backed-namespaces) over them) |
| `sudo` | `state` | poll 30s (`defaults/sudo.toml`) | none | `active` |
| `terraform` | `state` | watch (`defaults/terraform.toml`) | project | `workspace` |
| `uname` | `system` | poll 1h (`defaults/uname.toml`) | none | `sysname`, `release`, `version`, `machine` |
| `uptime` | `time` | poll 60s (`defaults/uptime.toml`) | none | `seconds`, `days`, `hours`, `minutes` |
| `user` | `name` | poll 10m (`defaults/user.toml`) | none | `name`, `uid` |

**Shipped files.** A provider whose strategy names a `defaults/` file is that file, written the way a user writes a Source: `hostname`, `user`, `uname`, `load`, `uptime`, `battery` and `network` each have one Source whose `data` line calls the compiled-in `sys` plugin, `git` is `defaults/git.toml`, whose Sources call the compiled-in `git` plugin, and `terraform`, `python`, `direnv`, `asdf`, `aws_profiles`, `gcloud_configs` and `sudo` read their files in theirs; `mise`'s runs `mise ls --current --json`. Set any of its lines in `config.toml` or `conf.d/` (`[daemon.providers.hostname.sources.host] result.name = "{{ data.name | upper }}"`) and the rest stay as shipped.

**watch + poll** sources watch files for quick invalidation and also poll on a timer as a backstop.

**Instances.** A project-rooted source has one instance per project root, plus the one at `~` for a directory with no root of its own. `git`, `direnv`, `python` and `terraform` answer from their nearest instance alone (`| nearest`): a nested repository shows only its own values, and a directory outside any repository has no git values. `mise` and `asdf` cascade: each leaf comes from the nearest instance that has it, up to `~`. Retune any of them with `[daemon.providers.<p>.sources.<s>] return = ...`, or switch the provider off with `[daemon.providers.<p>] enabled = false`.

## System

| Provider | Root | Fields | Invalidation | Typical Latency |
|---|---|---|---|---|
| `hostname` | none | `name` (string), `short` (string) | poll 10m | 400 ns |
| `user` | none | `name` (string), `uid` (int) | poll 10m | 395 ns |
| `load` | none | `one` (float), `five` (float), `fifteen` (float) | poll 10s | 550 ns |
| `uptime` | none | `seconds` (int), `days` (int), `hours` (int), `minutes` (int) | poll 60s | 660 ns |
| `battery` | none | `percent` (int), `charging` (bool), `time_remaining_secs` (int), `status` (string), `status_raw` (string, Linux) | poll 30s | 6 ms |
| `network` | none | `interface` (string), `ip` (string), `ipv6` (string), `vpn_active` (bool), `vpn_name` (string), `ssid` (string), `online` (bool) | poll 10s | 2 ms |
| `sudo` | none | `active` (bool) | poll 30s | < 1 µs |
| `uname` | none | `sysname` (string), `release` (string), `version` (string), `machine` (string) | poll 1h | < 1 µs |

**Platform notes:**

- **battery:** one Source on both platforms: `pmset -g batt` on macOS, sysfs (`/sys/class/power_supply`) on Linux. `time_remaining_secs` is absent when there is no estimate (a full battery, or none given); on Linux it is the driver's own `time_to_empty_now`/`time_to_full_now`, else the energy or charge over the rate. `status` takes one of `charging` / `discharging` / `charged` / `calculating` / `unknown`; Linux also gives `status_raw`, the kernel's own word. A machine with no battery has no `battery` fields.
- **network:** the preferred interface is an `en*` one on macOS and the default route's (`/proc/net/route`) on Linux. SSID detection uses `nmcli`/`iw` on Linux (vs `airport` on macOS, gone from macOS 14.4, where `ssid` is absent).
- **sudo:** `active` is whether the user's sudo timestamp (`/var/run/sudo/ts/$USER`, else `/run/sudo/ts/$USER`) is under five minutes old. A daemon running as the user can read it only where that directory is searchable. On Linux it is root's alone wherever sudo made it mode 0700, and on macOS sudo keeps its timestamps under `/var/db/sudo`, root's alone: there `active` is absent, not `false`.
- **uptime:** reads `/proc/uptime` on Linux (vs `sysctl` on macOS) — transparent to consumers, same fields.

**Example output:**

```json
// comb g.j battery       (-f json prints the bare value; ask for age with battery:cache.percent)
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

| Provider | Root | Fields | Invalidation | Typical Latency |
|---|---|---|---|---|
| `git` | project | 30 fields (see table below) | watch `.git` + fallback poll | `head` no process; the rest about 150 ms, 10 `git` processes, on this repository |

**Fields:**

| Field | Type | Description |
|---|---|---|
| `branch` | string | Current branch name; absent on a detached HEAD |
| `commit` | string | Short SHA of HEAD; absent before the first commit |
| `detached` | bool | Whether HEAD is detached |
| `upstream` | string | Upstream tracking branch (e.g., "origin/main"); absent when none |
| `tag` | string | Nearest tag; absent when none |
| `dirty` | bool | Whether working tree has changes |
| `staged` | int | Number of staged files |
| `unstaged` | int | Number of unstaged modified files |
| `untracked` | int | Number of untracked files |
| `conflicted` | int | Number of conflicted files |
| `ahead` | int | Commits ahead of upstream; absent without one |
| `behind` | int | Commits behind upstream; absent without one |
| `stash` | int | Number of stash entries |
| `lines_added` | int | Lines added in working tree (unstaged) |
| `lines_removed` | int | Lines removed in working tree (unstaged) |
| `lines_staged_added` | int | Lines added in index (staged) |
| `lines_staged_removed` | int | Lines removed in index (staged) |
| `state` | string | Repo state: "clean", "merge", "rebase", "cherry-pick", "bisect", "revert" |
| `state_step` | int | Current step of a rebase in progress; absent otherwise |
| `state_total` | int | Total steps of a rebase in progress; absent otherwise |
| `last_commit_timestamp` | int | When HEAD was committed (committer date), Unix seconds |
| `last_commit_age_secs` | int | Seconds since HEAD was committed, as of the read: a client template over `last_commit_timestamp` and `now()`, so it is not cached and `comb.git.last_commit_age_secs` (the raw cached value) has none |
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
  "last_commit_timestamp": 1793898717,
  "last_commit_age_secs": 3420,
  "commit_summary": "feat: add synchronous cache miss",
  "push_ahead": 2,
  "push_behind": 0
}

// comb g git.branch .    (g = get, text is the default format)
feature/fast-cache
```

## Cloud and DevOps

| Provider | Root | Fields | Invalidation | Typical Latency |
|---|---|---|---|---|
| `gcloud_configs` | none | `active_config` (string), plus one object per gcloud configuration: `{ project, account }` | poll 60s | 1.08 µs |
| `aws_profiles` | none | one object per profile in `~/.aws/config`: `{ region }` | watch `~/.aws/config` + poll 60s | < 1 µs |
| `terraform` | project | `workspace` (string) | watch `.terraform/environment` | < 1 µs |

`gcloud_configs` reads `~/.config/gcloud/configurations/` directly — no Python CLI subprocess. Both are **data providers**: they cache every profile or configuration, blind to the shell's selector, and the user-facing `aws.*` and `gcloud.*` fields are [templates](#template-backed-namespaces) that index them by `$AWS_PROFILE` / `$CLOUDSDK_ACTIVE_CONFIG_NAME`.

**Example output:**

```json
// comb g.j aws_profiles
{ "default": { "region": "us-east-1" }, "work-prod": { "region": "eu-west-1" } }

// AWS_PROFILE=work-prod comb g aws.region       (a template over aws_profiles)
eu-west-1
```

## Template-backed namespaces

A `comb g` namespace need not have a Source behind it: a provider named only in `[client.providers]` has templates and no Sources. The managed `defaults/` files the daemon writes to `~/.config/beachcomber/defaults/` declare templates under `[client.providers.<namespace>] template.*`; the daemon serves them as data and the client renders them with the caller's environment (see [Configuration Reference — Templates](/docs/reference/configuration-reference#templates-and-value-expressions)). Override one field by setting the same key in `config.toml` or `conf.d/`.

| Namespace | Default file | Fields | Renders from |
|---|---|---|---|
| `aws` | `defaults/aws.toml` | `profile`, `region`, `expiration` | `$AWS_PROFILE`/`$AWS_VAULT`/`$AWS_DEFAULT_PROFILE`, `$AWS_REGION`, `$AWS_CREDENTIAL_EXPIRATION`, falling back to `comb.aws_profiles[<profile>].region` |
| `gcloud` | `defaults/gcloud.toml` | `project`, `account` | `$CLOUDSDK_CORE_PROJECT`, `$CLOUDSDK_ACTIVE_CONFIG_NAME`, falling back to `comb.gcloud_configs[<active>]` |
| `python` | `defaults/python.toml` | `version`, `venv_name` | `$PYENV_VERSION`/`$MISE_PYTHON_VERSION`, `comb.mise.python`, `comb.asdf.python`, `comb.python.venv_version`; `comb.python.local_venv_name`, `$VIRTUAL_ENV` |
| `terraform` | `defaults/terraform.toml` | `workspace` | `$TF_WORKSPACE`, falling back to the cached `comb.terraform.workspace` — a template at a cached field's path wins on read |
| `conda` | `defaults/conda.toml` | `env` | `$CONDA_DEFAULT_ENV` |
| `op` | `defaults/op.toml` | `service_account` (bool) | whether `$OP_SERVICE_ACCOUNT_TOKEN` is set — never the token itself |
| `kubecontext` | `defaults/kubecontext.toml` | `context`, `namespace` | `exec('kubectl config …')`, memoised: re-run when `$KUBECONFIG` changes, when a kubeconfig file (`$KUBECONFIG`'s colon list, else `~/.kube/config`) changes, or every 10 minutes |
| `talos` | `defaults/talos.toml` | `context` | `exec('talosctl config info -o json …')`, memoised: re-run when `$TALOSCONFIG` changes, when the talos config file (`$TALOSCONFIG`, else `~/.talos/config`) changes, or every 10 minutes |

Only `python` and `terraform` have a daemon provider of the same name, so only there does `comb g <namespace>` return cached fields deep-merged with rendered ones. `aws` and `gcloud` are templates only: their data lives in the differently named `aws_profiles` and `gcloud_configs` namespaces, which the templates reach through `comb.*` refs, and `comb g aws` is just the rendered template tree. `conda`, `op`, `kubecontext` and `talos` are templates only too, over the environment or a command. `kubecontext` and `talos` call the tool the shell would run, so they honour the shell's own `$KUBECONFIG`/`$TALOSCONFIG`, and the field names are unchanged — `comb g kubecontext.context` still prints the active context. Both memoise their command (`watch() | ttl('10m') | cache()`; see [Caching a template](configuration-reference.md)), so a warm read, and a warm `comb status`, spawns nothing, and "no context" is remembered too. Because a cold read spawns a process, they render only where `exec()` is enabled: `comb get`/`comb eval`, or an SDK client constructed with `exec` on. Anywhere else the field is the error `exec() is disabled in this context`, and when `kubectl` itself fails (no current context) `comb g kubecontext.context` exits `2` with its error rather than printing nothing.

## Development Tools

| Provider | Root | Fields | Invalidation | Typical Latency |
|---|---|---|---|---|
| `python` | project | `venv` (bool), `local_venv_name` (string), `venv_version` (string) — plus the `venv_name` and `version` [templates](#template-backed-namespaces) | watch `.venv`, `pyproject.toml` | < 1 µs |
| `mise` | project | one string field per tool name (e.g. `node`, `rust`) — one `tools` source: what mise resolves at the project, global tools included; outside a project, at `~` | watch `.mise.toml`, `mise.toml` | varies |
| `asdf` | project | one string field per tool name (e.g. `nodejs`, `ruby`) — the project's `.tool-versions`, then the global one's at `~` | watch `.tool-versions` | < 1 µs |
| `direnv` | project | `status` (string: `allowed`, `blocked` or `denied`), `allowed` (bool) | watch `.envrc` | varies |

**Example output:**

```json
// comb g.j mise .            (project tools for the current directory, merged over the global set)
{ "node": "20.11.0", "python": "3.12.1", "rust": "1.75.0" }

// comb g.j --global mise     (global tools only)
{ "rust": "1.75.0" }

// comb g.j python .          (cached fields plus the two rendered templates)
{ "venv": true, "local_venv_name": ".venv", "venv_version": "3.12.1", "venv_name": ".venv", "version": "3.12.1" }
```
