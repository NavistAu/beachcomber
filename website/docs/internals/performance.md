---
sidebar_position: 3
title: Performance
---

# beachcomber Performance Guide

This document records all performance optimizations applied to beachcomber, the design principles behind them, and the measured results. It serves as a reference for future development to ensure performance regressions are avoided and further optimization opportunities are understood.

## Design Principles

1. **Never fork a process when you can read a file or call libc.** Process spawns cost 2-6ms minimum. File reads and syscalls cost nanoseconds. For a daemon that must serve cached state in microseconds, every process spawn in a provider is a performance bug waiting to happen.

2. **Cache reads are the hot path.** Every consumer query hits the cache. Optimize cache lookups above all else — avoid allocations, minimize hashing, return data without copying when possible.

3. **Provider execution is the cold path (but still matters).** Providers only execute on invalidation (filesystem change or poll timer), not on every query. But slow providers block `spawn_blocking` thread pool slots and delay cache freshness. Keep them fast.

4. **Amortize connection overhead.** Unix socket connect is ~30µs. For consumers querying multiple values per render cycle (prompts, status bars), a persistent connection (`ClientSession`) amortizes this to ~15µs/query.

5. **The scheduler must never block.** Provider execution happens on `spawn_blocking` threads. The scheduler's async loop must remain responsive to messages, filesystem events, and poll timers at all times.

---

## Optimization History

### Round 1: Core Infrastructure

#### 1.1 Git provider — read stash from file, not process

**Problem:** `git.rs` spawned TWO processes per execution: `git status --porcelain=v2 --branch` (6.2ms) and `git stash list` (~5ms). The stash count alone nearly doubled the provider's execution time.

**Fix:** Read `.git/logs/refs/stash` directly and count lines. Each line in that file is one stash entry.

```rust
// Before: ~5ms process spawn
fn count_stashes(dir: &Path) -> i64 {
    Command::new("git").args(["stash", "list"]).current_dir(dir).output()...
}

// After: ~1µs file read
fn count_stashes(dir: &Path) -> i64 {
    let stash_log = dir.join(".git").join("logs").join("refs").join("stash");
    std::fs::read_to_string(&stash_log)
        .map(|s| s.lines().count() as i64)
        .unwrap_or(0)
}
```

**Result:** 11.5ms → 5.6ms (-51%). Git provider now at parity with raw `git status`.

**Rule for future providers:** Before shelling out to a CLI for supplementary data, check if the information is available in a file. Git internals are mostly plain text files.

#### 1.2 Cache key — reduce allocations per lookup

**Problem:** Every `cache.get()` call allocated a `(String, Option<String>)` tuple — 2 heap allocations — just to look up a key in the DashMap.

**Fix:** Changed cache key to a single `String` using a null-byte separator: `"provider\0path"` for an entry at a directory, `"provider"` for the global entry. One allocation instead of two.

```rust
// Before: 2 allocations per lookup
let key = (provider.to_string(), path.map(|s| s.to_string()));

// After: 1 allocation per lookup
fn make_cache_key(provider: &str, path: Option<&str>) -> String {
    match path {
        Some(p) => format!("{}\0{}", provider, p),
        None => provider.to_string(),
    }
}
```

**Result:** 183ns → 157ns per read (-16%), 211ns → 182ns per write (-14%).

**Rule for future changes:** The cache key is on the hottest path in the system. Any change to the key type must be benchmarked. Zero-allocation lookups (via `Borrow` trait) would be the next step if needed.

#### 1.3 Scheduler — spawn_blocking for provider execution

**Problem:** `execute_provider()` ran synchronously on the scheduler's tokio task. A git provider taking 5.6ms blocked the entire scheduler loop — no messages processed, no poll timers fired, no filesystem events handled.

**Fix:** Changed `ProviderRegistry` to store its Sources as `Arc`s (today `Arc<dyn Source>`). The scheduler clones the Arc and moves it into `tokio::task::spawn_blocking`, making execution non-blocking.

```rust
// Before: blocks scheduler loop
fn execute_provider(&self, name: &str, path: Option<&str>) {
    let provider = self.registry.get(name).unwrap();
    let result = provider.execute(path); // blocks!
    self.cache.put(name, path, result);
}

// After: fire-and-forget on thread pool
fn execute_provider(&self, name: &str, path: Option<&str>) {
    let provider = self.registry.get(name).unwrap(); // Arc clone
    let cache = Arc::clone(&self.cache);
    tokio::task::spawn_blocking(move || {
        if let Some(result) = provider.execute(path) {
            cache.put(name, path, result);
        }
    });
}
```

**Result:** Scheduler loop stays responsive during provider execution. Multiple providers can execute concurrently.

**Rule for future changes:** Never add synchronous blocking calls to the scheduler's `run()` loop. All I/O, process spawns, and computation must go through `spawn_blocking` or be async.

#### 1.4 Client — persistent connection via ClientSession

**Problem:** Each `Client` method (get, refresh) opened a new Unix socket connection, sent one request, read one response, and closed the connection. A prompt querying 3 values paid 3× the connection overhead.

**Fix:** Added `ClientSession` that holds an open `UnixStream` split into reader/writer halves. Multiple requests share the same connection.

```rust
// Before: 3 queries = 3 connections = ~102µs
let branch = client.get("git.branch", path).await?;
let dirty = client.get("git.dirty", path).await?;
let host = client.get("hostname.name", None).await?;

// After: 3 queries = 1 connection = ~45µs
let mut session = client.connect().await?;
let branch = session.get("git.branch", path).await?;
let dirty = session.get("git.dirty", path).await?;
let host = session.get("hostname.name", None).await?;
```

**Result:** 34µs/query (cold) → 15µs/query (warm). 2.3x faster for multi-query consumers.

**Rule for future consumers:** Always use `ClientSession` for consumers that query multiple values per render cycle (prompts, status bars, editor plugins). The one-shot `Client::get()` is for scripts and CLI usage.

---

### Round 2: Provider Process Spawn Elimination

#### 2.1 GCloud — read config file instead of Python CLI

**Problem:** `gcloud.rs` spawned `gcloud config get-value project` and `gcloud config get-value account` — two invocations of a Python-based CLI. Python interpreter startup alone is 200-500ms. Two calls = 400ms-1000ms per provider execution.

**Fix:** Read `~/.config/gcloud/properties` directly. It's a simple INI file with `[core]` section containing `project` and `account`. Respects `CLOUDSDK_CONFIG` env var override.

```rust
// Before: ~400-1000ms (2 Python process spawns)
Command::new("gcloud").args(["config", "get-value", "project"]).output()
Command::new("gcloud").args(["config", "get-value", "account"]).output()

// After: ~1µs (file read + INI parse)
let content = std::fs::read_to_string(config_dir.join("properties")).ok()?;
// parse [core] section for project= and account= lines
```

**Result:** ~500ms → 1.08µs. **~500,000x improvement.**

**Rule for future providers:** If a CLI tool stores its state in a config file, read the file. Never spawn a Python/Ruby/Node CLI when you can parse a text file.

#### 2.2 Network — getifaddrs() instead of process spawns

**Problem:** `network.rs` spawned 3-4 processes per execution:
1. `route -n get default` — find default interface
2. `ifconfig <iface>` — get IP for that interface
3. `ifconfig` (full) — scan all interfaces for VPN (utun)
4. `airport -I` — get WiFi SSID

At ~5ms per spawn, this was ~15-20ms per provider execution.

**Fix:** Replaced the first three with a single `libc::getifaddrs()` call. One scan of the interface list extracts primary interface, IP address, and VPN detection simultaneously. Only the `airport` call for SSID remains (no practical non-ObjC alternative).

```rust
// Before: 3-4 process spawns (~15-20ms)
Command::new("route").args(["-n", "get", "default"]).output()
Command::new("ifconfig").arg(&iface).output()
Command::new("ifconfig").output()
Command::new("airport").args(["-I"]).output()

// After: 1 getifaddrs() call + 1 airport call (~2ms)
let mut ifaddrs: *mut libc::ifaddrs = std::ptr::null_mut();
libc::getifaddrs(&mut ifaddrs);
// single scan: find primary IPv4 interface, IP, and utun VPN interfaces
```

**Result:** ~20ms → 2ms (-90%). The remaining 2ms is the `airport` SSID lookup.

**Future opportunity:** Replace `airport` with CoreWLAN via `objc` crate to eliminate the last process spawn. This would bring network provider to sub-microsecond.

---

## Current Performance Profile

### Provider Execution Time Tiers

| Tier | Time | Providers | Method |
|---|---|---|---|
| **A file's lines** | about 90-170 µs | hostname, user, load, uptime (a `sys` call), gcloud_configs (reads files); `git.head` about 0.3 ms over its read | One run of the shipped Source: the engine, the lines, no process |
| **One process** | 3-12 ms | network 2.7 ms (`getifaddrs`, and `airport` where it exists), battery 11.8 ms (`pmset`) | One spawn, inside `sys` |
| **Commands** | 10-50 ms | mise, direnv (with direnv), Sources that call `exec()` | Process spawn (user-defined) |
| **A repository's worth** | `status` 16, `diff` 26, `lineage` 38, `refs` 64 ms on this repository | git: 1, 2, 3 and 4 or more `git` processes | Through the compiled-in `git` plugin; no shell |

These are one run through `ProviderRegistry::shipped` (`cargo bench --bench providers -- --quick`, load about 10), and for git the force-run times below. The sub-microsecond figures this table used to give were the libc call alone; the shipped Source's cost is the render around it, on a timer, never on a read.

### Socket and Cache Latency

| Operation | Latency |
|---|---|
| Cache read (global key) | 157 ns |
| Cache read (key with a path) | 205 ns |
| Cache write | 182 ns |
| Socket round-trip (cold, new connection) | 34 µs |
| Socket round-trip (warm, ClientSession) | 15 µs |
| 100 sequential gets on 1 connection | 945 µs (9.5 µs/get) |

### Throughput

| Concurrent clients | Requests/second |
|---|---|
| 1 | ~28,000 |
| 10 | ~45,000 |
| 50 | ~42,000 |
| 100 | ~41,000 |

### Git as a shipped file: phase 7 measured

Measured 2026-11-04, the Rust provider (`bfa7ccd8`) against the shipped file over the compiled-in plugin (`350975ef`), release builds, macOS, on this repository's main checkout, 120 rounds alternating the two binaries, each its own isolated daemon, over the raw socket, load 10-19. Paired difference is the median of each round's after minus before.

| | Rust p50 | file p50 | paired difference |
|---|---:|---:|---:|
| one full refresh, the five Sources started together | 65.96 ms | 66.73 ms | +0.78 ms |
| the five Sources one after another (what `DEC-261104082440` measured as 149.6 ms) | 143.59 ms | 145.84 ms | +2.40 ms |
| `head` alone | 0.13 ms | 0.42 ms | +0.30 ms |
| `status` alone | 15.89 ms | 16.31 ms | +0.15 ms |
| `diff` alone | 25.53 ms | 26.07 ms | +0.83 ms |
| `lineage` alone | 37.82 ms | 38.09 ms | +0.51 ms |
| `refs` alone | 64.35 ms | 64.17 ms | -0.21 ms |

A logging `git` on `PATH` sees the same 10 commands from both, argument for argument, and nothing else runs: no `sh`, no `awk`. `shipped::git::git_a_refresh_spawns_the_same_commands` pins that, and the values of `comb get git` were identical across a plain repository, a subdirectory, a linked worktree, a detached HEAD, a stash, a merge in progress, an upstream and push remote, a relative gitfile, a broken gitfile, no commits, an empty `.git` and outside any repository.

| # | Target | Result | Met |
|---|---|---|---|
| T7.1 | warm `get git.branch` p50 ≤ 0.25 ms; `get git` ≤ 0.4 ms | 59 and 101 µs (58 and 94 µs before), 480 alternating rounds | met |
| T7.2 | `comb get git.branch` within 1 ms of `comb get hostname.name` | 7.44 vs 7.64 ms, 120 alternating rounds | met |
| T7.3 | replaced by the ruling: a full refresh at the baseline plus a few ms for jinja and JSON, the same commands | +0.78 ms together, +2.40 ms one after another; the same 10 commands | met |
| T7.9 | the suite about 10 s, under the 30 s cap | 1891 tests, 13-15 s at load about 10 | under the cap |
| T7.10 | after a read of `git.dirty`, 2 s idle runs `status` at most once more | `git_status_does_not_trigger_itself` passes with the `cfg(linux)` lifted on macOS, and fails (1 run became 3) with `GIT_OPTIONAL_LOCKS=0` taken out; **not yet run on Linux** | pending a Linux run |

**The read straight after a lock file.** One cost did move. The Rust Sources overrode `inputs` to name only git's files, to keep `.git`'s own mtime out of a read's check. A config Source has no such override: `head` and `refs` are checked for the `.git` they `watch`, and all five for the `.git` their lines find with `lup`, so whatever makes a lockfile in `.git` invalidates every record, and the next `comb get git` waits out its 50 ms budget for them. One read pays: the one after settles. A read of one field pays only for its Source. Alternating, a clone per binary, 12 rounds, load 10:

| What happened, then the read | Rust p50 | file p50 |
|---|---:|---:|
| a lockfile came and went, `get git` | 0.14 ms | 51.66 ms |
| another tool's plain `git status` refreshed the index, `get git` | 20.42 ms | 51.87 ms |
| `git add`, `get git` | 20.45 ms | 51.66 ms |
| `git commit`, `get git` | 51.84 ms | 51.95 ms |
| a lockfile came and went, `get git.branch` | 0.14 ms | 0.63 ms |
| `git add`, `get git.branch` | 0.30 ms | 1.01 ms |

Per such event the daemon also runs `diff` and `lineage` on the next read, where they waited for their poll: 15 `git` processes against 10 for a lockfile, 16 against 12 for `git add` or `git commit` (the watch's own runs included). In exchange those counts are current on that read. Closing the gap needs a way to watch a directory without checking it on a read, and a `lup` hit stamped for its presence rather than its mtime.

The cdylib with the icon tables: `libbeachcomber.dylib` 3,455,232 bytes (release, macOS).

### Real-World Impact

| Scenario | Before beachcomber | With beachcomber |
|---|---|---|
| zsh prompt (3 queries) | ~5ms (gitstatus fork) | 45µs (ClientSession) — **111x faster** |
| tmux status (100 panes, 10s refresh) | 2.5s CPU (500 shell forks) | 7.5ms (socket queries) — **333x faster** |
| fseventsd load (N watchers) | N watchers × N dispatch | 1 watcher, shared cache |

---

## Remaining Optimization Opportunities

### High value, not yet implemented

1. **ProviderResult: Vec instead of HashMap.** Providers have 2-10 fields. HashMap's hashing overhead dominates for small collections. A `Vec<(String, Value)>` with linear scan would be faster for `<16` fields. Estimated 20-30% improvement in provider construction + field lookup.

2. **Response serialization: skip serde_json::Value intermediate.** The get handler converts `Value` → `serde_json::Value` → JSON string (double serialization). A direct serializer writing the Response in one pass would cut ~30% from response formatting.

3. **Battery: IOKit direct read.** Replace `pmset -g batt` (6ms) with `IOPSCopyPowerSourcesInfo()` via IOKit FFI. Would bring battery to sub-microsecond. Complexity: moderate (requires linking IOKit framework).

4. **Network SSID: CoreWLAN via objc.** Replace `airport -I` (2ms) with CoreWLAN framework call. Would bring network to sub-microsecond. Complexity: moderate (requires objc crate).

5. **metadata() allocation.** Every `metadata()` call allocates Strings for provider name and field names. Use `Cow<'static, str>` to allow zero-allocation for built-in providers while supporting dynamic names for config Sources.

### Low value / deferred

6. **Connection pooling in CLI.** The `beachcomber get` CLI spawns a tokio runtime per invocation. A shell function holding a persistent connection would eliminate runtime + connection cost. This is a consumer-side optimization, not a daemon optimization.

7. **mmap shared memory for cache reads.** Eliminate socket round-trip entirely by exposing cache via memory-mapped file. Consumers read directly from shared memory. This is the theoretical minimum latency (just a memory read) but adds significant complexity for lifecycle management.

---

## How to Run Benchmarks

```bash
# Run all benchmarks
cargo bench

# Run specific benchmark suite
cargo bench --bench cache
cargo bench --bench protocol
cargo bench --bench providers
cargo bench --bench socket
cargo bench --bench throughput

# Run with baseline comparison (after making changes)
cargo bench -- --baseline main
```

Benchmark results with historical comparison are stored in `target/criterion/`. HTML reports are generated in `target/criterion/*/report/index.html` (requires gnuplot for full reports, falls back to plotters).

---

## Performance Regression Checklist

When modifying beachcomber, verify these properties:

- [ ] Cache read latency stays under 200ns (run `cargo bench --bench cache`)
- [ ] Socket round-trip stays under 40µs cold, 20µs warm (run `cargo bench --bench socket`)
- [ ] A full `git` refresh runs the commands `git_a_refresh_spawns_the_same_commands` lists and no others (run `cargo bench --bench providers`)
- [ ] No new process spawns added to providers that poll frequently (< 30s interval)
- [ ] Provider execution does not block the scheduler loop (must use `spawn_blocking`)
- [ ] New providers that shell out document why a file read is not feasible
- [ ] Throughput sustains >30k req/s at 100 concurrent clients (run `cargo bench --bench throughput`)
