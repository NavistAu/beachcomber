---
sidebar_position: 3
title: What's Available
---

# What's Available

beachcomber ships with 16 built-in providers out of the box — no configuration required. Each one exposes a small set of fields under a namespace.

The table below gives a one-line summary of each. See the [Built-in Providers reference](../reference/built-in-providers.md) for every field, type, and default polling cadence.

## System

| Provider | Scope | What it tells you | Example |
|---|---|---|---|
| `hostname` | global | Machine hostname and short name | `comb g hostname.short` |
| `user` | global | Current user name and uid | `comb g user.name` |
| `load` | global | 1/5/15-minute load averages | `comb g load.one` |
| `uptime` | global | Seconds, minutes, hours, days since boot | `comb g uptime.days` |
| `battery` | global | Percent, charging state, time remaining | `comb g battery.percent` |
| `network` | global | Interface, IP, SSID, VPN state, online | `comb g network.ssid` |
| `sudo` | global | Whether sudo is currently cached | `comb g sudo.active` |
| `uname` | global | Kernel name, release, version, machine | `comb g uname.machine` |

## Git

| Provider | Scope | What it tells you | Example |
|---|---|---|---|
| `git` | path | Branch, dirty, staged, ahead/behind, stash, lines, state, lineage (30 fields) | `comb g git.branch .` |

## Cloud and DevOps

| Provider | Scope | What it tells you | Example |
|---|---|---|---|
| `gcloud_configs` | global | Every gcloud configuration's project and account, and which is active | `comb g gcloud_configs.active_config` |
| `aws_profiles` | global | Every profile's region from `~/.aws/config` | `comb g aws_profiles` |
| `terraform` | path | Current workspace | `comb g terraform.workspace .` |

## Development tools

| Provider | Scope | What it tells you | Example |
|---|---|---|---|
| `python` | path | Virtualenv presence, name, version | `comb g python.venv .` |
| `mise` | path | Tool versions in effect | `comb g mise .` |
| `asdf` | path | Tool versions in effect | `comb g asdf .` |
| `direnv` | path | envrc status and whether it's allowed | `comb g direnv.status .` |

## Template-backed namespaces

Some of the most useful keys are not daemon providers but **templates**: value expressions in the managed `defaults/` files the daemon writes to `~/.config/beachcomber/defaults/`, served as data and rendered by `comb g` with your shell's environment. They read the same way — `comb g aws.region` — and you can override any one field in your own config.

| Namespace | What it tells you | Example |
|---|---|---|
| `aws` | Profile, region and credential expiry in effect — `$AWS_PROFILE`/`$AWS_REGION` first, else the profile's region from `aws_profiles` | `comb g aws.region` |
| `gcloud` | Project and account in effect — `$CLOUDSDK_*` first, else the active configuration from `gcloud_configs` | `comb g gcloud.project` |
| `python` | Interpreter version and venv name in effect — `$PYENV_VERSION`, mise, asdf, then the venv | `comb g python.version .` |
| `terraform` | Workspace in effect — `$TF_WORKSPACE`, else the cached workspace | `comb g terraform.workspace .` |
| `conda` | Active conda env (`$CONDA_DEFAULT_ENV`) | `comb g conda.env` |
| `op` | Whether a 1Password service-account token is in the shell | `comb g op.service_account` |
| `kubecontext` | Current kubectl context and namespace, via `kubectl` | `comb g kubecontext.context` |
| `talos` | Current talosctl context, via `talosctl` | `comb g talos.context` |

`kubecontext` and `talos` spawn a process through `exec()`, so they render only where `exec()` is enabled (`comb get`/`eval`, or an SDK client with `exec` on). See the [Built-in Providers reference](../reference/built-in-providers.md#template-backed-namespaces).

## Scope at a glance

- **global** — one value for the whole machine. No path argument.
- **path** — the answer depends on which directory you're asking about. Pass `.` (current directory) or an absolute path as the second argument.

Not enough? You can add your own provider — a shell script, a REST endpoint, or a shared library plugin. See [Custom Providers](../reference/custom-providers.md).
