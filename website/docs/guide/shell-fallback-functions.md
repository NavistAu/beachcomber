---
sidebar_position: 8
---

# Shell Fallback Functions

Apps that want to support beachcomber without requiring it can embed a fallback function. If `comb` is not installed, the function falls back to the native tool. This pattern lets any shell script or prompt framework opt into beachcomber acceleration transparently.

## bash / zsh

```sh
# Returns the current git branch name.
# Uses beachcomber if installed; falls back to git directly.
_git_branch() {
    if command -v comb >/dev/null 2>&1; then
        comb get git.branch . -f text 2>/dev/null && return
    fi
    git rev-parse --abbrev-ref HEAD 2>/dev/null
}

# Returns "true" if the working tree has uncommitted changes.
_git_dirty() {
    if command -v comb >/dev/null 2>&1; then
        comb get git.dirty . -f text 2>/dev/null && return
    fi
    git diff --quiet 2>/dev/null || echo "true"
}

# Returns current kubernetes context.
_kube_context() {
    if command -v comb >/dev/null 2>&1; then
        comb get kubecontext.context -f text 2>/dev/null && return
    fi
    kubectl config current-context 2>/dev/null
}

# Returns battery percentage as an integer.
_battery_percent() {
    if command -v comb >/dev/null 2>&1; then
        comb get battery.percent -f text 2>/dev/null && return
    fi
    # macOS fallback
    pmset -g batt 2>/dev/null | grep -Eo '[0-9]+%' | head -1 | tr -d '%'
}
```

## fish

```fish
function _git_branch
    if command -q comb
        comb get git.branch . -f text 2>/dev/null; and return
    end
    git rev-parse --abbrev-ref HEAD 2>/dev/null
end

function _git_dirty
    if command -q comb
        comb get git.dirty . -f text 2>/dev/null; and return
    end
    git diff --quiet 2>/dev/null; or echo "true"
end

function _kube_context
    if command -q comb
        comb get kubecontext.context -f text 2>/dev/null; and return
    end
    kubectl config current-context 2>/dev/null
end

function _battery_percent
    if command -q comb
        comb get battery.percent -f text 2>/dev/null; and return
    end
    # macOS fallback
    pmset -g batt 2>/dev/null | string match -r '\d+%' | string replace '%' ''
end
```

These functions can be pasted directly into prompt frameworks, dotfile repos, or shared shell libraries. Users with beachcomber installed get the 15µs path; users without it get the native fallback. No beachcomber dependency required.

## Inline fallback with `||`

For scripts that only need a value once (not in a hot loop like a prompt), the wrapper function is unnecessary. `comb` exits non-zero when it's not installed, not running, or the key doesn't exist — so a simple `||` chain works:

```sh
# Single assignment, no wrapper needed
branch=$(comb get git.branch . -f text 2>/dev/null || git rev-parse --abbrev-ref HEAD 2>/dev/null)
dirty=$(comb get git.dirty . -f text 2>/dev/null || git diff --quiet 2>/dev/null || echo "true")

# Use the values
if [ -n "$branch" ]; then
    echo "on $branch"
fi
```

This keeps scripts portable with zero comb dependency — `2>/dev/null` swallows errors, the `||` falls through to the native tool, and there's nothing to source or import. Prefer this for standalone scripts; use the wrapper functions above for shared shell libraries where the pattern repeats.
