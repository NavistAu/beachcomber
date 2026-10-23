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

`exec('…')` runs a string under `sh -c` with the daemon's environment; `exec(['cmd', 'arg'])` runs one argument per element, with no shell. Without a `return` line, the Source takes the abstract Source's schedule: polled every 60 seconds while queried, backed off after repeated failures.

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

## HTTP Providers

For providers that fetch data from REST APIs, beachcomber has a built-in HTTP provider type. This makes HTTP requests directly in the daemon process — no `curl` fork, no shell spawning, with connection reuse and proper timeout handling.

> **Note:** You can also use a Source whose `data` calls `exec('curl …')` for quick-and-dirty HTTP queries. But for anything polling regularly, the `http` type is significantly more efficient — it avoids 2-6ms of process spawn overhead per request.

**Basic API status check:**

```toml
[providers.claude_status]
type = "http"
url = "https://status.anthropic.com/api/v2/summary.json"
extract = "status"
invalidation = { poll = "60s" }
```

Query: `comb g claude_status.indicator` returns `"none"`, `"minor"`, `"major"`, etc.

The `extract` field navigates into the JSON response using dot-separated paths. Without it, the entire response object becomes the provider's fields.

**Authenticated API with headers:**

```toml
[providers.github_rate]
type = "http"
url = "https://api.github.com/rate_limit"
headers = { Authorization = "Bearer ${GITHUB_TOKEN}", Accept = "application/json" }
extract = "rate"
invalidation = { poll = "30s" }
```

Query: `comb g github_rate.remaining`

Header values support `${ENV_VAR}` expansion — secrets stay in your environment, not in config files.

**Service health endpoint:**

```toml
[providers.api_health]
type = "http"
url = "https://internal.example.com/health"
invalidation = { poll = "10s" }
```

If the endpoint returns JSON, top-level keys become fields. If it returns non-JSON, the raw body is available as the `body` field.

**Exchange rate (infrequent poll):**

```toml
[providers.exchange]
type = "http"
url = "https://api.exchangerate-api.com/v4/latest/USD"
extract = "rates.AUD"
invalidation = { poll = "86400s" }
```

Query: `comb g exchange.value` — returns the AUD rate, refreshed daily.

**Comparison — `exec('curl')` vs HTTP for the same task:**

Using a Source (forks `sh` + `curl` every poll):
```toml
[daemon.providers.api_status_script.sources.main]
data = "{{ exec('curl -s https://status.anthropic.com/api/v2/summary.json') | fromjson }}"
result = "{{ data }}"
return = "{{ result | poll('60s') }}"
```

Using the HTTP provider (in-process, no fork):
```toml
[providers.api_status_http]
type = "http"
url = "https://status.anthropic.com/api/v2/summary.json"
invalidation = { poll = "60s" }
```

Both produce the same result. The HTTP version skips the ~5ms process spawn overhead and handles connection failures more gracefully.

### Failure handling and timeouts

HTTP providers share the same lifecycle as config Sources:

- **`provider_timeout_secs`** (default `10`, set under `[daemon]`) — the maximum time a single request may take. Requests that exceed this are cancelled and the last good cached value is retained.
- **`failback_count`** (default `3`) — the number of consecutive failures before the provider enters exponential backoff (up to 60s). Applies to any non-2xx response, connection error, or timeout.
- **`failback_interval`** (default `"1s"`) — the wait between retry attempts before backoff kicks in.

All three can be overridden per-source under `[providers.<name>.<source>]`. A transient 5xx or network error will not remove the last cached value — consumers will see `stale: true` on the wire (`comb g.j <provider>:cache.<field>` from the CLI) once the value is past its expected refresh time.

## Shared Library Providers

For maximum performance, you can write providers as shared libraries (`.so` on Linux, `.dylib` on macOS). The daemon loads the library once at startup and calls into it directly — no process spawning, no shell overhead.

```toml
[providers.my_native]
type = "library"
library_path = "/usr/local/lib/beachcomber/libmy_provider.so"
```

The library must export three C-compatible functions:

- `beachcomber_provider_metadata()` — returns JSON describing fields, invalidation strategy, and scope
- `beachcomber_provider_execute(path)` — returns JSON field values (or NULL on failure)
- `beachcomber_provider_free(ptr)` — frees strings returned by the above

See the [Provider Development Guide](/docs/internals/provider-development#8-shared-library-providers) for the full C ABI contract and example code.

## Secrets and Environment Variables

HTTP headers support `${VAR}` expansion, and a Source's `data` line sees the daemon's environment as `env` (a shell command run by `exec` inherits it), pulling values from the daemon's environment. But the daemon's environment depends on how it starts — socket activation inherits the env of whatever triggered it, which is unpredictable.

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

Then reference these in provider configs:

```toml
[providers.github_rate]
type = "http"
url = "https://api.github.com/rate_limit"
headers = { Authorization = "Bearer ${GITHUB_TOKEN}" }
invalidation = { poll = "30s" }
```

The `${GITHUB_TOKEN}` is expanded at request time from the daemon's environment (which includes the env file values).

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

- **Exit codes:** A non-zero exit from `exec` fails the refresh. The last cached value is retained. After repeated failures (the `failback(count, interval)` decorator; the abstract Source ships `failback(3, '1s')`), the Source enters exponential backoff from the interval (default 1s, 4 doubling levels).
- **Stderr:** A command that succeeds has its stderr dropped. One that fails (a non-zero exit, a timeout) has the tail of its stderr in the error message the daemon logs for the refresh.
- **Timeouts:** `exec` kills a command after `timeout_ms` (default 10000, 10 s): `exec('slow-tool', timeout_ms=30000)`. A timeout fails the refresh, and the next trigger retries it.
- **Shell:** A string is executed via `sh -c`; a list (`exec(['cmd', 'arg'])`) is one argument per element with no shell. Use absolute paths for reliability, or ensure your PATH is set correctly in the daemon's environment.
- **Path-scoped Sources:** `data` runs with the instance's `root` as its working directory. A Source with no `root` is the one instance at `~`.
- **Performance:** Every process spawn costs 2-6ms minimum. For Sources that poll frequently (< 30s), prefer `read` and `lup` over spawning CLI tools. See the design principles in `docs/performance.md`.
