---
sidebar_position: 2
title: Polyfill
---

# The Polyfill

Integrations that use `comb` can work without beachcomber installed by using the polyfill script. This lets upstream projects (themes, plugins, dotfile frameworks) adopt beachcomber without adding a hard dependency.

## The polyfill (recommended)

`scripts/polyfill.sh` defines a `comb()` shell function that stands in for the real binary. If comb is already installed, it does nothing — the real binary takes precedence. If comb is not installed, the function handles `comb g <key>` calls by falling back to native tools for known keys.

**Install:**

```sh
# curl into your shell rc (bash, zsh)
curl -fsSL https://beachcomber.sh/scripts/polyfill.sh >> ~/.zshrc

# or source it directly
source <(curl -fsSL https://beachcomber.sh/scripts/polyfill.sh)
```

**What it covers:**

| Key | Fallback |
|-----|----------|
| `git.branch` | `git rev-parse --abbrev-ref HEAD` |
| `git.dirty` | `git diff --quiet HEAD` |
| `git.ahead` | `git rev-list --count @{upstream}..HEAD` |
| `git.behind` | `git rev-list --count HEAD..@{upstream}` |
| `git.stash_count` | `git stash list \| wc -l` |
| `git.commit_summary` | `git log -1 --format='%s'` |
| `hostname.name` | `hostname` |
| `hostname.short` | `hostname -s` |
| `user.name` | `whoami` |
| `load.one` / `five` / `fifteen` | `uptime` (parsed) |
| `battery.percent` | `pmset -g batt` (macOS) / `/sys/class/power_supply/` (Linux) |
| `battery.charging` | same |

**How it works:**

1. If `comb` is already in `$PATH`, the script exits immediately — no function defined, no overhead
2. Otherwise it defines a `comb()` function that parses the command and format suffix (`g`, `get`, etc.)
3. For `g`/`get` commands, it dispatches to native tool fallbacks by key name
4. For any other command (`r`, `s`, `w`, etc.) or unknown key, it prints an install hint to stderr and returns non-zero

**Why this is the right default:** upstream integrations can write `comb g git.branch .` everywhere and it works on every machine. Users with beachcomber get the ~34µs cached path. Users without it get the native tool. No conditional logic in the integration code.

**Example — a starship module that works everywhere:**

```toml
[custom.git_branch]
command = "comb g git.branch ."
when = "comb g git.branch ."
format = "[$output]($style) "
style = "bold blue"
```

With beachcomber installed: reads from cache. Without: the polyfill calls `git rev-parse`. The starship config is identical either way.

## Alternative: per-function wrappers

If you don't want to source the polyfill, you can write individual wrapper functions. This is useful for shared shell libraries where you only need fallbacks for specific keys.

```sh
# Returns the current git branch.
# Uses beachcomber if installed; falls back to git directly.
_git_branch() {
    comb g git.branch . 2>/dev/null && return
    git rev-parse --abbrev-ref HEAD 2>/dev/null
}

_git_dirty() {
    comb g git.dirty . 2>/dev/null && return
    git diff --quiet 2>/dev/null || echo "true"
}

_kube_context() {
    comb g kubecontext.context 2>/dev/null && return
    kubectl config current-context 2>/dev/null
}

_battery_percent() {
    comb g battery.percent 2>/dev/null && return
    pmset -g batt 2>/dev/null | grep -Eo '[0-9]+%' | head -1 | tr -d '%'
}
```

fish equivalents:

```fish
function _git_branch
    comb g git.branch . 2>/dev/null; and return
    git rev-parse --abbrev-ref HEAD 2>/dev/null
end

function _git_dirty
    comb g git.dirty . 2>/dev/null; and return
    git diff --quiet 2>/dev/null; or echo "true"
end
```

These work whether or not beachcomber is installed — `comb` exits non-zero when it's not available, the `&& return` skips, and the fallback runs.

## Alternative: inline `||` fallback

For one-off uses in scripts (not hot loops like prompts), skip the wrapper entirely:

```sh
branch=$(comb g git.branch . 2>/dev/null || git rev-parse --abbrev-ref HEAD 2>/dev/null)
dirty=$(comb g git.dirty . 2>/dev/null || git diff --quiet 2>/dev/null || echo "true")
```

Portable with zero comb dependency. Prefer the polyfill for prompt/framework integrations where the pattern repeats across many keys.
