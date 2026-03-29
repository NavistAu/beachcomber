# beachcomber Roadmap

Single source of truth for project status and next steps.

Last updated: 2026-03-29

---

## What's Built

### Core (Plans 1-4)

**Daemon:** Single async Rust binary (tokio). Unix socket server with socket activation. Graceful shutdown (CancellationToken + SIGINT). Configurable idle shutdown (disabled by default). File-based logging to `~/.local/state/beachcomber/daemon.log`.

**Cache:** Concurrent DashMap. 157ns read latency. Staleness computation with expected refresh intervals. Auto-poke on cache miss (triggers background computation so next query hits). Detailed cache listing via `comb status`.

**Scheduler:** Poll timers, filesystem watching (notify/FSEvents), poke triggers. Provider execution on `spawn_blocking` (non-blocking). Execution timeouts (configurable, default 10s). Deduplication (in-flight tracking + pending rerun). Failure backoff (exponential after 3 consecutive failures, max 60s). Subscription manager with multi-tenant cadence resolution. Backoff/drain lifecycle (Grace -> SlowPoll -> Frozen -> Evict).

**Protocol:** get, poke, subscribe, unsubscribe, context, list, status. JSON and text output formats. Connection context for implicit path resolution. Staleness flag in responses. Path canonicalization (relative -> absolute).

**CLI:** `comb daemon | get | poke | subscribe | list | status`

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

**Client SDK:** `beachcomber-client` Rust crate (sync, no tokio dependency). Socket discovery, socket activation, typed field access, persistent sessions.

**Performance:** 42k req/sec. 15µs/query via ClientSession. Comprehensive benchmark suite (criterion).

### v0.1.0 Release Preparation (complete)

- [x] **A. Naming:** Project = beachcomber, binary = comb, crate = beachcomber
- [x] **B. Competitive research:** docs/competitive-landscape.md (gitstatus, watchman, powerline, starship, oh-my-posh, direnv, zoxide, atuin)
- [x] **C.1 README:** 1,443-line README with marketing pitch, 9 consumer integration examples, shell fallback functions, full config/provider/CLI/protocol reference, FAQ
- [x] **C.2 Internal docs:** docs/architecture.md, docs/provider-development.md
- [x] **D.1 CI:** GitHub Actions — check, clippy, fmt, test on macOS
- [x] **D.2 Release automation:** GitHub Actions — build macOS x86_64 + aarch64, create GitHub Release on tag
- [x] **D.3 Bench regression:** Benchmark results uploaded as artifacts on main pushes
- [x] **E.2 cargo install:** Cargo.toml metadata ready for crates.io publication
- [x] **E.3 Pre-built binaries:** Release workflow creates tarballs
- [x] **F. Repo hygiene:** MIT license, CONTRIBUTING.md, CHANGELOG.md, .gitignore, Cargo.toml metadata

### Remaining for v0.1.0 publish

- [ ] **Create GitHub repo** (jhogendorn/beachcomber or org)
- [ ] **Push code**
- [ ] **Tag v0.1.0** to trigger release workflow
- [ ] **`cargo publish`** beachcomber + beachcomber-client to crates.io
- [ ] **E.1 Homebrew formula** — create tap with formula pointing to release tarballs
- [ ] **D.4 SemVer policy** — document breaking change definitions
- [ ] **F.6 CLAUDE.md** — project-specific Claude Code instructions for contributors

---

## Milestone: v0.2.0 (Post-Launch)

After the initial release, based on feedback and adoption.

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

---

## Milestone: v1.0.0 (Stability)

- [ ] Protocol stability guarantee (wire format frozen)
- [ ] Config format stability guarantee
- [ ] mmap/shared memory for zero-latency reads (if demand exists)
- [ ] Consumer integration packages (zsh plugin, tmux plugin, neovim plugin)

### Client SDKs

Published, packaged client libraries for each language's native package manager. Each SDK wraps the Unix socket protocol with typed APIs, handles socket discovery, socket activation (starts daemon if not running), timeouts, and error handling. No consumer should need to hand-roll socket code.

| SDK | Package manager | Status |
|---|---|---|
| **Rust** (`beachcomber-client`) | crates.io | **Done** (workspace crate) |
| **C** (`libbeachcomber`) | Source / pkg-config | Not started — C ABI shared library, enables FFI from any language |
| **Python** (`beachcomber`) | PyPI | Not started — sync client, `socket` module, typed dataclasses |
| **Node.js** (`beachcomber`) | npm | Not started — `net.connect` to Unix socket, TypeScript types |
| **Go** (`beachcomber`) | Go module | Not started — `net.Dial("unix", ...)`, struct types |
| **Lua** (`beachcomber`) | LuaRocks | Not started — for neovim plugins, uses `vim.loop` or luasocket |
| **Ruby** (`beachcomber`) | RubyGems | Not started — `UNIXSocket`, for shell/devtool integrations |
| **Shell** (POSIX sh function) | N/A (copy-paste) | **Done** (in README, portable fallback functions) |

The C library is the highest priority after Rust — it enables FFI bindings for Python (ctypes/cffi), Ruby (ffi), and any other language without writing native socket code per language. The Python and Node.js SDKs are highest value for the developer community given the size of those ecosystems.
