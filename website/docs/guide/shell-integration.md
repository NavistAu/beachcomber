---
sidebar_position: 2
---

# Shell Integration

## zsh prompt (precmd hook)

The most common use case. Use `precmd` to refresh prompt variables before each prompt draw. A persistent `ClientSession` amortizes the connection cost across multiple queries — three fields for the price of one connection.

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

## bash prompt (PROMPT_COMMAND)

bash runs `PROMPT_COMMAND` before each prompt. Parse the `key=value` text output from a whole-provider query to minimize subprocess calls.

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

## fish prompt function

fish's `fish_prompt` function is called before each prompt. fish has no subshell penalty for command substitutions, so this is already efficient.

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
