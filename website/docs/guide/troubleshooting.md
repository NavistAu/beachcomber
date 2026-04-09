---
sidebar_position: 20
---

# Troubleshooting

Common issues when integrating beachcomber with shells, editors, and status bars.

## Daemon not running

If `comb get` returns nothing or exits with a connection error, the daemon is not running. Start it:

```sh
comb daemon &
```

Then verify:

```sh
comb status
```

## Verifying a query

Before adding a `comb get` call to any tool's config, test it directly in your terminal:

```sh
# Path-scoped provider — pass a directory
comb get git.branch . -f text

# Global provider — no path needed
comb get battery.percent -f text
```

If the command returns nothing, the provider has no data for the current context. This is normal — for example, `git.branch` returns empty outside a git repository, and `kubecontext.context` returns empty when no context is set.

## Path-scoped providers return empty or wrong data

Providers like `git`, `terraform`, `conda`, `python`, `direnv`, `mise`, and `asdf` are path-scoped. They require a directory argument so beachcomber knows which project to query.

In shells, pass `.` and the shell's working directory is used:

```sh
comb get git.branch . -f text
```

In tmux `#()` format strings, the command runs in the tmux server's working directory, not the pane's. Use tmux's format variable:

```
#(comb get git.branch "#{pane_current_path}" -f text)
```

In bar programs (polybar, waybar, sketchybar), `.` resolves to wherever the bar process started. Always pass an absolute path:

```sh
comb get git.branch /home/yourname/myproject -f text
```

Global providers (battery, load, network, kubecontext, gcloud, aws, hostname, uptime, user) do not take a path argument.

## Stale data

The daemon polls providers on a timer. If you just switched branches or changed a context, the cache may not have refreshed yet. Force an immediate refresh:

```sh
comb poke git .
```

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

```sh
comb list
```

This shows all registered providers. Check your `~/.config/beachcomber/config.toml` if a provider you expect is missing.

## First query is slow

On the first query for a provider, the daemon may not have a cached value yet and executes the provider inline. Subsequent queries return the cached value immediately. A short delay after daemon startup resolves on its own within one polling cycle.

## Socket discovery

SDKs and the CLI find the daemon socket in this order:

1. Config file override (if set in `~/.config/beachcomber/config.toml`)
2. `$XDG_RUNTIME_DIR/beachcomber/sock`
3. `$TMPDIR/beachcomber-<uid>/sock`

Run `comb status` to see the active socket path. In neovim, check what the environment resolves to: `:lua print(vim.env.XDG_RUNTIME_DIR)`.

## Nerd Font glyphs not rendering

Several integration examples use Nerd Font icons (branch symbol, battery, kubernetes). If these appear as boxes or question marks, your terminal font does not include the glyphs. Install a [Nerd Font](https://www.nerdfonts.com/) or replace the icons with text labels in your config.
