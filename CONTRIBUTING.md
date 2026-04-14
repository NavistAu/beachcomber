# Contributing to beachcomber

Thanks for your interest in contributing! beachcomber is a Rust project that benefits from contributions of all kinds — bug reports, documentation improvements, new providers, performance optimizations, and consumer integrations.

## Getting Started

```sh
git clone https://github.com/OWNER/beachcomber.git
cd beachcomber
cargo build
cargo test
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
cargo test
```

Some tests require filesystem watching (FSEvents) and may not work inside sandboxed environments. These are the `watcher` tests and the `uptime_provider_executes` test.

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
4. Run `cargo test` and `cargo bench --bench providers`

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
