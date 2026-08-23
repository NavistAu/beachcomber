---
sidebar_position: 1
title: For Integrators
---

# For Integrators

If you maintain a shell framework, prompt theme, status bar config, or terminal tool and want to add beachcomber support without requiring it as a dependency, this page explains the recommended patterns.

## The polyfill approach (recommended)

`scripts/polyfill.sh` defines a `comb()` shell function that stands in for the real binary. If `comb` is already installed, it does nothing — the real binary takes precedence. If `comb` is not installed, the function handles `comb g <key>` calls by falling back to native tools for known keys.

Your integration code calls `comb g git.branch .` everywhere. Users who have beachcomber installed get cached, microsecond-latency results. Users who do not get the native fallback transparently. Neither group needs to change your integration code.

Tell your users to source the polyfill once in their shell rc:

```sh
source <(curl -fsSL https://beachcomber.sh/scripts/polyfill.sh)
```

See the [Polyfill](./polyfill.md) page for the full list of covered keys and fallback implementations.

## Detection without dependency

If you prefer explicit detection over the polyfill, check for `comb` at runtime:

```sh
command -v comb >/dev/null 2>&1
```

The typical pattern for optional usage:

```sh
comb g git.branch . 2>/dev/null || git rev-parse --abbrev-ref HEAD 2>/dev/null
```

Exit codes are stable and documented:

| Code | Meaning |
|------|---------|
| 0 | Success — value printed to stdout |
| 1 | Cache miss or key not found |
| 2 | Error (daemon not running, socket problem) |

Use exit code 1 to distinguish "no value" from "comb not working" when you need to differentiate those cases.

## What to tell your users

In your documentation, a single sentence is enough:

> Install [beachcomber](https://beachcomber.sh) for faster git status, kubernetes context, and environment lookups in your prompt.

Link to `https://beachcomber.sh` for installation instructions. If your integration uses the polyfill, mention that it degrades gracefully:

> This theme uses beachcomber when available. Without it, the polyfill falls back to native commands automatically — no configuration needed.

## Don't bundle the binary

It's tempting to ship `comb` alongside your distribution so users don't need to install it separately. The MIT license allows this, but it creates real problems:

**The daemon is shared state.** beachcomber runs one daemon per user with a single Unix socket. If your tool bundles comb 0.4.0 and the user installs comb 0.5.0, whichever runs first starts the daemon. The other then talks to a daemon from a different version. Version skew across a shared socket is detected — the client compares its build identity against the daemon's on first use per connection — and surfaced as a non-fatal error.

**Update responsibility.** A security fix in beachcomber means every bundler needs to ship an update independently. With a system install, the user updates once.

**Platform matrix.** You'd need to ship per-arch binaries for every platform you support (macOS arm64/x86_64, Linux x86_64/aarch64 at minimum).

The recommended approach is the three-tier pattern:

1. **Polyfill** — works everywhere, zero binary dependency, graceful degradation
2. **Recommend install** — "install beachcomber for faster X" in your docs, link to beachcomber.sh
3. **Detect and use** — `command -v comb`, use if present, polyfill fallback if not

This keeps the user in control of their beachcomber version, avoids daemon conflicts, and still gives the full performance benefit when beachcomber is installed.

## Protocol stability

The CLI is the stable interface. Treat these as guaranteed:

- `comb g <key> [path]` — get a value as plain text, one line, no trailing newline
- `comb g.j <key> [path]` — get a value as JSON
- Exit codes 0 / 1 / 2 as described above
- Key format: `provider.field` (e.g., `git.branch`, `battery.percent`)

The Unix socket protocol and JSON wire format are stable for SDK authors. See the [Ecosystem Overview](/docs/ecosystem/overview) for available SDKs if you need deeper integration (watch subscriptions, bulk gets, or access from non-shell languages).

## Examples of integrations

**Starship** — custom module in `starship.toml`:

```toml
[custom.git_branch]
command = "comb g git.branch ."
when = "command -v comb"
```

The built-in git module can be disabled when beachcomber is present to avoid double work. See the [Starship integration](/docs/integrations/starship) page for a complete setup.

**tmux** — format strings in `tmux.conf`:

```sh
set -g status-right "#(comb g battery.percent)%"
```

tmux runs the command on each status refresh. With beachcomber, every refresh is a cache read. See the [tmux integration](/docs/integrations/tmux) page for a full status bar example.

**oh-my-zsh** — theme functions calling comb directly:

```sh
function _ohmyzsh_git_branch() {
  comb g git.branch . 2>/dev/null || git_current_branch
}
```

Drop this into a custom theme or plugin. The fallback keeps it working for users without beachcomber. See the [oh-my-zsh integration](/docs/integrations/oh-my-zsh) page for a complete theme example.
