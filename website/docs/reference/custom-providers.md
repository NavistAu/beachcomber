---
sidebar_position: 5
title: Custom Providers
---

# Custom Providers

A custom provider is a **Source**: a function with a body (`data`), the fields it returns (`result`), and a schedule (`return`). `data` runs on the daemon, on the Source's schedule; the results are cached and served to all consumers.

```toml
[daemon.providers.demo.sources.s]
data = "{{ exec('cat ' ~ env.HOME ~ '/demo.json') | fromjson }}"
result = "{{ data }}"
```

`exec('…')` runs a string under `sh -c` with the daemon's environment; `exec(['cmd', 'arg'])` runs one argument per element, with no shell. `$BAR` inside the string is expanded by the shell, which the engine cannot see (a future `cache()` hash would miss it): prefer `exec('cmd ' ~ env.bar)`. Without a `return` line, the Source takes the abstract Source's schedule: polled every 60 seconds while queried, backed off after repeated failures.

## Output Formats

The shape of the output is the filter after `exec`.

**JSON:** Stdout is a JSON object; `| fromjson` parses it. `result = "{{ data }}"` makes its top-level keys the provider's fields, or `result.<field>` lines pick them.

```toml
[daemon.providers.docker_ctx.sources.main]
data = "{{ exec('~/.config/beachcomber/providers/docker-context.sh') | fromjson }}"
result = "{{ data }}"
```

**Key-value:** Stdout is `key=value` lines. `| fromini` reads them (keys before any `[section]` land under `''`).

```toml
[daemon.providers.docker_ctx.sources.main]
data = "{{ exec('~/.config/beachcomber/providers/docker-context.sh') | fromini }}"
result.context = "{{ data[''].context }}"
```

**Text:** Stdout is a single value. `| trim` drops the trailing newline.

```toml
[daemon.providers.node_version.sources.main]
data = "{{ exec('node --version | tr -d v') | trim }}"
result.value = "{{ data }}"
```

Then query with `comb g node_version.value`.

## Invalidation Strategies

The decorators on `return` say when `data` runs again.

**Poll only:** Re-run every N seconds. Use for data that changes independently of filesystem events.

```toml
[daemon.providers.vpn_status.sources.main]
data = "{{ exec('~/.config/beachcomber/providers/vpn-check.sh') | fromini }}"
result.active = "{{ data[''].active }}"
return = "{{ result | poll('10s') }}"
```

**Watch only:** Re-run when specific files change. Use for data that's determined entirely by file content. Relative paths are relative to `root`; a path starting with `/` or `~` is absolute.

```toml
[daemon.providers.ruby_version.sources.main]
root = "{{ lup('.ruby-version') | dirname }}"
data = "{{ exec('rbenv version-name') | trim }}"
result.value = "{{ data }}"
return = "{{ result | watch('.ruby-version', 'Gemfile', '.tool-versions') }}"
```

**Watch with poll fallback (recommended):** FSEvents and inotify can occasionally drop events under heavy load. A poll fallback ensures eventual consistency even if an event is missed.

```toml
[daemon.providers.cargo_meta.sources.main]
root = "{{ lup('Cargo.toml') | dirname }}"
data = "{{ exec('cargo metadata --format-version=1 --no-deps --quiet') | fromjson }}"
result = "{{ data }}"
return = "{{ result | watch('Cargo.toml', 'Cargo.lock') | poll('120s') }}"
```

## Real-World Examples

**Docker context provider:**
```sh
#!/bin/sh
# ~/.config/beachcomber/providers/docker-context.sh
# Outputs the active Docker context and whether it's remote.

context=$(docker context show 2>/dev/null || echo "default")
endpoint=$(docker context inspect "$context" --format '{{.Endpoints.docker.Host}}' 2>/dev/null || echo "")

is_remote="false"
case "$endpoint" in
    tcp://*|ssh://*) is_remote="true" ;;
esac

printf '{"context":"%s","remote":%s}\n' "$context" "$is_remote"
```

```toml
[daemon.providers.docker_context.sources.main]
data = "{{ exec('~/.config/beachcomber/providers/docker-context.sh') | fromjson }}"
result = "{{ data }}"
return = "{{ result | poll('30s') }}"
```

Query: `comb g docker_context.context`

**Node.js version provider (path-scoped):**
```sh
#!/bin/sh
# ~/.config/beachcomber/providers/node-version.sh
# Reports the Node.js version in effect for the current directory.
# Respects .nvmrc, .node-version, and volta/mise if installed.

if command -v mise >/dev/null 2>&1; then
    version=$(mise current node 2>/dev/null)
elif command -v node >/dev/null 2>&1; then
    version=$(node --version 2>/dev/null | tr -d v)
fi

echo "version=${version:-unknown}"
```

```toml
[daemon.providers.node_version.sources.main]
root = "{{ lup('package.json') | dirname }}"
data = "{{ exec('~/.config/beachcomber/providers/node-version.sh') | fromini }}"
result.version = "{{ data[''].version }}"
return = "{{ result | watch('.node-version', '.nvmrc', 'package.json', '.mise.toml') | poll('60s') }}"
```

`data` runs with the instance's `root` as its working directory.

**Ruby version via rbenv:**
```toml
[daemon.providers.ruby_version.sources.main]
root = "{{ lup('.ruby-version') | dirname }}"
data = '''{{ exec("rbenv version-name 2>/dev/null || ruby --version | cut -d' ' -f2") | trim }}'''
result.value = "{{ data }}"
return = "{{ result | watch('.ruby-version', 'Gemfile', '.tool-versions') | poll('120s') }}"
```

Query: `comb g ruby_version.value`

**VPN connected check:**
```sh
#!/bin/sh
# ~/.config/beachcomber/providers/vpn-status.sh
# Checks whether a VPN tunnel is active.

# Look for any utun interface with an IP (macOS)
if ifconfig 2>/dev/null | grep -q '^utun.*flags'; then
    # Check if a utun has an inet address (not just link-local)
    if ifconfig 2>/dev/null | awk '/^utun/{iface=$1} /inet / && iface{print; iface=""}' | grep -q inet; then
        echo "active=true"
        # Try to get VPN name from pf/scutil
        name=$(scutil --nc list 2>/dev/null | grep Connected | head -1 | sed 's/.*"\(.*\)".*/\1/')
        echo "name=${name:-vpn}"
        exit 0
    fi
fi

echo "active=false"
echo "name="
```

```toml
[daemon.providers.vpn.sources.main]
data = "{{ exec('~/.config/beachcomber/providers/vpn-status.sh') | fromini }}"
result.active = "{{ data[''].active }}"
result.name = "{{ data[''].name }}"
return = "{{ result | poll('10s') }}"
```

Query: `comb g vpn.active`

## HTTP

A Source fetches over HTTP by calling `url()` in its `data` line. The request is made in the daemon process — no `curl` fork, no shell spawning, with proper timeout handling. `url(address, method='GET', headers={}, body=none, timeout='10s')` returns the response body as text; pipe it through `fromjson` to get a value `result` can read. `method` is `GET`, `POST`, `PUT` or `PATCH`. A non-2xx status fails the render, so the Source's execute fails and its `failback` applies.

> **Note:** `exec('curl …')` also works for a quick query, but for anything polling regularly `url()` avoids the 2-6ms process spawn per request.

**Basic API status check:**

```toml
[daemon.providers.claude_status.sources.summary]
data = "{{ url('https://status.anthropic.com/api/v2/summary.json') | fromjson }}"
result.indicator = "{{ data.status.indicator }}"
return = "{{ result | poll('60s') }}"
```

Query: `comb g claude_status.indicator` returns `"none"`, `"minor"`, `"major"`, etc.

`result.<field>` lines pick fields out of the parsed response; `result = "{{ data.status }}"` instead makes the whole `status` object the provider's node.

**Authenticated API with headers:**

```toml
[daemon.providers.github_rate.sources.main]
data = "{{ url('https://api.github.com/rate_limit', headers={'Authorization': 'Bearer ' ~ env.GITHUB_TOKEN, 'Accept': 'application/json'}) | fromjson }}"
result = "{{ data.rate }}"
return = "{{ result | poll('30s') }}"
```

Query: `comb g github_rate.remaining`

Secrets stay in the daemon's environment (`env.GITHUB_TOKEN`), not in config files.

**Service health endpoint:**

```toml
[daemon.providers.api_health.sources.main]
data = "{{ url('https://internal.example.com/health') | fromjson }}"
result = "{{ data }}"
return = "{{ result | poll('10s') }}"
```

If the endpoint returns JSON, its top-level keys become fields. For a non-JSON body, leave out `fromjson` and set `result.body = "{{ data }}"`.

**Exchange rate (infrequent poll):**

```toml
[daemon.providers.exchange.sources.main]
data = "{{ url('https://api.exchangerate-api.com/v4/latest/USD') | fromjson }}"
result.value = "{{ data.rates.AUD }}"
return = "{{ result | poll('86400s') }}"
```

Query: `comb g exchange.value` — returns the AUD rate, refreshed daily.

### Failure handling and timeouts

A Source that calls `url()` has the same lifecycle as any config Source:

- **`timeout`** (default `"10s"`, set under `[daemon]`, or per Source with `| timeout('30s')` on its `return`) — the maximum time a single execute may take; `url()` also takes its own `timeout=` (default `'10s'`). A request that exceeds the limit fails and the last good cached value is retained.
- **`failback(interval, count)`** on the `return` line (the shipped default is `failback('1s', 3)`) — after `count` consecutive failures the Source backs off exponentially, the first step being `interval` and the cap 8× `interval` (8 s with the shipped default). Attempts before the count are not delayed. The interval comes first, as in `poll`, and is a duration string: a number there (`failback(3, '1s')`, the count-first order) is a load error that says the order. A non-2xx response, a connection error and a timeout all count.

A transient 5xx or network error will not remove the last cached value — consumers will see `stale: true` on the wire (`comb g.j <provider>:cache.<field>` from the CLI) once the value is past its expected refresh time.

## Plugins: shared libraries

For work where a process spawn is too expensive, a Source's `data` line can call a function in a shared library (`.so` on Linux, `.dylib` on macOS) with `plugin(name, symbol, args...)`. The library is found on `[daemon.plugin] path` (default `~/.config/beachcomber/plugins`), loaded on first call, and its JSON result comes back parsed.

```toml
[daemon.providers.my_native.sources.main]
data = "{{ plugin('my_native', 'read_state', 'fast') }}"
result.value = "{{ data.value }}"
return = "{{ result | poll('10s') }}"
```

This calls `read_state` in `libmy_native.so` (or `.dylib`). A symbol is `const char* symbol(const char* input)`: it receives the arguments as a JSON array and returns a JSON value, or `NULL` on failure.

One plugin is compiled into `comb`: `sys`, for what only the operating system can say. Its symbols take no arguments and answer an object, leaving out what the machine has no value for:

| call | answers |
|---|---|
| `plugin('sys', 'hostname')` | `{name, short}` |
| `plugin('sys', 'loadavg')` | `{one, five, fifteen}` |
| `plugin('sys', 'uname')` | `{sysname, release, version, machine}` |
| `plugin('sys', 'uptime')` | `{seconds}` |
| `plugin('sys', 'user')` | `{name, uid}` |
| `plugin('sys', 'network')` | `{interface, ip, ipv6, vpn_active, vpn_name, ssid, online}` |
| `plugin('sys', 'battery')` | `{percent, charging, status, time_remaining_secs}` (and `status_raw` on Linux), `{}` with no battery |

`sys` is reached the way a library's symbols are, through the same C ABI, and is never looked for on `[daemon.plugin] path`: a `libsys.so` or `libsys.dylib` there is never loaded, and `comb check config` warns of it.

The shipped `hostname`, `user`, `uname`, `load` and `uptime` providers are Sources over these symbols, each in its own managed `defaults/<name>.toml`; any of their lines can be overridden in `config.toml` or `conf.d/`.

See the [Provider Development Guide](/docs/internals/provider-development#8-plugins) for the full C ABI contract, example code, and where each `sys` fact comes from.

## Secrets and Environment Variables

A Source's `data` line sees the daemon's environment as `env` (a shell command run by `exec` inherits it), pulling values from the daemon's environment. But the daemon's environment depends on how it starts — socket activation inherits the env of whatever triggered it, which is unpredictable.

The solution: **env files.** The daemon loads `~/.config/beachcomber/env` at startup before any providers execute, guaranteeing a consistent environment regardless of how the daemon was started.

```sh
# ~/.config/beachcomber/env
# This file is loaded by the daemon at startup.
# Format: KEY=VALUE (one per line). Blank lines and #comments are ignored.
# Values can be quoted: KEY="value with spaces" or KEY='single quoted'

GITHUB_TOKEN=ghp_xxxxxxxxxxxxxxxxxxxx
ANTHROPIC_API_KEY=sk-ant-xxxxxxxxxxxx
ANTHROPIC_ADMIN_KEY=sk-admin-xxxxxxxxxxxx
EXCHANGE_API_KEY=abc123
```

**Protect this file:**
```sh
chmod 600 ~/.config/beachcomber/env
```

Then reference these in a Source's `data` line:

```toml
[daemon.providers.github_rate.sources.main]
data = "{{ url('https://api.github.com/rate_limit', headers={'Authorization': 'Bearer ' ~ env.GITHUB_TOKEN}) | fromjson }}"
result = "{{ data.rate }}"
return = "{{ result | poll('30s') }}"
```

`env.GITHUB_TOKEN` comes from the daemon's environment as it stood when the daemon started, after the env file loaded. An unset variable renders empty, so `'Bearer ' ~ env.GITHUB_TOKEN` sends `Bearer ` and the API refuses it.

**Custom env file path:** If you keep secrets elsewhere:

```toml
[daemon]
env_file = "~/.secrets/beachcomber.env"
```

**Integration with secret managers:** Generate the env file from your secret manager of choice:

```sh
# 1Password
op read "op://Vault/beachcomber/env" > ~/.config/beachcomber/env

# pass
pass show beachcomber/env > ~/.config/beachcomber/env

# macOS Keychain
security find-generic-password -s beachcomber -w > ~/.config/beachcomber/env

# Vault
vault kv get -field=env secret/beachcomber > ~/.config/beachcomber/env
```

Then `chmod 600` and restart the daemon (`comb kill` — it socket-activates on next query).

## Source Tips

- **Exit codes:** A non-zero exit from `exec` fails the refresh. The last cached value is retained. After repeated failures (the `failback(interval, count)` decorator; the abstract Source ships `failback('1s', 3)`), the Source enters exponential backoff from the interval (default 1s, 4 doubling levels).
- **Stderr:** A command that succeeds has its stderr dropped. One that fails (a non-zero exit, a timeout) has the tail of its stderr in the error message the daemon logs for the refresh.
- **Timeouts:** `exec` kills a command after `timeout` (default `'10s'`): `exec('slow-tool', timeout='30s')`. A timeout fails the refresh, and the next trigger retries it.
- **Shell:** A string is executed via `sh -c`; a list (`exec(['cmd', 'arg'])`) is one argument per element with no shell. Use absolute paths for reliability, or ensure your PATH is set correctly in the daemon's environment.
- **Path-scoped Sources:** `data` runs with the instance's `root` as its working directory. A Source with no `root` is the one instance at `~`.
- **Performance:** Every process spawn costs 2-6ms minimum. For Sources that poll frequently (< 30s), prefer `read` and `lup` over spawning CLI tools. See the design principles in `docs/performance.md`.
