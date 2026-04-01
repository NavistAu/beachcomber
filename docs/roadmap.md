# beachcomber Roadmap

Single source of truth for project status and next steps.

Last updated: 2026-03-30

---

## What's Built

### Core (Plans 1-4)

**Daemon:** Single async Rust binary (tokio). Unix socket server with socket activation. Graceful shutdown (CancellationToken + SIGINT). Configurable idle shutdown (disabled by default). File-based logging to `~/.local/state/beachcomber/daemon.log`.

**Cache:** Concurrent DashMap. 157ns read latency. Staleness computation with expected refresh intervals. Auto-poke on cache miss (triggers background computation so next query hits). Detailed cache listing via `comb status`.

**Scheduler:** Poll timers, filesystem watching (notify/FSEvents), poke triggers. Provider execution on `spawn_blocking` (non-blocking). Execution timeouts (configurable, default 10s). Deduplication (in-flight tracking + pending rerun). Failure backoff (exponential after 3 consecutive failures, max 60s). Demand-driven cache warming (QueryActivity keeps providers warm while actively queried). Backoff/drain lifecycle (Grace -> SlowPoll -> Frozen -> Evict).

**Protocol:** get, poke, context, list, status. JSON and text output formats. Connection context for implicit path resolution. Staleness flag in responses. Path canonicalization (relative -> absolute). Subscribe/unsubscribe removed — demand-driven warming replaced explicit subscriptions (ephemeral consumers can't maintain persistent connections).

**CLI:** `comb daemon | get | poke | list | status`

**Config:** TOML at `~/.config/beachcomber/config.toml`. Provider enabled/disabled flag. Provider timeout, poll interval, floor overrides. Script provider definitions. Lifecycle tuning (grace period, idle shutdown).

**Providers (16 built-in + script backend):**

| Provider | Scope | Execution time |
|---|---|---|
| hostname, user | global/once | ~400-650ns |
| load, uptime | global/poll | ~550-660ns |
| kubecontext, gcloud, aws, conda | global/poll | <1µs |
| terraform, python, asdf | path/watch | <1µs |
| direnv, mise | path/watch | varies (process spawn) |
| network | global/poll | 2ms |
| git | path/watch+poll | 5.6ms |
| battery | global/poll | 6ms |

**External:** Script provider backend (JSON or kv output, any language).

**Client SDKs:** Rust (`beachcomber-client`), C (`libbeachcomber`), Python, Node.js (TypeScript), Go, Lua, Ruby. All stdlib-only, with test suites. Lua SDK has three backends (vim.uv, luasocket, CLI fallback).

**Performance:** 42k req/sec. 15µs/query via ClientSession. Comprehensive benchmark suite (criterion).

---

## Milestone: v0.1.0 Public Release

Everything below must be done before the first public release. Ordered by dependency — later items depend on earlier ones.

---

#### Phase A: Naming and Identity

- [x] **A.1 Final name decision.** Project = beachcomber, binary = comb, crate = beachcomber. Researched conflicts on crates.io, Homebrew, GitHub for vigil, witness, patina, conch, shuck, seer, comber, and others. Beachcomber/comb was the cleanest option.
- [x] **A.2 Rename.** Binary name, crate name, config paths, socket paths, all docs, all tests, all benchmarks.

---

#### Phase B: Competitive Research

- [x] **B.1 Deep research on similar projects.** Investigated:
  - `gitstatus` / `gitstatusd` (romkatv) — one-per-shell C++ daemon, 960 threads at scale
  - `watchman` (Meta) — general-purpose file watching, complementary not competitive
  - `direnv` — directory-scoped env management, beachcomber wraps it
  - `starship` — cross-shell prompt, highest-value integration target (55k stars, no caching)
  - `powerline-daemon` — direct ancestor, proved the model but cached the wrong thing (renderer not data)
  - `oh-my-posh` — Go prompt with TTL disk caching, closest existing approach
  - `zoxide`, `atuin` — shell tools with daemon patterns, minimal overlap
- [x] **B.2 Write competitive positioning.** Documented in docs/competitive-landscape.md. Honest comparison with capability gap table.

---

#### Phase C: Documentation

##### C.1 README.md

- [x] **Marketing pitch (top section).** The problem viscerally, the solution in one line, performance numbers, before/after comparison, quick terminal demo.
- [x] **Quick start.** Install, verify, see results in 30 seconds.
- [x] **How it works.** One paragraph + ASCII architecture diagram.
- [x] **Consumer integration examples.** 9 notably different examples:
  - [x] zsh prompt (precmd + shell variable)
  - [x] tmux status bar (`#()` format string replacement)
  - [x] bash prompt (PROMPT_COMMAND)
  - [x] fish prompt (fish_prompt function)
  - [x] neovim statusline (Lua via vim.loop)
  - [x] starship custom module
  - [x] polybar/waybar/sketchybar custom module
  - [x] Python script reading state
  - [x] shell script one-liner for CI/automation
- [x] **Shell fallback function.** Portable functions for bash/zsh/fish — apps can add beachcomber support without requiring it.
- [x] **Configuration reference.** Full config.toml documentation with every field, defaults, and examples.
- [x] **Built-in providers reference.** Every provider with fields, types, default triggers, example output.
- [x] **Custom providers guide.** Script providers with JSON and kv formats, invalidation strategies, real examples.
- [x] **CLI reference.** Every command with examples.
- [x] **Competitive landscape / alternatives.** Shout-outs to gitstatus, watchman, powerline, starship, oh-my-posh.
- [x] **FAQ.** 8 questions covering common concerns.
- [x] **Debugging section.** Log file location, log levels, foreground mode, `comb status`, common issues.
- [x] **Contributing.** Pointer to CONTRIBUTING.md.

##### C.2 Architecture / Internals Documentation

- [x] **Architecture overview.** docs/architecture.md — ASCII data flow diagrams, module map, request lifecycle, provider execution lifecycle, subscription lifecycle, concurrency model, key design decisions.
- [x] **Provider development guide.** docs/provider-development.md — Provider trait, step-by-step walkthrough, InvalidationStrategy guide, performance tiers, testing patterns, script provider reference.
- [x] **Performance considerations.** docs/performance.md — optimization history, provider tier list, regression checklist.

---

#### Phase D: CI/CD and Release Infrastructure

- [x] **D.1 GitHub Actions: CI.** On every PR and push to main:
  - `cargo check`
  - `cargo test` (with known sandbox-limited tests skipped)
  - `cargo clippy -- -D warnings`
  - `cargo fmt -- --check`
  - `cargo bench` (run on main, upload results as artifact)
- [x] **D.2 GitHub Actions: Release.** On tag push (`v*`):
  - Build release binaries for macOS (x86_64 + aarch64)
  - Create GitHub Release with binaries and auto-generated notes
- [x] **D.3 Benchmark regression tracking.** Benchmark results uploaded as artifacts on main pushes.
- [ ] **D.4 SemVer policy.** Document what constitutes breaking changes:
  - Protocol wire format changes = major
  - Config format changes = minor (with backwards compat) or major (without)
  - Provider field additions = minor
  - Provider field removals = major
  - CLI flag changes = minor (additions) or major (removals)

---

#### Phase E: Install Methods

- [ ] **E.1 Homebrew formula.** Primary macOS install method. Tap or core formula. Requires GitHub repo + first release tag.
- [x] **E.2 `cargo install`.** Cargo.toml has metadata, ready for crates.io publication.
- [x] **E.3 Pre-built binaries.** Release workflow creates `beachcomber-<tag>-<target>.tar.gz`.
- [ ] **E.4 Other package managers (future).** Nix, AUR, MacPorts, Scoop. Document how to request packaging.

---

#### Phase F: Repository Hygiene

- [x] **F.1 LICENSE.** MIT license.
- [x] **F.2 CONTRIBUTING.md.** How to contribute, PR process, code of conduct.
- [x] **F.3 CHANGELOG.md.** Started from v0.1.0, keepachangelog.com format.
- [x] **F.4 .gitignore.** target/, .claude/, docs/superpowers/, INIT.md excluded.
- [x] **F.5 Cargo.toml metadata.** description, repository, license, keywords, categories for crates.io.
- [ ] **F.6 CLAUDE.md.** Project-specific Claude Code instructions for contributors who use it.

---

---

## Done: Client SDKs

Client libraries for each language wrapping the Unix socket protocol with typed APIs, socket discovery, timeouts, and error handling.

| SDK | Package manager | Status |
|---|---|---|
| **Rust** (`beachcomber-client`) | crates.io | **Done** (workspace crate) |
| **C** (`libbeachcomber`) | Source / pkg-config | **Done** — embedded JSON parser, shared/static lib, 130 tests |
| **Python** (`beachcomber`) | PyPI | **Done** — sync client + session, dataclasses, 80 tests |
| **Node.js** (`beachcomber`) | npm | **Done** — TypeScript, async API, 62 tests |
| **Go** (`beachcomber`) | Go module | **Done** — idiomatic error returns, 47 tests |
| **Lua** (`beachcomber`) | LuaRocks | **Done** — vim.uv / luasocket / comb CLI fallback, 50 tests |
| **Ruby** (`beachcomber`) | RubyGems | **Done** — block-based sessions, minitest, 45 tests |
| **Shell** (POSIX sh function) | N/A (copy-paste) | **Done** (in README, portable fallback functions) |

---

## Deferred: v0.1.0 Publish

Remaining items to publish v0.1.0. Not blocking current work.

- [ ] Document SemVer policy (D.4)
- [ ] Write CLAUDE.md for contributors (F.6)
- [ ] Add SDK tests to CI (Python pytest, Go test, Node test, C make test, Lua, Ruby minitest)
- [ ] Create GitHub repo (jhogendorn/beachcomber or org)
- [ ] Push code
- [ ] Tag v0.1.0 to trigger release workflow
- [ ] `cargo publish` beachcomber + beachcomber-client to crates.io
- [ ] Create Homebrew tap with formula (E.1)
- [ ] Publish Python SDK to PyPI
- [ ] Publish Node.js SDK to npm
- [ ] Publish Go module (tagged in repo)
- [ ] Publish Lua SDK to LuaRocks
- [ ] Publish Ruby SDK to RubyGems
- [ ] Publish C SDK source tarball / pkg-config

---

## Deferred: Post-Launch

### Linux Support

- [ ] Battery provider: read `/sys/class/power_supply/`
- [ ] Network provider: Linux `getifaddrs` + `iwgetid` for SSID
- [ ] Uptime provider: read `/proc/uptime`
- [ ] Conditional compilation: `#[cfg(target_os)]` blocks in platform-specific providers
- [ ] CI: Add Linux test matrix
- [ ] Release workflow: Add Linux x86_64 + aarch64 targets

### External Provider Backends

- [ ] Lua backend via `mlua` crate
- [ ] Shared library backend via `libloading` crate

### Additional Features

- [ ] Watchdog — detect scheduler stalls, auto-restart
- [ ] Configurable backoff steps
- [ ] Push/streaming mode — stream cache updates to long-lived connections
- [ ] `comb watch <key> [path]` CLI command — stream value changes to stdout

### Install Methods

- [ ] Nix package
- [ ] AUR package
- [ ] MacPorts
- [ ] Scoop (Windows, if/when Windows support lands)

### Stability

- [ ] Protocol stability guarantee (wire format frozen)
- [ ] Config format stability guarantee
- [ ] mmap/shared memory for zero-latency reads (if demand exists)
- [ ] Consumer integration packages (zsh plugin, tmux plugin, neovim plugin)
