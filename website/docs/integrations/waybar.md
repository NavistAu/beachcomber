---
sidebar_position: 8
---

# Waybar

Waybar polls external commands on an interval to populate status bar modules. With slow providers — git, network lookups, kubectl — each poll adds latency or forces long intervals that leave your bar feeling stale. beachcomber keeps those results cached in the background, so every `comb g` query returns in microseconds regardless of what the underlying provider costs.

## Prerequisites

beachcomber is socket-activated: the daemon starts automatically on the first `comb` query and shuts down after an idle period. You do not need to launch it manually. Waybar itself will socket-activate the daemon the first time a module executes.

If you want the daemon warm before the bar appears (to avoid a one-time cache miss on the first poll), start it eagerly from your session init — for example, by calling `comb s` in a systemd user unit. See the [Reference](../reference/cli-commands.md) page for the full list of commands.

## Module configuration

Config file: `~/.config/waybar/config`

waybar uses JSON for module configuration and supports `custom/*` modules that run arbitrary commands. Each module has its own `interval` field in seconds.

Add these entries to your waybar config object:

```json
"custom/git-branch": {
    "exec": "comb g git.branch /home/yourname/myproject",
    "interval": 2,
    "format": " {}",
    "tooltip": false
},
"custom/battery": {
    "exec": "comb g battery.percent",
    "interval": 2,
    "format": " {}%",
    "tooltip": false
},
"custom/load": {
    "exec": "comb g load.one",
    "interval": 2,
    "format": " {}",
    "tooltip": false
},
"custom/network": {
    "exec": "comb g network.ssid",
    "interval": 5,
    "format": " {}",
    "tooltip": false
},
"custom/kube": {
    "exec": "comb g kubecontext.context",
    "interval": 5,
    "format": " {}",
    "tooltip": false
}
```

Add them to your `modules-left`, `modules-center`, or `modules-right` arrays:

```json
"modules-right": ["custom/kube", "custom/load", "custom/network", "custom/battery"]
```

## CSS selectors

waybar generates CSS class names from module names by replacing `/` with `-`. Target your custom modules in `~/.config/waybar/style.css`:

```css
#custom-git-branch {
    color: #a6e3a1;
    padding: 0 8px;
}

#custom-battery {
    color: #f9e2af;
    padding: 0 8px;
}

#custom-load {
    color: #89b4fa;
    padding: 0 8px;
}

#custom-network {
    color: #cba6f7;
    padding: 0 8px;
}

#custom-kube {
    color: #f38ba8;
    padding: 0 8px;
}
```

## Interval guidance

waybar's `interval` is in seconds. Setting it to `1` or `2` is safe with beachcomber. If a provider returns an empty string (daemon not running, provider not available), the module will display nothing — add a fallback in your exec if you want a placeholder:

```json
"exec": "comb g battery.percent || echo '--'"
```

## Troubleshooting

- **Path-scoped providers return wrong data:** bar programs often run scripts without a defined working directory. Always pass an absolute path for path-scoped providers like `git` and `terraform`.
- **Distinguishing "no data" from "daemon down":** add a fallback to your exec: `comb g git.branch /path || echo '--'`. If you see `--`, the daemon is not running or the key does not exist.
- **CSS not applying:** waybar maps `custom/name` to CSS selector `#custom-name` (slash becomes hyphen).

See the [built-in providers reference](../reference/built-in-providers.md) for the full list of providers and fields, and the [general troubleshooting guide](./troubleshooting.md).
