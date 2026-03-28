# shellstate Roadmap

This document tracks what has been built, what remains from the original design spec, and what was identified as future work during development. It is the single source of truth for project status.

Last updated: 2026-03-28

---

## What's Built (v0.1)

### Core Infrastructure
- Single async Rust daemon (tokio) with Unix socket server
- Concurrent cache (DashMap) with single-string key optimization
- Provider trait with 4 execution backends designed (2 implemented: built-in, script)
- Scheduler with poll timers, filesystem watching (notify/FSEvents), poke triggers
- Subscription manager with multi-tenant cadence resolution and provider floor enforcement
- Backoff state machine (Grace → SlowPoll → Frozen → Evict)
- Graceful shutdown via CancellationToken + SIGINT handling
- Socket activation (auto-start daemon on first client query)
- CLI: `shellstate daemon | get | poke | list | status`
- TOML config with XDG paths, zero-config defaults

### Providers (16 built-in)
| Provider | Scope | Method | Execution time |
|---|---|---|---|
| hostname | global/once | libc gethostname | 654ns |
| user | global/once | libc getuid/getpwuid | 395ns |
| load | global/poll | libc getloadavg | 548ns |
| uptime | global/poll | sysctl KERN_BOOTTIME | 658ns |
| kubecontext | global/poll | file read ~/.kube/config | 749ns |
| gcloud | global/poll | file read ~/.config/gcloud/properties | 1.08µs |
| aws | global/poll | env vars AWS_PROFILE/AWS_REGION | <1µs |
| conda | path/poll | env var CONDA_DEFAULT_ENV | <1µs |
| terraform | path/watch | file read .terraform/environment | <1µs |
| python | path/watch | file read .venv/pyvenv.cfg | <1µs |
| asdf | path/watch | file read .tool-versions | <1µs |
| direnv | path/watch | .envrc detection + direnv status | varies |
| mise | path/watch | mise ls --current | ~10ms |
| network | global/poll | getifaddrs + airport for SSID | 2ms |
| git | path/watch+poll | git status --porcelain=v2 | 5.6ms |
| battery | global/poll | pmset -g batt | 6ms |

### External Backends
- **Script provider**: custom providers via config.toml, JSON or kv output parsing

### Performance
- Cache read: 157ns, socket round-trip: 34µs (cold), 15µs (warm via ClientSession)
- 42k requests/sec sustained under 100 concurrent clients
- Provider execution on spawn_blocking (non-blocking scheduler)
- Comprehensive benchmark suite (cache, protocol, providers, socket, throughput)

---

## Remaining from Design Spec

These items were specified in the original design but not yet implemented. Grouped by priority for a production-ready release.

### Priority 1: Scheduler Robustness

These prevent resource leaks and hung states in production.

#### 1.1 Provider Timeouts

**Spec says:** "Timeouts: configurable per provider type (default 5s for scripts, 10s for built-ins). On timeout, task is cancelled, cache retains last good value."

**Current state:** Providers run without any timeout. A script provider that hangs will hold a `spawn_blocking` thread forever.

**What to build:** Wrap the `spawn_blocking` call in `tokio::time::timeout`. On timeout, log a warning and drop the task. The cache retains whatever value it had before. Add `timeout_secs` to `ScriptProviderConfig` and a default timeout per provider type.

#### 1.2 Execution Deduplication

**Spec says:** "If a provider is already computing for a given path, a second trigger queues rather than double-runs."

**Current state:** A burst of filesystem events (e.g., `git checkout` touching 50 files) triggers 50 separate `execute_provider` calls for the same `(git, /path)`. Each spawns a `git status` process.

**What to build:** Track in-flight executions in the scheduler with a `HashSet<(String, Option<String>)>`. Before spawning, check if a computation is already running. If so, skip (or set a "rerun after completion" flag if the trigger arrived after computation started). Clear the in-flight entry when spawn_blocking completes (use a channel or callback).

#### 1.3 Provider Failure Backoff

**Spec says:** "Repeated failures: exponential backoff on the provider itself. If a provider fails 3 times in a row, delay before retrying. Resets on success."

**Current state:** A provider that fails (e.g., git in a corrupted repo) is retried on every poll tick and every filesystem event with no delay.

**What to build:** Track consecutive failure count per `(provider, path)`. After N failures, suppress execution for an exponentially increasing duration (e.g., 1s, 2s, 4s, 8s, max 60s). Reset to 0 on success.

### Priority 2: Protocol Completeness

These make the protocol match the spec and improve consumer ergonomics.

#### 2.1 Connection Context

**Spec says:** "Consumers can set a working directory context on connect. Directory-scoped providers resolve relative to it, so `git.branch` without an explicit path uses the connection context."

**Current state:** Every query for a path-scoped provider requires an explicit `path` parameter. A prompt rendering in `/home/user/project` must send `{"op": "get", "key": "git.branch", "path": "/home/user/project"}` on every query.

**What to build:** Add a `context` operation to the protocol: `{"op": "context", "path": "/home/user/project"}`. The server stores the context per-connection. Subsequent `get`/`subscribe` requests without a `path` use the connection's context for path-scoped providers. Global providers ignore it.

#### 2.2 Staleness Flag

**Spec says:** "Response includes staleness metadata (age_ms, stale flag). The daemon never blocks a read waiting for fresh data. Consumers decide how to handle staleness."

**Current state:** `age_ms` is returned correctly. `stale` is always `false` — never computed.

**What to build:** Determine staleness by comparing `age_ms` against the provider's expected refresh interval. For a provider with `poll: 30s`, if `age_ms > 30000` the value is stale. For filesystem-watched providers, staleness depends on whether the watcher is active. Store the expected interval alongside the cache entry or look it up from the subscription manager's effective triggers.

#### 2.3 Subscribe CLI Command

**Spec says:** `shellstate subscribe <key> [path] [--watch] [--poll 10s]` — long-lived subscription.

**Current state:** The subscribe protocol works but there's no CLI subcommand for it. Users can only subscribe via raw socket messages.

**What to build:** Add a `subscribe` subcommand to clap that connects, sends a subscribe message, then holds the connection open (keeping the subscription alive). On Ctrl+C, disconnects cleanly. Useful for testing and for shell scripts that want to warm the cache.

### Priority 3: Config Application

These make the config file actually affect runtime behavior.

#### 3.1 Provider Override Application

**Spec says:** Config can override built-in provider defaults: `[providers.battery] poll = "10s"` changes battery's poll interval.

**Current state:** `ProviderOverride` was replaced by `ScriptProviderConfig` during Plan 3. The config is parsed but built-in provider overrides (poll interval, floor, enabled flag) are not applied. A user writing `[providers.battery]\npoll = "10s"` in config.toml gets no effect.

**What to build:** At daemon startup, after constructing the registry, iterate config overrides and apply them. This requires either: (a) a method on Provider to accept overrides, (b) wrapping providers in a decorator that overrides metadata, or (c) the scheduler consulting config when setting up poll timers (simplest — the scheduler already reads effective triggers from subscriptions, it can also factor in config overrides).

#### 3.2 Configurable Backoff Steps

**Spec says:** `backoff_steps = ["2x@30s", "4x@2m", "stop@5m"]` — configurable backoff progression.

**Current state:** Backoff stages are hardcoded in the `BackoffStage` enum (Grace → SlowPoll → Frozen → Evict). The config fields `grace_period_secs` and `eviction_timeout_secs` are parsed but `eviction_timeout_secs` is not used. The `backoff_steps` config format is not parsed at all.

**What to build:** Either implement the `backoff_steps` config format (parse "2x@30s" into multiplier + duration), or simplify to just `grace_period_secs` + `eviction_timeout_secs` with fixed intermediate stages. The latter is simpler and probably sufficient.

#### 3.3 Provider Enabled Flag

**Spec says:** Providers can be disabled: `[providers.battery] enabled = false`.

**Current state:** The `enabled` field existed on the old `ProviderOverride` type. The new `ScriptProviderConfig` doesn't have it. No provider can be disabled via config.

**What to build:** Add `enabled: Option<bool>` back to the config type. At registry construction, skip providers where `enabled == Some(false)`.

### Priority 4: Daemon Lifecycle

#### 4.1 Auto-Shutdown on Drain

**Spec says:** "When all cache entries are evicted and no connections remain, the daemon exits and removes the socket file."

**Current state:** The daemon runs forever. Even with zero subscriptions and an empty cache, it stays alive.

**What to build:** After the backoff check in the scheduler tick, if `cache.is_empty()` and no active subscriptions and no active connections, send a shutdown signal. The daemon cleans up the socket file on exit. Add a configurable idle timeout (e.g., 5 minutes of complete inactivity).

#### 4.2 Watchdog

**Spec says:** "If the event loop stalls (detected via periodic self-check timer), the daemon logs and exits."

**Current state:** No watchdog. If the tokio runtime hangs (e.g., a blocking call slips in), the daemon becomes unresponsive silently.

**What to build:** A separate thread (not a tokio task) that periodically checks if the scheduler's poll tick is advancing. If the scheduler hasn't ticked for N seconds (e.g., 30s), the watchdog logs an error and calls `std::process::exit(1)`. Socket activation will restart the daemon on next client connection.

### Priority 5: External Provider Backends

#### 5.1 Shared Library Backend

**Spec says:** "Shared library: `dlopen` + C ABI function call. Performance-sensitive third-party providers."

**Current state:** Not implemented. Config format is designed (`type = "library"`, `path = "~/.local/lib/shellstate/libfast_thing.so"`) but the backend doesn't exist.

**What to build:** A `LibraryProvider` that `dlopen`s a shared library and calls a C ABI function: `int shellstate_execute(const char* path, char* out_buf, size_t out_len)`. The function writes JSON to `out_buf`, the daemon parses it. Use the `libloading` crate. Add timeout wrapping since a buggy library could hang.

**Considerations:** ABI stability, versioning, error handling for segfaults (can't catch with Rust panic handler — need signal handling or fork+exec isolation).

#### 5.2 Lua Backend

**Spec says:** "Lua: embedded `mlua` interpreter. Neovim ecosystem, scriptable providers without fork overhead."

**Current state:** Not implemented. Config format is designed (`type = "lua"`, `script = "~/.config/shellstate/providers/git_extras.lua"`).

**What to build:** Embed the `mlua` crate. A `LuaProvider` loads and executes a Lua script that returns a table of key-value pairs. The Lua environment exposes a minimal API: `shellstate.read_file()`, `shellstate.exec()` (run a command), `shellstate.env()` (read env var). Scripts are loaded once and cached; execution calls a `run()` function.

**Considerations:** Lua state isolation (each provider gets its own Lua VM or shared?), memory limits, execution timeout.

### Priority 6: Cross-Platform

#### 6.1 Linux Support

**Spec says:** "Linux: inotify, `/sys/class/power_supply/`, `/sys/class/net/`."

**Current state:** The `notify` crate handles filesystem watching cross-platform (inotify on Linux). But battery, network, and uptime providers use macOS-specific APIs (pmset, getifaddrs with macOS constants, sysctl KERN_BOOTTIME).

**What to build:**
- Battery: read `/sys/class/power_supply/BAT0/capacity` and `/sys/class/power_supply/BAT0/status`
- Network: `getifaddrs` works on Linux too (different struct layout). SSID via `iwgetid` or `/proc/net/wireless`.
- Uptime: read `/proc/uptime`
- Conditional compilation: `#[cfg(target_os = "macos")]` / `#[cfg(target_os = "linux")]` in each provider, or platform trait abstraction.

**Spec also says:** "Platform-specific code lives behind traits: `FileWatcher`, `BatteryReader`, `NetworkReader`, etc."

This abstraction layer was designed-for but not implemented. Providers currently call platform APIs directly. Refactoring to traits would make adding Linux clean but adds indirection. Pragmatic approach: `#[cfg]` blocks in each provider file (simpler, less over-engineering for 2 platforms).

---

## Future Considerations

These were explicitly marked as out-of-scope in the design spec. They represent potential directions, not commitments.

### mmap/Shared Memory

Zero-latency cache reads by exposing cache state via a memory-mapped file. Consumers read directly from shared memory without a socket round-trip. Current socket latency is 15µs (warm) — mmap would bring this to ~100ns (memory read). Trade-off: significant complexity for lifecycle management, cache coherence, and cross-process locking. Most valuable for extremely high-frequency consumers (e.g., a prompt that re-renders on every keystroke).

### Push/Streaming Mode

Hold a connection open and stream cache updates as they happen. Instead of polling with `get`, consumers receive push notifications when subscribed values change. The server would send unsolicited messages on the connection when a cache entry is updated. Useful for editors (update gutter signs immediately on file save) and long-lived UI consumers. Requires bidirectional protocol and connection lifecycle management.

### Rust Client Crate

Publish a `shellstate-client` crate on crates.io. Rust tools (starship, other prompts) could depend on it directly instead of shelling out to the CLI. The crate would wrap the socket protocol with a typed API. Essentially the current `client.rs` + `protocol.rs` extracted into a separate crate.

### C Shared Library

Build a `libshellstate.so` / `libshellstate.dylib` with a C ABI. Functions like `shellstate_connect()`, `shellstate_get()`, `shellstate_disconnect()`. Enables FFI from Python, Ruby, Lua, Node.js, Go, etc. Consumers in any language get typed access without parsing CLI output.

### Consumer Integration Packages

Pre-built integrations for common consumers:
- **zsh plugin**: `precmd` hook that subscribes and populates prompt variables
- **tmux plugin**: status bar format strings that read from shellstate
- **neovim plugin**: Lua plugin using `vim.loop` to connect and expose state to statusline
- **starship integration**: custom module that queries shellstate instead of running its own git/battery/etc.

---

## Implementation Priority Summary

For a production-ready v0.1 release, focus on Priority 1 (scheduler robustness) and Priority 2 (protocol completeness). These prevent resource leaks, hung states, and make the protocol actually usable by consumers.

| Priority | Items | Effort | Risk if skipped |
|---|---|---|---|
| **P1: Scheduler Robustness** | Timeouts, deduplication, failure backoff | Medium | Hung providers, thundering herd on fs events, retry storms |
| **P2: Protocol Completeness** | Connection context, staleness, subscribe CLI | Medium | Awkward consumer integration, misleading staleness data |
| **P3: Config Application** | Provider overrides, backoff config, enabled flag | Small | Config file is partially decorative |
| **P4: Daemon Lifecycle** | Auto-shutdown, watchdog | Small | Orphaned daemons, silent hangs |
| **P5: External Backends** | Shared library, Lua | Large | Limits extensibility to scripts only |
| **P6: Cross-Platform** | Linux support | Medium | macOS-only |
