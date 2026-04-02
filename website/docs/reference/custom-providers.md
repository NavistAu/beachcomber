---
sidebar_position: 5
title: Custom Providers
---

# Custom Providers

Custom providers let you add any data source to beachcomber using any language. Your script runs on the configured schedule, and the results are cached and served to all consumers.

## Output Formats

**JSON (default):** Stdout must be a JSON object. Top-level keys become provider fields.

```sh
# A provider that outputs JSON
#!/bin/sh
docker context show --format '{"context":"{{.Name}}","driver":"{{.Driver}}"}'
```

```toml
[providers.docker_ctx]
command = "~/.config/beachcomber/providers/docker-context.sh"
output = "json"
```

**Key-value:** Stdout is `key=value` lines, one per field. Simpler for shell scripts.

```sh
# A provider using kv output
#!/bin/sh
context=$(docker context show 2>/dev/null || echo "default")
echo "context=${context}"
```

```toml
[providers.docker_ctx]
command = "~/.config/beachcomber/providers/docker-context.sh"
output = "kv"
```

**Text:** Stdout is a single value, exposed as the `value` field. For commands that print one thing.

```sh
# Single-value output
node --version 2>/dev/null | tr -d 'v'
```

```toml
[providers.node_version]
command = "node --version | tr -d v"
output = "text"
```

Then query with `comb get node_version.value -f text`.

## Invalidation Strategies

**Poll only:** Re-run every N seconds. Use for data that changes independently of filesystem events.

```toml
[providers.vpn_status]
command = "~/.config/beachcomber/providers/vpn-check.sh"
output = "kv"

[providers.vpn_status.invalidation]
poll = "10s"
```

**Watch only:** Re-run when specific files change. Use for data that's determined entirely by file content.

```toml
[providers.ruby_version]
command = "rbenv version-name"
output = "text"
scope = "path"

[providers.ruby_version.invalidation]
watch = [".ruby-version", "Gemfile", ".tool-versions"]
```

**Watch with poll fallback (recommended):** FSEvents and inotify can occasionally drop events under heavy load. A poll fallback ensures eventual consistency even if an event is missed.

```toml
[providers.cargo_meta]
command = "cargo metadata --format-version=1 --no-deps --quiet"
output = "json"
scope = "path"

[providers.cargo_meta.invalidation]
watch = ["Cargo.toml", "Cargo.lock"]
poll = "120s"
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
[providers.docker_context]
command = "~/.config/beachcomber/providers/docker-context.sh"
output = "json"

[providers.docker_context.invalidation]
poll = "30s"
```

Query: `comb get docker_context.context -f text`

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
[providers.node_version]
command = "~/.config/beachcomber/providers/node-version.sh"
output = "kv"
scope = "path"

[providers.node_version.invalidation]
watch = [".node-version", ".nvmrc", "package.json", ".mise.toml"]
poll = "60s"
```

**Ruby version via rbenv:**
```toml
[providers.ruby_version]
command = "rbenv version-name 2>/dev/null || ruby --version | cut -d' ' -f2"
output = "text"
scope = "path"

[providers.ruby_version.invalidation]
watch = [".ruby-version", "Gemfile", ".tool-versions"]
poll = "120s"
```

Query: `comb get ruby_version.value -f text`

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
[providers.vpn]
command = "~/.config/beachcomber/providers/vpn-status.sh"
output = "kv"

[providers.vpn.invalidation]
poll = "10s"
```

Query: `comb get vpn.active -f text`

## HTTP Providers

For providers that fetch data from REST APIs, beachcomber has a built-in HTTP provider type. This makes HTTP requests directly in the daemon process — no `curl` fork, no shell spawning, with connection reuse and proper timeout handling.

> **Note:** You can also use script providers with `curl` for quick-and-dirty HTTP queries. But for anything polling regularly, the `http` type is significantly more efficient — it avoids 2-6ms of process spawn overhead per request.

**Basic API status check:**

```toml
[providers.claude_status]
type = "http"
url = "https://status.anthropic.com/api/v2/summary.json"
extract = "status"
invalidation = { poll = "60s" }
```

Query: `comb get claude_status.indicator -f text` returns `"none"`, `"minor"`, `"major"`, etc.

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

Query: `comb get github_rate.remaining -f text`

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

Query: `comb get exchange.value -f text` — returns the AUD rate, refreshed daily.

**Comparison — script vs HTTP for the same task:**

Using a script provider (forks `sh` + `curl` every poll):
```toml
[providers.api_status_script]
type = "script"
command = "curl -s https://status.anthropic.com/api/v2/summary.json"
invalidation = { poll = "60s" }
```

Using the HTTP provider (in-process, no fork):
```toml
[providers.api_status_http]
type = "http"
url = "https://status.anthropic.com/api/v2/summary.json"
invalidation = { poll = "60s" }
```

Both produce the same result. The HTTP version skips the ~5ms process spawn overhead and handles connection failures more gracefully.

## Secrets and Environment Variables

HTTP headers and script commands support `${VAR}` expansion, pulling values from the daemon's environment. But the daemon's environment depends on how it starts — socket activation inherits the env of whatever triggered it, which is unpredictable.

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

Then `chmod 600` and restart the daemon (`pkill -f 'comb daemon'` — it socket-activates on next query).

## Script Provider Tips

- **Exit codes:** A non-zero exit is treated as a failure. The last cached value is retained. After 3 consecutive failures, the provider enters exponential backoff (up to 60s).
- **Stderr:** Stderr output from script providers is captured and logged at `debug` level. It does not affect the result.
- **Timeouts:** Script providers are subject to `provider_timeout_secs` (default 10s). Long-running scripts are cancelled and retried on the next trigger.
- **Shell:** Commands are executed via `sh -c`. Use absolute paths for reliability, or ensure your PATH is set correctly in the daemon's environment.
- **Path-scoped providers:** If `scope = "path"`, the script is called with the directory path as its working directory. Use `$PWD` inside the script to reference it.
- **Performance:** Every process spawn costs 2-6ms minimum. For providers that poll frequently (< 30s), prefer reading config files over spawning CLI tools. See the design principles in `docs/performance.md`.
