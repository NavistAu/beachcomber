---
sidebar_position: 5
---

# Oh My Zsh

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
# comb g returns plain text by default. g = get, no suffix needed.

_bc_git_info() {
    local branch=$(comb g git.branch . 2>/dev/null)
    [[ -z "$branch" ]] && return
    local dirty=$(comb g git.dirty . 2>/dev/null)
    local info="%F{blue}${branch}%f"
    [[ "$dirty" == "true" ]] && info+="%F{red}*%f"
    echo " ${info}"
}

_bc_kube_info() {
    local ctx=$(comb g kubecontext.context 2>/dev/null)
    [[ -n "$ctx" ]] && echo " %F{cyan}☸ ${ctx}%f"
}

_bc_battery_info() {
    local pct=$(comb g battery.percent 2>/dev/null)
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

bc_git_branch() { comb g git.branch . 2>/dev/null; }
bc_git_dirty() { comb g git.dirty . 2>/dev/null; }
bc_kube_context() { comb g kubecontext.context 2>/dev/null; }
bc_battery_percent() { comb g battery.percent 2>/dev/null; }
bc_network_ssid() { comb g network.ssid 2>/dev/null; }
bc_load() { comb g load.one 2>/dev/null; }
bc_gcloud_project() { comb g gcloud.project 2>/dev/null; }
bc_aws_profile() { comb g aws.profile 2>/dev/null; }
```

2. Enable in `~/.zshrc`: `plugins=(... beachcomber)`
3. Use the functions in your theme or prompt customization

## Testing

```sh
# Verify beachcomber is returning data
comb g git.branch .
comb s

# Open a new zsh session and check the prompt shows git info
```

## Troubleshooting

- **Theme not found:** ensure the file is at exactly `~/.oh-my-zsh/custom/themes/beachcomber.zsh-theme` (not inside a subdirectory).
- **Plugin not loading:** `plugins=(beachcomber)` must appear in `~/.zshrc` before the `source $ZSH/oh-my-zsh.sh` line.

See the [Troubleshooting](./troubleshooting.md) guide for general diagnostics.
