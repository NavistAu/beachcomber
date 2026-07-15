# Changelog

All notable changes to beachcomber will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Orphan reaping.** The canonical daemon (bound socket equals its own env-free resolution) reaps orphaned `comb daemon` processes at startup and hourly: uid-owned daemons reparented to PID 1, on sockets nothing resolves, carrying neither `--exit-with-parent` nor the new `--no-reap` flag, and older than 60s. Live test daemons, attended foreground runs, and flagged supervised daemons are exempt. Closes the leak class where daemons on session-scoped or deleted-worktree sockets accumulated for weeks (`docs/canon/singleton.md` §"Orphan reaping").
- **`comb daemon --no-reap`** — marks a deliberate, supervised, non-canonical daemon exempt from reaping.
- **Poll-guaranteed self-supervision.** The daemon's binary self-watch gains a 5s mtime poll alongside the fs-event watch. An fs-event stream can be created without error and then deliver nothing (sandboxed CI hosts, a degraded `fseventsd`); with an event-only watch such daemons outlived every rebuild indefinitely. The poll bounds staleness at one interval regardless of backend health.
- **Watch self-test.** At startup the daemon probes whether kernel fs events actually deliver (2s timeout, concurrent with the scheduler loop — no startup latency). If not, provider file-watching falls back to a polling backend (1s scan), and the degradation is surfaced via `comb check daemon` (WARN verdict), `comb status` (stderr warning), and a new `watch_backend` field in the daemon introspect payload.

### Changed

- **BREAKING: `$XDG_RUNTIME_DIR` no longer participates in socket path resolution.** Canonical resolution is now config override → `$BEACHCOMBER_SOCKET` → `/tmp/beachcomber-<uid>/sock`, in the daemon and all clients (CLI, `libbeachcomber`, and the C/Go/Lua/Node/Python/Ruby SDKs). Session-scoped environments (sandboxes, containers, per-session `XDG_RUNTIME_DIR` shims) previously resolved distinct socket paths and auto-spawned one daemon per session; singleton enforcement is per-socket-path, so the default must be a stable per-user path (see `docs/canon/singleton.md`). Environments that want a different placement (e.g. `/run/user/<uid>` on systemd) set `$BEACHCOMBER_SOCKET` or the config override. **Migration:** a daemon running on an old XDG-derived socket is unreachable by upgraded clients; the first client invocation spawns a daemon at the stable path, and the old daemon exits on binary replacement (self-supervision) or can be killed manually.

## [0.7.0] - 2026-06-23

The post-0.6.1 cycle: an **env-cascade** overhaul plus a broad provider-correctness
sweep. Each environment-aware provider is split into a daemon-side **data provider**
(pure on-disk/state enumeration, no environment reads) and a client-side **consumer
namespace** (virtual fields + path/value expressions that fold in the *querying
shell's* environment) — fixing the long-standing "frozen daemon environment" class
of bugs where the daemon reported its own launch-time env instead of the caller's.
Many changes are breaking; pre-1.0, they ship under a minor bump.

### Added

#### Field resolution model + `cache.*` namespace (env cascade)

- **New canon doc `docs/canon/field_resolution.md`.** Defines the client-side field-resolution model: field-type taxonomy (`native` / `external` / `literal` / `virtual` / `env`), the `cache.*` namespace, path expressions, and value expressions. This is now the authoritative spec for how consumers address and resolve provider fields.
- **`cache.*` value-expression model.** Expressions reference raw cached values as `cache.<provider>.<field>` (the stored value, bypassing any field expression) vs bare `<provider>.<field>` (the resolved field, with expression applied). A cached field's default expression is the identity; no rename is needed when a virtual field overrides a same-named cached value.
- **Path expressions.** A provider's cache-key path is computed client-side by an expression over `cwd` and `env.*` (`[providers.<name>] path = "<expr>"`). Empty/falsy ⇒ global slot. Built-in defaults compiled into the CLI; user config overrides per provider.

#### Data providers and consumer namespaces

- **`aws` → `aws_profiles` data provider.** The `aws` daemon provider is now named `aws_profiles`. It returns one field per profile (each an `Object{region}`) parsed from `~/.aws/config`. The `aws` consumer namespace is now composed of **virtual fields** that index the data provider by the active profile selector: `aws.region = env.AWS_REGION or env.AWS_DEFAULT_REGION or cache.aws_profiles[env.AWS_PROFILE or env.AWS_VAULT or env.AWS_DEFAULT_PROFILE or "default"].region`. `comb get aws` returns computed fields; `comb get aws_profiles` returns the raw profile enumeration.
- **`gcloud` → `gcloud_configs` data provider.** The `gcloud` daemon provider is now named `gcloud_configs`. It returns one field per gcloud configuration (each an `Object{project, account}`) plus an `active_config` field, parsed from `~/.config/gcloud/`. The `gcloud` consumer namespace is virtual fields indexing the data provider by active config. `comb get gcloud` returns computed fields; `comb get gcloud_configs` returns the raw config enumeration.

#### Env-selected file providers (Tier B)

- **`kubecontext` is now PathScoped.** The provider reads the kubeconfig file named by the path expression `env.KUBECONFIG or '~/.kube/config'`. A `:`-joined list is merged (later file wins). The daemon watches each resolved file via `Source::watched_files`; the daemon never reads `$KUBECONFIG` itself — the CLI resolves it to a path and sends that as the cache coordinate.
- **New `talos` provider.** Same shape as `kubecontext`: reads the Talos config file named by `env.TALOSCONFIG or '~/.talos/config'` as a PathScoped source. Fields: `context` (string), `endpoints` (array), `nodes` (array). Watches the resolved file via `Source::watched_files`.
- **`Source::watched_files`.** New trait method (default: empty) returning the explicit file paths this source needs to watch, given the resolved path. Used by Tier B env-selected-file providers to register per-file watches without relying on pattern-based watch registration.

#### CLI

- **Client-side `env.*` and virtual-field evaluation.** `comb get` / `comb eval` resolve `env.*` references and virtual expressions in the client; a query that references only `env.*` skips the daemon round-trip entirely. Includes a typed evaluator with undeclared-variable discovery and a `basename` filter on the shared minijinja environment.
- **`comb init --write-config`** materialises the built-in virtual-field defaults into `config.toml` (idempotent; safe to re-run).

#### Daemon

- **`--exit-with-parent`** flag: the daemon self-exits when the process that spawned it dies (e.g. an integration that owns the daemon's lifetime).

### Changed (breaking, pre-1.0)

- **BREAKING (pre-1.0):** `aws` daemon provider renamed to `aws_profiles`. `aws.config_region` field removed. `comb get aws_profiles` returns the raw profile dump; `comb get aws` evaluates the consumer virtual namespace. Scripts querying `aws.config_region` must be updated.
- **BREAKING (pre-1.0):** `gcloud` daemon provider renamed to `gcloud_configs`. `gcloud.config_project` field removed. `comb get gcloud_configs` returns the raw config dump; `comb get gcloud` evaluates the consumer virtual namespace. Scripts querying `gcloud.config_project` must be updated.
- **BREAKING (pre-1.0):** `terraform.path_workspace` renamed back to `terraform.workspace`. The P1 rename was forced by a virtual-field self-reference cycle; the `cache.*` model eliminates the cycle, so the field returns to its natural name.
- **BREAKING (pre-1.0):** `python.version` renamed to `python.venv_version`; new `python.local_venv_name` field added. The daemon no longer reads `$VIRTUAL_ENV`. Scripts querying `python.version` must update.
- **BREAKING (pre-1.0): daemon providers no longer read environment variables.** Under the env-cascade model the daemon enumerates on-disk state only; per-shell environment is applied client-side. Removed daemon env reads: `aws` (`$AWS_PROFILE`/`$AWS_REGION`/…), `gcloud` (`$CLOUDSDK_ACTIVE_CONFIG_NAME`), `terraform` (`$TF_WORKSPACE`), `python` (`$VIRTUAL_ENV`). The user-facing `aws.region`, `gcloud.project`, and `terraform.workspace` values are now computed client-side from `env.*` plus the cached data provider, so they finally track the querying shell rather than the daemon's launch environment.
- **BREAKING (pre-1.0): `conda` and `op` are now client-side virtual fields, not daemon providers.** Both were structurally env-frozen as daemon providers (they read the daemon's process environment, which never reflects the querying shell). They are reborn as virtual fields evaluated in the client: `conda.env` resolves `$CONDA_DEFAULT_ENV`, and `op.signed_in` reflects whether `$OP_SERVICE_ACCOUNT_TOKEN` is set — so both finally track the querying shell. The daemon no longer runs the conda/op providers (no `op whoami` subprocess); `op`'s previous `account` field and live-session validity check are gone.

### Fixed

- **git is now correct in worktrees and submodules.** A shared `resolve_git_dir` follows `.git`-as-a-file (`gitdir:` pointers), so `git.state` (rebase/merge/cherry-pick progress) and `git.stash` reflect reality in linked worktrees and submodules instead of always reporting "clean"/0. The git executor no longer inherits `$GIT_DIR` / `$GIT_COMMON_DIR` / `$GIT_WORK_TREE`. Refs moved to a read-always mechanism with a split `GitHead` source.
- **gcloud read a path that doesn't exist.** It read `~/.config/gcloud/properties` (absent on a standard install) and returned empty for essentially everyone; it now follows the `active_config` two-level indirection to the real `configurations/config_<name>/properties`.
- **kubecontext merges a multi-file `$KUBECONFIG`.** A `:`-joined list is merged (later file wins) instead of reading only the first path, and context-name matching is anchored (no more `prod` matching `prod-east`).
- **`user.name` is thread-safe.** Replaced `getpwuid` with `getpwuid_r`, removing undefined behaviour under `spawn_blocking`.
- **direnv reads the allow database directly** instead of shelling out to `direnv status`, so `direnv allow` from any shell is reflected.
- **sudo** omits the `active` field when the (root-only) timestamp file is unreadable, instead of silently reporting `false`.
- **asdf** falls back to the global `~/.tool-versions` and emits a flat field schema consistent with the other providers.
- **network:** removed the hardcoded `en0` interface assumption, added an IPv6 field, and now detects Tailscale (`tailscale0`) as a VPN.
- **A wedged daemon no longer blackholes the socket.** On startup, when another daemon holds the lock with the same build, the new process now probes the canonical socket before exiting: it exits silently only if that daemon is actually serving. A same-build owner that acquired the lock but never bound the socket (or whose socket was deleted) is superseded after a short grace, so a healthy daemon rebinds instead of clients hitting a permanently dead socket. Startup orphan-reaping (which could kill peer daemons on other socket paths) was removed in favour of this targeted probe.

### Developer / release tooling

- **`cargo xtask set-version X.Y.Z`** rewrites all 14 version touchpoints (both Cargo manifests, both lockfiles, the 5 SDK manifests, 3 AUR PKGBUILDs, the nix flake, the `release.yml` rockspec reference, the 8 README download URLs, and the Lua rockspec including its versioned filename) in one command, with count-guarded digit-boundary replacement that aborts on any drift.
- **Releases fire on merge to `main`.** `release.yml` now triggers on push to `main`, derives the version from `Cargo.toml`, tags the merge commit, and publishes — no tag-push trigger and no PAT/App token. The manual `git tag` step is gone from the release process.

### No wire-protocol change

- Env-cascade (env-selected file resolution, Tier B providers) adds **no** wire-protocol change. The existing `path` field on the `Get` request carries the client-resolved file path as the cache coordinate. See `docs/protocol-spec.md`.

## [0.6.1] - 2026-05-29

### Fixed

- **Socket-path resolution now agrees across the daemon, CLI, and every client.** All client SDKs (Rust, Go, Python, Ruby, Node, Lua, C) resolve the daemon socket as `$BEACHCOMBER_SOCKET` → `$XDG_RUNTIME_DIR/beachcomber/sock` → `/tmp/beachcomber-<uid>/sock`, mirroring the daemon's bind path (minus the daemon-only config-file step). Previously the clients ignored `BEACHCOMBER_SOCKET` and consulted `$TMPDIR` (a per-session `/var/folders/...` path on macOS), so on macOS with `XDG_RUNTIME_DIR` unset a client could look in `$TMPDIR` while the daemon bound `/tmp/beachcomber-<uid>/sock` — and they would fail to find each other. There is no longer an existence probe on the `XDG_RUNTIME_DIR` step: clients resolve to the single path the daemon binds and rely on connect-retry; non-standard layouts use `BEACHCOMBER_SOCKET`.
- Node SDK published type artifact (`dist/types.d.ts`) regenerated so the `IntrospectSubject` union reflects the `backoff` → `lifecycle` rename from 0.6.0. The npm publish step rebuilds `dist/` from `src/` via `prepublishOnly`, so the compiled artifact can no longer drift from source.

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
