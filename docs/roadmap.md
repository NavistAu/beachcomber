# beachcomber Roadmap

Single source of truth for project status and next steps.

Last updated: 2026-04-11

---

## What's Built

### Core (Plans 1-4)

**Daemon:** Single async Rust binary (tokio). Unix socket server with socket activation. Graceful shutdown (CancellationToken + SIGINT). Configurable idle shutdown (disabled by default). File-based logging to `~/.local/state/beachcomber/daemon.log`.

**Cache:** Concurrent DashMap. 157ns read latency. Staleness computation with expected refresh intervals. Auto-refresh on cache miss (triggers background computation so next query hits). Detailed cache listing via `comb status`.

**Scheduler:** Poll timers, filesystem watching (notify/FSEvents), refresh triggers. Provider execution on `spawn_blocking` (non-blocking). Execution timeouts (configurable, default 10s). Deduplication (in-flight tracking + pending rerun). Failure backoff (exponential after 3 consecutive failures, max 60s). Demand-driven cache warming (QueryActivity keeps providers warm while actively queried). Backoff/drain lifecycle is NOT wired — see Known Core Issues.

**Protocol:** get, refresh, put, watch, context, list, status. JSON and text output formats. Connection context for implicit path resolution. Staleness flag in responses. Path canonicalization (relative -> absolute). Subscribe/unsubscribe removed — demand-driven warming replaced explicit subscriptions (ephemeral consumers can't maintain persistent connections).

**CLI:** `comb daemon | get | refresh | put | watch | list | status`

**Config:** TOML at `~/.config/beachcomber/config.toml`. Provider enabled/disabled flag. Provider timeout, poll interval, floor overrides. Script provider definitions. Lifecycle tuning (grace period, idle shutdown).

**Providers (16 built-in + script backend):**

| Provider | Scope | Execution time |
|---|---|---|
| hostname, user, uname | global/once | ~400-650ns |
| load, uptime | global/poll | ~550-660ns |
| kubecontext, gcloud, aws, conda | global/poll | <1µs |
| terraform, python, asdf | path/watch | <1µs |
| direnv, mise | path/watch | varies (process spawn) |
| network | global/poll | 2ms |
| git | path/watch+poll | 5.6ms |
| battery | global/poll | 6ms |

**External:** Script provider backend (JSON or kv output, any language).

**Client SDKs:** Rust (`libbeachcomber`), C (`libbeachcomber`), Python, Node.js (TypeScript), Go, Lua, Ruby. All stdlib-only, with test suites. Lua SDK has three backends (vim.uv, luasocket, CLI fallback).

**Performance:** 42k req/sec. 15µs/query via ClientSession. Comprehensive benchmark suite (criterion).

---

## Milestone: v0.1.0 Public Release — Published 2026-04-01

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
  - SDK test suites: Python pytest, Node.js npm test, Go test, Ruby minitest, Lua test runner, C make test
- [x] **D.2 GitHub Actions: Release.** On tag push (`v*`):
  - Build release binaries for macOS (x86_64 + aarch64) and Linux (x86_64 gnu + musl)
  - Create GitHub Release with binaries, C SDK tarball, and auto-generated notes
  - Parallel publish to crates.io, PyPI, npm, RubyGems, LuaRocks
  - Tag Go module
  - Auto-update Homebrew tap formula
- [x] **D.3 Benchmark regression tracking.** Benchmark results uploaded as artifacts on main pushes.
- [x] **D.4 SemVer policy.** Documented in docs/versioning.md. Covers protocol, config, providers, CLI, SDKs, and Rust client crate. Defines what is not a public surface.

---

#### Phase E: Install Methods

- [x] **E.1 Homebrew formula.** `brew tap navistau/tap && brew install beachcomber`. Auto-updated from release workflow via NavistAu/homebrew-tap.
- [x] **E.2 `cargo install`.** `beachcomber` published to crates.io.
- [x] **E.3 Pre-built binaries.** Release workflow creates `beachcomber-<tag>-<target>.tar.gz` for 4 targets.
- [ ] **E.4 Other package managers (future).** Nix, AUR, MacPorts, Scoop. Document how to request packaging.

---

#### Phase F: Repository Hygiene

- [x] **F.1 LICENSE.** MIT license.
- [x] **F.2 CONTRIBUTING.md.** How to contribute, PR process, code of conduct.
- [x] **F.3 CHANGELOG.md.** Started from v0.1.0, keepachangelog.com format.
- [x] **F.4 .gitignore.** target/, .claude/, docs/superpowers/, INIT.md excluded.
- [x] **F.5 Cargo.toml metadata.** description, repository, license, keywords, categories for crates.io.
- [x] **F.6 CLAUDE.md.** Project-specific Claude Code instructions for contributors who use it.

---

#### Phase G: Publishing

- [x] Create GitHub repo (NavistAu/beachcomber)
- [x] Push code
- [x] Tag v0.1.0 to trigger release workflow
- [x] `cargo publish` beachcomber + libbeachcomber to crates.io
- [x] Create Homebrew tap with formula (NavistAu/homebrew-tap)
- [x] Publish Python SDK (`libbeachcomber`) to PyPI
- [x] Publish Node.js SDK (`libbeachcomber`) to npm
- [x] Publish Go module (tagged `sdks/go/v0.1.0` in repo)
- [x] Publish Lua SDK (`libbeachcomber`) to LuaRocks
- [x] Publish Ruby SDK (`libbeachcomber`) to RubyGems
- [x] Publish C SDK source tarball in GitHub Release
- [x] Reserve `beachcomber` name on npm, PyPI, RubyGems, LuaRocks (npm and PyPI now ship binary installers; RubyGems and LuaRocks remain name-reservation placeholders)

---

## Done: Client SDKs

Client libraries for each language wrapping the Unix socket protocol with typed APIs, socket discovery, timeouts, and error handling. All published as `libbeachcomber`.

| SDK | Registry | Status |
|---|---|---|
| **Rust** (`libbeachcomber`) | crates.io | **Published** |
| **C** (`libbeachcomber`) | GitHub Release tarball | **Published** — embedded JSON parser, shared/static lib, 130 tests |
| **Python** (`libbeachcomber`) | PyPI | **Published** — sync client + session, dataclasses, 80 tests |
| **Node.js** (`libbeachcomber`) | npm | **Published** — TypeScript, async API, 62 tests |
| **Go** | Go module | **Published** — idiomatic error returns, 47 tests |
| **Lua** (`libbeachcomber`) | LuaRocks | **Published** — vim.uv / luasocket / comb CLI fallback, 50 tests |
| **Ruby** (`libbeachcomber`) | RubyGems | **Published** — block-based sessions, minitest, 45 tests |
| **Shell** (POSIX sh function) | N/A (copy-paste) | **Done** (in README, portable fallback functions) |

---

## Deferred: Post-Launch

### Release Infrastructure

- [x] Swap temp registry tokens to trusted publishing (OIDC for crates.io, PyPI, npm, RubyGems; LuaRocks has no OIDC support)
- [x] Add aarch64-unknown-linux-gnu binary target (cross-rs with pinned 0.2.5 images)
- [x] Make Go module tag step idempotent in release workflow
- [x] Binary distribution via npm/PyPI `beachcomber` packages (replace noop placeholders with platform-specific binary installers)

### Website (beachcomber.sh)

- [x] GitHub Pages site for beachcomber.sh (docs, install instructions, examples)
- [x] CI workflow to build and deploy the site on push to main
- [x] Configure beachcomber.sh custom domain (DNS + GitHub Pages CNAME)
- [ ] Site analytics — Umami + Cloudflare Web Analytics. Design and rollout managed in ~/ws/analytics/ project.

### Linux Support

- [x] Battery provider: read `/sys/class/power_supply/`
- [x] Network provider: Linux `getifaddrs` + `nmcli`/`iw` for SSID
- [x] Uptime provider: read `/proc/uptime`
- [x] Conditional compilation: `#[cfg(target_os)]` blocks in platform-specific providers

### External Provider Backends

- [x] Shared library backend via `libloading` crate
- ~~Lua backend via `mlua` crate~~ — won't do; dependency/attack surface cost not justified given script and library backends cover the use cases

### Additional Features

- [x] Watchdog — detect scheduler stalls, trigger clean shutdown for supervisor restart. Configurable via `[daemon] watchdog_interval` and `watchdog_threshold`.
- [x] Configurable backoff steps — failure reattempts, backoff interval, cache lifespan, poll idle interval. Global defaults in `[lifecycle]`, per-provider overrides in `[providers.<name>]`. Duration strings ("30s", "2m", "500ms").
- [x] Virtual providers / store — comb store protocol op lets external processes write data into the cache. Creates virtual providers (no execute(), data-only). Namespace hierarchy: builtin > script > virtual.
- [x] comb watch <key> [path] — server-push streaming over long-lived connections. NDJSON line emitted on each cache update for the watched key. Subsumes the push/streaming mode item.
- [x] Git provider: `commit_summary` field — first line of HEAD commit message. Enables WIP detection in prompt integrations (p10k checks for "wip"/"WIP" in the summary).
- [x] Git provider: `push_ahead` and `push_behind` fields — commits ahead/behind the push remote (as distinct from the tracking remote). Used by p10k and starship for push remote indicators.
- [x] `sudo` provider — detect whether the user has an active sudo timestamp (`/var/run/sudo/ts/$USER` on Linux, `/var/db/sudo/` on macOS). Global, poll. Useful as a prompt indicator for elevated privilege awareness.
- [x] `op` provider — detect active 1Password CLI session. Check agent socket liveness or `op whoami` state. Global, poll. Useful for prompt indicators showing authenticated credential access.
- [x] Synchronous cache miss — when `comb get` hits a cold cache, execute the provider inline and return the result instead of returning empty and waiting for the next poll. Accept slightly higher latency on the first query rather than returning blank. Critical for prompt integrations where a blank first prompt looks broken.
- [ ] Per-cache-entry absolute-path watches — the current scheduler registers fs watches relative to the cache entry's path. Global (pathless) cache entries receive no watches and rely on fallback polling for invalidation.

  **Specific case:** `mise.global` cannot directly observe `~/.config/mise/config.toml` changes; it gets them within the 30s fallback poll window. Acceptable today, but other plausible providers want the same shape — an HTTP provider watching a local credentials file for rotation, a library provider watching an absolute-path state file, etc.

  **Design questions when picked up:**
  - Extend `InvalidationStrategy::Watch` to accept both relative patterns and absolute paths? Or a new `InvalidationStrategy::WatchAbsolute` variant?
  - Watch registration keyed per-`(provider, cache_path)` rather than per-provider, so each cache entry can have its own distinct watch set.
  - Path expansion for `~` and `$XDG_CONFIG_HOME` at registration time — who owns that?
  - Interaction with `fsevents_reinstate` (cache lifecycle) — orthogonal; watches still persist through decay only if that flag is true.

  **Scope when picked up:** affects `src/scheduler.rs` watch-registration code, `src/provider/mod.rs` `InvalidationStrategy`, and any provider that wants to use the new shape (starting with mise.global). Independent of the decay rebuild — doesn't block that work and isn't blocked by it.

### CLI Ergonomics

- [x] Command shorthands — `comb g` = `comb get`, `comb r` = `comb refresh`, `comb w` = `comb watch`, `comb s` = `comb status`, `comb l` = `comb list`, `comb p` = `comb put`, `comb d` = `comb daemon`.
- [x] Rename `comb store` to `comb put` and `comb poke` to `comb refresh` (shorter, clearer verbs).
- [x] Format suffix syntax on get — `comb g git.branch .`. Avoids the `-f` flag entirely. Text is the default (no suffix). Suffixes: `.s` (shell key=value), `.t` (tsv), `.T` (TSV with header), `.f` (template), `.c` (csv), `.C` (CSV with header), `.j` (json).
- [x] New output format: `sh` (shell) — outputs `name=val` pairs (the current `text` format for objects). `text` becomes just the raw value with no key prefix. `sh` is sourceable in shell scripts.
- [x] New output formats: `csv`/`tsv` (values only), `CSV`/`TSV` (with header row). For multi-field provider queries.
- [x] New output format: `fmt` — takes a `{field_name}` template string as an argument, applies it per field. e.g. `comb g.f '{branch} ({dirty})' git .`. Uses brace interpolation, not printf.
- [x] `comb eval` — template interpolation. e.g. `comb eval "branch: {git.branch} load: {load.one}" .` — resolves all referenced keys in a single connection and returns the formatted string. Alias: `e`.
- [x] Batch get — `comb fetch` (alias `f`). Single command to query multiple keys in one connection. Format-aware output.
- [x] Field metadata access — colon delimiter on key: `comb g git.branch:age` returns the cache age in milliseconds. Metadata fields: `age`, `stale`, `fresh`, `cache`, `source`.
- [x] Help screen branding — NavistAu authorship, project URL, tagline, MIT license in `comb --help` and `comb --version` output. Format suffix usage hint in after-help.
- [ ] `comb inspect <path>` (working name) — dump every applicable provider's fields for the given directory in one shot. Globals + path-scoped providers evaluated against the path; single command replaces "ask every provider one at a time". Useful for "what does beachcomber know about this dir right now?" as a diagnostic, onboarding, and integration-authoring tool. Format-aware output (json/text/human). Naming bikeshed: `inspect`, `dump`, `snapshot`, `all`, `info`.
- [ ] `comb status` UX redesign for lifecycle visibility — **needs its own brainstorm.** After the cache-decay rebuild lands, the steady-state daemon carries rich lifecycle information per entry that `comb status` currently under-exposes. A minimum `DECAY` column (0-4, where 0=Active) ships with the decay rebuild for basic visibility; the full UX pass below is deferred to a focused session.

  **Context for the brainstorm:**
  - Current columns: `PROVIDER PATH FIELD VALUE AGE STALE`.
  - Information worth surfacing per entry after the rebuild: current lifecycle state (Active, Decay1-4, Evicted), current poll interval, time since last demand, whether watches are active, whether the provider has `fsevents_reinstate`, age, staleness.
  - These values interact — e.g., `AGE` + `STALE` + `DECAY` together tell the "how fresh is this" story, but today `STALE` is a redundant bool derived from `AGE > expected_interval`. Folding staleness into a colored AGE column is a natural simplification.

  **Options considered (not yet decided):**
  - **AGE column with F/S prefix + color** — `F 23s` (fresh, green) / `S 2m` (stale, amber) / etc. Removes the separate `STALE` column.
  - **Decay column showing level 0-4** — integer with optional glyph; 0 could be blank to keep common case clean. Could also use unicode bar (▁▂▃▄▅ etc.).
  - **fsevent policy indicator** — per-provider, probably belongs in `comb introspect providers` rather than per-entry `status`.
  - **K and P values** — probably too noisy for `status`. Home is `comb introspect providers` or a dedicated `comb introspect lifecycle`.
  - **`comb introspect lifecycle` subject** — new subject showing the full per-entry lifecycle state for diagnostics. Currently `comb introspect backoff` exists; it should either be renamed to `lifecycle` or kept as an alias.

  **Open design questions:**
  - Should `STALE` disappear as a distinct column (folded into colored AGE) or stay for scriptable output where colors are off?
  - Should `DECAY 0` display as blank, or `0`, or `.`, or `Active`?
  - Do users ever want to see K and P in `status`, or is that better in `introspect`?
  - Is there value in a `--watch` mode for `status` (live-refreshing display of state transitions)?

  **Scope when picked up:** design brainstorm → spec → implementation. Should touch `src/cli/status_format.rs` and possibly `src/server.rs` (for introspect changes). No protocol change needed if the extra data is already carried in the status response.

### Shell Integration

- [x] `chpwd` hook — standalone `scripts/chpwd.sh` for zsh/bash/fish. Refreshes path-scoped providers on directory change. Curl-able from beachcomber.sh.
- [x] `comb` polyfill function — standalone `scripts/polyfill.sh`. POSIX shell function wrapping `comb` with transparent fallback to native tools for known keys (git, hostname, load, battery, user).
- [x] `comb init` — auto-detect installed tools (p10k, starship, oh-my-tmux, tmux, neovim, polybar, waybar, sketchybar, oh-my-zsh) and print integration snippets. Alias: `i`.

### Health and Diagnostics

- [x] `comb check` — health check command with subcommands: `all`, `daemon`, `config`, `providers`, `cache`, `procs`. Text report with PASS/WARN/FAIL indicators. Alias: `c`.
- [x] `comb check procs` — process exec tracing (`eslogger exec` on macOS, `/proc` scanning on Linux) that captures subprocess spawns over a sample window, categorizes them against known provider domains, and reports replacement opportunities.

### Additional Providers

- [ ] `ssh` provider — `keys_loaded` (count of loaded keys), `agent_running` (bool). Global, poll. Useful for prompt indicators before push/deploy operations.
- [ ] `kerberos` provider — active ticket state. Global, poll.
- [ ] `docker` provider — running container count, current context. Support Docker, OrbStack, and Podman. Global, poll.
- [ ] `brew` provider — count of outdated packages. Global, poll (daily). Extend pattern to other package managers: `apt`, `dnf`/`yum`, `apk`, `pacman`.

### Performance Validation

- [ ] Hyperfine benchmarks — before/after comparisons for tmux status bar refresh and p10k prompt render with beachcomber vs native tools. Publish results on the website.
- [ ] Docker test containers — controlled environments for each integration target (oh-my-tmux, p10k, starship, etc.) with both stock and beachcomber-integrated configs. Enables reproducible perf tuning, CI benchmarks, and side-by-side demos for upstream adoption PRs.

### Documentation

- [ ] Fun examples — copy-paste script provider recipes for the website. Show beachcomber as a general-purpose cached data layer, not just a devtools daemon. Practical: local weather, quote of the day, today's Wordle number, countdown to next One Piece episode, ISS overhead pass, Hacker News top story, Spotify current track, Home Assistant entity state (front door lock, thermostat, lights via HA REST API). Weird/fun (not all committed, pick the best):
  - Mercury Retrograde deploy guard — HTTP provider polling an astrology API, git pre-push hook refuses to push to production during retrograde. `mercury_yolo_mode = true` to override.
  - Don't Deploy On Friday — same pattern, simpler astrology. Block or warn on pushes to production branches after Thursday EOD.
  - Physical deploy confidence gauge — combine git.dirty, git.ahead, load, battery, time-since-commit into a 0-100 score. Pipe via `comb watch` to an Arduino servo gauge on your desk. Zones: "YOLO", "Maybe", "Safe", "It's Friday Don't".
  - Vibe check emoji — single emoji in prompt computed from weather, day of week, battery, git state, load. Monday + rain + dirty repo + low battery = 💀. Friday + sunshine + clean = 🏖️. Full decision tree as a flowchart.
  - Office thermostat wars — poll HA thermostat, alert in tmux when someone changes it with who and by how much. Rolling sparkline of temperature battles over the day.
  - Banana scale — compute physical length of terminal scrollback in bananas (lines x font size x approximate character height ÷ 17.78cm). Display as a literal string of 🍌 emoji. Utterly useless, deeply satisfying.
  - Claude token odometer — `wc -l` of Claude Code conversation history from the last 24 hours. Show in tmux/prompt as a rolling count of how much you've been talking to your AI pair programmer today.
  - Magic 8-Ball deploy oracle — script provider that caches a random 8-ball response ("Outlook not so good", "Signs point to yes", etc.) and reshakes at random intervals. Git pre-push hook checks `comb get 8ball.sentiment` and blocks deploys on negative readings. Show the current reading in tmux. The universe has opinions about your release schedule.
- [ ] Timeseries / sparkline guide — document how to use `comb store` to accumulate rolling timeseries data (e.g. service response times, error rates, CPU samples) and render sparkline/histogram status tickers in tmux or prompts. Cover append-to-array patterns, TTL-based expiry, and how to structure the data for consumers that render sparklines (e.g. oh-my-tmux, tmux-sparkline, unicode block characters).

### Project Quality

- [ ] Dependency audit — evaluate all Rust dependencies, document each one on the website with rationale for inclusion. Remove anything not strictly necessary.
- [ ] `llms.txt` for the website — machine-readable project summary for LLM consumption.
- [ ] Centralise the version number — currently every release touches 13+ files (`Cargo.toml`, `beachcomber-client/Cargo.toml`, 5 SDK manifests, 3 AUR PKGBUILDs, nix flake, README deb/rpm URLs, release.yml rockspec filename, plus the Lua rockspec rename). Options: a single `VERSION` file that a release script templates into each manifest; `cargo xtask release` that reads `Cargo.toml` and rewrites the rest; a release-plz / cargo-release workflow; or a CI-side step that patches non-Rust manifests from the Cargo version before publish. Goal: one edit + one command, no per-file fanout.

### Install Methods

- [x] Nix flake
- [x] AUR package
- ~~MacPorts~~ — won't do; small audience, not worth ongoing maintenance
- [x] Debian/Ubuntu (.deb) package
- [x] Fedora/RHEL (.rpm) package
- ~~Scoop~~ — won't do; no Windows support planned

### Linux C SDK Packages

- [x] `libbeachcomber-dev` (deb) — headers + shared/static lib + pkg-config
- [x] `libbeachcomber-devel` (rpm)
- [x] `libbeachcomber` (AUR)

### Stability

- ~~Protocol stability guarantee (wire format frozen)~~ — won't do; protocol is abstracted behind CLI/SDKs, no need for manual socket consumers
- ~~Config format stability guarantee~~ — won't do; same reasoning
- ~~mmap/shared memory for zero-latency reads~~ — won't do; 15µs socket reads are already invisible, maintenance burden not justified
- [x] Integration guides and tutorials for beachcomber.sh website — tested config snippets and step-by-step tutorials for starship, oh-my-zsh, powerlevel10k, oh-my-posh, oh-my-tmux, lualine.nvim, heirline.nvim, polybar, waybar, sketchybar. Replaces the original "consumer integration packages" plan.
