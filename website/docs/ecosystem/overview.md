---
sidebar_position: 1
title: Ecosystem Overview
---

# Ecosystem Overview

beachcomber has client libraries for seven languages. All of them answer from the same Rust library,
`libbeachcomber`: Rust links it directly, and the others load its C build (`libbeachcomber.dylib` / `.so`) through
their language's foreign-function interface. A value is therefore the same in every language as it is from
`comb get`. [How the Libraries Work](./how-the-libraries-work.md) explains the layers, how each SDK finds the shared
library, and what the fallbacks give up.

## Choose a library

| Language | Package | Binding | Fallback | Install |
|---|---|---|---|---|
| [Rust](./rust-sdk.md) | [`libbeachcomber`](https://crates.io/crates/libbeachcomber) on crates.io | the crate itself | none needed | `cargo add libbeachcomber` |
| [Python](./python-sdk.md) | [`libbeachcomber`](https://pypi.org/project/libbeachcomber/) on PyPI | stdlib `ctypes` | none | `pip install libbeachcomber` |
| [Ruby](./ruby-sdk.md) | [`libbeachcomber`](https://rubygems.org/gems/libbeachcomber) on RubyGems | stdlib `fiddle` | none | `gem install libbeachcomber` |
| [Go](./go-sdk.md) | [`github.com/NavistAu/beachcomber/sdks/go`](https://github.com/NavistAu/beachcomber/tree/main/sdks/go) | `purego` (vendored, no cgo) | none | `go get github.com/NavistAu/beachcomber/sdks/go` |
| [Node.js](./nodejs-sdk.md) | [`libbeachcomber`](https://www.npmjs.com/package/libbeachcomber) on npm | `koffi` (optional peer) | spawns `comb` when `koffi` is not installed | `npm install libbeachcomber koffi` |
| [Lua](./lua-sdk.md) | [`libbeachcomber`](https://luarocks.org/modules/navist/libbeachcomber) on LuaRocks | LuaJIT `ffi` | spawns `comb` under PUC Lua | `luarocks install libbeachcomber` |
| [C](./c-sdk.md) | [GitHub Release](https://github.com/NavistAu/beachcomber/releases): tarball, `.deb`, `.rpm`, [AUR](https://aur.archlinux.org/packages/libbeachcomber) | the C ABI itself (`-lbeachcomber`) | none | from your package manager |
| Shell | none | `comb` | none | see the [Polyfill](/docs/integrating/polyfill) |

Every SDK except Rust and C needs `libbeachcomber.{dylib,so}` at run time, found beside `comb` on `$PATH` or through
`$BEACHCOMBER_LIB` ([discovery](./how-the-libraries-work.md#the-discovery-rule)). Node without `koffi` and PUC Lua
instead run `comb` once per call, which is slower and lacks `hello`, `introspect` and some options
([subprocess tiers](./how-the-libraries-work.md#subprocess-tiers)).

## When to use an SDK instead of the CLI

Running `comb get` is fine for shell scripts, prompts and status bars that read a few keys. Use an SDK when you read
many keys, read often, or need to follow changes:

- **No process per read.** An SDK call is a library call; a CLI read starts a process.
- **Sessions.** Keep one connection open, and set a working directory once with `set_context`; later `get`, `put` and
  `refresh` calls that name no path use it.
- **Watches.** Receive a key's value again each time it changes.
- **Typed results.** Values arrive as the language's own strings, numbers, maps and lists; a miss is its null.

## One API

Every SDK has the same operations under the same names, with the same arguments, value shapes and error kinds,
written out in [`docs/sdk-api.md`](https://github.com/NavistAu/beachcomber/blob/main/docs/sdk-api.md) and checked by
each SDK's tests against [`sdks/api.json`](https://github.com/NavistAu/beachcomber/blob/main/sdks/api.json). Names
follow the language's casing (`set_context`, `setContext`, `SetContext`).

| Object | Methods |
|---|---|
| Client | `get`, `eval`, `watch`, `put`, `refresh`, `status`, `hello`, `introspect`, `session`, `close` |
| Session | `set_context`, `get`, `put`, `refresh`, `close` |
| Watch stream | `next_event`, `cancel`, `close` |

A client takes `socket_path`, `timeout_ms`, `connect_timeout_ms`, `autostart` and `exec`; any left out takes the
library's default. `exec` is off by default, so a template that calls `exec()` fails on a client unless you turn it
on ([why](./how-the-libraries-work.md#exec-is-off-on-a-client)). `get` answers the value `comb get` prints, and null
on a miss. Each failure is one error type per kind (`daemon_not_running`, `timeout`, `server_error`, …), carrying the
kind, the library's message and the library's version.

Need a language that isn't listed? The [protocol](../reference/protocol-reference.md) is newline-delimited JSON over a
Unix socket, so any language that can open one can be a client. A hand-written client gets cached values and
templates from the daemon, but must render templates itself; binding the C ABI gets you the same renderer as `comb`.
