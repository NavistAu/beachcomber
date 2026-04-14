---
sidebar_position: 7
---

# Polybar

Polybar polls external commands on an interval to populate status bar modules. With slow providers — git, network lookups, kubectl — each poll spawns a subprocess that adds visible latency or forces long intervals that leave your bar feeling stale. beachcomber keeps those results cached in the background, so every `comb g` query returns in microseconds regardless of what the underlying provider costs.

## Prerequisites

beachcomber is socket-activated: the daemon starts automatically on the first `comb` query and shuts down after an idle period. You do not need to launch it manually. Polybar itself will socket-activate the daemon the first time a module executes.

If you want the daemon warm before the bar appears (to avoid a one-time cache miss on the first poll), start it eagerly from your session init — for example, by calling `comb s` in `~/.xprofile` or via a systemd user unit. See the [Reference](../reference/cli-commands.md) page for the full list of commands.

## Module definitions

Config file: `~/.config/polybar/config.ini`

Each beachcomber-backed module uses `type = custom/script` with a `comb g` command. Because queries are cheap, you can set `interval = 1` or `interval = 2` across the board without worrying about CPU or latency.

Add these to your `config.ini`:

```ini
# comb g returns plain text by default. g = get, no suffix needed.
[module/git-branch]
type = custom/script
exec = comb g git.branch .
interval = 2
format = <label>
label = %output%

[module/battery]
type = custom/script
exec = comb g battery.percent
interval = 2
format = <label>
label = BAT: %output%%%

[module/load]
type = custom/script
exec = comb g load.one
interval = 2
format = <label>
label = LOAD: %output%

[module/network-ssid]
type = custom/script
exec = comb g network.ssid
interval = 5
format = <label>
label = %output%

[module/kube-context]
type = custom/script
exec = comb g kubecontext.context
interval = 5
format = <label>
label = k8s: %output%

[module/uptime]
type = custom/script
exec = comb g.f '{days}d {hours}h' uptime
interval = 60
format = <label>
label = up %output%
```

Global providers (battery, load, network, kubecontext, uptime) do not take a path argument. Path-scoped providers (git, terraform, etc.) take the target directory as a second argument — `.` means the current working directory, but since polybar runs scripts without a meaningful working directory you should pass an explicit path:

```ini
[module/git-branch]
type = custom/script
exec = comb g git.branch /home/yourname/myproject
interval = 2
```

## Referencing modules

Add the modules to the `modules-left`, `modules-center`, or `modules-right` line of your `[bar/main]` section:

```ini
[bar/main]
modules-left = git-branch kube-context
modules-center = date
modules-right = load network-ssid battery uptime
```

## Interval guidance

polybar's `interval` is in seconds. With beachcomber, setting `interval = 1` is fine — the query cost is negligible. The limiting factor is how often the underlying provider actually refreshes, which is controlled by beachcomber's config, not by polybar.

## Troubleshooting

- **Path-scoped providers return wrong data:** bar programs often run scripts without a defined working directory. `.` resolves to wherever the bar process started. Always pass an absolute path for path-scoped providers like `git` and `terraform`.
- **Distinguishing "no data" from "daemon down":** add a fallback to your exec: `comb g git.branch /path || echo '--'`. If you see `--`, the daemon is not running or the key does not exist.

See the [built-in providers reference](../reference/built-in-providers.md) for the full list of providers and fields, and the [general troubleshooting guide](./troubleshooting.md).
