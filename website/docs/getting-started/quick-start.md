---
sidebar_position: 2
---

# Quick Start

## Verify

The daemon starts automatically on first use — no setup required.

```sh
# Query your current git branch (run from inside a git repo)
comb get git.branch . -f text

# Query battery
comb get battery.percent -f text

# Check daemon status
comb status
```

That's it. The daemon started in the background when you ran that first query.

## Try it in your prompt

```sh
# Add to ~/.zshrc
precmd() {
    PS1="%F{blue}$(comb get git.branch . -f text 2>/dev/null)%f %# "
}
```

Source your `.zshrc` and open a few more shells. Then run `comb status` — you'll see the cache entry being shared across all shells, with a single filesystem watcher covering all of them.
