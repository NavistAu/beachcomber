# beachcomber

A daemon that caches shell environment state. macOS and Linux.

- Binary `comb`. Crates: `beachcomber` (workspace root), `libbeachcomber` (Rust client), `libbeachcomber-ffi` (C-ABI
  cdylib for the SDKs).
- Rust and `cargo-nextest` pinned in `mise.toml` (`mise install`); run cargo via `mise exec --` where it is not on
  PATH. `vendor/` is a gitignored local cache, not used by the build.
- Open work: `docs/roadmap.md`. Shipped: `CHANGELOG.md`. Also `docs/decisionlog/`, and in `website/docs/`:
  `internals/architecture.md`, `internals/performance.md`, `reference/protocol-reference.md` (wire protocol).

## Branches and releases

- `develop` is the default branch and integration target. `main` is the release branch, advanced only by a
  `develop` → `main` PR; branch protection requires green CI.
- Release: on `develop`, `cargo xtask set-version X.Y.Z` (every version touchpoint) + CHANGELOG; merge the
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

- The whole suite should finish in about 10s: a slower run is a regression to find and fix, not to accommodate.
  `.config/nextest.toml`'s `global-timeout = "60s"` only stops a hang (the `ci` profile allows 10m).
- `uptime_provider_executes` needs an unsandboxed environment; CI runs
  `cargo nextest run --workspace -E 'not test(uptime_provider_executes)'`. Without `--workspace` only the root crate's
  tests run, not `libbeachcomber`'s.
- Watcher tests use `FsWatcher::new_polling` (works under sandboxes without FSEvents/inotify); production uses
  `FsWatcher::new`.
- Unit tests are `#[cfg(test)]` modules in `src/`. Integration tests are one binary per crate,
  `tests/suite/main.rs`, with a module per file.

## Architecture

Async tokio daemon on a Unix socket, one task per connection.

- `src/scheduler/` — poll timers, filesystem watching (notify), demand tracking, cache lifecycle (Active / Decay1-4 /
  Evicted; `src/scheduler/lifecycle.rs`), provider execution via `spawn_blocking` (providers are synchronous).
- `src/server.rs` — accept loop; `src/server/` has a module per op (`read`, `put`, `refresh`, `status`,
  `introspect`, `watch`) and `connection.rs` dispatches to them. `src/cache/` — DashMap keyed `"provider\0path"`.
- `src/protocol.rs` re-exports `libbeachcomber::protocol` (NDJSON `Request`/`Response`, shared with the C ABI and
  SDKs). `src/config.rs` — TOML, XDG paths, env files (`#[serde(default)]` throughout).
- `src/provider/` — the `Source` trait and `ConfigSource`, its one implementation: every provider is config
  (`defaults/<name>.toml` plus the user's), registered by `registry.rs`. Field values are `provider::Value`.
- `src/plugin/` — the compiled-in `sys` and `git` plugins; `src/boundaries/library.rs` loads `.so`/`.dylib` C-ABI
  plugins via `libloading`.
- `src/daemon/` — process lifecycle, watchdog on the scheduler heartbeat.
- Only actively queried keys poll at the base rate; idle keys decay with exponential backoff and are evicted.
- Adding a provider: `website/docs/internals/provider-development.md`.

## Protocol and config

- NDJSON over the socket, `{"op":"..."}` dispatch: `get`, `refresh`, `put`, `memo`, `watch`, `status`, `layers`,
  `introspect`, `hello`. Keys are `provider.field` or `provider:facet.field`. Responses
  `{"ok":true,"data":...,"age_ms":...}`, absent fields omitted. Wire contract:
  `website/docs/reference/protocol-reference.md`. A `put` writes the `:put` layer, which hides what it covers.
- Config `~/.config/beachcomber/config.toml` (+ `conf.d/`): `[daemon]` and `[client]`; Sources at
  `[daemon.providers.<p>.sources.<s>]`.
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
