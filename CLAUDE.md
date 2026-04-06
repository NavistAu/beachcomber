# CLAUDE.md

Project-specific instructions for contributors using Claude Code.

## Basics

- **Project:** beachcomber — a daemon that caches shell environment state
- **Binary:** `comb` (not `beachcomber`)
- **Crate:** `beachcomber` (workspace root) + `beachcomber-client` (client library)
- **Rust version:** pinned in `mise.toml` — use `mise install` to get it
- **Platform:** macOS only (Linux support is planned, not yet implemented)

## Build and Test

```sh
cargo build              # debug build
cargo test               # all tests
cargo bench              # criterion benchmarks (cache, protocol, providers, socket, throughput)
cargo clippy -- -D warnings
cargo fmt -- --check
```

Tests that require FSEvents or are environment-sensitive:
- `watcher_*` tests — need real filesystem watching
- `uptime_provider_executes` — needs unsandboxed environment

CI skips these with `--skip watcher_ --skip uptime_provider_executes`.

## Architecture

Async tokio daemon listening on a Unix socket. One task per connection.

- `src/scheduler.rs` — core loop: poll timers, filesystem watching (notify), demand tracking, backoff, provider execution
- `src/server.rs` — socket accept loop, spawns connection tasks
- `src/cache.rs` — DashMap with null-byte-separated keys (`"provider\0path"`)
- `src/protocol.rs` — NDJSON wire format, `Request`/`Response` types
- `src/config.rs` — TOML config loading, XDG paths, env file parsing
- `src/provider/` — all providers implement the `Provider` trait
- `src/provider/registry.rs` — registers built-in + config-defined providers

Providers are synchronous — the scheduler runs them via `tokio::task::spawn_blocking`.

Demand-driven warming: only actively-queried keys are polled. Idle keys go through Grace → SlowPoll → Frozen → Evict.

## Protocol

NDJSON over Unix socket. Requests use `{"op":"..."}` tag dispatch. Operations: `get`, `poke`, `store`, `watch`, `context`, `list`, `status`.

Key format: `provider.field` (e.g., `git.branch`). `split_key()` splits on first `.`.

Responses: `{"ok":true,"data":...,"age_ms":...,"stale":...}` — fields omitted when null.

Virtual providers created by `store` are data-only entries in the cache — no `execute()`, namespace hierarchy builtin > script > virtual.

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

## Key Conventions

- One test file per functional area in `tests/`
- Benchmarks in `benches/` use criterion
- `cargo clippy -- -D warnings` must pass with zero warnings
- Provider fields are `serde_json::Value` — providers return `HashMap<String, Value>`
- Config uses `#[serde(default)]` throughout — every field has a sensible default
