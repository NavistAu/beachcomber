#!/bin/sh
# beachcomber chpwd hook — poke path-scoped providers on directory change.
# Warms git, mise, terraform, python, direnv, asdf caches before the first
# prompt renders in a new directory.
#
# Install: source this file from your shell rc, or curl it down:
#   curl -fsSL https://beachcomber.sh/scripts/chpwd.sh | source /dev/stdin
#
# Requires: comb (beachcomber CLI) in PATH.
# https://beachcomber.sh

_beachcomber_poke_providers() {
    command -v comb >/dev/null 2>&1 || return
    comb r git "$PWD" >/dev/null 2>&1 &
    comb r mise "$PWD" >/dev/null 2>&1 &
    comb r terraform "$PWD" >/dev/null 2>&1 &
    comb r python "$PWD" >/dev/null 2>&1 &
    comb r direnv "$PWD" >/dev/null 2>&1 &
    comb r asdf "$PWD" >/dev/null 2>&1 &
}

# --- zsh ---
if [ -n "$ZSH_VERSION" ]; then
    autoload -Uz add-zsh-hook
    add-zsh-hook chpwd _beachcomber_poke_providers
fi

# --- bash ---
if [ -n "$BASH_VERSION" ]; then
    _beachcomber_oldpwd="$PWD"
    _beachcomber_chpwd_bash() {
        if [ "$PWD" != "$_beachcomber_oldpwd" ]; then
            _beachcomber_oldpwd="$PWD"
            _beachcomber_poke_providers
        fi
    }
    PROMPT_COMMAND="_beachcomber_chpwd_bash${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
fi

# --- fish ---
# Fish users: save this block as ~/.config/fish/conf.d/beachcomber-chpwd.fish
#   function _beachcomber_chpwd --on-variable PWD
#       command -q comb; or return
#       comb r git $PWD &>/dev/null &
#       comb r mise $PWD &>/dev/null &
#       comb r terraform $PWD &>/dev/null &
#       comb r python $PWD &>/dev/null &
#       comb r direnv $PWD &>/dev/null &
#       comb r asdf $PWD &>/dev/null &
#   end
