# beachcomber Roadmap

Single source of truth for project status and next steps.

Last updated: 2026-03-28

---

## What's Built

### Core (Plans 1-3 + Production Hardening)

**Daemon:** Single async Rust binary (tokio). Unix socket server with socket activation. Graceful shutdown (CancellationToken + SIGINT). Auto-shutdown after idle timeout.

**Cache:** Concurrent DashMap. 157ns read latency. Staleness computation with expected refresh intervals.

**Scheduler:** Poll timers, filesystem watching (notify/FSEvents), poke triggers. Provider execution on `spawn_blocking` (non-blocking). Execution timeouts (configurable, default 10s). Deduplication (in-flight tracking + pending rerun). Failure backoff (exponential after 3 consecutive failures, max 60s). Subscription manager with multi-tenant cadence resolution. Backoff/drain lifecycle (Grace -> SlowPoll -> Frozen -> Evict).

**Protocol:** get, poke, subscribe, unsubscribe, context, list, status. JSON and text output formats. Connection context for implicit path resolution. Staleness flag in responses.

**CLI:** `comb daemon | get | poke | subscribe | list | status`

**Config:** TOML at `~/.config/beachcomber/config.toml` (binary: `comb`). Provider enabled/disabled flag. Provider timeout, poll interval, floor overrides. Script provider definitions. Lifecycle tuning (grace period, idle shutdown).

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

**Performance:** 42k req/sec. 15µs/query via ClientSession. Comprehensive benchmark suite (criterion).

---

## What's Next

### Milestone: v0.1.0 Public Release

Everything below must be done before the first public release. Ordered by dependency — later items depend on earlier ones.

---

#### Phase A: Naming and Identity

Before anything public-facing can be written (README, brew formula, crate name, binary name), the final name must be decided.

- [ ] **A.1 Final name decision.** "beachcomber" is the working name. Considerations: is the name taken on crates.io, homebrew, GitHub? Is it memorable, searchable, unambiguous? Short enough for CLI usage? Alternatives to consider.
- [ ] **A.2 Rename if needed.** Binary name, crate name, config paths, socket paths, all docs.

---

#### Phase B: Competitive Research

Must happen before README is written — the positioning depends on understanding the landscape.

- [ ] **B.1 Deep research on similar projects.** Investigate:
  - `gitstatus` / `gitstatusd` (romkatv) — the git-specific daemon we're replacing
  - `watchman` (Meta) — general-purpose file watching service
  - `direnv` — directory-scoped env management (similar watch pattern)
  - `starship` — cross-shell prompt with built-in git/battery/etc (potential consumer AND competitor)
  - `powerline-daemon` — the original "cache prompt data in a daemon" approach
  - `zoxide`, `atuin` — other shell tools that use daemon patterns
  - Any other tools doing centralized shell state caching
- [ ] **B.2 Write competitive positioning.** For each: what it does, how it overlaps with beachcomber, why beachcomber is different/better (or when to use that tool instead). Honest, not dismissive. Goes in README.

---

#### Phase C: Documentation

##### C.1 README.md

One long file with in-file linking. Sections in order:

- [ ] **Marketing pitch (top section).** Why you want this. The problem viscerally: "You have 30 terminal shells. Each one spawns its own git daemon. Your laptop is running 720 threads just to show branch names." Concrete performance numbers: "38µs vs 5ms. 111x faster. 333x fewer shell forks." Before/after comparison.
- [ ] **Quick start.** Install (brew, cargo install), verify it works, see a result in 30 seconds.
- [ ] **How it works.** One paragraph + diagram. Daemon, providers, cache, consumers.
- [ ] **Consumer integration examples.** Each example should be notably different — don't repeat the same pattern:
  - zsh prompt (precmd + shell variable)
  - tmux status bar (`#()` format string replacement)
  - bash prompt (PROMPT_COMMAND)
  - fish prompt (fish_prompt function)
  - neovim statusline (Lua via vim.loop)
  - starship custom module
  - VS Code extension (Node.js unix socket)
  - polybar/waybar/sketchybar custom module
  - shell script one-liner for CI/automation
  - Python script reading state
- [ ] **Shell fallback function.** A portable shell function that apps can use: "if beachcomber is installed, use it; else fall back to manual." Must work in bash, zsh, fish. This is critical for adoption — apps can add beachcomber support without requiring it.
- [ ] **Configuration reference.** Full config.toml documentation with every field, defaults, and examples.
- [ ] **Built-in providers reference.** Every provider with fields, types, default triggers, example output.
- [ ] **Custom providers guide.** How to write script providers. JSON and kv formats. Invalidation strategies. Real examples (docker context, kubectl version, node version, ruby version).
- [ ] **CLI reference.** Every command with examples.
- [ ] **Competitive landscape / alternatives.** Shout-outs to gitstatus, watchman, etc. Honest comparison.
- [ ] **FAQ.**
- [ ] **Contributing.**

##### C.2 Architecture / Internals Documentation

For contributors and app maintainers who want deep understanding.

- [ ] **Architecture overview.** Daemon, scheduler, watcher, cache, server, registry. Data flow diagrams.
- [ ] **Provider development guide.** How to write a new built-in provider. The Provider trait, metadata, invalidation strategies, testing patterns. Step-by-step walkthrough.
- [ ] **Protocol specification.** Complete wire protocol reference. Every operation, request/response format, error codes. For people building clients in other languages.
- [ ] **Performance considerations.** Link to docs/performance.md. Guidelines for keeping providers fast. When to use file reads vs process spawns.

---

#### Phase D: CI/CD and Release Infrastructure

- [ ] **D.1 GitHub Actions: CI.** On every PR and push to main:
  - `cargo check`
  - `cargo test` (with known sandbox-limited tests marked `#[ignore]`)
  - `cargo clippy -- -D warnings`
  - `cargo fmt -- --check`
  - `cargo bench` (run but don't fail on regression — report numbers)
- [ ] **D.2 GitHub Actions: Release.** On tag push (`v*`):
  - Build release binaries for macOS (x86_64 + aarch64)
  - Build release binaries for Linux (x86_64 + aarch64) — when Linux support lands
  - Create GitHub Release with binaries
  - Publish to crates.io
  - Update Homebrew formula
- [ ] **D.3 Benchmark regression tracking.** Store benchmark results per commit. CI compares against baseline. Report in PR comments. Alert on >10% regression. Options: `criterion`'s built-in baseline comparison, or a custom script using `cargo bench -- --save-baseline`.
- [ ] **D.4 SemVer policy.** Document what constitutes breaking changes:
  - Protocol wire format changes = major
  - Config format changes = minor (with backwards compat) or major (without)
  - Provider field additions = minor
  - Provider field removals = major
  - CLI flag changes = minor (additions) or major (removals)

---

#### Phase E: Install Methods

- [ ] **E.1 Homebrew formula.** Primary macOS install method. Tap or core formula. Auto-update via `brew upgrade`.
- [ ] **E.2 `cargo install`.** For Rust developers. Already works (`cargo install --path .`), needs crates.io publication.
- [ ] **E.3 Pre-built binaries.** GitHub Releases with `beachcomber-<version>-<target>.tar.gz`. Install script: `curl -fsSL https://... | sh`.
- [ ] **E.4 Other package managers (future).** Nix, AUR, MacPorts, Scoop. Document how to request packaging.

---

#### Phase F: Repository Hygiene

- [ ] **F.1 LICENSE.** Choose license (MIT? Apache-2.0? MIT/Apache dual?). Create LICENSE file.
- [ ] **F.2 CONTRIBUTING.md.** How to contribute, code of conduct, PR process.
- [ ] **F.3 CHANGELOG.md.** Start from v0.1.0. Follow keepachangelog.com format.
- [ ] **F.4 .gitignore.** Ensure target/, .claude/, docs/superpowers/, INIT.md are excluded from published artifacts. Keep planning docs in repo but add a note they're internal.
- [ ] **F.5 Cargo.toml metadata.** description, repository, license, keywords, categories for crates.io.
- [ ] **F.6 CLAUDE.md.** Project-specific Claude Code instructions for contributors who use it.

---

### Milestone: v0.2.0 (Post-Launch)

After the initial release, based on feedback and adoption.

#### Linux Support (P6 from original roadmap)

- [ ] Battery provider: read `/sys/class/power_supply/`
- [ ] Network provider: Linux `getifaddrs` + `iwgetid` for SSID
- [ ] Uptime provider: read `/proc/uptime`
- [ ] Conditional compilation: `#[cfg(target_os)]` blocks in platform-specific providers
- [ ] CI: Add Linux test matrix

#### External Provider Backends (P5 from original roadmap)

- [ ] Lua backend via `mlua` crate
- [ ] Shared library backend via `libloading` crate

#### Additional Features

- [ ] Watchdog (P4.2) — detect scheduler stalls, auto-restart
- [ ] Configurable backoff steps (P3.2)
- [ ] Push/streaming mode — stream cache updates to long-lived connections

---

### Milestone: v1.0.0 (Stability)

- [ ] Protocol stability guarantee (wire format frozen)
- [ ] Config format stability guarantee
- [ ] Published Rust client crate (`beachcomber-client`)
- [ ] C shared library (`libbeachcomber`) for FFI
- [ ] mmap/shared memory for zero-latency reads (if demand exists)
- [ ] Consumer integration packages (zsh plugin, tmux plugin, neovim plugin)

---

## Priority Order for v0.1.0 Release

| Phase | What | Blocks | Effort |
|---|---|---|---|
| **A** | Naming | Everything public-facing | Small (decision) |
| **B** | Competitive research | README positioning | Medium (research) |
| **C.1** | README | Publishing | Large (writing) |
| **C.2** | Internal docs | Contributing | Medium (writing) |
| **D.1** | CI | PRs, quality gates | Medium |
| **D.2** | Release automation | Publishing | Medium |
| **D.3** | Bench regression | Ongoing quality | Small |
| **E.1** | Homebrew | macOS install | Small |
| **E.2** | crates.io | Rust install | Small |
| **F** | Repo hygiene | Publishing | Small |

**Critical path:** A (naming) -> B (research) -> C.1 (README) -> F (hygiene) -> D (CI) -> E (install) -> publish.
