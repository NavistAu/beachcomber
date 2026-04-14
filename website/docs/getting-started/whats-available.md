---
sidebar_position: 3
title: What's Available
---

# What's Available

beachcomber ships with 19 built-in providers out of the box — no configuration required. Each one exposes a small set of fields under a namespace.

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
| `op` | global | 1Password CLI signed-in state | `comb g op.signed_in` |

## Git

| Provider | Scope | What it tells you | Example |
|---|---|---|---|
| `git` | path | Branch, dirty, staged, ahead/behind, stash, lines, state (24 fields) | `comb g git.branch .` |

## Cloud and DevOps

| Provider | Scope | What it tells you | Example |
|---|---|---|---|
| `kubecontext` | global | Active kube context and namespace | `comb g kubecontext.context` |
| `gcloud` | global | Active gcloud project and account | `comb g gcloud.project` |
| `aws` | global | Active AWS profile and region | `comb g aws.profile` |
| `terraform` | path | Current workspace | `comb g terraform.workspace .` |

## Development tools

| Provider | Scope | What it tells you | Example |
|---|---|---|---|
| `python` | path | Virtualenv presence, name, version | `comb g python.venv .` |
| `conda` | global | Active conda env and version | `comb g conda.env` |
| `mise` | path | Tool versions in effect | `comb g mise .` |
| `asdf` | path | Tool versions in effect | `comb g asdf .` |
| `direnv` | path | envrc status and whether it's allowed | `comb g direnv.status .` |

## Scope at a glance

- **global** — one value for the whole machine. No path argument.
- **path** — the answer depends on which directory you're asking about. Pass `.` (current directory) or an absolute path as the second argument.

Not enough? You can add your own provider — a shell script, a REST endpoint, or a shared library. See [Custom Providers](../reference/custom-providers.md).
