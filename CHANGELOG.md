# Changelog

All notable changes to beachcomber will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- Cache decay now works as designed. Previously, `BackoffStage::SlowPoll`, `Frozen`, and `Evict` were defined but unreachable; cache entries never evicted and accumulated until daemon restart. Now: Active → Decay1 → Decay2 → Decay3 → Decay4 → Evicted runs end-to-end with exponential decay polling (poll interval and step duration both double per step). See `docs/cache-lifecycle.md` for the full behaviour spec.
- Global providers (hostname, user, battery, etc.) no longer create ghost cache entries when queried with an explicit path. Previously `comb get hostname.short /some/dir` cached hostname under `/some/dir` in addition to the real pathless entry.
- `mise.global` is no longer duplicated per project directory. The `mise` provider now emits its global tool state as a pathless cache entry and its project tool state as a path-scoped cache entry.

### Added

- Per-provider cache lifecycle tuning: `poll_interval` (base poll rate `P`) and `poll_live_count` (keep-alive count `K` in polls) in `[lifecycle]` and `[providers.<name>]`.
- `fsevents_reinstate` per-provider bool. When true, an fsevent on a watched path reinstates a decaying entry back to Active.
- `DECAY` column in `comb status` showing the lifecycle level 0-4 per entry (0 = Active).

### Changed

- **Protocol-breaking:** `introspect` subject renamed from `"backoff"` to `"lifecycle"`. Payload shape carries new state values (`"Active"`, `"Decay1"`–`"Decay4"`) in place of the old `"Grace"` / `"SlowPoll"` / `"Frozen"` / `"Evict"`. All SDK `IntrospectSubject` constants renamed accordingly (no legacy alias — pre-1.0).
- Provider scope is now declared per field via `FieldSchema::scope` (`FieldScope::Global` or `FieldScope::PathScoped`). The `ProviderMetadata.global: bool` field is removed. Custom TOML and library providers can declare per-field scope; the provider-level `scope` key continues to work as a default.
- `Provider::execute` signature widened to return `Vec<(Option<String>, ProviderResult)>`, enabling one provider to emit multiple scoped cache entries per execution. SDKs are unaffected — wire protocol is unchanged.

### Removed

- `[lifecycle] cache_lifespan` config key — now derived as `poll_interval × poll_live_count`.
- `[providers.<name>] poll_idle_interval` config key — subsumed by the decay ladder plus `fsevents_reinstate`.
- `[providers.<name>] poll_live_interval` config key — renamed to `poll_interval`.
- The unused `[lifecycle] eviction_timeout_secs` config field. Configs that still declare these legacy keys continue to parse cleanly (serde ignores unknown fields); the daemon emits a `WARN` log at startup for each deprecated key detected.

## [0.6.0] - 2026-04-22

### Breaking

- **Wire:** `{"op":"poke"}` renamed to `{"op":"refresh"}` — all clients must update
- **Wire:** `{"op":"store"}` renamed to `{"op":"put"}` — all clients must update
- **Wire:** `Request::List` removed — the `list` op is no longer accepted
- **Wire:** `{"op":"status"}` response reshaped from a daemon-health object to an array of cache-entry row objects (`[{provider, path, field, value, age_ms, stale}, ...]`). Use `{"op":"introspect","subject":"daemon"}` for the old health fields
- **CLI:** `comb refresh` / `comb r` removed — use `comb get --force` to trigger immediate recomputation
- **CLI:** `comb fetch` / `comb f` removed — `comb get` now accepts variadic keys
- **CLI:** `comb list` / `comb l` removed — no replacement (provider introspection via `comb check providers`)
- **Templates:** single-brace `{field}` placeholder syntax removed from `.f` format and `comb eval`. Use minijinja double-brace `{{ field }}` syntax everywhere
- **`:age`:** now returns a JSON number (integer milliseconds), not a quoted string

### Added

- `Request::Introspect` wire op with 9 subjects: `daemon`, `providers`, `config`, `cache`, `backoff`, `watches`, `timers`, `demand`, `procs`
- `comb check` rewired onto `Introspect` — top-level aggregation when no subcommand given; each subject is a standalone subcommand (`backoff`, `watches`, `timers`, `demand` are new)
- `comb get --force` — trigger immediate provider recomputation before returning the result
- `comb get --wait` — block until a fresh value arrives (useful after an external trigger)
- `comb get` variadic keys — query multiple keys in a single connection: `comb g git.branch git.dirty battery.percent .`
- `comb put --null` — clear a previously written virtual provider entry
- `comb status` tabular output — one row per warm cache entry with provider/field/value/age_ms/stale columns
- `comb status` flags: `--format <template>`, `--filter <provider>`, `--sort <field|age|stale>`, `--no-trunc`, `--max-width <n>`, `--no-color` / `--no-colour`
- `comb status` custom minijinja templates via `--format "{{ provider }}.{{ field }}={{ value }}"`
- minijinja templating across `comb eval`, `.f` format suffix, and `comb status --format`; available filters: `truncate`, `default`, `upper`, `lower`, `length`
- Script provider `output = "text"` produces `{value: <stdout>}` in cache — field is `value`
- Watcher GC subscription lifecycle hook — watcher registrations are cleaned up when cache entries are evicted
- Watcher GC periodic tick — background task evicts stale watcher registrations on a fixed schedule

### Fixed

- `:age` metadata suffix now returns a JSON number, not a string
- Watcher registry could leak registrations when cache entries were evicted without triggering the GC hook
- `uptime_provider_executes` test marked as sandbox-unsafe to avoid flakiness in CI environments
- Various doc drift between shipped behaviour and reference pages (this release)

## [0.5.1] - 2026-04-21

### Fixed
- Git provider: set defensive environment variables (`GIT_OPTIONAL_LOCKS=0`, `GIT_TERMINAL_PROMPT=0`, `LC_ALL=C`) on all git subprocesses to prevent lock contention, interactive credential prompts, and locale-dependent output parsing
- Sudo provider: gate `check_timestamp_dir` on macOS only — the Linux path does not use `/var/db/sudo`
- CLI: collapse nested `if let` chains in the Linux `/proc` scanner (clippy `collapsible_if` under Rust 1.88+)
- Release workflow: attach daemon `.deb` / `.rpm` packages to the GitHub Release (previously only the C SDK packages were attached)

## [0.5.0] - 2026-04-14

### Added
- Git provider: `commit_summary` field — first line of HEAD commit message, extracted from the existing `git log` call (no additional subprocess)
- Git provider: `push_ahead` and `push_behind` fields — commits ahead/behind the push remote (distinct from tracking remote `ahead`/`behind`)
- Synchronous cache miss — `comb get` on a cold cache executes the provider inline via `spawn_blocking` and returns data immediately instead of returning empty
- Single-letter command aliases: `d`aemon, `g`et, `p`ut, `r`efresh, `w`atch, `s`tatus, `l`ist, `k`ill
- Format suffix syntax — append `.p` (plain text), `.j` (json), `.s` (sh), `.c`/`.C` (csv), `.t`/`.T` (tsv), `.f` (template) to a command for quick output format selection without `-f` flag
- New output format: `sh` — `key=value` pairs, sourceable in shell scripts (replaces old `text` behavior for objects)
- New output formats: `csv`/`tsv` (values only), `CSV`/`TSV` (with header row) for structured data export
- New output format: `fmt` — `{field_name}` template interpolation per field, e.g. `comb g.f '{branch} ({dirty})' git .`
- `comb kill` (alias `k`) — stop the running daemon via SIGTERM; socket-activates fresh on the next query. Queries the daemon's pid via the status socket so it works even when the pid file is stale
- `status` response now includes `pid` and `version` fields
- `comb eval` (alias `e`) — template interpolation across providers: `comb eval "branch: {git.branch} load: {load.one}" .`
- `comb fetch` (alias `f`) — batch get, query multiple keys in a single connection with format-aware output
- Field metadata access — colon delimiter on key (`git.branch:age`, `git.branch:stale`, `git.branch:source`) returns cache metadata instead of data
- `sudo` provider — detect active sudo timestamp. Global, poll 30s. Fields: `active` (bool)
- `op` provider — detect active 1Password CLI session. Global, poll 60s. Fields: `signed_in` (bool), `account` (string)
- `comb init` (alias `i`) — auto-detect installed tools (p10k, starship, tmux, neovim, polybar, waybar, sketchybar, oh-my-zsh) and print integration snippets
- `comb check` (alias `c`) — health check with subcommands: `all`, `daemon`, `config`, `providers`, `cache`, `procs`
- `comb check procs` — process exec tracing via eslogger (macOS) or /proc scanning (Linux) to measure beachcomber's potential impact
- Shell integration scripts: `scripts/chpwd.sh` (directory change hook for zsh/bash/fish), `scripts/polyfill.sh` (POSIX fallback function)
- Help screen branding with NavistAu authorship, beachcomber.sh URL, MIT license, format suffix usage hint

### Changed
- CLI: `comb poke` renamed to `comb refresh`
- CLI: `comb store` renamed to `comb put`
- **Breaking:** `text` output format for objects now returns raw values only (no key= prefix). Use `sh` format for the old `key=value` behavior
- **Breaking:** Default output format is now `text` (was `json`). `comb g git.branch .` prints just the branch name. Pass `-f json` or use the `.j` suffix for the old JSON envelope
- **Breaking:** Format suffixes remapped for better ergonomics: `.t` → `.p` (plain text, now the default — `.p` is rarely needed), `.sh` → `.s` (shell), `.s` → `.t` (tsv), `.S` → `.T` (tsv+header), `.fmt` → `.f` (template). `.j`, `.c`, and `.C` are unchanged
- The daemon now exits cleanly on SIGTERM as well as SIGINT (SIGTERM is what `comb kill` sends)

## [0.4.0] - 2026-04-10

### Added
- Shared library provider backend via `libloading` — load `.so`/`.dylib` plugins as providers with a C ABI contract (`beachcomber_provider_metadata`, `beachcomber_provider_execute`, `beachcomber_provider_free`). Configure with `type = "library"` and `library_path` in `[providers.<name>]`.
- Scheduler watchdog — monitors the scheduler heartbeat and triggers a clean daemon shutdown on stall detection. Configure with `watchdog_interval` and `watchdog_threshold` in `[daemon]`. Disabled by default.
- `aarch64-unknown-linux-gnu` pre-built binary, `.deb`, and `.rpm` packages in release workflow via cross-rs (pinned to 0.2.5)

## [0.3.1] - 2026-04-10

### Added
- Linux C SDK packages: `libbeachcomber-dev` (deb), `libbeachcomber-devel` (rpm), `libbeachcomber` (AUR)
- pkg-config support for the C SDK (`libbeachcomber.pc`)
- C SDK release workflow: builds deb/rpm, smoke-tests in containers, attaches to GitHub Release

## [0.3.0] - 2026-04-09

### Added
- Virtual providers via `comb store` — external processes can write data into the cache, creating data-only providers with no execute function
- Namespace hierarchy for providers: builtin > script > virtual (higher priority providers cannot be shadowed)
- `comb watch <key> [path]` — server-push streaming over long-lived connections, NDJSON line emitted on each cache update
- WatcherRegistry with broadcast channels for field-level change notification
- `store` and `watch` protocol operations
- `store` and `read_watch_line` methods on ClientSession
- Store and watch integration tests

## [0.2.0] - 2026-04-05

### Added
- Core daemon with Unix socket server and socket activation
- Concurrent cache with 157ns read latency
- Scheduler with filesystem watching, poll timers, and poke triggers
- Provider execution timeouts (configurable, default 10s)
- Execution deduplication (prevents thundering herd on filesystem bursts)
- Provider failure backoff (exponential delay after 3 consecutive failures)
- Subscription manager with multi-tenant cadence resolution
- Backoff/drain lifecycle (Grace -> SlowPoll -> Frozen -> Evict)
- Graceful shutdown via CancellationToken + SIGINT handling
- Daemon auto-shutdown after configurable idle timeout
- Connection context for implicit path resolution
- Staleness computation in cache responses
- CLI: `comb daemon | get | poke | subscribe | list | status`
- 16 built-in providers: hostname, user, git, battery, load, uptime, network, kubecontext, aws, gcloud, terraform, direnv, python, conda, mise, asdf
- Script provider backend for custom providers via config.toml
- Provider enabled/disabled flag in config
- JSON and text output formats
- ClientSession for persistent connections (15µs/query)
- Comprehensive benchmark suite (cache, protocol, providers, socket, throughput)
- Linux support for battery provider (sysfs + UPower), network provider (nmcli/iw SSID, tun/wg VPN detection), and uptime provider (/proc/uptime)
- Pre-built binaries for aarch64-unknown-linux-gnu and aarch64-unknown-linux-musl
- Debian/Ubuntu (.deb) and Fedora/RHEL (.rpm) packages published as GitHub Release assets
- AUR packages: `beachcomber` (source) and `beachcomber-bin` (prebuilt)
- Nix flake for building from source
- Linux CI job (cargo check, test, clippy, fmt on ubuntu-latest)

### Changed

- Network provider refactored into platform submodules (network/mod.rs, network/macos.rs, network/linux.rs)
- npm and PyPI binary installers now support Linux arm64
