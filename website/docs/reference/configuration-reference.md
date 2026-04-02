---
sidebar_position: 3
title: Configuration Reference
---

# Configuration Reference

beachcomber runs with sensible defaults and requires no configuration. The optional config file lives at `~/.config/beachcomber/config.toml`.

```toml
# ~/.config/beachcomber/config.toml

# ─── Daemon ────────────────────────────────────────────────────────────────────

[daemon]

# Override the Unix socket path.
# Default: $XDG_RUNTIME_DIR/beachcomber/sock
#          Falls back to: $TMPDIR/beachcomber-<uid>/sock
socket_path = ""

# Log level for daemon output.
# Options: "error", "warn", "info", "debug", "trace"
# Default: "info"
# Logs go to: $XDG_STATE_HOME/beachcomber/daemon.log
log_level = "info"

# Maximum time (in seconds) to wait for any provider to complete.
# Providers that exceed this are cancelled; the last good cached value is retained.
# Default: 10
provider_timeout_secs = 10

# Path to an environment file loaded at daemon startup.
# Each line is KEY=VALUE (or KEY="VALUE"). Blank lines and #comments are ignored.
# These vars are available to ${VAR} expansion in HTTP headers, script commands, etc.
# Default: ~/.config/beachcomber/env (loaded automatically if present)
# env_file = "~/.config/beachcomber/env"


# ─── Lifecycle ─────────────────────────────────────────────────────────────────

[lifecycle]

# How long (in seconds) to maintain full polling cadence after demand expires
# (i.e., after the last query for a key). During this window, cache entries
# stay warm in case a new shell opens.
# Default: 30
grace_period_secs = 30

# How long (in seconds) after demand expires before a cache entry is fully
# evicted. The daemon enters a progressive backoff between grace expiry and
# eviction (2x poll at 30s, 4x poll at 2min, frozen at 5min).
# Default: 900 (15 minutes)
eviction_timeout_secs = 900

# How long (in seconds) the daemon waits with no active connections before
# shutting itself down. The next client connection will socket-activate a
# fresh instance.
# Set to null to disable idle shutdown (daemon stays resident permanently).
# Default: 300 (5 minutes)
idle_shutdown_secs = 300


# ─── Built-in Provider Overrides ───────────────────────────────────────────────
# Use [providers.<name>] to override defaults for any built-in provider.
# Only specify the fields you want to change.

# Disable a provider entirely (it will never execute or appear in results)
[providers.conda]
enabled = false

# Override polling interval and floor for battery
[providers.battery]
poll_secs = 60          # default: 30
poll_floor_secs = 10    # default: 5

# Make git refresh more frequently (useful on fast machines or large repos)
[providers.git]
poll_secs = 30          # default: no poll (filesystem-triggered only)
poll_floor_secs = 2     # default: not set

# Override network polling interval
[providers.network]
poll_secs = 30          # default: 10
poll_floor_secs = 10    # default: 5


# ─── Custom Script Providers ───────────────────────────────────────────────────
# Define your own providers backed by any executable.

# Minimal: a global provider that polls every 30 seconds
[providers.docker_context]
command = "docker context show"
output = "text"         # single-line output becomes { "value": "<output>" }
# or use output = "json" for structured output: { "key": value, ... }
# or use output = "kv" for key=value line format

[providers.docker_context.invalidation]
poll = "30s"

# A path-scoped provider that watches a file and has a poll fallback
[providers.node_version]
command = "node --version"
output = "text"
scope = "path"          # scoped to a directory; path argument required

[providers.node_version.invalidation]
watch = [".node-version", ".nvmrc", "package.json"]
poll = "60s"            # safety-net poll in case filesystem events are missed

# A provider with structured JSON output
[providers.cargo_meta]
command = "cargo metadata --format-version=1 --no-deps --quiet"
output = "json"         # parse stdout as JSON object; top-level keys become fields
scope = "path"

[providers.cargo_meta.invalidation]
watch = ["Cargo.toml", "Cargo.lock"]
poll = "120s"

# Explicitly disable a custom provider without removing its config
[providers.my_slow_thing]
command = "my-slow-script"
enabled = false


# ─── HTTP Providers ──────────────────────────────────────────────────────────
# Fetch data directly from REST APIs — no curl fork, no shell spawning.
# Uses in-process HTTP client with connection reuse.

# Basic: poll a status API
[providers.service_status]
type = "http"
url = "https://status.anthropic.com/api/v2/summary.json"
extract = "status"              # dot-path into the JSON response
                                # e.g., response.status.indicator → provider field "indicator"

[providers.service_status.invalidation]
poll = "60s"

# With auth headers (env vars expanded at runtime)
[providers.github_rate]
type = "http"
url = "https://api.github.com/rate_limit"
headers = { Authorization = "Bearer ${GITHUB_TOKEN}" }
extract = "rate"                # extracts { "limit": 5000, "remaining": 4999, ... }

[providers.github_rate.invalidation]
poll = "30s"

# Infrequent poll (daily)
[providers.exchange_rate]
type = "http"
url = "https://api.exchangerate-api.com/v4/latest/USD"
extract = "rates.AUD"           # extracts a single nested value

[providers.exchange_rate.invalidation]
poll = "86400s"
```

## Config field summary

**`[daemon]` section:**

| Field | Type | Default | Description |
|---|---|---|---|
| `socket_path` | string | `$XDG_RUNTIME_DIR/beachcomber/sock` | Unix socket path |
| `log_level` | string | `"info"` | Tracing log level |
| `provider_timeout_secs` | int | `10` | Max seconds for any provider to run |
| `env_file` | string | `~/.config/beachcomber/env` | Path to env file loaded at startup |

**`[lifecycle]` section:**

| Field | Type | Default | Description |
|---|---|---|---|
| `grace_period_secs` | int | `30` | Seconds at full cadence after demand expires |
| `eviction_timeout_secs` | int | `900` | Seconds until cache entry is fully evicted |
| `idle_shutdown_secs` | int or null | `null` (disabled) | Seconds until idle daemon shuts down |

**`[providers.<name>]` section (built-in overrides):**

| Field | Type | Default | Description |
|---|---|---|---|
| `enabled` | bool | `true` | Set `false` to disable provider entirely |
| `poll_secs` | int | provider-specific | Override poll interval |
| `poll_floor_secs` | int | provider-specific | Minimum poll interval consumers can request |

**`[providers.<name>]` section (custom script providers):**

| Field | Type | Required | Description |
|---|---|---|---|
| `command` | string | yes | Shell command to execute |
| `output` | string | no | `"json"` (default), `"kv"`, or `"text"` |
| `scope` | string | no | `"global"` (default) or `"path"` |
| `enabled` | bool | no | `false` to disable |
| `poll_secs` | int | no | Poll interval in seconds |
| `poll_floor_secs` | int | no | Minimum poll interval |
| `invalidation.poll` | string | no | Poll interval as duration string (`"30s"`, `"2m"`) |
| `invalidation.watch` | array of strings | no | File/directory patterns to watch |

**`[providers.<name>]` section (HTTP providers):**

| Field | Type | Required | Description |
|---|---|---|---|
| `type` | string | yes | Must be `"http"` |
| `url` | string | yes | URL to fetch. Supports `${ENV_VAR}` expansion. |
| `method` | string | no | HTTP method: `"GET"` (default), `"POST"`, `"PUT"` |
| `headers` | table | no | HTTP headers. Values support `${ENV_VAR}` expansion. |
| `body` | string | no | Request body (for POST/PUT) |
| `extract` | string | no | Dot-separated path into the JSON response (e.g., `"status.indicator"`, `"rates.AUD"`) |
| `enabled` | bool | no | `false` to disable |
| `invalidation.poll` | string | no | Poll interval (default `"60s"`, floor `5s`) |
