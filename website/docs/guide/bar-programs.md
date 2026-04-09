---
sidebar_position: 6
---

# Bar Programs

Status bars on Linux (polybar, waybar) and macOS (sketchybar) work by polling external commands on an interval. Every poll spawns a subprocess, waits for it to finish, then displays the result. With slow providers — git, network lookups, kubectl — this creates visible lag or forces you to use long intervals that make your bar feel stale.

beachcomber inverts this model. The daemon runs providers in the background and keeps results cached. Each `comb get` query reads from that cache and returns in microseconds. You can set your bar's interval to 1–2 seconds everywhere without any performance cost.

## Prerequisites

The beachcomber daemon must be running before your bar program starts. Add it to your session startup:

```sh
comb &
```

Or use a systemd user unit, launchd plist, or however your system manages session daemons. Once it is running, all `comb get` calls return instantly from cache.

---

## polybar

Config file: `~/.config/polybar/config.ini`

Each beachcomber-backed module uses `type = custom/script` with a `comb get` command. Because queries are cheap, you can set `interval = 1` or `interval = 2` across the board without worrying about CPU or latency.

### Module definitions

Add these to your `config.ini`:

```ini
[module/git-branch]
type = custom/script
exec = comb get git.branch . -f text
interval = 2
format = <label>
label = %output%

[module/battery]
type = custom/script
exec = comb get battery.percent -f text
interval = 2
format = <label>
label = BAT: %output%%%

[module/load]
type = custom/script
exec = comb get load.1m -f text
interval = 2
format = <label>
label = LOAD: %output%

[module/network-ssid]
type = custom/script
exec = comb get network.ssid -f text
interval = 5
format = <label>
label = %output%

[module/kube-context]
type = custom/script
exec = comb get kubecontext.current -f text
interval = 5
format = <label>
label = k8s: %output%

[module/uptime]
type = custom/script
exec = comb get uptime.pretty -f text
interval = 60
format = <label>
label = up %output%
```

Global providers (battery, load, network, kubecontext, uptime) do not take a path argument. Path-scoped providers (git, terraform, etc.) take the target directory as a second argument — `.` means the current working directory, but since polybar runs scripts without a meaningful working directory you should pass an explicit path:

```ini
[module/git-branch]
type = custom/script
exec = comb get git.branch /home/yourname/myproject -f text
interval = 2
```

### Referencing modules in your bar

Add the modules to the `modules-left`, `modules-center`, or `modules-right` line of your `[bar/main]` section:

```ini
[bar/main]
modules-left = git-branch kube-context
modules-center = date
modules-right = load network-ssid battery uptime
```

### Interval guidance

polybar's `interval` is in seconds. With beachcomber, setting `interval = 1` is fine — the query cost is negligible. The limiting factor is how often the underlying provider actually refreshes, which is controlled by beachcomber's config, not by polybar.

---

## waybar

Config file: `~/.config/waybar/config`

waybar uses JSON for module configuration and supports `custom/*` modules that run arbitrary commands. Each module has its own `interval` field in seconds.

### Module configuration

Add these entries to your waybar config object:

```json
"custom/git-branch": {
    "exec": "comb get git.branch /home/yourname/myproject -f text",
    "interval": 2,
    "format": " {}",
    "tooltip": false
},
"custom/battery": {
    "exec": "comb get battery.percent -f text",
    "interval": 2,
    "format": " {}%",
    "tooltip": false
},
"custom/load": {
    "exec": "comb get load.1m -f text",
    "interval": 2,
    "format": " {}",
    "tooltip": false
},
"custom/network": {
    "exec": "comb get network.ssid -f text",
    "interval": 5,
    "format": " {}",
    "tooltip": false
},
"custom/kube": {
    "exec": "comb get kubecontext.current -f text",
    "interval": 5,
    "format": " {}",
    "tooltip": false
}
```

Add them to your `modules-left`, `modules-center`, or `modules-right` arrays:

```json
"modules-right": ["custom/kube", "custom/load", "custom/network", "custom/battery"]
```

### CSS selectors

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

### Interval guidance

waybar's `interval` is in seconds. Setting it to `1` or `2` is safe with beachcomber. If a provider returns an empty string (daemon not running, provider not available), the module will display nothing — add a fallback in your exec if you want a placeholder:

```json
"exec": "comb get battery.percent -f text || echo '--'"
```

---

## sketchybar (macOS)

Config file: `~/.config/sketchybar/sketchybarrc`

sketchybar items run scripts on a timer (`update_freq`) and update their label with the script's output. beachcomber fits naturally into this model. You can also use `--subscribe` to trigger updates on system events rather than polling.

### Basic items with polling

Add these to your `sketchybarrc`:

```sh
# Battery
sketchybar --add item battery right \
           --set battery \
               update_freq=5 \
               script='sketchybar --set battery label="BAT: $(comb get battery.percent -f text)%"'

# Git branch (path-scoped — pass the repo path explicitly)
sketchybar --add item git_branch left \
           --set git_branch \
               update_freq=2 \
               script='sketchybar --set git_branch label="$(comb get git.branch "$HOME/myproject" -f text)"'

# Network SSID
sketchybar --add item network right \
           --set network \
               update_freq=10 \
               script='sketchybar --set network label="$(comb get network.ssid -f text)"'
```

### Using --subscribe for event-driven updates

For providers where polling is wasteful, use `--subscribe` to trigger on system events. This requires a helper script. Create `~/.config/sketchybar/plugins/battery.sh`:

```sh
#!/bin/sh
sketchybar --set "$NAME" label="BAT: $(comb get battery.percent -f text)%"
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

### Multiple items example

A complete right-side cluster:

```sh
# Items are displayed right-to-left when added to the right side
sketchybar --add item battery right \
           --set battery \
               update_freq=5 \
               script='sketchybar --set battery label="$(comb get battery.percent -f text)%"'

sketchybar --add item network right \
           --set network \
               update_freq=10 \
               script='sketchybar --set network label="$(comb get network.ssid -f text)"'

sketchybar --add item kube right \
           --set kube \
               update_freq=5 \
               script='sketchybar --set kube label="$(comb get kubecontext.current -f text)"'

sketchybar --add item git_branch left \
           --set git_branch \
               update_freq=2 \
               script='sketchybar --set git_branch label="$(comb get git.branch "$HOME/myproject" -f text)"'
```

---

## Troubleshooting

- **Path-scoped providers return wrong data:** bar programs often run scripts without a defined working directory. `.` resolves to wherever the bar process started. Always pass an absolute path for path-scoped providers like `git` and `terraform`.
- **Distinguishing "no data" from "daemon down":** add a fallback to your exec: `comb get git.branch /path -f text || echo '--'`. If you see `--`, the daemon is not running or the key does not exist.
- **waybar CSS not applying:** waybar maps `custom/name` to CSS selector `#custom-name` (slash becomes hyphen).

See the [Troubleshooting](./troubleshooting.md) guide for general diagnostics.
