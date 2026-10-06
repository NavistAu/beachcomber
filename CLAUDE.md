# CLAUDE.md

Project-specific instructions for contributors using Claude Code.

## Read first

Before proposing changes or reasoning about behavior, read these. They are the authoritative project state — code alone will mislead you.

- **`docs/roadmap.md`** — open, forward-looking work only: planned features and deferred design work, grouped by area. It is **not** a bug tracker, an engineering log, or a session scratchpad. Done items are deleted, not annotated — shipped work lives in `CHANGELOG.md`, fix histories in git. Investigation write-ups belong in commit messages; never append them here.
- **`CHANGELOG.md`** — what shipped when, at what version.

## `docs/canon/` is deprecated

`docs/canon/` was meant to be the strict line of truth: canon specs define the tests, the tests define the code, the
docs describe the code. As it stands it has not worked or helped, and we need to find another, better way of
answering "are the docs or the code correct?". Until that approach exists, the files in `docs/canon/` are deprecated
and historical: read them for background only. They do not bind tests or code, they do not need approval to diverge
from, and they are not updated as behaviour changes. (Owner ruling, 2026-10-07.)

## Basics

- **Project:** beachcomber — a daemon that caches shell environment state
- **Binary:** `comb` (not `beachcomber`)
- **Crate:** `beachcomber` (workspace root) + `libbeachcomber` (client library) + `libbeachcomber-ffi` (C-ABI cdylib for the SDKs)
- **Rust version:** pinned in `mise.toml` — use `mise install` to get it
- **Platform:** macOS and Linux

## Branch workflow

- **`develop` is the default branch and the integration target.** All feature/fix work branches off `develop` and merges back into `develop` via PR. Routine commits land on `develop` (or short-lived branches off it). Never commit directly to `main`.
- **`main` is the release branch.** It only ever advances through a PR from `develop` → `main`. That PR is the **release gate**: branch protection on `main` requires all CI checks green before it can merge (no direct pushes, no force-pushes).
- **Releasing** = bump the version on `develop` (`cargo xtask set-version`), open a `develop` → `main` PR, wait for CI green, and merge. **Merging is the release:** `release.yml` triggers on push to `main`, reads the version from `Cargo.toml`, tags `vX.Y.Z` on the merge commit, and builds + publishes to all registries. No manual tagging. Full steps in `docs/releasing.md`.
- **CI** (`.github/workflows/ci.yml`) runs on pushes to and PRs targeting both `develop` and `main`.
- Delete branches and worktrees after merge — keep the branch list to `main` + `develop` plus whatever is actively in flight.
- `git push` still requires explicit per-instance confirmation (see the global rules); committing to `develop` does not.

## Build and Test

```sh
cargo build              # debug build
cargo nextest run        # run tests (mandatory, cargo test is designed to instantly fail)
cargo t                  # shorthand alias for `cargo nextest run`
cargo bench              # criterion benchmarks (cache, protocol, providers, socket, throughput)
cargo clippy -- -D warnings
cargo fmt -- --check
```

**Test runner:** `cargo-nextest` is the blessed runner. Run `mise install` to get it alongside Rust — it is declared in `mise.toml`. Use `cargo nextest run` or the shorter `cargo t` alias we ship. Plain `cargo test` triggers an advisory that fires immediately on binary startup (via `ctor`) before any test runs — it prints an instructive message and exits with code 2. Set `NEXTEST=1` in the environment to bypass it intentionally.

Tests that are environment-sensitive:
- `uptime_provider_executes` — needs unsandboxed environment

CI skips with: `cargo nextest run -E 'not test(uptime_provider_executes)'`.

**Watcher tests** use `FsWatcher::new_polling` (Phase 13 stabilization), so they run reliably under sandboxed hosts that can't use FSEvents / inotify. Production code still uses `FsWatcher::new` which picks the kernel-native backend.

**Verification budget (hard rules, not targets):**

This is a small daemon+CLI project. Verification and formatting should take a few seconds; 30s is the generous hard ceiling, not the expectation.

- The full test suite runs in under 30s or it fails. `global-timeout = "30s"` in `.config/nextest.toml` enforces this; raising it, or any other limit, requires explicit owner sign-off. Invoke suite runs with a 30s command timeout — never a generous one "for the compile".
- Verification happens at milestones — a task's commit gate, a plan's close-out — not after every edit. Iterate with `cargo check`; do not run integration tests a change cannot affect.
- Between milestones, run only what the change touches: unit tests via `cargo nextest run --lib`, or a module of the integration suite via `-E 'test(<module>::)'`.
- A verification that overruns its budget is a failure to investigate, not a reason to raise the budget or rerun with a longer timeout.

## Architecture

Async tokio daemon listening on a Unix socket. One task per connection.

- `src/scheduler/` — core loop: poll timers, filesystem watching (notify), demand tracking, cache lifecycle (Active / Decay1-4 / Evicted), provider execution. See `src/scheduler/lifecycle.rs` for the state machine.
- `src/server.rs` — socket accept loop, spawns connection tasks
- `src/cache.rs` — DashMap with null-byte-separated keys (`"provider\0path"`)
- `src/protocol.rs` — NDJSON wire format, `Request`/`Response` types
- `src/config.rs` — TOML config loading, XDG paths, env file parsing
- `src/provider/` — all providers implement the `Provider` trait (19 built-in)
- `src/provider/registry.rs` — registers built-in + config-defined providers
- `src/provider/library.rs` — shared library provider backend via `libloading`; loads `.so`/`.dylib` with C ABI
- `src/daemon.rs` — process lifecycle, watchdog task (monitors scheduler heartbeat, triggers shutdown on stall)

Providers are synchronous — the scheduler runs them via `tokio::task::spawn_blocking`.

Demand-driven warming: only actively-queried keys are polled at the provider's base rate. Idle keys enter an exponential-backoff decay (see `docs/cache-lifecycle.md`) and are eventually evicted.

## Protocol

NDJSON over Unix socket. Requests use `{"op":"..."}` tag dispatch. Operations: `get`, `refresh`, `put`, `watch`, `context`, `status`.

Key format: `provider.field` (e.g., `git.branch`). `split_key()` splits on first `.`.

Responses: `{"ok":true,"data":...,"age_ms":...,"stale":...}` — fields omitted when null.

Virtual providers created by `put` are data-only entries in the cache — no `execute()`, namespace hierarchy builtin > script > virtual.

## Config

TOML at `~/.config/beachcomber/config.toml`. Three sections: `[daemon]`, `[lifecycle]`, `[providers.<name>]`.

Socket path resolution (daemon): config override → `$BEACHCOMBER_SOCKET` → `/tmp/beachcomber-<uid>/sock`. Client SDKs mirror this minus the config-file step (`$BEACHCOMBER_SOCKET` → `/tmp/beachcomber-<uid>/sock`). No session-scoped environment is consulted (`$TMPDIR`, `$XDG_RUNTIME_DIR`) — see `docs/canon/singleton.md`.

## SDKs

Client SDKs live in `sdks/` — one directory per language (C, Go, Lua, Node.js, Python, Ruby). Each is self-contained with its own test suite. Each SDK binds the shared `libbeachcomber` C ABI (ctypes/fiddle/purego/LuaJIT-ffi/koffi). Python, Ruby, and C are stdlib-only. Go vendors `purego`. Node.js takes `koffi` as an optional peer dependency. Node and PUC-Lua (no `ffi`) have subprocess fallback tiers.

The Rust client SDK is `libbeachcomber/` (workspace member).

## Dependencies

All Rust dependencies are vendored in `vendor/`. The project builds offline.

## Adding a Provider

See `docs/provider-development.md`. Short version:

1. `src/provider/yourprovider.rs` — implement `Provider` trait
2. `tests/suite/provider_yourprovider.rs` — integration tests, registered as a `mod` in `tests/suite.rs`
3. Register in `src/provider/mod.rs` and `src/provider/registry.rs`
4. Prefer reading files over spawning processes (see performance tiers in `docs/performance.md`)

## Releasing

See `docs/releasing.md` for the full release checklist. Short version: on `develop`, run `cargo xtask set-version X.Y.Z` (bumps all 14 version touchpoints) and update CHANGELOG; open a `develop` → `main` PR (the release gate) and wait for CI green; merge it. **Merging is the release** — `release.yml` triggers on push to `main`, tags `vX.Y.Z` on the merge commit, and builds + publishes to all registries (incl. Homebrew). No manual tagging.

## Key Conventions

- Unit tests live in `src/` as `#[cfg(test)]` modules next to the code they test. Integration tests (anything needing `TestDaemon`, a socket, or the built `comb` binary via `CARGO_BIN_EXE`) are modules of the single `tests/suite.rs` binary — one module per functional area under `tests/suite/`. Never add a new top-level file in `tests/`: each one is a separate executable that relinks the whole crate.
- Benchmarks in `benches/` use criterion
- `cargo clippy -- -D warnings` must pass with zero warnings
- Provider fields are `serde_json::Value` — providers return `HashMap<String, Value>`
- Config uses `#[serde(default)]` throughout — every field has a sensible default
