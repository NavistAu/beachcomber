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

**Fix:** Changed cache key to a single `String` using a null-byte separator: `"provider\0path"` for path-scoped entries, `"provider"` for global entries. One allocation instead of two.

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

**Fix:** Changed `ProviderRegistry` to store `Arc<dyn Provider>` (converted from `Box` at registration). The scheduler clones the Arc and moves it into `tokio::task::spawn_blocking`, making execution non-blocking.

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

**Problem:** Each `Client` method (get, poke) opened a new Unix socket connection, sent one request, read one response, and closed the connection. A prompt querying 3 values paid 3× the connection overhead.

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

#### 2.2 Kubecontext — read kubeconfig instead of kubectl

**Problem:** `kubecontext.rs` spawned `kubectl config current-context` and `kubectl config view --minify`. kubectl is a Go binary with ~30ms startup time. Two calls = ~60ms.

**Fix:** Read `~/.kube/config` directly. Extract `current-context:` with a line scan, then find the matching context block for its namespace. Respects `KUBECONFIG` env var.

```rust
// Before: ~60ms (2 Go process spawns)
Command::new("kubectl").args(["config", "current-context"]).output()
Command::new("kubectl").args(["config", "view", "--minify", ...]).output()

// After: ~749ns (file read + YAML-like parse)
let content = std::fs::read_to_string(kubeconfig_path).ok()?;
// find "current-context:" line, then scan context blocks for namespace
```

**Result:** ~60ms → 749ns. **~80,000x improvement.**

**Caveat:** The kubeconfig parser is line-based, not a full YAML parser. It handles the standard kubeconfig format correctly but may not handle exotic formatting. If edge cases arise, consider adding `serde_yaml` as an optional dependency.

#### 2.3 Network — getifaddrs() instead of process spawns

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
| **Nanosecond** (< 1µs) | 395ns - 749ns | user, load, hostname, uptime, kubecontext, gcloud, aws, conda | libc calls, file reads, env vars |
| **Microsecond** (1-100µs) | ~1-50µs | terraform, python, asdf, direnv (no direnv binary) | File existence checks + reads |
| **Millisecond** (1-10ms) | 2-6ms | network (2ms), git (5.6ms), battery (6ms) | 1 process spawn each |
| **Slow** (10-50ms) | 10-50ms | mise, direnv (with direnv), script providers | Process spawn (user-defined) |

### Socket and Cache Latency

| Operation | Latency |
|---|---|
| Cache read (global key) | 157 ns |
| Cache read (path-scoped key) | 205 ns |
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

### Real-World Impact

| Scenario | Before beachcomber | With beachcomber |
|---|---|---|
| zsh prompt (3 queries) | ~5ms (gitstatus fork) | 45µs (ClientSession) — **111x faster** |
| tmux status (100 panes, 10s refresh) | 2.5s CPU (500 shell forks) | 7.5ms (socket queries) — **333x faster** |
| fseventsd load (N watchers) | N watchers × N dispatch | 1 watcher, shared cache |

---

## Remaining Optimization Opportunities

### High value, not yet implemented

1. **ProviderResult: Vec instead of HashMap.** Providers have 2-10 fields. HashMap's hashing overhead dominates for small collections. A `Vec<(String, Value)>` with linear scan would be faster for <16 fields. Estimated 20-30% improvement in provider construction + field lookup.

2. **Response serialization: skip serde_json::Value intermediate.** The get handler converts `Value` → `serde_json::Value` → JSON string (double serialization). A direct serializer writing the Response in one pass would cut ~30% from response formatting.

3. **Battery: IOKit direct read.** Replace `pmset -g batt` (6ms) with `IOPSCopyPowerSourcesInfo()` via IOKit FFI. Would bring battery to sub-microsecond. Complexity: moderate (requires linking IOKit framework).

4. **Network SSID: CoreWLAN via objc.** Replace `airport -I` (2ms) with CoreWLAN framework call. Would bring network to sub-microsecond. Complexity: moderate (requires objc crate).

5. **metadata() allocation.** Every `metadata()` call allocates Strings for provider name and field names. Use `Cow<'static, str>` to allow zero-allocation for built-in providers while supporting dynamic names for script providers.

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
- [ ] Git provider stays under 7ms (run `cargo bench --bench providers`)
- [ ] No new process spawns added to providers that poll frequently (< 30s interval)
- [ ] Provider execution does not block the scheduler loop (must use `spawn_blocking`)
- [ ] New providers that shell out document why a file read is not feasible
- [ ] Throughput sustains >30k req/s at 100 concurrent clients (run `cargo bench --bench throughput`)
