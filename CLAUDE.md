# CLAUDE.md

Project-specific instructions for contributors using Claude Code.

## Basics

- **Project:** beachcomber — a daemon that caches shell environment state
- **Binary:** `comb` (not `beachcomber`)
- **Crate:** `beachcomber` (workspace root) + `beachcomber-client` (client library)
- **Rust version:** pinned in `mise.toml` — use `mise install` to get it
- **Platform:** macOS and Linux

## Build and Test

```sh
cargo build              # debug build
cargo nextest run        # run tests (preferred — has a 2-minute global kill)
cargo t                  # shorthand alias for `cargo nextest run`
cargo bench              # criterion benchmarks (cache, protocol, providers, socket, throughput)
cargo clippy -- -D warnings
cargo fmt -- --check
```

**Test runner:** `cargo-nextest` is the blessed runner. Run `mise install` to get it alongside Rust — it is declared in `mise.toml`. Use `cargo nextest run` (full runner, 2m kill-on-hang + 30s per-test) or the shorter `cargo t` alias we ship. Plain `cargo test` triggers an advisory that fires immediately on binary startup (via `ctor`) before any test runs — it prints an instructive message and exits with code 2. Set `NEXTEST=1` in the environment to bypass it intentionally.

Tests that are environment-sensitive:
- `uptime_provider_executes` — needs unsandboxed environment

CI skips with: `cargo nextest run -E 'not test(uptime_provider_executes)'`.

**Watcher tests** use `FsWatcher::new_polling` (Phase 13 stabilization), so they run reliably under sandboxed hosts that can't use FSEvents / inotify. Production code still uses `FsWatcher::new` which picks the kernel-native backend.

## Architecture

Async tokio daemon listening on a Unix socket. One task per connection.

- `src/scheduler.rs` — core loop: poll timers, filesystem watching (notify), demand tracking, backoff, provider execution
- `src/server.rs` — socket accept loop, spawns connection tasks
- `src/cache.rs` — DashMap with null-byte-separated keys (`"provider\0path"`)
- `src/protocol.rs` — NDJSON wire format, `Request`/`Response` types
- `src/config.rs` — TOML config loading, XDG paths, env file parsing
- `src/provider/` — all providers implement the `Provider` trait (19 built-in)
- `src/provider/registry.rs` — registers built-in + config-defined providers
- `src/provider/library.rs` — shared library provider backend via `libloading`; loads `.so`/`.dylib` with C ABI
- `src/daemon.rs` — process lifecycle, watchdog task (monitors scheduler heartbeat, triggers shutdown on stall)

Providers are synchronous — the scheduler runs them via `tokio::task::spawn_blocking`.

Demand-driven warming: only actively-queried keys are polled. Idle keys go through Grace → SlowPoll → Frozen → Evict.

## Protocol

NDJSON over Unix socket. Requests use `{"op":"..."}` tag dispatch. Operations: `get`, `refresh`, `put`, `watch`, `context`, `status`.

Key format: `provider.field` (e.g., `git.branch`). `split_key()` splits on first `.`.

Responses: `{"ok":true,"data":...,"age_ms":...,"stale":...}` — fields omitted when null.

Virtual providers created by `put` are data-only entries in the cache — no `execute()`, namespace hierarchy builtin > script > virtual.

## Config

TOML at `~/.config/beachcomber/config.toml`. Three sections: `[daemon]`, `[lifecycle]`, `[providers.<name>]`.

Socket path resolution: config override → `$XDG_RUNTIME_DIR/beachcomber/sock` → `$TMPDIR/beachcomber-<uid>/sock`.

## SDKs

Client SDKs live in `sdks/` — one directory per language (C, Go, Lua, Node.js, Python, Ruby). Each is self-contained with its own test suite. All are stdlib-only (no external dependencies).

The Rust client SDK is `beachcomber-client/` (workspace member).

## Dependencies

All Rust dependencies are vendored in `vendor/`. The project builds offline.

## Adding a Provider

See `docs/provider-development.md`. Short version:

1. `src/provider/yourprovider.rs` — implement `Provider` trait
2. `tests/provider_yourprovider.rs` — integration tests
3. Register in `src/provider/mod.rs` and `src/provider/registry.rs`
4. Prefer reading files over spawning processes (see performance tiers in `docs/performance.md`)

## Releasing

See `docs/releasing.md` for the full release checklist. Short version: bump all version files, update CHANGELOG, push, wait for CI green, then `git tag vX.Y.Z && git push origin vX.Y.Z`. The tag triggers the release workflow which builds, publishes to all registries, and updates Homebrew.

## Key Conventions

- One test file per functional area in `tests/`
- Benchmarks in `benches/` use criterion
- `cargo clippy -- -D warnings` must pass with zero warnings
- Provider fields are `serde_json::Value` — providers return `HashMap<String, Value>`
- Config uses `#[serde(default)]` throughout — every field has a sensible default
