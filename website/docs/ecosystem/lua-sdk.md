---
sidebar_position: 6
---

# Lua SDK

Lua client for the beachcomber daemon. A binding over `libbeachcomber`'s C ABI, not a hand-rolled socket protocol. Published on [LuaRocks](https://luarocks.org/modules/navist/libbeachcomber).

Two transports, selected automatically by `comb.connect()`: under LuaJIT (including Neovim, which always ships LuaJIT), `ffi` calls straight into the shared library (~0.3ms/call); under PUC Lua 5.1–5.4, which has no `ffi`, the SDK falls back to shelling out to the `comb` binary (~5ms/call). Call `client:transport()` to see which one is active.

It follows the [SDK API table](https://github.com/NavistAu/beachcomber/blob/main/docs/sdk-api.md) every SDK follows: the same operations, names, value shapes and error kinds, which its test suite checks against `sdks/api.json`. Every method returns its value or `nil, err`; a miss is `nil` with no error, and a method that answers nothing returns `true`. Optional arguments are an options table.

## Requirements

- Lua 5.1+ or LuaJIT (Neovim-compatible)
- **LuaJIT (incl. Neovim):** no extra dependencies — `ffi` binds directly to `libbeachcomber.{so,dylib}`
- **PUC Lua 5.1–5.4:** no extra dependencies — subprocess fallback needs only a `comb` binary on `$PATH`

## Installation

### LuaRocks

```sh
luarocks install --local libbeachcomber
```

### Manual (Neovim)

Add `sdks/lua` to your `package.path`, or copy the `beachcomber/` directory somewhere on your `runtimepath`.

## Quick start

```lua
local comb = require('beachcomber')

-- Connect (LuaJIT: ffi into libbeachcomber; PUC Lua: comb subprocess)
local client, err = comb.connect()
if not client then
  error("beachcomber: " .. err)
end

-- get a single field: what `comb get git.branch` prints there, nil on a miss
local branch = client:get('git.branch', { cwd = '/my/repo' })
if branch then print(branch) end  -- "main"

-- get a full provider (returns a table)
local git = client:get('git', { cwd = '/my/repo' })
if git then print(git.branch) end  -- "main"

-- the template itself, unrendered: a facet, read like any key
local tpl = client:get('git:template.branch')

-- refresh (force recompute)
client:refresh('git', { path = '/my/repo' })

-- a session keeps a directory of its own for the calls that name none
local s = client:session()
s:set_context('/my/repo')
local branch2 = s:get('git.branch')  -- reads at the session's directory
s:close()

-- daemon cache status rows
local rows = client:status()
for _, row in ipairs(rows or {}) do
  print(row.provider, row.field, row.age_ms)
end

client:close()
```

### Custom socket path

```lua
local client = comb.connect({ socket_path = '/run/user/1000/beachcomber/sock' })
```

## API reference

### `comb.connect([opts])` → `Client | nil, error`

Connect to the daemon and return a `Client`.

| Option | Type | Description |
|---|---|---|
| `socket_path` | string | Override automatic socket discovery |
| `timeout_ms` | number | `ffi` transport: how long one read or write on the socket may take. Left out, the library's default (100); the SDK has none of its own. 0 is refused |
| `connect_timeout_ms` | number | `ffi` transport: how long connecting keeps retrying a socket nobody answers yet; 0 is a single attempt. Left out, the library's default (1500) |
| `exec` | boolean | `ffi` transport: let templates this client renders call `exec()`. Off by default — a template that calls it then fails with `exec() is disabled in this context` |
| `backend` | module | Override the socket backend (advanced) |

The subprocess transport cannot bound a spawned `comb`, so `comb.connect` refuses `timeout_ms` and `connect_timeout_ms` there: it returns `nil` and an `unsupported` error naming the `ffi` transport.

Returns `nil, err` on failure.

### `Client:get(key [, opts])` → `value | nil, error`

The value `comb get` prints for this key, cwd and env: the daemon holds the templates (`<provider>:template`, seeded from configuration or `put`) and the client renders them. A plain field is its cached value, a templated one (`kubecontext.context`, `python.version`) is rendered, and a bare provider name is the whole merged namespace; a miss is `nil` with no error. The unrendered halves are facets, read like any key: `p:template.x` is the template as stored, `p:put.x` and `p:source.x` those layers' values. `key` is `"provider"` or `"provider.field"`.

`opts.cwd` is the directory the read addresses (default: the global entry); `opts.env` is the caller's environment for `env.*` refs (default: the calling process's environment, as `comb` renders); `opts.force` re-executes every Source the read demands first, as `comb get --force`; `opts.wait` is how long to wait for a run the read needs before answering what is stored, as `comb get --wait`: a duration (`"0"`, `"50ms"`, `"2s"`) or `"forever"`, anything else a `"parse_error"` on both transports. A template that calls `exec()` needs `exec = true` on the connect. Over `subprocess` a spawned `comb` renders with its own environment: `opts.env` is an `"unsupported"` error there, and a `get` without it sees this process's environment.

### `Client:refresh(key [, opts])` → `true | nil, error`

Re-execute the Sources `key` demands — every Source of the namespace for a bare `provider`, the one Source owning the field for `provider.field`. A key that names no Source (a `put`-created namespace, a facet key, an unknown name, a field no Source owns) is an error. Over `subprocess` this is one `comb refresh <key>`; the daemon's refusal comes back as `err.kind == "server_error"` in the daemon's words. `opts.path` is the directory (default: the global entry).

### `Client:status()` → `table[] | nil, error`

Return the cache rows (one per warm cache entry) as an array of tables with the fields `libbeachcomber::protocol::CacheRow` defines — `provider`, `path` (`nil` for the global entry), `source`, `field`, `value`, `age_ms` and the lifecycle fields. A status that cannot be read is `nil, err` on either transport, never an empty list.

### `Client:hello()` → `table | nil, error`

Handshake — returns a table with `daemon_version` and `protocol_version` fields.

### `Client:put(key, data [, opts])` → `true | nil, error`

Write a value into the cache as static data. Pass `nil` (or `beachcomber.null`) for `data` to remove the node. `opts.ttl` (e.g. `"30s"`), `opts.path` (default: the global entry).

### `Client:introspect(subject [, opts])` → `table | nil, error`

Inspect a daemon subsystem. `subject` is a string: `"daemon"`, `"providers"`, `"config"`, `"cache"`, `"lifecycle"`, `"watches"`, `"timers"`, `"demand"`, or `"procs"`; `opts.duration_secs` is the `procs` subject's only option. Answers `{subject, daemon}` for `"daemon"` and `{subject, other}` for the rest.

### `Client:watch(key [, opts])` → `WatchStream | nil, error`

Subscribe to a key's value — what `Client:get` answers for it — and receive it again whenever it changes; a template edited under the watch fires it just as a changed cache record does. `opts.cwd` and `opts.env` as `Client:get` takes them. Returns a `WatchStream`. `stream:next_event([timeout_ms])` answers the event `{data}` (whose `data` carries no age or staleness; `nil` data is a miss), `nil` with no error once the stream has ended, or `nil, err` of kind `"timeout"` when `timeout_ms` ran out; no `timeout_ms` waits indefinitely. `stream:cancel()` ends it, `stream:close()` cancels and releases it, and `for event in stream:each() do … end` iterates it. `ffi` transport only.

### `Client:eval(template_str [, opts])` → `value | nil, error`

Render a template that is stored nowhere — the same value `comb eval` prints, every ref resolved as it would be inside a stored template, so `eval("{{ p.f }}")` and `get("p.f")` agree. `opts.cwd` and `opts.env` as `Client:get` takes them (no `cwd` is the global entry). `ffi` transport only.

### `Client:session()` → `Session | nil, error`

A session: one connection and a directory of its own. `s:set_context(path)` sets the directory; `s:get(key [, opts])`, `s:put(key, data [, opts])` and `s:refresh(key [, opts])` are the client's, addressing that directory when they name none; `s:close()` closes it. Over `subprocess` there is no connection to keep, and the session remembers its directory between spawns.

### `Client:close()` → `true`

Release the client.

### Errors

An `err` carries `kind`, one of `Error.KINDS` — the ABI's `daemon_not_running`, `connection_failed`, `io_error`, `parse_error`, `server_error`, `timeout`, `busy`, `panic` and `version_skew`, and this SDK's own `library_not_found`, `library_symbol` and `unsupported`; `message`, the ABI's message as it came; and `library_version`, the loaded library's version (`nil` over `subprocess`). Branch on `kind`.

## Wire protocol reference

All operations follow the wire contract defined in [docs/protocol-spec.md](https://github.com/NavistAu/beachcomber/blob/main/docs/protocol-spec.md).

## Library discovery

The ffi transport finds `libbeachcomber` by the rule every SDK follows
([`docs/sdk-api.md`](https://github.com/NavistAu/beachcomber/blob/main/docs/sdk-api.md#library-discovery),
"Library discovery"). The file is `libbeachcomber.dylib` on macOS and
`libbeachcomber.so` on every other platform, and the candidates are, in
order:

1. `$BEACHCOMBER_LIB`, when it is set and not empty. It is then the only
   candidate: a path set on purpose that does not load is an error, not a
   reason to load some other library. `opts.library_path` is the same thing
   for a caller that cannot set the environment (standard Lua has no
   `setenv`).
2. Beside `comb`: `<dir>/../lib/<file>`, where `<dir>` is the directory of
   the first executable `comb` on `$PATH`, its symlinks resolved. A Homebrew
   `comb` is a symlink into the Cellar, and the library sits in the Cellar
   beside it; a `.deb` or `.rpm` puts them in `/usr/bin` and `/usr/lib`.
3. The system loader, given the bare file name.

The first candidate that loads is used. When none does, the error is
`library_not_found`, naming every candidate tried and why each failed — a
missing library is a broken install, not a silent downgrade to `subprocess`.

## Socket path discovery

The SDK looks for the daemon socket in this order:

1. `$BEACHCOMBER_SOCKET` (if set and non-empty)
2. `/tmp/beachcomber-<uid>/sock`

This mirrors the daemon's bind path; no session-scoped environment (`$TMPDIR`, `$XDG_RUNTIME_DIR`) is consulted, so every shell of one user reaches the same daemon.

## Neovim example

```lua
-- In your statusline plugin or lualine component:
local ok, comb = pcall(require, 'beachcomber')
if not ok then return '' end

local client = comb.connect()
if not client then return '' end

-- Keep a persistent client across calls for best performance
vim.api.nvim_create_autocmd('VimLeavePre', {
  callback = function() client:close() end,
})

local function git_branch()
  local branch = client:get('git.branch', { cwd = vim.fn.getcwd() })
  if not branch then return '' end
  return ' ' .. branch
end
```

Every `ffi` call is synchronous, so `git_branch()` can be called directly from a statusline evaluation without scheduling callbacks.
