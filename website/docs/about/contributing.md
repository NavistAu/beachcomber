---
sidebar_position: 5
title: Contributing
---

# Contributing to beachcomber

Thanks for your interest in contributing! beachcomber is a Rust project that benefits from contributions of all kinds — bug reports, documentation improvements, new providers, performance optimizations, and consumer integrations.

## Getting Started

```sh
git clone https://github.com/OWNER/beachcomber.git
cd beachcomber
cargo build
cargo nextest run    # mandatory — plain cargo test aborts immediately with exit code 2
```

The binary is `comb` (not `beachcomber`):

```sh
cargo run -- get hostname.name
```

## Development

### Prerequisites

- Rust 1.85+ (see `mise.toml` for exact version)
- macOS and Linux

### Running Tests

```sh
cargo nextest run                                                              # preferred
cargo nextest run -E 'not (test(watcher_) + test(uptime_provider_executes))'   # skip env-sensitive tests
```

`cargo-nextest` is the blessed test runner. Install once with `cargo install cargo-nextest --locked`. Config at `.config/nextest.toml` enforces a 2-minute global wall-clock cap on the suite plus a 15s-warn / 30s-kill cap per test — a hung test terminates the run with a failure instead of blocking forever.

Plain `cargo test` aborts immediately with exit code 2 (a ctor advisory fires before any test runs). Use `cargo nextest run` exclusively.

Some tests require filesystem watching (FSEvents) and may not work inside sandboxed environments. These are the `watcher_*` tests and the `uptime_provider_executes` test; skip them as shown.

### Running Benchmarks

```sh
cargo bench
```

See `docs/performance.md` for the performance regression checklist — verify these thresholds before submitting PRs that touch the hot path.

### Project Structure

See `docs/architecture.md` for a full overview. Key files:

- `src/scheduler/` — the core: trigger management, provider execution, subscriptions
- `src/server.rs` — Unix socket server, protocol handling
- `src/cache.rs` — concurrent cache (DashMap)
- `defaults/` — the shipped providers, one file each
- `src/provider/` — the registry and `ConfigSource`; `src/plugin/` — the compiled-in plugins
- `benches/` — criterion benchmarks

## Contributing a New Provider

See `docs/provider-development.md`, "Writing a Source". The short version:

1. Write `defaults/yourprovider.toml`, and add it to `SHIPPED` in `src/daemon/defaults.rs`
2. Add tests in `tests/suite/shipped/yourprovider.rs`, registered as a `mod` in `tests/suite/shipped/mod.rs`
3. Run `cargo nextest run -E 'test(shipped::yourprovider::)'`

**Performance guidelines:** Read a file instead of spawning a process whenever possible. See the performance tiers in `docs/performance.md`.

## Pull Requests

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
