---
sidebar_position: 1
---

# Shell

Shell prompts run on every keypress that produces a new prompt line. If your prompt calls `git status`, `kubectl config current-context`, or similar tools, you are forking a process — often several — dozens of times per minute. On a busy workstation with many terminal windows open, this adds up quickly and causes visible prompt lag.

Beachcomber solves this by keeping those values in a shared cache updated by a background daemon. Your prompt calls `comb g`, which reads from that cache over a Unix socket in roughly 34 microseconds. The underlying tool (git, kubectl, etc.) runs on a timer in the background, not on your prompt's critical path.

## Prerequisites

Before adding shell integration, make sure `comb` is installed and on your `PATH`. Run `comb --version` to confirm.

The daemon is socket-activated — it starts automatically the first time your prompt runs `comb g`, and shuts down after an idle period. You do not need to launch it manually. At any point, run `comb s` to confirm the daemon is reachable and see cache statistics.

## zsh

### Where to put it

Add the `precmd` function to your `~/.zshrc`. The `precmd` hook runs after each command completes, just before zsh draws the next prompt — making it the right place to refresh prompt variables.

### Setup

```zsh
# ~/.zshrc
# comb g returns plain text by default. g = get, no suffix needed.
# See /docs/reference/cli-commands for all shorthands.
precmd() {
    local branch dirty untracked
    branch=$(comb g git.branch . 2>/dev/null)
    dirty=$(comb g git.dirty . 2>/dev/null)
    untracked=$(comb g git.untracked . 2>/dev/null)

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

The `.` argument passed to `comb g git.branch .` names the current directory, which is also what `comb g` addresses with no path. Rooted providers like `git` answer from the nearest repository root above that directory.

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
# comb g.s is shorthand for `comb get -f sh`. g = get, .s = shell format (key='value' lines).
__beachcomber_prompt() {
    # One query for the whole git namespace. `.s` output is key='value' lines,
    # shell-quoted, so eval it into prefixed locals rather than parsing by hand.
    local git_branch="" git_dirty=""
    eval "$(comb g.s git . 2>/dev/null | sed 's/^/local git_/')"

    local git_part=""
    if [[ -n "$git_branch" ]]; then
        git_part="(${git_branch}"
        [[ "$git_dirty" == true ]] && git_part+="*"
        git_part+=") "
    fi

    local kube
    kube=$(comb g kubecontext.context 2>/dev/null)
    local kube_part=""
    [[ -n "$kube" ]] && kube_part="[${kube}] "

    PS1="${kube_part}${git_part}\w \$ "
}

PROMPT_COMMAND=__beachcomber_prompt
```

The bash example queries the entire `git` namespace at once with `comb g.s git .`, which returns all fields as `key='value'` lines with every value shell-quoted, so `eval` is the right parser — the `sed` prefixes each line with `local git_` so the fields land in this function's scope and never collide with your own variables. Parsing that single response is more efficient than making one `comb g` call per field. The `kubecontext.context` query has no path argument because the current Kubernetes context is global, not directory-scoped.

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
    set -l branch (comb g git.branch . 2>/dev/null)
    set -l dirty (comb g git.dirty . 2>/dev/null)
    set -l battery (comb g battery.percent 2>/dev/null)

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

fish has no subshell penalty for command substitutions — each `(comb g ...)` call is still a socket round trip, but there is no fork overhead. The `battery.percent` field has no path argument because battery state is a machine-level value.

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
comb s

# Query a rooted provider from within a git repo
comb g git.branch .

# Query a value that does not depend on the directory
comb g kubecontext.context
```

If both commands return values, reload your shell config or open a new terminal. Your prompt should immediately reflect live data without any noticeable delay.

To confirm data freshness, make a change in a repository (stage a file) and press Enter at the prompt. The dirty indicator should appear within one polling cycle (default: a few seconds).

## Performance

Each `comb g` call returns a cached value over a Unix socket. Typical round-trip time is around 34 microseconds. Compare that to the tools beachcomber replaces:

| Source | Typical cost |
|---|---|
| `comb g` (cached) | ~34 µs |
| `git rev-parse --abbrev-ref HEAD` | 5–15 ms |
| `kubectl config current-context` | 50–150 ms |

A prompt that used to call `git` and `kubectl` directly could take 60–170 ms to draw. With beachcomber it takes under 1 ms. The difference is most noticeable when switching between many terminal tabs quickly or working over a slow filesystem.

The daemon handles the expensive work on its own schedule, independent of your prompt.

## Provided Scripts

The repository ships two curl-able integration scripts in `scripts/`:

- **`scripts/polyfill.sh`** — Defines a `comb()` shell function that stands in for the real binary. If comb is installed, the script does nothing. If comb is not installed, it handles `comb g <key>` calls by falling back to native tools (git, hostname, uptime, etc.) for known keys. This lets integrations write `comb g git.branch .` and have it work everywhere — with or without beachcomber. See the [Polyfill](../integrating/polyfill.md) guide for the full key list.

- **`scripts/chpwd.sh`** — A directory-change hook that pokes rooted providers (`git`, `mise`, `terraform`, `python`, `direnv`, `asdf`) in the background whenever the working directory changes. Warms the cache before your first prompt renders in a new directory. Supports zsh (`chpwd`), bash (`PROMPT_COMMAND`), and fish (separate config file). No-op if comb is not installed.

```sh
# Install via curl (bash/zsh)
source <(curl -fsSL https://beachcomber.sh/scripts/polyfill.sh)
source <(curl -fsSL https://beachcomber.sh/scripts/chpwd.sh)

# Or download and source from a local path
curl -fsSL https://beachcomber.sh/scripts/polyfill.sh -o ~/.local/share/beachcomber/polyfill.sh
source ~/.local/share/beachcomber/polyfill.sh
```

## Troubleshooting

- **Prompt shows no dynamic data:** the `2>/dev/null` in the examples silences errors, so if the daemon is not running you get an empty prompt with no error message. Remove `2>/dev/null` temporarily to see what `comb g` reports.
- **Wrong branch after switching:** the cache updates on its next poll cycle. Run `comb g --force git .` to force an immediate recomputation.

See the [Troubleshooting](./troubleshooting.md) guide for general diagnostics.
