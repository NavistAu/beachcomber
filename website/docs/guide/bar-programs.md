---
sidebar_position: 6
---

# Bar Programs

Status bars on Linux (polybar, waybar) and macOS (sketchybar) poll external commands for dynamic content. beachcomber makes the polling interval irrelevant — each query costs microseconds.

## polybar

```ini
[module/git]
type = custom/script
exec = comb get git.branch . -f text
interval = 5
format = <label>
label = %output%

[module/battery]
type = custom/script
exec = comb get battery.percent -f text
interval = 30
format = <label>
label = BAT: %output%%%

[module/network]
type = custom/script
exec = comb get network.ssid -f text
interval = 10
```

## waybar (JSON module)

```json
"custom/git": {
    "exec": "comb get git.branch . -f text",
    "interval": 5,
    "format": " {}",
    "tooltip": false
},
"custom/battery": {
    "exec": "comb get battery.percent -f text",
    "interval": 30,
    "format": " {}%"
}
```

## sketchybar

```sh
# In your sketchybarrc
sketchybar --add item git_branch right \
           --set git_branch update_freq=5 \
                            script="sketchybar --set git_branch label=\"$(comb get git.branch . -f text)\""
```
