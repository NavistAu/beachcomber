# Changelog

All notable changes to beachcomber will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- Git provider: `commit_summary` field — first line of HEAD commit message, extracted from the existing `git log` call (no additional subprocess)
- Git provider: `push_ahead` and `push_behind` fields — commits ahead/behind the push remote (distinct from tracking remote `ahead`/`behind`)
- Synchronous cache miss — `comb get` on a cold cache executes the provider inline via `spawn_blocking` and returns data immediately instead of returning empty
- Single-letter command aliases: `d`aemon, `g`et, `p`ut, `r`efresh, `w`atch, `s`tatus, `l`ist

### Changed
- CLI: `comb poke` renamed to `comb refresh`
- CLI: `comb store` renamed to `comb put`

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
