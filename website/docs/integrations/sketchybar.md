---
sidebar_position: 9
---

# Sketchybar

Sketchybar is a macOS status bar replacement that runs scripts on a timer to update item labels. With slow providers — git, kubectl, network — each poll adds latency or forces long update intervals. beachcomber keeps those results cached in the background, so every `comb g` query returns in microseconds regardless of what the underlying provider costs.

## Prerequisites

beachcomber is socket-activated: the daemon starts automatically on the first `comb` query and shuts down after an idle period. You do not need to launch it manually. Sketchybar will socket-activate the daemon the first time a script runs.

If you want the daemon warm before the bar appears (to avoid a one-time cache miss on the first poll), start it eagerly from your session init — for example, by calling `comb s` in a launchd plist. See the [Reference](../reference/cli-commands.md) page for the full list of commands.

## Basic items with polling

Config file: `~/.config/sketchybar/sketchybarrc`

Add these to your `sketchybarrc`:

```sh
# Battery
sketchybar --add item battery right \
           --set battery \
               update_freq=5 \
               script='sketchybar --set battery label="BAT: $(comb g battery.percent)%"'

# Git branch (path-scoped — pass the repo path explicitly)
sketchybar --add item git_branch left \
           --set git_branch \
               update_freq=2 \
               script='sketchybar --set git_branch label="$(comb g git.branch "$HOME/myproject")"'

# Network SSID
sketchybar --add item network right \
           --set network \
               update_freq=10 \
               script='sketchybar --set network label="$(comb g network.ssid)"'
```

## Using --subscribe for event-driven updates

For providers where polling is wasteful, use `--subscribe` to trigger on system events. This requires a helper script. Create `~/.config/sketchybar/plugins/battery.sh`:

```sh
#!/bin/sh
sketchybar --set "$NAME" label="BAT: $(comb g battery.percent)%"
```

Make it executable:

```sh
chmod +x ~/.config/sketchybar/plugins/battery.sh
```

Register the item and subscribe it to power events:

```sh
sketchybar --add item battery right \
           --set battery \
               update_freq=60 \
               script="$HOME/.config/sketchybar/plugins/battery.sh" \
           --subscribe battery power_source_change system_woke
```

This updates on power source changes and system wake, with a 60-second fallback poll. The script itself stays fast because it reads from the beachcomber cache.

## Multiple items example

A complete right-side cluster:

```sh
# Items are displayed right-to-left when added to the right side
sketchybar --add item battery right \
           --set battery \
               update_freq=5 \
               script='sketchybar --set battery label="$(comb g battery.percent)%"'

sketchybar --add item network right \
           --set network \
               update_freq=10 \
               script='sketchybar --set network label="$(comb g network.ssid)"'

sketchybar --add item kube right \
           --set kube \
               update_freq=5 \
               script='sketchybar --set kube label="$(comb g kubecontext.context)"'

sketchybar --add item git_branch left \
           --set git_branch \
               update_freq=2 \
               script='sketchybar --set git_branch label="$(comb g git.branch "$HOME/myproject")"'
```

## Troubleshooting

- **Path-scoped providers return wrong data:** sketchybar runs scripts without a defined working directory. Always pass an absolute path for path-scoped providers like `git` and `terraform` — use `$HOME/myproject` rather than a relative path.
- **Distinguishing "no data" from "daemon down":** add a fallback in your script: `comb g git.branch "$HOME/myproject" || echo '--'`. If you see `--`, the daemon is not running or the key does not exist.

See the [built-in providers reference](../reference/built-in-providers.md) for the full list of providers and fields, and the [general troubleshooting guide](./troubleshooting.md).
