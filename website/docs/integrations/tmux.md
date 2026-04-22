---
sidebar_position: 2
---

# tmux

## How it works

tmux evaluates `#(command)` format strings by forking a shell every time the status bar refreshes. With a `status-interval` of 5 seconds and a handful of queries, that is a steady stream of subprocesses — each one potentially spawning git, running a battery command, or querying the network.

beachcomber changes the economics. The daemon pre-caches values in the background; each `comb g` call reads from that cache in roughly 34 microseconds. The subprocesses still happen, but they return almost instantly instead of blocking on I/O. Across a session with multiple windows and a short refresh interval this adds up to a meaningful difference in shell and CPU overhead.

Each `comb g` call also signals demand to the daemon, which keeps the relevant provider warm automatically.

## Prerequisites

- The beachcomber daemon is running (`comb s` should return successfully)
- tmux is installed and you have an active session

If the daemon is not running, see the [Getting Started](../getting-started/quick-start.md) guide.

## Plain tmux setup

### Where to add the config

tmux reads `~/.tmux.conf` on startup. Status bar format strings go in `set -g status-left` and `set -g status-right` directives. You can add the lines below anywhere in that file; most people group all status bar configuration together near the bottom.

### Reload without restarting

After editing `~/.tmux.conf`, apply the changes to a running session:

```sh
tmux source-file ~/.tmux.conf
```

Or from inside tmux, press the prefix key then `:` and type `source-file ~/.tmux.conf`.

### The comb syntax

```sh
comb g <key> [path]           # g = get, text is the default format
comb get <key> [path]         # equivalent long form
```

`g` is the short form used throughout this guide — `g` is `get`, text is the default output format. See the [CLI reference](/docs/reference/cli-commands) for all shorthands.

Global providers (battery, load, hostname, uptime, network) do not require a path. Path-scoped providers like `git` require a directory to query against.

**Important:** `#()` commands in the tmux status bar run in the tmux server process, not inside any pane. The server has no concept of the current pane's working directory. For path-scoped providers you must pass the path explicitly using tmux's own format variables:

```
#(comb g git.branch "#{pane_current_path}")
```

tmux expands `#{pane_current_path}` before passing the string to the shell, so the daemon receives the actual path of the active pane.

### Provider examples

Battery percentage:

```
#(comb g battery.percent)%%
```

The `%%` is tmux's way of printing a literal `%`.

Load average (1-minute):

```
#(comb g load.one)
```

Network SSID (macOS):

```
#(comb g network.ssid)
```

Uptime (days and hours, via template format):

```
#(comb g.f '{{ days }}d {{ hours }}h' uptime)
```

Hostname:

```
#(comb g hostname.short)
```

Git branch for the active pane's directory:

```
#(comb g git.branch "#{pane_current_path}")
```

Kubernetes context:

```
#(comb g kubecontext.context)
```

### Complete status bar example

```tmux
# ~/.tmux.conf

# Refresh every 5 seconds — cheap with beachcomber
set -g status-interval 5

# Left: session name and hostname
set -g status-left '[#S] #(comb g hostname.short) | '

# Right: git branch, load, battery, kubernetes context
set -g status-right '#(comb g git.branch "#{pane_current_path}") | load:#(comb g load.one) | bat:#(comb g battery.percent)%% | #(comb g kubecontext.context)'

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

[oh-my-tmux](https://github.com/gpakosz/.tmux) is a popular tmux configuration framework. It ships with its own opinionated status bar and theme system. There are two levels of integration — a simple approach that adds beachcomber queries alongside the existing oh-my-tmux features, and a deep integration that replaces oh-my-tmux's built-in data sources entirely.

### Background: how oh-my-tmux works internally

Before choosing an approach, it helps to understand what oh-my-tmux does under the hood. The `.tmux.conf` file is both a tmux config and an embedded shell script — all lines prefixed with `# ` are actually shell code. When tmux evaluates a format string like `#{battery_percentage}`, oh-my-tmux has already rewritten it during theme application into something like:

```
#(cut -c3- "$TMUX_CONF" | sh -s _battery_info)
```

This strips the `# ` prefix from the entire `.tmux.conf` file, pipes the result into `sh`, and calls the named function. Every format string evaluation forks a new shell, and every one of those shells unconditionally runs `_uname_s=$(uname -s)` to detect the platform — even if the function being called doesn't need it.

With a `status-interval` of 30 seconds and several panes, oh-my-tmux's built-in variables like `#{battery_status}`, `#{battery_bar}`, `#{battery_percentage}`, `#{username}`, and `#{hostname}` collectively spawn hundreds of subprocesses per minute. Each battery query forks `pmset` on macOS. Each username/hostname query walks the pane's process tree.

beachcomber eliminates the data-gathering cost: the daemon pre-caches battery, hostname, and user data in the background, and each `comb g` call returns from cache in microseconds.

### How oh-my-tmux handles customization

oh-my-tmux is designed so that you never edit `.tmux.conf` directly. All user customization goes into `.tmux.conf.local`. oh-my-tmux sources this file after applying its own config, so anything you set there overrides the defaults without touching the framework files.

### Simple approach: add beachcomber queries

The quickest integration is to add `comb g` calls directly into the oh-my-tmux status variables. Find the `tmux_conf_theme_status_left` and `tmux_conf_theme_status_right` lines in your `.tmux.conf.local` and add beachcomber queries using `#()` format strings:

```tmux
# ~/.tmux.conf.local

tmux_conf_theme_status_left=' #S | #(comb g hostname.short) '

tmux_conf_theme_status_right=' #(comb g git.branch "#{pane_current_path}") | load #(comb g load.one) | bat #(comb g battery.percent)%% | #(comb g kubecontext.context) | %R '
```

This works alongside oh-my-tmux's built-in variables. The beachcomber queries return from cache nearly instantly; the built-in variables continue to fork as before.

### Deep integration: replace oh-my-tmux's data sources

The deep integration replaces oh-my-tmux's built-in `#{battery_*}`, `#{username}`, and `#{hostname}` variables with custom functions that read from beachcomber. This eliminates the subprocess storm at its source.

oh-my-tmux supports custom variables through POSIX shell functions defined in the `# EOF` section of `.tmux.conf.local`. A function named `foo` is referenced as `#{foo}` in the status string. oh-my-tmux's theme engine rewrites `#{foo}` into `#(cut -c3- "$TMUX_CONF_LOCAL" | sh -s foo)` — a shell fork that calls your function.

#### Step 1: Replace battery

oh-my-tmux's battery display uses three built-in variables: `#{battery_status}` (charging arrow), `#{battery_bar}` (gradient bar), and `#{battery_percentage}`. Behind the scenes, `_battery_info` runs a background polling loop that calls `pmset -g batt` on macOS, and `_battery_status` runs on every status refresh with another `pmset` call.

The key insight: oh-my-tmux's battery code path only activates if `#{battery_*}` tokens appear in the status string. Remove them and the entire battery subprocess machinery is disabled. Replace them with a single custom function that queries beachcomber and delegates rendering to oh-my-tmux's own `_bar()` function:

```tmux
# ~/.tmux.conf.local — status_right
# Comment out the original and add the beachcomber version:

# original:
# tmux_conf_theme_status_right=" #{prefix}#{mouse}#{pairing}#{synchronized}#{?battery_status,#{battery_status},}#{?battery_bar, #{battery_bar},}#{?battery_percentage, #{battery_percentage},} , %R , %d %b | #{username}#{root} | #{hostname} "

# beachcomber battery:
tmux_conf_theme_status_right=" #{prefix}#{mouse}#{pairing}#{synchronized}#{comb_battery} , %R , %d %b | #{username}#{root} | #{hostname} "
```

Then define the `comb_battery` function in the `# EOF` section at the bottom of `.tmux.conf.local`:

```sh
# # /!\ do not remove the following line
# EOF
#
# comb_battery() {
#   percent=$(comb g battery.percent 2>/dev/null) || return
#   [ -z "$percent" ] && return
#   charging=$(comb g battery.charging 2>/dev/null)
#
#   # status symbol
#   if [ "$charging" = "true" ]; then
#     status="↑"
#   else
#     status="↓"
#   fi
#
#   # compute charge as 0-1 float for oh-my-tmux's _bar renderer
#   charge=$(awk "BEGIN { printf \"%.2f\", $percent / 100 }")
#
#   # render the gradient bar using oh-my-tmux's built-in _bar() function
#   # args: palette, empty_symbol, full_symbol, length, charge, client_width
#   bar=$(cut -c3- "$TMUX_CONF" | sh -s _bar \
#     "gradient" "◻" "◼" "auto" "$charge" \
#     "$("$TMUX_PROGRAM" display -p '#{client_width}')")
#
#   printf '%s %s %s%%' "$status" "$bar" "$percent"
# }
#
# "$@"
# # /!\ do not remove the previous line
```

The `comb_battery` function calls `comb g` twice (battery percent and charging status), both returning from cache in microseconds. It then calls `_bar()` through oh-my-tmux's own shell payload to render the gradient bar identically to the original. The result is visually identical, but no `pmset` subprocess is ever forked.

Note the `_bar()` call: `cut -c3- "$TMUX_CONF" | sh -s _bar ...` invokes the function from the main `.tmux.conf`. The `_bar` function is a pure renderer — it takes a charge float and outputs tmux colour escape sequences. It does not query any system state.

If you want a simpler display without the gradient bar, you can skip the `_bar()` call entirely:

```sh
#   printf '%s %s%%' "$status" "$percent"
```

#### Step 2: Replace username and hostname

oh-my-tmux's `#{username}` and `#{hostname}` are not simple lookups — they walk the pane's process tree to detect SSH sessions and display the **remote** username and hostname when you are SSHed into another machine. This is genuinely useful and beachcomber cannot replicate it because the daemon only knows about the local system.

The solution is a hybrid approach: use beachcomber for local panes and fall back to oh-my-tmux's SSH-aware resolver for remote sessions. A `pgrep` check on the pane's child processes detects SSH sessions cheaply.

Update the status string to use custom functions instead of the built-in variables:

```tmux
# beachcomber battery + username + hostname:
tmux_conf_theme_status_right=" #{prefix}#{mouse}#{pairing}#{synchronized}#{comb_battery} , %R , %d %b | #{comb_username #{pane_pid} #{b:pane_tty} #D}#{root} | #{comb_hostname #{pane_pid} #{b:pane_tty} #h #D} "
```

The `#{pane_pid}`, `#{b:pane_tty}`, `#h`, and `#D` tokens are expanded by tmux before the function runs. They provide the pane's process ID, TTY, short hostname, and unique pane identifier — the same arguments oh-my-tmux passes to its own `_username` and `_hostname` functions.

Add the functions in the `# EOF` section:

```sh
# comb_username() {
#   # args: pane_pid, pane_tty, pane_id
#   if pgrep -P "$1" -q ssh mosh mosh-client 2>/dev/null; then
#     # SSH session — fall back to oh-my-tmux's process tree resolver
#     cut -c3- "$TMUX_CONF" | sh -s _username "$1" "$2" false "$3"
#   else
#     # Local session — read from beachcomber cache
#     comb g user.name 2>/dev/null || \
#       cut -c3- "$TMUX_CONF" | sh -s _username "$1" "$2" false "$3"
#   fi
# }
#
# comb_hostname() {
#   # args: pane_pid, pane_tty, h_or_H, pane_id
#   if pgrep -P "$1" -q ssh mosh mosh-client 2>/dev/null; then
#     # SSH session — fall back to oh-my-tmux's process tree resolver
#     cut -c3- "$TMUX_CONF" | sh -s _hostname "$1" "$2" false false "$3" "$4"
#   else
#     # Local session — read from beachcomber cache
#     comb g hostname.short 2>/dev/null || \
#       cut -c3- "$TMUX_CONF" | sh -s _hostname "$1" "$2" false false "$3" "$4"
#   fi
# }
```

For local panes, the `comb g` call returns from cache and the function exits immediately — no process tree walking, no `uname` side effect. For SSH panes, the full oh-my-tmux resolver runs as before, correctly displaying the remote username and hostname.

#### Step 3: Reload

After all changes, reload the configuration:

```sh
tmux source-file ~/.tmux.conf
```

You must source the main `.tmux.conf`, not just `.tmux.conf.local` — oh-my-tmux's `_apply_configuration` function is what processes custom variables and wires them into the status bar format strings.

#### What this eliminates

With the deep integration, a typical oh-my-tmux setup with 10 panes and a 30-second status interval goes from forking roughly 500+ subprocesses per minute down to a handful of lightweight `comb g` calls that return from cache. The battery provider polling (`pmset -g batt`) and the platform detection (`uname -s`) are the most expensive operations eliminated.

The `#{root}` variable is left unchanged — it is a simple check that oh-my-tmux handles cheaply.

### Reverting

All changes are in `.tmux.conf.local`. To revert to the original oh-my-tmux behaviour:

1. Uncomment the original `tmux_conf_theme_status_right` line
2. Comment out or delete the beachcomber version
3. The custom functions in the `# EOF` section are inert when not referenced — you can leave them in place

No changes to `.tmux.conf` are required at any point.

## Troubleshooting

- **Status bar not updating:** check `tmux show -g status-interval`. If set to a large value or 0, reduce it: `tmux set -g status-interval 5`.
- **Path-scoped providers empty in tmux:** `#()` runs in the tmux server process, not a pane. Verify tmux is expanding the path variable: `tmux display-message -p '#{pane_current_path}'`. The output should be the pane's working directory.
- **oh-my-tmux changes not taking effect:** after editing `.tmux.conf.local`, run `tmux source-file ~/.tmux.conf` — oh-my-tmux re-reads the local file as part of sourcing the main config. Sourcing only `.tmux.conf.local` will not trigger the custom variable wiring.
- **Custom functions not rendering:** oh-my-tmux's custom variable parser rejects function names starting with `_`. Use names like `comb_battery`, not `_comb_battery`.
- **Battery bar not rendering:** the `_bar()` call requires `$TMUX_CONF` to be set (oh-my-tmux sets this automatically). If you see the status and percentage but no bar, check that the `cut -c3-` path is correct.
- **SSH username/hostname showing local values:** verify `pgrep -P <pane_pid> ssh` detects the SSH process. If your SSH session is nested deeper in the process tree, the `pgrep -P` check may not find it and the function will use the local cache. The fallback (`|| cut -c3- ...`) ensures graceful degradation.

See the [Troubleshooting](./troubleshooting.md) guide for general diagnostics.
