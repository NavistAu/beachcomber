# Contributing to beachcomber

Thanks for your interest in contributing! beachcomber is a Rust project that benefits from contributions of all kinds — bug reports, documentation improvements, new providers, performance optimizations, and consumer integrations.

## Getting Started

```sh
git clone https://github.com/OWNER/beachcomber.git
cd beachcomber
mise install         # installs Rust + cargo-nextest
cargo build
cargo nextest run    # or: cargo t (shorthand alias we ship)
```

The binary is `comb` (not `beachcomber`):

```sh
cargo run -- get hostname.name
```

## Development

### Prerequisites

- Rust 1.85+ (see `mise.toml` for exact version)
- macOS (Linux support is planned but not yet implemented)

### Running Tests

```sh
cargo nextest run                                                              # preferred
cargo nextest run -E 'not (test(watcher_) + test(uptime_provider_executes))'   # skip env-sensitive tests
```

`cargo-nextest` is the blessed test runner. It is declared in `mise.toml`, so `mise install` provides it automatically. Config at `.config/nextest.toml` enforces a 2-minute global wall-clock cap on the suite plus a 15s-warn / 30s-kill cap per test — a hung test terminates the run with a failure instead of blocking forever.

`.cargo/config.toml` ships a `t` alias (`cargo t`) as a shorthand for `cargo nextest run`. Plain `cargo test` triggers an advisory that fires immediately on binary startup (via `ctor`) before any test runs — it prints an instructive message and exits with code 2. Set `NEXTEST=1` in the environment to bypass it intentionally.

Some tests require filesystem watching (FSEvents) and may not work inside sandboxed environments. These are the `watcher_*` tests and the `uptime_provider_executes` test; skip them as shown.

### Running Benchmarks

```sh
cargo bench
```

See `docs/performance.md` for the performance regression checklist — verify these thresholds before submitting PRs that touch the hot path.

### Project Structure

See `docs/architecture.md` for a full overview. Key files:

- `src/scheduler.rs` — the core: trigger management, provider execution, subscriptions
- `src/server.rs` — Unix socket server, protocol handling
- `src/cache.rs` — concurrent cache (DashMap)
- `src/provider/` — all provider implementations
- `benches/` — criterion benchmarks

## Contributing a New Provider

See `docs/provider-development.md` for a step-by-step walkthrough. The short version:

1. Create `src/provider/yourprovider.rs` implementing the `Provider` trait
2. Add tests in `tests/provider_yourprovider.rs`
3. Register in `src/provider/mod.rs` and `src/provider/registry.rs`
4. Run `cargo nextest run -E 'test(provider_yourprovider)'` and `cargo bench --bench providers`

**Performance guidelines:** Read a file instead of spawning a process whenever possible. See the performance tiers in `docs/performance.md`.

## Branch Workflow

beachcomber uses a two-branch model:

- **`develop`** is the default branch and the integration target. Branch your feature/fix work off `develop` and open PRs **back into `develop`**. This is where day-to-day development lands.
- **`main`** is the release branch. It only advances via a PR from `develop` → `main`, and that PR is the **release gate**: `main` is protected so a PR cannot merge until all CI checks pass (no direct pushes to `main`).
- **Releases** are cut from `main`: merging a `develop` → `main` PR *is* the release. `release.yml` triggers on push to `main`, reads the version from `Cargo.toml`, tags `vX.Y.Z` on the merge commit, and publishes. No manual tagging. See `docs/releasing.md`.

CI (`.github/workflows/ci.yml`) runs on pushes to and PRs targeting both `develop` and `main`, so your PR into `develop` is fully checked before it lands.

## Pull Requests

- **Target `develop`** (the default branch), not `main` — only release PRs go `develop` → `main`
- One logical change per PR
- Include tests for new functionality
- Run `cargo clippy` and `cargo fmt` before submitting
- If touching performance-sensitive code, include benchmark results (before/after)
- Update docs if you change user-facing behavior

## Reporting Bugs

Open an issue with:
- What you expected to happen
- What actually happened
- `comb --version` output
- macOS version
- Steps to reproduce

## Code of Conduct

Be kind, be constructive, assume good intent. We're all here to make terminals better.
