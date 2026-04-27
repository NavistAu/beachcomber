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

# How often the watchdog checks the scheduler heartbeat.
# If the heartbeat hasn't advanced within the threshold, the daemon shuts down
# for the process supervisor (launchd, systemd) to restart.
# Default: disabled (no watchdog)
# watchdog_interval = "30s"

# How long the heartbeat can be stale before the watchdog triggers shutdown.
# Default: 3x watchdog_interval
# watchdog_threshold = "90s"


# ─── Lifecycle ─────────────────────────────────────────────────────────────────

[lifecycle]

# Base polling interval used while an entry is in Active state (P in the
# cache lifecycle model). See docs/cache-lifecycle.md.
# Default: "60s"
poll_interval = "60s"

# Keep-alive count in polls (K in the cache lifecycle model). An Active
# entry stays warm for K × poll_interval seconds after the last demand
# signal, then enters the exponential decay ladder (Decay1..4, each step
# doubling interval and duration) until eviction.
# Default: 12
poll_live_count = 12

# Whether an fsevent during decay reinstates the entry to Active. Default
# false drops filesystem watches on entry to Decay1; set true to keep
# watches live through every decay step. Useful for providers whose
# watched files change rarely but matter quickly.
# Default: false
fsevents_reinstate = false

# How many times to retry a provider after consecutive failures before backing off.
# Default: 3
failure_reattempts = 3

# How long to wait between failure retry attempts.
# Default: "1s"
failure_backoff_interval = "1s"

# How long (in seconds) the daemon waits with no active connections before
# shutting itself down. The next client connection will socket-activate a
# fresh instance.
# Set to null to disable idle shutdown (daemon stays resident permanently).
# Default: 300 (5 minutes)
idle_shutdown_secs = 300


# ─── Built-in Provider Overrides ───────────────────────────────────────────────
# Provider config uses per-source nesting: [providers.<name>.<source>]
# The source name matches the source field carried per row in `comb status`'s
# wire response. Use [providers.<name>] only for whole-provider switches (e.g. enabled).

# Key reference for per-source blocks:
#
# type              string    Strategy: "poll", "fsevent", "fsevent_poll"
# scope             string    "path" or "global" (informational for built-ins)
# poll_interval     duration  Poll interval (poll, fsevent_poll)
# poll_count        integer   Keep-alive polls before decay (poll, fsevent_poll)
# fsevent_patterns  [string]  Relative path components to watch (fsevent, fsevent_poll)
# fsevent_abs_paths [string]  Absolute roots to watch (fsevent, fsevent_poll)
# fsevent_lifespan  duration  Keep-alive duration before decay (fsevent, path-scoped)
# fsevent_reinstates bool     Whether watches survive decay. Default true.
# failback_count    integer   Consecutive failures before suppression
# failback_interval duration  Suppression duration after failback_count hit
# enabled           bool      When false, source never executes. Default true.

# Disable a provider entirely (it will never execute or appear in results)
[providers.conda]
enabled = false

# Override the git.diff source to poll more frequently
[providers.git.diff]
poll_interval = "15s"         # default: "30s"
poll_count = 24               # keep-alive = 24 × 15s = 6 min

# Make git.refs reinstate faster from decay
[providers.git.refs]
fsevent_reinstates = true     # .git fsevent during decay reinstates to Active
fsevent_lifespan = "600s"     # stay warm for 10 min after last query

# Slow down battery polling (less important on desktop)
[providers.battery.state]
poll_interval = "60s"         # default: "30s"
poll_count = 4

# Override network polling interval
[providers.network.interfaces]
poll_interval = "30s"         # default: "10s"


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

### Per-field scope

Script and library providers can declare scope per field. The provider-level
`scope` acts as the default; individual fields can override.

```toml
[providers.mixed]
type = "script"
command = "/usr/bin/myscript --json"
scope = "path"

[providers.mixed.fields.branch]
type = "string"
# inherits provider-level "path"

[providers.mixed.fields.status]
type = "string"
scope = "global"   # override — always cached globally
```

Back-compat: `fields = { branch = "string" }` continues to parse. All fields
inherit the provider-level `scope`.

Library providers (.so / .dylib) use the same per-field scope model in their
JSON metadata:

```json
{
  "fields": {
    "branch": {"type": "string", "scope": "path"},
    "system": {"type": "string", "scope": "global"}
  }
}
```

## Config field summary

**`[daemon]` section:**

| Field | Type | Default | Description |
|---|---|---|---|
| `socket_path` | string | `$XDG_RUNTIME_DIR/beachcomber/sock` | Unix socket path |
| `log_level` | string | `"info"` | Tracing log level |
| `provider_timeout_secs` | int | `10` | Max seconds for any provider to run |
| `env_file` | string | `~/.config/beachcomber/env` | Path to env file loaded at startup |
| `watchdog_interval` | duration or null | `null` (disabled) | How often the watchdog checks scheduler liveness |
| `watchdog_threshold` | duration or null | 3x `watchdog_interval` | Stale heartbeat duration before triggering shutdown |

**`[lifecycle]` section:**

| Field | Type | Default | Description |
|---|---|---|---|
| `poll_interval` | duration | `"60s"` | Base polling interval `P` used in Active state |
| `poll_live_count` | int | `12` | Keep-alive count `K` in polls (see `docs/cache-lifecycle.md`) |
| `fsevents_reinstate` | bool | `false` | Whether fsevents during decay reinstate the entry to Active |
| `idle_shutdown_secs` | int or null | `null` (disabled) | Seconds until idle daemon shuts down |
| `failure_reattempts` | int | `3` | Number of retries after consecutive provider failures before backing off |
| `failure_backoff_interval` | duration | `"1s"` | Wait time between failure retry attempts |

> `cache_lifespan`, `poll_idle_interval`, and `poll_live_interval` have been removed. Cache warmth is now `poll_interval × poll_live_count`; the decay ladder plus `fsevents_reinstate` replaces the single idle rate. Legacy configs parse cleanly — the daemon emits a `WARN` log at startup for each deprecated key.

> Duration strings use whole-second values: `"30s"` (seconds), `"2m"` (minutes), `"1h"` (hours), `"2h30m"` (compound). Sub-second values (e.g. `"500ms"`) are not accepted.

**`[providers.<name>]` section (built-in overrides):**

| Field | Type | Default | Description |
|---|---|---|---|
| `enabled` | bool | `true` | Set `false` to disable provider entirely |
| `poll_interval` | duration | inherited from `[lifecycle]` | Override base poll rate `P` |
| `poll_live_count` | int | inherited from `[lifecycle]` | Override keep-alive count `K` |
| `fsevents_reinstate` | bool | inherited from `[lifecycle]` | Override fsevent-during-decay policy |
| `poll_floor_secs` | int | provider-specific | Minimum poll interval consumers can request |
| `failure_reattempts` | int | inherited from `[lifecycle]` | Retries after consecutive failures before backing off |
| `failure_backoff_interval` | duration | inherited from `[lifecycle]` | Wait time between failure retry attempts |

**`[providers.<name>]` section (custom script providers):**

| Field | Type | Required | Description |
|---|---|---|---|
| `command` | string | yes | Shell command to execute |
| `output` | string | no | `"json"` (default), `"kv"`, or `"text"` |
| `scope` | string | no | `"global"` (default) or `"path"` |
| `enabled` | bool | no | `false` to disable |
| `poll_interval` | duration | no | Base poll rate `P` |
| `poll_live_count` | int | no | Keep-alive count `K` |
| `fsevents_reinstate` | bool | no | Keep watches live during decay |
| `poll_floor_secs` | int | no | Minimum poll interval |
| `failure_reattempts` | int | no | Retries after consecutive failures before backing off |
| `failure_backoff_interval` | duration | no | Wait time between failure retry attempts |
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

**`[providers.<name>]` section (shared library providers):**

| Field | Type | Required | Description |
|---|---|---|---|
| `type` | string | yes | Must be `"library"` |
| `library_path` | string | yes | Path to `.so`/`.dylib` file. Supports `~/` expansion. |
| `scope` | string | no | `"global"` (default) or `"path"` — overrides library metadata |
| `fields` | table | no | Field name to type mapping — overrides library metadata |
| `enabled` | bool | no | `false` to disable |
| `invalidation.poll` | string | no | Poll interval — overrides library metadata |
| `invalidation.watch` | array of strings | no | Watch patterns — overrides library metadata |
