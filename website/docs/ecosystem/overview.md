---
sidebar_position: 1
---

# Ecosystem Overview

beachcomber has client SDKs for seven languages plus a POSIX shell polyfill. All SDKs are stdlib-only (no external dependencies) and are published to their native package registries.

## Install

| SDK | Registry | Install |
|---|---|---|
| [Rust](./rust-sdk.md) | [crates.io](https://crates.io/crates/beachcomber-client) | `cargo add beachcomber-client` |
| [Python](./python-sdk.md) | [PyPI](https://pypi.org/project/libbeachcomber/) | `pip install libbeachcomber` |
| [Node.js](./nodejs-sdk.md) | [npm](https://www.npmjs.com/package/libbeachcomber) | `npm install libbeachcomber` |
| [Go](./go-sdk.md) | [Go module](https://github.com/NavistAu/beachcomber/tree/main/sdks/go) | `go get github.com/NavistAu/beachcomber/sdks/go` |
| [Lua](./lua-sdk.md) | [LuaRocks](https://luarocks.org/modules/navist/libbeachcomber) | `luarocks install libbeachcomber` |
| [Ruby](./ruby-sdk.md) | [RubyGems](https://rubygems.org/gems/libbeachcomber) | `gem install libbeachcomber` |
| [C](./c-sdk.md) | [GitHub Release](https://github.com/NavistAu/beachcomber/releases) | Source tarball, `.deb`, `.rpm`, or [AUR](https://aur.archlinux.org/packages/libbeachcomber) |
| Shell / POSIX | — | See the [Polyfill](/docs/integrating/polyfill) |

The Rust SDK ships as `beachcomber-client` on crates.io; the other language SDKs are `libbeachcomber` on their native registries.

## When to use an SDK vs the CLI

Shelling out to `comb g.` is fine for one-off queries from shell scripts, prompts, and status bars. For anything that queries more than a handful of keys per second, or needs to stream updates, use a language SDK:

- **Persistent connections** — open a Unix socket once, reuse it for every query (avoids per-call socket setup).
- **Connection context** — set a working directory once with `set_context`; subsequent path-scoped `get` calls don't need to repeat the path.
- **Typed results** — scalar/object dispatch in idiomatic types for each language.
- **Lower latency** — skip `exec`/`fork` overhead; SDK `get` is ~15 µs vs ~34 µs for the CLI.

## Feature matrix

All SDKs cover the common read path. `put` and `watch` are protocol-level operations that are not yet exposed in any language SDK — use the raw [protocol](../reference/protocol-reference.md) over a Unix socket to drive them, or call `comb w` / `comb p` on the CLI.

| SDK | `get` | `refresh` | `context` / session | `list` | `status` | `put` | `watch` |
|---|---|---|---|---|---|---|---|
| Rust | ✓ | ✓ | ✓ | ✓ | ✓ | — | — |
| Python | ✓ | ✓ | ✓ | ✓ | ✓ | — | — |
| Node.js | ✓ | ✓ | ✓ | ✓ | ✓ | — | — |
| Go | ✓ | ✓ | ✓ | ✓ | ✓ | — | — |
| Lua | ✓ | ✓ | ✓ | ✓ | ✓ | — | — |
| Ruby | ✓ | ✓ | ✓ | ✓ | ✓ | — | — |
| C | ✓ | ✓ | ✓ | ✓ | ✓ | — | — |

Need a language that isn't listed? The [protocol](../reference/protocol-reference.md) is newline-delimited JSON over a Unix socket — any language that can open a socket can be a client in a few dozen lines.
