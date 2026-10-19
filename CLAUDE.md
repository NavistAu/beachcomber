# beachcomber

A daemon that caches shell environment state. macOS and Linux.

- Binary `comb`. Crates: `beachcomber` (workspace root), `libbeachcomber` (Rust client), `libbeachcomber-ffi` (C-ABI
  cdylib for the SDKs).
- Rust and `cargo-nextest` pinned in `mise.toml` (`mise install`). All Rust deps vendored in `vendor/`; builds offline.
- Open work: `docs/roadmap.md`. Shipped: `CHANGELOG.md`. Also `docs/architecture.md`, `docs/protocol-spec.md`,
  `docs/performance.md`, `docs/decisionlog/`.
- `docs/canon/` is deprecated and historical (2026-10-07): background only; it does not bind tests or code.

## Branches and releases

- `develop` is the default branch and integration target. `main` is the release branch, advanced only by a
  `develop` → `main` PR; branch protection requires green CI.
- Release: on `develop`, `cargo xtask set-version X.Y.Z` (all 14 version touchpoints) + CHANGELOG; merge the
  `develop` → `main` PR. `release.yml` on push to `main` tags `vX.Y.Z` and publishes to all registries (incl. Homebrew).
  Checklist: `docs/releasing.md`.
- CI (`.github/workflows/ci.yml`) runs on pushes/PRs to `develop` and `main`.

## Build and test

```sh
cargo build
cargo nextest run        # or `cargo t`; plain `cargo test` exits 2 at startup (ctor advisory) unless NEXTEST=1
cargo nextest run --lib  # unit tests only
cargo bench              # criterion (cache, protocol, providers, socket, throughput)
cargo clippy -- -D warnings
cargo fmt -- --check
```

- `.config/nextest.toml` sets `global-timeout = "30s"` for the suite.
- `uptime_provider_executes` needs an unsandboxed environment; CI runs
  `cargo nextest run -E 'not test(uptime_provider_executes)'`.
- Watcher tests use `FsWatcher::new_polling` (works under sandboxes without FSEvents/inotify); production uses
  `FsWatcher::new`.
- Unit tests are `#[cfg(test)]` modules in `src/`. Integration tests live in `tests/`; each top-level file is a separate
  binary that relinks the crate (`feat/one-tree` consolidates them into `tests/suite.rs` + `tests/suite/`).

## Architecture

Async tokio daemon on a Unix socket, one task per connection.

- `src/scheduler/` — poll timers, filesystem watching (notify), demand tracking, cache lifecycle (Active / Decay1-4 /
  Evicted; `src/scheduler/lifecycle.rs`), provider execution via `spawn_blocking` (providers are synchronous).
- `src/server.rs` — accept loop. `src/cache.rs` — DashMap keyed `"provider\0path"`.
- `src/protocol.rs` — NDJSON `Request`/`Response`. `src/config.rs` — TOML, XDG paths, env files (`#[serde(default)]`
  throughout).
- `src/provider/` — `Provider` trait implementations; `registry.rs` registers built-in + config-defined;
  `library.rs` loads `.so`/`.dylib` C-ABI providers via `libloading`. Provider fields are `serde_json::Value`.
- `src/daemon/` — process lifecycle, watchdog on the scheduler heartbeat.
- Only actively queried keys poll at the base rate; idle keys decay with exponential backoff and are evicted.
- Adding a provider: `docs/provider-development.md`.

## Protocol and config

- NDJSON over the socket, `{"op":"..."}` dispatch: `get`, `refresh`, `put`, `watch`, `context`, `status`. Keys are
  `provider.field` (`split_key()` splits on the first `.`). Responses `{"ok":true,"data":...,"age_ms":...,"stale":...}`,
  null fields omitted. `put` creates data-only virtual providers; namespace precedence builtin > script > virtual.
- Config `~/.config/beachcomber/config.toml`: `[daemon]`, `[lifecycle]`, `[providers.<name>]`.
- Socket path: config override → `$BEACHCOMBER_SOCKET` → `/tmp/beachcomber-<uid>/sock` (SDKs skip the config step).
  `$TMPDIR` and `$XDG_RUNTIME_DIR` are not consulted.
- SDKs in `sdks/` (c, go, lua, node, python, ruby), each self-contained with its own tests, binding the
  `libbeachcomber` C ABI. Python, Ruby, C stdlib-only; Go vendors `purego`; Node takes `koffi` as an optional peer;
  Node and PUC-Lua have subprocess fallbacks.

## How comb behaves

- One tree, read at any depth: `comb get git` and `comb get git:cache` return that whole subtree; every facet
  (`:template`, `:memo`, `:put`, `:source`, `:cache`) has the provider's shape.
- Nearer directory beats farther; globals (`~`) are defaults, never overrides. A Source's own setting beats its
  prototype's (`_`).
- Values merge per leaf up the directory chain to `~`.
- A read of a missing value runs whatever provides it (cold-miss path); nothing pre-computes.
- `null` means absent, never `""`.
