---
sidebar_position: 4
title: Versioning Policy
---

# Versioning Policy

beachcomber follows [Semantic Versioning 2.0.0](https://semver.org/spec/v2.0.0.html). This document defines what constitutes a breaking change across each public surface.

All versions before 1.0.0 are pre-release. Minor versions may include breaking changes during this period, but they will always be documented in the CHANGELOG.

## Public Surfaces

### Protocol (Unix socket wire format)

| Change | Version bump |
|---|---|
| New operation (e.g., a new `"op"` value) | Minor |
| New optional field in response | Minor |
| Remove or rename an operation | **Major** |
| Change response field semantics | **Major** |
| Change wire encoding (e.g., away from NDJSON) | **Major** |

### Config (config.toml)

| Change | Version bump |
|---|---|
| New config key with a default value | Minor |
| New config section with defaults | Minor |
| Remove or rename a config key | **Major** |
| Change the meaning of an existing value | **Major** |
| Change config file location or format | **Major** |

### Built-in Providers

| Change | Version bump |
|---|---|
| New provider | Minor |
| New field on an existing provider | Minor |
| Remove a provider | **Major** |
| Remove a field from a provider | **Major** |
| Change a field's type or semantics | **Major** |
| Change a provider's default trigger strategy | Minor (documented) |

### CLI (`comb`)

| Change | Version bump |
|---|---|
| New subcommand | Minor |
| New flag on an existing subcommand | Minor |
| Remove a subcommand | **Major** |
| Remove or rename a flag | **Major** |
| Change default output format | **Major** |

### Client SDKs

Each SDK follows its own language ecosystem versioning. SDK versions are independent of the daemon version, but SDK releases will document which daemon protocol version they target.

### Rust Library Crate (`beachcomber-client`)

Standard Rust API compatibility rules apply. Public API changes follow [the Cargo SemVer reference](https://doc.rust-lang.org/cargo/reference/semver.html).

## What Is Not a Public Surface

These may change in any release without a version bump:

- Log output format and messages
- Benchmark results and internal performance characteristics
- Internal module structure and private APIs
- Daemon process management details (PID file location, fork behavior)
- Script provider execution internals (as long as the config contract is maintained)
