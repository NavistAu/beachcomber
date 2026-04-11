---
sidebar_position: 2
---

# Shell Integration

Shell prompts run on every keypress that produces a new prompt line. If your prompt calls `git status`, `kubectl config current-context`, or similar tools, you are forking a process — often several — dozens of times per minute. On a busy workstation with many terminal windows open, this adds up quickly and causes visible prompt lag.

Beachcomber solves this by keeping those values in a shared cache updated by a background daemon. Your prompt calls `comb get`, which reads from that cache over a Unix socket in roughly 34 microseconds. The underlying tool (git, kubectl, etc.) runs on a timer in the background, not on your prompt's critical path.

## Prerequisites

Before adding shell integration, make sure:

1. `comb` is installed and on your `PATH`. Run `comb --version` to confirm.
2. The daemon is running. Start it with:

```sh
comb daemon &
```

Or configure it to launch automatically via your init system or shell login file so it is available in all terminal sessions. Check `comb status` at any time to confirm the daemon is reachable.

## zsh

### Where to put it

Add the `precmd` function to your `~/.zshrc`. The `precmd` hook runs after each command completes, just before zsh draws the next prompt — making it the right place to refresh prompt variables.

### Setup

```zsh
# ~/.zshrc
precmd() {
    local branch dirty untracked
    branch=$(comb get git.branch . -f text 2>/dev/null)
    dirty=$(comb get git.dirty . -f text 2>/dev/null)
    untracked=$(comb get git.untracked . -f text 2>/dev/null)

    local git_part=""
    if [[ -n "$branch" ]]; then
        git_part="%F{blue}${branch}%f"
        [[ "$dirty" == "true" ]] && git_part+="*"
        [[ "$untracked" -gt 0 ]] && git_part+="?"
        git_part+=" "
    fi

    PS1="${git_part}%F{green}%~%f %# "
}
```

The `.` argument passed to `comb get git.branch .` tells the daemon to scope the lookup to the current directory. Path-scoped providers like `git` return data relative to the nearest repository root from that path. Always pass `.` (or an explicit path) when querying providers that are directory-aware.

### How to reload

```zsh
source ~/.zshrc
```

### Expected result

Open a new terminal in a git repository. Your prompt should show the current branch name. Move into a directory with uncommitted changes and the `*` indicator should appear. In a directory outside any repository the git portion of the prompt will be absent.

## bash

### Where to put it

Add the function and `PROMPT_COMMAND` assignment to your `~/.bashrc`. bash evaluates `PROMPT_COMMAND` before drawing each prompt, equivalent to zsh's `precmd`.

### Setup

```bash
# ~/.bashrc
__beachcomber_prompt() {
    # Fetch entire git state in one query, parse key=value output
    local git_state
    git_state=$(comb get git . -f text 2>/dev/null)

    local branch dirty
    while IFS='=' read -r key value; do
        case "$key" in
            branch) branch="$value" ;;
            dirty)  dirty="$value" ;;
        esac
    done <<< "$git_state"

    local git_part=""
    [[ -n "$branch" ]] && git_part="(${branch}${dirty:+*}) "

    local kube
    kube=$(comb get kubecontext.context -f text 2>/dev/null)
    local kube_part=""
    [[ -n "$kube" ]] && kube_part="[${kube}] "

    PS1="${kube_part}${git_part}\w \$ "
}

PROMPT_COMMAND=__beachcomber_prompt
```

The bash example queries the entire `git` namespace at once with `comb get git . -f text`, which returns all fields as `key=value` lines. Parsing that single response is more efficient than making one `comb get` call per field. The `kubecontext.context` query has no path argument because the current Kubernetes context is global, not directory-scoped.

### How to reload

```bash
source ~/.bashrc
```

### Expected result

Open a new terminal. In a git repository the prompt shows `(branch) ~/path/to/repo $`. If your kubeconfig has a current context set, it appears as `[context-name]` before the git section. Outside a repository the git section is omitted entirely.

## fish

### Where to put it

Create or replace `~/.config/fish/functions/fish_prompt.fish`. fish auto-loads this file and calls `fish_prompt` before each prompt. If you already have a custom prompt, merge the beachcomber calls into your existing function.

### Setup

```fish
# ~/.config/fish/functions/fish_prompt.fish
function fish_prompt
    set -l branch (comb get git.branch . -f text 2>/dev/null)
    set -l dirty (comb get git.dirty . -f text 2>/dev/null)
    set -l battery (comb get battery.percent -f text 2>/dev/null)

    set -l git_info ""
    if test -n "$branch"
        set git_info " $branch"
        test "$dirty" = "true"; and set git_info "$git_info*"
    end

    set -l bat_info ""
    if test -n "$battery"
        set bat_info " $battery%%"
    end

    echo -n (set_color blue)(prompt_pwd)(set_color normal)$git_info$bat_info" > "
end
```

fish has no subshell penalty for command substitutions — each `(comb get ...)` call is still a socket round trip, but there is no fork overhead. The `battery.percent` field has no path argument because battery state is a machine-level value.

### How to reload

```fish
source ~/.config/fish/functions/fish_prompt.fish
```

Or open a new terminal. fish reloads function files automatically at startup.

### Expected result

Your prompt shows the current directory in blue, the git branch if present, a `*` if the working tree is dirty, and the battery percentage if the `battery` provider is enabled in your config.

## Testing your integration

Before restarting your shell, verify that the daemon is returning data:

```sh
# Confirm the daemon is reachable
comb status

# Query a path-scoped provider from within a git repo
comb get git.branch . -f text

# Query a global provider
comb get kubecontext.context -f text
```

If both commands return values, reload your shell config or open a new terminal. Your prompt should immediately reflect live data without any noticeable delay.

To confirm data freshness, make a change in a repository (stage a file) and press Enter at the prompt. The dirty indicator should appear within one polling cycle (default: a few seconds).

## Performance

Each `comb get` call returns a cached value over a Unix socket. Typical round-trip time is around 34 microseconds. Compare that to the tools beachcomber replaces:

| Source | Typical cost |
|---|---|
| `comb get` (cached) | ~34 µs |
| `git rev-parse --abbrev-ref HEAD` | 5–15 ms |
| `kubectl config current-context` | 50–150 ms |

A prompt that used to call `git` and `kubectl` directly could take 60–170 ms to draw. With beachcomber it takes under 1 ms. The difference is most noticeable when switching between many terminal tabs quickly or working over a slow filesystem.

The daemon handles the expensive work on its own schedule, independent of your prompt.

## Troubleshooting

- **Prompt shows no dynamic data:** the `2>/dev/null` in the examples silences errors, so if the daemon is not running you get an empty prompt with no error message. Remove `2>/dev/null` temporarily to see what `comb get` reports.
- **Wrong branch after switching:** the cache updates on its next poll cycle. Run `comb refresh git .` to force an immediate refresh.

See the [Troubleshooting](./troubleshooting.md) guide for general diagnostics.
