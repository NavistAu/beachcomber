# Changelog

All notable changes to beachcomber will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.6.0] - 2026-05-29

A large release. The daemon's internal model was rebuilt around a
Provider→Source→Field architecture, the wire protocol and CLI were overhauled,
and typed client SDKs reached parity across all six languages. Many changes are
breaking; pre-1.0, these ship under a minor bump.

### Added

#### Provider / Source / Field architecture

- **Provider→Source→Field model.** A `Provider` is now a namespace declaring 1+ `Source` objects; each `Source` has its own `InvalidationStrategy`, `KeepAlive`, `FailbackConfig`, `SourceScope`, and field set.
- Lifecycle keying moved from `(provider, path)` to `(provider, path, source)` — each source instance has an independent Active/Decay/Evicted lifecycle.
- Cache entries at `(provider, path)` hold per-source sub-entries; field ownership is disjoint across sources, so flatten reads are unambiguous.
- **Per-source eviction:** an evicting source removes only its own contribution; the `(provider, path)` entry is dropped only when its last source evicts.
- `InvalidationStrategy::Watch` gains `abs_paths` for absolute-path filesystem watches; global sources can watch `$XDG_CONFIG_HOME` and other absolute roots directly. `expand_abs_path()` expands `~`, `$HOME`, and the XDG vars in `Source::metadata()`.
- Pure-watch global sources (`Watch + Global + KeepAlive::Never`) execute once on first demand, re-execute only on fs events, and never decay.
- `ProviderRegistry` builds a `field → source` reverse map at registration; `comb get git.branch` routes to the owning `git.refs` source without a linear scan.

#### Source-aware query planning

- New `src/query.rs` request planner (`QueryPlan` / `SourceDemand`); `get` and `watch` build one plan so they share identical key semantics.
- A field query warms **only its owning source** — `comb get git.branch` no longer warms sibling `git` sources (`diff`, `status`). Whole-provider queries still warm all applicable sources.
- New addressing forms `provider.source` and `provider.source.field` accepted by `get`, `refresh`, and `watch`; `watch` now resolves source-qualified keys identically to `get`.

#### Protocol, clients, and SDKs

- `Request::Introspect` wire op with subjects `daemon`, `providers`, `config`, `cache`, `lifecycle`, `watches`, `timers`, `demand`, `procs`. `comb check` rewired onto it (top-level aggregation; each subject a subcommand).
- Typed client surface across `beachcomber-client` and all six SDKs (Python, Go, Node, Ruby, C, Lua): `status()` returns typed cache rows directly; `RowKind` discriminator and `FailureSnapshot` exposed on `CacheRow`.
- Provider conformance harness wired into the test suite; protocol spec / hello version negotiation documented.
- **Client connect retry** in the CLI and all six SDKs + `beachcomber-client`: transient `ECONNREFUSED`/`ENOENT` retried 3× with exponential backoff (250ms / 500ms / 1s), covering the daemon-restart window.

#### Daemon lifecycle

- **Singleton enforcement** via an exclusive `flock` on a PID file: a same-version second daemon exits; a different-version one takes over the old (SIGTERM → SIGKILL).
- **Automatic restart on binary change** (the daemon fs-watches its own executable) and **orphan reaping** of stale `comb daemon` processes sharing its binary path.
- `comb --version` reports `BEACHCOMBER_VERSION`, including git sha for dev/dirty builds.
- `BEACHCOMBER_SOCKET` env var overrides the socket path.

#### CLI: get / put / status

- `comb get` variadic keys, `--force` (immediate recompute) and `--wait` (block for a fresh value); `comb put --null` clears a virtual entry.
- `comb status` tabular output (one row per warm entry) with a `TTL`/lifecycle column (`★`/`3`–`0` countdown, poll interval, keep-alive, fsevents-reinstate), a failure `⚠` indicator, and flags `--filter`, `--sort` (incl. `lifecycle`), `--no-trunc`, `--max-width` (int or `auto`), `--color` (`auto`/`always`/`never`), `--ascii`, and `-f/--format` presets (`human`/`tsv`/`json`/`csv`/`table`/`sh`).
- minijinja templating in `comb eval` and the `.f` format suffix (filters: `truncate`, `default`, `upper`, `lower`, `length`); script provider `output = "text"`.

### Changed

- **BREAKING (pre-1.0):** daemon socket path no longer depends on `$TMPDIR` — resolution is config override → `$XDG_RUNTIME_DIR/beachcomber/sock` → `/tmp/beachcomber-<uid>/sock`. On macOS all shells now share one daemon.
- **BREAKING (pre-1.0):** `introspect` subject `backoff` → `lifecycle`, with state values `Active` / `Decay1`–`Decay4` replacing `Grace` / `SlowPoll` / `Frozen` / `Evict`; all SDK constants renamed (no legacy alias).
- **BREAKING (pre-1.0, all SDKs):** `status_rows()` removed; `status()` returns typed rows. The C SDK `comb_status_rows` is redesigned to heap-allocating (pair with `comb_free_cache_rows()`); `comb_cache_row_t` fields are now owned `char*`.
- **BREAKING (pre-1.0):** `comb status` defaults to the `human` preset regardless of TTY (use `-f tsv` / `-f json` in scripts); `--no-color` removed in favour of `--color=never|auto|always`.
- `comb status` default sort is `(provider, path, field)`; `--max-width` default raised 40 → 120; TSV/CSV gain one column per `CacheRow` field.
- Provider scope declared per field via `FieldSchema::scope`; `Provider::execute` returns `Vec<(Option<String>, ProviderResult)>` (wire protocol unchanged).

### Fixed

- Cache decay works end-to-end (Active → Decay1–4 → Evicted with exponential backoff); previously the decay stages were unreachable and entries never evicted.
- Global providers no longer create ghost cache entries when queried with an explicit path; `mise.global` is no longer duplicated per project directory.
- Library (FFI) provider dispatch is now strict UTF-8 (was silently lossy).
- **Lua SDK:** `Client:get_with_flags` returns `nil, error` on server-side failures, matching `Client:get`.
- `:age` metadata suffix returns a JSON number, not a string. Watcher registrations are GC'd on cache eviction and on a periodic tick.

### Removed

- **BREAKING (pre-1.0):** wire ops `poke` → `refresh`, `store` → `put`; `Request::List` removed; `status` reshaped to a cache-row array (old health fields via `introspect daemon`).
- **BREAKING (pre-1.0):** CLI `comb refresh`/`r`, `comb fetch`/`f`, `comb list`/`l` removed; single-brace `{field}` template syntax removed (use `{{ field }}`).
- **BREAKING (pre-1.0):** `InvalidationStrategy::Once`, `Watch::fallback_poll_secs`, and `Poll::floor_secs` removed; TOML moved to per-source `[providers.<name>.<source>]` blocks with `poll_*` / `fsevent_*` / `failback_*` prefixes (old flat source-knob keys are rejected with a clear error).
- `[lifecycle] cache_lifespan` (now derived as `poll_interval × poll_live_count`), `poll_idle_interval`, `poll_live_interval` (→ `poll_interval`), and `eviction_timeout_secs` config keys removed.

### Internal

- Test-suite-health initiative: dependency-injection seams for process / git / HTTP / library boundaries, golden CLI tests, mock-clock TTL tests, a shared git fixture, the provider conformance harness, and a coverage gate raised to 70%.

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
