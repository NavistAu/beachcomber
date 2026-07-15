---
sidebar_position: 20
---

# Troubleshooting

Common issues when integrating beachcomber with shells, editors, and status bars.

## Daemon not running

The daemon is socket-activated — it starts automatically on the first `comb` call. If `comb s` exits with a connection error and `comb --version` works, the binary is likely not on the `PATH` that your tool (starship, tmux, a bar program) is running under. See [comb not found in non-interactive shells](#comb-not-found-in-non-interactive-shells) below.

For deeper investigation (log location, log level, foreground mode), see the [Debugging](../about/debugging.md) page.

## Verifying a query

Before adding a `comb g` call to any tool's config, test it directly in your terminal:

```sh
# comb g returns plain text by default. g = get, no suffix needed.
# Path-scoped provider — pass a directory
comb g git.branch .

# Global provider — no path needed
comb g battery.percent
```

If the command returns nothing, the provider has no data for the current context. This is normal — for example, `git.branch` returns empty outside a git repository, and `kubecontext.context` returns empty when no context is set.

## Path-scoped providers return empty or wrong data

Providers like `git`, `terraform`, `python`, `direnv`, `mise`, and `asdf` are path-scoped. They require a directory argument so beachcomber knows which project to query.

In shells, pass `.` and the shell's working directory is used:

```sh
comb g git.branch .
```

In tmux `#()` format strings, the command runs in the tmux server's working directory, not the pane's. Use tmux's format variable:

```
#(comb g git.branch "#{pane_current_path}")
```

In bar programs (polybar, waybar, sketchybar), `.` resolves to wherever the bar process started. Always pass an absolute path:

```sh
comb g git.branch /home/yourname/myproject
```

Global providers (battery, load, network, kubecontext, gcloud, aws, hostname, uptime, user, conda) do not take a path argument.

## Stale data

The daemon polls providers on a timer. If you just switched branches or changed a context, the cache may not have refreshed yet. Force an immediate recomputation using `--force`:

```sh
comb g --force git .
```

See [CLI commands](../reference/cli-commands.md) for every command and its short form.

## comb not found in non-interactive shells

Some tools (starship, tmux, bar programs) run commands in non-interactive shells that may not source your `.zshrc` or `.bashrc`. If `comb` is installed to a path only added by your interactive shell config, those tools will not find it.

Check:

```sh
/bin/sh -c 'which comb'
```

If this returns nothing, either:
- Add the directory containing `comb` to `/etc/paths` (macOS) or `/etc/environment` (Linux)
- Use the absolute path to `comb` in your config

## Provider not available

If a key always returns empty, the provider may not be enabled or supported on your platform:

Check your `~/.config/beachcomber/config.toml` if a provider you expect is missing. See the [built-in providers reference](../reference/built-in-providers.md) for every provider and its fields, and [platform notes](../reference/built-in-providers.md) for fields that need optional components (e.g. UPower on Linux for `battery.time_remaining`).

## First query is slow

On the first query for a provider, the daemon may not have a cached value yet and executes the provider inline. Subsequent queries return the cached value immediately.

To eliminate this delay after a `cd`, install the [chpwd hook](./shell-helpers.md) — it pokes relevant providers in the background whenever you change directory, so the cache is warm by the time your prompt renders.

## Socket discovery

SDKs and the CLI find the daemon socket in this order:

1. Config file override (if set in `~/.config/beachcomber/config.toml`)
2. `$BEACHCOMBER_SOCKET` environment variable
3. `/tmp/beachcomber-<uid>/sock`

Run `comb check daemon` to see the active socket path and daemon status. In neovim, check for an override: `:lua print(vim.env.BEACHCOMBER_SOCKET)`. (Note: `comb s` shows cache rows, not the socket path.)

## Nerd Font glyphs not rendering

Several integration examples use Nerd Font icons (branch symbol, battery, kubernetes). If these appear as boxes or question marks, your terminal font does not include the glyphs. Install a [Nerd Font](https://www.nerdfonts.com/) or replace the icons with text labels in your config.
