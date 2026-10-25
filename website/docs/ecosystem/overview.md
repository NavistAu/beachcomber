---
sidebar_position: 1
---

# Ecosystem Overview

beachcomber has client SDKs for seven languages plus a POSIX shell polyfill. Each SDK is a binding over the shared `libbeachcomber` C ABI rather than a hand-rolled wire-protocol client: Go uses `purego` (vendored), Python uses stdlib `ctypes`, Ruby uses stdlib `fiddle`, Lua uses LuaJIT `ffi` (with a subprocess fallback for PUC Lua), and Node.js uses `koffi` as an optional peer dependency (falling back to a `comb` subprocess when it isn't installed). All are published to their native package registries.

## Install

| SDK | Registry | Install |
|---|---|---|
| [Rust](./rust-sdk.md) | [crates.io](https://crates.io/crates/libbeachcomber) | `cargo add libbeachcomber` |
| [Python](./python-sdk.md) | [PyPI](https://pypi.org/project/libbeachcomber/) | `pip install libbeachcomber` |
| [Node.js](./nodejs-sdk.md) | [npm](https://www.npmjs.com/package/libbeachcomber) | `npm install libbeachcomber` |
| [Go](./go-sdk.md) | [Go module](https://github.com/NavistAu/beachcomber/tree/main/sdks/go) | `go get github.com/NavistAu/beachcomber/sdks/go` |
| [Lua](./lua-sdk.md) | [LuaRocks](https://luarocks.org/modules/navist/libbeachcomber) | `luarocks install libbeachcomber` |
| [Ruby](./ruby-sdk.md) | [RubyGems](https://rubygems.org/gems/libbeachcomber) | `gem install libbeachcomber` |
| [C](./c-sdk.md) | [GitHub Release](https://github.com/NavistAu/beachcomber/releases) | Source tarball, `.deb`, `.rpm`, or [AUR](https://aur.archlinux.org/packages/libbeachcomber) |
| Shell / POSIX | — | See the [Polyfill](/docs/integrating/polyfill) |

The Rust SDK ships as `libbeachcomber` on crates.io, matching the other language SDKs' `libbeachcomber` naming on their native registries.

## When to use an SDK vs the CLI

Shelling out to `comb g.` is fine for one-off queries from shell scripts, prompts, and status bars. For anything that queries more than a handful of keys per second, or needs to stream updates, use a language SDK:

- **Persistent connections** — open a Unix socket once, reuse it for every query (avoids per-call socket setup).
- **Session directory** — set a working directory once with `set_context`; the session sends it with every later `get`, `put` and `refresh` that names no path.
- **Typed results** — scalar/object dispatch in idiomatic types for each language.
- **Lower latency** — skip `exec`/`fork` overhead; SDK `get` is ~15 µs vs ~34 µs for the CLI.

## One API

Every SDK follows one table — the same operations under the same names, the same arguments, the same value shapes and the same error kinds — written out in [`docs/sdk-api.md`](https://github.com/NavistAu/beachcomber/blob/main/docs/sdk-api.md) and checked by each SDK's own tests against [`sdks/api.json`](https://github.com/NavistAu/beachcomber/blob/main/sdks/api.json). Names follow the language's casing (`set_context`, `setContext`, `SetContext`).

| Object | Methods |
|---|---|
| Client | `get`, `eval`, `watch`, `put`, `refresh`, `status`, `hello`, `introspect`, `session`, `close` |
| Session | `set_context`, `get`, `put`, `refresh`, `close` |
| Watch stream | `next_event`, `cancel`, `close` |

A client takes `socket_path`, `timeout_ms`, `connect_timeout_ms`, `autostart` and `exec`, each with the library's default when left out. `get` answers the value `comb get` prints, and null on a miss. Each failure is one error type per kind (`daemon_not_running`, `timeout`, `server_error`, …), carrying the kind, the library's message and the library's version. The Rust crate is the library itself, and the C SDK is its ABI.

Need a language that isn't listed? The [protocol](../reference/protocol-reference.md) is newline-delimited JSON over a Unix socket — any language that can open a socket can be a client in a few dozen lines.
