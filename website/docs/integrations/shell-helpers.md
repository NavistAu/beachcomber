---
sidebar_position: 11
---

# Shell Helpers

beachcomber ships a cache-warming hook that fires on directory changes. It is not a fallback script — it requires `comb` and is designed to eliminate the one-time cache miss when you `cd` into a new directory.

> If you are looking for the opposite — a shim that makes integrations work _without_ `comb` installed — see the [Polyfill](../integrating/polyfill.md).

## chpwd hook

`scripts/chpwd.sh` is a directory-change hook that pokes beachcomber whenever you `cd` into a new directory. It sends pokes for `git`, `mise`, `terraform`, `python`, `direnv`, and `asdf` — so by the time you run your first command in a new directory, the cache is already warm. Without this hook, the first query after a `cd` pays the cold-cache synchronous execution cost (see [Troubleshooting → first query is slow](./troubleshooting.md)).

### Install

```sh
mkdir -p ~/.config/beachcomber
curl -fsSL https://beachcomber.sh/scripts/chpwd.sh -o ~/.config/beachcomber/chpwd.sh
```

### Shell setup

| Shell | Method |
|-------|--------|
| zsh | `add-zsh-hook chpwd` |
| bash | `PROMPT_COMMAND` |
| fish | `conf.d/` file that sources the downloaded script |

**zsh** — add to `~/.zshrc`:

```sh
source ~/.config/beachcomber/chpwd.sh
autoload -Uz add-zsh-hook
add-zsh-hook chpwd _beachcomber_chpwd
```

**bash** — add to `~/.bashrc`:

```sh
source ~/.config/beachcomber/chpwd.sh
PROMPT_COMMAND="${PROMPT_COMMAND:+$PROMPT_COMMAND; }_beachcomber_chpwd"
```

**fish** — create `~/.config/fish/conf.d/beachcomber.fish` that sources the downloaded file and wires it up:

```fish
source ~/.config/beachcomber/chpwd.sh
function _beachcomber_chpwd_hook --on-variable PWD
    _beachcomber_chpwd
end
```

### Behaviour

The hook is a no-op if `comb` is not installed — the `command -v comb` check at the top of the script exits silently when the binary is missing.

All refreshes run in the background (`&`), so the hook adds no delay to your prompt or directory change. If the daemon is down, the background refresh fails silently; the next foreground `comb g` will surface the error.
