---
sidebar_position: 9
---

# oh-my-zsh Integration

## Introduction

oh-my-zsh themes set `PROMPT` and `RPROMPT` using shell functions. Most themes shell out to git on every prompt. beachcomber replaces those subprocesses with cached reads.

## Prerequisites

- beachcomber installed and daemon running
- oh-my-zsh installed

## Option 1: Custom theme

The cleanest approach — create a custom theme that uses beachcomber for all dynamic data.

1. Create `~/.oh-my-zsh/custom/themes/beachcomber.zsh-theme`
2. Add the following content to the file:

```zsh
# beachcomber.zsh-theme — oh-my-zsh theme backed by beachcomber

_bc_git_info() {
    local branch=$(comb get git.branch . -f text 2>/dev/null)
    [[ -z "$branch" ]] && return
    local dirty=$(comb get git.dirty . -f text 2>/dev/null)
    local info="%F{blue}${branch}%f"
    [[ "$dirty" == "true" ]] && info+="%F{red}*%f"
    echo " ${info}"
}

_bc_kube_info() {
    local ctx=$(comb get kubecontext.context -f text 2>/dev/null)
    [[ -n "$ctx" ]] && echo " %F{cyan}☸ ${ctx}%f"
}

_bc_battery_info() {
    local pct=$(comb get battery.percent -f text 2>/dev/null)
    [[ -n "$pct" ]] && echo " %F{green}${pct}%%%f"
}

PROMPT='%F{green}%~%f$(_bc_git_info) %# '
RPROMPT='$(_bc_kube_info)$(_bc_battery_info)'
```

3. Set the theme in `~/.zshrc`: `ZSH_THEME="beachcomber"`
4. Reload: `source ~/.zshrc`

## Option 2: Custom plugin

If you want to keep your existing theme but add beachcomber-backed functions that other themes/plugins can call.

1. Create `~/.oh-my-zsh/custom/plugins/beachcomber/beachcomber.plugin.zsh` with the following content:

```zsh
# beachcomber oh-my-zsh plugin
# Provides functions that themes and other plugins can call

bc_git_branch() { comb get git.branch . -f text 2>/dev/null; }
bc_git_dirty() { comb get git.dirty . -f text 2>/dev/null; }
bc_kube_context() { comb get kubecontext.context -f text 2>/dev/null; }
bc_battery_percent() { comb get battery.percent -f text 2>/dev/null; }
bc_network_ssid() { comb get network.ssid -f text 2>/dev/null; }
bc_load() { comb get load.one -f text 2>/dev/null; }
bc_gcloud_project() { comb get gcloud.project -f text 2>/dev/null; }
bc_aws_profile() { comb get aws.profile -f text 2>/dev/null; }
```

2. Enable in `~/.zshrc`: `plugins=(... beachcomber)`
3. Use the functions in your theme or prompt customization

## Testing

```sh
# Verify beachcomber is returning data
comb get git.branch . -f text
comb status

# Open a new zsh session and check the prompt shows git info
```

## Troubleshooting

- **Theme not found:** ensure the file is at exactly `~/.oh-my-zsh/custom/themes/beachcomber.zsh-theme` (not inside a subdirectory).
- **Plugin not loading:** `plugins=(beachcomber)` must appear in `~/.zshrc` before the `source $ZSH/oh-my-zsh.sh` line.

See the [Troubleshooting](./troubleshooting.md) guide for general diagnostics.
