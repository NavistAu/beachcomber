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
# A provider with a root (it walks up from a directory) — pass the directory
comb g git.branch .

# A provider with no root — no path needed
comb g battery.percent
```

If the command returns nothing, the provider has no data for the current context. This is normal — for example, `git.branch` returns empty outside a git repository, and `kubecontext.context` is empty when no kubectl context is set or `kubectl` is not installed. A template that shells out fails only when its command fails for a reason the template does not expect: `comb g` then prints the error to stderr and exits `2`.

## Rooted providers return empty or wrong data

Providers like `git`, `terraform`, `python`, `direnv`, `mise`, and `asdf` have a root: they walk up from a directory to find their project. With no directory argument a read addresses the current directory, so a tool that runs from somewhere else must name the project.

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

Providers with no root (battery, load, network, hostname, uptime, user, sudo, uname) answer the same from any directory and need no path argument, and neither do the template-backed namespaces that read only the shell's environment or such a provider (`aws`, `gcloud`, `conda`, `op`, `kubecontext`, `talos`).

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

If a key always returns empty, the provider may be switched off (`enabled = false` under `[daemon.providers.<name>]`) or not supported on your platform:

Check your `~/.config/beachcomber/config.toml` if a provider you expect is missing. See the [built-in providers reference](../providers/built-in-providers.md) for every provider and its fields, and [platform notes](../providers/built-in-providers.md) for fields a platform may not give (e.g. `network.ssid` on macOS 14.4 and later).

## First query is slow

On the first query for a provider, the daemon may not have a cached value yet and executes the provider inline. Subsequent queries return the cached value immediately.

To eliminate this delay after a `cd`, install the [chpwd hook](../integrations/shell-helpers.md) — it pokes relevant providers in the background whenever you change directory, so the cache is warm by the time your prompt renders.

## Socket discovery

SDKs and the CLI find the daemon socket in this order:

1. Config file override (if set in `~/.config/beachcomber/config.toml`)
2. `$BEACHCOMBER_SOCKET` environment variable
3. `/tmp/beachcomber-<uid>/sock`

Run `comb check daemon` to see the active socket path and daemon status. In neovim, check for an override: `:lua print(vim.env.BEACHCOMBER_SOCKET)`. (Note: `comb s` shows cache rows, not the socket path.)

## Nerd Font glyphs not rendering

Several integration examples use Nerd Font icons (branch symbol, battery, kubernetes). If these appear as boxes or question marks, your terminal font does not include the glyphs. Install a [Nerd Font](https://www.nerdfonts.com/) or replace the icons with text labels in your config.
