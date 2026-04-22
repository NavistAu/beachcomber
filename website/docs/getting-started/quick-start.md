---
sidebar_position: 2
---

# Quick Start

No setup is required. beachcomber's daemon starts automatically on the first `comb` query and shuts down after an idle period. You don't start or stop it yourself.

## Your first query

Inside a git repository, run:

```sh
comb get git.branch .
```

You should see the branch name (e.g. `main`).

What just happened:

- `get` — read a cached value (short form: `g`).
- `git.branch` — the key. Keys are `provider.field`. `git` is the provider; `branch` is the field.
- `.` — the path to query. `git` is a **path-scoped** provider, so it needs to know which repo you mean. `.` means the current directory.

Output is plain text by default — just the value, no JSON envelope. That's designed for shell substitution like `branch=$(comb g git.branch .)`.

Now query a **global** provider (no path needed):

```sh
comb get battery.percent
```

You should see a number like `87`.

## The short form

Every command has a one-letter alias. These are equivalent:

```sh
comb get git.branch .
comb g   git.branch .
```

When you need a different output format, append a suffix:

| Suffix | Format |
|--------|--------|
| _(none)_ | plain text (default) |
| `.j` | JSON envelope (with `age_ms`, `stale`, etc.) |
| `.s` | shell `key=value` lines — sourceable |
| `.c` / `.C` | CSV (optionally with header) |
| `.t` / `.T` | TSV (optionally with header) |
| `.f` | template string with `{field}` placeholders |

See the [CLI reference](../reference/cli-commands.md) for the full list. Examples:

```sh
comb g.j git.branch .           # JSON envelope
comb g.s git .                  # all git fields as key=value
comb g.f '{branch} ({dirty})' git .   # templated
```

## What's running

The daemon started in the background. Ask it what it's doing:

```sh
comb s          # status — uptime, cache entries, watcher count
```

You can stop the daemon at any time with `comb kill` (short form `comb k`). The next `comb` query will start a fresh one. You rarely need to do this.

## Try it in your prompt

Put this in your `~/.zshrc`:

```zsh
precmd() {
    PS1="%F{blue}$(comb g git.branch . 2>/dev/null)%f %# "
}
```

Source your `.zshrc` and open a few more shells in the same repo. Run `comb s` — you'll see the cache entry is shared across every shell, with a single filesystem watcher covering all of them.

## Next steps

- **[What's Available](./whats-available.md)** — the 19 built-in providers and what they expose.
- **[How It Works](./how-it-works.md)** — the daemon architecture in one page.
- **[Integrations](../integrations/shell.md)** — wire beachcomber into your prompt, status bar, or editor.
