---
sidebar_position: 3
---

# tmux Integration

## How it works

tmux evaluates `#(command)` format strings by forking a shell every time the status bar refreshes. With a `status-interval` of 5 seconds and a handful of queries, that is a steady stream of subprocesses — each one potentially spawning git, running a battery command, or querying the network.

beachcomber changes the economics. The daemon pre-caches values in the background; each `comb get` call reads from that cache in roughly 34 microseconds. The subprocesses still happen, but they return almost instantly instead of blocking on I/O. Across a session with multiple windows and a short refresh interval this adds up to a meaningful difference in shell and CPU overhead.

Each `comb get` call also signals demand to the daemon, which keeps the relevant provider warm automatically.

## Prerequisites

- The beachcomber daemon is running (`comb status` should return successfully)
- tmux is installed and you have an active session

If the daemon is not running, see the [Getting Started](./getting-started.md) guide.

## Plain tmux setup

### Where to add the config

tmux reads `~/.tmux.conf` on startup. Status bar format strings go in `set -g status-left` and `set -g status-right` directives. You can add the lines below anywhere in that file; most people group all status bar configuration together near the bottom.

### Reload without restarting

After editing `~/.tmux.conf`, apply the changes to a running session:

```sh
tmux source-file ~/.tmux.conf
```

Or from inside tmux, press the prefix key then `:` and type `source-file ~/.tmux.conf`.

### The comb get syntax

```sh
comb get <key> [path] -f text
```

Global providers (battery, load, hostname, uptime, network) do not require a path. Path-scoped providers like `git` require a directory to query against.

**Important:** `#()` commands in the tmux status bar run in the tmux server process, not inside any pane. The server has no concept of the current pane's working directory. For path-scoped providers you must pass the path explicitly using tmux's own format variables:

```
#(comb get git.branch "#{pane_current_path}" -f text)
```

tmux expands `#{pane_current_path}` before passing the string to the shell, so the daemon receives the actual path of the active pane.

### Provider examples

Battery percentage:

```
#(comb get battery.percent -f text)%%
```

The `%%` is tmux's way of printing a literal `%`.

Load average (1-minute):

```
#(comb get load.load1 -f text)
```

Network SSID (macOS):

```
#(comb get network.ssid -f text)
```

Uptime:

```
#(comb get uptime.uptime -f text)
```

Hostname:

```
#(comb get hostname.hostname -f text)
```

Git branch for the active pane's directory:

```
#(comb get git.branch "#{pane_current_path}" -f text)
```

Kubernetes context:

```
#(comb get kubecontext.context -f text)
```

### Complete status bar example

```tmux
# ~/.tmux.conf

# Refresh every 5 seconds — cheap with beachcomber
set -g status-interval 5

# Left: session name and hostname
set -g status-left '[#S] #(comb get hostname.hostname -f text) | '

# Right: git branch, load, battery, kubernetes context
set -g status-right '#(comb get git.branch "#{pane_current_path}" -f text) | load:#(comb get load.load1 -f text) | bat:#(comb get battery.percent -f text)%% | #(comb get kubecontext.context -f text)'

# Optional: increase the space available for the right status
set -g status-right-length 120
```

### Expected output

With the example above the right side of the status bar will look roughly like:

```
main | load:0.42 | bat:87% | prod-cluster
```

Fields that return no data (for example, `git.branch` when the pane is not inside a git repository) will be empty strings. You can wrap them in tmux conditionals or simply accept the extra `|` separator.

## oh-my-tmux

[oh-my-tmux](https://github.com/gpakosz/.tmux) is a popular tmux configuration framework. It ships with its own opinionated status bar and theme system.

### How oh-my-tmux handles customization

oh-my-tmux is designed so that you never edit `.tmux.conf` directly. Instead, all user customization goes into `.tmux.conf.local`. oh-my-tmux sources this file after applying its own config, so anything you set there overrides the defaults without touching the framework files.

When you install oh-my-tmux you will typically copy the provided sample local file:

```sh
cp ~/.tmux.conf.local.example ~/.tmux.conf.local
# or, if the example is not present:
cp ~/.oh-my-tmux/.tmux.conf.local ~/.tmux.conf.local
```

### The relevant settings

oh-my-tmux exposes two variables for status bar content:

- `tmux_conf_theme_status_left` — content for the left segment
- `tmux_conf_theme_status_right` — content for the right segment

These accept the same `#()` format strings as plain tmux. Set them in `.tmux.conf.local`.

### Adding beachcomber queries

Find the `tmux_conf_theme_status_left` and `tmux_conf_theme_status_right` lines in your `.tmux.conf.local` (they may be commented out). Uncomment them and add your beachcomber queries. oh-my-tmux uses `|` and space as segment separators; match the style of the existing values.

```tmux
# ~/.tmux.conf.local

# Left: session name and hostname
tmux_conf_theme_status_left=' #S | #(comb get hostname.hostname -f text) '

# Right: git branch, load average, battery, kube context
tmux_conf_theme_status_right=' #(comb get git.branch "#{pane_current_path}" -f text) | load #(comb get load.load1 -f text) | bat #(comb get battery.percent -f text)%% | #(comb get kubecontext.context -f text) | %R '
```

The surrounding spaces are intentional — oh-my-tmux uses them for visual padding inside each segment.

After editing `.tmux.conf.local`, reload with:

```sh
tmux source-file ~/.tmux.conf
```

oh-my-tmux will re-read the local file as part of sourcing the main config.

## Troubleshooting

- **Status bar not updating:** check `tmux show -g status-interval`. If set to a large value or 0, reduce it: `tmux set -g status-interval 5`.
- **Path-scoped providers empty in tmux:** `#()` runs in the tmux server process, not a pane. Verify tmux is expanding the path variable: `tmux display-message -p '#{pane_current_path}'`. The output should be the pane's working directory.
- **oh-my-tmux changes not taking effect:** after editing `.tmux.conf.local`, run `tmux source-file ~/.tmux.conf` — oh-my-tmux re-reads the local file as part of sourcing the main config.

See the [Troubleshooting](./troubleshooting.md) guide for general diagnostics.
